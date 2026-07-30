#![cfg(test)]
#![cfg(unix)]
//! Integration tests for doctor diagnostics.

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
    fs::write(
        repository.path().join("app/mix.exs"),
        "defmodule Demo.MixProject do\n  use Mix.Project\n  def project, do: [test_coverage: [tool: ExCoveralls]]\n  defp deps, do: [{:excoveralls, \"0.18.5\", only: :test}]\nend\n",
    )
    .unwrap();
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
        "cargo-nextest",
        "cargo-llvm-cov",
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
    for provider in [
        "dialyzer",
        "mix-compile",
        "cargo-check",
        "exunit",
        "excoveralls",
        "nextest",
        "llvm-cov",
    ] {
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
fn target_test_doctor_checks_exclude_the_application_test_workspace() {
    let repository = repository();
    fs::write(
        repository.path().join("tests/package.json"),
        r#"{"scripts":{"test":"vitest"},"devDependencies":{"vitest":"4"}}"#,
    )
    .unwrap();
    let tools = tools(repository.path());
    let output = run(repository.path(), tools.path(), "json");
    assert_eq!(output.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["steps"].as_array().unwrap().iter().all(|step| {
        !matches!(
            step["provider"].as_str(),
            Some(
                "package-test"
                    | "vitest-coverage"
                    | "nextest"
                    | "llvm-cov"
                    | "exunit"
                    | "excoveralls"
            )
        ) || step["cwd"] != "tests"
    }));
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
fn target_coverage_adapters_are_reported_without_launching_tools() {
    let repository = repository();
    fs::write(
        repository.path().join("app/assets/package.json"),
        r#"{"scripts":{"test":"vitest"},"devDependencies":{"vitest":"4"}}"#,
    )
    .unwrap();
    let tools = tools(repository.path());
    let missing = run(repository.path(), tools.path(), "json");
    assert_eq!(missing.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&missing.stdout).unwrap();
    let coverage = report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["provider"] == "vitest-coverage")
        .unwrap();
    assert_eq!(coverage["status"], "failed");
    assert!(
        coverage["stderr"]
            .as_str()
            .unwrap()
            .contains("coverage adapter")
    );
    assert!(!repository.path().join("launched").exists());

    fs::write(
        repository.path().join("app/assets/package.json"),
        r#"{"scripts":{"test":"vitest"},"devDependencies":{"vitest":"4","@vitest/coverage-istanbul":"4"}}"#,
    )
    .unwrap();
    let ready = run(repository.path(), tools.path(), "json");
    assert_eq!(ready.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&ready.stdout).unwrap();
    assert_eq!(
        report["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|step| step["provider"] == "vitest-coverage")
            .unwrap()["status"],
        "passed"
    );
    assert!(!repository.path().join("launched").exists());
}

#[test]
fn excoveralls_accepts_end_inside_project_atom_without_launching_tools() {
    let repository = repository();
    fs::write(
        repository.path().join("app/mix.exs"),
        concat!(
            "defmodule Demo.MixProject do\n",
            "  use Mix.Project\n",
            "  def project,\n",
            "    do: [\n",
            "      app: :backend,\n",
            "      test_coverage: [tool: ExCoveralls]\n",
            "    ]\n",
            "  defp deps, do: [{:excoveralls, \"0.18.5\", only: :test}]\n",
            "end\n",
        ),
    )
    .unwrap();
    let tools = tools(repository.path());

    let output = run(repository.path(), tools.path(), "json");

    assert_eq!(output.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|step| step["provider"] == "excoveralls")
            .unwrap()["status"],
        "passed"
    );
    assert!(!repository.path().join("launched").exists());
}

#[test]
fn excoveralls_readiness_failures_are_reported_without_launching_tools() {
    let cases = [
        (
            "defmodule Demo.MixProject do\n  def project, do: [test_coverage: [tool: ExCoveralls]]\n  defp deps, do: [{:excoveralls, \"0.18.5\", only: :dev}]\nend\n",
            None,
            "ExCoveralls dependency is not available in MIX_ENV=test",
        ),
        (
            "defmodule Demo.MixProject do\n  def project, do: []\n  defp deps, do: [{:excoveralls, \"0.18.5\", only: :test}]\nend\n",
            None,
            "Mix project does not declare test_coverage: [tool: ExCoveralls]",
        ),
        (
            "defmodule Demo.MixProject do\n  # def project, do: [test_coverage: [tool: ExCoveralls]]\n  def project, do: []\n  # {:excoveralls, \"0.18.5\", only: :test}\n  defp deps, do: []\nend\n",
            None,
            "ExCoveralls dependency is not available in MIX_ENV=test",
        ),
        (
            "defmodule Demo.MixProject do\n  def project, do: [test_coverage: [tool: ExCoveralls]]\n  defp deps, do: [{:excoveralls, \"0.18.5\"}]\nend\n",
            Some("{invalid"),
            "cannot parse",
        ),
        (
            "defmodule Demo.MixProject do\n  def project, do: [test_coverage: [tool: ExCoveralls]]\n  defp deps, do: [{:excoveralls, \"0.18.5\"}]\nend\n",
            Some("[]"),
            "must contain a JSON object",
        ),
        (
            "defmodule Demo.MixProject do\n  def project, do: [test_coverage: [tool: ExCoveralls]]\n  defp deps, do: [{:excoveralls, \"0.18.5\"}]\nend\n",
            Some(r#"{"coverage_options":[]}"#),
            "coverage_options must contain a JSON object",
        ),
    ];

    for (manifest, coveralls, expected) in cases {
        let repository = repository();
        fs::write(repository.path().join("app/mix.exs"), manifest).unwrap();
        if let Some(source) = coveralls {
            fs::write(repository.path().join("app/coveralls.json"), source).unwrap();
        }
        let tools = tools(repository.path());
        let output = run(repository.path(), tools.path(), "json");
        assert_eq!(output.status.code(), Some(1), "{expected}");
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let finding = report["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|step| step["provider"] == "excoveralls")
            .unwrap();
        assert_eq!(finding["status"], "failed");
        assert!(
            finding["stderr"].as_str().unwrap().contains(expected),
            "{}",
            finding["stderr"]
        );
        assert!(!repository.path().join("launched").exists(), "{expected}");
    }
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

#[test]
fn specification_tools_and_local_configuration_are_reported_only_when_applicable() {
    let repository = repository();
    fs::create_dir_all(repository.path().join("specifications/tla")).unwrap();
    fs::create_dir_all(repository.path().join("specifications/alloy")).unwrap();
    fs::create_dir_all(repository.path().join("specifications/openapi")).unwrap();
    fs::write(
        repository.path().join("specifications/tla/Model.tla"),
        "---- MODULE Model ----\n====\n",
    )
    .unwrap();
    fs::write(
        repository.path().join("specifications/tla/Model.cfg"),
        "INIT Init\n",
    )
    .unwrap();
    fs::write(
        repository.path().join("specifications/tla/.apalache.cfg"),
        "checker.tuning.search.smt.encoding = \"oops\"\n",
    )
    .unwrap();
    fs::write(
        repository.path().join("specifications/alloy/model.als"),
        "run {} expect 1\n",
    )
    .unwrap();
    fs::write(
        repository
            .path()
            .join("specifications/openapi/vacuum.conf.yaml"),
        "rules: {}\n",
    )
    .unwrap();
    fs::write(
        repository.path().join("specifications/openapi/api.yaml"),
        "openapi: 3.1.0\n",
    )
    .unwrap();
    let tools = tools(repository.path());
    let output = run(repository.path(), tools.path(), "json");
    assert_eq!(output.status.code(), Some(1));
    assert!(!repository.path().join("launched").exists());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    for provider in ["apalache", "alloy", "oasdiff", "vacuum"] {
        let step = report["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|step| step["provider"] == provider)
            .unwrap();
        assert_eq!(step["status"], "failed", "{provider}");
        assert!(
            step["stderr"].as_str().unwrap().contains(match provider {
                "apalache" => "apalache-mc",
                other => other,
            }),
            "{provider}"
        );
    }
    let apalache = report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["provider"] == "apalache")
        .unwrap();
    assert_eq!(
        apalache["configuration_files"],
        serde_json::json!([
            "specifications/tla/Model.cfg",
            "specifications/tla/.apalache.cfg"
        ])
    );
    let vacuum = report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["provider"] == "vacuum")
        .unwrap();
    assert_eq!(
        vacuum["configuration_files"],
        serde_json::json!(["specifications/openapi/vacuum.conf.yaml"])
    );
}
