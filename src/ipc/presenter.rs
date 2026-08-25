use crate::ipc::ElementId;
use serde::{Deserialize, Serialize};

/// PresenterUpdatePayload — everything the presenter console renders for one
/// cursor position: the current slide, the slide that comes next, the speaker
/// notes, and the deck geometry the console scales its previews against.
///
/// `next_html` is empty on the last slide. `current_hidden` / `next_hidden`
/// carry the element ids that are not yet revealed, so the previews match what
/// the audience sees rather than showing every animation step at once.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PresenterUpdatePayload {
    pub index: usize,
    pub count: usize,
    pub width: u32,
    pub height: u32,
    pub current_html: String,
    pub next_html: String,

    #[serde(default)]
    pub current_hidden: Vec<ElementId>,

    #[serde(default)]
    pub next_hidden: Vec<ElementId>,
    pub notes: String,
    pub theme_css: String,
    pub globals_css: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn sample() -> PresenterUpdatePayload {
        PresenterUpdatePayload {
            index: 2,
            count: 5,
            width: 1920,
            height: 1080,
            current_html: "<section id=\"a\"/>".into(),
            next_html: "<section id=\"b\"/>".into(),
            current_hidden: vec!["el_a".into()],
            next_hidden: vec!["el_b".into(), "el_c".into()],
            notes: "say the thing".into(),
            theme_css: ".x{}".into(),
            globals_css: ":root{}".into(),
        }
    }

    #[test]
    fn presenter_update_payload_roundtrips() {
        let p = sample();
        let json = serde_json::to_string(&p).unwrap();
        let back: PresenterUpdatePayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn hidden_lists_default_to_empty_when_absent() {
        let raw = r#"{"index":0,"count":1,"width":1920,"height":1080,
            "current_html":"<section/>","next_html":"","notes":"",
            "theme_css":"","globals_css":""}"#;
        let back: PresenterUpdatePayload = serde_json::from_str(raw).unwrap();
        assert!(back.current_hidden.is_empty());
        assert!(back.next_hidden.is_empty());
    }

    #[test]
    fn presenter_update_roundtrips_through_envelope() {
        use crate::ipc::{IpcMessage, MessageKind};
        let env = IpcMessage::new(MessageKind::PresenterUpdate(sample()));
        let json = serde_json::to_string(&env).unwrap();
        let back: IpcMessage = serde_json::from_str(&json).unwrap();
        match back.kind {
            MessageKind::PresenterUpdate(p) => assert_eq!(p, sample()),
            other => panic!("expected PresenterUpdate, got {other:?}"),
        }
    }
}
