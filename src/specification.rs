//! TLA+, Alloy, and OpenAPI specification planning and execution.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

use clap::ValueEnum;
use ignore::WalkBuilder;

use crate::capability::{Capability, ProviderId};
use crate::error::WorkflowError;
use crate::formatting::{InterruptGuard, find_program, was_interrupted};
use crate::ownership::{Owner, SystemResource};
use crate::plan::{Plan, Step, StepId};
use crate::report::{Report, ReportStatus, StepReport, StepStatus};
use crate::workspace::Workspace;

const TEMPORARY: &str = "<temporary>";

/// Supported specification kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SpecificationKind {
    /// TLA+ modules and model configurations.
    Tla,
    /// Alloy models.
    Alloy,
    /// OpenAPI documents.
    Openapi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionProvenance {
    Discovered,
    Explicit,
}

impl SpecificationKind {
    const ALL: [Self; 3] = [Self::Tla, Self::Alloy, Self::Openapi];

    const fn directory(self) -> &'static str {
        match self {
            Self::Tla => "tla",
            Self::Alloy => "alloy",
            Self::Openapi => "openapi",
        }
    }

    fn accepts(self, path: &Path) -> bool {
        if self == Self::Tla && path.file_name().is_some_and(|name| name == ".apalache.cfg") {
            return false;
        }
        if self == Self::Openapi
            && path
                .file_name()
                .is_some_and(|name| name == "vacuum.conf.yaml")
        {
            return false;
        }
        matches!(
            (self, path.extension().and_then(OsStr::to_str)),
            (Self::Tla, Some("tla" | "cfg"))
                | (Self::Alloy, Some("als"))
                | (Self::Openapi, Some("yaml" | "yml" | "json"))
        )
    }
}

/// Builds a specification-check plan.
///
/// # Errors
///
/// Returns an error when kind-specific selections are invalid, ambiguous, or
/// unsupported by discovered evidence.
pub fn build_plan(
    workspace: &Workspace,
    kind: Option<SpecificationKind>,
    selections: &[String],
) -> Result<Plan, WorkflowError> {
    if selections
        .iter()
        .any(|selection| selection.starts_with('@'))
    {
        return Err(WorkflowError::Selector(
            "spec accepts only repository specification names or paths, not @target selectors"
                .into(),
        ));
    }
    if kind.is_none() && !selections.is_empty() {
        return Err(WorkflowError::Selector(
            "spec names and paths require an explicit kind".into(),
        ));
    }

    let kinds = kind.map_or_else(|| SpecificationKind::ALL.to_vec(), |kind| vec![kind]);
    let mut steps = Vec::new();
    for selected_kind in kinds {
        let (files, provenance) = select_files(workspace, selected_kind, selections)?;
        match selected_kind {
            SpecificationKind::Tla => build_tla_steps(workspace, &files, provenance, &mut steps)?,
            SpecificationKind::Alloy => build_alloy_steps(workspace, files, &mut steps),
            SpecificationKind::Openapi => build_openapi_steps(workspace, files, &mut steps),
        }
    }
    Plan::new(steps)
}

fn select_files(
    workspace: &Workspace,
    kind: SpecificationKind,
    selections: &[String],
) -> Result<(Vec<PathBuf>, SelectionProvenance), WorkflowError> {
    let root = workspace.config.system.specs.join(kind.directory());
    if !root.exists() {
        if selections.is_empty() {
            return Ok((Vec::new(), SelectionProvenance::Discovered));
        }
        return Err(WorkflowError::Selector(format!(
            "specification kind directory '{}' does not exist",
            root.display()
        )));
    }
    if !root.is_dir() {
        return Err(WorkflowError::Planning(format!(
            "specification kind root '{}' is not a directory",
            root.display()
        )));
    }
    if selections.is_empty() {
        return discovered_files(workspace, &root, kind)
            .map(|files| (files, SelectionProvenance::Discovered));
    }

    let all = all_files(&root)?;
    let mut selected = BTreeSet::new();
    for selection in selections {
        let candidate = resolve_explicit_selection(workspace, &root, kind, &all, selection)?;
        if candidate.is_dir() {
            for file in all_files(&candidate)? {
                if kind.accepts(&file) {
                    selected.insert(file);
                }
            }
        } else if kind.accepts(&candidate) {
            selected.insert(candidate);
        } else {
            return Err(WorkflowError::Selector(format!(
                "selection '{selection}' is not a {} specification",
                kind.directory()
            )));
        }
    }
    Ok((
        selected.into_iter().collect(),
        SelectionProvenance::Explicit,
    ))
}

fn resolve_explicit_selection(
    workspace: &Workspace,
    root: &Path,
    kind: SpecificationKind,
    all: &[PathBuf],
    selection: &str,
) -> Result<PathBuf, WorkflowError> {
    let path = PathBuf::from(selection);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(WorkflowError::Selector(format!(
            "spec selection '{selection}' must be relative and cannot escape system.specs"
        )));
    }
    let mut components = path.components();
    let is_bare_name = matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    );
    let direct_candidates = [
        workspace.invocation.join(&path),
        workspace.config.system.specs.join(&path),
        root.join(&path),
    ];
    let mut matches = direct_candidates
        .iter()
        .filter(|candidate| candidate.exists())
        .filter_map(|candidate| fs::canonicalize(candidate).ok())
        .collect::<BTreeSet<_>>();
    if is_bare_name {
        matches.retain(|candidate| {
            candidate.starts_with(root) && (candidate.is_dir() || kind.accepts(candidate))
        });
    } else {
        matches.retain(|candidate| candidate.starts_with(&workspace.config.system.specs));
    }
    if matches.is_empty() && is_bare_name {
        matches = all
            .iter()
            .filter(|candidate| kind.accepts(candidate))
            .filter(|candidate| {
                let requested_name = path.file_name();
                candidate.file_name() == requested_name || candidate.file_stem() == requested_name
            })
            .cloned()
            .collect();
    }
    if kind == SpecificationKind::Tla
        && matches.len() == 2
        && let Some(module) = matches
            .iter()
            .find(|candidate| {
                candidate
                    .extension()
                    .is_some_and(|extension| extension == "tla")
            })
            .filter(|module| matches.contains(&module.with_extension("cfg")))
            .cloned()
    {
        matches = std::iter::once(module).collect();
    }
    let qualifier = kind.directory();
    match matches.len() {
        0 => Err(WorkflowError::Selector(format!(
            "unknown {qualifier} specification selection '{selection}'"
        ))),
        1 => {
            let candidate = matches.into_iter().next().ok_or_else(|| {
                WorkflowError::Selector(format!(
                    "unknown {qualifier} specification selection '{selection}'"
                ))
            })?;
            if candidate.starts_with(root) {
                Ok(candidate)
            } else {
                Err(WorkflowError::Selector(format!(
                    "selection '{selection}' is not a {qualifier} specification"
                )))
            }
        }
        _ => Err(WorkflowError::Selector(format!(
            "ambiguous {qualifier} specification selection '{selection}'"
        ))),
    }
}

fn discovered_files(
    workspace: &Workspace,
    root: &Path,
    kind: SpecificationKind,
) -> Result<Vec<PathBuf>, WorkflowError> {
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
    let selected_root = root.to_path_buf();
    builder.filter_entry(move |entry| {
        entry.file_name() != ".git"
            && (entry.path().starts_with(&selected_root) || selected_root.starts_with(entry.path()))
    });
    let mut files = Vec::new();
    for entry in builder.build() {
        let entry = entry.map_err(|error| {
            WorkflowError::Discovery(format!("cannot discover specifications: {error}"))
        })?;
        if entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
            && entry.path().starts_with(root)
            && kind.accepts(entry.path())
        {
            files.push(fs::canonicalize(entry.path()).map_err(|error| {
                WorkflowError::Discovery(format!(
                    "cannot resolve specification '{}': {error}",
                    entry.path().display()
                ))
            })?);
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn all_files(root: &Path) -> Result<Vec<PathBuf>, WorkflowError> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|error| {
            WorkflowError::Discovery(format!(
                "cannot inspect specification directory '{}': {error}",
                directory.display()
            ))
        })? {
            let path = entry
                .map_err(|error| WorkflowError::Discovery(error.to_string()))?
                .path();
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                WorkflowError::Discovery(format!("cannot inspect '{}': {error}", path.display()))
            })?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                files.push(fs::canonicalize(path).map_err(|error| {
                    WorkflowError::Discovery(format!("cannot resolve specification file: {error}"))
                })?);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn build_tla_steps(
    workspace: &Workspace,
    files: &[PathBuf],
    provenance: SelectionProvenance,
    steps: &mut Vec<Step>,
) -> Result<(), WorkflowError> {
    let root = workspace.config.system.specs.join("tla");
    let selected = files.iter().cloned().collect::<BTreeSet<_>>();
    let mut modules = BTreeSet::new();
    for file in files {
        match file.extension().and_then(OsStr::to_str) {
            Some("tla") => {
                modules.insert(file.clone());
            }
            Some("cfg") => {
                let module = file.with_extension("tla");
                let has_module = match provenance {
                    SelectionProvenance::Discovered => selected.contains(&module),
                    SelectionProvenance::Explicit => module.is_file(),
                };
                if !has_module {
                    return Err(WorkflowError::Planning(format!(
                        "orphan TLA+ configuration '{}'",
                        file.display()
                    )));
                }
                modules.insert(module);
            }
            _ => {}
        }
    }
    for module in modules {
        let cfg = module.with_extension("cfg");
        let cfg = match provenance {
            SelectionProvenance::Discovered => selected.contains(&cfg).then_some(cfg),
            SelectionProvenance::Explicit => cfg.is_file().then_some(cfg),
        };
        let tool_config = nearest_config(&module, &root, ".apalache.cfg");
        let cwd = tool_config
            .as_ref()
            .and_then(|path| path.parent())
            .unwrap_or(&root)
            .to_path_buf();
        let mut argv = vec![OsString::from(format!("--out-dir={TEMPORARY}"))];
        if let Some(cfg) = &cfg {
            argv.push(OsString::from("check"));
            argv.push(OsString::from(format!(
                "--config={}",
                relative_argument(&cwd, cfg).to_string_lossy()
            )));
        } else {
            argv.push(OsString::from("typecheck"));
        }
        argv.push(relative_argument(&cwd, &module).into_os_string());
        let mut configuration_files = Vec::new();
        if let Some(cfg) = cfg {
            configuration_files.push(cfg);
        }
        if let Some(tool_config) = tool_config {
            configuration_files.push(tool_config);
        }
        steps.push(spec_step(
            workspace,
            &module,
            "apalache",
            "apalache-mc",
            cwd,
            argv,
            configuration_files,
        ));
    }
    Ok(())
}

fn build_alloy_steps(workspace: &Workspace, files: Vec<PathBuf>, steps: &mut Vec<Step>) {
    let root = workspace.config.system.specs.join("alloy");
    for model in files {
        steps.push(spec_step(
            workspace,
            &model,
            "alloy",
            "alloy",
            root.clone(),
            vec![
                OsString::from("exec"),
                OsString::from("--quiet"),
                OsString::from("--type"),
                OsString::from("none"),
                OsString::from("--output"),
                OsString::from(TEMPORARY),
                relative_argument(&root, &model).into_os_string(),
            ],
            Vec::new(),
        ));
    }
}

fn build_openapi_steps(workspace: &Workspace, files: Vec<PathBuf>, steps: &mut Vec<Step>) {
    let root = workspace.config.system.specs.join("openapi");
    for document in files {
        let vacuum_config = nearest_config(&document, &root, "vacuum.conf.yaml");
        let cwd = vacuum_config
            .as_ref()
            .and_then(|path| path.parent())
            .unwrap_or(&root)
            .to_path_buf();
        let target = relative_argument(&cwd, &document).into_os_string();
        steps.push(spec_step(
            workspace,
            &document,
            "oasdiff",
            "oasdiff",
            cwd.clone(),
            vec![OsString::from("validate"), target.clone()],
            Vec::new(),
        ));
        steps.push(spec_step(
            workspace,
            &document,
            "vacuum",
            "vacuum",
            cwd,
            vec![
                OsString::from("lint"),
                OsString::from("--no-style"),
                OsString::from("--no-banner"),
                OsString::from("--no-update-check"),
                target,
            ],
            vacuum_config.into_iter().collect(),
        ));
    }
}

fn nearest_config(path: &Path, root: &Path, name: &str) -> Option<PathBuf> {
    let mut current = path.parent();
    while let Some(directory) = current {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        if directory == root {
            break;
        }
        current = directory.parent();
    }
    None
}

fn spec_step(
    workspace: &Workspace,
    target: &Path,
    provider: &str,
    program: &str,
    cwd: PathBuf,
    argv: Vec<OsString>,
    configuration_files: Vec<PathBuf>,
) -> Step {
    let relative = target
        .strip_prefix(&workspace.config.system.specs)
        .unwrap_or(target);
    Step {
        id: StepId::new(format!("spec:{}:{}", relative.display(), provider)),
        label: format!("spec {provider} {}", relative.display()),
        capability: Capability::SpecCheck,
        provider: ProviderId::new(provider),
        owner: Owner::Repository(SystemResource::Specifications),
        cwd,
        program: OsString::from(program),
        argv,
        env: BTreeMap::new(),
        targets: vec![target.to_path_buf()],
        configuration_files,
        depends_on: Vec::new(),
    }
}

fn relative_argument(cwd: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(cwd).map_or_else(
        |_| path.to_path_buf(),
        |relative| {
            if relative
                .components()
                .next()
                .is_some_and(|component| component.as_os_str().to_string_lossy().starts_with('-'))
            {
                Path::new(".").join(relative)
            } else {
                relative.to_path_buf()
            }
        },
    )
}

/// Executes a specification-check plan.
///
/// # Errors
///
/// Returns an error when preflight, provider execution, temporary-output
/// cleanup, or reporting fails.
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
    reservations: Vec<Option<tempfile::TempDir>>,
}

pub(crate) fn prepare_plan(plan: &Plan) -> Result<PreparedPlan, WorkflowError> {
    Ok(PreparedPlan {
        plan: plan.clone(),
        reservations: preflight(plan)?,
    })
}

pub(crate) fn execute_prepared(
    workspace: &Workspace,
    prepared: &PreparedPlan,
    selectors: Vec<String>,
    started_at: String,
) -> Result<(Report, String, String), WorkflowError> {
    let interrupt = InterruptGuard::install()?;
    let plan = &prepared.plan;
    let reservations = &prepared.reservations;
    let started = Instant::now();
    let mut reports = Vec::new();
    let mut child_stdout = String::new();
    let mut child_stderr = String::new();
    let mut interrupted = false;
    for (index, step) in plan.steps().iter().enumerate() {
        if interrupt.received() {
            interrupted = true;
            break;
        }
        let step_started = Instant::now();
        let output = run(step, reservations[index].as_ref(), &interrupt)?;
        child_stdout.push_str(&String::from_utf8_lossy(&output.stdout));
        child_stderr.push_str(&String::from_utf8_lossy(&output.stderr));
        reports.push(step_report(
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
            command: "spec".into(),
            selectors,
            started_at,
            duration: started.elapsed().as_millis() as u64,
            status,
            steps: reports,
        },
        child_stdout,
        child_stderr,
    ))
}

fn preflight(plan: &Plan) -> Result<Vec<Option<tempfile::TempDir>>, WorkflowError> {
    let mut tools = BTreeSet::new();
    let mut reservations = Vec::new();
    for step in plan.steps() {
        if !step.cwd.is_dir() {
            return Err(WorkflowError::Planning(format!(
                "spec cwd '{}' is not accessible",
                step.cwd.display()
            )));
        }
        for path in step.targets.iter().chain(&step.configuration_files) {
            if !path.is_file() {
                return Err(WorkflowError::Planning(format!(
                    "spec input or configuration '{}' is not accessible",
                    path.display()
                )));
            }
        }
        tools.insert(step.program.clone());
        let needs_temporary = matches!(step.provider.as_str(), "apalache" | "alloy");
        reservations.push(
            needs_temporary
                .then(|| {
                    tempfile::Builder::new()
                        .prefix(".just-spec-")
                        .tempdir()
                        .map_err(|error| {
                            WorkflowError::Planning(format!(
                                "cannot reserve specification output directory: {error}"
                            ))
                        })
                })
                .transpose()?,
        );
    }
    for tool in tools {
        if find_program(&tool).is_none() {
            return Err(WorkflowError::Planning(format!(
                "spec tool '{}' is not available",
                tool.to_string_lossy()
            )));
        }
    }
    Ok(reservations)
}

fn run(
    step: &Step,
    temporary: Option<&tempfile::TempDir>,
    interrupt: &InterruptGuard,
) -> Result<Output, WorkflowError> {
    let mut argv = step.argv.clone();
    if let Some(temporary) = temporary {
        for argument in &mut argv {
            if argument == OsStr::new(TEMPORARY) {
                temporary.path().as_os_str().clone_into(argument);
            } else if argument == OsStr::new(&format!("--out-dir={TEMPORARY}")) {
                *argument =
                    OsString::from(format!("--out-dir={}", temporary.path().to_string_lossy()));
            }
        }
    }
    let mut command = Command::new(&step.program);
    command
        .args(argv)
        .current_dir(&step.cwd)
        .envs(&step.env)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|error| {
        WorkflowError::Planning(format!(
            "could not launch spec tool '{}': {error}",
            step.program.to_string_lossy()
        ))
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| WorkflowError::Planning("spec tool stdout was not captured".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| WorkflowError::Planning("spec tool stderr was not captured".into()))?;
    let stdout_reader = std::thread::spawn(move || read_output(stdout));
    let stderr_reader = std::thread::spawn(move || read_output(stderr));
    loop {
        if interrupt.received() {
            #[cfg_attr(
                unix,
                expect(
                    clippy::unnecessary_mut_passed,
                    reason = "the shared call must also compile with the mutable non-Unix helper"
                )
            )]
            terminate(&mut child).map_err(|error| {
                WorkflowError::Planning(format!(
                    "could not stop interrupted spec tool '{}': {error}",
                    step.program.to_string_lossy()
                ))
            })?;
            let status = child.wait().map_err(|error| {
                WorkflowError::Planning(format!(
                    "could not wait for interrupted spec tool '{}': {error}",
                    step.program.to_string_lossy()
                ))
            })?;
            return collected_output(status, stdout_reader, stderr_reader);
        }
        match child.try_wait().map_err(|error| {
            WorkflowError::Planning(format!(
                "could not wait for spec tool '{}': {error}",
                step.program.to_string_lossy()
            ))
        })? {
            Some(_) => {
                let status = child.wait().map_err(|error| {
                    WorkflowError::Planning(format!(
                        "could not finish waiting for spec tool '{}': {error}",
                        step.program.to_string_lossy()
                    ))
                })?;
                return collected_output(status, stdout_reader, stderr_reader);
            }
            None => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
}

#[cfg(unix)]
fn terminate(child: &std::process::Child) -> std::io::Result<()> {
    let process_group = rustix::process::Pid::from_child(child);
    // The child was created as its own process group, so this also stops launcher descendants.
    match rustix::process::kill_process_group(process_group, rustix::process::Signal::KILL) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(error) => Err(std::io::Error::from_raw_os_error(error.raw_os_error())),
    }
}

#[cfg(not(unix))]
fn terminate(child: &mut std::process::Child) -> std::io::Result<()> {
    child.kill()
}

fn read_output(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    reader.read_to_end(&mut output)?;
    Ok(output)
}

fn collected_output(
    status: std::process::ExitStatus,
    stdout: std::thread::JoinHandle<std::io::Result<Vec<u8>>>,
    stderr: std::thread::JoinHandle<std::io::Result<Vec<u8>>>,
) -> Result<Output, WorkflowError> {
    let collect = |name: &str,
                   reader: std::thread::JoinHandle<std::io::Result<Vec<u8>>>|
     -> Result<Vec<u8>, WorkflowError> {
        reader
            .join()
            .map_err(|_| WorkflowError::Planning(format!("spec tool {name} reader panicked")))?
            .map_err(|error| {
                WorkflowError::Planning(format!("could not read spec tool {name}: {error}"))
            })
    };
    Ok(Output {
        status,
        stdout: collect("stdout", stdout)?,
        stderr: collect("stderr", stderr)?,
    })
}

fn step_report(workspace: &Workspace, step: &Step, output: &Output, duration: u64) -> StepReport {
    let mut argv = vec![step.program.to_string_lossy().into_owned()];
    argv.extend(
        step.argv
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned()),
    );
    let relative = |path: &Path| {
        path.strip_prefix(&workspace.root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    };
    StepReport {
        id: step.id.as_str().into(),
        capability: "spec_check".into(),
        provider: step.provider.as_str().into(),
        target: None,
        component: None,
        cwd: relative(&step.cwd),
        argv,
        configuration_files: step
            .configuration_files
            .iter()
            .map(|path| relative(path))
            .collect(),
        targets: step.targets.iter().map(|path| relative(path)).collect(),
        status: if output.status.success() {
            StepStatus::Passed
        } else {
            StepStatus::Failed
        },
        exit_code: output.status.code(),
        duration,
        skip_reason: None,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}
