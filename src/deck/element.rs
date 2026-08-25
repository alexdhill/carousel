use crate::deck::ids::{ElementId, new_element_id};
use crate::deck::style::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ElementType {
    Text,
    Image,
    Shape,
    Media,
    Table,
    Group,
    Embed,
}

impl ElementType {
    pub fn as_html(self) -> &'static str {
        match self {
            ElementType::Text => "text",
            ElementType::Image => "image",
            ElementType::Shape => "shape",
            ElementType::Media => "media",
            ElementType::Table => "table",
            ElementType::Group => "group",
            ElementType::Embed => "embed",
        }
    }

    pub fn from_html(s: &str) -> Option<Self> {
        Some(match s {
            "text" => ElementType::Text,
            "image" => ElementType::Image,
            "shape" => ElementType::Shape,
            "media" => ElementType::Media,
            "table" => ElementType::Table,
            "group" => ElementType::Group,
            "embed" => ElementType::Embed,
            _ => return None,
        })
    }
}

/// Character-level formatting marks carried by a `TextRun`.
///
/// Every field is an override of the owning element's `TextStyle`: `false` /
/// `None` means "inherit". A run whose marks are entirely default carries no
/// information and is dropped by `normalize_runs`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct RunMarks {
    #[serde(default, skip_serializing_if = "is_false")]
    pub bold: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub italic: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub underline: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub strike: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<ColorRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<Length>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl RunMarks {
    /// True when the marks override nothing, i.e. the run is redundant.
    pub fn is_empty(&self) -> bool {
        !self.bold
            && !self.italic
            && !self.underline
            && !self.strike
            && self.color.is_none()
            && self.font_size.is_none()
            && self.link.is_none()
    }
}

/// A half-open byte range `[start, end)` of `RichText::plain` carrying marks.
///
/// Offsets are byte offsets and must fall on UTF-8 character boundaries;
/// `normalize_runs` is the single place that invariant is enforced.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TextRun {
    pub start: usize,
    pub end: usize,
    pub marks: RunMarks,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ListKind {
    #[default]
    Bullet,
    Ordered,
}

impl ListKind {
    pub fn as_html(self) -> &'static str {
        match self {
            ListKind::Bullet => "bullet",
            ListKind::Ordered => "ordered",
        }
    }

    pub fn from_html(s: &str) -> Option<Self> {
        Some(match s {
            "bullet" => ListKind::Bullet,
            "ordered" => ListKind::Ordered,
            _ => return None,
        })
    }
}

/// Whole-box list treatment. `levels` is parallel to the newline-split
/// paragraphs of `RichText::plain`; a missing entry reads as level 0.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListStyle {
    pub kind: ListKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub levels: Vec<u8>,
}

/// Text content of a text element or table cell.
///
/// `plain` is the flattened text and remains the token-substitution, search
/// and export surface. `runs` and `list` are additive and elide from both
/// serde and HTML when absent, so an unformatted 1.2 deck is byte-identical
/// to a 1.1 one.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct RichText {
    pub plain: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<TextRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<ListStyle>,
}

impl RichText {
    pub fn new(s: impl Into<String>) -> Self {
        Self {
            plain: s.into(),
            runs: Vec::new(),
            list: None,
        }
    }

    /// True when the value carries no formatting and can take the plain path.
    pub fn is_plain(&self) -> bool {
        self.runs.is_empty() && self.list.is_none()
    }
}

/// Enforce the `runs` invariants in place: in-bounds, on char boundaries,
/// sorted, non-overlapping, no empty marks, adjacent identical runs merged.
///
/// Inputs: any `RichText`, including one assembled by a parser from untrusted
/// HTML. Output: none; `rt.runs` is rewritten. Errors: none — out-of-range or
/// mid-character offsets are clamped rather than panicking, because they can
/// originate from a hand-edited deck. Control flow: clamp, drop, sort, merge,
/// then assert the post-conditions the rest of the code relies on.
pub fn normalize_runs(rt: &mut RichText) {
    const MAX_RUNS: usize = 100_000;
    assert!(rt.runs.len() <= MAX_RUNS, "normalize_runs: run ceiling");
    let len: usize = rt.plain.len();
    let mut kept: Vec<TextRun> = Vec::with_capacity(rt.runs.len());
    for run in rt.runs.drain(..) {
        let mut start: usize = run.start.min(len);
        let mut end: usize = run.end.min(len);
        while start < len && !rt.plain.is_char_boundary(start) {
            start += 1;
        }
        while end < len && !rt.plain.is_char_boundary(end) {
            end += 1;
        }
        if start >= end || run.marks.is_empty() {
            continue;
        }
        kept.push(TextRun {
            start,
            end,
            marks: run.marks,
        });
    }
    kept.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<TextRun> = Vec::with_capacity(kept.len());
    for run in kept {
        match merged.last_mut() {
            Some(prev) if prev.end >= run.start && prev.marks == run.marks => {
                prev.end = prev.end.max(run.end);
            }
            Some(prev) if prev.end > run.start => {
                let clipped: usize = prev.end.max(run.start);
                if clipped < run.end {
                    merged.push(TextRun {
                        start: clipped,
                        end: run.end,
                        marks: run.marks,
                    });
                }
            }
            _ => merged.push(run),
        }
    }
    for run in &merged {
        assert!(
            rt.plain.is_char_boundary(run.start) && rt.plain.is_char_boundary(run.end),
            "normalize_runs: run offset not on a char boundary"
        );
    }
    rt.runs = merged;
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetRef {
    pub asset_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ShapeGeometry {
    #[default]
    Rectangle,
    Ellipse,
    RoundedRect {
        radius_px: u32,
    },
    Path {
        d: String,
    },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TableData {
    pub rows: usize,
    pub columns: usize,
    pub cells: Vec<Vec<TableCell>>,
    pub header_rows: usize,
    pub header_columns: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TableCell {
    pub content: RichText,
    pub style_overrides: BTreeMap<String, String>,
    pub colspan: usize,
    pub rowspan: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ElementStyle {
    Text(TextStyle),
    Image(ImageStyle),
    Shape(ShapeStyle),
    Media(MediaStyle),
    Table(TableStyle),
    Group(GroupStyle),
    Embed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ElementContent {
    Text(RichText),
    Image(AssetRef),
    Shape(ShapeGeometry),
    Media(AssetRef),
    Table(TableData),
    Group,
    Embed(String),
}

pub type PlaceholderId = String;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ElementNode {
    pub id: ElementId,
    pub element_type: ElementType,
    pub geometry: Geometry,
    pub style: ElementStyle,
    pub content: ElementContent,
    pub children: Vec<ElementNode>,
    pub placeholder_fill: Option<PlaceholderId>,
    pub name: Option<String>,
    pub link: Option<String>,
    pub attributes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inline_styles: BTreeMap<String, String>,

    #[serde(default)]
    pub placeholder: bool,
}

impl ElementNode {
    pub fn is_layout_element(&self) -> bool {
        self.id.starts_with("layout_")
    }

    pub fn is_consistent(&self) -> bool {
        matches!(
            (&self.element_type, &self.style, &self.content),
            (
                ElementType::Text,
                ElementStyle::Text(_),
                ElementContent::Text(_)
            ) | (
                ElementType::Image,
                ElementStyle::Image(_),
                ElementContent::Image(_)
            ) | (
                ElementType::Shape,
                ElementStyle::Shape(_),
                ElementContent::Shape(_)
            ) | (
                ElementType::Media,
                ElementStyle::Media(_),
                ElementContent::Media(_)
            ) | (
                ElementType::Table,
                ElementStyle::Table(_),
                ElementContent::Table(_)
            ) | (
                ElementType::Group,
                ElementStyle::Group(_),
                ElementContent::Group
            ) | (
                ElementType::Embed,
                ElementStyle::Embed,
                ElementContent::Embed(_)
            )
        )
    }
}

pub fn regenerate_ids(root: &mut ElementNode) -> std::collections::HashMap<ElementId, ElementId> {
    const MAX_NODES: usize = 1_000_000;
    let mut map: std::collections::HashMap<ElementId, ElementId> = std::collections::HashMap::new();
    let mut stack: Vec<&mut ElementNode> = vec![root];
    let mut seen: usize = 0;
    while let Some(node) = stack.pop() {
        seen += 1;
        assert!(seen <= MAX_NODES, "regenerate_ids: node ceiling exceeded");
        let fresh: ElementId = new_element_id();
        map.insert(node.id.clone(), fresh.clone());
        node.id = fresh;

        node.placeholder = false;
        for child in node.children.iter_mut() {
            stack.push(child);
        }
    }
    map
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::builders::{group_element, text_element};

    #[test]
    fn is_layout_element_by_id_prefix() {
        let mut layout = crate::deck::builders::text_element("layout_text_title", "T");
        assert!(layout.is_layout_element());
        layout.id = "el_01ABC".into();
        assert!(!layout.is_layout_element());
    }

    #[test]
    fn regenerate_ids_clears_placeholder() {
        let mut n = crate::deck::builders::text_element("layout_text_title", "T");
        n.placeholder = true;
        regenerate_ids(&mut n);
        assert!(
            !n.placeholder,
            "a regenerated copy is user content, not a slot"
        );
        assert!(!n.is_layout_element(), "fresh id is not a layout slot");
    }

    #[test]
    fn regenerate_ids_changes_every_id_and_preserves_structure() {
        use std::collections::HashSet;
        let mut root = group_element(
            "el_root".to_string(),
            vec![group_element(
                "el_a".to_string(),
                vec![group_element("el_b".to_string(), vec![])],
            )],
        );
        let map = regenerate_ids(&mut root);
        assert_eq!(map.len(), 3);
        let ids: HashSet<&String> = [
            &root.id,
            &root.children[0].id,
            &root.children[0].children[0].id,
        ]
        .into_iter()
        .collect();
        assert_eq!(ids.len(), 3);
        assert!(root.id.starts_with("el_"));
        assert_ne!(root.id, "el_root");
        assert_eq!(
            map.get("el_root").map(String::as_str),
            Some(root.id.as_str())
        );
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].children.len(), 1);
    }

    #[test]
    fn type_html_roundtrips() {
        for t in [
            ElementType::Text,
            ElementType::Image,
            ElementType::Shape,
            ElementType::Media,
            ElementType::Table,
            ElementType::Group,
            ElementType::Embed,
        ] {
            assert_eq!(ElementType::from_html(t.as_html()), Some(t));
        }
    }

    #[test]
    fn type_from_html_rejects_unknown() {
        assert_eq!(ElementType::from_html("title"), None);
        assert_eq!(ElementType::from_html(""), None);
        assert_eq!(ElementType::from_html("Text"), None);
    }

    #[test]
    fn consistent_triple_passes_invariant() {
        let n = text_element("id_a", "hello");
        assert!(n.is_consistent());
    }

    #[test]
    fn inconsistent_triple_fails_invariant() {
        let mut n = text_element("id_a", "hello");
        n.element_type = ElementType::Image;
        assert!(!n.is_consistent());
    }

    #[test]
    fn group_default_is_consistent() {
        let g = group_element("id_g", vec![]);
        assert!(g.is_consistent());
    }

    fn bold() -> RunMarks {
        RunMarks {
            bold: true,
            ..RunMarks::default()
        }
    }

    #[test]
    fn normalize_drops_empty_and_zero_width_runs() {
        let mut rt = RichText::new("hello");
        rt.runs = vec![
            TextRun {
                start: 0,
                end: 2,
                marks: RunMarks::default(),
            },
            TextRun {
                start: 3,
                end: 3,
                marks: bold(),
            },
        ];
        normalize_runs(&mut rt);
        assert!(rt.runs.is_empty());
    }

    #[test]
    fn normalize_merges_adjacent_identical_runs() {
        let mut rt = RichText::new("hello");
        rt.runs = vec![
            TextRun {
                start: 2,
                end: 4,
                marks: bold(),
            },
            TextRun {
                start: 0,
                end: 2,
                marks: bold(),
            },
        ];
        normalize_runs(&mut rt);
        assert_eq!(rt.runs.len(), 1);
        assert_eq!((rt.runs[0].start, rt.runs[0].end), (0, 4));
    }

    #[test]
    fn normalize_clamps_out_of_range_and_mid_char_offsets() {
        let mut rt = RichText::new("héllo");
        rt.runs = vec![TextRun {
            start: 2,
            end: 999,
            marks: bold(),
        }];
        normalize_runs(&mut rt);
        assert_eq!(rt.runs.len(), 1);
        assert!(rt.plain.is_char_boundary(rt.runs[0].start));
        assert_eq!(rt.runs[0].end, rt.plain.len());
        assert_eq!(&rt.plain[rt.runs[0].start..rt.runs[0].end], "llo");
    }

    #[test]
    fn normalize_clips_overlapping_runs_with_different_marks() {
        let mut rt = RichText::new("abcdef");
        rt.runs = vec![
            TextRun {
                start: 0,
                end: 4,
                marks: bold(),
            },
            TextRun {
                start: 2,
                end: 6,
                marks: RunMarks {
                    italic: true,
                    ..RunMarks::default()
                },
            },
        ];
        normalize_runs(&mut rt);
        assert_eq!(rt.runs.len(), 2);
        assert_eq!((rt.runs[0].start, rt.runs[0].end), (0, 4));
        assert_eq!((rt.runs[1].start, rt.runs[1].end), (4, 6));
    }

    #[test]
    fn rich_text_serde_roundtrips_with_runs_and_list() {
        let mut rt = RichText::new("a\nb");
        rt.runs = vec![TextRun {
            start: 0,
            end: 1,
            marks: bold(),
        }];
        rt.list = Some(ListStyle {
            kind: ListKind::Ordered,
            levels: vec![0, 1],
        });
        let json = serde_json::to_string(&rt).unwrap();
        assert_eq!(serde_json::from_str::<RichText>(&json).unwrap(), rt);
    }

    #[test]
    fn rich_text_reads_one_one_shaped_json() {
        let rt: RichText = serde_json::from_str(r#"{"plain":"x"}"#).unwrap();
        assert_eq!(rt, RichText::new("x"));
        assert!(rt.is_plain());
    }

    #[test]
    fn unformatted_rich_text_serializes_without_new_keys() {
        let json = serde_json::to_string(&RichText::new("x")).unwrap();
        assert_eq!(json, r#"{"plain":"x"}"#);
    }

    #[test]
    fn list_kind_html_roundtrips() {
        for k in [ListKind::Bullet, ListKind::Ordered] {
            assert_eq!(ListKind::from_html(k.as_html()), Some(k));
        }
        assert_eq!(ListKind::from_html("dotted"), None);
    }

    #[test]
    fn element_node_serde_roundtrips() {
        let node = text_element("el_a", "hello");
        let json = serde_json::to_string(&node).unwrap();
        let back: ElementNode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, node);
    }
}
