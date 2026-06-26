// Theme archive I/O — the `.slidetheme` bundle.
//
// A theme is the reusable styling region of a deck: theme_css, globals_css, and
// the set of layout templates (+ display order), plus the asset bytes those
// layouts reference. This module factors that region into its own ZIP archive
// and its own serialize/parse path, reusing the bundle primitives
// (BundleWriter/BundleReader), the layout-HTML serialize/parse, and the
// content-addressed AssetRegistry. It mirrors `deck_io`'s main-thread/worker
// split: `serialize_theme` builds owned bytes on the main thread; `write_theme`
// streams them on the worker; `read_theme` + `deserialize_theme` reverse it.

#![allow(dead_code)]

use crate::bundle::deck_io::LayoutMeta;
use crate::bundle::manifest::validate_format_version;
use crate::bundle::{AssetRegistry, BundleError, BundleReader, BundleResult, BundleWriter};
use crate::deck::element::{ElementContent, ElementNode};
use crate::deck::slide::SlideNode;
use crate::deck::{LayoutNode, ThemeData};
use crate::html::parse::parse_slide_fragment;
use crate::html::serialize::serialize_slide;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tracing::warn;

pub const PATH_THEME_JSON: &str = "theme.json";
pub const PATH_THEME_CSS: &str = "theme.css";
pub const PATH_GLOBALS_CSS: &str = "globals.css";
pub const PATH_ASSETS_INDEX: &str = "assets/index.json";
const THEME_FORMAT_VERSION: &str = "1.0";
// Defensive bound for the layout element-tree walk (house style).
const MAX_TREE_NODES: usize = 100_000;

// theme_layout_path
// Inputs: a layout id.
// Output: the canonical archive path for that layout's serialized HTML root,
// `layouts/<id>.html`. (The theme archive's root is the theme region, so there
// is no `theme/` prefix as in the deck bundle.)
fn theme_layout_path(id: &str) -> String {
    format!("layouts/{id}.html")
}

// ThemeArchiveManifest
// On-disk schema for the archive's theme.json: a format version (validated on
// load like the deck manifest), the theme id + display name, and the layout
// list in display order. Reuses the deck bundle's LayoutMeta shape.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct ThemeArchiveManifest {
    format_version: String,
    theme_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    layouts: Vec<LayoutMeta>,
}

// SerializedTheme
// A complete `.slidetheme` archive's worth of file contents, all owned + Send.
#[derive(Debug, Default)]
pub struct SerializedTheme {
    pub theme_json: String,
    pub theme_css: String,
    pub globals_css: String,
    pub layout_files: BTreeMap<String, String>,
    pub asset_files: BTreeMap<String, Vec<u8>>,
    pub assets_index_json: String,
}

// serialize_theme
// Inputs: the live theme and the deck's asset registry.
// Output: a SerializedTheme holding every archive file. Only the assets the
// theme's layouts reference are collected (§4).
// Errors: BundleError::Json on encode; MalformedManifest if a layout in
// layout_order is missing from the map.
pub fn serialize_theme(theme: &ThemeData, assets: &AssetRegistry) -> BundleResult<SerializedTheme> {
    let mut layout_metas: Vec<LayoutMeta> = Vec::with_capacity(theme.layout_order.len());
    let mut layout_files: BTreeMap<String, String> = BTreeMap::new();
    for lid in &theme.layout_order {
        let layout: &LayoutNode = theme
            .layouts
            .get(lid)
            .ok_or_else(|| BundleError::MalformedManifest(format!("layout {lid} missing")))?;
        layout_metas.push(LayoutMeta {
            id: lid.clone(),
            name: layout.name.clone(),
            background: layout.background.clone(),
            background_image: layout.background_image.clone(),
        });
        // Reuse the slide serializer via a transient SlideNode carrying the
        // layout's own background (a layout root is a Group, like a slide root).
        layout_files.insert(
            theme_layout_path(lid),
            serialize_slide(&layout.preview_slide()),
        );
    }

    let manifest = ThemeArchiveManifest {
        format_version: THEME_FORMAT_VERSION.to_string(),
        theme_id: theme.theme_id.clone(),
        name: theme.theme_id.clone(),
        layouts: layout_metas,
    };
    let theme_json: String = serde_json::to_string_pretty(&manifest)?;

    // Collect only the assets the layouts reference, into a theme-scoped
    // registry that becomes assets/index.json + the asset bytes.
    let referenced: BTreeSet<String> = collect_layout_asset_ids(theme);
    let mut scoped: AssetRegistry = AssetRegistry::new_empty();
    let mut asset_files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for id in &referenced {
        let entry = match assets.find_by_id(id) {
            Some(e) => e,
            None => {
                warn!(asset_id = %id, "serialize_theme: referenced asset missing from registry");
                continue;
            }
        };
        let bytes = match assets.files.get(&entry.path) {
            Some(b) => b,
            None => {
                warn!(asset_id = %id, "serialize_theme: asset bytes missing");
                continue;
            }
        };
        scoped.assets.push(entry.clone());
        scoped.files.insert(entry.path.clone(), bytes.clone());
        asset_files.insert(entry.path.clone(), bytes.clone());
    }
    let assets_index_json: String = scoped.index_json()?;

    Ok(SerializedTheme {
        theme_json,
        theme_css: theme.theme_css.clone(),
        globals_css: theme.globals_css.clone(),
        layout_files,
        asset_files,
        assets_index_json,
    })
}

// write_theme
// Inputs: a target BundleWriter, a SerializedTheme.
// Output: side-effect; streams every entry. The caller calls writer.finish().
pub fn write_theme(writer: &mut BundleWriter, src: &SerializedTheme) -> BundleResult<()> {
    writer.write_string(PATH_THEME_JSON, &src.theme_json)?;
    writer.write_string(PATH_THEME_CSS, &src.theme_css)?;
    writer.write_string(PATH_GLOBALS_CSS, &src.globals_css)?;
    for (path, html) in &src.layout_files {
        writer.write_string(path, html)?;
    }
    for (path, bytes) in &src.asset_files {
        writer.write_bytes(path, bytes)?;
    }
    writer.write_string(PATH_ASSETS_INDEX, &src.assets_index_json)?;
    Ok(())
}

// read_theme
// Inputs: a BundleReader on an open archive.
// Output: a SerializedTheme. Optional files (theme.css, globals.css, assets)
// absent → empty defaults, so partial archives still open.
// Errors: MissingEntry on absent theme.json; version validated here.
pub fn read_theme(reader: &mut BundleReader) -> BundleResult<SerializedTheme> {
    let theme_json: String = reader.read_string(PATH_THEME_JSON)?;
    let manifest: ThemeArchiveManifest = serde_json::from_str(&theme_json)?;
    validate_format_version(&manifest.format_version)?;

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

    let mut layout_files: BTreeMap<String, String> = BTreeMap::new();
    for meta in &manifest.layouts {
        let path: String = theme_layout_path(&meta.id);
        if reader.has_entry(&path)? {
            layout_files.insert(path.clone(), reader.read_string(&path)?);
        }
    }

    let assets_index_json: String = if reader.has_entry(PATH_ASSETS_INDEX)? {
        reader.read_string(PATH_ASSETS_INDEX)?
    } else {
        AssetRegistry::new_empty().index_json()?
    };
    let parsed_registry: AssetRegistry = AssetRegistry::from_index_json(&assets_index_json)?;
    let mut asset_files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for entry in &parsed_registry.assets {
        if reader.has_entry(&entry.path)? {
            asset_files.insert(entry.path.clone(), reader.read_bytes(&entry.path)?);
        }
    }

    Ok(SerializedTheme {
        theme_json,
        theme_css,
        globals_css,
        layout_files,
        asset_files,
        assets_index_json,
    })
}

// deserialize_theme
// Inputs: a SerializedTheme from read_theme.
// Output: the parsed ThemeData plus an AssetRegistry holding only the theme's
// assets (entries + bytes) for the importer to merge.
// Errors: MalformedManifest / IncompatibleVersion / MissingEntry / SlideParse.
pub fn deserialize_theme(src: SerializedTheme) -> BundleResult<(ThemeData, AssetRegistry)> {
    let manifest: ThemeArchiveManifest = serde_json::from_str(&src.theme_json)?;
    validate_format_version(&manifest.format_version)?;

    // Rebuild layouts from the manifest list + per-layout HTML. An empty list
    // (degenerate theme) falls back to the Default "blank" seed so the editor
    // always has a layout to show.
    let (layouts, layout_order): (BTreeMap<_, _>, Vec<_>) = if manifest.layouts.is_empty() {
        let seed: ThemeData = ThemeData::default();
        (seed.layouts, seed.layout_order)
    } else {
        let mut layouts: BTreeMap<crate::deck::LayoutId, LayoutNode> = BTreeMap::new();
        let mut layout_order: Vec<crate::deck::LayoutId> =
            Vec::with_capacity(manifest.layouts.len());
        for meta in &manifest.layouts {
            let path: String = theme_layout_path(&meta.id);
            let html: &String = src.layout_files.get(&path).ok_or_else(|| {
                BundleError::MissingEntry(format!("layout html missing for {}", meta.id))
            })?;
            let parsed: SlideNode = parse_slide_fragment(html)
                .map_err(|e| BundleError::SlideParse(format!("layout {}: {}", meta.id, e)))?;
            let mut node: LayoutNode =
                LayoutNode::new(meta.id.clone(), meta.name.clone(), parsed.root);
            node.background = meta.background.clone();
            node.background_image = meta.background_image.clone();
            layouts.insert(meta.id.clone(), node);
            layout_order.push(meta.id.clone());
        }
        (layouts, layout_order)
    };

    let theme: ThemeData = ThemeData {
        theme_id: manifest.theme_id,
        theme_css: src.theme_css,
        globals_css: src.globals_css,
        layouts,
        layout_order,
    };

    let mut assets: AssetRegistry = AssetRegistry::from_index_json(&src.assets_index_json)?;
    let mut files: HashMap<String, Vec<u8>> = HashMap::with_capacity(src.asset_files.len());
    for (p, b) in src.asset_files {
        files.insert(p, b);
    }
    assets.files = files;

    Ok((theme, assets))
}

// collect_layout_asset_ids
// Inputs: the theme.
// Output: the set of asset ids referenced by Image/Media elements across every
// layout's element tree, in sorted (deterministic) order.
fn collect_layout_asset_ids(theme: &ThemeData) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for lid in &theme.layout_order {
        if let Some(layout) = theme.layouts.get(lid) {
            collect_node_assets(&layout.root, &mut out);
        }
    }
    out
}

// collect_node_assets
// Inputs: a root ElementNode, the accumulating id set.
// Output: side-effect; bounded DFS recording every Image/Media asset id.
fn collect_node_assets(root: &ElementNode, out: &mut BTreeSet<String>) {
    let mut stack: Vec<&ElementNode> = Vec::with_capacity(16);
    stack.push(root);
    let mut iter: usize = 0;
    while let Some(node) = stack.pop() {
        assert!(
            iter < MAX_TREE_NODES,
            "collect_node_assets: node bound exceeded"
        );
        iter += 1;
        match &node.content {
            ElementContent::Image(a) | ElementContent::Media(a) if !a.asset_id.is_empty() => {
                out.insert(a.asset_id.clone());
            }
            _ => {}
        }
        for child in &node.children {
            stack.push(child);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::bundle::{AssetRegistry, BundleReader, BundleWriter};
    use crate::deck::builders::{group_element, image_element, text_element};
    use crate::deck::{LayoutNode, ThemeData};
    use tempfile::TempDir;

    // theme_with_image_layout
    // Build (theme, registry) where one layout references a registered image and
    // a second registered asset is left unreferenced (must NOT travel).
    fn theme_with_image_layout() -> (ThemeData, AssetRegistry, String) {
        let mut assets = AssetRegistry::new_empty();
        let used = assets.insert_blob(
            vec![1, 2, 3, 4],
            "logo.png".into(),
            "image/png".into(),
            None,
        );
        let _unused =
            assets.insert_blob(vec![9, 9, 9], "stray.png".into(), "image/png".into(), None);

        let img = image_element("el_img", used.id.clone());
        let title = text_element("el_t", "Title");
        let root = group_element("el_layout_root", vec![title, img]);
        let layout = LayoutNode::new("title".into(), "Title".into(), root);

        let mut theme = ThemeData {
            theme_css: ":host{--x:1}".into(),
            globals_css: "@keyframes spin { to { rotate: 360deg } }".into(),
            ..ThemeData::default()
        };
        theme.layouts.insert("title".into(), layout);
        theme.layout_order.push("title".into());
        (theme, assets, used.id)
    }

    #[test]
    fn serialize_collects_only_referenced_assets() {
        let (theme, assets, used_id) = theme_with_image_layout();
        let s = serialize_theme(&theme, &assets).unwrap();
        // The referenced asset's index mentions the used id; the stray does not
        // travel (only one asset file present).
        assert!(s.assets_index_json.contains(&used_id));
        assert_eq!(s.asset_files.len(), 1);
        assert!(s.layout_files.keys().any(|k| k.contains("title")));
        assert!(s.theme_css.contains("--x"));
        assert!(s.globals_css.contains("@keyframes"));
    }

    #[test]
    fn full_round_trip_preserves_theme_and_referenced_asset() {
        let (theme, assets, used_id) = theme_with_image_layout();
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("t.slidetheme");
        let mut w = BundleWriter::create(&path).unwrap();
        write_theme(&mut w, &serialize_theme(&theme, &assets).unwrap()).unwrap();
        w.finish().unwrap();

        let mut r = BundleReader::open(&path).unwrap();
        let (back_theme, back_assets) = deserialize_theme(read_theme(&mut r).unwrap()).unwrap();

        assert_eq!(back_theme.theme_css, theme.theme_css);
        assert_eq!(back_theme.globals_css, theme.globals_css);
        assert!(back_theme.layout_order.contains(&"title".to_string()));
        assert_eq!(back_theme.layouts["title"].name, "Title");
        // Layout element tree survived the HTML round-trip.
        let root = &back_theme.layouts["title"].root;
        assert!(root.children.iter().any(|c| c.id == "el_img"));
        // The referenced asset travelled with its bytes; the stray did not.
        assert_eq!(back_assets.assets.len(), 1);
        let entry = back_assets
            .find_by_id(&used_id)
            .expect("used asset present");
        assert_eq!(
            back_assets.files.get(&entry.path),
            Some(&vec![1u8, 2, 3, 4])
        );
    }
}
