// Reveal computation — pure.
//
// Folds a slide's animation timeline + a cursor step into a `RevealPayload`:
// the per-element resolved visual state plus, for a forward transition, the set
// of entries that should animate. No webview, no deck ownership — just the
// timeline slice and a slide id, so every rule here is unit-testable.
//
// An element is VISIBLE at `step` iff:
//   (no entrance OR its entrance has fired) AND (no exit OR its exit has NOT fired)
// The animations pass guarantees entrance-before-exit, so this is unambiguous.
// Elements appearing in no entry are static — never listed, left as rendered.

use crate::deck::animation::{
    AnimationCategory, AnimationEntry, AnimationTrigger, entries_through, index_of_category,
};
use crate::ipc::present::{AnimateInstruction, RevealPayload};

// Defensive bound: timelines are tiny in practice, but every loop carries an
// explicit cap (house style: loops have a fixed upper bound).
const MAX_TIMELINE: usize = 4_096;

// snap_reveal
// Inputs: the slide id, its animation timeline, and a target step.
// Output: a RevealPayload whose `animate` is empty — every timeline element is
// resolved into `shown` or `hidden` at `step`. Used for backward steps, the
// initial mount, and cross-slide landings (no animation plays).
pub fn snap_reveal(slide_id: &str, timeline: &[AnimationEntry], step: usize) -> RevealPayload {
    assert!(!slide_id.is_empty(), "snap_reveal: empty slide id");
    let (hidden, shown): (Vec<String>, Vec<String>) =
        resolve_rest(timeline, step, &[]);
    RevealPayload { slide_id: slide_id.into(), hidden, shown, animate: Vec::new() }
}

// forward_reveal
// Inputs: the slide id, its timeline, and the step being advanced INTO
// (`to_step` >= 1).
// Output: a RevealPayload whose `animate` is the newly-fired group (the entries
// that fire on this k-1 -> k transition), each with an effective delay; all
// other timeline elements are resolved into `shown` / `hidden`. An element that
// animates this transition appears ONLY in `animate`.
pub fn forward_reveal(slide_id: &str, timeline: &[AnimationEntry], to_step: usize) -> RevealPayload {
    assert!(!slide_id.is_empty(), "forward_reveal: empty slide id");
    assert!(to_step >= 1, "forward_reveal: to_step must be >= 1");
    let prev_len: usize = entries_through(timeline, to_step - 1).len();
    let fired: &[AnimationEntry] = entries_through(timeline, to_step);
    let group: &[AnimationEntry] = &fired[prev_len..];
    let delays: Vec<u32> = effective_delays(group);

    let mut animate: Vec<AnimateInstruction> = Vec::with_capacity(group.len());
    let mut animating: Vec<&str> = Vec::with_capacity(group.len());
    for (i, e) in group.iter().enumerate() {
        assert!(i < MAX_TIMELINE, "forward_reveal: group bound exceeded");
        animate.push(AnimateInstruction {
            element_id: e.element_id.clone(),
            keyframe: e.keyframe.clone(),
            duration_ms: e.timing.duration_ms,
            delay_ms: delays[i],
            easing: e.timing.easing.clone(),
            iterations: e.timing.iterations,
            ends_hidden: e.category == AnimationCategory::Exit,
        });
        animating.push(e.element_id.as_str());
    }
    let (hidden, shown): (Vec<String>, Vec<String>) =
        resolve_rest(timeline, to_step, &animating);
    RevealPayload { slide_id: slide_id.into(), hidden, shown, animate }
}

// resolve_rest
// Inputs: timeline, the target step, and the element ids already accounted for
// by the animate set (skipped here).
// Output: (hidden, shown) for every other unique timeline element, in
// first-appearance order. Static (non-timeline) elements never appear.
fn resolve_rest(
    timeline: &[AnimationEntry],
    step: usize,
    animating: &[&str],
) -> (Vec<String>, Vec<String>) {
    let mut hidden: Vec<String> = Vec::new();
    let mut shown: Vec<String> = Vec::new();
    for el in unique_elements(timeline) {
        if animating.contains(&el) {
            continue;
        }
        if is_visible(timeline, el, step) {
            shown.push(el.into());
        } else {
            hidden.push(el.into());
        }
    }
    (hidden, shown)
}

// unique_elements
// Output: each element id that appears in the timeline, once, in
// first-appearance order.
fn unique_elements(timeline: &[AnimationEntry]) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for (i, e) in timeline.iter().enumerate() {
        assert!(i < MAX_TIMELINE, "unique_elements: timeline bound exceeded");
        let id: &str = e.element_id.as_str();
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

// is_visible
// Output: true iff `element_id` is visible at `step`:
//   (no entrance OR entrance has fired) AND (no exit OR exit has NOT fired).
fn is_visible(timeline: &[AnimationEntry], element_id: &str, step: usize) -> bool {
    assert!(!element_id.is_empty(), "is_visible: empty element id");
    let fired_len: usize = entries_through(timeline, step).len();
    let en: Option<usize> = index_of_category(timeline, element_id, AnimationCategory::Entrance);
    let ex: Option<usize> = index_of_category(timeline, element_id, AnimationCategory::Exit);
    let entered: bool = en.is_none_or(|i| i < fired_len);
    let exited: bool = ex.is_some_and(|i| i < fired_len);
    entered && !exited
}

// effective_delays
// Inputs: the newly-fired group (timeline order).
// Output: one effective delay per entry. OnClick / WithPrevious entries use
// their own delay; AfterPrevious entries start after all prior entries in the
// group have finished (cumulative delay + single duration), plus their own
// delay. Saturating arithmetic keeps the math total.
fn effective_delays(group: &[AnimationEntry]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::with_capacity(group.len());
    let mut prior_sum: u32 = 0;
    for (i, e) in group.iter().enumerate() {
        assert!(i < MAX_TIMELINE, "effective_delays: group bound exceeded");
        let eff: u32 = match e.trigger {
            AnimationTrigger::OnClick | AnimationTrigger::WithPrevious => e.timing.delay_ms,
            AnimationTrigger::AfterPrevious => prior_sum.saturating_add(e.timing.delay_ms),
        };
        out.push(eff);
        prior_sum = prior_sum
            .saturating_add(e.timing.delay_ms)
            .saturating_add(e.timing.duration_ms);
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::animation::{
        AnimationCategory, AnimationEntry, AnimationTiming, AnimationTrigger,
    };

    // entry
    // Build a timeline entry with explicit category/trigger/timing.
    fn entry(
        id: &str,
        el: &str,
        keyframe: &str,
        cat: AnimationCategory,
        trig: AnimationTrigger,
        delay_ms: u32,
        duration_ms: u32,
    ) -> AnimationEntry {
        AnimationEntry::new(
            id.into(),
            el.into(),
            keyframe.into(),
            cat,
            trig,
            AnimationTiming { duration_ms, delay_ms, easing: "ease".into(), ..Default::default() },
        )
    }

    fn enter(id: &str, el: &str, trig: AnimationTrigger) -> AnimationEntry {
        entry(id, el, "appear", AnimationCategory::Entrance, trig, 0, 500)
    }
    fn exit(id: &str, el: &str, trig: AnimationTrigger) -> AnimationEntry {
        entry(id, el, "disappear", AnimationCategory::Exit, trig, 0, 500)
    }

    // ---- snap_reveal: resolved visibility, never animates ----

    #[test]
    fn snap_at_step_zero_hides_not_yet_entered_elements() {
        // A enters on the first click (step 1); at step 0 it is hidden.
        let t = [enter("a1", "el_a", AnimationTrigger::OnClick)];
        let r = snap_reveal("s1", &t, 0);
        assert!(r.animate.is_empty());
        assert_eq!(r.hidden, vec!["el_a".to_string()]);
        assert!(r.shown.is_empty());
    }

    #[test]
    fn snap_after_entrance_shows_element() {
        let t = [enter("a1", "el_a", AnimationTrigger::OnClick)];
        let r = snap_reveal("s1", &t, 1);
        assert!(r.animate.is_empty());
        assert!(r.hidden.is_empty());
        assert_eq!(r.shown, vec!["el_a".to_string()]);
    }

    #[test]
    fn snap_after_exit_hides_element() {
        // el_a enters on step 1, exits on step 2.
        let t = [
            enter("a1", "el_a", AnimationTrigger::OnClick),
            exit("a2", "el_a", AnimationTrigger::OnClick),
        ];
        let r = snap_reveal("s1", &t, 2);
        assert!(r.animate.is_empty());
        assert_eq!(r.hidden, vec!["el_a".to_string()]);
        assert!(r.shown.is_empty());
    }

    #[test]
    fn snap_lists_each_element_once_in_first_appearance_order() {
        // el_b appears only via emphasis (always visible); el_a enters step 1.
        let t = [
            enter("a1", "el_a", AnimationTrigger::OnClick),
            entry("e1", "el_b", "pulse", AnimationCategory::Emphasis, AnimationTrigger::OnClick, 0, 300),
        ];
        let r = snap_reveal("s1", &t, 2);
        // el_a visible (entered), el_b visible (emphasis only) — order a then b.
        assert!(r.hidden.is_empty());
        assert_eq!(r.shown, vec!["el_a".to_string(), "el_b".to_string()]);
    }

    // ---- forward_reveal: newly-fired group animates ----

    #[test]
    fn forward_animates_only_the_newly_fired_group() {
        // step 1 fires a1 (el_a enter); step 2 fires a2 (el_b enter).
        let t = [
            enter("a1", "el_a", AnimationTrigger::OnClick),
            enter("a2", "el_b", AnimationTrigger::OnClick),
        ];
        let r = forward_reveal("s1", &t, 2);
        assert_eq!(r.animate.len(), 1);
        assert_eq!(r.animate[0].element_id, "el_b");
        assert_eq!(r.animate[0].keyframe, "appear");
        assert!(!r.animate[0].ends_hidden);
        // el_a already entered, not animating now → resolved shown.
        assert_eq!(r.shown, vec!["el_a".to_string()]);
        assert!(r.hidden.is_empty());
    }

    #[test]
    fn forward_exit_marks_ends_hidden() {
        let t = [
            enter("a1", "el_a", AnimationTrigger::OnClick),
            exit("a2", "el_a", AnimationTrigger::OnClick),
        ];
        let r = forward_reveal("s1", &t, 2);
        assert_eq!(r.animate.len(), 1);
        assert_eq!(r.animate[0].element_id, "el_a");
        assert!(r.animate[0].ends_hidden);
        // The animating element is listed ONLY in animate.
        assert!(r.shown.is_empty());
        assert!(r.hidden.is_empty());
    }

    #[test]
    fn forward_after_previous_accumulates_effective_delay() {
        // Same step (step 1): a1 OnClick(delay0,dur500), b1 AfterPrevious(delay100,dur300).
        let t = [
            entry("a1", "el_a", "appear", AnimationCategory::Entrance, AnimationTrigger::OnClick, 0, 500),
            entry("b1", "el_b", "appear", AnimationCategory::Entrance, AnimationTrigger::AfterPrevious, 100, 300),
        ];
        let r = forward_reveal("s1", &t, 1);
        assert_eq!(r.animate.len(), 2);
        let a = r.animate.iter().find(|i| i.element_id == "el_a").unwrap();
        let b = r.animate.iter().find(|i| i.element_id == "el_b").unwrap();
        assert_eq!(a.delay_ms, 0);
        // b starts after a finishes (0+500) plus its own 100.
        assert_eq!(b.delay_ms, 600);
    }

    #[test]
    fn forward_with_previous_uses_own_delay_only() {
        let t = [
            entry("a1", "el_a", "appear", AnimationCategory::Entrance, AnimationTrigger::OnClick, 0, 500),
            entry("b1", "el_b", "appear", AnimationCategory::Entrance, AnimationTrigger::WithPrevious, 50, 200),
        ];
        let r = forward_reveal("s1", &t, 1);
        let b = r.animate.iter().find(|i| i.element_id == "el_b").unwrap();
        assert_eq!(b.delay_ms, 50);
    }
}
