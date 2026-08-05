use ulid::Ulid;

pub type SlideId = String;
pub type ElementId = String;
pub type LayoutId = String;
pub type AssetId = String;
pub type AnimationId = String;

pub fn new_element_id() -> ElementId {
    let raw: Ulid = Ulid::new();
    let out: String = format!("el_{}", raw);
    assert!(out.starts_with("el_"), "element id must carry el_ prefix");
    out
}

pub fn new_slide_id() -> SlideId {
    let out: String = Ulid::new().to_string();
    assert!(!out.is_empty(), "ULID must produce non-empty string");
    out
}

pub fn new_animation_id() -> AnimationId {
    let out: String = format!("anim_{}", Ulid::new());
    assert!(
        out.starts_with("anim_"),
        "animation id must carry anim_ prefix"
    );
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn element_ids_are_prefixed() {
        let id = new_element_id();
        assert!(id.starts_with("el_"));
        assert!(id.len() > 3);
    }

    #[test]
    fn slide_ids_are_bare_ulids() {
        let id = new_slide_id();

        assert_eq!(id.len(), 26);
        assert!(!id.contains('_'));
    }

    #[test]
    fn ids_are_unique_within_a_burst() {
        let n: usize = 1000;
        let mut seen: std::collections::HashSet<String> =
            std::collections::HashSet::with_capacity(n);
        for _ in 0..n {
            assert!(seen.insert(new_element_id()));
        }
        assert_eq!(seen.len(), n);
    }
}
