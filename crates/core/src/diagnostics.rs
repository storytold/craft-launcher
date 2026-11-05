//! The UI's bounded context for a local desktop diagnostic report; no I/O here.

use crate::protocol::Source;

/// Upper bound for a preview and a saved report.
pub const MAX_REPORT_BYTES: usize = 128 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagnosticContext {
    pub language: String,
    pub auto_update: bool,
    pub prereleases: bool,
    pub checking: bool,
    pub last_check: Option<i64>,
    pub last_source: Option<Source>,
    pub rate_limited_until: Option<i64>,
    pub include_logs: bool,
}
