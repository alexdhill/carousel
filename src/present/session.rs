// Presentation session — the slide+step state machine.
//
// `PresentCursor` is the pure core: it owns the `AnimationState` cursor and the
// index into `deck.slide_order`, and turns Advance / Back into a `PresentStep`
// describing what the frontend should render. It borrows the deck per call (it
// never owns it), so it is fully unit-testable without a webview.
//
// `PresentationSession` pairs that cursor with the presentation `WebviewSender`
// (which owns the WebView). `ApplicationCore` drives it; the sender plays no
// part in the cursor logic, which is why the tests target `PresentCursor`.

use crate::deck::animation::{AnimationState, step_count};
use crate::deck::{Deck, SlideId};
use crate::html::serialize::serialize_slide_themed;
use crate::ipc::bridge::WebviewSender;
use crate::ipc::present::{PresentSlidePayload, RevealPayload};
use crate::present::reveal::{forward_reveal, snap_reveal};

// PresentStep
// What a cursor move asks the frontend to render:
//   - Reveal: same slide, apply this step's state.
//   - SlideChanged: crossed to another slide — mount it, then apply (snapped).
//   - Unchanged: clamped at the very first/last step of the deck (no-op).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresentStep {
    Reveal(RevealPayload),
    SlideChanged {
        slide: PresentSlidePayload,
        reveal: RevealPayload,
    },
    Unchanged,
}

// PresentCursor
// Pure slide+step state machine over a borrowed deck.
pub struct PresentCursor {
    cursor: AnimationState,
    slide_index: usize,
}

impl PresentCursor {
    // new
    // Inputs: the starting index into deck.slide_order.
    // Output: a cursor parked at step 0 of that slide.
    pub fn new(slide_index: usize) -> Self {
        Self {
            cursor: AnimationState::default(),
            slide_index,
        }
    }

    pub fn slide_index(&self) -> usize {
        self.slide_index
    }

    // advance
    // Inputs: the live deck.
    // Output: a PresentStep. Within a slide, advancing fires the newly-entered
    // step (forward animation). At the slide's last step, it crosses to the next
    // slide's step 0 (snapped). At the deck's final step, Unchanged.
    pub fn advance(&mut self, deck: &Deck) -> PresentStep {
        let sid: SlideId = match deck.slide_order.get(self.slide_index) {
            Some(id) => id.clone(),
            None => return PresentStep::Unchanged,
        };
        let timeline = self.timeline(deck, &sid);
        let last: usize = step_count(&timeline).saturating_sub(1);
        if self.cursor.current_step() < last {
            self.cursor.advance(&timeline);
            let step: usize = self.cursor.current_step();
            return PresentStep::Reveal(forward_reveal(&sid, &timeline, step));
        }
        if self.slide_index + 1 < deck.slide_order.len() {
            // Outgoing slide owns the transition; always carry it on a forward
            // cross-slide so the frontend knows this is a forward move (a
            // None-kind transition is a cut, but still enables element morphs).
            // Backward never animates, so it alone passes None.
            let outgoing: crate::deck::SlideTransition = deck
                .slides
                .get(&sid)
                .and_then(|s| s.metadata.transition.clone())
                .unwrap_or_default();
            self.slide_index += 1;
            self.cursor.reset();
            return self.snapped_slide_change(deck, 0, Some(outgoing));
        }
        PresentStep::Unchanged
    }

    // back
    // Inputs: the live deck.
    // Output: a PresentStep. Backward is always a SNAP (no reverse animation).
    // Within a slide it restores the previous step; at step 0 it crosses to the
    // previous slide's last step; at the deck's first step, Unchanged.
    pub fn back(&mut self, deck: &Deck) -> PresentStep {
        let sid: SlideId = match deck.slide_order.get(self.slide_index) {
            Some(id) => id.clone(),
            None => return PresentStep::Unchanged,
        };
        if self.cursor.current_step() > 0 {
            let timeline = self.timeline(deck, &sid);
            self.cursor.back(&timeline);
            let step: usize = self.cursor.current_step();
            return PresentStep::Reveal(snap_reveal(&sid, &timeline, step));
        }
        if self.slide_index > 0 {
            self.slide_index -= 1;
            let prev_id: SlideId = deck.slide_order[self.slide_index].clone();
            let prev_timeline = self.timeline(deck, &prev_id);
            self.cursor.jump_to_last(&prev_timeline);
            let step: usize = self.cursor.current_step();
            return self.snapped_slide_change(deck, step, None);
        }
        PresentStep::Unchanged
    }

    // current_reveal
    // Output: a snapped RevealPayload for the current slide+step (used for the
    // initial mount). None if the slide index is out of range.
    pub fn current_reveal(&self, deck: &Deck) -> Option<RevealPayload> {
        let sid: &SlideId = deck.slide_order.get(self.slide_index)?;
        let timeline = self.timeline(deck, sid);
        Some(snap_reveal(sid, &timeline, self.cursor.current_step()))
    }

    // current_slide_payload
    // Output: the mount payload for the current slide. None if out of range.
    pub fn current_slide_payload(&self, deck: &Deck) -> Option<PresentSlidePayload> {
        let sid: &SlideId = deck.slide_order.get(self.slide_index)?;
        Some(slide_payload(deck, sid, self.slide_index + 1, deck.slide_order.len()))
    }

    // snapped_slide_change
    // Build a SlideChanged for the current slide_index at `step`, snapped. The
    // reveal is always a snap; `transition` (the outgoing slide's, forward only)
    // rides on the mount payload so the frontend animates the host swap.
    fn snapped_slide_change(
        &self,
        deck: &Deck,
        step: usize,
        transition: Option<crate::deck::SlideTransition>,
    ) -> PresentStep {
        let sid: SlideId = deck.slide_order[self.slide_index].clone();
        let timeline = self.timeline(deck, &sid);
        let mut slide: PresentSlidePayload =
            slide_payload(deck, &sid, self.slide_index + 1, deck.slide_order.len());
        slide.transition = transition;
        PresentStep::SlideChanged {
            slide,
            reveal: snap_reveal(&sid, &timeline, step),
        }
    }

    // timeline: clone the slide's timeline (empty if the slide is missing).
    fn timeline(&self, deck: &Deck, sid: &str) -> Vec<crate::deck::AnimationEntry> {
        deck.slides
            .get(sid)
            .map(|s| s.animations.clone())
            .unwrap_or_default()
    }
}

// slide_payload
// Inputs: the deck and a slide id known to be in display order.
// Output: the PresentSlidePayload (serialized HTML + theme/globals CSS). A
// missing slide yields empty HTML rather than panicking — the caller guards
// membership, so this is defensive only.
fn slide_payload(deck: &Deck, sid: &str, number: usize, count: usize) -> PresentSlidePayload {
    assert!(!sid.is_empty(), "slide_payload: empty slide id");
    let opts: crate::html::serialize::RenderOpts = crate::html::serialize::RenderOpts {
        ctx: Some(crate::html::serialize::RenderCtx {
            number,
            count,
            date: crate::html::serialize::today_ymd(),
        }),
        hide_placeholders: true,
    };
    let slide_html: String = deck
        .slides
        .get(sid)
        .map(|s| {
            let (fill, img) = deck.effective_slide_bg(s);
            serialize_slide_themed(s, fill.as_deref(), img.as_deref(), &opts)
        })
        .unwrap_or_default();
    PresentSlidePayload {
        slide_id: sid.to_string(),
        slide_html,
        theme_css: deck.theme.theme_css.clone(),
        globals_css: deck.theme.globals_css.clone(),
        transition: None,
    }
}

// PresentationSession
// Pairs the pure cursor with the presentation WebviewSender (which owns the
// presentation WebView). ApplicationCore owns one of these while presenting.
pub struct PresentationSession {
    sender: WebviewSender,
    cursor: PresentCursor,
}

impl PresentationSession {
    // new
    // Inputs: the presentation WebviewSender and the starting slide index.
    pub fn new(sender: WebviewSender, slide_index: usize) -> Self {
        Self {
            sender,
            cursor: PresentCursor::new(slide_index),
        }
    }

    pub fn sender(&self) -> &WebviewSender {
        &self.sender
    }

    pub fn advance(&mut self, deck: &Deck) -> PresentStep {
        self.cursor.advance(deck)
    }

    pub fn back(&mut self, deck: &Deck) -> PresentStep {
        self.cursor.back(deck)
    }

    pub fn current_reveal(&self, deck: &Deck) -> Option<RevealPayload> {
        self.cursor.current_reveal(deck)
    }

    pub fn current_slide_payload(&self, deck: &Deck) -> Option<PresentSlidePayload> {
        self.cursor.current_slide_payload(deck)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;
    use crate::deck::animation::{
        AnimationCategory, AnimationEntry, AnimationTiming, AnimationTrigger,
    };
    use crate::deck::builders::group_element;
    use crate::deck::slide::SlideNode;
    use std::collections::BTreeMap;

    fn click_entry(id: &str, el: &str) -> AnimationEntry {
        AnimationEntry::new(
            id.into(),
            el.into(),
            crate::deck::animation::AnimationEffect::Named("appear".into()),
            AnimationCategory::Entrance,
            AnimationTrigger::OnClick,
            AnimationTiming::default(),
        )
    }

    // deck_with: build a deck whose slides carry the given timelines, in order.
    fn deck_with(specs: Vec<(&str, Vec<AnimationEntry>)>) -> Deck {
        let mut slides: BTreeMap<String, SlideNode> = BTreeMap::new();
        let mut order: Vec<String> = Vec::new();
        for (sid, anims) in specs {
            let root = group_element("el_root", vec![]);
            let mut s = SlideNode::new(sid.into(), "blank".into(), root);
            s.animations = anims;
            slides.insert(sid.into(), s);
            order.push(sid.into());
        }
        Deck {
            slides,
            slide_order: order,
            ..Deck::default()
        }
    }

    #[test]
    fn advance_within_slide_increments_step_and_animates() {
        // s1: one OnClick entry → step_count 2 (last step 1).
        let deck = deck_with(vec![("s1", vec![click_entry("a1", "el_a")])]);
        let mut cur = PresentCursor::new(0);
        match cur.advance(&deck) {
            PresentStep::Reveal(r) => {
                assert_eq!(r.slide_id, "s1");
                assert_eq!(r.animate.len(), 1);
                assert_eq!(r.animate[0].element_id, "el_a");
            }
            other => panic!("expected Reveal, got {other:?}"),
        }
        assert_eq!(cur.slide_index(), 0);
    }

    #[test]
    fn advance_at_last_step_crosses_to_next_slide_snapped() {
        // s1 has no animations → already at last step (0); advance crosses.
        let deck = deck_with(vec![
            ("s1", vec![]),
            ("s2", vec![click_entry("b1", "el_b")]),
        ]);
        let mut cur = PresentCursor::new(0);
        match cur.advance(&deck) {
            PresentStep::SlideChanged { slide, reveal } => {
                assert_eq!(slide.slide_id, "s2");
                assert!(!slide.slide_html.is_empty());
                assert_eq!(reveal.slide_id, "s2");
                assert!(reveal.animate.is_empty()); // cross-slide is a snap
                // el_b enters on step 1, so at step 0 it is hidden.
                assert_eq!(reveal.hidden, vec!["el_b".to_string()]);
            }
            other => panic!("expected SlideChanged, got {other:?}"),
        }
        assert_eq!(cur.slide_index(), 1);
    }

    #[test]
    fn advance_at_last_slide_last_step_is_unchanged() {
        let deck = deck_with(vec![("s1", vec![])]);
        let mut cur = PresentCursor::new(0);
        assert!(matches!(cur.advance(&deck), PresentStep::Unchanged));
        assert_eq!(cur.slide_index(), 0);
    }

    #[test]
    fn back_within_slide_snaps_without_animation() {
        let deck = deck_with(vec![("s1", vec![click_entry("a1", "el_a")])]);
        let mut cur = PresentCursor::new(0);
        let _ = cur.advance(&deck); // now at step 1
        match cur.back(&deck) {
            PresentStep::Reveal(r) => {
                assert!(r.animate.is_empty()); // snap, no reverse animation
                assert_eq!(r.hidden, vec!["el_a".to_string()]); // back at step 0
            }
            other => panic!("expected Reveal, got {other:?}"),
        }
    }

    #[test]
    fn back_at_step_zero_crosses_to_prev_slide_last_step() {
        // s1: one click (last step 1). s2: no animations.
        let deck = deck_with(vec![
            ("s1", vec![click_entry("a1", "el_a")]),
            ("s2", vec![]),
        ]);
        let mut cur = PresentCursor::new(1); // start on s2, step 0
        match cur.back(&deck) {
            PresentStep::SlideChanged { slide, reveal } => {
                assert_eq!(slide.slide_id, "s1");
                assert!(reveal.animate.is_empty());
                // s1 last step (1): el_a has entered → shown.
                assert_eq!(reveal.shown, vec!["el_a".to_string()]);
            }
            other => panic!("expected SlideChanged, got {other:?}"),
        }
        assert_eq!(cur.slide_index(), 0);
    }

    #[test]
    fn back_at_first_slide_step_zero_is_unchanged() {
        let deck = deck_with(vec![("s1", vec![click_entry("a1", "el_a")])]);
        let mut cur = PresentCursor::new(0);
        assert!(matches!(cur.back(&deck), PresentStep::Unchanged));
    }

    #[test]
    fn forward_cross_carries_outgoing_transition_back_carries_none() {
        use crate::deck::{SlideTransition, TransitionKind};
        // s1 owns a Push; s2 is a cut. Both have no animations (last step 0).
        let mut deck = deck_with(vec![("s1", vec![]), ("s2", vec![])]);
        deck.slides.get_mut("s1").unwrap().metadata.transition = Some(SlideTransition {
            kind: TransitionKind::Push,
            duration_ms: 500,
            easing: "ease-out".into(),
        });
        // Forward s1 -> s2: the mount payload carries s1's (outgoing) Push.
        let mut cur = PresentCursor::new(0);
        match cur.advance(&deck) {
            PresentStep::SlideChanged { slide, .. } => {
                let t = slide.transition.expect("forward cross carries transition");
                assert_eq!(t.kind, TransitionKind::Push);
                assert_eq!(t.duration_ms, 500);
            }
            other => panic!("expected SlideChanged, got {other:?}"),
        }
        // Back s2 -> s1: never animates, payload transition is None.
        match cur.back(&deck) {
            PresentStep::SlideChanged { slide, .. } => {
                assert!(
                    slide.transition.is_none(),
                    "back never carries a transition"
                );
            }
            other => panic!("expected SlideChanged, got {other:?}"),
        }
    }

    #[test]
    fn forward_cross_cut_carries_none_kind_transition() {
        use crate::deck::TransitionKind;
        // No authored transition: forward cross still carries a cut (None-kind)
        // so the frontend can tell a forward move from a backward one (and run
        // element morphs). It is a cut, not a panel animation.
        let deck = deck_with(vec![("s1", vec![]), ("s2", vec![])]);
        let mut cur = PresentCursor::new(0);
        match cur.advance(&deck) {
            PresentStep::SlideChanged { slide, .. } => {
                let t = slide.transition.expect("forward cross carries a transition");
                assert_eq!(t.kind, TransitionKind::None);
            }
            other => panic!("expected SlideChanged, got {other:?}"),
        }
    }

    #[test]
    fn current_reveal_is_a_snap_at_current_step() {
        let deck = deck_with(vec![("s1", vec![click_entry("a1", "el_a")])]);
        let cur = PresentCursor::new(0);
        let r = cur.current_reveal(&deck).expect("slide exists");
        assert_eq!(r.slide_id, "s1");
        assert!(r.animate.is_empty());
        assert_eq!(r.hidden, vec!["el_a".to_string()]);
    }
}
