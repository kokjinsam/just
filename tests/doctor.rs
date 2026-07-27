#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

fn workflow() -> Command {
    Command::new(env!("CARGO_BIN_EXE_workflow"))
}

fn executable(path: &Path, marker: &Path) {
    fs::write(
        path,
        format!(
            "#!/bin/sh\nprintf invoked >> '{}'\nexit 0\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn repository() -> tempfile::TempDir {
    let repository = tempfile::tempdir().unwrap();
    for directory in ["app/assets", "app/native", "specifications", "tests"] {
        fs::create_dir_all(repository.path().join(directory)).unwrap();
    }
    fs::write(
        repository.path().join("just.yaml"),
        concat!(
            "version: 1\n",
            "system:\n  specs: specifications\n  tests: tests\n",
            "targets:\n  app:\n    path: app\n",
        ),
    )
    .unwrap();
    fs::write(repository.path().join("app/mix.exs"), "Mix.Project").unwrap();
    fs::write(repository.path().join("app/.credo.exs"), "config").unwrap();
    fs::write(
        repository.path().join("app/assets/package.json"),
        r#"{"devDependencies":{"oxfmt":"1","oxlint":"1","stylelint":"1"}}"#,
    )
    .unwrap();
    fs::write(
        repository.path().join("app/assets/oxlint.config.js"),
        "module.exports={}",
    )
    .unwrap();
    fs::write(
        repository.path().join("app/assets/stylelint.config.js"),
        "module.exports={}",
    )
    .unwrap();
    fs::write(
        repository.path().join("app/native/Cargo.toml"),
        "[package]\nname='native'\nversion='0.1.0'\n",
    )
    .unwrap();
    repository
}

fn tools(repository: &Path) -> tempfile::TempDir {
    let tools = tempfile::tempdir().unwrap();
    let marker = repository.join("launched");
    for name in [
        "mix",
        "pnpm",
        "cargo",
        "cargo-fmt",
        "cargo-clippy",
        "cargo-dylint",
        "dylint-link",
        "java",
    ] {
        executable(&tools.path().join(name), &marker);
    }
    tools
}

fn run(repository: &Path, tools: &Path, output: &str) -> Output {
    workflow()
        .args(["doctor", "--output", output])
        .current_dir(repository)
        .env_remove("JUST_CALLER_DIR")
        .env_remove("TLA2TOOLS_JAR")
        .env("PATH", tools)
        .output()
        .unwrap()
}

#[test]
fn optional_dylint_is_skipped_and_doctor_launches_no_tools() {
    let repository = repository();
    let tools = tools(repository.path());
    let output = run(repository.path(), tools.path(), "json");

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert!(!repository.path().join("launched").exists());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "passed");
    let dylint = report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["provider"] == "dylint")
        .unwrap();
    assert_eq!(dylint["status"], "skipped");
    assert!(
        dylint["skip_reason"]
            .as_str()
            .unwrap()
            .contains("not applicable")
    );
    assert!(
        report["steps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|step| step["capability"] == "doctor-check")
    );
    for provider in ["dialyzer", "mix-compile", "cargo-check"] {
        assert_eq!(
            report["steps"]
                .as_array()
                .unwrap()
                .iter()
                .find(|step| step["provider"] == provider)
                .unwrap()["status"],
            "passed",
            "{provider}"
        );
    }
    for provider in ["package-typecheck", "package-build"] {
        let finding = report["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|step| step["provider"] == provider)
            .unwrap();
        assert_eq!(finding["status"], "skipped", "{provider}");
        assert!(
            finding["skip_reason"]
                .as_str()
                .unwrap()
                .contains("not applicable")
        );
    }
}

#[test]
fn declared_node_checking_scripts_require_pnpm_without_launching_it() {
    let repository = repository();
    fs::write(
        repository.path().join("app/assets/package.json"),
        r#"{"scripts":{"typecheck":"tsc","build":"vite build"}}"#,
    )
    .unwrap();
    let tools = tools(repository.path());
    let ready = run(repository.path(), tools.path(), "json");
    assert_eq!(ready.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&ready.stdout).unwrap();
    for provider in ["package-typecheck", "package-build"] {
        assert_eq!(
            report["steps"]
                .as_array()
                .unwrap()
                .iter()
                .find(|step| step["provider"] == provider)
                .unwrap()["status"],
            "passed"
        );
    }
    assert!(!repository.path().join("launched").exists());

    fs::remove_file(tools.path().join("pnpm")).unwrap();
    let missing = run(repository.path(), tools.path(), "json");
    assert_eq!(missing.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&missing.stdout).unwrap();
    for provider in ["package-typecheck", "package-build"] {
        let finding = report["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|step| step["provider"] == provider)
            .unwrap();
        assert_eq!(finding["status"], "failed");
        assert!(finding["stderr"].as_str().unwrap().contains("pnpm"));
    }
    assert!(!repository.path().join("launched").exists());
}

#[test]
fn configured_dylint_ready_and_missing_executable_states_are_aggregated() {
    let repository = repository();
    fs::write(
        repository.path().join("app/native/Cargo.toml"),
        "[workspace]\n[workspace.metadata.dylint]\nlibraries=[{path='lints'}]\n",
    )
    .unwrap();
    let tools = tools(repository.path());
    let ready = run(repository.path(), tools.path(), "json");
    assert_eq!(ready.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&ready.stdout).unwrap();
    let dylint = report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["provider"] == "dylint")
        .unwrap();
    assert_eq!(dylint["status"], "passed");
    assert!(!repository.path().join("launched").exists());

    fs::remove_file(tools.path().join("cargo-clippy")).unwrap();
    let missing_clippy = run(repository.path(), tools.path(), "json");
    assert_eq!(missing_clippy.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&missing_clippy.stdout).unwrap();
    let clippy = report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["provider"] == "clippy")
        .unwrap();
    assert_eq!(clippy["status"], "failed");
    assert!(clippy["stderr"].as_str().unwrap().contains("cargo-clippy"));
    executable(
        &tools.path().join("cargo-clippy"),
        &repository.path().join("launched"),
    );

    fs::remove_file(tools.path().join("cargo-dylint")).unwrap();
    let missing = run(repository.path(), tools.path(), "json");
    assert_eq!(missing.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&missing.stdout).unwrap();
    let dylint = report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["provider"] == "dylint")
        .unwrap();
    assert_eq!(dylint["status"], "failed");
    assert!(dylint["stderr"].as_str().unwrap().contains("cargo-dylint"));
}

#[test]
fn malformed_dylint_is_a_doctor_finding_not_a_global_usage_error() {
    let repository = repository();
    fs::write(
        repository.path().join("app/native/dylint.toml"),
        "[workspace.metadata.dylint]\nlibraries=[]\n",
    )
    .unwrap();
    let tools = tools(repository.path());
    let output = run(repository.path(), tools.path(), "human");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.contains("doctor-check · dylint"));
    assert!(diagnostic.contains("Dylint declaration"));
    assert!(!repository.path().join("launched").exists());
}

#[test]
fn tla_tools_are_required_only_when_tla_formatting_applies() {
    let repository = repository();
    let tools = tools(repository.path());
    fs::remove_file(tools.path().join("java")).unwrap();

    let without_tla = run(repository.path(), tools.path(), "json");
    assert_eq!(without_tla.status.code(), Some(0));

    fs::write(
        repository.path().join("specifications/Model.tla"),
        "---- MODULE Model ----\n====\n",
    )
    .unwrap();
    let with_tla = run(repository.path(), tools.path(), "json");
    assert_eq!(with_tla.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&with_tla.stdout).unwrap();
    let tla = report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["provider"] == "tla-format")
        .unwrap();
    assert_eq!(tla["status"], "failed");
    let diagnostic = tla["stderr"].as_str().unwrap();
    assert!(diagnostic.contains("java"));
    assert!(diagnostic.contains("TLA2TOOLS_JAR"));
}
