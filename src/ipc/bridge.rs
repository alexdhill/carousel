use crate::error::{AppError, AppResult};
use crate::ipc::{IpcMessage, MessageKind};
use tracing::{debug, error};
use wry::WebView;

pub struct WebviewSender {
    webview: WebView,
}

impl WebviewSender {

    pub fn new(webview: WebView) -> Self {
        Self { webview }
    }

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

        assert!(out.starts_with('"'));
        assert!(out.ends_with('"'));

        let back: String = serde_json::from_str(&out).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn escape_for_js_handles_unicode_separators() {

        let s = "a\u{2028}b\u{2029}c";
        let out = escape_for_js(s);
        let back: String = serde_json::from_str(&out).unwrap();
        assert_eq!(back, s);
    }
}
