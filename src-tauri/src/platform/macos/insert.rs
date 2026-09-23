use core_foundation::runloop::{kCFRunLoopCommonModes, kCFRunLoopDefaultMode, CFRunLoop};
use core_graphics::event::{
    CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CallbackResult,
};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString, NSRunningApplication, NSWorkspace};
use objc2_foundation::NSString;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn pronto_focused_text_field_is_editable(pid: i32) -> i32;
    fn pronto_capture_focused_text_field(pid: i32) -> i32;
    fn pronto_clear_captured_text_field();
    fn pronto_insert_with_accessibility(
        pid: i32,
        utf8: *const std::ffi::c_char,
        use_captured: i32,
    ) -> i32;
    fn pronto_insert_with_pasteboard(
        pid: i32,
        utf8: *const std::ffi::c_char,
        use_captured: i32,
    ) -> i32;
}

#[derive(Default)]
struct TargetState {
    active: bool,
    target: isize,
    last_clicked: isize,
}

static TARGET_STATE: OnceLock<Arc<Mutex<TargetState>>> = OnceLock::new();
static LAST_EXTERNAL_APP: AtomicIsize = AtomicIsize::new(0);

pub struct InsertionTargetTracker {
    state: Arc<Mutex<TargetState>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    foreground_thread: Option<JoinHandle<()>>,
}

impl InsertionTargetTracker {
    pub fn new() -> Self {
        let state = Arc::new(Mutex::new(TargetState::default()));
        let _ = TARGET_STATE.set(Arc::clone(&state));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_state = Arc::clone(&state);
        let thread_stop = Arc::clone(&stop);
        let foreground_stop = Arc::clone(&stop);
        let foreground_thread = std::thread::Builder::new()
            .name("pronto-macos-foreground-target".into())
            .spawn(move || {
                while !foreground_stop.load(Ordering::Acquire) {
                    let pid = foreground_window();
                    if pid > 0 && pid != std::process::id() as isize {
                        LAST_EXTERNAL_APP.store(pid, Ordering::Release);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
            .ok();
        let thread = std::thread::Builder::new()
            .name("pronto-macos-click-target".into())
            .spawn(move || {
                let clicked = Arc::new(AtomicBool::new(false));
                let callback_clicked = Arc::clone(&clicked);
                let Ok(tap) = CGEventTap::new(
                    CGEventTapLocation::HID,
                    CGEventTapPlacement::HeadInsertEventTap,
                    CGEventTapOptions::ListenOnly,
                    vec![CGEventType::LeftMouseDown],
                    move |_, event_type, _| {
                        if matches!(event_type, CGEventType::LeftMouseDown) {
                            callback_clicked.store(true, Ordering::Release);
                        }
                        CallbackResult::Keep
                    },
                ) else {
                    return;
                };
                let Ok(source) = tap.mach_port().create_runloop_source(0) else {
                    return;
                };
                CFRunLoop::get_current().add_source(&source, unsafe { kCFRunLoopCommonModes });
                tap.enable();
                let mut clicked_at = None;
                while !thread_stop.load(Ordering::Acquire) {
                    CFRunLoop::run_in_mode(
                        unsafe { kCFRunLoopDefaultMode },
                        Duration::from_millis(30),
                        true,
                    );
                    if clicked.swap(false, Ordering::AcqRel) {
                        clicked_at = Some(Instant::now());
                    }
                    if clicked_at
                        .is_some_and(|time: Instant| time.elapsed() >= Duration::from_millis(80))
                    {
                        clicked_at = None;
                        let pid = foreground_window();
                        if pid > 0 && pid != std::process::id() as isize {
                            if let Ok(mut state) = thread_state.lock() {
                                if state.active {
                                    if unsafe { pronto_capture_focused_text_field(pid as i32) } != 0
                                    {
                                        state.target = pid;
                                        state.last_clicked = pid;
                                    }
                                } else if unsafe {
                                    pronto_focused_text_field_is_editable(pid as i32)
                                } != 0
                                {
                                    state.last_clicked = pid;
                                }
                            }
                        }
                    }
                    tap.enable();
                }
            })
            .ok();
        Self {
            state,
            stop,
            thread,
            foreground_thread,
        }
    }

    pub fn begin(&self, initial_target: isize) {
        if let Ok(mut state) = self.state.lock() {
            state.active = true;
            state.target = initial_target;
            unsafe {
                pronto_clear_captured_text_field();
                if initial_target > 0 && initial_target != std::process::id() as isize {
                    let _ = pronto_capture_focused_text_field(initial_target as i32);
                }
            }
        }
    }

    pub fn finish(&self, fallback: isize) -> isize {
        self.state
            .lock()
            .map(|mut state| {
                state.active = false;
                if state.target > 0 {
                    state.target
                } else {
                    fallback
                }
            })
            .unwrap_or(fallback)
    }

    pub fn cancel(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.active = false;
        }
        unsafe { pronto_clear_captured_text_field() };
    }
}

impl Drop for InsertionTargetTracker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.foreground_thread.take() {
            let _ = thread.join();
        }
        unsafe { pronto_clear_captured_text_field() };
    }
}

/// The process identifier is stable for the lifetime of a target app and can
/// be used to ask Accessibility for its focused control without activating it.
pub fn foreground_window() -> isize {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .map(|app| app.processIdentifier() as isize)
        .unwrap_or(0)
}

/// A button in Pronto's own window should still write to the user's last
/// editor. NSWorkspace activation tracking does not require Input Monitoring.
pub fn preferred_target() -> isize {
    let current = foreground_window();
    if current > 0 && current != std::process::id() as isize {
        LAST_EXTERNAL_APP.store(current, Ordering::Release);
        current
    } else {
        TARGET_STATE
            .get()
            .and_then(|state| state.lock().ok().map(|state| state.last_clicked))
            .filter(|pid| {
                *pid > 0
                    && NSRunningApplication::runningApplicationWithProcessIdentifier(*pid as i32)
                        .is_some()
            })
            .unwrap_or_else(|| LAST_EXTERNAL_APP.load(Ordering::Acquire))
    }
}

pub fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

pub fn insert_text(target: isize, text: &str) -> Result<(), String> {
    insert_text_with_target(target, text, false)
}

pub fn insert_dictation_text(target: isize, text: &str) -> Result<(), String> {
    let result = insert_text_with_target(target, text, true);
    unsafe { pronto_clear_captured_text_field() };
    result
}

fn insert_text_with_target(target: isize, text: &str, use_captured: bool) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    if copy_and_paste_inner(target, text, use_captured)? {
        Ok(())
    } else {
        let reason = if accessibility_trusted() {
            "Insertion could not be confirmed"
        } else {
            "Allow Accessibility for Pronto to insert into other apps"
        };
        Err(format!(
            "{reason}. Transcript saved in History; your clipboard was left unchanged."
        ))
    }
}

fn copy_and_paste_inner(target: isize, text: &str, use_captured: bool) -> Result<bool, String> {
    if text.is_empty() {
        return Ok(true);
    }
    let target = if target == std::process::id() as isize {
        0
    } else {
        target
    };
    if target > 0 && unsafe { AXIsProcessTrusted() } {
        match set_selected_text(target as i32, text, use_captured) {
            Ok(true) => return Ok(true),
            Ok(false) => return Ok(false),
            Err(_) => {}
        }
        return Ok(matches!(
            paste_and_verify(target as i32, text, use_captured)?,
            PasteAttempt::Verified
        ));
    }
    Ok(false)
}

pub fn copy_and_paste_focus(fallback: isize, text: &str) -> Result<bool, String> {
    if text.is_empty() {
        return Ok(true);
    }
    let last_clicked = TARGET_STATE
        .get()
        .and_then(|state| state.lock().ok().map(|state| state.last_clicked))
        .unwrap_or(0);
    let current = foreground_window();
    let candidates = [current, last_clicked, fallback];
    if unsafe { AXIsProcessTrusted() } {
        for pid in candidates {
            if pid > 0 && pid != std::process::id() as isize {
                match set_selected_text(pid as i32, text, false) {
                    Ok(true) => return Ok(true),
                    Ok(false) => return Ok(false),
                    Err(_) => {}
                }
            }
        }
        for pid in candidates {
            if pid <= 0 || pid == std::process::id() as isize {
                continue;
            }
            match paste_and_verify(pid as i32, text, false)? {
                PasteAttempt::Verified => return Ok(true),
                PasteAttempt::SentUnverified => return Ok(false),
                PasteAttempt::NotSent => continue,
            }
        }
    }
    Ok(false)
}

enum PasteAttempt {
    Verified,
    SentUnverified,
    NotSent,
}

fn paste_and_verify(pid: i32, text: &str, use_captured: bool) -> Result<PasteAttempt, String> {
    let text = std::ffi::CString::new(text)
        .map_err(|_| "Transcript contains an unsupported NUL character".to_string())?;
    match unsafe { pronto_insert_with_pasteboard(pid, text.as_ptr(), i32::from(use_captured)) } {
        1 => Ok(PasteAttempt::Verified),
        2 => Ok(PasteAttempt::SentUnverified),
        0 => Ok(PasteAttempt::NotSent),
        -2 => Err("macOS could not restore the previous clipboard contents".into()),
        _ => Err("macOS could not perform temporary pasteboard insertion".into()),
    }
}

fn set_selected_text(pid: i32, text: &str, use_captured: bool) -> Result<bool, String> {
    let text = std::ffi::CString::new(text)
        .map_err(|_| "Transcript contains an unsupported NUL character".to_string())?;
    match unsafe { pronto_insert_with_accessibility(pid, text.as_ptr(), i32::from(use_captured)) } {
        1 => Ok(true),
        2 => Ok(false),
        _ => Err("The text field did not accept Accessibility insertion".into()),
    }
}

pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    if pasteboard.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString }) {
        Ok(())
    } else {
        Err("macOS could not copy the transcript to the clipboard".into())
    }
}
