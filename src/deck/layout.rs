// LayoutNode.
//
// A layout is a reusable element template edited with the same tooling as a
// slide. Like a slide it owns a single `root` ElementNode (always a Group)
// whose children are the top-level elements, plus a human-readable `id`
// (stable, used as the map key and on disk) and a display `name`. It is a
// Canvas, so the element commands edit it through the shared interface.

use crate::deck::canvas::Canvas;
use crate::deck::element::ElementNode;
use crate::deck::ids::LayoutId;
use serde::{Deserialize, Serialize};

// LayoutNode
// Inputs at construction: id, name, root (must be a Group).
// `dirty` is set by commands when the layout changes; clears on save.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LayoutNode {
    pub id: LayoutId,
    pub name: String,
    pub root: ElementNode,
    // Theme-level background for slides built on this layout. A slide inherits
    // these when its own metadata leaves the field empty (see
    // Deck::effective_slide_bg). `background_image` holds a var(--asset-<id>).
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub background_image: Option<String>,
    // Editor alignment guides owned by this layout. Slides built on the layout
    // display these read-only (inherited) in addition to their own; see
    // Deck::inherited_guides. Persisted via theme_io like the layout background.
    #[serde(default)]
    pub guides: Vec<crate::deck::guide::Guide>,
    pub dirty: bool,
}

impl LayoutNode {
    // new
    // Inputs: id (non-empty), display name, a root ElementNode that must be a
    // Group satisfying the element-triple invariant.
    // Output: a LayoutNode with dirty=false.
    // Errors: panics if id is empty or the root is not a consistent Group —
    // layouts are always group-rooted by the model invariant (as slides are).
    pub fn new(id: LayoutId, name: String, root: ElementNode) -> Self {
        assert!(!id.is_empty(), "layout id must not be empty");
        assert!(
            root.is_consistent(),
            "layout root must satisfy the element-triple invariant"
        );
        assert!(
            matches!(root.element_type, crate::deck::element::ElementType::Group),
            "layout root must be a Group element"
        );
        Self {
            id,
            name,
            root,
            background: None,
            background_image: None,
            guides: Vec::new(),
            dirty: false,
        }
    }

    // seeded_children
    // Output: this layout's top-level elements cloned for stamping onto a slide.
    // Content-bearing slots (Text / Image / Media / Table) are marked
    // `placeholder = true` so an untouched slot hides in playback; decorative
    // slots (Shape / Group / Embed) are always rendered (`placeholder = false`),
    // matching how PowerPoint/Keynote treat layout graphics. Ids are preserved —
    // they are the slot keys used for layout-change remapping.
    pub fn seeded_children(&self) -> Vec<ElementNode> {
        use crate::deck::element::ElementType;
        let mut out: Vec<ElementNode> = self.root.children.clone();
        for child in out.iter_mut() {
            child.placeholder = matches!(
                child.element_type,
                ElementType::Text
                    | ElementType::Image
                    | ElementType::Media
                    | ElementType::Table
            );
        }
        out
    }

    // preview_slide
    // Output: a transient SlideNode wrapping this layout's root so it reuses
    // the slide serializer / object-tree builder, carrying the layout's own
    // background so the layout preview and its on-disk HTML show it.
    pub fn preview_slide(&self) -> crate::deck::slide::SlideNode {
        let mut s =
            crate::deck::slide::SlideNode::new(self.id.clone(), self.id.clone(), self.root.clone());
        s.metadata.background = self.background.clone();
        s.metadata.background_image = self.background_image.clone();
        s
    }
}

// LayoutNode is an editable Canvas, sharing the slide element tooling.
impl Canvas for LayoutNode {
    fn root(&self) -> &ElementNode {
        &self.root
    }
    fn root_mut(&mut self) -> &mut ElementNode {
        &mut self.root
    }
    fn mark_dirty(&mut self) {
        self.dirty = true;
    }
    fn guides(&self) -> &Vec<crate::deck::guide::Guide> {
        &self.guides
    }
    fn guides_mut(&mut self) -> &mut Vec<crate::deck::guide::Guide> {
        &mut self.guides
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::builders::{group_element, text_element};

    #[test]
    fn layout_construction_succeeds_with_group_root() {
        let root = group_element("root", vec![text_element("a", "hi")]);
        let layout = LayoutNode::new("title".into(), "Title".into(), root);
        assert_eq!(layout.id, "title");
        assert_eq!(layout.name, "Title");
        assert!(!layout.dirty);
        assert_eq!(layout.root.children.len(), 1);
    }

    #[test]
    #[should_panic(expected = "layout root must be a Group")]
    fn layout_rejects_non_group_root() {
        let root = text_element("a", "hi");
        let _ = LayoutNode::new("l1".into(), "L".into(), root);
    }

    #[test]
    #[should_panic(expected = "layout id must not be empty")]
    fn layout_rejects_empty_id() {
        let root = group_element("root", vec![]);
        let _ = LayoutNode::new(String::new(), "L".into(), root);
    }

    #[test]
    fn find_element_via_trait_locates_nodes() {
        let root = group_element("rt", vec![text_element("a", "x"), text_element("b", "y")]);
        let layout = LayoutNode::new("l".into(), "L".into(), root);
        assert_eq!(layout.find_element("rt").map(|n| n.id.as_str()), Some("rt"));
        assert_eq!(layout.find_element("a").map(|n| n.id.as_str()), Some("a"));
        assert!(layout.find_element("missing").is_none());
    }

    #[test]
    fn mark_dirty_via_trait_sets_flag() {
        let root = group_element("rt", vec![]);
        let mut layout = LayoutNode::new("l".into(), "L".into(), root);
        layout.mark_dirty();
        assert!(layout.dirty);
    }

    #[test]
    fn layout_serde_roundtrips() {
        let root = group_element("r", vec![text_element("a", "hi")]);
        let layout = LayoutNode::new("l1".into(), "L".into(), root);
        let json = serde_json::to_string(&layout).unwrap();
        let back: LayoutNode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, layout);
    }

    #[test]
    fn seeded_children_marks_content_types_only() {
        use crate::deck::builders::{shape_element, text_element};
        use crate::deck::element::ShapeGeometry;
        let text = text_element("layout_text_title", "T");
        let shape = shape_element("layout_shape_hero", ShapeGeometry::Rectangle);
        let root = group_element("r", vec![text, shape]);
        let layout = LayoutNode::new("l".into(), "L".into(), root);
        let seeded = layout.seeded_children();
        let t = seeded.iter().find(|c| c.id == "layout_text_title").unwrap();
        let s = seeded.iter().find(|c| c.id == "layout_shape_hero").unwrap();
        assert!(t.placeholder, "text slot is a placeholder");
        assert!(!s.placeholder, "decorative shape always renders");
    }
}
