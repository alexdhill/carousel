pub mod agent;
pub mod bridge;
pub mod landing;
pub mod present;
pub mod presenter;

use serde::{Deserialize, Serialize};

pub type SlideId = String;
pub type ElementId = String;
#[allow(dead_code)]
pub type AssetId = String;
#[allow(dead_code)]
pub type LayoutId = String;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IpcMessage {
    pub id: String,
    pub timestamp: u64,
    #[serde(flatten)]
    pub kind: MessageKind,
}

impl IpcMessage {
    pub fn new(kind: MessageKind) -> Self {
        let id: String = ulid::Ulid::new().to_string();
        let timestamp: u64 = now_millis();
        Self {
            id,
            timestamp,
            kind,
        }
    }
}

fn now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now();
    let delta = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    delta.as_millis() as u64
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "payload")]
pub enum MessageKind {
    Ready,
    Interaction(InteractionEvent),
    ThumbnailGenerated(ThumbnailResult),
    Error {
        code: String,
        message: String,
    },

    AgentPromptSubmitted {
        text: String,
        #[serde(default)]
        agent: String,
    },

    AgentCancelRequested,

    AgentAddRequested {
        name: String,
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },

    AgentPanelToggled {
        open: bool,
    },

    AgentPermissionReply {
        request_id: String,
        allow: bool,
    },

    WindowControl {
        action: String,
    },

    MountSlide(MountSlideArgs),
    ApplyPatch(Patch),
    SetSelection(SelectionState),
    SetTheme(SetThemeArgs),
    Configure(EditorConfig),
    RequestThumbnail(ThumbnailRequest),

    ObjectTreeUpdate(ObjectTreeData),

    SlideListUpdate(SlideListData),

    AssetsUpdate(AssetsBundle),

    AssetAdded(AssetPayload),

    LayoutListUpdate(LayoutListData),

    ChromiumDownloadProgress {
        received: u64,
        total: Option<u64>,
    },
    ChromiumDownloadDone {
        ok: bool,
        message: String,
    },

    SlideLayoutPickerData(LayoutListData),

    SetMode {
        mode: String,
    },

    SlideAnimationsUpdate(SlideAnimationsData),

    GuidesUpdate(GuidesData),

    FontList {
        families: Vec<String>,
    },

    Notice {
        message: String,
        #[serde(default)]
        detail: Option<String>,
    },

    PresentInit(present::PresentInitPayload),

    PresentAssets(AssetsBundle),

    PresentSlide(present::PresentSlidePayload),

    PresentReveal(present::RevealPayload),

    PresenterUpdate(presenter::PresenterUpdatePayload),

    SlideInspectorUpdate(SlideInspectorData),

    SaveStateUpdate(bool),

    ShowQuitDialog,

    AgentPanelStateUpdate(agent::AgentPanelState),

    AgentStream(agent::AgentStreamChunk),

    AgentTool(agent::AgentToolNotice),

    AgentPermission(agent::AgentPermissionAsk),

    AgentListUpdate(agent::AgentList),

    AgentActivityUpdate(agent::AgentActivity),

    AgentThoughtUpdate(agent::AgentThought),

    AgentToolStatusUpdate(agent::AgentToolStatus),
}

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

    ElementsDragEnded {
        element_ids: Vec<ElementId>,
        delta: Vec2,
    },

    ScaleElements {
        element_ids: Vec<ElementId>,
        factor: f64,
        anchor: Point,
    },

    ElementResizeStarted {
        element_id: ElementId,
        handle: ResizeHandle,
        position: Point,
    },

    ElementResized {
        element_id: ElementId,
        handle: ResizeHandle,
        new_size: Size,
        new_position: Point,
    },

    ElementResizeEnded {
        element_id: ElementId,
        new_position: Point,
        new_size: Size,

        #[serde(default)]
        background_size: Option<String>,
        #[serde(default)]
        background_position: Option<String>,
    },

    ElementCropCommitted {
        element_id: ElementId,
        new_position: Point,
        new_size: Size,
        background_size: String,
        background_position: String,
    },

    CopyRequested {
        scope: ClipboardScope,
    },
    CutRequested {
        scope: ClipboardScope,
    },
    PasteRequested,

    RemoveSlideRequested {
        slide_id: SlideId,
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
        content: crate::deck::element::RichText,
    },

    EmbedHtmlEditRequested {
        element_id: ElementId,
        html: String,
    },

    CellTextEditRequested {
        element_id: ElementId,
        row: usize,
        col: usize,
        text: String,
    },

    CellStyleChanged {
        element_id: ElementId,
        cells: Vec<[usize; 2]>,
        property: String,
        value: String,
    },

    TableInsertRow {
        element_id: ElementId,
        at: usize,
    },
    TableDeleteRow {
        element_id: ElementId,
        at: usize,
    },
    TableInsertColumn {
        element_id: ElementId,
        at: usize,
    },
    TableDeleteColumn {
        element_id: ElementId,
        at: usize,
    },
    TableSetHeaderRows {
        element_id: ElementId,
        count: usize,
    },
    TableSetHeaderColumns {
        element_id: ElementId,
        count: usize,
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

    PropertyChanged {
        element_id: ElementId,
        property: String,
        value: String,
    },

    GuideAdded {
        axis: String,
        pos: f64,
    },
    GuideMoved {
        index: usize,
        pos: f64,
    },
    GuideRemoved {
        index: usize,
    },

    SetSelectionFromPanel {
        element_ids: Vec<ElementId>,
    },

    InsertElementRequested {
        element_type: String,
        #[serde(default)]
        parent_id: Option<ElementId>,
        #[serde(default)]
        position: Option<usize>,
    },

    RenameElementRequested {
        element_id: ElementId,
        new_name: String,
    },

    ReparentElementRequested {
        element_id: ElementId,
        new_parent_id: ElementId,
        new_position: usize,
    },

    AddSlideRequested {
        #[serde(default)]
        layout_id: String,
    },

    SlideLayoutPickerRequested,

    SlideTitleEditRequested {
        slide_id: SlideId,
        new_title: String,
    },

    SlideThumbnailReordered {
        slide_id: SlideId,
        new_index: usize,
    },

    ElementIdEditRequested {
        element_id: ElementId,
        new_id: String,
    },

    AssetImported {
        content_base64: String,
        original_filename: String,
        media_type: String,
        width: u32,
        height: u32,
        #[serde(default)]
        position: Option<Point>,

        #[serde(default)]
        as_slide_background: bool,

        #[serde(default)]
        as_element_fill: Option<String>,
    },

    SetEditorMode {
        mode: String,
    },

    LayoutThumbnailClicked {
        layout_id: LayoutId,
    },

    AddLayoutRequested,

    LayoutNameEditRequested {
        layout_id: LayoutId,
        new_name: String,
    },

    GlobalsCssEditRequested {
        new_css: String,
    },

    SetElementAnimation {
        element_id: ElementId,
        category: String,
        enabled: bool,
    },

    AddAnimation {
        element_id: ElementId,
        catalog_id: String,
        #[serde(default)]
        direction: Option<String>,
    },

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

    RemoveAnimationRequested {
        animation_id: String,
    },

    MoveAnimation {
        animation_id: String,
        new_index: usize,
        trigger: String,
    },

    SaveThemeRequested,
    LoadThemeRequested,

    SetSlideBackgroundRequested {
        background: String,
    },

    SetSlideBackgroundImageCleared,
    SetSlideNotesRequested {
        notes: String,
    },

    SetSlideTransitionRequested {
        transition: Option<crate::deck::SlideTransition>,
    },

    SetMorphTransitionRequested {
        element_id: ElementId,
        enabled: bool,
        duration_ms: u32,
        easing: String,
    },
    SetSlideLayoutRequested {
        layout_id: LayoutId,
    },

    SetDeckTitleRequested {
        title: String,
    },
    NudgeSelectionRequested {
        dx: f64,
        dy: f64,
    },

    NavigateSlideRequested {
        forward: bool,
    },

    SetGroupLayout {
        element_id: ElementId,
        #[serde(default)]
        direction: Option<String>,
        #[serde(default)]
        distribution: Option<String>,
        #[serde(default)]
        alignment: Option<String>,
    },

    SetGroupScale {
        element_id: ElementId,
        scale: f64,
    },

    GroupSelectionRequested {
        element_ids: Vec<ElementId>,
    },

    AlignSelectionRequested {
        element_ids: Vec<ElementId>,
        op: String,
    },

    QuitConfirmed {
        save: bool,
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

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct RichTextDelta {
    pub ops: Vec<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ThumbnailResult {
    pub slide_id: SlideId,
    pub png_base64: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MountSlideArgs {
    pub slide_id: SlideId,
    pub slide_html: String,
    pub theme_css: String,

    pub globals_css: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectionState {
    pub slide_id: Option<SlideId>,
    pub element_ids: Vec<ElementId>,
}

impl SelectionState {
    pub fn empty() -> Self {
        Self::default()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.element_ids.is_empty()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.element_ids.iter().any(|e| e == id)
    }

    pub fn toggle(&mut self, id: ElementId) {
        assert!(!id.is_empty(), "toggle called with empty id");
        if let Some(pos) = self.element_ids.iter().position(|e| e == &id) {
            self.element_ids.remove(pos);
        } else {
            self.element_ids.push(id);
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SetThemeArgs {
    pub theme_id: String,
    pub theme_css: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct EditorConfig {
    pub debug: bool,

    #[serde(default)]
    pub animation_keyframes_css: String,

    #[serde(default)]
    pub animation_catalog: Vec<crate::deck::anim_catalog::AnimCatalogItem>,

    #[serde(default)]
    pub deck_title: String,

    #[serde(default)]
    pub focus_title: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ThumbnailRequest {
    pub slide_id: SlideId,
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ObjectTreeData {
    pub slide_id: SlideId,
    pub root_id: ElementId,
    pub nodes: Vec<ObjectTreeNode>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ObjectTreeNode {
    pub id: ElementId,
    pub element_type: String,
    pub children: Vec<ObjectTreeNode>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AssetPayload {
    pub asset_id: String,
    pub media_type: String,
    pub content_base64: String,

    #[serde(default)]
    pub original_filename: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AssetsBundle {
    pub assets: Vec<AssetPayload>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SlideListData {
    pub slides: Vec<SlideListEntry>,
    pub active_slide_id: Option<SlideId>,
    pub theme_css: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SlideListEntry {
    pub slide_id: SlideId,
    pub title: String,
    pub html: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct LayoutListData {
    pub layouts: Vec<LayoutListEntry>,
    pub active_layout_id: Option<LayoutId>,
    pub theme_css: String,
    pub globals_css: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct LayoutListEntry {
    pub layout_id: LayoutId,
    pub name: String,
    pub html: String,

    #[serde(default)]
    pub background: String,
    #[serde(default)]
    pub background_image: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SlideAnimationsData {
    pub slide_id: SlideId,
    pub entries: Vec<SlideAnimationEntry>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct GuideDto {
    pub axis: String,
    pub pos: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct GuidesData {
    pub canvas_id: String,
    pub own: Vec<GuideDto>,
    pub inherited: Vec<GuideDto>,
}

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

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SlideInspectorLayout {
    pub id: LayoutId,
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SlideInspectorData {
    pub slide_id: SlideId,
    pub title: String,
    pub notes: String,
    pub background: String,

    #[serde(default)]
    pub background_image: String,

    #[serde(default)]
    pub transition: Option<crate::deck::SlideTransition>,
    pub layout_id: LayoutId,
    pub layouts: Vec<SlideInspectorLayout>,
}

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
        #[serde(skip_serializing_if = "Option::is_none", default)]
        src: Option<String>,
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

    Batch {
        patches: Vec<Patch>,
    },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

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
        let back: IpcMessage = serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        match back.kind {
            MessageKind::LayoutListUpdate(d) => assert_eq!(d, data),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn chromium_progress_roundtrips() {
        let m = MessageKind::ChromiumDownloadProgress {
            received: 5,
            total: Some(10),
        };
        let j = serde_json::to_string(&m).unwrap();
        assert!(j.contains("ChromiumDownloadProgress"));
        let back: MessageKind = serde_json::from_str(&j).unwrap();
        match back {
            MessageKind::ChromiumDownloadProgress { received, total } => {
                assert_eq!(received, 5);
                assert_eq!(total, Some(10));
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn set_mode_echo_roundtrips() {
        let msg = IpcMessage::new(MessageKind::SetMode {
            mode: "layout".into(),
        });
        let back: IpcMessage = serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        match back.kind {
            MessageKind::SetMode { mode } => assert_eq!(mode, "layout"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn slide_inspector_payload_and_events_roundtrip() {
        let data = SlideInspectorData {
            slide_id: "s1".into(),
            title: "Intro".into(),
            notes: "speak up".into(),
            background: "#222".into(),
            background_image: String::new(),
            transition: None,
            layout_id: "title".into(),
            layouts: vec![SlideInspectorLayout {
                id: "blank".into(),
                name: "Blank".into(),
            }],
        };
        let msg = IpcMessage::new(MessageKind::SlideInspectorUpdate(data.clone()));
        let back: IpcMessage = serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
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
        assert!(matches!(
            serde_json::from_str::<InteractionEvent>(a).unwrap(),
            InteractionEvent::SetGroupLayout { .. }
        ));
        let b = r#"{"kind":"SetGroupScale","element_id":"g","scale":1.5}"#;
        assert!(matches!(
            serde_json::from_str::<InteractionEvent>(b).unwrap(),
            InteractionEvent::SetGroupScale { .. }
        ));
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
            InteractionEvent::SetElementAnimation {
                element_id,
                category,
                enabled,
            } => {
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
            InteractionEvent::AddAnimation {
                element_id,
                catalog_id,
                ..
            } => {
                assert_eq!(element_id, "el_a");
                assert_eq!(catalog_id, "fade-in");
            }
            _ => panic!("wrong variant"),
        }
        let raw2 = r#"{"kind":"UpdateAnimation","animation_id":"a1","trigger":"on_click","duration_ms":700,"delay_ms":null,"easing":null,"iterations":null,"targets":null}"#;
        assert!(matches!(
            serde_json::from_str::<InteractionEvent>(raw2).unwrap(),
            InteractionEvent::UpdateAnimation { .. }
        ));
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
        let back: IpcMessage = serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        match back.kind {
            MessageKind::SlideAnimationsUpdate(d) => assert_eq!(d, data),
            other => panic!("unexpected variant: {other:?}"),
        }

        let n = IpcMessage::new(MessageKind::Notice {
            message: "moved".into(),
            detail: Some("the element was clamped into view".into()),
        });
        let back_n: IpcMessage = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        match back_n.kind {
            MessageKind::Notice { message, detail } => {
                assert_eq!(message, "moved");
                assert_eq!(detail.as_deref(), Some("the element was clamped into view"));
            }
            other => panic!("unexpected variant: {other:?}"),
        }

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
        let cfg = EditorConfig {
            debug: false,
            animation_keyframes_css: "@keyframes appear{}".into(),
            animation_catalog: Vec::new(),
            deck_title: "Deck".into(),
            focus_title: true,
        };
        let back: EditorConfig =
            serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.animation_keyframes_css, "@keyframes appear{}");
    }

    #[test]
    fn interaction_clicked_roundtrip() {
        let event = InteractionEvent::ElementClicked {
            element_id: "el_a".into(),
            modifiers: Modifiers {
                shift: true,
                ..Default::default()
            },
            position: Point { x: 10.0, y: 20.0 },
        };
        let msg = IpcMessage::new(MessageKind::Interaction(event));
        let parsed: IpcMessage = round_trip(&msg);
        match parsed.kind {
            MessageKind::Interaction(InteractionEvent::ElementClicked {
                element_id,
                modifiers,
                position,
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
            Patch::SetAttribute {
                element_id: "a".into(),
                attribute: "data-x".into(),
                value: "1".into(),
            },
            Patch::RemoveAttribute {
                element_id: "a".into(),
                attribute: "data-x".into(),
            },
            Patch::SetStyle {
                element_id: "a".into(),
                property: "left".into(),
                value: "10px".into(),
            },
            Patch::RemoveStyle {
                element_id: "a".into(),
                property: "left".into(),
            },
            Patch::SetText {
                element_id: "a".into(),
                text: "hi".into(),
                src: None,
            },
            Patch::SetInnerHtml {
                element_id: "a".into(),
                html: "<b/>".into(),
            },
            Patch::ReplaceElement {
                element_id: "a".into(),
                new_html: "<b/>".into(),
            },
            Patch::InsertElement {
                parent_id: "p".into(),
                position: 0,
                html: "<b/>".into(),
            },
            Patch::RemoveElement {
                element_id: "a".into(),
            },
            Patch::Batch {
                patches: vec![Patch::RemoveElement {
                    element_id: "a".into(),
                }],
            },
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

    #[test]
    fn object_tree_update_roundtrips_through_ipc() {
        let data = ObjectTreeData {
            slide_id: "s1".into(),
            root_id: "el_root".into(),
            nodes: vec![
                ObjectTreeNode {
                    id: "el_a".into(),
                    element_type: "text".into(),
                    children: vec![],
                },
                ObjectTreeNode {
                    id: "el_g".into(),
                    element_type: "group".into(),
                    children: vec![ObjectTreeNode {
                        id: "el_inner".into(),
                        element_type: "shape".into(),
                        children: vec![],
                    }],
                },
            ],
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
    fn align_selection_requested_roundtrips() {
        let event = InteractionEvent::AlignSelectionRequested {
            element_ids: vec!["e1".into(), "e2".into(), "e3".into()],
            op: "distribute-h".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "AlignSelectionRequested");
        let back: InteractionEvent = serde_json::from_value(json).unwrap();
        match back {
            InteractionEvent::AlignSelectionRequested { element_ids, op } => {
                assert_eq!(element_ids, vec!["e1", "e2", "e3"]);
                assert_eq!(op, "distribute-h");
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
                element_type,
                parent_id,
                position,
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
                element_type,
                parent_id,
                position,
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
            as_element_fill: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: InteractionEvent = serde_json::from_str(&json).unwrap();
        match back {
            InteractionEvent::AssetImported {
                content_base64,
                original_filename,
                media_type,
                width,
                height,
                position,
                ..
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
            original_filename: "logo.png".into(),
        };
        let msg_one = IpcMessage::new(MessageKind::AssetAdded(payload.clone()));
        let back_one: IpcMessage =
            serde_json::from_str(&serde_json::to_string(&msg_one).unwrap()).unwrap();
        match back_one.kind {
            MessageKind::AssetAdded(p) => assert_eq!(p, payload),
            other => panic!("unexpected variant: {other:?}"),
        }

        let bundle = AssetsBundle {
            assets: vec![payload],
        };
        let msg_all = IpcMessage::new(MessageKind::AssetsUpdate(bundle.clone()));
        let back_all: IpcMessage =
            serde_json::from_str(&serde_json::to_string(&msg_all).unwrap()).unwrap();
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

        let ended: InteractionEvent = serde_json::from_str(
            r#"{"kind":"TextEditEnded","element_id":"el_t","content":{"plain":"Hello world"}}"#,
        )
        .unwrap();
        assert!(matches!(
            ended,
            InteractionEvent::TextEditEnded { ref element_id, ref content }
                if element_id == "el_t" && content.plain == "Hello world" && content.is_plain()
        ));

        let cleared: InteractionEvent = serde_json::from_str(
            r#"{"kind":"TextEditEnded","element_id":"el_t","content":{"plain":""}}"#,
        )
        .unwrap();
        assert!(matches!(
            cleared,
            InteractionEvent::TextEditEnded { ref content, .. } if content.plain.is_empty()
        ));
    }

    #[test]
    fn rich_text_edit_events_parse_from_js_envelopes() {
        let ended: InteractionEvent = serde_json::from_str(
            r#"{"kind":"TextEditEnded","element_id":"el_t","content":{"plain":"ab","runs":[{"start":0,"end":1,"marks":{"bold":true}}]}}"#,
        )
        .unwrap();
        match ended {
            InteractionEvent::TextEditEnded { content, .. } => {
                assert_eq!(content.runs.len(), 1);
                assert!(content.runs[0].marks.bold);
            }
            other => panic!("expected TextEditEnded, got {other:?}"),
        }
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

    #[test]
    fn agent_prompt_submitted_roundtrips_through_ipc() {
        let msg = IpcMessage::new(MessageKind::AgentPromptSubmitted {
            text: "refactor this slide".into(),
            agent: "Claude".into(),
        });
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: IpcMessage = serde_json::from_str(&json).unwrap();
        match parsed.kind {
            MessageKind::AgentPromptSubmitted { text, agent } => {
                assert_eq!(text, "refactor this slide");
                assert_eq!(agent, "Claude");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn agent_panel_state_update_roundtrips_through_ipc() {
        let state = agent::AgentPanelState {
            running: true,
            error: None,
        };
        let msg = IpcMessage::new(MessageKind::AgentPanelStateUpdate(state.clone()));
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: IpcMessage = serde_json::from_str(&json).unwrap();
        match parsed.kind {
            MessageKind::AgentPanelStateUpdate(s) => {
                assert_eq!(s, state);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn agent_activity_update_roundtrips_through_ipc() {
        let activity = agent::AgentActivity {
            phase: "thinking".into(),
            label: "Analyzing slide content".into(),
        };
        let msg = IpcMessage::new(MessageKind::AgentActivityUpdate(activity.clone()));
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: IpcMessage = serde_json::from_str(&json).unwrap();
        match parsed.kind {
            MessageKind::AgentActivityUpdate(a) => {
                assert_eq!(a, activity);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }
}
