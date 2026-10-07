use serde::Serialize;

/// Stable machine-readable error kinds. Messages must not contain credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    General,
    Config,
    Usage,
    ConfirmationRequired,
    Auth,
    NotFound,
    Unsupported,
    ClientError,
    RetryLater,
    Unverified,
    BrokenPipe,
}

#[derive(Debug, Serialize, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}
