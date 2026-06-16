// HTML export: assemble a self-contained playable folder for the deck.
use crate::deck::animation::step_count;
use crate::deck::Deck;
use crate::html::serialize::{serialize_slide, ANIMATION_KEYFRAMES_CSS};
use crate::present::reveal::{forward_reveal, snap_reveal};
use serde::Serialize;

// Static player files, embedded and copied verbatim into the export.
const INDEX_HTML: &str = include_str!("../../assets/export/index.html");
const PLAYER_CSS: &str = include_str!("../../assets/export/player.css");
const PLAYER_JS: &str = include_str!("../../assets/export/player.js");

// ExportBundle
// An in-memory list of (relative path, bytes) to write into the export folder.
pub struct ExportBundle {
    pub files: Vec<(String, Vec<u8>)>,
}

#[derive(Serialize)]
struct SlideData {
    html: String,
    snaps: Vec<crate::ipc::present::RevealPayload>,
    forwards: Vec<crate::ipc::present::RevealPayload>,
}

#[derive(Serialize)]
struct AssetFile {
    id: String,
    path: String,
}

#[derive(Serialize)]
struct DeckData {
    width: u32,
    height: u32,
    theme_css: String,
    globals_css: String,
    keyframes_css: String,
    // The asset id→relative-path list. The player resolves each path to an
    // absolute URL (against document.baseURI) before building the --asset-<id>
    // custom properties — a relative url() inside a custom property in a shadow
    // root does not reliably resolve against the document base in browsers, so
    // it must be absolutized at runtime (keeping it portable if the folder
    // moves).
    assets: Vec<AssetFile>,
    slides: Vec<SlideData>,
}

// build_html_export
// Inputs: the deck. Output: an ExportBundle whose files are the static player,
// a generated deck.js data global, and every asset's bytes. Reuses the
// presentation step/reveal logic so playback matches presentation exactly.
// Errors: serde_json serialization failure (effectively never for this data).
pub fn build_html_export(deck: &Deck) -> Result<ExportBundle, serde_json::Error> {
    let mut slides: Vec<SlideData> = Vec::with_capacity(deck.slide_order.len());
    for sid in &deck.slide_order {
        let slide = &deck.slides[sid];
        let timeline = &slide.animations;
        let n: usize = step_count(timeline);
        let mut snaps = Vec::with_capacity(n);
        let mut forwards = Vec::with_capacity(n);
        let mut step: usize = 0;
        while step < n {
            snaps.push(snap_reveal(sid, timeline, step));
            // forward_reveal is the animated transition INTO step k (k >= 1).
            // Step 0 has no forward (you enter a slide via its snap), so park a
            // snap there; the player only reads forwards[k] when advancing (k>=1).
            if step == 0 {
                forwards.push(snap_reveal(sid, timeline, 0));
            } else {
                forwards.push(forward_reveal(sid, timeline, step));
            }
            step += 1;
        }
        slides.push(SlideData { html: serialize_slide(slide), snaps, forwards });
    }
    // Only assets whose bytes are present get written and listed.
    let mut assets: Vec<AssetFile> = Vec::new();
    let mut asset_files: Vec<(String, Vec<u8>)> = Vec::new();
    for entry in &deck.assets.assets {
        if let Some(bytes) = deck.assets.files.get(&entry.path) {
            assets.push(AssetFile { id: entry.id.clone(), path: entry.path.clone() });
            asset_files.push((entry.path.clone(), bytes.clone()));
        }
    }
    let data = DeckData {
        width: deck.manifest.dimensions.width,
        height: deck.manifest.dimensions.height,
        theme_css: deck.theme.theme_css.clone(),
        globals_css: deck.theme.globals_css.clone(),
        keyframes_css: ANIMATION_KEYFRAMES_CSS.to_string(),
        assets,
        slides,
    };
    let deck_js = format!("window.__DECK = {};", serde_json::to_string(&data)?);

    let mut files: Vec<(String, Vec<u8>)> = vec![
        ("index.html".to_string(), INDEX_HTML.as_bytes().to_vec()),
        ("player.css".to_string(), PLAYER_CSS.as_bytes().to_vec()),
        ("player.js".to_string(), PLAYER_JS.as_bytes().to_vec()),
        ("deck.js".to_string(), deck_js.into_bytes()),
    ];
    files.extend(asset_files);
    Ok(ExportBundle { files })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;

    fn file<'a>(b: &'a ExportBundle, name: &str) -> Option<&'a [u8]> {
        b.files.iter().find(|(p, _)| p == name).map(|(_, v)| v.as_slice())
    }

    #[test]
    fn export_lists_each_written_asset_with_id_and_path() {
        let mut deck = Deck::sample();
        let entry = deck.assets.insert_blob(
            vec![1, 2, 3, 4],
            "logo.png".to_string(),
            "image/png".to_string(),
            None,
        );
        let bundle = build_html_export(&deck).unwrap();
        // The asset's bytes are written at its path.
        assert!(file(&bundle, &entry.path).is_some());
        // deck.js lists the asset with id + the SAME relative path, so the
        // player can absolutize it.
        let deck_js = std::str::from_utf8(file(&bundle, "deck.js").unwrap()).unwrap();
        let v: serde_json::Value = serde_json::from_str(
            deck_js.trim_start_matches("window.__DECK = ").trim_end_matches(';'),
        )
        .unwrap();
        let assets = v["assets"].as_array().unwrap();
        let found = assets.iter().any(|a| {
            a["id"].as_str() == Some(entry.id.as_str())
                && a["path"].as_str() == Some(entry.path.as_str())
        });
        assert!(found, "asset {} not listed in deck.js assets", entry.id);
    }

    #[test]
    fn export_contains_player_and_data_and_assets() {
        let deck = Deck::sample();
        let bundle = build_html_export(&deck).unwrap();
        for name in ["index.html", "player.css", "player.js", "deck.js"] {
            assert!(file(&bundle, name).is_some(), "missing {name}");
        }
        let deck_js = std::str::from_utf8(file(&bundle, "deck.js").unwrap()).unwrap();
        assert!(deck_js.starts_with("window.__DECK = "));
        let parsed: serde_json::Value = serde_json::from_str(
            deck_js
                .trim_start_matches("window.__DECK = ")
                .trim_end_matches(';'),
        )
        .unwrap();
        let slides = parsed["slides"].as_array().unwrap();
        assert_eq!(slides.len(), deck.slide_order.len());
        for (i, sid) in deck.slide_order.iter().enumerate() {
            let n = crate::deck::animation::step_count(&deck.slides[sid].animations);
            assert_eq!(slides[i]["snaps"].as_array().unwrap().len(), n);
            assert_eq!(slides[i]["forwards"].as_array().unwrap().len(), n);
        }
        for entry in &deck.assets.assets {
            assert!(file(&bundle, &entry.path).is_some(), "missing asset {}", entry.path);
        }
    }
}
