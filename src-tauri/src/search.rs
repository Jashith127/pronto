use crate::mcp;
use crate::settings::deepseek_key;
use crate::ui_schema::{
    fallback_document, parse_and_validate_ui, SearchUiDocument, SourceItem, UiNode,
};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

pub const DEFAULT_PROVIDER_URL: &str = "https://html.duckduckgo.com/html/";
const MAX_RESULTS: usize = 5;
const MAX_SNIPPET_CHARS: usize = 140;
const SEARCH_MAX_TOKENS: u32 = 700;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SearchPhase {
    #[default]
    Idle,
    Listening,
    Searching,
    Complete,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchStatus {
    pub phase: SearchPhase,
    pub message: String,
    pub query: Option<String>,
    pub elapsed_ms: u128,
}

impl Default for SearchStatus {
    fn default() -> Self {
        Self {
            phase: SearchPhase::Idle,
            message: "Press Win + Space to search by voice".into(),
            query: None,
            elapsed_ms: 0,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultPayload {
    pub query: String,
    pub ui: SearchUiDocument,
    pub sources: Vec<SearchHit>,
    pub warning: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Pluggable web search backend. Brave / Tavily / Exa keyed providers, a
/// future Playwright scraper, or an MCP `web_search` tool can replace the
/// DuckDuckGo HTML MVP without changing the LLM synthesis code.
pub trait SearchProvider: Send + Sync {
    fn search(&self, query: &str) -> Result<Vec<SearchHit>, String>;
}

pub struct DuckDuckGoProvider {
    pub endpoint: String,
    client: Client,
}

impl DuckDuckGoProvider {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            client: Client::builder()
                .user_agent("ProntoVoiceSearch/0.1 (+local; DuckDuckGo HTML)")
                .timeout(std::time::Duration::from_secs(8))
                .build()
                .expect("reqwest client"),
        }
    }
}

impl SearchProvider for DuckDuckGoProvider {
    fn search(&self, query: &str) -> Result<Vec<SearchHit>, String> {
        let encoded = urlencoding_lite(query);
        // Prefer a single GET round-trip; fall back to the classic HTML POST form.
        let get_url = if self.endpoint.contains('?') {
            format!("{}&q={encoded}", self.endpoint.trim_end_matches('&'))
        } else {
            let base = self.endpoint.trim_end_matches('/');
            format!("{base}/?q={encoded}")
        };
        let response = self
            .client
            .get(&get_url)
            .send()
            .or_else(|_| {
                self.client
                    .post(&self.endpoint)
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(format!("q={encoded}"))
                    .send()
            })
            .map_err(|error| format!("DuckDuckGo request failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!("DuckDuckGo returned {}", response.status()));
        }
        let html = response
            .text()
            .map_err(|error| format!("DuckDuckGo body unreadable: {error}"))?;
        Ok(parse_duckduckgo_html(&html))
    }
}

pub fn parse_duckduckgo_html(html: &str) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    for block in html.split("result__a") {
        if hits.len() >= MAX_RESULTS {
            break;
        }
        let Some(href_start) = block.find("href=\"") else {
            continue;
        };
        let href_rest = &block[href_start + 6..];
        let Some(href_end) = href_rest.find('"') else {
            continue;
        };
        let mut url = decode_html_entities(&href_rest[..href_end]);
        url = unwrap_ddg_redirect(&url);
        if url.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
            continue;
        }
        let title = extract_between(block, '>', "</a>")
            .map(|value| strip_tags(&decode_html_entities(value)))
            .unwrap_or_default();
        if title.trim().is_empty() {
            continue;
        }
        let snippet = block
            .find("result__snippet")
            .and_then(|idx| extract_between(&block[idx..], '>', "</"))
            .map(|value| strip_tags(&decode_html_entities(value)).trim().to_string())
            .unwrap_or_default();
        if hits.iter().any(|hit: &SearchHit| hit.url == url) {
            continue;
        }
        hits.push(SearchHit {
            title: title.trim().to_string(),
            url,
            snippet,
        });
    }
    hits
}

fn unwrap_ddg_redirect(url: &str) -> String {
    // html.duckduckgo.com wraps outbound links as /l/?uddg=<urlencoded>
    if let Some(idx) = url.find("uddg=") {
        let encoded = &url[idx + 5..];
        let encoded = encoded.split('&').next().unwrap_or(encoded);
        return percent_decode(encoded);
    }
    url.to_string()
}

fn extract_between<'a>(haystack: &'a str, start: char, end: &str) -> Option<&'a str> {
    let begin = haystack.find(start)? + start.len_utf8();
    let rest = &haystack[begin..];
    let finish = rest.find(end)?;
    Some(&rest[..finish])
}

fn strip_tags(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut in_tag = false;
    for character in value.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    output
}

fn decode_html_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

fn urlencoding_lite(value: &str) -> String {
    let mut output = String::with_capacity(value.len() * 3);
    for byte in value.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                output.push(*byte as char)
            }
            b' ' => output.push('+'),
            _ => output.push_str(&format!("%{byte:02X}")),
        }
    }
    output
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Ok(high), Ok(low)) = (
                u8::from_str_radix(std::str::from_utf8(&bytes[index + 1..index + 2]).unwrap_or(""), 16),
                u8::from_str_radix(std::str::from_utf8(&bytes[index + 2..index + 3]).unwrap_or(""), 16),
            ) {
                output.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        if bytes[index] == b'+' {
            output.push(b' ');
        } else {
            output.push(bytes[index]);
        }
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

pub struct SearchController {
    status: Mutex<SearchStatus>,
    started_at: Mutex<Option<Instant>>,
    listen_generation: Mutex<u64>,
    last_allowed_urls: Mutex<HashSet<String>>,
    resource_dir: Mutex<Option<PathBuf>>,
}

impl Default for SearchController {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchController {
    pub fn new() -> Self {
        Self {
            status: Mutex::new(SearchStatus::default()),
            started_at: Mutex::new(None),
            listen_generation: Mutex::new(0),
            last_allowed_urls: Mutex::new(HashSet::new()),
            resource_dir: Mutex::new(None),
        }
    }

    pub fn set_resource_dir(&self, dir: Option<PathBuf>) {
        if let Ok(mut guard) = self.resource_dir.lock() {
            *guard = dir;
        }
    }

    pub fn status(&self) -> Result<SearchStatus, String> {
        self.status
            .lock()
            .map(|status| status.clone())
            .map_err(|_| "search status lock poisoned".into())
    }

    pub fn listen_elapsed_ms(&self) -> u128 {
        self.started_at
            .lock()
            .ok()
            .and_then(|guard| guard.map(|time| time.elapsed().as_millis()))
            .unwrap_or(0)
    }

    pub fn current_listen_generation(&self) -> u64 {
        self.listen_generation
            .lock()
            .map(|value| *value)
            .unwrap_or(0)
    }

    pub fn is_listening_generation(&self, generation: u64) -> bool {
        self.status
            .lock()
            .map(|status| status.phase == SearchPhase::Listening)
            .unwrap_or(false)
            && self.current_listen_generation() == generation
    }

    pub fn begin_listening(&self, hold_mode: bool) -> Result<(SearchStatus, u64), String> {
        let mut status = self.status.lock().map_err(|_| "search status lock poisoned")?;
        if !matches!(
            status.phase,
            SearchPhase::Idle | SearchPhase::Complete | SearchPhase::Error
        ) {
            return Ok((status.clone(), self.current_listen_generation()));
        }
        *self
            .started_at
            .lock()
            .map_err(|_| "search timer lock poisoned")? = Some(Instant::now());
        let generation = {
            let mut generation = self
                .listen_generation
                .lock()
                .map_err(|_| "search generation lock poisoned")?;
            *generation = generation.wrapping_add(1);
            *generation
        };
        *status = SearchStatus {
            phase: SearchPhase::Listening,
            message: if hold_mode {
                "Listening… release to search".into()
            } else {
                "Listening… press your shortcut again to search".into()
            },
            query: None,
            elapsed_ms: 0,
        };
        Ok((status.clone(), generation))
    }

    pub fn mark_transcribing(&self) -> Result<SearchStatus, String> {
        let mut status = self.status.lock().map_err(|_| "search status lock poisoned")?;
        if status.phase != SearchPhase::Listening {
            return Ok(status.clone());
        }
        let elapsed = self.listen_elapsed_ms();
        *status = SearchStatus {
            phase: SearchPhase::Searching,
            message: "Transcribing your question…".into(),
            query: None,
            elapsed_ms: elapsed,
        };
        Ok(status.clone())
    }

    pub fn mark_searching(&self, query_hint: Option<String>) -> Result<SearchStatus, String> {
        let mut status = self.status.lock().map_err(|_| "search status lock poisoned")?;
        if !matches!(status.phase, SearchPhase::Listening | SearchPhase::Searching) {
            return Ok(status.clone());
        }
        let elapsed = self.listen_elapsed_ms();
        *status = SearchStatus {
            phase: SearchPhase::Searching,
            message: if query_hint.as_ref().is_some_and(|query| !query.is_empty()) {
                "Searching the web…".into()
            } else {
                "Searching…".into()
            },
            query: query_hint,
            elapsed_ms: elapsed,
        };
        Ok(status.clone())
    }

    pub fn mark_synthesizing(&self, query: String) -> Result<SearchStatus, String> {
        let mut status = self.status.lock().map_err(|_| "search status lock poisoned")?;
        if status.phase != SearchPhase::Searching {
            return Ok(status.clone());
        }
        *status = SearchStatus {
            phase: SearchPhase::Searching,
            message: "Writing grounded answer…".into(),
            query: Some(query),
            elapsed_ms: self.listen_elapsed_ms(),
        };
        Ok(status.clone())
    }

    pub fn complete(&self, query: String, warning: Option<String>) -> Result<SearchStatus, String> {
        let elapsed = self
            .started_at
            .lock()
            .ok()
            .and_then(|mut guard| {
                let elapsed = guard.map(|time| time.elapsed().as_millis());
                *guard = None;
                elapsed
            })
            .unwrap_or(0);
        let mut status = self.status.lock().map_err(|_| "search status lock poisoned")?;
        *status = SearchStatus {
            phase: SearchPhase::Complete,
            message: warning.unwrap_or_else(|| format!("Answer ready in {elapsed} ms")),
            query: Some(query),
            elapsed_ms: elapsed,
        };
        Ok(status.clone())
    }

    pub fn fail(&self, message: impl Into<String>) -> Result<SearchStatus, String> {
        let mut status = self.status.lock().map_err(|_| "search status lock poisoned")?;
        *status = SearchStatus {
            phase: SearchPhase::Error,
            message: message.into(),
            query: status.query.clone(),
            elapsed_ms: status.elapsed_ms,
        };
        if let Ok(mut started) = self.started_at.lock() {
            *started = None;
        }
        Ok(status.clone())
    }

    pub fn reset(&self) -> Result<SearchStatus, String> {
        let mut status = self.status.lock().map_err(|_| "search status lock poisoned")?;
        *status = SearchStatus::default();
        if let Ok(mut started) = self.started_at.lock() {
            *started = None;
        }
        // Invalidate any outstanding listen timeout.
        if let Ok(mut generation) = self.listen_generation.lock() {
            *generation = generation.wrapping_add(1);
        }
        Ok(status.clone())
    }

    pub fn is_busy(&self) -> bool {
        self.status
            .lock()
            .map(|status| matches!(status.phase, SearchPhase::Listening | SearchPhase::Searching))
            .unwrap_or(false)
    }

    pub fn is_listening(&self) -> bool {
        self.status
            .lock()
            .map(|status| status.phase == SearchPhase::Listening)
            .unwrap_or(false)
    }

    pub fn remember_allowed_urls(&self, urls: HashSet<String>) {
        if let Ok(mut guard) = self.last_allowed_urls.lock() {
            *guard = urls;
        }
    }

    pub fn is_allowed_url(&self, url: &str) -> bool {
        self.last_allowed_urls
            .lock()
            .map(|urls| urls.contains(url))
            .unwrap_or(false)
    }

    pub fn resource_dir(&self) -> Option<PathBuf> {
        self.resource_dir.lock().ok().and_then(|guard| guard.clone())
    }
}

pub fn synthesize_search_ui(
    client: &Client,
    query: &str,
    hits: &[SearchHit],
    _resource_dir: Option<&std::path::Path>,
) -> Result<(SearchUiDocument, Option<String>), String> {
    let allowed: HashSet<String> = hits.iter().map(|hit| hit.url.clone()).collect();
    let sources: Vec<(u32, String, String, String)> = hits
        .iter()
        .enumerate()
        .map(|(index, hit)| {
            (
                (index + 1) as u32,
                hit.title.clone(),
                hit.url.clone(),
                hit.snippet.clone(),
            )
        })
        .collect();

    if hits.is_empty() {
        return Ok((
            fallback_document(query, &sources),
            Some("No evidence found in the retrieved sources.".into()),
        ));
    }

    let Some(api_key) = deepseek_key() else {
        return Ok((
            fallback_document(query, &sources),
            Some("Add a DeepSeek API key in Settings to synthesize answers.".into()),
        ));
    };

    match deepseek_search_ui(client, &api_key, query, hits) {
        Ok(raw) => match parse_and_validate_ui(&raw, &allowed) {
            Ok(document) => Ok((document, None)),
            // Do not spend a second LLM call repairing JSON — sources are
            // already on screen from the interim fallback document.
            Err(first_error) => Ok((
                fallback_document(query, &sources),
                Some(format!("UI synthesis failed validation: {first_error}")),
            )),
        },
        Err(error) => Ok((
            fallback_document(query, &sources),
            Some(format!("DeepSeek search UI failed: {error}")),
        )),
    }
}

pub fn deepseek_search_ui(
    client: &Client,
    api_key: &str,
    query: &str,
    hits: &[SearchHit],
) -> Result<String, String> {
    deepseek_search_ui_at(
        client,
        "https://api.deepseek.com/chat/completions",
        api_key,
        query,
        hits,
    )
}

fn compact_system_prompt() -> String {
    format!(
        r#"Pronto voice-search. Return ONLY JSON {{"nodes":[...]}} using {uri}.
Types: heading{{text}}, text{{text}}, divider, image_frame{{src,alt,caption?}}, youtube{{url,title?}}, button{{label,action,value}}, source_list{{items:[{{index,title,url,snippet?}}]}}, table{{columns,rows}}, chart{{chart_type:bar|line,labels,datasets:[{{label,data}}]}}.
Rules: use only provided sources; cite [n]; copy URLs exactly; no HTML/JS; ≤12 nodes; short text plus at most one visual."#,
        uri = mcp::DESIGN_SYSTEM_URI
    )
}

fn compact_sources_json(hits: &[SearchHit]) -> String {
    let rows: Vec<serde_json::Value> = hits
        .iter()
        .take(MAX_RESULTS)
        .enumerate()
        .map(|(index, hit)| {
            json!({
                "n": index + 1,
                "title": truncate_chars(&hit.title, 80),
                "url": hit.url,
                "snippet": truncate_chars(&hit.snippet, MAX_SNIPPET_CHARS),
            })
        })
        .collect();
    serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into())
}

fn truncate_chars(value: &str, max: usize) -> String {
    let mut chars = value.chars();
    let taken: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{taken}…")
    } else {
        taken
    }
}

fn search_ui_request_body(query: &str, hits: &[SearchHit]) -> serde_json::Value {
    json!({
        "model": "deepseek-v4-flash",
        "thinking": { "type": "disabled" },
        "messages": [
            { "role": "system", "content": compact_system_prompt() },
            { "role": "user", "content": format!("Q:{query}\nS:{}", compact_sources_json(hits)) }
        ],
        "temperature": 0.2,
        "max_tokens": SEARCH_MAX_TOKENS,
        "stream": false,
        "response_format": { "type": "json_object" }
    })
}

fn deepseek_search_ui_at(
    client: &Client,
    endpoint: &str,
    api_key: &str,
    query: &str,
    hits: &[SearchHit],
) -> Result<String, String> {
    let body = search_ui_request_body(query, hits);
    let response = client
        .post(endpoint)
        .timeout(std::time::Duration::from_secs(18))
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .map_err(|error| format!("DeepSeek search UI failed: {error}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().unwrap_or_default();
        return Err(format!("DeepSeek search UI returned {status}: {detail}"));
    }
    #[derive(Deserialize)]
    struct DeepSeekResponse {
        choices: Vec<Choice>,
    }
    #[derive(Deserialize)]
    struct Choice {
        message: Message,
    }
    #[derive(Deserialize)]
    struct Message {
        content: String,
    }
    response
        .json::<DeepSeekResponse>()
        .map_err(|error| format!("Invalid DeepSeek search UI response: {error}"))?
        .choices
        .into_iter()
        .next()
        .map(|choice| choice.message.content.trim().to_string())
        .filter(|content| !content.is_empty())
        .ok_or_else(|| "DeepSeek returned an empty search UI".to_string())
}

/// Ensure a source_list exists when the model omitted citations.
pub fn ensure_sources(document: &mut SearchUiDocument, hits: &[SearchHit]) {
    let has_sources = document
        .nodes
        .iter()
        .any(|node| matches!(node, UiNode::SourceList { .. }));
    if has_sources || hits.is_empty() {
        return;
    }
    document.nodes.push(UiNode::SourceList {
        items: hits
            .iter()
            .enumerate()
            .map(|(index, hit)| SourceItem {
                index: (index + 1) as u32,
                title: hit.title.clone(),
                url: hit.url.clone(),
                snippet: (!hit.snippet.is_empty()).then(|| hit.snippet.clone()),
            })
            .collect(),
    });
}

#[cfg(windows)]
pub fn open_url_in_default_browser(url: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide: Vec<u16> = std::ffi::OsStr::new(url)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let operation: Vec<u16> = std::ffi::OsStr::new("open")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as usize <= 32 {
        return Err(format!("Could not open URL in the default browser ({})", result.0 as usize));
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn open_url_in_default_browser(url: &str) -> Result<(), String> {
    Err(format!(
        "Opening URLs is only supported on Windows (requested {url})"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Duration;

    const FIXTURE: &str = r#"
<html><body>
<div class="result results_links web-result">
  <h2 class="result__title">
    <a rel="nofollow" class="result__a" href="https://duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Falpha">Alpha Title</a>
  </h2>
  <a class="result__snippet">Alpha snippet about widgets.</a>
</div>
<div class="result results_links web-result">
  <h2 class="result__title">
    <a class="result__a" href="https://example.com/beta">Beta Title</a>
  </h2>
  <a class="result__snippet">Beta snippet.</a>
</div>
</body></html>
"#;

    #[test]
    fn parses_duckduckgo_html_fixture() {
        let hits = parse_duckduckgo_html(FIXTURE);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Alpha Title");
        assert_eq!(hits[0].url, "https://example.com/alpha");
        assert!(hits[0].snippet.contains("Alpha snippet"));
        assert_eq!(hits[1].url, "https://example.com/beta");
    }

    #[test]
    fn mock_deepseek_search_ui_returns_json_object() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let count = stream.read(&mut chunk).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
                if let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..header_end + 4]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    while request.len() < header_end + 4 + content_length {
                        let count = stream.read(&mut chunk).unwrap();
                        if count == 0 {
                            break;
                        }
                        request.extend_from_slice(&chunk[..count]);
                    }
                    let body = String::from_utf8_lossy(&request[header_end + 4..]);
                    let _ = tx.send(body.into_owned());
                    break;
                }
            }
            let payload = r#"{"choices":[{"message":{"content":"{\"nodes\":[{\"type\":\"heading\",\"text\":\"Answer\"},{\"type\":\"text\",\"text\":\"See [1].\"},{\"type\":\"source_list\",\"items\":[{\"index\":1,\"title\":\"Alpha\",\"url\":\"https://example.com/alpha\",\"snippet\":\"snip\"}]}] }"}}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let hits = vec![SearchHit {
            title: "Alpha".into(),
            url: "https://example.com/alpha".into(),
            snippet: "snip".into(),
        }];
        let content = deepseek_search_ui_at(
            &client,
            &format!("http://{address}/chat/completions"),
            "test-key",
            "what is alpha",
            &hits,
        )
        .unwrap();
        let request = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(request.contains("deepseek-v4-flash"));
        assert!(request.contains("json_object"));
        assert!(request.contains("\"temperature\":0.2"));
        assert!(
            !request.contains("DESIGN SYSTEM CATALOG"),
            "full design-system.json must not be sent on every search"
        );
        assert!(request.contains("\"max_tokens\":700"));
        let allowed = hits.iter().map(|hit| hit.url.clone()).collect();
        let document = parse_and_validate_ui(&content, &allowed).unwrap();
        assert_eq!(document.nodes.len(), 3);
    }

    #[test]
    fn search_ui_prompt_stays_compact() {
        let hits: Vec<SearchHit> = (0..8)
            .map(|i| SearchHit {
                title: format!("Title {i} {}", "word ".repeat(40)),
                url: format!("https://example.com/{i}"),
                snippet: "x".repeat(800),
            })
            .collect();
        let body = search_ui_request_body("weather in austin texas this weekend", &hits);
        let system = body["messages"][0]["content"].as_str().unwrap();
        let user = body["messages"][1]["content"].as_str().unwrap();
        assert!(system.len() < 900, "system prompt was {} chars", system.len());
        assert!(user.len() < 1800, "user prompt was {} chars", user.len());
        assert_eq!(body["max_tokens"], 700);
        assert!(!user.contains(&"x".repeat(200)));
        assert_eq!(user.matches("https://example.com/").count(), 5);
    }

    #[test]
    fn search_does_not_touch_history_store() {
        // SearchController has no SettingsStore handle; completing a search
        // only mutates SearchStatus. This guards the architectural boundary.
        let controller = SearchController::new();
        let _ = controller.begin_listening(true).unwrap();
        let _ = controller.mark_searching(Some("weather".into())).unwrap();
        let status = controller
            .complete("weather".into(), None)
            .unwrap();
        assert_eq!(status.phase, SearchPhase::Complete);
        assert_eq!(status.query.as_deref(), Some("weather"));
    }

    #[test]
    fn listen_generation_invalidates_stale_watchdogs() {
        let controller = SearchController::new();
        let (_, first) = controller.begin_listening(true).unwrap();
        assert!(controller.is_listening_generation(first));
        let _ = controller.reset().unwrap();
        assert!(!controller.is_listening_generation(first));
        let (_, second) = controller.begin_listening(false).unwrap();
        assert_ne!(first, second);
        assert!(controller.is_listening_generation(second));
        let _ = controller.mark_transcribing().unwrap();
        assert!(!controller.is_listening_generation(second));
    }
}
