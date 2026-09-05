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
    pub audio_path: PathBuf,
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
    if let Some(writer) = active.microphone_writer.take() {
        writer
            .join()
            .map_err(|_| "Microphone writer stopped unexpectedly".to_string())??;
    }
    let system_error = active
        .system_writer
        .take()
        .and_then(|writer| writer.join().ok())
        .and_then(Result::err);
    let mixed = active.directory.join("meeting.wav");
    mix_sources(
        &active.directory.join("microphone.wav"),
        &active.directory.join("computer.wav"),
        &mixed,
    )?;
    active.record.duration_seconds = active.started.elapsed().as_secs();
    active.record.status = "processing".into();
    active.record.audio_path = Some(mixed.to_string_lossy().to_string());
    active.record.error =
        system_error.map(|error| format!("Computer audio was unavailable: {error}"));
    save_record(&active.directory, &active.record)?;
    Ok(StoppedMeeting {
        record: active.record,
        audio_path: mixed,
    })
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
    let config: StreamConfig = supported.clone().into();
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

#[cfg(not(windows))]
fn start_system_capture(
    _: PathBuf,
    _: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Result<(), String>> {
    std::thread::spawn(|| Err("Computer audio capture is available only on Windows".into()))
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
    let mut mic = open_wav_data(microphone)?;
    let mut system = open_wav_data(computer).ok();
    let mut writer = WavWriter::create(output)?;
    loop {
        let a = read_sample(&mut mic);
        let b = system.as_mut().and_then(read_sample);
        if a.is_none() && b.is_none() {
            break;
        }
        let mixed = match (a, b) {
            (Some(a), Some(b)) => (a + b) * 0.5,
            (Some(a), None) => a,
            (None, Some(b)) => b,
            _ => 0.0,
        };
        writer.write_sample(mixed)?;
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
fn read_sample(reader: &mut BufReader<File>) -> Option<f32> {
    let mut bytes = [0u8; 2];
    reader.read_exact(&mut bytes).ok()?;
    Some(i16::from_le_bytes(bytes) as f32 / 32768.0)
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
    record.error = error;
    record.status = if record.transcript.is_empty() {
        "error".into()
    } else {
        "ready".into()
    };
    save_record(&directory, &record)?;
    Ok(record)
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
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Pronto")
        .join("NoteTakerAudio")
}

pub fn notetaker_audio_path(item_id: &str) -> Option<PathBuf> {
    if item_id.trim().is_empty()
        || item_id.len() > 128
        || item_id.contains(['/', '\\', '.', ':'])
    {
        return None;
    }
    Some(notetaker_audio_root().join(format!("{item_id}.wav")))
}

pub fn save_notetaker_audio(item_id: &str, bytes: &[u8]) -> Result<(), String> {
    let path = notetaker_audio_path(item_id)
        .ok_or_else(|| "Invalid recording identifier.".to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(&path, bytes).map_err(|e| e.to_string())
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
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(&root).map_err(|e| e.to_string())?.flatten() {
        let directory = entry.path();
        let record_path = directory.join("meeting.json");
        let Ok(bytes) = fs::read(&record_path) else {
            continue;
        };
        let Ok(mut record) = serde_json::from_slice::<MeetingRecord>(&bytes) else {
            continue;
        };
        if record.status != "recording" {
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
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Pronto")
        .join("Meetings")
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
}
