use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

pub const MAX_UI_BYTES: usize = 64 * 1024;
pub const MAX_UI_NODES: usize = 24;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchUiDocument {
    pub nodes: Vec<UiNode>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum UiNode {
    Heading {
        text: String,
    },
    Text {
        text: String,
    },
    Divider,
    ImageFrame {
        src: String,
        alt: String,
        #[serde(default)]
        caption: Option<String>,
    },
    Youtube {
        url: String,
        #[serde(default)]
        title: Option<String>,
    },
    Button {
        label: String,
        action: ButtonAction,
        value: String,
    },
    SourceList {
        items: Vec<SourceItem>,
    },
    Table {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Chart {
        chart_type: ChartType,
        labels: Vec<String>,
        datasets: Vec<ChartDataset>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum ButtonAction {
    OpenUrl,
    Copy,
    Insert,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum ChartType {
    Bar,
    Line,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ChartDataset {
    pub label: String,
    pub data: Vec<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct SourceItem {
    pub index: u32,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub snippet: Option<String>,
}

pub fn parse_and_validate_ui(
    raw: &str,
    allowed_urls: &HashSet<String>,
) -> Result<SearchUiDocument, String> {
    if raw.len() > MAX_UI_BYTES {
        return Err(format!(
            "Search UI JSON exceeds the {MAX_UI_BYTES}-byte limit"
        ));
    }
    if raw_contains_disallowed_markup(raw) {
        return Err("Search UI must not contain raw HTML or JavaScript".into());
    }
    let document = decode_document(raw)?;
    validate_document(&document, allowed_urls)?;
    Ok(document)
}

fn decode_document(raw: &str) -> Result<SearchUiDocument, String> {
    if let Ok(document) = serde_json::from_str::<SearchUiDocument>(raw) {
        return Ok(document);
    }
    let value: Value =
        serde_json::from_str(raw).map_err(|error| format!("Invalid search UI JSON: {error}"))?;
    if let Some(nodes) = value.get("nodes") {
        let nodes: Vec<UiNode> = serde_json::from_value(nodes.clone())
            .map_err(|error| format!("Invalid search UI nodes: {error}"))?;
        return Ok(SearchUiDocument { nodes });
    }
    if value.is_array() {
        let nodes: Vec<UiNode> = serde_json::from_value(value)
            .map_err(|error| format!("Invalid search UI nodes: {error}"))?;
        return Ok(SearchUiDocument { nodes });
    }
    Err("Search UI JSON must include a nodes array".into())
}

fn raw_contains_disallowed_markup(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    lower.contains("<script")
        || lower.contains("</script")
        || lower.contains("javascript:")
        || lower.contains("<iframe")
        || lower.contains("<img")
        || lower.contains("<a ")
        || lower.contains("<div")
}

pub fn validate_document(
    document: &SearchUiDocument,
    allowed_urls: &HashSet<String>,
) -> Result<(), String> {
    if document.nodes.is_empty() {
        return Err("Search UI must contain at least one node".into());
    }
    if document.nodes.len() > MAX_UI_NODES {
        return Err(format!(
            "Search UI may contain at most {MAX_UI_NODES} nodes"
        ));
    }
    for node in &document.nodes {
        validate_node(node, allowed_urls)?;
    }
    Ok(())
}

fn validate_node(node: &UiNode, allowed_urls: &HashSet<String>) -> Result<(), String> {
    match node {
        UiNode::Heading { text } | UiNode::Text { text } => {
            if text.trim().is_empty() {
                return Err("heading/text nodes require non-empty text".into());
            }
            if looks_like_html(text) {
                return Err("UI text must not include HTML markup".into());
            }
            Ok(())
        }
        UiNode::Divider => Ok(()),
        UiNode::ImageFrame { src, alt, .. } => {
            require_allowed_image_url(src, allowed_urls, "image_frame.src")?;
            if alt.trim().is_empty() {
                return Err("image_frame requires alt text".into());
            }
            Ok(())
        }
        UiNode::Youtube { url, .. } => {
            require_allowed_url(url, allowed_urls, "youtube.url")?;
            if !is_youtube_url(url) {
                return Err("youtube.url must be a youtube.com or youtu.be link".into());
            }
            Ok(())
        }
        UiNode::Button {
            label,
            action,
            value,
        } => {
            if label.trim().is_empty() {
                return Err("button.label is required".into());
            }
            match action {
                ButtonAction::OpenUrl => require_allowed_url(value, allowed_urls, "button.value"),
                ButtonAction::Copy | ButtonAction::Insert => {
                    if value.is_empty() {
                        return Err("button.value is required".into());
                    }
                    if looks_like_html(value) {
                        return Err("button.value must not include HTML markup".into());
                    }
                    Ok(())
                }
            }
        }
        UiNode::SourceList { items } => {
            if items.is_empty() {
                return Err("source_list requires at least one item".into());
            }
            for item in items {
                require_allowed_url(&item.url, allowed_urls, "source_list.url")?;
                if item.title.trim().is_empty() {
                    return Err("source_list items require a title".into());
                }
            }
            Ok(())
        }
        UiNode::Table { columns, rows } => {
            if columns.is_empty() {
                return Err("table.columns must not be empty".into());
            }
            for row in rows {
                if row.len() != columns.len() {
                    return Err("table rows must match column count".into());
                }
                for cell in row {
                    if looks_like_html(cell) {
                        return Err("table cells must not include HTML markup".into());
                    }
                }
            }
            Ok(())
        }
        UiNode::Chart {
            labels, datasets, ..
        } => {
            if labels.is_empty() || datasets.is_empty() {
                return Err("chart requires labels and datasets".into());
            }
            for dataset in datasets {
                if dataset.data.len() != labels.len() {
                    return Err("chart dataset length must match labels".into());
                }
            }
            Ok(())
        }
    }
}

fn require_allowed_url(
    url: &str,
    allowed_urls: &HashSet<String>,
    field: &str,
) -> Result<(), String> {
    if !allowed_urls.contains(url) {
        return Err(format!(
            "{field} must exactly match a search-result URL (got {url})"
        ));
    }
    Ok(())
}

/// Images use a wider allowlist than links: exact result/thumbnail URLs plus
/// host-derived favicons. Favicons are derived locally (no backend fetch) so
/// every answer can include a visual without extra network or LLM cost, while
/// still never allowing arbitrary invented hosts.
fn require_allowed_image_url(
    url: &str,
    allowed_urls: &HashSet<String>,
    field: &str,
) -> Result<(), String> {
    if allowed_urls.contains(url) {
        return Ok(());
    }
    if is_derived_favicon(url, allowed_urls) {
        return Ok(());
    }
    // Direct image files on an already-allowed host (e.g. extracted og:image
    // or <img> thumbs that share the article host) stay grounded without
    // requiring the caller to pre-register every variant.
    if is_same_host_image(url, allowed_urls) {
        return Ok(());
    }
    Err(format!(
        "{field} must exactly match a search-result or favicon URL (got {url})"
    ))
}

/// `https://icons.duckduckgo.com/ip3/<host>.ico` for any allowed host.
pub fn favicon_for_host(host: &str) -> Option<String> {
    let host = host.trim().trim_start_matches("www.").to_lowercase();
    if host.is_empty() || host.contains([' ', '/', '?', '#']) || !host.contains('.') {
        return None;
    }
    Some(format!("https://icons.duckduckgo.com/ip3/{host}.ico"))
}

pub fn host_of_url(url: &str) -> Option<String> {
    url.split_once("://").and_then(|(_, rest)| {
        let authority = rest.split('/').next().unwrap_or(rest);
        let hostname = authority.split('@').next_back().unwrap_or(authority);
        let hostname = hostname.split(':').next().unwrap_or(hostname);
        let hostname = hostname.trim().trim_end_matches('.').to_lowercase();
        if hostname.is_empty() || !hostname.contains('.') {
            return None;
        }
        Some(hostname)
    })
}

pub fn favicon_for_url(url: &str) -> Option<String> {
    host_of_url(url).and_then(|host| favicon_for_host(&host))
}

fn is_derived_favicon(url: &str, allowed_urls: &HashSet<String>) -> bool {
    if !(url.starts_with("https://icons.duckduckgo.com/ip3/")
        && url.ends_with(".ico"))
    {
        return false;
    }
    let host = url
        .strip_prefix("https://icons.duckduckgo.com/ip3/")
        .and_then(|rest| rest.strip_suffix(".ico"))
        .unwrap_or("");
    if host.is_empty() {
        return false;
    }
    allowed_urls.iter().any(|allowed| {
        host_of_url(allowed).is_some_and(|allowed_host| {
            allowed_host == host
                || allowed_host == format!("www.{host}")
                || host == format!("www.{allowed_host}")
                || allowed_host.trim_start_matches("www.") == host.trim_start_matches("www.")
        })
    })
}

fn is_same_host_image(url: &str, allowed_urls: &HashSet<String>) -> bool {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return false;
    }
    // Only image files — never HTML pages masquerading as images.
    let is_image = lower.split(['?', '#']).next().is_some_and(|path| {
        path.ends_with(".png")
            || path.ends_with(".jpg")
            || path.ends_with(".jpeg")
            || path.ends_with(".webp")
            || path.ends_with(".gif")
            || path.ends_with(".avif")
            || path.ends_with(".ico")
    });
    if !is_image {
        return false;
    }
    let Some(image_host) = host_of_url(url) else {
        return false;
    };
    allowed_urls.iter().any(|allowed| {
        host_of_url(allowed).is_some_and(|allowed_host| {
            allowed_host.eq_ignore_ascii_case(&image_host)
        })
    })
}

pub fn is_youtube_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    host_is(&lower, "youtube.com")
        || host_is(&lower, "www.youtube.com")
        || host_is(&lower, "m.youtube.com")
        || host_is(&lower, "youtu.be")
        || host_is(&lower, "www.youtu.be")
}

fn host_is(url: &str, host: &str) -> bool {
    url.split_once("://")
        .map(|(_, rest)| {
            let authority = rest.split('/').next().unwrap_or(rest);
            let hostname = authority.split('@').next_back().unwrap_or(authority);
            let hostname = hostname.split(':').next().unwrap_or(hostname);
            hostname.eq_ignore_ascii_case(host)
        })
        .unwrap_or(false)
}

pub fn youtube_nocookie_embed(url: &str) -> Option<String> {
    let id = youtube_video_id(url)?;
    Some(format!("https://www.youtube-nocookie.com/embed/{id}"))
}

fn youtube_video_id(url: &str) -> Option<String> {
    if let Some(rest) = url
        .split("youtu.be/")
        .nth(1)
        .map(|part| part.split(['?', '&', '#']).next().unwrap_or(part))
    {
        if !rest.is_empty() && !rest.contains('/') {
            return Some(rest.to_string());
        }
    }
    for marker in ["v=", "/embed/", "/shorts/"] {
        if let Some(idx) = url.find(marker) {
            let start = idx + marker.len();
            let id = url[start..]
                .split(['?', '&', '#', '/'])
                .next()
                .unwrap_or("");
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

fn looks_like_html(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains('<')
        && (lower.contains("</")
            || lower.contains("<script")
            || lower.contains("<img")
            || lower.contains("<a ")
            || lower.contains("<div")
            || lower.contains("<iframe"))
}

#[allow(dead_code)]
pub fn fallback_document(query: &str, sources: &[(u32, String, String, String)]) -> SearchUiDocument {
    fallback_document_with_images(query, sources, &[])
}

/// Fast-path / no-key fallback. `images` must be real photos only (never
/// favicons — a 16px icon blown up to a banner is what produced the blurry
/// flag). Shows the top snippet as a quoted answer preview with a citation
/// so the card never says just "check below for sources" with no answer.
pub fn fallback_document_with_images(
    _query: &str,
    sources: &[(u32, String, String, String)],
    images: &[(String, String)],
) -> SearchUiDocument {
    // Query is shown once in the overlay chrome — no duplicate heading here.
    let mut nodes = Vec::new();
    if let Some((src, alt)) = images.first() {
        nodes.push(UiNode::ImageFrame {
            src: src.clone(),
            alt: alt.clone(),
            caption: None,
        });
    }
    if let Some((index, title, _, snippet)) = sources.first() {
        // Extractive preview: title + snippet is usually the direct answer
        // for who/what queries when the LLM is skipped or unavailable.
        let preview = if snippet.trim().is_empty() {
            title.trim().to_string()
        } else if title.trim().is_empty() {
            snippet.trim().to_string()
        } else {
            format!("{} — {}", title.trim(), snippet.trim())
        };
        if !preview.is_empty() {
            nodes.push(UiNode::Text {
                text: format!("{preview} [{index}]"),
            });
        }
    }
    if sources.is_empty() {
        nodes.push(UiNode::Text {
            text: "No evidence found in the retrieved sources.".into(),
        });
    }
    if !sources.is_empty() {
        nodes.push(UiNode::SourceList {
            items: sources
                .iter()
                .map(|(index, title, url, snippet)| SourceItem {
                    index: *index,
                    title: title.clone(),
                    url: url.clone(),
                    snippet: (!snippet.is_empty()).then(|| snippet.clone()),
                })
                .collect(),
        });
    }
    SearchUiDocument { nodes }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(urls: &[&str]) -> HashSet<String> {
        urls.iter().map(|url| (*url).to_string()).collect()
    }

    #[test]
    fn accepts_grounded_source_list_document() {
        let url = "https://example.com/a";
        let raw = format!(
            r#"{{"nodes":[{{"type":"heading","text":"Answer"}},{{"type":"text","text":"See [1]."}},{{"type":"source_list","items":[{{"index":1,"title":"A","url":"{url}","snippet":"snip"}}]}}]}}"#
        );
        let document = parse_and_validate_ui(&raw, &allowed(&[url])).unwrap();
        assert_eq!(document.nodes.len(), 3);
    }

    #[test]
    fn rejects_raw_html_and_unknown_types() {
        let allowed_urls = allowed(&["https://example.com"]);
        assert!(parse_and_validate_ui(
            r#"{"nodes":[{"type":"text","text":"<script>alert(1)</script>"}]}"#,
            &allowed_urls
        )
        .is_err());
        assert!(parse_and_validate_ui(
            r#"{"nodes":[{"type":"markdown","text":"hi"}]}"#,
            &allowed_urls
        )
        .is_err());
    }

    #[test]
    fn rejects_non_source_urls() {
        let allowed_urls = allowed(&["https://example.com/ok"]);
        let raw = r#"{"nodes":[{"type":"button","label":"Go","action":"open_url","value":"https://evil.example/x"}]}"#;
        assert!(parse_and_validate_ui(raw, &allowed_urls).is_err());
    }

    #[test]
    fn youtube_only_accepts_youtube_hosts() {
        let yt = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
        let allowed_urls = allowed(&[yt]);
        let raw = format!(r#"{{"nodes":[{{"type":"youtube","url":"{yt}","title":"Demo"}}]}}"#);
        assert!(parse_and_validate_ui(&raw, &allowed_urls).is_ok());
        assert_eq!(
            youtube_nocookie_embed(yt).as_deref(),
            Some("https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ")
        );
    }

    #[test]
    fn image_frame_accepts_derived_favicon() {
        let article = "https://example.com/alpha";
        let favicon = favicon_for_url(article).unwrap();
        let raw = format!(
            r#"{{"nodes":[{{"type":"heading","text":"A"}},{{"type":"image_frame","src":"{favicon}","alt":"Alpha"}}]}}"#
        );
        assert!(parse_and_validate_ui(&raw, &allowed(&[article])).is_ok());
    }

    #[test]
    fn image_frame_rejects_invented_hosts() {
        let allowed_urls = allowed(&["https://example.com/a"]);
        let raw = r#"{"nodes":[{"type":"heading","text":"A"},{"type":"image_frame","src":"https://evil.example/x.jpg","alt":"X"}]}"#;
        assert!(parse_and_validate_ui(raw, &allowed_urls).is_err());
    }

    #[test]
    fn fallback_with_images_includes_visual() {
        let sources = vec![(1, "A".to_string(), "https://example.com/a".to_string(), "snip".to_string())];
        let images = vec![("https://icons.duckduckgo.com/ip3/example.com.ico".to_string(), "A".to_string())];
        let doc = fallback_document_with_images("test", &sources, &images);
        assert!(doc.nodes.iter().any(|n| matches!(n, UiNode::ImageFrame { .. })));
        let plain = fallback_document("test", &sources);
        assert!(!plain.nodes.iter().any(|n| matches!(n, UiNode::ImageFrame { .. })));
    }
}
