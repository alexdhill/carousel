// SlideNode.
//
// A slide owns a single `root` ElementNode — always a Group whose children
// are the top-level elements on the canvas. Wrapping everything in a Group
// keeps the tree uniform: commands that operate on "the slide's elements"
// just walk root.children, and slide-level layout could later sit on the
// root's geometry without a special case.

use crate::deck::element::ElementNode;
use crate::deck::ids::{ElementId, LayoutId, SlideId};
use serde::{Deserialize, Serialize};

// Upper bound on tree depth and total node count for any bounded loop in
// this file. Commands enforce a stricter group nesting limit at the model
// level (SPEC §5.9 = 10); these constants exist only so the bounded-loop
// requirement (CLAUDE.md) has a concrete ceiling well above any plausible
// slide.
const MAX_TREE_NODES: usize = 100_000;
const MAX_TREE_DEPTH: usize = 1024;

// RemovedElement
// Returned by SlideNode::remove_element. Carries enough information to
// re-insert the subtree at the same location, which is what the inverse
// command for RemoveElement needs.
#[derive(Clone, Debug, PartialEq)]
pub struct RemovedElement {
    pub node: ElementNode,
    pub parent_id: ElementId,
    pub position: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SlideMetadata {
    pub title: Option<String>,
    pub notes: Option<String>,
}

// SlideNode
// Inputs at construction: id, layout_id, root (must be a Group).
// `dirty` is set by commands when the slide changes; clears on save.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SlideNode {
    pub id: SlideId,
    pub layout_id: LayoutId,
    pub root: ElementNode,
    pub metadata: SlideMetadata,
    pub dirty: bool,
}

impl SlideNode {
    // new
    // Inputs: id, layout_id, a root ElementNode that must be a Group.
    // Output: a SlideNode with default metadata and dirty=false.
    // Errors: panics if the root is not a Group — slides are always
    // group-rooted by the model invariant.
    // is_root_id
    // Inputs: an element id.
    // Output: true if it matches the slide's root group.
    pub fn is_root_id(&self, id: &str) -> bool {
        self.root.id == id
    }

    // find_element
    // Inputs: an element id to locate.
    // Output: an immutable reference to the matching ElementNode, or None.
    // Dataflow: iterative depth-first search over the owned tree using an
    // explicit stack. The outer loop is bounded by MAX_TREE_NODES.
    pub fn find_element<'a>(&'a self, id: &str) -> Option<&'a ElementNode> {
        assert!(!id.is_empty(), "find_element called with empty id");
        let mut stack: Vec<&'a ElementNode> = Vec::new();
        stack.push(&self.root);
        for _ in 0..MAX_TREE_NODES {
            let Some(node) = stack.pop() else { return None; };
            if node.id == id {
                return Some(node);
            }
            for child in node.children.iter().rev() {
                stack.push(child);
            }
        }
        None
    }

    // find_element_mut
    // Inputs: an element id to locate.
    // Output: a mutable reference to the matching ElementNode, or None.
    // Dataflow: same iterative DFS as find_element, but the stack stores
    // mutable references. The borrow checker accepts the pop-then-push
    // pattern because each child is a disjoint subtree.
    pub fn find_element_mut<'a>(&'a mut self, id: &str) -> Option<&'a mut ElementNode> {
        assert!(!id.is_empty(), "find_element_mut called with empty id");
        let mut stack: Vec<&'a mut ElementNode> = Vec::new();
        stack.push(&mut self.root);
        for _ in 0..MAX_TREE_NODES {
            let Some(node) = stack.pop() else { return None; };
            if node.id == id {
                return Some(node);
            }
            for child in node.children.iter_mut().rev() {
                stack.push(child);
            }
        }
        None
    }

    // remove_non_root_element
    // Inputs: an element id; must not equal the slide root id.
    // Output: the removed subtree + its parent id + its prior position, or
    // None if the id is not present in the tree.
    // Dataflow:
    //   1. Iterative DFS records the path-of-indices from root.children to
    //      the parent of the target (a Vec<usize>); the search itself is
    //      immutable so the borrow checker is content.
    //   2. With the path in hand, walk it mutably to land on the parent,
    //      then call Vec::remove at the recorded index.
    pub fn remove_non_root_element(&mut self, id: &str) -> Option<RemovedElement> {
        assert!(!id.is_empty(), "remove_non_root_element called with empty id");
        assert!(self.root.id != id, "remove_non_root_element called with root id");
        let (path, position): (Vec<usize>, usize) = find_parent_path(&self.root, id)?;
        let mut current: &mut ElementNode = &mut self.root;
        let mut step: usize = 0;
        for &idx in &path {
            assert!(step < MAX_TREE_DEPTH, "tree depth exceeded MAX_TREE_DEPTH");
            current = &mut current.children[idx];
            step += 1;
        }
        let parent_id: ElementId = current.id.clone();
        let removed: ElementNode = current.children.remove(position);
        Some(RemovedElement { node: removed, parent_id, position })
    }

    // insert_child
    // Inputs: parent id, 0-indexed position, the node to insert (consumed).
    // Output: Ok(()) on success.
    // Errors: parent absent (NotFound), or position > parent.children.len()
    // (OutOfRange).
    pub fn insert_child(
        &mut self,
        parent_id: &str,
        position: usize,
        node: ElementNode,
    ) -> Result<(), InsertError> {
        assert!(!parent_id.is_empty(), "insert_child called with empty parent_id");
        let parent: &mut ElementNode = self
            .find_element_mut(parent_id)
            .ok_or(InsertError::ParentNotFound)?;
        let len: usize = parent.children.len();
        if position > len {
            return Err(InsertError::PositionOutOfRange { len, requested: position });
        }
        parent.children.insert(position, node);
        Ok(())
    }

    // invalidate_index
    // Stage 3 has no per-slide index (see SPEC §5.6 — added in a later
    // stage). The hook lives here now so commands can call it without
    // conditional code; it becomes meaningful when the index is added.
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
            dirty: false,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum InsertError {
    ParentNotFound,
    PositionOutOfRange { len: usize, requested: usize },
}

// find_parent_path
// Inputs: a tree root and a target id to locate (target must not equal
// root id).
// Output: (path-of-indices-to-parent, position-of-target-within-parent),
// or None if the id is absent.
// Dataflow: iterative DFS with an explicit (path, node) stack; each entry
// holds the indices needed to descend back to that node from the root.
fn find_parent_path(root: &ElementNode, target: &str) -> Option<(Vec<usize>, usize)> {
    assert!(!target.is_empty(), "find_parent_path called with empty target");
    let mut stack: Vec<(Vec<usize>, &ElementNode)> = Vec::new();
    stack.push((Vec::new(), root));
    for _ in 0..MAX_TREE_NODES {
        let Some((path, node)) = stack.pop() else { return None; };
        for (i, child) in node.children.iter().enumerate() {
            if child.id == target {
                return Some((path, i));
            }
        }
        for (i, child) in node.children.iter().enumerate().rev() {
            let mut child_path: Vec<usize> = path.clone();
            child_path.push(i);
            stack.push((child_path, child));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::builders::{group_element, text_element};

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
        let err = slide.insert_child("nope", 0, text_element("b", "y")).unwrap_err();
        assert_eq!(err, InsertError::ParentNotFound);
    }

    #[test]
    fn insert_child_rejects_out_of_range_position() {
        let root = group_element("rt", vec![text_element("a", "x")]);
        let mut slide = SlideNode::new("s".into(), "title".into(), root);
        let err = slide.insert_child("rt", 99, text_element("b", "y")).unwrap_err();
        assert_eq!(err, InsertError::PositionOutOfRange { len: 1, requested: 99 });
    }

    #[test]
    fn remove_then_insert_at_same_position_round_trips_subtree() {
        let kids = vec![text_element("a", "x"), text_element("b", "y"), text_element("c", "z")];
        let root = group_element("rt", kids);
        let mut slide = SlideNode::new("s".into(), "title".into(), root);
        let removed = slide.remove_non_root_element("b").unwrap();
        slide.insert_child(&removed.parent_id, removed.position, removed.node).unwrap();
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
