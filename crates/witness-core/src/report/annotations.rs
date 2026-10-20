//! The review plan as inline annotations: GitHub Actions workflow commands
//! and GitLab Code Quality reports.

use serde_json::{Value, json};

use super::Report;
use super::sarif::describe;
use crate::classify::Status;

impl Report {
    /// `::warning file=...,line=...::message` lines for GitHub Actions,
    /// one per section for close review.
    pub fn to_github_annotations(&self) -> String {
        let mut out = String::new();
        for sec in self.close_review() {
            let command = if sec.status == Status::Failing {
                "error"
            } else {
                "warning"
            };
            let mut props = vec![format!("file={}", property(&self.repo_path(&sec.file)))];
            if let (Some(start), Some(end)) = (sec.start, sec.end) {
                props.push(format!("line={start}"));
                props.push(format!("endLine={}", end.max(start)));
            }
            props.push(format!(
                "title={}",
                property(&format!("Witness: {}", sec.status.as_str()))
            ));
            let mut message = sec.reason.clone();
            for signal in &sec.signals {
                message.push_str(&format!(" ({})", signal.detail));
            }
            out.push_str(&format!(
                "::{command} {}::{}\n",
                props.join(","),
                data(&message)
            ));
        }
        out
    }

    /// A GitLab Code Quality report (JSON array), one issue per section for
    /// close review.
    pub fn to_gitlab_codequality(&self) -> Value {
        let issues: Vec<Value> = self
            .close_review()
            .map(|sec| {
                let mut description = format!("Witness ({}): {}", sec.status.as_str(), sec.reason);
                for signal in &sec.signals {
                    description.push_str(&format!(" ({})", signal.detail));
                }
                let begin = sec.start.unwrap_or(1);
                json!({
                    "description": description,
                    "check_name": format!("witness/{}", sec.status.as_str()),
                    "categories": ["Bug Risk"],
                    "content": {"body": describe(sec.status)},
                    "fingerprint": format!("{}:{}", sec.fingerprint.path, sec.fingerprint.content_hash),
                    "severity": match sec.status {
                        Status::Failing => "critical",
                        Status::Guessed | Status::Unrecorded => "major",
                        _ => "minor",
                    },
                    "location": {
                        "path": self.repo_path(&sec.file),
                        "lines": {"begin": begin, "end": sec.end.unwrap_or(begin).max(begin)},
                    },
                })
            })
            .collect();
        Value::Array(issues)
    }
}

/// Escapes a workflow command message.
fn data(text: &str) -> String {
    text.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// Escapes a workflow command property value.
fn property(text: &str) -> String {
    data(text).replace(':', "%3A").replace(',', "%2C")
}
