//! What part of the codebase a test or build run covers.
//!
//! `cargo test -p parser` says nothing about `crates/cli`, and
//! `cd web && npm test` nothing about the backend. Capture records the
//! selectors it can read from the command line; the report resolves package
//! names to directories with the manifests at head (see [`Packages`]).
//! Anything Witness can't read counts as covering the whole repository, and
//! running a subset of the tests (`pytest -k`, `cargo test name`) marks the
//! run as filtered.

use serde::{Deserialize, Serialize};

use crate::command::shell_words;

/// Selectors read from a command line. All empty means the whole
/// repository.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    /// Working directory relative to the repo root, when not the root. It
    /// limits the run only if it is a package's directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Package names (`cargo -p`, npm workspaces).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<String>,
    /// Directories the run is limited to (go packages, `--manifest-path`,
    /// gradle and maven modules), relative to the repo root.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// Only some of the tests ran (a name filter, or specific test files).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub filtered: bool,
}

impl Scope {
    pub fn is_whole(&self) -> bool {
        self.cwd.is_none() && self.packages.is_empty() && self.paths.is_empty() && !self.filtered
    }
}

/// Package lookups against the tree at head.
pub trait Packages {
    /// Directory of the package called `name`, relative to the repo root.
    fn dir_of(&self, name: &str) -> Option<String>;
    /// Whether `dir` holds a package manifest of its own.
    fn is_package_root(&self, dir: &str) -> bool;
}

/// Resolves nothing: package-limited runs count as covering everything.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPackages;

impl Packages for NoPackages {
    fn dir_of(&self, _name: &str) -> Option<String> {
        None
    }
    fn is_package_root(&self, _dir: &str) -> bool {
        false
    }
}

/// File names that make a directory a package's root.
pub const MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "setup.py",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "composer.json",
    "mix.exs",
];

/// The package name a manifest declares (`Cargo.toml` `[package] name`,
/// `package.json` `name`, `pyproject.toml` `[project] name`), if any.
pub fn package_name(manifest: &str, content: &str) -> Option<String> {
    let file = manifest.rsplit('/').next().unwrap_or(manifest);
    match file {
        "Cargo.toml" | "pyproject.toml" => {
            let value: toml::Value = toml::from_str(content).ok()?;
            ["package", "project"]
                .iter()
                .find_map(|table| value.get(table)?.get("name")?.as_str())
                .map(str::to_owned)
        }
        "package.json" | "composer.json" => {
            let value: serde_json::Value = serde_json::from_str(content).ok()?;
            value.get("name")?.as_str().map(str::to_owned)
        }
        _ => None,
    }
}

/// Whether a run with `scope` covers `file`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coverage {
    Covers,
    /// The run was limited to these directories, and `file` is outside.
    Outside(Vec<String>),
}

/// Whether a run with `scope` covers `file` (repo-relative).
pub fn covers(scope: Option<&Scope>, file: &str, packages: &dyn Packages) -> Coverage {
    let Some(scope) = scope else {
        return Coverage::Covers;
    };
    let dirs: Vec<String> = if !scope.paths.is_empty() {
        scope.paths.clone()
    } else if !scope.packages.is_empty() {
        let resolved: Option<Vec<String>> =
            scope.packages.iter().map(|p| packages.dir_of(p)).collect();
        match resolved {
            Some(dirs) => dirs,
            // A package we can't find: don't claim the run missed the file.
            None => return Coverage::Covers,
        }
    } else {
        match &scope.cwd {
            Some(dir) if packages.is_package_root(dir) => vec![dir.clone()],
            _ => return Coverage::Covers,
        }
    };
    if dirs.iter().any(|d| under(file, d)) {
        Coverage::Covers
    } else {
        Coverage::Outside(dirs)
    }
}

fn under(file: &str, dir: &str) -> bool {
    dir.is_empty()
        || file == dir
        || file
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Reads the scope of `simple` (the classified part of a command line, see
/// [`crate::command::CommandVerdict`]) run in `cwd` (repo-relative; `None`
/// at the root) after `cd`-ing through `cds`. Returns `None` for a run of
/// the whole repository.
pub fn parse(simple: &str, cwd: Option<&str>, cds: &[String]) -> Option<Scope> {
    let words = without_redirects(shell_words(simple));
    let mut scope = Scope {
        cwd: join_cwd(cwd, cds),
        ..Scope::default()
    };
    let base = scope.cwd.clone();
    let rel = |p: &str| -> String { normalize(base.as_deref(), p) };
    let program = words
        .first()
        .map(|w| w.rsplit('/').next().unwrap_or(w).to_owned())
        .unwrap_or_default();
    let args: Vec<&str> = words.iter().skip(1).map(String::as_str).collect();
    match program.as_str() {
        "cargo" => cargo(&args, &mut scope, &rel),
        "go" => go(&args, &mut scope, &rel),
        "npm" | "pnpm" | "yarn" | "bun" => js_package_manager(&program, &args, &mut scope),
        "npx" | "jest" | "vitest" | "mocha" | "pytest" | "rspec" | "phpunit" => {
            test_runner(&args, &mut scope)
        }
        "python" | "python3" | "uv" | "poetry" => {
            if let Some(i) = args.iter().position(|a| *a == "pytest") {
                test_runner(&args[i + 1..], &mut scope);
            }
        }
        "gradle" | "gradlew" => gradle(&args, &mut scope, &rel),
        "mvn" | "mvnw" => maven(&args, &mut scope, &rel),
        "make" => {
            if let Some(dir) = value_of(&args, &["-C", "--directory"]) {
                scope.paths.push(rel(dir));
            }
        }
        _ => {}
    }
    (!scope.is_whole()).then_some(scope)
}

fn cargo(args: &[&str], scope: &mut Scope, rel: &dyn Fn(&str) -> String) {
    const VALUE_FLAGS: &[&str] = &[
        "-p",
        "--package",
        "--manifest-path",
        "--features",
        "-F",
        "--target",
        "--target-dir",
        "-j",
        "--jobs",
        "--profile",
        "--color",
        "--message-format",
        "-Z",
        "--config",
        "--exclude",
        "-E",
        "--filterset",
        "--partition",
        "--test-threads",
    ];
    let sub_at = args
        .iter()
        .position(|a| !a.starts_with('-') && !a.starts_with('+'));
    let Some(sub_at) = sub_at else { return };
    let sub = args[sub_at];
    let mut rest = args[sub_at + 1..].iter().peekable();
    // `cargo nextest run [filters]`
    if sub == "nextest" && rest.peek().is_some_and(|w| **w == "run") {
        rest.next();
    }
    while let Some(arg) = rest.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with('-') => (f, Some(v)),
            _ => (*arg, None),
        };
        match flag {
            "--" => {
                // Arguments for the test binary: a filter if any is not a flag.
                if rest.any(|a| !a.starts_with('-')) {
                    scope.filtered = true;
                }
                break;
            }
            "--workspace" | "--all" => {
                scope.cwd = None;
                scope.packages.clear();
            }
            "-p" | "--package" => {
                if let Some(v) = inline.or_else(|| rest.next().copied()) {
                    scope.packages.push(v.to_owned());
                }
            }
            "--manifest-path" => {
                if let Some(v) = inline.or_else(|| rest.next().copied()) {
                    let dir = v.rsplit_once('/').map_or("", |(d, _)| d);
                    scope.paths.push(rel(dir));
                }
            }
            "--lib" | "--bins" | "--bin" | "--test" | "--tests" | "--example" | "--bench"
            | "--doc"
                if sub == "test" || sub == "nextest" =>
            {
                scope.filtered = true;
                if matches!(flag, "--bin" | "--test" | "--example" | "--bench") && inline.is_none()
                {
                    rest.next();
                }
            }
            "-E" | "--filterset" => {
                scope.filtered = true;
                if inline.is_none() {
                    rest.next();
                }
            }
            f if VALUE_FLAGS.contains(&f) => {
                if inline.is_none() {
                    rest.next();
                }
            }
            f if f.starts_with('-') => {}
            // A positional argument to `cargo test`/`nextest` is a name filter.
            _ if sub == "test" || sub == "nextest" => scope.filtered = true,
            _ => {}
        }
    }
}

fn go(args: &[&str], scope: &mut Scope, rel: &dyn Fn(&str) -> String) {
    let mut rest = args
        .iter()
        .skip_while(|a| **a != "test" && **a != "build" && **a != "vet");
    rest.next();
    let mut rest = rest.peekable();
    let mut whole = false;
    while let Some(arg) = rest.next() {
        match *arg {
            "-run" | "-skip" => {
                scope.filtered = true;
                rest.next();
            }
            a if a.starts_with("-run=") => scope.filtered = true,
            "-tags" | "-timeout" | "-count" | "-p" | "-parallel" | "-coverprofile" | "-o" => {
                rest.next();
            }
            a if a.starts_with('-') => {}
            "./..." | "..." => whole = true,
            pkg => {
                let dir = pkg.strip_suffix("/...").unwrap_or(pkg);
                if dir.starts_with('.') {
                    scope.paths.push(rel(dir));
                }
            }
        }
    }
    if whole {
        // `./...` from a subdirectory covers that directory.
        scope.paths.retain(|_| false);
        if let Some(cwd) = scope.cwd.clone() {
            scope.paths.push(cwd);
        }
    }
}

fn js_package_manager(program: &str, args: &[&str], scope: &mut Scope) {
    let mut rest = args.iter().peekable();
    if program == "yarn" && rest.peek().is_some_and(|w| **w == "workspace") {
        rest.next();
        if let Some(name) = rest.next() {
            scope.packages.push((*name).to_owned());
        }
    }
    while let Some(arg) = rest.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with('-') => (f, Some(v)),
            _ => (*arg, None),
        };
        match flag {
            "-w" | "--workspace" | "--filter" | "-F" => {
                if let Some(v) = inline.or_else(|| rest.next().copied()) {
                    scope.packages.push(v.trim_start_matches("./").to_owned());
                }
            }
            "--" => {
                let extra: Vec<&str> = rest.by_ref().copied().collect();
                test_runner(&extra, scope);
                break;
            }
            _ => {}
        }
    }
}

/// Runners whose positional arguments are test files or patterns, so any
/// means a subset of the tests ran.
fn test_runner(args: &[&str], scope: &mut Scope) {
    const FILTER_FLAGS: &[&str] = &[
        "-k",
        "-m",
        "-t",
        "--testNamePattern",
        "--testPathPattern",
        "--grep",
        "-g",
        "--filter",
        "--example",
        "-e",
        "--tag",
    ];
    const VALUE_FLAGS: &[&str] = &[
        "-c",
        "--config",
        "-p",
        "--reporter",
        "-n",
        "--maxfail",
        "--rootdir",
        "--project",
        "--format",
    ];
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let flag = arg.split_once('=').map_or(*arg, |(f, _)| f);
        if FILTER_FLAGS.contains(&flag) {
            scope.filtered = true;
            if !arg.contains('=') {
                rest.next();
            }
        } else if VALUE_FLAGS.contains(&flag) {
            if !arg.contains('=') {
                rest.next();
            }
        } else if !arg.starts_with('-')
            && !matches!(*arg, "run" | "test" | "jest" | "vitest" | "mocha")
        {
            scope.filtered = true;
        }
    }
}

fn gradle(args: &[&str], scope: &mut Scope, rel: &dyn Fn(&str) -> String) {
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if *arg == "-p" || *arg == "--project-dir" {
            if let Some(dir) = rest.next() {
                scope.paths.push(rel(dir));
            }
        } else if *arg == "--tests" {
            scope.filtered = true;
            rest.next();
        } else if let Some(task) = arg.strip_prefix(':')
            && let Some((module, _)) = task.rsplit_once(':')
        {
            scope.paths.push(rel(&module.replace(':', "/")));
        }
    }
}

fn maven(args: &[&str], scope: &mut Scope, rel: &dyn Fn(&str) -> String) {
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if *arg == "-pl" || *arg == "--projects" {
            if let Some(list) = rest.next() {
                for module in list.split(',') {
                    scope.paths.push(rel(module.trim_start_matches(':')));
                }
            }
        } else if arg.starts_with("-Dtest=") || arg.starts_with("-Dit.test=") {
            scope.filtered = true;
        }
    }
}

/// Drops redirections (`2>&1`, `> out.txt`, `<in`) from a word list.
fn without_redirects(words: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(words.len());
    let mut skip_target = false;
    for word in words {
        if skip_target {
            skip_target = false;
            continue;
        }
        let rest = word.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
        if let Some(target) = rest
            .strip_prefix(">>")
            .or_else(|| rest.strip_prefix('>'))
            .or_else(|| rest.strip_prefix('<'))
        {
            // `>` alone: the target is the next word.
            skip_target = target.is_empty();
            continue;
        }
        out.push(word);
    }
    out
}

fn value_of<'a>(args: &[&'a str], flags: &[&str]) -> Option<&'a str> {
    args.iter().enumerate().find_map(|(i, a)| {
        if flags.contains(a) {
            args.get(i + 1).copied()
        } else {
            flags.iter().find_map(|f| a.strip_prefix(&format!("{f}=")))
        }
    })
}

/// The working directory after `cd`s, relative to the repo root.
fn join_cwd(cwd: Option<&str>, cds: &[String]) -> Option<String> {
    let mut dir = cwd.unwrap_or("").to_owned();
    for cd in cds {
        if cd.starts_with('/') || cd.starts_with('~') || cd == "-" {
            // Absolute or unknown: can't place it in the repo.
            return None;
        }
        dir = normalize(Some(&dir), cd);
    }
    (!dir.is_empty()).then_some(dir)
}

/// `path` resolved against `base`, `/`-separated, without `.` and `..`.
fn normalize(base: Option<&str>, path: &str) -> String {
    let mut parts: Vec<&str> = base
        .unwrap_or("")
        .split('/')
        .filter(|p| !p.is_empty())
        .collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(cmd: &str) -> Option<Scope> {
        parse(cmd, None, &[])
    }

    fn packages(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn cargo_selectors() {
        assert_eq!(scope("cargo test"), None);
        assert_eq!(scope("cargo test --workspace --release"), None);
        assert_eq!(
            scope("cargo test -p witness-core -p witness-git")
                .unwrap()
                .packages,
            packages(&["witness-core", "witness-git"])
        );
        assert_eq!(
            scope("cargo test --package=a").unwrap().packages,
            packages(&["a"])
        );
        let filtered = scope("cargo test -p a parse_ --features x").unwrap();
        assert!(filtered.filtered);
        assert_eq!(filtered.packages, packages(&["a"]));
        assert!(scope("cargo test -- --nocapture").is_none());
        assert!(scope("cargo test -- parse_").unwrap().filtered);
        assert_eq!(
            scope("cargo test --manifest-path crates/x/Cargo.toml")
                .unwrap()
                .paths,
            packages(&["crates/x"])
        );
        assert!(
            scope("cargo nextest run -E 'test(parse)'")
                .unwrap()
                .filtered
        );
        assert_eq!(scope("cargo build --release"), None);
        // Redirections are not filters.
        assert_eq!(scope("cargo test 2>&1"), None);
        assert_eq!(scope("cargo test > out.txt 2> err.txt"), None);
        assert!(scope("pytest -q 2>&1 tests/a.py").unwrap().filtered);
    }

    #[test]
    fn working_directory_and_cds() {
        let s = parse("cargo test", Some("crates"), &["x".into()]).unwrap();
        assert_eq!(s.cwd.as_deref(), Some("crates/x"));
        assert_eq!(parse("cargo test", Some("a"), &["..".into()]), None);
        assert_eq!(parse("cargo test", None, &["/tmp".into()]), None);
    }

    #[test]
    fn go_packages() {
        assert_eq!(scope("go test ./..."), None);
        assert_eq!(
            scope("go test ./internal/parser/... ./cmd").unwrap().paths,
            packages(&["internal/parser", "cmd"])
        );
        assert!(scope("go test -run TestParse ./...").unwrap().filtered);
        let sub = parse("go test ./...", Some("svc/api"), &[]).unwrap();
        assert_eq!(sub.paths, packages(&["svc/api"]));
    }

    #[test]
    fn javascript_and_python() {
        assert_eq!(scope("npm test"), None);
        assert_eq!(
            scope("npm test -w packages/web").unwrap().packages,
            packages(&["packages/web"])
        );
        assert_eq!(
            scope("pnpm --filter @acme/api test").unwrap().packages,
            packages(&["@acme/api"])
        );
        assert_eq!(
            scope("yarn workspace @acme/web test").unwrap().packages,
            packages(&["@acme/web"])
        );
        assert!(scope("npm test -- src/parse.test.ts").unwrap().filtered);
        assert!(scope("npx vitest run src/a.test.ts").unwrap().filtered);
        assert!(scope("npx jest -t 'parses dates'").unwrap().filtered);
        assert_eq!(scope("npx vitest run"), None);
        assert!(scope("pytest tests/test_parse.py").unwrap().filtered);
        assert!(scope("python -m pytest -k parse -q").unwrap().filtered);
        assert_eq!(scope("pytest -q -n 4"), None);
    }

    #[test]
    fn gradle_maven_make() {
        assert_eq!(
            scope("./gradlew :app:core:test").unwrap().paths,
            packages(&["app/core"])
        );
        assert_eq!(
            scope("mvn -pl api,web test").unwrap().paths,
            packages(&["api", "web"])
        );
        assert!(scope("mvn test -Dtest=ParserTest").unwrap().filtered);
        assert_eq!(
            scope("make -C backend test").unwrap().paths,
            packages(&["backend"])
        );
    }

    struct Fake;
    impl Packages for Fake {
        fn dir_of(&self, name: &str) -> Option<String> {
            (name == "core").then(|| "crates/core".to_owned())
        }
        fn is_package_root(&self, dir: &str) -> bool {
            dir == "crates/cli"
        }
    }

    #[test]
    fn reads_package_names() {
        assert_eq!(
            package_name("crates/a/Cargo.toml", "[package]\nname = \"a\"\n").as_deref(),
            Some("a")
        );
        assert_eq!(
            package_name("Cargo.toml", "[workspace]\nmembers = []\n"),
            None
        );
        assert_eq!(
            package_name("web/package.json", r#"{"name": "@acme/web"}"#).as_deref(),
            Some("@acme/web")
        );
        assert_eq!(
            package_name("py/pyproject.toml", "[project]\nname = \"tool\"\n").as_deref(),
            Some("tool")
        );
        assert_eq!(package_name("go.mod", "module x"), None);
    }

    #[test]
    fn coverage() {
        let core = Scope {
            packages: packages(&["core"]),
            ..Scope::default()
        };
        assert_eq!(
            covers(Some(&core), "crates/core/src/a.rs", &Fake),
            Coverage::Covers
        );
        assert_eq!(
            covers(Some(&core), "crates/cli/src/a.rs", &Fake),
            Coverage::Outside(packages(&["crates/core"]))
        );
        let unknown = Scope {
            packages: packages(&["mystery"]),
            ..Scope::default()
        };
        assert_eq!(covers(Some(&unknown), "x.rs", &Fake), Coverage::Covers);
        let cwd = Scope {
            cwd: Some("crates/cli".into()),
            ..Scope::default()
        };
        assert_eq!(
            covers(Some(&cwd), "crates/cli/main.rs", &Fake),
            Coverage::Covers
        );
        assert!(matches!(
            covers(Some(&cwd), "crates/core/a.rs", &Fake),
            Coverage::Outside(_)
        ));
        // A cwd that isn't a package (e.g. `cd tests`) limits nothing.
        let tests_dir = Scope {
            cwd: Some("tests".into()),
            ..Scope::default()
        };
        assert_eq!(
            covers(Some(&tests_dir), "src/a.rs", &Fake),
            Coverage::Covers
        );
        assert_eq!(covers(None, "src/a.rs", &Fake), Coverage::Covers);
    }
}
