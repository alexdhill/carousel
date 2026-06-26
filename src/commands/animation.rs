// Animation timeline commands: InsertAnimation, RemoveAnimation,
// ReorderAnimation, SetAnimationProperty.
//
// Animations live on `SlideNode.animations` (slides only), so these commands
// are slide-targeted by `slide_id` (they do not use CanvasTarget). None emit
// DOM patches — there is no playback — but each marks the slide dirty, sets
// `manifest_dirty` (the manifest persists the timeline), and reports
// `affects_animations` so the editor rebroadcasts the slide's timeline.
//
// Validation (see `crate::deck::animation`): multiplicity is unrestricted —
// an element may own any number of entries of any category, in any order.
// Entries insert exactly where requested (clamped to length); no command
// rejects a combination. Only the structural invariants in `AnimationEntry`
// (non-empty ids, effect/category pairing) can abort.

use crate::commands::{Command, CommandError, CommandOutput};
use crate::deck::animation::{AnimationEntry, accommodating_index};
use crate::deck::{AnimationCategory, CanvasTarget, SlideId, SlideNode};

// slide_mut
// Resolve a mutable slide, or SlideNotFound.
fn slide_mut<'a>(
    deck: &'a mut crate::deck::Deck,
    slide_id: &SlideId,
) -> Result<&'a mut SlideNode, CommandError> {
    deck.slides
        .get_mut(slide_id)
        .ok_or_else(|| CommandError::SlideNotFound(slide_id.clone()))
}

// InsertAnimation
// Inserts `entry` into the slide's timeline at `position` (clamped to length).
// Multiplicity is unrestricted; never rejects.
#[derive(Debug, Clone)]
pub struct InsertAnimation {
    pub slide_id: SlideId,
    pub position: usize,
    pub entry: AnimationEntry,
}

impl Command for InsertAnimation {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.slide_id.is_empty(), "InsertAnimation: slide_id empty");
        let slide = slide_mut(deck, &self.slide_id)?;
        let (idx, warning) = accommodating_index(&slide.animations, self.position, &self.entry);
        slide.animations.insert(idx, self.entry.clone());
        slide.dirty = true;
        let inverse = RemoveAnimation {
            slide_id: self.slide_id.clone(),
            animation_id: self.entry.id.clone(),
        };
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(inverse),
            dirty_targets: vec![CanvasTarget::Slide(self.slide_id.clone())],
            manifest_dirty: true,
            warnings: warning.into_iter().collect(),
        })
    }

    fn label(&self) -> &'static str {
        "Add Animation"
    }

    fn affects_animations(&self) -> bool {
        true
    }
}

// RemoveAnimation
// Removes the entry with `animation_id`. Inverse re-inserts it at its prior
// index.
#[derive(Debug, Clone)]
pub struct RemoveAnimation {
    pub slide_id: SlideId,
    pub animation_id: String,
}

impl Command for RemoveAnimation {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.animation_id.is_empty(), "RemoveAnimation: id empty");
        let slide = slide_mut(deck, &self.slide_id)?;
        let pos = slide
            .animations
            .iter()
            .position(|e| e.id == self.animation_id)
            .ok_or_else(|| CommandError::AnimationNotFound(self.animation_id.clone()))?;
        let removed = slide.animations.remove(pos);
        slide.dirty = true;
        let inverse = InsertAnimation {
            slide_id: self.slide_id.clone(),
            position: pos,
            entry: removed,
        };
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(inverse),
            dirty_targets: vec![CanvasTarget::Slide(self.slide_id.clone())],
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Remove Animation"
    }

    fn affects_animations(&self) -> bool {
        true
    }
}

// ReorderAnimation
// Moves an entry to `new_position` (clamped to the last index). Timeline order
// alone defines playback now, so any reorder is accepted.
#[derive(Debug, Clone)]
pub struct ReorderAnimation {
    pub slide_id: SlideId,
    pub animation_id: String,
    pub new_position: usize,
}

impl Command for ReorderAnimation {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        let slide = slide_mut(deck, &self.slide_id)?;
        let from = slide
            .animations
            .iter()
            .position(|e| e.id == self.animation_id)
            .ok_or_else(|| CommandError::AnimationNotFound(self.animation_id.clone()))?;
        let to = self.new_position.min(slide.animations.len() - 1);
        let entry = slide.animations.remove(from);
        slide.animations.insert(to, entry);
        slide.dirty = true;
        let inverse = ReorderAnimation {
            slide_id: self.slide_id.clone(),
            animation_id: self.animation_id.clone(),
            new_position: from,
        };
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(inverse),
            dirty_targets: vec![CanvasTarget::Slide(self.slide_id.clone())],
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Reorder Animation"
    }

    fn affects_animations(&self) -> bool {
        true
    }
}

// SetAnimationProperty
// Replaces an entry's mutable fields (effect / category / trigger / timing)
// with `new_entry`. No multiplicity or ordering checks; always accepted.
#[derive(Debug, Clone)]
pub struct SetAnimationProperty {
    pub slide_id: SlideId,
    pub animation_id: String,
    pub new_entry: AnimationEntry,
}

impl Command for SetAnimationProperty {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        let slide = slide_mut(deck, &self.slide_id)?;
        let pos = slide
            .animations
            .iter()
            .position(|e| e.id == self.animation_id)
            .ok_or_else(|| CommandError::AnimationNotFound(self.animation_id.clone()))?;
        let prior = slide.animations[pos].clone();
        slide.animations[pos] = self.new_entry.clone();
        slide.dirty = true;
        let inverse = SetAnimationProperty {
            slide_id: self.slide_id.clone(),
            animation_id: self.animation_id.clone(),
            new_entry: prior,
        };
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(inverse),
            dirty_targets: vec![CanvasTarget::Slide(self.slide_id.clone())],
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Edit Animation"
    }

    fn affects_animations(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;
    use crate::deck::animation::{AnimationEffect, AnimationTiming, AnimationTrigger};

    fn entry(id: &str, el: &str, cat: AnimationCategory, kf: &str) -> AnimationEntry {
        AnimationEntry::new(
            id.into(),
            el.into(),
            AnimationEffect::Named(kf.into()),
            cat,
            AnimationTrigger::OnClick,
            AnimationTiming::default(),
        )
    }

    fn deck_and_el() -> (Deck, SlideId, String) {
        let deck = Deck::sample();
        let sid = deck.slide_order[0].clone();
        let el = deck.slides[&sid].root.children[0].id.clone();
        (deck, sid, el)
    }

    #[test]
    fn insert_then_inverse_removes() {
        let (mut deck, sid, el) = deck_and_el();
        let cmd = InsertAnimation {
            slide_id: sid.clone(),
            position: 0,
            entry: entry("e", &el, AnimationCategory::Entrance, "appear"),
        };
        assert!(cmd.affects_animations());
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].animations.len(), 1);
        out.inverse.apply(&mut deck).unwrap();
        assert!(deck.slides[&sid].animations.is_empty());
    }

    #[test]
    fn remove_then_inverse_reinserts_at_index() {
        let (mut deck, sid, el) = deck_and_el();
        InsertAnimation {
            slide_id: sid.clone(),
            position: 0,
            entry: entry("a", &el, AnimationCategory::Emphasis, "pulse"),
        }
        .apply(&mut deck)
        .unwrap();
        InsertAnimation {
            slide_id: sid.clone(),
            position: 1,
            entry: entry("b", &el, AnimationCategory::Emphasis, "pulse"),
        }
        .apply(&mut deck)
        .unwrap();
        let out = RemoveAnimation {
            slide_id: sid.clone(),
            animation_id: "a".into(),
        }
        .apply(&mut deck)
        .unwrap();
        assert_eq!(deck.slides[&sid].animations[0].id, "b");
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].animations[0].id, "a");
        assert_eq!(deck.slides[&sid].animations[1].id, "b");
    }

    #[test]
    fn two_entrances_on_one_element_are_allowed() {
        let (mut deck, sid, el) = deck_and_el();
        InsertAnimation {
            slide_id: sid.clone(),
            position: 0,
            entry: entry("e1", &el, AnimationCategory::Entrance, "appear"),
        }
        .apply(&mut deck)
        .unwrap();
        InsertAnimation {
            slide_id: sid.clone(),
            position: 1,
            entry: entry("e2", &el, AnimationCategory::Entrance, "fade-in"),
        }
        .apply(&mut deck)
        .unwrap();
        assert_eq!(deck.slides[&sid].animations.len(), 2);
    }
}
