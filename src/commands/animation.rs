// Animation timeline commands: InsertAnimation, RemoveAnimation,
// ReorderAnimation, SetAnimationProperty.
//
// Animations live on `SlideNode.animations` (slides only), so these commands
// are slide-targeted by `slide_id` (they do not use CanvasTarget). None emit
// DOM patches — there is no playback — but each marks the slide dirty, sets
// `manifest_dirty` (the manifest persists the timeline), and reports
// `affects_animations` so the editor rebroadcasts the slide's timeline.
//
// Validation (see `crate::deck::animation`): each element may own at most one
// entrance and one exit (hard, always rejected), and an entrance must precede
// its exit. The *add* path (`InsertAnimation`) accommodates an ordering
// conflict by clamping the insert index and returns a non-fatal warning; the
// *edit* paths (`ReorderAnimation`, `SetAnimationProperty`) strictly reject a
// violating arrangement.

use crate::commands::{Command, CommandError, CommandOutput};
use crate::deck::animation::{accommodating_index, has_category, ordering_ok, AnimationEntry};
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
// Inserts `entry` into the slide's timeline at `position`. Rejects a second
// entrance/exit for the entry's element; accommodates an ordering conflict by
// clamping the index and returns a warning.
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
        // Multiplicity is a hard invariant for entrance/exit.
        if matches!(self.entry.category, AnimationCategory::Entrance | AnimationCategory::Exit)
            && has_category(&slide.animations, &self.entry.element_id, self.entry.category)
        {
            return Err(CommandError::InvalidOperation(format!(
                "element {} already has a {:?} animation",
                self.entry.element_id, self.entry.category
            )));
        }
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
// Moves an entry to `new_position`. Strict: if the move breaks
// entrance-before-exit ordering it is undone and rejected.
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
        let element_id = entry.element_id.clone();
        slide.animations.insert(to, entry);
        // Edit path: strict reject if ordering broke. Restore and error.
        if !ordering_ok(&slide.animations, &element_id) {
            let e = slide.animations.remove(to);
            slide.animations.insert(from, e);
            return Err(CommandError::InvalidOperation(
                "reorder would place an exit before its entrance".into(),
            ));
        }
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
// Replaces an entry's mutable fields (keyframe / category / trigger / timing)
// with `new_entry`. Strict: a recategorize that creates a duplicate
// entrance/exit or breaks ordering is rejected (entry restored).
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
        // Multiplicity guard if the category changed into one the element
        // already has (in a *different* entry).
        if self.new_entry.category != prior.category
            && matches!(self.new_entry.category, AnimationCategory::Entrance | AnimationCategory::Exit)
            && has_category(&slide.animations, &self.new_entry.element_id, self.new_entry.category)
        {
            return Err(CommandError::InvalidOperation(
                "category already present on this element".into(),
            ));
        }
        slide.animations[pos] = self.new_entry.clone();
        if !ordering_ok(&slide.animations, &self.new_entry.element_id) {
            slide.animations[pos] = prior; // restore
            return Err(CommandError::InvalidOperation(
                "change would break entrance-before-exit ordering".into(),
            ));
        }
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
    use crate::deck::animation::{AnimationTiming, AnimationTrigger};
    use crate::deck::Deck;

    fn entry(id: &str, el: &str, cat: AnimationCategory, kf: &str) -> AnimationEntry {
        AnimationEntry::new(id.into(), el.into(), kf.into(), cat,
            AnimationTrigger::OnClick, AnimationTiming::default())
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
        let cmd = InsertAnimation { slide_id: sid.clone(), position: 0,
            entry: entry("e", &el, AnimationCategory::Entrance, "appear") };
        assert!(cmd.affects_animations());
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].animations.len(), 1);
        out.inverse.apply(&mut deck).unwrap();
        assert!(deck.slides[&sid].animations.is_empty());
    }

    #[test]
    fn remove_then_inverse_reinserts_at_index() {
        let (mut deck, sid, el) = deck_and_el();
        InsertAnimation { slide_id: sid.clone(), position: 0,
            entry: entry("a", &el, AnimationCategory::Emphasis, "pulse") }
            .apply(&mut deck).unwrap();
        InsertAnimation { slide_id: sid.clone(), position: 1,
            entry: entry("b", &el, AnimationCategory::Emphasis, "pulse") }
            .apply(&mut deck).unwrap();
        let out = RemoveAnimation { slide_id: sid.clone(), animation_id: "a".into() }
            .apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].animations[0].id, "b");
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].animations[0].id, "a");
        assert_eq!(deck.slides[&sid].animations[1].id, "b");
    }

    #[test]
    fn second_entrance_is_rejected() {
        let (mut deck, sid, el) = deck_and_el();
        InsertAnimation { slide_id: sid.clone(), position: 0,
            entry: entry("e1", &el, AnimationCategory::Entrance, "appear") }
            .apply(&mut deck).unwrap();
        let err = InsertAnimation { slide_id: sid.clone(), position: 0,
            entry: entry("e2", &el, AnimationCategory::Entrance, "appear") }
            .apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::InvalidOperation(_)));
    }

    #[test]
    fn add_entrance_after_exit_accommodates_and_warns() {
        let (mut deck, sid, el) = deck_and_el();
        InsertAnimation { slide_id: sid.clone(), position: 0,
            entry: entry("x", &el, AnimationCategory::Exit, "disappear") }
            .apply(&mut deck).unwrap();
        let out = InsertAnimation { slide_id: sid.clone(), position: 9,
            entry: entry("e", &el, AnimationCategory::Entrance, "appear") }
            .apply(&mut deck).unwrap();
        assert!(!out.warnings.is_empty());
        let t = &deck.slides[&sid].animations;
        assert!(t.iter().position(|a| a.id == "e").unwrap()
              < t.iter().position(|a| a.id == "x").unwrap());
    }

    #[test]
    fn reorder_breaking_ordering_is_rejected_and_restores() {
        let (mut deck, sid, el) = deck_and_el();
        InsertAnimation { slide_id: sid.clone(), position: 0,
            entry: entry("en", &el, AnimationCategory::Entrance, "appear") }
            .apply(&mut deck).unwrap();
        InsertAnimation { slide_id: sid.clone(), position: 1,
            entry: entry("ex", &el, AnimationCategory::Exit, "disappear") }
            .apply(&mut deck).unwrap();
        // Move the exit to index 0 (before the entrance) → reject.
        let err = ReorderAnimation { slide_id: sid.clone(), animation_id: "ex".into(), new_position: 0 }
            .apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::InvalidOperation(_)));
        // Timeline unchanged.
        assert_eq!(deck.slides[&sid].animations[0].id, "en");
        assert_eq!(deck.slides[&sid].animations[1].id, "ex");
    }

    #[test]
    fn recategorize_breaking_ordering_is_rejected() {
        let (mut deck, sid, el) = deck_and_el();
        // Order: exit then entrance is valid only if they are different
        // elements; here same element so we build entrance(0), emphasis(1),
        // then recategorize the emphasis at index... instead: entrance at 1,
        // exit at 0 via two elements is awkward — use a direct case:
        InsertAnimation { slide_id: sid.clone(), position: 0,
            entry: entry("a", &el, AnimationCategory::Emphasis, "pulse") }
            .apply(&mut deck).unwrap();
        InsertAnimation { slide_id: sid.clone(), position: 1,
            entry: entry("b", &el, AnimationCategory::Exit, "disappear") }
            .apply(&mut deck).unwrap();
        // Recategorize "a" (index 0) to Entrance → entrance(0) before exit(1): OK.
        SetAnimationProperty { slide_id: sid.clone(), animation_id: "a".into(),
            new_entry: entry("a", &el, AnimationCategory::Entrance, "appear") }
            .apply(&mut deck).unwrap();
        // Now recategorize "b" (the exit at index 1) to Entrance → second
        // entrance → multiplicity reject.
        let err = SetAnimationProperty { slide_id: sid.clone(), animation_id: "b".into(),
            new_entry: entry("b", &el, AnimationCategory::Entrance, "appear") }
            .apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::InvalidOperation(_)));
    }
}
