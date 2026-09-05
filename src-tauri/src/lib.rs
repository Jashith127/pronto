mod audio;
mod engine;
mod gpu_memory;
mod hotkey;
mod insert;
mod meeting;
#[cfg(windows)]
mod meeting_detector;
#[cfg(windows)]
mod meeting_icon;
mod pipeline;
mod settings;
#[cfg(windows)]
mod single_instance;
mod sound;
mod startup;
mod system_audio;

use audio::{AudioController, MicrophoneStatus};
use engine::{
    CompletedMeetingTranscription, CompletedTranscription, EngineController,
    MeetingTranscriptionJob, ModelStatus, TranscriptionJob,
};
use hotkey::{Hotkey, HotkeyController, HotkeyEvent, HotkeyId, HotkeyStatus};
use pipeline::{EngineStatus, Phase, Pipeline};
use settings::{ActivationMode, AppPreferences, HistoryEntry, SettingsStore, UserSettings};
use sound::SoundController;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
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
    insertion_target: insert::InsertionTargetTracker,
    sounds: SoundController,
    engine: Mutex<Option<EngineController>>,
    settings: SettingsStore,
    target_window: Mutex<isize>,
    model_status: Mutex<ModelStatus>,
    active_shortcut: Mutex<Hotkey>,
    paste_shortcut: Mutex<Hotkey>,
    hotkey_controller: Mutex<Option<HotkeyController>>,
    hotkey_error: Mutex<Option<String>>,
    paste_hotkey_error: Mutex<Option<String>>,
    show_microphone_once: Mutex<bool>,
    pending_uploads: Mutex<HashMap<String, PendingUpload>>,
    meeting_tray_item: Mutex<Option<MenuItem<tauri::Wry>>>,
    detector_control: Arc<meeting_detector::DetectorControl>,
    dictation_active: Arc<AtomicBool>,
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
        if canonical != configured || paste_canonical != paste_configured {
            if let Ok(mut repaired) = settings.snapshot() {
                repaired.hotkey = canonical;
                repaired.paste_hotkey = paste_canonical;
                let _ = settings.replace(repaired);
            }
        }
        Self {
            pipeline: Mutex::new(Pipeline::default()),
            audio: AudioController::new(selected_microphone),
            system_audio: SystemAudioController::new(),
            meetings: meeting::MeetingController::new(),
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
            paste_shortcut: Mutex::new(paste_shortcut),
            hotkey_controller: Mutex::new(None),
            hotkey_error: Mutex::new(None),
            paste_hotkey_error: Mutex::new(None),
            show_microphone_once: Mutex::new(true),
            pending_uploads: Mutex::new(HashMap::new()),
            meeting_tray_item: Mutex::new(None),
            detector_control: Arc::new(meeting_detector::DetectorControl::new()),
            dictation_active: Arc::new(AtomicBool::new(false)),
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

pub(crate) fn set_model_status(app: &AppHandle, status: ModelStatus) {
    if let Ok(mut current) = app.state::<AppState>().model_status.lock() {
        *current = status.clone();
    }
    let _ = app.emit("model-status", status);
}

fn begin_recording(app: &AppHandle) -> Result<EngineStatus, String> {
    let state = app.state::<AppState>();
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
    match state.audio.start() {
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
        let logical_width = microphone_width.unwrap_or(136.0).max(136.0);
        let _ = window.set_size(LogicalSize::new(logical_width, logical_height));
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let max_logical_width = monitor.size().width as f64 / scale - 24.0;
    let logical_width = microphone_width
        .unwrap_or(136.0)
        .max(136.0)
        .min(max_logical_width.max(136.0));
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
            let message = match (
                completed.auto_insert,
                completed.cleanup_warning.as_ref(),
                insertion_error.as_ref(),
            ) {
                (_, _, Some(error)) => format!("Transcribed, but text insertion failed: {error}"),
                (true, Some(warning), None) => format!("Inserted with local cleanup · {warning}"),
                (true, None, None) => format!("Inserted in {} ms", completed.entry.total_ms),
                (false, Some(warning), None) => {
                    format!("File transcribed with local cleanup · {warning}")
                }
                (false, None, None) => {
                    format!("File transcribed in {} ms", completed.entry.total_ms)
                }
            };
            pipeline.complete(
                completed.entry.final_text.clone(),
                completed.entry.asr_ms,
                completed.entry.cleanup_ms,
                completed.entry.total_ms,
                message,
            );
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
        }
        Err(error) => {
            state.insertion_target.cancel();
            pipeline.fail(error)
        }
    }
    emit_status(app, &pipeline.status);
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
            std::thread::sleep(std::time::Duration::from_millis(140));
            let _ = overlay.hide();
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
    let stopped = state.meetings.stop()?;
    // Recording has ended regardless of what follows, so the tray goes
    // back to its idle label even on the error paths below.
    sync_meeting_tray_item(app, false);
    let settings = state.settings.snapshot()?;
    let engine = state.engine.lock().map_err(|_| "engine lock poisoned")?;
    let engine = engine
        .as_ref()
        .ok_or_else(|| "Transcription engine is still starting".to_string())?;
    engine.transcribe_meeting(MeetingTranscriptionJob {
        id: stopped.record.id.clone(),
        title: stopped.record.title.clone(),
        audio_path: stopped.audio_path,
        settings,
    })?;
    let _ = app.emit(
        "meeting-status",
        serde_json::json!({ "recording": false, "meeting": stopped.record, "elapsedSeconds": 0 }),
    );
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
    let mut pipeline = state
        .pipeline
        .lock()
        .map_err(|_| "pipeline lock poisoned")?;
    pipeline.reset();
    pipeline.status.message = "Dictation cancelled".into();
    emit_status(&app, &pipeline.status);
    if let Some(overlay) = app.get_webview_window("overlay") {
        // Lets the pill play its ~130ms exit animation before hiding.
        std::thread::sleep(std::time::Duration::from_millis(140));
        let _ = overlay.hide();
    }
    Ok(pipeline.status.clone())
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

fn handle_hotkey_event(app: &AppHandle, id: HotkeyId, event: HotkeyEvent) {
    if id == HotkeyId::Paste {
        if event == HotkeyEvent::Pressed {
            handle_paste_hotkey(app);
        }
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
    state: tauri::State<'_, AppState>,
    mut settings: UserSettings,
) -> Result<AppPreferences, String> {
    // Shortcut changes are transactional through set_hotkey so a settings
    // write can never persist a shortcut that Windows rejected.
    let previous = state.settings.snapshot()?;
    let gpu_memory_management_changed =
        settings.gpu_memory_management != previous.gpu_memory_management;
    settings.hotkey = previous.hotkey;
    settings.paste_hotkey = previous.paste_hotkey;
    settings.microphone_id = previous.microphone_id;
    settings.microphone_name = previous.microphone_name;
    settings.gpu_memory_management_configured = true;
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
    Ok(HotkeyStatus {
        shortcut: shortcut.canonical().to_string(),
        paste_shortcut: paste_shortcut.canonical().to_string(),
        registered: state
            .hotkey_controller
            .lock()
            .map(|value| value.is_some())
            .unwrap_or(false),
        error,
        paste_error,
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
    }
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
    controller.update(HotkeyId::Dictation, next.clone())?;

    let mut settings = state.settings.snapshot()?;
    settings.hotkey = canonical;
    if let Err(error) = state.settings.replace(settings) {
        let _ = controller.update(HotkeyId::Dictation, previous);
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
    }
    let previous = state
        .paste_shortcut
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
    controller.update(HotkeyId::Paste, next.clone())?;

    let mut settings = state.settings.snapshot()?;
    settings.paste_hotkey = canonical;
    if let Err(error) = state.settings.replace(settings) {
        let _ = controller.update(HotkeyId::Paste, previous);
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
                EngineController::new(app.handle().clone(), resource_dir, gpu_memory_management);
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
            let paste_shortcut = app
                .state::<AppState>()
                .paste_shortcut
                .lock()
                .expect("shortcut lock poisoned")
                .clone();
            let handle = app.handle().clone();
            match HotkeyController::new(
                vec![
                    (HotkeyId::Dictation, shortcut),
                    (HotkeyId::Paste, paste_shortcut),
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
            resize_microphone_overlay,
            resize_overlay,
            dismiss_meeting_prompt,
            dismiss_meeting_suggestion,
            get_hotkey_status,
            set_hotkey,
            set_paste_hotkey,
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
