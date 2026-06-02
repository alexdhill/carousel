// RenameElement command.
//
// Stage 9 — Object Panel. Writes the target element's `name` field (the
// human-readable label shown by the object panel) and emits a matching
// SetAttribute / RemoveAttribute patch so the DOM's `data-name` attribute
// reflects the model.
//
// `new_name = None` clears the name back to the default — the object
// panel falls back to the element id when `name` is empty, so a "cleared"
// element shows its id again. Inverse is symmetric: setting None inverses
// to the previous name (or to None when none was set).

use crate::commands::{Command, CommandError, CommandOutput};
use crate::deck::{ElementId, SlideId};
use crate::ipc::Patch;

#[derive(Debug, Clone)]
pub struct RenameElement {
    pub slide_id: SlideId,
    pub element_id: ElementId,
    pub new_name: Option<String>,
}

impl Command for RenameElement {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with one SetAttribute (when new_name is Some)
    // or RemoveAttribute (when new_name is None) patch on `data-name`,
    // an inverse RenameElement carrying the prior name, the slide marked
    // dirty.
    // Errors: SlideNotFound, ElementNotFound.
    // Dataflow: locate element -> snapshot prior name -> overwrite ->
    // invalidate index -> build patch + inverse.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.slide_id.is_empty(), "RenameElement: slide_id is empty");
        assert!(!self.element_id.is_empty(), "RenameElement: element_id is empty");
        let slide = deck
            .slides
            .get_mut(&self.slide_id)
            .ok_or_else(|| CommandError::SlideNotFound(self.slide_id.clone()))?;
        let element = slide
            .find_element_mut(&self.element_id)
            .ok_or_else(|| CommandError::ElementNotFound(self.element_id.clone()))?;

        let prior: Option<String> = element.name.clone();
        // Normalise empty strings to None so the panel's default-label
        // fallback fires.
        let normalised: Option<String> = match &self.new_name {
            Some(s) if !s.trim().is_empty() => Some(s.clone()),
            _ => None,
        };
        element.name = normalised.clone();
        slide.dirty = true;
        slide.invalidate_index();

        let patches: Vec<Patch> = match normalised {
            Some(name) => vec![Patch::SetAttribute {
                element_id: self.element_id.clone(),
                attribute: "data-name".into(),
                value: name,
            }],
            None => vec![Patch::RemoveAttribute {
                element_id: self.element_id.clone(),
                attribute: "data-name".into(),
            }],
        };

        let inverse: RenameElement = RenameElement {
            slide_id: self.slide_id.clone(),
            element_id: self.element_id.clone(),
            new_name: prior,
        };

        Ok(CommandOutput {
            patches,
            inverse: Box::new(inverse),
            dirty_slides: vec![self.slide_id.clone()],
            manifest_dirty: false,
        })
    }

    fn label(&self) -> &'static str {
        "Rename Element"
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
    fn set_writes_name_and_emits_set_attribute_patch() {
        let (mut deck, sid, eid) = fixture();
        let cmd = RenameElement {
            slide_id: sid.clone(),
            element_id: eid.clone(),
            new_name: Some("Header".into()),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(
            deck.slides[&sid].find_element(&eid).unwrap().name.as_deref(),
            Some("Header")
        );
        match &out.patches[0] {
            Patch::SetAttribute { element_id, attribute, value } => {
                assert_eq!(element_id, &eid);
                assert_eq!(attribute, "data-name");
                assert_eq!(value, "Header");
            }
            other => panic!("expected SetAttribute, got {other:?}"),
        }
    }

    #[test]
    fn clear_removes_attribute_and_resets_name() {
        let (mut deck, sid, eid) = fixture();
        deck.slides
            .get_mut(&sid)
            .unwrap()
            .find_element_mut(&eid)
            .unwrap()
            .name = Some("Existing".into());
        let cmd = RenameElement {
            slide_id: sid.clone(),
            element_id: eid.clone(),
            new_name: None,
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert!(deck.slides[&sid].find_element(&eid).unwrap().name.is_none());
        match &out.patches[0] {
            Patch::RemoveAttribute { attribute, .. } => {
                assert_eq!(attribute, "data-name");
            }
            other => panic!("expected RemoveAttribute, got {other:?}"),
        }
    }

    #[test]
    fn empty_string_treated_as_none() {
        let (mut deck, sid, eid) = fixture();
        let cmd = RenameElement {
            slide_id: sid.clone(),
            element_id: eid.clone(),
            new_name: Some("   ".into()),
        };
        cmd.apply(&mut deck).unwrap();
        assert!(deck.slides[&sid].find_element(&eid).unwrap().name.is_none());
    }

    #[test]
    fn inverse_round_trip_restores_prior_name() {
        let (mut deck, sid, eid) = fixture();
        deck.slides
            .get_mut(&sid)
            .unwrap()
            .find_element_mut(&eid)
            .unwrap()
            .name = Some("Original".into());
        let cmd = RenameElement {
            slide_id: sid.clone(),
            element_id: eid.clone(),
            new_name: Some("Changed".into()),
        };
        let out = cmd.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(
            deck.slides[&sid].find_element(&eid).unwrap().name.as_deref(),
            Some("Original")
        );
    }

    #[test]
    fn affects_object_tree_but_does_not_remount() {
        let cmd = RenameElement {
            slide_id: "s".into(),
            element_id: "e".into(),
            new_name: Some("X".into()),
        };
        assert!(cmd.affects_object_tree());
        assert!(!cmd.requires_remount());
    }

    #[test]
    fn errors_on_missing_slide() {
        let mut deck = Deck::sample();
        let cmd = RenameElement {
            slide_id: "ghost".into(),
            element_id: "x".into(),
            new_name: Some("y".into()),
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::SlideNotFound(id) if id == "ghost"));
    }

    #[test]
    fn errors_on_missing_element() {
        let (mut deck, sid, _) = fixture();
        let cmd = RenameElement {
            slide_id: sid,
            element_id: "no_such".into(),
            new_name: None,
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::ElementNotFound(id) if id == "no_such"));
    }

    #[test]
    fn marks_slide_dirty() {
        let (mut deck, sid, eid) = fixture();
        RenameElement {
            slide_id: sid.clone(),
            element_id: eid,
            new_name: Some("a".into()),
        }
        .apply(&mut deck)
        .unwrap();
        assert!(deck.slides[&sid].dirty);
    }
}
