// IoThread.
//
// SPEC §6.4 — saves and loads can take tens of milliseconds for large
// decks; running them on the main (event-loop) thread would freeze the
// editor. This module owns a dedicated worker thread that pulls IoRequest
// values off an mpsc channel and posts IoResponse values to another mpsc
// channel that the main thread drains. After each response is sent the
// worker calls a caller-supplied `wake` closure (typically posting a Tao
// UserEvent) so the main thread knows to look at the inbox.
//
// The worker is intentionally single-threaded: file I/O on the host disk
// is sequential anyway, and a single worker keeps the response ordering
// predictable (an Open immediately followed by a Save completes Open
// first, then Save). Shutdown is implicit — dropping the IoThread closes
// the request channel; the worker's recv() returns Err and the thread
// exits.

#![allow(dead_code)]

use crate::bundle::deck_io::{read_serialized, write_serialized};
use crate::bundle::{BundleError, BundleReader, BundleWriter, SerializedDeck};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use tracing::{debug, error, info};

// IoRequest
// Work items posted from the main thread to the worker. SerializedDeck
// holds owned String/Vec<u8> only, so it crosses thread boundaries cheaply.
#[derive(Debug)]
pub enum IoRequest {
    Save {
        serialized: SerializedDeck,
        target_path: PathBuf,
    },
    Load {
        path: PathBuf,
    },
}

// IoResponse
// Results posted from the worker back to the main thread.
#[derive(Debug)]
pub enum IoResponse {
    Saved {
        path: PathBuf,
    },
    Loaded {
        serialized: SerializedDeck,
        path: PathBuf,
    },
    Error {
        operation: &'static str,
        path: Option<PathBuf>,
        message: String,
    },
}

// IoThread
// Owns the request-side Sender and the thread handle. The response
// Receiver is held by the caller (typically attached to the Tao event
// loop), which is why `spawn` takes a Sender<IoResponse> as an argument.
pub struct IoThread {
    sender: Sender<IoRequest>,
    handle: Option<JoinHandle<()>>,
}

impl IoThread {
    // spawn
    // Inputs:
    //   - responses: a Sender the worker uses to push every IoResponse.
    //   - wake: a closure the worker calls after each send so the main
    //     thread's event loop notices the inbox. Typically posts a
    //     UserEvent::IoResponse on the Tao event-loop proxy.
    // Output: an IoThread holding the request Sender and the JoinHandle.
    // Errors: std::io::Error if the OS refuses to start a thread (fatal;
    // the caller has no recovery beyond aborting startup).
    // Dataflow: build the request channel; spawn a worker that loops on
    // recv(), processes each request, and forwards the response + wake.
    pub fn spawn(
        responses: Sender<IoResponse>,
        wake: Box<dyn Fn() + Send + 'static>,
    ) -> std::io::Result<Self> {
        let (tx, rx): (Sender<IoRequest>, Receiver<IoRequest>) = mpsc::channel();
        let handle: JoinHandle<()> = thread::Builder::new()
            .name("carousel-io".into())
            .spawn(move || worker_loop(rx, responses, wake))?;
        info!("IoThread spawned");
        Ok(Self { sender: tx, handle: Some(handle) })
    }

    // submit
    // Inputs: an IoRequest.
    // Output: Ok(()) if the request was enqueued; Err(()) if the worker
    // has already exited (channel closed). The original request is
    // dropped on the error path because every Stage 7 caller treats this
    // condition as fatal-but-non-recoverable and just logs.
    pub fn submit(&self, request: IoRequest) -> Result<(), ()> {
        self.sender.send(request).map_err(|_| ())
    }
}

impl Drop for IoThread {
    // drop
    // Inputs: &mut self.
    // Output: side-effect; close the request channel (drops Sender) so
    // the worker's recv() returns Err and the loop exits. Then join the
    // thread so the OS resources are cleaned up before the process exits.
    fn drop(&mut self) {
        // Closing the channel signals shutdown.
        let _ = std::mem::replace(&mut self.sender, mpsc::channel().0);
        if let Some(h) = self.handle.take()
            && let Err(e) = h.join()
        {
            error!("IoThread join failed: {:?}", e);
        }
    }
}

// worker_loop
// Inputs: the request Receiver, the response Sender, and the wake closure.
// Output: returns when the request channel closes (Sender dropped).
// Dataflow: loop pull -> dispatch -> send response -> wake. Caps the
// upper bound on iterations to a defensively large number so a runaway
// producer cannot loop forever.
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

// handle_request
// Inputs: a single IoRequest.
// Output: the corresponding IoResponse. Never panics; every error path
// produces an IoResponse::Error carrying the operation tag.
fn handle_request(request: IoRequest) -> IoResponse {
    match request {
        IoRequest::Save { serialized, target_path } => save_blocking(serialized, target_path),
        IoRequest::Load { path } => load_blocking(path),
    }
}

// save_blocking
// Inputs: serialized deck and target path.
// Output: Saved or Error response.
// Dataflow: open BundleWriter (creates the .tmp file) -> stream entries
// -> finish (commits atomically). Any error returns IoResponse::Error.
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

// load_blocking
// Inputs: path to a .slidedeck file.
// Output: Loaded or Error response.
// Dataflow: open BundleReader -> read_serialized -> return SerializedDeck.
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;

    fn drain_one(rx: &Receiver<IoResponse>) -> IoResponse {
        rx.recv_timeout(Duration::from_secs(5))
            .expect("io thread response within 5s")
    }

    #[test]
    fn save_then_load_round_trips_through_thread() {
        let (rtx, rrx) = mpsc::channel::<IoResponse>();
        let wakes = Arc::new(AtomicUsize::new(0));
        let wakes_for_closure = wakes.clone();
        let io = IoThread::spawn(rtx, Box::new(move || {
            wakes_for_closure.fetch_add(1, Ordering::SeqCst);
        }))
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
            IoResponse::Loaded { serialized, path: p } => {
                assert_eq!(p, path);
                assert!(!serialized.slide_files.is_empty());
            }
            other => panic!("expected Loaded, got {other:?}"),
        }

        assert!(wakes.load(Ordering::SeqCst) >= 2);
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
        // A directory we cannot create on macOS without privileges.
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
        // Drop ends the test — the Drop impl joins the worker thread; if
        // it hangs the test will hang (and CI catches it).
        drop(io);
    }
}
