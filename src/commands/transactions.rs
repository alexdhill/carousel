// Transactions.
//
// SPEC §9.4–9.5. A `Transaction` groups a stream of related commands into a
// single editor-level operation. The interpretation layer opens one on
// ElementDragStarted and closes it on ElementDragEnded; intermediate
// ElementDragged events apply MoveElement commands inside the transaction.
//
// At Stage 5, transactions exist but do not produce a history entry on
// commit — there is no history stack yet (it lands in Stage 6). The
// snapshot captured at begin time, together with the accumulated patches
// and dirty slide set, is the data that Stage 6 will fold into a composite
// inverse command.

use crate::deck::style::Geometry;
use crate::deck::{ElementId, SlideId};
use crate::ipc::Patch;
use std::collections::{HashMap, HashSet};

// TransactionSnapshot
// Records pre-transaction state for the fields a transaction's commands
// could touch. For a drag transaction only `geometry` is populated; for a
// text-edit transaction only `content`. The two maps are kept separate so
// snapshots stay tight (no need to clone a full element tree).
//
// Keyed on (SlideId, ElementId) because a transaction could in principle
// span multiple slides if a future command type allowed it.
#[derive(Debug, Default, Clone)]
pub struct TransactionSnapshot {
    pub geometry: HashMap<(SlideId, ElementId), Geometry>,
    pub content: HashMap<(SlideId, ElementId), crate::deck::element::ElementContent>,
}

impl TransactionSnapshot {
    // empty
    // Inputs: none.
    // Output: a TransactionSnapshot with both maps empty.
    pub fn empty() -> Self {
        Self::default()
    }

    // record_geometry
    // Inputs: slide id, element id, the geometry to remember.
    // Output: side-effect; stores the (x, y, w, h, ...) at transaction
    // start so the inverse command can restore it later.
    pub fn record_geometry(
        &mut self,
        slide_id: SlideId,
        element_id: ElementId,
        geometry: Geometry,
    ) {
        assert!(!slide_id.is_empty(), "record_geometry: slide_id is empty");
        assert!(!element_id.is_empty(), "record_geometry: element_id is empty");
        self.geometry.insert((slide_id, element_id), geometry);
    }

    // position_of
    // Inputs: slide id, element id.
    // Output: the (x, y) at transaction start as a tuple, or None if not
    // recorded. ElementDragged handlers use this to compute the new
    // absolute position from the cumulative drag delta.
    pub fn position_of(&self, slide_id: &str, element_id: &str) -> Option<(f64, f64)> {
        let key: (SlideId, ElementId) = (slide_id.to_string(), element_id.to_string());
        let g: &Geometry = self.geometry.get(&key)?;
        Some((g.x, g.y))
    }
}

// Transaction
// Active state for a single in-flight transaction. `patches` accumulates
// every patch the dispatcher emits while this transaction is open so the
// commit step (Stage 6) can build a single composite inverse and a single
// history entry. `dirty_slides` collects the union of slides touched.
#[derive(Debug)]
pub struct Transaction {
    pub label: &'static str,
    pub start_snapshot: TransactionSnapshot,
    pub patches: Vec<Patch>,
    pub dirty_slides: HashSet<SlideId>,
}

impl Transaction {
    // new
    // Inputs: a static label (e.g., "Move Element"), the start snapshot.
    // Output: a Transaction with empty patch and dirty-slide accumulators.
    pub fn new(label: &'static str, snapshot: TransactionSnapshot) -> Self {
        assert!(!label.is_empty(), "transaction label must not be empty");
        Self {
            label,
            start_snapshot: snapshot,
            patches: Vec::new(),
            dirty_slides: HashSet::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::style::Geometry;

    fn geom(x: f64, y: f64) -> Geometry {
        Geometry { x, y, ..Default::default() }
    }

    #[test]
    fn snapshot_default_is_empty() {
        let s = TransactionSnapshot::empty();
        assert!(s.geometry.is_empty());
        assert!(s.content.is_empty());
        assert!(s.position_of("any", "any").is_none());
    }

    #[test]
    fn snapshot_records_and_retrieves_position() {
        let mut s = TransactionSnapshot::empty();
        s.record_geometry("s1".into(), "el_a".into(), geom(10.0, 20.0));
        assert_eq!(s.position_of("s1", "el_a"), Some((10.0, 20.0)));
    }

    #[test]
    fn snapshot_position_of_returns_none_for_unrecorded_keys() {
        let mut s = TransactionSnapshot::empty();
        s.record_geometry("s1".into(), "el_a".into(), geom(1.0, 2.0));
        assert!(s.position_of("s2", "el_a").is_none());
        assert!(s.position_of("s1", "el_b").is_none());
    }

    #[test]
    fn snapshot_supports_multiple_elements_in_same_slide() {
        let mut s = TransactionSnapshot::empty();
        s.record_geometry("s1".into(), "el_a".into(), geom(1.0, 2.0));
        s.record_geometry("s1".into(), "el_b".into(), geom(3.0, 4.0));
        assert_eq!(s.position_of("s1", "el_a"), Some((1.0, 2.0)));
        assert_eq!(s.position_of("s1", "el_b"), Some((3.0, 4.0)));
    }

    #[test]
    #[should_panic(expected = "slide_id is empty")]
    fn snapshot_record_rejects_empty_slide_id() {
        let mut s = TransactionSnapshot::empty();
        s.record_geometry(String::new(), "el_a".into(), geom(0.0, 0.0));
    }

    #[test]
    fn transaction_new_starts_with_empty_accumulators() {
        let t = Transaction::new("Move Element", TransactionSnapshot::empty());
        assert_eq!(t.label, "Move Element");
        assert!(t.patches.is_empty());
        assert!(t.dirty_slides.is_empty());
    }

    #[test]
    #[should_panic(expected = "label must not be empty")]
    fn transaction_new_rejects_empty_label() {
        let _ = Transaction::new("", TransactionSnapshot::empty());
    }
}
