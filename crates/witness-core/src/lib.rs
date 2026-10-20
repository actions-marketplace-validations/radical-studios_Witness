//! Witness core: record format, hook capture, event-to-hunk matching,
//! classification and report rendering. Pure logic, no I/O.

pub mod capture;
pub mod classify;
pub mod command;
pub mod config;
pub mod history;
pub mod hunk;
pub mod provenance;
pub mod record;
pub mod report;
pub mod scope;
pub mod signals;
pub mod testsum;
pub mod timeline;

pub use classify::{Classifier, DefaultRules, Status};
pub use config::Config;
pub use hunk::Hunk;
pub use record::{Event, EventKind, LineRange};
pub use report::{REPORT_MARKER, Report};
