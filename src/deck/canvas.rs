// Canvas: the editable element-tree surface shared by slides and layouts.
//
// A canvas owns a single `root` ElementNode (always a Group) whose children
// are the top-level elements. Both `SlideNode` and `LayoutNode` implement
// `Canvas`, so the element commands can edit either through one interface
// (resolved via `Deck::canvas_mut` and a `CanvasTarget`).
//
// The depth-first tree operations live here as free functions so there is a
// single source of truth; `SlideNode` keeps thin inherent wrappers (for the
// many existing call sites that hold a concrete `SlideNode`) and the trait
// provides the same operations to `&mut dyn Canvas` holders.

use crate::deck::element::ElementNode;
use crate::deck::ids::ElementId;

// Upper bound on tree depth and total node count for any bounded loop here.
// Commands enforce a stricter group nesting limit at the model level
// (SPEC §5.9 = 10); these exist only so the bounded-loop requirement
// (CLAUDE.md) has a concrete ceiling well above any plausible canvas.
pub const MAX_TREE_NODES: usize = 100_000;
pub const MAX_TREE_DEPTH: usize = 1024;

// RemovedElement
// Returned by `remove_non_root_element`. Carries enough to re-insert the
// subtree at the same location, which is what the inverse of RemoveElement
// needs.
#[derive(Clone, Debug, PartialEq)]
pub struct RemovedElement {
    pub node: ElementNode,
    pub parent_id: ElementId,
    pub position: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum InsertError {
    ParentNotFound,
    PositionOutOfRange { len: usize, requested: usize },
}

// Canvas
// The shared editable surface. Required: access to the root and a dirty
// hook. Provided: the tree operations, delegating to the free functions
// below so slides and layouts share one implementation.
pub trait Canvas {
    fn root(&self) -> &ElementNode;
    fn root_mut(&mut self) -> &mut ElementNode;
    fn mark_dirty(&mut self);

    fn is_root_id(&self, id: &str) -> bool {
        self.root().id == id
    }
    fn find_element(&self, id: &str) -> Option<&ElementNode> {
        find_element(self.root(), id)
    }
    fn find_element_mut(&mut self, id: &str) -> Option<&mut ElementNode> {
        find_element_mut(self.root_mut(), id)
    }
    fn remove_non_root_element(&mut self, id: &str) -> Option<RemovedElement> {
        let is_root: bool = self.is_root_id(id);
        assert!(!is_root, "remove_non_root_element called with root id");
        remove_non_root_element(self.root_mut(), id)
    }
    fn insert_child(
        &mut self,
        parent_id: &str,
        position: usize,
        node: ElementNode,
    ) -> Result<(), InsertError> {
        insert_child(self.root_mut(), parent_id, position, node)
    }
    // No per-canvas index yet (SPEC §5.6 — added later). The hook lives
    // here so commands can call it unconditionally.
    fn invalidate_index(&mut self) {}
}

// find_element
// Inputs: a tree root and an element id to locate.
// Output: an immutable reference to the matching ElementNode, or None.
// Dataflow: iterative depth-first search with an explicit stack, bounded by
// MAX_TREE_NODES.
pub fn find_element<'a>(root: &'a ElementNode, id: &str) -> Option<&'a ElementNode> {
    assert!(!id.is_empty(), "find_element called with empty id");
    let mut stack: Vec<&'a ElementNode> = Vec::new();
    stack.push(root);
    for _ in 0..MAX_TREE_NODES {
        let node = stack.pop()?;
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
// Inputs: a tree root and an element id to locate.
// Output: a mutable reference to the matching ElementNode, or None.
// Dataflow: same iterative DFS as find_element with a mutable stack; the
// pop-then-push pattern is sound because children are disjoint subtrees.
pub fn find_element_mut<'a>(root: &'a mut ElementNode, id: &str) -> Option<&'a mut ElementNode> {
    assert!(!id.is_empty(), "find_element_mut called with empty id");
    let mut stack: Vec<&'a mut ElementNode> = Vec::new();
    stack.push(root);
    for _ in 0..MAX_TREE_NODES {
        let node = stack.pop()?;
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
// Inputs: a tree root and an element id (must not equal the root id).
// Output: the removed subtree + its parent id + prior position, or None if
// the id is absent.
// Dataflow: immutable DFS records the index-path to the target's parent,
// then a bounded mutable walk lands on the parent and removes the child.
pub fn remove_non_root_element(root: &mut ElementNode, id: &str) -> Option<RemovedElement> {
    assert!(
        !id.is_empty(),
        "remove_non_root_element called with empty id"
    );
    let (path, position): (Vec<usize>, usize) = find_parent_path(root, id)?;
    let mut current: &mut ElementNode = root;
    for (step, &idx) in path.iter().enumerate() {
        assert!(step < MAX_TREE_DEPTH, "tree depth exceeded MAX_TREE_DEPTH");
        current = &mut current.children[idx];
    }
    let parent_id: ElementId = current.id.clone();
    let removed: ElementNode = current.children.remove(position);
    Some(RemovedElement {
        node: removed,
        parent_id,
        position,
    })
}

// insert_child
// Inputs: a tree root, parent id, 0-indexed position, the node to insert.
// Output: Ok(()) on success.
// Errors: parent absent (ParentNotFound) or position > parent len
// (PositionOutOfRange).
pub fn insert_child(
    root: &mut ElementNode,
    parent_id: &str,
    position: usize,
    node: ElementNode,
) -> Result<(), InsertError> {
    assert!(
        !parent_id.is_empty(),
        "insert_child called with empty parent_id"
    );
    let parent: &mut ElementNode =
        find_element_mut(root, parent_id).ok_or(InsertError::ParentNotFound)?;
    let len: usize = parent.children.len();
    if position > len {
        return Err(InsertError::PositionOutOfRange {
            len,
            requested: position,
        });
    }
    parent.children.insert(position, node);
    Ok(())
}

// find_parent_path
// Inputs: a tree root and a target id (must not equal root id).
// Output: (index-path-to-parent, position-of-target-in-parent), or None.
// Dataflow: iterative DFS with an explicit (path, node) stack; each entry
// records the indices needed to descend back to that node from the root.
fn find_parent_path(root: &ElementNode, target: &str) -> Option<(Vec<usize>, usize)> {
    assert!(
        !target.is_empty(),
        "find_parent_path called with empty target"
    );
    let mut stack: Vec<(Vec<usize>, &ElementNode)> = Vec::new();
    stack.push((Vec::new(), root));
    for _ in 0..MAX_TREE_NODES {
        let (path, node) = stack.pop()?;
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
