#![cfg(test)]
#![cfg(unix)]
//! Integration tests for application-test workflows.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use kokjinsam_just::application_testing::{ApplicationSuite, discover};
use kokjinsam_just::workspace::Workspace;

fn write(path: impl AsRef<Path>, source: &str) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
}

fn repository() -> tempfile::TempDir {
    let repository = tempfile::tempdir().unwrap();
    for directory in ["app", "specs/openapi", "system-tests"] {
        fs::create_dir_all(repository.path().join(directory)).unwrap();
    }
    write(
        repository.path().join("just.yaml"),
        concat!(
            "version: 1\n",
            "system:\n  specs: specs\n  tests: system-tests\n",
            "targets:\n  app:\n    path: app\n",
        ),
    );
    write(
        repository.path().join(".gitignore"),
        "system-tests/artifacts/\nsystem-tests/.venv/\n",
    );
    write(
        repository.path().join("process-compose.yaml"),
        "version: '0.5'\nprocesses: {}\n",
    );
    repository
}

fn complete_suites(repository: &Path) {
    write(
        repository.join("specs/openapi/a.yaml"),
        "openapi: 3.1.0\ninfo: {title: A, version: 1}\npaths: {}\n",
    );
    write(
        repository.join("specs/openapi/b.json"),
        r#"{"openapi":"3.1.0","info":{"title":"B","version":"1"},"paths":{}}"#,
    );
    write(
        repository.join("specs/openapi/vacuum.conf.yaml"),
        "rules: {}\n",
    );
    write(
        repository.join("system-tests/pyproject.toml"),
        "[project]\ndependencies = [\"schemathesis==4.1.0\"]\n",
    );
    write(repository.join("system-tests/uv.lock"), "version = 1\n");
    write(
        repository.join("system-tests/package.json"),
        r#"{"devDependencies":{"@playwright/test":"1.50.0","@antithesishq/bombadil":"0.9.1"}}"#,
    );
    write(
        repository.join("system-tests/playwright.config.ts"),
        "export default {};\n",
    );
    write(
        repository.join("system-tests/e2e/a.spec.ts"),
        "test('a', () => {});\n",
    );
    write(
        repository.join("system-tests/visual/a.test.ts"),
        "test('a', () => {});\n",
    );
    write(
        repository.join("system-tests/bombadil/specification.ts"),
        "export {};\n",
    );
}

fn workflow() -> Command {
    Command::new(env!("CARGO_BIN_EXE_workflow"))
}

fn run(repository: &Path, tools: &Path, args: &[&str]) -> Output {
    workflow()
        .args(args)
        .current_dir(repository)
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools)
        .output()
        .unwrap()
}

fn tools() -> tempfile::TempDir {
    let tools = tempfile::tempdir().unwrap();
    for name in ["mix", "pnpm", "cargo", "uv", "process-compose"] {
        let path = tools.path().join(name);
        write(&path, "#!/bin/sh\nexit 0\n");
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    tools
}

fn lifecycle_tools() -> tempfile::TempDir {
    let tools = tempfile::tempdir().unwrap();
    let provider = r#"#!/bin/sh
name=${0##*/}
printf 'provider:%s|%s|run=%s|artifact=%s|api=%s|web=%s|db=%s\n' \
  "$name" "$*" "$SYSTEM_TEST_RUN_ID" "$SYSTEM_TEST_ARTIFACT_DIR" \
  "$SYSTEM_API_URL" "$SYSTEM_WEB_URL" "$DATABASE_URL" >> "$JUST_APP_LOG"
if [ "$JUST_FAIL_PROVIDER" = "$name" ]; then exit 7; fi
if [ "$JUST_EXIT130_PROVIDER" = "$name" ]; then exit 130; fi
if [ "$JUST_BREAK_ARTIFACT_LOGS" = "$name" ]; then
  /bin/chmod 500 "$SYSTEM_TEST_ARTIFACT_DIR"
fi
if [ "$JUST_HANG_PROVIDER" = "$name" ]; then
  /bin/sleep 30 &
  printf '%s' "$!" > "$JUST_PROVIDER_DESCENDANT_PID"
  wait
fi
exit 0
"#;
    for name in ["mix", "pnpm", "cargo", "cargo-nextest", "uv"] {
        let path = tools.path().join(name);
        write(&path, provider);
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let process_compose = r#"#!/bin/sh
case " $* " in
  *" --dry-run "*)
    printf 'pc:dry|%s\n' "$*" >> "$JUST_APP_LOG"
    previous=
    for argument in "$@"; do
      if [ "$previous" = "-e" ]; then
        /bin/cp "$argument" "$JUST_CAPTURE_ENV"
        /usr/bin/stat -f '%Lp' "$argument" > "$JUST_CAPTURE_MODE" 2>/dev/null ||
          /usr/bin/stat -c '%a' "$argument" > "$JUST_CAPTURE_MODE"
      fi
      previous=$argument
    done
    [ "$JUST_PC_BEHAVIOR" = "dry-fail" ] && exit 6
    exit 0
    ;;
  *" project is-ready "*)
    printf 'pc:ready|%s\n' "$*" >> "$JUST_APP_LOG"
    if [ "$JUST_PC_BEHAVIOR" = "hang-ready" ]; then
      /bin/sleep 30 &
      printf '%s' "$!" > "$JUST_CONTROL_DESCENDANT_PID"
      wait
    fi
    if [ "$JUST_PC_BEHAVIOR" = "remove-controller" ]; then
      /bin/rm "$0"
      exit 1
    fi
    [ -f "$JUST_PC_PID" ] || exit 1
    kill -0 "$(/bin/cat "$JUST_PC_PID")" 2>/dev/null || exit 1
    [ "$JUST_PC_BEHAVIOR" = "not-ready" ] && exit 1
    if [ "$JUST_PC_BEHAVIOR" = "die-after-ready" ]; then
      kill -KILL "$(/bin/cat "$JUST_PC_PID")" 2>/dev/null || true
    fi
    if [ "$JUST_PC_BEHAVIOR" = "remove-provider" ]; then
      /bin/rm "$JUST_PROVIDER_PATH"
    fi
    exit 0
    ;;
  *" down "*)
    printf 'pc:down|%s\n' "$*" >> "$JUST_APP_LOG"
    if [ "$JUST_PC_BEHAVIOR" = "hang-down" ]; then
      /bin/sleep 30 &
      printf '%s' "$!" > "$JUST_CONTROL_DESCENDANT_PID"
      wait
    fi
    [ "$JUST_PC_BEHAVIOR" = "force-cleanup" ] && exit 8
    if [ -f "$JUST_PC_PID" ]; then kill -TERM "$(/bin/cat "$JUST_PC_PID")" 2>/dev/null || true; fi
    exit 0
    ;;
  *" up "*)
    printf 'pc:up|%s\n' "$*" >> "$JUST_APP_LOG"
    printf '%s' "$$" > "$JUST_PC_PID"
    [ "$JUST_PC_BEHAVIOR" = "die-up" ] && exit 9
    if [ "$JUST_PC_BEHAVIOR" = "force-cleanup" ]; then
      trap '' TERM
      /bin/sleep 30 &
      printf '%s' "$!" > "$JUST_DESCENDANT_PID"
      wait
    fi
    trap 'exit 0' TERM INT
    while :; do /bin/sleep 1; done
    ;;
esac
exit 3
"#;
    let path = tools.path().join("process-compose");
    write(&path, process_compose);
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    tools
}

fn run_lifecycle(repository: &Path, tools: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let log = repository.join("application.log");
    let pid = repository.join("process-compose.pid");
    let captured_env = repository.join("captured.env");
    let captured_mode = repository.join("captured.mode");
    let mut command = workflow();
    command
        .args(args)
        .current_dir(repository)
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools)
        .env("JUST_APP_LOG", log)
        .env("JUST_PC_PID", pid)
        .env("JUST_CAPTURE_ENV", captured_env)
        .env("JUST_CAPTURE_MODE", captured_mode);
    for (name, value) in envs {
        command.env(name, value);
    }
    command.output().unwrap()
}

fn target_manifests(repository: &Path) {
    write(
        repository.join("app/mix.exs"),
        "defmodule Demo.MixProject do\n use Mix.Project\n def project, do: []\nend\n",
    );
    write(
        repository.join("app/package.json"),
        r#"{"scripts":{"build":"vite build","test":"vitest"}}"#,
    );
    write(
        repository.join("app/Cargo.toml"),
        "[package]\nname='app'\nversion='0.1.0'\n",
    );
}

#[test]
fn suite_discovery_is_git_aware_complete_and_deterministic() {
    let repository = repository();
    complete_suites(repository.path());
    let workspace = Workspace::discover(repository.path()).unwrap();
    let discovered = discover(&workspace).unwrap();

    assert_eq!(
        discovered.configured_suites(),
        [
            ApplicationSuite::Contract,
            ApplicationSuite::E2e,
            ApplicationSuite::Visual,
            ApplicationSuite::UiProperty,
        ]
    );
    assert_eq!(
        discovered
            .openapi_documents()
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy())
            .collect::<Vec<_>>(),
        ["a.yaml", "b.json"]
    );

    write(
        repository.path().join(".gitignore"),
        concat!(
            "system-tests/artifacts/\n",
            "system-tests/.venv/\n",
            "system-tests/visual/\n",
        ),
    );
    let error = discover(&workspace).unwrap_err();
    assert!(error.to_string().contains("visual"));
    assert!(error.to_string().contains("partial"));
}

#[test]
fn partial_suite_or_shared_environment_evidence_is_configuration_error() {
    let repository = repository();
    write(
        repository.path().join("system-tests/package.json"),
        r#"{"devDependencies":{"@playwright/test":"1.50.0"}}"#,
    );
    let workspace = Workspace::discover(repository.path()).unwrap();
    let error = discover(&workspace).unwrap_err();
    assert!(error.to_string().contains("e2e"));

    fs::remove_file(repository.path().join("system-tests/package.json")).unwrap();
    write(repository.path().join(".gitignore"), "");
    let error = discover(&workspace).unwrap_err();
    assert!(error.to_string().contains("ignore"));
}

#[test]
fn application_cli_rejects_selectors_and_runtime_owned_passthrough() {
    let repository = repository();
    complete_suites(repository.path());
    let tools = tools();

    let selected = run(repository.path(), tools.path(), &["test", "visual", "@app"]);
    assert_eq!(selected.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&selected.stderr)
            .contains("do not accept target or path selectors")
    );

    let override_owned = run(
        repository.path(),
        tools.path(),
        &["test", "e2e", "--", "--project=other"],
    );
    assert_eq!(override_owned.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&override_owned.stderr).contains("runtime-owned"));
}

#[test]
fn provider_specific_aliases_and_artifact_overrides_are_rejected_before_launch() {
    let cases: &[(&str, &[&str])] = &[
        ("visual", &["-u", "all"]),
        ("e2e", &["-c", "alternate.config.ts"]),
        ("e2e", &["--reporter=dot"]),
        ("visual", &["--output-dir", "elsewhere"]),
        ("visual", &["--ignore-snapshots"]),
        ("e2e", &["--last-failed-file", "elsewhere.json"]),
        ("visual", &["--last-failed-file=elsewhere.json"]),
        ("contract", &["-u", "http://elsewhere.test"]),
        ("contract", &["-w=8"]),
        ("contract", &["-n", "500"]),
        ("contract", &["--config-file=alternate.toml"]),
        ("contract", &["--report-custom-path=elsewhere.json"]),
        ("contract", &["--report-junit-path=elsewhere.xml"]),
        ("ui-property", &["--origin=http://elsewhere.test"]),
    ];
    for (suite, passthrough) in cases {
        let repository = repository();
        complete_suites(repository.path());
        if *suite == "contract" {
            fs::remove_file(repository.path().join("specs/openapi/b.json")).unwrap();
        }
        let tools = lifecycle_tools();
        let mut args = vec!["test", suite, "--"];
        args.extend_from_slice(passthrough);
        let output = run_lifecycle(repository.path(), tools.path(), &args, &[]);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{suite} {passthrough:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("runtime-owned"),
            "{suite} {passthrough:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !repository.path().join("application.log").exists(),
            "{suite} {passthrough:?} launched a child"
        );
    }
}

#[test]
fn schemathesis_admission_uses_only_supported_dependency_declarations() {
    for declaration in [
        "[project]\ndependencies = [\"SchemaThesis[cli]>=4\"]\n",
        "[project]\ndependencies = []\n[dependency-groups]\ndev = [\"SCHEMATHESIS==4.1\"]\n",
    ] {
        let repository = repository();
        complete_suites(repository.path());
        write(
            repository.path().join("system-tests/pyproject.toml"),
            declaration,
        );
        let workspace = Workspace::discover(repository.path()).unwrap();
        assert!(
            discover(&workspace)
                .unwrap()
                .is_configured(ApplicationSuite::Contract),
            "{declaration}"
        );
    }

    for declaration in [
        "[project]\nname = \"schemathesis\"\ndependencies = []\n",
        "[project]\ndependencies = []\n[project.optional-dependencies]\ntest = [\"schemathesis\"]\n",
        "[project]\ndependencies = []\n[tool.uv]\ndev-dependencies = [\"schemathesis\"]\n",
        "[project]\ndependencies = []\n[tool.unrelated]\npackage = \"schemathesis\"\n",
    ] {
        let repository = repository();
        complete_suites(repository.path());
        write(
            repository.path().join("system-tests/pyproject.toml"),
            declaration,
        );
        let workspace = Workspace::discover(repository.path()).unwrap();
        let error = discover(&workspace).unwrap_err();
        assert!(error.to_string().contains("contract"), "{declaration}");
    }
}

#[test]
fn bombadil_requires_a_semver_literal_for_planning_and_doctor() {
    for version in ["1.2.3", "1.2.3-alpha.1", "1.2.3+build.7"] {
        let repository = repository();
        complete_suites(repository.path());
        write(
            repository.path().join("system-tests/package.json"),
            &format!(
                r#"{{"devDependencies":{{"@playwright/test":"1.50.0","@antithesishq/bombadil":"{version}"}}}}"#
            ),
        );
        let tools = tools();
        let planned = run(
            repository.path(),
            tools.path(),
            &["test", "ui-property", "--output", "json"],
        );
        assert_ne!(planned.status.code(), Some(2), "{version}");
        let doctor = run(
            repository.path(),
            tools.path(),
            &["doctor", "--output", "json"],
        );
        assert_ne!(doctor.status.code(), Some(2), "{version}");
        let report: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
        assert!(
            report["steps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|step| { step["provider"] == "bombadil" && step["status"] == "passed" })
        );
    }

    for version in [
        "^1.2.3",
        "~1.2.3",
        ">=1.2.3",
        "1.2.3 || 2.0.0",
        "1.2.3 - 2.0.0",
        "latest",
        "next",
        "workspace:*",
        "file:../bombadil",
    ] {
        let repository = repository();
        complete_suites(repository.path());
        write(
            repository.path().join("system-tests/package.json"),
            &format!(
                r#"{{"devDependencies":{{"@playwright/test":"1.50.0","@antithesishq/bombadil":"{version}"}}}}"#
            ),
        );
        let tools = tools();
        let planned = run(
            repository.path(),
            tools.path(),
            &["test", "ui-property", "--output", "json"],
        );
        assert_eq!(planned.status.code(), Some(2), "{version}");
        let doctor = run(
            repository.path(),
            tools.path(),
            &["doctor", "--output", "json"],
        );
        assert_eq!(doctor.status.code(), Some(1), "{version}");
        let report: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
        assert!(
            report["steps"].as_array().unwrap().iter().any(|step| {
                step["provider"] == "application-tests" && step["status"] == "failed"
            })
        );
    }
}

#[test]
fn doctor_reports_application_structure_and_tools_without_launching_them() {
    let repository = repository();
    complete_suites(repository.path());
    let tools = tempfile::tempdir().unwrap();
    let marker = repository.path().join("doctor-launched");
    for name in ["pnpm", "uv", "process-compose", "oasdiff", "vacuum"] {
        let path = tools.path().join(name);
        write(
            &path,
            &format!(
                "#!/bin/sh\nprintf launched >> '{}'\nexit 0\n",
                marker.display()
            ),
        );
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let output = run(
        repository.path(),
        tools.path(),
        &["doctor", "--output", "json"],
    );
    assert_eq!(output.status.code(), Some(0));
    assert!(!marker.exists());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    for provider in [
        "process-compose",
        "schemathesis",
        "playwright-e2e",
        "playwright-visual",
        "bombadil",
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
    let process_compose = report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["provider"] == "process-compose")
        .unwrap();
    assert!(
        process_compose["stderr"]
            .as_str()
            .unwrap()
            .contains("structural")
    );
}

#[test]
fn aggregate_application_uses_build_first_one_environment_and_ordered_providers() {
    let repository = repository();
    complete_suites(repository.path());
    target_manifests(repository.path());
    let tools = lifecycle_tools();
    let output = run_lifecycle(
        repository.path(),
        tools.path(),
        &["test", "application", "--output", "json"],
        &[],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "passed");
    let log = fs::read_to_string(repository.path().join("application.log")).unwrap();
    assert_eq!(log.matches("pc:up|").count(), 1);
    for ordered in [
        "pc:dry|",
        "provider:mix|compile --warnings-as-errors",
        "provider:pnpm|run build",
        "provider:cargo|build",
        "pc:up|",
        "pc:ready|",
        "provider:uv|run --locked -- st run",
        "provider:pnpm|exec playwright test",
        "provider:pnpm|exec bombadil test",
        "pc:down|",
    ] {
        assert!(log.contains(ordered), "{ordered}\n{log}");
    }
    let first_doc = log.find("a.yaml").unwrap();
    let second_doc = log.find("b.json").unwrap();
    assert!(first_doc < second_doc);
    assert!(log.contains("--project visual"));
    assert!(log.contains("--update-snapshots=none"));
    assert!(!log.contains("update-snapshots=all"));
    assert!(log.contains("--time-limit 1m"));
    assert_eq!(
        fs::read_to_string(repository.path().join("captured.mode")).unwrap(),
        "600\n"
    );
    let env = fs::read_to_string(repository.path().join("captured.env")).unwrap();
    for name in [
        "SYSTEM_TEST_RUN_ID",
        "SYSTEM_TEST_PROJECT",
        "SYSTEM_TEST_ARTIFACT_DIR",
        "SYSTEM_API_URL",
        "SYSTEM_WEB_URL",
        "POSTGRES_PORT",
        "POSTGRES_DB",
        "DATABASE_URL",
    ] {
        assert!(env.contains(&format!("{name}=")), "{name}\n{env}");
    }
    let artifact = log
        .lines()
        .find(|line| line.starts_with("provider:uv|"))
        .unwrap()
        .split("|artifact=")
        .nth(1)
        .unwrap()
        .split('|')
        .next()
        .unwrap();
    assert!(Path::new(artifact).is_dir());
    assert!(
        Path::new(artifact)
            .join("process-compose.stdout.log")
            .is_file()
    );
}

#[test]
fn preparation_failures_aggregate_and_prevent_environment_start() {
    let repository = repository();
    complete_suites(repository.path());
    target_manifests(repository.path());
    let tools = lifecycle_tools();
    let output = run_lifecycle(
        repository.path(),
        tools.path(),
        &["test", "application", "--output", "json"],
        &[("JUST_FAIL_PROVIDER", "mix")],
    );
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let log = fs::read_to_string(repository.path().join("application.log")).unwrap();
    assert!(log.contains("provider:mix|compile --warnings-as-errors"));
    assert!(log.contains("provider:pnpm|run build"));
    assert!(log.contains("provider:cargo|build"));
    assert!(!log.contains("pc:up|"));
    assert!(!log.contains("exec playwright"));
    assert!(report["steps"].as_array().unwrap().iter().any(|step| {
        step["status"] == "skipped"
            && step["skip_reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("preparation failed"))
    }));
}

#[test]
fn readiness_timeout_skips_suites_then_cleans_up_and_retains_artifacts() {
    let repository = repository();
    complete_suites(repository.path());
    let tools = lifecycle_tools();
    let output = run_lifecycle(
        repository.path(),
        tools.path(),
        &["test", "visual", "--output", "json"],
        &[
            ("JUST_PC_BEHAVIOR", "not-ready"),
            ("JUST_TEST_APPLICATION_READINESS_TIMEOUT_MS", "100"),
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let log = fs::read_to_string(repository.path().join("application.log")).unwrap();
    assert!(log.contains("pc:up|"));
    assert!(log.contains("pc:ready|"));
    assert!(log.contains("pc:down|"));
    assert!(!log.contains("provider:pnpm|"));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["steps"].as_array().unwrap().iter().any(|step| {
        step["provider"] == "process-compose-readiness" && step["status"] == "failed"
    }));
    let artifact = fs::read_to_string(repository.path().join("captured.env"))
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("SYSTEM_TEST_ARTIFACT_DIR=\""))
        .unwrap()
        .trim_end_matches('"')
        .to_owned();
    assert!(Path::new(&artifact).is_dir());
}

#[test]
fn bare_test_runs_targets_once_before_application_preparation_and_suites() {
    let repository = repository();
    complete_suites(repository.path());
    target_manifests(repository.path());
    let tools = lifecycle_tools();
    let output = run_lifecycle(
        repository.path(),
        tools.path(),
        &["test", "--output", "json"],
        &[],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = fs::read_to_string(repository.path().join("application.log")).unwrap();
    assert_eq!(log.matches("provider:mix|test").count(), 1);
    assert_eq!(log.matches("provider:pnpm|run test").count(), 1);
    assert_eq!(log.matches("provider:cargo|nextest run").count(), 1);
    assert!(
        log.find("provider:mix|test").unwrap()
            < log
                .find("provider:mix|compile --warnings-as-errors")
                .unwrap()
    );
    assert_eq!(log.matches("pc:up|").count(), 1);
}

#[test]
fn complete_static_preflight_rejects_missing_provider_before_dry_run_or_targets() {
    let repository = repository();
    complete_suites(repository.path());
    target_manifests(repository.path());
    let tools = lifecycle_tools();
    fs::remove_file(tools.path().join("uv")).unwrap();
    let output = run_lifecycle(
        repository.path(),
        tools.path(),
        &["test", "--output", "json"],
        &[],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(!repository.path().join("application.log").exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("uv"));
}

#[test]
fn provider_failures_continue_and_voluntary_130_is_an_ordinary_failure() {
    let repository = repository();
    complete_suites(repository.path());
    let tools = lifecycle_tools();
    let failed = run_lifecycle(
        repository.path(),
        tools.path(),
        &["test", "application", "--output", "json"],
        &[("JUST_FAIL_PROVIDER", "uv")],
    );
    assert_eq!(failed.status.code(), Some(1));
    let failed_report: serde_json::Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert!(
        failed_report["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| {
                step["provider"] == "schemathesis"
                    && step["status"] == "failed"
                    && step["exit_code"] == 7
            })
    );
    let log = fs::read_to_string(repository.path().join("application.log")).unwrap();
    assert_eq!(log.matches("provider:uv|").count(), 2);
    assert!(log.contains("--project e2e"));
    assert!(log.contains("--project visual"));
    assert!(log.contains("exec bombadil test"));
    assert!(log.contains("pc:down|"));

    fs::remove_file(repository.path().join("application.log")).unwrap();
    let voluntary = run_lifecycle(
        repository.path(),
        tools.path(),
        &["test", "visual", "--output", "json"],
        &[("JUST_EXIT130_PROVIDER", "pnpm")],
    );
    assert_eq!(voluntary.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&voluntary.stdout).unwrap();
    assert_eq!(report["status"], "failed");
    assert!(
        fs::read_to_string(repository.path().join("application.log"))
            .unwrap()
            .contains("pc:down|")
    );
}

#[test]
fn dry_run_failure_or_environment_death_skips_all_application_providers() {
    let repository = repository();
    complete_suites(repository.path());
    let tools = lifecycle_tools();
    let dry = run_lifecycle(
        repository.path(),
        tools.path(),
        &["test", "visual", "--output", "json"],
        &[("JUST_PC_BEHAVIOR", "dry-fail")],
    );
    assert_eq!(dry.status.code(), Some(1));
    let log = fs::read_to_string(repository.path().join("application.log")).unwrap();
    assert!(log.contains("pc:dry|"));
    assert!(!log.contains("pc:up|"));
    assert!(!log.contains("provider:"), "{log}");

    fs::remove_file(repository.path().join("application.log")).unwrap();
    let death = run_lifecycle(
        repository.path(),
        tools.path(),
        &["test", "visual", "--output", "json"],
        &[("JUST_PC_BEHAVIOR", "die-up")],
    );
    assert_eq!(death.status.code(), Some(1));
    let log = fs::read_to_string(repository.path().join("application.log")).unwrap();
    assert!(log.contains("pc:up|"));
    assert!(!log.contains("provider:"), "{log}");
    let report: serde_json::Value = serde_json::from_slice(&death.stdout).unwrap();
    assert!(report["steps"].as_array().unwrap().iter().any(|step| {
        step["provider"] == "process-compose-readiness" && step["status"] == "failed"
    }));
}

#[test]
fn passthrough_requires_one_action_and_appends_only_safe_arguments() {
    let repository = repository();
    complete_suites(repository.path());
    let tools = lifecycle_tools();
    let ambiguous = run_lifecycle(
        repository.path(),
        tools.path(),
        &["test", "contract", "--", "--checks", "all"],
        &[],
    );
    assert_eq!(ambiguous.status.code(), Some(2));
    assert!(!repository.path().join("application.log").exists());

    let safe = run_lifecycle(
        repository.path(),
        tools.path(),
        &["test", "visual", "--", "--grep", "critical"],
        &[],
    );
    assert_eq!(safe.status.code(), Some(0));
    let log = fs::read_to_string(repository.path().join("application.log")).unwrap();
    assert!(log.contains("--update-snapshots=none --grep critical"));
}

#[test]
fn forced_cleanup_kills_the_owned_process_group_and_descendants() {
    let repository = repository();
    complete_suites(repository.path());
    let tools = lifecycle_tools();
    let descendant = repository.path().join("descendant.pid");
    let output = run_lifecycle(
        repository.path(),
        tools.path(),
        &["test", "visual", "--output", "json"],
        &[
            ("JUST_PC_BEHAVIOR", "force-cleanup"),
            ("JUST_DESCENDANT_PID", descendant.to_str().unwrap()),
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let pid = fs::read_to_string(descendant)
        .unwrap()
        .parse::<i32>()
        .unwrap();
    let alive = rustix::process::Pid::from_raw(pid)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok());
    assert!(!alive, "Process Compose descendant {pid} survived cleanup");
    assert!(repository.path().join("captured.env").is_file());
}

#[test]
fn real_sigint_returns_130_after_environment_and_descendant_cleanup() {
    use std::os::unix::process::CommandExt;

    let repository = repository();
    complete_suites(repository.path());
    let tools = lifecycle_tools();
    let log = repository.path().join("application.log");
    let pc_pid = repository.path().join("process-compose.pid");
    let provider_descendant = repository.path().join("provider-descendant.pid");
    let captured_env = repository.path().join("captured.env");
    let captured_mode = repository.path().join("captured.mode");
    let mut command = workflow();
    command
        .args(["test", "visual", "--output", "json"])
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools.path())
        .env("JUST_APP_LOG", &log)
        .env("JUST_PC_PID", &pc_pid)
        .env("JUST_CAPTURE_ENV", &captured_env)
        .env("JUST_CAPTURE_MODE", &captured_mode)
        .env("JUST_HANG_PROVIDER", "pnpm")
        .env("JUST_PROVIDER_DESCENDANT_PID", &provider_descendant)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !provider_descendant.is_file() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(provider_descendant.is_file());
    let result = rustix::process::kill_process(
        rustix::process::Pid::from_child(&child),
        rustix::process::Signal::INT,
    );
    assert_eq!(result, Ok(()));
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(130));
    assert!(fs::read_to_string(&log).unwrap().contains("pc:down|"));
    for pid_file in [&pc_pid, &provider_descendant] {
        let pid = fs::read_to_string(pid_file)
            .unwrap()
            .parse::<i32>()
            .unwrap();
        assert!(
            rustix::process::Pid::from_raw(pid)
                .is_none_or(|pid| rustix::process::test_kill_process(pid).is_err()),
            "descendant {pid} survived interruption"
        );
    }
}

fn assert_process_gone(path: &Path) {
    let pid = fs::read_to_string(path).unwrap().parse::<i32>().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline
        && rustix::process::Pid::from_raw(pid)
            .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        rustix::process::Pid::from_raw(pid)
            .is_none_or(|pid| rustix::process::test_kill_process(pid).is_err()),
        "process {pid} from '{}' survived",
        path.display()
    );
}

#[test]
fn hung_readiness_is_bounded_and_reaps_control_and_foreground_groups() {
    let repository = repository();
    complete_suites(repository.path());
    let tools = lifecycle_tools();
    let control_descendant = repository.path().join("control-descendant.pid");
    let started = Instant::now();
    let output = run_lifecycle(
        repository.path(),
        tools.path(),
        &["test", "visual", "--output", "json"],
        &[
            ("JUST_PC_BEHAVIOR", "hang-ready"),
            (
                "JUST_CONTROL_DESCENDANT_PID",
                control_descendant.to_str().unwrap(),
            ),
            ("JUST_TEST_APPLICATION_READINESS_TIMEOUT_MS", "100"),
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(started.elapsed() < Duration::from_secs(15));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["steps"].as_array().unwrap().iter().any(|step| {
        step["provider"] == "process-compose-readiness"
            && step["stderr"]
                .as_str()
                .is_some_and(|stderr| stderr.contains("deadline expired"))
    }));
    assert_process_gone(&control_descendant);
    assert_process_gone(&repository.path().join("process-compose.pid"));
}

#[test]
fn sigint_during_hung_readiness_finalizes_and_reaps_all_groups() {
    use std::os::unix::process::CommandExt;

    let repository = repository();
    complete_suites(repository.path());
    let tools = lifecycle_tools();
    let control_descendant = repository.path().join("control-descendant.pid");
    let mut command = workflow();
    command
        .args(["test", "visual", "--output", "json"])
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools.path())
        .env("JUST_APP_LOG", repository.path().join("application.log"))
        .env("JUST_PC_PID", repository.path().join("process-compose.pid"))
        .env("JUST_CAPTURE_ENV", repository.path().join("captured.env"))
        .env("JUST_CAPTURE_MODE", repository.path().join("captured.mode"))
        .env("JUST_PC_BEHAVIOR", "hang-ready")
        .env("JUST_CONTROL_DESCENDANT_PID", &control_descendant)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !control_descendant.is_file() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(control_descendant.is_file());
    assert_eq!(
        rustix::process::kill_process(
            rustix::process::Pid::from_child(&child),
            rustix::process::Signal::INT,
        ),
        Ok(())
    );
    let started = Instant::now();
    assert_eq!(child.wait().unwrap().code(), Some(130));
    assert!(started.elapsed() < Duration::from_secs(4));
    assert_process_gone(&control_descendant);
    assert_process_gone(&repository.path().join("process-compose.pid"));
}

#[test]
fn hung_down_is_bounded_and_falls_back_to_owned_group_termination() {
    let repository = repository();
    complete_suites(repository.path());
    let tools = lifecycle_tools();
    let control_descendant = repository.path().join("control-descendant.pid");
    let started = Instant::now();
    let output = run_lifecycle(
        repository.path(),
        tools.path(),
        &["test", "visual", "--output", "json"],
        &[
            ("JUST_PC_BEHAVIOR", "hang-down"),
            (
                "JUST_CONTROL_DESCENDANT_PID",
                control_descendant.to_str().unwrap(),
            ),
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(started.elapsed() < Duration::from_secs(15));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["steps"].as_array().unwrap().iter().any(|step| {
        step["provider"] == "process-compose-cleanup" && step["status"] == "failed"
    }));
    assert_process_gone(&control_descendant);
    assert_process_gone(&repository.path().join("process-compose.pid"));
}

#[test]
fn post_start_controller_failures_still_collect_report_and_cleanup() {
    for behavior in ["remove-controller", "remove-provider", "die-after-ready"] {
        let repository = repository();
        complete_suites(repository.path());
        let tools = lifecycle_tools();
        let provider_path = tools.path().join("pnpm");
        let output = run_lifecycle(
            repository.path(),
            tools.path(),
            &["test", "visual", "--output", "json"],
            &[
                ("JUST_PC_BEHAVIOR", behavior),
                ("JUST_PROVIDER_PATH", provider_path.to_str().unwrap()),
                ("JUST_TEST_APPLICATION_READINESS_TIMEOUT_MS", "300"),
            ],
        );
        assert_eq!(output.status.code(), Some(1), "{behavior}");
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let steps = report["steps"].as_array().unwrap();
        assert!(
            steps
                .iter()
                .any(|step| step["provider"] == "artifact-collection"),
            "{behavior}"
        );
        assert!(
            steps
                .iter()
                .any(|step| step["provider"] == "process-compose-cleanup"),
            "{behavior}"
        );
        assert_process_gone(&repository.path().join("process-compose.pid"));
    }

    let repository = repository();
    complete_suites(repository.path());
    let tools = lifecycle_tools();
    let output = run_lifecycle(
        repository.path(),
        tools.path(),
        &["test", "visual", "--output", "json"],
        &[("JUST_BREAK_ARTIFACT_LOGS", "pnpm")],
    );
    fs::set_permissions(
        repository.path().join("system-tests/artifacts"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["steps"].as_array().unwrap().iter().any(|step| {
        step["provider"] == "application-controller"
            && step["stderr"]
                .as_str()
                .is_some_and(|stderr| stderr.contains("provider logs"))
    }));
    assert!(
        report["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| { step["provider"] == "process-compose-cleanup" })
    );
    assert_process_gone(&repository.path().join("process-compose.pid"));
}
