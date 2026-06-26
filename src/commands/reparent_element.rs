// ReparentElement command.
//
// Stage 9 — Object Panel drag-and-drop. Moves an element from its current
// parent to a target (parent, position) pair within the same slide,
// preserving the element's subtree wholesale. Used by:
//   - Drag-to-reorder within a parent (source parent == target parent).
//   - Drag into a group (target parent is a Group element).
//   - Drag out of a group to the slide root.
//
// `new_position` is interpreted as the post-removal index in the target
// parent's children list. Callers moving within the same parent must
// pre-adjust their position by -1 when the desired insertion point lies
// after the source position. Doing the adjustment in the caller keeps the
// command's semantics straightforward (it just calls remove + insert).
//
// Effects on render: because the slide HTML serialiser assigns z-index
// from sibling positions, a reparent shifts the z-stack of every sibling
// between source and target. Computing precise patches is fiddly; this
// command emits none and sets `requires_remount = true`, letting the
// dispatcher trigger a fresh MountSlide. For Stage 9 slide sizes the
// remount cost is negligible; future optimisation may replace the remount
// with targeted z-index + Insert/Remove patches.

use crate::commands::{Command, CommandError, CommandOutput, resolve_canvas_mut};
use crate::deck::canvas::{InsertError, RemovedElement};
use crate::deck::element::ElementNode;
use crate::deck::{Canvas, CanvasTarget, ElementId, SlideId};

#[derive(Debug, Clone)]
pub struct ReparentElement {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub new_parent_id: ElementId,
    pub new_position: usize,
}

impl Command for ReparentElement {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with no patches (the dispatcher remounts the
    // active slide), an inverse ReparentElement carrying the prior parent
    // + position, and the slide marked dirty.
    // Errors:
    //   SlideNotFound       — slide_id absent.
    //   InvalidOperation    — moving the slide root, moving an element
    //                         under itself, or moving into a non-existent
    //                         parent.
    //   ElementNotFound     — element_id absent in the tree.
    // Dataflow:
    //   1. Refuse to move the slide root.
    //   2. Verify the target parent exists and is not a descendant of
    //      element_id (cycle check).
    //   3. Snapshot the source (parent_id, position) for the inverse.
    //   4. Remove the element from its current parent.
    //   5. Insert it into the target parent at new_position.
    //   6. Build the inverse command.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.target.id().is_empty(),
            "ReparentElement: target id is empty"
        );
        assert!(
            !self.element_id.is_empty(),
            "ReparentElement: element_id is empty"
        );
        assert!(
            !self.new_parent_id.is_empty(),
            "ReparentElement: new_parent_id is empty"
        );

        let canvas = resolve_canvas_mut(deck, &self.target)?;

        if canvas.is_root_id(&self.element_id) {
            return Err(CommandError::InvalidOperation(
                "ReparentElement: cannot move the canvas root".into(),
            ));
        }
        let moving: &ElementNode = canvas
            .find_element(&self.element_id)
            .ok_or_else(|| CommandError::ElementNotFound(self.element_id.clone()))?;
        if subtree_contains(moving, &self.new_parent_id) {
            return Err(CommandError::InvalidOperation(format!(
                "ReparentElement: cannot move {} under its own descendant {}",
                self.element_id, self.new_parent_id,
            )));
        }
        // Verify target parent exists at all.
        if canvas.find_element(&self.new_parent_id).is_none() {
            return Err(CommandError::ElementNotFound(self.new_parent_id.clone()));
        }

        // Capture coordinate frames BEFORE mutation so the moved element keeps
        // its visual position/size when its coordinates switch parent spaces
        // (each parent's children are stored in that parent's local, scaled
        // space). Converting here is what makes a drag into/out of a scaled
        // group not teleport the element; the subsequent relayout shrink-wraps.
        let move_frame = crate::deck::group_layout::element_frame(canvas.root(), &self.element_id);
        let np_frame = crate::deck::group_layout::element_frame(canvas.root(), &self.new_parent_id);
        let old_size: (f64, f64) = canvas
            .find_element(&self.element_id)
            .map(|n| (n.geometry.width, n.geometry.height))
            .unwrap_or((0.0, 0.0));

        let RemovedElement {
            node,
            parent_id: old_parent,
            position: old_position,
        } = canvas
            .remove_non_root_element(&self.element_id)
            .ok_or_else(|| CommandError::ElementNotFound(self.element_id.clone()))?;

        canvas
            .insert_child(&self.new_parent_id, self.new_position, node)
            .map_err(|e| match e {
                InsertError::ParentNotFound => {
                    CommandError::ElementNotFound(self.new_parent_id.clone())
                }
                InsertError::PositionOutOfRange { len, requested } => {
                    CommandError::InvalidOperation(format!(
                        "ReparentElement: position {requested} > len {len} in {pid}",
                        pid = self.new_parent_id,
                    ))
                }
            })?;
        // Convert the moved element into its new parent's coordinate space and
        // re-fit the affected groups. No patches: the command remounts.
        if let (Some((mx, my, ms_anc, _)), Some((px, py, ps_anc, ps_own))) = (move_frame, np_frame)
        {
            let content_scale: f64 = {
                let c = ps_anc * ps_own;
                if c.abs() < f64::EPSILON { 1.0 } else { c }
            };
            let size_factor: f64 = ms_anc / content_scale;
            if let Some(n) = canvas.find_element_mut(&self.element_id) {
                n.geometry.x = (mx - px) / content_scale;
                n.geometry.y = (my - py) / content_scale;
                n.geometry.width = old_size.0 * size_factor;
                n.geometry.height = old_size.1 * size_factor;
            }
        }
        crate::deck::group_layout::relayout_ancestors(canvas.root_mut(), &self.element_id);
        crate::deck::group_layout::relayout_ancestors(canvas.root_mut(), &old_parent);

        canvas.mark_dirty();
        canvas.invalidate_index();

        let inverse: ReparentElement = ReparentElement {
            target: self.target.clone(),
            element_id: self.element_id.clone(),
            new_parent_id: old_parent,
            new_position: old_position,
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
        "Reparent Element"
    }

    fn affects_object_tree(&self) -> bool {
        true
    }

    fn requires_remount(&self) -> bool {
        true
    }
}

// subtree_contains
// Inputs: a node, a candidate id to search for.
// Output: true if `candidate` equals the node itself or matches any
// descendant id. Used as the cycle-detection check before reparenting.
// Dataflow: iterative DFS over the node's children with a depth cap.
fn subtree_contains(node: &ElementNode, candidate: &str) -> bool {
    assert!(!candidate.is_empty(), "subtree_contains: empty candidate");
    const MAX_DEPTH_FRAMES: usize = 4_096;
    if node.id == candidate {
        return true;
    }
    let mut stack: Vec<&ElementNode> = Vec::with_capacity(16);
    for child in &node.children {
        stack.push(child);
    }
    let mut iter: usize = 0;
    while let Some(n) = stack.pop() {
        assert!(
            iter < MAX_DEPTH_FRAMES,
            "subtree_contains: depth bound exceeded"
        );
        iter += 1;
        if n.id == candidate {
            return true;
        }
        for child in &n.children {
            stack.push(child);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;
    use crate::deck::builders::{group_element, text_element};
    use crate::deck::slide::SlideNode;
    use std::collections::BTreeMap;

    fn fixture() -> (Deck, SlideId, ElementId) {
        let deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let eid: ElementId = deck.slides[&sid].root.children[0].id.clone();
        (deck, sid, eid)
    }

    fn child_ids(deck: &Deck, sid: &SlideId, parent_id: &str) -> Vec<ElementId> {
        deck.slides[sid]
            .find_element(parent_id)
            .unwrap()
            .children
            .iter()
            .map(|c| c.id.clone())
            .collect()
    }

    #[test]
    fn move_within_same_parent_reorders() {
        let (mut deck, sid, _) = fixture();
        let root_id = deck.slides[&sid].root.id.clone();
        let kids = child_ids(&deck, &sid, &root_id);
        let third = kids[2].clone();
        let cmd = ReparentElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: third.clone(),
            new_parent_id: root_id.clone(),
            new_position: 0,
        };
        cmd.apply(&mut deck).unwrap();
        let after = child_ids(&deck, &sid, &root_id);
        assert_eq!(after[0], third);
        assert_eq!(after.len(), kids.len());
    }

    #[test]
    fn move_across_parents_relocates_subtree() {
        // Build: root -> [text_a, group_g -> [text_g_inner]].
        let inner = text_element("el_inner", "g");
        let group = group_element("el_group", vec![inner]);
        let outer = text_element("el_a", "a");
        let root = group_element("el_root", vec![outer, group]);
        let slide = SlideNode::new("s".into(), "title".into(), root);
        let mut slides: BTreeMap<SlideId, SlideNode> = BTreeMap::new();
        slides.insert("s".into(), slide);
        let mut deck: Deck = Deck::default();
        deck.slides = slides;
        deck.slide_order = vec!["s".into()];

        let cmd = ReparentElement {
            target: CanvasTarget::Slide("s".into()),
            element_id: "el_a".into(),
            new_parent_id: "el_group".into(),
            new_position: 0,
        };
        cmd.apply(&mut deck).unwrap();
        let group_kids = child_ids(&deck, &"s".into(), "el_group");
        assert!(group_kids.iter().any(|k| k == "el_a"));
        let root_kids = child_ids(&deck, &"s".into(), "el_root");
        assert!(!root_kids.iter().any(|k| k == "el_a"));
    }

    #[test]
    fn reparent_into_group_shrinkwraps_and_preserves_position() {
        // root -> [ a(200,100,20,10), g(@50,50) -> [ b(0,0,30,30) ] ]
        let mut a = text_element("el_a", "a");
        a.geometry.x = 200.0;
        a.geometry.y = 100.0;
        a.geometry.width = 20.0;
        a.geometry.height = 10.0;
        let mut b = text_element("el_b", "b");
        b.geometry.width = 30.0;
        b.geometry.height = 30.0;
        let mut g = group_element("el_group", vec![b]);
        g.geometry.x = 50.0;
        g.geometry.y = 50.0;
        let root = group_element("el_root", vec![a, g]);
        let slide = SlideNode::new("s".into(), "t".into(), root);
        let mut slides: BTreeMap<SlideId, SlideNode> = BTreeMap::new();
        slides.insert("s".into(), slide);
        let mut deck: Deck = Deck::default();
        deck.slides = slides;
        deck.slide_order = vec!["s".into()];

        ReparentElement {
            target: CanvasTarget::Slide("s".into()),
            element_id: "el_a".into(),
            new_parent_id: "el_group".into(),
            new_position: 1,
        }
        .apply(&mut deck)
        .unwrap();

        let sid: SlideId = "s".into();
        let g = deck.slides[&sid].find_element("el_group").unwrap();
        // bbox of {b(0,0,30,30), a(150,50,20,10)} -> 170 x 60.
        assert_eq!(g.geometry.width, 170.0);
        assert_eq!(g.geometry.height, 60.0);
        assert_eq!(g.geometry.x, 50.0); // origin unchanged (min was 0,0)
        let a = g.children.iter().find(|c| c.id == "el_a").unwrap();
        assert_eq!(a.geometry.x, 150.0); // 200 - 50 group origin
        assert_eq!(a.geometry.y, 50.0); // 100 - 50
    }

    #[test]
    fn inverse_restores_original_parent_and_position() {
        let (mut deck, sid, _) = fixture();
        let root_id = deck.slides[&sid].root.id.clone();
        let kids_before = child_ids(&deck, &sid, &root_id);
        let second = kids_before[1].clone();
        let cmd = ReparentElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: second.clone(),
            new_parent_id: root_id.clone(),
            new_position: 0,
        };
        let out = cmd.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();
        let after = child_ids(&deck, &sid, &root_id);
        assert_eq!(after, kids_before);
    }

    #[test]
    fn moving_root_is_invalid_operation() {
        let (mut deck, sid, _) = fixture();
        let root_id = deck.slides[&sid].root.id.clone();
        let cmd = ReparentElement {
            target: CanvasTarget::Slide(sid),
            element_id: root_id.clone(),
            new_parent_id: root_id,
            new_position: 0,
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::InvalidOperation(_)));
    }

    #[test]
    fn moving_element_under_itself_is_invalid() {
        // Build: root -> [group_g -> [text_inner]].
        let inner = text_element("el_inner", "x");
        let group = group_element("el_group", vec![inner]);
        let root = group_element("el_root", vec![group]);
        let slide = SlideNode::new("s".into(), "title".into(), root);
        let mut slides: BTreeMap<SlideId, SlideNode> = BTreeMap::new();
        slides.insert("s".into(), slide);
        let mut deck: Deck = Deck::default();
        deck.slides = slides;
        deck.slide_order = vec!["s".into()];

        let cmd = ReparentElement {
            target: CanvasTarget::Slide("s".into()),
            element_id: "el_group".into(),
            new_parent_id: "el_inner".into(),
            new_position: 0,
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::InvalidOperation(_)));
    }

    #[test]
    fn target_parent_missing_yields_element_not_found() {
        let (mut deck, sid, eid) = fixture();
        let cmd = ReparentElement {
            target: CanvasTarget::Slide(sid),
            element_id: eid,
            new_parent_id: "no_such".into(),
            new_position: 0,
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::ElementNotFound(id) if id == "no_such"));
    }

    #[test]
    fn position_out_of_range_errors() {
        let (mut deck, sid, eid) = fixture();
        let root_id = deck.slides[&sid].root.id.clone();
        let cmd = ReparentElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid,
            new_parent_id: root_id,
            new_position: 999,
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::InvalidOperation(_)));
    }

    #[test]
    fn produces_no_patches_but_requires_remount() {
        let (mut deck, sid, _) = fixture();
        let root_id = deck.slides[&sid].root.id.clone();
        let third = child_ids(&deck, &sid, &root_id)[2].clone();
        let cmd = ReparentElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: third,
            new_parent_id: root_id,
            new_position: 0,
        };
        assert!(cmd.affects_object_tree());
        assert!(cmd.requires_remount());
        let out = cmd.apply(&mut deck).unwrap();
        assert!(out.patches.is_empty());
        assert_eq!(out.dirty_targets.len(), 1);
    }

    #[test]
    fn marks_slide_dirty() {
        let (mut deck, sid, _) = fixture();
        let root_id = deck.slides[&sid].root.id.clone();
        let third = child_ids(&deck, &sid, &root_id)[2].clone();
        ReparentElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: third,
            new_parent_id: root_id,
            new_position: 0,
        }
        .apply(&mut deck)
        .unwrap();
        assert!(deck.slides[&sid].dirty);
    }
}
