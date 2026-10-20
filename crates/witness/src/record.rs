//! `witness record`: record an edit, read or command by hand, for agents
//! without hooks and for wrapper scripts.

use anyhow::{Context, Result, anyhow};
use time::OffsetDateTime;
use witness_core::capture::{normalize, relative_to};
use witness_core::command;
use witness_core::record::{CommandClass, EditVia, Event, EventKind};
use witness_core::scope;
use witness_core::testsum::RunOutcome;

use crate::cli::{RecordArgs, RecordWhat};
use crate::exec::shell_join;
use crate::workspace::{self, parse_lines, repo_path};

pub fn run(args: RecordArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("finding the working directory")?;
    let root = workspace::find_root(&cwd).ok_or_else(|| {
        anyhow!("no .witness/ directory found; run `witness init` at the repo root")
    })?;
    let (session, agent) = workspace::session_for(&root, args.session, args.agent);
    let (kind, summary) = match args.what {
        RecordWhat::Edit { file, lines, tool } => {
            let file = repo_path(&root, &cwd, &file)?;
            let lines = lines.as_deref().map(parse_lines).transpose()?;
            let summary = match lines {
                Some(range) => format!("edit of {file}:{range}"),
                None => format!("edit of {file}"),
            };
            let kind = EventKind::Edit {
                line_hashes: workspace::range_hashes(&root, &file, lines),
                file,
                lines,
                change: None,
                tool,
                via: Some(EditVia::Reported),
                removed_hashes: None,
            };
            (kind, summary)
        }
        RecordWhat::Read { file } => {
            let file = repo_path(&root, &cwd, &file)?;
            let summary = format!("read of {file}");
            (EventKind::Read { file }, summary)
        }
        RecordWhat::Command { exit_code, command } => {
            let config = workspace::load_config(&root)?;
            let text = shell_join(&command);
            let verdict = command::classify(&config, &text);
            let exit_code = exit_code.filter(|_| verdict.exit_attributable);
            let checks = verdict.class != CommandClass::Other;
            let outcome = exit_code.filter(|_| checks).map(|c| {
                if c == 0 {
                    RunOutcome::Passed
                } else {
                    RunOutcome::Failed
                }
            });
            let cwd = relative_to(&normalize(&cwd), &normalize(&root));
            let kind = EventKind::Command {
                classification: verdict.class,
                exit_code,
                command: command::sanitize_command(&text),
                outcome,
                tests: None,
                scope: checks
                    .then(|| scope::parse(&verdict.simple, cwd.as_deref(), &verdict.cd))
                    .flatten(),
            };
            let summary = format!("{} command", verdict.class.as_str());
            (kind, summary)
        }
    };
    let event = Event::new(&session, &agent, OffsetDateTime::now_utc(), kind);
    workspace::append_events(&root, &[event])?;
    println!("witness: recorded {summary} in session {session}");
    Ok(())
}
