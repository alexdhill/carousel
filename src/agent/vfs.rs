// Virtual filesystem mapping for agent reads/writes.
// Translates between the Deck and a flat HTML file view the agent reads/writes.
// Parse-only: never mutates the Deck.

use crate::deck::element::ElementNode;
use crate::deck::{Deck, SlideId};
use crate::html::parse;
use crate::html::serialize::serialize_slide;
use std::fmt;

// SlideWrite
// Result of parsing an agent write: the slide_id, the elements that parsed, and
// `skipped` — how many child elements were dropped because they did not match
// the slide element format (so the caller can advise the user).
#[derive(Debug, Clone)]
pub struct SlideWrite {
    pub slide_id: SlideId,
    pub new_children: Vec<ElementNode>,
    pub skipped: usize,
}

// VfsError
// Variants for unwritable path. Derive Debug and implement Display for messaging.
#[derive(Debug)]
pub enum VfsError {
    UnwritablePath,
}

impl fmt::Display for VfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VfsError::UnwritablePath => write!(f, "path is read-only"),
        }
    }
}

// render_index
// Build /deck/index.md: one line per slide (id, title, element count).
// Inputs: &Deck.
// Output: String with markdown content.
// Errors: none.
// Dataflow: iterate slide_order, fetch each slide, format as markdown line.
pub fn render_index(deck: &Deck) -> String {
    assert!(
        !deck.slide_order.is_empty() || deck.slides.is_empty(),
        "slide_order must align with slides"
    );

    let mut out = String::new();
    out.push_str("# Deck slides (");
    out.push_str(&deck.slide_order.len().to_string());
    out.push_str(" total)\n\nRead or edit a slide using its exact path below.\n\n");

    for (index, slide_id) in deck.slide_order.iter().enumerate() {
        if let Some(slide) = deck.slides.get(slide_id) {
            let title = slide
                .metadata
                .title
                .clone()
                .unwrap_or_else(|| "(untitled)".to_string());
            let element_count = slide.root.children.len();
            out.push_str("- /deck/slides/slide");
            out.push_str(&(index + 1).to_string());
            out.push_str(".html — \"");
            out.push_str(&title);
            out.push_str("\" (");
            out.push_str(&element_count.to_string());
            out.push_str(" elements) [id: ");
            out.push_str(slide_id);
            out.push_str("]\n");
        }
    }

    out
}

// resolve_slide_ref
// Map a path segment to a real slide id. Accepts the exact slide id, or a
// positional reference "slide<N>" / "<N>" (1-based into slide_order) so an
// agent that guesses conventional filenames still hits the right slide.
// Inputs: &Deck, the raw segment. Output: the real slide id, or None.
pub fn resolve_slide_ref(deck: &Deck, segment: &str) -> Option<SlideId> {
    assert!(!segment.is_empty(), "segment must not be empty");
    if deck.slides.contains_key(segment) {
        return Some(segment.to_string());
    }
    let digits: &str = segment.strip_prefix("slide").unwrap_or(segment);
    let n: usize = digits.parse::<usize>().ok()?;
    if n == 0 {
        return None;
    }
    deck.slide_order.get(n - 1).cloned()
}

// render_slide
// Serialize one slide to HTML for a read.
// Inputs: &Deck, a slide reference (real id or positional slide<N>/<N>).
// Output: Option<String> - Some(html) if the slide resolves, None otherwise.
// Errors: none.
// Dataflow: resolve the reference to a real id, look it up, serialize.
pub fn render_slide(deck: &Deck, slide_id: &str) -> Option<String> {
    assert!(!slide_id.is_empty(), "slide_id must not be empty");

    let real: SlideId = resolve_slide_ref(deck, slide_id)?;
    deck.slides.get(&real).map(serialize_slide)
}

// resolve_read
// Dispatch a read path to index or slide content.
// Inputs: &Deck, path as &str.
// Output: Option<String> - Some(content) for valid paths, None otherwise.
// Errors: none.
// Dataflow: match path against /deck/index.md and /deck/slides/<id>.html patterns.
pub fn resolve_read(deck: &Deck, path: &str) -> Option<String> {
    assert!(!path.is_empty(), "path must not be empty");

    if path == "/deck/index.md" {
        return Some(render_index(deck));
    }

    if path.starts_with("/deck/slides/") && path.ends_with(".html") {
        let slide_id = slide_id_from_path(path)?;
        return render_slide(deck, &slide_id);
    }

    None
}

// is_write_allowed_path
// True only for /deck/slides/<id>.html (reject writes to index or unknown).
// Inputs: path as &str.
// Output: bool.
// Errors: none.
// Dataflow: check prefix and suffix.
pub fn is_write_allowed_path(path: &str) -> bool {
    assert!(!path.is_empty(), "path must not be empty");

    path.starts_with("/deck/slides/") && path.ends_with(".html")
}

// parse_slide_write
// Path → slide id, HTML → slide children via the lenient slide parser.
// Inputs: path as &str, contents (HTML) as &str.
// Output: Result<SlideWrite, VfsError>.
// Errors: UnwritablePath if path is not a slide path. Individual malformed
// elements do not error — they are dropped and reported via SlideWrite.skipped.
// Dataflow: check is_write_allowed_path, extract slide_id, parse all child
// elements leniently, return them with the skipped count.
pub fn parse_slide_write(path: &str, contents: &str) -> Result<SlideWrite, VfsError> {
    assert!(!path.is_empty(), "path must not be empty");

    if !is_write_allowed_path(path) {
        return Err(VfsError::UnwritablePath);
    }

    let slide_id = slide_id_from_path(path).ok_or(VfsError::UnwritablePath)?;

    let (new_children, skipped) = parse::parse_slide_children_lenient(contents);

    Ok(SlideWrite {
        slide_id,
        new_children,
        skipped,
    })
}

// slide_id_from_path
// Extract <id> from a slide path /deck/slides/<id>.html.
// Inputs: path as &str.
// Output: Option<SlideId>.
// Errors: none.
// Dataflow: check prefix and suffix, extract middle segment.
pub fn slide_id_from_path(path: &str) -> Option<SlideId> {
    assert!(!path.is_empty(), "path must not be empty");

    if !path.starts_with("/deck/slides/") || !path.ends_with(".html") {
        return None;
    }

    let without_prefix = &path["/deck/slides/".len()..];
    let without_suffix = &without_prefix[..without_prefix.len() - ".html".len()];

    if without_suffix.is_empty() {
        return None;
    }

    Some(without_suffix.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;

    #[test]
    fn is_write_allowed_path_accepts_slide_paths() {
        assert!(is_write_allowed_path("/deck/slides/slide_123.html"));
        assert!(is_write_allowed_path("/deck/slides/abc.html"));
    }

    #[test]
    fn parse_slide_write_keeps_all_good_elements_and_counts_bad() {
        let html = concat!(
            "<section class=\"slide\" data-slide-id=\"s\" data-layout=\"l\" data-root-id=\"r\">",
            "<div class=\"slide__content\">",
            "<div data-element-id=\"a\" data-element-type=\"text\" ",
            "style=\"left:0px;top:0px;width:10px;height:10px\">A</div>",
            "<div data-element-id=\"b\" data-element-type=\"shape\" data-shape=\"ellipse\" ",
            "style=\"left:0px;top:0px;width:10px;height:10px\"></div>",
            "<div>no data attrs — should be skipped</div>",
            "</div></section>",
        );
        let sw = parse_slide_write("/deck/slides/slide1.html", html).unwrap();
        assert_eq!(sw.new_children.len(), 2, "both valid elements kept");
        assert_eq!(sw.skipped, 1, "the malformed element is counted");
    }

    #[test]
    fn parse_slide_write_roundtrips_every_slide_element() {
        let deck = Deck::sample();
        let sid = &deck.slide_order[0];
        let html = render_slide(&deck, sid).unwrap();
        let original = deck.slides[sid].root.children.len();
        let sw = parse_slide_write("/deck/slides/slide1.html", &html).unwrap();
        assert_eq!(sw.new_children.len(), original, "no elements dropped");
        assert_eq!(sw.skipped, 0);
    }

    #[test]
    fn is_write_allowed_path_rejects_index() {
        assert!(!is_write_allowed_path("/deck/index.md"));
    }

    #[test]
    fn is_write_allowed_path_rejects_unknown_paths() {
        assert!(!is_write_allowed_path("/other/path.html"));
        assert!(!is_write_allowed_path("/deck/slides/slide_123.txt"));
    }

    #[test]
    fn slide_id_from_path_extracts_id() {
        let id = slide_id_from_path("/deck/slides/slide_abc123.html");
        assert_eq!(id, Some("slide_abc123".to_string()));
    }

    #[test]
    fn slide_id_from_path_returns_none_for_invalid() {
        assert!(slide_id_from_path("/deck/index.md").is_none());
        assert!(slide_id_from_path("/deck/slides/.html").is_none());
        assert!(slide_id_from_path("/deck/slides/").is_none());
    }

    #[test]
    fn render_index_lists_each_slide() {
        let deck = Deck::sample();
        let index = render_index(&deck);
        assert!(index.contains("Deck slides"));
        for slide_id in &deck.slide_order {
            assert!(index.contains(slide_id));
        }
    }

    #[test]
    fn resolve_slide_ref_accepts_id_and_positional() {
        let deck = Deck::sample();
        let first: &String = deck.slide_order.first().unwrap();
        // exact id resolves to itself
        assert_eq!(resolve_slide_ref(&deck, first).as_ref(), Some(first));
        // positional "slide1" and "1" resolve to the first slide
        assert_eq!(resolve_slide_ref(&deck, "slide1").as_ref(), Some(first));
        assert_eq!(resolve_slide_ref(&deck, "1").as_ref(), Some(first));
        // out-of-range / zero / garbage -> None
        assert!(resolve_slide_ref(&deck, "0").is_none());
        assert!(resolve_slide_ref(&deck, "slide999").is_none());
        assert!(resolve_slide_ref(&deck, "nope").is_none());
        // a guessed positional path reads back the same slide as its real id
        assert_eq!(
            resolve_read(&deck, "/deck/slides/slide1.html"),
            render_slide(&deck, first)
        );
    }

    #[test]
    fn render_index_contains_element_counts() {
        let deck = Deck::sample();
        let index = render_index(&deck);
        assert!(index.contains("elements"));
    }

    #[test]
    fn resolve_read_returns_index_for_index_path() {
        let deck = Deck::sample();
        let result = resolve_read(&deck, "/deck/index.md");
        assert!(result.is_some());
        let content = result.unwrap();
        assert!(content.contains("Deck slides"));
    }

    #[test]
    fn resolve_read_returns_none_for_unknown_path() {
        let deck = Deck::sample();
        assert!(resolve_read(&deck, "/unknown/path").is_none());
    }

    #[test]
    fn resolve_read_returns_some_for_valid_slide_path() {
        let deck = Deck::sample();
        let slide_id = &deck.slide_order[0];
        let path = format!("/deck/slides/{}.html", slide_id);
        let result = resolve_read(&deck, &path);
        assert!(result.is_some());
    }
}
