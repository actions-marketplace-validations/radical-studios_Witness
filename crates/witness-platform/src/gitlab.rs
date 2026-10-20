//! GitLab merge request notes, through the REST API.
//!
//! Lists the MR's notes, finds the one containing [`crate::REPORT_MARKER`],
//! then updates it or creates a new one. Needs a token with `api` scope
//! (a project access token); CI job tokens can't write notes.

use serde_json::{Value, json};

use crate::http::{Http, Request, Response};
use crate::{Platform, PlatformError, PostOutcome, PostTarget, REPORT_MARKER, RenderedReport};

const PER_PAGE: usize = 100;
const MAX_PAGES: usize = 30;

pub struct GitLab<'a> {
    pub http: &'a dyn Http,
    pub token: String,
    /// e.g. `https://gitlab.com/api/v4` (`CI_API_V4_URL` in GitLab CI).
    pub api_url: String,
    /// Web URL of the merge request, for the outcome.
    pub mr_url: Option<String>,
}

impl GitLab<'_> {
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
                ("PRIVATE-TOKEN", self.token.clone()),
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
        .or_else(|| value.get("error"))
        .map(|m| m.as_str().map_or_else(|| m.to_string(), str::to_owned))
        .unwrap_or_else(|| response.body.clone());
    Err(match response.status {
        401 | 403 => PlatformError::Forbidden(message),
        status => PlatformError::Status { status, message },
    })
}

impl Platform for GitLab<'_> {
    fn name(&self) -> &'static str {
        "gitlab"
    }

    fn post(
        &self,
        report: &RenderedReport,
        target: &PostTarget,
    ) -> Result<PostOutcome, PlatformError> {
        if !report.markdown.starts_with(REPORT_MARKER) {
            return Err(PlatformError::MissingMarker);
        }
        let project = target
            .repository
            .as_deref()
            .ok_or(PlatformError::Missing("the project (id or path)"))?;
        // A path like group/app must be URL-encoded.
        let project = project.replace('/', "%2F");
        let iid = target
            .number
            .ok_or(PlatformError::Missing("the merge request IID"))?;
        let api = self.api_url.trim_end_matches('/');
        let notes_url = format!("{api}/projects/{project}/merge_requests/{iid}/notes");

        let mut existing: Option<u64> = None;
        for page in 1..=MAX_PAGES {
            let url =
                format!("{notes_url}?per_page={PER_PAGE}&page={page}&sort=asc&order_by=created_at");
            let notes = self.request("GET", &url, None)?;
            let list = notes.as_array().cloned().unwrap_or_default();
            if let Some(found) = list.iter().find(|n| {
                !n.get("system").and_then(Value::as_bool).unwrap_or(false)
                    && n.get("body")
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
        let url = |id: Option<u64>| {
            let base = self.mr_url.clone()?;
            Some(match id {
                Some(id) => format!("{base}#note_{id}"),
                None => base,
            })
        };
        match existing {
            Some(id) => {
                self.request("PUT", &format!("{notes_url}/{id}"), Some(body))?;
                Ok(PostOutcome::Updated { url: url(Some(id)) })
            }
            None => {
                let created = self.request("POST", &notes_url, Some(body))?;
                let id = created.get("id").and_then(Value::as_u64);
                Ok(PostOutcome::Created { url: url(id) })
            }
        }
    }
}
