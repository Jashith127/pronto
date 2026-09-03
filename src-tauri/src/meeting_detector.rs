use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, IsWindowVisible,
};

pub fn start(app: AppHandle, recording: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("pronto-meeting-detector".into())
        .spawn(move || {
            let mut candidate = String::new();
            let mut stable = 0u8;
            let mut dismissed = String::new();
            let mut last_prompt: Option<Instant> = None;
            loop {
                std::thread::sleep(Duration::from_secs(2));
                if recording.load(Ordering::Acquire) {
                    stable = 0;
                    continue;
                }
                let found = meeting_window_title().unwrap_or_default();
                if found.is_empty() {
                    candidate.clear();
                    stable = 0;
                    dismissed.clear();
                    continue;
                }
                if found != candidate {
                    candidate = found.clone();
                    stable = 1;
                } else {
                    stable = stable.saturating_add(1);
                }
                let cooldown_over =
                    last_prompt.is_none_or(|time| time.elapsed() >= Duration::from_secs(30 * 60));
                if stable == 4 && found != dismissed && cooldown_over {
                    if let Some(overlay) = app.get_webview_window("overlay") {
                        let _ = overlay.show();
                    }
                    let _ = app.emit("meeting-suggestion", serde_json::json!({ "title": found }));
                    dismissed = candidate.clone();
                    last_prompt = Some(Instant::now());
                }
            }
        })
        .expect("failed to start meeting detector");
}

fn meeting_window_title() -> Option<String> {
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        if unsafe { !IsWindowVisible(hwnd).as_bool() } {
            return BOOL(1);
        }
        let length = unsafe { GetWindowTextLengthW(hwnd) };
        if length <= 0 {
            return BOOL(1);
        }
        let mut buffer = vec![0u16; length as usize + 1];
        let read = unsafe { GetWindowTextW(hwnd, &mut buffer) };
        if read > 0 {
            let title = String::from_utf16_lossy(&buffer[..read as usize]);
            let lower = title.to_lowercase();
            let matches = [
                "google meet",
                "zoom meeting",
                "microsoft teams",
                "webex",
                "slack huddle",
                "discord",
            ]
            .iter()
            .any(|needle| lower.contains(needle));
            if matches {
                unsafe {
                    *(lparam.0 as *mut Option<String>) = Some(title);
                }
                return BOOL(0);
            }
        }
        BOOL(1)
    }
    let mut result = None;
    unsafe {
        let _ = EnumWindows(
            Some(collect),
            LPARAM((&mut result as *mut Option<String>) as isize),
        );
    }
    result
}
