use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AgentPanelState {
    pub running: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AgentStreamChunk {
    pub role: String,
    pub text: String,
    pub final_chunk: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AgentToolNotice {
    pub kind: String,
    #[serde(default)]
    pub slide_id: Option<String>,
    pub summary: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AgentList {
    pub agents: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AgentPermissionAsk {
    pub request_id: String,
    pub slide_id: String,
    pub summary: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AgentActivity {
    pub phase: String,
    pub label: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AgentThought {
    pub text: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AgentToolStatus {
    pub id: String,
    pub title: String,
    pub status: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn round_trip<T>(value: &T) -> T
    where
        T: serde::Serialize + for<'de> serde::Deserialize<'de>,
    {
        let json = serde_json::to_string(value).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn agent_panel_state_roundtrips() {
        let state = AgentPanelState {
            running: true,
            error: None,
        };
        assert_eq!(round_trip(&state), state);
    }

    #[test]
    fn agent_stream_chunk_roundtrips() {
        let chunk = AgentStreamChunk {
            role: "assistant".into(),
            text: "hello".into(),
            final_chunk: false,
        };
        assert_eq!(round_trip(&chunk), chunk);
    }

    #[test]
    fn agent_tool_notice_roundtrips() {
        let notice = AgentToolNotice {
            kind: "fs_read".into(),
            slide_id: Some("s1".into()),
            summary: "read slide 1".into(),
        };
        assert_eq!(round_trip(&notice), notice);
    }

    #[test]
    fn agent_permission_ask_roundtrips() {
        let ask = AgentPermissionAsk {
            request_id: "req_123".into(),
            slide_id: "s1".into(),
            summary: "write slide 1".into(),
        };
        assert_eq!(round_trip(&ask), ask);
    }

    #[test]
    fn agent_activity_roundtrips() {
        let activity = AgentActivity {
            phase: "thinking".into(),
            label: "Analyzing slide content".into(),
        };
        assert_eq!(round_trip(&activity), activity);
    }

    #[test]
    fn agent_thought_roundtrips() {
        let thought = AgentThought {
            text: "The user wants to refactor this slide for clarity.".into(),
        };
        assert_eq!(round_trip(&thought), thought);
    }

    #[test]
    fn agent_tool_status_roundtrips() {
        let status = AgentToolStatus {
            id: "tool_1".into(),
            title: "Update Text".into(),
            status: "in_progress".into(),
        };
        assert_eq!(round_trip(&status), status);
    }
}
