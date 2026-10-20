//! The review plan as SARIF 2.1.0, for code scanning uploads.

use serde_json::{Value, json};

use super::Report;
use crate::classify::Status;

impl Report {
    /// SARIF with one result per section for close review.
    pub fn to_sarif(&self, tool_version: &str) -> Value {
        let rules: Vec<Value> = Status::ALL
            .iter()
            .filter(|s| **s != Status::Human)
            .map(|status| {
                json!({
                    "id": rule_id(*status),
                    "name": status.as_str(),
                    "shortDescription": {"text": describe(*status)},
                    "defaultConfiguration": {"level": level(*status)},
                })
            })
            .collect();
        let results: Vec<Value> = self
            .close_review()
            .map(|sec| {
                let mut message = sec.reason.clone();
                for signal in &sec.signals {
                    message.push_str(&format!(" ({})", signal.detail));
                }
                let mut region = json!({});
                if let (Some(start), Some(end)) = (sec.start, sec.end) {
                    region = json!({"startLine": start, "endLine": end.max(start)});
                }
                json!({
                    "ruleId": rule_id(sec.status),
                    "level": level(sec.status),
                    "message": {"text": message},
                    "locations": [{
                        "physicalLocation": {
                            "artifactLocation": {"uri": self.repo_path(&sec.file)},
                            "region": region,
                        }
                    }],
                    "partialFingerprints": {"witnessContent/v1": sec.fingerprint.content_hash},
                })
            })
            .collect();
        json!({
            "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
            "version": "2.1.0",
            "runs": [{
                "tool": {"driver": {
                    "name": "Witness",
                    "informationUri": "https://github.com/radical-studios/witness",
                    "version": tool_version,
                    "rules": rules,
                }},
                "results": results,
            }],
        })
    }
}

fn rule_id(status: Status) -> String {
    format!("witness/{}", status.as_str())
}

fn level(status: Status) -> &'static str {
    match status {
        Status::Failing => "error",
        Status::Guessed | Status::Unrecorded | Status::Unverified => "warning",
        Status::Compiled | Status::Tested | Status::Human => "note",
    }
}

pub(super) fn describe(status: Status) -> &'static str {
    match status {
        Status::Failing => "A test or build failed after the agent's last edit",
        Status::Guessed => "The agent recorded an assumption about this code",
        Status::Unrecorded => "Committed by an agent that left no Witness record",
        Status::Unverified => "Agent-written; nothing covering it passed afterwards",
        Status::Compiled => "Only a build passed after the agent's last edit",
        Status::Tested => "A covering test passed after the agent's last edit",
        Status::Human => "Not written by an agent",
    }
}
