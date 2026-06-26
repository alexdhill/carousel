// InsertElement command.
//
// SPEC §9.3 (element lifecycle). Inserts a complete ElementNode subtree
// under a parent at a specific position. Used as a real command (tool
// palette → insert shape) and as the inverse of RemoveElement.
//
// The emitted patch is `Patch::InsertElement { parent_id, position, html }`
// where `html` is the serialized form of `self.node`. The JS host calls
// the HTML parser to materialize the subtree into the shadow root.

use crate::commands::remove_element::RemoveElementCommand;
use crate::commands::{Command, CommandError, CommandOutput, resolve_canvas_mut};
use crate::deck::canvas::InsertError;
use crate::deck::element::ElementNode;
use crate::deck::{Canvas, CanvasTarget, ElementId, SlideId};
use crate::html::serialize::serialize_element;
use crate::ipc::Patch;

#[derive(Debug, Clone)]
pub struct InsertElement {
    pub target: CanvasTarget,
    pub parent_id: ElementId,
    pub position: usize,
    pub node: ElementNode,
}

impl Command for InsertElement {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with an InsertElement patch and a
    // RemoveElementCommand inverse keyed on the inserted node's id.
    // Errors:
    //   SlideNotFound      — slide_id absent
    //   ElementNotFound    — parent_id absent in the tree
    //   InvalidOperation   — position out of range
    // Dataflow: locate slide -> clone node into the tree at (parent, pos)
    // -> serialize the inserted node -> build patch + inverse.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.target.id().is_empty(),
            "InsertElement: target id is empty"
        );
        assert!(
            !self.parent_id.is_empty(),
            "InsertElement: parent_id is empty"
        );
        assert!(!self.node.id.is_empty(), "InsertElement: node has empty id");
        assert!(
            self.node.is_consistent(),
            "InsertElement: node is inconsistent"
        );
        let canvas = resolve_canvas_mut(deck, &self.target)?;

        let inserted_id: ElementId = self.node.id.clone();
        let html: String = serialize_element(&self.node);
        canvas
            .insert_child(&self.parent_id, self.position, self.node.clone())
            .map_err(|e| match e {
                InsertError::ParentNotFound => {
                    CommandError::ElementNotFound(self.parent_id.clone())
                }
                InsertError::PositionOutOfRange { len, requested } => {
                    CommandError::InvalidOperation(format!(
                        "insert position {requested} exceeds parent {pid}'s {len} children",
                        pid = self.parent_id,
                    ))
                }
            })?;
        canvas.mark_dirty();
        canvas.invalidate_index();

        let inverse: RemoveElementCommand = RemoveElementCommand {
            target: self.target.clone(),
            element_id: inserted_id,
        };

        Ok(CommandOutput {
            patches: vec![Patch::InsertElement {
                parent_id: self.parent_id.clone(),
                position: self.position,
                html,
            }],
            inverse: Box::new(inverse),
            dirty_targets: vec![self.target.clone()],
            manifest_dirty: false,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Insert Element"
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
    use crate::deck::builders::text_element;

    fn deck_root_id() -> (Deck, SlideId, ElementId) {
        let deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let root_id: ElementId = deck.slides[&sid].root.id.clone();
        (deck, sid, root_id)
    }

    #[test]
    fn insert_appends_under_root() {
        let (mut deck, sid, root_id) = deck_root_id();
        let count_before: usize = deck.slides[&sid].root.children.len();
        let cmd = InsertElement {
            target: CanvasTarget::Slide(sid.clone()),
            parent_id: root_id,
            position: count_before,
            node: text_element("new_a", "hello"),
        };
        let _ = cmd.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].root.children.len(), count_before + 1);
        assert!(deck.slides[&sid].find_element("new_a").is_some());
    }

    #[test]
    fn insert_at_head_shifts_existing() {
        let (mut deck, sid, root_id) = deck_root_id();
        let first_existing: ElementId = deck.slides[&sid].root.children[0].id.clone();
        let cmd = InsertElement {
            target: CanvasTarget::Slide(sid.clone()),
            parent_id: root_id,
            position: 0,
            node: text_element("new_a", "hi"),
        };
        let _ = cmd.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].root.children[0].id, "new_a");
        assert_eq!(deck.slides[&sid].root.children[1].id, first_existing);
    }

    #[test]
    fn insert_emits_patch_with_serialized_html() {
        let (mut deck, sid, root_id) = deck_root_id();
        let cmd = InsertElement {
            target: CanvasTarget::Slide(sid),
            parent_id: root_id.clone(),
            position: 0,
            node: text_element("new_a", "hello"),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(out.patches.len(), 1);
        match &out.patches[0] {
            Patch::InsertElement {
                parent_id,
                position,
                html,
            } => {
                assert_eq!(parent_id, &root_id);
                assert_eq!(*position, 0);
                assert!(html.contains(r#"data-element-id="new_a""#));
                assert!(html.contains("hello"));
            }
            other => panic!("expected InsertElement patch, got {other:?}"),
        }
    }

    #[test]
    fn insert_inverse_removes_inserted_node() {
        let (mut deck, sid, root_id) = deck_root_id();
        let cmd = InsertElement {
            target: CanvasTarget::Slide(sid.clone()),
            parent_id: root_id,
            position: 0,
            node: text_element("new_a", "hi"),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert!(deck.slides[&sid].find_element("new_a").is_some());
        out.inverse.apply(&mut deck).unwrap();
        assert!(deck.slides[&sid].find_element("new_a").is_none());
    }

    #[test]
    fn insert_errors_on_missing_slide() {
        let cmd = InsertElement {
            target: CanvasTarget::Slide("ghost".into()),
            parent_id: "rt".into(),
            position: 0,
            node: text_element("n", "x"),
        };
        let err = cmd.apply(&mut Deck::sample()).unwrap_err();
        assert!(matches!(err, CommandError::SlideNotFound(_)));
    }

    #[test]
    fn insert_errors_on_missing_parent() {
        let (mut deck, sid, _) = deck_root_id();
        let cmd = InsertElement {
            target: CanvasTarget::Slide(sid),
            parent_id: "no_such_parent".into(),
            position: 0,
            node: text_element("n", "x"),
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::ElementNotFound(_)));
    }

    #[test]
    fn insert_errors_on_position_out_of_range() {
        let (mut deck, sid, root_id) = deck_root_id();
        let cmd = InsertElement {
            target: CanvasTarget::Slide(sid),
            parent_id: root_id,
            position: 999,
            node: text_element("n", "x"),
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::InvalidOperation(_)));
    }

    #[test]
    fn insert_marks_slide_dirty() {
        let (mut deck, sid, root_id) = deck_root_id();
        let cmd = InsertElement {
            target: CanvasTarget::Slide(sid.clone()),
            parent_id: root_id,
            position: 0,
            node: text_element("n", "x"),
        };
        let _ = cmd.apply(&mut deck).unwrap();
        assert!(deck.slides[&sid].dirty);
    }

    #[test]
    fn insert_then_remove_inverse_round_trip() {
        let (mut deck, sid, root_id) = deck_root_id();
        let before: usize = deck.slides[&sid].root.children.len();
        let cmd = InsertElement {
            target: CanvasTarget::Slide(sid.clone()),
            parent_id: root_id,
            position: 1,
            node: text_element("inserted", "hi"),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].root.children.len(), before + 1);
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].root.children.len(), before);
    }

    #[test]
    fn insert_label_and_undoable() {
        let cmd = InsertElement {
            target: CanvasTarget::Slide("s".into()),
            parent_id: "p".into(),
            position: 0,
            node: text_element("n", "x"),
        };
        assert_eq!(cmd.label(), "Insert Element");
        assert!(cmd.undoable());
    }
}
