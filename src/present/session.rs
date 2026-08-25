use crate::deck::animation::{AnimationState, step_count};
use crate::deck::{Deck, SlideId};
use crate::html::serialize::serialize_slide_themed;
use crate::ipc::bridge::WebviewSender;
use crate::ipc::present::{PresentSlidePayload, RevealPayload};
use crate::ipc::presenter::PresenterUpdatePayload;
use crate::present::reveal::{forward_reveal, snap_reveal};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresentStep {
    Reveal(RevealPayload),
    SlideChanged {
        slide: PresentSlidePayload,
        reveal: RevealPayload,
    },
    Unchanged,
}

pub struct PresentCursor {
    cursor: AnimationState,
    slide_index: usize,
}

impl PresentCursor {
    pub fn new(slide_index: usize) -> Self {
        Self {
            cursor: AnimationState::default(),
            slide_index,
        }
    }

    pub fn slide_index(&self) -> usize {
        self.slide_index
    }

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

    pub fn current_reveal(&self, deck: &Deck) -> Option<RevealPayload> {
        let sid: &SlideId = deck.slide_order.get(self.slide_index)?;
        let timeline = self.timeline(deck, sid);
        Some(snap_reveal(sid, &timeline, self.cursor.current_step()))
    }

    pub fn current_slide_payload(&self, deck: &Deck) -> Option<PresentSlidePayload> {
        let sid: &SlideId = deck.slide_order.get(self.slide_index)?;
        Some(slide_payload(
            deck,
            sid,
            self.slide_index + 1,
            deck.slide_order.len(),
        ))
    }

    /// next_slide_payload — renders the slide after the cursor, or `None` when
    /// the cursor sits on the last slide. Used only by the presenter console;
    /// the audience window never mounts it.
    pub fn next_slide_payload(&self, deck: &Deck) -> Option<PresentSlidePayload> {
        let index: usize = self.slide_index + 1;
        let sid: &SlideId = deck.slide_order.get(index)?;
        Some(slide_payload(deck, sid, index + 1, deck.slide_order.len()))
    }

    /// current_notes — speaker notes for the slide under the cursor, empty when
    /// the slide is missing or has no notes set.
    pub fn current_notes(&self, deck: &Deck) -> String {
        deck.slide_order
            .get(self.slide_index)
            .and_then(|sid| deck.slides.get(sid))
            .and_then(|s| s.metadata.notes.clone())
            .unwrap_or_default()
    }

    /// presenter_payload — one console frame: current slide, next slide, notes,
    /// position, and the not-yet-revealed element ids for both previews so they
    /// match what the audience sees. `None` when the cursor has no slide.
    pub fn presenter_payload(&self, deck: &Deck) -> Option<PresenterUpdatePayload> {
        let current: PresentSlidePayload = self.current_slide_payload(deck)?;
        let current_hidden: Vec<String> = self
            .current_reveal(deck)
            .map(|r| r.hidden)
            .unwrap_or_default();
        let next: Option<PresentSlidePayload> = self.next_slide_payload(deck);
        Some(PresenterUpdatePayload {
            index: self.slide_index,
            count: deck.slide_order.len(),
            width: deck.manifest.dimensions.width,
            height: deck.manifest.dimensions.height,
            current_html: current.slide_html,
            next_html: next.map(|n| n.slide_html).unwrap_or_default(),
            current_hidden,
            next_hidden: self.next_hidden(deck),
            notes: self.current_notes(deck),
            theme_css: current.theme_css,
            globals_css: current.globals_css,
        })
    }

    fn next_hidden(&self, deck: &Deck) -> Vec<String> {
        let sid: &SlideId = match deck.slide_order.get(self.slide_index + 1) {
            Some(id) => id,
            None => return Vec::new(),
        };
        let timeline = self.timeline(deck, sid);
        snap_reveal(sid, &timeline, 0).hidden
    }

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

    fn timeline(&self, deck: &Deck, sid: &str) -> Vec<crate::deck::AnimationEntry> {
        deck.slides
            .get(sid)
            .map(|s| s.animations.clone())
            .unwrap_or_default()
    }
}

fn slide_payload(deck: &Deck, sid: &str, number: usize, count: usize) -> PresentSlidePayload {
    assert!(!sid.is_empty(), "slide_payload: empty slide id");
    let opts: crate::html::serialize::RenderOpts = crate::html::serialize::RenderOpts {
        ctx: Some(crate::html::serialize::RenderCtx {
            number,
            count,
            date: crate::html::serialize::today_ymd(),
        }),
        hide_placeholders: true,
        min_element_size: 0.0,
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

pub struct PresentationSession {
    sender: WebviewSender,
    cursor: PresentCursor,
}

impl PresentationSession {
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

    pub fn presenter_payload(&self, deck: &Deck) -> Option<PresenterUpdatePayload> {
        self.cursor.presenter_payload(deck)
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
                assert!(reveal.animate.is_empty());

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
        let _ = cur.advance(&deck);
        match cur.back(&deck) {
            PresentStep::Reveal(r) => {
                assert!(r.animate.is_empty());
                assert_eq!(r.hidden, vec!["el_a".to_string()]);
            }
            other => panic!("expected Reveal, got {other:?}"),
        }
    }

    #[test]
    fn back_at_step_zero_crosses_to_prev_slide_last_step() {
        let deck = deck_with(vec![
            ("s1", vec![click_entry("a1", "el_a")]),
            ("s2", vec![]),
        ]);
        let mut cur = PresentCursor::new(1);
        match cur.back(&deck) {
            PresentStep::SlideChanged { slide, reveal } => {
                assert_eq!(slide.slide_id, "s1");
                assert!(reveal.animate.is_empty());

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

        let mut deck = deck_with(vec![("s1", vec![]), ("s2", vec![])]);
        deck.slides.get_mut("s1").unwrap().metadata.transition = Some(SlideTransition {
            kind: TransitionKind::Push,
            duration_ms: 500,
            easing: "ease-out".into(),
        });

        let mut cur = PresentCursor::new(0);
        match cur.advance(&deck) {
            PresentStep::SlideChanged { slide, .. } => {
                let t = slide.transition.expect("forward cross carries transition");
                assert_eq!(t.kind, TransitionKind::Push);
                assert_eq!(t.duration_ms, 500);
            }
            other => panic!("expected SlideChanged, got {other:?}"),
        }

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

        let deck = deck_with(vec![("s1", vec![]), ("s2", vec![])]);
        let mut cur = PresentCursor::new(0);
        match cur.advance(&deck) {
            PresentStep::SlideChanged { slide, .. } => {
                let t = slide
                    .transition
                    .expect("forward cross carries a transition");
                assert_eq!(t.kind, TransitionKind::None);
            }
            other => panic!("expected SlideChanged, got {other:?}"),
        }
    }

    #[test]
    fn next_slide_payload_is_none_on_last_slide_and_set_otherwise() {
        let deck = deck_with(vec![("s1", vec![]), ("s2", vec![])]);
        let first = PresentCursor::new(0);
        let next = first.next_slide_payload(&deck).expect("s2 follows s1");
        assert_eq!(next.slide_id, "s2");
        assert!(!next.slide_html.is_empty());

        let last = PresentCursor::new(1);
        assert!(last.next_slide_payload(&deck).is_none());
    }

    #[test]
    fn presenter_payload_carries_notes_position_and_hidden_lists() {
        let mut deck = deck_with(vec![
            ("s1", vec![]),
            ("s2", vec![click_entry("b1", "el_b")]),
        ]);
        deck.slides.get_mut("s1").unwrap().metadata.notes = Some("open strong".into());

        let cur = PresentCursor::new(0);
        let p = cur.presenter_payload(&deck).expect("slide exists");
        assert_eq!(p.index, 0);
        assert_eq!(p.count, 2);
        assert_eq!(p.notes, "open strong");
        assert!(!p.current_html.is_empty());
        assert!(!p.next_html.is_empty());

        assert_eq!(p.next_hidden, vec!["el_b".to_string()]);
    }

    #[test]
    fn presenter_payload_has_empty_notes_and_next_on_last_slide() {
        let deck = deck_with(vec![("s1", vec![])]);
        let cur = PresentCursor::new(0);
        let p = cur.presenter_payload(&deck).expect("slide exists");
        assert!(p.notes.is_empty(), "unset notes come through empty");
        assert!(p.next_html.is_empty(), "last slide has no next preview");
        assert!(p.next_hidden.is_empty());
    }

    #[test]
    fn presenter_payload_is_none_for_out_of_range_cursor() {
        let deck = deck_with(vec![("s1", vec![])]);
        let cur = PresentCursor::new(7);
        assert!(cur.presenter_payload(&deck).is_none());
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
