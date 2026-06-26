// SwapTheme command.
//
// Theme save/load — the undoable import. Replaces `deck.theme` wholesale
// (theme_css + globals_css + layouts + order) and merges the imported theme's
// assets into the deck's registry. Slides are NOT restructured: their element
// trees are untouched; they simply re-render under the new theme/globals CSS on
// remount. Reversible via a self-inverse that carries the prior theme plus the
// mirror asset delta (so undo removes exactly what the import added, and redo
// re-adds it).
//
// It emits no DOM patches; it reports requires_remount + affects_layout_list +
// affects_globals + affects_assets so the editor re-mounts the active canvas,
// refreshes the layouts row / globals editor, and resends the asset bundle on
// apply, undo, and redo. `manifest_dirty` flags that a save is needed.

use crate::bundle::assets::AssetEntry;
use crate::commands::{Command, CommandError, CommandOutput};
use crate::deck::ThemeData;

#[derive(Debug, Clone)]
pub struct SwapTheme {
    pub install_theme: ThemeData,
    pub add_assets: Vec<(AssetEntry, Vec<u8>)>,
    pub remove_asset_ids: Vec<String>,
}

impl Command for SwapTheme {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with no patches, manifest_dirty=true, and an inverse
    // SwapTheme carrying the prior theme + the mirror asset delta.
    // Errors: none — installing a theme is always valid (trusted, validated on
    // load).
    // Dataflow: snapshot prior theme -> install -> add absent assets (recording
    // which were actually added) -> remove requested assets (capturing their
    // bytes) -> build the inverse from those captures.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        let prior_theme: ThemeData = deck.theme.clone();
        deck.theme = self.install_theme.clone();

        let mut added_ids: Vec<String> = Vec::new();
        for (entry, bytes) in &self.add_assets {
            if deck.assets.find_by_id(&entry.id).is_none() {
                deck.assets.assets.push(entry.clone());
                deck.assets.files.insert(entry.path.clone(), bytes.clone());
                added_ids.push(entry.id.clone());
            }
        }

        let mut removed: Vec<(AssetEntry, Vec<u8>)> = Vec::new();
        for id in &self.remove_asset_ids {
            let entry: AssetEntry = match deck.assets.find_by_id(id) {
                Some(e) => e.clone(),
                None => continue,
            };
            if let Some(bytes) = deck.assets.files.remove(&entry.path) {
                removed.push((entry.clone(), bytes));
            }
            deck.assets.assets.retain(|e| e.id != *id);
        }

        deck.manifest_dirty = true;
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(SwapTheme {
                install_theme: prior_theme,
                add_assets: removed,
                remove_asset_ids: added_ids,
            }),
            dirty_targets: Vec::new(),
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Apply Theme"
    }

    fn requires_remount(&self) -> bool {
        true
    }

    fn affects_layout_list(&self) -> bool {
        true
    }

    fn affects_globals(&self) -> bool {
        true
    }

    fn affects_assets(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::bundle::AssetRegistry;
    use crate::commands::{CommandDispatcher, EditorMode};
    use crate::deck::builders::{group_element, image_element};
    use crate::deck::{Deck, LayoutNode};

    // new_theme_with_layout: a distinct ThemeData carrying one extra layout.
    fn new_theme_with_layout(img_asset_id: &str) -> ThemeData {
        let img = image_element("el_img", img_asset_id);
        let root = group_element("el_layout_root", vec![img]);
        let mut theme = ThemeData {
            theme_css: ":host{--new:1}".into(),
            globals_css: "@keyframes z { to { opacity: 1 } }".into(),
            ..ThemeData::default()
        };
        theme.layouts.insert(
            "custom".into(),
            LayoutNode::new("custom".into(), "Custom".into(), root),
        );
        theme.layout_order.push("custom".into());
        theme
    }

    // one_asset: an (entry, bytes) pair built through a real registry so the id
    // is content-derived like production.
    fn one_asset() -> (AssetEntry, Vec<u8>) {
        let mut reg = AssetRegistry::new_empty();
        let entry = reg.insert_blob(
            vec![7, 7, 7, 7],
            "logo.png".into(),
            "image/png".into(),
            None,
        );
        let bytes = reg.files.get(&entry.path).unwrap().clone();
        (entry, bytes)
    }

    #[test]
    fn apply_replaces_theme_merges_assets_and_leaves_slides() {
        let mut deck = Deck::sample();
        let sid = deck.slide_order[0].clone();
        let slide_children_before = deck.slides[&sid].root.children.len();
        let (entry, bytes) = one_asset();
        let cmd = SwapTheme {
            install_theme: new_theme_with_layout(&entry.id),
            add_assets: vec![(entry.clone(), bytes)],
            remove_asset_ids: Vec::new(),
        };
        let out = cmd.apply(&mut deck).unwrap();

        assert!(deck.theme.theme_css.contains("--new"));
        assert!(deck.theme.layout_order.contains(&"custom".to_string()));
        assert!(deck.assets.find_by_id(&entry.id).is_some());
        // Slides untouched.
        assert_eq!(deck.slides[&sid].root.children.len(), slide_children_before);
        // Flags.
        assert!(cmd.requires_remount());
        assert!(cmd.affects_layout_list());
        assert!(cmd.affects_globals());
        assert!(cmd.affects_assets());
        assert!(out.manifest_dirty);
    }

    #[test]
    fn inverse_restores_prior_theme_and_removes_added_asset() {
        let mut deck = Deck::sample();
        let prior_css = deck.theme.theme_css.clone();
        let prior_layouts = deck.theme.layout_order.clone();
        let (entry, bytes) = one_asset();
        let cmd = SwapTheme {
            install_theme: new_theme_with_layout(&entry.id),
            add_assets: vec![(entry.clone(), bytes)],
            remove_asset_ids: Vec::new(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert!(deck.assets.find_by_id(&entry.id).is_some());

        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.theme.theme_css, prior_css);
        assert_eq!(deck.theme.layout_order, prior_layouts);
        // The added asset was removed by the inverse.
        assert!(deck.assets.find_by_id(&entry.id).is_none());
    }

    #[test]
    fn dispatcher_undo_redo_round_trips_theme_and_assets() {
        let mut d = CommandDispatcher::new(Deck::sample());
        d.set_mode(EditorMode::Layout);
        let prior_css = d.deck().theme.theme_css.clone();
        let (entry, bytes) = one_asset();
        d.dispatch(Box::new(SwapTheme {
            install_theme: new_theme_with_layout(&entry.id),
            add_assets: vec![(entry.clone(), bytes)],
            remove_asset_ids: Vec::new(),
        }))
        .unwrap();
        assert!(d.deck().theme.theme_css.contains("--new"));
        assert!(d.deck().assets.find_by_id(&entry.id).is_some());

        d.undo().unwrap();
        assert_eq!(d.deck().theme.theme_css, prior_css);
        assert!(d.deck().assets.find_by_id(&entry.id).is_none());

        d.redo().unwrap();
        assert!(d.deck().theme.theme_css.contains("--new"));
        assert!(d.deck().assets.find_by_id(&entry.id).is_some());
    }
}
