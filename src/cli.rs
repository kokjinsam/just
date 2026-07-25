use std::env;
use std::ffi::OsStr;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::error::{EXIT_USAGE, WorkflowError};
use crate::report::{Report, render_human, render_json};
use crate::selector::resolve_selectors;
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
    Spec,
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

impl FoundationCommand {
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Format(_) => "format",
            Self::Lint(_) => "lint",
            Self::Typecheck(_) => "typecheck",
            Self::Compile(_) => "compile",
            Self::Spec => "spec",
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
            Self::Spec | Self::Verify | Self::Doctor => &[],
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
    if let Err(error) = resolve_selectors(&workspace, cli.command.selectors()) {
        return render_error(cli.output, &error);
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
