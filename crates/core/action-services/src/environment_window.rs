//! What a workspace VM's daemon answers about the sessions it runs (`docs/hosted-browser.md`).
//!
//! A hosted workspace's VM runs one daemon, a device of the person confined to that workspace,
//! and the daemon runs the VM's sessions as its shells: one holding the workspace's Browser
//! window, as the image's browser account, and one holding its Linux apps' windows, as the
//! workspace's tenant. The host that placed the workspace asks the daemon through the
//! workspace's dispatch, as it asks any device:
//!
//! - [`LAUNCH`] opens something in one of them ([`Launch`]): Browser on a record, or a Linux app
//!   by its desktop file. The session starts on its first launch.
//! - [`WINDOW`] reads what the sessions show ([`Window`]): whether each runs, the provider its
//!   window is listed under, which is what a page action names, and when it was last used.
//! - [`STOP`] ends one of them now.
//!
//! The sessions are the daemon's clients and ask it over their shell link, under [`SESSIONS`],
//! which only a shell on the VM reaches: [`NEXT_LAUNCH`] answers what was launched for one since
//! it last asked, and [`REPORT`] tells the daemon its window is up or in use ([`Report`]). A
//! session ended after it was unused for a while, or alive for its lifetime, and once none runs
//! the daemon ends too, and its VM sleeps.
//!
//! What the VM holds no keys for, a session asks of the host through its daemon, which asks the
//! host as this device ([`crate::environment_device`]) and opens what the host sealed to the
//! device's key: [`USE_ITEM`] a vault item's text for one fill of a page ([`ItemLent`]), and
//! [`SHARE_WINDOW`] the share of one of its windows ([`Share`]).

use serde::{Deserialize, Serialize};

use crate::linux_apps::AppLaunch;
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::RemoteActionTarget;

/// The provider a workspace VM's daemon answers the host's asks under: [`LAUNCH`], [`WINDOW`]
/// and [`STOP`].
pub const PROVIDER: &str = "tilo.vm";
/// The provider it answers its own sessions' asks under, on the VM alone: [`NEXT_LAUNCH`],
/// [`REPORT`], [`USE_ITEM`] and [`SHARE_WINDOW`].
pub const SESSIONS: &str = "tilo.vm.sessions";
/// The resource they are actions of: the VM's sessions.
pub const RESOURCE: &str = "environment";
/// Open something in one of the VM's sessions, starting it when none runs.
pub const LAUNCH: &str = "launch";
/// Read what the VM's sessions show.
pub const WINDOW: &str = "window";
/// End one of the VM's sessions now.
pub const STOP: &str = "stop";
/// What a session asks: what was launched for it since it last asked.
pub const NEXT_LAUNCH: &str = "next-launch";
/// What a session tells: its window is up, or in use.
pub const REPORT: &str = "report";
/// What a session asks for one fill of a page: a vault item's text, opened.
pub const USE_ITEM: &str = "use-item";
/// What a session asks of one of its windows: that the host share it, or end its share.
pub const SHARE_WINDOW: &str = "share-window";
/// The provider a workspace VM's daemon takes what concerns the device itself from the host that
/// placed it under, on every such VM: [`RENEW`].
pub const PLACEMENT: &str = "tilo.vm.placement";
/// What the host that placed the VM hands it before the device's certificate ends: the same
/// device certified again ([`Renewal`]), which the daemon holds from then on.
pub const RENEW: &str = "renew";
/// Set in the environment of a daemon that is a workspace VM's: it answers these.
pub const MARK: &str = "TILO_VM_ENVIRONMENT";
/// Set in the environment of a session the VM's daemon starts: the workspace it serves.
pub const WORKSPACE: &str = "TILO_VM_WORKSPACE";

/// Which of a VM's sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Holds {
    /// The workspace's Browser window, as the image's browser account.
    Browser,
    /// The windows of the workspace's Linux apps, as its tenant.
    Apps,
}

impl Holds {
    /// The mark the session's environment carries, by which it knows what it holds.
    #[must_use]
    pub const fn mark(self) -> &'static str {
        match self {
            Self::Browser => crate::browser::MARK,
            Self::Apps => crate::linux_apps::MARK,
        }
    }
}

/// What [`LAUNCH`] opens, and what [`NEXT_LAUNCH`] answers a session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    /// The workspace it opens in: the one the VM serves.
    pub workspace: String,
    /// The session it opens in.
    pub holds: Holds,
    /// The record Browser opens on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
    /// The Linux app to start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<AppLaunch>,
}

impl Launch {
    /// Whether it names what its session opens: a record for Browser, an app for the apps
    /// session, and nothing else.
    #[must_use]
    pub fn is_whole(&self) -> bool {
        match self.holds {
            Holds::Browser => self.record.is_some() && self.app.is_none(),
            Holds::Apps => self.app.is_some() && self.record.is_none(),
        }
    }
}

/// What one of a VM's sessions shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shown {
    /// Whether it runs.
    pub running: bool,
    /// The provider its window is listed under once it is up (`42`): what a page action names.
    /// An apps session holds no one window, and says [`crate::linux_apps::UP_PROVIDER`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// When it was last used, in Unix milliseconds: launched, up, or reported in use.
    #[serde(default)]
    pub used_ms: u64,
}

/// What [`WINDOW`] answers: each of the VM's sessions.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub browser: Shown,
    pub apps: Shown,
}

impl Window {
    /// What the session `holds` shows.
    #[must_use]
    pub const fn of(&self, holds: Holds) -> &Shown {
        match holds {
            Holds::Browser => &self.browser,
            Holds::Apps => &self.apps,
        }
    }
}

/// What a session tells its daemon ([`REPORT`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Report {
    /// Its window is up, listed under `provider`.
    Up { holds: Holds, provider: String },
    /// It is in use: a page action ran, somebody views it, or an app's window is open.
    Used { holds: Holds },
}

/// What [`NEXT_LAUNCH`] and [`STOP`] name: one of the sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub holds: Holds,
}

/// What [`USE_ITEM`] answers: the item's text for the field it fills, that the person is asked
/// first, or why nothing was released, each in words a person is shown.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ItemLent {
    /// The text, and the kind of field it is for, in the engine's word: `password`, `code` or
    /// `value`.
    Text { text: String, into: String },
    /// The person is asked first, and nothing was released.
    Asked { words: String },
    /// Nothing was released, and why.
    Refused { reason: String },
}

impl std::fmt::Debug for ItemLent {
    /// Never the text.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text { into, .. } => formatter
                .debug_struct("Text")
                .field("into", into)
                .finish_non_exhaustive(),
            Self::Asked { words } => formatter
                .debug_struct("Asked")
                .field("words", words)
                .finish(),
            Self::Refused { reason } => formatter
                .debug_struct("Refused")
                .field("reason", reason)
                .finish(),
        }
    }
}

/// What [`SHARE_WINDOW`] names: the app the window is of, its tab as the session minted it, and
/// whether it is shared from now on or its share ends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Share {
    pub app: String,
    pub tab: u64,
    pub shared: bool,
}

/// What [`RENEW`] hands the VM: the device's new certificate, as its JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Renewal {
    pub certificate: String,
}

/// What `action` of the VM's sessions is invoked on: under [`SESSIONS`] for a session's own asks,
/// under [`PLACEMENT`] for the device's renewal, under [`PROVIDER`] for the host's other asks.
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    let provider = if matches!(action, NEXT_LAUNCH | REPORT | USE_ITEM | SHARE_WINDOW) {
        SESSIONS
    } else if action == RENEW {
        PLACEMENT
    } else {
        PROVIDER
    };
    RemoteActionTarget {
        provider: provider.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
}

/// `value` as an action's argument: an object.
#[must_use]
pub fn argument(value: &impl Serialize) -> tilo_actions::remote::InvokeArgument {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::Object(named)) => tilo_actions::remote::InvokeArgument::Json(named),
        _ => tilo_actions::remote::InvokeArgument::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launch_names_what_its_session_opens_and_reads_back() {
        let browser = Launch {
            workspace: "ws".into(),
            holds: Holds::Browser,
            record: Some("uuid:1".into()),
            app: None,
        };
        assert!(browser.is_whole());
        assert_eq!(
            serde_json::to_value(&browser).unwrap(),
            serde_json::json!({"workspace": "ws", "holds": "browser", "record": "uuid:1"})
        );
        let app = Launch {
            workspace: "ws".into(),
            holds: Holds::Apps,
            record: None,
            app: Some(AppLaunch::new("xterm").unwrap()),
        };
        assert!(app.is_whole());
        assert_eq!(
            serde_json::from_value::<Launch>(serde_json::to_value(&app).unwrap()).unwrap(),
            app
        );
        for half in [
            Launch {
                record: None,
                ..browser.clone()
            },
            Launch {
                app: app.app.clone(),
                ..browser
            },
            Launch { app: None, ..app },
        ] {
            assert!(!half.is_whole(), "{half:?}");
        }
        assert_eq!(
            serde_json::to_value(Report::Up {
                holds: Holds::Browser,
                provider: "42".into()
            })
            .unwrap(),
            serde_json::json!({"up": {"holds": "browser", "provider": "42"}})
        );
    }
}
