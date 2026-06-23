// Font system access.
//
// Wraps font-kit so the rest of the app deals in plain strings + bytes:
//   - `enumerate_families` lists the installed font families for the styles
//     pane dropdown (sent to the webview as a FontList).
//   - `load_face` + `sniff_format` load a concrete (family, weight, style)
//     face and identify its container format so HTML export can bundle the
//     bytes behind an @font-face.
// Generic CSS family keywords (sans-serif, system-ui, …) are not real faces
// and are filtered out before any font-kit lookup.

use font_kit::family_name::FamilyName;
use font_kit::handle::Handle;
use font_kit::properties::{Properties, Style, Weight};
use font_kit::source::SystemSource;
use tracing::warn;

// FontFormat
// The container formats we recognise from a face's leading magic bytes, with
// the matching file extension and CSS `format()` token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontFormat {
    Ttf,
    Otf,
    Woff,
    Woff2,
}

impl FontFormat {
    // ext / css
    // Output: the on-disk extension and the CSS `src: ... format("…")` token.
    pub fn ext(self) -> &'static str {
        match self {
            FontFormat::Ttf => "ttf",
            FontFormat::Otf => "otf",
            FontFormat::Woff => "woff",
            FontFormat::Woff2 => "woff2",
        }
    }

    pub fn css(self) -> &'static str {
        match self {
            FontFormat::Ttf => "truetype",
            FontFormat::Otf => "opentype",
            FontFormat::Woff => "woff",
            FontFormat::Woff2 => "woff2",
        }
    }
}

// enumerate_families
// Inputs: none. Output: the installed font family names, de-duplicated and
// sorted. Errors are logged and yield an empty list (the dropdown then just
// offers free text). Control flow: query SystemSource, sort, dedup.
pub fn enumerate_families() -> Vec<String> {
    let source = SystemSource::new();
    let mut families: Vec<String> = match source.all_families() {
        Ok(f) => f,
        Err(e) => {
            warn!("font enumeration failed: {}", e);
            return Vec::new();
        }
    };
    families.sort();
    families.dedup();
    families
}

// is_generic_family
// Inputs: a family name. Output: true when it is a CSS generic keyword (or a
// non-family CSS keyword) that has no concrete face to bundle. Comparison is
// case-insensitive and ignores surrounding quotes/whitespace.
pub fn is_generic_family(name: &str) -> bool {
    const GENERIC: [&str; 14] = [
        "sans-serif", "serif", "monospace", "system-ui", "ui-sans-serif",
        "ui-serif", "ui-monospace", "cursive", "fantasy", "emoji", "math",
        "-apple-system", "inherit", "initial",
    ];
    let n: String = name.trim().trim_matches('"').trim_matches('\'').to_ascii_lowercase();
    GENERIC.contains(&n.as_str())
}

// font_slug
// Inputs: a family name. Output: a filesystem-safe lowercase slug (non
// alphanumeric runs collapse to single dashes) for the bundled file name.
pub fn font_slug(name: &str) -> String {
    let mut out: String = String::with_capacity(name.len());
    let mut last_dash: bool = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

// sniff_format
// Inputs: the leading bytes of a font file. Output: the recognised
// FontFormat, or None for an unknown container.
pub fn sniff_format(bytes: &[u8]) -> Option<FontFormat> {
    if bytes.len() < 4 {
        return None;
    }
    match &bytes[0..4] {
        b"wOF2" => Some(FontFormat::Woff2),
        b"wOFF" => Some(FontFormat::Woff),
        b"OTTO" => Some(FontFormat::Otf),
        b"ttcf" | b"true" | [0x00, 0x01, 0x00, 0x00] => Some(FontFormat::Ttf),
        _ => None,
    }
}

// load_face
// Inputs: a family name, a numeric weight (100..900), and whether the face is
// italic. Output: the best-matching installed face's raw bytes, or None when
// no match loads. Control flow: build font-kit Properties, select the best
// match, load it, copy out the underlying file bytes.
pub fn load_face(family: &str, weight: u16, italic: bool) -> Option<Vec<u8>> {
    assert!(!family.is_empty(), "load_face: empty family");
    let source = SystemSource::new();
    let props = Properties {
        weight: Weight(f32::from(weight)),
        style: if italic { Style::Italic } else { Style::Normal },
        stretch: font_kit::properties::Stretch::NORMAL,
    };
    let handle: Handle = source
        .select_best_match(&[FamilyName::Title(family.to_string())], &props)
        .ok()?;
    let font = handle.load().ok()?;
    let data = font.copy_font_data()?;
    Some((*data).clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_known_formats() {
        assert_eq!(sniff_format(b"wOF2...."), Some(FontFormat::Woff2));
        assert_eq!(sniff_format(b"wOFF...."), Some(FontFormat::Woff));
        assert_eq!(sniff_format(b"OTTO...."), Some(FontFormat::Otf));
        assert_eq!(sniff_format(b"true...."), Some(FontFormat::Ttf));
        assert_eq!(sniff_format(&[0x00, 0x01, 0x00, 0x00, 0x00]), Some(FontFormat::Ttf));
        assert_eq!(sniff_format(b"junk"), None);
        assert_eq!(sniff_format(b"ab"), None);
    }

    #[test]
    fn format_ext_and_css() {
        assert_eq!(FontFormat::Ttf.ext(), "ttf");
        assert_eq!(FontFormat::Ttf.css(), "truetype");
        assert_eq!(FontFormat::Otf.css(), "opentype");
        assert_eq!(FontFormat::Woff2.ext(), "woff2");
    }

    #[test]
    fn generic_families_filtered() {
        assert!(is_generic_family("sans-serif"));
        assert!(is_generic_family("  System-UI "));
        assert!(is_generic_family("\"monospace\""));
        assert!(!is_generic_family("Helvetica Neue"));
        assert!(!is_generic_family("Inter"));
    }

    #[test]
    fn slug_is_filesystem_safe() {
        assert_eq!(font_slug("Helvetica Neue"), "helvetica-neue");
        assert_eq!(font_slug("PT Sans!!"), "pt-sans");
        assert_eq!(font_slug("Arial"), "arial");
    }
}
