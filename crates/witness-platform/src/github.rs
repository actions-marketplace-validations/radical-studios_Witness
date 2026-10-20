//! GitHub pull request comments, through the REST API.
//!
//! Lists the PR's issue comments, finds the one containing
//! [`crate::REPORT_MARKER`], then updates it or creates a new one.

use serde_json::{Value, json};

use crate::http::{Http, Request, Response};
use crate::{Platform, PlatformError, PostOutcome, PostTarget, REPORT_MARKER, RenderedReport};

/// Comments per page when looking for the previous report.
const PER_PAGE: usize = 100;
/// Give up looking after this many pages.
const MAX_PAGES: usize = 30;

pub struct GitHub<'a> {
    pub http: &'a dyn Http,
    pub token: String,
    /// `https://api.github.com`, or a GitHub Enterprise Server API URL.
    pub api_url: String,
}

impl GitHub<'_> {
    fn request(
        &self,
        method: &'static str,
        url: &str,
        body: Option<Value>,
    ) -> Result<Value, PlatformError> {
        let request = Request {
            method,
            url,
            headers: vec![
                ("Accept", "application/vnd.github+json".to_owned()),
                ("Authorization", format!("Bearer {}", self.token)),
                ("X-GitHub-Api-Version", "2022-11-28".to_owned()),
                (
                    "User-Agent",
                    format!("witness/{}", env!("CARGO_PKG_VERSION")),
                ),
                ("Content-Type", "application/json".to_owned()),
            ],
            body: body.map(|b| b.to_string()),
        };
        parse(self.http.send(&request)?)
    }
}

fn parse(response: Response) -> Result<Value, PlatformError> {
    let value: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
    if response.is_success() {
        return Ok(value);
    }
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or(response.body.as_str())
        .to_owned();
    Err(match response.status {
        401 | 403 => PlatformError::Forbidden(message),
        status => PlatformError::Status { status, message },
    })
}

impl Platform for GitHub<'_> {
    fn name(&self) -> &'static str {
        "github"
    }

    fn post(
        &self,
        report: &RenderedReport,
        target: &PostTarget,
    ) -> Result<PostOutcome, PlatformError> {
        if !report.markdown.starts_with(REPORT_MARKER) {
            return Err(PlatformError::MissingMarker);
        }
        let repo = target
            .repository
            .as_deref()
            .ok_or(PlatformError::Missing("the repository (owner/repo)"))?;
        let number = target
            .number
            .ok_or(PlatformError::Missing("the pull request number"))?;
        let api = self.api_url.trim_end_matches('/');

        let mut existing: Option<u64> = None;
        for page in 1..=MAX_PAGES {
            let url = format!(
                "{api}/repos/{repo}/issues/{number}/comments?per_page={PER_PAGE}&page={page}"
            );
            let comments = self.request("GET", &url, None)?;
            let list = comments.as_array().cloned().unwrap_or_default();
            if let Some(found) = list.iter().find(|c| {
                c.get("body")
                    .and_then(Value::as_str)
                    .is_some_and(|b| b.contains(REPORT_MARKER))
            }) {
                existing = found.get("id").and_then(Value::as_u64);
                break;
            }
            if list.len() < PER_PAGE {
                break;
            }
        }
        let body = json!({ "body": report.markdown });
        let html_url = |v: &Value| v.get("html_url").and_then(Value::as_str).map(str::to_owned);
        match existing {
            Some(id) => {
                let url = format!("{api}/repos/{repo}/issues/comments/{id}");
                let updated = self.request("PATCH", &url, Some(body))?;
                Ok(PostOutcome::Updated {
                    url: html_url(&updated),
                })
            }
            None => {
                let url = format!("{api}/repos/{repo}/issues/{number}/comments");
                let created = self.request("POST", &url, Some(body))?;
                Ok(PostOutcome::Created {
                    url: html_url(&created),
                })
            }
        }
    }
}
