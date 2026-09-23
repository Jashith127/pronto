//! Local-only design-system resource for voice search UI synthesis.
//!
//! Exposed in-process as `design://system/v1`. There is no remote MCP server
//! and no sidecar in the MVP — callers read the bundled catalog (or an
//! optional `%LOCALAPPDATA%/Pronto/design-system.json` override).

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const DESIGN_SYSTEM_URI: &str = "design://system/v1";
const BUNDLED_DESIGN_SYSTEM: &str = include_str!("../resources/design-system.json");

static CACHED: OnceLock<Value> = OnceLock::new();

pub fn design_system_json(resource_dir: Option<&Path>) -> Result<&'static Value, String> {
    if let Some(value) = CACHED.get() {
        return Ok(value);
    }
    let value = load_design_system(resource_dir)?;
    let _ = CACHED.set(value);
    CACHED
        .get()
        .ok_or_else(|| "design system cache unavailable".into())
}

pub fn design_system_catalog_text(resource_dir: Option<&Path>) -> Result<String, String> {
    let value = design_system_json(resource_dir)?;
    serde_json::to_string_pretty(value).map_err(|error| error.to_string())
}

/// Compact single-line catalog for LLM prompts. Same content as
/// `design_system_catalog_text`, ~30-40% fewer tokens per search call.
pub fn design_system_catalog_minified(resource_dir: Option<&Path>) -> Result<String, String> {
    // Minified form is derived from the same cached JSON value, so overrides
    // and bundling behavior stay identical — only whitespace differs.
    static MINIFIED: OnceLock<String> = OnceLock::new();
    // Only cache the default (no resource_dir) variant; custom dirs bypass.
    if resource_dir.is_none() {
        if let Some(cached) = MINIFIED.get() {
            return Ok(cached.clone());
        }
    }
    let value = design_system_json(resource_dir)?;
    let minified = serde_json::to_string(value).map_err(|error| error.to_string())?;
    if resource_dir.is_none() {
        let _ = MINIFIED.set(minified.clone());
    }
    Ok(minified)
}

fn load_design_system(resource_dir: Option<&Path>) -> Result<Value, String> {
    if let Some(override_path) = local_override_path() {
        if override_path.is_file() {
            let bytes = fs::read(&override_path).map_err(|error| {
                format!(
                    "Could not read design-system override {}: {error}",
                    override_path.display()
                )
            })?;
            return serde_json::from_slice(&bytes).map_err(|error| {
                format!(
                    "Invalid design-system override {}: {error}",
                    override_path.display()
                )
            });
        }
    }
    if let Some(dir) = resource_dir {
        let bundled = dir.join("resources").join("design-system.json");
        if bundled.is_file() {
            let bytes = fs::read(&bundled).map_err(|error| error.to_string())?;
            return serde_json::from_slice(&bytes).map_err(|error| error.to_string());
        }
        let alt = dir.join("design-system.json");
        if alt.is_file() {
            let bytes = fs::read(&alt).map_err(|error| error.to_string())?;
            return serde_json::from_slice(&bytes).map_err(|error| error.to_string());
        }
    }
    serde_json::from_str(BUNDLED_DESIGN_SYSTEM)
        .map_err(|error| format!("Bundled design-system.json is invalid: {error}"))
}

fn local_override_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|dir| dir.join("Pronto").join("design-system.json"))
    }
    #[cfg(not(windows))]
    {
        Some(crate::platform_paths::data_dir().join("design-system.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_design_system_exposes_v1_uri_and_components() {
        let value = load_design_system(None).unwrap();
        assert_eq!(
            value.get("uri").and_then(|v| v.as_str()),
            Some(DESIGN_SYSTEM_URI)
        );
        let components = value
            .get("components")
            .and_then(|v| v.as_array())
            .expect("components array");
        assert!(components
            .iter()
            .any(|component| { component.get("type").and_then(|v| v.as_str()) == Some("chart") }));
        assert!(components.iter().any(|component| {
            component.get("type").and_then(|v| v.as_str()) == Some("source_list")
        }));
    }
}
