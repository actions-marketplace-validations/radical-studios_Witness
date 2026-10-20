//! Building the agent track record from git history.
//!
//! Landed changes are the commits on the base branch's first-parent chain
//! that changed `.witness/sessions/` (the merge or squash commits of agent
//! work). Each is analyzed like a report on `commit^..commit`, and its
//! agent-written sections are judged `[history] horizon_days` after it
//! landed (see `witness_core::history`). Results are cached in
//! `.witness/state/history.json`: a change is analyzed once and judged once,
//! after its horizon has passed.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use witness_core::classify::Status;
use witness_core::history::{History, HistoryEntry, Outcome, Tally, percent, survival};
use witness_core::provenance::is_neutral;
use witness_core::record::line_hash;
use witness_core::report;
use witness_git::Git;

use crate::cli::StatsArgs;
use crate::pipeline::{self, Repo, Request};
use crate::review::resolve_base;
use crate::workspace::{SESSIONS_DIR, STATE_DIR};

const CACHE_FILE: &str = "history.json";
const CACHE_VERSION: u32 = 1;
const DAY: i64 = 24 * 60 * 60;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    version: u32,
    /// The settings outcomes depend on; when they change the cache is
    /// rebuilt.
    horizon_days: u32,
    prefix: String,
    changes: Vec<Change>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Change {
    commit: String,
    /// Committer time, Unix seconds.
    landed_at: i64,
    /// Whether the sections have been judged.
    judged: bool,
    sections: Vec<CachedSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedSection {
    file: String,
    agent: String,
    status: Status,
    lines: usize,
    /// Distinct line hashes of the section's distinctive lines.
    hashes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome: Option<Outcome>,
}

/// The track record and how it was obtained.
#[derive(Debug, Default)]
pub struct Built {
    pub history: History,
    /// Landed changes still inside their horizon.
    pub pending: usize,
    /// Why the record may be incomplete.
    pub notes: Vec<String>,
}

/// The track record for changes landed on `base`. With `update`, new
/// changes are analyzed and due ones judged (this reads git history and can
/// take a while the first time); otherwise only the cache is read.
pub fn load(repo: &Repo, base: &str, update: bool) -> Result<Built> {
    let settings = &repo.config.raw.history;
    if !settings.enabled {
        return Ok(Built::default());
    }
    let path = cache_path(repo);
    let mut cache: Cache = fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .filter(|c: &Cache| {
            c.version == CACHE_VERSION
                && c.horizon_days == settings.horizon_days
                && c.prefix == repo.prefix
        })
        .unwrap_or_else(|| Cache {
            version: CACHE_VERSION,
            horizon_days: settings.horizon_days,
            prefix: repo.prefix.clone(),
            changes: Vec::new(),
        });

    let mut notes = Vec::new();
    if update {
        if repo.git.is_shallow() {
            notes.push(
                "shallow clone: the track record was not updated (fetch full history, e.g. \
                 fetch-depth: 0)"
                    .to_owned(),
            );
        } else {
            refresh(repo, base, &mut cache)?;
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(&path, serde_json::to_string(&cache)?)
                .with_context(|| format!("writing {}", path.display()))?;
        }
    }

    let mut entries = Vec::new();
    let mut pending = 0;
    for change in &cache.changes {
        if !change.judged {
            pending += usize::from(!change.sections.is_empty());
            continue;
        }
        let landed_at = OffsetDateTime::from_unix_timestamp(change.landed_at)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH);
        for s in &change.sections {
            let Some(outcome) = s.outcome else { continue };
            entries.push(HistoryEntry {
                commit: change.commit.clone(),
                landed_at,
                file: s.file.clone(),
                agent: s.agent.clone(),
                area: repo.config.area_for(&s.file).to_owned(),
                status: s.status,
                lines: s.lines,
                outcome,
            });
        }
    }
    Ok(Built {
        history: History {
            horizon_days: cache.horizon_days,
            entries,
        },
        pending,
        notes,
    })
}

fn cache_path(repo: &Repo) -> PathBuf {
    repo.root.join(STATE_DIR).join(CACHE_FILE)
}

/// Brings `cache` up to date with the changes landed on `base`.
fn refresh(repo: &Repo, base: &str, cache: &mut Cache) -> Result<()> {
    let git = &repo.git;
    let sessions = format!("{}{SESSIONS_DIR}", repo.prefix);
    let max = repo.config.raw.history.max_changes;
    let landings = git
        .first_parent(base, Some(&sessions), Some(max))
        .with_context(|| format!("reading the history of {base}"))?;
    let tip_time = git
        .first_parent(base, None, Some(1))?
        .first()
        .map_or(0, |(_, t)| *t);

    let mut known: HashMap<String, Change> = cache
        .changes
        .drain(..)
        .map(|c| (c.commit.clone(), c))
        .collect();
    let mut changes = Vec::with_capacity(landings.len());
    for (commit, landed_at) in landings {
        let change = match known.remove(&commit) {
            Some(change) => change,
            None => Change {
                sections: analyze(repo, &commit).unwrap_or_default(),
                commit,
                landed_at,
                judged: false,
            },
        };
        changes.push(change);
    }

    let horizon = i64::from(cache.horizon_days) * DAY;
    let mut reverts: Option<Vec<(String, i64)>> = None;
    for change in &mut changes {
        if change.judged || change.sections.is_empty() {
            change.judged |= change.sections.is_empty();
            continue;
        }
        let due = change.landed_at + horizon;
        if tip_time < due {
            continue;
        }
        let Some(at) = git.first_parent_at(base, due)? else {
            continue;
        };
        let reverts = match &mut reverts {
            Some(r) => &*r,
            None => reverts.insert(git.reverts(base)?),
        };
        let reverted = reverts
            .iter()
            .any(|(sha, t)| change.commit.starts_with(sha.as_str()) && *t <= due);
        let renames: HashMap<String, String> =
            git.renames(&change.commit, &at)?.into_iter().collect();
        let mut texts: HashMap<String, Option<String>> = HashMap::new();
        for s in &mut change.sections {
            let path = format!("{}{}", repo.prefix, s.file);
            let path = renames.get(&path).cloned().unwrap_or(path);
            if !texts.contains_key(&path) {
                let text = git.show_file(&at, &path)?;
                texts.insert(path.clone(), text);
            }
            let text = texts.get(&path).and_then(Option::as_deref);
            s.outcome = Some(Outcome::judge(survival(&s.hashes, text), reverted));
        }
        change.judged = true;
    }
    cache.changes = changes;
    Ok(())
}

/// The agent-written sections of the change `commit` landed.
fn analyze(repo: &Repo, commit: &str) -> Result<Vec<CachedSection>> {
    let parent = format!("{commit}^");
    let req = Request {
        base: &parent,
        head: commit,
        prefix: &repo.prefix,
        worktree: None,
        diff: None,
    };
    let gathered = pipeline::gather(&repo.git, &req)?;
    if gathered.sessions.is_empty() {
        return Ok(Vec::new());
    }
    let analysis = pipeline::with_input(
        &repo.git,
        &req,
        &repo.config,
        &History::default(),
        gathered,
        |input, _| report::analyze(input),
    );
    let mut sections = Vec::new();
    for (segment, section) in analysis.segments.iter().zip(&analysis.sections) {
        let Some((section, _)) = section else {
            continue;
        };
        let Some(agent) = &section.agent else {
            continue;
        };
        if section.status == Status::Human {
            continue;
        }
        let mut seen = HashSet::new();
        let hashes: Vec<String> = segment
            .added
            .iter()
            .map(|line| line.trim())
            .filter(|line| !is_neutral(line))
            .map(line_hash)
            .filter(|h| seen.insert(h.clone()))
            .collect();
        if hashes.is_empty() {
            continue;
        }
        sections.push(CachedSection {
            file: section.file.clone(),
            agent: agent.clone(),
            status: section.status,
            lines: segment.added.len(),
            hashes,
            outcome: None,
        });
    }
    Ok(sections)
}

/// `witness stats`: the track record, as a table or JSON.
pub fn run_stats(args: StatsArgs) -> Result<()> {
    let repo = Repo::open()?;
    let base = resolve_base(&repo.git, args.base.as_deref())?;
    if !repo.config.raw.history.enabled {
        println!("The track record is turned off ([history] enabled = false in witness.toml).");
        return Ok(());
    }
    let built = load(&repo, &base, !args.cached)?;
    for note in &built.notes {
        eprintln!("witness: {note}");
    }
    let h = &built.history;
    let calibration = h.calibration();
    if args.json {
        let tally = |t: &Tally| json!({"samples": t.samples, "rewritten": t.rewritten, "reverted": t.reverted});
        let out = json!({
            "base": base,
            "horizon_days": h.horizon_days,
            "changes": h.changes(),
            "pending_changes": built.pending,
            "overall": tally(&h.overall()),
            "records": h.track_records(),
            "by_status": h.by_status().iter().map(|(status, t)| {
                json!({"status": status, "samples": t.samples, "rewritten": t.rewritten, "reverted": t.reverted})
            }).collect::<Vec<_>>(),
            "calibration": {
                "flagged": tally(&calibration.flagged),
                "tested": tally(&calibration.tested),
                "sentence": calibration.sentence(h.horizon_days),
            },
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    let pending = match built.pending {
        0 => String::new(),
        1 => format!(" 1 more change is still within {} days.", h.horizon_days),
        n => format!(
            " {n} more changes are still within {} days.",
            h.horizon_days
        ),
    };
    if h.is_empty() {
        println!(
            "No track record yet: no agent change recorded by Witness landed on {base} more than \
             {} days ago.{pending}",
            h.horizon_days
        );
        return Ok(());
    }
    let overall = h.overall();
    println!(
        "Track record on {base}: {} agent-written sections from {} changes, judged {} days after \
         landing.{pending}",
        overall.samples,
        h.changes(),
        h.horizon_days
    );
    println!();
    let records = h.track_records();
    let agent_w = records
        .iter()
        .map(|r| r.agent.len())
        .max()
        .unwrap_or(0)
        .max(5);
    let area_w = records
        .iter()
        .map(|r| r.area.len())
        .max()
        .unwrap_or(0)
        .max(4);
    println!(
        "{:<agent_w$}  {:<area_w$}  {:>8}  {:>9}  {:>8}",
        "Agent", "Area", "Sections", "Rewritten", "Reverted"
    );
    let share = |n: u32, t: &Tally| percent(f64::from(n) / f64::from(t.samples.max(1)));
    for r in &records {
        println!(
            "{:<agent_w$}  {:<area_w$}  {:>8}  {:>9}  {:>8}",
            r.agent,
            r.area,
            r.tally.samples,
            share(r.tally.rewritten, &r.tally),
            share(r.tally.reverted, &r.tally)
        );
    }
    println!();
    println!(
        "{:<17}  {:>8}  {:>21}",
        "Status at landing", "Sections", "Rewritten or reverted"
    );
    for (status, t) in h.by_status() {
        println!(
            "{:<17}  {:>8}  {:>21}",
            status.as_str(),
            t.samples,
            percent(t.churn())
        );
    }
    println!();
    match calibration.sentence(h.horizon_days) {
        Some(sentence) => println!("{sentence}"),
        None => println!(
            "Not enough samples yet to compare flagged sections with tested ones (10 of each \
             are needed)."
        ),
    }
    Ok(())
}
