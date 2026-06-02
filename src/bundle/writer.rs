// BundleWriter.
//
// SPEC §6.4 — atomic deck save. The pattern: open `<target>.tmp` as a fresh
// ZipWriter on the same directory as the target, stream every entry into
// it, finalize the archive, fsync, then `std::fs::rename` over the target
// path. Same-directory rename is atomic on POSIX and on Windows ≥10 when
// using the standard library wrapper.
//
// If the process dies mid-write the `.tmp` is left behind but the
// previous bundle at `<target>` is untouched. The next save overwrites
// `.tmp` cleanly — no special recovery path needed for v1.

use crate::bundle::{BundleError, BundleResult};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

const COMPRESSION_LEVEL: i64 = 6;
const TMP_SUFFIX: &str = "slidedeck.tmp";

pub struct BundleWriter {
    target_path: PathBuf,
    tmp_path: PathBuf,
    writer: Option<ZipWriter<File>>,
    options: SimpleFileOptions,
    finished: bool,
}

impl BundleWriter {
    // create
    // Inputs: the final bundle path (with .slidedeck extension or any
    // user-chosen extension).
    // Output: a writer with the temp file open and the deflate options
    // pre-configured.
    // Errors: Io if the temp file cannot be created.
    // Dataflow: derive `<target>.slidedeck.tmp` next to the target so the
    // eventual rename stays on the same volume; create the file with
    // truncate; wrap in a ZipWriter.
    pub fn create(target_path: &Path) -> BundleResult<Self> {
        assert!(!target_path.as_os_str().is_empty(), "BundleWriter::create: empty path");
        let tmp_path: PathBuf = tmp_path_for(target_path);
        debug!(target = %target_path.display(), tmp = %tmp_path.display(), "bundle: create");
        if let Some(parent) = tmp_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let file: File = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)?;
        let writer: ZipWriter<File> = ZipWriter::new(file);
        let options: SimpleFileOptions = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(COMPRESSION_LEVEL));
        Ok(Self {
            target_path: target_path.to_path_buf(),
            tmp_path,
            writer: Some(writer),
            options,
            finished: false,
        })
    }

    pub fn target_path(&self) -> &Path {
        &self.target_path
    }

    pub fn tmp_path(&self) -> &Path {
        &self.tmp_path
    }

    // write_string
    // Inputs: an entry name and its UTF-8 contents.
    // Output: side-effect; appends the entry to the in-progress archive.
    // Errors: Zip if start_file fails; Io on write failure.
    pub fn write_string(&mut self, name: &str, content: &str) -> BundleResult<()> {
        assert!(!name.is_empty(), "write_string: empty name");
        assert!(!self.finished, "write_string: writer already finished");
        let writer = match self.writer.as_mut() {
            Some(w) => w,
            None => unreachable!("writer present until finish"),
        };
        writer.start_file(name, self.options)?;
        writer.write_all(content.as_bytes())?;
        Ok(())
    }

    // write_bytes
    // Inputs: an entry name and its raw byte contents.
    // Output: side-effect; appends the entry.
    // Errors: Zip / Io.
    pub fn write_bytes(&mut self, name: &str, content: &[u8]) -> BundleResult<()> {
        assert!(!name.is_empty(), "write_bytes: empty name");
        assert!(!self.finished, "write_bytes: writer already finished");
        let writer = match self.writer.as_mut() {
            Some(w) => w,
            None => unreachable!("writer present until finish"),
        };
        writer.start_file(name, self.options)?;
        writer.write_all(content)?;
        Ok(())
    }

    // finish
    // Inputs: self (consumed).
    // Output: Ok(()) once the archive is finalized, fsync'd, and renamed
    // over the target path.
    // Errors: Zip on finalize failure; Io on fsync; RenameFailed on the
    // atomic rename step (the original target is untouched in that case).
    // Dataflow:
    //   1. finalize the zip directory
    //   2. sync_all so kernel buffers hit disk before rename
    //   3. drop the file handle
    //   4. rename .tmp -> target (atomic on POSIX; atomic on Windows 10+)
    pub fn finish(mut self) -> BundleResult<()> {
        assert!(!self.finished, "finish: already finished");
        let writer: ZipWriter<File> = match self.writer.take() {
            Some(w) => w,
            None => unreachable!("writer present until finish"),
        };
        let file: File = writer.finish()?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&self.tmp_path, &self.target_path).map_err(|e| {
            BundleError::RenameFailed {
                target: self.target_path.clone(),
                source: e,
            }
        })?;
        self.finished = true;
        debug!(target = %self.target_path.display(), "bundle: write committed");
        Ok(())
    }
}

impl Drop for BundleWriter {
    // drop
    // Inputs: &mut self.
    // Output: side-effect; if the caller never called finish(), the temp
    // file is removed so half-written archives do not linger on disk. The
    // target path is never touched here — the rename only happens in
    // finish(), so the previous bundle (if any) remains intact.
    fn drop(&mut self) {
        if !self.finished {
            // Close the writer first so the handle is released before unlink.
            self.writer = None;
            if self.tmp_path.exists()
                && let Err(e) = std::fs::remove_file(&self.tmp_path)
            {
                warn!(
                    path = %self.tmp_path.display(),
                    "bundle: failed to remove abandoned tmp file: {}", e
                );
            }
        }
    }
}

// tmp_path_for
// Inputs: the target bundle path.
// Output: a sibling path with `.slidedeck.tmp` appended to the file name
// stem. Choosing a sibling (same parent dir) is what makes the eventual
// rename atomic.
fn tmp_path_for(target: &Path) -> PathBuf {
    assert!(!target.as_os_str().is_empty(), "tmp_path_for: empty path");
    let mut tmp: PathBuf = target.to_path_buf();
    let file_name: String = target
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "bundle".to_string());
    tmp.set_file_name(format!("{file_name}.{TMP_SUFFIX}"));
    tmp
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::bundle::BundleReader;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn tmp_path_is_sibling_with_tmp_suffix() {
        let p = tmp_path_for(Path::new("/tmp/deck.slidedeck"));
        assert_eq!(p.to_string_lossy(), "/tmp/deck.slidedeck.slidedeck.tmp");
    }

    #[test]
    fn write_then_read_round_trips_entries() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("d.slidedeck");
        let mut w = BundleWriter::create(&path).unwrap();
        w.write_string("manifest.json", r#"{"x":1}"#).unwrap();
        w.write_bytes("assets/x.bin", &[10, 20, 30]).unwrap();
        w.finish().unwrap();

        assert!(path.exists(), "target file must exist after finish");
        let mut r = BundleReader::open(&path).unwrap();
        assert_eq!(r.read_string("manifest.json").unwrap(), r#"{"x":1}"#);
        assert_eq!(r.read_bytes("assets/x.bin").unwrap(), vec![10, 20, 30]);
    }

    #[test]
    fn finish_replaces_an_existing_target_atomically() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ex.slidedeck");

        // First save.
        let mut w1 = BundleWriter::create(&path).unwrap();
        w1.write_string("manifest.json", "v1").unwrap();
        w1.finish().unwrap();
        assert_eq!(BundleReader::open(&path).unwrap().read_string("manifest.json").unwrap(), "v1");

        // Second save overwrites.
        let mut w2 = BundleWriter::create(&path).unwrap();
        w2.write_string("manifest.json", "v2").unwrap();
        w2.finish().unwrap();
        assert_eq!(BundleReader::open(&path).unwrap().read_string("manifest.json").unwrap(), "v2");
    }

    #[test]
    fn aborted_write_leaves_target_untouched_and_no_tmp_residue() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a.slidedeck");
        // Seed the target with known bytes via the writer (a complete save).
        let mut w0 = BundleWriter::create(&path).unwrap();
        w0.write_string("manifest.json", "intact").unwrap();
        w0.finish().unwrap();
        let original_bytes: Vec<u8> = std::fs::read(&path).unwrap();

        // Begin a second save and abandon it without calling finish().
        let tmp_existed_during: PathBuf;
        {
            let mut w = BundleWriter::create(&path).unwrap();
            w.write_string("manifest.json", "garbage").unwrap();
            tmp_existed_during = w.tmp_path().to_path_buf();
            assert!(tmp_existed_during.exists());
            // drop without finish.
        }
        // Drop cleared the tmp file.
        assert!(!tmp_existed_during.exists());
        // Target bytes unchanged.
        assert_eq!(std::fs::read(&path).unwrap(), original_bytes);
    }

    #[test]
    fn write_to_missing_directory_creates_it() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("a/b/c");
        let path = nested.join("deck.slidedeck");
        let mut w = BundleWriter::create(&path).unwrap();
        w.write_string("manifest.json", "{}").unwrap();
        w.finish().unwrap();
        assert!(path.exists());
    }

    #[test]
    fn write_after_finish_panics() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("p.slidedeck");
        let mut w = BundleWriter::create(&path).unwrap();
        w.write_string("a", "b").unwrap();
        // Move out of `w` via finish — subsequent writes are impossible
        // because `w` is consumed; this test exists for documentation
        // (it would not compile if we called w.write_string after finish).
        w.finish().unwrap();
    }
}
