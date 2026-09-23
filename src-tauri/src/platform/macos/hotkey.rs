use core_foundation::runloop::{kCFRunLoopCommonModes, kCFRunLoopDefaultMode, CFRunLoop};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType, CallbackResult, EventField,
};
use serde::Serialize;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn pronto_carbon_hotkeys_start(callback: extern "C" fn(i32, i32)) -> i32;
    fn pronto_carbon_hotkey_update(identifier: i32, key: u32, modifiers: u32) -> i32;
    fn pronto_carbon_hotkeys_stop();
}

static CARBON_EVENTS: Mutex<Option<mpsc::Sender<(HotkeyId, HotkeyEvent)>>> = Mutex::new(None);

extern "C" fn carbon_event(identifier: i32, pressed: i32) {
    let id = match identifier {
        1 => HotkeyId::Dictation,
        2 => HotkeyId::Paste,
        3 => HotkeyId::Search,
        _ => return,
    };
    let event = if pressed != 0 {
        HotkeyEvent::Pressed
    } else {
        HotkeyEvent::Released
    };
    if let Ok(sender) = CARBON_EVENTS.lock() {
        if let Some(sender) = sender.as_ref() {
            let _ = sender.send((id, event));
        }
    }
}

fn carbon_identifier(id: HotkeyId) -> i32 {
    match id {
        HotkeyId::Dictation => 1,
        HotkeyId::Paste => 2,
        HotkeyId::Search => 3,
    }
}

fn carbon_modifiers(config: &Hotkey) -> u32 {
    (if config.win { 1 << 8 } else { 0 })
        | (if config.shift { 1 << 9 } else { 0 })
        | (if config.alt { 1 << 11 } else { 0 })
        | (if config.control { 1 << 12 } else { 0 })
}

fn carbon_update(id: HotkeyId, config: &Hotkey) -> Result<(), String> {
    let key = config.key.unwrap_or(u32::MAX);
    let status = unsafe {
        pronto_carbon_hotkey_update(carbon_identifier(id), key, carbon_modifiers(config))
    };
    if status == 0 {
        Ok(())
    } else {
        Err(format!(
            "macOS could not register {} (OSStatus {status}); choose another shortcut",
            config.canonical()
        ))
    }
}

pub const DEFAULT_HOTKEY: &str = "control+alt+Space";
pub const DEFAULT_PASTE_HOTKEY: &str = "control+alt+KeyV";
pub const DEFAULT_SEARCH_HOTKEY: &str = "control+alt+KeyS";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyId {
    Dictation,
    Paste,
    Search,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyEvent {
    Pressed,
    Released,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub key: Option<u32>,
    canonical: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyStatus {
    pub shortcut: String,
    pub paste_shortcut: String,
    pub search_shortcut: String,
    pub registered: bool,
    pub error: Option<String>,
    pub paste_error: Option<String>,
    pub search_error: Option<String>,
}

impl Hotkey {
    pub fn canonical(&self) -> &str {
        &self.canonical
    }

    pub fn modifier_only(&self) -> bool {
        self.key.is_none()
    }

    fn matches(&self, flags: CGEventFlags, pressed: &HashSet<u32>) -> bool {
        flags.contains(CGEventFlags::CGEventFlagControl) == self.control
            && flags.contains(CGEventFlags::CGEventFlagAlternate) == self.alt
            && flags.contains(CGEventFlags::CGEventFlagShift) == self.shift
            && flags.contains(CGEventFlags::CGEventFlagCommand) == self.win
            && self.key.is_none_or(|key| pressed.contains(&key))
    }
}

pub fn shortcuts_conflict(first: &Hotkey, second: &Hotkey) -> bool {
    first.canonical() == second.canonical()
}

#[cfg(test)]
pub fn shortcuts_conflict_any(dictation: &Hotkey, paste: &Hotkey, search: &Hotkey) -> bool {
    shortcuts_conflict(dictation, paste)
        || shortcuts_conflict(dictation, search)
        || shortcuts_conflict(paste, search)
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
        .filter(|token| !token.trim().is_empty())
    {
        let token = token.trim();
        match token.to_ascii_lowercase().as_str() {
            "control" | "ctrl" => hotkey.control = true,
            "alt" | "option" => hotkey.alt = true,
            "shift" => hotkey.shift = true,
            "super" | "win" | "meta" | "command" | "cmd" => hotkey.win = true,
            _ => {
                if hotkey.key.is_some() {
                    return Err("A shortcut can contain only one non-modifier key".into());
                }
                let (key, canonical) = virtual_key(token)?;
                hotkey.key = Some(key);
                key_name = Some(canonical);
            }
        }
    }
    let modifiers = [hotkey.control, hotkey.alt, hotkey.shift, hotkey.win]
        .into_iter()
        .filter(|enabled| *enabled)
        .count();
    if hotkey.key.is_none() && modifiers < 2 {
        return Err("Use at least two modifier keys".into());
    }
    if hotkey.key.is_some() && !(hotkey.control || hotkey.alt || hotkey.win) {
        return Err("Use Control, Option, or Command so normal typing is unaffected".into());
    }
    if hotkey.win
        && !hotkey.control
        && !hotkey.alt
        && !hotkey.shift
        && matches!(hotkey.key, Some(49 | 48))
    {
        return Err("Command+Space and Command+Tab are reserved by macOS".into());
    }
    if hotkey.control && !hotkey.win && !hotkey.alt && !hotkey.shift && hotkey.key == Some(49) {
        return Err("Control+Space is used for input source switching".into());
    }
    let mut parts = Vec::new();
    if hotkey.control {
        parts.push("control");
    }
    if hotkey.alt {
        parts.push("alt");
    }
    if hotkey.shift {
        parts.push("shift");
    }
    if hotkey.win {
        parts.push("super");
    }
    hotkey.canonical = parts.join("+");
    if let Some(key_name) = key_name {
        hotkey.canonical.push('+');
        hotkey.canonical.push_str(&key_name);
    }
    Ok(hotkey)
}

fn virtual_key(token: &str) -> Result<(u32, String), String> {
    let lower = token.to_ascii_lowercase();
    let code = if lower.len() == 4 && lower.starts_with("key") {
        let letter = lower.as_bytes()[3] as char;
        let code = match letter {
            'a' => 0,
            'b' => 11,
            'c' => 8,
            'd' => 2,
            'e' => 14,
            'f' => 3,
            'g' => 5,
            'h' => 4,
            'i' => 34,
            'j' => 38,
            'k' => 40,
            'l' => 37,
            'm' => 46,
            'n' => 45,
            'o' => 31,
            'p' => 35,
            'q' => 12,
            'r' => 15,
            's' => 1,
            't' => 17,
            'u' => 32,
            'v' => 9,
            'w' => 13,
            'x' => 7,
            'y' => 16,
            'z' => 6,
            _ => return Err(format!("Unsupported shortcut key: {token}")),
        };
        return Ok((code, format!("Key{}", letter.to_ascii_uppercase())));
    } else if lower.len() == 6 && lower.starts_with("digit") {
        let digit = lower.as_bytes()[5] as char;
        let code = match digit {
            '0' => 29,
            '1' => 18,
            '2' => 19,
            '3' => 20,
            '4' => 21,
            '5' => 23,
            '6' => 22,
            '7' => 26,
            '8' => 28,
            '9' => 25,
            _ => return Err(format!("Unsupported shortcut key: {token}")),
        };
        return Ok((code, format!("Digit{digit}")));
    } else {
        match lower.as_str() {
            "space" => 49,
            "enter" => 36,
            "tab" => 48,
            "escape" => 53,
            "backspace" => 51,
            "arrowleft" => 123,
            "arrowright" => 124,
            "arrowdown" => 125,
            "arrowup" => 126,
            "home" => 115,
            "end" => 119,
            "pageup" => 116,
            "pagedown" => 121,
            "delete" => 117,
            _ => {
                return match lower.as_str() {
                    "f1" => Ok((122, "F1".into())),
                    "f2" => Ok((120, "F2".into())),
                    "f3" => Ok((99, "F3".into())),
                    "f4" => Ok((118, "F4".into())),
                    "f5" => Ok((96, "F5".into())),
                    "f6" => Ok((97, "F6".into())),
                    "f7" => Ok((98, "F7".into())),
                    "f8" => Ok((100, "F8".into())),
                    "f9" => Ok((101, "F9".into())),
                    "f10" => Ok((109, "F10".into())),
                    "f11" => Ok((103, "F11".into())),
                    "f12" => Ok((111, "F12".into())),
                    _ => Err(format!("Unsupported shortcut key: {token}")),
                };
            }
        }
    };
    let canonical = match lower.as_str() {
        "space" => "Space",
        "enter" => "Enter",
        "tab" => "Tab",
        "escape" => "Escape",
        "backspace" => "Backspace",
        "arrowleft" => "ArrowLeft",
        "arrowright" => "ArrowRight",
        "arrowdown" => "ArrowDown",
        "arrowup" => "ArrowUp",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "delete" => "Delete",
        _ => unreachable!(),
    };
    Ok((code, canonical.into()))
}

struct WatchedHotkey {
    id: HotkeyId,
    config: Hotkey,
    active: bool,
}

struct HookState {
    watched: Vec<WatchedHotkey>,
    pressed: HashSet<u32>,
    events: mpsc::Sender<(HotkeyId, HotkeyEvent)>,
}

impl HookState {
    fn handle(&mut self, event_type: CGEventType, event: &CGEvent) {
        if matches!(
            event_type,
            CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
        ) {
            self.pressed.clear();
            for watched in &mut self.watched {
                if watched.config.key.is_none() && watched.active {
                    watched.active = false;
                    let _ = self.events.send((watched.id, HotkeyEvent::Released));
                }
            }
            return;
        }
        let key = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u32;
        match event_type {
            CGEventType::KeyDown => {
                self.pressed.insert(key);
            }
            CGEventType::KeyUp => {
                self.pressed.remove(&key);
            }
            _ => {}
        }
        let flags = event.get_flags();
        for watched in &mut self.watched {
            if watched.config.key.is_some() {
                continue;
            }
            let active = watched.config.matches(flags, &self.pressed);
            if active != watched.active {
                watched.active = active;
                let _ = self.events.send((
                    watched.id,
                    if active {
                        HotkeyEvent::Pressed
                    } else {
                        HotkeyEvent::Released
                    },
                ));
            }
        }
    }
}

pub struct HotkeyController {
    state: Arc<Mutex<HookState>>,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
    modifier_tap_available: AtomicBool,
    modifier_tap_error: Mutex<Option<String>>,
}

impl HotkeyController {
    pub fn new(
        configs: Vec<(HotkeyId, Hotkey)>,
        callback: impl Fn(HotkeyId, HotkeyEvent) + Send + 'static,
    ) -> Result<Self, String> {
        let (events, event_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("pronto-hotkey-events".into())
            .spawn(move || {
                while let Ok((id, event)) = event_rx.recv() {
                    callback(id, event);
                }
            })
            .map_err(|error| format!("Could not start shortcut event thread: {error}"))?;
        let modifier_only = configs.iter().any(|(_, config)| config.key.is_none());
        let state = Arc::new(Mutex::new(HookState {
            watched: configs
                .into_iter()
                .map(|(id, config)| WatchedHotkey {
                    id,
                    config,
                    active: false,
                })
                .collect(),
            pressed: HashSet::new(),
            events: events.clone(),
        }));
        *CARBON_EVENTS
            .lock()
            .map_err(|_| "Shortcut channel lock poisoned")? = Some(events);
        let carbon_status = unsafe { pronto_carbon_hotkeys_start(carbon_event) };
        if carbon_status != 0 {
            *CARBON_EVENTS
                .lock()
                .map_err(|_| "Shortcut channel lock poisoned")? = None;
            return Err(format!(
                "Could not start macOS registered shortcuts (OSStatus {carbon_status})"
            ));
        }
        for watched in &state
            .lock()
            .map_err(|_| "Shortcut listener lock poisoned")?
            .watched
        {
            if let Err(error) = carbon_update(watched.id, &watched.config) {
                unsafe { pronto_carbon_hotkeys_stop() };
                *CARBON_EVENTS
                    .lock()
                    .map_err(|_| "Shortcut channel lock poisoned")? = None;
                return Err(error);
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        let controller = Self {
            state,
            stop,
            thread: Mutex::new(None),
            modifier_tap_available: AtomicBool::new(false),
            modifier_tap_error: Mutex::new(None),
        };
        if modifier_only {
            // A denied Input Monitoring grant affects modifier-only chords,
            // not Carbon's independently registered key-based shortcuts.
            let _ = controller.ensure_modifier_tap();
        }
        Ok(controller)
    }

    fn ensure_modifier_tap(&self) -> Result<(), String> {
        let result = self.start_modifier_tap();
        if let Ok(mut error) = self.modifier_tap_error.lock() {
            *error = result.as_ref().err().cloned();
        }
        result
    }

    pub fn modifier_tap_error(&self) -> Option<String> {
        self.modifier_tap_error.lock().ok()?.clone()
    }

    fn start_modifier_tap(&self) -> Result<(), String> {
        if self.modifier_tap_available.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut owned_thread = self
            .thread
            .lock()
            .map_err(|_| "Shortcut listener lock poisoned".to_string())?;
        if owned_thread.is_some() {
            return Ok(());
        }
        let (ready, response) = mpsc::channel();
        let thread_state = Arc::clone(&self.state);
        let thread_stop = Arc::clone(&self.stop);
        let thread = std::thread::Builder::new()
            .name("pronto-macos-event-tap".into())
            .spawn(move || {
                let tap = CGEventTap::new(
                    CGEventTapLocation::Session,
                    CGEventTapPlacement::HeadInsertEventTap,
                    CGEventTapOptions::ListenOnly,
                    vec![CGEventType::KeyDown, CGEventType::KeyUp, CGEventType::FlagsChanged],
                    move |_, event_type, event| {
                        if let Ok(mut state) = thread_state.lock() { state.handle(event_type, event); }
                        CallbackResult::Keep
                    },
                );
                let Ok(tap) = tap else {
                    let _ = ready.send(Err("Enable Input Monitoring for Pronto in System Settings → Privacy & Security, then retry the shortcut".to_string()));
                    return;
                };
                let Ok(source) = tap.mach_port().create_runloop_source(0) else {
                    let _ = ready.send(Err("Could not create the macOS keyboard event source".to_string()));
                    return;
                };
                CFRunLoop::get_current().add_source(&source, unsafe { kCFRunLoopCommonModes });
                tap.enable();
                let _ = ready.send(Ok(()));
                while !thread_stop.load(Ordering::Acquire) {
                    CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, Duration::from_millis(250), true);
                    tap.enable();
                }
            })
            .map_err(|error| format!("Could not start macOS shortcut listener: {error}"))?;
        let tap_result = response
            .recv()
            .map_err(|_| "Shortcut listener stopped during startup".to_string())
            .and_then(|value| value);
        if let Err(error) = tap_result {
            let _ = thread.join();
            return Err(error);
        }
        *owned_thread = Some(thread);
        self.modifier_tap_available.store(true, Ordering::Release);
        Ok(())
    }

    pub fn update(&self, id: HotkeyId, config: Hotkey) -> Result<(), String> {
        if config.key.is_none() {
            if !self.modifier_tap_available.load(Ordering::Acquire) {
                let _ = crate::permissions::request("input-monitoring");
            }
            self.ensure_modifier_tap()?;
        }
        carbon_update(id, &config)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Shortcut listener lock poisoned".to_string())?;
        let events = state.events.clone();
        let watched = state
            .watched
            .iter_mut()
            .find(|watched| watched.id == id)
            .ok_or_else(|| "Shortcut listener is not running".to_string())?;
        if watched.active {
            let _ = events.send((id, HotkeyEvent::Released));
        }
        watched.config = config;
        watched.active = false;
        state.pressed.clear();
        Ok(())
    }
}

impl Drop for HotkeyController {
    fn drop(&mut self) {
        unsafe { pronto_carbon_hotkeys_stop() };
        if let Ok(mut sender) = CARBON_EVENTS.lock() {
            *sender = None;
        }
        self.stop.store(true, Ordering::Release);
        if let Ok(mut thread) = self.thread.lock() {
            if let Some(thread) = thread.take() {
                let _ = thread.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_shortcut_defaults_are_distinct_and_not_os_reserved() {
        let a = parse(DEFAULT_HOTKEY).unwrap();
        let b = parse(DEFAULT_PASTE_HOTKEY).unwrap();
        let c = parse(DEFAULT_SEARCH_HOTKEY).unwrap();
        assert!(!shortcuts_conflict_any(&a, &b, &c));
        assert!(parse("super+Space").is_err());
        assert!(parse("super+Tab").is_err());
    }

    #[test]
    fn modifier_only_chord_round_trips() {
        assert_eq!(parse("Ctrl+Option").unwrap().canonical(), "control+alt");
    }
}
