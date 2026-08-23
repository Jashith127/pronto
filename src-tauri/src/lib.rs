mod audio;
mod engine;
mod hotkey;
mod insert;
mod pipeline;
mod settings;
mod startup;
mod system_audio;

use audio::AudioController;
use engine::{CompletedTranscription, EngineController, ModelStatus, TranscriptionJob};
use hotkey::{Hotkey, HotkeyController, HotkeyEvent, HotkeyStatus};
use pipeline::{EngineStatus, Phase, Pipeline};
use settings::{ActivationMode, AppPreferences, HistoryEntry, SettingsStore, UserSettings};
use std::sync::Mutex;
use system_audio::SystemAudioController;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition};

struct AppState {
    pipeline: Mutex<Pipeline>,
    audio: AudioController,
    system_audio: SystemAudioController,
    engine: Mutex<Option<EngineController>>,
    settings: SettingsStore,
    target_window: Mutex<isize>,
    model_status: Mutex<ModelStatus>,
    active_shortcut: Mutex<Hotkey>,
    hotkey_controller: Mutex<Option<HotkeyController>>,
    hotkey_error: Mutex<Option<String>>,
}

impl AppState {
    fn new() -> Self {
        let settings = SettingsStore::load();
        let configured = settings
            .snapshot()
            .map(|value| value.hotkey)
            .unwrap_or_else(|_| hotkey::DEFAULT_HOTKEY.into());
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
            audio: AudioController::new(),
            system_audio: SystemAudioController::new(),
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

    *state
        .target_window
        .lock()
        .map_err(|_| "target window lock poisoned")? = insert::foreground_window();
    if settings.duck_audio {
        if let Err(error) = state.system_audio.duck() {
            let _ = app.emit("audio-warning", error);
        }
    }
    if let Err(error) = state.audio.start() {
        let _ = state.system_audio.restore();
        pipeline.fail(format!("Microphone unavailable: {error}"));
    } else if let Some(overlay) = app.get_webview_window("overlay") {
        position_overlay(&overlay);
        let _ = overlay.show();
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

fn position_overlay(window: &tauri::WebviewWindow) {
    let monitor = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let width = (96.0 * scale).round() as u32;
    let height = (26.0 * scale).round() as u32;
    #[cfg(windows)]
    if let Ok(hwnd) = window.hwnd() {
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
        };
        let _ = unsafe {
            SetWindowPos(
                hwnd,
                None,
                0,
                0,
                width as i32,
                height as i32,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    }
    let area = monitor.size();
    let origin = monitor.position();
    let x = origin.x + (area.width.saturating_sub(width) / 2) as i32;
    let y = origin.y + area.height.saturating_sub(height + 74) as i32;
    let _ = window.set_position(PhysicalPosition::new(x, y));
    #[cfg(windows)]
    if let Ok(hwnd) = window.hwnd() {
        use windows::Win32::Graphics::Gdi::{CreateRoundRectRgn, SetWindowRgn};
        let radius = height as i32;
        let region = unsafe {
            CreateRoundRectRgn(0, 0, width as i32 + 1, height as i32 + 1, radius, radius)
        };
        if !region.is_invalid() {
            // Windows owns the region after a successful SetWindowRgn call.
            let _ = unsafe { SetWindowRgn(hwnd, Some(region), true) };
        }
    }
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
            let insertion_error = if completed.auto_insert {
                insert::insert_text(completed.target_window, &completed.entry.final_text).err()
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
        Err(error) => pipeline.fail(error),
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
    settings.hotkey = previous.hotkey;
    if settings.launch_at_startup != previous.launch_at_startup {
        startup::set_enabled(settings.launch_at_startup)?;
    }
    match state.settings.replace(settings) {
        Ok(preferences) => Ok(preferences),
        Err(error) => {
            let _ = startup::set_enabled(previous.launch_at_startup);
            Err(error)
        }
    }
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
            let engine = EngineController::new(app.handle().clone(), resource_dir);
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
            get_hotkey_status,
            set_hotkey,
            save_api_key,
            add_dictionary_term,
            remove_dictionary_term,
            get_history,
            clear_history,
            insert_again,
            paste_last_transcript,
            minimize_main_window,
            hide_main_window
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pronto");
}
