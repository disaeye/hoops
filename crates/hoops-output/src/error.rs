use hoops_core::{DecodeError, EngineInitError, ReplayCommandError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OutputError {
    #[error("codec error: {0}")]
    Decode(#[from] DecodeError),
    #[error("zstd codec error: {0}")]
    Zstd(String),
    #[error("engine init error during replay: {0}")]
    EngineInit(#[from] EngineInitError),
    #[error("replay command error: {0}")]
    ReplayCommand(#[from] ReplayCommandError),
    #[error("checksum or hash verification failed: {0}")]
    VerificationFailed(String),
    #[error("io error: {0}")]
    Io(String),
}
