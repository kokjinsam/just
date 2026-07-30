#![cfg(test)]
//! Integration tests for report rendering and exit-code mapping.

use kokjinsam_just::error::{
    EXIT_INTERRUPTED, EXIT_SUCCESS, EXIT_USAGE, EXIT_VERIFICATION_FAILED, WorkflowError,
};
use kokjinsam_just::report::{
    Report, ReportStatus, StepReport, StepStatus, render_human, render_json,
};

#[test]
fn json_reports_are_deterministic_and_include_every_frozen_field() {
    let report = Report {
        schema_version: 1,
        command: "lint".into(),
        selectors: vec!["@api:web".into()],
        started_at: "2026-07-25T00:00:00Z".into(),
        duration: 12,
        status: ReportStatus::Failed,
        steps: vec![StepReport {
            id: "lint-api-web-oxlint".into(),
            capability: "lint".into(),
            provider: "oxlint".into(),
            target: Some("api".into()),
            component: Some("web".into()),
            cwd: "apps/api/assets".into(),
            argv: vec!["oxlint".into(), ".".into()],
            configuration_files: vec!["oxlint.json".into()],
            targets: vec!["src".into()],
            status: StepStatus::Failed,
            exit_code: Some(1),
            duration: 12,
            skip_reason: None,
            stdout: "formatted\n".into(),
            stderr: "warning\n".into(),
        }],
    };

    assert_eq!(
        render_json(&report).unwrap(),
        concat!(
            r#"{"schema_version":1,"command":"lint","selectors":["@api:web"],"#,
            r#""started_at":"2026-07-25T00:00:00Z","duration":12,"status":"failed","#,
            r#""steps":[{"id":"lint-api-web-oxlint","capability":"lint","#,
            r#""provider":"oxlint","target":"api","component":"web","#,
            r#""cwd":"apps/api/assets","argv":["oxlint","."],"#,
            r#""configuration_files":["oxlint.json"],"targets":["src"],"#,
            r#""status":"failed","exit_code":1,"duration":12,"skip_reason":null,"#,
            r#""stdout":"formatted\n","stderr":"warning\n"}]}"#
        )
    );
}

#[test]
fn unavailable_reports_render_a_clear_human_diagnostic() {
    let report = Report::unavailable(
        "coverage",
        vec!["@api".into()],
        "2026-07-25T00:00:00Z".into(),
    );

    assert_eq!(report.status, ReportStatus::Unavailable);
    assert!(report.steps.is_empty());
    assert_eq!(
        render_human(&report),
        "error: command 'coverage' is not implemented in milestone 1\n"
    );
}

#[test]
fn human_summary_includes_target_component_scope_and_complete_step_details() {
    let report = Report {
        schema_version: 1,
        command: "lint".into(),
        selectors: vec!["@api:web".into()],
        started_at: "2026-07-25T00:00:00Z".into(),
        duration: 12,
        status: ReportStatus::Failed,
        steps: vec![StepReport {
            id: "lint-api-web-oxlint".into(),
            capability: "lint".into(),
            provider: "oxlint".into(),
            target: Some("api".into()),
            component: Some("web".into()),
            cwd: "apps/api/assets".into(),
            argv: vec!["oxlint".into(), ".".into()],
            configuration_files: vec!["oxlint.json".into()],
            targets: vec!["src".into()],
            status: StepStatus::Failed,
            exit_code: Some(1),
            duration: 12,
            skip_reason: None,
            stdout: String::new(),
            stderr: String::new(),
        }],
    };

    assert_eq!(
        render_human(&report),
        concat!(
            "Command summary\n===============\n\n",
            "api/web · lint · oxlint · apps/api/assets · failed · 12 ms\n",
            "\n1 steps · failed\n"
        )
    );
}

#[test]
fn human_summary_uses_repository_scope_when_target_and_component_are_absent() {
    let report = Report {
        schema_version: 1,
        command: "test".into(),
        selectors: Vec::new(),
        started_at: "2026-07-25T00:00:00Z".into(),
        duration: 7,
        status: ReportStatus::Passed,
        steps: vec![StepReport {
            id: "test-repository-cargo".into(),
            capability: "test".into(),
            provider: "cargo".into(),
            target: None,
            component: None,
            cwd: ".".into(),
            argv: vec!["cargo".into(), "test".into()],
            configuration_files: Vec::new(),
            targets: Vec::new(),
            status: StepStatus::Passed,
            exit_code: Some(0),
            duration: 7,
            skip_reason: None,
            stdout: String::new(),
            stderr: String::new(),
        }],
    };

    assert_eq!(
        render_human(&report),
        concat!(
            "Command summary\n===============\n\n",
            "repository · test · cargo · . · passed · 7 ms\n",
            "\n1 steps · passed\n"
        )
    );
}

#[test]
fn typed_errors_map_to_the_frozen_exit_codes() {
    assert_eq!(EXIT_SUCCESS, 0);
    assert_eq!(
        WorkflowError::VerificationFailed.exit_code(),
        EXIT_VERIFICATION_FAILED
    );
    assert_eq!(
        WorkflowError::Configuration("bad config".into()).exit_code(),
        EXIT_USAGE
    );
    assert_eq!(
        WorkflowError::Workspace("not found".into()).exit_code(),
        EXIT_USAGE
    );
    assert_eq!(
        WorkflowError::Selector("unknown".into()).exit_code(),
        EXIT_USAGE
    );
    assert_eq!(WorkflowError::Interrupted.exit_code(), EXIT_INTERRUPTED);
}
