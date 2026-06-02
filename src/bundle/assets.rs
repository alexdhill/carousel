// AssetRegistry.
//
// SPEC §3.4 — `assets/index.json` records metadata for each binary asset
// in the deck so we can deduplicate by content hash and surface integrity
// info to the user. Stage 7's scope does not include asset-import commands,
// so this module is intentionally minimal: a typed wrapper that serializes
// to / from the JSON shape on disk and tracks zero-or-more entries.
//
// When the asset-import command set lands, this struct gains insertion /
// lookup methods. Until then, the registry is created empty for fresh
// decks and parsed back verbatim from an opened bundle.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub const ASSETS_INDEX_VERSION: &str = "1.0";
pub const ASSET_ID_HASH_LEN: usize = 16;
pub const ASSETS_IMAGES_DIR: &str = "assets/images";

// AssetEntry
// One row in assets/index.json. Carries enough metadata for dedup
// (content_hash), display (original_filename, dimensions), and re-mounting
// (path, media_type, size_bytes).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetEntry {
    pub id: String,
    pub path: String,
    pub content_hash: String,
    pub original_filename: String,
    pub media_type: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub dimensions: Option<AssetDimensions>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetDimensions {
    pub width: u32,
    pub height: u32,
}

// AssetRegistry
// Top-level shape of assets/index.json. Holds the registry version (for
// forward-compat) and a flat vec of entries. A separate `files` map is
// populated only at save/load time — the in-memory deck holds raw bytes
// in that map so the I/O thread can write or rebuild the bundle without
// re-reading anything from disk.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetRegistry {
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub assets: Vec<AssetEntry>,
    #[serde(skip)]
    pub files: HashMap<String, Vec<u8>>,
}

fn default_version() -> String {
    ASSETS_INDEX_VERSION.to_string()
}

impl AssetRegistry {
    // new_empty
    // Inputs: none.
    // Output: a registry with the current version and no entries.
    pub fn new_empty() -> Self {
        Self {
            version: ASSETS_INDEX_VERSION.to_string(),
            assets: Vec::new(),
            files: HashMap::new(),
        }
    }

    // is_empty
    // Inputs: self.
    // Output: true iff the registry holds no entries.
    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }

    // entry_count
    // Inputs: self.
    // Output: number of asset entries.
    pub fn entry_count(&self) -> usize {
        self.assets.len()
    }

    // index_json
    // Inputs: self.
    // Output: pretty-printed JSON of the on-disk `assets/index.json` shape
    // (version + assets). The `files` map is excluded via `#[serde(skip)]`.
    // Errors: serde_json failure (impossible in practice for owned data).
    pub fn index_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    // from_index_json
    // Inputs: a string containing the on-disk `assets/index.json` body.
    // Output: the parsed registry with an empty `files` map (the caller
    // populates `files` from the bundle's asset blobs separately).
    // Errors: serde_json failure on malformed input.
    pub fn from_index_json(s: &str) -> Result<Self, serde_json::Error> {
        assert!(!s.is_empty(), "from_index_json: empty input");
        let parsed: Self = serde_json::from_str(s)?;
        Ok(parsed)
    }

    // insert_blob
    // Inputs: the raw bytes, the user-facing filename (used for the
    // bundle path extension), the MIME media type, and the optional
    // pixel dimensions.
    // Output: the AssetEntry now backing this asset. If an entry with
    // the same content_hash already exists, returns that one verbatim
    // (deduplication — SPEC §3.4). Otherwise builds a new entry, stores
    // the bytes under `assets/images/<id>.<ext>`, and returns the new
    // entry.
    // Dataflow:
    //   1. sha256 the bytes → hex digest → asset id = "asset_<first-16>".
    //   2. Look up the digest in self.assets; on hit, return the cached
    //      entry without writing anything.
    //   3. On miss, derive the file extension from original_filename
    //      (or the media_type when the name has no extension), build
    //      the on-disk path, append the AssetEntry, and write the bytes
    //      into self.files.
    pub fn insert_blob(
        &mut self,
        bytes: Vec<u8>,
        original_filename: String,
        media_type: String,
        dimensions: Option<AssetDimensions>,
    ) -> AssetEntry {
        assert!(!bytes.is_empty(), "AssetRegistry::insert_blob: empty bytes");
        assert!(!media_type.is_empty(), "AssetRegistry::insert_blob: empty media_type");

        let hash_hex: String = sha256_hex(&bytes);
        let content_hash: String = format!("sha256:{hash_hex}");

        // Dedup pass — by content hash.
        let mut i: usize = 0;
        while i < self.assets.len() {
            if self.assets[i].content_hash == content_hash {
                return self.assets[i].clone();
            }
            i += 1;
        }

        let asset_id: String = derive_asset_id(&hash_hex);
        let ext: String = derive_extension(&original_filename, &media_type);
        let path: String = if ext.is_empty() {
            format!("{ASSETS_IMAGES_DIR}/{asset_id}")
        } else {
            format!("{ASSETS_IMAGES_DIR}/{asset_id}.{ext}")
        };
        let size_bytes: u64 = bytes.len() as u64;
        let entry: AssetEntry = AssetEntry {
            id: asset_id,
            path: path.clone(),
            content_hash,
            original_filename,
            media_type,
            size_bytes,
            dimensions,
        };
        self.assets.push(entry.clone());
        self.files.insert(path, bytes);
        entry
    }

    // find_by_id
    // Inputs: an asset id.
    // Output: the matching entry, when present.
    pub fn find_by_id(&self, id: &str) -> Option<&AssetEntry> {
        assert!(!id.is_empty(), "find_by_id: empty id");
        self.assets.iter().find(|e| e.id == id)
    }
}

// derive_asset_id
// Inputs: lowercase hex digest from sha256.
// Output: "asset_<first ASSET_ID_HASH_LEN chars>" — keeps the id short
// enough to be readable in HTML while preserving plenty of entropy
// (16 hex chars = 64 bits, collision-safe for our scale).
fn derive_asset_id(hash_hex: &str) -> String {
    assert!(
        hash_hex.len() >= ASSET_ID_HASH_LEN,
        "derive_asset_id: digest too short"
    );
    format!("asset_{}", &hash_hex[..ASSET_ID_HASH_LEN])
}

// derive_extension
// Inputs: a filename and a media type.
// Output: a lowercase file extension (no leading dot). Prefers the
// filename's own extension; falls back to a small media-type mapping
// for common image MIMEs. Returns "" when nothing is recognisable.
fn derive_extension(filename: &str, media_type: &str) -> String {
    let lower: String = filename.to_ascii_lowercase();
    if let Some(idx) = lower.rfind('.') {
        let ext: &str = &lower[(idx + 1)..];
        if !ext.is_empty() && ext.len() <= 8 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
            return ext.to_string();
        }
    }
    match media_type.to_ascii_lowercase().as_str() {
        "image/png" => "png".to_string(),
        "image/jpeg" | "image/jpg" => "jpg".to_string(),
        "image/gif" => "gif".to_string(),
        "image/webp" => "webp".to_string(),
        "image/svg+xml" => "svg".to_string(),
        "image/bmp" => "bmp".to_string(),
        _ => String::new(),
    }
}

// sha256_hex
// Inputs: a byte slice.
// Output: lowercase hex digest of its sha256.
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher: Sha256 = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out: String = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn empty_registry_round_trips_json() {
        let r = AssetRegistry::new_empty();
        let json = r.index_json().unwrap();
        let back = AssetRegistry::from_index_json(&json).unwrap();
        assert_eq!(back, r);
        assert!(back.is_empty());
        assert_eq!(back.version, ASSETS_INDEX_VERSION);
    }

    #[test]
    fn registry_with_entry_round_trips_json() {
        let mut r = AssetRegistry::new_empty();
        r.assets.push(AssetEntry {
            id: "asset_01HQ".into(),
            path: "assets/images/logo.svg".into(),
            content_hash: "sha256:abc".into(),
            original_filename: "logo.svg".into(),
            media_type: "image/svg+xml".into(),
            size_bytes: 42,
            dimensions: Some(AssetDimensions { width: 200, height: 200 }),
        });
        let json = r.index_json().unwrap();
        let back = AssetRegistry::from_index_json(&json).unwrap();
        assert_eq!(back.assets.len(), 1);
        assert_eq!(back.assets[0].id, "asset_01HQ");
        // Files map is skipped intentionally — empty on round trip.
        assert!(back.files.is_empty());
    }

    #[test]
    fn pre_existing_index_without_version_uses_default() {
        let raw = r#"{"assets":[]}"#;
        let r = AssetRegistry::from_index_json(raw).unwrap();
        assert_eq!(r.version, ASSETS_INDEX_VERSION);
        assert!(r.assets.is_empty());
    }

    #[test]
    fn insert_blob_assigns_hash_prefixed_id_and_stores_bytes() {
        let mut r = AssetRegistry::new_empty();
        let bytes = b"\x89PNG\r\n\x1a\nfake-png-bytes".to_vec();
        let entry = r.insert_blob(
            bytes.clone(),
            "photo.png".into(),
            "image/png".into(),
            Some(AssetDimensions { width: 200, height: 100 }),
        );
        assert!(entry.id.starts_with("asset_"));
        assert_eq!(entry.id.len(), 6 + ASSET_ID_HASH_LEN);
        assert!(entry.path.starts_with("assets/images/"));
        assert!(entry.path.ends_with(".png"));
        assert_eq!(entry.size_bytes, bytes.len() as u64);
        assert!(entry.content_hash.starts_with("sha256:"));
        assert_eq!(r.files.get(&entry.path), Some(&bytes));
        assert_eq!(r.entry_count(), 1);
        assert_eq!(r.find_by_id(&entry.id), Some(&entry));
    }

    #[test]
    fn insert_blob_dedupes_identical_content() {
        let mut r = AssetRegistry::new_empty();
        let bytes = b"hello world bytes".to_vec();
        let a = r.insert_blob(
            bytes.clone(),
            "first.jpg".into(),
            "image/jpeg".into(),
            None,
        );
        let b = r.insert_blob(
            bytes,
            "second_name.jpg".into(),
            "image/jpeg".into(),
            None,
        );
        assert_eq!(a, b);
        assert_eq!(r.entry_count(), 1);
    }

    #[test]
    fn insert_blob_derives_extension_from_media_type_when_filename_missing_ext() {
        let mut r = AssetRegistry::new_empty();
        let entry = r.insert_blob(
            b"jpg-bytes".to_vec(),
            "noext".into(),
            "image/jpeg".into(),
            None,
        );
        assert!(entry.path.ends_with(".jpg"));
    }

    #[test]
    fn entry_round_trip_with_no_dimensions() {
        let entry = AssetEntry {
            id: "asset_x".into(),
            path: "assets/media/song.mp3".into(),
            content_hash: "sha256:def".into(),
            original_filename: "song.mp3".into(),
            media_type: "audio/mpeg".into(),
            size_bytes: 10_000,
            dimensions: None,
        };
        let s = serde_json::to_string(&entry).unwrap();
        let back: AssetEntry = serde_json::from_str(&s).unwrap();
        assert_eq!(back, entry);
    }
}
