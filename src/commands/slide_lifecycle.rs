use crate::bundle::SlideEntry;
use crate::commands::{Command, CommandError, CommandOutput};
use crate::deck::slide::SlideNode;
use crate::deck::{CanvasTarget, SlideId};

#[derive(Debug, Clone)]
pub struct InsertSlide {
    pub position: usize,
    pub slide: SlideNode,
    pub manifest_entry: SlideEntry,
}

impl Command for InsertSlide {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        let slide_id: SlideId = self.slide.id.clone();
        assert!(!slide_id.is_empty(), "InsertSlide: slide id is empty");
        assert_eq!(
            self.manifest_entry.id, slide_id,
            "InsertSlide: manifest entry id must match slide id"
        );
        if deck.slides.contains_key(&slide_id) {
            return Err(CommandError::Conflict(format!(
                "InsertSlide: slide {slide_id} already exists"
            )));
        }
        let order_pos: usize = self.position.min(deck.slide_order.len());
        let manifest_pos: usize = self.position.min(deck.manifest.slides.len());

        deck.slides.insert(slide_id.clone(), self.slide.clone());
        deck.slide_order.insert(order_pos, slide_id.clone());
        deck.manifest
            .slides
            .insert(manifest_pos, self.manifest_entry.clone());
        deck.dirty_slides.insert(slide_id.clone());
        deck.manifest_dirty = true;

        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(RemoveSlide { slide_id }),
            dirty_targets: vec![CanvasTarget::Slide(self.slide.id.clone())],
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Add Slide"
    }

    fn affects_slide_list(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone)]
pub struct RemoveSlide {
    pub slide_id: SlideId,
}

impl Command for RemoveSlide {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.slide_id.is_empty(), "RemoveSlide: slide id is empty");
        if !deck.slides.contains_key(&self.slide_id) {
            return Err(CommandError::SlideNotFound(self.slide_id.clone()));
        }
        if deck.slide_order.len() <= 1 {
            return Err(CommandError::InvalidOperation(
                "RemoveSlide: cannot remove the deck's only slide".into(),
            ));
        }
        let order_pos: usize = deck
            .slide_order
            .iter()
            .position(|id| id == &self.slide_id)
            .ok_or_else(|| CommandError::SlideNotFound(self.slide_id.clone()))?;

        let removed_slide: SlideNode = deck
            .slides
            .remove(&self.slide_id)
            .ok_or_else(|| CommandError::SlideNotFound(self.slide_id.clone()))?;
        deck.slide_order.remove(order_pos);

        let manifest_pos: usize = deck
            .manifest
            .slides
            .iter()
            .position(|e| e.id == self.slide_id)
            .unwrap_or(order_pos.min(deck.manifest.slides.len()));
        let removed_entry: SlideEntry = if manifest_pos < deck.manifest.slides.len() {
            deck.manifest.slides.remove(manifest_pos)
        } else {
            SlideEntry {
                id: self.slide_id.clone(),
                path: crate::bundle::manifest::slide_path_for(&self.slide_id),
                layout_id: removed_slide.layout_id.clone(),
                title: String::new(),
                thumbnail: None,
                transition: None,
                duration_hint: None,
                notes_ref: None,
                animations: Vec::new(),
                guides: Vec::new(),
                background: None,
                background_image: None,
                notes: None,
            }
        };
        deck.dirty_slides.remove(&self.slide_id);
        deck.manifest_dirty = true;

        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(InsertSlide {
                position: order_pos,
                slide: removed_slide,
                manifest_entry: removed_entry,
            }),
            dirty_targets: Vec::new(),
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Delete Slide"
    }

    fn affects_slide_list(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone)]
pub struct ReorderSlide {
    pub slide_id: SlideId,
    pub new_index: usize,
}

impl Command for ReorderSlide {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.slide_id.is_empty(), "ReorderSlide: slide id is empty");
        let from: usize = deck
            .slide_order
            .iter()
            .position(|id| id == &self.slide_id)
            .ok_or_else(|| CommandError::SlideNotFound(self.slide_id.clone()))?;
        let last: usize = deck.slide_order.len().saturating_sub(1);
        let to: usize = self.new_index.min(last);
        assert_ne!(from, to, "ReorderSlide: source and target index are equal");

        let moved_id: SlideId = deck.slide_order.remove(from);
        deck.slide_order.insert(to, moved_id);

        let manifest_last: usize = deck.manifest.slides.len().saturating_sub(1);
        let manifest_from: usize = deck
            .manifest
            .slides
            .iter()
            .position(|e| e.id == self.slide_id)
            .unwrap_or(from.min(manifest_last));
        let manifest_to: usize = to.min(manifest_last);
        let moved_entry: SlideEntry = deck.manifest.slides.remove(manifest_from);
        deck.manifest.slides.insert(manifest_to, moved_entry);
        deck.manifest_dirty = true;

        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(ReorderSlide {
                slide_id: self.slide_id.clone(),
                new_index: from,
            }),
            dirty_targets: Vec::new(),
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Reorder Slide"
    }

    fn affects_slide_list(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::bundle::manifest::slide_path_for;
    use crate::deck::Deck;
    use crate::deck::builders::{group_element, text_element};

    fn blank_slide(id: &str) -> SlideNode {
        SlideNode::new(
            id.into(),
            "blank".into(),
            group_element("rt_new", vec![text_element("el_new", "hi")]),
        )
    }

    fn entry_for(id: &str) -> SlideEntry {
        SlideEntry {
            id: id.into(),
            path: slide_path_for(id),
            layout_id: "blank".into(),
            title: String::new(),
            thumbnail: None,
            transition: None,
            duration_hint: None,
            notes_ref: None,
            animations: Vec::new(),
            guides: Vec::new(),
            background: None,
            background_image: None,
            notes: None,
        }
    }

    #[test]
    fn insert_slide_adds_to_order_map_and_manifest() {
        let mut deck = Deck::sample();
        let before = deck.slide_order.len();
        let cmd = InsertSlide {
            position: before,
            slide: blank_slide("s_new"),
            manifest_entry: entry_for("s_new"),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(deck.slide_order.len(), before + 1);
        assert!(deck.slides.contains_key("s_new"));
        assert!(deck.manifest.slides.iter().any(|e| e.id == "s_new"));
        assert_eq!(deck.slide_order.last().map(String::as_str), Some("s_new"));
        assert!(out.patches.is_empty());
        assert!(out.manifest_dirty);
        assert!(cmd.affects_slide_list());
    }

    #[test]
    fn insert_slide_at_position_inserts_in_order() {
        let mut deck = Deck::sample();
        let cmd = InsertSlide {
            position: 0,
            slide: blank_slide("s_first"),
            manifest_entry: entry_for("s_first"),
        };
        cmd.apply(&mut deck).unwrap();
        assert_eq!(deck.slide_order[0], "s_first");
        assert_eq!(deck.manifest.slides[0].id, "s_first");
    }

    #[test]
    fn insert_slide_clamps_out_of_range_position() {
        let mut deck = Deck::sample();
        let cmd = InsertSlide {
            position: 999,
            slide: blank_slide("s_app"),
            manifest_entry: entry_for("s_app"),
        };
        cmd.apply(&mut deck).unwrap();
        assert_eq!(deck.slide_order.last().map(String::as_str), Some("s_app"));
    }

    #[test]
    fn insert_slide_rejects_duplicate_id() {
        let mut deck = Deck::sample();
        let existing = deck.slide_order[0].clone();
        let cmd = InsertSlide {
            position: 0,
            slide: blank_slide(&existing),
            manifest_entry: entry_for(&existing),
        };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::Conflict(_)));
    }

    #[test]
    fn insert_then_inverse_removes_the_slide() {
        let mut deck = Deck::sample();
        let before = deck.slide_order.clone();
        let cmd = InsertSlide {
            position: 1,
            slide: blank_slide("s_tmp"),
            manifest_entry: entry_for("s_tmp"),
        };
        let out = cmd.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.slide_order, before);
        assert!(!deck.slides.contains_key("s_tmp"));
        assert!(!deck.manifest.slides.iter().any(|e| e.id == "s_tmp"));
    }

    #[test]
    fn remove_slide_drops_from_all_three_structures() {
        let mut deck = Deck::sample();

        InsertSlide {
            position: 1,
            slide: blank_slide("s_b"),
            manifest_entry: entry_for("s_b"),
        }
        .apply(&mut deck)
        .unwrap();
        let cmd = RemoveSlide {
            slide_id: "s_b".into(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert!(!deck.slides.contains_key("s_b"));
        assert!(!deck.slide_order.iter().any(|id| id == "s_b"));
        assert!(!deck.manifest.slides.iter().any(|e| e.id == "s_b"));
        assert!(out.manifest_dirty);
        assert!(cmd.affects_slide_list());
    }

    #[test]
    fn remove_last_slide_is_rejected() {
        let mut deck = Deck::sample();
        assert_eq!(deck.slide_order.len(), 1);
        let only = deck.slide_order[0].clone();
        let err = RemoveSlide { slide_id: only }.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::InvalidOperation(_)));
    }

    #[test]
    fn remove_missing_slide_errors() {
        let mut deck = Deck::sample();
        InsertSlide {
            position: 1,
            slide: blank_slide("s_b"),
            manifest_entry: entry_for("s_b"),
        }
        .apply(&mut deck)
        .unwrap();
        let err = RemoveSlide {
            slide_id: "ghost".into(),
        }
        .apply(&mut deck)
        .unwrap_err();
        assert!(matches!(err, CommandError::SlideNotFound(_)));
    }

    #[test]
    fn remove_then_inverse_restores_slide_verbatim() {
        let mut deck = Deck::sample();
        InsertSlide {
            position: 1,
            slide: blank_slide("s_b"),
            manifest_entry: entry_for("s_b"),
        }
        .apply(&mut deck)
        .unwrap();
        let order_before = deck.slide_order.clone();
        let slide_before = deck.slides.get("s_b").cloned().unwrap();

        let out = RemoveSlide {
            slide_id: "s_b".into(),
        }
        .apply(&mut deck)
        .unwrap();
        out.inverse.apply(&mut deck).unwrap();

        assert_eq!(deck.slide_order, order_before);
        assert_eq!(deck.slides.get("s_b").cloned().unwrap(), slide_before);
        assert!(deck.manifest.slides.iter().any(|e| e.id == "s_b"));
    }

    #[test]
    fn remove_preserves_position_for_inverse() {
        let mut deck = Deck::sample();

        InsertSlide {
            position: 1,
            slide: blank_slide("s_b"),
            manifest_entry: entry_for("s_b"),
        }
        .apply(&mut deck)
        .unwrap();
        InsertSlide {
            position: 2,
            slide: blank_slide("s_c"),
            manifest_entry: entry_for("s_c"),
        }
        .apply(&mut deck)
        .unwrap();
        let order_before = deck.slide_order.clone();

        let out = RemoveSlide {
            slide_id: "s_b".into(),
        }
        .apply(&mut deck)
        .unwrap();
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.slide_order, order_before);
        assert_eq!(deck.slide_order[1], "s_b");
    }

    fn three_slide_deck() -> Deck {
        let mut deck = Deck::sample();
        InsertSlide {
            position: 1,
            slide: blank_slide("s_b"),
            manifest_entry: entry_for("s_b"),
        }
        .apply(&mut deck)
        .unwrap();
        InsertSlide {
            position: 2,
            slide: blank_slide("s_c"),
            manifest_entry: entry_for("s_c"),
        }
        .apply(&mut deck)
        .unwrap();
        deck
    }

    #[test]
    fn reorder_moves_slide_forward_in_order_and_manifest() {
        let mut deck = three_slide_deck();
        let first: SlideId = deck.slide_order[0].clone();
        let out = ReorderSlide {
            slide_id: first.clone(),
            new_index: 2,
        }
        .apply(&mut deck)
        .unwrap();
        assert_eq!(deck.slide_order[2], first);
        assert_eq!(deck.manifest.slides[2].id, first);
        assert!(out.manifest_dirty);
        assert!(out.patches.is_empty());
    }

    #[test]
    fn reorder_clamps_out_of_range_index_to_last() {
        let mut deck = three_slide_deck();
        let first: SlideId = deck.slide_order[0].clone();
        ReorderSlide {
            slide_id: first.clone(),
            new_index: 999,
        }
        .apply(&mut deck)
        .unwrap();
        assert_eq!(deck.slide_order.last().cloned(), Some(first));
    }

    #[test]
    fn reorder_missing_slide_errors() {
        let mut deck = three_slide_deck();
        let err = ReorderSlide {
            slide_id: "ghost".into(),
            new_index: 0,
        }
        .apply(&mut deck)
        .unwrap_err();
        assert!(matches!(err, CommandError::SlideNotFound(_)));
    }

    #[test]
    fn reorder_then_inverse_restores_original_order() {
        let mut deck = three_slide_deck();
        let before = deck.slide_order.clone();
        let last: SlideId = deck.slide_order[2].clone();
        let out = ReorderSlide {
            slide_id: last,
            new_index: 0,
        }
        .apply(&mut deck)
        .unwrap();
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.slide_order, before);
    }
}
