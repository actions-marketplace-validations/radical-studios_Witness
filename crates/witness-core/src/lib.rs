//! Witness core: record format, hook capture, event-to-hunk matching,
//! classification and report rendering. Pure logic, no I/O.

pub mod capture;
pub mod classify;
pub mod command;
pub mod config;
pub mod history;
pub mod hunk;
pub mod record;
pub mod report;
pub mod timeline;

pub use classify::{Classifier, Status, V0Rules};
pub use config::Config;
pub use hunk::Hunk;
pub use record::{Event, EventKind, LineRange};
pub use report::{REPORT_MARKER, Report};
