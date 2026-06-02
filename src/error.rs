use crate::bundle::BundleError;

// AppError
// One error type for the bootstrap, IPC, and bundle I/O layers. Implements
// From for the underlying crate errors so call-sites can use `?`.
// Downstream layers (commands, deck) define their own error enums and
// bubble up via dedicated variants here.
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
}

pub type AppResult<T> = Result<T, AppError>;
