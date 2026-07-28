use std::env;
use std::ffi::OsStr;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::capability::Capability;
use crate::checking;
use crate::doctor;
use crate::error::{EXIT_INTERRUPTED, EXIT_SUCCESS, EXIT_VERIFICATION_FAILED};
use crate::error::{EXIT_USAGE, WorkflowError};
use crate::formatting::{build_plan, execute_plan};
use crate::linting;
use crate::report::{Report, ReportStatus, render_human, render_json};
use crate::selector::resolve_selectors;
use crate::specification;
use crate::workspace::{Workspace, invocation_directory};

#[derive(Debug, Parser)]
#[command(name = "workflow", version, about = "kokjinsam/just workflow runtime")]
pub struct Cli {
    #[arg(long, value_enum, default_value_t, global = true)]
    pub output: OutputFormat,

    #[command(subcommand)]
    pub command: FoundationCommand,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Human,
    Json,
}

#[derive(Debug, Subcommand)]
pub enum FoundationCommand {
    Format(SelectorArgs),
    Lint(SelectorArgs),
    Typecheck(SelectorArgs),
    Compile(SelectorArgs),
    Spec(SpecArgs),
    Test(SelectorArgs),
    Coverage(SelectorArgs),
    Verify,
    Doctor,
}

#[derive(Debug, Args)]
pub struct SelectorArgs {
    #[arg(value_name = "SELECTOR")]
    pub selectors: Vec<String>,
}

#[derive(Debug, Args)]
pub struct SpecArgs {
    #[arg(value_enum, value_name = "KIND")]
    pub kind: Option<specification::SpecificationKind>,

    #[arg(value_name = "NAME_OR_PATH")]
    pub selections: Vec<String>,
}

impl FoundationCommand {
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Format(_) => "format",
            Self::Lint(_) => "lint",
            Self::Typecheck(_) => "typecheck",
            Self::Compile(_) => "compile",
            Self::Spec(_) => "spec",
            Self::Test(_) => "test",
            Self::Coverage(_) => "coverage",
            Self::Verify => "verify",
            Self::Doctor => "doctor",
        }
    }

    #[must_use]
    pub fn selectors(&self) -> &[String] {
        match self {
            Self::Format(args)
            | Self::Lint(args)
            | Self::Typecheck(args)
            | Self::Compile(args)
            | Self::Test(args)
            | Self::Coverage(args) => &args.selectors,
            Self::Spec(args) => &args.selections,
            Self::Verify | Self::Doctor => &[],
        }
    }
}

pub fn run_from_environment(cli: &Cli) -> u8 {
    let current = match env::current_dir() {
        Ok(current) => current,
        Err(error) => {
            return render_error(
                cli.output,
                &WorkflowError::Workspace(format!("cannot determine current directory: {error}")),
            );
        }
    };
    let caller = env::var_os("JUST_CALLER_DIR");
    run(cli, &current, caller.as_deref())
}

pub fn run(cli: &Cli, current: &Path, caller: Option<&OsStr>) -> u8 {
    let invocation = match invocation_directory(current, caller) {
        Ok(invocation) => invocation,
        Err(error) => return render_error(cli.output, &error),
    };
    let workspace = match Workspace::discover(&invocation) {
        Ok(workspace) => workspace,
        Err(error) => return render_error(cli.output, &error),
    };
    if let FoundationCommand::Spec(args) = &cli.command {
        return run_spec(cli, &workspace, args);
    }
    let selections = match resolve_selectors(&workspace, cli.command.selectors()) {
        Ok(selections) => selections,
        Err(error) => return render_error(cli.output, &error),
    };

    if matches!(cli.command, FoundationCommand::Format(_)) {
        return run_format(cli, &workspace, &selections);
    }
    if matches!(cli.command, FoundationCommand::Lint(_)) {
        return run_lint(cli, &workspace, &selections);
    }
    if matches!(
        cli.command,
        FoundationCommand::Typecheck(_) | FoundationCommand::Compile(_)
    ) {
        return run_checking(cli, &workspace, &selections);
    }
    if matches!(cli.command, FoundationCommand::Doctor) {
        return run_doctor(cli, &workspace);
    }

    let report = Report::unavailable(
        cli.command.name(),
        cli.command.selectors().to_vec(),
        unix_timestamp(),
    );
    match cli.output {
        OutputFormat::Human => eprint!("{}", render_human(&report)),
        OutputFormat::Json => match render_json(&report) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("error: could not serialize report: {error}");
                return EXIT_USAGE;
            }
        },
    }
    EXIT_USAGE
}

fn run_spec(cli: &Cli, workspace: &Workspace, args: &SpecArgs) -> u8 {
    let plan = match specification::build_plan(workspace, args.kind, &args.selections) {
        Ok(plan) => plan,
        Err(error) => return render_error(cli.output, &error),
    };
    let (report, child_stdout, child_stderr) = match specification::execute_plan(
        workspace,
        &plan,
        args.selections.clone(),
        unix_timestamp(),
    ) {
        Ok(result) => result,
        Err(error) => return render_error(cli.output, &error),
    };
    render_completed(cli.output, &report, &child_stdout, &child_stderr)
}

fn run_format(cli: &Cli, workspace: &Workspace, selections: &[crate::selector::Selection]) -> u8 {
    let plan = match build_plan(workspace, selections, Capability::Format) {
        Ok(plan) => plan,
        Err(error) => return render_error(cli.output, &error),
    };
    let (report, child_stdout, child_stderr) = match execute_plan(
        workspace,
        &plan,
        cli.command.selectors().to_vec(),
        unix_timestamp(),
    ) {
        Ok(result) => result,
        Err(error) => return render_error(cli.output, &error),
    };

    if !child_stdout.is_empty() {
        eprint!("{child_stdout}");
    }
    if !child_stderr.is_empty() {
        eprint!("{child_stderr}");
    }
    match cli.output {
        OutputFormat::Human => eprint!("{}", render_human(&report)),
        OutputFormat::Json => match render_json(&report) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("error: could not serialize report: {error}");
                return EXIT_USAGE;
            }
        },
    }

    match report.status {
        ReportStatus::Passed => EXIT_SUCCESS,
        ReportStatus::Failed => EXIT_VERIFICATION_FAILED,
        ReportStatus::Interrupted => EXIT_INTERRUPTED,
        ReportStatus::Error | ReportStatus::Unavailable => EXIT_USAGE,
    }
}

fn run_lint(cli: &Cli, workspace: &Workspace, selections: &[crate::selector::Selection]) -> u8 {
    let plan = match linting::build_plan(workspace, selections) {
        Ok(plan) => plan,
        Err(error) => return render_error(cli.output, &error),
    };
    let (report, child_stdout, child_stderr) = match linting::execute_plan(
        workspace,
        &plan,
        cli.command.selectors().to_vec(),
        unix_timestamp(),
    ) {
        Ok(result) => result,
        Err(error) => return render_error(cli.output, &error),
    };
    render_completed(cli.output, &report, &child_stdout, &child_stderr)
}

fn run_checking(cli: &Cli, workspace: &Workspace, selections: &[crate::selector::Selection]) -> u8 {
    let capability = match cli.command {
        FoundationCommand::Typecheck(_) => Capability::Typecheck,
        FoundationCommand::Compile(_) => Capability::Compile,
        _ => unreachable!("checking requires typecheck or compile"),
    };
    let plan = match checking::build_plan(workspace, selections, capability) {
        Ok(plan) => plan,
        Err(error) => return render_error(cli.output, &error),
    };
    let (report, child_stdout, child_stderr) = match checking::execute_plan(
        workspace,
        &plan,
        cli.command.selectors().to_vec(),
        unix_timestamp(),
        capability,
    ) {
        Ok(result) => result,
        Err(error) => return render_error(cli.output, &error),
    };
    render_completed(cli.output, &report, &child_stdout, &child_stderr)
}

fn run_doctor(cli: &Cli, workspace: &Workspace) -> u8 {
    let report = match doctor::inspect(workspace, unix_timestamp()) {
        Ok(report) => report,
        Err(error) => return render_error(cli.output, &error),
    };
    render_completed(cli.output, &report, "", "")
}

fn render_completed(
    output: OutputFormat,
    report: &Report,
    child_stdout: &str,
    child_stderr: &str,
) -> u8 {
    if !child_stdout.is_empty() {
        eprint!("{child_stdout}");
    }
    if !child_stderr.is_empty() {
        eprint!("{child_stderr}");
    }
    match output {
        OutputFormat::Human => eprint!("{}", render_human(report)),
        OutputFormat::Json => match render_json(report) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("error: could not serialize report: {error}");
                return EXIT_USAGE;
            }
        },
    }
    match report.status {
        ReportStatus::Passed => EXIT_SUCCESS,
        ReportStatus::Failed => EXIT_VERIFICATION_FAILED,
        ReportStatus::Interrupted => EXIT_INTERRUPTED,
        ReportStatus::Error | ReportStatus::Unavailable => EXIT_USAGE,
    }
}

fn render_error(output: OutputFormat, error: &WorkflowError) -> u8 {
    match output {
        OutputFormat::Human => eprintln!("error: {error}"),
        OutputFormat::Json => {
            // No report exists until configuration and selectors are valid.
            eprintln!("error: {error}");
        }
    }
    error.exit_code()
}

fn unix_timestamp() -> String {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or_else(
        |_| "0".to_owned(),
        |duration| duration.as_secs().to_string(),
    )
}
