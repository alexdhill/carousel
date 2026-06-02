// Style and presentation primitives.
//
// `Geometry` is shared by every element. The per-type `*Style` structs are
// selected by the `ElementStyle` enum in `element.rs`. `ColorRef` and
// `FontRef` separate theme bindings from literals so theme changes can
// propagate without rewriting elements.

use serde::{Deserialize, Serialize};

// Geometry
// Position, size, rotation, opacity, z-order. Coordinates in pixels;
// rotation in radians; opacity in [0,1].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Geometry {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub rotation: f64,
    pub opacity: f64,
    pub z_order: i32,
}

impl Default for Geometry {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            z_order: 0,
        }
    }
}

// Length
// A CSS length value bound to a unit.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Length {
    pub value: f64,
    pub unit: LengthUnit,
}

impl Length {
    pub fn px(v: f64) -> Self {
        Self { value: v, unit: LengthUnit::Px }
    }
}

impl Default for Length {
    fn default() -> Self {
        Self::px(0.0)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum LengthUnit {
    Px,
    Em,
    Pt,
    Pct,
}

impl LengthUnit {
    pub fn as_css(self) -> &'static str {
        match self {
            LengthUnit::Px => "px",
            LengthUnit::Em => "em",
            LengthUnit::Pt => "pt",
            LengthUnit::Pct => "%",
        }
    }
}

// ColorRef
// Either a theme-palette key (rendered as `var(--theme-<key>)`) or a literal
// CSS color string.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ColorRef {
    Theme(String),
    Literal(String),
}

// FontRef
// Either a theme typography key (rendered as `var(--theme-<key>)`) or a
// literal font family stack.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum FontRef {
    Theme(String),
    Literal(String),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum FontStyle {
    Normal,
    Italic,
}

impl FontStyle {
    pub fn as_css(self) -> &'static str {
        match self {
            FontStyle::Normal => "normal",
            FontStyle::Italic => "italic",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
    Justify,
}

impl TextAlign {
    pub fn as_css(self) -> &'static str {
        match self {
            TextAlign::Left => "left",
            TextAlign::Center => "center",
            TextAlign::Right => "right",
            TextAlign::Justify => "justify",
        }
    }
}

// TextStyle
// All the per-element typography settings. font-family and color use Ref
// types so theme bindings survive serialization.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TextStyle {
    pub font_family: FontRef,
    pub font_size: Length,
    pub font_weight: u16,
    pub font_style: FontStyle,
    pub color: ColorRef,
    pub text_align: TextAlign,
    pub line_height: f64,
    pub letter_spacing: Length,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font_family: FontRef::Theme("body_family".into()),
            font_size: Length::px(24.0),
            font_weight: 400,
            font_style: FontStyle::Normal,
            color: ColorRef::Theme("foreground".into()),
            text_align: TextAlign::Left,
            line_height: 1.2,
            letter_spacing: Length::px(0.0),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ObjectFit {
    Cover,
    Contain,
    Fill,
    None,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum BorderStyle {
    Solid,
    Dashed,
    Dotted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Border {
    pub width: f64,
    pub color: ColorRef,
    pub style: BorderStyle,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Shadow {
    pub offset_x: f64,
    pub offset_y: f64,
    pub blur: f64,
    pub spread: f64,
    pub color: ColorRef,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Filter {
    Blur(f64),
    Brightness(f64),
    Contrast(f64),
    Saturate(f64),
    HueRotate(f64),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ImageStyle {
    pub object_fit: ObjectFit,
    pub border: Option<Border>,
    pub corner_radius: f64,
    pub shadow: Option<Shadow>,
    pub filters: Vec<Filter>,
}

impl Default for ImageStyle {
    fn default() -> Self {
        Self {
            object_fit: ObjectFit::Contain,
            border: None,
            corner_radius: 0.0,
            shadow: None,
            filters: vec![],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum FillRef {
    Color(ColorRef),
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Stroke {
    pub width: f64,
    pub color: ColorRef,
    pub style: BorderStyle,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ShapeStyle {
    pub fill: FillRef,
    pub stroke: Option<Stroke>,
    pub shadow: Option<Shadow>,
}

impl Default for ShapeStyle {
    fn default() -> Self {
        Self {
            fill: FillRef::Color(ColorRef::Literal("#888".into())),
            stroke: None,
            shadow: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct MediaStyle {
    pub controls: bool,
    pub autoplay: bool,
    pub loop_: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TableStyle {
    pub border: Option<Border>,
    pub cell_padding: f64,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn assert_json_roundtrip<T>(value: &T)
    where
        T: serde::Serialize + for<'de> serde::Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).unwrap();
        let back: T = serde_json::from_str(&json).unwrap();
        assert_eq!(&back, value);
    }

    #[test]
    fn geometry_default_is_unit_opacity() {
        let g = Geometry::default();
        assert_eq!(g.opacity, 1.0);
        assert_eq!(g.z_order, 0);
    }

    #[test]
    fn geometry_serde_roundtrips() {
        assert_json_roundtrip(&Geometry {
            x: 10.0, y: -5.0, width: 100.0, height: 50.0,
            rotation: 1.5, opacity: 0.75, z_order: 3,
        });
    }

    #[test]
    fn length_units_format_as_expected_css() {
        assert_eq!(LengthUnit::Px.as_css(), "px");
        assert_eq!(LengthUnit::Em.as_css(), "em");
        assert_eq!(LengthUnit::Pt.as_css(), "pt");
        assert_eq!(LengthUnit::Pct.as_css(), "%");
    }

    #[test]
    fn color_ref_roundtrips_both_variants() {
        assert_json_roundtrip(&ColorRef::Theme("accent".into()));
        assert_json_roundtrip(&ColorRef::Literal("#ff0066".into()));
    }

    #[test]
    fn font_ref_roundtrips_both_variants() {
        assert_json_roundtrip(&FontRef::Theme("title_family".into()));
        assert_json_roundtrip(&FontRef::Literal("Inter, sans-serif".into()));
    }

    #[test]
    fn text_style_default_targets_theme() {
        let t = TextStyle::default();
        assert!(matches!(t.color, ColorRef::Theme(_)));
        assert!(matches!(t.font_family, FontRef::Theme(_)));
        assert_eq!(t.font_weight, 400);
    }

    #[test]
    fn shape_and_image_styles_serde_ok() {
        assert_json_roundtrip(&ImageStyle::default());
        assert_json_roundtrip(&ShapeStyle::default());
        assert_json_roundtrip(&MediaStyle::default());
        assert_json_roundtrip(&TableStyle::default());
    }
}
