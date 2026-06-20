// Layout-lifecycle commands: InsertLayout, RemoveLayout, SetLayoutName.
//
// Stage 11 — layout editor. These are the theme-level analogues of the
// slide-lifecycle commands: rather than touching the active slide, they
// change the set, order, and names of reusable layout templates stored in
// `theme.layouts` + `theme.layout_order`.
//
// They produce no DOM patches; instead each reports
// `affects_layout_list() == true` so the editor rebroadcasts the layouts
// list. The theme is part of the deck's persisted state, so they set
// `manifest_dirty` to flag that a save is needed (the bundle writer emits
// the theme artifacts on save).
//
// InsertLayout and RemoveLayout are mutual inverses:
//   - InsertLayout.apply -> inverse RemoveLayout
//   - RemoveLayout.apply -> inverse InsertLayout (carrying the captured
//                           layout node + original position so undo restores
//                           the layout verbatim, edits and all).

use crate::commands::{Command, CommandError, CommandOutput};
use crate::deck::layout::LayoutNode;
use crate::deck::{CanvasTarget, LayoutId};

// InsertLayout
// Inserts a fully-formed layout at `position` in layout_order (and the
// layouts map). `position` is clamped to the current length, so an
// out-of-range index appends.
#[derive(Debug, Clone)]
pub struct InsertLayout {
    pub position: usize,
    pub layout: LayoutNode,
}

impl Command for InsertLayout {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with no patches, the layout marked dirty,
    // manifest_dirty=true, and an inverse RemoveLayout keyed on the
    // inserted layout's id.
    // Errors: Conflict if a layout with the same id already exists.
    // Dataflow: guard duplicate id -> clamp position -> insert into
    // theme.layouts + theme.layout_order -> build inverse.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        let layout_id: LayoutId = self.layout.id.clone();
        assert!(!layout_id.is_empty(), "InsertLayout: layout id is empty");
        if deck.theme.layouts.contains_key(&layout_id) {
            return Err(CommandError::Conflict(format!(
                "InsertLayout: layout {layout_id} already exists"
            )));
        }
        let order_pos: usize = self.position.min(deck.theme.layout_order.len());
        deck.theme.layouts.insert(layout_id.clone(), self.layout.clone());
        deck.theme.layout_order.insert(order_pos, layout_id.clone());
        deck.manifest_dirty = true;

        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(RemoveLayout { layout_id: layout_id.clone() }),
            dirty_targets: vec![CanvasTarget::Layout(layout_id)],
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Add Layout"
    }

    fn affects_layout_list(&self) -> bool {
        true
    }
}

// RemoveLayout
// Removes the layout with `layout_id` from theme.layouts + layout_order.
// Refuses to remove the theme's last layout (a theme must always hold at
// least one). Its inverse is an InsertLayout carrying the removed node at
// its original position so undo restores it exactly.
#[derive(Debug, Clone)]
pub struct RemoveLayout {
    pub layout_id: LayoutId,
}

impl Command for RemoveLayout {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with no patches, manifest_dirty=true, and an
    // inverse InsertLayout carrying the removed layout + original position.
    // Errors:
    //   LayoutNotFound   — layout_id absent.
    //   InvalidOperation — attempting to remove the theme's last layout.
    // Dataflow: guard last-layout -> find order position -> remove from
    // layout_order + layouts -> build inverse with the captured node.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.layout_id.is_empty(), "RemoveLayout: layout id is empty");
        if !deck.theme.layouts.contains_key(&self.layout_id) {
            return Err(CommandError::LayoutNotFound(self.layout_id.clone()));
        }
        if deck.theme.layout_order.len() <= 1 {
            return Err(CommandError::InvalidOperation(
                "RemoveLayout: cannot remove the theme's only layout".into(),
            ));
        }
        let order_pos: usize = deck
            .theme
            .layout_order
            .iter()
            .position(|id| id == &self.layout_id)
            .ok_or_else(|| CommandError::LayoutNotFound(self.layout_id.clone()))?;

        let removed_layout: LayoutNode = deck
            .theme
            .layouts
            .remove(&self.layout_id)
            .ok_or_else(|| CommandError::LayoutNotFound(self.layout_id.clone()))?;
        deck.theme.layout_order.remove(order_pos);
        deck.manifest_dirty = true;

        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(InsertLayout {
                position: order_pos,
                layout: removed_layout,
            }),
            dirty_targets: Vec::new(),
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Delete Layout"
    }

    fn affects_layout_list(&self) -> bool {
        true
    }
}

// SetLayoutName
// Updates a layout's display `name`. The id (map key + on-disk filename)
// is immutable here; only the human-readable label changes. Its inverse
// restores the prior name.
#[derive(Debug, Clone)]
pub struct SetLayoutName {
    pub layout_id: LayoutId,
    pub new_name: String,
}

impl Command for SetLayoutName {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with no patches, the layout marked dirty,
    // manifest_dirty=true, and an inverse SetLayoutName carrying the prior
    // name.
    // Errors: LayoutNotFound when the id is absent.
    // Dataflow: locate layout -> snapshot prior name -> overwrite -> build
    // inverse.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.layout_id.is_empty(), "SetLayoutName: layout id is empty");
        let layout = deck
            .theme
            .layouts
            .get_mut(&self.layout_id)
            .ok_or_else(|| CommandError::LayoutNotFound(self.layout_id.clone()))?;
        let prior_name: String = layout.name.clone();
        layout.name = self.new_name.clone();
        layout.dirty = true;
        deck.manifest_dirty = true;

        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(SetLayoutName {
                layout_id: self.layout_id.clone(),
                new_name: prior_name,
            }),
            dirty_targets: vec![CanvasTarget::Layout(self.layout_id.clone())],
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Rename Layout"
    }

    fn affects_layout_list(&self) -> bool {
        true
    }
}

// SetLayoutBackground / SetLayoutBackgroundImage
// Theme-level background for a layout. Slides built on the layout inherit
// these when their own field is empty (Deck::effective_slide_bg). Mirrors the
// slide background commands: self-inverse, remounts the layout canvas, and
// reports affects_layout_list so the layout thumbnail re-renders. The theme is
// persisted state, so manifest_dirty flags a save.
#[derive(Debug, Clone)]
pub struct SetLayoutBackground {
    pub layout_id: LayoutId,
    pub background: Option<String>,
}

impl Command for SetLayoutBackground {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.layout_id.is_empty(), "SetLayoutBackground: layout id is empty");
        let layout = deck
            .theme
            .layouts
            .get_mut(&self.layout_id)
            .ok_or_else(|| CommandError::LayoutNotFound(self.layout_id.clone()))?;
        let prior: Option<String> = layout.background.clone();
        layout.background = self.background.clone();
        layout.dirty = true;
        deck.manifest_dirty = true;
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(SetLayoutBackground {
                layout_id: self.layout_id.clone(),
                background: prior,
            }),
            dirty_targets: vec![CanvasTarget::Layout(self.layout_id.clone())],
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Set Layout Background"
    }

    fn requires_remount(&self) -> bool {
        true
    }

    fn affects_layout_list(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone)]
pub struct SetLayoutBackgroundImage {
    pub layout_id: LayoutId,
    pub background_image: Option<String>,
}

impl Command for SetLayoutBackgroundImage {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.layout_id.is_empty(), "SetLayoutBackgroundImage: layout id is empty");
        let layout = deck
            .theme
            .layouts
            .get_mut(&self.layout_id)
            .ok_or_else(|| CommandError::LayoutNotFound(self.layout_id.clone()))?;
        let prior: Option<String> = layout.background_image.clone();
        layout.background_image = self.background_image.clone();
        layout.dirty = true;
        deck.manifest_dirty = true;
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(SetLayoutBackgroundImage {
                layout_id: self.layout_id.clone(),
                background_image: prior,
            }),
            dirty_targets: vec![CanvasTarget::Layout(self.layout_id.clone())],
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Set Layout Background Image"
    }

    fn requires_remount(&self) -> bool {
        true
    }

    fn affects_layout_list(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;
    use crate::deck::builders::group_element;

    fn blank_layout(id: &str, name: &str) -> LayoutNode {
        LayoutNode::new(id.into(), name.into(), group_element("el_layout_root", vec![]))
    }

    #[test]
    fn layout_background_sets_and_inverts() {
        let mut deck = Deck::default();
        let lid = deck.theme.layout_order[0].clone();
        let cmd = SetLayoutBackground { layout_id: lid.clone(), background: Some("#0af".into()) };
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(deck.theme.layouts[&lid].background, Some("#0af".to_string()));
        assert!(cmd.requires_remount());
        assert!(cmd.affects_layout_list());
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.theme.layouts[&lid].background, None);
    }

    #[test]
    fn layout_background_image_sets_and_inverts() {
        let mut deck = Deck::default();
        let lid = deck.theme.layout_order[0].clone();
        let out = SetLayoutBackgroundImage {
            layout_id: lid.clone(),
            background_image: Some("var(--asset-z)".into()),
        }
        .apply(&mut deck)
        .unwrap();
        assert_eq!(
            deck.theme.layouts[&lid].background_image,
            Some("var(--asset-z)".to_string())
        );
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.theme.layouts[&lid].background_image, None);
    }

    #[test]
    fn insert_layout_adds_to_map_and_order() {
        let mut deck = Deck::default();
        let before = deck.theme.layout_order.len();
        let cmd = InsertLayout { position: before, layout: blank_layout("title", "Title") };
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(deck.theme.layout_order.len(), before + 1);
        assert!(deck.theme.layouts.contains_key("title"));
        assert_eq!(deck.theme.layout_order.last().map(String::as_str), Some("title"));
        assert!(out.patches.is_empty());
        assert!(out.manifest_dirty);
        assert!(cmd.affects_layout_list());
    }

    #[test]
    fn insert_layout_at_position_inserts_in_order() {
        let mut deck = Deck::default();
        let cmd = InsertLayout { position: 0, layout: blank_layout("first", "First") };
        cmd.apply(&mut deck).unwrap();
        assert_eq!(deck.theme.layout_order[0], "first");
    }

    #[test]
    fn insert_layout_clamps_out_of_range_position() {
        let mut deck = Deck::default();
        let cmd = InsertLayout { position: 999, layout: blank_layout("app", "App") };
        cmd.apply(&mut deck).unwrap();
        assert_eq!(deck.theme.layout_order.last().map(String::as_str), Some("app"));
    }

    #[test]
    fn insert_layout_rejects_duplicate_id() {
        let mut deck = Deck::default();
        // "blank" is the seeded default layout.
        let cmd = InsertLayout { position: 0, layout: blank_layout("blank", "Dup") };
        let err = cmd.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::Conflict(_)));
    }

    #[test]
    fn insert_then_inverse_removes_the_layout() {
        let mut deck = Deck::default();
        let before = deck.theme.layout_order.clone();
        let out = InsertLayout { position: 1, layout: blank_layout("tmp", "Tmp") }
            .apply(&mut deck)
            .unwrap();
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.theme.layout_order, before);
        assert!(!deck.theme.layouts.contains_key("tmp"));
    }

    #[test]
    fn remove_layout_drops_from_map_and_order() {
        let mut deck = Deck::default();
        InsertLayout { position: 1, layout: blank_layout("b", "B") }
            .apply(&mut deck)
            .unwrap();
        let cmd = RemoveLayout { layout_id: "b".into() };
        let out = cmd.apply(&mut deck).unwrap();
        assert!(!deck.theme.layouts.contains_key("b"));
        assert!(!deck.theme.layout_order.iter().any(|id| id == "b"));
        assert!(out.manifest_dirty);
        assert!(cmd.affects_layout_list());
    }

    #[test]
    fn remove_last_layout_is_rejected() {
        let mut deck = Deck::default();
        assert_eq!(deck.theme.layout_order.len(), 1);
        let only = deck.theme.layout_order[0].clone();
        let err = RemoveLayout { layout_id: only }.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::InvalidOperation(_)));
    }

    #[test]
    fn remove_missing_layout_errors() {
        let mut deck = Deck::default();
        InsertLayout { position: 1, layout: blank_layout("b", "B") }
            .apply(&mut deck)
            .unwrap();
        let err = RemoveLayout { layout_id: "ghost".into() }.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::LayoutNotFound(_)));
    }

    #[test]
    fn remove_then_inverse_restores_layout_verbatim() {
        let mut deck = Deck::default();
        InsertLayout { position: 1, layout: blank_layout("b", "B") }
            .apply(&mut deck)
            .unwrap();
        let order_before = deck.theme.layout_order.clone();
        let layout_before = deck.theme.layouts.get("b").cloned().unwrap();

        let out = RemoveLayout { layout_id: "b".into() }.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();

        assert_eq!(deck.theme.layout_order, order_before);
        assert_eq!(deck.theme.layouts.get("b").cloned().unwrap(), layout_before);
    }

    #[test]
    fn remove_preserves_position_for_inverse() {
        let mut deck = Deck::default();
        InsertLayout { position: 1, layout: blank_layout("b", "B") }.apply(&mut deck).unwrap();
        InsertLayout { position: 2, layout: blank_layout("c", "C") }.apply(&mut deck).unwrap();
        let order_before = deck.theme.layout_order.clone();
        let out = RemoveLayout { layout_id: "b".into() }.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.theme.layout_order, order_before);
        assert_eq!(deck.theme.layout_order[1], "b");
    }

    #[test]
    fn set_layout_name_updates_and_inverts() {
        let mut deck = Deck::default();
        let cmd = SetLayoutName { layout_id: "blank".into(), new_name: "Renamed".into() };
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(deck.theme.layouts["blank"].name, "Renamed");
        assert!(cmd.affects_layout_list());
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.theme.layouts["blank"].name, "Blank");
    }

    #[test]
    fn set_layout_name_errors_on_missing_layout() {
        let mut deck = Deck::default();
        let err = SetLayoutName { layout_id: "ghost".into(), new_name: "X".into() }
            .apply(&mut deck)
            .unwrap_err();
        assert!(matches!(err, CommandError::LayoutNotFound(_)));
    }
}
