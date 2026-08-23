#![allow(dead_code)]

use crate::bundle::deck_io::{read_serialized, write_serialized};
use crate::bundle::theme_io::{read_theme, write_theme};
use crate::bundle::{BundleError, BundleReader, BundleWriter, SerializedDeck, SerializedTheme};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use tracing::{debug, error, info};

#[derive(Debug)]
pub enum IoRequest {
    Save {
        serialized: SerializedDeck,
        target_path: PathBuf,
    },
    Load {
        path: PathBuf,
    },

    SaveTheme {
        serialized: SerializedTheme,
        target_path: PathBuf,
    },
    LoadTheme {
        path: PathBuf,
    },

    ExportHtml {
        files: Vec<(String, Vec<u8>)>,
        dest_dir: PathBuf,
    },
}

#[derive(Debug)]
pub enum IoResponse {
    Saved {
        path: PathBuf,
    },
    Loaded {
        serialized: SerializedDeck,
        path: PathBuf,
    },
    ThemeSaved {
        path: PathBuf,
    },
    ThemeLoaded {
        serialized: SerializedTheme,
        path: PathBuf,
    },
    Exported {
        dest: PathBuf,
    },
    Error {
        operation: &'static str,
        path: Option<PathBuf>,
        message: String,
    },
}

pub struct IoThread {
    sender: Sender<IoRequest>,
    handle: Option<JoinHandle<()>>,
}

impl IoThread {
    pub fn spawn(
        responses: Sender<IoResponse>,
        wake: Box<dyn Fn() + Send + 'static>,
    ) -> std::io::Result<Self> {
        let (tx, rx): (Sender<IoRequest>, Receiver<IoRequest>) = mpsc::channel();
        let handle: JoinHandle<()> = thread::Builder::new()
            .name("carousel-io".into())
            .spawn(move || worker_loop(rx, responses, wake))?;
        info!("IoThread spawned");
        Ok(Self {
            sender: tx,
            handle: Some(handle),
        })
    }

    pub fn submit(&self, request: IoRequest) -> Result<(), ()> {
        self.sender.send(request).map_err(|_| ())
    }
}

impl Drop for IoThread {
    fn drop(&mut self) {
        let _ = std::mem::replace(&mut self.sender, mpsc::channel().0);
        if let Some(h) = self.handle.take()
            && let Err(e) = h.join()
        {
            error!("IoThread join failed: {:?}", e);
        }
    }
}

fn worker_loop(
    requests: Receiver<IoRequest>,
    responses: Sender<IoResponse>,
    wake: Box<dyn Fn() + Send + 'static>,
) {
    const MAX_ITERATIONS: u64 = u64::MAX / 2;
    let mut iter: u64 = 0;
    while iter < MAX_ITERATIONS {
        iter += 1;
        let request: IoRequest = match requests.recv() {
            Ok(r) => r,
            Err(_) => {
                debug!("IoThread: request channel closed; exiting");
                return;
            }
        };
        let response: IoResponse = handle_request(request);
        if responses.send(response).is_err() {
            error!("IoThread: response channel closed; exiting");
            return;
        }
        wake();
    }
    error!("IoThread: MAX_ITERATIONS hit; this should never happen");
}

fn handle_request(request: IoRequest) -> IoResponse {
    match request {
        IoRequest::Save {
            serialized,
            target_path,
        } => save_blocking(serialized, target_path),
        IoRequest::Load { path } => load_blocking(path),
        IoRequest::SaveTheme {
            serialized,
            target_path,
        } => save_theme_blocking(serialized, target_path),
        IoRequest::LoadTheme { path } => load_theme_blocking(path),
        IoRequest::ExportHtml { files, dest_dir } => export_html_blocking(files, dest_dir),
    }
}

fn save_theme_blocking(serialized: SerializedTheme, target_path: PathBuf) -> IoResponse {
    debug!(target = %target_path.display(), "io: save theme begin");
    let mut writer: BundleWriter = match BundleWriter::create(&target_path) {
        Ok(w) => w,
        Err(e) => return error_response("save_theme", Some(target_path), e),
    };
    if let Err(e) = write_theme(&mut writer, &serialized) {
        return error_response("save_theme", Some(target_path), e);
    }
    if let Err(e) = writer.finish() {
        return error_response("save_theme", Some(target_path), e);
    }
    info!(target = %target_path.display(), "io: save theme committed");
    IoResponse::ThemeSaved { path: target_path }
}

fn load_theme_blocking(path: PathBuf) -> IoResponse {
    debug!(path = %path.display(), "io: load theme begin");
    let mut reader: BundleReader = match BundleReader::open(&path) {
        Ok(r) => r,
        Err(e) => return error_response("load_theme", Some(path), e),
    };
    let serialized: SerializedTheme = match read_theme(&mut reader) {
        Ok(s) => s,
        Err(e) => return error_response("load_theme", Some(path), e),
    };
    info!(path = %path.display(), "io: load theme complete");
    IoResponse::ThemeLoaded { serialized, path }
}

fn export_html_blocking(files: Vec<(String, Vec<u8>)>, dest_dir: PathBuf) -> IoResponse {
    if let Err(e) = std::fs::create_dir_all(&dest_dir) {
        return IoResponse::Error {
            operation: "export_html",
            path: Some(dest_dir),
            message: e.to_string(),
        };
    }
    for (rel, bytes) in &files {
        let full: PathBuf = dest_dir.join(rel);
        if let Some(parent) = full.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return IoResponse::Error {
                operation: "export_html",
                path: Some(full),
                message: e.to_string(),
            };
        }
        if let Err(e) = std::fs::write(&full, bytes) {
            return IoResponse::Error {
                operation: "export_html",
                path: Some(full),
                message: e.to_string(),
            };
        }
    }
    IoResponse::Exported { dest: dest_dir }
}

fn save_blocking(serialized: SerializedDeck, target_path: PathBuf) -> IoResponse {
    debug!(target = %target_path.display(), "io: save begin");
    let mut writer: BundleWriter = match BundleWriter::create(&target_path) {
        Ok(w) => w,
        Err(e) => return error_response("save", Some(target_path), e),
    };
    if let Err(e) = write_serialized(&mut writer, &serialized) {
        return error_response("save", Some(target_path), e);
    }
    if let Err(e) = writer.finish() {
        return error_response("save", Some(target_path), e);
    }
    info!(target = %target_path.display(), "io: save committed");
    IoResponse::Saved { path: target_path }
}

fn load_blocking(path: PathBuf) -> IoResponse {
    debug!(path = %path.display(), "io: load begin");
    let mut reader: BundleReader = match BundleReader::open(&path) {
        Ok(r) => r,
        Err(e) => return error_response("load", Some(path), e),
    };
    let serialized: SerializedDeck = match read_serialized(&mut reader) {
        Ok(s) => s,
        Err(e) => return error_response("load", Some(path), e),
    };
    info!(path = %path.display(), "io: load complete");
    IoResponse::Loaded { serialized, path }
}

fn error_response(op: &'static str, path: Option<PathBuf>, e: BundleError) -> IoResponse {
    error!(operation = op, "io error: {}", e);
    IoResponse::Error {
        operation: op,
        path,
        message: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::bundle::deck_io::serialize_deck;
    use crate::deck::Deck;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tempfile::TempDir;

    fn drain_one(rx: &Receiver<IoResponse>) -> IoResponse {
        rx.recv_timeout(Duration::from_secs(15))
            .expect("io thread response within 15s")
    }

    #[test]
    fn save_then_load_round_trips_through_thread() {
        let (rtx, rrx) = mpsc::channel::<IoResponse>();
        let wakes = Arc::new(AtomicUsize::new(0));
        let wakes_for_closure = wakes.clone();
        let io = IoThread::spawn(
            rtx,
            Box::new(move || {
                wakes_for_closure.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .unwrap();

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("roundtrip.slidedeck");
        let deck = Deck::sample();
        let serialized = serialize_deck(&deck).unwrap();

        io.submit(IoRequest::Save {
            serialized,
            target_path: path.clone(),
        })
        .unwrap();
        match drain_one(&rrx) {
            IoResponse::Saved { path: p } => assert_eq!(p, path),
            other => panic!("expected Saved, got {other:?}"),
        }

        io.submit(IoRequest::Load { path: path.clone() }).unwrap();
        match drain_one(&rrx) {
            IoResponse::Loaded {
                serialized,
                path: p,
            } => {
                assert_eq!(p, path);
                assert!(!serialized.slide_files.is_empty());
            }
            other => panic!("expected Loaded, got {other:?}"),
        }

        assert!(wakes.load(Ordering::SeqCst) >= 2);
    }

    #[test]
    fn save_then_load_theme_round_trips_through_thread() {
        use crate::bundle::{AssetRegistry, serialize_theme};
        use crate::deck::ThemeData;
        let (rtx, rrx) = mpsc::channel::<IoResponse>();
        let io = IoThread::spawn(rtx, Box::new(|| {})).unwrap();

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("t.slidetheme");
        let serialized =
            serialize_theme(&ThemeData::default(), &AssetRegistry::new_empty()).unwrap();

        io.submit(IoRequest::SaveTheme {
            serialized,
            target_path: path.clone(),
        })
        .unwrap();
        match drain_one(&rrx) {
            IoResponse::ThemeSaved { path: p } => assert_eq!(p, path),
            other => panic!("expected ThemeSaved, got {other:?}"),
        }

        io.submit(IoRequest::LoadTheme { path: path.clone() })
            .unwrap();
        match drain_one(&rrx) {
            IoResponse::ThemeLoaded {
                serialized,
                path: p,
            } => {
                assert_eq!(p, path);
                assert!(serialized.theme_json.contains("theme_id"));
            }
            other => panic!("expected ThemeLoaded, got {other:?}"),
        }
    }

    #[test]
    fn load_missing_path_returns_error_response() {
        let (rtx, rrx) = mpsc::channel::<IoResponse>();
        let io = IoThread::spawn(rtx, Box::new(|| {})).unwrap();
        io.submit(IoRequest::Load {
            path: PathBuf::from("/no/such/file.slidedeck"),
        })
        .unwrap();
        match drain_one(&rrx) {
            IoResponse::Error { operation, .. } => assert_eq!(operation, "load"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn save_to_unwritable_target_returns_error_response() {
        let (rtx, rrx) = mpsc::channel::<IoResponse>();
        let io = IoThread::spawn(rtx, Box::new(|| {})).unwrap();
        let deck = Deck::sample();
        let serialized = serialize_deck(&deck).unwrap();

        io.submit(IoRequest::Save {
            serialized,
            target_path: PathBuf::from("/this/should/not/exist/anywhere/foo.slidedeck"),
        })
        .unwrap();
        match drain_one(&rrx) {
            IoResponse::Error { operation, .. } => assert_eq!(operation, "save"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn dropping_io_thread_exits_worker_cleanly() {
        let (rtx, _rrx) = mpsc::channel::<IoResponse>();
        let io = IoThread::spawn(rtx, Box::new(|| {})).unwrap();

        drop(io);
    }
}
