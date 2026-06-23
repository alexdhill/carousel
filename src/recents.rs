// Recents store.
//
// A tiny JSON list of recently saved/opened decks for the landing page's top
// row. Recording is best-effort and never blocks a save: failures log and are
// dropped. The pure `upsert` (dedupe by path, newest-first, capped) holds the
// only non-trivial logic and is unit-tested; disk read/write is thin.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

// Most-recent decks kept; older entries fall off the end.
const CAP: usize = 12;

// RecentEntry
// One recently used deck: absolute bundle path, display title (file stem), and
// the unix-seconds timestamp of the last save/open.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentEntry {
    pub path: String,
    pub title: String,
    pub modified: u64,
}

// now_secs
// Output: the current unix time in seconds (0 if the clock predates the epoch).
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// recents_file
// Output: the recents.json path under the macOS app-support directory, or None
// when $HOME is unset. The parent directory is created lazily on write.
pub fn recents_file() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library/Application Support/Carousel/recents.json"),
    )
}

// upsert
// Inputs: the current list, a new entry, the cap. Output: the list with any
// same-path entry removed, the new one prepended, re-sorted newest-first, and
// truncated to `cap`. Pure.
pub fn upsert(mut list: Vec<RecentEntry>, entry: RecentEntry, cap: usize) -> Vec<RecentEntry> {
    list.retain(|e| e.path != entry.path);
    list.insert(0, entry);
    list.sort_by(|a, b| b.modified.cmp(&a.modified));
    list.truncate(cap);
    list
}

// load_from / save_to
// Disk primitives split out so tests can drive them against a temp path.
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

// load
// Output: the recents list (newest first), or empty when absent/unreadable.
// Consumed by the landing window (sub-project 3); allow until then.
#[allow(dead_code)]
pub fn load() -> Vec<RecentEntry> {
    match recents_file() {
        Some(p) => load_from(&p),
        None => Vec::new(),
    }
}

// record
// Inputs: a bundle path and its display title. Output: side-effect; upserts the
// entry into the on-disk list with a fresh timestamp. Best-effort — any IO
// error is logged and dropped so a save is never blocked.
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
        RecentEntry { path: path.into(), title: path.into(), modified }
    }

    #[test]
    fn upsert_dedupes_by_path_newest_first() {
        let mut list = vec![entry("/a", 10), entry("/b", 20)];
        list = upsert(list, entry("/a", 30), 12);
        assert_eq!(list.iter().filter(|e| e.path == "/a").count(), 1);
        assert_eq!(list[0].path, "/a"); // newest
        assert_eq!(list[1].path, "/b");
    }

    #[test]
    fn upsert_caps_length() {
        let mut list = Vec::new();
        for i in 0..20u64 {
            list = upsert(list, entry(&format!("/d{i}"), i), 12);
        }
        assert_eq!(list.len(), 12);
        assert_eq!(list[0].path, "/d19"); // newest survives
        assert!(!list.iter().any(|e| e.path == "/d0")); // oldest dropped
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
