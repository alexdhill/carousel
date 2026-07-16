// On-disk agent workspace: the live deck materialized as real files.
// claude-code-acp-rs (and every native-tool agent) reads/writes the real
// filesystem, never the ACP client fs methods, so the deck is written out as
// `deck/index.md` + `deck/slides/slideN.html` under a temp dir used as the
// agent cwd. Agent edits land on those files; collect_changes ingests them
// back into the Deck at turn end.

use crate::agent::vfs::{parse_slide_write, render_index};
use crate::deck::element::ElementNode;
use crate::deck::Deck;
use crate::html::serialize::serialize_slide;
use std::fmt::Write as _;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tracing::warn;

// SlideChange
// One slide file the agent touched this turn. `slide_ref` is the positional
// reference from the filename (e.g. "slide4"), resolved to a real slide id by
// the caller. `is_new` is true when the file did not exist at materialization,
// i.e. the agent created a new slide (append) rather than editing an existing
// one.
#[derive(Debug, Clone)]
pub struct SlideChange {
    pub slide_ref: String,
    pub new_children: Vec<ElementNode>,
    pub is_new: bool,
    pub skipped: usize,
}

// Workspace
// Owns a temp dir (auto-removed on drop) holding the materialized deck, and a
// map of slide filename -> last content this process wrote or ingested. The
// map is the change baseline: a file whose content diverges from its baseline
// is an agent edit; a slide file absent from the map is an agent-created slide.
pub struct Workspace {
    dir: TempDir,
    known: HashMap<String, String>,
}

impl Workspace {
    // create
    // Input: the live deck. Output: a Workspace with the deck written out under
    // a fresh temp dir. Errors: io failures creating the dir or files.
    // Control flow: make temp dir, delegate to write_all.
    pub fn create(deck: &Deck) -> io::Result<Workspace> {
        assert!(!deck.slides.is_empty(), "cannot materialize an empty deck");
        let dir: TempDir = tempfile::Builder::new()
            .prefix("carousel-deck-")
            .tempdir()?;
        let mut ws: Workspace = Workspace {
            dir,
            known: HashMap::new(),
        };
        ws.write_all(deck)?;
        Ok(ws)
    }

    // path
    // Input: &self. Output: the workspace root, used as the agent cwd.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    // write_all
    // Input: &mut self, the live deck. Output: Ok after writing index.md and one
    // slideN.html per slide, resetting the change baseline. Errors: io failures.
    // Control flow: ensure deck/slides exists, write index, loop slide_order.
    pub fn write_all(&mut self, deck: &Deck) -> io::Result<()> {
        assert!(!deck.slides.is_empty(), "cannot materialize an empty deck");
        let slides_dir: PathBuf = self.dir.path().join("deck").join("slides");
        fs::create_dir_all(&slides_dir)?;
        let deck_dir: PathBuf = self.dir.path().join("deck");
        fs::write(deck_dir.join("index.md"), render_index(deck))?;
        fs::write(deck_dir.join("format.md"), __format_reference(deck))?;
        self.known.clear();
        for (index, slide_id) in deck.slide_order.iter().enumerate() {
            let slide = match deck.slides.get(slide_id) {
                Some(s) => s,
                None => continue,
            };
            let name: String = format!("slide{}.html", index + 1);
            let content: String = serialize_slide(slide);
            fs::write(slides_dir.join(&name), &content)?;
            self.known.insert(name, content);
        }
        Ok(())
    }

    // collect_changes
    // Input: &mut self. Output: one SlideChange per slide file the agent edited
    // (baseline diverged) or created (file absent from baseline), each parsed
    // cleanly; the baseline is advanced for every accepted change. Errors: none
    // (unreadable or unparseable files are skipped, leaving the baseline stale to
    // retry). Control flow: diff the known files for edits, then scan the slides
    // dir for new files in ascending positional order.
    pub fn collect_changes(&mut self) -> Vec<SlideChange> {
        let slides_dir: PathBuf = self.dir.path().join("deck").join("slides");
        let mut out: Vec<SlideChange> = Vec::new();
        for name in self.__edited_names(&slides_dir) {
            self.__ingest_file(&slides_dir, &name, false, &mut out);
        }
        for name in self.__new_names(&slides_dir) {
            self.__ingest_file(&slides_dir, &name, true, &mut out);
        }
        out
    }

    // __ingest_file
    // Input: &mut self, the slides dir, a filename, whether the file is new, and
    // the output sink. Output: none; on a clean parse pushes a SlideChange and
    // advances the baseline, else logs and leaves the baseline untouched.
    fn __ingest_file(&mut self, slides_dir: &Path, name: &str, is_new: bool, out: &mut Vec<SlideChange>) {
        assert!(!name.is_empty(), "__ingest_file: empty filename");
        let content: String = match fs::read_to_string(slides_dir.join(name)) {
            Ok(c) => c,
            Err(_) => return,
        };
        let vpath: String = format!("/deck/slides/{}", name);
        match parse_slide_write(&vpath, &content) {
            Ok(sw) => {
                self.known.insert(name.to_string(), content);
                out.push(SlideChange {
                    slide_ref: sw.slide_id,
                    new_children: sw.new_children,
                    is_new,
                    skipped: sw.skipped,
                });
            }
            Err(e) => warn!("agent wrote unparseable slide {}: {}", name, e),
        }
    }

    // __edited_names
    // Input: &self, the slides dir. Output: baseline filenames whose on-disk
    // content now differs from the baseline. Errors: unreadable files are
    // treated as unchanged (skipped).
    fn __edited_names(&self, slides_dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (name, baseline) in self.known.iter() {
            match fs::read_to_string(slides_dir.join(name)) {
                Ok(c) if c != *baseline => out.push(name.clone()),
                _ => {}
            }
        }
        out
    }

    // __new_names
    // Input: &self, the slides dir. Output: slideN.html files present on disk but
    // absent from the baseline (agent-created), sorted ascending by N so multiple
    // new slides append in the order the agent numbered them. Errors: an
    // unreadable dir yields an empty list.
    fn __new_names(&self, slides_dir: &Path) -> Vec<String> {
        let entries = match fs::read_dir(slides_dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        let mut numbered: Vec<(u32, String)> = Vec::new();
        for entry in entries.flatten() {
            let name: String = entry.file_name().to_string_lossy().into_owned();
            if self.known.contains_key(&name) {
                continue;
            }
            if let Some(n) = __slide_file_number(&name) {
                numbered.push((n, name));
            }
        }
        numbered.sort_by_key(|(n, _)| *n);
        numbered.into_iter().map(|(_, name)| name).collect()
    }
}

// __format_reference
// Input: the live deck. Output: deck/format.md — the list of image assets the
// agent may reference in an image element's `data-asset-id` (assets whose
// media_type starts with "image/"), or a note that none exist. The element
// markup schema itself is injected into the agent's prompt context, not here.
// Control flow: append a bullet per image asset.
fn __format_reference(deck: &Deck) -> String {
    let mut out: String = String::from(
        "# Image assets\n\nUse one of these ids in an image element's \
`data-asset-id`. New image files cannot be added from HTML.\n\n",
    );
    let mut any: bool = false;
    for entry in deck.assets.assets.iter() {
        if !entry.media_type.starts_with("image/") {
            continue;
        }
        any = true;
        let _ = writeln!(out, "- `{}` — {}", entry.id, entry.original_filename);
    }
    if !any {
        out.push_str("- (none — this deck has no image assets, so no images can be added)\n");
    }
    out
}

// __slide_file_number
// Input: a filename. Output: Some(N) when it is exactly "slideN.html" with N a
// positive integer, else None. Used to identify and order agent-created slides.
fn __slide_file_number(name: &str) -> Option<u32> {
    let digits: &str = name.strip_prefix("slide")?.strip_suffix(".html")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u32>().ok().filter(|n| *n > 0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn materializes_index_and_slides() {
        let deck: Deck = Deck::sample();
        let ws: Workspace = Workspace::create(&deck).unwrap();
        let root: &Path = ws.path();
        assert!(root.join("deck").join("index.md").exists());
        assert!(root.join("deck").join("format.md").exists());
        assert!(root.join("deck").join("slides").join("slide1.html").exists());
    }

    #[test]
    fn collect_changes_reports_only_edited_slides() {
        let deck: Deck = Deck::sample();
        let mut ws: Workspace = Workspace::create(&deck).unwrap();
        assert!(ws.collect_changes().is_empty(), "no edits yet");
        let slide1: PathBuf = ws.path().join("deck").join("slides").join("slide1.html");
        let original: String = fs::read_to_string(&slide1).unwrap();
        fs::write(&slide1, format!("{}{}", original, original)).unwrap();
        let changes: Vec<SlideChange> = ws.collect_changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].slide_ref, "slide1");
        assert!(!changes[0].is_new);
        assert!(ws.collect_changes().is_empty(), "baseline advanced");
    }

    #[test]
    fn collect_changes_reports_agent_created_slides() {
        let deck: Deck = Deck::sample();
        let mut ws: Workspace = Workspace::create(&deck).unwrap();
        let slides_dir: PathBuf = ws.path().join("deck").join("slides");
        let existing: usize = deck.slide_order.len();
        let template: String =
            fs::read_to_string(slides_dir.join("slide1.html")).unwrap();
        let new_name: String = format!("slide{}.html", existing + 1);
        fs::write(slides_dir.join(&new_name), &template).unwrap();
        let changes: Vec<SlideChange> = ws.collect_changes();
        assert_eq!(changes.len(), 1);
        assert!(changes[0].is_new);
        assert_eq!(changes[0].slide_ref, format!("slide{}", existing + 1));
        assert!(ws.collect_changes().is_empty(), "new slide baselined");
    }
}
