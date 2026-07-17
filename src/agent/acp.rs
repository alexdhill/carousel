// ACP transport layer: spawn agent binary, JSON-RPC framing, reader/writer threads.
// This is the only file that touches ACP wire bytes or the child process.

use crate::agent::{AgentConfig, AgentEvent};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use tracing::{debug, error, warn};

// First request id used for user prompts. Ids 1 and 2 are reserved for the
// initialize and session/new handshake requests so they never collide.
const PROMPT_ID_BASE: u64 = 1000;

// DECK_CONTEXT
// Prepended to the first prompt of each session so a generic coding agent
// understands it is editing this app's deck (exposed as virtual files over the
// ACP fs methods) rather than hunting the real filesystem for presentation
// files. Sent exactly once per session; later prompts go through verbatim.
const DECK_CONTEXT: &str = "\
You are editing a Carousel slide deck. The deck is laid out as real files in \
your working directory — read and edit them with your normal file tools:\n\
- Read `deck/index.md` first — it lists every slide (filename, title, element \
count). This is how you discover the slide files.\n\
- Read `deck/slides/slideN.html` to see one slide's contents.\n\
- To edit a slide, read `deck/slides/slideN.html`, modify it, then write the \
WHOLE file back. Your edits are applied to the live deck when your turn ends.\n\
- `deck/format.md` lists the image asset ids you may reference.\n\n\
ELEMENT FORMAT — every visual element is ONE tag inside the slide's \
`div.slide__content`, carrying:\n\
- `data-element-id`: a unique short id (any string not already on the slide).\n\
- `data-element-type`: `text`, `shape`, or `image`.\n\
- `style`: absolute geometry in px — `left`, `top`, `width`, `height` — plus any \
CSS (color, background, font-size, border-radius, …).\n\
Both data-* attributes are mandatory; an element missing either, or with an \
unknown type, is dropped (the rest of the slide still applies). Preserve the ids \
of elements you are not changing.\n\
Text:  <div data-element-id=\"t1\" data-element-type=\"text\" \
style=\"left:80px;top:80px;width:600px;height:120px;font-size:48px;color:#111\">Hello</div>\n\
Shape: <div data-element-id=\"s1\" data-element-type=\"shape\" data-shape=\"rectangle\" \
style=\"left:80px;top:240px;width:200px;height:120px;background:#44ff88\"></div>\n\
  data-shape is one of rectangle, ellipse, rounded-rect (add data-shape-radius=\"16\"), \
or path (add data-shape-d=\"M0 0 L100 0 L50 100 Z\"). Fill/stroke come from style.\n\
Image: <div data-element-id=\"i1\" data-element-type=\"image\" data-asset-id=\"ASSET_ID\" \
style=\"left:80px;top:80px;width:320px;height:240px\"></div>\n\
  data-asset-id must be an id from deck/format.md; you cannot add a new image file.\n\n\
--- The user's request follows ---\n\n";

// __prompt_body
// Input: the per-session context flag and the raw user text. Output: the text
// to send — prefixed with DECK_CONTEXT for the first prompt of the session
// (claimed atomically), verbatim afterwards.
fn __prompt_body(context_sent: &AtomicBool, text: &str) -> String {
    if context_sent.swap(true, Ordering::SeqCst) {
        text.to_string()
    } else {
        format!("{}{}", DECK_CONTEXT, text)
    }
}

// ReaderCtx
// State the reader thread needs to drive the ACP handshake and flush queued
// prompts: the writer channel, the session cwd, the shared request-id counter,
// the shared session id, and the pending-prompt queue.
struct ReaderCtx {
    tx: Sender<String>,
    cwd: String,
    next_id: Arc<AtomicU64>,
    session_id: Arc<Mutex<Option<String>>>,
    pending: Arc<Mutex<Vec<String>>>,
    context_sent: Arc<AtomicBool>,
}

// AgentHandle
// Live session handle: owns the writer channel sender, the child process,
// the shared session_id, the shared request-id counter, and the queue of
// prompts submitted before the session was ready.
// Used to send prompts, permission replies, FS responses, and to cancel/shutdown.
pub struct AgentHandle {
    pub writer_tx: Sender<String>,
    pub child: Child,
    pub session_id: Arc<Mutex<Option<String>>>,
    pub next_id: Arc<AtomicU64>,
    pub pending: Arc<Mutex<Vec<String>>>,
    pub context_sent: Arc<AtomicBool>,
}

impl AgentHandle {
    // send_prompt
    // Input: a text prompt from the user. Output: AppResult<()> after queueing
    // a session/prompt request to the writer thread. Errors: channel closed,
    // invalid input (empty text triggers assert).
    pub fn send_prompt(&self, text: &str) -> crate::error::AppResult<()> {
        assert!(!text.is_empty(), "prompt text must not be empty");
        let session_id_str: Option<String> = self
            .session_id
            .lock()
            .map_err(|_| crate::error::AppError::IpcChannelClosed)?
            .clone();
        match session_id_str {
            Some(sid) if !sid.is_empty() => {
                let id: u64 = self.next_id.fetch_add(1, Ordering::SeqCst);
                let body: String = __prompt_body(&self.context_sent, text);
                let line: String = __frame_prompt(id, &sid, &body);
                self.writer_tx
                    .send(line)
                    .map_err(|_| crate::error::AppError::IpcChannelClosed)?;
            }
            _ => {
                self.pending
                    .lock()
                    .map_err(|_| crate::error::AppError::IpcChannelClosed)?
                    .push(text.to_string());
                debug!("prompt queued until agent session is ready");
            }
        }
        Ok(())
    }

    // send_permission_reply
    // Input: a request_id from an earlier PermissionRequest event, and a boolean
    // allow flag. Output: AppResult<()> after queueing a permission response.
    // Errors: channel closed, invalid input (empty request_id triggers assert).
    pub fn send_permission_reply(
        &self,
        request_id: &str,
        allow: bool,
    ) -> crate::error::AppResult<()> {
        assert!(!request_id.is_empty(), "request_id must not be empty");
        let option_id: &str = if allow { "allow" } else { "reject" };
        let result: serde_json::Value = serde_json::json!({
            "outcome": {
                "outcome": "selected",
                "optionId": option_id
            }
        });
        let line: String = __frame_response(request_id, result);
        self.writer_tx
            .send(line)
            .map_err(|_| crate::error::AppError::IpcChannelClosed)?;
        Ok(())
    }

    // send_fs_response
    // Input: a request_id and an arbitrary serde_json Value result.
    // Output: AppResult<()> after queueing the response. Errors: channel closed,
    // invalid input (empty request_id triggers assert).
    pub fn send_fs_response(
        &self,
        request_id: &str,
        result: serde_json::Value,
    ) -> crate::error::AppResult<()> {
        assert!(!request_id.is_empty(), "request_id must not be empty");
        let line: String = __frame_response(request_id, result);
        self.writer_tx
            .send(line)
            .map_err(|_| crate::error::AppError::IpcChannelClosed)?;
        Ok(())
    }

    // send_fs_error
    // Input: a request_id and an error message string. Output: AppResult<()>
    // after queueing a JSON-RPC error response. Errors: channel closed,
    // invalid input (empty request_id or message triggers assert).
    pub fn send_fs_error(&self, request_id: &str, message: &str) -> crate::error::AppResult<()> {
        assert!(!request_id.is_empty(), "request_id must not be empty");
        assert!(!message.is_empty(), "error message must not be empty");
        let error_obj: serde_json::Value = serde_json::json!({
            "code": -32603,
            "message": message
        });
        let line: String = serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "error": error_obj
        }))
        .unwrap_or_default();
        self.writer_tx
            .send(line)
            .map_err(|_| crate::error::AppError::IpcChannelClosed)?;
        Ok(())
    }

    // cancel
    // Input: self reference. Output: AppResult<()> after queueing a session/cancel
    // notification. This is a notification (no ID). Errors: channel closed.
    pub fn cancel(&self) -> crate::error::AppResult<()> {
        let session_id_lock = self
            .session_id
            .lock()
            .map_err(|_| crate::error::AppError::IpcChannelClosed)?;
        let session_id_str: String = session_id_lock.as_ref().cloned().unwrap_or_default();
        drop(session_id_lock);
        let params: serde_json::Value = serde_json::json!({
            "sessionId": session_id_str
        });
        let line: String = __frame_notification("session/cancel", params);
        self.writer_tx
            .send(line)
            .map_err(|_| crate::error::AppError::IpcChannelClosed)?;
        Ok(())
    }

    // shutdown
    // Input: mutable self reference. Output: AppResult<()> after killing the
    // child process. Errors: child wait/kill failures converted to string-based
    // errors; panics are caught and logged.
    pub fn shutdown(&mut self) -> crate::error::AppResult<()> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        Ok(())
    }
}

// spawn_agent
// Input: AgentConfig reference, cwd path, and a generic event callback.
// Output: AppResult<AgentHandle> with a live session ready for prompts.
// Errors: child spawn failure, JSON parsing.
// Control flow: spawn child, create channels, start reader+writer threads,
// send initialize + session/new requests, capture session_id, return handle.
pub fn spawn_agent<F>(
    config: &AgentConfig,
    cwd: &std::path::Path,
    on_event: F,
) -> crate::error::AppResult<AgentHandle>
where
    F: Fn(AgentEvent) + Send + 'static,
{
    let mut child: Child = std::process::Command::new(&config.command)
        .args(&config.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(cwd)
        .spawn()
        .map_err(|e| crate::error::AppError::Agent(format!("spawn '{}': {}", config.command, e)))?;

    let stdout: ChildStdout = child
        .stdout
        .take()
        .ok_or(crate::error::AppError::IpcChannelClosed)?;
    let stdin: ChildStdin = child
        .stdin
        .take()
        .ok_or(crate::error::AppError::IpcChannelClosed)?;
    let stderr: ChildStderr = child
        .stderr
        .take()
        .ok_or(crate::error::AppError::IpcChannelClosed)?;

    let (tx, rx): (Sender<String>, Receiver<String>) = std::sync::mpsc::channel();
    let writer_tx: Sender<String> = tx.clone();

    let session_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let next_id: Arc<AtomicU64> = Arc::new(AtomicU64::new(PROMPT_ID_BASE));
    let pending: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let context_sent: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

    let ctx: ReaderCtx = ReaderCtx {
        tx: tx.clone(),
        cwd: cwd.to_string_lossy().into_owned(),
        next_id: Arc::clone(&next_id),
        session_id: Arc::clone(&session_id),
        pending: Arc::clone(&pending),
        context_sent: Arc::clone(&context_sent),
    };
    std::thread::spawn(move || {
        __reader_loop(stdout, ctx, on_event);
    });
    std::thread::spawn(move || {
        __stderr_loop(stderr);
    });
    std::thread::spawn(move || {
        __writer_loop(rx, stdin);
    });

    // Send only `initialize`. The reader sends `session/new` after the agent
    // acks initialize, and captures the session id from its response; sending
    // both up front races the handshake and many agents reject it.
    let init_params: serde_json::Value = serde_json::json!({
        "protocolVersion": 1,
        "clientCapabilities": {
            "fs": {
                "readTextFile": true,
                "writeTextFile": true
            }
        }
    });
    let init_line: String = __frame_request(1, "initialize", init_params);
    tx.send(init_line)
        .map_err(|_| crate::error::AppError::IpcChannelClosed)?;

    Ok(AgentHandle {
        writer_tx,
        child,
        session_id,
        next_id,
        pending,
        context_sent,
    })
}

// __reader_loop
// Input: child stdout, the reader context (handshake plumbing), and the event
// callback. Output: none (runs until EOF). Control flow: loop reading lines,
// parse JSON, drive the handshake (send session/new after initialize, capture
// the session id and flush queued prompts), emit SessionReady if handshake just
// completed, classify the message, call the callback. Exits on EOF (bounded by
// stream length, no recursion).
fn __reader_loop<F>(stdout: ChildStdout, ctx: ReaderCtx, on_event: F)
where
    F: Fn(AgentEvent),
{
    let reader: BufReader<ChildStdout> = BufReader::new(stdout);
    let mut lines = reader.lines();

    loop {
        match lines.next() {
            Some(Ok(line)) => {
                if line.is_empty() {
                    continue;
                }
                // Full raw wire dump for debugging the agent process; DEBUG-only
                // (opt-in via -v 4) since it contains prompt/slide bodies.
                debug!("acp <- {}", line);
                match serde_json::from_str::<serde_json::Value>(&line) {
                    Ok(msg) => {
                        if __drive_handshake(&msg, &ctx) {
                            on_event(AgentEvent::SessionReady);
                        }
                        if let Some(evt) = __classify_message(msg) {
                            on_event(evt);
                        }
                    }
                    Err(e) => {
                        error!("json parse error: {}", e);
                    }
                }
            }
            Some(Err(e)) => {
                error!("read line error: {}", e);
                break;
            }
            None => {
                debug!("reader EOF");
                break;
            }
        }
    }
}

// __drive_handshake
// Input: one parsed inbound message and the reader context. Output: bool
// (true only when this call JUST set the session id). Side-effects on ctx.
// Control flow: when the message is the initialize ack (result carries
// protocolVersion), send session/new. When it is the session/new ack (result
// carries sessionId), store the id, flush any prompts queued before the session
// was ready, and return true. Other messages are ignored and return false.
fn __drive_handshake(msg: &serde_json::Value, ctx: &ReaderCtx) -> bool {
    let result: Option<&serde_json::Value> = msg.get("result");
    if let Some(sid) = result
        .and_then(|r| r.get("sessionId"))
        .and_then(|v| v.as_str())
    {
        if let Ok(mut lock) = ctx.session_id.lock() {
            *lock = Some(sid.to_string());
        }
        __flush_pending(ctx, sid);
        return true;
    }
    if result.and_then(|r| r.get("protocolVersion")).is_some() {
        let params: serde_json::Value = serde_json::json!({
            "cwd": ctx.cwd,
            "mcpServers": []
        });
        let line: String = __frame_request(2, "session/new", params);
        if ctx.tx.send(line).is_err() {
            error!("failed to send session/new; writer channel closed");
        }
    }
    false
}

// __flush_pending
// Input: the reader context and the now-known session id. Output: none; drains
// the pending-prompt queue, sending each as a session/prompt request with a
// fresh id. Runs once when the session becomes ready.
fn __flush_pending(ctx: &ReaderCtx, session_id: &str) {
    let queued: Vec<String> = match ctx.pending.lock() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => return,
    };
    for text in queued {
        let id: u64 = ctx.next_id.fetch_add(1, Ordering::SeqCst);
        let body: String = __prompt_body(&ctx.context_sent, &text);
        let line: String = __frame_prompt(id, session_id, &body);
        if ctx.tx.send(line).is_err() {
            error!("failed to flush queued prompt; writer channel closed");
            return;
        }
    }
}

// __stderr_loop
// Input: child stderr. Output: none (runs until EOF). Control flow: log each
// line at warn so agent diagnostics surface in the app log. Bounded by stream
// length, no recursion.
fn __stderr_loop(stderr: ChildStderr) {
    let reader: BufReader<ChildStderr> = BufReader::new(stderr);
    for line in reader.lines() {
        match line {
            Ok(l) if !l.is_empty() => warn!("agent stderr: {}", l),
            Ok(_) => {}
            Err(_) => break,
        }
    }
}

// __writer_loop
// Input: message receiver and child stdin. Output: none (runs until channel
// closes). Control flow: drain receiver, write each line + '\n', flush.
// Exits when channel is closed (bounded by sender count, no recursion).
fn __writer_loop(rx: Receiver<String>, mut stdin: ChildStdin) {
    while let Ok(line) = rx.recv() {
        // Full raw wire dump (DEBUG-only, -v 4): outbound handshake/prompts.
        debug!("acp -> {}", line);
        let full_line: String = format!("{}\n", line);
        if let Err(e) = stdin.write_all(full_line.as_bytes()) {
            error!("stdin write error: {}", e);
            break;
        }
        if let Err(e) = stdin.flush() {
            error!("stdin flush error: {}", e);
            break;
        }
    }
    debug!("writer thread exiting; channel closed");
}

// __frame_request
// Input: numeric id, method name, and params Value. Output: a compact
// JSON-RPC 2.0 request string. Control flow: serialize the object to JSON.
fn __frame_request(id: u64, method: &str, params: serde_json::Value) -> String {
    serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    }))
    .unwrap_or_default()
}

// __frame_prompt
// Input: request id, the active session id, and prompt text. Output: a
// session/prompt JSON-RPC request string. Control flow: build the params and
// delegate to __frame_request.
fn __frame_prompt(id: u64, session_id: &str, text: &str) -> String {
    let params: serde_json::Value = serde_json::json!({
        "sessionId": session_id,
        "prompt": [{"type": "text", "text": text}]
    });
    __frame_request(id, "session/prompt", params)
}

// __frame_notification
// Input: method name and params Value. Output: a JSON-RPC 2.0 notification
// string (no id field). Control flow: serialize the object.
fn __frame_notification(method: &str, params: serde_json::Value) -> String {
    serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params
    }))
    .unwrap_or_default()
}

// __frame_response
// Input: request_id string and result Value. Output: a JSON-RPC 2.0 response
// string. Control flow: serialize the object.
fn __frame_response(id_str: &str, result: serde_json::Value) -> String {
    serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id_str,
        "result": result
    }))
    .unwrap_or_default()
}

// __classify_session_update
// Input: params object from a session/update notification. Output:
// Some(AgentEvent) if the update should be exposed, None otherwise.
// Control flow: extract update.sessionUpdate to determine the type, then read
// fields defensively with .get().and_then() chains and sensible fallbacks.
// Maps: agent_message_chunk -> StreamChunk, agent_thought_chunk -> Thought,
// tool_call -> ToolStatus with pending, tool_call_update -> ToolStatus with
// in_progress, unknown -> None.
fn __classify_session_update(params: &serde_json::Value) -> Option<AgentEvent> {
    let update: &serde_json::Value = params.get("update")?;
    let session_update: Option<&str> = update.get("sessionUpdate").and_then(|su| su.as_str());

    match session_update {
        Some("agent_message_chunk") => Some(AgentEvent::StreamChunk {
            role: "assistant".to_string(),
            text: __update_text(update),
            final_chunk: false,
        }),
        Some("agent_thought_chunk") => Some(AgentEvent::Thought {
            text: __update_text(update),
        }),
        Some("tool_call") => Some(__tool_status(update, "pending")),
        Some("tool_call_update") => Some(__tool_status(update, "in_progress")),
        _ => None,
    }
}

// __update_text
// Input: a session/update `update` object. Output: its content text, reading
// `content.text` then falling back to a bare string `content`, else empty.
fn __update_text(update: &serde_json::Value) -> String {
    update
        .get("content")
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .or_else(|| update.get("content").and_then(|c| c.as_str()))
        .unwrap_or("")
        .to_string()
}

// __tool_status
// Input: a session/update `update` object and the default status to use when
// the update omits one. Output: an AgentEvent::ToolStatus with id/title/status
// read defensively.
fn __tool_status(update: &serde_json::Value, default_status: &str) -> AgentEvent {
    let id: String = update
        .get("toolCallId")
        .and_then(|i| i.as_str())
        .unwrap_or("")
        .to_string();
    let title: String = update
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let status: String = update
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or(default_status)
        .to_string();
    AgentEvent::ToolStatus { id, title, status }
}

// __classify_message
// Input: a parsed JSON-RPC message. Output: Some(AgentEvent) if the message
// should be exposed, None if it should be ignored. Control flow: check for
// method/id/result/error fields and route to appropriate event variant.
// Handling:
//   - method + id = request from agent (FsRead/FsWrite/PermissionRequest)
//   - method no id = notification (StreamChunk for agent_message_chunk)
//   - id + result/error = response (TurnEnded for stopReason, Failed for error)
fn __classify_message(msg: serde_json::Value) -> Option<AgentEvent> {
    let has_method: bool = msg.get("method").is_some();
    let has_id: bool = msg.get("id").is_some();
    let has_result: bool = msg.get("result").is_some();
    let has_error: bool = msg.get("error").is_some();

    if has_method && has_id {
        let method_str: Option<&str> = msg.get("method").and_then(|m| m.as_str());
        let id_str: String = match msg.get("id") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Number(n)) => n.to_string(),
            _ => return None,
        };

        match method_str {
            Some("fs/read_text_file") => {
                let path: String = msg
                    .get("params")
                    .and_then(|p| p.get("path"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                return Some(AgentEvent::FsRead {
                    request_id: id_str,
                    path,
                });
            }
            Some("fs/write_text_file") => {
                let path: String = msg
                    .get("params")
                    .and_then(|p| p.get("path"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                let contents: String = msg
                    .get("params")
                    .and_then(|p| p.get("content"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                return Some(AgentEvent::FsWrite {
                    request_id: id_str,
                    path,
                    contents,
                });
            }
            Some("session/request_permission") => {
                let path: String = msg
                    .get("params")
                    .and_then(|p| p.get("path"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                let summary: String = msg
                    .get("params")
                    .and_then(|p| p.get("toolCall"))
                    .and_then(|tc| tc.get("title"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("permission requested")
                    .to_string();
                return Some(AgentEvent::PermissionRequest {
                    request_id: id_str,
                    path,
                    summary,
                });
            }
            _ => return None,
        }
    }

    if has_method && !has_id {
        let method_str: Option<&str> = msg.get("method").and_then(|m| m.as_str());
        if method_str == Some("session/update") {
            let params: Option<&serde_json::Value> = msg.get("params");
            if let Some(p) = params {
                return __classify_session_update(p);
            }
        }
        return None;
    }

    if has_id && (has_result || has_error) && !has_method {
        if has_error {
            let err: Option<&serde_json::Value> = msg.get("error");
            let message: &str = err
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            let code: Option<i64> = err.and_then(|e| e.get("code")).and_then(|c| c.as_i64());
            let data: Option<String> = err
                .and_then(|e| e.get("data"))
                .map(|d| d.to_string())
                .filter(|d| d != "null");
            let mut full: String = match code {
                Some(c) => format!("{} (code {})", message, c),
                None => message.to_string(),
            };
            if let Some(d) = data {
                full.push_str(": ");
                full.push_str(&d);
            }
            return Some(AgentEvent::Failed { message: full });
        }

        if has_result
            && msg
                .get("result")
                .and_then(|r| r.get("stopReason"))
                .is_some()
        {
            return Some(AgentEvent::TurnEnded);
        }
        return None;
    }

    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn test_frame_request() {
        let params: serde_json::Value = serde_json::json!({
            "protocolVersion": 1
        });
        let line: String = __frame_request(1, "initialize", params);
        let parsed: serde_json::Value =
            serde_json::from_str(&line).expect("line must be valid json");
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["id"], 1);
        assert_eq!(parsed["method"], "initialize");
        assert_eq!(parsed["params"]["protocolVersion"], 1);
    }

    #[test]
    fn test_classify_fs_write() {
        let msg: serde_json::Value = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "req123",
            "method": "fs/write_text_file",
            "params": {
                "path": "/deck/slides/slide1.html",
                "content": "<div>test</div>"
            }
        });
        let evt: Option<AgentEvent> = __classify_message(msg);
        match evt {
            Some(AgentEvent::FsWrite {
                request_id,
                path,
                contents,
            }) => {
                assert_eq!(request_id, "req123");
                assert_eq!(path, "/deck/slides/slide1.html");
                assert_eq!(contents, "<div>test</div>");
            }
            _ => panic!("expected FsWrite event"),
        }
    }

    #[test]
    fn test_classify_stream_chunk() {
        let msg: serde_json::Value = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {
                        "text": "hello world"
                    }
                }
            }
        });
        let evt: Option<AgentEvent> = __classify_message(msg);
        match evt {
            Some(AgentEvent::StreamChunk {
                role,
                text,
                final_chunk,
            }) => {
                assert_eq!(role, "assistant");
                assert_eq!(text, "hello world");
                assert!(!final_chunk);
            }
            _ => panic!("expected StreamChunk event"),
        }
    }

    #[test]
    fn test_classify_error_response() {
        let msg: serde_json::Value = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "req456",
            "error": {
                "code": -32600,
                "message": "invalid request"
            }
        });
        let evt: Option<AgentEvent> = __classify_message(msg);
        match evt {
            Some(AgentEvent::Failed { message }) => {
                assert_eq!(message, "invalid request (code -32600)");
            }
            _ => panic!("expected Failed event"),
        }
    }

    #[test]
    fn prompt_body_injects_context_once() {
        let flag = AtomicBool::new(false);
        let first = __prompt_body(&flag, "edit slide 2");
        assert!(first.contains("Carousel slide deck"));
        assert!(first.contains("deck/slides/"));
        assert!(first.ends_with("edit slide 2"));
        let second = __prompt_body(&flag, "now slide 3");
        assert_eq!(second, "now slide 3");
    }

    #[test]
    fn handshake_sequences_and_flushes_pending() {
        let (tx, rx): (Sender<String>, Receiver<String>) = std::sync::mpsc::channel();
        let ctx = ReaderCtx {
            tx,
            cwd: "/tmp".to_string(),
            next_id: Arc::new(AtomicU64::new(PROMPT_ID_BASE)),
            session_id: Arc::new(Mutex::new(None)),
            pending: Arc::new(Mutex::new(vec!["hello".to_string()])),
            context_sent: Arc::new(AtomicBool::new(false)),
        };

        // initialize ack -> should emit session/new, not touch session id yet.
        let init_ack = serde_json::json!({"id": 1, "result": {"protocolVersion": 1}});
        __drive_handshake(&init_ack, &ctx);
        let line1: String = rx.recv().unwrap();
        assert!(line1.contains("\"session/new\""));
        assert!(line1.contains("/tmp"));
        assert!(ctx.session_id.lock().unwrap().is_none());

        // session/new ack -> store id and flush the queued prompt.
        let sess_ack = serde_json::json!({"id": 2, "result": {"sessionId": "sess_1"}});
        __drive_handshake(&sess_ack, &ctx);
        assert_eq!(ctx.session_id.lock().unwrap().as_deref(), Some("sess_1"));
        let line2: String = rx.recv().unwrap();
        assert!(line2.contains("\"session/prompt\""));
        assert!(line2.contains("sess_1"));
        assert!(line2.contains("hello"));
        assert!(ctx.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn test_classify_turn_ended() {
        let msg: serde_json::Value = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "stopReason": "end_turn"
            }
        });
        let evt: Option<AgentEvent> = __classify_message(msg);
        match evt {
            Some(AgentEvent::TurnEnded) => {
                // success
            }
            _ => panic!("expected TurnEnded event"),
        }
    }

    #[test]
    fn test_classify_agent_thought_chunk() {
        let msg: serde_json::Value = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "agent_thought_chunk",
                    "content": {
                        "text": "analyzing the problem"
                    }
                }
            }
        });
        let evt: Option<AgentEvent> = __classify_message(msg);
        match evt {
            Some(AgentEvent::Thought { text }) => {
                assert_eq!(text, "analyzing the problem");
            }
            _ => panic!("expected Thought event"),
        }
    }

    #[test]
    fn test_classify_tool_call() {
        let msg: serde_json::Value = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "call_123",
                    "title": "read_file",
                    "kind": "function",
                    "status": "pending"
                }
            }
        });
        let evt: Option<AgentEvent> = __classify_message(msg);
        match evt {
            Some(AgentEvent::ToolStatus { id, title, status }) => {
                assert_eq!(id, "call_123");
                assert_eq!(title, "read_file");
                assert_eq!(status, "pending");
            }
            _ => panic!("expected ToolStatus event"),
        }
    }

    #[test]
    fn test_classify_tool_call_update() {
        let msg: serde_json::Value = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call_456",
                    "title": "write_file",
                    "kind": "function",
                    "status": "completed"
                }
            }
        });
        let evt: Option<AgentEvent> = __classify_message(msg);
        match evt {
            Some(AgentEvent::ToolStatus { id, title, status }) => {
                assert_eq!(id, "call_456");
                assert_eq!(title, "write_file");
                assert_eq!(status, "completed");
            }
            _ => panic!("expected ToolStatus event"),
        }
    }

    #[test]
    fn handshake_returns_true_when_session_set() {
        let (tx, _rx): (Sender<String>, Receiver<String>) = std::sync::mpsc::channel();
        let ctx = ReaderCtx {
            tx,
            cwd: "/tmp".to_string(),
            next_id: Arc::new(AtomicU64::new(PROMPT_ID_BASE)),
            session_id: Arc::new(Mutex::new(None)),
            pending: Arc::new(Mutex::new(vec![])),
            context_sent: Arc::new(AtomicBool::new(false)),
        };

        let init_ack = serde_json::json!({"id": 1, "result": {"protocolVersion": 1}});
        let result: bool = __drive_handshake(&init_ack, &ctx);
        assert!(!result, "initialize ack should return false");

        let sess_ack = serde_json::json!({"id": 2, "result": {"sessionId": "sess_1"}});
        let result: bool = __drive_handshake(&sess_ack, &ctx);
        assert!(result, "session ack should return true");
    }
}
