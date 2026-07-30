//! Lint workflow planning and execution.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

use ignore::WalkBuilder;

use crate::capability::{
    Capability, CapabilityBinding, bindings_for_selections, resources_for_selections,
};
use crate::discovery::{DiscoveredResource, ResourceKind};
use crate::error::WorkflowError;
use crate::formatting::{InterruptGuard, find_program, was_interrupted};
use crate::ownership::Owner;
use crate::plan::{Plan, Step, StepId};
use crate::report::{Report, ReportStatus, StepReport, StepStatus};
use crate::selector::Selection;
use crate::workspace::Workspace;

const SPECIFICATION_PLACEMENT: &str = "specification-placement";

/// Builds a lint plan for selected resources.
///
/// # Errors
///
/// Returns an error when discovery, provider configuration, policy routing, or
/// plan validation fails.
pub fn build_plan(workspace: &Workspace, selections: &[Selection]) -> Result<Plan, WorkflowError> {
    let bindings = bindings_for_selections(workspace, selections)?;
    let resources = resources_for_selections(workspace, selections)?;
    let mut steps = Vec::new();
    if let Some(step) = specification_placement_step(workspace, selections)? {
        steps.push(step);
    }

    let mut lint_bindings = bindings
        .into_iter()
        .filter(|binding| binding.capability == Capability::Lint)
        .collect::<Vec<_>>();
    lint_bindings.sort_by(|left, right| {
        provider_order(left.provider.as_str())
            .cmp(&provider_order(right.provider.as_str()))
            .then_with(|| left.root.cmp(&right.root))
    });
    for (index, binding) in lint_bindings.into_iter().enumerate() {
        if let Some(step) = step_for_binding(workspace, selections, &resources, binding, index)? {
            steps.push(step);
        }
    }
    Plan::new(steps)
}

fn provider_order(provider: &str) -> usize {
    match provider {
        "credo" => 0,
        "sobelow" => 1,
        "ex-dna" => 2,
        "reach" => 3,
        "oxlint" => 4,
        "stylelint" => 5,
        "clippy" => 6,
        "dylint" => 7,
        _ => usize::MAX,
    }
}

fn step_for_binding(
    workspace: &Workspace,
    selections: &[Selection],
    resources: &[DiscoveredResource],
    binding: CapabilityBinding,
    index: usize,
) -> Result<Option<Step>, WorkflowError> {
    let provider = binding.provider.as_str();
    let targets = match provider {
        "oxlint" => package_targets(
            workspace,
            &binding,
            selections,
            resources,
            is_javascript_source,
        )?,
        "stylelint" => package_targets(workspace, &binding, selections, resources, is_css_source)?,
        _ => Vec::new(),
    };
    if matches!(provider, "oxlint" | "stylelint") && targets.is_empty() {
        return Ok(None);
    }
    let (program, argv) = match provider {
        "credo" => ("mix", vec!["credo", "--strict"]),
        "sobelow" => (
            "mix",
            vec!["sobelow", "--private", "--exit", "--threshold", "high"],
        ),
        "ex-dna" => (
            "mix",
            vec!["ex_dna", "--min-mass", "40", "--max-clones", "0"],
        ),
        "reach" => ("mix", vec!["reach.check", "--arch", "--smells", "--strict"]),
        "oxlint" => ("pnpm", vec!["exec", "oxlint"]),
        "stylelint" => ("pnpm", vec!["exec", "stylelint"]),
        "clippy" => (
            "cargo",
            vec![
                "clippy",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ],
        ),
        "dylint" => ("cargo", vec!["dylint", "--all", "--", "--all-targets"]),
        other => {
            return Err(WorkflowError::Planning(format!(
                "unsupported lint provider '{other}'"
            )));
        }
    };
    let mut argv = argv.into_iter().map(OsString::from).collect::<Vec<_>>();
    argv.extend(targets.iter().map(|target| target_argument(target)));
    Ok(Some(Step {
        id: StepId::new(format!("lint:{index:04}:{provider}")),
        label: format!("lint {provider}"),
        capability: Capability::Lint,
        provider: binding.provider,
        owner: binding.owner,
        cwd: binding.root,
        program: OsString::from(program),
        argv,
        env: BTreeMap::new(),
        targets,
        configuration_files: binding.configuration_files,
        depends_on: Vec::new(),
    }))
}

fn target_argument(target: &Path) -> OsString {
    if target
        .components()
        .next()
        .and_then(|component| component.as_os_str().to_str())
        .is_some_and(|component| component.starts_with('-'))
    {
        Path::new(".").join(target).into_os_string()
    } else {
        target.as_os_str().to_owned()
    }
}

fn package_targets(
    workspace: &Workspace,
    binding: &CapabilityBinding,
    selections: &[Selection],
    resources: &[DiscoveredResource],
    accepts: fn(&Path) -> bool,
) -> Result<Vec<PathBuf>, WorkflowError> {
    let mut targets = Vec::new();
    for selection in selections {
        let path = match selection {
            Selection::Repository => binding.root.as_path(),
            Selection::Target(target) if owner_target(&binding.owner) == Some(target.as_str()) => {
                binding.root.as_path()
            }
            Selection::Component { target, component }
                if binding.owner
                    == (Owner::Component {
                        target: target.clone(),
                        component: Some(component.clone()),
                    }) =>
            {
                binding.root.as_path()
            }
            Selection::Path(path)
                if path == &binding.root
                    || path == &binding.evidence
                    || binding.root.starts_with(path)
                    || binding.configuration_files.contains(path) =>
            {
                binding.root.as_path()
            }
            Selection::Path(path) if path.starts_with(&binding.root) => path,
            _ => continue,
        };
        if path.is_file() {
            if accepts(path)
                && !is_lint_configuration(path)
                && nearest_package_root(path, resources).as_deref() == Some(&binding.root)
            {
                targets.push(relative_to(&binding.root, path));
            }
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        for source in walk_files(workspace, path)? {
            if accepts(&source)
                && !is_lint_configuration(&source)
                && nearest_package_root(&source, resources).as_deref() == Some(&binding.root)
            {
                targets.push(relative_to(&binding.root, &source));
            }
        }
    }
    targets.sort();
    targets.dedup();
    Ok(targets)
}

fn is_lint_configuration(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| {
            name.starts_with("oxlint.config.") || name.starts_with("stylelint.config.")
        })
}

fn owner_target(owner: &Owner) -> Option<&str> {
    match owner {
        Owner::Target(target) | Owner::Component { target, .. } => Some(target),
        Owner::Repository(_) => None,
    }
}

fn nearest_package_root(path: &Path, resources: &[DiscoveredResource]) -> Option<PathBuf> {
    resources
        .iter()
        .filter(|resource| {
            resource.kind == ResourceKind::NodeManifest
                && resource
                    .path
                    .parent()
                    .is_some_and(|root| path.starts_with(root))
        })
        .filter_map(|resource| resource.path.parent())
        .max_by_key(|root| root.components().count())
        .map(Path::to_path_buf)
}

fn is_javascript_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(OsStr::to_str),
        Some("js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts")
    )
}

fn is_css_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(OsStr::to_str),
        Some("css" | "scss")
    )
}

fn specification_placement_step(
    workspace: &Workspace,
    selections: &[Selection],
) -> Result<Option<Step>, WorkflowError> {
    let mut specifications = Vec::new();
    for selection in selections {
        match selection {
            Selection::Repository => {
                specifications.extend(specification_files(workspace, &workspace.root)?);
            }
            Selection::Target(target) => {
                specifications.extend(specification_files(
                    workspace,
                    &workspace.config.targets[target].root,
                )?);
            }
            Selection::Component { target, component } => {
                let expected_owner = Owner::Component {
                    target: target.clone(),
                    component: Some(component.clone()),
                };
                let root = workspace.config.targets[target]
                    .components
                    .iter()
                    .find(|candidate| candidate.name.as_deref() == Some(component))
                    .map(|component| component.root.as_path())
                    .ok_or_else(|| {
                        WorkflowError::Planning(format!(
                            "cannot find component '@{target}:{component}'"
                        ))
                    })?;
                let ownership = crate::ownership::Ownership::new(&workspace.config);
                specifications.extend(specification_files(workspace, root)?.into_iter().filter(
                    |path| {
                        ownership
                            .owner_of(path)
                            .is_ok_and(|owner| owner == expected_owner)
                    },
                ));
            }
            Selection::Path(path) if path.is_dir() => {
                specifications.extend(specification_files(workspace, path)?);
            }
            Selection::Path(path) if is_specification(path)? => {
                specifications.push(path.clone());
            }
            Selection::Path(_) => {}
        }
    }
    specifications.sort();
    specifications.dedup();
    if specifications.is_empty() {
        return Ok(None);
    }
    let misplaced = specifications
        .into_iter()
        .filter(|path| !path.starts_with(&workspace.config.system.specs))
        .collect::<Vec<_>>();
    Ok(Some(Step {
        id: StepId::new("lint:specification-placement"),
        label: "lint specification placement".into(),
        capability: Capability::Lint,
        provider: crate::capability::ProviderId::new(SPECIFICATION_PLACEMENT),
        owner: Owner::Repository(crate::ownership::SystemResource::Repository),
        cwd: workspace.root.clone(),
        program: OsString::new(),
        argv: Vec::new(),
        env: BTreeMap::new(),
        targets: misplaced,
        configuration_files: vec![workspace.root.join("just.yaml")],
        depends_on: Vec::new(),
    }))
}

fn specification_files(workspace: &Workspace, root: &Path) -> Result<Vec<PathBuf>, WorkflowError> {
    let mut files = Vec::new();
    for path in walk_files(workspace, root)? {
        if is_specification(&path)? {
            files.push(path);
        }
    }
    Ok(files)
}

fn walk_files(workspace: &Workspace, root: &Path) -> Result<Vec<PathBuf>, WorkflowError> {
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
    let scope = root.to_path_buf();
    builder.filter_entry(move |entry| {
        entry.file_name() != ".git"
            && (entry.path().starts_with(&scope) || scope.starts_with(entry.path()))
    });
    let mut files = Vec::new();
    for entry in builder.build() {
        let entry = entry.map_err(|error| {
            WorkflowError::Discovery(format!("cannot walk '{}': {error}", root.display()))
        })?;
        if entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
            && entry.path().starts_with(root)
        {
            files.push(fs::canonicalize(entry.path()).map_err(|error| {
                WorkflowError::Discovery(format!(
                    "cannot resolve '{}': {error}",
                    entry.path().display()
                ))
            })?);
        }
    }
    Ok(files)
}

fn is_specification(path: &Path) -> Result<bool, WorkflowError> {
    match path.extension().and_then(OsStr::to_str) {
        Some("tla" | "cfg" | "als") => Ok(true),
        Some("yaml" | "yml" | "json") => {
            let source = fs::read_to_string(path).map_err(|error| {
                WorkflowError::Discovery(format!(
                    "cannot inspect specification candidate '{}': {error}",
                    path.display()
                ))
            })?;
            let Ok(document) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&source) else {
                return Ok(false);
            };
            Ok(document.as_mapping().is_some_and(|mapping| {
                ["openapi", "swagger"]
                    .iter()
                    .any(|key| mapping.contains_key(serde_yaml_ng::Value::String((*key).into())))
            }))
        }
        _ => Ok(false),
    }
}

/// Executes a lint plan.
///
/// # Errors
///
/// Returns an error when preflight, provider execution, interruption cleanup,
/// or reporting fails.
pub fn execute_plan(
    workspace: &Workspace,
    plan: &Plan,
    selectors: Vec<String>,
    started_at: String,
) -> Result<(Report, String, String), WorkflowError> {
    let prepared = prepare_plan(plan)?;
    execute_prepared(workspace, &prepared, selectors, started_at)
}

pub(crate) struct PreparedPlan {
    plan: Plan,
}

pub(crate) fn prepare_plan(plan: &Plan) -> Result<PreparedPlan, WorkflowError> {
    preflight(plan)?;
    Ok(PreparedPlan { plan: plan.clone() })
}

pub(crate) fn execute_prepared(
    workspace: &Workspace,
    prepared: &PreparedPlan,
    selectors: Vec<String>,
    started_at: String,
) -> Result<(Report, String, String), WorkflowError> {
    let interrupt = InterruptGuard::install()?;
    let plan = &prepared.plan;
    let command_started = Instant::now();
    let mut reports = Vec::new();
    let mut aggregate_stdout = String::new();
    let mut aggregate_stderr = String::new();
    let mut interrupted = false;

    for step in plan.steps() {
        if interrupt.received() {
            interrupted = true;
            break;
        }
        let started = Instant::now();
        if step.provider.as_str() == SPECIFICATION_PLACEMENT {
            reports.push(policy_report(
                workspace,
                step,
                started.elapsed().as_millis() as u64,
            ));
            continue;
        }
        let output = run_command(step)?;
        aggregate_stdout.push_str(&String::from_utf8_lossy(&output.stdout));
        aggregate_stderr.push_str(&String::from_utf8_lossy(&output.stderr));
        reports.push(command_report(
            workspace,
            step,
            &output,
            started.elapsed().as_millis() as u64,
        ));
        if interrupt.received() || was_interrupted(&output) {
            interrupted = true;
            break;
        }
    }
    let status = if interrupted {
        ReportStatus::Interrupted
    } else if reports
        .iter()
        .any(|report| report.status == StepStatus::Failed)
    {
        ReportStatus::Failed
    } else {
        ReportStatus::Passed
    };
    Ok((
        Report {
            schema_version: 1,
            command: "lint".into(),
            selectors,
            started_at,
            duration: command_started.elapsed().as_millis() as u64,
            status,
            steps: reports,
        },
        aggregate_stdout,
        aggregate_stderr,
    ))
}

fn preflight(plan: &Plan) -> Result<(), WorkflowError> {
    let mut tools = BTreeSet::new();
    for step in plan.steps() {
        if step.provider.as_str() == SPECIFICATION_PLACEMENT {
            continue;
        }
        if !step.cwd.is_dir() {
            return Err(WorkflowError::Planning(format!(
                "lint working directory '{}' is not accessible",
                step.cwd.display()
            )));
        }
        tools.insert(step.program.clone());
        if step.provider.as_str() == "dylint" {
            tools.insert(OsString::from("cargo-dylint"));
            tools.insert(OsString::from("dylint-link"));
        }
        if step.provider.as_str() == "clippy" {
            tools.insert(OsString::from("cargo-clippy"));
        }
    }
    for tool in tools {
        if find_program(&tool).is_none() {
            return Err(WorkflowError::Planning(format!(
                "lint tool '{}' is not available",
                tool.to_string_lossy()
            )));
        }
    }
    Ok(())
}

fn run_command(step: &Step) -> Result<Output, WorkflowError> {
    Command::new(&step.program)
        .args(&step.argv)
        .current_dir(&step.cwd)
        .envs(&step.env)
        .output()
        .map_err(|error| {
            WorkflowError::Planning(format!(
                "could not execute lint tool '{}': {error}",
                step.program.to_string_lossy()
            ))
        })
}

fn policy_report(workspace: &Workspace, step: &Step, duration: u64) -> StepReport {
    let failed = !step.targets.is_empty();
    let stderr = if failed {
        let mut message = format!(
            "specification files must be beneath '{}':\n",
            relative(workspace, &workspace.config.system.specs).display()
        );
        for path in &step.targets {
            let _ = writeln!(message, "  {}", relative(workspace, path).display());
        }
        message
    } else {
        String::new()
    };
    report_fields(
        workspace,
        step,
        ReportOutput {
            exit_code: failed.then_some(1).or(Some(0)),
            status: if failed {
                StepStatus::Failed
            } else {
                StepStatus::Passed
            },
            duration,
            stdout: String::new(),
            stderr,
            argv: Vec::new(),
        },
    )
}

fn command_report(
    workspace: &Workspace,
    step: &Step,
    output: &Output,
    duration: u64,
) -> StepReport {
    let mut argv = vec![step.program.to_string_lossy().into_owned()];
    argv.extend(
        step.argv
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned()),
    );
    report_fields(
        workspace,
        step,
        ReportOutput {
            exit_code: output.status.code(),
            status: if output.status.success() {
                StepStatus::Passed
            } else {
                StepStatus::Failed
            },
            duration,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            argv,
        },
    )
}

struct ReportOutput {
    exit_code: Option<i32>,
    status: StepStatus,
    duration: u64,
    stdout: String,
    stderr: String,
    argv: Vec<String>,
}

fn report_fields(workspace: &Workspace, step: &Step, output: ReportOutput) -> StepReport {
    let (target, component) = match &step.owner {
        Owner::Target(target) => (Some(target.clone()), None),
        Owner::Component { target, component } => (Some(target.clone()), component.clone()),
        Owner::Repository(_) => (None, None),
    };
    StepReport {
        id: step.id.as_str().into(),
        capability: "lint".into(),
        provider: step.provider.as_str().into(),
        target,
        component,
        cwd: relative(workspace, &step.cwd)
            .to_string_lossy()
            .into_owned(),
        argv: output.argv,
        configuration_files: step
            .configuration_files
            .iter()
            .map(|path| relative(workspace, path).to_string_lossy().into_owned())
            .collect(),
        targets: step
            .targets
            .iter()
            .map(|path| relative(workspace, path).to_string_lossy().into_owned())
            .collect(),
        status: output.status,
        exit_code: output.exit_code,
        duration: output.duration,
        skip_reason: None,
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

fn relative<'a>(workspace: &'a Workspace, path: &'a Path) -> &'a Path {
    path.strip_prefix(&workspace.root).unwrap_or(path)
}

fn relative_to(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}
