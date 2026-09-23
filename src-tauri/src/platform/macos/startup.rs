use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;

const BACKGROUND_ARG: &str = "--background";

pub fn is_background_launch() -> bool {
    std::env::args_os().any(|argument| argument == BACKGROUND_ARG)
}

pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<(), String> {
    let manager = app.autolaunch();
    if enabled {
        manager
            .enable()
            .map_err(|error| format!("Could not enable Launch at Login: {error}"))
    } else {
        manager
            .disable()
            .map_err(|error| format!("Could not disable Launch at Login: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_login_launch_uses_explicit_argument() {
        assert_eq!(BACKGROUND_ARG, "--background");
    }
}
