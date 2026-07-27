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
    FormatCheck,
    Lint,
    Typecheck,
    Compile,
    TargetTest,
    SpecCheck,
    ApplicationTest,
    DoctorCheck,
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
    pub evidence: PathBuf,
    pub configuration_files: Vec<PathBuf>,
}

pub fn bindings_for_selections(
    workspace: &Workspace,
    selections: &[Selection],
) -> Result<Vec<CapabilityBinding>, WorkflowError> {
    let resources = resources_for_selections(workspace, selections)?;
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

pub(crate) fn resources_for_selections(
    workspace: &Workspace,
    selections: &[Selection],
) -> Result<Vec<DiscoveredResource>, WorkflowError> {
    let mut resources = discover_resources(workspace, selections)?;
    let mut resource_paths = resources
        .iter()
        .map(|resource| resource.path.clone())
        .collect::<HashSet<_>>();
    add_explicit_ancestor_manifests(workspace, selections, &mut resources, &mut resource_paths)?;
    resources.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(resources)
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
            "oxlint.config.js",
            "oxlint.config.cjs",
            "oxlint.config.mjs",
            "oxlint.config.ts",
            "stylelint.config.js",
            "stylelint.config.cjs",
            "stylelint.config.mjs",
            "stylelint.config.ts",
        ],
        ResourceKind::CargoManifest => &[".config/nextest.toml", "dylint.toml"],
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
        Some(name)
            if name.starts_with("oxlint.config.") || name.starts_with("stylelint.config.") =>
        {
            Some(ResourceKind::NodeManifest)
        }
        Some("dylint.toml") => Some(ResourceKind::CargoManifest),
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
    if is_oxfmt_source(path) {
        return Some(ResourceKind::NodeManifest);
    }
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("ex" | "exs" | "heex") => Some(ResourceKind::MixManifest),
        Some("rs") => Some(ResourceKind::CargoManifest),
        _ => None,
    }
}

#[must_use]
pub(crate) fn is_oxfmt_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" | "css" | "scss")
    )
}

fn bindings_for_resource(
    resource: &DiscoveredResource,
    resources: &[DiscoveredResource],
) -> Result<Vec<CapabilityBinding>, WorkflowError> {
    match resource.kind {
        ResourceKind::MixManifest => Ok(mix_bindings(resource, resources)),
        ResourceKind::NodeManifest => node_bindings(resource, resources),
        ResourceKind::CargoManifest => rust_bindings(resource, resources),
        ResourceKind::Specification if is_tla_file(&resource.path) => Ok(vec![
            direct_binding(resource, Capability::Format, "tla-format"),
            direct_binding(resource, Capability::FormatCheck, "tla-format"),
        ]),
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
            Capability::FormatCheck,
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
        bindings.push(binding(
            resource,
            &root,
            Capability::FormatCheck,
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
            "oxlint.config.",
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
) -> Result<Vec<CapabilityBinding>, WorkflowError> {
    let root = resource
        .path
        .parent()
        .unwrap_or(Path::new(""))
        .to_path_buf();
    let configs = configurations_for_manifest(resource, resources);
    let mut bindings = vec![
        binding(resource, &root, Capability::Format, "rustfmt", &configs, ""),
        binding(
            resource,
            &root,
            Capability::FormatCheck,
            "rustfmt",
            &configs,
            "",
        ),
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
    ];
    if let Some(declarations) = dylint_declaration(&resource.path, &configs)? {
        let mut dylint = binding(
            resource,
            &root,
            Capability::Lint,
            "dylint",
            &configs,
            "dylint.toml",
        );
        dylint.configuration_files = declarations;
        dylint.configuration_files.extend(
            configs
                .iter()
                .filter(|path| path.file_name().is_some_and(|name| name == "dylint.toml"))
                .cloned(),
        );
        dylint.configuration_files.sort();
        dylint.configuration_files.dedup();
        bindings.push(dylint);
    }
    Ok(bindings)
}

pub fn dylint_declaration(
    cargo_manifest: &Path,
    configurations: &[PathBuf],
) -> Result<Option<Vec<PathBuf>>, WorkflowError> {
    let dylint_configs = configurations
        .iter()
        .filter(|path| path.file_name().is_some_and(|name| name == "dylint.toml"));
    let mut declarations = Vec::new();
    for path in std::iter::once(cargo_manifest).chain(dylint_configs.map(PathBuf::as_path)) {
        let source = fs::read_to_string(path).map_err(|error| {
            WorkflowError::Discovery(format!(
                "cannot read Dylint configuration '{}': {error}",
                path.display()
            ))
        })?;
        let document: toml::Value = toml::from_str(&source).map_err(|error| {
            WorkflowError::Discovery(format!(
                "cannot parse Dylint configuration '{}': {error}",
                path.display()
            ))
        })?;
        let Some(dylint) = document
            .get("workspace")
            .and_then(|value| value.get("metadata"))
            .and_then(|value| value.get("dylint"))
        else {
            continue;
        };
        let Some(libraries) = dylint.get("libraries") else {
            return Err(WorkflowError::Discovery(format!(
                "Dylint declaration in '{}' is incomplete: \
                 workspace.metadata.dylint.libraries is required",
                path.display()
            )));
        };
        let libraries = libraries.as_array().ok_or_else(|| {
            WorkflowError::Discovery(format!(
                "Dylint declaration in '{}' is malformed: \
                 workspace.metadata.dylint.libraries must be an array",
                path.display()
            ))
        })?;
        if libraries.is_empty() {
            return Err(WorkflowError::Discovery(format!(
                "Dylint declaration in '{}' is empty: \
                 workspace.metadata.dylint.libraries must contain at least one library",
                path.display()
            )));
        }
        for (index, library) in libraries.iter().enumerate() {
            validate_dylint_library(path, index, library)?;
        }
        declarations.push(path.to_path_buf());
    }
    Ok((!declarations.is_empty()).then_some(declarations))
}

fn validate_dylint_library(
    path: &Path,
    index: usize,
    library: &toml::Value,
) -> Result<(), WorkflowError> {
    let table = library.as_table().ok_or_else(|| {
        WorkflowError::Discovery(format!(
            "Dylint declaration in '{}' is malformed: library {} must be a Cargo-style table",
            path.display(),
            index + 1
        ))
    })?;
    let sources = ["git", "path"]
        .into_iter()
        .filter(|key| table.contains_key(*key))
        .collect::<Vec<_>>();
    if sources.len() != 1
        || table
            .get(sources.first().copied().unwrap_or(""))
            .and_then(toml::Value::as_str)
            .is_none_or(|value| value.trim().is_empty())
    {
        return Err(WorkflowError::Discovery(format!(
            "Dylint declaration in '{}' is incomplete: library {} must have exactly one \
             non-empty 'git' or 'path' source",
            path.display(),
            index + 1
        )));
    }
    let revisions = ["branch", "tag", "rev"]
        .into_iter()
        .filter(|key| table.contains_key(*key))
        .collect::<Vec<_>>();
    if revisions.len() > 1
        || revisions.iter().any(|key| {
            table
                .get(*key)
                .and_then(toml::Value::as_str)
                .is_none_or(|value| value.trim().is_empty())
        })
        || (sources == ["path"] && !revisions.is_empty())
    {
        return Err(WorkflowError::Discovery(format!(
            "Dylint declaration in '{}' is malformed: library {} may use one non-empty \
             'branch', 'tag', or 'rev' option with a 'git' source",
            path.display(),
            index + 1
        )));
    }
    if let Some(pattern) = table.get("pattern") {
        let valid = pattern
            .as_str()
            .is_some_and(|value| !value.trim().is_empty())
            || pattern.as_array().is_some_and(|values| {
                !values.is_empty()
                    && values
                        .iter()
                        .all(|value| value.as_str().is_some_and(|value| !value.trim().is_empty()))
            });
        if !valid {
            return Err(WorkflowError::Discovery(format!(
                "Dylint declaration in '{}' is malformed: library {} pattern must be a \
                 non-empty string or string array",
                path.display(),
                index + 1
            )));
        }
    }
    Ok(())
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
        evidence: resource.path.clone(),
        configuration_files,
    }
}

fn direct_binding(
    resource: &DiscoveredResource,
    capability: Capability,
    provider: &str,
) -> CapabilityBinding {
    CapabilityBinding {
        capability,
        provider: ProviderId::new(provider),
        owner: resource.owner.clone(),
        root: resource
            .path
            .parent()
            .unwrap_or(Path::new(""))
            .to_path_buf(),
        evidence: resource.path.clone(),
        configuration_files: Vec::new(),
    }
}

fn is_tla_file(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "tla")
}
