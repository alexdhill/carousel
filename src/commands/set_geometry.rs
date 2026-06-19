// SetGeometryProperty command.
//
// Stage 8 — Property Inspector. Writes one geometry field at a time (x, y,
// width, height, rotation, opacity), emits the matching SetStyle patch,
// and produces an inverse SetGeometryProperty that restores the prior
// value. Z-order is intentionally NOT a writable geometry property because
// the serializer derives z-index from sibling position (Stage 8 §17 note).
//
// Compared with MoveElement (which always writes x AND y together), this
// command is purpose-built for the inspector's per-input edit model:
// "the user typed a new value into the width input" → one
// SetGeometryProperty(Width, ...). MoveElement remains the right tool for
// drag-end (atomic x+y update).

use crate::commands::{Command, CommandError, CommandOutput, resolve_canvas_mut};
use crate::deck::style::Geometry;
use crate::deck::{Canvas, CanvasTarget, ElementId, SlideId};
use crate::ipc::Patch;

// GeometryProperty
// Tag identifying which scalar field of Geometry a SetGeometryProperty
// command will mutate. Kept as an enum (not a string) so callers cannot
// pass an unknown property name and the matcher is exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometryProperty {
    X,
    Y,
    Width,
    Height,
    Rotation,
    Opacity,
}

impl GeometryProperty {
    // css_property
    // Inputs: self.
    // Output: the CSS property name a SetStyle patch must carry to mirror
    // a write of this geometry field in the DOM.
    pub fn css_property(&self) -> &'static str {
        match self {
            Self::X => "left",
            Self::Y => "top",
            Self::Width => "width",
            Self::Height => "height",
            Self::Rotation => "transform",
            Self::Opacity => "opacity",
        }
    }

    // from_inspector_key
    // Inputs: a string the inspector posts in the PropertyChanged event
    // ("x", "y", "width", "height", "rotation", "opacity").
    // Output: Some(GeometryProperty) on a known key, None otherwise.
    // Dataflow: pure lookup; the interpret layer uses this to decide
    // whether a PropertyChanged routes to SetGeometryProperty or to
    // SetInlineStyle.
    pub fn from_inspector_key(key: &str) -> Option<Self> {
        match key {
            "x" => Some(Self::X),
            "y" => Some(Self::Y),
            "width" => Some(Self::Width),
            "height" => Some(Self::Height),
            "rotation" => Some(Self::Rotation),
            "opacity" => Some(Self::Opacity),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SetGeometryProperty {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub property: GeometryProperty,
    pub new_value: f64,
}

impl Command for SetGeometryProperty {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with one SetStyle patch (CSS property derived
    // from self.property), an inverse SetGeometryProperty carrying the
    // prior value, and the slide marked dirty.
    // Errors: SlideNotFound, ElementNotFound.
    // Dataflow: locate slide -> locate element -> snapshot prior scalar
    // -> overwrite -> invalidate index -> build patch + inverse.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.target.id().is_empty(), "SetGeometryProperty: target id is empty");
        assert!(!self.element_id.is_empty(), "SetGeometryProperty: element_id is empty");
        let canvas = resolve_canvas_mut(deck, &self.target)?;
        let element = canvas
            .find_element_mut(&self.element_id)
            .ok_or_else(|| CommandError::ElementNotFound(self.element_id.clone()))?;

        let prior: f64 = read_field(&element.geometry, self.property);
        write_field(&mut element.geometry, self.property, self.new_value);
        canvas.mark_dirty();
        canvas.invalidate_index();

        let css_value: String = format_css_value(self.property, self.new_value);
        let mut patches: Vec<Patch> = vec![Patch::SetStyle {
            element_id: self.element_id.clone(),
            property: self.property.css_property().to_string(),
            value: css_value,
        }];
        patches.extend(crate::commands::relayout_patches(canvas.root_mut(), &self.element_id));

        let inverse: SetGeometryProperty = SetGeometryProperty {
            target: self.target.clone(),
            element_id: self.element_id.clone(),
            property: self.property,
            new_value: prior,
        };

        Ok(CommandOutput {
            patches,
            inverse: Box::new(inverse),
            dirty_targets: vec![self.target.clone()],
            manifest_dirty: false,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        match self.property {
            GeometryProperty::X | GeometryProperty::Y => "Move Element",
            GeometryProperty::Width | GeometryProperty::Height => "Resize Element",
            GeometryProperty::Rotation => "Rotate Element",
            GeometryProperty::Opacity => "Change Opacity",
        }
    }
}

// read_field
// Inputs: geometry, which scalar to read.
// Output: the current f64 value for that field.
fn read_field(g: &Geometry, p: GeometryProperty) -> f64 {
    match p {
        GeometryProperty::X => g.x,
        GeometryProperty::Y => g.y,
        GeometryProperty::Width => g.width,
        GeometryProperty::Height => g.height,
        GeometryProperty::Rotation => g.rotation,
        GeometryProperty::Opacity => g.opacity,
    }
}

// write_field
// Inputs: geometry, which scalar to update, the new value.
// Output: side-effect; overwrites the named field.
fn write_field(g: &mut Geometry, p: GeometryProperty, v: f64) {
    match p {
        GeometryProperty::X => g.x = v,
        GeometryProperty::Y => g.y = v,
        GeometryProperty::Width => g.width = v,
        GeometryProperty::Height => g.height = v,
        GeometryProperty::Rotation => g.rotation = v,
        GeometryProperty::Opacity => g.opacity = v,
    }
}

// format_css_value
// Inputs: which property, the value.
// Output: the CSS value string the SetStyle patch will carry. Position
// and size go in px; rotation becomes `rotate(<v>rad)` so the parser's
// existing transform rules work; opacity is a bare number.
fn format_css_value(p: GeometryProperty, v: f64) -> String {
    match p {
        GeometryProperty::X
        | GeometryProperty::Y
        | GeometryProperty::Width
        | GeometryProperty::Height => format!("{v}px"),
        GeometryProperty::Rotation => format!("rotate({v}rad)"),
        GeometryProperty::Opacity => format!("{v}"),
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

    fn run(p: GeometryProperty, v: f64) -> (Deck, SlideId, ElementId, CommandOutput) {
        let (mut deck, sid, eid) = fixture();
        let cmd = SetGeometryProperty {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            property: p,
            new_value: v,
        };
        let out = cmd.apply(&mut deck).unwrap();
        (deck, sid, eid, out)
    }

    #[test]
    fn set_width_updates_geometry_only_in_that_field() {
        let (mut deck, sid, eid) = fixture();
        let before = deck.slides[&sid].find_element(&eid).unwrap().geometry.clone();
        let cmd = SetGeometryProperty {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            property: GeometryProperty::Width,
            new_value: 500.0,
        };
        cmd.apply(&mut deck).unwrap();
        let after = deck.slides[&sid].find_element(&eid).unwrap().geometry.clone();
        assert_eq!(after.width, 500.0);
        // Everything else untouched.
        assert_eq!(after.x, before.x);
        assert_eq!(after.y, before.y);
        assert_eq!(after.height, before.height);
        assert_eq!(after.rotation, before.rotation);
        assert_eq!(after.opacity, before.opacity);
    }

    #[test]
    fn set_x_emits_set_style_left_in_px() {
        let (_, _, eid, out) = run(GeometryProperty::X, 123.0);
        match &out.patches[0] {
            Patch::SetStyle { element_id, property, value } => {
                assert_eq!(element_id, &eid);
                assert_eq!(property, "left");
                assert_eq!(value, "123px");
            }
            other => panic!("expected SetStyle, got {other:?}"),
        }
    }

    #[test]
    fn set_rotation_emits_rotate_rad_transform() {
        let (_, _, _, out) = run(GeometryProperty::Rotation, 0.5);
        match &out.patches[0] {
            Patch::SetStyle { property, value, .. } => {
                assert_eq!(property, "transform");
                assert_eq!(value, "rotate(0.5rad)");
            }
            other => panic!("expected SetStyle(transform), got {other:?}"),
        }
    }

    #[test]
    fn set_opacity_emits_bare_number() {
        let (_, _, _, out) = run(GeometryProperty::Opacity, 0.75);
        match &out.patches[0] {
            Patch::SetStyle { property, value, .. } => {
                assert_eq!(property, "opacity");
                assert_eq!(value, "0.75");
            }
            other => panic!("expected SetStyle(opacity), got {other:?}"),
        }
    }

    #[test]
    fn inverse_restores_prior_value() {
        let (mut deck, sid, eid) = fixture();
        let original_h = deck.slides[&sid].find_element(&eid).unwrap().geometry.height;
        let cmd = SetGeometryProperty {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            property: GeometryProperty::Height,
            new_value: 999.0,
        };
        let out = cmd.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();
        let after = deck.slides[&sid].find_element(&eid).unwrap().geometry.height;
        assert_eq!(after, original_h);
    }

    #[test]
    fn labels_are_stable_and_undoable() {
        let cmd = SetGeometryProperty {
            target: CanvasTarget::Slide("s".into()),
            element_id: "e".into(),
            property: GeometryProperty::Width,
            new_value: 1.0,
        };
        assert_eq!(cmd.label(), "Resize Element");
        assert!(cmd.undoable());
    }

    #[test]
    fn errors_on_missing_slide() {
        let mut deck = Deck::sample();
        let cmd = SetGeometryProperty {
            target: CanvasTarget::Slide("ghost".into()),
            element_id: "x".into(),
            property: GeometryProperty::X,
            new_value: 0.0,
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::SlideNotFound(id) if id == "ghost"));
    }

    #[test]
    fn errors_on_missing_element() {
        let (mut deck, sid, _) = fixture();
        let cmd = SetGeometryProperty {
            target: CanvasTarget::Slide(sid),
            element_id: "no_such".into(),
            property: GeometryProperty::X,
            new_value: 0.0,
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::ElementNotFound(id) if id == "no_such"));
    }

    #[test]
    fn set_geometry_inside_group_relayouts() {
        use crate::deck::builders::{group_element, text_element};
        use crate::deck::element::ElementStyle;
        use crate::deck::style::{GroupAlignment, GroupStyle};
        let mut deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let mut a = text_element("ca", "t"); a.geometry.width = 20.0; a.geometry.height = 10.0;
        let mut b = text_element("cb", "t"); b.geometry.x = 30.0; b.geometry.width = 20.0; b.geometry.height = 10.0;
        let mut g = group_element("cg", vec![a, b]);
        g.style = ElementStyle::Group(GroupStyle { alignment: GroupAlignment::Start, ..Default::default() });
        deck.slides.get_mut(&sid).unwrap().root.children.push(g);
        let cmd = SetGeometryProperty {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: "cb".into(),
            property: GeometryProperty::Width,
            new_value: 120.0,
        };
        let out = cmd.apply(&mut deck).unwrap();
        let g = deck.slides[&sid].find_element("cg").unwrap();
        assert!(g.geometry.width >= 150.0);
        assert!(out.patches.iter().any(|p| matches!(p,
            Patch::SetStyle { element_id, property, .. } if element_id == "cg" && property == "width")));
    }

    #[test]
    fn from_inspector_key_known_strings_map() {
        for (k, p) in [
            ("x", GeometryProperty::X),
            ("y", GeometryProperty::Y),
            ("width", GeometryProperty::Width),
            ("height", GeometryProperty::Height),
            ("rotation", GeometryProperty::Rotation),
            ("opacity", GeometryProperty::Opacity),
        ] {
            assert_eq!(GeometryProperty::from_inspector_key(k), Some(p));
        }
        assert_eq!(GeometryProperty::from_inspector_key("color"), None);
        assert_eq!(GeometryProperty::from_inspector_key(""), None);
    }

    #[test]
    fn marks_slide_dirty() {
        let (mut deck, sid, eid) = fixture();
        let cmd = SetGeometryProperty {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid,
            property: GeometryProperty::X,
            new_value: 5.0,
        };
        cmd.apply(&mut deck).unwrap();
        assert!(deck.slides[&sid].dirty);
    }
}
