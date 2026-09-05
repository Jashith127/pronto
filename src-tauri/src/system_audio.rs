use std::ptr;
use std::sync::mpsc::{self, Sender};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};

const DUCK_LEVEL: f32 = 0.12;

enum Command {
    Duck(Sender<Result<(), String>>),
    Restore(Sender<Result<(), String>>),
    Shutdown,
}

struct Snapshot {
    endpoint: IAudioEndpointVolume,
    volume: f32,
    muted: bool,
}

pub struct SystemAudioController {
    sender: Sender<Command>,
}

impl SystemAudioController {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("pronto-system-audio".into())
            .spawn(move || {
                let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() };
                let mut snapshot = None;
                while let Ok(command) = receiver.recv() {
                    match command {
                        Command::Duck(reply) => {
                            let result = if initialized {
                                duck_endpoint(&mut snapshot)
                            } else {
                                Err("Windows audio control could not initialize".into())
                            };
                            let _ = reply.send(result);
                        }
                        Command::Restore(reply) => {
                            let result = restore_endpoint(&mut snapshot);
                            let _ = reply.send(result);
                        }
                        Command::Shutdown => {
                            let _ = restore_endpoint(&mut snapshot);
                            break;
                        }
                    }
                }
                if initialized {
                    unsafe { CoUninitialize() };
                }
            })
            .expect("failed to start Windows audio controller");
        Self { sender }
    }

    pub fn duck(&self) -> Result<(), String> {
        self.request(Command::Duck)
    }

    pub fn restore(&self) -> Result<(), String> {
        self.request(Command::Restore)
    }

    fn request(
        &self,
        build: impl FnOnce(Sender<Result<(), String>>) -> Command,
    ) -> Result<(), String> {
        let (reply, response) = mpsc::channel();
        self.sender
            .send(build(reply))
            .map_err(|_| "Windows audio controller stopped".to_string())?;
        response
            .recv()
            .map_err(|_| "Windows audio controller did not respond".to_string())?
    }
}

impl Drop for SystemAudioController {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Shutdown);
    }
}

fn duck_endpoint(snapshot: &mut Option<Snapshot>) -> Result<(), String> {
    if snapshot.is_some() {
        return Ok(());
    }
    let endpoint = default_endpoint()?;
    let volume = unsafe { endpoint.GetMasterVolumeLevelScalar() }
        .map_err(|error| format!("Could not read system volume: {error}"))?;
    let muted = unsafe { endpoint.GetMute() }
        .map_err(|error| format!("Could not read system mute state: {error}"))?
        .as_bool();
    if !muted {
        unsafe { endpoint.SetMasterVolumeLevelScalar(ducked_volume(volume), ptr::null()) }
            .map_err(|error| format!("Could not duck system audio: {error}"))?;
    }
    *snapshot = Some(Snapshot {
        endpoint,
        volume,
        muted,
    });
    Ok(())
}

fn restore_endpoint(snapshot: &mut Option<Snapshot>) -> Result<(), String> {
    let Some(saved) = snapshot.take() else {
        return Ok(());
    };
    unsafe {
        saved
            .endpoint
            .SetMasterVolumeLevelScalar(saved.volume, ptr::null())
            .and_then(|_| saved.endpoint.SetMute(saved.muted, ptr::null()))
    }
    .map_err(|error| format!("Could not restore system audio: {error}"))
}

/// True when the default playback route is a Bluetooth device. Opening a
/// microphone flips such headsets from music (A2DP) to hands-free mode,
/// whose switch gap swallows any cue played during it. Any failure here
/// returns false so callers fall back to immediate cueing.
pub fn default_render_is_bluetooth() -> bool {
    unsafe {
        // COM is apartment-local; this runs on hotkey/caller threads that
        // have no COM state, so initialize privately like the controller.
        let initialized = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let bluetooth = default_endpoint_id()
            .map(|id| id.to_ascii_lowercase().contains("bthenum"))
            .unwrap_or(false);
        if initialized {
            CoUninitialize();
        }
        bluetooth
    }
}

fn default_endpoint_id() -> Result<String, String> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|error| format!("Could not access Windows audio devices: {error}"))?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|error| format!("No default playback device is available: {error}"))?;
        device
            .GetId()
            .map(|id| id.to_string().unwrap_or_default())
            .map_err(|error| format!("Could not identify playback device: {error}"))
    }
}

fn default_endpoint() -> Result<IAudioEndpointVolume, String> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|error| format!("Could not access Windows audio devices: {error}"))?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|error| format!("No default playback device is available: {error}"))?;
        device
            .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
            .map_err(|error| format!("Could not control the default playback device: {error}"))
    }
}

fn ducked_volume(original: f32) -> f32 {
    original.clamp(0.0, 1.0).min(DUCK_LEVEL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ducking_never_raises_volume() {
        assert_eq!(ducked_volume(0.8), DUCK_LEVEL);
        assert_eq!(ducked_volume(0.04), 0.04);
        assert_eq!(ducked_volume(0.0), 0.0);
    }

    #[test]
    #[ignore = "temporarily changes the interactive desktop's playback volume"]
    fn real_endpoint_is_restored_exactly() {
        let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() };
        assert!(initialized, "test COM apartment should initialize");
        let endpoint = default_endpoint().expect("default endpoint should exist");
        let before_volume = unsafe { endpoint.GetMasterVolumeLevelScalar() }.unwrap();
        let before_muted = unsafe { endpoint.GetMute() }.unwrap().as_bool();
        let controller = SystemAudioController::new();
        controller.duck().expect("default endpoint should duck");
        controller
            .restore()
            .expect("default endpoint should restore");
        let after_volume = unsafe { endpoint.GetMasterVolumeLevelScalar() }.unwrap();
        let after_muted = unsafe { endpoint.GetMute() }.unwrap().as_bool();
        assert!((before_volume - after_volume).abs() < f32::EPSILON);
        assert_eq!(before_muted, after_muted);
        unsafe { CoUninitialize() };
    }
}
