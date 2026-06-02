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
    },
    TextEditStarted {
        element_id: ElementId,
    },
    TextEdited {
        element_id: ElementId,
        delta: RichTextDelta,
    },
    TextEditEnded {
        element_id: ElementId,
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
// One-shot configuration the webview reads at startup. Placeholder fields.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct EditorConfig {
    pub debug: bool,
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
// the matching badge icon. `display_name` is the rename-able label;
// when an element has no `name` set, the Rust side passes the element
// id verbatim and the JS panel renders that.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ObjectTreeNode {
    pub id: ElementId,
    pub element_type: String,
    pub display_name: String,
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
        }));
        let parsed: IpcMessage = round_trip(&msg);
        match parsed.kind {
            MessageKind::MountSlide(args) => {
                assert_eq!(args.slide_id, "s1");
                assert_eq!(args.slide_html, "<section/>");
                assert_eq!(args.theme_css, ".x{}");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
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
                display_name: "Title".into(),
                children: vec![],
            }, ObjectTreeNode {
                id: "el_g".into(),
                element_type: "group".into(),
                display_name: "el_g".into(),
                children: vec![ObjectTreeNode {
                    id: "el_inner".into(),
                    element_type: "shape".into(),
                    display_name: "el_inner".into(),
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
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: InteractionEvent = serde_json::from_str(&json).unwrap();
        match back {
            InteractionEvent::AssetImported {
                content_base64, original_filename, media_type, width, height, position,
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
}
