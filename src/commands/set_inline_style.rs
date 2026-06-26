// SetInlineStyle / RemoveInlineStyle commands.
//
// Stage 8 — Property Inspector. SetInlineStyle writes one CSS declaration
// into the target element's `inline_styles` map and emits a matching
// SetStyle patch. RemoveInlineStyle is the inverse op for keys that did
// not exist before the set; it deletes the key and emits a RemoveStyle
// patch.
//
// Together they cover everything the typed geometry/text fields don't
// own: fill, border, border-radius, box-shadow, and any custom CSS the
// inspector's key:value entry produces. Theme references are not
// resolved at this layer — the value string is what hits the DOM.

use crate::commands::{Command, CommandError, CommandOutput, resolve_canvas_mut};
use crate::deck::{Canvas, CanvasTarget, ElementId, SlideId};
use crate::ipc::Patch;

#[derive(Debug, Clone)]
pub struct SetInlineStyle {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub property: String,
    pub new_value: String,
}

impl Command for SetInlineStyle {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with one SetStyle patch and an inverse that
    // restores the prior key state — either SetInlineStyle (when the key
    // existed) or RemoveInlineStyle (when it did not). Slide marked dirty.
    // Errors: SlideNotFound, ElementNotFound, InvalidOperation on empty
    // property name.
    // Dataflow: locate element -> snapshot prior value -> overwrite the
    // inline_styles entry -> invalidate index -> build patch + inverse.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.target.id().is_empty(),
            "SetInlineStyle: target id is empty"
        );
        assert!(
            !self.element_id.is_empty(),
            "SetInlineStyle: element_id is empty"
        );
        if self.property.trim().is_empty() {
            return Err(CommandError::InvalidOperation(
                "SetInlineStyle: empty property name".into(),
            ));
        }
        let canvas = resolve_canvas_mut(deck, &self.target)?;
        let element = canvas
            .find_element_mut(&self.element_id)
            .ok_or_else(|| CommandError::ElementNotFound(self.element_id.clone()))?;

        let prior: Option<String> = element.inline_styles.get(&self.property).cloned();
        element
            .inline_styles
            .insert(self.property.clone(), self.new_value.clone());
        canvas.mark_dirty();
        canvas.invalidate_index();

        let patch: Patch = Patch::SetStyle {
            element_id: self.element_id.clone(),
            property: self.property.clone(),
            value: self.new_value.clone(),
        };
        let inverse: Box<dyn Command> = match prior {
            Some(old) => Box::new(SetInlineStyle {
                target: self.target.clone(),
                element_id: self.element_id.clone(),
                property: self.property.clone(),
                new_value: old,
            }),
            None => Box::new(RemoveInlineStyle {
                target: self.target.clone(),
                element_id: self.element_id.clone(),
                property: self.property.clone(),
            }),
        };

        Ok(CommandOutput {
            patches: vec![patch],
            inverse,
            dirty_targets: vec![self.target.clone()],
            manifest_dirty: false,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Set Inline Style"
    }
}

#[derive(Debug, Clone)]
pub struct RemoveInlineStyle {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub property: String,
}

impl Command for RemoveInlineStyle {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with one RemoveStyle patch and a
    // SetInlineStyle inverse carrying the deleted value. If the key was
    // absent the apply succeeds as a no-op (no patch, no-op inverse) so
    // the inspector's "clear field" gesture is idempotent.
    // Errors: SlideNotFound, ElementNotFound, InvalidOperation on empty
    // property.
    // Dataflow: locate element -> remove the entry -> invalidate index
    // -> build patch + inverse.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.target.id().is_empty(),
            "RemoveInlineStyle: target id is empty"
        );
        assert!(
            !self.element_id.is_empty(),
            "RemoveInlineStyle: element_id is empty"
        );
        if self.property.trim().is_empty() {
            return Err(CommandError::InvalidOperation(
                "RemoveInlineStyle: empty property name".into(),
            ));
        }
        let canvas = resolve_canvas_mut(deck, &self.target)?;
        let element = canvas
            .find_element_mut(&self.element_id)
            .ok_or_else(|| CommandError::ElementNotFound(self.element_id.clone()))?;

        let prior: Option<String> = element.inline_styles.remove(&self.property);
        canvas.mark_dirty();
        canvas.invalidate_index();

        let (patches, inverse): (Vec<Patch>, Box<dyn Command>) = match prior {
            Some(old) => (
                vec![Patch::RemoveStyle {
                    element_id: self.element_id.clone(),
                    property: self.property.clone(),
                }],
                Box::new(SetInlineStyle {
                    target: self.target.clone(),
                    element_id: self.element_id.clone(),
                    property: self.property.clone(),
                    new_value: old,
                }) as Box<dyn Command>,
            ),
            None => (
                Vec::new(),
                Box::new(RemoveInlineStyle {
                    target: self.target.clone(),
                    element_id: self.element_id.clone(),
                    property: self.property.clone(),
                }) as Box<dyn Command>,
            ),
        };

        Ok(CommandOutput {
            patches,
            inverse,
            dirty_targets: vec![self.target.clone()],
            manifest_dirty: false,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Remove Inline Style"
    }

    fn undoable(&self) -> bool {
        // Clearing an absent key is a no-op; it would create an empty
        // history entry. The dispatcher pushes inverses unconditionally,
        // so we always claim undoable=true and let the patch-buffer
        // coalescer handle no-op cases.
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
    fn set_writes_into_inline_styles_and_emits_patch() {
        let (mut deck, sid, eid) = fixture();
        let cmd = SetInlineStyle {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            property: "background-color".into(),
            new_value: "#ff0066".into(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        let stored = &deck.slides[&sid].find_element(&eid).unwrap().inline_styles;
        assert_eq!(
            stored.get("background-color").map(String::as_str),
            Some("#ff0066")
        );
        match &out.patches[0] {
            Patch::SetStyle {
                property,
                value,
                element_id,
            } => {
                assert_eq!(property, "background-color");
                assert_eq!(value, "#ff0066");
                assert_eq!(element_id, &eid);
            }
            other => panic!("expected SetStyle, got {other:?}"),
        }
    }

    #[test]
    fn set_inverse_when_key_was_absent_is_remove() {
        let (mut deck, sid, eid) = fixture();
        let cmd = SetInlineStyle {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            property: "border".into(),
            new_value: "1px solid #000".into(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();
        assert!(
            !deck.slides[&sid]
                .find_element(&eid)
                .unwrap()
                .inline_styles
                .contains_key("border")
        );
    }

    #[test]
    fn set_inverse_when_key_was_present_restores_prior() {
        let (mut deck, sid, eid) = fixture();
        // Seed an existing inline style.
        deck.slides
            .get_mut(&sid)
            .unwrap()
            .find_element_mut(&eid)
            .unwrap()
            .inline_styles
            .insert("border".into(), "1px dotted #aaa".into());
        let cmd = SetInlineStyle {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            property: "border".into(),
            new_value: "2px solid #000".into(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();
        let stored = &deck.slides[&sid].find_element(&eid).unwrap().inline_styles;
        assert_eq!(
            stored.get("border").map(String::as_str),
            Some("1px dotted #aaa")
        );
    }

    #[test]
    fn remove_existing_key_emits_remove_style() {
        let (mut deck, sid, eid) = fixture();
        deck.slides
            .get_mut(&sid)
            .unwrap()
            .find_element_mut(&eid)
            .unwrap()
            .inline_styles
            .insert("border-radius".into(), "12px".into());
        let cmd = RemoveInlineStyle {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            property: "border-radius".into(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(out.patches.len(), 1);
        match &out.patches[0] {
            Patch::RemoveStyle {
                property,
                element_id,
            } => {
                assert_eq!(property, "border-radius");
                assert_eq!(element_id, &eid);
            }
            other => panic!("expected RemoveStyle, got {other:?}"),
        }
    }

    #[test]
    fn remove_absent_key_is_no_op_with_no_patches() {
        let (mut deck, sid, eid) = fixture();
        let cmd = RemoveInlineStyle {
            target: CanvasTarget::Slide(sid),
            element_id: eid,
            property: "background-color".into(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert!(out.patches.is_empty());
    }

    #[test]
    fn remove_inverse_re_sets_value() {
        let (mut deck, sid, eid) = fixture();
        deck.slides
            .get_mut(&sid)
            .unwrap()
            .find_element_mut(&eid)
            .unwrap()
            .inline_styles
            .insert("background-color".into(), "#abc".into());
        let cmd = RemoveInlineStyle {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            property: "background-color".into(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();
        let stored = &deck.slides[&sid].find_element(&eid).unwrap().inline_styles;
        assert_eq!(
            stored.get("background-color").map(String::as_str),
            Some("#abc")
        );
    }

    #[test]
    fn empty_property_is_rejected() {
        let (mut deck, sid, eid) = fixture();
        let cmd = SetInlineStyle {
            target: CanvasTarget::Slide(sid),
            element_id: eid,
            property: "  ".into(),
            new_value: "anything".into(),
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::InvalidOperation(_)));
    }

    #[test]
    fn set_then_remove_round_trip_clears_state() {
        let (mut deck, sid, eid) = fixture();
        SetInlineStyle {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            property: "border".into(),
            new_value: "1px solid red".into(),
        }
        .apply(&mut deck)
        .unwrap();
        RemoveInlineStyle {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            property: "border".into(),
        }
        .apply(&mut deck)
        .unwrap();
        assert!(
            deck.slides[&sid]
                .find_element(&eid)
                .unwrap()
                .inline_styles
                .is_empty()
        );
    }

    #[test]
    fn marks_slide_dirty() {
        let (mut deck, sid, eid) = fixture();
        SetInlineStyle {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid,
            property: "border".into(),
            new_value: "1px solid #000".into(),
        }
        .apply(&mut deck)
        .unwrap();
        assert!(deck.slides[&sid].dirty);
    }
}
