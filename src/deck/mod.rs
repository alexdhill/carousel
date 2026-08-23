#![allow(dead_code, unused_imports)]

pub mod anim_catalog;
pub mod animation;
pub mod builders;
pub mod canvas;
pub mod element;
pub mod group_layout;
pub mod guide;
pub mod ids;
pub mod layout;
pub mod slide;
pub mod style;
pub mod templates;
pub mod theme;

use crate::bundle::{AssetRegistry, ManifestData, SlideEntry, manifest::slide_path_for};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

pub use animation::{
    AnimationCategory, AnimationEntry, AnimationIterations, AnimationState, AnimationTiming,
    AnimationTrigger,
};
pub use canvas::{Canvas, InsertError, RemovedElement};
pub use element::{
    AssetRef, ElementContent, ElementNode, ElementStyle, ElementType, RichText, ShapeGeometry,
    TableCell, TableData,
};
pub use guide::{Guide, GuideAxis};
pub use ids::{
    AnimationId, AssetId, ElementId, LayoutId, SlideId, new_animation_id, new_element_id,
    new_slide_id,
};
pub use layout::LayoutNode;
pub use slide::{SlideMetadata, SlideNode, SlideTransition, TransitionKind};
pub use style::{
    Border, BorderStyle, ColorRef, FillRef, Filter, FontRef, FontStyle, Geometry, ImageStyle,
    Length, LengthUnit, MediaStyle, ObjectFit, Shadow, ShapeStyle, Stroke, TableStyle, TextAlign,
    TextStyle,
};
pub use theme::ThemeData;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CanvasTarget {
    Slide(SlideId),
    Layout(LayoutId),
}

impl CanvasTarget {
    pub fn id(&self) -> &str {
        match self {
            CanvasTarget::Slide(id) => id,
            CanvasTarget::Layout(id) => id,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Deck {
    pub manifest: ManifestData,
    pub theme: ThemeData,
    pub slides: BTreeMap<SlideId, SlideNode>,
    pub slide_order: Vec<SlideId>,
    pub assets: AssetRegistry,
    pub dirty_slides: HashSet<SlideId>,
    pub manifest_dirty: bool,
    pub bundle_path: Option<PathBuf>,
}

impl Deck {
    pub fn effective_slide_bg(&self, slide: &SlideNode) -> (Option<String>, Option<String>) {
        let layout: Option<&crate::deck::layout::LayoutNode> =
            self.theme.layouts.get(&slide.layout_id);
        let pick = |own: &Option<String>, lay: Option<&String>| -> Option<String> {
            match own {
                Some(s) if !s.is_empty() => Some(s.clone()),
                _ => lay.filter(|s| !s.is_empty()).cloned(),
            }
        };
        let fill = pick(
            &slide.metadata.background,
            layout.and_then(|l| l.background.as_ref()),
        );
        let img = pick(
            &slide.metadata.background_image,
            layout.and_then(|l| l.background_image.as_ref()),
        );
        (fill, img)
    }

    pub fn inherited_guides(&self, slide: &SlideNode) -> Vec<crate::deck::guide::Guide> {
        match self.theme.layouts.get(&slide.layout_id) {
            Some(layout) => layout.guides.clone(),
            None => Vec::new(),
        }
    }

    pub fn new_blank() -> Self {
        use builders::group_element;
        let slide_id: SlideId = new_slide_id();
        let root: ElementNode = group_element("el_root", vec![]);
        let slide: SlideNode = SlideNode::new(slide_id.clone(), "blank".into(), root);
        let mut slides: BTreeMap<SlideId, SlideNode> = BTreeMap::new();
        slides.insert(slide_id.clone(), slide);

        let manifest: ManifestData = ManifestData {
            slides: vec![crate::bundle::SlideEntry {
                id: slide_id.clone(),
                path: slide_path_for(&slide_id),
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
            }],
            ..ManifestData::default()
        };

        Self {
            manifest,
            theme: ThemeData::default(),
            slides,
            slide_order: vec![slide_id],
            assets: AssetRegistry::new_empty(),
            dirty_slides: HashSet::new(),
            manifest_dirty: false,
            bundle_path: None,
        }
    }

    pub fn sample() -> Self {
        use builders::{group_element, text_element_styled};

        let title_id: ElementId = "el_demo_title".into();
        let subtitle_id: ElementId = "el_demo_subtitle".into();
        let body_id: ElementId = "el_demo_body".into();

        let title = text_element_styled(
            title_id,
            "Hello from the in-memory tree.",
            Geometry {
                x: 120.0,
                y: 200.0,
                width: 1680.0,
                height: 120.0,
                ..Default::default()
            },
            TextStyle {
                font_size: Length::px(72.0),
                font_weight: 700,
                color: ColorRef::Theme("accent".into()),
                font_family: FontRef::Theme("title_family".into()),
                ..TextStyle::default()
            },
        );

        let subtitle = text_element_styled(
            subtitle_id,
            "Slide HTML now produced by the Rust serializer.",
            Geometry {
                x: 120.0,
                y: 340.0,
                width: 1680.0,
                height: 60.0,
                ..Default::default()
            },
            TextStyle {
                font_size: Length::px(36.0),
                color: ColorRef::Theme("muted".into()),
                ..TextStyle::default()
            },
        );

        let body = text_element_styled(
            body_id,
            "Edit Deck::sample in src/deck/mod.rs and recompile.",
            Geometry {
                x: 120.0,
                y: 460.0,
                width: 1680.0,
                height: 60.0,
                ..Default::default()
            },
            TextStyle {
                font_size: Length::px(28.0),
                color: ColorRef::Literal("#444".into()),
                ..TextStyle::default()
            },
        );

        let root: ElementNode = group_element("el_slide_root", vec![title, subtitle, body]);
        let slide_id: SlideId = "slide_demo".into();
        let slide: SlideNode = SlideNode::new(slide_id.clone(), "title".into(), root);

        let mut slides: BTreeMap<SlideId, SlideNode> = BTreeMap::new();
        slides.insert(slide_id.clone(), slide);

        let manifest: ManifestData = ManifestData {
            slides: vec![SlideEntry {
                id: slide_id.clone(),
                path: slide_path_for(&slide_id),
                layout_id: "title".into(),
                title: "Sample slide".into(),
                thumbnail: None,
                transition: None,
                duration_hint: None,
                notes_ref: None,
                animations: Vec::new(),
                guides: Vec::new(),
                background: None,
                background_image: None,
                notes: None,
            }],
            ..ManifestData::default()
        };

        Self {
            manifest,
            theme: ThemeData::default(),
            slides,
            slide_order: vec![slide_id],
            assets: AssetRegistry::new_empty(),
            dirty_slides: HashSet::new(),
            manifest_dirty: false,
            bundle_path: None,
        }
    }

    pub fn active_slide(&self) -> Option<&SlideNode> {
        let first: &SlideId = self.slide_order.first()?;
        self.slides.get(first)
    }

    pub fn canvas(&self, target: &CanvasTarget) -> Option<&dyn Canvas> {
        match target {
            CanvasTarget::Slide(id) => self.slides.get(id).map(|s| s as &dyn Canvas),
            CanvasTarget::Layout(id) => self.theme.layouts.get(id).map(|l| l as &dyn Canvas),
        }
    }

    pub fn canvas_mut(&mut self, target: &CanvasTarget) -> Option<&mut dyn Canvas> {
        match target {
            CanvasTarget::Slide(id) => self.slides.get_mut(id).map(|s| s as &mut dyn Canvas),
            CanvasTarget::Layout(id) => {
                self.theme.layouts.get_mut(id).map(|l| l as &mut dyn Canvas)
            }
        }
    }

    pub fn has_unsaved_changes(&self) -> bool {
        !self.dirty_slides.is_empty()
            || self.manifest_dirty
            || self.theme.layouts.values().any(|l| l.dirty)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn sample_has_one_slide_with_three_elements() {
        let d = Deck::sample();
        assert_eq!(d.slides.len(), 1);
        assert_eq!(d.slide_order.len(), 1);
        let s = d.active_slide().unwrap();
        assert_eq!(s.root.children.len(), 3);
    }

    #[test]
    fn sample_manifest_matches_slide_order() {
        let d = Deck::sample();
        assert_eq!(d.manifest.slides.len(), d.slide_order.len());
        assert_eq!(d.manifest.slides[0].id, d.slide_order[0]);
        assert!(d.manifest.slides[0].path.ends_with(".html"));
        assert!(d.assets.is_empty());
        assert!(d.bundle_path.is_none());
    }

    #[test]
    fn new_blank_has_one_empty_slide_and_no_bundle_path() {
        let d = Deck::new_blank();
        assert_eq!(d.slides.len(), 1);
        assert_eq!(d.slide_order.len(), 1);
        assert_eq!(d.manifest.slides.len(), 1);
        let sid = &d.slide_order[0];
        assert_eq!(d.manifest.slides[0].id, *sid);
        assert!(d.slides[sid].root.children.is_empty());
        assert!(d.assets.is_empty());
        assert!(d.bundle_path.is_none());
        assert!(!d.manifest.deck_id.is_empty());
    }

    #[test]
    fn has_unsaved_changes_tracks_slide_manifest_and_layout_dirt() {
        let mut d = Deck::sample();
        assert!(!d.has_unsaved_changes());
        d.dirty_slides.insert(d.slide_order[0].clone());
        assert!(d.has_unsaved_changes());
        d.dirty_slides.clear();
        assert!(!d.has_unsaved_changes());
        d.manifest_dirty = true;
        assert!(d.has_unsaved_changes());
        d.manifest_dirty = false;
        if let Some(layout) = d.theme.layouts.values_mut().next() {
            layout.dirty = true;
            assert!(d.has_unsaved_changes());
        }
    }

    #[test]
    fn sample_root_is_a_group() {
        let d = Deck::sample();
        let s = d.active_slide().unwrap();
        assert_eq!(s.root.element_type, ElementType::Group);
    }

    #[test]
    fn sample_all_elements_consistent() {
        let d = Deck::sample();
        let s = d.active_slide().unwrap();
        assert!(s.root.is_consistent());
        for c in &s.root.children {
            assert!(c.is_consistent());
        }
    }

    #[test]
    fn empty_deck_has_no_active_slide() {
        let d = Deck::default();
        assert!(d.active_slide().is_none());
    }

    #[test]
    fn canvas_mut_resolves_slide_target() {
        let mut d = Deck::sample();
        let sid: SlideId = d.slide_order[0].clone();
        let target = CanvasTarget::Slide(sid.clone());
        let canvas = d.canvas_mut(&target).expect("slide canvas resolves");
        assert!(canvas.find_element("el_demo_title").is_some());
    }

    #[test]
    fn canvas_mut_resolves_blank_layout_target() {
        let mut d = Deck::default();
        let target = CanvasTarget::Layout("blank".into());
        let canvas = d.canvas_mut(&target).expect("layout canvas resolves");

        assert!(canvas.find_element("el_layout_root").is_some());
    }

    #[test]
    fn canvas_returns_none_for_unknown_target() {
        let d = Deck::sample();
        assert!(d.canvas(&CanvasTarget::Slide("nope".into())).is_none());
        assert!(d.canvas(&CanvasTarget::Layout("nope".into())).is_none());
    }
}
