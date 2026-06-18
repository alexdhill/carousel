// Animation catalog — the user-facing effect list. One source of truth for
// the add-menu and effect picker; shipped to the client in EditorConfig.
// `kind`: "named" (canned @keyframes) or "property" (property-change). For
// directional effects the named keyframe is the UP variant; the client maps
// the chosen direction to fly-<dir> at author time.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnimCatalogItem {
    pub id: String,
    pub label: String,
    pub category: String,
    pub kind: String,
    pub keyframe: Option<String>,
    pub directional: bool,
}

// named
// Inputs: catalog id, display label, category string, keyframe name, and
// whether the effect is directional.
// Output: a "named" AnimCatalogItem (canned @keyframes effect).
fn named(id: &str, label: &str, cat: &str, kf: &str, dir: bool) -> AnimCatalogItem {
    assert!(!id.is_empty(), "catalog id must not be empty");
    AnimCatalogItem {
        id: id.into(),
        label: label.into(),
        category: cat.into(),
        kind: "named".into(),
        keyframe: Some(kf.into()),
        directional: dir,
    }
}

// animation_catalog
// Inputs: none.
// Output: the full effect list in menu order — 15 canned effects plus one
// property-change entry. The list is the single source of truth shared with
// the client.
pub fn animation_catalog() -> Vec<AnimCatalogItem> {
    let items: Vec<AnimCatalogItem> = vec![
        named("appear", "Appear", "entrance", "appear", false),
        named("fade-in", "Fade In", "entrance", "fade-in", false),
        named("fly-in", "Fly In", "entrance", "fly-in-top", true),
        named("scale-in", "Scale In", "entrance", "scale-in", false),
        named("blur-in", "Blur In", "entrance", "blur-in", false),
        named("pulse", "Pulse", "emphasis", "pulse", false),
        named("bounce", "Bounce", "emphasis", "bounce", false),
        named("shake", "Shake", "emphasis", "shake", false),
        named("spin", "Spin", "emphasis", "spin", false),
        named("flash", "Flash", "emphasis", "flash", false),
        named("disappear", "Disappear", "exit", "disappear", false),
        named("fade-out", "Fade Out", "exit", "fade-out", false),
        named("fly-out", "Fly Out", "exit", "fly-out-top", true),
        named("scale-out", "Scale Out", "exit", "scale-out", false),
        named("blur-out", "Blur Out", "exit", "blur-out", false),
        AnimCatalogItem {
            id: "property".into(),
            label: "Property change".into(),
            category: "property".into(),
            kind: "property".into(),
            keyframe: None,
            directional: false,
        },
    ];
    assert!(!items.is_empty(), "animation catalog must not be empty");
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_has_15_canned_plus_property_and_unique_ids() {
        let c = animation_catalog();
        let canned = c.iter().filter(|i| i.kind == "named").count();
        let prop = c.iter().filter(|i| i.kind == "property").count();
        assert_eq!(canned, 15);
        assert_eq!(prop, 1);
        let mut ids: Vec<&str> = c.iter().map(|i| i.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), c.len(), "catalog ids must be unique");
    }
    #[test]
    fn every_named_keyframe_exists_in_css() {
        let css = crate::html::serialize::ANIMATION_KEYFRAMES_CSS;
        for item in animation_catalog() {
            if let Some(kf) = &item.keyframe {
                assert!(css.contains(&format!("@keyframes {}", kf)),
                    "missing @keyframes {}", kf);
            }
        }
    }
}
