// Default deck templates.
//
// The light/dark starter themes the landing page offers: each carries the
// `--theme-*` palette, a globals_css block of four text presets (title /
// slide-header / default-text / footnote, written with the
// [data-element-type="text"].<class> convention), and three layouts
// (title / hero / text). `new_deck` builds a blank one-slide deck seeded from a
// chosen layout; `catalog` lists the 2×3 entries for the landing rows.
//
// Content only — the boot path still uses Deck::sample until the landing flow
// is wired (later sub-project).

use crate::deck::Deck;
use crate::deck::builders::{group_element, shape_element, text_element_styled};
use crate::deck::element::{ElementStyle, ShapeGeometry};
use crate::deck::layout::LayoutNode;
use crate::deck::style::{ColorRef, FillRef, Geometry, Length, TextStyle};
use crate::deck::theme::ThemeData;
use std::collections::BTreeMap;

// System sans stack reused for both title and body families (no bundling
// dependency at launch).
const FONT_STACK: &str = "-apple-system, \"Helvetica Neue\", Arial, sans-serif";

// Structural slide CSS shared by every theme (mirrors the base in
// ThemeData::default); only the :host vars differ per theme.
const BASE_SLIDE_CSS: &str = r#"
.slide {
    width: 1920px;
    height: 1080px;
    background: var(--theme-background);
    position: relative;
    overflow: hidden;
}
.slide__content { position: relative; width: 100%; height: 100%; }
[data-element-id] { position: absolute; user-select: none; }
[data-element-type="text"] { white-space: pre-wrap; display: flex; flex-direction: column; }
"#;

// The four text presets, identical across themes (colours resolve through the
// per-theme --theme-* vars).
const PRESET_CSS: &str = r#"[data-element-type="text"].title {
    font-size: 96px;
    font-weight: 700;
    line-height: 1.05;
    letter-spacing: -0.02em;
    color: var(--theme-foreground);
}
[data-element-type="text"].slide-header {
    font-size: 56px;
    font-weight: 600;
    line-height: 1.1;
    color: var(--theme-foreground);
}
[data-element-type="text"].default-text {
    font-size: 32px;
    font-weight: 400;
    line-height: 1.4;
    color: var(--theme-foreground);
}
[data-element-type="text"].footnote {
    font-size: 20px;
    font-weight: 400;
    line-height: 1.3;
    color: var(--theme-muted);
}"#;

// TemplateEntry
// One landing-row option: a theme + one of its layouts, with display names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateEntry {
    pub theme_id: String,
    pub theme_name: String,
    pub layout_id: String,
    pub layout_name: String,
}

// theme_css
// Inputs: the four palette colours. Output: the full per-theme CSS — the
// :host variable block followed by the shared structural slide rules.
fn theme_css(bg: &str, fg: &str, muted: &str, accent: &str) -> String {
    format!(
        ":host {{\n    --theme-accent: {accent};\n    --theme-foreground: {fg};\n    \
--theme-muted: {muted};\n    --theme-background: {bg};\n    \
--theme-title-family: {FONT_STACK};\n    --theme-body-family: {FONT_STACK};\n}}\n{}",
        BASE_SLIDE_CSS
    )
}

// geom / tstyle
// Small constructors keeping the layout builders readable.
fn geom(x: f64, y: f64, w: f64, h: f64) -> Geometry {
    Geometry {
        x,
        y,
        width: w,
        height: h,
        ..Geometry::default()
    }
}

fn tstyle(size: f64, weight: u16, line_height: f64, color_key: &str) -> TextStyle {
    TextStyle {
        font_size: Length::px(size),
        font_weight: weight,
        line_height,
        color: ColorRef::Theme(color_key.to_string()),
        ..TextStyle::default()
    }
}

// title_layout / hero_layout / text_layout
// The three layout templates. Text is baked with TextStyle matching the
// presets (stamp model — inline beats class — so the layout's own text already
// looks right; the presets serve new text the user adds).
fn title_layout() -> LayoutNode {
    let title = text_element_styled(
        "el_title",
        "Title",
        geom(160.0, 420.0, 1600.0, 180.0),
        tstyle(96.0, 700, 1.05, "foreground"),
    );
    let subtitle = text_element_styled(
        "el_subtitle",
        "Subtitle",
        geom(160.0, 620.0, 1600.0, 90.0),
        tstyle(32.0, 400, 1.4, "muted"),
    );
    let root = group_element("el_layout_root", vec![title, subtitle]);
    LayoutNode::new("title".to_string(), "Title".to_string(), root)
}

fn hero_layout() -> LayoutNode {
    let title = text_element_styled(
        "el_hero_title",
        "Hero headline",
        geom(160.0, 360.0, 1080.0, 360.0),
        tstyle(96.0, 700, 1.05, "foreground"),
    );
    let copy = text_element_styled(
        "el_hero_copy",
        "Supporting copy",
        geom(160.0, 760.0, 1000.0, 160.0),
        tstyle(32.0, 400, 1.4, "foreground"),
    );
    let mut block = shape_element(
        "el_hero_block",
        ShapeGeometry::RoundedRect { radius_px: 24 },
    );
    block.geometry = geom(1300.0, 240.0, 460.0, 600.0);
    if let ElementStyle::Shape(s) = &mut block.style {
        s.fill = FillRef::Color(ColorRef::Theme("accent".to_string()));
    }
    let root = group_element("el_layout_root", vec![title, copy, block]);
    LayoutNode::new("hero".to_string(), "Hero".to_string(), root)
}

fn text_layout() -> LayoutNode {
    let header = text_element_styled(
        "el_header",
        "Section header",
        geom(160.0, 140.0, 1600.0, 120.0),
        tstyle(56.0, 600, 1.1, "foreground"),
    );
    let body = text_element_styled(
        "el_body",
        "Body text",
        geom(160.0, 320.0, 1600.0, 560.0),
        tstyle(32.0, 400, 1.4, "foreground"),
    );
    let footnote = text_element_styled(
        "el_footnote",
        "Footnote",
        geom(160.0, 980.0, 1600.0, 60.0),
        tstyle(20.0, 400, 1.3, "muted"),
    );
    let root = group_element("el_layout_root", vec![header, body, footnote]);
    LayoutNode::new("text".to_string(), "Text".to_string(), root)
}

// build_theme
// Inputs: the theme id + palette. Output: a ThemeData with the four presets in
// globals_css and the three ordered layouts.
fn build_theme(theme_id: &str, bg: &str, fg: &str, muted: &str, accent: &str) -> ThemeData {
    let mut layouts: BTreeMap<String, LayoutNode> = BTreeMap::new();
    for layout in [title_layout(), hero_layout(), text_layout()] {
        layouts.insert(layout.id.clone(), layout);
    }
    ThemeData {
        theme_id: theme_id.to_string(),
        theme_css: theme_css(bg, fg, muted, accent),
        globals_css: PRESET_CSS.to_string(),
        layouts,
        layout_order: vec!["title".to_string(), "hero".to_string(), "text".to_string()],
    }
}

// Per-theme palettes as (background, foreground, muted, accent), shared by the
// theme builders and the landing-card preview colours so they never diverge.
const LIGHT_PALETTE: (&str, &str, &str, &str) = ("#ffffff", "#1a1a1a", "#6b6b6b", "#f19035");
const DARK_PALETTE: (&str, &str, &str, &str) = ("#16140f", "#f4f2ec", "#9a9384", "#f19035");

// light_theme / dark_theme
// The two starter themes.
pub fn light_theme() -> ThemeData {
    let (bg, fg, muted, accent) = LIGHT_PALETTE;
    build_theme("light", bg, fg, muted, accent)
}

pub fn dark_theme() -> ThemeData {
    let (bg, fg, muted, accent) = DARK_PALETTE;
    build_theme("dark", bg, fg, muted, accent)
}

// theme_by_id
// Inputs: a theme id. Output: the matching starter theme; unknown ids default
// to light.
pub fn theme_by_id(theme_id: &str) -> ThemeData {
    match theme_id {
        "dark" => dark_theme(),
        _ => light_theme(),
    }
}

// theme_palette
// Inputs: a theme id. Output: (background, foreground, accent) for the landing
// card previews. Unknown ids default to light.
pub fn theme_palette(theme_id: &str) -> (String, String, String) {
    let (bg, fg, _muted, accent) = if theme_id == "dark" {
        DARK_PALETTE
    } else {
        LIGHT_PALETTE
    };
    (bg.to_string(), fg.to_string(), accent.to_string())
}

// new_deck
// Inputs: a theme and a layout id. Output: a blank one-slide deck on that
// theme whose slide is seeded by cloning the layout's children (so the chosen
// layout opens with editable elements). An unknown layout id falls back to the
// theme's first layout. Control flow: start from Deck::new_blank, swap the
// theme, then re-root the single slide on the resolved layout's children.
pub fn new_deck(theme: ThemeData, layout_id: &str) -> Deck {
    let mut deck = Deck::new_blank();
    deck.theme = theme;
    let resolved: String = if deck.theme.layouts.contains_key(layout_id) {
        layout_id.to_string()
    } else {
        deck.theme
            .layout_order
            .first()
            .cloned()
            .unwrap_or_else(|| "blank".to_string())
    };
    let children = deck
        .theme
        .layouts
        .get(&resolved)
        .map(|l| l.root.children.clone())
        .unwrap_or_default();
    let sid: String = deck.slide_order.first().cloned().unwrap_or_default();
    if let Some(slide) = deck.slides.get_mut(&sid) {
        slide.layout_id = resolved.clone();
        slide.root = group_element("el_root", children);
    }
    if let Some(entry) = deck.manifest.slides.iter_mut().find(|e| e.id == sid) {
        entry.layout_id = resolved;
    }
    deck
}

// new_deck_all_layouts
// Inputs: a theme. Output: a deck carrying one slide per layout in the theme's
// canonical order, each slide seeded by cloning that layout's children. This is
// what the landing's per-theme card opens — the whole "Light"/"Dark" starter,
// not a single chosen layout. Control flow: start from a blank deck, swap the
// theme, then rebuild slides/order/manifest from layout_order.
pub fn new_deck_all_layouts(theme: ThemeData) -> Deck {
    use crate::bundle::{SlideEntry, manifest::slide_path_for};
    use crate::deck::ids::new_slide_id;
    use crate::deck::slide::SlideNode;
    assert!(
        !theme.layout_order.is_empty(),
        "new_deck_all_layouts: theme has no layouts"
    );
    let mut deck: Deck = Deck::new_blank();
    deck.theme = theme;
    let mut slides: BTreeMap<String, SlideNode> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut entries: Vec<SlideEntry> = Vec::new();
    for layout_id in deck.theme.layout_order.clone() {
        let children = deck
            .theme
            .layouts
            .get(&layout_id)
            .map(|l| l.root.children.clone())
            .unwrap_or_default();
        let sid: String = new_slide_id();
        let root = group_element("el_root", children);
        slides.insert(
            sid.clone(),
            SlideNode::new(sid.clone(), layout_id.clone(), root),
        );
        order.push(sid.clone());
        entries.push(SlideEntry {
            id: sid.clone(),
            path: slide_path_for(&sid),
            layout_id: layout_id.clone(),
            title: String::new(),
            thumbnail: None,
            transition: None,
            duration_hint: None,
            notes_ref: None,
            animations: Vec::new(),
            background: None,
            background_image: None,
            notes: None,
        });
    }
    deck.slides = slides;
    deck.slide_order = order;
    deck.manifest.slides = entries;
    deck
}

// catalog
// Output: the landing template cards — one per starter theme. Each card opens
// the full theme (every layout as a slide) via new_deck_all_layouts; the
// layouts are no longer individually selectable, so layout_id/name stay empty.
pub fn catalog() -> Vec<TemplateEntry> {
    let mut out: Vec<TemplateEntry> = Vec::new();
    for (theme, theme_name) in [(light_theme(), "Light"), (dark_theme(), "Dark")] {
        out.push(TemplateEntry {
            theme_id: theme.theme_id.clone(),
            theme_name: theme_name.to_string(),
            layout_id: String::new(),
            layout_name: String::new(),
        });
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn preset_classes(globals: &str) -> Vec<&str> {
        ["title", "slide-header", "default-text", "footnote"]
            .into_iter()
            .filter(|c| {
                globals.contains(&format!(".{} ", c)) || globals.contains(&format!(".{}\n", c))
            })
            .collect()
    }

    #[test]
    fn themes_carry_four_type_scoped_presets() {
        for theme in [light_theme(), dark_theme()] {
            assert!(theme.globals_css.contains("[data-element-type=\"text\"]"));
            assert_eq!(preset_classes(&theme.globals_css).len(), 4);
        }
    }

    #[test]
    fn themes_have_three_ordered_layouts() {
        for theme in [light_theme(), dark_theme()] {
            assert_eq!(theme.layout_order, vec!["title", "hero", "text"]);
            assert_eq!(theme.layouts.len(), 3);
        }
    }

    #[test]
    fn palettes_differ() {
        assert!(
            light_theme()
                .theme_css
                .contains("--theme-background: #ffffff")
        );
        assert!(
            dark_theme()
                .theme_css
                .contains("--theme-background: #16140f")
        );
    }

    #[test]
    fn new_deck_seeds_slide_from_layout() {
        let deck = new_deck(light_theme(), "title");
        let sid = &deck.slide_order[0];
        let slide = &deck.slides[sid];
        assert_eq!(slide.layout_id, "title");
        assert_eq!(slide.root.children.len(), 2); // title + subtitle
        let entry = deck.manifest.slides.iter().find(|e| &e.id == sid).unwrap();
        assert_eq!(entry.layout_id, "title");
    }

    #[test]
    fn new_deck_hero_has_shape_block() {
        let deck = new_deck(dark_theme(), "hero");
        let slide = &deck.slides[&deck.slide_order[0]];
        assert_eq!(slide.root.children.len(), 3); // title + copy + accent block
    }

    #[test]
    fn new_deck_unknown_layout_falls_back_to_first() {
        let deck = new_deck(light_theme(), "nope");
        let slide = &deck.slides[&deck.slide_order[0]];
        assert_eq!(slide.layout_id, "title");
    }

    #[test]
    fn catalog_lists_one_card_per_theme() {
        let c = catalog();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].theme_id, "light");
        assert_eq!(c[1].theme_id, "dark");
        for e in &c {
            assert!(!e.theme_name.is_empty());
        }
    }

    #[test]
    fn new_deck_all_layouts_has_a_slide_per_layout() {
        let deck = new_deck_all_layouts(light_theme());
        assert_eq!(deck.slide_order.len(), 3);
        assert_eq!(deck.manifest.slides.len(), 3);
        let layouts: Vec<&str> = deck
            .slide_order
            .iter()
            .map(|s| deck.slides[s].layout_id.as_str())
            .collect();
        assert_eq!(layouts, vec!["title", "hero", "text"]);
    }

    #[test]
    fn themes_serde_roundtrip() {
        for theme in [light_theme(), dark_theme()] {
            let json = serde_json::to_string(&theme).unwrap();
            let back: ThemeData = serde_json::from_str(&json).unwrap();
            assert_eq!(back, theme);
        }
    }
}
