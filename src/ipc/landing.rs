// Landing-window IPC payloads.
//
// A small protocol between Rust and the dedicated landing webview
// (`assets/landing.*`), independent of the editor's IpcMessage envelope.
// Inbound (JS -> Rust) controls are internally tagged on `kind` (like
// PresentInbound) so landing.js posts a flat object, e.g.
// {"kind":"OpenTemplate","theme_id":"light","layout_id":"hero"}. Outbound
// (Rust -> JS) is the single LandingData payload, serialized and delivered via
// `window.__landing.receive(...)`.

use serde::{Deserialize, Serialize};

// LandingInbound
// Controls the landing webview posts: a one-shot Ready (asking for data), the
// three Open intents, and Cancel.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum LandingInbound {
    Ready,
    OpenTemplate { theme_id: String, layout_id: String },
    OpenRecent { path: String },
    OpenDefault,
    Cancel,
}

// LandingRecent
// One recents card: bundle path, display title, last-modified unix seconds.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct LandingRecent {
    pub path: String,
    pub title: String,
    pub modified: u64,
}

// LandingTemplate
// One layout card: theme + layout identity, display names, and the three
// palette colours the card paints its proportioned preview with.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct LandingTemplate {
    pub theme_id: String,
    pub theme_name: String,
    pub layout_id: String,
    pub layout_name: String,
    pub background: String,
    pub foreground: String,
    pub accent: String,
}

// LandingData
// The payload sent to the landing webview on Ready: the recents row and the
// layouts row.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct LandingData {
    pub recents: Vec<LandingRecent>,
    pub templates: Vec<LandingTemplate>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn round_trip(value: &LandingInbound) -> LandingInbound {
        let json = serde_json::to_string(value).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn inbound_variants_roundtrip() {
        for v in [
            LandingInbound::Ready,
            LandingInbound::OpenTemplate { theme_id: "dark".into(), layout_id: "hero".into() },
            LandingInbound::OpenRecent { path: "/x.slidedeck".into() },
            LandingInbound::OpenDefault,
            LandingInbound::Cancel,
        ] {
            assert_eq!(round_trip(&v), v);
        }
    }

    #[test]
    fn inbound_is_kind_tagged() {
        let json = serde_json::to_string(&LandingInbound::OpenTemplate {
            theme_id: "light".into(),
            layout_id: "title".into(),
        })
        .unwrap();
        assert!(json.contains("\"kind\":\"OpenTemplate\""));
        assert!(json.contains("\"theme_id\":\"light\""));
    }

    #[test]
    fn data_roundtrips() {
        let data = LandingData {
            recents: vec![LandingRecent {
                path: "/a.slidedeck".into(),
                title: "a".into(),
                modified: 7,
            }],
            templates: vec![LandingTemplate {
                theme_id: "light".into(),
                theme_name: "Light".into(),
                layout_id: "title".into(),
                layout_name: "Title".into(),
                background: "#ffffff".into(),
                foreground: "#1a1a1a".into(),
                accent: "#f19035".into(),
            }],
        };
        let json = serde_json::to_string(&data).unwrap();
        let back: LandingData = serde_json::from_str(&json).unwrap();
        assert_eq!(back, data);
    }
}
