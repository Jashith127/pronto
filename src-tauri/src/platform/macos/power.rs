use std::sync::OnceLock;
use tauri::{AppHandle, Manager};

static APP: OnceLock<AppHandle> = OnceLock::new();

unsafe extern "C" {
    fn pronto_observe_wake(callback: extern "C" fn());
}

extern "C" fn woke() {
    if let Some(app) = APP.get() {
        crate::handle_system_resume(app);
        if let Ok(engine) = app.state::<crate::AppState>().engine.lock() {
            if let Some(engine) = engine.as_ref() {
                engine.warm();
            }
        }
    }
}

pub fn start(app: AppHandle) {
    let _ = APP.set(app);
    unsafe {
        pronto_observe_wake(woke);
    }
}
