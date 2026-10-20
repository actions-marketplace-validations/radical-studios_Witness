//! Claims an agent makes in its final message ("all tests pass"), read in
//! memory and compared with the record. Only the kind of claim is stored.
//!
//! Detection is deliberately conservative: a sentence counts only if it
//! asserts success outright, without negation or hedging.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Claim {
    /// "All tests pass", "the tests are passing".
    TestsPass,
    /// "It builds cleanly", "compiles without errors".
    Builds,
}

impl Claim {
    pub fn describe(&self) -> &'static str {
        match self {
            Claim::TestsPass => "the tests pass",
            Claim::Builds => "it builds",
        }
    }
}

static TESTS_PASS: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(all|the|every|both|existing|new)?\s*(unit |integration )?tests?\s+(now\s+|still\s+)?(pass|passed|passing|succeed|succeeded|are\s+(all\s+)?(passing|green)|is\s+passing|run\s+green)\b|\btests?\s+(are\s+)?(all\s+)?green\b",
    )
    .ok()
});

static BUILDS: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(builds?|compiles?)\s+(cleanly|successfully|without\s+(errors|warnings))\b|\b(build|compilation)\s+(passes|passed|succeeds|succeeded)\b",
    )
    .ok()
});

/// Words that make a sentence's claim unreliable: negations, hedges,
/// conditionals and failures.
static UNSURE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(not|no|never|n't|cannot|can't|couldn't|didn't|don't|doesn't|won't|unable|fail\w*|should|would|might|may|if|once|after you|expect\w*|hopefully|probably|assum\w*|unless|except|but)\b|n't\b",
    )
    .ok()
});

/// The claims `message` makes outright.
pub fn detect(message: &str) -> Vec<Claim> {
    let mut found = Vec::new();
    for sentence in message.split(['.', '!', '\n', ';']) {
        if UNSURE.as_ref().is_some_and(|re| re.is_match(sentence)) {
            continue;
        }
        for (claim, re) in [(Claim::TestsPass, &*TESTS_PASS), (Claim::Builds, &*BUILDS)] {
            if re.as_ref().is_some_and(|re| re.is_match(sentence)) && !found.contains(&claim) {
                found.push(claim);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_outright_claims() {
        for text in [
            "Done. All tests pass.",
            "I fixed the parser and the tests are passing now",
            "Tests pass and the build succeeded.",
            "Everything is green: tests are all green!",
        ] {
            assert!(detect(text).contains(&Claim::TestsPass), "{text}");
        }
        assert_eq!(
            detect("The crate builds cleanly; tests pass."),
            vec![Claim::Builds, Claim::TestsPass]
        );
    }

    #[test]
    fn ignores_negated_hedged_or_conditional_ones() {
        for text in [
            "I couldn't get the tests to pass.",
            "The tests don't pass yet.",
            "Tests should pass once the fixture is updated.",
            "If the tests pass, merge it.",
            "Two tests fail; the rest pass.",
            "I didn't run the tests.",
            "Run the tests to check they pass.",
            "Tests probably pass now.",
        ] {
            assert!(detect(text).is_empty(), "{text}");
        }
    }
}
