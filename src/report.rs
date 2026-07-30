//! Stable workflow report schemas and renderers.

use std::fmt::Write;

use serde::Serialize;

/// A complete workflow execution report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Report schema version.
    pub schema_version: u32,
    /// Command that produced the report.
    pub command: String,
    /// Normalized selectors supplied to the command.
    pub selectors: Vec<String>,
    /// Timestamp recorded when execution started.
    pub started_at: String,
    /// Total elapsed time in milliseconds.
    pub duration: u64,
    /// Aggregate report status.
    pub status: ReportStatus,
    /// Ordered step reports.
    pub steps: Vec<StepReport>,
}

/// Aggregate workflow status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    /// Every applicable step passed.
    Passed,
    /// At least one step failed.
    Failed,
    /// The command or environment was invalid.
    Error,
    /// The capability is unavailable.
    Unavailable,
    /// Execution stopped after an interruption.
    Interrupted,
}

/// Result of one planned workflow step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepReport {
    /// Stable step identifier.
    pub id: String,
    /// Capability name.
    pub capability: String,
    /// Provider name.
    pub provider: String,
    /// Optional owning target.
    pub target: Option<String>,
    /// Optional owning component.
    pub component: Option<String>,
    /// Repository-relative working directory.
    pub cwd: String,
    /// Executed argument vector.
    pub argv: Vec<String>,
    /// Repository-relative configuration evidence.
    pub configuration_files: Vec<String>,
    /// Repository-relative provider targets.
    pub targets: Vec<String>,
    /// Step status.
    pub status: StepStatus,
    /// Provider exit code, when available.
    pub exit_code: Option<i32>,
    /// Step duration in milliseconds.
    pub duration: u64,
    /// Reason the step was skipped.
    pub skip_reason: Option<String>,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

/// Status of one workflow step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// The step completed successfully.
    Passed,
    /// The step completed unsuccessfully.
    Failed,
    /// The step was valid but not applicable.
    Skipped,
}

impl Report {
    /// Builds the standard unavailable-capability report.
    #[must_use]
    pub fn unavailable(command: &str, selectors: Vec<String>, started_at: String) -> Self {
        Self {
            schema_version: 1,
            command: command.to_owned(),
            selectors,
            started_at,
            duration: 0,
            status: ReportStatus::Unavailable,
            steps: Vec::new(),
        }
    }
}

/// Serializes a report as JSON.
///
/// # Errors
///
/// Returns the underlying serialization error when JSON encoding fails.
pub fn render_json(report: &Report) -> Result<String, serde_json::Error> {
    serde_json::to_string(report)
}

/// Renders a deterministic human-readable report.
#[must_use]
pub fn render_human(report: &Report) -> String {
    if report.status == ReportStatus::Unavailable {
        return format!(
            "error: command '{}' is not implemented in milestone 1\n",
            report.command
        );
    }

    let mut output = String::from("Command summary\n===============\n\n");
    for step in &report.steps {
        let _ = writeln!(
            output,
            "{} · {} · {} · {} · {} · {} ms",
            scope_label(step),
            step.capability,
            step.provider,
            step.cwd,
            status_label(step.status),
            step.duration
        );
        if let Some(reason) = &step.skip_reason {
            let _ = writeln!(output, "  {reason}");
        }
        if (report.command == "doctor" || step.provider == "specification-placement")
            && !step.stderr.is_empty()
        {
            for line in step.stderr.lines() {
                let _ = writeln!(output, "  {line}");
            }
        }
    }
    let _ = writeln!(
        output,
        "\n{} steps · {}",
        report.steps.len(),
        report_status_label(report.status)
    );
    output
}

fn scope_label(step: &StepReport) -> String {
    match (&step.target, &step.component) {
        (Some(target), Some(component)) => format!("{target}/{component}"),
        (Some(target), None) => target.clone(),
        (None, _) => "repository".to_owned(),
    }
}

const fn status_label(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Passed => "passed",
        StepStatus::Failed => "failed",
        StepStatus::Skipped => "skipped",
    }
}

const fn report_status_label(status: ReportStatus) -> &'static str {
    match status {
        ReportStatus::Passed => "passed",
        ReportStatus::Failed => "failed",
        ReportStatus::Error => "error",
        ReportStatus::Unavailable => "unavailable",
        ReportStatus::Interrupted => "interrupted",
    }
}
