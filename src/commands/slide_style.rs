// Slide-level metadata commands for the inspector's Slide box.
//
// When nothing is selected, the inspector targets the active slide. These three
// commands back its controls:
//   - SetSlideBackground — per-slide background (renders; lives on the SlideNode
//     metadata, synced to the manifest like the animation timeline).
//   - SetSlideNotes      — inline speaker notes (manifest chrome; does not render).
//   - SetSlideLayout      — which saved layout the slide references (tag only until
//     the deferred layout-binding feature re-flows content).
// All are slide-targeted, self-inverse, set manifest_dirty, and report
// affects_slide_meta so the editor rebroadcasts SlideInspectorUpdate.

use crate::commands::{Command, CommandError, CommandOutput};
use crate::deck::element::regenerate_ids;
use crate::deck::{CanvasTarget, ElementNode, SlideId};

// SetSlideBackground
#[derive(Debug, Clone)]
pub struct SetSlideBackground {
    pub slide_id: SlideId,
    pub background: Option<String>,
}

impl Command for SetSlideBackground {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput; sets the SlideNode metadata background, marks the
    // slide dirty, requires a remount (the section style changes), and returns
    // the inverse carrying the prior value.
    // Errors: SlideNotFound when no slide matches slide_id.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.slide_id.is_empty(),
            "SetSlideBackground: empty slide_id"
        );
        let slide = deck
            .slides
            .get_mut(&self.slide_id)
            .ok_or_else(|| CommandError::SlideNotFound(self.slide_id.clone()))?;
        let prior: Option<String> = slide.metadata.background.clone();
        slide.metadata.background = self.background.clone();
        deck.manifest_dirty = true;
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(SetSlideBackground {
                slide_id: self.slide_id.clone(),
                background: prior,
            }),
            dirty_targets: vec![CanvasTarget::Slide(self.slide_id.clone())],
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Set Slide Background"
    }

    fn requires_remount(&self) -> bool {
        true
    }

    fn affects_slide_meta(&self) -> bool {
        true
    }
}

// SetSlideBackgroundImage — per-slide background image (drawn over the fill).
// Same shape as SetSlideBackground: SlideNode-metadata authoritative, renders,
// self-inverse, remounts, rebroadcasts the Slide box.
#[derive(Debug, Clone)]
pub struct SetSlideBackgroundImage {
    pub slide_id: SlideId,
    pub background_image: Option<String>,
}

impl Command for SetSlideBackgroundImage {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.slide_id.is_empty(),
            "SetSlideBackgroundImage: empty slide_id"
        );
        let slide = deck
            .slides
            .get_mut(&self.slide_id)
            .ok_or_else(|| CommandError::SlideNotFound(self.slide_id.clone()))?;
        let prior: Option<String> = slide.metadata.background_image.clone();
        slide.metadata.background_image = self.background_image.clone();
        deck.manifest_dirty = true;
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(SetSlideBackgroundImage {
                slide_id: self.slide_id.clone(),
                background_image: prior,
            }),
            dirty_targets: vec![CanvasTarget::Slide(self.slide_id.clone())],
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Set Slide Background Image"
    }

    fn requires_remount(&self) -> bool {
        true
    }

    fn affects_slide_meta(&self) -> bool {
        true
    }
}

// SetSlideTransition — per-slide outgoing presentation transition.
// Presentation-only: it never renders, so NO remount. Slide-meta authoritative,
// self-inverse, manifest-dirty, rebroadcasts the Slide box.
#[derive(Debug, Clone)]
pub struct SetSlideTransition {
    pub slide_id: SlideId,
    pub transition: Option<crate::deck::SlideTransition>,
}

impl Command for SetSlideTransition {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput; sets the SlideNode metadata transition, marks the
    // slide dirty + manifest dirty, returns the inverse carrying the prior value.
    // No patches/remount — transitions affect presentation playback only.
    // Errors: SlideNotFound when no slide matches slide_id.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.slide_id.is_empty(),
            "SetSlideTransition: empty slide_id"
        );
        let slide = deck
            .slides
            .get_mut(&self.slide_id)
            .ok_or_else(|| CommandError::SlideNotFound(self.slide_id.clone()))?;
        let prior: Option<crate::deck::SlideTransition> = slide.metadata.transition.clone();
        slide.metadata.transition = self.transition.clone();
        deck.manifest_dirty = true;
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(SetSlideTransition {
                slide_id: self.slide_id.clone(),
                transition: prior,
            }),
            dirty_targets: vec![CanvasTarget::Slide(self.slide_id.clone())],
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Set Slide Transition"
    }

    fn affects_slide_meta(&self) -> bool {
        true
    }
}

// SetSlideNotes
#[derive(Debug, Clone)]
pub struct SetSlideNotes {
    pub slide_id: SlideId,
    pub notes: Option<String>,
}

impl Command for SetSlideNotes {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput; sets the manifest entry's notes (no remount — notes
    // do not render), returns the inverse with the prior value.
    // Errors: SlideNotFound when no manifest entry matches slide_id.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.slide_id.is_empty(), "SetSlideNotes: empty slide_id");
        let entry = deck
            .manifest
            .slides
            .iter_mut()
            .find(|e| e.id == self.slide_id)
            .ok_or_else(|| CommandError::SlideNotFound(self.slide_id.clone()))?;
        let prior: Option<String> = entry.notes.clone();
        entry.notes = self.notes.clone();
        deck.manifest_dirty = true;
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(SetSlideNotes {
                slide_id: self.slide_id.clone(),
                notes: prior,
            }),
            dirty_targets: Vec::new(),
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Set Slide Notes"
    }

    fn affects_slide_meta(&self) -> bool {
        true
    }
}

// SetSlideLayout
#[derive(Debug, Clone)]
pub struct SetSlideLayout {
    pub slide_id: SlideId,
    pub new_layout_id: String,
    // Undo payload: the exact slide root to restore. None on a forward
    // (user-driven) apply — the slide is re-stamped from the chosen layout's
    // template elements. Some(_) only when this command is an inverse.
    pub restore_root: Option<ElementNode>,
}

impl Command for SetSlideLayout {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput; retags the slide + manifest entry with the new
    // layout id and stamps that layout's template elements on top of the slide's
    // existing content (the layout's elements are appended, not replaced, so the
    // user's prior edits survive). Each stamped subtree gets fresh element ids so
    // re-applying the same layout never collides. The layout's text styles ride
    // along baked inline on those elements; its background inherits via
    // Deck::effective_slide_bg. Requires a remount. The returned inverse carries
    // the prior layout id and the prior root so undo restores both.
    // Errors: SlideNotFound (missing slide).
    // Note: the layout id is NOT validated against the theme — slides may
    // legitimately reference ids absent from the current theme (validating would
    // break undo). A forward apply whose layout id is unknown to the theme
    // retags only and leaves the root untouched. The inspector's Layout picker
    // constrains forward choices to real layouts.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.slide_id.is_empty(), "SetSlideLayout: empty slide_id");
        // Forward apply: clone the chosen layout's template children (with fresh
        // ids so re-stamping the same layout can't collide) before the mutable
        // slide borrow. Inverse apply (restore_root set) skips this.
        let mut template: Vec<ElementNode> = if self.restore_root.is_none() {
            deck.theme
                .layouts
                .get(&self.new_layout_id)
                .map(|l| l.root.children.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        for child in template.iter_mut() {
            regenerate_ids(child);
        }
        let slide = deck
            .slides
            .get_mut(&self.slide_id)
            .ok_or_else(|| CommandError::SlideNotFound(self.slide_id.clone()))?;
        let prior_layout: String = slide.layout_id.clone();
        let prior_root: ElementNode = slide.root.clone();
        slide.layout_id = self.new_layout_id.clone();
        if let Some(root) = &self.restore_root {
            slide.root = root.clone();
        } else {
            slide.root.children.extend(template);
        }
        if let Some(entry) = deck
            .manifest
            .slides
            .iter_mut()
            .find(|e| e.id == self.slide_id)
        {
            entry.layout_id = self.new_layout_id.clone();
        }
        deck.manifest_dirty = true;
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(SetSlideLayout {
                slide_id: self.slide_id.clone(),
                new_layout_id: prior_layout,
                restore_root: Some(prior_root),
            }),
            dirty_targets: vec![CanvasTarget::Slide(self.slide_id.clone())],
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Set Slide Layout"
    }

    fn requires_remount(&self) -> bool {
        true
    }

    fn affects_slide_meta(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;

    fn sample() -> (Deck, SlideId) {
        let deck = Deck::sample();
        let sid = deck.slide_order[0].clone();
        (deck, sid)
    }

    #[test]
    fn background_sets_and_inverts() {
        let (mut deck, sid) = sample();
        let cmd = SetSlideBackground {
            slide_id: sid.clone(),
            background: Some("#222".into()),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(
            deck.slides[&sid].metadata.background,
            Some("#222".to_string())
        );
        assert!(cmd.requires_remount());
        assert!(cmd.affects_slide_meta());
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].metadata.background, None);
    }

    #[test]
    fn background_image_sets_and_inverts() {
        let (mut deck, sid) = sample();
        let cmd = SetSlideBackgroundImage {
            slide_id: sid.clone(),
            background_image: Some("var(--asset-x)".into()),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(
            deck.slides[&sid].metadata.background_image,
            Some("var(--asset-x)".to_string())
        );
        assert!(cmd.requires_remount());
        assert!(cmd.affects_slide_meta());
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].metadata.background_image, None);
    }

    #[test]
    fn transition_sets_and_inverts() {
        let (mut deck, sid) = sample();
        let t = crate::deck::SlideTransition {
            kind: crate::deck::TransitionKind::Fade,
            duration_ms: 600,
            easing: "ease-in-out".into(),
        };
        let cmd = SetSlideTransition {
            slide_id: sid.clone(),
            transition: Some(t.clone()),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].metadata.transition, Some(t));
        assert!(deck.manifest_dirty);
        assert!(cmd.affects_slide_meta());
        assert!(!cmd.requires_remount()); // presentation-only, never re-renders
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].metadata.transition, None);
    }

    #[test]
    fn notes_set_and_invert_on_manifest() {
        let (mut deck, sid) = sample();
        let out = SetSlideNotes {
            slide_id: sid.clone(),
            notes: Some("hi".into()),
        }
        .apply(&mut deck)
        .unwrap();
        let notes = |d: &Deck| {
            d.manifest
                .slides
                .iter()
                .find(|e| e.id == sid)
                .unwrap()
                .notes
                .clone()
        };
        assert_eq!(notes(&deck), Some("hi".to_string()));
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(notes(&deck), None);
    }

    #[test]
    fn layout_sets_both_slide_and_manifest_and_inverts() {
        let (mut deck, sid) = sample();
        // The default theme seeds a "blank" layout; the sample slide uses "title".
        let out = SetSlideLayout {
            slide_id: sid.clone(),
            new_layout_id: "blank".into(),
            restore_root: None,
        }
        .apply(&mut deck)
        .unwrap();
        assert_eq!(deck.slides[&sid].layout_id, "blank");
        assert_eq!(
            deck.manifest
                .slides
                .iter()
                .find(|e| e.id == sid)
                .unwrap()
                .layout_id,
            "blank"
        );
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].layout_id, "title");
    }

    #[test]
    fn layout_appends_template_elements_keeping_edits_and_undo_restores() {
        use crate::deck::templates::{light_theme, new_deck};
        // A blank one-slide light deck; its slide starts seeded with "title".
        let mut deck = new_deck(light_theme(), "title");
        let sid = deck.slide_order[0].clone();
        let before = deck.slides[&sid].root.children.len();
        // Switch to "hero" (title + copy + accent block = 3): appended on top,
        // not replacing the existing content.
        let out = SetSlideLayout {
            slide_id: sid.clone(),
            new_layout_id: "hero".into(),
            restore_root: None,
        }
        .apply(&mut deck)
        .unwrap();
        assert_eq!(deck.slides[&sid].layout_id, "hero");
        assert_eq!(deck.slides[&sid].root.children.len(), before + 3);
        // Fresh ids: no duplicates after stamping on top.
        let ids: std::collections::HashSet<&String> = deck.slides[&sid]
            .root
            .children
            .iter()
            .map(|c| &c.id)
            .collect();
        assert_eq!(ids.len(), before + 3);
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].layout_id, "title");
        assert_eq!(deck.slides[&sid].root.children.len(), before);
    }

    #[test]
    fn missing_slide_errors() {
        let (mut deck, _sid) = sample();
        let err = SetSlideBackground {
            slide_id: "ghost".into(),
            background: None,
        }
        .apply(&mut deck)
        .unwrap_err();
        assert!(matches!(err, CommandError::SlideNotFound(_)));
    }
}
