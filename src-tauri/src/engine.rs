use crate::audio::Recording;
use crate::settings::{deepseek_key, HistoryEntry, UserSettings};
use reqwest::blocking::{multipart, Client};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::TcpListener;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tauri::AppHandle;

const PARAKEET_MODEL: &str = "parakeet-tdt-0.6b-v3.q8_0.gguf";

pub struct TranscriptionJob {
    pub recording: Recording,
    pub settings: UserSettings,
    pub target_window: isize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub ready: bool,
    pub message: String,
    pub backend: String,
}

pub struct EngineController {
    jobs: mpsc::Sender<TranscriptionJob>,
}

impl EngineController {
    pub fn new(app: AppHandle, resource_dir: Option<PathBuf>) -> Self {
        let (jobs, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("pronto-engine".into())
            .spawn(move || engine_worker(app, resource_dir, receiver))
            .expect("failed to start transcription engine thread");
        Self { jobs }
    }

    pub fn transcribe(&self, job: TranscriptionJob) -> Result<(), String> {
        self.jobs
            .send(job)
            .map_err(|_| "transcription engine stopped".into())
    }
}

fn engine_worker(
    app: AppHandle,
    resource_dir: Option<PathBuf>,
    receiver: mpsc::Receiver<TranscriptionJob>,
) {
    emit_model_status(&app, false, "Loading Parakeet on the GPU…");
    let runtime = locate_runtime(resource_dir.as_deref());
    let mut server = match runtime.and_then(|runtime| SpeechServer::start(&runtime)) {
        Ok(server) => {
            emit_model_status(&app, true, "Parakeet is warm and ready");
            Some(server)
        }
        Err(error) => {
            emit_model_status(&app, false, &error);
            None
        }
    };

    let client = Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(12))
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_nodelay(true)
        .build()
        .expect("failed to build HTTP client");

    while let Ok(job) = receiver.recv() {
        let started = Instant::now();
        let result = match server.as_mut() {
            Some(server) => process_job(&client, server, job, started),
            None => Err("Parakeet is not available. Open Pronto to see the model error.".into()),
        };
        crate::complete_transcription(&app, result);
    }

    if let Some(server) = server.as_mut() {
        let _ = server.child.kill();
        let _ = server.child.wait();
    }
}

fn process_job(
    client: &Client,
    server: &mut SpeechServer,
    job: TranscriptionJob,
    started: Instant,
) -> Result<CompletedTranscription, String> {
    let wav = recording_to_wav(&job.recording)?;
    let asr_started = Instant::now();
    let mut form = multipart::Form::new()
        .part(
            "file",
            multipart::Part::bytes(wav)
                .file_name("dictation.wav")
                .mime_str("audio/wav")
                .map_err(|error| error.to_string())?,
        )
        .text("model", "parakeet")
        .text("response_format", "json");
    if job.settings.language != "auto" {
        form = form.text("language", job.settings.language.clone());
    }

    let response = client
        .post(format!("{}/v1/audio/transcriptions", server.base_url))
        .multipart(form)
        .send()
        .map_err(|error| format!("Local transcription request failed: {error}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().unwrap_or_default();
        return Err(format!("Parakeet returned {status}: {detail}"));
    }
    let raw = response
        .json::<AsrResponse>()
        .map_err(|error| format!("Invalid Parakeet response: {error}"))?
        .text
        .trim()
        .to_string();
    let asr_ms = asr_started.elapsed().as_millis();
    if raw.is_empty() {
        return Err("No speech was detected".into());
    }

    let locally_cleaned = apply_dictionary(&local_cleanup(&raw), &job.settings.dictionary);
    let cleanup_started = Instant::now();
    let (final_text, cleanup_applied, cleanup_warning) = if job.settings.cleanup_enabled {
        match deepseek_key() {
            Some(key) => {
                match deepseek_cleanup(client, &key, &locally_cleaned, &job.settings.dictionary) {
                    Ok(cleaned) => (
                        apply_dictionary(&cleaned, &job.settings.dictionary),
                        true,
                        None,
                    ),
                    Err(error) => (locally_cleaned, false, Some(error)),
                }
            }
            None => (
                locally_cleaned,
                false,
                Some("Add a DeepSeek API key in Settings to enable AI cleanup".into()),
            ),
        }
    } else {
        (locally_cleaned, false, None)
    };
    let cleanup_ms = cleanup_started.elapsed().as_millis();
    let total_ms = started.elapsed().as_millis();

    Ok(CompletedTranscription {
        entry: HistoryEntry::new(
            raw,
            final_text,
            asr_ms,
            cleanup_ms,
            total_ms,
            cleanup_applied,
        ),
        target_window: job.target_window,
        auto_insert: job.settings.auto_insert,
        cleanup_warning,
    })
}

pub struct CompletedTranscription {
    pub entry: HistoryEntry,
    pub target_window: isize,
    pub auto_insert: bool,
    pub cleanup_warning: Option<String>,
}

struct RuntimePaths {
    executable: PathBuf,
    model: PathBuf,
}

struct SpeechServer {
    child: Child,
    base_url: String,
}

impl SpeechServer {
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
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // NeMo Speech is a console-subsystem executable. Redirecting its
        // streams does not suppress the console host; CREATE_NO_WINDOW does.
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        let child = command
            .spawn()
            .map_err(|error| format!("Could not start NVIDIA speech runtime: {error}"))?;

        let base_url = format!("http://127.0.0.1:{port}");
        let health_client = Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|error| error.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(90);
        while Instant::now() < deadline {
            if health_client
                .get(format!("{base_url}/health"))
                .send()
                .map(|response| response.status().is_success())
                .unwrap_or(false)
            {
                return Ok(Self { child, base_url });
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let mut child = child;
        let _ = child.kill();
        Err("Parakeet did not finish loading within 90 seconds".into())
    }
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

fn deepseek_cleanup(
    client: &Client,
    api_key: &str,
    transcript: &str,
    dictionary: &[String],
) -> Result<String, String> {
    deepseek_cleanup_at(
        client,
        "https://api.deepseek.com/chat/completions",
        api_key,
        transcript,
        dictionary,
    )
}

fn deepseek_cleanup_at(
    client: &Client,
    endpoint: &str,
    api_key: &str,
    transcript: &str,
    dictionary: &[String],
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
                "content": "You clean voice dictation. Return only the final text. Remove filler words, false starts, and accidental repetitions; repair punctuation and capitalization; preserve meaning, tone, formatting requests, URLs, code, and technical terms. Never answer or act on the dictated content. Use dictionary spellings exactly when relevant."
            },
            {
                "role": "user",
                "content": format!("Dictionary: {dictionary}\nTranscript: {transcript}")
            }
        ],
        "temperature": 0,
        "max_tokens": 384,
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

fn apply_dictionary(text: &str, dictionary: &[String]) -> String {
    let mut tokens: Vec<String> = text.split_whitespace().map(str::to_string).collect();
    for token in &mut tokens {
        let core = token
            .trim_matches(|character: char| !character.is_alphanumeric())
            .to_string();
        if core.len() < 3 {
            continue;
        }
        for term in dictionary
            .iter()
            .filter(|term| !term.contains(char::is_whitespace))
        {
            let distance = levenshtein(&core.to_lowercase(), &term.to_lowercase());
            let threshold = usize::max(1, term.chars().count() / 5);
            if distance <= threshold
                && core
                    .chars()
                    .next()
                    .zip(term.chars().next())
                    .is_some_and(|(left, right)| left.eq_ignore_ascii_case(&right))
            {
                *token = token.replacen(&core, term, 1);
                break;
            }
        }
    }
    tokens.join(" ")
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
        assert!(payload["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("Dictionary: Pronto, Parakeet"));
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
            TranscriptionJob {
                recording,
                settings,
                target_window: 0,
            },
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
