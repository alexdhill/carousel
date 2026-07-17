// Agent configuration and cross-thread event types.

pub mod acp;
pub mod vfs;
pub mod workspace;

// AgentConfig
// Spawnable agent binary with command and arguments.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub command: String,
    pub args: Vec<String>,
}

// AgentEvent
// Worker→main-thread boundary events. Activity events (SessionReady, Thought,
// ToolStatus) surface the ACP handshake and reasoning/tool progress. FsRead,
// FsWrite, and PermissionRequest carry a JSON-RPC request_id so the main thread
// can post a reply back to the agent; other variants are terminal signals
// (stream end, error, etc.).
#[derive(Debug)]
pub enum AgentEvent {
    SessionReady,
    Thought {
        text: String,
    },
    ToolStatus {
        id: String,
        title: String,
        status: String,
    },
    StreamChunk {
        role: String,
        text: String,
        final_chunk: bool,
    },
    FsRead {
        request_id: String,
        path: String,
    },
    FsWrite {
        request_id: String,
        path: String,
        contents: String,
    },
    PermissionRequest {
        request_id: String,
        path: String,
        summary: String,
    },
    TurnEnded,
    Failed {
        message: String,
    },
}

// from_named
// Input: a Config reference and the display name of a configured agent.
// Output: Some(AgentConfig) when an agent with a non-empty command carries
// that name, else None (unknown name or blank command).
// Dataflow: look the agent up by name via config::find_agent, then wrap its
// command and args into an AgentConfig for spawn use.
pub fn from_named(cfg: &crate::config::Config, name: &str) -> Option<AgentConfig> {
    assert!(!name.is_empty(), "from_named called with empty name");
    let def = crate::config::find_agent(cfg, name)?;
    if def.command.is_empty() {
        return None;
    }
    Some(AgentConfig {
        command: def.command.clone(),
        args: def.args.clone(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn from_named_returns_none_for_unknown() {
        let cfg: crate::config::Config = crate::config::Config::default();
        assert!(from_named(&cfg, "Claude").is_none());
    }

    #[test]
    fn from_named_returns_some_when_found() {
        let cfg: crate::config::Config = crate::config::Config {
            agents: vec![crate::config::AgentDef {
                name: "Claude".to_string(),
                command: "claude-code".to_string(),
                args: vec!["--flag".to_string(), "value".to_string()],
            }],
            ..crate::config::Config::default()
        };
        let ac: AgentConfig = from_named(&cfg, "Claude").unwrap();
        assert_eq!(ac.command, "claude-code");
        assert_eq!(ac.args, vec!["--flag".to_string(), "value".to_string()]);
        assert!(from_named(&cfg, "Missing").is_none());
    }
}
