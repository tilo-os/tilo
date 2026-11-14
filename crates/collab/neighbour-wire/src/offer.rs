//! Offering work to another device of the workspace: the question and the
//! answer.
//!
//! A request this device holds runs here. One it does not hold — an app it
//! has not installed, a record outside what it keeps, a job better run
//! elsewhere — is offered to the devices that may run work for the person,
//! and each answers with what it knows of itself ([`Answer`]). The answer is
//! a routing preference and nothing more: the device that is picked still
//! decides the real request through its own dispatcher, against the caller
//! it authenticated. So nothing here is signed, and an answer that names
//! another device is never sent.
//!
//! An offer is one invocation's ([`Query::request_id`]): the device asked
//! keeps it under the device that asked and that id, and the invocation that
//! follows names it, so work is run for the device whose work it is.
//!
//! The device asked decides whether it takes the work ([`Answer::verdict`]):
//! it is the one that knows its person's work setting, its battery and what
//! its machine has free. The requester weighs and ranks the willing answers
//! (`tilo_peer_state::placement`), and the work travels in the question
//! ([`Work`]) and the answer so that moving a job's inputs is weighed
//! against running it where it was asked.

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
    /// Running from a battery.
    Battery,
}

/// Why a device will not run the work it was offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decline {
    /// Not connected, so the offer never came back.
    Offline,
    /// It cannot run the app the work's action belongs to.
    AppNotInstalled,
    /// Its person's work setting does not take this work.
    NotTakingWork,
    /// Its setting takes work only while charging, and it runs on battery.
    OnBattery,
    /// Its setting takes work only with more of the machine free than it has.
    LowResources,
    /// It is running as many jobs as it takes on at once: an offer it cannot
    /// admit now is answered not here, rather than won and refused later.
    AtCapacity,
}

impl Decline {
    /// Stable token for logs and action output.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::AppNotInstalled => "app_not_installed",
            Self::NotTakingWork => "not_taking_work",
            Self::OnBattery => "on_battery",
            Self::LowResources => "low_resources",
            Self::AtCapacity => "at_capacity",
        }
    }
}

/// What a willing device is ranked by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rank {
    /// Power standing; better wins.
    pub power: Power,
    /// Work already in hand per thousand slots; lower wins.
    pub load_permille: u32,
    /// Measured round trip to the requester; lower wins. The requester sets
    /// it: it is the round trip it saw, not the device's own figure.
    pub latency_ms: u32,
}

/// One device's answer to an offer: what it is ranked by, or why it does not
/// take the work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// What it said of the offer.
    pub verdict: Result<Rank, Decline>,
    /// The digest of each of the job's inputs it already holds, so moving
    /// them is no part of the choice ([`Work::inputs`]).
    pub holds: Vec<[u8; 32]>,
}

impl Answer {
    /// A device that takes the work, ranked by what it reported.
    #[must_use]
    pub fn taking(rank: Rank, holds: Vec<[u8; 32]>) -> Self {
        Self {
            verdict: Ok(rank),
            holds,
        }
    }

    /// A device that does not take the work, `decline` being why.
    #[must_use]
    pub fn declining(decline: Decline) -> Self {
        Self {
            verdict: Err(decline),
            holds: Vec::new(),
        }
    }

    /// What a device that never answered is taken to have said: it is not
    /// there, and it holds none of the job's inputs.
    #[must_use]
    pub fn away() -> Self {
        Self::declining(Decline::Offline)
    }
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
    /// never answers.
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

#[cfg(test)]
mod tests {
    use super::{Answer, Decline, Power, Query, Rank, Route, Work};

    fn rank() -> Rank {
        Rank {
            power: Power::External,
            load_permille: 0,
            latency_ms: 20,
        }
    }

    /// A device that answered without a reason to decline takes the work and
    /// ranks by what it reported; one that never answered is not there.
    #[test]
    fn an_answer_says_either_what_a_device_is_ranked_by_or_why_it_declines() {
        assert_eq!(
            Answer::taking(rank(), vec![[3; 32]]).verdict,
            Ok(rank()),
            "a device with no reason to decline takes the work"
        );
        assert_eq!(
            Answer::away().verdict,
            Err(Decline::Offline),
            "a device that never answered is not there"
        );
        for (decline, token) in [
            (Decline::Offline, "offline"),
            (Decline::AppNotInstalled, "app_not_installed"),
            (Decline::NotTakingWork, "not_taking_work"),
            (Decline::OnBattery, "on_battery"),
            (Decline::LowResources, "low_resources"),
            (Decline::AtCapacity, "at_capacity"),
        ] {
            let answer = Answer::declining(decline);
            assert_eq!(answer.verdict, Err(decline));
            assert!(answer.holds.is_empty(), "a decline holds nothing");
            assert_eq!(decline.token(), token);
        }
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
        let answer = Answer::taking(rank(), vec![[3; 32]]);
        let bytes = postcard::to_allocvec(&answer).unwrap();
        assert_eq!(postcard::from_bytes::<Answer>(&bytes).unwrap(), answer);
    }
}
