// Bundle I/O.
//
// SPEC §3 + §6.3–6.4. Decks are persisted as `.slidedeck` ZIP archives with
// the layout fixed in §3.2: manifest.json at the root, slide HTML fragments
// under slides/, theme files under theme/, an asset registry under
// assets/, and per-slide thumbnails under thumbnails/.
//
// This module exposes:
//   - ManifestData / SlideEntry / Dimensions / ThemeRef / Metadata
//     (§3.3 schema as Rust structs)
//   - AssetRegistry (§3.4) — minimal pass-through implementation, kept
//     here because no asset-import commands exist yet
//   - BundleReader / BundleWriter — typed ZIP I/O with atomic write semantics
//   - SerializedDeck + Deck::serialize_for_save / load_from_serialized —
//     the bridge between the in-memory tree and the bundle's file contents
//   - IoThread — a background worker that handles save / load off the main
//     thread, returning typed responses via mpsc
//
// Quicksave (§6.5) is intentionally deferred per the Stage 7 scope note.

#![allow(dead_code, unused_imports)]

pub mod assets;
pub mod deck_io;
pub mod io_thread;
pub mod manifest;
pub mod reader;
pub mod theme_io;
pub mod writer;

pub use assets::AssetRegistry;
pub use deck_io::{SerializedDeck, deserialize_deck, serialize_deck};
pub use theme_io::{SerializedTheme, deserialize_theme, serialize_theme};
pub use io_thread::{IoRequest, IoResponse, IoThread};
pub use manifest::{
    Dimensions, ManifestData, Metadata, SUPPORTED_FORMAT_MAJOR, SlideEntry, ThemeRef,
    validate_format_version,
};
pub use reader::BundleReader;
pub use writer::BundleWriter;

use std::path::PathBuf;

// BundleError
// Tagged errors for every step of bundle I/O. Each variant pinpoints the
// failure layer so the UI layer can surface a precise message.
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("missing bundle entry: {0}")]
    MissingEntry(String),

    #[error("incompatible format version: {0} (this app supports major {1})")]
    IncompatibleVersion(String, u32),

    #[error("malformed manifest: {0}")]
    MalformedManifest(String),

    #[error("slide parse: {0}")]
    SlideParse(String),

    #[error("rename failed for {target:?}: {source}")]
    RenameFailed {
        target: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub type BundleResult<T> = Result<T, BundleError>;
