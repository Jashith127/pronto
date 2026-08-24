mod audio;
mod engine;
mod gpu_memory;
mod hotkey;
mod insert;
mod pipeline;
mod settings;
#[cfg(windows)]
mod single_instance;
mod sound;
mod startup;
mod system_audio;

use audio::{AudioController, MicrophoneStatus};
use engine::{CompletedTranscription, EngineController, ModelStatus, TranscriptionJob};
use hotkey::{Hotkey, HotkeyController, HotkeyEvent, HotkeyStatus};
use pipeline::{EngineStatus, Phase, Pipeline};
use settings::{ActivationMode, AppPreferences, HistoryEntry, SettingsStore, UserSettings};
use sound::SoundController;
use std::sync::Mutex;
use system_audio::SystemAudioController;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition};

struct AppState {
    pipeline: Mutex<Pipeline>,
    audio: AudioController,
    system_audio: SystemAudioController,
    insertion_target: insert::InsertionTargetTracker,
    sounds: SoundController,
    engine: Mutex<Option<EngineController>>,
    settings: SettingsStore,
    target_window: Mutex<isize>,
    model_status: Mutex<ModelStatus>,
    active_shortcut: Mutex<Hotkey>,
    hotkey_controller: Mutex<Option<HotkeyController>>,
    hotkey_error: Mutex<Option<String>>,
    show_microphone_once: Mutex<bool>,
}

impl AppState {
    fn new() -> Self {
        let settings = SettingsStore::load();
        let configured = settings
            .snapshot()
            .map(|value| value.hotkey)
            .unwrap_or_else(|_| hotkey::DEFAULT_HOTKEY.into());
        let selected_microphone = settings
            .snapshot()
            .ok()
            .and_then(|value| value.microphone_id);
        let active_shortcut = hotkey::parse(&configured)
            .or_else(|_| hotkey::parse(hotkey::DEFAULT_HOTKEY))
            .expect("the built-in shortcut must be valid");
        let canonical = active_shortcut.canonical().to_string();
        if canonical != configured {
            if let Ok(mut repaired) = settings.snapshot() {
                repaired.hotkey = canonical;
                let _ = settings.replace(repaired);
            }
        }
        Self {
            pipeline: Mutex::new(Pipeline::default()),
            audio: AudioController::new(selected_microphone),
            system_audio: SystemAudioController::new(),
            insertion_target: insert::InsertionTargetTracker::new(),
            sounds: SoundController::new(),
            engine: Mutex::new(None),
            settings,
            target_window: Mutex::new(0),
            model_status: Mutex::new(ModelStatus {
                ready: false,
                message: "Starting local speech engine…".into(),
                backend: "NVIDIA Parakeet TDT 0.6B v3 · CUDA".into(),
            }),
            active_shortcut: Mutex::new(active_shortcut),
            hotkey_controller: Mutex::new(None),
            hotkey_error: Mutex::new(None),
            show_microphone_once: Mutex::new(true),
        }
    }
}

fn emit_status(app: &AppHandle, status: &EngineStatus) {
    let _ = app.emit("engine-status", status);
}

pub(crate) fn set_model_status(app: &AppHandle, status: ModelStatus) {
    if let Ok(mut current) = app.state::<AppState>().model_status.lock() {
        *current = status.clone();
    }
    let _ = app.emit("model-status", status);
}

fn begin_recording(app: &AppHandle) -> Result<EngineStatus, String> {
    let state = app.state::<AppState>();
    let settings = state.settings.snapshot()?;
    let mut pipeline = state
        .pipeline
        .lock()
        .map_err(|_| "pipeline lock poisoned")?;
    if !pipeline.begin() {
        return Ok(pipeline.status.clone());
    }
    pipeline.status.message = match settings.activation_mode {
        ActivationMode::Hold => "Listening… release to transcribe".into(),
        ActivationMode::Toggle => "Listening… press your shortcut again to finish".into(),
    };

    let initial_target = insert::foreground_window();
    *state
        .target_window
        .lock()
        .map_err(|_| "target window lock poisoned")? = initial_target;
    state.insertion_target.begin(initial_target);
    if settings.duck_audio {
        if let Err(error) = state.system_audio.duck() {
            let _ = app.emit("audio-warning", error);
        }
    }
    match state.audio.start() {
        Err(error) => {
            state.insertion_target.cancel();
            let _ = state.system_audio.restore();
            pipeline.fail(format!("Microphone unavailable: {error}"));
        }
        Ok(microphone_name) => {
            if settings.dictation_sounds {
                state.sounds.start();
            }
            if let Ok(engine) = state.engine.lock() {
                if let Some(engine) = engine.as_ref() {
                    engine.warm();
                }
            }
            let show_microphone = state
                .show_microphone_once
                .lock()
                .map(|mut first| {
                    let show = *first;
                    *first = false;
                    show
                })
                .unwrap_or(false);
            if let Some(overlay) = app.get_webview_window("overlay") {
                let microphone_width = show_microphone
                    .then(|| (microphone_name.chars().count() as f64 * 6.2 + 24.0).max(96.0));
                position_overlay(&overlay, microphone_width);
                let _ = overlay.show();
                if show_microphone {
                    let _ = app.emit(
                        "microphone-activated",
                        serde_json::json!({ "name": microphone_name }),
                    );
                }
            }
        }
    }
    emit_status(app, &pipeline.status);
    Ok(pipeline.status.clone())
}

fn finish_recording(app: &AppHandle) -> Result<EngineStatus, String> {
    let state = app.state::<AppState>();
    let recording = state.audio.stop();
    let restore_result = state.system_audio.restore();
    let recording = recording?;
    if let Err(error) = restore_result {
        let _ = app.emit("audio-warning", error);
    }
    let target_window = *state
        .target_window
        .lock()
        .map_err(|_| "target window lock poisoned")?;
    let settings = state.settings.snapshot()?;
    if settings.dictation_sounds {
        state.sounds.finish();
    }
    let mut pipeline = state
        .pipeline
        .lock()
        .map_err(|_| "pipeline lock poisoned")?;
    if !pipeline.processing(recording.samples.len(), recording.sample_rate) {
        return Ok(pipeline.status.clone());
    }
    emit_status(app, &pipeline.status);

    let engine = state.engine.lock().map_err(|_| "engine lock poisoned")?;
    if let Some(engine) = engine.as_ref() {
        engine.transcribe(TranscriptionJob {
            recording,
            settings,
            target_window,
        })?;
    } else {
        pipeline.fail("Transcription engine is still starting");
        emit_status(app, &pipeline.status);
    }
    Ok(pipeline.status.clone())
}

fn position_overlay(window: &tauri::WebviewWindow, microphone_width: Option<f64>) {
    let logical_height = if microphone_width.is_some() {
        54.0
    } else {
        26.0
    };
    let monitor = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        let logical_width = microphone_width.unwrap_or(96.0).max(96.0);
        let _ = window.set_size(LogicalSize::new(logical_width, logical_height));
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let max_logical_width = monitor.size().width as f64 / scale - 24.0;
    let logical_width = microphone_width
        .unwrap_or(96.0)
        .max(96.0)
        .min(max_logical_width.max(96.0));
    let _ = window.set_size(LogicalSize::new(logical_width, logical_height));
    let width = (logical_width * scale).round() as u32;
    let height = (logical_height * scale).round() as u32;
    let area = monitor.size();
    let origin = monitor.position();
    let x = origin.x + (area.width.saturating_sub(width) / 2) as i32;
    let y = origin.y + area.height.saturating_sub(height + 74) as i32;
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

pub(crate) fn complete_transcription(
    app: &AppHandle,
    result: Result<CompletedTranscription, String>,
) {
    let state = app.state::<AppState>();
    let mut pipeline = match state.pipeline.lock() {
        Ok(pipeline) => pipeline,
        Err(_) => return,
    };
    match result {
        Ok(completed) => {
            let insertion_target = state.insertion_target.finish(completed.target_window);
            if let Ok(mut remembered) = state.target_window.lock() {
                *remembered = insertion_target;
            }
            let insertion_error = if completed.auto_insert {
                insert::insert_text(insertion_target, &completed.entry.final_text).err()
            } else {
                None
            };
            let message = match (completed.cleanup_warning.as_ref(), insertion_error.as_ref()) {
                (_, Some(error)) => format!("Transcribed, but text insertion failed: {error}"),
                (Some(warning), None) => format!("Inserted with local cleanup · {warning}"),
                (None, None) => format!("Inserted in {} ms", completed.entry.total_ms),
            };
            pipeline.complete(
                completed.entry.final_text.clone(),
                completed.entry.asr_ms,
                completed.entry.cleanup_ms,
                completed.entry.total_ms,
                message,
            );
            let _ = state.settings.push_history(completed.entry.clone());
            let _ = app.emit("history-updated", completed.entry);
        }
        Err(error) => {
            state.insertion_target.cancel();
            pipeline.fail(error)
        }
    }
    emit_status(app, &pipeline.status);
    if let Some(overlay) = app.get_webview_window("overlay") {
        let _ = overlay.hide();
    }
}

#[tauri::command]
fn get_status(state: tauri::State<'_, AppState>) -> Result<EngineStatus, String> {
    Ok(state
        .pipeline
        .lock()
        .map_err(|_| "pipeline lock poisoned")?
        .status
        .clone())
}

#[tauri::command]
fn get_model_status(state: tauri::State<'_, AppState>) -> Result<ModelStatus, String> {
    state
        .model_status
        .lock()
        .map(|status| status.clone())
        .map_err(|_| "model status lock poisoned".into())
}

#[tauri::command]
fn start_recording(app: AppHandle) -> Result<EngineStatus, String> {
    begin_recording(&app)
}

#[tauri::command]
fn stop_recording(app: AppHandle) -> Result<EngineStatus, String> {
    finish_recording(&app)
}

#[tauri::command]
fn cancel_recording(app: AppHandle) -> Result<EngineStatus, String> {
    let state = app.state::<AppState>();
    let _ = state.audio.stop();
    state.insertion_target.cancel();
    let _ = state.system_audio.restore();
    let mut pipeline = state
        .pipeline
        .lock()
        .map_err(|_| "pipeline lock poisoned")?;
    pipeline.reset();
    pipeline.status.message = "Dictation cancelled".into();
    emit_status(&app, &pipeline.status);
    if let Some(overlay) = app.get_webview_window("overlay") {
        let _ = overlay.hide();
    }
    Ok(pipeline.status.clone())
}

fn handle_hotkey_event(app: &AppHandle, event: HotkeyEvent) {
    let mode = app
        .state::<AppState>()
        .settings
        .snapshot()
        .map(|settings| settings.activation_mode)
        .unwrap_or_default();
    match (mode, event) {
        (ActivationMode::Hold, HotkeyEvent::Pressed) => {
            let _ = begin_recording(app);
        }
        (ActivationMode::Hold, HotkeyEvent::Released) => {
            let _ = finish_recording(app);
        }
        (ActivationMode::Toggle, HotkeyEvent::Pressed) => {
            let phase = app
                .state::<AppState>()
                .pipeline
                .lock()
                .ok()
                .map(|pipeline| pipeline.status.phase.clone());
            match phase {
                Some(Phase::Listening) => {
                    let _ = finish_recording(app);
                }
                Some(Phase::Idle | Phase::Complete | Phase::Error) => {
                    let _ = begin_recording(app);
                }
                _ => {}
            }
        }
        (ActivationMode::Toggle, HotkeyEvent::Released) => {}
    }
}

#[tauri::command]
fn reset(app: AppHandle) -> Result<EngineStatus, String> {
    let state = app.state::<AppState>();
    state.insertion_target.cancel();
    let _ = state.system_audio.restore();
    let mut pipeline = state
        .pipeline
        .lock()
        .map_err(|_| "pipeline lock poisoned")?;
    pipeline.reset();
    emit_status(&app, &pipeline.status);
    Ok(pipeline.status.clone())
}

#[tauri::command]
fn get_preferences(state: tauri::State<'_, AppState>) -> Result<AppPreferences, String> {
    state.settings.preferences()
}

#[tauri::command]
fn save_settings(
    state: tauri::State<'_, AppState>,
    mut settings: UserSettings,
) -> Result<AppPreferences, String> {
    // Shortcut changes are transactional through set_hotkey so a settings
    // write can never persist a shortcut that Windows rejected.
    let previous = state.settings.snapshot()?;
    let gpu_memory_management_changed =
        settings.gpu_memory_management != previous.gpu_memory_management;
    settings.hotkey = previous.hotkey;
    settings.microphone_id = previous.microphone_id;
    settings.microphone_name = previous.microphone_name;
    if settings.launch_at_startup != previous.launch_at_startup {
        startup::set_enabled(settings.launch_at_startup)?;
    }
    match state.settings.replace(settings) {
        Ok(preferences) => {
            if gpu_memory_management_changed {
                if let Ok(engine) = state.engine.lock() {
                    if let Some(engine) = engine.as_ref() {
                        engine
                            .set_gpu_memory_management(preferences.settings.gpu_memory_management);
                    }
                }
            }
            Ok(preferences)
        }
        Err(error) => {
            let _ = startup::set_enabled(previous.launch_at_startup);
            Err(error)
        }
    }
}

#[tauri::command]
fn get_microphones(state: tauri::State<'_, AppState>) -> Result<MicrophoneStatus, String> {
    state.audio.status()
}

#[tauri::command]
fn set_microphone(
    state: tauri::State<'_, AppState>,
    device_id: Option<String>,
) -> Result<MicrophoneStatus, String> {
    let previous = state.settings.snapshot()?;
    let status = state.audio.select(device_id.clone())?;
    let mut next = previous.clone();
    next.microphone_id = device_id;
    next.microphone_name = Some(status.active_name.clone());
    if let Err(error) = state.settings.replace(next) {
        let _ = state.audio.select(previous.microphone_id);
        return Err(error);
    }
    Ok(status)
}

#[tauri::command]
fn compact_overlay(app: AppHandle) -> Result<(), String> {
    let overlay = app
        .get_webview_window("overlay")
        .ok_or_else(|| "Dictation overlay is unavailable".to_string())?;
    position_overlay(&overlay, None);
    Ok(())
}

#[tauri::command]
fn resize_microphone_overlay(app: AppHandle, width: f64) -> Result<(), String> {
    if !width.is_finite() || width <= 0.0 {
        return Err("Invalid microphone label width".to_string());
    }
    let overlay = app
        .get_webview_window("overlay")
        .ok_or_else(|| "Dictation overlay is unavailable".to_string())?;
    position_overlay(&overlay, Some(width));
    Ok(())
}

#[tauri::command]
fn get_hotkey_status(state: tauri::State<'_, AppState>) -> Result<HotkeyStatus, String> {
    let shortcut = state
        .active_shortcut
        .lock()
        .map_err(|_| "shortcut lock poisoned")?
        .clone();
    let error = state
        .hotkey_error
        .lock()
        .map_err(|_| "shortcut status lock poisoned")?
        .clone();
    Ok(HotkeyStatus {
        shortcut: shortcut.canonical().to_string(),
        registered: state
            .hotkey_controller
            .lock()
            .map(|value| value.is_some())
            .unwrap_or(false),
        error,
    })
}

#[tauri::command]
fn set_hotkey(app: AppHandle, hotkey: String) -> Result<HotkeyStatus, String> {
    let next = hotkey::parse(&hotkey)?;
    let canonical = next.canonical().to_string();
    let state = app.state::<AppState>();
    let previous = state
        .active_shortcut
        .lock()
        .map_err(|_| "shortcut lock poisoned")?
        .clone();
    let controller = state
        .hotkey_controller
        .lock()
        .map_err(|_| "shortcut controller lock poisoned")?;
    let controller = controller
        .as_ref()
        .ok_or_else(|| "Shortcut listener is still starting".to_string())?;
    controller.update(next.clone())?;

    let mut settings = state.settings.snapshot()?;
    settings.hotkey = canonical;
    if let Err(error) = state.settings.replace(settings) {
        let _ = controller.update(previous);
        return Err(format!(
            "The shortcut worked but could not be saved: {error}"
        ));
    }
    *state
        .active_shortcut
        .lock()
        .map_err(|_| "shortcut lock poisoned")? = next.clone();
    *state
        .hotkey_error
        .lock()
        .map_err(|_| "shortcut status lock poisoned")? = None;
    let status = HotkeyStatus {
        shortcut: next.canonical().to_string(),
        registered: true,
        error: None,
    };
    let _ = app.emit("hotkey-status", status.clone());
    Ok(status)
}

#[tauri::command]
fn save_api_key(
    state: tauri::State<'_, AppState>,
    api_key: String,
) -> Result<AppPreferences, String> {
    settings::set_deepseek_key(&api_key)?;
    state.settings.preferences()
}

#[tauri::command]
fn add_dictionary_term(
    state: tauri::State<'_, AppState>,
    term: String,
) -> Result<UserSettings, String> {
    state.settings.add_dictionary_term(term)
}

#[tauri::command]
fn remove_dictionary_term(
    state: tauri::State<'_, AppState>,
    term: String,
) -> Result<UserSettings, String> {
    state.settings.remove_dictionary_term(&term)
}

#[tauri::command]
fn get_history(state: tauri::State<'_, AppState>) -> Result<Vec<HistoryEntry>, String> {
    state.settings.history()
}

#[tauri::command]
fn clear_history(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.settings.clear_history()
}

#[tauri::command]
fn insert_again(text: String) -> Result<(), String> {
    insert::insert_text(insert::foreground_window(), &text)
}

fn paste_last(app: &AppHandle) -> Result<String, String> {
    let state = app.state::<AppState>();
    let text = state
        .settings
        .last_transcript()?
        .ok_or_else(|| "There is no previous transcript yet".to_string())?;
    let target = *state
        .target_window
        .lock()
        .map_err(|_| "target window lock poisoned")?;
    if insert::copy_and_paste(target, &text)? {
        Ok("Last transcript pasted".into())
    } else {
        Ok("Last transcript copied to the clipboard".into())
    }
}

#[tauri::command]
fn paste_last_transcript(app: AppHandle) -> Result<String, String> {
    paste_last(&app)
}

#[tauri::command]
fn copy_transcript(state: tauri::State<'_, AppState>, id: String) -> Result<(), String> {
    let id = id
        .parse::<u128>()
        .map_err(|_| "Invalid transcript identifier".to_string())?;
    let text = state
        .settings
        .transcript(id)?
        .ok_or_else(|| "That transcript is no longer in history".to_string())?;
    insert::copy_to_clipboard(&text)
}

#[tauri::command]
fn minimize_main_window(app: AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "Pronto's main window is unavailable".to_string())?;
    window.minimize().map_err(|error| error.to_string())
}

#[tauri::command]
fn hide_main_window(app: AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "Pronto's main window is unavailable".to_string())?;
    window.hide().map_err(|error| error.to_string())
}

fn create_tray(app: &tauri::App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Pronto", true, None::<&str>)?;
    let paste = MenuItem::with_id(
        app,
        "paste-last",
        "Paste Last Transcript",
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &paste, &quit])?;
    let mut tray = TrayIconBuilder::new()
        .tooltip("Pronto dictation")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "paste-last" => {
                let result = paste_last(app);
                let (message, error) = match result {
                    Ok(message) => (message, false),
                    Err(message) => (message, true),
                };
                if error {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
                let _ = app.emit(
                    "tray-message",
                    serde_json::json!({
                        "message": message,
                        "error": error
                    }),
                );
            }
            "quit" => {
                let _ = app.state::<AppState>().system_audio.restore();
                app.exit(0);
            }
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(windows)]
    let _single_instance = match single_instance::SingleInstance::acquire() {
        Ok(Some(instance)) => instance,
        Ok(None) => return,
        Err(_) => return,
    };
    tauri::Builder::default()
        // Builder-managed state exists before configured WebViews are created,
        // so early IPC and WebView2 lifecycle callbacks cannot race setup().
        .manage(AppState::new())
        .setup(|app| {
            let shortcut = app
                .state::<AppState>()
                .active_shortcut
                .lock()
                .expect("shortcut lock poisoned")
                .clone();
            let resource_dir = app.path().resource_dir().ok();
            let gpu_memory_management = app
                .state::<AppState>()
                .settings
                .snapshot()
                .map(|settings| settings.gpu_memory_management)
                .unwrap_or(true);
            let engine =
                EngineController::new(app.handle().clone(), resource_dir, gpu_memory_management);
            *app.state::<AppState>()
                .engine
                .lock()
                .expect("engine lock poisoned") = Some(engine);
            let handle = app.handle().clone();
            match HotkeyController::new(shortcut, move |event| handle_hotkey_event(&handle, event))
            {
                Ok(controller) => {
                    *app.state::<AppState>()
                        .hotkey_controller
                        .lock()
                        .expect("shortcut controller lock poisoned") = Some(controller);
                }
                Err(error) => {
                    *app.state::<AppState>()
                        .hotkey_error
                        .lock()
                        .expect("shortcut status lock poisoned") = Some(error);
                }
            }
            create_tray(app)?;
            if !startup::is_background_launch() {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            get_model_status,
            start_recording,
            stop_recording,
            cancel_recording,
            reset,
            get_preferences,
            save_settings,
            get_microphones,
            set_microphone,
            compact_overlay,
            resize_microphone_overlay,
            get_hotkey_status,
            set_hotkey,
            save_api_key,
            add_dictionary_term,
            remove_dictionary_term,
            get_history,
            clear_history,
            insert_again,
            paste_last_transcript,
            copy_transcript,
            minimize_main_window,
            hide_main_window
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pronto");
}
