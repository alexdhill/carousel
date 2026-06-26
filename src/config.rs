// Application config + data directory.
//
// The app keeps a small JSON config and any downloaded Chromium outside the
// (read-only, code-signed) program bundle, in the per-OS user data dir. The
// directory is computed from environment variables so no `dirs`-style crate is
// needed.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// app_data_dir
// Output: the carousel data dir for this OS (created lazily by callers when
// they write into it). macOS: ~/Library/Application Support/carousel; Windows:
// %LOCALAPPDATA%\carousel; Linux: $XDG_DATA_HOME or ~/.local/share/carousel.
pub fn app_data_dir() -> PathBuf {
    let base: PathBuf = base_data_dir();
    base.join("carousel")
}

#[cfg(target_os = "macos")]
fn base_data_dir() -> PathBuf {
    let home: String = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join("Library")
        .join("Application Support")
}

#[cfg(target_os = "windows")]
fn base_data_dir() -> PathBuf {
    let local: String = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(local)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn base_data_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg);
    }
    let home: String = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".local").join("share")
}

// Config
// Persisted in app_data_dir()/config.json. `chrome_path` is the resolved
// browser binary; `chromium_revision` records a downloaded build so we can
// detect/upgrade it later. Both optional (a fresh install has neither).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub chrome_path: Option<PathBuf>,
    #[serde(default)]
    pub chromium_revision: Option<String>,
}

fn config_path() -> PathBuf {
    app_data_dir().join("config.json")
}

// load
// Output: the saved Config, or Config::default() when the file is absent or
// unparseable (a corrupt config must never block startup).
pub fn load() -> Config {
    let path: PathBuf = config_path();
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

// save
// Inputs: the Config to persist. Output: Ok after creating the data dir and
// writing config.json. Errors: filesystem failures.
pub fn save(cfg: &Config) -> std::io::Result<()> {
    let dir: PathBuf = app_data_dir();
    std::fs::create_dir_all(&dir)?;
    let json: String = serde_json::to_string_pretty(cfg).unwrap_or_else(|_| "{}".to_string());
    std::fs::write(config_path(), json)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn app_data_dir_ends_with_carousel() {
        let dir = app_data_dir();
        assert!(dir.ends_with("carousel"), "got {dir:?}");
    }

    #[test]
    fn config_json_roundtrips() {
        let cfg = Config {
            chrome_path: Some(PathBuf::from("/usr/bin/chrome")),
            chromium_revision: Some("1300313".into()),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn config_defaults_when_fields_missing() {
        let back: Config = serde_json::from_str("{}").unwrap();
        assert!(back.chrome_path.is_none());
        assert!(back.chromium_revision.is_none());
    }
}
