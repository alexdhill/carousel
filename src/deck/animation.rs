// Animation model + cursor state machine + validation helpers.
//
// A slide owns an ordered timeline of `AnimationEntry`s (see
// `SlideNode.animations`). Each entry targets an element by id and carries a
// category (entrance/emphasis/exit), a trigger (on-click / with-previous /
// after-previous), and timing. There is NO playback in this pass: the state
// machine is a pure cursor that derives "steps" by folding on-click
// boundaries, and the validation helpers keep the timeline's invariants
// (≤1 entrance/exit per element; entrance before exit).

use crate::deck::ids::{AnimationId, ElementId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AnimationCategory {
    Entrance,
    Emphasis,
    Exit,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AnimationTrigger {
    OnClick,
    WithPrevious,
    AfterPrevious,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AnimationIterations {
    Count(u32),
    Infinite,
}

impl Default for AnimationIterations {
    fn default() -> Self {
        AnimationIterations::Count(1)
    }
}

// AnimationTiming
// CSS-shaped timing for one entry. `easing` is a CSS timing-function token
// ("ease", "linear", …). All fields are integer/string so the type is `Eq`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnimationTiming {
    pub duration_ms: u32,
    pub delay_ms: u32,
    pub easing: String,
    pub iterations: AnimationIterations,
}

impl Default for AnimationTiming {
    fn default() -> Self {
        Self {
            duration_ms: 500,
            delay_ms: 0,
            easing: "ease".into(),
            iterations: AnimationIterations::Count(1),
        }
    }
}

// AnimationEntry
// One row in a slide's timeline. `keyframe` is a name reference (built-in or
// custom @keyframes); it is never validated against the library — an unknown
// name simply fails to animate in a future runtime, harmlessly.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnimationEntry {
    pub id: AnimationId,
    pub element_id: ElementId,
    pub keyframe: String,
    pub category: AnimationCategory,
    pub trigger: AnimationTrigger,
    pub timing: AnimationTiming,
}

impl AnimationEntry {
    // new
    // Inputs: id, target element id, keyframe name, category, trigger, timing.
    // Output: an AnimationEntry.
    // Errors: panics on empty id / element_id / keyframe (model invariant).
    pub fn new(
        id: AnimationId,
        element_id: ElementId,
        keyframe: String,
        category: AnimationCategory,
        trigger: AnimationTrigger,
        timing: AnimationTiming,
    ) -> Self {
        assert!(!id.is_empty(), "animation id must not be empty");
        assert!(!element_id.is_empty(), "animation element_id must not be empty");
        assert!(!keyframe.is_empty(), "animation keyframe must not be empty");
        Self { id, element_id, keyframe, category, trigger, timing }
    }
}

// ---------- cursor state machine ----------

// step_count
// A step is a maximal run starting at an OnClick entry. Step 0 is the
// pre-click initial state; each OnClick opens the next step. Leading
// with/after-previous entries belong to step 0.
// Output: total number of steps (1 + count of OnClick entries).
pub fn step_count(timeline: &[AnimationEntry]) -> usize {
    let mut clicks: usize = 0;
    for e in timeline {
        if matches!(e.trigger, AnimationTrigger::OnClick) {
            clicks += 1;
        }
    }
    clicks + 1
}

// entries_through
// Output: the prefix of the timeline that has fired by `step` — every entry
// up to and including the `step`-th OnClick group. Step 0 returns the leading
// run before the first OnClick.
pub fn entries_through(timeline: &[AnimationEntry], step: usize) -> &[AnimationEntry] {
    let mut clicks: usize = 0;
    let mut end: usize = 0;
    for (i, e) in timeline.iter().enumerate() {
        if matches!(e.trigger, AnimationTrigger::OnClick) {
            clicks += 1;
            if clicks > step {
                break;
            }
        }
        end = i + 1;
    }
    &timeline[..end]
}

// AnimationState
// Pure cursor over a slide's timeline. Holds only the current step; all
// queries take the timeline as an argument so the state never goes stale
// against an edited timeline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnimationState {
    current_step: usize,
}

impl AnimationState {
    pub fn current_step(&self) -> usize {
        self.current_step
    }

    pub fn reset(&mut self) {
        self.current_step = 0;
    }

    // advance
    // Move the cursor forward within 0..=last_step, clamped (no wrap).
    pub fn advance(&mut self, timeline: &[AnimationEntry]) {
        let last: usize = step_count(timeline).saturating_sub(1);
        if self.current_step < last {
            self.current_step += 1;
        }
    }

    // back
    // Move the cursor backward, clamped at 0.
    pub fn back(&mut self, _timeline: &[AnimationEntry]) {
        self.current_step = self.current_step.saturating_sub(1);
    }

    // jump_to_last
    // Move the cursor to the timeline's final step. Used by presentation mode
    // when stepping backward into the previous slide, which lands fully
    // resolved on that slide's last step.
    pub fn jump_to_last(&mut self, timeline: &[AnimationEntry]) {
        self.current_step = step_count(timeline).saturating_sub(1);
    }
}

// ---------- validation helpers ----------

// index_of_category
// Output: the timeline index of `element_id`'s entry of `category`, or None.
pub fn index_of_category(
    timeline: &[AnimationEntry],
    element_id: &str,
    category: AnimationCategory,
) -> Option<usize> {
    timeline
        .iter()
        .position(|e| e.element_id == element_id && e.category == category)
}

// has_category
// Output: true if `element_id` already owns an entry of `category`.
pub fn has_category(
    timeline: &[AnimationEntry],
    element_id: &str,
    category: AnimationCategory,
) -> bool {
    index_of_category(timeline, element_id, category).is_some()
}

// ordering_ok
// Output: true if, for the given element, any entrance precedes any exit (or
// either is absent). The only ordering invariant the timeline must preserve.
pub fn ordering_ok(timeline: &[AnimationEntry], element_id: &str) -> bool {
    match (
        index_of_category(timeline, element_id, AnimationCategory::Entrance),
        index_of_category(timeline, element_id, AnimationCategory::Exit),
    ) {
        (Some(en), Some(ex)) => en < ex,
        _ => true,
    }
}

// accommodating_index
// Inputs: the timeline, the requested insert position, and the entry about to
// be inserted.
// Output: (final_index, warning). For an Entrance whose element already has an
// Exit at index `ex`, clamp the index to `ex` (so it lands immediately before
// the exit). For an Exit whose element already has an Entrance at index `en`,
// clamp the index to at least `en + 1`. Otherwise the requested index (clamped
// to len). The warning is Some(msg) only when the index was adjusted.
pub fn accommodating_index(
    timeline: &[AnimationEntry],
    requested: usize,
    entry: &AnimationEntry,
) -> (usize, Option<String>) {
    let len: usize = timeline.len();
    let want: usize = requested.min(len);
    match entry.category {
        AnimationCategory::Entrance => {
            if let Some(ex) =
                index_of_category(timeline, &entry.element_id, AnimationCategory::Exit)
                && want > ex
            {
                return (
                    ex,
                    Some(format!("Entrance for {} moved before its exit", entry.element_id)),
                );
            }
            (want, None)
        }
        AnimationCategory::Exit => {
            if let Some(en) =
                index_of_category(timeline, &entry.element_id, AnimationCategory::Entrance)
                && want <= en
            {
                return (
                    en + 1,
                    Some(format!("Exit for {} moved after its entrance", entry.element_id)),
                );
            }
            (want, None)
        }
        AnimationCategory::Emphasis => (want, None),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn entry_at(id: &str, el: &str, cat: AnimationCategory, trig: AnimationTrigger) -> AnimationEntry {
        AnimationEntry::new(id.into(), el.into(), "appear".into(), cat, trig, AnimationTiming::default())
    }
    fn click(id: &str) -> AnimationEntry {
        entry_at(id, "el", AnimationCategory::Entrance, AnimationTrigger::OnClick)
    }
    fn with(id: &str) -> AnimationEntry {
        entry_at(id, "el", AnimationCategory::Entrance, AnimationTrigger::WithPrevious)
    }
    fn enter(id: &str, el: &str) -> AnimationEntry {
        AnimationEntry::new(id.into(), el.into(), "appear".into(),
            AnimationCategory::Entrance, AnimationTrigger::OnClick, AnimationTiming::default())
    }
    fn exit(id: &str, el: &str) -> AnimationEntry {
        AnimationEntry::new(id.into(), el.into(), "disappear".into(),
            AnimationCategory::Exit, AnimationTrigger::OnClick, AnimationTiming::default())
    }

    #[test]
    fn entry_serde_roundtrips_with_finite_and_infinite() {
        for iters in [AnimationIterations::Count(3), AnimationIterations::Infinite] {
            let e = AnimationEntry::new(
                "anim_1".into(), "el_a".into(), "appear".into(),
                AnimationCategory::Entrance, AnimationTrigger::OnClick,
                AnimationTiming { iterations: iters, ..AnimationTiming::default() },
            );
            let json = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<AnimationEntry>(&json).unwrap(), e);
        }
    }

    #[test]
    #[should_panic(expected = "keyframe must not be empty")]
    fn entry_rejects_empty_keyframe() {
        let _ = AnimationEntry::new("a".into(), "el".into(), String::new(),
            AnimationCategory::Emphasis, AnimationTrigger::OnClick, AnimationTiming::default());
    }

    #[test]
    fn step_count_counts_onclick_groups() {
        assert_eq!(step_count(&[]), 1);
        assert_eq!(step_count(&[click("a"), with("b"), click("c")]), 3);
        // Leading with-previous belongs to step 0; first OnClick opens step 1.
        assert_eq!(step_count(&[with("a"), click("b")]), 2);
    }

    #[test]
    fn entries_through_returns_fired_prefix() {
        let t = [with("a"), click("b"), with("c"), click("d")];
        assert_eq!(entries_through(&t, 0).len(), 1); // just "a"
        assert_eq!(entries_through(&t, 1).len(), 3); // a,b,c
        assert_eq!(entries_through(&t, 2).len(), 4); // all
    }

    #[test]
    fn jump_to_last_lands_on_final_step() {
        let t = [click("a"), click("b")]; // step_count 3 -> last step 2
        let mut s = AnimationState::default();
        s.jump_to_last(&t);
        assert_eq!(s.current_step(), 2);
        // Empty timeline: single step, last is 0.
        let mut e = AnimationState::default();
        e.jump_to_last(&[]);
        assert_eq!(e.current_step(), 0);
    }

    #[test]
    fn advance_back_clamp() {
        let t = [click("a"), click("b")]; // step_count 3 -> last step 2
        let mut s = AnimationState::default();
        s.advance(&t);
        s.advance(&t);
        s.advance(&t); // clamps at 2
        assert_eq!(s.current_step(), 2);
        s.back(&t);
        s.back(&t);
        s.back(&t); // clamps at 0
        assert_eq!(s.current_step(), 0);
    }

    #[test]
    fn accommodate_entrance_before_existing_exit() {
        let t = [exit("x", "el_a")];
        let (idx, warn) = accommodating_index(&t, 5, &enter("e", "el_a"));
        assert_eq!(idx, 0);
        assert!(warn.is_some());
    }

    #[test]
    fn ordering_helpers() {
        let bad = [exit("x", "el_a"), enter("e", "el_a")];
        assert!(!ordering_ok(&bad, "el_a"));
        let good = [enter("e", "el_a"), exit("x", "el_a")];
        assert!(ordering_ok(&good, "el_a"));
        assert!(has_category(&good, "el_a", AnimationCategory::Exit));
        assert_eq!(index_of_category(&good, "el_a", AnimationCategory::Entrance), Some(0));
    }
}
