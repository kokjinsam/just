//! Repository-wide verification planning and execution.

use std::time::Instant;

use crate::application_testing::{self, ApplicationPlan, ApplicationSuite};
use crate::capability::Capability;
use crate::checking;
use crate::error::WorkflowError;
use crate::formatting;
use crate::linting;
use crate::plan::Plan;
use crate::report::{Report, ReportStatus, StepReport, StepStatus};
use crate::selector::Selection;
use crate::specification;
use crate::target_testing::{self, TestMode};
use crate::workspace::Workspace;

/// Complete repository-wide workflow in deterministic stage order.
#[derive(Debug)]
pub struct VerificationPlan {
    formatting: Plan,
    linting: Plan,
    typecheck: Plan,
    compile: Plan,
    specification: Plan,
    coverage: Plan,
    application: Option<ApplicationPlan>,
}

/// Builds every verification stage before provider preflight begins.
///
/// # Errors
///
/// Returns an error when any stage cannot be planned from repository state.
pub fn build_plan(workspace: &Workspace) -> Result<VerificationPlan, WorkflowError> {
    let selections = [Selection::Repository];
    let formatting = formatting::build_plan(workspace, &selections, Capability::FormatCheck)?;
    let linting = linting::build_plan(workspace, &selections)?;
    let typecheck = checking::build_plan(workspace, &selections, Capability::Typecheck)?;
    let compile = checking::build_plan(workspace, &selections, Capability::Compile)?;
    let specification = specification::build_plan(workspace, None, &[])?;
    let coverage =
        target_testing::build_plan(workspace, &selections, None, &[], TestMode::Coverage)?;
    let discovery = application_testing::discover(workspace)?;
    let application = if discovery.configured_suites().is_empty() {
        None
    } else {
        Some(application_testing::build_plan(
            workspace,
            ApplicationSuite::Application,
            &[],
        )?)
    };
    Ok(VerificationPlan {
        formatting,
        linting,
        typecheck,
        compile,
        specification,
        coverage,
        application,
    })
}

struct PreparedVerification {
    formatting: formatting::PreparedPlan,
    linting: linting::PreparedPlan,
    typecheck: checking::PreparedPlan,
    compile: checking::PreparedPlan,
    specification: specification::PreparedPlan,
    coverage: target_testing::PreparedPlan,
    application: Option<application_testing::PreparedPlan>,
}

fn prepare(
    workspace: &Workspace,
    plan: VerificationPlan,
) -> Result<PreparedVerification, WorkflowError> {
    let formatting = formatting::prepare_plan(&plan.formatting)?;
    let linting = linting::prepare_plan(&plan.linting)?;
    let typecheck = checking::prepare_plan(&plan.typecheck, Capability::Typecheck)?;
    let compile = checking::prepare_plan(&plan.compile, Capability::Compile)?;
    let specification = specification::prepare_plan(&plan.specification)?;
    let coverage = target_testing::prepare_plan(workspace, &plan.coverage, TestMode::Coverage)?;
    let application = plan
        .application
        .map(|application| application_testing::prepare_plan(workspace, application))
        .transpose()?;
    Ok(PreparedVerification {
        formatting,
        linting,
        typecheck,
        compile,
        specification,
        coverage,
        application,
    })
}

/// Preflights every stage, then executes one combined verification workflow.
///
/// # Errors
///
/// Returns an error when planning, preflight, provider launch, or cleanup fails.
pub fn execute_plan(
    workspace: &Workspace,
    plan: VerificationPlan,
    started_at: String,
) -> Result<(Report, String, String), WorkflowError> {
    let mut prepared = prepare(workspace, plan)?;
    let started = Instant::now();
    let mut reports = Vec::new();
    let mut stdout = String::new();
    let mut stderr = String::new();

    macro_rules! execute_stage {
        ($execution:expr) => {{
            let (report, stage_stdout, stage_stderr) = $execution?;
            stdout.push_str(&stage_stdout);
            stderr.push_str(&stage_stderr);
            let interrupted = report.status == ReportStatus::Interrupted;
            reports.extend(report.steps);
            if interrupted {
                return Ok(finish(reports, started_at, started, true, stdout, stderr));
            }
        }};
    }

    execute_stage!(formatting::execute_prepared(
        workspace,
        &prepared.formatting,
        Vec::new(),
        started_at.clone(),
    ));
    execute_stage!(linting::execute_prepared(
        workspace,
        &prepared.linting,
        Vec::new(),
        started_at.clone(),
    ));
    execute_stage!(checking::execute_prepared(
        workspace,
        &prepared.typecheck,
        Vec::new(),
        started_at.clone(),
    ));
    execute_stage!(checking::execute_prepared(
        workspace,
        &prepared.compile,
        Vec::new(),
        started_at.clone(),
    ));
    execute_stage!(specification::execute_prepared(
        workspace,
        &prepared.specification,
        Vec::new(),
        started_at.clone(),
    ));
    execute_stage!(target_testing::execute_prepared(
        workspace,
        &prepared.coverage,
        Vec::new(),
        started_at.clone(),
    ));
    if let Some(application) = &mut prepared.application {
        execute_stage!(application_testing::execute_prepared(
            workspace,
            application,
            Vec::new(),
            started_at.clone(),
        ));
    }

    Ok(finish(reports, started_at, started, false, stdout, stderr))
}

fn finish(
    steps: Vec<StepReport>,
    started_at: String,
    started: Instant,
    interrupted: bool,
    stdout: String,
    stderr: String,
) -> (Report, String, String) {
    let status = if interrupted {
        ReportStatus::Interrupted
    } else if steps.iter().any(|step| step.status == StepStatus::Failed) {
        ReportStatus::Failed
    } else {
        ReportStatus::Passed
    };
    (
        Report {
            schema_version: 1,
            command: "verify".into(),
            selectors: Vec::new(),
            started_at,
            duration: started.elapsed().as_millis() as u64,
            status,
            steps,
        },
        stdout,
        stderr,
    )
}
