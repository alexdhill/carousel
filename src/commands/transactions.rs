use crate::deck::style::Geometry;
use crate::deck::{CanvasTarget, ElementId};
use crate::ipc::Patch;
use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct TransactionSnapshot {
    pub geometry: HashMap<(CanvasTarget, ElementId), Geometry>,
    pub content: HashMap<(CanvasTarget, ElementId), crate::deck::element::ElementContent>,
}

impl TransactionSnapshot {

    pub fn empty() -> Self {
        Self::default()
    }

    pub fn record_geometry(
        &mut self,
        target: CanvasTarget,
        element_id: ElementId,
        geometry: Geometry,
    ) {
        assert!(
            !target.id().is_empty(),
            "record_geometry: target id is empty"
        );
        assert!(
            !element_id.is_empty(),
            "record_geometry: element_id is empty"
        );
        self.geometry.insert((target, element_id), geometry);
    }

    pub fn position_of(&self, target: &CanvasTarget, element_id: &str) -> Option<(f64, f64)> {
        let key: (CanvasTarget, ElementId) = (target.clone(), element_id.to_string());
        let g: &Geometry = self.geometry.get(&key)?;
        Some((g.x, g.y))
    }
}

#[derive(Debug)]
pub struct Transaction {
    pub label: &'static str,
    pub start_snapshot: TransactionSnapshot,
    pub patches: Vec<Patch>,
    pub dirty_targets: Vec<CanvasTarget>,
}

impl Transaction {

    pub fn new(label: &'static str, snapshot: TransactionSnapshot) -> Self {
        assert!(!label.is_empty(), "transaction label must not be empty");
        Self {
            label,
            start_snapshot: snapshot,
            patches: Vec::new(),
            dirty_targets: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::style::Geometry;

    fn geom(x: f64, y: f64) -> Geometry {
        Geometry {
            x,
            y,
            ..Default::default()
        }
    }

    fn slide(id: &str) -> CanvasTarget {
        CanvasTarget::Slide(id.to_string())
    }

    #[test]
    fn snapshot_default_is_empty() {
        let s = TransactionSnapshot::empty();
        assert!(s.geometry.is_empty());
        assert!(s.content.is_empty());
        assert!(s.position_of(&slide("any"), "any").is_none());
    }

    #[test]
    fn snapshot_records_and_retrieves_position() {
        let mut s = TransactionSnapshot::empty();
        s.record_geometry(slide("s1"), "el_a".into(), geom(10.0, 20.0));
        assert_eq!(s.position_of(&slide("s1"), "el_a"), Some((10.0, 20.0)));
    }

    #[test]
    fn snapshot_position_of_returns_none_for_unrecorded_keys() {
        let mut s = TransactionSnapshot::empty();
        s.record_geometry(slide("s1"), "el_a".into(), geom(1.0, 2.0));
        assert!(s.position_of(&slide("s2"), "el_a").is_none());
        assert!(s.position_of(&slide("s1"), "el_b").is_none());
    }

    #[test]
    fn snapshot_distinguishes_slide_and_layout_targets() {
        let mut s = TransactionSnapshot::empty();
        s.record_geometry(slide("s1"), "el_a".into(), geom(1.0, 2.0));
        s.record_geometry(
            CanvasTarget::Layout("s1".into()),
            "el_a".into(),
            geom(9.0, 9.0),
        );

        assert_eq!(s.position_of(&slide("s1"), "el_a"), Some((1.0, 2.0)));
        assert_eq!(
            s.position_of(&CanvasTarget::Layout("s1".into()), "el_a"),
            Some((9.0, 9.0))
        );
    }

    #[test]
    fn snapshot_supports_multiple_elements_in_same_slide() {
        let mut s = TransactionSnapshot::empty();
        s.record_geometry(slide("s1"), "el_a".into(), geom(1.0, 2.0));
        s.record_geometry(slide("s1"), "el_b".into(), geom(3.0, 4.0));
        assert_eq!(s.position_of(&slide("s1"), "el_a"), Some((1.0, 2.0)));
        assert_eq!(s.position_of(&slide("s1"), "el_b"), Some((3.0, 4.0)));
    }

    #[test]
    #[should_panic(expected = "target id is empty")]
    fn snapshot_record_rejects_empty_target_id() {
        let mut s = TransactionSnapshot::empty();
        s.record_geometry(slide(""), "el_a".into(), geom(0.0, 0.0));
    }

    #[test]
    fn transaction_new_starts_with_empty_accumulators() {
        let t = Transaction::new("Move Element", TransactionSnapshot::empty());
        assert_eq!(t.label, "Move Element");
        assert!(t.patches.is_empty());
        assert!(t.dirty_targets.is_empty());
    }

    #[test]
    #[should_panic(expected = "label must not be empty")]
    fn transaction_new_rejects_empty_label() {
        let _ = Transaction::new("", TransactionSnapshot::empty());
    }
}
