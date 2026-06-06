// Application core.

#![allow(dead_code, unused_imports)]

//
// Stage 5: owns the CommandDispatcher (and through it the Deck), the
// editor's SelectionState, the egress WebviewSender, and a closure that
// schedules a patch-buffer flush on the event loop.
//
// `interpret` (SPEC §9.4, ROADMAP Stage 5) is the policy layer that maps
// an InteractionEvent to an InterpretResult. `handle_interaction` is the
// effects layer that turns the result into dispatcher calls and outbound
// IPC messages. Splitting them keeps `interpret` purely functional so it
// can be unit-tested without touching the webview.

use crate::bundle::assets::{AssetDimensions, AssetEntry};
use crate::bundle::{
    IoRequest, IoResponse, IoThread, deserialize_deck, serialize_deck,
};
use crate::commands::{
    Command, CommandDispatcher, CompositeCommand, EditorMode, FileAction, GeometryProperty,
    InsertAnimation, InsertElement, InsertLayout, InsertSlide, InterpretResult, MoveElement,
    RemoveAnimation, RemoveElementCommand, RemoveInlineStyle, RenameElement, ReparentElement,
    ResizeElement, SetElementId, SetGeometryProperty, SetGlobalsCss, SetInlineStyle, SetLayoutName,
    SetSlideTitle, SetTextContent, TransactionSnapshot,
};
use crate::deck::animation::{AnimationCategory, AnimationEntry, AnimationTiming, AnimationTrigger};
use crate::deck::element::{
    AssetRef, ElementContent, ElementNode, ElementStyle, ElementType, RichText,
};
use crate::deck::ids::{new_animation_id, new_element_id};
use crate::deck::layout::LayoutNode;
use crate::deck::slide::SlideNode;
use crate::deck::style::{
    ColorRef, FontRef, Geometry, ImageStyle, Length, ShapeStyle, TextStyle,
};
use crate::deck::{Canvas, CanvasTarget, Deck, ElementId, LayoutId, ShapeGeometry, SlideId};
use crate::error::{AppError, AppResult};
use crate::html::serialize::{serialize_slide, ANIMATION_KEYFRAMES_CSS};
use crate::ipc::bridge::WebviewSender;
use crate::ipc::present::{PresentInbound, PresentInitPayload};
use crate::present::session::{PresentationSession, PresentStep};
use crate::ipc::{
    AssetPayload, AssetsBundle, EditorConfig, InteractionEvent, IpcMessage, LayoutListData,
    LayoutListEntry, MessageKind, MountSlideArgs, ObjectTreeData, ObjectTreeNode, Patch, Point,
    SelectionState, Size, SlideAnimationEntry, SlideAnimationsData, SlideListData, SlideListEntry,
};
use base64::Engine;
use std::collections::BTreeMap;
use std::path::PathBuf;
use tracing::{debug, info, warn};

const DEBUG_KEY: &str = "d";
const DEBUG_NUDGE_PX: f64 = 50.0;
const DRAG_TRANSACTION_LABEL: &str = "Move Element";
const RESIZE_TRANSACTION_LABEL: &str = "Resize Element";
// Synthetic key names the JS host posts for accelerator shortcuts. Kept as
// constants so both interpret() and any future platform-specific shortcut
// layer reference the same strings.
const UNDO_KEY: &str = "undo";
const REDO_KEY: &str = "redo";
const NEW_KEY: &str = "new_deck";
const OPEN_KEY: &str = "open_deck";
const SAVE_KEY: &str = "save_deck";
const SAVE_AS_KEY: &str = "save_as_deck";
// Synthetic accelerator the JS host posts for ⌘↩ / the toolbar Play button.
const PRESENT_KEY: &str = "present";
const BUNDLE_FILE_EXTENSION: &str = "slidedeck";
// Keys forwarded by the JS host that should trigger element deletion.
// Both names cover the two physical keys users reach for: macOS users
// typically press Delete (which the platform reports as "Backspace"),
// while Windows / external keyboards distinguish a forward-delete.
const DELETE_KEY_BACKSPACE: &str = "Backspace";
const DELETE_KEY_DELETE: &str = "Delete";

// HistoryStep
// Direction tag for run_history_step. A two-variant enum (rather than a
// bool) so the logger and any future telemetry can distinguish undo from
// redo by name.
#[derive(Debug, Clone, Copy)]
enum HistoryStep {
    Undo,
    Redo,
}

// ApplicationCore
// Owns dispatcher, selection, the egress channel, the patch-flush wake,
// and (Stage 7) a handle to the IoThread for background bundle I/O.
pub struct ApplicationCore {
    dispatcher: CommandDispatcher,
    active_slide: Option<SlideId>,
    // The layout currently being edited in layout mode. Joins `active_slide`
    // so the editor remembers each mode's selection independently. The
    // dispatcher's mode decides which one `active_canvas()` returns.
    active_layout: Option<LayoutId>,
    selection: SelectionState,
    sender: WebviewSender,
    schedule_flush: Box<dyn Fn()>,
    io_thread: IoThread,
    // Presentation mode (None unless presenting). Holds the cursor + the
    // presentation WebviewSender. The two wakes ask the event loop to build /
    // tear down the fullscreen window (window creation needs the event-loop
    // target, only available inside the run closure).
    present: Option<PresentationSession>,
    request_present_open: Box<dyn Fn()>,
    request_present_close: Box<dyn Fn()>,
    // Set by start_presentation to the slide index the presentation should open
    // on; consumed by begin_presentation once main.rs has built the webview.
    pending_present_index: Option<usize>,
    // Set by interpret_asset_imported just before it returns the
    // InsertElement command. handle_interaction consumes it after the
    // command dispatches successfully — at that point the asset is
    // both in the deck and referenced by a slide element, so JS needs
    // a copy of the bytes to render the image.
    pending_asset_broadcast: Option<String>,
    // Set by the AddSlideRequested interpret arm to the id of the
    // freshly-built slide. react_to_outcome consumes it on the
    // affects_slide_list path to switch the active slide to the new
    // one once the InsertSlide command has applied.
    pending_new_active_slide: Option<SlideId>,
    // Layout-mode analogue of pending_new_active_slide: set by the
    // AddLayoutRequested arm so react_to_outcome switches to the freshly
    // created layout once InsertLayout has applied.
    pending_new_active_layout: Option<LayoutId>,
}

impl ApplicationCore {
    // new
    // Inputs: a WebviewSender, a no-arg closure that posts
    // UserEvent::FlushPatches on the event loop, and an IoThread handle
    // used for all background bundle reads/writes.
    // Output: an ApplicationCore preloaded with `Deck::sample()` and the
    // first slide selected as active.
    pub fn new(
        sender: WebviewSender,
        schedule_flush: Box<dyn Fn()>,
        io_thread: IoThread,
        request_present_open: Box<dyn Fn()>,
        request_present_close: Box<dyn Fn()>,
    ) -> Self {
        let deck: Deck = Deck::sample();
        let active_slide: Option<SlideId> = deck.slide_order.first().cloned();
        let active_layout: Option<LayoutId> = deck.theme.layout_order.first().cloned();
        assert!(active_slide.is_some(), "sample deck must contain a slide");
        Self {
            dispatcher: CommandDispatcher::new(deck),
            active_slide,
            active_layout,
            selection: SelectionState::empty(),
            sender,
            schedule_flush,
            io_thread,
            present: None,
            request_present_open,
            request_present_close,
            pending_present_index: None,
            pending_asset_broadcast: None,
            pending_new_active_slide: None,
            pending_new_active_layout: None,
        }
    }

    // active_canvas
    // Inputs: none.
    // Output: the CanvasTarget for the current editor mode — the active
    // slide in Slide mode, the active layout in Layout mode — or None when
    // that mode has no active canvas. This is the single source of truth for
    // "which surface do mounts, the object tree, and element commands act
    // on"; `active_target` is its alias used by command builders.
    fn active_canvas(&self) -> Option<CanvasTarget> {
        match self.dispatcher.mode() {
            EditorMode::Slide => self.active_slide.clone().map(CanvasTarget::Slide),
            EditorMode::Layout => self.active_layout.clone().map(CanvasTarget::Layout),
        }
    }

    // active_canvas_id
    // The active canvas's id as a String (used as SelectionState.slide_id so
    // the JS overlay scopes to whichever surface — slide or layout — is
    // mounted in the viewport).
    fn active_canvas_id(&self) -> Option<String> {
        self.active_canvas().map(|t| t.id().to_string())
    }

    pub fn selection(&self) -> &SelectionState {
        &self.selection
    }

    pub fn active_slide(&self) -> Option<&SlideId> {
        self.active_slide.as_ref()
    }

    // handle_ipc
    // Inputs: a fully-parsed IpcMessage from the webview.
    // Output: Ok(()) on success.
    // Errors: forwarded from any outbound send or command dispatch.
    pub fn handle_ipc(&mut self, msg: IpcMessage) -> AppResult<()> {
        assert!(!msg.id.is_empty(), "ipc message missing id");
        debug!(id = %msg.id, "ipc <- webview");
        match msg.kind {
            MessageKind::Ready => {
                // First contact from JS: deliver one-shot config (the built-in
                // animation keyframes), announce the full slide list so the
                // thumbnail row can render every slide, dump every asset's
                // bytes so embedded images resolve to blob URLs, then mount
                // the active slide and its animation timeline.
                self.sender.send(MessageKind::Configure(EditorConfig {
                    debug: false,
                    animation_keyframes_css: ANIMATION_KEYFRAMES_CSS.to_string(),
                }))?;
                self.send_slide_list()?;
                self.send_assets_bundle()?;
                self.send_active_slide()?;
                self.send_slide_animations()
            }
            MessageKind::Interaction(event) => self.handle_interaction(event),
            other => {
                warn!("unhandled message kind: {:?}", std::mem::discriminant(&other));
                Ok(())
            }
        }
    }

    // flush_patches
    // Inputs: none.
    // Output: Ok(()) after the patch buffer has been drained (or was
    // already empty).
    // Dataflow: take coalesced patches from the dispatcher; wrap one in
    // ApplyPatch directly, multiple in Patch::Batch; ship via the sender.
    pub fn flush_patches(&mut self) -> AppResult<()> {
        let patches: Vec<Patch> = self.dispatcher.take_patches();
        if patches.is_empty() {
            return Ok(());
        }
        let payload: Patch = if patches.len() == 1 {
            patches.into_iter().next().unwrap_or(Patch::Batch { patches: vec![] })
        } else {
            Patch::Batch { patches }
        };
        debug!("flushing patches");
        self.sender.send(MessageKind::ApplyPatch(payload))
    }

    // send_active_slide
    // Inputs: none.
    // Output: Ok(()) after sending one MountSlide envelope AND one
    // ObjectTreeUpdate envelope (the panel always rebuilds alongside a
    // remount, so it cannot drift out of sync with the shadow DOM).
    // Dataflow: lookup active slide -> serialize via html::serialize_slide
    // -> bundle slide_html + theme_css into MountSlideArgs -> dispatch
    // -> build the matching ObjectTreeData -> dispatch.
    fn send_active_slide(&mut self) -> AppResult<()> {
        let target: CanvasTarget = match self.active_canvas() {
            Some(t) => t,
            None => {
                warn!("no active canvas; nothing to mount");
                return Ok(());
            }
        };
        let (id, slide_html, tree): (String, String, ObjectTreeData) =
            match self.canvas_mount_artifacts(&target) {
                Some(parts) => parts,
                None => {
                    warn!(target = ?target, "active canvas absent; nothing to mount");
                    return Ok(());
                }
            };
        assert!(!slide_html.is_empty(), "serializer produced empty canvas");
        info!(canvas = %id, "mounting active canvas via IPC");
        let args = MountSlideArgs {
            slide_id: id,
            slide_html,
            theme_css: self.dispatcher.deck().theme.theme_css.clone(),
            globals_css: self.dispatcher.deck().theme.globals_css.clone(),
        };
        self.sender.send(MessageKind::MountSlide(args))?;
        self.sender.send(MessageKind::ObjectTreeUpdate(tree))
    }

    // canvas_mount_artifacts
    // Inputs: a CanvasTarget.
    // Output: (id, serialized HTML, object tree) for the target canvas, or
    // None if it no longer exists. Layouts serialize through a transient
    // SlideNode wrapper so they reuse the exact, tested slide serializer and
    // object-tree builder (a layout root is a Group, like a slide root).
    fn canvas_mount_artifacts(
        &self,
        target: &CanvasTarget,
    ) -> Option<(String, String, ObjectTreeData)> {
        match target {
            CanvasTarget::Slide(id) => {
                let slide = self.dispatcher.deck().slides.get(id)?;
                Some((id.clone(), serialize_slide(slide), build_object_tree(slide)))
            }
            CanvasTarget::Layout(id) => {
                let layout = self.dispatcher.deck().theme.layouts.get(id)?;
                let transient: SlideNode =
                    SlideNode::new(layout.id.clone(), layout.id.clone(), layout.root.clone());
                Some((
                    id.clone(),
                    serialize_slide(&transient),
                    build_object_tree(&transient),
                ))
            }
        }
    }

    // send_object_tree
    // Inputs: none.
    // Output: Ok(()) after sending one ObjectTreeUpdate. Used when a
    // command changes the object panel's payload but not the shadow DOM
    // — RenameElement is the only Stage 9 example (data-name patch
    // updates the DOM; the panel needs the new label string).
    // Dataflow: lookup active slide -> build ObjectTreeData -> dispatch.
    fn send_object_tree(&self) -> AppResult<()> {
        let target: CanvasTarget = match self.active_canvas() {
            Some(t) => t,
            None => return Ok(()),
        };
        let tree: ObjectTreeData = match self.canvas_mount_artifacts(&target) {
            Some((_, _, tree)) => tree,
            None => return Ok(()),
        };
        self.sender.send(MessageKind::ObjectTreeUpdate(tree))
    }

    // send_slide_list
    // Inputs: none.
    // Output: Ok(()) after sending one SlideListUpdate envelope carrying
    // every slide's id + title + serialized HTML. Stage 10 calls this on
    // app start and after each file Open / New; future slide-level
    // commands (add / remove / reorder) will call it too. The active
    // slide's individual MountSlide events keep its thumbnail HTML fresh
    // after structural edits, so this message is intentionally rare.
    // Dataflow: iterate the deck's slide_order -> serialize each slide
    // -> pair with its manifest title -> ship.
    // send_assets_bundle
    // Inputs: none.
    // Output: Ok(()) after sending one AssetsUpdate envelope containing
    // every registered asset's bytes (base64-encoded). Called on app
    // start and after file Open / New so JS can rebuild its blob URL
    // cache from scratch. Skipped silently when no assets exist (the
    // empty payload is harmless but the noise isn't worth it).
    fn send_assets_bundle(&self) -> AppResult<()> {
        let bundle: AssetsBundle = match build_assets_bundle(self.dispatcher.deck()) {
            Some(b) => b,
            None => return Ok(()),
        };
        debug!(count = bundle.assets.len(), "ipc -> AssetsUpdate");
        self.sender.send(MessageKind::AssetsUpdate(bundle))
    }

    // send_asset_added
    // Inputs: an asset id known to be present in deck.assets.
    // Output: Ok(()) after sending one AssetAdded envelope. Used as an
    // incremental delivery vehicle after AssetImported so JS picks up
    // just the new asset rather than re-receiving every existing one.
    fn send_asset_added(&self, asset_id: &str) -> AppResult<()> {
        let registry = &self.dispatcher.deck().assets;
        let entry: &AssetEntry = match registry.find_by_id(asset_id) {
            Some(e) => e,
            None => return Ok(()),
        };
        let bytes: &Vec<u8> = match registry.files.get(&entry.path) {
            Some(b) => b,
            None => return Ok(()),
        };
        debug!(asset_id, "ipc -> AssetAdded");
        self.sender.send(MessageKind::AssetAdded(AssetPayload {
            asset_id: entry.id.clone(),
            media_type: entry.media_type.clone(),
            content_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        }))
    }

    fn send_slide_list(&self) -> AppResult<()> {
        let data: SlideListData = build_slide_list_data(
            self.dispatcher.deck(),
            self.active_slide.as_ref(),
        );
        debug!(
            slide_count = data.slides.len(),
            active = ?data.active_slide_id,
            "ipc -> SlideListUpdate"
        );
        self.sender.send(MessageKind::SlideListUpdate(data))
    }

    // send_slide_animations
    // Inputs: none.
    // Output: Ok(()) after sending one SlideAnimationsUpdate carrying the
    // active slide's timeline (id / element / category per entry). The
    // inspector's Appear/Disappear toggles reflect this. No-op when there is
    // no active slide (animations are slide-only).
    fn send_slide_animations(&self) -> AppResult<()> {
        let sid: SlideId = match &self.active_slide {
            Some(s) => s.clone(),
            None => return Ok(()),
        };
        let slide = match self.dispatcher.deck().slides.get(&sid) {
            Some(s) => s,
            None => return Ok(()),
        };
        let entries: Vec<SlideAnimationEntry> = slide
            .animations
            .iter()
            .map(|e| SlideAnimationEntry {
                animation_id: e.id.clone(),
                element_id: e.element_id.clone(),
                category: match e.category {
                    AnimationCategory::Entrance => "entrance",
                    AnimationCategory::Emphasis => "emphasis",
                    AnimationCategory::Exit => "exit",
                }
                .to_string(),
            })
            .collect();
        self.sender.send(MessageKind::SlideAnimationsUpdate(SlideAnimationsData {
            slide_id: sid,
            entries,
        }))
    }

    // set_active_slide
    // Inputs: the target slide id.
    // Output: Ok(()) on success; Ok(()) (no-op) when the id is empty,
    // unknown, or already active.
    // Dataflow:
    //   1. Reject empty / unknown / same-as-active inputs early.
    //   2. Flush any pending patches so they apply to the OLD slide's
    //      shadow DOM before it is replaced.
    //   3. Swap `active_slide` and clear selection (selection is
    //      per-slide editor state, not deck state).
    //   4. Send a fresh MountSlide + ObjectTreeUpdate for the new
    //      active slide. Unsaved edits to the previous slide live on
    //      in `dispatcher.deck.slides[<old_id>]` untouched.
    fn set_active_slide(&mut self, slide_id: SlideId) -> AppResult<()> {
        if slide_id.is_empty() {
            return Ok(());
        }
        if !self.dispatcher.deck().slides.contains_key(&slide_id) {
            warn!(target = %slide_id, "set_active_slide: unknown slide id");
            return Ok(());
        }
        if self.active_slide.as_deref() == Some(slide_id.as_str()) {
            return Ok(());
        }
        info!(target = %slide_id, "switching active slide");
        // Step 2: flush so the OLD slide's shadow DOM receives any
        // queued patches before we tear it down.
        self.flush_patches()?;
        // Step 3: swap state.
        self.active_slide = Some(slide_id);
        self.selection = SelectionState::empty();
        // Step 4: announce the swap. send_active_slide also re-sends
        // the object tree, so the panel resyncs in one shot.
        self.sender
            .send(MessageKind::SetSelection(SelectionState::empty()))?;
        self.send_active_slide()
    }

    // set_editor_mode
    // Inputs: the mode to switch to.
    // Output: Ok(()); no-op when already in that mode.
    // Dataflow: flush pending patches to the OLD canvas -> switch the
    // dispatcher's mode -> ensure the new mode has an active canvas (lazily
    // adopt the first layout when entering Layout mode with none) -> clear
    // selection -> echo SetMode to JS -> broadcast the relevant list and
    // remount the active canvas.
    fn set_editor_mode(&mut self, mode: EditorMode) -> AppResult<()> {
        if self.dispatcher.mode() == mode {
            return Ok(());
        }
        info!(?mode, "switching editor mode");
        self.flush_patches()?;
        self.dispatcher.set_mode(mode);
        if mode == EditorMode::Layout && self.active_layout.is_none() {
            self.active_layout = self.dispatcher.deck().theme.layout_order.first().cloned();
        }
        self.selection = SelectionState::empty();
        let mode_str: &str = match mode {
            EditorMode::Slide => "slide",
            EditorMode::Layout => "layout",
        };
        self.sender.send(MessageKind::SetMode { mode: mode_str.to_string() })?;
        self.sender
            .send(MessageKind::SetSelection(SelectionState::empty()))?;
        match mode {
            EditorMode::Slide => self.send_slide_list()?,
            EditorMode::Layout => self.send_layout_list()?,
        }
        self.send_active_slide()
    }

    // set_active_layout
    // Inputs: the target layout id.
    // Output: Ok(()); no-op when empty, unknown, or already active.
    // The layout-mode analogue of set_active_slide: flush, swap the active
    // layout, clear selection, then remount the active canvas.
    fn set_active_layout(&mut self, layout_id: LayoutId) -> AppResult<()> {
        if layout_id.is_empty() {
            return Ok(());
        }
        if !self.dispatcher.deck().theme.layouts.contains_key(&layout_id) {
            warn!(target = %layout_id, "set_active_layout: unknown layout id");
            return Ok(());
        }
        if self.active_layout.as_deref() == Some(layout_id.as_str()) {
            return Ok(());
        }
        info!(target = %layout_id, "switching active layout");
        self.flush_patches()?;
        self.active_layout = Some(layout_id);
        self.selection = SelectionState::empty();
        self.sender
            .send(MessageKind::SetSelection(SelectionState::empty()))?;
        self.send_active_slide()
    }

    // send_layout_list
    // Inputs: none.
    // Output: Ok(()) after sending one LayoutListUpdate carrying every
    // layout's id + name + serialized HTML, the active layout id, and the
    // shared theme/globals CSS. Sent on entering layout mode and after any
    // command reporting affects_layout_list / affects_globals.
    fn send_layout_list(&self) -> AppResult<()> {
        let data: LayoutListData =
            build_layout_list_data(self.dispatcher.deck(), self.active_layout.as_ref());
        debug!(
            layout_count = data.layouts.len(),
            active = ?data.active_layout_id,
            "ipc -> LayoutListUpdate"
        );
        self.sender.send(MessageKind::LayoutListUpdate(data))
    }

    // interpret
    // Inputs: an InteractionEvent.
    // Output: an InterpretResult describing what should happen next.
    // Dataflow: pure — reads selection, active slide, and dispatcher
    // state but does not mutate. Side effects belong to the caller.
    pub fn interpret(&mut self, event: InteractionEvent) -> InterpretResult {
        match event {
            InteractionEvent::ElementClicked { element_id, modifiers, .. } => {
                let mut sel: SelectionState = if modifiers.shift {
                    self.selection.clone()
                } else {
                    SelectionState::empty()
                };
                sel.slide_id = self.active_canvas_id();
                if modifiers.shift {
                    sel.toggle(element_id);
                } else if !sel.contains(&element_id) {
                    sel.element_ids.push(element_id);
                }
                InterpretResult::Selection(sel)
            }
            InteractionEvent::ElementDragStarted { element_id, .. } => {
                let snapshot: TransactionSnapshot = self.snapshot_for_drag(&element_id);
                InterpretResult::TransactionBegin {
                    label: DRAG_TRANSACTION_LABEL,
                    snapshot,
                }
            }
            // ElementDragged is intentionally a no-op on the Rust side.
            // The optimistic transform on the JS host carries the visual
            // state of the drag; mutating the deck on every event would
            // emit SetStyle patches that double-translate the element
            // (the JS host already moved it via transform). The deck is
            // updated once at ElementDragEnded with the full delta.
            InteractionEvent::ElementDragged { .. } => InterpretResult::Nothing,
            InteractionEvent::ElementDragEnded { element_id, delta } => {
                let target: CanvasTarget = match self.active_canvas() {
                    Some(t) => t,
                    None => return InterpretResult::Nothing,
                };
                let start_xy: (f64, f64) = match self
                    .dispatcher
                    .transaction()
                    .and_then(|t| t.start_snapshot.position_of(&target, &element_id))
                {
                    Some(p) => p,
                    None => return InterpretResult::Nothing,
                };
                let cmd = MoveElement {
                    target,
                    element_id,
                    new_position: Point {
                        x: start_xy.0 + delta.x,
                        y: start_xy.1 + delta.y,
                    },
                    previous_position: None,
                };
                InterpretResult::CommitTransactionWith(Box::new(cmd))
            }
            InteractionEvent::ElementResizeStarted { element_id, .. } => {
                let snapshot: TransactionSnapshot = self.snapshot_for_drag(&element_id);
                InterpretResult::TransactionBegin {
                    label: RESIZE_TRANSACTION_LABEL,
                    snapshot,
                }
            }
            // ElementResized is the throttled mid-resize update. Same
            // rationale as ElementDragged: the JS host has already
            // applied an optimistic style write to the element, so
            // mutating the deck here would double-apply on the next
            // patch flush.
            InteractionEvent::ElementResized { .. } => InterpretResult::Nothing,
            InteractionEvent::ElementResizeEnded {
                element_id,
                new_position,
                new_size,
            } => {
                let target: CanvasTarget = match self.active_canvas() {
                    Some(t) => t,
                    None => return InterpretResult::Nothing,
                };
                // Verify we actually have a snapshot for this element — a
                // ResizeEnded without a matching Started is a host bug
                // and we drop it rather than risk a no-op commit.
                if self
                    .dispatcher
                    .transaction()
                    .and_then(|t| t.start_snapshot.position_of(&target, &element_id))
                    .is_none()
                {
                    return InterpretResult::Nothing;
                }
                InterpretResult::CommitTransactionWith(Box::new(ResizeElement {
                    target,
                    element_id,
                    new_x: new_position.x,
                    new_y: new_position.y,
                    new_width: new_size.width,
                    new_height: new_size.height,
                }))
            }
            // Inline text editing (SPEC §8.5). The webview owns the text
            // during the session, so Started / Edited are no-ops on the
            // Rust side; only the commit produces a mutation.
            InteractionEvent::TextEditStarted { .. } => InterpretResult::Nothing,
            InteractionEvent::TextEdited { .. } => InterpretResult::Nothing,
            InteractionEvent::TextEditEnded { element_id, text } => {
                match build_set_text_command(
                    &self.dispatcher,
                    self.active_canvas(),
                    element_id,
                    text,
                ) {
                    Some(cmd) => InterpretResult::Command(cmd),
                    None => InterpretResult::Nothing,
                }
            }
            InteractionEvent::BackgroundClicked { .. } => {
                InterpretResult::Selection(SelectionState::empty())
            }
            InteractionEvent::PropertyChanged { element_id, property, value } => {
                interpret_property_changed(
                    self.active_canvas(),
                    element_id,
                    property,
                    value,
                )
            }
            InteractionEvent::SetSelectionFromPanel { element_ids } => {
                let mut sel: SelectionState = SelectionState::empty();
                sel.slide_id = self.active_canvas_id();
                sel.element_ids = element_ids;
                InterpretResult::Selection(sel)
            }
            InteractionEvent::InsertElementRequested {
                element_type,
                parent_id,
                position,
            } => interpret_insert_element_request(
                &self.dispatcher,
                self.active_canvas(),
                element_type,
                parent_id,
                position,
            ),
            InteractionEvent::RenameElementRequested { element_id, new_name } => {
                interpret_rename_request(self.active_canvas(), element_id, new_name)
            }
            InteractionEvent::ReparentElementRequested {
                element_id,
                new_parent_id,
                new_position,
            } => interpret_reparent_request(
                self.active_canvas(),
                element_id,
                new_parent_id,
                new_position,
            ),
            InteractionEvent::AssetImported {
                content_base64,
                original_filename,
                media_type,
                width,
                height,
                position,
            } => self.interpret_asset_imported(
                content_base64,
                original_filename,
                media_type,
                width,
                height,
                position,
            ),
            InteractionEvent::SlideThumbnailClicked { slide_id } => {
                if slide_id.is_empty() {
                    InterpretResult::Nothing
                } else {
                    InterpretResult::SetActiveSlide(slide_id)
                }
            }
            InteractionEvent::AddSlideRequested => {
                match build_insert_slide_after_active(&self.dispatcher, self.active_slide.as_ref())
                {
                    Some((cmd, new_id)) => {
                        // react_to_outcome switches to this slide once
                        // the InsertSlide command has applied.
                        self.pending_new_active_slide = Some(new_id);
                        InterpretResult::Command(cmd)
                    }
                    None => InterpretResult::Nothing,
                }
            }
            InteractionEvent::SlideTitleEditRequested { slide_id, new_title } => {
                match build_set_slide_title_command(&self.dispatcher, &slide_id, &new_title) {
                    Some(cmd) => InterpretResult::Command(cmd),
                    None => InterpretResult::Nothing,
                }
            }
            // ---- Stage 11: layout editor ----
            InteractionEvent::SetEditorMode { mode } => match mode.as_str() {
                "slide" => InterpretResult::SetEditorMode(EditorMode::Slide),
                "layout" => InterpretResult::SetEditorMode(EditorMode::Layout),
                other => {
                    warn!("SetEditorMode with unknown mode: {}", other);
                    InterpretResult::Nothing
                }
            },
            InteractionEvent::LayoutThumbnailClicked { layout_id } => {
                if layout_id.is_empty() {
                    InterpretResult::Nothing
                } else {
                    InterpretResult::SetActiveLayout(layout_id)
                }
            }
            InteractionEvent::AddLayoutRequested => {
                match build_insert_layout_after_active(
                    &self.dispatcher,
                    self.active_layout.as_ref(),
                ) {
                    Some((cmd, new_id)) => {
                        self.pending_new_active_layout = Some(new_id);
                        InterpretResult::Command(cmd)
                    }
                    None => InterpretResult::Nothing,
                }
            }
            InteractionEvent::LayoutNameEditRequested { layout_id, new_name } => {
                if layout_id.is_empty()
                    || !self.dispatcher.deck().theme.layouts.contains_key(&layout_id)
                {
                    InterpretResult::Nothing
                } else {
                    InterpretResult::Command(Box::new(SetLayoutName { layout_id, new_name }))
                }
            }
            InteractionEvent::GlobalsCssEditRequested { new_css } => {
                // No-op when unchanged so the globals textarea blur doesn't
                // push a dead history entry.
                if self.dispatcher.deck().theme.globals_css == new_css {
                    InterpretResult::Nothing
                } else {
                    InterpretResult::Command(Box::new(SetGlobalsCss { new_css }))
                }
            }
            // ---- Stage: animations ----
            InteractionEvent::SetElementAnimation { element_id, category, enabled } => {
                interpret_set_element_animation(
                    self.dispatcher.deck(),
                    self.dispatcher.mode(),
                    self.active_slide.as_ref(),
                    element_id,
                    &category,
                    enabled,
                )
            }
            InteractionEvent::KeyPressed { ref key, .. } if key == UNDO_KEY => {
                InterpretResult::Undo
            }
            InteractionEvent::KeyPressed { ref key, .. } if key == REDO_KEY => {
                InterpretResult::Redo
            }
            InteractionEvent::KeyPressed { ref key, .. } if key == NEW_KEY => {
                InterpretResult::FileAction(FileAction::New)
            }
            InteractionEvent::KeyPressed { ref key, .. } if key == OPEN_KEY => {
                InterpretResult::FileAction(FileAction::Open)
            }
            InteractionEvent::KeyPressed { ref key, .. } if key == SAVE_KEY => {
                InterpretResult::FileAction(FileAction::Save)
            }
            InteractionEvent::KeyPressed { ref key, .. } if key == SAVE_AS_KEY => {
                InterpretResult::FileAction(FileAction::SaveAs)
            }
            InteractionEvent::KeyPressed { ref key, .. } if key == PRESENT_KEY => {
                InterpretResult::StartPresentation
            }
            InteractionEvent::KeyPressed { ref key, .. }
                if key == DELETE_KEY_BACKSPACE || key == DELETE_KEY_DELETE =>
            {
                self.interpret_delete_selection()
            }
            InteractionEvent::KeyPressed { key, .. } if key.eq_ignore_ascii_case(DEBUG_KEY) => {
                // Stage 4 debug shortcut preserved: build a +50px move
                // against the first child of the active slide. Returned
                // as a top-level Command (not a TransactionUpdate) so it
                // remains independently undoable when history arrives.
                match self.build_debug_nudge_command() {
                    Some(cmd) => InterpretResult::Command(cmd),
                    None => InterpretResult::Nothing,
                }
            }
            other => {
                debug!("interaction not interpreted at Stage 5: {:?}", other);
                InterpretResult::Nothing
            }
        }
    }

    // handle_interaction
    // Inputs: an InteractionEvent.
    // Output: Ok(()) on success.
    // Dataflow: route through interpret(), then realize each result
    // variant via dispatcher calls and outbound IPC. Scheduling a flush
    // happens whenever the patch buffer transitions empty → non-empty.
    // ElementIdEditRequested is handled ahead of interpret() because it
    // needs a multi-step follow-up (remap the selection onto the new id)
    // that a single InterpretResult cannot express.
    fn handle_interaction(&mut self, event: InteractionEvent) -> AppResult<()> {
        if let InteractionEvent::ElementIdEditRequested { element_id, new_id } = &event {
            return self.handle_element_id_edit(element_id.clone(), new_id.clone());
        }
        let result: InterpretResult = self.interpret(event);
        match result {
            InterpretResult::Command(cmd) => {
                // Ship any pending asset bytes BEFORE the dispatch's
                // remount lands — otherwise JS would render the new
                // image element with an unresolvable CSS variable for
                // one frame.
                if let Some(asset_id) = self.pending_asset_broadcast.take() {
                    if let Err(e) = self.send_asset_added(&asset_id) {
                        warn!(asset_id = %asset_id, "AssetAdded broadcast failed: {}", e);
                    }
                }
                self.dispatch_and_maybe_flush(cmd);
                Ok(())
            }
            InterpretResult::Selection(sel) => {
                self.selection = sel.clone();
                self.sender.send(MessageKind::SetSelection(sel))?;
                // Keep the inspector's animation toggles in sync with the
                // newly-selected element (the panel filters the slide's
                // timeline by the selected id client-side).
                self.send_slide_animations()
            }
            InterpretResult::TransactionBegin { label, snapshot } => {
                self.dispatcher.begin_transaction(label, snapshot);
                Ok(())
            }
            InterpretResult::TransactionUpdate(cmd) => {
                if !self.dispatcher.has_open_transaction() {
                    warn!("TransactionUpdate received with no open transaction");
                    return Ok(());
                }
                self.dispatch_and_maybe_flush(cmd);
                Ok(())
            }
            InterpretResult::TransactionCommit => {
                let _ = self.dispatcher.commit_transaction();
                Ok(())
            }
            InterpretResult::CommitTransactionWith(cmd) => {
                if !self.dispatcher.has_open_transaction() {
                    warn!("CommitTransactionWith received with no open transaction");
                    return Ok(());
                }
                self.dispatch_and_maybe_flush(cmd);
                let _ = self.dispatcher.commit_transaction();
                Ok(())
            }
            InterpretResult::Undo => {
                self.run_history_step(HistoryStep::Undo);
                Ok(())
            }
            InterpretResult::Redo => {
                self.run_history_step(HistoryStep::Redo);
                Ok(())
            }
            InterpretResult::FileAction(action) => self.run_file_action(action),
            InterpretResult::SetActiveSlide(slide_id) => self.set_active_slide(slide_id),
            InterpretResult::SetEditorMode(mode) => self.set_editor_mode(mode),
            InterpretResult::SetActiveLayout(layout_id) => self.set_active_layout(layout_id),
            InterpretResult::StartPresentation => {
                self.start_presentation();
                Ok(())
            }
            InterpretResult::Nothing => Ok(()),
        }
    }

    // start_presentation
    // Inputs: none (reads the deck + active slide).
    // Output: side-effect; records the start slide index and asks the event
    // loop to build the presentation window. No-op when already presenting or
    // when the deck has no slides (nothing to present).
    fn start_presentation(&mut self) {
        if self.present.is_some() {
            debug!("start_presentation: already presenting; ignoring");
            return;
        }
        let idx: usize = match present_start_index(
            self.dispatcher.deck(),
            self.active_slide.as_ref(),
        ) {
            Some(i) => i,
            None => {
                warn!("start_presentation: empty deck; nothing to present");
                return;
            }
        };
        info!(slide_index = idx, "presentation requested");
        self.pending_present_index = Some(idx);
        (self.request_present_open)();
    }

    // begin_presentation
    // Inputs: the presentation WebviewSender built by main.rs.
    // Output: side-effect; constructs the PresentationSession at the pending
    // start index. The reveal/mount happens later, when the presentation
    // webview reports Ready.
    pub fn begin_presentation(&mut self, sender: WebviewSender) {
        let idx: usize = self.pending_present_index.take().unwrap_or(0);
        assert!(
            !self.dispatcher.deck().slide_order.is_empty(),
            "begin_presentation: empty deck"
        );
        info!(slide_index = idx, "presentation window ready; session begun");
        self.present = Some(PresentationSession::new(sender, idx));
    }

    // handle_present_control
    // Inputs: a control posted by the presentation webview.
    // Output: Ok(()) on success.
    // Dataflow: Ready mounts the start slide; Advance/Back step the cursor and
    // send the resulting reveal; Exit asks the event loop to tear the window
    // down (main.rs owns the close path so the webview drops before the window).
    pub fn handle_present_control(&mut self, ctrl: PresentInbound) -> AppResult<()> {
        match ctrl {
            PresentInbound::Ready => self.handle_present_ready(),
            PresentInbound::Advance => self.present_step(true),
            PresentInbound::Back => self.present_step(false),
            PresentInbound::Exit => {
                info!("presentation exit requested");
                (self.request_present_close)();
                Ok(())
            }
        }
    }

    // handle_present_ready
    // Inputs: none.
    // Output: Ok(()) after sending PresentInit + PresentSlide + the snapped
    // step-0 PresentReveal. No-op if there is no active session.
    fn handle_present_ready(&mut self) -> AppResult<()> {
        let session = match &self.present {
            Some(s) => s,
            None => return Ok(()),
        };
        let deck: &Deck = self.dispatcher.deck();
        let init = PresentInitPayload {
            animation_keyframes_css: ANIMATION_KEYFRAMES_CSS.to_string(),
            width: deck.manifest.dimensions.width,
            height: deck.manifest.dimensions.height,
        };
        session.sender().send(MessageKind::PresentInit(init))?;
        // Ship asset bytes before the first mount so images resolve on the very
        // first paint (the present webview mints its own blob URLs from these).
        if let Some(bundle) = build_assets_bundle(deck) {
            session.sender().send(MessageKind::PresentAssets(bundle))?;
        }
        if let Some(slide) = session.current_slide_payload(deck) {
            session.sender().send(MessageKind::PresentSlide(slide))?;
        }
        if let Some(reveal) = session.current_reveal(deck) {
            session.sender().send(MessageKind::PresentReveal(reveal))?;
        }
        Ok(())
    }

    // present_step
    // Inputs: forward (true = Advance, false = Back).
    // Output: Ok(()); advances/rewinds the cursor and sends the resulting
    // reveal (and a slide mount when the step crossed slides). No-op without a
    // session. The deck and the session live in disjoint fields of self, so the
    // immutable deck borrow and the mutable session borrow coexist.
    fn present_step(&mut self, forward: bool) -> AppResult<()> {
        let deck: &Deck = self.dispatcher.deck();
        let session = match self.present.as_mut() {
            Some(s) => s,
            None => return Ok(()),
        };
        let step: PresentStep = if forward { session.advance(deck) } else { session.back(deck) };
        match step {
            PresentStep::Reveal(reveal) => {
                session.sender().send(MessageKind::PresentReveal(reveal))
            }
            PresentStep::SlideChanged { slide, reveal } => {
                session.sender().send(MessageKind::PresentSlide(slide))?;
                session.sender().send(MessageKind::PresentReveal(reveal))
            }
            PresentStep::Unchanged => Ok(()),
        }
    }

    // end_presentation
    // Inputs: none.
    // Output: side-effect; drops the session (and with it the presentation
    // WebviewSender / WebView). main.rs drops the Window afterwards.
    pub fn end_presentation(&mut self) {
        if self.present.take().is_some() {
            info!("presentation session ended");
        }
    }

    // run_file_action
    // Inputs: which File-menu action was triggered.
    // Output: side-effect; routes to the matching ApplicationCore method.
    // Errors: forwarded from the underlying file method (serialization or
    // IPC send failure). File-dialog cancellation is silent and returns Ok.
    fn run_file_action(&mut self, action: FileAction) -> AppResult<()> {
        match action {
            FileAction::New => self.file_new(),
            FileAction::Open => self.file_open(),
            FileAction::Save => self.file_save(),
            FileAction::SaveAs => self.file_save_as(),
        }
    }

    // run_history_step
    // Inputs: which direction (Undo or Redo).
    // Output: side-effect; delegates to dispatcher.undo() or
    // dispatcher.redo() and schedules a patch flush iff the patch buffer
    // transitioned empty → non-empty. Logs on no-op (empty stack) and on
    // command failure; never propagates errors because keyboard-driven
    // undo failing is a UX event, not a fatal one.
    // Dataflow: dispatcher returns Ok(Some(DispatchOutcome)) on success,
    // Ok(None) on empty stack, or Err on inverse-apply failure.
    fn run_history_step(&mut self, step: HistoryStep) {
        assert!(
            !self.dispatcher.has_open_transaction(),
            "history step received while a transaction is open"
        );
        let result = match step {
            HistoryStep::Undo => self.dispatcher.undo(),
            HistoryStep::Redo => self.dispatcher.redo(),
        };
        match result {
            Ok(Some(outcome)) => {
                debug!(?step, "history step applied");
                if outcome.needs_flush {
                    (self.schedule_flush)();
                }
                self.react_to_outcome(outcome);
            }
            Ok(None) => debug!(?step, "history step: stack empty"),
            Err(e) => warn!(?step, "history step failed: {}", e),
        }
    }

    // dispatch_and_maybe_flush
    // Inputs: a boxed Command.
    // Output: side-effect; dispatches via the dispatcher, logs failures,
    // schedules a flush if the patch buffer just became non-empty, and
    // (Stage 9) reacts to the outcome's structural flags by remounting
    // the slide and/or rebroadcasting the object tree.
    fn dispatch_and_maybe_flush(&mut self, cmd: Box<dyn Command>) {
        let label: &'static str = cmd.label();
        match self.dispatcher.dispatch(cmd) {
            Ok(outcome) => {
                debug!(label, "command dispatched");
                if outcome.needs_flush {
                    (self.schedule_flush)();
                }
                self.react_to_outcome(outcome);
            }
            Err(e) => warn!(label, "command failed: {}", e),
        }
    }

    // handle_element_id_edit
    // Inputs: the element's current id and the raw replacement text typed
    // in the object panel.
    // Output: Ok(()). Validates and sanitizes the new id, dispatches
    // SetElementId (which remounts the slide and rebuilds the object
    // tree), then remaps the selection so the renamed element stays
    // selected. No-ops (empty/unchanged id, missing element, collision)
    // re-send the object tree so the panel's edit-in-place input reverts
    // to the real id.
    // Dataflow: sanitize -> resolve active slide -> guard empty/unchanged
    // -> guard missing element / id collision -> dispatch -> remap
    // selection -> SetSelection.
    fn handle_element_id_edit(&mut self, old_id: ElementId, raw_new_id: String) -> AppResult<()> {
        let new_id: ElementId = sanitize_element_id(&raw_new_id);
        let slide_id: SlideId = match &self.active_slide {
            Some(s) => s.clone(),
            None => return Ok(()),
        };
        if new_id.is_empty() || new_id == old_id {
            // Nothing to change; refresh the panel so the inline editor
            // reverts to the element's real id.
            return self.send_object_tree();
        }
        let slide = match self.dispatcher.deck().slides.get(&slide_id) {
            Some(s) => s,
            None => return Ok(()),
        };
        if slide.find_element(&old_id).is_none() {
            return Ok(());
        }
        if slide.find_element(&new_id).is_some() {
            warn!(new_id = %new_id, "element id already in use on slide; ignoring rename");
            return self.send_object_tree();
        }

        self.dispatch_and_maybe_flush(Box::new(SetElementId {
            target: CanvasTarget::Slide(slide_id),
            old_id: old_id.clone(),
            new_id: new_id.clone(),
        }));

        let mut remapped: bool = false;
        for id in self.selection.element_ids.iter_mut() {
            if *id == old_id {
                *id = new_id.clone();
                remapped = true;
            }
        }
        if remapped {
            self.sender.send(MessageKind::SetSelection(self.selection.clone()))?;
        }
        Ok(())
    }

    // react_to_outcome
    // Inputs: a DispatchOutcome from dispatch / undo / redo.
    // Output: side-effect; honors the structural flags:
    //   affects_slide_list → re-anchor the active slide (to the pending
    //                        new slide, else validate the current one),
    //                        clear selection, rebroadcast SlideListUpdate,
    //                        and remount. Takes precedence over the
    //                        remount / tree paths because it remounts too.
    //   requires_remount → send_active_slide (which also re-sends the
    //                      tree, keeping panel and DOM atomically in sync).
    //   affects_object_tree (no remount) → send_object_tree alone.
    // Errors logged, not propagated — these are housekeeping sends that
    // should never fail a primary edit.
    fn react_to_outcome(&mut self, outcome: crate::commands::DispatchOutcome) {
        // Surface any non-fatal advisories first (an accommodation warning
        // still applied the command), regardless of which structural branch
        // the outcome takes below.
        for msg in &outcome.warnings {
            if let Err(e) = self.sender.send(MessageKind::Notice { message: msg.clone() }) {
                warn!("notice send failed: {}", e);
            }
        }
        if outcome.affects_slide_list {
            self.resync_after_slide_list_change();
            return;
        }
        if outcome.affects_layout_list {
            self.resync_after_layout_list_change();
            return;
        }
        if outcome.affects_globals {
            // Remount the active canvas so the new globals CSS is visible,
            // and (in layout mode) refresh the layout list so the globals
            // textarea + thumbnails reflect the committed value.
            if let Err(e) = self.send_active_slide() {
                warn!("remount after globals change failed: {}", e);
            }
            if self.dispatcher.mode() == EditorMode::Layout
                && let Err(e) = self.send_layout_list()
            {
                warn!("layout list broadcast after globals change failed: {}", e);
            }
            return;
        }
        if outcome.affects_animations {
            // The timeline changed; rebroadcast it so the inspector toggles
            // resync. No remount (animations have no static visual effect).
            if let Err(e) = self.send_slide_animations() {
                warn!("animations broadcast after dispatch failed: {}", e);
            }
            return;
        }
        if outcome.requires_remount {
            if let Err(e) = self.send_active_slide() {
                warn!("remount after dispatch failed: {}", e);
            }
        } else if outcome.affects_object_tree {
            if let Err(e) = self.send_object_tree() {
                warn!("object tree broadcast after dispatch failed: {}", e);
            }
        }
    }

    // resync_after_slide_list_change
    // Inputs: none (consumes self.pending_new_active_slide).
    // Output: side-effect; re-establishes a coherent active slide after
    // the deck's slide set changed, then rebroadcasts the slide list,
    // clears selection, and remounts.
    // Dataflow:
    //   1. If a pending new active slide was set (slide just added) and
    //      it exists, adopt it.
    //   2. Otherwise, if the current active slide vanished (slide just
    //      removed, e.g. via undo), fall back to the first slide in
    //      order — or None when the deck is empty.
    //   3. Clear selection (it referenced the prior slide's elements).
    //   4. Broadcast SlideListUpdate + SetSelection(empty), then mount
    //      the active slide (which also re-sends its object tree).
    fn resync_after_slide_list_change(&mut self) {
        if let Some(pending) = self.pending_new_active_slide.take() {
            if self.dispatcher.deck().slides.contains_key(&pending) {
                self.active_slide = Some(pending);
            }
        }
        let active_valid: bool = self
            .active_slide
            .as_ref()
            .map(|id| self.dispatcher.deck().slides.contains_key(id))
            .unwrap_or(false);
        if !active_valid {
            self.active_slide = self.dispatcher.deck().slide_order.first().cloned();
        }
        self.selection = SelectionState::empty();
        if let Err(e) = self.send_slide_list() {
            warn!("slide list broadcast after slide-list change failed: {}", e);
        }
        if let Err(e) = self.sender.send(MessageKind::SetSelection(SelectionState::empty())) {
            warn!("selection clear after slide-list change failed: {}", e);
        }
        if let Err(e) = self.send_active_slide() {
            warn!("remount after slide-list change failed: {}", e);
        }
    }

    // resync_after_layout_list_change
    // Layout-mode analogue of resync_after_slide_list_change: re-establish a
    // coherent active layout after the theme's layout set changed (add /
    // remove / rename / undo), then rebroadcast the layout list, clear
    // selection, and remount the active canvas.
    fn resync_after_layout_list_change(&mut self) {
        if let Some(pending) = self.pending_new_active_layout.take()
            && self.dispatcher.deck().theme.layouts.contains_key(&pending)
        {
            self.active_layout = Some(pending);
        }
        let active_valid: bool = self
            .active_layout
            .as_ref()
            .map(|id| self.dispatcher.deck().theme.layouts.contains_key(id))
            .unwrap_or(false);
        if !active_valid {
            self.active_layout = self.dispatcher.deck().theme.layout_order.first().cloned();
        }
        self.selection = SelectionState::empty();
        if let Err(e) = self.send_layout_list() {
            warn!("layout list broadcast after layout-list change failed: {}", e);
        }
        if let Err(e) = self.sender.send(MessageKind::SetSelection(SelectionState::empty())) {
            warn!("selection clear after layout-list change failed: {}", e);
        }
        if let Err(e) = self.send_active_slide() {
            warn!("remount after layout-list change failed: {}", e);
        }
    }

    // snapshot_for_drag
    // Inputs: the element id being dragged.
    // Output: a TransactionSnapshot pre-loaded with the element's current
    // geometry. ElementDragged handlers read this to compute absolute
    // positions from cumulative drag deltas.
    fn snapshot_for_drag(&self, element_id: &str) -> TransactionSnapshot {
        let mut snap: TransactionSnapshot = TransactionSnapshot::empty();
        let target: CanvasTarget = match self.active_canvas() {
            Some(t) => t,
            None => return snap,
        };
        let canvas = match self.dispatcher.deck().canvas(&target) {
            Some(c) => c,
            None => return snap,
        };
        let el = match canvas.find_element(element_id) {
            Some(e) => e,
            None => return snap,
        };
        snap.record_geometry(target, element_id.to_string(), el.geometry.clone());
        snap
    }

    // interpret_delete_selection
    // Inputs: none.
    // Output: an InterpretResult that, when executed, removes every
    // selected non-root element from the active slide. Zero-selection
    // and selection-of-root cases are no-ops. Multi-element selections
    // wrap into a CompositeCommand so a single undo reverses the entire
    // delete.
    fn interpret_delete_selection(&self) -> InterpretResult {
        interpret_delete_selection(&self.dispatcher, self.active_canvas(), &self.selection)
    }

    // interpret_asset_imported
    // Inputs: the AssetImported event payload — base64 bytes, file
    // metadata, natural pixel dimensions, optional slide-space drop
    // position.
    // Output: InterpretResult dispatching one InsertElement that
    // references the registered asset. The registry is mutated in this
    // method (deduping by content hash) BEFORE the command is built;
    // the `AssetAdded` IPC broadcast is fired by handle_interaction
    // after a successful dispatch so JS can resolve the new asset id.
    // Errors: returns Nothing on base64 decode failure or when no
    // active slide is present.
    // Dataflow:
    //   1. Decode base64 → bytes. Bail on error.
    //   2. registry.insert_blob → AssetEntry (deduped).
    //   3. Remember the asset id so handle_interaction can broadcast it.
    //   4. Build an Image ElementNode (natural dimensions, centered or
    //      at the drop point, inline-style background-size:cover so the
    //      object-fit semantic carries through the <div> render path).
    //   5. Construct InsertElement targeting the active slide's root.
    fn interpret_asset_imported(
        &mut self,
        content_base64: String,
        original_filename: String,
        media_type: String,
        width: u32,
        height: u32,
        position: Option<Point>,
    ) -> InterpretResult {
        let slide_id: SlideId = match &self.active_slide {
            Some(id) => id.clone(),
            None => return InterpretResult::Nothing,
        };
        let bytes: Vec<u8> =
            match base64::engine::general_purpose::STANDARD.decode(content_base64) {
                Ok(b) if !b.is_empty() => b,
                Ok(_) => {
                    warn!("AssetImported: empty bytes after decode");
                    return InterpretResult::Nothing;
                }
                Err(e) => {
                    warn!("AssetImported: base64 decode failed: {}", e);
                    return InterpretResult::Nothing;
                }
            };
        let dims = if width > 0 && height > 0 {
            Some(AssetDimensions { width, height })
        } else {
            None
        };
        let entry: AssetEntry = self.dispatcher.deck_mut().assets.insert_blob(
            bytes,
            original_filename,
            media_type,
            dims,
        );
        // Snapshot the id for the post-dispatch AssetAdded broadcast.
        self.pending_asset_broadcast = Some(entry.id.clone());

        let slide_dims: (u32, u32) = (
            self.dispatcher.deck().manifest.dimensions.width,
            self.dispatcher.deck().manifest.dimensions.height,
        );
        let node: ElementNode =
            build_image_element_from_asset(&entry, width, height, position, slide_dims);
        let parent_id: ElementId = self.dispatcher.deck().slides[&slide_id].root.id.clone();
        let position_in_parent: usize =
            self.dispatcher.deck().slides[&slide_id].root.children.len();
        InterpretResult::Command(Box::new(InsertElement {
            target: CanvasTarget::Slide(slide_id),
            parent_id,
            position: position_in_parent,
            node,
        }))
    }

    // build_debug_nudge_command
    // Inputs: none.
    // Output: a MoveElement for the active slide's first child shifted
    // +50px on x, or None if no element is available.
    fn build_debug_nudge_command(&self) -> Option<Box<dyn Command>> {
        let slide_id: SlideId = self.active_slide.clone()?;
        let slide = self.dispatcher.deck().slides.get(&slide_id)?;
        let first = slide.root.children.first()?;
        let cmd = MoveElement {
            target: CanvasTarget::Slide(slide_id),
            element_id: first.id.clone(),
            new_position: Point {
                x: first.geometry.x + DEBUG_NUDGE_PX,
                y: first.geometry.y,
            },
            previous_position: None,
        };
        Some(Box::new(cmd))
    }

    // file_new
    // Inputs: none.
    // Output: side-effect; replaces the in-memory deck with a fresh blank
    // one (single empty slide) and remounts the viewport.
    // Errors: only an IPC failure forwarding the new mount.
    // Dataflow: build Deck::new_blank -> swap into the dispatcher
    // (which also clears history because it owns the dispatcher) -> ship
    // a SetSelection clear + MountSlide of the new blank slide.
    pub fn file_new(&mut self) -> AppResult<()> {
        info!("file: new (blank deck)");
        let deck: Deck = Deck::new_blank();
        self.adopt_deck(deck);
        self.sender
            .send(MessageKind::SetSelection(SelectionState::empty()))?;
        self.send_slide_list()?;
        self.send_assets_bundle()?;
        self.send_active_slide()
    }

    // file_save
    // Inputs: none.
    // Output: Ok(()) when the save was queued (or Save-As was triggered).
    // Errors: serialization failure.
    // Dataflow: if the deck has a bundle_path, serialize and submit a Save
    // IoRequest. Otherwise fall through to file_save_as.
    pub fn file_save(&mut self) -> AppResult<()> {
        let target: PathBuf = match self.dispatcher.deck().bundle_path.clone() {
            Some(p) => p,
            None => {
                debug!("file: save → falling through to save-as (no bundle_path)");
                return self.file_save_as();
            }
        };
        self.submit_save(target)
    }

    // file_save_as
    // Inputs: none.
    // Output: Ok(()) regardless of whether the user picked a path
    // (cancellation is silent).
    // Errors: serialization failure.
    // Dataflow: show the OS Save dialog (blocking call; OK because the
    // dialog itself is modal) -> if a path was picked, serialize and
    // submit a Save IoRequest.
    pub fn file_save_as(&mut self) -> AppResult<()> {
        let picked: Option<PathBuf> = prompt_save_as(self.dispatcher.deck().bundle_path.as_deref());
        let target: PathBuf = match picked {
            Some(p) => ensure_extension(p, BUNDLE_FILE_EXTENSION),
            None => {
                debug!("file: save-as cancelled by user");
                return Ok(());
            }
        };
        self.submit_save(target)
    }

    // file_open
    // Inputs: none.
    // Output: Ok(()) regardless of whether the user picked a path.
    // Errors: none direct (load failures arrive asynchronously as
    // IoResponse::Error and are reported from handle_io_response).
    // Dataflow: show the OS Open dialog -> if a path was picked, submit
    // a Load IoRequest. The deck is replaced when the IoResponse::Loaded
    // comes back.
    pub fn file_open(&mut self) -> AppResult<()> {
        let path: PathBuf = match prompt_open() {
            Some(p) => p,
            None => {
                debug!("file: open cancelled by user");
                return Ok(());
            }
        };
        info!(path = %path.display(), "file: open requested");
        if self
            .io_thread
            .submit(IoRequest::Load { path: path.clone() })
            .is_err()
        {
            warn!("file: open could not be queued (io thread closed)");
        }
        Ok(())
    }

    // handle_io_response
    // Inputs: an IoResponse posted by the IoThread.
    // Output: Ok(()) on success.
    // Errors: deserialize failures on a Loaded response; IPC send
    // failures forwarding MountSlide to the webview.
    // Dataflow:
    //   Saved   → record the bundle_path, clear dirty flags.
    //   Loaded  → deserialize the SerializedDeck, swap into the
    //             dispatcher, remount the active slide.
    //   Error   → log and continue (the editor stays on the current deck).
    pub fn handle_io_response(&mut self, response: IoResponse) -> AppResult<()> {
        match response {
            IoResponse::Saved { path } => {
                info!(path = %path.display(), "file: save committed");
                {
                    let deck = self.dispatcher.deck_mut();
                    deck.bundle_path = Some(path);
                    deck.dirty_slides.clear();
                    deck.manifest_dirty = false;
                }
                Ok(())
            }
            IoResponse::Loaded { serialized, path } => {
                info!(path = %path.display(), "file: load received");
                let mut deck: Deck = deserialize_deck(serialized)?;
                deck.bundle_path = Some(path);
                self.adopt_deck(deck);
                self.sender
                    .send(MessageKind::SetSelection(SelectionState::empty()))?;
                self.send_slide_list()?;
                self.send_assets_bundle()?;
                self.send_active_slide()
            }
            IoResponse::Error { operation, path, message } => {
                warn!(operation, ?path, "file: io error: {}", message);
                Ok(())
            }
        }
    }

    // submit_save
    // Inputs: target file path.
    // Output: Ok(()) when the request was enqueued.
    // Errors: BundleError from serialize_deck.
    // Dataflow: serialize the current deck -> hand the owned bytes to the
    // IoThread; the I/O happens off-thread.
    fn submit_save(&mut self, target: PathBuf) -> AppResult<()> {
        info!(target = %target.display(), "file: save queued");
        let serialized = serialize_deck(self.dispatcher.deck())?;
        if self
            .io_thread
            .submit(IoRequest::Save {
                serialized,
                target_path: target,
            })
            .is_err()
        {
            warn!("file: save could not be queued (io thread closed)");
        }
        Ok(())
    }

    // adopt_deck
    // Inputs: an owned Deck (typically Deck::new_blank() or a freshly-
    // deserialised one).
    // Output: side-effect; replaces the dispatcher (and thus the deck +
    // history + transaction state), resets the active slide pointer, and
    // empties the selection.
    fn adopt_deck(&mut self, deck: Deck) {
        let active: Option<SlideId> = deck.slide_order.first().cloned();
        self.dispatcher = CommandDispatcher::new(deck);
        self.active_slide = active;
        self.selection = SelectionState::empty();
    }
}

// prompt_save_as
// Inputs: optional current bundle path used to seed the dialog's initial
// directory + filename.
// Output: the user's chosen path, or None on cancel.
// Dataflow: build an rfd::FileDialog, set the .slidedeck filter, show
// `save_file()` (blocks on the main thread; OK — modal dialog).
fn prompt_save_as(current: Option<&std::path::Path>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().add_filter("Slide Deck", &[BUNDLE_FILE_EXTENSION]);
    if let Some(p) = current {
        if let Some(parent) = p.parent() {
            dialog = dialog.set_directory(parent);
        }
        if let Some(name) = p.file_name() {
            dialog = dialog.set_file_name(name.to_string_lossy().to_string());
        }
    } else {
        dialog = dialog.set_file_name(format!("Untitled.{BUNDLE_FILE_EXTENSION}"));
    }
    dialog.save_file()
}

// prompt_open
// Inputs: none.
// Output: the user's chosen path, or None on cancel.
fn prompt_open() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Slide Deck", &[BUNDLE_FILE_EXTENSION])
        .pick_file()
}

// ensure_extension
// Inputs: a chosen path, the canonical extension (no leading dot).
// Output: the path with the extension appended if missing. Some OS save
// dialogs return paths without an extension when the user types one in
// the name field — this guarantees `.slidedeck` is always present.
fn ensure_extension(path: PathBuf, ext: &str) -> PathBuf {
    assert!(!ext.is_empty(), "ensure_extension: empty extension");
    match path.extension().and_then(|e| e.to_str()) {
        Some(existing) if existing.eq_ignore_ascii_case(ext) => path,
        _ => path.with_extension(ext),
    }
}

// build_assets_bundle
// Inputs: the deck (reads its asset registry).
// Output: an AssetsBundle carrying every registered asset's bytes
// (base64-encoded), or None when the deck has no assets (an empty bundle is
// harmless but not worth the IPC). Shared by the editor's AssetsUpdate path and
// the presentation webview's PresentAssets path so both build their blob-URL
// caches from identical data — a `blob:` URL minted in one webview is invalid in
// the other, so each context must receive the raw bytes and mint its own.
fn build_assets_bundle(deck: &Deck) -> Option<AssetsBundle> {
    let registry = &deck.assets;
    if registry.is_empty() {
        return None;
    }
    let mut payloads: Vec<AssetPayload> = Vec::with_capacity(registry.entry_count());
    for entry in &registry.assets {
        let bytes: &Vec<u8> = match registry.files.get(&entry.path) {
            Some(b) => b,
            None => {
                warn!(asset_id = %entry.id, "build_assets_bundle: file bytes missing");
                continue;
            }
        };
        payloads.push(AssetPayload {
            asset_id: entry.id.clone(),
            media_type: entry.media_type.clone(),
            content_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        });
    }
    if payloads.is_empty() {
        return None;
    }
    Some(AssetsBundle { assets: payloads })
}

// present_start_index
// Inputs: the deck and the editor's active slide id.
// Output: the index into `deck.slide_order` the presentation should open on —
// the active slide's position, falling back to the first slide, or None when
// the deck has no slides (presentation is impossible).
fn present_start_index(deck: &Deck, active: Option<&SlideId>) -> Option<usize> {
    if deck.slide_order.is_empty() {
        return None;
    }
    if let Some(a) = active
        && let Some(i) = deck.slide_order.iter().position(|s| s == a)
    {
        return Some(i);
    }
    Some(0)
}

// interpret_property_changed
// Inputs: active slide id (the inspector targets the currently mounted
// slide), element id, property name, value string.
// Output: an InterpretResult describing the command (or Nothing on a no-
// op, e.g. when no slide is active).
// Errors: none direct; downstream command apply may fail and is logged
// by handle_interaction.
// Dataflow:
//   - "x" / "y" / "width" / "height" / "rotation" / "opacity" parse as
//     f64 and route to SetGeometryProperty.
//   - Empty value on any property routes to RemoveInlineStyle.
//   - Any other property name routes to SetInlineStyle with the value
//     string passed through verbatim (CSS is the contract; we do not
//     validate the value here — the parser would reject malformed CSS
//     in a later pass).
// The function is intentionally a free function so both ApplicationCore
// (production) and the test mirror `interpret_inline` can call it.
fn interpret_property_changed(
    active: Option<CanvasTarget>,
    element_id: ElementId,
    property: String,
    value: String,
) -> InterpretResult {
    assert!(!element_id.is_empty(), "interpret_property_changed: empty element_id");
    let target: CanvasTarget = match active {
        Some(t) => t,
        None => return InterpretResult::Nothing,
    };

    if let Some(geom_prop) = GeometryProperty::from_inspector_key(&property) {
        let parsed: f64 = match value.trim().parse::<f64>() {
            Ok(v) => v,
            Err(_) => {
                warn!(property, value, "inspector: non-numeric geometry value");
                return InterpretResult::Nothing;
            }
        };
        return InterpretResult::Command(Box::new(SetGeometryProperty {
            target,
            element_id,
            property: geom_prop,
            new_value: parsed,
        }));
    }

    if value.trim().is_empty() {
        return InterpretResult::Command(Box::new(RemoveInlineStyle {
            target,
            element_id,
            property,
        }));
    }

    InterpretResult::Command(Box::new(SetInlineStyle {
        target,
        element_id,
        property,
        new_value: value,
    }))
}

// interpret_delete_selection
// Inputs: dispatcher (to read slide membership + protect the root),
// active slide id, the current selection state.
// Output: an InterpretResult dispatching one RemoveElementCommand for a
// single-element selection, or a CompositeCommand bundling N removes
// for a multi-element selection. Nothing is dispatched when the
// selection is empty, the active slide is absent, or every selected id
// is the slide root.
// Dataflow: filter selected ids → drop ones that don't exist or are
// the slide root → build one boxed Command per remaining id → wrap or
// unwrap based on count.
fn interpret_delete_selection(
    dispatcher: &CommandDispatcher,
    active: Option<CanvasTarget>,
    selection: &SelectionState,
) -> InterpretResult {
    let target: CanvasTarget = match active {
        Some(t) => t,
        None => return InterpretResult::Nothing,
    };
    if selection.element_ids.is_empty() {
        return InterpretResult::Nothing;
    }
    let canvas = match dispatcher.deck().canvas(&target) {
        Some(c) => c,
        None => return InterpretResult::Nothing,
    };
    // Filter out elements whose ancestor is also selected — removing
    // the ancestor removes the descendant as part of its subtree, so
    // explicitly deleting the descendant after would error out.
    let selected_set: std::collections::HashSet<&str> = selection
        .element_ids
        .iter()
        .map(String::as_str)
        .collect();
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    for eid in &selection.element_ids {
        if eid.is_empty() || canvas.is_root_id(eid) {
            continue;
        }
        let node = match canvas.find_element(eid) {
            Some(n) => n,
            None => continue,
        };
        if has_selected_ancestor(canvas.root(), &node.id, &selected_set) {
            continue;
        }
        commands.push(Box::new(RemoveElementCommand {
            target: target.clone(),
            element_id: eid.clone(),
        }));
    }
    match commands.len() {
        0 => InterpretResult::Nothing,
        1 => match commands.pop() {
            Some(cmd) => InterpretResult::Command(cmd),
            None => InterpretResult::Nothing,
        },
        _ => InterpretResult::Command(Box::new(CompositeCommand::new(
            commands,
            "Delete Elements",
        ))),
    }
}

// has_selected_ancestor
// Inputs: a node to scan from (slide root), the target element id, the
// set of selected ids.
// Output: true when the target sits beneath an ancestor whose id is also
// in `selected_set` (NOT counting the target itself — being selected does
// not make you your own ancestor).
// Dataflow: iterative DFS that tracks whether any node visited on the
// current path is in selected_set. On reaching the target id, returns
// true iff at least one path ancestor was selected.
fn has_selected_ancestor(
    root: &ElementNode,
    target: &str,
    selected_set: &std::collections::HashSet<&str>,
) -> bool {
    assert!(!target.is_empty(), "has_selected_ancestor: empty target id");
    const MAX_FRAMES: usize = 4_096;
    // Stack frames: (node, depth, ancestor_selected_count).
    let mut stack: Vec<(&ElementNode, usize)> = Vec::with_capacity(16);
    stack.push((root, 0));
    let mut iter: usize = 0;
    while let Some((node, ancestor_hits)) = stack.pop() {
        assert!(iter < MAX_FRAMES, "has_selected_ancestor: depth bound exceeded");
        iter += 1;
        if node.id == target {
            return ancestor_hits > 0;
        }
        let here_hits: usize = ancestor_hits
            + if selected_set.contains(node.id.as_str()) {
                1
            } else {
                0
            };
        for child in &node.children {
            stack.push((child, here_hits));
        }
    }
    false
}

// build_object_tree
// Inputs: a SlideNode (the active slide).
// Output: the ObjectTreeData payload for the panel — slide id, the slide
// root's element id, and a list of ObjectTreeNode trees representing
// every non-root element in display order.
// Dataflow: recurse over root.children with a bounded helper.
fn build_object_tree(slide: &SlideNode) -> ObjectTreeData {
    let mut nodes: Vec<ObjectTreeNode> = Vec::with_capacity(slide.root.children.len());
    for child in &slide.root.children {
        nodes.push(build_object_tree_node(child));
    }
    ObjectTreeData {
        slide_id: slide.id.clone(),
        root_id: slide.root.id.clone(),
        nodes,
    }
}

// build_object_tree_node
// Inputs: an ElementNode.
// Output: the ObjectTreeNode mirror — id, type token, and recursive
// children. The panel labels each row with the id itself (no separate
// display name), so the shown value and the editable identity match.
// Dataflow: pure walk; iteration order matches the source children Vec,
// which is the z-order shown in the panel.
fn build_object_tree_node(node: &ElementNode) -> ObjectTreeNode {
    assert!(!node.id.is_empty(), "build_object_tree_node: empty id");
    let mut children: Vec<ObjectTreeNode> = Vec::with_capacity(node.children.len());
    for child in &node.children {
        children.push(build_object_tree_node(child));
    }
    ObjectTreeNode {
        id: node.id.clone(),
        element_type: node.element_type.as_html().to_string(),
        children,
    }
}

// build_slide_list_data
// Inputs: the deck (for slide_order, slides, manifest titles, theme,
// dimensions) and the currently active slide id.
// Output: SlideListData ready to ship in SlideListUpdate. Iterates
// slide_order so the wire payload matches canonical display order.
// Dataflow: for each slide in slide_order, find the SlideNode and its
// manifest entry, serialise the slide HTML, fall back to slide id when
// the manifest title is empty.
fn build_slide_list_data(deck: &Deck, active_slide: Option<&SlideId>) -> SlideListData {
    let mut slides: Vec<SlideListEntry> = Vec::with_capacity(deck.slide_order.len());
    for sid in &deck.slide_order {
        let slide = match deck.slides.get(sid) {
            Some(s) => s,
            None => {
                warn!(slide_id = %sid, "build_slide_list_data: slide_order ref missing");
                continue;
            }
        };
        let title: String = match deck.manifest.slides.iter().find(|e| e.id == *sid) {
            Some(entry) if !entry.title.trim().is_empty() => entry.title.clone(),
            _ => sid.clone(),
        };
        let html: String = serialize_slide(slide);
        slides.push(SlideListEntry {
            slide_id: sid.clone(),
            title,
            html,
        });
    }
    SlideListData {
        slides,
        active_slide_id: active_slide.cloned(),
        theme_css: deck.theme.theme_css.clone(),
        width: deck.manifest.dimensions.width,
        height: deck.manifest.dimensions.height,
    }
}

// build_image_element_from_asset
// Inputs: the registry entry the element will reference, the natural
// pixel dimensions of the image, the (optional) slide-space drop point,
// and the slide's pixel dimensions for centering when no drop point
// was supplied.
// Output: a fully-formed Image ElementNode ready for InsertElement.
// Dataflow:
//   1. width / height: use natural dimensions verbatim ("initialized
//      to their full size"). Width/height fall back to 320×180 when
//      the natural size is unknown so the element is still selectable.
//   2. position: clamp the drop point so the element sits inside the
//      slide; otherwise center on the slide.
//   3. inline_styles seed background-size / background-position /
//      background-repeat so the rendered <div> behaves like an
//      object-fit:cover <img>. background-color provides a placeholder
//      tone while the image's blob URL is decoded by JS.
fn build_image_element_from_asset(
    entry: &AssetEntry,
    natural_w: u32,
    natural_h: u32,
    drop_position: Option<Point>,
    slide_dims: (u32, u32),
) -> ElementNode {
    assert!(!entry.id.is_empty(), "build_image_element_from_asset: empty asset id");
    let (slide_w, slide_h) = slide_dims;
    let width: f64 = if natural_w > 0 { natural_w as f64 } else { 320.0 };
    let height: f64 = if natural_h > 0 { natural_h as f64 } else { 180.0 };
    let (px, py) = match drop_position {
        Some(p) => (p.x - width / 2.0, p.y - height / 2.0),
        None => (
            (slide_w as f64 - width) / 2.0,
            (slide_h as f64 - height) / 2.0,
        ),
    };
    let mut inline_styles: BTreeMap<String, String> = BTreeMap::new();
    inline_styles.insert("background-image".into(), format!("var(--asset-{})", entry.id));
    inline_styles.insert("background-size".into(), "cover".into());
    inline_styles.insert("background-position".into(), "center".into());
    inline_styles.insert("background-repeat".into(), "no-repeat".into());
    inline_styles.insert("background-color".into(), "#222".into());

    ElementNode {
        id: new_element_id(),
        element_type: ElementType::Image,
        geometry: crate::deck::style::Geometry {
            x: px,
            y: py,
            width,
            height,
            ..crate::deck::style::Geometry::default()
        },
        style: ElementStyle::Image(ImageStyle::default()),
        content: ElementContent::Image(AssetRef { asset_id: entry.id.clone() }),
        children: vec![],
        placeholder_fill: None,
        name: None,
        link: None,
        attributes: BTreeMap::new(),
        inline_styles,
    }
}

// interpret_rename_request
// Inputs: active slide id, element id, the new display name (empty → clear).
// Output: an InterpretResult dispatching RenameElement, or Nothing when
// there is no active slide / the element id is missing.
fn interpret_rename_request(
    active: Option<CanvasTarget>,
    element_id: ElementId,
    new_name: String,
) -> InterpretResult {
    assert!(!element_id.is_empty(), "interpret_rename_request: empty id");
    let target: CanvasTarget = match active {
        Some(t) => t,
        None => return InterpretResult::Nothing,
    };
    let new_name: Option<String> = if new_name.trim().is_empty() {
        None
    } else {
        Some(new_name)
    };
    InterpretResult::Command(Box::new(RenameElement {
        target,
        element_id,
        new_name,
    }))
}

// interpret_reparent_request
// Inputs: active slide id, the moving element id, the target parent id,
// the post-removal position in the target parent's children list.
// Output: an InterpretResult dispatching ReparentElement.
fn interpret_reparent_request(
    active: Option<CanvasTarget>,
    element_id: ElementId,
    new_parent_id: ElementId,
    new_position: usize,
) -> InterpretResult {
    assert!(!element_id.is_empty(), "interpret_reparent_request: empty element id");
    assert!(!new_parent_id.is_empty(), "interpret_reparent_request: empty parent id");
    let target: CanvasTarget = match active {
        Some(t) => t,
        None => return InterpretResult::Nothing,
    };
    InterpretResult::Command(Box::new(ReparentElement {
        target,
        element_id,
        new_parent_id,
        new_position,
    }))
}

// interpret_insert_element_request
// Inputs: dispatcher (to read the current slide tree for defaults),
// active slide id, requested element type token, optional explicit
// parent + position.
// Output: an InterpretResult dispatching InsertElement with a fresh
// ElementNode constructed via type-specific defaults. Unknown element
// types log a warning and return Nothing.
// Dataflow: resolve parent (defaulting to the slide root id when
// omitted) -> resolve position (defaulting to end-of-children when
// omitted) -> build the element via construct_default_element_for_type.
fn interpret_insert_element_request(
    dispatcher: &CommandDispatcher,
    active: Option<CanvasTarget>,
    element_type: String,
    parent_id: Option<ElementId>,
    position: Option<usize>,
) -> InterpretResult {
    let target: CanvasTarget = match active {
        Some(t) => t,
        None => return InterpretResult::Nothing,
    };
    let canvas = match dispatcher.deck().canvas(&target) {
        Some(c) => c,
        None => return InterpretResult::Nothing,
    };
    let parent_id: ElementId = parent_id.unwrap_or_else(|| canvas.root().id.clone());
    let parent_children_len: usize = match canvas.find_element(&parent_id) {
        Some(n) => n.children.len(),
        None => return InterpretResult::Nothing,
    };
    let position: usize = position.unwrap_or(parent_children_len);

    let node: ElementNode = match construct_default_element_for_type(&element_type) {
        Some(n) => n,
        None => {
            warn!("InsertElementRequested with unknown element_type: {}", element_type);
            return InterpretResult::Nothing;
        }
    };
    InterpretResult::Command(Box::new(InsertElement {
        target,
        parent_id,
        position,
        node,
    }))
}

// sanitize_element_id
// Inputs: the raw id text the user typed.
// Output: the id with every run of whitespace collapsed to a single '_'
// (and leading/trailing whitespace dropped), e.g. "my  box \t" -> "my_box".
// A new id that is all whitespace collapses to the empty string, which the
// caller treats as "no change".
fn sanitize_element_id(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<&str>>().join("_")
}

// build_set_slide_title_command
// Inputs: read access to the deck, the target slide id, and the new title.
// Output: Some(SetSlideTitle) when a manifest entry for the slide exists
//   AND the title actually changed; None otherwise (unknown slide, or an
//   unchanged title that would only add a dead history entry).
// Errors: none — failures collapse to None.
fn build_set_slide_title_command(
    dispatcher: &CommandDispatcher,
    slide_id: &SlideId,
    new_title: &str,
) -> Option<Box<dyn Command>> {
    let entry = dispatcher
        .deck()
        .manifest
        .slides
        .iter()
        .find(|e| e.id == *slide_id)?;
    if entry.title == new_title {
        return None;
    }
    Some(Box::new(SetSlideTitle {
        slide_id: slide_id.clone(),
        new_title: new_title.to_string(),
    }))
}

// build_set_text_command
// Inputs:
//   dispatcher   — read access to the live deck.
//   active_slide — the slide currently being edited, if any.
//   element_id   — the text element whose content was committed.
//   new_text     — the final plain text from the webview edit session.
// Output: Some(SetTextContent) when the element exists, is a Text element,
//   AND the text actually changed; None otherwise. Returning None for an
//   unchanged edit keeps double-click-then-click-away from pushing an
//   empty history entry.
// Errors: none — every validation failure collapses to None so malformed
//   inbound IPC can never panic the editor.
// Control flow: resolve the active slide -> find the element -> confirm it
//   carries Text content and that new_text differs from the current plain
//   text -> build the command.
fn build_set_text_command(
    dispatcher: &CommandDispatcher,
    active: Option<CanvasTarget>,
    element_id: ElementId,
    new_text: String,
) -> Option<Box<dyn Command>> {
    let target: CanvasTarget = active?;
    assert!(!target.id().is_empty(), "build_set_text_command: active canvas id is empty");
    let canvas = dispatcher.deck().canvas(&target)?;
    let element = canvas.find_element(&element_id)?;
    let current: &str = match &element.content {
        ElementContent::Text(rt) => rt.plain.as_str(),
        _ => return None,
    };
    if current == new_text {
        return None;
    }
    Some(Box::new(SetTextContent {
        target,
        element_id,
        new_content: RichText::new(new_text),
    }))
}

// interpret_set_element_animation
// Inputs: the deck (read), editor mode, active slide, target element, the
// category string ("entrance"|"exit"), and the toggle state.
// Output: an InsertAnimation when enabling an absent animation of that
// category, a RemoveAnimation when disabling a present one, else Nothing.
// Animations are slide-only, so this no-ops outside Slide mode. The minimal
// UI uses the built-in "appear"/"disappear" keyframes with default timing.
fn interpret_set_element_animation(
    deck: &Deck,
    mode: EditorMode,
    active_slide: Option<&SlideId>,
    element_id: ElementId,
    category: &str,
    enabled: bool,
) -> InterpretResult {
    if mode != EditorMode::Slide {
        return InterpretResult::Nothing;
    }
    let slide_id: SlideId = match active_slide {
        Some(s) => s.clone(),
        None => return InterpretResult::Nothing,
    };
    let cat = match category {
        "entrance" => AnimationCategory::Entrance,
        "exit" => AnimationCategory::Exit,
        _ => return InterpretResult::Nothing,
    };
    let slide = match deck.slides.get(&slide_id) {
        Some(s) => s,
        None => return InterpretResult::Nothing,
    };
    let existing: Option<String> = slide
        .animations
        .iter()
        .find(|e| e.element_id == element_id && e.category == cat)
        .map(|e| e.id.clone());
    match (enabled, existing) {
        (true, None) => {
            let keyframe = if cat == AnimationCategory::Entrance { "appear" } else { "disappear" };
            let entry = AnimationEntry::new(
                new_animation_id(),
                element_id,
                keyframe.to_string(),
                cat,
                AnimationTrigger::OnClick,
                AnimationTiming::default(),
            );
            InterpretResult::Command(Box::new(InsertAnimation {
                slide_id,
                position: slide.animations.len(),
                entry,
            }))
        }
        (false, Some(id)) => {
            InterpretResult::Command(Box::new(RemoveAnimation { slide_id, animation_id: id }))
        }
        _ => InterpretResult::Nothing,
    }
}

// build_insert_slide_after_active
// Inputs:
//   dispatcher   — read access to the live deck (slide_order is consulted
//                  to derive the insert position).
//   active_slide — the currently-mounted slide, if any. The new slide is
//                  inserted directly after it; when None (or the active id
//                  is somehow absent from the order) the slide is appended.
// Output: Some((InsertSlide command, fresh slide id)). The caller stashes
//   the id as the pending new active slide so react_to_outcome switches to
//   it once the command applies. Never None in practice — construction
//   cannot fail — but the Option keeps the interpret arm uniform with the
//   other request builders.
// Errors: none here; InsertSlide validates duplicate ids at apply time.
// Control flow: mint a fresh slide id -> locate the active slide's index
//   to derive position (+1), defaulting to append -> build an empty
//   blank-layout SlideNode plus its matching manifest entry -> wrap them
//   in an InsertSlide command.
fn build_insert_slide_after_active(
    dispatcher: &CommandDispatcher,
    active_slide: Option<&SlideId>,
) -> Option<(Box<dyn Command>, SlideId)> {
    use crate::bundle::SlideEntry;
    use crate::bundle::manifest::slide_path_for;
    use crate::deck::builders::group_element;
    use crate::deck::new_slide_id;

    let order: &[SlideId] = &dispatcher.deck().slide_order;
    let position: usize = match active_slide {
        Some(id) => order.iter().position(|s| s == id).map(|i| i + 1).unwrap_or(order.len()),
        None => order.len(),
    };

    let slide_id: SlideId = new_slide_id();
    assert!(!slide_id.is_empty(), "build_insert_slide_after_active: minted empty slide id");
    let root: ElementNode = group_element(new_element_id(), vec![]);
    let slide: SlideNode = SlideNode::new(slide_id.clone(), "blank".into(), root);

    let manifest_entry: SlideEntry = SlideEntry {
        id: slide_id.clone(),
        path: slide_path_for(&slide_id),
        layout_id: "blank".into(),
        title: String::new(),
        thumbnail: None,
        transition: None,
        duration_hint: None,
        notes_ref: None,
        animations: Vec::new(),
    };

    let cmd: Box<dyn Command> = Box::new(InsertSlide { position, slide, manifest_entry });
    Some((cmd, slide_id))
}

// build_insert_layout_after_active
// Inputs: dispatcher (to read layout_order for the insert position and to
// dedupe the new id) and the active layout id.
// Output: Some((InsertLayout command, fresh layout id)). The caller stashes
// the id as the pending new active layout. Mirrors
// build_insert_slide_after_active.
// Control flow: derive position (+1 after the active layout, else append)
// -> mint a unique "Layout N" name / slugged id not already in the theme
// -> build an empty blank-rooted LayoutNode -> wrap in InsertLayout.
fn build_insert_layout_after_active(
    dispatcher: &CommandDispatcher,
    active_layout: Option<&LayoutId>,
) -> Option<(Box<dyn Command>, LayoutId)> {
    use crate::deck::builders::group_element;

    let order: &[LayoutId] = &dispatcher.deck().theme.layout_order;
    let position: usize = match active_layout {
        Some(id) => order.iter().position(|l| l == id).map(|i| i + 1).unwrap_or(order.len()),
        None => order.len(),
    };

    // Mint a unique display name + slugged id. Bounded by a generous cap so
    // the search always terminates (CLAUDE.md: loops need a fixed bound).
    let mut n: usize = order.len() + 1;
    let mut layout_id: LayoutId = String::new();
    let mut name: String = String::new();
    const MAX_TRIES: usize = 10_000;
    let mut tries: usize = 0;
    while tries < MAX_TRIES {
        name = format!("Layout {n}");
        layout_id = sanitize_element_id(&name.to_lowercase());
        if !dispatcher.deck().theme.layouts.contains_key(&layout_id) {
            break;
        }
        n += 1;
        tries += 1;
    }
    assert!(!layout_id.is_empty(), "build_insert_layout_after_active: minted empty id");

    let root: ElementNode = group_element(new_element_id(), vec![]);
    let layout: LayoutNode = LayoutNode::new(layout_id.clone(), name, root);
    let cmd: Box<dyn Command> = Box::new(InsertLayout { position, layout });
    Some((cmd, layout_id))
}

// build_layout_list_data
// Inputs: the deck (layout_order, layouts, theme/globals CSS, dimensions)
// and the active layout id.
// Output: LayoutListData ready to ship in LayoutListUpdate, iterating
// layout_order so the wire payload matches display order. Each layout
// serializes through a transient SlideNode so it reuses the slide
// serializer (a layout root is a Group, like a slide root).
fn build_layout_list_data(deck: &Deck, active_layout: Option<&LayoutId>) -> LayoutListData {
    let mut layouts: Vec<LayoutListEntry> = Vec::with_capacity(deck.theme.layout_order.len());
    for lid in &deck.theme.layout_order {
        let layout = match deck.theme.layouts.get(lid) {
            Some(l) => l,
            None => {
                warn!(layout_id = %lid, "build_layout_list_data: layout_order ref missing");
                continue;
            }
        };
        let transient: SlideNode =
            SlideNode::new(layout.id.clone(), layout.id.clone(), layout.root.clone());
        layouts.push(LayoutListEntry {
            layout_id: lid.clone(),
            name: layout.name.clone(),
            html: serialize_slide(&transient),
        });
    }
    LayoutListData {
        layouts,
        active_layout_id: active_layout.cloned(),
        theme_css: deck.theme.theme_css.clone(),
        globals_css: deck.theme.globals_css.clone(),
        width: deck.manifest.dimensions.width,
        height: deck.manifest.dimensions.height,
    }
}

// construct_default_element_for_type
// Inputs: an element-type token ("text", "shape", "group").
// Output: Some(ElementNode) with reasonable defaults — centered on the
// 1920×1080 slide, sensible content placeholder, fresh ULID; None for
// unknown types (Stage 9 wires three; image / media / table land with
// asset import later).
// Dataflow: branch on the type token; each branch builds a fresh node.
fn construct_default_element_for_type(element_type: &str) -> Option<ElementNode> {
    match element_type {
        "text" => Some(default_text_element()),
        "shape" => Some(default_shape_element()),
        "group" => Some(default_group_element()),
        _ => None,
    }
}

fn default_text_element() -> ElementNode {
    let id: ElementId = new_element_id();
    ElementNode {
        id,
        element_type: ElementType::Text,
        geometry: Geometry {
            x: 720.0,
            y: 480.0,
            width: 480.0,
            height: 120.0,
            ..Geometry::default()
        },
        style: ElementStyle::Text(TextStyle {
            font_size: Length::px(48.0),
            color: ColorRef::Theme("foreground".into()),
            font_family: FontRef::Theme("body_family".into()),
            ..TextStyle::default()
        }),
        content: ElementContent::Text(RichText::new("New Text")),
        children: vec![],
        placeholder_fill: None,
        name: None,
        link: None,
        attributes: BTreeMap::new(),
        inline_styles: BTreeMap::new(),
    }
}

fn default_shape_element() -> ElementNode {
    let id: ElementId = new_element_id();
    ElementNode {
        id,
        element_type: ElementType::Shape,
        geometry: Geometry {
            x: 760.0,
            y: 465.0,
            width: 400.0,
            height: 200.0,
            ..Geometry::default()
        },
        style: ElementStyle::Shape(ShapeStyle::default()),
        content: ElementContent::Shape(ShapeGeometry::Rectangle),
        children: vec![],
        placeholder_fill: None,
        name: None,
        link: None,
        attributes: BTreeMap::new(),
        inline_styles: {
            let mut m: BTreeMap<String, String> = BTreeMap::new();
            // Give the empty shape a visible default so it doesn't look
            // like nothing happened on insert.
            m.insert("background-color".into(), "var(--theme-accent, #0066ff)".into());
            m
        },
    }
}

fn default_group_element() -> ElementNode {
    let id: ElementId = new_element_id();
    ElementNode {
        id,
        element_type: ElementType::Group,
        geometry: Geometry {
            x: 760.0,
            y: 465.0,
            width: 400.0,
            height: 200.0,
            ..Geometry::default()
        },
        style: ElementStyle::Group,
        content: ElementContent::Group,
        children: vec![],
        placeholder_fill: None,
        name: None,
        link: None,
        attributes: BTreeMap::new(),
        inline_styles: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::ipc::{Modifiers, Vec2};

    // build_test_core
    // Constructs an ApplicationCore without an actual webview. The
    // `sender` field requires a WebviewSender, which owns a Wry WebView
    // that cannot be constructed headlessly; therefore `interpret` is
    // tested without going through `handle_interaction`. The dispatcher
    // and selection are exercised directly for transaction tests.
    fn modifiers_default() -> Modifiers {
        Modifiers::default()
    }

    fn modifiers_shift() -> Modifiers {
        Modifiers { shift: true, ..Modifiers::default() }
    }

    fn fixture() -> (CommandDispatcher, SelectionState, SlideId, ElementId) {
        let deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let eid: ElementId = deck.slides[&sid].root.children[0].id.clone();
        let dispatcher = CommandDispatcher::new(deck);
        (dispatcher, SelectionState::empty(), sid, eid)
    }

    // ApplicationCore::interpret is a method on `self`, but for tests we
    // only need a value carrying selection + dispatcher + active_slide.
    // The shared bits live in this helper that returns a tuple so we can
    // call the inner interpretation logic without standing up a webview.
    // Each test below re-implements the same dispatch using these parts.
    fn interpret_inline(
        dispatcher: &CommandDispatcher,
        selection: &SelectionState,
        active_slide: &Option<SlideId>,
        event: InteractionEvent,
    ) -> InterpretResult {
        // Mirrors ApplicationCore::interpret's body. Kept in lock-step
        // with the production method.
        match event {
            InteractionEvent::ElementClicked { element_id, modifiers, .. } => {
                let mut sel: SelectionState = if modifiers.shift {
                    selection.clone()
                } else {
                    SelectionState::empty()
                };
                sel.slide_id = active_slide.clone();
                if modifiers.shift {
                    sel.toggle(element_id);
                } else if !sel.contains(&element_id) {
                    sel.element_ids.push(element_id);
                }
                InterpretResult::Selection(sel)
            }
            InteractionEvent::ElementDragStarted { element_id, .. } => {
                let mut snap = TransactionSnapshot::empty();
                if let Some(sid) = active_slide.clone() {
                    if let Some(slide) = dispatcher.deck().slides.get(&sid) {
                        if let Some(el) = slide.find_element(&element_id) {
                            snap.record_geometry(CanvasTarget::Slide(sid), element_id, el.geometry.clone());
                        }
                    }
                }
                InterpretResult::TransactionBegin { label: DRAG_TRANSACTION_LABEL, snapshot: snap }
            }
            InteractionEvent::ElementDragged { .. } => InterpretResult::Nothing,
            InteractionEvent::ElementDragEnded { element_id, delta } => {
                let sid = match active_slide.clone() {
                    Some(s) => s,
                    None => return InterpretResult::Nothing,
                };
                let (sx, sy) = match dispatcher
                    .transaction()
                    .and_then(|t| t.start_snapshot.position_of(&CanvasTarget::Slide(sid.clone()), &element_id))
                {
                    Some(p) => p,
                    None => return InterpretResult::Nothing,
                };
                InterpretResult::CommitTransactionWith(Box::new(MoveElement {
                    target: CanvasTarget::Slide(sid),
                    element_id,
                    new_position: Point { x: sx + delta.x, y: sy + delta.y },
                    previous_position: None,
                }))
            }
            InteractionEvent::ElementResizeStarted { element_id, .. } => {
                let mut snap = TransactionSnapshot::empty();
                if let Some(sid) = active_slide.clone() {
                    if let Some(slide) = dispatcher.deck().slides.get(&sid) {
                        if let Some(el) = slide.find_element(&element_id) {
                            snap.record_geometry(CanvasTarget::Slide(sid), element_id, el.geometry.clone());
                        }
                    }
                }
                InterpretResult::TransactionBegin {
                    label: RESIZE_TRANSACTION_LABEL,
                    snapshot: snap,
                }
            }
            InteractionEvent::ElementResized { .. } => InterpretResult::Nothing,
            InteractionEvent::ElementResizeEnded {
                element_id,
                new_position,
                new_size,
            } => {
                let sid = match active_slide.clone() {
                    Some(s) => s,
                    None => return InterpretResult::Nothing,
                };
                if dispatcher
                    .transaction()
                    .and_then(|t| t.start_snapshot.position_of(&CanvasTarget::Slide(sid.clone()), &element_id))
                    .is_none()
                {
                    return InterpretResult::Nothing;
                }
                InterpretResult::CommitTransactionWith(Box::new(ResizeElement {
                    target: CanvasTarget::Slide(sid),
                    element_id,
                    new_x: new_position.x,
                    new_y: new_position.y,
                    new_width: new_size.width,
                    new_height: new_size.height,
                }))
            }
            InteractionEvent::BackgroundClicked { .. } => {
                InterpretResult::Selection(SelectionState::empty())
            }
            InteractionEvent::PropertyChanged { element_id, property, value } => {
                interpret_property_changed(
                    active_slide.clone().map(CanvasTarget::Slide),
                    element_id,
                    property,
                    value,
                )
            }
            InteractionEvent::SetSelectionFromPanel { element_ids } => {
                let mut sel: SelectionState = SelectionState::empty();
                sel.slide_id = active_slide.clone();
                sel.element_ids = element_ids;
                InterpretResult::Selection(sel)
            }
            InteractionEvent::InsertElementRequested {
                element_type,
                parent_id,
                position,
            } => interpret_insert_element_request(
                dispatcher,
                active_slide.clone().map(CanvasTarget::Slide),
                element_type,
                parent_id,
                position,
            ),
            InteractionEvent::RenameElementRequested { element_id, new_name } => {
                interpret_rename_request(active_slide.clone().map(CanvasTarget::Slide), element_id, new_name)
            }
            InteractionEvent::ReparentElementRequested {
                element_id,
                new_parent_id,
                new_position,
            } => interpret_reparent_request(
                active_slide.clone().map(CanvasTarget::Slide),
                element_id,
                new_parent_id,
                new_position,
            ),
            InteractionEvent::SlideThumbnailClicked { slide_id } => {
                if slide_id.is_empty() {
                    InterpretResult::Nothing
                } else {
                    InterpretResult::SetActiveSlide(slide_id)
                }
            }
            InteractionEvent::KeyPressed { ref key, .. } if key == UNDO_KEY => {
                InterpretResult::Undo
            }
            InteractionEvent::KeyPressed { ref key, .. } if key == REDO_KEY => {
                InterpretResult::Redo
            }
            InteractionEvent::KeyPressed { ref key, .. } if key == NEW_KEY => {
                InterpretResult::FileAction(FileAction::New)
            }
            InteractionEvent::KeyPressed { ref key, .. } if key == OPEN_KEY => {
                InterpretResult::FileAction(FileAction::Open)
            }
            InteractionEvent::KeyPressed { ref key, .. } if key == SAVE_KEY => {
                InterpretResult::FileAction(FileAction::Save)
            }
            InteractionEvent::KeyPressed { ref key, .. } if key == SAVE_AS_KEY => {
                InterpretResult::FileAction(FileAction::SaveAs)
            }
            InteractionEvent::KeyPressed { ref key, .. }
                if key == DELETE_KEY_BACKSPACE || key == DELETE_KEY_DELETE =>
            {
                interpret_delete_selection(dispatcher, active_slide.clone().map(CanvasTarget::Slide), selection)
            }
            _ => InterpretResult::Nothing,
        }
    }

    #[test]
    fn clicking_element_produces_singleton_selection() {
        let (d, sel, sid, eid) = fixture();
        let event = InteractionEvent::ElementClicked {
            element_id: eid.clone(),
            modifiers: modifiers_default(),
            position: Point { x: 0.0, y: 0.0 },
        };
        match interpret_inline(&d, &sel, &Some(sid.clone()), event) {
            InterpretResult::Selection(s) => {
                assert_eq!(s.element_ids, vec![eid]);
                assert_eq!(s.slide_id, Some(sid));
            }
            other => panic!("expected Selection, got {other:?}"),
        }
    }

    #[test]
    fn shift_click_extends_existing_selection() {
        let (d, mut sel, sid, eid) = fixture();
        sel.slide_id = Some(sid.clone());
        sel.element_ids.push("existing".into());
        let event = InteractionEvent::ElementClicked {
            element_id: eid.clone(),
            modifiers: modifiers_shift(),
            position: Point { x: 0.0, y: 0.0 },
        };
        match interpret_inline(&d, &sel, &Some(sid.clone()), event) {
            InterpretResult::Selection(s) => {
                assert!(s.element_ids.contains(&"existing".to_string()));
                assert!(s.element_ids.contains(&eid));
                assert_eq!(s.slide_id, Some(sid));
            }
            other => panic!("expected Selection, got {other:?}"),
        }
    }

    #[test]
    fn shift_click_on_selected_element_toggles_off() {
        let (d, mut sel, sid, eid) = fixture();
        sel.slide_id = Some(sid.clone());
        sel.element_ids.push(eid.clone());
        let event = InteractionEvent::ElementClicked {
            element_id: eid.clone(),
            modifiers: modifiers_shift(),
            position: Point { x: 0.0, y: 0.0 },
        };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::Selection(s) => {
                assert!(!s.element_ids.contains(&eid));
            }
            other => panic!("expected Selection, got {other:?}"),
        }
    }

    #[test]
    fn plain_click_replaces_existing_selection() {
        let (d, mut sel, sid, eid) = fixture();
        sel.slide_id = Some(sid.clone());
        sel.element_ids.push("other_thing".into());
        let event = InteractionEvent::ElementClicked {
            element_id: eid.clone(),
            modifiers: modifiers_default(),
            position: Point { x: 0.0, y: 0.0 },
        };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::Selection(s) => {
                assert_eq!(s.element_ids, vec![eid]);
            }
            other => panic!("expected Selection, got {other:?}"),
        }
    }

    #[test]
    fn background_click_clears_selection() {
        let (d, mut sel, sid, _) = fixture();
        sel.slide_id = Some(sid.clone());
        sel.element_ids.push("foo".into());
        let event = InteractionEvent::BackgroundClicked { position: Point { x: 0.0, y: 0.0 } };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::Selection(s) => {
                assert!(s.is_empty());
                assert!(s.slide_id.is_none());
            }
            other => panic!("expected Selection, got {other:?}"),
        }
    }

    #[test]
    fn drag_started_emits_transaction_begin_with_geometry_snapshot() {
        let (d, sel, sid, eid) = fixture();
        let event = InteractionEvent::ElementDragStarted {
            element_id: eid.clone(),
            position: Point { x: 0.0, y: 0.0 },
        };
        match interpret_inline(&d, &sel, &Some(sid.clone()), event) {
            InterpretResult::TransactionBegin { label, snapshot } => {
                assert_eq!(label, "Move Element");
                assert!(snapshot.position_of(&CanvasTarget::Slide(sid.clone()), &eid).is_some());
            }
            other => panic!("expected TransactionBegin, got {other:?}"),
        }
    }

    #[test]
    fn drag_dragged_is_a_no_op_on_rust_side() {
        // Mid-drag tree mutations are suppressed to avoid echo patches
        // that would double-translate the optimistically-transformed
        // element. The Rust tree is updated once at ElementDragEnded.
        let (mut d, sel, sid, eid) = fixture();
        let geo = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();
        let mut snap = TransactionSnapshot::empty();
        snap.record_geometry(CanvasTarget::Slide(sid.clone()), eid.clone(), geo);
        d.begin_transaction("Move Element", snap);

        let event = InteractionEvent::ElementDragged {
            element_id: eid,
            delta: Vec2 { x: 25.0, y: -10.0 },
            position: Point { x: 0.0, y: 0.0 },
        };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::Nothing => {}
            other => panic!("expected Nothing, got {other:?}"),
        }
    }

    #[test]
    fn drag_ended_emits_commit_with_final_move() {
        let (mut d, sel, sid, eid) = fixture();
        let geo = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();
        let mut snap = TransactionSnapshot::empty();
        snap.record_geometry(CanvasTarget::Slide(sid.clone()), eid.clone(), geo.clone());
        d.begin_transaction("Move Element", snap);

        let event = InteractionEvent::ElementDragEnded {
            element_id: eid,
            delta: Vec2 { x: 25.0, y: -10.0 },
        };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::CommitTransactionWith(cmd) => {
                assert_eq!(cmd.label(), "Move Element");
                let mut tmp = Deck::sample();
                let out = cmd.apply(&mut tmp).unwrap();
                let mut left_seen = false;
                let mut top_seen = false;
                for p in &out.patches {
                    if let Patch::SetStyle { property, value, .. } = p {
                        if property == "left" {
                            assert_eq!(value, &format!("{}px", geo.x + 25.0));
                            left_seen = true;
                        }
                        if property == "top" {
                            assert_eq!(value, &format!("{}px", geo.y - 10.0));
                            top_seen = true;
                        }
                    }
                }
                assert!(left_seen && top_seen);
            }
            other => panic!("expected CommitTransactionWith, got {other:?}"),
        }
    }

    #[test]
    fn drag_ended_without_snapshot_returns_nothing() {
        let (d, sel, sid, eid) = fixture();
        let event = InteractionEvent::ElementDragEnded {
            element_id: eid,
            delta: Vec2 { x: 1.0, y: 1.0 },
        };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::Nothing => {}
            other => panic!("expected Nothing, got {other:?}"),
        }
    }

    #[test]
    fn drag_lifecycle_end_to_end_updates_geometry() {
        let (mut d, _sel, sid, eid) = fixture();
        let start_geo = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();

        // Begin: snapshot
        let mut snap = TransactionSnapshot::empty();
        snap.record_geometry(CanvasTarget::Slide(sid.clone()), eid.clone(), start_geo.clone());
        d.begin_transaction("Move Element", snap);

        // Update: dispatch a MoveElement against the deck
        let cmd = MoveElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_position: Point { x: start_geo.x + 100.0, y: start_geo.y + 200.0 },
            previous_position: None,
        };
        d.dispatch(Box::new(cmd)).unwrap();

        // Commit: drop the transaction; geometry should reflect new pos.
        d.commit_transaction().unwrap();
        let after = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();
        assert_eq!(after.x, start_geo.x + 100.0);
        assert_eq!(after.y, start_geo.y + 200.0);
    }

    // ---------- Stage 6: undo/redo interpret + end-to-end routing ----------

    #[test]
    fn key_pressed_undo_maps_to_interpret_undo() {
        let (d, sel, sid, _) = fixture();
        let event = InteractionEvent::KeyPressed {
            key: UNDO_KEY.into(),
            modifiers: modifiers_default(),
        };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::Undo => {}
            other => panic!("expected Undo, got {other:?}"),
        }
    }

    #[test]
    fn key_pressed_redo_maps_to_interpret_redo() {
        let (d, sel, sid, _) = fixture();
        let event = InteractionEvent::KeyPressed {
            key: REDO_KEY.into(),
            modifiers: modifiers_default(),
        };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::Redo => {}
            other => panic!("expected Redo, got {other:?}"),
        }
    }

    #[test]
    fn key_pressed_undo_with_meta_modifier_still_maps_to_undo() {
        // The JS host posts modifiers along with the synthetic key. The
        // interpret arm matches on the key string only — modifiers travel
        // for telemetry but do not gate the dispatch.
        let (d, sel, sid, _) = fixture();
        let event = InteractionEvent::KeyPressed {
            key: UNDO_KEY.into(),
            modifiers: Modifiers { meta: true, ..Modifiers::default() },
        };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::Undo => {}
            other => panic!("expected Undo, got {other:?}"),
        }
    }

    #[test]
    fn dispatcher_undo_after_dispatch_restores_geometry() {
        // Drives the end-to-end command-history cycle that interpret's
        // Undo branch ultimately triggers (sans the WebviewSender):
        // dispatch -> undo -> verify deck restored.
        let (mut d, _sel, sid, eid) = fixture();
        let original = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();
        d.dispatch(Box::new(MoveElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_position: Point { x: original.x + 444.0, y: original.y + 222.0 },
            previous_position: None,
        }))
        .unwrap();
        let _ = d.take_patches();
        d.undo().unwrap().expect("undo not a no-op");
        let geo = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();
        assert_eq!(geo.x, original.x);
        assert_eq!(geo.y, original.y);
    }

    #[test]
    fn dispatcher_redo_after_undo_reapplies_command() {
        let (mut d, _sel, sid, eid) = fixture();
        d.dispatch(Box::new(MoveElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_position: Point { x: 21.0, y: 84.0 },
            previous_position: None,
        }))
        .unwrap();
        let _ = d.take_patches();
        d.undo().unwrap();
        let _ = d.take_patches();
        d.redo().unwrap().expect("redo not a no-op");
        let geo = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();
        assert_eq!(geo.x, 21.0);
        assert_eq!(geo.y, 84.0);
    }

    // ---------- Stage 7: file accelerator interpret arms ----------

    fn assert_file_action(key: &str, expected: FileAction) {
        let (d, sel, sid, _) = fixture();
        let event = InteractionEvent::KeyPressed {
            key: key.into(),
            modifiers: modifiers_default(),
        };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::FileAction(a) => assert_eq!(a, expected),
            other => panic!("expected FileAction({expected:?}), got {other:?}"),
        }
    }

    #[test]
    fn key_pressed_new_deck_maps_to_file_new() {
        assert_file_action(NEW_KEY, FileAction::New);
    }

    #[test]
    fn key_pressed_open_deck_maps_to_file_open() {
        assert_file_action(OPEN_KEY, FileAction::Open);
    }

    #[test]
    fn key_pressed_save_deck_maps_to_file_save() {
        assert_file_action(SAVE_KEY, FileAction::Save);
    }

    #[test]
    fn key_pressed_save_as_deck_maps_to_file_save_as() {
        assert_file_action(SAVE_AS_KEY, FileAction::SaveAs);
    }

    #[test]
    fn ensure_extension_appends_when_missing() {
        let p = ensure_extension(PathBuf::from("/tmp/foo"), "slidedeck");
        assert_eq!(p.to_string_lossy(), "/tmp/foo.slidedeck");
    }

    #[test]
    fn ensure_extension_is_idempotent_when_already_present() {
        let p = ensure_extension(PathBuf::from("/tmp/foo.slidedeck"), "slidedeck");
        assert_eq!(p.to_string_lossy(), "/tmp/foo.slidedeck");
    }

    #[test]
    fn ensure_extension_replaces_mismatched_extension() {
        let p = ensure_extension(PathBuf::from("/tmp/foo.txt"), "slidedeck");
        assert_eq!(p.to_string_lossy(), "/tmp/foo.slidedeck");
    }

    #[test]
    fn ensure_extension_is_case_insensitive() {
        let p = ensure_extension(PathBuf::from("/tmp/foo.SLIDEDECK"), "slidedeck");
        assert_eq!(p.to_string_lossy(), "/tmp/foo.SLIDEDECK");
    }

    #[test]
    fn drag_then_undo_collapses_to_a_single_history_step() {
        // Mimics the JS drag lifecycle: begin -> N intermediate dispatches
        // -> commit -> undo. After one undo the element returns to the
        // pre-drag position even though 32 mid-drag dispatches happened.
        let (mut d, _sel, sid, eid) = fixture();
        let start = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();

        let mut snap = TransactionSnapshot::empty();
        snap.record_geometry(CanvasTarget::Slide(sid.clone()), eid.clone(), start.clone());
        d.begin_transaction(DRAG_TRANSACTION_LABEL, snap);
        let mut step: f64 = 0.0;
        while step < 32.0 {
            d.dispatch(Box::new(MoveElement {
                target: CanvasTarget::Slide(sid.clone()),
                element_id: eid.clone(),
                new_position: Point { x: start.x + step, y: start.y },
                previous_position: None,
            }))
            .unwrap();
            step += 1.0;
        }
        d.commit_transaction().unwrap();
        let _ = d.take_patches();

        assert_eq!(d.history().undo_len(), 1);
        d.undo().unwrap().expect("undo not a no-op");
        let geo = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();
        assert_eq!(geo.x, start.x);
        assert_eq!(geo.y, start.y);
    }

    // ---------- Stage 8: PropertyChanged interpret ----------

    fn run_property_changed(
        prop: &str,
        value: &str,
    ) -> (InterpretResult, SlideId, ElementId) {
        let (d, sel, sid, eid) = fixture();
        let event = InteractionEvent::PropertyChanged {
            element_id: eid.clone(),
            property: prop.into(),
            value: value.into(),
        };
        (interpret_inline(&d, &sel, &Some(sid.clone()), event), sid, eid)
    }

    #[test]
    fn property_changed_x_routes_to_set_geometry_property() {
        let (result, sid, eid) = run_property_changed("x", "250");
        match result {
            InterpretResult::Command(cmd) => {
                // Apply and verify the geometry moved.
                let mut deck = Deck::sample();
                let out = cmd.apply(&mut deck).unwrap();
                assert_eq!(
                    deck.slides[&sid].find_element(&eid).unwrap().geometry.x,
                    250.0
                );
                assert_eq!(out.patches.len(), 1);
            }
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn property_changed_opacity_routes_to_set_geometry_property() {
        let (result, sid, eid) = run_property_changed("opacity", "0.5");
        match result {
            InterpretResult::Command(cmd) => {
                let mut deck = Deck::sample();
                cmd.apply(&mut deck).unwrap();
                assert_eq!(
                    deck.slides[&sid].find_element(&eid).unwrap().geometry.opacity,
                    0.5
                );
            }
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn property_changed_background_color_routes_to_set_inline_style() {
        let (result, sid, eid) = run_property_changed("background-color", "#ff0066");
        match result {
            InterpretResult::Command(cmd) => {
                let mut deck = Deck::sample();
                cmd.apply(&mut deck).unwrap();
                assert_eq!(
                    deck.slides[&sid]
                        .find_element(&eid)
                        .unwrap()
                        .inline_styles
                        .get("background-color")
                        .map(String::as_str),
                    Some("#ff0066")
                );
            }
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn property_changed_empty_value_clears_via_remove_inline_style() {
        // Seed an existing inline style, then trigger a clear.
        let (mut d, sel, sid, eid) = fixture();
        d.dispatch(Box::new(SetInlineStyle {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            property: "border".into(),
            new_value: "1px solid #000".into(),
        }))
        .unwrap();
        let event = InteractionEvent::PropertyChanged {
            element_id: eid.clone(),
            property: "border".into(),
            value: "".into(),
        };
        let result = interpret_inline(&d, &sel, &Some(sid.clone()), event);
        match result {
            InterpretResult::Command(cmd) => {
                cmd.apply(d.deck_mut()).unwrap();
                assert!(
                    !d.deck()
                        .slides[&sid]
                        .find_element(&eid)
                        .unwrap()
                        .inline_styles
                        .contains_key("border")
                );
            }
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn property_changed_invalid_geometry_value_is_nothing() {
        let (result, _, _) = run_property_changed("x", "not-a-number");
        match result {
            InterpretResult::Nothing => {}
            other => panic!("expected Nothing, got {other:?}"),
        }
    }

    #[test]
    fn property_changed_with_no_active_slide_is_nothing() {
        let (d, sel, _, eid) = fixture();
        let event = InteractionEvent::PropertyChanged {
            element_id: eid,
            property: "x".into(),
            value: "1".into(),
        };
        let result = interpret_inline(&d, &sel, &None, event);
        match result {
            InterpretResult::Nothing => {}
            other => panic!("expected Nothing, got {other:?}"),
        }
    }

    // ---------- Stage 9: object-panel interpret arms ----------

    #[test]
    fn set_selection_from_panel_replaces_selection() {
        let (d, sel, sid, _) = fixture();
        let event = InteractionEvent::SetSelectionFromPanel {
            element_ids: vec!["el_a".into(), "el_b".into()],
        };
        match interpret_inline(&d, &sel, &Some(sid.clone()), event) {
            InterpretResult::Selection(s) => {
                assert_eq!(s.slide_id, Some(sid));
                assert_eq!(s.element_ids, vec!["el_a", "el_b"]);
            }
            other => panic!("expected Selection, got {other:?}"),
        }
    }

    #[test]
    fn insert_element_requested_text_constructs_a_text_node() {
        let (d, sel, sid, _) = fixture();
        let event = InteractionEvent::InsertElementRequested {
            element_type: "text".into(),
            parent_id: None,
            position: None,
        };
        match interpret_inline(&d, &sel, &Some(sid.clone()), event) {
            InterpretResult::Command(cmd) => {
                let mut deck = Deck::sample();
                let out = cmd.apply(&mut deck).unwrap();
                // One InsertElement patch; the new node lives under root.
                assert!(out.patches.iter().any(|p| matches!(p, Patch::InsertElement { .. })));
                let new_count = deck.slides[&sid].root.children.len();
                assert_eq!(new_count, 4); // sample has 3 + the new text
            }
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn insert_element_requested_unknown_type_is_nothing() {
        let (d, sel, sid, _) = fixture();
        let event = InteractionEvent::InsertElementRequested {
            element_type: "spaceship".into(),
            parent_id: None,
            position: None,
        };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::Nothing => {}
            other => panic!("expected Nothing, got {other:?}"),
        }
    }

    #[test]
    fn insert_element_requested_with_explicit_parent_and_position_routes_through() {
        let (d, sel, sid, _) = fixture();
        let root_id = d.deck().slides[&sid].root.id.clone();
        let event = InteractionEvent::InsertElementRequested {
            element_type: "shape".into(),
            parent_id: Some(root_id.clone()),
            position: Some(0),
        };
        match interpret_inline(&d, &sel, &Some(sid.clone()), event) {
            InterpretResult::Command(cmd) => {
                let mut deck = Deck::sample();
                cmd.apply(&mut deck).unwrap();
                let first = &deck.slides[&sid].root.children[0];
                assert_eq!(first.element_type.as_html(), "shape");
            }
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn rename_request_routes_to_rename_element() {
        let (d, sel, sid, eid) = fixture();
        let event = InteractionEvent::RenameElementRequested {
            element_id: eid.clone(),
            new_name: "Title".into(),
        };
        match interpret_inline(&d, &sel, &Some(sid.clone()), event) {
            InterpretResult::Command(cmd) => {
                let mut deck = Deck::sample();
                cmd.apply(&mut deck).unwrap();
                assert_eq!(
                    deck.slides[&sid].find_element(&eid).unwrap().name.as_deref(),
                    Some("Title")
                );
            }
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn rename_request_with_empty_name_clears() {
        let (d, sel, sid, eid) = fixture();
        // Seed an existing name.
        let mut deck = Deck::sample();
        deck.slides
            .get_mut(&sid)
            .unwrap()
            .find_element_mut(&eid)
            .unwrap()
            .name = Some("Existing".into());
        let event = InteractionEvent::RenameElementRequested {
            element_id: eid.clone(),
            new_name: "   ".into(),
        };
        match interpret_inline(&d, &sel, &Some(sid.clone()), event) {
            InterpretResult::Command(cmd) => {
                cmd.apply(&mut deck).unwrap();
                assert!(deck.slides[&sid].find_element(&eid).unwrap().name.is_none());
            }
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn reparent_request_routes_to_reparent_element() {
        let (d, sel, sid, _) = fixture();
        let root_id = d.deck().slides[&sid].root.id.clone();
        let third = d.deck().slides[&sid].root.children[2].id.clone();
        let event = InteractionEvent::ReparentElementRequested {
            element_id: third.clone(),
            new_parent_id: root_id.clone(),
            new_position: 0,
        };
        match interpret_inline(&d, &sel, &Some(sid.clone()), event) {
            InterpretResult::Command(cmd) => {
                let mut deck = Deck::sample();
                cmd.apply(&mut deck).unwrap();
                assert_eq!(deck.slides[&sid].root.children[0].id, third);
            }
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn build_object_tree_mirrors_slide_children_in_order() {
        let deck = Deck::sample();
        let sid = &deck.slide_order[0];
        let slide = &deck.slides[sid];
        let tree = build_object_tree(slide);
        assert_eq!(tree.slide_id, *sid);
        assert_eq!(tree.root_id, slide.root.id);
        assert_eq!(tree.nodes.len(), slide.root.children.len());
        for i in 0..tree.nodes.len() {
            // The panel labels each row with the element id directly.
            assert_eq!(tree.nodes[i].id, slide.root.children[i].id);
        }
    }

    // ---------- Stage 9 fix: Backspace / Delete deletes selection ----------

    fn keypress(name: &str) -> InteractionEvent {
        InteractionEvent::KeyPressed {
            key: name.into(),
            modifiers: modifiers_default(),
        }
    }

    #[test]
    fn backspace_with_no_selection_is_nothing() {
        let (d, sel, sid, _) = fixture();
        match interpret_inline(&d, &sel, &Some(sid), keypress("Backspace")) {
            InterpretResult::Nothing => {}
            other => panic!("expected Nothing, got {other:?}"),
        }
    }

    #[test]
    fn backspace_with_single_selection_dispatches_remove() {
        let (d, _, sid, eid) = fixture();
        let mut sel = SelectionState::empty();
        sel.slide_id = Some(sid.clone());
        sel.element_ids.push(eid.clone());
        match interpret_inline(&d, &sel, &Some(sid.clone()), keypress("Backspace")) {
            InterpretResult::Command(cmd) => {
                assert_eq!(cmd.label(), "Delete Element");
                let mut deck = Deck::sample();
                cmd.apply(&mut deck).unwrap();
                assert!(deck.slides[&sid].find_element(&eid).is_none());
            }
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn delete_key_is_treated_the_same_as_backspace() {
        let (d, _, sid, eid) = fixture();
        let mut sel = SelectionState::empty();
        sel.slide_id = Some(sid.clone());
        sel.element_ids.push(eid);
        match interpret_inline(&d, &sel, &Some(sid), keypress("Delete")) {
            InterpretResult::Command(_) => {}
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn backspace_with_multi_selection_wraps_in_composite() {
        let (d, _, sid, _) = fixture();
        let kids: Vec<ElementId> = d.deck().slides[&sid]
            .root
            .children
            .iter()
            .map(|c| c.id.clone())
            .collect();
        let mut sel = SelectionState::empty();
        sel.slide_id = Some(sid.clone());
        sel.element_ids = kids.clone();
        match interpret_inline(&d, &sel, &Some(sid.clone()), keypress("Backspace")) {
            InterpretResult::Command(cmd) => {
                assert_eq!(cmd.label(), "Delete Elements");
                let mut deck = Deck::sample();
                cmd.apply(&mut deck).unwrap();
                for id in &kids {
                    assert!(deck.slides[&sid].find_element(id).is_none());
                }
            }
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn backspace_skips_slide_root_in_selection() {
        let (d, _, sid, _) = fixture();
        let root_id = d.deck().slides[&sid].root.id.clone();
        let mut sel = SelectionState::empty();
        sel.slide_id = Some(sid.clone());
        sel.element_ids.push(root_id);
        match interpret_inline(&d, &sel, &Some(sid), keypress("Backspace")) {
            InterpretResult::Nothing => {}
            other => panic!("expected Nothing (root cannot be deleted), got {other:?}"),
        }
    }

    #[test]
    fn backspace_with_parent_and_child_selected_only_removes_parent() {
        // Build root -> [parent_group -> [inner_text]] explicitly so we
        // can guarantee a known ancestor relationship.
        use crate::deck::builders::{group_element, text_element};
        use crate::deck::slide::SlideNode;
        use std::collections::BTreeMap;

        let inner = text_element("el_inner", "x");
        let parent = group_element("el_parent", vec![inner]);
        let root = group_element("el_root", vec![parent]);
        let slide = SlideNode::new("s".into(), "title".into(), root);
        let mut slides: BTreeMap<SlideId, SlideNode> = BTreeMap::new();
        slides.insert("s".into(), slide);
        let mut deck: Deck = Deck::default();
        deck.slides = slides;
        deck.slide_order = vec!["s".into()];

        let dispatcher = crate::commands::CommandDispatcher::new(deck);
        let mut sel = SelectionState::empty();
        sel.slide_id = Some("s".into());
        sel.element_ids = vec!["el_parent".into(), "el_inner".into()];
        match interpret_inline(&dispatcher, &sel, &Some("s".into()), keypress("Backspace")) {
            InterpretResult::Command(cmd) => {
                // The composite would have errored on the child if we
                // weren't filtering; a single Delete Element is what
                // remains.
                assert_eq!(cmd.label(), "Delete Element");
            }
            other => panic!("expected single Delete Element, got {other:?}"),
        }
    }

    // ---------- Stage 10: thumbnail / slide navigation ----------

    // Build a two-slide deck so we can test switching between slides.
    fn two_slide_deck() -> (Deck, SlideId, SlideId) {
        use crate::deck::builders::{group_element, text_element};
        use crate::deck::slide::SlideNode;
        use std::collections::BTreeMap;

        let slide_a = SlideNode::new(
            "s_a".into(),
            "title".into(),
            group_element("rt_a", vec![text_element("el_a", "a")]),
        );
        let slide_b = SlideNode::new(
            "s_b".into(),
            "title".into(),
            group_element("rt_b", vec![text_element("el_b", "b")]),
        );
        let mut slides: BTreeMap<SlideId, SlideNode> = BTreeMap::new();
        slides.insert("s_a".into(), slide_a);
        slides.insert("s_b".into(), slide_b);
        let mut deck: Deck = Deck::default();
        deck.slides = slides;
        deck.slide_order = vec!["s_a".into(), "s_b".into()];
        (deck, "s_a".into(), "s_b".into())
    }

    #[test]
    fn thumbnail_click_maps_to_set_active_slide() {
        let (d, sel, sid, _) = fixture();
        let event = InteractionEvent::SlideThumbnailClicked {
            slide_id: "s_b".into(),
        };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::SetActiveSlide(id) => assert_eq!(id, "s_b"),
            other => panic!("expected SetActiveSlide, got {other:?}"),
        }
    }

    #[test]
    fn thumbnail_click_with_empty_slide_id_is_nothing() {
        let (d, sel, sid, _) = fixture();
        let event = InteractionEvent::SlideThumbnailClicked { slide_id: String::new() };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::Nothing => {}
            other => panic!("expected Nothing, got {other:?}"),
        }
    }

    #[test]
    fn build_slide_list_data_emits_every_slide_in_order() {
        let (deck, sid_a, sid_b) = two_slide_deck();
        let data = build_slide_list_data(&deck, Some(&sid_a));
        assert_eq!(data.slides.len(), 2);
        assert_eq!(data.slides[0].slide_id, sid_a);
        assert_eq!(data.slides[1].slide_id, sid_b);
        assert_eq!(data.active_slide_id.as_deref(), Some("s_a"));
        assert_eq!(data.width, deck.manifest.dimensions.width);
        assert_eq!(data.height, deck.manifest.dimensions.height);
        // Each entry carries a non-empty serialized HTML body.
        for entry in &data.slides {
            assert!(entry.html.contains("data-slide-id"));
        }
    }

    #[test]
    fn build_slide_list_data_falls_back_to_id_when_title_empty() {
        let (deck, sid_a, _) = two_slide_deck();
        let data = build_slide_list_data(&deck, Some(&sid_a));
        // No SlideEntry entries in manifest.slides -> title falls back
        // to the slide id verbatim.
        assert_eq!(data.slides[0].title, sid_a);
    }

    // Helper: the slide-switch handler logic without depending on a
    // WebviewSender. Mirrors set_active_slide's tree-state effects so
    // we can exercise the edit-preservation invariant in tests.
    fn switch_active_slide_in_tree(
        dispatcher: &mut CommandDispatcher,
        active_slide: &mut Option<SlideId>,
        target: SlideId,
    ) -> bool {
        if !dispatcher.deck().slides.contains_key(&target) {
            return false;
        }
        if active_slide.as_deref() == Some(target.as_str()) {
            return false;
        }
        let _ = dispatcher.take_patches();
        *active_slide = Some(target);
        true
    }

    #[test]
    fn switching_slides_preserves_in_memory_edits_to_previous_slide() {
        // Reproduce the contract: edit slide A → switch to B → switch
        // back to A → edits are still present.
        let (deck, sid_a, sid_b) = two_slide_deck();
        let mut dispatcher = CommandDispatcher::new(deck);
        let mut active: Option<SlideId> = Some(sid_a.clone());

        // Edit on A.
        let original_x = dispatcher.deck().slides[&sid_a]
            .find_element("el_a").unwrap().geometry.x;
        dispatcher
            .dispatch(Box::new(MoveElement {
                target: CanvasTarget::Slide(sid_a.clone()),
                element_id: "el_a".into(),
                new_position: Point { x: original_x + 250.0, y: 0.0 },
                previous_position: None,
            }))
            .unwrap();

        assert!(switch_active_slide_in_tree(&mut dispatcher, &mut active, sid_b.clone()));
        assert_eq!(active.as_deref(), Some("s_b"));

        // Slide A's mutation must survive the switch.
        let x_after = dispatcher.deck().slides[&sid_a]
            .find_element("el_a").unwrap().geometry.x;
        assert_eq!(x_after, original_x + 250.0);

        // Switch back.
        assert!(switch_active_slide_in_tree(&mut dispatcher, &mut active, sid_a.clone()));
        let x_back = dispatcher.deck().slides[&sid_a]
            .find_element("el_a").unwrap().geometry.x;
        assert_eq!(x_back, original_x + 250.0);
    }

    #[test]
    fn switch_to_unknown_slide_is_rejected() {
        let (deck, sid_a, _) = two_slide_deck();
        let mut dispatcher = CommandDispatcher::new(deck);
        let mut active: Option<SlideId> = Some(sid_a.clone());
        let ok = switch_active_slide_in_tree(&mut dispatcher, &mut active, "ghost".into());
        assert!(!ok);
        assert_eq!(active.as_deref(), Some("s_a"));
    }

    #[test]
    fn switch_to_currently_active_slide_is_no_op() {
        let (deck, sid_a, _) = two_slide_deck();
        let mut dispatcher = CommandDispatcher::new(deck);
        let mut active: Option<SlideId> = Some(sid_a.clone());
        let ok = switch_active_slide_in_tree(&mut dispatcher, &mut active, sid_a);
        assert!(!ok);
    }

    // ---------- Resize handles: interpret lifecycle ----------

    use crate::ipc::{ResizeHandle, Size};

    #[test]
    fn resize_started_opens_transaction_with_geometry_snapshot() {
        let (d, sel, sid, eid) = fixture();
        let event = InteractionEvent::ElementResizeStarted {
            element_id: eid.clone(),
            handle: ResizeHandle::BottomRight,
            position: Point { x: 0.0, y: 0.0 },
        };
        match interpret_inline(&d, &sel, &Some(sid.clone()), event) {
            InterpretResult::TransactionBegin { label, snapshot } => {
                assert_eq!(label, "Resize Element");
                assert!(snapshot.position_of(&CanvasTarget::Slide(sid.clone()), &eid).is_some());
            }
            other => panic!("expected TransactionBegin, got {other:?}"),
        }
    }

    #[test]
    fn resize_mid_drag_is_a_no_op_on_rust_side() {
        let (d, sel, sid, eid) = fixture();
        let event = InteractionEvent::ElementResized {
            element_id: eid,
            handle: ResizeHandle::Right,
            new_size: Size { width: 200.0, height: 100.0 },
            new_position: Point { x: 0.0, y: 0.0 },
        };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::Nothing => {}
            other => panic!("expected Nothing, got {other:?}"),
        }
    }

    #[test]
    fn resize_ended_emits_commit_with_resize_command() {
        let (mut d, sel, sid, eid) = fixture();
        let geo = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();
        let mut snap = TransactionSnapshot::empty();
        snap.record_geometry(CanvasTarget::Slide(sid.clone()), eid.clone(), geo);
        d.begin_transaction("Resize Element", snap);

        let event = InteractionEvent::ElementResizeEnded {
            element_id: eid.clone(),
            new_position: Point { x: 50.0, y: 60.0 },
            new_size: Size { width: 300.0, height: 200.0 },
        };
        match interpret_inline(&d, &sel, &Some(sid.clone()), event) {
            InterpretResult::CommitTransactionWith(cmd) => {
                assert_eq!(cmd.label(), "Resize Element");
                let mut tmp = Deck::sample();
                let out = cmd.apply(&mut tmp).unwrap();
                let g = tmp.slides[&sid].find_element(&eid).unwrap().geometry.clone();
                assert_eq!(g.x, 50.0);
                assert_eq!(g.y, 60.0);
                assert_eq!(g.width, 300.0);
                assert_eq!(g.height, 200.0);
                // Four SetStyle patches (left, top, width, height).
                assert_eq!(out.patches.len(), 4);
            }
            other => panic!("expected CommitTransactionWith, got {other:?}"),
        }
    }

    #[test]
    fn resize_ended_without_transaction_is_nothing() {
        let (d, sel, sid, eid) = fixture();
        let event = InteractionEvent::ElementResizeEnded {
            element_id: eid,
            new_position: Point { x: 0.0, y: 0.0 },
            new_size: Size { width: 1.0, height: 1.0 },
        };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::Nothing => {}
            other => panic!("expected Nothing, got {other:?}"),
        }
    }

    #[test]
    fn resize_lifecycle_round_trip_undo_restores_original_rect() {
        let (mut d, _, sid, eid) = fixture();
        let original = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();

        // Started → snapshot.
        let mut snap = TransactionSnapshot::empty();
        snap.record_geometry(CanvasTarget::Slide(sid.clone()), eid.clone(), original.clone());
        d.begin_transaction("Resize Element", snap);

        // Ended → commit ResizeElement.
        let cmd = ResizeElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_x: original.x + 100.0,
            new_y: original.y + 50.0,
            new_width: original.width - 80.0,
            new_height: original.height + 20.0,
        };
        d.dispatch(Box::new(cmd)).unwrap();
        d.commit_transaction().unwrap();
        let _ = d.take_patches();

        let after = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();
        assert_eq!(after.x, original.x + 100.0);
        assert_eq!(after.width, original.width - 80.0);

        // Single undo restores all four fields.
        d.undo().unwrap().expect("undo not a no-op");
        let restored = d.deck().slides[&sid].find_element(&eid).unwrap().geometry.clone();
        assert_eq!(restored.x, original.x);
        assert_eq!(restored.y, original.y);
        assert_eq!(restored.width, original.width);
        assert_eq!(restored.height, original.height);
    }

    // ---------- Image import + AssetRegistry plumbing ----------

    fn sample_asset_entry() -> crate::bundle::assets::AssetEntry {
        crate::bundle::assets::AssetEntry {
            id: "asset_deadbeef00000000".into(),
            path: "assets/images/asset_deadbeef00000000.png".into(),
            content_hash: "sha256:dead".into(),
            original_filename: "logo.png".into(),
            media_type: "image/png".into(),
            size_bytes: 42,
            dimensions: Some(crate::bundle::assets::AssetDimensions {
                width: 200,
                height: 100,
            }),
        }
    }

    #[test]
    fn build_image_element_uses_natural_size_and_centres_when_no_drop_point() {
        let entry = sample_asset_entry();
        let node = build_image_element_from_asset(&entry, 800, 600, None, (1920, 1080));
        assert_eq!(node.element_type, ElementType::Image);
        assert_eq!(node.geometry.width, 800.0);
        assert_eq!(node.geometry.height, 600.0);
        // Centered: (1920 - 800)/2 = 560 ; (1080 - 600)/2 = 240
        assert_eq!(node.geometry.x, 560.0);
        assert_eq!(node.geometry.y, 240.0);
        // object-fit:cover semantics via background-* shortcuts.
        assert_eq!(
            node.inline_styles.get("background-size").map(String::as_str),
            Some("cover")
        );
        assert_eq!(
            node.inline_styles.get("background-position").map(String::as_str),
            Some("center")
        );
        // The CSS background-image points at a custom property the
        // shadow root will resolve via the slide's theme stylesheet.
        let bg_image = node
            .inline_styles
            .get("background-image")
            .map(String::as_str)
            .unwrap_or("");
        assert!(bg_image.contains(&entry.id));
        // The model side keeps the asset reference too.
        match node.content {
            ElementContent::Image(ref a) => assert_eq!(a.asset_id, entry.id),
            ref other => panic!("expected Image content, got {other:?}"),
        }
    }

    #[test]
    fn build_image_element_centers_around_drop_point_when_provided() {
        let entry = sample_asset_entry();
        let drop = Some(Point { x: 1000.0, y: 500.0 });
        let node = build_image_element_from_asset(&entry, 400, 200, drop, (1920, 1080));
        // The element is sized natural and positioned so its centre
        // lands on the drop point.
        assert_eq!(node.geometry.x, 1000.0 - 200.0);
        assert_eq!(node.geometry.y, 500.0 - 100.0);
        assert_eq!(node.geometry.width, 400.0);
        assert_eq!(node.geometry.height, 200.0);
    }

    #[test]
    fn build_image_element_falls_back_to_default_size_when_dimensions_unknown() {
        let entry = sample_asset_entry();
        let node = build_image_element_from_asset(&entry, 0, 0, None, (1920, 1080));
        assert_eq!(node.geometry.width, 320.0);
        assert_eq!(node.geometry.height, 180.0);
    }

    #[test]
    fn asset_registry_insert_blob_increases_count_and_serializes_via_deck_io() {
        // Add an asset to a sample deck, serialize, deserialize, confirm
        // the asset bytes survive the bundle round trip.
        use crate::bundle::deck_io::{deserialize_deck, serialize_deck};

        let mut deck = Deck::sample();
        let bytes = b"hello-world-as-image".to_vec();
        let before_count = deck.assets.entry_count();
        deck.assets.insert_blob(
            bytes.clone(),
            "x.png".into(),
            "image/png".into(),
            Some(crate::bundle::assets::AssetDimensions { width: 10, height: 10 }),
        );
        assert_eq!(deck.assets.entry_count(), before_count + 1);

        let serialized = serialize_deck(&deck).unwrap();
        assert!(!serialized.assets_index_json.is_empty());
        assert!(!serialized.asset_files.is_empty());

        let back = deserialize_deck(serialized).unwrap();
        assert_eq!(back.assets.entry_count(), before_count + 1);
        // The bytes are present under the same path the registry
        // assigned, so round-tripping the deck preserves images.
        let entry = back.assets.assets.last().unwrap().clone();
        assert_eq!(back.assets.files.get(&entry.path), Some(&bytes));
    }

    #[test]
    fn build_insert_slide_after_active_inserts_after_the_active_slide() {
        // Order: [orig, s_b]. Adding after s_b must land at index 2,
        // i.e. append; adding after orig must land at index 1.
        let mut deck = Deck::sample();
        let orig: SlideId = deck.slide_order[0].clone();
        InsertSlide {
            position: 1,
            slide: SlideNode::new(
                "s_b".into(),
                "blank".into(),
                crate::deck::builders::group_element("rt_b", vec![]),
            ),
            manifest_entry: crate::bundle::SlideEntry {
                id: "s_b".into(),
                path: crate::bundle::manifest::slide_path_for("s_b"),
                layout_id: "blank".into(),
                title: String::new(),
                thumbnail: None,
                transition: None,
                duration_hint: None,
                notes_ref: None,
                animations: Vec::new(),
            },
        }
        .apply(&mut deck)
        .unwrap();
        let dispatcher = CommandDispatcher::new(deck);

        let (cmd, new_id) =
            build_insert_slide_after_active(&dispatcher, Some(&orig)).unwrap();
        assert!(!new_id.is_empty());
        assert_eq!(cmd.label(), "Add Slide");
        assert!(cmd.affects_slide_list());

        // Applying it on a clone of the deck must place the new slide at
        // index 1 (directly after `orig`), ahead of s_b.
        let mut deck2 = dispatcher.deck().clone();
        cmd.apply(&mut deck2).unwrap();
        assert_eq!(deck2.slide_order[1], new_id);
        assert_eq!(deck2.slide_order[2], "s_b");
        assert!(deck2.slides.contains_key(&new_id));
        assert!(deck2.manifest.slides.iter().any(|e| e.id == new_id));
    }

    #[test]
    fn build_insert_slide_after_active_appends_when_no_active_slide() {
        let deck = Deck::sample();
        let len_before: usize = deck.slide_order.len();
        let dispatcher = CommandDispatcher::new(deck);

        let (cmd, new_id) = build_insert_slide_after_active(&dispatcher, None).unwrap();
        let mut deck2 = dispatcher.deck().clone();
        cmd.apply(&mut deck2).unwrap();
        assert_eq!(deck2.slide_order.len(), len_before + 1);
        assert_eq!(deck2.slide_order.last().cloned(), Some(new_id));
    }

    #[test]
    fn build_insert_slide_after_active_makes_a_blank_layout_slide() {
        let deck = Deck::sample();
        let active: SlideId = deck.slide_order[0].clone();
        let dispatcher = CommandDispatcher::new(deck);
        let (cmd, new_id) =
            build_insert_slide_after_active(&dispatcher, Some(&active)).unwrap();

        let mut deck2 = dispatcher.deck().clone();
        cmd.apply(&mut deck2).unwrap();
        let slide = deck2.slides.get(&new_id).unwrap();
        assert_eq!(slide.layout_id, "blank");
        // A brand-new slide carries an empty root group (no elements yet).
        assert!(slide.root.children.is_empty());
    }

    #[test]
    fn build_set_text_command_some_on_changed_text() {
        let (dispatcher, _sel, sid, eid) = fixture();
        let out = build_set_text_command(&dispatcher, Some(CanvasTarget::Slide(sid.clone())), eid, "brand new text".into());
        let cmd = out.expect("changed text should produce a command");
        assert_eq!(cmd.label(), "Edit Text");
    }

    #[test]
    fn build_set_text_command_none_when_text_unchanged() {
        let (dispatcher, _sel, sid, eid) = fixture();
        let current: String =
            match &dispatcher.deck().slides[&sid].find_element(&eid).unwrap().content {
                ElementContent::Text(rt) => rt.plain.clone(),
                other => panic!("expected text, got {other:?}"),
            };
        // Re-committing the identical text must not create a history entry.
        assert!(build_set_text_command(&dispatcher, Some(CanvasTarget::Slide(sid.clone())), eid, current).is_none());
    }

    #[test]
    fn build_set_text_command_none_without_active_slide() {
        let (dispatcher, _sel, _sid, eid) = fixture();
        assert!(build_set_text_command(&dispatcher, None, eid, "x".into()).is_none());
    }

    #[test]
    fn build_set_text_command_none_on_non_text_element() {
        // The slide root is a Group, not a Text element.
        let (dispatcher, _sel, sid, _eid) = fixture();
        let root_id: ElementId = dispatcher.deck().slides[&sid].root.id.clone();
        assert!(build_set_text_command(&dispatcher, Some(CanvasTarget::Slide(sid.clone())), root_id, "x".into()).is_none());
    }

    #[test]
    fn sanitize_element_id_collapses_whitespace_runs() {
        assert_eq!(sanitize_element_id("my  box"), "my_box");
        assert_eq!(sanitize_element_id("a\tb\nc"), "a_b_c");
        assert_eq!(sanitize_element_id("  lead trail  "), "lead_trail");
        assert_eq!(sanitize_element_id("nospace"), "nospace");
        assert_eq!(sanitize_element_id("   "), "");
    }

    #[test]
    fn build_set_slide_title_some_on_change_none_on_same() {
        let (dispatcher, _sel, sid, _eid) = fixture();
        assert!(build_set_slide_title_command(&dispatcher, &sid, "New Title").is_some());
        let current: String = dispatcher
            .deck()
            .manifest
            .slides
            .iter()
            .find(|e| e.id == sid)
            .unwrap()
            .title
            .clone();
        assert!(build_set_slide_title_command(&dispatcher, &sid, &current).is_none());
    }

    #[test]
    fn build_set_slide_title_none_on_unknown_slide() {
        let (dispatcher, _sel, _sid, _eid) = fixture();
        let ghost: SlideId = "ghost".into();
        assert!(build_set_slide_title_command(&dispatcher, &ghost, "x").is_none());
    }

    // ---------- Stage 11: layout editor helpers ----------

    #[test]
    fn build_insert_layout_after_active_creates_a_unique_layout() {
        let (mut dispatcher, _sel, _sid, _eid) = fixture();
        // Deck::sample's theme seeds the "blank" layout.
        let active: Option<LayoutId> = Some("blank".into());
        let (cmd, new_id) =
            build_insert_layout_after_active(&dispatcher, active.as_ref()).unwrap();
        assert_ne!(new_id, "blank");
        assert!(!dispatcher.deck().theme.layouts.contains_key(&new_id));
        cmd.apply(dispatcher.deck_mut()).unwrap();
        assert!(dispatcher.deck().theme.layouts.contains_key(&new_id));
        // Inserted directly after the active layout.
        let pos = dispatcher.deck().theme.layout_order.iter().position(|l| l == &new_id);
        assert_eq!(pos, Some(1));
    }

    #[test]
    fn build_layout_list_data_emits_layouts_in_order_with_globals() {
        let (mut dispatcher, _sel, _sid, _eid) = fixture();
        dispatcher.deck_mut().theme.globals_css = ":root{--g:1}".into();
        let data = build_layout_list_data(dispatcher.deck(), Some(&"blank".to_string()));
        assert_eq!(data.layouts.len(), dispatcher.deck().theme.layout_order.len());
        assert_eq!(data.layouts[0].layout_id, "blank");
        assert_eq!(data.layouts[0].name, "Blank");
        assert!(!data.layouts[0].html.is_empty());
        assert_eq!(data.active_layout_id.as_deref(), Some("blank"));
        assert_eq!(data.globals_css, ":root{--g:1}");
    }

    #[test]
    fn property_changed_targets_the_active_layout_in_layout_mode() {
        let (mut dispatcher, _sel, sid, _eid) = fixture();
        // Add an element to the blank layout to edit.
        dispatcher
            .deck_mut()
            .theme
            .layouts
            .get_mut("blank")
            .unwrap()
            .root
            .children
            .push(crate::deck::builders::text_element("el_lt", "hi"));

        let result = interpret_property_changed(
            Some(CanvasTarget::Layout("blank".into())),
            "el_lt".into(),
            "width".into(),
            "321".into(),
        );
        let cmd = match result {
            InterpretResult::Command(c) => c,
            other => panic!("expected Command, got {other:?}"),
        };
        cmd.apply(dispatcher.deck_mut()).unwrap();
        // The layout element changed; no slide was touched.
        assert_eq!(
            dispatcher.deck().theme.layouts["blank"].find_element("el_lt").unwrap().geometry.width,
            321.0
        );
        let slide_root_children = dispatcher.deck().slides[&sid].root.children.len();
        assert!(slide_root_children > 0);
    }

    // ---------- Stage: animations interpret path ----------

    #[test]
    fn set_element_animation_enable_builds_insert() {
        let (mut dispatcher, _sel, sid, eid) = fixture();
        let result = interpret_set_element_animation(
            dispatcher.deck(), EditorMode::Slide, Some(&sid), eid.clone(), "entrance", true);
        let cmd = match result {
            InterpretResult::Command(c) => c,
            other => panic!("expected Command, got {other:?}"),
        };
        cmd.apply(dispatcher.deck_mut()).unwrap();
        let t = &dispatcher.deck().slides[&sid].animations;
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].category, AnimationCategory::Entrance);
        assert_eq!(t[0].keyframe, "appear");
        assert_eq!(t[0].element_id, eid);
    }

    #[test]
    fn set_element_animation_disable_removes_existing() {
        let (mut dispatcher, _sel, sid, eid) = fixture();
        if let InterpretResult::Command(c) = interpret_set_element_animation(
            dispatcher.deck(), EditorMode::Slide, Some(&sid), eid.clone(), "exit", true) {
            c.apply(dispatcher.deck_mut()).unwrap();
        }
        assert_eq!(dispatcher.deck().slides[&sid].animations.len(), 1);
        let result = interpret_set_element_animation(
            dispatcher.deck(), EditorMode::Slide, Some(&sid), eid, "exit", false);
        match result {
            InterpretResult::Command(c) => { c.apply(dispatcher.deck_mut()).unwrap(); }
            other => panic!("expected Command, got {other:?}"),
        }
        assert!(dispatcher.deck().slides[&sid].animations.is_empty());
    }

    #[test]
    fn set_element_animation_noop_in_layout_mode() {
        let (dispatcher, _sel, sid, eid) = fixture();
        let result = interpret_set_element_animation(
            dispatcher.deck(), EditorMode::Layout, Some(&sid), eid, "entrance", true);
        assert!(matches!(result, InterpretResult::Nothing));
    }

    // ---------- Presentation mode ----------

    #[test]
    fn present_start_index_uses_active_slide_position() {
        let mut deck = Deck::sample();
        // Add a second slide so the active one is not trivially index 0.
        let root = crate::deck::builders::group_element("el_root", vec![]);
        let s2 = SlideNode::new("slide_two".into(), "blank".into(), root);
        deck.slides.insert("slide_two".into(), s2);
        deck.slide_order.push("slide_two".into());
        let active: Option<SlideId> = Some("slide_two".into());
        assert_eq!(present_start_index(&deck, active.as_ref()), Some(1));
    }

    #[test]
    fn build_assets_bundle_encodes_each_registered_asset() {
        let mut deck = Deck::sample();
        let entry = deck.assets.insert_blob(
            vec![1, 2, 3, 4],
            "logo.png".into(),
            "image/png".into(),
            None,
        );
        let bundle = build_assets_bundle(&deck).expect("non-empty registry yields a bundle");
        assert_eq!(bundle.assets.len(), 1);
        assert_eq!(bundle.assets[0].asset_id, entry.id);
        assert_eq!(bundle.assets[0].media_type, "image/png");
        // base64 of [1,2,3,4] is "AQIDBA==".
        assert_eq!(bundle.assets[0].content_base64, "AQIDBA==");
    }

    #[test]
    fn build_assets_bundle_is_none_when_no_assets() {
        let deck = Deck::sample();
        assert!(build_assets_bundle(&deck).is_none());
    }

    #[test]
    fn present_start_index_falls_back_to_zero_then_none() {
        let deck = Deck::sample();
        // Unknown active id → first slide.
        assert_eq!(present_start_index(&deck, Some(&"ghost".to_string())), Some(0));
        // Empty deck → no presentation possible.
        let empty = Deck::default();
        assert_eq!(present_start_index(&empty, None), None);
    }
}
