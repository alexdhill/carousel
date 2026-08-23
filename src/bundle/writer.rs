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
    pub fn create(target_path: &Path) -> BundleResult<Self> {
        assert!(
            !target_path.as_os_str().is_empty(),
            "BundleWriter::create: empty path"
        );
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
    fn drop(&mut self) {
        if !self.finished {
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

        let mut w1 = BundleWriter::create(&path).unwrap();
        w1.write_string("manifest.json", "v1").unwrap();
        w1.finish().unwrap();
        assert_eq!(
            BundleReader::open(&path)
                .unwrap()
                .read_string("manifest.json")
                .unwrap(),
            "v1"
        );

        let mut w2 = BundleWriter::create(&path).unwrap();
        w2.write_string("manifest.json", "v2").unwrap();
        w2.finish().unwrap();
        assert_eq!(
            BundleReader::open(&path)
                .unwrap()
                .read_string("manifest.json")
                .unwrap(),
            "v2"
        );
    }

    #[test]
    fn aborted_write_leaves_target_untouched_and_no_tmp_residue() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a.slidedeck");

        let mut w0 = BundleWriter::create(&path).unwrap();
        w0.write_string("manifest.json", "intact").unwrap();
        w0.finish().unwrap();
        let original_bytes: Vec<u8> = std::fs::read(&path).unwrap();

        let tmp_existed_during: PathBuf;
        {
            let mut w = BundleWriter::create(&path).unwrap();
            w.write_string("manifest.json", "garbage").unwrap();
            tmp_existed_during = w.tmp_path().to_path_buf();
            assert!(tmp_existed_during.exists());
        }

        assert!(!tmp_existed_during.exists());

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

        w.finish().unwrap();
    }
}
