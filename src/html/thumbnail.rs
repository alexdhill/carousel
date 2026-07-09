// First-slide thumbnail builder.
//
// Renders a deck's first slide into a self-contained payload the landing page
// mounts in a shadow root (see assets/landing.js). Reuses the editor's slide
// serializer and theme sheets so the thumbnail is byte-for-byte the same render
// the editor shows — only scaled down. Images are inlined as data URIs because
// the landing webview has no access to the deck's asset blob URLs.
//
// Best-effort: any unreadable bundle, missing slide, or serialization problem
// yields None, and the card falls back to a blank tile. Never panics on a bad
// deck.

use crate::bundle::deck_io::{deserialize_deck, read_serialized};
use crate::bundle::{BundleReader, SerializedDeck};
use crate::deck::Deck;
use crate::html::serialize::{ANIMATION_KEYFRAMES_CSS, serialize_slide_themed};
use crate::ipc::landing::ThumbData;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use image::ImageFormat;
use image::imageops::FilterType;
use std::io::Cursor;
use std::path::Path;

// Longest edge, in pixels, a thumbnail asset is resized down to. The cards
// render ~180px wide, so 480 covers 2-3x retina without inlining full-
// resolution photographs.
const THUMB_MAX_DIM: u32 = 480;

// JPEG quality for opaque (photographic) thumbnail assets. Small at card size;
// 72 keeps the payload light while looking clean scaled down.
const THUMB_JPEG_QUALITY: u8 = 72;

// Safety ceiling for assets that cannot be decoded/resized (e.g. SVG, WebP with
// these image features off): inline the original bytes only when under this cap,
// else leave the reference blank rather than bloat the landing payload.
const THUMB_HARD_CAP: usize = 12 * 1024 * 1024;

// build_thumb
// Input: a deck bundle path.
// Output: Some(ThumbData) rendering the deck's first slide, or None when the
// bundle is unreadable, has no slides, or fails to load.
// Control flow: open + deserialize the bundle, serialize slide 0 with its
// effective background, assemble the theme sheet, inline referenced assets as
// data URIs, and read the native dimensions from the manifest.
pub fn build_thumb(path: &Path) -> Option<ThumbData> {
    let deck: Deck = load_deck(path)?;
    let first_id = deck.slide_order.first()?;
    let slide = deck.slides.get(first_id)?;
    assert!(!deck.slide_order.is_empty(), "slide_order lost first id");

    let (fill, img) = deck.effective_slide_bg(slide);
    let opts: crate::html::serialize::RenderOpts = crate::html::serialize::RenderOpts {
        ctx: Some(crate::html::serialize::RenderCtx {
            number: 1,
            count: deck.slide_order.len(),
            date: crate::html::serialize::today_ymd(),
        }),
        hide_placeholders: true,
    };
    let html: String = serialize_slide_themed(slide, fill.as_deref(), img.as_deref(), &opts);
    let css: String = format!(
        "{}\n{}\n{}",
        ANIMATION_KEYFRAMES_CSS, deck.theme.theme_css, deck.theme.globals_css
    );
    let asset_vars_css: String = build_asset_vars(&deck, &html);

    Some(ThumbData {
        html,
        css,
        asset_vars_css,
        width: deck.manifest.dimensions.width,
        height: deck.manifest.dimensions.height,
    })
}

// load_deck
// Input: a bundle path. Output: the fully deserialized Deck, or None on any I/O
// or parse error (logged at debug via the caller's context).
fn load_deck(path: &Path) -> Option<Deck> {
    let mut reader: BundleReader = BundleReader::open(path).ok()?;
    let serialized: SerializedDeck = read_serialized(&mut reader).ok()?;
    deserialize_deck(serialized).ok()
}

// build_asset_vars
// Inputs: the loaded deck and the serialized slide HTML.
// Output: a `:host{ --asset-<id>: url(data:…); … }` block mapping every asset id
// referenced by the slide HTML to an inlined data URI, matching the custom-
// property scheme the editor's shadow root uses. Empty string when nothing
// qualifies.
fn build_asset_vars(deck: &Deck, html: &str) -> String {
    let mut body: String = String::new();
    let max: usize = deck.assets.assets.len();
    let mut i: usize = 0;
    while i < max {
        let entry = &deck.assets.assets[i];
        i += 1;
        if !html.contains(&entry.id) {
            continue;
        }
        let bytes: &Vec<u8> = match deck.assets.files.get(&entry.path) {
            Some(b) => b,
            None => continue,
        };
        let (mime, b64): (String, String) = match encode_asset(&entry.media_type, bytes) {
            Some(v) => v,
            None => continue,
        };
        body.push_str("--asset-");
        body.push_str(&entry.id);
        body.push_str(":url(data:");
        body.push_str(&mime);
        body.push_str(";base64,");
        body.push_str(&b64);
        body.push_str(");");
    }
    if body.is_empty() {
        return String::new();
    }
    format!(":host{{{}}}", body)
}

// encode_asset
// Inputs: an asset's media type and raw bytes.
// Output: Some((mime, base64)) ready to embed in a data URI, or None when the
// asset is undecodable and too large to inline safely.
// Control flow: shrink decodable rasters larger than THUMB_MAX_DIM to a small
// PNG; otherwise inline the original bytes (unchanged format/alpha) when under
// the hard cap.
fn encode_asset(media_type: &str, bytes: &[u8]) -> Option<(String, String)> {
    if let Some((mime, small)) = downscale(bytes) {
        return Some((mime, STANDARD.encode(&small)));
    }
    if bytes.len() > THUMB_HARD_CAP {
        return None;
    }
    Some((media_type.to_string(), STANDARD.encode(bytes)))
}

// downscale
// Inputs: raw image bytes.
// Output: Some((mime, bytes)) when the image decodes and is larger than
// THUMB_MAX_DIM on either edge; None when it is already small or does not decode
// (leaving the caller to inline the original). Opaque images re-encode as JPEG
// (far smaller for photographs); images with an alpha channel stay PNG so
// transparency survives.
fn downscale(bytes: &[u8]) -> Option<(String, Vec<u8>)> {
    let img = image::load_from_memory(bytes).ok()?;
    if img.width() <= THUMB_MAX_DIM && img.height() <= THUMB_MAX_DIM {
        return None;
    }
    let thumb = img.resize(THUMB_MAX_DIM, THUMB_MAX_DIM, FilterType::Triangle);
    let mut out: Vec<u8> = Vec::new();
    if thumb.color().has_alpha() {
        thumb
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .ok()?;
        return Some(("image/png".to_string(), out));
    }
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
        Cursor::new(&mut out),
        THUMB_JPEG_QUALITY,
    );
    image::DynamicImage::ImageRgb8(thumb.to_rgb8())
        .write_with_encoder(encoder)
        .ok()?;
    Some(("image/jpeg".to_string(), out))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::bundle::deck_io::serialize_deck;
    use crate::bundle::{BundleWriter, deck_io::write_serialized};
    use std::path::PathBuf;

    fn write_deck(dir: &Path, deck: &Deck) -> PathBuf {
        let path: PathBuf = dir.join("t.slidedeck");
        let serialized = serialize_deck(deck).unwrap();
        let mut writer = BundleWriter::create(&path).unwrap();
        write_serialized(&mut writer, &serialized).unwrap();
        writer.finish().unwrap();
        path
    }

    #[test]
    fn missing_bundle_returns_none() {
        assert!(build_thumb(Path::new("/no/such/deck.slidedeck")).is_none());
    }

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, _| image::Rgb([(x % 256) as u8, 40, 90]));
        let mut out: Vec<u8> = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn downscale_shrinks_large_keeps_small() {
        assert!(
            downscale(&png_bytes(1600, 900)).is_some(),
            "large image resizes"
        );
        assert!(
            downscale(&png_bytes(320, 180)).is_none(),
            "small image untouched"
        );
    }

    #[test]
    fn encode_asset_inlines_undecodable_under_cap() {
        let (mime, b64) = encode_asset("image/svg+xml", b"<svg/>").expect("svg inlines as-is");
        assert_eq!(mime, "image/svg+xml");
        assert!(!b64.is_empty());
    }

    #[test]
    fn builds_thumb_for_sample_deck() {
        let dir = tempfile::tempdir().unwrap();
        let deck = Deck::sample();
        let path = write_deck(dir.path(), &deck);

        let thumb = build_thumb(&path).expect("sample deck should render");
        assert!(thumb.html.contains("<section"));
        assert_eq!(thumb.width, deck.manifest.dimensions.width);
        assert_eq!(thumb.height, deck.manifest.dimensions.height);
        assert!(thumb.css.contains("--theme-foreground"));
    }
}
