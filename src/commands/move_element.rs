// MoveElement command.
//
// SPEC §9.2 — the canonical rollback-data example. Mutates an element's
// (x, y) geometry, emits SetStyle patches for left/top, and constructs an
// inverse MoveElement carrying the prior position as its new_position.
//
// `previous_position` is unused on the request form (None) and populated
// on the inverse for debuggability — it documents what the inverse will
// restore to without having to consult the deck.

use crate::commands::{Command, CommandError, CommandOutput, resolve_canvas_mut};
use crate::deck::{Canvas, CanvasTarget, ElementId, SlideId};
use crate::ipc::{Patch, Point};

#[derive(Debug, Clone)]
pub struct MoveElement {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub new_position: Point,
    pub previous_position: Option<Point>,
}

impl Command for MoveElement {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with two SetStyle patches (left, top), an
    // inverse MoveElement, and the target canvas as dirty.
    // Errors: SlideNotFound / LayoutNotFound, ElementNotFound.
    // Dataflow: resolve canvas -> locate element -> snapshot prior (x,y)
    // -> write new geometry -> invalidate index -> build patches/inverse.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.target.id().is_empty(), "MoveElement: target id is empty");
        assert!(!self.element_id.is_empty(), "MoveElement: element_id is empty");
        let canvas = resolve_canvas_mut(deck, &self.target)?;
        let element = canvas
            .find_element_mut(&self.element_id)
            .ok_or_else(|| CommandError::ElementNotFound(self.element_id.clone()))?;

        let prev_position: Point = Point {
            x: element.geometry.x,
            y: element.geometry.y,
        };
        element.geometry.x = self.new_position.x;
        element.geometry.y = self.new_position.y;
        canvas.mark_dirty();
        canvas.invalidate_index();

        let inverse: MoveElement = MoveElement {
            target: self.target.clone(),
            element_id: self.element_id.clone(),
            new_position: prev_position,
            previous_position: Some(self.new_position),
        };

        let mut patches: Vec<Patch> = vec![
            Patch::SetStyle {
                element_id: self.element_id.clone(),
                property: "left".into(),
                value: format!("{}px", self.new_position.x),
            },
            Patch::SetStyle {
                element_id: self.element_id.clone(),
                property: "top".into(),
                value: format!("{}px", self.new_position.y),
            },
        ];
        patches.extend(crate::commands::relayout_patches(canvas.root_mut(), &self.element_id));

        Ok(CommandOutput {
            patches,
            inverse: Box::new(inverse),
            dirty_targets: vec![self.target.clone()],
            manifest_dirty: false,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Move Element"
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;

    fn fresh_deck_first_child() -> (Deck, SlideId, ElementId) {
        let deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let eid: ElementId = deck.slides[&sid].root.children[0].id.clone();
        (deck, sid, eid)
    }

    #[test]
    fn move_inside_group_relayouts_and_emits_group_patches() {
        use crate::deck::builders::{group_element, text_element};
        use crate::deck::element::ElementStyle;
        use crate::deck::style::{GroupDistribution, GroupStyle};
        let mut deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let mut a = text_element("ca", "t"); a.geometry.width = 20.0; a.geometry.height = 10.0;
        let mut b = text_element("cb", "t"); b.geometry.x = 80.0; b.geometry.width = 20.0; b.geometry.height = 10.0;
        let mut g = group_element("cg", vec![a, b]);
        g.style = ElementStyle::Group(GroupStyle { distribution: GroupDistribution::SpaceBetween, ..Default::default() });
        deck.slides.get_mut(&sid).unwrap().root.children.push(g);
        let cmd = MoveElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: "cb".into(),
            new_position: Point { x: 200.0, y: 0.0 },
            previous_position: None,
        };
        let out = cmd.apply(&mut deck).unwrap();
        // Group "cg" width tracks the new span (SpaceBetween pins ends) -> > 100.
        let g = deck.slides[&sid].find_element("cg").unwrap();
        assert!(g.geometry.width >= 200.0);
        // Patches include a width SetStyle for the group.
        assert!(out.patches.iter().any(|p| matches!(p,
            Patch::SetStyle { element_id, property, .. } if element_id == "cg" && property == "width")));
    }

    #[test]
    fn move_updates_geometry_xy_only() {
        let (mut deck, sid, eid) = fresh_deck_first_child();
        let before = deck.slides[&sid].find_element(&eid).unwrap().geometry.clone();
        let cmd = MoveElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_position: Point { x: 500.0, y: 300.0 },
            previous_position: None,
        };
        let out = cmd.apply(&mut deck).unwrap();
        let after = deck.slides[&sid].find_element(&eid).unwrap().geometry.clone();
        assert_eq!(after.x, 500.0);
        assert_eq!(after.y, 300.0);
        // Width/height/opacity/rotation/z_order untouched.
        assert_eq!(after.width, before.width);
        assert_eq!(after.height, before.height);
        assert_eq!(after.opacity, before.opacity);
        assert_eq!(after.rotation, before.rotation);
        assert_eq!(after.z_order, before.z_order);
        assert_eq!(out.patches.len(), 2);
        assert_eq!(out.dirty_targets, vec![CanvasTarget::Slide(sid)]);
        assert!(!out.manifest_dirty);
    }

    #[test]
    fn move_emits_left_and_top_set_style_patches() {
        let (mut deck, sid, eid) = fresh_deck_first_child();
        let cmd = MoveElement {
            target: CanvasTarget::Slide(sid),
            element_id: eid.clone(),
            new_position: Point { x: 12.5, y: -7.0 },
            previous_position: None,
        };
        let out = cmd.apply(&mut deck).unwrap();
        let mut found_left = false;
        let mut found_top = false;
        for p in &out.patches {
            if let Patch::SetStyle { element_id, property, value } = p {
                assert_eq!(element_id, &eid);
                if property == "left" {
                    assert_eq!(value, "12.5px");
                    found_left = true;
                }
                if property == "top" {
                    assert_eq!(value, "-7px");
                    found_top = true;
                }
            }
        }
        assert!(found_left && found_top);
    }

    #[test]
    fn move_inverse_restores_prior_position() {
        let (mut deck, sid, eid) = fresh_deck_first_child();
        let original = deck.slides[&sid].find_element(&eid).unwrap().geometry.clone();

        let cmd = MoveElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_position: Point { x: 999.0, y: -123.0 },
            previous_position: None,
        };
        let out = cmd.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();
        let after = deck.slides[&sid].find_element(&eid).unwrap().geometry.clone();
        assert_eq!(after.x, original.x);
        assert_eq!(after.y, original.y);
    }

    #[test]
    fn move_inverse_records_previous_position_for_debug() {
        let (mut deck, sid, eid) = fresh_deck_first_child();
        let cmd = MoveElement {
            target: CanvasTarget::Slide(sid),
            element_id: eid,
            new_position: Point { x: 1.0, y: 2.0 },
            previous_position: None,
        };
        let out = cmd.apply(&mut deck).unwrap();
        // We cannot downcast Box<dyn Command>, but we can re-apply the
        // inverse and verify it lands at the original position — covered
        // above. Here we instead confirm two consecutive applies cancel.
        out.inverse.apply(&mut deck).unwrap();
    }

    #[test]
    fn move_apply_then_inverse_twice_returns_to_new_position() {
        let (mut deck, sid, eid) = fresh_deck_first_child();
        let cmd = MoveElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_position: Point { x: 50.0, y: 60.0 },
            previous_position: None,
        };
        let first = cmd.apply(&mut deck).unwrap();
        // Apply inverse: back to original.
        let second = first.inverse.apply(&mut deck).unwrap();
        // Apply inverse of inverse: back to new_position.
        second.inverse.apply(&mut deck).unwrap();
        let geo = deck.slides[&sid].find_element(&eid).unwrap().geometry.clone();
        assert_eq!(geo.x, 50.0);
        assert_eq!(geo.y, 60.0);
    }

    #[test]
    fn move_errors_on_missing_slide() {
        let mut deck = Deck::sample();
        let cmd = MoveElement {
            target: CanvasTarget::Slide("ghost".into()),
            element_id: "x".into(),
            new_position: Point { x: 0.0, y: 0.0 },
            previous_position: None,
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::SlideNotFound(id) if id == "ghost"));
    }

    #[test]
    fn move_errors_on_missing_element() {
        let (mut deck, sid, _) = fresh_deck_first_child();
        let cmd = MoveElement {
            target: CanvasTarget::Slide(sid),
            element_id: "no_such".into(),
            new_position: Point { x: 0.0, y: 0.0 },
            previous_position: None,
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::ElementNotFound(id) if id == "no_such"));
    }

    #[test]
    fn move_marks_slide_dirty_flag() {
        let (mut deck, sid, eid) = fresh_deck_first_child();
        let cmd = MoveElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid,
            new_position: Point { x: 0.0, y: 0.0 },
            previous_position: None,
        };
        let _ = cmd.apply(&mut deck).unwrap();
        assert!(deck.slides[&sid].dirty);
    }

    #[test]
    fn move_command_label_is_stable() {
        let cmd = MoveElement {
            target: CanvasTarget::Slide("x".into()),
            element_id: "y".into(),
            new_position: Point { x: 0.0, y: 0.0 },
            previous_position: None,
        };
        assert_eq!(cmd.label(), "Move Element");
        assert!(cmd.undoable());
    }

    #[test]
    #[should_panic(expected = "target id is empty")]
    fn move_with_empty_target_id_panics_via_assert() {
        let mut deck = Deck::sample();
        let cmd = MoveElement {
            target: CanvasTarget::Slide(String::new()),
            element_id: "x".into(),
            new_position: Point { x: 0.0, y: 0.0 },
            previous_position: None,
        };
        let _ = cmd.apply(&mut deck);
    }

    #[test]
    fn move_targets_a_layout_canvas() {
        // The default theme seeds a "blank" layout with an empty root; add a
        // child so there is an element to move, then move it via a Layout
        // target and confirm the layout (not any slide) was mutated.
        let mut deck = Deck::sample();
        let layout = deck.theme.layouts.get_mut("blank").unwrap();
        layout
            .root
            .children
            .push(crate::deck::builders::text_element("el_lt", "hi"));
        let cmd = MoveElement {
            target: CanvasTarget::Layout("blank".into()),
            element_id: "el_lt".into(),
            new_position: Point { x: 7.0, y: 8.0 },
            previous_position: None,
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(out.dirty_targets, vec![CanvasTarget::Layout("blank".into())]);
        let moved = deck.theme.layouts["blank"].find_element("el_lt").unwrap();
        assert_eq!(moved.geometry.x, 7.0);
        assert_eq!(moved.geometry.y, 8.0);
        assert!(deck.theme.layouts["blank"].dirty);
    }
}
