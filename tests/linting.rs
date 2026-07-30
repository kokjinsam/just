#![cfg(test)]
//! Integration tests for lint workflows.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use kokjinsam_just::capability::{Capability, bindings_for_selections};
use kokjinsam_just::discovery::{ResourceKind, discover_resources};
use kokjinsam_just::linting::build_plan;
use kokjinsam_just::selector::resolve_selectors;
use kokjinsam_just::workspace::Workspace;

fn workflow() -> Command {
    Command::new(env!("CARGO_BIN_EXE_workflow"))
}

#[cfg(unix)]
fn executable(path: &Path, source: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn repository() -> tempfile::TempDir {
    let repository = tempfile::tempdir().unwrap();
    for directory in [
        "apps/api/assets/src/generated",
        "apps/api/lib",
        "apps/api/native/src",
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
        ),
    )
    .unwrap();
    fs::write(repository.path().join("apps/api/mix.exs"), "Mix.Project").unwrap();
    for config in [".credo.exs", ".sobelow-conf", ".ex_dna.exs", ".reach.exs"] {
        fs::write(repository.path().join("apps/api").join(config), "config").unwrap();
    }
    fs::write(
        repository.path().join("apps/api/lib/app.ex"),
        "defmodule App do end",
    )
    .unwrap();
    fs::write(
        repository.path().join("apps/api/assets/package.json"),
        r#"{"devDependencies":{"oxlint":"1","stylelint":"1"}}"#,
    )
    .unwrap();
    fs::write(
        repository.path().join("apps/api/assets/oxlint.config.ts"),
        "export default {}",
    )
    .unwrap();
    fs::write(
        repository
            .path()
            .join("apps/api/assets/stylelint.config.mjs"),
        "export default {}",
    )
    .unwrap();
    for source in [
        "app.js", "app.jsx", "app.mjs", "app.cjs", "app.ts", "app.tsx", "app.mts", "app.cts",
        "app.css", "app.scss",
    ] {
        fs::write(
            repository.path().join("apps/api/assets/src").join(source),
            "source",
        )
        .unwrap();
    }
    fs::write(
        repository.path().join("apps/api/assets/src/other.py"),
        "pass",
    )
    .unwrap();
    fs::write(
        repository.path().join("apps/api/native/Cargo.toml"),
        "[package]\nname='native'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(
        repository.path().join("apps/api/native/dylint.toml"),
        "[example_lint]\nlimit=10\n",
    )
    .unwrap();
    fs::write(
        repository.path().join("apps/api/native/src/lib.rs"),
        "pub fn value() {}",
    )
    .unwrap();
    fs::write(
        repository.path().join(".gitignore"),
        "apps/api/assets/src/generated/ignored.ts\n",
    )
    .unwrap();
    fs::write(
        repository
            .path()
            .join("apps/api/assets/src/generated/ignored.ts"),
        "ignored",
    )
    .unwrap();
    repository
}

#[cfg(unix)]
fn tools() -> tempfile::TempDir {
    let tools = tempfile::tempdir().unwrap();
    let script = concat!(
        "#!/bin/sh\n",
        "printf '%s\\t%s\\t%s\\n' \"${0##*/}\" \"$PWD\" \"$*\" >> \"$LINT_LOG\"\n",
        "printf '%s-out\\n' \"${0##*/}\"\n",
        "printf '%s-err\\n' \"${0##*/}\" >&2\n",
        "provider=\"${1:-}\"\n",
        "if [ \"${SIGNAL_PROVIDER:-}\" = \"$provider\" ]; then kill -INT $$; fi\n",
        "if [ \"${DELAY_PROVIDER:-}\" = \"$provider\" ]; then /bin/sleep 30; fi\n",
        "case \"$provider\" in\n",
        "  credo) exit \"${CREDO_STATUS:-0}\" ;;\n",
        "  sobelow) exit \"${SOBELOW_STATUS:-0}\" ;;\n",
        "  ex_dna) exit \"${EX_DNA_STATUS:-0}\" ;;\n",
        "  reach.check) exit \"${REACH_STATUS:-0}\" ;;\n",
        "  exec) exit \"${PNPM_STATUS:-0}\" ;;\n",
        "  clippy) exit \"${CLIPPY_STATUS:-0}\" ;;\n",
        "  dylint) exit \"${DYLINT_STATUS:-0}\" ;;\n",
        "esac\n",
    );
    for name in [
        "mix",
        "pnpm",
        "cargo",
        "cargo-clippy",
        "cargo-dylint",
        "dylint-link",
        "java",
    ] {
        executable(&tools.path().join(name), script);
    }
    tools
}

#[cfg(unix)]
fn run(repository: &Path, tools: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut command = workflow();
    command
        .args(args)
        .current_dir(repository)
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools)
        .env("LINT_LOG", repository.join("lint.log"));
    for (name, value) in envs {
        command.env(name, value);
    }
    command.output().unwrap()
}

#[cfg(unix)]
fn log_lines(repository: &Path) -> Vec<String> {
    fs::read_to_string(repository.join("lint.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn deterministic_bindings_commands_cwds_targets_and_order_are_frozen() {
    let repository = repository();
    fs::write(
        repository.path().join("apps/api/native/Cargo.toml"),
        concat!(
            "[package]\nname='native'\nversion='0.1.0'\n",
            "[workspace]\n",
            "[workspace.metadata.dylint]\n",
            "libraries=[{ path='lints' }]\n",
        ),
    )
    .unwrap();
    let workspace = Workspace::discover(repository.path()).unwrap();
    let selections = resolve_selectors(&workspace, &[]).unwrap();
    let plan = build_plan(&workspace, &selections).unwrap();
    let commands = plan
        .steps()
        .iter()
        .map(|step| {
            (
                step.provider.as_str().to_owned(),
                step.cwd
                    .strip_prefix(&workspace.root)
                    .unwrap()
                    .to_path_buf(),
                std::iter::once(step.program.to_string_lossy().into_owned())
                    .chain(
                        step.argv
                            .iter()
                            .map(|argument| argument.to_string_lossy().into_owned()),
                    )
                    .collect::<Vec<_>>(),
                step.targets.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        commands
            .iter()
            .map(|(provider, _, argv, _)| (provider.as_str(), argv.join(" ")))
            .collect::<Vec<_>>(),
        vec![
            ("credo", "mix credo --strict".into()),
            (
                "sobelow",
                "mix sobelow --private --exit --threshold high".into()
            ),
            ("ex-dna", "mix ex_dna --min-mass 40 --max-clones 0".into()),
            ("reach", "mix reach.check --arch --smells --strict".into()),
            (
                "oxlint",
                concat!(
                    "pnpm exec oxlint src/app.cjs src/app.cts src/app.js src/app.jsx ",
                    "src/app.mjs src/app.mts src/app.ts src/app.tsx",
                )
                .into(),
            ),
            (
                "stylelint",
                "pnpm exec stylelint src/app.css src/app.scss".into(),
            ),
            (
                "clippy",
                "cargo clippy --all-targets --all-features -- -D warnings".into()
            ),
            ("dylint", "cargo dylint --all -- --all-targets".into()),
        ]
    );
    assert!(
        commands[..4]
            .iter()
            .all(|(_, _, _, targets)| targets.is_empty())
    );
    assert_eq!(commands[4].3.len(), 8);
    assert_eq!(commands[5].3.len(), 2);
    assert!(
        commands[6..]
            .iter()
            .all(|(_, _, _, targets)| targets.is_empty())
    );
    assert_eq!(commands[0].1, PathBuf::from("apps/api"));
    assert_eq!(commands[4].1, PathBuf::from("apps/api/assets"));
    assert_eq!(commands[6].1, PathBuf::from("apps/api/native"));
    let dylint = plan
        .steps()
        .iter()
        .find(|step| step.provider.as_str() == "dylint")
        .unwrap();
    assert_eq!(
        dylint.configuration_files,
        vec![
            workspace.root.join("apps/api/native/Cargo.toml"),
            workspace.root.join("apps/api/native/dylint.toml"),
        ]
    );
}

#[test]
fn oxlint_and_dylint_configuration_evidence_is_complete_and_deduplicated() {
    let repository = repository();
    let node_root = repository.path().join("apps/api/assets");
    for extension in ["js", "cjs", "mjs"] {
        fs::write(
            node_root.join(format!("oxlint.config.{extension}")),
            "export default {}",
        )
        .unwrap();
    }
    fs::write(
        repository.path().join("apps/api/native/dylint.toml"),
        "[workspace.metadata.dylint]\nlibraries=[{path='lints'}]\n",
    )
    .unwrap();
    let workspace = Workspace::discover(repository.path()).unwrap();
    let bindings =
        bindings_for_selections(&workspace, &resolve_selectors(&workspace, &[]).unwrap()).unwrap();
    let oxlint = bindings
        .iter()
        .find(|binding| {
            binding.capability == Capability::Lint && binding.provider.as_str() == "oxlint"
        })
        .unwrap();
    assert_eq!(
        oxlint
            .configuration_files
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec![
            "oxlint.config.cjs",
            "oxlint.config.js",
            "oxlint.config.mjs",
            "oxlint.config.ts",
        ]
    );
    let discovered = discover_resources(&workspace, &resolve_selectors(&workspace, &[]).unwrap())
        .unwrap()
        .into_iter()
        .filter(|resource| resource.kind == ResourceKind::Configuration)
        .filter_map(|resource| {
            resource
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .filter(|name| name.starts_with("oxlint.config."))
        .collect::<Vec<_>>();
    assert_eq!(
        discovered,
        vec![
            "oxlint.config.cjs",
            "oxlint.config.js",
            "oxlint.config.mjs",
            "oxlint.config.ts",
        ]
    );
    let dylint = bindings
        .iter()
        .find(|binding| binding.provider.as_str() == "dylint")
        .unwrap();
    assert_eq!(
        dylint.configuration_files,
        vec![workspace.root.join("apps/api/native/dylint.toml")]
    );
}

#[test]
fn leading_dash_targets_are_passed_as_paths_not_provider_options() {
    let repository = repository();
    fs::write(
        repository.path().join("apps/api/assets/-generated.ts"),
        "source",
    )
    .unwrap();
    let workspace = Workspace::discover(repository.path()).unwrap();
    let selections =
        resolve_selectors(&workspace, &["apps/api/assets/-generated.ts".into()]).unwrap();
    let plan = build_plan(&workspace, &selections).unwrap();
    assert_eq!(
        plan.steps()[0].argv.last().unwrap().to_string_lossy(),
        "./-generated.ts"
    );
}

#[test]
fn extension_routing_scopes_deduplication_and_empty_no_ops_are_focused() {
    let repository = repository();
    let workspace = Workspace::discover(repository.path()).unwrap();
    let selections = resolve_selectors(&workspace, &["apps/api/assets/src".into()]).unwrap();
    let plan = build_plan(&workspace, &selections).unwrap();
    let oxlint = plan
        .steps()
        .iter()
        .find(|step| step.provider.as_str() == "oxlint")
        .unwrap();
    let stylelint = plan
        .steps()
        .iter()
        .find(|step| step.provider.as_str() == "stylelint")
        .unwrap();
    assert_eq!(oxlint.targets.len(), 8);
    assert!(oxlint.targets.iter().all(|path| {
        matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts")
        )
    }));
    assert_eq!(
        stylelint.targets,
        vec![PathBuf::from("src/app.css"), PathBuf::from("src/app.scss")]
    );

    let repeated = resolve_selectors(
        &workspace,
        &[
            "apps/api/assets/src/app.ts".into(),
            "apps/api/assets/src/app.ts".into(),
            "apps/api/assets/src".into(),
        ],
    )
    .unwrap();
    let repeated_plan = build_plan(&workspace, &repeated).unwrap();
    assert_eq!(
        repeated_plan
            .steps()
            .iter()
            .filter(|step| step.provider.as_str() == "oxlint")
            .count(),
        1
    );

    for selector in [
        "apps/api/assets/src/other.py",
        "apps/api/assets/src/generated",
    ] {
        let selections = resolve_selectors(&workspace, &[selector.into()]).unwrap();
        let plan = build_plan(&workspace, &selections).unwrap();
        assert!(plan.steps().is_empty(), "{selector}: {:?}", plan.steps());
    }
    let ignored = resolve_selectors(
        &workspace,
        &["apps/api/assets/src/generated/ignored.ts".into()],
    )
    .unwrap();
    let ignored_plan = build_plan(&workspace, &ignored).unwrap();
    assert_eq!(ignored_plan.steps().len(), 1);
    assert_eq!(
        ignored_plan.steps()[0].targets,
        vec![PathBuf::from("src/generated/ignored.ts")]
    );
}

#[test]
fn node_lint_ownership_uses_only_selection_admitted_manifests() {
    let repository = repository();
    let node_root = repository.path().join("apps/api/assets");
    let ignored_package = node_root.join("ignored-boundary/package.json");
    let visible_package = node_root.join("visible-boundary/package.json");
    for manifest in [&ignored_package, &visible_package] {
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    }
    fs::write(
        &ignored_package,
        r#"{"devDependencies":{"oxlint":"1","stylelint":"1"}}"#,
    )
    .unwrap();
    fs::write(
        ignored_package
            .parent()
            .unwrap()
            .join("stylelint.config.js"),
        "module.exports={}",
    )
    .unwrap();
    fs::write(&visible_package, "{}").unwrap();
    for source in [
        "ignored-boundary/app.ts",
        "ignored-boundary/app.css",
        "visible-boundary/app.ts",
        "visible-boundary/app.css",
    ] {
        fs::write(node_root.join(source), "source").unwrap();
    }
    fs::OpenOptions::new()
        .append(true)
        .open(repository.path().join(".gitignore"))
        .unwrap()
        .write_all(b"apps/api/assets/ignored-boundary/package.json\n")
        .unwrap();

    let workspace = Workspace::discover(repository.path()).unwrap();
    let repository_plan =
        build_plan(&workspace, &resolve_selectors(&workspace, &[]).unwrap()).unwrap();
    let oxlint = repository_plan
        .steps()
        .iter()
        .find(|step| step.provider.as_str() == "oxlint")
        .unwrap();
    let stylelint = repository_plan
        .steps()
        .iter()
        .find(|step| step.provider.as_str() == "stylelint")
        .unwrap();
    assert!(
        oxlint
            .targets
            .contains(&PathBuf::from("ignored-boundary/app.ts"))
    );
    assert!(
        !oxlint
            .targets
            .contains(&PathBuf::from("visible-boundary/app.ts"))
    );
    assert!(
        stylelint
            .targets
            .contains(&PathBuf::from("ignored-boundary/app.css"))
    );
    assert!(
        !stylelint
            .targets
            .contains(&PathBuf::from("visible-boundary/app.css"))
    );

    let explicit = resolve_selectors(
        &workspace,
        &["apps/api/assets/ignored-boundary/package.json".into()],
    )
    .unwrap();
    let explicit_plan = build_plan(&workspace, &explicit).unwrap();
    assert_eq!(
        explicit_plan
            .steps()
            .iter()
            .map(|step| {
                (
                    step.provider.as_str(),
                    step.cwd.strip_prefix(&workspace.root).unwrap(),
                    step.targets.clone(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                "oxlint",
                Path::new("apps/api/assets/ignored-boundary"),
                vec![PathBuf::from("app.ts")],
            ),
            (
                "stylelint",
                Path::new("apps/api/assets/ignored-boundary"),
                vec![PathBuf::from("app.css")],
            ),
        ]
    );
}

#[test]
fn mix_and_cargo_files_select_complete_owning_roots() {
    let repository = repository();
    let workspace = Workspace::discover(repository.path()).unwrap();
    for (selector, providers) in [
        (
            "apps/api/lib/app.ex",
            vec!["credo", "sobelow", "ex-dna", "reach"],
        ),
        ("apps/api/native/src/lib.rs", vec!["clippy"]),
    ] {
        let selections = resolve_selectors(&workspace, &[selector.into()]).unwrap();
        let plan = build_plan(&workspace, &selections).unwrap();
        assert_eq!(
            plan.steps()
                .iter()
                .map(|step| step.provider.as_str())
                .collect::<Vec<_>>(),
            providers
        );
        assert!(plan.steps().iter().all(|step| step.targets.is_empty()));
    }
}

#[test]
fn repository_target_component_directory_file_and_overlapping_scopes_are_consistent() {
    let repository = repository();
    let workspace = Workspace::discover(repository.path()).unwrap();
    let cases = [
        (
            Vec::<String>::new(),
            vec![
                "credo",
                "sobelow",
                "ex-dna",
                "reach",
                "oxlint",
                "stylelint",
                "clippy",
            ],
        ),
        (
            vec!["@api".into()],
            vec![
                "credo",
                "sobelow",
                "ex-dna",
                "reach",
                "oxlint",
                "stylelint",
                "clippy",
            ],
        ),
        (
            vec!["@api:backend".into()],
            vec!["credo", "sobelow", "ex-dna", "reach"],
        ),
        (vec!["@api:web".into()], vec!["oxlint", "stylelint"]),
        (
            vec!["apps/api/assets/src".into()],
            vec!["oxlint", "stylelint"],
        ),
        (vec!["apps/api/assets/src/app.ts".into()], vec!["oxlint"]),
        (
            vec!["apps/api/assets/src/app.css".into()],
            vec!["stylelint"],
        ),
        (
            vec![
                "apps/api/assets/src/app.ts".into(),
                "apps/api/assets/src".into(),
                "@api:web".into(),
            ],
            vec!["oxlint", "stylelint"],
        ),
    ];
    for (selectors, expected) in cases {
        let selections = resolve_selectors(&workspace, &selectors).unwrap();
        let plan = build_plan(&workspace, &selections).unwrap();
        assert_eq!(
            plan.steps()
                .iter()
                .map(|step| step.provider.as_str())
                .collect::<Vec<_>>(),
            expected,
            "{selectors:?}"
        );
    }
}

#[test]
fn dylint_absent_valid_cargo_valid_file_malformed_empty_and_incomplete_are_distinct() {
    let repository = repository();
    let manifest = repository.path().join("apps/api/native/Cargo.toml");
    let workspace = Workspace::discover(repository.path()).unwrap();
    let selections = resolve_selectors(&workspace, &["@api:native".into()]).unwrap();
    assert_eq!(
        build_plan(&workspace, &selections).unwrap().steps().len(),
        1
    );

    fs::write(
        &manifest,
        "[workspace]\n[workspace.metadata.dylint]\nlibraries=[{git='https://example.test/lints', pattern=['one','two']}]\n",
    )
    .unwrap();
    let workspace = Workspace::discover(repository.path()).unwrap();
    let plan = build_plan(&workspace, &resolve_selectors(&workspace, &[]).unwrap()).unwrap();
    assert!(
        plan.steps()
            .iter()
            .any(|step| step.provider.as_str() == "dylint")
    );

    fs::write(&manifest, "[package]\nname='native'\nversion='0.1.0'\n").unwrap();
    fs::write(
        repository.path().join("apps/api/native/dylint.toml"),
        "[workspace.metadata.dylint]\nlibraries=[{path='lints'}]\n",
    )
    .unwrap();
    let workspace = Workspace::discover(repository.path()).unwrap();
    let plan = build_plan(&workspace, &resolve_selectors(&workspace, &[]).unwrap()).unwrap();
    assert!(
        plan.steps()
            .iter()
            .any(|step| step.provider.as_str() == "dylint")
    );

    for (source, diagnostic) in [
        ("[workspace.metadata.dylint\n", "cannot parse Dylint"),
        ("[workspace.metadata.dylint]\nlibraries=[]\n", "declaration"),
        (
            "[workspace.metadata.dylint]\nlibraries=[{}]\n",
            "incomplete",
        ),
        (
            "[workspace.metadata.dylint]\nlibraries=[{git='https://example.test/lints', branch=''}]\n",
            "malformed",
        ),
    ] {
        fs::write(
            repository.path().join("apps/api/native/dylint.toml"),
            source,
        )
        .unwrap();
        let workspace = Workspace::discover(repository.path()).unwrap();
        let error = build_plan(&workspace, &resolve_selectors(&workspace, &[]).unwrap())
            .unwrap_err()
            .to_string();
        assert!(error.contains(diagnostic), "{source}: {error}");
    }
}

#[test]
fn specification_placement_uses_configured_specs_and_recognizes_all_frozen_formats() {
    let repository = repository();
    for file in ["Model.tla", "Model.cfg", "relations.als"] {
        fs::write(repository.path().join("contracts").join(file), "spec").unwrap();
    }
    fs::create_dir(repository.path().join("apps/api/spec")).unwrap();
    for file in ["Bad.tla", "Bad.cfg", "bad.als"] {
        fs::write(repository.path().join("apps/api/spec").join(file), "spec").unwrap();
    }
    fs::write(
        repository.path().join("apps/api/spec/openapi.yaml"),
        "openapi: 3.1.0\ninfo: {}\n",
    )
    .unwrap();
    fs::write(
        repository.path().join("apps/api/spec/swagger.json"),
        r#"{"swagger":"2.0","info":{}}"#,
    )
    .unwrap();
    fs::write(
        repository.path().join("apps/api/spec/ordinary.yaml"),
        "name: ordinary\n",
    )
    .unwrap();
    fs::write(
        repository.path().join(".gitignore"),
        concat!(
            "apps/api/assets/src/generated/ignored.ts\n",
            "apps/api/spec/ignored.tla\n",
        ),
    )
    .unwrap();
    fs::write(
        repository.path().join("apps/api/spec/ignored.tla"),
        "ignored spec",
    )
    .unwrap();

    let workspace = Workspace::discover(repository.path()).unwrap();
    let plan = build_plan(&workspace, &resolve_selectors(&workspace, &[]).unwrap()).unwrap();
    let policy = plan
        .steps()
        .iter()
        .find(|step| step.provider.as_str() == "specification-placement")
        .unwrap();
    assert_eq!(policy.targets.len(), 5);
    assert!(
        policy
            .targets
            .iter()
            .all(|path| path.starts_with(workspace.root.join("apps/api/spec")))
    );
    assert!(
        policy
            .targets
            .iter()
            .all(|path| !path.ends_with("ordinary.yaml"))
    );

    let component = resolve_selectors(&workspace, &["@api:web".into()]).unwrap();
    let component_plan = build_plan(&workspace, &component).unwrap();
    assert!(
        component_plan
            .steps()
            .iter()
            .all(|step| step.provider.as_str() != "specification-placement")
    );

    let explicit = resolve_selectors(&workspace, &["apps/api/spec/ignored.tla".into()]).unwrap();
    let explicit_plan = build_plan(&workspace, &explicit).unwrap();
    let policy = explicit_plan
        .steps()
        .iter()
        .find(|step| step.provider.as_str() == "specification-placement")
        .unwrap();
    assert_eq!(policy.targets.len(), 1);
}

#[cfg(unix)]
#[test]
fn complete_preflight_and_independent_failures_obey_exit_contracts() {
    let repository = repository();
    let tools = tools();
    let output = run(
        repository.path(),
        tools.path(),
        &["lint", "@api", "--output", "json"],
        &[("CREDO_STATUS", "7"), ("CLIPPY_STATUS", "9")],
    );
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "failed");
    assert_eq!(log_lines(repository.path()).len(), 7);
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("mix-err")
    );

    fs::remove_file(repository.path().join("lint.log")).unwrap();
    fs::remove_file(tools.path().join("cargo-clippy")).unwrap();
    let clippy_preflight = run(
        repository.path(),
        tools.path(),
        &["lint", "@api", "--output", "json"],
        &[],
    );
    assert_eq!(clippy_preflight.status.code(), Some(2));
    assert!(!repository.path().join("lint.log").exists());
    assert!(
        String::from_utf8(clippy_preflight.stderr)
            .unwrap()
            .contains("cargo-clippy")
    );
    executable(&tools.path().join("cargo-clippy"), "#!/bin/sh\nexit 0\n");

    fs::write(
        repository.path().join("apps/api/native/Cargo.toml"),
        "[workspace]\n[workspace.metadata.dylint]\nlibraries=[{path='lints'}]\n",
    )
    .unwrap();
    fs::remove_file(tools.path().join("dylint-link")).unwrap();
    let preflight = run(
        repository.path(),
        tools.path(),
        &["lint", "--output", "json"],
        &[],
    );
    assert_eq!(preflight.status.code(), Some(2));
    assert!(!repository.path().join("lint.log").exists());
    assert!(
        String::from_utf8(preflight.stderr)
            .unwrap()
            .contains("dylint-link")
    );
}

#[cfg(unix)]
#[test]
fn child_signal_two_stops_but_voluntary_130_aggregates() {
    let repository = repository();
    let tools = tools();
    let signalled = run(
        repository.path(),
        tools.path(),
        &["lint", "@api"],
        &[("SIGNAL_PROVIDER", "credo")],
    );
    assert_eq!(signalled.status.code(), Some(130));
    assert_eq!(log_lines(repository.path()).len(), 1);

    fs::remove_file(repository.path().join("lint.log")).unwrap();
    let voluntary = run(
        repository.path(),
        tools.path(),
        &["lint", "@api"],
        &[("CREDO_STATUS", "130")],
    );
    assert_eq!(voluntary.status.code(), Some(1));
    assert_eq!(log_lines(repository.path()).len(), 7);
}

#[cfg(unix)]
#[test]
fn process_group_sigint_stops_the_plan_with_130() {
    use std::os::unix::process::CommandExt;
    use std::time::{Duration, Instant};

    let repository = repository();
    let tools = tools();
    let mut child = workflow()
        .args(["lint", "@api"])
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .env("PATH", tools.path())
        .env("LINT_LOG", repository.path().join("lint.log"))
        .env("DELAY_PROVIDER", "credo")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !repository.path().join("lint.log").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = Command::new("/bin/kill")
        .args(["-INT", &format!("-{}", child.id())])
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(child.wait().unwrap().code(), Some(130));
    assert_eq!(log_lines(repository.path()).len(), 1);
}

#[cfg(unix)]
#[test]
fn reports_are_human_or_json_and_forbidden_tools_never_run() {
    let repository = repository();
    let tools = tools();
    let human = run(
        repository.path(),
        tools.path(),
        &["lint", "apps/api/assets/src"],
        &[],
    );
    assert_eq!(human.status.code(), Some(0));
    assert!(human.stdout.is_empty());
    let stderr = String::from_utf8(human.stderr).unwrap();
    assert!(stderr.contains("lint · oxlint"));
    assert!(stderr.contains("lint · stylelint"));
    assert!(stderr.contains("2 steps · passed"));
    let lines = log_lines(repository.path());
    assert!(lines.iter().all(|line| {
        !line.contains("format")
            && !line.contains("ruff")
            && !line.contains("vacuum")
            && !line.contains("openapi")
    }));

    fs::remove_file(repository.path().join("lint.log")).unwrap();
    let json = run(
        repository.path(),
        tools.path(),
        &["lint", "apps/api/assets/src/app.ts", "--output", "json"],
        &[],
    );
    assert_eq!(json.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(report["command"], "lint");
    assert_eq!(report["steps"][0]["provider"], "oxlint");
    assert!(String::from_utf8(json.stderr).unwrap().contains("pnpm-err"));
}
