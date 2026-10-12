use std::mem::size_of;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{mpsc, OnceLock};
use std::time::{Duration, Instant};
use windows::core::Interface;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    DeleteEnhMetaFile, DeleteMetaFile, DeleteObject, HENHMETAFILE, HGDIOBJ,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
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
use windows::Win32::System::Variant::VT_BOOL;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomation2, IUIAutomationElement, IUIAutomationValuePattern,
    UIA_ComboBoxControlTypeId, UIA_CustomControlTypeId, UIA_DocumentControlTypeId,
    UIA_EditControlTypeId, UIA_GroupControlTypeId, UIA_IsTextEditPatternAvailablePropertyId,
    UIA_PaneControlTypeId, UIA_ValuePatternId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, SetActiveWindow, SetFocus, INPUT, INPUT_0, INPUT_KEYBOARD,
    KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CallNextHookEx, DispatchMessageW, GetAncestor, GetClassNameW, GetCursorInfo,
    GetForegroundWindow, GetGUIThreadInfo, GetMessageW, GetWindowThreadProcessId, IsIconic,
    IsWindow, LoadCursorW, PostThreadMessageW, SendMessageTimeoutW, SetForegroundWindow,
    SetWindowsHookExW, ShowWindow, TranslateMessage, UnhookWindowsHookEx, WindowFromPoint,
    CURSORINFO, GA_ROOT, GUITHREADINFO, IDC_IBEAM, MSG, MSLLHOOKSTRUCT, SMTO_ABORTIFHUNG,
    SW_RESTORE, WH_MOUSE_LL, WM_APP, WM_LBUTTONDOWN, WM_PASTE,
};

const STOP_TARGET_HOOK: u32 = WM_APP + 0x565;
/// How long after a click the focus is given to settle in the clicked
/// control before Pronto checks whether it is a text box.
const CLICK_SETTLE: Duration = Duration::from_millis(120);
/// Upper bound on a UI Automation round trip, so a hung application can
/// never stall a paste for long.
const UIA_TIMEOUT_MS: u32 = 800;
const FOCUS_REPLY_TIMEOUT: Duration = Duration::from_millis(1500);

/// Root window of the most recent click in any external application, text
/// box or not. A fallback for when no text box was selected yet.
static LAST_CLICK_ROOT: AtomicIsize = AtomicIsize::new(0);
static FOCUS_WORKER: OnceLock<mpsc::Sender<FocusCommand>> = OnceLock::new();

/// Requests handled by the focus worker, which owns every UI Automation
/// object so they are only ever touched from one COM thread.
enum FocusCommand {
    Clicked {
        root: isize,
        ibeam: bool,
    },
    Resolve {
        fallback: isize,
        reply: mpsc::Sender<(isize, isize)>,
    },
    Refocus {
        root: isize,
        reply: mpsc::Sender<bool>,
    },
}

/// The text box the user selected most recently, in any application and at
/// any time (before or during a dictation).
struct Textbox {
    root: isize,
    control: isize,
    element: Option<IUIAutomationElement>,
}

pub struct InsertionTargetTracker {
    hook_thread: Option<u32>,
}

impl InsertionTargetTracker {
    pub fn new() -> Self {
        start_focus_worker();
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

    /// Windows resolves the target at paste time (the last selected text
    /// box wins, whenever it was selected), so a dictation needs no
    /// per-session state. Kept for parity with the macOS tracker.
    pub fn begin(&self, _initial_target: isize) {}

    pub fn finish(&self, fallback: isize) -> isize {
        fallback
    }

    pub fn cancel(&self) {}
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
        if !root.0.is_null() && is_external_application_window(root) {
            LAST_CLICK_ROOT.store(root.0 as isize, Ordering::Release);
            // Whether the click selected a text box is decided by the
            // worker once focus has moved: at click-down it often has not
            // reached the clicked control yet. The cursor shape is sampled
            // now, as a fallback for controls UI Automation cannot read.
            if let Some(worker) = FOCUS_WORKER.get() {
                let _ = worker.send(FocusCommand::Clicked {
                    root: root.0 as isize,
                    ibeam: cursor_is_ibeam(),
                });
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn cursor_is_ibeam() -> bool {
    static IBEAM: OnceLock<isize> = OnceLock::new();
    let ibeam = *IBEAM.get_or_init(|| {
        unsafe { LoadCursorW(None, IDC_IBEAM) }
            .map(|cursor| cursor.0 as isize)
            .unwrap_or(0)
    });
    let mut info = CURSORINFO {
        cbSize: size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };
    ibeam != 0 && unsafe { GetCursorInfo(&mut info) }.is_ok() && info.hCursor.0 as isize == ibeam
}

fn is_external_application_window(window: HWND) -> bool {
    let mut process_id = 0;
    unsafe { GetWindowThreadProcessId(window, Some(&mut process_id)) };
    if process_id == 0 || process_id == unsafe { GetCurrentProcessId() } {
        return false;
    }
    let mut class_name = [0u16; 64];
    let length = unsafe { GetClassNameW(window, &mut class_name) } as usize;
    let class_name = String::from_utf16_lossy(&class_name[..length]);
    !matches!(class_name.as_str(), "Shell_TrayWnd" | "Progman" | "WorkerW")
}

fn start_focus_worker() {
    if FOCUS_WORKER.get().is_some() {
        return;
    }
    let (commands, receiver) = mpsc::channel();
    let started = std::thread::Builder::new()
        .name("pronto-focus-target".into())
        .spawn(move || FocusWorker::new().run(receiver));
    if started.is_ok() {
        let _ = FOCUS_WORKER.set(commands);
    }
}

fn ask_focus_worker<T>(command: impl FnOnce(mpsc::Sender<T>) -> FocusCommand) -> Option<T> {
    let worker = FOCUS_WORKER.get()?;
    let (reply, response) = mpsc::channel();
    worker.send(command(reply)).ok()?;
    response.recv_timeout(FOCUS_REPLY_TIMEOUT).ok()
}

struct FocusWorker {
    automation: Option<IUIAutomation>,
    textbox: Option<Textbox>,
}

impl FocusWorker {
    fn new() -> Self {
        // UI Automation elements are free-threaded in the MTA, but they are
        // only ever used from this thread anyway.
        let automation = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()
        };
        if let Some(automation) = &automation {
            if let Ok(automation) = automation.cast::<IUIAutomation2>() {
                unsafe {
                    let _ = automation.SetConnectionTimeout(UIA_TIMEOUT_MS);
                    let _ = automation.SetTransactionTimeout(UIA_TIMEOUT_MS);
                }
            }
        }
        Self {
            automation,
            textbox: None,
        }
    }

    fn run(mut self, commands: mpsc::Receiver<FocusCommand>) {
        let mut pending: Option<(isize, bool, Instant)> = None;
        loop {
            let command = match pending {
                Some((_, _, due)) => {
                    match commands.recv_timeout(due.saturating_duration_since(Instant::now())) {
                        Ok(command) => Some(command),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                None => match commands.recv() {
                    Ok(command) => Some(command),
                    Err(_) => break,
                },
            };
            match command {
                Some(FocusCommand::Clicked { root, ibeam }) => {
                    pending = Some((root, ibeam, Instant::now() + CLICK_SETTLE));
                }
                Some(FocusCommand::Resolve { fallback, reply }) => {
                    // A click just before the paste still counts: let its
                    // focus settle and classify it first.
                    if let Some((root, ibeam, due)) = pending.take() {
                        std::thread::sleep(due.saturating_duration_since(Instant::now()));
                        self.classify_click(root, ibeam);
                    }
                    let _ = reply.send(self.resolve(fallback));
                }
                Some(FocusCommand::Refocus { root, reply }) => {
                    let _ = reply.send(self.refocus(root));
                }
                None => {
                    if let Some((root, ibeam, _)) = pending.take() {
                        self.classify_click(root, ibeam);
                    }
                }
            }
        }
    }

    /// Remembers the clicked control when the click selected a text box.
    /// Clicks on anything else (buttons, page backgrounds, title bars) keep
    /// the previously selected text box as the target.
    fn classify_click(&mut self, root: isize, ibeam: bool) {
        if !window_alive(root) {
            return;
        }
        let info = gui_thread_info(root);
        // The focused child window is unknown when focus lives in another
        // process (UWP apps) or never reached the clicked window; UI
        // Automation and the cursor shape still decide in that case.
        let focus = Some(info.hwndFocus).filter(|focus| {
            !focus.0.is_null() && unsafe { GetAncestor(*focus, GA_ROOT) }.0 as isize == root
        });
        let element = self.focused_element_in(root);
        let editable = focus.is_some_and(is_text_control_class)
            || (focus.is_some() && !info.hwndCaret.0.is_null())
            || match &element {
                Some(element) => uia_is_editable(element).unwrap_or(ibeam),
                None => ibeam,
            };
        if editable {
            self.textbox = Some(Textbox {
                root,
                control: focus.map(|focus| focus.0 as isize).unwrap_or(0),
                element,
            });
        }
    }

    fn resolve(&mut self, fallback: isize) -> (isize, isize) {
        if let Some(textbox) = &self.textbox {
            if window_alive(textbox.root) {
                return (textbox.root, textbox.control);
            }
            self.textbox = None;
        }
        for root in [fallback, LAST_CLICK_ROOT.load(Ordering::Acquire)] {
            if root != 0
                && window_alive(root)
                && is_external_application_window(HWND(root as *mut _))
            {
                return (root, focused_control(root).0 as isize);
            }
        }
        (0, 0)
    }

    /// Puts the keyboard focus back into the remembered text box of the
    /// (already activated) `root`, unless the user is already in a text box
    /// there. Returns true when the focus was moved.
    fn refocus(&mut self, root: isize) -> bool {
        let Some(textbox) = self.textbox.as_ref().filter(|textbox| textbox.root == root) else {
            return false;
        };
        if let (Some(automation), Some(element)) = (&self.automation, &textbox.element) {
            if let Ok(focused) = unsafe { automation.GetFocusedElement() } {
                if element_process(&focused) == window_process(root) {
                    let same = unsafe { automation.CompareElements(&focused, element) }
                        .map(|same| same.as_bool())
                        .unwrap_or(false);
                    if same || uia_is_editable(&focused).unwrap_or(false) {
                        return false;
                    }
                }
            }
            if unsafe { element.SetFocus() }.is_ok() {
                return true;
            }
        }
        focus_native_control(root, textbox.control)
    }

    fn focused_element_in(&self, root: isize) -> Option<IUIAutomationElement> {
        let element = unsafe { self.automation.as_ref()?.GetFocusedElement() }.ok()?;
        (element_process(&element) == window_process(root)).then_some(element)
    }
}

fn uia_is_editable(element: &IUIAutomationElement) -> Option<bool> {
    let control_type = unsafe { element.CurrentControlType() }.ok()?;
    let read_only = unsafe {
        element
            .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            .and_then(|pattern| pattern.CurrentIsReadOnly())
    }
    .ok()
    .map(|read_only| read_only.as_bool());
    let text_edit =
        unsafe { element.GetCurrentPropertyValue(UIA_IsTextEditPatternAvailablePropertyId) }
            .ok()
            .map(|value| unsafe {
                value.Anonymous.Anonymous.vt == VT_BOOL
                    && value.Anonymous.Anonymous.Anonymous.boolVal.as_bool()
            })
            .unwrap_or(false);
    Some(if control_type == UIA_EditControlTypeId {
        read_only != Some(true)
    } else if [
        UIA_DocumentControlTypeId,
        UIA_ComboBoxControlTypeId,
        UIA_GroupControlTypeId,
        UIA_CustomControlTypeId,
        UIA_PaneControlTypeId,
    ]
    .contains(&control_type)
    {
        // Browsers expose whole pages as read-only documents; only
        // editable regions (contenteditable, rich editors) qualify.
        read_only == Some(false) || text_edit
    } else {
        false
    })
}

fn element_process(element: &IUIAutomationElement) -> u32 {
    unsafe { element.CurrentProcessId() }
        .map(|process| process as u32)
        .unwrap_or(0)
}

fn window_process(root: isize) -> u32 {
    let mut process_id = 0;
    unsafe { GetWindowThreadProcessId(HWND(root as *mut _), Some(&mut process_id)) };
    process_id
}

pub fn foreground_window() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}

/// Window dictation falls back to when no text box was selected yet: the
/// foreground window when it is another application, otherwise the window
/// of the last external click. Pronto's own windows (the listening pill,
/// voice search, Settings) are never a paste target.
pub fn preferred_target() -> isize {
    let foreground = unsafe { GetForegroundWindow() };
    if !foreground.0.is_null() && is_external_application_window(foreground) {
        foreground.0 as isize
    } else {
        LAST_CLICK_ROOT.load(Ordering::Acquire)
    }
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

/// Dictation pastes into the text box the user selected most recently (see
/// `copy_and_paste_focus`); `target` is only the fallback.
pub fn insert_dictation_text(target: isize, text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    if copy_and_paste_focus(target, text)? {
        Ok(())
    } else {
        Err("Transcript copied, but Windows blocked automatic pasting".into())
    }
}

/// Copies a transcript and, when Windows permits foreground activation, pastes
/// it into the remembered target. `false` means the clipboard fallback worked.
pub fn copy_and_paste(target: isize, text: &str) -> Result<bool, String> {
    paste_into(target, focused_control(target), false, text)
}

/// Pastes into the text box the user selected most recently, whenever that
/// was: before the dictation, or while it was recording. A text box is
/// selected by clicking into it; clicks elsewhere keep the previous one.
/// Its window is brought back to the foreground and, when the focus has
/// left the text box meanwhile, the text box is focused again. Without any
/// selected text box, `fallback_root` (then the last clicked window) is used.
pub fn copy_and_paste_focus(fallback_root: isize, text: &str) -> Result<bool, String> {
    let (root, control) = resolve_focus_target(fallback_root);
    paste_into(root, control, true, text)
}

fn paste_into(root: isize, control: HWND, refocus: bool, text: &str) -> Result<bool, String> {
    let mut snapshot = ClipboardSnapshot::capture();
    copy_to_clipboard(text)?;
    let transcript_sequence = unsafe { GetClipboardSequenceNumber() };
    let pasted = paste_current_clipboard(root, control, refocus);

    // Give the receiving application time to consume Ctrl+V before restoring
    // the clipboard. Never overwrite clipboard content copied by the user (or
    // another application) while the paste was in flight.
    if pasted {
        std::thread::sleep(Duration::from_millis(180));
        snapshot.restore_if_unchanged(transcript_sequence);
    }
    Ok(pasted)
}

/// The last selected text box when its window is alive, otherwise the
/// fallback window, otherwise no target. The focused control inside a
/// fallback window is resolved now (it is only stable after the click was
/// processed).
fn resolve_focus_target(fallback_root: isize) -> (isize, HWND) {
    if let Some((root, control)) = ask_focus_worker(|reply| FocusCommand::Resolve {
        fallback: fallback_root,
        reply,
    }) {
        return (root, HWND(control as *mut _));
    }
    for root in [LAST_CLICK_ROOT.load(Ordering::Acquire), fallback_root] {
        if root != 0 && window_alive(root) {
            return (root, focused_control(root));
        }
    }
    (0, HWND::default())
}

fn window_alive(root: isize) -> bool {
    unsafe { IsWindow(Some(HWND(root as *mut _))).as_bool() }
}

fn gui_thread_info(root: isize) -> GUITHREADINFO {
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    let thread = unsafe { GetWindowThreadProcessId(HWND(root as *mut _), None) };
    if thread == 0 || unsafe { GetGUIThreadInfo(thread, &mut info) }.is_err() {
        return GUITHREADINFO::default();
    }
    info
}

fn focused_control(root: isize) -> HWND {
    gui_thread_info(root).hwndFocus
}

fn is_text_control_class(control: HWND) -> bool {
    if control.0.is_null() {
        return false;
    }
    let mut class_name = [0u16; 64];
    let length = unsafe { GetClassNameW(control, &mut class_name) } as usize;
    is_text_class_name(&String::from_utf16_lossy(&class_name[..length]))
}

fn is_text_class_name(class_name: &str) -> bool {
    let class_name = class_name.to_ascii_lowercase();
    class_name == "edit" || class_name.starts_with("richedit") || class_name == "scintilla"
}

/// Native-control fallback for `FocusWorker::refocus`: focuses the
/// remembered child window unless a text control already has focus.
fn focus_native_control(root: isize, control: isize) -> bool {
    let control = HWND(control as *mut _);
    if control.0.is_null()
        || !unsafe { IsWindow(Some(control)) }.as_bool()
        || unsafe { GetAncestor(control, GA_ROOT) }.0 as isize != root
    {
        return false;
    }
    let focused = focused_control(root);
    if focused == control || is_text_control_class(focused) {
        return false;
    }
    let current_thread = unsafe { GetCurrentThreadId() };
    let target_thread = unsafe { GetWindowThreadProcessId(control, None) };
    if target_thread == 0
        || target_thread == current_thread
        || !unsafe { AttachThreadInput(current_thread, target_thread, true) }.as_bool()
    {
        return false;
    }
    let focused_now = unsafe { SetFocus(Some(control)) }.is_ok();
    let _ = unsafe { AttachThreadInput(current_thread, target_thread, false) };
    focused_now
}

fn paste_current_clipboard(root: isize, control: HWND, refocus: bool) -> bool {
    if root == 0 {
        return false;
    }
    let target = HWND(root as *mut _);
    if !activate_target(target) {
        return paste_to_control(control);
    }
    // Highlight the selected text box again when the focus moved off it
    // (another control in its window was clicked, or the window was left).
    if refocus && ask_focus_worker(|reply| FocusCommand::Refocus { root, reply }) == Some(true) {
        std::thread::sleep(Duration::from_millis(40));
    }

    // A hold shortcut becomes inactive as soon as its first key is released,
    // while Ctrl/Alt/Win may still physically be down. Sending Ctrl+V during
    // that small window creates a different Windows chord and explains the
    // intermittent no-paste behavior. Wait for the user's modifiers to clear.
    if !wait_for_modifiers_released() {
        return false;
    }
    std::thread::sleep(Duration::from_millis(18));
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
    if !is_text_control_class(control) {
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
    if unsafe { IsIconic(target) }.as_bool() {
        let _ = unsafe { ShowWindow(target, SW_RESTORE) };
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
        GetWindowTextW, GetWindowThreadProcessId, PeekMessageW, SetWindowTextW, ShowWindow,
        TranslateMessage, MSG, PM_REMOVE, SW_SHOW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    #[test]
    fn text_control_classes_are_recognised() {
        assert!(is_text_class_name("Edit"));
        assert!(is_text_class_name("RichEdit20W"));
        assert!(is_text_class_name("RichEditD2DPT"));
        assert!(is_text_class_name("Scintilla"));
        assert!(!is_text_class_name("Button"));
        assert!(!is_text_class_name("Chrome_RenderWidgetHostHWND"));
    }

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

    /// Runs `body` with a native edit control in the foreground, as a dictation
    /// target, and closes it afterwards.
    fn with_foreground_edit<R>(body: impl FnOnce(HWND) -> R) -> R {
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
                std::thread::sleep(Duration::from_millis(1));
            }
            unsafe {
                let _ = DestroyWindow(window);
            }
        });
        let target = window_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("GUI thread should publish its edit control");
        std::thread::sleep(Duration::from_millis(100));
        let result = body(HWND(target as *mut _));
        stop.store(true, Ordering::Release);
        gui.join().expect("GUI thread should close cleanly");
        result
    }

    fn window_text(window: HWND) -> String {
        let length = unsafe { GetWindowTextLengthW(window) };
        let mut text = vec![0u16; length as usize + 1];
        let copied = unsafe { GetWindowTextW(window, &mut text) } as usize;
        String::from_utf16_lossy(&text[..copied])
    }

    /// End-to-end verification against a native editable Windows control. Kept
    /// ignored for ordinary CI because it requires an interactive desktop.
    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn inserts_unicode_into_foreground_edit_control() {
        let original_clipboard = "Clipboard before Pronto";
        copy_to_clipboard(original_clipboard).expect("test clipboard should be seeded");
        let expected = "Pronto ✓ Parakeet";
        let inserted = with_foreground_edit(|window| {
            insert_text(window.0 as isize, expected).expect("SendInput should accept every event");
            std::thread::sleep(Duration::from_millis(250));
            window_text(window)
        });
        assert_eq!(inserted, expected);
        assert_eq!(clipboard_text(), original_clipboard);
    }

    /// Insertion latency: `insert_text` start to the text appearing in the
    /// target, and to `insert_text` returning (after the clipboard-restore wait).
    /// Prints JSON lines; PRONTO_BENCH_REPEATS sets the count (default 10).
    #[test]
    #[ignore = "benchmark: requires an interactive Windows desktop"]
    fn benchmark_insertion_latency() {
        let repeats: usize = std::env::var("PRONTO_BENCH_REPEATS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(10);
        let text = "The quick brown fox jumps over the lazy dog, twice. ".repeat(4);
        let text = text.trim_end();
        copy_to_clipboard("Clipboard before Pronto").expect("test clipboard should be seeded");
        with_foreground_edit(|window| {
            for repeat in 0..repeats {
                unsafe {
                    let _ = SetWindowTextW(window, w!(""));
                }
                let target = window.0 as isize;
                let started = std::time::Instant::now();
                let watcher = std::thread::spawn(move || {
                    let window = HWND(target as *mut _);
                    let deadline = started + Duration::from_secs(2);
                    while std::time::Instant::now() < deadline {
                        if unsafe { GetWindowTextLengthW(window) } > 0 {
                            return Some(started.elapsed().as_secs_f64() * 1000.0);
                        }
                        std::thread::yield_now();
                    }
                    None
                });
                insert_text(target, text).expect("SendInput should accept every event");
                let returned_ms = started.elapsed().as_secs_f64() * 1000.0;
                let visible_ms = watcher.join().unwrap();
                assert_eq!(window_text(window), text);
                println!(
                    "{{\"repeat\": {repeat}, \"visible_ms\": {visible_ms:?}, \"returned_ms\": {returned_ms:.2}}}"
                );
                std::thread::sleep(Duration::from_millis(300));
            }
        });
    }
}
