// SetEmbedHtml command.
//
// Replaces an Embed element's raw inner HTML (the "code block"). The model
// carries the HTML verbatim in ElementContent::Embed(String); the serializer
// writes it unmodified, so editing is a straight string swap. Emits one
// SetInnerHtml patch (the embed wrapper's innerHTML is replaced live) and a
// self-inverse carrying the prior HTML.

use crate::commands::{Command, CommandError, CommandOutput, resolve_canvas_mut};
use crate::deck::element::ElementContent;
use crate::deck::{Canvas, CanvasTarget, ElementId};
use crate::ipc::Patch;

#[derive(Debug, Clone)]
pub struct SetEmbedHtml {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub new_html: String,
}

impl Command for SetEmbedHtml {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with one SetInnerHtml patch and an inverse
    // SetEmbedHtml carrying the prior HTML.
    // Errors:
    //   SlideNotFound / ElementNotFound — target or element absent.
    //   InvalidOperation — element is not an Embed element.
    // Dataflow: resolve canvas -> locate element -> assert Embed content ->
    // snapshot prior -> overwrite -> build patch + inverse.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.target.id().is_empty(),
            "SetEmbedHtml: target id is empty"
        );
        assert!(
            !self.element_id.is_empty(),
            "SetEmbedHtml: element_id is empty"
        );
        let canvas = resolve_canvas_mut(deck, &self.target)?;
        let element = canvas
            .find_element_mut(&self.element_id)
            .ok_or_else(|| CommandError::ElementNotFound(self.element_id.clone()))?;

        let prev_html: String = match &element.content {
            ElementContent::Embed(html) => html.clone(),
            _ => {
                return Err(CommandError::InvalidOperation(format!(
                    "SetEmbedHtml on non-embed element {}",
                    self.element_id
                )));
            }
        };

        element.content = ElementContent::Embed(self.new_html.clone());
        canvas.mark_dirty();
        canvas.invalidate_index();

        let inverse: SetEmbedHtml = SetEmbedHtml {
            target: self.target.clone(),
            element_id: self.element_id.clone(),
            new_html: prev_html,
        };

        Ok(CommandOutput {
            patches: vec![Patch::SetInnerHtml {
                element_id: self.element_id.clone(),
                html: self.new_html.clone(),
            }],
            inverse: Box::new(inverse),
            dirty_targets: vec![self.target.clone()],
            manifest_dirty: false,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Edit Code Block"
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;
    use crate::deck::SlideId;
    use crate::deck::builders::{embed_element, group_element, image_element};

    fn deck_with_embed() -> (Deck, SlideId, ElementId) {
        let mut deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        deck.slides.get_mut(&sid).unwrap().root =
            group_element("rt", vec![embed_element("em_a", "<b>old</b>")]);
        (deck, sid, "em_a".into())
    }

    #[test]
    fn set_embed_replaces_html_and_emits_patch() {
        let (mut deck, sid, eid) = deck_with_embed();
        let cmd = SetEmbedHtml {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_html: "<i>new</i>".into(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        match &deck.slides[&sid].find_element(&eid).unwrap().content {
            ElementContent::Embed(h) => assert_eq!(h, "<i>new</i>"),
            other => panic!("expected Embed, got {other:?}"),
        }
        assert_eq!(out.patches.len(), 1);
        match &out.patches[0] {
            Patch::SetInnerHtml { element_id, html } => {
                assert_eq!(element_id, &eid);
                assert_eq!(html, "<i>new</i>");
            }
            other => panic!("expected SetInnerHtml, got {other:?}"),
        }
    }

    #[test]
    fn set_embed_inverse_restores_prior_html() {
        let (mut deck, sid, eid) = deck_with_embed();
        let cmd = SetEmbedHtml {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_html: "<i>new</i>".into(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();
        match &deck.slides[&sid].find_element(&eid).unwrap().content {
            ElementContent::Embed(h) => assert_eq!(h, "<b>old</b>"),
            other => panic!("expected Embed, got {other:?}"),
        }
    }

    #[test]
    fn set_embed_errors_on_non_embed_element() {
        let mut deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        deck.slides.get_mut(&sid).unwrap().root =
            group_element("rt", vec![image_element("im_a", "asset_x")]);
        let cmd = SetEmbedHtml {
            target: CanvasTarget::Slide(sid),
            element_id: "im_a".into(),
            new_html: "x".into(),
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::InvalidOperation(_)));
    }

    #[test]
    fn set_embed_label_and_undoable() {
        let cmd = SetEmbedHtml {
            target: CanvasTarget::Slide("s".into()),
            element_id: "e".into(),
            new_html: String::new(),
        };
        assert_eq!(cmd.label(), "Edit Code Block");
        assert!(cmd.undoable());
    }
}
