#![allow(dead_code)]

use crate::deck::element::{ElementContent, ElementNode, ElementStyle};
use crate::deck::slide::SlideNode;
use crate::deck::style::*;
use std::collections::BTreeMap;

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
[data-element-type="table"] > table { width: 100%; height: 100%; border-collapse: collapse; table-layout: fixed; }
[data-element-type="table"] th, [data-element-type="table"] td { border: 1px solid var(--theme-muted, #bbb); padding: 6px 10px; text-align: left; vertical-align: top; color: inherit; font: inherit; overflow: hidden; }
[data-element-type="table"] th { font-weight: 600; background: color-mix(in srgb, var(--theme-foreground, #000) 8%, transparent); }
.slide { isolation: isolate; }
:where(.slide) { background: #fff; }
[data-placeholder="true"] { opacity: 0.45; }
"#;

type AnimMap<'a> = BTreeMap<&'a str, Vec<&'a str>>;

pub struct RenderCtx {
    pub number: usize,
    pub count: usize,
    pub date: String,
}

#[derive(Default)]
pub struct RenderOpts {
    pub ctx: Option<RenderCtx>,
    pub hide_placeholders: bool,

    pub min_element_size: f64,
}

fn below_min_size(node: &ElementNode, opts: &RenderOpts) -> bool {
    opts.min_element_size > 0.0
        && node.geometry.width.max(node.geometry.height) < opts.min_element_size
}

pub fn resolve_tokens(raw: &str, ctx: &RenderCtx) -> String {
    assert!(
        raw.len() < usize::MAX,
        "resolve_tokens: absurd input length"
    );
    let bytes: &[u8] = raw.as_bytes();
    let mut out: String = String::with_capacity(raw.len());
    let mut i: usize = 0;
    let n: usize = bytes.len();
    while i < n {
        if bytes[i] == b'$' && i + 1 < n && bytes[i + 1] == b'{' {
            match raw[i + 2..].find('}') {
                Some(rel) => {
                    let name: &str = &raw[i + 2..i + 2 + rel];
                    let value: Option<String> = match name {
                        "slideNumber" => Some(ctx.number.to_string()),
                        "slideCount" => Some(ctx.count.to_string()),
                        "date" => Some(ctx.date.clone()),
                        _ => None,
                    };
                    match value {
                        Some(v) => {
                            out.push_str(&v);
                            i = i + 2 + rel + 1;
                        }
                        None => {
                            out.push_str(&raw[i..i + 2 + rel + 1]);
                            i = i + 2 + rel + 1;
                        }
                    }
                }
                None => {
                    out.push_str(&raw[i..]);
                    i = n;
                }
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z: i64 = days + 719_468;
    let era: i64 = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe: i64 = z - era * 146_097;
    let yoe: i64 = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y: i64 = yoe + era * 400;
    let doy: i64 = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp: i64 = (5 * doy + 2) / 153;
    let d: u32 = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m: u32 = if mp < 10 {
        (mp + 3) as u32
    } else {
        (mp - 9) as u32
    };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn today_ymd() -> String {
    let secs: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d): (i64, u32, u32) = civil_from_days((secs / 86_400) as i64);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

pub fn serialize_element(node: &ElementNode) -> String {
    assert!(
        node.is_consistent(),
        "cannot serialize inconsistent element"
    );
    let mut out: String = String::new();

    let anim: AnimMap = AnimMap::new();
    write_node(node, None, &anim, &RenderOpts::default(), &mut out);
    out
}

pub fn serialize_slide(slide: &SlideNode) -> String {
    serialize_slide_themed(slide, None, None, &RenderOpts::default())
}

pub fn serialize_slide_themed(
    slide: &SlideNode,
    fb_fill: Option<&str>,
    fb_img: Option<&str>,
    opts: &RenderOpts,
) -> String {
    assert!(
        slide.root.is_consistent(),
        "slide root must satisfy the element-triple invariant"
    );

    let mut anim: AnimMap = AnimMap::new();
    for e in &slide.animations {
        anim.entry(e.element_id.as_str())
            .or_default()
            .push(e.id.as_str());
    }
    let mut out: String = String::new();
    out.push_str("<section class=\"slide\" data-slide-id=\"");
    out.push_str(&escape_attr(&slide.id));
    out.push_str("\" data-layout=\"");
    out.push_str(&escape_attr(&slide.layout_id));
    out.push_str("\" data-root-id=\"");
    out.push_str(&escape_attr(&slide.root.id));
    out.push('"');

    let bg_fill: Option<&str> = slide
        .metadata
        .background
        .as_deref()
        .filter(|s| !s.is_empty())
        .or_else(|| fb_fill.filter(|s| !s.is_empty()));
    let bg_img: Option<&str> = slide
        .metadata
        .background_image
        .as_deref()
        .filter(|s| !s.is_empty())
        .or_else(|| fb_img.filter(|s| !s.is_empty()));
    if bg_fill.is_some() || bg_img.is_some() {
        out.push_str(" style=\"");
        if let Some(fill) = bg_fill {
            out.push_str("background:");
            out.push_str(&escape_attr(fill));
            out.push(';');
        }
        if let Some(img) = bg_img {
            out.push_str("background-image:");
            out.push_str(&escape_attr(img));
            out.push_str(
                ";background-size:cover;background-position:center;background-repeat:no-repeat;",
            );
        }
        out.push('"');
    }
    out.push_str("><div class=\"slide__content\">");
    for (idx, child) in slide.root.children.iter().enumerate() {
        if opts.hide_placeholders && child.placeholder {
            continue;
        }
        if below_min_size(child, opts) {
            continue;
        }
        write_node(child, Some(idx as i32), &anim, opts, &mut out);
    }
    out.push_str("</div></section>");
    out
}

fn write_node(
    node: &ElementNode,
    sibling_index: Option<i32>,
    anim: &AnimMap,
    opts: &RenderOpts,
    out: &mut String,
) {
    out.push_str("<div");
    write_attributes(node, sibling_index, anim, opts, out);
    out.push('>');
    write_content(node, anim, opts, out);
    out.push_str("</div>");
}

fn write_attributes(
    node: &ElementNode,
    sibling_index: Option<i32>,
    anim: &AnimMap,
    opts: &RenderOpts,
    out: &mut String,
) {
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
    let token_raw: Option<&str> = match (&node.content, &opts.ctx) {
        (ElementContent::Text(rt), Some(_)) if rt.plain.contains("${") => Some(rt.plain.as_str()),
        _ => None,
    };
    if let Some(raw) = token_raw {
        out.push_str(" data-src=\"");
        out.push_str(&escape_attr(raw));
        out.push('"');
    }

    if node.placeholder {
        out.push_str(" data-placeholder=\"true\"");
    }
}

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
        attrs.insert(
            "data-flex-dist".into(),
            group_dist_token(gs.distribution).into(),
        );
        attrs.insert(
            "data-flex-align".into(),
            group_align_token(gs.alignment).into(),
        );
        attrs.insert("data-flex-scale".into(), format!("{}", gs.scale));
    }
}

fn group_dir_token(d: crate::deck::style::GroupDirection) -> &'static str {
    use crate::deck::style::GroupDirection::*;
    match d {
        Row => "row",
        Column => "column",
    }
}
fn group_dist_token(d: crate::deck::style::GroupDistribution) -> &'static str {
    use crate::deck::style::GroupDistribution::*;
    match d {
        None => "none",
        Start => "start",
        Center => "center",
        End => "end",
        SpaceBetween => "space-between",
        SpaceAround => "space-around",
        SpaceEvenly => "space-evenly",
    }
}
fn group_align_token(a: crate::deck::style::GroupAlignment) -> &'static str {
    use crate::deck::style::GroupAlignment::*;
    match a {
        None => "none",
        Start => "start",
        Center => "center",
        End => "end",
    }
}

fn write_content(node: &ElementNode, anim: &AnimMap, opts: &RenderOpts, out: &mut String) {
    match &node.content {
        ElementContent::Text(rt) => match &opts.ctx {
            Some(c) => out.push_str(&escape_text(&resolve_tokens(&rt.plain, c))),
            None => out.push_str(&escape_text(&rt.plain)),
        },
        ElementContent::Group => {
            for (idx, child) in node.children.iter().enumerate() {
                if opts.hide_placeholders && child.placeholder {
                    continue;
                }
                if below_min_size(child, opts) {
                    continue;
                }
                write_node(child, Some(idx as i32), anim, opts, out);
            }
        }
        ElementContent::Embed(html) => out.push_str(html),
        ElementContent::Table(td) => write_table(td, opts, out),
        ElementContent::Image(_) | ElementContent::Media(_) | ElementContent::Shape(_) => {}
    }
}

fn write_table(td: &crate::deck::element::TableData, opts: &RenderOpts, out: &mut String) {
    out.push_str("<table data-rows=\"");
    out.push_str(&td.rows.to_string());
    out.push_str("\" data-columns=\"");
    out.push_str(&td.columns.to_string());
    out.push_str("\" data-header-rows=\"");
    out.push_str(&td.header_rows.to_string());
    out.push_str("\" data-header-columns=\"");
    out.push_str(&td.header_columns.to_string());
    out.push_str("\">");
    for r in 0..td.rows {
        out.push_str("<tr>");
        for c in 0..td.columns {
            let is_header: bool = r < td.header_rows || c < td.header_columns;
            let tag: &str = if is_header { "th" } else { "td" };
            out.push('<');
            out.push_str(tag);
            if let Some(cell) = td.cells.get(r).and_then(|row| row.get(c)) {
                if !cell.style_overrides.is_empty() {
                    out.push_str(" style=\"");
                    for (k, v) in &cell.style_overrides {
                        out.push_str(k);
                        out.push(':');
                        out.push_str(&escape_attr(v));
                        out.push(';');
                    }
                    out.push('"');
                }
                out.push('>');
                match &opts.ctx {
                    Some(c) => out.push_str(&escape_text(&resolve_tokens(&cell.content.plain, c))),
                    None => out.push_str(&escape_text(&cell.content.plain)),
                }
            } else {
                out.push('>');
            }
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
        out.push_str("</tr>");
    }
    out.push_str("</table>");
}

fn build_style(node: &ElementNode, sibling_index: Option<i32>) -> String {
    let mut s: String = String::new();
    write_geom(&node.geometry, &mut s);
    if let ElementStyle::Group(gs) = &node.style
        && gs.scale != 1.0
    {
        let rot: String = if node.geometry.rotation != 0.0 {
            format!("rotate({}rad) ", node.geometry.rotation)
        } else {
            String::new()
        };
        decl(&mut s, "transform", &format!("{}scale({})", rot, gs.scale));
        decl(&mut s, "transform-origin", "0 0");
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

        assert!(!serialize_slide(&slide).contains("background:"));
        slide.metadata.background = Some("#101820".into());
        let html = serialize_slide(&slide);
        assert!(html.contains("style=\"background:#101820;\""));
    }

    #[test]
    fn serialize_emits_data_anim_ids_only_for_animated_elements() {
        use crate::deck::animation::{
            AnimationCategory, AnimationEntry, AnimationTiming, AnimationTrigger,
        };
        let root = group_element(
            "rt",
            vec![text_element("el_a", "x"), text_element("el_b", "y")],
        );
        let mut slide = SlideNode::new("s".into(), "title".into(), root);
        slide.animations.push(AnimationEntry::new(
            "anim_1".into(),
            "el_a".into(),
            crate::deck::animation::AnimationEffect::Named("appear".into()),
            AnimationCategory::Entrance,
            AnimationTrigger::OnClick,
            AnimationTiming::default(),
        ));
        let html = serialize_slide(&slide);
        assert!(html.contains(r#"data-anim-ids="anim_1""#));

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
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 50.0,
            ..Default::default()
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
    fn base_css_isolates_slide_for_consistent_blend_modes() {
        assert!(ANIMATION_KEYFRAMES_CSS.contains(".slide { isolation: isolate; }"));
    }

    #[test]
    fn base_css_gives_slides_a_white_floor_overridable_by_theme() {
        assert!(ANIMATION_KEYFRAMES_CSS.contains(":where(.slide) { background: #fff; }"));
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

    fn sized(id: &str, w: f64, h: f64) -> ElementNode {
        let mut n = text_element(id, id);
        n.geometry = Geometry {
            width: w,
            height: h,
            ..Default::default()
        };
        n
    }

    #[test]
    fn min_element_size_drops_small_keeps_large() {
        use crate::deck::slide::SlideNode;
        let root = group_element(
            "rt",
            vec![sized("big", 200.0, 200.0), sized("tiny", 10.0, 10.0)],
        );
        let slide = SlideNode::new("s".into(), "title".into(), root);
        let opts = RenderOpts {
            min_element_size: 40.0,
            ..Default::default()
        };
        let html = serialize_slide_themed(&slide, None, None, &opts);
        assert!(
            html.contains(r#"data-element-id="big""#),
            "large element kept"
        );
        assert!(
            !html.contains(r#"data-element-id="tiny""#),
            "small element dropped"
        );

        let all = serialize_slide(&slide);
        assert!(
            all.contains(r#"data-element-id="tiny""#),
            "floor off keeps small"
        );
    }

    #[test]
    fn serialize_panics_on_inconsistent_element() {
        let mut n = text_element("a", "x");
        n.element_type = crate::deck::element::ElementType::Image;
        let result = std::panic::catch_unwind(|| serialize_element(&n));
        assert!(result.is_err());
    }

    #[test]
    fn inline_styles_appear_after_typed_properties() {
        let mut n = text_element("a", "x");
        n.inline_styles
            .insert("background-color".into(), "#ff0066".into());
        n.inline_styles
            .insert("border".into(), "2px solid #000".into());
        let html = serialize_element(&n);
        let style_start = html.find("style=\"").expect("style attr present") + 7;
        let style_end = html[style_start..].find('"').expect("end quote") + style_start;
        let style = &html[style_start..style_end];

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
        let group = group_element("g", vec![text_element("c0", "a"), text_element("c1", "b")]);
        let html = serialize_element(&group);

        assert!(html.contains("z-index:0"));
        assert!(html.contains("z-index:1"));
    }

    #[test]
    fn z_order_field_is_no_longer_emitted_as_css() {
        let mut n = text_element("a", "x");
        n.geometry.z_order = 999;

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

    #[test]
    fn resolve_tokens_substitutes_known_vars() {
        let ctx = RenderCtx {
            number: 3,
            count: 12,
            date: "2026-07-08".into(),
        };
        assert_eq!(
            resolve_tokens("Slide ${slideNumber} of ${slideCount}", &ctx),
            "Slide 3 of 12"
        );
        assert_eq!(resolve_tokens("${date}", &ctx), "2026-07-08");
    }

    #[test]
    fn resolve_tokens_leaves_unknown_and_unclosed_literal() {
        let ctx = RenderCtx {
            number: 1,
            count: 1,
            date: "2026-07-08".into(),
        };
        assert_eq!(resolve_tokens("${foo}", &ctx), "${foo}");
        assert_eq!(resolve_tokens("a ${date", &ctx), "a ${date");
        assert_eq!(resolve_tokens("plain", &ctx), "plain");
    }

    #[test]
    fn civil_from_days_matches_known_epochs() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(59), (1970, 3, 1));
        assert_eq!(civil_from_days(20_642), (2026, 7, 8));
    }

    #[test]
    fn serialize_with_ctx_substitutes_and_emits_data_src() {
        use crate::deck::builders::{group_element, text_element};
        let child = text_element("t1", "Slide ${slideNumber}");
        let root = group_element("root", vec![child]);
        let slide = SlideNode::new("s1".into(), "blank".into(), root);
        let opts = RenderOpts {
            ctx: Some(RenderCtx {
                number: 4,
                count: 9,
                date: "2026-07-08".into(),
            }),
            hide_placeholders: false,
            min_element_size: 0.0,
        };
        let html = serialize_slide_themed(&slide, None, None, &opts);
        assert!(html.contains(">Slide 4<"), "value shown: {html}");
        assert!(
            html.contains("data-src=\"Slide ${slideNumber}\""),
            "raw carried: {html}"
        );
    }

    #[test]
    fn serialize_without_ctx_leaves_raw_and_no_data_src() {
        use crate::deck::builders::{group_element, text_element};
        let child = text_element("t1", "Slide ${slideNumber}");
        let root = group_element("root", vec![child]);
        let slide = SlideNode::new("s1".into(), "blank".into(), root);
        let html = serialize_slide_themed(&slide, None, None, &RenderOpts::default());
        assert!(html.contains(">Slide ${slideNumber}<"), "raw shown: {html}");
        assert!(!html.contains("data-src"), "no data-src: {html}");
    }

    #[test]
    fn placeholder_hidden_in_playback_shown_in_editor() {
        use crate::deck::builders::{group_element, text_element};
        let mut ph = text_element("layout_text_title", "Title");
        ph.placeholder = true;
        let root = group_element("root", vec![ph]);
        let slide = SlideNode::new("s1".into(), "title".into(), root);
        let editor = serialize_slide_themed(&slide, None, None, &RenderOpts::default());
        assert!(
            editor.contains("data-placeholder=\"true\""),
            "editor shows: {editor}"
        );
        assert!(editor.contains(">Title<"));
        let playback = serialize_slide_themed(
            &slide,
            None,
            None,
            &RenderOpts {
                ctx: None,
                hide_placeholders: true,
                min_element_size: 0.0,
            },
        );
        assert!(
            !playback.contains("layout_text_title"),
            "playback hides: {playback}"
        );
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
