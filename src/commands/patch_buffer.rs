// Patch buffer.
//
// `PatchBuffer` accumulates patches emitted by commands within a single
// event-loop iteration. Draining the buffer applies §8.4 coalescing:
// repeated `SetStyle` or `SetAttribute` writes targeting the same
// (element_id, property|attribute) keep only the last write; all other
// patches pass through in their original order.
//
// The buffer itself does not know how to flush. The owner (ApplicationCore)
// checks `add()`'s return value — true means the buffer transitioned from
// empty to non-empty — and posts a UserEvent::FlushPatches so the flush
// happens on the next event-loop iteration (per SPEC §8.4 / Stage 4
// debugging note about deferring flush).

use crate::ipc::{ElementId, Patch};
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct PatchBuffer {
    pending: Vec<Patch>,
}

impl PatchBuffer {
    // new
    // Inputs: none.
    // Output: an empty PatchBuffer.
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    // is_empty
    // Inputs: self.
    // Output: true if no patches are buffered.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    // len
    // Inputs: self.
    // Output: count of buffered (un-coalesced) patches.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    // add
    // Inputs: a vector of patches produced by a command.
    // Output: true if the buffer was empty before this call and is now
    // non-empty (the signal callers use to schedule a flush).
    // Dataflow: append-only; coalescing is deferred to drain time so
    // multiple add() calls in one iteration can still cancel each other.
    pub fn add(&mut self, patches: Vec<Patch>) -> bool {
        let was_empty: bool = self.pending.is_empty();
        if patches.is_empty() {
            return false;
        }
        self.pending.extend(patches);
        was_empty && !self.pending.is_empty()
    }

    // take_coalesced
    // Inputs: self.
    // Output: the buffered patches in original order, with redundant
    // SetStyle/SetAttribute writes elided. The buffer is left empty.
    pub fn take_coalesced(&mut self) -> Vec<Patch> {
        let raw: Vec<Patch> = std::mem::take(&mut self.pending);
        coalesce_patches(raw)
    }
}

// CoalesceKind
// Tag bytes used to keep SetStyle and SetAttribute namespaces distinct
// inside the dedup map. Using a `&'static str` avoids allocating per key.
const STYLE_KIND: &str = "style";
const ATTR_KIND: &str = "attr";

// coalesce_patches
// Inputs: the raw pending vector.
// Output: filtered vector where any (element, property) reached by
// multiple SetStyle writes retains only the last; same for SetAttribute.
// Dataflow:
//   pass 1: scan left-to-right, remember the last index touching each key.
//   pass 2: keep only patches whose index is the last for their key (or
//           are not coalescable at all).
// Non-coalescable variants (SetText, Remove, Insert, Batch, etc.) pass
// through untouched. Ordering is preserved.
fn coalesce_patches(patches: Vec<Patch>) -> Vec<Patch> {
    let n: usize = patches.len();
    if n <= 1 {
        return patches;
    }
    let mut last_index: HashMap<(ElementId, String, &'static str), usize> =
        HashMap::with_capacity(n);
    let mut keep: Vec<bool> = vec![true; n];
    for (i, patch) in patches.iter().enumerate() {
        let key_opt: Option<(ElementId, String, &'static str)> = match patch {
            Patch::SetStyle {
                element_id,
                property,
                ..
            } => Some((element_id.clone(), property.clone(), STYLE_KIND)),
            Patch::SetAttribute {
                element_id,
                attribute,
                ..
            } => Some((element_id.clone(), attribute.clone(), ATTR_KIND)),
            _ => None,
        };
        if let Some(key) = key_opt {
            if let Some(&prev) = last_index.get(&key) {
                keep[prev] = false;
            }
            last_index.insert(key, i);
        }
    }
    let mut out: Vec<Patch> = Vec::with_capacity(n);
    for (i, patch) in patches.into_iter().enumerate() {
        if keep[i] {
            out.push(patch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn set_style(id: &str, prop: &str, val: &str) -> Patch {
        Patch::SetStyle {
            element_id: id.into(),
            property: prop.into(),
            value: val.into(),
        }
    }

    fn set_attr(id: &str, attr: &str, val: &str) -> Patch {
        Patch::SetAttribute {
            element_id: id.into(),
            attribute: attr.into(),
            value: val.into(),
        }
    }

    fn set_text(id: &str, t: &str) -> Patch {
        Patch::SetText {
            element_id: id.into(),
            text: t.into(),
            src: None,
        }
    }

    #[test]
    fn empty_buffer_is_empty() {
        let mut buf = PatchBuffer::new();
        assert!(buf.is_empty());
        assert_eq!(buf.take_coalesced(), vec![]);
    }

    #[test]
    fn adding_empty_vec_does_not_signal_flush() {
        let mut buf = PatchBuffer::new();
        assert!(!buf.add(vec![]));
        assert!(buf.is_empty());
    }

    #[test]
    fn add_returns_true_on_empty_to_nonempty_transition() {
        let mut buf = PatchBuffer::new();
        assert!(buf.add(vec![set_text("a", "x")]));
    }

    #[test]
    fn add_returns_false_on_subsequent_nonempty_adds() {
        let mut buf = PatchBuffer::new();
        assert!(buf.add(vec![set_text("a", "x")]));
        assert!(!buf.add(vec![set_text("b", "y")]));
    }

    #[test]
    fn coalesce_keeps_only_last_set_style_for_same_property() {
        let patches = vec![
            set_style("a", "left", "100px"),
            set_style("a", "left", "200px"),
            set_style("a", "top", "50px"),
        ];
        let mut buf = PatchBuffer::new();
        buf.add(patches);
        let out = buf.take_coalesced();
        assert_eq!(out.len(), 2);
        // Last left write survives; top write also.
        match &out[0] {
            Patch::SetStyle {
                property, value, ..
            } => {
                assert_eq!(property, "left");
                assert_eq!(value, "200px");
            }
            other => panic!("expected SetStyle, got {other:?}"),
        }
        match &out[1] {
            Patch::SetStyle {
                property, value, ..
            } => {
                assert_eq!(property, "top");
                assert_eq!(value, "50px");
            }
            other => panic!("expected SetStyle, got {other:?}"),
        }
    }

    #[test]
    fn coalesce_preserves_order_after_dedup() {
        let patches = vec![
            set_style("a", "left", "100px"),
            set_style("b", "top", "50px"),
            set_style("a", "left", "200px"),
        ];
        let mut buf = PatchBuffer::new();
        buf.add(patches);
        let out = buf.take_coalesced();
        // b/top was second in input; after dedup the surviving a/left is
        // the third input, so order becomes [b/top, a/left(200)].
        assert_eq!(out.len(), 2);
        match &out[0] {
            Patch::SetStyle {
                element_id,
                property,
                ..
            } => {
                assert_eq!(element_id, "b");
                assert_eq!(property, "top");
            }
            _ => panic!(),
        }
        match &out[1] {
            Patch::SetStyle {
                element_id,
                property,
                value,
            } => {
                assert_eq!(element_id, "a");
                assert_eq!(property, "left");
                assert_eq!(value, "200px");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn coalesce_keeps_last_set_attribute_per_key() {
        let patches = vec![
            set_attr("a", "data-x", "1"),
            set_attr("a", "data-x", "2"),
            set_attr("a", "data-y", "3"),
        ];
        let mut buf = PatchBuffer::new();
        buf.add(patches);
        let out = buf.take_coalesced();
        assert_eq!(out.len(), 2);
        match &out[0] {
            Patch::SetAttribute {
                attribute, value, ..
            } => {
                assert_eq!(attribute, "data-x");
                assert_eq!(value, "2");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn coalesce_does_not_collapse_style_for_different_elements() {
        let patches = vec![
            set_style("a", "left", "100px"),
            set_style("b", "left", "200px"),
        ];
        let mut buf = PatchBuffer::new();
        buf.add(patches);
        let out = buf.take_coalesced();
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn coalesce_passes_through_set_text_and_remove() {
        let patches = vec![
            set_text("a", "first"),
            set_text("a", "second"),
            Patch::RemoveElement {
                element_id: "b".into(),
            },
        ];
        let mut buf = PatchBuffer::new();
        buf.add(patches);
        let out = buf.take_coalesced();
        // SetText is not coalesced — both writes survive; RemoveElement
        // also passes through.
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn coalesce_handles_interleaved_style_and_text() {
        let patches = vec![
            set_style("a", "left", "100px"),
            set_text("a", "hi"),
            set_style("a", "left", "200px"),
        ];
        let mut buf = PatchBuffer::new();
        buf.add(patches);
        let out = buf.take_coalesced();
        // Only the latest left=200 SetStyle survives, plus the SetText.
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn coalesce_single_patch_is_returned_verbatim() {
        let mut buf = PatchBuffer::new();
        buf.add(vec![set_style("a", "left", "100px")]);
        let out = buf.take_coalesced();
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn take_coalesced_empties_buffer() {
        let mut buf = PatchBuffer::new();
        buf.add(vec![set_style("a", "left", "100px")]);
        let _ = buf.take_coalesced();
        assert!(buf.is_empty());
    }

    #[test]
    fn second_add_after_drain_signals_flush_again() {
        let mut buf = PatchBuffer::new();
        buf.add(vec![set_text("a", "x")]);
        let _ = buf.take_coalesced();
        assert!(buf.add(vec![set_text("b", "y")]));
    }

    #[test]
    fn coalesce_keeps_batch_variant_untouched() {
        let inner = vec![set_style("c", "top", "10px")];
        let patches = vec![
            set_style("a", "left", "1px"),
            Patch::Batch {
                patches: inner.clone(),
            },
            set_style("a", "left", "2px"),
        ];
        let mut buf = PatchBuffer::new();
        buf.add(patches);
        let out = buf.take_coalesced();
        // Batch is opaque — coalescing doesn't peek inside, so the Batch
        // itself stays, and the latest a/left=2px wins between the outer
        // SetStyle pair.
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], Patch::Batch { .. }));
    }
}
