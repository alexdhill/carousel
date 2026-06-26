// RemoveElement command.
//
// SPEC §9.3 (element lifecycle). Removes an element subtree from a slide
// and emits a Patch::RemoveElement. The inverse is an InsertElement that
// captures the removed subtree, its parent id, and its prior position, so
// undo can reinsert it at exactly the same location.
//
// The type name is `RemoveElementCommand` (not `RemoveElement`) so it
// does not collide with `Patch::RemoveElement` when both are imported in
// the same module.

use crate::commands::insert_element::InsertElement;
use crate::commands::{Command, CommandError, CommandOutput, resolve_canvas_mut};
use crate::deck::canvas::RemovedElement;
use crate::deck::{Canvas, CanvasTarget, ElementId, SlideId};
use crate::ipc::Patch;

#[derive(Debug, Clone)]
pub struct RemoveElementCommand {
    pub target: CanvasTarget,
    pub element_id: ElementId,
}

impl Command for RemoveElementCommand {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with a RemoveElement patch and an
    // InsertElement inverse holding the removed subtree.
    // Errors:
    //   SlideNotFound     — slide_id absent
    //   InvalidOperation  — element_id equals the slide root
    //   ElementNotFound   — element_id is not in the tree
    // Dataflow: locate slide -> reject root removal -> call
    // remove_non_root_element -> wrap captured subtree as InsertElement.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.target.id().is_empty(),
            "RemoveElement: target id is empty"
        );
        assert!(
            !self.element_id.is_empty(),
            "RemoveElement: element_id is empty"
        );
        let canvas = resolve_canvas_mut(deck, &self.target)?;
        if canvas.is_root_id(&self.element_id) {
            return Err(CommandError::InvalidOperation(format!(
                "cannot remove canvas root element {}",
                self.element_id
            )));
        }
        let removed: RemovedElement = canvas
            .remove_non_root_element(&self.element_id)
            .ok_or_else(|| CommandError::ElementNotFound(self.element_id.clone()))?;
        canvas.mark_dirty();
        canvas.invalidate_index();

        let inverse: InsertElement = InsertElement {
            target: self.target.clone(),
            parent_id: removed.parent_id,
            position: removed.position,
            node: removed.node,
        };

        Ok(CommandOutput {
            patches: vec![Patch::RemoveElement {
                element_id: self.element_id.clone(),
            }],
            inverse: Box::new(inverse),
            dirty_targets: vec![self.target.clone()],
            manifest_dirty: false,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Delete Element"
    }

    fn affects_object_tree(&self) -> bool {
        true
    }

    fn requires_remount(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;

    fn deck_first_child() -> (Deck, SlideId, ElementId) {
        let deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let eid: ElementId = deck.slides[&sid].root.children[0].id.clone();
        (deck, sid, eid)
    }

    #[test]
    fn remove_drops_element_from_tree() {
        let (mut deck, sid, eid) = deck_first_child();
        let count_before: usize = deck.slides[&sid].root.children.len();
        let cmd = RemoveElementCommand {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
        };
        let _ = cmd.apply(&mut deck).unwrap();
        let count_after: usize = deck.slides[&sid].root.children.len();
        assert_eq!(count_after, count_before - 1);
        assert!(deck.slides[&sid].find_element(&eid).is_none());
    }

    #[test]
    fn remove_emits_one_remove_patch() {
        let (mut deck, sid, eid) = deck_first_child();
        let cmd = RemoveElementCommand {
            target: CanvasTarget::Slide(sid),
            element_id: eid.clone(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(out.patches.len(), 1);
        match &out.patches[0] {
            Patch::RemoveElement { element_id } => assert_eq!(element_id, &eid),
            other => panic!("expected RemoveElement patch, got {other:?}"),
        }
    }

    #[test]
    fn remove_inverse_reinserts_at_original_position() {
        let (mut deck, sid, eid) = deck_first_child();
        let pos_before: usize = 0;
        let cmd = RemoveElementCommand {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].root.children[pos_before].id, eid);
    }

    #[test]
    fn remove_inverse_preserves_subtree_geometry_and_content() {
        let (mut deck, sid, eid) = deck_first_child();
        let geometry_before = deck.slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
        let content_before = deck.slides[&sid]
            .find_element(&eid)
            .unwrap()
            .content
            .clone();

        let cmd = RemoveElementCommand {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();

        let geometry_after = deck.slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
        let content_after = deck.slides[&sid]
            .find_element(&eid)
            .unwrap()
            .content
            .clone();
        assert_eq!(geometry_after, geometry_before);
        assert_eq!(content_after, content_before);
    }

    #[test]
    fn remove_errors_on_missing_slide() {
        let mut deck = Deck::sample();
        let cmd = RemoveElementCommand {
            target: CanvasTarget::Slide("ghost".into()),
            element_id: "x".into(),
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::SlideNotFound(_)));
    }

    #[test]
    fn remove_errors_on_missing_element() {
        let (mut deck, sid, _) = deck_first_child();
        let cmd = RemoveElementCommand {
            target: CanvasTarget::Slide(sid),
            element_id: "no_such".into(),
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::ElementNotFound(_)));
    }

    #[test]
    fn remove_root_is_invalid() {
        let (mut deck, sid, _) = deck_first_child();
        let root_id: ElementId = deck.slides[&sid].root.id.clone();
        let cmd = RemoveElementCommand {
            target: CanvasTarget::Slide(sid),
            element_id: root_id,
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::InvalidOperation(_)));
    }

    #[test]
    fn remove_marks_slide_dirty() {
        let (mut deck, sid, eid) = deck_first_child();
        let cmd = RemoveElementCommand {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid,
        };
        let _ = cmd.apply(&mut deck).unwrap();
        assert!(deck.slides[&sid].dirty);
    }

    #[test]
    fn remove_label_and_undoable() {
        let cmd = RemoveElementCommand {
            target: CanvasTarget::Slide("s".into()),
            element_id: "e".into(),
        };
        assert_eq!(cmd.label(), "Delete Element");
        assert!(cmd.undoable());
    }
}
