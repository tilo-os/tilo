//! What this computer recorded of the times it went wrong: a desktop that
//! stopped responding or stopped unexpectedly, a part of it that restarted,
//! the computer itself stopping (the kernel's report from before a reset),
//! and a fault the system's core went on from.
//!
//! The reports are files the host keeps in the person's private crash
//! directory; nothing here sends one anywhere. They read two ways, over the
//! same names: as the session's published resources — [`RESOURCE`] reads as
//! every report without its text ([`CrashReport`]s), [`report`] as one
//! report's text, the stacks included ([`CrashReportText`]) — and, to a
//! caller that reaches the reports as steps, under [`PROVIDER`]: [`LIST`]
//! and [`READ`] answer the same two. Nothing is ever changed.

use serde::{Deserialize, Serialize};

use super::RemoteActionTarget;
use crate::api::ActionAddress;

/// The provider that answers the reports under.
pub const PROVIDER: &str = "tilo.crash-reports";
/// The resource the steps are actions of, and the session's reading of
/// every report, newest first.
pub const RESOURCE: &str = "crash-reports";
/// Every report, newest first, as [`CrashReport`]s without their text.
pub const LIST: &str = "list";
/// One report's text (`id`), as a [`CrashReportText`].
pub const READ: &str = "read";

/// Every step, in the order a catalogue lists them.
pub const STEPS: [&str; 2] = [LIST, READ];

/// What a report's resource id starts with, before its name.
pub const REPORT_PREFIX: &str = "crash-report:";

/// The resource the report `id` reads as: its facts and its text.
#[must_use]
pub fn report(id: &str) -> String {
    format!("{REPORT_PREFIX}{id}")
}

#[must_use]
pub fn target(step: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, step),
    }
}

/// What a report says happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CrashKind {
    /// The desktop stopped responding and was ended, then started again.
    Froze,
    /// The desktop stopped unexpectedly and started again.
    Crashed,
    /// A part of the desktop stopped and started again; the session went on.
    PartRestarted,
    /// The whole computer stopped and started again.
    ComputerStopped,
    /// The system's core met a fault (an oops) and went on running: the
    /// kernel kept its log, but the computer did not stop for it.
    SystemFault,
}

/// What the kernel's report says it met: why the computer stopped, or the
/// fault it went on from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StopReason {
    /// The system met an error it could not recover from.
    Panic,
    /// A fault in the system's core, which it went on from
    /// ([`CrashKind::SystemFault`]).
    Oops,
    /// The system restarted itself at once.
    Emergency,
}

/// One report, as [`LIST`] answers it: what happened and when, without the
/// text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashReport {
    /// The report's name, which [`READ`] takes.
    pub id: String,
    /// When it happened, in seconds since the Unix epoch. For the computer
    /// stopping, when it is not recorded, the start that found the report.
    pub at: i64,
    pub kind: CrashKind,
    /// What the kernel met, for the computer stopping or a fault of the
    /// system's core, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<StopReason>,
    /// The version of the desktop that wrote the report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The exact build that wrote it: the program's build id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
}

/// One report's text, as [`READ`] answers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashReportText {
    pub report: CrashReport,
    /// The report as it was written: its facts, then every stack or the
    /// system's log.
    pub text: String,
    /// Whether the text is the start of a longer report.
    #[serde(default)]
    pub truncated: bool,
}
