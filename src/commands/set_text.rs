use crate::commands::{Command, CommandError, CommandOutput, resolve_canvas_mut};
use crate::deck::element::{ElementContent, RichText};
use crate::deck::{Canvas, CanvasTarget, ElementId, SlideId};
use crate::ipc::Patch;

#[derive(Debug, Clone)]
pub struct SetTextContent {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub new_content: RichText,
}

impl Command for SetTextContent {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.target.id().is_empty(),
            "SetTextContent: target id is empty"
        );
        assert!(
            !self.element_id.is_empty(),
            "SetTextContent: element_id is empty"
        );
        let count: usize = deck.slide_order.len();
        let number: usize = match &self.target {
            CanvasTarget::Slide(id) => deck
                .slide_order
                .iter()
                .position(|s| s == id)
                .map(|p| p + 1)
                .unwrap_or(1),
            _ => 1,
        };
        let canvas = resolve_canvas_mut(deck, &self.target)?;
        let element = canvas
            .find_element_mut(&self.element_id)
            .ok_or_else(|| CommandError::ElementNotFound(self.element_id.clone()))?;

        let prev_content: RichText = match &element.content {
            ElementContent::Text(rt) => rt.clone(),
            _ => {
                return Err(CommandError::InvalidOperation(format!(
                    "SetTextContent on non-text element {}",
                    self.element_id
                )));
            }
        };

        element.content = ElementContent::Text(self.new_content.clone());
        element.placeholder = false;
        canvas.mark_dirty();
        canvas.invalidate_index();

        let inverse: SetTextContent = SetTextContent {
            target: self.target.clone(),
            element_id: self.element_id.clone(),
            new_content: prev_content,
        };

        let raw: &str = self.new_content.plain.as_str();
        let ctx: crate::html::serialize::RenderCtx = crate::html::serialize::RenderCtx {
            number,
            count,
            date: crate::html::serialize::today_ymd(),
        };
        // `SetText` writes textContent and would flatten any markup, so a
        // formatted body has to go over as inner HTML instead.
        let patch: Patch = if self.new_content.is_plain() {
            Patch::SetText {
                element_id: self.element_id.clone(),
                text: crate::html::serialize::resolve_tokens(raw, &ctx),
                src: if raw.contains("${") {
                    Some(raw.to_string())
                } else {
                    None
                },
            }
        } else {
            // ponytail: SetInnerHtml carries no `src`, so a formatted body that
            // also contains a ${token} loses the raw text the editor would
            // restore. Widen the patch if tokens and inline marks need to mix.
            let opts: crate::html::serialize::RenderOpts = crate::html::serialize::RenderOpts {
                ctx: Some(ctx),
                hide_placeholders: false,
                min_element_size: 0.0,
            };
            Patch::SetInnerHtml {
                element_id: self.element_id.clone(),
                html: crate::html::serialize::serialize_rich_body(&self.new_content, &opts),
            }
        };

        Ok(CommandOutput {
            patches: vec![patch],
            inverse: Box::new(inverse),
            dirty_targets: vec![self.target.clone()],
            manifest_dirty: false,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Edit Text"
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;
    use crate::deck::builders::{group_element, image_element};

    fn bold_head(text: &str, end: usize) -> RichText {
        use crate::deck::element::{RunMarks, TextRun};
        let mut rt = RichText::new(text);
        rt.runs = vec![TextRun {
            start: 0,
            end,
            marks: RunMarks {
                bold: true,
                ..RunMarks::default()
            },
        }];
        rt
    }

    fn fresh_deck_first_text_child() -> (Deck, SlideId, ElementId) {
        let deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let eid: ElementId = deck.slides[&sid].root.children[0].id.clone();
        (deck, sid, eid)
    }

    #[test]
    fn set_text_patch_resolves_and_carries_src_for_tokens() {
        let (mut deck, sid, eid) = fresh_deck_first_text_child();
        let cmd = SetTextContent {
            target: CanvasTarget::Slide(sid),
            element_id: eid,
            new_content: RichText::new("Slide ${slideNumber}"),
        };
        let out = cmd.apply(&mut deck).unwrap();
        match &out.patches[0] {
            Patch::SetText { text, src, .. } => {
                assert_eq!(text, "Slide 1");
                assert_eq!(src.as_deref(), Some("Slide ${slideNumber}"));
            }
            _ => panic!("expected SetText"),
        }
    }

    #[test]
    fn set_text_emits_inner_html_when_the_body_is_formatted() {
        let (mut deck, sid, eid) = fresh_deck_first_text_child();
        let content = bold_head("bold plain", 4);
        let out = SetTextContent {
            target: CanvasTarget::Slide(sid),
            element_id: eid,
            new_content: content,
        }
        .apply(&mut deck)
        .unwrap();
        match &out.patches[0] {
            Patch::SetInnerHtml { html, .. } => assert_eq!(html, "<b>bold</b> plain"),
            other => panic!("expected SetInnerHtml, got {other:?}"),
        }
    }

    #[test]
    fn set_text_inverse_of_a_formatted_body_restores_the_runs() {
        let (mut deck, sid, eid) = fresh_deck_first_text_child();
        let content = bold_head("bold plain", 4);
        let out = SetTextContent {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_content: content.clone(),
        }
        .apply(&mut deck)
        .unwrap();
        match &deck.slides[&sid].find_element(&eid).unwrap().content {
            ElementContent::Text(rt) => assert_eq!(rt, &content),
            other => panic!("expected Text, got {other:?}"),
        }
        out.inverse.apply(&mut deck).unwrap();
        match &deck.slides[&sid].find_element(&eid).unwrap().content {
            ElementContent::Text(rt) => assert!(rt.is_plain()),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn set_text_clears_placeholder_flag() {
        let (mut deck, sid, eid) = fresh_deck_first_text_child();

        if let Some(canvas) = deck.canvas_mut(&CanvasTarget::Slide(sid.clone()))
            && let Some(el) = canvas.find_element_mut(&eid)
        {
            el.placeholder = true;
        }
        SetTextContent {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_content: RichText::new("edited"),
        }
        .apply(&mut deck)
        .unwrap();
        let canvas = deck.canvas(&CanvasTarget::Slide(sid)).unwrap();
        assert!(!canvas.find_element(&eid).unwrap().placeholder);
    }

    #[test]
    fn set_text_patch_no_src_without_tokens() {
        let (mut deck, sid, eid) = fresh_deck_first_text_child();
        let cmd = SetTextContent {
            target: CanvasTarget::Slide(sid),
            element_id: eid,
            new_content: RichText::new("plain"),
        };
        let out = cmd.apply(&mut deck).unwrap();
        match &out.patches[0] {
            Patch::SetText { text, src, .. } => {
                assert_eq!(text, "plain");
                assert!(src.is_none());
            }
            _ => panic!("expected SetText"),
        }
    }

    #[test]
    fn set_text_replaces_plain_content() {
        let (mut deck, sid, eid) = fresh_deck_first_text_child();
        let cmd = SetTextContent {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_content: RichText::new("new contents"),
        };
        let _ = cmd.apply(&mut deck).unwrap();
        match &deck.slides[&sid].find_element(&eid).unwrap().content {
            ElementContent::Text(rt) => assert_eq!(rt.plain, "new contents"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn set_text_emits_one_set_text_patch() {
        let (mut deck, sid, eid) = fresh_deck_first_text_child();
        let cmd = SetTextContent {
            target: CanvasTarget::Slide(sid),
            element_id: eid.clone(),
            new_content: RichText::new("hi"),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(out.patches.len(), 1);
        match &out.patches[0] {
            Patch::SetText {
                element_id, text, ..
            } => {
                assert_eq!(element_id, &eid);
                assert_eq!(text, "hi");
            }
            other => panic!("expected SetText, got {other:?}"),
        }
    }

    #[test]
    fn set_text_inverse_restores_prior_text() {
        let (mut deck, sid, eid) = fresh_deck_first_text_child();
        let original: String = match &deck.slides[&sid].find_element(&eid).unwrap().content {
            ElementContent::Text(rt) => rt.plain.clone(),
            _ => panic!("expected Text"),
        };
        let cmd = SetTextContent {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_content: RichText::new("nope"),
        };
        let out = cmd.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();
        match &deck.slides[&sid].find_element(&eid).unwrap().content {
            ElementContent::Text(rt) => assert_eq!(rt.plain, original),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn set_text_errors_on_non_text_element() {
        let mut deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let slide = deck.slides.get_mut(&sid).unwrap();
        slide.root = group_element("rt", vec![image_element("im_a", "asset_x")]);
        let cmd = SetTextContent {
            target: CanvasTarget::Slide(sid),
            element_id: "im_a".into(),
            new_content: RichText::new("x"),
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::InvalidOperation(_)));
    }

    #[test]
    fn set_text_errors_on_missing_slide() {
        let mut deck = Deck::sample();
        let cmd = SetTextContent {
            target: CanvasTarget::Slide("ghost".into()),
            element_id: "x".into(),
            new_content: RichText::new("x"),
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::SlideNotFound(_)));
    }

    #[test]
    fn set_text_errors_on_missing_element() {
        let (mut deck, sid, _) = fresh_deck_first_text_child();
        let cmd = SetTextContent {
            target: CanvasTarget::Slide(sid),
            element_id: "no_such".into(),
            new_content: RichText::new("x"),
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::ElementNotFound(_)));
    }

    #[test]
    fn set_text_marks_slide_dirty() {
        let (mut deck, sid, eid) = fresh_deck_first_text_child();
        let cmd = SetTextContent {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid,
            new_content: RichText::new("x"),
        };
        let _ = cmd.apply(&mut deck).unwrap();
        assert!(deck.slides[&sid].dirty);
    }

    #[test]
    fn set_text_with_empty_string_is_valid() {
        let (mut deck, sid, eid) = fresh_deck_first_text_child();
        let cmd = SetTextContent {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_content: RichText::new(""),
        };
        let out = cmd.apply(&mut deck).unwrap();
        match &out.patches[0] {
            Patch::SetText { text, .. } => assert_eq!(text, ""),
            _ => panic!(),
        }
        match &deck.slides[&sid].find_element(&eid).unwrap().content {
            ElementContent::Text(rt) => assert_eq!(rt.plain, ""),
            _ => panic!(),
        }
    }

    #[test]
    fn set_text_label_and_undoable() {
        let cmd = SetTextContent {
            target: CanvasTarget::Slide("s".into()),
            element_id: "e".into(),
            new_content: RichText::new(""),
        };
        assert_eq!(cmd.label(), "Edit Text");
        assert!(cmd.undoable());
    }
}
