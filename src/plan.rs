use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::PathBuf;

use crate::capability::{Capability, ProviderId};
use crate::error::WorkflowError;
use crate::ownership::Owner;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StepId(String);

impl StepId {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub id: StepId,
    pub label: String,
    pub capability: Capability,
    pub provider: ProviderId,
    pub owner: Owner,
    pub cwd: PathBuf,
    pub program: OsString,
    pub argv: Vec<OsString>,
    pub env: BTreeMap<OsString, OsString>,
    pub targets: Vec<PathBuf>,
    pub configuration_files: Vec<PathBuf>,
    pub depends_on: Vec<StepId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    steps: Vec<Step>,
}

impl Plan {
    pub fn new(steps: Vec<Step>) -> Result<Self, WorkflowError> {
        let mut unique = Vec::new();
        let mut positions = HashMap::<StepId, usize>::new();
        for step in steps {
            if let Some(position) = positions.get(&step.id) {
                if unique[*position] != step {
                    return Err(WorkflowError::Planning(format!(
                        "conflicting definitions for step '{}'",
                        step.id.as_str()
                    )));
                }
            } else {
                positions.insert(step.id.clone(), unique.len());
                unique.push(step);
            }
        }

        let ids = positions.keys().cloned().collect::<HashSet<_>>();
        for step in &unique {
            for dependency in &step.depends_on {
                if dependency == &step.id {
                    return Err(WorkflowError::Planning(format!(
                        "step '{}' cannot depend on itself",
                        step.id.as_str()
                    )));
                }
                if !ids.contains(dependency) {
                    return Err(WorkflowError::Planning(format!(
                        "step '{}' has missing dependency '{}'",
                        step.id.as_str(),
                        dependency.as_str()
                    )));
                }
            }
        }
        reject_cycles(&unique)?;
        Ok(Self { steps: unique })
    }

    #[must_use]
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }
}

fn reject_cycles(steps: &[Step]) -> Result<(), WorkflowError> {
    let dependencies = steps
        .iter()
        .map(|step| (&step.id, &step.depends_on))
        .collect::<HashMap<_, _>>();
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for step in steps {
        visit(&step.id, &dependencies, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn visit<'a>(
    id: &'a StepId,
    dependencies: &HashMap<&'a StepId, &'a Vec<StepId>>,
    visiting: &mut HashSet<StepId>,
    visited: &mut HashSet<StepId>,
) -> Result<(), WorkflowError> {
    if visited.contains(id) {
        return Ok(());
    }
    if !visiting.insert(id.clone()) {
        return Err(WorkflowError::Planning(format!(
            "cyclic dependency involving step '{}'",
            id.as_str()
        )));
    }
    if let Some(step_dependencies) = dependencies.get(id) {
        for dependency in *step_dependencies {
            visit(dependency, dependencies, visiting, visited)?;
        }
    }
    visiting.remove(id);
    visited.insert(id.clone());
    Ok(())
}
