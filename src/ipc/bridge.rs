// IPC bridge.
//
// `WebviewSender` owns the Wry WebView and is the single point of egress from
// Rust to JS. All sends wrap the payload in an IpcMessage envelope, serialize
// to JSON, and invoke `window.__deck.receive(...)` inside the webview.
//
// Incoming messages do not flow through this file. They are pushed onto an
// mpsc channel by the IPC handler closure registered on the WebViewBuilder
// (see `main.rs`), and the Tao event loop drains the channel into
// `ApplicationCore::handle_ipc` on the main thread.

use crate::error::{AppError, AppResult};
use crate::ipc::{IpcMessage, MessageKind};
use tracing::{debug, error};
use wry::WebView;

// WebviewSender
// Holds the WebView; not Send/Sync. Used only on the main thread.
pub struct WebviewSender {
    webview: WebView,
}

impl WebviewSender {
    // new
    // Inputs: an already-built Wry WebView.
    // Output: a sender owning that webview.
    pub fn new(webview: WebView) -> Self {
        Self { webview }
    }

    // send
    // Inputs: a MessageKind to push to the webview.
    // Output: Ok(()) on success.
    // Errors: serde failures or wry evaluate_script failures.
    // Dataflow: wrap in envelope -> JSON-encode -> JS-escape via JSON
    // stringify-of-string -> evaluate the snippet
    //   `window.__deck.receive(<escaped json>)`
    // in the webview's main world.
    pub fn send(&self, kind: MessageKind) -> AppResult<()> {
        let envelope: IpcMessage = IpcMessage::new(kind);
        let json: String = serde_json::to_string(&envelope)?;
        assert!(!json.is_empty(), "serialized envelope is empty");
        let escaped: String = escape_for_js(&json);
        let script: String = format!("window.__deck.receive({});", escaped);
        debug!(id = %envelope.id, "ipc -> webview");
        if let Err(e) = self.webview.evaluate_script(&script) {
            error!("evaluate_script failed: {}", e);
            return Err(AppError::from(e));
        }
        Ok(())
    }
}

// escape_for_js
// Inputs: an arbitrary UTF-8 string.
// Output: a JS source-level string literal containing the same value,
// with all quoting and escapes applied. Wraps the input in double quotes.
// Dataflow: serde_json::to_string of a `&str` yields a valid JS string
// literal (JSON string syntax is a strict subset of JS string syntax).
fn escape_for_js(s: &str) -> String {
    assert!(s.len() <= usize::MAX / 2, "input too large to escape");
    serde_json::to_string(s).unwrap_or_else(|_| String::from("\"\""))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn escape_for_js_quotes_and_escapes() {
        let s = r#"hello "world" with \ slash and ' quote"#;
        let out = escape_for_js(s);
        // Output is a JS string literal: starts and ends with double quotes.
        assert!(out.starts_with('"'));
        assert!(out.ends_with('"'));
        // Re-parse back to verify lossless round-trip.
        let back: String = serde_json::from_str(&out).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn escape_for_js_handles_unicode_separators() {
        // U+2028 / U+2029 are valid in JSON strings but historically broke JS
        // parsers. serde_json escapes them to   /  .
        let s = "a\u{2028}b\u{2029}c";
        let out = escape_for_js(s);
        let back: String = serde_json::from_str(&out).unwrap();
        assert_eq!(back, s);
    }
}
