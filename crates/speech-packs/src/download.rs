//! Resumable, cancellable HTTPS transfer into a `.part` file, plus SHA-256
//! verification. Nothing is trusted until its hash matches the manifest.
use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub(crate) const CANCELLED: &str = "Download cancelled; progress is saved for next time";

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// `Ok(false)` for a missing, wrong-size, or wrong-hash file.
pub fn verify_file(path: &Path, sha256: &str, size: u64) -> Result<bool, String> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() == size => {}
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    }
    Ok(sha256_file(path)?.eq_ignore_ascii_case(sha256))
}

async fn wait_for_cancel(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Continue `partial` from `offset` until it holds exactly `size` bytes.
/// `progress` receives the byte count of this file at most every 200 ms.
pub(crate) async fn download_partial(
    cancel: &AtomicBool,
    url: &str,
    size: u64,
    partial: &Path,
    mut offset: u64,
    mut progress: impl FnMut(u64),
) -> Result<(), String> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 || attempt.url().scheme() != "https" {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client.get(url);
    if offset > 0 {
        request = request.header(RANGE, format!("bytes={offset}-"));
    }
    let mut response = tokio::select! {
        response = request.send() => response.map_err(|e| format!("Could not reach the download server: {e}"))?,
        _ = wait_for_cancel(cancel) => return Err(CANCELLED.into()),
    };
    let status = response.status();
    if offset > 0 && status.as_u16() == 206 {
        let range = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !valid_content_range(range, offset, size) {
            return Err("The server returned an unexpected byte range".into());
        }
    } else if status.is_success() {
        offset = 0;
    } else {
        return Err(format!("Download failed: HTTP {status}"));
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(offset > 0)
        .truncate(offset == 0)
        .open(partial)
        .map_err(|e| e.to_string())?;
    let mut downloaded = offset;
    let mut published = Instant::now();
    progress(downloaded);
    loop {
        let chunk = tokio::select! {
            chunk = response.chunk() => chunk.map_err(|e| format!("Download interrupted: {e}"))?,
            _ = wait_for_cancel(cancel) => {
                file.sync_all().map_err(|e| e.to_string())?;
                return Err(CANCELLED.into());
            },
        };
        let Some(chunk) = chunk else {
            break;
        };
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        if downloaded > size {
            return Err("Download exceeded its expected size".into());
        }
        if published.elapsed() >= Duration::from_millis(200) {
            progress(downloaded);
            published = Instant::now();
        }
    }
    file.sync_all().map_err(|e| e.to_string())?;
    progress(downloaded);
    if downloaded != size {
        return Err(format!(
            "Download incomplete: {downloaded} of {size} bytes. Try again to resume."
        ));
    }
    Ok(())
}

fn valid_content_range(value: &str, offset: u64, total: u64) -> bool {
    let Some((range, actual_total)) = value.strip_prefix("bytes ").and_then(|s| s.split_once('/'))
    else {
        return false;
    };
    let Some((start, end)) = range.split_once('-') else {
        return false;
    };
    matches!(
        (start.parse::<u64>(), end.parse::<u64>(), actual_total.parse::<u64>()),
        (Ok(start), Ok(end), Ok(actual_total))
            if start == offset && end >= start && end < total && actual_total == total
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn checksum_rejects_corrupt_file_with_correct_size() {
        let path = std::env::temp_dir().join(format!("speech-pack-check-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(verify_file(&path, abc, 3).unwrap());
        assert!(!verify_file(&path, abc, 4).unwrap());
        std::fs::write(&path, b"abd").unwrap();
        assert!(!verify_file(&path, abc, 3).unwrap());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn resumed_download_rejects_wrong_range_or_total() {
        assert!(valid_content_range("bytes 3-8/9", 3, 9));
        assert!(!valid_content_range("bytes 0-8/9", 3, 9));
        assert!(!valid_content_range("bytes 3-8/10", 3, 9));
        assert!(!valid_content_range("bytes 3-9/9", 3, 9));
        assert!(!valid_content_range("bytes */9", 3, 9));
    }

    #[test]
    fn cancel_interrupts_a_stalled_transfer_and_keeps_partial() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nabc")
                .unwrap();
            stream.flush().unwrap();
            std::thread::sleep(Duration::from_millis(700));
        });
        let partial =
            std::env::temp_dir().join(format!("speech-pack-stalled-{}.part", std::process::id()));
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let worker_path = partial.clone();
        let url = format!("http://{address}/pack");
        let worker = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(download_partial(
                    &worker_cancel,
                    &url,
                    6,
                    &worker_path,
                    0,
                    |_| {},
                ))
        });
        let wait_started = Instant::now();
        while std::fs::metadata(&partial).map(|m| m.len()).unwrap_or(0) < 3 {
            assert!(wait_started.elapsed() < Duration::from_secs(3));
            std::thread::sleep(Duration::from_millis(10));
        }
        let cancelled_at = Instant::now();
        cancel.store(true, Ordering::Release);
        let error = worker.join().unwrap().unwrap_err();
        assert!(error.contains("cancelled"), "{error}");
        assert!(cancelled_at.elapsed() < Duration::from_millis(500));
        assert_eq!(std::fs::read(&partial).unwrap(), b"abc");
        server.join().unwrap();
        let _ = std::fs::remove_file(partial);
    }
}
