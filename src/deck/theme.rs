// Theme data.
//
// Beyond the per-theme base CSS (`theme_css`) the theme now carries the
// layout-editor state (SPEC §11.4 / Stage 11): a deck-wide raw CSS blob
// (`globals_css`) injected into every shadow root, and the set of reusable
// layout templates with their canonical display order. The fuller palette /
// typography inspector (SPEC §6.2) lands later.
//
// `Eq` is dropped because `LayoutNode` embeds `ElementNode`, whose geometry
// is floating-point (PartialEq only) — mirroring `SlideNode`.

use crate::deck::ids::LayoutId;
use crate::deck::layout::LayoutNode;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ThemeData {
    pub theme_id: String,
    pub theme_css: String,
    pub globals_css: String,
    // BTreeMap for deterministic serialization; `layout_order` holds the
    // canonical display order because LayoutIds do not sort meaningfully
    // (same rationale as `slide_order`).
    pub layouts: BTreeMap<LayoutId, LayoutNode>,
    pub layout_order: Vec<LayoutId>,
}

const DEFAULT_THEME_CSS: &str = r#"
:host {
    --theme-accent: #0066ff;
    --theme-foreground: #111;
    --theme-muted: #888;
    --theme-background: #fff;
    --theme-title-family: -apple-system, "Helvetica Neue", Arial, sans-serif;
    --theme-body-family: -apple-system, "Helvetica Neue", Arial, sans-serif;
}
.slide {
    width: 1920px;
    height: 1080px;
    background: var(--theme-background);
    position: relative;
    /* Mask anything positioned beyond the slide bounds — content outside the
       canvas is never part of the rendered slide. */
    overflow: hidden;
}
.slide__content {
    position: relative;
    width: 100%;
    height: 100%;
}
[data-element-id] {
    position: absolute;
    user-select: none;
}
[data-element-type="text"] {
    /* Honor newline characters in text content so a box renders exactly
       what the user typed while editing it (WYSIWYG between the inline
       contenteditable session and the committed render). */
    white-space: pre-wrap;
    /* Flex column so the inspector's vertical-align control can map
       Top/Middle/Bottom to justify-content: flex-start/center/flex-end.
       Default flex-start keeps text at the top (unchanged from a plain box). */
    display: flex;
    flex-direction: column;
}
"#;

impl Default for ThemeData {
    fn default() -> Self {
        // Seed one empty "blank" layout so the layouts list is never empty
        // (the editor always has a layout to show / a fallback target).
        let blank_id: LayoutId = "blank".to_string();
        let blank_root = crate::deck::builders::group_element("el_layout_root", vec![]);
        let blank = LayoutNode::new(blank_id.clone(), "Blank".to_string(), blank_root);
        let mut layouts: BTreeMap<LayoutId, LayoutNode> = BTreeMap::new();
        layouts.insert(blank_id.clone(), blank);
        Self {
            theme_id: "default".into(),
            theme_css: DEFAULT_THEME_CSS.to_string(),
            globals_css: String::new(),
            layouts,
            layout_order: vec![blank_id],
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn default_theme_carries_known_vars() {
        let t = ThemeData::default();
        assert!(t.theme_css.contains("--theme-accent"));
        assert!(t.theme_css.contains("--theme-foreground"));
        assert!(!t.theme_id.is_empty());
    }

    #[test]
    fn default_theme_seeds_one_blank_layout() {
        let t = ThemeData::default();
        assert!(t.globals_css.is_empty());
        assert_eq!(t.layout_order, vec!["blank".to_string()]);
        assert_eq!(t.layouts.len(), 1);
        let blank = t.layouts.get("blank").expect("blank layout present");
        assert_eq!(blank.name, "Blank");
        assert!(blank.root.children.is_empty());
        assert_eq!(blank.root.element_type, crate::deck::element::ElementType::Group);
    }

    #[test]
    fn theme_serde_roundtrips() {
        let t = ThemeData::default();
        let json = serde_json::to_string(&t).unwrap();
        let back: ThemeData = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }
}
