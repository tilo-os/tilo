//! A workspace's browser environment, started on the member device that runs
//! the workspace's VMs (`docs/hosted-browser.md`, "Where it runs").
//!
//! A workspace hosted on a relay opens Browser like any app, and the page has
//! to run somewhere: in a microVM of the workspace's own, on the host that
//! keeps the workspace's VMs. The relay asks that host's member device over
//! the device plane, as the device it is, the way it asks for a shell command
//! ([`environment_runs`](super::environment_runs)):
//!
//! - [`target`] names an environment's open, an action of the workspace's
//!   environment ([`RESOURCE`]); the argument is an [`OpenBrowser`].
//! - It succeeds with an [`OpenedBrowser`] once the environment's session is
//!   up in the workspace: what opening took, and whether this call opened it
//!   or attached to the one already serving the workspace. A host with no
//!   room answers
//!   [`Unavailable`](super::ActionHostError::Unavailable) with the words to
//!   wait on.
//! - [`STOP`] ends it, with everything it wrote.
//!
//! Only the workspace's host asks, as a device of the operator the member's
//! own identity belongs to: the workspace it opens is the caller's, never one
//! the request names.

use serde::{Deserialize, Serialize};

use super::RemoteActionTarget;
use crate::door::ActionAddress;

/// The provider a member answers browser environments under.
pub const PROVIDER: &str = "tilo.environment";
/// The resource opening one is an action of: the workspace's environment.
pub const RESOURCE: &str = "environment";
/// Start a workspace's browser environment, or attach to it.
pub const OPEN: &str = "open-browser";
/// End it now.
pub const STOP: &str = "stop-browser";

/// What [`OPEN`] is invoked on.
#[must_use]
pub fn target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, OPEN),
    }
}

/// What [`STOP`] is invoked on.
#[must_use]
pub fn stop_target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, STOP),
    }
}

/// A workspace's browser environment to open, and what its session joins the
/// workspace with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenBrowser {
    /// The workspace whose Browser it opens: the caller's.
    pub workspace: String,
    /// Who the machine's time is booked to, as the relay knows the account.
    #[serde(default)]
    pub account: String,
    /// What the session joins the workspace with — the workspace, the record
    /// to open, the device certificate — opaque to the host running it and to
    /// the VM service: one line of the session's input.
    pub material: String,
}

/// A workspace's browser environment, up.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenedBrowser {
    /// What opening took, when this call opened it; absent when it attached
    /// to the environment already serving the workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_browser_open_names_the_environments_own_action() {
        assert_eq!(target().provider, PROVIDER);
        assert_eq!(target().address.resource, RESOURCE);
        assert_eq!(target().address.action, OPEN);
        assert_eq!(stop_target().address.action, STOP);
        // An opening that attached says nothing about time.
        assert_eq!(
            serde_json::to_string(&OpenedBrowser { opened_ms: None }).unwrap(),
            "{}"
        );
        assert_eq!(
            serde_json::to_string(&OpenedBrowser {
                opened_ms: Some(900)
            })
            .unwrap(),
            r#"{"opened_ms":900}"#
        );
    }
}
