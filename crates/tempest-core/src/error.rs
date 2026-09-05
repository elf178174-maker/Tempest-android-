use std::fmt;

/// Every fallible operation in the core returns this.
///
/// Variants carry enough structure that the UI layer can render an actionable
/// message ("which component failed to download?") rather than a flat string.
#[derive(Debug, thiserror::Error)]
pub enum TempestError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Authentication failed: {0}")]
    Auth(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Runtime component '{component}' failed: {reason}")]
    Runtime { component: String, reason: String },

    /// A prerequisite the user (or a previous step) has to satisfy first.
    #[error("{what} is not available: {reason}")]
    Missing { what: String, reason: String },

    #[error("Integrity check failed for {what}: expected {expected}, got {actual}")]
    Integrity {
        what: String,
        expected: String,
        actual: String,
    },

    #[error("Invalid vortex:// URI: {0}")]
    Uri(String),

    #[error("Process error: {0}")]
    Process(String),

    #[error("Operation cancelled")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, TempestError>;

impl TempestError {
    pub fn other(msg: impl fmt::Display) -> Self {
        TempestError::Other(msg.to_string())
    }

    pub fn missing(what: impl Into<String>, reason: impl Into<String>) -> Self {
        TempestError::Missing {
            what: what.into(),
            reason: reason.into(),
        }
    }

    pub fn runtime(component: impl Into<String>, reason: impl fmt::Display) -> Self {
        TempestError::Runtime {
            component: component.into(),
            reason: reason.to_string(),
        }
    }

    /// A stable machine-readable code, so the Android UI can key off the
    /// failure class instead of pattern-matching on English prose.
    pub fn kind(&self) -> &'static str {
        match self {
            TempestError::Config(_) => "config",
            TempestError::Network(_) => "network",
            TempestError::Auth(_) => "auth",
            TempestError::Io(_) => "io",
            TempestError::Runtime { .. } => "runtime",
            TempestError::Missing { .. } => "missing",
            TempestError::Integrity { .. } => "integrity",
            TempestError::Uri(_) => "uri",
            TempestError::Process(_) => "process",
            TempestError::Cancelled => "cancelled",
            TempestError::Other(_) => "other",
        }
    }
}

impl From<reqwest::Error> for TempestError {
    fn from(e: reqwest::Error) -> Self {
        // reqwest's Display can include the full URL, which for Vortex play
        // links carries a session token in the query string. Strip it.
        let status = e.status();
        let mut msg = if e.is_timeout() {
            "request timed out".to_string()
        } else if e.is_connect() {
            "could not connect to the server".to_string()
        } else if e.is_decode() {
            "malformed response from the server".to_string()
        } else {
            e.without_url().to_string()
        };
        if let Some(status) = status {
            msg = format!("HTTP {status}: {msg}");
        }
        TempestError::Network(msg)
    }
}
