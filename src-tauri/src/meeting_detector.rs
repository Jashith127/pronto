use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, IsWindowVisible,
};

/// Brief nag guard for connection blips that look like new sessions.
/// Deliberate dismissals are suppressed per session instead, so genuine
/// rejoins prompt again after this short window.
const PROMPT_COOLDOWN: Duration = Duration::from_secs(2 * 60);
/// Consecutive matching polls before a new session prompts (~4s at 2s cadence).
const STABILITY_POLLS: u8 = 2;

/// Shared control between the detector thread and the dismiss command.
/// Generations identify continuous meeting sessions: the counter advances
/// on every absent -> present edge, so a meeting that goes away and comes
/// back always prompts again (subject to cooldown), while a dismissed
/// session stays quiet until it ends.
pub struct DetectorControl {
    generation: Mutex<u64>,
    suppressed: Mutex<u64>,
}

impl DetectorControl {
    pub fn new() -> Self {
        Self {
            generation: Mutex::new(0),
            suppressed: Mutex::new(u64::MAX),
        }
    }

    pub fn dismiss_current(&self) {
        let generation = self.generation.lock().map(|guard| *guard).unwrap_or(0);
        if let Ok(mut suppressed) = self.suppressed.lock() {
            *suppressed = generation;
        }
    }

    fn next_generation(&self) -> u64 {
        self.generation
            .lock()
            .map(|mut guard| {
                *guard += 1;
                *guard
            })
            .unwrap_or(0)
    }

    fn is_suppressed(&self, generation: u64) -> bool {
        self.suppressed
            .lock()
            .map(|guard| *guard == generation)
            .unwrap_or(false)
    }
}

pub fn start(
    app: AppHandle,
    meeting_recording: Arc<AtomicBool>,
    dictation_active: Arc<AtomicBool>,
    control: Arc<DetectorControl>,
) {
    std::thread::Builder::new()
        .name("pronto-meeting-detector".into())
        .spawn(move || {
            let mut present = false;
            let mut stable = 0u8;
            let mut key = String::new();
            let mut generation = 0u64;
            let mut last_prompt: Option<(String, Instant)> = None;
            loop {
                std::thread::sleep(Duration::from_secs(2));
                if meeting_recording.load(Ordering::Acquire)
                    || dictation_active.load(Ordering::Acquire)
                {
                    // Freeze all presence state while Pronto itself owns the
                    // microphone. Resuming must not look like a new session,
                    // or stopping notes would instantly re-prompt.
                    continue;
                }
                if !app
                    .state::<crate::AppState>()
                    .meeting_suggestions_enabled()
                {
                    // Suggestions disabled in Settings: stay frozen so that
                    // re-enabling mid-meeting prompts promptly.
                    continue;
                }
                let found = meeting_window_title();
                let found_key = found
                    .as_ref()
                    .and_then(|(title, _)| vendor_key(&title.to_lowercase()));
                match (found, found_key) {
                    (Some((title, hwnd)), Some(next_key)) => {
                        if !present || next_key != key {
                            present = true;
                            stable = 1;
                            key = next_key.to_string();
                            generation = control.next_generation();
                        } else {
                            stable = stable.saturating_add(1);
                        }
                        let cooldown_over = last_prompt.as_ref().is_none_or(|(last_key, time)| {
                            last_key != &key || time.elapsed() >= PROMPT_COOLDOWN
                        });
                        if stable == STABILITY_POLLS
                            && !control.is_suppressed(generation)
                            && cooldown_over
                        {
                            if let Some(overlay) = app.get_webview_window("overlay") {
                                let _ = overlay.show();
                            }
                            let icon = crate::meeting_icon::icon_for_window(hwnd);
                            let _ = app.emit(
                                "meeting-suggestion",
                                serde_json::json!({
                                    "title": title,
                                    "vendor": key,
                                    "icon": icon,
                                }),
                            );
                            last_prompt = Some((key.clone(), Instant::now()));
                        }
                    }
                    _ => {
                        present = false;
                        stable = 0;
                    }
                }
            }
        })
        .expect("failed to start meeting detector");
}

/// Maps a lowercase window title to a stable vendor identity. Strong
/// vendor matches come first; the trailing weak needles (e.g. a Zen
/// "Meet" tab title) still identify browser calls.
fn vendor_key(lower: &str) -> Option<&'static str> {
    for (needle, key) in [
        ("google meet", "gmeet"),
        ("meet.google", "gmeet"),
        ("zoom", "zoom"),
        ("microsoft teams", "teams"),
        ("teams meeting", "teams"),
        ("webex", "webex"),
        ("slack huddle", "slack"),
        ("discord", "discord"),
        ("jitsi", "jitsi"),
        ("chime", "chime"),
        ("skype", "skype"),
        ("facetime", "facetime"),
        ("voov", "voov"),
        ("lark", "lark"),
        ("dingtalk", "dingtalk"),
        ("gotomeeting", "gotomeeting"),
        ("whereby", "whereby"),
        ("bluejeans", "bluejeans"),
        ("huddle", "huddle"),
        ("teams", "teams"),
        ("meet", "meet-generic"),
    ] {
        if lower.contains(needle) {
            return Some(key);
        }
    }
    None
}

fn meeting_window_title() -> Option<(String, HWND)> {
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
            if vendor_key(&title.to_lowercase()).is_some() {
                unsafe {
                    *(lparam.0 as *mut Option<(String, HWND)>) = Some((title, hwnd));
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
            LPARAM((&mut result as *mut Option<(String, HWND)>) as isize),
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_keys_cover_common_clients() {
        assert_eq!(vendor_key("google meet - abc-defg-hij"), Some("gmeet"));
        assert_eq!(vendor_key("zoom meeting"), Some("zoom"));
        assert_eq!(vendor_key("zoom workplace"), Some("zoom"));
        assert_eq!(vendor_key("microsoft teams - standup"), Some("teams"));
        assert_eq!(vendor_key("meet"), Some("meet-generic"));
        assert_eq!(vendor_key("ada wong - meet"), Some("meet-generic"));
    }

    #[test]
    fn vendor_keys_reject_ordinary_windows() {
        assert_eq!(vendor_key("quarterly report - word"), None);
        assert_eq!(vendor_key("inbox - mail"), None);
        assert_eq!(vendor_key("pronto"), None);
    }

    #[test]
    fn dismiss_suppresses_only_its_generation() {
        let control = DetectorControl::new();
        let first = control.next_generation();
        control.dismiss_current();
        assert!(control.is_suppressed(first));
        let second = control.next_generation();
        assert!(!control.is_suppressed(second));
    }
}
