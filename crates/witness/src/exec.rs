//! `witness exec -- <command>`: run a command and record its exact exit code
//! and the files it changed.
//!
//! The command's output passes through unchanged (Witness keeps only the
//! tail in memory, to find test results), and `witness exec` exits with the
//! command's code. Outside a repository with `.witness/` it just runs the
//! command.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result, bail};
use time::OffsetDateTime;
use witness_core::capture::{
    CaptureContext, CapturePlan, Exit, HookEvent, HookInput, Phase, ShellCall, ToolAction,
    ToolCall, call_key, capture_plan, events_for,
};

use crate::cli::ExecArgs;
use crate::snapshot::Snapshots;
use crate::workspace;

/// Output kept for finding test results: the end of each stream.
const TAIL_BYTES: usize = 256 * 1024;

pub fn run(args: ExecArgs) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("finding the working directory")?;
    let display = shell_join(&args.command);
    let Some(root) = workspace::find_root(&cwd) else {
        let outcome = run_child(&args.command)?;
        return Ok(exit_code(outcome.code));
    };
    let config = workspace::load_config(&root)?;
    let (session, agent) = workspace::session_for(&root, args.session, args.agent);
    let canonical = std::fs::canonicalize(&root).ok().filter(|c| c != &root);
    let call_id = format!("exec-{}-{}", std::process::id(), now_nanos());
    let input = |phase: Phase, exit: Exit, output: Option<String>| HookInput {
        agent: agent.clone(),
        session_id: session.clone(),
        cwd: Some(cwd.to_string_lossy().into_owned()),
        turn: None,
        subagent: None,
        model: None,
        event: HookEvent::Tool(ToolCall {
            phase,
            tool: "witness exec".to_owned(),
            call_id: Some(call_id.clone()),
            action: ToolAction::Shell(ShellCall {
                command: display.clone(),
                cwd: None,
                background: false,
                interrupted: false,
                exit,
                output,
            }),
        }),
    };

    let pre = input(Phase::Pre, Exit::Unknown, None);
    let ctx_now = |now| CaptureContext {
        config: &config,
        root: &root,
        canonical_root: canonical.as_deref(),
        now,
    };
    let plan = if config.raw.capture.snapshots {
        capture_plan(&pre, &ctx_now(OffsetDateTime::now_utc()))
    } else {
        CapturePlan::None
    };
    let key = call_key(&pre).unwrap_or_default();
    let snaps = Snapshots::new(&root, &config);
    if let Err(err) = snaps.before(&plan, &key, &session) {
        eprintln!("witness exec: could not snapshot the working tree: {err:#}");
    }

    let outcome = run_child(&args.command)?;

    let observed = match snaps.after(&plan, &key, &session, false) {
        Ok(after) => after.observed,
        Err(err) => {
            eprintln!("witness exec: could not snapshot the working tree: {err:#}");
            None
        }
    };
    let (phase, exit) = match outcome.code {
        Some(0) => (Phase::Post, Exit::Code(0)),
        Some(code) => (Phase::PostFailure, Exit::Code(code)),
        None => (Phase::PostFailure, Exit::Failed),
    };
    let post = input(phase, exit, Some(outcome.output));
    let read_file = |path: &Path| std::fs::read_to_string(path).ok();
    let events = events_for(
        &post,
        &ctx_now(OffsetDateTime::now_utc()),
        &read_file,
        observed.as_ref(),
    );
    if let Err(err) = workspace::append_events(&root, &events) {
        eprintln!("witness exec: could not record the run: {err:#}");
    }
    Ok(exit_code(outcome.code))
}

struct Outcome {
    /// `None` when the command was killed by a signal.
    code: Option<i32>,
    /// The tail of stdout and stderr.
    output: String,
}

fn run_child(command: &[String]) -> Result<Outcome> {
    let Some((program, rest)) = command.split_first() else {
        bail!("no command given");
    };
    // `witness exec "cargo test | tail"`: one argument with shell syntax.
    let mut cmd = if rest.is_empty() && program.contains(|c: char| c.is_whitespace()) {
        shell(program)
    } else {
        let mut cmd = Command::new(program);
        cmd.args(rest);
        cmd
    };
    let mut child = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running {program}"))?;
    let out = tee(child.stdout.take(), std::io::stdout());
    let err = tee(child.stderr.take(), std::io::stderr());
    let status = child.wait().context("waiting for the command")?;
    let mut output = String::new();
    for handle in [out, err] {
        if let Some(handle) = handle
            && let Ok(tail) = handle.join()
        {
            output.push_str(&String::from_utf8_lossy(&tail));
            output.push('\n');
        }
    }
    Ok(Outcome {
        code: status.code(),
        output,
    })
}

#[cfg(windows)]
fn shell(script: &str) -> Command {
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", script]);
    cmd
}

#[cfg(not(windows))]
fn shell(script: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", script]);
    cmd
}

/// Copies a child stream to `sink` as it arrives, keeping its tail.
fn tee<R, W>(source: Option<R>, sink: W) -> Option<thread::JoinHandle<Vec<u8>>>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    let mut source = source?;
    let sink = Arc::new(Mutex::new(sink));
    Some(thread::spawn(move || {
        let mut tail: Vec<u8> = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match source.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut sink) = sink.lock() {
                        let _ = sink.write_all(&buf[..n]);
                        let _ = sink.flush();
                    }
                    tail.extend_from_slice(&buf[..n]);
                    if tail.len() > TAIL_BYTES * 2 {
                        tail.drain(..tail.len() - TAIL_BYTES);
                    }
                }
            }
        }
        if tail.len() > TAIL_BYTES {
            tail.drain(..tail.len() - TAIL_BYTES);
        }
        tail
    }))
}

fn exit_code(code: Option<i32>) -> ExitCode {
    match code {
        Some(c) => ExitCode::from(u8::try_from(c & 0xff).unwrap_or(1)),
        None => ExitCode::FAILURE,
    }
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default()
}

/// The command as a shell would show it, for classification and the log.
pub fn shell_join(words: &[String]) -> String {
    if let [single] = words {
        return single.clone();
    }
    words
        .iter()
        .map(|w| {
            let plain = !w.is_empty()
                && w.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_./:=@%+,-".contains(c));
            if plain {
                w.clone()
            } else {
                format!("'{}'", w.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_words_with_shell_quoting() {
        let words = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            shell_join(&words(&["cargo", "test", "-p", "x"])),
            "cargo test -p x"
        );
        assert_eq!(
            shell_join(&words(&["pytest", "-k", "a and b"])),
            "pytest -k 'a and b'"
        );
        assert_eq!(
            shell_join(&words(&["cargo test | tail"])),
            "cargo test | tail"
        );
        assert_eq!(shell_join(&words(&["echo", "it's"])), r"echo 'it'\''s'");
    }
}
