use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("TOML parse error in {path}: {source}")]
    Toml {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("validation error: {0}")]
    Validation(String),
    #[error("unknown reference '{kind}' = '{id}'")]
    UnknownRef { kind: &'static str, id: String },
    #[error("compiled MatchInitSpecV1 size {bytes} exceeds {limit} bytes")]
    InitSpecTooLarge { bytes: usize, limit: usize },
}
