// relayout_patches — relayout an element's ancestor groups and emit the DOM
// patches (left/top/width/height SetStyle) for every node whose geometry
// changed, so the editor reflects shrink-wrap without a full remount.

use crate::deck::canvas::find_element;
use crate::deck::element::ElementNode;
use crate::deck::group_layout::relayout_ancestors;
use crate::deck::style::Geometry;
use crate::ipc::Patch;
use std::collections::BTreeMap;

// snapshot_geom — map of id → geometry for `node` and all descendants.
// Iterative, fixed ceiling.
fn snapshot_geom(node: &ElementNode) -> BTreeMap<String, Geometry> {
    const MAX_NODES: usize = 1_000_000;
    let mut out: BTreeMap<String, Geometry> = BTreeMap::new();
    let mut stack: Vec<&ElementNode> = vec![node];
    let mut seen: usize = 0;
    while let Some(n) = stack.pop() {
        seen += 1;
        assert!(seen <= MAX_NODES, "snapshot_geom: node ceiling");
        out.insert(n.id.clone(), n.geometry.clone());
        for c in &n.children {
            stack.push(c);
        }
    }
    out
}

// geom_patches — for a node, diff its subtree geometry against `before` and emit
// SetStyle left/top/width/height patches for changed nodes.
fn geom_patches(node: &ElementNode, before: &BTreeMap<String, Geometry>) -> Vec<Patch> {
    const MAX_NODES: usize = 1_000_000;
    let mut out: Vec<Patch> = Vec::new();
    let mut stack: Vec<&ElementNode> = vec![node];
    let mut seen: usize = 0;
    while let Some(n) = stack.pop() {
        seen += 1;
        assert!(seen <= MAX_NODES, "geom_patches: node ceiling");
        let prev = before.get(&n.id);
        let g = &n.geometry;
        if prev.map(|p| p.x != g.x).unwrap_or(true) {
            out.push(set_style(&n.id, "left", g.x));
        }
        if prev.map(|p| p.y != g.y).unwrap_or(true) {
            out.push(set_style(&n.id, "top", g.y));
        }
        if prev.map(|p| p.width != g.width).unwrap_or(true) {
            out.push(set_style(&n.id, "width", g.width));
        }
        if prev.map(|p| p.height != g.height).unwrap_or(true) {
            out.push(set_style(&n.id, "height", g.height));
        }
        for c in &n.children {
            stack.push(c);
        }
    }
    out
}

fn set_style(id: &str, prop: &str, v: f64) -> Patch {
    Patch::SetStyle {
        element_id: id.to_string(),
        property: prop.to_string(),
        value: format!("{}px", v),
    }
}

// relayout_patches
// Inputs: root (mutated), the edited element id.
// Output: SetStyle patches for every node whose geometry changed after
// relayouting the element's ancestor groups. Empty when the element has no
// group ancestor or nothing changed.
pub fn relayout_patches(root: &mut ElementNode, element_id: &str) -> Vec<Patch> {
    let anchor: String = match find_element(root, element_id) {
        Some(_) => element_id.to_string(),
        None => return Vec::new(),
    };
    let before: BTreeMap<String, Geometry> = snapshot_geom(root);
    let changed: bool = relayout_ancestors(root, &anchor);
    if !changed {
        return Vec::new();
    }
    geom_patches(root, &before)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::deck::builders::{group_element, text_element};
    use crate::deck::element::ElementStyle;
    use crate::deck::style::{GroupDistribution, GroupStyle};

    #[test]
    fn relayout_patches_emit_for_changed_nodes() {
        let mut a = text_element("a", "t");
        a.geometry.x = 0.0;
        a.geometry.width = 20.0;
        a.geometry.height = 10.0;
        let mut b = text_element("b", "t");
        b.geometry.x = 80.0;
        b.geometry.width = 20.0;
        b.geometry.height = 10.0;
        let mut c = text_element("c", "t");
        c.geometry.x = 50.0;
        c.geometry.width = 20.0;
        c.geometry.height = 10.0;
        let mut g = group_element("g", vec![a, c, b]);
        g.style = ElementStyle::Group(GroupStyle {
            distribution: GroupDistribution::SpaceBetween,
            ..Default::default()
        });
        // The flex group sits under a structural root (the slide root is never
        // a user flex group); relayout against that root.
        let mut root = group_element("root", vec![g]);
        let patches = relayout_patches(&mut root, "a");
        // c moved 50 -> 40, so at least one SetStyle left patch for c exists.
        let has_c_left = patches.iter().any(|p| {
            matches!(p,
            Patch::SetStyle { element_id, property, value }
                if element_id == "c" && property == "left" && value == "40px")
        });
        assert!(has_c_left);
    }
}
