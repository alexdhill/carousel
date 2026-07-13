// Virtual filesystem mapping for agent reads/writes.
// Translates between the Deck and a flat HTML file view the agent reads/writes.
// Parse-only: never mutates the Deck.

use crate::deck::{Deck, SlideId};
use crate::deck::element::ElementNode;
use crate::html::serialize::serialize_slide;
use crate::html::parse;
use std::fmt;

// SlideWrite
// Result of parsing an agent write: the slide_id and new_children elements.
#[derive(Debug, Clone)]
pub struct SlideWrite {
    pub slide_id: SlideId,
    pub new_children: Vec<ElementNode>,
}

// VfsError
// Variants for unknown path, unwritable path, parse failure.
// Derive Debug and implement Display for error messaging.
#[derive(Debug)]
pub enum VfsError {
    UnwritablePath,
    ParseFailure(String),
}

impl fmt::Display for VfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VfsError::UnwritablePath => write!(f, "path is read-only"),
            VfsError::ParseFailure(msg) => write!(f, "HTML parse failure: {}", msg),
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
    assert!(!deck.slide_order.is_empty() || deck.slides.is_empty(), "slide_order must align with slides");

    let mut out = String::new();
    out.push_str("# Deck Index\n\n");

    for slide_id in &deck.slide_order {
        if let Some(slide) = deck.slides.get(slide_id) {
            let title = slide.metadata.title.clone().unwrap_or_else(|| "(untitled)".to_string());
            let element_count = slide.root.children.len();
            out.push_str("- ");
            out.push_str(slide_id);
            out.push_str(": ");
            out.push_str(&title);
            out.push_str(" (");
            out.push_str(&element_count.to_string());
            out.push_str(" elements)\n");
        }
    }

    out
}

// render_slide
// Serialize one slide to HTML for a read.
// Inputs: &Deck, slide_id as &str.
// Output: Option<String> - Some(html) if slide exists, None if not found.
// Errors: none.
// Dataflow: lookup slide by id, serialize with serialize_slide.
pub fn render_slide(deck: &Deck, slide_id: &str) -> Option<String> {
    assert!(!slide_id.is_empty(), "slide_id must not be empty");

    let sid: SlideId = slide_id.to_string();
    deck.slides.get(&sid).map(serialize_slide)
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
// Path → slide id, HTML → element tree via existing parser (no deck touch).
// Inputs: path as &str, contents (HTML) as &str.
// Output: Result<SlideWrite, VfsError>.
// Errors: UnwritablePath if path is not a slide path, ParseFailure if HTML is invalid.
// Dataflow: check is_write_allowed_path, extract slide_id, parse contents into
// element tree, assert root is a group, return SlideWrite.
pub fn parse_slide_write(path: &str, contents: &str) -> Result<SlideWrite, VfsError> {
    assert!(!path.is_empty(), "path must not be empty");

    if !is_write_allowed_path(path) {
        return Err(VfsError::UnwritablePath);
    }

    let slide_id = slide_id_from_path(path)
        .ok_or(VfsError::UnwritablePath)?;

    let new_children = parse_html_to_elements(contents)
        .map_err(|e| VfsError::ParseFailure(e.to_string()))?;

    Ok(SlideWrite {
        slide_id,
        new_children,
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

// parse_html_to_elements
// Parse an HTML fragment into a vector of ElementNode children.
// Inputs: html as &str.
// Output: Result<Vec<ElementNode>, parse::ParseError>.
// Errors: parse errors from kuchikiki or parse module.
// Dataflow: wrap in a container div, parse with kuchikiki, extract direct element children only.
fn parse_html_to_elements(html: &str) -> Result<Vec<ElementNode>, parse::ParseError> {
    use kuchikiki::traits::*;

    assert!(html.len() < 10_000_000, "parse_html_to_elements: HTML too large");

    let wrapped = format!("<div>{}</div>", html);
    let doc = kuchikiki::parse_html().one(wrapped);

    let mut out = Vec::new();
    for child in doc.children() {
        if child.as_element().is_some() {
            let html_str = serialize_node_to_html(&child)?;
            out.push(parse::parse_element(&html_str)?);
        }
    }

    Ok(out)
}

// serialize_node_to_html
// Convert a kuchikiki NodeRef back to an HTML string (internal helper).
// Inputs: node as &kuchikiki::NodeRef.
// Output: Result<String, parse::ParseError>.
// Errors: serialization error.
// Dataflow: use kuchikiki's serialize method.
fn serialize_node_to_html(node: &kuchikiki::NodeRef) -> Result<String, parse::ParseError> {
    let mut buf: Vec<u8> = Vec::new();
    node
        .serialize(&mut buf)
        .map_err(|_| parse::ParseError::Serialization)?;
    String::from_utf8(buf).map_err(|_| parse::ParseError::Serialization)
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
        assert!(index.contains("Deck Index"));
        for slide_id in &deck.slide_order {
            assert!(index.contains(slide_id));
        }
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
        assert!(content.contains("Deck Index"));
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
