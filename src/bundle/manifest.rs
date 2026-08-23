use crate::bundle::BundleError;
use crate::deck::guide::Guide;
use crate::deck::{AnimationEntry, SlideId, SlideTransition};
use serde::{Deserialize, Serialize};

pub const SUPPORTED_FORMAT_MAJOR: u32 = 1;
pub const CURRENT_FORMAT_VERSION: &str = "1.0";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ManifestData {
    pub format_version: String,
    pub deck_id: String,
    pub metadata: Metadata,
    pub dimensions: Dimensions,
    pub theme: ThemeRef,
    pub slides: Vec<SlideEntry>,
    pub assets_manifest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Metadata {
    pub title: String,
    pub author: String,
    pub created: String,
    pub modified: String,
    pub app_version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
    pub unit: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ThemeRef {
    pub path: String,
    pub theme_id: String,
    #[serde(default)]
    pub overrides: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SlideEntry {
    pub id: SlideId,
    pub path: String,
    pub layout_id: String,
    pub title: String,
    #[serde(default)]
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub transition: Option<SlideTransition>,
    #[serde(default)]
    pub duration_hint: Option<u32>,
    #[serde(default)]
    pub notes_ref: Option<String>,

    #[serde(default)]
    pub animations: Vec<AnimationEntry>,

    #[serde(default)]
    pub guides: Vec<Guide>,

    #[serde(default)]
    pub background: Option<String>,

    #[serde(default)]
    pub background_image: Option<String>,

    #[serde(default)]
    pub notes: Option<String>,
}

pub fn slide_path_for(slide_id: &str) -> String {
    assert!(!slide_id.is_empty(), "slide_path_for: empty id");
    format!("slides/slide_{slide_id}.html")
}

pub fn validate_format_version(version: &str) -> Result<(), BundleError> {
    assert!(!version.is_empty(), "validate_format_version: empty input");
    let major_str: &str = version.split('.').next().unwrap_or("");
    let major: u32 = major_str.parse().map_err(|_| {
        BundleError::IncompatibleVersion(version.to_string(), SUPPORTED_FORMAT_MAJOR)
    })?;
    if major != SUPPORTED_FORMAT_MAJOR {
        return Err(BundleError::IncompatibleVersion(
            version.to_string(),
            SUPPORTED_FORMAT_MAJOR,
        ));
    }
    Ok(())
}

impl Default for ManifestData {
    fn default() -> Self {
        Self {
            format_version: CURRENT_FORMAT_VERSION.to_string(),
            deck_id: ulid::Ulid::new().to_string(),
            metadata: Metadata::default(),
            dimensions: Dimensions::default(),
            theme: ThemeRef::default(),
            slides: Vec::new(),
            assets_manifest: "assets/index.json".to_string(),
        }
    }
}

impl Default for Metadata {
    fn default() -> Self {
        let now: String = current_iso8601();
        Self {
            title: String::new(),
            author: String::new(),
            created: now.clone(),
            modified: now,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

impl Default for Dimensions {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            unit: "px".to_string(),
        }
    }
}

impl Default for ThemeRef {
    fn default() -> Self {
        Self {
            path: "theme/".to_string(),
            theme_id: "default".to_string(),
            overrides: serde_json::Value::Null,
        }
    }
}

fn current_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs: u64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    civil_iso8601_from_unix(secs)
}

fn civil_iso8601_from_unix(secs: u64) -> String {
    assert!(
        secs < (u64::MAX / 2),
        "civil_iso8601_from_unix: implausible time"
    );
    const SECS_PER_DAY: u64 = 86_400;
    let days_total: u64 = secs / SECS_PER_DAY;
    let time_of_day: u64 = secs % SECS_PER_DAY;
    let hour: u64 = time_of_day / 3_600;
    let minute: u64 = (time_of_day / 60) % 60;
    let second: u64 = time_of_day % 60;

    let (year, month, day) = civil_ymd_from_epoch_days(days_total);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_ymd_from_epoch_days(days_in: u64) -> (u32, u32, u32) {
    const MAX_YEARS: u32 = 10_000;
    let mut remaining: u64 = days_in;
    let mut year: u32 = 1970;
    let mut year_iter: u32 = 0;
    while year_iter < MAX_YEARS {
        let year_len: u64 = if is_leap_year(year) { 366 } else { 365 };
        if remaining < year_len {
            break;
        }
        remaining -= year_len;
        year += 1;
        year_iter += 1;
    }
    let months: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month_idx: usize = 0;
    let mut month_iter: usize = 0;
    while month_iter < 12 {
        let mut month_len: u32 = months[month_idx];
        if month_idx == 1 && is_leap_year(year) {
            month_len = 29;
        }
        if remaining < month_len as u64 {
            break;
        }
        remaining -= month_len as u64;
        month_idx += 1;
        month_iter += 1;
    }
    let month: u32 = (month_idx as u32) + 1;
    let day: u32 = (remaining as u32) + 1;
    (year, month, day)
}

fn is_leap_year(y: u32) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn slide_path_uses_canonical_layout() {
        assert_eq!(slide_path_for("01HQTEST"), "slides/slide_01HQTEST.html");
    }

    #[test]
    #[should_panic(expected = "empty id")]
    fn slide_path_panics_on_empty() {
        let _ = slide_path_for("");
    }

    #[test]
    fn current_format_version_validates() {
        validate_format_version(CURRENT_FORMAT_VERSION).unwrap();
    }

    #[test]
    fn minor_version_bump_is_accepted() {
        validate_format_version("1.42").unwrap();
    }

    #[test]
    fn future_major_version_is_rejected() {
        let err = validate_format_version("2.0").unwrap_err();
        assert!(matches!(err, BundleError::IncompatibleVersion(_, _)));
    }

    #[test]
    fn malformed_version_is_rejected() {
        let err = validate_format_version("garbage").unwrap_err();
        assert!(matches!(err, BundleError::IncompatibleVersion(_, _)));
    }

    #[test]
    fn manifest_default_round_trips_json() {
        let m: ManifestData = ManifestData::default();
        let json = serde_json::to_string(&m).unwrap();
        let back: ManifestData = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn manifest_default_fields_match_spec() {
        let m: ManifestData = ManifestData::default();
        assert_eq!(m.format_version, "1.0");
        assert!(!m.deck_id.is_empty());
        assert_eq!(m.dimensions.width, 1920);
        assert_eq!(m.dimensions.height, 1080);
        assert_eq!(m.dimensions.unit, "px");
        assert_eq!(m.theme.path, "theme/");
        assert_eq!(m.theme.theme_id, "default");
        assert_eq!(m.assets_manifest, "assets/index.json");
        assert!(m.slides.is_empty());
    }

    #[test]
    fn slide_entry_round_trips_with_optional_fields_present_and_absent() {
        let with = SlideEntry {
            id: "01HQTEST".into(),
            path: slide_path_for("01HQTEST"),
            layout_id: "title".into(),
            title: "Hello".into(),
            thumbnail: Some("thumbnails/x.png".into()),
            transition: Some(crate::deck::SlideTransition {
                kind: crate::deck::TransitionKind::Fade,
                duration_ms: 400,
                easing: "ease".into(),
            }),
            duration_hint: Some(30),
            notes_ref: None,
            animations: Vec::new(),
            guides: Vec::new(),
            background: None,
            background_image: None,
            notes: None,
        };
        let json_with = serde_json::to_string(&with).unwrap();
        let back_with: SlideEntry = serde_json::from_str(&json_with).unwrap();
        assert_eq!(back_with, with);

        let lean = SlideEntry {
            id: "01HQTEST".into(),
            path: slide_path_for("01HQTEST"),
            layout_id: "blank".into(),
            title: String::new(),
            thumbnail: None,
            transition: None,
            duration_hint: None,
            notes_ref: None,
            animations: Vec::new(),
            guides: Vec::new(),
            background: None,
            background_image: None,
            notes: None,
        };
        let json_lean = serde_json::to_string(&lean).unwrap();
        let back_lean: SlideEntry = serde_json::from_str(&json_lean).unwrap();
        assert_eq!(back_lean, lean);
    }

    #[test]
    fn manifest_parses_missing_optional_slide_fields() {
        let raw = r#"{
            "id":"01HQTEST",
            "path":"slides/slide_01HQTEST.html",
            "layout_id":"title",
            "title":"x"
        }"#;
        let entry: SlideEntry = serde_json::from_str(raw).unwrap();
        assert_eq!(entry.id, "01HQTEST");
        assert!(entry.thumbnail.is_none());
        assert!(entry.transition.is_none());
        assert!(entry.duration_hint.is_none());
        assert!(entry.notes_ref.is_none());
    }

    #[test]
    fn iso8601_for_known_unix_epoch_is_1970() {
        let s = civil_iso8601_from_unix(0);
        assert!(s.starts_with("1970-01-01T00:00"));
    }

    #[test]
    fn iso8601_format_is_canonical() {
        let s = civil_iso8601_from_unix(946_684_800);
        assert_eq!(s, "2000-01-01T00:00:00Z");
    }

    #[test]
    fn iso8601_carries_into_next_day_and_month() {
        let s = civil_iso8601_from_unix(951_868_799);
        assert_eq!(s, "2000-02-29T23:59:59Z");
    }

    #[test]
    fn iso8601_lengths_are_invariant() {
        let s = civil_iso8601_from_unix(0);

        assert_eq!(s.len(), 20);
    }

    #[test]
    fn leap_year_logic_matches_gregorian() {
        assert!(is_leap_year(2000));
        assert!(!is_leap_year(1900));
        assert!(is_leap_year(2024));
        assert!(!is_leap_year(2026));
    }
}
