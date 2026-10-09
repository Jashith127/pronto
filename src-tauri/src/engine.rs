use crate::audio::Recording;
use crate::cleanup_provider::{self, CleanupTarget};
use crate::gpu_memory::{GpuMemoryMonitor, MemoryInfo};
use crate::settings::{
    HistoryEntry, UserSettings, DEFAULT_CLEANUP_PROMPT, DEFAULT_LONGFORM_CLEANUP_PROMPT,
};
use reqwest::blocking::{multipart, Client};
use serde::{Deserialize, Serialize};
use speech_packs::AsrModel;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::TcpListener;
#[cfg(windows)]
use std::os::windows::{io::AsRawHandle, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::AppHandle;

const PARAKEET_MODEL: &str = "parakeet-tdt-0.6b-v3.q8_0.gguf";
const MIB: u64 = 1024 * 1024;
// Pressure detection must beat the stutter a game launch causes: poll every
// second and release after a few mostly-low readings (~3 s), not after a
// minute of unbroken lows.
const GPU_POLL_INTERVAL: Duration = Duration::from_secs(1);
const GPU_PRESSURE_SAMPLES: u8 = 3;
const MODEL_IDLE_BEFORE_UNLOAD: Duration = Duration::from_secs(10);
const MODEL_TRANSITION_COOLDOWN: Duration = Duration::from_secs(20);
/// VRAM another app must claim, beyond what it held when the model loaded,
/// to count as a new heavy GPU workload (a game starting).
const GPU_EXTERNAL_GROWTH: u64 = 1536 * MIB;
const GPU_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const DEEPSEEK_MODELS_URL: &str = "https://api.deepseek.com/models";
/// `recording_to_wav`'s error for audio that is silent after trimming.
const NO_SPEECH: &str = "No speech was detected";
const WARM_RETRY_COOLDOWN: Duration = Duration::from_secs(30);
const ENGINE_LOG_LIMIT: u64 = 1024 * 1024;
const PHONON_MODEL_DIR: &str = "models/phonon-2";
/// Start error for a warm-up the user switched away from; not a failure.
const SUPERSEDED: &str = "superseded by a model switch";
const PHONON_PYTHON: &str = "runtimes/phonon-cpu/python/python.exe";
const CUDA_RUNTIME_PACK_EXE: &str = "runtimes/nemo-speech-cuda/bin/nemo-speech.exe";

/// The backend the engine thread is running, for status labels.
static ACTIVE_BACKEND: AtomicU8 = AtomicU8::new(0);
/// The backend the user last chose. It is set before the switch command is
/// queued, so a long warm-up (Phonon takes over a minute on the CPU) can
/// notice it has been superseded and give way instead of blocking the queue.
static DESIRED_BACKEND: AtomicU8 = AtomicU8::new(0);

fn backend_code(model: AsrModel) -> u8 {
    match model {
        AsrModel::Parakeet => 0,
        AsrModel::Phonon => 1,
    }
}

fn backend_from_code(code: u8) -> AsrModel {
    match code {
        1 => AsrModel::Phonon,
        _ => AsrModel::Parakeet,
    }
}

fn active_backend() -> AsrModel {
    backend_from_code(ACTIVE_BACKEND.load(Ordering::Acquire))
}

fn set_active_backend(model: AsrModel) {
    ACTIVE_BACKEND.store(backend_code(model), Ordering::Release);
}

fn desired_backend() -> AsrModel {
    backend_from_code(DESIRED_BACKEND.load(Ordering::Acquire))
}

fn set_desired_backend(model: AsrModel) {
    DESIRED_BACKEND.store(backend_code(model), Ordering::Release);
}

/// Where the active backend runs, for status text ("Warming Phonon on the CPU…").
fn device_phrase(model: AsrModel) -> &'static str {
    match model {
        AsrModel::Phonon => "the CPU",
        AsrModel::Parakeet if cfg!(target_os = "macos") => "Metal",
        AsrModel::Parakeet => "the GPU",
    }
}

/// Meeting chunks go one at a time on both backends. Phonon's packed CPU
/// engine competes with itself in parallel; Parakeet's server runs one
/// request at a time and its single HTTP worker stays on an idle keep-alive
/// connection for 5 s, so parallel chunks stalled behind each other and
/// could be dropped (3 workers: 16 s and an aborted chunk where one worker
/// took ~5 s; see scripts/benchmarks/README.md).
const MEETING_WORKERS: usize = 1;
pub struct TranscriptionJob {
    pub recording: Recording,
    pub settings: UserSettings,
    pub target_window: isize,
    #[allow(dead_code)]
    pub skip_history: bool,
    #[allow(dead_code)]
    pub upload_id: Option<String>,
    /// Segments already transcribed while recording (`LiveSegments`).
    pub live: Option<LiveTranscript>,
}

impl TranscriptionJob {
    pub fn live(
        recording: Recording,
        settings: UserSettings,
        target_window: isize,
        live: Option<LiveTranscript>,
    ) -> Self {
        Self {
            recording,
            settings,
            target_window,
            skip_history: false,
            upload_id: None,
            live,
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
            live: None,
        }
    }
}

pub struct MeetingTranscriptionJob {
    pub id: String,
    pub title: String,
    pub audio_path: PathBuf,
    pub settings: UserSettings,
}

pub struct SearchAsrJob {
    pub recording: Recording,
    pub language: String,
    pub dictionary: Vec<String>,
    pub provider_url: String,
}

pub struct CompletedMeetingTranscription {
    pub id: String,
    pub transcript: String,
    pub notes: String,
    pub warning: Option<String>,
}

pub struct CompletedSearchAsr {
    pub query: String,
    pub provider_url: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub ready: bool,
    pub message: String,
    pub backend: String,
}

#[derive(Clone)]
pub struct EngineController {
    commands: mpsc::Sender<EngineCommand>,
    /// Shared with the engine thread: one connection pool, so a connection
    /// opened by `preconnect_cleanup` is the one cleanup reuses.
    client: Client,
    /// The running server's URL and backend, published by the engine thread.
    endpoint: Arc<Mutex<Option<(String, AsrModel)>>>,
}

impl EngineController {
    pub fn new(
        app: AppHandle,
        resource_dir: Option<PathBuf>,
        gpu_memory_management: bool,
        model: AsrModel,
    ) -> Self {
        let (commands, receiver) = mpsc::channel();
        set_desired_backend(model);
        let client = engine_client();
        let worker_client = client.clone();
        let endpoint = Arc::new(Mutex::new(None));
        let worker_endpoint = Arc::clone(&endpoint);
        std::thread::Builder::new()
            .name("pronto-engine".into())
            .spawn(move || {
                engine_worker(
                    app,
                    resource_dir,
                    gpu_memory_management,
                    model,
                    receiver,
                    worker_client,
                    worker_endpoint,
                )
            })
            .expect("failed to start transcription engine thread");
        Self {
            commands,
            client,
            endpoint,
        }
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

    pub fn transcribe_search(&self, job: SearchAsrJob) -> Result<(), String> {
        self.commands
            .send(EngineCommand::TranscribeSearch(job))
            .map_err(|_| "transcription engine stopped".into())
    }

    pub fn warm(&self) {
        let _ = self.commands.send(EngineCommand::Warm);
    }

    /// Starts transcribing a dictation's finished segments while it records.
    /// `snapshot(from_frame)` returns the audio captured since that frame, or
    /// `None` when recording has stopped or the capture path cannot tell.
    /// Parakeet only: Phonon's CPU engine would compete with itself.
    pub fn start_live_segments(
        &self,
        snapshot: impl Fn(usize) -> Result<Option<Recording>, String> + Send + 'static,
        language: String,
    ) -> LiveSegments {
        let stop = Arc::new(AtomicBool::new(false));
        let (client, endpoint, thread_stop) = (
            self.client.clone(),
            Arc::clone(&self.endpoint),
            Arc::clone(&stop),
        );
        let thread = std::thread::Builder::new()
            .name("pronto-live-segments".into())
            .spawn(move || {
                let mut segmenter = Segmenter::new();
                while !thread_stop.load(Ordering::Acquire) {
                    std::thread::sleep(SEGMENT_POLL);
                    if thread_stop.load(Ordering::Acquire) {
                        break;
                    }
                    let current = endpoint.lock().ok().and_then(|endpoint| endpoint.clone());
                    let Some((base_url, backend)) =
                        current.filter(|(_, backend)| backend.uses_gpu())
                    else {
                        continue;
                    };
                    match snapshot(segmenter.transcript.covered_frames) {
                        Ok(Some(pending)) => {
                            segmenter.step(&pending, &client, &base_url, backend, &language)
                        }
                        _ => break,
                    }
                }
                segmenter.transcript
            })
            .ok();
        LiveSegments { stop, thread }
    }

    /// Opens the DeepSeek connection (DNS, TCP, TLS) while the user is still
    /// speaking, so cleanup reuses it. After a few idle minutes the DNS
    /// lookup alone took 2.2-2.6 s, beyond the 2 s connect timeout, and
    /// cleanup silently fell back to local; a fresh connection otherwise costs
    /// ~110 ms. Sends only the key, never text, to the same provider.
    pub fn preconnect_cleanup(&self) {
        let client = self.client.clone();
        let _ = std::thread::Builder::new()
            .name("pronto-cleanup-connect".into())
            .spawn(move || {
                if let Some(key) = deepseek_key() {
                    let _ = client.get(DEEPSEEK_MODELS_URL).bearer_auth(key).send();
                }
            });
    }

    #[cfg(target_os = "macos")]
    pub fn warm_after_install(&self) {
        let _ = self.commands.send(EngineCommand::ModelInstalled);
    }

    /// Stop the running backend and warm `model` instead. Its packs must
    /// already be installed.
    pub fn switch_model(&self, model: AsrModel) {
        set_desired_backend(model);
        let _ = self.commands.send(EngineCommand::SwitchModel(model));
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
    TranscribeSearch(SearchAsrJob),
    Warm,
    #[cfg(target_os = "macos")]
    ModelInstalled,
    ConfigureGpuMemory(bool),
    SwitchModel(AsrModel),
}

struct GpuPressurePolicy {
    enabled: bool,
    low_readings: u8,
    last_activity: Instant,
    last_transition: Instant,
    model_bytes: u64,
    /// VRAM used by everything except the model, sampled right after it loaded.
    others_at_load: Option<u64>,
}

impl GpuPressurePolicy {
    fn new(enabled: bool, now: Instant, model_bytes: u64) -> Self {
        Self {
            enabled,
            low_readings: 0,
            last_activity: now,
            last_transition: now,
            model_bytes,
            others_at_load: None,
        }
    }

    /// Record a successful load: learn the model's real footprint and the
    /// VRAM other apps were using, so later growth can be attributed to them.
    fn note_loaded(&mut self, before: Option<MemoryInfo>, after: Option<MemoryInfo>, now: Instant) {
        if let (Some(before), Some(after)) = (before, after) {
            self.model_bytes = self.model_bytes.max(after.used.saturating_sub(before.used));
        }
        self.others_at_load = after.map(|after| after.used.saturating_sub(self.model_bytes));
        self.note_transition(now);
    }

    fn under_pressure(&self, memory: MemoryInfo) -> bool {
        if memory.free < Self::reserve_bytes(memory) {
            return true;
        }
        // A game reserves VRAM up to its budget and WDDM pages everything
        // else out instead of failing, so free memory rarely reaches zero
        // even while the whole system stutters. Treat a large jump in other
        // apps' usage as pressure once the model is what stands between them
        // and a comfortable reserve.
        let others = memory.used.saturating_sub(self.model_bytes);
        let grew = self
            .others_at_load
            .is_some_and(|baseline| others >= baseline.saturating_add(GPU_EXTERNAL_GROWTH));
        grew && memory.free < self.model_bytes.saturating_add(Self::reserve_bytes(memory))
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
        // Game loading makes VRAM bounce, so one healthy reading only backs
        // the count off instead of restarting it.
        if self.under_pressure(memory) {
            self.low_readings = self.low_readings.saturating_add(1);
        } else {
            self.low_readings = self.low_readings.saturating_sub(1);
        }
        self.low_readings >= GPU_PRESSURE_SAMPLES
    }
}

fn engine_worker(
    app: AppHandle,
    resource_dir: Option<PathBuf>,
    gpu_memory_management: bool,
    initial_model: AsrModel,
    receiver: mpsc::Receiver<EngineCommand>,
    client: Client,
    endpoint: Arc<Mutex<Option<(String, AsrModel)>>>,
) {
    let mut backend = initial_model;
    set_active_backend(backend);
    let gpu = GpuMemoryMonitor::new().ok();
    let before_load = gpu.as_ref().and_then(|gpu| gpu.memory_info().ok());
    emit_model_status(
        &app,
        false,
        &if cfg!(target_os = "macos") {
            "Checking Parakeet for Metal…".to_string()
        } else {
            format!(
                "Loading {} on {}…",
                backend.short_name(),
                device_phrase(backend)
            )
        },
    );
    let mut runtime = locate_runtime(resource_dir.as_deref(), backend);
    let mut policy = GpuPressurePolicy::new(
        gpu_memory_management,
        Instant::now(),
        minimum_model_bytes(runtime.as_ref().ok()),
    );
    let mut server = match runtime.as_ref() {
        Ok(runtime) => match SpeechServer::start(runtime) {
            Ok(server) => {
                policy.note_loaded(
                    before_load,
                    gpu.as_ref().and_then(|gpu| gpu.memory_info().ok()),
                    Instant::now(),
                );
                emit_model_status(&app, true, &ready_message(backend));
                Some(server)
            }
            Err(error) => {
                if error != SUPERSEDED {
                    emit_model_status(&app, false, &error);
                }
                None
            }
        },
        Err(error) => {
            emit_model_status(&app, false, error);
            None
        }
    };
    let mut last_start_failure =
        (server.is_none() && desired_backend() == backend).then(Instant::now);

    loop {
        if let Ok(mut endpoint) = endpoint.lock() {
            *endpoint = server
                .as_ref()
                .map(|server| (server.base_url.clone(), server.backend));
        }
        let command = if server.is_some() && policy.enabled && gpu.is_some() && backend.uses_gpu() {
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
                            // Status only: a persistent overlay notice here
                            // would sit on top of the game that caused it.
                            crate::set_model_status(
                                &app,
                                ModelStatus {
                                    ready: false,
                                    message: if cfg!(target_os = "macos") {
                                        "Parakeet released under unified-memory pressure"
                                    } else {
                                        "Parakeet released to protect GPU memory"
                                    }
                                    .into(),
                                    backend: active_backend().backend_label().into(),
                                },
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

        // Follow the user's latest choice before running anything queued
        // behind a switch, so dictations never wait on an abandoned backend.
        if desired_backend() != backend {
            adopt_backend(
                desired_backend(),
                &mut backend,
                &mut runtime,
                &mut server,
                &mut policy,
                &mut last_start_failure,
                resource_dir.as_deref(),
            );
        }

        match command.expect("received engine command") {
            #[cfg(target_os = "macos")]
            EngineCommand::ModelInstalled => {
                runtime = locate_runtime(resource_dir.as_deref(), backend);
                last_start_failure = None;
                if server.is_none() {
                    server = warm_server(
                        &app,
                        runtime.as_ref().map_err(String::as_str),
                        &mut policy,
                        gpu.as_ref(),
                        &mut last_start_failure,
                    );
                }
            }
            EngineCommand::SwitchModel(model) => {
                // A later switch has already been adopted above; this one is stale.
                if model != backend {
                    continue;
                }
                if server.is_none() {
                    server = warm_server(
                        &app,
                        runtime.as_ref().map_err(String::as_str),
                        &mut policy,
                        gpu.as_ref(),
                        &mut last_start_failure,
                    );
                }
            }
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
                            &format!(
                                "{} could not start; retrying shortly…",
                                backend.short_name()
                            ),
                        );
                        continue;
                    }
                    let can_load = gpu
                        .as_ref()
                        .and_then(|gpu| gpu.memory_info().ok())
                        .is_none_or(|memory| !backend.uses_gpu() || policy.can_load(memory));
                    if can_load {
                        server = warm_server(
                            &app,
                            runtime.as_ref().map_err(String::as_str),
                            &mut policy,
                            gpu.as_ref(),
                            &mut last_start_failure,
                        );
                    } else {
                        emit_model_status(
                            &app,
                            false,
                            if cfg!(target_os = "macos") {
                                "Waiting for available memory…"
                            } else {
                                "Waiting for available GPU memory…"
                            },
                        );
                    }
                }
            }
            EngineCommand::Transcribe(job) => {
                let started = Instant::now();
                policy.note_activity(started);
                // A switch made while this dictation waited on a warm-up wins:
                // the warm-up gives way and the dictation runs on the new
                // backend. Two passes cover one switch per dictation.
                for _ in 0..2 {
                    let recent_start_failure = last_start_failure
                        .is_some_and(|failed| failed.elapsed() < WARM_RETRY_COOLDOWN);
                    if server.is_some() || recent_start_failure {
                        break;
                    }
                    let deadline = Instant::now() + GPU_WAIT_TIMEOUT;
                    let mut waiting_emitted = false;
                    loop {
                        let can_load = gpu
                            .as_ref()
                            .and_then(|gpu| gpu.memory_info().ok())
                            .is_none_or(|memory| !backend.uses_gpu() || policy.can_load(memory));
                        if can_load {
                            server = warm_server(
                                &app,
                                runtime.as_ref().map_err(String::as_str),
                                &mut policy,
                                gpu.as_ref(),
                                &mut last_start_failure,
                            );
                            break;
                        }
                        if !waiting_emitted {
                            emit_model_status(
                                &app,
                                false,
                                if cfg!(target_os = "macos") {
                                    "Waiting for available memory…"
                                } else {
                                    "Waiting for available GPU memory…"
                                },
                            );
                            waiting_emitted = true;
                        }
                        if Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(GPU_POLL_INTERVAL);
                    }
                    if server.is_some() || desired_backend() == backend {
                        break;
                    }
                    adopt_backend(
                        desired_backend(),
                        &mut backend,
                        &mut runtime,
                        &mut server,
                        &mut policy,
                        &mut last_start_failure,
                        resource_dir.as_deref(),
                    );
                }
                let recent_start_failure =
                    last_start_failure.is_some_and(|failed| failed.elapsed() < WARM_RETRY_COOLDOWN);
                let result = match server.as_mut() {
                    Some(server) => process_job(&client, server, job, started),
                    // A missing install explains itself better than engine.log.
                    None if runtime.is_err() => Err(runtime.as_ref().err().cloned().unwrap_or_default()),
                    None if recent_start_failure => Err(
                        format!("{} failed to start. See engine.log in Pronto's local data folder, then try again shortly.", backend.short_name()),
                    ),
                    None => Err(if cfg!(target_os = "macos") {
                        "Not enough available memory to load Parakeet. Dictation was not transcribed.".to_string()
                    } else if backend.uses_gpu() {
                        "Not enough GPU memory to load Parakeet. Dictation was not transcribed.".to_string()
                    } else {
                        "Phonon could not start. Dictation was not transcribed.".to_string()
                    }),
                };
                crate::complete_transcription(&app, result);
            }
            EngineCommand::TranscribeMeeting(job) => {
                let started = Instant::now();
                policy.note_activity(started);
                let meeting_id = job.id.clone();
                if server.is_none() {
                    server = warm_server(
                        &app,
                        runtime.as_ref().map_err(String::as_str),
                        &mut policy,
                        gpu.as_ref(),
                        &mut last_start_failure,
                    );
                }
                let result = match server.as_mut() {
                    Some(server) => process_meeting_job(&client, server, job, &app),
                    None => Err(format!(
                        "{} could not start to process the meeting",
                        backend.short_name()
                    )),
                };
                match result {
                    Ok(completed) => crate::complete_meeting_transcription(&app, Ok(completed)),
                    Err(error) => crate::fail_meeting_transcription(&app, &meeting_id, error),
                }
            }
            EngineCommand::TranscribeSearch(job) => {
                let started = Instant::now();
                policy.note_activity(started);
                if server.is_none() {
                    server = warm_server(
                        &app,
                        runtime.as_ref().map_err(String::as_str),
                        &mut policy,
                        gpu.as_ref(),
                        &mut last_start_failure,
                    );
                }
                let result = match server.as_mut() {
                    Some(server) => process_search_asr(&client, server, job),
                    None => Err(format!(
                        "{} could not start for voice search",
                        backend.short_name()
                    )),
                };
                crate::complete_search_asr(&app, result);
            }
        }
    }

    if let Some(server) = server.as_mut() {
        server.stop();
    }
}

/// The engine's one HTTP client: local speech server and DeepSeek cleanup.
fn engine_client() -> Client {
    Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(12))
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_nodelay(true)
        .build()
        .expect("failed to build HTTP client")
}

/// Stop the running backend and point the engine at `model`. Resets the
/// start-failure cooldown, which belonged to the previous backend.
fn adopt_backend(
    model: AsrModel,
    backend: &mut AsrModel,
    runtime: &mut Result<RuntimePaths, String>,
    server: &mut Option<SpeechServer>,
    policy: &mut GpuPressurePolicy,
    last_start_failure: &mut Option<Instant>,
    resource_dir: Option<&Path>,
) {
    if let Some(mut previous) = server.take() {
        previous.stop();
    }
    *backend = model;
    set_active_backend(model);
    *runtime = locate_runtime(resource_dir, model);
    policy.model_bytes = minimum_model_bytes(runtime.as_ref().ok());
    policy.note_transition(Instant::now());
    *last_start_failure = None;
}

/// Start the speech server, recording a failed start for the retry
/// cooldown. A start abandoned for a model switch is not a failure.
fn warm_server(
    app: &AppHandle,
    runtime: Result<&RuntimePaths, &str>,
    policy: &mut GpuPressurePolicy,
    gpu: Option<&GpuMemoryMonitor>,
    last_start_failure: &mut Option<Instant>,
) -> Option<SpeechServer> {
    let runtime = match runtime {
        Ok(runtime) => runtime,
        Err(error) => {
            emit_model_status(app, false, error);
            *last_start_failure = Some(Instant::now());
            return None;
        }
    };
    emit_model_status(
        app,
        false,
        &format!(
            "Warming {} on {}{}…",
            runtime.backend.short_name(),
            device_phrase(runtime.backend),
            if runtime.backend == AsrModel::Phonon {
                ", about a minute"
            } else {
                ""
            }
        ),
    );
    let memory = || gpu.and_then(|gpu| gpu.memory_info().ok());
    let before = memory();
    match SpeechServer::start(runtime) {
        Ok(server) => {
            policy.note_loaded(before, memory(), Instant::now());
            emit_model_status(app, true, &ready_message(runtime.backend));
            *last_start_failure = None;
            Some(server)
        }
        Err(error) if error == SUPERSEDED => None,
        Err(error) => {
            emit_model_status(app, false, &error);
            *last_start_failure = Some(Instant::now());
            None
        }
    }
}

fn ready_message(model: AsrModel) -> String {
    format!("{} is warm and ready", model.short_name())
}

/// Free GPU memory required before (re)loading: 1.5x the model on disk.
fn minimum_model_bytes(runtime: Option<&RuntimePaths>) -> u64 {
    runtime
        .and_then(|runtime| runtime.model.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len().saturating_mul(3) / 2)
        .unwrap_or(1024 * MIB)
}

fn process_job(
    client: &Client,
    server: &mut SpeechServer,
    mut job: TranscriptionJob,
    started: Instant,
) -> Result<CompletedTranscription, String> {
    let audio_ms = job.recording.samples.len() as u128 * 1_000
        / (job.recording.sample_rate as u128 * job.recording.channels.max(1) as u128);
    let asr_started = Instant::now();
    let raw = match job.live.take() {
        // Segments transcribed while recording cover the start; only the
        // audio after the last pause is left.
        Some(live) if !live.failed && live.covered_frames > 0 => {
            let channels = job.recording.channels.max(1) as usize;
            let start = (live.covered_frames * channels).min(job.recording.samples.len());
            let tail = Recording {
                samples: job.recording.samples[start..].to_vec(),
                sample_rate: job.recording.sample_rate,
                channels: job.recording.channels,
            };
            let tail_text =
                match transcribe_recording(client, server, &tail, &job.settings.language) {
                    Ok(text) => text,
                    Err(error) if error == NO_SPEECH => String::new(),
                    Err(error) => return Err(error),
                };
            let mut texts = live.texts;
            if !tail_text.is_empty() {
                texts.push(tail_text);
            }
            texts.join(" ")
        }
        _ => transcribe_recording(client, server, &job.recording, &job.settings.language)?,
    };
    let asr_ms = asr_started.elapsed().as_millis();
    if raw.is_empty() {
        return Err("No speech was detected".into());
    }

    let locally_cleaned = local_cleanup(&raw);
    let dictionary_fallback = apply_dictionary(&locally_cleaned, &job.settings.dictionary);
    let cleanup_started = Instant::now();
    let (final_text, cleanup_applied, cleanup_warning) = if job.settings.cleanup_enabled {
        match cleanup_provider::resolve(&job.settings) {
            Ok(target) => {
                let prompt = job
                    .settings
                    .cleanup_prompt
                    .as_deref()
                    .unwrap_or(DEFAULT_CLEANUP_PROMPT);
                match ai_cleanup(
                    client,
                    &target,
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
            Err(missing) => (
                dictionary_fallback,
                false,
                Some(format!("{missing} to enable AI cleanup")),
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

fn process_search_asr(
    client: &Client,
    server: &SpeechServer,
    job: SearchAsrJob,
) -> Result<CompletedSearchAsr, String> {
    // Voice search uses ASR + light local cleanup only — never AI rewrite
    // and never the dictation history / insertion path.
    let raw = transcribe_recording(client, server, &job.recording, &job.language)?;
    if raw.is_empty() {
        return Err("No speech was detected".into());
    }
    let query = apply_dictionary(&local_cleanup(&raw), &job.dictionary);
    if query.trim().is_empty() {
        return Err("No speech was detected".into());
    }
    Ok(CompletedSearchAsr {
        query,
        provider_url: job.provider_url,
    })
}

fn transcribe_recording(
    client: &Client,
    server: &SpeechServer,
    recording: &Recording,
    language: &str,
) -> Result<String, String> {
    transcribe_recording_url(
        client,
        &server.base_url,
        server.backend,
        recording,
        language,
    )
}

fn transcribe_recording_url(
    client: &Client,
    base_url: &str,
    backend: AsrModel,
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
        .text("response_format", "json");
    // Phonon's server checks `model` against its own id and is English-only,
    // so both fields are Parakeet-only.
    if backend == AsrModel::Parakeet {
        form = form.text("model", "parakeet");
    }
    if backend == AsrModel::Parakeet && language != "auto" {
        form = form.text("language", language.to_string());
    }
    let response = client
        .post(format!("{base_url}/v1/audio/transcriptions"))
        .timeout(Duration::from_secs(
            ((audio_ms / 10_000) + 30).clamp(30, 600) as u64,
        ))
        .multipart(form)
        .send()
        .map_err(|e| format!("Local transcription request failed: {e}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().unwrap_or_default();
        return Err(format!(
            "{} returned {status}: {detail}",
            backend.short_name()
        ));
    }
    Ok(response
        .json::<AsrResponse>()
        .map_err(|e| format!("Invalid {} response: {e}", backend.short_name()))?
        .text
        .trim()
        .to_string())
}

/// Dictation pauses this long split the recording into segments that are
/// transcribed while the user keeps talking; the first segment needs this
/// much audio, so short dictations stay one request.
const SEGMENT_PAUSE: Duration = Duration::from_millis(800);
const SEGMENT_MIN_AUDIO: Duration = Duration::from_secs(3);
/// How often the recording thread looks for a new pause.
const SEGMENT_POLL: Duration = Duration::from_millis(250);

/// 16 kHz sample index at which to cut `mono` (16 kHz, from the last cut):
/// the middle of the first pause of at least `SEGMENT_PAUSE` that follows
/// speech and at least `SEGMENT_MIN_AUDIO` of audio. Speech is detected per
/// 20 ms frame exactly as `trim_silence` does, so the cut lands in audio
/// that trimming would discard anyway.
fn find_segment_cut(mono: &[f32]) -> Option<usize> {
    let frame = 16_000 / 50;
    let pause_frames = (SEGMENT_PAUSE.as_millis() / 20) as usize;
    let min_frames = (SEGMENT_MIN_AUDIO.as_millis() / 20) as usize;
    let mut heard = false;
    let mut quiet_since = None;
    for (index, chunk) in mono.chunks_exact(frame).enumerate() {
        let rms = (chunk.iter().map(|sample| sample * sample).sum::<f32>() / frame as f32).sqrt();
        if rms > 0.004 {
            heard = true;
            quiet_since = None;
            continue;
        }
        let start = *quiet_since.get_or_insert(index);
        if heard && start >= min_frames && index + 1 - start >= pause_frames {
            return Some((start + pause_frames / 2) * frame);
        }
    }
    None
}

/// Text of the segments transcribed during a recording, and how much of the
/// recording (frames at its native rate) they cover. `process_job`
/// transcribes only the rest; a failed segment means the whole recording.
pub struct LiveTranscript {
    texts: Vec<String>,
    covered_frames: usize,
    failed: bool,
}

/// One recording's segmentation state. `step` is called with the audio
/// captured since the last cut; production calls it from `LiveSegments`,
/// the benchmark from a simulated recording.
struct Segmenter {
    transcript: LiveTranscript,
}

impl Segmenter {
    fn new() -> Self {
        Self {
            transcript: LiveTranscript {
                texts: Vec::new(),
                covered_frames: 0,
                failed: false,
            },
        }
    }

    fn step(
        &mut self,
        pending: &Recording,
        client: &Client,
        base_url: &str,
        backend: AsrModel,
        language: &str,
    ) {
        if self.transcript.failed || pending.sample_rate == 0 || pending.channels == 0 {
            return;
        }
        let channels = pending.channels as usize;
        let mono = resample_linear(
            &downmix(&pending.samples, channels),
            pending.sample_rate,
            16_000,
        );
        let Some(cut) = find_segment_cut(&mono) else {
            return;
        };
        let frames = (cut as u64 * pending.sample_rate as u64 / 16_000) as usize;
        let segment = Recording {
            samples: pending.samples[..(frames * channels).min(pending.samples.len())].to_vec(),
            sample_rate: pending.sample_rate,
            channels: pending.channels,
        };
        match transcribe_recording_url(client, base_url, backend, &segment, language) {
            Ok(text) => {
                if !text.is_empty() {
                    self.transcript.texts.push(text);
                }
                self.transcript.covered_frames += frames;
            }
            Err(error) if error == NO_SPEECH => self.transcript.covered_frames += frames,
            Err(_) => self.transcript.failed = true,
        }
    }
}

/// Transcribes a dictation's finished segments while it is still being
/// recorded (Parakeet only), so stopping leaves only the audio after the
/// last pause. Each segment also wakes the GPU, which an idle laptop keeps
/// in P8. Dropping it stops the thread.
pub struct LiveSegments {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<LiveTranscript>>,
}

impl LiveSegments {
    /// Recording has stopped: wait for an in-flight segment and return the
    /// transcript so far.
    pub fn finish(mut self) -> Option<LiveTranscript> {
        self.stop.store(true, Ordering::Release);
        self.thread.take()?.join().ok()
    }
}

impl Drop for LiveSegments {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn process_meeting_job(
    client: &Client,
    server: &SpeechServer,
    job: MeetingTranscriptionJob,
    app: &AppHandle,
) -> Result<CompletedMeetingTranscription, String> {
    let transcript_parts = transcribe_meeting_chunks(
        client,
        &server.base_url,
        server.backend,
        &job.audio_path,
        &job.settings.language,
        MEETING_WORKERS,
        &|finished, total| {
            let _ = tauri::Emitter::emit(
                app,
                "meeting-transcription-progress",
                serde_json::json!({
                    "id": job.id.as_str(),
                    "done": finished,
                    "total": total,
                }),
            );
        },
    )?;
    let transcript = apply_dictionary(
        &local_cleanup(&transcript_parts.join(" ")),
        &job.settings.dictionary,
    );
    if transcript.is_empty() {
        return Err("No speech was detected in the meeting".into());
    }
    let (notes, warning) = match cleanup_provider::resolve(&job.settings) {
        Ok(target) => match generate_meeting_notes(client, &target, &job.title, &transcript) {
            Ok(notes) => (notes, None),
            Err(error) => (local_meeting_notes(&job.title, &transcript), Some(error)),
        },
        Err(missing) => (
            local_meeting_notes(&job.title, &transcript),
            Some(format!("{missing} for structured AI meeting notes")),
        ),
    };
    Ok(CompletedMeetingTranscription {
        id: job.id,
        transcript,
        notes,
        warning,
    })
}

/// Transcribes a saved meeting WAV in 120 s chunks; returns the non-empty
/// chunk transcripts in order. `on_progress(done, total)` follows each chunk.
fn transcribe_meeting_chunks(
    client: &Client,
    base_url: &str,
    backend: AsrModel,
    audio_path: &Path,
    language: &str,
    workers: usize,
    on_progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<Vec<String>, String> {
    const CHUNK_SAMPLES: usize = 16_000 * 120;
    const CHUNK_BYTES: usize = CHUNK_SAMPLES * 2;
    let audio_len = fs::metadata(audio_path)
        .map_err(|error| format!("Could not inspect meeting audio: {error}"))?
        .len();
    let pcm_bytes = audio_len.saturating_sub(44) & !1;
    let total = pcm_bytes.div_ceil(CHUNK_BYTES as u64) as usize;
    if total == 0 {
        return Err("The saved meeting audio has no samples".into());
    }
    // Bounded parallel transcription, order preserved. Any chunk failure
    // fails the whole job. Each worker loads only its current chunk, so a
    // two-hour recording does not sit in memory beside the warmed model.
    let slots: Vec<Mutex<Option<Result<String, String>>>> =
        (0..total).map(|_| Mutex::new(None)).collect();
    let done = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for worker in 0..workers.min(total.max(1)) {
            // Fresh shared references per worker: the `move` closure takes
            // copies of these while the owned values stay put for later use.
            let start = worker;
            let slot_list = &slots;
            let counter = &done;
            let http = client;
            scope.spawn(move || {
                let mut index = start;
                while index < total {
                    let result = read_meeting_chunk(audio_path, index, CHUNK_BYTES, pcm_bytes)
                        .and_then(|raw| {
                            let samples = raw
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .map(|v| i16::from_le_bytes([v[0], v[1]]) as f32 / 32768.0)
                                .collect();
                            transcribe_recording_url(
                                http,
                                base_url,
                                backend,
                                &Recording {
                                    samples,
                                    sample_rate: 16_000,
                                    channels: 1,
                                },
                                language,
                            )
                        });
                    if let Ok(mut slot) = slot_list[index].lock() {
                        *slot = Some(result);
                    }
                    let finished = counter.fetch_add(1, Ordering::Relaxed) + 1;
                    on_progress(finished, total);
                    index += workers;
                }
            });
        }
    });
    let mut transcript_parts = Vec::with_capacity(total);
    for slot in &slots {
        match slot
            .lock()
            .map_err(|_| "meeting progress lock poisoned")?
            .take()
        {
            Some(Ok(text)) if !text.is_empty() => transcript_parts.push(text),
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(error),
            None => return Err("Meeting transcription was interrupted".into()),
        }
    }
    Ok(transcript_parts)
}

fn read_meeting_chunk(
    path: &Path,
    index: usize,
    chunk_bytes: usize,
    pcm_bytes: u64,
) -> Result<Vec<u8>, String> {
    let offset = (index as u64)
        .checked_mul(chunk_bytes as u64)
        .ok_or_else(|| "Meeting audio is too large".to_string())?;
    let available = pcm_bytes
        .checked_sub(offset)
        .ok_or_else(|| "Meeting chunk is outside the saved audio".to_string())?;
    if available == 0 {
        return Err("Meeting chunk is outside the saved audio".into());
    }
    let count = available.min(chunk_bytes as u64) as usize;
    let mut file =
        fs::File::open(path).map_err(|error| format!("Could not open meeting audio: {error}"))?;
    file.seek(SeekFrom::Start(44 + offset))
        .map_err(|error| error.to_string())?;
    let mut bytes = vec![0u8; count];
    file.read_exact(&mut bytes)
        .map_err(|error| format!("Meeting audio was truncated: {error}"))?;
    Ok(bytes)
}

fn generate_meeting_notes(
    client: &Client,
    target: &CleanupTarget,
    title: &str,
    transcript: &str,
) -> Result<String, String> {
    const PROMPT: &str = "Create concise Markdown meeting notes grounded only in the transcript. Use sections: Summary, Decisions, Action items, Open questions, and Key points. Never invent an owner, deadline, decision, or fact. Write 'None captured' when a section has no evidence.";
    // Partial notes resolve concurrently and assemble in order, so long
    // transcripts don't pay one network round-trip per chunk in series.
    // Any failure still fails the whole step, as before.
    let text_chunks = split_utf8_chunks(transcript, 36_000);
    let mut ordered: Vec<(usize, String)> = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for (index, chunk) in text_chunks.iter().enumerate() {
            handles.push(scope.spawn(move || {
                cleanup_with_dictionary(client, target, chunk, &[], PROMPT, 3000)
                    .map(|text| (index, text))
            }));
        }
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| "Meeting notes worker stopped".to_string())?
            })
            .collect::<Result<Vec<(usize, String)>, String>>()
    })?;
    ordered.sort_by_key(|(index, _)| *index);
    let mut partials: Vec<String> = ordered.into_iter().map(|(_, text)| text).collect();
    if partials.len() == 1 {
        return Ok(partials.remove(0));
    }
    let combined = format!(
        "Meeting: {title}\n\nPARTIAL NOTES:\n{}",
        partials.join("\n\n---\n\n")
    );
    cleanup_with_dictionary(client, target, &combined, &[], PROMPT, 5000)
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
        .as_chunks::<2>()
        .0
        .iter()
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
    backend: AsrModel,
    executable: PathBuf,
    /// Parakeet: the GGUF file. Phonon: the unpacked model folder.
    model: PathBuf,
}

struct SpeechServer {
    child: Child,
    base_url: String,
    backend: AsrModel,
    #[cfg(windows)]
    _job: ProcessJob,
}

/// Pronto dictates into other apps, so it and its windowless server are
/// background processes, and Windows applies EcoQoS to them: E-cores at low
/// clocks. Phonon's CPU encoder then runs 3-4x slower (measured on an
/// i5-12450H: median 3.2 s vs 0.74 s, server CPU 31% vs 74%; see
/// scripts/benchmarks/README.md). Opting out pins nothing; the OS still
/// schedules freely.
#[cfg(windows)]
fn opt_out_of_power_throttling(child: &Child) -> Result<(), String> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::{
        ProcessPowerThrottling, SetProcessInformation, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        PROCESS_POWER_THROTTLING_STATE,
    };
    // Controlled bits with a zero state mean "never throttle", overriding the heuristic.
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
            | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        StateMask: 0,
    };
    unsafe {
        SetProcessInformation(
            HANDLE(child.as_raw_handle()),
            ProcessPowerThrottling,
            &state as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    }
    .map_err(|error| error.to_string())
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
        let mut command = server_command(runtime, port)?;
        command
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log.try_clone().map_err(|error| error.to_string())?,
            ))
            .stderr(Stdio::from(stderr_log));
        // Both runtimes are console-subsystem executables. Redirecting their
        // streams does not suppress the console host; CREATE_NO_WINDOW does.
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        let mut child = command
            .spawn()
            .map_err(|error| format!("Could not start local speech runtime: {error}"))?;
        #[cfg(target_os = "macos")]
        register_speech_child(child.id());
        #[cfg(windows)]
        let job = match ProcessJob::attach(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        #[cfg(windows)]
        if runtime.backend == AsrModel::Phonon {
            if let Err(error) = opt_out_of_power_throttling(&child) {
                let _ = writeln!(log, "[pronto] could not disable power throttling: {error}");
            }
        }

        let base_url = format!("http://127.0.0.1:{port}");
        let health_client = Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|error| error.to_string())?;
        let backend = runtime.backend;
        let name = backend.short_name();
        // Phonon imports Python + torch and expands its weights on load,
        // which takes over a minute even on a fast CPU.
        let startup_limit = if backend == AsrModel::Phonon { 300 } else { 90 };
        let started = Instant::now();
        let deadline = started + Duration::from_secs(startup_limit);
        while Instant::now() < deadline {
            if desired_backend() != backend {
                let _ = child.kill();
                let _ = child.wait();
                let _ = writeln!(log, "[pronto] {name} start abandoned for a model switch");
                return Err(SUPERSEDED.into());
            }
            if let Some(status) = child
                .try_wait()
                .map_err(|error| format!("Could not inspect speech runtime: {error}"))?
            {
                let detail = log_tail(&mut log);
                return Err(format!("{name} exited during startup ({status}).{detail}"));
            }
            if health_client
                .get(format!("{base_url}/health"))
                .send()
                .map(|response| response.status().is_success())
                .unwrap_or(false)
            {
                let loaded = started.elapsed().as_secs_f32();
                let server = Self {
                    child,
                    base_url,
                    backend,
                    #[cfg(windows)]
                    _job: job,
                };
                // Phonon's first decode pays ~3 s of one-time setup; spend it
                // here so the user's first dictation does not.
                if backend == AsrModel::Phonon {
                    let warm = Instant::now();
                    let _ = transcribe_recording_url(
                        &health_client,
                        &server.base_url,
                        backend,
                        &warm_up_clip(),
                        "en",
                    );
                    let _ = writeln!(
                        log,
                        "[pronto] {name} warm-up decode {:.1}s",
                        warm.elapsed().as_secs_f32()
                    );
                }
                let _ = writeln!(log, "[pronto] {name} ready after {loaded:.1}s");
                return Ok(server);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let mut child = child;
        let _ = child.kill();
        let detail = log_tail(&mut log);
        Err(format!(
            "{name} did not finish loading within {startup_limit} seconds.{detail}"
        ))
    }
}

/// Two seconds of a quiet 220 Hz tone: loud enough to survive silence
/// trimming, so the warm-up runs the whole encoder and decoder.
fn warm_up_clip() -> Recording {
    let rate = 16_000u32;
    let samples = (0..rate * 2)
        .map(|index| (index as f32 * 220.0 * std::f32::consts::TAU / rate as f32).sin() * 0.05)
        .collect();
    Recording {
        samples,
        sample_rate: rate,
        channels: 1,
    }
}

impl Drop for SpeechServer {
    fn drop(&mut self) {
        // Child::drop alone leaves the local server running after Pronto exits.
        let _ = self.child.kill();
        let _ = self.child.wait();
        #[cfg(target_os = "macos")]
        {
            let _ = SPEECH_CHILD_PID.compare_exchange(
                self.child.id() as i32,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
}

#[cfg(target_os = "macos")]
static SPEECH_CHILD_PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

#[cfg(target_os = "macos")]
extern "C" fn stop_speech_child_at_exit() {
    let pid = SPEECH_CHILD_PID.swap(0, Ordering::AcqRel);
    if pid > 0 {
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
}

#[cfg(target_os = "macos")]
extern "C" fn stop_speech_child_on_signal(signal: i32) {
    let pid = SPEECH_CHILD_PID.load(Ordering::Acquire);
    if pid > 0 {
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    unsafe { libc::_exit(128 + signal) };
}

#[cfg(target_os = "macos")]
fn register_speech_child(pid: u32) {
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| unsafe {
        libc::atexit(stop_speech_child_at_exit);
        libc::signal(
            libc::SIGTERM,
            stop_speech_child_on_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            stop_speech_child_on_signal as *const () as libc::sighandler_t,
        );
    });
    SPEECH_CHILD_PID.store(pid as i32, Ordering::Release);
}

fn engine_log_path() -> PathBuf {
    crate::platform_paths::log_dir().join("engine.log")
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

fn server_command(runtime: &RuntimePaths, port: u16) -> Result<Command, String> {
    let port = port.to_string();
    match runtime.backend {
        AsrModel::Parakeet => {
            let bin_dir = runtime
                .executable
                .parent()
                .ok_or_else(|| "Invalid NeMo Speech runtime path".to_string())?;
            let mut command = Command::new(&runtime.executable);
            command
                .args([
                    "serve",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &port,
                    "--threads",
                    "1",
                    "--no-ui",
                    "--device",
                    if cfg!(target_os = "macos") {
                        "metal"
                    } else {
                        "cuda:0"
                    },
                    "--asr-model",
                ])
                .arg(&runtime.model)
                .current_dir(bin_dir);
            Ok(command)
        }
        AsrModel::Phonon => {
            // `fermion serve <dir>` serves an unpacked local Phonon profile;
            // the environment pins the CPU engine and forbids any network
            // lookup, so the runtime only ever reads the verified pack.
            let pack_root = runtime
                .executable
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| "Invalid Phonon runtime path".to_string())?;
            let mut command = Command::new(&runtime.executable);
            command
                // -I isolates from user site-packages and PYTHON* variables.
                // Bytecode stays cached in the pack: without it every start
                // recompiles torch and transformers (~35 s of a ~80 s load).
                .args(["-I", "-X", "utf8", "-m", "fermion.cli", "serve"])
                .arg(&runtime.model)
                .args(["--host", "127.0.0.1", "--port", &port])
                .env("FERMION_DEVICE", "cpu")
                .env("FERMION_CACHE_DIR", pack_root.join("cache"))
                .env("HF_HUB_OFFLINE", "1")
                .env("HF_HUB_DISABLE_TELEMETRY", "1")
                .env("TRANSFORMERS_OFFLINE", "1")
                .current_dir(pack_root);
            Ok(command)
        }
    }
}

/// True when an older NSIS install shipped the CUDA runtime beside the
/// executable, which satisfies the `nemo-speech-cuda` pack.
pub fn bundled_cuda_runtime(resource_dir: Option<&Path>) -> bool {
    runtime_roots(resource_dir)
        .iter()
        .any(|root| root.join(NEMO_SPEECH_EXE).is_file())
}

#[cfg(target_os = "macos")]
const NEMO_SPEECH_EXE: &str = "runtime/nemo-speech/bin/nemo-speech";
#[cfg(not(target_os = "macos"))]
const NEMO_SPEECH_EXE: &str = "runtime/nemo-speech/bin/nemo-speech.exe";

fn runtime_roots(resource_dir: Option<&Path>) -> Vec<PathBuf> {
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
    roots
}

fn locate_runtime(resource_dir: Option<&Path>, backend: AsrModel) -> Result<RuntimePaths, String> {
    match backend {
        AsrModel::Parakeet => locate_parakeet(resource_dir),
        AsrModel::Phonon => locate_phonon(),
    }
}

fn locate_parakeet(resource_dir: Option<&Path>) -> Result<RuntimePaths, String> {
    let data = crate::platform_paths::data_dir();
    let roots = runtime_roots(resource_dir);
    // Bundled runtime first (NSIS installs, dev checkouts), then the
    // downloadable CUDA runtime pack.
    let mut executables: Vec<PathBuf> = roots
        .iter()
        .map(|root| root.join(NEMO_SPEECH_EXE))
        .collect();
    if cfg!(windows) {
        executables.push(data.join(CUDA_RUNTIME_PACK_EXE));
    }
    #[cfg(target_os = "macos")]
    let models = vec![data.join("models").join(PARAKEET_MODEL)];
    #[cfg(not(target_os = "macos"))]
    let models: Vec<PathBuf> = std::iter::once(data.join("models").join(PARAKEET_MODEL))
        .chain(
            roots
                .iter()
                .map(|root| root.join("models").join(PARAKEET_MODEL)),
        )
        .collect();
    #[cfg(target_os = "macos")]
    let model_ready = |model: &Path| crate::model_provision::verified_model(model);
    #[cfg(not(target_os = "macos"))]
    let model_ready = |model: &Path| model.is_file();

    let executable = executables.into_iter().find(|path| path.is_file());
    let model = models.into_iter().find(|path| model_ready(path));
    if let (Some(executable), Some(model)) = (executable, model) {
        return Ok(RuntimePaths {
            backend: AsrModel::Parakeet,
            executable,
            model,
        });
    }
    #[cfg(target_os = "macos")]
    return Err(format!("Parakeet runtime or verified model ({PARAKEET_MODEL}) is unavailable. Check the model download in Settings."));
    #[cfg(not(target_os = "macos"))]
    Err("Parakeet isn't installed yet. Download it from Settings, Advanced.".into())
}

fn locate_phonon() -> Result<RuntimePaths, String> {
    if cfg!(target_os = "macos") {
        return Err("Phonon is available on Windows only.".into());
    }
    let data = crate::platform_paths::data_dir();
    // Developers can point at any Python with fermion-research installed.
    let executable = std::env::var_os("PRONTO_PHONON_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| data.join(PHONON_PYTHON));
    let model = data.join(PHONON_MODEL_DIR);
    // fermion accepts a local folder holding both files side by side.
    let model_ready =
        model.join("config.json").is_file() && model.join("packed_manifest.json").is_file();
    if executable.is_file() && model_ready {
        return Ok(RuntimePaths {
            backend: AsrModel::Phonon,
            executable,
            model,
        });
    }
    Err("Phonon isn't installed yet. Download it from Settings, Advanced.".into())
}

#[derive(Deserialize)]
struct AsrResponse {
    text: String,
}

pub(crate) fn ai_cleanup(
    client: &Client,
    target: &CleanupTarget,
    transcript: &str,
    dictionary: &[String],
    system_prompt: &str,
) -> Result<String, String> {
    cleanup_with_dictionary(client, target, transcript, dictionary, system_prompt, 768)
}

pub(crate) fn ai_longform_cleanup(
    client: &Client,
    target: &CleanupTarget,
    transcript: &str,
    dictionary: &[String],
) -> Result<String, String> {
    // Long interviews need room to breathe: scale output budget with input
    // length instead of the 768-token cap used for short dictations.
    let words = transcript.split_whitespace().count().max(1);
    let max_tokens = (words * 2 + 500).clamp(1500, 8192) as u32;
    cleanup_with_dictionary(
        client,
        target,
        transcript,
        dictionary,
        DEFAULT_LONGFORM_CLEANUP_PROMPT,
        max_tokens,
    )
}

fn cleanup_with_dictionary(
    client: &Client,
    target: &CleanupTarget,
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
    cleanup_provider::complete(
        client,
        target,
        system_prompt,
        &format!("USER DICTIONARY (optional spelling hints):\n{dictionary}\n\nRAW TRANSCRIPT:\n{transcript}"),
        max_tokens,
    )
}

fn recording_to_wav(recording: &Recording) -> Result<Vec<u8>, String> {
    if recording.sample_rate == 0 || recording.channels == 0 {
        return Err("Microphone returned an invalid audio format".into());
    }
    let mono = downmix(&recording.samples, recording.channels as usize);
    let mono = resample_linear(&mono, recording.sample_rate, 16_000);
    let mono = trim_silence(&mono, 16_000);
    if mono.len() < 1_600 {
        return Err(NO_SPEECH.into());
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
            backend: active_backend().backend_label().into(),
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
    fn meeting_chunks_are_read_on_demand_in_order_and_detect_truncation() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "pronto-meeting-chunks-{}-{unique}.wav",
            std::process::id()
        ));
        let mut bytes = vec![0u8; 44];
        for value in 0i16..10 {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        fs::write(&path, &bytes).unwrap();
        assert_eq!(read_meeting_chunk(&path, 0, 8, 20).unwrap(), bytes[44..52]);
        assert_eq!(read_meeting_chunk(&path, 1, 8, 20).unwrap(), bytes[52..60]);
        assert_eq!(read_meeting_chunk(&path, 2, 8, 20).unwrap(), bytes[60..64]);
        assert!(read_meeting_chunk(&path, 3, 8, 20).is_err());
        fs::write(&path, &bytes[..60]).unwrap();
        assert!(read_meeting_chunk(&path, 2, 8, 20).is_err());
        fs::remove_file(path).unwrap();
    }

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

    /// Answer one transcription request with `text` and return the raw request.
    fn transcribe_against_fake_server(backend: AsrModel, language: &str) -> (String, Vec<u8>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers.lines().find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    });
                    if content_length.is_some_and(|length| request.len() >= header_end + 4 + length)
                    {
                        break;
                    }
                }
            }
            let body = r#"{"text":"Pronto works on this Mac."}"#;
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            request
        });
        let recording = Recording {
            samples: vec![0.25; 1_600],
            sample_rate: 16_000,
            channels: 1,
        };
        let client = Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let text = transcribe_recording_url(
            &client,
            &format!("http://{address}"),
            backend,
            &recording,
            language,
        )
        .unwrap();
        (text, server.join().unwrap())
    }

    fn has_field(request: &[u8], name: &str) -> bool {
        let needle = format!("name=\"{name}\"");
        request
            .windows(needle.len())
            .any(|part| part == needle.as_bytes())
    }

    #[test]
    fn local_asr_request_sends_wav_and_reads_transcript() {
        let (text, request) = transcribe_against_fake_server(AsrModel::Parakeet, "de");
        let headers_end = request
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .unwrap();
        let headers = String::from_utf8_lossy(&request[..headers_end]);
        assert!(headers.starts_with("POST /v1/audio/transcriptions HTTP/1.1"));
        assert!(headers.to_ascii_lowercase().contains("multipart/form-data"));
        assert!(request.windows(4).any(|part| part == b"RIFF"));
        assert!(request.windows(8).any(|part| part == b"parakeet"));
        assert!(has_field(&request, "language"));
        assert_eq!(text, "Pronto works on this Mac.");
    }

    #[test]
    fn phonon_request_omits_parakeet_model_and_language() {
        // Phonon's server 404s an unknown `model` and is English-only.
        let (text, request) = transcribe_against_fake_server(AsrModel::Phonon, "de");
        assert!(request.windows(4).any(|part| part == b"RIFF"));
        assert!(!has_field(&request, "model"));
        assert!(!has_field(&request, "language"));
        assert!(has_field(&request, "response_format"));
        assert_eq!(text, "Pronto works on this Mac.");
    }

    #[test]
    fn meeting_chunks_run_one_at_a_time() {
        assert_eq!(MEETING_WORKERS, 1);
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
    fn gpu_pressure_survives_bouncing_readings() {
        let now = Instant::now();
        let mut policy = GpuPressurePolicy::new(true, now, 1024 * MIB);
        policy.last_activity = now - MODEL_IDLE_BEFORE_UNLOAD - Duration::from_secs(1);
        policy.last_transition = now - MODEL_TRANSITION_COOLDOWN - Duration::from_secs(1);
        let low = MemoryInfo {
            total: 8 * 1024 * MIB,
            free: 600 * MIB,
            used: 7 * 1024 * MIB,
        };
        let spike = MemoryInfo {
            free: 2500 * MIB,
            used: 5 * 1024 * MIB,
            ..low
        };
        assert!(!policy.observe_loaded(low, now));
        assert!(!policy.observe_loaded(low, now));
        assert!(!policy.observe_loaded(spike, now));
        assert!(!policy.observe_loaded(low, now));
        assert!(policy.observe_loaded(low, now));
    }

    #[test]
    fn game_claiming_vram_counts_as_pressure_before_free_hits_reserve() {
        let now = Instant::now();
        let mut policy = GpuPressurePolicy::new(true, now, 1024 * MIB);
        // 12 GiB card: model loads using 2.5 GiB while the desktop uses 1.5 GiB.
        let before = MemoryInfo {
            total: 12 * 1024 * MIB,
            free: 10752 * MIB,
            used: 1536 * MIB,
        };
        let after = MemoryInfo {
            free: 8192 * MIB,
            used: 4096 * MIB,
            ..before
        };
        policy.note_loaded(Some(before), Some(after), now);
        assert_eq!(policy.model_bytes, 2560 * MIB);
        assert!(!policy.under_pressure(after));
        // A game takes 5.5 GiB: free stays above the 2 GiB reserve, but the
        // model now blocks a comfortable margin for the game.
        let gaming = MemoryInfo {
            free: 2560 * MIB,
            used: 9728 * MIB,
            ..before
        };
        assert!(policy.under_pressure(gaming));
        // Plenty of headroom left: growth alone is not pressure.
        let light_app = MemoryInfo {
            free: 6144 * MIB,
            used: 6144 * MIB,
            ..before
        };
        assert!(!policy.under_pressure(light_app));
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

    /// One-shot HTTP server that records the request and answers `body`.
    fn mock_llm_server(
        body: &'static str,
    ) -> (
        std::net::SocketAddr,
        mpsc::Receiver<Vec<u8>>,
        std::thread::JoinHandle<()>,
    ) {
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
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        (address, request_rx, server)
    }

    fn split_request(request: &[u8]) -> (String, serde_json::Value) {
        let header_end = request
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .unwrap()
            + 4;
        (
            String::from_utf8_lossy(&request[..header_end]).into_owned(),
            serde_json::from_slice(&request[header_end..]).unwrap(),
        )
    }

    fn mock_target(
        provider: crate::cleanup_provider::CleanupProvider,
        address: std::net::SocketAddr,
        path: &str,
    ) -> CleanupTarget {
        CleanupTarget {
            provider,
            endpoint: format!("http://{address}{path}"),
            model: provider.default_model().unwrap().into(),
            api_key: Some("test-secret".into()),
        }
    }

    #[test]
    fn deepseek_cleanup_uses_fast_model_dictionary_and_parses_response() {
        let (address, request_rx, server) =
            mock_llm_server(r#"{"choices":[{"message":{"content":"Use Pronto with Parakeet."}}]}"#);
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let target = mock_target(
            crate::cleanup_provider::CleanupProvider::DeepSeek,
            address,
            "/chat/completions",
        );
        let cleaned = ai_cleanup(
            &client,
            &target,
            "use pronto with parakeet",
            &["Pronto".into(), "Parakeet".into()],
            DEFAULT_CLEANUP_PROMPT,
        )
        .expect("mock cleanup should succeed");
        assert_eq!(cleaned, "Use Pronto with Parakeet.");

        let request = request_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        server.join().unwrap();
        let (headers, payload) = split_request(&request);
        assert!(headers
            .lines()
            .any(|line| line.eq_ignore_ascii_case("authorization: Bearer test-secret")));
        assert_eq!(payload["model"], "deepseek-v4-flash");
        assert_eq!(payload["thinking"]["type"], "disabled");
        assert_eq!(payload["stream"], false);
        assert_eq!(payload["temperature"], 0);
        assert_eq!(payload["max_tokens"], 768);
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
    fn openai_cleanup_uses_completion_token_budget() {
        let (address, request_rx, server) =
            mock_llm_server(r#"{"choices":[{"message":{"content":"Hello there."}}]}"#);
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let target = mock_target(
            crate::cleanup_provider::CleanupProvider::OpenAi,
            address,
            "/v1/chat/completions",
        );
        let cleaned = ai_cleanup(&client, &target, "hello there", &[], DEFAULT_CLEANUP_PROMPT)
            .expect("mock cleanup should succeed");
        assert_eq!(cleaned, "Hello there.");
        let request = request_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        server.join().unwrap();
        let (_, payload) = split_request(&request);
        assert_eq!(payload["max_completion_tokens"], 768);
        assert!(payload.get("max_tokens").is_none());
        assert!(payload.get("thinking").is_none());
    }

    #[test]
    fn anthropic_cleanup_uses_messages_api_and_joins_text_blocks() {
        let (address, request_rx, server) = mock_llm_server(
            r#"{"content":[{"type":"thinking","thinking":""},{"type":"text","text":"Use Pronto "},{"type":"text","text":"with Parakeet."}],"stop_reason":"end_turn"}"#,
        );
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let target = mock_target(
            crate::cleanup_provider::CleanupProvider::Anthropic,
            address,
            "/v1/messages",
        );
        let cleaned = ai_cleanup(
            &client,
            &target,
            "use pronto with parakeet",
            &["Pronto".into()],
            DEFAULT_CLEANUP_PROMPT,
        )
        .expect("mock cleanup should succeed");
        assert_eq!(cleaned, "Use Pronto with Parakeet.");
        let request = request_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        server.join().unwrap();
        let (headers, payload) = split_request(&request);
        assert!(headers
            .lines()
            .any(|line| line.eq_ignore_ascii_case("x-api-key: test-secret")));
        assert!(headers
            .lines()
            .any(|line| line.eq_ignore_ascii_case("anthropic-version: 2023-06-01")));
        assert_eq!(payload["model"], "claude-opus-5-5");
        assert_eq!(payload["output_config"]["effort"], "low");
        assert!(payload["system"]
            .as_str()
            .unwrap()
            .contains("Never use em dashes"));
        assert_eq!(payload["messages"][0]["role"], "user");
        assert!(payload.get("temperature").is_none());
    }

    #[test]
    #[ignore = "requires a DeepSeek key saved by Pronto or DEEPSEEK_API_KEY"]
    fn live_deepseek_cleanup_roundtrip() {
        let target = crate::cleanup_provider::resolve(&UserSettings::default())
            .expect("save a DeepSeek key in Pronto Settings first");
        let client = Client::builder()
            .timeout(Duration::from_secs(12))
            .build()
            .unwrap();
        let started = Instant::now();
        let cleaned = ai_cleanup(
            &client,
            &target,
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
    fn warm_up_clip_survives_silence_trimming() {
        let wav = recording_to_wav(&warm_up_clip()).unwrap();
        assert!(wav.len() > 44 + 16_000 * 2);
    }

    fn bench_env<T: std::str::FromStr>(name: &str, default: T) -> T {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    }

    fn bench_backend() -> AsrModel {
        match std::env::var("PRONTO_BENCH_BACKEND").as_deref() {
            Ok("parakeet") => AsrModel::Parakeet,
            Ok("phonon") | Err(_) => AsrModel::Phonon,
            Ok(other) => panic!("PRONTO_BENCH_BACKEND must be phonon or parakeet, not {other}"),
        }
    }

    #[cfg(windows)]
    fn server_cpu_seconds(server: &SpeechServer) -> f64 {
        use windows::Win32::Foundation::{FILETIME, HANDLE};
        use windows::Win32::System::Threading::GetProcessTimes;
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        unsafe {
            GetProcessTimes(
                HANDLE(server.child.as_raw_handle()),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
            .unwrap();
        }
        let ticks = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
        (ticks(kernel) + ticks(user)) as f64 / 10_000_000.0
    }

    #[cfg(not(windows))]
    fn server_cpu_seconds(_server: &SpeechServer) -> f64 {
        0.0
    }

    /// The production `process_job` path (audio preparation, multipart HTTP,
    /// server transcription, local cleanup, formatting) against a server
    /// started by `SpeechServer::start`, with the engine's own HTTP client.
    /// PRONTO_BENCH_BACKEND=phonon|parakeet; PRONTO_BENCH_CLEANUP=1 adds the
    /// DeepSeek call with the user's key; PRONTO_BENCH_GAP_MS idles before
    /// each request. Insertion is excluded (see insert.rs benchmark).
    #[test]
    #[ignore = "benchmark: requires the backend's packs and PRONTO_BENCH_MANIFEST; run in release mode"]
    fn benchmark_warm_pipeline() {
        let manifest_path = PathBuf::from(std::env::var("PRONTO_BENCH_MANIFEST").unwrap());
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        let clips: Vec<_> = manifest["clips"]
            .as_array()
            .unwrap()
            .iter()
            .map(|clip| {
                let path = manifest_path
                    .parent()
                    .unwrap()
                    .join(clip["path"].as_str().unwrap());
                let recording = recording_from_pcm16_wav(&fs::read(path).unwrap()).unwrap();
                (clip["id"].as_str().unwrap().to_string(), recording)
            })
            .collect();
        let backend = bench_backend();
        let cleanup = bench_env("PRONTO_BENCH_CLEANUP", 0u8) == 1;
        let gap = Duration::from_millis(bench_env("PRONTO_BENCH_GAP_MS", 0u64));
        let repeats: usize = bench_env("PRONTO_BENCH_REPEATS", 10);
        let warmups: usize = bench_env("PRONTO_BENCH_WARMUPS", 2);
        if cleanup {
            assert!(
                deepseek_key().is_some(),
                "PRONTO_BENCH_CLEANUP=1 needs a DeepSeek key"
            );
        }
        set_desired_backend(backend);
        let runtime = locate_runtime(None, backend).unwrap();
        let start = Instant::now();
        let mut server = SpeechServer::start(&runtime).unwrap();
        let load_s = start.elapsed().as_secs_f64();
        let client = engine_client();
        let health: serde_json::Value = client
            .get(format!("{}/health", server.base_url))
            .send()
            .unwrap()
            .json()
            .unwrap();
        let settings = UserSettings {
            cleanup_enabled: cleanup,
            auto_insert: false,
            ..UserSettings::default()
        };
        let logical_cpus = std::thread::available_parallelism().unwrap().get() as f64;
        let mut rows = Vec::new();
        for repeat in 0..repeats + warmups {
            for offset in 0..clips.len() {
                let (id, recording) = &clips[(offset + repeat) % clips.len()];
                // Separate preparation probe. The full pipeline below still performs
                // its own preparation and includes that work in latency_s.
                let prep = Instant::now();
                let wav = recording_to_wav(recording).unwrap();
                let preparation_s = prep.elapsed().as_secs_f64();
                let input = Recording {
                    samples: recording.samples.clone(),
                    sample_rate: recording.sample_rate,
                    channels: recording.channels,
                };
                let job = TranscriptionJob::file_import(input, settings.clone(), false, None);
                std::thread::sleep(gap);
                let cpu_start = server_cpu_seconds(&server);
                let started = Instant::now();
                let result = process_job(&client, &mut server, job, started).unwrap();
                let latency_s = started.elapsed().as_secs_f64();
                let cpu_s = server_cpu_seconds(&server) - cpu_start;
                if repeat >= warmups {
                    rows.push(json!({"clip": id, "repeat": repeat - warmups, "latency_s": latency_s,
                        "server_cpu_s": cpu_s,
                        "server_cpu_percent": 100.0 * cpu_s / latency_s / logical_cpus,
                        "preparation_probe_s": preparation_s, "submitted_audio_s": (wav.len()-44) as f64/32000.0,
                        "asr_ms": result.entry.asr_ms, "cleanup_ms": result.entry.cleanup_ms,
                        "cleanup_applied": result.entry.cleanup_applied,
                        "cleanup_warning": result.cleanup_warning,
                        "raw_text": result.entry.raw_text, "entry": result.entry}));
                }
            }
        }
        let stop = Instant::now();
        server.stop();
        let stop_s = stop.elapsed().as_secs_f64();
        let report = json!({"backend": backend.short_name(), "cleanup": cleanup,
            "gap_ms": gap.as_millis() as u64, 
            "load_including_startup_warmup_s": load_s,
            "stop_s": stop_s, "health": health, "manifest": manifest, "rows": rows});
        fs::write(
            std::env::var("PRONTO_BENCH_OUTPUT").unwrap(),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        println!(
            "{} benchmark complete; load {load_s:.3}s, stop {stop_s:.3}s",
            backend.short_name()
        );
    }

    /// The production meeting chunk loop (`transcribe_meeting_chunks`, the
    /// engine's client) against a server from `SpeechServer::start`, for each
    /// worker count in PRONTO_BENCH_WORKERS (default "1,3"). A failed chunk is
    /// recorded, not fatal, because the app fails the whole meeting on one.
    #[test]
    #[ignore = "benchmark: requires the backend's packs, PRONTO_BENCH_MEETING_WAV and PRONTO_BENCH_OUTPUT"]
    fn benchmark_meeting_chunks() {
        let wav = PathBuf::from(std::env::var("PRONTO_BENCH_MEETING_WAV").unwrap());
        let backend = bench_backend();
        let repeats: usize = bench_env("PRONTO_BENCH_REPEATS", 3);
        let workers: Vec<usize> = std::env::var("PRONTO_BENCH_WORKERS")
            .unwrap_or_else(|_| "1,3".into())
            .split(',')
            .map(|value| value.trim().parse().unwrap())
            .collect();
        set_desired_backend(backend);
        let mut server = SpeechServer::start(&locate_runtime(None, backend).unwrap()).unwrap();
        let client = engine_client();
        let mut rows = Vec::new();
        for repeat in 0..repeats {
            for &count in &workers {
                std::thread::sleep(Duration::from_secs(2));
                let started = Instant::now();
                let progress = Mutex::new(Vec::new());
                let result = transcribe_meeting_chunks(
                    &client,
                    &server.base_url,
                    backend,
                    &wav,
                    "auto",
                    count,
                    &|done, _| {
                        progress
                            .lock()
                            .unwrap()
                            .push((done, started.elapsed().as_secs_f64()))
                    },
                );
                let wall_s = started.elapsed().as_secs_f64();
                let row = match result {
                    Ok(parts) => json!({"repeat": repeat, "workers": count, "wall_s": wall_s,
                        "chunk_done_s": progress.into_inner().unwrap(), "parts": parts}),
                    Err(error) => json!({"repeat": repeat, "workers": count, "wall_s": wall_s,
                        "chunk_done_s": progress.into_inner().unwrap(), "error": error}),
                };
                println!(
                    "{}",
                    json!({"repeat": repeat, "workers": count, "wall_s": wall_s,
                    "error": row.get("error")})
                );
                rows.push(row);
            }
        }
        server.stop();
        fs::write(
            std::env::var("PRONTO_BENCH_OUTPUT").unwrap(),
            serde_json::to_vec_pretty(&json!({"backend": backend.short_name(),
                "wav": wav, "rows": rows}))
            .unwrap(),
        )
        .unwrap();
    }

    /// DeepSeek cleanup latency by connection state, with the engine's client
    /// and the user's key. Each case follows its own idle gap
    /// (PRONTO_BENCH_GAP_MS, default 180 s, so DNS and pooled connections
    /// expire as between real dictations): `cold` is a new client (DNS, TCP and
    /// TLS inside the call, today's behaviour) followed by `reused`, the same
    /// client at once; `preconnected` is `preconnect_cleanup`'s request, then
    /// PRONTO_BENCH_RECORD_MS (default 1500) of simulated recording, then the
    /// timed cleanup. Failures are recorded: the app falls back to local cleanup.
    #[test]
    #[ignore = "benchmark: calls DeepSeek with the user's key; PRONTO_BENCH_OUTPUT optional"]
    fn benchmark_deepseek_connection() {
        let key = deepseek_key().expect("a DeepSeek key is required");
        let transcript = "He hoped there would be stew for dinner, turnips and carrots and bruised potatoes and fat mutton pieces to be ladled out in thick peppered flour fattened sauce.";
        let iterations: usize = bench_env("PRONTO_BENCH_REPEATS", 6);
        let gap = Duration::from_millis(bench_env("PRONTO_BENCH_GAP_MS", 180_000u64));
        let record = Duration::from_millis(bench_env("PRONTO_BENCH_RECORD_MS", 1_500u64));
        let cleanup = |client: &Client| {
            let started = Instant::now();
            let result = deepseek_cleanup(client, &key, transcript, &[], DEFAULT_CLEANUP_PROMPT);
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            match result {
                Ok(text) => json!({"ms": ms, "text": text}),
                Err(error) => json!({"ms": ms, "error": error}),
            }
        };
        let mut rows = Vec::new();
        for iteration in 0..iterations {
            std::thread::sleep(gap);
            let client = engine_client();
            let cold = cleanup(&client);
            let reused = cleanup(&client);
            std::thread::sleep(gap);
            let client = engine_client();
            let connect = Instant::now();
            let status = client
                .get(DEEPSEEK_MODELS_URL)
                .bearer_auth(&key)
                .send()
                .map(|response| response.status().as_u16())
                .map_err(|error| error.to_string());
            let preconnect_ms = connect.elapsed().as_secs_f64() * 1000.0;
            std::thread::sleep(record.saturating_sub(connect.elapsed()));
            let preconnected = cleanup(&client);
            let row = json!({"iteration": iteration, "cold": cold, "reused": reused,
                "preconnect_ms": preconnect_ms, "preconnect_status": format!("{status:?}"),
                "preconnected": preconnected});
            println!("{row}");
            rows.push(row);
        }
        if let Ok(output) = std::env::var("PRONTO_BENCH_OUTPUT") {
            fs::write(
                output,
                serde_json::to_vec_pretty(&json!({"gap_ms": gap.as_millis() as u64,
                "record_ms": record.as_millis() as u64, "rows": rows}))
                .unwrap(),
            )
            .unwrap();
        }
    }

    fn tone_and_silence(parts: &[(f32, bool)]) -> Vec<f32> {
        parts
            .iter()
            .flat_map(|&(seconds, speech)| {
                (0..(seconds * 16_000.0) as usize).map(move |index| {
                    if speech {
                        (index as f32 * 220.0 * std::f32::consts::TAU / 16_000.0).sin() * 0.1
                    } else {
                        0.0
                    }
                })
            })
            .collect()
    }

    #[test]
    fn segment_cut_lands_mid_pause_after_enough_speech() {
        let audio = tone_and_silence(&[(4.0, true), (1.0, false), (2.0, true)]);
        let cut = find_segment_cut(&audio).unwrap();
        assert_eq!(
            cut,
            64_000 + 6_400,
            "4 s of speech, then half the 0.8 s pause"
        );
        // The cut falls in silence that trimming drops, on a frame boundary.
        assert!(audio[cut..cut + 320].iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn short_pauses_and_short_openings_are_not_cut() {
        assert_eq!(
            find_segment_cut(&tone_and_silence(&[(4.0, true), (0.6, false), (2.0, true)])),
            None
        );
        assert_eq!(
            find_segment_cut(&tone_and_silence(&[(2.0, true), (1.0, false), (2.0, true)])),
            None
        );
        assert_eq!(find_segment_cut(&tone_and_silence(&[(5.0, false)])), None);
    }

    /// Transcribing finished segments while recording versus the whole
    /// recording at stop, on the production path. Each recording is replayed
    /// in real time after PRONTO_BENCH_GAP_MS idle (default 12 s, so the GPU
    /// is in P8 as before a real dictation), polling every `SEGMENT_POLL` with
    /// the real `Segmenter`; only `process_job` is timed, which is what the
    /// user waits for after stopping. Modes alternate per repeat. Writes
    /// `<PRONTO_BENCH_OUTPUT>-full.json` and `-segmented.json` for compare.py.
    #[test]
    #[ignore = "benchmark: requires Parakeet, PRONTO_BENCH_MANIFEST and PRONTO_BENCH_OUTPUT"]
    fn benchmark_live_segments() {
        let manifest_path = PathBuf::from(std::env::var("PRONTO_BENCH_MANIFEST").unwrap());
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        let clips: Vec<_> = manifest["clips"]
            .as_array()
            .unwrap()
            .iter()
            .map(|clip| {
                let path = manifest_path
                    .parent()
                    .unwrap()
                    .join(clip["path"].as_str().unwrap());
                let recording = recording_from_pcm16_wav(&fs::read(path).unwrap()).unwrap();
                (clip["id"].as_str().unwrap().to_string(), recording)
            })
            .collect();
        let repeats: usize = bench_env("PRONTO_BENCH_REPEATS", 3);
        let gap = Duration::from_millis(bench_env("PRONTO_BENCH_GAP_MS", 12_000u64));
        set_desired_backend(AsrModel::Parakeet);
        let mut server =
            SpeechServer::start(&locate_runtime(None, AsrModel::Parakeet).unwrap()).unwrap();
        let client = engine_client();
        let settings = UserSettings {
            cleanup_enabled: false,
            auto_insert: false,
            ..UserSettings::default()
        };
        let logical_cpus = std::thread::available_parallelism().unwrap().get() as f64;
        for (_, recording) in &clips {
            transcribe_recording(&client, &server, recording, "auto").unwrap();
        }
        let mut rows = [Vec::new(), Vec::new()];
        for repeat in 0..repeats {
            for (id, recording) in &clips {
                let modes = if repeat % 2 == 0 {
                    [false, true]
                } else {
                    [true, false]
                };
                for segmented in modes {
                    std::thread::sleep(gap);
                    let channels = recording.channels as usize;
                    let frames = recording.samples.len() / channels;
                    let mut segmenter = Segmenter::new();
                    let recording_started = Instant::now();
                    loop {
                        std::thread::sleep(SEGMENT_POLL);
                        let captured = ((recording_started.elapsed().as_secs_f64()
                            * recording.sample_rate as f64)
                            as usize)
                            .min(frames);
                        if segmented {
                            let from = segmenter.transcript.covered_frames * channels;
                            let pending = Recording {
                                samples: recording.samples[from..captured * channels].to_vec(),
                                sample_rate: recording.sample_rate,
                                channels: recording.channels,
                            };
                            segmenter.step(
                                &pending,
                                &client,
                                &server.base_url,
                                AsrModel::Parakeet,
                                "auto",
                            );
                        }
                        if captured == frames {
                            break;
                        }
                    }
                    let segments = segmenter.transcript.texts.len();
                    let covered_s =
                        segmenter.transcript.covered_frames as f64 / recording.sample_rate as f64;
                    let failed = segmenter.transcript.failed;
                    let job = TranscriptionJob::live(
                        Recording {
                            samples: recording.samples.clone(),
                            sample_rate: recording.sample_rate,
                            channels: recording.channels,
                        },
                        settings.clone(),
                        0,
                        segmented.then_some(segmenter.transcript),
                    );
                    let cpu_start = server_cpu_seconds(&server);
                    let started = Instant::now();
                    let result = process_job(&client, &mut server, job, started).unwrap();
                    let latency_s = started.elapsed().as_secs_f64();
                    let cpu_s = server_cpu_seconds(&server) - cpu_start;
                    println!(
                        "{id} segmented={segmented} segments={segments} covered={covered_s:.1}s latency={:.1}ms",
                        latency_s * 1000.0
                    );
                    rows[segmented as usize].push(json!({"clip": id, "repeat": repeat,
                        "latency_s": latency_s, "server_cpu_percent": 100.0 * cpu_s / latency_s / logical_cpus,
                        "segments_before_stop": segments, "covered_s": covered_s, "segment_failed": failed,
                        "asr_ms": result.entry.asr_ms, "raw_text": result.entry.raw_text,
                        "entry": result.entry}));
                }
            }
        }
        server.stop();
        let output = std::env::var("PRONTO_BENCH_OUTPUT").unwrap();
        for (mode, rows) in [("full", &rows[0]), ("segmented", &rows[1])] {
            fs::write(
                format!("{output}-{mode}.json"),
                serde_json::to_vec_pretty(&json!({"label": mode, "backend": "Parakeet",
                    "gap_ms": gap.as_millis() as u64, "manifest": manifest, "rows": rows}))
                .unwrap(),
            )
            .unwrap();
        }
    }

    /// Load, unload and switch costs on the production path: `SpeechServer::start`
    /// (its 200 ms health polling and Phonon's warm-up decode included) and `stop`.
    /// A GPU-pressure release is one Parakeet stop; the reload is one start plus
    /// the first request. A switch is the previous backend's stop plus the next
    /// backend's start, as `adopt_backend` + `warm_server` do.
    #[test]
    #[ignore = "benchmark: requires the Parakeet and Phonon packs, PRONTO_BENCH_MANIFEST and PRONTO_BENCH_OUTPUT"]
    fn benchmark_model_lifecycle() {
        let manifest_path = PathBuf::from(std::env::var("PRONTO_BENCH_MANIFEST").unwrap());
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        let clip = manifest["clips"]
            .as_array()
            .unwrap()
            .iter()
            .min_by(|a, b| {
                a["duration_s"]
                    .as_f64()
                    .unwrap()
                    .total_cmp(&b["duration_s"].as_f64().unwrap())
            })
            .unwrap()
            .clone();
        let recording = recording_from_pcm16_wav(
            &fs::read(
                manifest_path
                    .parent()
                    .unwrap()
                    .join(clip["path"].as_str().unwrap()),
            )
            .unwrap(),
        )
        .unwrap();
        let cycles: usize = bench_env("PRONTO_BENCH_CYCLES", 5);
        let client = engine_client();
        let request = |server: &SpeechServer| {
            let started = Instant::now();
            transcribe_recording(&client, server, &recording, "auto").unwrap();
            started.elapsed().as_secs_f64()
        };
        let start = |model: AsrModel| {
            set_desired_backend(model);
            let runtime = locate_runtime(None, model).unwrap();
            let started = Instant::now();
            let server = SpeechServer::start(&runtime).unwrap();
            (server, started.elapsed().as_secs_f64())
        };
        let stop = |mut server: SpeechServer| {
            let started = Instant::now();
            server.stop();
            started.elapsed().as_secs_f64()
        };
        let mut reloads = Vec::new();
        for cycle in 0..cycles {
            std::thread::sleep(Duration::from_secs(2));
            let (server, load_s) = start(AsrModel::Parakeet);
            let first_s = request(&server);
            let second_s = request(&server);
            let stop_s = stop(server);
            reloads.push(
                json!({"cycle": cycle, "load_s": load_s, "first_request_s": first_s,
                "second_request_s": second_s, "stop_s": stop_s}),
            );
            println!("{}", reloads.last().unwrap());
        }
        let mut switches = Vec::new();
        let (mut server, _) = start(AsrModel::Parakeet);
        request(&server);
        for cycle in 0..cycles.min(3) {
            for next in [AsrModel::Phonon, AsrModel::Parakeet] {
                let from = server.backend;
                let stop_s = stop(server);
                let (started, load_s) = start(next);
                server = started;
                let first_s = request(&server);
                switches.push(
                    json!({"cycle": cycle, "from": from.short_name(), "to": next.short_name(),
                    "stop_previous_s": stop_s, "load_s": load_s, "first_request_s": first_s,
                    "ready_s": stop_s + load_s}),
                );
                println!("{}", switches.last().unwrap());
            }
        }
        stop(server);
        let report = json!({"clip": clip, "reloads": reloads, "switches": switches});
        fs::write(
            std::env::var("PRONTO_BENCH_OUTPUT").unwrap(),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }

    #[test]
    #[ignore = "requires the Phonon pack and PRONTO_TEST_WAV; optional PRONTO_TEST_TRANSCRIPT"]
    fn end_to_end_phonon_cpu_transcription() {
        let wav_path = std::env::var("PRONTO_TEST_WAV").expect("PRONTO_TEST_WAV is required");
        let recording = recording_from_pcm16_wav(&std::fs::read(wav_path).unwrap()).unwrap();
        set_desired_backend(AsrModel::Phonon);
        let runtime = locate_runtime(None, AsrModel::Phonon).unwrap();
        let started = Instant::now();
        let mut server = SpeechServer::start(&runtime).unwrap();
        let load = started.elapsed();
        let client = Client::builder().build().unwrap();
        let settings = UserSettings {
            cleanup_enabled: false,
            auto_insert: false,
            ..UserSettings::default()
        };
        let completed = process_job(
            &client,
            &mut server,
            TranscriptionJob::file_import(recording, settings, false, None),
            Instant::now(),
        )
        .unwrap();
        println!(
            "load {:.1}s, first transcription {} ms for {} ms of audio: {}",
            load.as_secs_f32(),
            completed.entry.asr_ms,
            completed.entry.audio_ms,
            completed.entry.raw_text
        );
        // Proper names may vary (the dictionary exists for them), so require
        // nearly every expected word rather than an exact string.
        if let Ok(expected) = std::env::var("PRONTO_TEST_TRANSCRIPT") {
            let words = |text: &str| -> Vec<String> {
                text.split_whitespace()
                    .map(|word| {
                        word.trim_matches(|c: char| !c.is_alphanumeric())
                            .to_lowercase()
                    })
                    .collect()
            };
            let heard = words(&completed.entry.raw_text);
            let expected = words(&expected);
            let matched = expected.iter().filter(|word| heard.contains(word)).count();
            assert!(
                matched * 100 >= expected.len() * 95,
                "only {matched}/{} expected words were transcribed",
                expected.len()
            );
        }
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
            .as_chunks::<2>()
            .0
            .iter()
            .map(|sample| i16::from_le_bytes([sample[0], sample[1]]) as f32 / i16::MAX as f32)
            .collect();
        let recording = Recording {
            samples,
            sample_rate,
            channels,
        };
        let runtime = locate_runtime(None, AsrModel::Parakeet).unwrap();
        let mut server = SpeechServer::start(&runtime).unwrap();
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let settings = UserSettings {
            cleanup_enabled: false,
            auto_insert: false,
            ..UserSettings::default()
        };
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
