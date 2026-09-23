//! First-use, local model provisioning for the macOS Metal runtime.
use fs2::available_space;
use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::Client;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

const MANIFEST: &str = include_str!("../model.sha256");

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallStatus {
    pub phase: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub message: String,
}

pub struct ModelProvisioner {
    active: AtomicBool,
    cancel: Arc<AtomicBool>,
    status: Mutex<InstallStatus>,
}

impl ModelProvisioner {
    pub fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            cancel: Arc::new(AtomicBool::new(false)),
            status: Mutex::new(InstallStatus {
                phase: "checking".into(),
                downloaded_bytes: 0,
                total_bytes: model_spec().map(|spec| spec.size).unwrap_or(0),
                message: "Checking local speech model…".into(),
            }),
        }
    }

    pub fn status(&self) -> Result<InstallStatus, String> {
        self.status
            .lock()
            .map(|s| s.clone())
            .map_err(|e| e.to_string())
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub fn start(&self, app: AppHandle) -> Result<(), String> {
        if self.active.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.cancel.store(false, Ordering::Release);
        let cancel = Arc::clone(&self.cancel);
        std::thread::Builder::new()
            .name("pronto-model-download".into())
            .spawn(move || {
                let provisioner = app.state::<ModelProvisioner>();
                let result = install(&app, &provisioner, &cancel);
                if let Err(error) = result {
                    let phase = if cancel.load(Ordering::Acquire) {
                        "cancelled"
                    } else {
                        "error"
                    };
                    provisioner.publish(&app, phase, 0, error);
                } else if let Some(engine) = app
                    .state::<crate::AppState>()
                    .engine
                    .lock()
                    .ok()
                    .and_then(|e| e.as_ref().cloned())
                {
                    engine.warm_after_install();
                }
                provisioner.active.store(false, Ordering::Release);
            })
            .map_err(|e| {
                self.active.store(false, Ordering::Release);
                e.to_string()
            })?;
        Ok(())
    }

    fn publish(&self, app: &AppHandle, phase: &str, downloaded: u64, message: impl Into<String>) {
        let status = InstallStatus {
            phase: phase.into(),
            downloaded_bytes: downloaded,
            total_bytes: model_spec().map(|spec| spec.size).unwrap_or(0),
            message: message.into(),
        };
        if let Ok(mut current) = self.status.lock() {
            *current = status.clone();
        }
        let _ = app.emit("model-install-status", status);
    }
}

struct ModelSpec {
    filename: String,
    url: String,
    hash: String,
    size: u64,
}

fn model_spec() -> Result<ModelSpec, String> {
    let values: std::collections::HashMap<_, _> = MANIFEST
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect();
    let get = |key: &str| {
        values
            .get(key)
            .copied()
            .ok_or_else(|| format!("Model manifest lacks {key}"))
    };
    let url = get("URL")?.to_string();
    if !url.starts_with("https://") {
        return Err("Model URL must use HTTPS".into());
    }
    Ok(ModelSpec {
        filename: get("FILE")?.to_string(),
        url,
        hash: get("SHA256")?.to_ascii_lowercase(),
        size: get("SIZE_BYTES")?
            .parse()
            .map_err(|_| "Invalid model size")?,
    })
}

fn verify(path: &Path, spec: &ModelSpec) -> Result<bool, String> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    if file.metadata().map_err(|e| e.to_string())?.len() != spec.size {
        return Ok(false);
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()) == spec.hash)
}

/// A crash can leave a fully downloaded `.part` file before the final rename.
/// Validate it locally before making any HTTP range request.
fn install_complete_partial(
    partial: &Path,
    final_path: &Path,
    spec: &ModelSpec,
) -> Result<bool, String> {
    if !verify(partial, spec)? {
        fs::remove_file(partial).map_err(|e| e.to_string())?;
        return Ok(false);
    }
    fs::rename(partial, final_path)
        .map_err(|e| format!("Could not install verified model: {e}"))?;
    Ok(true)
}

pub fn verified_model(path: &Path) -> bool {
    model_spec()
        .and_then(|spec| verify(path, &spec))
        .unwrap_or(false)
}

fn install(
    app: &AppHandle,
    provisioner: &ModelProvisioner,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let spec = model_spec()?;
    let root = crate::platform_paths::data_dir().join("models");
    fs::create_dir_all(&root).map_err(|e| format!("Could not create model folder: {e}"))?;
    let final_path = root.join(&spec.filename);
    provisioner.publish(app, "checking", 0, "Checking local speech model…");
    if verify(&final_path, &spec)? {
        provisioner.publish(
            app,
            "installed",
            spec.size,
            "Model verified. Warming Parakeet…",
        );
        return Ok(());
    }
    if final_path.exists() {
        fs::remove_file(&final_path).map_err(|e| format!("Could not remove corrupt model: {e}"))?;
    }
    let partial = root.join(format!("{}.part", spec.filename));
    let mut offset = partial.metadata().map(|m| m.len()).unwrap_or(0);
    if offset == spec.size {
        provisioner.publish(
            app,
            "verifying",
            offset,
            "Verifying saved Parakeet download…",
        );
        if install_complete_partial(&partial, &final_path, &spec)? {
            provisioner.publish(
                app,
                "installed",
                spec.size,
                "Model verified. Warming Parakeet…",
            );
            return Ok(());
        }
        offset = 0;
    } else if offset > spec.size {
        fs::remove_file(&partial).map_err(|e| e.to_string())?;
        offset = 0;
    }
    let free =
        available_space(&root).map_err(|e| format!("Could not check free disk space: {e}"))?;
    let needed = spec
        .size
        .saturating_sub(offset)
        .saturating_add(100 * 1024 * 1024);
    if free < needed {
        return Err(format!(
            "Not enough free disk space for Parakeet. Need {} MB more.",
            (needed - free).div_ceil(1024 * 1024)
        ));
    }
    if cancel.load(Ordering::Acquire) {
        return Err("Model download cancelled".into());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Could not start model downloader: {e}"))?;
    runtime.block_on(download_partial(
        cancel,
        &spec,
        &partial,
        offset,
        |downloaded| {
            provisioner.publish(
                app,
                "downloading",
                downloaded,
                format!("Downloading Parakeet… {}%", downloaded * 100 / spec.size),
            );
        },
    ))?;
    provisioner.publish(app, "verifying", spec.size, "Verifying Parakeet checksum…");
    if !verify(&partial, &spec)? {
        fs::remove_file(&partial).ok();
        return Err("Parakeet checksum did not match; download again".into());
    }
    fs::rename(&partial, &final_path)
        .map_err(|e| format!("Could not install verified model: {e}"))?;
    provisioner.publish(
        app,
        "installed",
        spec.size,
        "Model verified. Warming Parakeet…",
    );
    Ok(())
}

async fn wait_for_cancel(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn download_partial(
    cancel: &AtomicBool,
    spec: &ModelSpec,
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
    let mut request = client.get(&spec.url);
    if offset > 0 {
        request = request.header(RANGE, format!("bytes={offset}-"));
    }
    let mut response = tokio::select! {
        response = request.send() => response.map_err(|e| format!("Could not download Parakeet: {e}"))?,
        _ = wait_for_cancel(cancel) => return Err("Model download cancelled; progress saved for retry".into()),
    };
    let status = response.status();
    if offset > 0 && status.as_u16() == 206 {
        let range = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !valid_content_range(range, offset, spec.size) {
            return Err("Server returned an unexpected model byte range".into());
        }
    } else if status.is_success() {
        offset = 0;
    } else {
        return Err(format!("Model download failed: HTTP {status}"));
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
            chunk = response.chunk() => chunk.map_err(|e| format!("Model download interrupted: {e}"))?,
            _ = wait_for_cancel(cancel) => {
                file.sync_all().map_err(|e| e.to_string())?;
                return Err("Model download cancelled; progress saved for retry".into());
            },
        };
        let Some(chunk) = chunk else {
            break;
        };
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        if downloaded > spec.size {
            return Err("Model download exceeded expected size".into());
        }
        if published.elapsed() >= Duration::from_millis(250) {
            progress(downloaded);
            published = Instant::now();
        }
    }
    file.sync_all().map_err(|e| e.to_string())?;
    if downloaded != spec.size {
        return Err(format!(
            "Model download incomplete: {downloaded} of {} bytes",
            spec.size
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
    #[test]
    fn manifest_is_complete() {
        let spec = model_spec().unwrap();
        assert_eq!(spec.filename, "parakeet-tdt-0.6b-v3.q8_0.gguf");
        assert_eq!(spec.hash.len(), 64);
        assert!(spec.size > 700_000_000);
    }

    #[test]
    fn checksum_rejects_corrupt_model_with_correct_size() {
        let path = std::env::temp_dir().join(format!("pronto-model-check-{}", std::process::id()));
        fs::write(&path, b"abc").unwrap();
        let spec = ModelSpec {
            filename: "fixture".into(),
            url: "https://example.test/model".into(),
            hash: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
            size: 3,
        };
        assert!(verify(&path, &spec).unwrap());
        fs::write(&path, b"abd").unwrap();
        assert!(!verify(&path, &spec).unwrap());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn complete_partial_is_installed_only_after_verification() {
        let root = std::env::temp_dir().join(format!(
            "pronto-model-partial-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::create_dir_all(&root).unwrap();
        let partial = root.join("model.part");
        let final_path = root.join("model");
        let spec = ModelSpec {
            filename: "model".into(),
            url: "https://example.test/model".into(),
            hash: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
            size: 3,
        };
        fs::write(&partial, b"abd").unwrap();
        assert!(!install_complete_partial(&partial, &final_path, &spec).unwrap());
        assert!(!partial.exists());
        assert!(!final_path.exists());
        fs::write(&partial, b"abc").unwrap();
        assert!(install_complete_partial(&partial, &final_path, &spec).unwrap());
        assert!(!partial.exists());
        assert_eq!(fs::read(&final_path).unwrap(), b"abc");
        let _ = fs::remove_dir_all(root);
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
    fn cancel_interrupts_a_stalled_model_transfer_and_keeps_partial() {
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
        let partial = std::env::temp_dir().join(format!(
            "pronto-stalled-model-{}-{}.part",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let spec = ModelSpec {
            filename: "fixture".into(),
            url: format!("http://{address}/model"),
            hash: String::new(),
            size: 6,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let worker_path = partial.clone();
        let worker = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(download_partial(
                    &worker_cancel,
                    &spec,
                    &worker_path,
                    0,
                    |_| {},
                ))
        });
        let wait_started = Instant::now();
        while partial.metadata().map(|m| m.len()).unwrap_or(0) < 3 {
            assert!(wait_started.elapsed() < Duration::from_secs(3));
            std::thread::sleep(Duration::from_millis(10));
        }
        let cancelled_at = Instant::now();
        cancel.store(true, Ordering::Release);
        let error = worker.join().unwrap().unwrap_err();
        assert!(error.contains("cancelled"), "{error}");
        assert!(cancelled_at.elapsed() < Duration::from_millis(500));
        assert_eq!(fs::read(&partial).unwrap(), b"abc");
        server.join().unwrap();
        let _ = fs::remove_file(partial);
    }
}
