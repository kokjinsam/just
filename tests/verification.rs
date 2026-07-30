#![cfg(test)]
//! Integration tests for repository-wide verification.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

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

fn write(path: impl AsRef<Path>, source: &str) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
}

fn executable(path: &Path, source: &str) {
    write(path, source);
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn cargo_repository() -> tempfile::TempDir {
    let repository = tempfile::tempdir().unwrap();
    for directory in ["app/src", "specs", "system-tests"] {
        fs::create_dir_all(repository.path().join(directory)).unwrap();
    }
    write(
        repository.path().join("just.yaml"),
        concat!(
            "version: 1\n",
            "system:\n  specs: specs\n  tests: system-tests\n",
            "coverage:\n  lines: 80\n",
            "targets:\n  app:\n    path: app\n",
        ),
    );
    write(
        repository.path().join("app/Cargo.toml"),
        "[package]\nname='app'\nversion='0.1.0'\n",
    );
    write(
        repository.path().join("app/src/lib.rs"),
        "pub fn value()->u8{1}\n",
    );
    repository
}

fn cargo_tools() -> tempfile::TempDir {
    let tools = tempfile::tempdir().unwrap();
    let script = r#"#!/bin/sh
printf '%s|%s\n' "${0##*/}" "$*" >> "$VERIFY_LOG"
case "$*" in
  *clippy*) exit "${FAIL_CLIPPY:-0}" ;;
  *llvm-cov*)
    if [ -n "$HANG_COVERAGE" ]; then
      /bin/sleep 30 &
      printf '%s' "$!" > "$VERIFY_DESCENDANT_PID"
      wait
    fi
    exit "${FAIL_COVERAGE:-0}"
    ;;
esac
exit 0
"#;
    for name in ["cargo", "cargo-clippy", "cargo-nextest", "cargo-llvm-cov"] {
        executable(&tools.path().join(name), script);
    }
    tools
}

fn run_with_tools(repository: &Path, tools: &Path, envs: &[(&str, &str)], args: &[&str]) -> Output {
    let mut command = workflow();
    command
        .args(args)
        .current_dir(repository)
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools)
        .env("VERIFY_LOG", repository.join("verify.log"));
    for (name, value) in envs {
        command.env(name, value);
    }
    command.output().unwrap()
}

fn configure_e2e(repository: &Path, tools: &Path) {
    write(
        repository.join(".gitignore"),
        "system-tests/artifacts/\nsystem-tests/.venv/\n",
    );
    write(
        repository.join("process-compose.yaml"),
        "version: '0.5'\nprocesses: {}\n",
    );
    write(
        repository.join("system-tests/package.json"),
        r#"{"devDependencies":{"@playwright/test":"1.50.0"}}"#,
    );
    write(
        repository.join("system-tests/playwright.config.ts"),
        "export default {};\n",
    );
    write(
        repository.join("system-tests/e2e/app.spec.ts"),
        "test('app', () => {});\n",
    );
    write(
        repository.join("system-tests/visual/app.spec.ts"),
        "test('app visual', () => {});\n",
    );
    executable(
        &tools.join("pnpm"),
        "#!/bin/sh\nprintf 'pnpm|%s\\n' \"$*\" >> \"$VERIFY_LOG\"\nif [ -n \"$FAIL_APPLICATION\" ]; then exit \"$FAIL_APPLICATION\"; fi\nexit 0\n",
    );
    executable(
        &tools.join("process-compose"),
        r#"#!/bin/sh
case " $* " in
  *" --dry-run "*)
    printf 'pc:dry\n' >> "$VERIFY_LOG"
    exit 0
    ;;
  *" project is-ready "*)
    printf 'pc:ready\n' >> "$VERIFY_LOG"
    kill -0 "$(/bin/cat "$VERIFY_PC_PID")" 2>/dev/null
    exit $?
    ;;
  *" down "*)
    printf 'pc:down\n' >> "$VERIFY_LOG"
    if [ -f "$VERIFY_PC_PID" ]; then
      kill -TERM "$(/bin/cat "$VERIFY_PC_PID")" 2>/dev/null || true
    fi
    exit 0
    ;;
  *" up "*)
    printf 'pc:up\n' >> "$VERIFY_LOG"
    printf '%s' "$$" > "$VERIFY_PC_PID"
    trap 'exit 0' TERM INT
    while :; do /bin/sleep 1; done
    ;;
esac
exit 3
"#,
    );
}

#[test]
fn verify_is_a_repository_wide_command_with_one_combined_report() {
    let root = fixture("valid-minimal");
    let output = run(&["verify", "--output", "json"], &root);

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["command"], "verify");
    assert_eq!(report["selectors"], serde_json::json!([]));
    assert_eq!(report["status"], "passed");
    assert_eq!(report["steps"].as_array().unwrap().len(), 2);
    assert_eq!(report["steps"][0]["id"], "typecheck:skip:0000");
    assert_eq!(report["steps"][1]["id"], "compile:skip:0000");
}

#[test]
fn missing_final_stage_tool_prevents_every_provider_launch() {
    let repository = cargo_repository();
    let tools = cargo_tools();
    fs::remove_file(tools.path().join("cargo-llvm-cov")).unwrap();

    let output = run_with_tools(repository.path(), tools.path(), &[], &["verify"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(!repository.path().join("verify.log").exists());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("target-test tool 'cargo-llvm-cov' is not available")
    );
}

#[test]
fn independent_failures_are_combined_and_coverage_runs_exactly_once() {
    let repository = cargo_repository();
    let tools = cargo_tools();

    let output = run_with_tools(
        repository.path(),
        tools.path(),
        &[("FAIL_CLIPPY", "7"), ("FAIL_COVERAGE", "8")],
        &["verify", "--output", "json"],
    );

    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let steps = report["steps"].as_array().unwrap();
    assert_eq!(
        steps
            .iter()
            .map(|step| step["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "format_check:0000:rustfmt",
            "lint:0000:clippy",
            "typecheck:skip:0000",
            "compile:0000:cargo-check",
            "coverage:0000:llvm-cov",
        ]
    );
    let failed = steps
        .iter()
        .filter(|step| step["status"] == "failed")
        .map(|step| step["provider"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(failed, ["clippy", "llvm-cov"]);
    let log = fs::read_to_string(repository.path().join("verify.log")).unwrap();
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("llvm-cov nextest"))
            .count(),
        1
    );
    assert!(!log.lines().any(|line| line.ends_with("|nextest run")));
}

#[test]
fn verify_without_application_suites_needs_no_process_compose_and_preserves_sources() {
    let repository = cargo_repository();
    let tools = cargo_tools();
    let source = repository.path().join("app/src/lib.rs");
    fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).unwrap();
    let before = fs::read(&source).unwrap();

    for envs in [Vec::<(&str, &str)>::new(), vec![("FAIL_COVERAGE", "9")]] {
        let output = run_with_tools(repository.path(), tools.path(), &envs, &["verify"]);
        assert!(
            matches!(output.status.code(), Some(0 | 1)),
            "{:?}",
            output.status
        );
        assert_eq!(fs::read(&source).unwrap(), before);
        assert_eq!(
            fs::metadata(&source).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
}

#[test]
fn configured_application_suites_use_one_process_compose_lifecycle_after_coverage() {
    let repository = cargo_repository();
    let tools = cargo_tools();
    configure_e2e(repository.path(), tools.path());
    let pid = repository.path().join("process-compose.pid");

    let output = run_with_tools(
        repository.path(),
        tools.path(),
        &[("VERIFY_PC_PID", pid.to_str().unwrap())],
        &["verify", "--output", "json"],
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let providers = report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["provider"].as_str().unwrap())
        .collect::<Vec<_>>();
    let coverage = providers
        .iter()
        .position(|provider| *provider == "llvm-cov");
    let application = providers
        .iter()
        .position(|provider| *provider == "playwright");
    assert!(coverage < application, "{providers:?}");
    let log = fs::read_to_string(repository.path().join("verify.log")).unwrap();
    assert_eq!(log.lines().filter(|line| *line == "pc:up").count(), 1);
    assert_eq!(log.lines().filter(|line| *line == "pc:dry").count(), 1);
    assert_eq!(log.lines().filter(|line| *line == "pc:down").count(), 1);
}

#[test]
fn voluntary_child_130_is_failure_but_real_sigint_returns_130_after_cleanup() {
    let repository = cargo_repository();
    let tools = cargo_tools();
    let voluntary = run_with_tools(
        repository.path(),
        tools.path(),
        &[("FAIL_COVERAGE", "130")],
        &["verify", "--output", "json"],
    );
    assert_eq!(voluntary.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&voluntary.stdout).unwrap();
    assert_eq!(report["status"], "failed");
    assert_eq!(
        report["steps"].as_array().unwrap().last().unwrap()["exit_code"],
        130
    );

    let descendant = repository.path().join("descendant.pid");
    let mut child = workflow();
    child
        .args(["verify", "--output", "json"])
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools.path())
        .env("VERIFY_LOG", repository.path().join("verify.log"))
        .env("HANG_COVERAGE", "1")
        .env("VERIFY_DESCENDANT_PID", &descendant)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = child.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !descendant.is_file() {
        assert!(Instant::now() < deadline, "coverage provider did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
    rustix::process::kill_process(
        rustix::process::Pid::from_child(&child),
        rustix::process::Signal::INT,
    )
    .unwrap();
    let interrupted = child.wait_with_output().unwrap();
    assert_eq!(interrupted.status.code(), Some(130));
    let report: serde_json::Value = serde_json::from_slice(&interrupted.stdout).unwrap();
    assert_eq!(report["status"], "interrupted");
    let descendant_pid = fs::read_to_string(descendant)
        .unwrap()
        .parse::<i32>()
        .unwrap();
    let descendant_pid = rustix::process::Pid::from_raw(descendant_pid).unwrap();
    assert_eq!(
        rustix::process::test_kill_process(descendant_pid),
        Err(rustix::io::Errno::SRCH)
    );
}

#[test]
fn post_start_application_failure_still_collects_and_cleans_up() {
    let repository = cargo_repository();
    let tools = cargo_tools();
    configure_e2e(repository.path(), tools.path());
    let pid = repository.path().join("process-compose.pid");

    let output = run_with_tools(
        repository.path(),
        tools.path(),
        &[
            ("VERIFY_PC_PID", pid.to_str().unwrap()),
            ("FAIL_APPLICATION", "7"),
        ],
        &["verify", "--output", "json"],
    );

    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let steps = report["steps"].as_array().unwrap();
    assert!(
        steps
            .iter()
            .any(|step| step["provider"] == "playwright" && step["status"] == "failed")
    );
    assert!(
        steps.iter().any(|step| {
            step["provider"] == "artifact-collection" && step["status"] == "passed"
        })
    );
    assert!(steps.iter().any(|step| {
        step["provider"] == "process-compose-cleanup" && step["status"] == "passed"
    }));
    let log = fs::read_to_string(repository.path().join("verify.log")).unwrap();
    assert_eq!(log.lines().filter(|line| *line == "pc:down").count(), 1);
}

#[test]
fn verify_accepts_no_selectors_or_passthrough_arguments() {
    let root = fixture("valid-minimal");

    for arguments in [vec!["verify", "@app"], vec!["verify", "--", "--nocapture"]] {
        let output = run(&arguments, &root);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
    }
}
