use crate::deck::{Deck, ElementNode, ElementStyle, FontRef, FontStyle, TextStyle};
use crate::fonts::{font_slug, is_generic_family, load_face, sniff_format};
use std::collections::{BTreeMap, BTreeSet};

type UsedFace = (String, u16, bool);

pub fn build_font_faces(deck: &Deck) -> (String, Vec<(String, Vec<u8>)>) {
    let vars: BTreeMap<String, String> = collect_family_vars(deck);
    let faces: BTreeSet<UsedFace> = collect_used_faces(deck, &vars);
    let mut css: String = String::new();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for (family, weight, italic) in &faces {
        let bytes: Vec<u8> = match load_face(family, *weight, *italic) {
            Some(b) => b,
            None => continue,
        };
        let fmt = match sniff_format(&bytes) {
            Some(f) => f,
            None => continue,
        };
        let style: &str = if *italic { "italic" } else { "normal" };
        let path: String = format!(
            "fonts/{}-{}-{}.{}",
            font_slug(family),
            weight,
            style,
            fmt.ext()
        );
        css.push_str(&font_face_rule(family, *weight, *italic, &path, fmt.css()));
        files.push((path, bytes));
    }
    (css, files)
}

fn font_face_rule(family: &str, weight: u16, italic: bool, path: &str, fmt: &str) -> String {
    let style: &str = if italic { "italic" } else { "normal" };
    format!(
        "@font-face{{font-family:\"{}\";font-weight:{};font-style:{};\
src:url(\"{}\") format(\"{}\");}}\n",
        family, weight, style, path, fmt
    )
}

fn collect_family_vars(deck: &Deck) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    parse_family_vars(&deck.theme.theme_css, &mut out);
    parse_family_vars(&deck.theme.globals_css, &mut out);
    out
}

fn parse_family_vars(css: &str, out: &mut BTreeMap<String, String>) {
    for raw in css.split(';') {
        let decl: &str = raw.trim();
        let body: &str = match decl.strip_prefix("--") {
            Some(b) => b,
            None => continue,
        };
        let colon: usize = match body.find(':') {
            Some(i) => i,
            None => continue,
        };
        let name: &str = body[..colon].trim();
        if !name.contains("family") {
            continue;
        }
        let value: &str = body[colon + 1..].trim();
        if !name.is_empty() && !value.is_empty() {
            out.insert(name.to_string(), value.to_string());
        }
    }
}

fn collect_used_faces(deck: &Deck, vars: &BTreeMap<String, String>) -> BTreeSet<UsedFace> {
    let mut out: BTreeSet<UsedFace> = BTreeSet::new();
    for slide in deck.slides.values() {
        walk_element(&slide.root, vars, &mut out);
    }
    for layout in deck.theme.layouts.values() {
        walk_element(&layout.root, vars, &mut out);
    }
    out
}

fn walk_element(node: &ElementNode, vars: &BTreeMap<String, String>, out: &mut BTreeSet<UsedFace>) {
    let family_value: Option<String> = effective_family(node);
    if let Some(value) = family_value {
        let weight: u16 = effective_weight(node);
        let italic: bool = effective_italic(node);
        for family in concrete_families(&value, vars) {
            out.insert((family, weight, italic));
        }
    }
    for child in &node.children {
        walk_element(child, vars, out);
    }
}

fn effective_family(node: &ElementNode) -> Option<String> {
    if let Some(v) = node.inline_styles.get("font-family") {
        return Some(v.clone());
    }
    if let ElementStyle::Text(ts) = &node.style {
        return Some(font_ref_css(ts));
    }
    None
}

fn effective_weight(node: &ElementNode) -> u16 {
    if let Some(v) = node.inline_styles.get("font-weight")
        && let Ok(n) = v.trim().parse::<u16>()
    {
        return n;
    }
    if let ElementStyle::Text(ts) = &node.style {
        return ts.font_weight;
    }
    400
}

fn effective_italic(node: &ElementNode) -> bool {
    if let Some(v) = node.inline_styles.get("font-style") {
        return v.trim().eq_ignore_ascii_case("italic");
    }
    if let ElementStyle::Text(ts) = &node.style {
        return ts.font_style == FontStyle::Italic;
    }
    false
}

fn font_ref_css(ts: &TextStyle) -> String {
    match &ts.font_family {
        FontRef::Theme(k) => format!("var(--theme-{})", k.replace('_', "-")),
        FontRef::Literal(s) => s.clone(),
    }
}

fn concrete_families(value: &str, vars: &BTreeMap<String, String>) -> Vec<String> {
    let stack: String = match resolve_var(value.trim(), vars) {
        Some(s) => s,
        None => value.to_string(),
    };
    let mut out: Vec<String> = Vec::new();
    for part in stack.split(',') {
        let fam: &str = part.trim().trim_matches('"').trim_matches('\'').trim();
        if fam.is_empty() || is_generic_family(fam) || fam.starts_with("var(") {
            continue;
        }
        if !out.iter().any(|f| f == fam) {
            out.push(fam.to_string());
        }
    }
    out
}

fn resolve_var(value: &str, vars: &BTreeMap<String, String>) -> Option<String> {
    let inner: &str = value.strip_prefix("var(")?.strip_suffix(')')?;
    let name: &str = inner.trim().strip_prefix("--")?;
    vars.get(name).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parse_family_vars_picks_family_decls_only() {
        let mut out = BTreeMap::new();
        parse_family_vars(
            "--theme-title-family: Inter, sans-serif; --theme-accent: #f00;",
            &mut out,
        );
        assert_eq!(
            out.get("theme-title-family").map(String::as_str),
            Some("Inter, sans-serif")
        );
        assert!(!out.contains_key("theme-accent"));
    }

    #[test]
    fn concrete_families_skips_generics_and_resolves_vars() {
        let v = vars(&[("theme-body-family", "\"Helvetica Neue\", Arial, sans-serif")]);
        assert_eq!(
            concrete_families("\"Inter\", sans-serif", &v),
            vec!["Inter".to_string()]
        );
        assert_eq!(
            concrete_families("var(--theme-body-family)", &v),
            vec!["Helvetica Neue".to_string(), "Arial".to_string()]
        );
        assert!(concrete_families("sans-serif", &v).is_empty());
    }

    #[test]
    fn font_face_rule_shape() {
        let r = font_face_rule("Inter", 700, true, "fonts/inter-700-italic.ttf", "truetype");
        assert!(r.contains("font-family:\"Inter\""));
        assert!(r.contains("font-weight:700"));
        assert!(r.contains("font-style:italic"));
        assert!(r.contains("url(\"fonts/inter-700-italic.ttf\") format(\"truetype\")"));
    }

    #[test]
    fn collect_used_faces_resolves_inline_and_theme() {
        let deck = Deck::sample();
        let v = collect_family_vars(&deck);
        let faces = collect_used_faces(&deck, &v);

        for (family, weight, _italic) in &faces {
            assert!(!family.is_empty());
            assert!(*weight >= 1 && *weight <= 1000);
            assert!(!is_generic_family(family));
        }
    }
}
