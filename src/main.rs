//! Executable entry point for the workflow CLI.

use std::process::ExitCode;

use clap::Parser;
use kokjinsam_just::cli::{Cli, run_from_environment};

fn main() -> ExitCode {
    ExitCode::from(run_from_environment(&Cli::parse()))
}
