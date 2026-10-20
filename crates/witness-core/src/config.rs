//! `witness.toml`.

use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::classify::Status;
use crate::record::CommandClass;
use crate::signals::SignalKind;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid witness.toml: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid {class} pattern {pattern:?}: {source}")]
    Pattern {
        class: &'static str,
        pattern: String,
        source: regex::Error,
    },
    #[error("invalid glob {glob:?} in area {area:?}: {source}")]
    Glob {
        area: String,
        glob: String,
        source: globset::Error,
    },
}

/// The file as written on disk. Every section is optional.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RawConfig {
    pub report: ReportConfig,
    pub capture: CaptureConfig,
    pub commands: CommandPatterns,
    pub tests: TestsConfig,
    pub attribution: AttributionConfig,
    pub areas: Vec<AreaConfig>,
}

/// Which files are tests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestsConfig {
    /// Globs for test files. Edits to them get test-specific signals, such
    /// as a test changed right after it failed.
    pub paths: Vec<String>,
}

impl Default for TestsConfig {
    fn default() -> Self {
        Self {
            paths: [
                "**/tests/**",
                "**/test/**",
                "**/__tests__/**",
                "**/spec/**",
                "**/*_test.go",
                "**/test_*.py",
                "**/*_test.py",
                "**/*.test.*",
                "**/*.spec.*",
                "**/*_spec.rb",
                "**/*Test.java",
                "**/*Tests.cs",
                "**/src/test/**",
            ]
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
        }
    }
}

/// Recognizing commits made by agents that left no Witness record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AttributionConfig {
    /// A commit whose author, committer or message matches `pattern` is
    /// treated as written by `agent`. When the branch has no Witness record
    /// from that agent, its lines are reported as `unrecorded`.
    pub agent_commits: Vec<AgentCommit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCommit {
    pub agent: String,
    /// Regex matched against `author <email>`, `committer <email>` and the
    /// full commit message, one per line.
    pub pattern: String,
}

impl Default for AttributionConfig {
    fn default() -> Self {
        let rule = |agent: &str, pattern: &str| AgentCommit {
            agent: agent.to_owned(),
            pattern: pattern.to_owned(),
        };
        Self {
            agent_commits: vec![
                rule("claude-code", r"(?im)^co-authored-by:\s*claude\b"),
                rule(
                    "copilot",
                    r"(?im)^co-authored-by:\s*copilot\b|copilot-swe-agent",
                ),
                rule(
                    "codex",
                    r"(?im)^co-authored-by:\s*(openai )?codex\b|chatgpt-codex",
                ),
                rule(
                    "cursor",
                    r"(?im)^co-authored-by:\s*cursor\b|cursoragent@cursor\.com",
                ),
                rule("gemini-cli", r"(?im)^co-authored-by:\s*gemini\b"),
                rule("aider", r"(?im)^co-authored-by:\s*aider\b|\(aider\) <"),
                rule("devin", r"(?i)devin-ai-integration"),
            ],
        }
    }
}

/// How hooks observe file changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptureConfig {
    /// Snapshot the working tree around shell commands and tools without
    /// patch information, so their file changes are recorded.
    pub snapshots: bool,
    /// A snapshot slower than this turns snapshots off for the rest of the
    /// session (recorded as a note).
    pub snapshot_budget_ms: u64,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            snapshots: true,
            snapshot_budget_ms: 500,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReportConfig {
    /// Cap on how many sections the report flags for close review.
    pub max_flagged: usize,
    /// Statuses to review closely; the others are listed to skim.
    pub close: Vec<Status>,
    /// Signals that put a section up for close review whatever its status.
    pub escalate: Vec<SignalKind>,
}

impl Default for ReportConfig {
    fn default() -> Self {
        Self {
            max_flagged: 10,
            close: vec![
                Status::Failing,
                Status::Guessed,
                Status::Unrecorded,
                Status::Unverified,
            ],
            escalate: vec![
                SignalKind::TestChangedAfterFailure,
                SignalKind::AssertionsRemoved,
                SignalKind::HighRiskArea,
            ],
        }
    }
}

/// Regexes matched against each simple command (after stripping leading
/// `VAR=value` assignments and wrappers like `timeout 60`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandPatterns {
    pub test: Vec<String>,
    pub build: Vec<String>,
    pub lint: Vec<String>,
}

impl Default for CommandPatterns {
    fn default() -> Self {
        let own = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect();
        Self {
            test: own(&[
                r"^cargo (\+\S+ )?(test|nextest)\b",
                r"^(npm|pnpm|yarn|bun)( run)? test\b",
                r"^(npx |pnpm exec |yarn )?(jest|vitest|mocha)\b",
                r"^(python3? -m )?pytest\b",
                r"^(uv|poetry) run (python3? -m )?pytest\b",
                r"^go test\b",
                r"^(\./)?gradlew?( \S+)* (\S*:)?(test|check)\b",
                r"^(\./)?mvnw?( \S+)* (test|verify)\b",
                r"^make( \S+)* (test|check)\b",
                r"^just( \S+)* test\b",
                r"^(tox|nox)\b",
                r"^(bundle exec )?(rspec|rake (test|spec))\b",
                r"^dotnet test\b",
                r"^deno test\b",
                r"^mix test\b",
                r"^(\./)?(vendor/bin/)?(phpunit|pest)\b",
                r"^ctest\b",
                r"^swift test\b",
                r"^flutter test\b",
                r"^sbt( \S+)* test\b",
                r"^bazel(isk)? test\b",
                r"^zig build test\b",
            ]),
            build: own(&[
                r"^cargo (\+\S+ )?(build|check)\b",
                r"^(npm|pnpm|yarn|bun)( run)? build\b",
                r"^(npx )?tsc\b",
                r"^go build\b",
                r"^(\./)?gradlew?( \S+)* (\S*:)?(build|assemble|compile\w*)\b",
                r"^(\./)?mvnw?( \S+)* (compile|package|install)\b",
                r"^make\b",
                r"^dotnet build\b",
                r"^swift build\b",
                r"^bazel(isk)? build\b",
                r"^zig build\b",
                r"^mix compile\b",
                r"^(npx )?(vite|next|webpack) build\b",
            ]),
            lint: own(&[
                r"^cargo (clippy|fmt)\b",
                r"^(npm|pnpm|yarn|bun)( run)? (lint|format)\b",
                r"^(npx )?(eslint|prettier|biome)\b",
                r"^(ruff|flake8|pylint|mypy|black)\b",
                r"^go vet\b",
                r"^golangci-lint\b",
                r"^gofmt\b",
                r"^(rubocop|shellcheck|hadolint|stylelint|swiftlint)\b",
                r"^dotnet format\b",
                r"^mix (format|credo)\b",
            ]),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AreaConfig {
    pub name: String,
    pub paths: Vec<String>,
    /// `high` marks code where mistakes are costly (auth, payments,
    /// migrations): agent sections there are reviewed closely.
    #[serde(default)]
    pub risk: Risk,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    #[default]
    Normal,
    High,
}

/// Validated configuration with compiled patterns.
#[derive(Debug, Clone)]
pub struct Config {
    pub raw: RawConfig,
    test: Vec<Regex>,
    build: Vec<Regex>,
    lint: Vec<Regex>,
    agent_commits: Vec<(String, Regex)>,
    areas: Vec<(String, Risk, GlobSet)>,
    tests: GlobSet,
}

/// The configuration file, at the repo root.
pub const CONFIG_FILE: &str = "witness.toml";
/// Witness's own directory, at the repo root.
pub const WITNESS_DIR: &str = ".witness";

/// Whether a repo-relative path is Witness's own (its config or records),
/// which is never reported as a change.
pub fn is_witness_path(path: &str) -> bool {
    path == CONFIG_FILE
        || path
            .strip_prefix(WITNESS_DIR)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Name of the area a path falls in when no `[[areas]]` entry matches.
pub const DEFAULT_AREA: &str = "default";

impl Config {
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        Self::from_raw(toml::from_str(text)?)
    }

    pub fn from_raw(raw: RawConfig) -> Result<Self, ConfigError> {
        let compile = |class: &'static str, patterns: &[String]| {
            patterns
                .iter()
                .map(|p| {
                    Regex::new(p).map_err(|source| ConfigError::Pattern {
                        class,
                        pattern: p.clone(),
                        source,
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        };
        let test = compile("test", &raw.commands.test)?;
        let build = compile("build", &raw.commands.build)?;
        let lint = compile("lint", &raw.commands.lint)?;
        let agent_commits = raw
            .attribution
            .agent_commits
            .iter()
            .map(|rule| {
                Regex::new(&rule.pattern)
                    .map(|re| (rule.agent.clone(), re))
                    .map_err(|source| ConfigError::Pattern {
                        class: "attribution",
                        pattern: rule.pattern.clone(),
                        source,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut areas = Vec::with_capacity(raw.areas.len());
        for area in &raw.areas {
            let mut builder = GlobSetBuilder::new();
            for glob in &area.paths {
                let compiled = Glob::new(glob).map_err(|source| ConfigError::Glob {
                    area: area.name.clone(),
                    glob: glob.clone(),
                    source,
                })?;
                builder.add(compiled);
            }
            let set = builder.build().map_err(|source| ConfigError::Glob {
                area: area.name.clone(),
                glob: area.paths.join(", "),
                source,
            })?;
            areas.push((area.name.clone(), area.risk, set));
        }
        let mut tests = GlobSetBuilder::new();
        for glob in &raw.tests.paths {
            tests.add(Glob::new(glob).map_err(|source| ConfigError::Glob {
                area: "tests".to_owned(),
                glob: glob.clone(),
                source,
            })?);
        }
        let tests = tests.build().map_err(|source| ConfigError::Glob {
            area: "tests".to_owned(),
            glob: raw.tests.paths.join(", "),
            source,
        })?;
        Ok(Self {
            raw,
            test,
            build,
            lint,
            agent_commits,
            areas,
            tests,
        })
    }

    /// Classifies one simple command (already stripped of env assignments
    /// and wrappers).
    pub fn classify_simple(&self, simple: &str) -> CommandClass {
        let hit = |set: &[Regex]| set.iter().any(|r| r.is_match(simple));
        if hit(&self.test) {
            CommandClass::Test
        } else if hit(&self.build) {
            CommandClass::Build
        } else if hit(&self.lint) {
            CommandClass::Lint
        } else {
            CommandClass::Other
        }
    }

    /// First area whose globs match `path`, else [`DEFAULT_AREA`].
    pub fn area_for(&self, path: &str) -> &str {
        self.areas
            .iter()
            .find(|(_, _, set)| set.is_match(path))
            .map_or(DEFAULT_AREA, |(name, _, _)| name.as_str())
    }

    /// Risk of the first area whose globs match `path`.
    pub fn risk_for(&self, path: &str) -> Risk {
        self.areas
            .iter()
            .find(|(_, _, set)| set.is_match(path))
            .map_or(Risk::Normal, |(_, risk, _)| *risk)
    }

    /// Whether `path` is a test file (`[tests] paths`).
    pub fn is_test_file(&self, path: &str) -> bool {
        self.tests.is_match(path)
    }

    /// Whether sections with `status` are reviewed closely.
    pub fn closes(&self, status: Status) -> bool {
        self.raw.report.close.contains(&status)
    }

    /// Whether `signal` puts a section up for close review.
    pub fn escalates(&self, signal: SignalKind) -> bool {
        self.raw.report.escalate.contains(&signal)
    }

    /// The agent a commit is attributed to, from its author, committer and
    /// message (see [`AttributionConfig`]).
    pub fn agent_for_commit(&self, commit_text: &str) -> Option<&str> {
        self.agent_commits
            .iter()
            .find(|(_, re)| re.is_match(commit_text))
            .map(|(agent, _)| agent.as_str())
    }

    pub fn max_flagged(&self) -> usize {
        self.raw.report.max_flagged
    }
}

impl Default for Config {
    fn default() -> Self {
        // The built-in patterns are covered by tests, so this cannot fail in
        // practice; fall back to an empty matcher set rather than panic.
        Self::from_raw(RawConfig::default()).unwrap_or_else(|_| Self {
            raw: RawConfig::default(),
            test: Vec::new(),
            build: Vec::new(),
            lint: Vec::new(),
            agent_commits: Vec::new(),
            areas: Vec::new(),
            tests: GlobSet::empty(),
        })
    }
}

/// The `witness.toml` that `witness init` writes.
pub fn default_toml() -> String {
    let patterns = CommandPatterns::default();
    let list = |v: &[String]| {
        v.iter()
            .map(|p| format!("  '{p}',"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        r#"# Witness configuration. See docs/ARCHITECTURE.md.

[report]
# Cap on how many sections the report flags for close review.
max_flagged = {max}
# Statuses to review closely: failing, guessed, unrecorded, unverified,
# compiled, tested. The rest are listed to skim.
close = [{close}]
# Signals that put a section up for close review whatever its status:
# test-changed-after-failure, assertions-removed, high-risk-area, blind-edit,
# thrash, filtered-test-run, human-modified.
escalate = [{escalate}]

# Globs for test files, for test-specific signals.
[tests]
paths = [
{tests}
]

[capture]
# Snapshot the working tree around shell commands (and tools that report no
# patch) so files they change are recorded. Snapshots use a private git index
# under .witness/state/ and skip read-only commands.
snapshots = {snapshots}
# A snapshot slower than this (ms) turns snapshots off for the session.
snapshot_budget_ms = {budget}

# Regexes matched against each simple command the agent runs, after leading
# VAR=value assignments and wrappers (timeout, time, env, nice) are removed.
# A passing `test` or `build` run after an edit marks that edit as checked.
[commands]
test = [
{test}
]
build = [
{build}
]
lint = [
{lint}
]

# Commits recognized as written by an agent. When a branch has no Witness
# record from that agent, lines from its commits are flagged "unrecorded".
# Each pattern is matched against the commit's author, committer and message.
[attribution]
agent_commits = [
{attribution}
]

# Areas group paths for the agent track record. The first matching area
# wins; unmatched paths fall in "default". risk = "high" marks code where
# mistakes are costly: agent sections there are reviewed closely.
# [[areas]]
# name = "auth"
# paths = ["src/auth/**"]
# risk = "high"
"#,
        max = ReportConfig::default().max_flagged,
        close = ReportConfig::default()
            .close
            .iter()
            .map(|s| format!("\"{}\"", s.as_str()))
            .collect::<Vec<_>>()
            .join(", "),
        escalate = ReportConfig::default()
            .escalate
            .iter()
            .map(|s| format!("\"{}\"", s.as_str()))
            .collect::<Vec<_>>()
            .join(", "),
        tests = list(&TestsConfig::default().paths),
        attribution = AttributionConfig::default()
            .agent_commits
            .iter()
            .map(|r| format!(
                "  {{ agent = \"{}\", pattern = '{}' }},",
                r.agent, r.pattern
            ))
            .collect::<Vec<_>>()
            .join("\n"),
        snapshots = CaptureConfig::default().snapshots,
        budget = CaptureConfig::default().snapshot_budget_ms,
        test = list(&patterns.test),
        build = list(&patterns.build),
        lint = list(&patterns.lint),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_toml_parses_to_defaults() {
        let config = Config::parse(&default_toml()).unwrap();
        assert_eq!(config.raw, RawConfig::default());
    }

    #[test]
    fn empty_file_uses_defaults() {
        let config = Config::parse("").unwrap();
        assert_eq!(config.max_flagged(), 10);
        assert_eq!(config.classify_simple("cargo test"), CommandClass::Test);
    }

    #[test]
    fn default_patterns_cover_common_tools() {
        let c = Config::default();
        for cmd in [
            "cargo test -p witness-core",
            "cargo +nightly test",
            "cargo nextest run",
            "npm test",
            "npm run test -- --watch=false",
            "pnpm test",
            "yarn test",
            "npx vitest run",
            "pytest -q tests/",
            "python -m pytest",
            "go test ./...",
            "./gradlew test",
            "gradle :app:check",
            "mvn -q test",
            "./mvnw verify",
            "make test",
            "make -j4 check",
            "just test",
            "tox -e py312",
            "bundle exec rspec spec/models",
            "dotnet test",
            "mix test",
            "bazel test //...",
        ] {
            assert_eq!(c.classify_simple(cmd), CommandClass::Test, "{cmd}");
        }
        for cmd in [
            "cargo build --release",
            "cargo check",
            "npm run build",
            "pnpm build",
            "tsc --noEmit",
            "go build ./...",
            "./gradlew assemble",
            "mvn package",
            "make",
            "make all",
            "dotnet build",
        ] {
            assert_eq!(c.classify_simple(cmd), CommandClass::Build, "{cmd}");
        }
        for cmd in [
            "cargo clippy -- -D warnings",
            "cargo fmt --check",
            "npx eslint .",
            "ruff check",
            "go vet ./...",
        ] {
            assert_eq!(c.classify_simple(cmd), CommandClass::Lint, "{cmd}");
        }
        for cmd in [
            "ls -la",
            "git status",
            "cat Cargo.toml",
            "cargo add serde",
            "npm install",
        ] {
            assert_eq!(c.classify_simple(cmd), CommandClass::Other, "{cmd}");
        }
    }

    #[test]
    fn areas_match_first_glob() {
        let config = Config::parse(
            r#"
[[areas]]
name = "core"
paths = ["crates/witness-core/**"]

[[areas]]
name = "crates"
paths = ["crates/**"]
"#,
        )
        .unwrap();
        assert_eq!(config.area_for("crates/witness-core/src/lib.rs"), "core");
        assert_eq!(config.area_for("crates/witness/src/main.rs"), "crates");
        assert_eq!(config.area_for("README.md"), DEFAULT_AREA);
    }

    #[test]
    fn recognizes_agent_commits() {
        let c = Config::default();
        let commit = |author: &str, message: &str| format!("{author}\n{author}\n\n{message}");
        assert_eq!(
            c.agent_for_commit(&commit(
                "Ada <ada@example.com>",
                "Fix parser\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
            )),
            Some("claude-code")
        );
        assert_eq!(
            c.agent_for_commit(&commit(
                "copilot-swe-agent[bot] <198982749+Copilot@users.noreply.github.com>",
                "Add tests"
            )),
            Some("copilot")
        );
        assert_eq!(
            c.agent_for_commit(&commit("Ada (aider) <ada@example.com>", "aider: fix")),
            Some("aider")
        );
        assert_eq!(
            c.agent_for_commit(&commit("Ada <ada@example.com>", "Mention Claude in docs")),
            None
        );
    }

    #[test]
    fn test_files_and_risky_areas() {
        let c = Config::parse(
            "[[areas]]\nname = \"auth\"\npaths = [\"src/auth/**\"]\nrisk = \"high\"\n",
        )
        .unwrap();
        for test in [
            "tests/parse.rs",
            "crates/x/tests/a.rs",
            "pkg/parse_test.go",
            "app/test_models.py",
            "web/src/a.test.ts",
            "web/__tests__/b.js",
            "src/test/java/FooTest.java",
        ] {
            assert!(c.is_test_file(test), "{test}");
        }
        assert!(!c.is_test_file("src/lib.rs"));
        assert_eq!(c.risk_for("src/auth/token.rs"), Risk::High);
        assert_eq!(c.risk_for("src/lib.rs"), Risk::Normal);
        assert!(c.closes(Status::Failing));
        assert!(!c.closes(Status::Tested));
        assert!(c.escalates(SignalKind::HighRiskArea));
        assert!(!c.escalates(SignalKind::BlindEdit));
    }

    #[test]
    fn bad_pattern_is_reported() {
        let err = Config::parse("[commands]\ntest = ['(']").unwrap_err();
        assert!(matches!(err, ConfigError::Pattern { class: "test", .. }));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::parse("[report]\nmax_flaged = 3").is_err());
    }
}
