// Deck — the in-memory tree.

#![allow(dead_code, unused_imports)]

//
// Owns the theme, the slide map, the canonical slide order, and the
// dirty-tracking sets. Stage 3 builds a `sample()` deck in code; Stage 7
// will replace that with bundle I/O.

pub mod anim_catalog;
pub mod animation;
pub mod builders;
pub mod canvas;
pub mod element;
pub mod group_layout;
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

pub use element::{
    AssetRef, ElementContent, ElementNode, ElementStyle, ElementType, RichText, ShapeGeometry,
    TableCell, TableData,
};
pub use animation::{
    AnimationCategory, AnimationEntry, AnimationIterations, AnimationState, AnimationTiming,
    AnimationTrigger,
};
pub use canvas::{Canvas, InsertError, RemovedElement};
pub use ids::{
    AnimationId, AssetId, ElementId, LayoutId, SlideId, new_animation_id, new_element_id,
    new_slide_id,
};
pub use layout::LayoutNode;
pub use slide::{SlideMetadata, SlideNode};
pub use style::{
    Border, BorderStyle, ColorRef, FillRef, Filter, FontRef, FontStyle, Geometry, ImageStyle,
    Length, LengthUnit, MediaStyle, ObjectFit, Shadow, ShapeStyle, Stroke, TableStyle, TextAlign,
    TextStyle,
};
pub use theme::ThemeData;

// CanvasTarget
// Identifies which editable surface an element command operates on: a slide
// (in `deck.slides`) or a layout template (in `deck.theme.layouts`).
// Resolved to a `&dyn Canvas` by `Deck::canvas[_mut]`. Element commands carry
// one of these in place of the old `slide_id` field.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CanvasTarget {
    Slide(SlideId),
    Layout(LayoutId),
}

impl CanvasTarget {
    // id
    // Inputs: self.
    // Output: the inner slide / layout id as a &str. Lets callers assert a
    // non-empty target id without matching the variant.
    pub fn id(&self) -> &str {
        match self {
            CanvasTarget::Slide(id) => id,
            CanvasTarget::Layout(id) => id,
        }
    }
}

// Deck
// Top-level deck state. `slides` is a BTreeMap for deterministic
// serialization order; display order lives in `slide_order` because ULIDs
// sort by creation time, which rarely matches user intent.
//
// Stage 7 adds three persistence fields:
//   - manifest:    the parsed manifest.json (deck id, metadata, dims, theme ref,
//                  slide entries). Mirrors slide_order for serialization.
//   - assets:      the asset registry — minimal Stage 7 implementation.
//   - bundle_path: where this deck lives on disk. `None` means "never saved
//                  yet"; Save falls through to Save-As when None.
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
    // effective_slide_bg
    // Inputs: a slide.
    // Output: (fill, image) background values the slide should render with —
    // the slide's own metadata, falling back to its layout's background for
    // any field the slide leaves empty (layout→slide theme inheritance).
    // Errors: none; a missing/empty layout simply yields no fallback.
    pub fn effective_slide_bg(&self, slide: &SlideNode) -> (Option<String>, Option<String>) {
        let layout: Option<&crate::deck::layout::LayoutNode> =
            self.theme.layouts.get(&slide.layout_id);
        let pick = |own: &Option<String>, lay: Option<&String>| -> Option<String> {
            match own {
                Some(s) if !s.is_empty() => Some(s.clone()),
                _ => lay.filter(|s| !s.is_empty()).cloned(),
            }
        };
        let fill = pick(&slide.metadata.background, layout.and_then(|l| l.background.as_ref()));
        let img = pick(
            &slide.metadata.background_image,
            layout.and_then(|l| l.background_image.as_ref()),
        );
        (fill, img)
    }

    // new_blank
    // Inputs: none.
    // Output: a fresh deck with one empty slide (the ROADMAP definition of
    // "File → New creates an empty deck (one blank slide)"). The manifest
    // is regenerated with a fresh deck id and one matching slide entry.
    // Dataflow: build an empty Group as the slide root, wrap it in a
    // SlideNode under a freshly-minted slide id, then build the
    // single-entry manifest pointing at it.
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

    // sample
    // Inputs: none.
    // Output: a Deck containing a single slide with three demonstration
    // elements (title, subtitle, body paragraph), plus a manifest entry
    // pointing at the slide's canonical bundle path so save/load round
    // trips the sample deck verbatim.
    // Dataflow: build the elements via builders, wrap them in a Group
    // root, wrap that in a SlideNode, register it under a stable id;
    // synthesise the manifest's slides[] from slide_order.
    pub fn sample() -> Self {
        use builders::{group_element, text_element_styled};

        let title_id: ElementId = "el_demo_title".into();
        let subtitle_id: ElementId = "el_demo_subtitle".into();
        let body_id: ElementId = "el_demo_body".into();

        let title = text_element_styled(
            title_id,
            "Hello from the in-memory tree.",
            Geometry { x: 120.0, y: 200.0, width: 1680.0, height: 120.0, ..Default::default() },
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
            Geometry { x: 120.0, y: 340.0, width: 1680.0, height: 60.0, ..Default::default() },
            TextStyle {
                font_size: Length::px(36.0),
                color: ColorRef::Theme("muted".into()),
                ..TextStyle::default()
            },
        );

        let body = text_element_styled(
            body_id,
            "Edit Deck::sample in src/deck/mod.rs and recompile.",
            Geometry { x: 120.0, y: 460.0, width: 1680.0, height: 60.0, ..Default::default() },
            TextStyle {
                font_size: Length::px(28.0),
                color: ColorRef::Literal("#444".into()),
                ..TextStyle::default()
            },
        );

        let root: ElementNode = group_element(
            "el_slide_root",
            vec![title, subtitle, body],
        );
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

    // active_slide
    // Inputs: self.
    // Output: a reference to the first slide in canonical order, if any.
    pub fn active_slide(&self) -> Option<&SlideNode> {
        let first: &SlideId = self.slide_order.first()?;
        self.slides.get(first)
    }

    // canvas
    // Inputs: a CanvasTarget.
    // Output: an immutable `&dyn Canvas` for that surface, or None if the
    // referenced slide / layout does not exist.
    // Dataflow: resolves Slide(id) into `self.slides` and Layout(id) into
    // `self.theme.layouts`, erasing the concrete type to the shared trait.
    pub fn canvas(&self, target: &CanvasTarget) -> Option<&dyn Canvas> {
        match target {
            CanvasTarget::Slide(id) => self.slides.get(id).map(|s| s as &dyn Canvas),
            CanvasTarget::Layout(id) => {
                self.theme.layouts.get(id).map(|l| l as &dyn Canvas)
            }
        }
    }

    // canvas_mut
    // Inputs: a CanvasTarget.
    // Output: a mutable `&mut dyn Canvas` for that surface, or None if the
    // referenced slide / layout does not exist.
    // Dataflow: mirror of `canvas`, used by element commands to mutate the
    // active editable surface regardless of whether it is a slide or layout.
    pub fn canvas_mut(&mut self, target: &CanvasTarget) -> Option<&mut dyn Canvas> {
        match target {
            CanvasTarget::Slide(id) => {
                self.slides.get_mut(id).map(|s| s as &mut dyn Canvas)
            }
            CanvasTarget::Layout(id) => {
                self.theme.layouts.get_mut(id).map(|l| l as &mut dyn Canvas)
            }
        }
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
        // The default theme (carried by Deck::default) seeds a "blank" layout.
        let mut d = Deck::default();
        let target = CanvasTarget::Layout("blank".into());
        let canvas = d.canvas_mut(&target).expect("layout canvas resolves");
        // The layout root is "el_layout_root"; find it via the shared trait.
        assert!(canvas.find_element("el_layout_root").is_some());
    }

    #[test]
    fn canvas_returns_none_for_unknown_target() {
        let d = Deck::sample();
        assert!(d.canvas(&CanvasTarget::Slide("nope".into())).is_none());
        assert!(d.canvas(&CanvasTarget::Layout("nope".into())).is_none());
    }
}
