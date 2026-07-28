use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

use ignore::WalkBuilder;
use serde::Deserialize;

use crate::capability::{
    Capability, ExCoverallsReadiness, ProviderId, excoveralls_readiness, manifests_for_selections,
};
use crate::discovery::{DiscoveredResource, ResourceKind};
use crate::error::WorkflowError;
use crate::formatting::{InterruptGuard, find_program, was_interrupted};
use crate::ownership::{Owner, Ownership};
use crate::plan::{Plan, Step, StepId};
use crate::report::{Report, ReportStatus, StepReport, StepStatus};
use crate::selector::Selection;
use crate::workspace::Workspace;

const NOT_APPLICABLE: &str = "not-applicable";
const TEMPORARY_CONFIG: &str = "<temporary-coveralls-config>";
const EXCOVERALLS_BOOTSTRAP: &str = concat!(
    "Application.put_env(:excoveralls, :config_file, ",
    "System.fetch_env!(\"JUST_EXCOVERALLS_CONFIG\")); ",
    "Mix.Task.run(\"coveralls\", [])"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestSuite {
    Unit,
    Integration,
    Model,
    Property,
}

impl TestSuite {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "unit" => Some(Self::Unit),
            "integration" => Some(Self::Integration),
            "model" => Some(Self::Model),
            "property" => Some(Self::Property),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unit => "unit",
            Self::Integration => "integration",
            Self::Model => "model",
            Self::Property => "property",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestMode {
    Test,
    Coverage,
}

pub fn build_plan(
    workspace: &Workspace,
    selections: &[Selection],
    suite: Option<TestSuite>,
    passthrough: &[OsString],
    mode: TestMode,
) -> Result<Plan, WorkflowError> {
    if mode == TestMode::Coverage && (suite.is_some() || !passthrough.is_empty()) {
        return Err(WorkflowError::Usage(
            "coverage accepts selectors only".into(),
        ));
    }

    let manifests = manifests_for_selections(workspace, selections)?
        .into_iter()
        .filter(|manifest| matches!(manifest.owner, Owner::Component { .. } | Owner::Target(_)))
        .collect::<Vec<_>>();
    let selection_manifests = selection_manifests(workspace, selections)?;
    let mut planned = Vec::new();
    let mut suite_supported = 0_usize;

    for manifest in manifests {
        let Some(provider) = test_provider(&manifest)? else {
            continue;
        };
        let direct_targets =
            direct_targets_for(&manifest, selections, &selection_manifests, provider, mode);
        let supports_suite = suite
            .map(|suite| suite_supported_by(workspace, &manifest, provider, suite, selections))
            .transpose()?
            .unwrap_or(true);
        if !supports_suite {
            planned.push(PlannedStep::Skipped {
                manifest,
                provider,
                suite: suite.expect("unsupported suite requires a suite"),
            });
            continue;
        }
        suite_supported += usize::from(suite.is_some());
        planned.push(PlannedStep::Executable {
            manifest,
            provider,
            direct_targets,
        });
    }

    if let Some(suite) = suite.filter(|_| suite_supported == 0) {
        return Err(WorkflowError::Planning(format!(
            "none of the selected components supports the '{}' target-test suite",
            suite.as_str()
        )));
    }

    planned.sort_by(|left, right| {
        provider_order(mode, left.provider())
            .cmp(&provider_order(mode, right.provider()))
            .then_with(|| {
                owner_key(&left.manifest().owner).cmp(&owner_key(&right.manifest().owner))
            })
            .then_with(|| left.root().cmp(right.root()))
    });

    let mut steps = planned
        .into_iter()
        .enumerate()
        .map(|(index, planned)| step(workspace, planned, suite, mode, index))
        .collect::<Result<Vec<_>, _>>()?;

    let executable_count = steps
        .iter()
        .filter(|step| step.provider.as_str() != NOT_APPLICABLE)
        .count();
    if !passthrough.is_empty() && executable_count != 1 {
        return Err(WorkflowError::Usage(format!(
            "test passthrough requires exactly one executable step; resolved {executable_count}"
        )));
    }
    if let Some(executable) = steps
        .iter_mut()
        .find(|step| step.provider.as_str() != NOT_APPLICABLE)
    {
        executable.argv.extend_from_slice(passthrough);
    }
    Plan::new(steps)
}

enum PlannedStep {
    Executable {
        manifest: DiscoveredResource,
        provider: &'static str,
        direct_targets: Vec<PathBuf>,
    },
    Skipped {
        manifest: DiscoveredResource,
        provider: &'static str,
        suite: TestSuite,
    },
}

impl PlannedStep {
    const fn provider(&self) -> &'static str {
        match self {
            Self::Executable { provider, .. } | Self::Skipped { provider, .. } => provider,
        }
    }

    const fn manifest(&self) -> &DiscoveredResource {
        match self {
            Self::Executable { manifest, .. } | Self::Skipped { manifest, .. } => manifest,
        }
    }

    fn root(&self) -> &Path {
        self.manifest().path.parent().unwrap_or(Path::new(""))
    }
}

fn selection_manifests(
    workspace: &Workspace,
    selections: &[Selection],
) -> Result<HashMap<Selection, Vec<PathBuf>>, WorkflowError> {
    let mut result = HashMap::new();
    for selection in selections {
        let paths = manifests_for_selections(workspace, std::slice::from_ref(selection))?
            .into_iter()
            .map(|resource| resource.path)
            .collect();
        result.insert(selection.clone(), paths);
    }
    Ok(result)
}

fn test_provider(manifest: &DiscoveredResource) -> Result<Option<&'static str>, WorkflowError> {
    match manifest.kind {
        ResourceKind::MixManifest => Ok(Some("exunit")),
        ResourceKind::CargoManifest => Ok(Some("nextest")),
        ResourceKind::NodeManifest => {
            let package = read_package(&manifest.path)?;
            Ok(package
                .scripts
                .contains_key("test")
                .then_some("package-test"))
        }
        _ => Ok(None),
    }
}

fn direct_targets_for(
    manifest: &DiscoveredResource,
    selections: &[Selection],
    selection_manifests: &HashMap<Selection, Vec<PathBuf>>,
    provider: &str,
    mode: TestMode,
) -> Vec<PathBuf> {
    if mode == TestMode::Coverage {
        return Vec::new();
    }
    let relevant = selections.iter().filter(|selection| {
        selection_manifests
            .get(*selection)
            .is_some_and(|paths| paths.contains(&manifest.path))
    });
    let mut targets = Vec::new();
    for selection in relevant {
        let Selection::Path(path) = selection else {
            return Vec::new();
        };
        if path.is_dir()
            || !safe_test_file(
                path,
                manifest.path.parent().unwrap_or(Path::new("")),
                provider,
            )
        {
            return Vec::new();
        }
        targets.push(path.clone());
    }
    targets.sort();
    targets.dedup();
    targets
}

fn safe_test_file(path: &Path, root: &Path, provider: &str) -> bool {
    if !path.starts_with(root) {
        return false;
    }
    match provider {
        "exunit" => {
            path.extension().is_some_and(|extension| extension == "exs")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with("_test.exs"))
        }
        "package-test" => {
            let extension = path.extension().and_then(|extension| extension.to_str());
            let name = path.file_name().and_then(|name| name.to_str());
            matches!(
                extension,
                Some("js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts")
            ) && name.is_some_and(|name| name.contains(".test.") || name.contains(".spec."))
        }
        "nextest" => false,
        _ => false,
    }
}

fn suite_supported_by(
    workspace: &Workspace,
    manifest: &DiscoveredResource,
    provider: &str,
    suite: TestSuite,
    selections: &[Selection],
) -> Result<bool, WorkflowError> {
    match provider {
        "exunit" => exunit_suite_evidence(workspace, manifest, suite, selections),
        "package-test" => Ok(read_package(&manifest.path)?
            .scripts
            .contains_key(&format!("test:{}", suite.as_str()))),
        "nextest" if suite == TestSuite::Property => {
            let source = fs::read_to_string(&manifest.path).map_err(|error| {
                WorkflowError::Discovery(format!(
                    "cannot read Cargo manifest '{}': {error}",
                    manifest.path.display()
                ))
            })?;
            let document: toml::Value = toml::from_str(&source).map_err(|error| {
                WorkflowError::Discovery(format!(
                    "cannot parse Cargo manifest '{}': {error}",
                    manifest.path.display()
                ))
            })?;
            Ok(contains_key(&document, "proptest"))
        }
        "nextest" => Ok(false),
        _ => Ok(false),
    }
}

fn exunit_suite_evidence(
    workspace: &Workspace,
    manifest: &DiscoveredResource,
    suite: TestSuite,
    selections: &[Selection],
) -> Result<bool, WorkflowError> {
    let root = manifest.path.parent().unwrap_or(Path::new(""));
    let ownership = Ownership::new(&workspace.config);
    let mut files = git_visible_files(workspace, root)?;
    files.extend(selections.iter().filter_map(|selection| {
        match selection {
            Selection::Path(path)
                if safe_test_file(path, root, "exunit")
                    && ownership
                        .owner_of(path)
                        .is_ok_and(|owner| owner == manifest.owner) =>
            {
                Some(path.clone())
            }
            _ => None,
        }
    }));
    files.sort();
    files.dedup();
    for path in files {
        if ownership.owner_of(&path)? != manifest.owner || !safe_test_file(&path, root, "exunit") {
            continue;
        }
        let source = fs::read_to_string(&path).map_err(|error| {
            WorkflowError::Discovery(format!(
                "cannot read ExUnit test '{}': {error}",
                path.display()
            ))
        })?;
        if source
            .lines()
            .any(|line| literal_exunit_tag(line, suite.as_str()))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn git_visible_files(workspace: &Workspace, root: &Path) -> Result<Vec<PathBuf>, WorkflowError> {
    let mut builder = WalkBuilder::new(&workspace.root);
    builder
        .hidden(false)
        .ignore(false)
        .parents(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .follow_links(false);
    let scope = root.to_path_buf();
    builder.filter_entry(move |entry| {
        entry.file_name() != ".git"
            && (entry.path().starts_with(&scope) || scope.starts_with(entry.path()))
    });
    let mut files = Vec::new();
    for entry in builder.build() {
        let entry = entry.map_err(|error| {
            WorkflowError::Discovery(format!(
                "cannot walk target-test root '{}': {error}",
                root.display()
            ))
        })?;
        if entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
            && entry.path().starts_with(root)
        {
            files.push(fs::canonicalize(entry.path()).map_err(|error| {
                WorkflowError::Discovery(format!(
                    "cannot resolve target-test file '{}': {error}",
                    entry.path().display()
                ))
            })?);
        }
    }
    Ok(files)
}

fn literal_exunit_tag(line: &str, suite: &str) -> bool {
    let line = line.trim_start();
    let Some(rest) = line
        .strip_prefix("@tag")
        .or_else(|| line.strip_prefix("@moduletag"))
    else {
        return false;
    };
    let rest = rest.trim_start();
    rest.strip_prefix(':')
        .is_some_and(|value| identifier_starts_with(value, suite))
        || identifier_starts_with(rest, suite)
            && rest[suite.len()..]
                .trim_start()
                .strip_prefix(':')
                .is_some_and(|value| identifier_starts_with(value.trim_start(), "true"))
}

fn identifier_starts_with(value: &str, expected: &str) -> bool {
    value.starts_with(expected)
        && value[expected.len()..]
            .chars()
            .next()
            .is_none_or(|character| !character.is_ascii_alphanumeric() && character != '_')
}

fn contains_key(value: &toml::Value, key: &str) -> bool {
    match value {
        toml::Value::Table(table) => {
            table.contains_key(key) || table.values().any(|value| contains_key(value, key))
        }
        toml::Value::Array(values) => values.iter().any(|value| contains_key(value, key)),
        _ => false,
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PackageManifest {
    scripts: BTreeMap<String, String>,
    #[serde(rename = "devDependencies")]
    dev_dependencies: BTreeMap<String, String>,
    dependencies: BTreeMap<String, String>,
}

impl PackageManifest {
    fn has_dependency(&self, name: &str) -> bool {
        self.dev_dependencies.contains_key(name) || self.dependencies.contains_key(name)
    }
}

fn read_package(path: &Path) -> Result<PackageManifest, WorkflowError> {
    let source = fs::read_to_string(path).map_err(|error| {
        WorkflowError::Discovery(format!(
            "cannot read Node manifest '{}': {error}",
            path.display()
        ))
    })?;
    serde_json::from_str(&source).map_err(|error| {
        WorkflowError::Discovery(format!(
            "cannot parse Node manifest '{}': {error}",
            path.display()
        ))
    })
}

fn step(
    workspace: &Workspace,
    planned: PlannedStep,
    suite: Option<TestSuite>,
    mode: TestMode,
    index: usize,
) -> Result<Step, WorkflowError> {
    let manifest = planned.manifest().clone();
    let root = manifest
        .path
        .parent()
        .unwrap_or(Path::new(""))
        .to_path_buf();
    if let PlannedStep::Skipped {
        provider, suite, ..
    } = planned
    {
        return Ok(Step {
            id: StepId::new(format!("target-test:skip:{index:04}:{provider}")),
            label: format!("target test {provider} {} not applicable", suite.as_str()),
            capability: Capability::TargetTest,
            provider: ProviderId::new(NOT_APPLICABLE),
            owner: manifest.owner,
            cwd: root,
            program: OsString::new(),
            argv: Vec::new(),
            env: BTreeMap::new(),
            targets: Vec::new(),
            configuration_files: vec![manifest.path],
            depends_on: Vec::new(),
        });
    }
    let PlannedStep::Executable {
        provider,
        direct_targets,
        ..
    } = planned
    else {
        unreachable!("skipped steps returned above");
    };
    let effective_provider = coverage_provider(&manifest, provider, mode)?;
    let capability = if mode == TestMode::Coverage {
        Capability::Coverage
    } else {
        Capability::TargetTest
    };
    let threshold = coverage_threshold(workspace, &manifest.owner);
    let mut configuration_files = vec![manifest.path.clone()];
    let mut env = BTreeMap::new();
    let (program, mut argv) = match (mode, provider) {
        (TestMode::Test, "exunit") => {
            let mut argv = vec![OsString::from("test")];
            if let Some(suite) = suite {
                argv.extend([OsString::from("--only"), OsString::from(suite.as_str())]);
            }
            argv.extend(relative_targets(&root, &direct_targets));
            ("mix", argv)
        }
        (TestMode::Test, "package-test") => {
            let script = suite.map_or_else(
                || "test".to_owned(),
                |suite| format!("test:{}", suite.as_str()),
            );
            let mut argv = vec![OsString::from("run"), OsString::from(script)];
            argv.extend(relative_targets(&root, &direct_targets));
            ("pnpm", argv)
        }
        (TestMode::Test, "nextest") => (
            "cargo",
            vec![OsString::from("nextest"), OsString::from("run")],
        ),
        (TestMode::Coverage, "exunit") => {
            let coveralls = root.join("coveralls.json");
            if coveralls.is_file() {
                configuration_files.push(coveralls);
            }
            configuration_files.push(workspace.root.join("just.yaml"));
            env.insert(OsString::from("MIX_ENV"), OsString::from("test"));
            env.insert(
                OsString::from("JUST_EXCOVERALLS_CONFIG"),
                OsString::from(TEMPORARY_CONFIG),
            );
            env.insert(
                OsString::from("JUST_COVERAGE_LINES"),
                OsString::from(threshold.unwrap_or(0).to_string()),
            );
            (
                "mix",
                vec![
                    OsString::from("eval"),
                    OsString::from(EXCOVERALLS_BOOTSTRAP),
                ],
            )
        }
        (TestMode::Coverage, "package-test") => {
            let mut argv = vec![
                OsString::from("run"),
                OsString::from("test"),
                OsString::from("--coverage.enabled"),
            ];
            if let Some(threshold) = threshold {
                configuration_files.push(workspace.root.join("just.yaml"));
                argv.push(OsString::from(format!(
                    "--coverage.thresholds.lines={threshold}"
                )));
            }
            ("pnpm", argv)
        }
        (TestMode::Coverage, "nextest") => {
            let mut argv = vec![OsString::from("llvm-cov"), OsString::from("nextest")];
            if let Some(threshold) = threshold {
                configuration_files.push(workspace.root.join("just.yaml"));
                argv.extend([
                    OsString::from("--fail-under-lines"),
                    OsString::from(threshold.to_string()),
                ]);
            }
            ("cargo", argv)
        }
        _ => {
            return Err(WorkflowError::Planning(format!(
                "unsupported target-test provider '{provider}'"
            )));
        }
    };
    if mode == TestMode::Coverage {
        argv.shrink_to_fit();
    }
    configuration_files.sort();
    configuration_files.dedup();
    Ok(Step {
        id: StepId::new(format!(
            "{}:{index:04}:{effective_provider}",
            capability_name(capability)
        )),
        label: format!("{} {effective_provider}", capability_name(capability)),
        capability,
        provider: ProviderId::new(effective_provider),
        owner: manifest.owner,
        cwd: root,
        program: OsString::from(program),
        argv,
        env,
        targets: direct_targets,
        configuration_files,
        depends_on: Vec::new(),
    })
}

fn coverage_provider(
    manifest: &DiscoveredResource,
    provider: &str,
    mode: TestMode,
) -> Result<&'static str, WorkflowError> {
    if mode == TestMode::Test {
        return Ok(match provider {
            "exunit" => "exunit",
            "package-test" => "package-test",
            "nextest" => "nextest",
            _ => {
                return Err(WorkflowError::Planning(
                    "unknown target-test provider".into(),
                ));
            }
        });
    }
    match provider {
        "exunit" => match excoveralls_readiness(&manifest.path) {
            ExCoverallsReadiness::Ready { .. } => Ok("excoveralls"),
            ExCoverallsReadiness::Invalid(reason) => Err(WorkflowError::Planning(reason)),
        },
        "package-test" => {
            let package = read_package(&manifest.path)?;
            if !package.has_dependency("vitest") {
                return Err(missing_adapter(manifest, "Vitest"));
            }
            if !package.has_dependency("@vitest/coverage-v8")
                && !package.has_dependency("@vitest/coverage-istanbul")
            {
                return Err(missing_adapter(manifest, "Vitest coverage adapter"));
            }
            Ok("vitest-coverage")
        }
        "nextest" => Ok("llvm-cov"),
        _ => Err(WorkflowError::Planning(format!(
            "unsupported target-test provider '{provider}'"
        ))),
    }
}

fn missing_adapter(manifest: &DiscoveredResource, adapter: &str) -> WorkflowError {
    WorkflowError::Planning(format!(
        "selected test-capable component at '{}' requires {adapter} for coverage",
        manifest.path.parent().unwrap_or(Path::new("")).display()
    ))
}

fn coverage_threshold(workspace: &Workspace, owner: &Owner) -> Option<u8> {
    let Owner::Component { target, component } = owner else {
        return workspace.config.coverage_lines;
    };
    workspace.config.targets.get(target).and_then(|target| {
        target
            .components
            .iter()
            .find(|candidate| candidate.name == *component)
            .and_then(|component| component.coverage_lines)
    })
}

fn relative_targets(root: &Path, targets: &[PathBuf]) -> Vec<OsString> {
    targets
        .iter()
        .map(|target| {
            target
                .strip_prefix(root)
                .unwrap_or(target)
                .as_os_str()
                .to_owned()
        })
        .collect()
}

fn provider_order(mode: TestMode, provider: &str) -> usize {
    match (mode, provider) {
        (_, "exunit") => 0,
        (_, "package-test") => 1,
        (_, "nextest") => 2,
        _ => usize::MAX,
    }
}

fn owner_key(owner: &Owner) -> String {
    match owner {
        Owner::Repository(_) => String::new(),
        Owner::Target(target) => target.clone(),
        Owner::Component { target, component } => {
            format!("{target}:{}", component.as_deref().unwrap_or_default())
        }
    }
}

pub fn execute_plan(
    workspace: &Workspace,
    plan: &Plan,
    selectors: Vec<String>,
    started_at: String,
    mode: TestMode,
) -> Result<(Report, String, String), WorkflowError> {
    let interrupt = InterruptGuard::install()?;
    let reservations = preflight(workspace, plan)?;
    let started = Instant::now();
    let mut reports = Vec::new();
    let mut aggregate_stdout = String::new();
    let mut aggregate_stderr = String::new();
    let mut interrupted = false;

    for step in plan.steps() {
        if interrupt.received() {
            interrupted = true;
            break;
        }
        if step.provider.as_str() == NOT_APPLICABLE {
            reports.push(skipped_report(workspace, step));
            continue;
        }
        let reservation = reservations.get(step.id.as_str());
        let step_started = Instant::now();
        let output = run_command(step, reservation, &interrupt)?;
        aggregate_stdout.push_str(&String::from_utf8_lossy(&output.stdout));
        aggregate_stderr.push_str(&String::from_utf8_lossy(&output.stderr));
        reports.push(command_report(
            workspace,
            step,
            &output,
            step_started.elapsed().as_millis() as u64,
        ));
        if interrupt.received() || was_interrupted(&output) {
            interrupted = true;
            break;
        }
    }

    let status = if interrupted {
        ReportStatus::Interrupted
    } else if reports
        .iter()
        .any(|report| report.status == StepStatus::Failed)
    {
        ReportStatus::Failed
    } else {
        ReportStatus::Passed
    };
    Ok((
        Report {
            schema_version: 1,
            command: command_name(mode).into(),
            selectors,
            started_at,
            duration: started.elapsed().as_millis() as u64,
            status,
            steps: reports,
        },
        aggregate_stdout,
        aggregate_stderr,
    ))
}

struct CoverallsReservation {
    file: tempfile::NamedTempFile,
}

fn preflight(
    workspace: &Workspace,
    plan: &Plan,
) -> Result<HashMap<String, CoverallsReservation>, WorkflowError> {
    let mut tools = BTreeSet::new();
    let mut companions = BTreeSet::new();
    let mut reservations = HashMap::new();
    for step in plan.steps() {
        if step.provider.as_str() == NOT_APPLICABLE {
            continue;
        }
        if !step.cwd.is_dir() {
            return Err(WorkflowError::Planning(format!(
                "{} working directory '{}' is not accessible",
                capability_name(step.capability),
                step.cwd.display()
            )));
        }
        for target in &step.targets {
            if !target.is_file() {
                return Err(WorkflowError::Planning(format!(
                    "target-test input '{}' is not accessible",
                    target.display()
                )));
            }
        }
        for configuration in &step.configuration_files {
            if !configuration.is_file() {
                return Err(WorkflowError::Planning(format!(
                    "target-test configuration '{}' is not accessible",
                    configuration.display()
                )));
            }
        }
        tools.insert(step.program.clone());
        match step.provider.as_str() {
            "nextest" => {
                companions.insert(OsString::from("cargo-nextest"));
            }
            "llvm-cov" => {
                companions.insert(OsString::from("cargo-llvm-cov"));
                companions.insert(OsString::from("cargo-nextest"));
            }
            "excoveralls" => {
                let reservation = reserve_coveralls(workspace, step)?;
                reservations.insert(step.id.as_str().to_owned(), reservation);
            }
            _ => {}
        }
    }
    for tool in tools.into_iter().chain(companions) {
        if find_program(&tool).is_none() {
            return Err(WorkflowError::Planning(format!(
                "target-test tool '{}' is not available",
                tool.to_string_lossy()
            )));
        }
    }
    Ok(reservations)
}

fn reserve_coveralls(
    workspace: &Workspace,
    step: &Step,
) -> Result<CoverallsReservation, WorkflowError> {
    let coveralls = step.cwd.join("coveralls.json");
    if coveralls.exists() {
        let resolved = fs::canonicalize(&coveralls).map_err(|error| {
            WorkflowError::Configuration(format!(
                "cannot resolve '{}': {error}",
                coveralls.display()
            ))
        })?;
        if !resolved.starts_with(&workspace.root) {
            return Err(WorkflowError::Configuration(format!(
                "'{}' resolves outside repository '{}'",
                coveralls.display(),
                workspace.root.display()
            )));
        }
    }
    let manifest = step.cwd.join("mix.exs");
    let mut document = match excoveralls_readiness(&manifest) {
        ExCoverallsReadiness::Ready { configuration } => configuration,
        ExCoverallsReadiness::Invalid(reason) => {
            return Err(WorkflowError::Configuration(reason));
        }
    };
    let object = document
        .as_object_mut()
        .expect("ready ExCoveralls configuration has an object root");
    let coverage_options = object
        .entry("coverage_options")
        .or_insert_with(|| serde_json::json!({}));
    let coverage_options = coverage_options
        .as_object_mut()
        .expect("ready ExCoveralls coverage_options is an object");
    let threshold = step
        .env
        .get(OsStr::new("JUST_COVERAGE_LINES"))
        .and_then(|value| value.to_str())
        .and_then(|value| value.parse::<u8>().ok())
        .ok_or_else(|| WorkflowError::Planning("missing ExCoveralls threshold".into()))?;
    coverage_options.insert("minimum_coverage".into(), serde_json::json!(threshold));

    let mut file = tempfile::Builder::new()
        .prefix(".just-coveralls-")
        .suffix(".json")
        .tempfile()
        .map_err(|error| {
            WorkflowError::Planning(format!(
                "cannot reserve temporary ExCoveralls configuration: {error}"
            ))
        })?;
    if file.path().starts_with(&workspace.root) {
        return Err(WorkflowError::Planning(
            "temporary ExCoveralls configuration was reserved inside the repository".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                WorkflowError::Planning(format!(
                    "cannot restrict temporary ExCoveralls configuration: {error}"
                ))
            })?;
    }
    serde_json::to_writer(file.as_file_mut(), &document).map_err(|error| {
        WorkflowError::Planning(format!(
            "cannot write temporary ExCoveralls configuration: {error}"
        ))
    })?;
    Ok(CoverallsReservation { file })
}

fn run_command(
    step: &Step,
    reservation: Option<&CoverallsReservation>,
    interrupt: &InterruptGuard,
) -> Result<Output, WorkflowError> {
    let mut env = step.env.clone();
    env.remove(OsStr::new("JUST_COVERAGE_LINES"));
    if let Some(reservation) = reservation {
        env.insert(
            OsString::from("JUST_EXCOVERALLS_CONFIG"),
            reservation.file.path().as_os_str().to_owned(),
        );
    }
    let mut command = Command::new(&step.program);
    command
        .args(&step.argv)
        .current_dir(&step.cwd)
        .envs(env)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|error| {
        WorkflowError::Planning(format!(
            "could not launch target-test tool '{}': {error}",
            step.program.to_string_lossy()
        ))
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        WorkflowError::Planning("target-test tool stdout was not captured".into())
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        WorkflowError::Planning("target-test tool stderr was not captured".into())
    })?;
    let stdout_reader = std::thread::spawn(move || read_output(stdout));
    let stderr_reader = std::thread::spawn(move || read_output(stderr));
    loop {
        if interrupt.received() {
            terminate(&mut child).map_err(|error| {
                WorkflowError::Planning(format!(
                    "could not stop interrupted target-test tool '{}': {error}",
                    step.program.to_string_lossy()
                ))
            })?;
            let status = child.wait().map_err(|error| {
                WorkflowError::Planning(format!(
                    "could not wait for interrupted target-test tool '{}': {error}",
                    step.program.to_string_lossy()
                ))
            })?;
            return collected_output(status, stdout_reader, stderr_reader);
        }
        match child.try_wait().map_err(|error| {
            WorkflowError::Planning(format!(
                "could not wait for target-test tool '{}': {error}",
                step.program.to_string_lossy()
            ))
        })? {
            Some(status) => {
                #[cfg(unix)]
                if provider_received_sigint(&status) {
                    terminate(&mut child).map_err(|error| {
                        WorkflowError::Planning(format!(
                            "could not stop interrupted target-test tool '{}': {error}",
                            step.program.to_string_lossy()
                        ))
                    })?;
                }
                return collected_output(status, stdout_reader, stderr_reader);
            }
            None => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
}

#[cfg(unix)]
fn provider_received_sigint(status: &std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;

    status.signal() == Some(libc::SIGINT)
}

#[cfg(unix)]
fn terminate(child: &mut std::process::Child) -> std::io::Result<()> {
    let process_group = i32::try_from(child.id())
        .map_err(|error| std::io::Error::other(format!("invalid child process id: {error}")))?;
    let result = unsafe { libc::kill(-process_group, libc::SIGKILL) };
    if result == 0 {
        Ok(())
    } else {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

#[cfg(not(unix))]
fn terminate(child: &mut std::process::Child) -> std::io::Result<()> {
    child.kill()
}

fn read_output(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    reader.read_to_end(&mut output)?;
    Ok(output)
}

fn collected_output(
    status: std::process::ExitStatus,
    stdout: std::thread::JoinHandle<std::io::Result<Vec<u8>>>,
    stderr: std::thread::JoinHandle<std::io::Result<Vec<u8>>>,
) -> Result<Output, WorkflowError> {
    let collect = |name: &str,
                   reader: std::thread::JoinHandle<std::io::Result<Vec<u8>>>|
     -> Result<Vec<u8>, WorkflowError> {
        reader
            .join()
            .map_err(|_| {
                WorkflowError::Planning(format!("target-test tool {name} reader panicked"))
            })?
            .map_err(|error| {
                WorkflowError::Planning(format!("could not read target-test tool {name}: {error}"))
            })
    };
    Ok(Output {
        status,
        stdout: collect("stdout", stdout)?,
        stderr: collect("stderr", stderr)?,
    })
}

fn skipped_report(workspace: &Workspace, step: &Step) -> StepReport {
    report_fields(
        workspace,
        step,
        StepStatus::Skipped,
        None,
        0,
        Vec::new(),
        Some("not applicable: provider does not configure the requested suite".into()),
        String::new(),
        String::new(),
    )
}

fn command_report(
    workspace: &Workspace,
    step: &Step,
    output: &Output,
    duration: u64,
) -> StepReport {
    let mut argv = vec![step.program.to_string_lossy().into_owned()];
    argv.extend(
        step.argv
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned()),
    );
    report_fields(
        workspace,
        step,
        if output.status.success() {
            StepStatus::Passed
        } else {
            StepStatus::Failed
        },
        output.status.code(),
        duration,
        argv,
        None,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[allow(clippy::too_many_arguments)]
fn report_fields(
    workspace: &Workspace,
    step: &Step,
    status: StepStatus,
    exit_code: Option<i32>,
    duration: u64,
    argv: Vec<String>,
    skip_reason: Option<String>,
    stdout: String,
    stderr: String,
) -> StepReport {
    let (target, component) = match &step.owner {
        Owner::Target(target) => (Some(target.clone()), None),
        Owner::Component { target, component } => (Some(target.clone()), component.clone()),
        Owner::Repository(_) => (None, None),
    };
    let relative = |path: &Path| {
        path.strip_prefix(&workspace.root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    };
    StepReport {
        id: step.id.as_str().into(),
        capability: capability_name(step.capability).into(),
        provider: step.provider.as_str().into(),
        target,
        component,
        cwd: relative(&step.cwd),
        argv,
        configuration_files: step
            .configuration_files
            .iter()
            .map(|path| relative(path))
            .collect(),
        targets: step.targets.iter().map(|path| relative(path)).collect(),
        status,
        exit_code,
        duration,
        skip_reason,
        stdout,
        stderr,
    }
}

fn capability_name(capability: Capability) -> &'static str {
    match capability {
        Capability::TargetTest => "target_test",
        Capability::Coverage => "coverage",
        _ => unreachable!("target testing supports test and coverage only"),
    }
}

const fn command_name(mode: TestMode) -> &'static str {
    match mode {
        TestMode::Test => "test",
        TestMode::Coverage => "coverage",
    }
}
