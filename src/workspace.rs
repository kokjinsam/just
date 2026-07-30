//! Workspace discovery and invocation-directory resolution.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::WorkspaceConfig;
use crate::error::WorkflowError;

/// A loaded workflow workspace.
#[derive(Debug)]
pub struct Workspace {
    /// Canonical repository root.
    pub root: PathBuf,
    /// Canonical directory from which the workflow was invoked.
    pub invocation: PathBuf,
    /// Validated workspace configuration.
    pub config: WorkspaceConfig,
}

impl Workspace {
    /// Discovers and loads the workspace containing `invocation`.
    ///
    /// # Errors
    ///
    /// Returns an error when no workspace root can be found or its
    /// configuration cannot be loaded.
    pub fn discover(invocation: &Path) -> Result<Self, WorkflowError> {
        let invocation = canonical_directory(invocation, "invocation directory")?;
        let root = invocation
            .ancestors()
            .find(|ancestor| ancestor.join("just.yaml").is_file())
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                WorkflowError::Workspace(format!(
                    "could not find 'just.yaml' in '{}' or any ancestor",
                    invocation.display()
                ))
            })?;
        let config = WorkspaceConfig::load(&root)?;

        Ok(Self {
            root,
            invocation,
            config,
        })
    }
}

/// Resolves the effective invocation directory.
///
/// `caller` overrides `current` when supplied by the shell wrapper.
///
/// # Errors
///
/// Returns an error when the selected directory cannot be canonicalized.
pub fn invocation_directory(
    current: &Path,
    caller: Option<&OsStr>,
) -> Result<PathBuf, WorkflowError> {
    let current = canonical_directory(current, "current directory")?;
    let selected = caller.map_or_else(
        || current.clone(),
        |caller| {
            let caller = PathBuf::from(caller);
            if caller.is_absolute() {
                caller
            } else {
                current.join(caller)
            }
        },
    );
    canonical_directory(&selected, "JUST_CALLER_DIR")
}

fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf, WorkflowError> {
    let resolved = fs::canonicalize(path).map_err(|error| {
        WorkflowError::Workspace(format!(
            "{label} '{}' is not accessible: {error}",
            path.display()
        ))
    })?;
    if !resolved.is_dir() {
        return Err(WorkflowError::Workspace(format!(
            "{label} '{}' is not a directory",
            resolved.display()
        )));
    }
    Ok(resolved)
}
