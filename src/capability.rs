use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::discovery::{DiscoveredResource, ResourceKind, discover_resources};
use crate::error::WorkflowError;
use crate::ownership::{Owner, Ownership};
use crate::selector::Selection;
use crate::workspace::Workspace;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Capability {
    Format,
    Lint,
    Typecheck,
    Compile,
    TargetTest,
    SpecCheck,
    ApplicationTest,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProviderId(String);

impl ProviderId {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CapabilityBinding {
    pub capability: Capability,
    pub provider: ProviderId,
    pub owner: Owner,
    pub root: PathBuf,
    pub manifest: PathBuf,
    pub configuration_files: Vec<PathBuf>,
}

pub fn bindings_for_selections(
    workspace: &Workspace,
    selections: &[Selection],
) -> Result<Vec<CapabilityBinding>, WorkflowError> {
    let mut resources = discover_resources(workspace, selections)?;
    let mut resource_paths = resources
        .iter()
        .map(|resource| resource.path.clone())
        .collect::<HashSet<_>>();
    add_explicit_ancestor_manifests(workspace, selections, &mut resources, &mut resource_paths)?;
    resources.sort_by(|left, right| left.path.cmp(&right.path));

    let mut bindings = Vec::new();
    let mut seen = HashSet::new();
    let ownership = Ownership::new(&workspace.config);

    for resource in &resources {
        if !selections.iter().any(|selection| {
            selection_includes_manifest(selection, resource, &resources, &ownership)
        }) {
            continue;
        }
        for binding in bindings_for_resource(resource, &resources)? {
            if seen.insert(binding.clone()) {
                bindings.push(binding);
            }
        }
    }
    Ok(bindings)
}

fn add_explicit_ancestor_manifests(
    workspace: &Workspace,
    selections: &[Selection],
    resources: &mut Vec<DiscoveredResource>,
    resource_paths: &mut HashSet<PathBuf>,
) -> Result<(), WorkflowError> {
    let ownership = Ownership::new(&workspace.config);
    for path in selections.iter().filter_map(|selection| match selection {
        Selection::Path(path) => Some(path),
        _ => None,
    }) {
        let mut ancestor = if path.is_dir() {
            Some(path.as_path())
        } else {
            path.parent()
        };
        while let Some(root) = ancestor {
            for (name, kind) in [
                ("mix.exs", ResourceKind::MixManifest),
                ("package.json", ResourceKind::NodeManifest),
                ("Cargo.toml", ResourceKind::CargoManifest),
            ] {
                let candidate = root.join(name);
                if !candidate.is_file() {
                    continue;
                }
                let candidate = fs::canonicalize(&candidate).map_err(|error| {
                    WorkflowError::Discovery(format!(
                        "cannot resolve ancestor manifest '{}': {error}",
                        candidate.display()
                    ))
                })?;
                if resource_paths.insert(candidate.clone()) {
                    resources.push(DiscoveredResource {
                        owner: ownership.owner_of(&candidate)?,
                        path: candidate.clone(),
                        kind,
                    });
                }
                add_direct_configuration_evidence(
                    &candidate,
                    kind,
                    &ownership,
                    resources,
                    resource_paths,
                )?;
            }
            if root == workspace.root {
                break;
            }
            ancestor = root.parent();
        }
    }
    Ok(())
}

fn add_direct_configuration_evidence(
    manifest: &Path,
    kind: ResourceKind,
    ownership: &Ownership<'_>,
    resources: &mut Vec<DiscoveredResource>,
    resource_paths: &mut HashSet<PathBuf>,
) -> Result<(), WorkflowError> {
    let root = manifest.parent().unwrap_or(Path::new(""));
    let names: &[&str] = match kind {
        ResourceKind::MixManifest => &[
            ".formatter.exs",
            ".credo.exs",
            ".sobelow-conf",
            ".ex_dna.exs",
            ".reach.exs",
        ],
        ResourceKind::NodeManifest => &[
            "stylelint.config.js",
            "stylelint.config.cjs",
            "stylelint.config.mjs",
            "stylelint.config.ts",
        ],
        ResourceKind::CargoManifest => &[".config/nextest.toml"],
        _ => &[],
    };
    for name in names {
        let path = root.join(name);
        if !path.is_file() {
            continue;
        }
        let path = fs::canonicalize(&path).map_err(|error| {
            WorkflowError::Discovery(format!(
                "cannot resolve configuration evidence '{}': {error}",
                path.display()
            ))
        })?;
        if resource_paths.insert(path.clone()) {
            resources.push(DiscoveredResource {
                owner: ownership.owner_of(&path)?,
                path,
                kind: ResourceKind::Configuration,
            });
        }
    }
    Ok(())
}

fn selection_includes_manifest(
    selection: &Selection,
    manifest: &DiscoveredResource,
    resources: &[DiscoveredResource],
    ownership: &Ownership<'_>,
) -> bool {
    match selection {
        Selection::Repository => true,
        Selection::Target(selected) => match &manifest.owner {
            Owner::Target(candidate)
            | Owner::Component {
                target: candidate, ..
            } => selected == candidate,
            Owner::Repository(_) => false,
        },
        Selection::Component { target, component } => {
            manifest.owner
                == (Owner::Component {
                    target: target.clone(),
                    component: Some(component.clone()),
                })
        }
        Selection::Path(path) if path.is_dir() => {
            manifest.path.starts_with(path)
                || manifest.path.parent().is_some_and(|root| {
                    path.starts_with(root)
                        && ownership
                            .owner_of(path)
                            .is_ok_and(|owner| owner == manifest.owner)
                        && nearest_manifest_for_path(path, manifest.kind, resources)
                            .is_some_and(|nearest| nearest.path == manifest.path)
                })
        }
        Selection::Path(path) => file_selects_manifest(path, manifest, resources),
    }
}

fn file_selects_manifest(
    path: &Path,
    manifest: &DiscoveredResource,
    resources: &[DiscoveredResource],
) -> bool {
    if manifest.path == path {
        return true;
    }
    let Some(selected) = resources.iter().find(|resource| resource.path == path) else {
        return false;
    };
    let applicable_kind = match selected.kind {
        ResourceKind::Configuration => configuration_manifest_kind(path),
        ResourceKind::SelectedFile => source_manifest_kind(path),
        ResourceKind::MixManifest
        | ResourceKind::NodeManifest
        | ResourceKind::CargoManifest
        | ResourceKind::Specification
        | ResourceKind::ApplicationTest => None,
    };
    applicable_kind == Some(manifest.kind)
        && nearest_manifest_for_path(path, manifest.kind, resources)
            .is_some_and(|nearest| nearest.path == manifest.path)
}

fn configuration_manifest_kind(path: &Path) -> Option<ResourceKind> {
    match path.file_name().and_then(|name| name.to_str()) {
        Some(".formatter.exs" | ".credo.exs" | ".sobelow-conf" | ".ex_dna.exs" | ".reach.exs") => {
            Some(ResourceKind::MixManifest)
        }
        Some(name) if name.starts_with("stylelint.config.") => Some(ResourceKind::NodeManifest),
        Some("nextest.toml")
            if path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == ".config") =>
        {
            Some(ResourceKind::CargoManifest)
        }
        _ => None,
    }
}

fn source_manifest_kind(path: &Path) -> Option<ResourceKind> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("ex" | "exs") => Some(ResourceKind::MixManifest),
        Some("js" | "jsx" | "ts" | "tsx" | "css" | "scss") => Some(ResourceKind::NodeManifest),
        Some("rs") => Some(ResourceKind::CargoManifest),
        _ => None,
    }
}

fn bindings_for_resource(
    resource: &DiscoveredResource,
    resources: &[DiscoveredResource],
) -> Result<Vec<CapabilityBinding>, WorkflowError> {
    match resource.kind {
        ResourceKind::MixManifest => Ok(mix_bindings(resource, resources)),
        ResourceKind::NodeManifest => node_bindings(resource, resources),
        ResourceKind::CargoManifest => Ok(rust_bindings(resource, resources)),
        ResourceKind::Configuration
        | ResourceKind::Specification
        | ResourceKind::ApplicationTest
        | ResourceKind::SelectedFile => Ok(Vec::new()),
    }
}

fn mix_bindings(
    resource: &DiscoveredResource,
    resources: &[DiscoveredResource],
) -> Vec<CapabilityBinding> {
    let root = resource
        .path
        .parent()
        .unwrap_or(Path::new(""))
        .to_path_buf();
    let configs = configurations_for_manifest(resource, resources);
    let mut bindings = vec![
        binding(
            resource,
            &root,
            Capability::Format,
            "mix-format",
            &configs,
            ".formatter.exs",
        ),
        binding(
            resource,
            &root,
            Capability::Compile,
            "mix-compile",
            &configs,
            "",
        ),
        binding(
            resource,
            &root,
            Capability::TargetTest,
            "exunit",
            &configs,
            "",
        ),
    ];
    for (file, provider) in [
        (".credo.exs", "credo"),
        (".sobelow-conf", "sobelow"),
        (".ex_dna.exs", "ex-dna"),
        (".reach.exs", "reach"),
    ] {
        if configs
            .iter()
            .any(|path| path.file_name().is_some_and(|name| name == file))
        {
            bindings.push(binding(
                resource,
                &root,
                Capability::Lint,
                provider,
                &configs,
                file,
            ));
        }
    }
    bindings
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PackageManifest {
    scripts: std::collections::BTreeMap<String, String>,
    #[serde(rename = "devDependencies")]
    dev_dependencies: std::collections::BTreeMap<String, String>,
    dependencies: std::collections::BTreeMap<String, String>,
}

fn node_bindings(
    resource: &DiscoveredResource,
    resources: &[DiscoveredResource],
) -> Result<Vec<CapabilityBinding>, WorkflowError> {
    let source = fs::read_to_string(&resource.path).map_err(|error| {
        WorkflowError::Discovery(format!(
            "cannot read Node manifest '{}': {error}",
            resource.path.display()
        ))
    })?;
    let package: PackageManifest = serde_json::from_str(&source).map_err(|error| {
        WorkflowError::Discovery(format!(
            "cannot parse Node manifest '{}': {error}",
            resource.path.display()
        ))
    })?;
    let root = resource
        .path
        .parent()
        .unwrap_or(Path::new(""))
        .to_path_buf();
    let configs = configurations_for_manifest(resource, resources);
    let has_dependency = |name: &str| {
        package.dev_dependencies.contains_key(name) || package.dependencies.contains_key(name)
    };
    let mut bindings = Vec::new();
    if has_dependency("oxfmt") {
        bindings.push(binding(
            resource,
            &root,
            Capability::Format,
            "oxfmt",
            &configs,
            "",
        ));
    }
    if has_dependency("oxlint") {
        bindings.push(binding(
            resource,
            &root,
            Capability::Lint,
            "oxlint",
            &configs,
            "",
        ));
    }
    if has_dependency("stylelint")
        && configs.iter().any(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("stylelint.config."))
        })
    {
        bindings.push(binding(
            resource,
            &root,
            Capability::Lint,
            "stylelint",
            &configs,
            "stylelint.config.",
        ));
    }
    for (script, capability, provider) in [
        ("build", Capability::Compile, "package-build"),
        ("test", Capability::TargetTest, "package-test"),
        ("typecheck", Capability::Typecheck, "package-typecheck"),
    ] {
        if package.scripts.contains_key(script) {
            bindings.push(binding(resource, &root, capability, provider, &configs, ""));
        }
    }
    Ok(bindings)
}

fn rust_bindings(
    resource: &DiscoveredResource,
    resources: &[DiscoveredResource],
) -> Vec<CapabilityBinding> {
    let root = resource
        .path
        .parent()
        .unwrap_or(Path::new(""))
        .to_path_buf();
    let configs = configurations_for_manifest(resource, resources);
    vec![
        binding(resource, &root, Capability::Format, "rustfmt", &configs, ""),
        binding(resource, &root, Capability::Lint, "clippy", &configs, ""),
        binding(
            resource,
            &root,
            Capability::Compile,
            "cargo-check",
            &configs,
            "",
        ),
        binding(
            resource,
            &root,
            Capability::TargetTest,
            "nextest",
            &configs,
            "nextest.toml",
        ),
    ]
}

fn configurations_for_manifest(
    manifest: &DiscoveredResource,
    resources: &[DiscoveredResource],
) -> Vec<PathBuf> {
    resources
        .iter()
        .filter(|configuration| {
            configuration.kind == ResourceKind::Configuration
                && nearest_manifest(configuration, manifest.kind, resources)
                    .is_some_and(|nearest| nearest.path == manifest.path)
        })
        .map(|resource| resource.path.clone())
        .collect()
}

fn nearest_manifest<'a>(
    configuration: &DiscoveredResource,
    manifest_kind: ResourceKind,
    resources: &'a [DiscoveredResource],
) -> Option<&'a DiscoveredResource> {
    nearest_manifest_for_path(&configuration.path, manifest_kind, resources)
}

fn nearest_manifest_for_path<'a>(
    path: &Path,
    manifest_kind: ResourceKind,
    resources: &'a [DiscoveredResource],
) -> Option<&'a DiscoveredResource> {
    resources
        .iter()
        .filter(|candidate| {
            candidate.kind == manifest_kind
                && candidate
                    .path
                    .parent()
                    .is_some_and(|root| path.starts_with(root))
        })
        .max_by_key(|candidate| {
            candidate
                .path
                .parent()
                .map_or(0, |root| root.components().count())
        })
}

fn binding(
    resource: &DiscoveredResource,
    root: &Path,
    capability: Capability,
    provider: &str,
    configs: &[PathBuf],
    evidence_name: &str,
) -> CapabilityBinding {
    let configuration_files = if evidence_name.is_empty() {
        Vec::new()
    } else {
        configs
            .iter()
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name == evidence_name
                            || (evidence_name.ends_with('.') && name.starts_with(evidence_name))
                    })
            })
            .cloned()
            .collect()
    };
    CapabilityBinding {
        capability,
        provider: ProviderId::new(provider),
        owner: resource.owner.clone(),
        root: root.to_path_buf(),
        manifest: resource.path.clone(),
        configuration_files,
    }
}
