//! Adapters that post a Witness report to a code review platform.
//!
//! Only posting differs per platform; the report itself is computed from
//! plain git. Each adapter should find its previous comment by
//! [`REPORT_MARKER`] and update it rather than add a new one.

use std::cell::RefCell;
use std::io::Write;

pub use witness_core::REPORT_MARKER;

pub mod github;
pub mod gitlab;
pub mod http;

/// A rendered report, ready to post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedReport {
    /// Markdown body; starts with [`REPORT_MARKER`].
    pub markdown: String,
}

/// Where to post. Fields are optional because each platform needs a
/// different subset, and v0 adapters can also infer them from CI variables.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PostTarget {
    /// `owner/repo` on GitHub, project path or id on GitLab.
    pub repository: Option<String>,
    /// Pull request or merge request number.
    pub number: Option<u64>,
}

/// What a post did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostOutcome {
    Created { url: Option<String> },
    Updated { url: Option<String> },
    DryRun,
}

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("report body is missing the {REPORT_MARKER} marker")]
    MissingMarker,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Http(String),
    #[error("don't know {0}; pass it on the command line or run in CI")]
    Missing(&'static str),
    /// The token can't do this (401/403), e.g. a pull request from a fork.
    #[error("not allowed: {0}")]
    Forbidden(String),
    #[error("HTTP {status}: {message}")]
    Status { status: u16, message: String },
}

/// A code review platform.
pub trait Platform {
    fn name(&self) -> &'static str;
    fn post(
        &self,
        report: &RenderedReport,
        target: &PostTarget,
    ) -> Result<PostOutcome, PlatformError>;
}

/// Prints what would be posted instead of posting it.
pub struct DryRun<W: Write> {
    out: RefCell<W>,
}

impl<W: Write> DryRun<W> {
    pub fn new(out: W) -> Self {
        Self {
            out: RefCell::new(out),
        }
    }

    pub fn into_inner(self) -> W {
        self.out.into_inner()
    }
}

impl<W: Write> Platform for DryRun<W> {
    fn name(&self) -> &'static str {
        "dry-run"
    }

    fn post(
        &self,
        report: &RenderedReport,
        target: &PostTarget,
    ) -> Result<PostOutcome, PlatformError> {
        if !report.markdown.starts_with(REPORT_MARKER) {
            return Err(PlatformError::MissingMarker);
        }
        let mut out = self.out.borrow_mut();
        let repo = target.repository.as_deref().unwrap_or("<repository>");
        let number = target
            .number
            .map_or_else(|| "<number>".to_owned(), |n| n.to_string());
        writeln!(
            out,
            "dry-run: would create or update the comment marked {REPORT_MARKER} on {repo}#{number} with:"
        )?;
        writeln!(out, "----")?;
        write!(out, "{}", report.markdown)?;
        if !report.markdown.ends_with('\n') {
            writeln!(out)?;
        }
        writeln!(out, "----")?;
        Ok(PostOutcome::DryRun)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> RenderedReport {
        RenderedReport {
            markdown: format!("{REPORT_MARKER}\n### Witness review plan\n"),
        }
    }

    #[test]
    fn dry_run_prints_body_and_target() {
        let platform = DryRun::new(Vec::new());
        let target = PostTarget {
            repository: Some("acme/app".into()),
            number: Some(42),
        };
        assert_eq!(
            platform.post(&report(), &target).unwrap(),
            PostOutcome::DryRun
        );
        let printed = String::from_utf8(platform.into_inner()).unwrap();
        assert!(printed.contains("acme/app#42"));
        assert!(printed.contains("### Witness review plan"));
    }

    #[test]
    fn dry_run_requires_marker() {
        let platform = DryRun::new(Vec::new());
        let bad = RenderedReport {
            markdown: "hi".into(),
        };
        assert!(matches!(
            platform.post(&bad, &PostTarget::default()),
            Err(PlatformError::MissingMarker)
        ));
    }

    use std::cell::RefCell;

    use http::{Http, Request, Response};

    /// Answers requests from a script and records them.
    struct FakeHttp {
        responses: RefCell<Vec<Response>>,
        seen: RefCell<Vec<(String, String, Option<String>)>>,
    }

    impl FakeHttp {
        fn new(responses: Vec<(u16, &str)>) -> Self {
            Self {
                responses: RefCell::new(
                    responses
                        .into_iter()
                        .rev()
                        .map(|(status, body)| Response {
                            status,
                            body: body.to_owned(),
                        })
                        .collect(),
                ),
                seen: RefCell::new(Vec::new()),
            }
        }
        fn requests(&self) -> Vec<(String, String)> {
            self.seen
                .borrow()
                .iter()
                .map(|(m, u, _)| (m.clone(), u.clone()))
                .collect()
        }
    }

    impl Http for FakeHttp {
        fn send(&self, request: &Request<'_>) -> Result<Response, PlatformError> {
            self.seen.borrow_mut().push((
                request.method.to_owned(),
                request.url.to_owned(),
                request.body.clone(),
            ));
            self.responses
                .borrow_mut()
                .pop()
                .ok_or_else(|| PlatformError::Http("no scripted response".into()))
        }
    }

    fn target() -> PostTarget {
        PostTarget {
            repository: Some("acme/app".into()),
            number: Some(7),
        }
    }

    #[test]
    fn github_updates_the_marked_comment() {
        let http = FakeHttp::new(vec![
            (
                200,
                r#"[{"id": 1, "body": "LGTM"}, {"id": 2, "body": "<!-- witness:report -->\nold"}]"#,
            ),
            (
                200,
                r#"{"id": 2, "html_url": "https://github.com/acme/app/pull/7#issuecomment-2"}"#,
            ),
        ]);
        let gh = github::GitHub {
            http: &http,
            token: "t".into(),
            api_url: "https://api.github.com".into(),
        };
        let outcome = gh.post(&report(), &target()).unwrap();
        assert_eq!(
            outcome,
            PostOutcome::Updated {
                url: Some("https://github.com/acme/app/pull/7#issuecomment-2".into())
            }
        );
        assert_eq!(
            http.requests(),
            vec![
                (
                    "GET".into(),
                    "https://api.github.com/repos/acme/app/issues/7/comments?per_page=100&page=1"
                        .into()
                ),
                (
                    "PATCH".into(),
                    "https://api.github.com/repos/acme/app/issues/comments/2".into()
                ),
            ]
        );
        let body = http.seen.borrow()[1].2.clone().unwrap();
        assert!(body.contains("### Witness review plan"));
    }

    #[test]
    fn github_creates_a_comment_and_reports_forbidden() {
        let http = FakeHttp::new(vec![(200, "[]"), (201, r#"{"id": 9, "html_url": "u"}"#)]);
        let gh = github::GitHub {
            http: &http,
            token: "t".into(),
            api_url: "https://ghe.example.com/api/v3/".into(),
        };
        assert_eq!(
            gh.post(&report(), &target()).unwrap(),
            PostOutcome::Created {
                url: Some("u".into())
            }
        );
        assert_eq!(
            http.requests()[1],
            (
                "POST".into(),
                "https://ghe.example.com/api/v3/repos/acme/app/issues/7/comments".into()
            )
        );

        let http = FakeHttp::new(vec![(
            403,
            r#"{"message": "Resource not accessible by integration"}"#,
        )]);
        let gh = github::GitHub {
            http: &http,
            token: "t".into(),
            api_url: "https://api.github.com".into(),
        };
        assert!(matches!(
            gh.post(&report(), &target()),
            Err(PlatformError::Forbidden(m)) if m.contains("not accessible")
        ));
        assert!(matches!(
            gh.post(&report(), &PostTarget::default()),
            Err(PlatformError::Missing(_))
        ));
    }

    #[test]
    fn gitlab_updates_the_marked_note() {
        let http = FakeHttp::new(vec![
            (
                200,
                r#"[{"id": 5, "system": true, "body": "<!-- witness:report -->"}, {"id": 6, "body": "<!-- witness:report -->"}]"#,
            ),
            (200, r#"{"id": 6}"#),
        ]);
        let gl = gitlab::GitLab {
            http: &http,
            token: "t".into(),
            api_url: "https://gitlab.com/api/v4".into(),
            mr_url: Some("https://gitlab.com/acme/app/-/merge_requests/7".into()),
        };
        let outcome = gl.post(&report(), &target()).unwrap();
        assert_eq!(
            outcome,
            PostOutcome::Updated {
                url: Some("https://gitlab.com/acme/app/-/merge_requests/7#note_6".into())
            }
        );
        assert_eq!(
            http.requests()[1],
            (
                "PUT".into(),
                "https://gitlab.com/api/v4/projects/acme%2Fapp/merge_requests/7/notes/6".into()
            )
        );
    }
}
