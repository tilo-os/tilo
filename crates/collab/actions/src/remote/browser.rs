//! A workspace's Browser, started on the member device that runs
//! the workspace's VM (`docs/hosted-browser.md`, "Where it runs").
//!
//! A workspace hosted on a relay opens Browser like any app, and the page has
//! to run somewhere: in the workspace's own VM, on the host that
//! keeps that VM. The relay asks that host's member device over
//! the device plane, as the device it is, the way it asks for a shell command
//! ([`environment_runs`](super::environment_runs)):
//!
//! - [`target`] names a Browser open, an action of the workspace's
//!   environment ([`RESOURCE`]); the argument is an [`OpenBrowser`].
//! - It succeeds with an [`OpenedBrowser`] once the session is
//!   up in the workspace: what opening took, whether this call opened it or
//!   attached to the one already serving the workspace, and the provider
//!   token of the window it holds (what a page action names). A host with no
//!   room answers
//!   [`Unavailable`](super::ActionHostError::Unavailable) with the words to
//!   wait on.
//! - [`STOP`] ends the session; the VM then sleeps like any idle VM.
//!
//! The session in the workspace's VM reports through its standard output, one
//! line per event ([`Report`]): [`READY`] once its engine is up with no
//! workspace, [`UP`] with the window's provider once it has joined and opened
//! the window, and [`USED`] whenever the page is used. [`MARK`] is set in its
//! environment, so the session knows it holds a hosted Browser.
//!
//! Only the workspace's host asks, as a device of the operator the member's
//! own identity belongs to: the workspace it opens is the caller's, never one
//! the request names.

use serde::{Deserialize, Serialize};

use super::RemoteActionTarget;
use crate::door::ActionAddress;

/// The provider a member answers the workspaces' Browser under.
pub const PROVIDER: &str = "tilo.environment";
/// The resource opening one is an action of: the workspace's environment.
pub const RESOURCE: &str = "environment";
/// Start the browser session in a workspace's VM, or attach to it.
pub const OPEN: &str = "open-browser";
/// End it now.
pub const STOP: &str = "stop-browser";
/// The page action that lets a held request leave, by the id the window
/// published it with: what a departure nothing asked for is answered with.
pub const SEND_HELD: &str = "send-held-request";
/// The page action that drops a held request, leaving the page as it was.
pub const KEEP_HELD: &str = "keep-held-request";

/// Set in the session's own environment: it serves a browser
/// environment, and reports on its standard output.
pub const MARK: &str = "TILO_HOSTED_BROWSER";
/// The line the session prints once its engine is up and it waits for a
/// workspace.
pub const READY: &str = "ready";
/// The word the session prints with the window's provider once it has joined
/// its workspace and opened the window: `up <provider>`.
pub const UP: &str = "up";
/// The line the session prints whenever the page is used: an action ran, or
/// somebody is viewing it.
pub const USED: &str = "used";

/// What the session in a workspace's VM printed on its standard output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// Its engine is up, and it waits for a workspace.
    Ready,
    /// It joined its workspace and opened the window, whose provider token
    /// this is: what a page action names in place of the app.
    Up { provider: String },
    /// The page was used: an action ran, or somebody views it.
    Used,
}

impl Report {
    /// The report `line` is, if it is one.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim();
        if line == READY {
            return Some(Self::Ready);
        }
        if line == USED {
            return Some(Self::Used);
        }
        let provider = line.strip_prefix(UP)?.strip_prefix(' ')?.trim().to_owned();
        (!provider.is_empty() && !provider.contains(char::is_whitespace))
            .then_some(Self::Up { provider })
    }

    /// The line it prints, without its end.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Self::Ready => READY.to_owned(),
            Self::Up { provider } => format!("{UP} {provider}"),
            Self::Used => USED.to_owned(),
        }
    }
}

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

/// A workspace's Browser to open, and what its session joins the
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

/// A workspace's Browser, up.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenedBrowser {
    /// What opening took, when this call opened it; absent when it attached
    /// to the session already running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened_ms: Option<u64>,
    /// The provider token of the window the session holds (`42`): what a
    /// page action on it is addressed to.
    pub provider: String,
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
    }

    #[test]
    fn an_opened_environment_names_its_window() {
        // An opening that attached says nothing about time, but always names
        // the window it attached to.
        assert_eq!(
            serde_json::to_string(&OpenedBrowser {
                opened_ms: None,
                provider: "42".into()
            })
            .unwrap(),
            r#"{"provider":"42"}"#
        );
        assert_eq!(
            serde_json::to_string(&OpenedBrowser {
                opened_ms: Some(900),
                provider: "42".into()
            })
            .unwrap(),
            r#"{"opened_ms":900,"provider":"42"}"#
        );
    }

    #[test]
    fn a_session_reports_its_lines_and_reads_them_back() {
        for report in [
            Report::Ready,
            Report::Up {
                provider: "42".into(),
            },
            Report::Used,
        ] {
            assert_eq!(Report::parse(&report.line()), Some(report));
        }
        assert_eq!(Report::parse(" ready\n"), Some(Report::Ready));
        assert_eq!(
            Report::parse("up 42"),
            Some(Report::Up {
                provider: "42".into()
            }),
            "the window's provider is what the line carries"
        );
        // and nothing else is a report
        for line in ["up", "up ", "uptime", "used it", "ready now", "notup"] {
            assert_eq!(Report::parse(line), None, "{line:?}");
        }
    }
}
