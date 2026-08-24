use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE,
};

pub struct SingleInstance(HANDLE);

impl SingleInstance {
    /// Returns `None` after handing activation to the already-running Pronto.
    pub fn acquire() -> Result<Option<Self>, String> {
        let handle =
            unsafe { CreateMutexW(None, false, w!("Local\\Pronto.Dictation.SingleInstance")) }
                .map_err(|error| format!("Could not create the Pronto instance guard: {error}"))?;
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            let _ = unsafe { CloseHandle(handle) };
            if let Ok(window) = unsafe { FindWindowW(None, w!("Pronto")) } {
                unsafe {
                    let _ = ShowWindow(window, SW_RESTORE);
                    let _ = SetForegroundWindow(window);
                }
            }
            return Ok(None);
        }
        Ok(Some(Self(handle)))
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}
