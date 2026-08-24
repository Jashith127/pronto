use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const KEYRING_SERVICE: &str = "app.pronto.dictation";
const LEGACY_KEYRING_SERVICE: &str = "app.vela.dictation";
const KEYRING_ACCOUNT: &str = "deepseek-api-key";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct UserSettings {
    pub cleanup_enabled: bool,
    pub language: String,
    pub auto_insert: bool,
    pub dictionary: Vec<String>,
    pub hotkey: String,
    pub duck_audio: bool,
    pub activation_mode: ActivationMode,
    pub launch_at_startup: bool,
    pub microphone_id: Option<String>,
    pub microphone_name: Option<String>,
    pub gpu_memory_management: bool,
    pub dictation_sounds: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ActivationMode {
    #[default]
    Hold,
    Toggle,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            cleanup_enabled: true,
            language: "auto".into(),
            auto_insert: true,
            dictionary: Vec::new(),
            hotkey: "control+alt+Space".into(),
            duck_audio: false,
            activation_mode: ActivationMode::Hold,
            launch_at_startup: false,
            microphone_id: None,
            microphone_name: None,
            gpu_memory_management: true,
            dictation_sounds: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: u128,
    pub created_at_ms: u128,
    pub raw_text: String,
    pub final_text: String,
    pub asr_ms: u128,
    pub cleanup_ms: u128,
    pub total_ms: u128,
    #[serde(default)]
    pub audio_ms: u128,
    pub cleanup_applied: bool,
}

impl HistoryEntry {
    pub fn new(
        raw_text: String,
        final_text: String,
        asr_ms: u128,
        cleanup_ms: u128,
        total_ms: u128,
        audio_ms: u128,
        cleanup_applied: bool,
    ) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        Self {
            id: now,
            created_at_ms: now,
            raw_text,
            final_text,
            asr_ms,
            cleanup_ms,
            total_ms,
            audio_ms,
            cleanup_applied,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppPreferences {
    pub settings: UserSettings,
    pub api_key_configured: bool,
}

pub struct SettingsStore {
    settings: Mutex<UserSettings>,
    history: Mutex<Vec<HistoryEntry>>,
    data_dir: PathBuf,
}

impl SettingsStore {
    pub fn load() -> Self {
        let data_dir = data_dir();
        let legacy_dir = data_dir.with_file_name("Vela");
        if !data_dir.exists() && legacy_dir.is_dir() {
            let _ = fs::create_dir_all(&data_dir);
            for name in ["settings.json", "history.json"] {
                let source = legacy_dir.join(name);
                if source.is_file() {
                    let _ = fs::copy(source, data_dir.join(name));
                }
            }
        }
        let _ = fs::create_dir_all(&data_dir);
        let settings = read_json(data_dir.join("settings.json")).unwrap_or_default();
        let history = read_json(data_dir.join("history.json")).unwrap_or_default();
        Self {
            settings: Mutex::new(settings),
            history: Mutex::new(history),
            data_dir,
        }
    }

    pub fn preferences(&self) -> Result<AppPreferences, String> {
        Ok(AppPreferences {
            settings: self.snapshot()?,
            api_key_configured: deepseek_key().is_some(),
        })
    }

    pub fn snapshot(&self) -> Result<UserSettings, String> {
        self.settings
            .lock()
            .map(|settings| settings.clone())
            .map_err(|_| "settings lock poisoned".into())
    }

    pub fn replace(&self, mut next: UserSettings) -> Result<AppPreferences, String> {
        next.dictionary = normalize_dictionary(next.dictionary);
        *self.settings.lock().map_err(|_| "settings lock poisoned")? = next.clone();
        write_json(self.data_dir.join("settings.json"), &next)?;
        self.preferences()
    }

    pub fn add_dictionary_term(&self, term: String) -> Result<UserSettings, String> {
        let term = term.trim();
        if term.is_empty() || term.len() > 100 {
            return Err("Dictionary terms must contain 1–100 characters".into());
        }
        let mut settings = self.settings.lock().map_err(|_| "settings lock poisoned")?;
        if !settings
            .dictionary
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(term))
        {
            settings.dictionary.push(term.into());
            settings
                .dictionary
                .sort_by_key(|value| value.to_lowercase());
        }
        write_json(self.data_dir.join("settings.json"), &*settings)?;
        Ok(settings.clone())
    }

    pub fn remove_dictionary_term(&self, term: &str) -> Result<UserSettings, String> {
        let mut settings = self.settings.lock().map_err(|_| "settings lock poisoned")?;
        settings
            .dictionary
            .retain(|existing| !existing.eq_ignore_ascii_case(term));
        write_json(self.data_dir.join("settings.json"), &*settings)?;
        Ok(settings.clone())
    }

    pub fn history(&self) -> Result<Vec<HistoryEntry>, String> {
        self.history
            .lock()
            .map(|history| history.clone())
            .map_err(|_| "history lock poisoned".into())
    }

    pub fn push_history(&self, entry: HistoryEntry) -> Result<(), String> {
        let mut history = self.history.lock().map_err(|_| "history lock poisoned")?;
        history.insert(0, entry);
        history.truncate(100);
        write_json(self.data_dir.join("history.json"), &*history)
    }

    pub fn clear_history(&self) -> Result<(), String> {
        let mut history = self.history.lock().map_err(|_| "history lock poisoned")?;
        history.clear();
        write_json(self.data_dir.join("history.json"), &*history)
    }

    pub fn last_transcript(&self) -> Result<Option<String>, String> {
        self.history
            .lock()
            .map(|history| history.first().map(|entry| entry.final_text.clone()))
            .map_err(|_| "history lock poisoned".into())
    }

    pub fn transcript(&self, id: u128) -> Result<Option<String>, String> {
        self.history
            .lock()
            .map(|history| {
                history
                    .iter()
                    .find(|entry| entry.id == id)
                    .map(|entry| entry.final_text.clone())
            })
            .map_err(|_| "history lock poisoned".into())
    }
}

pub fn set_deepseek_key(api_key: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
        .map_err(|error| format!("Credential Manager unavailable: {error}"))?;
    if api_key.trim().is_empty() {
        match entry.delete_password() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(format!("Could not remove API key: {error}")),
        }
    } else {
        entry
            .set_password(api_key.trim())
            .map_err(|error| format!("Could not securely save API key: {error}"))
    }
}

pub fn deepseek_key() -> Option<String> {
    std::env::var("DEEPSEEK_API_KEY")
        .ok()
        .filter(|key| !key.is_empty())
        .or_else(|| {
            keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
                .ok()?
                .get_password()
                .ok()
        })
        .or_else(|| {
            keyring::Entry::new(LEGACY_KEYRING_SERVICE, KEYRING_ACCOUNT)
                .ok()?
                .get_password()
                .ok()
        })
}

fn data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Pronto")
}

fn normalize_dictionary(values: Vec<String>) -> Vec<String> {
    let mut output = Vec::new();
    for value in values {
        let value = value.trim();
        if !value.is_empty()
            && value.len() <= 100
            && !output
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(value))
        {
            output.push(value.to_string());
        }
    }
    output.sort_by_key(|value| value.to_lowercase());
    output
}

fn read_json<T: for<'de> Deserialize<'de>>(path: PathBuf) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn write_json(path: PathBuf, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    fs::write(path, bytes).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_is_trimmed_sorted_and_deduplicated() {
        let values = normalize_dictionary(vec![
            "  Pronto ".into(),
            "deepSeek".into(),
            "pronto".into(),
            "".into(),
        ]);
        assert_eq!(values, vec!["deepSeek", "Pronto"]);
    }

    #[test]
    fn older_settings_without_microphone_are_compatible() {
        let settings: UserSettings = serde_json::from_str(r#"{"language":"en"}"#).unwrap();
        assert_eq!(settings.language, "en");
        assert!(settings.microphone_id.is_none());
        assert!(settings.microphone_name.is_none());
        assert!(settings.gpu_memory_management);
        assert!(settings.dictation_sounds);
    }
}
