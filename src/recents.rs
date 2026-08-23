use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

pub const CAP: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentEntry {
    pub path: String,
    pub title: String,
    pub modified: u64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn recents_file() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/Carousel/recents.json"))
}

pub fn upsert(mut list: Vec<RecentEntry>, entry: RecentEntry, cap: usize) -> Vec<RecentEntry> {
    list.retain(|e| e.path != entry.path);
    list.insert(0, entry);
    list.sort_by_key(|a| std::cmp::Reverse(a.modified));
    list.truncate(cap);
    list
}

fn load_from(path: &Path) -> Vec<RecentEntry> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn save_to(path: &Path, list: &[RecentEntry]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json: String = serde_json::to_string_pretty(list).unwrap_or_else(|_| "[]".to_string());
    std::fs::write(path, json)
}

#[allow(dead_code)]
pub fn load() -> Vec<RecentEntry> {
    match recents_file() {
        Some(p) => load_from(&p),
        None => Vec::new(),
    }
}

pub fn forget(path: &str) {
    let file: PathBuf = match recents_file() {
        Some(f) => f,
        None => return,
    };
    let mut list: Vec<RecentEntry> = load_from(&file);
    let before: usize = list.len();
    list.retain(|e| e.path != path);
    if list.len() == before {
        return;
    }
    if let Err(e) = save_to(&file, &list) {
        warn!("recents: forget write failed: {}", e);
    }
}

pub fn drop_missing(list: Vec<RecentEntry>) -> Vec<RecentEntry> {
    list.into_iter()
        .filter(|e| Path::new(&e.path).exists())
        .collect()
}

pub fn load_existing() -> Vec<RecentEntry> {
    let file: PathBuf = match recents_file() {
        Some(f) => f,
        None => return Vec::new(),
    };
    let list: Vec<RecentEntry> = load_from(&file);
    let before: usize = list.len();
    let kept: Vec<RecentEntry> = drop_missing(list);
    if kept.len() != before
        && let Err(e) = save_to(&file, &kept)
    {
        warn!("recents: prune write failed: {}", e);
    }
    kept
}

pub fn record(path: &Path, title: &str) {
    let file: PathBuf = match recents_file() {
        Some(f) => f,
        None => return,
    };
    let entry = RecentEntry {
        path: path.to_string_lossy().to_string(),
        title: title.to_string(),
        modified: now_secs(),
    };
    let list: Vec<RecentEntry> = upsert(load_from(&file), entry, CAP);
    if let Err(e) = save_to(&file, &list) {
        warn!("recents: write failed: {}", e);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn entry(path: &str, modified: u64) -> RecentEntry {
        RecentEntry {
            path: path.into(),
            title: path.into(),
            modified,
        }
    }

    #[test]
    fn upsert_dedupes_by_path_newest_first() {
        let mut list = vec![entry("/a", 10), entry("/b", 20)];
        list = upsert(list, entry("/a", 30), 12);
        assert_eq!(list.iter().filter(|e| e.path == "/a").count(), 1);
        assert_eq!(list[0].path, "/a");
        assert_eq!(list[1].path, "/b");
    }

    #[test]
    fn upsert_caps_length() {
        let mut list = Vec::new();
        for i in 0..20u64 {
            list = upsert(list, entry(&format!("/d{i}"), i), 12);
        }
        assert_eq!(list.len(), 12);
        assert_eq!(list[0].path, "/d19");
        assert!(!list.iter().any(|e| e.path == "/d0"));
    }

    #[test]
    fn drop_missing_keeps_only_paths_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("here.slidedeck");
        std::fs::write(&real, b"x").unwrap();
        let list = vec![
            entry(&real.to_string_lossy(), 1),
            entry("/definitely/not/here.slidedeck", 2),
        ];
        let kept = drop_missing(list);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].path, real.to_string_lossy());
    }

    #[test]
    fn list_serde_roundtrips() {
        let list = vec![entry("/x", 1), entry("/y", 2)];
        let json = serde_json::to_string(&list).unwrap();
        let back: Vec<RecentEntry> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, list);
    }

    #[test]
    fn disk_roundtrip_through_temp_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub/recents.json");
        assert!(load_from(&file).is_empty());
        let list = upsert(load_from(&file), entry("/deck.slidedeck", 5), 12);
        save_to(&file, &list).unwrap();
        let back = load_from(&file);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].path, "/deck.slidedeck");
    }
}
