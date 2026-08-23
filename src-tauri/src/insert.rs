use std::mem::size_of;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    VIRTUAL_KEY, VK_CONTROL, VK_RETURN,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};

pub fn foreground_window() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}

pub fn insert_text(target: isize, text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }

    if target != 0 {
        unsafe {
            let _ = SetForegroundWindow(HWND(target as *mut _));
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    let mut inputs = Vec::with_capacity(text.encode_utf16().count() * 2);
    for unit in text.encode_utf16() {
        if unit == b'\n' as u16 {
            inputs.push(key_input(VK_RETURN, 0, Default::default()));
            inputs.push(key_input(VK_RETURN, 0, KEYEVENTF_KEYUP));
        } else {
            inputs.push(key_input(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE));
            inputs.push(key_input(
                VIRTUAL_KEY(0),
                unit,
                KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
            ));
        }
    }

    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) } as usize;
    if sent != inputs.len() {
        let os_error = std::io::Error::last_os_error();
        return Err(format!(
            "Windows accepted {sent} of {} text input events ({os_error})",
            inputs.len(),
        ));
    }
    Ok(())
}

/// Copies a transcript and, when Windows permits foreground activation, pastes
/// it into the remembered target. `false` means the clipboard fallback worked.
pub fn copy_and_paste(target: isize, text: &str) -> Result<bool, String> {
    copy_to_clipboard(text)?;
    if target == 0 {
        return Ok(false);
    }
    let focused = unsafe { SetForegroundWindow(HWND(target as *mut _)).as_bool() };
    if !focused {
        return Ok(false);
    }
    std::thread::sleep(std::time::Duration::from_millis(35));
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
