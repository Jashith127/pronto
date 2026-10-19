//! Pronto's own tray menu.
//!
//! The menu is a WebView (`ui/tray.html`) rather than a native menu so it can
//! wear Pronto's tokens and open a transcript history flyout, neither of which
//! a native menu can do. The tray icon only decides *when* the menu opens;
//! every label and every enabled state is read from the engine at open time,
//! so nothing here is a copy of a saved setting that could go stale.
//!
//! Geometry is owned by Rust. The page lays itself out and reports the box it
//! needs plus the point inside that box which should sit under the cursor;
//! Rust picks the monitor, flips the menu back inside the work area, and only
//! then shows the window. The page never paints at a size the user can see.

use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, PhysicalRect};

use crate::engine::ModelStatus;
use crate::pipeline::Phase;
use crate::{
    begin_recording, finish_meeting_recording, finish_recording, insert, paste_last, AppState,
    HistoryEntry,
};

#[cfg(target_os = "macos")]
use crate::log_dictation_step;

/// Window label of the menu page, from `tauri.conf.json`.
pub const WINDOW_LABEL: &str = "tray-menu";

/// Gap between the menu and the screen edge, in CSS pixels. The gap between the
/// root menu and its flyout belongs to the page, which owns that layout.
const SCREEN_MARGIN: f64 = 8.0;

/// Showing a window and focusing it are two round trips to the window manager.
/// A `Focused(false)` that arrives in between belongs to the menu's own
/// activation, not to the user clicking away, so it must not dismiss it.
const FOCUS_GRACE: Duration = Duration::from_millis(400);

// ---------------------------------------------------------------- menu state

/// Where the speech model actually is, right now.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayModelState {
    /// Short name for the row's trailing label, e.g. "Parakeet".
    pub name: String,
    /// Held in memory: the menu offers to offload it.
    pub resident: bool,
    /// Starting up: neither load nor offload would do anything yet.
    pub loading: bool,
    pub backend: String,
    pub message: String,
}

/// One recent transcript, trimmed to what fits a menu row.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayTranscript {
    pub id: String,
    pub created_at_ms: u128,
    pub preview: String,
    pub total_ms: u128,
    pub word_count: usize,
}

/// Everything the menu draws, read fresh on every open.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayMenuState {
    pub listening: bool,
    pub meeting_recording: bool,
    pub model: TrayModelState,
    pub history: Vec<TrayTranscript>,
    /// How many transcripts were asked for, shown as the flyout's count.
    pub history_count: usize,
    /// Everything in history, so the flyout can say so when it lists a few.
    pub history_total: usize,
    pub theme: crate::settings::ThemeMode,
    pub dictation_hotkey: String,
    pub paste_hotkey: String,
}

/// The cursor and the work area around it, in CSS pixels. The page needs this
/// to decide which side the history flyout opens on and how much of it fits:
/// a tray icon in the bottom-right corner has no room to the right, one in a
/// top-left taskbar has plenty.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayMenuSpace {
    pub cursor_x: f64,
    pub cursor_y: f64,
    pub work_top: f64,
    pub work_right: f64,
    pub work_bottom: f64,
    pub margin: f64,
}

/// What one open of the menu needs, fetched in a single round trip.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayMenuPayload {
    pub state: TrayMenuState,
    pub space: TrayMenuSpace,
}

/// A menu row long enough to read at a glance. Longer previews are cut with
/// an ellipsis by the page instead, since it can measure the real font.
const PREVIEW_CHARS: usize = 220;

fn transcript_row(entry: &HistoryEntry) -> TrayTranscript {
    let trimmed = entry.final_text.trim();
    let preview = if trimmed.chars().count() > PREVIEW_CHARS {
        trimmed.chars().take(PREVIEW_CHARS).collect::<String>() + "…"
    } else {
        trimmed.to_string()
    };
    TrayTranscript {
        id: entry.id.to_string(),
        created_at_ms: entry.created_at_ms,
        word_count: trimmed.split_whitespace().count(),
        preview,
        total_ms: entry.total_ms,
    }
}

fn model_state(status: &ModelStatus, name: &str) -> TrayModelState {
    TrayModelState {
        name: name.to_string(),
        resident: status.resident,
        loading: status.loading,
        backend: status.backend.clone(),
        message: status.message.clone(),
    }
}

#[tauri::command]
pub fn tray_menu_state(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<TrayMenuPayload, String> {
    state.tray_menu_loaded.store(true, Ordering::Release);
    let listening = state
        .pipeline
        .lock()
        .map(|pipeline| pipeline.status.phase == Phase::Listening)
        .unwrap_or(false);
    let meeting_recording = state
        .meetings
        .status()
        .map(|status| status.recording)
        .unwrap_or(false);
    let settings = state.settings.snapshot()?;
    let depth = usize::from(crate::settings::normalize_tray_history_count(
        settings.tray_history_count,
    ));
    let history = state.settings.history()?;
    let model = state
        .model_status
        .lock()
        .map_err(|_| "model status lock poisoned")?
        .clone();
    let state_payload = TrayMenuState {
        listening,
        meeting_recording,
        model: model_state(&model, settings.asr_model.short_name()),
        history: history.iter().take(depth).map(transcript_row).collect(),
        history_count: depth,
        history_total: history.len(),
        theme: settings.theme,
        dictation_hotkey: settings.hotkey.clone(),
        paste_hotkey: settings.paste_hotkey.clone(),
    };
    Ok(TrayMenuPayload {
        state: state_payload,
        space: menu_space(&app, &state),
    })
}

/// Read the cursor and the work area around it in CSS pixels. Falls back to a
/// full screen at the origin, which only happens if the window is not up yet.
fn menu_space(app: &AppHandle, state: &AppState) -> TrayMenuSpace {
    let fallback = TrayMenuSpace {
        cursor_x: 0.0,
        cursor_y: 0.0,
        work_top: 0.0,
        work_right: SCREEN_MARGIN * 2.0,
        work_bottom: SCREEN_MARGIN * 2.0,
        margin: SCREEN_MARGIN,
    };
    let (Some(window), Ok(cursor)) = (
        app.get_webview_window(WINDOW_LABEL),
        state.tray_menu_cursor.lock(),
    ) else {
        return fallback;
    };
    let Some(cursor) = cursor.as_ref().copied() else {
        return fallback;
    };
    let scale = window.scale_factor().unwrap_or(1.0).max(0.5);
    let Some(work) = work_area(&window, cursor) else {
        return fallback;
    };
    TrayMenuSpace {
        cursor_x: cursor.x / scale,
        cursor_y: cursor.y / scale,
        work_top: work.position.y as f64 / scale,
        work_right: (work.position.x as f64 + work.size.width as f64) / scale,
        work_bottom: (work.position.y as f64 + work.size.height as f64) / scale,
        margin: SCREEN_MARGIN,
    }
}

// ------------------------------------------------------------------ commands

/// The page's own measurements, in CSS pixels. `anchor_x`/`anchor_y` are the
/// point inside that box that belongs under the cursor: the top-left corner
/// for the closed menu, and the inner menu's own top-left corner once the
/// flyout opens beside it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayMenuLayout {
    width: f64,
    height: f64,
    anchor_x: f64,
    anchor_y: f64,
}

#[tauri::command]
pub fn tray_menu_ready(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    layout: TrayMenuLayout,
) -> Result<(), String> {
    let Some(window) = app.get_webview_window(WINDOW_LABEL) else {
        return Err("Pronto's tray menu is unavailable".into());
    };
    let Some(cursor) = state
        .tray_menu_cursor
        .lock()
        .map_err(|_| "cursor lock poisoned")?
        .to_owned()
    else {
        // The page loaded on its own, with no click behind it. Wait for a real
        // open rather than parking a menu at an arbitrary corner.
        return Ok(());
    };
    let scale = window.scale_factor().unwrap_or(1.0).max(0.5);
    let work = work_area(&window, cursor).unwrap_or_else(|| PhysicalRect {
        position: PhysicalPosition::new(0, 0),
        size: tauri::PhysicalSize::new(1920, 1080),
    });
    let margin = (SCREEN_MARGIN * scale).round() as i32;
    let width = layout.width.max(1.0);
    let height = layout.height.max(1.0);
    let width_px = (width * scale).round() as i32;
    let height_px = (height * scale).round() as i32;

    let mut x = (cursor.x - layout.anchor_x * scale).round() as i32;
    let mut y = (cursor.y - layout.anchor_y * scale).round() as i32;
    let right = work.position.x + work.size.width as i32;
    let bottom = work.position.y + work.size.height as i32;
    // Clamp rather than flip: the anchor is already the page's choice, and a
    // menu taller than the work area has to sit somewhere.
    x = x.clamp(
        work.position.x + margin,
        (right - margin - width_px).max(work.position.x),
    );
    y = y.clamp(
        work.position.y + margin,
        (bottom - margin - height_px).max(work.position.y),
    );

    window
        .set_size(LogicalSize::new(width, height))
        .map_err(|error| error.to_string())?;
    window
        .set_position(PhysicalPosition::new(x as f64, y as f64))
        .map_err(|error| error.to_string())?;
    if let Ok(mut shown) = state.tray_menu_shown_at.lock() {
        *shown = Some(Instant::now());
    }
    window.show().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn tray_menu_close(app: AppHandle) {
    hide(&app);
}

pub fn hide(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.hide();
    }
}

/// A blur that lands before the menu could plausibly have been focused is the
/// activation itself; anything later is the user moving on.
pub fn dismiss_on_blur(app: &AppHandle) {
    let state = app.state::<AppState>();
    let settled = state
        .tray_menu_shown_at
        .lock()
        .ok()
        .and_then(|shown| *shown)
        .is_some_and(|shown| shown.elapsed() > FOCUS_GRACE);
    if settled {
        hide(app);
    }
}

/// One entry point for every menu row, so the page holds no backend logic and
/// the actions the tray used to own stay in one place. Dismissing the menu is
/// the backend's job, not the page's: an action that types into the foreground
/// window would otherwise aim at Pronto's own menu.
#[tauri::command]
pub async fn tray_menu_action(
    app: AppHandle,
    action: String,
    arg: Option<String>,
) -> Result<(), String> {
    hide(&app);
    // These bodies wait on audio threads and the window manager, which would
    // otherwise stall the menu's own close.
    crate::off_main_thread(move || {
        await_dismiss(&app);
        run_action(&app, &action, arg.as_deref())
    })
    .await
}

/// Hiding a window is posted to the window thread, so `is_visible` can still
/// read true for a moment after `hide` returns, and the foreground window only
/// catches up once the window manager has finished with it. Actions that type
/// somewhere read the foreground window, so wait for the real thing.
fn await_dismiss(app: &AppHandle) {
    #[cfg(windows)]
    let menu = app
        .get_webview_window(WINDOW_LABEL)
        .and_then(|window| window.hwnd().ok())
        .map(|hwnd| hwnd.0 as isize);
    for _ in 0..25 {
        let hidden = app
            .get_webview_window(WINDOW_LABEL)
            .and_then(|window| window.is_visible().ok())
            .is_none_or(|visible| !visible);
        #[cfg(windows)]
        let focused_elsewhere = menu.is_none_or(|menu| insert::foreground_window() != menu);
        #[cfg(not(windows))]
        let focused_elsewhere = true;
        if hidden && focused_elsewhere {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn run_action(app: &AppHandle, action: &str, arg: Option<&str>) -> Result<(), String> {
    let result = dispatch(app, action, arg);
    // A menu has nowhere to report a failure, so every error brings Pronto to
    // the front with the reason rather than failing silently in a log.
    if let Err(message) = &result {
        focus_main(app);
        let _ = app.emit(
            "tray-message",
            serde_json::json!({ "message": message, "error": true }),
        );
    }
    result
}

fn dispatch(app: &AppHandle, action: &str, arg: Option<&str>) -> Result<(), String> {
    match action {
        "open" => {
            focus_main(app);
            Ok(())
        }
        "open-history" => {
            focus_main(app);
            let _ = app.emit("navigate-view", serde_json::json!({ "view": "history" }));
            Ok(())
        }
        "open-settings" => {
            focus_main(app);
            let _ = app.emit("navigate-view", serde_json::json!({ "view": "settings" }));
            Ok(())
        }
        "toggle-dictation" => {
            #[cfg(target_os = "macos")]
            log_dictation_step("tray menu dictation action");
            let listening = app
                .state::<AppState>()
                .pipeline
                .lock()
                .map(|pipeline| pipeline.status.phase == Phase::Listening)
                .unwrap_or(false);
            if listening {
                finish_recording(app).map(|_| ())
            } else {
                begin_recording(app).map(|_| ())
            }
        }
        "toggle-meeting" => {
            let recording = app
                .state::<AppState>()
                .meetings
                .status()
                .map(|status| status.recording)
                .unwrap_or(false);
            if recording {
                let _ = app.emit(
                    "tray-message",
                    serde_json::json!({
                        "message": "Meeting saved. Creating notes…",
                        "error": false
                    }),
                );
                return finish_meeting_recording(app).map(|_| ());
            }
            app.state::<AppState>().claim_overlay();
            if let Some(overlay) = app.get_webview_window("overlay") {
                let _ = overlay.show();
            }
            let _ = app.emit(
                "meeting-suggestion",
                serde_json::json!({ "title": "Untitled meeting" }),
            );
            Ok(())
        }
        "paste-last" => {
            notify(app, &paste_last(app)?);
            Ok(())
        }
        "paste-transcript" => paste_transcript(app, arg),
        "copy-transcript" => copy_transcript(app, arg),
        "load-model" => load_model(app),
        "offload-model" => offload_model(app),
        "quit" => {
            let _ = app.state::<AppState>().system_audio.restore();
            app.exit(0);
            Ok(())
        }
        other => Err(format!("Unknown tray action: {other}")),
    }
}

fn notify(app: &AppHandle, message: &str) {
    let _ = app.emit(
        "tray-message",
        serde_json::json!({ "message": message, "error": false }),
    );
}

/// A transcript picked out of the history flyout is inserted where dictation
/// began, the same target `Paste Last Transcript` uses.
fn paste_transcript(app: &AppHandle, arg: Option<&str>) -> Result<(), String> {
    let state = app.state::<AppState>();
    let text = transcript_by_id(&state, arg)?;
    let target = *state
        .target_window
        .lock()
        .map_err(|_| "target window lock poisoned")?;
    if insert::copy_and_paste_focus(target, &text)? {
        notify(app, "Transcript pasted");
        Ok(())
    } else {
        #[cfg(target_os = "macos")]
        return Err("That transcript could not be inserted. It is still in History; your clipboard is unchanged.".to_string());
        #[cfg(windows)]
        {
            notify(app, "Transcript copied to the clipboard");
            Ok(())
        }
    }
}

fn copy_transcript(app: &AppHandle, arg: Option<&str>) -> Result<(), String> {
    let text = transcript_by_id(&app.state::<AppState>(), arg)?;
    insert::copy_to_clipboard(&text)?;
    notify(app, "Transcript copied");
    Ok(())
}

fn transcript_by_id(state: &AppState, arg: Option<&str>) -> Result<String, String> {
    let id = arg
        .ok_or_else(|| "No transcript was chosen".to_string())?
        .parse::<u128>()
        .map_err(|_| "Invalid transcript identifier".to_string())?;
    state
        .settings
        .transcript(id)?
        .ok_or_else(|| "That transcript is no longer in history".to_string())
}

/// Load and offload act on the engine, never on a stored preference: the menu
/// only offers the one that is actually available, and the next dictation warms
/// an offloaded model on its own.
fn load_model(app: &AppHandle) -> Result<(), String> {
    if model_resident(app)? {
        return Ok(());
    }
    engine(app)?.load();
    notify(app, "Loading the speech model…");
    Ok(())
}

fn offload_model(app: &AppHandle) -> Result<(), String> {
    if !model_resident(app)? {
        return Ok(());
    }
    let transcribing = app
        .state::<AppState>()
        .pipeline
        .lock()
        .map(|pipeline| pipeline.status.phase == Phase::Processing)
        .unwrap_or(false);
    if transcribing {
        // Stopping the server mid-job would throw the recording away.
        return Err("Pronto is finishing a dictation. Try offloading in a moment.".into());
    }
    engine(app)?.offload();
    notify(app, "Speech model offloaded");
    Ok(())
}

/// Read the published load state, not a guess: the engine may have released
/// the model on its own to free VRAM since the menu was last drawn.
fn model_resident(app: &AppHandle) -> Result<bool, String> {
    Ok(app
        .state::<AppState>()
        .model_status
        .lock()
        .map_err(|_| "model status lock poisoned")?
        .resident)
}

fn engine(app: &AppHandle) -> Result<crate::engine::EngineController, String> {
    app.state::<AppState>()
        .engine
        .lock()
        .ok()
        .and_then(|engine| engine.clone())
        .ok_or_else(|| "The speech engine is not running yet".to_string())
}

fn focus_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

// ------------------------------------------------------------------ geometry

/// The work area of the display under `point`, in physical pixels. Tray icons
/// on a second monitor must open there, not on the primary one.
fn work_area(
    window: &tauri::WebviewWindow,
    point: PhysicalPosition<f64>,
) -> Option<PhysicalRect<i32, u32>> {
    let monitors = window.available_monitors().ok()?;
    let area = monitors
        .iter()
        .find(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            point.x >= position.x as f64
                && point.x < position.x as f64 + size.width as f64
                && point.y >= position.y as f64
                && point.y < position.y as f64 + size.height as f64
        })
        .map(|monitor| *monitor.work_area());
    if area.is_some() {
        return area;
    }
    // A tray icon's reported position can sit a hair outside every monitor's
    // bounds on some multi-DPI setups; the nearest one is the right guess.
    let nearest = window.primary_monitor().ok().flatten();
    nearest.map(|monitor| *monitor.work_area())
}

// ---------------------------------------------------------------- tray icon

/// Builds the tray icon. Left and right click both open the menu; the icon
/// carries no native menu, so Pronto's page is the only menu.
pub fn create(app: &tauri::App) -> tauri::Result<()> {
    let mut tray = TrayIconBuilder::new()
        .tooltip("Pronto dictation")
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button,
                button_state,
                position,
                ..
            } = event
            {
                if button_state != MouseButtonState::Up {
                    return;
                }
                if !matches!(button, MouseButton::Left | MouseButton::Right) {
                    return;
                }
                open(tray.app_handle(), position);
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

/// Reopen the menu under the cursor. The page re-reads its state on the
/// `tray-menu-open` event, so a menu left open while the engine finished
/// loading can never show a stale label.
pub fn open(app: &AppHandle, position: PhysicalPosition<f64>) {
    let state = app.state::<AppState>();
    if let Ok(mut cursor) = state.tray_menu_cursor.lock() {
        *cursor = Some(position);
    }
    // Always rebuild: the page may be showing an old menu, and a second click
    // should reposition it rather than leave it where it was.
    hide(app);
    // Before the page's first read there is nobody listening for the event;
    // it reports its own box on load and finds the cursor already set.
    if state.tray_menu_loaded.load(Ordering::Acquire) {
        let _ = app.emit_to(WINDOW_LABEL, "tray-menu-open", ());
    }
}
