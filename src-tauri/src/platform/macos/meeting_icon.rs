use objc2_app_kit::NSRunningApplication;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingIcon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Read only the owning application's icon. No window content is captured.
pub fn icon_for_window(pid: i32) -> Option<MeetingIcon> {
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
    let tiff = app.icon()?.TIFFRepresentation()?;
    let decoded = image::load_from_memory_with_format(&tiff.to_vec(), image::ImageFormat::Tiff)
        .ok()?
        .thumbnail(64, 64)
        .to_rgba8();
    Some(MeetingIcon {
        width: decoded.width(),
        height: decoded.height(),
        rgba: decoded.into_raw(),
    })
}
