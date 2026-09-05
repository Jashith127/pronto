use std::mem::size_of;
use std::sync::{mpsc, Mutex, OnceLock};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    DeleteEnhMetaFile, DeleteMetaFile, DeleteObject, HENHMETAFILE, HGDIOBJ,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardSequenceNumber, OpenClipboard, SetClipboardData, METAFILEPICT,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalUnlock, GLOBAL_ALLOC_FLAGS, GMEM_MOVEABLE,
};
use windows::Win32::System::Ole::{OleDuplicateData, CLIPBOARD_FORMAT};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, SetActiveWindow, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CallNextHookEx, DispatchMessageW, GetAncestor, GetClassNameW,
    GetForegroundWindow, GetGUIThreadInfo, GetMessageW, GetWindowThreadProcessId, IsWindow,
    PostThreadMessageW, SendMessageTimeoutW, SetForegroundWindow, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, WindowFromPoint, GA_ROOT, GUITHREADINFO, MSG,
    MSLLHOOKSTRUCT, SMTO_ABORTIFHUNG, WH_MOUSE_LL, WM_APP, WM_LBUTTONDOWN, WM_PASTE,
};

const STOP_TARGET_HOOK: u32 = WM_APP + 0x565;

struct TargetState {
    active: bool,
    target: isize,
    own_process: u32,
    /// Root window of the most recent click in any external application,
    /// tracked at all times (not only during dictation) so pasting can
    /// follow the textbox the user selected most recently.
    last_focus_root: isize,
    last_focus_at_ms: u64,
}

static TARGET_STATE: OnceLock<Mutex<TargetState>> = OnceLock::new();

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

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
            last_focus_root: 0,
            last_focus_at_ms: 0,
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
                    if is_external_application_window(root, state.own_process) {
                        // The focused control is resolved lazily at paste
                        // time: at click-down the focus often has not moved
                        // to the clicked control yet.
                        state.last_focus_root = root.0 as isize;
                        state.last_focus_at_ms = now_ms();
                        if state.active {
                            state.target = root.0 as isize;
                        }
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
    paste_into(target, focused_control(target), text)
}

/// Pastes into the textbox the user selected most recently: the window of
/// the latest external click wins when it is still alive, otherwise the
/// remembered dictation target is used. Focusing a textbox in a new app
/// after switching to it therefore redirects the paste there, while
/// switching without clicking keeps pasting into the original textbox
/// (which is brought back to the foreground).
pub fn copy_and_paste_focus(fallback_root: isize, text: &str) -> Result<bool, String> {
    let (root, control) = resolve_focus_target(fallback_root);
    paste_into(root, control, text)
}

fn paste_into(root: isize, control: HWND, text: &str) -> Result<bool, String> {
    let mut snapshot = ClipboardSnapshot::capture();
    copy_to_clipboard(text)?;
    let transcript_sequence = unsafe { GetClipboardSequenceNumber() };
    let pasted = paste_current_clipboard(root, control);

    // Give the receiving application time to consume Ctrl+V before restoring
    // the clipboard. Never overwrite clipboard content copied by the user (or
    // another application) while the paste was in flight.
    if pasted {
        std::thread::sleep(std::time::Duration::from_millis(180));
        snapshot.restore_if_unchanged(transcript_sequence);
    }
    Ok(pasted)
}

/// Most recent click target wins when alive; the focused control inside it
/// is resolved now (it is only stable after the click was processed).
/// Falls back to the remembered dictation target, then to no target.
fn resolve_focus_target(fallback_root: isize) -> (isize, HWND) {
    let last_root = TARGET_STATE
        .get()
        .and_then(|state| state.lock().ok())
        .map(|state| state.last_focus_root)
        .unwrap_or(0);
    for root in [last_root, fallback_root] {
        if root != 0 && window_alive(root) {
            return (root, focused_control(root));
        }
    }
    (0, HWND::default())
}

fn window_alive(root: isize) -> bool {
    unsafe { IsWindow(Some(HWND(root as *mut _))).as_bool() }
}

fn focused_control(root: isize) -> HWND {
    let window = HWND(root as *mut _);
    let thread = unsafe { GetWindowThreadProcessId(window, None) };
    if thread == 0 {
        return HWND::default();
    }
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetGUIThreadInfo(thread, &mut info) }.is_err() {
        return HWND::default();
    }
    info.hwndFocus
}

fn paste_current_clipboard(root: isize, control: HWND) -> bool {
    if root == 0 {
        return false;
    }
    let target = HWND(root as *mut _);
    if !activate_target(target) {
        return paste_to_control(control);
    }

    // A hold shortcut becomes inactive as soon as its first key is released,
    // while Ctrl/Alt/Win may still physically be down. Sending Ctrl+V during
    // that small window creates a different Windows chord and explains the
    // intermittent no-paste behavior. Wait for the user's modifiers to clear.
    if !wait_for_modifiers_released() {
        return false;
    }
    std::thread::sleep(std::time::Duration::from_millis(18));
    let v = VIRTUAL_KEY(b'V' as u16);
    let inputs = [
        key_input(VK_CONTROL, 0, Default::default()),
        key_input(v, 0, Default::default()),
        key_input(v, 0, KEYEVENTF_KEYUP),
        key_input(VK_CONTROL, 0, KEYEVENTF_KEYUP),
    ];
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) } as usize;
    if sent == inputs.len() {
        true
    } else {
        paste_to_control(control)
    }
}

struct ClipboardEntry {
    format: u32,
    handle: HANDLE,
}

/// Materializes independent copies of every clipboard format before Pronto
/// replaces them. Holding the original IDataObject is insufficient because a
/// Win32 clipboard owner may invalidate it as soon as EmptyClipboard is called.
struct ClipboardSnapshot {
    entries: Vec<ClipboardEntry>,
    captured: bool,
}

impl ClipboardSnapshot {
    fn capture() -> Self {
        if !open_clipboard_with_retry() {
            return Self {
                entries: Vec::new(),
                captured: false,
            };
        }

        let mut entries = Vec::new();
        let mut format = 0;
        loop {
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                break;
            }
            let Ok(source) = (unsafe { GetClipboardData(format) }) else {
                continue;
            };
            let duplicate = unsafe {
                OleDuplicateData(
                    source,
                    CLIPBOARD_FORMAT(format as u16),
                    GLOBAL_ALLOC_FLAGS(0),
                )
            };
            if !duplicate.0.is_null() {
                entries.push(ClipboardEntry {
                    format,
                    handle: duplicate,
                });
            }
        }
        let _ = unsafe { CloseClipboard() };
        Self {
            entries,
            captured: true,
        }
    }

    fn restore_if_unchanged(&mut self, transcript_sequence: u32) {
        if !clipboard_sequence_is_unchanged(transcript_sequence, unsafe {
            GetClipboardSequenceNumber()
        }) || !self.captured
            || !open_clipboard_with_retry()
        {
            return;
        }

        if unsafe { EmptyClipboard() }.is_ok() {
            for entry in &mut self.entries {
                if unsafe { SetClipboardData(entry.format, Some(entry.handle)) }.is_ok() {
                    // SetClipboardData transfers ownership to Windows.
                    entry.handle = HANDLE::default();
                }
            }
        }
        let _ = unsafe { CloseClipboard() };
    }
}

fn clipboard_sequence_is_unchanged(transcript_sequence: u32, current_sequence: u32) -> bool {
    transcript_sequence != 0 && transcript_sequence == current_sequence
}

impl Drop for ClipboardSnapshot {
    fn drop(&mut self) {
        for entry in &mut self.entries {
            if !entry.handle.0.is_null() {
                unsafe { release_clipboard_handle(entry.format, entry.handle) };
                entry.handle = HANDLE::default();
            }
        }
    }
}

fn open_clipboard_with_retry() -> bool {
    for _ in 0..8 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    false
}

unsafe fn release_clipboard_handle(format: u32, handle: HANDLE) {
    match format {
        2 | 9 => {
            let _ = unsafe { DeleteObject(HGDIOBJ(handle.0)) };
        }
        3 => {
            let global = HGLOBAL(handle.0);
            let pointer = unsafe { GlobalLock(global) } as *const METAFILEPICT;
            if !pointer.is_null() {
                let _ = unsafe { DeleteMetaFile((*pointer).hMF) };
                let _ = unsafe { GlobalUnlock(global) };
            }
            let _ = unsafe { GlobalFree(Some(global)) };
        }
        14 => {
            let _ = unsafe { DeleteEnhMetaFile(Some(HENHMETAFILE(handle.0))) };
        }
        _ => {
            let _ = unsafe { GlobalFree(Some(HGLOBAL(handle.0))) };
        }
    }
}

fn paste_to_control(control: HWND) -> bool {
    if control.0.is_null() {
        return false;
    }
    let mut class_name = [0u16; 64];
    let length = unsafe { GetClassNameW(control, &mut class_name) } as usize;
    let class_name = String::from_utf16_lossy(&class_name[..length]).to_ascii_lowercase();
    if !(class_name == "edit" || class_name.starts_with("richedit") || class_name == "scintilla") {
        return false;
    }
    unsafe {
        let _ = SendMessageTimeoutW(
            control,
            WM_PASTE,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            250,
            None,
        );
    }
    true
}

fn activate_target(target: HWND) -> bool {
    if !unsafe { IsWindow(Some(target)) }.as_bool() {
        return false;
    }
    if unsafe { GetForegroundWindow() } == target {
        return true;
    }

    // SetForegroundWindow is intentionally restricted by Windows. Temporarily
    // joining the relevant input queues lets us restore the application the
    // user selected without clicking or typing into the wrong foreground app.
    let current_thread = unsafe { GetCurrentThreadId() };
    let foreground = unsafe { GetForegroundWindow() };
    let foreground_thread = unsafe { GetWindowThreadProcessId(foreground, None) };
    let target_thread = unsafe { GetWindowThreadProcessId(target, None) };
    let attached_foreground = foreground_thread != 0
        && foreground_thread != current_thread
        && unsafe { AttachThreadInput(current_thread, foreground_thread, true) }.as_bool();
    let attached_target = target_thread != 0
        && target_thread != current_thread
        && target_thread != foreground_thread
        && unsafe { AttachThreadInput(current_thread, target_thread, true) }.as_bool();

    unsafe {
        let _ = BringWindowToTop(target);
        let _ = SetActiveWindow(target);
        let _ = SetForegroundWindow(target);
    }

    if attached_target {
        let _ = unsafe { AttachThreadInput(current_thread, target_thread, false) };
    }
    if attached_foreground {
        let _ = unsafe { AttachThreadInput(current_thread, foreground_thread, false) };
    }

    for _ in 0..20 {
        if unsafe { GetForegroundWindow() } == target {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

fn wait_for_modifiers_released() -> bool {
    for _ in 0..100 {
        let down = [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN]
            .into_iter()
            .any(|key| unsafe { GetAsyncKeyState(key.0 as i32) } as u16 & 0x8000 != 0);
        if !down {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
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
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::GetClipboardData;
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::Input::KeyboardAndMouse::{SetActiveWindow, SetFocus};
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, CreateWindowExW, DestroyWindow, DispatchMessageW, GetWindowTextLengthW,
        GetWindowTextW, GetWindowThreadProcessId, PeekMessageW, ShowWindow, TranslateMessage, MSG,
        PM_REMOVE, SW_SHOW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    #[test]
    fn clipboard_sequence_guard_only_accepts_pronto_sequence() {
        assert!(clipboard_sequence_is_unchanged(42, 42));
        assert!(!clipboard_sequence_is_unchanged(42, 43));
        assert!(!clipboard_sequence_is_unchanged(0, 0));
    }

    fn clipboard_text() -> String {
        unsafe {
            OpenClipboard(None).expect("clipboard should open");
            let handle = GetClipboardData(13).expect("Unicode clipboard data should exist");
            let pointer = GlobalLock(HGLOBAL(handle.0)) as *const u16;
            assert!(!pointer.is_null(), "clipboard data should lock");
            let mut length = 0;
            while *pointer.add(length) != 0 {
                length += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length));
            let _ = GlobalUnlock(HGLOBAL(handle.0));
            let _ = CloseClipboard();
            text
        }
    }

    /// End-to-end verification against a native editable Windows control. Kept
    /// ignored for ordinary CI because it requires an interactive desktop.
    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn inserts_unicode_into_foreground_edit_control() {
        let original_clipboard = "Clipboard before Pronto";
        copy_to_clipboard(original_clipboard).expect("test clipboard should be seeded");
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
        assert_eq!(clipboard_text(), original_clipboard);
    }
}
