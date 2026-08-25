use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDef {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// Which chrome the editor and landing windows paint themselves in. `System`
/// defers to the OS setting and is resolved in the webview, not here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Light,
    Dark,
    #[default]
    System,
}

impl Appearance {
    pub fn as_str(self) -> &'static str {
        match self {
            Appearance::Light => "light",
            Appearance::Dark => "dark",
            Appearance::System => "system",
        }
    }

    /// Parses a mode name coming from the webview. Unknown names are rejected
    /// rather than defaulted, so a malformed message never silently rewrites
    /// the saved preference.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "light" => Some(Appearance::Light),
            "dark" => Some(Appearance::Dark),
            "system" => Some(Appearance::System),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub chrome_path: Option<PathBuf>,
    #[serde(default)]
    pub chromium_revision: Option<String>,
    #[serde(default)]
    pub agents: Vec<AgentDef>,
    #[serde(default)]
    pub appearance: Appearance,
}

fn config_path() -> PathBuf {
    app_data_dir().join("config.json")
}

pub fn load() -> Config {
    let path: PathBuf = config_path();
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

pub fn save(cfg: &Config) -> std::io::Result<()> {
    let dir: PathBuf = app_data_dir();
    std::fs::create_dir_all(&dir)?;
    let json: String = serde_json::to_string_pretty(cfg).unwrap_or_else(|_| "{}".to_string());
    std::fs::write(config_path(), json)
}

pub fn agent_names(cfg: &Config) -> Vec<String> {
    cfg.agents.iter().map(|a| a.name.clone()).collect()
}

pub fn find_agent<'a>(cfg: &'a Config, name: &str) -> Option<&'a AgentDef> {
    assert!(!name.is_empty(), "find_agent called with empty name");
    cfg.agents.iter().find(|a| a.name == name)
}

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
    fn appearance_defaults_to_system_and_rejects_junk() {
        let back: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(back.appearance, Appearance::System);
        assert_eq!(Appearance::parse("dark"), Some(Appearance::Dark));
        assert_eq!(Appearance::parse("Dark"), None);
        assert_eq!(Appearance::parse(""), None);
        for mode in [Appearance::Light, Appearance::Dark, Appearance::System] {
            assert_eq!(Appearance::parse(mode.as_str()), Some(mode));
        }
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
