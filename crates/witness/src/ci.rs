//! What the CI environment says about the change being built.

use std::path::{Path, PathBuf};

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    GitHub,
    GitLab,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CiContext {
    pub host: Option<Host>,
    /// `owner/repo` on GitHub; the project id on GitLab.
    pub repository: Option<String>,
    /// Pull request number or merge request IID.
    pub number: Option<u64>,
    pub api_url: Option<String>,
    /// The change's own head commit (GitHub checks out a merge commit).
    pub head_sha: Option<String>,
    /// Web URL of the merge request (GitLab).
    pub mr_url: Option<String>,
    /// GitHub Actions job summary file.
    pub step_summary: Option<PathBuf>,
}

/// Reads the CI context from environment variables (`env`) and, on GitHub
/// Actions, the event payload file (`read`).
pub fn detect(
    env: &dyn Fn(&str) -> Option<String>,
    read: &dyn Fn(&Path) -> Option<String>,
) -> CiContext {
    let var = |name: &str| env(name).filter(|v| !v.is_empty());
    if var("GITHUB_ACTIONS").as_deref() == Some("true") {
        let event: Value = var("GITHUB_EVENT_PATH")
            .and_then(|p| read(Path::new(&p)))
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null);
        let number = event
            .pointer("/pull_request/number")
            .or_else(|| event.pointer("/issue/number"))
            .or_else(|| event.get("number"))
            .and_then(Value::as_u64)
            .or_else(|| {
                var("GITHUB_REF")?
                    .strip_prefix("refs/pull/")?
                    .split('/')
                    .next()?
                    .parse()
                    .ok()
            });
        return CiContext {
            host: Some(Host::GitHub),
            repository: var("GITHUB_REPOSITORY"),
            number,
            api_url: Some(
                var("GITHUB_API_URL").unwrap_or_else(|| "https://api.github.com".to_owned()),
            ),
            head_sha: event
                .pointer("/pull_request/head/sha")
                .and_then(Value::as_str)
                .map(str::to_owned),
            mr_url: None,
            step_summary: var("GITHUB_STEP_SUMMARY").map(PathBuf::from),
        };
    }
    if var("GITLAB_CI").as_deref() == Some("true") {
        let number = var("CI_MERGE_REQUEST_IID").and_then(|n| n.parse().ok());
        let mr_url = match (var("CI_PROJECT_URL"), number) {
            (Some(project), Some(iid)) => Some(format!("{project}/-/merge_requests/{iid}")),
            _ => None,
        };
        return CiContext {
            host: Some(Host::GitLab),
            repository: var("CI_PROJECT_ID").or_else(|| var("CI_PROJECT_PATH")),
            number,
            api_url: var("CI_API_V4_URL"),
            head_sha: var("CI_MERGE_REQUEST_SOURCE_BRANCH_SHA").or_else(|| var("CI_COMMIT_SHA")),
            mr_url,
            step_summary: None,
        };
    }
    CiContext::default()
}

/// [`detect`] on the real environment.
pub fn current() -> CiContext {
    detect(&|name| std::env::var(name).ok(), &|path| {
        std::fs::read_to_string(path).ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(vars: &[(&str, &str)], event: Option<&str>) -> CiContext {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let event = event.map(str::to_owned);
        detect(
            &|name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()),
            &|_| event.clone(),
        )
    }

    #[test]
    fn github_pull_request_events() {
        let c = ctx(
            &[
                ("GITHUB_ACTIONS", "true"),
                ("GITHUB_REPOSITORY", "acme/app"),
                ("GITHUB_EVENT_PATH", "/tmp/event.json"),
                ("GITHUB_STEP_SUMMARY", "/tmp/summary.md"),
            ],
            Some(r#"{"pull_request": {"number": 42, "head": {"sha": "abc"}}}"#),
        );
        assert_eq!(c.host, Some(Host::GitHub));
        assert_eq!(c.repository.as_deref(), Some("acme/app"));
        assert_eq!(c.number, Some(42));
        assert_eq!(c.head_sha.as_deref(), Some("abc"));
        assert_eq!(c.api_url.as_deref(), Some("https://api.github.com"));
        assert_eq!(c.step_summary, Some(PathBuf::from("/tmp/summary.md")));
        // Without an event file, the ref names the PR.
        let c = ctx(
            &[
                ("GITHUB_ACTIONS", "true"),
                ("GITHUB_REF", "refs/pull/7/merge"),
            ],
            None,
        );
        assert_eq!(c.number, Some(7));
    }

    #[test]
    fn gitlab_merge_request_pipelines() {
        let c = ctx(
            &[
                ("GITLAB_CI", "true"),
                ("CI_PROJECT_ID", "123"),
                ("CI_MERGE_REQUEST_IID", "9"),
                ("CI_API_V4_URL", "https://gitlab.example.com/api/v4"),
                ("CI_PROJECT_URL", "https://gitlab.example.com/acme/app"),
            ],
            None,
        );
        assert_eq!(c.host, Some(Host::GitLab));
        assert_eq!(c.repository.as_deref(), Some("123"));
        assert_eq!(c.number, Some(9));
        assert_eq!(
            c.mr_url.as_deref(),
            Some("https://gitlab.example.com/acme/app/-/merge_requests/9")
        );
    }

    #[test]
    fn outside_ci() {
        assert_eq!(ctx(&[], None), CiContext::default());
    }
}
