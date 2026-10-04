//! Windows integration for Pronto Setup: processes, shortcuts, the
//! Add/Remove Programs entry, WebView2, and native message boxes.
//! Non-Windows builds get inert stand-ins so the crate can be checked anywhere.
use std::path::{Path, PathBuf};

pub const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Pronto";
const WEBVIEW2_CLIENT: &str =
    r"Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
const WEBVIEW2_BOOTSTRAPPER: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Per-user install location. It is also Pronto's data folder, which keeps
/// upgrades from the NSIS installer in place.
pub fn install_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Pronto")
}

pub struct UninstallEntry<'a> {
    pub version: &'a str,
    pub install_dir: &'a Path,
    pub estimated_kb: u32,
}

fn hidden(command: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Close Pronto and its speech runtimes so their files can be replaced.
/// Phonon's Python process lives in Pronto's kill-on-close job object, so it
/// exits with Pronto.
pub fn stop_pronto() {
    for image in ["pronto.exe", "nemo-speech.exe"] {
        let _ = hidden(std::process::Command::new("taskkill").args(["/F", "/T", "/IM", image]))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    std::thread::sleep(std::time::Duration::from_millis(800));
}

pub fn launch_detached(exe: &Path) -> Result<(), String> {
    std::process::Command::new(exe)
        .current_dir(exe.parent().unwrap_or(Path::new(".")))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not open Pronto: {e}"))
}

/// Delete `file` a moment after this process exits (used by the uninstaller's
/// temporary copy, which cannot delete itself while running).
pub fn delete_after_exit(file: &Path) {
    let command = format!(
        "ping -n 3 127.0.0.1 >NUL & del /F /Q \"{}\"",
        file.display()
    );
    let _ = hidden(std::process::Command::new("cmd").args(["/C", &command])).spawn();
}

fn shortcut_paths() -> Vec<PathBuf> {
    [known_folder::programs(), known_folder::desktop()]
        .into_iter()
        .flatten()
        .map(|folder| folder.join("Pronto.lnk"))
        .collect()
}

pub fn create_shortcuts(exe: &Path) -> Result<(), String> {
    for link in shortcut_paths() {
        imp::create_shortcut(&link, exe)?;
    }
    Ok(())
}

pub fn remove_shortcuts() {
    for link in shortcut_paths() {
        let _ = std::fs::remove_file(link);
    }
}

pub use imp::{
    message_box_yes_no, remove_uninstall_entry, show_error, webview2_installed,
    write_uninstall_entry,
};

/// Download and run Microsoft's WebView2 bootstrapper silently.
pub fn install_webview2() -> Result<(), String> {
    let target = std::env::temp_dir().join("MicrosoftEdgeWebview2Setup.exe");
    let bytes = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .and_then(|client| client.get(WEBVIEW2_BOOTSTRAPPER).send())
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.bytes())
        .map_err(|e| format!("Could not download WebView2: {e}"))?;
    std::fs::write(&target, &bytes).map_err(|e| e.to_string())?;
    let status = std::process::Command::new(&target)
        .args(["/silent", "/install"])
        .status()
        .map_err(|e| format!("Could not run the WebView2 installer: {e}"))?;
    let _ = std::fs::remove_file(&target);
    if status.success() {
        Ok(())
    } else {
        Err(format!("The WebView2 installer failed ({status})."))
    }
}

#[cfg(windows)]
mod known_folder {
    use std::path::PathBuf;
    use windows::core::GUID;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Programs, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
    };

    fn get(id: &GUID) -> Option<PathBuf> {
        unsafe {
            let path = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
            let value = path.to_string().ok();
            CoTaskMemFree(Some(path.0 as _));
            value.map(PathBuf::from)
        }
    }

    pub fn programs() -> Option<PathBuf> {
        get(&FOLDERID_Programs)
    }

    pub fn desktop() -> Option<PathBuf> {
        get(&FOLDERID_Desktop)
    }
}

#[cfg(not(windows))]
mod known_folder {
    use std::path::PathBuf;
    pub fn programs() -> Option<PathBuf> {
        None
    }
    pub fn desktop() -> Option<PathBuf> {
        None
    }
}

#[cfg(windows)]
mod imp {
    use super::{UninstallEntry, UNINSTALL_KEY, WEBVIEW2_CLIENT};
    use std::path::Path;
    use windows::core::{Interface, HSTRING, PCWSTR};
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE,
        REG_SZ, RRF_RT_REG_SZ,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, MB_ICONERROR, MB_ICONQUESTION, MB_OK, MB_YESNO,
    };

    pub fn create_shortcut(link: &Path, exe: &Path) -> Result<(), String> {
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let shell: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| format!("Could not create a shortcut: {e}"))?;
            let exe_w = HSTRING::from(exe.as_os_str());
            let dir_w = HSTRING::from(exe.parent().unwrap_or(exe).as_os_str());
            shell.SetPath(&exe_w).map_err(|e| e.to_string())?;
            shell
                .SetWorkingDirectory(&dir_w)
                .map_err(|e| e.to_string())?;
            shell
                .SetDescription(&HSTRING::from("Push-to-talk dictation"))
                .map_err(|e| e.to_string())?;
            shell
                .SetIconLocation(&exe_w, 0)
                .map_err(|e| e.to_string())?;
            let file: IPersistFile = shell.cast().map_err(|e| e.to_string())?;
            file.Save(&HSTRING::from(link.as_os_str()), true)
                .map_err(|e| format!("Could not save shortcut {}: {e}", link.display()))
        }
    }

    fn set_string(key: HKEY, name: &str, value: &str) -> Result<(), String> {
        let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes =
            unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };
        let status =
            unsafe { RegSetValueExW(key, &HSTRING::from(name), None, REG_SZ, Some(bytes)) };
        (status == ERROR_SUCCESS)
            .then_some(())
            .ok_or_else(|| format!("Could not write {name} to the registry"))
    }

    fn set_dword(key: HKEY, name: &str, value: u32) -> Result<(), String> {
        let bytes = value.to_le_bytes();
        let status =
            unsafe { RegSetValueExW(key, &HSTRING::from(name), None, REG_DWORD, Some(&bytes)) };
        (status == ERROR_SUCCESS)
            .then_some(())
            .ok_or_else(|| format!("Could not write {name} to the registry"))
    }

    pub fn write_uninstall_entry(entry: &UninstallEntry) -> Result<(), String> {
        let dir = entry.install_dir.display().to_string();
        let uninstaller = entry
            .install_dir
            .join("uninstall.exe")
            .display()
            .to_string();
        let mut key = HKEY::default();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                &HSTRING::from(UNINSTALL_KEY),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut key,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            return Err("Could not register Pronto in Installed apps".into());
        }
        let result = (|| {
            set_string(key, "DisplayName", "Pronto")?;
            set_string(key, "DisplayVersion", entry.version)?;
            set_string(key, "Publisher", "Pronto")?;
            set_string(key, "DisplayIcon", &format!("{dir}\\pronto.exe"))?;
            set_string(key, "InstallLocation", &dir)?;
            set_string(
                key,
                "UninstallString",
                &format!("\"{uninstaller}\" --uninstall"),
            )?;
            set_string(
                key,
                "QuietUninstallString",
                &format!("\"{uninstaller}\" --uninstall --silent"),
            )?;
            set_dword(key, "NoModify", 1)?;
            set_dword(key, "NoRepair", 1)?;
            set_dword(key, "EstimatedSize", entry.estimated_kb)
        })();
        unsafe {
            let _ = RegCloseKey(key);
        }
        result
    }

    pub fn remove_uninstall_entry() {
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(UNINSTALL_KEY));
        }
    }

    fn registry_string(root: HKEY, subkey: &str, name: &str) -> Option<String> {
        let mut buffer = [0u16; 128];
        let mut size = (buffer.len() * 2) as u32;
        let status = unsafe {
            RegGetValueW(
                root,
                &HSTRING::from(subkey),
                &HSTRING::from(name),
                RRF_RT_REG_SZ,
                None,
                Some(buffer.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..len]))
    }

    /// The Evergreen WebView2 runtime registers a version under EdgeUpdate.
    pub fn webview2_installed() -> bool {
        [
            (
                HKEY_LOCAL_MACHINE,
                format!(r"SOFTWARE\WOW6432Node\{WEBVIEW2_CLIENT}"),
            ),
            (HKEY_LOCAL_MACHINE, format!(r"SOFTWARE\{WEBVIEW2_CLIENT}")),
            (HKEY_CURRENT_USER, format!(r"Software\{WEBVIEW2_CLIENT}")),
        ]
        .iter()
        .filter_map(|(root, key)| registry_string(*root, key, "pv"))
        .any(|version| !version.is_empty() && version != "0.0.0.0")
    }

    pub fn message_box_yes_no(title: &str, text: &str) -> bool {
        unsafe {
            MessageBoxW(
                None,
                &HSTRING::from(text),
                &HSTRING::from(title),
                MB_YESNO | MB_ICONQUESTION,
            ) == IDYES
        }
    }

    pub fn show_error(title: &str, text: &str) {
        unsafe {
            MessageBoxW(
                None,
                &HSTRING::from(text),
                &HSTRING::from(title),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::UninstallEntry;
    use std::path::Path;

    pub fn create_shortcut(_link: &Path, _exe: &Path) -> Result<(), String> {
        Ok(())
    }
    pub fn write_uninstall_entry(_entry: &UninstallEntry) -> Result<(), String> {
        Ok(())
    }
    pub fn remove_uninstall_entry() {}
    pub fn webview2_installed() -> bool {
        true
    }
    pub fn message_box_yes_no(_title: &str, text: &str) -> bool {
        eprintln!("{text}");
        false
    }
    pub fn show_error(_title: &str, text: &str) {
        eprintln!("{text}");
    }
}
