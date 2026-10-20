//! GitLab merge request notes. Stub.
//!
//! Planned: list MR notes, find the one containing
//! [`crate::REPORT_MARKER`], then PUT it or POST a new one, using
//! `CI_PROJECT_ID`, `CI_MERGE_REQUEST_IID` and a project access token.

use crate::{Platform, PlatformError, PostOutcome, PostTarget, RenderedReport};

#[derive(Debug, Clone, Copy, Default)]
pub struct GitLab;

impl Platform for GitLab {
    fn name(&self) -> &'static str {
        "gitlab"
    }

    fn post(
        &self,
        _report: &RenderedReport,
        _target: &PostTarget,
    ) -> Result<PostOutcome, PlatformError> {
        Err(PlatformError::NotImplemented("gitlab"))
    }
}
