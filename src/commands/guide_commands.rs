// AddGuide / MoveGuide / RemoveGuide — editing a canvas's saveable guides.
//
// Guides are not part of the element tree, so these commands emit no DOM
// patches. They mutate the target canvas's `guides` vec, mark it dirty, and
// report `affects_guides()` so the dispatcher re-broadcasts the guide set to
// the editor (which redraws the overlay). Indices address the *target* canvas's
// own guides; a slide never edits its layout's (inherited) guides through these.

use crate::commands::{Command, CommandError, CommandOutput, resolve_canvas_mut};
use crate::deck::guide::{Guide, GuideAxis};
use crate::deck::{CanvasTarget, Deck};

// AddGuide
// Adds a guide to the target canvas. `index` is None for the normal "drag a new
// guide off the ruler" gesture (append); Some(i) is used only as the inverse of
// RemoveGuide, restoring a removed guide at its original position so undo does
// not reorder the list.
#[derive(Debug, Clone)]
pub struct AddGuide {
    pub target: CanvasTarget,
    pub axis: GuideAxis,
    pub pos: f64,
    pub index: Option<usize>,
}

impl Command for AddGuide {
    fn apply(&self, deck: &mut Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.target.id().is_empty(), "AddGuide: empty target id");
        let canvas = resolve_canvas_mut(deck, &self.target)?;
        let guides = canvas.guides_mut();
        let at: usize = match self.index {
            Some(i) if i <= guides.len() => i,
            Some(_) => return Err(CommandError::InvalidOperation("AddGuide: index out of range".into())),
            None => guides.len(),
        };
        guides.insert(at, Guide::new(self.axis, self.pos));
        canvas.mark_dirty();
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(RemoveGuide {
                target: self.target.clone(),
                index: at,
            }),
            dirty_targets: vec![self.target.clone()],
            manifest_dirty: false,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Add Guide"
    }

    fn affects_guides(&self) -> bool {
        true
    }
}

// MoveGuide
// Repositions the guide at `index` to `new_pos`. The inverse carries the prior
// position; consecutive moves of the same guide coalesce in the patch buffer so
// a drag is a single undo step.
#[derive(Debug, Clone)]
pub struct MoveGuide {
    pub target: CanvasTarget,
    pub index: usize,
    pub new_pos: f64,
}

impl Command for MoveGuide {
    fn apply(&self, deck: &mut Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.target.id().is_empty(), "MoveGuide: empty target id");
        let canvas = resolve_canvas_mut(deck, &self.target)?;
        let guides = canvas.guides_mut();
        let prior: f64 = match guides.get_mut(self.index) {
            Some(g) => {
                let old = g.pos;
                g.pos = self.new_pos;
                old
            }
            None => return Err(CommandError::InvalidOperation("MoveGuide: index out of range".into())),
        };
        canvas.mark_dirty();
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(MoveGuide {
                target: self.target.clone(),
                index: self.index,
                new_pos: prior,
            }),
            dirty_targets: vec![self.target.clone()],
            manifest_dirty: false,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Move Guide"
    }

    fn affects_guides(&self) -> bool {
        true
    }
}

// RemoveGuide
// Deletes the guide at `index`. The inverse re-adds it at the same index so
// undo restores both the guide and its list position.
#[derive(Debug, Clone)]
pub struct RemoveGuide {
    pub target: CanvasTarget,
    pub index: usize,
}

impl Command for RemoveGuide {
    fn apply(&self, deck: &mut Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.target.id().is_empty(), "RemoveGuide: empty target id");
        let canvas = resolve_canvas_mut(deck, &self.target)?;
        let guides = canvas.guides_mut();
        if self.index >= guides.len() {
            return Err(CommandError::InvalidOperation("RemoveGuide: index out of range".into()));
        }
        let removed: Guide = guides.remove(self.index);
        canvas.mark_dirty();
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(AddGuide {
                target: self.target.clone(),
                axis: removed.axis,
                pos: removed.pos,
                index: Some(self.index),
            }),
            dirty_targets: vec![self.target.clone()],
            manifest_dirty: false,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Remove Guide"
    }

    fn affects_guides(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;

    fn deck_with_slide() -> (Deck, CanvasTarget) {
        let deck = Deck::sample();
        let sid = deck.slide_order[0].clone();
        (deck, CanvasTarget::Slide(sid))
    }

    fn guides(deck: &Deck, target: &CanvasTarget) -> Vec<Guide> {
        deck.canvas(target).unwrap().guides().clone()
    }

    #[test]
    fn add_appends_and_inverse_removes() {
        let (mut deck, t) = deck_with_slide();
        let out = AddGuide { target: t.clone(), axis: GuideAxis::Vertical, pos: 120.0, index: None }
            .apply(&mut deck)
            .unwrap();
        assert_eq!(guides(&deck, &t).len(), 1);
        assert_eq!(guides(&deck, &t)[0].pos, 120.0);
        out.inverse.apply(&mut deck).unwrap();
        assert!(guides(&deck, &t).is_empty());
    }

    #[test]
    fn move_updates_pos_and_inverse_restores() {
        let (mut deck, t) = deck_with_slide();
        AddGuide { target: t.clone(), axis: GuideAxis::Horizontal, pos: 10.0, index: None }
            .apply(&mut deck)
            .unwrap();
        let out = MoveGuide { target: t.clone(), index: 0, new_pos: 88.0 }
            .apply(&mut deck)
            .unwrap();
        assert_eq!(guides(&deck, &t)[0].pos, 88.0);
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(guides(&deck, &t)[0].pos, 10.0);
    }

    #[test]
    fn remove_inverse_restores_at_same_index() {
        let (mut deck, t) = deck_with_slide();
        for p in [0.0, 50.0, 100.0] {
            AddGuide { target: t.clone(), axis: GuideAxis::Vertical, pos: p, index: None }
                .apply(&mut deck)
                .unwrap();
        }
        let out = RemoveGuide { target: t.clone(), index: 1 }.apply(&mut deck).unwrap();
        assert_eq!(guides(&deck, &t).iter().map(|g| g.pos).collect::<Vec<_>>(), vec![0.0, 100.0]);
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(guides(&deck, &t).iter().map(|g| g.pos).collect::<Vec<_>>(), vec![0.0, 50.0, 100.0]);
    }

    #[test]
    fn out_of_range_index_errors() {
        let (mut deck, t) = deck_with_slide();
        assert!(MoveGuide { target: t.clone(), index: 0, new_pos: 1.0 }.apply(&mut deck).is_err());
        assert!(RemoveGuide { target: t.clone(), index: 0 }.apply(&mut deck).is_err());
    }

    #[test]
    fn add_marks_canvas_dirty() {
        let (mut deck, t) = deck_with_slide();
        AddGuide { target: t.clone(), axis: GuideAxis::Vertical, pos: 5.0, index: None }
            .apply(&mut deck)
            .unwrap();
        match &t {
            CanvasTarget::Slide(id) => assert!(deck.slides[id].dirty),
            CanvasTarget::Layout(_) => unreachable!(),
        }
    }
}
