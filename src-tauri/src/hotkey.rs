use serde::Serialize;
use std::collections::HashSet;
use std::sync::{mpsc, Mutex, OnceLock};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL, VK_RMENU,
    VK_RSHIFT, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_APP,
    WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

pub const DEFAULT_HOTKEY: &str = "control+alt+Space";
const STOP_HOOK: u32 = WM_APP + 0x564;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub key: Option<u32>,
    canonical: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyEvent {
    Pressed,
    Released,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyStatus {
    pub shortcut: String,
    pub registered: bool,
    pub error: Option<String>,
}

impl Hotkey {
    pub fn canonical(&self) -> &str {
        &self.canonical
    }
    fn matches(&self, pressed: &HashSet<u32>) -> bool {
        modifier_down(
            pressed,
            VK_CONTROL.0 as u32,
            VK_LCONTROL.0 as u32,
            VK_RCONTROL.0 as u32,
        ) == self.control
            && modifier_down(
                pressed,
                VK_MENU.0 as u32,
                VK_LMENU.0 as u32,
                VK_RMENU.0 as u32,
            ) == self.alt
            && modifier_down(
                pressed,
                VK_SHIFT.0 as u32,
                VK_LSHIFT.0 as u32,
                VK_RSHIFT.0 as u32,
            ) == self.shift
            && (pressed.contains(&(VK_LWIN.0 as u32)) || pressed.contains(&(VK_RWIN.0 as u32)))
                == self.win
            && self.key.map(|key| pressed.contains(&key)).unwrap_or(true)
    }
}

fn modifier_down(pressed: &HashSet<u32>, generic: u32, left: u32, right: u32) -> bool {
    pressed.contains(&generic) || pressed.contains(&left) || pressed.contains(&right)
}

pub fn parse(value: &str) -> Result<Hotkey, String> {
    let mut hotkey = Hotkey {
        control: false,
        alt: false,
        shift: false,
        win: false,
        key: None,
        canonical: String::new(),
    };
    let mut key_name = None;
    for token in value
        .trim()
        .split('+')
        .filter(|part| !part.trim().is_empty())
    {
        let token = token.trim();
        match token.to_ascii_lowercase().as_str() {
            "control" | "ctrl" => hotkey.control = true,
            "alt" => hotkey.alt = true,
            "shift" => hotkey.shift = true,
            "super" | "win" | "meta" => hotkey.win = true,
            _ => {
                if hotkey.key.is_some() {
                    return Err("A shortcut can contain only one non-modifier key".into());
                }
                let (vk, canonical) = virtual_key(token)?;
                hotkey.key = Some(vk);
                key_name = Some(canonical);
            }
        }
    }
    let modifier_count = [hotkey.control, hotkey.alt, hotkey.shift, hotkey.win]
        .into_iter()
        .filter(|enabled| *enabled)
        .count();
    if hotkey.key.is_none() && modifier_count < 2 {
        return Err("Use at least two modifier keys, such as Win + Ctrl".into());
    }
    if hotkey.key.is_some() && !(hotkey.control || hotkey.alt || hotkey.win) {
        return Err("Use Ctrl, Alt, or Win so normal typing is unaffected".into());
    }
    if hotkey.key == Some(0x7B) {
        return Err("F12 is reserved by Windows debuggers; choose another key".into());
    }
    if is_reserved_windows_chord(&hotkey) {
        return Err("That shortcut is reserved by Windows. Choose another combination".into());
    }
    let mut parts = Vec::new();
    if hotkey.control {
        parts.push("control".to_string());
    }
    if hotkey.alt {
        parts.push("alt".to_string());
    }
    if hotkey.shift {
        parts.push("shift".to_string());
    }
    if hotkey.win {
        parts.push("super".to_string());
    }
    if let Some(key) = key_name {
        parts.push(key);
    }
    hotkey.canonical = parts.join("+");
    Ok(hotkey)
}

fn virtual_key(token: &str) -> Result<(u32, String), String> {
    let lower = token.to_ascii_lowercase();
    if lower.len() == 4 && lower.starts_with("key") {
        let letter = lower.as_bytes()[3];
        if letter.is_ascii_alphabetic() {
            let upper = (letter as char).to_ascii_uppercase();
            return Ok((upper as u32, format!("Key{upper}")));
        }
    }
    if lower.len() == 6 && lower.starts_with("digit") {
        let digit = lower.as_bytes()[5];
        if digit.is_ascii_digit() {
            return Ok((digit as u32, format!("Digit{}", digit as char)));
        }
    }
    if let Some(number) = lower.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=24).contains(&number) {
            return Ok((0x6F + number, format!("F{number}")));
        }
    }
    let mapped = match lower.as_str() {
        "space" => (0x20, "Space"),
        "enter" => (0x0D, "Enter"),
        "tab" => (0x09, "Tab"),
        "escape" => (0x1B, "Escape"),
        "backspace" => (0x08, "Backspace"),
        "arrowleft" => (0x25, "ArrowLeft"),
        "arrowup" => (0x26, "ArrowUp"),
        "arrowright" => (0x27, "ArrowRight"),
        "arrowdown" => (0x28, "ArrowDown"),
        "home" => (0x24, "Home"),
        "end" => (0x23, "End"),
        "pageup" => (0x21, "PageUp"),
        "pagedown" => (0x22, "PageDown"),
        "insert" => (0x2D, "Insert"),
        "delete" => (0x2E, "Delete"),
        _ => return Err(format!("Unsupported shortcut key: {token}")),
    };
    Ok((mapped.0, mapped.1.into()))
}

fn is_reserved_windows_chord(hotkey: &Hotkey) -> bool {
    hotkey.win
        && !hotkey.control
        && !hotkey.alt
        && !hotkey.shift
        && matches!(hotkey.key, Some(0x4C | 0x55 | 0x52 | 0x49 | 0x45 | 0x44))
}

struct HookState {
    config: Hotkey,
    pressed: HashSet<u32>,
    active: bool,
    events: mpsc::Sender<HotkeyEvent>,
}
static HOOK_STATE: OnceLock<Mutex<Option<HookState>>> = OnceLock::new();

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let message = wparam.0 as u32;
        if matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP) {
            let data = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            if let Ok(mut guard) = HOOK_STATE.get_or_init(|| Mutex::new(None)).lock() {
                if let Some(state) = guard.as_mut() {
                    if matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN) {
                        state.pressed.insert(data.vkCode);
                    } else {
                        state.pressed.remove(&data.vkCode);
                    }
                    let active = state.config.matches(&state.pressed);
                    if active != state.active {
                        state.active = active;
                        let _ = state.events.send(if active {
                            HotkeyEvent::Pressed
                        } else {
                            HotkeyEvent::Released
                        });
                    }
                }
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

pub struct HotkeyController {
    thread_id: u32,
}

impl HotkeyController {
    pub fn new(
        config: Hotkey,
        callback: impl Fn(HotkeyEvent) + Send + 'static,
    ) -> Result<Self, String> {
        let (events, event_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("pronto-hotkey-events".into())
            .spawn(move || {
                while let Ok(event) = event_rx.recv() {
                    callback(event);
                }
            })
            .map_err(|error| format!("Could not start shortcut event thread: {error}"))?;
        let (ready_tx, ready_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("pronto-keyboard-hook".into())
            .spawn(move || {
                let thread_id = unsafe { GetCurrentThreadId() };
                if let Ok(mut state) = HOOK_STATE.get_or_init(|| Mutex::new(None)).lock() {
                    *state = Some(HookState {
                        config,
                        pressed: HashSet::new(),
                        active: false,
                        events,
                    });
                }
                match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), None, 0) } {
                    Ok(hook) => {
                        let _ = ready_tx.send(Ok(thread_id));
                        let mut message = MSG::default();
                        while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
                            if message.message == STOP_HOOK {
                                break;
                            }
                            unsafe {
                                let _ = TranslateMessage(&message);
                                DispatchMessageW(&message);
                            }
                        }
                        let _ = unsafe { UnhookWindowsHookEx(hook) };
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(format!(
                            "Could not install Windows keyboard hook: {error}"
                        )));
                    }
                }
                if let Ok(mut state) = HOOK_STATE.get_or_init(|| Mutex::new(None)).lock() {
                    *state = None;
                }
            })
            .map_err(|error| format!("Could not start shortcut listener: {error}"))?;
        let thread_id = ready_rx
            .recv()
            .map_err(|_| "Shortcut listener stopped during startup".to_string())??;
        Ok(Self { thread_id })
    }

    pub fn update(&self, config: Hotkey) -> Result<(), String> {
        let mut guard = HOOK_STATE
            .get_or_init(|| Mutex::new(None))
            .lock()
            .map_err(|_| "shortcut listener lock poisoned")?;
        let state = guard
            .as_mut()
            .ok_or_else(|| "Shortcut listener is not running".to_string())?;
        if state.active {
            let _ = state.events.send(HotkeyEvent::Released);
        }
        state.config = config;
        state.pressed.clear();
        state.active = false;
        Ok(())
    }
}

impl Drop for HotkeyController {
    fn drop(&mut self) {
        let _ = unsafe { PostThreadMessageW(self.thread_id, STOP_HOOK, WPARAM(0), LPARAM(0)) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_modifier_only_windows_control() {
        let shortcut = parse("Win+Ctrl").unwrap();
        assert!(shortcut.win && shortcut.control);
        assert_eq!(shortcut.key, None);
        assert_eq!(shortcut.canonical(), "control+super");
    }
    #[test]
    fn validates_safe_combinations() {
        assert!(parse("KeyV").is_err());
        assert!(parse("Shift+KeyV").is_err());
        assert!(parse("Control+F12").is_err());
        assert!(parse("Win+L").is_err());
        assert_eq!(
            parse("Super+Shift+KeyV").unwrap().canonical(),
            "shift+super+KeyV"
        );
    }

    #[test]
    #[ignore = "injects a real Win+Ctrl chord on the interactive Windows desktop"]
    fn native_hook_observes_modifier_only_chord() {
        use std::mem::size_of;
        use std::time::Duration;
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_LWIN,
        };
        let (sender, receiver) = mpsc::channel();
        let _controller = HotkeyController::new(parse("Win+Ctrl").unwrap(), move |event| {
            let _ = sender.send(event);
        })
        .unwrap();
        let input = |key, flags| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: key,
                    dwFlags: flags,
                    ..Default::default()
                },
            },
        };
        let inputs = [
            input(VK_LWIN, Default::default()),
            input(VK_LCONTROL, Default::default()),
            input(VK_LCONTROL, KEYEVENTF_KEYUP),
            input(VK_LWIN, KEYEVENTF_KEYUP),
        ];
        assert_eq!(unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) }, 4);
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            HotkeyEvent::Pressed
        );
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            HotkeyEvent::Released
        );
    }
}
