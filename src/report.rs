use std::fmt::Write;

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub command: String,
    pub selectors: Vec<String>,
    pub started_at: String,
    pub duration: u64,
    pub status: ReportStatus,
    pub steps: Vec<StepReport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Passed,
    Failed,
    Error,
    Unavailable,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepReport {
    pub id: String,
    pub capability: String,
    pub provider: String,
    pub target: Option<String>,
    pub component: Option<String>,
    pub cwd: String,
    pub argv: Vec<String>,
    pub configuration_files: Vec<String>,
    pub targets: Vec<String>,
    pub status: StepStatus,
    pub exit_code: Option<i32>,
    pub duration: u64,
    pub skip_reason: Option<String>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Passed,
    Failed,
    Skipped,
}

impl Report {
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

pub fn render_json(report: &Report) -> Result<String, serde_json::Error> {
    serde_json::to_string(report)
}

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
