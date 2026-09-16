mod audio;
mod engine;
mod gpu_memory;
mod hotkey;
mod insert;
mod mcp;
mod meeting;
#[cfg(windows)]
mod meeting_detector;
#[cfg(windows)]
mod meeting_icon;
mod pipeline;
#[cfg(windows)]
mod power;
mod search;
mod settings;
#[cfg(windows)]
mod single_instance;
mod sound;
mod startup;
mod system_audio;
mod ui_schema;

use audio::{AudioController, MicrophoneStatus};
use engine::{
    CompletedMeetingTranscription, CompletedSearchAsr, CompletedTranscription, EngineController,
    MeetingTranscriptionJob, ModelStatus, SearchAsrJob, TranscriptionJob,
};
use hotkey::{Hotkey, HotkeyController, HotkeyEvent, HotkeyId, HotkeyStatus};
use pipeline::{EngineStatus, Phase, Pipeline};
use search::{
    DuckDuckGoProvider, SearchController, SearchPhase, SearchProvider,
    SearchResultPayload,
    SearchStatus,
};
use settings::{ActivationMode, AppPreferences, HistoryEntry, SettingsStore, UserSettings};
use sound::SoundController;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use system_audio::SystemAudioController;

struct PendingUpload {
    file_name: String,
    bytes: Vec<u8>,
}
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition};

pub(crate) struct AppState {
    pipeline: Mutex<Pipeline>,
    audio: AudioController,
    system_audio: SystemAudioController,
    meetings: meeting::MeetingController,
    search: SearchController,
    insertion_target: insert::InsertionTargetTracker,
    sounds: SoundController,
    engine: Mutex<Option<EngineController>>,
    settings: SettingsStore,
    /// Shared HTTP client for search retrieval + DeepSeek synthesis.
    /// Reused across searches for keep-alive (skips TLS+TCP setup).
    search_http: reqwest::blocking::Client,
    target_window: Mutex<isize>,
    model_status: Mutex<ModelStatus>,
    active_shortcut: Mutex<Hotkey>,
    paste_shortcut: Mutex<Hotkey>,
    search_shortcut: Mutex<Hotkey>,
    hotkey_controller: Mutex<Option<HotkeyController>>,
    hotkey_error: Mutex<Option<String>>,
    paste_hotkey_error: Mutex<Option<String>>,
    search_hotkey_error: Mutex<Option<String>>,
    show_microphone_once: Mutex<bool>,
    pending_uploads: Mutex<HashMap<String, PendingUpload>>,
    meeting_tray_item: Mutex<Option<MenuItem<tauri::Wry>>>,
    detector_control: Arc<meeting_detector::DetectorControl>,
    dictation_active: Arc<AtomicBool>,
    /// Last check-in from the dictation overlay page (`overlay_heartbeat`
    /// command, sent on load and every few seconds while visible). A quiet
    /// page means its renderer is dead or wedged: dictation still works, but
    /// the pill never paints. The backend reloads the page instead of showing
    /// a blank pill. Timers throttle while the window idles hidden, hence the
    /// generous TTL on the read side.
    overlay_heartbeat: Mutex<Option<Instant>>,
    /// When true, losing focus on the search overlay dismisses it.
    /// Kept false while listening so Win+Space Hold release stays reliable.
    search_blur_dismiss: AtomicBool,
}

fn sync_meeting_tray_item(app: &AppHandle, recording: bool) {
    let label = if recording {
        "Stop taking notes"
    } else {
        "Take meeting notes"
    };
    if let Ok(guard) = app.state::<AppState>().meeting_tray_item.lock() {
        if let Some(item) = guard.as_ref() {
            let _ = item.set_text(label);
        }
    }
}

impl AppState {
    pub(crate) fn meeting_suggestions_enabled(&self) -> bool {
        self.settings
            .snapshot()
            .map(|settings| settings.meeting_suggestions)
            .unwrap_or(true)
    }

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
        let paste_configured = settings
            .snapshot()
            .map(|value| value.paste_hotkey)
            .unwrap_or_else(|_| hotkey::DEFAULT_PASTE_HOTKEY.into());
        let mut paste_shortcut = hotkey::parse(&paste_configured)
            .or_else(|_| hotkey::parse(hotkey::DEFAULT_PASTE_HOTKEY))
            .expect("the built-in paste shortcut must be valid");
        if hotkey::shortcuts_conflict(&paste_shortcut, &active_shortcut) {
            paste_shortcut = hotkey::parse(hotkey::DEFAULT_PASTE_HOTKEY)
                .expect("the built-in paste shortcut must be valid");
        }
        if hotkey::shortcuts_conflict(&paste_shortcut, &active_shortcut) {
            paste_shortcut = hotkey::parse("control+super+KeyV")
                .expect("the fallback paste shortcut must be valid");
        }
        let paste_canonical = paste_shortcut.canonical().to_string();
        let search_configured = settings
            .snapshot()
            .map(|value| value.search_shortcut)
            .unwrap_or_else(|_| hotkey::DEFAULT_SEARCH_HOTKEY.into());
        let mut search_shortcut = hotkey::parse(&search_configured)
            .or_else(|_| hotkey::parse(hotkey::DEFAULT_SEARCH_HOTKEY))
            .expect("the built-in search shortcut must be valid");
        if hotkey::shortcuts_conflict(&search_shortcut, &active_shortcut)
            || hotkey::shortcuts_conflict(&search_shortcut, &paste_shortcut)
        {
            // Prefer keeping dictation/paste; fall back to a non-colliding chord.
            search_shortcut = hotkey::parse("control+super+Space")
                .expect("the fallback search shortcut must be valid");
            if hotkey::shortcuts_conflict(&search_shortcut, &active_shortcut)
                || hotkey::shortcuts_conflict(&search_shortcut, &paste_shortcut)
            {
                search_shortcut = hotkey::parse("alt+super+Space")
                    .expect("the secondary fallback search shortcut must be valid");
            }
        }
        let search_canonical = search_shortcut.canonical().to_string();
        if canonical != configured
            || paste_canonical != paste_configured
            || search_canonical != search_configured
        {
            if let Ok(mut repaired) = settings.snapshot() {
                repaired.hotkey = canonical;
                repaired.paste_hotkey = paste_canonical;
                repaired.search_shortcut = search_canonical;
                let _ = settings.replace(repaired);
            }
        }
        Self {
            pipeline: Mutex::new(Pipeline::default()),
            audio: AudioController::new(selected_microphone),
            system_audio: SystemAudioController::new(),
            meetings: meeting::MeetingController::new(),
            search: SearchController::new(),
            insertion_target: insert::InsertionTargetTracker::new(),
            sounds: SoundController::new(),
            engine: Mutex::new(None),
            search_http: search::shared_search_client().clone(),
            settings,
            target_window: Mutex::new(0),
            model_status: Mutex::new(ModelStatus {
                ready: false,
                message: "Starting local speech engine…".into(),
                backend: "NVIDIA Parakeet TDT 0.6B v3 · CUDA".into(),
            }),
            active_shortcut: Mutex::new(active_shortcut),
            paste_shortcut: Mutex::new(paste_shortcut),
            search_shortcut: Mutex::new(search_shortcut),
            hotkey_controller: Mutex::new(None),
            hotkey_error: Mutex::new(None),
            paste_hotkey_error: Mutex::new(None),
            search_hotkey_error: Mutex::new(None),
            show_microphone_once: Mutex::new(true),
            pending_uploads: Mutex::new(HashMap::new()),
            meeting_tray_item: Mutex::new(None),
            detector_control: Arc::new(meeting_detector::DetectorControl::new()),
            dictation_active: Arc::new(AtomicBool::new(false)),
            overlay_heartbeat: Mutex::new(None),
            search_blur_dismiss: AtomicBool::new(false),
        }
    }
}

pub(crate) fn complete_meeting_transcription(
    app: &AppHandle,
    result: Result<CompletedMeetingTranscription, String>,
) {
    let (id, transcript, notes, warning) = match result {
        Ok(completed) => (
            completed.id,
            completed.transcript,
            completed.notes,
            completed.warning,
        ),
        Err(error) => {
            let _ = app.emit("meeting-processing-error", error);
            return;
        }
    };
    match meeting::update_record(&id, transcript, notes, warning) {
        Ok(record) => {
            let _ = app.emit("meeting-updated", record);
        }
        Err(error) => {
            let _ = app.emit("meeting-processing-error", error);
        }
    }
}

pub(crate) fn fail_meeting_transcription(app: &AppHandle, id: &str, error: String) {
    match meeting::mark_error(id, error.clone()) {
        Ok(record) => {
            let _ = app.emit("meeting-updated", record);
        }
        Err(storage_error) => {
            let _ = app.emit(
                "meeting-processing-error",
                format!("{error}. Meeting status could not be saved: {storage_error}"),
            );
        }
    }
}

fn emit_status(app: &AppHandle, status: &EngineStatus) {
    let _ = app.emit("engine-status", status);
}

/// User-facing durations: milliseconds below one second, seconds at/above it.
pub(crate) fn format_duration(ms: u128) -> String {
    if ms < 1000 {
        return format!("{ms} ms");
    }
    let tenths = (ms + 50) / 100;
    let (seconds, tenth) = (tenths / 10, tenths % 10);
    if tenth == 0 {
        format!("{seconds} s")
    } else {
        format!("{seconds}.{tenth} s")
    }
}

fn restore_system_audio(app: &AppHandle) {
    let state = app.state::<AppState>();
    if let Err(error) = state.system_audio.restore() {
        let _ = app.emit("audio-warning", error);
    }
}

fn duck_system_audio_if_enabled(app: &AppHandle) {
    let state = app.state::<AppState>();
    let duck = state
        .settings
        .snapshot()
        .map(|settings| settings.duck_audio)
        .unwrap_or(false);
    if duck {
        if let Err(error) = state.system_audio.duck() {
            let _ = app.emit("audio-warning", error);
        }
    }
}

pub(crate) fn set_model_status(app: &AppHandle, status: ModelStatus) {
    if let Ok(mut current) = app.state::<AppState>().model_status.lock() {
        *current = status.clone();
    }
    let _ = app.emit("model-status", status);
}

fn begin_recording(app: &AppHandle) -> Result<EngineStatus, String> {
    let state = app.state::<AppState>();
    if state.search.is_busy() {
        return Err("Voice search is in progress. Finish or cancel it before dictating.".into());
    }
    if state.meetings.status()?.recording {
        return Err(
            "Meeting notes are being taken. Stop the meeting before starting dictation.".into(),
        );
    }
    let settings = state.settings.snapshot()?;
    let mut pipeline = state
        .pipeline
        .lock()
        .map_err(|_| "pipeline lock poisoned")?;
    if !pipeline.begin() {
        return Ok(pipeline.status.clone());
    }
    state.dictation_active.store(true, Ordering::Release);
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
    // Bluetooth headsets flip to hands-free mode when the mic opens, and
    // the switch gap swallows an immediate cue. On those routes the cue
    // plays late on the settled link instead (capture still starts
    // instantly, so no speech is lost); everywhere else it plays up front
    // on the stable route at full volume.
    let bluetooth_route =
        settings.dictation_sounds && system_audio::default_render_is_bluetooth();
    if settings.dictation_sounds && !bluetooth_route {
        // Ducking still happens afterwards so it never touches the cue.
        if let Err(error) = state.sounds.start_and_wait() {
            let _ = app.emit("audio-warning", error);
        }
    }
    // The prewarmed capture handle can go stale when Windows power-cycles
    // audio devices across sleep (or a USB/Bluetooth mic re-enumerates).
    // One reprepare-and-retry keeps a stale handle from killing dictation.
    let microphone = match state.audio.start() {
        Ok(name) => Ok(name),
        Err(first_error) => match state.audio.reprepare() {
            Ok(_) => state.audio.start(),
            Err(_) => Err(first_error),
        },
    };
    match microphone {
        Err(error) => {
            state.insertion_target.cancel();
            let _ = state.system_audio.restore();
            pipeline.fail(format!("Microphone unavailable: {error}"));
        }
        Ok(microphone_name) => {
            if bluetooth_route {
                let cue_app = app.clone();
                let duck_after_cue = settings.duck_audio;
                std::thread::Builder::new()
                    .name("pronto-start-cue".into())
                    .spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(700));
                        let listening = cue_app
                            .state::<AppState>()
                            .pipeline
                            .lock()
                            .map(|pipeline| pipeline.status.phase == Phase::Listening)
                            .unwrap_or(false);
                        // A tap shorter than the settle delay skips the cue:
                        // a start blip landing after dictation finished is
                        // worse than no blip at all.
                        if !listening {
                            return;
                        }
                        let state = cue_app.state::<AppState>();
                        if let Err(error) = state.sounds.start_and_wait() {
                            let _ = cue_app.emit("audio-warning", error);
                        }
                        if duck_after_cue {
                            if let Err(error) = state.system_audio.duck() {
                                let _ = cue_app.emit("audio-warning", error);
                            }
                        }
                    })
                    .ok();
            } else if settings.duck_audio {
                if let Err(error) = state.system_audio.duck() {
                    let _ = app.emit("audio-warning", error);
                }
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
            // Capture is already running, so a sick overlay page can reload
            // here without losing speech; the pill just appears a beat late.
            ensure_overlay_page(app);
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
    let settings = state.settings.snapshot()?;
    // Restore the endpoint before cueing: ducking lowers the master volume,
    // so a cue played first would be inaudible, and stopping the mic first
    // flips Bluetooth headsets back to music mode whose switch gap eats the
    // cue's attack. Restore, let the route settle, cue at full volume on
    // the stable route, and only then stop the microphone.
    let restore_result = state.system_audio.restore();
    if let Err(error) = restore_result {
        let _ = app.emit("audio-warning", error);
    }
    if settings.dictation_sounds {
        std::thread::sleep(std::time::Duration::from_millis(120));
        if let Err(error) = state.sounds.finish_and_wait() {
            let _ = app.emit("audio-warning", error);
        }
    }
    let recording = state.audio.stop();
    state.dictation_active.store(false, Ordering::Release);
    let recording = recording?;
    let target_window = *state
        .target_window
        .lock()
        .map_err(|_| "target window lock poisoned")?;
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
        engine.transcribe(TranscriptionJob::live(recording, settings, target_window))?;
    } else {
        pipeline.fail("Transcription engine is still starting");
        emit_status(app, &pipeline.status);
    }
    Ok(pipeline.status.clone())
}

fn position_overlay(window: &tauri::WebviewWindow, microphone_width: Option<f64>) {
    // Mic label floats 34px above the pill row at ~32px tall, so the
    // window needs 34 + 32 = 66px plus slack to avoid clipping its top.
    // The pill row itself sits 1px off the window bottom inside a 32px
    // window so fractional display scaling can't shave its bottom edge.
    let logical_height = if microphone_width.is_some() {
        72.0
    } else {
        32.0
    };
    let monitor = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        let logical_width = microphone_width.unwrap_or(170.0).max(170.0);
        let _ = window.set_size(LogicalSize::new(logical_width, logical_height));
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let max_logical_width = monitor.size().width as f64 / scale - 24.0;
    let logical_width = microphone_width
        .unwrap_or(170.0)
        .max(170.0)
        .min(max_logical_width.max(170.0));
    let _ = window.set_size(LogicalSize::new(logical_width, logical_height));
    let width = (logical_width * scale).round() as u32;
    let height = (logical_height * scale).round() as u32;
    let area = monitor.size();
    let origin = monitor.position();
    let x = origin.x + (area.width.saturating_sub(width) / 2) as i32;
    let y = origin.y + area.height.saturating_sub(height + 74) as i32;
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

/// How recently the dictation overlay page must have checked in (via the
/// `overlay_heartbeat` command) to be trusted to paint. The page beats on
/// load and every few seconds; sustained silence means its renderer is dead
/// or wedged, which surfaces as "dictation works but the pill never appears".
/// The TTL is generous because Chromium throttles timers in hidden pages
/// (down to ~1/min after minutes hidden), and this window idles hidden.
const OVERLAY_HEARTBEAT_TTL: Duration = Duration::from_secs(150);
/// Upper bound for waiting on a reloaded overlay page to check back in.
/// Capture is already running by then, so this only delays the pill.
const OVERLAY_RELOAD_WAIT: Duration = Duration::from_secs(2);

#[tauri::command]
fn overlay_heartbeat(app: AppHandle) {
    if let Ok(mut beat) = app.state::<AppState>().overlay_heartbeat.lock() {
        *beat = Some(Instant::now());
    }
}

fn overlay_beat_newer_than(app: &AppHandle, marker: Instant) -> bool {
    app.state::<AppState>()
        .overlay_heartbeat
        .lock()
        .map(|beat| beat.is_some_and(|seen| seen >= marker))
        .unwrap_or(false)
}

fn overlay_page_healthy(app: &AppHandle) -> bool {
    app.state::<AppState>()
        .overlay_heartbeat
        .lock()
        .map(|beat| {
            beat.is_some_and(|seen| seen.elapsed() < OVERLAY_HEARTBEAT_TTL)
        })
        .unwrap_or(false)
}

/// Reloads the overlay page when its renderer has gone quiet, then waits
/// briefly for the fresh page to check in. No-op on the healthy path, so
/// normal dictations pay nothing.
fn ensure_overlay_page(app: &AppHandle) {
    if overlay_page_healthy(app) {
        return;
    }
    let Some(window) = app.get_webview_window("overlay") else {
        return;
    };
    // Instant is monotonic, so a beat at or after this marker can only have
    // come from the reloaded page.
    let marker = Instant::now();
    if window.reload().is_err() {
        return;
    }
    let start = Instant::now();
    while start.elapsed() < OVERLAY_RELOAD_WAIT {
        if overlay_beat_newer_than(app, marker) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Recovery after sleep/hibernate. The overlay pages are hidden at idle, so
/// a lost compositor surface or dead renderer goes unnoticed until the next
/// dictation shows a blank pill: reload both pages while they are invisible
/// so they repaint fresh. A dictation (or search) left listening across
/// suspend can never complete meaningfully, so its resources are released
/// instead of leaving the pipeline wedged. Meetings are untouched.
pub(crate) fn handle_system_resume(app: &AppHandle) {
    let state = app.state::<AppState>();
    let dictation_stuck = state
        .pipeline
        .lock()
        .map(|pipeline| pipeline.status.phase == Phase::Listening)
        .unwrap_or(false);
    if dictation_stuck {
        let _ = state.audio.stop();
        state.insertion_target.cancel();
        state.dictation_active.store(false, Ordering::Release);
        let _ = state.system_audio.restore();
        if let Ok(mut pipeline) = state.pipeline.lock() {
            pipeline.reset();
            pipeline.status.message = "Dictation stopped during sleep".into();
            emit_status(app, &pipeline.status);
        }
        if let Some(overlay) = app.get_webview_window("overlay") {
            let _ = overlay.hide();
        }
    }
    if state.search.is_listening() {
        dismiss_search_overlay_inner(app);
    }
    restore_system_audio(app);
    // Hidden reloads: no visible effect, but a fresh page re-registers its
    // event listeners and repaints on next show.
    if let Some(overlay) = app.get_webview_window("overlay") {
        let _ = overlay.reload();
    }
    if let Some(search) = app.get_webview_window("search") {
        let _ = search.reload();
    }
    if dictation_stuck {
        let _ = app.emit(
            "tray-message",
            serde_json::json!({ "message": "Pronto recovered after sleep", "error": false }),
        );
    }
}

pub(crate) fn complete_transcription(
    app: &AppHandle,
    result: Result<CompletedTranscription, String>,
) {
    let state = app.state::<AppState>();
    // The pipeline lock is held only for the state flip itself. Insertion,
    // history disk writes, and the overlay pause below all run lock-free so
    // status reads and the next dictation never queue behind them.
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
            let message = match (
                completed.auto_insert,
                completed.cleanup_warning.as_ref(),
                insertion_error.as_ref(),
            ) {
                (_, _, Some(error)) => format!("Transcribed, but text insertion failed: {error}"),
                (true, Some(warning), None) => format!("Inserted with local cleanup · {warning}"),
                (true, None, None) => format!("Inserted in {}", format_duration(completed.entry.total_ms)),
                (false, Some(warning), None) => {
                    format!("File transcribed with local cleanup · {warning}")
                }
                (false, None, None) => {
                    format!("File transcribed in {}", format_duration(completed.entry.total_ms))
                }
            };
            let status = match state.pipeline.lock() {
                Ok(mut pipeline) => {
                    pipeline.complete(
                        completed.entry.final_text.clone(),
                        completed.entry.asr_ms,
                        completed.entry.cleanup_ms,
                        completed.entry.total_ms,
                        message,
                    );
                    pipeline.status.clone()
                }
                Err(_) => return,
            };
            if completed.skip_history {
                // Note Taker file uploads and other background imports must not
                // pollute the Dictation History clipboard. They are delivered
                // on a dedicated channel so the Note Taker can attach them.
                let _ = app.emit(
                    "notetaker-transcription",
                    serde_json::json!({
                        "uploadId": completed.upload_id,
                        "entry": completed.entry,
                    }),
                );
            } else {
                let _ = state.settings.push_history(completed.entry.clone());
                let _ = app.emit("history-updated", completed.entry);
            }
            emit_status(app, &status);
        }
        Err(error) => {
            state.insertion_target.cancel();
            let status = match state.pipeline.lock() {
                Ok(mut pipeline) => {
                    pipeline.fail(error);
                    pipeline.status.clone()
                }
                Err(_) => return,
            };
            emit_status(app, &status);
        }
    }
    // A transcription finishing in the background must not hide the overlay
    // while meeting notes are being recorded or offered.
    let meeting_active = app
        .state::<AppState>()
        .meetings
        .status()
        .map(|status| status.recording)
        .unwrap_or(false);
    if !meeting_active {
        if let Some(overlay) = app.get_webview_window("overlay") {
            // The result is already inserted and reported; this pause only
            // lets the pill play its ~130ms exit animation before hiding.
            // Off the engine worker so queued jobs don't wait on animation.
            std::thread::Builder::new()
                .name("pronto-overlay-hide".into())
                .spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(140));
                    let _ = overlay.hide();
                })
                .ok();
        }
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
fn start_meeting_recording(
    app: AppHandle,
    title: String,
) -> Result<meeting::MeetingRecord, String> {
    let state = app.state::<AppState>();
    if state.search.is_busy() {
        return Err(
            "Voice search is in progress. Finish or cancel it before taking meeting notes.".into(),
        );
    }
    {
        let pipeline = state
            .pipeline
            .lock()
            .map_err(|_| "pipeline lock poisoned")?;
        if pipeline.status.phase == Phase::Listening {
            // A live dictation is holding the microphone. Stop it so the
            // meeting can take over capture instead of forcing an app restart.
            drop(pipeline);
            let _ = state.audio.stop();
            state.insertion_target.cancel();
            let _ = state.system_audio.restore();
            let mut pipeline = state
                .pipeline
                .lock()
                .map_err(|_| "pipeline lock poisoned")?;
            pipeline.reset();
            pipeline.status.message = "Dictation stopped for meeting notes".into();
            emit_status(&app, &pipeline.status);
        }
        // A background transcription finishing up (Phase::Processing) no
        // longer blocks meeting capture; it completes into history on its own.
    }
    let settings = state.settings.snapshot()?;
    let record = state.meetings.start(title, settings.microphone_id)?;
    sync_meeting_tray_item(&app, true);
    let _ = app.emit(
        "meeting-status",
        serde_json::json!({ "recording": true, "meeting": record, "elapsedSeconds": 0 }),
    );
    Ok(record)
}

fn finish_meeting_recording(app: &AppHandle) -> Result<meeting::MeetingRecord, String> {
    let state = app.state::<AppState>();
    // stop() only halts the writers and marks the record processing, so
    // this returns in about a second even for hour-long meetings. Mixing
    // the two source files (minutes of disk IO) happens on a background
    // thread below; the meeting worker stays free for status/list.
    let stopped = state.meetings.stop()?;
    // Recording has ended regardless of what follows, so the tray goes
    // back to its idle label even on the error paths below.
    sync_meeting_tray_item(app, false);
    let _ = app.emit(
        "meeting-status",
        serde_json::json!({ "recording": false, "meeting": stopped.record, "elapsedSeconds": 0 }),
    );
    let record_id = stopped.record.id.clone();
    let record_title = stopped.record.title.clone();
    let handle = app.clone();
    std::thread::Builder::new()
        .name("pronto-meeting-finalize".into())
        .spawn(move || {
            let state = handle.state::<AppState>();
            let settings = match state.settings.snapshot() {
                Ok(settings) => settings,
                Err(error) => {
                    fail_meeting_transcription(&handle, &record_id, error);
                    return;
                }
            };
            let record = match meeting::finalize_meeting(&record_id) {
                Ok(record) => record,
                Err(error) => {
                    fail_meeting_transcription(&handle, &record_id, error);
                    return;
                }
            };
            let audio_path =
                std::path::PathBuf::from(record.audio_path.clone().unwrap_or_default());
            let enqueue = state
                .engine
                .lock()
                .map_err(|_| "engine lock poisoned".to_string())
                .and_then(|engine| {
                    engine
                        .as_ref()
                        .ok_or_else(|| "Transcription engine is still starting".to_string())
                        .and_then(|engine| {
                            engine.transcribe_meeting(MeetingTranscriptionJob {
                                id: record_id.clone(),
                                title: record_title.clone(),
                                audio_path,
                                settings,
                            })
                        })
                });
            if let Err(error) = enqueue {
                fail_meeting_transcription(&handle, &record_id, error);
            }
        })
        .map_err(|error| format!("Could not finalize meeting: {error}"))?;
    Ok(stopped.record)
}

#[tauri::command]
fn stop_meeting_recording(app: AppHandle) -> Result<meeting::MeetingRecord, String> {
    finish_meeting_recording(&app)
}

#[tauri::command]
fn get_meeting_status(state: tauri::State<'_, AppState>) -> Result<meeting::MeetingStatus, String> {
    state.meetings.status()
}

#[tauri::command]
fn get_meetings(state: tauri::State<'_, AppState>) -> Result<Vec<meeting::MeetingRecord>, String> {
    state.meetings.list()
}

const MAX_WAV_BYTES: usize = 180 * 1024 * 1024;

fn queue_file_import(
    app: &AppHandle,
    file_name: String,
    wav_bytes: &[u8],
    skip_history: bool,
    upload_id: Option<String>,
) -> Result<EngineStatus, String> {
    if wav_bytes.len() > MAX_WAV_BYTES {
        return Err(
            "This file is too long. Import a recording shorter than about 90 minutes.".into(),
        );
    }
    // Note Taker uploads persist their WAV to disk so a failed item ("Needs
    // attention") keeps its audio for retry even after restart.
    if skip_history {
        if let Some(id) = upload_id.as_deref() {
            let _ = meeting::save_notetaker_audio(id, wav_bytes);
        }
    }
    let recording = engine::recording_from_pcm16_wav(wav_bytes)?;
    let state = app.state::<AppState>();
    let settings = state.settings.snapshot()?;
    let mut pipeline = state
        .pipeline
        .lock()
        .map_err(|_| "pipeline lock poisoned")?;
    if !pipeline.import_processing(recording.samples.len(), recording.sample_rate) {
        return Err("Pronto is already recording or transcribing another item.".into());
    }
    pipeline.status.message = format!("Transcribing {file_name} locally…");
    emit_status(app, &pipeline.status);
    drop(pipeline);

    let engine = state.engine.lock().map_err(|_| "engine lock poisoned")?;
    let engine = engine
        .as_ref()
        .ok_or_else(|| "Transcription engine is still starting".to_string())?;
    engine.transcribe(TranscriptionJob::file_import(
        recording,
        UserSettings {
            auto_insert: false,
            // Local file imports never go through automatic cleanup, even
            // when "Clean up speech" is enabled for live dictation.
            // Note Taker transcripts stay verbatim until the user presses
            // "Clean Up Speech" for manual long-form cleanup.
            cleanup_enabled: false,
            ..settings
        },
        skip_history,
        upload_id,
    ))?;
    state.insertion_target.cancel();
    let status = state
        .pipeline
        .lock()
        .map_err(|_| "pipeline lock poisoned")?
        .status
        .clone();
    Ok(status)
}

#[tauri::command]
fn transcribe_media_file(
    app: AppHandle,
    file_name: String,
    wav_bytes: Vec<u8>,
    skip_history: Option<bool>,
    upload_id: Option<String>,
) -> Result<EngineStatus, String> {
    queue_file_import(
        &app,
        file_name,
        &wav_bytes,
        skip_history.unwrap_or(false),
        upload_id,
    )
}

#[tauri::command]
fn start_media_upload(
    state: tauri::State<'_, AppState>,
    upload_id: String,
    file_name: String,
    total_bytes: usize,
) -> Result<(), String> {
    if upload_id.trim().is_empty() || upload_id.len() > 128 {
        return Err("Invalid upload identifier".into());
    }
    if total_bytes == 0 || total_bytes > MAX_WAV_BYTES {
        return Err(
            "This file is too long. Import a recording shorter than about 90 minutes.".into(),
        );
    }
    let mut pending = state
        .pending_uploads
        .lock()
        .map_err(|_| "upload lock poisoned")?;
    pending.insert(
        upload_id,
        PendingUpload {
            file_name,
            bytes: Vec::new(),
        },
    );
    Ok(())
}

#[tauri::command]
fn append_media_chunk(
    state: tauri::State<'_, AppState>,
    upload_id: String,
    chunk: Vec<u8>,
) -> Result<usize, String> {
    let mut pending = state
        .pending_uploads
        .lock()
        .map_err(|_| "upload lock poisoned")?;
    let entry = pending
        .get_mut(&upload_id)
        .ok_or_else(|| "Upload session expired. Please try again.".to_string())?;
    if entry.bytes.len().saturating_add(chunk.len()) > MAX_WAV_BYTES {
        return Err(
            "This file is too long. Import a recording shorter than about 90 minutes.".into(),
        );
    }
    entry.bytes.extend_from_slice(&chunk);
    Ok(entry.bytes.len())
}

#[tauri::command]
fn abort_media_upload(state: tauri::State<'_, AppState>, upload_id: String) -> Result<(), String> {
    state
        .pending_uploads
        .lock()
        .map_err(|_| "upload lock poisoned")?
        .remove(&upload_id);
    Ok(())
}

#[tauri::command]
fn finish_media_upload(
    app: AppHandle,
    upload_id: String,
    skip_history: Option<bool>,
) -> Result<EngineStatus, String> {
    let (file_name, bytes) = {
        let state = app.state::<AppState>();
        let mut pending = state
            .pending_uploads
            .lock()
            .map_err(|_| "upload lock poisoned")?;
        let entry = pending.remove(&upload_id).ok_or_else(|| {
            "Upload session expired. Please try again.".to_string()
        })?;
        (entry.file_name, entry.bytes)
    };
    // queue_file_import re-validates size and decodes the WAV off the
    // small-chunk IPC path, so the webview never blocks on one giant payload.
    queue_file_import(
        &app,
        file_name,
        &bytes,
        skip_history.unwrap_or(false),
        Some(upload_id),
    )
}

#[tauri::command]
fn rename_meeting(
    _app: AppHandle,
    id: String,
    title: String,
) -> Result<meeting::MeetingRecord, String> {
    meeting::rename_record(&id, &title)
}

#[tauri::command]
fn delete_meeting(_app: AppHandle, id: String) -> Result<(), String> {
    meeting::delete_record(&id)
}

#[tauri::command]
fn retry_meeting(app: AppHandle, id: String) -> Result<meeting::MeetingRecord, String> {
    let state = app.state::<AppState>();
    let (record, audio_path) = meeting::record_for_retry(&id)?;
    let settings = state.settings.snapshot()?;
    let engine = state.engine.lock().map_err(|_| "engine lock poisoned")?;
    let engine = engine
        .as_ref()
        .ok_or_else(|| "Transcription engine is still starting".to_string())?;
    engine.transcribe_meeting(MeetingTranscriptionJob {
        id: record.id.clone(),
        title: record.title.clone(),
        audio_path,
        settings,
    })?;
    Ok(record)
}

#[tauri::command]
fn retry_notetaker_upload(app: AppHandle, item_id: String) -> Result<EngineStatus, String> {
    let path = meeting::notetaker_audio_path(&item_id)
        .ok_or_else(|| "That recording was not found.".to_string())?;
    let bytes = std::fs::read(&path).map_err(|_| {
        "The saved audio for this item is missing, so it cannot be retried.".to_string()
    })?;
    let file_name = format!("{item_id}.wav");
    queue_file_import(&app, file_name, &bytes, true, Some(item_id))
}

#[tauri::command]
fn delete_notetaker_audio(_app: AppHandle, item_id: String) -> Result<(), String> {
    meeting::delete_notetaker_audio(&item_id)
}

#[tauri::command]
fn cancel_recording(app: AppHandle) -> Result<EngineStatus, String> {
    let state = app.state::<AppState>();
    let _ = state.audio.stop();
    state.insertion_target.cancel();
    state.dictation_active.store(false, Ordering::Release);
    let _ = state.system_audio.restore();
    let status = {
        let mut pipeline = state
            .pipeline
            .lock()
            .map_err(|_| "pipeline lock poisoned")?;
        pipeline.reset();
        pipeline.status.message = "Dictation cancelled".into();
        pipeline.status.clone()
    };
    emit_status(&app, &status);
    if let Some(overlay) = app.get_webview_window("overlay") {
        // Lets the pill play its ~130ms exit animation before hiding, off
        // the command thread so cancel returns immediately.
        std::thread::Builder::new()
            .name("pronto-overlay-hide".into())
            .spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(140));
                let _ = overlay.hide();
            })
            .ok();
    }
    Ok(status)
}

fn handle_paste_hotkey(app: &AppHandle) {
    match paste_last(app) {
        Ok(message) => {
            let _ = app.emit(
                "tray-message",
                serde_json::json!({ "message": message, "error": false }),
            );
        }
        Err(message) => {
            let _ = app.emit(
                "tray-message",
                serde_json::json!({ "message": message, "error": true }),
            );
        }
    }
}

fn emit_search_status(app: &AppHandle, status: &SearchStatus) {
    let _ = app.emit("search-status", status);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchOverlayStage {
    Pill,
    Orb,
    Stage,
    Peek,
}

impl SearchOverlayStage {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "pill" => Ok(Self::Pill),
            "orb" => Ok(Self::Orb),
            "stage" => Ok(Self::Stage),
            "peek" => Ok(Self::Peek),
            other => Err(format!("Unknown search overlay stage: {other}")),
        }
    }
}

fn monitor_for(window: &tauri::WebviewWindow) -> Option<tauri::Monitor> {
    window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
}

/// Top edge (physical pixels, virtual-screen coords) of a bottom taskbar
/// that belongs to the given monitor, via SHAppBarMessage. This is the
/// only taskbar geometry query: the pill/overlay code elsewhere assumes a
/// bottom taskbar with a hardcoded 74px clearance instead.
fn bottom_taskbar_top(origin_x: i32, origin_y: i32, width: u32, height: u32) -> Option<i32> {
    use windows::Win32::UI::Shell::{ABM_GETTASKBARPOS, ABE_BOTTOM, APPBARDATA, SHAppBarMessage};
    let mut data: APPBARDATA = Default::default();
    data.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
    let ok = unsafe { SHAppBarMessage(ABM_GETTASKBARPOS, &mut data) };
    if ok == 0 || data.uEdge != ABE_BOTTOM {
        return None;
    }
    let rc = data.rc;
    if rc.left < origin_x
        || rc.right > origin_x + width as i32
        || rc.bottom != origin_y + height as i32
    {
        return None;
    }
    Some(rc.top)
}

fn apply_search_overlay_stage(window: &tauri::WebviewWindow, stage: SearchOverlayStage) {
    let Some(monitor) = monitor_for(window) else {
        let (w, h) = match stage {
            SearchOverlayStage::Pill => (120.0, 32.0),
            // Orb is retired: searching reuses the regular pill loading state.
            SearchOverlayStage::Orb => (120.0, 32.0),
            SearchOverlayStage::Stage => (920.0, 640.0),
            SearchOverlayStage::Peek => (280.0, 72.0),
        };
        let _ = window.set_size(LogicalSize::new(w, h));
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let area = monitor.size();
    let origin = monitor.position();

    match stage {
        SearchOverlayStage::Pill | SearchOverlayStage::Orb => {
            let logical_width = 120.0;
            let logical_height = 32.0;
            let _ = window.set_size(LogicalSize::new(logical_width, logical_height));
            let width = (logical_width * scale).round() as u32;
            let height = (logical_height * scale).round() as u32;
            let x = origin.x + (area.width.saturating_sub(width) / 2) as i32;
            let y = origin.y + area.height.saturating_sub(height + 74) as i32;
            let _ = window.set_position(PhysicalPosition::new(x, y));
        }
        SearchOverlayStage::Stage => {
            // Cover the monitor so the result panel can pop in centered and
            // the dimmed backdrop can catch outside clicks.
            let _ = window.set_size(tauri::Size::Physical(tauri::PhysicalSize::new(
                area.width,
                area.height,
            )));
            let _ = window.set_position(PhysicalPosition::new(origin.x, origin.y));
        }
        SearchOverlayStage::Peek => {
            // Folder-edge tab, bottom-left, tucked behind a bottom taskbar
            // so only a sliver peeks out. Falls back to the classic 74px
            // bottom clearance when the taskbar is elsewhere.
            let logical_width = 280.0;
            let logical_height = 72.0;
            let visible_height = 40.0;
            let _ = window.set_size(LogicalSize::new(logical_width, logical_height));
            let x = origin.x + (12.0 * scale).round() as i32;
            let y = match bottom_taskbar_top(origin.x, origin.y, area.width, area.height) {
                Some(taskbar_top) => taskbar_top - (visible_height * scale).round() as i32,
                None => {
                    let height = (logical_height * scale).round() as u32;
                    origin.y + area.height.saturating_sub(height + 74) as i32
                }
            };
            let _ = window.set_position(PhysicalPosition::new(x, y));
        }
    }
}

fn hide_search_overlay(app: &AppHandle) {
    let state = app.state::<AppState>();
    state.search_blur_dismiss.store(false, Ordering::Release);
    if let Some(window) = app.get_webview_window("search") {
        let _ = window.hide();
        apply_search_overlay_stage(&window, SearchOverlayStage::Pill);
    }
}

fn show_search_overlay(app: &AppHandle, stage: SearchOverlayStage, focus: bool) {
    let state = app.state::<AppState>();
    // Disable blur-dismiss around show/resize so transient focus churn from
    // set_size / set_position cannot cancel an in-flight search.
    state.search_blur_dismiss.store(false, Ordering::Release);
    if let Some(window) = app.get_webview_window("search") {
        apply_search_overlay_stage(&window, stage);
        let _ = window.show();
        if focus {
            let _ = window.set_focus();
            state.search_blur_dismiss.store(true, Ordering::Release);
        }
    }
}

fn dismiss_search_overlay_inner(app: &AppHandle) {
    let state = app.state::<AppState>();
    state.search_blur_dismiss.store(false, Ordering::Release);
    let phase = state.search.status().map(|status| status.phase).ok();
    if matches!(
        phase,
        Some(SearchPhase::Listening | SearchPhase::Searching)
    ) {
        restore_system_audio(app);
        let _ = state.audio.stop();
        if let Ok(status) = state.search.reset() {
            let mut cancelled = status;
            cancelled.message = "Search dismissed".into();
            emit_search_status(app, &cancelled);
        }
    }
    hide_search_overlay(app);
}

/// Click-away from a finished result parks the panel as a small peek tab
/// at the bottom-left instead of hiding it. Anything else (listening,
/// searching, idle, error) hides the overlay as before. Returns true when
/// the result was parked.
fn park_search_overlay_inner(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    state.search_blur_dismiss.store(false, Ordering::Release);
    let phase = state.search.status().map(|status| status.phase).ok();
    if !matches!(phase, Some(SearchPhase::Complete)) {
        dismiss_search_overlay_inner(app);
        return false;
    }
    if let Some(window) = app.get_webview_window("search") {
        apply_search_overlay_stage(&window, SearchOverlayStage::Peek);
        let _ = window.show();
        // Deliberately unfocused with blur-dismiss off: the tab lingers
        // until picked, dismissed, or timed out by the frontend.
    }
    let _ = app.emit("search-parked", ());
    true
}

fn search_blocked_reason(state: &AppState) -> Option<String> {
    if state
        .pipeline
        .lock()
        .map(|pipeline| matches!(pipeline.status.phase, Phase::Listening | Phase::Processing))
        .unwrap_or(false)
        || state.dictation_active.load(Ordering::Acquire)
    {
        return Some("Dictation is in progress. Finish it before starting voice search.".into());
    }
    if state
        .meetings
        .status()
        .map(|status| status.recording)
        .unwrap_or(false)
    {
        return Some("Meeting notes are being taken. Stop the meeting before voice search.".into());
    }
    None
}

/// Minimum listen time before Hold-release / Toggle-stop is accepted.
/// Win+Space layout switching often synthesizes an immediate key-up; ignoring
/// that bounce keeps hold-to-talk usable. A deferred finish still runs.
const SEARCH_MIN_LISTEN_MS: u128 = 280;
/// Hard cap so a missed key-up cannot leave search listening forever.
const SEARCH_MAX_LISTEN_SECS: u64 = 8;

fn arm_search_listen_watchdog(app: &AppHandle, generation: u64) {
    let app = app.clone();
    std::thread::Builder::new()
        .name("pronto-search-watchdog".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(SEARCH_MAX_LISTEN_SECS));
            if app
                .state::<AppState>()
                .search
                .is_listening_generation(generation)
            {
                let _ = finish_search_recording_inner(&app, true);
            }
        })
        .ok();
}

fn arm_deferred_search_finish(app: &AppHandle, generation: u64, delay_ms: u128) {
    let app = app.clone();
    std::thread::Builder::new()
        .name("pronto-search-defer-finish".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms as u64));
            if app
                .state::<AppState>()
                .search
                .is_listening_generation(generation)
            {
                let _ = finish_search_recording_inner(&app, true);
            }
        })
        .ok();
}

fn begin_search_recording_inner(app: &AppHandle) -> Result<SearchStatus, String> {
    let state = app.state::<AppState>();
    if let Some(reason) = search_blocked_reason(&state) {
        let _ = app.emit(
            "tray-message",
            serde_json::json!({ "message": reason, "error": true }),
        );
        return Err(reason);
    }
    let hold_mode = state
        .settings
        .snapshot()
        .map(|settings| settings.activation_mode == ActivationMode::Hold)
        .unwrap_or(true);
    let (status, generation) = state.search.begin_listening(hold_mode)?;
    if status.phase != SearchPhase::Listening {
        return Ok(status);
    }
    match state.audio.start() {
        Ok(_) => {
            duck_system_audio_if_enabled(app);
            if let Ok(engine) = state.engine.lock() {
                if let Some(engine) = engine.as_ref() {
                    engine.warm();
                }
            }
            // Show without stealing focus — focusing mid-chord desyncs the
            // WH_KEYBOARD_LL pressed-set for Win+Space and breaks Hold release.
            show_search_overlay(app, SearchOverlayStage::Pill, false);
            emit_search_status(app, &status);
            arm_search_listen_watchdog(app, generation);
            Ok(status)
        }
        Err(error) => {
            let failed = state
                .search
                .fail(format!("Microphone unavailable: {error}"))?;
            emit_search_status(app, &failed);
            Err(failed.message)
        }
    }
}

fn finish_search_recording_inner(app: &AppHandle, force: bool) -> Result<SearchStatus, String> {
    let state = app.state::<AppState>();
    let current = state.search.status()?;
    if current.phase != SearchPhase::Listening {
        return Ok(current);
    }
    let generation = state.search.current_listen_generation();
    let elapsed = state.search.listen_elapsed_ms();
    if !force && elapsed < SEARCH_MIN_LISTEN_MS {
        // Bounce release from Win+Space layout switching — finish shortly if
        // we are still listening (keys are typically already up).
        arm_deferred_search_finish(app, generation, SEARCH_MIN_LISTEN_MS.saturating_sub(elapsed));
        return Ok(current);
    }

    let settings = state.settings.snapshot()?;
    restore_system_audio(app);
    let recording = match state.audio.stop() {
        Ok(recording) => recording,
        Err(error) => {
            let failed = state
                .search
                .fail(format!("Could not stop microphone: {error}"))?;
            emit_search_status(app, &failed);
            let _ = app.emit("search-error", failed.message.clone());
            return Err(failed.message);
        }
    };
    let status = state.search.mark_transcribing()?;
    if status.phase != SearchPhase::Searching {
        return Ok(status);
    }
    emit_search_status(app, &status);
    // Stay on the regular pill loading state while transcribing — the result
    // panel pops only once grounded content is ready. No focus steal.
    show_search_overlay(app, SearchOverlayStage::Pill, false);
    let engine = state.engine.lock().map_err(|_| "engine lock poisoned")?;
    if let Some(engine) = engine.as_ref() {
        if let Err(error) = engine.transcribe_search(SearchAsrJob {
            recording,
            language: settings.language.clone(),
            dictionary: settings.dictionary.clone(),
            provider_url: settings.search_provider_url.clone(),
        }) {
            let failed = state.search.fail(error)?;
            emit_search_status(app, &failed);
            let _ = app.emit("search-error", failed.message.clone());
            return Ok(failed);
        }
    } else {
        let failed = state
            .search
            .fail("Transcription engine is still starting")?;
        emit_search_status(app, &failed);
        let _ = app.emit("search-error", failed.message.clone());
        return Ok(failed);
    }
    Ok(status)
}

fn reroute_dictation_to_search(app: &AppHandle) -> Result<SearchStatus, String> {
    let state = app.state::<AppState>();
    if state.search.is_busy() {
        return Err("Voice search is already in progress.".into());
    }
    let listening = state
        .pipeline
        .lock()
        .map(|pipeline| pipeline.status.phase == Phase::Listening)
        .unwrap_or(false);
    if !listening {
        return Err("Start dictating before routing to search.".into());
    }

    let settings = state.settings.snapshot()?;
    let restore_result = state.system_audio.restore();
    if let Err(error) = restore_result {
        let _ = app.emit("audio-warning", error);
    }
    if settings.dictation_sounds {
        std::thread::sleep(std::time::Duration::from_millis(120));
        if let Err(error) = state.sounds.finish_and_wait() {
            let _ = app.emit("audio-warning", error);
        }
    }
    let recording = state.audio.stop()?;
    state.insertion_target.cancel();
    state.dictation_active.store(false, Ordering::Release);
    {
        let mut pipeline = state
            .pipeline
            .lock()
            .map_err(|_| "pipeline lock poisoned")?;
        pipeline.reset();
        emit_status(app, &pipeline.status);
    }
    if let Some(overlay) = app.get_webview_window("overlay") {
        let _ = overlay.hide();
    }

    let status = state.search.begin_transcribing_imported()?;
    emit_search_status(app, &status);
    show_search_overlay(app, SearchOverlayStage::Pill, false);
    let engine = state.engine.lock().map_err(|_| "engine lock poisoned")?;
    if let Some(engine) = engine.as_ref() {
        if let Err(error) = engine.transcribe_search(SearchAsrJob {
            recording,
            language: settings.language.clone(),
            dictionary: settings.dictionary.clone(),
            provider_url: settings.search_provider_url.clone(),
        }) {
            let failed = state.search.fail(error)?;
            emit_search_status(app, &failed);
            let _ = app.emit("search-error", failed.message.clone());
            return Ok(failed);
        }
    } else {
        let failed = state
            .search
            .fail("Transcription engine is still starting")?;
        emit_search_status(app, &failed);
        let _ = app.emit("search-error", failed.message.clone());
        return Ok(failed);
    }
    Ok(status)
}

fn cancel_search_inner(app: &AppHandle) -> Result<SearchStatus, String> {
    let state = app.state::<AppState>();
    restore_system_audio(app);
    let _ = state.audio.stop();
    let status = state.search.reset()?;
    let mut cancelled = status;
    cancelled.message = "Search cancelled".into();
    emit_search_status(app, &cancelled);
    hide_search_overlay(app);
    Ok(cancelled)
}

/// "Ask next" follow-up chip: run a new search for literal text, skipping
/// microphone capture and going straight to web retrieval + synthesis.
fn run_text_search_inner(app: &AppHandle, query: String) -> Result<SearchStatus, String> {
    let query = query.trim().to_string();
    if query.is_empty() {
        return Err("Empty follow-up question".into());
    }
    let state = app.state::<AppState>();
    if let Some(reason) = search_blocked_reason(&state) {
        let _ = app.emit(
            "tray-message",
            serde_json::json!({ "message": reason, "error": true }),
        );
        return Err(reason);
    }
    let settings = state.settings.snapshot()?;
    // Session context, same as voice results: "what about tomorrow?"
    // expands against the last queries with no extra LLM cost.
    let recent = state.search.recent_queries();
    let expanded = search::expand_followup(&query, &recent);
    let status = state.search.begin_text_search(expanded.clone())?;
    emit_search_status(app, &status);
    let _ = app.emit(
        "search-query",
        serde_json::json!({ "query": expanded }),
    );
    show_search_overlay(app, SearchOverlayStage::Pill, false);
    let completed = CompletedSearchAsr {
        query: expanded,
        provider_url: settings.search_provider_url.clone(),
    };
    let app_handle = app.clone();
    let resource_dir = state.search.resource_dir();
    std::thread::Builder::new()
        .name("pronto-search-web".into())
        .spawn(move || {
            run_web_search_and_synthesize(&app_handle, completed, resource_dir);
        })
        .map_err(|error| format!("Could not start web search: {error}"))?;
    Ok(status)
}

pub(crate) fn complete_search_asr(app: &AppHandle, result: Result<CompletedSearchAsr, String>) {
    let state = app.state::<AppState>();
    match result {
        Ok(mut completed) => {
            // Session context: expand "what about tomorrow?" using last queries.
            // Local rule-based, no extra LLM cost; expanded query is shown in UI.
            let recent = state.search.recent_queries();
            let expanded = search::expand_followup(&completed.query, &recent);
            if expanded != completed.query {
                completed.query = expanded;
            }
            let status = match state.search.mark_searching(Some(completed.query.clone())) {
                Ok(status) => status,
                Err(_) => return,
            };
            emit_search_status(app, &status);
            // Surface the recognized query immediately so the wait feels shorter.
            let _ = app.emit(
                "search-query",
                serde_json::json!({ "query": completed.query }),
            );
            let app_handle = app.clone();
            let resource_dir = state.search.resource_dir();
            std::thread::Builder::new()
                .name("pronto-search-web".into())
                .spawn(move || {
                    run_web_search_and_synthesize(&app_handle, completed, resource_dir);
                })
                .ok();
        }
        Err(error) => {
            if let Ok(status) = state.search.fail(error.clone()) {
                emit_search_status(app, &status);
            }
            let _ = app.emit("search-error", error);
            show_search_overlay(app, SearchOverlayStage::Stage, true);
        }
    }
}

fn run_web_search_and_synthesize(
    app: &AppHandle,
    completed: CompletedSearchAsr,
    _resource_dir: Option<std::path::PathBuf>,
) {
    let state = app.state::<AppState>();
    let retrieval_started = std::time::Instant::now();
    let normalized = search::normalize_query(&completed.query);
    let cache_key = if normalized.is_empty() {
        completed.query.clone()
    } else {
        normalized.clone()
    };
    let needs_web = search::needs_web_retrieval(&completed.query);
    let time_sensitive = search::is_time_sensitive(&completed.query);
    let mut cache_hit = false;
    let mut hits: Vec<search::SearchHit> = Vec::new();
    let mut retrieved_from_network = false;
    if needs_web {
        hits = if !time_sensitive {
            search::cached_hits_for(&cache_key).map(|cached| {
                cache_hit = true;
                cached
            }).unwrap_or_default()
        } else {
            Vec::new()
        };
        retrieved_from_network = cache_hit && !hits.is_empty();
        if hits.is_empty() {
            retrieved_from_network = false;
            cache_hit = false;
            let provider = DuckDuckGoProvider::with_client(
                completed.provider_url,
                state.search_http.clone(),
            );
            hits = match provider.search(&completed.query) {
                Ok(hits) => hits,
                Err(error) => {
                    if let Ok(status) = state.search.fail(error.clone()) {
                        emit_search_status(app, &status);
                    }
                    let _ = app.emit("search-error", error);
                    show_search_overlay(app, SearchOverlayStage::Stage, true);
                    return;
                }
            };
            search::rerank_hits(&completed.query, &mut hits);
            let kind = search::classify_query(&completed.query);
            let top_k = match kind {
                search::QueryKind::Fast => 3,
                search::QueryKind::Grounded => 4,
            };
            hits = search::truncate_hits(hits, top_k);
            if !time_sensitive && !hits.is_empty() {
                search::store_hits_cache(cache_key, hits.clone());
            }
        }
    }
    let retrieval_ms = retrieval_started.elapsed().as_millis();
    // Thumbs are instant; DDG photos fetch in parallel with LLM synthesis.
    let thumbs = search::photo_candidates_for_hits(&hits);
    state.search.remember_allowed_urls(
        search::expanded_allowed_urls_with_images(&hits, &thumbs),
    );

    if let Ok(status) = state.search.mark_synthesizing(completed.query.clone()) {
        emit_search_status(app, &status);
    }

    let http = state.search_http.clone();
    let query_for_images = completed.query.clone();
    let hits_for_images = hits.clone();
    let banner_handle = std::thread::Builder::new()
        .name("pronto-search-banner-img".into())
        .spawn(move || {
            search::resolve_banner_images(&http, &query_for_images, &hits_for_images, 3)
        })
        .ok();

    let client = state.search_http.clone();
    let synthesis_started = std::time::Instant::now();
    let grounded = needs_web && !hits.is_empty();
    let synth_result = search::synthesize_search_markdown(
        &client,
        &completed.query,
        &hits,
        grounded,
    );

    let (parsed, mut warning) = match synth_result {
        Ok(result) => result,
        Err(error) => {
            if let Ok(status) = state.search.fail(error.clone()) {
                emit_search_status(app, &status);
            }
            let _ = app.emit("search-error", error);
            return;
        }
    };
    let synthesis_ms = synthesis_started.elapsed().as_millis();
    // Surface cache + timing in warning when otherwise silent (observability
    // without extra IPC): keeps fast-path transparent.
    if warning.is_none() && (cache_hit || retrieval_ms > 0) {
        let mode = if cache_hit { "cached" } else { "live" };
        // Only annotate fast cached answers to avoid noise on grounded cards.
        if cache_hit && search::classify_query(&completed.query) == search::QueryKind::Fast {
            warning = Some(format!("Instant answer ({mode}, retrieval {})", format_duration(retrieval_ms)));
        } else {
            let _ = (retrieval_ms, synthesis_ms, retrieved_from_network);
        }
    }
    // First paint carries text + sources immediately; the banner image
    // resolves on a side thread below so a slow image host never delays
    // the answer. Final content is identical, just staged.
    let payload = SearchResultPayload {
        query: completed.query.clone(),
        markdown: parsed.markdown,
        layout: parsed.layout,
        key_facts: parsed.key_facts,
        followups: parsed.followups,
        banner_image: None,
        sources: hits.clone(),
        warning: warning.clone(),
    };
    if let Ok(status) = state.search.complete(completed.query.clone(), warning) {
        emit_search_status(app, &status);
    }
    let _ = app.emit("search-result", payload);
    show_search_overlay(app, SearchOverlayStage::Stage, true);
    let banner_app = app.clone();
    let banner_http = state.search_http.clone();
    let banner_query = completed.query.clone();
    std::thread::Builder::new()
        .name("pronto-search-banner-embed".into())
        .spawn(move || {
            let images = banner_handle
                .and_then(|handle| handle.join().ok())
                .map(|resolved| search::merge_banner_images(thumbs.clone(), resolved, 3))
                .unwrap_or(thumbs);
            banner_app
                .state::<AppState>()
                .search
                .remember_allowed_urls(search::expanded_allowed_urls_with_images(&hits, &images));
            if let Some(banner) = search::build_banner_image(&banner_http, &images) {
                let _ = banner_app.emit(
                    "search-banner-ready",
                    serde_json::json!({ "query": banner_query, "bannerImage": banner }),
                );
            }
        })
        .ok();
}

fn handle_search_hotkey(app: &AppHandle, event: HotkeyEvent) {
    // Mirror dictation: Hold uses press/release; Toggle uses press edges only.
    let mode = app
        .state::<AppState>()
        .settings
        .snapshot()
        .map(|settings| settings.activation_mode)
        .unwrap_or_default();
    match (mode, event) {
        (ActivationMode::Hold, HotkeyEvent::Pressed) => {
            let _ = begin_search_recording_inner(app);
        }
        (ActivationMode::Hold, HotkeyEvent::Released) => {
            if let Err(error) = finish_search_recording_inner(app, false) {
                let _ = app.emit(
                    "tray-message",
                    serde_json::json!({ "message": error, "error": true }),
                );
            }
        }
        (ActivationMode::Toggle, HotkeyEvent::Pressed) => {
            let listening = app.state::<AppState>().search.is_listening();
            if listening {
                // Ignore chord bounce right after start (common with Win+Space).
                if app.state::<AppState>().search.listen_elapsed_ms() < SEARCH_MIN_LISTEN_MS {
                    return;
                }
                if let Err(error) = finish_search_recording_inner(app, false) {
                    let _ = app.emit(
                        "tray-message",
                        serde_json::json!({ "message": error, "error": true }),
                    );
                }
            } else {
                let phase = app
                    .state::<AppState>()
                    .search
                    .status()
                    .ok()
                    .map(|status| status.phase);
                if matches!(
                    phase,
                    Some(SearchPhase::Idle | SearchPhase::Complete | SearchPhase::Error) | None
                ) {
                    let _ = begin_search_recording_inner(app);
                }
            }
        }
        (ActivationMode::Toggle, HotkeyEvent::Released) => {}
    }
}

fn handle_hotkey_event(app: &AppHandle, id: HotkeyId, event: HotkeyEvent) {
    if id == HotkeyId::Paste {
        if event == HotkeyEvent::Pressed {
            handle_paste_hotkey(app);
        }
        return;
    }
    if id == HotkeyId::Search {
        handle_search_hotkey(app, event);
        return;
    }
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
    state.dictation_active.store(false, Ordering::Release);
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
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    mut settings: UserSettings,
) -> Result<AppPreferences, String> {
    // Shortcut changes are transactional through set_hotkey so a settings
    // write can never persist a shortcut that Windows rejected.
    let previous = state.settings.snapshot()?;
    let theme_changed = settings.theme != previous.theme;
    let gpu_memory_management_changed =
        settings.gpu_memory_management != previous.gpu_memory_management;
    settings.hotkey = previous.hotkey;
    settings.paste_hotkey = previous.paste_hotkey;
    settings.search_shortcut = previous.search_shortcut;
    settings.microphone_id = previous.microphone_id;
    settings.microphone_name = previous.microphone_name;
    settings.gpu_memory_management_configured = true;
    if settings.launch_at_startup != previous.launch_at_startup {
        startup::set_enabled(settings.launch_at_startup)?;
    }
    match state.settings.replace(settings) {
        Ok(preferences) => {
            if theme_changed {
                let theme = match preferences.settings.theme {
                    settings::ThemeMode::Light => "light",
                    settings::ThemeMode::Dark => "dark",
                    settings::ThemeMode::System => "system",
                };
                let _ = app.emit("theme-changed", theme);
            }
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
fn resize_overlay(app: AppHandle, width: f64, height: f64) -> Result<(), String> {
    if !width.is_finite()
        || !height.is_finite()
        || !(100.0..=420.0).contains(&width)
        || !(30.0..=180.0).contains(&height)
    {
        return Err("Invalid overlay size".into());
    }
    let overlay = app
        .get_webview_window("overlay")
        .ok_or_else(|| "Pronto overlay is unavailable".to_string())?;
    overlay
        .set_size(LogicalSize::new(width, height))
        .map_err(|e| e.to_string())?;
    position_overlay_custom(&overlay, width, height);
    Ok(())
}

fn position_overlay_custom(window: &tauri::WebviewWindow, width: f64, height: f64) {
    let Some(monitor) = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
    else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let size = monitor.size();
    let origin = monitor.position();
    let physical_width = (width * scale).round() as u32;
    let physical_height = (height * scale).round() as u32;
    let x = origin.x + (size.width.saturating_sub(physical_width) / 2) as i32;
    let y = origin.y + size.height.saturating_sub(physical_height + 74) as i32;
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

#[tauri::command]
fn dismiss_meeting_prompt(app: AppHandle) -> Result<(), String> {
    if let Some(overlay) = app.get_webview_window("overlay") {
        overlay.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn dismiss_meeting_suggestion(state: tauri::State<'_, AppState>) -> Result<(), String> {
    // Suppress re-prompts for the meeting session that is currently
    // visible. A new session (meeting goes away and returns) starts a new
    // generation and prompts again.
    state.detector_control.dismiss_current();
    Ok(())
}

fn hotkey_status(state: &AppState) -> Result<HotkeyStatus, String> {
    let shortcut = state
        .active_shortcut
        .lock()
        .map_err(|_| "shortcut lock poisoned")?
        .clone();
    let paste_shortcut = state
        .paste_shortcut
        .lock()
        .map_err(|_| "shortcut lock poisoned")?
        .clone();
    let search_shortcut = state
        .search_shortcut
        .lock()
        .map_err(|_| "shortcut lock poisoned")?
        .clone();
    let error = state
        .hotkey_error
        .lock()
        .map_err(|_| "shortcut status lock poisoned")?
        .clone();
    let paste_error = state
        .paste_hotkey_error
        .lock()
        .map_err(|_| "shortcut status lock poisoned")?
        .clone();
    let search_error = state
        .search_hotkey_error
        .lock()
        .map_err(|_| "shortcut status lock poisoned")?
        .clone();
    Ok(HotkeyStatus {
        shortcut: shortcut.canonical().to_string(),
        paste_shortcut: paste_shortcut.canonical().to_string(),
        search_shortcut: search_shortcut.canonical().to_string(),
        registered: state
            .hotkey_controller
            .lock()
            .map(|value| value.is_some())
            .unwrap_or(false),
        error,
        paste_error,
        search_error,
    })
}

#[tauri::command]
fn get_hotkey_status(state: tauri::State<'_, AppState>) -> Result<HotkeyStatus, String> {
    hotkey_status(&state)
}

#[tauri::command]
fn set_hotkey(app: AppHandle, hotkey: String) -> Result<HotkeyStatus, String> {
    let next = hotkey::parse(&hotkey)?;
    let canonical = next.canonical().to_string();
    let state = app.state::<AppState>();
    {
        let paste = state
            .paste_shortcut
            .lock()
            .map_err(|_| "shortcut lock poisoned")?;
        if hotkey::shortcuts_conflict(&next, &paste) {
            return Err(
                "That shortcut is already used for pasting the last transcript".into(),
            );
        }
        let search = state
            .search_shortcut
            .lock()
            .map_err(|_| "shortcut lock poisoned")?;
        if hotkey::shortcuts_conflict(&next, &search) {
            return Err("That shortcut is already used for voice search".into());
        }
    }
    let previous = state
        .active_shortcut
        .lock()
        .map_err(|_| "shortcut lock poisoned")?
        .clone();
    {
        let controller = state
            .hotkey_controller
            .lock()
            .map_err(|_| "shortcut controller lock poisoned")?;
        let controller = controller
            .as_ref()
            .ok_or_else(|| "Shortcut listener is still starting".to_string())?;
        controller.update(HotkeyId::Dictation, next.clone())?;
    }

    let mut settings = state.settings.snapshot()?;
    settings.hotkey = canonical;
    if let Err(error) = state.settings.replace(settings) {
        if let Ok(controller) = state.hotkey_controller.lock() {
            if let Some(controller) = controller.as_ref() {
                let _ = controller.update(HotkeyId::Dictation, previous);
            }
        }
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
    let status = hotkey_status(&state)?;
    let _ = app.emit("hotkey-status", status.clone());
    Ok(status)
}

#[tauri::command]
fn set_paste_hotkey(app: AppHandle, hotkey: String) -> Result<HotkeyStatus, String> {
    let next = hotkey::parse(&hotkey)?;
    let canonical = next.canonical().to_string();
    let state = app.state::<AppState>();
    {
        let active = state
            .active_shortcut
            .lock()
            .map_err(|_| "shortcut lock poisoned")?;
        if hotkey::shortcuts_conflict(&next, &active) {
            return Err("That shortcut is already used for dictation".into());
        }
        let search = state
            .search_shortcut
            .lock()
            .map_err(|_| "shortcut lock poisoned")?;
        if hotkey::shortcuts_conflict(&next, &search) {
            return Err("That shortcut is already used for voice search".into());
        }
    }
    let previous = state
        .paste_shortcut
        .lock()
        .map_err(|_| "shortcut lock poisoned")?
        .clone();
    {
        let controller = state
            .hotkey_controller
            .lock()
            .map_err(|_| "shortcut controller lock poisoned")?;
        let controller = controller
            .as_ref()
            .ok_or_else(|| "Shortcut listener is still starting".to_string())?;
        controller.update(HotkeyId::Paste, next.clone())?;
    }

    let mut settings = state.settings.snapshot()?;
    settings.paste_hotkey = canonical;
    if let Err(error) = state.settings.replace(settings) {
        if let Ok(controller) = state.hotkey_controller.lock() {
            if let Some(controller) = controller.as_ref() {
                let _ = controller.update(HotkeyId::Paste, previous);
            }
        }
        return Err(format!(
            "The shortcut worked but could not be saved: {error}"
        ));
    }
    *state
        .paste_shortcut
        .lock()
        .map_err(|_| "shortcut lock poisoned")? = next.clone();
    *state
        .paste_hotkey_error
        .lock()
        .map_err(|_| "shortcut status lock poisoned")? = None;
    let status = hotkey_status(&state)?;
    let _ = app.emit("hotkey-status", status.clone());
    Ok(status)
}

#[tauri::command]
fn set_search_hotkey(app: AppHandle, hotkey: String) -> Result<HotkeyStatus, String> {
    let next = hotkey::parse(&hotkey)?;
    let canonical = next.canonical().to_string();
    let state = app.state::<AppState>();
    {
        let active = state
            .active_shortcut
            .lock()
            .map_err(|_| "shortcut lock poisoned")?;
        if hotkey::shortcuts_conflict(&next, &active) {
            return Err("That shortcut is already used for dictation".into());
        }
        let paste = state
            .paste_shortcut
            .lock()
            .map_err(|_| "shortcut lock poisoned")?;
        if hotkey::shortcuts_conflict(&next, &paste) {
            return Err(
                "That shortcut is already used for pasting the last transcript".into(),
            );
        }
    }
    let previous = state
        .search_shortcut
        .lock()
        .map_err(|_| "shortcut lock poisoned")?
        .clone();
    {
        let controller = state
            .hotkey_controller
            .lock()
            .map_err(|_| "shortcut controller lock poisoned")?;
        let controller = controller
            .as_ref()
            .ok_or_else(|| "Shortcut listener is still starting".to_string())?;
        controller.update(HotkeyId::Search, next.clone())?;
    }

    let mut settings = state.settings.snapshot()?;
    settings.search_shortcut = canonical;
    if let Err(error) = state.settings.replace(settings) {
        if let Ok(controller) = state.hotkey_controller.lock() {
            if let Some(controller) = controller.as_ref() {
                let _ = controller.update(HotkeyId::Search, previous);
            }
        }
        return Err(format!(
            "The shortcut worked but could not be saved: {error}"
        ));
    }
    *state
        .search_shortcut
        .lock()
        .map_err(|_| "shortcut lock poisoned")? = next.clone();
    *state
        .search_hotkey_error
        .lock()
        .map_err(|_| "shortcut status lock poisoned")? = None;
    let status = hotkey_status(&state)?;
    let _ = app.emit("hotkey-status", status.clone());
    Ok(status)
}

#[tauri::command]
fn get_search_hotkey(state: tauri::State<'_, AppState>) -> Result<String, String> {
    state
        .search_shortcut
        .lock()
        .map(|shortcut| shortcut.canonical().to_string())
        .map_err(|_| "shortcut lock poisoned".into())
}

#[tauri::command]
fn start_search_recording(app: AppHandle) -> Result<SearchStatus, String> {
    begin_search_recording_inner(&app)
}

#[tauri::command]
fn stop_search_recording(app: AppHandle) -> Result<SearchStatus, String> {
    finish_search_recording_inner(&app, true)
}

#[tauri::command]
fn cancel_search(app: AppHandle) -> Result<SearchStatus, String> {
    cancel_search_inner(&app)
}

#[tauri::command]
fn run_text_search(app: AppHandle, query: String) -> Result<SearchStatus, String> {
    run_text_search_inner(&app, query)
}

#[tauri::command]
fn reroute_dictation_to_search_command(app: AppHandle) -> Result<SearchStatus, String> {
    reroute_dictation_to_search(&app)
}

#[tauri::command]
fn dismiss_search_overlay(app: AppHandle) -> Result<(), String> {
    dismiss_search_overlay_inner(&app);
    Ok(())
}

#[tauri::command]
fn park_search_overlay(app: AppHandle) -> Result<bool, String> {
    Ok(park_search_overlay_inner(&app))
}

#[tauri::command]
fn set_search_overlay_stage(app: AppHandle, stage: String) -> Result<(), String> {
    let parsed = SearchOverlayStage::parse(&stage)?;
    let focus = parsed == SearchOverlayStage::Stage;
    show_search_overlay(&app, parsed, focus);
    Ok(())
}

#[tauri::command]
fn open_search_result(app: AppHandle, url: String) -> Result<(), String> {
    let state = app.state::<AppState>();
    if !state.search.is_allowed_url(&url) && !search::is_trusted_external_url(&url) {
        return Err("That URL is not part of the current search results".into());
    }
    search::open_url_in_default_browser(&url)?;
    // The link leaves Pronto: drop the always-on-top overlay so the browser
    // comes forward instead of staying buried underneath it.
    dismiss_search_overlay_inner(&app);
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchImagePayload {
    mime: String,
    data: Vec<u8>,
}

#[tauri::command]
fn fetch_search_image(app: AppHandle, url: String) -> Result<SearchImagePayload, String> {
    let state = app.state::<AppState>();
    if !state.search.is_allowed_url(&url) && !search::is_trusted_search_image_url(&url) {
        return Err("That image is not part of the current search results".into());
    }
    let (mime, data) = search::fetch_allowlisted_image(&state.search_http, &url)?;
    Ok(SearchImagePayload { mime, data })
}

#[tauri::command]
fn open_ddg_search(app: AppHandle, query: String) -> Result<(), String> {
    let url = search::ddg_search_url(&query)
        .ok_or_else(|| "No search query to open".to_string())?;
    search::open_url_in_default_browser(&url)?;
    dismiss_search_overlay_inner(&app);
    Ok(())
}

#[tauri::command]
fn get_search_status(state: tauri::State<'_, AppState>) -> Result<SearchStatus, String> {
    state.search.status()
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
    if insert::copy_and_paste_focus(target, &text)? {
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
fn cleanup_notetaker_transcript(
    state: tauri::State<'_, AppState>,
    text: String,
) -> Result<String, String> {
    let transcript = text.trim().to_string();
    if transcript.is_empty() {
        return Err("There is no transcript text to clean up yet.".into());
    }
    if transcript.len() > 60_000 {
        return Err("This transcript is too long to clean up in one request.".into());
    }
    let settings = state.settings.snapshot()?;
    let api_key = settings::deepseek_key().ok_or_else(|| {
        "Add a DeepSeek API key in Settings to enable Clean Up Speech.".to_string()
    })?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|error| format!("DeepSeek cleanup failed: {error}"))?;
    let cleaned =
        engine::deepseek_longform_cleanup(&client, &api_key, &transcript, &settings.dictionary)?;
    Ok(engine::apply_dictionary_public(
        &cleaned,
        &settings.dictionary,
    ))
}

#[tauri::command]
fn minimize_main_window(app: AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "Pronto's main window is unavailable".to_string())?;
    window.minimize().map_err(|error| error.to_string())
}

#[tauri::command]
fn toggle_maximize_main_window(app: AppHandle) -> Result<bool, String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "Pronto's main window is unavailable".to_string())?;
    let maximized = window.is_maximized().map_err(|error| error.to_string())?;
    if maximized {
        window.unmaximize().map_err(|error| error.to_string())?;
    } else {
        window.maximize().map_err(|error| error.to_string())?;
    }
    Ok(!maximized)
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
    let meeting = MenuItem::with_id(app, "meeting", "Take meeting notes", true, None::<&str>)?;
    let paste = MenuItem::with_id(
        app,
        "paste-last",
        "Paste Last Transcript",
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &meeting, &paste, &quit])?;
    *app.state::<AppState>()
        .meeting_tray_item
        .lock()
        .expect("tray item lock poisoned") = Some(meeting);
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
            "meeting" => {
                let recording = app
                    .state::<AppState>()
                    .meetings
                    .status()
                    .map(|status| status.recording)
                    .unwrap_or(false);
                if recording {
                    match finish_meeting_recording(app) {
                        Ok(_) => {
                            let _ = app.emit(
                                "tray-message",
                                serde_json::json!({
                                    "message": "Meeting saved. Creating notes…",
                                    "error": false
                                }),
                            );
                        }
                        Err(message) => {
                            let _ = app.emit(
                                "tray-message",
                                serde_json::json!({
                                    "message": message,
                                    "error": true
                                }),
                            );
                        }
                    }
                    return;
                }
                if let Some(overlay) = app.get_webview_window("overlay") {
                    let _ = overlay.show();
                }
                let _ = app.emit(
                    "meeting-suggestion",
                    serde_json::json!({ "title": "Untitled meeting" }),
                );
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
                EngineController::new(app.handle().clone(), resource_dir.clone(), gpu_memory_management);
            *app.state::<AppState>()
                .engine
                .lock()
                .expect("engine lock poisoned") = Some(engine);
            #[cfg(windows)]
            meeting_detector::start(
                app.handle().clone(),
                app.state::<AppState>().meetings.activity_flag(),
                Arc::clone(&app.state::<AppState>().dictation_active),
                Arc::clone(&app.state::<AppState>().detector_control),
            );
            // Power resume heals sleep-related failures (dead overlay page,
            // wedged dictation state) instead of degrading silently.
            #[cfg(windows)]
            power::start(app.handle().clone());
            let paste_shortcut = app
                .state::<AppState>()
                .paste_shortcut
                .lock()
                .expect("shortcut lock poisoned")
                .clone();
            let search_shortcut = app
                .state::<AppState>()
                .search_shortcut
                .lock()
                .expect("shortcut lock poisoned")
                .clone();
            app.state::<AppState>()
                .search
                .set_resource_dir(resource_dir.clone());
            let handle = app.handle().clone();
            match HotkeyController::new(
                vec![
                    (HotkeyId::Dictation, shortcut),
                    (HotkeyId::Paste, paste_shortcut),
                    (HotkeyId::Search, search_shortcut),
                ],
                move |id, event| handle_hotkey_event(&handle, id, event),
            ) {
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
            } else if window.label() == "search" {
                match event {
                    tauri::WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        dismiss_search_overlay_inner(window.app_handle());
                    }
                    tauri::WindowEvent::Focused(true) => {
                        let app = window.app_handle();
                        let state = app.state::<AppState>();
                        let phase = state.search.status().map(|status| status.phase).ok();
                        if matches!(
                            phase,
                            Some(SearchPhase::Searching | SearchPhase::Complete | SearchPhase::Error)
                        ) {
                            state.search_blur_dismiss.store(true, Ordering::Release);
                        }
                    }
                    tauri::WindowEvent::Focused(false) => {
                        let app = window.app_handle();
                        let state = app.state::<AppState>();
                        if state.search_blur_dismiss.swap(false, Ordering::AcqRel) {
                            park_search_overlay_inner(app);
                        }
                    }
                    _ => {}
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            get_model_status,
            start_recording,
            stop_recording,
            start_meeting_recording,
            stop_meeting_recording,
            get_meeting_status,
            get_meetings,
            transcribe_media_file,
            start_media_upload,
            append_media_chunk,
            abort_media_upload,
            finish_media_upload,
            rename_meeting,
            delete_meeting,
            retry_meeting,
            retry_notetaker_upload,
            delete_notetaker_audio,
            cancel_recording,
            reset,
            get_preferences,
            save_settings,
            get_microphones,
            set_microphone,
            compact_overlay,
            overlay_heartbeat,
            resize_microphone_overlay,
            resize_overlay,
            dismiss_meeting_prompt,
            dismiss_meeting_suggestion,
            get_hotkey_status,
            set_hotkey,
            set_paste_hotkey,
            set_search_hotkey,
            get_search_hotkey,
            start_search_recording,
            stop_search_recording,
            cancel_search,
            run_text_search,
            reroute_dictation_to_search_command,
            dismiss_search_overlay,
            park_search_overlay,
            set_search_overlay_stage,
            open_search_result,
            open_ddg_search,
            fetch_search_image,
            get_search_status,
            save_api_key,
            add_dictionary_term,
            remove_dictionary_term,
            get_history,
            clear_history,
            insert_again,
            paste_last_transcript,
            copy_transcript,
            cleanup_notetaker_transcript,
            minimize_main_window,
            toggle_maximize_main_window,
            hide_main_window
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pronto");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_stay_ms_below_one_second() {
        assert_eq!(format_duration(0), "0 ms");
        assert_eq!(format_duration(86), "86 ms");
        assert_eq!(format_duration(999), "999 ms");
    }

    #[test]
    fn durations_switch_to_seconds_at_one_second() {
        assert_eq!(format_duration(1000), "1 s");
        assert_eq!(format_duration(1050), "1.1 s");
        assert_eq!(format_duration(1500), "1.5 s");
        assert_eq!(format_duration(1999), "2 s");
        assert_eq!(format_duration(12_340), "12.3 s");
    }
}
