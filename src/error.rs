use crate::bundle::BundleError;

#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error("serde_json: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("wry: {0}")]
    Wry(#[from] wry::Error),

    #[error("bundle: {0}")]
    Bundle(#[from] BundleError),

    #[allow(dead_code)]
    #[error("ipc send: channel closed")]
    IpcChannelClosed,

    #[error("agent: {0}")]
    Agent(String),
}

pub type AppResult<T> = Result<T, AppError>;
