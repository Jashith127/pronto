//! System power notifications. A message-only window receives
//! WM_POWERBROADCAST so Pronto can heal sleep-related failures (dead overlay
//! page, wedged dictation state) on resume instead of degrading silently.

use std::sync::OnceLock;
use tauri::AppHandle;
use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetMessageW, RegisterClassW, HWND_MESSAGE, MSG,
    PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, WINDOW_EX_STYLE, WINDOW_STYLE, WM_POWERBROADCAST,
    WNDCLASSW,
};

static RESUME_APP: OnceLock<AppHandle> = OnceLock::new();

unsafe extern "system" fn power_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_POWERBROADCAST {
        let event = wparam.0 as u32;
        if event == PBT_APMRESUMESUSPEND || event == PBT_APMRESUMEAUTOMATIC {
            if let Some(app) = RESUME_APP.get() {
                crate::handle_system_resume(app);
            }
        }
        // Grant every power transition (suspend queries expect TRUE).
        return LRESULT(1);
    }
    DefWindowProcW(hwnd, message, wparam, lparam)
}

/// Starts the power-notification thread. The message-only window is owned by
/// the thread and lives until process exit; no shutdown is needed.
pub fn start(app: AppHandle) {
    let _ = RESUME_APP.set(app);
    std::thread::Builder::new()
        .name("pronto-power".into())
        .spawn(|| unsafe {
            let instance = GetModuleHandleW(None)
                .map(|module| HINSTANCE(module.0))
                .unwrap_or_default();
            let class = WNDCLASSW {
                lpfnWndProc: Some(power_window_proc),
                hInstance: instance.into(),
                lpszClassName: w!("ProntoPowerNotifications"),
                ..Default::default()
            };
            // A second registration in the same process fails; the existing
            // class is still usable, so the result is intentionally ignored.
            let _ = RegisterClassW(&class);
            let window = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("ProntoPowerNotifications"),
                w!(""),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(instance),
                None,
            );
            if window.is_err() {
                return;
            }
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {}
        })
        .ok();
}
