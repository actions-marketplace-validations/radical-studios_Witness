//! Test and build results read from a command's output.
//!
//! When a command's exit code doesn't say how the test or build went (it was
//! piped into `tail`, or the agent reports no exit code), the runner's own
//! summary lines usually do. The output is read in memory only; just the
//! outcome and counts are kept.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// How a test or build run went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Passed,
    Failed,
}

/// Test counts from a runner's summary; absent counts were not reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCounts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passed: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<u32>,
}

impl TestCounts {
    pub fn is_empty(&self) -> bool {
        self.passed.is_none() && self.failed.is_none() && self.skipped.is_none()
    }

    /// e.g. `142 passed, 1 skipped`.
    pub fn describe(&self) -> Option<String> {
        let parts: Vec<String> = [
            (self.passed, "passed"),
            (self.failed, "failed"),
            (self.skipped, "skipped"),
        ]
        .into_iter()
        .filter_map(|(n, what)| {
            n.filter(|&n| n > 0 || what == "passed")
                .map(|n| format!("{n} {what}"))
        })
        .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

/// What a command's output says about its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub outcome: Option<RunOutcome>,
    pub tests: TestCounts,
}

#[derive(Default)]
struct Tally {
    passed: Option<u32>,
    failed: Option<u32>,
    skipped: Option<u32>,
    pass_marker: bool,
    fail_marker: bool,
}

impl Tally {
    fn add(slot: &mut Option<u32>, n: u32) {
        *slot = Some(slot.unwrap_or(0).saturating_add(n));
    }
    fn passed(&mut self, n: u32) {
        Self::add(&mut self.passed, n);
    }
    fn failed(&mut self, n: u32) {
        Self::add(&mut self.failed, n);
    }
    fn skipped(&mut self, n: u32) {
        Self::add(&mut self.skipped, n);
    }
}

fn re(pattern: &str) -> Option<Regex> {
    Regex::new(pattern).ok()
}

macro_rules! regex {
    ($name:ident, $pattern:expr) => {
        static $name: LazyLock<Option<Regex>> = LazyLock::new(|| re($pattern));
    };
}

// cargo test: `test result: ok. 12 passed; 0 failed; 1 ignored; ...`
regex!(
    CARGO,
    r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored"
);
// cargo nextest: `12 tests run: 11 passed, 1 failed, 0 skipped`
regex!(
    NEXTEST,
    r"\d+ tests? run: (\d+) passed(?:, (\d+) failed)?(?:.*?(\d+) skipped)?"
);
// pytest: `==== 1 failed, 2 passed, 1 skipped in 0.31s ====` or `3 passed in 0.1s`
regex!(PYTEST_LINE, r"^=*\s*(\d+ \w+(?:, \d+ \w+)*) in [\d.]+s\b");
regex!(
    PYTEST_PART,
    r"(\d+) (passed|failed|errors?|skipped|xfailed|xpassed)"
);
// jest: `Tests:       1 failed, 11 passed, 12 total`
regex!(JEST, r"^Tests:\s+(.*\d+ total)");
// vitest: `Tests  1 failed | 11 passed (12)`
regex!(VITEST, r"^Tests\s+(\d+ \w+(?: \| \d+ \w+)*) \(\d+\)");
regex!(COUNT_PART, r"(\d+) (passed|failed|skipped|todo|pending)");
// mocha: `12 passing (34ms)`, `1 failing`, `2 pending`
regex!(MOCHA, r"^(\d+) (passing|failing|pending)\b");
// go test: `ok  	pkg	0.1s`, `FAIL	pkg	0.1s`, `--- FAIL: TestX`
regex!(GO_OK, r"^ok\s+\S+\s+(\(cached\)|[\d.]+s)");
regex!(GO_FAIL, r"^(FAIL\s+\S+|--- FAIL:|FAIL$)");
// maven surefire: `Tests run: 12, Failures: 1, Errors: 0, Skipped: 0`
regex!(
    MAVEN,
    r"^(?:\[\w+\]\s+)?Tests run: (\d+), Failures: (\d+), Errors: (\d+), Skipped: (\d+)$"
);
// gradle: `12 tests completed, 1 failed`
regex!(GRADLE, r"^(\d+) tests completed, (\d+) failed");
// rspec: `12 examples, 1 failure, 2 pending`
regex!(
    RSPEC,
    r"^(\d+) examples?, (\d+) failures?(?:, (\d+) pending)?"
);
// dotnet test: `Failed!  - Failed:     1, Passed:    11, Skipped:     0, Total:    12`
regex!(
    DOTNET,
    r"^(Passed|Failed)!\s+-\s+Failed:\s+(\d+),\s+Passed:\s+(\d+),\s+Skipped:\s+(\d+)"
);
// bun test: ` 12 pass`, ` 1 fail`
regex!(BUN, r"^(\d+) (pass|fail|skip)$");
// deno test: `ok | 12 passed | 0 failed (34ms)`
regex!(
    DENO,
    r"^(ok|FAILED) \| (\d+) passed(?: \(\d+ steps?\))? \| (\d+) failed"
);
// phpunit: `OK (12 tests, 30 assertions)` / `Tests: 12, Assertions: 30, Failures: 1.`
regex!(PHPUNIT_OK, r"^OK \((\d+) tests?,");
regex!(
    PHPUNIT_FAIL,
    r"^Tests: (\d+), Assertions: \d+, (?:Errors: (\d+), )?Failures: (\d+)"
);
// tsc: `Found 3 errors in 2 files.`
regex!(TSC, r"^Found (\d+) errors?\b");

/// Reads test and build summaries from `output`. Returns `None` when
/// nothing recognizable was found.
pub fn parse(output: &str) -> Option<Summary> {
    let mut t = Tally::default();
    let mut found = false;
    let caps_u32 =
        |c: &regex::Captures<'_>, i: usize| c.get(i).and_then(|m| m.as_str().parse::<u32>().ok());
    for raw in output.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(c) = CARGO.as_ref().and_then(|r| r.captures(line)) {
            found = true;
            t.passed(caps_u32(&c, 2).unwrap_or(0));
            t.failed(caps_u32(&c, 3).unwrap_or(0));
            t.skipped(caps_u32(&c, 4).unwrap_or(0));
            if &c[1] == "FAILED" {
                t.fail_marker = true;
            } else {
                t.pass_marker = true;
            }
            continue;
        }
        if line.starts_with("error: could not compile")
            || line.starts_with("error: test failed")
            || line.starts_with("error: test run failed")
            || line == "BUILD FAILED"
            || line.starts_with("BUILD FAILED in")
            || line.ends_with("BUILD FAILURE")
        {
            found = true;
            t.fail_marker = true;
            continue;
        }
        if (line.starts_with("Finished `") && line.contains(" profile "))
            || line.starts_with("BUILD SUCCESSFUL")
            || line.ends_with("BUILD SUCCESS")
        {
            found = true;
            t.pass_marker = true;
            continue;
        }
        if let Some(c) = NEXTEST.as_ref().and_then(|r| r.captures(line)) {
            found = true;
            t.passed(caps_u32(&c, 1).unwrap_or(0));
            if let Some(n) = caps_u32(&c, 2) {
                t.failed(n);
            }
            if let Some(n) = caps_u32(&c, 3) {
                t.skipped(n);
            }
            continue;
        }
        if let Some(c) = JEST.as_ref().and_then(|r| r.captures(line)) {
            found = true;
            tally_parts(&mut t, &c[1]);
            continue;
        }
        if let Some(c) = VITEST.as_ref().and_then(|r| r.captures(line)) {
            found = true;
            tally_parts(&mut t, &c[1]);
            continue;
        }
        if let Some(c) = PYTEST_LINE.as_ref().and_then(|r| r.captures(line))
            && PYTEST_PART.as_ref().is_some_and(|r| r.is_match(&c[1]))
        {
            found = true;
            if let Some(part) = PYTEST_PART.as_ref() {
                for p in part.captures_iter(&c[1]) {
                    let n = p[1].parse().unwrap_or(0);
                    match &p[2] {
                        "passed" | "xfailed" | "xpassed" => t.passed(n),
                        "failed" | "error" | "errors" => t.failed(n),
                        _ => t.skipped(n),
                    }
                }
            }
            continue;
        }
        if let Some(c) = MOCHA.as_ref().and_then(|r| r.captures(line)) {
            found = true;
            let n = c[1].parse().unwrap_or(0);
            match &c[2] {
                "passing" => t.passed(n),
                "failing" => t.failed(n),
                _ => t.skipped(n),
            }
            continue;
        }
        if GO_OK.as_ref().is_some_and(|r| r.is_match(line)) {
            found = true;
            t.pass_marker = true;
            continue;
        }
        if GO_FAIL.as_ref().is_some_and(|r| r.is_match(line)) {
            found = true;
            t.fail_marker = true;
            continue;
        }
        if let Some(c) = MAVEN.as_ref().and_then(|r| r.captures(line)) {
            found = true;
            let (run, fail, err, skip) = (
                caps_u32(&c, 1).unwrap_or(0),
                caps_u32(&c, 2).unwrap_or(0),
                caps_u32(&c, 3).unwrap_or(0),
                caps_u32(&c, 4).unwrap_or(0),
            );
            // Surefire prints per-class lines and a total; keep the largest.
            t.passed = Some(
                t.passed
                    .unwrap_or(0)
                    .max(run.saturating_sub(fail + err + skip)),
            );
            t.failed = Some(t.failed.unwrap_or(0).max(fail + err));
            t.skipped = Some(t.skipped.unwrap_or(0).max(skip));
            continue;
        }
        if let Some(c) = GRADLE.as_ref().and_then(|r| r.captures(line)) {
            found = true;
            let (done, fail) = (caps_u32(&c, 1).unwrap_or(0), caps_u32(&c, 2).unwrap_or(0));
            t.passed(done.saturating_sub(fail));
            t.failed(fail);
            continue;
        }
        if let Some(c) = RSPEC.as_ref().and_then(|r| r.captures(line)) {
            found = true;
            let (all, fail) = (caps_u32(&c, 1).unwrap_or(0), caps_u32(&c, 2).unwrap_or(0));
            let pending = caps_u32(&c, 3).unwrap_or(0);
            t.passed(all.saturating_sub(fail + pending));
            t.failed(fail);
            t.skipped(pending);
            continue;
        }
        if let Some(c) = DOTNET.as_ref().and_then(|r| r.captures(line)) {
            found = true;
            t.failed(caps_u32(&c, 2).unwrap_or(0));
            t.passed(caps_u32(&c, 3).unwrap_or(0));
            t.skipped(caps_u32(&c, 4).unwrap_or(0));
            continue;
        }
        if let Some(c) = DENO.as_ref().and_then(|r| r.captures(line)) {
            found = true;
            t.passed(caps_u32(&c, 2).unwrap_or(0));
            t.failed(caps_u32(&c, 3).unwrap_or(0));
            continue;
        }
        if let Some(c) = BUN.as_ref().and_then(|r| r.captures(line)) {
            found = true;
            let n = c[1].parse().unwrap_or(0);
            match &c[2] {
                "pass" => t.passed(n),
                "fail" => t.failed(n),
                _ => t.skipped(n),
            }
            continue;
        }
        if let Some(c) = PHPUNIT_OK.as_ref().and_then(|r| r.captures(line)) {
            found = true;
            t.passed(caps_u32(&c, 1).unwrap_or(0));
            continue;
        }
        if let Some(c) = PHPUNIT_FAIL.as_ref().and_then(|r| r.captures(line)) {
            found = true;
            let all = caps_u32(&c, 1).unwrap_or(0);
            let fail = caps_u32(&c, 2).unwrap_or(0) + caps_u32(&c, 3).unwrap_or(0);
            t.passed(all.saturating_sub(fail));
            t.failed(fail);
            continue;
        }
        if let Some(c) = TSC.as_ref().and_then(|r| r.captures(line)) {
            found = true;
            if caps_u32(&c, 1).unwrap_or(0) > 0 {
                t.fail_marker = true;
            }
        }
    }
    if !found {
        return None;
    }
    let failed = t.fail_marker || t.failed.is_some_and(|n| n > 0);
    let passed = t.pass_marker || t.passed.is_some_and(|n| n > 0);
    let outcome = if failed {
        Some(RunOutcome::Failed)
    } else if passed {
        Some(RunOutcome::Passed)
    } else {
        None
    };
    Some(Summary {
        outcome,
        tests: TestCounts {
            passed: t.passed,
            failed: t.failed,
            skipped: t.skipped,
        },
    })
}

fn tally_parts(t: &mut Tally, text: &str) {
    let Some(part) = COUNT_PART.as_ref() else {
        return;
    };
    for p in part.captures_iter(text) {
        let n = p[1].parse().unwrap_or(0);
        match &p[2] {
            "passed" => t.passed(n),
            "failed" => t.failed(n),
            _ => t.skipped(n),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(text: &str) -> Option<RunOutcome> {
        parse(text).and_then(|s| s.outcome)
    }

    /// Passed, failed and skipped.
    type Counts = (Option<u32>, Option<u32>, Option<u32>);

    fn counts(text: &str) -> Counts {
        let t = parse(text).unwrap().tests;
        (t.passed, t.failed, t.skipped)
    }

    #[test]
    fn cargo_test() {
        let ok = "running 3 tests\ntest a ... ok\n\ntest result: ok. 3 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\n   Doc-tests x\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out";
        assert_eq!(outcome(ok), Some(RunOutcome::Passed));
        assert_eq!(counts(ok), (Some(5), Some(0), Some(1)));
        let failed = "test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\nerror: test failed, to rerun pass `--lib`";
        assert_eq!(outcome(failed), Some(RunOutcome::Failed));
        let compile = "   Compiling x v0.1.0\nerror[E0425]: cannot find value `y`\nerror: could not compile `x` (lib test) due to 1 previous error";
        assert_eq!(outcome(compile), Some(RunOutcome::Failed));
        let build = "   Compiling x v0.1.0\n    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.1s";
        assert_eq!(outcome(build), Some(RunOutcome::Passed));
    }

    #[test]
    fn other_runners() {
        let cases: &[(&str, RunOutcome, Counts)] = &[
            (
                "     Summary [   0.1s] 12 tests run: 11 passed, 1 failed, 0 skipped",
                RunOutcome::Failed,
                (Some(11), Some(1), Some(0)),
            ),
            (
                "============ 1 failed, 2 passed, 1 skipped in 0.31s ============",
                RunOutcome::Failed,
                (Some(2), Some(1), Some(1)),
            ),
            (
                "3 passed in 0.12s",
                RunOutcome::Passed,
                (Some(3), None, None),
            ),
            (
                "Tests:       1 failed, 11 passed, 12 total",
                RunOutcome::Failed,
                (Some(11), Some(1), None),
            ),
            (
                " Tests  12 passed (12)",
                RunOutcome::Passed,
                (Some(12), None, None),
            ),
            (
                "  12 passing (34ms)\n  1 failing",
                RunOutcome::Failed,
                (Some(12), Some(1), None),
            ),
            (
                "Tests run: 12, Failures: 0, Errors: 0, Skipped: 1\n[INFO] BUILD SUCCESS",
                RunOutcome::Passed,
                (Some(11), Some(0), Some(1)),
            ),
            (
                "12 examples, 0 failures, 2 pending",
                RunOutcome::Passed,
                (Some(10), Some(0), Some(2)),
            ),
            (
                "Passed!  - Failed:     0, Passed:    12, Skipped:     0, Total:    12",
                RunOutcome::Passed,
                (Some(12), Some(0), Some(0)),
            ),
            (
                " 12 pass\n 1 fail\nRan 13 tests across 2 files.",
                RunOutcome::Failed,
                (Some(12), Some(1), None),
            ),
            (
                "FAILED | 11 passed | 1 failed (40ms)",
                RunOutcome::Failed,
                (Some(11), Some(1), None),
            ),
            (
                "OK (12 tests, 30 assertions)",
                RunOutcome::Passed,
                (Some(12), None, None),
            ),
        ];
        for (text, expected, expected_counts) in cases {
            assert_eq!(outcome(text), Some(*expected), "{text}");
            assert_eq!(counts(text), *expected_counts, "{text}");
        }
    }

    #[test]
    fn go_and_markers() {
        assert_eq!(
            outcome("ok  \texample.com/a\t0.12s\nok  \texample.com/b\t(cached)"),
            Some(RunOutcome::Passed)
        );
        assert_eq!(
            outcome("--- FAIL: TestParse (0.00s)\nFAIL\nFAIL\texample.com/a\t0.1s"),
            Some(RunOutcome::Failed)
        );
        assert_eq!(outcome("BUILD SUCCESSFUL in 3s"), Some(RunOutcome::Passed));
        assert_eq!(outcome("BUILD FAILED in 3s"), Some(RunOutcome::Failed));
        assert_eq!(
            outcome("src/a.ts:1:1 - error TS2304\n\nFound 2 errors in 1 file."),
            Some(RunOutcome::Failed)
        );
    }

    #[test]
    fn unrelated_output_says_nothing() {
        assert_eq!(parse("hello\nworld"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("the test result was fine"), None);
    }

    #[test]
    fn describes_counts() {
        let t = TestCounts {
            passed: Some(142),
            failed: Some(0),
            skipped: Some(0),
        };
        assert_eq!(t.describe().as_deref(), Some("142 passed"));
        let t = TestCounts {
            passed: Some(11),
            failed: Some(1),
            skipped: Some(2),
        };
        assert_eq!(
            t.describe().as_deref(),
            Some("11 passed, 1 failed, 2 skipped")
        );
    }
}
