// ResizeElement command.
//
// Stage 8/10 — Resize handles. Writes an element's full (x, y, width,
// height) rect in a single atomic mutation, emitting four SetStyle
// patches (left, top, width, height). Used by the selection overlay's
// 8-handle resize gesture; one transaction wraps one ElementResizeEnded
// event, so the user sees a single undo entry per resize regardless of
// how many mid-drag frames fired.
//
// Width and height are clamped to MIN_DIMENSION_PX (1.0). The JS host
// clamps too — this clamp here is the safety net for malformed IPC.
// Negative or near-zero dimensions on the wire would otherwise produce
// a degenerate element that's hard to select again.

use crate::commands::{Command, CommandError, CommandOutput};
use crate::deck::{ElementId, SlideId};
use crate::ipc::Patch;

pub const MIN_DIMENSION_PX: f64 = 1.0;

#[derive(Debug, Clone)]
pub struct ResizeElement {
    pub slide_id: SlideId,
    pub element_id: ElementId,
    pub new_x: f64,
    pub new_y: f64,
    pub new_width: f64,
    pub new_height: f64,
}

impl Command for ResizeElement {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with four SetStyle patches (left, top,
    // width, height), an inverse ResizeElement carrying the prior rect,
    // and the slide marked dirty.
    // Errors: SlideNotFound, ElementNotFound.
    // Dataflow: locate slide -> locate element -> snapshot prior rect ->
    // clamp width/height to MIN_DIMENSION_PX -> overwrite -> invalidate
    // index -> build the four CSS patches + inverse.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.slide_id.is_empty(), "ResizeElement: slide_id is empty");
        assert!(!self.element_id.is_empty(), "ResizeElement: element_id is empty");
        let slide = deck
            .slides
            .get_mut(&self.slide_id)
            .ok_or_else(|| CommandError::SlideNotFound(self.slide_id.clone()))?;
        let element = slide
            .find_element_mut(&self.element_id)
            .ok_or_else(|| CommandError::ElementNotFound(self.element_id.clone()))?;

        let prior_x: f64 = element.geometry.x;
        let prior_y: f64 = element.geometry.y;
        let prior_w: f64 = element.geometry.width;
        let prior_h: f64 = element.geometry.height;

        let clamped_w: f64 = self.new_width.max(MIN_DIMENSION_PX);
        let clamped_h: f64 = self.new_height.max(MIN_DIMENSION_PX);

        element.geometry.x = self.new_x;
        element.geometry.y = self.new_y;
        element.geometry.width = clamped_w;
        element.geometry.height = clamped_h;
        slide.dirty = true;
        slide.invalidate_index();

        let patches: Vec<Patch> = vec![
            Patch::SetStyle {
                element_id: self.element_id.clone(),
                property: "left".into(),
                value: format!("{}px", self.new_x),
            },
            Patch::SetStyle {
                element_id: self.element_id.clone(),
                property: "top".into(),
                value: format!("{}px", self.new_y),
            },
            Patch::SetStyle {
                element_id: self.element_id.clone(),
                property: "width".into(),
                value: format!("{}px", clamped_w),
            },
            Patch::SetStyle {
                element_id: self.element_id.clone(),
                property: "height".into(),
                value: format!("{}px", clamped_h),
            },
        ];

        let inverse: ResizeElement = ResizeElement {
            slide_id: self.slide_id.clone(),
            element_id: self.element_id.clone(),
            new_x: prior_x,
            new_y: prior_y,
            new_width: prior_w,
            new_height: prior_h,
        };

        Ok(CommandOutput {
            patches,
            inverse: Box::new(inverse),
            dirty_slides: vec![self.slide_id.clone()],
            manifest_dirty: false,
        })
    }

    fn label(&self) -> &'static str {
        "Resize Element"
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
    fn apply_writes_all_four_geometry_fields() {
        let (mut deck, sid, eid) = fixture();
        let cmd = ResizeElement {
            slide_id: sid.clone(),
            element_id: eid.clone(),
            new_x: 100.0,
            new_y: 200.0,
            new_width: 400.0,
            new_height: 250.0,
        };
        cmd.apply(&mut deck).unwrap();
        let g = deck.slides[&sid].find_element(&eid).unwrap().geometry.clone();
        assert_eq!(g.x, 100.0);
        assert_eq!(g.y, 200.0);
        assert_eq!(g.width, 400.0);
        assert_eq!(g.height, 250.0);
    }

    #[test]
    fn apply_emits_four_set_style_patches() {
        let (mut deck, sid, eid) = fixture();
        let cmd = ResizeElement {
            slide_id: sid,
            element_id: eid.clone(),
            new_x: 1.0,
            new_y: 2.0,
            new_width: 3.0,
            new_height: 4.0,
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(out.patches.len(), 4);
        let mut props: Vec<&str> = Vec::new();
        for p in &out.patches {
            match p {
                Patch::SetStyle { element_id, property, .. } => {
                    assert_eq!(element_id, &eid);
                    props.push(property.as_str());
                }
                other => panic!("expected SetStyle, got {other:?}"),
            }
        }
        assert!(props.contains(&"left"));
        assert!(props.contains(&"top"));
        assert!(props.contains(&"width"));
        assert!(props.contains(&"height"));
    }

    #[test]
    fn inverse_round_trip_restores_prior_rect() {
        let (mut deck, sid, eid) = fixture();
        let before = deck.slides[&sid].find_element(&eid).unwrap().geometry.clone();
        let cmd = ResizeElement {
            slide_id: sid.clone(),
            element_id: eid.clone(),
            new_x: 999.0,
            new_y: 888.0,
            new_width: 777.0,
            new_height: 666.0,
        };
        let out = cmd.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();
        let after = deck.slides[&sid].find_element(&eid).unwrap().geometry.clone();
        assert_eq!(after.x, before.x);
        assert_eq!(after.y, before.y);
        assert_eq!(after.width, before.width);
        assert_eq!(after.height, before.height);
    }

    #[test]
    fn width_height_clamped_to_min_dimension() {
        let (mut deck, sid, eid) = fixture();
        let cmd = ResizeElement {
            slide_id: sid.clone(),
            element_id: eid.clone(),
            new_x: 0.0,
            new_y: 0.0,
            new_width: -100.0,
            new_height: 0.0,
        };
        cmd.apply(&mut deck).unwrap();
        let g = deck.slides[&sid].find_element(&eid).unwrap().geometry.clone();
        assert_eq!(g.width, MIN_DIMENSION_PX);
        assert_eq!(g.height, MIN_DIMENSION_PX);
    }

    #[test]
    fn label_and_undoable_are_stable() {
        let cmd = ResizeElement {
            slide_id: "s".into(),
            element_id: "e".into(),
            new_x: 0.0,
            new_y: 0.0,
            new_width: 1.0,
            new_height: 1.0,
        };
        assert_eq!(cmd.label(), "Resize Element");
        assert!(cmd.undoable());
        assert!(!cmd.affects_object_tree());
        assert!(!cmd.requires_remount());
    }

    #[test]
    fn errors_on_missing_slide() {
        let mut deck = Deck::sample();
        let cmd = ResizeElement {
            slide_id: "ghost".into(),
            element_id: "x".into(),
            new_x: 0.0,
            new_y: 0.0,
            new_width: 1.0,
            new_height: 1.0,
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::SlideNotFound(id) if id == "ghost"));
    }

    #[test]
    fn errors_on_missing_element() {
        let (mut deck, sid, _) = fixture();
        let cmd = ResizeElement {
            slide_id: sid,
            element_id: "no_such".into(),
            new_x: 0.0,
            new_y: 0.0,
            new_width: 1.0,
            new_height: 1.0,
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::ElementNotFound(id) if id == "no_such"));
    }

    #[test]
    fn marks_slide_dirty() {
        let (mut deck, sid, eid) = fixture();
        ResizeElement {
            slide_id: sid.clone(),
            element_id: eid,
            new_x: 0.0,
            new_y: 0.0,
            new_width: 50.0,
            new_height: 50.0,
        }
        .apply(&mut deck)
        .unwrap();
        assert!(deck.slides[&sid].dirty);
    }
}
