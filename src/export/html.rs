use crate::deck::Deck;
use crate::deck::animation::step_count;
use crate::html::serialize::{ANIMATION_KEYFRAMES_CSS, serialize_slide_themed};
use crate::present::reveal::{forward_reveal, snap_reveal};
use serde::Serialize;

const INDEX_HTML: &str = include_str!("../../assets/export/index.html");
const PLAYER_CSS: &str = include_str!("../../assets/export/player.css");
const PLAYER_JS: &str = include_str!("../../assets/export/player.js");
const MORPH_JS: &str = include_str!("../../assets/morph.js");

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

    assets: Vec<AssetFile>,
    slides: Vec<SlideData>,
}

pub fn build_html_export(deck: &Deck) -> Result<ExportBundle, serde_json::Error> {
    let mut slides: Vec<SlideData> = Vec::with_capacity(deck.slide_order.len());
    let count: usize = deck.slide_order.len();
    let date: String = crate::html::serialize::today_ymd();
    for (idx, sid) in deck.slide_order.iter().enumerate() {
        let slide = &deck.slides[sid];
        let timeline = &slide.animations;
        let n: usize = step_count(timeline);
        let mut snaps = Vec::with_capacity(n);
        let mut forwards = Vec::with_capacity(n);
        let mut step: usize = 0;
        while step < n {
            snaps.push(snap_reveal(sid, timeline, step));

            if step == 0 {
                forwards.push(snap_reveal(sid, timeline, 0));
            } else {
                forwards.push(forward_reveal(sid, timeline, step));
            }
            step += 1;
        }
        let (fill, img) = deck.effective_slide_bg(slide);
        let opts = crate::html::serialize::RenderOpts {
            ctx: Some(crate::html::serialize::RenderCtx {
                number: idx + 1,
                count,
                date: date.clone(),
            }),
            hide_placeholders: true,
            min_element_size: 0.0,
        };
        let html: String = serialize_slide_themed(slide, fill.as_deref(), img.as_deref(), &opts);
        slides.push(SlideData {
            html,
            snaps,
            forwards,
        });
    }

    let mut assets: Vec<AssetFile> = Vec::new();
    let mut asset_files: Vec<(String, Vec<u8>)> = Vec::new();
    for entry in &deck.assets.assets {
        if let Some(bytes) = deck.assets.files.get(&entry.path) {
            assets.push(AssetFile {
                id: entry.id.clone(),
                path: entry.path.clone(),
            });
            asset_files.push((entry.path.clone(), bytes.clone()));
        }
    }

    let (font_css, font_files) = crate::export::fonts::build_font_faces(deck);
    let globals_css: String = if font_css.is_empty() {
        deck.theme.globals_css.clone()
    } else {
        format!("{}\n{}", deck.theme.globals_css, font_css)
    };
    let data = DeckData {
        width: deck.manifest.dimensions.width,
        height: deck.manifest.dimensions.height,
        theme_css: deck.theme.theme_css.clone(),
        globals_css,
        keyframes_css: ANIMATION_KEYFRAMES_CSS.to_string(),
        assets,
        slides,
    };
    let deck_js = format!("window.__DECK = {};", serde_json::to_string(&data)?);

    let mut files: Vec<(String, Vec<u8>)> = vec![
        ("index.html".to_string(), INDEX_HTML.as_bytes().to_vec()),
        ("player.css".to_string(), PLAYER_CSS.as_bytes().to_vec()),
        ("morph.js".to_string(), MORPH_JS.as_bytes().to_vec()),
        ("player.js".to_string(), PLAYER_JS.as_bytes().to_vec()),
        ("deck.js".to_string(), deck_js.into_bytes()),
    ];
    files.extend(asset_files);
    files.extend(font_files);
    Ok(ExportBundle { files })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;

    fn file<'a>(b: &'a ExportBundle, name: &str) -> Option<&'a [u8]> {
        b.files
            .iter()
            .find(|(p, _)| p == name)
            .map(|(_, v)| v.as_slice())
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

        assert!(file(&bundle, &entry.path).is_some());

        let deck_js = std::str::from_utf8(file(&bundle, "deck.js").unwrap()).unwrap();
        let v: serde_json::Value = serde_json::from_str(
            deck_js
                .trim_start_matches("window.__DECK = ")
                .trim_end_matches(';'),
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
        for name in [
            "index.html",
            "player.css",
            "morph.js",
            "player.js",
            "deck.js",
        ] {
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
            assert!(
                file(&bundle, &entry.path).is_some(),
                "missing asset {}",
                entry.path
            );
        }
    }

    #[test]
    fn export_substitutes_slide_tokens() {
        use crate::deck::builders::{group_element, text_element};
        let mut deck = Deck::sample();
        let sid = deck.slide_order[0].clone();

        let root = group_element(
            "root",
            vec![text_element("tk", "${slideNumber}/${slideCount}")],
        );
        deck.slides.get_mut(&sid).unwrap().root = root;
        let bundle = build_html_export(&deck).unwrap();
        let deck_js = String::from_utf8(file(&bundle, "deck.js").unwrap().to_vec()).unwrap();
        assert!(
            deck_js.contains("1/"),
            "slide 1 number substituted: {}",
            &deck_js[..200.min(deck_js.len())]
        );
    }
}
