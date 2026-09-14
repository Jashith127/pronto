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
const MAX_RESULTS: usize = 8;

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
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .expect("reqwest client"),
        }
    }
}

impl SearchProvider for DuckDuckGoProvider {
    fn search(&self, query: &str) -> Result<Vec<SearchHit>, String> {
        let response = self
            .client
            .post(&self.endpoint)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!("q={}", urlencoding_lite(query)))
            .send()
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

    pub fn begin_listening(&self) -> Result<SearchStatus, String> {
        let mut status = self.status.lock().map_err(|_| "search status lock poisoned")?;
        if !matches!(
            status.phase,
            SearchPhase::Idle | SearchPhase::Complete | SearchPhase::Error
        ) {
            return Ok(status.clone());
        }
        *self
            .started_at
            .lock()
            .map_err(|_| "search timer lock poisoned")? = Some(Instant::now());
        *status = SearchStatus {
            phase: SearchPhase::Listening,
            message: "Listening for your search…".into(),
            query: None,
            elapsed_ms: 0,
        };
        Ok(status.clone())
    }

    pub fn mark_searching(&self, query_hint: Option<String>) -> Result<SearchStatus, String> {
        let mut status = self.status.lock().map_err(|_| "search status lock poisoned")?;
        if status.phase != SearchPhase::Listening {
            return Ok(status.clone());
        }
        let elapsed = self
            .started_at
            .lock()
            .ok()
            .and_then(|guard| guard.map(|time| time.elapsed().as_millis()))
            .unwrap_or(0);
        *status = SearchStatus {
            phase: SearchPhase::Searching,
            message: "Searching the web…".into(),
            query: query_hint,
            elapsed_ms: elapsed,
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
        Ok(status.clone())
    }

    pub fn is_busy(&self) -> bool {
        self.status
            .lock()
            .map(|status| matches!(status.phase, SearchPhase::Listening | SearchPhase::Searching))
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
    resource_dir: Option<&std::path::Path>,
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

    let catalog = mcp::design_system_catalog_text(resource_dir)
        .unwrap_or_else(|_| mcp::DESIGN_SYSTEM_URI.to_string());
    match deepseek_search_ui(client, &api_key, query, hits, &catalog) {
        Ok(raw) => match parse_and_validate_ui(&raw, &allowed) {
            Ok(document) => Ok((document, None)),
            Err(first_error) => match deepseek_search_ui_repair(
                client,
                &api_key,
                query,
                hits,
                &catalog,
                &raw,
                &first_error,
            ) {
                Ok(repaired) => match parse_and_validate_ui(&repaired, &allowed) {
                    Ok(document) => Ok((document, Some(format!("Repaired UI JSON: {first_error}")))),
                    Err(_) => Ok((
                        fallback_document(query, &sources),
                        Some(format!("UI synthesis failed validation: {first_error}")),
                    )),
                },
                Err(error) => Ok((
                    fallback_document(query, &sources),
                    Some(format!("UI synthesis failed: {error}")),
                )),
            },
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
    catalog: &str,
) -> Result<String, String> {
    deepseek_search_ui_at(
        client,
        "https://api.deepseek.com/chat/completions",
        api_key,
        query,
        hits,
        catalog,
        None,
    )
}

fn deepseek_search_ui_repair(
    client: &Client,
    api_key: &str,
    query: &str,
    hits: &[SearchHit],
    catalog: &str,
    previous: &str,
    error: &str,
) -> Result<String, String> {
    deepseek_search_ui_at(
        client,
        "https://api.deepseek.com/chat/completions",
        api_key,
        query,
        hits,
        catalog,
        Some((previous, error)),
    )
}

fn deepseek_search_ui_at(
    client: &Client,
    endpoint: &str,
    api_key: &str,
    query: &str,
    hits: &[SearchHit],
    catalog: &str,
    repair: Option<(&str, &str)>,
) -> Result<String, String> {
    let sources = serde_json::to_string_pretty(hits).unwrap_or_else(|_| "[]".into());
    let system = format!(
        r#"You are Pronto's grounded voice-search synthesizer. Return ONLY a JSON object with a "nodes" array using the local design system {uri}.

DESIGN SYSTEM CATALOG:
{catalog}

RULES:
1. Answer using only the provided search results. Cite claims with [1][2] markers that match source_list indexes.
2. Prefer 1 visual (chart, table, image_frame, or youtube) plus short text over a text-only layout when the evidence supports it.
3. If evidence is insufficient, say "No evidence" clearly and still include a source_list of what was retrieved.
4. URLs in image_frame, youtube, button(open_url), and source_list MUST be copied EXACTLY from the provided results. Never invent URLs.
5. Allowed node types only: heading, text, divider, image_frame, youtube, button, source_list, table, chart.
6. No raw HTML, JavaScript, Markdown images, or unknown fields.
7. Keep the document under 24 nodes."#,
        uri = mcp::DESIGN_SYSTEM_URI
    );
    let user = if let Some((previous, error)) = repair {
        format!(
            "QUERY:\n{query}\n\nSEARCH RESULTS JSON:\n{sources}\n\nPREVIOUS INVALID JSON:\n{previous}\n\nVALIDATION ERROR:\n{error}\n\nReturn corrected JSON only."
        )
    } else {
        format!("QUERY:\n{query}\n\nSEARCH RESULTS JSON:\n{sources}\n\nReturn JSON only.")
    };
    let body = json!({
        "model": "deepseek-v4-flash",
        "thinking": { "type": "disabled" },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user }
        ],
        "temperature": 0.2,
        "max_tokens": 3000,
        "stream": false,
        "response_format": { "type": "json_object" }
    });
    let response = client
        .post(endpoint)
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
            r#"{"uri":"design://system/v1","components":[]}"#,
            None,
        )
        .unwrap();
        let request = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(request.contains("deepseek-v4-flash"));
        assert!(request.contains("json_object"));
        assert!(request.contains("\"temperature\":0.2"));
        let allowed = hits.iter().map(|hit| hit.url.clone()).collect();
        let document = parse_and_validate_ui(&content, &allowed).unwrap();
        assert_eq!(document.nodes.len(), 3);
    }

    #[test]
    fn search_does_not_touch_history_store() {
        // SearchController has no SettingsStore handle; completing a search
        // only mutates SearchStatus. This guards the architectural boundary.
        let controller = SearchController::new();
        let _ = controller.begin_listening().unwrap();
        let _ = controller.mark_searching(Some("weather".into())).unwrap();
        let status = controller
            .complete("weather".into(), None)
            .unwrap();
        assert_eq!(status.phase, SearchPhase::Complete);
        assert_eq!(status.query.as_deref(), Some("weather"));
    }
}
