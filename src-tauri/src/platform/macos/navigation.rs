use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Runtime, Url};

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("pronto-navigation")
        .on_navigation(|webview, url| {
            let permitted = allowed(webview.label(), url);
            if !permitted {
                crate::log_dictation_step(&format!(
                    "blocked webview navigation: {} {} {}",
                    webview.label(),
                    url.scheme(),
                    url.host_str().unwrap_or("")
                ));
            }
            permitted
        })
        .on_page_load(|webview, payload| {
            if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                crate::log_dictation_step(&format!("webview loaded: {}", webview.label()));
            }
        })
        .build()
}

fn allowed(label: &str, url: &Url) -> bool {
    let local = url.scheme() == "tauri" && url.host_str() == Some("localhost");
    if local {
        return true;
    }
    if cfg!(debug_assertions)
        && matches!(url.scheme(), "http" | "https")
        && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
    {
        return true;
    }
    if label != "search" {
        return false;
    }
    if url.scheme() == "about" && url.path() == "blank" {
        return true;
    }
    if url.scheme() != "https"
        || url.host_str() != Some("www.youtube-nocookie.com")
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let Some(id) = url.path().strip_prefix("/embed/") else {
        return false;
    };
    id.len() == 11
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_local_pages_and_only_the_search_video_embed() {
        for label in ["main", "overlay", "search"] {
            assert!(allowed(
                label,
                &Url::parse("tauri://localhost/index.html").unwrap()
            ));
        }
        let video = Url::parse("https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ").unwrap();
        assert!(allowed("search", &video));
        assert!(!allowed("main", &video));
        for url in [
            "https://example.com/path",
            "https://tauri.localhost.example.com/",
            "http://tauri.localhost/search.html",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://www.youtube-nocookie.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ?autoplay=1",
        ] {
            assert!(!allowed("search", &Url::parse(url).unwrap()), "{url}");
        }
    }
}
