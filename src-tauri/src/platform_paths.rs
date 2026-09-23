use std::path::PathBuf;

/// Persistent user data. Keep the Windows location stable for existing installs.
pub fn data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home_dir()
            .join("Library")
            .join("Application Support")
            .join("app.pronto.dictation")
    }
    #[cfg(windows)]
    {
        return std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("Pronto");
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".local/share"))
            .join("app.pronto.dictation")
    }
}

pub fn log_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home_dir().join("Library/Logs/app.pronto.dictation")
    }
    #[cfg(not(target_os = "macos"))]
    {
        data_dir()
    }
}

#[cfg(not(windows))]
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_data_never_uses_temporary_storage_with_home() {
        #[cfg(target_os = "macos")]
        assert!(data_dir().ends_with("Library/Application Support/app.pronto.dictation"));
        #[cfg(windows)]
        assert!(data_dir().ends_with("Pronto"));
    }
}
