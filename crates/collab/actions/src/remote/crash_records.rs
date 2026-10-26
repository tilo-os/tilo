//! What this computer recorded of the times it went wrong: a desktop that
//! stopped responding or stopped unexpectedly, a part of it that restarted,
//! the computer itself stopping (the kernel's record from before a reset),
//! and a fault the system's core went on from.
//!
//! The records are files the host keeps in the person's private crash
//! directory; nothing here sends one anywhere. The host answers two steps
//! under its own provider: [`LIST`] says what each record is without its
//! text, and [`READ`] returns one record's text, the stacks included, only
//! when it is asked for by name. Settings lists them through the same door
//! an agent or a script uses.

use serde::{Deserialize, Serialize};

use super::RemoteActionTarget;
use crate::door::ActionAddress;

/// The provider the host answers the records under.
pub const PROVIDER: &str = "tilo.crash-records";
/// The resource the steps are actions of.
pub const RESOURCE: &str = "crash-records";
/// Every record, newest first, as [`CrashRecord`]s without their text.
pub const LIST: &str = "list";
/// One record's text (`id`), as a [`CrashRecordText`].
pub const READ: &str = "read";

/// Every step, in the order a catalogue lists them.
pub const STEPS: [&str; 2] = [LIST, READ];

#[must_use]
pub fn target(step: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, step),
    }
}

/// What a record says happened.
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

/// What the kernel's record says it met: why the computer stopped, or the
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

/// One record, as [`LIST`] answers it: what happened and when, without the
/// text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashRecord {
    /// The record's name, which [`READ`] takes.
    pub id: String,
    /// When it happened, in seconds since the Unix epoch. For the computer
    /// stopping, when it is not recorded, the start that found the record.
    pub at: i64,
    pub kind: CrashKind,
    /// What the kernel met, for the computer stopping or a fault of the
    /// system's core, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<StopReason>,
    /// The version of the desktop that wrote the record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The exact build that wrote it: the program's build id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
}

/// One record's text, as [`READ`] answers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashRecordText {
    pub record: CrashRecord,
    /// The record as it was written: its facts, then every stack or the
    /// system's log.
    pub text: String,
    /// Whether the text is the start of a longer record.
    #[serde(default)]
    pub truncated: bool,
}
