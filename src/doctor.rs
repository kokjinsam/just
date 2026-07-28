use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::capability::{Capability, dylint_declaration};
use crate::discovery::{ResourceKind, discover_resources};
use crate::error::WorkflowError;
use crate::formatting::find_program;
use crate::ownership::{Owner, SystemResource};
use crate::report::{Report, ReportStatus, StepReport, StepStatus};
use crate::selector::Selection;
use crate::workspace::Workspace;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Check {
    provider: String,
    owner: Owner,
    root: PathBuf,
    evidence: PathBuf,
    configuration_files: Vec<PathBuf>,
    tools: Vec<OsString>,
    tla_jar: bool,
}

pub fn inspect(workspace: &Workspace, started_at: String) -> Result<Report, WorkflowError> {
    let started = Instant::now();
    let resources = discover_resources(workspace, &[Selection::Repository])?;
    let mut checks = Vec::new();
    let configurations = resources
        .iter()
        .filter(|resource| resource.kind == ResourceKind::Configuration)
        .map(|resource| resource.path.clone())
        .collect::<Vec<_>>();
    let mut findings = vec![step_report(
        workspace,
        "configuration",
        &Owner::Repository(SystemResource::Repository),
        &workspace.root,
        vec![workspace.root.join("just.yaml")],
        vec![workspace.root.join("just.yaml")],
        StepStatus::Passed,
        None,
        String::new(),
    )];

    for resource in &resources {
        let root = resource
            .path
            .parent()
            .unwrap_or(Path::new(""))
            .to_path_buf();
        match resource.kind {
            ResourceKind::MixManifest => {
                findings.push(manifest_report(workspace, resource, "mix-manifest"));
                checks.push(check(resource, "mix-format", "mix", Vec::new()));
                checks.push(check(resource, "dialyzer", "mix", Vec::new()));
                checks.push(check(resource, "mix-compile", "mix", Vec::new()));
                for (file, provider) in [
                    (".credo.exs", "credo"),
                    (".sobelow-conf", "sobelow"),
                    (".ex_dna.exs", "ex-dna"),
                    (".reach.exs", "reach"),
                ] {
                    let evidence = root.join(file);
                    if evidence.is_file() {
                        checks.push(check(resource, provider, "mix", vec![evidence]));
                    }
                }
            }
            ResourceKind::NodeManifest => {
                findings.push(manifest_report(workspace, resource, "node-manifest"));
                let package = read_package(&resource.path)?;
                let has = |name: &str| {
                    ["devDependencies", "dependencies"].iter().any(|field| {
                        package
                            .get(field)
                            .and_then(|dependencies| dependencies.get(name))
                            .is_some()
                    })
                };
                if has("oxfmt") {
                    checks.push(check(resource, "oxfmt", "pnpm", Vec::new()));
                }
                if has("oxlint") {
                    checks.push(check(
                        resource,
                        "oxlint",
                        "pnpm",
                        matching_configs(&root, &configurations, "oxlint.config."),
                    ));
                }
                if has("stylelint") {
                    let configs = matching_configs(&root, &configurations, "stylelint.config.");
                    if !configs.is_empty() {
                        checks.push(check(resource, "stylelint", "pnpm", configs));
                    }
                }
                for (script, provider) in [
                    ("typecheck", "package-typecheck"),
                    ("build", "package-build"),
                ] {
                    if package
                        .get("scripts")
                        .and_then(|scripts| scripts.get(script))
                        .is_some()
                    {
                        checks.push(check(resource, provider, "pnpm", Vec::new()));
                    } else {
                        findings.push(skipped_script_report(workspace, resource, provider, script));
                    }
                }
            }
            ResourceKind::CargoManifest => {
                findings.push(manifest_report(workspace, resource, "cargo-manifest"));
                checks.push(check_with_tools(
                    resource,
                    "rustfmt",
                    &["cargo", "cargo-fmt"],
                    Vec::new(),
                ));
                checks.push(check_with_tools(
                    resource,
                    "clippy",
                    &["cargo", "cargo-clippy"],
                    Vec::new(),
                ));
                checks.push(check(resource, "cargo-check", "cargo", Vec::new()));
            }
            ResourceKind::Specification
                if resource
                    .path
                    .extension()
                    .is_some_and(|extension| extension == "tla") =>
            {
                checks.push(Check {
                    provider: "tla-format".into(),
                    owner: resource.owner.clone(),
                    root,
                    evidence: resource.path.clone(),
                    configuration_files: Vec::new(),
                    tools: vec![OsString::from("java")],
                    tla_jar: true,
                });
                if resource
                    .path
                    .starts_with(workspace.config.system.specs.join("tla"))
                {
                    checks.push(Check {
                        provider: "apalache".into(),
                        owner: resource.owner.clone(),
                        root: workspace.config.system.specs.join("tla"),
                        evidence: resource.path.clone(),
                        configuration_files: {
                            let mut files = Vec::new();
                            let cfg = resource.path.with_extension("cfg");
                            if cfg.is_file() {
                                files.push(cfg);
                            }
                            files.extend(specification_configurations(
                                &resource.path,
                                &workspace.config.system.specs.join("tla"),
                                &[".apalache.cfg"],
                            ));
                            files
                        },
                        tools: vec![OsString::from("apalache-mc")],
                        tla_jar: false,
                    });
                }
            }
            ResourceKind::Specification
                if resource
                    .path
                    .starts_with(workspace.config.system.specs.join("alloy"))
                    && resource
                        .path
                        .extension()
                        .is_some_and(|extension| extension == "als") =>
            {
                checks.push(Check {
                    provider: "alloy".into(),
                    owner: resource.owner.clone(),
                    root: workspace.config.system.specs.join("alloy"),
                    evidence: resource.path.clone(),
                    configuration_files: Vec::new(),
                    tools: vec![OsString::from("alloy")],
                    tla_jar: false,
                });
            }
            ResourceKind::Specification
                if resource
                    .path
                    .starts_with(workspace.config.system.specs.join("openapi"))
                    && resource
                        .path
                        .file_name()
                        .is_none_or(|name| name != "vacuum.conf.yaml")
                    && matches!(
                        resource
                            .path
                            .extension()
                            .and_then(|extension| extension.to_str()),
                        Some("yaml" | "yml" | "json")
                    ) =>
            {
                let configurations = specification_configurations(
                    &resource.path,
                    &workspace.config.system.specs.join("openapi"),
                    &["vacuum.conf.yaml"],
                );
                checks.push(Check {
                    provider: "oasdiff".into(),
                    owner: resource.owner.clone(),
                    root: workspace.config.system.specs.join("openapi"),
                    evidence: resource.path.clone(),
                    configuration_files: Vec::new(),
                    tools: vec![OsString::from("oasdiff")],
                    tla_jar: false,
                });
                checks.push(Check {
                    provider: "vacuum".into(),
                    owner: resource.owner.clone(),
                    root: workspace.config.system.specs.join("openapi"),
                    evidence: resource.path.clone(),
                    configuration_files: configurations,
                    tools: vec![OsString::from("vacuum")],
                    tla_jar: false,
                });
            }
            ResourceKind::Configuration
            | ResourceKind::Specification
            | ResourceKind::ApplicationTest
            | ResourceKind::SelectedFile => {}
        }
    }

    checks.sort_by(|left, right| {
        left.root
            .cmp(&right.root)
            .then_with(|| left.provider.cmp(&right.provider))
    });
    checks.dedup_by(|left, right| {
        left.root == right.root
            && left.provider == right.provider
            && (!matches!(
                left.provider.as_str(),
                "apalache" | "alloy" | "oasdiff" | "vacuum"
            ) || left.evidence == right.evidence)
    });
    let mut reports = findings;
    reports.extend(
        checks
            .into_iter()
            .map(|check| ready_report(workspace, check)),
    );
    let cargo_manifests = resources
        .iter()
        .filter(|resource| resource.kind == ResourceKind::CargoManifest);
    for manifest in cargo_manifests {
        let root = manifest.path.parent().unwrap_or(Path::new(""));
        let configs = configurations
            .iter()
            .filter(|path| path.parent() == Some(root))
            .cloned()
            .collect::<Vec<_>>();
        reports.push(match dylint_declaration(&manifest.path, &configs) {
            Ok(Some(mut declaration_files)) => {
                if !declaration_files.contains(&manifest.path) {
                    declaration_files.insert(0, manifest.path.clone());
                }
                ready_report(
                    workspace,
                    Check {
                        provider: "dylint".into(),
                        owner: manifest.owner.clone(),
                        root: root.to_path_buf(),
                        evidence: manifest.path.clone(),
                        configuration_files: declaration_files,
                        tools: vec![
                            OsString::from("cargo"),
                            OsString::from("cargo-dylint"),
                            OsString::from("dylint-link"),
                        ],
                        tla_jar: false,
                    },
                )
            }
            Ok(None) => skipped_dylint_report(workspace, manifest),
            Err(error) => failed_dylint_report(workspace, manifest, error.to_string()),
        });
    }
    reports.sort_by(|left, right| {
        left.cwd
            .cmp(&right.cwd)
            .then_with(|| left.provider.cmp(&right.provider))
    });
    let status = if reports
        .iter()
        .any(|report| report.status == StepStatus::Failed)
    {
        ReportStatus::Failed
    } else {
        ReportStatus::Passed
    };
    Ok(Report {
        schema_version: 1,
        command: "doctor".into(),
        selectors: Vec::new(),
        started_at,
        duration: started.elapsed().as_millis() as u64,
        status,
        steps: reports,
    })
}

fn specification_configurations(path: &Path, root: &Path, names: &[&str]) -> Vec<PathBuf> {
    let mut current = path.parent();
    while let Some(directory) = current {
        let found = names
            .iter()
            .map(|name| directory.join(name))
            .filter(|candidate| candidate.is_file())
            .collect::<Vec<_>>();
        if !found.is_empty() {
            return found;
        }
        if directory == root {
            break;
        }
        current = directory.parent();
    }
    Vec::new()
}

fn manifest_report(
    workspace: &Workspace,
    resource: &crate::discovery::DiscoveredResource,
    provider: &str,
) -> StepReport {
    step_report(
        workspace,
        provider,
        &resource.owner,
        resource.path.parent().unwrap_or(Path::new("")),
        vec![resource.path.clone()],
        Vec::new(),
        StepStatus::Passed,
        None,
        String::new(),
    )
}

fn check(
    resource: &crate::discovery::DiscoveredResource,
    provider: &str,
    tool: &str,
    configuration_files: Vec<PathBuf>,
) -> Check {
    Check {
        provider: provider.into(),
        owner: resource.owner.clone(),
        root: resource
            .path
            .parent()
            .unwrap_or(Path::new(""))
            .to_path_buf(),
        evidence: resource.path.clone(),
        configuration_files,
        tools: vec![OsString::from(tool)],
        tla_jar: false,
    }
}

fn check_with_tools(
    resource: &crate::discovery::DiscoveredResource,
    provider: &str,
    tools: &[&str],
    configuration_files: Vec<PathBuf>,
) -> Check {
    Check {
        provider: provider.into(),
        owner: resource.owner.clone(),
        root: resource
            .path
            .parent()
            .unwrap_or(Path::new(""))
            .to_path_buf(),
        evidence: resource.path.clone(),
        configuration_files,
        tools: tools.iter().map(OsString::from).collect(),
        tla_jar: false,
    }
}

fn matching_configs(root: &Path, configurations: &[PathBuf], prefix: &str) -> Vec<PathBuf> {
    configurations
        .iter()
        .filter(|path| {
            path.parent() == Some(root)
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(prefix))
        })
        .cloned()
        .collect()
}

fn read_package(path: &Path) -> Result<serde_json::Value, WorkflowError> {
    let source = fs::read_to_string(path).map_err(|error| {
        WorkflowError::Discovery(format!(
            "cannot read Node manifest '{}': {error}",
            path.display()
        ))
    })?;
    serde_json::from_str(&source).map_err(|error| {
        WorkflowError::Discovery(format!(
            "cannot parse Node manifest '{}': {error}",
            path.display()
        ))
    })
}

fn ready_report(workspace: &Workspace, check: Check) -> StepReport {
    let missing = check
        .tools
        .iter()
        .filter(|tool| find_program(tool).is_none())
        .map(|tool| tool.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let jar_error = check.tla_jar.then(|| match env::var_os("TLA2TOOLS_JAR") {
        Some(path) if Path::new(&path).is_file() => None,
        Some(path) => Some(format!(
            "TLA2TOOLS_JAR '{}' is not accessible",
            Path::new(&path).display()
        )),
        None => Some("TLA2TOOLS_JAR is not set".into()),
    });
    let mut diagnostics = Vec::new();
    if !missing.is_empty() {
        diagnostics.push(format!("missing executable(s): {}", missing.join(", ")));
    }
    if let Some(Some(error)) = jar_error {
        diagnostics.push(error);
    }
    step_report(
        workspace,
        &check.provider,
        &check.owner,
        &check.root,
        vec![check.evidence],
        check.configuration_files,
        if diagnostics.is_empty() {
            StepStatus::Passed
        } else {
            StepStatus::Failed
        },
        None,
        diagnostics.join("; "),
    )
}

fn skipped_dylint_report(
    workspace: &Workspace,
    manifest: &crate::discovery::DiscoveredResource,
) -> StepReport {
    step_report(
        workspace,
        "dylint",
        &manifest.owner,
        manifest.path.parent().unwrap_or(Path::new("")),
        vec![manifest.path.clone()],
        Vec::new(),
        StepStatus::Skipped,
        Some("not applicable: workspace.metadata.dylint.libraries is not declared".into()),
        String::new(),
    )
}

fn skipped_script_report(
    workspace: &Workspace,
    manifest: &crate::discovery::DiscoveredResource,
    provider: &str,
    script: &str,
) -> StepReport {
    step_report(
        workspace,
        provider,
        &manifest.owner,
        manifest.path.parent().unwrap_or(Path::new("")),
        vec![manifest.path.clone()],
        Vec::new(),
        StepStatus::Skipped,
        Some(format!(
            "not applicable: package.json scripts.{script} is not declared"
        )),
        String::new(),
    )
}

fn failed_dylint_report(
    workspace: &Workspace,
    manifest: &crate::discovery::DiscoveredResource,
    diagnostic: String,
) -> StepReport {
    step_report(
        workspace,
        "dylint",
        &manifest.owner,
        manifest.path.parent().unwrap_or(Path::new("")),
        vec![manifest.path.clone()],
        Vec::new(),
        StepStatus::Failed,
        None,
        diagnostic,
    )
}

#[allow(clippy::too_many_arguments)]
fn step_report(
    workspace: &Workspace,
    provider: &str,
    owner: &Owner,
    root: &Path,
    targets: Vec<PathBuf>,
    configuration_files: Vec<PathBuf>,
    status: StepStatus,
    skip_reason: Option<String>,
    stderr: String,
) -> StepReport {
    let (target, component) = match owner {
        Owner::Target(target) => (Some(target.clone()), None),
        Owner::Component { target, component } => (Some(target.clone()), component.clone()),
        Owner::Repository(_) => (None, None),
    };
    StepReport {
        id: format!(
            "doctor:{}:{}",
            relative(workspace, root).display(),
            provider
        ),
        capability: capability_name(Capability::DoctorCheck).into(),
        provider: provider.into(),
        target,
        component,
        cwd: relative(workspace, root).to_string_lossy().into_owned(),
        argv: Vec::new(),
        configuration_files: configuration_files
            .iter()
            .map(|path| relative(workspace, path).to_string_lossy().into_owned())
            .collect(),
        targets: targets
            .iter()
            .map(|path| relative(workspace, path).to_string_lossy().into_owned())
            .collect(),
        status,
        exit_code: None,
        duration: 0,
        skip_reason,
        stdout: String::new(),
        stderr,
    }
}

fn relative<'a>(workspace: &'a Workspace, path: &'a Path) -> &'a Path {
    path.strip_prefix(&workspace.root).unwrap_or(path)
}

fn capability_name(capability: Capability) -> &'static str {
    match capability {
        Capability::DoctorCheck => "doctor-check",
        _ => unreachable!("doctor only renders doctor checks"),
    }
}
