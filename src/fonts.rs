use font_kit::family_name::FamilyName;
use font_kit::handle::Handle;
use font_kit::properties::{Properties, Style, Weight};
use font_kit::source::SystemSource;
use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontFormat {
    Ttf,
    Otf,
    Woff,
    Woff2,
}

impl FontFormat {
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

pub fn is_generic_family(name: &str) -> bool {
    const GENERIC: [&str; 14] = [
        "sans-serif",
        "serif",
        "monospace",
        "system-ui",
        "ui-sans-serif",
        "ui-serif",
        "ui-monospace",
        "cursive",
        "fantasy",
        "emoji",
        "math",
        "-apple-system",
        "inherit",
        "initial",
    ];
    let n: String = name
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_ascii_lowercase();
    GENERIC.contains(&n.as_str())
}

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
        assert_eq!(
            sniff_format(&[0x00, 0x01, 0x00, 0x00, 0x00]),
            Some(FontFormat::Ttf)
        );
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
