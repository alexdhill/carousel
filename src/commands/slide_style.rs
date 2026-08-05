use crate::commands::{Command, CommandError, CommandOutput};
use crate::deck::element::ElementContent;
use crate::deck::{CanvasTarget, ElementNode, SlideId};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct SetSlideBackground {
    pub slide_id: SlideId,
    pub background: Option<String>,
}

impl Command for SetSlideBackground {

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

#[derive(Debug, Clone)]
pub struct SetSlideTransition {
    pub slide_id: SlideId,
    pub transition: Option<crate::deck::SlideTransition>,
}

impl Command for SetSlideTransition {

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

#[derive(Debug, Clone)]
pub struct SetSlideNotes {
    pub slide_id: SlideId,
    pub notes: Option<String>,
}

impl Command for SetSlideNotes {

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

#[derive(Debug, Clone)]
pub struct SetSlideLayout {
    pub slide_id: SlideId,
    pub new_layout_id: String,

    pub restore_root: Option<ElementNode>,
}

impl Command for SetSlideLayout {

    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.slide_id.is_empty(), "SetSlideLayout: empty slide_id");

        let new_slots: Vec<ElementNode> = if self.restore_root.is_none() {
            deck.theme
                .layouts
                .get(&self.new_layout_id)
                .map(|l| l.seeded_children())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
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
            let old_children: Vec<ElementNode> = std::mem::take(&mut slide.root.children);
            slide.root.children = remap_layout_children(old_children, new_slots);
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

fn remap_layout_children(old: Vec<ElementNode>, new_slots: Vec<ElementNode>) -> Vec<ElementNode> {
    let touched: HashMap<String, ElementContent> = old
        .iter()
        .filter(|e| e.is_layout_element() && !e.placeholder)
        .map(|e| (e.id.clone(), e.content.clone()))
        .collect();
    let new_ids: HashSet<String> = new_slots.iter().map(|s| s.id.clone()).collect();
    let mut result: Vec<ElementNode> = Vec::with_capacity(new_slots.len() + old.len());
    for mut slot in new_slots {
        if let Some(content) = touched.get(&slot.id) {
            slot.content = content.clone();
            slot.placeholder = false;
        }
        result.push(slot);
    }
    for el in old {

        let overflow: bool = el.is_layout_element() && !el.placeholder && !new_ids.contains(&el.id);
        if !el.is_layout_element() || overflow {
            result.push(el);
        }
    }
    result
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
        assert!(!cmd.requires_remount());
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
    fn layout_change_remaps_edited_title_and_undo_restores() {
        use crate::commands::SetTextContent;
        use crate::deck::ElementContent;
        use crate::deck::templates::{light_theme, new_deck};

        let mut deck = new_deck(light_theme(), "title");
        let sid = deck.slide_order[0].clone();

        SetTextContent {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: "layout_text_title".into(),
            new_content: crate::deck::RichText::new("My Talk"),
        }
        .apply(&mut deck)
        .unwrap();
        let out = SetSlideLayout {
            slide_id: sid.clone(),
            new_layout_id: "hero".into(),
            restore_root: None,
        }
        .apply(&mut deck)
        .unwrap();
        assert_eq!(deck.slides[&sid].layout_id, "hero");
        let kids = &deck.slides[&sid].root.children;

        let title = kids.iter().find(|c| c.id == "layout_text_title").unwrap();
        assert!(!title.placeholder, "edited title is not a placeholder");
        match &title.content {
            ElementContent::Text(rt) => assert_eq!(rt.plain, "My Talk"),
            _ => panic!("title should be text"),
        }
        assert!(
            kids.iter()
                .any(|c| c.id == "layout_text_body" && c.placeholder)
        );
        assert!(kids.iter().any(|c| c.id == "layout_shape_hero"));
        assert!(
            !kids.iter().any(|c| c.id == "layout_text_subtitle"),
            "untouched subtitle dropped"
        );

        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].layout_id, "title");
        assert!(
            deck.slides[&sid]
                .root
                .children
                .iter()
                .any(|c| c.id == "layout_text_subtitle")
        );
    }

    #[test]
    fn layout_change_keeps_added_and_overflow_elements() {
        use crate::commands::SetTextContent;
        use crate::deck::templates::{light_theme, new_deck};

        let mut deck = new_deck(light_theme(), "title");
        let sid = deck.slide_order[0].clone();
        SetTextContent {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: "layout_text_subtitle".into(),
            new_content: crate::deck::RichText::new("kept"),
        }
        .apply(&mut deck)
        .unwrap();

        deck.slides
            .get_mut(&sid)
            .unwrap()
            .root
            .children
            .push(crate::deck::builders::text_element("usr_note", "note"));
        SetSlideLayout {
            slide_id: sid.clone(),
            new_layout_id: "hero".into(),
            restore_root: None,
        }
        .apply(&mut deck)
        .unwrap();
        let kids = &deck.slides[&sid].root.children;
        assert!(
            kids.iter().any(|c| c.id == "layout_text_subtitle"),
            "touched overflow kept"
        );
        assert!(
            kids.iter().any(|c| c.id == "usr_note"),
            "added element kept"
        );
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
