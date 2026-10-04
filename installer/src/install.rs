//! What Pronto Setup actually does: download the chosen speech packs, unpack
//! the app, and register it. Uninstall reverses the same list.
use crate::system;
use serde::Serialize;
use speech_packs::{AsrModel, InstallPhase};
use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// The Pronto app (pronto.exe + resources), embedded at build time.
pub const PAYLOAD: &[u8] = include_bytes!(env!("PRONTO_PAYLOAD"));
pub const APP_VERSION: &str = env!("PRONTO_APP_VERSION");

/// Share of the progress bar given to downloads; files + registration take the rest.
const DOWNLOAD_SHARE: f64 = 0.9;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupProgress {
    /// download | files | finish | done | error | cancelled
    pub stage: &'static str,
    pub message: String,
    /// 0.0 ..= 1.0 across the whole install.
    pub fraction: f64,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub bytes_per_sec: u64,
    pub eta_secs: Option<u64>,
}

impl SetupProgress {
    pub fn simple(stage: &'static str, fraction: f64, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: message.into(),
            fraction,
            downloaded_bytes: 0,
            total_bytes: 0,
            bytes_per_sec: 0,
            eta_secs: None,
        }
    }
}

pub fn has_payload() -> bool {
    !PAYLOAD.is_empty()
}

pub fn is_installed(dir: &Path) -> bool {
    dir.join("pronto.exe").is_file()
}

/// The backend recorded in an existing install's settings, if any.
pub fn current_model(dir: &Path) -> Option<AsrModel> {
    let text = fs::read_to_string(dir.join("settings.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    match value.get("asrModel") {
        Some(model) => serde_json::from_value(model.clone()).ok(),
        // Settings written before model switching existed mean Parakeet.
        None => Some(AsrModel::Parakeet),
    }
}

/// An older NSIS install bundled the CUDA runtime beside pronto.exe.
fn legacy_cuda_runtime(dir: &Path, pack: &speech_packs::Pack) -> bool {
    pack.id == "nemo-speech-cuda"
        && dir
            .join("runtime/nemo-speech/bin/nemo-speech.exe")
            .is_file()
}

pub fn download_bytes(dir: &Path, model: AsrModel) -> u64 {
    speech_packs::missing_packs(dir, model)
        .into_iter()
        .filter(|pack| !legacy_cuda_runtime(dir, pack))
        .map(|pack| pack.display_bytes())
        .sum()
}

pub fn install(
    dir: &Path,
    model: AsrModel,
    cancel: &AtomicBool,
    mut emit: impl FnMut(SetupProgress),
) -> Result<(), String> {
    if !has_payload() {
        return Err("This copy of Pronto Setup was built without the app. Download the installer from the Pronto releases page.".into());
    }
    let name = model.short_name();
    emit(SetupProgress::simple(
        "download",
        0.0,
        format!("Getting {name} ready"),
    ));
    speech_packs::install_model(
        dir,
        model,
        |pack| legacy_cuda_runtime(dir, pack),
        cancel,
        |progress| {
            let fraction = if progress.total_bytes == 0 {
                DOWNLOAD_SHARE
            } else {
                DOWNLOAD_SHARE * progress.downloaded_bytes as f64 / progress.total_bytes as f64
            };
            let message = match progress.phase {
                InstallPhase::Verifying => format!("Verifying {name}"),
                InstallPhase::Unpacking => format!("Unpacking {name}"),
                InstallPhase::Done => format!("{name} is ready"),
                _ => format!("Downloading {name}"),
            };
            emit(SetupProgress {
                stage: "download",
                message,
                fraction,
                downloaded_bytes: progress.downloaded_bytes,
                total_bytes: progress.total_bytes,
                bytes_per_sec: progress.bytes_per_sec,
                eta_secs: progress.eta_secs,
            });
        },
    )?;
    if cancel.load(Ordering::Acquire) {
        return Err("Setup was cancelled".into());
    }

    emit(SetupProgress::simple(
        "files",
        DOWNLOAD_SHARE,
        "Installing Pronto",
    ));
    system::stop_pronto();
    extract_payload(dir)?;
    let uninstaller = dir.join("uninstall.exe");
    if let Ok(me) = std::env::current_exe() {
        if !same_file(&me, &uninstaller) {
            fs::copy(&me, &uninstaller)
                .map_err(|e| format!("Could not save the uninstaller: {e}"))?;
        }
    }

    emit(SetupProgress::simple("finish", 0.97, "Finishing up"));
    write_model_choice(dir, model)?;
    let exe = dir.join("pronto.exe");
    system::create_shortcuts(&exe)?;
    system::write_uninstall_entry(&system::UninstallEntry {
        version: APP_VERSION,
        install_dir: dir,
        estimated_kb: (folder_size(dir) / 1024).min(u32::MAX as u64) as u32,
    })?;
    emit(SetupProgress::simple("done", 1.0, "Pronto is ready"));
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Overwrite the app files in place. Each file goes to a temp name first so a
/// failed write never leaves a truncated executable behind.
fn extract_payload(dir: &Path) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(PAYLOAD))
        .map_err(|e| format!("The installer is damaged: {e}"))?;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|e| e.to_string())?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(format!(
                "Unsafe path in installer payload: {}",
                entry.name()
            ));
        };
        let target = dir.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let staging = target.with_extension("setup-new");
        {
            let mut out = fs::File::create(&staging).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut out)
                .map_err(|e| format!("Could not write {}: {e}", target.display()))?;
        }
        if target.exists() {
            fs::remove_file(&target).map_err(|e| {
                format!(
                    "Could not replace {} (is Pronto still open?): {e}",
                    target.display()
                )
            })?;
        }
        fs::rename(&staging, &target).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Record the chosen backend in Pronto's settings, keeping everything else.
pub fn write_model_choice(dir: &Path, model: AsrModel) -> Result<(), String> {
    let path = dir.join("settings.json");
    let mut value = fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    value["asrModel"] = serde_json::to_value(model).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    fs::write(&path, text).map_err(|e| format!("Could not save your choice: {e}"))
}

fn folder_size(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => folder_size(&entry.path()),
            Ok(_) => entry.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

/// Personal data that "Keep my history and settings" preserves.
const USER_DATA: &[&str] = &[
    "settings.json",
    "history.json",
    "Meetings",
    "NoteTakerAudio",
    "design-system.json",
];

pub fn uninstall(
    dir: &Path,
    keep_data: bool,
    mut emit: impl FnMut(SetupProgress),
) -> Result<(), String> {
    emit(SetupProgress::simple("files", 0.2, "Closing Pronto"));
    system::stop_pronto();
    emit(SetupProgress::simple("files", 0.5, "Removing Pronto"));
    system::remove_shortcuts();
    system::remove_uninstall_entry();
    if keep_data {
        let Ok(entries) = fs::read_dir(dir) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if USER_DATA.iter().any(|keep| name == *keep) {
                continue;
            }
            let path = entry.path();
            let removed = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            if let Err(error) = removed {
                return Err(format!("Could not remove {}: {error}", path.display()));
            }
        }
    } else if dir.exists() {
        fs::remove_dir_all(dir).map_err(|e| format!("Could not remove {}: {e}", dir.display()))?;
    }
    emit(SetupProgress::simple("done", 1.0, "Pronto was removed"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pronto-setup-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn model_choice_merges_into_existing_settings() {
        let dir = temp_dir("settings");
        assert_eq!(current_model(&dir), None);
        write_model_choice(&dir, AsrModel::Phonon).unwrap();
        assert_eq!(current_model(&dir), Some(AsrModel::Phonon));
        fs::write(
            dir.join("settings.json"),
            r#"{"language":"de","asrModel":"phonon"}"#,
        )
        .unwrap();
        write_model_choice(&dir, AsrModel::Parakeet).unwrap();
        let saved: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(saved["language"], "de");
        assert_eq!(saved["asrModel"], "parakeet");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn settings_from_before_model_switching_mean_parakeet() {
        let dir = temp_dir("legacy");
        fs::write(dir.join("settings.json"), r#"{"language":"en"}"#).unwrap();
        assert_eq!(current_model(&dir), Some(AsrModel::Parakeet));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn uninstall_keeps_personal_data_when_asked() {
        let dir = temp_dir("uninstall");
        fs::write(dir.join("pronto.exe"), b"x").unwrap();
        fs::write(dir.join("history.json"), b"[]").unwrap();
        fs::write(dir.join("settings.json"), b"{}").unwrap();
        fs::create_dir_all(dir.join("models/phonon-2")).unwrap();
        fs::write(dir.join("models/phonon-2/config.json"), b"{}").unwrap();
        uninstall(&dir, true, |_| {}).unwrap();
        assert!(!dir.join("pronto.exe").exists());
        assert!(!dir.join("models").exists());
        assert!(dir.join("history.json").exists());
        assert!(dir.join("settings.json").exists());
        uninstall(&dir, false, |_| {}).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn install_refuses_without_payload_in_dev_builds() {
        if has_payload() {
            return;
        }
        let dir = temp_dir("no-payload");
        let cancel = AtomicBool::new(false);
        assert!(install(&dir, AsrModel::Phonon, &cancel, |_| {}).is_err());
        assert!(!dir.join("pronto.exe").exists());
        let _ = fs::remove_dir_all(dir);
    }
}
