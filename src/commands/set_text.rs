// SetTextContent command.
//
// SPEC §9.3 (element style and content). Replaces a Text element's
// content with a new RichText value. Stage 3's RichText is a plain-text
// wrapper, so the emitted patch is `SetText` (rather than the more
// general SetInnerHtml the spec mentions for fully rich content). When
// RichText gains spans in a later stage, this command can switch to
// SetInnerHtml + a serializer for the inline run sequence.

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
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with one SetText patch and an inverse
    // SetTextContent carrying the prior RichText.
    // Errors:
    //   SlideNotFound       — slide_id absent
    //   ElementNotFound     — element_id absent
    //   InvalidOperation    — element is not a Text element
    // Dataflow: locate slide -> locate element -> assert it carries Text
    // content -> snapshot prior -> overwrite -> build patch + inverse.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.target.id().is_empty(),
            "SetTextContent: target id is empty"
        );
        assert!(
            !self.element_id.is_empty(),
            "SetTextContent: element_id is empty"
        );
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
        canvas.mark_dirty();
        canvas.invalidate_index();

        let inverse: SetTextContent = SetTextContent {
            target: self.target.clone(),
            element_id: self.element_id.clone(),
            new_content: prev_content,
        };

        Ok(CommandOutput {
            patches: vec![Patch::SetText {
                element_id: self.element_id.clone(),
                text: self.new_content.plain.clone(),
            }],
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

    fn fresh_deck_first_text_child() -> (Deck, SlideId, ElementId) {
        let deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let eid: ElementId = deck.slides[&sid].root.children[0].id.clone();
        (deck, sid, eid)
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
            Patch::SetText { element_id, text } => {
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
        // Construct a deck whose first child is an image.
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
