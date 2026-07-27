use std::fs;
use std::path::Path;
use std::process::Command;

use kokjinsam_just::config::WorkspaceConfig;

fn fixture(name: &str) -> &'static Path {
    match name {
        "valid-minimal" => Path::new("tests/fixtures/valid-minimal"),
        "valid-components" => Path::new("tests/fixtures/valid-components"),
        "invalid-version" => Path::new("tests/fixtures/invalid-version"),
        "invalid-path" => Path::new("tests/fixtures/invalid-path"),
        "invalid-coverage" => Path::new("tests/fixtures/invalid-coverage"),
        _ => unreachable!("unknown fixture"),
    }
}

#[test]
fn minimal_configuration_uses_defaults_and_an_implicit_component() {
    let config = WorkspaceConfig::load(fixture("valid-minimal")).unwrap();
    let root = fixture("valid-minimal").canonicalize().unwrap();

    assert_eq!(config.system.specs, root.join("specs"));
    assert_eq!(config.system.tests, root.join("test"));
    assert_eq!(config.targets.len(), 1);

    let target = &config.targets["website"];
    assert_eq!(target.root, root.join("apps/website"));
    assert_eq!(target.components.len(), 1);
    assert_eq!(target.components[0].name, None);
    assert!(target.components[0].implicit);
    assert_eq!(target.components[0].coverage_lines, Some(80));
}

#[test]
fn explicit_components_can_be_nested_and_override_coverage() {
    let config = WorkspaceConfig::load(fixture("valid-components")).unwrap();
    let root = fixture("valid-components").canonicalize().unwrap();
    let target = &config.targets["api"];

    assert_eq!(target.components.len(), 3);
    assert_eq!(target.components[0].name.as_deref(), Some("backend"));
    assert_eq!(target.components[0].coverage_lines, Some(75));
    assert_eq!(target.components[1].name.as_deref(), Some("native"));
    assert_eq!(target.components[1].coverage_lines, Some(90));
    assert_eq!(target.components[2].name.as_deref(), Some("web"));
    assert_eq!(target.components[2].root, root.join("apps/api/assets"));
}

#[test]
fn invalid_configuration_is_rejected_at_the_public_loader() {
    for (name, expected) in [
        ("invalid-version", "version 1"),
        ("invalid-path", "cannot escape"),
        ("invalid-coverage", "0..=100"),
    ] {
        let error = WorkspaceConfig::load(fixture(name)).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "{name}: expected {expected:?}, got {error}"
        );
    }
}

fn synthetic_repository(yaml: &str, directories: &[&str]) -> tempfile::TempDir {
    let repository = tempfile::tempdir().unwrap();
    fs::write(repository.path().join("just.yaml"), yaml).unwrap();
    for directory in directories {
        fs::create_dir_all(repository.path().join(directory)).unwrap();
    }
    repository
}

fn load_error(yaml: &str, directories: &[&str]) -> String {
    let repository = synthetic_repository(yaml, directories);
    WorkspaceConfig::load(repository.path())
        .unwrap_err()
        .to_string()
}

#[test]
fn targets_must_be_nonempty_and_identifiers_unambiguous() {
    let empty = load_error("version: 1\ntargets: {}\n", &[]);
    assert!(empty.contains("at least one target"));

    let ambiguous = load_error(
        "version: 1\ntargets:\n  'api:web':\n    path: apps/api\n",
        &["apps/api", "specs", "test"],
    );
    assert!(ambiguous.contains("identifier 'api:web' is ambiguous"));
}

#[test]
fn configured_paths_must_be_relative_nonempty_existing_directories() {
    let empty = load_error(
        "version: 1\ntargets:\n  api:\n    path: ''\n",
        &["specs", "test"],
    );
    assert!(empty.contains("must not be empty"));

    let absolute = load_error(
        "version: 1\ntargets:\n  api:\n    path: /tmp\n",
        &["specs", "test"],
    );
    assert!(absolute.contains("must be relative"));

    let missing = load_error(
        "version: 1\ntargets:\n  api:\n    path: apps/api\n",
        &["apps/api"],
    );
    assert!(missing.contains("system.specs must name an existing directory"));
}

#[test]
fn component_roots_are_unique_across_the_workspace() {
    let error = load_error(
        concat!(
            "version: 1\n",
            "targets:\n",
            "  first:\n",
            "    path: apps/shared\n",
            "  second:\n",
            "    path: apps/shared\n",
        ),
        &["apps/shared", "specs", "test"],
    );

    assert!(error.contains("same component root"), "{error}");
}

#[test]
fn duplicate_explicit_component_roots_are_rejected() {
    let error = load_error(
        concat!(
            "version: 1\n",
            "targets:\n",
            "  api:\n",
            "    path: apps/api\n",
            "    components:\n",
            "      backend:\n",
            "        path: .\n",
            "      duplicate:\n",
            "        path: ./\n",
        ),
        &["apps/api", "specs", "test"],
    );

    assert!(error.contains("have the same root"), "{error}");
}

#[test]
fn exact_system_and_target_owner_root_collisions_are_rejected() {
    let cases = [
        (
            concat!(
                "version: 1\n",
                "system:\n  specs: shared\n  tests: shared\n",
                "targets:\n  api:\n    path: apps/api\n",
            ),
            vec!["apps/api", "shared"],
            ["system.specs", "system.tests"],
        ),
        (
            concat!(
                "version: 1\n",
                "system:\n  specs: specs\n  tests: apps/api\n",
                "targets:\n  api:\n    path: apps/api\n",
            ),
            vec!["apps/api", "specs"],
            ["system.tests", "target '@api'"],
        ),
        (
            concat!(
                "version: 1\n",
                "system:\n  specs: apps/api/web\n  tests: test\n",
                "targets:\n",
                "  api:\n",
                "    path: apps/api\n",
                "    components:\n",
                "      backend:\n        path: backend\n",
                "      web:\n        path: web\n",
            ),
            vec!["apps/api/backend", "apps/api/web", "test"],
            ["system.specs", "component '@api:web'"],
        ),
    ];

    for (yaml, directories, owners) in cases {
        let repository = synthetic_repository(yaml, &directories);
        let root = repository.path().canonicalize().unwrap();
        let error = WorkspaceConfig::load(repository.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains(owners[0]), "{error}");
        assert!(error.contains(owners[1]), "{error}");
        assert!(error.contains(root.to_string_lossy().as_ref()), "{error}");
    }
}

#[test]
fn ancestor_relationships_and_legitimate_nested_components_are_allowed() {
    let repository = synthetic_repository(
        concat!(
            "version: 1\n",
            "system:\n  specs: apps\n  tests: test\n",
            "targets:\n",
            "  api:\n",
            "    path: apps/api\n",
            "    components:\n",
            "      backend:\n        path: .\n",
            "      web:\n        path: web\n",
        ),
        &["apps/api/web", "test"],
    );

    let config = WorkspaceConfig::load(repository.path()).unwrap();

    assert_eq!(config.targets["api"].components.len(), 2);
}

#[test]
fn owner_root_collisions_fail_before_doctor_with_exit_two() {
    let repository = synthetic_repository(
        concat!(
            "version: 1\n",
            "system:\n  specs: shared\n  tests: shared\n",
            "targets:\n  api:\n    path: apps/api\n",
        ),
        &["apps/api", "shared"],
    );

    let output = Command::new(env!("CARGO_BIN_EXE_workflow"))
        .arg("doctor")
        .current_dir(repository.path())
        .env_remove("JUST_CALLER_DIR")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.contains("system.specs"), "{diagnostic}");
    assert!(diagnostic.contains("system.tests"), "{diagnostic}");
    assert!(!diagnostic.contains("doctor-check"), "{diagnostic}");
}

#[test]
fn absent_coverage_remains_optional() {
    let repository = synthetic_repository(
        "version: 1\ntargets:\n  api:\n    path: apps/api\n",
        &["apps/api", "specs", "test"],
    );

    let config = WorkspaceConfig::load(repository.path()).unwrap();

    assert_eq!(config.coverage_lines, None);
    assert_eq!(config.targets["api"].components[0].coverage_lines, None);
}
