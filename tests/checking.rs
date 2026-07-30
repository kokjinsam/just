#![cfg(test)]
#![cfg(unix)]
//! Integration tests for type-checking and compilation workflows.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use kokjinsam_just::capability::Capability;
use kokjinsam_just::checking::build_plan;
use kokjinsam_just::ownership::Owner;
use kokjinsam_just::selector::resolve_selectors;
use kokjinsam_just::workspace::Workspace;

fn executable(path: &Path, source: &str) {
    fs::write(path, source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn repository() -> tempfile::TempDir {
    let repository = tempfile::tempdir().unwrap();
    for directory in [
        "apps/api/lib",
        "apps/api/assets/src",
        "apps/api/native/src",
        "apps/api/empty",
        "contracts",
        "tests",
    ] {
        fs::create_dir_all(repository.path().join(directory)).unwrap();
    }
    fs::write(
        repository.path().join("just.yaml"),
        concat!(
            "version: 1\n",
            "system:\n  specs: contracts\n  tests: tests\n",
            "targets:\n",
            "  api:\n",
            "    path: apps/api\n",
            "    components:\n",
            "      backend:\n        path: .\n",
            "      web:\n        path: assets\n",
            "      native:\n        path: native\n",
            "      empty:\n        path: empty\n",
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
        r#"{"scripts":{"typecheck":"tsc","build":"vite build"}}"#,
    )
    .unwrap();
    fs::write(
        repository.path().join("apps/api/assets/src/app.ts"),
        "export {}",
    )
    .unwrap();
    fs::write(
        repository.path().join("apps/api/native/Cargo.toml"),
        "[package]\nname='native'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(
        repository.path().join("apps/api/native/src/lib.rs"),
        "pub fn value() {}",
    )
    .unwrap();
    repository
}

fn implicit_repository(with_mix_manifest: bool) -> tempfile::TempDir {
    let repository = tempfile::tempdir().unwrap();
    for directory in ["apps/api/lib", "contracts", "tests"] {
        fs::create_dir_all(repository.path().join(directory)).unwrap();
    }
    fs::write(
        repository.path().join("just.yaml"),
        concat!(
            "version: 1\n",
            "system:\n  specs: contracts\n  tests: tests\n",
            "targets:\n",
            "  api:\n",
            "    path: apps/api\n",
        ),
    )
    .unwrap();
    if with_mix_manifest {
        fs::write(repository.path().join("apps/api/mix.exs"), "Mix.Project").unwrap();
    }
    repository
}

fn tools() -> tempfile::TempDir {
    let tools = tempfile::tempdir().unwrap();
    let script = concat!(
        "#!/bin/sh\n",
        "printf '%s\\t%s\\t%s\\n' \"${0##*/}\" \"$PWD\" \"$*\" >> \"$CHECK_LOG\"\n",
        "printf '%s-out\\n' \"${0##*/}\"\n",
        "printf '%s-err\\n' \"${0##*/}\" >&2\n",
        "provider=\"${1:-}\"\n",
        "if [ \"${SIGNAL_PROVIDER:-}\" = \"$provider\" ]; then kill -INT $$; fi\n",
        "if [ \"${DELAY_PROVIDER:-}\" = \"$provider\" ]; then /bin/sleep 30; fi\n",
        "case \"$provider\" in\n",
        "  dialyzer) exit \"${DIALYZER_STATUS:-0}\" ;;\n",
        "  run) exit \"${PNPM_STATUS:-0}\" ;;\n",
        "  compile) exit \"${MIX_COMPILE_STATUS:-0}\" ;;\n",
        "  check) exit \"${CARGO_STATUS:-0}\" ;;\n",
        "esac\n",
    );
    for name in ["mix", "pnpm", "cargo"] {
        executable(&tools.path().join(name), script);
    }
    for name in ["uv", "pyrefly", "ruff", "schemathesis"] {
        executable(
            &tools.path().join(name),
            "#!/bin/sh\nprintf forbidden >> \"$FORBIDDEN_LOG\"\nexit 99\n",
        );
    }
    tools
}

fn workflow() -> Command {
    Command::new(env!("CARGO_BIN_EXE_workflow"))
}

fn run(repository: &Path, tools: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut command = workflow();
    command
        .args(args)
        .current_dir(repository)
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools)
        .env("CHECK_LOG", repository.join("checking.log"))
        .env("FORBIDDEN_LOG", repository.join("forbidden.log"));
    for (name, value) in envs {
        command.env(name, value);
    }
    command.output().unwrap()
}

fn log_lines(repository: &Path) -> Vec<String> {
    fs::read_to_string(repository.join("checking.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn implicit_mix_target_has_only_its_applicable_provider_for_repository_and_target() {
    let repository = implicit_repository(true);
    let workspace = Workspace::discover(repository.path()).unwrap();

    for selectors in [vec![], vec!["@api".to_owned()]] {
        let selections = resolve_selectors(&workspace, &selectors).unwrap();
        for (capability, expected_provider) in [
            (Capability::Typecheck, "dialyzer"),
            (Capability::Compile, "mix-compile"),
        ] {
            let plan = build_plan(&workspace, &selections, capability).unwrap();
            assert_eq!(plan.steps().len(), 1, "{selectors:?} {capability:?}");
            assert_eq!(plan.steps()[0].provider.as_str(), expected_provider);
            assert_eq!(
                plan.steps()[0].owner,
                Owner::Component {
                    target: "api".into(),
                    component: None,
                }
            );
        }
    }
}

#[test]
fn manifest_free_implicit_target_is_one_target_scoped_successful_skip() {
    let repository = implicit_repository(false);
    let workspace = Workspace::discover(repository.path()).unwrap();
    let tools = tools();

    for selectors in [vec![], vec!["@api".to_owned()]] {
        let selections = resolve_selectors(&workspace, &selectors).unwrap();
        let plan = build_plan(&workspace, &selections, Capability::Typecheck).unwrap();
        assert_eq!(plan.steps().len(), 1, "{selectors:?}");
        assert_eq!(plan.steps()[0].provider.as_str(), "not-applicable");
        assert_eq!(
            plan.steps()[0].owner,
            Owner::Component {
                target: "api".into(),
                component: None,
            }
        );

        let mut args = vec!["typecheck"];
        args.extend(selectors.iter().map(String::as_str));
        let output = run(repository.path(), tools.path(), &args, &[]);
        assert_eq!(output.status.code(), Some(0), "{selectors:?}");
        assert!(output.stdout.is_empty());
        let report = String::from_utf8(output.stderr).unwrap();
        assert!(report.contains("api · typecheck · not-applicable"));
        assert!(report.contains("1 steps · passed"));
    }
    assert!(log_lines(repository.path()).is_empty());
}

#[test]
fn exact_commands_cwds_order_all_selectors_and_deduplication_are_frozen() {
    let repository = repository();
    let workspace = Workspace::discover(repository.path()).unwrap();
    for (capability, expected) in [
        (
            Capability::Typecheck,
            vec![
                ("dialyzer", "apps/api", "mix dialyzer"),
                ("package-typecheck", "apps/api/assets", "pnpm run typecheck"),
                ("not-applicable", "apps/api/empty", ""),
                ("not-applicable", "apps/api/native", ""),
            ],
        ),
        (
            Capability::Compile,
            vec![
                (
                    "mix-compile",
                    "apps/api",
                    "mix compile --warnings-as-errors",
                ),
                ("package-build", "apps/api/assets", "pnpm run build"),
                (
                    "cargo-check",
                    "apps/api/native",
                    "cargo check --all-targets",
                ),
                ("not-applicable", "apps/api/empty", ""),
            ],
        ),
    ] {
        let selections = resolve_selectors(&workspace, &[]).unwrap();
        let plan = build_plan(&workspace, &selections, capability).unwrap();
        let actual = plan
            .steps()
            .iter()
            .map(|step| {
                (
                    step.provider.as_str(),
                    step.cwd
                        .strip_prefix(&workspace.root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    std::iter::once(step.program.to_string_lossy().into_owned())
                        .chain(
                            step.argv
                                .iter()
                                .map(|argument| argument.to_string_lossy().into_owned()),
                        )
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            expected
                .into_iter()
                .map(|(provider, cwd, argv)| (provider, cwd.into(), argv.into()))
                .collect::<Vec<(&str, String, String)>>()
        );
        assert!(plan.steps().iter().all(|step| step.targets.is_empty()));
    }

    let cases = [
        (
            "@api",
            vec![
                "dialyzer",
                "package-typecheck",
                "not-applicable",
                "not-applicable",
            ],
        ),
        ("@api:backend", vec!["dialyzer"]),
        ("@api:web", vec!["package-typecheck"]),
        ("apps/api/assets/src", vec!["package-typecheck"]),
        ("apps/api/assets/src/app.ts", vec!["package-typecheck"]),
        ("apps/api/native/Cargo.toml", vec!["not-applicable"]),
        ("@api:empty", vec!["not-applicable"]),
    ];
    for (selector, expected) in cases {
        let selectors = vec![selector.to_owned(), selector.to_owned()];
        let selections = resolve_selectors(&workspace, &selectors).unwrap();
        let plan = build_plan(&workspace, &selections, Capability::Typecheck).unwrap();
        assert_eq!(
            plan.steps()
                .iter()
                .map(|step| step.provider.as_str())
                .collect::<Vec<_>>(),
            expected,
            "{selector}"
        );
    }
}

#[test]
fn missing_node_scripts_and_manifest_free_scopes_report_not_applicable_successfully() {
    let repository = repository();
    fs::write(
        repository.path().join("apps/api/assets/package.json"),
        r#"{"scripts":{}}"#,
    )
    .unwrap();
    let tools = tools();
    for (command, selector) in [
        ("typecheck", "@api:web"),
        ("compile", "@api:web"),
        ("typecheck", "@api:native"),
        ("typecheck", "@api:empty"),
    ] {
        let output = run(
            repository.path(),
            tools.path(),
            &[command, selector, "--output", "json"],
            &[],
        );
        assert_eq!(output.status.code(), Some(0), "{command} {selector}");
        assert!(output.stderr.is_empty());
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["status"], "passed");
        assert_eq!(report["steps"][0]["status"], "skipped");
        assert!(report["steps"][0]["exit_code"].is_null());
        assert!(
            report["steps"][0]["skip_reason"]
                .as_str()
                .unwrap()
                .contains("not applicable")
        );
    }
    assert!(log_lines(repository.path()).is_empty());
}

#[test]
fn complete_preflight_aggregation_reports_and_forbidden_provider_absence_are_enforced() {
    let repository = repository();
    let tools = tools();
    let failed = run(
        repository.path(),
        tools.path(),
        &["compile", "@api", "--output", "json"],
        &[("MIX_COMPILE_STATUS", "7"), ("CARGO_STATUS", "9")],
    );
    assert_eq!(failed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&failed.stderr).contains("mix-err"));
    let report: serde_json::Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(report["status"], "failed");
    assert_eq!(report["steps"].as_array().unwrap().len(), 4);
    assert_eq!(report["steps"][0]["exit_code"], 7);
    assert_eq!(report["steps"][2]["exit_code"], 9);
    assert_eq!(log_lines(repository.path()).len(), 3);
    assert!(!repository.path().join("forbidden.log").exists());

    fs::remove_file(repository.path().join("checking.log")).unwrap();
    fs::remove_file(tools.path().join("cargo")).unwrap();
    let preflight = run(
        repository.path(),
        tools.path(),
        &["compile", "@api", "--output", "json"],
        &[],
    );
    assert_eq!(preflight.status.code(), Some(2));
    assert!(!repository.path().join("checking.log").exists());
    assert!(
        String::from_utf8(preflight.stderr)
            .unwrap()
            .contains("compile tool 'cargo' is not available")
    );
}

#[test]
fn human_output_is_deterministic_and_voluntary_130_is_an_ordinary_failure() {
    let repository = repository();
    let tools = tools();
    let human = run(
        repository.path(),
        tools.path(),
        &["typecheck", "@api:web"],
        &[],
    );
    assert_eq!(human.status.code(), Some(0));
    assert!(human.stdout.is_empty());
    let diagnostic = String::from_utf8(human.stderr).unwrap();
    assert!(
        diagnostic.contains("api/web · typecheck · package-typecheck · apps/api/assets · passed")
    );
    assert!(diagnostic.contains("1 steps · passed"));

    fs::remove_file(repository.path().join("checking.log")).unwrap();
    let voluntary = run(
        repository.path(),
        tools.path(),
        &["typecheck", "@api"],
        &[("DIALYZER_STATUS", "130")],
    );
    assert_eq!(voluntary.status.code(), Some(1));
    assert_eq!(log_lines(repository.path()).len(), 2);
}

#[test]
fn child_signal_two_and_process_group_sigint_stop_remaining_steps() {
    let repository = repository();
    let tools = tools();
    let signalled = run(
        repository.path(),
        tools.path(),
        &["compile", "@api"],
        &[("SIGNAL_PROVIDER", "compile")],
    );
    assert_eq!(signalled.status.code(), Some(130));
    assert_eq!(log_lines(repository.path()).len(), 1);

    fs::remove_file(repository.path().join("checking.log")).unwrap();
    let mut child = workflow()
        .args(["typecheck", "@api"])
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools.path())
        .env("CHECK_LOG", repository.path().join("checking.log"))
        .env("DELAY_PROVIDER", "dialyzer")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !repository.path().join("checking.log").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        Command::new("/bin/kill")
            .args(["-INT", &format!("-{}", child.id())])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(child.wait().unwrap().code(), Some(130));
    assert_eq!(log_lines(repository.path()).len(), 1);
}

#[test]
fn explicit_ignored_manifest_is_selected_but_repository_discovery_stays_git_aware() {
    let repository = repository();
    let ignored = repository.path().join("apps/api/ignored");
    fs::create_dir(&ignored).unwrap();
    fs::write(
        ignored.join("package.json"),
        r#"{"scripts":{"typecheck":"tsc"}}"#,
    )
    .unwrap();
    fs::write(
        repository.path().join(".gitignore"),
        "apps/api/ignored/package.json\n",
    )
    .unwrap();
    let workspace = Workspace::discover(repository.path()).unwrap();
    let repository_plan = build_plan(
        &workspace,
        &resolve_selectors(&workspace, &[]).unwrap(),
        Capability::Typecheck,
    )
    .unwrap();
    assert!(
        repository_plan
            .steps()
            .iter()
            .all(|step| step.cwd != ignored)
    );
    let explicit = build_plan(
        &workspace,
        &resolve_selectors(&workspace, &["apps/api/ignored/package.json".into()]).unwrap(),
        Capability::Typecheck,
    )
    .unwrap();
    assert_eq!(explicit.steps().len(), 1);
    assert_eq!(explicit.steps()[0].provider.as_str(), "package-typecheck");
    assert_eq!(explicit.steps()[0].cwd, fs::canonicalize(ignored).unwrap());
}
