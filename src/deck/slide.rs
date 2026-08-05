use crate::deck::animation::AnimationEntry;
use crate::deck::canvas::{self, Canvas};
use crate::deck::element::ElementNode;
use crate::deck::ids::{LayoutId, SlideId};
use serde::{Deserialize, Serialize};

pub use crate::deck::canvas::{InsertError, RemovedElement};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransitionKind {
    #[default]
    None,
    Fade,
    Push,

    Dissolve,
    Wipe,
    Flip,
    Cube,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SlideTransition {
    pub kind: TransitionKind,
    pub duration_ms: u32,

    pub easing: String,
}

impl Default for SlideTransition {

    fn default() -> Self {
        Self {
            kind: TransitionKind::None,
            duration_ms: 400,
            easing: "ease".into(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SlideMetadata {
    pub title: Option<String>,
    pub notes: Option<String>,

    #[serde(default)]
    pub background: Option<String>,

    #[serde(default)]
    pub background_image: Option<String>,

    #[serde(default)]
    pub transition: Option<SlideTransition>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SlideNode {
    pub id: SlideId,
    pub layout_id: LayoutId,
    pub root: ElementNode,
    pub metadata: SlideMetadata,

    #[serde(default)]
    pub animations: Vec<AnimationEntry>,

    #[serde(default)]
    pub guides: Vec<crate::deck::guide::Guide>,
    pub dirty: bool,
}

impl SlideNode {

    pub fn is_root_id(&self, id: &str) -> bool {
        self.root.id == id
    }

    pub fn find_element<'a>(&'a self, id: &str) -> Option<&'a ElementNode> {
        canvas::find_element(&self.root, id)
    }

    pub fn find_element_mut<'a>(&'a mut self, id: &str) -> Option<&'a mut ElementNode> {
        canvas::find_element_mut(&mut self.root, id)
    }

    pub fn remove_non_root_element(&mut self, id: &str) -> Option<RemovedElement> {
        assert!(
            self.root.id != id,
            "remove_non_root_element called with root id"
        );
        canvas::remove_non_root_element(&mut self.root, id)
    }

    pub fn insert_child(
        &mut self,
        parent_id: &str,
        position: usize,
        node: ElementNode,
    ) -> Result<(), InsertError> {
        canvas::insert_child(&mut self.root, parent_id, position, node)
    }

    pub fn invalidate_index(&mut self) {}

    pub fn new(id: SlideId, layout_id: LayoutId, root: ElementNode) -> Self {
        assert!(!id.is_empty(), "slide id must not be empty");
        assert!(
            root.is_consistent(),
            "slide root must satisfy the element-triple invariant"
        );
        assert!(
            matches!(root.element_type, crate::deck::element::ElementType::Group),
            "slide root must be a Group element"
        );
        Self {
            id,
            layout_id,
            root,
            metadata: SlideMetadata::default(),
            animations: Vec::new(),
            guides: Vec::new(),
            dirty: false,
        }
    }
}

impl Canvas for SlideNode {
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
    fn transition_kinds_round_trip_by_name() {

        for kind in [
            TransitionKind::None,
            TransitionKind::Fade,
            TransitionKind::Push,
            TransitionKind::Dissolve,
            TransitionKind::Wipe,
            TransitionKind::Flip,
            TransitionKind::Cube,
        ] {
            let t = SlideTransition {
                kind: kind.clone(),
                duration_ms: 500,
                easing: "ease".into(),
            };
            let json = serde_json::to_string(&t).unwrap();
            let back: SlideTransition = serde_json::from_str(&json).unwrap();
            assert_eq!(back.kind, kind);
        }

        let json = serde_json::to_string(&TransitionKind::Cube).unwrap();
        assert_eq!(json, "\"Cube\"");
    }

    #[test]
    fn slide_construction_succeeds_with_group_root() {
        let root = group_element("root", vec![text_element("a", "hi")]);
        let slide = SlideNode::new("s1".into(), "title".into(), root);
        assert_eq!(slide.id, "s1");
        assert!(!slide.dirty);
        assert_eq!(slide.root.children.len(), 1);
    }

    #[test]
    #[should_panic(expected = "slide root must be a Group")]
    fn slide_rejects_non_group_root() {
        let root = text_element("a", "hi");
        let _ = SlideNode::new("s1".into(), "title".into(), root);
    }

    #[test]
    #[should_panic(expected = "slide id must not be empty")]
    fn slide_rejects_empty_id() {
        let root = group_element("root", vec![]);
        let _ = SlideNode::new(String::new(), "title".into(), root);
    }

    #[test]
    fn find_element_locates_root_and_children() {
        let root = group_element("rt", vec![text_element("a", "x"), text_element("b", "y")]);
        let slide = SlideNode::new("s".into(), "title".into(), root);
        assert_eq!(slide.find_element("rt").map(|n| n.id.as_str()), Some("rt"));
        assert_eq!(slide.find_element("a").map(|n| n.id.as_str()), Some("a"));
        assert_eq!(slide.find_element("b").map(|n| n.id.as_str()), Some("b"));
        assert!(slide.find_element("missing").is_none());
    }

    #[test]
    fn find_element_descends_into_nested_groups() {
        let inner = group_element("g_in", vec![text_element("deep", "z")]);
        let outer = group_element("g_out", vec![inner]);
        let root = group_element("rt", vec![outer]);
        let slide = SlideNode::new("s".into(), "title".into(), root);
        assert!(slide.find_element("deep").is_some());
    }

    #[test]
    fn find_element_mut_allows_geometry_mutation() {
        let root = group_element("rt", vec![text_element("a", "x")]);
        let mut slide = SlideNode::new("s".into(), "title".into(), root);
        let target = slide.find_element_mut("a").unwrap();
        target.geometry.x = 42.0;
        assert_eq!(slide.find_element("a").unwrap().geometry.x, 42.0);
    }

    #[test]
    fn remove_non_root_element_returns_subtree_and_position() {
        let kids = vec![
            text_element("a", "x"),
            text_element("b", "y"),
            text_element("c", "z"),
        ];
        let root = group_element("rt", kids);
        let mut slide = SlideNode::new("s".into(), "title".into(), root);
        let removed = slide.remove_non_root_element("b").unwrap();
        assert_eq!(removed.node.id, "b");
        assert_eq!(removed.parent_id, "rt");
        assert_eq!(removed.position, 1);
        assert_eq!(slide.root.children.len(), 2);
        assert_eq!(slide.root.children[0].id, "a");
        assert_eq!(slide.root.children[1].id, "c");
    }

    #[test]
    fn remove_non_root_element_returns_none_for_missing_id() {
        let root = group_element("rt", vec![text_element("a", "x")]);
        let mut slide = SlideNode::new("s".into(), "title".into(), root);
        assert!(slide.remove_non_root_element("missing").is_none());
    }

    #[test]
    #[should_panic(expected = "remove_non_root_element called with root id")]
    fn remove_non_root_element_panics_on_root() {
        let root = group_element("rt", vec![]);
        let mut slide = SlideNode::new("s".into(), "title".into(), root);
        let _ = slide.remove_non_root_element("rt");
    }

    #[test]
    fn insert_child_into_root() {
        let root = group_element("rt", vec![text_element("a", "x")]);
        let mut slide = SlideNode::new("s".into(), "title".into(), root);
        let new_node = text_element("b", "y");
        slide.insert_child("rt", 1, new_node).unwrap();
        assert_eq!(slide.root.children.len(), 2);
        assert_eq!(slide.root.children[1].id, "b");
    }

    #[test]
    fn insert_child_at_head() {
        let root = group_element("rt", vec![text_element("a", "x")]);
        let mut slide = SlideNode::new("s".into(), "title".into(), root);
        slide.insert_child("rt", 0, text_element("b", "y")).unwrap();
        assert_eq!(slide.root.children[0].id, "b");
        assert_eq!(slide.root.children[1].id, "a");
    }

    #[test]
    fn insert_child_rejects_missing_parent() {
        let root = group_element("rt", vec![]);
        let mut slide = SlideNode::new("s".into(), "title".into(), root);
        let err = slide
            .insert_child("nope", 0, text_element("b", "y"))
            .unwrap_err();
        assert_eq!(err, InsertError::ParentNotFound);
    }

    #[test]
    fn insert_child_rejects_out_of_range_position() {
        let root = group_element("rt", vec![text_element("a", "x")]);
        let mut slide = SlideNode::new("s".into(), "title".into(), root);
        let err = slide
            .insert_child("rt", 99, text_element("b", "y"))
            .unwrap_err();
        assert_eq!(
            err,
            InsertError::PositionOutOfRange {
                len: 1,
                requested: 99
            }
        );
    }

    #[test]
    fn remove_then_insert_at_same_position_round_trips_subtree() {
        let kids = vec![
            text_element("a", "x"),
            text_element("b", "y"),
            text_element("c", "z"),
        ];
        let root = group_element("rt", kids);
        let mut slide = SlideNode::new("s".into(), "title".into(), root);
        let removed = slide.remove_non_root_element("b").unwrap();
        slide
            .insert_child(&removed.parent_id, removed.position, removed.node)
            .unwrap();
        assert_eq!(slide.root.children.len(), 3);
        assert_eq!(slide.root.children[0].id, "a");
        assert_eq!(slide.root.children[1].id, "b");
        assert_eq!(slide.root.children[2].id, "c");
    }

    #[test]
    fn slide_serde_roundtrips() {
        let root = group_element("r", vec![text_element("a", "hi")]);
        let slide = SlideNode::new("s1".into(), "title".into(), root);
        let json = serde_json::to_string(&slide).unwrap();
        let back: SlideNode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, slide);
    }
}
