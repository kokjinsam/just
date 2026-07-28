#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use kokjinsam_just::selector::resolve_selectors;
use kokjinsam_just::target_testing::{TestMode, TestSuite, build_plan};
use kokjinsam_just::workspace::Workspace;

fn workflow() -> Command {
    Command::new(env!("CARGO_BIN_EXE_workflow"))
}

fn write(path: impl AsRef<Path>, source: &str) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
}

fn repository() -> tempfile::TempDir {
    let repository = tempfile::tempdir().unwrap();
    for directory in [
        "apps/api/test",
        "apps/api/lib",
        "apps/api/assets/src",
        "apps/api/assets/tests",
        "apps/api/native/src",
        "specs",
        "application-tests",
    ] {
        fs::create_dir_all(repository.path().join(directory)).unwrap();
    }
    write(
        repository.path().join("just.yaml"),
        concat!(
            "version: 1\n",
            "system:\n  specs: specs\n  tests: application-tests\n",
            "coverage:\n  lines: 80\n",
            "targets:\n  api:\n    path: apps/api\n    components:\n",
            "      backend:\n        path: .\n",
            "      web:\n        path: assets\n        coverage:\n          lines: 90\n",
            "      native:\n        path: native\n",
        ),
    );
    write(
        repository.path().join("apps/api/mix.exs"),
        "defmodule Demo.MixProject do\n  use Mix.Project\n  def project, do: [test_coverage: [tool: ExCoveralls]]\n  defp deps, do: [{:excoveralls, \"0.18.5\", only: :test}]\nend\n",
    );
    write(
        repository.path().join("apps/api/coveralls.json"),
        r#"{"skip_files":["test/support"],"coverage_options":{"minimum_coverage":12,"floor_coverage":false}}"#,
    );
    write(
        repository.path().join("apps/api/test/review_test.exs"),
        "@moduletag :unit\n@tag integration: true\n",
    );
    write(
        repository.path().join("apps/api/lib/review.ex"),
        "defmodule Review do end\n",
    );
    write(
        repository.path().join("apps/api/assets/package.json"),
        r#"{"scripts":{"test":"vitest","test:unit":"vitest --project unit"},"devDependencies":{"vitest":"4","@vitest/coverage-v8":"4"}}"#,
    );
    write(
        repository
            .path()
            .join("apps/api/assets/tests/button.test.ts"),
        "test('button', () => {})\n",
    );
    write(
        repository.path().join("apps/api/assets/src/button.ts"),
        "export {}\n",
    );
    write(
        repository.path().join("apps/api/native/Cargo.toml"),
        "[package]\nname='native'\nversion='0.1.0'\n[dev-dependencies]\nproptest='1'\n",
    );
    write(
        repository.path().join("apps/api/native/src/lib.rs"),
        "pub fn parse() {}\n",
    );
    write(
        repository.path().join("application-tests/package.json"),
        r#"{"scripts":{"test":"vitest"},"devDependencies":{"vitest":"4"}}"#,
    );
    repository
}

fn tools() -> tempfile::TempDir {
    let tools = tempfile::tempdir().unwrap();
    let script = r#"#!/bin/sh
name=${0##*/}
printf '%s|%s|' "$name" "$PWD" >> "$JUST_TEST_LOG"
printf '%s ' "$@" >> "$JUST_TEST_LOG"
printf '|config=%s\n' "$JUST_EXCOVERALLS_CONFIG" >> "$JUST_TEST_LOG"
if [ -n "$JUST_CAPTURE_CONFIG" ] && [ -n "$JUST_EXCOVERALLS_CONFIG" ]; then
  /bin/cp "$JUST_EXCOVERALLS_CONFIG" "$JUST_CAPTURE_CONFIG"
  printf '%s' "$JUST_EXCOVERALLS_CONFIG" > "$JUST_CAPTURE_PATH"
  /usr/bin/stat -f '%Lp' "$JUST_EXCOVERALLS_CONFIG" > "$JUST_CAPTURE_MODE"
fi
case "$JUST_STUB_BEHAVIOR:$name:$1" in
  fail-mix:mix:*) exit 7 ;;
  fail-pnpm:pnpm:*) exit 8 ;;
  signal2:mix:*) kill -INT $$ ;;
  signal2-descendant:mix:*)
    /bin/sleep 30 &
    printf '%s' "$!" > "$JUST_DESCENDANT_PID"
    kill -INT $$
    ;;
  exit130:mix:*) exit 130 ;;
  hang:mix:*)
    /bin/sleep 30 &
    printf '%s' "$!" > "$JUST_DESCENDANT_PID"
    wait
    ;;
esac
exit 0
"#;
    for name in ["mix", "pnpm", "cargo", "cargo-nextest", "cargo-llvm-cov"] {
        let path = tools.path().join(name);
        write(&path, script);
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    tools
}

fn run(
    repository: &Path,
    tools: &Path,
    log: &Path,
    args: &[&str],
    behavior: Option<&str>,
) -> Output {
    let mut command = workflow();
    command
        .args(args)
        .current_dir(repository)
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools)
        .env("JUST_TEST_LOG", log);
    if let Some(behavior) = behavior {
        command.env("JUST_STUB_BEHAVIOR", behavior);
    }
    command.output().unwrap()
}

fn plan(
    repository: &Path,
    selectors: &[&str],
    suite: Option<TestSuite>,
    mode: TestMode,
) -> kokjinsam_just::plan::Plan {
    let workspace = Workspace::discover(repository).unwrap();
    let selectors = selectors
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    let selections = resolve_selectors(&workspace, &selectors).unwrap();
    build_plan(&workspace, &selections, suite, &[], mode).unwrap()
}

#[test]
fn target_and_component_selection_are_polyglot_and_deterministic() {
    let repository = repository();
    let target = plan(repository.path(), &["@api"], None, TestMode::Test);
    assert_eq!(
        target
            .steps()
            .iter()
            .map(|step| step.provider.as_str())
            .collect::<Vec<_>>(),
        ["exunit", "package-test", "nextest"]
    );
    assert!(target.steps().iter().all(|step| {
        !step
            .cwd
            .starts_with(repository.path().join("application-tests"))
            && !step.cwd.starts_with(repository.path().join("specs"))
    }));

    let native = plan(repository.path(), &["@api:native"], None, TestMode::Test);
    assert_eq!(native.steps().len(), 1);
    assert_eq!(native.steps()[0].provider.as_str(), "nextest");
    assert_eq!(
        native.steps()[0]
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>(),
        ["nextest", "run"]
    );
}

#[test]
fn direct_test_files_merge_and_source_or_rust_paths_run_full_owner_tests() {
    let repository = repository();
    write(
        repository.path().join("apps/api/test/other_test.exs"),
        "@tag :unit\n",
    );
    let direct = plan(
        repository.path(),
        &[
            "apps/api/test/review_test.exs",
            "apps/api/test/other_test.exs",
        ],
        None,
        TestMode::Test,
    );
    assert_eq!(direct.steps().len(), 1);
    assert_eq!(direct.steps()[0].targets.len(), 2);
    assert_eq!(
        direct.steps()[0]
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>(),
        ["test", "test/other_test.exs", "test/review_test.exs"]
    );

    let full = plan(
        repository.path(),
        &["apps/api/test/review_test.exs", "apps/api/lib/review.ex"],
        None,
        TestMode::Test,
    );
    assert!(full.steps()[0].targets.is_empty());
    assert_eq!(full.steps()[0].argv, [OsString::from("test")]);

    let rust = plan(
        repository.path(),
        &["apps/api/native/src/lib.rs"],
        None,
        TestMode::Test,
    );
    assert!(rust.steps()[0].targets.is_empty());

    let vitest = plan(
        repository.path(),
        &["apps/api/assets/tests/button.test.ts"],
        None,
        TestMode::Test,
    );
    assert_eq!(
        vitest.steps()[0].argv,
        ["run", "test", "tests/button.test.ts"]
    );
}

#[test]
fn suite_evidence_filters_providers_and_rejects_zero_applicability() {
    let repository = repository();
    let unit = plan(
        repository.path(),
        &["@api"],
        Some(TestSuite::Unit),
        TestMode::Test,
    );
    assert_eq!(
        unit.steps()
            .iter()
            .map(|step| step.provider.as_str())
            .collect::<Vec<_>>(),
        ["exunit", "package-test", "not-applicable"]
    );
    assert_eq!(unit.steps()[0].argv, ["test", "--only", "unit"]);
    assert_eq!(unit.steps()[1].argv, ["run", "test:unit"]);

    let property = plan(
        repository.path(),
        &["@api:native"],
        Some(TestSuite::Property),
        TestMode::Test,
    );
    assert_eq!(property.steps()[0].argv, ["nextest", "run"]);

    let workspace = Workspace::discover(repository.path()).unwrap();
    let selections = resolve_selectors(&workspace, &["@api:native".into()]).unwrap();
    let error = build_plan(
        &workspace,
        &selections,
        Some(TestSuite::Model),
        &[],
        TestMode::Test,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("none of the selected components")
    );
}

#[test]
fn ignored_explicit_exunit_file_can_supply_suite_evidence() {
    let repository = repository();
    write(
        repository.path().join(".gitignore"),
        "apps/api/test/ignored_test.exs\n",
    );
    write(
        repository.path().join("apps/api/test/ignored_test.exs"),
        "@tag :model\n",
    );
    let explicit = plan(
        repository.path(),
        &["apps/api/test/ignored_test.exs"],
        Some(TestSuite::Model),
        TestMode::Test,
    );
    assert_eq!(explicit.steps().len(), 1);
    assert_eq!(
        explicit.steps()[0].argv,
        ["test", "--only", "model", "test/ignored_test.exs"]
    );
}

#[test]
fn coverage_uses_per_component_thresholds_and_full_suites() {
    let repository = repository();
    let coverage = plan(repository.path(), &["@api"], None, TestMode::Coverage);
    assert_eq!(
        coverage
            .steps()
            .iter()
            .map(|step| step.provider.as_str())
            .collect::<Vec<_>>(),
        ["excoveralls", "vitest-coverage", "llvm-cov"]
    );
    assert_eq!(
        coverage.steps()[0].env[OsString::from("MIX_ENV").as_os_str()],
        "test"
    );
    assert_eq!(
        coverage.steps()[0].env[OsString::from("JUST_COVERAGE_LINES").as_os_str()],
        "80"
    );
    assert_eq!(
        coverage.steps()[1]
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>(),
        [
            "run",
            "test",
            "--coverage.enabled",
            "--coverage.thresholds.lines=90"
        ]
    );
    assert_eq!(
        coverage.steps()[2]
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>(),
        ["llvm-cov", "nextest", "--fail-under-lines", "80"]
    );
    assert!(coverage.steps().iter().all(|step| step.targets.is_empty()));
}

#[test]
fn absent_threshold_omits_native_flags_but_forces_excoveralls_zero() {
    let repository = repository();
    let config = fs::read_to_string(repository.path().join("just.yaml")).unwrap();
    write(
        repository.path().join("just.yaml"),
        &config
            .replace("coverage:\n  lines: 80\n", "")
            .replace("        coverage:\n          lines: 90\n", ""),
    );
    let coverage = plan(repository.path(), &["@api"], None, TestMode::Coverage);
    assert_eq!(
        coverage.steps()[0].env[OsString::from("JUST_COVERAGE_LINES").as_os_str()],
        "0"
    );
    assert_eq!(
        coverage.steps()[1].argv,
        ["run", "test", "--coverage.enabled"]
    );
    assert_eq!(coverage.steps()[2].argv, ["llvm-cov", "nextest"]);
}

#[test]
fn missing_coverage_adapter_fails_closed() {
    let repository = repository();
    write(
        repository.path().join("apps/api/assets/package.json"),
        r#"{"scripts":{"test":"vitest"},"devDependencies":{"vitest":"4"}}"#,
    );
    let workspace = Workspace::discover(repository.path()).unwrap();
    let selections = resolve_selectors(&workspace, &["@api:web".into()]).unwrap();
    let error = build_plan(&workspace, &selections, None, &[], TestMode::Coverage).unwrap_err();
    assert!(error.to_string().contains("Vitest coverage adapter"));
}

#[test]
fn excoveralls_accepts_unrestricted_and_test_only_dependencies() {
    for dependency in [
        r#"{:excoveralls, "0.18.5"}"#,
        r#"{:excoveralls, "0.18.5", only: :test}"#,
        r#"{:excoveralls, "0.18.5", only: [:dev, :test]}"#,
    ] {
        let repository = repository();
        write(
            repository.path().join("apps/api/mix.exs"),
            &format!(
                "defmodule Demo.MixProject do\n  use Mix.Project\n  def project do\n    [test_coverage: [tool: ExCoveralls, summary: true]]\n  end\n  defp deps, do: [{dependency}]\nend\n"
            ),
        );
        let coverage = plan(
            repository.path(),
            &["@api:backend"],
            None,
            TestMode::Coverage,
        );
        assert_eq!(coverage.steps()[0].provider.as_str(), "excoveralls");
    }
}

#[test]
fn excoveralls_accepts_end_inside_project_atom() {
    let repository = repository();
    write(
        repository.path().join("apps/api/mix.exs"),
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
    );

    let coverage = plan(
        repository.path(),
        &["@api:backend"],
        None,
        TestMode::Coverage,
    );

    assert_eq!(coverage.steps().len(), 1);
    assert_eq!(coverage.steps()[0].provider.as_str(), "excoveralls");
}

#[test]
fn invalid_excoveralls_readiness_fails_before_any_provider_launches() {
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
        write(repository.path().join("apps/api/mix.exs"), manifest);
        let coveralls_path = repository.path().join("apps/api/coveralls.json");
        match coveralls {
            Some(source) => write(&coveralls_path, source),
            None => fs::remove_file(&coveralls_path).unwrap(),
        }
        let tools = tools();
        let log = repository.path().join("commands.log");
        let output = run(
            repository.path(),
            tools.path(),
            &log,
            &["coverage", "@api:backend"],
            None,
        );
        assert_eq!(output.status.code(), Some(2), "{expected}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!log.exists(), "{expected}");
    }
}

#[test]
fn absent_coveralls_json_is_ready() {
    let repository = repository();
    fs::remove_file(repository.path().join("apps/api/coveralls.json")).unwrap();
    let tools = tools();
    let log = repository.path().join("commands.log");
    let output = run(
        repository.path(),
        tools.path(),
        &log,
        &["coverage", "@api:backend"],
        None,
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(fs::read_to_string(log).unwrap().lines().count(), 1);
}

#[test]
fn passthrough_requires_one_executable_step() {
    let repository = repository();
    let workspace = Workspace::discover(repository.path()).unwrap();
    let one = resolve_selectors(&workspace, &["@api:backend".into()]).unwrap();
    let plan = build_plan(
        &workspace,
        &one,
        None,
        &[OsString::from("--seed"), OsString::from("1")],
        TestMode::Test,
    )
    .unwrap();
    assert_eq!(plan.steps()[0].argv, ["test", "--seed", "1"]);

    let many = resolve_selectors(&workspace, &["@api".into()]).unwrap();
    let error = build_plan(
        &workspace,
        &many,
        None,
        &[OsString::from("--watch")],
        TestMode::Test,
    )
    .unwrap_err();
    assert!(error.to_string().contains("exactly one executable step"));
}

#[test]
fn cli_reserves_application_suites_and_treats_dot_unit_as_a_path() {
    let repository = repository();
    let tools = tools();
    let log = repository.path().join("commands.log");
    let reserved = run(
        repository.path(),
        tools.path(),
        &log,
        &["test", "application"],
        None,
    );
    assert_eq!(reserved.status.code(), Some(2));
    assert!(!log.exists());

    fs::create_dir_all(repository.path().join("unit")).unwrap();
    let path = run(
        repository.path(),
        tools.path(),
        &log,
        &["test", "./unit"],
        None,
    );
    assert_eq!(path.status.code(), Some(0));
    assert!(!log.exists());

    let later_selector = run(
        repository.path(),
        tools.path(),
        &log,
        &["test", "@api:backend", "unit"],
        None,
    );
    assert_eq!(later_selector.status.code(), Some(0));
    assert!(fs::read_to_string(log).unwrap().contains("mix|"));
}

#[test]
fn cli_passthrough_is_forwarded_only_for_one_test_step() {
    let repository = repository();
    let tools = tools();
    let log = repository.path().join("commands.log");
    let one = run(
        repository.path(),
        tools.path(),
        &log,
        &["test", "@api:backend", "--", "--seed", "4"],
        None,
    );
    assert_eq!(one.status.code(), Some(0));
    assert!(fs::read_to_string(&log).unwrap().contains("test --seed 4"));

    fs::remove_file(&log).unwrap();
    let many = run(
        repository.path(),
        tools.path(),
        &log,
        &["test", "@api", "--", "--watch"],
        None,
    );
    assert_eq!(many.status.code(), Some(2));
    assert!(!log.exists());

    let coverage = run(
        repository.path(),
        tools.path(),
        &log,
        &["coverage", "@api:backend", "--", "--watch"],
        None,
    );
    assert_eq!(coverage.status.code(), Some(2));
    assert!(!log.exists());
}

#[test]
fn execution_aggregates_failures_and_json_stdout_stays_pure() {
    let repository = repository();
    let tools = tools();
    let log = repository.path().join("commands.log");
    let output = run(
        repository.path(),
        tools.path(),
        &log,
        &["test", "@api", "--output", "json"],
        Some("fail-mix"),
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "failed");
    assert_eq!(report["steps"].as_array().unwrap().len(), 3);
    assert_eq!(fs::read_to_string(log).unwrap().lines().count(), 3);
}

#[test]
fn excoveralls_config_is_merged_outside_repo_and_cleaned() {
    let repository = repository();
    let tools = tools();
    let log = repository.path().join("commands.log");
    let captured = repository.path().join("captured.json");
    let captured_path = repository.path().join("captured-path");
    let captured_mode = repository.path().join("captured-mode");
    let output = workflow()
        .args(["coverage", "@api:backend"])
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools.path())
        .env("JUST_TEST_LOG", &log)
        .env("JUST_CAPTURE_CONFIG", &captured)
        .env("JUST_CAPTURE_PATH", &captured_path)
        .env("JUST_CAPTURE_MODE", &captured_mode)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let merged: serde_json::Value = serde_json::from_slice(&fs::read(&captured).unwrap()).unwrap();
    assert_eq!(merged["skip_files"], serde_json::json!(["test/support"]));
    assert_eq!(merged["coverage_options"]["floor_coverage"], false);
    assert_eq!(merged["coverage_options"]["minimum_coverage"], 80);
    let temporary = PathBuf::from(fs::read_to_string(captured_path).unwrap());
    assert!(!temporary.starts_with(repository.path()));
    assert!(!temporary.exists());
    assert_eq!(fs::read_to_string(captured_mode).unwrap(), "600\n");
    assert_eq!(
        fs::read_to_string(repository.path().join("apps/api/coveralls.json")).unwrap(),
        r#"{"skip_files":["test/support"],"coverage_options":{"minimum_coverage":12,"floor_coverage":false}}"#
    );
}

#[test]
fn excoveralls_temporary_config_is_cleaned_after_provider_failure() {
    let repository = repository();
    let tools = tools();
    let log = repository.path().join("commands.log");
    let captured = repository.path().join("captured.json");
    let captured_path = repository.path().join("captured-path");
    let captured_mode = repository.path().join("captured-mode");
    let output = workflow()
        .args(["coverage", "@api:backend"])
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools.path())
        .env("JUST_TEST_LOG", &log)
        .env("JUST_CAPTURE_CONFIG", &captured)
        .env("JUST_CAPTURE_PATH", &captured_path)
        .env("JUST_CAPTURE_MODE", &captured_mode)
        .env("JUST_STUB_BEHAVIOR", "fail-mix")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let temporary = PathBuf::from(fs::read_to_string(captured_path).unwrap());
    assert!(!temporary.exists());
}

#[test]
fn invalid_coveralls_json_fails_whole_plan_preflight_before_launch() {
    let repository = repository();
    write(
        repository.path().join("apps/api/coveralls.json"),
        "{invalid",
    );
    let tools = tools();
    let log = repository.path().join("commands.log");
    let output = run(
        repository.path(),
        tools.path(),
        &log,
        &["coverage", "@api"],
        None,
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(!log.exists());
}

#[test]
fn coverage_requires_nextest_before_any_provider_launches() {
    let repository = repository();
    let tools = tools();
    let log = repository.path().join("commands.log");
    fs::remove_file(tools.path().join("cargo-nextest")).unwrap();

    let output = run(
        repository.path(),
        tools.path(),
        &log,
        &["coverage", "@api"],
        None,
    );

    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("target-test tool 'cargo-nextest' is not available"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!log.exists());
}

#[test]
fn missing_tools_and_adapters_fail_before_any_provider_launches() {
    let repository = repository();
    let tools = tools();
    let log = repository.path().join("commands.log");
    fs::remove_file(tools.path().join("cargo-llvm-cov")).unwrap();
    let missing_tool = run(
        repository.path(),
        tools.path(),
        &log,
        &["coverage", "@api"],
        None,
    );
    assert_eq!(missing_tool.status.code(), Some(2));
    assert!(!log.exists());

    write(
        repository.path().join("apps/api/assets/package.json"),
        r#"{"scripts":{"test":"vitest"},"devDependencies":{"vitest":"4"}}"#,
    );
    let missing_adapter = run(
        repository.path(),
        tools.path(),
        &log,
        &["coverage", "@api"],
        None,
    );
    assert_eq!(missing_adapter.status.code(), Some(2));
    assert!(!log.exists());
}

#[test]
fn child_signal_two_interrupts_but_voluntary_exit_130_is_an_ordinary_failure() {
    let repository = repository();
    let tools = tools();
    let log = repository.path().join("commands.log");
    let interrupted = run(
        repository.path(),
        tools.path(),
        &log,
        &["test", "@api"],
        Some("signal2"),
    );
    assert_eq!(interrupted.status.code(), Some(130));
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 1);

    fs::remove_file(&log).unwrap();
    let ordinary = run(
        repository.path(),
        tools.path(),
        &log,
        &["test", "@api:backend"],
        Some("exit130"),
    );
    assert_eq!(ordinary.status.code(), Some(1));
}

#[test]
fn child_signal_two_stops_residual_process_group_members() {
    let repository = repository();
    let tools = tools();
    let log = repository.path().join("commands.log");
    let descendant_path = repository.path().join("descendant.pid");
    let mut child = workflow()
        .args(["test", "@api"])
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools.path())
        .env("JUST_TEST_LOG", &log)
        .env("JUST_STUB_BEHAVIOR", "signal2-descendant")
        .env("JUST_DESCENDANT_PID", &descendant_path)
        .spawn()
        .unwrap();

    let mut early_status = None;
    for _ in 0..200 {
        if descendant_path.is_file() {
            break;
        }
        if let Some(status) = child.try_wait().unwrap() {
            early_status = Some(status);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if !descendant_path.is_file() {
        let _ = unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
        let _ = child.wait();
        panic!("provider did not record its descendant before workflow exit {early_status:?}");
    }
    let descendant = fs::read_to_string(&descendant_path)
        .unwrap()
        .parse::<i32>()
        .unwrap();

    let mut status = None;
    for _ in 0..200 {
        if let Some(completed) = child.try_wait().unwrap() {
            status = Some(completed);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if status.is_none() {
        let _ = unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
        let _ = child.wait();
    }
    let mut descendant_gone = false;
    for _ in 0..100 {
        if unsafe { libc::kill(descendant, 0) } != 0 {
            descendant_gone = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if !descendant_gone {
        let _ = unsafe { libc::kill(descendant, libc::SIGKILL) };
    }

    assert_eq!(status.and_then(|status| status.code()), Some(130));
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 1);
    assert!(
        descendant_gone,
        "target-test descendant {descendant} survived child SIGINT"
    );
}

#[test]
fn real_sigint_stops_the_process_group_and_returns_130() {
    let repository = repository();
    let tools = tools();
    let log = repository.path().join("commands.log");
    let descendant_path = repository.path().join("descendant.pid");
    let mut child = workflow()
        .args(["test", "@api:backend"])
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools.path())
        .env("JUST_TEST_LOG", &log)
        .env("JUST_STUB_BEHAVIOR", "hang")
        .env("JUST_DESCENDANT_PID", &descendant_path)
        .spawn()
        .unwrap();
    for _ in 0..200 {
        if descendant_path.is_file() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(descendant_path.is_file());
    let descendant = fs::read_to_string(&descendant_path)
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(130));
    for _ in 0..100 {
        if unsafe { libc::kill(descendant, 0) } != 0 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("target-test descendant {descendant} survived interruption");
}
