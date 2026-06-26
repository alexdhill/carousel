// CompositeCommand.
//
// SPEC §9.5 — bundles multiple sub-commands into one logical operation so
// that a transaction commit (or any caller assembling a batched edit) can
// push exactly one history entry. `apply` runs each sub-command in order,
// concatenates their patches and dirty-slide lists, and constructs an
// inverse that is itself a CompositeCommand whose sub-commands are the
// per-step inverses **in reversed order** — so undoing unwinds the
// sequence latest-first.
//
// Failure semantics: the first sub-command error propagates and aborts the
// composite. Sub-commands that applied before the failure are NOT rolled
// back automatically; Stage 6 accepts this risk for drag-style transactions
// where partial failure is implausible (per ROADMAP §6 debugging note).

use crate::commands::{Command, CommandError, CommandOutput};
use crate::deck::{CanvasTarget, Deck};
use crate::ipc::Patch;

#[derive(Debug)]
pub struct CompositeCommand {
    pub commands: Vec<Box<dyn Command>>,
    pub label: &'static str,
}

impl CompositeCommand {
    // new
    // Inputs: a Vec of sub-commands, a stable label.
    // Output: a CompositeCommand instance.
    // Errors: asserts non-empty commands and non-empty label.
    // Dataflow: pure constructor.
    pub fn new(commands: Vec<Box<dyn Command>>, label: &'static str) -> Self {
        assert!(!label.is_empty(), "CompositeCommand: label is empty");
        assert!(
            !commands.is_empty(),
            "CompositeCommand: needs at least one sub-command"
        );
        Self { commands, label }
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

impl Command for CompositeCommand {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput whose patches and dirty_slides concatenate
    // those of every sub-command in dispatch order; manifest_dirty is the
    // logical-or of sub-commands'. The inverse is a CompositeCommand whose
    // sub-commands are the per-step inverses in reversed order.
    // Errors: first sub-command failure aborts and propagates; earlier
    // sub-commands remain applied (see module-level doc).
    // Dataflow: iterate sub-commands with a bounded counter -> for each,
    // apply against deck and collect (patches, dirty, inverse) -> reverse
    // inverses -> assemble output.
    fn apply(&self, deck: &mut Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.commands.is_empty(),
            "CompositeCommand::apply: no sub-commands"
        );
        let n: usize = self.commands.len();
        let mut patches: Vec<Patch> = Vec::new();
        let mut dirty_targets: Vec<CanvasTarget> = Vec::new();
        let mut manifest_dirty: bool = false;
        let mut warnings: Vec<String> = Vec::new();
        let mut inverses: Vec<Box<dyn Command>> = Vec::with_capacity(n);

        let mut i: usize = 0;
        while i < n {
            let out: CommandOutput = self.commands[i].apply(deck)?;
            patches.extend(out.patches);
            dirty_targets.extend(out.dirty_targets);
            warnings.extend(out.warnings);
            if out.manifest_dirty {
                manifest_dirty = true;
            }
            inverses.push(out.inverse);
            i += 1;
        }
        inverses.reverse();

        Ok(CommandOutput {
            patches,
            inverse: Box::new(CompositeCommand {
                commands: inverses,
                label: self.label,
            }),
            dirty_targets,
            manifest_dirty,
            warnings,
        })
    }

    fn label(&self) -> &'static str {
        self.label
    }

    fn affects_object_tree(&self) -> bool {
        self.commands.iter().any(|c| c.affects_object_tree())
    }

    fn requires_remount(&self) -> bool {
        self.commands.iter().any(|c| c.requires_remount())
    }

    fn affects_slide_list(&self) -> bool {
        self.commands.iter().any(|c| c.affects_slide_list())
    }

    // Propagate the remaining rebroadcast flags so a bundled edit (e.g. a
    // drag that reorders + retriggers an animation) still tells the editor to
    // resync the relevant pane. Omitting these silently drops the refresh —
    // the model changes but the UI never updates.
    fn affects_layout_list(&self) -> bool {
        self.commands.iter().any(|c| c.affects_layout_list())
    }

    fn affects_animations(&self) -> bool {
        self.commands.iter().any(|c| c.affects_animations())
    }

    fn affects_slide_meta(&self) -> bool {
        self.commands.iter().any(|c| c.affects_slide_meta())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::commands::MoveElement;
    use crate::deck::{Deck, ElementId, SlideId};
    use crate::ipc::Point;

    fn fresh_deck_with_two_children() -> (Deck, SlideId, ElementId, ElementId) {
        let deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let eid_a: ElementId = deck.slides[&sid].root.children[0].id.clone();
        let eid_b: ElementId = deck.slides[&sid].root.children[1].id.clone();
        (deck, sid, eid_a, eid_b)
    }

    fn move_cmd(sid: &SlideId, eid: &ElementId, x: f64, y: f64) -> Box<dyn Command> {
        Box::new(MoveElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_position: Point { x, y },
            previous_position: None,
        })
    }

    #[test]
    #[should_panic(expected = "label is empty")]
    fn new_rejects_empty_label() {
        let _ = CompositeCommand::new(vec![], "");
    }

    #[test]
    #[should_panic(expected = "at least one sub-command")]
    fn new_rejects_empty_commands() {
        let _ = CompositeCommand::new(Vec::<Box<dyn Command>>::new(), "Composite");
    }

    #[test]
    fn label_is_stable() {
        let (_, sid, eid, _) = fresh_deck_with_two_children();
        let cc = CompositeCommand::new(vec![move_cmd(&sid, &eid, 1.0, 1.0)], "Multi-Move");
        assert_eq!(cc.label(), "Multi-Move");
        assert_eq!(cc.len(), 1);
        assert!(!cc.is_empty());
    }

    #[test]
    fn apply_runs_sub_commands_in_order() {
        let (mut deck, sid, eid_a, eid_b) = fresh_deck_with_two_children();
        let cc = CompositeCommand::new(
            vec![
                move_cmd(&sid, &eid_a, 100.0, 200.0),
                move_cmd(&sid, &eid_b, 300.0, 400.0),
            ],
            "Multi-Move",
        );
        let out = cc.apply(&mut deck).unwrap();
        let geo_a = deck.slides[&sid]
            .find_element(&eid_a)
            .unwrap()
            .geometry
            .clone();
        let geo_b = deck.slides[&sid]
            .find_element(&eid_b)
            .unwrap()
            .geometry
            .clone();
        assert_eq!(geo_a.x, 100.0);
        assert_eq!(geo_a.y, 200.0);
        assert_eq!(geo_b.x, 300.0);
        assert_eq!(geo_b.y, 400.0);
        // Each MoveElement produces two patches (left + top), so 4 total.
        assert_eq!(out.patches.len(), 4);
        // Dirty targets aggregated.
        assert_eq!(out.dirty_targets.len(), 2);
        assert!(
            out.dirty_targets
                .iter()
                .all(|t| t == &CanvasTarget::Slide(sid.clone()))
        );
        assert!(!out.manifest_dirty);
    }

    #[test]
    fn inverse_reverses_sub_command_order() {
        let (mut deck, sid, eid_a, eid_b) = fresh_deck_with_two_children();
        let original_a = deck.slides[&sid]
            .find_element(&eid_a)
            .unwrap()
            .geometry
            .clone();
        let original_b = deck.slides[&sid]
            .find_element(&eid_b)
            .unwrap()
            .geometry
            .clone();
        let cc = CompositeCommand::new(
            vec![
                move_cmd(&sid, &eid_a, 11.0, 22.0),
                move_cmd(&sid, &eid_b, 33.0, 44.0),
            ],
            "Multi-Move",
        );
        let out = cc.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();
        let geo_a = deck.slides[&sid]
            .find_element(&eid_a)
            .unwrap()
            .geometry
            .clone();
        let geo_b = deck.slides[&sid]
            .find_element(&eid_b)
            .unwrap()
            .geometry
            .clone();
        assert_eq!(geo_a.x, original_a.x);
        assert_eq!(geo_a.y, original_a.y);
        assert_eq!(geo_b.x, original_b.x);
        assert_eq!(geo_b.y, original_b.y);
    }

    #[test]
    fn apply_inverse_inverse_returns_to_post_apply_state() {
        let (mut deck, sid, eid_a, eid_b) = fresh_deck_with_two_children();
        let cc = CompositeCommand::new(
            vec![
                move_cmd(&sid, &eid_a, 50.0, 60.0),
                move_cmd(&sid, &eid_b, 70.0, 80.0),
            ],
            "Multi-Move",
        );
        let first = cc.apply(&mut deck).unwrap();
        let second = first.inverse.apply(&mut deck).unwrap();
        second.inverse.apply(&mut deck).unwrap();
        let geo_a = deck.slides[&sid]
            .find_element(&eid_a)
            .unwrap()
            .geometry
            .clone();
        let geo_b = deck.slides[&sid]
            .find_element(&eid_b)
            .unwrap()
            .geometry
            .clone();
        assert_eq!(geo_a.x, 50.0);
        assert_eq!(geo_a.y, 60.0);
        assert_eq!(geo_b.x, 70.0);
        assert_eq!(geo_b.y, 80.0);
    }

    #[test]
    fn inverse_label_propagated() {
        let (mut deck, sid, eid, _) = fresh_deck_with_two_children();
        let cc = CompositeCommand::new(vec![move_cmd(&sid, &eid, 1.0, 2.0)], "Multi-Move");
        let out = cc.apply(&mut deck).unwrap();
        assert_eq!(out.inverse.label(), "Multi-Move");
    }

    #[test]
    fn apply_propagates_first_error() {
        let (mut deck, sid, eid, _) = fresh_deck_with_two_children();
        let cc = CompositeCommand::new(
            vec![
                move_cmd(&"ghost".into(), &eid, 1.0, 2.0),
                move_cmd(&sid, &eid, 3.0, 4.0),
            ],
            "Multi-Move",
        );
        let err = cc.apply(&mut deck).unwrap_err();
        assert!(matches!(err, CommandError::SlideNotFound(_)));
    }

    #[test]
    fn propagates_affects_animations_from_children() {
        use crate::commands::ReorderAnimation;
        let (_, sid, _, _) = fresh_deck_with_two_children();
        let cc = CompositeCommand::new(
            vec![Box::new(ReorderAnimation {
                slide_id: sid,
                animation_id: "a1".into(),
                new_position: 0,
            })],
            "Move Animation",
        );
        // ReorderAnimation reports affects_animations; the composite must too,
        // else react_to_outcome never rebroadcasts the timeline.
        assert!(cc.affects_animations());
    }

    #[test]
    fn single_sub_command_composite_round_trips() {
        let (mut deck, sid, eid, _) = fresh_deck_with_two_children();
        let original = deck.slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
        let cc = CompositeCommand::new(vec![move_cmd(&sid, &eid, 999.0, -1.0)], "Single");
        let out = cc.apply(&mut deck).unwrap();
        out.inverse.apply(&mut deck).unwrap();
        let geo = deck.slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
        assert_eq!(geo.x, original.x);
        assert_eq!(geo.y, original.y);
    }
}
