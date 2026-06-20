// PDF export: build a single static print-HTML document — one page per
// (slide, step) rendered at that step's snapped reveal — for the macOS webview
// print-to-PDF path. This module is pure (string in/out); the rendering lives
// in the macOS print integration.
use crate::deck::animation::step_count;
use crate::deck::Deck;
use crate::html::serialize::{serialize_slide_themed, ANIMATION_KEYFRAMES_CSS};
use crate::present::reveal::snap_reveal;
use base64::Engine;

// Reduced render scale: pages are ~2/3 of the deck pixels so AppKit rasterizes
// at print DPI rather than full 1920-px bitmaps.
const PDF_PAGE_SCALE: f64 = 2.0 / 3.0;

// pdf_page_size_pt
// Inputs: the deck. Output: the print page size in POINTS (1pt = 1/72in) at the
// reduced scale — the value to set on NSPrintInfo.paperSize (CSS @page is
// ignored by a programmatic NSPrintOperation). px→pt is ×72/96 = ×0.75.
pub fn pdf_page_size_pt(deck: &Deck) -> (f64, f64) {
    let w = deck.manifest.dimensions.width as f64;
    let h = deck.manifest.dimensions.height as f64;
    (w * PDF_PAGE_SCALE * 0.75, h * PDF_PAGE_SCALE * 0.75)
}

// pdf_asset_vars
// Inputs: the asset registry. Output: a :root { --asset-<id>: url(data:…) }
// block inlining each asset as a base64 data URI (the print doc is transient,
// so assets cannot be file references).
fn pdf_asset_vars(reg: &crate::bundle::assets::AssetRegistry) -> String {
    let mut s = String::from(":root {\n");
    for entry in &reg.assets {
        if let Some(bytes) = reg.files.get(&entry.path) {
            let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
            s.push_str(&format!(
                "  --asset-{}: url(data:{};base64,{});\n",
                entry.id, entry.media_type, b64
            ));
        }
    }
    s.push_str("}\n");
    s
}

// build_pdf_print_html
// Inputs: the deck. Output: a single static HTML document with one `.print-page`
// per (slide, step), the slide rendered at `snap_reveal(step)` (hidden elements
// faded to opacity:0), the theme/globals/keyframes CSS with `:host` rewritten to
// `:root` (the pages are light-DOM, not shadow roots), assets inlined as data
// URIs, and `@page` pagination sized to the deck. A load script posts
// `print-ready` so the renderer knows when to print.
pub fn build_pdf_print_html(deck: &Deck) -> String {
    let w: u32 = deck.manifest.dimensions.width;
    let h: u32 = deck.manifest.dimensions.height;
    let theme: String = deck.theme.theme_css.replace(":host", ":root");
    let asset_vars: String = pdf_asset_vars(&deck.assets);

    let mut pages: String = String::new();
    let mut page_index: usize = 0;
    for sid in &deck.slide_order {
        let slide = &deck.slides[sid];
        let timeline = &slide.animations;
        let n: usize = step_count(timeline);
        let (fill, img) = deck.effective_slide_bg(slide);
        let html: String = serialize_slide_themed(slide, fill.as_deref(), img.as_deref());
        let mut step: usize = 0;
        while step < n {
            let reveal = snap_reveal(sid, timeline, step);
            let mut hidden_css: String = String::new();
            for id in &reveal.hidden {
                hidden_css.push_str(&format!(
                    ".print-page[data-page=\"{page_index}\"] [data-element-id=\"{id}\"] {{ opacity: 0; }}\n"
                ));
            }
            pages.push_str(&format!(
                "<div class=\"print-page\" data-page=\"{page_index}\"><style>{hidden_css}</style><div class=\"slide-scale\">{html}</div></div>"
            ));
            page_index += 1;
            step += 1;
        }
    }

    // Render pages at a physical, reduced size (≈2/3 of the deck pixels) so
    // AppKit rasterizes filter/shadow layers at print DPI rather than at giant
    // 1920-px-wide bitmaps. The slide content is fixed at the deck's pixel size,
    // so a wrapper TRANSFORMS it down to fit the page. The transform lives on
    // `.slide-scale` (no clip) while the clips live on `.print-page`/`.slide`, so
    // the transform+overflow combo that breaks backdrop-filter is avoided.
    let scale: f64 = PDF_PAGE_SCALE;
    let page_in_w: f64 = w as f64 * scale / 96.0;
    let page_in_h: f64 = h as f64 * scale / 96.0;
    let page_px_w: f64 = w as f64 * scale;
    let page_px_h: f64 = h as f64 * scale;
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><style>\n\
{theme}\n{globals}\n{keyframes}\n{asset_vars}\n\
/* Force background images/colors to print — WebKit drops them otherwise. */\n\
* {{ -webkit-print-color-adjust: exact; print-color-adjust: exact; }}\n\
@page {{ size: {page_in_w:.3}in {page_in_h:.3}in; margin: 0; }}\n\
html, body {{ margin: 0; padding: 0; }}\n\
.print-page {{ width: {page_px_w:.1}px; height: {page_px_h:.1}px; page-break-after: always; position: relative; overflow: hidden; }}\n\
.slide-scale {{ width: {w}px; height: {h}px; transform: scale({scale}); transform-origin: top left; }}\n\
</style></head><body>{pages}\
<script>window.addEventListener('load', function () {{ if (window.ipc) {{ window.ipc.postMessage('print-ready'); }} }});</script>\
</body></html>",
        globals = deck.theme.globals_css,
        keyframes = ANIMATION_KEYFRAMES_CSS,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;

    #[test]
    fn one_page_per_slide_step_with_deck_sized_pagination() {
        let deck = Deck::sample();
        let html = build_pdf_print_html(&deck);
        let total_steps: usize = deck
            .slide_order
            .iter()
            .map(|sid| step_count(&deck.slides[sid].animations))
            .sum();
        assert_eq!(html.matches("class=\"print-page\"").count(), total_steps);
        let w = deck.manifest.dimensions.width;
        let h = deck.manifest.dimensions.height;
        // Pages are sized physically (inches) at 2/3 scale so AppKit rasterizes
        // at print DPI instead of giant 1920-px bitmaps.
        let page_in_w = w as f64 * (2.0 / 3.0) / 96.0;
        assert!(html.contains(&format!("size: {page_in_w:.3}in")));
        assert!(html.contains("class=\"slide-scale\""));
        let _ = h;
        // Theme :host tokens were rewritten to :root for the light-DOM doc.
        assert!(html.contains(":root"));
        assert!(!html.contains(":host"));
    }

    #[test]
    fn hidden_elements_get_a_page_scoped_opacity_rule_and_assets_inline() {
        let mut deck = Deck::sample();
        // An image asset is inlined as a data URI.
        deck.assets.insert_blob(
            vec![1, 2, 3, 4],
            "logo.png".to_string(),
            "image/png".to_string(),
            None,
        );
        let html = build_pdf_print_html(&deck);
        assert!(html.contains("url(data:image/png;base64,"));
    }
}
