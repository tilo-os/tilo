//! Running an offloadable invocation on another device of the workspace.
//!
//! An action that declares an offload runs where the door
//! ([`tilo_actions::offload::ActionOffload`]) sends it. A route built on the
//! offer round sends it to the device that wins the round: it asks the
//! workspace's other devices what would run ([`Route`]) and what it reads
//! ([`Work`]), and picks one among the willing ones that beat the run where
//! it was asked ([`pick`](crate::placement::pick)).
//!
//! The devices asked are the ones the requesting device's own plane resolves
//! in the workspace ([`ActionPlane::participants`](crate::actions::ActionPlane::participants)):
//! certified, unrevoked, unexpired, their person a member. An app names no
//! device and no address. [`OffloadPeers`] is what such a route needs of that
//! plane — the asking and the running — so the route can live where the
//! content it weighs does, and a host supplies both from the one plane it
//! serves.
//!
//! Nothing here carries authority: the device that wins the round decides the
//! invocation through its own dispatcher, against the caller it
//! authenticated.

use neighbour_wire::offer::{Route, Work};
use std::time::Duration;
use tilo_actions::ActionOutput;
use tilo_actions::InvocationId;
use tilo_actions::remote::{InvokeRequest, Pending};

use crate::placement::Choice;

/// What running an offloadable invocation on another device came to.
#[derive(Clone, Debug, PartialEq)]
pub struct RanAway {
    /// The device that ran it.
    pub device: [u8; 32],
    /// What the action answered there.
    pub output: ActionOutput,
    /// The bytes of each content resource `output` names, in order: what the
    /// device that asked keeps, so the app that invoked the action here reads
    /// its output by the same addresses a local run returns.
    pub outputs: Vec<Vec<u8>>,
}

/// The workspace's devices, as an offload route asks them: which one would
/// run a job, and the job run there.
pub trait OffloadPeers: Send + Sync {
    /// Ask every other device of `workspace` whether it would run `route` on
    /// `work`, and rank what they answered: the device to run it on, or —
    /// nobody willing, or moving the inputs there no faster than the run
    /// here — that it stays here ([`Choice`]).
    ///
    /// `invocation` is the invocation the work would run, and names the
    /// offer: the device that takes it keeps the work under it, for the
    /// device that asked. Every device is asked at once and given the whole
    /// of `within` to answer; one that does not is ranked as away. `None`
    /// when the round was abandoned before it finished.
    fn ask(
        &self,
        workspace: &str,
        route: &Route,
        work: &Work,
        invocation: InvocationId,
        within: Duration,
    ) -> Pending<Choice>;

    /// Run `invocation` on `device` in `workspace` and follow it there until
    /// it settles, with the bytes of the content it produced.
    fn run(
        &self,
        workspace: &str,
        device: [u8; 32],
        invocation: InvokeRequest,
    ) -> Pending<Result<RanAway, String>>;
}
