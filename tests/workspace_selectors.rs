#![cfg(test)]
//! Integration tests for workspace and selector resolution.

use std::ffi::OsStr;
use std::path::Path;

use kokjinsam_just::selector::{Selection, resolve_selectors};
use kokjinsam_just::workspace::{Workspace, invocation_directory};

fn fixture(name: &str) -> &'static Path {
    match name {
        "valid-minimal" => Path::new("tests/fixtures/valid-minimal"),
        "valid-components" => Path::new("tests/fixtures/valid-components"),
        _ => unreachable!("unknown fixture"),
    }
}

#[test]
fn discovers_the_workspace_from_a_nested_invocation_directory() {
    let nested = fixture("valid-components").join("apps/api/assets/src");
    let workspace = Workspace::discover(&nested).unwrap();

    assert_eq!(
        workspace.root,
        fixture("valid-components").canonicalize().unwrap()
    );
    assert_eq!(workspace.invocation, nested.canonicalize().unwrap());
}

#[test]
fn caller_directory_overrides_the_process_directory() {
    let current = fixture("valid-minimal").canonicalize().unwrap();
    let caller = fixture("valid-components")
        .join("apps/api")
        .canonicalize()
        .unwrap();

    assert_eq!(
        invocation_directory(&current, Some(OsStr::new(caller.to_str().unwrap()))).unwrap(),
        caller
    );
}

#[test]
fn target_component_and_path_selectors_resolve_in_stable_union_order() {
    let nested = fixture("valid-components").join("apps/api");
    let workspace = Workspace::discover(&nested).unwrap();
    let selectors = vec![
        "@api:web".to_owned(),
        "assets/src".to_owned(),
        "@api".to_owned(),
        "@api:web".to_owned(),
        "assets/src".to_owned(),
    ];

    let resolved = resolve_selectors(&workspace, &selectors).unwrap();

    assert_eq!(
        resolved,
        vec![
            Selection::Component {
                target: "api".into(),
                component: "web".into(),
            },
            Selection::Path(
                fixture("valid-components")
                    .join("apps/api/assets/src")
                    .canonicalize()
                    .unwrap()
            ),
            Selection::Target("api".into()),
        ]
    );
}

// macOS filesystems reject non-UTF-8 filenames before canonicalization.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn distinct_non_utf8_paths_are_not_collapsed_and_remain_in_order() {
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    for directory in ["app", "specs", "test"] {
        fs::create_dir(root.join(directory)).unwrap();
    }
    fs::write(
        root.join("just.yaml"),
        "version: 1\ntargets:\n  app:\n    path: app\n",
    )
    .unwrap();

    let first = root.join(OsString::from_vec(vec![b'p', b'a', b't', b'h', 0x80]));
    let second = root.join(OsString::from_vec(vec![b'p', b'a', b't', b'h', 0x81]));
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    symlink(&first, root.join("first")).unwrap();
    symlink(&second, root.join("second")).unwrap();

    let workspace = Workspace::discover(root).unwrap();
    let resolved =
        resolve_selectors(&workspace, &["first".to_owned(), "second".to_owned()]).unwrap();

    assert_eq!(
        resolved,
        vec![
            Selection::Path(first.canonicalize().unwrap()),
            Selection::Path(second.canonicalize().unwrap()),
        ]
    );
}

#[test]
fn no_selector_means_repository_scope() {
    let workspace = Workspace::discover(fixture("valid-minimal")).unwrap();

    assert_eq!(
        resolve_selectors(&workspace, &[]).unwrap(),
        vec![Selection::Repository]
    );
}

#[test]
fn invalid_at_selectors_and_missing_paths_fail_without_fallback() {
    let workspace = Workspace::discover(fixture("valid-components")).unwrap();

    for (selector, expected) in [
        ("@unknown", "unknown target"),
        ("@api:mobile", "has no component"),
        ("@", "invalid selector"),
        ("@api:web:extra", "invalid selector"),
        ("api", "path does not exist"),
    ] {
        let error = resolve_selectors(&workspace, &[selector.to_owned()]).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "{selector}: expected {expected:?}, got {error}"
        );
    }
}

#[test]
fn discovery_outside_a_workspace_fails_clearly() {
    let outside = tempfile::tempdir().unwrap();

    let error = Workspace::discover(outside.path()).unwrap_err();

    assert!(error.to_string().contains("could not find 'just.yaml'"));
}
