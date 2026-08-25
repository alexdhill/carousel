#![allow(dead_code, unused_imports)]

use crate::bundle::assets::{AssetDimensions, AssetEntry};
use crate::bundle::{
    AssetRegistry, IoRequest, IoResponse, IoThread, deserialize_deck, deserialize_theme,
    serialize_deck, serialize_theme,
};
use crate::commands::{
    AlignAxis, AlignElements, AlignOp, Command, CommandDispatcher, CompositeCommand,
    DeleteTableColumn, DeleteTableRow, DistributeAxis, EditorMode, ElementTransform, FileAction,
    GeometryProperty, GroupElements, InsertAnimation, InsertElement, InsertLayout, InsertSlide,
    InsertTableColumn, InsertTableRow, InterpretResult, MoveElement, RemoveAnimation,
    RemoveElementCommand, RemoveInlineStyle, RemoveSlide, RenameElement, ReorderSlide,
    ReparentElement, ReplaceSlideContent, ResizeElement, SetAnimationProperty, SetCellStyles,
    SetCellText, SetDeckTitle, SetElementId, SetElementsTransform, SetEmbedHtml,
    SetGeometryProperty, SetGlobalsCss, SetGroupLayout, SetGroupScale, SetInlineStyle,
    SetLayoutBackground, SetLayoutBackgroundImage, SetLayoutName, SetMorphTransition,
    SetSlideBackground, SetSlideBackgroundImage, SetSlideLayout, SetSlideNotes, SetSlideTitle,
    SetSlideTransition, SetTableHeaderColumns, SetTableHeaderRows, SetTextContent, SwapTheme,
    TransactionSnapshot,
};
use crate::deck::animation::{
    AnimationCategory, AnimationEffect, AnimationEntry, AnimationTiming, AnimationTrigger,
    PropertyTarget,
};
use crate::deck::element::{
    AssetRef, ElementContent, ElementNode, ElementStyle, ElementType, RichText,
};
use crate::deck::ids::{new_animation_id, new_element_id};
use crate::deck::layout::LayoutNode;
use crate::deck::slide::SlideNode;
use crate::deck::style::{ColorRef, FontRef, Geometry, ImageStyle, Length, ShapeStyle, TextStyle};
use crate::deck::{Canvas, CanvasTarget, Deck, ElementId, LayoutId, ShapeGeometry, SlideId};
use crate::error::{AppError, AppResult};
use crate::html::serialize::{ANIMATION_KEYFRAMES_CSS, serialize_slide, serialize_slide_themed};
use crate::ipc::agent::{AgentPanelState, AgentPermissionAsk, AgentStreamChunk, AgentToolNotice};
use crate::ipc::bridge::WebviewSender;
use crate::ipc::present::{PresentInbound, PresentInitPayload};
use crate::ipc::{
    AssetPayload, AssetsBundle, EditorConfig, GuideDto, GuidesData, InteractionEvent, IpcMessage,
    LayoutListData, LayoutListEntry, MessageKind, MountSlideArgs, ObjectTreeData, ObjectTreeNode,
    Patch, Point, SelectionState, Size, SlideAnimationEntry, SlideAnimationsData,
    SlideInspectorData, SlideInspectorLayout, SlideListData, SlideListEntry,
};
use crate::present::session::{PresentStep, PresentationSession};
use base64::Engine;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use tracing::{debug, info, warn};

const DEBUG_KEY: &str = "d";
const DEBUG_NUDGE_PX: f64 = 50.0;
const DRAG_TRANSACTION_LABEL: &str = "Move Element";
const RESIZE_TRANSACTION_LABEL: &str = "Resize Element";
const CROP_TRANSACTION_LABEL: &str = "Crop Image";
const PASTE_LABEL: &str = "Paste";
const CUT_LABEL: &str = "Cut";

enum Clipboard {
    Elements(Vec<ElementNode>),
    Slide(Box<SlideNode>),
}

struct AssetImport {
    content_base64: String,
    original_filename: String,
    media_type: String,
    width: u32,
    height: u32,
    position: Option<Point>,
    as_slide_background: bool,
    as_element_fill: Option<String>,
}

enum PasteOutcome {
    Elements(Vec<ElementId>),
    Slide(SlideId),
}

const UNDO_KEY: &str = "undo";
const REDO_KEY: &str = "redo";
const NEW_KEY: &str = "new_deck";
const OPEN_KEY: &str = "open_deck";
const SAVE_KEY: &str = "save_deck";
const SAVE_AS_KEY: &str = "save_as_deck";
const EXPORT_HTML_KEY: &str = "export_html";
const EXPORT_PDF_KEY: &str = "export_pdf";

const PRESENT_KEY: &str = "present";
const PRESENT_WINDOWED_KEY: &str = "present_windowed";
const BUNDLE_FILE_EXTENSION: &str = "deck";
const THEME_FILE_EXTENSION: &str = "slidetheme";

const DELETE_KEY_BACKSPACE: &str = "Backspace";
const DELETE_KEY_DELETE: &str = "Delete";

pub struct PdfJob {
    pub html: String,
    pub raster: Vec<crate::export::pdf::PageRect>,
    pub chrome: PathBuf,
    pub dest: PathBuf,
}

#[derive(Debug, Clone, Copy)]
enum HistoryStep {
    Undo,
    Redo,
}

pub struct ApplicationCore {
    dispatcher: CommandDispatcher,
    active_slide: Option<SlideId>,

    active_layout: Option<LayoutId>,
    selection: SelectionState,
    sender: WebviewSender,
    schedule_flush: Box<dyn Fn()>,
    io_thread: IoThread,

    present: Option<PresentationSession>,

    presenter: Option<WebviewSender>,
    request_present_open: Box<dyn Fn(bool)>,
    request_present_close: Box<dyn Fn()>,

    pending_present_index: Option<usize>,

    pending_asset_broadcast: Option<String>,

    pending_new_active_slide: Option<SlideId>,

    pending_new_active_layout: Option<LayoutId>,

    clipboard: Option<Clipboard>,

    pending_paste_selection: Option<Vec<ElementId>>,

    dispatch_pdf_job: Box<dyn Fn(PdfJob)>,

    dispatch_chromium_download: Box<dyn Fn()>,

    pending_export_after_chrome: bool,

    font_families: Option<Vec<String>>,

    focus_title: bool,

    pending_quit: bool,

    quit_requested: bool,

    agent: Option<crate::agent::acp::AgentHandle>,

    agent_name: Option<String>,

    agent_sink: std::sync::Arc<dyn Fn(crate::agent::AgentEvent) + Send + Sync>,

    agent_pending: std::collections::HashMap<String, (String, String)>,

    agent_workspace: Option<crate::agent::workspace::Workspace>,
}

impl ApplicationCore {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sender: WebviewSender,
        schedule_flush: Box<dyn Fn()>,
        io_thread: IoThread,
        request_present_open: Box<dyn Fn(bool)>,
        request_present_close: Box<dyn Fn()>,
        dispatch_pdf_job: Box<dyn Fn(PdfJob)>,
        dispatch_chromium_download: Box<dyn Fn()>,
        agent_sink: std::sync::Arc<dyn Fn(crate::agent::AgentEvent) + Send + Sync>,
    ) -> Self {
        Self::new_with_deck(
            Deck::sample(),
            sender,
            schedule_flush,
            io_thread,
            request_present_open,
            request_present_close,
            dispatch_pdf_job,
            dispatch_chromium_download,
            agent_sink,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_deck(
        deck: Deck,
        sender: WebviewSender,
        schedule_flush: Box<dyn Fn()>,
        io_thread: IoThread,
        request_present_open: Box<dyn Fn(bool)>,
        request_present_close: Box<dyn Fn()>,
        dispatch_pdf_job: Box<dyn Fn(PdfJob)>,
        dispatch_chromium_download: Box<dyn Fn()>,
        agent_sink: std::sync::Arc<dyn Fn(crate::agent::AgentEvent) + Send + Sync>,
        focus_title: bool,
    ) -> Self {
        let active_slide: Option<SlideId> = deck.slide_order.first().cloned();
        let active_layout: Option<LayoutId> = deck.theme.layout_order.first().cloned();
        assert!(active_slide.is_some(), "deck must contain a slide");
        Self {
            dispatcher: CommandDispatcher::new(deck),
            active_slide,
            active_layout,
            selection: SelectionState::empty(),
            sender,
            schedule_flush,
            io_thread,
            present: None,
            presenter: None,
            request_present_open,
            request_present_close,
            pending_present_index: None,
            pending_asset_broadcast: None,
            pending_new_active_slide: None,
            pending_new_active_layout: None,
            clipboard: None,
            pending_paste_selection: None,
            dispatch_pdf_job,
            dispatch_chromium_download,
            pending_export_after_chrome: false,
            font_families: None,
            focus_title,
            pending_quit: false,
            quit_requested: false,
            agent: None,
            agent_name: None,
            agent_sink,
            agent_pending: HashMap::new(),
            agent_workspace: None,
        }
    }

    // ponytail: font enumeration is synchronous on the main thread, cached for the
    // session; move to a worker delivering via EventLoopProxy if it lags first paint.
    fn send_font_list(&mut self) -> AppResult<()> {
        if self.font_families.is_none() {
            self.font_families = Some(crate::fonts::enumerate_families());
        }
        let families: Vec<String> = self.font_families.clone().unwrap_or_default();
        self.sender.send(MessageKind::FontList { families })
    }

    fn active_canvas(&self) -> Option<CanvasTarget> {
        match self.dispatcher.mode() {
            EditorMode::Slide => self.active_slide.clone().map(CanvasTarget::Slide),
            EditorMode::Layout => self.active_layout.clone().map(CanvasTarget::Layout),
        }
    }

    fn active_canvas_id(&self) -> Option<String> {
        self.active_canvas().map(|t| t.id().to_string())
    }

    pub fn selection(&self) -> &SelectionState {
        &self.selection
    }

    pub fn active_slide(&self) -> Option<&SlideId> {
        self.active_slide.as_ref()
    }

    pub fn handle_ipc(&mut self, msg: IpcMessage) -> AppResult<()> {
        assert!(!msg.id.is_empty(), "ipc message missing id");
        debug!(id = %msg.id, "ipc <- webview");
        match msg.kind {
            MessageKind::Ready => {
                self.sender.send(MessageKind::Configure(EditorConfig {
                    debug: false,
                    animation_keyframes_css: ANIMATION_KEYFRAMES_CSS.to_string(),
                    animation_catalog: crate::deck::anim_catalog::animation_catalog(),
                    deck_title: self.dispatcher.deck().manifest.metadata.title.clone(),
                    focus_title: self.focus_title,
                }))?;
                self.send_slide_list()?;
                self.send_assets_bundle()?;
                self.send_font_list()?;
                self.send_active_slide()?;
                self.send_slide_animations()?;
                self.send_guides()?;
                self.send_save_state()
            }
            MessageKind::Interaction(event) => self.handle_interaction(event),
            MessageKind::AgentPanelToggled { open } => self.on_agent_panel_toggled(open),
            MessageKind::AgentPromptSubmitted { text, agent } => self.on_agent_prompt(text, agent),
            MessageKind::AgentCancelRequested => self.on_agent_cancel(),
            MessageKind::AgentAddRequested {
                name,
                command,
                args,
            } => self.on_agent_add(name, command, args),
            MessageKind::AgentPermissionReply { request_id, allow } => {
                self.on_agent_permission_reply(request_id, allow)
            }
            other => {
                warn!(
                    "unhandled message kind: {:?}",
                    std::mem::discriminant(&other)
                );
                Ok(())
            }
        }
    }

    pub fn flush_patches(&mut self) -> AppResult<()> {
        let patches: Vec<Patch> = self.dispatcher.take_patches();
        if patches.is_empty() {
            return Ok(());
        }
        let payload: Patch = if patches.len() == 1 {
            patches
                .into_iter()
                .next()
                .unwrap_or(Patch::Batch { patches: vec![] })
        } else {
            Patch::Batch { patches }
        };
        debug!("flushing patches");
        self.sender.send(MessageKind::ApplyPatch(payload))
    }

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
        self.sender.send(MessageKind::ObjectTreeUpdate(tree))?;

        self.send_slide_inspector()
    }

    fn interpret_nudge(&self, dx: f64, dy: f64) -> InterpretResult {
        let target: CanvasTarget = match self.active_canvas() {
            Some(t) => t,
            None => return InterpretResult::Nothing,
        };
        let canvas = match self.dispatcher.deck().canvas(&target) {
            Some(c) => c,
            None => return InterpretResult::Nothing,
        };
        let mut cmds: Vec<Box<dyn Command>> = Vec::new();
        for id in &self.selection.element_ids {
            if let Some(el) = canvas.find_element(id) {
                cmds.push(Box::new(MoveElement {
                    target: target.clone(),
                    element_id: id.clone(),
                    new_position: Point {
                        x: el.geometry.x + dx,
                        y: el.geometry.y + dy,
                    },
                    previous_position: None,
                }));
            }
        }
        if cmds.is_empty() {
            return InterpretResult::Nothing;
        }

        InterpretResult::Command(Box::new(CompositeCommand::new(cmds, "Nudge Elements")))
    }

    fn interpret_navigate_slide(&self, forward: bool) -> InterpretResult {
        if self.dispatcher.mode() != EditorMode::Slide {
            return InterpretResult::Nothing;
        }
        let order: &[SlideId] = &self.dispatcher.deck().slide_order;
        let cur: usize = match self
            .active_slide
            .as_ref()
            .and_then(|sid| order.iter().position(|s| s == sid))
        {
            Some(i) => i,
            None => return InterpretResult::Nothing,
        };
        let next: usize = if forward {
            cur + 1
        } else {
            cur.wrapping_sub(1)
        };
        match order.get(next) {
            Some(sid) if forward || cur > 0 => InterpretResult::SetActiveSlide(sid.clone()),
            _ => InterpretResult::Nothing,
        }
    }

    fn send_slide_inspector(&self) -> AppResult<()> {
        if self.dispatcher.mode() != EditorMode::Slide {
            return Ok(());
        }
        let data: SlideInspectorData =
            match build_slide_inspector_data(self.dispatcher.deck(), self.active_slide.as_ref()) {
                Some(d) => d,
                None => return Ok(()),
            };
        self.sender.send(MessageKind::SlideInspectorUpdate(data))
    }

    fn canvas_mount_artifacts(
        &self,
        target: &CanvasTarget,
    ) -> Option<(String, String, ObjectTreeData)> {
        match target {
            CanvasTarget::Slide(id) => {
                let deck = self.dispatcher.deck();
                let slide = deck.slides.get(id)?;
                let (fill, img) = deck.effective_slide_bg(slide);
                let number: usize = deck
                    .slide_order
                    .iter()
                    .position(|s| s == id)
                    .map(|p| p + 1)
                    .unwrap_or(1);
                let opts: crate::html::serialize::RenderOpts = crate::html::serialize::RenderOpts {
                    ctx: Some(crate::html::serialize::RenderCtx {
                        number,
                        count: deck.slide_order.len(),
                        date: crate::html::serialize::today_ymd(),
                    }),
                    hide_placeholders: false,
                    min_element_size: 0.0,
                };
                Some((
                    id.clone(),
                    serialize_slide_themed(slide, fill.as_deref(), img.as_deref(), &opts),
                    build_object_tree(slide),
                ))
            }
            CanvasTarget::Layout(id) => {
                let layout = self.dispatcher.deck().theme.layouts.get(id)?;
                let transient: SlideNode = layout.preview_slide();
                Some((
                    id.clone(),
                    serialize_slide(&transient),
                    build_object_tree(&transient),
                ))
            }
        }
    }

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

    fn send_save_state(&self) -> AppResult<()> {
        let dirty: bool = self.dispatcher.deck().has_unsaved_changes();
        self.sender.send(MessageKind::SaveStateUpdate(dirty))
    }

    pub fn wants_quit_confirmation(&self) -> bool {
        self.dispatcher.deck().has_unsaved_changes()
    }

    pub fn show_quit_dialog(&self) -> AppResult<()> {
        self.sender.send(MessageKind::ShowQuitDialog)
    }

    pub fn take_quit_requested(&mut self) -> bool {
        let requested: bool = self.quit_requested;
        self.quit_requested = false;
        requested
    }

    fn send_assets_bundle(&self) -> AppResult<()> {
        let bundle: AssetsBundle = match build_assets_bundle(self.dispatcher.deck()) {
            Some(b) => b,
            None => return Ok(()),
        };
        debug!(count = bundle.assets.len(), "ipc -> AssetsUpdate");
        self.sender.send(MessageKind::AssetsUpdate(bundle))
    }

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
            original_filename: entry.original_filename.clone(),
        }))
    }

    fn send_slide_list(&self) -> AppResult<()> {
        let data: SlideListData =
            build_slide_list_data(self.dispatcher.deck(), self.active_slide.as_ref());
        debug!(
            slide_count = data.slides.len(),
            active = ?data.active_slide_id,
            "ipc -> SlideListUpdate"
        );
        self.sender.send(MessageKind::SlideListUpdate(data))
    }

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
                    AnimationCategory::Property => "property",
                }
                .to_string(),
                effect_id: e.effect.keyframe_name().unwrap_or("property").to_string(),
                keyframe: e.effect.keyframe_name().map(str::to_string),
                targets: e.effect.targets().map(<[_]>::to_vec).unwrap_or_default(),
                trigger: match e.trigger {
                    AnimationTrigger::OnClick => "on_click",
                    AnimationTrigger::WithPrevious => "with_previous",
                    AnimationTrigger::AfterPrevious => "after_previous",
                }
                .to_string(),
                duration_ms: e.timing.duration_ms,
                delay_ms: e.timing.delay_ms,
                easing: e.timing.easing.clone(),
                iterations: e.timing.iterations,
            })
            .collect();
        self.sender
            .send(MessageKind::SlideAnimationsUpdate(SlideAnimationsData {
                slide_id: sid,
                entries,
            }))
    }

    fn send_guides(&self) -> AppResult<()> {
        let target: CanvasTarget = match self.active_canvas() {
            Some(t) => t,
            None => return Ok(()),
        };
        let deck = self.dispatcher.deck();
        let own: Vec<GuideDto> = match deck.canvas(&target) {
            Some(c) => c.guides().iter().map(guide_to_dto).collect(),
            None => return Ok(()),
        };
        let inherited: Vec<GuideDto> = match &target {
            CanvasTarget::Slide(id) => match deck.slides.get(id) {
                Some(s) => deck.inherited_guides(s).iter().map(guide_to_dto).collect(),
                None => Vec::new(),
            },
            CanvasTarget::Layout(_) => Vec::new(),
        };
        self.sender.send(MessageKind::GuidesUpdate(GuidesData {
            canvas_id: target.id().to_string(),
            own,
            inherited,
        }))
    }

    fn set_active_slide(&mut self, slide_id: SlideId) -> AppResult<()> {
        if slide_id.is_empty() {
            return Ok(());
        }
        if !self.dispatcher.deck().slides.contains_key(&slide_id) {
            warn!(target = %slide_id, "set_active_slide: unknown slide id");
            return Ok(());
        }
        if self.active_slide.as_deref() == Some(slide_id.as_str()) {
            if !self.selection.is_empty() {
                self.selection = SelectionState::empty();
                self.sender
                    .send(MessageKind::SetSelection(SelectionState::empty()))?;
            }
            return Ok(());
        }
        info!(target = %slide_id, "switching active slide");

        self.flush_patches()?;

        self.active_slide = Some(slide_id);
        self.selection = SelectionState::empty();

        self.sender
            .send(MessageKind::SetSelection(SelectionState::empty()))?;
        self.send_active_slide()?;
        self.send_guides()
    }

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
        self.sender.send(MessageKind::SetMode {
            mode: mode_str.to_string(),
        })?;
        self.sender
            .send(MessageKind::SetSelection(SelectionState::empty()))?;
        match mode {
            EditorMode::Slide => self.send_slide_list()?,
            EditorMode::Layout => self.send_layout_list()?,
        }
        self.send_active_slide()?;
        self.send_guides()
    }

    fn set_active_layout(&mut self, layout_id: LayoutId) -> AppResult<()> {
        if layout_id.is_empty() {
            return Ok(());
        }
        if !self
            .dispatcher
            .deck()
            .theme
            .layouts
            .contains_key(&layout_id)
        {
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

        self.send_layout_list()?;
        self.send_active_slide()?;
        self.send_guides()
    }

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

    fn send_slide_layout_picker(&self) -> AppResult<()> {
        let data: LayoutListData =
            build_layout_list_data(self.dispatcher.deck(), self.active_layout.as_ref());
        debug!(
            layout_count = data.layouts.len(),
            "ipc -> SlideLayoutPickerData"
        );
        self.sender.send(MessageKind::SlideLayoutPickerData(data))
    }

    pub fn interpret(&mut self, event: InteractionEvent) -> InterpretResult {
        match event {
            InteractionEvent::ElementClicked {
                element_id,
                modifiers,
                ..
            } => {
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
            InteractionEvent::ElementsDragEnded { element_ids, delta } => {
                let target: CanvasTarget = match self.active_canvas() {
                    Some(t) => t,
                    None => return InterpretResult::Nothing,
                };
                let mut cmds: Vec<Box<dyn Command>> = Vec::new();
                if let Some(tx) = self.dispatcher.transaction() {
                    for id in &element_ids {
                        if let Some((sx, sy)) = tx.start_snapshot.position_of(&target, id) {
                            cmds.push(Box::new(MoveElement {
                                target: target.clone(),
                                element_id: id.clone(),
                                new_position: Point {
                                    x: sx + delta.x,
                                    y: sy + delta.y,
                                },
                                previous_position: None,
                            }));
                        }
                    }
                }
                if cmds.is_empty() {
                    return InterpretResult::Nothing;
                }
                InterpretResult::CommitTransactionWith(Box::new(CompositeCommand::new(
                    cmds,
                    "Move Elements",
                )))
            }
            InteractionEvent::ScaleElements {
                element_ids,
                factor,
                anchor,
            } => interpret_scale_elements(
                self.dispatcher.deck(),
                self.active_canvas(),
                &element_ids,
                factor,
                anchor,
            ),
            InteractionEvent::ElementResizeStarted { element_id, .. } => {
                let snapshot: TransactionSnapshot = self.snapshot_for_drag(&element_id);
                InterpretResult::TransactionBegin {
                    label: RESIZE_TRANSACTION_LABEL,
                    snapshot,
                }
            }

            InteractionEvent::ElementResized { .. } => InterpretResult::Nothing,
            InteractionEvent::ElementResizeEnded {
                element_id,
                new_position,
                new_size,
                background_size,
                background_position,
            } => {
                let target: CanvasTarget = match self.active_canvas() {
                    Some(t) => t,
                    None => return InterpretResult::Nothing,
                };

                if self
                    .dispatcher
                    .transaction()
                    .and_then(|t| t.start_snapshot.position_of(&target, &element_id))
                    .is_none()
                {
                    return InterpretResult::Nothing;
                }
                InterpretResult::CommitTransactionWith(resize_commit_command(
                    target,
                    element_id,
                    new_position,
                    new_size,
                    background_size,
                    background_position,
                ))
            }
            InteractionEvent::ElementCropCommitted {
                element_id,
                new_position,
                new_size,
                background_size,
                background_position,
            } => interpret_crop_committed(
                self.active_canvas(),
                element_id,
                new_position,
                new_size,
                background_size,
                background_position,
            ),
            InteractionEvent::CopyRequested { scope } => {
                self.clipboard = collect_copy(
                    scope,
                    self.active_canvas(),
                    &self.selection,
                    self.active_slide.as_ref(),
                    self.dispatcher.deck(),
                );
                InterpretResult::Nothing
            }
            InteractionEvent::CutRequested { scope } => {
                self.clipboard = collect_copy(
                    scope,
                    self.active_canvas(),
                    &self.selection,
                    self.active_slide.as_ref(),
                    self.dispatcher.deck(),
                );
                match build_cut_removal(
                    scope,
                    self.active_canvas(),
                    &self.selection,
                    self.active_slide.as_ref(),
                    self.dispatcher.deck(),
                ) {
                    Some(cmd) => InterpretResult::Command(cmd),
                    None => InterpretResult::Nothing,
                }
            }
            InteractionEvent::RemoveSlideRequested { slide_id } => {
                interpret_remove_slide(self.dispatcher.deck(), &slide_id)
            }
            InteractionEvent::PasteRequested => {
                let built = self.clipboard.as_ref().and_then(|clip| {
                    build_paste_command(self.active_canvas(), clip, self.dispatcher.deck())
                });
                match built {
                    Some((cmd, PasteOutcome::Elements(ids))) => {
                        self.pending_paste_selection = Some(ids);
                        InterpretResult::Command(cmd)
                    }
                    Some((cmd, PasteOutcome::Slide(new_id))) => {
                        self.pending_new_active_slide = Some(new_id);
                        InterpretResult::Command(cmd)
                    }
                    None => InterpretResult::Nothing,
                }
            }

            InteractionEvent::TextEditStarted { .. } => InterpretResult::Nothing,
            InteractionEvent::TextEdited { .. } => InterpretResult::Nothing,
            InteractionEvent::TextEditEnded {
                element_id,
                content,
            } => {
                match build_set_text_command(
                    &self.dispatcher,
                    self.active_canvas(),
                    element_id,
                    content,
                ) {
                    Some(cmd) => InterpretResult::Command(cmd),
                    None => InterpretResult::Nothing,
                }
            }
            InteractionEvent::EmbedHtmlEditRequested { element_id, html } => {
                match build_set_embed_command(
                    &self.dispatcher,
                    self.active_canvas(),
                    element_id,
                    html,
                ) {
                    Some(cmd) => InterpretResult::Command(cmd),
                    None => InterpretResult::Nothing,
                }
            }

            InteractionEvent::CellTextEditRequested {
                element_id,
                row,
                col,
                text,
            } => match self.active_canvas() {
                Some(target) => InterpretResult::Command(Box::new(SetCellText {
                    target,
                    element_id,
                    row,
                    col,
                    text,
                })),
                None => InterpretResult::Nothing,
            },
            InteractionEvent::CellStyleChanged {
                element_id,
                cells,
                property,
                value,
            } => {
                if property.is_empty() || cells.is_empty() {
                    InterpretResult::Nothing
                } else {
                    match self.active_canvas() {
                        Some(target) => InterpretResult::Command(Box::new(SetCellStyles {
                            target,
                            element_id,
                            cells: cells.into_iter().map(|p| (p[0], p[1])).collect(),
                            property,
                            value,
                        })),
                        None => InterpretResult::Nothing,
                    }
                }
            }
            InteractionEvent::TableInsertRow { element_id, at } => match self.active_canvas() {
                Some(target) => InterpretResult::Command(Box::new(InsertTableRow {
                    target,
                    element_id,
                    at,
                })),
                None => InterpretResult::Nothing,
            },
            InteractionEvent::TableDeleteRow { element_id, at } => match self.active_canvas() {
                Some(target) => InterpretResult::Command(Box::new(DeleteTableRow {
                    target,
                    element_id,
                    at,
                })),
                None => InterpretResult::Nothing,
            },
            InteractionEvent::TableInsertColumn { element_id, at } => match self.active_canvas() {
                Some(target) => InterpretResult::Command(Box::new(InsertTableColumn {
                    target,
                    element_id,
                    at,
                })),
                None => InterpretResult::Nothing,
            },
            InteractionEvent::TableDeleteColumn { element_id, at } => match self.active_canvas() {
                Some(target) => InterpretResult::Command(Box::new(DeleteTableColumn {
                    target,
                    element_id,
                    at,
                })),
                None => InterpretResult::Nothing,
            },
            InteractionEvent::TableSetHeaderRows { element_id, count } => {
                match self.active_canvas() {
                    Some(target) => InterpretResult::Command(Box::new(SetTableHeaderRows {
                        target,
                        element_id,
                        count,
                    })),
                    None => InterpretResult::Nothing,
                }
            }
            InteractionEvent::TableSetHeaderColumns { element_id, count } => {
                match self.active_canvas() {
                    Some(target) => InterpretResult::Command(Box::new(SetTableHeaderColumns {
                        target,
                        element_id,
                        count,
                    })),
                    None => InterpretResult::Nothing,
                }
            }
            InteractionEvent::BackgroundClicked { .. } => {
                InterpretResult::Selection(SelectionState::empty())
            }
            InteractionEvent::PropertyChanged {
                element_id,
                property,
                value,
            } => interpret_property_changed(self.active_canvas(), element_id, property, value),
            InteractionEvent::GuideAdded { axis, pos } => {
                interpret_guide_added(self.active_canvas(), &axis, pos)
            }
            InteractionEvent::GuideMoved { index, pos } => match self.active_canvas() {
                Some(target) => {
                    InterpretResult::Command(Box::new(crate::commands::guide_commands::MoveGuide {
                        target,
                        index,
                        new_pos: pos,
                    }))
                }
                None => InterpretResult::Nothing,
            },
            InteractionEvent::GuideRemoved { index } => match self.active_canvas() {
                Some(target) => InterpretResult::Command(Box::new(
                    crate::commands::guide_commands::RemoveGuide { target, index },
                )),
                None => InterpretResult::Nothing,
            },
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
            InteractionEvent::RenameElementRequested {
                element_id,
                new_name,
            } => interpret_rename_request(self.active_canvas(), element_id, new_name),
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
                as_slide_background,
                as_element_fill,
            } => self.interpret_asset_imported(AssetImport {
                content_base64,
                original_filename,
                media_type,
                width,
                height,
                position,
                as_slide_background,
                as_element_fill,
            }),
            InteractionEvent::SlideThumbnailClicked { slide_id } => {
                if slide_id.is_empty() {
                    InterpretResult::Nothing
                } else {
                    InterpretResult::SetActiveSlide(slide_id)
                }
            }
            InteractionEvent::AddSlideRequested { layout_id } => {
                match build_insert_slide_after_active(
                    &self.dispatcher,
                    self.active_slide.as_ref(),
                    &layout_id,
                ) {
                    Some((cmd, new_id)) => {
                        self.pending_new_active_slide = Some(new_id);
                        InterpretResult::Command(cmd)
                    }
                    None => InterpretResult::Nothing,
                }
            }
            InteractionEvent::SlideLayoutPickerRequested => InterpretResult::SendSlideLayoutPicker,
            InteractionEvent::SlideTitleEditRequested {
                slide_id,
                new_title,
            } => match build_set_slide_title_command(&self.dispatcher, &slide_id, &new_title) {
                Some(cmd) => InterpretResult::Command(cmd),
                None => InterpretResult::Nothing,
            },
            InteractionEvent::SlideThumbnailReordered {
                slide_id,
                new_index,
            } => {
                let order: &[SlideId] = &self.dispatcher.deck().slide_order;
                match order.iter().position(|id| id == &slide_id) {
                    Some(from) => {
                        let to: usize = new_index.min(order.len().saturating_sub(1));
                        if from == to {
                            InterpretResult::Nothing
                        } else {
                            InterpretResult::Command(Box::new(ReorderSlide {
                                slide_id,
                                new_index: to,
                            }))
                        }
                    }
                    None => InterpretResult::Nothing,
                }
            }

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
            InteractionEvent::LayoutNameEditRequested {
                layout_id,
                new_name,
            } => {
                if layout_id.is_empty()
                    || !self
                        .dispatcher
                        .deck()
                        .theme
                        .layouts
                        .contains_key(&layout_id)
                {
                    InterpretResult::Nothing
                } else {
                    InterpretResult::Command(Box::new(SetLayoutName {
                        layout_id,
                        new_name,
                    }))
                }
            }
            InteractionEvent::GlobalsCssEditRequested { new_css } => {
                if self.dispatcher.deck().theme.globals_css == new_css {
                    InterpretResult::Nothing
                } else {
                    InterpretResult::Command(Box::new(SetGlobalsCss { new_css }))
                }
            }

            InteractionEvent::SetElementAnimation {
                element_id,
                category,
                enabled,
            } => interpret_set_element_animation(
                self.dispatcher.deck(),
                self.dispatcher.mode(),
                self.active_slide.as_ref(),
                element_id,
                &category,
                enabled,
            ),
            InteractionEvent::AddAnimation {
                element_id,
                catalog_id,
                direction,
            } => interpret_add_animation(
                self.dispatcher.deck(),
                self.dispatcher.mode(),
                self.active_slide.as_ref(),
                element_id,
                &catalog_id,
                direction.as_deref(),
            ),
            InteractionEvent::UpdateAnimation {
                animation_id,
                trigger,
                duration_ms,
                delay_ms,
                easing,
                iterations,
                targets,
            } => interpret_update_animation(
                self.dispatcher.deck(),
                self.active_slide.as_ref(),
                &animation_id,
                trigger.as_deref(),
                duration_ms,
                delay_ms,
                easing.as_deref(),
                iterations,
                targets,
            ),
            InteractionEvent::RemoveAnimationRequested { animation_id } => {
                match self.active_slide.clone() {
                    Some(slide_id) => InterpretResult::Command(Box::new(RemoveAnimation {
                        slide_id,
                        animation_id,
                    })),
                    None => InterpretResult::Nothing,
                }
            }
            InteractionEvent::MoveAnimation {
                animation_id,
                new_index,
                trigger,
            } => interpret_move_animation(
                self.dispatcher.deck(),
                self.active_slide.as_ref(),
                &animation_id,
                new_index,
                &trigger,
            ),
            InteractionEvent::SaveThemeRequested => {
                InterpretResult::FileAction(FileAction::SaveTheme)
            }
            InteractionEvent::LoadThemeRequested => {
                InterpretResult::FileAction(FileAction::LoadTheme)
            }
            InteractionEvent::SetSlideBackgroundRequested { background } => {
                match self.active_canvas() {
                    Some(CanvasTarget::Slide(sid)) => {
                        InterpretResult::Command(Box::new(SetSlideBackground {
                            slide_id: sid,
                            background: empty_to_none(background),
                        }))
                    }
                    Some(CanvasTarget::Layout(lid)) => {
                        InterpretResult::Command(Box::new(SetLayoutBackground {
                            layout_id: lid,
                            background: empty_to_none(background),
                        }))
                    }
                    None => InterpretResult::Nothing,
                }
            }
            InteractionEvent::SetSlideBackgroundImageCleared => match self.active_canvas() {
                Some(CanvasTarget::Slide(sid)) => {
                    InterpretResult::Command(Box::new(SetSlideBackgroundImage {
                        slide_id: sid,
                        background_image: None,
                    }))
                }
                Some(CanvasTarget::Layout(lid)) => {
                    InterpretResult::Command(Box::new(SetLayoutBackgroundImage {
                        layout_id: lid,
                        background_image: None,
                    }))
                }
                None => InterpretResult::Nothing,
            },
            InteractionEvent::SetSlideNotesRequested { notes } => match &self.active_slide {
                Some(sid) => InterpretResult::Command(Box::new(SetSlideNotes {
                    slide_id: sid.clone(),
                    notes: empty_to_none(notes),
                })),
                None => InterpretResult::Nothing,
            },
            InteractionEvent::SetSlideTransitionRequested { transition } => {
                match &self.active_slide {
                    Some(sid) => InterpretResult::Command(Box::new(SetSlideTransition {
                        slide_id: sid.clone(),
                        transition,
                    })),
                    None => InterpretResult::Nothing,
                }
            }
            InteractionEvent::SetMorphTransitionRequested {
                element_id,
                enabled,
                duration_ms,
                easing,
            } => match self.active_slide.clone() {
                Some(sid) => InterpretResult::Command(Box::new(SetMorphTransition {
                    target: CanvasTarget::Slide(sid),
                    element_id,
                    enabled,
                    duration_ms,
                    easing,
                })),
                None => InterpretResult::Nothing,
            },
            InteractionEvent::SetDeckTitleRequested { title } => {
                InterpretResult::Command(Box::new(SetDeckTitle { new_title: title }))
            }
            InteractionEvent::NudgeSelectionRequested { dx, dy } => self.interpret_nudge(dx, dy),
            InteractionEvent::NavigateSlideRequested { forward } => {
                self.interpret_navigate_slide(forward)
            }
            InteractionEvent::SetSlideLayoutRequested { layout_id } => {
                if layout_id.is_empty() {
                    InterpretResult::Nothing
                } else {
                    match &self.active_slide {
                        Some(sid) => InterpretResult::Command(Box::new(SetSlideLayout {
                            slide_id: sid.clone(),
                            new_layout_id: layout_id,
                            restore_root: None,
                        })),
                        None => InterpretResult::Nothing,
                    }
                }
            }
            InteractionEvent::SetGroupLayout {
                element_id,
                direction,
                distribution,
                alignment,
            } => match self.active_slide.clone() {
                Some(sid) => InterpretResult::Command(Box::new(SetGroupLayout {
                    target: CanvasTarget::Slide(sid),
                    element_id,
                    direction: parse_group_dir_opt(direction.as_deref()),
                    distribution: parse_group_dist_opt(distribution.as_deref()),
                    alignment: parse_group_align_opt(alignment.as_deref()),
                })),
                None => InterpretResult::Nothing,
            },
            InteractionEvent::SetGroupScale { element_id, scale } => {
                match self.active_slide.clone() {
                    Some(sid) => InterpretResult::Command(Box::new(SetGroupScale {
                        target: CanvasTarget::Slide(sid),
                        element_id,
                        scale,
                    })),
                    None => InterpretResult::Nothing,
                }
            }
            InteractionEvent::GroupSelectionRequested { element_ids } => {
                if element_ids.len() < 2 {
                    return InterpretResult::Nothing;
                }
                match self.active_slide.clone() {
                    Some(sid) => {
                        let group_id: ElementId = new_element_id();
                        self.pending_paste_selection = Some(vec![group_id.clone()]);
                        InterpretResult::Command(Box::new(GroupElements {
                            target: CanvasTarget::Slide(sid),
                            group_id,
                            element_ids,
                        }))
                    }
                    None => InterpretResult::Nothing,
                }
            }
            InteractionEvent::AlignSelectionRequested { element_ids, op } => {
                let target: CanvasTarget = match self.active_canvas() {
                    Some(t) => t,
                    None => return InterpretResult::Nothing,
                };
                let parsed: AlignOp = match parse_align_op(&op) {
                    Some(o) => o,
                    None => {
                        warn!(op = %op, "unknown align op");
                        return InterpretResult::Nothing;
                    }
                };
                InterpretResult::Command(Box::new(AlignElements {
                    target,
                    element_ids,
                    op: parsed,
                }))
            }
            InteractionEvent::QuitConfirmed { save } => {
                if save {
                    self.pending_quit = true;
                    InterpretResult::FileAction(FileAction::Save)
                } else {
                    self.quit_requested = true;
                    InterpretResult::Nothing
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
            InteractionEvent::KeyPressed { ref key, .. } if key == EXPORT_HTML_KEY => {
                InterpretResult::FileAction(FileAction::ExportHtml)
            }
            InteractionEvent::KeyPressed { ref key, .. } if key == EXPORT_PDF_KEY => {
                InterpretResult::FileAction(FileAction::ExportPdf)
            }
            InteractionEvent::KeyPressed { ref key, .. } if key == PRESENT_KEY => {
                InterpretResult::StartPresentation { windowed: false }
            }
            InteractionEvent::KeyPressed { ref key, .. } if key == PRESENT_WINDOWED_KEY => {
                InterpretResult::StartPresentation { windowed: true }
            }
            InteractionEvent::KeyPressed { ref key, .. }
                if key == DELETE_KEY_BACKSPACE || key == DELETE_KEY_DELETE =>
            {
                self.interpret_delete_selection()
            }
            InteractionEvent::KeyPressed { key, .. } if key.eq_ignore_ascii_case(DEBUG_KEY) => {
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

    fn handle_interaction(&mut self, event: InteractionEvent) -> AppResult<()> {
        if let InteractionEvent::ElementIdEditRequested { element_id, new_id } = &event {
            return self.handle_element_id_edit(element_id.clone(), new_id.clone());
        }
        let result: InterpretResult = self.interpret(event);
        match result {
            InterpretResult::Command(cmd) => {
                if let Some(asset_id) = self.pending_asset_broadcast.take()
                    && let Err(e) = self.send_asset_added(&asset_id)
                {
                    warn!(asset_id = %asset_id, "AssetAdded broadcast failed: {}", e);
                }
                self.dispatch_and_maybe_flush(cmd);
                Ok(())
            }
            InterpretResult::Selection(sel) => {
                self.selection = sel.clone();
                self.sender.send(MessageKind::SetSelection(sel))?;

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
            InterpretResult::StartPresentation { windowed } => {
                self.start_presentation(windowed);
                Ok(())
            }
            InterpretResult::SendSlideLayoutPicker => self.send_slide_layout_picker(),
            InterpretResult::Nothing => Ok(()),
        }
    }

    /// start_presentation — records the slide to open on and asks the event loop
    /// for presentation windows. `windowed` picks the layout: `false` puts one
    /// borderless fullscreen audience window on the external display, `true`
    /// opens an ordinary audience window plus a presenter console side by side
    /// on the display the editor is on.
    fn start_presentation(&mut self, windowed: bool) {
        if self.present.is_some() {
            debug!("start_presentation: already presenting; ignoring");
            return;
        }
        let idx: usize =
            match present_start_index(self.dispatcher.deck(), self.active_slide.as_ref()) {
                Some(i) => i,
                None => {
                    warn!("start_presentation: empty deck; nothing to present");
                    return;
                }
            };
        info!(slide_index = idx, windowed, "presentation requested");
        self.pending_present_index = Some(idx);
        (self.request_present_open)(windowed);
    }

    pub fn begin_presentation(&mut self, sender: WebviewSender) {
        let idx: usize = self.pending_present_index.take().unwrap_or(0);
        assert!(
            !self.dispatcher.deck().slide_order.is_empty(),
            "begin_presentation: empty deck"
        );
        info!(
            slide_index = idx,
            "presentation window ready; session begun"
        );
        self.present = Some(PresentationSession::new(sender, idx));
    }

    /// begin_presenter — adopts the presenter console webview. Called only when
    /// a second display exists; the console mirrors the same cursor as the
    /// audience window and never owns one of its own.
    pub fn begin_presenter(&mut self, sender: WebviewSender) {
        info!("presenter console window ready");
        self.presenter = Some(sender);
    }

    pub fn handle_present_control(&mut self, ctrl: PresentInbound) -> AppResult<()> {
        match ctrl {
            PresentInbound::Ready => self.handle_present_ready(),
            PresentInbound::PresenterReady => self.handle_presenter_ready(),
            PresentInbound::Advance => self.present_step(true),
            PresentInbound::Back => self.present_step(false),
            PresentInbound::Exit => {
                info!("presentation exit requested");
                (self.request_present_close)();
                Ok(())
            }
        }
    }

    /// handle_presenter_ready — first frame for the console: deck assets so the
    /// previews resolve image urls, then the current console frame.
    fn handle_presenter_ready(&self) -> AppResult<()> {
        let presenter = match &self.presenter {
            Some(p) => p,
            None => return Ok(()),
        };
        if let Some(bundle) = build_assets_bundle(self.dispatcher.deck()) {
            presenter.send(MessageKind::PresentAssets(bundle))?;
        }
        self.send_presenter_update()
    }

    /// send_presenter_update — pushes the current console frame. No-op when the
    /// console is not open or the cursor has no slide, so every call site can
    /// fire it unconditionally after a cursor move.
    fn send_presenter_update(&self) -> AppResult<()> {
        let (presenter, session) = match (&self.presenter, &self.present) {
            (Some(p), Some(s)) => (p, s),
            _ => return Ok(()),
        };
        let payload = match session.presenter_payload(self.dispatcher.deck()) {
            Some(p) => p,
            None => return Ok(()),
        };
        debug!(index = payload.index, "ipc -> PresenterUpdate");
        presenter.send(MessageKind::PresenterUpdate(payload))
    }

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

        if let Some(bundle) = build_assets_bundle(deck) {
            session.sender().send(MessageKind::PresentAssets(bundle))?;
        }
        if let Some(slide) = session.current_slide_payload(deck) {
            session.sender().send(MessageKind::PresentSlide(slide))?;
        }
        if let Some(reveal) = session.current_reveal(deck) {
            session.sender().send(MessageKind::PresentReveal(reveal))?;
        }
        self.send_presenter_update()
    }

    fn present_step(&mut self, forward: bool) -> AppResult<()> {
        let deck: &Deck = self.dispatcher.deck();
        let session = match self.present.as_mut() {
            Some(s) => s,
            None => return Ok(()),
        };
        let step: PresentStep = if forward {
            session.advance(deck)
        } else {
            session.back(deck)
        };
        match step {
            PresentStep::Reveal(reveal) => {
                session.sender().send(MessageKind::PresentReveal(reveal))?;
            }
            PresentStep::SlideChanged { slide, reveal } => {
                session.sender().send(MessageKind::PresentSlide(slide))?;
                session.sender().send(MessageKind::PresentReveal(reveal))?;
            }
            PresentStep::Unchanged => return Ok(()),
        }
        self.send_presenter_update()
    }

    pub fn end_presentation(&mut self) {
        self.presenter = None;
        if self.present.take().is_some() {
            info!("presentation session ended");
        }
    }

    fn run_file_action(&mut self, action: FileAction) -> AppResult<()> {
        match action {
            FileAction::New => self.file_new(),
            FileAction::Open => self.file_open(),
            FileAction::Save => self.file_save(),
            FileAction::SaveAs => self.file_save_as(),
            FileAction::SaveTheme => self.theme_save(),
            FileAction::LoadTheme => self.theme_load(),
            FileAction::ExportHtml => self.file_export_html(),
            FileAction::ExportPdf => self.file_export_pdf(),
        }
    }

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
            Err(e) => {
                warn!(label, "command failed: {}", e);
                if let Err(send_err) = self.sender.send(MessageKind::Notice {
                    message: format!("{label} failed"),
                    detail: Some(e.to_string()),
                }) {
                    warn!("notice send failed: {}", send_err);
                }
            }
        }
    }

    fn handle_element_id_edit(&mut self, old_id: ElementId, raw_new_id: String) -> AppResult<()> {
        let new_id: ElementId = sanitize_element_id(&raw_new_id);
        let target: CanvasTarget = match self.active_canvas() {
            Some(t) => t,
            None => return Ok(()),
        };
        if new_id.is_empty() || new_id == old_id {
            return self.send_object_tree();
        }
        let canvas = match self.dispatcher.deck().canvas(&target) {
            Some(c) => c,
            None => return Ok(()),
        };
        if canvas.find_element(&old_id).is_none() {
            return Ok(());
        }
        if canvas.find_element(&new_id).is_some() {
            warn!(new_id = %new_id, "element id already in use on canvas; ignoring rename");
            return self.send_object_tree();
        }

        self.dispatch_and_maybe_flush(Box::new(SetElementId {
            target: target.clone(),
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
            self.sender
                .send(MessageKind::SetSelection(self.selection.clone()))?;
        }
        Ok(())
    }

    fn react_to_outcome(&mut self, outcome: crate::commands::DispatchOutcome) {
        for msg in &outcome.warnings {
            if let Err(e) = self.sender.send(MessageKind::Notice {
                message: msg.clone(),
                detail: None,
            }) {
                warn!("notice send failed: {}", e);
            }
        }

        if outcome.affects_assets
            && let Err(e) = self.send_assets_bundle()
        {
            warn!("assets broadcast after dispatch failed: {}", e);
        }

        if outcome.affects_slide_meta
            && let Err(e) = self.send_slide_inspector()
        {
            warn!("slide inspector broadcast after dispatch failed: {}", e);
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
            if let Err(e) = self.send_slide_animations() {
                warn!("animations broadcast after dispatch failed: {}", e);
            }
            return;
        }
        if outcome.affects_guides {
            if let Err(e) = self.send_guides() {
                warn!("guides broadcast after dispatch failed: {}", e);
            }
            return;
        }
        if outcome.requires_remount {
            if let Err(e) = self.send_active_slide() {
                warn!("remount after dispatch failed: {}", e);
            }
        } else if outcome.affects_object_tree
            && let Err(e) = self.send_object_tree()
        {
            warn!("object tree broadcast after dispatch failed: {}", e);
        }

        if let Some(ids) = self.pending_paste_selection.take() {
            let mut sel = SelectionState::empty();
            sel.slide_id = self.active_slide.clone();
            sel.element_ids = ids;
            self.selection = sel.clone();
            if let Err(e) = self.sender.send(MessageKind::SetSelection(sel)) {
                warn!("paste selection broadcast failed: {}", e);
            }
        }

        if let Err(e) = self.send_save_state() {
            warn!("save-state broadcast after dispatch failed: {}", e);
        }
    }

    fn resync_after_slide_list_change(&mut self) {
        if let Some(pending) = self.pending_new_active_slide.take()
            && self.dispatcher.deck().slides.contains_key(&pending)
        {
            self.active_slide = Some(pending);
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
        if let Err(e) = self
            .sender
            .send(MessageKind::SetSelection(SelectionState::empty()))
        {
            warn!("selection clear after slide-list change failed: {}", e);
        }
        if let Err(e) = self.send_active_slide() {
            warn!("remount after slide-list change failed: {}", e);
        }
    }

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
            warn!(
                "layout list broadcast after layout-list change failed: {}",
                e
            );
        }
        if let Err(e) = self
            .sender
            .send(MessageKind::SetSelection(SelectionState::empty()))
        {
            warn!("selection clear after layout-list change failed: {}", e);
        }
        if let Err(e) = self.send_active_slide() {
            warn!("remount after layout-list change failed: {}", e);
        }
    }

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
        let mut ids: Vec<String> = self.selection.element_ids.clone();
        if !ids.iter().any(|id| id == element_id) {
            ids.push(element_id.to_string());
        }
        for id in &ids {
            if let Some(el) = canvas.find_element(id) {
                snap.record_geometry(target.clone(), id.clone(), el.geometry.clone());
            }
        }
        snap
    }

    fn interpret_delete_selection(&self) -> InterpretResult {
        interpret_delete_selection(&self.dispatcher, self.active_canvas(), &self.selection)
    }

    fn interpret_asset_imported(&mut self, a: AssetImport) -> InterpretResult {
        let AssetImport {
            content_base64,
            original_filename,
            media_type,
            width,
            height,
            position,
            as_slide_background,
            as_element_fill,
        } = a;

        let target: CanvasTarget = match self.active_canvas() {
            Some(t) => t,
            None => return InterpretResult::Nothing,
        };
        let bytes: Vec<u8> = match base64::engine::general_purpose::STANDARD.decode(content_base64)
        {
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

        self.pending_asset_broadcast = Some(entry.id.clone());

        if as_slide_background {
            let img: String = format!("var(--asset-{})", entry.id);
            return match self.active_canvas() {
                Some(CanvasTarget::Slide(sid)) => {
                    InterpretResult::Command(Box::new(SetSlideBackgroundImage {
                        slide_id: sid,
                        background_image: Some(img),
                    }))
                }
                Some(CanvasTarget::Layout(lid)) => {
                    InterpretResult::Command(Box::new(SetLayoutBackgroundImage {
                        layout_id: lid,
                        background_image: Some(img),
                    }))
                }
                None => InterpretResult::Nothing,
            };
        }

        if let Some(element_id) = as_element_fill {
            if element_id.is_empty() {
                return InterpretResult::Nothing;
            }
            let img: String = format!("var(--asset-{})", entry.id);
            let decls: [(&str, String); 4] = [
                ("background-image", img),
                ("background-size", "cover".to_string()),
                ("background-repeat", "no-repeat".to_string()),
                ("background-position", "center".to_string()),
            ];
            let cmds: Vec<Box<dyn Command>> = decls
                .into_iter()
                .map(|(prop, value)| {
                    Box::new(SetInlineStyle {
                        target: target.clone(),
                        element_id: element_id.clone(),
                        property: prop.to_string(),
                        new_value: value,
                    }) as Box<dyn Command>
                })
                .collect();
            return InterpretResult::Command(Box::new(CompositeCommand::new(
                cmds,
                "Set fill image",
            )));
        }

        let slide_dims: (u32, u32) = (
            self.dispatcher.deck().manifest.dimensions.width,
            self.dispatcher.deck().manifest.dimensions.height,
        );
        let node: ElementNode =
            build_image_element_from_asset(&entry, width, height, position, slide_dims);
        let canvas = match self.dispatcher.deck().canvas(&target) {
            Some(c) => c,
            None => return InterpretResult::Nothing,
        };
        let parent_id: ElementId = canvas.root().id.clone();
        let position_in_parent: usize = canvas.root().children.len();
        InterpretResult::Command(Box::new(InsertElement {
            target,
            parent_id,
            position: position_in_parent,
            node,
        }))
    }

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

    pub fn file_new(&mut self) -> AppResult<()> {
        info!("file: new (blank deck)");
        let deck: Deck = Deck::new_blank();
        self.adopt_deck(deck);
        self.sender
            .send(MessageKind::SetSelection(SelectionState::empty()))?;
        self.send_slide_list()?;
        self.send_assets_bundle()?;
        self.send_active_slide()?;
        self.send_guides()
    }

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

    pub fn file_export_html(&mut self) -> AppResult<()> {
        let dest: PathBuf = match rfd::FileDialog::new().pick_folder() {
            Some(p) => p,
            None => return Ok(()),
        };
        let bundle = match crate::export::build_html_export(self.dispatcher.deck()) {
            Ok(b) => b,
            Err(e) => {
                self.sender.send(MessageKind::Notice {
                    message: "HTML export failed".to_string(),
                    detail: Some(e.to_string()),
                })?;
                return Ok(());
            }
        };
        if self
            .io_thread
            .submit(IoRequest::ExportHtml {
                files: bundle.files,
                dest_dir: dest,
            })
            .is_err()
        {
            warn!("export: could not queue (io thread closed)");
        }
        Ok(())
    }

    pub fn file_export_pdf(&mut self) -> AppResult<()> {
        let chrome: PathBuf = match self.resolve_chrome_for_export() {
            Some(p) => p,
            None => return Ok(()),
        };
        let mut dest: PathBuf = match rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name("deck.pdf")
            .save_file()
        {
            Some(p) => p,
            None => return Ok(()),
        };
        if dest.extension().and_then(|e| e.to_str()) != Some("pdf") {
            dest.set_extension("pdf");
        }
        let html: String = crate::export::build_pdf_print_html(self.dispatcher.deck());
        let raster = crate::export::pdf::raster_page_rects(self.dispatcher.deck());
        (self.dispatch_pdf_job)(PdfJob {
            html,
            raster,
            chrome,
            dest,
        });
        Ok(())
    }

    const LOCATE_LABEL: &'static str = "Locate…";
    const DOWNLOAD_LABEL: &'static str = "Download Chromium";

    fn resolve_chrome_for_export(&mut self) -> Option<PathBuf> {
        use crate::export::chromium::{
            Resolved, is_valid_chrome, normalize_chrome_path, resolve_from_config_or_system,
        };
        if let Resolved::Found(p) = resolve_from_config_or_system() {
            return Some(p);
        }
        let choice = rfd::MessageDialog::new()
            .set_title("Chromium needed for PDF export")
            .set_description(
                "No Chrome/Chromium was found. Locate an existing install, or download a \
private copy (~150 MB).",
            )
            .set_buttons(rfd::MessageButtons::YesNoCancelCustom(
                Self::LOCATE_LABEL.to_string(),
                Self::DOWNLOAD_LABEL.to_string(),
                "Cancel".to_string(),
            ))
            .show();
        match choice {
            rfd::MessageDialogResult::Custom(label) if label == Self::LOCATE_LABEL => {
                let picked = normalize_chrome_path(rfd::FileDialog::new().pick_file()?);
                if !is_valid_chrome(&picked) {
                    self.toast(
                        "PDF export",
                        "That file is not a working Chrome/Chromium binary.",
                    );
                    return None;
                }
                let mut cfg = crate::config::load();
                cfg.chrome_path = Some(picked.clone());
                let _ = crate::config::save(&cfg);
                Some(picked)
            }
            rfd::MessageDialogResult::Custom(label) if label == Self::DOWNLOAD_LABEL => {
                self.pending_export_after_chrome = true;
                (self.dispatch_chromium_download)();
                None
            }
            _ => None,
        }
    }

    pub fn on_chromium_ready(&mut self) {
        if !self.pending_export_after_chrome {
            return;
        }
        self.pending_export_after_chrome = false;
        if let Err(e) = self.file_export_pdf() {
            warn!("resumed pdf export failed: {}", e);
        }
    }

    pub fn send_chromium_progress(&self, received: u64, total: Option<u64>) {
        if let Err(e) = self
            .sender
            .send(MessageKind::ChromiumDownloadProgress { received, total })
        {
            warn!("chromium progress send failed: {}", e);
        }
    }

    pub fn send_chromium_done(&self, ok: bool, message: String) {
        if let Err(e) = self
            .sender
            .send(MessageKind::ChromiumDownloadDone { ok, message })
        {
            warn!("chromium done send failed: {}", e);
        }
    }

    fn toast(&self, message: &str, detail: &str) {
        if let Err(e) = self.sender.send(MessageKind::Notice {
            message: message.to_string(),
            detail: Some(detail.to_string()),
        }) {
            warn!("toast failed: {}", e);
        }
    }

    pub fn notify_pdf_export(&self, dest: &std::path::Path, ok: bool) {
        let message = if ok {
            "Exported PDF"
        } else {
            "PDF export failed"
        };
        if let Err(e) = self.sender.send(MessageKind::Notice {
            message: message.to_string(),
            detail: Some(dest.display().to_string()),
        }) {
            warn!("pdf export toast failed: {}", e);
        }
    }

    pub fn file_save_as(&mut self) -> AppResult<()> {
        let picked: Option<PathBuf> = prompt_save_as(self.dispatcher.deck().bundle_path.as_deref());
        let target: PathBuf = match picked {
            Some(p) => ensure_extension(p, BUNDLE_FILE_EXTENSION),
            None => {
                debug!("file: save-as cancelled by user");
                self.pending_quit = false;
                return Ok(());
            }
        };
        self.submit_save(target)
    }

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

    pub fn load_path(&mut self, path: PathBuf) {
        info!(path = %path.display(), "landing: open recent");
        if self.io_thread.submit(IoRequest::Load { path }).is_err() {
            warn!("landing: open recent could not be queued (io thread closed)");
        }
    }

    pub fn theme_save(&mut self) -> AppResult<()> {
        let picked: Option<PathBuf> = prompt_save_theme();
        let target: PathBuf = match picked {
            Some(p) => ensure_extension(p, THEME_FILE_EXTENSION),
            None => {
                debug!("theme: save cancelled by user");
                return Ok(());
            }
        };
        let serialized = serialize_theme(
            &self.dispatcher.deck().theme,
            &self.dispatcher.deck().assets,
        )?;
        info!(target = %target.display(), "theme: save queued");
        if self
            .io_thread
            .submit(IoRequest::SaveTheme {
                serialized,
                target_path: target,
            })
            .is_err()
        {
            warn!("theme: save could not be queued (io thread closed)");
        }
        Ok(())
    }

    pub fn theme_load(&mut self) -> AppResult<()> {
        let path: PathBuf = match prompt_open_theme() {
            Some(p) => p,
            None => {
                debug!("theme: load cancelled by user");
                return Ok(());
            }
        };
        info!(path = %path.display(), "theme: load requested");
        if self
            .io_thread
            .submit(IoRequest::LoadTheme { path })
            .is_err()
        {
            warn!("theme: load could not be queued (io thread closed)");
        }
        Ok(())
    }

    pub fn handle_io_response(&mut self, response: IoResponse) -> AppResult<()> {
        match response {
            IoResponse::Exported { dest } => {
                info!(dest = %dest.display(), "export: html written");
                self.sender.send(MessageKind::Notice {
                    message: "Exported HTML".to_string(),
                    detail: Some(dest.display().to_string()),
                })?;
                Ok(())
            }
            IoResponse::Saved { path } => {
                info!(path = %path.display(), "file: save committed");
                crate::recents::record(&path, &recent_title(&path));
                {
                    let deck = self.dispatcher.deck_mut();
                    deck.bundle_path = Some(path);
                    deck.dirty_slides.clear();
                    deck.manifest_dirty = false;
                    for layout in deck.theme.layouts.values_mut() {
                        layout.dirty = false;
                    }
                }
                if self.pending_quit {
                    self.pending_quit = false;
                    self.quit_requested = true;
                }
                self.send_save_state()
            }
            IoResponse::Loaded { serialized, path } => {
                info!(path = %path.display(), "file: load received");
                crate::recents::record(&path, &recent_title(&path));
                let mut deck: Deck = deserialize_deck(serialized)?;
                deck.bundle_path = Some(path);
                self.adopt_deck(deck);
                self.sender
                    .send(MessageKind::SetSelection(SelectionState::empty()))?;
                self.send_slide_list()?;
                self.send_assets_bundle()?;
                self.send_active_slide()?;
                self.send_save_state()
            }
            IoResponse::ThemeSaved { path } => {
                info!(path = %path.display(), "theme: save committed");
                Ok(())
            }
            IoResponse::ThemeLoaded { serialized, path } => {
                info!(path = %path.display(), "theme: load received");
                match deserialize_theme(serialized) {
                    Ok((theme, assets)) => {
                        let add_assets = collect_loaded_assets(&assets);
                        self.dispatch_and_maybe_flush(Box::new(SwapTheme {
                            install_theme: theme,
                            add_assets,
                            remove_asset_ids: Vec::new(),
                        }));
                        Ok(())
                    }
                    Err(e) => {
                        warn!("theme: deserialize failed: {}", e);
                        self.sender.send(MessageKind::Notice {
                            message: format!("Failed to load theme: {e}"),
                            detail: None,
                        })
                    }
                }
            }
            IoResponse::Error {
                operation,
                path,
                message,
            } => {
                warn!(operation, ?path, "file: io error: {}", message);
                Ok(())
            }
        }
    }

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

    fn adopt_deck(&mut self, deck: Deck) {
        let active: Option<SlideId> = deck.slide_order.first().cloned();
        self.dispatcher = CommandDispatcher::new(deck);
        self.active_slide = active;
        self.selection = SelectionState::empty();
    }

    fn on_agent_panel_toggled(&mut self, open: bool) -> AppResult<()> {
        if open {
            let mut cfg: crate::config::Config = crate::config::load();
            if cfg.agents.is_empty()
                && let Some(def) = crate::config::detect_default_agent()
            {
                cfg.agents.push(def);
                if let Err(e) = crate::config::save(&cfg) {
                    warn!("failed to save seeded agent config: {}", e);
                }
                info!("seeded default agent from installed claude-code-acp-rs");
            }
            let names: Vec<String> = crate::config::agent_names(&cfg);
            self.sender
                .send(MessageKind::AgentListUpdate(crate::ipc::agent::AgentList {
                    agents: names.clone(),
                }))?;
            let error: Option<String> = if names.is_empty() {
                Some(
                    "No agents configured. Install claude-code-acp-rs or add one via + Add agent."
                        .to_string(),
                )
            } else {
                None
            };
            self.sender
                .send(MessageKind::AgentPanelStateUpdate(AgentPanelState {
                    running: false,
                    error,
                }))?;
        } else {
            if let Some(mut h) = self.agent.take() {
                let _ = h.shutdown();
            }
            self.agent_name = None;
        }
        Ok(())
    }

    fn ensure_agent(&mut self, name: &str) -> AppResult<bool> {
        if name.is_empty() {
            self.send_agent_error("Select an agent from the dropdown.")?;
            return Ok(false);
        }
        if self.agent.is_some() && self.agent_name.as_deref() == Some(name) {
            return Ok(true);
        }
        if let Some(mut h) = self.agent.take() {
            let _ = h.shutdown();
        }
        self.agent_workspace = None;
        self.agent_name = None;
        let cfg: crate::config::Config = crate::config::load();
        let agent_cfg = match crate::agent::from_named(&cfg, name) {
            Some(c) => c,
            None => {
                self.send_agent_error(&format!("Agent '{}' is not configured.", name))?;
                return Ok(false);
            }
        };
        let workspace: crate::agent::workspace::Workspace =
            match crate::agent::workspace::Workspace::create(self.dispatcher.deck()) {
                Ok(w) => w,
                Err(e) => {
                    self.send_agent_error(&format!("Failed to prepare deck workspace: {}", e))?;
                    return Ok(false);
                }
            };
        let cwd: std::path::PathBuf = workspace.path().to_path_buf();
        let sink = self.agent_sink.clone();
        let callback = move |ev: crate::agent::AgentEvent| {
            sink(ev);
        };
        match crate::agent::acp::spawn_agent(&agent_cfg, &cwd, callback) {
            Ok(handle) => {
                self.agent = Some(handle);
                self.agent_workspace = Some(workspace);
                self.agent_name = Some(name.to_string());
                info!("agent spawned: {}", name);
                Ok(true)
            }
            Err(e) => {
                self.send_agent_error(&format!("Failed to spawn agent: {}", e))?;
                warn!("agent spawn failed: {}", e);
                Ok(false)
            }
        }
    }

    fn send_activity(&self, phase: &str, label: &str) -> AppResult<()> {
        assert!(!phase.is_empty(), "send_activity called with empty phase");
        self.sender.send(MessageKind::AgentActivityUpdate(
            crate::ipc::agent::AgentActivity {
                phase: phase.into(),
                label: label.into(),
            },
        ))
    }

    fn send_agent_error(&self, message: &str) -> AppResult<()> {
        assert!(
            !message.is_empty(),
            "send_agent_error called with empty message"
        );
        self.sender
            .send(MessageKind::AgentPanelStateUpdate(AgentPanelState {
                running: false,
                error: Some(message.to_string()),
            }))
    }

    fn on_agent_prompt(&mut self, text: String, agent: String) -> AppResult<()> {
        let was_ready: bool =
            self.agent.is_some() && self.agent_name.as_deref() == Some(agent.as_str());
        if !self.ensure_agent(&agent)? {
            return Ok(());
        }
        if let Some(handle) = &self.agent {
            handle.send_prompt(&text)?;
            self.sender
                .send(MessageKind::AgentPanelStateUpdate(AgentPanelState {
                    running: true,
                    error: None,
                }))?;
            if was_ready {
                self.send_activity("thinking", "Thinking…")?;
            } else {
                self.send_activity("starting", "Starting agent…")?;
            }
        }
        Ok(())
    }

    fn on_agent_add(&mut self, name: String, command: String, args: Vec<String>) -> AppResult<()> {
        if name.is_empty() || command.is_empty() {
            return self.send_agent_error("Agent name and command are required.");
        }
        let mut cfg: crate::config::Config = crate::config::load();
        cfg.agents.retain(|a| a.name != name);
        cfg.agents.push(crate::config::AgentDef {
            name,
            command,
            args,
        });
        if let Err(e) = crate::config::save(&cfg) {
            warn!("failed to save agent config: {}", e);
        }
        self.sender
            .send(MessageKind::AgentListUpdate(crate::ipc::agent::AgentList {
                agents: crate::config::agent_names(&cfg),
            }))
    }

    fn on_agent_cancel(&mut self) -> AppResult<()> {
        if let Some(agent) = &self.agent {
            agent.cancel()?;
        }
        self.send_activity("idle", "")?;
        Ok(())
    }

    fn on_agent_permission_reply(&mut self, request_id: String, allow: bool) -> AppResult<()> {
        if let Some((path, contents)) = self.agent_pending.remove(&request_id) {
            if allow {
                match crate::agent::vfs::parse_slide_write(&path, &contents) {
                    Ok(sw) if sw.new_children.is_empty() => {
                        if let Some(agent) = &self.agent {
                            let _ = agent.send_fs_error(&request_id, "no usable elements in slide");
                        }
                    }
                    Ok(sw) => {
                        let real_id: Option<SlideId> = crate::agent::vfs::resolve_slide_ref(
                            self.dispatcher.deck(),
                            &sw.slide_id,
                        );
                        let real_id = match real_id {
                            Some(id) => id,
                            None => {
                                if let Some(agent) = &self.agent {
                                    let _ = agent.send_fs_error(&request_id, "unknown slide path");
                                }
                                return Ok(());
                            }
                        };
                        self.dispatch_and_maybe_flush(Box::new(ReplaceSlideContent {
                            slide_id: real_id,
                            new_children: sw.new_children,
                        }));
                        if let Some(agent) = &self.agent {
                            let _ = agent.send_fs_response(&request_id, serde_json::Value::Null);
                        }
                        if let Err(e) = self.send_slide_list() {
                            warn!("slide list broadcast after agent write failed: {}", e);
                        }
                        if let Err(e) = self.send_object_tree() {
                            warn!("object tree broadcast after agent write failed: {}", e);
                        }
                    }
                    Err(e) => {
                        if let Some(agent) = &self.agent {
                            let _ = agent.send_fs_error(&request_id, &e.to_string());
                        }
                    }
                }
            } else if let Some(agent) = &self.agent {
                let _ = agent.send_fs_error(&request_id, "user rejected edit");
            }
        } else if let Some(agent) = &self.agent {
            let _ = agent.send_permission_reply(&request_id, allow);
        }
        Ok(())
    }

    fn __ingest_agent_changes(&mut self) -> AppResult<()> {
        let changes: Vec<crate::agent::workspace::SlideChange> = match self.agent_workspace.as_mut()
        {
            Some(ws) => ws.collect_changes(),
            None => return Ok(()),
        };
        if changes.is_empty() {
            return Ok(());
        }
        let mut skipped_total: usize = 0;
        for change in changes {
            skipped_total += change.skipped;
            if change.new_children.is_empty() {
                warn!("agent slide {} had no usable elements", change.slide_ref);
                continue;
            }
            if change.is_new {
                self.dispatch_and_maybe_flush(__new_slide_command(
                    self.dispatcher.deck().slide_order.len(),
                    change.new_children,
                ));
                continue;
            }
            let real_id: Option<SlideId> =
                crate::agent::vfs::resolve_slide_ref(self.dispatcher.deck(), &change.slide_ref);
            let real_id = match real_id {
                Some(id) => id,
                None => {
                    warn!("agent edited unknown slide {}", change.slide_ref);
                    continue;
                }
            };
            self.dispatch_and_maybe_flush(Box::new(ReplaceSlideContent {
                slide_id: real_id,
                new_children: change.new_children,
            }));
        }
        if skipped_total > 0 {
            self.toast(
                &format!("{} element(s) could not be loaded", skipped_total),
                "The agent produced HTML that did not match the slide element format.",
            );
        }
        self.send_slide_list()?;
        self.send_object_tree()?;
        Ok(())
    }

    fn __handle_agent_fs_read(&mut self, request_id: String, path: String) -> AppResult<()> {
        match crate::agent::vfs::resolve_read(self.dispatcher.deck(), &path) {
            Some(c) => {
                let _ = self
                    .agent
                    .as_ref()
                    .map(|a| a.send_fs_response(&request_id, serde_json::json!({"content": c})));
            }
            None => {
                let _ = self
                    .agent
                    .as_ref()
                    .map(|a| a.send_fs_error(&request_id, "no such path"));
            }
        }
        self.sender.send(MessageKind::AgentTool(AgentToolNotice {
            kind: "read".to_string(),
            slide_id: None,
            summary: format!("read {}", path),
        }))
    }

    pub fn handle_agent_event(&mut self, ev: crate::agent::AgentEvent) -> AppResult<()> {
        match ev {
            crate::agent::AgentEvent::SessionReady => {
                self.send_activity("thinking", "Thinking…")?;
            }
            crate::agent::AgentEvent::Thought { text } => {
                self.send_activity("thinking", "Thinking…")?;
                self.sender.send(MessageKind::AgentThoughtUpdate(
                    crate::ipc::agent::AgentThought { text },
                ))?;
            }
            crate::agent::AgentEvent::StreamChunk {
                role,
                text,
                final_chunk,
            } => {
                self.send_activity("streaming", "Responding…")?;
                self.sender
                    .send(MessageKind::AgentStream(AgentStreamChunk {
                        role,
                        text,
                        final_chunk,
                    }))?;
            }
            crate::agent::AgentEvent::ToolStatus { id, title, status } => {
                let label: String = if title.is_empty() {
                    "Working…".to_string()
                } else {
                    title.clone()
                };
                self.send_activity("tool", &label)?;
                self.sender.send(MessageKind::AgentToolStatusUpdate(
                    crate::ipc::agent::AgentToolStatus { id, title, status },
                ))?;
            }
            crate::agent::AgentEvent::FsRead { request_id, path } => {
                self.send_activity("tool", "Reading slide")?;
                self.__handle_agent_fs_read(request_id, path)?;
            }
            crate::agent::AgentEvent::FsWrite {
                request_id,
                path,
                contents,
            } => {
                self.send_activity("tool", "Writing slide")?;
                self.agent_pending
                    .insert(request_id.clone(), (path.clone(), contents));
                self.sender
                    .send(MessageKind::AgentPermission(AgentPermissionAsk {
                        request_id,
                        slide_id: crate::agent::vfs::slide_id_from_path(&path).unwrap_or_default(),
                        summary: format!("Agent wants to edit {}", path),
                    }))?;
            }
            crate::agent::AgentEvent::PermissionRequest {
                request_id,
                path: _path,
                summary,
            } => {
                self.send_activity("awaiting_approval", "Waiting for approval")?;
                self.sender
                    .send(MessageKind::AgentPermission(AgentPermissionAsk {
                        request_id,
                        slide_id: String::new(),
                        summary,
                    }))?;
            }
            crate::agent::AgentEvent::TurnEnded => {
                self.__ingest_agent_changes()?;
                self.send_activity("idle", "")?;
                self.sender
                    .send(crate::ipc::MessageKind::AgentPanelStateUpdate(
                        crate::ipc::agent::AgentPanelState {
                            running: false,
                            error: None,
                        },
                    ))?;
            }
            crate::agent::AgentEvent::Failed { message } => {
                self.send_activity("error", "Error")?;
                self.sender
                    .send(crate::ipc::MessageKind::AgentPanelStateUpdate(
                        crate::ipc::agent::AgentPanelState {
                            running: false,
                            error: Some(message),
                        },
                    ))?;
            }
        }
        Ok(())
    }
}

fn recent_title(path: &std::path::Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "Untitled".to_string())
}

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

fn prompt_open() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Slide Deck", &[BUNDLE_FILE_EXTENSION, "slidedeck"])
        .pick_file()
}

fn prompt_save_theme() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Slide Theme", &[THEME_FILE_EXTENSION])
        .set_file_name(format!("Untitled.{THEME_FILE_EXTENSION}"))
        .save_file()
}

fn prompt_open_theme() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Slide Theme", &[THEME_FILE_EXTENSION])
        .pick_file()
}

fn collect_loaded_assets(registry: &AssetRegistry) -> Vec<(AssetEntry, Vec<u8>)> {
    let mut out: Vec<(AssetEntry, Vec<u8>)> = Vec::with_capacity(registry.assets.len());
    for entry in &registry.assets {
        if let Some(bytes) = registry.files.get(&entry.path) {
            out.push((entry.clone(), bytes.clone()));
        } else {
            warn!(asset_id = %entry.id, "collect_loaded_assets: bytes missing; skipping");
        }
    }
    out
}

fn ensure_extension(path: PathBuf, ext: &str) -> PathBuf {
    assert!(!ext.is_empty(), "ensure_extension: empty extension");
    match path.extension().and_then(|e| e.to_str()) {
        Some(existing) if existing.eq_ignore_ascii_case(ext) => path,
        _ => path.with_extension(ext),
    }
}

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
            original_filename: entry.original_filename.clone(),
        });
    }
    if payloads.is_empty() {
        return None;
    }
    Some(AssetsBundle { assets: payloads })
}

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

fn interpret_crop_committed(
    active: Option<CanvasTarget>,
    element_id: ElementId,
    new_position: Point,
    new_size: Size,
    background_size: String,
    background_position: String,
) -> InterpretResult {
    assert!(
        !element_id.is_empty(),
        "interpret_crop_committed: empty element_id"
    );
    let target: CanvasTarget = match active {
        Some(t) => t,
        None => return InterpretResult::Nothing,
    };
    let cmds: Vec<Box<dyn Command>> = vec![
        Box::new(SetInlineStyle {
            target: target.clone(),
            element_id: element_id.clone(),
            property: "background-size".to_string(),
            new_value: background_size,
        }),
        Box::new(SetInlineStyle {
            target: target.clone(),
            element_id: element_id.clone(),
            property: "background-position".to_string(),
            new_value: background_position,
        }),
        Box::new(SetInlineStyle {
            target: target.clone(),
            element_id: element_id.clone(),
            property: "background-repeat".to_string(),
            new_value: "no-repeat".to_string(),
        }),
        Box::new(SetInlineStyle {
            target: target.clone(),
            element_id: element_id.clone(),
            property: "overflow".to_string(),
            new_value: "hidden".to_string(),
        }),
        Box::new(ResizeElement {
            target,
            element_id,
            new_x: new_position.x,
            new_y: new_position.y,
            new_width: new_size.width,
            new_height: new_size.height,
        }),
    ];
    InterpretResult::Command(Box::new(CompositeCommand::new(
        cmds,
        CROP_TRANSACTION_LABEL,
    )))
}

fn resize_commit_command(
    target: CanvasTarget,
    element_id: ElementId,
    new_position: Point,
    new_size: Size,
    background_size: Option<String>,
    background_position: Option<String>,
) -> Box<dyn Command> {
    let resize = ResizeElement {
        target: target.clone(),
        element_id: element_id.clone(),
        new_x: new_position.x,
        new_y: new_position.y,
        new_width: new_size.width,
        new_height: new_size.height,
    };
    match (background_size, background_position) {
        (Some(bs), Some(bp)) => {
            let cmds: Vec<Box<dyn Command>> = vec![
                Box::new(SetInlineStyle {
                    target: target.clone(),
                    element_id: element_id.clone(),
                    property: "background-size".to_string(),
                    new_value: bs,
                }),
                Box::new(SetInlineStyle {
                    target,
                    element_id,
                    property: "background-position".to_string(),
                    new_value: bp,
                }),
                Box::new(resize),
            ];
            Box::new(CompositeCommand::new(cmds, RESIZE_TRANSACTION_LABEL))
        }
        _ => Box::new(resize),
    }
}

fn interpret_remove_slide(deck: &Deck, slide_id: &SlideId) -> InterpretResult {
    if deck.slide_order.len() <= 1 || !deck.slides.contains_key(slide_id) {
        return InterpretResult::Nothing;
    }
    InterpretResult::Command(Box::new(RemoveSlide {
        slide_id: slide_id.clone(),
    }))
}

fn collect_copy(
    scope: crate::ipc::ClipboardScope,
    active: Option<CanvasTarget>,
    selection: &SelectionState,
    active_slide: Option<&SlideId>,
    deck: &Deck,
) -> Option<Clipboard> {
    match scope {
        crate::ipc::ClipboardScope::Elements => {
            let target = active?;
            let canvas = deck.canvas(&target)?;
            let mut out: Vec<ElementNode> = Vec::new();
            for id in &selection.element_ids {
                if let Some(node) = canvas.find_element(id) {
                    out.push(node.clone());
                }
            }
            if out.is_empty() {
                return None;
            }
            Some(Clipboard::Elements(out))
        }
        crate::ipc::ClipboardScope::Slide => {
            let sid = active_slide?;
            let slide = deck.slides.get(sid)?;
            Some(Clipboard::Slide(Box::new(slide.clone())))
        }
    }
}

fn build_paste_command(
    active: Option<CanvasTarget>,
    clipboard: &Clipboard,
    deck: &Deck,
) -> Option<(Box<dyn Command>, PasteOutcome)> {
    match clipboard {
        Clipboard::Elements(nodes) => {
            let target = active?;
            let canvas = deck.canvas(&target)?;
            let parent_id: ElementId = canvas.root().id.clone();
            let base: usize = canvas.root().children.len();
            let mut cmds: Vec<Box<dyn Command>> = Vec::new();
            let mut ids: Vec<ElementId> = Vec::new();
            for (i, node) in nodes.iter().enumerate() {
                let mut copy = node.clone();
                crate::deck::element::regenerate_ids(&mut copy);
                ids.push(copy.id.clone());
                cmds.push(Box::new(InsertElement {
                    target: target.clone(),
                    parent_id: parent_id.clone(),
                    position: base + i,
                    node: copy,
                }));
            }
            if cmds.is_empty() {
                return None;
            }
            let cmd: Box<dyn Command> = Box::new(CompositeCommand::new(cmds, PASTE_LABEL));
            Some((cmd, PasteOutcome::Elements(ids)))
        }
        Clipboard::Slide(slide) => {
            let position: usize = match active {
                Some(CanvasTarget::Slide(sid)) => deck
                    .slide_order
                    .iter()
                    .position(|s| *s == sid)
                    .map(|i| i + 1)
                    .unwrap_or(deck.slide_order.len()),
                _ => deck.slide_order.len(),
            };
            let mut copy = slide.clone();
            let new_id = crate::deck::new_slide_id();
            let id_map = crate::deck::element::regenerate_ids(&mut copy.root);
            for anim in copy.animations.iter_mut() {
                if let Some(new) = id_map.get(&anim.element_id) {
                    anim.element_id = new.clone();
                }
            }
            copy.id = new_id.clone();
            let manifest_entry = crate::bundle::SlideEntry {
                id: new_id.clone(),
                path: crate::bundle::manifest::slide_path_for(&new_id),
                layout_id: copy.layout_id.clone(),
                title: String::new(),
                thumbnail: None,
                transition: None,
                duration_hint: None,
                notes_ref: None,
                animations: copy.animations.clone(),
                guides: copy.guides.clone(),
                background: copy.metadata.background.clone(),
                background_image: copy.metadata.background_image.clone(),
                notes: None,
            };
            let cmd: Box<dyn Command> = Box::new(InsertSlide {
                position,
                slide: *copy,
                manifest_entry,
            });
            Some((cmd, PasteOutcome::Slide(new_id)))
        }
    }
}

fn build_cut_removal(
    scope: crate::ipc::ClipboardScope,
    active: Option<CanvasTarget>,
    selection: &SelectionState,
    active_slide: Option<&SlideId>,
    deck: &Deck,
) -> Option<Box<dyn Command>> {
    match scope {
        crate::ipc::ClipboardScope::Elements => {
            let target = active?;
            let canvas = deck.canvas(&target)?;
            let mut cmds: Vec<Box<dyn Command>> = Vec::new();
            for id in &selection.element_ids {
                if canvas.find_element(id).is_some() {
                    cmds.push(Box::new(RemoveElementCommand {
                        target: target.clone(),
                        element_id: id.clone(),
                    }));
                }
            }
            if cmds.is_empty() {
                return None;
            }
            Some(Box::new(CompositeCommand::new(cmds, CUT_LABEL)))
        }
        crate::ipc::ClipboardScope::Slide => {
            if deck.slide_order.len() <= 1 {
                return None;
            }
            let sid = active_slide?;
            if !deck.slides.contains_key(sid) {
                return None;
            }
            Some(Box::new(RemoveSlide {
                slide_id: sid.clone(),
            }))
        }
    }
}

fn interpret_property_changed(
    active: Option<CanvasTarget>,
    element_id: ElementId,
    property: String,
    value: String,
) -> InterpretResult {
    assert!(
        !element_id.is_empty(),
        "interpret_property_changed: empty element_id"
    );
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

fn interpret_guide_added(active: Option<CanvasTarget>, axis: &str, pos: f64) -> InterpretResult {
    let target: CanvasTarget = match active {
        Some(t) => t,
        None => return InterpretResult::Nothing,
    };
    let ax: crate::deck::guide::GuideAxis = match axis {
        "h" => crate::deck::guide::GuideAxis::Horizontal,
        "v" => crate::deck::guide::GuideAxis::Vertical,
        other => {
            warn!(axis = other, "guide: unrecognised axis token");
            return InterpretResult::Nothing;
        }
    };
    InterpretResult::Command(Box::new(crate::commands::guide_commands::AddGuide {
        target,
        axis: ax,
        pos,
        index: None,
    }))
}

fn guide_to_dto(g: &crate::deck::guide::Guide) -> GuideDto {
    GuideDto {
        axis: match g.axis {
            crate::deck::guide::GuideAxis::Horizontal => "h",
            crate::deck::guide::GuideAxis::Vertical => "v",
        }
        .to_string(),
        pos: g.pos,
    }
}

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

    let selected_set: std::collections::HashSet<&str> =
        selection.element_ids.iter().map(String::as_str).collect();
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
        _ => InterpretResult::Command(Box::new(CompositeCommand::new(commands, "Delete Elements"))),
    }
}

fn has_selected_ancestor(
    root: &ElementNode,
    target: &str,
    selected_set: &std::collections::HashSet<&str>,
) -> bool {
    assert!(!target.is_empty(), "has_selected_ancestor: empty target id");
    const MAX_FRAMES: usize = 4_096;

    let mut stack: Vec<(&ElementNode, usize)> = Vec::with_capacity(16);
    stack.push((root, 0));
    let mut iter: usize = 0;
    while let Some((node, ancestor_hits)) = stack.pop() {
        assert!(
            iter < MAX_FRAMES,
            "has_selected_ancestor: depth bound exceeded"
        );
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

fn build_slide_list_data(deck: &Deck, active_slide: Option<&SlideId>) -> SlideListData {
    let mut slides: Vec<SlideListEntry> = Vec::with_capacity(deck.slide_order.len());
    let count: usize = deck.slide_order.len();
    let date: String = crate::html::serialize::today_ymd();
    for (idx, sid) in deck.slide_order.iter().enumerate() {
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
        let (fill, img) = deck.effective_slide_bg(slide);
        let opts: crate::html::serialize::RenderOpts = crate::html::serialize::RenderOpts {
            ctx: Some(crate::html::serialize::RenderCtx {
                number: idx + 1,
                count,
                date: date.clone(),
            }),
            hide_placeholders: false,
            min_element_size: 0.0,
        };
        let html: String = serialize_slide_themed(slide, fill.as_deref(), img.as_deref(), &opts);
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

fn build_slide_inspector_data(deck: &Deck, active: Option<&SlideId>) -> Option<SlideInspectorData> {
    let sid: &SlideId = active?;
    let entry = deck.manifest.slides.iter().find(|e| &e.id == sid);
    let title: String = entry.map(|e| e.title.clone()).unwrap_or_default();
    let notes: String = entry.and_then(|e| e.notes.clone()).unwrap_or_default();
    let layout_id: String = entry
        .map(|e| e.layout_id.clone())
        .or_else(|| deck.slides.get(sid).map(|s| s.layout_id.clone()))
        .unwrap_or_default();
    let background: String = deck
        .slides
        .get(sid)
        .and_then(|s| s.metadata.background.clone())
        .unwrap_or_default();
    let background_image: String = deck
        .slides
        .get(sid)
        .and_then(|s| s.metadata.background_image.clone())
        .unwrap_or_default();
    let transition: Option<crate::deck::SlideTransition> = deck
        .slides
        .get(sid)
        .and_then(|s| s.metadata.transition.clone());
    let layouts: Vec<SlideInspectorLayout> = deck
        .theme
        .layout_order
        .iter()
        .filter_map(|lid| {
            deck.theme.layouts.get(lid).map(|l| SlideInspectorLayout {
                id: lid.clone(),
                name: l.name.clone(),
            })
        })
        .collect();
    Some(SlideInspectorData {
        slide_id: sid.clone(),
        title,
        notes,
        background,
        background_image,
        transition,
        layout_id,
        layouts,
    })
}

fn empty_to_none(s: String) -> Option<String> {
    if s.trim().is_empty() { None } else { Some(s) }
}

fn build_image_element_from_asset(
    entry: &AssetEntry,
    natural_w: u32,
    natural_h: u32,
    drop_position: Option<Point>,
    slide_dims: (u32, u32),
) -> ElementNode {
    assert!(
        !entry.id.is_empty(),
        "build_image_element_from_asset: empty asset id"
    );
    let (slide_w, slide_h) = slide_dims;
    let width: f64 = if natural_w > 0 {
        natural_w as f64
    } else {
        320.0
    };
    let height: f64 = if natural_h > 0 {
        natural_h as f64
    } else {
        180.0
    };
    let (px, py) = match drop_position {
        Some(p) => (p.x - width / 2.0, p.y - height / 2.0),
        None => (
            (slide_w as f64 - width) / 2.0,
            (slide_h as f64 - height) / 2.0,
        ),
    };
    let mut inline_styles: BTreeMap<String, String> = BTreeMap::new();
    inline_styles.insert(
        "background-image".into(),
        format!("var(--asset-{})", entry.id),
    );
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
        content: ElementContent::Image(AssetRef {
            asset_id: entry.id.clone(),
        }),
        children: vec![],
        placeholder_fill: None,
        placeholder: false,
        name: None,
        link: None,
        attributes: BTreeMap::new(),
        inline_styles,
    }
}

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

fn interpret_reparent_request(
    active: Option<CanvasTarget>,
    element_id: ElementId,
    new_parent_id: ElementId,
    new_position: usize,
) -> InterpretResult {
    assert!(
        !element_id.is_empty(),
        "interpret_reparent_request: empty element id"
    );
    assert!(
        !new_parent_id.is_empty(),
        "interpret_reparent_request: empty parent id"
    );
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
            warn!(
                "InsertElementRequested with unknown element_type: {}",
                element_type
            );
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

fn sanitize_element_id(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<&str>>().join("_")
}

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

fn build_set_text_command(
    dispatcher: &CommandDispatcher,
    active: Option<CanvasTarget>,
    element_id: ElementId,
    new_content: RichText,
) -> Option<Box<dyn Command>> {
    let target: CanvasTarget = active?;
    assert!(
        !target.id().is_empty(),
        "build_set_text_command: active canvas id is empty"
    );
    let current: RichText = read_text_content(dispatcher, &target, &element_id)?;
    if current == new_content {
        return None;
    }
    Some(Box::new(SetTextContent {
        target,
        element_id,
        new_content,
    }))
}

fn read_text_content(
    dispatcher: &CommandDispatcher,
    target: &CanvasTarget,
    element_id: &ElementId,
) -> Option<RichText> {
    let canvas = dispatcher.deck().canvas(target)?;
    match &canvas.find_element(element_id)?.content {
        ElementContent::Text(rt) => Some(rt.clone()),
        _ => None,
    }
}

fn build_set_embed_command(
    dispatcher: &CommandDispatcher,
    active: Option<CanvasTarget>,
    element_id: ElementId,
    new_html: String,
) -> Option<Box<dyn Command>> {
    let target: CanvasTarget = active?;
    assert!(
        !target.id().is_empty(),
        "build_set_embed_command: active canvas id is empty"
    );
    let canvas = dispatcher.deck().canvas(&target)?;
    let element = canvas.find_element(&element_id)?;
    let current: &str = match &element.content {
        ElementContent::Embed(html) => html.as_str(),
        _ => return None,
    };
    if current == new_html {
        return None;
    }
    Some(Box::new(SetEmbedHtml {
        target,
        element_id,
        new_html,
    }))
}

fn parse_group_dir_opt(s: Option<&str>) -> Option<crate::deck::style::GroupDirection> {
    use crate::deck::style::GroupDirection::*;
    match s {
        Some("row") => Some(Row),
        Some("column") => Some(Column),
        _ => None,
    }
}
/// Maps a wire token from the inspector's align row onto an `AlignOp`.
///
/// Input: one of `left | h-center | right | top | v-center | bottom |
/// distribute-h | distribute-v`. Output: the operation, or `None` for any
/// other token so an unknown request is ignored rather than guessed at.
fn parse_align_op(s: &str) -> Option<AlignOp> {
    match s {
        "left" => Some(AlignOp::Align(AlignAxis::Left)),
        "h-center" => Some(AlignOp::Align(AlignAxis::HCenter)),
        "right" => Some(AlignOp::Align(AlignAxis::Right)),
        "top" => Some(AlignOp::Align(AlignAxis::Top)),
        "v-center" => Some(AlignOp::Align(AlignAxis::VCenter)),
        "bottom" => Some(AlignOp::Align(AlignAxis::Bottom)),
        "distribute-h" => Some(AlignOp::Distribute(DistributeAxis::Horizontal)),
        "distribute-v" => Some(AlignOp::Distribute(DistributeAxis::Vertical)),
        _ => None,
    }
}

fn parse_group_dist_opt(s: Option<&str>) -> Option<crate::deck::style::GroupDistribution> {
    use crate::deck::style::GroupDistribution::*;
    match s {
        Some("none") => Some(None),
        Some("start") => Some(Start),
        Some("center") => Some(Center),
        Some("end") => Some(End),
        Some("space-between") => Some(SpaceBetween),
        Some("space-around") => Some(SpaceAround),
        Some("space-evenly") => Some(SpaceEvenly),
        _ => Option::None,
    }
}
fn parse_group_align_opt(s: Option<&str>) -> Option<crate::deck::style::GroupAlignment> {
    use crate::deck::style::GroupAlignment::*;
    match s {
        Some("none") => Some(None),
        Some("start") => Some(Start),
        Some("center") => Some(Center),
        Some("end") => Some(End),
        _ => Option::None,
    }
}

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
            let keyframe = if cat == AnimationCategory::Entrance {
                "appear"
            } else {
                "disappear"
            };
            let entry = AnimationEntry::new(
                new_animation_id(),
                element_id,
                AnimationEffect::Named(keyframe.to_string()),
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
        (false, Some(id)) => InterpretResult::Command(Box::new(RemoveAnimation {
            slide_id,
            animation_id: id,
        })),
        _ => InterpretResult::Nothing,
    }
}

fn interpret_add_animation(
    deck: &Deck,
    mode: EditorMode,
    active_slide: Option<&SlideId>,
    element_id: ElementId,
    catalog_id: &str,
    direction: Option<&str>,
) -> InterpretResult {
    assert!(
        !element_id.is_empty(),
        "interpret_add_animation: empty element id"
    );
    if mode != EditorMode::Slide {
        return InterpretResult::Nothing;
    }
    let slide_id: SlideId = match active_slide {
        Some(s) => s.clone(),
        None => return InterpretResult::Nothing,
    };
    let item = match crate::deck::anim_catalog::animation_catalog()
        .into_iter()
        .find(|i| i.id == catalog_id)
    {
        Some(i) => i,
        None => return InterpretResult::Nothing,
    };
    let category = match item.category.as_str() {
        "entrance" => AnimationCategory::Entrance,
        "emphasis" => AnimationCategory::Emphasis,
        "exit" => AnimationCategory::Exit,
        "property" => AnimationCategory::Property,
        _ => return InterpretResult::Nothing,
    };
    let effect: AnimationEffect = if item.kind == "property" {
        AnimationEffect::PropertyChange(vec![PropertyTarget {
            property: "opacity".into(),
            value: "1".into(),
        }])
    } else {
        AnimationEffect::Named(directional_keyframe(&item, direction))
    };
    let slide = match deck.slides.get(&slide_id) {
        Some(s) => s,
        None => return InterpretResult::Nothing,
    };
    let entry = AnimationEntry::new(
        new_animation_id(),
        element_id,
        effect,
        category,
        AnimationTrigger::OnClick,
        AnimationTiming::default(),
    );
    InterpretResult::Command(Box::new(InsertAnimation {
        slide_id,
        position: slide.animations.len(),
        entry,
    }))
}

fn directional_keyframe(
    item: &crate::deck::anim_catalog::AnimCatalogItem,
    dir: Option<&str>,
) -> String {
    let base: &str = item.keyframe.as_deref().unwrap_or("appear");
    if !item.directional {
        return base.to_string();
    }
    let d: &str = dir.unwrap_or("top");
    let prefix: &str = if base.starts_with("fly-out") {
        "fly-out"
    } else {
        "fly-in"
    };
    assert!(
        matches!(d, "top" | "bottom" | "left" | "right"),
        "bad direction"
    );
    format!("{}-{}", prefix, d)
}

#[allow(clippy::too_many_arguments)]
fn interpret_update_animation(
    deck: &Deck,
    active_slide: Option<&SlideId>,
    animation_id: &str,
    trigger: Option<&str>,
    duration_ms: Option<u32>,
    delay_ms: Option<u32>,
    easing: Option<&str>,
    iterations: Option<crate::deck::animation::AnimationIterations>,
    targets: Option<Vec<PropertyTarget>>,
) -> InterpretResult {
    assert!(
        !animation_id.is_empty(),
        "interpret_update_animation: empty id"
    );
    let slide_id: SlideId = match active_slide {
        Some(s) => s.clone(),
        None => return InterpretResult::Nothing,
    };
    let slide = match deck.slides.get(&slide_id) {
        Some(s) => s,
        None => return InterpretResult::Nothing,
    };
    let prior = match slide.animations.iter().find(|e| e.id == animation_id) {
        Some(e) => e.clone(),
        None => return InterpretResult::Nothing,
    };
    let trig = match trigger {
        Some("on_click") => AnimationTrigger::OnClick,
        Some("with_previous") => AnimationTrigger::WithPrevious,
        Some("after_previous") => AnimationTrigger::AfterPrevious,
        _ => prior.trigger,
    };
    let timing = AnimationTiming {
        duration_ms: duration_ms.unwrap_or(prior.timing.duration_ms),
        delay_ms: delay_ms.unwrap_or(prior.timing.delay_ms),
        easing: easing
            .map(str::to_string)
            .unwrap_or_else(|| prior.timing.easing.clone()),
        iterations: iterations.unwrap_or(prior.timing.iterations),
    };
    let effect = match (targets, prior.category) {
        (Some(t), AnimationCategory::Property) if !t.is_empty() => {
            AnimationEffect::PropertyChange(t)
        }
        _ => prior.effect.clone(),
    };
    let new_entry = AnimationEntry::new(
        prior.id.clone(),
        prior.element_id.clone(),
        effect,
        prior.category,
        trig,
        timing,
    );
    InterpretResult::Command(Box::new(SetAnimationProperty {
        slide_id,
        animation_id: animation_id.to_string(),
        new_entry,
    }))
}

fn interpret_move_animation(
    deck: &Deck,
    active_slide: Option<&SlideId>,
    animation_id: &str,
    new_index: usize,
    trigger: &str,
) -> InterpretResult {
    assert!(
        !animation_id.is_empty(),
        "interpret_move_animation: empty id"
    );
    let slide_id: SlideId = match active_slide {
        Some(s) => s.clone(),
        None => return InterpretResult::Nothing,
    };
    let slide = match deck.slides.get(&slide_id) {
        Some(s) => s,
        None => return InterpretResult::Nothing,
    };
    let prior = match slide.animations.iter().find(|e| e.id == animation_id) {
        Some(e) => e.clone(),
        None => return InterpretResult::Nothing,
    };
    let trig = match trigger {
        "on_click" => AnimationTrigger::OnClick,
        "with_previous" => AnimationTrigger::WithPrevious,
        "after_previous" => AnimationTrigger::AfterPrevious,
        _ => prior.trigger,
    };
    let new_entry = AnimationEntry::new(
        prior.id.clone(),
        prior.element_id.clone(),
        prior.effect.clone(),
        prior.category,
        trig,
        prior.timing.clone(),
    );
    let cmds: Vec<Box<dyn Command>> = vec![
        Box::new(crate::commands::ReorderAnimation {
            slide_id: slide_id.clone(),
            animation_id: animation_id.to_string(),
            new_position: new_index,
        }),
        Box::new(SetAnimationProperty {
            slide_id,
            animation_id: animation_id.to_string(),
            new_entry,
        }),
    ];
    InterpretResult::Command(Box::new(CompositeCommand::new(cmds, "Move Animation")))
}

fn interpret_scale_elements(
    deck: &Deck,
    target_opt: Option<CanvasTarget>,
    ids: &[ElementId],
    factor: f64,
    anchor: Point,
) -> InterpretResult {
    let target: CanvasTarget = match target_opt {
        Some(t) => t,
        None => return InterpretResult::Nothing,
    };
    match factor.partial_cmp(&0.0) {
        Some(std::cmp::Ordering::Greater) => {}
        _ => return InterpretResult::Nothing,
    }
    let canvas = match deck.canvas(&target) {
        Some(c) => c,
        None => return InterpretResult::Nothing,
    };
    let mut items: Vec<ElementTransform> = Vec::with_capacity(ids.len());
    for id in ids {
        let el = match canvas.find_element(id) {
            Some(e) => e,
            None => continue,
        };
        let g = &el.geometry;
        let font_size_px = match &el.style {
            ElementStyle::Text(ts) => Some(ts.font_size.value * factor),
            _ => None,
        };
        let group_scale = match &el.style {
            ElementStyle::Group(gs) => Some(gs.scale * factor),
            _ => None,
        };
        items.push(ElementTransform {
            id: id.clone(),
            x: anchor.x + (g.x - anchor.x) * factor,
            y: anchor.y + (g.y - anchor.y) * factor,
            width: g.width * factor,
            height: g.height * factor,
            font_size_px,
            group_scale,
        });
    }
    if items.is_empty() {
        return InterpretResult::Nothing;
    }
    InterpretResult::Command(Box::new(SetElementsTransform { target, items }))
}

fn build_insert_slide_after_active(
    dispatcher: &CommandDispatcher,
    active_slide: Option<&SlideId>,
    layout_id: &str,
) -> Option<(Box<dyn Command>, SlideId)> {
    use crate::bundle::SlideEntry;
    use crate::bundle::manifest::slide_path_for;
    use crate::deck::builders::group_element;
    use crate::deck::new_slide_id;

    let order: &[SlideId] = &dispatcher.deck().slide_order;
    let position: usize = match active_slide {
        Some(id) => order
            .iter()
            .position(|s| s == id)
            .map(|i| i + 1)
            .unwrap_or(order.len()),
        None => order.len(),
    };

    let layout = dispatcher.deck().theme.layouts.get(layout_id);
    let seed_layout: String = if layout.is_some() {
        layout_id.to_string()
    } else {
        "blank".into()
    };
    let children: Vec<ElementNode> = layout
        .map(|l| {
            l.root
                .children
                .iter()
                .map(|c| {
                    let mut copy: ElementNode = c.clone();
                    crate::deck::element::regenerate_ids(&mut copy);
                    copy
                })
                .collect()
        })
        .unwrap_or_default();

    let slide_id: SlideId = new_slide_id();
    assert!(
        !slide_id.is_empty(),
        "build_insert_slide_after_active: minted empty slide id"
    );
    let root: ElementNode = group_element(new_element_id(), children);
    let slide: SlideNode = SlideNode::new(slide_id.clone(), seed_layout.clone(), root);

    let manifest_entry: SlideEntry = SlideEntry {
        id: slide_id.clone(),
        path: slide_path_for(&slide_id),
        layout_id: seed_layout,
        title: format!("Slide {}", position + 1),
        thumbnail: None,
        transition: None,
        duration_hint: None,
        notes_ref: None,
        animations: Vec::new(),
        guides: Vec::new(),
        background: None,
        background_image: None,
        notes: None,
    };

    let cmd: Box<dyn Command> = Box::new(InsertSlide {
        position,
        slide,
        manifest_entry,
    });
    Some((cmd, slide_id))
}

fn __new_slide_command(position: usize, children: Vec<ElementNode>) -> Box<dyn Command> {
    use crate::bundle::SlideEntry;
    use crate::bundle::manifest::slide_path_for;
    use crate::deck::builders::group_element;
    use crate::deck::new_slide_id;

    assert!(
        !children.is_empty(),
        "__new_slide_command: empty slide children"
    );
    let slide_id: SlideId = new_slide_id();
    assert!(
        !slide_id.is_empty(),
        "__new_slide_command: minted empty slide id"
    );
    let root: ElementNode = group_element(new_element_id(), children);
    let slide: SlideNode = SlideNode::new(slide_id.clone(), "blank".to_string(), root);
    let manifest_entry: SlideEntry = SlideEntry {
        id: slide_id.clone(),
        path: slide_path_for(&slide_id),
        layout_id: "blank".to_string(),
        title: String::new(),
        thumbnail: None,
        transition: None,
        duration_hint: None,
        notes_ref: None,
        animations: Vec::new(),
        guides: Vec::new(),
        background: None,
        background_image: None,
        notes: None,
    };
    Box::new(InsertSlide {
        position,
        slide,
        manifest_entry,
    })
}

fn build_insert_layout_after_active(
    dispatcher: &CommandDispatcher,
    active_layout: Option<&LayoutId>,
) -> Option<(Box<dyn Command>, LayoutId)> {
    use crate::deck::builders::group_element;

    let order: &[LayoutId] = &dispatcher.deck().theme.layout_order;
    let position: usize = match active_layout {
        Some(id) => order
            .iter()
            .position(|l| l == id)
            .map(|i| i + 1)
            .unwrap_or(order.len()),
        None => order.len(),
    };

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
    assert!(
        !layout_id.is_empty(),
        "build_insert_layout_after_active: minted empty id"
    );

    let root: ElementNode = group_element(new_element_id(), vec![]);
    let layout: LayoutNode = LayoutNode::new(layout_id.clone(), name, root);
    let cmd: Box<dyn Command> = Box::new(InsertLayout { position, layout });
    Some((cmd, layout_id))
}

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
        let transient: SlideNode = layout.preview_slide();
        layouts.push(LayoutListEntry {
            layout_id: lid.clone(),
            name: layout.name.clone(),
            html: serialize_slide(&transient),
            background: layout.background.clone().unwrap_or_default(),
            background_image: layout.background_image.clone().unwrap_or_default(),
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

fn construct_default_element_for_type(element_type: &str) -> Option<ElementNode> {
    match element_type {
        "text" => Some(default_text_element()),
        "shape" => Some(default_shape_element()),
        "group" => Some(default_group_element()),
        "embed" => Some(default_embed_element()),
        "table" => Some(default_table_element()),
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
        placeholder: false,
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
        placeholder: false,
        name: None,
        link: None,
        attributes: BTreeMap::new(),
        inline_styles: {
            let mut m: BTreeMap<String, String> = BTreeMap::new();

            m.insert(
                "background-color".into(),
                "var(--theme-accent, #0066ff)".into(),
            );
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
        style: ElementStyle::Group(crate::deck::style::GroupStyle::default()),
        content: ElementContent::Group,
        children: vec![],
        placeholder_fill: None,
        placeholder: false,
        name: None,
        link: None,
        attributes: BTreeMap::new(),
        inline_styles: BTreeMap::new(),
    }
}

fn default_table_element() -> ElementNode {
    use crate::deck::element::{TableCell, TableData};
    let rows: usize = 3;
    let columns: usize = 3;
    let cell = |text: &str| TableCell {
        content: RichText::new(text),
        style_overrides: BTreeMap::new(),
        colspan: 1,
        rowspan: 1,
    };
    let mut cells: Vec<Vec<TableCell>> = Vec::with_capacity(rows);
    for r in 0..rows {
        let mut row: Vec<TableCell> = Vec::with_capacity(columns);
        for c in 0..columns {
            let text: String = if r == 0 {
                format!("Header {}", c + 1)
            } else {
                String::new()
            };
            row.push(cell(&text));
        }
        cells.push(row);
    }
    let data = TableData {
        rows,
        columns,
        cells,
        header_rows: 1,
        header_columns: 0,
    };
    ElementNode {
        id: new_element_id(),
        element_type: ElementType::Table,
        geometry: Geometry {
            x: 660.0,
            y: 440.0,
            width: 600.0,
            height: 200.0,
            ..Geometry::default()
        },
        style: ElementStyle::Table(crate::deck::style::TableStyle::default()),
        content: ElementContent::Table(data),
        children: vec![],
        placeholder_fill: None,
        placeholder: false,
        name: None,
        link: None,
        attributes: BTreeMap::new(),
        inline_styles: BTreeMap::new(),
    }
}

fn default_embed_element() -> ElementNode {
    let id: ElementId = new_element_id();
    let placeholder: &str = "<div style=\"font:14px ui-monospace,monospace;color:var(--theme-muted,#888);\
padding:12px;\">&lt;!-- HTML block: double-click to edit --&gt;</div>";
    ElementNode {
        id,
        element_type: ElementType::Embed,
        geometry: Geometry {
            x: 760.0,
            y: 465.0,
            width: 400.0,
            height: 200.0,
            ..Geometry::default()
        },
        style: ElementStyle::Embed,
        content: ElementContent::Embed(placeholder.to_string()),
        children: vec![],
        placeholder_fill: None,
        placeholder: false,
        name: None,
        link: None,
        attributes: BTreeMap::new(),
        inline_styles: {
            let mut m: BTreeMap<String, String> = BTreeMap::new();
            m.insert(
                "border".into(),
                "1px dashed var(--theme-muted, #888)".into(),
            );
            m.insert("overflow".into(), "auto".into());
            m
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::ipc::{Modifiers, Vec2};

    fn modifiers_default() -> Modifiers {
        Modifiers::default()
    }

    fn modifiers_shift() -> Modifiers {
        Modifiers {
            shift: true,
            ..Modifiers::default()
        }
    }

    fn fixture() -> (CommandDispatcher, SelectionState, SlideId, ElementId) {
        let deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let eid: ElementId = deck.slides[&sid].root.children[0].id.clone();
        let dispatcher = CommandDispatcher::new(deck);
        (dispatcher, SelectionState::empty(), sid, eid)
    }

    fn interpret_inline(
        dispatcher: &CommandDispatcher,
        selection: &SelectionState,
        active_slide: &Option<SlideId>,
        event: InteractionEvent,
    ) -> InterpretResult {
        match event {
            InteractionEvent::ElementClicked {
                element_id,
                modifiers,
                ..
            } => {
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
                if let Some(sid) = active_slide.clone()
                    && let Some(slide) = dispatcher.deck().slides.get(&sid)
                    && let Some(el) = slide.find_element(&element_id)
                {
                    snap.record_geometry(CanvasTarget::Slide(sid), element_id, el.geometry.clone());
                }
                InterpretResult::TransactionBegin {
                    label: DRAG_TRANSACTION_LABEL,
                    snapshot: snap,
                }
            }
            InteractionEvent::ElementDragged { .. } => InterpretResult::Nothing,
            InteractionEvent::ElementDragEnded { element_id, delta } => {
                let sid = match active_slide.clone() {
                    Some(s) => s,
                    None => return InterpretResult::Nothing,
                };
                let (sx, sy) = match dispatcher.transaction().and_then(|t| {
                    t.start_snapshot
                        .position_of(&CanvasTarget::Slide(sid.clone()), &element_id)
                }) {
                    Some(p) => p,
                    None => return InterpretResult::Nothing,
                };
                InterpretResult::CommitTransactionWith(Box::new(MoveElement {
                    target: CanvasTarget::Slide(sid),
                    element_id,
                    new_position: Point {
                        x: sx + delta.x,
                        y: sy + delta.y,
                    },
                    previous_position: None,
                }))
            }
            InteractionEvent::ElementResizeStarted { element_id, .. } => {
                let mut snap = TransactionSnapshot::empty();
                if let Some(sid) = active_slide.clone()
                    && let Some(slide) = dispatcher.deck().slides.get(&sid)
                    && let Some(el) = slide.find_element(&element_id)
                {
                    snap.record_geometry(CanvasTarget::Slide(sid), element_id, el.geometry.clone());
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
                background_size,
                background_position,
            } => {
                let sid = match active_slide.clone() {
                    Some(s) => s,
                    None => return InterpretResult::Nothing,
                };
                if dispatcher
                    .transaction()
                    .and_then(|t| {
                        t.start_snapshot
                            .position_of(&CanvasTarget::Slide(sid.clone()), &element_id)
                    })
                    .is_none()
                {
                    return InterpretResult::Nothing;
                }
                InterpretResult::CommitTransactionWith(resize_commit_command(
                    CanvasTarget::Slide(sid),
                    element_id,
                    new_position,
                    new_size,
                    background_size,
                    background_position,
                ))
            }
            InteractionEvent::ElementCropCommitted {
                element_id,
                new_position,
                new_size,
                background_size,
                background_position,
            } => interpret_crop_committed(
                active_slide.clone().map(CanvasTarget::Slide),
                element_id,
                new_position,
                new_size,
                background_size,
                background_position,
            ),

            InteractionEvent::CopyRequested { .. }
            | InteractionEvent::CutRequested { .. }
            | InteractionEvent::PasteRequested
            | InteractionEvent::RemoveSlideRequested { .. } => InterpretResult::Nothing,
            InteractionEvent::BackgroundClicked { .. } => {
                InterpretResult::Selection(SelectionState::empty())
            }
            InteractionEvent::PropertyChanged {
                element_id,
                property,
                value,
            } => interpret_property_changed(
                active_slide.clone().map(CanvasTarget::Slide),
                element_id,
                property,
                value,
            ),
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
            InteractionEvent::RenameElementRequested {
                element_id,
                new_name,
            } => interpret_rename_request(
                active_slide.clone().map(CanvasTarget::Slide),
                element_id,
                new_name,
            ),
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
            InteractionEvent::KeyPressed { ref key, .. } if key == EXPORT_HTML_KEY => {
                InterpretResult::FileAction(FileAction::ExportHtml)
            }
            InteractionEvent::KeyPressed { ref key, .. } if key == EXPORT_PDF_KEY => {
                InterpretResult::FileAction(FileAction::ExportPdf)
            }
            InteractionEvent::KeyPressed { ref key, .. }
                if key == DELETE_KEY_BACKSPACE || key == DELETE_KEY_DELETE =>
            {
                interpret_delete_selection(
                    dispatcher,
                    active_slide.clone().map(CanvasTarget::Slide),
                    selection,
                )
            }

            InteractionEvent::NudgeSelectionRequested { dx, dy } => {
                let target: CanvasTarget = match active_slide.clone() {
                    Some(s) => CanvasTarget::Slide(s),
                    None => return InterpretResult::Nothing,
                };
                let canvas = match dispatcher.deck().canvas(&target) {
                    Some(c) => c,
                    None => return InterpretResult::Nothing,
                };
                let mut cmds: Vec<Box<dyn Command>> = Vec::new();
                for id in &selection.element_ids {
                    if let Some(el) = canvas.find_element(id) {
                        cmds.push(Box::new(MoveElement {
                            target: target.clone(),
                            element_id: id.clone(),
                            new_position: Point {
                                x: el.geometry.x + dx,
                                y: el.geometry.y + dy,
                            },
                            previous_position: None,
                        }));
                    }
                }
                if cmds.is_empty() {
                    return InterpretResult::Nothing;
                }
                InterpretResult::Command(Box::new(CompositeCommand::new(cmds, "Nudge Elements")))
            }

            InteractionEvent::NavigateSlideRequested { forward } => {
                let order: &[SlideId] = &dispatcher.deck().slide_order;
                let cur: usize = match active_slide
                    .as_ref()
                    .and_then(|sid| order.iter().position(|s| s == sid))
                {
                    Some(i) => i,
                    None => return InterpretResult::Nothing,
                };
                let next: usize = if forward {
                    cur + 1
                } else {
                    cur.wrapping_sub(1)
                };
                match order.get(next) {
                    Some(sid) if forward || cur > 0 => InterpretResult::SetActiveSlide(sid.clone()),
                    _ => InterpretResult::Nothing,
                }
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
        let event = InteractionEvent::BackgroundClicked {
            position: Point { x: 0.0, y: 0.0 },
        };
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
                assert!(
                    snapshot
                        .position_of(&CanvasTarget::Slide(sid.clone()), &eid)
                        .is_some()
                );
            }
            other => panic!("expected TransactionBegin, got {other:?}"),
        }
    }

    #[test]
    fn drag_dragged_is_a_no_op_on_rust_side() {
        let (mut d, sel, sid, eid) = fixture();
        let geo = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
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
        let geo = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
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
                    if let Patch::SetStyle {
                        property, value, ..
                    } = p
                    {
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

        let cmd = MoveElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_position: Point {
                x: start_geo.x + 100.0,
                y: start_geo.y + 200.0,
            },
            previous_position: None,
        };
        d.dispatch(Box::new(cmd)).unwrap();

        d.commit_transaction().unwrap();
        let after = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
        assert_eq!(after.x, start_geo.x + 100.0);
        assert_eq!(after.y, start_geo.y + 200.0);
    }

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
        let (d, sel, sid, _) = fixture();
        let event = InteractionEvent::KeyPressed {
            key: UNDO_KEY.into(),
            modifiers: Modifiers {
                meta: true,
                ..Modifiers::default()
            },
        };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::Undo => {}
            other => panic!("expected Undo, got {other:?}"),
        }
    }

    #[test]
    fn dispatcher_undo_after_dispatch_restores_geometry() {
        let (mut d, _sel, sid, eid) = fixture();
        let original = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
        d.dispatch(Box::new(MoveElement {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_position: Point {
                x: original.x + 444.0,
                y: original.y + 222.0,
            },
            previous_position: None,
        }))
        .unwrap();
        let _ = d.take_patches();
        d.undo().unwrap().expect("undo not a no-op");
        let geo = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
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
        let geo = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
        assert_eq!(geo.x, 21.0);
        assert_eq!(geo.y, 84.0);
    }

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
        let (mut d, _sel, sid, eid) = fixture();
        let start = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();

        let mut snap = TransactionSnapshot::empty();
        snap.record_geometry(CanvasTarget::Slide(sid.clone()), eid.clone(), start.clone());
        d.begin_transaction(DRAG_TRANSACTION_LABEL, snap);
        let mut step: f64 = 0.0;
        while step < 32.0 {
            d.dispatch(Box::new(MoveElement {
                target: CanvasTarget::Slide(sid.clone()),
                element_id: eid.clone(),
                new_position: Point {
                    x: start.x + step,
                    y: start.y,
                },
                previous_position: None,
            }))
            .unwrap();
            step += 1.0;
        }
        d.commit_transaction().unwrap();
        let _ = d.take_patches();

        assert_eq!(d.history().undo_len(), 1);
        d.undo().unwrap().expect("undo not a no-op");
        let geo = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
        assert_eq!(geo.x, start.x);
        assert_eq!(geo.y, start.y);
    }

    fn run_property_changed(prop: &str, value: &str) -> (InterpretResult, SlideId, ElementId) {
        let (d, sel, sid, eid) = fixture();
        let event = InteractionEvent::PropertyChanged {
            element_id: eid.clone(),
            property: prop.into(),
            value: value.into(),
        };
        (
            interpret_inline(&d, &sel, &Some(sid.clone()), event),
            sid,
            eid,
        )
    }

    #[test]
    fn property_changed_x_routes_to_set_geometry_property() {
        let (result, sid, eid) = run_property_changed("x", "250");
        match result {
            InterpretResult::Command(cmd) => {
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
                    deck.slides[&sid]
                        .find_element(&eid)
                        .unwrap()
                        .geometry
                        .opacity,
                    0.5
                );
            }
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn collect_copy_returns_elements_when_selected_else_slide() {
        let (d, _sel, sid, eid) = fixture();
        let target = CanvasTarget::Slide(sid.clone());
        let mut sel = SelectionState::empty();
        sel.slide_id = Some(sid.clone());
        sel.element_ids = vec![eid.clone()];
        match collect_copy(
            crate::ipc::ClipboardScope::Elements,
            Some(target.clone()),
            &sel,
            Some(&sid),
            d.deck(),
        ) {
            Some(Clipboard::Elements(v)) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].id, eid);
            }
            _ => panic!("expected Elements"),
        }
        let empty = SelectionState::empty();
        match collect_copy(
            crate::ipc::ClipboardScope::Slide,
            Some(target),
            &empty,
            Some(&sid),
            d.deck(),
        ) {
            Some(Clipboard::Slide(s)) => assert_eq!(s.id, sid),
            _ => panic!("expected Slide"),
        }
    }

    #[test]
    fn paste_elements_inserts_clones_with_fresh_ids_same_geometry() {
        let (d, _sel, sid, eid) = fixture();
        let target = CanvasTarget::Slide(sid.clone());
        let source = d
            .deck()
            .canvas(&target)
            .unwrap()
            .find_element(&eid)
            .unwrap()
            .clone();
        let clip = Clipboard::Elements(vec![source.clone()]);
        let (cmd, outcome) = build_paste_command(Some(target.clone()), &clip, d.deck()).unwrap();
        let mut deck = d.deck().clone();
        cmd.apply(&mut deck).unwrap();
        let root = deck.canvas(&target).unwrap().root();
        let pasted = root.children.last().unwrap();
        assert_ne!(pasted.id, eid);
        assert_eq!(pasted.geometry, source.geometry);
        match outcome {
            PasteOutcome::Elements(ids) => assert_eq!(ids, vec![pasted.id.clone()]),
            _ => panic!("expected Elements outcome"),
        }
    }

    #[test]
    fn paste_slide_inserts_after_active_with_fresh_ids() {
        let (d, _sel, sid, _eid) = fixture();
        let slide = d.deck().slides.get(&sid).unwrap().clone();
        let clip = Clipboard::Slide(Box::new(slide.clone()));
        let (cmd, outcome) =
            build_paste_command(Some(CanvasTarget::Slide(sid.clone())), &clip, d.deck()).unwrap();
        let mut deck = d.deck().clone();
        let before = deck.slide_order.len();
        cmd.apply(&mut deck).unwrap();
        assert_eq!(deck.slide_order.len(), before + 1);
        match outcome {
            PasteOutcome::Slide(new_id) => {
                assert!(deck.slides.contains_key(&new_id));
                assert_ne!(new_id, sid);
                assert_ne!(deck.slides[&new_id].root.id, slide.root.id);
            }
            _ => panic!("expected Slide outcome"),
        }
    }

    #[test]
    fn cut_removal_removes_selected_elements() {
        let (d, _sel, sid, eid) = fixture();
        let mut sel = SelectionState::empty();
        sel.slide_id = Some(sid.clone());
        sel.element_ids = vec![eid.clone()];
        let cmd = build_cut_removal(
            crate::ipc::ClipboardScope::Elements,
            Some(CanvasTarget::Slide(sid.clone())),
            &sel,
            Some(&sid),
            d.deck(),
        )
        .unwrap();
        assert_eq!(cmd.label(), "Cut");
        let mut deck = d.deck().clone();
        cmd.apply(&mut deck).unwrap();
        assert!(
            deck.canvas(&CanvasTarget::Slide(sid.clone()))
                .unwrap()
                .find_element(&eid)
                .is_none()
        );
    }

    #[test]
    fn cut_removal_guards_the_last_slide() {
        let (d, _sel, sid, _eid) = fixture();
        let empty = SelectionState::empty();

        assert!(
            build_cut_removal(
                crate::ipc::ClipboardScope::Slide,
                Some(CanvasTarget::Slide(sid.clone())),
                &empty,
                Some(&sid),
                d.deck(),
            )
            .is_none()
        );
    }

    #[test]
    fn remove_slide_guards_last_slide_and_unknown() {
        let (d, _sel, sid, _eid) = fixture();
        match interpret_remove_slide(d.deck(), &sid) {
            InterpretResult::Nothing => {}
            _ => panic!("expected Nothing for last slide"),
        }
        match interpret_remove_slide(d.deck(), &"nope".to_string()) {
            InterpretResult::Nothing => {}
            _ => panic!("expected Nothing for unknown slide"),
        }
    }

    #[test]
    fn remove_slide_drops_slide_when_more_than_one() {
        let (mut d, _sel, sid, _eid) = fixture();
        let (add_cmd, new_id) = build_insert_slide_after_active(&d, Some(&sid), "").unwrap();
        d.dispatch(add_cmd).unwrap();
        match interpret_remove_slide(d.deck(), &new_id) {
            InterpretResult::Command(cmd) => {
                cmd.apply(d.deck_mut()).unwrap();
                assert!(!d.deck().slides.contains_key(&new_id));
                assert!(!d.deck().slide_order.contains(&new_id));
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn resize_commit_with_background_bundles_styles_and_geometry() {
        let (_d, _sel, sid, eid) = fixture();
        let cmd = resize_commit_command(
            CanvasTarget::Slide(sid.clone()),
            eid.clone(),
            Point { x: 5.0, y: 6.0 },
            Size {
                width: 800.0,
                height: 600.0,
            },
            Some("1200px 600px".to_string()),
            Some("-200px 0px".to_string()),
        );
        let mut deck = Deck::sample();
        cmd.apply(&mut deck).unwrap();
        let el = deck.slides[&sid].find_element(&eid).unwrap();
        assert_eq!(el.geometry.width, 800.0);
        assert_eq!(
            el.inline_styles.get("background-size").map(String::as_str),
            Some("1200px 600px")
        );
        assert_eq!(
            el.inline_styles
                .get("background-position")
                .map(String::as_str),
            Some("-200px 0px")
        );
    }

    #[test]
    fn resize_commit_without_background_is_plain_resize() {
        let (_d, _sel, sid, eid) = fixture();
        let cmd = resize_commit_command(
            CanvasTarget::Slide(sid.clone()),
            eid.clone(),
            Point { x: 5.0, y: 6.0 },
            Size {
                width: 800.0,
                height: 600.0,
            },
            None,
            None,
        );
        assert_eq!(cmd.label(), "Resize Element");
    }

    #[test]
    fn crop_committed_builds_composite_with_styles_and_geometry() {
        let (_d, _sel, sid, eid) = fixture();
        let result = interpret_crop_committed(
            Some(CanvasTarget::Slide(sid.clone())),
            eid.clone(),
            Point { x: 10.0, y: 20.0 },
            Size {
                width: 400.0,
                height: 300.0,
            },
            "600px 300px".to_string(),
            "-100px 0px".to_string(),
        );
        match result {
            InterpretResult::Command(cmd) => {
                let mut deck = Deck::sample();
                cmd.apply(&mut deck).unwrap();
                let el = deck.slides[&sid].find_element(&eid).unwrap();
                assert_eq!(
                    el.inline_styles.get("background-size").map(String::as_str),
                    Some("600px 300px")
                );
                assert_eq!(
                    el.inline_styles
                        .get("background-position")
                        .map(String::as_str),
                    Some("-100px 0px")
                );
                assert_eq!(
                    el.inline_styles.get("overflow").map(String::as_str),
                    Some("hidden")
                );
                assert_eq!(
                    el.inline_styles
                        .get("background-repeat")
                        .map(String::as_str),
                    Some("no-repeat")
                );
                assert_eq!(el.geometry.width, 400.0);
                assert_eq!(el.geometry.height, 300.0);
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
                    !d.deck().slides[&sid]
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

                assert!(
                    out.patches
                        .iter()
                        .any(|p| matches!(p, Patch::InsertElement { .. }))
                );
                let new_count = deck.slides[&sid].root.children.len();
                assert_eq!(new_count, 4);
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
                    deck.slides[&sid]
                        .find_element(&eid)
                        .unwrap()
                        .name
                        .as_deref(),
                    Some("Title")
                );
            }
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn rename_request_with_empty_name_clears() {
        let (d, sel, sid, eid) = fixture();

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
            assert_eq!(tree.nodes[i].id, slide.root.children[i].id);
        }
    }

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
        use crate::deck::builders::{group_element, text_element};
        use crate::deck::slide::SlideNode;
        use std::collections::BTreeMap;

        let inner = text_element("el_inner", "x");
        let parent = group_element("el_parent", vec![inner]);
        let root = group_element("el_root", vec![parent]);
        let slide = SlideNode::new("s".into(), "title".into(), root);
        let mut slides: BTreeMap<SlideId, SlideNode> = BTreeMap::new();
        slides.insert("s".into(), slide);
        let deck: Deck = Deck {
            slides,
            slide_order: vec!["s".into()],
            ..Default::default()
        };

        let dispatcher = crate::commands::CommandDispatcher::new(deck);
        let mut sel = SelectionState::empty();
        sel.slide_id = Some("s".into());
        sel.element_ids = vec!["el_parent".into(), "el_inner".into()];
        match interpret_inline(&dispatcher, &sel, &Some("s".into()), keypress("Backspace")) {
            InterpretResult::Command(cmd) => {
                assert_eq!(cmd.label(), "Delete Element");
            }
            other => panic!("expected single Delete Element, got {other:?}"),
        }
    }

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
        let deck: Deck = Deck {
            slides,
            slide_order: vec!["s_a".into(), "s_b".into()],
            ..Default::default()
        };
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
        let event = InteractionEvent::SlideThumbnailClicked {
            slide_id: String::new(),
        };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::Nothing => {}
            other => panic!("expected Nothing, got {other:?}"),
        }
    }

    #[test]
    fn nudge_with_selection_moves_element_by_delta() {
        let (d, _sel, sid, eid) = fixture();
        let mut sel = SelectionState::empty();
        sel.element_ids = vec![eid.clone()];
        let target = CanvasTarget::Slide(sid.clone());
        let prior_x: f64 = d
            .deck()
            .canvas(&target)
            .unwrap()
            .find_element(&eid)
            .unwrap()
            .geometry
            .x;
        let event = InteractionEvent::NudgeSelectionRequested { dx: 1.0, dy: 0.0 };
        match interpret_inline(&d, &sel, &Some(sid.clone()), event) {
            InterpretResult::Command(cmd) => {
                let mut deck = d.deck().clone();
                cmd.apply(&mut deck).unwrap();
                let moved = deck.canvas(&target).unwrap().find_element(&eid).unwrap();
                assert_eq!(moved.geometry.x, prior_x + 1.0);
            }
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn nudge_with_empty_selection_is_nothing() {
        let (d, sel, sid, _) = fixture();
        let event = InteractionEvent::NudgeSelectionRequested { dx: 0.0, dy: -1.0 };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::Nothing => {}
            other => panic!("expected Nothing, got {other:?}"),
        }
    }

    #[test]
    fn navigate_forward_moves_to_next_slide() {
        let (deck, a, b) = two_slide_deck();
        let d = CommandDispatcher::new(deck);
        let event = InteractionEvent::NavigateSlideRequested { forward: true };
        match interpret_inline(&d, &SelectionState::empty(), &Some(a), event) {
            InterpretResult::SetActiveSlide(id) => assert_eq!(id, b),
            other => panic!("expected SetActiveSlide, got {other:?}"),
        }
    }

    #[test]
    fn navigate_clamps_at_deck_ends() {
        let (deck, a, b) = two_slide_deck();
        let d = CommandDispatcher::new(deck);

        let fwd = InteractionEvent::NavigateSlideRequested { forward: true };
        match interpret_inline(&d, &SelectionState::empty(), &Some(b), fwd) {
            InterpretResult::Nothing => {}
            other => panic!("expected Nothing at last slide, got {other:?}"),
        }

        let back = InteractionEvent::NavigateSlideRequested { forward: false };
        match interpret_inline(&d, &SelectionState::empty(), &Some(a), back) {
            InterpretResult::Nothing => {}
            other => panic!("expected Nothing at first slide, got {other:?}"),
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

        for entry in &data.slides {
            assert!(entry.html.contains("data-slide-id"));
        }
    }

    #[test]
    fn build_slide_list_data_falls_back_to_id_when_title_empty() {
        let (deck, sid_a, _) = two_slide_deck();
        let data = build_slide_list_data(&deck, Some(&sid_a));

        assert_eq!(data.slides[0].title, sid_a);
    }

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
        let (deck, sid_a, sid_b) = two_slide_deck();
        let mut dispatcher = CommandDispatcher::new(deck);
        let mut active: Option<SlideId> = Some(sid_a.clone());

        let original_x = dispatcher.deck().slides[&sid_a]
            .find_element("el_a")
            .unwrap()
            .geometry
            .x;
        dispatcher
            .dispatch(Box::new(MoveElement {
                target: CanvasTarget::Slide(sid_a.clone()),
                element_id: "el_a".into(),
                new_position: Point {
                    x: original_x + 250.0,
                    y: 0.0,
                },
                previous_position: None,
            }))
            .unwrap();

        assert!(switch_active_slide_in_tree(
            &mut dispatcher,
            &mut active,
            sid_b.clone()
        ));
        assert_eq!(active.as_deref(), Some("s_b"));

        let x_after = dispatcher.deck().slides[&sid_a]
            .find_element("el_a")
            .unwrap()
            .geometry
            .x;
        assert_eq!(x_after, original_x + 250.0);

        assert!(switch_active_slide_in_tree(
            &mut dispatcher,
            &mut active,
            sid_a.clone()
        ));
        let x_back = dispatcher.deck().slides[&sid_a]
            .find_element("el_a")
            .unwrap()
            .geometry
            .x;
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
                assert!(
                    snapshot
                        .position_of(&CanvasTarget::Slide(sid.clone()), &eid)
                        .is_some()
                );
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
            new_size: Size {
                width: 200.0,
                height: 100.0,
            },
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
        let geo = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
        let mut snap = TransactionSnapshot::empty();
        snap.record_geometry(CanvasTarget::Slide(sid.clone()), eid.clone(), geo);
        d.begin_transaction("Resize Element", snap);

        let event = InteractionEvent::ElementResizeEnded {
            element_id: eid.clone(),
            new_position: Point { x: 50.0, y: 60.0 },
            new_size: Size {
                width: 300.0,
                height: 200.0,
            },
            background_size: None,
            background_position: None,
        };
        match interpret_inline(&d, &sel, &Some(sid.clone()), event) {
            InterpretResult::CommitTransactionWith(cmd) => {
                assert_eq!(cmd.label(), "Resize Element");
                let mut tmp = Deck::sample();
                let out = cmd.apply(&mut tmp).unwrap();
                let g = tmp.slides[&sid]
                    .find_element(&eid)
                    .unwrap()
                    .geometry
                    .clone();
                assert_eq!(g.x, 50.0);
                assert_eq!(g.y, 60.0);
                assert_eq!(g.width, 300.0);
                assert_eq!(g.height, 200.0);

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
            new_size: Size {
                width: 1.0,
                height: 1.0,
            },
            background_size: None,
            background_position: None,
        };
        match interpret_inline(&d, &sel, &Some(sid), event) {
            InterpretResult::Nothing => {}
            other => panic!("expected Nothing, got {other:?}"),
        }
    }

    #[test]
    fn resize_lifecycle_round_trip_undo_restores_original_rect() {
        let (mut d, _, sid, eid) = fixture();
        let original = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();

        let mut snap = TransactionSnapshot::empty();
        snap.record_geometry(
            CanvasTarget::Slide(sid.clone()),
            eid.clone(),
            original.clone(),
        );
        d.begin_transaction("Resize Element", snap);

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

        let after = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
        assert_eq!(after.x, original.x + 100.0);
        assert_eq!(after.width, original.width - 80.0);

        d.undo().unwrap().expect("undo not a no-op");
        let restored = d.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
        assert_eq!(restored.x, original.x);
        assert_eq!(restored.y, original.y);
        assert_eq!(restored.width, original.width);
        assert_eq!(restored.height, original.height);
    }

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

        assert_eq!(node.geometry.x, 560.0);
        assert_eq!(node.geometry.y, 240.0);

        assert_eq!(
            node.inline_styles
                .get("background-size")
                .map(String::as_str),
            Some("cover")
        );
        assert_eq!(
            node.inline_styles
                .get("background-position")
                .map(String::as_str),
            Some("center")
        );

        let bg_image = node
            .inline_styles
            .get("background-image")
            .map(String::as_str)
            .unwrap_or("");
        assert!(bg_image.contains(&entry.id));

        match node.content {
            ElementContent::Image(ref a) => assert_eq!(a.asset_id, entry.id),
            ref other => panic!("expected Image content, got {other:?}"),
        }
    }

    #[test]
    fn build_image_element_centers_around_drop_point_when_provided() {
        let entry = sample_asset_entry();
        let drop = Some(Point {
            x: 1000.0,
            y: 500.0,
        });
        let node = build_image_element_from_asset(&entry, 400, 200, drop, (1920, 1080));

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
        use crate::bundle::deck_io::{deserialize_deck, serialize_deck};

        let mut deck = Deck::sample();
        let bytes = b"hello-world-as-image".to_vec();
        let before_count = deck.assets.entry_count();
        deck.assets.insert_blob(
            bytes.clone(),
            "x.png".into(),
            "image/png".into(),
            Some(crate::bundle::assets::AssetDimensions {
                width: 10,
                height: 10,
            }),
        );
        assert_eq!(deck.assets.entry_count(), before_count + 1);

        let serialized = serialize_deck(&deck).unwrap();
        assert!(!serialized.assets_index_json.is_empty());
        assert!(!serialized.asset_files.is_empty());

        let back = deserialize_deck(serialized).unwrap();
        assert_eq!(back.assets.entry_count(), before_count + 1);

        let entry = back.assets.assets.last().unwrap().clone();
        assert_eq!(back.assets.files.get(&entry.path), Some(&bytes));
    }

    #[test]
    fn build_insert_slide_after_active_inserts_after_the_active_slide() {
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
                guides: Vec::new(),
                background: None,
                background_image: None,
                notes: None,
            },
        }
        .apply(&mut deck)
        .unwrap();
        let dispatcher = CommandDispatcher::new(deck);

        let (cmd, new_id) = build_insert_slide_after_active(&dispatcher, Some(&orig), "").unwrap();
        assert!(!new_id.is_empty());
        assert_eq!(cmd.label(), "Add Slide");
        assert!(cmd.affects_slide_list());

        let mut deck2 = dispatcher.deck().clone();
        cmd.apply(&mut deck2).unwrap();
        assert_eq!(deck2.slide_order[1], new_id);
        assert_eq!(deck2.slide_order[2], "s_b");
        assert!(deck2.slides.contains_key(&new_id));
        assert!(deck2.manifest.slides.iter().any(|e| e.id == new_id));
    }

    #[test]
    fn build_insert_slide_after_active_titles_slide_by_position() {
        let mut deck = Deck::sample();
        let orig: SlideId = deck.slide_order[0].clone();
        InsertSlide {
            position: 1,
            slide: SlideNode::new(
                "s_b".into(),
                "blank".into(),
                crate::deck::builders::group_element("rt_b", Vec::new()),
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
                guides: Vec::new(),
                background: None,
                background_image: None,
                notes: None,
            },
        }
        .apply(&mut deck)
        .unwrap();
        let dispatcher = CommandDispatcher::new(deck);

        let (cmd, new_id) = build_insert_slide_after_active(&dispatcher, Some(&orig), "").unwrap();
        let mut deck2 = dispatcher.deck().clone();
        cmd.apply(&mut deck2).unwrap();
        let entry = deck2
            .manifest
            .slides
            .iter()
            .find(|e| e.id == new_id)
            .unwrap();
        assert_eq!(entry.title, "Slide 2");
    }

    #[test]
    fn build_insert_slide_after_active_appends_when_no_active_slide() {
        let deck = Deck::sample();
        let len_before: usize = deck.slide_order.len();
        let dispatcher = CommandDispatcher::new(deck);

        let (cmd, new_id) = build_insert_slide_after_active(&dispatcher, None, "").unwrap();
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
            build_insert_slide_after_active(&dispatcher, Some(&active), "").unwrap();

        let mut deck2 = dispatcher.deck().clone();
        cmd.apply(&mut deck2).unwrap();
        let slide = deck2.slides.get(&new_id).unwrap();
        assert_eq!(slide.layout_id, "blank");

        assert!(slide.root.children.is_empty());
    }

    #[test]
    fn build_insert_slide_after_active_seeds_from_chosen_layout() {
        let deck = crate::deck::templates::new_deck(crate::deck::templates::light_theme(), "title");
        let active: SlideId = deck.slide_order[0].clone();
        let dispatcher = CommandDispatcher::new(deck);
        let (cmd, new_id) =
            build_insert_slide_after_active(&dispatcher, Some(&active), "hero").unwrap();

        let mut deck2 = dispatcher.deck().clone();
        cmd.apply(&mut deck2).unwrap();
        let slide = deck2.slides.get(&new_id).unwrap();
        assert_eq!(slide.layout_id, "hero");
        assert_eq!(slide.root.children.len(), 3);

        let layout = &deck2.theme.layouts["hero"];
        for (a, b) in slide.root.children.iter().zip(layout.root.children.iter()) {
            assert_ne!(a.id, b.id);
        }
    }

    #[test]
    fn build_set_text_command_some_on_changed_text() {
        let (dispatcher, _sel, sid, eid) = fixture();
        let out = build_set_text_command(
            &dispatcher,
            Some(CanvasTarget::Slide(sid.clone())),
            eid,
            RichText::new("brand new text"),
        );
        let cmd = out.expect("changed text should produce a command");
        assert_eq!(cmd.label(), "Edit Text");
    }

    #[test]
    fn build_set_text_command_none_when_text_unchanged() {
        let (dispatcher, _sel, sid, eid) = fixture();
        let current: RichText = match &dispatcher.deck().slides[&sid]
            .find_element(&eid)
            .unwrap()
            .content
        {
            ElementContent::Text(rt) => rt.clone(),
            other => panic!("expected text, got {other:?}"),
        };

        assert!(
            build_set_text_command(
                &dispatcher,
                Some(CanvasTarget::Slide(sid.clone())),
                eid,
                current
            )
            .is_none()
        );
    }

    #[test]
    fn build_set_text_command_none_without_active_slide() {
        let (dispatcher, _sel, _sid, eid) = fixture();
        assert!(build_set_text_command(&dispatcher, None, eid, RichText::new("x")).is_none());
    }

    #[test]
    fn build_set_text_command_none_on_non_text_element() {
        let (dispatcher, _sel, sid, _eid) = fixture();
        let root_id: ElementId = dispatcher.deck().slides[&sid].root.id.clone();
        assert!(
            build_set_text_command(
                &dispatcher,
                Some(CanvasTarget::Slide(sid.clone())),
                root_id,
                RichText::new("x")
            )
            .is_none()
        );
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

    #[test]
    fn build_insert_layout_after_active_creates_a_unique_layout() {
        let (mut dispatcher, _sel, _sid, _eid) = fixture();

        let active: Option<LayoutId> = Some("blank".into());
        let (cmd, new_id) = build_insert_layout_after_active(&dispatcher, active.as_ref()).unwrap();
        assert_ne!(new_id, "blank");
        assert!(!dispatcher.deck().theme.layouts.contains_key(&new_id));
        cmd.apply(dispatcher.deck_mut()).unwrap();
        assert!(dispatcher.deck().theme.layouts.contains_key(&new_id));

        let pos = dispatcher
            .deck()
            .theme
            .layout_order
            .iter()
            .position(|l| l == &new_id);
        assert_eq!(pos, Some(1));
    }

    #[test]
    fn build_layout_list_data_emits_layouts_in_order_with_globals() {
        let (mut dispatcher, _sel, _sid, _eid) = fixture();
        dispatcher.deck_mut().theme.globals_css = ":root{--g:1}".into();
        let data = build_layout_list_data(dispatcher.deck(), Some(&"blank".to_string()));
        assert_eq!(
            data.layouts.len(),
            dispatcher.deck().theme.layout_order.len()
        );
        assert_eq!(data.layouts[0].layout_id, "blank");
        assert_eq!(data.layouts[0].name, "Blank");
        assert!(!data.layouts[0].html.is_empty());
        assert_eq!(data.active_layout_id.as_deref(), Some("blank"));
        assert_eq!(data.globals_css, ":root{--g:1}");
    }

    #[test]
    fn property_changed_targets_the_active_layout_in_layout_mode() {
        let (mut dispatcher, _sel, sid, _eid) = fixture();

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

        assert_eq!(
            dispatcher.deck().theme.layouts["blank"]
                .find_element("el_lt")
                .unwrap()
                .geometry
                .width,
            321.0
        );
        let slide_root_children = dispatcher.deck().slides[&sid].root.children.len();
        assert!(slide_root_children > 0);
    }

    #[test]
    fn set_element_animation_enable_builds_insert() {
        let (mut dispatcher, _sel, sid, eid) = fixture();
        let result = interpret_set_element_animation(
            dispatcher.deck(),
            EditorMode::Slide,
            Some(&sid),
            eid.clone(),
            "entrance",
            true,
        );
        let cmd = match result {
            InterpretResult::Command(c) => c,
            other => panic!("expected Command, got {other:?}"),
        };
        cmd.apply(dispatcher.deck_mut()).unwrap();
        let t = &dispatcher.deck().slides[&sid].animations;
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].category, AnimationCategory::Entrance);
        assert_eq!(t[0].effect.keyframe_name(), Some("appear"));
        assert_eq!(t[0].element_id, eid);
    }

    #[test]
    fn add_animation_appends_catalog_effect() {
        let (mut dispatcher, _sel, sid, eid) = fixture();
        let result = interpret_add_animation(
            dispatcher.deck(),
            EditorMode::Slide,
            Some(&sid),
            eid.clone(),
            "fly-in",
            Some("left"),
        );
        let cmd = match result {
            InterpretResult::Command(c) => c,
            other => panic!("expected Command, got {other:?}"),
        };
        cmd.apply(dispatcher.deck_mut()).unwrap();
        let t = &dispatcher.deck().slides[&sid].animations;
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].effect.keyframe_name(), Some("fly-in-left"));
    }

    #[test]
    fn update_animation_overlays_timing() {
        let (mut dispatcher, _sel, sid, eid) = fixture();
        let add = match interpret_set_element_animation(
            dispatcher.deck(),
            EditorMode::Slide,
            Some(&sid),
            eid,
            "entrance",
            true,
        ) {
            InterpretResult::Command(c) => c,
            other => panic!("expected Command, got {other:?}"),
        };
        add.apply(dispatcher.deck_mut()).unwrap();
        let anim_id = dispatcher.deck().slides[&sid].animations[0].id.clone();
        let upd = match interpret_update_animation(
            dispatcher.deck(),
            Some(&sid),
            &anim_id,
            Some("after_previous"),
            Some(700),
            None,
            None,
            None,
            None,
        ) {
            InterpretResult::Command(c) => c,
            other => panic!("expected Command, got {other:?}"),
        };
        upd.apply(dispatcher.deck_mut()).unwrap();
        let e = &dispatcher.deck().slides[&sid].animations[0];
        assert_eq!(e.timing.duration_ms, 700);
        assert_eq!(e.trigger, AnimationTrigger::AfterPrevious);
    }

    #[test]
    fn move_animation_reorders_and_retriggers() {
        let (mut dispatcher, _sel, sid, eid) = fixture();

        for kind in ["entrance", "exit"] {
            if let InterpretResult::Command(c) = interpret_add_animation(
                dispatcher.deck(),
                EditorMode::Slide,
                Some(&sid),
                eid.clone(),
                if kind == "entrance" {
                    "fade-in"
                } else {
                    "fade-out"
                },
                None,
            ) {
                c.apply(dispatcher.deck_mut()).unwrap();
            }
        }
        let second = dispatcher.deck().slides[&sid].animations[1].id.clone();

        let cmd = match interpret_move_animation(
            dispatcher.deck(),
            Some(&sid),
            &second,
            0,
            "with_previous",
        ) {
            InterpretResult::Command(c) => c,
            other => panic!("expected Command, got {other:?}"),
        };
        cmd.apply(dispatcher.deck_mut()).unwrap();
        let t = &dispatcher.deck().slides[&sid].animations;
        assert_eq!(t[0].id, second);
        assert_eq!(t[0].trigger, AnimationTrigger::WithPrevious);
    }

    #[test]
    fn set_element_animation_disable_removes_existing() {
        let (mut dispatcher, _sel, sid, eid) = fixture();
        if let InterpretResult::Command(c) = interpret_set_element_animation(
            dispatcher.deck(),
            EditorMode::Slide,
            Some(&sid),
            eid.clone(),
            "exit",
            true,
        ) {
            c.apply(dispatcher.deck_mut()).unwrap();
        }
        assert_eq!(dispatcher.deck().slides[&sid].animations.len(), 1);
        let result = interpret_set_element_animation(
            dispatcher.deck(),
            EditorMode::Slide,
            Some(&sid),
            eid,
            "exit",
            false,
        );
        match result {
            InterpretResult::Command(c) => {
                c.apply(dispatcher.deck_mut()).unwrap();
            }
            other => panic!("expected Command, got {other:?}"),
        }
        assert!(dispatcher.deck().slides[&sid].animations.is_empty());
    }

    #[test]
    fn set_element_animation_noop_in_layout_mode() {
        let (dispatcher, _sel, sid, eid) = fixture();
        let result = interpret_set_element_animation(
            dispatcher.deck(),
            EditorMode::Layout,
            Some(&sid),
            eid,
            "entrance",
            true,
        );
        assert!(matches!(result, InterpretResult::Nothing));
    }

    #[test]
    fn present_start_index_uses_active_slide_position() {
        let mut deck = Deck::sample();

        let root = crate::deck::builders::group_element("el_root", vec![]);
        let s2 = SlideNode::new("slide_two".into(), "blank".into(), root);
        deck.slides.insert("slide_two".into(), s2);
        deck.slide_order.push("slide_two".into());
        let active: Option<SlideId> = Some("slide_two".into());
        assert_eq!(present_start_index(&deck, active.as_ref()), Some(1));
    }

    #[test]
    fn build_slide_inspector_data_reads_active_slide_and_layouts() {
        let mut deck = Deck::sample();
        let sid = deck.slide_order[0].clone();
        deck.slides.get_mut(&sid).unwrap().metadata.background = Some("#222".into());
        deck.manifest
            .slides
            .iter_mut()
            .find(|e| e.id == sid)
            .unwrap()
            .notes = Some("speak up".into());

        let data = build_slide_inspector_data(&deck, Some(&sid)).expect("active slide");
        assert_eq!(data.slide_id, sid);
        assert_eq!(data.background, "#222");
        assert_eq!(data.notes, "speak up");
        assert_eq!(data.layout_id, "title");
        assert!(data.layouts.iter().any(|l| l.id == "blank"));

        assert!(build_slide_inspector_data(&deck, None).is_none());
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

        assert_eq!(
            present_start_index(&deck, Some(&"ghost".to_string())),
            Some(0)
        );

        let empty = Deck::default();
        assert_eq!(present_start_index(&empty, None), None);
    }
}
