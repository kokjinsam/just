#![cfg(test)]
#![cfg(unix)]
//! Integration tests for specification workflows.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use kokjinsam_just::specification::{SpecificationKind, build_plan};
use kokjinsam_just::workspace::Workspace;

fn workflow() -> Command {
    Command::new(env!("CARGO_BIN_EXE_workflow"))
}

fn repository() -> tempfile::TempDir {
    let repository = tempfile::tempdir().unwrap();
    for directory in [
        "app",
        "specs/tla/nested",
        "specs/alloy",
        "specs/openapi",
        "tests",
    ] {
        fs::create_dir_all(repository.path().join(directory)).unwrap();
    }
    fs::write(
        repository.path().join("just.yaml"),
        concat!(
            "version: 1\n",
            "system:\n  specs: specs\n  tests: tests\n",
            "targets:\n  app:\n    path: app\n",
        ),
    )
    .unwrap();
    repository
}

fn write(path: impl AsRef<Path>, source: &str) {
    fs::write(path, source).unwrap();
}

fn tools() -> tempfile::TempDir {
    let tools = tempfile::tempdir().unwrap();
    for name in ["apalache-mc", "alloy", "oasdiff", "vacuum"] {
        let path = tools.path().join(name);
        fs::write(
            &path,
            r#"#!/bin/sh
printf '%s|%s|%s\n' "${0##*/}" "$PWD" "$*" >> "$SPEC_LOG"
case "$*" in
  *interrupt*) kill -INT $$ ;;
  *slow*) /bin/sleep 10 ;;
  *voluntary*) exit 130 ;;
  *fail*) exit 1 ;;
esac
exit 0
"#,
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    tools
}

fn run(repository: &Path, tools: &Path, log: &Path, args: &[&str]) -> Output {
    workflow()
        .args(args)
        .current_dir(repository)
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools)
        .env("SPEC_LOG", log)
        .output()
        .unwrap()
}

#[test]
fn plans_tla_alloy_and_openapi_in_deterministic_order_with_configuration_evidence() {
    let repository = repository();
    write(repository.path().join("specs/tla/.apalache.cfg"), "config");
    write(
        repository.path().join("specs/tla/Model.tla"),
        "---- MODULE Model ----\n====\n",
    );
    write(repository.path().join("specs/tla/Model.cfg"), "INIT Init");
    write(
        repository.path().join("specs/tla/nested/Types.tla"),
        "---- MODULE Types ----\n====\n",
    );
    write(
        repository.path().join("specs/alloy/domain.als"),
        "run {} expect 1",
    );
    write(
        repository.path().join("specs/openapi/vacuum.conf.yaml"),
        "rules: {}",
    );
    write(
        repository.path().join("specs/openapi/api.yaml"),
        "openapi: 3.1.0",
    );

    let workspace = Workspace::discover(repository.path()).unwrap();
    let plan = build_plan(&workspace, None, &[]).unwrap();
    let providers = plan
        .steps()
        .iter()
        .map(|step| step.provider.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        providers,
        ["apalache", "apalache", "alloy", "oasdiff", "vacuum"]
    );
    assert_eq!(
        plan.steps()[0]
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>(),
        [
            "--out-dir=<temporary>",
            "check",
            "--config=Model.cfg",
            "Model.tla"
        ]
    );
    assert_eq!(
        plan.steps()[1]
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>(),
        ["--out-dir=<temporary>", "typecheck", "nested/Types.tla"]
    );
    assert!(plan.steps().iter().all(|step| {
        step.capability == kokjinsam_just::capability::Capability::SpecCheck
            && step.owner
                == kokjinsam_just::ownership::Owner::Repository(
                    kokjinsam_just::ownership::SystemResource::Specifications,
                )
    }));
    assert_eq!(
        plan.steps()[4].configuration_files,
        [repository
            .path()
            .join("specs/openapi/vacuum.conf.yaml")
            .canonicalize()
            .unwrap()]
    );
    let named = build_plan(
        &workspace,
        Some(SpecificationKind::Tla),
        &["Model".into(), "Model.tla".into()],
    )
    .unwrap();
    assert_eq!(named.steps().len(), 1);
    assert_eq!(named.steps()[0].provider.as_str(), "apalache");
}

#[test]
fn dotted_bare_openapi_name_resolves_by_file_stem() {
    let repository = repository();
    write(
        repository.path().join("specs/openapi/api.v1.yaml"),
        "openapi: 3.1.0",
    );
    let workspace = Workspace::discover(repository.path()).unwrap();

    let plan = build_plan(
        &workspace,
        Some(SpecificationKind::Openapi),
        &["api.v1".into()],
    )
    .unwrap();

    let expected = repository
        .path()
        .join("specs/openapi/api.v1.yaml")
        .canonicalize()
        .unwrap();
    assert_eq!(plan.steps().len(), 2);
    assert!(
        plan.steps()
            .iter()
            .all(|step| step.targets == [expected.clone()])
    );
}

#[test]
fn explicit_ignored_path_is_selectable_but_whole_discovery_ignores_it() {
    let repository = repository();
    write(
        repository.path().join(".gitignore"),
        "specs/alloy/ignored.als\n",
    );
    write(
        repository.path().join("specs/alloy/visible.als"),
        "run {} expect 1",
    );
    write(
        repository.path().join("specs/alloy/ignored.als"),
        "run {} expect 1",
    );
    let workspace = Workspace::discover(repository.path()).unwrap();

    let whole = build_plan(&workspace, Some(SpecificationKind::Alloy), &[]).unwrap();
    assert_eq!(whole.steps().len(), 1);
    assert!(whole.steps()[0].targets[0].ends_with("visible.als"));

    let explicit = build_plan(
        &workspace,
        Some(SpecificationKind::Alloy),
        &["specs/alloy/ignored.als".into()],
    )
    .unwrap();
    assert_eq!(explicit.steps().len(), 1);
    assert!(explicit.steps()[0].targets[0].ends_with("ignored.als"));
}

#[test]
fn ignored_tla_module_is_an_orphan_for_discovery_but_explicitly_selectable() {
    let repository = repository();
    write(
        repository.path().join(".gitignore"),
        "specs/tla/Model.tla\n",
    );
    write(repository.path().join("specs/tla/Model.tla"), "module");
    write(repository.path().join("specs/tla/Model.cfg"), "INIT Init");
    let tools = tools();
    let log = repository.path().join("commands.log");

    for args in [&["spec"][..], &["spec", "tla"][..]] {
        let output = run(repository.path(), tools.path(), &log, args);
        assert_eq!(output.status.code(), Some(2));
        assert!(!log.exists());
    }

    let explicit = run(
        repository.path(),
        tools.path(),
        &log,
        &["spec", "tla", "specs/tla/Model.tla"],
    );
    assert_eq!(explicit.status.code(), Some(0));
    let command = fs::read_to_string(&log).unwrap();
    assert!(command.starts_with("apalache-mc|"));
    assert!(command.contains("|--out-dir="));
    assert!(command.contains(" check --config=Model.cfg Model.tla\n"));
    assert_eq!(command.lines().count(), 1);
}

#[test]
fn missing_path_shaped_selection_does_not_fall_back_to_basename() {
    let repository = repository();
    fs::create_dir_all(repository.path().join("specs/tla/actual")).unwrap();
    write(
        repository.path().join("specs/tla/actual/Model.tla"),
        "module",
    );
    let tools = tools();
    let log = repository.path().join("commands.log");

    let output = run(
        repository.path(),
        tools.path(),
        &log,
        &["spec", "tla", "missing/Model"],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(!log.exists());
}

#[test]
fn bare_tla_name_ignores_extensionless_direct_file_and_selects_nested_module() {
    let repository = repository();
    write(repository.path().join("specs/tla/Model"), "not a module");
    write(
        repository.path().join("specs/tla/nested/Model.tla"),
        "module",
    );
    let tools = tools();
    let log = repository.path().join("commands.log");

    let output = run(
        repository.path(),
        tools.path(),
        &log,
        &["spec", "tla", "Model"],
    );
    assert_eq!(output.status.code(), Some(0));
    let command = fs::read_to_string(&log).unwrap();
    assert_eq!(command.lines().count(), 1);
    assert!(command.contains(" typecheck nested/Model.tla\n"));
}

#[test]
fn bare_tla_name_ignores_wrong_kind_files_and_normalizes_a_pair_once() {
    let repository = repository();
    write(repository.path().join("specs/tla/Model.tla"), "module");
    write(repository.path().join("specs/tla/Model.md"), "notes");
    let workspace = Workspace::discover(repository.path()).unwrap();

    let module = build_plan(&workspace, Some(SpecificationKind::Tla), &["Model".into()]).unwrap();
    assert_eq!(module.steps().len(), 1);
    assert_eq!(module.steps()[0].argv[1], "typecheck");

    write(repository.path().join("specs/tla/Model.cfg"), "INIT Init");
    let paired = build_plan(&workspace, Some(SpecificationKind::Tla), &["Model".into()]).unwrap();
    assert_eq!(paired.steps().len(), 1);
    assert_eq!(paired.steps()[0].argv[1], "check");
    assert_eq!(paired.steps()[0].argv[2], "--config=Model.cfg");
}

#[test]
fn invalid_selections_and_orphan_tla_configuration_fail_before_launch() {
    let repository = repository();
    write(repository.path().join("specs/tla/Orphan.cfg"), "INIT Init");
    let workspace = Workspace::discover(repository.path()).unwrap();
    for (kind, selection) in [
        (SpecificationKind::Tla, "@app"),
        (SpecificationKind::Tla, "../../escape.tla"),
        (SpecificationKind::Tla, "missing"),
        (SpecificationKind::Tla, "specs/alloy/model.als"),
    ] {
        assert!(build_plan(&workspace, Some(kind), &[selection.into()]).is_err());
    }
    assert!(build_plan(&workspace, Some(SpecificationKind::Tla), &[]).is_err());
}

#[test]
fn execution_aggregates_failures_keeps_json_pure_and_cleans_temporary_outputs() {
    let repository = repository();
    write(repository.path().join("specs/tla/Pass.tla"), "module");
    write(
        repository.path().join("specs/alloy/fail.als"),
        "run {} expect 1",
    );
    write(
        repository.path().join("specs/openapi/fail.yaml"),
        "openapi: 3.1.0",
    );
    let tools = tools();
    let log = repository.path().join("commands.log");

    let output = run(
        repository.path(),
        tools.path(),
        &log,
        &["spec", "--output", "json"],
    );
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "failed");
    assert_eq!(report["steps"].as_array().unwrap().len(), 4);
    let lines = fs::read_to_string(&log).unwrap();
    assert_eq!(lines.lines().count(), 4);
    for argument in lines
        .split_whitespace()
        .filter(|argument| argument.contains(".just-spec-"))
    {
        let path = argument
            .trim_start_matches("--out-dir=")
            .trim_end_matches('|');
        assert!(!PathBuf::from(path).exists(), "{path}");
    }
}

#[test]
fn child_signal_two_interrupts_but_voluntary_exit_130_is_a_failure() {
    let repository = repository();
    write(repository.path().join("specs/alloy/interrupt.als"), "model");
    write(repository.path().join("specs/alloy/voluntary.als"), "model");
    let tools = tools();
    let log = repository.path().join("commands.log");

    let interrupted = run(
        repository.path(),
        tools.path(),
        &log,
        &["spec", "alloy", "interrupt"],
    );
    assert_eq!(interrupted.status.code(), Some(130));

    let voluntary = run(
        repository.path(),
        tools.path(),
        &log,
        &["spec", "alloy", "voluntary"],
    );
    assert_eq!(voluntary.status.code(), Some(1));
}

#[test]
fn real_sigint_stops_the_active_child_and_returns_130() {
    let repository = repository();
    write(repository.path().join("specs/alloy/slow.als"), "model");
    let tools = tools();
    let log = repository.path().join("commands.log");
    let child = workflow()
        .args(["spec", "alloy", "slow"])
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools.path())
        .env("SPEC_LOG", &log)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if log.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(log.exists());
    let signal = Command::new("/bin/kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(signal.success());
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(130));
}

#[test]
fn empty_kind_is_a_success_and_missing_tools_preflight_all_steps() {
    let repository = repository();
    let tools = tools();
    let log = repository.path().join("commands.log");
    let empty = run(repository.path(), tools.path(), &log, &["spec", "alloy"]);
    assert_eq!(empty.status.code(), Some(0));
    assert!(!log.exists());

    write(repository.path().join("specs/tla/Model.tla"), "module");
    write(repository.path().join("specs/alloy/Model.als"), "model");
    fs::remove_file(tools.path().join("alloy")).unwrap();
    let missing = run(repository.path(), tools.path(), &log, &["spec"]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(!log.exists());
}
