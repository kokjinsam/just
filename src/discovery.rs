//! Git-aware discovery of workflow resources.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::capability::is_oxfmt_source;
use crate::error::WorkflowError;
use crate::ownership::{Owner, Ownership, SystemResource};
use crate::selector::Selection;
use crate::workspace::Workspace;

/// Kind of discovered workflow evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// An Elixir Mix manifest.
    MixManifest,
    /// A Node package manifest.
    NodeManifest,
    /// A Cargo manifest.
    CargoManifest,
    /// Provider configuration.
    Configuration,
    /// A specification model or document.
    Specification,
    /// An application-test resource.
    ApplicationTest,
    /// A directly selected source file.
    SelectedFile,
}

/// A discovered path with its kind and owner.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DiscoveredResource {
    /// Canonical resource path.
    pub path: PathBuf,
    /// Resource kind.
    pub kind: ResourceKind,
    /// Most specific configured owner.
    pub owner: Owner,
}

/// Discovers resources admitted by `selections`.
///
/// # Errors
///
/// Returns an error when selected paths or traversed resources cannot be
/// resolved and classified.
pub fn discover_resources(
    workspace: &Workspace,
    selections: &[Selection],
) -> Result<Vec<DiscoveredResource>, WorkflowError> {
    let ownership = Ownership::new(&workspace.config);
    let mut resources = Vec::new();
    let mut seen = HashSet::new();

    for selection in selections {
        match selection {
            Selection::Repository => {
                discover_directory(
                    workspace,
                    &workspace.root,
                    &ownership,
                    None,
                    &mut resources,
                    &mut seen,
                )?;
            }
            Selection::Target(target_name) => {
                let target = workspace.config.targets.get(target_name).ok_or_else(|| {
                    WorkflowError::Discovery(format!("unknown target '@{target_name}'"))
                })?;
                discover_directory(
                    workspace,
                    &target.root,
                    &ownership,
                    None,
                    &mut resources,
                    &mut seen,
                )?;
            }
            Selection::Component { target, component } => {
                let component = workspace
                    .config
                    .targets
                    .get(target)
                    .and_then(|target| {
                        target
                            .components
                            .iter()
                            .find(|candidate| candidate.name.as_deref() == Some(component))
                    })
                    .ok_or_else(|| {
                        WorkflowError::Discovery(format!(
                            "unknown component '@{target}:{component}'"
                        ))
                    })?;
                discover_directory(
                    workspace,
                    &component.root,
                    &ownership,
                    Some(&Owner::Component {
                        target: target.clone(),
                        component: component.name.clone(),
                    }),
                    &mut resources,
                    &mut seen,
                )?;
            }
            Selection::Path(path) if path.is_dir() => {
                discover_directory(workspace, path, &ownership, None, &mut resources, &mut seen)?;
            }
            Selection::Path(path) => {
                let path = fs::canonicalize(path).map_err(|error| {
                    WorkflowError::Discovery(format!(
                        "cannot inspect explicitly selected file '{}': {error}",
                        path.display()
                    ))
                })?;
                if !path.starts_with(&workspace.root) {
                    return Err(WorkflowError::Discovery(format!(
                        "selected file '{}' escapes repository '{}'",
                        path.display(),
                        workspace.root.display()
                    )));
                }
                if seen.insert(path.clone()) {
                    resources.push(DiscoveredResource {
                        kind: classify(workspace, &path).unwrap_or(ResourceKind::SelectedFile),
                        owner: ownership.owner_of(&path)?,
                        path,
                    });
                }
            }
        }
    }

    resources.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(resources)
}

fn discover_directory(
    workspace: &Workspace,
    scope: &Path,
    ownership: &Ownership<'_>,
    owner_filter: Option<&Owner>,
    resources: &mut Vec<DiscoveredResource>,
    seen: &mut HashSet<PathBuf>,
) -> Result<(), WorkflowError> {
    let scope = fs::canonicalize(scope).map_err(|error| {
        WorkflowError::Discovery(format!(
            "cannot inspect directory '{}': {error}",
            scope.display()
        ))
    })?;
    if !scope.starts_with(&workspace.root) {
        return Err(WorkflowError::Discovery(format!(
            "discovery root '{}' escapes repository '{}'",
            scope.display(),
            workspace.root.display()
        )));
    }

    // Always anchor the walker at the repository root so target/component
    // discovery inherits the repository's root .gitignore as well as nested
    // ignore files. Global and per-clone .git/info/exclude state stay disabled.
    let mut builder = WalkBuilder::new(&workspace.root);
    builder
        .hidden(false)
        .ignore(false)
        .parents(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .follow_links(false);
    let filtered_scope = scope.clone();
    builder.filter_entry(move |entry| {
        entry.file_name() != ".git"
            && (entry.path().starts_with(&filtered_scope)
                || filtered_scope.starts_with(entry.path()))
    });

    for entry in builder.build() {
        let entry = entry.map_err(|error| {
            WorkflowError::Discovery(format!(
                "cannot walk discovery root '{}': {error}",
                scope.display()
            ))
        })?;
        let file_type = entry.file_type();
        if !file_type.is_some_and(|file_type| file_type.is_file()) {
            continue;
        }
        if !entry.path().starts_with(&scope) {
            continue;
        }
        let path = fs::canonicalize(entry.path()).map_err(|error| {
            WorkflowError::Discovery(format!(
                "cannot resolve discovered path '{}': {error}",
                entry.path().display()
            ))
        })?;
        if !path.starts_with(&workspace.root) {
            return Err(WorkflowError::Discovery(format!(
                "discovered path '{}' escapes repository '{}'",
                path.display(),
                workspace.root.display()
            )));
        }
        let Some(kind) = classify(workspace, &path) else {
            continue;
        };
        let owner = ownership.owner_of(&path)?;
        if owner_filter.is_some_and(|expected| expected != &owner) {
            continue;
        }
        if seen.insert(path.clone()) {
            resources.push(DiscoveredResource { path, kind, owner });
        }
    }
    Ok(())
}

fn classify(workspace: &Workspace, path: &Path) -> Option<ResourceKind> {
    if path.starts_with(&workspace.config.system.specs) {
        return Some(ResourceKind::Specification);
    }
    let normal_kind = match path.file_name().and_then(|name| name.to_str()) {
        Some("mix.exs") => Some(ResourceKind::MixManifest),
        Some("package.json") => Some(ResourceKind::NodeManifest),
        Some("Cargo.toml") => Some(ResourceKind::CargoManifest),
        Some(
            ".formatter.exs"
            | ".credo.exs"
            | ".sobelow-conf"
            | ".ex_dna.exs"
            | ".reach.exs"
            | "coveralls.json"
            | "oxlint.config.js"
            | "oxlint.config.cjs"
            | "oxlint.config.mjs"
            | "oxlint.config.ts"
            | "stylelint.config.js"
            | "stylelint.config.cjs"
            | "stylelint.config.mjs"
            | "stylelint.config.ts"
            | "dylint.toml",
        ) => Some(ResourceKind::Configuration),
        Some("nextest.toml")
            if path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == ".config") =>
        {
            Some(ResourceKind::Configuration)
        }
        _ if path.extension().is_some_and(|extension| extension == "tla") => {
            Some(ResourceKind::Specification)
        }
        _ => None,
    };
    if path.starts_with(&workspace.config.system.tests) {
        return normal_kind.or_else(|| {
            is_oxfmt_source(path)
                .then_some(ResourceKind::SelectedFile)
                .or(Some(ResourceKind::ApplicationTest))
        });
    }
    normal_kind
}

/// Returns whether `owner` represents a repository-level resource.
#[must_use]
pub const fn is_repository_resource(owner: &Owner) -> bool {
    matches!(
        owner,
        Owner::Repository(
            SystemResource::Repository
                | SystemResource::Specifications
                | SystemResource::ApplicationTests
        )
    )
}
