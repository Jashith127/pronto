use std::mem::size_of;
use std::sync::{mpsc, Mutex, OnceLock};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetAncestor, GetClassNameW, GetForegroundWindow, GetMessageW,
    GetWindowThreadProcessId, PostThreadMessageW, SetForegroundWindow, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, WindowFromPoint, GA_ROOT, MSG, MSLLHOOKSTRUCT,
    WH_MOUSE_LL, WM_APP, WM_LBUTTONDOWN,
};

const STOP_TARGET_HOOK: u32 = WM_APP + 0x565;

struct TargetState {
    active: bool,
    target: isize,
    own_process: u32,
}

static TARGET_STATE: OnceLock<Mutex<TargetState>> = OnceLock::new();

pub struct InsertionTargetTracker {
    hook_thread: Option<u32>,
}

impl InsertionTargetTracker {
    pub fn new() -> Self {
        let own_process = unsafe { GetCurrentProcessId() };
        let _ = TARGET_STATE.set(Mutex::new(TargetState {
            active: false,
            target: 0,
            own_process,
        }));
        let (ready, response) = mpsc::channel();
        let started = std::thread::Builder::new()
            .name("pronto-insertion-target".into())
            .spawn(move || {
                let thread_id = unsafe { GetCurrentThreadId() };
                match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), None, 0) } {
                    Ok(hook) => {
                        let _ = ready.send(Some(thread_id));
                        let mut message = MSG::default();
                        while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
                            if message.message == STOP_TARGET_HOOK {
                                break;
                            }
                            unsafe {
                                let _ = TranslateMessage(&message);
                                DispatchMessageW(&message);
                            }
                        }
                        let _ = unsafe { UnhookWindowsHookEx(hook) };
                    }
                    Err(_) => {
                        let _ = ready.send(None);
                    }
                }
            });
        let hook_thread = if started.is_ok() {
            response.recv().ok().flatten()
        } else {
            None
        };
        Self { hook_thread }
    }

    pub fn begin(&self, initial_target: isize) {
        if let Some(state) = TARGET_STATE.get() {
            if let Ok(mut state) = state.lock() {
                state.target = initial_target;
                state.active = true;
            }
        }
    }

    pub fn finish(&self, fallback: isize) -> isize {
        let Some(state) = TARGET_STATE.get() else {
            return fallback;
        };
        state
            .lock()
            .map(|mut state| {
                state.active = false;
                if state.target == 0 {
                    fallback
                } else {
                    state.target
                }
            })
            .unwrap_or(fallback)
    }

    pub fn cancel(&self) {
        if let Some(state) = TARGET_STATE.get() {
            if let Ok(mut state) = state.lock() {
                state.active = false;
            }
        }
    }
}

impl Drop for InsertionTargetTracker {
    fn drop(&mut self) {
        if let Some(thread_id) = self.hook_thread {
            let _ =
                unsafe { PostThreadMessageW(thread_id, STOP_TARGET_HOOK, WPARAM(0), LPARAM(0)) };
        }
    }
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam.0 as u32 == WM_LBUTTONDOWN {
        let event = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let clicked = unsafe { WindowFromPoint(event.pt) };
        let root = unsafe { GetAncestor(clicked, GA_ROOT) };
        if !root.0.is_null() {
            if let Some(state) = TARGET_STATE.get() {
                if let Ok(mut state) = state.lock() {
                    if state.active && is_external_application_window(root, state.own_process) {
                        state.target = root.0 as isize;
                    }
                }
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn is_external_application_window(window: HWND, own_process: u32) -> bool {
    let mut process_id = 0;
    unsafe { GetWindowThreadProcessId(window, Some(&mut process_id)) };
    if process_id == 0 || process_id == own_process {
        return false;
    }
    let mut class_name = [0u16; 64];
    let length = unsafe { GetClassNameW(window, &mut class_name) } as usize;
    let class_name = String::from_utf16_lossy(&class_name[..length]);
    !matches!(class_name.as_str(), "Shell_TrayWnd" | "Progman" | "WorkerW")
}

pub fn foreground_window() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}

pub fn insert_text(target: isize, text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    if copy_and_paste(target, text)? {
        Ok(())
    } else {
        Err("Transcript copied, but Windows blocked automatic pasting".into())
    }
}

/// Copies a transcript and, when Windows permits foreground activation, pastes
/// it into the remembered target. `false` means the clipboard fallback worked.
pub fn copy_and_paste(target: isize, text: &str) -> Result<bool, String> {
    copy_to_clipboard(text)?;
    if target == 0 {
        return Ok(false);
    }
    let target = HWND(target as *mut _);
    if unsafe { GetForegroundWindow() } != target {
        let _ = unsafe { SetForegroundWindow(target) };
        for _ in 0..5 {
            if unsafe { GetForegroundWindow() } == target {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if unsafe { GetForegroundWindow() } != target {
            return Ok(false);
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(8));
    let v = VIRTUAL_KEY(b'V' as u16);
    let inputs = [
        key_input(VK_CONTROL, 0, Default::default()),
        key_input(v, 0, Default::default()),
        key_input(v, 0, KEYEVENTF_KEYUP),
        key_input(VK_CONTROL, 0, KEYEVENTF_KEYUP),
    ];
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) } as usize;
    Ok(sent == inputs.len())
}

pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    utf16.push(0);
    let mut opened = false;
    for _ in 0..8 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    if !opened {
        return Err("The clipboard is busy; try again".into());
    }

    let result = (|| -> Result<(), String> {
        unsafe {
            EmptyClipboard().map_err(|error| format!("Could not clear clipboard: {error}"))?;
            let memory = GlobalAlloc(GMEM_MOVEABLE, utf16.len() * size_of::<u16>())
                .map_err(|error| format!("Could not allocate clipboard memory: {error}"))?;
            let pointer = GlobalLock(memory) as *mut u16;
            if pointer.is_null() {
                let _ = GlobalFree(Some(memory));
                Err("Could not lock clipboard memory".to_string())
            } else {
                std::ptr::copy_nonoverlapping(utf16.as_ptr(), pointer, utf16.len());
                let _ = GlobalUnlock(memory);
                match SetClipboardData(13, Some(HANDLE(memory.0))) {
                    Ok(_) => Ok(()),
                    Err(error) => {
                        let _ = GlobalFree(Some(memory));
                        Err(format!("Could not update clipboard: {error}"))
                    }
                }
            }
        }
    })();
    let _ = unsafe { CloseClipboard() };
    result
}

fn key_input(
    virtual_key: VIRTUAL_KEY,
    scan_code: u16,
    flags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS,
) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: virtual_key,
                wScan: scan_code,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    };
    use std::time::Duration;
    use windows::core::w;
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::Input::KeyboardAndMouse::{SetActiveWindow, SetFocus};
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, CreateWindowExW, DestroyWindow, DispatchMessageW, GetWindowTextLengthW,
        GetWindowTextW, GetWindowThreadProcessId, PeekMessageW, ShowWindow, TranslateMessage, MSG,
        PM_REMOVE, SW_SHOW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    /// End-to-end verification against a native editable Windows control. Kept
    /// ignored for ordinary CI because it requires an interactive desktop.
    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn inserts_unicode_into_foreground_edit_control() {
        let (ready, window_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let gui_stop = Arc::clone(&stop);
        let gui = std::thread::spawn(move || {
            let window = unsafe {
                CreateWindowExW(
                    Default::default(),
                    w!("EDIT"),
                    w!(""),
                    WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                    20,
                    20,
                    420,
                    90,
                    None,
                    None,
                    None,
                    None,
                )
                .expect("edit control should be created")
            };
            let current_thread = unsafe { GetCurrentThreadId() };
            let foreground_thread =
                unsafe { GetWindowThreadProcessId(GetForegroundWindow(), None) };
            let attached = foreground_thread != 0 && foreground_thread != current_thread;
            unsafe {
                if attached {
                    let _ = AttachThreadInput(current_thread, foreground_thread, true);
                }
                let _ = ShowWindow(window, SW_SHOW);
                let _ = BringWindowToTop(window);
                let _ = SetActiveWindow(window);
                let _ = SetForegroundWindow(window);
                let _ = SetFocus(Some(window));
                if attached {
                    let _ = AttachThreadInput(current_thread, foreground_thread, false);
                }
            }
            ready
                .send(window.0 as isize)
                .expect("test receiver should exist");

            while !gui_stop.load(Ordering::Acquire) {
                let mut message = MSG::default();
                unsafe {
                    while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            unsafe {
                let _ = DestroyWindow(window);
            }
        });
        let target = window_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("GUI thread should publish its edit control");
        let window = HWND(target as *mut _);
        std::thread::sleep(Duration::from_millis(100));

        let expected = "Pronto ✓ Parakeet";
        insert_text(target, expected).expect("SendInput should accept every event");
        std::thread::sleep(Duration::from_millis(250));

        let length = unsafe { GetWindowTextLengthW(window) };
        let mut text = vec![0u16; length as usize + 1];
        let copied = unsafe { GetWindowTextW(window, &mut text) } as usize;
        stop.store(true, Ordering::Release);
        gui.join().expect("GUI thread should close cleanly");
        assert_eq!(String::from_utf16_lossy(&text[..copied]), expected);
    }
}
