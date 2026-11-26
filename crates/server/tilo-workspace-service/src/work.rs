//! Which device takes a turn of work: the one way a device of a workspace
//! takes work another device asked for.
//!
//! A device that wants work done somewhere else asks the workspace's devices it
//! reaches to take it (`tilo.work#take` on the device action service,
//! [`WORK_PROVIDER`]). Each one asked calls [`takes_turn`]. A device that may
//! take the turn claims it with an ordinary change, a lease of the workspace
//! named after the turn; the earliest claim wins, and a claim that is not
//! renewed for a minute ([`tilo_peer_state::CLOCK_SKEW_NS`]) may be taken over.
//! The device that won runs the turn and renews its claim while it does;
//! every other device answers whose turn it is, or why it may not take it.
//!
//! An agent's turn is a turn too ([`Turn::Agent`]), claimed under the agent
//! and the place it was started in: every device that runs an agent engine
//! and reads what started it calls [`takes_turn`] before it answers, and the
//! one whose claim wins writes the answer.
//!
//! Whether a device may take a turn is decided in one place
//! ([`eligibility`]). For an offloaded action's work: the person's entry for
//! this device among their servers offers compute and is not retired, this
//! device's job runner ([`HostJobs`]) runs the transform and keeps every input
//! already, it has room for one more job, the workspace lets this device open
//! the record the work is about, and it lets the device that asked run the
//! action. For an agent's turn: the workspace lets this device open the
//! record the turn is about, and the device runs an agent engine that can run
//! the agent: it holds the agent's approved key, or it is a device of the
//! person it acts for (a server of theirs only where its entry offers compute and
//! is not retired). Who started the turn does not decide where it runs: any
//! member may start any agent's turn. No caller checks any of this itself.
//!
//! A run that needs Linux (a workspace's shell command, its terminal, its
//! machine, its Browser) is asked the same way ([`Turn::Linux`], on
//! [`LINUX_RESOURCE`]). A device takes it when it runs Linux machines
//! ([`HostLinux`]) and has room for one more, unless the workspace's machine
//! is kept on another device already: a machine stays where its disk is.

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
    /// A turn of an agent's, in the place something started it.
    Agent {
        /// The agent that answers.
        agent: String,
        /// The agent with the signing grant: the lead when a child answers.
        signer: String,
        /// Where the turn was started: the conversation, or the task, it
        /// answers in. One device answers there at a time.
        place: String,
        /// The person the agent acts for, by their root key.
        acts_for: [u8; 32],
        /// The app keeping the record the turn is about.
        app: String,
        /// That record.
        record: String,
    },
}

impl Turn {
    /// The lease the turn is claimed under.
    fn lease(&self) -> String {
        match self {
            Self::Invocation { id, .. } => format!("invocation/{}", id.0),
            Self::Linux { id, .. } => format!("linux/{}", id.0),
            Self::Agent { agent, place, .. } => {
                let lease = format!("turn/{agent}/{place}");
                if lease.len() <= tilo_peer_state::workspace_membership::MAX_LEASE_ID
                    || lease.chars().any(char::is_control)
                {
                    return lease;
                }
                // A direct conversation spells out all its members. Keep existing lease names,
                // and hash the two separate parts of a longer name. Neither may contain NUL.
                let mut hash = blake3::Hasher::new_derive_key("Tilo agent turn lease v1");
                hash.update(agent.as_bytes());
                hash.update(&[0]);
                hash.update(place.as_bytes());
                format!("turn-hash/{}", hash.finalize().to_hex())
            }
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
    /// This device holds no approved key of the agent and is none of the
    /// devices of the person it acts for.
    CannotRunAgent,
    /// This device runs no agent engine.
    RunsNoAgents,
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
            Self::CannotRunAgent => "this device cannot run the agent",
            Self::RunsNoAgents => "this device runs no agents",
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
}

impl Claim {
    /// Push the claim out again. `false` once another device holds the turn,
    /// as it does after two devices claimed it at once and their claims met:
    /// the device that renews then stops the work.
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

/// Whether this device takes `turn` of `workspace`: its claim when it does, or
/// whose turn it is and why not.
///
/// # Errors
///
/// [`NotTaken`]: another device holds the turn, this device may not take it, or
/// its claim could not be written.
pub async fn takes_turn(
    workspaces: &Workspaces,
    workspace: &str,
    turn: &Turn,
) -> Result<Claim, NotTaken> {
    eligibility(workspaces, workspace, turn)
        .await
        .map_err(NotTaken::Ineligible)?;
    let device = workspaces.lease_device(workspace).await;
    let lease = turn.lease();
    let holder = workspaces
        .lease_holder(workspace, &lease)
        .await
        .ok()
        .flatten();
    if let Some(holder) = holder.filter(|holder| *holder != device) {
        return Err(NotTaken::TakenBy(holder));
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
        }),
        Some(holder) => Err(NotTaken::TakenBy(holder)),
        None => Err(NotTaken::Unwritten(
            written.err().unwrap_or_else(|| "it was not kept".into()),
        )),
    }
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
        Turn::Agent {
            agent,
            signer,
            acts_for,
            app,
            record,
            ..
        } => {
            if !workspaces
                .opens_record(device, workspace, app, record)
                .await
            {
                return Err(Ineligible::CannotOpen);
            }
            let access = workspaces.agent_access(workspace, signer, acts_for).await?;
            if !access.permits_actor(agent, signer) || !access.permits_app(app) {
                return Err(Ineligible::NotPermitted);
            }
            Ok(())
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

/// Whether this device runs agents for the person whose root is `person`: it
/// is a device of theirs, and where their servers name it, its entry offers
/// compute and is not retired.
pub(crate) async fn device_of(
    workspaces: &Workspaces,
    person: &[u8; 32],
) -> Result<(), Ineligible> {
    match server_entry(workspaces, person).await {
        Some(entry) if entry.compute && !entry.retired => Ok(()),
        Some(_) => Err(Ineligible::OffersNoCompute),
        None if workspaces.serving_certificate().user_public_key == *person => Ok(()),
        None => Err(Ineligible::CannotRunAgent),
    }
}

/// The authority from which this engine could take an agent's next step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentAccess {
    /// A personal device still applies the keeper's local policy.
    Device,
    /// A live hosted signing session for this exact incarnation of its agent.
    Hosted {
        device: [u8; 32],
        created_at: i64,
        scope: tilo_server_protocol::delegation::DelegateScope,
    },
}

impl AgentAccess {
    #[must_use]
    pub fn permits_app(&self, app: &str) -> bool {
        match self {
            Self::Device => true,
            Self::Hosted { scope, .. } => scope.permits_app(app),
        }
    }

    /// A child works on the job kept by Assistant, under its lead's permission for that app.
    pub(crate) fn permits_actor(&self, actor: &str, signer: &str) -> bool {
        actor == signer || self.permits_app(tilo_app_id::apps::ASSISTANT)
    }

    /// A delayed step from a former signing session may never use a replacement grant.
    #[must_use]
    pub fn matches_caller(&self, caller: &tilo_actions::remote::Caller) -> bool {
        match self {
            Self::Device => true,
            Self::Hosted {
                device, created_at, ..
            } => {
                caller.device_public_key() == Some(device)
                    && caller.roster_created_at() == Some(*created_at)
            }
        }
    }
}

/// The agent engine a device runs, as [`takes_turn`] asks it about an agent.
pub trait AgentEngine: Send + Sync {
    /// The keeper's current authority for this agent on this device.
    fn access<'a>(
        &'a self,
        workspace: &'a str,
        agent: &'a str,
        acts_for: &'a [u8; 32],
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AgentAccess, Ineligible>> + Send + 'a>,
    >;
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
    use super::Turn;
    use tilo_peer_state::workspace_membership::MAX_LEASE_ID;

    #[test]
    fn agent_claims_keep_existing_names_and_identify_long_conversations_separately() {
        let lease = |agent: &str, place: &str| {
            Turn::Agent {
                agent: agent.into(),
                signer: agent.into(),
                place: place.into(),
                acts_for: [0; 32],
                app: "so.tilo.chat".into(),
                record: String::new(),
            }
            .lease()
        };
        let place = "p".repeat(MAX_LEASE_ID - "turn/scout/".len());
        let existing = format!("turn/scout/{place}");
        assert_eq!(existing.len(), 256);
        assert_eq!(lease("scout", &place), existing);

        let longer = format!("{place}x");
        assert_eq!(format!("turn/scout/{longer}").len(), 257);
        let first = lease("scout", &longer);
        assert!(first.starts_with("turn-hash/") && first.len() <= MAX_LEASE_ID);
        assert_eq!(first, lease("scout", &longer));
        assert_ne!(first, lease("other", &longer));
        assert_ne!(first, lease("scout", &format!("{place}y")));
        assert_ne!(
            lease("scout/b", &longer),
            lease("scout", &format!("b/{longer}"))
        );
        let invalid = format!("{longer}\n");
        assert_eq!(lease("scout", &invalid), format!("turn/scout/{invalid}"));
    }
}
