use std::os::windows::ffi::OsStrExt;
use windows::core::w;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_SET_VALUE, REG_SZ,
};

const BACKGROUND_ARG: &str = "--background";

pub fn is_background_launch() -> bool {
    std::env::args_os().any(|argument| argument == BACKGROUND_ARG)
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let mut key = HKEY::default();
    let open = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            None,
            KEY_SET_VALUE,
            &mut key,
        )
    };
    if open != ERROR_SUCCESS {
        return Err(format!(
            "Windows startup settings are unavailable ({})",
            open.0
        ));
    }
    let result = if enabled {
        let executable =
            std::env::current_exe().map_err(|error| format!("Could not locate Pronto: {error}"))?;
        let command = format!("\"{}\" {BACKGROUND_ARG}", executable.display());
        let wide: Vec<u16> = std::ffi::OsStr::new(&command)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let bytes =
            unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };
        unsafe { RegSetValueExW(key, w!("Pronto"), None, REG_SZ, Some(bytes)) }
    } else {
        unsafe { RegDeleteValueW(key, w!("Pronto")) }
    };
    // Remove an entry left by pre-rename builds so only Pronto can autostart.
    let _ = unsafe { RegDeleteValueW(key, w!("Vela")) };
    let _ = unsafe { RegCloseKey(key) };
    if result == ERROR_SUCCESS || (!enabled && result == ERROR_FILE_NOT_FOUND) {
        Ok(())
    } else {
        Err(format!(
            "Windows could not update the startup entry ({})",
            result.0
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn background_flag_is_stable() {
        assert_eq!(BACKGROUND_ARG, "--background");
    }
}
