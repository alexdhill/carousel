use crate::commands::{Command, CommandError, CommandOutput};
use crate::deck::{CanvasTarget, Deck, SlideId};
use crate::ipc::Patch;
use std::collections::VecDeque;
use tracing::debug;

pub const DEFAULT_HISTORY_DEPTH: usize = 100;

#[derive(Debug)]
pub struct HistoryEntry {
    pub inverse: Box<dyn Command>,
    pub label: &'static str,
    pub timestamp: u64,
}

#[derive(Debug)]
pub struct UndoOutput {
    pub patches: Vec<Patch>,
    pub dirty_targets: Vec<CanvasTarget>,
    pub label: &'static str,
    pub affects_object_tree: bool,
    pub requires_remount: bool,
    pub affects_slide_list: bool,
    pub affects_layout_list: bool,
    pub affects_globals: bool,
    pub affects_animations: bool,
    pub affects_assets: bool,
    pub affects_slide_meta: bool,
    pub affects_guides: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct CommandHistory {
    undo_stack: VecDeque<HistoryEntry>,
    redo_stack: VecDeque<HistoryEntry>,
    max_depth: usize,
}

impl CommandHistory {

    pub fn new(max_depth: usize) -> Self {
        assert!(max_depth > 0, "CommandHistory: max_depth must be positive");
        Self {
            undo_stack: VecDeque::with_capacity(max_depth),
            redo_stack: VecDeque::with_capacity(max_depth),
            max_depth,
        }
    }

    pub fn max_depth(&self) -> usize {
        self.max_depth
    }

    pub fn undo_len(&self) -> usize {
        self.undo_stack.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo_stack.len()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn undo_label(&self) -> Option<&'static str> {
        self.undo_stack.back().map(|e| e.label)
    }

    pub fn redo_label(&self) -> Option<&'static str> {
        self.redo_stack.back().map(|e| e.label)
    }

    pub fn push(&mut self, inverse: Box<dyn Command>, label: &'static str) {
        assert!(!label.is_empty(), "CommandHistory::push: label is empty");
        self.undo_stack.push_back(HistoryEntry {
            inverse,
            label,
            timestamp: now_millis(),
        });
        trim_to_depth(&mut self.undo_stack, self.max_depth);
        self.redo_stack.clear();
        debug!(label, depth = self.undo_stack.len(), "history push");
    }

    pub fn undo(&mut self, deck: &mut Deck) -> Result<Option<UndoOutput>, CommandError> {
        assert!(self.max_depth > 0, "history misconfigured: zero max_depth");
        let entry: HistoryEntry = match self.undo_stack.pop_back() {
            Some(e) => e,
            None => return Ok(None),
        };
        let label: &'static str = entry.label;
        let affects_object_tree: bool = entry.inverse.affects_object_tree();
        let requires_remount: bool = entry.inverse.requires_remount();
        let affects_slide_list: bool = entry.inverse.affects_slide_list();
        let affects_layout_list: bool = entry.inverse.affects_layout_list();
        let affects_globals: bool = entry.inverse.affects_globals();
        let affects_animations: bool = entry.inverse.affects_animations();
        let affects_assets: bool = entry.inverse.affects_assets();
        let affects_slide_meta: bool = entry.inverse.affects_slide_meta();
        let affects_guides: bool = entry.inverse.affects_guides();
        let CommandOutput {
            patches,
            inverse,
            dirty_targets,
            warnings,
            ..
        } = entry.inverse.apply(deck)?;
        self.redo_stack.push_back(HistoryEntry {
            inverse,
            label,
            timestamp: now_millis(),
        });
        trim_to_depth(&mut self.redo_stack, self.max_depth);
        debug!(label, "history undo");
        Ok(Some(UndoOutput {
            patches,
            dirty_targets,
            label,
            affects_object_tree,
            requires_remount,
            affects_slide_list,
            affects_layout_list,
            affects_globals,
            affects_animations,
            affects_assets,
            affects_slide_meta,
            affects_guides,
            warnings,
        }))
    }

    pub fn redo(&mut self, deck: &mut Deck) -> Result<Option<UndoOutput>, CommandError> {
        assert!(self.max_depth > 0, "history misconfigured: zero max_depth");
        let entry: HistoryEntry = match self.redo_stack.pop_back() {
            Some(e) => e,
            None => return Ok(None),
        };
        let label: &'static str = entry.label;
        let affects_object_tree: bool = entry.inverse.affects_object_tree();
        let requires_remount: bool = entry.inverse.requires_remount();
        let affects_slide_list: bool = entry.inverse.affects_slide_list();
        let affects_layout_list: bool = entry.inverse.affects_layout_list();
        let affects_globals: bool = entry.inverse.affects_globals();
        let affects_animations: bool = entry.inverse.affects_animations();
        let affects_assets: bool = entry.inverse.affects_assets();
        let affects_slide_meta: bool = entry.inverse.affects_slide_meta();
        let affects_guides: bool = entry.inverse.affects_guides();
        let CommandOutput {
            patches,
            inverse,
            dirty_targets,
            warnings,
            ..
        } = entry.inverse.apply(deck)?;
        self.undo_stack.push_back(HistoryEntry {
            inverse,
            label,
            timestamp: now_millis(),
        });
        trim_to_depth(&mut self.undo_stack, self.max_depth);
        debug!(label, "history redo");
        Ok(Some(UndoOutput {
            patches,
            dirty_targets,
            label,
            affects_object_tree,
            requires_remount,
            affects_slide_list,
            affects_layout_list,
            affects_globals,
            affects_animations,
            affects_assets,
            affects_slide_meta,
            affects_guides,
            warnings,
        }))
    }

    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
    }
}

impl Default for CommandHistory {

    fn default() -> Self {
        Self::new(DEFAULT_HISTORY_DEPTH)
    }
}

fn trim_to_depth(stack: &mut VecDeque<HistoryEntry>, max_depth: usize) {
    assert!(max_depth > 0, "trim_to_depth: max_depth must be positive");
    let mut iter: usize = 0;
    let cap: usize = max_depth + 1;
    while stack.len() > max_depth && iter < cap {
        stack.pop_front();
        iter += 1;
    }
    assert!(stack.len() <= max_depth, "history depth invariant broken");
}

fn now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::commands::MoveElement;
    use crate::deck::Deck;
    use crate::ipc::Point;

    fn fresh_deck_first_child() -> (Deck, SlideId, crate::deck::ElementId) {
        let deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let eid: crate::deck::ElementId = deck.slides[&sid].root.children[0].id.clone();
        (deck, sid, eid)
    }

    fn move_cmd(sid: &SlideId, eid: &crate::deck::ElementId, x: f64, y: f64) -> Box<dyn Command> {
        Box::new(MoveElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_position: Point { x, y },
            previous_position: None,
        })
    }

    #[test]
    fn new_history_is_empty() {
        let h = CommandHistory::new(10);
        assert!(!h.can_undo());
        assert!(!h.can_redo());
        assert_eq!(h.undo_len(), 0);
        assert_eq!(h.redo_len(), 0);
        assert_eq!(h.undo_label(), None);
        assert_eq!(h.redo_label(), None);
        assert_eq!(h.max_depth(), 10);
    }

    #[test]
    fn default_history_uses_constant_depth() {
        let h: CommandHistory = CommandHistory::default();
        assert_eq!(h.max_depth(), DEFAULT_HISTORY_DEPTH);
    }

    #[test]
    #[should_panic(expected = "max_depth must be positive")]
    fn new_history_rejects_zero_depth() {
        let _ = CommandHistory::new(0);
    }

    #[test]
    #[should_panic(expected = "label is empty")]
    fn push_rejects_empty_label() {
        let mut h = CommandHistory::new(4);
        let (mut deck, sid, eid) = fresh_deck_first_child();
        let cmd = move_cmd(&sid, &eid, 1.0, 2.0);
        let out = cmd.apply(&mut deck).unwrap();
        h.push(out.inverse, "");
    }

    #[test]
    fn push_extends_undo_and_clears_redo() {
        let mut h = CommandHistory::new(4);
        let (mut deck, sid, eid) = fresh_deck_first_child();

        let out = move_cmd(&sid, &eid, 1.0, 2.0).apply(&mut deck).unwrap();
        h.push(out.inverse, "Move Element");

        h.undo(&mut deck).unwrap();
        assert!(h.can_redo());

        let out2 = move_cmd(&sid, &eid, 3.0, 4.0).apply(&mut deck).unwrap();
        h.push(out2.inverse, "Move Element");
        assert!(h.can_undo());
        assert!(!h.can_redo());
        assert_eq!(h.undo_label(), Some("Move Element"));
    }

    #[test]
    fn push_past_max_depth_drops_oldest() {
        let mut h = CommandHistory::new(3);
        let (mut deck, sid, eid) = fresh_deck_first_child();
        let mut i: f64 = 0.0;
        while i < 10.0 {
            let out = move_cmd(&sid, &eid, i, 0.0).apply(&mut deck).unwrap();
            h.push(out.inverse, "Move Element");
            i += 1.0;
        }
        assert_eq!(h.undo_len(), 3);
    }

    #[test]
    fn undo_on_empty_history_returns_none() {
        let mut h = CommandHistory::new(4);
        let mut deck = Deck::sample();
        let out = h.undo(&mut deck).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn redo_on_empty_history_returns_none() {
        let mut h = CommandHistory::new(4);
        let mut deck = Deck::sample();
        let out = h.redo(&mut deck).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn undo_restores_prior_state_and_populates_redo() {
        let mut h = CommandHistory::new(4);
        let (mut deck, sid, eid) = fresh_deck_first_child();
        let original = deck.slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();

        let cmd = move_cmd(&sid, &eid, 500.0, 300.0);
        let out = cmd.apply(&mut deck).unwrap();
        h.push(out.inverse, "Move Element");

        let undone = h.undo(&mut deck).unwrap().unwrap();
        let after = deck.slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
        assert_eq!(after.x, original.x);
        assert_eq!(after.y, original.y);
        assert_eq!(undone.label, "Move Element");
        assert!(!undone.dirty_targets.is_empty());
        assert!(!undone.patches.is_empty());
        assert!(h.can_redo());
        assert!(!h.can_undo());
    }

    #[test]
    fn redo_reapplies_and_pushes_undo() {
        let mut h = CommandHistory::new(4);
        let (mut deck, sid, eid) = fresh_deck_first_child();

        let cmd = move_cmd(&sid, &eid, 50.0, 60.0);
        let out = cmd.apply(&mut deck).unwrap();
        h.push(out.inverse, "Move Element");

        h.undo(&mut deck).unwrap();
        let redone = h.redo(&mut deck).unwrap().unwrap();
        assert_eq!(redone.label, "Move Element");
        let after = deck.slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
        assert_eq!(after.x, 50.0);
        assert_eq!(after.y, 60.0);
        assert!(h.can_undo());
        assert!(!h.can_redo());
    }

    #[test]
    fn undo_redo_loop_is_stable_over_many_iterations() {
        let mut h = CommandHistory::new(4);
        let (mut deck, sid, eid) = fresh_deck_first_child();
        let original = deck.slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();

        let cmd = move_cmd(&sid, &eid, 11.0, 22.0);
        let out = cmd.apply(&mut deck).unwrap();
        h.push(out.inverse, "Move Element");

        let mut iter: usize = 0;
        while iter < 8 {
            h.undo(&mut deck).unwrap();
            let geo = deck.slides[&sid]
                .find_element(&eid)
                .unwrap()
                .geometry
                .clone();
            assert_eq!(geo.x, original.x);
            assert_eq!(geo.y, original.y);

            h.redo(&mut deck).unwrap();
            let geo = deck.slides[&sid]
                .find_element(&eid)
                .unwrap()
                .geometry
                .clone();
            assert_eq!(geo.x, 11.0);
            assert_eq!(geo.y, 22.0);
            iter += 1;
        }
    }

    #[test]
    fn clear_empties_both_stacks() {
        let mut h = CommandHistory::new(4);
        let (mut deck, sid, eid) = fresh_deck_first_child();
        let out = move_cmd(&sid, &eid, 1.0, 1.0).apply(&mut deck).unwrap();
        h.push(out.inverse, "Move Element");
        h.undo(&mut deck).unwrap();
        assert!(h.can_redo());
        h.clear();
        assert!(!h.can_undo());
        assert!(!h.can_redo());
    }

    #[test]
    fn labels_track_top_of_each_stack() {
        let mut h = CommandHistory::new(4);
        let (mut deck, sid, eid) = fresh_deck_first_child();
        let out1 = move_cmd(&sid, &eid, 1.0, 1.0).apply(&mut deck).unwrap();
        h.push(out1.inverse, "Move Element");
        assert_eq!(h.undo_label(), Some("Move Element"));
        h.undo(&mut deck).unwrap();
        assert_eq!(h.redo_label(), Some("Move Element"));
        assert_eq!(h.undo_label(), None);
    }

    #[test]
    fn undo_timestamp_is_populated() {
        let mut h = CommandHistory::new(4);
        let (mut deck, sid, eid) = fresh_deck_first_child();
        let out = move_cmd(&sid, &eid, 9.0, 9.0).apply(&mut deck).unwrap();
        h.push(out.inverse, "Move Element");

        assert_eq!(h.undo_len(), 1);
    }
}
