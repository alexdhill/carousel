// SetMorphTransition command.
//
// Enables or disables the morphing transition for an element on forward
// slide advances. The morph is opt-in per element, stored as three
// data-* attributes that both playback engines consume:
//   - `data-morph-next` = "1" (presence flag; absence = disabled)
//   - `data-morph-dur`  = duration in milliseconds (integer string)
//   - `data-morph-ease` = CSS easing token (e.g., "ease-in-out")
//
// When enabled, the command validates that the next slide (by `slide_order`)
// contains an element with the same id; if not, a warning is added to the
// output but the attributes are still written (never refuse).
//
// The morph is forward-only; backward advance snapshots without animation.
// A flagged element morphs as a single box (geometry only); its children ride
// along and the new element's content fades in over the moving box.

use crate::commands::{Command, CommandError, CommandOutput, resolve_canvas_mut};
use crate::deck::{Canvas, CanvasTarget, Deck, ElementId, SlideId};

// MORPH_ATTRIBUTES: The three attribute keys used by both authoring and
// playback. Keep synchronized with assets/morph.js and other playback layers.
const MORPH_NEXT: &str = "data-morph-next";
const MORPH_DUR: &str = "data-morph-dur";
const MORPH_EASE: &str = "data-morph-ease";

#[derive(Debug, Clone)]
pub struct SetMorphTransition {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub enabled: bool,
    pub duration_ms: u32,
    pub easing: String,
}

impl Command for SetMorphTransition {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with no patches (requires_remount re-serializes),
    // an inverse SetMorphTransition that restores prior attrs, the canvas
    // marked dirty.
    // Errors:
    //   SlideNotFound   — canvas not found.
    //   ElementNotFound — element id not found.
    // Dataflow: locate element -> save prior attrs -> set or clear attrs ->
    // check next slide (if enabled) -> build inverse with saved attrs.
    fn apply(&self, deck: &mut Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.element_id.is_empty(),
            "SetMorphTransition: element_id is empty"
        );
        let canvas = resolve_canvas_mut(deck, &self.target)?;
        let element = canvas
            .find_element_mut(&self.element_id)
            .ok_or_else(|| CommandError::ElementNotFound(self.element_id.clone()))?;

        // Save prior attribute values.
        let prior_next = element.attributes.get(MORPH_NEXT).cloned();
        let prior_dur = element.attributes.get(MORPH_DUR).cloned();
        let prior_ease = element.attributes.get(MORPH_EASE).cloned();

        // Set or clear the attributes.
        if self.enabled {
            assert!(
                self.duration_ms > 0,
                "SetMorphTransition: duration_ms must be > 0 when enabled"
            );
            assert!(
                !self.easing.is_empty(),
                "SetMorphTransition: easing must be non-empty when enabled"
            );
            element.attributes.insert(MORPH_NEXT.into(), "1".into());
            element
                .attributes
                .insert(MORPH_DUR.into(), self.duration_ms.to_string());
            element
                .attributes
                .insert(MORPH_EASE.into(), self.easing.clone());
        } else {
            element.attributes.remove(MORPH_NEXT);
            element.attributes.remove(MORPH_DUR);
            element.attributes.remove(MORPH_EASE);
        }

        canvas.mark_dirty();

        // Check if enabled and the next slide has the element id.
        let mut warnings: Vec<String> = Vec::new();
        if self.enabled {
            if !next_slide_has_id(deck, &self.target, &self.element_id) {
                warnings.push(format!(
                    "No element with id '{}' found on the next slide",
                    self.element_id
                ));
            }
        }

        let inverse = SetMorphTransition {
            target: self.target.clone(),
            element_id: self.element_id.clone(),
            enabled: prior_next.is_some(),
            duration_ms: prior_dur
                .as_ref()
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(300),
            easing: prior_ease.unwrap_or_default(),
        };

        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(inverse),
            dirty_targets: vec![self.target.clone()],
            manifest_dirty: false,
            warnings,
        })
    }

    fn label(&self) -> &'static str {
        "Toggle Slide Transition"
    }

    fn requires_remount(&self) -> bool {
        true
    }

    fn affects_object_tree(&self) -> bool {
        false
    }
}

// next_slide_has_id
// Inputs: deck, a CanvasTarget (must be a Slide), an element id.
// Output: true if the slide after the target's slide contains an element
// with that id. Returns false when there is no next slide or the target
// is not a Slide.
// Dataflow: locate the target slide in slide_order -> check if a next
// slide exists -> search the next slide for the id.
fn next_slide_has_id(deck: &Deck, target: &CanvasTarget, id: &str) -> bool {
    let slide_id = match target {
        CanvasTarget::Slide(sid) => sid,
        CanvasTarget::Layout(_) => return false,
    };
    let pos = match deck.slide_order.iter().position(|s| s == slide_id) {
        Some(p) => p,
        None => return false,
    };
    if pos + 1 >= deck.slide_order.len() {
        return false;
    }
    let next_slide_id = &deck.slide_order[pos + 1];
    match deck.slides.get(next_slide_id) {
        Some(slide) => slide.find_element(id).is_some(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::builders::text_element;

    fn fixture() -> (Deck, SlideId, ElementId) {
        let deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let eid: ElementId = deck.slides[&sid].root.children[0].id.clone();
        (deck, sid, eid)
    }

    #[test]
    fn sets_morph_attributes_when_enabled() {
        let (mut deck, sid, eid) = fixture();
        let cmd = SetMorphTransition {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            enabled: true,
            duration_ms: 500,
            easing: "ease-in-out".into(),
        };
        cmd.apply(&mut deck).unwrap();
        let el = deck.slides[&sid].find_element(&eid).unwrap();
        assert_eq!(el.attributes.get(MORPH_NEXT), Some(&"1".into()));
        assert_eq!(el.attributes.get(MORPH_DUR), Some(&"500".into()));
        assert_eq!(el.attributes.get(MORPH_EASE), Some(&"ease-in-out".into()));
    }

    #[test]
    fn clears_morph_attributes_when_disabled() {
        let (mut deck, sid, eid) = fixture();
        let el = deck
            .slides
            .get_mut(&sid)
            .unwrap()
            .find_element_mut(&eid)
            .unwrap();
        el.attributes.insert(MORPH_NEXT.into(), "1".into());
        el.attributes.insert(MORPH_DUR.into(), "300".into());
        el.attributes.insert(MORPH_EASE.into(), "ease-out".into());

        let cmd = SetMorphTransition {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            enabled: false,
            duration_ms: 0,
            easing: String::new(),
        };
        cmd.apply(&mut deck).unwrap();
        let el = deck.slides[&sid].find_element(&eid).unwrap();
        assert!(!el.attributes.contains_key(MORPH_NEXT));
        assert!(!el.attributes.contains_key(MORPH_DUR));
        assert!(!el.attributes.contains_key(MORPH_EASE));
    }

    #[test]
    fn inverse_restores_prior_attributes() {
        let (mut deck, sid, eid) = fixture();
        let cmd = SetMorphTransition {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            enabled: true,
            duration_ms: 400,
            easing: "linear".into(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();
        let el = deck.slides[&sid].find_element(&eid).unwrap();
        assert!(!el.attributes.contains_key(MORPH_NEXT));
        assert!(!el.attributes.contains_key(MORPH_DUR));
        assert!(!el.attributes.contains_key(MORPH_EASE));
    }

    #[test]
    fn warns_when_enabled_and_next_slide_lacks_id() {
        let mut deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let eid: ElementId = deck.slides[&sid].root.children[0].id.clone();

        // Add a second slide without a matching element.
        let sid2: SlideId = "s_second".into();
        let root2 = crate::deck::builders::group_element("el_rt2", vec![]);
        let slide2 = crate::deck::slide::SlideNode::new("s_second".into(), "blank".into(), root2);
        deck.slides.insert(sid2.clone(), slide2);
        deck.slide_order.push(sid2);

        let cmd = SetMorphTransition {
            target: CanvasTarget::Slide(sid),
            element_id: eid.clone(),
            enabled: true,
            duration_ms: 300,
            easing: "ease".into(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert!(!out.warnings.is_empty());
        assert!(out.warnings[0].contains(&eid));
    }

    #[test]
    fn no_warning_when_next_slide_has_id() {
        let mut deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let eid: ElementId = deck.slides[&sid].root.children[0].id.clone();

        // Add a second slide WITH a matching element.
        let sid2: SlideId = "s_second".into();
        let matching_el = text_element(&eid, "matched");
        let root2 = crate::deck::builders::group_element("el_rt2", vec![matching_el]);
        let slide2 = crate::deck::slide::SlideNode::new("s_second".into(), "blank".into(), root2);
        deck.slides.insert(sid2.clone(), slide2);
        deck.slide_order.push(sid2);

        let cmd = SetMorphTransition {
            target: CanvasTarget::Slide(sid),
            element_id: eid.clone(),
            enabled: true,
            duration_ms: 300,
            easing: "ease".into(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn no_warning_when_disabled() {
        let mut deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let eid: ElementId = deck.slides[&sid].root.children[0].id.clone();

        // Add a second slide WITHOUT a matching element, but disable the morph.
        let sid2: SlideId = "s_second".into();
        let root2 = crate::deck::builders::group_element("el_rt2", vec![]);
        let slide2 = crate::deck::slide::SlideNode::new("s_second".into(), "blank".into(), root2);
        deck.slides.insert(sid2.clone(), slide2);
        deck.slide_order.push(sid2);

        let cmd = SetMorphTransition {
            target: CanvasTarget::Slide(sid),
            element_id: eid.clone(),
            enabled: false,
            duration_ms: 0,
            easing: String::new(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn marks_canvas_dirty() {
        let (mut deck, sid, eid) = fixture();
        let cmd = SetMorphTransition {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            enabled: true,
            duration_ms: 300,
            easing: "ease".into(),
        };
        cmd.apply(&mut deck).unwrap();
        assert!(deck.slides[&sid].dirty);
    }

    #[test]
    fn remounts_and_does_not_affect_object_tree() {
        let cmd = SetMorphTransition {
            target: CanvasTarget::Slide("s".into()),
            element_id: "el_test".into(),
            enabled: true,
            duration_ms: 300,
            easing: "ease".into(),
        };
        assert!(cmd.requires_remount());
        assert!(!cmd.affects_object_tree());
    }

    #[test]
    fn errors_on_missing_element() {
        let (mut deck, sid, _) = fixture();
        let cmd = SetMorphTransition {
            target: CanvasTarget::Slide(sid),
            element_id: "ghost".into(),
            enabled: true,
            duration_ms: 300,
            easing: "ease".into(),
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::ElementNotFound(_)));
    }

    #[test]
    fn asserts_enabled_requires_positive_duration() {
        let (mut deck, sid, eid) = fixture();
        let cmd = SetMorphTransition {
            target: CanvasTarget::Slide(sid),
            element_id: eid.clone(),
            enabled: true,
            duration_ms: 0,
            easing: "ease".into(),
        };
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cmd.apply(&mut deck)));
        assert!(result.is_err());
    }

    #[test]
    fn asserts_enabled_requires_non_empty_easing() {
        let (mut deck, sid, eid) = fixture();
        let cmd = SetMorphTransition {
            target: CanvasTarget::Slide(sid),
            element_id: eid.clone(),
            enabled: true,
            duration_ms: 300,
            easing: String::new(),
        };
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cmd.apply(&mut deck)));
        assert!(result.is_err());
    }
}
