use std::fs;
use std::path::{Path, PathBuf};

use crate::config::WorkspaceConfig;
use crate::error::WorkflowError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemResource {
    Repository,
    Specifications,
    ApplicationTests,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Owner {
    Repository(SystemResource),
    Target(String),
    Component {
        target: String,
        component: Option<String>,
    },
}

#[derive(Debug)]
pub struct Ownership<'a> {
    config: &'a WorkspaceConfig,
}

impl<'a> Ownership<'a> {
    #[must_use]
    pub const fn new(config: &'a WorkspaceConfig) -> Self {
        Self { config }
    }

    pub fn owner_of(&self, path: &Path) -> Result<Owner, WorkflowError> {
        let path = canonical_path(path)?;
        if !path.starts_with(&self.config.root) {
            return Err(WorkflowError::Ownership(format!(
                "path '{}' is outside repository '{}'",
                path.display(),
                self.config.root.display()
            )));
        }

        if path.starts_with(&self.config.system.specs) {
            return Ok(Owner::Repository(SystemResource::Specifications));
        }
        if path.starts_with(&self.config.system.tests) {
            return Ok(Owner::Repository(SystemResource::ApplicationTests));
        }

        let mut component_owner = None;
        let mut component_depth = 0;
        for target in self.config.targets.values() {
            for component in &target.components {
                if path.starts_with(&component.root) {
                    let depth = component.root.components().count();
                    if depth > component_depth {
                        component_depth = depth;
                        component_owner = Some(Owner::Component {
                            target: target.name.clone(),
                            component: component.name.clone(),
                        });
                    }
                }
            }
        }
        if let Some(owner) = component_owner {
            return Ok(owner);
        }

        for target in self.config.targets.values() {
            if path.starts_with(&target.root) {
                return Ok(Owner::Target(target.name.clone()));
            }
        }

        Ok(Owner::Repository(SystemResource::Repository))
    }
}

fn canonical_path(path: &Path) -> Result<PathBuf, WorkflowError> {
    fs::canonicalize(path).map_err(|error| {
        WorkflowError::Ownership(format!(
            "cannot resolve owned path '{}': {error}",
            path.display()
        ))
    })
}
