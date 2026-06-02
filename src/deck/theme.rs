// Theme data.
//
// Stage 3 only needs `theme_css` so the shadow root has CSS to apply.
// The fuller `ThemeData` per SPEC §6.2 (palette, typography, layouts,
// overrides) lands in later stages.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThemeData {
    pub theme_id: String,
    pub theme_css: String,
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
"#;

impl Default for ThemeData {
    fn default() -> Self {
        Self {
            theme_id: "default".into(),
            theme_css: DEFAULT_THEME_CSS.to_string(),
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
    fn theme_serde_roundtrips() {
        let t = ThemeData::default();
        let json = serde_json::to_string(&t).unwrap();
        let back: ThemeData = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }
}
