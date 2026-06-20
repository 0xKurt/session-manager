use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("toml parse: {0}")]
    TomlDe(#[from] toml::de::Error),

    #[error("toml serialize: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("session {0} not found")]
    SessionNotFound(String),

    #[error("session {0} already exists")]
    SessionExists(String),

    #[error("unknown agent backend: {0}")]
    UnknownBackend(String),

    #[error("agent binary not found for backend `{backend}`: {detail}")]
    BinaryNotFound { backend: String, detail: String },

    #[error("path expansion failed: {0}")]
    PathExpand(String),

    #[error("config path missing parent: {0}")]
    BadConfigPath(PathBuf),

    #[error("supervisor not running")]
    SupervisorDown,

    #[error("OS layer: {0}")]
    Os(String),

    #[error("backend: {0}")]
    Backend(String),

    #[error("{0}")]
    Other(String),
}

impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        Error::Other(e.to_string())
    }
}
