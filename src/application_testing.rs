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

const READINESS_TIMEOUT: Duration = Duration::from_secs(60);
const CONTROL_WAIT: Duration = Duration::from_secs(2);
static RUN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ApplicationSuite {
    Contract,
    E2e,
    Visual,
    UiProperty,
    Application,
}

impl ApplicationSuite {
    pub const SHORT: [Self; 4] = [Self::Contract, Self::E2e, Self::Visual, Self::UiProperty];

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationDiscovery {
    configured: Vec<ApplicationSuite>,
    openapi_documents: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct ApplicationPlan {
    plan: Plan,
    preparation_count: usize,
    suite_count: usize,
    state: ApplicationRunState,
}

impl ApplicationPlan {
    #[must_use]
    pub fn steps(&self) -> &[Step] {
        self.plan.steps()
    }

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
    #[must_use]
    pub fn configured_suites(&self) -> &[ApplicationSuite] {
        &self.configured
    }

    #[must_use]
    pub fn openapi_documents(&self) -> &[PathBuf] {
        &self.openapi_documents
    }

    #[must_use]
    pub fn is_configured(&self, suite: ApplicationSuite) -> bool {
        self.configured.contains(&suite)
    }
}

pub fn discover(workspace: &Workspace) -> Result<ApplicationDiscovery, WorkflowError> {
    let tests = &workspace.config.system.tests;
    let specs = &workspace.config.system.specs;
    let visible = visible_files(workspace)?;
    let visible_file = |path: &Path| {
        fs::canonicalize(path)
            .ok()
            .is_some_and(|path| visible.contains(&path))
    };

    let openapi_root = specs.join("openapi");
    let mut openapi_documents = visible
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
    openapi_documents.sort();

    let pyproject = tests.join("pyproject.toml");
    let uv_lock = tests.join("uv.lock");
    let has_pyproject = visible_file(&pyproject);
    let declares_schemathesis = has_pyproject && toml_declares(&pyproject, "schemathesis")?;
    let has_uv_lock = visible_file(&uv_lock);
    let contract_evidence =
        !openapi_documents.is_empty() || has_pyproject || declares_schemathesis || has_uv_lock;
    let contract_ready = !openapi_documents.is_empty() && declares_schemathesis && has_uv_lock;
    reject_partial(
        "contract",
        contract_evidence,
        contract_ready,
        "requires visible OpenAPI documents, pyproject.toml declaring Schemathesis, and uv.lock",
    )?;

    let package_path = tests.join("package.json");
    let package_visible = visible_file(&package_path);
    let package = package_visible
        .then(|| read_json(&package_path))
        .transpose()?;
    let playwright = package
        .as_ref()
        .and_then(|package| dependency(package, "@playwright/test"))
        .is_some();
    let config = tests.join("playwright.config.ts");
    let config_visible = visible_file(&config);
    let e2e_files = recognized_test_files(&visible, &tests.join("e2e"));
    let visual_files = recognized_test_files(&visible, &tests.join("visual"));
    let e2e_evidence = playwright || config_visible || !e2e_files.is_empty();
    let e2e_ready = playwright && config_visible && !e2e_files.is_empty();
    reject_partial(
        "e2e",
        e2e_evidence,
        e2e_ready,
        "requires @playwright/test, playwright.config.ts, and a recognized e2e test file",
    )?;
    let visual_evidence = playwright || config_visible || !visual_files.is_empty();
    let visual_ready = playwright && config_visible && !visual_files.is_empty();
    reject_partial(
        "visual",
        visual_evidence,
        visual_ready,
        "requires @playwright/test, playwright.config.ts, and a recognized visual test file",
    )?;

    let bombadil_version = package
        .as_ref()
        .and_then(|package| dependency(package, "@antithesishq/bombadil"));
    let bombadil_spec = tests.join("bombadil/specification.ts");
    let bombadil_spec_visible = visible_file(&bombadil_spec);
    let bombadil_evidence = bombadil_version.is_some() || bombadil_spec_visible;
    let bombadil_ready = bombadil_version.is_some_and(exact_version) && bombadil_spec_visible;
    reject_partial(
        "ui-property",
        bombadil_evidence,
        bombadil_ready,
        "requires an exactly pinned @antithesishq/bombadil dependency and bombadil/specification.ts",
    )?;

    let process_compose = workspace.root.join("process-compose.yaml");
    let process_compose_visible = visible_file(&process_compose);
    let artifacts_ignored = ignored_by_repository(workspace, &tests.join("artifacts"))?;
    let venv_ignored = ignored_by_repository(workspace, &tests.join(".venv"))?;
    let shared_evidence = process_compose_visible || artifacts_ignored || venv_ignored;
    let shared_ready = process_compose_visible && artifacts_ignored && venv_ignored;
    reject_partial(
        "application environment",
        shared_evidence,
        shared_ready,
        "requires root process-compose.yaml and Git-ignore coverage for system.tests/artifacts/ and .venv/",
    )?;

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
            .expect("one resolved application action has one provider");
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

pub fn execute_plan(
    workspace: &Workspace,
    plan: &mut ApplicationPlan,
    selectors: Vec<String>,
    started_at: String,
) -> Result<(Report, String, String), WorkflowError> {
    execute_with_target(workspace, plan, None, selectors, started_at)
}

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

    let dry = process_compose_output(workspace, &plan.state, &["up", "--dry-run"])?;
    append_output(&mut aggregate_stdout, &mut aggregate_stderr, &dry);
    reports.push(lifecycle_report(
        workspace,
        "application:environment:dry-run",
        "process-compose-dry-run",
        &dry,
        &process_compose_argv(workspace, &plan.state, &["up", "--dry-run"]),
        0,
    ));
    if !dry.status.success() {
        reports.extend(skipped_application_reports(
            workspace,
            plan,
            "Process Compose dry-run validation failed",
        ));
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

    if let Some(target) = target {
        let (target_report, stdout, stderr) = target_testing::execute_plan(
            workspace,
            target,
            Vec::new(),
            started_at.clone(),
            TestMode::Test,
        )?;
        aggregate_stdout.push_str(&stdout);
        aggregate_stderr.push_str(&stderr);
        reports.extend(target_report.steps);
        if target_report.status == ReportStatus::Interrupted || interrupt.received() {
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

    let mut preparation_failed = false;
    for step in &plan.plan.steps()[..plan.preparation_count] {
        let step_started = Instant::now();
        let output = run_owned(step, &interrupt, "application preparation")?;
        append_output(&mut aggregate_stdout, &mut aggregate_stderr, &output);
        write_provider_logs(&plan.state.artifact_dir, step.id.as_str(), &output)?;
        reports.push(step_report(
            workspace,
            step,
            &output,
            step_started.elapsed().as_millis() as u64,
        ));
        preparation_failed |= !output.status.success();
        if interrupt.received() || was_interrupted(&output) {
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
    if preparation_failed {
        reports.extend(skipped_suite_reports(
            workspace,
            plan,
            "application preparation failed; environment and suites were skipped",
        ));
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

    plan.state.reservations.take();
    let mut environment = ProcessCompose::start(workspace, &plan.state)?;
    reports.push(environment.started_report(workspace));
    let suite_steps =
        &plan.plan.steps()[plan.preparation_count..plan.preparation_count + plan.suite_count];
    let mut suites_accounted_for = 0;
    let operational = (|| -> Result<(), WorkflowError> {
        let readiness = environment.wait_ready(workspace, &plan.state, &interrupt)?;
        append_output(
            &mut aggregate_stdout,
            &mut aggregate_stderr,
            &readiness.output,
        );
        reports.push(readiness.report);

        if !readiness.ready {
            reports.extend(suite_steps.iter().map(|step| {
                skipped_step_report(
                    workspace,
                    step,
                    "Process Compose environment did not become ready",
                )
            }));
            suites_accounted_for = suite_steps.len();
            return Ok(());
        }

        for (index, step) in suite_steps.iter().enumerate() {
            if interrupt.received() {
                break;
            }
            if !environment.healthy()? {
                reports.push(environment_health_report(
                    workspace,
                    "Process Compose environment exited before all suites ran",
                ));
                reports.extend(suite_steps[index..].iter().map(|remaining| {
                    skipped_step_report(
                        workspace,
                        remaining,
                        "Process Compose environment exited before the suite ran",
                    )
                }));
                suites_accounted_for = suite_steps.len();
                break;
            }
            let step_started = Instant::now();
            let output = run_owned(step, &interrupt, "application-test provider")?;
            append_output(&mut aggregate_stdout, &mut aggregate_stderr, &output);
            write_provider_logs(&plan.state.artifact_dir, step.id.as_str(), &output)?;
            reports.push(step_report(
                workspace,
                step,
                &output,
                step_started.elapsed().as_millis() as u64,
            ));
            suites_accounted_for = index + 1;
            if interrupt.received() || was_interrupted(&output) {
                break;
            }
        }
        if !interrupt.received()
            && !environment.healthy()?
            && !reports
                .iter()
                .any(|report| report.provider == "process-compose-health")
        {
            reports.push(environment_health_report(
                workspace,
                "Process Compose environment exited while suites were running",
            ));
        }
        Ok(())
    })();
    if let Err(error) = operational {
        reports.push(controller_error_report(workspace, &error));
        reports.extend(suite_steps[suites_accounted_for..].iter().map(|step| {
            skipped_step_report(
                workspace,
                step,
                "application controller failed after the environment started",
            )
        }));
    } else if interrupt.received() {
        reports.extend(suite_steps[suites_accounted_for..].iter().map(|step| {
            skipped_step_report(
                workspace,
                step,
                "application test execution was interrupted",
            )
        }));
    }
    reports.push(collection_report(workspace, &plan.state));
    let cleanup = environment.cleanup(workspace, &plan.state, &interrupt);
    append_output(
        &mut aggregate_stdout,
        &mut aggregate_stderr,
        &cleanup.output,
    );
    reports.push(cleanup.report);
    Ok(finish_report(
        reports,
        selectors,
        started_at,
        started,
        interrupt.received(),
        aggregate_stdout,
        aggregate_stderr,
    ))
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
                for (index, document) in discovery.openapi_documents.iter().enumerate() {
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
                    steps.push(Step {
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
                        configuration_files: vec![
                            tests.join("pyproject.toml"),
                            tests.join("uv.lock"),
                        ],
                        depends_on: Vec::new(),
                    });
                }
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
            ApplicationSuite::Application => unreachable!("aggregate suite was expanded"),
        }
    }
    Ok(steps)
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
        let api_port = reservations[0].local_addr().map_err(port_error)?.port();
        let web_port = reservations[1].local_addr().map_err(port_error)?.port();
        let postgres_port = reservations[2].local_addr().map_err(port_error)?.port();
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

fn port_error(error: std::io::Error) -> WorkflowError {
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
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = terminate_group(&mut child, libc::SIGKILL);
            let _ = child.wait();
            return Err(WorkflowError::Planning(
                "Process Compose control stdout was not captured".into(),
            ));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let _ = terminate_group(&mut child, libc::SIGKILL);
            let _ = child.wait();
            return Err(WorkflowError::Planning(
                "Process Compose control stderr was not captured".into(),
            ));
        }
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
                let termination = terminate_group(&mut child, libc::SIGKILL);
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
                let termination = terminate_group(&mut child, libc::SIGKILL);
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
                let termination = terminate_group(&mut child, libc::SIGKILL);
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
            "application:environment:start",
            "application_test",
            "process-compose",
            &workspace.root,
            argv_strings("process-compose", &self.argv),
            vec![workspace.root.join("process-compose.yaml")],
            Vec::new(),
            StepStatus::Passed,
            None,
            self.started_at.elapsed().as_millis() as u64,
            None,
            String::new(),
            String::new(),
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
                "application:environment:readiness",
                "application_test",
                "process-compose-readiness",
                &workspace.root,
                argv_strings("process-compose", &argv),
                vec![workspace.root.join("process-compose.yaml")],
                Vec::new(),
                if ready {
                    StepStatus::Passed
                } else {
                    StepStatus::Failed
                },
                last.status.code(),
                started.elapsed().as_millis() as u64,
                None,
                String::from_utf8_lossy(&last.stdout).into_owned(),
                String::from_utf8_lossy(&last.stderr).into_owned(),
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
        let deadline = Instant::now() + CONTROL_WAIT;
        let mut cleanup_errors = Vec::new();
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(error) => {
                    cleanup_errors.push(format!("could not wait for Process Compose: {error}"));
                    break;
                }
            }
        }
        let foreground_running = match self.child.try_wait() {
            Ok(status) => status.is_none(),
            Err(error) => {
                cleanup_errors.push(format!(
                    "could not inspect Process Compose cleanup: {error}"
                ));
                true
            }
        };
        if foreground_running {
            if let Err(error) = terminate_group(&mut self.child, libc::SIGTERM) {
                cleanup_errors.push(error.to_string());
            }
            let term_deadline = Instant::now() + Duration::from_millis(300);
            while Instant::now() < term_deadline {
                match self.child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                    Err(error) => {
                        cleanup_errors.push(format!(
                            "could not wait for Process Compose termination: {error}"
                        ));
                        break;
                    }
                }
            }
            let still_running = match self.child.try_wait() {
                Ok(status) => status.is_none(),
                Err(error) => {
                    cleanup_errors.push(format!(
                        "could not inspect Process Compose termination: {error}"
                    ));
                    true
                }
            };
            if still_running && let Err(error) = terminate_group(&mut self.child, libc::SIGKILL) {
                cleanup_errors.push(error.to_string());
            }
            if let Err(error) = self.child.wait() {
                cleanup_errors.push(format!("could not reap Process Compose: {error}"));
            }
        }
        if let Err(error) = terminate_group(&mut self.child, libc::SIGKILL) {
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
                "application:environment:cleanup",
                "application_test",
                "process-compose-cleanup",
                &workspace.root,
                argv_strings("process-compose", &argv),
                vec![workspace.root.join("process-compose.yaml")],
                Vec::new(),
                if cleanup_success {
                    StepStatus::Passed
                } else {
                    StepStatus::Failed
                },
                output.status.code(),
                started.elapsed().as_millis() as u64,
                None,
                String::from_utf8_lossy(&output.stdout).into_owned(),
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ),
            output,
        }
    }
}

impl Drop for ProcessCompose {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = terminate_group(&mut self.child, libc::SIGKILL);
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
            terminate_group(&mut child, libc::SIGKILL)?;
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
                if status.signal() == Some(libc::SIGINT) {
                    terminate_group(&mut child, libc::SIGKILL)?;
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

fn terminate_group(child: &mut Child, signal: i32) -> Result<(), WorkflowError> {
    #[cfg(unix)]
    {
        let group = i32::try_from(child.id()).map_err(|error| {
            WorkflowError::Planning(format!("invalid child process id: {error}"))
        })?;
        let result = unsafe { libc::kill(-group, signal) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            if error.raw_os_error() == Some(libc::EPERM) {
                return child.kill().or_else(|direct_error| {
                    (direct_error.kind() == std::io::ErrorKind::InvalidInput)
                        .then_some(())
                        .ok_or(direct_error)
                }).map_err(|direct_error| {
                    WorkflowError::Planning(format!(
                        "could not stop application child after process-group denial: {direct_error}"
                    ))
                });
            }
            {
                return Err(WorkflowError::Planning(format!(
                    "could not stop process group: {error}"
                )));
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = signal;
        child.kill().map_err(|error| {
            WorkflowError::Planning(format!("could not stop application child: {error}"))
        })
    }
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
    #[cfg(not(unix))]
    {
        let status = Command::new(if code == 0 { "cmd" } else { "cmd" })
            .args(if code == 0 {
                ["/C", "exit", "0"]
            } else {
                ["/C", "exit", "1"]
            })
            .status()
            .expect("platform command shell is available");
        Output {
            status,
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
        step.id.as_str(),
        capability_name(step.capability),
        step.provider.as_str(),
        &step.cwd,
        argv,
        step.configuration_files.clone(),
        step.targets.clone(),
        if output.status.success() {
            StepStatus::Passed
        } else {
            StepStatus::Failed
        },
        output.status.code(),
        duration,
        None,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn skipped_step_report(workspace: &Workspace, step: &Step, reason: &str) -> StepReport {
    raw_report(
        workspace,
        step.id.as_str(),
        capability_name(step.capability),
        step.provider.as_str(),
        &step.cwd,
        Vec::new(),
        step.configuration_files.clone(),
        step.targets.clone(),
        StepStatus::Skipped,
        None,
        0,
        Some(reason.into()),
        String::new(),
        String::new(),
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
        id,
        "application_test",
        provider,
        &workspace.root,
        argv_strings("process-compose", argv),
        vec![workspace.root.join("process-compose.yaml")],
        Vec::new(),
        if output.status.success() {
            StepStatus::Passed
        } else {
            StepStatus::Failed
        },
        output.status.code(),
        duration,
        None,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn collection_report(workspace: &Workspace, state: &ApplicationRunState) -> StepReport {
    raw_report(
        workspace,
        "application:artifacts:collection",
        "application_test",
        "artifact-collection",
        &workspace.root,
        Vec::new(),
        Vec::new(),
        vec![state.artifact_dir.clone()],
        StepStatus::Passed,
        None,
        0,
        None,
        String::new(),
        String::new(),
    )
}

fn environment_health_report(workspace: &Workspace, message: &str) -> StepReport {
    raw_report(
        workspace,
        "application:environment:health",
        "application_test",
        "process-compose-health",
        &workspace.root,
        Vec::new(),
        vec![workspace.root.join("process-compose.yaml")],
        Vec::new(),
        StepStatus::Failed,
        None,
        0,
        None,
        String::new(),
        message.into(),
    )
}

fn controller_error_report(workspace: &Workspace, error: &WorkflowError) -> StepReport {
    raw_report(
        workspace,
        "application:controller",
        "application_test",
        "application-controller",
        &workspace.root,
        Vec::new(),
        vec![workspace.root.join("process-compose.yaml")],
        Vec::new(),
        StepStatus::Failed,
        None,
        0,
        None,
        String::new(),
        error.to_string(),
    )
}

#[allow(clippy::too_many_arguments)]
fn raw_report(
    workspace: &Workspace,
    id: &str,
    capability: &str,
    provider: &str,
    cwd: &Path,
    argv: Vec<String>,
    configuration_files: Vec<PathBuf>,
    targets: Vec<PathBuf>,
    status: StepStatus,
    exit_code: Option<i32>,
    duration: u64,
    skip_reason: Option<String>,
    stdout: String,
    stderr: String,
) -> StepReport {
    StepReport {
        id: id.into(),
        capability: capability.into(),
        provider: provider.into(),
        target: None,
        component: None,
        cwd: relative(workspace, cwd).to_string_lossy().into_owned(),
        argv,
        configuration_files: configuration_files
            .iter()
            .map(|path| relative(workspace, path).to_string_lossy().into_owned())
            .collect(),
        targets: targets
            .iter()
            .map(|path| relative(workspace, path).to_string_lossy().into_owned())
            .collect(),
        status,
        exit_code,
        duration,
        skip_reason,
        stdout,
        stderr,
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

fn capability_name(capability: Capability) -> &'static str {
    match capability {
        Capability::Compile => "compile",
        Capability::ApplicationTest => "application_test",
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
