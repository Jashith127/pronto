use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionStatus {
    microphone: bool,
    accessibility: bool,
    input_monitoring: bool,
    screen_recording: bool,
}

unsafe extern "C" {
    fn pronto_permission_status(kind: i32) -> i32;
    fn pronto_request_permission(kind: i32);
    fn pronto_open_permission_settings(kind: i32);
}

pub fn status() -> PermissionStatus {
    PermissionStatus {
        microphone: unsafe { pronto_permission_status(0) != 0 },
        accessibility: unsafe { pronto_permission_status(1) != 0 },
        input_monitoring: unsafe { pronto_permission_status(2) != 0 },
        screen_recording: unsafe { pronto_permission_status(3) != 0 },
    }
}

pub fn request(kind: &str) -> Result<(), String> {
    let id = permission_id(kind)?;
    unsafe { pronto_request_permission(id) };
    Ok(())
}

pub fn open_settings(kind: &str) -> Result<(), String> {
    let id = permission_id(kind)?;
    unsafe { pronto_open_permission_settings(id) };
    Ok(())
}

fn permission_id(kind: &str) -> Result<i32, String> {
    match kind {
        "microphone" => Ok(0),
        "accessibility" => Ok(1),
        "input-monitoring" => Ok(2),
        "screen-recording" => Ok(3),
        _ => Err("Unknown macOS permission".into()),
    }
}
