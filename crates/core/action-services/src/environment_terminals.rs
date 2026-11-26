//! A Terminal session's shell on a terminal, opened on the member device that takes the
//! workspace's work (`docs/app-model/environments.md`, "Members that take work").
//!
//! A person typing in a hosted workspace's Terminal needs the shell on a terminal, so prompts,
//! editors and `claude` work as on a device. The keys and what is drawn are no actions: they go
//! over a stream of their own (`ush::environment_terminals`). What the server asks of the
//! member over the device action service is only the open:
//!
//! - [`target`] opens one: the argument is an [`OpenTerminal`]. It succeeds with a
//!   [`TerminalOpened`]: the one-use id of a stream the member hands to the device that asked,
//!   and the session's own id, to come back to it.
//! - The server then dials the member's terminal stream as that same device and presents the
//!   stream id. Nobody else may take it.
//! - An open that started the session's shell also hands the server the session's runs, on a
//!   stream of their own the server follows the same way: each command line the shell ran, as
//!   it ended, for the server to record on the session. A device attached or not, every run
//!   reaches it once.
//!
//! Only the workspace's server may ask, as a device of the operator the member's own identity
//! belongs to: the shell's VM is paid for by the account the request names, which only the
//! server knows.

use serde::{Deserialize, Serialize};

use tilo_actions::api::ActionAddress;
use tilo_actions::remote::RemoteActionTarget;

/// The provider the member answers a workspace's terminals under: the one its commands are.
pub const PROVIDER: &str = crate::environment_commands::PROVIDER;
/// The resource a terminal is an action of: the workspace's environment.
pub const RESOURCE: &str = crate::environment_commands::RESOURCE;
/// Open a session's shell on a terminal.
pub const OPEN: &str = "open-terminal";

/// What [`OPEN`] is invoked on.
#[must_use]
pub fn target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, OPEN),
    }
}

/// A session's shell to open on a terminal, in the workspace asked in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenTerminal {
    /// The session whose shell runs on it: the one its commands run in.
    pub session: String,
    /// The window's size.
    pub cols: u16,
    pub rows: u16,
    /// The id of the session to come back to, when this is no new one: it must still be
    /// kept, for the same session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach: Option<String>,
    /// Who the shell runs as: the person or agent who opened it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<crate::environment_commands::Person>,
    /// Who pays for the workspace's VM, as the server knows it.
    pub account: String,
    /// The size of the workspace's VM (`small`, `default`, `large`, `xlarge`).
    pub size: String,
    /// What every program the session's shell starts in the VM gets in its environment
    /// besides the shell's variables: read as the session starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<crate::environment_commands::VmSecret>,
}

/// An opened terminal: what the device that asked presents on the stream, and the session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalOpened {
    /// The one-use id of the stream, for the device that asked alone.
    pub stream: String,
    /// The session's id, 32 hex characters, to come back to it.
    pub session: String,
    /// The one-use id of the stream of the session's runs, when this open started its shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<String>,
}
