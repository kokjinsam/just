use thiserror::Error;

pub const EXIT_SUCCESS: u8 = 0;
pub const EXIT_VERIFICATION_FAILED: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_INTERRUPTED: u8 = 130;

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("workspace error: {0}")]
    Workspace(String),
    #[error("selector error: {0}")]
    Selector(String),
    #[error("usage error: {0}")]
    Usage(String),
    #[error("command '{command}' is not implemented in milestone 1")]
    Unavailable { command: String },
    #[error("one or more verification steps failed")]
    VerificationFailed,
    #[error("interrupted")]
    Interrupted,
}

impl WorkflowError {
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::VerificationFailed => EXIT_VERIFICATION_FAILED,
            Self::Interrupted => EXIT_INTERRUPTED,
            Self::Configuration(_)
            | Self::Workspace(_)
            | Self::Selector(_)
            | Self::Usage(_)
            | Self::Unavailable { .. } => EXIT_USAGE,
        }
    }
}
