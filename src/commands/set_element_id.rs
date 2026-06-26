// SetElementId command.
//
// Renames an element's `id` — the value that keys the per-slide element
// index and appears as `data-element-id` in the rendered DOM. Editing the
// id from the object panel routes here so the change lands in the true
// deck model, not just the webview.
//
// The id is a per-slide primary key, so the new id must be non-empty and
// must not already belong to another element on the same slide. The
// command re-mounts the slide (requires_remount) so the serialized
// `data-element-id` and the object panel rebuild together, and reports
// affects_object_tree so the panel label refreshes. Its inverse swaps the
// ids back.

use crate::commands::{Command, CommandError, CommandOutput, resolve_canvas_mut};
use crate::deck::{Canvas, CanvasTarget, ElementId, SlideId};

#[derive(Debug, Clone)]
pub struct SetElementId {
    pub target: CanvasTarget,
    pub old_id: ElementId,
    pub new_id: ElementId,
}

impl Command for SetElementId {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with no patches (the remount re-serializes the
    // slide), an inverse SetElementId that swaps the ids back, the slide
    // marked dirty.
    // Errors:
    //   SlideNotFound    — slide_id absent.
    //   ElementNotFound  — old_id absent on the slide.
    //   Conflict         — new_id already names a different element.
    // Dataflow: locate slide -> guard collision -> locate element by old
    // id -> overwrite its id -> invalidate index -> build inverse.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.target.id().is_empty(),
            "SetElementId: target id is empty"
        );
        assert!(!self.new_id.is_empty(), "SetElementId: new_id is empty");
        let canvas = resolve_canvas_mut(deck, &self.target)?;
        if self.new_id != self.old_id && canvas.find_element(&self.new_id).is_some() {
            return Err(CommandError::Conflict(format!(
                "element id {} already exists on canvas {}",
                self.new_id,
                self.target.id()
            )));
        }
        let element = canvas
            .find_element_mut(&self.old_id)
            .ok_or_else(|| CommandError::ElementNotFound(self.old_id.clone()))?;
        element.id = self.new_id.clone();
        canvas.mark_dirty();
        canvas.invalidate_index();

        let inverse: SetElementId = SetElementId {
            target: self.target.clone(),
            old_id: self.new_id.clone(),
            new_id: self.old_id.clone(),
        };

        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(inverse),
            dirty_targets: vec![self.target.clone()],
            manifest_dirty: false,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Rename Element ID"
    }

    fn requires_remount(&self) -> bool {
        true
    }

    fn affects_object_tree(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;

    fn fixture() -> (Deck, SlideId, ElementId) {
        let deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let eid: ElementId = deck.slides[&sid].root.children[0].id.clone();
        (deck, sid, eid)
    }

    #[test]
    fn renames_the_element_id_in_the_tree() {
        let (mut deck, sid, eid) = fixture();
        let cmd = SetElementId {
            target: CanvasTarget::Slide(sid.clone()),
            old_id: eid.clone(),
            new_id: "el_renamed".into(),
        };
        cmd.apply(&mut deck).unwrap();
        assert!(deck.slides[&sid].find_element("el_renamed").is_some());
        assert!(deck.slides[&sid].find_element(&eid).is_none());
    }

    #[test]
    fn remounts_and_refreshes_object_tree() {
        let cmd = SetElementId {
            target: CanvasTarget::Slide("s".into()),
            old_id: "a".into(),
            new_id: "b".into(),
        };
        assert!(cmd.requires_remount());
        assert!(cmd.affects_object_tree());
    }

    #[test]
    fn inverse_swaps_the_id_back() {
        let (mut deck, sid, eid) = fixture();
        let cmd = SetElementId {
            target: CanvasTarget::Slide(sid.clone()),
            old_id: eid.clone(),
            new_id: "el_x".into(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();
        assert!(deck.slides[&sid].find_element(&eid).is_some());
        assert!(deck.slides[&sid].find_element("el_x").is_none());
    }

    #[test]
    fn rejects_collision_with_existing_id() {
        let mut deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let children = &deck.slides[&sid].root.children;
        let first: ElementId = children[0].id.clone();
        let second: ElementId = children[1].id.clone();
        let cmd = SetElementId {
            target: CanvasTarget::Slide(sid),
            old_id: first,
            new_id: second,
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::Conflict(_)));
    }

    #[test]
    fn errors_on_missing_element() {
        let (mut deck, sid, _) = fixture();
        let cmd = SetElementId {
            target: CanvasTarget::Slide(sid),
            old_id: "ghost".into(),
            new_id: "el_y".into(),
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::ElementNotFound(_)));
    }

    #[test]
    fn marks_slide_dirty() {
        let (mut deck, sid, eid) = fixture();
        SetElementId {
            target: CanvasTarget::Slide(sid.clone()),
            old_id: eid,
            new_id: "el_z".into(),
        }
        .apply(&mut deck)
        .unwrap();
        assert!(deck.slides[&sid].dirty);
    }
}
