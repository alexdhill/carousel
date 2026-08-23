use crate::commands::{Command, CommandError, CommandOutput, resolve_canvas_mut};
use crate::deck::builders::group_element;
use crate::deck::canvas::RemovedElement;
use crate::deck::element::ElementNode;
use crate::deck::group_layout::relayout_ancestors;
use crate::deck::{Canvas, CanvasTarget, ElementId};

#[derive(Debug, Clone)]
pub struct GroupElements {
    pub target: CanvasTarget,
    pub group_id: ElementId,
    pub element_ids: Vec<ElementId>,
}

fn parent_of(root: &ElementNode, id: &str) -> Option<String> {
    const MAX_NODES: usize = 1_000_000;
    let mut stack: Vec<&ElementNode> = vec![root];
    let mut seen: usize = 0;
    while let Some(n) = stack.pop() {
        seen += 1;
        assert!(seen <= MAX_NODES, "parent_of: node ceiling");
        if n.children.iter().any(|c| c.id == id) {
            return Some(n.id.clone());
        }
        for c in &n.children {
            stack.push(c);
        }
    }
    None
}

impl Command for GroupElements {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.group_id.is_empty(), "GroupElements: empty group_id");
        assert!(
            self.element_ids.len() >= 2,
            "GroupElements: need >= 2 members"
        );
        let canvas = resolve_canvas_mut(deck, &self.target)?;

        let parent_id: String = match parent_of(canvas.root(), &self.element_ids[0]) {
            Some(p) => p,
            None => return Err(CommandError::ElementNotFound(self.element_ids[0].clone())),
        };
        for id in &self.element_ids {
            if canvas.is_root_id(id) {
                return Err(CommandError::InvalidOperation(
                    "GroupElements: cannot group the root".into(),
                ));
            }
            match parent_of(canvas.root(), id) {
                Some(p) if p == parent_id => {}
                Some(_) => {
                    return Err(CommandError::InvalidOperation(
                        "GroupElements: members must share one parent".into(),
                    ));
                }
                None => return Err(CommandError::ElementNotFound(id.clone())),
            }
        }

        let mut originals: Vec<(usize, ElementNode)> = Vec::new();
        for id in &self.element_ids {
            let RemovedElement { node, position, .. } = canvas
                .remove_non_root_element(id)
                .ok_or_else(|| CommandError::ElementNotFound(id.clone()))?;
            originals.push((position, node));
        }

        originals.sort_by_key(|(p, _)| *p);
        let max_pos: usize = originals.iter().map(|(p, _)| *p).max().unwrap_or(0);
        let insert_index: usize = max_pos.saturating_sub(self.element_ids.len() - 1);

        let children: Vec<ElementNode> = originals.iter().map(|(_, n)| n.clone()).collect();
        let group_node: ElementNode = group_element(self.group_id.clone(), children);

        let parent_len: usize = canvas
            .find_element(&parent_id)
            .map(|p| p.children.len())
            .unwrap_or(0);
        let idx: usize = insert_index.min(parent_len);
        canvas
            .insert_child(&parent_id, idx, group_node)
            .map_err(|_| CommandError::InvalidOperation("GroupElements: insert failed".into()))?;

        relayout_ancestors(canvas.root_mut(), &self.group_id);
        canvas.mark_dirty();
        canvas.invalidate_index();

        let inverse = DissolveGroup {
            target: self.target.clone(),
            parent_id,
            group_id: self.group_id.clone(),
            originals,
        };
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(inverse),
            dirty_targets: vec![self.target.clone()],
            manifest_dirty: false,
            warnings: Vec::new(),
        })
    }
    fn label(&self) -> &'static str {
        "Group Elements"
    }
    fn affects_object_tree(&self) -> bool {
        true
    }
    fn requires_remount(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone)]
pub struct DissolveGroup {
    pub target: CanvasTarget,
    pub parent_id: ElementId,
    pub group_id: ElementId,
    pub originals: Vec<(usize, ElementNode)>,
}

impl Command for DissolveGroup {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.group_id.is_empty(), "DissolveGroup: empty group_id");
        let canvas = resolve_canvas_mut(deck, &self.target)?;
        canvas
            .remove_non_root_element(&self.group_id)
            .ok_or_else(|| CommandError::ElementNotFound(self.group_id.clone()))?;

        for (pos, node) in &self.originals {
            canvas
                .insert_child(&self.parent_id, *pos, node.clone())
                .map_err(|_| {
                    CommandError::InvalidOperation("DissolveGroup: insert failed".into())
                })?;
        }

        relayout_ancestors(canvas.root_mut(), &self.parent_id);
        canvas.mark_dirty();
        canvas.invalidate_index();

        let inverse = GroupElements {
            target: self.target.clone(),
            group_id: self.group_id.clone(),
            element_ids: self.originals.iter().map(|(_, n)| n.id.clone()).collect(),
        };
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(inverse),
            dirty_targets: vec![self.target.clone()],
            manifest_dirty: false,
            warnings: Vec::new(),
        })
    }
    fn label(&self) -> &'static str {
        "Ungroup Elements"
    }
    fn affects_object_tree(&self) -> bool {
        true
    }
    fn requires_remount(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::deck::builders::text_element;
    use crate::deck::slide::SlideNode;
    use crate::deck::{Deck, SlideId};
    use std::collections::BTreeMap;

    fn deck_with(children: Vec<ElementNode>) -> Deck {
        let root = group_element("el_root", children);
        let slide = SlideNode::new("s".into(), "t".into(), root);
        let mut slides: BTreeMap<SlideId, SlideNode> = BTreeMap::new();
        slides.insert("s".into(), slide);
        Deck {
            slides,
            slide_order: vec!["s".into()],
            ..Default::default()
        }
    }
    fn kid(id: &str, x: f64, y: f64, w: f64, h: f64) -> ElementNode {
        let mut n = text_element(id, "t");
        n.geometry.x = x;
        n.geometry.y = y;
        n.geometry.width = w;
        n.geometry.height = h;
        n
    }

    #[test]
    fn groups_siblings_and_shrinkwraps_at_top_slot() {
        let mut deck = deck_with(vec![
            kid("a", 10.0, 10.0, 20.0, 10.0),
            kid("b", 60.0, 40.0, 20.0, 10.0),
            kid("c", 0.0, 0.0, 5.0, 5.0),
        ]);
        GroupElements {
            target: CanvasTarget::Slide("s".into()),
            group_id: "grp".into(),
            element_ids: vec!["a".into(), "b".into()],
        }
        .apply(&mut deck)
        .unwrap();

        let sid: SlideId = "s".into();
        let root = &deck.slides[&sid].root;

        assert!(root.children.iter().any(|n| n.id == "grp"));
        assert!(root.children.iter().any(|n| n.id == "c"));
        let g = root.children.iter().find(|n| n.id == "grp").unwrap();

        assert_eq!(g.geometry.width, 70.0);
        assert_eq!(g.geometry.height, 40.0);
        assert_eq!(g.geometry.x, 10.0);
        assert_eq!(g.geometry.y, 10.0);

        let a = g.children.iter().find(|n| n.id == "a").unwrap();
        let b = g.children.iter().find(|n| n.id == "b").unwrap();
        assert_eq!((a.geometry.x, a.geometry.y), (0.0, 0.0));
        assert_eq!((b.geometry.x, b.geometry.y), (50.0, 30.0));
    }

    #[test]
    fn inverse_restores_original_siblings() {
        let mut deck = deck_with(vec![
            kid("a", 10.0, 10.0, 20.0, 10.0),
            kid("b", 60.0, 40.0, 20.0, 10.0),
        ]);
        let out = GroupElements {
            target: CanvasTarget::Slide("s".into()),
            group_id: "grp".into(),
            element_ids: vec!["a".into(), "b".into()],
        }
        .apply(&mut deck)
        .unwrap();
        out.inverse.apply(&mut deck).unwrap();
        let sid: SlideId = "s".into();
        let root = &deck.slides[&sid].root;
        assert!(!root.children.iter().any(|n| n.id == "grp"));
        let a = root.children.iter().find(|n| n.id == "a").unwrap();
        assert_eq!((a.geometry.x, a.geometry.y), (10.0, 10.0));
        assert_eq!(root.children.len(), 2);
    }

    #[test]
    fn rejects_cross_parent_members() {
        let g1 = group_element("g1", vec![kid("a", 0.0, 0.0, 5.0, 5.0)]);
        let mut deck = deck_with(vec![g1, kid("b", 0.0, 0.0, 5.0, 5.0)]);
        let err = GroupElements {
            target: CanvasTarget::Slide("s".into()),
            group_id: "grp".into(),
            element_ids: vec!["a".into(), "b".into()],
        }
        .apply(&mut deck)
        .unwrap_err();
        assert!(matches!(err, CommandError::InvalidOperation(_)));
    }
}
