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

// AgentDef
// One user-configured agent the chat panel can spawn. `name` is the label the
// panel's dropdown shows and the key the prompt selects by; `command` is the
// spawnable ACP binary path/name; `args` are extra arguments. Users add one
// entry per agent under `agents` in config.json.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDef {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

// Config
// Persisted in app_data_dir()/config.json. `chrome_path` is the resolved
// browser binary; `chromium_revision` records a downloaded build so we can
// detect/upgrade it later. Both optional (a fresh install has neither).
// `agents` is the list of user-configured ACP agents the chat panel offers in
// its dropdown; empty by default (a fresh install has none).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub chrome_path: Option<PathBuf>,
    #[serde(default)]
    pub chromium_revision: Option<String>,
    #[serde(default)]
    pub agents: Vec<AgentDef>,
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

// agent_names
// Input: cfg is a Config reference. Output: the display names of every
// configured agent, in config order. Empty when none are configured.
pub fn agent_names(cfg: &Config) -> Vec<String> {
    cfg.agents.iter().map(|a| a.name.clone()).collect()
}

// find_agent
// Input: cfg and a display name. Output: the matching AgentDef, or None when
// no agent carries that name.
pub fn find_agent<'a>(cfg: &'a Config, name: &str) -> Option<&'a AgentDef> {
    assert!(!name.is_empty(), "find_agent called with empty name");
    cfg.agents.iter().find(|a| a.name == name)
}

// cargo_bin
// Input: a binary base name. Output: the path to that binary under the Cargo
// bin directory (CARGO_HOME/bin or ~/.cargo/bin) when it exists, else None.
// Checks the bare name and the `.exe` variant for Windows.
fn cargo_bin(name: &str) -> Option<PathBuf> {
    assert!(!name.is_empty(), "cargo_bin called with empty name");
    let base: PathBuf = match std::env::var_os("CARGO_HOME") {
        Some(h) => PathBuf::from(h),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".cargo"),
    };
    let bin_dir: PathBuf = base.join("bin");
    for candidate_name in [name.to_string(), format!("{}.exe", name)] {
        let candidate: PathBuf = bin_dir.join(&candidate_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// detect_default_agent
// Output: a ready-to-use AgentDef for the `claude-code-acp-rs` binary when it
// is installed under the Cargo bin dir, else None. Used to seed a first agent
// so `cargo install claude-code-acp-rs` works without any manual config.
pub fn detect_default_agent() -> Option<AgentDef> {
    let bin: PathBuf = cargo_bin("claude-code-acp-rs")?;
    Some(AgentDef {
        name: "Claude Code".to_string(),
        command: bin.to_string_lossy().to_string(),
        args: Vec::new(),
    })
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
            ..Config::default()
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

    #[test]
    fn agents_default_empty_and_lookup() {
        let back: Config = serde_json::from_str("{}").unwrap();
        assert!(back.agents.is_empty());
        assert!(agent_names(&back).is_empty());

        let configured: Config = serde_json::from_str(
            r#"{"agents":[{"name":"Claude","command":"claude-code-acp","args":["--x"]}]}"#,
        )
        .unwrap();
        assert_eq!(agent_names(&configured), vec!["Claude".to_string()]);
        let found = find_agent(&configured, "Claude").unwrap();
        assert_eq!(found.command, "claude-code-acp");
        assert_eq!(found.args, vec!["--x".to_string()]);
        assert!(find_agent(&configured, "Missing").is_none());
    }
}
