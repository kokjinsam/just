//! Planning, execution, and reporting primitives for the workflow CLI.

/// Application-test discovery and lifecycle execution.
pub mod application_testing;
/// Capability and provider binding.
pub mod capability;
/// Type-checking and compilation workflows.
pub mod checking;
/// Command-line parsing and dispatch.
pub mod cli;
/// Workspace configuration loading and validation.
pub mod config;
/// Git-aware workspace resource discovery.
pub mod discovery;
/// Environment and provider readiness diagnostics.
pub mod doctor;
/// Stable workflow errors and exit codes.
pub mod error;
/// Formatting workflow planning and execution.
pub mod formatting;
/// Lint workflow planning and execution.
pub mod linting;
/// Resource ownership classification.
pub mod ownership;
/// Executable workflow plans and steps.
pub mod plan;
/// Stable human and JSON reports.
pub mod report;
/// User selector parsing and resolution.
pub mod selector;
/// Specification checking workflows.
pub mod specification;
/// Target test and coverage workflows.
pub mod target_testing;
/// Workspace discovery and invocation context.
pub mod workspace;
