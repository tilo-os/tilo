//! The signing grants of the agents a workspace's host runs for it.
//!
//! An agent on the workspace's roster (`tilo.workspace.agent`) signs what it changes with a
//! device key of its own, certified under its person's root, with a root-signed delegation
//! that names its one workspace. Its person asks for that grant from a certified session
//! (`request-grant` with the roster entry's `agentId`, on provider `agents`); the host
//! generates the key, puts the proposal to the person's approving device in one consent step
//! and, once approved, opens the agent's signing session ([`AgentSessions`]), which the
//! workspace's actions sign that agent's changes through. [`AgentSupervisor`] holds every
//! grant: pending, approved, or settled (denied, expired, revoked), and keeps them across
//! restarts ([`SupervisorSnapshot`]).

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_actions::ActionOutput;
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{
    ActionCallError, Caller, InvocationLog, InvocationReceipt, InvocationState, InvokeRequest,
};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::agents::{GrantRequest, GrantState, parse_color_hex};
use tilo_server_protocol::delegation::{
    Delegate, DelegateProposal, DelegateScope, Delegation, MAX_DELEGATE_TEXT_BYTES,
    MAX_DELEGATED_APPS, MAX_DELEGATED_WORKSPACES,
};
use tilo_user_identity::DeviceCert;

/// Grants kept, settled (denied, expired, revoked) ones first to go.
const MAX_GRANTS: usize = 32;
/// Shortest and longest grant a person may ask for.
const GRANT_SECS: std::ops::RangeInclusive<u64> = 60..=30 * 24 * 60 * 60;

/// An agent's certified device: key, capability-less certificate and
/// root-signed delegation.
#[derive(Clone)]
pub struct AgentIdentity {
    pub key: SigningKey,
    /// Its root-signed certificate.
    pub cert: DeviceCert,
    /// Its root-signed delegation.
    pub delegation: Delegation,
}

/// The agent id `delegation` names. The supervisor only ever holds agent
/// delegations: a grant's proposal names an agent, the approver's answer is
/// held to the proposal, and restoring refuses any other kind.
#[must_use]
pub fn agent_id(delegation: &Delegation) -> &str {
    delegation.delegate.agent_id().unwrap_or_default()
}

/// The person `delegation`'s agent acts for.
#[must_use]
pub fn person(delegation: &Delegation) -> tilo_actions::MemberId {
    tilo_actions::MemberId::person(ui::permissions::policy::PersonId::from_root(
        delegation.user_public_key,
    ))
}

/// Raised whenever something [`AgentSupervisor::snapshot`] returns changed,
/// so the host writes its state when it changes rather than on an interval.
#[derive(Default)]
pub(crate) struct SnapshotChanges {
    generation: Mutex<u64>,
    condvar: Condvar,
}

impl SnapshotChanges {
    fn generation_guard(&self) -> MutexGuard<'_, u64> {
        self.generation.lock_recover()
    }

    /// Something the snapshot holds changed, or the host is stopping.
    pub(crate) fn raise(&self) {
        let mut generation = self.generation_guard();
        *generation = generation.wrapping_add(1);
        drop(generation);
        self.condvar.notify_all();
    }

    /// The current generation: read it before taking a snapshot, then
    /// [`Self::wait_past`] it.
    pub(crate) fn generation(&self) -> u64 {
        *self.generation_guard()
    }

    /// Block until the generation moves past `seen` or `timeout` passes
    /// (`None`: no limit). A change raised after `seen` was read returns at
    /// once, so none is lost between a write and this wait.
    pub(crate) fn wait_past(&self, seen: u64, timeout: Option<Duration>) {
        let generation = self.generation_guard();
        let woken = match timeout {
            Some(timeout) => {
                self.condvar
                    .wait_timeout_while(generation, timeout, |now| *now == seen)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
            None => self
                .condvar
                .wait_while(generation, |now| *now == seen)
                .unwrap_or_else(PoisonError::into_inner),
        };
        drop(woken);
    }
}

/// The sessions of the agents working beside a device, by the device each
/// signs as: a change an action made for one of them is that agent's, signed
/// with its own key.
#[derive(Clone, Default)]
pub struct AgentSessions {
    links: Arc<Mutex<BTreeMap<[u8; 32], AgentSession>>>,
}

struct AgentSession {
    link: tilo_session_client::Link,
    roster: Option<(RosterCreatedAt, Delegation)>,
    /// The agent's own key and the certificate its person issued for it: what each
    /// invocation a turn of this agent makes is signed with ([`AgentSessions::sign_as`]).
    signer: (DeviceCert, SigningKey),
}

impl AgentSessions {
    /// The session of agent `device`, while it is open here.
    #[must_use]
    pub fn link(&self, device: &[u8; 32]) -> Option<tilo_session_client::Link> {
        self.links
            .lock_recover()
            .get(device)
            .map(|session| session.link.clone())
    }

    /// Whether this peer was opened solely to sign for a hosted roster entry.
    #[must_use]
    pub fn is_roster_signer(&self, device: &[u8; 32]) -> bool {
        self.links
            .lock_recover()
            .get(device)
            .is_some_and(|session| session.roster.is_some())
    }

    /// Whether this signing peer was opened for the exact roster assignment.
    #[must_use]
    pub fn matches_roster(
        &self,
        device: &[u8; 32],
        workspace: WorkspaceId,
        agent: &str,
        acts_for: &[u8; 32],
        created_at: RosterCreatedAt,
    ) -> bool {
        self.links
            .lock_recover()
            .get(device)
            .and_then(|session| session.roster.as_ref())
            .is_some_and(|(created, delegation)| {
                *created == created_at
                    && delegation.scope.workspaces.as_slice() == [workspace]
                    && delegation.delegate.agent_id() == Some(agent)
                    && &delegation.user_public_key == acts_for
            })
    }

    /// The live, joined signing session and its exact approved delegation for
    /// a caller bound by the hosted roster runner.
    #[must_use]
    pub fn live_roster(
        &self,
        workspace: WorkspaceId,
        caller: &tilo_actions::remote::Caller,
    ) -> Option<(tilo_session_client::Link, Delegation)> {
        let device = caller.device_public_key()?;
        let sessions = self.links.lock_recover();
        let session = sessions.get(device)?;
        let (created_at, delegation) = session.roster.as_ref()?;
        if caller.roster_created_at() != Some(*created_at)
            || caller.workspace().and_then(WorkspaceId::parse) != Some(workspace)
            || delegation.scope.workspaces.as_slice() != [workspace]
            || delegation.delegate.agent_id() != caller.approved_agent()
            || caller.acting().app().is_some()
            || !caller.acting().people().iter().any(|person| {
                tilo_actions::PersonId::parse(person)
                    .ok()
                    .and_then(|person| person.root())
                    == Some(delegation.user_public_key)
            })
            || Some(&delegation.user_public_key) != caller.user_public_key()
            || &delegation.device_public_key != device
            || tilo_clock::unix_now_ns() >= delegation.expires_at_ns
        {
            return None;
        }
        let link = session.link.clone();
        let delegation = delegation.clone();
        drop(sessions);
        let status = link.status();
        (status.authenticated
            && status.phase == tilo_session_client::TransportPhase::Connected
            && &status.device == device
            && status.workspaces.get(&workspace) == Some(&true)
            && status.serving.contains(&workspace))
        .then_some((link, delegation))
    }

    /// `link` now serves the changes of the agent `identity` certifies.
    pub(crate) fn joined(
        &self,
        identity: &AgentIdentity,
        link: tilo_session_client::Link,
        roster: Option<(RosterCreatedAt, Delegation)>,
    ) {
        self.links.lock_recover().insert(
            identity.cert.device_public_key,
            AgentSession {
                link,
                roster,
                signer: (identity.cert.clone(), identity.key.clone()),
            },
        );
    }

    /// Sign `request` in `workspace` as the agent `device` names, with
    /// its own key, while its session is open here: an agent's turn asks the workspace for
    /// nothing under any key but its own.
    ///
    /// # Errors
    ///
    /// No session of `device` is open here, or its key does not sign.
    pub fn sign_as(
        &self,
        device: &[u8; 32],
        workspace: WorkspaceId,
        request: &mut tilo_actions::remote::InvokeRequest,
    ) -> Result<(), String> {
        let (cert, key) = self
            .links
            .lock_recover()
            .get(device)
            .map(|session| session.signer.clone())
            .ok_or("the agent's signing session is not open here")?;
        request.signed = None;
        let signed = tilo_server_protocol::invocation::sign_invocation_now(
            &workspace.to_string(),
            request,
            &cert,
            &key,
            tilo_clock::unix_now_ns(),
        )
        .map_err(|error| error.to_string())?;
        request.signed = Some(Box::new(signed));
        Ok(())
    }

    /// Agent `device`'s session ended.
    pub(crate) fn left(&self, device: &[u8; 32]) {
        self.links.lock_recover().remove(device);
    }
}

/// The incarnation of an existing hosted roster entry. The signed proposal supplies
/// its id, person and single workspace.
pub type RosterCreatedAt = i64;

/// Opens the signing session of an approved roster grant, on its own thread.
pub(crate) type RosterSpawner =
    Arc<dyn Fn(AgentIdentity, RosterCreatedAt) -> Result<(), String> + Send + Sync>;

/// What a host gives its supervisor: the approver for new grants, and the
/// server's register of agent devices.
pub trait AgentHostLink: Send + Sync {
    /// Ask the person's approving device to certify `key` as the roster agent `proposal`
    /// describes, in one consent step. Returns the pairing code the approver shows; `done`
    /// later receives the agent's certificate and root-signed delegation, or why there are
    /// none.
    fn request_roster_approval(
        &self,
        key: &SigningKey,
        proposal: &DelegateProposal,
        created_at: RosterCreatedAt,
        requester: &DeviceCert,
        done: ApprovalDone,
    ) -> Result<String, String>;

    /// Recheck the roster entry before admitting its passive signing session.
    fn admit_roster_session(
        &self,
        identity: &AgentIdentity,
        created_at: RosterCreatedAt,
        lifetime: tokio::sync::oneshot::Receiver<()>,
        done: RosterAdmissionDone,
    );

    /// Refuse the agent device from now on.
    fn withdraw(&self, device_public_key: [u8; 32]);
}

/// Receives the outcome of admitting a roster agent's signing session.
pub type RosterAdmissionDone = Box<dyn FnOnce(Result<(), String>) + Send>;

/// Receives the outcome of [`AgentHostLink::request_roster_approval`].
pub type ApprovalDone = Box<dyn FnOnce(Result<(DeviceCert, Delegation), String>) + Send>;

/// One grant: what was asked, whose it is, where it stands, and once approved the
/// agent identity it signs as.
#[derive(Clone)]
struct Grant {
    proposal: DelegateProposal,
    /// The person whose root approves it and keeps the agent.
    acts_for: [u8; 32],
    roster_created_at: RosterCreatedAt,
    state: GrantState,
    /// The agent's key while the approver has not answered.
    key: Option<SigningKey>,
    identity: Option<AgentIdentity>,
    pairing_code: Option<String>,
    reason: Option<String>,
}

impl Grant {
    fn id(&self) -> &str {
        self.proposal.delegate.agent_id().unwrap_or_default()
    }

    fn is_in(&self, workspace: WorkspaceId) -> bool {
        self.proposal.scope.workspaces.as_slice() == [workspace]
    }

    /// Settle the grant: drop its key material and say why.
    fn settle(&mut self, state: GrantState, reason: &str) {
        self.state = state;
        self.key = None;
        self.identity = None;
        self.pairing_code = None;
        self.reason = Some(reason.to_owned());
    }
}

/// A grant as a host keeps it across restarts. The key is the agent
/// device's secret: the file holding it is the host's to protect.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantRecord {
    /// What was asked.
    pub proposal: DelegateProposal,
    /// Which existing roster entry this grant provisions. A record that names none is no
    /// roster grant and is not read back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster_created_at: Option<RosterCreatedAt>,
    /// Where it stands.
    pub state: GrantState,
    /// The agent device's key seed (hex), while approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Its certificate, while approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert: Option<DeviceCert>,
    /// Its delegation ([`Delegation::encode`], hex), while approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation: Option<String>,
    /// Why it was settled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Everything a supervisor needs to pick up where it was.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SupervisorSnapshot {
    pub grants: Vec<GrantRecord>,
}

/// Every signing grant of one host.
pub struct AgentSupervisor {
    roster_spawner: OnceLock<RosterSpawner>,
    link: Mutex<Option<Arc<dyn AgentHostLink>>>,
    grants: Mutex<Vec<Grant>>,
    log: Mutex<InvocationLog<InvocationState>>,
    /// Raised whenever [`Self::snapshot`] would return something new.
    changes: Arc<SnapshotChanges>,
    me: Weak<Self>,
}

impl AgentSupervisor {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            roster_spawner: OnceLock::new(),
            link: Mutex::new(None),
            grants: Mutex::new(Vec::new()),
            log: Mutex::new(InvocationLog::default()),
            changes: Arc::default(),
            me: me.clone(),
        })
    }

    fn grants(&self) -> MutexGuard<'_, Vec<Grant>> {
        self.grants.lock_recover()
    }

    /// The signal raised whenever [`Self::snapshot`] would return something
    /// new: a grant changed.
    pub(crate) fn changes(&self) -> &Arc<SnapshotChanges> {
        &self.changes
    }

    /// When the next approved grant expires (ns since the epoch), if any.
    pub(crate) fn next_expiry_ns(&self) -> Option<u64> {
        self.grants()
            .iter()
            .filter(|grant| grant.state == GrantState::Approved)
            .map(|grant| grant.proposal.expires_at_ns)
            .min()
    }

    fn log(&self) -> MutexGuard<'_, InvocationLog<InvocationState>> {
        self.log.lock_recover()
    }

    fn link(&self) -> Option<Arc<dyn AgentHostLink>> {
        self.link.lock_recover().clone()
    }

    /// Connect the host: grants can then be requested, and approved ones
    /// are withdrawn with it.
    pub fn set_link(&self, link: Arc<dyn AgentHostLink>) {
        *self.link.lock_recover() = Some(link);
    }

    pub(crate) fn set_roster_spawner(&self, spawner: RosterSpawner) {
        let _ = self.roster_spawner.set(spawner);
    }

    /// Ask for the certified signing session of a verified existing roster entry.
    pub fn invoke_roster_grant(
        &self,
        caller: &Caller,
        invocation: &InvokeRequest,
        request: &GrantRequest,
        agent: &str,
        created_at: RosterCreatedAt,
        requester: &DeviceCert,
    ) -> Result<InvocationReceipt, ActionCallError> {
        tilo_actions::appearance::refuse_here(invocation)?;
        if !the_persons_session(caller, &requester.user_public_key)
            || caller.device_public_key() != Some(&requester.device_public_key)
        {
            return Err(ActionCallError::UnknownAction);
        }
        self.invoke_once(caller, invocation, || {
            let pairing_code = self
                .request_grant(
                    request,
                    tilo_clock::unix_now_ns(),
                    agent,
                    created_at,
                    requester,
                )
                .map_err(ActionCallError::refused)?;
            Ok(ActionOutput {
                value: Some(serde_json::json!({"grant": agent, "pairingCode": pairing_code})),
                resources: Vec::new(),
            })
        })
    }

    fn invoke_once(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        dispatch: impl FnOnce() -> Result<ActionOutput, ActionCallError>,
    ) -> Result<InvocationReceipt, ActionCallError> {
        let invocation = request.invocation;
        self.log()
            .admit(caller, request, || {
                let output = dispatch()?;
                Ok(InvocationState::new(
                    invocation,
                    InvocationUpdate::Succeeded { invocation, output },
                ))
            })
            .map(|(state, _)| state.receipt())
    }

    /// Ask the person's approver for the grant of roster agent `agent`. Returns the pairing
    /// code the approver shows, or none for a grant already approved.
    fn request_grant(
        &self,
        request: &GrantRequest,
        now_ns: u64,
        agent: &str,
        created_at: RosterCreatedAt,
        requester: &DeviceCert,
    ) -> Result<String, String> {
        let link = self.link().ok_or("this host cannot ask for agent grants")?;
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).map_err(|error| error.to_string())?;
        let key = SigningKey::from_bytes(&secret);
        let device = key.verifying_key().to_bytes();
        let acts_for = requester.user_public_key;
        let proposal = grant_proposal(request, now_ns, agent)?;
        let mut grants = self.grants();
        let prior = grants.iter().position(|grant| {
            grant.id() == agent
                && grant.acts_for == acts_for
                && grant.proposal.scope.workspaces == proposal.scope.workspaces
        });
        if let Some(grant) = prior.and_then(|index| grants.get(index)) {
            if grant.roster_created_at != created_at {
                return Err("that agent id already belongs to another grant".into());
            }
            match grant.state {
                GrantState::Pending
                    if grant.proposal.label == proposal.label
                        && grant.proposal.color == proposal.color
                        && grant.proposal.scope == proposal.scope =>
                {
                    return Ok(grant.pairing_code.clone().unwrap_or_default());
                }
                GrantState::Approved if grant.proposal.scope == proposal.scope => {
                    return Ok(String::new());
                }
                GrantState::Denied | GrantState::Expired => {}
                _ => return Err("that agent already has a different or withdrawn grant".into()),
            }
        }
        let me = self.me.clone();
        let answered = agent.to_owned();
        let done: ApprovalDone = Box::new(move |outcome| {
            if let Some(supervisor) = me.upgrade() {
                supervisor.approval_answered(&answered, device, acts_for, outcome);
            }
        });
        // Hold the grants across enqueue so a fast answer sees its pending grant,
        // and simultaneous requests for this roster id share only one proposal.
        let pairing_code =
            link.request_roster_approval(&key, &proposal, created_at, requester, done)?;
        if let Some(index) = prior {
            grants.remove(index);
        }
        grants.push(Grant {
            proposal,
            acts_for,
            roster_created_at: created_at,
            state: GrantState::Pending,
            key: Some(key),
            identity: None,
            pairing_code: Some(pairing_code.clone()),
            reason: None,
        });
        trim_grants(&mut grants);
        drop(grants);
        self.changes.raise();
        Ok(pairing_code)
    }

    fn approval_answered(
        &self,
        id: &str,
        device: [u8; 32],
        acts_for: [u8; 32],
        outcome: Result<(DeviceCert, Delegation), String>,
    ) {
        let mut grants = self.grants();
        let Some(grant) = grants.iter_mut().find(|grant| {
            grant.id() == id
                && grant.acts_for == acts_for
                && grant.state == GrantState::Pending
                && grant
                    .key
                    .as_ref()
                    .is_some_and(|key| key.verifying_key().to_bytes() == device)
        }) else {
            return;
        };
        let Some(key) = grant.key.take() else {
            return;
        };
        let approved = outcome.and_then(|(cert, delegation)| {
            if cert.device_public_key != device
                || delegation.device_public_key != device
                || delegation.user_public_key != acts_for
            {
                return Err("the approval names a different identity".into());
            }
            grant.proposal.admits(&delegation).map_err(str::to_owned)?;
            if delegation.scope.workspaces != grant.proposal.scope.workspaces {
                return Err("the approval does not cover the roster workspace".into());
            }
            let spawn = self
                .roster_spawner
                .get()
                .ok_or("this supervisor cannot join roster sessions")?;
            let identity = AgentIdentity {
                key,
                cert,
                delegation,
            };
            spawn(identity.clone(), grant.roster_created_at)?;
            Ok(identity)
        });
        match approved {
            Ok(identity) => {
                grant.proposal.scope = identity.delegation.scope.clone();
                grant.proposal.expires_at_ns = identity.delegation.expires_at_ns;
                grant.state = GrantState::Approved;
                grant.pairing_code = None;
                grant.identity = Some(identity);
            }
            Err(reason) => grant.settle(GrantState::Denied, &reason),
        }
        drop(grants);
        self.changes.raise();
    }

    pub(crate) fn roster_session_failed(&self, device: [u8; 32], reason: &str) {
        let mut grants = self.grants();
        let Some(grant) = grants.iter_mut().find(|grant| {
            grant
                .identity
                .as_ref()
                .is_some_and(|identity| identity.cert.device_public_key == device)
        }) else {
            return;
        };
        if let Some(link) = self.link() {
            link.withdraw(device);
        }
        grant.settle(GrantState::Denied, reason);
        drop(grants);
        self.changes.raise();
    }

    /// Whether this id has a signing grant in this exact workspace.
    #[must_use]
    pub fn has_roster_grant(&self, workspace: WorkspaceId, id: &str) -> bool {
        self.grants()
            .iter()
            .any(|grant| grant.id() == id && grant.is_in(workspace))
    }

    /// Withdraw the signing grant in the workspace of this certified request: the host
    /// refuses its device and the agent's key is forgotten.
    pub fn invoke_roster_revoke(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        workspace: WorkspaceId,
        id: &str,
    ) -> Result<InvocationReceipt, ActionCallError> {
        tilo_actions::appearance::refuse_here(request)?;
        if !caller.acting().is_person() {
            return Err(ActionCallError::UnknownAction);
        }
        self.invoke_once(
            &caller.clone().in_workspace(workspace.to_string()),
            request,
            || {
                self.revoke(caller, id, workspace)
                    .map_err(ActionCallError::refused)?;
                Ok(ActionOutput {
                    value: Some(serde_json::json!({})),
                    resources: Vec::new(),
                })
            },
        )
    }

    fn revoke(&self, caller: &Caller, id: &str, workspace: WorkspaceId) -> Result<(), String> {
        let mut grants = self.grants();
        let grant = grants
            .iter_mut()
            .find(|grant| {
                grant.id() == id
                    && grant.is_in(workspace)
                    && the_persons_session(caller, &grant.acts_for)
            })
            .ok_or("no such agent")?;
        if let (Some(identity), Some(link)) = (&grant.identity, self.link()) {
            link.withdraw(identity.cert.device_public_key);
        }
        grant.settle(GrantState::Revoked, "revoked by a person");
        drop(grants);
        self.changes.raise();
        Ok(())
    }

    /// Settle every approved grant that expired by `now_ns`: the host refuses its device.
    /// Returns the grants expired.
    pub fn expire(&self, now_ns: u64) -> Vec<String> {
        let link = self.link();
        let mut grants = self.grants();
        let due = grants
            .iter_mut()
            .filter_map(|grant| {
                if grant.state != GrantState::Approved || grant.proposal.expires_at_ns > now_ns {
                    return None;
                }
                if let (Some(identity), Some(link)) = (&grant.identity, &link) {
                    link.withdraw(identity.cert.device_public_key);
                }
                grant.settle(GrantState::Expired, "the grant expired");
                Some(grant.id().to_owned())
            })
            .collect::<Vec<_>>();
        drop(grants);
        if !due.is_empty() {
            self.changes.raise();
        }
        due
    }

    /// Every grant, for the host to keep across restarts.
    #[must_use]
    pub fn snapshot(&self) -> SupervisorSnapshot {
        SupervisorSnapshot {
            grants: self
                .grants()
                .iter()
                .map(|grant| GrantRecord {
                    proposal: grant.proposal.clone(),
                    roster_created_at: Some(grant.roster_created_at),
                    state: grant.state,
                    key: grant
                        .identity
                        .as_ref()
                        .map(|identity| hex::encode(identity.key.to_bytes())),
                    cert: grant
                        .identity
                        .as_ref()
                        .map(|identity| identity.cert.clone()),
                    delegation: grant
                        .identity
                        .as_ref()
                        .map(|identity| hex::encode(identity.delegation.encode())),
                    reason: grant.reason.clone(),
                })
                .collect(),
        }
    }

    /// Pick up a [`Self::snapshot`] after a restart: each approved grant opens its signing
    /// session again, unless it expired meanwhile; a grant still waiting for its approver
    /// lost its request and is denied.
    pub fn restore(&self, snapshot: SupervisorSnapshot, now_ns: u64) {
        let link = self.link();
        let mut grants = Vec::new();
        for saved in snapshot.grants {
            let Some(roster_created_at) = saved.roster_created_at else {
                continue;
            };
            let mut grant = Grant {
                proposal: saved.proposal,
                // Only an approved grant has an identity, and it names its person's root.
                acts_for: [0; 32],
                roster_created_at,
                state: saved.state,
                key: None,
                identity: None,
                pairing_code: None,
                reason: saved.reason,
            };
            match grant.state {
                GrantState::Pending => {
                    grant.settle(GrantState::Denied, "the host restarted before the approval");
                }
                GrantState::Approved => {
                    match restore_identity(saved.key, saved.cert, saved.delegation) {
                        Some(identity) if identity.delegation.expires_at_ns <= now_ns => {
                            if let Some(link) = &link {
                                link.withdraw(identity.delegation.device_public_key);
                            }
                            grant.settle(GrantState::Expired, "the grant expired");
                        }
                        Some(identity)
                            if grant.proposal.scope.workspaces
                                == identity.delegation.scope.workspaces
                                && grant.proposal.admits(&identity.delegation).is_ok() =>
                        {
                            grant.acts_for = identity.delegation.user_public_key;
                            grant.identity = Some(identity);
                        }
                        Some(_) | None => {
                            grant.settle(GrantState::Denied, "its grant could not be read back");
                        }
                    }
                }
                GrantState::Denied | GrantState::Expired | GrantState::Revoked => {}
            }
            grants.push(grant);
        }
        for grant in &mut grants {
            if let Some(identity) = &grant.identity {
                let spawned = self
                    .roster_spawner
                    .get()
                    .ok_or_else(|| "this supervisor cannot join roster sessions".to_owned())
                    .and_then(|spawn| spawn(identity.clone(), grant.roster_created_at));
                if let Err(reason) = spawned {
                    grant.settle(GrantState::Denied, &reason);
                }
            }
        }
        *self.grants() = grants;
        self.changes.raise();
    }

    /// Hold `identity` as an approved grant of roster entry `created_at`, without opening
    /// its session: what a test of the host's keeping starts from.
    #[cfg(test)]
    pub(crate) fn hold(&self, identity: AgentIdentity, created_at: RosterCreatedAt) {
        let delegation = &identity.delegation;
        self.grants().push(Grant {
            proposal: DelegateProposal {
                delegate: delegation.delegate.clone(),
                label: delegation.label.clone(),
                color: delegation.color,
                scope: delegation.scope.clone(),
                expires_at_ns: delegation.expires_at_ns,
            },
            acts_for: delegation.user_public_key,
            roster_created_at: created_at,
            state: GrantState::Approved,
            key: None,
            identity: Some(identity),
            pairing_code: None,
            reason: None,
        });
        self.changes.raise();
    }
}

/// Whether `caller` is the person the agent acts for, in a session of theirs:
/// who alone manages the grants of their agent.
fn the_persons_session(caller: &Caller, person: &[u8; 32]) -> bool {
    caller.acting().is_person() && caller.user_public_key() == Some(person)
}

fn trim_grants(grants: &mut Vec<Grant>) {
    while grants.len() > MAX_GRANTS {
        match grants
            .iter()
            .position(|grant| !matches!(grant.state, GrantState::Pending | GrantState::Approved))
        {
            Some(index) => {
                grants.remove(index);
            }
            None => break,
        }
    }
}

fn restore_identity(
    key: Option<String>,
    cert: Option<DeviceCert>,
    delegation: Option<String>,
) -> Option<AgentIdentity> {
    let seed = <[u8; 32]>::try_from(hex::decode(key?).ok()?).ok()?;
    let delegation = Delegation::decode(&hex::decode(delegation?).ok()?).ok()?;
    let key = SigningKey::from_bytes(&seed);
    let cert = cert?;
    (cert.device_public_key == key.verifying_key().to_bytes()
        && delegation.device_public_key == cert.device_public_key
        && delegation.delegate.agent_id().is_some())
    .then_some(AgentIdentity {
        key,
        cert,
        delegation,
    })
}

fn grant_proposal(
    request: &GrantRequest,
    now_ns: u64,
    agent_id: &str,
) -> Result<DelegateProposal, String> {
    let label = request.label.trim();
    if label.is_empty() || label.len() > MAX_DELEGATE_TEXT_BYTES {
        return Err(format!(
            "a grant's label is 1 to {MAX_DELEGATE_TEXT_BYTES} bytes"
        ));
    }
    if request.workspaces.is_empty() || request.workspaces.len() > MAX_DELEGATED_WORKSPACES {
        return Err(format!(
            "a grant names 1 to {MAX_DELEGATED_WORKSPACES} workspaces"
        ));
    }
    if request.apps.len() > MAX_DELEGATED_APPS {
        return Err(format!("a grant names at most {MAX_DELEGATED_APPS} apps"));
    }
    if !GRANT_SECS.contains(&request.expires_in_secs) {
        return Err("a grant lasts from a minute to 30 days".into());
    }
    // A grant that names no colour wears the one the agent member it is wears everywhere.
    let color = if let Some(text) = &request.color {
        parse_color_hex(text).ok_or("a grant's colour is #rrggbb")?
    } else {
        let (r, g, b) =
            collab_types::presence::identity_rgb(collab_types::presence::PresenceHue::of_member(
                &collab_types::MemberId::agent(agent_id),
            ));
        [r, g, b]
    };
    Ok(DelegateProposal {
        delegate: Delegate::Agent {
            agent_id: agent_id.to_owned(),
        },
        label: label.to_owned(),
        color,
        scope: DelegateScope {
            workspaces: request
                .workspaces
                .iter()
                .filter_map(|workspace| WorkspaceId::parse(workspace))
                .collect(),
            apps: request.apps.clone(),
        },
        expires_at_ns: now_ns.saturating_add(request.expires_in_secs.saturating_mul(1_000_000_000)),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

    /// The `writer` agent of `root`, delegated until `expires_at_ns`.
    pub(crate) fn identity_until(root: &SigningKey, expires_at_ns: u64) -> AgentIdentity {
        let key = SigningKey::from_bytes(&[9; 32]);
        let device = key.verifying_key().to_bytes();
        let delegation = Delegation::sign(
            root,
            device,
            Delegate::Agent {
                agent_id: "writer".into(),
            },
            "Writer helper",
            [0x7c, 0x3a, 0xed],
            DelegateScope {
                workspaces: vec![WorkspaceId::parse(WORKSPACE).unwrap()],
                apps: Vec::new(),
            },
            1,
            expires_at_ns,
        )
        .unwrap();
        let cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device,
            device_label: "Agent: Writer helper".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        AgentIdentity {
            key,
            cert,
            delegation,
        }
    }
}
