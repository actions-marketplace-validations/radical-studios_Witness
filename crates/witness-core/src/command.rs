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

/// Commands that never change files (given no output redirection).
const READ_ONLY: &[&str] = &[
    "ls",
    "cat",
    "head",
    "tail",
    "less",
    "more",
    "wc",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "ack",
    "fd",
    "tree",
    "pwd",
    "echo",
    "printf",
    "which",
    "whereis",
    "type",
    "file",
    "stat",
    "du",
    "df",
    "printenv",
    "date",
    "whoami",
    "id",
    "uname",
    "hostname",
    "true",
    "false",
    "test",
    "[",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "diff",
    "cmp",
    "uniq",
    "cut",
    "tr",
    "column",
    "jq",
    "xxd",
    "hexdump",
    "od",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "shasum",
    "ps",
    "sleep",
    "cd",
    "pushd",
    "popd",
    "export",
    "unset",
    "set",
    "nproc",
    "uptime",
];

/// `git` subcommands that leave the working tree alone.
const GIT_READ_ONLY: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "rev-parse",
    "ls-files",
    "ls-tree",
    "blame",
    "grep",
    "describe",
    "shortlog",
    "reflog",
    "cat-file",
    "merge-base",
    "rev-list",
    "for-each-ref",
    "show-ref",
    "name-rev",
    "config",
    "remote",
    "branch",
    "tag",
    "fetch",
    "push",
    "add",
    "commit",
    "notes",
    "var",
    "version",
    "help",
];

/// `git` subcommands that replace working-tree files with other content
/// (another branch, a stash, someone else's commits).
const GIT_MOVES_WORKTREE: &[&str] = &[
    "checkout",
    "switch",
    "reset",
    "stash",
    "pull",
    "merge",
    "rebase",
    "cherry-pick",
    "revert",
    "am",
    "bisect",
    "worktree",
    "restore",
];

/// Whether a command line moves the working tree to other content, e.g.
/// `git checkout other-branch` or `git pull`. Files it changes are not the
/// agent's edits.
pub fn moves_worktree(command: &str) -> bool {
    split(command).iter().any(|(text, _)| {
        let words: Vec<&str> = strip_prefixes(text).split_whitespace().collect();
        words
            .first()
            .is_some_and(|w| w.rsplit('/').next() == Some("git"))
            && git_subcommand(&words).is_some_and(|sub| GIT_MOVES_WORKTREE.contains(&sub))
    })
}

/// The subcommand of `git [global options] <subcommand> ...`.
fn git_subcommand<'a>(words: &[&'a str]) -> Option<&'a str> {
    let mut rest = words.iter().skip(1);
    while let Some(w) = rest.next() {
        match *w {
            // Global options that take a separate value.
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" => {
                rest.next();
            }
            w if w.starts_with('-') => {}
            w => return Some(w),
        }
    }
    None
}

/// Whether a command line can't change files in the working tree, so its
/// effect needs no snapshot. Conservative: anything unrecognized counts as
/// a possible writer.
pub fn is_read_only(command: &str) -> bool {
    let parts = split(command);
    !parts.is_empty()
        && parts.iter().all(|(text, _)| {
            let simple = strip_prefixes(text);
            !writes_through_redirect(simple) && simple_is_read_only(simple)
        })
}

fn simple_is_read_only(simple: &str) -> bool {
    let words: Vec<&str> = simple.split_whitespace().collect();
    let Some(&first) = words.first() else {
        return true;
    };
    let program = first.rsplit('/').next().unwrap_or(first);
    let has = |flag: &str| {
        words
            .iter()
            .any(|w| *w == flag || w.starts_with(&format!("{flag}=")))
    };
    match program {
        "git" => git_subcommand(&words).is_some_and(|sub| GIT_READ_ONLY.contains(&sub)),
        "find" => ![
            "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fls",
        ]
        .iter()
        .any(|f| has(f)),
        "sed" => !words
            .iter()
            .any(|w| w.starts_with("-i") || w.starts_with("--in-place")),
        "sort" => !has("-o") && !has("--output"),
        "witness" => words.get(1).is_some_and(|w| {
            [
                "report",
                "status",
                "explain",
                "sessions",
                "assume",
                "doctor",
                "--version",
            ]
            .contains(w)
        }),
        p => READ_ONLY.contains(&p),
    }
}

/// Whether a simple command redirects output into a file (anything but a
/// file descriptor or `/dev/null`).
fn writes_through_redirect(simple: &str) -> bool {
    let chars: Vec<char> = simple.chars().collect();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    i += 1;
                }
            }
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '\\' => i += 1,
            None if c == '>' => {
                let mut j = i + 1;
                if chars.get(j) == Some(&'>') || chars.get(j) == Some(&'|') {
                    j += 1;
                }
                if chars.get(j) == Some(&'&') {
                    // `>&2`, `2>&1`: duplicating a descriptor.
                    i = j + 1;
                    continue;
                }
                while chars.get(j).is_some_and(|c| c.is_whitespace()) {
                    j += 1;
                }
                let target: String = chars[j..]
                    .iter()
                    .take_while(|c| !c.is_whitespace())
                    .collect();
                if target != "/dev/null" {
                    return true;
                }
                i = j;
                continue;
            }
            None => {}
        }
        i += 1;
    }
    false
}

/// Classification of a whole command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandVerdict {
    pub class: CommandClass,
    /// Whether the command line's final exit code reflects the classified
    /// part: true when only `&&` follows it.
    pub exit_attributable: bool,
    /// The classified simple command, without assignments and wrappers.
    pub simple: String,
    /// Directories `cd` moved to before it, in order.
    pub cd: Vec<String>,
}

/// Classifies a command line: the strongest class among its simple
/// commands wins; for ties, the last one.
pub fn classify(config: &Config, command: &str) -> CommandVerdict {
    let parts = split(command);
    let mut best: Option<(CommandClass, usize)> = None;
    for (idx, (text, _)) in parts.iter().enumerate() {
        let simple = strip_prefixes(text);
        // `witness exec` records the command it runs itself, with the exact
        // exit code; counting the wrapper too would double it.
        let class = if is_witness_exec(simple) {
            CommandClass::Other
        } else {
            config.classify_simple(simple)
        };
        if best.is_none_or(|(c, _)| class >= c) {
            best = Some((class, idx));
        }
    }
    let Some((class, idx)) = best else {
        return CommandVerdict {
            class: CommandClass::Other,
            exit_attributable: false,
            simple: String::new(),
            cd: Vec::new(),
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
    let cd = parts[..idx]
        .iter()
        .filter_map(|(text, _)| {
            let words = shell_words(strip_prefixes(text));
            match words.as_slice() {
                [cd, dir] if cd == "cd" => Some(dir.clone()),
                _ => None,
            }
        })
        .collect();
    CommandVerdict {
        class,
        exit_attributable,
        simple: strip_prefixes(&parts[idx].0).to_owned(),
        cd,
    }
}

/// Splits a simple command into words, removing quotes.
pub fn shell_words(simple: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = simple.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some('"') if c == '\\' => {
                if let Some(n) = chars.next() {
                    current.push(n);
                }
            }
            Some(_) => current.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                in_word = true;
            }
            None if c == '\\' => {
                if let Some(n) = chars.next() {
                    current.push(n);
                    in_word = true;
                }
            }
            None if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            None => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(current);
    }
    words
}

fn is_witness_exec(simple: &str) -> bool {
    let mut words = simple.split_whitespace();
    words
        .next()
        .is_some_and(|w| w.rsplit('/').next() == Some("witness"))
        && words.next() == Some("exec")
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
    fn reports_the_classified_part_and_cds() {
        let v = classify(
            &Config::default(),
            "cd crates/x && RUST_LOG=1 cargo test -p x 2>&1 | tail",
        );
        assert_eq!(v.simple, "cargo test -p x 2>&1");
        assert_eq!(v.cd, vec!["crates/x"]);
    }

    #[test]
    fn splits_words() {
        assert_eq!(
            shell_words(r#"pytest -k "a and b" 'x y' z\ w"#),
            vec!["pytest", "-k", "a and b", "x y", "z w"]
        );
        assert_eq!(shell_words(""), Vec::<String>::new());
        assert_eq!(shell_words("a ''"), vec!["a", ""]);
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
    fn read_only_commands() {
        for cmd in [
            "ls -la",
            "cat src/lib.rs | head -20",
            "rg -n foo src/ && git status",
            "git log --oneline -5",
            "git diff HEAD~1 > /dev/null 2>&1",
            "grep -r x . 2>&1",
            "find . -name '*.rs'",
            "sed -n 1,20p src/lib.rs",
            "cd crates/x && git diff",
            "echo 'a > b'",
            "witness status",
        ] {
            assert!(is_read_only(cmd), "{cmd}");
        }
        for cmd in [
            "cargo test",
            "echo hi > notes.txt",
            "cat a >> b",
            "sed -i 's/a/b/' src/lib.rs",
            "find . -name '*.tmp' -delete",
            "git checkout main",
            "git stash",
            "rm -rf target",
            "python gen.py",
            "ls | tee files.txt",
            "witness exec -- cargo test",
            "",
        ] {
            assert!(!is_read_only(cmd), "{cmd}");
        }
    }

    #[test]
    fn worktree_moving_git_commands() {
        for cmd in [
            "git checkout main",
            "git -C . switch -c x",
            "git stash && cargo test",
            "git pull --rebase",
            "git restore src/a.rs",
        ] {
            assert!(moves_worktree(cmd), "{cmd}");
        }
        for cmd in [
            "git status",
            "git apply fix.patch",
            "cargo test",
            "git commit -am x",
        ] {
            assert!(!moves_worktree(cmd), "{cmd}");
        }
    }

    #[test]
    fn witness_exec_wrapper_is_not_counted() {
        assert_eq!(
            verdict("witness exec -- cargo test | tail -5").0,
            CommandClass::Other
        );
        assert_eq!(
            verdict("/usr/local/bin/witness exec cargo test").0,
            CommandClass::Other
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
