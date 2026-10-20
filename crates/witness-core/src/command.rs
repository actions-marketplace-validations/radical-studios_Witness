//! Shell command classification and secret redaction.

use std::sync::LazyLock;

use regex::Regex;

use crate::config::Config;
use crate::record::{CommandClass, MAX_COMMAND_CHARS, truncate_chars};

/// What separates a simple command from the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Separator {
    /// `&&`: the next command runs only on success, so a final exit 0
    /// implies this one succeeded.
    And,
    /// `|` or `|&`: the final exit code is the last stage's, unless
    /// `pipefail` is set.
    Pipe,
    /// `||`, `;`, newline or a trailing `&`: the final exit code says
    /// nothing about this command.
    Other,
}

/// Splits a command line into simple commands, respecting quotes.
pub fn split(command: &str) -> Vec<(String, Option<Separator>)> {
    let chars: Vec<char> = command.chars().collect();
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if let Some(q) = quote {
            current.push(c);
            if c == '\\' && q == '"' {
                if let Some(n) = next {
                    current.push(n);
                    i += 1;
                }
            } else if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        let sep = match (c, next) {
            ('\\', Some(n)) => {
                current.push(c);
                current.push(n);
                i += 2;
                continue;
            }
            ('\'' | '"', _) => {
                quote = Some(c);
                current.push(c);
                i += 1;
                continue;
            }
            ('&', Some('&')) => Some((Separator::And, 2)),
            ('|', Some('|')) => Some((Separator::Other, 2)),
            ('|', Some('&')) => Some((Separator::Pipe, 2)),
            ('|', _) => Some((Separator::Pipe, 1)),
            (';' | '\n', _) => Some((Separator::Other, 1)),
            ('&', Some('>')) => None,
            ('&', _) if current.ends_with('>') => None,
            ('&', _) => Some((Separator::Other, 1)),
            _ => None,
        };
        match sep {
            Some((kind, len)) => {
                parts.push((current.trim().to_owned(), Some(kind)));
                current.clear();
                i += len;
            }
            None => {
                current.push(c);
                i += 1;
            }
        }
    }
    parts.push((current.trim().to_owned(), None));
    parts.retain(|(text, _)| !text.is_empty());
    // A trailing separator (e.g. `cmd &`) still applies to the last part.
    parts
}

static ASSIGNMENT: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*=(\S*)\s+").ok());
static WRAPPER: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"^(timeout( -\S+)* \d+[smhd]?|time( -p)?|env( -\S+)*|nice( -n -?\d+)?|nohup|command|exec)\s+",
    )
    .ok()
});

/// Removes leading `VAR=value` assignments and wrapper commands.
pub fn strip_prefixes(simple: &str) -> &str {
    let mut rest = simple.trim().trim_start_matches(['(', '{']).trim_start();
    loop {
        let before = rest.len();
        for re in [&*ASSIGNMENT, &*WRAPPER].into_iter().flatten() {
            if let Some(m) = re.find(rest) {
                rest = rest[m.end()..].trim_start();
            }
        }
        if rest.len() == before {
            return rest;
        }
    }
}

/// `set -o pipefail`, `set -euo pipefail` and the like.
fn is_pipefail(simple: &str) -> bool {
    let words: Vec<&str> = simple.split_whitespace().collect();
    words.first() == Some(&"set")
        && words.contains(&"pipefail")
        && words
            .iter()
            .any(|w| w.starts_with('-') && !w.starts_with("--") && w.ends_with('o'))
}

/// Classification of a whole command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandVerdict {
    pub class: CommandClass,
    /// Whether the command line's final exit code reflects the classified
    /// part: true when only `&&` follows it.
    pub exit_attributable: bool,
}

/// Classifies a command line: the strongest class among its simple
/// commands wins; for ties, the last one.
pub fn classify(config: &Config, command: &str) -> CommandVerdict {
    let parts = split(command);
    let mut best: Option<(CommandClass, usize)> = None;
    for (idx, (text, _)) in parts.iter().enumerate() {
        let class = config.classify_simple(strip_prefixes(text));
        if best.is_none_or(|(c, _)| class >= c) {
            best = Some((class, idx));
        }
    }
    let Some((class, idx)) = best else {
        return CommandVerdict {
            class: CommandClass::Other,
            exit_attributable: false,
        };
    };
    // With `set -o pipefail` earlier in the line, a failing stage fails the
    // whole pipeline, so a following `|` keeps the exit code meaningful.
    let pipefail = parts[..idx].iter().any(|(text, _)| is_pipefail(text));
    let exit_attributable = parts[idx..].iter().all(|(_, sep)| match sep {
        None | Some(Separator::And) => true,
        Some(Separator::Pipe) => pipefail,
        Some(Separator::Other) => false,
    });
    CommandVerdict {
        class,
        exit_attributable,
    }
}

const REDACTED: &str = "[REDACTED]";

/// Key names whose value is a secret. The secret word must end the key, so
/// `--author` or `TOKEN_FILE` are left alone.
const SECRET_KEY: &str = r"[A-Za-z0-9_.-]*(?i:token|secret|secret[_-]?key|access[_-]?key|passw(?:or)?d|pwd|api[_-]?key|apikey|auth|authorization|credentials?|private[_-]?key)";

static KEY_VALUE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // KEY=value, KEY: value
        format!(r#"(?P<pre>\b{SECRET_KEY}\s*[=:]\s*)(?P<val>"[^"]*"|'[^']*'|[^\s'"&;|]+)"#),
        // --token value, --password=value
        format!(r#"(?P<pre>(?:^|\s)--?{SECRET_KEY}[= ])(?P<val>"[^"]*"|'[^']*'|[^\s'"&;|]+)"#),
    ]
    .iter()
    .filter_map(|re| Regex::new(re).ok())
    .collect()
});

static SECRET_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        // Credentials in URLs: scheme://user:pass@host
        (
            r"(?P<pre>[A-Za-z][A-Za-z0-9+.-]*://[^\s:/@]+:)[^\s@/]+(?P<post>@)",
            "${pre}[REDACTED]${post}",
        ),
        // Authorization header values
        (
            r"(?i)(?P<pre>\b(?:bearer|basic)\s+)[A-Za-z0-9._~+/=-]+",
            "${pre}[REDACTED]",
        ),
        // Well-known token shapes
        (r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", REDACTED),
        (r"\bgh[pousr]_[A-Za-z0-9]{20,}\b", REDACTED),
        (r"\bgithub_pat_[A-Za-z0-9_]{20,}\b", REDACTED),
        (r"\bglpat-[A-Za-z0-9_-]{20,}\b", REDACTED),
        (r"\bxox[abprs]-[A-Za-z0-9-]{10,}\b", REDACTED),
        (r"\bsk-[A-Za-z0-9_-]{20,}\b", REDACTED),
        (
            r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b",
            REDACTED,
        ),
    ]
    .into_iter()
    .filter_map(|(re, rep)| Regex::new(re).ok().map(|r| (r, rep)))
    .collect()
});

/// Replaces anything that looks like a secret with `[REDACTED]`.
pub fn redact(text: &str) -> String {
    let mut out = text.to_owned();
    for (re, rep) in SECRET_PATTERNS.iter() {
        out = re.replace_all(&out, *rep).into_owned();
    }
    for re in KEY_VALUE_PATTERNS.iter() {
        out = re
            .replace_all(&out, |caps: &regex::Captures<'_>| {
                let pre = caps.name("pre").map_or("", |m| m.as_str());
                let val = caps.name("val").map_or("", |m| m.as_str());
                // Already handled by a more specific pattern above.
                let keep = val.starts_with(REDACTED)
                    || val.eq_ignore_ascii_case("bearer")
                    || val.eq_ignore_ascii_case("basic");
                if keep {
                    format!("{pre}{val}")
                } else {
                    format!("{pre}{REDACTED}")
                }
            })
            .into_owned();
    }
    out
}

/// Redacts, then truncates to the stored length.
pub fn sanitize_command(command: &str) -> String {
    truncate_chars(&redact(command.trim()), MAX_COMMAND_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(cmd: &str) -> (CommandClass, bool) {
        let v = classify(&Config::default(), cmd);
        (v.class, v.exit_attributable)
    }

    #[test]
    fn splits_on_operators_but_not_inside_quotes_or_redirects() {
        let parts = split(r#"cd app && echo "a && b" | grep 'x;y' ; cargo test 2>&1 &> out.txt"#);
        let texts: Vec<_> = parts.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "cd app",
                r#"echo "a && b""#,
                "grep 'x;y'",
                "cargo test 2>&1 &> out.txt"
            ]
        );
        assert_eq!(parts[0].1, Some(Separator::And));
        assert_eq!(parts[1].1, Some(Separator::Pipe));
        assert_eq!(parts[2].1, Some(Separator::Other));
        assert_eq!(parts[3].1, None);
    }

    proptest::proptest! {
        #[test]
        fn split_and_classify_never_panic(cmd in "\\PC{0,80}") {
            let _ = split(&cmd);
            let _ = classify(&Config::default(), &cmd);
            let _ = redact(&cmd);
        }

        #[test]
        fn and_chains_split_into_their_words(
            words in proptest::collection::vec("[a-z][a-z0-9 ./_-]{0,12}[a-z0-9]", 1..6),
        ) {
            let parts = split(&words.join(" && "));
            let texts: Vec<&str> = parts.iter().map(|(t, _)| t.as_str()).collect();
            let expected: Vec<&str> = words.iter().map(|w| w.trim()).collect();
            proptest::prop_assert_eq!(texts, expected);
            proptest::prop_assert!(parts[..parts.len() - 1].iter().all(|(_, s)| *s == Some(Separator::And)));
        }
    }

    #[test]
    fn strips_env_assignments_and_wrappers() {
        assert_eq!(
            strip_prefixes("RUST_LOG=debug FOO=1 cargo test"),
            "cargo test"
        );
        assert_eq!(strip_prefixes("timeout 300 cargo test"), "cargo test");
        assert_eq!(
            strip_prefixes("time env -i nice -n 5 go test ./..."),
            "go test ./..."
        );
        assert_eq!(strip_prefixes("(cargo build)"), "cargo build)");
    }

    #[test]
    fn exit_code_is_attributable_only_through_and_chains() {
        assert_eq!(verdict("cargo test"), (CommandClass::Test, true));
        assert_eq!(
            verdict("cd crates/x && cargo test"),
            (CommandClass::Test, true)
        );
        assert_eq!(verdict("cargo test && echo ok"), (CommandClass::Test, true));
        assert_eq!(
            verdict("cargo test 2>&1 | tail -20"),
            (CommandClass::Test, false)
        );
        assert_eq!(verdict("cargo test || true"), (CommandClass::Test, false));
        assert_eq!(
            verdict("cargo test; echo done"),
            (CommandClass::Test, false)
        );
        assert_eq!(verdict("cargo test &"), (CommandClass::Test, false));
        assert_eq!(
            verdict("set -o pipefail; cargo test 2>&1 | tail -20"),
            (CommandClass::Test, true)
        );
        assert_eq!(
            verdict("set -euo pipefail && cargo test | tail"),
            (CommandClass::Test, true)
        );
        assert_eq!(
            verdict("set -e; cargo test | tail"),
            (CommandClass::Test, false)
        );
    }

    #[test]
    fn strongest_class_wins() {
        assert_eq!(verdict("cargo build && cargo test").0, CommandClass::Test);
        assert_eq!(
            verdict("cargo clippy && cargo build").0,
            CommandClass::Build
        );
        assert_eq!(verdict("ls && git status").0, CommandClass::Other);
        assert_eq!(verdict("").0, CommandClass::Other);
    }

    #[test]
    fn redacts_secret_shapes() {
        let cases = [
            (
                "API_TOKEN=abc123 cargo test",
                "API_TOKEN=[REDACTED] cargo test",
            ),
            (
                "export DB_PASSWORD='hunter2'",
                "export DB_PASSWORD=[REDACTED]",
            ),
            (
                r#"curl -H "Authorization: Bearer abc.def.ghi" https://x"#,
                r#"curl -H "Authorization: Bearer [REDACTED]" https://x"#,
            ),
            (
                "git clone https://bob:s3cret@example.com/r.git",
                "git clone https://bob:[REDACTED]@example.com/r.git",
            ),
            (
                "mytool --api-key 12345 run",
                "mytool --api-key [REDACTED] run",
            ),
            ("mytool --password=xyz", "mytool --password=[REDACTED]"),
            ("echo AKIAIOSFODNN7EXAMPLE", "echo [REDACTED]"),
            (
                "gh auth login --with-token ghp_abcdefghijklmnopqrstuvwxyz0123",
                "gh auth login --with-token [REDACTED]",
            ),
            ("OPENAI=sk-proj-abcdefghijklmnopqrstuv", "OPENAI=[REDACTED]"),
        ];
        for (input, expected) in cases {
            assert_eq!(redact(input), expected, "{input}");
        }
    }

    #[test]
    fn leaves_ordinary_commands_alone() {
        for cmd in [
            "cargo test -p witness-core",
            "git log --oneline -5",
            "npm run build",
            "grep -rn tokenize src/",
            "git commit --author=\"Ada <ada@example.com>\" -m wip",
            "TOKEN_FILE=path cargo test",
        ] {
            assert_eq!(redact(cmd), cmd);
        }
    }

    #[test]
    fn sanitize_truncates_after_redaction() {
        let long = format!("echo {}", "a".repeat(300));
        assert_eq!(sanitize_command(&long).chars().count(), MAX_COMMAND_CHARS);
    }
}
