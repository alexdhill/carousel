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
use crate::deck::{CanvasTarget, SlideId};

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
        assert!(!self.slide_id.is_empty(), "SetSlideBackground: empty slide_id");
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
        assert!(!self.slide_id.is_empty(), "SetSlideBackgroundImage: empty slide_id");
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
}

impl Command for SetSlideLayout {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput; sets both the manifest entry and the SlideNode
    // layout_id (rendered as data-layout), requires a remount, returns the
    // inverse with the prior layout id.
    // Errors: SlideNotFound (missing slide).
    // Note: the layout id is NOT validated against the theme. It is an
    // associative tag until the deferred layout-binding feature, and slides may
    // legitimately reference ids absent from the current theme (so validating
    // would break undo, which restores the prior tag). The inspector's Layout
    // picker constrains forward choices to real layouts.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.slide_id.is_empty(), "SetSlideLayout: empty slide_id");
        let slide = deck
            .slides
            .get_mut(&self.slide_id)
            .ok_or_else(|| CommandError::SlideNotFound(self.slide_id.clone()))?;
        let prior: String = slide.layout_id.clone();
        slide.layout_id = self.new_layout_id.clone();
        if let Some(entry) = deck.manifest.slides.iter_mut().find(|e| e.id == self.slide_id) {
            entry.layout_id = self.new_layout_id.clone();
        }
        deck.manifest_dirty = true;
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(SetSlideLayout {
                slide_id: self.slide_id.clone(),
                new_layout_id: prior,
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
        let cmd = SetSlideBackground { slide_id: sid.clone(), background: Some("#222".into()) };
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].metadata.background, Some("#222".to_string()));
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
    fn notes_set_and_invert_on_manifest() {
        let (mut deck, sid) = sample();
        let out = SetSlideNotes { slide_id: sid.clone(), notes: Some("hi".into()) }
            .apply(&mut deck)
            .unwrap();
        let notes = |d: &Deck| d.manifest.slides.iter().find(|e| e.id == sid).unwrap().notes.clone();
        assert_eq!(notes(&deck), Some("hi".to_string()));
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(notes(&deck), None);
    }

    #[test]
    fn layout_sets_both_slide_and_manifest_and_inverts() {
        let (mut deck, sid) = sample();
        // The default theme seeds a "blank" layout; the sample slide uses "title".
        let out = SetSlideLayout { slide_id: sid.clone(), new_layout_id: "blank".into() }
            .apply(&mut deck)
            .unwrap();
        assert_eq!(deck.slides[&sid].layout_id, "blank");
        assert_eq!(
            deck.manifest.slides.iter().find(|e| e.id == sid).unwrap().layout_id,
            "blank"
        );
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].layout_id, "title");
    }

    #[test]
    fn missing_slide_errors() {
        let (mut deck, _sid) = sample();
        let err = SetSlideBackground { slide_id: "ghost".into(), background: None }
            .apply(&mut deck)
            .unwrap_err();
        assert!(matches!(err, CommandError::SlideNotFound(_)));
    }
}
