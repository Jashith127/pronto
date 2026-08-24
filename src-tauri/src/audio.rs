use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig, SupportedStreamConfig};
use serde::Serialize;
use std::sync::{mpsc, Arc, Mutex};
use windows::core::PSTR;
use windows::Win32::Media::Audio::{
    waveInAddBuffer, waveInClose, waveInOpen, waveInPrepareHeader, waveInReset, waveInStart,
    waveInStop, waveInUnprepareHeader, CALLBACK_NULL, HWAVEIN, WAVEFORMATEX, WAVEHDR,
    WAVE_FORMAT_PCM, WAVE_MAPPER,
};

pub struct Recording {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MicrophoneDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MicrophoneStatus {
    pub devices: Vec<MicrophoneDevice>,
    pub selected_id: Option<String>,
    pub active_id: String,
    pub active_name: String,
    pub fallback: bool,
}

#[derive(Clone, Debug)]
struct ActiveMicrophone {
    id: String,
    name: String,
    fallback: bool,
}

struct AudioCapture {
    samples: Arc<Mutex<Vec<f32>>>,
    stream: Option<Stream>,
    wave_in: Option<WaveInCapture>,
    sample_rate: u32,
    channels: u16,
    active: Option<ActiveMicrophone>,
}

impl Default for AudioCapture {
    fn default() -> Self {
        Self {
            samples: Arc::new(Mutex::new(Vec::new())),
            stream: None,
            wave_in: None,
            sample_rate: 0,
            channels: 0,
            active: None,
        }
    }
}

impl AudioCapture {
    pub fn prepare(&mut self, selected_id: Option<&str>) -> Result<ActiveMicrophone, String> {
        if self.stream.is_some() || self.wave_in.is_some() {
            return self
                .active
                .clone()
                .ok_or_else(|| "Prepared microphone is unavailable".to_string());
        }
        let host = cpal::default_host();
        let default_device = host
            .default_input_device()
            .ok_or_else(|| "No input device found".to_string())?;
        let default_id = device_id(&default_device)?;
        let (device, fallback) = match selected_id {
            Some(selected) => {
                let selected_device = host
                    .input_devices()
                    .map_err(|error| format!("Could not enumerate microphones: {error}"))?
                    .find(|device| device_id(device).is_ok_and(|id| id == selected));
                match selected_device {
                    Some(device) => (device, false),
                    None => (default_device, true),
                }
            }
            None => (default_device, false),
        };
        let active_id = device_id(&device)?;
        let device_name = device
            .description()
            .map(|description| description.name().to_owned())
            .unwrap_or_else(|_| "default microphone".into());
        let mut candidates = Vec::new();
        if let Ok(default) = device.default_input_config() {
            candidates.push(default);
        }
        if let Ok(ranges) = device.supported_input_configs() {
            for range in ranges {
                let preferred = [48_000, 44_100, 16_000].into_iter().find(|rate| {
                    *rate >= range.min_sample_rate() && *rate <= range.max_sample_rate()
                });
                candidates.push(match preferred {
                    Some(rate) => range.with_sample_rate(rate),
                    None => range.with_max_sample_rate(),
                });
            }
        }

        let mut failures = Vec::new();
        for supported in candidates {
            let description = format!(
                "{}ch {}Hz {:?}",
                supported.channels(),
                supported.sample_rate(),
                supported.sample_format()
            );
            match build_input_stream(&device, &supported, &self.samples) {
                Ok(stream) => {
                    self.sample_rate = supported.sample_rate();
                    self.channels = supported.channels();
                    self.stream = Some(stream);
                    let active = ActiveMicrophone {
                        id: active_id,
                        name: device_name,
                        fallback,
                    };
                    self.active = Some(active.clone());
                    return Ok(active);
                }
                Err(error) => failures.push(format!("{description}: {error}")),
            }
        }

        if active_id != default_id {
            return Err(format!(
                "Could not open {device_name}. Tried: {}",
                failures.join("; ")
            ));
        }

        match WaveInCapture::prepare() {
            Ok(capture) => {
                self.sample_rate = capture.sample_rate;
                self.channels = capture.channels;
                self.wave_in = Some(capture);
                let active = ActiveMicrophone {
                    id: active_id,
                    name: device_name,
                    fallback,
                };
                self.active = Some(active.clone());
                Ok(active)
            }
            Err(wave_error) => Err(format!(
                "Could not open {device_name}. WASAPI tried: {}. WinMM fallback: {wave_error}",
                failures.join("; ")
            )),
        }
    }

    pub fn start(&mut self) -> Result<ActiveMicrophone, String> {
        let active = self
            .active
            .clone()
            .ok_or_else(|| "Microphone is not prepared".to_string())?;
        self.samples
            .lock()
            .map_err(|_| "audio buffer poisoned")?
            .clear();
        if let Some(capture) = self.wave_in.as_mut() {
            capture.start()?;
            return Ok(active);
        }
        if let Some(stream) = self.stream.as_ref() {
            stream
                .play()
                .map_err(|error| format!("Could not resume the prewarmed microphone: {error}"))?;
        }
        Ok(active)
    }

    pub fn stop(&mut self) -> Result<Recording, String> {
        if let Some(capture) = self.wave_in.as_mut() {
            return capture.stop();
        }
        if let Some(stream) = self.stream.as_ref() {
            stream
                .pause()
                .map_err(|error| format!("Could not pause microphone: {error}"))?;
        }
        let samples =
            std::mem::take(&mut *self.samples.lock().map_err(|_| "audio buffer poisoned")?);
        Ok(Recording {
            samples,
            sample_rate: self.sample_rate,
            channels: self.channels,
        })
    }
}

/// Compatibility capture path for audio drivers that advertise a WASAPI mix
/// format but reject `IAudioClient::Initialize`. The system wave mapper handles
/// conversion to a simple PCM format and is available on every supported Windows
/// version.
struct WaveInCapture {
    handle: HWAVEIN,
    buffer: Vec<i16>,
    header: Box<WAVEHDR>,
    sample_rate: u32,
    channels: u16,
}

impl WaveInCapture {
    fn prepare() -> Result<Self, String> {
        const MAX_SECONDS: usize = 300;
        let mut errors = Vec::new();
        for (sample_rate, channels) in [(16_000u32, 1u16), (48_000, 1), (48_000, 2)] {
            let format = WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_PCM as u16,
                nChannels: channels,
                nSamplesPerSec: sample_rate,
                nAvgBytesPerSec: sample_rate * u32::from(channels) * 2,
                nBlockAlign: channels * 2,
                wBitsPerSample: 16,
                cbSize: 0,
            };
            let mut handle = HWAVEIN::default();
            let open_result = unsafe {
                waveInOpen(
                    Some(&mut handle),
                    WAVE_MAPPER,
                    &format,
                    Some(0),
                    Some(0),
                    CALLBACK_NULL,
                )
            };
            if open_result != 0 {
                errors.push(format!("{channels}ch {sample_rate}Hz open={open_result}"));
                continue;
            }

            let mut buffer = vec![0i16; sample_rate as usize * channels as usize * MAX_SECONDS];
            let mut header = Box::new(WAVEHDR {
                lpData: PSTR(buffer.as_mut_ptr().cast::<u8>()),
                dwBufferLength: (buffer.len() * std::mem::size_of::<i16>()) as u32,
                ..Default::default()
            });
            let header_size = std::mem::size_of::<WAVEHDR>() as u32;
            let prepare_result = unsafe { waveInPrepareHeader(handle, &mut *header, header_size) };
            if prepare_result == 0 {
                return Ok(Self {
                    handle,
                    buffer,
                    header,
                    sample_rate,
                    channels,
                });
            }

            unsafe {
                waveInUnprepareHeader(handle, &mut *header, header_size);
                waveInClose(handle);
            }
            errors.push(format!(
                "{channels}ch {sample_rate}Hz prepare={prepare_result}"
            ));
        }
        Err(errors.join("; "))
    }

    fn start(&mut self) -> Result<(), String> {
        self.header.dwBytesRecorded = 0;
        self.header.dwFlags &= !0x0000_0001;
        let header_size = std::mem::size_of::<WAVEHDR>() as u32;
        let add = unsafe { waveInAddBuffer(self.handle, &mut *self.header, header_size) };
        if add != 0 {
            return Err(format!(
                "Could not queue prewarmed microphone buffer ({add})"
            ));
        }
        let start = unsafe { waveInStart(self.handle) };
        if start != 0 {
            return Err(format!("Could not resume prewarmed microphone ({start})"));
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<Recording, String> {
        unsafe {
            waveInStop(self.handle);
            waveInReset(self.handle);
        }
        let sample_count = (self.header.dwBytesRecorded as usize / std::mem::size_of::<i16>())
            .min(self.buffer.len());
        let samples = self.buffer[..sample_count]
            .iter()
            .map(|&value| value as f32 / i16::MAX as f32)
            .collect();
        Ok(Recording {
            samples,
            sample_rate: self.sample_rate,
            channels: self.channels,
        })
    }
}

impl Drop for WaveInCapture {
    fn drop(&mut self) {
        if self.handle.is_invalid() {
            return;
        }
        let header_size = std::mem::size_of::<WAVEHDR>() as u32;
        unsafe {
            waveInReset(self.handle);
            waveInUnprepareHeader(self.handle, &mut *self.header, header_size);
            waveInClose(self.handle);
        }
    }
}

fn build_input_stream(
    device: &Device,
    supported: &SupportedStreamConfig,
    shared: &Arc<Mutex<Vec<f32>>>,
) -> Result<Stream, cpal::Error> {
    let config: StreamConfig = supported.clone().into();
    match supported.sample_format() {
        SampleFormat::F32 => {
            let buffer = Arc::clone(shared);
            device.build_input_stream(
                config,
                move |data: &[f32], _| append_f32(&buffer, data),
                |error| eprintln!("audio stream error: {error}"),
                None,
            )
        }
        SampleFormat::I16 => {
            let buffer = Arc::clone(shared);
            device.build_input_stream(
                config,
                move |data: &[i16], _| append_i16(&buffer, data),
                |error| eprintln!("audio stream error: {error}"),
                None,
            )
        }
        SampleFormat::U16 => {
            let buffer = Arc::clone(shared);
            device.build_input_stream(
                config,
                move |data: &[u16], _| append_u16(&buffer, data),
                |error| eprintln!("audio stream error: {error}"),
                None,
            )
        }
        format => {
            eprintln!("unsupported microphone sample format: {format:?}");
            Err(cpal::Error::new(cpal::ErrorKind::UnsupportedConfig))
        }
    }
}

enum AudioCommand {
    Start(mpsc::Sender<Result<ActiveMicrophone, String>>),
    Stop(mpsc::Sender<Result<Recording, String>>),
    Status(mpsc::Sender<Result<MicrophoneStatus, String>>),
    Select(
        Option<String>,
        mpsc::Sender<Result<MicrophoneStatus, String>>,
    ),
}

/// Owns CPAL on one dedicated thread. CPAL streams are deliberately not `Send`,
/// and keeping creation/destruction on the same thread also gives WASAPI a clean
/// home when the capture layer grows COM-specific features.
pub struct AudioController {
    commands: mpsc::Sender<AudioCommand>,
}

impl AudioController {
    pub fn new(selected_id: Option<String>) -> Self {
        let (commands, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("pronto-audio".into())
            .spawn(move || {
                let mut capture = AudioCapture::default();
                // Device enumeration, format negotiation, allocation, and stream
                // creation happen at app startup. Hotkey activation only clears
                // the buffer and resumes this already-open capture path.
                let mut selected_id = selected_id;
                let mut prepared = capture.prepare(selected_id.as_deref());
                let mut recording = false;
                while let Ok(command) = receiver.recv() {
                    match command {
                        AudioCommand::Start(reply) => {
                            let result = match prepared.as_ref() {
                                Ok(_) => capture.start(),
                                Err(error) => Err(error.clone()),
                            };
                            if result.is_ok() {
                                recording = true;
                            }
                            let _ = reply.send(result);
                        }
                        AudioCommand::Stop(reply) => {
                            let result = capture.stop();
                            recording = false;
                            let _ = reply.send(result);
                        }
                        AudioCommand::Status(reply) => {
                            let active = prepared.as_ref().ok();
                            let _ = reply.send(microphone_status(selected_id.clone(), active));
                        }
                        AudioCommand::Select(next_id, reply) => {
                            if recording {
                                let _ =
                                    reply
                                        .send(Err("Finish dictation before changing microphones"
                                            .to_string()));
                                continue;
                            }
                            let previous_id = selected_id.clone();
                            // Some Windows microphone drivers only allow one open
                            // capture handle. Release the prewarmed stream before
                            // opening the replacement, then restore it on failure.
                            capture = AudioCapture::default();
                            let mut next_capture = AudioCapture::default();
                            let result = match next_capture.prepare(next_id.as_deref()) {
                                Ok(active) => {
                                    capture = next_capture;
                                    selected_id = next_id;
                                    prepared = Ok(active.clone());
                                    microphone_status(selected_id.clone(), Some(&active))
                                }
                                Err(error) => {
                                    let mut restored = AudioCapture::default();
                                    match restored.prepare(previous_id.as_deref()) {
                                        Ok(active) => {
                                            capture = restored;
                                            prepared = Ok(active);
                                        }
                                        Err(restore_error) => {
                                            prepared = Err(format!(
                                                "{error}; previous microphone could not be restored: {restore_error}"
                                            ));
                                        }
                                    }
                                    Err(error)
                                }
                            };
                            let _ = reply.send(result);
                        }
                    }
                }
            })
            .expect("failed to start audio thread");
        Self { commands }
    }

    pub fn start(&self) -> Result<String, String> {
        let (reply, response) = mpsc::channel();
        self.commands
            .send(AudioCommand::Start(reply))
            .map_err(|_| "audio thread stopped".to_string())?;
        response
            .recv()
            .map_err(|_| "audio thread stopped".to_string())?
            .map(|active| active.name)
    }

    pub fn stop(&self) -> Result<Recording, String> {
        let (reply, response) = mpsc::channel();
        self.commands
            .send(AudioCommand::Stop(reply))
            .map_err(|_| "audio thread stopped".to_string())?;
        response
            .recv()
            .map_err(|_| "audio thread stopped".to_string())?
    }

    pub fn status(&self) -> Result<MicrophoneStatus, String> {
        let (reply, response) = mpsc::channel();
        self.commands
            .send(AudioCommand::Status(reply))
            .map_err(|_| "audio thread stopped".to_string())?;
        response
            .recv()
            .map_err(|_| "audio thread stopped".to_string())?
    }

    pub fn select(&self, device_id: Option<String>) -> Result<MicrophoneStatus, String> {
        let (reply, response) = mpsc::channel();
        self.commands
            .send(AudioCommand::Select(device_id, reply))
            .map_err(|_| "audio thread stopped".to_string())?;
        response
            .recv()
            .map_err(|_| "audio thread stopped".to_string())?
    }
}

fn device_id(device: &Device) -> Result<String, String> {
    device
        .id()
        .map(|id| id.to_string())
        .map_err(|error| format!("Could not identify microphone: {error}"))
}

fn microphone_status(
    selected_id: Option<String>,
    active: Option<&ActiveMicrophone>,
) -> Result<MicrophoneStatus, String> {
    let host = cpal::default_host();
    let default_id = host
        .default_input_device()
        .and_then(|device| device.id().ok())
        .map(|id| id.to_string());
    let mut devices = host
        .input_devices()
        .map_err(|error| format!("Could not enumerate microphones: {error}"))?
        .filter_map(|device| {
            let id = device.id().ok()?.to_string();
            let name = device.description().ok()?.name().to_owned();
            Some(MicrophoneDevice {
                is_default: default_id.as_deref() == Some(id.as_str()),
                id,
                name,
            })
        })
        .collect::<Vec<_>>();
    devices.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    devices.dedup_by(|left, right| left.id == right.id);
    let active_id = active.map(|device| device.id.clone()).unwrap_or_default();
    let active_name = active
        .map(|device| device.name.clone())
        .unwrap_or_else(|| "No microphone available".to_string());
    Ok(MicrophoneStatus {
        devices,
        selected_id,
        active_id,
        active_name,
        fallback: active.is_some_and(|device| device.fallback),
    })
}

fn append_f32(buffer: &Mutex<Vec<f32>>, data: &[f32]) {
    if let Ok(mut samples) = buffer.lock() {
        samples.extend_from_slice(data);
    }
}

fn append_i16(buffer: &Mutex<Vec<f32>>, data: &[i16]) {
    if let Ok(mut samples) = buffer.lock() {
        samples.extend(data.iter().map(|&value| value as f32 / i16::MAX as f32));
    }
}

fn append_u16(buffer: &Mutex<Vec<f32>>, data: &[u16]) {
    if let Ok(mut samples) = buffer.lock() {
        samples.extend(
            data.iter()
                .map(|&value| value as f32 / u16::MAX as f32 * 2.0 - 1.0),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Hardware validation for release checks. It is ignored during ordinary
    /// unit tests because CI machines may not expose a microphone.
    #[test]
    #[ignore = "requires a Windows microphone"]
    fn captures_from_default_microphone() {
        let audio = AudioController::new(None);
        audio.start().expect("default microphone should start");
        std::thread::sleep(Duration::from_millis(750));
        let recording = audio.stop().expect("microphone should stop cleanly");

        assert!(recording.sample_rate >= 16_000);
        assert!(recording.channels > 0);
        assert!(
            recording.samples.len()
                >= recording.sample_rate as usize * recording.channels as usize / 2,
            "expected at least half a second of captured samples"
        );
    }

    #[test]
    #[ignore = "requires a Windows microphone"]
    fn repeated_activation_uses_prewarmed_microphone() {
        use std::time::Instant;
        let audio = AudioController::new(None);
        audio.start().expect("prewarmed microphone should start");
        std::thread::sleep(Duration::from_millis(100));
        let _ = audio.stop().unwrap();
        let start = Instant::now();
        audio.start().expect("microphone should resume");
        let resume_ms = start.elapsed().as_millis();
        println!("prewarmed microphone resumed in {resume_ms} ms");
        std::thread::sleep(Duration::from_millis(100));
        let recording = audio.stop().unwrap();
        assert!(resume_ms < 75, "prewarmed resume took {resume_ms} ms");
        assert!(!recording.samples.is_empty());
    }

    #[test]
    #[ignore = "requires a Windows microphone"]
    fn enumerates_and_selects_microphone() {
        let audio = AudioController::new(None);
        let status = audio.status().expect("microphones should enumerate");
        assert!(!status.devices.is_empty());
        let selected = status.devices[0].clone();
        let changed = audio
            .select(Some(selected.id.clone()))
            .expect("microphone should prewarm");
        assert_eq!(changed.selected_id.as_deref(), Some(selected.id.as_str()));
        assert_eq!(changed.active_id, selected.id);
    }
}
