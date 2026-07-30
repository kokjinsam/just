//! Formatting workflow planning, execution, and interruption control.

use std::collections::{BTreeMap, HashSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
#[cfg(unix)]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

use ignore::WalkBuilder;

use crate::capability::{Capability, CapabilityBinding, bindings_for_selections, is_oxfmt_source};
use crate::error::WorkflowError;
use crate::ownership::{Owner, SystemResource};
use crate::plan::{Plan, Step, StepId};
use crate::report::{Report, ReportStatus, StepReport, StepStatus};
use crate::selector::Selection;
use crate::workspace::Workspace;

/// Builds a formatting plan for selected resources.
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
    if !matches!(capability, Capability::Format | Capability::FormatCheck) {
        return Err(WorkflowError::Planning(
            "format planning requires a formatting capability".into(),
        ));
    }

    let bindings = bindings_for_selections(workspace, selections)?;
    let mut steps = Vec::new();
    for (index, binding) in bindings
        .into_iter()
        .filter(|binding| binding.capability == capability)
        .enumerate()
    {
        if let Some(step) = step_for_binding(workspace, selections, binding, index)? {
            steps.push(step);
        }
    }
    Plan::new(steps)
}

fn step_for_binding(
    workspace: &Workspace,
    selections: &[Selection],
    binding: CapabilityBinding,
    index: usize,
) -> Result<Option<Step>, WorkflowError> {
    let check = binding.capability == Capability::FormatCheck;
    let whole_binding = selections
        .iter()
        .any(|selection| selection_selects_whole_binding(selection, &binding));
    let targets = match binding.provider.as_str() {
        "mix-format" if !whole_binding => mix_targets(workspace, &binding, selections)?,
        "oxfmt" => oxfmt_targets(workspace, &binding, selections)?,
        _ => format_targets(&binding, selections),
    };
    if ((binding.provider.as_str() == "mix-format" && !whole_binding)
        || binding.provider.as_str() == "oxfmt")
        && targets.is_empty()
    {
        return Ok(None);
    }
    let (program, mut argv) = match binding.provider.as_str() {
        "mix-format" => (OsString::from("mix"), vec![OsString::from("format")]),
        "oxfmt" => (
            OsString::from("pnpm"),
            vec![OsString::from("exec"), OsString::from("oxfmt")],
        ),
        "rustfmt" => (OsString::from("cargo"), vec![OsString::from("fmt")]),
        "tla-format" => {
            let jar = tla2tools_jar()?;
            (
                OsString::from("java"),
                vec![
                    OsString::from("-cp"),
                    jar.into_os_string(),
                    OsString::from("formatter.Main"),
                    binding.evidence.clone().into_os_string(),
                    binding.evidence.clone().into_os_string(),
                ],
            )
        }
        provider => {
            return Err(WorkflowError::Planning(format!(
                "unsupported formatting provider '{provider}'"
            )));
        }
    };

    match binding.provider.as_str() {
        "mix-format" if check => argv.push(OsString::from("--check-formatted")),
        "oxfmt" if check => argv.push(OsString::from("--check")),
        "rustfmt" if check => {
            argv.push(OsString::from("--"));
            argv.push(OsString::from("--check"));
        }
        _ => {}
    }
    if binding.provider.as_str() == "oxfmt" {
        argv.push(OsString::from("--no-error-on-unmatched-pattern"));
    }
    if !targets.is_empty() && matches!(binding.provider.as_str(), "mix-format" | "oxfmt") {
        argv.push(OsString::from("--"));
    }
    if !matches!(binding.provider.as_str(), "rustfmt" | "tla-format") {
        argv.extend(targets.iter().map(|target| target.clone().into_os_string()));
    }

    Ok(Some(Step {
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
        program,
        argv,
        env: BTreeMap::new(),
        targets,
        configuration_files: binding.configuration_files,
        depends_on: Vec::new(),
    }))
}

fn mix_targets(
    workspace: &Workspace,
    binding: &CapabilityBinding,
    selections: &[Selection],
) -> Result<Vec<PathBuf>, WorkflowError> {
    let mut targets = Vec::new();
    for path in selections.iter().filter_map(|selection| match selection {
        Selection::Path(path) => Some(path),
        _ => None,
    }) {
        if path.is_file() && path.starts_with(&binding.root) && is_mix_source(path) {
            targets.push(relative_to(&binding.root, path));
            continue;
        }
        if !path.is_dir() || !path.starts_with(&binding.root) {
            continue;
        }

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
        let scope = path.clone();
        builder.filter_entry(move |entry| {
            entry.file_name() != ".git"
                && (entry.path().starts_with(&scope) || scope.starts_with(entry.path()))
        });
        for entry in builder.build() {
            let entry = entry.map_err(|error| {
                WorkflowError::Discovery(format!(
                    "cannot walk Mix format selection '{}': {error}",
                    path.display()
                ))
            })?;
            if entry
                .file_type()
                .is_some_and(|file_type| file_type.is_file())
                && entry.path().starts_with(path)
                && is_mix_source(entry.path())
            {
                let source = fs::canonicalize(entry.path()).map_err(|error| {
                    WorkflowError::Discovery(format!(
                        "cannot resolve Mix format source '{}': {error}",
                        entry.path().display()
                    ))
                })?;
                targets.push(relative_to(&binding.root, &source));
            }
        }
    }
    targets.sort();
    targets.dedup();
    Ok(targets)
}

fn is_mix_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(OsStr::to_str),
        Some("ex" | "exs" | "heex")
    )
}

fn oxfmt_targets(
    workspace: &Workspace,
    binding: &CapabilityBinding,
    selections: &[Selection],
) -> Result<Vec<PathBuf>, WorkflowError> {
    let mut targets = Vec::new();
    for selection in selections {
        let scope = match selection {
            Selection::Repository => binding.root.as_path(),
            Selection::Target(target)
                if owner_target(&binding.owner).is_some_and(|owner| owner == target) =>
            {
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
                if path == &binding.evidence
                    || path == &binding.root
                    || binding.root.starts_with(path) =>
            {
                binding.root.as_path()
            }
            Selection::Path(path) if path.starts_with(&binding.root) => path,
            _ => continue,
        };
        if scope.is_file() {
            if is_oxfmt_source(scope) && nearest_oxfmt_root(scope).as_deref() == Some(&binding.root)
            {
                targets.push(relative_to(&binding.root, scope));
            }
            continue;
        }
        if !scope.is_dir() {
            continue;
        }

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
        let filtered_scope = scope.to_path_buf();
        builder.filter_entry(move |entry| {
            entry.file_name() != ".git"
                && (entry.path().starts_with(&filtered_scope)
                    || filtered_scope.starts_with(entry.path()))
        });
        for entry in builder.build() {
            let entry = entry.map_err(|error| {
                WorkflowError::Discovery(format!(
                    "cannot walk Oxfmt selection '{}': {error}",
                    scope.display()
                ))
            })?;
            if !entry
                .file_type()
                .is_some_and(|file_type| file_type.is_file())
                || !entry.path().starts_with(scope)
                || !is_oxfmt_source(entry.path())
            {
                continue;
            }
            let source = fs::canonicalize(entry.path()).map_err(|error| {
                WorkflowError::Discovery(format!(
                    "cannot resolve Oxfmt source '{}': {error}",
                    entry.path().display()
                ))
            })?;
            if nearest_oxfmt_root(&source).as_deref() == Some(&binding.root) {
                targets.push(relative_to(&binding.root, &source));
            }
        }
    }
    targets.sort();
    targets.dedup();
    Ok(targets)
}

fn nearest_oxfmt_root(path: &Path) -> Option<PathBuf> {
    let mut ancestor = path.parent();
    while let Some(root) = ancestor {
        let manifest = root.join("package.json");
        if package_has_oxfmt(&manifest) {
            return Some(root.to_path_buf());
        }
        ancestor = root.parent();
    }
    None
}

fn package_has_oxfmt(manifest: &Path) -> bool {
    let Ok(source) = fs::read_to_string(manifest) else {
        return false;
    };
    let Ok(package) = serde_json::from_str::<serde_json::Value>(&source) else {
        return false;
    };
    ["devDependencies", "dependencies"].iter().any(|field| {
        package
            .get(field)
            .and_then(|dependencies| dependencies.get("oxfmt"))
            .is_some()
    })
}

fn relative_to(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

fn format_targets(binding: &CapabilityBinding, selections: &[Selection]) -> Vec<PathBuf> {
    if binding.provider.as_str() == "tla-format" {
        return vec![binding.evidence.clone()];
    }
    if binding.provider.as_str() == "rustfmt"
        || selections
            .iter()
            .any(|selection| selection_selects_whole_binding(selection, binding))
    {
        return Vec::new();
    }

    let mut seen = HashSet::new();
    selections
        .iter()
        .filter_map(|selection| match selection {
            Selection::Path(path)
                if path != &binding.evidence
                    && path != &binding.root
                    && path.starts_with(&binding.root) =>
            {
                path.strip_prefix(&binding.root).ok().map(Path::to_path_buf)
            }
            _ => None,
        })
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

fn selection_selects_whole_binding(selection: &Selection, binding: &CapabilityBinding) -> bool {
    match selection {
        Selection::Repository => true,
        Selection::Target(target) => {
            owner_target(&binding.owner).is_some_and(|owner| owner == target)
        }
        Selection::Component { target, component } => {
            binding.owner
                == (Owner::Component {
                    target: target.clone(),
                    component: Some(component.clone()),
                })
        }
        Selection::Path(path) => {
            path == &binding.root || path == &binding.evidence || binding.root.starts_with(path)
        }
    }
}

/// Executes a formatting plan.
///
/// # Errors
///
/// Returns an error when preflight, provider execution, temporary-file
/// handling, or reporting fails.
pub fn execute_plan(
    workspace: &Workspace,
    plan: &Plan,
    selectors: Vec<String>,
    started_at: String,
) -> Result<(Report, String, String), WorkflowError> {
    let interrupt = InterruptGuard::install()?;
    let reservations = preflight(plan)?;
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
        let step_started = Instant::now();
        let output = if step.provider.as_str() == "tla-format" {
            let reservation = reservations
                .iter()
                .find(|reservation| reservation.step_id == step.id)
                .ok_or_else(|| {
                    WorkflowError::Planning("TLA+ formatter reservation is missing".into())
                })?;
            run_tla_step(step, &reservation.path)?
        } else {
            run_command(step)?
        };
        aggregate_stdout.push_str(&String::from_utf8_lossy(&output.stdout));
        aggregate_stderr.push_str(&String::from_utf8_lossy(&output.stderr));
        let step_interrupted = interrupt.received() || was_interrupted(&output);
        reports.push(step_report(
            workspace,
            step,
            &output,
            step_started.elapsed().as_millis() as u64,
        ));
        if step_interrupted {
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
            command: "format".into(),
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

#[cfg(unix)]
pub(crate) struct InterruptGuard {
    received: Arc<AtomicBool>,
    registration: signal_hook::SigId,
}

#[cfg(unix)]
impl InterruptGuard {
    pub(crate) fn install() -> Result<Self, WorkflowError> {
        let received = Arc::new(AtomicBool::new(false));
        let registration =
            signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&received))
                .map_err(|error| {
                    WorkflowError::Planning(format!("cannot install interruption handler: {error}"))
                })?;
        Ok(Self {
            received,
            registration,
        })
    }

    pub(crate) fn received(&self) -> bool {
        self.received.load(Ordering::Relaxed)
    }
}

#[cfg(unix)]
impl Drop for InterruptGuard {
    fn drop(&mut self) {
        signal_hook::low_level::unregister(self.registration);
    }
}

#[cfg(not(unix))]
pub(crate) struct InterruptGuard;

#[cfg(not(unix))]
impl InterruptGuard {
    pub(crate) fn install() -> Result<Self, WorkflowError> {
        Ok(Self)
    }

    pub(crate) const fn received(&self) -> bool {
        false
    }
}

#[derive(Debug)]
struct TlaReservation {
    step_id: StepId,
    path: PathBuf,
}

impl Drop for TlaReservation {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn preflight(plan: &Plan) -> Result<Vec<TlaReservation>, WorkflowError> {
    let mut reservations = Vec::new();
    for step in plan.steps() {
        if !step.cwd.is_dir() {
            return Err(WorkflowError::Planning(format!(
                "formatter working directory '{}' is not accessible",
                step.cwd.display()
            )));
        }
        if find_program(&step.program).is_none() {
            return Err(WorkflowError::Planning(format!(
                "formatter tool '{}' is not available",
                step.program.to_string_lossy()
            )));
        }
        if step.provider.as_str() == "tla-format" {
            let Some(jar) = step.argv.get(1).map(PathBuf::from) else {
                return Err(WorkflowError::Planning(
                    "TLA+ formatter classpath is missing".into(),
                ));
            };
            if !jar.is_file() {
                return Err(WorkflowError::Planning(format!(
                    "TLA+ formatter jar '{}' is not accessible",
                    jar.display()
                )));
            }
            let source = step.targets.first().ok_or_else(|| {
                WorkflowError::Planning("TLA+ formatter source is missing".into())
            })?;
            if !source.is_file() {
                return Err(WorkflowError::Planning(format!(
                    "TLA+ source '{}' is not accessible",
                    source.display()
                )));
            }
            reservations.push(TlaReservation {
                step_id: step.id.clone(),
                path: reserve_temporary(source)?,
            });
        }
    }
    Ok(reservations)
}

pub(crate) fn find_program(program: &OsStr) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return executable(path).then(|| path.to_path_buf());
    }
    env::var_os("PATH")
        .into_iter()
        .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(program))
        .find(|candidate| executable(candidate))
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn run_command(step: &Step) -> Result<Output, WorkflowError> {
    let mut command = Command::new(&step.program);
    command.args(&step.argv).current_dir(&step.cwd);
    command.envs(&step.env);
    command.output().map_err(|error| {
        WorkflowError::Planning(format!(
            "could not execute formatter '{}': {error}",
            step.program.to_string_lossy()
        ))
    })
}

fn run_tla_step(step: &Step, temporary: &Path) -> Result<Output, WorkflowError> {
    let source = step
        .targets
        .first()
        .ok_or_else(|| WorkflowError::Planning("TLA+ formatter source is missing".into()))?;
    let mut argv = step.argv.clone();
    let output_argument = argv.last_mut().ok_or_else(|| {
        WorkflowError::Planning("TLA+ formatter output argument is missing".into())
    })?;
    temporary.as_os_str().clone_into(output_argument);

    let mut command = Command::new(&step.program);
    let output = command
        .args(&argv)
        .current_dir(&step.cwd)
        .envs(&step.env)
        .output()
        .map_err(|error| {
            WorkflowError::Planning(format!("could not execute TLA+ formatter: {error}"))
        })?;

    if !output.status.success() {
        return Ok(output);
    }

    if step.capability == Capability::FormatCheck {
        let equal = fs::read(source)
            .and_then(|original| fs::read(temporary).map(|formatted| original == formatted));
        Ok(match equal {
            Ok(equal) => {
                if equal {
                    output
                } else {
                    failed_output(output, 1)
                }
            }
            Err(error) => execution_failure(output, &error),
        })
    } else {
        let replacement = fs::metadata(source)
            .and_then(|metadata| fs::set_permissions(temporary, metadata.permissions()))
            .and_then(|()| fs::rename(temporary, source));
        match replacement {
            Ok(()) => Ok(output),
            Err(error) => Ok(execution_failure(output, &error)),
        }
    }
}

fn reserve_temporary(source: &Path) -> Result<PathBuf, WorkflowError> {
    let directory = source.parent().unwrap_or_else(|| Path::new(""));
    let name = source
        .file_name()
        .unwrap_or_else(|| OsStr::new("source"))
        .to_string_lossy();
    for sequence in 0..1000_u32 {
        let candidate = directory.join(format!(
            ".{name}.just-format-{}-{sequence}.tmp",
            std::process::id()
        ));
        match create_reservation(&candidate, source) {
            Ok(_) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(WorkflowError::Planning(format!(
                    "cannot create same-directory TLA+ temporary file for '{}': {error}",
                    source.display()
                )));
            }
        }
    }
    Err(WorkflowError::Planning(format!(
        "cannot allocate a TLA+ temporary file for '{}'",
        source.display()
    )))
}

#[cfg(unix)]
fn create_reservation(candidate: &Path, source: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mode = fs::metadata(source)?.permissions().mode() & 0o777;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(candidate)
}

#[cfg(not(unix))]
fn create_reservation(candidate: &Path, _source: &Path) -> io::Result<fs::File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(candidate)
}

#[cfg(unix)]
pub(crate) fn was_interrupted(output: &Output) -> bool {
    use std::os::unix::process::ExitStatusExt;

    output.status.signal() == Some(2)
}

#[cfg(not(unix))]
pub(crate) fn was_interrupted(_output: &Output) -> bool {
    false
}

#[cfg(unix)]
fn failed_output(mut output: Output, code: i32) -> Output {
    use std::os::unix::process::ExitStatusExt;
    output.status = std::process::ExitStatus::from_raw(code << 8);
    output
}

fn execution_failure(mut output: Output, error: &io::Error) -> Output {
    use std::io::Write;

    let _ = writeln!(output.stderr, "TLA+ formatter finalization failed: {error}");
    failed_output(output, 1)
}

#[cfg(windows)]
fn failed_output(mut output: Output, code: i32) -> Output {
    use std::os::windows::process::ExitStatusExt;
    output.status = std::process::ExitStatus::from_raw(code as u32);
    output
}

fn step_report(workspace: &Workspace, step: &Step, output: &Output, duration: u64) -> StepReport {
    let (target, component) = owner_labels(&step.owner);
    let mut argv = vec![step.program.to_string_lossy().into_owned()];
    argv.extend(
        step.argv
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned()),
    );
    StepReport {
        id: step.id.as_str().into(),
        capability: capability_name(step.capability).into(),
        provider: step.provider.as_str().into(),
        target,
        component,
        cwd: relative(workspace, &step.cwd)
            .to_string_lossy()
            .into_owned(),
        argv,
        configuration_files: step
            .configuration_files
            .iter()
            .map(|path| relative(workspace, path).to_string_lossy().into_owned())
            .collect(),
        targets: step
            .targets
            .iter()
            .map(|path| {
                if path.is_absolute() {
                    relative(workspace, path).to_string_lossy().into_owned()
                } else {
                    path.to_string_lossy().into_owned()
                }
            })
            .collect(),
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

fn owner_labels(owner: &Owner) -> (Option<String>, Option<String>) {
    match owner {
        Owner::Target(target) => (Some(target.clone()), None),
        Owner::Component { target, component } => (Some(target.clone()), component.clone()),
        Owner::Repository(
            SystemResource::Repository
            | SystemResource::Specifications
            | SystemResource::ApplicationTests,
        ) => (None, None),
    }
}

fn owner_target(owner: &Owner) -> Option<&str> {
    match owner {
        Owner::Target(target) | Owner::Component { target, .. } => Some(target),
        Owner::Repository(_) => None,
    }
}

fn relative<'a>(workspace: &'a Workspace, path: &'a Path) -> &'a Path {
    path.strip_prefix(&workspace.root).unwrap_or(path)
}

const fn capability_name(capability: Capability) -> &'static str {
    match capability {
        Capability::Format => "format",
        Capability::FormatCheck => "format_check",
        Capability::Lint => "lint",
        Capability::Typecheck => "typecheck",
        Capability::Compile => "compile",
        Capability::TargetTest => "target_test",
        Capability::Coverage => "coverage",
        Capability::SpecCheck => "spec_check",
        Capability::ApplicationTest => "application_test",
        Capability::DoctorCheck => "doctor_check",
    }
}

fn tla2tools_jar() -> Result<PathBuf, WorkflowError> {
    let path = env::var_os("TLA2TOOLS_JAR").ok_or_else(|| {
        WorkflowError::Planning(
            "cannot find tla2tools.jar; set TLA2TOOLS_JAR for TLA+ formatting".into(),
        )
    })?;
    let path = PathBuf::from(path);
    fs::canonicalize(&path).map_err(|error| {
        WorkflowError::Planning(format!(
            "TLA2TOOLS_JAR '{}' is not an accessible file: {error}",
            path.display()
        ))
    })
}
