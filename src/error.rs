//! Workflow errors and their stable process exit codes.

use thiserror::Error;

/// Successful workflow completion.
pub const EXIT_SUCCESS: u8 = 0;
/// A provider or verification step failed.
pub const EXIT_VERIFICATION_FAILED: u8 = 1;
/// The command, selector, configuration, or plan was invalid.
pub const EXIT_USAGE: u8 = 2;
/// The workflow was interrupted by the user.
pub const EXIT_INTERRUPTED: u8 = 130;

/// A classified workflow failure.
#[derive(Debug, Error)]
pub enum WorkflowError {
    /// The workspace configuration is invalid.
    #[error("configuration error: {0}")]
    Configuration(String),
    /// The workspace cannot be found or loaded.
    #[error("workspace error: {0}")]
    Workspace(String),
    /// A selector is invalid or unresolved.
    #[error("selector error: {0}")]
    Selector(String),
    /// A path cannot be assigned to an owner.
    #[error("ownership error: {0}")]
    Ownership(String),
    /// Resource discovery failed.
    #[error("discovery error: {0}")]
    Discovery(String),
    /// Capability planning or execution failed.
    #[error("planning error: {0}")]
    Planning(String),
    /// The command invocation is invalid.
    #[error("usage error: {0}")]
    Usage(String),
    /// The recognized command is unavailable.
    #[error("command '{command}' is not implemented in milestone 1")]
    Unavailable {
        /// Unavailable command name.
        command: String,
    },
    /// One or more verification steps failed.
    #[error("one or more verification steps failed")]
    VerificationFailed,
    /// The user interrupted execution.
    #[error("interrupted")]
    Interrupted,
}

impl WorkflowError {
    /// Returns the stable process exit code for this error class.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::VerificationFailed => EXIT_VERIFICATION_FAILED,
            Self::Interrupted => EXIT_INTERRUPTED,
            Self::Configuration(_)
            | Self::Workspace(_)
            | Self::Selector(_)
            | Self::Ownership(_)
            | Self::Discovery(_)
            | Self::Planning(_)
            | Self::Usage(_)
            | Self::Unavailable { .. } => EXIT_USAGE,
        }
    }
}
