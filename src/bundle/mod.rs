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
pub use io_thread::{IoRequest, IoResponse, IoThread};
pub use manifest::{
    Dimensions, ManifestData, Metadata, SUPPORTED_FORMAT_MAJOR, SlideEntry, ThemeRef,
    validate_format_version,
};
pub use reader::BundleReader;
pub use theme_io::{SerializedTheme, deserialize_theme, serialize_theme};
pub use writer::BundleWriter;

use std::path::PathBuf;

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
