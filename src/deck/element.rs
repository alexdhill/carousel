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

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RichText {
    pub plain: String,
}

impl RichText {
    pub fn new(s: impl Into<String>) -> Self {
        Self { plain: s.into() }
    }
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

    #[test]
    fn element_node_serde_roundtrips() {
        let node = text_element("el_a", "hello");
        let json = serde_json::to_string(&node).unwrap();
        let back: ElementNode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, node);
    }
}
