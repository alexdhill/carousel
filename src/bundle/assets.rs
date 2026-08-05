use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub const ASSETS_INDEX_VERSION: &str = "1.0";
pub const ASSET_ID_HASH_LEN: usize = 16;
pub const ASSETS_IMAGES_DIR: &str = "assets/images";

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

    pub fn new_empty() -> Self {
        Self {
            version: ASSETS_INDEX_VERSION.to_string(),
            assets: Vec::new(),
            files: HashMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }

    pub fn entry_count(&self) -> usize {
        self.assets.len()
    }

    pub fn index_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_index_json(s: &str) -> Result<Self, serde_json::Error> {
        assert!(!s.is_empty(), "from_index_json: empty input");
        let parsed: Self = serde_json::from_str(s)?;
        Ok(parsed)
    }

    pub fn insert_blob(
        &mut self,
        bytes: Vec<u8>,
        original_filename: String,
        media_type: String,
        dimensions: Option<AssetDimensions>,
    ) -> AssetEntry {
        assert!(!bytes.is_empty(), "AssetRegistry::insert_blob: empty bytes");
        assert!(
            !media_type.is_empty(),
            "AssetRegistry::insert_blob: empty media_type"
        );

        let hash_hex: String = sha256_hex(&bytes);
        let content_hash: String = format!("sha256:{hash_hex}");

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

    pub fn find_by_id(&self, id: &str) -> Option<&AssetEntry> {
        assert!(!id.is_empty(), "find_by_id: empty id");
        self.assets.iter().find(|e| e.id == id)
    }
}

fn derive_asset_id(hash_hex: &str) -> String {
    assert!(
        hash_hex.len() >= ASSET_ID_HASH_LEN,
        "derive_asset_id: digest too short"
    );
    format!("asset_{}", &hash_hex[..ASSET_ID_HASH_LEN])
}

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
            dimensions: Some(AssetDimensions {
                width: 200,
                height: 200,
            }),
        });
        let json = r.index_json().unwrap();
        let back = AssetRegistry::from_index_json(&json).unwrap();
        assert_eq!(back.assets.len(), 1);
        assert_eq!(back.assets[0].id, "asset_01HQ");

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
            Some(AssetDimensions {
                width: 200,
                height: 100,
            }),
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
        let a = r.insert_blob(bytes.clone(), "first.jpg".into(), "image/jpeg".into(), None);
        let b = r.insert_blob(bytes, "second_name.jpg".into(), "image/jpeg".into(), None);
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
