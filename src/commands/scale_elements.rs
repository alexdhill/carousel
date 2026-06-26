// SetElementsTransform command.
//
// Multi-select proportional scale (and the move-all path could reuse it, but
// drag uses a CompositeCommand of MoveElement). Carries an ABSOLUTE target
// state per element — geometry plus optional text font-size and optional group
// scale — and applies them in one mutation. `apply` captures the prior state
// of every touched field into the inverse (another SetElementsTransform), so
// undo/redo round-trips exactly in a single step.
//
// The interpret layer (app.rs) computes the target states from a scale factor
// and anchor by reading current geometry/font/scale; this command is the
// dumb, undoable writer.

use crate::commands::{Command, CommandError, CommandOutput, resolve_canvas_mut};
use crate::deck::element::ElementStyle;
use crate::deck::style::Length;
use crate::deck::{Canvas, CanvasTarget, ElementId};

const MIN_PX: f64 = 1.0;

#[derive(Debug, Clone)]
pub struct ElementTransform {
    pub id: ElementId,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    // Set only for text elements (absolute px); None leaves the font as-is.
    pub font_size_px: Option<f64>,
    // Set only for groups (absolute uniform scale); None leaves it as-is.
    pub group_scale: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct SetElementsTransform {
    pub target: CanvasTarget,
    pub items: Vec<ElementTransform>,
}

impl Command for SetElementsTransform {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with no patches (requires_remount re-renders the
    // canvas), an inverse SetElementsTransform carrying the captured prior
    // state of each element, and the canvas marked dirty.
    // Errors: SlideNotFound / ElementNotFound (a missing id aborts).
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.target.id().is_empty(),
            "SetElementsTransform: target id empty"
        );
        assert!(!self.items.is_empty(), "SetElementsTransform: no items");
        let canvas = resolve_canvas_mut(deck, &self.target)?;
        let mut prior: Vec<ElementTransform> = Vec::with_capacity(self.items.len());
        for it in &self.items {
            let el = canvas
                .find_element_mut(&it.id)
                .ok_or_else(|| CommandError::ElementNotFound(it.id.clone()))?;
            let mut p = ElementTransform {
                id: it.id.clone(),
                x: el.geometry.x,
                y: el.geometry.y,
                width: el.geometry.width,
                height: el.geometry.height,
                font_size_px: None,
                group_scale: None,
            };
            el.geometry.x = it.x;
            el.geometry.y = it.y;
            el.geometry.width = it.width.max(MIN_PX);
            el.geometry.height = it.height.max(MIN_PX);
            if let Some(fs) = it.font_size_px {
                if let ElementStyle::Text(ts) = &mut el.style {
                    p.font_size_px = Some(ts.font_size.value);
                    ts.font_size = Length::px(fs.max(MIN_PX));
                }
            }
            if let Some(gsf) = it.group_scale {
                if let ElementStyle::Group(gs) = &mut el.style {
                    p.group_scale = Some(gs.scale);
                    if gsf > 0.0 {
                        gs.scale = gsf;
                    }
                }
            }
            prior.push(p);
        }
        canvas.mark_dirty();
        canvas.invalidate_index();
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(SetElementsTransform {
                target: self.target.clone(),
                items: prior,
            }),
            dirty_targets: vec![self.target.clone()],
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Scale Elements"
    }

    fn requires_remount(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::{Deck, SlideId};

    fn fixture() -> (Deck, SlideId, ElementId) {
        let deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let eid: ElementId = deck.slides[&sid].root.children[0].id.clone();
        (deck, sid, eid)
    }

    #[test]
    fn scales_geometry_and_text_font_size_and_inverts() {
        let (mut deck, sid, eid) = fixture();
        // Make the element a known size + font.
        {
            let el = deck
                .slides
                .get_mut(&sid)
                .unwrap()
                .find_element_mut(&eid)
                .unwrap();
            el.geometry.x = 100.0;
            el.geometry.y = 100.0;
            el.geometry.width = 200.0;
            el.geometry.height = 100.0;
            if let ElementStyle::Text(ts) = &mut el.style {
                ts.font_size = Length::px(20.0);
            }
        }
        let is_text = matches!(
            deck.slides[&sid].find_element(&eid).unwrap().style,
            ElementStyle::Text(_)
        );
        // Scale 2x about anchor (0,0): pos and size double; font 20 → 40.
        let item = ElementTransform {
            id: eid.clone(),
            x: 200.0,
            y: 200.0,
            width: 400.0,
            height: 200.0,
            font_size_px: if is_text { Some(40.0) } else { None },
            group_scale: None,
        };
        let cmd = SetElementsTransform {
            target: CanvasTarget::Slide(sid.clone()),
            items: vec![item],
        };
        let out = cmd.apply(&mut deck).unwrap();
        {
            let el = deck.slides[&sid].find_element(&eid).unwrap();
            assert_eq!(el.geometry.x, 200.0);
            assert_eq!(el.geometry.width, 400.0);
            if let ElementStyle::Text(ts) = &el.style {
                assert_eq!(ts.font_size.value, 40.0);
            }
        }
        out.inverse.apply(&mut deck).unwrap();
        let el = deck.slides[&sid].find_element(&eid).unwrap();
        assert_eq!(el.geometry.x, 100.0);
        assert_eq!(el.geometry.width, 200.0);
        if is_text {
            if let ElementStyle::Text(ts) = &el.style {
                assert_eq!(ts.font_size.value, 20.0);
            }
        }
    }

    #[test]
    fn missing_element_errors() {
        let (mut deck, sid, _eid) = fixture();
        let cmd = SetElementsTransform {
            target: CanvasTarget::Slide(sid),
            items: vec![ElementTransform {
                id: "ghost".into(),
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                font_size_px: None,
                group_scale: None,
            }],
        };
        assert!(matches!(
            cmd.apply(&mut deck),
            Err(CommandError::ElementNotFound(_))
        ));
    }
}
