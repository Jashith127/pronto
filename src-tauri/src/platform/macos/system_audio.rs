use std::ffi::c_void;
use std::sync::mpsc::{self, Sender};

const SYSTEM_OBJECT: u32 = 1;
const GLOBAL: u32 = u32::from_be_bytes(*b"glob");
const OUTPUT: u32 = u32::from_be_bytes(*b"outp");
const DEFAULT_OUTPUT: u32 = u32::from_be_bytes(*b"dOut");
const VIRTUAL_MAIN_VOLUME: u32 = u32::from_be_bytes(*b"vmvc");
const MUTE: u32 = u32::from_be_bytes(*b"mute");
const TRANSPORT: u32 = u32::from_be_bytes(*b"tran");
const BLUETOOTH: u32 = u32::from_be_bytes(*b"blue");
const BLUETOOTH_LE: u32 = u32::from_be_bytes(*b"blea");
const DUCK_LEVEL: f32 = 0.12;

#[repr(C)]
struct PropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyData(
        object: u32,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        data_size: *mut u32,
        data: *mut c_void,
    ) -> i32;
    fn AudioObjectSetPropertyData(
        object: u32,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        data_size: u32,
        data: *const c_void,
    ) -> i32;
}

fn address(selector: u32, scope: u32) -> PropertyAddress {
    PropertyAddress {
        selector,
        scope,
        element: 0,
    }
}

fn get<T: Copy + Default>(object: u32, selector: u32, scope: u32) -> Result<T, String> {
    let mut result = T::default();
    let mut size = std::mem::size_of::<T>() as u32;
    let error = unsafe {
        AudioObjectGetPropertyData(
            object,
            &address(selector, scope),
            0,
            std::ptr::null(),
            &mut size,
            (&mut result as *mut T).cast(),
        )
    };
    if error != 0 || size != std::mem::size_of::<T>() as u32 {
        return Err(format!(
            "CoreAudio property {selector:#x} read failed ({error})"
        ));
    }
    Ok(result)
}

fn set<T: Copy>(object: u32, selector: u32, scope: u32, value: &T) -> Result<(), String> {
    let error = unsafe {
        AudioObjectSetPropertyData(
            object,
            &address(selector, scope),
            0,
            std::ptr::null(),
            std::mem::size_of::<T>() as u32,
            (value as *const T).cast(),
        )
    };
    if error != 0 {
        return Err(format!(
            "CoreAudio property {selector:#x} write failed ({error})"
        ));
    }
    Ok(())
}

fn default_output() -> Result<u32, String> {
    let device: u32 = get(SYSTEM_OBJECT, DEFAULT_OUTPUT, GLOBAL)?;
    if device == 0 {
        return Err("No default playback device is available".into());
    }
    Ok(device)
}

struct Snapshot {
    device: u32,
    volume: f32,
    muted: Option<u32>,
}

enum Command {
    Duck(Sender<Result<(), String>>),
    Restore(Sender<Result<(), String>>),
    Shutdown,
}

pub struct SystemAudioController {
    sender: Sender<Command>,
}

impl SystemAudioController {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("pronto-macos-system-audio".into())
            .spawn(move || {
                let mut snapshot = None;
                while let Ok(command) = receiver.recv() {
                    match command {
                        Command::Duck(reply) => {
                            let _ = reply.send(duck_endpoint(&mut snapshot));
                        }
                        Command::Restore(reply) => {
                            let _ = reply.send(restore_endpoint(&mut snapshot));
                        }
                        Command::Shutdown => break,
                    }
                }
                let _ = restore_endpoint(&mut snapshot);
            })
            .expect("failed to start macOS audio controller");
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
            .map_err(|_| "macOS audio controller stopped".to_string())?;
        response
            .recv()
            .map_err(|_| "macOS audio controller did not respond".to_string())?
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
    let device = default_output()?;
    let volume: f32 = get(device, VIRTUAL_MAIN_VOLUME, OUTPUT)?;
    let muted = get::<u32>(device, MUTE, OUTPUT).ok();
    if muted != Some(1) {
        set(device, VIRTUAL_MAIN_VOLUME, OUTPUT, &volume.min(DUCK_LEVEL))?;
    }
    *snapshot = Some(Snapshot {
        device,
        volume,
        muted,
    });
    Ok(())
}

fn restore_endpoint(snapshot: &mut Option<Snapshot>) -> Result<(), String> {
    let Some(saved) = snapshot.take() else {
        return Ok(());
    };
    set(saved.device, VIRTUAL_MAIN_VOLUME, OUTPUT, &saved.volume)?;
    if let Some(muted) = saved.muted {
        set(saved.device, MUTE, OUTPUT, &muted)?;
    }
    Ok(())
}

pub fn default_render_is_bluetooth() -> bool {
    default_output()
        .and_then(|device| get::<u32>(device, TRANSPORT, GLOBAL))
        .is_ok_and(|transport| transport == BLUETOOTH || transport == BLUETOOTH_LE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ducking_never_raises_volume() {
        assert_eq!(0.8f32.min(DUCK_LEVEL), DUCK_LEVEL);
        assert_eq!(0.04f32.min(DUCK_LEVEL), 0.04);
    }

    #[test]
    #[ignore = "changes the interactive desktop's playback volume"]
    fn real_endpoint_is_restored_exactly() {
        let device = default_output().unwrap();
        let before: f32 = get(device, VIRTUAL_MAIN_VOLUME, OUTPUT).unwrap();
        let controller = SystemAudioController::new();
        controller.duck().unwrap();
        controller.restore().unwrap();
        let after: f32 = get(device, VIRTUAL_MAIN_VOLUME, OUTPUT).unwrap();
        assert!((before - after).abs() < f32::EPSILON);
    }
}
