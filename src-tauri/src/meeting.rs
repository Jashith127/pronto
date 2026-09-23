use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
};
#[cfg(windows)]
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};

const TARGET_RATE: u32 = 16_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingRecord {
    pub id: String,
    pub title: String,
    pub created_at: u64,
    pub duration_seconds: u64,
    pub status: String,
    pub audio_path: Option<String>,
    pub transcript: String,
    pub notes: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingStatus {
    pub recording: bool,
    pub meeting: Option<MeetingRecord>,
    pub elapsed_seconds: u64,
}

pub struct StoppedMeeting {
    pub record: MeetingRecord,
    microphone_writer: Option<std::thread::JoinHandle<Result<(), String>>>,
    system_writer: Option<std::thread::JoinHandle<Result<(), String>>>,
    directory: PathBuf,
}

impl StoppedMeeting {
    /// Wait for file headers and buffered audio to finish on the finalization
    /// worker, after Stop has already returned and the meeting IPC is free.
    pub fn finish_capture(mut self) -> Result<MeetingRecord, String> {
        let microphone_result = self.microphone_writer.take().map(|writer| {
            writer
                .join()
                .map_err(|_| "Microphone writer stopped unexpectedly".to_string())?
        });
        let system_error = self
            .system_writer
            .take()
            .and_then(|writer| match writer.join() {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error),
                Err(_) => Some("Computer audio writer stopped unexpectedly".into()),
            });
        if let Some(result) = microphone_result {
            result?;
        }
        self.record.error =
            system_error.map(|error| format!("Computer audio was unavailable: {error}"));
        save_record(&self.directory, &self.record)?;
        Ok(self.record)
    }
}

enum Command {
    Start(
        String,
        Option<String>,
        mpsc::Sender<Result<MeetingRecord, String>>,
    ),
    Stop(mpsc::Sender<Result<StoppedMeeting, String>>),
    Status(mpsc::Sender<Result<MeetingStatus, String>>),
    List(mpsc::Sender<Result<Vec<MeetingRecord>, String>>),
}

pub struct MeetingController {
    sender: mpsc::Sender<Command>,
    active: Arc<AtomicBool>,
}

impl MeetingController {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        let active = Arc::new(AtomicBool::new(false));
        let worker_active = Arc::clone(&active);
        std::thread::Builder::new()
            .name("pronto-meetings".into())
            .spawn(move || meeting_worker(receiver, worker_active))
            .expect("failed to start meeting controller");
        Self { sender, active }
    }

    pub fn activity_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.active)
    }

    pub fn start(
        &self,
        title: String,
        microphone_id: Option<String>,
    ) -> Result<MeetingRecord, String> {
        request(&self.sender, |reply| {
            Command::Start(title, microphone_id, reply)
        })
    }

    pub fn stop(&self) -> Result<StoppedMeeting, String> {
        request(&self.sender, Command::Stop)
    }

    pub fn status(&self) -> Result<MeetingStatus, String> {
        request(&self.sender, Command::Status)
    }

    pub fn list(&self) -> Result<Vec<MeetingRecord>, String> {
        request(&self.sender, Command::List)
    }
}

fn request<T>(
    sender: &mpsc::Sender<Command>,
    build: impl FnOnce(mpsc::Sender<Result<T, String>>) -> Command,
) -> Result<T, String> {
    let (reply, response) = mpsc::channel();
    sender
        .send(build(reply))
        .map_err(|_| "Meeting recorder stopped".to_string())?;
    response
        .recv()
        .map_err(|_| "Meeting recorder did not respond".to_string())?
}

struct ActiveMeeting {
    record: MeetingRecord,
    started: Instant,
    stop: Arc<AtomicBool>,
    microphone: Option<Stream>,
    microphone_writer: Option<std::thread::JoinHandle<Result<(), String>>>,
    system_writer: Option<std::thread::JoinHandle<Result<(), String>>>,
    directory: PathBuf,
}

fn meeting_worker(receiver: mpsc::Receiver<Command>, active: Arc<AtomicBool>) {
    let _ = recover_interrupted_meetings();
    let mut current: Option<ActiveMeeting> = None;
    while let Ok(command) = receiver.recv() {
        match command {
            Command::Start(title, microphone_id, reply) => {
                if current.is_some() {
                    let _ = reply.send(Err("A meeting is already being recorded".into()));
                    continue;
                }
                let result = start_capture(title, microphone_id);
                match result {
                    Ok(meeting) => {
                        active.store(true, Ordering::Release);
                        let record = meeting.record.clone();
                        current = Some(meeting);
                        let _ = reply.send(Ok(record));
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            Command::Stop(reply) => {
                let result = current
                    .take()
                    .ok_or_else(|| "No meeting is being recorded".to_string())
                    .and_then(stop_capture);
                active.store(false, Ordering::Release);
                let _ = reply.send(result);
            }
            Command::Status(reply) => {
                let status = MeetingStatus {
                    recording: current.is_some(),
                    meeting: current.as_ref().map(|meeting| meeting.record.clone()),
                    elapsed_seconds: current
                        .as_ref()
                        .map(|meeting| meeting.started.elapsed().as_secs())
                        .unwrap_or(0),
                };
                let _ = reply.send(Ok(status));
            }
            Command::List(reply) => {
                let _ = reply.send(list_records());
            }
        }
    }
}

fn start_capture(title: String, microphone_id: Option<String>) -> Result<ActiveMeeting, String> {
    let created_at = now_ms();
    let id = format!("meeting-{created_at}");
    let directory = meetings_root().join(&id);
    fs::create_dir_all(&directory)
        .map_err(|error| format!("Could not create meeting folder: {error}"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let mic_path = directory.join("microphone.wav");
    let system_path = directory.join("computer.wav");
    let record = MeetingRecord {
        id,
        title: normalized_title(&title),
        created_at,
        duration_seconds: 0,
        status: "recording".into(),
        audio_path: None,
        transcript: String::new(),
        notes: String::new(),
        error: None,
    };
    save_record(&directory, &record)?;
    let (microphone, microphone_writer) =
        match start_microphone_capture(&mic_path, microphone_id.as_deref(), Arc::clone(&stop)) {
            Ok(capture) => capture,
            Err(error) => {
                let _ = mark_error(&record.id, error.clone());
                return Err(error);
            }
        };
    let system_writer = start_system_capture(system_path, Arc::clone(&stop));
    Ok(ActiveMeeting {
        record,
        started: Instant::now(),
        stop,
        microphone: Some(microphone),
        microphone_writer: Some(microphone_writer),
        system_writer: Some(system_writer),
        directory,
    })
}

fn stop_capture(mut active: ActiveMeeting) -> Result<StoppedMeeting, String> {
    active.stop.store(true, Ordering::Release);
    if let Some(stream) = active.microphone.take() {
        let _ = stream.pause();
        drop(stream);
    }
    // File flush, ScreenCaptureKit shutdown, and mixing continue after this
    // reply, so Stop never queues status/list behind a slow capture teardown.
    active.record.duration_seconds = active.started.elapsed().as_secs();
    active.record.status = "processing".into();
    active.record.audio_path = None;
    save_record(&active.directory, &active.record)?;
    Ok(StoppedMeeting {
        record: active.record,
        microphone_writer: active.microphone_writer,
        system_writer: active.system_writer,
        directory: active.directory,
    })
}

/// Mix the stopped microphone + computer captures into meeting.wav.
/// Runs on a background thread (never the meeting worker or a Tauri
/// command), so status/list stay instant while it works.
pub fn finalize_meeting(id: &str) -> Result<MeetingRecord, String> {
    if id.trim().is_empty() || id.contains(['/', '\\', '.']) {
        return Err("Invalid recording identifier.".into());
    }
    let directory = meetings_root().join(id);
    let mixed = directory.join("meeting.wav");
    mix_sources(
        &directory.join("microphone.wav"),
        &directory.join("computer.wav"),
        &mixed,
    )?;
    let path = directory.join("meeting.json");
    let mut record: MeetingRecord =
        serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    record.audio_path = Some(mixed.to_string_lossy().to_string());
    save_record(&directory, &record)?;
    Ok(record)
}

fn start_microphone_capture(
    path: &Path,
    selected_id: Option<&str>,
    stop: Arc<AtomicBool>,
) -> Result<(Stream, std::thread::JoinHandle<Result<(), String>>), String> {
    let host = cpal::default_host();
    let default = host
        .default_input_device()
        .ok_or_else(|| "No microphone is available".to_string())?;
    let device = selected_id
        .and_then(|selected| {
            host.input_devices().ok()?.find(|device| {
                device
                    .id()
                    .map(|id| id.to_string() == selected)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(default);
    let supported = device
        .default_input_config()
        .map_err(|error| format!("Could not open microphone: {error}"))?;
    let rate = supported.sample_rate();
    let channels = supported.channels() as usize;
    let (sender, receiver) = mpsc::sync_channel::<Vec<f32>>(16);
    let config: StreamConfig = supported.into();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => device.build_input_stream(
            config,
            move |data: &[f32], _| {
                let _ = sender.try_send(data.to_vec());
            },
            move |error| eprintln!("meeting microphone stream error: {error}"),
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _| {
                let _ = sender.try_send(data.iter().map(|v| *v as f32 / 32768.0).collect());
            },
            move |error| eprintln!("meeting microphone stream error: {error}"),
            None,
        ),
        SampleFormat::U16 => device.build_input_stream(
            config,
            move |data: &[u16], _| {
                let _ = sender.try_send(data.iter().map(|v| *v as f32 / 32768.0 - 1.0).collect());
            },
            move |error| eprintln!("meeting microphone stream error: {error}"),
            None,
        ),
        format => return Err(format!("Unsupported microphone format: {format:?}")),
    }
    .map_err(|error| format!("Could not prepare microphone: {error}"))?;
    let path = path.to_path_buf();
    let writer = std::thread::Builder::new()
        .name("pronto-meeting-mic-writer".into())
        .spawn(move || {
            let mut wav = WavWriter::create(&path)?;
            let mut reducer = RateReducer::new(rate, TARGET_RATE);
            loop {
                match receiver.recv_timeout(Duration::from_millis(40)) {
                    Ok(samples) if !samples.is_empty() => {
                        for frame in samples.chunks(channels) {
                            let mono =
                                frame.iter().copied().sum::<f32>() / frame.len().max(1) as f32;
                            if let Some(value) = reducer.push(mono) {
                                wav.write_sample(value)?;
                            }
                        }
                    }
                    Ok(_) => continue,
                    Err(mpsc::RecvTimeoutError::Timeout) if stop.load(Ordering::Acquire) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            wav.finish()
        })
        .map_err(|error| error.to_string())?;
    stream
        .play()
        .map_err(|error| format!("Could not start microphone: {error}"))?;
    Ok((stream, writer))
}

#[cfg(windows)]
fn start_system_capture(
    path: PathBuf,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Result<(), String>> {
    std::thread::Builder::new()
        .name("pronto-meeting-loopback".into())
        .spawn(move || capture_system_audio(&path, &stop))
        .expect("failed to start system audio capture")
}

#[cfg(target_os = "macos")]
fn start_system_capture(
    path: PathBuf,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Result<(), String>> {
    std::thread::Builder::new()
        .name("pronto-meeting-screen-audio".into())
        .spawn(move || capture_screen_audio(&path, &stop))
        .expect("failed to start computer audio capture")
}

#[cfg(target_os = "macos")]
struct ScreenAudioWriter {
    wav: WavWriter,
    reducer: RateReducer,
    rate: u32,
    error: Option<String>,
}

#[cfg(target_os = "macos")]
struct ScreenAudioContext<'a> {
    stop: &'a AtomicBool,
    writer: std::sync::Mutex<ScreenAudioWriter>,
}

#[cfg(target_os = "macos")]
extern "C" fn screen_audio_should_stop(context: *mut std::ffi::c_void) -> bool {
    let context = unsafe { &*(context as *const ScreenAudioContext<'_>) };
    context.stop.load(Ordering::Acquire)
}

#[cfg(target_os = "macos")]
extern "C" fn screen_audio_samples(
    samples: *const f32,
    count: usize,
    rate: u32,
    context: *mut std::ffi::c_void,
) {
    if samples.is_null() || count == 0 || rate == 0 {
        return;
    }
    let context = unsafe { &*(context as *const ScreenAudioContext<'_>) };
    if let Ok(mut writer) = context.writer.lock() {
        if writer.error.is_some() {
            return;
        }
        if writer.rate != rate {
            writer.reducer = RateReducer::new(rate, TARGET_RATE);
            writer.rate = rate;
        }
        for &sample in unsafe { std::slice::from_raw_parts(samples, count) } {
            if let Some(value) = writer.reducer.push(sample) {
                if let Err(error) = writer.wav.write_sample(value) {
                    writer.error = Some(error);
                    break;
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn pronto_capture_screen_audio(
        should_stop: extern "C" fn(*mut std::ffi::c_void) -> bool,
        receive: extern "C" fn(*const f32, usize, u32, *mut std::ffi::c_void),
        context: *mut std::ffi::c_void,
        error: *mut std::ffi::c_char,
        error_capacity: usize,
    ) -> i32;
}

#[cfg(target_os = "macos")]
fn capture_screen_audio(path: &Path, stop: &AtomicBool) -> Result<(), String> {
    let mut context = ScreenAudioContext {
        stop,
        writer: std::sync::Mutex::new(ScreenAudioWriter {
            wav: WavWriter::create(path)?,
            reducer: RateReducer::new(48_000, TARGET_RATE),
            rate: 48_000,
            error: None,
        }),
    };
    let mut error = [0i8; 512];
    let result = unsafe {
        pronto_capture_screen_audio(
            screen_audio_should_stop,
            screen_audio_samples,
            (&mut context as *mut ScreenAudioContext<'_>).cast(),
            error.as_mut_ptr(),
            error.len(),
        )
    };
    let writer = context.writer.into_inner().map_err(|e| e.to_string())?;
    writer.wav.finish()?;
    if let Some(error) = writer.error {
        return Err(error);
    }
    if result != 0 {
        return Err(unsafe { std::ffi::CStr::from_ptr(error.as_ptr()) }
            .to_string_lossy()
            .into_owned());
    }
    Ok(())
}

#[cfg(windows)]
fn capture_system_audio(path: &Path, stop: &AtomicBool) -> Result<(), String> {
    let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() };
    if !initialized {
        return Err("Windows audio capture could not initialize".into());
    }
    let result = (|| unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| e.to_string())?;
        let client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| e.to_string())?;
        let format = client.GetMixFormat().map_err(|e| e.to_string())?;
        let rate = (*format).nSamplesPerSec;
        let channels = (*format).nChannels as usize;
        let bits = (*format).wBitsPerSample;
        let tag = (*format).wFormatTag;
        let encoding = if tag == 0xfffe && (*format).cbSize >= 22 {
            std::ptr::read_unaligned((format.cast::<u8>().add(24)).cast::<u32>())
        } else {
            tag as u32
        };
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                10_000_000,
                0,
                format,
                None,
            )
            .map_err(|e| e.to_string())?;
        let capture: IAudioCaptureClient = client.GetService().map_err(|e| e.to_string())?;
        CoTaskMemFree(Some(format.cast()));
        let mut wav = WavWriter::create(path)?;
        let mut reducer = RateReducer::new(rate, TARGET_RATE);
        client.Start().map_err(|e| e.to_string())?;
        while !stop.load(Ordering::Acquire) {
            while capture.GetNextPacketSize().map_err(|e| e.to_string())? > 0 {
                let mut data = std::ptr::null_mut();
                let mut frames = 0;
                let mut flags = 0;
                capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    .map_err(|e| e.to_string())?;
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                for frame in 0..frames as usize {
                    let mono = if silent {
                        0.0
                    } else {
                        read_mono_frame(data, frame, channels, bits, encoding)
                    };
                    if let Some(value) = reducer.push(mono) {
                        wav.write_sample(value)?;
                    }
                }
                capture.ReleaseBuffer(frames).map_err(|e| e.to_string())?;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = client.Stop();
        wav.finish()
    })();
    unsafe {
        CoUninitialize();
    }
    result.map_err(|error: String| format!("Could not capture computer audio: {error}"))
}

#[cfg(windows)]
unsafe fn read_mono_frame(
    data: *mut u8,
    frame: usize,
    channels: usize,
    bits: u16,
    encoding: u32,
) -> f32 {
    let mut sum = 0.0;
    for channel in 0..channels {
        let index = frame * channels + channel;
        let value = match (encoding, bits) {
            (3, 32) => *(data.cast::<f32>().add(index)),
            (1, 16) => *(data.cast::<i16>().add(index)) as f32 / 32768.0,
            (1, 24) => {
                let p = data.add(index * 3);
                let raw = ((*p as i32) | ((*p.add(1) as i32) << 8) | ((*p.add(2) as i32) << 16))
                    << 8
                    >> 8;
                raw as f32 / 8_388_608.0
            }
            (1, 32) => *(data.cast::<i32>().add(index)) as f32 / 2_147_483_648.0,
            _ => 0.0,
        };
        sum += value;
    }
    sum / channels.max(1) as f32
}

struct RateReducer {
    input_rate: u32,
    output_rate: u32,
    phase: u32,
    sum: f32,
    count: u32,
}
impl RateReducer {
    fn new(input_rate: u32, output_rate: u32) -> Self {
        Self {
            input_rate,
            output_rate,
            phase: 0,
            sum: 0.0,
            count: 0,
        }
    }
    fn push(&mut self, sample: f32) -> Option<f32> {
        self.sum += sample;
        self.count += 1;
        self.phase += self.output_rate;
        if self.phase >= self.input_rate {
            self.phase -= self.input_rate;
            let value = self.sum / self.count as f32;
            self.sum = 0.0;
            self.count = 0;
            Some(value)
        } else {
            None
        }
    }
}

struct WavWriter {
    file: BufWriter<File>,
    samples: u32,
}
impl WavWriter {
    fn create(path: &Path) -> Result<Self, String> {
        let mut file = BufWriter::new(File::create(path).map_err(|e| e.to_string())?);
        file.write_all(&wav_header(0)).map_err(|e| e.to_string())?;
        Ok(Self { file, samples: 0 })
    }
    fn write_sample(&mut self, value: f32) -> Result<(), String> {
        let sample = (value.clamp(-1.0, 1.0) * 32767.0) as i16;
        self.file
            .write_all(&sample.to_le_bytes())
            .map_err(|e| e.to_string())?;
        self.samples = self.samples.saturating_add(1);
        Ok(())
    }
    fn finish(mut self) -> Result<(), String> {
        self.file.flush().map_err(|e| e.to_string())?;
        let mut file = self.file.into_inner().map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        file.write_all(&wav_header(self.samples))
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())
    }
}

fn wav_header(samples: u32) -> [u8; 44] {
    let data_len = samples.saturating_mul(2);
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36u32.saturating_add(data_len)).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&1u16.to_le_bytes());
    h[24..28].copy_from_slice(&TARGET_RATE.to_le_bytes());
    h[28..32].copy_from_slice(&(TARGET_RATE * 2).to_le_bytes());
    h[32..34].copy_from_slice(&2u16.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

fn mix_sources(microphone: &Path, computer: &Path, output: &Path) -> Result<(), String> {
    const BLOCK_SAMPLES: usize = 65536;
    let mut mic = open_wav_data(microphone)?;
    let mut system = open_wav_data(computer).ok();
    let mut writer = WavWriter::create(output)?;
    let mut a_bytes = vec![0u8; BLOCK_SAMPLES * 2];
    let mut b_bytes = vec![0u8; BLOCK_SAMPLES * 2];
    loop {
        let a_count = read_block(&mut mic, &mut a_bytes)?;
        let b_count = match system.as_mut() {
            Some(reader) => read_block(reader, &mut b_bytes)?,
            None => 0,
        };
        if a_count == 0 && b_count == 0 {
            break;
        }
        for index in 0..a_count.max(b_count) {
            let a = (index < a_count)
                .then(|| sample_at(&a_bytes, index))
                .flatten();
            let b = (index < b_count)
                .then(|| sample_at(&b_bytes, index))
                .flatten();
            let mixed = match (a, b) {
                (Some(a), Some(b)) => (a + b) * 0.5,
                (Some(a), None) => a,
                (None, Some(b)) => b,
                _ => 0.0,
            };
            writer.write_sample(mixed)?;
        }
    }
    writer.finish()
}

fn open_wav_data(path: &Path) -> Result<BufReader<File>, String> {
    let mut reader = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    reader
        .seek(SeekFrom::Start(44))
        .map_err(|e| e.to_string())?;
    Ok(reader)
}
/// Fill `buffer` with raw PCM16 bytes, returning the sample count.
fn read_block(reader: &mut BufReader<File>, buffer: &mut [u8]) -> Result<usize, String> {
    let even_len = buffer.len() - (buffer.len() % 2);
    let mut filled = 0;
    while filled < even_len {
        match reader.read(&mut buffer[filled..even_len]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(filled / 2)
}

fn sample_at(block: &[u8], index: usize) -> Option<f32> {
    let bytes = block.get(index * 2..index * 2 + 2)?;
    Some(i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 32768.0)
}

pub fn update_record(
    id: &str,
    transcript: String,
    notes: String,
    error: Option<String>,
) -> Result<MeetingRecord, String> {
    let directory = meetings_root().join(id);
    let path = directory.join("meeting.json");
    let mut record: MeetingRecord =
        serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    record.transcript = transcript;
    record.notes = notes;
    record.error = preserve_capture_warning(record.error.take(), error);
    record.status = if record.transcript.is_empty() {
        "error".into()
    } else {
        "ready".into()
    };
    save_record(&directory, &record)?;
    Ok(record)
}

fn preserve_capture_warning(
    existing: Option<String>,
    processing: Option<String>,
) -> Option<String> {
    let capture_warning =
        existing.filter(|message| message.starts_with("Computer audio was unavailable:"));
    match (capture_warning, processing) {
        (Some(capture), Some(processing)) => Some(format!("{capture} {processing}")),
        (Some(capture), None) => Some(capture),
        (None, processing) => processing,
    }
}

pub fn mark_error(id: &str, error: String) -> Result<MeetingRecord, String> {
    let directory = meetings_root().join(id);
    let path = directory.join("meeting.json");
    let mut record: MeetingRecord =
        serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    record.status = "error".into();
    record.error = Some(error);
    save_record(&directory, &record)?;
    Ok(record)
}

pub fn rename_record(id: &str, title: &str) -> Result<MeetingRecord, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("Enter a name for this recording.".into());
    }
    let directory = meetings_root().join(id);
    let path = directory.join("meeting.json");
    let mut record: MeetingRecord =
        serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    record.title = title.chars().take(120).collect();
    save_record(&directory, &record)?;
    Ok(record)
}

pub fn delete_record(id: &str) -> Result<(), String> {
    if id.trim().is_empty() || id.contains(['/', '\\', '.']) {
        return Err("Invalid recording identifier.".into());
    }
    let directory = meetings_root().join(id);
    if !directory.join("meeting.json").is_file() {
        return Err("That recording was not found.".into());
    }
    fs::remove_dir_all(&directory).map_err(|e| e.to_string())
}

pub fn record_for_retry(id: &str) -> Result<(MeetingRecord, PathBuf), String> {
    let directory = meetings_root().join(id);
    let path = directory.join("meeting.json");
    let mut record: MeetingRecord =
        serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let audio = directory.join("meeting.wav");
    if !audio.is_file() {
        return Err("The saved audio for this meeting is missing, so it cannot be retried.".into());
    }
    record.status = "processing".into();
    record.error = None;
    save_record(&directory, &record)?;
    Ok((record, audio))
}

pub fn notetaker_audio_root() -> PathBuf {
    crate::platform_paths::data_dir().join("NoteTakerAudio")
}

pub fn notetaker_audio_path(item_id: &str) -> Option<PathBuf> {
    if item_id.trim().is_empty() || item_id.len() > 128 || item_id.contains(['/', '\\', '.', ':']) {
        return None;
    }
    Some(notetaker_audio_root().join(format!("{item_id}.wav")))
}

pub fn save_notetaker_audio(item_id: &str, bytes: &[u8]) -> Result<(), String> {
    let path =
        notetaker_audio_path(item_id).ok_or_else(|| "Invalid recording identifier.".to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    save_notetaker_audio_at(&path, bytes)
}

fn save_notetaker_audio_at(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("wav.tmp");
    {
        let mut file = File::create(&temporary).map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
    }
    fs::rename(&temporary, path).map_err(|e| e.to_string())
}

pub fn delete_notetaker_audio(item_id: &str) -> Result<(), String> {
    if let Some(path) = notetaker_audio_path(item_id) {
        let _ = fs::remove_file(path);
    }
    Ok(())
}
fn save_record(directory: &Path, record: &MeetingRecord) -> Result<(), String> {
    let path = directory.join("meeting.json");
    let temporary = directory.join("meeting.json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    fs::rename(&temporary, &path).map_err(|e| e.to_string())
}
fn list_records() -> Result<Vec<MeetingRecord>, String> {
    let root = meetings_root();
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let mut records = Vec::new();
    for entry in fs::read_dir(root).map_err(|e| e.to_string())?.flatten() {
        if let Ok(bytes) = fs::read(entry.path().join("meeting.json")) {
            if let Ok(record) = serde_json::from_slice(&bytes) {
                records.push(record);
            }
        }
    }
    records.sort_by_key(|record: &MeetingRecord| std::cmp::Reverse(record.created_at));
    Ok(records)
}

fn recover_interrupted_meetings() -> Result<(), String> {
    let root = meetings_root();
    recover_interrupted_meetings_at(&root)
}

fn recover_interrupted_meetings_at(root: &Path) -> Result<(), String> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root).map_err(|e| e.to_string())?.flatten() {
        let directory = entry.path();
        let record_path = directory.join("meeting.json");
        let Ok(bytes) = fs::read(&record_path) else {
            continue;
        };
        let Ok(mut record) = serde_json::from_slice::<MeetingRecord>(&bytes) else {
            continue;
        };
        if record.status != "recording"
            && !(record.status == "processing" && record.audio_path.is_none())
        {
            continue;
        }
        let microphone = directory.join("microphone.wav");
        let computer = directory.join("computer.wav");
        let mixed = directory.join("meeting.wav");
        let _ = repair_wav_header(&microphone);
        let _ = repair_wav_header(&computer);
        if mix_sources(&microphone, &computer, &mixed).is_ok() {
            record.audio_path = Some(mixed.to_string_lossy().to_string());
            record.status = "interrupted".into();
            record.error = Some("Pronto closed before this recording was stopped. The captured audio was recovered.".into());
        } else {
            record.status = "error".into();
            record.error = Some("Pronto closed before this recording could be finalized.".into());
        }
        let _ = save_record(&directory, &record);
    }
    Ok(())
}

fn repair_wav_header(path: &Path) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    if length < 44 {
        return Err("Incomplete WAV file".into());
    }
    let samples = ((length - 44) / 2).min(u32::MAX as u64) as u32;
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    file.write_all(&wav_header(samples))
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}
fn meetings_root() -> PathBuf {
    crate::platform_paths::data_dir().join("Meetings")
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn normalized_title(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        "Untitled meeting".into()
    } else {
        title.chars().take(120).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_describes_mono_pcm16() {
        let header = wav_header(16_000);
        assert_eq!(&header[0..4], b"RIFF");
        assert_eq!(&header[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes(header[22..24].try_into().unwrap()), 1);
        assert_eq!(
            u32::from_le_bytes(header[24..28].try_into().unwrap()),
            16_000
        );
        assert_eq!(
            u32::from_le_bytes(header[40..44].try_into().unwrap()),
            32_000
        );
    }

    #[test]
    fn reducer_produces_target_sample_count() {
        let mut reducer = RateReducer::new(48_000, 16_000);
        let produced = (0..48_000).filter_map(|_| reducer.push(0.25)).count();
        assert_eq!(produced, 16_000);
    }

    #[test]
    fn notetaker_audio_save_preserves_previous_file_on_failure() {
        let directory = std::env::temp_dir().join(format!(
            "pronto-notetaker-atomic-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("upload.wav");
        save_notetaker_audio_at(&path, b"old audio").unwrap();
        let temporary = path.with_extension("wav.tmp");
        fs::create_dir(&temporary).unwrap();
        assert!(save_notetaker_audio_at(&path, b"replacement").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old audio");
        fs::remove_dir(&temporary).unwrap();
        save_notetaker_audio_at(&path, b"replacement").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn stop_returns_before_capture_writers_finish() {
        let directory = std::env::temp_dir().join(format!(
            "pronto-stop-timing-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&directory).unwrap();
        let writer = || {
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(500));
                Ok(())
            })
        };
        let active = ActiveMeeting {
            record: MeetingRecord {
                id: "test".into(),
                title: "Test".into(),
                created_at: now_ms(),
                duration_seconds: 0,
                status: "recording".into(),
                audio_path: None,
                transcript: String::new(),
                notes: String::new(),
                error: None,
            },
            started: Instant::now(),
            stop: Arc::new(AtomicBool::new(false)),
            microphone: None,
            microphone_writer: Some(writer()),
            system_writer: Some(writer()),
            directory: directory.clone(),
        };
        let started = Instant::now();
        let stopped = stop_capture(active).unwrap();
        assert!(started.elapsed() < Duration::from_millis(250));
        assert_eq!(stopped.record.status, "processing");
        assert!(stopped.finish_capture().is_ok());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn successful_notes_keep_a_computer_audio_failure_visible() {
        let warning = preserve_capture_warning(
            Some("Computer audio was unavailable: Screen Recording denied".into()),
            Some("Local notes used after network failure".into()),
        )
        .unwrap();
        assert!(warning.contains("Screen Recording denied"));
        assert!(warning.contains("Local notes used"));
    }

    #[test]
    fn startup_recovers_a_recording_interrupted_during_finalization() {
        let root = std::env::temp_dir().join(format!(
            "pronto-recover-processing-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let directory = root.join("meeting-test");
        fs::create_dir_all(&directory).unwrap();
        let record = MeetingRecord {
            id: "meeting-test".into(),
            title: "Interrupted".into(),
            created_at: now_ms(),
            duration_seconds: 10,
            status: "processing".into(),
            audio_path: None,
            transcript: String::new(),
            notes: String::new(),
            error: None,
        };
        save_record(&directory, &record).unwrap();
        let mut microphone = WavWriter::create(&directory.join("microphone.wav")).unwrap();
        microphone.write_sample(0.5).unwrap();
        microphone.finish().unwrap();
        // A process killed mid-recording can leave a stale WAV size/header.
        let mut stale = OpenOptions::new()
            .write(true)
            .open(directory.join("microphone.wav"))
            .unwrap();
        stale.write_all(&[0u8; 44]).unwrap();
        recover_interrupted_meetings_at(&root).unwrap();
        let recovered: MeetingRecord =
            serde_json::from_slice(&fs::read(directory.join("meeting.json")).unwrap()).unwrap();
        assert_eq!(recovered.status, "interrupted");
        assert!(recovered
            .audio_path
            .as_ref()
            .is_some_and(|path| Path::new(path).is_file()));
        assert!(recovered.error.unwrap().contains("recovered"));
        let _ = fs::remove_dir_all(root);
    }
}
