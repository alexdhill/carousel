// Presentation mode — runtime.
//
// A transient, separately-windowed renderer that plays the deck slide-by-slide
// and step-by-step. Rust owns the cursor (`AnimationState`) and computes each
// step's resolved visual state; the dedicated presentation webview applies it.
//
// `reveal` is the pure visibility/animate computation; `session` is the
// slide+step state machine driving it.

#![allow(dead_code)]

pub mod reveal;
pub mod session;
