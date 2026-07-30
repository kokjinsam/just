#![cfg(test)]
//! Integration tests for formatting workflows.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use kokjinsam_just::capability::{Capability, ProviderId};
use kokjinsam_just::formatting::{build_plan, execute_plan};
use kokjinsam_just::ownership::{Owner, SystemResource};
use kokjinsam_just::plan::{Plan, Step, StepId};
use kokjinsam_just::report::{ReportStatus, StepStatus};
use kokjinsam_just::selector::resolve_selectors;
use kokjinsam_just::workspace::Workspace;

fn workflow() -> Command {
    Command::new(env!("CARGO_BIN_EXE_workflow"))
}

fn repository() -> tempfile::TempDir {
    let repository = tempfile::tempdir().unwrap();
    for directory in ["app", "specs", "tests"] {
        fs::create_dir(repository.path().join(directory)).unwrap();
    }
    fs::write(
        repository.path().join("just.yaml"),
        "version: 1\nsystem:\n  specs: specs\n  tests: tests\ntargets:\n  app:\n    path: app\n",
    )
    .unwrap();
    repository
}

fn run(repository: &Path, args: &[&str]) -> Output {
    workflow()
        .args(args)
        .current_dir(repository)
        .env_remove("JUST_CALLER_DIR")
        .output()
        .unwrap()
}

#[cfg(unix)]
fn executable(path: &Path, source: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
fn format_repository() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let repository = tempfile::tempdir().unwrap();
    for directory in [
        "apps/api/assets/src",
        "apps/api/lib",
        "apps/api/native/src",
        "specs",
        "tests",
    ] {
        fs::create_dir_all(repository.path().join(directory)).unwrap();
    }
    fs::write(
        repository.path().join("just.yaml"),
        concat!(
            "version: 1\n",
            "system:\n  specs: specs\n  tests: tests\n",
            "targets:\n",
            "  api:\n",
            "    path: apps/api\n",
            "    components:\n",
            "      backend:\n        path: .\n",
            "      web:\n        path: assets\n",
            "      native:\n        path: native\n",
        ),
    )
    .unwrap();
    fs::write(repository.path().join("apps/api/mix.exs"), "Mix.Project").unwrap();
    fs::write(
        repository.path().join("apps/api/lib/app.ex"),
        "defmodule App do end",
    )
    .unwrap();
    fs::write(
        repository.path().join("apps/api/assets/package.json"),
        r#"{"devDependencies":{"oxfmt":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        repository.path().join("apps/api/assets/src/app.ts"),
        "const value=1",
    )
    .unwrap();
    fs::write(
        repository.path().join("apps/api/native/Cargo.toml"),
        "[package]\nname='native'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(
        repository.path().join("apps/api/native/src/lib.rs"),
        "pub fn value()->u8{1}",
    )
    .unwrap();
    fs::write(
        repository.path().join("specs/Model.tla"),
        "---- MODULE Model ----\n====\n",
    )
    .unwrap();
    fs::write(
        repository.path().join(".gitignore"),
        "apps/api/assets/src/ignored.ts\n",
    )
    .unwrap();
    fs::write(
        repository.path().join("apps/api/assets/src/ignored.ts"),
        "const ignored=1",
    )
    .unwrap();

    let tools = tempfile::tempdir().unwrap();
    let script = "#!/bin/sh\nprintf '%s\\t%s\\t%s\\n' \"${0##*/}\" \"$PWD\" \"$*\" >> \"$FORMAT_LOG\"\nprintf '%s-out\\n' \"${0##*/}\"\nprintf '%s-err\\n' \"${0##*/}\" >&2\nif [ -n \"${FORMAT_DELAY:-}\" ] && [ \"${0##*/}\" = cargo ]; then\n  /bin/sleep \"$FORMAT_DELAY\"\nfi\ncase \"${0##*/}\" in\n  mix) exit \"${MIX_STATUS:-0}\" ;;\n  pnpm) exit \"${PNPM_STATUS:-0}\" ;;\n  cargo) exit \"${CARGO_STATUS:-0}\" ;;\nesac\n";
    for name in ["mix", "pnpm", "cargo"] {
        executable(&tools.path().join(name), script);
    }
    executable(
        &tools.path().join("java"),
        concat!(
            "#!/bin/sh\n",
            "printf '%s\\t%s\\t%s\\n' java \"$PWD\" \"$*\" >> \"$FORMAT_LOG\"\n",
            "if [ -n \"${TEMP_MODE_LOG:-",
            "}\" ]; then\n",
            "  (/usr/bin/stat -c '%a' \"$5\" 2>/dev/null",
            " || /usr/bin/stat -f '%Lp' \"$5\") > \"$TEMP_MODE_LOG\"\n",
            "fi\n",
            "case \"${JAVA_MODE:-",
            "copy}\" in\n",
            "  copy) /bin/cp \"$4\" \"$5\" ;;\n",
            "  change) printf '%s\\n' formatted > \"$5\" ;;\n",
            "  fail) printf '%s\\n' partial > \"$5\";",
            " printf '%s\\n' java-failed >&2; exit 23 ;;\n",
            "esac\n",
        ),
    );
    let jar = repository.path().join("tla2tools.jar");
    fs::write(&jar, "stub").unwrap();
    (repository, tools, jar)
}

#[cfg(unix)]
fn run_format(repository: &Path, tools: &Path, jar: &Path, args: &[&str]) -> Output {
    workflow()
        .args(args)
        .current_dir(repository)
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools)
        .env("FORMAT_LOG", repository.join("tools.log"))
        .env("TLA2TOOLS_JAR", jar)
        .output()
        .unwrap()
}

#[cfg(unix)]
fn log_lines(repository: &Path) -> Vec<String> {
    fs::read_to_string(repository.join("tools.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn public_format_check_is_an_unsupported_usage_error() {
    let repository = repository();

    let output = run(repository.path(), &["format", "--check"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("unexpected argument '--check'")
    );
}

#[test]
fn valid_selection_without_a_formatter_is_a_successful_zero_step_no_op() {
    let repository = repository();

    let output = run(repository.path(), &["format", "--output", "json"]);

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["command"], "format");
    assert_eq!(report["status"], "passed");
    assert_eq!(report["steps"], serde_json::json!([]));
}

#[cfg(unix)]
#[test]
fn repository_target_component_directory_file_and_ignored_file_scopes_are_isolated() {
    type FormatScopeCase<'a> = (&'a [&'a str], &'a [&'a str], &'a [&'a str]);
    let cases: &[FormatScopeCase<'_>] = &[
        (&["format"], &["mix", "pnpm", "cargo", "java"], &[]),
        (&["format", "@api"], &["mix", "pnpm", "cargo"], &["java"]),
        (
            &["format", "@api:web"],
            &["pnpm"],
            &["mix", "cargo", "java"],
        ),
        (
            &["format", "apps/api/assets/src"],
            &["pnpm"],
            &["mix", "cargo", "java"],
        ),
        (
            &["format", "apps/api/assets/src/app.ts"],
            &["pnpm"],
            &["mix", "cargo", "java"],
        ),
        (
            &["format", "apps/api/assets/src/ignored.ts"],
            &["pnpm"],
            &["mix", "cargo", "java"],
        ),
        (
            &["format", "apps/api/lib/app.ex"],
            &["mix"],
            &["pnpm", "cargo", "java"],
        ),
        (
            &["format", "apps/api/lib"],
            &["mix"],
            &["pnpm", "cargo", "java"],
        ),
        (
            &["format", "apps/api/native/src/lib.rs"],
            &["cargo"],
            &["mix", "pnpm", "java"],
        ),
    ];

    for (args, expected, absent) in cases {
        let (repository, tools, jar) = format_repository();
        let output = run_format(repository.path(), tools.path(), &jar, args);
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
        let lines = log_lines(repository.path());
        for program in *expected {
            assert!(
                lines.iter().any(|line| line.starts_with(program)),
                "{args:?}: missing {program} in {lines:?}"
            );
        }
        for program in *absent {
            assert!(
                lines.iter().all(|line| !line.starts_with(program)),
                "{args:?}: unexpected {program} in {lines:?}"
            );
        }
        if args.last() == Some(&"apps/api/assets/src") {
            assert!(
                lines.iter().any(|line| {
                    line.ends_with("--no-error-on-unmatched-pattern -- src/app.ts")
                })
            );
        }
        if args.last() == Some(&"apps/api/assets/src/app.ts") {
            assert!(
                lines.iter().any(|line| {
                    line.ends_with("--no-error-on-unmatched-pattern -- src/app.ts")
                })
            );
        }
        if args.last() == Some(&"apps/api/lib/app.ex") {
            assert!(
                lines
                    .iter()
                    .any(|line| line.ends_with("format -- lib/app.ex"))
            );
        }
        if args.last() == Some(&"apps/api/lib") {
            assert!(
                lines
                    .iter()
                    .any(|line| line.ends_with("format -- lib/app.ex"))
            );
            assert!(lines.iter().all(|line| !line.ends_with("format lib")));
        }
        if args.last() == Some(&"apps/api/native/src/lib.rs") {
            assert!(lines.iter().any(|line| line.ends_with("fmt")));
        }
    }
}

#[cfg(unix)]
#[test]
fn internal_check_planning_uses_only_the_four_frozen_provider_commands() {
    let (repository, _tools, _jar) = format_repository();
    let workspace = Workspace::discover(repository.path()).unwrap();
    let selections = resolve_selectors(&workspace, &["@api".into()]).unwrap();

    let plan = build_plan(&workspace, &selections, Capability::FormatCheck).unwrap();
    let commands = plan
        .steps()
        .iter()
        .map(|step| {
            (
                step.provider.as_str(),
                step.program.to_string_lossy().into_owned(),
                step.argv
                    .iter()
                    .map(|argument| argument.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        commands,
        vec![
            (
                "oxfmt",
                "pnpm".into(),
                vec![
                    "exec".into(),
                    "oxfmt".into(),
                    "--check".into(),
                    "--no-error-on-unmatched-pattern".into(),
                    "--".into(),
                    "src/app.ts".into()
                ]
            ),
            (
                "mix-format",
                "mix".into(),
                vec!["format".into(), "--check-formatted".into()]
            ),
            (
                "rustfmt",
                "cargo".into(),
                vec!["fmt".into(), "--".into(), "--check".into()]
            ),
        ]
    );
}

#[cfg(unix)]
#[test]
fn repeated_selectors_and_overlapping_provider_evidence_produce_one_stable_step() {
    let (repository, tools, jar) = format_repository();

    let output = run_format(
        repository.path(),
        tools.path(),
        &jar,
        &[
            "format",
            "apps/api/assets/src/app.ts",
            "apps/api/assets/src/app.ts",
        ],
    );

    assert_eq!(output.status.code(), Some(0));
    let lines = log_lines(repository.path());
    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with("pnpm\t"));
    assert!(lines[0].ends_with("--no-error-on-unmatched-pattern -- src/app.ts"));
}

#[cfg(unix)]
#[test]
fn complete_preflight_fails_closed_before_any_formatter_executes() {
    let (repository, tools, jar) = format_repository();
    fs::remove_file(tools.path().join("cargo")).unwrap();

    let output = run_format(repository.path(), tools.path(), &jar, &["format", "@api"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(!repository.path().join("tools.log").exists());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("formatter tool 'cargo' is not available")
    );
}

#[cfg(unix)]
#[test]
fn independent_failures_are_aggregated_and_json_stdout_remains_pure() {
    let (repository, tools, jar) = format_repository();
    let output = workflow()
        .args(["format", "@api", "--output", "json"])
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools.path())
        .env("FORMAT_LOG", repository.path().join("tools.log"))
        .env("TLA2TOOLS_JAR", &jar)
        .env("MIX_STATUS", "7")
        .env("CARGO_STATUS", "9")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "failed");
    assert_eq!(report["steps"].as_array().unwrap().len(), 3);
    let steps = report["steps"].as_array().unwrap();
    let exit_code = |provider: &str| {
        steps
            .iter()
            .find(|step| step["provider"] == provider)
            .unwrap()["exit_code"]
            .as_i64()
            .unwrap()
    };
    assert_eq!(exit_code("mix-format"), 7);
    assert_eq!(exit_code("oxfmt"), 0);
    assert_eq!(exit_code("rustfmt"), 9);
    assert_eq!(log_lines(repository.path()).len(), 3);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("mix-out"));
    assert!(stderr.contains("pnpm-err"));
    assert!(stderr.contains("cargo-out"));
    let mix = steps
        .iter()
        .find(|step| step["provider"] == "mix-format")
        .unwrap();
    let rust = steps
        .iter()
        .find(|step| step["provider"] == "rustfmt")
        .unwrap();
    assert_eq!(mix["stdout"], "mix-out\n");
    assert_eq!(rust["stderr"], "cargo-err\n");
}

#[cfg(unix)]
#[test]
fn unix_sigint_returns_130_stops_remaining_steps_and_cleans_tla_reservations() {
    use std::os::unix::process::CommandExt;
    use std::thread;
    use std::time::Duration;

    let (repository, tools, jar) = format_repository();
    let mut command = workflow();
    command
        .args(["format", "--output", "json"])
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools.path())
        .env("FORMAT_LOG", repository.path().join("tools.log"))
        .env("TLA2TOOLS_JAR", &jar)
        .env("FORMAT_DELAY", "10")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let child = command.spawn().unwrap();
    for _ in 0..500 {
        if log_lines(repository.path())
            .iter()
            .any(|line| line.starts_with("cargo\t"))
        {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let signal = Command::new("/bin/kill")
        .args(["-INT", &format!("-{}", child.id())])
        .status()
        .unwrap();
    assert!(signal.success());
    let output = child.wait_with_output().unwrap();

    assert_eq!(output.status.code(), Some(130));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "interrupted");
    assert!(
        log_lines(repository.path())
            .iter()
            .all(|line| !line.starts_with("java\t"))
    );
    assert!(temporary_files(&repository.path().join("specs/Model.tla")).is_empty());
}

#[cfg(unix)]
#[test]
fn formatter_targets_are_terminated_from_provider_options() {
    let (repository, _tools, _jar) = format_repository();
    let oxfmt_source = repository.path().join("apps/api/assets/--config=other.ts");
    let mix_source = repository.path().join("apps/api/--stale.ex");
    fs::write(&oxfmt_source, "const value=1").unwrap();
    fs::write(&mix_source, "defmodule Stale do end").unwrap();
    let workspace = Workspace::discover(repository.path()).unwrap();

    for (source, provider) in [(oxfmt_source, "oxfmt"), (mix_source, "mix-format")] {
        let source = source.canonicalize().unwrap();
        let selector = source
            .strip_prefix(&workspace.root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let selections = resolve_selectors(&workspace, &[selector]).unwrap();
        let plan = build_plan(&workspace, &selections, Capability::Format).unwrap();
        let step = plan
            .steps()
            .iter()
            .find(|step| step.provider.as_str() == provider)
            .unwrap();
        let delimiter = step
            .argv
            .iter()
            .position(|argument| argument == "--")
            .unwrap();
        assert!(step.argv[delimiter + 1].to_string_lossy().starts_with("--"));
    }
}

#[cfg(unix)]
#[test]
fn voluntary_exit_130_is_a_failure_not_an_interruption() {
    let (repository, tools, jar) = format_repository();
    let output = workflow()
        .args(["format", "--output", "json"])
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools.path())
        .env("FORMAT_LOG", repository.path().join("tools.log"))
        .env("TLA2TOOLS_JAR", &jar)
        .env("CARGO_STATUS", "130")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "failed");
    assert!(
        log_lines(repository.path())
            .iter()
            .any(|line| line.starts_with("java\t"))
    );
}

#[cfg(unix)]
#[test]
fn human_reporting_contains_each_step_and_the_aggregate_result() {
    let (repository, tools, jar) = format_repository();

    let output = run_format(
        repository.path(),
        tools.path(),
        &jar,
        &["format", "@api:web"],
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("api/web · format · oxfmt · apps/api/assets · passed"));
    assert!(stderr.contains("1 steps · passed"));
}

#[cfg(unix)]
fn tla_check_step(java: &Path, jar: &Path, source: &Path) -> Step {
    Step {
        id: StepId::new(format!("tla-check:{}", source.display())),
        label: "TLA+ format check".into(),
        capability: Capability::FormatCheck,
        provider: ProviderId::new("tla-format"),
        owner: Owner::Repository(SystemResource::Specifications),
        cwd: source.parent().unwrap().to_path_buf(),
        program: java.as_os_str().to_owned(),
        argv: vec![
            OsString::from("-cp"),
            jar.as_os_str().to_owned(),
            OsString::from("formatter.Main"),
            source.as_os_str().to_owned(),
            source.as_os_str().to_owned(),
        ],
        env: BTreeMap::new(),
        targets: vec![source.to_path_buf()],
        configuration_files: Vec::new(),
        depends_on: Vec::new(),
    }
}

#[cfg(unix)]
fn temporary_files(source: &Path) -> Vec<PathBuf> {
    fs::read_dir(source.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".just-format-")
        })
        .collect()
}

#[cfg(unix)]
#[test]
fn tla_failure_preserves_original_reports_java_status_and_cleans_temporary_output() {
    let (repository, tools, jar) = format_repository();
    let source = repository.path().join("specs/Model.tla");
    let original = fs::read(&source).unwrap();

    let output = workflow()
        .arg("format")
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools.path())
        .env("FORMAT_LOG", repository.path().join("tools.log"))
        .env("TLA2TOOLS_JAR", &jar)
        .env("JAVA_MODE", "fail")
        .env("MIX_STATUS", "0")
        .env("PNPM_STATUS", "0")
        .env("CARGO_STATUS", "0")
        .env("RUST_BACKTRACE", "0")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(fs::read(&source).unwrap(), original);
    assert!(temporary_files(&source).is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("java-failed"));
    let java = log_lines(repository.path())
        .into_iter()
        .find(|line| line.starts_with("java\t"))
        .unwrap();
    let output_path = PathBuf::from(java.split('\t').nth(2).unwrap().split(' ').nth(4).unwrap());
    assert_eq!(
        output_path.parent().unwrap().canonicalize().unwrap(),
        source.parent().unwrap().canonicalize().unwrap()
    );

    let json = workflow()
        .args(["format", "specs/Model.tla", "--output", "json"])
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools.path())
        .env("FORMAT_LOG", repository.path().join("tools-2.log"))
        .env("TLA2TOOLS_JAR", &jar)
        .env("JAVA_MODE", "fail")
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(report["steps"][0]["exit_code"], 23);
}

#[cfg(unix)]
#[test]
fn tla_success_atomically_replaces_content_and_preserves_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let (repository, tools, jar) = format_repository();
    let source = repository.path().join("specs/Model.tla");
    fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).unwrap();

    let output = workflow()
        .args(["format", "specs/Model.tla"])
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools.path())
        .env("FORMAT_LOG", repository.path().join("tools.log"))
        .env("TLA2TOOLS_JAR", &jar)
        .env("JAVA_MODE", "change")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(fs::read_to_string(&source).unwrap(), "formatted\n");
    assert_eq!(
        fs::metadata(&source).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert!(temporary_files(&source).is_empty());
}

#[cfg(unix)]
#[test]
fn tla_preflight_reserves_with_source_mode_before_java_runs() {
    use std::os::unix::fs::PermissionsExt;

    let (repository, tools, jar) = format_repository();
    let source = repository.path().join("specs/Model.tla");
    let mode_log = repository.path().join("temporary-mode");
    fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();

    let output = workflow()
        .args(["format", "specs/Model.tla"])
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools.path())
        .env("FORMAT_LOG", repository.path().join("tools.log"))
        .env("TEMP_MODE_LOG", &mode_log)
        .env("TLA2TOOLS_JAR", &jar)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(fs::read_to_string(mode_log).unwrap().trim(), "600");
    assert_eq!(
        fs::metadata(source).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[cfg(unix)]
#[test]
fn tla_allocation_failure_cleans_earlier_reservations_and_executes_nothing() {
    use std::os::unix::fs::PermissionsExt;

    let (repository, tools, jar) = format_repository();
    let first = repository.path().join("specs/First.tla");
    let blocked_directory = repository.path().join("blocked");
    let second = blocked_directory.join("Second.tla");
    fs::write(&first, "---- MODULE First ----\n====\n").unwrap();
    fs::create_dir(&blocked_directory).unwrap();
    fs::write(&second, "---- MODULE Second ----\n====\n").unwrap();
    fs::set_permissions(&blocked_directory, fs::Permissions::from_mode(0o500)).unwrap();

    let workspace = Workspace::discover(repository.path()).unwrap();
    let plan = Plan::new(vec![
        tla_check_step(&tools.path().join("java"), &jar, &first),
        tla_check_step(&tools.path().join("java"), &jar, &second),
    ])
    .unwrap();
    let result = execute_plan(&workspace, &plan, vec![], "now".into());

    fs::set_permissions(&blocked_directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err(), "{result:?}");
    assert!(!repository.path().join("tools.log").exists());
    assert!(temporary_files(&first).is_empty());
    assert!(temporary_files(&second).is_empty());
}

#[cfg(unix)]
#[test]
fn oxfmt_routes_only_the_frozen_extensions_and_empty_work_is_a_no_op() {
    let (repository, tools, jar) = format_repository();
    let root = repository.path().join("apps/api/assets");
    for extension in [
        "js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts", "css", "scss",
    ] {
        fs::write(root.join(format!("src/frozen.{extension}")), "content").unwrap();
    }
    fs::write(root.join("src/unrelated.py"), "pass\n").unwrap();
    fs::create_dir(root.join("empty")).unwrap();

    for capability in [Capability::Format, Capability::FormatCheck] {
        let workspace = Workspace::discover(repository.path()).unwrap();
        let selections = resolve_selectors(&workspace, &["apps/api/assets/src".into()]).unwrap();
        let plan = build_plan(&workspace, &selections, capability).unwrap();
        let step = plan.steps().first().unwrap();
        let argv = step
            .argv
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            argv.iter()
                .any(|argument| argument == "--no-error-on-unmatched-pattern")
        );
        assert!(argv.iter().all(|argument| {
            !Path::new(argument)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("py"))
        }));
        for extension in [
            "js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts", "css", "scss",
        ] {
            assert!(
                argv.iter()
                    .any(|argument| argument == &format!("src/frozen.{extension}")),
                "missing {extension} from {argv:?}"
            );
        }
    }

    for selector in ["apps/api/assets/src/unrelated.py", "apps/api/assets/empty"] {
        let output = run_format(repository.path(), tools.path(), &jar, &["format", selector]);
        assert_eq!(output.status.code(), Some(0), "{selector}: {output:?}");
        assert!(log_lines(repository.path()).is_empty(), "{selector}");
    }

    fs::remove_dir_all(root.join("src")).unwrap();
    let output = run_format(
        repository.path(),
        tools.path(),
        &jar,
        &["format", "apps/api/assets"],
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(log_lines(repository.path()).is_empty());
}

#[cfg(unix)]
#[test]
fn nested_oxfmt_packages_receive_only_their_nearest_sources() {
    let (repository, _tools, _jar) = format_repository();
    let parent = repository.path().join("apps/api/assets");
    let child = parent.join("nested");
    fs::create_dir(&child).unwrap();
    fs::write(
        child.join("package.json"),
        r#"{"devDependencies":{"oxfmt":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(child.join("child.cts"), "const child=1").unwrap();

    let workspace = Workspace::discover(repository.path()).unwrap();
    let selections = resolve_selectors(&workspace, &["@api".into()]).unwrap();
    let plan = build_plan(&workspace, &selections, Capability::Format).unwrap();
    let oxfmt = plan
        .steps()
        .iter()
        .filter(|step| step.provider.as_str() == "oxfmt")
        .collect::<Vec<_>>();

    assert_eq!(oxfmt.len(), 2);
    let parent_step = oxfmt
        .iter()
        .find(|step| step.cwd == parent.canonicalize().unwrap())
        .unwrap();
    assert_eq!(parent_step.targets, vec![PathBuf::from("src/app.ts")]);
    let child_step = oxfmt
        .iter()
        .find(|step| step.cwd == child.canonicalize().unwrap())
        .unwrap();
    assert_eq!(child_step.targets, vec![PathBuf::from("child.cts")]);
}

#[cfg(unix)]
#[test]
fn relative_tla_jar_is_resolved_before_java_changes_directory() {
    let (repository, tools, _jar) = format_repository();

    let output = workflow()
        .args(["format", "specs/Model.tla"])
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools.path())
        .env("FORMAT_LOG", repository.path().join("tools.log"))
        .env("TLA2TOOLS_JAR", "tla2tools.jar")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let java = log_lines(repository.path())
        .into_iter()
        .find(|line| line.starts_with("java\t"))
        .unwrap();
    let classpath = java.split('\t').nth(2).unwrap().split(' ').nth(1).unwrap();
    assert!(Path::new(classpath).is_absolute(), "{java}");
}

#[cfg(unix)]
#[test]
fn internal_tla_check_compares_temporary_output_without_mutating_source() {
    let (repository, tools, jar) = format_repository();
    let workspace = Workspace::discover(repository.path()).unwrap();
    let source = repository.path().join("specs/Model.tla");
    let original = fs::read(&source).unwrap();

    let mut changed = tla_check_step(&tools.path().join("java"), &jar, &source);
    changed
        .env
        .insert(OsString::from("JAVA_MODE"), OsString::from("change"));
    changed.env.insert(
        OsString::from("FORMAT_LOG"),
        repository.path().join("tools.log").into_os_string(),
    );
    let plan = Plan::new(vec![changed]).unwrap();
    let (report, _, _) = execute_plan(&workspace, &plan, vec![], "now".into()).unwrap();
    assert_eq!(report.status, ReportStatus::Failed);
    assert_eq!(report.steps[0].status, StepStatus::Failed);
    assert_eq!(report.steps[0].exit_code, Some(1));
    assert_eq!(fs::read(&source).unwrap(), original);
    assert!(temporary_files(&source).is_empty());

    let mut unchanged = tla_check_step(&tools.path().join("java"), &jar, &source);
    unchanged
        .env
        .insert(OsString::from("JAVA_MODE"), OsString::from("copy"));
    unchanged.env.insert(
        OsString::from("FORMAT_LOG"),
        repository.path().join("tools.log").into_os_string(),
    );
    let plan = Plan::new(vec![unchanged]).unwrap();
    let (report, _, _) = execute_plan(&workspace, &plan, vec![], "now".into()).unwrap();
    assert_eq!(report.status, ReportStatus::Passed);
    assert_eq!(fs::read(&source).unwrap(), original);
    assert!(temporary_files(&source).is_empty());

    let mut failed = tla_check_step(&tools.path().join("java"), &jar, &source);
    failed
        .env
        .insert(OsString::from("JAVA_MODE"), OsString::from("fail"));
    failed.env.insert(
        OsString::from("FORMAT_LOG"),
        repository.path().join("tools.log").into_os_string(),
    );
    let plan = Plan::new(vec![failed]).unwrap();
    let (report, _, _) = execute_plan(&workspace, &plan, vec![], "now".into()).unwrap();
    assert_eq!(report.status, ReportStatus::Failed);
    assert_eq!(report.steps[0].exit_code, Some(23));
    assert_eq!(fs::read(&source).unwrap(), original);
    assert!(temporary_files(&source).is_empty());
}
