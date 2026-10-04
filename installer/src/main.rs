// Pronto Setup: a small Tauri app with Pronto's look that installs Pronto,
// lets the user pick a speech engine, and downloads it with live progress.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod install;
mod system;

use install::SetupProgress;
use serde::Serialize;
use speech_packs::AsrModel;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

const PROGRESS_EVENT: &str = "setup-progress";

#[derive(Debug, Default, PartialEq)]
struct Args {
    uninstall: bool,
    silent: bool,
    /// `None` means "recommended for this PC".
    model: Option<AsrModel>,
    remove_data: bool,
    /// Set on the temporary copy the uninstaller relaunches as.
    from_temp: bool,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag.to_string(), Some(value.to_string())),
            None => (arg.clone(), None),
        };
        match flag.to_ascii_lowercase().as_str() {
            "--uninstall" => parsed.uninstall = true,
            // `/S` keeps NSIS-style silent installs working.
            "--silent" | "/s" => parsed.silent = true,
            "--remove-data" => parsed.remove_data = true,
            "--from-temp" => parsed.from_temp = true,
            "--model" => {
                let value = inline
                    .or_else(|| args.next())
                    .ok_or("--model needs parakeet, phonon, or auto")?;
                parsed.model = match value.to_ascii_lowercase().as_str() {
                    "auto" => None,
                    other => {
                        Some(AsrModel::parse(other).ok_or(format!("Unknown model `{other}`"))?)
                    }
                };
            }
            _ => return Err(format!("Unknown option `{arg}`")),
        }
    }
    Ok(parsed)
}

struct SetupState {
    args: Args,
    dir: PathBuf,
    busy: AtomicBool,
    cancel: Arc<AtomicBool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelChoice {
    id: AsrModel,
    name: &'static str,
    download_bytes: u64,
    recommended: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SetupInfo {
    mode: &'static str,
    version: &'static str,
    upgrade: bool,
    current_model: Option<AsrModel>,
    gpu_name: Option<String>,
    has_nvidia: bool,
    recommended: AsrModel,
    models: Vec<ModelChoice>,
}

#[tauri::command]
fn setup_info(state: tauri::State<'_, SetupState>) -> SetupInfo {
    let gpu = speech_packs::detect_gpus();
    let recommended = speech_packs::recommended_model(&gpu);
    let upgrade = install::is_installed(&state.dir);
    SetupInfo {
        mode: if state.args.uninstall {
            "uninstall"
        } else {
            "install"
        },
        version: install::APP_VERSION,
        upgrade,
        current_model: upgrade
            .then(|| install::current_model(&state.dir))
            .flatten(),
        gpu_name: gpu.primary().map(|adapter| adapter.name.clone()),
        has_nvidia: gpu.has_nvidia(),
        recommended,
        models: AsrModel::ALL
            .into_iter()
            .map(|model| ModelChoice {
                id: model,
                name: model.short_name(),
                download_bytes: install::download_bytes(&state.dir, model),
                recommended: model == recommended,
            })
            .collect(),
    }
}

fn emit(app: &AppHandle, progress: SetupProgress) {
    let _ = app.emit(PROGRESS_EVENT, progress);
}

/// Run `work` on a background thread, reporting its result as a final event.
fn run_job(
    app: AppHandle,
    work: impl FnOnce(&AppHandle, &AtomicBool) -> Result<(), String> + Send + 'static,
) -> Result<(), String> {
    let state = app.state::<SetupState>();
    if state.busy.swap(true, Ordering::AcqRel) {
        return Err("Setup is already running".into());
    }
    state.cancel.store(false, Ordering::Release);
    let cancel = Arc::clone(&state.cancel);
    std::thread::spawn(move || {
        if let Err(error) = work(&app, &cancel) {
            let stage = if cancel.load(Ordering::Acquire) {
                "cancelled"
            } else {
                "error"
            };
            emit(&app, SetupProgress::simple(stage, 0.0, error));
        }
        app.state::<SetupState>()
            .busy
            .store(false, Ordering::Release);
    });
    Ok(())
}

#[tauri::command]
fn start_install(app: AppHandle, model: AsrModel) -> Result<(), String> {
    run_job(app, move |app, cancel| {
        let dir = app.state::<SetupState>().dir.clone();
        install::install(&dir, model, cancel, |progress| emit(app, progress))
    })
}

#[tauri::command]
fn cancel_install(state: tauri::State<'_, SetupState>) {
    state.cancel.store(true, Ordering::Release);
}

#[tauri::command]
fn start_uninstall(app: AppHandle, keep_data: bool) -> Result<(), String> {
    run_job(app, move |app, _| {
        let dir = app.state::<SetupState>().dir.clone();
        install::uninstall(&dir, keep_data, |progress| emit(app, progress))
    })
}

#[tauri::command]
fn launch_pronto(app: AppHandle, state: tauri::State<'_, SetupState>) -> Result<(), String> {
    system::launch_detached(&state.dir.join("pronto.exe"))?;
    finish(&app);
    Ok(())
}

#[tauri::command]
fn show_window(app: AppHandle) {
    if let Some(window) = app.get_webview_window("setup") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[tauri::command]
fn minimize_window(app: AppHandle) {
    if let Some(window) = app.get_webview_window("setup") {
        let _ = window.minimize();
    }
}

#[tauri::command]
fn close_window(app: AppHandle, state: tauri::State<'_, SetupState>) {
    // Closing mid-download cancels it; the partial download resumes next time.
    state.cancel.store(true, Ordering::Release);
    finish(&app);
}

fn finish(app: &AppHandle) {
    let state = app.state::<SetupState>();
    if state.args.from_temp {
        if let Ok(me) = std::env::current_exe() {
            system::delete_after_exit(&me);
        }
    }
    app.exit(0);
}

/// The uninstaller lives in the folder it removes, so it reruns itself from
/// a temporary copy first. Returns true when this process should just exit.
fn relaunch_uninstaller_from_temp(args: &Args) -> bool {
    if !args.uninstall || args.from_temp {
        return false;
    }
    let Ok(me) = std::env::current_exe() else {
        return false;
    };
    let temp = std::env::temp_dir().join(format!("pronto-uninstall-{}.exe", std::process::id()));
    if std::fs::copy(&me, &temp).is_err() {
        return false;
    }
    let mut command = std::process::Command::new(&temp);
    command.args(std::env::args().skip(1)).arg("--from-temp");
    command.spawn().is_ok()
}

fn run_silent(args: &Args, dir: PathBuf) -> i32 {
    let result = if args.uninstall {
        install::uninstall(&dir, !args.remove_data, |_| {})
    } else {
        let model = args
            .model
            .unwrap_or_else(|| speech_packs::recommended_model(&speech_packs::detect_gpus()));
        install::install(&dir, model, &AtomicBool::new(false), |_| {})
    };
    if args.from_temp {
        if let Ok(me) = std::env::current_exe() {
            system::delete_after_exit(&me);
        }
    }
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

fn ensure_webview2() -> bool {
    if system::webview2_installed() {
        return true;
    }
    if !system::message_box_yes_no(
        "Pronto Setup",
        "Pronto needs the Microsoft Edge WebView2 Runtime, which is missing on this PC.\n\nDownload and install it now?",
    ) {
        return false;
    }
    match system::install_webview2() {
        Ok(()) => true,
        Err(error) => {
            system::show_error("Pronto Setup", &error);
            false
        }
    }
}

fn main() {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            system::show_error("Pronto Setup", &error);
            std::process::exit(2);
        }
    };
    if relaunch_uninstaller_from_temp(&args) {
        return;
    }
    let dir = system::install_dir();
    if args.silent {
        std::process::exit(run_silent(&args, dir));
    }
    if !ensure_webview2() {
        std::process::exit(1);
    }
    tauri::Builder::default()
        .manage(SetupState {
            args,
            dir,
            busy: AtomicBool::new(false),
            cancel: Arc::new(AtomicBool::new(false)),
        })
        .invoke_handler(tauri::generate_handler![
            setup_info,
            start_install,
            cancel_install,
            start_uninstall,
            launch_pronto,
            show_window,
            minimize_window,
            close_window
        ])
        .run(tauri::generate_context!())
        .expect("failed to start Pronto Setup");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn parses_silent_install_with_model() {
        let args = parse(&["/S", "--model=phonon"]).unwrap();
        assert!(args.silent);
        assert_eq!(args.model, Some(AsrModel::Phonon));
        assert_eq!(parse(&["--silent", "--model", "auto"]).unwrap().model, None);
    }

    #[test]
    fn parses_uninstall_flags() {
        let args = parse(&["--uninstall", "--silent", "--remove-data", "--from-temp"]).unwrap();
        assert!(args.uninstall && args.silent && args.remove_data && args.from_temp);
    }

    #[test]
    fn rejects_unknown_input() {
        assert!(parse(&["--model", "whisper"]).is_err());
        assert!(parse(&["--model"]).is_err());
        assert!(parse(&["--frobnicate"]).is_err());
    }
}
