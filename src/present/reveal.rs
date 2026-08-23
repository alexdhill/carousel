use crate::deck::animation::{
    AnimationCategory, AnimationEntry, AnimationTrigger, entries_through, index_of_category,
};
use crate::ipc::present::{AnimateInstruction, RevealPayload};

const MAX_TIMELINE: usize = 4_096;

pub fn snap_reveal(slide_id: &str, timeline: &[AnimationEntry], step: usize) -> RevealPayload {
    assert!(!slide_id.is_empty(), "snap_reveal: empty slide id");
    let (hidden, shown): (Vec<String>, Vec<String>) = resolve_rest(timeline, step, &[]);
    RevealPayload {
        slide_id: slide_id.into(),
        hidden,
        shown,
        animate: Vec::new(),
    }
}

pub fn forward_reveal(
    slide_id: &str,
    timeline: &[AnimationEntry],
    to_step: usize,
) -> RevealPayload {
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
            keyframe: e.effect.keyframe_name().unwrap_or("").to_string(),
            targets: e.effect.targets().map(<[_]>::to_vec).unwrap_or_default(),
            duration_ms: e.timing.duration_ms,
            delay_ms: delays[i],
            easing: e.timing.easing.clone(),
            iterations: e.timing.iterations,
            ends_hidden: e.category == AnimationCategory::Exit,
        });
        animating.push(e.element_id.as_str());
    }
    let (hidden, shown): (Vec<String>, Vec<String>) = resolve_rest(timeline, to_step, &animating);
    RevealPayload {
        slide_id: slide_id.into(),
        hidden,
        shown,
        animate,
    }
}

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

fn is_visible(timeline: &[AnimationEntry], element_id: &str, step: usize) -> bool {
    assert!(!element_id.is_empty(), "is_visible: empty element id");
    let fired_len: usize = entries_through(timeline, step).len();
    let en: Option<usize> = index_of_category(timeline, element_id, AnimationCategory::Entrance);
    let ex: Option<usize> = index_of_category(timeline, element_id, AnimationCategory::Exit);
    let entered: bool = en.is_none_or(|i| i < fired_len);
    let exited: bool = ex.is_some_and(|i| i < fired_len);
    entered && !exited
}

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
        AnimationCategory, AnimationEffect, AnimationEntry, AnimationTiming, AnimationTrigger,
    };

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
            AnimationEffect::Named(keyframe.into()),
            cat,
            trig,
            AnimationTiming {
                duration_ms,
                delay_ms,
                easing: "ease".into(),
                ..Default::default()
            },
        )
    }

    fn enter(id: &str, el: &str, trig: AnimationTrigger) -> AnimationEntry {
        entry(id, el, "appear", AnimationCategory::Entrance, trig, 0, 500)
    }
    fn exit(id: &str, el: &str, trig: AnimationTrigger) -> AnimationEntry {
        entry(id, el, "disappear", AnimationCategory::Exit, trig, 0, 500)
    }

    #[test]
    fn snap_at_step_zero_hides_not_yet_entered_elements() {
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
        let t = [
            enter("a1", "el_a", AnimationTrigger::OnClick),
            entry(
                "e1",
                "el_b",
                "pulse",
                AnimationCategory::Emphasis,
                AnimationTrigger::OnClick,
                0,
                300,
            ),
        ];
        let r = snap_reveal("s1", &t, 2);

        assert!(r.hidden.is_empty());
        assert_eq!(r.shown, vec!["el_a".to_string(), "el_b".to_string()]);
    }

    #[test]
    fn forward_animates_only_the_newly_fired_group() {
        let t = [
            enter("a1", "el_a", AnimationTrigger::OnClick),
            enter("a2", "el_b", AnimationTrigger::OnClick),
        ];
        let r = forward_reveal("s1", &t, 2);
        assert_eq!(r.animate.len(), 1);
        assert_eq!(r.animate[0].element_id, "el_b");
        assert_eq!(r.animate[0].keyframe, "appear");
        assert!(!r.animate[0].ends_hidden);

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

        assert!(r.shown.is_empty());
        assert!(r.hidden.is_empty());
    }

    #[test]
    fn forward_after_previous_accumulates_effective_delay() {
        let t = [
            entry(
                "a1",
                "el_a",
                "appear",
                AnimationCategory::Entrance,
                AnimationTrigger::OnClick,
                0,
                500,
            ),
            entry(
                "b1",
                "el_b",
                "appear",
                AnimationCategory::Entrance,
                AnimationTrigger::AfterPrevious,
                100,
                300,
            ),
        ];
        let r = forward_reveal("s1", &t, 1);
        assert_eq!(r.animate.len(), 2);
        let a = r.animate.iter().find(|i| i.element_id == "el_a").unwrap();
        let b = r.animate.iter().find(|i| i.element_id == "el_b").unwrap();
        assert_eq!(a.delay_ms, 0);

        assert_eq!(b.delay_ms, 600);
    }

    #[test]
    fn property_entry_emits_targets_not_keyframe() {
        use crate::deck::animation::PropertyTarget;
        let e = AnimationEntry::new(
            "p".into(),
            "el_a".into(),
            AnimationEffect::PropertyChange(vec![PropertyTarget {
                property: "opacity".into(),
                value: "1".into(),
            }]),
            AnimationCategory::Property,
            AnimationTrigger::OnClick,
            AnimationTiming::default(),
        );
        let r = forward_reveal("s1", std::slice::from_ref(&e), 1);
        assert_eq!(r.animate.len(), 1);
        assert!(r.animate[0].keyframe.is_empty());
        assert_eq!(r.animate[0].targets.len(), 1);
        assert_eq!(r.animate[0].targets[0].property, "opacity");
        assert!(!r.animate[0].ends_hidden);
    }

    #[test]
    fn forward_with_previous_uses_own_delay_only() {
        let t = [
            entry(
                "a1",
                "el_a",
                "appear",
                AnimationCategory::Entrance,
                AnimationTrigger::OnClick,
                0,
                500,
            ),
            entry(
                "b1",
                "el_b",
                "appear",
                AnimationCategory::Entrance,
                AnimationTrigger::WithPrevious,
                50,
                200,
            ),
        ];
        let r = forward_reveal("s1", &t, 1);
        let b = r.animate.iter().find(|i| i.element_id == "el_b").unwrap();
        assert_eq!(b.delay_ms, 50);
    }
}
