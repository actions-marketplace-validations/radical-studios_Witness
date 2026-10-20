//! GitHub pull request comments. Stub.
//!
//! Planned: list issue comments on the PR, find the one containing
//! [`crate::REPORT_MARKER`], then PATCH it or POST a new one, using
//! `GITHUB_TOKEN`, `GITHUB_REPOSITORY` and the PR number from the event.

use crate::{Platform, PlatformError, PostOutcome, PostTarget, RenderedReport};

#[derive(Debug, Clone, Copy, Default)]
pub struct GitHub;

impl Platform for GitHub {
    fn name(&self) -> &'static str {
        "github"
    }

    fn post(
        &self,
        _report: &RenderedReport,
        _target: &PostTarget,
    ) -> Result<PostOutcome, PlatformError> {
        Err(PlatformError::NotImplemented("github"))
    }
}
