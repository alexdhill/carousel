use crate::deck::element::ElementNode;
use crate::deck::ids::ElementId;

pub const MAX_TREE_NODES: usize = 100_000;
pub const MAX_TREE_DEPTH: usize = 1024;

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

pub trait Canvas {
    fn root(&self) -> &ElementNode;
    fn root_mut(&mut self) -> &mut ElementNode;
    fn mark_dirty(&mut self);

    fn guides(&self) -> &Vec<crate::deck::guide::Guide>;
    fn guides_mut(&mut self) -> &mut Vec<crate::deck::guide::Guide>;

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

    fn invalidate_index(&mut self) {}
}

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

/// Finds the node whose children contain `id`.
///
/// Input: the tree root and a non-empty element id. Output: the parent node,
/// or `None` when `id` is the root or is absent. Control flow: iterative
/// depth-first walk bounded by `MAX_TREE_NODES`.
pub fn find_parent<'a>(root: &'a ElementNode, id: &str) -> Option<&'a ElementNode> {
    assert!(!id.is_empty(), "find_parent called with empty id");
    let mut stack: Vec<&'a ElementNode> = Vec::new();
    stack.push(root);
    for _ in 0..MAX_TREE_NODES {
        let node = stack.pop()?;
        if node.children.iter().any(|c| c.id == id) {
            return Some(node);
        }
        for child in node.children.iter().rev() {
            stack.push(child);
        }
    }
    None
}

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
