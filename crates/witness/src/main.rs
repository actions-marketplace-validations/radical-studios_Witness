//! The `witness` command-line tool.

mod assume;
mod cli;
mod hook;
mod init;
mod pipeline;
mod workspace;

use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Command};

fn main() -> ExitCode {
    // `witness hook` must never fail the agent's tool call: even argument
    // errors and panics end in exit 0, with details on stderr.
    if std::env::args_os().nth(1).is_some_and(|a| a == "hook") {
        hook::main_guarded();
        return ExitCode::SUCCESS;
    }
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("witness: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Init(args) => init::run(args),
        Command::Hook { event } => {
            hook::run(&event);
            Ok(())
        }
        Command::Assume(args) => assume::run(args),
        Command::Report(args) => pipeline::run_report(args),
        Command::Post(args) => pipeline::run_post(args),
    }
}
