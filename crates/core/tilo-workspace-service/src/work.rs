//! Which device takes a turn of work: the one placement rule of where work
//! runs, decided here and nowhere else.
//!
//! A turn is work that has to run on one device of a workspace: an offloaded
//! action another device asked for, a saved automation's trigger firing, or an
//! action that runs once in the workspace (a package's migration of a
//! resource, a tilolang trigger's run).
//! Each device that may take it calls [`takes_turn`], and the turn goes, in
//! this order, to the device that started it if that device qualifies,
//! otherwise to an always-on server of the person, otherwise to any other
//! eligible device:
//!
//! - the starting device ([`Turn::starter`]) claims at once, and every device
//!   that asks for an action that runs once ([`Turn::Once`]) is its starter;
//! - an always-on server (its entry among the person's servers,
//!   [`crate::workspaces::servers`]) claims at once when the starter asked it
//!   to take the turn, which a device does only when it cannot run the work
//!   itself (`tilo.work#take` on the device action service, [`WORK_PROVIDER`]),
//!   and otherwise once [`Order::server_grace`] passed since the turn came up,
//!   time for the starter's claim to sync;
//! - any other eligible device claims once [`Order::other_grace`] passed.
//!
//! A claim is an ordinary change, a lease of the workspace named after the
//! turn. A device never claims over another device's live claim; a claim that
//! is not renewed for a minute ([`tilo_peer_state::CLOCK_SKEW_NS`]) is over,
//! and the next device in the order takes the turn over. The device that won
//! runs the turn and renews its claim while it does. Work that must never run
//! twice is kept done when it ran ([`keep_done`]), and no device takes a turn
//! kept done again.
//!
//! An agent's turn and an automation's firing are actions that run once too
//! ([`Turn::Once`] acting as an [`Actor`]): every device that reads what
//! started one calls [`takes_turn`] before it acts, and the one whose claim
//! wins acts.
//!
//! Whether a device may take a turn is decided in one place
//! ([`eligibility`]). For an offloaded action's work: the person's entry for
//! this device among their servers offers compute and is not retired, this
//! device's job runner ([`HostJobs`]) runs the transform and keeps every input
//! already, it has room for one more job, the workspace lets this device open
//! the record the work is about, and it lets the device that asked run the
//! action. For an action that runs once: the workspace lets this device open
//! the record it is about, and the device can act as whoever it acts as: for
//! an agent, its engine holds the agent's approved key or can make it; for a
//! person (an automation's saver), it acts for them. Who started the turn does
//! not decide where it runs: any member may start any agent's turn. An action
//! that acts as nobody named runs where the workspace lets this device open
//! the resource it acts on and run the action on it. No caller checks any of
//! this itself.
//!
//! A run that needs Linux (a workspace's shell command, its terminal, its
//! machine, its Browser) is asked the same way ([`Turn::Linux`], on
//! [`LINUX_RESOURCE`]). A device takes it when it runs Linux machines
//! ([`HostLinux`]) and has room for one more, unless the workspace's machine
//! is kept on another device already: a machine stays where its disk is.
//!
//! An app that keeps a long-lived connection to an outside service (a chat
//! bridge's gateway, a mail account's fetching) runs on exactly one device of
//! the workspace at a time ([`Turn::Connection`]). A device takes it when it
//! has what the app needs ([`HostConnections`]): the app installed, every
//! capability the app declares given to it, and power. An always-on server
//! claims it at once; any other device once [`Order::server_grace`] passed
//! since it saw the turn free. The device that holds the lease runs the app's
//! process with no window and renews its claim while the connection runs; it
//! stops the process as soon as it cannot write its claim, another device
//! holds the lease, or it went longer without renewing than the claim stays
//! live (asleep, suspended), so it never runs the connection beside the device
//! that took over. That device takes the turn once the claim it no longer
//! renews is over.

use std::fmt;

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, InvokeArgument, InvokeRequest, Pending, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId, Offload};

use crate::workspaces::Workspaces;
use crate::workspaces::servers;
use tilo_server_protocol::servers::person_workspace_id;

/// The provider a turn of work is asked under on the device action service.
pub const WORK_PROVIDER: &str = "tilo.work";
/// The resource a turn of work is asked on.
pub const WORK_RESOURCE: &str = "work";
/// The resource a run that needs Linux is asked on.
pub const LINUX_RESOURCE: &str = "linux";
/// The action that asks a device to take a turn.
pub const TAKE: &str = "take";

/// The ask that a device take `run`, an action of the Linux machine a
/// workspace's devices run its shells and Browser in, as the turn `run` names.
#[must_use]
pub fn linux_take_request(run: &InvokeRequest) -> InvokeRequest {
    let argument = match serde_json::json!(run) {
        serde_json::Value::Object(run) => InvokeArgument::Json(run),
        _ => InvokeArgument::None,
    };
    InvokeRequest {
        invocation: run.invocation,
        target: RemoteActionTarget {
            provider: WORK_PROVIDER.to_owned(),
            address: ActionAddress::new(LINUX_RESOURCE, TAKE),
        },
        argument,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// How long a turn's claim lasts past its last write, before the minute every
/// claim is given for clocks that differ: none, so a claim is live for exactly
/// that minute after its holder last renewed it.
const CLAIM_TTL_MS: u64 = 1;

/// How long after a turn came up each device that did not start it waits
/// before it claims: the starter's claim syncs in that time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Order {
    /// An always-on server of the person, for a turn its starter did not ask
    /// it to take: a few seconds.
    pub server_grace: std::time::Duration,
    /// Any other eligible device.
    pub other_grace: std::time::Duration,
}

impl Order {
    /// The order every device of a workspace keeps.
    pub const WORKSPACE: Self = Self {
        server_grace: std::time::Duration::from_secs(5),
        other_grace: std::time::Duration::from_secs(30),
    };
}

/// An app's offloadable action, as a device is asked to take it: the transform
/// its contract declares, over the app's content it names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    /// The app whose action it is, and whose content the inputs and outputs are.
    pub app: String,
    /// The resource the action was invoked on.
    pub resource: String,
    /// The action.
    pub action: String,
    /// The transform the action declares: a signed package's entrypoint and its
    /// bounds.
    pub offload: Offload,
    /// The content it reads, by digest, each once.
    pub inputs: Vec<[u8; 32]>,
}

impl Job {
    /// The ask that a device take this job as the turn `invocation`: what the
    /// device that wants it done sends each device it reaches.
    #[must_use]
    pub fn take_request(&self, invocation: InvocationId) -> InvokeRequest {
        let argument = match serde_json::json!(self) {
            serde_json::Value::Object(job) => InvokeArgument::Json(job),
            _ => InvokeArgument::None,
        };
        InvokeRequest {
            invocation,
            target: RemoteActionTarget {
                provider: WORK_PROVIDER.to_owned(),
                address: ActionAddress::new(WORK_RESOURCE, TAKE),
            },
            argument,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }
}

/// One turn of work a device may take.
#[derive(Clone, Debug, PartialEq)]
pub enum Turn {
    /// An offloaded invocation of an app's action, asked by the device
    /// `requester`.
    Invocation {
        /// The invocation, which names the turn.
        id: InvocationId,
        /// The device that asked.
        requester: [u8; 32],
        /// What runs.
        job: Job,
    },
    /// A run on the workspace's Linux machine, asked by the device
    /// `requester`: the action of the machine it invokes there.
    Linux {
        /// The invocation, which names the turn.
        id: InvocationId,
        /// The device that asked.
        requester: [u8; 32],
        /// What runs.
        run: Box<InvokeRequest>,
    },
    /// An action that runs once in the workspace, whichever device runs it:
    /// the same action on the same inputs is the same turn on every device.
    Once {
        /// The action and the resource it acts on, `<app>/<resource>#<action>`.
        address: String,
        /// What it is invoked with.
        inputs: serde_json::Value,
        /// Who it acts as; `None` for an action the device runs under its own
        /// grants, which it may take where it opens the resource and may run
        /// the action.
        actor: Option<Actor>,
        /// The app and record it is about: only a device that opens it takes it.
        about: Option<(String, String)>,
        /// The device it started on, which takes it at once; `None` where
        /// every device that asks for it is its starter.
        starter: Option<[u8; 32]>,
        /// When it came up, ms since the epoch: the fire time, or when the
        /// change that started it was seen.
        came_up_ms: Option<i64>,
    },
    /// An app's long-lived connection to an outside service: its process,
    /// run with no window on exactly one device of the workspace for as long
    /// as that device renews its claim.
    Connection {
        /// The app whose process keeps the connection.
        app: String,
        /// The capabilities the app declares it needs (`network.http`,
        /// `secrets`): only a device that gives it every one takes the turn.
        needs: Vec<String>,
        /// When this device saw the turn free, ms since the epoch: a device
        /// that is not an always-on server claims [`Order::server_grace`]
        /// after it.
        came_up_ms: Option<i64>,
    },
}

/// The lease ids of every turn that runs `address` once all start with this.
#[must_use]
pub fn once_leases(address: &str) -> String {
    format!("once/{address}/")
}

/// Who a turn that runs once acts as: a device takes it only where it can act as them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Actor {
    /// A person, by their person id: a saved automation runs as the person who saved it.
    Person(String),
    /// An agent, by its id: its turn acts with its own key.
    Agent(String),
}

impl Turn {
    /// The lease the turn is claimed under, and its done row is kept under.
    #[must_use]
    pub fn lease(&self) -> String {
        match self {
            Self::Invocation { id, .. } => format!("invocation/{}", id.0),
            Self::Linux { id, .. } => format!("linux/{}", id.0),
            Self::Connection { app, .. } => format!("connection/{app}"),
            Self::Once {
                address, inputs, ..
            } => {
                let inputs = serde_json::to_vec(&canonical(inputs)).unwrap_or_default();
                format!("{}{}", once_leases(address), blake3::hash(&inputs).to_hex())
            }
        }
    }

    /// The turn that runs `address` once over `inputs`: its name is the address and a hash of
    /// the inputs' canonical form, the same on every device that saw what started it.
    #[must_use]
    pub fn once(address: impl Into<String>, inputs: serde_json::Value) -> Self {
        Self::Once {
            address: address.into(),
            inputs,
            actor: None,
            about: None,
            starter: None,
            came_up_ms: None,
        }
    }

    /// The turn that keeps `app`'s connection, which needs every capability in `needs`.
    #[must_use]
    pub fn connection(app: impl Into<String>, needs: Vec<String>) -> Self {
        Self::Connection {
            app: app.into(),
            needs,
            came_up_ms: None,
        }
    }

    /// This connection turn, seen free by this device at `at_ms`.
    #[must_use]
    pub fn free_since(mut self, at_ms: i64) -> Self {
        if let Self::Connection { came_up_ms, .. } = &mut self {
            *came_up_ms = Some(at_ms);
        }
        self
    }

    /// This turn, acting as `who`: only a device that can act as them takes it.
    #[must_use]
    pub fn acting_as(mut self, who: Actor) -> Self {
        if let Self::Once { actor, .. } = &mut self {
            *actor = Some(who);
        }
        self
    }

    /// This turn, about the record `record` of `app`: only a device that opens it takes it.
    #[must_use]
    pub fn about(mut self, app: impl Into<String>, record: impl Into<String>) -> Self {
        if let Self::Once { about, .. } = &mut self {
            *about = Some((app.into(), record.into()));
        }
        self
    }

    /// This turn, started on `device` at `at_ms`: that device takes it at once, and the others
    /// in their order after it.
    #[must_use]
    pub fn started(mut self, device: [u8; 32], at_ms: i64) -> Self {
        if let Self::Once {
            starter,
            came_up_ms,
            ..
        } = &mut self
        {
            *starter = Some(device);
            *came_up_ms = Some(at_ms);
        }
        self
    }

    /// The device the turn started on; `None` for an action that runs once
    /// that every device that may take it starts.
    #[must_use]
    pub const fn starter(&self) -> Option<[u8; 32]> {
        match self {
            Self::Invocation { requester, .. } | Self::Linux { requester, .. } => Some(*requester),
            Self::Once { starter, .. } => *starter,
            Self::Connection { .. } => None,
        }
    }

    /// Whether the starter asked this device to take the turn, which it does
    /// only when it cannot run the work itself.
    const fn asked(&self) -> bool {
        matches!(self, Self::Invocation { .. } | Self::Linux { .. })
    }

    /// When the turn came up, ms since the epoch; `None` for one asked of
    /// this device now.
    const fn came_up_ms(&self) -> Option<i64> {
        match self {
            Self::Invocation { .. } | Self::Linux { .. } => None,
            Self::Once { came_up_ms, .. } | Self::Connection { came_up_ms, .. } => *came_up_ms,
        }
    }
}

/// Why a device may not take a turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ineligible {
    /// The person's entry for this device among their servers offers no
    /// compute, or it was retired, or there is no entry.
    OffersNoCompute,
    /// This device has no job runner.
    RunsNoJobs,
    /// Its job runner does not run the transform the job declares.
    CannotRun,
    /// It does not keep every input of the job.
    InputsElsewhere,
    /// It is running as many jobs as it takes on.
    AtCapacity,
    /// The workspace does not let this device open the record the work is
    /// about.
    CannotOpen,
    /// The workspace does not let the device that asked run the action.
    NotPermitted,
    /// This device runs no Linux machines.
    RunsNoLinux,
    /// It runs as many Linux machines as it has room for.
    NoRoomForLinux,
    /// The workspace's Linux machine is kept on another device.
    MachineElsewhere,
    /// This device runs no agent engine.
    RunsNoAgents,
    /// This device does not act for the person the turn acts as.
    CannotActAs,
    /// This device runs no app's connection.
    RunsNoConnections,
    /// The app whose connection it is is not installed on this device.
    NotInstalled,
    /// This device does not give the app a capability it needs.
    LacksCapability,
    /// This device runs on its battery.
    OnBattery,
}

impl fmt::Display for Ineligible {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OffersNoCompute => "this device offers no compute",
            Self::RunsNoJobs => "this device runs no jobs",
            Self::CannotRun => "this device cannot run the transform",
            Self::InputsElsewhere => "this device does not keep every input",
            Self::AtCapacity => "this device is running as many jobs as it takes on",
            Self::CannotOpen => "this device cannot open the record the work is about",
            Self::NotPermitted => "the workspace does not let the device that asked run this",
            Self::RunsNoLinux => "this device runs no Linux machines",
            Self::NoRoomForLinux => "this device has no room for another Linux machine",
            Self::MachineElsewhere => "the workspace's Linux machine is kept on another device",
            Self::RunsNoAgents => "this device runs no agents",
            Self::CannotActAs => "this device does not act for the person the turn acts as",
            Self::RunsNoConnections => "this device runs no app's connection",
            Self::NotInstalled => "the app is not installed on this device",
            Self::LacksCapability => "this device does not give the app a capability it needs",
            Self::OnBattery => "this device runs on its battery",
        })
    }
}

/// Why a device does not take a turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotTaken {
    /// Another device claimed it first.
    TakenBy([u8; 32]),
    /// This device may not take it.
    Ineligible(Ineligible),
    /// The claim could not be written.
    Unwritten(String),
    /// A device earlier in the order may still claim it: this device asks
    /// again from `at_ms`, ms since the epoch.
    Waiting {
        /// When this device's turn in the order comes.
        at_ms: i64,
    },
    /// A device did the turn already, and kept that it did and what it answered.
    Done(serde_json::Value),
}

impl fmt::Display for NotTaken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TakenBy(device) => {
                let mut short = hex::encode(device);
                short.truncate(8);
                write!(f, "the turn is taken by {short}")
            }
            Self::Ineligible(why) => write!(f, "{why}"),
            Self::Unwritten(why) => write!(f, "the claim was not written: {why}"),
            Self::Waiting { .. } => f.write_str("waiting for the device that started it"),
            Self::Done(_) => f.write_str("the turn was done already"),
        }
    }
}

/// This device's claim on a turn: renewed while the turn runs, released when
/// it is done.
pub struct Claim {
    workspaces: Workspaces,
    workspace: String,
    lease: String,
    device: [u8; 32],
    /// Whether a renewal that was not written loses the turn: a connection
    /// must not run on where no device reads that it still does.
    written_only: bool,
}

impl Claim {
    /// Push the claim out again. `false` once another device claimed the turn,
    /// as it does after two devices claimed it at once and their claims met:
    /// the device that renews then stops the work. For a connection turn,
    /// `false` too when the renewal could not be written: the other devices
    /// read the claim going silent, and one of them takes the turn over.
    pub async fn renew(&self) -> bool {
        if self
            .workspaces
            .lease_holder(&self.workspace, &self.lease)
            .await
            .ok()
            .flatten()
            .is_some_and(|holder| holder != self.device)
        {
            return false;
        }
        if let Err(error) = self
            .workspaces
            .write_lease(&self.workspace, &self.lease, CLAIM_TTL_MS)
            .await
        {
            tracing::warn!(lease = %self.lease, %error, "a turn's claim was not renewed");
            if self.written_only {
                return false;
            }
        }
        self.workspaces
            .lease_holder(&self.workspace, &self.lease)
            .await
            .ok()
            .flatten()
            == Some(self.device)
    }

    /// Let go of the turn, which is done.
    pub async fn release(self) {
        if let Err(error) = self
            .workspaces
            .write_lease(&self.workspace, &self.lease, 0)
            .await
        {
            tracing::warn!(lease = %self.lease, %error, "a finished turn's claim was not released");
        }
    }
}

/// Whether this device takes `turn` of `workspace` now, in the order every
/// device keeps ([`Order::WORKSPACE`]): its claim when it does, or whose turn
/// it is and why not.
///
/// # Errors
///
/// [`NotTaken`]: another device claimed the turn, it is done, it is not this
/// device's turn in the order yet, this device may not take it, or its claim
/// could not be written.
pub async fn takes_turn(
    workspaces: &Workspaces,
    workspace: &str,
    turn: &Turn,
) -> Result<Claim, NotTaken> {
    takes_turn_in(workspaces, workspace, turn, Order::WORKSPACE).await
}

/// [`takes_turn`], with the waits of `order`.
///
/// # Errors
///
/// As [`takes_turn`].
pub async fn takes_turn_in(
    workspaces: &Workspaces,
    workspace: &str,
    turn: &Turn,
    order: Order,
) -> Result<Claim, NotTaken> {
    let lease = turn.lease();
    if let Some(answer) = turn_done(workspaces, workspace, turn).await {
        return Err(NotTaken::Done(answer));
    }
    eligibility(workspaces, workspace, turn)
        .await
        .map_err(NotTaken::Ineligible)?;
    let device = workspaces.lease_device(workspace).await;
    let holder = workspaces
        .lease_holder(workspace, &lease)
        .await
        .ok()
        .flatten();
    if let Some(holder) = holder.filter(|holder| *holder != device) {
        return Err(NotTaken::TakenBy(holder));
    }
    if holder.is_none()
        && let Some(at_ms) = turn_comes(workspaces, turn, device, order).await
        && tilo_clock::unix_now_ms_i64() < at_ms
    {
        return Err(NotTaken::Waiting { at_ms });
    }
    let written = workspaces
        .write_lease(workspace, &lease, CLAIM_TTL_MS)
        .await;
    match workspaces
        .lease_holder(workspace, &lease)
        .await
        .ok()
        .flatten()
    {
        Some(holder) if holder == device => Ok(Claim {
            workspaces: workspaces.clone(),
            workspace: workspace.to_owned(),
            lease,
            device,
            written_only: matches!(turn, Turn::Connection { .. }),
        }),
        Some(holder) => Err(NotTaken::TakenBy(holder)),
        None => Err(NotTaken::Unwritten(
            written.err().unwrap_or_else(|| "it was not kept".into()),
        )),
    }
}

/// When this device's place in the order lets it claim `turn`, ms since the
/// epoch; `None` for at once.
async fn turn_comes(
    workspaces: &Workspaces,
    turn: &Turn,
    device: [u8; 32],
    order: Order,
) -> Option<i64> {
    if let Turn::Connection { came_up_ms, .. } = turn {
        if always_on(workspaces).await {
            return None;
        }
        let wait = i64::try_from(order.server_grace.as_millis()).unwrap_or(i64::MAX);
        let came_up = came_up_ms.unwrap_or_else(tilo_clock::unix_now_ms_i64);
        return Some(came_up.saturating_add(wait));
    }
    if turn.starter().is_none_or(|starter| starter == device) {
        return None;
    }
    let came_up = turn.came_up_ms()?;
    let wait = if always_on(workspaces).await {
        if turn.asked() {
            return None;
        }
        order.server_grace
    } else {
        order.other_grace
    };
    Some(came_up.saturating_add(i64::try_from(wait.as_millis()).unwrap_or(i64::MAX)))
}

/// Whether this device runs the shortcuts of every member of the workspaces it keeps: it is one
/// of its person's always-on servers and offers them compute.
pub async fn hosts_shortcuts(workspaces: &Workspaces) -> bool {
    offers_compute(workspaces).await
}

/// Whether this device is one of its person's always-on servers: it has an
/// entry among their servers that was not retired.
async fn always_on(workspaces: &Workspaces) -> bool {
    let person = workspaces.serving_certificate().user_public_key;
    server_entry(workspaces, &person)
        .await
        .is_some_and(|entry| !entry.retired)
}

/// What the device of `workspace` that did `turn` answered, once one kept that it did.
pub async fn turn_done(
    workspaces: &Workspaces,
    workspace: &str,
    turn: &Turn,
) -> Option<serde_json::Value> {
    workspaces
        .lease_done(workspace, &turn.lease())
        .await
        .ok()
        .flatten()
}

/// Keep that this device did `turn` of `workspace`, and what it answered:
/// every device then reads the turn as done ([`NotTaken::Done`]).
///
/// # Errors
///
/// The workspace is not kept here, or the row could not be written.
pub async fn keep_done(
    workspaces: &Workspaces,
    workspace: &str,
    turn: &Turn,
    answer: serde_json::Value,
) -> Result<(), String> {
    workspaces
        .write_done(workspace, &turn.lease(), answer)
        .await
}

/// Whether this device offers compute to its person's workspaces: the person's
/// entry for it among their servers says so and it was not retired.
pub async fn offers_compute(workspaces: &Workspaces) -> bool {
    let person = workspaces.serving_certificate().user_public_key;
    server_entry(workspaces, &person)
        .await
        .is_some_and(|entry| entry.compute && !entry.retired)
}

/// This device's entry among the servers of the person whose root is `person`.
async fn server_entry(workspaces: &Workspaces, person: &[u8; 32]) -> Option<servers::Server> {
    let device = workspaces.serving_certificate().device_public_key;
    let servers = servers::read_on(workspaces, &person_workspace_id(person)).await;
    servers
        .ok()?
        .servers
        .get(&servers::device_id(&device))
        .cloned()
}

/// Whether this device may take `turn` in `workspace` now, or why not: what a device that
/// holds a turn for long asks again while it keeps it.
///
/// # Errors
/// Why this device may not take it.
pub async fn eligible(
    workspaces: &Workspaces,
    workspace: &str,
    turn: &Turn,
) -> Result<(), Ineligible> {
    eligibility(workspaces, workspace, turn).await
}

/// Whether this device may take `turn` in `workspace`, or why not.
async fn eligibility(
    workspaces: &Workspaces,
    workspace: &str,
    turn: &Turn,
) -> Result<(), Ineligible> {
    let device = workspaces.serving_certificate().device_public_key;
    match turn {
        Turn::Invocation { requester, job, .. } => {
            if !offers_compute(workspaces).await {
                return Err(Ineligible::OffersNoCompute);
            }
            let jobs = workspaces.host_jobs().ok_or(Ineligible::RunsNoJobs)?;
            if !jobs.runs(workspace, job) {
                return Err(Ineligible::CannotRun);
            }
            if !jobs.holds(workspace, job) {
                return Err(Ineligible::InputsElsewhere);
            }
            if jobs.free_slots() == Some(0) {
                return Err(Ineligible::AtCapacity);
            }
            if !workspaces
                .opens_record(device, workspace, &job.app, &job.resource)
                .await
            {
                return Err(Ineligible::CannotOpen);
            }
            if !workspaces.permits_job(*requester, workspace, job).await {
                return Err(Ineligible::NotPermitted);
            }
            Ok(())
        }
        Turn::Linux { .. } => {
            let linux = workspaces.host_linux().ok_or(Ineligible::RunsNoLinux)?;
            let device = workspaces.lease_device(workspace).await;
            let machine_on = workspaces
                .lease_holder(workspace, &linux.machine_lease())
                .await
                .ok()
                .flatten();
            match machine_on {
                Some(machine_on) if machine_on == device => Ok(()),
                Some(_) => Err(Ineligible::MachineElsewhere),
                None if linux.has_room() => Ok(()),
                None => Err(Ineligible::NoRoomForLinux),
            }
        }
        Turn::Connection { app, needs, .. } => {
            let host = workspaces
                .host_connections()
                .ok_or(Ineligible::RunsNoConnections)?;
            if !host.installed(app) {
                return Err(Ineligible::NotInstalled);
            }
            if !needs
                .iter()
                .all(|capability| host.gives(workspace, app, capability))
            {
                return Err(Ineligible::LacksCapability);
            }
            if !host.on_power() {
                return Err(Ineligible::OnBattery);
            }
            Ok(())
        }
        Turn::Once {
            address,
            actor,
            about,
            ..
        } => {
            if let Some((app, record)) = about
                && !workspaces
                    .opens_record(device, workspace, app, record)
                    .await
            {
                return Err(Ineligible::CannotOpen);
            }
            match actor {
                // The workspace's host runs a member's shortcuts for them, as their saved proof
                // says, so a member's automations go on while none of their own devices is up.
                Some(Actor::Person(person)) => {
                    let serving = hex::encode(workspaces.serving_certificate().user_public_key);
                    if *person != serving && !hosts_shortcuts(workspaces).await {
                        return Err(Ineligible::CannotActAs);
                    }
                    Ok(())
                }
                Some(Actor::Agent(agent)) => {
                    let engine = workspaces.agent_keys().ok_or(Ineligible::RunsNoAgents)?;
                    if !engine.acts_as(workspace, agent).await {
                        return Err(Ineligible::CannotActAs);
                    }
                    Ok(())
                }
                None => {
                    let (app, resource, action) =
                        once_parts(address).ok_or(Ineligible::CannotOpen)?;
                    if !workspaces
                        .opens_record(device, workspace, app, resource)
                        .await
                    {
                        return Err(Ineligible::CannotOpen);
                    }
                    if !workspaces
                        .permits_action(device, workspace, (app, resource, action))
                        .await
                    {
                        return Err(Ineligible::NotPermitted);
                    }
                    Ok(())
                }
            }
        }
    }
}

/// The Linux machines a device runs its workspaces' shells, terminals and
/// Browser in.
pub trait HostLinux: Send + Sync {
    /// The lease of a workspace the device keeping its machine claims: a
    /// machine stays where its disk is.
    fn machine_lease(&self) -> String;

    /// Whether this device has room to start one more machine.
    fn has_room(&self) -> bool;

    /// What answers the machine's actions here: a run, a terminal, a branch
    /// or delete of the machine, a Browser.
    fn runs(&self) -> Arc<dyn ActionProvider + Send + Sync>;
}

/// The apps whose long-lived connection this device runs ([`Turn::Connection`]):
/// what [`takes_turn`] asks before it lets the device take one, and what the
/// device that holds the lease starts and stops the app's process with.
pub trait HostConnections: Send + Sync {
    /// The apps installed here that keep a connection, each with the
    /// capabilities it declares it needs.
    fn connections(&self) -> Vec<(String, Vec<String>)>;

    /// Whether `app` is installed on this device.
    fn installed(&self, app: &str) -> bool;

    /// Whether this device gives `app` the capability `capability` in `workspace`.
    fn gives(&self, workspace: &str, app: &str, capability: &str) -> bool;

    /// Whether this device runs on external power, or has no battery.
    fn on_power(&self) -> bool;

    /// Start `app`'s process in `workspace` with no window, unless it runs
    /// there already.
    ///
    /// # Errors
    /// Why the process did not start.
    fn start(&self, workspace: &str, app: &str) -> Result<(), String>;

    /// Whether the process this device started for `app` in `workspace` runs.
    fn runs(&self, workspace: &str, app: &str) -> bool;

    /// Stop the process this device started for `app` in `workspace`.
    fn stop(&self, workspace: &str, app: &str);
}

/// The agent engine a device runs, as [`takes_turn`] asks it about an agent.
pub trait AgentEngine: Send + Sync {
    /// Whether this device can act as `agent` of `workspace`: its engine holds the agent's
    /// approved key, or can make it.
    fn acts_as<'a>(
        &'a self,
        workspace: &'a str,
        agent: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>>;

    /// The agents whose answer this engine takes, by id with their names, in every workspace
    /// it runs agents of.
    fn answering(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Start `agent`'s turn in `workspace` on `place`, the cause of the change a trigger `saver`
    /// keeps fired on, with `event` what the change said, as the answer invocation `invocation`:
    /// whether this device took the turn, or why it
    /// did not start, once the turn ended. The reply the turn writes names `invocation`, and
    /// the invocation's cancel sets `stop`, which ends the turn at its next step.
    #[cfg(feature = "agents")]
    fn answer(
        &self,
        workspace: String,
        agent: String,
        place: crate::agents::triggered::Where,
        event: tilo_automation::dispatch::DocumentEvent,
        saver: String,
        (invocation, stop): (
            tilo_actions::InvocationId,
            Arc<std::sync::atomic::AtomicBool>,
        ),
    ) -> Pending<Result<Started, String>> {
        let _ = (workspace, agent, place, event, saver, invocation, stop);
        Pending::ready(Err(
            "this device's agent engine answers no trigger".to_owned()
        ))
    }
}

/// What became of a turn a trigger started ([`AgentEngine::answer`]).
#[cfg(feature = "agents")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Started {
    /// This device took the turn and ran it: what answered its model calls and what they used
    /// and cost (`provider`, `model`, `tokens`, `cost_usd`), as its invocation keeps them.
    Taken(serde_json::Value),
    /// Another device took it, or did it already: why this one did not.
    Elsewhere(String),
}

/// `inputs` with every object's keys in order and every whole number as an integer: the same
/// inputs, however they were written, hash to the same lease.
///
/// Kept apart from `tilo_canonical_json` on purpose: `tilo_canonical_json` preserves `serde_json`'s
/// float spelling so `{"a":1.0}` encodes as `{"a":1.0}`, whereas lease keys normalize whole floats
/// below `2^53` to integers so `{"a":1.0}` encodes as `{"a":1}` and matches `{"a":1}`. Merging
/// this into `tilo_canonical_json` would change existing `Turn::Once` lease hashes.
fn canonical(inputs: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match inputs {
        Value::Object(fields) => {
            let mut sorted: Vec<_> = fields.iter().collect();
            sorted.sort_by_key(|(key, _)| *key);
            Value::Object(
                sorted
                    .into_iter()
                    .map(|(key, value)| (key.clone(), canonical(value)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        Value::Number(number) => number
            .as_f64()
            .filter(|float| float.fract() == 0.0 && float.abs() < 2f64.powi(53))
            .filter(|_| number.is_f64())
            .map_or_else(
                || inputs.clone(),
                #[expect(clippy::cast_possible_truncation, reason = "a whole number below 2^53")]
                |whole| Value::from(whole as i64),
            ),
        _ => inputs.clone(),
    }
}

/// The app, resource and action of an action address `<app>/<resource>#<action>`.
fn once_parts(address: &str) -> Option<(&str, &str, &str)> {
    let (at, action) = address.rsplit_once('#')?;
    let (app, resource) = at.split_once('/')?;
    (!app.is_empty() && !resource.is_empty() && !action.is_empty())
        .then_some((app, resource, action))
}

/// The jobs a device runs for the devices of its workspaces: an offloadable
/// action's declared transform, run over content the device keeps.
pub trait HostJobs: Send + Sync {
    /// Whether this device runs `job`'s transform in `workspace`: it has the
    /// package that performs it.
    fn runs(&self, workspace: &str, job: &Job) -> bool;

    /// Whether this device keeps every one of `job`'s inputs already.
    fn holds(&self, workspace: &str, job: &Job) -> bool;

    /// How many more jobs this device can start right now, when it keeps
    /// count: zero says it is at its limit. `None` for a runner that takes
    /// what it is handed.
    fn free_slots(&self) -> Option<u32> {
        None
    }

    /// Run `job` of `workspace`, the turn `invocation` the device `requester`
    /// asked for, and answer with what it produced: each output kept where
    /// the workspace's devices fetch the app's content, addressed as
    /// `tilo.content:<hex>`.
    fn run(
        &self,
        workspace: &str,
        job: &Job,
        requester: [u8; 32],
        invocation: InvocationId,
    ) -> Pending<Result<ActionOutput, ActionCallError>>;

    /// Stop the job running here for `invocation`, when it still runs.
    fn cancel(&self, invocation: InvocationId) {
        let _ = invocation;
    }
}

#[cfg(test)]
mod tests {
    use super::{Turn, canonical};

    #[test]
    fn canonical_normalizes_whole_floats_to_integers_for_lease_keys() {
        let whole_float: serde_json::Value = serde_json::from_str(r#"{"a":1.0}"#).unwrap();
        let encoded = serde_json::to_vec(&canonical(&whole_float)).unwrap();
        assert_eq!(encoded, br#"{"a":1}"#);

        let lease = Turn::once("so.tilo.notes/notebook-1#migrate", whole_float).lease();
        assert_eq!(
            lease,
            format!(
                "once/so.tilo.notes/notebook-1#migrate/{}",
                blake3::hash(br#"{"a":1}"#).to_hex()
            )
        );
    }
}
