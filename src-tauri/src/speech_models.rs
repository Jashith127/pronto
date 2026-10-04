//! Settings → Advanced → Speech model: list the backends, download the packs
//! a switch needs (with live progress), then hand the engine the new backend.
use crate::engine::bundled_cuda_runtime;
use serde::Serialize;
use speech_packs::{AsrModel, GpuInfo, InstallPhase, Pack};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{AppHandle, Emitter, Manager};

const STATUS_EVENT: &str = "speech-model-status";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchStatus {
    pub model: AsrModel,
    /// preparing | downloading | verifying | unpacking | starting | done | error | cancelled
    pub phase: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub bytes_per_sec: u64,
    pub eta_secs: Option<u64>,
    pub message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechModelInfo {
    id: AsrModel,
    name: &'static str,
    installed: bool,
    recommended: bool,
    /// Bytes still to download before this backend can run.
    download_bytes: u64,
    english_only: bool,
    uses_gpu: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechModelsView {
    /// False on macOS, where only Parakeet (Metal) ships.
    supported: bool,
    active: AsrModel,
    gpu_name: Option<String>,
    has_nvidia: bool,
    models: Vec<SpeechModelInfo>,
    switching: Option<SwitchStatus>,
}

#[derive(Default)]
pub struct SpeechModelManager {
    active: AtomicBool,
    cancel: Arc<AtomicBool>,
    status: Mutex<Option<SwitchStatus>>,
}

fn gpu() -> &'static GpuInfo {
    static GPU: OnceLock<GpuInfo> = OnceLock::new();
    GPU.get_or_init(speech_packs::detect_gpus)
}

fn data_root() -> PathBuf {
    crate::platform_paths::data_dir()
}

/// Packs that are already satisfied some other way.
fn satisfied_elsewhere(pack: &Pack, resource_dir: Option<&Path>) -> bool {
    pack.id == "nemo-speech-cuda" && bundled_cuda_runtime(resource_dir)
}

fn missing(model: AsrModel, resource_dir: Option<&Path>) -> Vec<&'static Pack> {
    speech_packs::missing_packs(&data_root(), model)
        .into_iter()
        .filter(|pack| !satisfied_elsewhere(pack, resource_dir))
        .collect()
}

pub fn supported() -> bool {
    cfg!(windows)
}

impl SpeechModelManager {
    pub fn view(&self, app: &AppHandle) -> Result<SpeechModelsView, String> {
        let resource_dir = app.path().resource_dir().ok();
        let active = app
            .state::<crate::AppState>()
            .settings
            .snapshot()?
            .asr_model;
        let gpu = gpu();
        let recommended = speech_packs::recommended_model(gpu);
        let models = AsrModel::ALL
            .into_iter()
            .map(|model| {
                let missing = missing(model, resource_dir.as_deref());
                SpeechModelInfo {
                    id: model,
                    name: model.short_name(),
                    installed: missing.is_empty(),
                    recommended: model == recommended,
                    download_bytes: missing.iter().map(|pack| pack.display_bytes()).sum(),
                    english_only: model.english_only(),
                    uses_gpu: model.uses_gpu(),
                }
            })
            .collect();
        Ok(SpeechModelsView {
            supported: supported(),
            active,
            gpu_name: gpu.primary().map(|adapter| adapter.name.clone()),
            has_nvidia: gpu.has_nvidia(),
            models,
            switching: self.status.lock().ok().and_then(|status| status.clone()),
        })
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub fn switch(&self, app: AppHandle, model: AsrModel) -> Result<(), String> {
        if !supported() {
            return Err("Switching speech models is available on Windows only.".into());
        }
        if self.active.swap(true, Ordering::AcqRel) {
            return Err("A speech model download is already in progress.".into());
        }
        self.cancel.store(false, Ordering::Release);
        let cancel = Arc::clone(&self.cancel);
        let spawned = std::thread::Builder::new()
            .name("pronto-speech-switch".into())
            .spawn(move || {
                let manager = app.state::<SpeechModelManager>();
                let result = run_switch(&app, &manager, model, &cancel);
                if let Err(error) = result {
                    let phase = if cancel.load(Ordering::Acquire) {
                        "cancelled"
                    } else {
                        "error"
                    };
                    manager.publish(&app, model, phase, 0, 0, error);
                }
                manager.active.store(false, Ordering::Release);
            });
        if let Err(error) = spawned {
            self.active.store(false, Ordering::Release);
            return Err(error.to_string());
        }
        Ok(())
    }

    fn publish(
        &self,
        app: &AppHandle,
        model: AsrModel,
        phase: &str,
        downloaded: u64,
        total: u64,
        message: impl Into<String>,
    ) {
        self.emit(
            app,
            SwitchStatus {
                model,
                phase: phase.into(),
                downloaded_bytes: downloaded,
                total_bytes: total,
                bytes_per_sec: 0,
                eta_secs: None,
                message: message.into(),
            },
        );
    }

    fn emit(&self, app: &AppHandle, status: SwitchStatus) {
        if let Ok(mut current) = self.status.lock() {
            *current = Some(status.clone());
        }
        let _ = app.emit(STATUS_EVENT, status);
    }
}

fn run_switch(
    app: &AppHandle,
    manager: &SpeechModelManager,
    model: AsrModel,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let resource_dir = app.path().resource_dir().ok();
    let name = model.short_name();
    manager.publish(app, model, "preparing", 0, 0, format!("Preparing {name}…"));
    speech_packs::install_model(
        &data_root(),
        model,
        |pack| satisfied_elsewhere(pack, resource_dir.as_deref()),
        cancel,
        |progress| {
            let message = match progress.phase {
                InstallPhase::Verifying => format!("Verifying {name}…"),
                InstallPhase::Unpacking => format!("Unpacking {name}…"),
                InstallPhase::Done => format!("{name} downloaded"),
                _ => format!("Downloading {name}…"),
            };
            let phase = match progress.phase {
                InstallPhase::Preparing => "preparing",
                InstallPhase::Downloading => "downloading",
                InstallPhase::Verifying => "verifying",
                InstallPhase::Unpacking => "unpacking",
                InstallPhase::Done => "starting",
            };
            manager.emit(
                app,
                SwitchStatus {
                    model,
                    phase: phase.into(),
                    downloaded_bytes: progress.downloaded_bytes,
                    total_bytes: progress.total_bytes,
                    bytes_per_sec: progress.bytes_per_sec,
                    eta_secs: progress.eta_secs,
                    message,
                },
            );
        },
    )?;
    if cancel.load(Ordering::Acquire) {
        return Err("Switch cancelled".into());
    }
    let state = app.state::<crate::AppState>();
    state.settings.set_asr_model(model)?;
    if let Some(engine) = state.engine.lock().ok().and_then(|e| e.as_ref().cloned()) {
        engine.switch_model(model);
    }
    manager.publish(
        app,
        model,
        "done",
        0,
        0,
        format!("Switched to {name}. Warming it up…"),
    );
    Ok(())
}

#[tauri::command]
pub fn get_speech_models(
    app: AppHandle,
    manager: tauri::State<'_, SpeechModelManager>,
) -> Result<SpeechModelsView, String> {
    manager.view(&app)
}

#[tauri::command]
pub fn switch_speech_model(
    app: AppHandle,
    manager: tauri::State<'_, SpeechModelManager>,
    model: AsrModel,
) -> Result<(), String> {
    manager.switch(app.clone(), model)
}

#[tauri::command]
pub fn cancel_speech_model_switch(manager: tauri::State<'_, SpeechModelManager>) {
    manager.cancel();
}
