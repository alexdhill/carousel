// Element builders.
//
// Constructor surface that enforces the (ElementType, ElementStyle,
// ElementContent) invariant by construction. Every variant has exactly one
// builder so it is impossible to land in an inconsistent state from
// application code.

use crate::deck::element::*;
use crate::deck::ids::ElementId;
use crate::deck::style::*;
use std::collections::BTreeMap;

// text_element
// Inputs: id, plain text.
// Output: a Text ElementNode with default style and zero geometry.
pub fn text_element(id: impl Into<ElementId>, text: impl Into<String>) -> ElementNode {
    let id: ElementId = id.into();
    assert!(!id.is_empty(), "element id must not be empty");
    ElementNode {
        id,
        element_type: ElementType::Text,
        geometry: Geometry::default(),
        style: ElementStyle::Text(TextStyle::default()),
        content: ElementContent::Text(RichText::new(text)),
        children: vec![],
        placeholder_fill: None,
        placeholder: false,
        name: None,
        link: None,
        attributes: BTreeMap::new(),
        inline_styles: BTreeMap::new(),
    }
}

// text_element_styled
// Inputs: id, text, geometry, style.
// Output: a Text ElementNode using the supplied geometry + style.
pub fn text_element_styled(
    id: impl Into<ElementId>,
    text: impl Into<String>,
    geometry: Geometry,
    style: TextStyle,
) -> ElementNode {
    let mut node = text_element(id, text);
    node.geometry = geometry;
    node.style = ElementStyle::Text(style);
    node
}

// image_element
// Inputs: id, asset id.
// Output: an Image ElementNode with default ImageStyle.
pub fn image_element(id: impl Into<ElementId>, asset_id: impl Into<String>) -> ElementNode {
    let id: ElementId = id.into();
    assert!(!id.is_empty(), "element id must not be empty");
    ElementNode {
        id,
        element_type: ElementType::Image,
        geometry: Geometry::default(),
        style: ElementStyle::Image(ImageStyle::default()),
        content: ElementContent::Image(AssetRef {
            asset_id: asset_id.into(),
        }),
        children: vec![],
        placeholder_fill: None,
        placeholder: false,
        name: None,
        link: None,
        attributes: BTreeMap::new(),
        inline_styles: BTreeMap::new(),
    }
}

// shape_element
// Inputs: id, geometry kind.
// Output: a Shape ElementNode with default ShapeStyle.
pub fn shape_element(id: impl Into<ElementId>, geom: ShapeGeometry) -> ElementNode {
    let id: ElementId = id.into();
    assert!(!id.is_empty(), "element id must not be empty");
    ElementNode {
        id,
        element_type: ElementType::Shape,
        geometry: Geometry::default(),
        style: ElementStyle::Shape(ShapeStyle::default()),
        content: ElementContent::Shape(geom),
        children: vec![],
        placeholder_fill: None,
        placeholder: false,
        name: None,
        link: None,
        attributes: BTreeMap::new(),
        inline_styles: BTreeMap::new(),
    }
}

// group_element
// Inputs: id, children.
// Output: a Group ElementNode that owns the supplied children.
pub fn group_element(id: impl Into<ElementId>, children: Vec<ElementNode>) -> ElementNode {
    let id: ElementId = id.into();
    assert!(!id.is_empty(), "element id must not be empty");
    ElementNode {
        id,
        element_type: ElementType::Group,
        geometry: Geometry::default(),
        style: ElementStyle::Group(crate::deck::style::GroupStyle::default()),
        content: ElementContent::Group,
        children,
        placeholder_fill: None,
        placeholder: false,
        name: None,
        link: None,
        attributes: BTreeMap::new(),
        inline_styles: BTreeMap::new(),
    }
}

// embed_element
// Inputs: id, raw HTML string.
// Output: an Embed ElementNode.
pub fn embed_element(id: impl Into<ElementId>, html: impl Into<String>) -> ElementNode {
    let id: ElementId = id.into();
    assert!(!id.is_empty(), "element id must not be empty");
    ElementNode {
        id,
        element_type: ElementType::Embed,
        geometry: Geometry::default(),
        style: ElementStyle::Embed,
        content: ElementContent::Embed(html.into()),
        children: vec![],
        placeholder_fill: None,
        placeholder: false,
        name: None,
        link: None,
        attributes: BTreeMap::new(),
        inline_styles: BTreeMap::new(),
    }
}

// table_element
// Inputs: id, a TableData grid.
// Output: a Table ElementNode wrapping the grid.
pub fn table_element(
    id: impl Into<ElementId>,
    data: crate::deck::element::TableData,
) -> ElementNode {
    let id: ElementId = id.into();
    assert!(!id.is_empty(), "element id must not be empty");
    ElementNode {
        id,
        element_type: ElementType::Table,
        geometry: Geometry::default(),
        style: ElementStyle::Table(crate::deck::style::TableStyle::default()),
        content: ElementContent::Table(data),
        children: vec![],
        placeholder_fill: None,
        placeholder: false,
        name: None,
        link: None,
        attributes: BTreeMap::new(),
        inline_styles: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn every_builder_yields_a_consistent_triple() {
        assert!(text_element("a", "x").is_consistent());
        assert!(image_element("b", "asset_1").is_consistent());
        assert!(shape_element("c", ShapeGeometry::Rectangle).is_consistent());
        assert!(group_element("d", vec![]).is_consistent());
        assert!(embed_element("e", "<b/>").is_consistent());
    }

    #[test]
    #[should_panic(expected = "element id must not be empty")]
    fn empty_id_is_rejected() {
        let _ = text_element("", "hi");
    }

    #[test]
    fn styled_text_preserves_inputs() {
        let g = Geometry {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
            ..Default::default()
        };
        let s = TextStyle {
            font_weight: 700,
            ..TextStyle::default()
        };
        let n = text_element_styled("id", "x", g.clone(), s.clone());
        assert_eq!(n.geometry, g);
        match &n.style {
            ElementStyle::Text(ts) => assert_eq!(ts.font_weight, 700),
            other => panic!("unexpected style: {other:?}"),
        }
    }

    #[test]
    fn group_holds_children_in_order() {
        let kids = vec![text_element("a", "1"), text_element("b", "2")];
        let g = group_element("g", kids);
        assert_eq!(g.children.len(), 2);
        assert_eq!(g.children[0].id, "a");
        assert_eq!(g.children[1].id, "b");
    }
}
