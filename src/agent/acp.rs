// ACP transport layer: spawn agent binary, JSON-RPC framing, reader/writer threads.
// This is the only file that touches ACP wire bytes or the child process.

use crate::agent::{AgentConfig, AgentEvent};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use tracing::{debug, error};

// AgentHandle
// Live session handle: owns the writer channel sender, the child process,
// the shared session_id, and the atomic counter for next request ID.
// Used to send prompts, permission replies, FS responses, and to cancel/shutdown.
pub struct AgentHandle {
    pub writer_tx: Sender<String>,
    pub child: Child,
    pub session_id: Arc<Mutex<Option<String>>>,
    pub next_id: AtomicU64,
}

impl AgentHandle {
    // send_prompt
    // Input: a text prompt from the user. Output: AppResult<()> after queueing
    // a session/prompt request to the writer thread. Errors: channel closed,
    // invalid input (empty text triggers assert).
    pub fn send_prompt(&self, text: &str) -> crate::error::AppResult<()> {
        assert!(!text.is_empty(), "prompt text must not be empty");
        let id: u64 = self.next_id.fetch_add(1, Ordering::SeqCst);
        let session_id_lock = self
            .session_id
            .lock()
            .map_err(|_| crate::error::AppError::IpcChannelClosed)?;
        let session_id_str: String = session_id_lock
            .as_ref()
            .cloned()
            .unwrap_or_default();
        drop(session_id_lock);
        let params: serde_json::Value = serde_json::json!({
            "sessionId": session_id_str,
            "prompt": [{"type": "text", "text": text}]
        });
        let line: String = __frame_request(id, "session/prompt", params);
        self.writer_tx.send(line)
            .map_err(|_| crate::error::AppError::IpcChannelClosed)?;
        Ok(())
    }

    // send_permission_reply
    // Input: a request_id from an earlier PermissionRequest event, and a boolean
    // allow flag. Output: AppResult<()> after queueing a permission response.
    // Errors: channel closed, invalid input (empty request_id triggers assert).
    pub fn send_permission_reply(&self, request_id: &str, allow: bool) -> crate::error::AppResult<()> {
        assert!(!request_id.is_empty(), "request_id must not be empty");
        let option_id: &str = if allow { "allow" } else { "reject" };
        let result: serde_json::Value = serde_json::json!({
            "outcome": {
                "outcome": "selected",
                "optionId": option_id
            }
        });
        let line: String = __frame_response(request_id, result);
        self.writer_tx.send(line)
            .map_err(|_| crate::error::AppError::IpcChannelClosed)?;
        Ok(())
    }

    // send_fs_response
    // Input: a request_id and an arbitrary serde_json Value result.
    // Output: AppResult<()> after queueing the response. Errors: channel closed,
    // invalid input (empty request_id triggers assert).
    pub fn send_fs_response(&self, request_id: &str, result: serde_json::Value) -> crate::error::AppResult<()> {
        assert!(!request_id.is_empty(), "request_id must not be empty");
        let line: String = __frame_response(request_id, result);
        self.writer_tx.send(line)
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
        })).unwrap_or_default();
        self.writer_tx.send(line)
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
        let session_id_str: String = session_id_lock
            .as_ref()
            .cloned()
            .unwrap_or_default();
        drop(session_id_lock);
        let params: serde_json::Value = serde_json::json!({
            "sessionId": session_id_str
        });
        let line: String = __frame_notification("session/cancel", params);
        self.writer_tx.send(line)
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
        .map_err(|e| {
            crate::error::AppError::Agent(format!("spawn '{}': {}", config.command, e))
        })?;

    let stdout: ChildStdout = child
        .stdout
        .take()
        .ok_or(crate::error::AppError::IpcChannelClosed)?;
    let stdin: ChildStdin = child
        .stdin
        .take()
        .ok_or(crate::error::AppError::IpcChannelClosed)?;

    let (tx, rx): (Sender<String>, Receiver<String>) = std::sync::mpsc::channel();
    let writer_tx: Sender<String> = tx.clone();

    let session_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let next_id: AtomicU64 = AtomicU64::new(1);

    let session_id_reader: Arc<Mutex<Option<String>>> = Arc::clone(&session_id);
    std::thread::spawn(move || {
        __reader_loop(stdout, session_id_reader, on_event);
    });

    std::thread::spawn(move || {
        __writer_loop(rx, stdin);
    });

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

    let cwd_str: String = cwd.to_string_lossy().into_owned();
    let session_params: serde_json::Value = serde_json::json!({
        "cwd": cwd_str,
        "mcpServers": []
    });
    let session_line: String = __frame_request(2, "session/new", session_params);
    tx.send(session_line)
        .map_err(|_| crate::error::AppError::IpcChannelClosed)?;

    Ok(AgentHandle {
        writer_tx,
        child,
        session_id,
        next_id,
    })
}

// __reader_loop
// Input: child stdout, session_id Arc, and event callback. Output: none (runs
// until EOF). Control flow: loop reading lines, parse JSON, extract sessionId
// if present, classify message, call callback with AgentEvent. Exits on EOF
// (bounded by stream length, no recursion).
fn __reader_loop<F>(
    stdout: ChildStdout,
    session_id: Arc<Mutex<Option<String>>>,
    on_event: F,
) where
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
                match serde_json::from_str::<serde_json::Value>(&line) {
                    Ok(msg) => {
                        if let Some(id_str) = msg
                            .get("result")
                            .and_then(|r| r.get("sessionId"))
                            .and_then(|v| v.as_str())
                            && let Ok(mut lock) = session_id.lock()
                        {
                            *lock = Some(id_str.to_string());
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

// __writer_loop
// Input: message receiver and child stdin. Output: none (runs until channel
// closes). Control flow: drain receiver, write each line + '\n', flush.
// Exits when channel is closed (bounded by sender count, no recursion).
fn __writer_loop(rx: Receiver<String>, mut stdin: ChildStdin) {
    while let Ok(line) = rx.recv() {
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
    })).unwrap_or_default()
}

// __frame_notification
// Input: method name and params Value. Output: a JSON-RPC 2.0 notification
// string (no id field). Control flow: serialize the object.
fn __frame_notification(method: &str, params: serde_json::Value) -> String {
    serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params
    })).unwrap_or_default()
}

// __frame_response
// Input: request_id string and result Value. Output: a JSON-RPC 2.0 response
// string. Control flow: serialize the object.
fn __frame_response(id_str: &str, result: serde_json::Value) -> String {
    serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id_str,
        "result": result
    })).unwrap_or_default()
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
                let path: String = msg.get("params")
                    .and_then(|p| p.get("path"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                return Some(AgentEvent::FsRead { request_id: id_str, path });
            }
            Some("fs/write_text_file") => {
                let path: String = msg.get("params")
                    .and_then(|p| p.get("path"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                let contents: String = msg.get("params")
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
                let path: String = msg.get("params")
                    .and_then(|p| p.get("path"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                let summary: String = msg.get("params")
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
            let session_update: Option<&str> = msg.get("params")
                .and_then(|p| p.get("update"))
                .and_then(|u| u.get("sessionUpdate"))
                .and_then(|su| su.as_str());

            if session_update == Some("agent_message_chunk") {
                let text: String = msg.get("params")
                    .and_then(|p| p.get("update"))
                    .and_then(|u| u.get("content"))
                    .and_then(|c| c.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                return Some(AgentEvent::StreamChunk {
                    role: "assistant".to_string(),
                    text,
                    final_chunk: false,
                });
            }
        }
        return None;
    }

    if has_id && (has_result || has_error) && !has_method {
        if has_error {
            let error_msg: String = msg.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error")
                .to_string();
            return Some(AgentEvent::Failed { message: error_msg });
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
        let parsed: serde_json::Value = serde_json::from_str(&line)
            .expect("line must be valid json");
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
            Some(AgentEvent::FsWrite { request_id, path, contents }) => {
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
            Some(AgentEvent::StreamChunk { role, text, final_chunk }) => {
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
                assert_eq!(message, "invalid request");
            }
            _ => panic!("expected Failed event"),
        }
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
}
