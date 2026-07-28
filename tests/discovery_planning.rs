use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use kokjinsam_just::capability::{Capability, ProviderId, bindings_for_selections};
use kokjinsam_just::discovery::{ResourceKind, discover_resources};
use kokjinsam_just::ownership::{Owner, Ownership, SystemResource};
use kokjinsam_just::plan::{Plan, Step, StepId};
use kokjinsam_just::selector::{Selection, resolve_selectors};
use kokjinsam_just::workspace::Workspace;

fn copy_fixture(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_fixture(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn repository() -> tempfile::TempDir {
    let repository = tempfile::tempdir().unwrap();
    copy_fixture(
        Path::new("tests/fixtures/phoenix-polyglot"),
        repository.path(),
    );
    repository
}

fn relative(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap().to_path_buf()
}

#[test]
fn nested_components_and_system_resources_have_precise_owners() {
    let repository = repository();
    let workspace = Workspace::discover(repository.path()).unwrap();
    let ownership = Ownership::new(&workspace.config);

    assert_eq!(
        ownership
            .owner_of(&repository.path().join("apps/api/assets/src/app.ts"))
            .unwrap(),
        Owner::Component {
            target: "api".into(),
            component: Some("web".into()),
        }
    );
    assert_eq!(
        ownership
            .owner_of(&repository.path().join("apps/api/lib/fixture.ex"))
            .unwrap(),
        Owner::Component {
            target: "api".into(),
            component: Some("backend".into()),
        }
    );
    assert_eq!(
        ownership
            .owner_of(&repository.path().join("specs/openapi/reviews.yaml"))
            .unwrap(),
        Owner::Repository(SystemResource::Specifications)
    );
    assert_eq!(
        ownership
            .owner_of(
                &repository
                    .path()
                    .join("tests/application/playwright.config.ts")
            )
            .unwrap(),
        Owner::Repository(SystemResource::ApplicationTests)
    );

    let backend = discover_resources(
        &workspace,
        &[Selection::Component {
            target: "api".into(),
            component: "backend".into(),
        }],
    )
    .unwrap();
    assert!(
        backend
            .iter()
            .any(|resource| resource.kind == ResourceKind::MixManifest)
    );
    assert!(!backend.iter().any(|resource| {
        matches!(
            resource.kind,
            ResourceKind::NodeManifest | ResourceKind::CargoManifest
        )
    }));

    let outside = tempfile::tempdir().unwrap();
    assert!(
        ownership
            .owner_of(outside.path())
            .unwrap_err()
            .to_string()
            .contains("outside")
    );
}

#[test]
fn discovery_is_stable_git_aware_and_explicit_files_bypass_ignores() {
    let repository = repository();
    let ignored = repository.path().join("ignored/hidden.ex");
    let nested_ignored = repository.path().join("apps/api/assets/cache/hidden.ts");
    let root_ignored_for_target = repository
        .path()
        .join("apps/api/assets/generated/ignored-package/package.json");
    fs::create_dir_all(ignored.parent().unwrap()).unwrap();
    fs::create_dir_all(nested_ignored.parent().unwrap()).unwrap();
    fs::create_dir_all(root_ignored_for_target.parent().unwrap()).unwrap();
    fs::write(&ignored, "ignored").unwrap();
    fs::write(&nested_ignored, "ignored").unwrap();
    fs::write(&root_ignored_for_target, "{}").unwrap();

    let workspace = Workspace::discover(repository.path()).unwrap();
    let resources = discover_resources(&workspace, &[Selection::Repository]).unwrap();
    let paths = resources
        .iter()
        .map(|resource| relative(&workspace.root, &resource.path))
        .collect::<Vec<_>>();

    assert_eq!(
        resources
            .iter()
            .filter(|resource| {
                matches!(
                    resource.kind,
                    ResourceKind::MixManifest
                        | ResourceKind::NodeManifest
                        | ResourceKind::CargoManifest
                )
            })
            .map(|resource| relative(&workspace.root, &resource.path))
            .collect::<Vec<_>>(),
        vec![
            PathBuf::from("apps/api/assets/package.json"),
            PathBuf::from("apps/api/mix.exs"),
            PathBuf::from("apps/api/native/parser/Cargo.toml"),
        ]
    );
    assert!(!paths.iter().any(|path| path.starts_with("ignored")));
    assert!(
        !paths
            .iter()
            .any(|path| path.starts_with("apps/api/assets/cache"))
    );

    let target_resources =
        discover_resources(&workspace, &[Selection::Target("api".into())]).unwrap();
    assert!(
        !target_resources
            .iter()
            .any(|resource| resource.path == root_ignored_for_target.canonicalize().unwrap())
    );

    let selected = discover_resources(&workspace, &[Selection::Path(ignored.clone())]).unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].path, ignored.canonicalize().unwrap());
}

#[cfg(unix)]
#[test]
fn scoped_discovery_does_not_enter_unrelated_unreadable_directories() {
    use std::os::unix::fs::PermissionsExt;

    let repository = repository();
    let unreadable = repository.path().join("aaa-unreadable");
    fs::create_dir(&unreadable).unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();

    let workspace = Workspace::discover(repository.path()).unwrap();
    let result = discover_resources(&workspace, &[Selection::Target("api".into())]);

    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn phoenix_evidence_produces_deterministic_deduplicated_bindings() {
    let repository = repository();
    let workspace = Workspace::discover(repository.path()).unwrap();
    let selections = resolve_selectors(
        &workspace,
        &[
            "@api".into(),
            "@api:web".into(),
            "apps/api/assets".into(),
            "@api".into(),
        ],
    )
    .unwrap();

    let bindings = bindings_for_selections(&workspace, &selections).unwrap();
    let summary = bindings
        .iter()
        .map(|binding| {
            (
                binding.provider.as_str(),
                binding.capability,
                relative(&workspace.root, &binding.root),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        summary,
        vec![
            (
                "oxfmt",
                Capability::Format,
                PathBuf::from("apps/api/assets")
            ),
            (
                "oxfmt",
                Capability::FormatCheck,
                PathBuf::from("apps/api/assets")
            ),
            ("oxlint", Capability::Lint, PathBuf::from("apps/api/assets")),
            (
                "stylelint",
                Capability::Lint,
                PathBuf::from("apps/api/assets"),
            ),
            (
                "package-build",
                Capability::Compile,
                PathBuf::from("apps/api/assets"),
            ),
            (
                "package-test",
                Capability::TargetTest,
                PathBuf::from("apps/api/assets"),
            ),
            (
                "package-typecheck",
                Capability::Typecheck,
                PathBuf::from("apps/api/assets"),
            ),
            ("mix-format", Capability::Format, PathBuf::from("apps/api")),
            (
                "mix-format",
                Capability::FormatCheck,
                PathBuf::from("apps/api"),
            ),
            ("dialyzer", Capability::Typecheck, PathBuf::from("apps/api"),),
            (
                "mix-compile",
                Capability::Compile,
                PathBuf::from("apps/api")
            ),
            ("exunit", Capability::TargetTest, PathBuf::from("apps/api")),
            (
                "excoveralls",
                Capability::Coverage,
                PathBuf::from("apps/api"),
            ),
            ("credo", Capability::Lint, PathBuf::from("apps/api")),
            (
                "rustfmt",
                Capability::Format,
                PathBuf::from("apps/api/native/parser"),
            ),
            (
                "rustfmt",
                Capability::FormatCheck,
                PathBuf::from("apps/api/native/parser"),
            ),
            (
                "clippy",
                Capability::Lint,
                PathBuf::from("apps/api/native/parser"),
            ),
            (
                "cargo-check",
                Capability::Compile,
                PathBuf::from("apps/api/native/parser"),
            ),
            (
                "nextest",
                Capability::TargetTest,
                PathBuf::from("apps/api/native/parser"),
            ),
            (
                "llvm-cov",
                Capability::Coverage,
                PathBuf::from("apps/api/native/parser"),
            ),
        ]
    );
    let nextest = bindings
        .iter()
        .find(|binding| binding.provider.as_str() == "nextest")
        .unwrap();
    assert_eq!(
        nextest.configuration_files,
        vec![
            workspace
                .root
                .join("apps/api/native/parser/.config/nextest.toml")
        ]
    );

    fs::write(
        repository.path().join("apps/api/mix.exs"),
        "defmodule Fixture.MixProject do\n  use Mix.Project\n  def project, do: [app: :fixture]\n  defp deps, do: [{:excoveralls, \"~> 0.18\", only: :test}]\nend\n",
    )
    .unwrap();
    let invalid_mix_bindings =
        bindings_for_selections(&workspace, &[Selection::Target("api".into())]).unwrap();
    assert!(
        invalid_mix_bindings
            .iter()
            .any(|binding| binding.provider.as_str() == "exunit")
    );
    assert!(
        !invalid_mix_bindings
            .iter()
            .any(|binding| binding.provider.as_str() == "excoveralls")
    );
}

#[test]
fn binding_planning_retains_repository_ignored_and_nearest_manifest_evidence() {
    let repository = repository();
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"root\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let ignored_package = repository.path().join("ignored/tool/package.json");
    let ignored_source = repository.path().join("ignored/application/src/app.ts");
    fs::create_dir_all(ignored_package.parent().unwrap()).unwrap();
    fs::create_dir_all(ignored_source.parent().unwrap()).unwrap();
    fs::write(&ignored_package, r#"{"devDependencies":{"oxfmt":"0.1.0"}}"#).unwrap();
    fs::write(
        ignored_source
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("package.json"),
        r#"{"devDependencies":{"oxlint":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(&ignored_source, "export const ignored = true;").unwrap();

    let nested_mix = repository.path().join("apps/api/tools/child/mix.exs");
    fs::create_dir_all(nested_mix.parent().unwrap()).unwrap();
    fs::write(&nested_mix, "defmodule Child.MixProject do\nend\n").unwrap();
    fs::write(nested_mix.parent().unwrap().join(".reach.exs"), "%{}").unwrap();

    let nextest = repository
        .path()
        .join("apps/api/native/parser/.config/nextest.toml");
    fs::remove_file(nextest).unwrap();
    let unrelated_nextest = repository
        .path()
        .join("apps/api/native/parser/docs/nextest.toml");
    fs::create_dir_all(unrelated_nextest.parent().unwrap()).unwrap();
    fs::write(unrelated_nextest, "[profile.default]\n").unwrap();

    let workspace = Workspace::discover(repository.path()).unwrap();
    let repository_bindings =
        bindings_for_selections(&workspace, &[Selection::Repository]).unwrap();
    assert!(repository_bindings.iter().any(|binding| {
        binding.provider.as_str() == "cargo-check" && binding.root == workspace.root
    }));
    let nextest = repository_bindings
        .iter()
        .find(|binding| {
            binding.provider.as_str() == "nextest"
                && binding.evidence == workspace.root.join("apps/api/native/parser/Cargo.toml")
        })
        .unwrap();
    assert_eq!(nextest.configuration_files, Vec::<PathBuf>::new());

    let ignored_bindings = bindings_for_selections(
        &workspace,
        &[Selection::Path(ignored_package.canonicalize().unwrap())],
    )
    .unwrap();
    assert_eq!(
        ignored_bindings
            .iter()
            .filter(|binding| {
                binding.provider.as_str() == "oxfmt" && binding.capability == Capability::Format
            })
            .map(|binding| relative(&workspace.root, &binding.root))
            .collect::<Vec<_>>(),
        vec![PathBuf::from("ignored/tool")]
    );
    assert!(
        !ignored_bindings
            .iter()
            .any(|binding| binding.provider.as_str() == "cargo-check")
    );

    let ignored_source_bindings = bindings_for_selections(
        &workspace,
        &[Selection::Path(ignored_source.canonicalize().unwrap())],
    )
    .unwrap();
    assert_eq!(
        ignored_source_bindings
            .iter()
            .map(|binding| binding.provider.as_str())
            .collect::<Vec<_>>(),
        vec!["oxlint"]
    );

    let api_directory = bindings_for_selections(
        &workspace,
        &[Selection::Path(
            repository.path().join("apps/api").canonicalize().unwrap(),
        )],
    )
    .unwrap();
    for provider in ["mix-compile", "package-build", "cargo-check"] {
        assert!(
            api_directory
                .iter()
                .any(|binding| binding.provider.as_str() == provider),
            "missing {provider}"
        );
    }
    let source_directory = bindings_for_selections(
        &workspace,
        &[Selection::Path(
            repository
                .path()
                .join("apps/api/assets/src")
                .canonicalize()
                .unwrap(),
        )],
    )
    .unwrap();
    assert!(
        source_directory
            .iter()
            .any(|binding| binding.provider.as_str() == "oxlint")
    );
    assert!(
        !source_directory
            .iter()
            .any(|binding| { matches!(binding.provider.as_str(), "cargo-check" | "mix-compile") })
    );

    assert_eq!(
        repository_bindings
            .iter()
            .filter(|binding| binding.provider.as_str() == "reach")
            .map(|binding| relative(&workspace.root, &binding.root))
            .collect::<Vec<_>>(),
        vec![PathBuf::from("apps/api/tools/child")]
    );
}

#[test]
fn application_test_packages_keep_system_ownership_and_discover_oxfmt() {
    let repository = repository();
    let package = repository.path().join("tests/application/browser");
    fs::create_dir_all(package.join("src/empty")).unwrap();
    fs::write(
        package.join("package.json"),
        r#"{"devDependencies":{"oxfmt":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(package.join("src/spec.mts"), "const value=1").unwrap();
    fs::write(package.join("src/unrelated.py"), "pass\n").unwrap();
    let workspace = Workspace::discover(repository.path()).unwrap();
    let package = package.canonicalize().unwrap();

    for selection in [
        Selection::Repository,
        Selection::Path(package.clone()),
        Selection::Path(package.join("src/spec.mts").canonicalize().unwrap()),
    ] {
        let bindings = bindings_for_selections(&workspace, &[selection]).unwrap();
        let binding = bindings
            .iter()
            .find(|binding| {
                binding.provider.as_str() == "oxfmt"
                    && binding.capability == Capability::Format
                    && binding.root == package
            })
            .unwrap();
        assert_eq!(
            binding.owner,
            Owner::Repository(SystemResource::ApplicationTests)
        );
    }

    let unsupported = bindings_for_selections(
        &workspace,
        &[Selection::Path(
            package.join("src/unrelated.py").canonicalize().unwrap(),
        )],
    )
    .unwrap();
    assert!(unsupported.is_empty());
}

fn step(id: &str, dependencies: &[&str]) -> Step {
    Step {
        id: StepId::new(id),
        label: id.into(),
        capability: Capability::Lint,
        provider: ProviderId::new("synthetic"),
        owner: Owner::Target("api".into()),
        cwd: PathBuf::from("apps/api"),
        program: OsString::from("synthetic"),
        argv: vec![OsString::from("--check")],
        env: BTreeMap::new(),
        targets: vec![PathBuf::from("src")],
        configuration_files: Vec::new(),
        depends_on: dependencies.iter().map(|id| StepId::new(*id)).collect(),
    }
}

#[test]
fn plans_dedupe_identical_steps_and_retain_valid_dependencies() {
    let compile = step("compile", &[]);
    let lint = step("lint", &["compile"]);
    let plan = Plan::new(vec![compile.clone(), lint.clone(), compile.clone()]).unwrap();

    assert_eq!(plan.steps(), &[compile, lint]);
}

#[test]
fn plans_reject_conflicting_or_invalid_dependency_graphs() {
    let mut conflicting = step("lint", &[]);
    conflicting.label = "different".into();
    for (steps, expected) in [
        (vec![step("lint", &[]), conflicting], "conflicting"),
        (vec![step("lint", &["missing"])], "missing"),
        (vec![step("lint", &["lint"])], "itself"),
        (
            vec![step("first", &["second"]), step("second", &["first"])],
            "cyclic",
        ),
    ] {
        let error = Plan::new(steps).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "expected {expected:?}, got {error}"
        );
    }
}
