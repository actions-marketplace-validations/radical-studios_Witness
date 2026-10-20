//! The `witness` command-line tool.

mod assume;
mod ci;
mod cli;
mod exec;
mod hook;
mod hooks;
mod init;
mod pipeline;
mod record;
mod review;
mod snapshot;
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
    // `witness exec` exits with the command's own code.
    if let Command::Exec(args) = cli.command {
        return exec::run(args).unwrap_or_else(|err| {
            eprintln!("witness exec: {err:#}");
            ExitCode::FAILURE
        });
    }
    match run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("witness: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let ok = |r: Result<()>| r.map(|()| ExitCode::SUCCESS);
    match cli.command {
        Command::Init(args) => ok(init::run(args)),
        Command::Hook { agent, event } => {
            hook::run(agent.as_deref(), &event);
            Ok(ExitCode::SUCCESS)
        }
        Command::Assume(args) => ok(assume::run(args)),
        Command::Exec(_) => ok(unreachable_exec()),
        Command::Record(args) => ok(record::run(args)),
        Command::Hooks(args) => ok(hooks::run(args)),
        Command::Report(args) => review::run_report(args),
        Command::Status(args) => review::run_status(args),
        Command::Explain(args) => ok(review::run_explain(args)),
        Command::Sessions(args) => ok(review::run_sessions(args)),
        Command::Post(args) => review::run_post(args),
    }
}

/// `witness exec` is dispatched before [`run`], so it can return the
/// command's exit code.
fn unreachable_exec() -> Result<()> {
    anyhow::bail!("internal error: exec reached the generic dispatcher")
}
