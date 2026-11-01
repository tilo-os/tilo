//! Offering work to another device of the workspace: the question and the
//! answer.
//!
//! A request this device holds runs here. One it does not hold — an app it
//! has not installed, a record outside what it keeps, a job better run
//! elsewhere — is offered to the devices that may run work for the person,
//! and each answers with what it knows of itself ([`State`]). The answer is
//! a routing preference and nothing more: the device that is picked still
//! decides the real request through its own dispatcher, against the caller
//! it authenticated. So nothing here is signed, and an answer that names
//! another device is never sent.
//!
//! [`State::verdict`] is the one place a decline is decided, in the order a
//! person would state the rules; the requester ranks the willing answers
//! (`tilo_peer_state::placement`).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How a device is powered, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Power {
    /// Stable external power.
    External,
    Charging,
    /// No reliable observation.
    Unknown,
    /// Running from a battery above the critical threshold.
    Battery,
}

/// Why a device will not run the request it was offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decline {
    /// The device that asked. It is never its own remote worker: work with
    /// no other willing device runs where it was asked, or not at all.
    Requester,
    /// Not connected, so the offer never came back.
    Offline,
    /// A store keeps ciphertext and holds no keys: it never runs work.
    StoreOnly,
    /// It does not hold the workspace the request names.
    WorkspaceNotHeld,
    /// It cannot run the app the request's action belongs to.
    AppNotInstalled,
    /// Its person asked the device to avoid discretionary work.
    DataSaving,
    /// Its battery is critically low.
    CriticalBattery,
}

impl Decline {
    /// Stable token for logs and action output.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Requester => "requester",
            Self::Offline => "offline",
            Self::StoreOnly => "store_only",
            Self::WorkspaceNotHeld => "workspace_not_held",
            Self::AppNotInstalled => "app_not_installed",
            Self::DataSaving => "data_saving",
            Self::CriticalBattery => "critical_battery",
        }
    }
}

/// What a device says of itself when it is offered work: the answer to one
/// offer, from its own state.
///
/// The answerer fills the facts it knows of itself; the device that asked
/// overrides the ones it knows better — that the device answered at all
/// (`online`), whether it is the one asking (`requester`), that the
/// workspace was found here (`holds_workspace`), and the round trip it
/// measured (`latency_ms`).
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a separate fact the device reports, not a mode"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// Whether this is the device that asked.
    pub requester: bool,
    /// Whether it is connected right now.
    pub online: bool,
    /// Whether it holds the workspace's keys and state.
    pub holds_workspace: bool,
    /// Whether it can run the request's app.
    pub app_installed: bool,
    /// Whether it is a store of the workspace rather than a participant.
    pub store_role: bool,
    /// Its person asked it to avoid discretionary work.
    pub data_saving: bool,
    /// Bumped-out-of-shape low battery: not while there is anything else.
    pub critical_battery: bool,
    /// Operator preference; higher wins.
    pub priority: i32,
    pub power: Power,
    /// Work already in hand per thousand slots; lower wins.
    pub load_permille: u32,
    /// Measured round trip to the requester; lower wins.
    pub latency_ms: u32,
}

impl State {
    /// What a device that never answered is taken to have said: it is not
    /// there.
    #[must_use]
    pub const fn unreachable() -> Self {
        Self {
            requester: false,
            online: false,
            holds_workspace: false,
            app_installed: false,
            store_role: false,
            data_saving: false,
            critical_battery: false,
            priority: 0,
            power: Power::Unknown,
            load_permille: 0,
            latency_ms: 0,
        }
    }

    /// What this device ranks by, or why it declines.
    ///
    /// # Errors
    ///
    /// The first rule it fails, in the order a person would state them.
    pub fn verdict(&self) -> Result<Rank, Decline> {
        let decline = if self.requester {
            Decline::Requester
        } else if !self.online {
            Decline::Offline
        } else if self.store_role {
            Decline::StoreOnly
        } else if !self.holds_workspace {
            Decline::WorkspaceNotHeld
        } else if !self.app_installed {
            Decline::AppNotInstalled
        } else if self.data_saving {
            Decline::DataSaving
        } else if self.critical_battery {
            Decline::CriticalBattery
        } else {
            return Ok(Rank {
                priority: self.priority,
                power: self.power,
                load_permille: self.load_permille,
                latency_ms: self.latency_ms,
            });
        };
        Err(decline)
    }
}

/// What a willing device was ranked by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rank {
    /// Operator preference; higher wins.
    pub priority: i32,
    /// Power standing; better wins.
    pub power: Power,
    /// Work in hand per thousand slots; lower wins.
    pub load_permille: u32,
    /// Measured round trip; lower wins.
    pub latency_ms: u32,
}

/// What would run on the device that is offered it: an app's action on one
/// of that app's resources.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    /// The app whose action it is.
    pub app: String,
    /// The resource within it.
    pub resource: String,
    /// The action.
    pub action: String,
}

impl Route {
    /// The route to `action` of `resource` in `app`.
    #[must_use]
    pub fn new(
        app: impl Into<String>,
        resource: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self {
            app: app.into(),
            resource: resource.into(),
            action: action.into(),
        }
    }
}

/// One round's question to one device of the workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Query {
    /// The workspace the route would run in: a device that does not hold it
    /// says so.
    pub workspace: String,
    /// Names the round, so a stale answer is known for what it is.
    pub request_id: Uuid,
    /// What would run.
    pub route: Route,
}

#[cfg(test)]
mod tests {
    use super::{Decline, Power, Query, Route, State};

    fn worker() -> State {
        State {
            requester: false,
            online: true,
            holds_workspace: true,
            app_installed: true,
            store_role: false,
            data_saving: false,
            critical_battery: false,
            priority: 0,
            power: Power::External,
            load_permille: 0,
            latency_ms: 20,
        }
    }

    /// Each rule passes a device over with its own reason, in the order a
    /// person would state them, and a healthy device ranks by what it
    /// reported.
    #[test]
    fn each_rule_passes_a_device_over_with_its_reason() {
        type Change = fn(&mut State);
        let cases: [(Change, Decline); 6] = [
            (|w| w.requester = true, Decline::Requester),
            (|w| w.online = false, Decline::Offline),
            (|w| w.store_role = true, Decline::StoreOnly),
            (|w| w.holds_workspace = false, Decline::WorkspaceNotHeld),
            (|w| w.app_installed = false, Decline::AppNotInstalled),
            (|w| w.data_saving = true, Decline::DataSaving),
        ];
        for (change, reason) in cases {
            let mut state = worker();
            change(&mut state);
            assert_eq!(state.verdict(), Err(reason), "{reason:?}");
        }
        assert_eq!(
            worker().verdict().map(|rank| rank.load_permille),
            Ok(0),
            "a healthy device ranks by what it reported"
        );
        assert_eq!(
            State::unreachable().verdict(),
            Err(Decline::Offline),
            "a device that never answered is not there"
        );
    }

    /// A question and its shape survive a trip through the wire, and a
    /// question on the wire never reads as another kind of message.
    #[test]
    fn a_query_and_an_answer_survive_the_wire() {
        let query = Query {
            workspace: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
            request_id: uuid::Uuid::from_u128(7),
            route: Route::new("so.tilo.notes", "notes.root", "insert"),
        };
        let bytes = postcard::to_allocvec(&query).unwrap();
        assert_eq!(postcard::from_bytes::<Query>(&bytes).unwrap(), query);
        let answer = postcard::to_allocvec(&worker()).unwrap();
        assert_eq!(postcard::from_bytes::<State>(&answer).unwrap(), worker());
    }
}
