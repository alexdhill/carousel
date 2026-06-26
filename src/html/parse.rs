// HTML parser.

// Stage 3 scaffolding: parse_* is exercised in tests but the binary path
// only serializes today. Stage 4 commands will call parse_element on
// inbound interactions.
#![allow(dead_code)]

//
// Uses `kuchikiki` (an html5ever-backed DOM) to parse fragments produced by
// the serializer. The round-trip contract is:
//   parse_element(serialize_element(node)) == node
// for any consistent ElementNode whose content variant is supported by the
// serializer/parser pair. Stage 3 supports Text, Image, Shape, Group, and
// Embed in full. Table and Media round-trip only their data-* attributes
// (the inner table grid is a Stage 5+ concern).
//
// kuchikiki normalizes whitespace per HTML5; we emit compact HTML so no
// whitespace text nodes appear between siblings inside a Group.

use crate::deck::element::*;
use crate::deck::ids::new_element_id;
use crate::deck::slide::SlideNode;
use crate::deck::style::*;
use kuchikiki::NodeRef;
use kuchikiki::traits::*;
use regex::Regex;
use std::collections::BTreeMap;
use std::sync::OnceLock;

#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum ParseError {
    #[error("missing slide root <section class=\"slide\">")]
    MissingSlideRoot,
    #[error("missing data-slide-id on slide root")]
    MissingSlideId,
    #[error("missing <div class=\"slide__content\">")]
    MissingSlideContent,
    #[error("no element found in fragment")]
    NoElement,
    #[error("missing data-element-id on element")]
    MissingElementId,
    #[error("missing data-element-type on element")]
    MissingElementType,
    #[error("unknown data-element-type: {0}")]
    UnknownElementType(String),
    #[error("html serialization error")]
    Serialization,
}

// parse_element
// Inputs: an HTML fragment containing a single top-level element with
// `data-element-id` and `data-element-type` attributes.
// Output: the reconstructed ElementNode.
// Errors: missing required attributes, unknown element type, or
// internal serialization failure when reading Embed inner HTML.
pub fn parse_element(html: &str) -> Result<ElementNode, ParseError> {
    assert!(!html.is_empty(), "parse_element received empty input");
    let doc: NodeRef = kuchikiki::parse_html().one(html);
    let first: NodeRef = find_first_payload_element(&doc).ok_or(ParseError::NoElement)?;
    parse_node(&first)
}

// parse_slide_fragment
// Inputs: an HTML fragment whose root is <section class="slide" …>.
// Output: a SlideNode with `root` set to a Group containing the parsed
// children of <div class="slide__content">.
// Errors: missing section, missing slide id, missing content div, or any
// per-element parse error.
pub fn parse_slide_fragment(html: &str) -> Result<SlideNode, ParseError> {
    assert!(
        !html.is_empty(),
        "parse_slide_fragment received empty input"
    );
    let doc: NodeRef = kuchikiki::parse_html().one(html);
    let section_ref = doc
        .select_first("section.slide")
        .map_err(|_| ParseError::MissingSlideRoot)?;
    let section_node: &NodeRef = section_ref.as_node();
    let section_ed = section_node
        .as_element()
        .ok_or(ParseError::MissingSlideRoot)?;
    let section_attrs = section_ed.attributes.borrow();
    let slide_id: String = section_attrs
        .get("data-slide-id")
        .ok_or(ParseError::MissingSlideId)?
        .to_string();
    let layout_id: String = section_attrs
        .get("data-layout")
        .unwrap_or("default")
        .to_string();
    let root_id: String = section_attrs
        .get("data-root-id")
        .map(str::to_string)
        .unwrap_or_else(new_element_id);
    drop(section_attrs);

    let content_ref = section_node
        .select_first("div.slide__content")
        .map_err(|_| ParseError::MissingSlideContent)?;
    let children: Vec<ElementNode> = parse_element_children(content_ref.as_node())?;

    let root: ElementNode = ElementNode {
        id: root_id,
        element_type: ElementType::Group,
        geometry: Geometry::default(),
        style: ElementStyle::Group(crate::deck::style::GroupStyle::default()),
        content: ElementContent::Group,
        children,
        placeholder_fill: None,
        name: None,
        link: None,
        attributes: BTreeMap::new(),
        inline_styles: BTreeMap::new(),
    };
    Ok(SlideNode::new(slide_id, layout_id, root))
}

// find_first_payload_element
// Inputs: any NodeRef.
// Output: the first descendant element node carrying `data-element-id`.
// Dataflow: pre-order DFS via kuchikiki's descendants iterator.
fn find_first_payload_element(root: &NodeRef) -> Option<NodeRef> {
    for desc in root.descendants() {
        if let Some(ed) = desc.as_element() {
            let attrs = ed.attributes.borrow();
            if attrs.get("data-element-id").is_some() {
                return Some(desc.clone());
            }
        }
    }
    None
}

// parse_node
// Inputs: a NodeRef pointing to a DOM element.
// Output: the ElementNode it encodes.
// Dataflow: read attributes -> resolve element_type -> dispatch to a
// per-type content/style parser -> assemble ElementNode.
fn parse_node(node: &NodeRef) -> Result<ElementNode, ParseError> {
    let ed = node.as_element().ok_or(ParseError::NoElement)?;
    let attrs = ed.attributes.borrow();

    let id: String = attrs
        .get("data-element-id")
        .ok_or(ParseError::MissingElementId)?
        .to_string();
    let type_token: String = attrs
        .get("data-element-type")
        .ok_or(ParseError::MissingElementType)?
        .to_string();
    let element_type: ElementType = ElementType::from_html(&type_token)
        .ok_or_else(|| ParseError::UnknownElementType(type_token.clone()))?;

    let style_decls: BTreeMap<String, String> = parse_style_decls(attrs.get("style").unwrap_or(""));
    let geometry: Geometry = parse_geometry(&style_decls);
    let name: Option<String> = attrs.get("data-name").map(str::to_string);
    let link: Option<String> = attrs.get("data-link").map(str::to_string);
    let placeholder_fill: Option<String> = attrs.get("data-placeholder-fill").map(str::to_string);

    let mut custom: BTreeMap<String, String> = BTreeMap::new();
    for (qn, attr) in attrs.map.iter() {
        let key: &str = qn.local.as_ref();
        if !is_known_attr(key) {
            custom.insert(key.to_string(), attr.value.clone());
        }
    }

    let asset_id: String = attrs.get("data-asset-id").unwrap_or("").to_string();
    let shape_kind: Option<String> = attrs.get("data-shape").map(str::to_string);
    let shape_radius: Option<u32> = attrs
        .get("data-shape-radius")
        .and_then(|s| s.parse::<u32>().ok());
    let shape_d: Option<String> = attrs.get("data-shape-d").map(str::to_string);
    drop(attrs);

    let (style, content, children) = parse_typed_payload(
        element_type,
        &style_decls,
        node,
        ParsedTypedInputs {
            asset_id,
            shape_kind,
            shape_radius,
            shape_d,
        },
    )?;

    // Stage 8: leftover CSS declarations (anything the typed style
    // parsers did not consume) land in inline_styles so the inspector's
    // custom-CSS workflow can read and edit them, and so save/load
    // round-trips preserve them.
    let inline_styles: BTreeMap<String, String> = extract_inline_styles(&style_decls, element_type);

    let result = ElementNode {
        id,
        element_type,
        geometry,
        style,
        content,
        children,
        placeholder_fill,
        name,
        link,
        attributes: custom,
        inline_styles,
    };
    assert!(
        result.is_consistent(),
        "parser produced inconsistent triple"
    );
    Ok(result)
}

struct ParsedTypedInputs {
    asset_id: String,
    shape_kind: Option<String>,
    shape_radius: Option<u32>,
    shape_d: Option<String>,
}

// parse_typed_payload
// Inputs: element_type, parsed style declarations, the source node, and the
// data-* fields extracted by parse_node.
// Output: (style, content, children) tuple — the variant trio for the node.
// Dataflow: dispatch on element_type; only Group recurses into children.
fn parse_typed_payload(
    element_type: ElementType,
    style_decls: &BTreeMap<String, String>,
    node: &NodeRef,
    typed: ParsedTypedInputs,
) -> Result<(ElementStyle, ElementContent, Vec<ElementNode>), ParseError> {
    match element_type {
        ElementType::Text => {
            let ts: TextStyle = parse_text_style(style_decls);
            let text: String = extract_text(node);
            Ok((
                ElementStyle::Text(ts),
                ElementContent::Text(RichText::new(text)),
                vec![],
            ))
        }
        ElementType::Image => Ok((
            ElementStyle::Image(ImageStyle::default()),
            ElementContent::Image(AssetRef {
                asset_id: typed.asset_id,
            }),
            vec![],
        )),
        ElementType::Shape => Ok((
            ElementStyle::Shape(ShapeStyle::default()),
            ElementContent::Shape(reconstruct_shape(
                typed.shape_kind.as_deref(),
                typed.shape_radius,
                typed.shape_d,
            )),
            vec![],
        )),
        ElementType::Media => Ok((
            ElementStyle::Media(MediaStyle::default()),
            ElementContent::Media(AssetRef {
                asset_id: typed.asset_id,
            }),
            vec![],
        )),
        ElementType::Table => Ok((
            ElementStyle::Table(TableStyle::default()),
            ElementContent::Table(parse_table(node)),
            vec![],
        )),
        ElementType::Group => {
            let children: Vec<ElementNode> = parse_element_children(node)?;
            let ed = node.as_element().ok_or(ParseError::NoElement)?;
            let a = ed.attributes.borrow();
            let scale: f64 = parse_scale(
                style_decls
                    .get("transform")
                    .map(String::as_str)
                    .unwrap_or(""),
            );
            let gs = crate::deck::style::GroupStyle {
                direction: parse_group_dir(a.get("data-flex-dir")),
                distribution: parse_group_dist(a.get("data-flex-dist")),
                alignment: parse_group_align(a.get("data-flex-align")),
                scale,
            };
            drop(a);
            Ok((ElementStyle::Group(gs), ElementContent::Group, children))
        }
        ElementType::Embed => {
            let html: String = serialize_inner_html(node)?;
            Ok((ElementStyle::Embed, ElementContent::Embed(html), vec![]))
        }
    }
}

// parse_table
// Inputs: the table wrapper element node (data-element-type="table").
// Output: the TableData grid reconstructed from the inner `<table>`. Header
// counts come from the table's data-* attrs; dimensions from data-rows /
// data-columns (falling back to the parsed shape). Each cell's inline style
// becomes its style_overrides and its text node its content. The grid is
// normalized to a rows×columns rectangle so the model invariant holds even if
// the HTML was hand-edited. A missing/empty `<table>` yields an empty grid.
// Control flow: locate <table> -> read header/dim attrs -> select every <tr>
// (html5ever wraps them in <tbody>, so a descendant select is used) -> read
// each row's <td>/<th> direct children -> normalize to a rectangle.
fn parse_table(node: &NodeRef) -> TableData {
    let table_ref = match node.select_first("table") {
        Ok(t) => t,
        Err(_) => return TableData::default(),
    };
    let table_node: &NodeRef = table_ref.as_node();
    let (header_rows, header_columns, attr_rows, attr_columns) = read_table_attrs(table_node);

    let mut cells: Vec<Vec<TableCell>> = Vec::new();
    if let Ok(rows) = table_node.select("tr") {
        const MAX_ROWS: usize = 4096;
        for tr in rows.take(MAX_ROWS) {
            cells.push(parse_table_row(tr.as_node()));
        }
    }

    let rows: usize = attr_rows.unwrap_or(cells.len());
    let columns: usize =
        attr_columns.unwrap_or_else(|| cells.iter().map(Vec::len).max().unwrap_or(0));
    normalize_grid(&mut cells, rows, columns);
    TableData {
        rows,
        columns,
        cells,
        header_rows,
        header_columns,
    }
}

// read_table_attrs — header counts + optional declared dimensions.
fn read_table_attrs(table_node: &NodeRef) -> (usize, usize, Option<usize>, Option<usize>) {
    let ed = match table_node.as_element() {
        Some(e) => e,
        None => return (0, 0, None, None),
    };
    let a = ed.attributes.borrow();
    let get = |k: &str| a.get(k).and_then(|v| v.parse::<usize>().ok());
    (
        get("data-header-rows").unwrap_or(0),
        get("data-header-columns").unwrap_or(0),
        get("data-rows"),
        get("data-columns"),
    )
}

// parse_table_row — the <td>/<th> direct children of one <tr> as cells.
fn parse_table_row(tr_node: &NodeRef) -> Vec<TableCell> {
    let mut row: Vec<TableCell> = Vec::new();
    for child in tr_node.children() {
        let ed = match child.as_element() {
            Some(e) => e,
            None => continue,
        };
        let name: &str = ed.name.local.as_ref();
        if name != "td" && name != "th" {
            continue;
        }
        let style_overrides: BTreeMap<String, String> = {
            let a = ed.attributes.borrow();
            parse_style_decls(a.get("style").unwrap_or(""))
        };
        row.push(TableCell {
            content: RichText::new(extract_text(&child)),
            style_overrides,
            colspan: 1,
            rowspan: 1,
        });
    }
    row
}

// normalize_grid — force `cells` to exactly rows×columns (pad with default
// cells, truncate overflow) so the TableData invariant always holds.
fn normalize_grid(cells: &mut Vec<Vec<TableCell>>, rows: usize, columns: usize) {
    cells.truncate(rows);
    while cells.len() < rows {
        cells.push(Vec::new());
    }
    for row in cells.iter_mut() {
        row.truncate(columns);
        while row.len() < columns {
            row.push(TableCell::default());
        }
    }
}

fn reconstruct_shape(kind: Option<&str>, radius: Option<u32>, d: Option<String>) -> ShapeGeometry {
    match kind {
        Some("ellipse") => ShapeGeometry::Ellipse,
        Some("rounded-rect") => ShapeGeometry::RoundedRect {
            radius_px: radius.unwrap_or(0),
        },
        Some("path") => ShapeGeometry::Path {
            d: d.unwrap_or_default(),
        },
        _ => ShapeGeometry::Rectangle,
    }
}

fn is_known_attr(key: &str) -> bool {
    matches!(
        key,
        "data-element-id"
            | "data-element-type"
            | "data-name"
            | "data-link"
            | "data-placeholder-fill"
            | "style"
            | "data-asset-id"
            | "data-shape"
            | "data-shape-radius"
            | "data-shape-d"
            | "data-flex-dir"
            | "data-flex-dist"
            | "data-flex-align"
            | "data-flex-scale"
            | "class"
            // data-anim-ids is a derived targeting tag emitted by the
            // serializer from the slide timeline; consume (drop) it on read
            // so it never accumulates in element.attributes (the manifest is
            // authoritative for animations, not the HTML).
            | "data-anim-ids"
    )
}

// parse_element_children
// Inputs: a NodeRef whose children may include elements and text nodes.
// Output: parsed ElementNodes for every direct element child; text and
// comment nodes are silently skipped.
fn parse_element_children(parent: &NodeRef) -> Result<Vec<ElementNode>, ParseError> {
    let mut out: Vec<ElementNode> = Vec::new();
    for child in parent.children() {
        if child.as_element().is_some() {
            out.push(parse_node(&child)?);
        }
    }
    Ok(out)
}

fn extract_text(node: &NodeRef) -> String {
    let mut out: String = String::new();
    for child in node.children() {
        if let Some(text) = child.as_text() {
            out.push_str(&text.borrow());
        }
    }
    out
}

fn serialize_inner_html(node: &NodeRef) -> Result<String, ParseError> {
    let mut buf: Vec<u8> = Vec::new();
    for child in node.children() {
        child
            .serialize(&mut buf)
            .map_err(|_| ParseError::Serialization)?;
    }
    String::from_utf8(buf).map_err(|_| ParseError::Serialization)
}

// parse_style_decls
// Inputs: a CSS declaration list ("k:v;k:v;").
// Output: a BTreeMap from property name to value, with both sides trimmed.
// Limitation: splits on `;` and the first `:`. Complex values containing
// `;` (data URLs) or unbalanced parens are not supported; Stage 3 only
// uses simple values.
fn parse_style_decls(s: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for decl in s.split(';') {
        let decl: &str = decl.trim();
        if decl.is_empty() {
            continue;
        }
        if let Some((k, v)) = decl.split_once(':') {
            out.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

fn parse_geometry(map: &BTreeMap<String, String>) -> Geometry {
    Geometry {
        x: parse_px_default(map, "left"),
        y: parse_px_default(map, "top"),
        width: parse_px_default(map, "width"),
        height: parse_px_default(map, "height"),
        rotation: parse_rotation(map.get("transform").map(String::as_str).unwrap_or("")),
        opacity: map
            .get("opacity")
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(1.0),
        // z-index is intentionally NOT round-tripped into z_order. The
        // serializer assigns z-index from sibling position, so the value
        // stored in the manifest carries no authoritative information.
        // Parsed elements always start with z_order = 0.
        z_order: 0,
    }
}

// known_style_keys
// Inputs: the element type.
// Output: the set of CSS property names the typed-style parsers consume
// for elements of that type. Anything in the parsed declarations map that
// is NOT in this set lands in the node's `inline_styles` so the inspector
// can show and edit it (and so save/load round-trips preserve it).
fn known_style_keys(element_type: ElementType) -> &'static [&'static str] {
    const GEOMETRY: &[&str] = &[
        "left",
        "top",
        "width",
        "height",
        "transform",
        "transform-origin",
        "opacity",
        "z-index",
    ];
    const TEXT_EXTRA: &[&str] = &[
        "font-family",
        "font-size",
        "font-weight",
        "font-style",
        "color",
        "text-align",
        "line-height",
        "letter-spacing",
    ];
    match element_type {
        ElementType::Text => {
            // Both arrays are static; allocating a join would force
            // heap usage on every parse. Stage 8 callers always
            // iterate, so a const slice covering both arrays is
            // generated at the call site below instead.
            TEXT_KEYS
        }
        _ => GEOMETRY,
    }
}

// TEXT_KEYS
// Static concatenation of GEOMETRY + TEXT_EXTRA in known_style_keys().
// Kept as a single const so `known_style_keys` can return a &'static
// slice without runtime allocation.
const TEXT_KEYS: &[&str] = &[
    "left",
    "top",
    "width",
    "height",
    "transform",
    "transform-origin",
    "opacity",
    "z-index",
    "font-family",
    "font-size",
    "font-weight",
    "font-style",
    "color",
    "text-align",
    "line-height",
    "letter-spacing",
];

// extract_inline_styles
// Inputs: the parsed declaration map, the element's type.
// Output: a map of declarations the typed parsers did not consume.
// Dataflow: filter style_decls by membership in known_style_keys; copy
// everything else (preserves alphabetic order via BTreeMap).
fn extract_inline_styles(
    style_decls: &BTreeMap<String, String>,
    element_type: ElementType,
) -> BTreeMap<String, String> {
    let known: &[&str] = known_style_keys(element_type);
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in style_decls {
        if !known.contains(&k.as_str()) {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

fn parse_px_default(map: &BTreeMap<String, String>, key: &str) -> f64 {
    map.get(key)
        .and_then(|s| parse_length(s))
        .map(|l| l.value)
        .unwrap_or(0.0)
}

fn parse_length(s: &str) -> Option<Length> {
    let s: &str = s.trim();
    let suffixes: &[(&str, LengthUnit)] = &[
        ("px", LengthUnit::Px),
        ("em", LengthUnit::Em),
        ("pt", LengthUnit::Pt),
        ("%", LengthUnit::Pct),
    ];
    for (suffix, unit) in suffixes {
        if let Some(num) = s.strip_suffix(*suffix) {
            let v: f64 = num.trim().parse::<f64>().ok()?;
            return Some(Length {
                value: v,
                unit: *unit,
            });
        }
    }
    None
}

fn parse_rotation(s: &str) -> f64 {
    let re: &Regex = rotation_regex();
    re.captures(s)
        .and_then(|caps| caps.get(1))
        .and_then(|m| m.as_str().parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn parse_scale(s: &str) -> f64 {
    let re: &Regex = scale_regex();
    re.captures(s)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<f64>().ok())
        .unwrap_or(1.0)
}
fn scale_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"scale\(\s*([0-9.]+)\s*\)").unwrap())
}

fn parse_group_dir(s: Option<&str>) -> crate::deck::style::GroupDirection {
    use crate::deck::style::GroupDirection::*;
    match s {
        Some("column") => Column,
        _ => Row,
    }
}
fn parse_group_dist(s: Option<&str>) -> crate::deck::style::GroupDistribution {
    use crate::deck::style::GroupDistribution::*;
    match s {
        Some("start") => Start,
        Some("center") => Center,
        Some("end") => End,
        Some("space-between") => SpaceBetween,
        Some("space-around") => SpaceAround,
        Some("space-evenly") => SpaceEvenly,
        _ => None,
    }
}
fn parse_group_align(s: Option<&str>) -> crate::deck::style::GroupAlignment {
    use crate::deck::style::GroupAlignment::*;
    match s {
        Some("start") => Start,
        Some("center") => Center,
        Some("end") => End,
        _ => None,
    }
}

fn parse_text_style(map: &BTreeMap<String, String>) -> TextStyle {
    let mut t: TextStyle = TextStyle::default();
    if let Some(s) = map.get("font-family") {
        t.font_family = parse_font_ref(s);
    }
    if let Some(s) = map.get("font-size")
        && let Some(l) = parse_length(s)
    {
        t.font_size = l;
    }
    if let Some(s) = map.get("font-weight")
        && let Ok(v) = s.parse::<u16>()
    {
        t.font_weight = v;
    }
    t.font_style = match map.get("font-style").map(String::as_str) {
        Some("italic") => FontStyle::Italic,
        _ => FontStyle::Normal,
    };
    if let Some(s) = map.get("color") {
        t.color = parse_color_ref(s);
    }
    t.text_align = match map.get("text-align").map(String::as_str) {
        Some("center") => TextAlign::Center,
        Some("right") => TextAlign::Right,
        Some("justify") => TextAlign::Justify,
        _ => TextAlign::Left,
    };
    if let Some(s) = map.get("line-height")
        && let Ok(v) = s.parse::<f64>()
    {
        t.line_height = v;
    }
    if let Some(s) = map.get("letter-spacing")
        && let Some(l) = parse_length(s)
    {
        t.letter_spacing = l;
    }
    t
}

fn parse_color_ref(s: &str) -> ColorRef {
    match extract_theme_var(s) {
        Some(k) => ColorRef::Theme(k),
        None => ColorRef::Literal(s.trim().to_string()),
    }
}

fn parse_font_ref(s: &str) -> FontRef {
    match extract_theme_var(s) {
        Some(k) => FontRef::Theme(k),
        None => FontRef::Literal(s.trim().to_string()),
    }
}

fn extract_theme_var(s: &str) -> Option<String> {
    let re: &Regex = theme_var_regex();
    let caps = re.captures(s.trim())?;
    Some(caps.get(1)?.as_str().replace('-', "_"))
}

fn theme_var_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // The pattern is a compile-time literal; compilation cannot fail.
        #[allow(clippy::unwrap_used)]
        Regex::new(r"^\s*var\(\s*--theme-([a-zA-Z0-9_\-]+)\s*\)\s*$").unwrap()
    })
}

fn rotation_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"rotate\(\s*(-?\d+\.?\d*|-?\.\d+)\s*rad\s*\)").unwrap()
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::builders::*;
    use crate::html::serialize::{serialize_element, serialize_slide};
    use proptest::prelude::*;

    // ---------- unit tests: known-input/known-output ("red-black") ----------

    #[test]
    fn group_roundtrips_flex_props_and_scale() {
        use crate::deck::builders::group_element;
        use crate::deck::element::ElementStyle;
        use crate::deck::style::{GroupAlignment, GroupDirection, GroupDistribution, GroupStyle};
        let mut g = group_element("g", vec![]);
        g.geometry.width = 100.0;
        g.geometry.height = 50.0;
        g.style = ElementStyle::Group(GroupStyle {
            direction: GroupDirection::Column,
            distribution: GroupDistribution::SpaceAround,
            alignment: GroupAlignment::End,
            scale: 1.5,
        });
        let html = crate::html::serialize::serialize_element(&g);
        let back = parse_element(&html).unwrap();
        // The scale transform/origin are model-owned, not free CSS.
        assert!(!back.inline_styles.contains_key("transform-origin"));
        assert!(!back.inline_styles.contains_key("transform"));
        match back.style {
            ElementStyle::Group(gs) => {
                assert_eq!(gs.direction, GroupDirection::Column);
                assert_eq!(gs.distribution, GroupDistribution::SpaceAround);
                assert_eq!(gs.alignment, GroupAlignment::End);
                assert_eq!(gs.scale, 1.5);
            }
            other => panic!("expected group, got {other:?}"),
        }
    }

    #[test]
    fn parser_drops_derived_data_anim_ids() {
        let html = r#"<div data-element-id="el_a" data-element-type="text"
                          data-anim-ids="anim_1 anim_2">hi</div>"#;
        let node = parse_element(html).unwrap();
        // The derived targeting tag must NOT be swept into the catch-all
        // attributes map, or it would accumulate across save/load cycles.
        assert!(!node.attributes.contains_key("data-anim-ids"));
    }

    #[test]
    fn anim_tag_does_not_accumulate_through_html_round_trip() {
        // A slide HTML carrying data-anim-ids, parsed back, has no animation
        // state in the element tree (the manifest is authoritative), so a
        // re-serialization of that parsed slide emits no tag.
        let html = r#"<section class="slide" data-slide-id="s" data-layout="t" data-root-id="rt"><div class="slide__content"><div data-element-id="el_a" data-element-type="text" data-anim-ids="anim_1">hi</div></div></section>"#;
        let slide = parse_slide_fragment(html).unwrap();
        assert!(slide.animations.is_empty());
        let again = serialize_slide(&slide);
        assert_eq!(again.matches("data-anim-ids").count(), 0);
    }

    #[test]
    fn parse_minimal_text_element() {
        let html = r#"<div data-element-id="el_a" data-element-type="text"
                          style="left:10px;top:20px;width:30px;height:40px;
                                 font-family:Inter;font-size:24px;
                                 color:#000;">Hello</div>"#;
        let n = parse_element(html).unwrap();
        assert_eq!(n.id, "el_a");
        assert_eq!(n.element_type, ElementType::Text);
        assert_eq!(n.geometry.x, 10.0);
        assert_eq!(n.geometry.y, 20.0);
        match &n.content {
            ElementContent::Text(rt) => assert_eq!(rt.plain, "Hello"),
            other => panic!("expected Text content, got {other:?}"),
        }
    }

    #[test]
    fn parse_unknown_type_returns_error() {
        let html = r#"<div data-element-id="x" data-element-type="bogus"></div>"#;
        let result = parse_element(html);
        assert!(matches!(result, Err(ParseError::UnknownElementType(_))));
    }

    #[test]
    fn parse_missing_id_returns_error() {
        // No data-element-id anywhere — find_first_payload_element returns
        // None, so this surfaces as NoElement.
        let html = r#"<div data-element-type="text"></div>"#;
        let result = parse_element(html);
        assert!(matches!(result, Err(ParseError::NoElement)));
    }

    #[test]
    fn parse_missing_type_returns_error() {
        let html = r#"<div data-element-id="a">x</div>"#;
        let result = parse_element(html);
        assert!(matches!(result, Err(ParseError::MissingElementType)));
    }

    #[test]
    fn parse_empty_input_panics_assertion() {
        let result = std::panic::catch_unwind(|| parse_element(""));
        assert!(result.is_err());
    }

    #[test]
    fn parse_theme_color_returns_theme_ref() {
        let html = r#"<div data-element-id="a" data-element-type="text"
                          style="font-family:Inter;font-size:24px;
                                 color:var(--theme-accent);">x</div>"#;
        let n = parse_element(html).unwrap();
        match &n.style {
            ElementStyle::Text(ts) => match &ts.color {
                ColorRef::Theme(k) => assert_eq!(k, "accent"),
                other => panic!("expected Theme color, got {other:?}"),
            },
            _ => panic!("expected Text style"),
        }
    }

    #[test]
    fn parse_theme_font_family_converts_hyphens_to_underscores() {
        let html = r#"<div data-element-id="a" data-element-type="text"
                          style="font-family:var(--theme-title-family);
                                 font-size:24px;color:#000;">x</div>"#;
        let n = parse_element(html).unwrap();
        match &n.style {
            ElementStyle::Text(ts) => match &ts.font_family {
                FontRef::Theme(k) => assert_eq!(k, "title_family"),
                _ => panic!("expected Theme font"),
            },
            _ => panic!("expected Text style"),
        }
    }

    #[test]
    fn parse_handles_amp_and_lt_in_text() {
        // Serialized escape sequences -> parsed as the original characters.
        let html = r#"<div data-element-id="a" data-element-type="text"
                          style="font-family:Inter;font-size:24px;color:#000;"
                          >a &amp; b &lt; c</div>"#;
        let n = parse_element(html).unwrap();
        if let ElementContent::Text(rt) = &n.content {
            assert_eq!(rt.plain, "a & b < c");
        } else {
            panic!("expected text content");
        }
    }

    #[test]
    fn parse_image_extracts_asset_id() {
        let html = r#"<div data-element-id="im" data-element-type="image"
                          data-asset-id="asset_xyz"
                          style="left:0px;top:0px;width:0px;height:0px;"></div>"#;
        let n = parse_element(html).unwrap();
        if let ElementContent::Image(a) = &n.content {
            assert_eq!(a.asset_id, "asset_xyz");
        } else {
            panic!("expected Image content");
        }
    }

    #[test]
    fn parse_shape_rounded_rect_with_radius() {
        let html = r#"<div data-element-id="sh" data-element-type="shape"
                          data-shape="rounded-rect" data-shape-radius="12"
                          style="left:0px;top:0px;width:0px;height:0px;"></div>"#;
        let n = parse_element(html).unwrap();
        if let ElementContent::Shape(ShapeGeometry::RoundedRect { radius_px }) = &n.content {
            assert_eq!(*radius_px, 12);
        } else {
            panic!("expected RoundedRect, got {:?}", n.content);
        }
    }

    #[test]
    fn parse_slide_fragment_extracts_metadata_and_children() {
        let root = group_element("rt", vec![text_element("c1", "hello")]);
        let slide = SlideNode::new("s1".into(), "title".into(), root);
        let html = serialize_slide(&slide);
        let back = parse_slide_fragment(&html).unwrap();
        assert_eq!(back.id, "s1");
        assert_eq!(back.layout_id, "title");
        assert_eq!(back.root.id, "rt");
        assert_eq!(back.root.children.len(), 1);
        assert_eq!(back.root.children[0].id, "c1");
    }

    #[test]
    fn parse_slide_fragment_rejects_missing_section() {
        let html = "<div>no slide here</div>";
        let err = parse_slide_fragment(html).unwrap_err();
        assert_eq!(err, ParseError::MissingSlideRoot);
    }

    // ---------- low-level helper unit tests ----------

    #[test]
    fn parse_style_decls_basic_split() {
        let m = parse_style_decls("left:10px;top:20px;color:red");
        assert_eq!(m.get("left").map(String::as_str), Some("10px"));
        assert_eq!(m.get("top").map(String::as_str), Some("20px"));
        assert_eq!(m.get("color").map(String::as_str), Some("red"));
    }

    #[test]
    fn parse_style_decls_handles_extra_spaces() {
        let m = parse_style_decls("  left : 10px ;  top:20px ;");
        assert_eq!(m.get("left").map(String::as_str), Some("10px"));
        assert_eq!(m.get("top").map(String::as_str), Some("20px"));
    }

    #[test]
    fn parse_length_handles_units() {
        assert_eq!(
            parse_length("72px"),
            Some(Length {
                value: 72.0,
                unit: LengthUnit::Px
            })
        );
        assert_eq!(
            parse_length("1.5em"),
            Some(Length {
                value: 1.5,
                unit: LengthUnit::Em
            })
        );
        assert_eq!(
            parse_length("12pt"),
            Some(Length {
                value: 12.0,
                unit: LengthUnit::Pt
            })
        );
        assert_eq!(
            parse_length("50%"),
            Some(Length {
                value: 50.0,
                unit: LengthUnit::Pct
            })
        );
    }

    #[test]
    fn parse_length_rejects_garbage() {
        assert_eq!(parse_length("abc"), None);
        assert_eq!(parse_length(""), None);
        assert_eq!(parse_length("10kg"), None);
    }

    #[test]
    fn parse_rotation_reads_radians() {
        assert!((parse_rotation("rotate(1.5rad)") - 1.5).abs() < f64::EPSILON);
        assert!((parse_rotation("rotate( -0.5 rad )") - -0.5).abs() < f64::EPSILON);
        assert_eq!(parse_rotation("scale(2)"), 0.0);
    }

    #[test]
    fn extract_theme_var_round_trips_underscore_keys() {
        assert_eq!(
            extract_theme_var("var(--theme-accent)"),
            Some("accent".into())
        );
        assert_eq!(
            extract_theme_var("var(--theme-title-family)"),
            Some("title_family".into())
        );
        assert_eq!(extract_theme_var("#ff0066"), None);
        assert_eq!(extract_theme_var("var(--other-thing)"), None);
    }

    // ---------- single-element round-trip ----------

    #[test]
    fn roundtrip_default_text_element() {
        let n = text_element("el_a", "Hello");
        let html = serialize_element(&n);
        let back = parse_element(&html).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn roundtrip_text_with_theme_bindings() {
        let mut n = text_element("el_a", "Title");
        if let ElementStyle::Text(ts) = &mut n.style {
            ts.color = ColorRef::Theme("accent".into());
            ts.font_family = FontRef::Theme("title_family".into());
            ts.font_size = Length::px(96.0);
            ts.font_weight = 700;
            ts.text_align = TextAlign::Center;
        }
        n.geometry = Geometry {
            x: 120.0,
            y: 200.0,
            width: 800.0,
            height: 100.0,
            ..Default::default()
        };
        let html = serialize_element(&n);
        let back = parse_element(&html).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn roundtrip_image_element() {
        let n = image_element("im_a", "asset_xyz");
        let html = serialize_element(&n);
        let back = parse_element(&html).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn roundtrip_image_element_with_background_inline_styles() {
        // Mirrors what build_image_element_from_asset produces: an image
        // element carrying object-fit:cover via background-* shortcuts
        // and a var(--asset-…) reference. Both the AssetRef content and
        // the inline styles must survive a serialize → parse cycle so
        // save / load preserves dropped images.
        let mut n = image_element("im_a", "asset_deadbeef");
        n.inline_styles.insert(
            "background-image".into(),
            "var(--asset-asset_deadbeef)".into(),
        );
        n.inline_styles
            .insert("background-size".into(), "cover".into());
        n.inline_styles
            .insert("background-position".into(), "center".into());
        n.inline_styles
            .insert("background-repeat".into(), "no-repeat".into());
        let html = serialize_element(&n);
        let back = parse_element(&html).unwrap();
        assert_eq!(back, n);
        match back.content {
            crate::deck::element::ElementContent::Image(ref a) => {
                assert_eq!(a.asset_id, "asset_deadbeef");
            }
            ref other => panic!("expected Image content, got {other:?}"),
        }
        assert_eq!(
            back.inline_styles
                .get("background-size")
                .map(String::as_str),
            Some("cover")
        );
    }

    #[test]
    fn roundtrip_shape_variants() {
        for sg in [
            ShapeGeometry::Rectangle,
            ShapeGeometry::Ellipse,
            ShapeGeometry::RoundedRect { radius_px: 4 },
            ShapeGeometry::Path {
                d: "M0 0 L10 10".into(),
            },
        ] {
            let n = shape_element("sh", sg.clone());
            let html = serialize_element(&n);
            let back = parse_element(&html).unwrap();
            assert_eq!(back.content, ElementContent::Shape(sg));
        }
    }

    #[test]
    fn roundtrip_group_with_children() {
        let kids = vec![text_element("c1", "one"), text_element("c2", "two")];
        let g = group_element("g", kids);
        let html = serialize_element(&g);
        let back = parse_element(&html).unwrap();
        assert_eq!(back, g);
    }

    fn sample_cell(text: &str, overrides: &[(&str, &str)]) -> TableCell {
        let mut so: BTreeMap<String, String> = BTreeMap::new();
        for (k, v) in overrides {
            so.insert((*k).into(), (*v).into());
        }
        TableCell {
            content: RichText::new(text),
            style_overrides: so,
            colspan: 1,
            rowspan: 1,
        }
    }

    #[test]
    fn roundtrip_table_with_headers_and_cell_styles() {
        use crate::deck::builders::table_element;
        let cells = vec![
            vec![sample_cell("H1", &[]), sample_cell("H2", &[])],
            vec![
                sample_cell("a", &[("background-color", "#eee"), ("color", "#111")]),
                sample_cell("b", &[]),
            ],
            vec![sample_cell("c", &[]), sample_cell("", &[])],
        ];
        let td = TableData {
            rows: 3,
            columns: 2,
            cells,
            header_rows: 1,
            header_columns: 0,
        };
        let n = table_element("tbl", td.clone());
        let html = serialize_element(&n);
        // Header row emits <th>, body emits <td>.
        assert!(html.contains("<th"));
        assert!(html.contains("<td"));
        let back = parse_element(&html).unwrap();
        assert_eq!(back.content, ElementContent::Table(td));
    }

    #[test]
    fn roundtrip_empty_table_is_rectangular() {
        use crate::deck::builders::table_element;
        let td = TableData {
            rows: 2,
            columns: 3,
            cells: vec![
                vec![
                    TableCell::default(),
                    TableCell::default(),
                    TableCell::default(),
                ],
                vec![
                    TableCell::default(),
                    TableCell::default(),
                    TableCell::default(),
                ],
            ],
            header_rows: 0,
            header_columns: 0,
        };
        // TableCell::default has colspan/rowspan 0; the parser normalizes to 1,
        // so build the expected grid with explicit spans of 1.
        let expected_cells: Vec<Vec<TableCell>> = (0..2)
            .map(|_| (0..3).map(|_| sample_cell("", &[])).collect())
            .collect();
        let expected = TableData {
            cells: expected_cells,
            ..td.clone()
        };
        let n = table_element("t2", td);
        let html = serialize_element(&n);
        let back = parse_element(&html).unwrap();
        assert_eq!(back.content, ElementContent::Table(expected));
    }

    #[test]
    fn roundtrip_slide_fragment() {
        let kids = vec![text_element("c1", "one"), text_element("c2", "two")];
        let root = group_element("rt", kids);
        let slide = SlideNode::new("sx".into(), "title".into(), root);
        let html = serialize_slide(&slide);
        let back = parse_slide_fragment(&html).unwrap();
        assert_eq!(back, slide);
    }

    #[test]
    fn roundtrip_preserves_custom_attributes() {
        let mut n = text_element("a", "x");
        n.attributes
            .insert("data-custom".into(), "value-here".into());
        let html = serialize_element(&n);
        let back = parse_element(&html).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn roundtrip_preserves_inline_styles() {
        let mut n = text_element("a", "x");
        n.inline_styles
            .insert("background-color".into(), "#ff0066".into());
        n.inline_styles
            .insert("border".into(), "2px solid #000".into());
        n.inline_styles.insert("border-radius".into(), "8px".into());
        let html = serialize_element(&n);
        let back = parse_element(&html).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn parse_drops_z_index_from_geometry() {
        // Even when an HTML fragment carries a z-index declaration (e.g.
        // produced by the new slide serializer), parsing it back must
        // not store anything in z_order — the field is no longer
        // round-tripped through CSS.
        let html = r#"<div data-element-id="a" data-element-type="text"
            style="left:10px;top:20px;width:0px;height:0px;z-index:7;
                   font-family:Arial;font-size:14px;color:#000">hi</div>"#;
        let back = parse_element(html).unwrap();
        assert_eq!(back.geometry.z_order, 0);
    }

    #[test]
    fn parse_extracts_unknown_css_into_inline_styles() {
        let html = r#"<div data-element-id="a" data-element-type="text"
            style="left:0px;top:0px;width:0px;height:0px;
                   font-family:Arial;font-size:14px;color:#000;
                   background-color:#fafafa;border-radius:12px">x</div>"#;
        let back = parse_element(html).unwrap();
        assert_eq!(
            back.inline_styles
                .get("background-color")
                .map(String::as_str),
            Some("#fafafa")
        );
        assert_eq!(
            back.inline_styles.get("border-radius").map(String::as_str),
            Some("12px")
        );
        // Typed-style keys do NOT leak into inline_styles.
        assert!(!back.inline_styles.contains_key("color"));
        assert!(!back.inline_styles.contains_key("font-family"));
    }

    #[test]
    fn slide_roundtrip_after_inserting_inline_styles_is_stable() {
        let mut a = text_element("c0", "x");
        a.inline_styles
            .insert("background-color".into(), "#abc".into());
        let mut b = text_element("c1", "y");
        b.inline_styles
            .insert("border".into(), "1px solid #def".into());
        let slide = SlideNode::new("s".into(), "title".into(), group_element("rt", vec![a, b]));
        let html = serialize_slide(&slide);
        let back = parse_slide_fragment(&html).unwrap();
        assert_eq!(back, slide);
    }

    #[test]
    fn roundtrip_preserves_name_and_link() {
        let mut n = text_element("a", "x");
        n.name = Some("Title".into());
        n.link = Some("https://example.org/".into());
        n.placeholder_fill = Some("title".into());
        let html = serialize_element(&n);
        let back = parse_element(&html).unwrap();
        assert_eq!(back, n);
    }

    // ---------- proptest strategies ----------

    fn arb_geometry() -> impl Strategy<Value = Geometry> {
        (-1000i32..1000, -1000i32..1000, 1i32..2000, 1i32..2000).prop_map(|(x, y, w, h)| Geometry {
            x: x as f64,
            y: y as f64,
            width: w as f64,
            height: h as f64,
            rotation: 0.0,
            opacity: 1.0,
            z_order: 0,
        })
    }

    fn arb_color_ref() -> impl Strategy<Value = ColorRef> {
        prop_oneof![
            "[a-z][a-z_]{1,8}".prop_map(ColorRef::Theme),
            prop::sample::select(vec!["#000", "#fff", "#ff0066", "#0066ff", "#abcdef"])
                .prop_map(|s| ColorRef::Literal(s.to_string())),
        ]
    }

    fn arb_font_ref() -> impl Strategy<Value = FontRef> {
        prop_oneof![
            "[a-z][a-z_]{1,8}".prop_map(FontRef::Theme),
            prop::sample::select(vec!["Inter", "Georgia", "Helvetica", "Arial"])
                .prop_map(|s| FontRef::Literal(s.to_string())),
        ]
    }

    fn arb_text_style() -> impl Strategy<Value = TextStyle> {
        (
            arb_font_ref(),
            10i32..200,
            prop::sample::select(vec![300u16, 400, 500, 600, 700, 800]),
            prop_oneof![Just(FontStyle::Normal), Just(FontStyle::Italic)],
            arb_color_ref(),
            prop_oneof![
                Just(TextAlign::Left),
                Just(TextAlign::Center),
                Just(TextAlign::Right),
                Just(TextAlign::Justify),
            ],
        )
            .prop_map(|(ff, fs, fw, fst, c, ta)| TextStyle {
                font_family: ff,
                font_size: Length::px(fs as f64),
                font_weight: fw,
                font_style: fst,
                color: c,
                text_align: ta,
                line_height: 1.2,
                letter_spacing: Length::px(0.0),
            })
    }

    // arb_safe_text: ASCII text including the escape-relevant chars `&<>`
    // but no leading/trailing whitespace and no newlines (HTML5 would
    // normalize those).
    fn arb_safe_text() -> impl Strategy<Value = String> {
        "[A-Za-z0-9 &<>]{0,40}".prop_map(|s| s.trim().to_string())
    }

    fn arb_element_id() -> impl Strategy<Value = String> {
        "el_[A-Z0-9]{6,12}".prop_map(|s| s.to_string())
    }

    fn arb_text_element() -> impl Strategy<Value = ElementNode> {
        (
            arb_element_id(),
            arb_geometry(),
            arb_text_style(),
            arb_safe_text(),
        )
            .prop_map(|(id, geom, style, text)| ElementNode {
                id,
                element_type: ElementType::Text,
                geometry: geom,
                style: ElementStyle::Text(style),
                content: ElementContent::Text(RichText::new(text)),
                children: vec![],
                placeholder_fill: None,
                name: None,
                link: None,
                attributes: BTreeMap::new(),
                inline_styles: BTreeMap::new(),
            })
    }

    fn arb_image_element() -> impl Strategy<Value = ElementNode> {
        (arb_element_id(), arb_geometry(), "[a-z0-9_]{4,12}").prop_map(|(id, geom, aid)| {
            let mut n = image_element(id, aid);
            n.geometry = geom;
            n
        })
    }

    fn arb_shape_element() -> impl Strategy<Value = ElementNode> {
        let shape = prop_oneof![
            Just(ShapeGeometry::Rectangle),
            Just(ShapeGeometry::Ellipse),
            (0u32..100).prop_map(|r| ShapeGeometry::RoundedRect { radius_px: r }),
        ];
        (arb_element_id(), arb_geometry(), shape).prop_map(|(id, geom, sg)| {
            let mut n = shape_element(id, sg);
            n.geometry = geom;
            n
        })
    }

    fn arb_leaf_element() -> impl Strategy<Value = ElementNode> {
        prop_oneof![arb_text_element(), arb_image_element(), arb_shape_element()]
    }

    fn arb_element_tree() -> impl Strategy<Value = ElementNode> {
        // prop_recursive: leaves are simple elements; inner nodes are Groups
        // that bundle up to 4 children. Depth capped at 3 to keep test sizes
        // tractable.
        arb_leaf_element().prop_recursive(3, 16, 4, |inner| {
            (arb_element_id(), prop::collection::vec(inner, 0..4))
                .prop_map(|(id, kids)| group_element(id, kids))
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 256,
            .. ProptestConfig::default()
        })]

        #[test]
        fn fuzz_text_element_roundtrip(n in arb_text_element()) {
            let html = serialize_element(&n);
            let back = parse_element(&html).map_err(|e| TestCaseError::fail(format!("{e}")))?;
            prop_assert_eq!(back, n);
        }

        #[test]
        fn fuzz_image_element_roundtrip(n in arb_image_element()) {
            let html = serialize_element(&n);
            let back = parse_element(&html).map_err(|e| TestCaseError::fail(format!("{e}")))?;
            prop_assert_eq!(back, n);
        }

        #[test]
        fn fuzz_shape_element_roundtrip(n in arb_shape_element()) {
            let html = serialize_element(&n);
            let back = parse_element(&html).map_err(|e| TestCaseError::fail(format!("{e}")))?;
            prop_assert_eq!(back, n);
        }

        #[test]
        fn fuzz_element_tree_roundtrip(n in arb_element_tree()) {
            let html = serialize_element(&n);
            let back = parse_element(&html).map_err(|e| TestCaseError::fail(format!("{e}")))?;
            prop_assert_eq!(back, n);
        }

        #[test]
        fn fuzz_slide_roundtrip(
            slide_id in "s_[A-Z0-9]{4,10}",
            layout_id in "[a-z]{3,8}",
            root_id in arb_element_id(),
            kids in prop::collection::vec(arb_leaf_element(), 0..4),
        ) {
            let root = group_element(root_id, kids);
            let slide = SlideNode::new(slide_id, layout_id, root);
            let html = serialize_slide(&slide);
            let back = parse_slide_fragment(&html)
                .map_err(|e| TestCaseError::fail(format!("{e}")))?;
            prop_assert_eq!(back, slide);
        }
    }
}
