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
//! An offer is one invocation's ([`Query::request_id`]): the device asked
//! keeps it under the device that asked and that id, and the invocation that
//! follows names it, so work is run for the device whose work it is.
//!
//! [`State::verdict`] is the one place a decline is decided, in the order a
//! person would state the rules; the requester weighs and ranks the willing
//! answers (`tilo_peer_state::placement`), and the work travels in the
//! question ([`Work`]) and the answer ([`Answer`]) so that moving a job's
//! inputs is weighed against running it where it was asked.

use serde::{Deserialize, Serialize};
use tilo_actions::Offload;
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
    /// It cannot run the app the request's action belongs to.
    AppNotInstalled,
    /// Its person asked the device to avoid discretionary work.
    DataSaving,
    /// Its battery is critically low.
    CriticalBattery,
    /// It is running as many jobs as it takes on at once: an offer it cannot
    /// admit now is answered not here, rather than won and refused later.
    AtCapacity,
}

impl Decline {
    /// Stable token for logs and action output.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Requester => "requester",
            Self::Offline => "offline",
            Self::AppNotInstalled => "app_not_installed",
            Self::DataSaving => "data_saving",
            Self::CriticalBattery => "critical_battery",
            Self::AtCapacity => "at_capacity",
        }
    }
}

/// What a device says of itself when it is offered work: the answer to one
/// offer, from its own state.
///
/// The answerer fills the facts it knows of itself; the device that asked
/// overrides the ones it knows better — that the device answered at all
/// (`online`), whether it is the one asking (`requester`), and the round trip
/// it measured (`latency_ms`).
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
    /// Whether it can run the request's app.
    pub app_installed: bool,
    /// Its person asked it to avoid discretionary work.
    pub data_saving: bool,
    /// Bumped-out-of-shape low battery: not while there is anything else.
    pub critical_battery: bool,
    /// Operator preference; higher wins.
    pub priority: i32,
    pub power: Power,
    /// Work already in hand per thousand slots; lower wins.
    pub load_permille: u32,
    /// How many more jobs it can start right now, when it keeps count: zero
    /// says it is at its limit and takes nothing more, never that it is
    /// slow. `None` for a device that runs what it is handed.
    pub free_slots: Option<u32>,
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
            app_installed: false,
            data_saving: false,
            critical_battery: false,
            priority: 0,
            power: Power::Unknown,
            load_permille: 0,
            free_slots: None,
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
        } else if !self.app_installed {
            Decline::AppNotInstalled
        } else if self.free_slots == Some(0) {
            Decline::AtCapacity
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
    /// Names the offer: the invocation the work would run. A device that
    /// answers keeps the offer under this id and the device that asked, and
    /// runs the invocation that names it — so two devices offering one route,
    /// or one device offering it twice, never stand for each other's work.
    pub request_id: Uuid,
    /// What would run.
    pub route: Route,
    /// What it would read, and how long the asking device last took for it.
    pub work: Work,
}

/// The most distinct inputs one job names; past this its inputs travel as
/// their total bytes alone.
pub const MAX_WORK_INPUTS: usize = 64;

/// What a route would read, and how long the device that asked last took
/// for it: the cost of moving the work weighed against the cost of running
/// it where it was asked.
///
/// An answering device says which of these it already holds, so an input it
/// has is not weighed as moving; the device that asked names them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Work {
    /// The distinct inputs, by digest, each with its bytes; at most
    /// [`MAX_WORK_INPUTS`], and their bytes are part of `input_bytes`.
    pub inputs: Vec<([u8; 32], u64)>,
    /// Every input's bytes, whether or not it was named above.
    pub input_bytes: u64,
    /// How long the device that asked last took for this action over
    /// inputs like these, which offloading has to beat; `None` before it
    /// has run it.
    pub local_wall_ms: Option<u64>,
    /// The transform the route's action declares, when it declares one: what
    /// a device that takes the work runs over these inputs, as a signed
    /// package's entrypoint. A device without it has no work it could run,
    /// and answers so.
    ///
    /// Encoded always, absent or not: the frame is postcard, which carries no
    /// field names or lengths, so a field skipped when absent would be read
    /// out of the next field's bytes.
    #[serde(default)]
    pub offload: Option<Offload>,
}

impl Work {
    /// What runs where it was asked: reading nothing, never offloaded.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }
}

/// One device's answer to an offer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// What it says of itself.
    pub state: State,
    /// The digest of each of the job's inputs it already holds, so moving
    /// them is no part of the choice ([`Work::inputs`]).
    pub holds: Vec<[u8; 32]>,
}

#[cfg(test)]
mod tests {
    use super::{Answer, Decline, Power, Query, Route, State, Work};

    fn worker() -> State {
        State {
            requester: false,
            online: true,
            app_installed: true,
            data_saving: false,
            critical_battery: false,
            priority: 0,
            power: Power::External,
            load_permille: 0,
            free_slots: Some(1),
            latency_ms: 20,
        }
    }

    /// Each rule passes a device over with its own reason, in the order a
    /// person would state them, and a healthy device ranks by what it
    /// reported.
    #[test]
    fn each_rule_passes_a_device_over_with_its_reason() {
        type Change = fn(&mut State);
        let cases: [(Change, Decline); 5] = [
            (|w| w.requester = true, Decline::Requester),
            (|w| w.online = false, Decline::Offline),
            (|w| w.app_installed = false, Decline::AppNotInstalled),
            (|w| w.data_saving = true, Decline::DataSaving),
            (|w| w.free_slots = Some(0), Decline::AtCapacity),
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
            work: Work {
                inputs: vec![([3; 32], 4_000)],
                input_bytes: 9_000,
                local_wall_ms: Some(1_200),
                offload: Some(tilo_actions::Offload {
                    app: "so.tilo.thumbnails".into(),
                    entrypoint: "thumbnail-batch".into(),
                    gpu: false,
                    cpu_time_ms: 60_000,
                    memory_bytes: 512 * 1024 * 1024,
                    output_bytes: 8 * 1024 * 1024,
                }),
            },
        };
        let bytes = postcard::to_allocvec(&query).unwrap();
        assert_eq!(postcard::from_bytes::<Query>(&bytes).unwrap(), query);
        let answer = Answer {
            state: worker(),
            holds: vec![[3; 32]],
        };
        let bytes = postcard::to_allocvec(&answer).unwrap();
        assert_eq!(postcard::from_bytes::<Answer>(&bytes).unwrap(), answer);
    }
}
