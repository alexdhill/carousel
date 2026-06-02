// HTML serialization and parsing.

#![allow(unused_imports)]

//
// `serialize.rs` converts an `ElementNode` tree to the HTML fragment used
// inside `<section class="slide">`. `parse.rs` does the inverse using
// kuchikiki for HTML5-compliant parsing. Round-tripping any tree through
// serialize → parse must yield an equal tree; this is the central contract
// validated by property tests in `parse.rs`.

pub mod parse;
pub mod serialize;

pub use parse::{ParseError, parse_element, parse_slide_fragment};
pub use serialize::{serialize_element, serialize_slide};
