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
    #[error("posting to {0} is not implemented yet")]
    NotImplemented(&'static str),
    #[error("report body is missing the {REPORT_MARKER} marker")]
    MissingMarker,
    #[error("could not write dry-run output: {0}")]
    Io(#[from] std::io::Error),
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

    #[test]
    fn real_platforms_are_stubs() {
        for platform in [&github::GitHub as &dyn Platform, &gitlab::GitLab] {
            assert!(matches!(
                platform.post(&report(), &PostTarget::default()),
                Err(PlatformError::NotImplemented(_))
            ));
        }
    }
}
