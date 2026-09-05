use crate::audio::Recording;
use crate::gpu_memory::{GpuMemoryMonitor, MemoryInfo};
use crate::settings::{
    deepseek_key, HistoryEntry, UserSettings, DEFAULT_CLEANUP_PROMPT,
    DEFAULT_LONGFORM_CLEANUP_PROMPT,
};
use reqwest::blocking::{multipart, Client};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::net::TcpListener;
#[cfg(windows)]
use std::os::windows::{io::AsRawHandle, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};
use tauri::AppHandle;

const PARAKEET_MODEL: &str = "parakeet-tdt-0.6b-v3.q8_0.gguf";
const MIB: u64 = 1024 * 1024;
const GPU_POLL_INTERVAL: Duration = Duration::from_secs(2);
const GPU_PRESSURE_SAMPLES: u8 = 4;
const MODEL_IDLE_BEFORE_UNLOAD: Duration = Duration::from_secs(30);
const MODEL_TRANSITION_COOLDOWN: Duration = Duration::from_secs(60);
const GPU_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const WARM_RETRY_COOLDOWN: Duration = Duration::from_secs(30);
const ENGINE_LOG_LIMIT: u64 = 1024 * 1024;
pub struct TranscriptionJob {
    pub recording: Recording,
    pub settings: UserSettings,
    pub target_window: isize,
    #[allow(dead_code)]
    pub skip_history: bool,
    #[allow(dead_code)]
    pub upload_id: Option<String>,
}

impl TranscriptionJob {
    pub fn live(recording: Recording, settings: UserSettings, target_window: isize) -> Self {
        Self {
            recording,
            settings,
            target_window,
            skip_history: false,
            upload_id: None,
        }
    }

    pub fn file_import(
        recording: Recording,
        settings: UserSettings,
        skip_history: bool,
        upload_id: Option<String>,
    ) -> Self {
        Self {
            recording,
            settings,
            target_window: 0,
            skip_history,
            upload_id,
        }
    }
}

pub struct MeetingTranscriptionJob {
    pub id: String,
    pub title: String,
    pub audio_path: PathBuf,
    pub settings: UserSettings,
}

pub struct CompletedMeetingTranscription {
    pub id: String,
    pub transcript: String,
    pub notes: String,
    pub warning: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub ready: bool,
    pub message: String,
    pub backend: String,
}

pub struct EngineController {
    commands: mpsc::Sender<EngineCommand>,
}

impl EngineController {
    pub fn new(app: AppHandle, resource_dir: Option<PathBuf>, gpu_memory_management: bool) -> Self {
        let (commands, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("pronto-engine".into())
            .spawn(move || engine_worker(app, resource_dir, gpu_memory_management, receiver))
            .expect("failed to start transcription engine thread");
        Self { commands }
    }

    pub fn transcribe(&self, job: TranscriptionJob) -> Result<(), String> {
        self.commands
            .send(EngineCommand::Transcribe(job))
            .map_err(|_| "transcription engine stopped".into())
    }

    pub fn transcribe_meeting(&self, job: MeetingTranscriptionJob) -> Result<(), String> {
        self.commands
            .send(EngineCommand::TranscribeMeeting(job))
            .map_err(|_| "transcription engine stopped".into())
    }

    pub fn warm(&self) {
        let _ = self.commands.send(EngineCommand::Warm);
    }

    pub fn set_gpu_memory_management(&self, enabled: bool) {
        let _ = self
            .commands
            .send(EngineCommand::ConfigureGpuMemory(enabled));
    }
}

enum EngineCommand {
    Transcribe(TranscriptionJob),
    TranscribeMeeting(MeetingTranscriptionJob),
    Warm,
    ConfigureGpuMemory(bool),
}

struct GpuPressurePolicy {
    enabled: bool,
    low_readings: u8,
    last_activity: Instant,
    last_transition: Instant,
    model_bytes: u64,
}

impl GpuPressurePolicy {
    fn new(enabled: bool, now: Instant, model_bytes: u64) -> Self {
        Self {
            enabled,
            low_readings: 0,
            last_activity: now,
            last_transition: now,
            model_bytes,
        }
    }

    fn note_activity(&mut self, now: Instant) {
        self.last_activity = now;
        self.low_readings = 0;
    }

    fn note_transition(&mut self, now: Instant) {
        self.last_transition = now;
        self.low_readings = 0;
    }

    fn reserve_bytes(memory: MemoryInfo) -> u64 {
        (memory.total / 5).clamp(1024 * MIB, 2048 * MIB)
    }

    fn can_load(&self, memory: MemoryInfo) -> bool {
        !self.enabled || memory.free >= self.model_bytes.saturating_add(Self::reserve_bytes(memory))
    }

    fn observe_loaded(&mut self, memory: MemoryInfo, now: Instant) -> bool {
        if !self.enabled
            || now.duration_since(self.last_activity) < MODEL_IDLE_BEFORE_UNLOAD
            || now.duration_since(self.last_transition) < MODEL_TRANSITION_COOLDOWN
        {
            self.low_readings = 0;
            return false;
        }
        if memory.free < Self::reserve_bytes(memory) {
            self.low_readings = self.low_readings.saturating_add(1);
        } else {
            self.low_readings = 0;
        }
        self.low_readings >= GPU_PRESSURE_SAMPLES
    }
}

fn engine_worker(
    app: AppHandle,
    resource_dir: Option<PathBuf>,
    gpu_memory_management: bool,
    receiver: mpsc::Receiver<EngineCommand>,
) {
    let gpu = GpuMemoryMonitor::new().ok();
    let before_load = gpu.as_ref().and_then(|gpu| gpu.memory_info().ok());
    emit_model_status(&app, false, "Loading Parakeet on the GPU…");
    let runtime = locate_runtime(resource_dir.as_deref());
    let minimum_model_bytes = runtime
        .as_ref()
        .ok()
        .and_then(|runtime| runtime.model.metadata().ok())
        .map(|metadata| metadata.len().saturating_mul(3) / 2)
        .unwrap_or(1024 * MIB);
    let mut policy =
        GpuPressurePolicy::new(gpu_memory_management, Instant::now(), minimum_model_bytes);
    let mut server = match runtime.as_ref() {
        Ok(runtime) => match SpeechServer::start(runtime) {
            Ok(server) => {
                if let (Some(before), Some(after)) = (
                    before_load,
                    gpu.as_ref().and_then(|gpu| gpu.memory_info().ok()),
                ) {
                    let observed = after.used.saturating_sub(before.used);
                    policy.model_bytes = policy.model_bytes.max(observed);
                }
                policy.note_transition(Instant::now());
                emit_model_status(&app, true, "Parakeet is warm and ready");
                Some(server)
            }
            Err(error) => {
                emit_model_status(&app, false, &error);
                None
            }
        },
        Err(error) => {
            emit_model_status(&app, false, error);
            None
        }
    };
    let mut last_start_failure = server.is_none().then(Instant::now);

    let client = Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(12))
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_nodelay(true)
        .build()
        .expect("failed to build HTTP client");

    loop {
        let command = if server.is_some() && policy.enabled && gpu.is_some() {
            match receiver.recv_timeout(GPU_POLL_INTERVAL) {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(memory) = gpu.as_ref().and_then(|gpu| gpu.memory_info().ok()) {
                        if policy.observe_loaded(memory, Instant::now()) {
                            if let Some(server) = server.as_mut() {
                                server.stop();
                            }
                            server = None;
                            policy.note_transition(Instant::now());
                            emit_model_status(
                                &app,
                                false,
                                "Parakeet released to protect GPU memory",
                            );
                        }
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match receiver.recv() {
                Ok(command) => Some(command),
                Err(_) => break,
            }
        };

        match command.expect("received engine command") {
            EngineCommand::ConfigureGpuMemory(enabled) => {
                policy.enabled = enabled;
                policy.low_readings = 0;
            }
            EngineCommand::Warm => {
                policy.note_activity(Instant::now());
                if server.is_none() {
                    if last_start_failure
                        .is_some_and(|failed| failed.elapsed() < WARM_RETRY_COOLDOWN)
                    {
                        emit_model_status(
                            &app,
                            false,
                            "Parakeet could not start; retrying shortly…",
                        );
                        continue;
                    }
                    let can_load = gpu
                        .as_ref()
                        .and_then(|gpu| gpu.memory_info().ok())
                        .is_none_or(|memory| policy.can_load(memory));
                    if can_load {
                        server = warm_server(
                            &app,
                            runtime.as_ref().map_err(String::as_str),
                            &mut policy,
                        );
                        last_start_failure = server.is_none().then(Instant::now);
                    } else {
                        emit_model_status(&app, false, "Waiting for available GPU memory…");
                    }
                }
            }
            EngineCommand::Transcribe(job) => {
                let started = Instant::now();
                policy.note_activity(started);
                let recent_start_failure =
                    last_start_failure.is_some_and(|failed| failed.elapsed() < WARM_RETRY_COOLDOWN);
                if server.is_none() && !recent_start_failure {
                    let deadline = Instant::now() + GPU_WAIT_TIMEOUT;
                    let mut waiting_emitted = false;
                    loop {
                        let can_load = gpu
                            .as_ref()
                            .and_then(|gpu| gpu.memory_info().ok())
                            .is_none_or(|memory| policy.can_load(memory));
                        if can_load {
                            server = warm_server(
                                &app,
                                runtime.as_ref().map_err(String::as_str),
                                &mut policy,
                            );
                            last_start_failure = server.is_none().then(Instant::now);
                            break;
                        }
                        if !waiting_emitted {
                            emit_model_status(&app, false, "Waiting for available GPU memory…");
                            waiting_emitted = true;
                        }
                        if Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(GPU_POLL_INTERVAL);
                    }
                }
                let recent_start_failure =
                    last_start_failure.is_some_and(|failed| failed.elapsed() < WARM_RETRY_COOLDOWN);
                let result = match server.as_mut() {
                    Some(server) => process_job(&client, server, job, started),
                    None if recent_start_failure => Err(
                        "Parakeet failed to start. See engine.log in Pronto's local data folder, then try again shortly."
                            .into(),
                    ),
                    None => Err("Not enough GPU memory to load Parakeet. Dictation was not transcribed.".into()),
                };
                crate::complete_transcription(&app, result);
            }
            EngineCommand::TranscribeMeeting(job) => {
                let started = Instant::now();
                policy.note_activity(started);
                let meeting_id = job.id.clone();
                if server.is_none() {
                    server =
                        warm_server(&app, runtime.as_ref().map_err(String::as_str), &mut policy);
                    last_start_failure = server.is_none().then(Instant::now);
                }
                let result = match server.as_mut() {
                    Some(server) => process_meeting_job(&client, server, job),
                    None => Err("Parakeet could not start to process the meeting".into()),
                };
                match result {
                    Ok(completed) => crate::complete_meeting_transcription(&app, Ok(completed)),
                    Err(error) => crate::fail_meeting_transcription(&app, &meeting_id, error),
                }
            }
        }
    }

    if let Some(server) = server.as_mut() {
        server.stop();
    }
}

fn warm_server(
    app: &AppHandle,
    runtime: Result<&RuntimePaths, &str>,
    policy: &mut GpuPressurePolicy,
) -> Option<SpeechServer> {
    let runtime = match runtime {
        Ok(runtime) => runtime,
        Err(error) => {
            emit_model_status(app, false, error);
            return None;
        }
    };
    emit_model_status(app, false, "Warming Parakeet on the GPU…");
    match SpeechServer::start(runtime) {
        Ok(server) => {
            policy.note_transition(Instant::now());
            emit_model_status(&app, true, "Parakeet is warm and ready");
            Some(server)
        }
        Err(error) => {
            emit_model_status(app, false, &error);
            None
        }
    }
}

fn process_job(
    client: &Client,
    server: &mut SpeechServer,
    job: TranscriptionJob,
    started: Instant,
) -> Result<CompletedTranscription, String> {
    let audio_ms = job.recording.samples.len() as u128 * 1_000
        / (job.recording.sample_rate as u128 * job.recording.channels.max(1) as u128);
    let asr_started = Instant::now();
    let raw = transcribe_recording(client, server, &job.recording, &job.settings.language)?;
    let asr_ms = asr_started.elapsed().as_millis();
    if raw.is_empty() {
        return Err("No speech was detected".into());
    }

    let locally_cleaned = local_cleanup(&raw);
    let dictionary_fallback = apply_dictionary(&locally_cleaned, &job.settings.dictionary);
    let cleanup_started = Instant::now();
    let (final_text, cleanup_applied, cleanup_warning) = if job.settings.cleanup_enabled {
        match deepseek_key() {
            Some(key) => {
                let prompt = job
                    .settings
                    .cleanup_prompt
                    .as_deref()
                    .unwrap_or(DEFAULT_CLEANUP_PROMPT);
                match deepseek_cleanup(
                    client,
                    &key,
                    &locally_cleaned,
                    &job.settings.dictionary,
                    prompt,
                ) {
                    Ok(cleaned) => (
                        apply_dictionary(&cleaned, &job.settings.dictionary),
                        true,
                        None,
                    ),
                    Err(error) => (dictionary_fallback, false, Some(error)),
                }
            }
            None => (
                dictionary_fallback,
                false,
                Some("Add a DeepSeek API key in Settings to enable AI cleanup".into()),
            ),
        }
    } else {
        (dictionary_fallback, false, None)
    };
    let final_text = rewrite_em_dashes(&format_enumerated_points(&final_text));
    let cleanup_ms = cleanup_started.elapsed().as_millis();
    let total_ms = started.elapsed().as_millis();

    Ok(CompletedTranscription {
        entry: HistoryEntry::new(
            raw,
            final_text,
            asr_ms,
            cleanup_ms,
            total_ms,
            audio_ms,
            cleanup_applied,
        ),
        target_window: job.target_window,
        auto_insert: job.settings.auto_insert,
        cleanup_warning,
        skip_history: job.skip_history,
        upload_id: job.upload_id,
    })
}

fn transcribe_recording(
    client: &Client,
    server: &SpeechServer,
    recording: &Recording,
    language: &str,
) -> Result<String, String> {
    let audio_ms = recording.samples.len() as u128 * 1_000
        / (recording.sample_rate as u128 * recording.channels.max(1) as u128);
    let wav = recording_to_wav(recording)?;
    let mut form = multipart::Form::new()
        .part(
            "file",
            multipart::Part::bytes(wav)
                .file_name("audio.wav")
                .mime_str("audio/wav")
                .map_err(|e| e.to_string())?,
        )
        .text("model", "parakeet")
        .text("response_format", "json");
    if language != "auto" {
        form = form.text("language", language.to_string());
    }
    let response = client
        .post(format!("{}/v1/audio/transcriptions", server.base_url))
        .timeout(Duration::from_secs(
            ((audio_ms / 10_000) + 30).clamp(30, 600) as u64,
        ))
        .multipart(form)
        .send()
        .map_err(|e| format!("Local transcription request failed: {e}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().unwrap_or_default();
        return Err(format!("Parakeet returned {status}: {detail}"));
    }
    Ok(response
        .json::<AsrResponse>()
        .map_err(|e| format!("Invalid Parakeet response: {e}"))?
        .text
        .trim()
        .to_string())
}

fn process_meeting_job(
    client: &Client,
    server: &SpeechServer,
    job: MeetingTranscriptionJob,
) -> Result<CompletedMeetingTranscription, String> {
    const CHUNK_SAMPLES: usize = 16_000 * 120;
    let mut file = BufReader::new(
        fs::File::open(&job.audio_path)
            .map_err(|e| format!("Could not open meeting audio: {e}"))?,
    );
    file.seek(SeekFrom::Start(44)).map_err(|e| e.to_string())?;
    let mut transcript_parts = Vec::new();
    loop {
        let mut bytes = vec![0u8; CHUNK_SAMPLES * 2];
        let read = file.read(&mut bytes).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        bytes.truncate(read - (read % 2));
        let samples = bytes
            .chunks_exact(2)
            .map(|v| i16::from_le_bytes([v[0], v[1]]) as f32 / 32768.0)
            .collect();
        let recording = Recording {
            samples,
            sample_rate: 16_000,
            channels: 1,
        };
        let text = transcribe_recording(client, server, &recording, &job.settings.language)?;
        if !text.is_empty() {
            transcript_parts.push(text);
        }
    }
    let transcript = apply_dictionary(
        &local_cleanup(&transcript_parts.join(" ")),
        &job.settings.dictionary,
    );
    if transcript.is_empty() {
        return Err("No speech was detected in the meeting".into());
    }
    let (notes, warning) = match deepseek_key() {
        Some(key) => match generate_meeting_notes(client, &key, &job.title, &transcript) {
            Ok(notes) => (notes, None),
            Err(error) => (local_meeting_notes(&job.title, &transcript), Some(error)),
        },
        None => (
            local_meeting_notes(&job.title, &transcript),
            Some("Add a DeepSeek API key for structured AI meeting notes".into()),
        ),
    };
    Ok(CompletedMeetingTranscription {
        id: job.id,
        transcript,
        notes,
        warning,
    })
}

fn generate_meeting_notes(
    client: &Client,
    key: &str,
    title: &str,
    transcript: &str,
) -> Result<String, String> {
    const PROMPT: &str = "Create concise Markdown meeting notes grounded only in the transcript. Use sections: Summary, Decisions, Action items, Open questions, and Key points. Never invent an owner, deadline, decision, or fact. Write 'None captured' when a section has no evidence.";
    let mut partials = Vec::new();
    for chunk in split_utf8_chunks(transcript, 36_000) {
        partials.push(deepseek_cleanup_at(
            client,
            "https://api.deepseek.com/chat/completions",
            key,
            chunk,
            &[],
            PROMPT,
            3000,
        )?);
    }
    if partials.len() == 1 {
        return Ok(partials.remove(0));
    }
    let combined = format!(
        "Meeting: {title}\n\nPARTIAL NOTES:\n{}",
        partials.join("\n\n---\n\n")
    );
    deepseek_cleanup_at(
        client,
        "https://api.deepseek.com/chat/completions",
        key,
        &combined,
        &[],
        PROMPT,
        5000,
    )
}

fn split_utf8_chunks(text: &str, maximum: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + maximum).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(&text[start..end]);
        start = end;
    }
    chunks
}

fn local_meeting_notes(title: &str, transcript: &str) -> String {
    let summary = transcript
        .split_terminator(['.', '!', '?'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join(". ");
    format!("# {title}\n\n## Summary\n\n{}.\n\n## Decisions\n\nNone captured automatically.\n\n## Action items\n\nNone captured automatically.\n\n## Open questions\n\nNone captured automatically.\n\n## Key points\n\nSee the complete transcript below.", summary)
}

pub fn recording_from_pcm16_wav(bytes: &[u8]) -> Result<Recording, String> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("Pronto could not read the decoded audio.".into());
    }
    let mut offset = 12usize;
    let mut format = None;
    let mut data = None;
    while offset.saturating_add(8) <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let start = offset + 8;
        let end = start
            .checked_add(size)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| "The decoded audio file is incomplete.".to_string())?;
        if id == b"fmt " && size >= 16 {
            format = Some((
                u16::from_le_bytes(bytes[start..start + 2].try_into().unwrap()),
                u16::from_le_bytes(bytes[start + 2..start + 4].try_into().unwrap()),
                u32::from_le_bytes(bytes[start + 4..start + 8].try_into().unwrap()),
                u16::from_le_bytes(bytes[start + 14..start + 16].try_into().unwrap()),
            ));
        } else if id == b"data" {
            data = Some(&bytes[start..end]);
        }
        offset = end + (size & 1);
    }
    let (encoding, channels, sample_rate, bits) =
        format.ok_or_else(|| "The decoded audio has no format information.".to_string())?;
    if encoding != 1 || channels != 1 || sample_rate != 16_000 || bits != 16 {
        return Err("The decoded audio is not 16 kHz mono PCM.".into());
    }
    let data = data.ok_or_else(|| "The decoded audio contains no samples.".to_string())?;
    let samples = data
        .chunks_exact(2)
        .map(|sample| i16::from_le_bytes([sample[0], sample[1]]) as f32 / 32768.0)
        .collect::<Vec<_>>();
    if samples.is_empty() {
        return Err("The selected file has no audible content.".into());
    }
    Ok(Recording {
        samples,
        sample_rate,
        channels,
    })
}

pub struct CompletedTranscription {
    pub entry: HistoryEntry,
    pub target_window: isize,
    pub auto_insert: bool,
    pub cleanup_warning: Option<String>,
    pub skip_history: bool,
    pub upload_id: Option<String>,
}

struct RuntimePaths {
    executable: PathBuf,
    model: PathBuf,
}

struct SpeechServer {
    child: Child,
    base_url: String,
    #[cfg(windows)]
    _job: ProcessJob,
}

#[cfg(windows)]
struct ProcessJob(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl ProcessJob {
    fn attach(child: &Child) -> Result<Self, String> {
        use std::mem::size_of;
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        let job = unsafe { CreateJobObjectW(None, None) }
            .map_err(|error| format!("Could not create speech process job: {error}"))?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const core::ffi::c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if let Err(error) = configured {
            let _ = unsafe { CloseHandle(job) };
            return Err(format!(
                "Could not configure speech process cleanup: {error}"
            ));
        }
        let process = HANDLE(child.as_raw_handle());
        if let Err(error) = unsafe { AssignProcessToJobObject(job, process) } {
            let _ = unsafe { CloseHandle(job) };
            return Err(format!("Could not attach speech process cleanup: {error}"));
        }
        Ok(Self(job))
    }
}

#[cfg(windows)]
impl Drop for ProcessJob {
    fn drop(&mut self) {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.0) };
    }
}

impl SpeechServer {
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn start(runtime: &RuntimePaths) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| format!("Could not reserve local speech port: {error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| error.to_string())?
            .port();
        drop(listener);

        let bin_dir = runtime
            .executable
            .parent()
            .ok_or_else(|| "Invalid NeMo Speech runtime path".to_string())?;
        let log_path = engine_log_path();
        if fs::metadata(&log_path).is_ok_and(|metadata| metadata.len() > ENGINE_LOG_LIMIT) {
            let _ = fs::rename(&log_path, log_path.with_extension("old.log"));
        }
        if let Some(parent) = log_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&log_path)
            .map_err(|error| format!("Could not open engine log: {error}"))?;
        let stderr_log = log
            .try_clone()
            .map_err(|error| format!("Could not prepare engine log: {error}"))?;
        let mut command = Command::new(&runtime.executable);
        command
            .args([
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--threads",
                "1",
                "--no-ui",
                "--device",
                "cuda:0",
                "--asr-model",
            ])
            .arg(&runtime.model)
            .current_dir(bin_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log.try_clone().map_err(|error| error.to_string())?,
            ))
            .stderr(Stdio::from(stderr_log));
        // NeMo Speech is a console-subsystem executable. Redirecting its
        // streams does not suppress the console host; CREATE_NO_WINDOW does.
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        let mut child = command
            .spawn()
            .map_err(|error| format!("Could not start NVIDIA speech runtime: {error}"))?;
        #[cfg(windows)]
        let job = match ProcessJob::attach(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };

        let base_url = format!("http://127.0.0.1:{port}");
        let health_client = Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|error| error.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(90);
        while Instant::now() < deadline {
            if let Some(status) = child
                .try_wait()
                .map_err(|error| format!("Could not inspect speech runtime: {error}"))?
            {
                let detail = log_tail(&mut log);
                return Err(format!(
                    "Parakeet exited during startup ({status}).{detail}"
                ));
            }
            if health_client
                .get(format!("{base_url}/health"))
                .send()
                .map(|response| response.status().is_success())
                .unwrap_or(false)
            {
                return Ok(Self {
                    child,
                    base_url,
                    #[cfg(windows)]
                    _job: job,
                });
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let mut child = child;
        let _ = child.kill();
        let detail = log_tail(&mut log);
        Err(format!(
            "Parakeet did not finish loading within 90 seconds.{detail}"
        ))
    }
}

fn engine_log_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Pronto")
        .join("engine.log")
}

fn log_tail(log: &mut fs::File) -> String {
    let Ok(length) = log.seek(SeekFrom::End(0)) else {
        return String::new();
    };
    let start = length.saturating_sub(2_048);
    if log.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut tail = String::new();
    if log.read_to_string(&mut tail).is_err() {
        return String::new();
    }
    let line = tail.lines().rev().find(|line| !line.trim().is_empty());
    line.map(|line| format!(" Last engine message: {}", line.trim()))
        .unwrap_or_default()
}

fn locate_runtime(resource_dir: Option<&Path>) -> Result<RuntimePaths, String> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("PRONTO_HOME") {
        roots.push(PathBuf::from(home));
    }
    if let Some(resource_dir) = resource_dir {
        roots.push(resource_dir.to_path_buf());
    }
    if let Ok(executable) = std::env::current_exe() {
        for ancestor in executable.ancestors().skip(1).take(6) {
            roots.push(ancestor.to_path_buf());
        }
    }
    if let Ok(current) = std::env::current_dir() {
        roots.push(current.clone());
        if let Some(parent) = current.parent() {
            roots.push(parent.to_path_buf());
        }
    }

    for root in roots {
        let executable = root.join("runtime/nemo-speech/bin/nemo-speech.exe");
        let model = root.join("models").join(PARAKEET_MODEL);
        if executable.is_file() && model.is_file() {
            return Ok(RuntimePaths { executable, model });
        }
    }
    Err(format!(
        "Missing Parakeet runtime or model ({PARAKEET_MODEL}). Reinstall Pronto or set PRONTO_HOME."
    ))
}

#[derive(Deserialize)]
struct AsrResponse {
    text: String,
}

#[derive(Deserialize)]
struct DeepSeekResponse {
    choices: Vec<DeepSeekChoice>,
}

#[derive(Deserialize)]
struct DeepSeekChoice {
    message: DeepSeekMessage,
}

#[derive(Deserialize)]
struct DeepSeekMessage {
    content: String,
}

pub(crate) fn deepseek_cleanup(
    client: &Client,
    api_key: &str,
    transcript: &str,
    dictionary: &[String],
    system_prompt: &str,
) -> Result<String, String> {
    deepseek_cleanup_at(
        client,
        "https://api.deepseek.com/chat/completions",
        api_key,
        transcript,
        dictionary,
        system_prompt,
        768,
    )
}

pub(crate) fn deepseek_longform_cleanup(
    client: &Client,
    api_key: &str,
    transcript: &str,
    dictionary: &[String],
) -> Result<String, String> {
    // Long interviews need room to breathe: scale output budget with input
    // length instead of the 768-token cap used for short dictations.
    let words = transcript.split_whitespace().count().max(1);
    let max_tokens = ((words * 2 + 500).min(8192)).max(1500) as u32;
    deepseek_cleanup_at(
        client,
        "https://api.deepseek.com/chat/completions",
        api_key,
        transcript,
        dictionary,
        DEFAULT_LONGFORM_CLEANUP_PROMPT,
        max_tokens,
    )
}

fn deepseek_cleanup_at(
    client: &Client,
    endpoint: &str,
    api_key: &str,
    transcript: &str,
    dictionary: &[String],
    system_prompt: &str,
    max_tokens: u32,
) -> Result<String, String> {
    let dictionary = if dictionary.is_empty() {
        "(none)".into()
    } else {
        dictionary.join(", ")
    };
    let body = json!({
        "model": "deepseek-v4-flash",
        "thinking": { "type": "disabled" },
        "messages": [
            {
                "role": "system",
                "content": system_prompt
            },
            {
                "role": "user",
                "content": format!("USER DICTIONARY (optional spelling hints):\n{dictionary}\n\nRAW TRANSCRIPT:\n{transcript}")
            }
        ],
        "temperature": 0,
        "max_tokens": max_tokens,
        "stream": false
    });
    let response = client
        .post(endpoint)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .map_err(|error| format!("DeepSeek cleanup failed: {error}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().unwrap_or_default();
        return Err(format!("DeepSeek cleanup returned {status}: {detail}"));
    }
    let content = response
        .json::<DeepSeekResponse>()
        .map_err(|error| format!("Invalid DeepSeek response: {error}"))?
        .choices
        .into_iter()
        .next()
        .map(|choice| choice.message.content.trim().to_string())
        .filter(|content| !content.is_empty())
        .ok_or_else(|| "DeepSeek returned an empty cleanup".to_string())?;
    Ok(content)
}

fn recording_to_wav(recording: &Recording) -> Result<Vec<u8>, String> {
    if recording.sample_rate == 0 || recording.channels == 0 {
        return Err("Microphone returned an invalid audio format".into());
    }
    let mono = downmix(&recording.samples, recording.channels as usize);
    let mono = resample_linear(&mono, recording.sample_rate, 16_000);
    let mono = trim_silence(&mono, 16_000);
    if mono.len() < 1_600 {
        return Err("No speech was detected".into());
    }

    let data_len = (mono.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&(16_000u32 * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for sample in mono {
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        wav.extend_from_slice(&value.to_le_bytes());
    }
    Ok(wav)
}

fn downmix(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    samples
        .chunks(channels)
        .map(|frame| frame.iter().copied().sum::<f32>() / frame.len() as f32)
        .collect()
}

fn resample_linear(samples: &[f32], input_rate: u32, output_rate: u32) -> Vec<f32> {
    if samples.is_empty() || input_rate == output_rate {
        return samples.to_vec();
    }
    let output_len = samples.len() * output_rate as usize / input_rate as usize;
    let ratio = input_rate as f64 / output_rate as f64;
    (0..output_len)
        .map(|index| {
            let position = index as f64 * ratio;
            let left = position.floor() as usize;
            let right = (left + 1).min(samples.len() - 1);
            let fraction = (position - left as f64) as f32;
            samples[left] * (1.0 - fraction) + samples[right] * fraction
        })
        .collect()
}

fn trim_silence(samples: &[f32], sample_rate: usize) -> Vec<f32> {
    let frame = sample_rate / 50;
    if samples.len() <= frame {
        return samples.to_vec();
    }
    let active: Vec<bool> = samples
        .chunks(frame)
        .map(|chunk| {
            let rms = (chunk.iter().map(|sample| sample * sample).sum::<f32>()
                / chunk.len() as f32)
                .sqrt();
            rms > 0.004
        })
        .collect();
    let Some(first) = active.iter().position(|active| *active) else {
        return Vec::new();
    };
    let last = active.iter().rposition(|active| *active).unwrap_or(first);
    let padding_frames = 5;
    let start = first.saturating_sub(padding_frames) * frame;
    let end = ((last + padding_frames + 1) * frame).min(samples.len());
    samples[start..end].to_vec()
}

fn local_cleanup(text: &str) -> String {
    let mut words = Vec::new();
    for word in text.split_whitespace() {
        let normalized = word.trim_matches(|character: char| !character.is_alphanumeric());
        let filler = matches!(
            normalized.to_ascii_lowercase().as_str(),
            "um" | "uh" | "erm"
        );
        let repeated = words.last().is_some_and(|previous: &String| {
            previous
                .trim_matches(|character: char| !character.is_alphanumeric())
                .eq_ignore_ascii_case(normalized)
        });
        if !filler && !repeated {
            words.push(word.to_string());
        }
    }
    let mut output = words.join(" ");
    if let Some(first) = output.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    output
}

pub(crate) fn apply_dictionary_public(text: &str, dictionary: &[String]) -> String {
    apply_dictionary(text, dictionary)
}

fn apply_dictionary(text: &str, dictionary: &[String]) -> String {
    let mut output = String::with_capacity(text.len());
    let mut token = String::new();
    for character in text.chars() {
        if character.is_whitespace() {
            if !token.is_empty() {
                output.push_str(&apply_dictionary_token(&token, dictionary));
                token.clear();
            }
            output.push(character);
        } else {
            token.push(character);
        }
    }
    if !token.is_empty() {
        output.push_str(&apply_dictionary_token(&token, dictionary));
    }
    output
}

fn apply_dictionary_token(token: &str, dictionary: &[String]) -> String {
    let core = token
        .trim_matches(|character: char| !character.is_alphanumeric())
        .to_string();
    if core.len() < 3 {
        return token.to_string();
    }
    for term in dictionary
        .iter()
        .filter(|term| !term.contains(char::is_whitespace))
    {
        if core.eq_ignore_ascii_case(term) {
            return token.replacen(&core, term, 1);
        }

        // Fuzzy replacement is deliberately limited to visually distinctive
        // terms (camel case, initialisms, digits, or punctuation) and a
        // single-character typo. Ordinary dictionary words are left to the
        // contextual cleanup model instead of being force-fit by similarity.
        let distinctive = term
            .chars()
            .skip(1)
            .any(|character| character.is_uppercase() || !character.is_alphabetic());
        if !distinctive || core.chars().count() < 5 {
            continue;
        }
        let core_lower = core.to_lowercase();
        let term_lower = term.to_lowercase();
        let same_edges = core_lower
            .chars()
            .next()
            .zip(term_lower.chars().next())
            .is_some_and(|(left, right)| left == right)
            && core_lower
                .chars()
                .last()
                .zip(term_lower.chars().last())
                .is_some_and(|(left, right)| left == right);
        if same_edges && levenshtein(&core_lower, &term_lower) == 1 {
            return token.replacen(&core, term, 1);
        }
    }
    token.to_string()
}

fn format_enumerated_points(text: &str) -> String {
    const MARKERS: [(&str, usize); 20] = [
        ("firstly", 1),
        ("first", 1),
        ("secondly", 2),
        ("second", 2),
        ("thirdly", 3),
        ("third", 3),
        ("fourthly", 4),
        ("fourth", 4),
        ("fifthly", 5),
        ("fifth", 5),
        ("sixthly", 6),
        ("sixth", 6),
        ("seventhly", 7),
        ("seventh", 7),
        ("eighthly", 8),
        ("eighth", 8),
        ("ninthly", 9),
        ("ninth", 9),
        ("tenthly", 10),
        ("tenth", 10),
    ];
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut candidates = Vec::new();
    for (marker, ordinal) in MARKERS {
        for (start, _) in lower.match_indices(marker) {
            let previous = bytes[..start]
                .iter()
                .rev()
                .copied()
                .find(|byte| !byte.is_ascii_whitespace());
            let sentence_boundary = previous.is_none_or(|byte| b".!?;:\n".contains(&byte));
            let end = start + marker.len();
            let word_boundary = bytes
                .get(end)
                .is_none_or(|byte| !byte.is_ascii_alphanumeric());
            if sentence_boundary && word_boundary {
                candidates.push((start, end, ordinal));
            }
        }
    }
    candidates.sort_by_key(|candidate| candidate.0);
    let Some(first_index) = candidates.iter().position(|candidate| candidate.2 == 1) else {
        return text.to_string();
    };
    let mut sequence = vec![candidates[first_index]];
    let mut expected = 2;
    for candidate in candidates.into_iter().skip(first_index + 1) {
        if candidate.2 == expected {
            sequence.push(candidate);
            expected += 1;
        }
    }
    if sequence.len() < 2 {
        return text.to_string();
    }

    let intro = text[..sequence[0].0].trim();
    let mut formatted = String::new();
    if !intro.is_empty() {
        formatted.push_str(intro);
        formatted.push_str("\n\n");
    }
    for (index, (_, marker_end, _)) in sequence.iter().enumerate() {
        let segment_end = sequence
            .get(index + 1)
            .map(|next| next.0)
            .unwrap_or(text.len());
        let mut item = text[*marker_end..segment_end]
            .trim_start_matches(|character: char| {
                character.is_whitespace() || matches!(character, ',' | ':' | '-' | '—')
            })
            .trim();
        if let Some(rest) = item
            .strip_prefix("of all")
            .or_else(|| item.strip_prefix("Of all"))
        {
            item = rest
                .trim_start_matches(|character: char| {
                    character.is_whitespace() || matches!(character, ',' | ':' | '-' | '—')
                })
                .trim();
        }
        let mut characters = item.chars();
        let item = match characters.next() {
            Some(first) => first.to_uppercase().chain(characters).collect::<String>(),
            None => String::new(),
        };
        formatted.push_str(&format!("{}. {}", index + 1, item));
        if index + 1 < sequence.len() {
            formatted.push('\n');
        }
    }
    formatted
}

fn rewrite_em_dashes(text: &str) -> String {
    text.replace(" — ", "; ").replace('—', ", ")
}

fn levenshtein(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut costs: Vec<usize> = (0..=right.len()).collect();
    for (row, left_char) in left.chars().enumerate() {
        let mut diagonal = row;
        costs[0] = row + 1;
        for (column, right_char) in right.iter().enumerate() {
            let above = costs[column + 1];
            costs[column + 1] = if left_char == *right_char {
                diagonal
            } else {
                1 + diagonal.min(above).min(costs[column])
            };
            diagonal = above;
        }
    }
    costs[right.len()]
}

fn emit_model_status(app: &AppHandle, ready: bool, message: &str) {
    crate::set_model_status(
        app,
        ModelStatus {
            ready,
            message: message.into(),
            backend: "NVIDIA Parakeet TDT 0.6B v3 · CUDA".into(),
        },
    );
    if !ready {
        let _ = tauri::Emitter::emit(
            app,
            "dictation-notice",
            serde_json::json!({ "message": message }),
        );
    } else if ready {
        let _ = tauri::Emitter::emit(app, "dictation-notice-clear", ());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    #[test]
    fn creates_valid_mono_16k_wav() {
        let recording = Recording {
            samples: vec![0.1; 48_000],
            sample_rate: 48_000,
            channels: 1,
        };
        let wav = recording_to_wav(&recording).unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
    }

    #[test]
    fn reads_imported_pcm16_wav() {
        let recording = Recording {
            samples: vec![0.5; 2_000],
            sample_rate: 16_000,
            channels: 1,
        };
        let wav = recording_to_wav(&recording).unwrap();
        let decoded = recording_from_pcm16_wav(&wav).unwrap();
        assert_eq!(decoded.sample_rate, 16_000);
        assert_eq!(decoded.channels, 1);
        assert_eq!(decoded.samples.len(), 2_000);
        assert!((decoded.samples[0] - 0.5).abs() < 0.001);
    }

    #[test]
    fn gpu_pressure_requires_sustained_low_memory_and_cooldown() {
        let now = Instant::now();
        let mut policy = GpuPressurePolicy::new(true, now, 1024 * MIB);
        policy.last_activity = now - MODEL_IDLE_BEFORE_UNLOAD - Duration::from_secs(1);
        policy.last_transition = now - MODEL_TRANSITION_COOLDOWN - Duration::from_secs(1);
        let low = MemoryInfo {
            total: 6 * 1024 * MIB,
            free: 900 * MIB,
            used: 5 * 1024 * MIB,
        };
        for _ in 0..GPU_PRESSURE_SAMPLES - 1 {
            assert!(!policy.observe_loaded(low, now));
        }
        assert!(policy.observe_loaded(low, now));

        policy.note_transition(now);
        assert!(!policy.observe_loaded(low, now));
    }

    #[test]
    fn gpu_reload_preserves_model_and_system_reserve() {
        let now = Instant::now();
        let policy = GpuPressurePolicy::new(true, now, 1024 * MIB);
        let enough = MemoryInfo {
            total: 6 * 1024 * MIB,
            free: 2300 * MIB,
            used: 3700 * MIB,
        };
        let constrained = MemoryInfo {
            free: 1800 * MIB,
            ..enough
        };
        assert!(policy.can_load(enough));
        assert!(!policy.can_load(constrained));
    }

    #[test]
    fn deepseek_cleanup_uses_fast_model_dictionary_and_parses_response() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock server should bind");
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("mock request should connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let count = stream.read(&mut chunk).expect("mock request should read");
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
                if let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let header_end = header_end + 4;
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if request.len() >= header_end + content_length {
                        break;
                    }
                }
            }
            request_tx.send(request).unwrap();
            let body = r#"{"choices":[{"message":{"content":"Use Pronto with Parakeet."}}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let cleaned = deepseek_cleanup_at(
            &client,
            &format!("http://{address}/chat/completions"),
            "test-secret",
            "use pronto with parakeet",
            &["Pronto".into(), "Parakeet".into()],
            DEFAULT_CLEANUP_PROMPT,
            768,
        )
        .expect("mock cleanup should succeed");
        assert_eq!(cleaned, "Use Pronto with Parakeet.");

        let request = request_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        server.join().unwrap();
        let header_end = request
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .unwrap()
            + 4;
        let headers = String::from_utf8_lossy(&request[..header_end]);
        assert!(headers
            .lines()
            .any(|line| line.eq_ignore_ascii_case("authorization: Bearer test-secret")));
        let payload: serde_json::Value = serde_json::from_slice(&request[header_end..]).unwrap();
        assert_eq!(payload["model"], "deepseek-v4-flash");
        assert_eq!(payload["thinking"]["type"], "disabled");
        assert_eq!(payload["stream"], false);
        assert_eq!(payload["temperature"], 0);
        assert!(payload["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("substantially duplicated sentences"));
        assert!(payload["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("dictionary is a list of spelling hints"));
        assert!(payload["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Never use em dashes"));
        assert!(payload["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("Pronto, Parakeet"));
    }

    #[test]
    #[ignore = "requires a DeepSeek key saved by Pronto or DEEPSEEK_API_KEY"]
    fn live_deepseek_cleanup_roundtrip() {
        let key = deepseek_key().expect("save a DeepSeek key in Pronto Settings first");
        let client = Client::builder()
            .timeout(Duration::from_secs(12))
            .build()
            .unwrap();
        let started = Instant::now();
        let cleaned = deepseek_cleanup(
            &client,
            &key,
            "um please use deep seek deep seek for cleanup",
            &["DeepSeek".into()],
            DEFAULT_CLEANUP_PROMPT,
        )
        .expect("live DeepSeek cleanup should succeed");
        println!(
            "DeepSeek cleanup: {} ms; text: {}",
            started.elapsed().as_millis(),
            cleaned
        );
        assert!(cleaned.contains("DeepSeek"));
        assert!(!cleaned.to_lowercase().contains("um "));
    }

    #[test]
    fn local_cleanup_removes_fillers_and_repeats() {
        assert_eq!(local_cleanup("um hello hello world"), "Hello world");
    }

    #[test]
    fn dictionary_repairs_close_spellings() {
        assert_eq!(
            apply_dictionary("Send it through DeepSeak.", &["DeepSeek".into()]),
            "Send it through DeepSeek."
        );
    }

    #[test]
    fn dictionary_does_not_force_similar_ordinary_words() {
        assert_eq!(
            apply_dictionary(
                "The prompt explains the meaning clearly.",
                &["Pronto".into(), "meeting".into()]
            ),
            "The prompt explains the meaning clearly."
        );
    }

    #[test]
    fn dictionary_canonicalizes_exact_terms_without_fuzzy_guessing() {
        assert_eq!(
            apply_dictionary(
                "Use pronto and DEEPSEEK.",
                &["Pronto".into(), "DeepSeek".into()]
            ),
            "Use Pronto and DeepSeek."
        );
    }

    #[test]
    fn dictionary_preserves_cleanup_formatting() {
        assert_eq!(
            apply_dictionary(
                "Changes:\n\n1. Use pronto.\n2. Keep the layout.",
                &["Pronto".into()]
            ),
            "Changes:\n\n1. Use Pronto.\n2. Keep the layout."
        );
    }

    #[test]
    fn spoken_ordinals_become_numbered_points() {
        assert_eq!(
            format_enumerated_points(
                "I need three changes. First, add padding. Second, fix formatting. Third, make pasting reliable."
            ),
            "I need three changes.\n\n1. Add padding.\n2. Fix formatting.\n3. Make pasting reliable."
        );
        assert_eq!(
            format_enumerated_points("First of all, keep this. Second, change that."),
            "1. Keep this.\n2. Change that."
        );
    }

    #[test]
    fn em_dashes_are_restructured_without_dropping_text() {
        assert_eq!(
            rewrite_em_dashes("It is ready — ship it. Pronto—our app—stays open."),
            "It is ready; ship it. Pronto, our app, stays open."
        );
    }

    #[test]
    #[ignore = "requires the bundled Parakeet model, CUDA runtime, and PRONTO_TEST_WAV"]
    fn end_to_end_parakeet_cuda_transcription() {
        let wav_path = std::env::var("PRONTO_TEST_WAV").expect("PRONTO_TEST_WAV is required");
        let bytes = std::fs::read(wav_path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        let channels = u16::from_le_bytes(bytes[22..24].try_into().unwrap());
        let sample_rate = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
        let bits = u16::from_le_bytes(bytes[34..36].try_into().unwrap());
        assert_eq!(bits, 16);
        let data_offset = bytes
            .windows(4)
            .position(|window| window == b"data")
            .map(|position| position + 8)
            .unwrap();
        let samples = bytes[data_offset..]
            .chunks_exact(2)
            .map(|sample| i16::from_le_bytes([sample[0], sample[1]]) as f32 / i16::MAX as f32)
            .collect();
        let recording = Recording {
            samples,
            sample_rate,
            channels,
        };
        let runtime = locate_runtime(None).unwrap();
        let mut server = SpeechServer::start(&runtime).unwrap();
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let mut settings = UserSettings::default();
        settings.cleanup_enabled = false;
        settings.auto_insert = false;
        let result = process_job(
            &client,
            &mut server,
            TranscriptionJob::file_import(recording, settings, false, None),
            Instant::now(),
        )
        .unwrap();
        let _ = server.child.kill();
        let _ = server.child.wait();
        println!(
            "Parakeet ASR: {} ms; total pipeline: {} ms; text: {}",
            result.entry.asr_ms, result.entry.total_ms, result.entry.final_text
        );
        assert!(result
            .entry
            .final_text
            .to_lowercase()
            .contains("your country"));
        assert!(
            result.entry.asr_ms < 1_000,
            "ASR took {} ms",
            result.entry.asr_ms
        );
    }
}
