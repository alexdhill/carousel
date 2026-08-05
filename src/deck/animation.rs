use crate::deck::ids::{AnimationId, ElementId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AnimationCategory {
    Entrance,
    Emphasis,
    Exit,
    Property,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PropertyTarget {
    pub property: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AnimationEffect {
    Named(String),
    PropertyChange(Vec<PropertyTarget>),
}

impl AnimationEffect {

    pub fn keyframe_name(&self) -> Option<&str> {
        match self {
            AnimationEffect::Named(n) => Some(n.as_str()),
            AnimationEffect::PropertyChange(_) => None,
        }
    }

    pub fn targets(&self) -> Option<&[PropertyTarget]> {
        match self {
            AnimationEffect::PropertyChange(t) => Some(t.as_slice()),
            AnimationEffect::Named(_) => None,
        }
    }
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnimationEntry {
    pub id: AnimationId,
    pub element_id: ElementId,
    pub effect: AnimationEffect,
    pub category: AnimationCategory,
    pub trigger: AnimationTrigger,
    pub timing: AnimationTiming,
}

impl AnimationEntry {

    pub fn new(
        id: AnimationId,
        element_id: ElementId,
        effect: AnimationEffect,
        category: AnimationCategory,
        trigger: AnimationTrigger,
        timing: AnimationTiming,
    ) -> Self {
        assert!(!id.is_empty(), "animation id must not be empty");
        assert!(
            !element_id.is_empty(),
            "animation element_id must not be empty"
        );
        let pairing_ok = match (category, &effect) {
            (AnimationCategory::Property, AnimationEffect::PropertyChange(t)) => !t.is_empty(),
            (AnimationCategory::Property, _) => false,
            (_, AnimationEffect::Named(n)) => !n.is_empty(),
            (_, AnimationEffect::PropertyChange(_)) => false,
        };
        assert!(pairing_ok, "animation effect/category mismatch or empty");
        Self {
            id,
            element_id,
            effect,
            category,
            trigger,
            timing,
        }
    }
}

pub fn step_count(timeline: &[AnimationEntry]) -> usize {
    let mut clicks: usize = 0;
    for e in timeline {
        if matches!(e.trigger, AnimationTrigger::OnClick) {
            clicks += 1;
        }
    }
    clicks + 1
}

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

    pub fn advance(&mut self, timeline: &[AnimationEntry]) {
        let last: usize = step_count(timeline).saturating_sub(1);
        if self.current_step < last {
            self.current_step += 1;
        }
    }

    pub fn back(&mut self, _timeline: &[AnimationEntry]) {
        self.current_step = self.current_step.saturating_sub(1);
    }

    pub fn jump_to_last(&mut self, timeline: &[AnimationEntry]) {
        self.current_step = step_count(timeline).saturating_sub(1);
    }
}

pub fn index_of_category(
    timeline: &[AnimationEntry],
    element_id: &str,
    category: AnimationCategory,
) -> Option<usize> {
    timeline
        .iter()
        .position(|e| e.element_id == element_id && e.category == category)
}

pub fn has_category(
    timeline: &[AnimationEntry],
    element_id: &str,
    category: AnimationCategory,
) -> bool {
    index_of_category(timeline, element_id, category).is_some()
}

pub fn ordering_ok(timeline: &[AnimationEntry], element_id: &str) -> bool {
    match (
        index_of_category(timeline, element_id, AnimationCategory::Entrance),
        index_of_category(timeline, element_id, AnimationCategory::Exit),
    ) {
        (Some(en), Some(ex)) => en < ex,
        _ => true,
    }
}

pub fn accommodating_index(
    timeline: &[AnimationEntry],
    requested: usize,
    entry: &AnimationEntry,
) -> (usize, Option<String>) {
    let len: usize = timeline.len();
    let want: usize = requested.min(len);
    assert!(want <= len, "accommodating_index: index past end");
    let _ = entry;
    (want, None)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn entry_at(
        id: &str,
        el: &str,
        cat: AnimationCategory,
        trig: AnimationTrigger,
    ) -> AnimationEntry {
        AnimationEntry::new(
            id.into(),
            el.into(),
            AnimationEffect::Named("appear".into()),
            cat,
            trig,
            AnimationTiming::default(),
        )
    }
    fn click(id: &str) -> AnimationEntry {
        entry_at(
            id,
            "el",
            AnimationCategory::Entrance,
            AnimationTrigger::OnClick,
        )
    }
    fn with(id: &str) -> AnimationEntry {
        entry_at(
            id,
            "el",
            AnimationCategory::Entrance,
            AnimationTrigger::WithPrevious,
        )
    }
    fn enter(id: &str, el: &str) -> AnimationEntry {
        AnimationEntry::new(
            id.into(),
            el.into(),
            AnimationEffect::Named("appear".into()),
            AnimationCategory::Entrance,
            AnimationTrigger::OnClick,
            AnimationTiming::default(),
        )
    }
    fn exit(id: &str, el: &str) -> AnimationEntry {
        AnimationEntry::new(
            id.into(),
            el.into(),
            AnimationEffect::Named("disappear".into()),
            AnimationCategory::Exit,
            AnimationTrigger::OnClick,
            AnimationTiming::default(),
        )
    }

    #[test]
    fn entry_serde_roundtrips_with_finite_and_infinite() {
        for iters in [AnimationIterations::Count(3), AnimationIterations::Infinite] {
            let e = AnimationEntry::new(
                "anim_1".into(),
                "el_a".into(),
                AnimationEffect::Named("appear".into()),
                AnimationCategory::Entrance,
                AnimationTrigger::OnClick,
                AnimationTiming {
                    iterations: iters,
                    ..AnimationTiming::default()
                },
            );
            let json = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<AnimationEntry>(&json).unwrap(), e);
        }
    }

    #[test]
    fn effect_named_and_property_serde_roundtrip() {
        let named = AnimationEffect::Named("fade-in".into());
        let prop = AnimationEffect::PropertyChange(vec![PropertyTarget {
            property: "opacity".into(),
            value: "1".into(),
        }]);
        for eff in [named, prop] {
            let j = serde_json::to_string(&eff).unwrap();
            assert_eq!(serde_json::from_str::<AnimationEffect>(&j).unwrap(), eff);
        }
    }

    #[test]
    #[should_panic(expected = "effect/category")]
    fn property_category_requires_property_effect() {
        let _ = AnimationEntry::new(
            "a".into(),
            "el".into(),
            AnimationEffect::Named("pulse".into()),
            AnimationCategory::Property,
            AnimationTrigger::OnClick,
            AnimationTiming::default(),
        );
    }

    #[test]
    fn multiple_entrances_allowed_by_accommodating_index() {

        let e1 = AnimationEntry::new(
            "e1".into(),
            "el".into(),
            AnimationEffect::Named("appear".into()),
            AnimationCategory::Entrance,
            AnimationTrigger::OnClick,
            AnimationTiming::default(),
        );
        let (idx, warn) = accommodating_index(std::slice::from_ref(&e1), 9, &e1);
        assert_eq!(idx, 1);
        assert!(warn.is_none());
    }

    #[test]
    fn step_count_counts_onclick_groups() {
        assert_eq!(step_count(&[]), 1);
        assert_eq!(step_count(&[click("a"), with("b"), click("c")]), 3);

        assert_eq!(step_count(&[with("a"), click("b")]), 2);
    }

    #[test]
    fn entries_through_returns_fired_prefix() {
        let t = [with("a"), click("b"), with("c"), click("d")];
        assert_eq!(entries_through(&t, 0).len(), 1);
        assert_eq!(entries_through(&t, 1).len(), 3);
        assert_eq!(entries_through(&t, 2).len(), 4);
    }

    #[test]
    fn jump_to_last_lands_on_final_step() {
        let t = [click("a"), click("b")];
        let mut s = AnimationState::default();
        s.jump_to_last(&t);
        assert_eq!(s.current_step(), 2);

        let mut e = AnimationState::default();
        e.jump_to_last(&[]);
        assert_eq!(e.current_step(), 0);
    }

    #[test]
    fn advance_back_clamp() {
        let t = [click("a"), click("b")];
        let mut s = AnimationState::default();
        s.advance(&t);
        s.advance(&t);
        s.advance(&t);
        assert_eq!(s.current_step(), 2);
        s.back(&t);
        s.back(&t);
        s.back(&t);
        assert_eq!(s.current_step(), 0);
    }

    #[test]
    fn accommodating_index_inserts_at_requested_clamped() {
        let t = [exit("x", "el_a")];
        let (idx, warn) = accommodating_index(&t, 5, &enter("e", "el_a"));
        assert_eq!(idx, 1);
        assert!(warn.is_none());
    }

    #[test]
    fn ordering_helpers() {
        let bad = [exit("x", "el_a"), enter("e", "el_a")];
        assert!(!ordering_ok(&bad, "el_a"));
        let good = [enter("e", "el_a"), exit("x", "el_a")];
        assert!(ordering_ok(&good, "el_a"));
        assert!(has_category(&good, "el_a", AnimationCategory::Exit));
        assert_eq!(
            index_of_category(&good, "el_a", AnimationCategory::Entrance),
            Some(0)
        );
    }
}
