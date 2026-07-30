#![cfg(test)]
//! Integration tests for CLI dispatch and output contracts.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixture(name: &str) -> PathBuf {
    Path::new("tests/fixtures")
        .join(name)
        .canonicalize()
        .unwrap()
}

fn workflow() -> Command {
    Command::new(env!("CARGO_BIN_EXE_workflow"))
}

fn run(args: &[&str], current_dir: &Path) -> Output {
    workflow()
        .args(args)
        .current_dir(current_dir)
        .env_remove("JUST_CALLER_DIR")
        .output()
        .unwrap()
}

#[test]
fn every_foundation_command_is_recognized_and_fails_closed() {
    let root = fixture("valid-minimal");

    let format = run(&["format"], &root);
    assert_eq!(format.status.code(), Some(0));
    assert!(format.stdout.is_empty());
    assert!(
        String::from_utf8(format.stderr)
            .unwrap()
            .contains("0 steps · passed")
    );

    for command in ["lint", "doctor"] {
        let output = run(&[command], &root);
        assert_eq!(output.status.code(), Some(0), "{command}");
        assert!(String::from_utf8(output.stderr).unwrap().contains("passed"));
    }

    for command in ["typecheck", "compile"] {
        let output = run(&[command], &root);
        assert_eq!(output.status.code(), Some(0), "{command}");
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("1 steps · passed")
        );
    }

    let spec = run(&["spec"], &root);
    assert_eq!(spec.status.code(), Some(0));
    assert!(
        String::from_utf8(spec.stderr)
            .unwrap()
            .contains("0 steps · passed")
    );

    for command in ["test", "coverage"] {
        let output = run(&[command], &root);

        assert_eq!(output.status.code(), Some(0), "{command}");
        assert!(output.stdout.is_empty(), "{command}");
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("0 steps · passed")
        );
    }
    let verify = run(&["verify"], &root);
    assert_eq!(verify.status.code(), Some(2));
}

#[test]
fn applicable_selectors_are_validated_before_the_unavailable_diagnostic() {
    let root = fixture("valid-components");
    let nested = root.join("apps/api");

    let valid = run(&["lint", "@api:web", "assets/src"], &nested);
    assert_eq!(valid.status.code(), Some(0));
    assert!(
        String::from_utf8(valid.stderr)
            .unwrap()
            .contains("0 steps · passed")
    );

    let invalid = run(&["lint", "@unknown"], &nested);
    assert_eq!(invalid.status.code(), Some(2));
    let diagnostic = String::from_utf8(invalid.stderr).unwrap();
    assert!(diagnostic.contains("unknown target '@unknown'"));
    assert!(!diagnostic.contains("not implemented"));
}

#[test]
fn json_mode_keeps_stdout_machine_readable_and_stderr_empty() {
    let root = fixture("valid-components");
    let output = run(&["lint", "@api:web", "--output", "json"], &root);

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["command"], "lint");
    assert_eq!(report["selectors"], serde_json::json!(["@api:web"]));
    assert_eq!(report["status"], "passed");
    assert_eq!(report["steps"], serde_json::json!([]));
}

#[test]
fn just_caller_dir_controls_workspace_and_path_resolution() {
    let root = fixture("valid-components");
    let caller = root.join("apps/api");
    let output = workflow()
        .args(["lint", "assets/src"])
        .current_dir(Path::new("/"))
        .env("JUST_CALLER_DIR", &caller)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("0 steps · passed")
    );
}

#[test]
fn invalid_configuration_and_workspace_fail_with_usage_exit_code() {
    let invalid = run(&["lint"], &fixture("invalid-version"));
    assert_eq!(invalid.status.code(), Some(2));
    assert!(
        String::from_utf8(invalid.stderr)
            .unwrap()
            .contains("only version 1 is supported")
    );

    let outside = tempfile::tempdir().unwrap();
    let missing = run(&["lint"], outside.path());
    assert_eq!(missing.status.code(), Some(2));
    assert!(
        String::from_utf8(missing.stderr)
            .unwrap()
            .contains("could not find 'just.yaml'")
    );
}

#[test]
fn help_succeeds_but_invalid_usage_exits_two() {
    let root = fixture("valid-minimal");
    let help = run(&["--help"], &root);
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("Usage: workflow")
    );

    let invalid = run(&["spec", "@website"], &root);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(
        String::from_utf8(invalid.stderr)
            .unwrap()
            .contains("invalid value")
    );
}

#[test]
#[cfg(unix)]
fn foundation_commands_never_invoke_external_ecosystem_tools() {
    use std::os::unix::fs::PermissionsExt;

    let root = fixture("valid-minimal");
    let fake_bin = tempfile::tempdir().unwrap();
    let marker = fake_bin.path().join("invoked");
    for executable in ["mix", "pnpm", "cargo", "uv", "process-compose"] {
        let executable_path = fake_bin.path().join(executable);
        fs::write(
            &executable_path,
            format!("#!/bin/sh\nprintf invoked >> '{}'\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&executable_path, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            fs::metadata(&executable_path).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    for command in [
        "format",
        "lint",
        "typecheck",
        "compile",
        "spec",
        "test",
        "coverage",
        "verify",
        "doctor",
    ] {
        let output = workflow()
            .arg(command)
            .current_dir(&root)
            .env_remove("JUST_CALLER_DIR")
            .env("PATH", fake_bin.path())
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(
                if matches!(
                    command,
                    "format"
                        | "lint"
                        | "typecheck"
                        | "compile"
                        | "spec"
                        | "test"
                        | "coverage"
                        | "doctor"
                ) {
                    0
                } else {
                    2
                }
            ),
            "{command}"
        );
    }

    assert!(!marker.exists());
}
