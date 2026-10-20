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
    /// Read a Claude Code hook payload from stdin and append to the session log.
    /// Always exits 0.
    Hook {
        /// Hook event, e.g. post-tool-use, post-tool-use-failure, session-end.
        /// The payload's hook_event_name takes precedence.
        event: String,
    },
    /// Record an assumption the agent relied on without verifying it.
    Assume(AssumeArgs),
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
