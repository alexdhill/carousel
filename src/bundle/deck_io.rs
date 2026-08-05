#![allow(dead_code)]

use crate::bundle::{
    AssetRegistry, BundleError, BundleReader, BundleResult, BundleWriter, ManifestData,
    manifest::{slide_path_for, validate_format_version},
};
use crate::deck::{Deck, LayoutNode, SlideId, SlideNode, ThemeData};
use crate::html::parse::parse_slide_fragment;
use crate::html::serialize::serialize_slide;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const PATH_MANIFEST: &str = "manifest.json";
pub const PATH_THEME_CSS: &str = "theme/theme.css";
pub const PATH_THEME_JSON: &str = "theme/theme.json";
pub const PATH_GLOBALS_CSS: &str = "theme/globals.css";
pub const PATH_ASSETS_INDEX: &str = "assets/index.json";

fn layout_path_for(id: &str) -> String {
    format!("theme/layouts/{id}.html")
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct LayoutMeta {
    pub(crate) id: String,
    pub(crate) name: String,

    #[serde(default)]
    pub(crate) background: Option<String>,
    #[serde(default)]
    pub(crate) background_image: Option<String>,

    #[serde(default)]
    pub(crate) guides: Vec<crate::deck::guide::Guide>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct ThemeJson {
    theme_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    layouts: Vec<LayoutMeta>,
}

#[derive(Debug, Default)]
pub struct SerializedDeck {
    pub manifest_json: String,
    pub theme_json: String,
    pub theme_css: String,

    pub globals_css: String,
    pub slide_files: BTreeMap<String, String>,

    pub layout_files: BTreeMap<String, String>,
    pub asset_files: BTreeMap<String, Vec<u8>>,
    pub assets_index_json: String,
}

pub fn serialize_deck(deck: &Deck) -> BundleResult<SerializedDeck> {
    assert!(
        deck.slide_order.len() == deck.slides.len(),
        "serialize_deck: slide_order/slides length mismatch"
    );

    let mut layout_metas: Vec<LayoutMeta> = Vec::with_capacity(deck.theme.layout_order.len());
    let mut layout_files: BTreeMap<String, String> = BTreeMap::new();
    for lid in &deck.theme.layout_order {
        let layout: &LayoutNode = deck
            .theme
            .layouts
            .get(lid)
            .ok_or_else(|| BundleError::MalformedManifest(format!("layout {lid} missing")))?;
        layout_metas.push(LayoutMeta {
            id: lid.clone(),
            name: layout.name.clone(),
            background: layout.background.clone(),
            background_image: layout.background_image.clone(),
            guides: layout.guides.clone(),
        });

        layout_files.insert(
            layout_path_for(lid),
            serialize_slide(&layout.preview_slide()),
        );
    }

    let mut manifest = deck.manifest.clone();
    for entry in &mut manifest.slides {
        if let Some(slide) = deck.slides.get(&entry.id) {
            entry.animations = slide.animations.clone();

            entry.guides = slide.guides.clone();

            entry.background = slide.metadata.background.clone();
            entry.background_image = slide.metadata.background_image.clone();

            entry.transition = slide.metadata.transition.clone();
        }
    }
    let manifest_json: String = serde_json::to_string_pretty(&manifest)?;
    let theme_json: String = serde_json::to_string_pretty(&ThemeJson {
        theme_id: deck.theme.theme_id.clone(),
        name: deck.theme.theme_id.clone(),
        layouts: layout_metas,
    })?;
    let theme_css: String = deck.theme.theme_css.clone();
    let globals_css: String = deck.theme.globals_css.clone();

    let mut slide_files: BTreeMap<String, String> = BTreeMap::new();
    let mut i: usize = 0;
    let n: usize = deck.slide_order.len();
    while i < n {
        let sid: &SlideId = &deck.slide_order[i];
        let slide: &SlideNode = deck
            .slides
            .get(sid)
            .ok_or_else(|| BundleError::MalformedManifest(format!("slide {sid} missing")))?;
        let path: String = slide_path_for(sid);
        let html: String = serialize_slide(slide);
        slide_files.insert(path, html);
        i += 1;
    }

    let assets_index_json: String = deck.assets.index_json()?;
    let asset_files: BTreeMap<String, Vec<u8>> = deck
        .assets
        .files
        .iter()
        .map(|(p, b)| (p.clone(), b.clone()))
        .collect();

    Ok(SerializedDeck {
        manifest_json,
        theme_json,
        theme_css,
        globals_css,
        slide_files,
        layout_files,
        asset_files,
        assets_index_json,
    })
}

pub fn write_serialized(writer: &mut BundleWriter, src: &SerializedDeck) -> BundleResult<()> {
    writer.write_string(PATH_MANIFEST, &src.manifest_json)?;
    writer.write_string(PATH_THEME_JSON, &src.theme_json)?;
    writer.write_string(PATH_THEME_CSS, &src.theme_css)?;
    writer.write_string(PATH_GLOBALS_CSS, &src.globals_css)?;
    for (path, html) in &src.layout_files {
        writer.write_string(path, html)?;
    }
    for (path, html) in &src.slide_files {
        writer.write_string(path, html)?;
    }
    for (path, bytes) in &src.asset_files {
        writer.write_bytes(path, bytes)?;
    }
    writer.write_string(PATH_ASSETS_INDEX, &src.assets_index_json)?;
    Ok(())
}

pub fn read_serialized(reader: &mut BundleReader) -> BundleResult<SerializedDeck> {
    let manifest_json: String = reader.read_string(PATH_MANIFEST)?;
    let manifest: ManifestData = serde_json::from_str(&manifest_json)?;
    validate_format_version(&manifest.format_version)?;

    let theme_json: String = if reader.has_entry(PATH_THEME_JSON)? {
        reader.read_string(PATH_THEME_JSON)?
    } else {
        serde_json::to_string(&ThemeJson {
            theme_id: "default".into(),
            name: "default".into(),
            layouts: Vec::new(),
        })?
    };
    let theme_css: String = if reader.has_entry(PATH_THEME_CSS)? {
        reader.read_string(PATH_THEME_CSS)?
    } else {
        String::new()
    };
    let globals_css: String = if reader.has_entry(PATH_GLOBALS_CSS)? {
        reader.read_string(PATH_GLOBALS_CSS)?
    } else {
        String::new()
    };

    let theme_meta: ThemeJson = serde_json::from_str(&theme_json)?;
    let mut layout_files: BTreeMap<String, String> = BTreeMap::new();
    for meta in &theme_meta.layouts {
        let path: String = layout_path_for(&meta.id);
        if reader.has_entry(&path)? {
            let html: String = reader.read_string(&path)?;
            layout_files.insert(path, html);
        }
    }

    let mut slide_files: BTreeMap<String, String> = BTreeMap::new();
    let mut i: usize = 0;
    let n: usize = manifest.slides.len();
    while i < n {
        let entry = &manifest.slides[i];
        let html: String = reader.read_string(&entry.path)?;
        slide_files.insert(entry.path.clone(), html);
        i += 1;
    }

    let assets_index_json: String = if reader.has_entry(PATH_ASSETS_INDEX)? {
        reader.read_string(PATH_ASSETS_INDEX)?
    } else {
        AssetRegistry::new_empty().index_json()?
    };

    let parsed_registry: AssetRegistry = AssetRegistry::from_index_json(&assets_index_json)?;
    let mut asset_files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut a: usize = 0;
    let max_assets: usize = parsed_registry.assets.len();
    while a < max_assets {
        let entry = &parsed_registry.assets[a];
        if reader.has_entry(&entry.path)? {
            let bytes: Vec<u8> = reader.read_bytes(&entry.path)?;
            asset_files.insert(entry.path.clone(), bytes);
        }
        a += 1;
    }

    Ok(SerializedDeck {
        manifest_json,
        theme_json,
        theme_css,
        globals_css,
        slide_files,
        layout_files,
        asset_files,
        assets_index_json,
    })
}

pub fn deserialize_deck(serialized: SerializedDeck) -> BundleResult<Deck> {
    let manifest: ManifestData = serde_json::from_str(&serialized.manifest_json)?;
    validate_format_version(&manifest.format_version)?;
    let theme_meta: ThemeJson = serde_json::from_str(&serialized.theme_json)?;

    let (layouts, layout_order): (BTreeMap<_, _>, Vec<_>) = if theme_meta.layouts.is_empty() {
        let seed: ThemeData = ThemeData::default();
        (seed.layouts, seed.layout_order)
    } else {
        let mut layouts: BTreeMap<crate::deck::LayoutId, LayoutNode> = BTreeMap::new();
        let mut layout_order: Vec<crate::deck::LayoutId> =
            Vec::with_capacity(theme_meta.layouts.len());
        for meta in &theme_meta.layouts {
            let path: String = layout_path_for(&meta.id);
            let html: &String = serialized.layout_files.get(&path).ok_or_else(|| {
                BundleError::MissingEntry(format!("layout html missing for {}", meta.id))
            })?;
            let parsed: SlideNode = parse_slide_fragment(html)
                .map_err(|e| BundleError::SlideParse(format!("layout {}: {}", meta.id, e)))?;
            let mut node: LayoutNode =
                LayoutNode::new(meta.id.clone(), meta.name.clone(), parsed.root);
            node.background = meta.background.clone();
            node.background_image = meta.background_image.clone();
            node.guides = meta.guides.clone();
            layouts.insert(meta.id.clone(), node);
            layout_order.push(meta.id.clone());
        }
        (layouts, layout_order)
    };
    let theme: ThemeData = ThemeData {
        theme_id: theme_meta.theme_id,
        theme_css: serialized.theme_css,
        globals_css: serialized.globals_css,
        layouts,
        layout_order,
    };

    let mut slides: BTreeMap<SlideId, SlideNode> = BTreeMap::new();
    let mut slide_order: Vec<SlideId> = Vec::with_capacity(manifest.slides.len());
    let mut i: usize = 0;
    let n: usize = manifest.slides.len();
    while i < n {
        let entry = &manifest.slides[i];
        let html: &String = serialized.slide_files.get(&entry.path).ok_or_else(|| {
            BundleError::MissingEntry(format!(
                "slide html missing for manifest entry {}",
                entry.id
            ))
        })?;
        let mut slide: SlideNode = parse_slide_fragment(html)
            .map_err(|e| BundleError::SlideParse(format!("{}: {}", entry.id, e)))?;

        slide.animations = entry.animations.clone();

        slide.guides = entry.guides.clone();

        slide.metadata.background = entry.background.clone();
        slide.metadata.background_image = entry.background_image.clone();
        slide.metadata.transition = entry.transition.clone();

        if slide.id != entry.id {
            return Err(BundleError::MalformedManifest(format!(
                "slide id mismatch: manifest={}, html={}",
                entry.id, slide.id
            )));
        }
        slides.insert(entry.id.clone(), slide);
        slide_order.push(entry.id.clone());
        i += 1;
    }

    let mut assets: AssetRegistry = AssetRegistry::from_index_json(&serialized.assets_index_json)?;
    let mut files: HashMap<String, Vec<u8>> = HashMap::with_capacity(serialized.asset_files.len());
    for (p, b) in serialized.asset_files {
        files.insert(p, b);
    }
    assets.files = files;

    Ok(Deck {
        manifest,
        theme,
        slides,
        slide_order,
        assets,
        dirty_slides: HashSet::new(),
        manifest_dirty: false,
        bundle_path: None,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::bundle::{BundleReader, BundleWriter};
    use tempfile::TempDir;

    #[test]
    fn bundle_preserves_raw_token_text() {
        use crate::deck::builders::{group_element, text_element};
        let mut deck = Deck::sample();
        let sid = deck.slide_order[0].clone();
        deck.slides.get_mut(&sid).unwrap().root =
            group_element("root", vec![text_element("tk", "Slide ${slideNumber}")]);
        let serialized = serialize_deck(&deck).unwrap();
        let back = deserialize_deck(serialized).unwrap();
        let child = &back.slides[&sid].root.children[0];
        match &child.content {
            crate::deck::ElementContent::Text(rt) => assert_eq!(rt.plain, "Slide ${slideNumber}"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn bundle_preserves_placeholder_flag() {
        use crate::deck::builders::{group_element, text_element};
        let mut deck = Deck::sample();
        let sid = deck.slide_order[0].clone();
        let mut ph = text_element("layout_text_title", "Title");
        ph.placeholder = true;
        let edited = text_element("layout_text_body", "real");
        deck.slides.get_mut(&sid).unwrap().root = group_element("root", vec![ph, edited]);
        let back = deserialize_deck(serialize_deck(&deck).unwrap()).unwrap();
        let kids = &back.slides[&sid].root.children;
        assert!(
            kids.iter()
                .find(|c| c.id == "layout_text_title")
                .unwrap()
                .placeholder
        );
        assert!(
            !kids
                .iter()
                .find(|c| c.id == "layout_text_body")
                .unwrap()
                .placeholder
        );
    }

    #[test]
    fn serialize_sample_deck_has_manifest_theme_slides_assets() {
        let deck = Deck::sample();
        let s = serialize_deck(&deck).unwrap();
        assert!(s.manifest_json.contains("format_version"));
        assert!(!s.theme_css.is_empty());
        assert_eq!(s.slide_files.len(), 1);
        let (path, html) = s.slide_files.iter().next().unwrap();
        assert!(path.starts_with("slides/slide_"));
        assert!(path.ends_with(".html"));
        assert!(html.contains("data-slide-id"));
        assert!(s.assets_index_json.contains("assets"));
    }

    #[test]
    fn deserialize_after_serialize_round_trips() {
        let original = Deck::sample();
        let s = serialize_deck(&original).unwrap();
        let back = deserialize_deck(s).unwrap();
        assert_eq!(back.slide_order, original.slide_order);
        assert_eq!(back.slides.len(), original.slides.len());
        assert_eq!(
            back.manifest.format_version,
            original.manifest.format_version
        );

        assert!(back.bundle_path.is_none());
    }

    #[test]
    fn serialize_then_write_then_read_then_deserialize_full_loop() {
        let original = Deck::sample();
        let s_out = serialize_deck(&original).unwrap();

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("loop.slidedeck");
        let mut w = BundleWriter::create(&path).unwrap();
        write_serialized(&mut w, &s_out).unwrap();
        w.finish().unwrap();

        let mut r = BundleReader::open(&path).unwrap();
        let s_in = read_serialized(&mut r).unwrap();
        let back = deserialize_deck(s_in).unwrap();

        assert_eq!(back.slide_order, original.slide_order);
        let orig_first = original.slide_order[0].clone();
        let back_first = back.slide_order[0].clone();
        assert_eq!(back_first, orig_first);
        assert_eq!(
            back.slides[&back_first].root.children.len(),
            original.slides[&orig_first].root.children.len()
        );
    }

    #[test]
    fn slide_html_is_round_trip_stable() {
        let original = Deck::sample();
        let s1 = serialize_deck(&original).unwrap();
        let back = deserialize_deck(serialize_deck(&original).unwrap()).unwrap();
        let s2 = serialize_deck(&back).unwrap();
        assert_eq!(s1.slide_files, s2.slide_files);
    }

    #[test]
    fn deserialize_rejects_future_major_version() {
        let mut deck = Deck::sample();
        deck.manifest.format_version = "2.0".into();
        let s = serialize_deck(&deck).unwrap();
        let err = deserialize_deck(s).unwrap_err();
        assert!(matches!(err, BundleError::IncompatibleVersion(_, _)));
    }

    #[test]
    fn deserialize_errors_on_manifest_slide_missing_from_files() {
        let original = Deck::sample();
        let mut s = serialize_deck(&original).unwrap();

        s.slide_files.clear();
        let err = deserialize_deck(s).unwrap_err();
        assert!(matches!(err, BundleError::MissingEntry(_)));
    }

    #[test]
    fn write_serialized_emits_all_canonical_paths() {
        let deck = Deck::sample();
        let s = serialize_deck(&deck).unwrap();
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("p.slidedeck");
        let mut w = BundleWriter::create(&path).unwrap();
        write_serialized(&mut w, &s).unwrap();
        w.finish().unwrap();

        let mut r = BundleReader::open(&path).unwrap();
        let names = r.entry_names().unwrap();
        assert!(names.iter().any(|n| n == PATH_MANIFEST));
        assert!(names.iter().any(|n| n == PATH_THEME_JSON));
        assert!(names.iter().any(|n| n == PATH_THEME_CSS));
        assert!(names.iter().any(|n| n == PATH_ASSETS_INDEX));
        assert!(names.iter().any(|n| n.starts_with("slides/slide_")));
    }

    #[test]
    fn mutation_then_save_then_load_persists_geometry() {
        use crate::commands::{CommandDispatcher, MoveElement};
        use crate::ipc::Point;

        let original_deck = Deck::sample();
        let sid = original_deck.slide_order[0].clone();
        let eid = original_deck.slides[&sid].root.children[0].id.clone();
        let mut d = CommandDispatcher::new(original_deck);
        d.dispatch(Box::new(MoveElement {
            target: crate::deck::CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            new_position: Point { x: 777.0, y: 333.0 },
            previous_position: None,
        }))
        .unwrap();

        let serialized_out = serialize_deck(d.deck()).unwrap();
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("after_edit.slidedeck");
        let mut writer = BundleWriter::create(&path).unwrap();
        write_serialized(&mut writer, &serialized_out).unwrap();
        writer.finish().unwrap();

        let mut reader = BundleReader::open(&path).unwrap();
        let serialized_in = read_serialized(&mut reader).unwrap();
        let loaded = deserialize_deck(serialized_in).unwrap();
        let geo = loaded.slides[&sid]
            .find_element(&eid)
            .unwrap()
            .geometry
            .clone();
        assert_eq!(geo.x, 777.0);
        assert_eq!(geo.y, 333.0);
        assert_eq!(loaded.slide_order, vec![sid]);
    }

    #[test]
    fn second_save_overwrites_first_save() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("overwrite.slidedeck");

        let deck1 = Deck::sample();
        let s1 = serialize_deck(&deck1).unwrap();
        let mut w1 = BundleWriter::create(&path).unwrap();
        write_serialized(&mut w1, &s1).unwrap();
        w1.finish().unwrap();

        let deck2 = Deck::new_blank();
        let s2 = serialize_deck(&deck2).unwrap();
        let mut w2 = BundleWriter::create(&path).unwrap();
        write_serialized(&mut w2, &s2).unwrap();
        w2.finish().unwrap();

        let mut r = BundleReader::open(&path).unwrap();
        let s_back = read_serialized(&mut r).unwrap();
        let loaded = deserialize_deck(s_back).unwrap();
        assert_eq!(loaded.slide_order.len(), 1);
        let sid = &loaded.slide_order[0];
        assert!(loaded.slides[sid].root.children.is_empty());
    }

    #[test]
    fn read_serialized_works_when_optional_theme_files_absent() {

        let deck = Deck::sample();
        let s = serialize_deck(&deck).unwrap();
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("minimal.slidedeck");
        let mut w = BundleWriter::create(&path).unwrap();
        w.write_string(PATH_MANIFEST, &s.manifest_json).unwrap();
        for (p, h) in &s.slide_files {
            w.write_string(p, h).unwrap();
        }
        w.finish().unwrap();

        let mut r = BundleReader::open(&path).unwrap();
        let s_back = read_serialized(&mut r).unwrap();
        assert_eq!(s_back.slide_files.len(), 1);
        assert!(s_back.theme_css.is_empty());
    }

    #[test]
    fn round_trip_preserves_layouts_and_globals() {
        use crate::deck::builders::{group_element, text_element};
        let mut original = Deck::sample();
        original.theme.globals_css = "@keyframes spin { to { rotate: 360deg; } }".into();

        let title_root = group_element("el_layout_root", vec![text_element("el_t", "Title")]);
        original.theme.layouts.insert(
            "title".into(),
            LayoutNode::new("title".into(), "Title".into(), title_root),
        );
        original.theme.layout_order.push("title".into());

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("layouts.slidedeck");
        let mut w = BundleWriter::create(&path).unwrap();
        write_serialized(&mut w, &serialize_deck(&original).unwrap()).unwrap();
        w.finish().unwrap();

        let mut r = BundleReader::open(&path).unwrap();
        let back = deserialize_deck(read_serialized(&mut r).unwrap()).unwrap();

        assert_eq!(back.theme.globals_css, original.theme.globals_css);
        assert_eq!(
            back.theme.layout_order,
            vec!["blank".to_string(), "title".to_string()]
        );
        assert_eq!(back.theme.layouts.len(), 2);
        assert_eq!(back.theme.layouts["title"].name, "Title");

        let title = &back.theme.layouts["title"];
        assert_eq!(title.root.children.len(), 1);
        assert_eq!(title.root.children[0].id, "el_t");
        assert_eq!(back.theme.layouts["blank"].name, "Blank");
    }

    #[test]
    fn round_trip_preserves_slide_background_and_notes() {
        let mut original = Deck::sample();
        let sid = original.slide_order[0].clone();
        original.slides.get_mut(&sid).unwrap().metadata.background = Some("#101820".into());
        original
            .manifest
            .slides
            .iter_mut()
            .find(|e| e.id == sid)
            .unwrap()
            .notes = Some("speak slowly".into());

        let back = deserialize_deck(serialize_deck(&original).unwrap()).unwrap();

        assert_eq!(
            back.slides[&sid].metadata.background,
            Some("#101820".to_string())
        );

        let entry = back.manifest.slides.iter().find(|e| e.id == sid).unwrap();
        assert_eq!(entry.notes, Some("speak slowly".to_string()));
    }

    #[test]
    fn round_trip_preserves_slide_animations() {
        use crate::deck::animation::{
            AnimationCategory, AnimationEntry, AnimationTiming, AnimationTrigger,
        };
        let mut original = Deck::sample();
        let sid = original.slide_order[0].clone();
        let el = original.slides[&sid].root.children[0].id.clone();
        original
            .slides
            .get_mut(&sid)
            .unwrap()
            .animations
            .push(AnimationEntry::new(
                "anim_1".into(),
                el.clone(),
                crate::deck::animation::AnimationEffect::Named("appear".into()),
                AnimationCategory::Entrance,
                AnimationTrigger::OnClick,
                AnimationTiming {
                    duration_ms: 700,
                    ..AnimationTiming::default()
                },
            ));

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("anim.slidedeck");
        let mut w = BundleWriter::create(&path).unwrap();
        write_serialized(&mut w, &serialize_deck(&original).unwrap()).unwrap();
        w.finish().unwrap();

        let mut r = BundleReader::open(&path).unwrap();
        let back = deserialize_deck(read_serialized(&mut r).unwrap()).unwrap();

        let t = &back.slides[&sid].animations;
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].id, "anim_1");
        assert_eq!(t[0].element_id, el);
        assert_eq!(t[0].category, AnimationCategory::Entrance);
        assert_eq!(t[0].timing.duration_ms, 700);
    }

    #[test]
    fn round_trip_preserves_slide_and_layout_guides() {
        use crate::deck::guide::{Guide, GuideAxis};
        let mut original = Deck::sample();
        let sid = original.slide_order[0].clone();
        original
            .slides
            .get_mut(&sid)
            .unwrap()
            .guides
            .push(Guide::new(GuideAxis::Vertical, 123.5));
        let lid = original.theme.layout_order[0].clone();
        original
            .theme
            .layouts
            .get_mut(&lid)
            .unwrap()
            .guides
            .push(Guide::new(GuideAxis::Horizontal, 40.0));

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("guides.slidedeck");
        let mut w = BundleWriter::create(&path).unwrap();
        write_serialized(&mut w, &serialize_deck(&original).unwrap()).unwrap();
        w.finish().unwrap();

        let mut r = BundleReader::open(&path).unwrap();
        let back = deserialize_deck(read_serialized(&mut r).unwrap()).unwrap();

        let sg = &back.slides[&sid].guides;
        assert_eq!(sg.len(), 1);
        assert_eq!(sg[0].axis, GuideAxis::Vertical);
        assert_eq!(sg[0].pos, 123.5);
        let lg = &back.theme.layouts[&lid].guides;
        assert_eq!(lg.len(), 1);
        assert_eq!(lg[0].axis, GuideAxis::Horizontal);
        assert_eq!(lg[0].pos, 40.0);
    }

    #[test]
    fn older_bundle_without_guides_loads_empty() {

        let deck = Deck::sample();
        let mut s = serialize_deck(&deck).unwrap();
        let mut manifest: serde_json::Value = serde_json::from_str(&s.manifest_json).unwrap();
        for slide in manifest["slides"].as_array_mut().unwrap() {
            slide.as_object_mut().unwrap().remove("guides");
        }
        s.manifest_json = serde_json::to_string(&manifest).unwrap();
        let back = deserialize_deck(s).unwrap();
        let sid = deck.slide_order[0].clone();
        assert!(back.slides[&sid].guides.is_empty());
    }

    #[test]
    fn older_bundle_without_animations_loads_empty_timeline() {

        let deck = Deck::sample();
        let mut s = serialize_deck(&deck).unwrap();

        let mut manifest: serde_json::Value = serde_json::from_str(&s.manifest_json).unwrap();
        for slide in manifest["slides"].as_array_mut().unwrap() {
            slide.as_object_mut().unwrap().remove("animations");
        }
        assert!(manifest["slides"][0].get("animations").is_none());
        s.manifest_json = serde_json::to_string(&manifest).unwrap();
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("legacy_anim.slidedeck");
        let mut w = BundleWriter::create(&path).unwrap();
        write_serialized(&mut w, &s).unwrap();
        w.finish().unwrap();

        let mut r = BundleReader::open(&path).unwrap();
        let back = deserialize_deck(read_serialized(&mut r).unwrap()).unwrap();
        let sid = back.slide_order[0].clone();
        assert!(back.slides[&sid].animations.is_empty());
    }

    #[test]
    fn older_bundle_without_layouts_loads_the_seed() {

        let deck = Deck::sample();
        let s = serialize_deck(&deck).unwrap();
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("legacy.slidedeck");
        let mut w = BundleWriter::create(&path).unwrap();
        w.write_string(PATH_MANIFEST, &s.manifest_json).unwrap();
        w.write_string(PATH_THEME_JSON, r#"{"theme_id":"legacy","name":"legacy"}"#)
            .unwrap();
        w.write_string(PATH_THEME_CSS, &s.theme_css).unwrap();
        for (p, h) in &s.slide_files {
            w.write_string(p, h).unwrap();
        }
        w.finish().unwrap();

        let mut r = BundleReader::open(&path).unwrap();
        let back = deserialize_deck(read_serialized(&mut r).unwrap()).unwrap();
        assert_eq!(back.theme.layout_order, vec!["blank".to_string()]);
        assert!(back.theme.layouts.contains_key("blank"));
        assert!(back.theme.globals_css.is_empty());
    }
}
