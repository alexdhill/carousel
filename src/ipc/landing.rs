use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum LandingInbound {
    Ready,
    OpenTemplate { theme_id: String, layout_id: String },
    OpenRecent { path: String },
    ForgetRecent { path: String },
    SetAppearance { mode: String },
    OpenDefault,
    Cancel,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ThumbData {
    pub html: String,
    pub css: String,
    pub asset_vars_css: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct LandingRecent {
    pub path: String,
    pub title: String,
    pub modified: u64,
    pub thumb: Option<ThumbData>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct LandingTemplate {
    pub theme_id: String,
    pub theme_name: String,
    pub layout_id: String,
    pub layout_name: String,
    pub background: String,
    pub foreground: String,
    pub accent: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct LandingData {
    pub recents: Vec<LandingRecent>,
    pub templates: Vec<LandingTemplate>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn round_trip(value: &LandingInbound) -> LandingInbound {
        let json = serde_json::to_string(value).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn inbound_variants_roundtrip() {
        for v in [
            LandingInbound::Ready,
            LandingInbound::OpenTemplate {
                theme_id: "dark".into(),
                layout_id: "hero".into(),
            },
            LandingInbound::OpenRecent {
                path: "/x.slidedeck".into(),
            },
            LandingInbound::ForgetRecent {
                path: "/x.slidedeck".into(),
            },
            LandingInbound::SetAppearance {
                mode: "dark".into(),
            },
            LandingInbound::OpenDefault,
            LandingInbound::Cancel,
        ] {
            assert_eq!(round_trip(&v), v);
        }
    }

    #[test]
    fn inbound_is_kind_tagged() {
        let json = serde_json::to_string(&LandingInbound::OpenTemplate {
            theme_id: "light".into(),
            layout_id: "title".into(),
        })
        .unwrap();
        assert!(json.contains("\"kind\":\"OpenTemplate\""));
        assert!(json.contains("\"theme_id\":\"light\""));
    }

    #[test]
    fn data_roundtrips() {
        let data = LandingData {
            recents: vec![LandingRecent {
                path: "/a.slidedeck".into(),
                title: "a".into(),
                modified: 7,
                thumb: None,
            }],
            templates: vec![LandingTemplate {
                theme_id: "light".into(),
                theme_name: "Light".into(),
                layout_id: "title".into(),
                layout_name: "Title".into(),
                background: "#ffffff".into(),
                foreground: "#1a1a1a".into(),
                accent: "#f19035".into(),
            }],
        };
        let json = serde_json::to_string(&data).unwrap();
        let back: LandingData = serde_json::from_str(&json).unwrap();
        assert_eq!(back, data);
    }
}
