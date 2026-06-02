// BundleReader.
//
// SPEC §3.1 + §6.3 — typed wrapper around `zip::ZipArchive<File>`. Decks
// are random-access: when a slide is opened we read just its HTML; when
// the user navigates we only re-read what is required. This reader keeps
// the archive open for the lifetime of the deck so re-reads do not
// repeatedly hit `File::open`.
//
// Lookup APIs return BundleError::MissingEntry on absent paths so the
// caller can distinguish "deck doesn't contain X" from "I/O failure".

use crate::bundle::{BundleError, BundleResult};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use tracing::debug;
use zip::ZipArchive;

#[derive(Debug)]
pub struct BundleReader {
    archive: ZipArchive<File>,
    path: PathBuf,
}

impl BundleReader {
    // open
    // Inputs: path to a .slidedeck (or any ZIP-format) file.
    // Output: a BundleReader holding the open ZipArchive.
    // Errors: Io if the file cannot be opened; Zip if the archive header
    // is malformed.
    // Dataflow: File::open -> ZipArchive::new -> wrap.
    pub fn open(path: &Path) -> BundleResult<Self> {
        assert!(!path.as_os_str().is_empty(), "BundleReader::open: empty path");
        debug!(path = %path.display(), "bundle: opening archive");
        let file: File = File::open(path)?;
        let archive: ZipArchive<File> = ZipArchive::new(file)?;
        Ok(Self { archive, path: path.to_path_buf() })
    }

    // path
    // Inputs: self.
    // Output: the path the reader was opened from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    // entry_names
    // Inputs: &self (interior-mutable via &mut for ZipArchive::by_index).
    // Output: vector of every entry name in archive order.
    // Errors: Zip on any per-index lookup failure.
    pub fn entry_names(&mut self) -> BundleResult<Vec<String>> {
        let n: usize = self.archive.len();
        let mut out: Vec<String> = Vec::with_capacity(n);
        let mut i: usize = 0;
        while i < n {
            let entry = self.archive.by_index(i)?;
            out.push(entry.name().to_string());
            i += 1;
        }
        Ok(out)
    }

    // has_entry
    // Inputs: an entry name.
    // Output: true if the archive contains an entry with that exact name.
    // Errors: Zip on per-index lookup failure.
    pub fn has_entry(&mut self, name: &str) -> BundleResult<bool> {
        assert!(!name.is_empty(), "has_entry: empty name");
        match self.archive.by_name(name) {
            Ok(_) => Ok(true),
            Err(zip::result::ZipError::FileNotFound) => Ok(false),
            Err(e) => Err(BundleError::Zip(e)),
        }
    }

    // read_string
    // Inputs: an entry name.
    // Output: the entry contents as a UTF-8 String.
    // Errors: MissingEntry if the name is absent; Zip on read failure; Io
    // on UTF-8 decode (wrapped through std::io::Error -> BundleError::Io).
    pub fn read_string(&mut self, name: &str) -> BundleResult<String> {
        assert!(!name.is_empty(), "read_string: empty name");
        let mut entry = self
            .archive
            .by_name(name)
            .map_err(|e| match e {
                zip::result::ZipError::FileNotFound => {
                    BundleError::MissingEntry(name.to_string())
                }
                other => BundleError::Zip(other),
            })?;
        let mut buf: String = String::new();
        entry.read_to_string(&mut buf)?;
        Ok(buf)
    }

    // read_bytes
    // Inputs: an entry name.
    // Output: the entry contents as raw bytes.
    // Errors: MissingEntry if absent; Zip / Io on read.
    pub fn read_bytes(&mut self, name: &str) -> BundleResult<Vec<u8>> {
        assert!(!name.is_empty(), "read_bytes: empty name");
        let mut entry = self
            .archive
            .by_name(name)
            .map_err(|e| match e {
                zip::result::ZipError::FileNotFound => {
                    BundleError::MissingEntry(name.to_string())
                }
                other => BundleError::Zip(other),
            })?;
        let mut buf: Vec<u8> = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut buf)?;
        Ok(buf)
    }

    // entry_count
    // Inputs: self.
    // Output: number of entries in the archive.
    pub fn entry_count(&self) -> usize {
        self.archive.len()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::bundle::BundleWriter;
    use tempfile::TempDir;

    fn write_simple_bundle(dir: &TempDir, name: &str) -> PathBuf {
        let path = dir.path().join(name);
        let mut w = BundleWriter::create(&path).unwrap();
        w.write_string("manifest.json", "{}").unwrap();
        w.write_string("slides/slide_a.html", "<section/>").unwrap();
        w.write_bytes("assets/x.bin", &[1, 2, 3, 4]).unwrap();
        w.finish().unwrap();
        path
    }

    #[test]
    fn open_missing_file_errors() {
        let err = BundleReader::open(Path::new("/no/such/path.slidedeck")).unwrap_err();
        assert!(matches!(err, BundleError::Io(_)));
    }

    #[test]
    fn open_then_list_entries() {
        let dir = TempDir::new().unwrap();
        let path = write_simple_bundle(&dir, "x.slidedeck");
        let mut r = BundleReader::open(&path).unwrap();
        let names = r.entry_names().unwrap();
        assert!(names.iter().any(|n| n == "manifest.json"));
        assert!(names.iter().any(|n| n == "slides/slide_a.html"));
        assert!(names.iter().any(|n| n == "assets/x.bin"));
        assert_eq!(r.entry_count(), 3);
    }

    #[test]
    fn read_string_returns_entry_contents() {
        let dir = TempDir::new().unwrap();
        let path = write_simple_bundle(&dir, "y.slidedeck");
        let mut r = BundleReader::open(&path).unwrap();
        assert_eq!(r.read_string("manifest.json").unwrap(), "{}");
        assert_eq!(r.read_string("slides/slide_a.html").unwrap(), "<section/>");
    }

    #[test]
    fn read_bytes_round_trips_binary() {
        let dir = TempDir::new().unwrap();
        let path = write_simple_bundle(&dir, "z.slidedeck");
        let mut r = BundleReader::open(&path).unwrap();
        assert_eq!(r.read_bytes("assets/x.bin").unwrap(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn has_entry_reports_presence() {
        let dir = TempDir::new().unwrap();
        let path = write_simple_bundle(&dir, "h.slidedeck");
        let mut r = BundleReader::open(&path).unwrap();
        assert!(r.has_entry("manifest.json").unwrap());
        assert!(!r.has_entry("nope").unwrap());
    }

    #[test]
    fn missing_entry_yields_missing_entry_error() {
        let dir = TempDir::new().unwrap();
        let path = write_simple_bundle(&dir, "m.slidedeck");
        let mut r = BundleReader::open(&path).unwrap();
        let err = r.read_string("does/not/exist").unwrap_err();
        assert!(matches!(err, BundleError::MissingEntry(s) if s == "does/not/exist"));
    }
}
