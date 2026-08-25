#![allow(dead_code, unused_imports)]

pub mod align_commands;
pub mod animation;
pub mod composite;
pub mod group_commands;
pub mod group_relayout;
pub mod group_select;
pub mod guide_commands;
pub mod history;
pub mod insert_element;
pub mod layout_lifecycle;
pub mod move_element;
pub mod patch_buffer;
pub mod remove_element;
pub mod rename_element;
pub mod reparent_element;
pub mod replace_slide;
pub mod resize_element;
pub mod scale_elements;
pub mod set_element_id;
pub mod set_embed;
pub mod set_geometry;
pub mod set_inline_style;
pub mod set_morph_transition;
pub mod set_text;
pub mod slide_lifecycle;
pub mod slide_metadata;
pub mod slide_style;
pub mod swap_theme;
pub mod table_commands;
pub mod theme_globals;
pub mod transactions;

use crate::deck::element::ElementContent;
use crate::deck::{Canvas, CanvasTarget, Deck, ElementId, LayoutId, SlideId};
use crate::ipc::{Patch, Point, SelectionState};
use tracing::{debug, warn};

pub use align_commands::{AlignAxis, AlignElements, AlignOp, DistributeAxis};
pub use animation::{InsertAnimation, RemoveAnimation, ReorderAnimation, SetAnimationProperty};
pub use composite::CompositeCommand;
pub use group_commands::{SetGroupLayout, SetGroupScale};
pub use group_relayout::relayout_patches;
pub use group_select::{DissolveGroup, GroupElements};
pub use history::{CommandHistory, DEFAULT_HISTORY_DEPTH, HistoryEntry, UndoOutput};
pub use rename_element::RenameElement;
pub use reparent_element::ReparentElement;
pub use replace_slide::ReplaceSlideContent;
pub use resize_element::ResizeElement;
pub use scale_elements::{ElementTransform, SetElementsTransform};
pub use set_geometry::{GeometryProperty, SetGeometryProperty};
pub use set_inline_style::{RemoveInlineStyle, SetInlineStyle};

pub use insert_element::InsertElement;
pub use layout_lifecycle::{
    InsertLayout, RemoveLayout, SetLayoutBackground, SetLayoutBackgroundImage, SetLayoutName,
};
pub use move_element::MoveElement;
pub use patch_buffer::PatchBuffer;
pub use remove_element::RemoveElementCommand;
pub use set_element_id::SetElementId;
pub use set_embed::SetEmbedHtml;
pub use set_morph_transition::SetMorphTransition;
pub use set_text::SetTextContent;
pub use slide_lifecycle::{InsertSlide, RemoveSlide, ReorderSlide};
pub use slide_metadata::{SetDeckTitle, SetSlideTitle};
pub use slide_style::{
    SetSlideBackground, SetSlideBackgroundImage, SetSlideLayout, SetSlideNotes, SetSlideTransition,
};
pub use swap_theme::SwapTheme;
pub use table_commands::{
    DeleteTableColumn, DeleteTableRow, InsertTableColumn, InsertTableRow, SetCellStyles,
    SetCellText, SetTableData, SetTableHeaderColumns, SetTableHeaderRows,
};
pub use theme_globals::SetGlobalsCss;
pub use transactions::{Transaction, TransactionSnapshot};

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

    fn affects_slide_list(&self) -> bool {
        false
    }

    fn affects_layout_list(&self) -> bool {
        false
    }

    fn affects_globals(&self) -> bool {
        false
    }

    fn affects_animations(&self) -> bool {
        false
    }

    fn affects_assets(&self) -> bool {
        false
    }

    fn affects_slide_meta(&self) -> bool {
        false
    }

    fn affects_guides(&self) -> bool {
        false
    }
}

#[derive(Debug)]
pub struct CommandOutput {
    pub patches: Vec<Patch>,
    pub inverse: Box<dyn Command>,
    pub dirty_targets: Vec<CanvasTarget>,
    pub manifest_dirty: bool,

    pub warnings: Vec<String>,
}

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

pub fn resolve_canvas_mut<'a>(
    deck: &'a mut Deck,
    target: &CanvasTarget,
) -> Result<&'a mut dyn Canvas, CommandError> {
    deck.canvas_mut(target)
        .ok_or_else(|| canvas_not_found(target))
}

pub fn canvas_not_found(target: &CanvasTarget) -> CommandError {
    match target {
        CanvasTarget::Slide(s) => CommandError::SlideNotFound(s.clone()),
        CanvasTarget::Layout(l) => CommandError::LayoutNotFound(l.clone()),
    }
}

#[derive(Debug, Default, Clone)]
pub struct DispatchOutcome {
    pub needs_flush: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    New,
    Open,
    Save,
    SaveAs,

    SaveTheme,
    LoadTheme,

    ExportHtml,

    ExportPdf,
}

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

    SetActiveSlide(SlideId),

    SetEditorMode(EditorMode),

    SetActiveLayout(LayoutId),

    StartPresentation {
        windowed: bool,
    },

    SendSlideLayoutPicker,
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
            Self::StartPresentation { windowed } => write!(f, "StartPresentation({windowed})"),
            Self::SendSlideLayoutPicker => f.write_str("SendSlideLayoutPicker"),
            Self::Nothing => f.write_str("Nothing"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditorMode {
    #[default]
    Slide,
    Layout,
}

pub struct CommandDispatcher {
    deck: Deck,
    patch_buffer: PatchBuffer,
    transaction: Option<Transaction>,
    mode: EditorMode,
    slide_history: CommandHistory,
    layout_history: CommandHistory,
}

impl CommandDispatcher {
    pub fn new(deck: Deck) -> Self {
        Self::with_history(deck, CommandHistory::default())
    }

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

    pub fn mode(&self) -> EditorMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: EditorMode) {
        self.mode = mode;
    }

    fn active_history_mut(&mut self) -> &mut CommandHistory {
        match self.mode {
            EditorMode::Slide => &mut self.slide_history,
            EditorMode::Layout => &mut self.layout_history,
        }
    }

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

    pub fn begin_transaction(&mut self, label: &'static str, snapshot: TransactionSnapshot) {
        assert!(!label.is_empty(), "begin_transaction: label is empty");
        if self.transaction.is_some() {
            warn!("begin_transaction called while one is already open; replacing");
        }
        debug!(label, "transaction begin");
        self.transaction = Some(Transaction::new(label, snapshot));
    }

    pub fn commit_transaction(&mut self) -> Option<Transaction> {
        let txn: Transaction = self.transaction.take()?;
        debug!(
            label = txn.label,
            patches = txn.patches.len(),
            "transaction commit"
        );
        if let Some(inverse) = build_composite_inverse(&txn) {
            self.active_history_mut().push(inverse, txn.label);
        }
        Some(txn)
    }

    pub fn abort_transaction(&mut self) {
        if self.transaction.take().is_some() {
            debug!("transaction abort");
        }
    }

    pub fn dispatch(&mut self, command: Box<dyn Command>) -> Result<DispatchOutcome, CommandError> {
        let label: &'static str = command.label();
        let undoable: bool = command.undoable();
        let affects_object_tree: bool = command.affects_object_tree();
        let requires_remount: bool = command.requires_remount();
        let affects_slide_list: bool = command.affects_slide_list();
        let affects_layout_list: bool = command.affects_layout_list();
        let affects_globals: bool = command.affects_globals();
        let affects_animations: bool = command.affects_animations();
        let affects_assets: bool = command.affects_assets();
        let affects_slide_meta: bool = command.affects_slide_meta();
        let affects_guides: bool = command.affects_guides();
        debug!("dispatching: {}", label);
        let output: CommandOutput = command.apply(&mut self.deck)?;

        assert!(
            !output.dirty_targets.is_empty()
                || output.manifest_dirty
                || !output.patches.is_empty()
                || requires_remount,
            "command produced no side effects at all (label = {label})"
        );
        if let Some(txn) = self.transaction.as_mut() {
            txn.patches.extend(output.patches.iter().cloned());
            txn.dirty_targets
                .extend(output.dirty_targets.iter().cloned());
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
            affects_assets,
            affects_slide_meta,
            affects_guides,
            warnings,
        })
    }

    pub fn undo(&mut self) -> Result<Option<DispatchOutcome>, CommandError> {
        assert!(self.transaction.is_none(), "undo while transaction is open");

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
            affects_assets: out.affects_assets,
            affects_slide_meta: out.affects_slide_meta,
            affects_guides: out.affects_guides,
            warnings: out.warnings,
        }))
    }

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
            affects_assets: out.affects_assets,
            affects_slide_meta: out.affects_slide_meta,
            affects_guides: out.affects_guides,
            warnings: out.warnings,
        }))
    }

    pub fn take_patches(&mut self) -> Vec<Patch> {
        self.patch_buffer.take_coalesced()
    }
}

fn build_composite_inverse(txn: &Transaction) -> Option<Box<dyn Command>> {
    let mut subs: Vec<Box<dyn Command>> = Vec::new();
    for ((target, eid), geom) in &txn.start_snapshot.geometry {
        assert!(
            !target.id().is_empty() && !eid.is_empty(),
            "snapshot has empty key"
        );
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

        assert_eq!(d.patch_buffer_len(), 2);
        assert_eq!(d.transaction().unwrap().patches.len(), 2);
        assert!(
            d.transaction()
                .unwrap()
                .dirty_targets
                .contains(&CanvasTarget::Slide(sid.clone()))
        );
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
        let start_geo = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();

        let mut snap = TransactionSnapshot::empty();
        snap.record_geometry(
            CanvasTarget::Slide(sid.clone()),
            eid.clone(),
            start_geo.clone(),
        );
        d.begin_transaction("Move Element", snap);

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
        let original = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();

        d.dispatch(move_to(&sid, &eid, 999.0, -7.0)).unwrap();
        let _ = d.take_patches();

        let outcome = d.undo().unwrap().expect("undo should not be no-op");
        assert!(outcome.needs_flush);
        let geo = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
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

        let geo = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
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
        let start_geo = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();

        let mut snap = TransactionSnapshot::empty();
        snap.record_geometry(
            CanvasTarget::Slide(sid.clone()),
            eid.clone(),
            start_geo.clone(),
        );
        d.begin_transaction("Move Element", snap);
        d.dispatch(move_to(&sid, &eid, start_geo.x + 200.0, start_geo.y + 80.0))
            .unwrap();
        d.commit_transaction().unwrap();
        let _ = d.take_patches();

        d.undo().unwrap();
        let after = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
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
        let original = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();

        d.dispatch(move_to(&sid, &eid, 333.0, 444.0)).unwrap();
        let _ = d.take_patches();

        let mut iter: usize = 0;
        while iter < 6 {
            d.undo().unwrap();
            let g = d.deck().slides[&sid]
                .find_element(&eid)
                .unwrap()
                .geometry
                .clone();
            assert_eq!(g.x, original.x);
            assert_eq!(g.y, original.y);

            d.redo().unwrap();
            let g = d.deck().slides[&sid]
                .find_element(&eid)
                .unwrap()
                .geometry
                .clone();
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

        assert!(d.can_undo());
        d.set_mode(EditorMode::Layout);
        assert!(!d.can_undo());
        d.set_mode(EditorMode::Slide);
        assert!(d.can_undo());
    }

    #[test]
    fn undo_in_each_mode_only_touches_that_modes_tree() {
        let mut d = CommandDispatcher::new(Deck::sample());

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

        d.dispatch(move_to(&sid, &eid, 111.0, 222.0)).unwrap();

        d.set_mode(EditorMode::Layout);
        d.dispatch(Box::new(MoveElement {
            target: CanvasTarget::Layout("blank".into()),
            element_id: "el_lt".into(),
            new_position: Point { x: 7.0, y: 8.0 },
            previous_position: None,
        }))
        .unwrap();

        d.undo().unwrap().expect("layout undo applies");
        assert_eq!(
            d.deck().theme.layouts["blank"]
                .find_element("el_lt")
                .unwrap()
                .geometry
                .x,
            0.0
        );
        assert_eq!(
            d.deck().slides[&sid].find_element(&eid).unwrap().geometry.x,
            111.0
        );

        d.set_mode(EditorMode::Slide);
        d.undo().unwrap().expect("slide undo applies");
        assert_eq!(
            d.deck().slides[&sid].find_element(&eid).unwrap().geometry.x,
            slide_start
        );
        assert_eq!(
            d.deck().theme.layouts["blank"]
                .find_element("el_lt")
                .unwrap()
                .geometry
                .x,
            0.0
        );
    }
}
