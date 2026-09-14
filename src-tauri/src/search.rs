use crate::mcp;
use crate::settings::deepseek_key;
use crate::ui_schema::{
    fallback_document_with_images, parse_and_validate_ui, SearchUiDocument, SourceItem, UiNode,
};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const DEFAULT_PROVIDER_URL: &str = "https://html.duckduckgo.com/html/";
const MAX_RESULTS: usize = 8;
const SNIPPET_TRUNCATE_CHARS: usize = 180;
const SEARCH_CACHE_TTL: Duration = Duration::from_secs(600);
const SEARCH_CACHE_CAP: usize = 64;

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
    /// Optional thumbnail extracted from DDG HTML (`<img>` in result block).
    /// `None` is the common case; favicons are derived separately so this
    /// stays cheap with no extra backend fetches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryKind {
    /// Simple factual / navigation query — render fallback + image, skip LLM.
    Fast,
    /// Needs grounded LLM synthesis.
    Grounded,
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
    #[allow(dead_code)]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self::with_client(endpoint, shared_search_client().clone())
    }

    pub fn with_client(endpoint: impl Into<String>, client: Client) -> Self {
        Self {
            endpoint: endpoint.into(),
            client,
        }
    }
}

/// Shared blocking client with keep-alive so repeat searches skip TLS + TCP
/// setup. Built once; 3.5s timeout keeps the pill snappy on slow networks.
pub fn shared_search_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent("ProntoVoiceSearch/0.1 (+local; DuckDuckGo HTML)")
            .timeout(Duration::from_secs(4))
            .connect_timeout(Duration::from_secs(3))
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(4)
            .tcp_nodelay(true)
            .build()
            .expect("reqwest client")
    })
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
        let raw_snippet = block
            .find("result__snippet")
            .and_then(|idx| extract_between(&block[idx..], '>', "</"))
            .map(|value| strip_tags(&decode_html_entities(value)).trim().to_string())
            .unwrap_or_default();
        let snippet = truncate_snippet(&raw_snippet, SNIPPET_TRUNCATE_CHARS);
        if hits.iter().any(|hit: &SearchHit| hit.url == url) {
            continue;
        }
        let image = extract_thumb_from_block(block);
        hits.push(SearchHit {
            title: title.trim().to_string(),
            url,
            snippet,
            image,
        });
    }
    hits
}

fn extract_thumb_from_block(block: &str) -> Option<String> {
    // Cheap: reuse already-downloaded HTML. Looks for <img src="https://...">.
    // Only keeps direct image files to avoid stuffing HTML pages into <img>.
    let mut search_from = 0;
    while let Some(img_idx) = block[search_from..].find("<img") {
        let abs_idx = search_from + img_idx;
        let tag_slice = &block[abs_idx..];
        let tag_end = tag_slice.find('>').map(|i| abs_idx + i).unwrap_or(block.len());
        let tag = &block[abs_idx..tag_end.min(block.len())];
        if let Some(src_idx) = tag.find("src=\"") {
            let rest = &tag[src_idx + 5..];
            if let Some(end) = rest.find('"') {
                let mut candidate = decode_html_entities(&rest[..end]);
                candidate = unwrap_ddg_redirect(&candidate);
                let lower = candidate.to_ascii_lowercase();
                let is_image = (lower.starts_with("https://") || lower.starts_with("http://"))
                    && (lower.split(['?', '#']).next().is_some_and(|path| {
                        path.ends_with(".png")
                            || path.ends_with(".jpg")
                            || path.ends_with(".jpeg")
                            || path.ends_with(".webp")
                            || path.ends_with(".gif")
                            || path.ends_with(".avif")
                    }));
                if is_image {
                    return Some(candidate);
                }
            }
        }
        search_from = tag_end.min(block.len());
        if search_from >= block.len() {
            break;
        }
    }
    None
}

fn truncate_snippet(raw: &str, max_chars: usize) -> String {
    let trimmed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.chars().count() <= max_chars {
        return trimmed;
    }
    let mut end = 0usize;
    let mut count = 0usize;
    for (byte_idx, _) in trimmed.char_indices() {
        if count >= max_chars {
            end = byte_idx;
            break;
        }
        count += 1;
        end = byte_idx;
    }
    let mut out: String = trimmed.chars().take(max_chars).collect();
    // Prefer word boundary.
    if let Some(space) = out.rfind(' ') {
        if space > max_chars.saturating_sub(30) {
            out.truncate(space);
        }
    }
    let _ = end;
    format!("{}…", out.trim_end())
}

/// Lightweight relevance: title matches weigh 2x, snippet 1x.
pub fn rerank_hits(query: &str, hits: &mut Vec<SearchHit>) {
    let terms: Vec<String> = normalize_query(query)
        .split_whitespace()
        .filter(|w| w.len() > 2)
        .map(|s| s.to_string())
        .collect();
    if terms.is_empty() {
        return;
    }
    hits.sort_by(|a, b| {
        let score = |hit: &SearchHit| {
            let title = hit.title.to_ascii_lowercase();
            let snippet = hit.snippet.to_ascii_lowercase();
            terms.iter().map(|t| {
                let in_title = title.matches(t).count() as i32 * 2;
                let in_snip = snippet.matches(t).count() as i32;
                in_title + in_snip
            }).sum::<i32>()
        };
        score(b).cmp(&score(a))
    });
}

pub fn truncate_hits(mut hits: Vec<SearchHit>, top_k: usize) -> Vec<SearchHit> {
    // De-dup case-insensitively by URL, then keep top_k.
    let mut seen = HashSet::new();
    hits.retain(|hit| seen.insert(hit.url.to_ascii_lowercase()));
    hits.truncate(top_k);
    hits
}

/// Lowercase, strip voice fillers / command prefixes, collapse spaces.
pub fn normalize_query(raw: &str) -> String {
    let mut query = raw.trim().to_lowercase();
    for prefix in [
        "hey pronto search for",
        "hey pronto",
        "pronto search for",
        "search for",
        "search",
        "look up",
        "lookup",
        "find",
        "google",
    ] {
        if let Some(rest) = query.strip_prefix(prefix) {
            if rest.starts_with([' ', ',', ':']) || rest.is_empty() {
                query = rest.trim_start_matches([' ', ',', ':']).to_string();
                break;
            }
        }
    }
    // Remove standalone fillers but keep question structure.
    let fillers = ["um", "uh", "erm", "please"];
    let words: Vec<&str> = query
        .split_whitespace()
        .filter(|w| {
            let core: String = w.chars().filter(|c| c.is_alphanumeric()).collect();
            !fillers.contains(&core.as_str())
        })
        .collect();
    let mut out = words.join(" ");
    // Fix common ASR spacing around question words.
    out = out.replace("whats", "what's").replace("what s", "what's");
    if out.chars().count() > 300 {
        out = out.chars().take(300).collect();
        if let Some(space) = out.rfind(' ') {
            out.truncate(space);
        }
    }
    out.trim().to_string()
}

/// Rule-based follow-up expansion using last queries (no LLM call).
/// "what about tomorrow?" + ["weather osaka"] -> "weather osaka tomorrow".
pub fn expand_followup(query: &str, recent: &[String]) -> String {
    let trimmed = query.trim();
    let lower = trimmed.to_ascii_lowercase();
    let is_followup = ["and ", "what about", "how about", "then", "and then", "what if", "also "]
        .iter()
        .any(|prefix| lower.starts_with(prefix));
    if !is_followup {
        return trimmed.to_string();
    }
    let Some(last) = recent.iter().rev().find(|q| !q.trim().is_empty()) else {
        return trimmed.to_string();
    };
    // Strip the follow-up prefix, keep the new constraint.
    let mut constraint = trimmed.to_string();
    for prefix in ["what about", "how about", "and then", "and", "then", "also", "what if"] {
        if lower.starts_with(prefix) {
            constraint = trimmed[prefix.len()..]
                .trim_start_matches([' ', ',', '?', '!'])
                .to_string();
            break;
        }
    }
    if constraint.trim().is_empty() {
        return last.clone();
    }
    // Avoid duplicating if last already contains constraint.
    if last.to_ascii_lowercase().contains(&constraint.to_ascii_lowercase()) {
        return last.clone();
    }
    format!("{} {}", last.trim(), constraint.trim())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn classify_query(query: &str) -> QueryKind {
    let normalized = normalize_query(query);
    let words = normalized.split_whitespace().count();
    let lower = normalized.to_ascii_lowercase();
    // Time-sensitive or comparison queries always need grounding.
    let needs_grounding = [
        "compare", " vs ", " versus ", "table", "chart", "graph", "stats", "statistics",
        "price", "today", "now", "score", "weather", "news",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    if needs_grounding {
        return QueryKind::Grounded;
    }
    // Fast-path is ONLY for pure navigation intents where the link list IS
    // the answer (open / watch). Who/what/define/meaning questions must go
    // through the LLM — otherwise the card says "check below for sources"
    // with no actual answer (reported bug: "Who is the president of India?").
    let navigation_markers = [
        "open ", "youtube", "watch ", "video of ", "play ",
    ];
    if words <= 8 && navigation_markers.iter().any(|m| lower.contains(m)) {
        return QueryKind::Fast;
    }
    QueryKind::Grounded
}

/// Only chart/table-flavored queries get the paid repair retry.
pub fn needs_visual_repair(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    [
        "chart", "graph", "table", "compare", "comparison", " vs ", " versus ",
        "stats", "statistics", "timeline", "breakdown",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

pub fn adaptive_max_tokens(query: &str, kind: QueryKind) -> u32 {
    if kind == QueryKind::Fast {
        return 400;
    }
    if needs_visual_repair(query) {
        return 1400;
    }
    800
}

/// True for tiny site icons — crisp at 14px in the source list, but a
/// blurry mess when blown up into a 600px banner (reported bug).
pub fn is_favicon_candidate(src: &str) -> bool {
    src.contains("icons.duckduckgo.com/ip3/")
        || src.contains("/favicons")
        || src.ends_with(".ico")
}

/// `(src, alt)` candidates: real thumbs first, then host favicons.
/// Kept for allowlist validation + small source-list icons.
pub fn image_candidates_for_hits(hits: &[SearchHit]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for hit in hits.iter().take(5) {
        if let Some(thumb) = hit.image.as_ref() {
            if seen.insert(thumb.clone()) {
                out.push((thumb.clone(), hit.title.clone()));
            }
        }
    }
    for hit in hits.iter().take(5) {
        if let Some(favicon) = crate::ui_schema::favicon_for_url(&hit.url) {
            if seen.insert(favicon.clone()) {
                let host = crate::ui_schema::host_of_url(&hit.url).unwrap_or_default();
                out.push((favicon, format!("{} — {}", hit.title, host)));
                if out.len() >= 3 {
                    break;
                }
            }
        }
    }
    out
}

/// Large-banner candidates ONLY: real photos, never favicons.
/// A 16px favicon stretched to full width is what produced the bloody /
/// blurry flag banner — those now render as tiny icons in the source list
/// instead (see `ui/search.js`).
pub fn photo_candidates_for_hits(hits: &[SearchHit]) -> Vec<(String, String)> {
    image_candidates_for_hits(hits)
        .into_iter()
        .filter(|(src, _)| !is_favicon_candidate(src))
        .collect()
}

pub fn expanded_allowed_urls(hits: &[SearchHit]) -> HashSet<String> {
    expanded_allowed_urls_with_images(hits, &[])
}

/// Result URLs plus every vetted image URL (thumbnails, DDG proxies, favicons).
pub fn expanded_allowed_urls_with_images(
    hits: &[SearchHit],
    images: &[(String, String)],
) -> HashSet<String> {
    let mut allowed: HashSet<String> = hits.iter().map(|hit| hit.url.clone()).collect();
    for (src, _) in image_candidates_for_hits(hits) {
        allowed.insert(src);
    }
    for hit in hits {
        if let Some(thumb) = hit.image.as_ref() {
            allowed.insert(thumb.clone());
        }
    }
    for (src, _) in images {
        allowed.insert(src.clone());
    }
    allowed
}

/// Banner photos: result thumbnails first, then DuckDuckGo image search.
pub fn banner_images_for_search(
    client: &Client,
    query: &str,
    hits: &[SearchHit],
    max: usize,
) -> Vec<(String, String)> {
    let mut out = photo_candidates_for_hits(hits);
    if out.len() >= max {
        return out.into_iter().take(max).collect();
    }
    let mut seen: HashSet<String> = out.iter().map(|(src, _)| src.clone()).collect();
    for (src, alt) in fetch_ddg_images(client, query, max) {
        if seen.insert(src.clone()) {
            out.push((src, alt));
            if out.len() >= max {
                break;
            }
        }
    }
    out
}

/// Fetch an allowlisted image for the search WebView (handles referrer / hotlink quirks).
pub fn fetch_allowlisted_image(client: &Client, url: &str) -> Result<(String, Vec<u8>), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("Only http(s) image URLs are supported".into());
    }
    let response = client
        .get(url)
        .header("Accept", "image/*,*/*;q=0.8")
        .header("Referer", "https://duckduckgo.com/")
        .send()
        .map_err(|error| format!("Image fetch failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("Image host returned {}", response.status()));
    }
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(';').next().unwrap_or(value).trim().to_string())
        .filter(|value| value.starts_with("image/"))
        .unwrap_or_else(|| guess_image_mime(url));
    let bytes = response
        .bytes()
        .map_err(|error| format!("Image body unreadable: {error}"))?
        .to_vec();
    if bytes.is_empty() {
        return Err("Image body was empty".into());
    }
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("Image exceeds the 4 MB limit".into());
    }
    Ok((mime, bytes))
}

fn guess_image_mime(url: &str) -> String {
    let lower = url.to_ascii_lowercase();
    if lower.contains(".png") {
        "image/png".into()
    } else if lower.contains(".webp") {
        "image/webp".into()
    } else if lower.contains(".gif") {
        "image/gif".into()
    } else if lower.contains(".avif") {
        "image/avif".into()
    } else if lower.contains(".ico") {
        "image/x-icon".into()
    } else {
        "image/jpeg".into()
    }
}

// ---- DDG image search (i.js): real photos for banners ----

const IMAGE_FETCH_TIMEOUT: Duration = Duration::from_secs(3);
const IMAGE_RESULT_CAP: usize = 3;

type ImageCacheValue = (Instant, Vec<(String, String)>);

fn image_cache() -> &'static Mutex<HashMap<String, ImageCacheValue>> {
    static CACHE: OnceLock<Mutex<HashMap<String, ImageCacheValue>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn cached_images_for(normalized_query: &str) -> Option<Vec<(String, String)>> {
    image_cache().lock().ok().and_then(|cache| {
        cache.get(normalized_query).and_then(|(at, images)| {
            if at.elapsed() < SEARCH_CACHE_TTL {
                Some(images.clone())
            } else {
                None
            }
        })
    })
}

pub fn store_images_cache(normalized_query: String, images: Vec<(String, String)>) {
    if let Ok(mut cache) = image_cache().lock() {
        if cache.len() >= SEARCH_CACHE_CAP {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(normalized_query, (Instant::now(), images));
    }
}

/// Fetch up to `max` real photo URLs from DuckDuckGo image search.
/// Two lightweight steps (vqd token + i.js JSON), ~300-800ms, short timeout.
/// Failures return empty — banners are optional, never fatal.
pub fn fetch_ddg_images(client: &Client, query: &str, max: usize) -> Vec<(String, String)> {
    let normalized = normalize_query(query);
    if normalized.is_empty() {
        return Vec::new();
    }
    if let Some(cached) = cached_images_for(&normalized) {
        return cached.into_iter().take(max).collect();
    }
    let fetched = fetch_ddg_images_network(client, &normalized, max);
    if !fetched.is_empty() {
        store_images_cache(normalized, fetched.clone());
    }
    fetched
}

fn fetch_ddg_images_network(
    client: &Client,
    normalized_query: &str,
    max: usize,
) -> Vec<(String, String)> {
    let encoded = urlencoding_lite(normalized_query);
    // Step 1: vqd token embedded in the image-search HTML.
    let search_url = format!("https://duckduckgo.com/?q={encoded}&iar=images&iax=images&ia=images");
    let html = match client
        .get(&search_url)
        .timeout(IMAGE_FETCH_TIMEOUT)
        .header("Accept", "text/html")
        .send()
    {
        Ok(response) => match response.text() {
            Ok(text) => text,
            Err(_) => return Vec::new(),
        },
        Err(_) => return Vec::new(),
    };
    let Some(vqd) = parse_ddg_vqd(&html) else {
        return Vec::new();
    };
    // Step 2: JSON results. `thumbnail` is a fast DDG proxy URL; `image` is
    // the original (often hotlink-protected / slow) — prefer thumbnail.
    let json_url = format!(
        "https://duckduckgo.com/i.js?l=us-en&o=json&q={encoded}&vqd={vqd}&f=,,,,
,&p=1"
    );
    // Note: `f` param must be `,,,,,` — written across lines above only for
    // readability; rebuild without whitespace.
    let json_url = json_url.replace([' ', '\n'], "");
    let body = match client
        .get(&json_url)
        .timeout(IMAGE_FETCH_TIMEOUT)
        .header("Referer", "https://duckduckgo.com/")
        .header("Accept", "application/json")
        .send()
    {
        Ok(response) => match response.text() {
            Ok(text) => text,
            Err(_) => return Vec::new(),
        },
        Err(_) => return Vec::new(),
    };
    parse_ddg_image_json(&body, max, normalized_query)
}

pub fn parse_ddg_vqd(html: &str) -> Option<String> {
    for marker in ["vqd=\"", "vqd='", "vqd="] {
        let mut search_from = 0;
        while let Some(idx) = html[search_from..].find(marker) {
            let abs_idx = search_from + idx + marker.len();
            let rest = &html[abs_idx..];
            let quoted = marker.ends_with('"') || marker.ends_with('\'');
            let end = if quoted {
                let quote = marker.chars().last().unwrap();
                rest.find(quote)
            } else {
                rest.find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
            };
            if let Some(end) = end {
                let token = rest[..end].trim();
                if token.len() >= 8 {
                    return Some(token.to_string());
                }
            }
            search_from = abs_idx.min(html.len());
            if search_from >= html.len() {
                break;
            }
        }
    }
    None
}

pub fn parse_ddg_image_json(
    body: &str,
    max: usize,
    query_hint: &str,
) -> Vec<(String, String)> {
    #[derive(Deserialize)]
    struct ImageApiResponse {
        #[serde(default)]
        results: Vec<ImageApiItem>,
    }
    #[derive(Deserialize)]
    struct ImageApiItem {
        #[serde(default)]
        thumbnail: String,
        #[serde(default)]
        image: String,
        #[serde(default)]
        title: String,
    }
    let response: ImageApiResponse = match serde_json::from_str(body) {
        Ok(parsed) => parsed,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for item in response.results {
        if out.len() >= max.min(IMAGE_RESULT_CAP) {
            break;
        }
        let src = if item.thumbnail.starts_with("https://") {
            item.thumbnail
        } else if item.image.starts_with("https://") {
            item.image
        } else {
            continue;
        };
        if !seen.insert(src.clone()) {
            continue;
        }
        let alt = if item.title.trim().is_empty() {
            query_hint.to_string()
        } else {
            item.title.trim().to_string()
        };
        out.push((src, alt));
    }
    out
}

// ---- retrieval cache: normalized query -> (timestamp, hits) ----

type HitsCacheValue = (Instant, Vec<SearchHit>);

fn hits_cache() -> &'static Mutex<HashMap<String, HitsCacheValue>> {
    static CACHE: OnceLock<Mutex<HashMap<String, HitsCacheValue>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn cached_hits_for(normalized_query: &str) -> Option<Vec<SearchHit>> {
    hits_cache().lock().ok().and_then(|cache| {
        cache.get(normalized_query).and_then(|(at, hits)| {
            if at.elapsed() < SEARCH_CACHE_TTL {
                Some(hits.clone())
            } else {
                None
            }
        })
    })
}

pub fn store_hits_cache(normalized_query: String, hits: Vec<SearchHit>) {
    if let Ok(mut cache) = hits_cache().lock() {
        if cache.len() >= SEARCH_CACHE_CAP {
            // Evict oldest entry (cheap linear scan; cap is tiny).
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(normalized_query, (Instant::now(), hits));
    }
}

pub fn is_time_sensitive(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    ["today", "now", "current", "live ", "score", "price", "weather", "stock"]
        .iter()
        .any(|m| lower.contains(m))
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
    /// Short session memory for follow-ups: last 3 queries, 5min window.
    /// Purely local, no extra LLM calls — used by `expand_followup`.
    history: Mutex<VecDeque<(String, Instant)>>,
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
            history: Mutex::new(VecDeque::new()),
        }
    }

    /// Recent queries for follow-up expansion (oldest first, max 3).
    pub fn recent_queries(&self) -> Vec<String> {
        self.history
            .lock()
            .map(|history| {
                history
                    .iter()
                    .filter(|(_, at)| at.elapsed() < Duration::from_secs(300))
                    .map(|(query, _)| query.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn push_history(&self, query: String) {
        if query.trim().is_empty() {
            return;
        }
        if let Ok(mut history) = self.history.lock() {
            history.retain(|(_, at)| at.elapsed() < Duration::from_secs(300));
            if history.back().is_some_and(|(last, _)| last == &query) {
                return;
            }
            history.push_back((query, Instant::now()));
            while history.len() > 3 {
                history.pop_front();
            }
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
        self.push_history(query.clone());
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
    resource_dir: Option<&std::path::Path>,
    images: &[(String, String)],
) -> Result<(SearchUiDocument, Option<String>), String> {
    let allowed: HashSet<String> = expanded_allowed_urls_with_images(hits, images);
    let photos = images;
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
            fallback_document_with_images(query, &sources, photos),
            Some("No evidence found in the retrieved sources.".into()),
        ));
    }

    let Some(api_key) = deepseek_key() else {
        return Ok((
            fallback_document_with_images(query, &sources, photos),
            Some("Add a DeepSeek API key in Settings to synthesize answers.".into()),
        ));
    };

    // Fast-path: navigation queries render instantly with photo + sources, $0.
    if classify_query(query) == QueryKind::Fast {
        return Ok((
            fallback_document_with_images(query, &sources, photos),
            None,
        ));
    }

    // Minified catalog saves ~30-40% system tokens with identical content.
    let catalog = mcp::design_system_catalog_minified(resource_dir).unwrap_or_else(|_| {
        mcp::design_system_catalog_text(resource_dir)
            .unwrap_or_else(|_| mcp::DESIGN_SYSTEM_URI.to_string())
    });
    match deepseek_search_ui(client, &api_key, query, hits, &catalog, photos) {
        Ok(raw) => match parse_and_validate_ui(&raw, &allowed) {
            Ok(mut document) => {
                ensure_image(&mut document, photos);
                Ok((document, None))
            }
            Err(first_error) => {
                // Locked policy: only chart/table queries get the paid repair.
                if !needs_visual_repair(query) {
                    return Ok((
                        fallback_document_with_images(query, &sources, photos),
                        Some(format!("UI synthesis failed validation: {first_error}")),
                    ));
                }
                match deepseek_search_ui_repair(client, &api_key, query, hits, &catalog, photos, &raw, &first_error) {
                    Ok(repaired) => match parse_and_validate_ui(&repaired, &allowed) {
                        Ok(mut document) => {
                            ensure_image(&mut document, photos);
                            Ok((document, Some(format!("Repaired UI JSON: {first_error}"))))
                        }
                        Err(_) => Ok((
                            fallback_document_with_images(query, &sources, photos),
                            Some(format!("UI synthesis failed validation: {first_error}")),
                        )),
                    },
                    Err(error) => Ok((
                        fallback_document_with_images(query, &sources, photos),
                        Some(format!("UI synthesis failed: {error}")),
                    )),
                }
            }
        },
        Err(error) => Ok((
            fallback_document_with_images(query, &sources, photos),
            Some(format!("DeepSeek search UI failed: {error}")),
        )),
    }
}

/// Guarantee frequent visuals: if the model returned text-only, inject the
/// top vetted image after the first answer paragraph. Zero extra LLM cost.
fn ensure_image(document: &mut SearchUiDocument, images: &[(String, String)]) {
    if images.is_empty() {
        return;
    }
    let has_visual = document.nodes.iter().any(|node| matches!(
        node,
        UiNode::ImageFrame { .. } | UiNode::Youtube { .. } | UiNode::Chart { .. } | UiNode::Table { .. }
    ));
    if has_visual {
        return;
    }
    let (src, alt) = &images[0];
    let insert_at = document
        .nodes
        .iter()
        .position(|node| matches!(node, UiNode::Text { .. }))
        .map(|idx| idx + 1)
        .unwrap_or(0)
        .min(document.nodes.len());
    document.nodes.insert(
        insert_at,
        UiNode::ImageFrame {
            src: src.clone(),
            alt: alt.clone(),
            caption: None,
        },
    );
}

pub fn deepseek_search_ui(
    client: &Client,
    api_key: &str,
    query: &str,
    hits: &[SearchHit],
    catalog: &str,
    images: &[(String, String)],
) -> Result<String, String> {
    deepseek_search_ui_at(
        client,
        "https://api.deepseek.com/chat/completions",
        api_key,
        query,
        hits,
        catalog,
        images,
        None,
    )
}

fn deepseek_search_ui_repair(
    client: &Client,
    api_key: &str,
    query: &str,
    hits: &[SearchHit],
    catalog: &str,
    images: &[(String, String)],
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
        images,
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
    images: &[(String, String)],
    repair: Option<(&str, &str)>,
) -> Result<String, String> {
    // Compact JSON (not pretty) + truncated snippets already keep tokens low.
    let sources = serde_json::to_string(hits).unwrap_or_else(|_| "[]".into());
    let image_list = if images.is_empty() {
        "(none)".to_string()
    } else {
        images
            .iter()
            .take(3)
            .enumerate()
            .map(|(i, (src, alt))| format!("{}. src={src} alt={alt}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let system = format!(
        r#"You are Pronto's grounded voice-search synthesizer. Return ONLY a JSON object with a "nodes" array using the local design system {uri}.

DESIGN SYSTEM CATALOG:
{catalog}

RULES:
1. Answer using only the provided search results. Cite claims with [1][2] markers that match source_list indexes.
2. Do NOT include a heading node — the spoken query is already shown in the overlay. Start with a text answer, then include exactly 1 image_frame using IMAGE CANDIDATES below when available. Always set alt + short caption from evidence. Skip image only if candidates are "(none)".
3. If evidence is insufficient, say "No evidence" clearly and still include a source_list of what was retrieved.
4. image_frame.src MUST be copied EXACTLY from IMAGE CANDIDATES. youtube/button(open_url)/source_list URLs MUST be copied EXACTLY from search results. Never invent URLs.
5. Allowed node types only: heading, text, divider, image_frame, youtube, button, source_list, table, chart.
6. No raw HTML, JavaScript, Markdown images, or unknown fields.
7. Keep the document under 24 nodes, answer text concise."#,
        uri = mcp::DESIGN_SYSTEM_URI
    );
    let user = if let Some((previous, error)) = repair {
        format!(
            "QUERY:\n{query}\n\nSEARCH RESULTS JSON:\n{sources}\n\nIMAGE CANDIDATES:\n{image_list}\n\nPREVIOUS INVALID JSON:\n{previous}\n\nVALIDATION ERROR:\n{error}\n\nReturn corrected JSON only."
        )
    } else {
        format!("QUERY:\n{query}\n\nSEARCH RESULTS JSON:\n{sources}\n\nIMAGE CANDIDATES:\n{image_list}\n\nReturn JSON only.")
    };
    let max_tokens = adaptive_max_tokens(query, classify_query(query));
    let body = json!({
        "model": "deepseek-v4-flash",
        "thinking": { "type": "disabled" },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user }
        ],
        "temperature": 0,
        "max_tokens": max_tokens,
        "stream": false,
        "response_format": { "type": "json_object" }
    });
    let response = client
        .post(endpoint)
        .timeout(std::time::Duration::from_secs(10))
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
            image: None,
        }];
        let content = deepseek_search_ui_at(
            &client,
            &format!("http://{address}/chat/completions"),
            "test-key",
            "compare alpha vs beta chart",
            &hits,
            r#"{"uri":"design://system/v1","components":[]}"#,
            &[],
            None,
        )
        .unwrap();
        let request = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(request.contains("deepseek-v4-flash"));
        assert!(request.contains("json_object"));
        assert!(request.contains("\"temperature\":0")); 
        let allowed = hits.iter().map(|hit| hit.url.clone()).collect();
        let document = parse_and_validate_ui(&content, &allowed).unwrap();
        assert_eq!(document.nodes.len(), 3);
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

    #[test]
    fn normalize_strips_fillers_and_prefixes() {
        assert_eq!(normalize_query("  Search for um best ramens in Osaka please "), "best ramens in osaka");
        assert_eq!(normalize_query("hey pronto search for weather"), "weather");
    }

    #[test]
    fn followup_expands_with_recent_query() {
        let recent = vec!["weather osaka".to_string()];
        assert_eq!(
            expand_followup("what about tomorrow?", &recent),
            "weather osaka tomorrow?"
        );
        assert_eq!(
            expand_followup("weather tokyo", &recent),
            "weather tokyo"
        );
    }

    #[test]
    fn fast_path_only_for_navigation_queries() {
        // Navigation: link list IS the answer — skip LLM.
        assert_eq!(classify_query("open youtube lofi"), QueryKind::Fast);
        assert_eq!(classify_query("watch lofi video"), QueryKind::Fast);
        // Who/what/define must go through the LLM so the card contains an
        // actual answer instead of "check below for sources".
        assert_eq!(classify_query("what is photosynthesis"), QueryKind::Grounded);
        assert_eq!(
            classify_query("Who is the president of India?"),
            QueryKind::Grounded
        );
        assert_eq!(
            classify_query("compare iphone vs pixel with table and stats"),
            QueryKind::Grounded
        );
    }

    #[test]
    fn visual_repair_only_for_chart_table_queries() {
        assert!(needs_visual_repair("compare prices with chart"));
        assert!(needs_visual_repair("show me a table of scores"));
        assert!(!needs_visual_repair("weather in osaka"));
        assert!(!needs_visual_repair("who is einstein"));
        assert_eq!(adaptive_max_tokens("weather osaka", QueryKind::Fast), 400);
        assert_eq!(
            adaptive_max_tokens("compare a vs b chart", QueryKind::Grounded),
            1400
        );
        assert_eq!(
            adaptive_max_tokens("capital of france history", QueryKind::Grounded),
            800
        );
    }

    #[test]
    fn image_candidates_prefer_thumbs_then_favicons() {
        let hits = vec![SearchHit {
            title: "Alpha".into(),
            url: "https://example.com/alpha".into(),
            snippet: "snip".into(),
            image: Some("https://example.com/a.jpg".into()),
        }];
        let candidates = image_candidates_for_hits(&hits);
        assert!(!candidates.is_empty());
        assert_eq!(candidates[0].0, "https://example.com/a.jpg");
        // Banner candidates exclude favicons so tiny icons never blow up
        // into blurry full-width banners.
        let photos = photo_candidates_for_hits(&hits);
        assert_eq!(photos.len(), 1);
        assert_eq!(photos[0].0, "https://example.com/a.jpg");
        let icon_only = vec![SearchHit {
            title: "Beta".into(),
            url: "https://example.com/beta".into(),
            snippet: "snip".into(),
            image: None,
        }];
        assert!(photo_candidates_for_hits(&icon_only).is_empty());
        // Favicon derived for same host is still allowlisted for validation.
        let allowed = expanded_allowed_urls(&hits);
        assert!(allowed.contains("https://example.com/alpha"));
        assert!(allowed.contains("https://example.com/a.jpg"));
        assert!(allowed.iter().any(|u| u.contains("icons.duckduckgo.com")));
    }

    #[test]
    fn rerank_prefers_title_matches_and_truncates() {
        let mut hits = vec![
            SearchHit {
                title: "Unrelated cooking".into(),
                url: "https://example.com/a".into(),
                snippet: "pasta recipe".into(),
                image: None,
            },
            SearchHit {
                title: "Osaka ramen guide".into(),
                url: "https://example.com/b".into(),
                snippet: "best ramens in osaka".into(),
                image: None,
            },
        ];
        rerank_hits("osaka ramen", &mut hits);
        assert_eq!(hits[0].url, "https://example.com/b");
        let truncated = truncate_hits(hits, 1);
        assert_eq!(truncated.len(), 1);
    }
}
