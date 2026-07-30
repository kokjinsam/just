//! Application-test discovery, planning, lifecycle control, and reporting.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ignore::{WalkBuilder, gitignore::GitignoreBuilder};

use crate::capability::{Capability, ProviderId};
use crate::checking;
use crate::error::WorkflowError;
use crate::formatting::{InterruptGuard, find_program, was_interrupted};
use crate::ownership::{Owner, SystemResource};
use crate::plan::{Plan, Step, StepId};
use crate::report::{Report, ReportStatus, StepReport, StepStatus};
use crate::selector::Selection;
use crate::target_testing::{self, TestMode};
use crate::workspace::Workspace;

const READINESS_TIMEOUT: Duration = Duration::from_mins(1);
const CONTROL_WAIT: Duration = Duration::from_secs(2);
static RUN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Supported application-test suites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ApplicationSuite {
    /// OpenAPI contract tests.
    Contract,
    /// End-to-end browser tests.
    E2e,
    /// Visual regression tests.
    Visual,
    /// UI property tests.
    UiProperty,
    /// Aggregate application suite.
    Application,
}

impl ApplicationSuite {
    /// Leaf suites included by the aggregate application suite.
    pub const SHORT: [Self; 4] = [Self::Contract, Self::E2e, Self::Visual, Self::UiProperty];

    /// Parses a CLI suite name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "contract" => Some(Self::Contract),
            "e2e" => Some(Self::E2e),
            "visual" => Some(Self::Visual),
            "ui-property" => Some(Self::UiProperty),
            "application" => Some(Self::Application),
            _ => None,
        }
    }

    /// Returns the stable CLI suite name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Contract => "contract",
            Self::E2e => "e2e",
            Self::Visual => "visual",
            Self::UiProperty => "ui-property",
            Self::Application => "application",
        }
    }
}

/// Discovered application-test configuration and evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationDiscovery {
    configured: Vec<ApplicationSuite>,
    openapi_documents: Vec<PathBuf>,
}

/// Prepared application-test plan and run state.
#[derive(Debug)]
pub struct ApplicationPlan {
    plan: Plan,
    preparation_count: usize,
    suite_count: usize,
    state: ApplicationRunState,
}

impl ApplicationPlan {
    /// Returns all preparation and suite steps.
    #[must_use]
    pub fn steps(&self) -> &[Step] {
        self.plan.steps()
    }

    /// Returns the retained artifact directory.
    #[must_use]
    pub fn artifact_dir(&self) -> &Path {
        &self.state.artifact_dir
    }
}

#[derive(Debug)]
struct ApplicationRunState {
    run_id: String,
    project: String,
    artifact_dir: PathBuf,
    _control: tempfile::TempDir,
    env_file: PathBuf,
    socket: PathBuf,
    api_port: u16,
    web_port: u16,
    postgres_port: u16,
    reservations: Option<[TcpListener; 3]>,
}

impl ApplicationDiscovery {
    /// Returns configured leaf suites in deterministic order.
    #[must_use]
    pub fn configured_suites(&self) -> &[ApplicationSuite] {
        &self.configured
    }

    /// Returns discovered OpenAPI documents.
    #[must_use]
    pub fn openapi_documents(&self) -> &[PathBuf] {
        &self.openapi_documents
    }

    /// Returns whether `suite` is configured.
    #[must_use]
    pub fn is_configured(&self, suite: ApplicationSuite) -> bool {
        self.configured.contains(&suite)
    }
}

/// Discovers configured application-test suites and evidence.
///
/// # Errors
///
/// Returns an error when configured tests or specifications cannot be
/// discovered or validated.
pub fn discover(workspace: &Workspace) -> Result<ApplicationDiscovery, WorkflowError> {
    let visible = visible_files(workspace)?;
    let (openapi_documents, contract_ready) = discover_contract(workspace, &visible)?;
    let [e2e_ready, visual_ready, bombadil_ready] = discover_node_suites(workspace, &visible)?;
    let shared_ready = shared_environment_ready(workspace, &visible)?;

    let mut configured = Vec::new();
    if contract_ready {
        configured.push(ApplicationSuite::Contract);
    }
    if e2e_ready {
        configured.push(ApplicationSuite::E2e);
    }
    if visual_ready {
        configured.push(ApplicationSuite::Visual);
    }
    if bombadil_ready {
        configured.push(ApplicationSuite::UiProperty);
    }
    if !configured.is_empty() && !shared_ready {
        return Err(WorkflowError::Configuration(
            "configured application suites require the shared application environment".into(),
        ));
    }

    Ok(ApplicationDiscovery {
        configured,
        openapi_documents,
    })
}

fn visible_path(visible: &BTreeSet<PathBuf>, path: &Path) -> bool {
    fs::canonicalize(path)
        .ok()
        .is_some_and(|path| visible.contains(&path))
}

fn discover_contract(
    workspace: &Workspace,
    visible: &BTreeSet<PathBuf>,
) -> Result<(Vec<PathBuf>, bool), WorkflowError> {
    let openapi_root = workspace.config.system.specs.join("openapi");
    let mut documents = visible
        .iter()
        .filter(|path| path.starts_with(&openapi_root))
        .filter(|path| {
            path.file_name()
                .is_none_or(|name| name != "vacuum.conf.yaml")
        })
        .filter(|path| {
            matches!(
                path.extension().and_then(OsStr::to_str),
                Some("yaml" | "yml" | "json")
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    documents.sort();
    let tests = &workspace.config.system.tests;
    let pyproject = tests.join("pyproject.toml");
    let uv_lock = tests.join("uv.lock");
    let has_pyproject = visible_path(visible, &pyproject);
    let declares_schemathesis = has_pyproject && toml_declares(&pyproject, "schemathesis")?;
    let has_uv_lock = visible_path(visible, &uv_lock);
    let evidence = !documents.is_empty() || has_pyproject || declares_schemathesis || has_uv_lock;
    let ready = !documents.is_empty() && declares_schemathesis && has_uv_lock;
    reject_partial(
        "contract",
        evidence,
        ready,
        "requires visible OpenAPI documents, pyproject.toml declaring Schemathesis, and uv.lock",
    )?;
    Ok((documents, ready))
}

fn discover_node_suites(
    workspace: &Workspace,
    visible: &BTreeSet<PathBuf>,
) -> Result<[bool; 3], WorkflowError> {
    let tests = &workspace.config.system.tests;
    let package_path = tests.join("package.json");
    let package = visible_path(visible, &package_path)
        .then(|| read_json(&package_path))
        .transpose()?;
    let playwright = package
        .as_ref()
        .and_then(|package| dependency(package, "@playwright/test"))
        .is_some();
    let config_visible = visible_path(visible, &tests.join("playwright.config.ts"));
    let suite_ready = |suite: &str| {
        let files = recognized_test_files(visible, &tests.join(suite));
        (
            playwright || config_visible || !files.is_empty(),
            playwright && config_visible && !files.is_empty(),
        )
    };
    let (e2e_evidence, e2e_ready) = suite_ready("e2e");
    reject_partial(
        "e2e",
        e2e_evidence,
        e2e_ready,
        "requires @playwright/test, playwright.config.ts, and a recognized e2e test file",
    )?;
    let (visual_evidence, visual_ready) = suite_ready("visual");
    reject_partial(
        "visual",
        visual_evidence,
        visual_ready,
        "requires @playwright/test, playwright.config.ts, and a recognized visual test file",
    )?;
    let bombadil_version = package
        .as_ref()
        .and_then(|package| dependency(package, "@antithesishq/bombadil"));
    let bombadil_spec_visible = visible_path(visible, &tests.join("bombadil/specification.ts"));
    let bombadil_ready = bombadil_version.is_some_and(exact_version) && bombadil_spec_visible;
    reject_partial(
        "ui-property",
        bombadil_version.is_some() || bombadil_spec_visible,
        bombadil_ready,
        "requires an exactly pinned @antithesishq/bombadil dependency and bombadil/specification.ts",
    )?;
    Ok([e2e_ready, visual_ready, bombadil_ready])
}

fn shared_environment_ready(
    workspace: &Workspace,
    visible: &BTreeSet<PathBuf>,
) -> Result<bool, WorkflowError> {
    let tests = &workspace.config.system.tests;
    let process_compose_visible =
        visible_path(visible, &workspace.root.join("process-compose.yaml"));
    let artifacts_ignored = ignored_by_repository(workspace, &tests.join("artifacts"))?;
    let venv_ignored = ignored_by_repository(workspace, &tests.join(".venv"))?;
    let ready = process_compose_visible && artifacts_ignored && venv_ignored;
    reject_partial(
        "application environment",
        process_compose_visible || artifacts_ignored || venv_ignored,
        ready,
        "requires root process-compose.yaml and Git-ignore coverage for system.tests/artifacts/ and .venv/",
    )?;
    Ok(ready)
}

/// Builds an application-test plan.
///
/// # Errors
///
/// Returns an error when the requested suite, passthrough arguments,
/// preparation steps, or provider configuration is invalid.
pub fn build_plan(
    workspace: &Workspace,
    requested: ApplicationSuite,
    passthrough: &[OsString],
) -> Result<ApplicationPlan, WorkflowError> {
    let discovery = discover(workspace)?;
    let suites = match requested {
        ApplicationSuite::Application => {
            if discovery.configured.is_empty() {
                return Err(WorkflowError::Configuration(
                    "application test suite requires at least one configured application suite"
                        .into(),
                ));
            }
            discovery.configured.clone()
        }
        suite if discovery.is_configured(suite) => vec![suite],
        suite => {
            return Err(WorkflowError::Configuration(format!(
                "{} application-test suite is not configured",
                suite.as_str()
            )));
        }
    };
    let action_count = suites
        .iter()
        .map(|suite| match suite {
            ApplicationSuite::Contract => discovery.openapi_documents.len(),
            ApplicationSuite::E2e | ApplicationSuite::Visual | ApplicationSuite::UiProperty => 1,
            ApplicationSuite::Application => 0,
        })
        .sum::<usize>();
    if !passthrough.is_empty() && action_count != 1 {
        return Err(WorkflowError::Usage(format!(
            "test passthrough requires exactly one executable suite action; resolved {action_count}"
        )));
    }
    if !passthrough.is_empty() {
        let suite = suites
            .iter()
            .copied()
            .find(|suite| match suite {
                ApplicationSuite::Contract => discovery.openapi_documents.len() == 1,
                ApplicationSuite::E2e | ApplicationSuite::Visual | ApplicationSuite::UiProperty => {
                    true
                }
                ApplicationSuite::Application => false,
            })
            .ok_or_else(|| {
                WorkflowError::Planning(
                    "resolved application passthrough action has no provider".into(),
                )
            })?;
        reject_owned_passthrough(suite, passthrough)?;
    }

    let state = ApplicationRunState::new(workspace)?;
    state.write_env(workspace)?;
    let mut preparation = preparation_steps(workspace)?;
    let preparation_count = preparation.len();
    let mut suite_steps = suite_steps(workspace, &discovery, &suites, &state)?;
    if let Some(step) = suite_steps.first_mut().filter(|_| !passthrough.is_empty()) {
        step.argv.extend_from_slice(passthrough);
    }
    let suite_count = suite_steps.len();
    preparation.extend(suite_steps);
    Ok(ApplicationPlan {
        plan: Plan::new(preparation)?,
        preparation_count,
        suite_count,
        state,
    })
}

/// Executes an application plan without a target-test prerequisite.
///
/// # Errors
///
/// Returns an error when lifecycle setup, provider execution, cleanup, or
/// reporting fails.
pub fn execute_plan(
    workspace: &Workspace,
    plan: &mut ApplicationPlan,
    selectors: Vec<String>,
    started_at: String,
) -> Result<(Report, String, String), WorkflowError> {
    execute_with_target(workspace, plan, None, selectors, started_at)
}

/// Executes an application plan after an optional target-test plan.
///
/// # Errors
///
/// Returns an error when target execution, application lifecycle control,
/// provider execution, cleanup, or reporting fails.
pub fn execute_with_target(
    workspace: &Workspace,
    plan: &mut ApplicationPlan,
    target: Option<&Plan>,
    selectors: Vec<String>,
    started_at: String,
) -> Result<(Report, String, String), WorkflowError> {
    static_preflight(workspace, plan, target)?;
    let started = Instant::now();
    let interrupt = InterruptGuard::install()?;
    let mut reports = Vec::new();
    let mut aggregate_stdout = String::new();
    let mut aggregate_stderr = String::new();

    if !run_dry_run(
        workspace,
        plan,
        &mut reports,
        &mut aggregate_stdout,
        &mut aggregate_stderr,
    )? {
        return Ok(finish_report(
            reports,
            selectors,
            started_at,
            started,
            false,
            aggregate_stdout,
            aggregate_stderr,
        ));
    }

    if run_target_plan(
        workspace,
        target,
        &interrupt,
        &started_at,
        &mut reports,
        &mut aggregate_stdout,
        &mut aggregate_stderr,
    )? {
        return Ok(finish_report(
            reports,
            selectors,
            started_at,
            started,
            true,
            aggregate_stdout,
            aggregate_stderr,
        ));
    }

    match run_preparation(
        workspace,
        plan,
        &interrupt,
        &mut reports,
        &mut aggregate_stdout,
        &mut aggregate_stderr,
    )? {
        PreparationStatus::Ready => {}
        PreparationStatus::Failed => {
            return Ok(finish_report(
                reports,
                selectors,
                started_at,
                started,
                false,
                aggregate_stdout,
                aggregate_stderr,
            ));
        }
        PreparationStatus::Interrupted => {
            return Ok(finish_report(
                reports,
                selectors,
                started_at,
                started,
                true,
                aggregate_stdout,
                aggregate_stderr,
            ));
        }
    }

    run_environment(EnvironmentRun {
        workspace,
        plan,
        interrupt: &interrupt,
        selectors,
        started_at,
        started,
        reports,
        stdout: aggregate_stdout,
        stderr: aggregate_stderr,
    })
}

struct EnvironmentRun<'a> {
    workspace: &'a Workspace,
    plan: &'a mut ApplicationPlan,
    interrupt: &'a InterruptGuard,
    selectors: Vec<String>,
    started_at: String,
    started: Instant,
    reports: Vec<StepReport>,
    stdout: String,
    stderr: String,
}

fn run_environment(run: EnvironmentRun<'_>) -> Result<(Report, String, String), WorkflowError> {
    let EnvironmentRun {
        workspace,
        plan,
        interrupt,
        selectors,
        started_at,
        started,
        mut reports,
        mut stdout,
        mut stderr,
    } = run;
    plan.state.reservations.take();
    let mut environment = ProcessCompose::start(workspace, &plan.state)?;
    reports.push(environment.started_report(workspace));
    let steps =
        &plan.plan.steps()[plan.preparation_count..plan.preparation_count + plan.suite_count];
    let mut accounted_for = 0;
    let operational = run_suites(&mut SuiteRun {
        workspace,
        state: &plan.state,
        interrupt,
        environment: &mut environment,
        steps,
        accounted_for: &mut accounted_for,
        reports: &mut reports,
        stdout: &mut stdout,
        stderr: &mut stderr,
    });
    if let Err(error) = operational {
        reports.push(controller_error_report(workspace, &error));
        reports.extend(steps[accounted_for..].iter().map(|step| {
            skipped_step_report(
                workspace,
                step,
                "application controller failed after the environment started",
            )
        }));
    } else if interrupt.received() {
        reports.extend(steps[accounted_for..].iter().map(|step| {
            skipped_step_report(
                workspace,
                step,
                "application test execution was interrupted",
            )
        }));
    }
    reports.push(collection_report(workspace, &plan.state));
    let cleanup = environment.cleanup(workspace, &plan.state, interrupt);
    append_output(&mut stdout, &mut stderr, &cleanup.output);
    reports.push(cleanup.report);
    Ok(finish_report(
        reports,
        selectors,
        started_at,
        started,
        interrupt.received(),
        stdout,
        stderr,
    ))
}

struct SuiteRun<'a> {
    workspace: &'a Workspace,
    state: &'a ApplicationRunState,
    interrupt: &'a InterruptGuard,
    environment: &'a mut ProcessCompose,
    steps: &'a [Step],
    accounted_for: &'a mut usize,
    reports: &'a mut Vec<StepReport>,
    stdout: &'a mut String,
    stderr: &'a mut String,
}

fn run_suites(run: &mut SuiteRun<'_>) -> Result<(), WorkflowError> {
    let readiness = run
        .environment
        .wait_ready(run.workspace, run.state, run.interrupt)?;
    append_output(run.stdout, run.stderr, &readiness.output);
    run.reports.push(readiness.report);
    if !readiness.ready {
        run.reports.extend(run.steps.iter().map(|step| {
            skipped_step_report(
                run.workspace,
                step,
                "Process Compose environment did not become ready",
            )
        }));
        *run.accounted_for = run.steps.len();
        return Ok(());
    }
    for (index, step) in run.steps.iter().enumerate() {
        if run.interrupt.received() {
            break;
        }
        if !run.environment.healthy()? {
            run.reports.push(environment_health_report(
                run.workspace,
                "Process Compose environment exited before all suites ran",
            ));
            run.reports
                .extend(run.steps[index..].iter().map(|remaining| {
                    skipped_step_report(
                        run.workspace,
                        remaining,
                        "Process Compose environment exited before the suite ran",
                    )
                }));
            *run.accounted_for = run.steps.len();
            break;
        }
        let started = Instant::now();
        let output = run_owned(step, run.interrupt, "application-test provider")?;
        append_output(run.stdout, run.stderr, &output);
        write_provider_logs(&run.state.artifact_dir, step.id.as_str(), &output)?;
        run.reports.push(step_report(
            run.workspace,
            step,
            &output,
            started.elapsed().as_millis() as u64,
        ));
        *run.accounted_for = index + 1;
        if run.interrupt.received() || was_interrupted(&output) {
            break;
        }
    }
    if !run.interrupt.received()
        && !run.environment.healthy()?
        && !run
            .reports
            .iter()
            .any(|report| report.provider == "process-compose-health")
    {
        run.reports.push(environment_health_report(
            run.workspace,
            "Process Compose environment exited while suites were running",
        ));
    }
    Ok(())
}

fn run_target_plan(
    workspace: &Workspace,
    target: Option<&Plan>,
    interrupt: &InterruptGuard,
    started_at: &str,
    reports: &mut Vec<StepReport>,
    stdout: &mut String,
    stderr: &mut String,
) -> Result<bool, WorkflowError> {
    let Some(target) = target else {
        return Ok(false);
    };
    let (target_report, target_stdout, target_stderr) = target_testing::execute_plan(
        workspace,
        target,
        Vec::new(),
        started_at.to_owned(),
        TestMode::Test,
    )?;
    stdout.push_str(&target_stdout);
    stderr.push_str(&target_stderr);
    let interrupted = target_report.status == ReportStatus::Interrupted || interrupt.received();
    reports.extend(target_report.steps);
    Ok(interrupted)
}

fn run_dry_run(
    workspace: &Workspace,
    plan: &ApplicationPlan,
    reports: &mut Vec<StepReport>,
    stdout: &mut String,
    stderr: &mut String,
) -> Result<bool, WorkflowError> {
    let dry = process_compose_output(workspace, &plan.state, &["up", "--dry-run"])?;
    append_output(stdout, stderr, &dry);
    reports.push(lifecycle_report(
        workspace,
        "application:environment:dry-run",
        "process-compose-dry-run",
        &dry,
        &process_compose_argv(workspace, &plan.state, &["up", "--dry-run"]),
        0,
    ));
    if dry.status.success() {
        Ok(true)
    } else {
        reports.extend(skipped_application_reports(
            workspace,
            plan,
            "Process Compose dry-run validation failed",
        ));
        Ok(false)
    }
}

enum PreparationStatus {
    Ready,
    Failed,
    Interrupted,
}

fn run_preparation(
    workspace: &Workspace,
    plan: &ApplicationPlan,
    interrupt: &InterruptGuard,
    reports: &mut Vec<StepReport>,
    stdout: &mut String,
    stderr: &mut String,
) -> Result<PreparationStatus, WorkflowError> {
    let mut failed = false;
    for step in &plan.plan.steps()[..plan.preparation_count] {
        let started = Instant::now();
        let output = run_owned(step, interrupt, "application preparation")?;
        append_output(stdout, stderr, &output);
        write_provider_logs(&plan.state.artifact_dir, step.id.as_str(), &output)?;
        reports.push(step_report(
            workspace,
            step,
            &output,
            started.elapsed().as_millis() as u64,
        ));
        failed |= !output.status.success();
        if interrupt.received() || was_interrupted(&output) {
            return Ok(PreparationStatus::Interrupted);
        }
    }
    if failed {
        reports.extend(skipped_suite_reports(
            workspace,
            plan,
            "application preparation failed; environment and suites were skipped",
        ));
        Ok(PreparationStatus::Failed)
    } else {
        Ok(PreparationStatus::Ready)
    }
}

fn preparation_steps(workspace: &Workspace) -> Result<Vec<Step>, WorkflowError> {
    let compile = checking::build_plan(workspace, &[Selection::Repository], Capability::Compile)?;
    compile
        .steps()
        .iter()
        .filter(|step| {
            matches!(step.owner, Owner::Target(_) | Owner::Component { .. })
                && step.provider.as_str() != "not-applicable"
        })
        .enumerate()
        .map(|(index, source)| {
            let mut step = source.clone();
            step.id = StepId::new(format!(
                "application:prepare:{index:04}:{}",
                step.provider.as_str()
            ));
            step.label = format!("application preparation {}", step.provider.as_str());
            if step.provider.as_str() == "mix-compile" {
                step.env
                    .insert(OsString::from("MIX_ENV"), OsString::from("test"));
            }
            if step.provider.as_str() == "cargo-check" {
                step.provider = ProviderId::new("cargo-build");
                step.argv = vec![OsString::from("build")];
            }
            Ok(step)
        })
        .collect()
}

fn suite_steps(
    workspace: &Workspace,
    discovery: &ApplicationDiscovery,
    suites: &[ApplicationSuite],
    state: &ApplicationRunState,
) -> Result<Vec<Step>, WorkflowError> {
    let tests = &workspace.config.system.tests;
    let owner = Owner::Repository(SystemResource::ApplicationTests);
    let mut steps = Vec::new();
    for suite in suites {
        match suite {
            ApplicationSuite::Contract => {
                steps.extend(contract_steps(workspace, discovery, state, &owner)?);
            }
            ApplicationSuite::E2e | ApplicationSuite::Visual => {
                let output = state.artifact_dir.join(suite.as_str()).join("output");
                let html = state.artifact_dir.join(suite.as_str()).join("html");
                fs::create_dir_all(&html).map_err(|error| {
                    WorkflowError::Planning(format!(
                        "cannot create Playwright artifact directory '{}': {error}",
                        html.display()
                    ))
                })?;
                let mut env = state.environment();
                env.insert(
                    OsString::from("PLAYWRIGHT_HTML_OUTPUT_DIR"),
                    html.into_os_string(),
                );
                steps.push(Step {
                    id: StepId::new(format!("application:{}", suite.as_str())),
                    label: format!("{} Playwright", suite.as_str()),
                    capability: Capability::ApplicationTest,
                    provider: ProviderId::new("playwright"),
                    owner: owner.clone(),
                    cwd: tests.clone(),
                    program: OsString::from("pnpm"),
                    argv: vec![
                        "exec".into(),
                        "playwright".into(),
                        "test".into(),
                        "--config".into(),
                        tests.join("playwright.config.ts").into_os_string(),
                        "--project".into(),
                        suite.as_str().into(),
                        "--output".into(),
                        output.into_os_string(),
                        "--update-snapshots=none".into(),
                    ],
                    env,
                    targets: recognized_test_files(
                        &visible_files(workspace)?,
                        &tests.join(suite.as_str()),
                    ),
                    configuration_files: vec![
                        tests.join("package.json"),
                        tests.join("playwright.config.ts"),
                    ],
                    depends_on: Vec::new(),
                });
            }
            ApplicationSuite::UiProperty => {
                steps.push(Step {
                    id: StepId::new("application:ui-property"),
                    label: "ui-property Bombadil".into(),
                    capability: Capability::ApplicationTest,
                    provider: ProviderId::new("bombadil"),
                    owner: owner.clone(),
                    cwd: tests.clone(),
                    program: OsString::from("pnpm"),
                    argv: vec![
                        "exec".into(),
                        "bombadil".into(),
                        "test".into(),
                        "--headless".into(),
                        "--exit-on-violation".into(),
                        "--time-limit".into(),
                        "1m".into(),
                        "--output-path".into(),
                        state.artifact_dir.join("ui-property").into_os_string(),
                        state.web_url().into(),
                        tests.join("bombadil/specification.ts").into_os_string(),
                    ],
                    env: state.environment(),
                    targets: vec![tests.join("bombadil/specification.ts")],
                    configuration_files: vec![tests.join("package.json")],
                    depends_on: Vec::new(),
                });
            }
            ApplicationSuite::Application => {
                return Err(WorkflowError::Planning(
                    "aggregate application suite was not expanded".into(),
                ));
            }
        }
    }
    Ok(steps)
}

fn contract_steps(
    workspace: &Workspace,
    discovery: &ApplicationDiscovery,
    state: &ApplicationRunState,
    owner: &Owner,
) -> Result<Vec<Step>, WorkflowError> {
    let tests = &workspace.config.system.tests;
    discovery
        .openapi_documents
        .iter()
        .enumerate()
        .map(|(index, document)| {
            let report_dir = state
                .artifact_dir
                .join("contract")
                .join(format!("{index:04}"));
            fs::create_dir_all(&report_dir).map_err(|error| {
                WorkflowError::Planning(format!(
                    "cannot create Schemathesis report directory '{}': {error}",
                    report_dir.display()
                ))
            })?;
            Ok(Step {
                id: StepId::new(format!("application:contract:{index:04}")),
                label: format!("contract {}", relative(workspace, document).display()),
                capability: Capability::ApplicationTest,
                provider: ProviderId::new("schemathesis"),
                owner: owner.clone(),
                cwd: tests.clone(),
                program: OsString::from("uv"),
                argv: vec![
                    "run".into(),
                    "--locked".into(),
                    "--".into(),
                    "st".into(),
                    "run".into(),
                    document.as_os_str().to_owned(),
                    "--url".into(),
                    state.api_url().into(),
                    "--workers".into(),
                    "2".into(),
                    "--phases".into(),
                    "examples,coverage,fuzzing,stateful".into(),
                    "--max-examples".into(),
                    "20".into(),
                    "--report".into(),
                    "junit".into(),
                    "--report-junit-path".into(),
                    report_dir.join("junit.xml").into_os_string(),
                    "--report".into(),
                    "ndjson".into(),
                    "--report-ndjson-path".into(),
                    report_dir.join("events.ndjson").into_os_string(),
                ],
                env: state.environment(),
                targets: vec![document.clone()],
                configuration_files: vec![tests.join("pyproject.toml"), tests.join("uv.lock")],
                depends_on: Vec::new(),
            })
        })
        .collect()
}

impl ApplicationRunState {
    fn new(workspace: &Workspace) -> Result<Self, WorkflowError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let sequence = RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let run_id = format!("run-{now}-{}-{sequence}", std::process::id());
        let project = format!("system-test-{run_id}");
        let artifact_dir = workspace
            .config
            .system
            .tests
            .join("artifacts")
            .join(&run_id);
        fs::create_dir_all(&artifact_dir).map_err(|error| {
            WorkflowError::Planning(format!(
                "cannot create application artifact directory '{}': {error}",
                artifact_dir.display()
            ))
        })?;
        let control = tempfile::Builder::new()
            .prefix("just-application-")
            .tempdir()
            .map_err(|error| {
                WorkflowError::Planning(format!(
                    "cannot create application control directory: {error}"
                ))
            })?;
        let env_file = control.path().join("environment.env");
        let socket = control.path().join("process-compose.sock");
        let reservations = [
            reserve_port("API")?,
            reserve_port("web")?,
            reserve_port("PostgreSQL")?,
        ];
        let api_port = reservations[0]
            .local_addr()
            .map_err(|error| port_error(&error))?
            .port();
        let web_port = reservations[1]
            .local_addr()
            .map_err(|error| port_error(&error))?
            .port();
        let postgres_port = reservations[2]
            .local_addr()
            .map_err(|error| port_error(&error))?
            .port();
        Ok(Self {
            run_id,
            project,
            artifact_dir,
            _control: control,
            env_file,
            socket,
            api_port,
            web_port,
            postgres_port,
            reservations: Some(reservations),
        })
    }

    fn write_env(&self, workspace: &Workspace) -> Result<(), WorkflowError> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&self.env_file)
            .map_err(|error| {
                WorkflowError::Planning(format!(
                    "cannot create application environment file '{}': {error}",
                    self.env_file.display()
                ))
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|error| {
                    WorkflowError::Planning(format!(
                        "cannot restrict application environment file '{}': {error}",
                        self.env_file.display()
                    ))
                })?;
        }
        for (name, value) in self.environment() {
            writeln!(
                file,
                "{}={}",
                name.to_string_lossy(),
                dotenv_value(&value.to_string_lossy())
            )
            .map_err(|error| {
                WorkflowError::Planning(format!(
                    "cannot write application environment file '{}': {error}",
                    self.env_file.display()
                ))
            })?;
        }
        writeln!(
            file,
            "SYSTEM_TEST_ROOT={}",
            dotenv_value(&workspace.root.to_string_lossy())
        )
        .map_err(|error| {
            WorkflowError::Planning(format!(
                "cannot write application environment file '{}': {error}",
                self.env_file.display()
            ))
        })
    }

    fn environment(&self) -> std::collections::BTreeMap<OsString, OsString> {
        [
            ("SYSTEM_TEST_RUN_ID", self.run_id.clone()),
            ("SYSTEM_TEST_PROJECT", self.project.clone()),
            (
                "SYSTEM_TEST_ARTIFACT_DIR",
                self.artifact_dir.to_string_lossy().into_owned(),
            ),
            ("SYSTEM_API_HOST", Ipv4Addr::LOCALHOST.to_string()),
            ("SYSTEM_API_PORT", self.api_port.to_string()),
            ("SYSTEM_API_URL", self.api_url()),
            ("API_HOST", Ipv4Addr::LOCALHOST.to_string()),
            ("API_PORT", self.api_port.to_string()),
            ("API_URL", self.api_url()),
            ("SYSTEM_WEB_HOST", Ipv4Addr::LOCALHOST.to_string()),
            ("SYSTEM_WEB_PORT", self.web_port.to_string()),
            ("SYSTEM_WEB_URL", self.web_url()),
            ("WEB_HOST", Ipv4Addr::LOCALHOST.to_string()),
            ("WEB_PORT", self.web_port.to_string()),
            ("WEB_URL", self.web_url()),
            ("POSTGRES_HOST", Ipv4Addr::LOCALHOST.to_string()),
            ("POSTGRES_PORT", self.postgres_port.to_string()),
            ("POSTGRES_USER", "postgres".into()),
            ("POSTGRES_PASSWORD", "postgres".into()),
            ("POSTGRES_DB", self.database()),
            ("SYSTEM_POSTGRES_HOST", Ipv4Addr::LOCALHOST.to_string()),
            ("SYSTEM_POSTGRES_PORT", self.postgres_port.to_string()),
            ("SYSTEM_POSTGRES_USER", "postgres".into()),
            ("SYSTEM_POSTGRES_PASSWORD", "postgres".into()),
            ("SYSTEM_POSTGRES_DB", self.database()),
            ("SYSTEM_DATABASE_URL", self.database_url()),
            ("DATABASE_URL", self.database_url()),
        ]
        .into_iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect()
    }

    fn api_url(&self) -> String {
        format!("http://{}:{}", Ipv4Addr::LOCALHOST, self.api_port)
    }

    fn web_url(&self) -> String {
        format!("http://{}:{}", Ipv4Addr::LOCALHOST, self.web_port)
    }

    fn database(&self) -> String {
        format!("system_test_{}", self.run_id.replace('-', "_"))
    }

    fn database_url(&self) -> String {
        format!(
            "postgresql://postgres:postgres@{}:{}/{}",
            Ipv4Addr::LOCALHOST,
            self.postgres_port,
            self.database()
        )
    }
}

fn reserve_port(label: &str) -> Result<TcpListener, WorkflowError> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|error| {
        WorkflowError::Planning(format!(
            "cannot reserve loopback {label} application-test port: {error}"
        ))
    })
}

fn port_error(error: &std::io::Error) -> WorkflowError {
    WorkflowError::Planning(format!(
        "cannot inspect reserved application-test port: {error}"
    ))
}

fn dotenv_value(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn reject_owned_passthrough(
    suite: ApplicationSuite,
    passthrough: &[OsString],
) -> Result<(), WorkflowError> {
    let owned: &[&str] = match suite {
        ApplicationSuite::Contract => &[
            "-u",
            "--url",
            "--schema",
            "-w",
            "--workers",
            "-n",
            "--max-examples",
            "--phases",
            "--config",
            "--config-file",
            "--report",
        ],
        ApplicationSuite::E2e | ApplicationSuite::Visual => &[
            "-c",
            "--config",
            "--project",
            "--output",
            "--output-dir",
            "--reporter",
            "-u",
            "--update-snapshots",
            "--update-source-method",
            "--ignore-snapshots",
            "--last-failed-file",
        ],
        ApplicationSuite::UiProperty => &[
            "--origin",
            "--specification",
            "--headless",
            "--exit-on-violation",
            "--time-limit",
            "--output-path",
        ],
        ApplicationSuite::Application => unreachable!("aggregate suite was expanded"),
    };
    if let Some(argument) = passthrough
        .iter()
        .map(|argument| argument.to_string_lossy())
        .find(|argument| {
            owned
                .iter()
                .any(|owned| option_matches(argument.as_ref(), owned))
                || (suite == ApplicationSuite::Contract && argument.starts_with("--report-"))
        })
    {
        return Err(WorkflowError::Usage(format!(
            "test passthrough argument '{argument}' overrides a runtime-owned application-test value"
        )));
    }
    Ok(())
}

fn option_matches(argument: &str, option: &str) -> bool {
    argument == option
        || argument
            .strip_prefix(option)
            .is_some_and(|suffix| suffix.starts_with('='))
}

fn static_preflight(
    workspace: &Workspace,
    plan: &ApplicationPlan,
    target: Option<&Plan>,
) -> Result<(), WorkflowError> {
    let mut tools = BTreeSet::from([OsString::from("process-compose")]);
    let plans = target
        .into_iter()
        .chain(std::iter::once(&plan.plan))
        .flat_map(Plan::steps);
    for step in plans {
        if step.program.is_empty() || step.provider.as_str() == "not-applicable" {
            continue;
        }
        if !step.cwd.is_dir() {
            return Err(WorkflowError::Planning(format!(
                "application invocation working directory '{}' is not accessible",
                step.cwd.display()
            )));
        }
        for path in step.targets.iter().chain(&step.configuration_files) {
            if !path.is_file() {
                return Err(WorkflowError::Planning(format!(
                    "application invocation input '{}' is not accessible",
                    path.display()
                )));
            }
        }
        tools.insert(step.program.clone());
        if step.provider.as_str() == "nextest" {
            tools.insert(OsString::from("cargo-nextest"));
        }
    }
    let config = workspace.root.join("process-compose.yaml");
    if !config.is_file() {
        return Err(WorkflowError::Planning(format!(
            "Process Compose configuration '{}' is not accessible",
            config.display()
        )));
    }
    for tool in tools {
        if find_program(&tool).is_none() {
            return Err(WorkflowError::Planning(format!(
                "application invocation tool '{}' is not available",
                tool.to_string_lossy()
            )));
        }
    }
    Ok(())
}

fn process_compose_argv(
    workspace: &Workspace,
    state: &ApplicationRunState,
    action: &[&str],
) -> Vec<OsString> {
    let mut argv = vec![
        OsString::from("-f"),
        workspace.root.join("process-compose.yaml").into_os_string(),
        OsString::from("-e"),
        state.env_file.as_os_str().to_owned(),
        OsString::from("--disable-dotenv"),
        OsString::from("--ordered-shutdown"),
        OsString::from("-u"),
        state.socket.as_os_str().to_owned(),
        OsString::from("-U"),
        OsString::from("-L"),
        state
            .artifact_dir
            .join("process-compose.log")
            .into_os_string(),
        OsString::from("--log-no-color"),
        OsString::from("-t=false"),
    ];
    argv.extend(action.iter().map(OsString::from));
    argv
}

fn process_compose_output(
    workspace: &Workspace,
    state: &ApplicationRunState,
    action: &[&str],
) -> Result<Output, WorkflowError> {
    Command::new("process-compose")
        .args(process_compose_argv(workspace, state, action))
        .current_dir(&workspace.root)
        .envs(state.environment())
        .output()
        .map_err(|error| {
            WorkflowError::Planning(format!("could not execute Process Compose: {error}"))
        })
}

fn process_compose_control(
    workspace: &Workspace,
    state: &ApplicationRunState,
    argv: &[OsString],
    deadline: Instant,
    interrupt: &InterruptGuard,
) -> Result<Output, WorkflowError> {
    let mut command = Command::new("process-compose");
    command
        .args(argv)
        .current_dir(&workspace.root)
        .envs(state.environment())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|error| {
        WorkflowError::Planning(format!(
            "could not execute Process Compose control: {error}"
        ))
    })?;
    let Some(stdout) = child.stdout.take() else {
        let _ = terminate_group(&mut child, KILL_SIGNAL);
        let _ = child.wait();
        return Err(WorkflowError::Planning(
            "Process Compose control stdout was not captured".into(),
        ));
    };
    let Some(stderr) = child.stderr.take() else {
        let _ = terminate_group(&mut child, KILL_SIGNAL);
        let _ = child.wait();
        return Err(WorkflowError::Planning(
            "Process Compose control stderr was not captured".into(),
        ));
    };
    let stdout_reader = std::thread::spawn(move || read_all(stdout));
    let stderr_reader = std::thread::spawn(move || read_all(stderr));
    let started = Instant::now();
    let mut stopped_reason = None;
    let status_result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if interrupt.received() && started.elapsed() >= Duration::from_millis(50) => {
                stopped_reason = Some("Process Compose control interrupted");
                let termination = terminate_group(&mut child, KILL_SIGNAL);
                let waited = child.wait().map_err(|error| {
                    WorkflowError::Planning(format!(
                        "could not reap interrupted Process Compose control: {error}"
                    ))
                });
                break match (termination, waited) {
                    (Err(error), _) | (_, Err(error)) => Err(error),
                    (Ok(()), Ok(status)) => Ok(status),
                };
            }
            Ok(None) if Instant::now() >= deadline => {
                stopped_reason = Some("Process Compose control deadline expired");
                let termination = terminate_group(&mut child, KILL_SIGNAL);
                let waited = child.wait().map_err(|error| {
                    WorkflowError::Planning(format!(
                        "could not reap timed out Process Compose control: {error}"
                    ))
                });
                break match (termination, waited) {
                    (Err(error), _) | (_, Err(error)) => Err(error),
                    (Ok(()), Ok(status)) => Ok(status),
                };
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let termination = terminate_group(&mut child, KILL_SIGNAL);
                let waited = child.wait();
                break Err(termination.err().unwrap_or_else(|| {
                    WorkflowError::Planning(format!(
                        "could not inspect Process Compose control: {error}; reap result: {waited:?}"
                    ))
                }));
            }
        }
    };
    let stdout = join_reader(stdout_reader, "Process Compose control", "stdout");
    let stderr = join_reader(stderr_reader, "Process Compose control", "stderr");
    let status = status_result?;
    let mut stderr = stderr?;
    if let Some(reason) = stopped_reason {
        stderr.extend_from_slice(format!("\n{reason}").as_bytes());
    }
    Ok(Output {
        status,
        stdout: stdout?,
        stderr,
    })
}

struct ProcessCompose {
    child: Child,
    argv: Vec<OsString>,
    started_at: Instant,
}

struct LifecycleOutcome {
    report: StepReport,
    output: Output,
}

struct ReadinessOutcome {
    report: StepReport,
    output: Output,
    ready: bool,
}

impl ProcessCompose {
    fn start(workspace: &Workspace, state: &ApplicationRunState) -> Result<Self, WorkflowError> {
        let argv = process_compose_argv(workspace, state, &["up"]);
        let stdout = File::create(state.artifact_dir.join("process-compose.stdout.log")).map_err(
            |error| {
                WorkflowError::Planning(format!(
                    "cannot create Process Compose stdout artifact: {error}"
                ))
            },
        )?;
        let stderr = File::create(state.artifact_dir.join("process-compose.stderr.log")).map_err(
            |error| {
                WorkflowError::Planning(format!(
                    "cannot create Process Compose stderr artifact: {error}"
                ))
            },
        )?;
        let mut command = Command::new("process-compose");
        command
            .args(&argv)
            .current_dir(&workspace.root)
            .envs(state.environment())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command.spawn().map_err(|error| {
            WorkflowError::Planning(format!("could not start Process Compose: {error}"))
        })?;
        Ok(Self {
            child,
            argv,
            started_at: Instant::now(),
        })
    }

    fn started_report(&self, workspace: &Workspace) -> StepReport {
        raw_report(
            workspace,
            RawReport {
                id: "application:environment:start",
                capability: "application_test",
                provider: "process-compose",
                cwd: &workspace.root,
                argv: argv_strings("process-compose", &self.argv),
                configuration_files: vec![workspace.root.join("process-compose.yaml")],
                targets: Vec::new(),
                status: StepStatus::Passed,
                exit_code: None,
                duration: self.started_at.elapsed().as_millis() as u64,
                skip_reason: None,
                stdout: String::new(),
                stderr: String::new(),
            },
        )
    }

    fn healthy(&mut self) -> Result<bool, WorkflowError> {
        self.child
            .try_wait()
            .map(|status| status.is_none())
            .map_err(|error| {
                WorkflowError::Planning(format!(
                    "could not inspect Process Compose environment: {error}"
                ))
            })
    }

    fn wait_ready(
        &mut self,
        workspace: &Workspace,
        state: &ApplicationRunState,
        interrupt: &InterruptGuard,
    ) -> Result<ReadinessOutcome, WorkflowError> {
        let timeout = std::env::var("JUST_TEST_APPLICATION_READINESS_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map_or(READINESS_TIMEOUT, Duration::from_millis);
        let started = Instant::now();
        let argv = vec![
            OsString::from("-u"),
            state.socket.as_os_str().to_owned(),
            OsString::from("-U"),
            OsString::from("project"),
            OsString::from("is-ready"),
        ];
        let mut last = synthetic_output(1, "", "readiness was not checked");
        let mut ready = false;
        while started.elapsed() < timeout && !interrupt.received() {
            if !self.healthy()? {
                last = synthetic_output(1, "", "Process Compose exited before readiness");
                break;
            }
            last = process_compose_control(workspace, state, &argv, started + timeout, interrupt)?;
            if last.status.success() {
                if self.healthy()? {
                    ready = true;
                } else {
                    last = synthetic_output(1, "", "Process Compose exited during readiness");
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !ready && started.elapsed() >= timeout {
            last.stderr
                .extend_from_slice(b"\nRust readiness deadline expired");
        }
        Ok(ReadinessOutcome {
            report: raw_report(
                workspace,
                RawReport {
                    id: "application:environment:readiness",
                    capability: "application_test",
                    provider: "process-compose-readiness",
                    cwd: &workspace.root,
                    argv: argv_strings("process-compose", &argv),
                    configuration_files: vec![workspace.root.join("process-compose.yaml")],
                    targets: Vec::new(),
                    status: if ready {
                        StepStatus::Passed
                    } else {
                        StepStatus::Failed
                    },
                    exit_code: last.status.code(),
                    duration: started.elapsed().as_millis() as u64,
                    skip_reason: None,
                    stdout: String::from_utf8_lossy(&last.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&last.stderr).into_owned(),
                },
            ),
            output: last,
            ready,
        })
    }

    fn cleanup(
        &mut self,
        workspace: &Workspace,
        state: &ApplicationRunState,
        interrupt: &InterruptGuard,
    ) -> LifecycleOutcome {
        let started = Instant::now();
        let argv = vec![
            OsString::from("-u"),
            state.socket.as_os_str().to_owned(),
            OsString::from("-U"),
            OsString::from("--ordered-shutdown"),
            OsString::from("down"),
        ];
        let mut output = process_compose_control(
            workspace,
            state,
            &argv,
            Instant::now() + CONTROL_WAIT,
            interrupt,
        )
        .unwrap_or_else(|error| synthetic_output(1, "", &error.to_string()));
        let mut cleanup_errors = Vec::new();
        wait_for_child(
            &mut self.child,
            Instant::now() + CONTROL_WAIT,
            "Process Compose",
            &mut cleanup_errors,
        );
        let foreground_running = child_running(
            &mut self.child,
            "Process Compose cleanup",
            &mut cleanup_errors,
        );
        if foreground_running {
            if let Err(error) = terminate_group(&mut self.child, TERM_SIGNAL) {
                cleanup_errors.push(error.to_string());
            }
            wait_for_child(
                &mut self.child,
                Instant::now() + Duration::from_millis(300),
                "Process Compose termination",
                &mut cleanup_errors,
            );
            let still_running = child_running(
                &mut self.child,
                "Process Compose termination",
                &mut cleanup_errors,
            );
            if still_running && let Err(error) = terminate_group(&mut self.child, KILL_SIGNAL) {
                cleanup_errors.push(error.to_string());
            }
            if let Err(error) = self.child.wait() {
                cleanup_errors.push(format!("could not reap Process Compose: {error}"));
            }
        }
        if let Err(error) = terminate_group(&mut self.child, KILL_SIGNAL) {
            cleanup_errors.push(format!(
                "could not terminate surviving Process Compose descendants: {error}"
            ));
        }
        let cleanup_ok = self.child.try_wait().ok().flatten().is_some();
        if !cleanup_errors.is_empty() {
            output
                .stderr
                .extend_from_slice(format!("\n{}", cleanup_errors.join("\n")).as_bytes());
        }
        let cleanup_success = cleanup_ok && output.status.success() && cleanup_errors.is_empty();
        LifecycleOutcome {
            report: raw_report(
                workspace,
                RawReport {
                    id: "application:environment:cleanup",
                    capability: "application_test",
                    provider: "process-compose-cleanup",
                    cwd: &workspace.root,
                    argv: argv_strings("process-compose", &argv),
                    configuration_files: vec![workspace.root.join("process-compose.yaml")],
                    targets: Vec::new(),
                    status: if cleanup_success {
                        StepStatus::Passed
                    } else {
                        StepStatus::Failed
                    },
                    exit_code: output.status.code(),
                    duration: started.elapsed().as_millis() as u64,
                    skip_reason: None,
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                },
            ),
            output,
        }
    }
}

fn wait_for_child(child: &mut Child, deadline: Instant, label: &str, errors: &mut Vec<String>) {
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => {
                errors.push(format!("could not wait for {label}: {error}"));
                break;
            }
        }
    }
}

fn child_running(child: &mut Child, label: &str, errors: &mut Vec<String>) -> bool {
    match child.try_wait() {
        Ok(status) => status.is_none(),
        Err(error) => {
            errors.push(format!("could not inspect {label}: {error}"));
            true
        }
    }
}

impl Drop for ProcessCompose {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = terminate_group(&mut self.child, KILL_SIGNAL);
            let _ = self.child.wait();
        }
    }
}

fn run_owned(
    step: &Step,
    interrupt: &InterruptGuard,
    label: &str,
) -> Result<Output, WorkflowError> {
    let mut command = Command::new(&step.program);
    command
        .args(&step.argv)
        .current_dir(&step.cwd)
        .envs(&step.env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|error| {
        WorkflowError::Planning(format!(
            "could not launch {label} '{}': {error}",
            step.program.to_string_lossy()
        ))
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| WorkflowError::Planning(format!("{label} stdout was not captured")))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| WorkflowError::Planning(format!("{label} stderr was not captured")))?;
    let stdout_reader = std::thread::spawn(move || read_all(stdout));
    let stderr_reader = std::thread::spawn(move || read_all(stderr));
    let status = loop {
        if interrupt.received() {
            terminate_group(&mut child, KILL_SIGNAL)?;
            break child.wait().map_err(|error| {
                WorkflowError::Planning(format!("could not wait for interrupted {label}: {error}"))
            })?;
        }
        if let Some(status) = child.try_wait().map_err(|error| {
            WorkflowError::Planning(format!("could not wait for {label}: {error}"))
        })? {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                if status.signal() == Some(rustix::process::Signal::INT.as_raw()) {
                    terminate_group(&mut child, KILL_SIGNAL)?;
                }
            }
            break status;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Ok(Output {
        status,
        stdout: join_reader(stdout_reader, label, "stdout")?,
        stderr: join_reader(stderr_reader, label, "stderr")?,
    })
}

#[cfg(unix)]
const KILL_SIGNAL: rustix::process::Signal = rustix::process::Signal::KILL;
#[cfg(unix)]
const TERM_SIGNAL: rustix::process::Signal = rustix::process::Signal::TERM;
#[cfg(not(unix))]
const KILL_SIGNAL: () = ();
#[cfg(not(unix))]
const TERM_SIGNAL: () = ();

#[cfg(unix)]
fn terminate_group(
    child: &mut Child,
    signal: rustix::process::Signal,
) -> Result<(), WorkflowError> {
    let group = rustix::process::Pid::from_child(child);
    match rustix::process::kill_process_group(group, signal) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(rustix::io::Errno::PERM) => child
            .kill()
            .or_else(|direct_error| {
                (direct_error.kind() == std::io::ErrorKind::InvalidInput)
                    .then_some(())
                    .ok_or(direct_error)
            })
            .map_err(|direct_error| {
                WorkflowError::Planning(format!(
                    "could not stop application child after process-group denial: {direct_error}"
                ))
            }),
        Err(error) => Err(WorkflowError::Planning(format!(
            "could not stop process group: {error}"
        ))),
    }
}

#[cfg(not(unix))]
fn terminate_group(child: &mut Child, _signal: ()) -> Result<(), WorkflowError> {
    child.kill().map_err(|error| {
        WorkflowError::Planning(format!("could not stop application child: {error}"))
    })
}

fn read_all(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    reader.read_to_end(&mut output)?;
    Ok(output)
}

fn join_reader(
    reader: std::thread::JoinHandle<std::io::Result<Vec<u8>>>,
    label: &str,
    stream: &str,
) -> Result<Vec<u8>, WorkflowError> {
    reader
        .join()
        .map_err(|_| WorkflowError::Planning(format!("{label} {stream} reader panicked")))?
        .map_err(|error| {
            WorkflowError::Planning(format!("could not read {label} {stream}: {error}"))
        })
}

fn synthetic_output(code: i32, stdout: &str, stderr: &str) -> Output {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        Output {
            status: std::process::ExitStatus::from_raw(code.cast_unsigned()),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }
}

fn append_output(stdout: &mut String, stderr: &mut String, output: &Output) {
    stdout.push_str(&String::from_utf8_lossy(&output.stdout));
    stderr.push_str(&String::from_utf8_lossy(&output.stderr));
}

fn write_provider_logs(artifacts: &Path, id: &str, output: &Output) -> Result<(), WorkflowError> {
    let safe = id.replace(':', "-");
    fs::write(artifacts.join(format!("{safe}.stdout.log")), &output.stdout)
        .and_then(|()| fs::write(artifacts.join(format!("{safe}.stderr.log")), &output.stderr))
        .map_err(|error| {
            WorkflowError::Planning(format!("cannot retain application provider logs: {error}"))
        })
}

fn step_report(workspace: &Workspace, step: &Step, output: &Output, duration: u64) -> StepReport {
    let argv = std::iter::once(step.program.to_string_lossy().into_owned())
        .chain(
            step.argv
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned()),
        )
        .collect();
    raw_report(
        workspace,
        RawReport {
            id: step.id.as_str(),
            capability: capability_name(step.capability),
            provider: step.provider.as_str(),
            cwd: &step.cwd,
            argv,
            configuration_files: step.configuration_files.clone(),
            targets: step.targets.clone(),
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
        },
    )
}

fn skipped_step_report(workspace: &Workspace, step: &Step, reason: &str) -> StepReport {
    raw_report(
        workspace,
        RawReport {
            id: step.id.as_str(),
            capability: capability_name(step.capability),
            provider: step.provider.as_str(),
            cwd: &step.cwd,
            argv: Vec::new(),
            configuration_files: step.configuration_files.clone(),
            targets: step.targets.clone(),
            status: StepStatus::Skipped,
            exit_code: None,
            duration: 0,
            skip_reason: Some(reason.into()),
            stdout: String::new(),
            stderr: String::new(),
        },
    )
}

fn skipped_suite_reports(
    workspace: &Workspace,
    plan: &ApplicationPlan,
    reason: &str,
) -> Vec<StepReport> {
    plan.plan.steps()[plan.preparation_count..]
        .iter()
        .map(|step| skipped_step_report(workspace, step, reason))
        .collect()
}

fn skipped_application_reports(
    workspace: &Workspace,
    plan: &ApplicationPlan,
    reason: &str,
) -> Vec<StepReport> {
    plan.plan
        .steps()
        .iter()
        .map(|step| skipped_step_report(workspace, step, reason))
        .collect()
}

fn lifecycle_report(
    workspace: &Workspace,
    id: &str,
    provider: &str,
    output: &Output,
    argv: &[OsString],
    duration: u64,
) -> StepReport {
    raw_report(
        workspace,
        RawReport {
            id,
            capability: "application_test",
            provider,
            cwd: &workspace.root,
            argv: argv_strings("process-compose", argv),
            configuration_files: vec![workspace.root.join("process-compose.yaml")],
            targets: Vec::new(),
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
        },
    )
}

fn collection_report(workspace: &Workspace, state: &ApplicationRunState) -> StepReport {
    raw_report(
        workspace,
        RawReport {
            id: "application:artifacts:collection",
            capability: "application_test",
            provider: "artifact-collection",
            cwd: &workspace.root,
            argv: Vec::new(),
            configuration_files: Vec::new(),
            targets: vec![state.artifact_dir.clone()],
            status: StepStatus::Passed,
            exit_code: None,
            duration: 0,
            skip_reason: None,
            stdout: String::new(),
            stderr: String::new(),
        },
    )
}

fn environment_health_report(workspace: &Workspace, message: &str) -> StepReport {
    raw_report(
        workspace,
        RawReport {
            id: "application:environment:health",
            capability: "application_test",
            provider: "process-compose-health",
            cwd: &workspace.root,
            argv: Vec::new(),
            configuration_files: vec![workspace.root.join("process-compose.yaml")],
            targets: Vec::new(),
            status: StepStatus::Failed,
            exit_code: None,
            duration: 0,
            skip_reason: None,
            stdout: String::new(),
            stderr: message.into(),
        },
    )
}

fn controller_error_report(workspace: &Workspace, error: &WorkflowError) -> StepReport {
    raw_report(
        workspace,
        RawReport {
            id: "application:controller",
            capability: "application_test",
            provider: "application-controller",
            cwd: &workspace.root,
            argv: Vec::new(),
            configuration_files: vec![workspace.root.join("process-compose.yaml")],
            targets: Vec::new(),
            status: StepStatus::Failed,
            exit_code: None,
            duration: 0,
            skip_reason: None,
            stdout: String::new(),
            stderr: error.to_string(),
        },
    )
}

struct RawReport<'a> {
    id: &'a str,
    capability: &'a str,
    provider: &'a str,
    cwd: &'a Path,
    argv: Vec<String>,
    configuration_files: Vec<PathBuf>,
    targets: Vec<PathBuf>,
    status: StepStatus,
    exit_code: Option<i32>,
    duration: u64,
    skip_reason: Option<String>,
    stdout: String,
    stderr: String,
}

fn raw_report(workspace: &Workspace, report: RawReport<'_>) -> StepReport {
    StepReport {
        id: report.id.into(),
        capability: report.capability.into(),
        provider: report.provider.into(),
        target: None,
        component: None,
        cwd: relative(workspace, report.cwd)
            .to_string_lossy()
            .into_owned(),
        argv: report.argv,
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
        exit_code: report.exit_code,
        duration: report.duration,
        skip_reason: report.skip_reason,
        stdout: report.stdout,
        stderr: report.stderr,
    }
}

fn finish_report(
    reports: Vec<StepReport>,
    selectors: Vec<String>,
    started_at: String,
    started: Instant,
    interrupted: bool,
    stdout: String,
    stderr: String,
) -> (Report, String, String) {
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
    (
        Report {
            schema_version: 1,
            command: "test".into(),
            selectors,
            started_at,
            duration: started.elapsed().as_millis() as u64,
            status,
            steps: reports,
        },
        stdout,
        stderr,
    )
}

fn argv_strings(program: &str, argv: &[OsString]) -> Vec<String> {
    std::iter::once(program.into())
        .chain(
            argv.iter()
                .map(|argument| argument.to_string_lossy().into_owned()),
        )
        .collect()
}

const fn capability_name(capability: Capability) -> &'static str {
    match capability {
        Capability::Compile => "compile",
        _ => "application_test",
    }
}

fn relative<'a>(workspace: &'a Workspace, path: &'a Path) -> &'a Path {
    path.strip_prefix(&workspace.root).unwrap_or(path)
}

fn reject_partial(
    suite: &str,
    evidence: bool,
    ready: bool,
    requirement: &str,
) -> Result<(), WorkflowError> {
    if evidence && !ready {
        Err(WorkflowError::Configuration(format!(
            "partial {suite} application-test configuration: {requirement}"
        )))
    } else {
        Ok(())
    }
}

fn visible_files(workspace: &Workspace) -> Result<BTreeSet<PathBuf>, WorkflowError> {
    let mut builder = WalkBuilder::new(&workspace.root);
    builder
        .hidden(false)
        .ignore(false)
        .parents(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|entry| entry.file_name() != ".git");
    let mut files = BTreeSet::new();
    for entry in builder.build() {
        let entry = entry.map_err(|error| {
            WorkflowError::Discovery(format!(
                "cannot discover application-test evidence: {error}"
            ))
        })?;
        if entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            files.insert(fs::canonicalize(entry.path()).map_err(|error| {
                WorkflowError::Discovery(format!(
                    "cannot resolve application-test evidence '{}': {error}",
                    entry.path().display()
                ))
            })?);
        }
    }
    Ok(files)
}

fn recognized_test_files(visible: &BTreeSet<PathBuf>, root: &Path) -> Vec<PathBuf> {
    visible
        .iter()
        .filter(|path| path.starts_with(root))
        .filter(|path| {
            let Some(name) = path.file_name().and_then(OsStr::to_str) else {
                return false;
            };
            let recognized_name = name.contains(".spec.") || name.contains(".test.");
            let recognized_extension = matches!(
                path.extension().and_then(OsStr::to_str),
                Some("ts" | "tsx" | "js" | "jsx" | "mts" | "cts" | "mjs" | "cjs")
            );
            recognized_name && recognized_extension
        })
        .cloned()
        .collect()
}

fn read_json(path: &Path) -> Result<serde_json::Value, WorkflowError> {
    let source = fs::read_to_string(path).map_err(|error| {
        WorkflowError::Configuration(format!("cannot read '{}': {error}", path.display()))
    })?;
    serde_json::from_str(&source).map_err(|error| {
        WorkflowError::Configuration(format!("cannot parse '{}': {error}", path.display()))
    })
}

fn dependency<'a>(package: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    ["devDependencies", "dependencies"]
        .iter()
        .find_map(|field| package.get(field)?.get(name)?.as_str())
}

fn exact_version(version: &str) -> bool {
    semver::Version::parse(version).is_ok()
}

fn toml_declares(path: &Path, dependency: &str) -> Result<bool, WorkflowError> {
    let source = fs::read_to_string(path).map_err(|error| {
        WorkflowError::Configuration(format!("cannot read '{}': {error}", path.display()))
    })?;
    let document: toml::Value = toml::from_str(&source).map_err(|error| {
        WorkflowError::Configuration(format!("cannot parse '{}': {error}", path.display()))
    })?;
    let project_dependencies = document
        .get("project")
        .and_then(|project| project.get("dependencies"))
        .and_then(toml::Value::as_array);
    let uv_development_dependencies = document
        .get("dependency-groups")
        .and_then(|groups| groups.get("dev"))
        .and_then(toml::Value::as_array);
    Ok(project_dependencies
        .into_iter()
        .chain(uv_development_dependencies)
        .flatten()
        .filter_map(toml::Value::as_str)
        .any(|requirement| requirement_name(requirement) == normalize_package_name(dependency)))
}

fn requirement_name(requirement: &str) -> String {
    let name = requirement
        .trim_start()
        .chars()
        .take_while(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
        .collect::<String>();
    normalize_package_name(&name)
}

fn normalize_package_name(name: &str) -> String {
    let mut normalized = String::new();
    let mut separator = false;
    for character in name.chars() {
        if matches!(character, '-' | '_' | '.') {
            separator = !normalized.is_empty();
        } else {
            if separator {
                normalized.push('-');
                separator = false;
            }
            normalized.extend(character.to_lowercase());
        }
    }
    normalized
}

fn ignored_by_repository(workspace: &Workspace, path: &Path) -> Result<bool, WorkflowError> {
    let mut builder = GitignoreBuilder::new(&workspace.root);
    for ignore in [
        workspace.root.join(".gitignore"),
        workspace.config.system.tests.join(".gitignore"),
    ] {
        if ignore.is_file() {
            builder.add(ignore);
        }
    }
    let ignore = builder.build().map_err(|error| {
        WorkflowError::Configuration(format!("cannot parse Git ignore rules: {error}"))
    })?;
    Ok(ignore.matched_path_or_any_parents(path, true).is_ignore())
}
