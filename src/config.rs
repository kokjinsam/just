use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Component as PathComponent, Path, PathBuf};

use serde::Deserialize;

use crate::error::WorkflowError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceConfig {
    pub root: PathBuf,
    pub system: SystemPaths,
    pub coverage_lines: Option<u8>,
    pub targets: BTreeMap<String, Target>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPaths {
    pub specs: PathBuf,
    pub tests: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub root: PathBuf,
    pub components: Vec<Component>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    pub name: Option<String>,
    pub root: PathBuf,
    pub coverage_lines: Option<u8>,
    pub implicit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    version: u64,
    #[serde(default)]
    system: RawSystemPaths,
    coverage: Option<RawCoverage>,
    targets: BTreeMap<String, RawTarget>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawSystemPaths {
    specs: PathBuf,
    tests: PathBuf,
}

impl Default for RawSystemPaths {
    fn default() -> Self {
        Self {
            specs: PathBuf::from("specs"),
            tests: PathBuf::from("test"),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCoverage {
    lines: u16,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTarget {
    path: PathBuf,
    components: Option<BTreeMap<String, RawComponent>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawComponent {
    path: PathBuf,
    coverage: Option<RawCoverage>,
}

impl WorkspaceConfig {
    pub fn load(root: &Path) -> Result<Self, WorkflowError> {
        let root = fs::canonicalize(root).map_err(|error| {
            WorkflowError::Configuration(format!(
                "cannot access workspace root '{}': {error}",
                root.display()
            ))
        })?;
        let config_path = root.join("just.yaml");
        let source = fs::read_to_string(&config_path).map_err(|error| {
            WorkflowError::Configuration(format!(
                "cannot read '{}': {error}",
                config_path.display()
            ))
        })?;
        let raw: RawConfig = serde_yaml_ng::from_str(&source).map_err(|error| {
            WorkflowError::Configuration(format!(
                "cannot parse '{}': {error}",
                config_path.display()
            ))
        })?;

        if raw.version != 1 {
            return Err(WorkflowError::Configuration(format!(
                "unsupported configuration version {}; only version 1 is supported",
                raw.version
            )));
        }
        if raw.targets.is_empty() {
            return Err(WorkflowError::Configuration(
                "targets must contain at least one target".into(),
            ));
        }

        let coverage_lines = validate_coverage(raw.coverage.as_ref(), "repository")?;
        let mut targets = BTreeMap::new();
        let mut component_roots = HashMap::<PathBuf, String>::new();

        for (target_name, raw_target) in raw.targets {
            validate_identifier("target", &target_name)?;
            let target_root = owned_directory(
                &root,
                &raw_target.path,
                &format!("target '{target_name}' path"),
            )?;
            let components = match raw_target.components {
                Some(raw_components) => {
                    if raw_components.is_empty() {
                        return Err(WorkflowError::Configuration(format!(
                            "target '{target_name}' components must not be empty"
                        )));
                    }

                    let mut roots = HashMap::<PathBuf, String>::new();
                    let mut components = Vec::with_capacity(raw_components.len());
                    for (component_name, raw_component) in raw_components {
                        validate_identifier("component", &component_name)?;
                        let component_root = owned_directory(
                            &target_root,
                            &raw_component.path,
                            &format!("component '{target_name}:{component_name}' path"),
                        )?;
                        if let Some(existing) =
                            roots.insert(component_root.clone(), component_name.clone())
                        {
                            return Err(WorkflowError::Configuration(format!(
                                "components '{target_name}:{existing}' and \
                                 '{target_name}:{component_name}' have the same root '{}'",
                                component_root.display()
                            )));
                        }
                        components.push(Component {
                            name: Some(component_name),
                            root: component_root,
                            coverage_lines: validate_coverage(
                                raw_component.coverage.as_ref(),
                                "component",
                            )?
                            .or(coverage_lines),
                            implicit: false,
                        });
                    }
                    components
                }
                None => vec![Component {
                    name: None,
                    root: target_root.clone(),
                    coverage_lines,
                    implicit: true,
                }],
            };
            for component in &components {
                let owner = component.name.as_ref().map_or_else(
                    || format!("@{target_name}"),
                    |component| format!("@{target_name}:{component}"),
                );
                if let Some(existing) =
                    component_roots.insert(component.root.clone(), owner.clone())
                {
                    return Err(WorkflowError::Configuration(format!(
                        "'{existing}' and '{owner}' have the same component root '{}'",
                        component.root.display()
                    )));
                }
            }

            targets.insert(
                target_name.clone(),
                Target {
                    name: target_name,
                    root: target_root,
                    components,
                },
            );
        }

        let system = SystemPaths {
            specs: owned_directory(&root, &raw.system.specs, "system.specs")?,
            tests: owned_directory(&root, &raw.system.tests, "system.tests")?,
        };

        Ok(Self {
            root,
            system,
            coverage_lines,
            targets,
        })
    }
}

fn validate_identifier(kind: &str, identifier: &str) -> Result<(), WorkflowError> {
    let mut characters = identifier.chars();
    let valid_first = characters
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric());
    let valid_rest = characters
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'));

    if !valid_first || !valid_rest {
        return Err(WorkflowError::Configuration(format!(
            "{kind} identifier '{identifier}' is ambiguous; use ASCII letters, digits, '-' or '_', \
             starting with a letter or digit"
        )));
    }
    Ok(())
}

fn validate_coverage(
    coverage: Option<&RawCoverage>,
    owner: &str,
) -> Result<Option<u8>, WorkflowError> {
    coverage
        .map(|coverage| {
            u8::try_from(coverage.lines).map_err(|_| {
                WorkflowError::Configuration(format!(
                    "{owner} coverage.lines must be in 0..=100, got {}",
                    coverage.lines
                ))
            })
        })
        .transpose()
        .and_then(|coverage| {
            if coverage.is_some_and(|lines| lines > 100) {
                Err(WorkflowError::Configuration(format!(
                    "{owner} coverage.lines must be in 0..=100, got {}",
                    coverage.unwrap_or_default()
                )))
            } else {
                Ok(coverage)
            }
        })
}

fn owned_directory(owner: &Path, raw: &Path, label: &str) -> Result<PathBuf, WorkflowError> {
    if raw.as_os_str().is_empty() {
        return Err(WorkflowError::Configuration(format!(
            "{label} must not be empty"
        )));
    }
    if raw.is_absolute() {
        return Err(WorkflowError::Configuration(format!(
            "{label} must be relative, got '{}'",
            raw.display()
        )));
    }

    let mut depth = 0_usize;
    for component in raw.components() {
        match component {
            PathComponent::Normal(_) => depth += 1,
            PathComponent::CurDir => {}
            PathComponent::ParentDir if depth > 0 => depth -= 1,
            PathComponent::ParentDir => {
                return Err(WorkflowError::Configuration(format!(
                    "{label} cannot escape its owner: '{}'",
                    raw.display()
                )));
            }
            PathComponent::RootDir | PathComponent::Prefix(_) => {
                return Err(WorkflowError::Configuration(format!(
                    "{label} must be relative, got '{}'",
                    raw.display()
                )));
            }
        }
    }

    let owner = fs::canonicalize(owner).map_err(|error| {
        WorkflowError::Configuration(format!(
            "cannot access owner directory '{}': {error}",
            owner.display()
        ))
    })?;
    let joined = owner.join(raw);
    let resolved = fs::canonicalize(&joined).map_err(|error| {
        WorkflowError::Configuration(format!(
            "{label} must name an existing directory '{}': {error}",
            joined.display()
        ))
    })?;
    if !resolved.is_dir() {
        return Err(WorkflowError::Configuration(format!(
            "{label} must name an existing directory '{}'",
            resolved.display()
        )));
    }
    if !resolved.starts_with(&owner) {
        return Err(WorkflowError::Configuration(format!(
            "{label} cannot escape its owner: '{}'",
            raw.display()
        )));
    }
    Ok(resolved)
}
