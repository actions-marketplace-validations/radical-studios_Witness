//! Command-line interface definition.

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "witness",
    version,
    about = "Tells reviewers which parts of an AI-written change were verified and which were guessed"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Write a default witness.toml and create .witness/ at the repo root.
    Init(InitArgs),
    /// Set up, remove or show the hooks that record agent sessions, for
    /// claude-code, codex, copilot, cursor and gemini-cli.
    Hooks(HooksArgs),
    /// Read an agent's hook payload from stdin and append to the session
    /// log. Always exits 0.
    Hook {
        /// The agent whose hook this is: claude-code, codex, copilot, cursor,
        /// gemini-cli or generic. Guessed from the payload when omitted.
        #[arg(long)]
        agent: Option<String>,
        /// Hook event, e.g. pre-tool-use, post-tool-use, session-end. The
        /// payload's own event name takes precedence when it has one.
        event: String,
    },
    /// Record an assumption the agent relied on without verifying it.
    Assume(AssumeArgs),
    /// Run a command and record its exact exit code and the files it
    /// changed. For agents without hooks, or to keep a piped test run's
    /// result: `witness exec -- cargo test | tail`.
    Exec(ExecArgs),
    /// Record an event by hand, for agents without hooks and wrapper scripts.
    Record(RecordArgs),
    /// Print the review plan for base...head.
    Report(ReportArgs),
    /// Post the review plan to a code review platform.
    Post(PostArgs),
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Initialize the current directory instead of the git top level, e.g.
    /// one package of a monorepo.
    #[arg(long)]
    pub here: bool,
}

#[derive(Debug, Args)]
pub struct AssumeArgs {
    /// File the assumption applies to.
    #[arg(long)]
    pub file: String,
    /// Line range in the file as it is now, e.g. 10-24 or 7.
    #[arg(long)]
    pub lines: Option<String>,
    /// Session to record in. Defaults to $WITNESS_SESSION_ID, then the most
    /// recent session seen by `witness hook`.
    #[arg(long)]
    pub session: Option<String>,
    /// What was assumed, in one sentence.
    #[arg(required = true, num_args = 1..)]
    pub text: Vec<String>,
}

#[derive(Debug, Args)]
pub struct HooksArgs {
    #[command(subcommand)]
    pub action: HooksAction,
}

#[derive(Debug, Subcommand)]
pub enum HooksAction {
    /// Write the agents' hook configuration into this repository.
    Install {
        /// Agents to set up: claude-code, codex, copilot, cursor, gemini-cli.
        agents: Vec<String>,
        /// Set up every supported agent.
        #[arg(long)]
        all: bool,
        /// Write Claude Code hooks even though the witness plugin is enabled.
        #[arg(long)]
        force: bool,
    },
    /// Remove Witness's hooks; other hooks are kept.
    Uninstall {
        agents: Vec<String>,
        #[arg(long)]
        all: bool,
    },
    /// Show which agents have Witness hooks installed.
    Status,
}

#[derive(Debug, Args)]
pub struct ExecArgs {
    /// Session to record in. Defaults to $WITNESS_SESSION_ID, then the most
    /// recent session seen by `witness hook`.
    #[arg(long)]
    pub session: Option<String>,
    /// Agent name to record. Defaults to the session's agent.
    #[arg(long)]
    pub agent: Option<String>,
    /// The command and its arguments. A single argument with spaces runs
    /// through the shell.
    #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
pub struct RecordArgs {
    #[command(subcommand)]
    pub what: RecordWhat,
    /// Session to record in. Defaults to $WITNESS_SESSION_ID, then the most
    /// recent session seen by `witness hook`.
    #[arg(long, global = true)]
    pub session: Option<String>,
    /// Agent name to record. Defaults to the session's agent.
    #[arg(long, global = true)]
    pub agent: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum RecordWhat {
    /// The agent changed a file.
    Edit {
        #[arg(long)]
        file: String,
        /// Line range in the file as it is now, e.g. 10-24. Omit for the
        /// whole file.
        #[arg(long)]
        lines: Option<String>,
        /// Name of the tool that made the edit.
        #[arg(long, default_value = "manual")]
        tool: String,
    },
    /// The agent read a file.
    Read {
        #[arg(long)]
        file: String,
    },
    /// The agent ran a command.
    Command {
        /// Its exit code; omit if unknown.
        #[arg(long)]
        exit_code: Option<i32>,
        #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
}

#[derive(Debug, Clone, Args)]
pub struct RangeArgs {
    /// Base ref the change will merge into.
    #[arg(long, default_value = "main")]
    pub base: String,
    /// Head ref of the change.
    #[arg(long, default_value = "HEAD")]
    pub head: String,
}

#[derive(Debug, Args)]
pub struct ReportArgs {
    #[command(flatten)]
    pub range: RangeArgs,
    #[arg(long, value_enum, default_value_t = Format::Markdown)]
    pub format: Format,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Markdown,
    Json,
}

#[derive(Debug, Args)]
pub struct PostArgs {
    #[arg(long, value_enum)]
    pub platform: PlatformArg,
    #[command(flatten)]
    pub range: RangeArgs,
    /// owner/repo (GitHub) or project path (GitLab).
    #[arg(long)]
    pub repo: Option<String>,
    /// Pull request or merge request number.
    #[arg(long)]
    pub number: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PlatformArg {
    Github,
    Gitlab,
    DryRun,
}
