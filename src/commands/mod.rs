// Commands and the dispatcher.
//
// SPEC §9.1–9.6. Each Command knows how to (a) mutate the deck and (b)
// describe its inverse, returning both alongside a list of `Patch`es and
// the slides that became dirty.
//
// Stage 5 added Transaction tracking: while a transaction is open, every
// dispatched command's patches and dirty-slide set are folded into the
// transaction's accumulators.
//
// Stage 6 adds the history stack. Outside of an open transaction, each
// undoable dispatch pushes its inverse onto CommandHistory; transaction
// commit builds a composite inverse from the start snapshot and pushes a
// single history entry. `undo` / `redo` on the dispatcher delegate to the
// history with the deck reference.

#![allow(dead_code, unused_imports)]

pub mod animation;
pub mod composite;
pub mod history;
pub mod insert_element;
pub mod layout_lifecycle;
pub mod move_element;
pub mod patch_buffer;
pub mod remove_element;
pub mod rename_element;
pub mod reparent_element;
pub mod resize_element;
pub mod set_geometry;
pub mod set_inline_style;
pub mod set_element_id;
pub mod set_text;
pub mod slide_lifecycle;
pub mod slide_metadata;
pub mod theme_globals;
pub mod transactions;

use crate::deck::element::ElementContent;
use crate::deck::{Canvas, CanvasTarget, Deck, ElementId, LayoutId, SlideId};
use crate::ipc::{Patch, Point, SelectionState};
use tracing::{debug, warn};

pub use animation::{InsertAnimation, RemoveAnimation, ReorderAnimation, SetAnimationProperty};
pub use composite::CompositeCommand;
pub use history::{CommandHistory, DEFAULT_HISTORY_DEPTH, HistoryEntry, UndoOutput};
pub use rename_element::RenameElement;
pub use reparent_element::ReparentElement;
pub use resize_element::ResizeElement;
pub use set_geometry::{GeometryProperty, SetGeometryProperty};
pub use set_inline_style::{RemoveInlineStyle, SetInlineStyle};
// FileAction is re-exported below via the public InterpretResult enum.
pub use insert_element::InsertElement;
pub use layout_lifecycle::{InsertLayout, RemoveLayout, SetLayoutName};
pub use move_element::MoveElement;
pub use patch_buffer::PatchBuffer;
pub use remove_element::RemoveElementCommand;
pub use set_element_id::SetElementId;
pub use set_text::SetTextContent;
pub use slide_lifecycle::{InsertSlide, RemoveSlide};
pub use slide_metadata::SetSlideTitle;
pub use theme_globals::SetGlobalsCss;
pub use transactions::{Transaction, TransactionSnapshot};

// Command
// SPEC §9.1. Every editor mutation implements this trait. `apply` runs the
// mutation and returns the inverse so a future history stack can record it.
//
// Default-false trait methods declare cross-cutting effects the dispatcher
// reacts to after apply:
//   - `affects_object_tree` (Stage 9) — a SetAttribute(data-name) edit
//     does not change DOM geometry, but the object panel still needs to
//     refresh. Insert / Remove / Reparent / Rename override to true.
//   - `requires_remount` (Stage 9) — when the element tree's child order
//     or membership changed, the slide must be re-serialised and
//     re-mounted so the per-child z-index stack and DOM parentage match
//     the tree. Insert / Remove / Reparent override to true.
pub trait Command: Send + Sync + std::fmt::Debug {
    fn apply(&self, deck: &mut Deck) -> Result<CommandOutput, CommandError>;
    fn label(&self) -> &'static str;
    fn undoable(&self) -> bool {
        true
    }
    fn affects_object_tree(&self) -> bool {
        false
    }
    fn requires_remount(&self) -> bool {
        false
    }
    // affects_slide_list (Stage 10) — the command changed the deck's
    // set or order of slides (add / remove / reorder / duplicate). The
    // dispatcher reacts by rebroadcasting SlideListUpdate and, if the
    // active slide was the one removed, re-anchoring it to a valid
    // slide before remounting.
    fn affects_slide_list(&self) -> bool {
        false
    }
    // affects_layout_list (Stage 11) — the command changed the theme's set
    // or order of layouts, or a layout's display name. The editor reacts by
    // rebroadcasting the layout list.
    fn affects_layout_list(&self) -> bool {
        false
    }
    // affects_globals (Stage 11) — the command changed the deck-wide
    // globals CSS blob. The editor reacts by re-mounting the active canvas
    // so the new CSS is visible immediately.
    fn affects_globals(&self) -> bool {
        false
    }
    // affects_animations (animations) — the command changed a slide's
    // animation timeline. The editor reacts by rebroadcasting
    // SlideAnimationsUpdate for the active slide.
    fn affects_animations(&self) -> bool {
        false
    }
}

// CommandOutput
// The four artifacts every command produces:
// - patches: DOM mutations to be shipped to the webview.
// - inverse: a Command that, when applied, undoes this one.
// - dirty_targets: canvas targets whose persistence state changed. Slide
//   targets feed `deck.dirty_slides` (quicksave); layout targets set the
//   layout's `dirty` flag. Slide- and layout-level lifecycle commands
//   report the canvas they touched here too.
// - manifest_dirty: true if deck-level metadata changed.
#[derive(Debug)]
pub struct CommandOutput {
    pub patches: Vec<Patch>,
    pub inverse: Box<dyn Command>,
    pub dirty_targets: Vec<CanvasTarget>,
    pub manifest_dirty: bool,
    // Non-fatal advisory messages (e.g. an add-time ordering accommodation).
    // The command still applied; the dispatcher surfaces these as Notices.
    pub warnings: Vec<String>,
}

// CommandError
// Per SPEC §9.1. Wraps the failure modes that any command might surface.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("element {0} not found")]
    ElementNotFound(ElementId),
    #[error("slide {0} not found")]
    SlideNotFound(SlideId),
    #[error("layout {0} not found")]
    LayoutNotFound(LayoutId),
    #[error("animation {0} not found")]
    AnimationNotFound(String),
    #[error("invalid operation: {0}")]
    InvalidOperation(String),
    #[error("nesting depth exceeded")]
    DepthExceeded,
    #[error("conflict: {0}")]
    Conflict(String),
}

// resolve_canvas_mut
// Inputs: the deck and a CanvasTarget.
// Output: a `&mut dyn Canvas` for the target, or the variant-appropriate
// CommandError (SlideNotFound / LayoutNotFound) when the target is absent.
// Shared by every element command so the resolve-or-error step is written
// once.
pub fn resolve_canvas_mut<'a>(
    deck: &'a mut Deck,
    target: &CanvasTarget,
) -> Result<&'a mut dyn Canvas, CommandError> {
    deck.canvas_mut(target).ok_or_else(|| canvas_not_found(target))
}

// canvas_not_found
// Inputs: the missing target.
// Output: the matching not-found CommandError variant.
pub fn canvas_not_found(target: &CanvasTarget) -> CommandError {
    match target {
        CanvasTarget::Slide(s) => CommandError::SlideNotFound(s.clone()),
        CanvasTarget::Layout(l) => CommandError::LayoutNotFound(l.clone()),
    }
}

// DispatchOutcome
// Returned to the caller of `CommandDispatcher::dispatch` (and of
// `undo`/`redo`) so the event loop can decide what follow-up work to
// schedule:
//   - needs_flush: the patch buffer transitioned empty → non-empty;
//     post a FlushPatches user event.
//   - affects_object_tree: the dispatched command (or its inverse, on
//     undo/redo) reports that the slide's element tree shape or names
//     changed; the ApplicationCore should rebroadcast ObjectTreeUpdate.
//   - requires_remount: the dispatched command (or its inverse) reports
//     that the slide must be re-serialised and re-mounted because
//     z-index ordering or DOM parentage changed; the ApplicationCore
//     should re-send MountSlide for the active slide.
// `warnings` makes this non-`Copy`; callers move or clone it (they already
// read its bool fields before handing the whole outcome to react_to_outcome).
#[derive(Debug, Default, Clone)]
pub struct DispatchOutcome {
    pub needs_flush: bool,
    pub affects_object_tree: bool,
    pub requires_remount: bool,
    pub affects_slide_list: bool,
    pub affects_layout_list: bool,
    pub affects_globals: bool,
    pub affects_animations: bool,
    pub warnings: Vec<String>,
}

// FileAction
// Direction tag for InterpretResult::FileAction. One variant per
// File-menu accelerator the JS host can fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    New,
    Open,
    Save,
    SaveAs,
}

// InterpretResult
// The output of `ApplicationCore::interpret`. Maps an inbound
// InteractionEvent to a concrete next-step instruction:
//   Command            — apply as a one-shot mutation.
//   Selection          — replace editor selection state and notify JS.
//   TransactionBegin   — open a transaction with a label + snapshot.
//   TransactionUpdate  — apply a command inside the open transaction.
//   TransactionCommit  — close the open transaction.
//   CommitTransactionWith — apply, then close. Drag-end's path.
//   Undo / Redo        — pop the next history entry (Stage 6).
//   FileAction         — New / Open / Save / SaveAs (Stage 7).
//   Nothing            — event is currently a no-op (e.g., unhandled).
pub enum InterpretResult {
    Command(Box<dyn Command>),
    Selection(SelectionState),
    TransactionBegin {
        label: &'static str,
        snapshot: TransactionSnapshot,
    },
    TransactionUpdate(Box<dyn Command>),
    TransactionCommit,
    CommitTransactionWith(Box<dyn Command>),
    Undo,
    Redo,
    FileAction(FileAction),
    // SetActiveSlide
    // Stage 10 — thumbnail navigation. Non-undoable: switching slides
    // is an editor-state change, not a deck-state change. The handler
    // flushes pending patches, swaps active_slide, clears selection,
    // and remounts the new slide.
    SetActiveSlide(SlideId),
    // SetEditorMode (Stage 11) — toolbar mode toggle. Non-undoable editor
    // state: the handler switches the dispatcher's mode and rebroadcasts the
    // active canvas + the relevant list.
    SetEditorMode(EditorMode),
    // SetActiveLayout (Stage 11) — layout-thumbnail navigation, the layout
    // analogue of SetActiveSlide.
    SetActiveLayout(LayoutId),
    // StartPresentation — request to enter fullscreen presentation mode from
    // the active slide. Non-undoable editor action: the handler asks the event
    // loop (via a wake) to build the presentation window.
    StartPresentation,
    Nothing,
}

impl std::fmt::Debug for InterpretResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Command(_) => f.write_str("Command(..)"),
            Self::Selection(s) => write!(f, "Selection({s:?})"),
            Self::TransactionBegin { label, .. } => write!(f, "TransactionBegin({label})"),
            Self::TransactionUpdate(_) => f.write_str("TransactionUpdate(..)"),
            Self::TransactionCommit => f.write_str("TransactionCommit"),
            Self::CommitTransactionWith(_) => f.write_str("CommitTransactionWith(..)"),
            Self::Undo => f.write_str("Undo"),
            Self::Redo => f.write_str("Redo"),
            Self::FileAction(a) => write!(f, "FileAction({a:?})"),
            Self::SetActiveSlide(id) => write!(f, "SetActiveSlide({id})"),
            Self::SetEditorMode(m) => write!(f, "SetEditorMode({m:?})"),
            Self::SetActiveLayout(id) => write!(f, "SetActiveLayout({id})"),
            Self::StartPresentation => f.write_str("StartPresentation"),
            Self::Nothing => f.write_str("Nothing"),
        }
    }
}

// EditorMode
// Which editing surface the dispatcher is currently driving. This is
// editor state (not deck state): it selects which undo/redo stack a
// dispatch/undo/redo routes through, and (via ApplicationCore) which
// canvas element commands target. Default is Slide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditorMode {
    #[default]
    Slide,
    Layout,
}

// CommandDispatcher
// Owns the deck, the patch buffer, the in-flight Transaction (Stage 5), and
// two mode-scoped CommandHistory stacks (Stage 11). The active `mode`
// selects which stack dispatch/undo/redo operate on, so editing a layout
// can never reach into the slide's history or vice versa (the isolation
// requirement). Selection state remains in ApplicationCore.
pub struct CommandDispatcher {
    deck: Deck,
    patch_buffer: PatchBuffer,
    transaction: Option<Transaction>,
    mode: EditorMode,
    slide_history: CommandHistory,
    layout_history: CommandHistory,
}

impl CommandDispatcher {
    // new
    // Inputs: an initial Deck (typically `Deck::sample()` at startup).
    // Output: a dispatcher wrapping the deck, an empty patch buffer, no
    // open transaction, Slide mode, and two DEFAULT_HISTORY_DEPTH stacks.
    pub fn new(deck: Deck) -> Self {
        Self::with_history(deck, CommandHistory::default())
    }

    // with_history
    // Inputs: an initial Deck and a preconfigured CommandHistory.
    // Output: a dispatcher whose *slide* history is the supplied one and
    // whose layout history is a default — useful in tests that pin a small
    // max_depth and operate in the default Slide mode.
    pub fn with_history(deck: Deck, history: CommandHistory) -> Self {
        Self {
            deck,
            patch_buffer: PatchBuffer::new(),
            transaction: None,
            mode: EditorMode::Slide,
            slide_history: history,
            layout_history: CommandHistory::default(),
        }
    }

    pub fn deck(&self) -> &Deck {
        &self.deck
    }

    pub fn deck_mut(&mut self) -> &mut Deck {
        &mut self.deck
    }

    pub fn patch_buffer_len(&self) -> usize {
        self.patch_buffer.len()
    }

    pub fn transaction(&self) -> Option<&Transaction> {
        self.transaction.as_ref()
    }

    pub fn has_open_transaction(&self) -> bool {
        self.transaction.is_some()
    }

    // mode / set_mode
    // The active editor mode selects the live history stack. Switching
    // modes does not touch either stack — it just changes which one
    // subsequent dispatch/undo/redo calls operate on.
    pub fn mode(&self) -> EditorMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: EditorMode) {
        self.mode = mode;
    }

    // active_history_mut
    // The history stack for the current mode. All push/undo/redo routing
    // goes through here so the two stacks stay isolated.
    fn active_history_mut(&mut self) -> &mut CommandHistory {
        match self.mode {
            EditorMode::Slide => &mut self.slide_history,
            EditorMode::Layout => &mut self.layout_history,
        }
    }

    // history
    // The active mode's history stack (read-only). UI overlays and tests
    // read undo/redo depth and labels from here.
    pub fn history(&self) -> &CommandHistory {
        match self.mode {
            EditorMode::Slide => &self.slide_history,
            EditorMode::Layout => &self.layout_history,
        }
    }

    pub fn can_undo(&self) -> bool {
        self.history().can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history().can_redo()
    }

    // begin_transaction
    // Inputs: label, start snapshot.
    // Output: side-effect; opens a transaction. If one is already open,
    // logs and replaces it — the policy layer must not begin twice but
    // we recover defensively.
    pub fn begin_transaction(&mut self, label: &'static str, snapshot: TransactionSnapshot) {
        assert!(!label.is_empty(), "begin_transaction: label is empty");
        if self.transaction.is_some() {
            warn!("begin_transaction called while one is already open; replacing");
        }
        debug!(label, "transaction begin");
        self.transaction = Some(Transaction::new(label, snapshot));
    }

    // commit_transaction
    // Inputs: none.
    // Output: the closed Transaction, or None if none was open.
    // Dataflow: take the in-flight transaction, then build a composite
    // inverse from its start snapshot. The composite restores every
    // element the transaction touched to its pre-transaction state. If
    // any sub-inverses exist, push a single history entry labelled with
    // the transaction's label (one drag = one undo). If the snapshot is
    // empty (no element was actually touched), skip the push.
    pub fn commit_transaction(&mut self) -> Option<Transaction> {
        let txn: Transaction = self.transaction.take()?;
        debug!(label = txn.label, patches = txn.patches.len(), "transaction commit");
        if let Some(inverse) = build_composite_inverse(&txn) {
            self.active_history_mut().push(inverse, txn.label);
        }
        Some(txn)
    }

    // abort_transaction
    // Inputs: none.
    // Output: drops the in-flight transaction without producing a history
    // entry. Stage 5 does not call this; included for symmetry and Stage
    // 6's escape hatch (e.g., command failure mid-transaction).
    pub fn abort_transaction(&mut self) {
        if self.transaction.take().is_some() {
            debug!("transaction abort");
        }
    }

    // dispatch
    // Inputs: a boxed Command.
    // Output: a DispatchOutcome describing whether the patch buffer became
    // non-empty as a result.
    // Errors: any CommandError raised by `apply`.
    // Dataflow:
    //   1. apply against the deck
    //   2. if a transaction is open, extend its patches + dirty-slide set
    //      (the inverse is discarded; the transaction's commit will build
    //      one composite inverse from the start snapshot)
    //   3. else if the command is undoable, push its inverse onto history
    //      as a single entry
    //   4. fold dirty_slides + manifest_dirty into the deck's bookkeeping
    //   5. append patches to the patch buffer; signal flush on empty →
    //      non-empty
    pub fn dispatch(&mut self, command: Box<dyn Command>) -> Result<DispatchOutcome, CommandError> {
        let label: &'static str = command.label();
        let undoable: bool = command.undoable();
        let affects_object_tree: bool = command.affects_object_tree();
        let requires_remount: bool = command.requires_remount();
        let affects_slide_list: bool = command.affects_slide_list();
        let affects_layout_list: bool = command.affects_layout_list();
        let affects_globals: bool = command.affects_globals();
        let affects_animations: bool = command.affects_animations();
        debug!("dispatching: {}", label);
        let output: CommandOutput = command.apply(&mut self.deck)?;
        // Reparent emits no patches yet still touches the tree, so the
        // strict "any side effect" assertion must allow that case.
        assert!(
            !output.dirty_targets.is_empty()
                || output.manifest_dirty
                || !output.patches.is_empty()
                || requires_remount,
            "command produced no side effects at all (label = {label})"
        );
        if let Some(txn) = self.transaction.as_mut() {
            txn.patches.extend(output.patches.iter().cloned());
            txn.dirty_targets.extend(output.dirty_targets.iter().cloned());
        } else if undoable {
            self.active_history_mut().push(output.inverse, label);
        }
        fold_dirty_targets(&mut self.deck, &output.dirty_targets);
        if output.manifest_dirty {
            self.deck.manifest_dirty = true;
        }
        let warnings: Vec<String> = output.warnings;
        let needs_flush: bool = self.patch_buffer.add(output.patches);
        Ok(DispatchOutcome {
            needs_flush,
            affects_object_tree,
            requires_remount,
            affects_slide_list,
            affects_layout_list,
            affects_globals,
            affects_animations,
            warnings,
        })
    }

    // undo
    // Inputs: none.
    // Output: Ok(Some(DispatchOutcome)) when an entry was applied;
    // Ok(None) when the undo stack was empty.
    // Errors: any CommandError raised by the recorded inverse's apply.
    // Dataflow: delegate to CommandHistory::undo, then fold the resulting
    // dirty_slides into the deck and queue the patches.
    pub fn undo(&mut self) -> Result<Option<DispatchOutcome>, CommandError> {
        assert!(self.transaction.is_none(), "undo while transaction is open");
        // Inline the mode→stack selection so the history borrow stays
        // field-disjoint from the `&mut self.deck` the inverse needs.
        let history: &mut CommandHistory = match self.mode {
            EditorMode::Slide => &mut self.slide_history,
            EditorMode::Layout => &mut self.layout_history,
        };
        let out: UndoOutput = match history.undo(&mut self.deck)? {
            Some(o) => o,
            None => return Ok(None),
        };
        fold_dirty_targets(&mut self.deck, &out.dirty_targets);
        let needs_flush: bool = self.patch_buffer.add(out.patches);
        Ok(Some(DispatchOutcome {
            needs_flush,
            affects_object_tree: out.affects_object_tree,
            requires_remount: out.requires_remount,
            affects_slide_list: out.affects_slide_list,
            affects_layout_list: out.affects_layout_list,
            affects_globals: out.affects_globals,
            affects_animations: out.affects_animations,
            warnings: out.warnings,
        }))
    }

    // redo
    // Inputs: none.
    // Output: Ok(Some(DispatchOutcome)) when an entry was applied;
    // Ok(None) when the redo stack was empty.
    // Errors: any CommandError raised by the recorded inverse's apply.
    // Dataflow: symmetric with undo.
    pub fn redo(&mut self) -> Result<Option<DispatchOutcome>, CommandError> {
        assert!(self.transaction.is_none(), "redo while transaction is open");
        let history: &mut CommandHistory = match self.mode {
            EditorMode::Slide => &mut self.slide_history,
            EditorMode::Layout => &mut self.layout_history,
        };
        let out: UndoOutput = match history.redo(&mut self.deck)? {
            Some(o) => o,
            None => return Ok(None),
        };
        fold_dirty_targets(&mut self.deck, &out.dirty_targets);
        let needs_flush: bool = self.patch_buffer.add(out.patches);
        Ok(Some(DispatchOutcome {
            needs_flush,
            affects_object_tree: out.affects_object_tree,
            requires_remount: out.requires_remount,
            affects_slide_list: out.affects_slide_list,
            affects_layout_list: out.affects_layout_list,
            affects_globals: out.affects_globals,
            affects_animations: out.affects_animations,
            warnings: out.warnings,
        }))
    }

    // take_patches
    // Inputs: self.
    // Output: the buffered patches with §8.4 coalescing applied.
    pub fn take_patches(&mut self) -> Vec<Patch> {
        self.patch_buffer.take_coalesced()
    }
}

// build_composite_inverse
// Inputs: a closed Transaction (its snapshot is the source of truth for
// pre-transaction state).
// Output: Some(Box<dyn Command>) when at least one element-restoring
// sub-command can be constructed; None when the snapshot is empty.
// Dataflow:
//   1. For each (slide, element) in snapshot.geometry, emit a
//      ResizeElement to restore the prior (x, y, width, height). We use
//      ResizeElement rather than MoveElement so both move-only
//      transactions (drag) and resize transactions get their full rect
//      restored from one snapshot type. For move-only transactions
//      width/height are unchanged so the extra patches are no-ops the
//      patch buffer will coalesce on the next flush.
//   2. For each (slide, element) in snapshot.content with text content,
//      emit a SetTextContent to restore the prior RichText. Non-text
//      content is skipped (no command yet covers re-asserting it).
//   3. If exactly one sub-command was built, return it directly (avoid an
//      unnecessary CompositeCommand wrapper). If multiple, wrap them under
//      the transaction's label. If none, return None.
fn build_composite_inverse(txn: &Transaction) -> Option<Box<dyn Command>> {
    let mut subs: Vec<Box<dyn Command>> = Vec::new();
    for ((target, eid), geom) in &txn.start_snapshot.geometry {
        assert!(!target.id().is_empty() && !eid.is_empty(), "snapshot has empty key");
        subs.push(Box::new(ResizeElement {
            target: target.clone(),
            element_id: eid.clone(),
            new_x: geom.x,
            new_y: geom.y,
            new_width: geom.width,
            new_height: geom.height,
        }));
    }
    for ((target, eid), content) in &txn.start_snapshot.content {
        if let ElementContent::Text(rt) = content {
            subs.push(Box::new(SetTextContent {
                target: target.clone(),
                element_id: eid.clone(),
                new_content: rt.clone(),
            }));
        }
    }
    match subs.len() {
        0 => None,
        1 => subs.pop(),
        _ => Some(Box::new(CompositeCommand::new(subs, txn.label))),
    }
}

// fold_dirty_targets
// Inputs: the deck and the dirty targets a command (or undo/redo) reported.
// Output: side-effect — slide targets join `deck.dirty_slides` (quicksave
// set); layout targets set that layout's `dirty` flag. Absent layouts are
// ignored (the command that produced the target already resolved it).
fn fold_dirty_targets(deck: &mut Deck, targets: &[CanvasTarget]) {
    for target in targets {
        match target {
            CanvasTarget::Slide(id) => {
                deck.dirty_slides.insert(id.clone());
            }
            CanvasTarget::Layout(id) => {
                if let Some(layout) = deck.theme.layouts.get_mut(id) {
                    layout.dirty = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;
    use crate::ipc::{Patch, Point};

    fn first_child_id(deck: &Deck) -> (SlideId, ElementId) {
        let slide_id: SlideId = deck.slide_order[0].clone();
        let element_id: ElementId = deck.slides[&slide_id].root.children[0].id.clone();
        (slide_id, element_id)
    }

    #[test]
    fn dispatcher_dispatches_a_move_and_buffers_patches() {
        let mut d = CommandDispatcher::new(Deck::sample());
        let (sid, eid) = first_child_id(d.deck());
        let cmd = MoveElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_position: Point { x: 500.0, y: 300.0 },
            previous_position: None,
        };
        let outcome = d.dispatch(Box::new(cmd)).unwrap();
        assert!(outcome.needs_flush);
        assert_eq!(d.patch_buffer_len(), 2);
        assert!(d.deck().dirty_slides.contains(&sid));
    }

    #[test]
    fn dispatcher_take_patches_drains_and_coalesces() {
        let mut d = CommandDispatcher::new(Deck::sample());
        let (sid, eid) = first_child_id(d.deck());
        d.dispatch(Box::new(MoveElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_position: Point { x: 10.0, y: 20.0 },
            previous_position: None,
        }))
        .unwrap();
        d.dispatch(Box::new(MoveElement {
            target: CanvasTarget::Slide(sid),
            element_id: eid,
            new_position: Point { x: 30.0, y: 40.0 },
            previous_position: None,
        }))
        .unwrap();
        let patches: Vec<Patch> = d.take_patches();
        assert_eq!(patches.len(), 2);
        for p in &patches {
            match p {
                Patch::SetStyle { value, .. } => {
                    assert!(value == "30px" || value == "40px");
                }
                other => panic!("expected SetStyle, got {other:?}"),
            }
        }
        assert_eq!(d.patch_buffer_len(), 0);
    }

    #[test]
    fn dispatcher_propagates_command_errors() {
        let mut d = CommandDispatcher::new(Deck::sample());
        let bogus = MoveElement {
            target: CanvasTarget::Slide("no_such_slide".into()),
            element_id: "x".into(),
            new_position: Point { x: 0.0, y: 0.0 },
            previous_position: None,
        };
        let result = d.dispatch(Box::new(bogus));
        assert!(matches!(result, Err(CommandError::SlideNotFound(_))));
    }

    #[test]
    fn dispatcher_begin_and_commit_round_trip() {
        let mut d = CommandDispatcher::new(Deck::sample());
        assert!(!d.has_open_transaction());
        d.begin_transaction("Move Element", TransactionSnapshot::empty());
        assert!(d.has_open_transaction());
        let txn = d.commit_transaction().unwrap();
        assert_eq!(txn.label, "Move Element");
        assert!(!d.has_open_transaction());
    }

    #[test]
    fn dispatcher_commit_without_begin_returns_none() {
        let mut d = CommandDispatcher::new(Deck::sample());
        assert!(d.commit_transaction().is_none());
    }

    #[test]
    fn dispatcher_accumulates_patches_into_open_transaction() {
        let mut d = CommandDispatcher::new(Deck::sample());
        let (sid, eid) = first_child_id(d.deck());
        d.begin_transaction("Move Element", TransactionSnapshot::empty());
        d.dispatch(Box::new(MoveElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_position: Point { x: 11.0, y: 22.0 },
            previous_position: None,
        }))
        .unwrap();
        // Two patches buffered (left + top) AND two patches in transaction.
        assert_eq!(d.patch_buffer_len(), 2);
        assert_eq!(d.transaction().unwrap().patches.len(), 2);
        assert!(d
            .transaction()
            .unwrap()
            .dirty_targets
            .contains(&CanvasTarget::Slide(sid.clone())));
    }

    #[test]
    fn dispatcher_abort_drops_open_transaction() {
        let mut d = CommandDispatcher::new(Deck::sample());
        d.begin_transaction("Move Element", TransactionSnapshot::empty());
        assert!(d.has_open_transaction());
        d.abort_transaction();
        assert!(!d.has_open_transaction());
    }

    #[test]
    fn begin_while_open_replaces_existing_transaction() {
        let mut d = CommandDispatcher::new(Deck::sample());
        d.begin_transaction("first", TransactionSnapshot::empty());
        d.begin_transaction("second", TransactionSnapshot::empty());
        assert_eq!(d.transaction().unwrap().label, "second");
    }

    // ---------- Stage 6: history wiring ----------

    fn move_to(sid: &SlideId, eid: &ElementId, x: f64, y: f64) -> Box<dyn Command> {
        Box::new(MoveElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_position: Point { x, y },
            previous_position: None,
        })
    }

    #[test]
    fn dispatch_outside_transaction_pushes_inverse_to_history() {
        let mut d = CommandDispatcher::new(Deck::sample());
        let (sid, eid) = first_child_id(d.deck());
        assert!(!d.can_undo());
        d.dispatch(move_to(&sid, &eid, 1.0, 2.0)).unwrap();
        assert!(d.can_undo());
        assert!(!d.can_redo());
        assert_eq!(d.history().undo_len(), 1);
        assert_eq!(d.history().undo_label(), Some("Move Element"));
    }

    #[test]
    fn dispatch_inside_transaction_does_not_push_history() {
        let mut d = CommandDispatcher::new(Deck::sample());
        let (sid, eid) = first_child_id(d.deck());
        d.begin_transaction("Move Element", TransactionSnapshot::empty());
        d.dispatch(move_to(&sid, &eid, 1.0, 2.0)).unwrap();
        assert!(!d.can_undo());
        assert_eq!(d.history().undo_len(), 0);
    }

    #[test]
    fn commit_transaction_with_snapshot_pushes_one_history_entry() {
        let mut d = CommandDispatcher::new(Deck::sample());
        let (sid, eid) = first_child_id(d.deck());
        let start_geo = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();

        let mut snap = TransactionSnapshot::empty();
        snap.record_geometry(CanvasTarget::Slide(sid.clone()), eid.clone(), start_geo.clone());
        d.begin_transaction("Move Element", snap);

        // 50 mid-drag dispatches should collapse to ONE history entry on commit.
        let mut i: f64 = 0.0;
        while i < 50.0 {
            d.dispatch(move_to(&sid, &eid, i, i)).unwrap();
            i += 1.0;
        }
        d.commit_transaction().unwrap();

        assert_eq!(d.history().undo_len(), 1);
        assert_eq!(d.history().undo_label(), Some("Move Element"));
    }

    #[test]
    fn commit_transaction_with_empty_snapshot_pushes_nothing() {
        let mut d = CommandDispatcher::new(Deck::sample());
        d.begin_transaction("Move Element", TransactionSnapshot::empty());
        d.commit_transaction().unwrap();
        assert!(!d.can_undo());
    }

    #[test]
    fn undo_restores_geometry_and_populates_redo() {
        let mut d = CommandDispatcher::new(Deck::sample());
        let (sid, eid) = first_child_id(d.deck());
        let original = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();

        d.dispatch(move_to(&sid, &eid, 999.0, -7.0)).unwrap();
        let _ = d.take_patches();

        let outcome = d.undo().unwrap().expect("undo should not be no-op");
        assert!(outcome.needs_flush);
        let geo = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();
        assert_eq!(geo.x, original.x);
        assert_eq!(geo.y, original.y);
        assert!(d.can_redo());
        assert!(!d.can_undo());
    }

    #[test]
    fn redo_reapplies_the_command() {
        let mut d = CommandDispatcher::new(Deck::sample());
        let (sid, eid) = first_child_id(d.deck());

        d.dispatch(move_to(&sid, &eid, 17.0, 19.0)).unwrap();
        let _ = d.take_patches();
        d.undo().unwrap();
        let _ = d.take_patches();
        d.redo().unwrap().expect("redo should not be no-op");

        let geo = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();
        assert_eq!(geo.x, 17.0);
        assert_eq!(geo.y, 19.0);
        assert!(d.can_undo());
        assert!(!d.can_redo());
    }

    #[test]
    fn undo_on_empty_history_returns_none() {
        let mut d = CommandDispatcher::new(Deck::sample());
        let out = d.undo().unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn redo_on_empty_history_returns_none() {
        let mut d = CommandDispatcher::new(Deck::sample());
        let out = d.redo().unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn new_dispatch_after_undo_clears_redo_stack() {
        let mut d = CommandDispatcher::new(Deck::sample());
        let (sid, eid) = first_child_id(d.deck());

        d.dispatch(move_to(&sid, &eid, 1.0, 1.0)).unwrap();
        d.undo().unwrap();
        assert!(d.can_redo());

        d.dispatch(move_to(&sid, &eid, 2.0, 2.0)).unwrap();
        assert!(!d.can_redo());
        assert_eq!(d.history().undo_len(), 1);
    }

    #[test]
    fn drag_transaction_undo_restores_start_position() {
        let mut d = CommandDispatcher::new(Deck::sample());
        let (sid, eid) = first_child_id(d.deck());
        let start_geo = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();

        let mut snap = TransactionSnapshot::empty();
        snap.record_geometry(CanvasTarget::Slide(sid.clone()), eid.clone(), start_geo.clone());
        d.begin_transaction("Move Element", snap);
        d.dispatch(move_to(&sid, &eid, start_geo.x + 200.0, start_geo.y + 80.0))
            .unwrap();
        d.commit_transaction().unwrap();
        let _ = d.take_patches();

        d.undo().unwrap();
        let after = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();
        assert_eq!(after.x, start_geo.x);
        assert_eq!(after.y, start_geo.y);
    }

    #[test]
    fn bounded_history_drops_oldest_on_overflow() {
        let history = CommandHistory::new(3);
        let mut d = CommandDispatcher::with_history(Deck::sample(), history);
        let (sid, eid) = first_child_id(d.deck());
        let mut i: f64 = 0.0;
        while i < 10.0 {
            d.dispatch(move_to(&sid, &eid, i, i)).unwrap();
            i += 1.0;
        }
        assert_eq!(d.history().undo_len(), 3);
    }

    #[test]
    fn undo_redo_round_trip_is_idempotent() {
        let mut d = CommandDispatcher::new(Deck::sample());
        let (sid, eid) = first_child_id(d.deck());
        let original = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();

        d.dispatch(move_to(&sid, &eid, 333.0, 444.0)).unwrap();
        let _ = d.take_patches();

        let mut iter: usize = 0;
        while iter < 6 {
            d.undo().unwrap();
            let g = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();
            assert_eq!(g.x, original.x);
            assert_eq!(g.y, original.y);

            d.redo().unwrap();
            let g = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();
            assert_eq!(g.x, 333.0);
            assert_eq!(g.y, 444.0);
            iter += 1;
        }
    }

    #[test]
    #[should_panic(expected = "undo while transaction is open")]
    fn undo_panics_when_transaction_is_open() {
        let mut d = CommandDispatcher::new(Deck::sample());
        d.begin_transaction("Move Element", TransactionSnapshot::empty());
        let _ = d.undo();
    }

    #[test]
    #[should_panic(expected = "redo while transaction is open")]
    fn redo_panics_when_transaction_is_open() {
        let mut d = CommandDispatcher::new(Deck::sample());
        d.begin_transaction("Move Element", TransactionSnapshot::empty());
        let _ = d.redo();
    }

    // ---------- Stage 11: two mode-scoped history stacks ----------

    #[test]
    fn default_mode_is_slide() {
        let d = CommandDispatcher::new(Deck::sample());
        assert_eq!(d.mode(), EditorMode::Slide);
    }

    #[test]
    fn set_mode_switches_active_stack_without_touching_either() {
        let mut d = CommandDispatcher::new(Deck::sample());
        let (sid, eid) = first_child_id(d.deck());
        d.dispatch(move_to(&sid, &eid, 1.0, 2.0)).unwrap();
        // Slide stack has one entry; switching to Layout shows an empty stack
        // but does NOT discard the slide stack.
        assert!(d.can_undo());
        d.set_mode(EditorMode::Layout);
        assert!(!d.can_undo());
        d.set_mode(EditorMode::Slide);
        assert!(d.can_undo());
    }

    #[test]
    fn undo_in_each_mode_only_touches_that_modes_tree() {
        let mut d = CommandDispatcher::new(Deck::sample());
        // Seed an element into the default "blank" layout to edit.
        d.deck_mut()
            .theme
            .layouts
            .get_mut("blank")
            .unwrap()
            .root
            .children
            .push(crate::deck::builders::text_element("el_lt", "hi"));
        let (sid, eid) = first_child_id(d.deck());
        let slide_start = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.x;

        // Slide-mode edit (default).
        d.dispatch(move_to(&sid, &eid, 111.0, 222.0)).unwrap();

        // Layout-mode edit on a separate tree.
        d.set_mode(EditorMode::Layout);
        d.dispatch(Box::new(MoveElement {
            target: CanvasTarget::Layout("blank".into()),
            element_id: "el_lt".into(),
            new_position: Point { x: 7.0, y: 8.0 },
            previous_position: None,
        }))
        .unwrap();

        // Undo in Layout mode restores only the layout; the slide is intact.
        d.undo().unwrap().expect("layout undo applies");
        assert_eq!(
            d.deck().theme.layouts["blank"].find_element("el_lt").unwrap().geometry.x,
            0.0
        );
        assert_eq!(d.deck().slides[&sid].find_element(&eid).unwrap().geometry.x, 111.0);

        // Undo in Slide mode restores only the slide; the layout is intact.
        d.set_mode(EditorMode::Slide);
        d.undo().unwrap().expect("slide undo applies");
        assert_eq!(
            d.deck().slides[&sid].find_element(&eid).unwrap().geometry.x,
            slide_start
        );
        assert_eq!(
            d.deck().theme.layouts["blank"].find_element("el_lt").unwrap().geometry.x,
            0.0
        );
    }
}
