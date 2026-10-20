//! `witness.toml`.

use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::record::CommandClass;

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
    pub areas: Vec<AreaConfig>,
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
}

impl Default for ReportConfig {
    fn default() -> Self {
        Self { max_flagged: 10 }
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
            ]),
            build: own(&[
                r"^cargo (\+\S+ )?(build|check)\b",
                r"^(npm|pnpm|yarn|bun)( run)? build\b",
                r"^(npx )?tsc\b",
                r"^go build\b",
                r"^(\./)?gradlew?( \S+)* (\S*:)?(build|assemble|compile\w*)\b",
                r"^(\./)?mvnw?( \S+)* (compile|package|install)\b",
            ]),
            lint: own(&[
                r"^cargo (clippy|fmt)\b",
                r"^(npm|pnpm|yarn|bun)( run)? (lint|format)\b",
                r"^(npx )?(eslint|prettier|biome)\b",
                r"^(ruff|flake8|pylint|mypy|black)\b",
                r"^go vet\b",
                r"^golangci-lint\b",
                r"^gofmt\b",
            ]),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AreaConfig {
    pub name: String,
    pub paths: Vec<String>,
}

/// Validated configuration with compiled patterns.
#[derive(Debug, Clone)]
pub struct Config {
    pub raw: RawConfig,
    test: Vec<Regex>,
    build: Vec<Regex>,
    lint: Vec<Regex>,
    areas: Vec<(String, GlobSet)>,
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
            areas.push((area.name.clone(), set));
        }
        Ok(Self {
            raw,
            test,
            build,
            lint,
            areas,
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
            .find(|(_, set)| set.is_match(path))
            .map_or(DEFAULT_AREA, |(name, _)| name.as_str())
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
            areas: Vec::new(),
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

# Areas group paths for the agent track record (not computed yet).
# The first matching area wins; unmatched paths fall in "default".
# [[areas]]
# name = "core"
# paths = ["src/core/**"]
"#,
        max = ReportConfig::default().max_flagged,
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
    fn bad_pattern_is_reported() {
        let err = Config::parse("[commands]\ntest = ['(']").unwrap_err();
        assert!(matches!(err, ConfigError::Pattern { class: "test", .. }));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::parse("[report]\nmax_flaged = 3").is_err());
    }
}
