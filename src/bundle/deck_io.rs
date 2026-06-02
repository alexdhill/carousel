// SerializedDeck — bridge between the in-memory Deck and the bundle's
// on-disk file contents.
//
// `serialize_deck` runs on the main thread (it reads the deck), produces a
// `SerializedDeck` holding every file's bytes, and hands that off to the
// I/O thread for the actual ZIP write. `deserialize_deck` runs on the main
// thread after the I/O thread has populated a `SerializedDeck` from a
// bundle on disk; it parses every entry back into a Deck. Splitting it
// this way means the main thread does the borrow-y, ref-heavy work and
// the worker thread only handles owned String/Vec<u8> + ZipWriter.

#![allow(dead_code)]

use crate::bundle::{
    AssetRegistry, BundleError, BundleResult, BundleReader, BundleWriter, ManifestData,
    manifest::{slide_path_for, validate_format_version},
};
use crate::deck::{Deck, SlideId, SlideNode, ThemeData};
use crate::html::parse::parse_slide_fragment;
use crate::html::serialize::serialize_slide;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const PATH_MANIFEST: &str = "manifest.json";
pub const PATH_THEME_CSS: &str = "theme/theme.css";
pub const PATH_THEME_JSON: &str = "theme/theme.json";
pub const PATH_ASSETS_INDEX: &str = "assets/index.json";

// ThemeJson
// On-disk schema for theme/theme.json. Stage 7 only needs the theme_id
// (and a placeholder name) so loaded decks rehydrate ThemeData fully. The
// fuller theme schema per SPEC §6.2 will land alongside theme-editor work.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct ThemeJson {
    theme_id: String,
    #[serde(default)]
    name: String,
}

// SerializedDeck
// A complete bundle's worth of file contents, all owned and `Send`.
// Strings hold UTF-8 (manifest, theme, slide HTML, asset index); Vec<u8>
// holds binary blobs (assets, thumbnails — none in Stage 7 by default).
#[derive(Debug, Default)]
pub struct SerializedDeck {
    pub manifest_json: String,
    pub theme_json: String,
    pub theme_css: String,
    pub slide_files: BTreeMap<String, String>,
    pub asset_files: BTreeMap<String, Vec<u8>>,
    pub assets_index_json: String,
}

// serialize_deck
// Inputs: a reference to the in-memory Deck.
// Output: a SerializedDeck containing every file the bundle should hold.
// Errors: serde failures on JSON encode (BundleError::Json).
// Dataflow:
//   1. Manifest JSON: write the deck's manifest verbatim (the editor keeps
//      it in sync with slide_order/dirty state).
//   2. Theme: emit theme.json (id + name) and theme.css as-is.
//   3. Slides: for each entry in slide_order, serialize the corresponding
//      SlideNode to HTML and key it by the canonical bundle path.
//   4. Assets: emit assets/index.json and any binary blobs the registry
//      tracks. The Stage 7 registry holds zero or many files depending on
//      what's been imported.
pub fn serialize_deck(deck: &Deck) -> BundleResult<SerializedDeck> {
    assert!(
        deck.slide_order.len() == deck.slides.len(),
        "serialize_deck: slide_order/slides length mismatch"
    );
    let manifest_json: String = serde_json::to_string_pretty(&deck.manifest)?;
    let theme_json: String = serde_json::to_string_pretty(&ThemeJson {
        theme_id: deck.theme.theme_id.clone(),
        name: deck.theme.theme_id.clone(),
    })?;
    let theme_css: String = deck.theme.theme_css.clone();

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
        slide_files,
        asset_files,
        assets_index_json,
    })
}

// write_serialized
// Inputs: a target BundleWriter, a SerializedDeck.
// Output: side-effect; streams every entry into the writer. The caller
// is responsible for calling `writer.finish()` to commit atomically.
// Errors: any Zip/Io failure from the writer.
// Dataflow: manifest → theme.json → theme.css → every slide path in sorted
// order (BTreeMap iteration) → every asset path in sorted order →
// assets/index.json last (so partial archives never look complete).
pub fn write_serialized(writer: &mut BundleWriter, src: &SerializedDeck) -> BundleResult<()> {
    writer.write_string(PATH_MANIFEST, &src.manifest_json)?;
    writer.write_string(PATH_THEME_JSON, &src.theme_json)?;
    writer.write_string(PATH_THEME_CSS, &src.theme_css)?;
    for (path, html) in &src.slide_files {
        writer.write_string(path, html)?;
    }
    for (path, bytes) in &src.asset_files {
        writer.write_bytes(path, bytes)?;
    }
    writer.write_string(PATH_ASSETS_INDEX, &src.assets_index_json)?;
    Ok(())
}

// read_serialized
// Inputs: a BundleReader pointing at an open archive.
// Output: a SerializedDeck holding every entry the deserializer will
// need. Assets and thumbnails are NOT slurped (could be huge); we only
// load the manifest, theme, all slides referenced by the manifest, and
// the asset index.
// Errors: MissingEntry on absent required files; Zip/Io on read failures.
// Dataflow: read manifest -> read theme.json + theme.css -> for each
// manifest slide, read the slide HTML -> read assets/index.json if
// present.
pub fn read_serialized(reader: &mut BundleReader) -> BundleResult<SerializedDeck> {
    let manifest_json: String = reader.read_string(PATH_MANIFEST)?;
    let manifest: ManifestData = serde_json::from_str(&manifest_json)?;
    validate_format_version(&manifest.format_version)?;

    let theme_json: String = if reader.has_entry(PATH_THEME_JSON)? {
        reader.read_string(PATH_THEME_JSON)?
    } else {
        serde_json::to_string(&ThemeJson { theme_id: "default".into(), name: "default".into() })?
    };
    let theme_css: String = if reader.has_entry(PATH_THEME_CSS)? {
        reader.read_string(PATH_THEME_CSS)?
    } else {
        String::new()
    };

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

    // Asset blobs: only those referenced by the registry. Stage 7 keeps
    // them all in memory (matches the SerializedDeck contract); larger
    // decks would justify lazy/on-demand loading instead.
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
        slide_files,
        asset_files,
        assets_index_json,
    })
}

// deserialize_deck
// Inputs: a SerializedDeck produced by `read_serialized`.
// Output: a fully-populated Deck with bundle_path = None (the caller sets
// it after a successful load).
// Errors: MalformedManifest on JSON parse / version validation failure;
// SlideParse on per-slide HTML parse failure.
// Dataflow: parse manifest -> parse theme.json -> rehydrate ThemeData ->
// for each manifest slide, parse the HTML into a SlideNode and insert ->
// parse the asset registry index and re-attach the file bytes.
pub fn deserialize_deck(serialized: SerializedDeck) -> BundleResult<Deck> {
    let manifest: ManifestData = serde_json::from_str(&serialized.manifest_json)?;
    validate_format_version(&manifest.format_version)?;
    let theme_meta: ThemeJson = serde_json::from_str(&serialized.theme_json)?;

    let theme: ThemeData = ThemeData {
        theme_id: theme_meta.theme_id,
        theme_css: serialized.theme_css,
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
        let slide: SlideNode = parse_slide_fragment(html)
            .map_err(|e| BundleError::SlideParse(format!("{}: {}", entry.id, e)))?;
        // Manifest is authoritative for slide id; ensure parsed slide
        // matches so round trips are stable.
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
        assert_eq!(back.manifest.format_version, original.manifest.format_version);
        // bundle_path is reset on load (caller assigns it).
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
        // Drop the only slide html — manifest still references it.
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

        // 1. Build a deck and mutate an element through the dispatcher.
        let original_deck = Deck::sample();
        let sid = original_deck.slide_order[0].clone();
        let eid = original_deck.slides[&sid].root.children[0].id.clone();
        let mut d = CommandDispatcher::new(original_deck);
        d.dispatch(Box::new(MoveElement {
            slide_id: sid.clone(),
            element_id: eid.clone(),
            new_position: Point { x: 777.0, y: 333.0 },
            previous_position: None,
        }))
        .unwrap();

        // 2. Serialize, write atomically, then read back.
        let serialized_out = serialize_deck(d.deck()).unwrap();
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("after_edit.slidedeck");
        let mut writer = BundleWriter::create(&path).unwrap();
        write_serialized(&mut writer, &serialized_out).unwrap();
        writer.finish().unwrap();

        // 3. Open the bundle in a fresh reader → deserialize → confirm
        //    the mutation made it through.
        let mut reader = BundleReader::open(&path).unwrap();
        let serialized_in = read_serialized(&mut reader).unwrap();
        let loaded = deserialize_deck(serialized_in).unwrap();
        let geo = loaded.slides[&sid].find_element(&eid).unwrap().geometry.clone();
        assert_eq!(geo.x, 777.0);
        assert_eq!(geo.y, 333.0);
        assert_eq!(loaded.slide_order, vec![sid]);
    }

    #[test]
    fn second_save_overwrites_first_save() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("overwrite.slidedeck");

        // First save: stock sample.
        let deck1 = Deck::sample();
        let s1 = serialize_deck(&deck1).unwrap();
        let mut w1 = BundleWriter::create(&path).unwrap();
        write_serialized(&mut w1, &s1).unwrap();
        w1.finish().unwrap();

        // Second save: blank deck overrides the file at the same path.
        let deck2 = Deck::new_blank();
        let s2 = serialize_deck(&deck2).unwrap();
        let mut w2 = BundleWriter::create(&path).unwrap();
        write_serialized(&mut w2, &s2).unwrap();
        w2.finish().unwrap();

        // Reading back should yield the blank deck's structure (one
        // empty slide, no elements) — not the sample's three children.
        let mut r = BundleReader::open(&path).unwrap();
        let s_back = read_serialized(&mut r).unwrap();
        let loaded = deserialize_deck(s_back).unwrap();
        assert_eq!(loaded.slide_order.len(), 1);
        let sid = &loaded.slide_order[0];
        assert!(loaded.slides[sid].root.children.is_empty());
    }

    #[test]
    fn read_serialized_works_when_optional_theme_files_absent() {
        // Build a minimal bundle by hand: manifest + one slide, no theme files.
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
}
