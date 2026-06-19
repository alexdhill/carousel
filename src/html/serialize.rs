// HTML serializer.

#![allow(dead_code)]

//
// Walks an `ElementNode` depth-first and writes a compact HTML fragment.
// Compact = no whitespace between tags or around text content; this avoids
// whitespace text nodes drifting in during round-trips through the parser.
//
// Tag policy: every element serializes as `<div>` for Stage 3. The element
// type lives in `data-element-type`, so semantic tag information is not
// needed for re-rendering. Layout-driven tag choice (h1/p) is a Stage 5
// concern when layouts compose with slides.
//
// Attribute order: alphabetic, via BTreeMap. Determinism matters for
// content-hashing slides on save (Stage 7) and for clean diffs in version
// control.

use crate::deck::element::{ElementContent, ElementNode, ElementStyle};
use crate::deck::slide::SlideNode;
use crate::deck::style::*;
use std::collections::BTreeMap;

// ANIMATION_KEYFRAMES_CSS
// The immutable built-in @keyframes library injected into every shadow root
// (viewport mount + thumbnails) alongside theme_css / globals_css. Inert
// until a playback runtime exists; users cannot delete these (custom
// @keyframes live in the editable theme.globals_css instead).
pub const ANIMATION_KEYFRAMES_CSS: &str = r#"
@keyframes appear { from { opacity: 0; } to { opacity: 1; } }
@keyframes disappear { from { opacity: 1; } to { opacity: 0; } }
@keyframes fade-in { from { opacity: 0; } to { opacity: 1; } }
@keyframes fade-out { from { opacity: 1; } to { opacity: 0; } }
@keyframes fly-in-left { from { opacity: 0; transform: translateX(-40px); } to { opacity: 1; transform: none; } }
@keyframes fly-in-right { from { opacity: 0; transform: translateX(40px); } to { opacity: 1; transform: none; } }
@keyframes fly-in-top { from { opacity: 0; transform: translateY(-40px); } to { opacity: 1; transform: none; } }
@keyframes fly-in-bottom { from { opacity: 0; transform: translateY(40px); } to { opacity: 1; transform: none; } }
@keyframes pulse { 0% { transform: scale(1); } 50% { transform: scale(1.06); } 100% { transform: scale(1); } }
@keyframes scale-in { from { opacity: 0; transform: scale(.8); } to { opacity: 1; transform: none; } }
@keyframes scale-out { from { opacity: 1; transform: none; } to { opacity: 0; transform: scale(.8); } }
@keyframes blur-in { from { opacity: 0; filter: blur(8px); } to { opacity: 1; filter: blur(0); } }
@keyframes blur-out { from { opacity: 1; filter: blur(0); } to { opacity: 0; filter: blur(8px); } }
@keyframes fly-out-left { from { opacity: 1; transform: none; } to { opacity: 0; transform: translateX(-40px); } }
@keyframes fly-out-right { from { opacity: 1; transform: none; } to { opacity: 0; transform: translateX(40px); } }
@keyframes fly-out-top { from { opacity: 1; transform: none; } to { opacity: 0; transform: translateY(-40px); } }
@keyframes fly-out-bottom { from { opacity: 1; transform: none; } to { opacity: 0; transform: translateY(40px); } }
@keyframes bounce { 0% { transform: translateY(0); } 30% { transform: translateY(-18px); } 55% { transform: translateY(0); } 75% { transform: translateY(-8px); } 100% { transform: translateY(0); } }
@keyframes shake { 0%,100% { transform: translateX(0); } 20% { transform: translateX(-8px); } 40% { transform: translateX(8px); } 60% { transform: translateX(-6px); } 80% { transform: translateX(6px); } }
@keyframes spin { from { transform: rotate(0); } to { transform: rotate(360deg); } }
@keyframes flash { 0%,100% { opacity: 1; } 25%,75% { opacity: .25; } 50% { opacity: 1; } }
"#;

// AnimMap: element id → its animation entry ids (in timeline order). Built
// from `SlideNode.animations` and threaded through the writers so each
// animated element gets a `data-anim-ids` targeting tag (a forward-looking
// hook for a future runtime; inert now, dropped by the parser on read).
type AnimMap<'a> = BTreeMap<&'a str, Vec<&'a str>>;

// serialize_element
// Inputs: a reference to a tree-rooted ElementNode.
// Output: an HTML fragment string containing exactly one top-level element.
// Dataflow: allocates a String buffer and walks the tree. The top-level
// element receives no z-index — z-index is only meaningful relative to
// siblings, and a standalone element has none.
pub fn serialize_element(node: &ElementNode) -> String {
    assert!(node.is_consistent(), "cannot serialize inconsistent element");
    let mut out: String = String::new();
    // Standalone elements (e.g. InsertElement patches) have no slide-level
    // animation context, so they carry no targeting tag; the element picks
    // one up on the next full slide mount.
    let anim: AnimMap = AnimMap::new();
    write_node(node, None, &anim, &mut out);
    out
}

// serialize_slide
// Inputs: a SlideNode whose root is a Group.
// Output: an HTML fragment of the form
//   <section class="slide" data-slide-id="…" data-layout="…"
//            data-root-id="…">
//     <div class="slide__content">…children…</div>
//   </section>
// `data-root-id` preserves the slide's root element id across round-trips
// so SlideNode comparison stays exact. Each child of the slide's root
// group receives a z-index equal to its sibling index — z is therefore
// app-determined by tree position, not by any stored z_order field.
pub fn serialize_slide(slide: &SlideNode) -> String {
    assert!(
        slide.root.is_consistent(),
        "slide root must satisfy the element-triple invariant"
    );
    // Build the element → animation-ids map (timeline order) for the tag.
    let mut anim: AnimMap = AnimMap::new();
    for e in &slide.animations {
        anim.entry(e.element_id.as_str()).or_default().push(e.id.as_str());
    }
    let mut out: String = String::new();
    out.push_str("<section class=\"slide\" data-slide-id=\"");
    out.push_str(&escape_attr(&slide.id));
    out.push_str("\" data-layout=\"");
    out.push_str(&escape_attr(&slide.layout_id));
    out.push_str("\" data-root-id=\"");
    out.push_str(&escape_attr(&slide.root.id));
    out.push('"');
    // Per-slide background overrides the theme's .slide background via an inline
    // style (inline beats the class rule). Omitted when None so the slide
    // inherits the theme background.
    if let Some(bg) = &slide.metadata.background {
        if !bg.is_empty() {
            out.push_str(" style=\"background:");
            out.push_str(&escape_attr(bg));
            out.push('"');
        }
    }
    out.push_str("><div class=\"slide__content\">");
    for (idx, child) in slide.root.children.iter().enumerate() {
        write_node(child, Some(idx as i32), &anim, &mut out);
    }
    out.push_str("</div></section>");
    out
}

// write_node
// Inputs: a node, an optional sibling z-index (None when the node is the
// top-level of a serialize_element call; Some(i) when it sits inside a
// parent's children list), an out buffer.
// Output: side-effect; appends `<div …>content</div>` to out.
// Dataflow: attributes (with computed z-index in the style attr) →
// content → recurse for groups, threading per-child sibling indices.
fn write_node(node: &ElementNode, sibling_index: Option<i32>, anim: &AnimMap, out: &mut String) {
    out.push_str("<div");
    write_attributes(node, sibling_index, anim, out);
    out.push('>');
    write_content(node, anim, out);
    out.push_str("</div>");
}

// write_attributes
// Inputs: node, optional sibling index for the z-index decl, out buffer.
// Output: side-effect; appends ` k="v"` pairs in BTreeMap (alphabetic)
// order. Model-owned attributes overwrite user-supplied ones of the same
// key so the parser can rely on canonical key names.
fn write_attributes(node: &ElementNode, sibling_index: Option<i32>, anim: &AnimMap, out: &mut String) {
    let mut attrs: BTreeMap<String, String> = node.attributes.clone();
    attrs.insert("data-element-id".into(), node.id.clone());
    attrs.insert(
        "data-element-type".into(),
        node.element_type.as_html().to_string(),
    );
    if let Some(v) = &node.name {
        attrs.insert("data-name".into(), v.clone());
    }
    if let Some(v) = &node.link {
        attrs.insert("data-link".into(), v.clone());
    }
    if let Some(v) = &node.placeholder_fill {
        attrs.insert("data-placeholder-fill".into(), v.clone());
    }
    // Animation targeting tag (derived from the slide timeline; dropped by
    // the parser on read so it never accumulates).
    if let Some(ids) = anim.get(node.id.as_str()) {
        attrs.insert("data-anim-ids".into(), ids.join(" "));
    }
    add_content_attrs(node, &mut attrs);
    let style: String = build_style(node, sibling_index);
    if !style.is_empty() {
        attrs.insert("style".into(), style);
    }
    for (k, v) in &attrs {
        out.push(' ');
        out.push_str(k);
        out.push_str("=\"");
        out.push_str(&escape_attr(v));
        out.push('"');
    }
}

// add_content_attrs
// Inputs: node, attribute accumulator.
// Output: side-effect; injects content-specific data-* attributes the
// parser uses to reconstruct the content variant for non-Text types.
fn add_content_attrs(node: &ElementNode, attrs: &mut BTreeMap<String, String>) {
    use crate::deck::element::ShapeGeometry as SG;
    match &node.content {
        ElementContent::Image(a) | ElementContent::Media(a) => {
            attrs.insert("data-asset-id".into(), a.asset_id.clone());
        }
        ElementContent::Shape(g) => match g {
            SG::Rectangle => {
                attrs.insert("data-shape".into(), "rectangle".into());
            }
            SG::Ellipse => {
                attrs.insert("data-shape".into(), "ellipse".into());
            }
            SG::RoundedRect { radius_px } => {
                attrs.insert("data-shape".into(), "rounded-rect".into());
                attrs.insert("data-shape-radius".into(), format!("{}", radius_px));
            }
            SG::Path { d } => {
                attrs.insert("data-shape".into(), "path".into());
                attrs.insert("data-shape-d".into(), d.clone());
            }
        },
        _ => {}
    }
    if let ElementStyle::Group(gs) = &node.style {
        attrs.insert("data-flex-dir".into(), group_dir_token(gs.direction).into());
        attrs.insert("data-flex-dist".into(), group_dist_token(gs.distribution).into());
        attrs.insert("data-flex-align".into(), group_align_token(gs.alignment).into());
    }
}

fn group_dir_token(d: crate::deck::style::GroupDirection) -> &'static str {
    use crate::deck::style::GroupDirection::*;
    match d { Row => "row", Column => "column" }
}
fn group_dist_token(d: crate::deck::style::GroupDistribution) -> &'static str {
    use crate::deck::style::GroupDistribution::*;
    match d {
        None => "none", Start => "start", Center => "center", End => "end",
        SpaceBetween => "space-between", SpaceAround => "space-around", SpaceEvenly => "space-evenly",
    }
}
fn group_align_token(a: crate::deck::style::GroupAlignment) -> &'static str {
    use crate::deck::style::GroupAlignment::*;
    match a { None => "none", Start => "start", Center => "center", End => "end" }
}

// write_content
// Inputs: node, out buffer.
// Output: side-effect; appends inner content for the variant.
// Text → escaped plain text; Group → recursive children threaded with
// per-child sibling indices for z-index; Embed → raw HTML; other variants
// emit nothing (content sits in data-* attributes).
fn write_content(node: &ElementNode, anim: &AnimMap, out: &mut String) {
    match &node.content {
        ElementContent::Text(rt) => out.push_str(&escape_text(&rt.plain)),
        ElementContent::Group => {
            for (idx, child) in node.children.iter().enumerate() {
                write_node(child, Some(idx as i32), anim, out);
            }
        }
        ElementContent::Embed(html) => out.push_str(html),
        ElementContent::Image(_)
        | ElementContent::Media(_)
        | ElementContent::Shape(_)
        | ElementContent::Table(_) => {}
    }
}

// build_style
// Inputs: node, optional sibling index for the computed z-index.
// Output: CSS declarations for this element as one string.
// Order: geometry first → type-specific style → sibling z-index (when
// present) → inline_styles entries (alphabetic). User-entered CSS lands
// last so it wins under last-declaration CSS rules.
fn build_style(node: &ElementNode, sibling_index: Option<i32>) -> String {
    let mut s: String = String::new();
    write_geom(&node.geometry, &mut s);
    if let ElementStyle::Group(gs) = &node.style {
        if gs.scale != 1.0 {
            // Compose with rotation if present; scale grows from the top-left.
            let rot: String = if node.geometry.rotation != 0.0 {
                format!("rotate({}rad) ", node.geometry.rotation)
            } else {
                String::new()
            };
            decl(&mut s, "transform", &format!("{}scale({})", rot, gs.scale));
            decl(&mut s, "transform-origin", "0 0");
        }
    }
    if let ElementStyle::Text(ts) = &node.style {
        write_text_style(ts, &mut s);
    }
    if let Some(i) = sibling_index {
        decl(&mut s, "z-index", &format!("{i}"));
    }
    for (k, v) in &node.inline_styles {
        decl(&mut s, k, v);
    }
    s
}

fn write_geom(g: &Geometry, out: &mut String) {
    decl(out, "left", &px(g.x));
    decl(out, "top", &px(g.y));
    decl(out, "width", &px(g.width));
    decl(out, "height", &px(g.height));
    if g.rotation != 0.0 {
        decl(out, "transform", &format!("rotate({}rad)", g.rotation));
    }
    if g.opacity != 1.0 {
        decl(out, "opacity", &format!("{}", g.opacity));
    }
    // Note: g.z_order is no longer emitted. The serializer assigns
    // z-index from each element's position in its parent's children
    // vector instead, so z-order stays "app-determined" by tree
    // arrangement rather than by a user-editable field.
}

fn write_text_style(ts: &TextStyle, out: &mut String) {
    decl(out, "font-family", &font_ref_css(&ts.font_family));
    decl(out, "font-size", &length_css(&ts.font_size));
    if ts.font_weight != 400 {
        decl(out, "font-weight", &format!("{}", ts.font_weight));
    }
    if ts.font_style != FontStyle::Normal {
        decl(out, "font-style", ts.font_style.as_css());
    }
    decl(out, "color", &color_ref_css(&ts.color));
    if ts.text_align != TextAlign::Left {
        decl(out, "text-align", ts.text_align.as_css());
    }
    if (ts.line_height - 1.2).abs() > f64::EPSILON {
        decl(out, "line-height", &format!("{}", ts.line_height));
    }
    if ts.letter_spacing.value != 0.0 {
        decl(out, "letter-spacing", &length_css(&ts.letter_spacing));
    }
}

fn decl(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push(':');
    out.push_str(value);
    out.push(';');
}

fn px(v: f64) -> String {
    format!("{}px", v)
}

fn length_css(l: &Length) -> String {
    format!("{}{}", l.value, l.unit.as_css())
}

fn color_ref_css(c: &ColorRef) -> String {
    match c {
        ColorRef::Theme(k) => format!("var(--theme-{})", k.replace('_', "-")),
        ColorRef::Literal(s) => s.clone(),
    }
}

fn font_ref_css(f: &FontRef) -> String {
    match f {
        FontRef::Theme(k) => format!("var(--theme-{})", k.replace('_', "-")),
        FontRef::Literal(s) => s.clone(),
    }
}

// escape_attr
// Inputs: an attribute value string.
// Output: HTML-attribute-safe encoding. Doubles-quoted attributes need
// only `&` and `"` escaped; `<` and `>` are safe inside attribute values
// per HTML5 but we escape them anyway against context confusion.
fn escape_attr(s: &str) -> String {
    let mut out: String = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

// escape_text
// Inputs: a text-content string.
// Output: HTML-text-safe encoding. Escapes `&`, `<`, `>`.
fn escape_text(s: &str) -> String {
    let mut out: String = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::builders::*;
    use crate::deck::element::ShapeGeometry;

    #[test]
    fn slide_background_emitted_on_section_only_when_set() {
        let mut slide = SlideNode::new("s".into(), "title".into(), group_element("rt", vec![]));
        // None → no inline background on the section.
        assert!(!serialize_slide(&slide).contains("background:"));
        slide.metadata.background = Some("#101820".into());
        let html = serialize_slide(&slide);
        assert!(html.contains("style=\"background:#101820\""));
    }

    #[test]
    fn serialize_emits_data_anim_ids_only_for_animated_elements() {
        use crate::deck::animation::{
            AnimationCategory, AnimationEntry, AnimationTiming, AnimationTrigger,
        };
        let root = group_element("rt", vec![text_element("el_a", "x"), text_element("el_b", "y")]);
        let mut slide = SlideNode::new("s".into(), "title".into(), root);
        slide.animations.push(AnimationEntry::new(
            "anim_1".into(), "el_a".into(),
            crate::deck::animation::AnimationEffect::Named("appear".into()),
            AnimationCategory::Entrance, AnimationTrigger::OnClick, AnimationTiming::default(),
        ));
        let html = serialize_slide(&slide);
        assert!(html.contains(r#"data-anim-ids="anim_1""#));
        // el_b has no animation → exactly one tag in the whole slide.
        assert_eq!(html.matches("data-anim-ids").count(), 1);
    }

    #[test]
    fn text_serializes_with_required_attrs() {
        let n = text_element("el_a", "Hello");
        let html = serialize_element(&n);
        assert!(html.contains(r#"data-element-id="el_a""#));
        assert!(html.contains(r#"data-element-type="text""#));
        assert!(html.contains(">Hello<"));
    }

    #[test]
    fn attribute_order_is_alphabetic() {
        let n = text_element("el_a", "x");
        let html = serialize_element(&n);
        let id_pos = html.find("data-element-id").unwrap();
        let type_pos = html.find("data-element-type").unwrap();
        let style_pos = html.find("style=").unwrap();
        assert!(id_pos < type_pos);
        assert!(type_pos < style_pos);
    }

    #[test]
    fn text_content_is_html_escaped() {
        let n = text_element("a", "a & b < c");
        let html = serialize_element(&n);
        assert!(html.contains("a &amp; b &lt; c"));
    }

    #[test]
    fn attribute_value_is_escaped() {
        let mut n = text_element("a", "x");
        n.name = Some(r#"has "quote" and &"#.into());
        let html = serialize_element(&n);
        assert!(html.contains(r#"data-name="has &quot;quote&quot; and &amp;""#));
    }

    #[test]
    fn theme_color_renders_as_var() {
        let mut n = text_element("a", "x");
        if let ElementStyle::Text(ts) = &mut n.style {
            ts.color = ColorRef::Theme("accent".into());
        }
        let html = serialize_element(&n);
        assert!(html.contains("color:var(--theme-accent)"));
    }

    #[test]
    fn theme_font_family_underscores_become_hyphens() {
        let mut n = text_element("a", "x");
        if let ElementStyle::Text(ts) = &mut n.style {
            ts.font_family = FontRef::Theme("title_family".into());
        }
        let html = serialize_element(&n);
        assert!(html.contains("font-family:var(--theme-title-family)"));
    }

    #[test]
    fn literal_color_renders_verbatim() {
        let mut n = text_element("a", "x");
        if let ElementStyle::Text(ts) = &mut n.style {
            ts.color = ColorRef::Literal("#ff0066".into());
        }
        let html = serialize_element(&n);
        assert!(html.contains("color:#ff0066"));
    }

    #[test]
    fn default_text_align_is_omitted() {
        let n = text_element("a", "x");
        let html = serialize_element(&n);
        assert!(!html.contains("text-align"));
    }

    #[test]
    fn non_default_text_align_emitted() {
        let mut n = text_element("a", "x");
        if let ElementStyle::Text(ts) = &mut n.style {
            ts.text_align = TextAlign::Center;
        }
        let html = serialize_element(&n);
        assert!(html.contains("text-align:center"));
    }

    #[test]
    fn geometry_emits_left_top_width_height() {
        let mut n = text_element("a", "x");
        n.geometry = Geometry {
            x: 10.0, y: 20.0, width: 100.0, height: 50.0, ..Default::default()
        };
        let html = serialize_element(&n);
        assert!(html.contains("left:10px"));
        assert!(html.contains("top:20px"));
        assert!(html.contains("width:100px"));
        assert!(html.contains("height:50px"));
    }

    #[test]
    fn geometry_rotation_only_when_nonzero() {
        let n = text_element("a", "x");
        let html = serialize_element(&n);
        assert!(!html.contains("transform"));
    }

    #[test]
    fn geometry_opacity_only_when_not_one() {
        let n = text_element("a", "x");
        let html = serialize_element(&n);
        assert!(!html.contains("opacity"));
    }

    #[test]
    fn image_emits_asset_id_attribute() {
        let n = image_element("img_a", "asset_42");
        let html = serialize_element(&n);
        assert!(html.contains(r#"data-element-type="image""#));
        assert!(html.contains(r#"data-asset-id="asset_42""#));
    }

    #[test]
    fn shape_emits_shape_kind_attribute() {
        let n = shape_element("sh_a", ShapeGeometry::Ellipse);
        let html = serialize_element(&n);
        assert!(html.contains(r#"data-shape="ellipse""#));
    }

    #[test]
    fn shape_rounded_rect_emits_radius() {
        let n = shape_element("sh_a", ShapeGeometry::RoundedRect { radius_px: 8 });
        let html = serialize_element(&n);
        assert!(html.contains(r#"data-shape="rounded-rect""#));
        assert!(html.contains(r#"data-shape-radius="8""#));
    }

    #[test]
    fn group_recursively_serializes_children() {
        let children = vec![text_element("c1", "First"), text_element("c2", "Second")];
        let g = group_element("g", children);
        let html = serialize_element(&g);
        assert!(html.contains(r#"data-element-id="g""#));
        assert!(html.contains(r#"data-element-id="c1""#));
        assert!(html.contains(r#"data-element-id="c2""#));
        assert!(html.find("First").unwrap() < html.find("Second").unwrap());
    }

    #[test]
    fn embed_writes_raw_html_unmodified() {
        let n = embed_element("em", "<b>raw &amp; held</b>");
        let html = serialize_element(&n);
        assert!(html.contains("<b>raw &amp; held</b>"));
    }

    #[test]
    fn slide_wraps_children_in_section_and_content_div() {
        use crate::deck::slide::SlideNode;
        let root = group_element("rt", vec![text_element("c1", "x")]);
        let slide = SlideNode::new("sx".into(), "title".into(), root);
        let html = serialize_slide(&slide);
        assert!(html.contains(r#"<section class="slide""#));
        assert!(html.contains(r#"data-slide-id="sx""#));
        assert!(html.contains(r#"data-layout="title""#));
        assert!(html.contains(r#"data-root-id="rt""#));
        assert!(html.contains(r#"<div class="slide__content""#));
        assert!(html.contains(r#"data-element-id="c1""#));
    }

    #[test]
    fn serialize_panics_on_inconsistent_element() {
        let mut n = text_element("a", "x");
        n.element_type = crate::deck::element::ElementType::Image;
        let result = std::panic::catch_unwind(|| serialize_element(&n));
        assert!(result.is_err());
    }

    // ---------- Stage 8 ----------

    #[test]
    fn inline_styles_appear_after_typed_properties() {
        let mut n = text_element("a", "x");
        n.inline_styles.insert("background-color".into(), "#ff0066".into());
        n.inline_styles.insert("border".into(), "2px solid #000".into());
        let html = serialize_element(&n);
        let style_start = html.find("style=\"").expect("style attr present") + 7;
        let style_end = html[style_start..].find('"').expect("end quote") + style_start;
        let style = &html[style_start..style_end];
        // The typed `color` comes before any inline_styles entry.
        let color_pos = style.find("color:").expect("typed color present");
        let bg_pos = style.find("background-color:").expect("inline bg present");
        let border_pos = style.find("border:").expect("inline border present");
        assert!(color_pos < bg_pos);
        assert!(color_pos < border_pos);
    }

    #[test]
    fn slide_emits_per_child_z_index() {
        use crate::deck::slide::SlideNode;
        let root = group_element(
            "rt",
            vec![
                text_element("c0", "0"),
                text_element("c1", "1"),
                text_element("c2", "2"),
            ],
        );
        let slide = SlideNode::new("s".into(), "title".into(), root);
        let html = serialize_slide(&slide);
        assert!(html.contains("z-index:0"));
        assert!(html.contains("z-index:1"));
        assert!(html.contains("z-index:2"));
    }

    #[test]
    fn group_children_get_per_child_z_index() {
        let group = group_element(
            "g",
            vec![text_element("c0", "a"), text_element("c1", "b")],
        );
        let html = serialize_element(&group);
        // The group itself is the top-level element → no z-index.
        // Its children carry z-index 0 and 1.
        assert!(html.contains("z-index:0"));
        assert!(html.contains("z-index:1"));
    }

    #[test]
    fn z_order_field_is_no_longer_emitted_as_css() {
        let mut n = text_element("a", "x");
        n.geometry.z_order = 999;
        // Standalone serialization: no parent context → no z-index.
        let html = serialize_element(&n);
        assert!(!html.contains("z-index:999"));
    }

    #[test]
    fn empty_inline_styles_emit_nothing_extra() {
        let n = text_element("a", "x");
        assert!(n.inline_styles.is_empty());
        let html = serialize_element(&n);
        assert!(!html.contains("background-color"));
    }
}

#[cfg(test)]
mod group_render_tests {
    use super::*;
    use crate::deck::builders::group_element;
    use crate::deck::element::ElementStyle;
    use crate::deck::style::{GroupAlignment, GroupDirection, GroupDistribution, GroupStyle};

    #[test]
    fn group_emits_flex_attrs_and_scale_transform() {
        let mut g = group_element("g", vec![]);
        g.geometry.width = 100.0;
        g.geometry.height = 50.0;
        g.style = ElementStyle::Group(GroupStyle {
            direction: GroupDirection::Column,
            distribution: GroupDistribution::SpaceBetween,
            alignment: GroupAlignment::Center,
            scale: 2.0,
        });
        let html = serialize_element(&g);
        assert!(html.contains("data-flex-dir=\"column\""), "{html}");
        assert!(html.contains("data-flex-dist=\"space-between\""), "{html}");
        assert!(html.contains("data-flex-align=\"center\""), "{html}");
        assert!(html.contains("scale(2)"), "{html}");
        assert!(html.contains("transform-origin:0 0"), "{html}");
    }
}
