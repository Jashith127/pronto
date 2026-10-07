#[cfg(target_os = "macos")]
use core_foundation::base::TCFType;
#[cfg(target_os = "macos")]
use core_foundation::number::CFNumber;
#[cfg(target_os = "macos")]
use core_foundation::string::CFString;
#[cfg(target_os = "macos")]
use core_graphics::window::{
    create_description_from_array, create_window_list, kCGNullWindowID, kCGWindowLayer,
    kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly, kCGWindowName,
    kCGWindowOwnerPID,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
#[cfg(windows)]
use windows::core::BOOL;
#[cfg(windows)]
use windows::Win32::Foundation::{HWND, LPARAM};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, IsWindowVisible,
};

#[cfg(windows)]
type MeetingWindow = HWND;
#[cfg(target_os = "macos")]
type MeetingWindow = i32;

/// Brief nag guard for connection blips that look like new sessions.
/// Deliberate dismissals are suppressed per session instead, so genuine
/// rejoins prompt again after this short window.
const PROMPT_COOLDOWN: Duration = Duration::from_secs(2 * 60);
/// Consecutive matching polls before a new session prompts (~4s at 2s cadence)
/// when the microphone signal is unavailable and only titles can be checked.
const STABILITY_POLLS: u8 = 2;
/// Consecutive polls a meeting app must hold the microphone before prompting
/// (~10s), so a quick mic test or voice memo does not count as a call.
const MIC_STABILITY_POLLS: u8 = 5;

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
                if !app.state::<crate::AppState>().meeting_suggestions_enabled() {
                    // Suggestions disabled in Settings: stay frozen so that
                    // re-enabling mid-meeting prompts promptly.
                    continue;
                }
                let (found, mic_gated) = live_meeting_window();
                let required_polls = if mic_gated {
                    MIC_STABILITY_POLLS
                } else {
                    STABILITY_POLLS
                };
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
                        if stable == required_polls
                            && !control.is_suppressed(generation)
                            && cooldown_over
                        {
                            app.state::<crate::AppState>().claim_overlay();
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
        if contains_word(lower, needle) {
            return Some(key);
        }
    }
    None
}

/// Whole-word match, so "meet" does not fire on "meeting" and "teams" does
/// not fire on "teamspeak".
fn contains_word(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(start, _)| {
        let before = haystack[..start].chars().next_back();
        let after = haystack[start + needle.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// The first visible meeting window whose app is actually holding the
/// microphone. The flag reports whether the microphone signal was
/// available; without it the detector falls back to title matching alone.
fn live_meeting_window() -> (Option<(String, MeetingWindow)>, bool) {
    let candidates = meeting_windows();
    if candidates.is_empty() {
        return (None, false);
    }
    match mic_users() {
        Some(users) => (
            candidates
                .into_iter()
                .find(|(_, window)| users.holds(*window)),
            true,
        ),
        None => (candidates.into_iter().next(), false),
    }
}

#[cfg(windows)]
fn meeting_windows() -> Vec<(String, MeetingWindow)> {
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
                    (*(lparam.0 as *mut Vec<(String, HWND)>)).push((title, hwnd));
                }
            }
        }
        BOOL(1)
    }
    let mut result = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(collect),
            LPARAM((&mut result as *mut Vec<(String, HWND)>) as isize),
        );
    }
    result
}

/// Apps Windows reports as holding the microphone right now, from the
/// per-app privacy records behind the taskbar mic indicator. Each app key
/// carries LastUsedTimeStop, which is 0 while capture is in progress.
/// Desktop apps sit under `NonPackaged`, keyed by exe path with `#` for `\`.
#[cfg(windows)]
struct MicUsers {
    /// Lowercase exe file names of desktop apps, e.g. `zoom.exe`.
    executables: Vec<String>,
    /// Lowercase package family names of Store apps, e.g. `msteams_8wekyb3d8bbwe`.
    packages: Vec<String>,
}

#[cfg(windows)]
impl MicUsers {
    fn holds(&self, window: MeetingWindow) -> bool {
        let Some(path) = crate::meeting_icon::window_exe_path(window) else {
            return false;
        };
        let path = path.to_lowercase();
        let name = path.rsplit('\\').next().unwrap_or(&path);
        self.executables.iter().any(|exe| exe == name)
            || self
                .packages
                .iter()
                .any(|family| exe_in_package(&path, family))
    }
}

#[cfg(windows)]
const MIC_CONSENT_KEY: windows::core::PCWSTR = windows::core::w!(
    "Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\microphone"
);

#[cfg(windows)]
fn mic_users() -> Option<MicUsers> {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    };
    let mut root = HKEY::default();
    if unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            MIC_CONSENT_KEY,
            None,
            KEY_READ,
            &mut root,
        )
    } != ERROR_SUCCESS
    {
        return None;
    }
    let mut users = MicUsers {
        executables: Vec::new(),
        packages: Vec::new(),
    };
    for name in registry_subkeys(root) {
        if name.eq_ignore_ascii_case("NonPackaged") {
            let mut desktop = HKEY::default();
            let opened = unsafe {
                RegOpenKeyExW(
                    root,
                    &windows::core::HSTRING::from(name.as_str()),
                    None,
                    KEY_READ,
                    &mut desktop,
                )
            };
            if opened == ERROR_SUCCESS {
                for app in registry_subkeys(desktop) {
                    if capturing_now(desktop, &app) {
                        users.executables.push(nonpackaged_exe_name(&app));
                    }
                }
                unsafe {
                    let _ = RegCloseKey(desktop);
                }
            }
        } else if capturing_now(root, &name) {
            users.packages.push(name.to_lowercase());
        }
    }
    unsafe {
        let _ = RegCloseKey(root);
    }
    Some(users)
}

#[cfg(windows)]
fn registry_subkeys(key: windows::Win32::System::Registry::HKEY) -> Vec<String> {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::RegEnumKeyExW;
    let mut names = Vec::new();
    let mut buffer = [0u16; 512];
    for index in 0.. {
        let mut length = buffer.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                key,
                index,
                Some(windows::core::PWSTR(buffer.as_mut_ptr())),
                &mut length,
                None,
                None,
                None,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            break;
        }
        names.push(String::from_utf16_lossy(&buffer[..length as usize]));
    }
    names
}

/// True when the app key has started capturing and not yet stopped.
#[cfg(windows)]
fn capturing_now(parent: windows::Win32::System::Registry::HKEY, app: &str) -> bool {
    let start = registry_qword(parent, app, "LastUsedTimeStart");
    let stop = registry_qword(parent, app, "LastUsedTimeStop");
    matches!((start, stop), (Some(start), Some(0)) if start != 0)
}

#[cfg(windows)]
fn registry_qword(
    parent: windows::Win32::System::Registry::HKEY,
    subkey: &str,
    value: &str,
) -> Option<u64> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_QWORD};
    let mut data = 0u64;
    let mut size = std::mem::size_of::<u64>() as u32;
    let status = unsafe {
        RegGetValueW(
            parent,
            &HSTRING::from(subkey),
            &HSTRING::from(value),
            RRF_RT_REG_QWORD,
            None,
            Some((&mut data as *mut u64).cast()),
            Some(&mut size),
        )
    };
    (status == ERROR_SUCCESS).then_some(data)
}

/// `C:#Program Files#Zoom#bin#Zoom.exe` -> `zoom.exe`.
#[cfg(windows)]
fn nonpackaged_exe_name(key: &str) -> String {
    key.rsplit(['#', '\\']).next().unwrap_or(key).to_lowercase()
}

/// Store apps install to `WindowsApps\<Name>_<version>_<arch>__<publisher>`,
/// while the consent key is the family name `<Name>_<publisher>`.
#[cfg(windows)]
fn exe_in_package(path_lower: &str, family_lower: &str) -> bool {
    let Some((name, publisher)) = family_lower.rsplit_once('_') else {
        return false;
    };
    path_lower.contains(&format!("\\windowsapps\\{name}_"))
        && path_lower.contains(&format!("_{publisher}\\"))
}

#[cfg(target_os = "macos")]
fn meeting_windows() -> Vec<(String, MeetingWindow)> {
    let mut found = Vec::new();
    let Some(ids) = create_window_list(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID,
    ) else {
        return found;
    };
    let Some(windows) = create_description_from_array(ids) else {
        return found;
    };
    let name_key = unsafe { CFString::wrap_under_get_rule(kCGWindowName) };
    let owner_key = unsafe { CFString::wrap_under_get_rule(kCGWindowOwnerPID) };
    let layer_key = unsafe { CFString::wrap_under_get_rule(kCGWindowLayer) };
    for window in windows.iter() {
        let layer = window
            .find(&layer_key)
            .and_then(|value| value.downcast::<CFNumber>())
            .and_then(|value| value.to_i32());
        if layer != Some(0) {
            continue;
        }
        let Some(title) = window
            .find(&name_key)
            .and_then(|value| value.downcast::<CFString>())
            .map(|value| value.to_string())
        else {
            continue;
        };
        if vendor_key(&title.to_lowercase()).is_none() {
            continue;
        }
        let Some(pid) = window
            .find(&owner_key)
            .and_then(|value| value.downcast::<CFNumber>())
            .and_then(|value| value.to_i32())
        else {
            continue;
        };
        if pid > 0 && pid != std::process::id() as i32 {
            found.push((title, pid));
        }
    }
    found
}

/// Processes other than Pronto that are capturing audio input right now.
/// Browsers capture from helper processes whose pid differs from the
/// window owner, so any outside capture vouches for a meeting window.
#[cfg(target_os = "macos")]
struct MicUsers {
    pids: Vec<i32>,
}

#[cfg(target_os = "macos")]
impl MicUsers {
    fn holds(&self, _window: MeetingWindow) -> bool {
        !self.pids.is_empty()
    }
}

/// Per-process input state from the CoreAudio process objects (macOS 14+).
/// Older systems lack the property and fall back to title matching.
#[cfg(target_os = "macos")]
fn mic_users() -> Option<MicUsers> {
    use std::ffi::c_void;

    #[repr(C)]
    struct PropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioObjectGetPropertyDataSize(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            data_size: *mut u32,
        ) -> i32;
        fn AudioObjectGetPropertyData(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            data_size: *mut u32,
            data: *mut c_void,
        ) -> i32;
    }

    const SYSTEM_OBJECT: u32 = 1;
    const GLOBAL: u32 = u32::from_be_bytes(*b"glob");
    const PROCESS_OBJECT_LIST: u32 = u32::from_be_bytes(*b"prs#");
    const PROCESS_PID: u32 = u32::from_be_bytes(*b"ppid");
    const PROCESS_IS_RUNNING_INPUT: u32 = u32::from_be_bytes(*b"piri");

    fn address(selector: u32) -> PropertyAddress {
        PropertyAddress {
            selector,
            scope: GLOBAL,
            element: 0,
        }
    }

    fn read<T: Copy + Default>(object: u32, selector: u32) -> Option<T> {
        let mut value = T::default();
        let mut size = std::mem::size_of::<T>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                &address(selector),
                0,
                std::ptr::null(),
                &mut size,
                (&mut value as *mut T).cast(),
            )
        };
        (status == 0 && size == std::mem::size_of::<T>() as u32).then_some(value)
    }

    let mut size = 0u32;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            SYSTEM_OBJECT,
            &address(PROCESS_OBJECT_LIST),
            0,
            std::ptr::null(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let mut processes = vec![0u32; size as usize / std::mem::size_of::<u32>()];
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            &address(PROCESS_OBJECT_LIST),
            0,
            std::ptr::null(),
            &mut size,
            processes.as_mut_ptr().cast(),
        )
    };
    if status != 0 {
        return None;
    }
    processes.truncate(size as usize / std::mem::size_of::<u32>());
    let own = std::process::id() as i32;
    let pids = processes
        .into_iter()
        .filter(|&process| read::<u32>(process, PROCESS_IS_RUNNING_INPUT).unwrap_or(0) != 0)
        .filter_map(|process| read::<i32>(process, PROCESS_PID))
        .filter(|&pid| pid > 0 && pid != own)
        .collect();
    Some(MicUsers { pids })
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
        assert_eq!(vendor_key("meeting agenda - word"), None);
        assert_eq!(vendor_key("weekly meetup notes"), None);
        assert_eq!(vendor_key("teamspeak 3"), None);
        assert_eq!(vendor_key("zoomed screenshot.png - photos"), None);
    }

    #[test]
    fn whole_word_matching_respects_punctuation() {
        assert!(contains_word("meet - abc-defg-hij", "meet"));
        assert!(contains_word("https://meet.google.com/abc", "meet.google"));
        assert!(contains_word("(zoom) call", "zoom"));
        assert!(!contains_word("meetings", "meet"));
        assert!(!contains_word("unmeet", "meet"));
    }

    #[cfg(windows)]
    #[test]
    fn nonpackaged_keys_map_to_exe_names() {
        assert_eq!(
            nonpackaged_exe_name("C:#Program Files#Zoom#bin#Zoom.exe"),
            "zoom.exe"
        );
        assert_eq!(
            nonpackaged_exe_name("C:#Users#Ada#AppData#Local#Discord#app-1.0.9260#Discord.exe"),
            "discord.exe"
        );
    }

    #[cfg(windows)]
    #[test]
    fn store_app_paths_match_their_family() {
        let teams = "c:\\program files\\windowsapps\\msteams_25153.1010.3727.5483_x64__8wekyb3d8bbwe\\ms-teams.exe";
        assert!(exe_in_package(teams, "msteams_8wekyb3d8bbwe"));
        assert!(!exe_in_package(
            teams,
            "microsoft.screensketch_8wekyb3d8bbwe"
        ));
        assert!(!exe_in_package(
            "c:\\program files\\zoom\\bin\\zoom.exe",
            "msteams_8wekyb3d8bbwe"
        ));
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
