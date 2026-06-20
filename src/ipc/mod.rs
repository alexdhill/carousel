// IPC message protocol.
//
// All messages crossing the JS <-> Rust boundary share a uniform envelope:
//   { id, timestamp, type, payload }
//
// `MessageKind` is adjacently tagged on (`type`, `payload`) and flattened into
// the envelope. JS sends interaction events and lifecycle signals; Rust sends
// patches, selection state, and slide-mount instructions. Patches are designed
// to be idempotent so they can be applied without coordination.

pub mod bridge;
pub mod present;

use serde::{Deserialize, Serialize};

pub type SlideId = String;
pub type ElementId = String;
#[allow(dead_code)]
pub type AssetId = String;
#[allow(dead_code)]
pub type LayoutId = String;

// IpcMessage
// Wire envelope. The `kind` field is flattened so `type` and `payload`
// appear at the top level of the JSON object.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IpcMessage {
    pub id: String,
    pub timestamp: u64,
    #[serde(flatten)]
    pub kind: MessageKind,
}

impl IpcMessage {
    // new
    // Inputs: a MessageKind to send.
    // Output: an IpcMessage with a fresh ULID and millisecond timestamp.
    // Dataflow: pure constructor; reads the wall clock once.
    pub fn new(kind: MessageKind) -> Self {
        let id: String = ulid::Ulid::new().to_string();
        let timestamp: u64 = now_millis();
        Self { id, timestamp, kind }
    }
}

// now_millis
// Inputs: none. Reads SystemTime::now().
// Output: milliseconds since UNIX epoch as u64. Returns 0 if the clock is
// behind UNIX epoch (impossible in practice but handled defensively).
fn now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now();
    let delta = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    delta.as_millis() as u64
}

// MessageKind
// All message variants in either direction. Adjacent tagging keeps the
// payload as its own JSON object so JS can read `msg.payload` directly
// without re-parsing.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "payload")]
pub enum MessageKind {
    // ---- JS -> Rust ----
    Ready,
    Interaction(InteractionEvent),
    ThumbnailGenerated(ThumbnailResult),
    Error { code: String, message: String },

    // ---- Rust -> JS ----
    MountSlide(MountSlideArgs),
    ApplyPatch(Patch),
    SetSelection(SelectionState),
    SetTheme(SetThemeArgs),
    Configure(EditorConfig),
    RequestThumbnail(ThumbnailRequest),
    // ObjectTreeUpdate
    // Stage 9 — Object Panel. Sent after slide mount and after any
    // dispatch whose command reports affects_object_tree=true. The
    // payload mirrors the active slide's element tree in display order
    // (= z-order, since the serializer assigns z-index from sibling
    // position). JS rebuilds the object panel from this payload — it
    // does not crawl the shadow DOM for structure.
    ObjectTreeUpdate(ObjectTreeData),
    // SlideListUpdate
    // Stage 10 — Thumbnail row. Sent once on app start and after every
    // file Open / New. Carries every slide's id, title and serialized
    // HTML so JS can mount each thumbnail in its own shadow root. The
    // active slide's thumbnail re-renders on subsequent MountSlide
    // events (JS caches the new HTML keyed on slide id). Slide-list
    // shape changes (add / remove / reorder) — once those commands
    // exist — will re-send this message.
    SlideListUpdate(SlideListData),
    // AssetsUpdate
    // Bulk snapshot of every registered asset's bytes. Sent on app
    // start and after file Open / New. JS caches blob URLs keyed by
    // asset id so any subsequent slide mount can resolve images
    // without another IPC round-trip.
    AssetsUpdate(AssetsBundle),
    // AssetAdded
    // Incremental delivery of a single newly-imported asset. Avoids
    // re-shipping every asset on each import.
    AssetAdded(AssetPayload),
    // LayoutListUpdate
    // Stage 11 — layouts row + globals editor. The layout-mode analogue of
    // SlideListUpdate: every layout's id, name and serialized HTML plus the
    // active layout id, the shared theme/globals CSS, and dimensions. Sent
    // when entering layout mode and after any command that reports
    // affects_layout_list / affects_globals.
    LayoutListUpdate(LayoutListData),
    // SetMode
    // Stage 11 — editor mode echo. Tells JS which mode is now active
    // ("slide" | "layout") so it can flip `body[data-mode]` and swap the
    // slides-vs-layouts list and inspector-vs-globals panels.
    SetMode { mode: String },
    // SlideAnimationsUpdate
    // Stage: animations — the active slide's timeline (id/element/category
    // per entry) so the inspector's Appear/Disappear toggles reflect state.
    SlideAnimationsUpdate(SlideAnimationsData),
    // Notice
    // A non-fatal advisory message (e.g. an add-time ordering accommodation).
    // `detail` is an optional longer description the toast reveals on click.
    Notice {
        message: String,
        #[serde(default)]
        detail: Option<String>,
    },

    // ---- Presentation mode (Rust -> presentation webview) ----
    // PresentInit
    // One-shot config sent after the presentation webview reports Ready:
    // built-in keyframes CSS + deck pixel dimensions for stage scaling.
    PresentInit(present::PresentInitPayload),
    // PresentAssets
    // Ships every registered asset's bytes to the presentation webview so it can
    // build its own blob-URL cache (blob URLs are not shareable across webviews).
    // Reuses the editor's AssetsBundle shape. Sent on Ready, before PresentSlide.
    PresentAssets(AssetsBundle),
    // PresentSlide
    // Mount a slide in the presentation stage (slide HTML + theme/globals CSS).
    PresentSlide(present::PresentSlidePayload),
    // PresentReveal
    // Apply one step's resolved visual state (hidden / shown / animate).
    PresentReveal(present::RevealPayload),

    // SlideInspectorUpdate
    // Smart styles pane — the active slide's inspector data (title, notes,
    // background, layout, and the available layouts for the picker). Sent on
    // slide mount and after any slide-metadata command so the Slide box (shown
    // when nothing is selected) stays in sync.
    SlideInspectorUpdate(SlideInspectorData),
}

// ---------- JS -> Rust payloads ----------

// InteractionEvent
// Descriptive event from the webview: "this happened", not "do this".
// Internal tag on `kind` so payload reads as a flat object on the JS side.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind")]
pub enum InteractionEvent {
    ElementClicked {
        element_id: ElementId,
        modifiers: Modifiers,
        position: Point,
    },
    ElementDragStarted {
        element_id: ElementId,
        position: Point,
    },
    ElementDragged {
        element_id: ElementId,
        delta: Vec2,
        position: Point,
    },
    ElementDragEnded {
        element_id: ElementId,
        delta: Vec2,
    },
    // ElementResizeStarted
    // Fired when the user mousedowns on a selection-overlay handle.
    // The interpret layer opens a transaction and snapshots geometry
    // so the eventual commit can be undone in one step.
    ElementResizeStarted {
        element_id: ElementId,
        handle: ResizeHandle,
        position: Point,
    },
    // ElementResized
    // Throttled mid-drag updates (one per rAF). Ignored on the Rust
    // side; the JS host applies optimistic style writes directly so
    // there's no echo-patch interference.
    ElementResized {
        element_id: ElementId,
        handle: ResizeHandle,
        new_size: Size,
        new_position: Point,
    },
    // ElementResizeEnded
    // Final resize commit. The interpret layer dispatches one
    // ResizeElement inside the open transaction so undo restores the
    // pre-resize geometry in a single step.
    ElementResizeEnded {
        element_id: ElementId,
        new_position: Point,
        new_size: Size,
        // Present only when resizing a cropped image: the proportionally
        // scaled background values so the picture scales with the box
        // (B-proportional). Absent for every other resize.
        #[serde(default)]
        background_size: Option<String>,
        #[serde(default)]
        background_position: Option<String>,
    },
    // ElementCropCommitted
    // Final commit of a crop session. Carries the mask geometry plus the two
    // background-* values the webview computed. Interpreted into one
    // CompositeCommand so the whole crop is a single undo step.
    ElementCropCommitted {
        element_id: ElementId,
        new_position: Point,
        new_size: Size,
        background_size: String,
        background_position: String,
    },
    // Clipboard accelerators. Copy/Cut carry a focus-derived scope; paste
    // dispatches by the typed clipboard buffer (focus-independent).
    CopyRequested { scope: ClipboardScope },
    CutRequested { scope: ClipboardScope },
    PasteRequested,
    // Delete a specific slide (navigator focus + Delete, or the thumbnail "×").
    RemoveSlideRequested { slide_id: SlideId },
    // TextEditStarted
    // Fired when a text element enters inline editing (double-click). The
    // webview is authoritative for the text content during the session;
    // Rust takes no action until TextEditEnded (SPEC §8.5).
    TextEditStarted {
        element_id: ElementId,
    },
    TextEdited {
        element_id: ElementId,
        delta: RichTextDelta,
    },
    // TextEditEnded
    // Fired when an inline edit session commits (Enter or blur). `text`
    // carries the element's final plain textContent so the Rust side can
    // dispatch a single SetTextContent. An empty string is a valid edit
    // (the user cleared the text).
    TextEditEnded {
        element_id: ElementId,
        text: String,
    },
    BackgroundClicked {
        position: Point,
    },
    KeyPressed {
        key: String,
        modifiers: Modifiers,
    },
    SlideThumbnailClicked {
        slide_id: SlideId,
    },
    // PropertyChanged
    // Stage 8 — Property Inspector. Sent when the user commits a value
    // in the inspector (input blur / Enter / custom-CSS Apply click).
    // Property names follow CSS conventions for arbitrary style writes,
    // with six reserved tokens — "x", "y", "width", "height", "rotation",
    // "opacity" — routed by the interpret layer to SetGeometryProperty.
    // An empty `value` requests deletion of the property; the interpret
    // layer maps that to RemoveInlineStyle.
    PropertyChanged {
        element_id: ElementId,
        property: String,
        value: String,
    },
    // SetSelectionFromPanel
    // Stage 9 — Object Panel. The user clicked an element in the panel.
    // The interpret layer reuses the existing selection plumbing and
    // emits InterpretResult::Selection (non-undoable, like viewport
    // clicks).
    SetSelectionFromPanel {
        element_ids: Vec<ElementId>,
    },
    // InsertElementRequested
    // Stage 9 — Object Panel toolbar. The user clicked Text / Shape /
    // Group. The interpret layer constructs a fresh ElementNode of the
    // requested type and dispatches InsertElement. The `parent_id` and
    // `position` are optional; when omitted, the new element appends to
    // the slide's root group.
    InsertElementRequested {
        element_type: String,
        #[serde(default)]
        parent_id: Option<ElementId>,
        #[serde(default)]
        position: Option<usize>,
    },
    // RenameElementRequested
    // Stage 9 — Object Panel long-click rename. An empty string clears
    // the name (panel falls back to displaying the element id).
    RenameElementRequested {
        element_id: ElementId,
        new_name: String,
    },
    // ReparentElementRequested
    // Stage 9 — Object Panel drag-drop. JS pre-adjusts `new_position`
    // for the post-removal coordinate system (see ReparentElement docs);
    // the command applies remove + insert directly.
    ReparentElementRequested {
        element_id: ElementId,
        new_parent_id: ElementId,
        new_position: usize,
    },
    // AddSlideRequested
    // Stage 10 — thumbnail "+" tile / Cmd+Shift+N. Inserts a blank
    // slide directly after the active slide and makes it active.
    AddSlideRequested,
    // SlideTitleEditRequested
    // Double-click a thumbnail label. The Rust side dispatches a
    // SetSlideTitle command (manifest title) and rebroadcasts the slide
    // list so the label refreshes.
    SlideTitleEditRequested {
        slide_id: SlideId,
        new_title: String,
    },
    // ElementIdEditRequested
    // Double-click an object-panel row to rename the element's id.
    // `new_id` is the raw text the user typed; the Rust side sanitizes it
    // (runs of whitespace collapse to a single '_') before dispatching a
    // SetElementId command against the active slide.
    ElementIdEditRequested {
        element_id: ElementId,
        new_id: String,
    },
    // AssetImported
    // Stage — image import. Sent when the user drops a file (or pastes
    // an image) onto the viewport. `content_base64` carries the raw
    // bytes; the Rust side decodes once, hashes, dedupes, registers,
    // and dispatches an InsertElement that references the resulting
    // asset id.
    //
    // `position` is the drop point in SLIDE coordinates (JS has
    // already divided by the viewport scale). When omitted, the
    // element lands centered on the slide. `width` / `height` are the
    // image's natural pixel dimensions decoded by JS via Image().
    AssetImported {
        content_base64: String,
        original_filename: String,
        media_type: String,
        width: u32,
        height: u32,
        #[serde(default)]
        position: Option<Point>,
        // When true, the import targets the active slide's background image
        // (SetSlideBackgroundImage) instead of inserting a picture element.
        #[serde(default)]
        as_slide_background: bool,
    },
    // ---- Stage 11: layout editor ----
    // SetEditorMode
    // Toolbar mode toggle. `mode` is "slide" or "layout"; anything else is
    // ignored by the Rust handler.
    SetEditorMode {
        mode: String,
    },
    // LayoutThumbnailClicked
    // Select / mount a layout in the layouts row (layout-mode analogue of
    // SlideThumbnailClicked).
    LayoutThumbnailClicked {
        layout_id: LayoutId,
    },
    // AddLayoutRequested
    // Layouts row "+" tile. Inserts a new blank layout after the active one
    // and makes it active.
    AddLayoutRequested,
    // LayoutNameEditRequested
    // Double-click a layout label to rename it (reuses the floating editor).
    LayoutNameEditRequested {
        layout_id: LayoutId,
        new_name: String,
    },
    // GlobalsCssEditRequested
    // Commit of the globals CSS textarea (blur). Replaces the deck-wide
    // globals blob.
    GlobalsCssEditRequested {
        new_css: String,
    },
    // ---- Stage: animations ----
    // SetElementAnimation
    // Minimal Appear/Disappear toggle on the selected element. `category` is
    // "entrance" or "exit"; `enabled` toggles add/remove of a default entry.
    SetElementAnimation {
        element_id: ElementId,
        category: String,
        enabled: bool,
    },
    // AddAnimation — append a catalog effect to the selected element. The
    // server resolves catalog_id → category/keyframe/effect; `direction`
    // (top|bottom|left|right) selects the fly-<dir> keyframe when directional.
    AddAnimation {
        element_id: ElementId,
        catalog_id: String,
        #[serde(default)]
        direction: Option<String>,
    },
    // UpdateAnimation — patch one entry's mutable fields; None = leave as-is.
    UpdateAnimation {
        animation_id: String,
        #[serde(default)]
        trigger: Option<String>,
        #[serde(default)]
        duration_ms: Option<u32>,
        #[serde(default)]
        delay_ms: Option<u32>,
        #[serde(default)]
        easing: Option<String>,
        #[serde(default)]
        iterations: Option<crate::deck::animation::AnimationIterations>,
        #[serde(default)]
        targets: Option<Vec<crate::deck::animation::PropertyTarget>>,
    },
    // RemoveAnimationRequested — drop the entry with this id.
    RemoveAnimationRequested {
        animation_id: String,
    },
    // ---- Theme save/load ----
    // SaveThemeRequested / LoadThemeRequested — the layout-mode "Save Theme…" /
    // "Load Theme…" buttons. Map to FileAction::SaveTheme / LoadTheme.
    SaveThemeRequested,
    LoadThemeRequested,
    // ---- Smart styles pane: Slide box (no selection) ----
    // Each targets the active slide (the Rust side supplies the id). An empty
    // string clears the field (background/notes → None).
    SetSlideBackgroundRequested { background: String },
    // Clear the active slide's background image. Setting one happens via
    // AssetImported with as_slide_background=true (it carries the bytes).
    SetSlideBackgroundImageCleared,
    SetSlideNotesRequested { notes: String },
    SetSlideLayoutRequested { layout_id: LayoutId },
    // SetGroupLayout — patch a group's flex props (None = leave as-is).
    SetGroupLayout {
        element_id: ElementId,
        #[serde(default)] direction: Option<String>,
        #[serde(default)] distribution: Option<String>,
        #[serde(default)] alignment: Option<String>,
    },
    // SetGroupScale — set a group's uniform scale.
    SetGroupScale {
        element_id: ElementId,
        scale: f64,
    },
    // GroupSelectionRequested — Cmd+Shift+G. Wrap the given sibling elements in
    // a new group at the top member's z-slot. The Rust side mints the group id.
    GroupSelectionRequested {
        element_ids: Vec<ElementId>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

// ClipboardScope: what a copy/cut acts on, derived from the webview's focused
// region (navigator -> Slide, else Elements).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardScope {
    Elements,
    Slide,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub enum ResizeHandle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

// RichTextDelta
// Placeholder for the rich-text editing protocol. Filled in alongside the
// text-editing command set; here only to satisfy InteractionEvent::TextEdited.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct RichTextDelta {
    pub ops: Vec<serde_json::Value>,
}

// ThumbnailResult
// JS reports completion of an offscreen render with a base64 PNG payload.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ThumbnailResult {
    pub slide_id: SlideId,
    pub png_base64: String,
}

// ---------- Rust -> JS payloads ----------

// MountSlideArgs
// Tells the webview to swap the viewport's slide-host with the given HTML
// inside a fresh shadow root.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MountSlideArgs {
    pub slide_id: SlideId,
    pub slide_html: String,
    pub theme_css: String,
    // globals_css (Stage 11) — the deck-wide globals blob injected into the
    // shadow root between theme_css and the asset-vars block. The same mount
    // path serves both slides and layouts (the id is whichever canvas is
    // active); JS does not need to know which kind it is.
    pub globals_css: String,
}

// SelectionState
// Identifies the currently selected elements on a given slide. The webview
// uses it to render selection overlays. `slide_id` is optional because
// "no slide active" is a valid editor state (e.g., between mounts, after
// the user closes a deck).
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectionState {
    pub slide_id: Option<SlideId>,
    pub element_ids: Vec<ElementId>,
}

impl SelectionState {
    // empty
    // Inputs: none.
    // Output: a SelectionState with no slide and no elements.
    pub fn empty() -> Self {
        Self::default()
    }

    // is_empty
    // Inputs: self.
    // Output: true iff no elements are selected.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.element_ids.is_empty()
    }

    // contains
    // Inputs: an element id.
    // Output: true iff that id is in the selection set.
    pub fn contains(&self, id: &str) -> bool {
        self.element_ids.iter().any(|e| e == id)
    }

    // toggle
    // Inputs: an element id (consumed).
    // Output: side-effect; adds the id if absent, removes it if present.
    // Used by Shift-click handling.
    pub fn toggle(&mut self, id: ElementId) {
        assert!(!id.is_empty(), "toggle called with empty id");
        if let Some(pos) = self.element_ids.iter().position(|e| e == &id) {
            self.element_ids.remove(pos);
        } else {
            self.element_ids.push(id);
        }
    }
}

// SetThemeArgs
// Pushes new theme CSS into the active shadow root. Placeholder shape until
// the theme model is implemented.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SetThemeArgs {
    pub theme_id: String,
    pub theme_css: String,
}

// EditorConfig
// One-shot configuration the webview reads at startup.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct EditorConfig {
    pub debug: bool,
    // The immutable built-in @keyframes library (Stage: animations). JS caches
    // it and injects it into every shadow root alongside theme/globals CSS.
    #[serde(default)]
    pub animation_keyframes_css: String,
    // The effect catalog (add-menu + effect picker source of truth).
    #[serde(default)]
    pub animation_catalog: Vec<crate::deck::anim_catalog::AnimCatalogItem>,
}

// ThumbnailRequest
// Asks the webview to render an offscreen thumbnail for a slide.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ThumbnailRequest {
    pub slide_id: SlideId,
    pub width: u32,
    pub height: u32,
}

// ObjectTreeData
// Stage 9 — Object Panel payload. Top-level `nodes` are the slide root's
// children in display order (= z-order). Group elements carry their own
// children inside their ObjectTreeNode. The shape is recursive and
// dense — every element in the slide appears exactly once.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ObjectTreeData {
    pub slide_id: SlideId,
    pub root_id: ElementId,
    pub nodes: Vec<ObjectTreeNode>,
}

// ObjectTreeNode
// One row in the panel. `element_type` is the HTML token ("text",
// "image", "shape", "media", "table", "group", "embed") so JS can pick
// the matching badge icon. The panel labels each row with the element
// `id` directly — double-clicking it edits that id (see ElementIdEdit-
// Requested). There is no separate display name, so the row label and
// the editable identity are always the same value.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ObjectTreeNode {
    pub id: ElementId,
    pub element_type: String,
    pub children: Vec<ObjectTreeNode>,
}

// AssetPayload
// One asset's bytes plus the metadata JS needs to render it as a blob
// URL. `content_base64` is the raw bytes; JS decodes once and stores a
// Blob + URL.createObjectURL handle keyed on asset_id.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AssetPayload {
    pub asset_id: String,
    pub media_type: String,
    pub content_base64: String,
}

// AssetsBundle
// Bulk version of AssetPayload — used by AssetsUpdate to ship every
// asset in one envelope on mount/load.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AssetsBundle {
    pub assets: Vec<AssetPayload>,
}

// SlideListData
// Stage 10 payload — full slide list with everything JS needs to mount
// thumbnails. `theme_css` and `dimensions` are shared across slides so
// they ride at this top level instead of being duplicated per entry.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SlideListData {
    pub slides: Vec<SlideListEntry>,
    pub active_slide_id: Option<SlideId>,
    pub theme_css: String,
    pub width: u32,
    pub height: u32,
}

// SlideListEntry
// One thumbnail's worth of data. `title` falls back to the slide id
// when the manifest entry's title is empty; JS renders it under the
// thumbnail.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SlideListEntry {
    pub slide_id: SlideId,
    pub title: String,
    pub html: String,
}

// LayoutListData
// Stage 11 payload — the layouts row + globals editor state. Mirrors
// SlideListData but carries `globals_css` so the JS host can refresh the
// globals textarea from the same message, and the per-entry display label
// is `name` (layouts have an explicit display name; slides fall back to id).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct LayoutListData {
    pub layouts: Vec<LayoutListEntry>,
    pub active_layout_id: Option<LayoutId>,
    pub theme_css: String,
    pub globals_css: String,
    pub width: u32,
    pub height: u32,
}

// LayoutListEntry
// One layout thumbnail's worth of data: stable id, display name, and the
// serialized root HTML (rendered in its own shadow root like a slide).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct LayoutListEntry {
    pub layout_id: LayoutId,
    pub name: String,
    pub html: String,
    // Theme background of this layout, so the inspector's Slide box can show
    // the layout's Fill/Image controls in layout mode.
    #[serde(default)]
    pub background: String,
    #[serde(default)]
    pub background_image: String,
}

// SlideAnimationsData
// Stage: animations payload — the active slide's timeline, one entry per
// animation so the inspector can reflect per-element Appear/Disappear state.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SlideAnimationsData {
    pub slide_id: SlideId,
    pub entries: Vec<SlideAnimationEntry>,
}

// SlideAnimationEntry
// One timeline entry fully rendered for the panel: stable id, target element,
// category ("entrance"|"emphasis"|"exit"|"property"), the resolved effect
// (keyframe name OR property targets), trigger, and timing. `effect_id` is the
// keyframe name for Named effects, or "property" for a property-change entry.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SlideAnimationEntry {
    pub animation_id: String,
    pub element_id: ElementId,
    pub category: String,
    pub effect_id: String,
    pub keyframe: Option<String>,
    pub targets: Vec<crate::deck::animation::PropertyTarget>,
    pub trigger: String,
    pub duration_ms: u32,
    pub delay_ms: u32,
    pub easing: String,
    pub iterations: crate::deck::animation::AnimationIterations,
}

// SlideInspectorLayout
// One entry in the Slide box's Layout picker: stable id + display name.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SlideInspectorLayout {
    pub id: LayoutId,
    pub name: String,
}

// SlideInspectorData
// The active slide's inspector payload. `notes` / `background` are empty strings
// when unset (the JS box treats "" as cleared). `layouts` is the theme's layout
// list in display order, so the picker works in slide mode without the full
// LayoutListUpdate.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SlideInspectorData {
    pub slide_id: SlideId,
    pub title: String,
    pub notes: String,
    pub background: String,
    // Background image as stored (e.g. "var(--asset-<id>)"); "" when none. The
    // JS box resolves the asset id to a blob URL for the picker thumbnail.
    #[serde(default)]
    pub background_image: String,
    pub layout_id: LayoutId,
    pub layouts: Vec<SlideInspectorLayout>,
}

// Patch
// Tagged on `op`. Patches are idempotent DOM mutations applied to the
// element matched by `element_id` inside the active slide's shadow root.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "op")]
pub enum Patch {
    SetAttribute {
        element_id: ElementId,
        attribute: String,
        value: String,
    },
    RemoveAttribute {
        element_id: ElementId,
        attribute: String,
    },
    SetStyle {
        element_id: ElementId,
        property: String,
        value: String,
    },
    RemoveStyle {
        element_id: ElementId,
        property: String,
    },
    SetText {
        element_id: ElementId,
        text: String,
    },
    SetInnerHtml {
        element_id: ElementId,
        html: String,
    },
    ReplaceElement {
        element_id: ElementId,
        new_html: String,
    },
    InsertElement {
        parent_id: ElementId,
        position: usize,
        html: String,
    },
    RemoveElement {
        element_id: ElementId,
    },
    // Batch
    // Spec §8.3 sketches this as `Batch(Vec<Patch>)`, but a newtype-of-Vec
    // cannot carry an internal tag — serde emits an array with no place to
    // merge `op`. Represent it as a struct variant; wire form is
    //   {"op":"Batch","patches":[...]}
    Batch { patches: Vec<Patch> },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    // round_trip
    // Serializes a value, parses it back, and returns the reparsed value
    // for inspection.
    fn round_trip<T>(value: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let json = serde_json::to_string(value).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn envelope_ready_roundtrip() {
        let msg = IpcMessage {
            id: "01HQTEST".into(),
            timestamp: 1_735_000_000_000,
            kind: MessageKind::Ready,
        };
        let parsed: IpcMessage = round_trip(&msg);
        assert_eq!(parsed.id, "01HQTEST");
        assert_eq!(parsed.timestamp, 1_735_000_000_000);
        assert!(matches!(parsed.kind, MessageKind::Ready));
    }

    #[test]
    fn envelope_shape_matches_spec() {
        let msg = IpcMessage {
            id: "01H".into(),
            timestamp: 0,
            kind: MessageKind::Ready,
        };
        let json: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["id"], "01H");
        assert_eq!(json["timestamp"], 0);
        assert_eq!(json["type"], "Ready");
    }

    #[test]
    fn mount_slide_roundtrip() {
        let msg = IpcMessage::new(MessageKind::MountSlide(MountSlideArgs {
            slide_id: "s1".into(),
            slide_html: "<section/>".into(),
            theme_css: ".x{}".into(),
            globals_css: "@keyframes a{}".into(),
        }));
        let parsed: IpcMessage = round_trip(&msg);
        match parsed.kind {
            MessageKind::MountSlide(args) => {
                assert_eq!(args.slide_id, "s1");
                assert_eq!(args.slide_html, "<section/>");
                assert_eq!(args.theme_css, ".x{}");
                assert_eq!(args.globals_css, "@keyframes a{}");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    // ---------- Stage 11: layout editor messages ----------

    #[test]
    fn layout_editor_events_roundtrip() {
        let events = [
            (
                r#"{"kind":"SetEditorMode","mode":"layout"}"#,
                "SetEditorMode",
            ),
            (
                r#"{"kind":"LayoutThumbnailClicked","layout_id":"title"}"#,
                "LayoutThumbnailClicked",
            ),
            (r#"{"kind":"AddLayoutRequested"}"#, "AddLayoutRequested"),
            (
                r#"{"kind":"LayoutNameEditRequested","layout_id":"title","new_name":"Title"}"#,
                "LayoutNameEditRequested",
            ),
            (
                r#"{"kind":"GlobalsCssEditRequested","new_css":":root{}"}"#,
                "GlobalsCssEditRequested",
            ),
        ];
        for (raw, kind) in events {
            let parsed: InteractionEvent = serde_json::from_str(raw).unwrap();
            let json = serde_json::to_value(&parsed).unwrap();
            assert_eq!(json["kind"], kind);
        }
    }

    #[test]
    fn layout_list_update_roundtrips_through_ipc() {
        let data = LayoutListData {
            layouts: vec![LayoutListEntry {
                layout_id: "blank".into(),
                name: "Blank".into(),
                html: "<section/>".into(),
                background: String::new(),
                background_image: String::new(),
            }],
            active_layout_id: Some("blank".into()),
            theme_css: ".x{}".into(),
            globals_css: ":root{--a:1}".into(),
            width: 1920,
            height: 1080,
        };
        let msg = IpcMessage::new(MessageKind::LayoutListUpdate(data.clone()));
        let back: IpcMessage =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        match back.kind {
            MessageKind::LayoutListUpdate(d) => assert_eq!(d, data),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn set_mode_echo_roundtrips() {
        let msg = IpcMessage::new(MessageKind::SetMode { mode: "layout".into() });
        let back: IpcMessage =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        match back.kind {
            MessageKind::SetMode { mode } => assert_eq!(mode, "layout"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    // ---------- Stage: animations messages ----------

    #[test]
    fn slide_inspector_payload_and_events_roundtrip() {
        let data = SlideInspectorData {
            slide_id: "s1".into(),
            title: "Intro".into(),
            notes: "speak up".into(),
            background: "#222".into(),
            background_image: String::new(),
            layout_id: "title".into(),
            layouts: vec![SlideInspectorLayout { id: "blank".into(), name: "Blank".into() }],
        };
        let msg = IpcMessage::new(MessageKind::SlideInspectorUpdate(data.clone()));
        let back: IpcMessage =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        match back.kind {
            MessageKind::SlideInspectorUpdate(d) => assert_eq!(d, data),
            other => panic!("unexpected variant: {other:?}"),
        }

        for raw in [
            r##"{"kind":"SetSlideBackgroundRequested","background":"#222"}"##,
            r#"{"kind":"SetSlideNotesRequested","notes":"hi"}"#,
            r#"{"kind":"SetSlideLayoutRequested","layout_id":"blank"}"#,
        ] {
            let _e: InteractionEvent = serde_json::from_str(raw).unwrap();
        }
    }

    #[test]
    fn group_layout_and_scale_events_decode() {
        let a = r#"{"kind":"SetGroupLayout","element_id":"g","direction":"column","distribution":"space-between","alignment":null}"#;
        assert!(matches!(serde_json::from_str::<InteractionEvent>(a).unwrap(),
            InteractionEvent::SetGroupLayout { .. }));
        let b = r#"{"kind":"SetGroupScale","element_id":"g","scale":1.5}"#;
        assert!(matches!(serde_json::from_str::<InteractionEvent>(b).unwrap(),
            InteractionEvent::SetGroupScale { .. }));
    }

    #[test]
    fn theme_action_events_roundtrip() {
        for (raw, kind) in [
            (r#"{"kind":"SaveThemeRequested"}"#, "SaveThemeRequested"),
            (r#"{"kind":"LoadThemeRequested"}"#, "LoadThemeRequested"),
        ] {
            let parsed: InteractionEvent = serde_json::from_str(raw).unwrap();
            let json = serde_json::to_value(&parsed).unwrap();
            assert_eq!(json["kind"], kind);
        }
    }

    #[test]
    fn set_element_animation_event_parses() {
        let raw = r#"{"kind":"SetElementAnimation","element_id":"el_a","category":"entrance","enabled":true}"#;
        let parsed: InteractionEvent = serde_json::from_str(raw).unwrap();
        match parsed {
            InteractionEvent::SetElementAnimation { element_id, category, enabled } => {
                assert_eq!(element_id, "el_a");
                assert_eq!(category, "entrance");
                assert!(enabled);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn add_and_update_animation_decode() {
        let raw = r#"{"kind":"AddAnimation","element_id":"el_a","catalog_id":"fade-in","direction":null}"#;
        match serde_json::from_str::<InteractionEvent>(raw).unwrap() {
            InteractionEvent::AddAnimation { element_id, catalog_id, .. } => {
                assert_eq!(element_id, "el_a");
                assert_eq!(catalog_id, "fade-in");
            }
            _ => panic!("wrong variant"),
        }
        let raw2 = r#"{"kind":"UpdateAnimation","animation_id":"a1","trigger":"on_click","duration_ms":700,"delay_ms":null,"easing":null,"iterations":null,"targets":null}"#;
        assert!(matches!(serde_json::from_str::<InteractionEvent>(raw2).unwrap(),
            InteractionEvent::UpdateAnimation { .. }));
    }

    #[test]
    fn slide_animations_update_and_notice_roundtrip() {
        let data = SlideAnimationsData {
            slide_id: "s1".into(),
            entries: vec![SlideAnimationEntry {
                animation_id: "anim_1".into(),
                element_id: "el_a".into(),
                category: "entrance".into(),
                effect_id: "appear".into(),
                keyframe: Some("appear".into()),
                targets: Vec::new(),
                trigger: "on_click".into(),
                duration_ms: 500,
                delay_ms: 0,
                easing: "ease".into(),
                iterations: crate::deck::animation::AnimationIterations::Count(1),
            }],
        };
        let msg = IpcMessage::new(MessageKind::SlideAnimationsUpdate(data.clone()));
        let back: IpcMessage =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        match back.kind {
            MessageKind::SlideAnimationsUpdate(d) => assert_eq!(d, data),
            other => panic!("unexpected variant: {other:?}"),
        }

        let n = IpcMessage::new(MessageKind::Notice {
            message: "moved".into(),
            detail: Some("the element was clamped into view".into()),
        });
        let back_n: IpcMessage =
            serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        match back_n.kind {
            MessageKind::Notice { message, detail } => {
                assert_eq!(message, "moved");
                assert_eq!(detail.as_deref(), Some("the element was clamped into view"));
            }
            other => panic!("unexpected variant: {other:?}"),
        }
        // A payload omitting `detail` still deserializes (serde default → None).
        let legacy: IpcMessage = serde_json::from_str(
            r#"{"id":"x","timestamp":0,"type":"Notice","payload":{"message":"hi"}}"#,
        )
        .unwrap();
        match legacy.kind {
            MessageKind::Notice { detail, .. } => assert_eq!(detail, None),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn editor_config_carries_keyframes() {
        let cfg = EditorConfig { debug: false, animation_keyframes_css: "@keyframes appear{}".into(), animation_catalog: Vec::new() };
        let back: EditorConfig =
            serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.animation_keyframes_css, "@keyframes appear{}");
    }

    #[test]
    fn interaction_clicked_roundtrip() {
        let event = InteractionEvent::ElementClicked {
            element_id: "el_a".into(),
            modifiers: Modifiers { shift: true, ..Default::default() },
            position: Point { x: 10.0, y: 20.0 },
        };
        let msg = IpcMessage::new(MessageKind::Interaction(event));
        let parsed: IpcMessage = round_trip(&msg);
        match parsed.kind {
            MessageKind::Interaction(InteractionEvent::ElementClicked {
                element_id, modifiers, position,
            }) => {
                assert_eq!(element_id, "el_a");
                assert!(modifiers.shift);
                assert!((position.x - 10.0).abs() < f64::EPSILON);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn interaction_dragged_payload_shape() {
        let event = InteractionEvent::ElementDragged {
            element_id: "el_a".into(),
            delta: Vec2 { x: 5.0, y: -3.0 },
            position: Point { x: 1.0, y: 2.0 },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "ElementDragged");
        assert_eq!(json["element_id"], "el_a");
        assert_eq!(json["delta"]["x"], 5.0);
        assert_eq!(json["delta"]["y"], -3.0);
    }

    #[test]
    fn patch_variants_all_roundtrip() {
        let patches = [
            Patch::SetAttribute { element_id: "a".into(), attribute: "data-x".into(), value: "1".into() },
            Patch::RemoveAttribute { element_id: "a".into(), attribute: "data-x".into() },
            Patch::SetStyle { element_id: "a".into(), property: "left".into(), value: "10px".into() },
            Patch::RemoveStyle { element_id: "a".into(), property: "left".into() },
            Patch::SetText { element_id: "a".into(), text: "hi".into() },
            Patch::SetInnerHtml { element_id: "a".into(), html: "<b/>".into() },
            Patch::ReplaceElement { element_id: "a".into(), new_html: "<b/>".into() },
            Patch::InsertElement { parent_id: "p".into(), position: 0, html: "<b/>".into() },
            Patch::RemoveElement { element_id: "a".into() },
            Patch::Batch { patches: vec![Patch::RemoveElement { element_id: "a".into() }] },
        ];
        for p in patches {
            let json = serde_json::to_string(&p).unwrap();
            let _back: Patch = serde_json::from_str(&json).unwrap();
        }
    }

    #[test]
    fn patch_op_tag_field_named_op() {
        let p = Patch::SetStyle {
            element_id: "a".into(),
            property: "left".into(),
            value: "10px".into(),
        };
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json["op"], "SetStyle");
    }

    #[test]
    fn js_style_envelope_parses() {
        // Mimic the literal JSON the JS bridge will post for Ready.
        let raw = r#"{"id":"abc","timestamp":1,"type":"Ready"}"#;
        let parsed: IpcMessage = serde_json::from_str(raw).unwrap();
        assert!(matches!(parsed.kind, MessageKind::Ready));
    }

    #[test]
    fn js_style_interaction_envelope_parses() {
        let raw = r#"{
            "id":"abc","timestamp":1,
            "type":"Interaction",
            "payload":{"kind":"BackgroundClicked","position":{"x":0,"y":0}}
        }"#;
        let parsed: IpcMessage = serde_json::from_str(raw).unwrap();
        assert!(matches!(
            parsed.kind,
            MessageKind::Interaction(InteractionEvent::BackgroundClicked { .. })
        ));
    }

    #[test]
    fn selection_state_default_is_empty() {
        let s = SelectionState::empty();
        assert!(s.slide_id.is_none());
        assert!(s.is_empty());
    }

    #[test]
    fn selection_state_toggle_adds_then_removes() {
        let mut s = SelectionState::default();
        s.toggle("a".into());
        assert!(s.contains("a"));
        s.toggle("a".into());
        assert!(!s.contains("a"));
    }

    #[test]
    fn selection_state_toggle_keeps_unrelated() {
        let mut s = SelectionState::default();
        s.toggle("a".into());
        s.toggle("b".into());
        s.toggle("a".into());
        assert!(!s.contains("a"));
        assert!(s.contains("b"));
    }

    #[test]
    fn selection_state_roundtrips_through_ipc() {
        let s = SelectionState {
            slide_id: Some("s1".into()),
            element_ids: vec!["e1".into(), "e2".into()],
        };
        let msg = IpcMessage::new(MessageKind::SetSelection(s.clone()));
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: IpcMessage = serde_json::from_str(&json).unwrap();
        match parsed.kind {
            MessageKind::SetSelection(back) => assert_eq!(back, s),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn ipc_message_constructor_assigns_clock() {
        let m = IpcMessage::new(MessageKind::Ready);
        assert!(!m.id.is_empty());
        assert!(m.timestamp > 0);
    }

    // ---------- Stage 9: object tree messages ----------

    #[test]
    fn object_tree_update_roundtrips_through_ipc() {
        let data = ObjectTreeData {
            slide_id: "s1".into(),
            root_id: "el_root".into(),
            nodes: vec![ObjectTreeNode {
                id: "el_a".into(),
                element_type: "text".into(),
                children: vec![],
            }, ObjectTreeNode {
                id: "el_g".into(),
                element_type: "group".into(),
                children: vec![ObjectTreeNode {
                    id: "el_inner".into(),
                    element_type: "shape".into(),
                    children: vec![],
                }],
            }],
        };
        let msg = IpcMessage::new(MessageKind::ObjectTreeUpdate(data.clone()));
        let json = serde_json::to_string(&msg).unwrap();
        let back: IpcMessage = serde_json::from_str(&json).unwrap();
        match back.kind {
            MessageKind::ObjectTreeUpdate(d) => assert_eq!(d, data),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn set_selection_from_panel_roundtrips() {
        let event = InteractionEvent::SetSelectionFromPanel {
            element_ids: vec!["e1".into(), "e2".into()],
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "SetSelectionFromPanel");
        let back: InteractionEvent = serde_json::from_value(json).unwrap();
        match back {
            InteractionEvent::SetSelectionFromPanel { element_ids } => {
                assert_eq!(element_ids, vec!["e1", "e2"]);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn insert_element_requested_parses_with_optional_fields_omitted() {
        let raw = r#"{"kind":"InsertElementRequested","element_type":"text"}"#;
        let parsed: InteractionEvent = serde_json::from_str(raw).unwrap();
        match parsed {
            InteractionEvent::InsertElementRequested {
                element_type, parent_id, position,
            } => {
                assert_eq!(element_type, "text");
                assert!(parent_id.is_none());
                assert!(position.is_none());
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn insert_element_requested_with_parent_and_position_roundtrips() {
        let event = InteractionEvent::InsertElementRequested {
            element_type: "shape".into(),
            parent_id: Some("el_group".into()),
            position: Some(2),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: InteractionEvent = serde_json::from_str(&json).unwrap();
        match back {
            InteractionEvent::InsertElementRequested {
                element_type, parent_id, position,
            } => {
                assert_eq!(element_type, "shape");
                assert_eq!(parent_id.as_deref(), Some("el_group"));
                assert_eq!(position, Some(2));
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn asset_imported_event_roundtrips() {
        let event = InteractionEvent::AssetImported {
            content_base64: "ZmFrZS1ieXRlcw==".into(),
            original_filename: "logo.png".into(),
            media_type: "image/png".into(),
            width: 800,
            height: 600,
            position: Some(Point { x: 100.0, y: 200.0 }),
            as_slide_background: false,
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: InteractionEvent = serde_json::from_str(&json).unwrap();
        match back {
            InteractionEvent::AssetImported {
                content_base64, original_filename, media_type, width, height, position, ..
            } => {
                assert_eq!(content_base64, "ZmFrZS1ieXRlcw==");
                assert_eq!(original_filename, "logo.png");
                assert_eq!(media_type, "image/png");
                assert_eq!(width, 800);
                assert_eq!(height, 600);
                assert!(position.is_some());
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn asset_imported_event_parses_with_optional_position_omitted() {
        let raw = r#"{
            "kind":"AssetImported",
            "content_base64":"AA==",
            "original_filename":"x.png",
            "media_type":"image/png",
            "width":10,
            "height":10
        }"#;
        let parsed: InteractionEvent = serde_json::from_str(raw).unwrap();
        match parsed {
            InteractionEvent::AssetImported { position, .. } => assert!(position.is_none()),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn asset_added_and_assets_update_roundtrip() {
        let payload = AssetPayload {
            asset_id: "asset_abc".into(),
            media_type: "image/png".into(),
            content_base64: "ZmFrZS1ieXRlcw==".into(),
        };
        let msg_one = IpcMessage::new(MessageKind::AssetAdded(payload.clone()));
        let back_one: IpcMessage = serde_json::from_str(&serde_json::to_string(&msg_one).unwrap()).unwrap();
        match back_one.kind {
            MessageKind::AssetAdded(p) => assert_eq!(p, payload),
            other => panic!("unexpected variant: {other:?}"),
        }

        let bundle = AssetsBundle { assets: vec![payload] };
        let msg_all = IpcMessage::new(MessageKind::AssetsUpdate(bundle.clone()));
        let back_all: IpcMessage = serde_json::from_str(&serde_json::to_string(&msg_all).unwrap()).unwrap();
        match back_all.kind {
            MessageKind::AssetsUpdate(b) => assert_eq!(b, bundle),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn slide_list_update_roundtrips_through_ipc() {
        let data = SlideListData {
            slides: vec![
                SlideListEntry {
                    slide_id: "s1".into(),
                    title: "Title".into(),
                    html: "<section/>".into(),
                },
                SlideListEntry {
                    slide_id: "s2".into(),
                    title: "Second".into(),
                    html: "<section/>".into(),
                },
            ],
            active_slide_id: Some("s1".into()),
            theme_css: ".x{}".into(),
            width: 1920,
            height: 1080,
        };
        let msg = IpcMessage::new(MessageKind::SlideListUpdate(data.clone()));
        let json = serde_json::to_string(&msg).unwrap();
        let back: IpcMessage = serde_json::from_str(&json).unwrap();
        match back.kind {
            MessageKind::SlideListUpdate(d) => assert_eq!(d, data),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn rename_and_reparent_event_payloads_parse() {
        let rename: InteractionEvent = serde_json::from_str(
            r#"{"kind":"RenameElementRequested","element_id":"el_a","new_name":"Header"}"#,
        )
        .unwrap();
        assert!(matches!(
            rename,
            InteractionEvent::RenameElementRequested { ref element_id, ref new_name }
                if element_id == "el_a" && new_name == "Header"
        ));

        let reparent: InteractionEvent = serde_json::from_str(
            r#"{"kind":"ReparentElementRequested","element_id":"el_a","new_parent_id":"el_g","new_position":1}"#,
        )
        .unwrap();
        assert!(matches!(
            reparent,
            InteractionEvent::ReparentElementRequested {
                ref element_id, ref new_parent_id, new_position: 1,
            } if element_id == "el_a" && new_parent_id == "el_g"
        ));
    }

    #[test]
    fn text_edit_events_parse_from_js_envelopes() {
        let started: InteractionEvent =
            serde_json::from_str(r#"{"kind":"TextEditStarted","element_id":"el_t"}"#).unwrap();
        assert!(matches!(
            started,
            InteractionEvent::TextEditStarted { ref element_id } if element_id == "el_t"
        ));

        // TextEditEnded now carries the committed plain text, including the
        // empty-string case (the user cleared the element).
        let ended: InteractionEvent = serde_json::from_str(
            r#"{"kind":"TextEditEnded","element_id":"el_t","text":"Hello world"}"#,
        )
        .unwrap();
        assert!(matches!(
            ended,
            InteractionEvent::TextEditEnded { ref element_id, ref text }
                if element_id == "el_t" && text == "Hello world"
        ));

        let cleared: InteractionEvent = serde_json::from_str(
            r#"{"kind":"TextEditEnded","element_id":"el_t","text":""}"#,
        )
        .unwrap();
        assert!(matches!(
            cleared,
            InteractionEvent::TextEditEnded { ref text, .. } if text.is_empty()
        ));
    }

    #[test]
    fn slide_title_and_element_id_edit_events_parse() {
        let title: InteractionEvent = serde_json::from_str(
            r#"{"kind":"SlideTitleEditRequested","slide_id":"s1","new_title":"Intro"}"#,
        )
        .unwrap();
        assert!(matches!(
            title,
            InteractionEvent::SlideTitleEditRequested { ref slide_id, ref new_title }
                if slide_id == "s1" && new_title == "Intro"
        ));

        // new_id arrives raw (whitespace and all); the Rust side sanitizes.
        let id_event: InteractionEvent = serde_json::from_str(
            r#"{"kind":"ElementIdEditRequested","element_id":"el_a","new_id":"el b"}"#,
        )
        .unwrap();
        assert!(matches!(
            id_event,
            InteractionEvent::ElementIdEditRequested { ref element_id, ref new_id }
                if element_id == "el_a" && new_id == "el b"
        ));
    }
}
