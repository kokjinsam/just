//! Type-checking and compilation workflow planning and execution.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

use crate::capability::{
    Capability, CapabilityBinding, ProviderId, bindings_for_selections, manifests_for_selections,
};
use crate::error::WorkflowError;
use crate::formatting::{InterruptGuard, find_program, was_interrupted};
use crate::ownership::{Owner, Ownership};
use crate::plan::{Plan, Step, StepId};
use crate::report::{Report, ReportStatus, StepReport, StepStatus};
use crate::selector::Selection;
use crate::workspace::Workspace;

const NOT_APPLICABLE: &str = "not-applicable";

/// Builds a type-checking or compilation plan.
///
/// # Errors
///
/// Returns an error when discovery, provider configuration, or plan validation
/// fails.
pub fn build_plan(
    workspace: &Workspace,
    selections: &[Selection],
    capability: Capability,
) -> Result<Plan, WorkflowError> {
    if !matches!(capability, Capability::Typecheck | Capability::Compile) {
        return Err(WorkflowError::Planning(
            "checking requires typecheck or compile".into(),
        ));
    }

    let mut bindings = bindings_for_selections(workspace, selections)?
        .into_iter()
        .filter(|binding| binding.capability == capability)
        .collect::<Vec<_>>();
    bindings.sort_by(|left, right| {
        provider_order(capability, left.provider.as_str())
            .cmp(&provider_order(capability, right.provider.as_str()))
            .then_with(|| left.root.cmp(&right.root))
    });

    let applicable_owners = bindings
        .iter()
        .map(|binding| binding.owner.clone())
        .collect::<HashSet<_>>();
    let mut steps = bindings
        .into_iter()
        .enumerate()
        .map(|(index, binding)| executable_step(binding, index))
        .collect::<Result<Vec<_>, _>>()?;

    let mut owners = selected_owner_roots(workspace, selections)?;
    owners.sort_by(|left, right| {
        left.1
            .cmp(&right.1)
            .then_with(|| owner_key(&left.0).cmp(&owner_key(&right.0)))
    });
    let mut seen_owners = HashSet::new();
    owners.retain(|(owner, _)| seen_owners.insert(owner.clone()));
    for (index, (owner, root)) in owners.into_iter().enumerate() {
        if !applicable_owners.contains(&owner) {
            steps.push(skipped_step(capability, owner, root, index));
        }
    }
    Plan::new(steps)
}

fn provider_order(capability: Capability, provider: &str) -> usize {
    match (capability, provider) {
        (Capability::Typecheck, "dialyzer") | (Capability::Compile, "mix-compile") => 0,
        (Capability::Typecheck, "package-typecheck") | (Capability::Compile, "package-build") => 1,
        (Capability::Compile, "cargo-check") => 2,
        _ => usize::MAX,
    }
}

fn executable_step(binding: CapabilityBinding, index: usize) -> Result<Step, WorkflowError> {
    let (program, argv): (&str, &[&str]) = match binding.provider.as_str() {
        "dialyzer" => ("mix", &["dialyzer"]),
        "package-typecheck" => ("pnpm", &["run", "typecheck"]),
        "mix-compile" => ("mix", &["compile", "--warnings-as-errors"]),
        "package-build" => ("pnpm", &["run", "build"]),
        "cargo-check" => ("cargo", &["check", "--all-targets"]),
        provider => {
            return Err(WorkflowError::Planning(format!(
                "unsupported checking provider '{provider}'"
            )));
        }
    };
    Ok(Step {
        id: StepId::new(format!(
            "{}:{index:04}:{}",
            capability_name(binding.capability),
            binding.provider.as_str()
        )),
        label: format!(
            "{} {}",
            capability_name(binding.capability),
            binding.provider.as_str()
        ),
        capability: binding.capability,
        provider: binding.provider,
        owner: binding.owner,
        cwd: binding.root,
        program: OsString::from(program),
        argv: argv.iter().map(OsString::from).collect(),
        env: BTreeMap::new(),
        targets: Vec::new(),
        configuration_files: binding.configuration_files,
        depends_on: Vec::new(),
    })
}

fn skipped_step(capability: Capability, owner: Owner, root: PathBuf, index: usize) -> Step {
    Step {
        id: StepId::new(format!("{}:skip:{index:04}", capability_name(capability))),
        label: format!("{} not applicable", capability_name(capability)),
        capability,
        provider: ProviderId::new(NOT_APPLICABLE),
        owner,
        cwd: root,
        program: OsString::new(),
        argv: Vec::new(),
        env: BTreeMap::new(),
        targets: Vec::new(),
        configuration_files: Vec::new(),
        depends_on: Vec::new(),
    }
}

fn selected_owner_roots(
    workspace: &Workspace,
    selections: &[Selection],
) -> Result<Vec<(Owner, PathBuf)>, WorkflowError> {
    let mut owners = manifests_for_selections(workspace, selections)?
        .iter()
        .map(|resource| {
            (
                resource.owner.clone(),
                resource
                    .path
                    .parent()
                    .unwrap_or_else(|| Path::new(""))
                    .to_path_buf(),
            )
        })
        .collect::<Vec<_>>();
    let ownership = Ownership::new(&workspace.config);
    for selection in selections {
        let configured = match selection {
            Selection::Repository => workspace
                .config
                .targets
                .values()
                .flat_map(configured_target_owners)
                .collect::<Vec<_>>(),
            Selection::Target(target) => {
                configured_target_owners(&workspace.config.targets[target]).collect()
            }
            Selection::Component { target, component } => workspace.config.targets[target]
                .components
                .iter()
                .filter(|candidate| candidate.name.as_deref() == Some(component))
                .map(|candidate| {
                    (
                        Owner::Component {
                            target: target.clone(),
                            component: Some(component.clone()),
                        },
                        candidate.root.clone(),
                    )
                })
                .collect(),
            Selection::Path(_) => Vec::new(),
        };
        for configured_owner in configured {
            if !owners
                .iter()
                .any(|(candidate, _)| candidate == &configured_owner.0)
            {
                owners.push(configured_owner);
            }
        }
        if !matches!(selection, Selection::Path(_)) {
            continue;
        }
        let root = match selection {
            Selection::Path(path) if path.is_dir() => path.clone(),
            Selection::Path(path) => path.parent().unwrap_or(&workspace.root).to_path_buf(),
            _ => unreachable!("configured selections were handled above"),
        };
        let owner = ownership.owner_of(&root)?;
        if !owners.iter().any(|(candidate, _)| candidate == &owner) {
            owners.push((owner, root));
        }
    }
    Ok(owners)
}

fn configured_target_owners(
    target: &crate::config::Target,
) -> impl Iterator<Item = (Owner, PathBuf)> + '_ {
    target.components.iter().map(|component| {
        (
            Owner::Component {
                target: target.name.clone(),
                component: component.name.clone(),
            },
            component.root.clone(),
        )
    })
}

fn owner_key(owner: &Owner) -> String {
    match owner {
        Owner::Repository(_) => String::new(),
        Owner::Target(target) => target.clone(),
        Owner::Component { target, component } => {
            format!("{target}:{}", component.as_deref().unwrap_or_default())
        }
    }
}

/// Executes a type-checking or compilation plan.
///
/// # Errors
///
/// Returns an error when preflight, provider execution, or reporting fails.
pub fn execute_plan(
    workspace: &Workspace,
    plan: &Plan,
    selectors: Vec<String>,
    started_at: String,
    capability: Capability,
) -> Result<(Report, String, String), WorkflowError> {
    let interrupt = InterruptGuard::install()?;
    preflight(plan, capability)?;
    let started = Instant::now();
    let mut reports = Vec::new();
    let mut aggregate_stdout = String::new();
    let mut aggregate_stderr = String::new();
    let mut interrupted = false;

    for step in plan.steps() {
        if interrupt.received() {
            interrupted = true;
            break;
        }
        if step.provider.as_str() == NOT_APPLICABLE {
            reports.push(skipped_report(workspace, step));
            continue;
        }
        let step_started = Instant::now();
        let output = run_command(step, capability)?;
        aggregate_stdout.push_str(&String::from_utf8_lossy(&output.stdout));
        aggregate_stderr.push_str(&String::from_utf8_lossy(&output.stderr));
        reports.push(command_report(
            workspace,
            step,
            &output,
            step_started.elapsed().as_millis() as u64,
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
            command: capability_name(capability).into(),
            selectors,
            started_at,
            duration: started.elapsed().as_millis() as u64,
            status,
            steps: reports,
        },
        aggregate_stdout,
        aggregate_stderr,
    ))
}

fn preflight(plan: &Plan, capability: Capability) -> Result<(), WorkflowError> {
    let mut tools = BTreeSet::new();
    for step in plan.steps() {
        if step.provider.as_str() == NOT_APPLICABLE {
            continue;
        }
        if !step.cwd.is_dir() {
            return Err(WorkflowError::Planning(format!(
                "{} working directory '{}' is not accessible",
                capability_name(capability),
                step.cwd.display()
            )));
        }
        tools.insert(step.program.clone());
    }
    for tool in tools {
        if find_program(&tool).is_none() {
            return Err(WorkflowError::Planning(format!(
                "{} tool '{}' is not available",
                capability_name(capability),
                tool.to_string_lossy()
            )));
        }
    }
    Ok(())
}

fn run_command(step: &Step, capability: Capability) -> Result<Output, WorkflowError> {
    Command::new(&step.program)
        .args(&step.argv)
        .current_dir(&step.cwd)
        .envs(&step.env)
        .output()
        .map_err(|error| {
            WorkflowError::Planning(format!(
                "could not execute {} tool '{}': {error}",
                capability_name(capability),
                step.program.to_string_lossy()
            ))
        })
}

fn skipped_report(workspace: &Workspace, step: &Step) -> StepReport {
    report_fields(
        workspace,
        step,
        ReportOutput {
            status: StepStatus::Skipped,
            exit_code: None,
            duration: 0,
            argv: Vec::new(),
            skip_reason: Some(format!(
                "not applicable: selected owner has no {} provider",
                capability_name(step.capability)
            )),
            stdout: String::new(),
            stderr: String::new(),
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
            status: if output.status.success() {
                StepStatus::Passed
            } else {
                StepStatus::Failed
            },
            exit_code: output.status.code(),
            duration,
            argv,
            skip_reason: None,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        },
    )
}

struct ReportOutput {
    status: StepStatus,
    exit_code: Option<i32>,
    duration: u64,
    argv: Vec<String>,
    skip_reason: Option<String>,
    stdout: String,
    stderr: String,
}

fn report_fields(workspace: &Workspace, step: &Step, output: ReportOutput) -> StepReport {
    let (target, component) = match &step.owner {
        Owner::Target(target) => (Some(target.clone()), None),
        Owner::Component { target, component } => (Some(target.clone()), component.clone()),
        Owner::Repository(_) => (None, None),
    };
    StepReport {
        id: step.id.as_str().into(),
        capability: capability_name(step.capability).into(),
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
        targets: Vec::new(),
        status: output.status,
        exit_code: output.exit_code,
        duration: output.duration,
        skip_reason: output.skip_reason,
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

fn relative<'a>(workspace: &'a Workspace, path: &'a Path) -> &'a Path {
    path.strip_prefix(&workspace.root).unwrap_or(path)
}

fn capability_name(capability: Capability) -> &'static str {
    match capability {
        Capability::Typecheck => "typecheck",
        Capability::Compile => "compile",
        _ => unreachable!("checking only supports typecheck and compile"),
    }
}
