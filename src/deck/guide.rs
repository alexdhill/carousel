// Guide — a saveable editor alignment line on a slide or layout.
//
// Guides are editor-only aids: they persist in the bundle (per slide and per
// layout) but never enter the element tree, so the slide serializer never emits
// them and they are structurally absent from presentation, HTML export, PDF and
// thumbnails. A slide shows its own guides plus (read-only) the guides of the
// layout it is built on; see Deck::inherited_guides.

use serde::{Deserialize, Serialize};

// GuideAxis
// The orientation of a guide line. Horizontal lines span the slide width and
// move in Y; vertical lines span the height and move in X. The names describe
// the *line*, matching the ruler the user drags it from (top ruler = a
// horizontal line, left ruler = a vertical line).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuideAxis {
    Horizontal,
    Vertical,
}

// Guide
// One alignment line. `pos` is in slide pixels from the slide's top-left
// corner: the Y of a horizontal line, the X of a vertical line. There is no
// persisted id — position is the identity; the editor assigns ephemeral ids on
// hydration for drag/select.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Guide {
    pub axis: GuideAxis,
    pub pos: f64,
}

impl Guide {
    // new
    // Inputs: axis, position in slide px. Output: a Guide. The position is
    // taken verbatim (clamping to the slide bounds is an editor concern).
    pub fn new(axis: GuideAxis, pos: f64) -> Self {
        Self { axis, pos }
    }
}
