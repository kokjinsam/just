use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::WorkspaceConfig;
use crate::error::WorkflowError;

#[derive(Debug)]
pub struct Workspace {
    pub root: PathBuf,
    pub invocation: PathBuf,
    pub config: WorkspaceConfig,
}

impl Workspace {
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
