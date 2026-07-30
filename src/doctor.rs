//! Read-only diagnostics for workflow structure and provider readiness.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::application_testing::{self, ApplicationSuite};
use crate::capability::{
    Capability, ExCoverallsReadiness, dylint_declaration, excoveralls_readiness,
};
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

/// Inspects configuration and all applicable provider requirements.
///
/// # Errors
///
/// Returns an error when repository discovery or application-test inspection
/// cannot complete.
pub fn inspect(workspace: &Workspace, started_at: String) -> Result<Report, WorkflowError> {
    let started = Instant::now();
    let resources = discover_resources(workspace, &[Selection::Repository])?;
    let checks = Vec::new();
    let configurations = resources
        .iter()
        .filter(|resource| resource.kind == ResourceKind::Configuration)
        .map(|resource| resource.path.clone())
        .collect::<Vec<_>>();
    let findings = vec![step_report(
        workspace,
        DoctorReport {
            provider: "configuration",
            owner: &Owner::Repository(SystemResource::Repository),
            root: &workspace.root,
            targets: vec![workspace.root.join("just.yaml")],
            configuration_files: vec![workspace.root.join("just.yaml")],
            status: StepStatus::Passed,
            skip_reason: None,
            stderr: String::new(),
        },
    )];
    collect_resource_diagnostics(
        workspace,
        started_at,
        started,
        &resources,
        &configurations,
        checks,
        findings,
    )
}

fn collect_resource_diagnostics(
    workspace: &Workspace,
    started_at: String,
    started: Instant,
    resources: &[crate::discovery::DiscoveredResource],
    configurations: &[PathBuf],
    mut checks: Vec<Check>,
    mut findings: Vec<StepReport>,
) -> Result<Report, WorkflowError> {
    for resource in resources {
        let root = resource
            .path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf();
        match resource.kind {
            ResourceKind::MixManifest => {
                inspect_mix(workspace, resource, &root, &mut checks, &mut findings);
            }
            ResourceKind::NodeManifest => {
                inspect_node(
                    workspace,
                    resource,
                    &root,
                    configurations,
                    &mut checks,
                    &mut findings,
                )?;
            }
            ResourceKind::CargoManifest => {
                inspect_cargo(workspace, resource, &mut checks, &mut findings);
            }
            ResourceKind::Specification => {
                inspect_specification(workspace, resource, root, &mut checks);
            }
            ResourceKind::Configuration
            | ResourceKind::ApplicationTest
            | ResourceKind::SelectedFile => {}
        }
    }
    Ok(finish_inspection(
        workspace,
        started_at,
        started,
        resources,
        configurations,
        checks,
        findings,
    ))
}

fn inspect_mix(
    workspace: &Workspace,
    resource: &crate::discovery::DiscoveredResource,
    root: &Path,
    checks: &mut Vec<Check>,
    findings: &mut Vec<StepReport>,
) {
    findings.push(manifest_report(workspace, resource, "mix-manifest"));
    checks.push(check(resource, "mix-format", "mix", Vec::new()));
    checks.push(check(resource, "dialyzer", "mix", Vec::new()));
    checks.push(check(resource, "mix-compile", "mix", Vec::new()));
    if is_target_owner(&resource.owner) {
        checks.push(check(resource, "exunit", "mix", Vec::new()));
        match excoveralls_readiness(&resource.path) {
            ExCoverallsReadiness::Ready { .. } => {
                let coveralls = root.join("coveralls.json");
                let mut configuration_files = vec![workspace.root.join("just.yaml")];
                if coveralls.is_file() {
                    configuration_files.push(coveralls);
                }
                checks.push(check(resource, "excoveralls", "mix", configuration_files));
            }
            ExCoverallsReadiness::Invalid(reason) => findings.push(failed_requirement_report(
                workspace,
                resource,
                "excoveralls",
                reason,
            )),
        }
    }
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

fn inspect_cargo(
    workspace: &Workspace,
    resource: &crate::discovery::DiscoveredResource,
    checks: &mut Vec<Check>,
    findings: &mut Vec<StepReport>,
) {
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
    if is_target_owner(&resource.owner) {
        checks.push(check_with_tools(
            resource,
            "nextest",
            &["cargo", "cargo-nextest"],
            Vec::new(),
        ));
        checks.push(check_with_tools(
            resource,
            "llvm-cov",
            &["cargo", "cargo-llvm-cov"],
            vec![workspace.root.join("just.yaml")],
        ));
    }
}

fn inspect_specification(
    workspace: &Workspace,
    resource: &crate::discovery::DiscoveredResource,
    root: PathBuf,
    checks: &mut Vec<Check>,
) {
    let specs = &workspace.config.system.specs;
    if resource
        .path
        .extension()
        .is_some_and(|extension| extension == "tla")
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
        let tla_root = specs.join("tla");
        if resource.path.starts_with(&tla_root) {
            let mut configuration_files = Vec::new();
            let cfg = resource.path.with_extension("cfg");
            if cfg.is_file() {
                configuration_files.push(cfg);
            }
            configuration_files.extend(specification_configurations(
                &resource.path,
                &tla_root,
                &[".apalache.cfg"],
            ));
            checks.push(Check {
                provider: "apalache".into(),
                owner: resource.owner.clone(),
                root: tla_root,
                evidence: resource.path.clone(),
                configuration_files,
                tools: vec![OsString::from("apalache-mc")],
                tla_jar: false,
            });
        }
        return;
    }
    let alloy_root = specs.join("alloy");
    if resource.path.starts_with(&alloy_root)
        && resource
            .path
            .extension()
            .is_some_and(|extension| extension == "als")
    {
        checks.push(Check {
            provider: "alloy".into(),
            owner: resource.owner.clone(),
            root: alloy_root,
            evidence: resource.path.clone(),
            configuration_files: Vec::new(),
            tools: vec![OsString::from("alloy")],
            tla_jar: false,
        });
        return;
    }
    inspect_openapi(workspace, resource, checks);
}

fn inspect_openapi(
    workspace: &Workspace,
    resource: &crate::discovery::DiscoveredResource,
    checks: &mut Vec<Check>,
) {
    let root = workspace.config.system.specs.join("openapi");
    if !resource.path.starts_with(&root)
        || resource
            .path
            .file_name()
            .is_some_and(|name| name == "vacuum.conf.yaml")
        || !matches!(
            resource
                .path
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("yaml" | "yml" | "json")
        )
    {
        return;
    }
    let configurations = specification_configurations(&resource.path, &root, &["vacuum.conf.yaml"]);
    for (provider, tool, configuration_files) in [
        ("oasdiff", "oasdiff", Vec::new()),
        ("vacuum", "vacuum", configurations),
    ] {
        checks.push(Check {
            provider: provider.into(),
            owner: resource.owner.clone(),
            root: root.clone(),
            evidence: resource.path.clone(),
            configuration_files,
            tools: vec![OsString::from(tool)],
            tla_jar: false,
        });
    }
}

fn inspect_node(
    workspace: &Workspace,
    resource: &crate::discovery::DiscoveredResource,
    root: &Path,
    configurations: &[PathBuf],
    checks: &mut Vec<Check>,
    findings: &mut Vec<StepReport>,
) -> Result<(), WorkflowError> {
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
            matching_configs(root, configurations, "oxlint.config."),
        ));
    }
    if has("stylelint") {
        let configs = matching_configs(root, configurations, "stylelint.config.");
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
    if is_target_owner(&resource.owner) {
        inspect_node_tests(workspace, resource, &package, &has, checks, findings);
    }
    Ok(())
}

fn inspect_node_tests(
    workspace: &Workspace,
    resource: &crate::discovery::DiscoveredResource,
    package: &serde_json::Value,
    has: &impl Fn(&str) -> bool,
    checks: &mut Vec<Check>,
    findings: &mut Vec<StepReport>,
) {
    let has_test = package
        .get("scripts")
        .and_then(|scripts| scripts.get("test"))
        .is_some();
    if !has_test {
        findings.push(skipped_script_report(
            workspace,
            resource,
            "package-test",
            "test",
        ));
        return;
    }
    checks.push(check(resource, "package-test", "pnpm", Vec::new()));
    if has("vitest") && (has("@vitest/coverage-v8") || has("@vitest/coverage-istanbul")) {
        checks.push(check(
            resource,
            "vitest-coverage",
            "pnpm",
            vec![workspace.root.join("just.yaml")],
        ));
    } else {
        findings.push(failed_requirement_report(
            workspace,
            resource,
            "vitest-coverage",
            "missing Vitest or a supported Vitest coverage adapter".into(),
        ));
    }
}

fn finish_inspection(
    workspace: &Workspace,
    started_at: String,
    started: Instant,
    resources: &[crate::discovery::DiscoveredResource],
    configurations: &[PathBuf],
    mut checks: Vec<Check>,
    findings: Vec<StepReport>,
) -> Report {
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
    reports.extend(dylint_reports(workspace, resources, configurations));
    reports.extend(application_reports(workspace));
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
    Report {
        schema_version: 1,
        command: "doctor".into(),
        selectors: Vec::new(),
        started_at,
        duration: started.elapsed().as_millis() as u64,
        status,
        steps: reports,
    }
}

fn dylint_reports(
    workspace: &Workspace,
    resources: &[crate::discovery::DiscoveredResource],
    configurations: &[PathBuf],
) -> Vec<StepReport> {
    resources
        .iter()
        .filter(|resource| resource.kind == ResourceKind::CargoManifest)
        .map(|manifest| {
            let root = manifest.path.parent().unwrap_or_else(|| Path::new(""));
            let configs = configurations
                .iter()
                .filter(|path| path.parent() == Some(root))
                .cloned()
                .collect::<Vec<_>>();
            match dylint_declaration(&manifest.path, &configs) {
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
            }
        })
        .collect()
}

fn application_reports(workspace: &Workspace) -> Vec<StepReport> {
    let mut reports = Vec::new();
    match application_testing::discover(workspace) {
        Ok(application) if application.configured_suites().is_empty() => {
            reports.push(step_report(
                workspace,
                DoctorReport {
                    provider: "application-tests",
                    owner: &Owner::Repository(SystemResource::ApplicationTests),
                    root: &workspace.config.system.tests,
                    targets: Vec::new(),
                    configuration_files: Vec::new(),
                    status: StepStatus::Skipped,
                    skip_reason: Some(
                        "optional and not configured: no application-test suites were found".into(),
                    ),
                    stderr: String::new(),
                },
            ));
        }
        Ok(application) => {
            let owner = Owner::Repository(SystemResource::ApplicationTests);
            let mut process_compose = ready_report(
                workspace,
                Check {
                    provider: "process-compose".into(),
                    owner: owner.clone(),
                    root: workspace.root.clone(),
                    evidence: workspace.root.join("process-compose.yaml"),
                    configuration_files: vec![workspace.root.join("process-compose.yaml")],
                    tools: vec![OsString::from("process-compose")],
                    tla_jar: false,
                },
            );
            process_compose.stderr =
                "structural evidence only; semantic dry-run validation occurs during application test execution"
                    .into();
            reports.push(process_compose);
            reports.extend(
                application
                    .configured_suites()
                    .iter()
                    .filter_map(|suite| application_check(workspace, &application, &owner, *suite))
                    .map(|check| ready_report(workspace, check)),
            );
        }
        Err(error) => reports.push(step_report(
            workspace,
            DoctorReport {
                provider: "application-tests",
                owner: &Owner::Repository(SystemResource::ApplicationTests),
                root: &workspace.config.system.tests,
                targets: Vec::new(),
                configuration_files: Vec::new(),
                status: StepStatus::Failed,
                skip_reason: None,
                stderr: error.to_string(),
            },
        )),
    }
    reports
}

fn application_check(
    workspace: &Workspace,
    application: &application_testing::ApplicationDiscovery,
    owner: &Owner,
    suite: ApplicationSuite,
) -> Option<Check> {
    let tests = &workspace.config.system.tests;
    let (provider, tool, evidence, configuration_files) = match suite {
        ApplicationSuite::Contract => (
            "schemathesis",
            "uv",
            application.openapi_documents().first()?.clone(),
            vec![tests.join("pyproject.toml"), tests.join("uv.lock")],
        ),
        ApplicationSuite::E2e => (
            "playwright-e2e",
            "pnpm",
            tests.join("e2e"),
            vec![
                tests.join("package.json"),
                tests.join("playwright.config.ts"),
            ],
        ),
        ApplicationSuite::Visual => (
            "playwright-visual",
            "pnpm",
            tests.join("visual"),
            vec![
                tests.join("package.json"),
                tests.join("playwright.config.ts"),
            ],
        ),
        ApplicationSuite::UiProperty => (
            "bombadil",
            "pnpm",
            tests.join("bombadil/specification.ts"),
            vec![tests.join("package.json")],
        ),
        ApplicationSuite::Application => return None,
    };
    Some(Check {
        provider: provider.into(),
        owner: owner.clone(),
        root: tests.clone(),
        evidence,
        configuration_files,
        tools: vec![OsString::from(tool)],
        tla_jar: false,
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

const fn is_target_owner(owner: &Owner) -> bool {
    matches!(owner, Owner::Target(_) | Owner::Component { .. })
}

fn manifest_report(
    workspace: &Workspace,
    resource: &crate::discovery::DiscoveredResource,
    provider: &str,
) -> StepReport {
    step_report(
        workspace,
        DoctorReport {
            provider,
            owner: &resource.owner,
            root: resource.path.parent().unwrap_or_else(|| Path::new("")),
            targets: vec![resource.path.clone()],
            configuration_files: Vec::new(),
            status: StepStatus::Passed,
            skip_reason: None,
            stderr: String::new(),
        },
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
            .unwrap_or_else(|| Path::new(""))
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
            .unwrap_or_else(|| Path::new(""))
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
        DoctorReport {
            provider: &check.provider,
            owner: &check.owner,
            root: &check.root,
            targets: vec![check.evidence],
            configuration_files: check.configuration_files,
            status: if diagnostics.is_empty() {
                StepStatus::Passed
            } else {
                StepStatus::Failed
            },
            skip_reason: None,
            stderr: diagnostics.join("; "),
        },
    )
}

fn skipped_dylint_report(
    workspace: &Workspace,
    manifest: &crate::discovery::DiscoveredResource,
) -> StepReport {
    step_report(
        workspace,
        DoctorReport {
            provider: "dylint",
            owner: &manifest.owner,
            root: manifest.path.parent().unwrap_or_else(|| Path::new("")),
            targets: vec![manifest.path.clone()],
            configuration_files: Vec::new(),
            status: StepStatus::Skipped,
            skip_reason: Some(
                "not applicable: workspace.metadata.dylint.libraries is not declared".into(),
            ),
            stderr: String::new(),
        },
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
        DoctorReport {
            provider,
            owner: &manifest.owner,
            root: manifest.path.parent().unwrap_or_else(|| Path::new("")),
            targets: vec![manifest.path.clone()],
            configuration_files: Vec::new(),
            status: StepStatus::Skipped,
            skip_reason: Some(format!(
                "not applicable: package.json scripts.{script} is not declared"
            )),
            stderr: String::new(),
        },
    )
}

fn failed_dylint_report(
    workspace: &Workspace,
    manifest: &crate::discovery::DiscoveredResource,
    diagnostic: String,
) -> StepReport {
    step_report(
        workspace,
        DoctorReport {
            provider: "dylint",
            owner: &manifest.owner,
            root: manifest.path.parent().unwrap_or_else(|| Path::new("")),
            targets: vec![manifest.path.clone()],
            configuration_files: Vec::new(),
            status: StepStatus::Failed,
            skip_reason: None,
            stderr: diagnostic,
        },
    )
}

fn failed_requirement_report(
    workspace: &Workspace,
    manifest: &crate::discovery::DiscoveredResource,
    provider: &str,
    diagnostic: String,
) -> StepReport {
    step_report(
        workspace,
        DoctorReport {
            provider,
            owner: &manifest.owner,
            root: manifest.path.parent().unwrap_or_else(|| Path::new("")),
            targets: vec![manifest.path.clone()],
            configuration_files: Vec::new(),
            status: StepStatus::Failed,
            skip_reason: None,
            stderr: diagnostic,
        },
    )
}

struct DoctorReport<'a> {
    provider: &'a str,
    owner: &'a Owner,
    root: &'a Path,
    targets: Vec<PathBuf>,
    configuration_files: Vec<PathBuf>,
    status: StepStatus,
    skip_reason: Option<String>,
    stderr: String,
}

fn step_report(workspace: &Workspace, report: DoctorReport<'_>) -> StepReport {
    let (target, component) = match report.owner {
        Owner::Target(target) => (Some(target.clone()), None),
        Owner::Component { target, component } => (Some(target.clone()), component.clone()),
        Owner::Repository(_) => (None, None),
    };
    StepReport {
        id: format!(
            "doctor:{}:{}",
            relative(workspace, report.root).display(),
            report.provider
        ),
        capability: capability_name(Capability::DoctorCheck).into(),
        provider: report.provider.into(),
        target,
        component,
        cwd: relative(workspace, report.root)
            .to_string_lossy()
            .into_owned(),
        argv: Vec::new(),
        configuration_files: report
            .configuration_files
            .iter()
            .map(|path| relative(workspace, path).to_string_lossy().into_owned())
            .collect(),
        targets: report
            .targets
            .iter()
            .map(|path| relative(workspace, path).to_string_lossy().into_owned())
            .collect(),
        status: report.status,
        exit_code: None,
        duration: 0,
        skip_reason: report.skip_reason,
        stdout: String::new(),
        stderr: report.stderr,
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
