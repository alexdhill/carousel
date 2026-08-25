use crate::deck::animation::{AnimationIterations, PropertyTarget};
use crate::ipc::{ElementId, SlideId};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PresentInitPayload {
    pub animation_keyframes_css: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PresentSlidePayload {
    pub slide_id: SlideId,
    pub slide_html: String,
    pub theme_css: String,
    pub globals_css: String,

    #[serde(default)]
    pub transition: Option<crate::deck::SlideTransition>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AnimateInstruction {
    pub element_id: ElementId,
    pub keyframe: String,

    #[serde(default)]
    pub targets: Vec<PropertyTarget>,
    pub duration_ms: u32,
    pub delay_ms: u32,
    pub easing: String,
    pub iterations: AnimationIterations,
    pub ends_hidden: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RevealPayload {
    pub slide_id: SlideId,
    pub hidden: Vec<ElementId>,
    pub shown: Vec<ElementId>,
    pub animate: Vec<AnimateInstruction>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum PresentInbound {
    Ready,
    Advance,
    Back,
    Exit,

    PresenterReady,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::animation::AnimationIterations;

    fn round_trip<T>(value: &T) -> T
    where
        T: serde::Serialize + for<'de> serde::Deserialize<'de>,
    {
        let json = serde_json::to_string(value).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn present_init_payload_roundtrips() {
        let p = PresentInitPayload {
            animation_keyframes_css: "@keyframes appear{}".into(),
            width: 1920,
            height: 1080,
        };
        assert_eq!(round_trip(&p), p);
    }

    #[test]
    fn present_slide_payload_roundtrips() {
        let p = PresentSlidePayload {
            slide_id: "s1".into(),
            slide_html: "<section/>".into(),
            theme_css: ".x{}".into(),
            globals_css: ":root{}".into(),
            transition: Some(crate::deck::SlideTransition {
                kind: crate::deck::TransitionKind::Push,
                duration_ms: 500,
                easing: "ease-out".into(),
            }),
        };
        assert_eq!(round_trip(&p), p);
    }

    #[test]
    fn reveal_payload_roundtrips_with_all_buckets() {
        let p = RevealPayload {
            slide_id: "s1".into(),
            hidden: vec!["a".into()],
            shown: vec!["b".into(), "c".into()],
            animate: vec![AnimateInstruction {
                element_id: "d".into(),
                keyframe: "fly-in-left".into(),
                targets: Vec::new(),
                duration_ms: 500,
                delay_ms: 250,
                easing: "ease".into(),
                iterations: AnimationIterations::Count(1),
                ends_hidden: false,
            }],
        };
        assert_eq!(round_trip(&p), p);
    }

    #[test]
    fn animate_instruction_carries_infinite_iterations_and_exit_flag() {
        let a = AnimateInstruction {
            element_id: "d".into(),
            keyframe: "pulse".into(),
            targets: Vec::new(),
            duration_ms: 600,
            delay_ms: 0,
            easing: "linear".into(),
            iterations: AnimationIterations::Infinite,
            ends_hidden: true,
        };
        let back = round_trip(&a);
        assert_eq!(back, a);
        assert!(back.ends_hidden);
        assert_eq!(back.iterations, AnimationIterations::Infinite);
    }

    #[test]
    fn present_message_kinds_roundtrip_through_envelope() {
        use crate::ipc::{IpcMessage, MessageKind};
        let msgs = [
            MessageKind::PresentInit(PresentInitPayload {
                animation_keyframes_css: "@keyframes appear{}".into(),
                width: 1920,
                height: 1080,
            }),
            MessageKind::PresentSlide(PresentSlidePayload {
                slide_id: "s1".into(),
                slide_html: "<section/>".into(),
                theme_css: ".x{}".into(),
                globals_css: ":root{}".into(),
                transition: None,
            }),
            MessageKind::PresentReveal(RevealPayload {
                slide_id: "s1".into(),
                hidden: vec!["a".into()],
                shown: vec!["b".into()],
                animate: vec![],
            }),
        ];
        for kind in msgs {
            let env = IpcMessage::new(kind.clone());
            let json = serde_json::to_string(&env).unwrap();
            let back: IpcMessage = serde_json::from_str(&json).unwrap();

            assert_eq!(
                std::mem::discriminant(&back.kind),
                std::mem::discriminant(&kind)
            );
        }
    }

    #[test]
    fn present_inbound_controls_parse_from_kind_tagged_json() {
        let cases = [
            (r#"{"kind":"Ready"}"#, PresentInbound::Ready),
            (r#"{"kind":"Advance"}"#, PresentInbound::Advance),
            (r#"{"kind":"Back"}"#, PresentInbound::Back),
            (r#"{"kind":"Exit"}"#, PresentInbound::Exit),
            (
                r#"{"kind":"PresenterReady"}"#,
                PresentInbound::PresenterReady,
            ),
        ];
        for (raw, want) in cases {
            let got: PresentInbound = serde_json::from_str(raw).unwrap();
            assert_eq!(got, want);
        }
    }
}
