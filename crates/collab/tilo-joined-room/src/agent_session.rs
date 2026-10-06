//! Host-local launch custody in the native owner's existing durable record.
//! These records confer no live process authority: the launcher must still
//! verify its signed runtime, kernel process and exact connected socket.
use crate::{NativeCommittedView, NativeWorkspaceOwner, RoomId};
use serde::{Deserialize, Serialize};
use std::time::SystemTime;
use tilo_user_identity::DeviceCert;

const MAX_SESSIONS: usize = 4096;

pub(crate) fn validate(view: &NativeCommittedView) -> Result<(), String> {
    if view.record.agent_sessions.len() > MAX_SESSIONS {
        return Err("agent session custody capacity exceeded".into());
    }
    let mut live_agents = std::collections::BTreeSet::new();
    for (key, session) in &view.record.agent_sessions {
        let reservation = &session.reservation;
        let binding = &session.binding;
        if key != &reservation.launch
            || key != &binding.launch
            || reservation.room != view.record.room
            || binding.room != reservation.room
            || binding.agent != reservation.agent
            || binding.runtime_epoch != reservation.runtime_epoch
            || binding.runtime_digest != reservation.runtime_digest
            || binding.generation == 0
            || binding.generation > view.generation()
            || invalid_reservation(reservation)
        {
            return Err("invalid retained agent session binding".into());
        }
        if let SessionPhase::ProcessBound { evidence_digest } = &session.phase
            && *evidence_digest == [0; 32]
        {
            return Err("retained agent process evidence is empty".into());
        }
        if session.phase != SessionPhase::Closed && !live_agents.insert(&binding.agent) {
            return Err("multiple retained sessions name one live Agent".into());
        }
    }
    Ok(())
}

fn invalid_reservation(request: &AgentSessionReservation) -> bool {
    request.room.is_nil()
        || tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::RemoveAgent {
            agent: request.agent.clone(),
        }
        .validate()
        .is_err()
        || [&request.launch, &request.runtime_epoch]
            .into_iter()
            .any(|id| {
                RoomId::parse(id).is_none_or(|value| value.is_nil() || value.to_string() != *id)
            })
        || request.runtime_digest == [0; 32]
}

/// Host-retained original launch request, never decoded from agent arguments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionReservation {
    pub room: RoomId,
    pub caller: DeviceCert,
    pub attestor: DeviceCert,
    pub agent: String,
    pub launch: String,
    pub runtime_epoch: String,
    pub runtime_digest: [u8; 32],
}

/// Descriptive reservation identity; possession is not a launch capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionBinding {
    pub room: RoomId,
    pub agent: String,
    pub launch: String,
    pub runtime_epoch: String,
    pub runtime_digest: [u8; 32],
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum SessionPhase {
    Reserved,
    Claimed,
    ProcessBound { evidence_digest: [u8; 32] },
    Closed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredAgentSession {
    reservation: AgentSessionReservation,
    binding: AgentSessionBinding,
    phase: SessionPhase,
}

pub(crate) fn authorize_host(
    view: &NativeCommittedView,
    attestor: &DeviceCert,
    now: SystemTime,
) -> Result<(), String> {
    let pinned = view.host().unwrap_or_else(|| view.governor());
    if attestor != pinned
        || view.device() != attestor
        || (view.host().is_some() && !attestor.capabilities.can_act_as_host)
    {
        return Err("agent session requires the exact local pinned host".into());
    }
    let certificates = view.peer().certificates();
    if !certificates.contains(attestor) {
        return Err("agent session certificate is not exact admitted evidence".into());
    }
    view.peer().permissions(attestor.device_public_key, now)?;
    Ok(())
}

fn authorize(
    view: &NativeCommittedView,
    reservation: &AgentSessionReservation,
    attestor: &DeviceCert,
    now: SystemTime,
) -> Result<(), String> {
    authorize_host(view, attestor, now)?;
    if &reservation.attestor != attestor {
        return Err("agent session's original host custody changed".into());
    }
    if !view.peer().certificates().contains(&reservation.caller) {
        return Err("agent session person certificate is not exact admitted evidence".into());
    }
    let (role, caps, person) = view
        .peer()
        .permissions(reservation.caller.device_public_key, now)?;
    let document = view
        .peer()
        .admitted_room()
        .ok_or("agent session room is absent")?;
    let (_, owner, _) = tilo_peer_state::room::extract_governed_fields(document);
    let mut workspace = tilo_desktop_model::Workspace::new(
        tilo_desktop_model::WorkspaceId(view.workspace()),
        "agent session authority",
    );
    workspace.room = reservation.room;
    workspace.owner = owner;
    workspace.capabilities = caps;
    let seconds = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| "agent session clock precedes the Unix epoch")?
        .as_secs();
    if !matches!(role, collab_types::Role::Owner | collab_types::Role::Editor)
        || !workspace.may_administer_at(&person, seconds)
    {
        return Err("agent session person lacks current administration authority".into());
    }
    if workspace
        .role_of_agent_at(&reservation.agent, seconds)
        .is_none_or(|role| role == collab_types::Role::Denied)
    {
        return Err("agent session agent is not a canonical room member".into());
    }
    Ok(())
}

impl NativeWorkspaceOwner {
    /// Reserve an idle collaborator independently of any queued task.
    ///
    /// # Errors
    /// The exact person/host/agent authority is absent, identifiers are invalid,
    /// another session is active, or the durable publication fails.
    pub fn reserve_agent_session(
        &self,
        request: AgentSessionReservation,
        now: SystemTime,
    ) -> Result<AgentSessionBinding, String> {
        if invalid_reservation(&request) {
            return Err("invalid native agent session reservation".into());
        }
        let _turn = self.0.mutation.lock();
        let room = request.room;
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        authorize(&candidate, &request, &request.attestor, now)?;
        if let Some(existing) = candidate.record.agent_sessions.get(&request.launch) {
            if existing.reservation != request || existing.phase == SessionPhase::Closed {
                return Err("agent session launch identity was already consumed".into());
            }
            return Ok(existing.binding.clone());
        }
        let launch = uuid::Uuid::parse_str(&request.launch)
            .map_err(|_| "invalid native Agent launch UUID")?;
        if crate::agent_session_history::lookup(
            self.0.store.as_ref(),
            candidate.record.agent_session_history_root,
            launch,
        )?
        .is_some()
        {
            return Err("agent session launch identity was already consumed".into());
        }
        if candidate
            .record
            .agent_sessions
            .values()
            .filter(|session| session.phase != SessionPhase::Closed)
            .count()
            >= MAX_SESSIONS
        {
            return Err("agent session custody capacity exhausted".into());
        }
        if candidate.record.agent_sessions.values().any(|session| {
            session.binding.agent == request.agent && session.phase != SessionPhase::Closed
        }) {
            return Err("agent already has a retained session in this room".into());
        }
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        let binding = AgentSessionBinding {
            room,
            agent: request.agent.clone(),
            launch: request.launch.clone(),
            runtime_epoch: request.runtime_epoch.clone(),
            runtime_digest: request.runtime_digest,
            generation: candidate.generation(),
        };
        candidate.record.agent_sessions.insert(
            request.launch.clone(),
            StoredAgentSession {
                reservation: request,
                binding: binding.clone(),
                phase: SessionPhase::Reserved,
            },
        );
        self.publish_room(room, candidate, None)?;
        Ok(binding)
    }

    /// Check retained authority without consuming or recreating a launch.
    ///
    /// # Errors
    /// The binding, original person, host or agent authority is no longer current.
    pub fn revalidate_agent_session(
        &self,
        binding: &AgentSessionBinding,
        attestor: &DeviceCert,
        now: SystemTime,
    ) -> Result<(), String> {
        self.with_current(binding.room, |view| {
            let session = view
                .record
                .agent_sessions
                .get(&binding.launch)
                .ok_or("native agent session is not reserved")?;
            if session.binding != *binding || session.phase == SessionPhase::Closed {
                return Err("native agent session binding is stale or closed".into());
            }
            authorize(view, &session.reservation, attestor, now)
        })
    }

    /// Check that the exact reserved launch crossed the durable process-bound
    /// barrier. A claimed reservation alone cannot be adopted by the broker.
    ///
    /// # Errors
    /// The binding or current authority changed, or no process was durably bound.
    pub fn revalidate_bound_agent_session(
        &self,
        binding: &AgentSessionBinding,
        attestor: &DeviceCert,
        now: SystemTime,
    ) -> Result<(), String> {
        self.with_current(binding.room, |view| {
            let session = view
                .record
                .agent_sessions
                .get(&binding.launch)
                .ok_or("native agent session is not reserved")?;
            if session.binding != *binding
                || !matches!(session.phase, SessionPhase::ProcessBound { .. })
            {
                return Err("native agent session is not process-bound".into());
            }
            authorize(view, &session.reservation, attestor, now)
        })
    }

    /// Consume the exact launch claim once, before any executable process.
    ///
    /// # Errors
    /// Authority is stale, the claim was already consumed, or publication fails.
    pub fn claim_agent_session(
        &self,
        binding: &AgentSessionBinding,
        attestor: &DeviceCert,
        now: SystemTime,
    ) -> Result<(), String> {
        self.transition_agent_session(binding, attestor, SessionPhase::Claimed, now)
    }

    /// Retain a digest of actual launcher-verified process/channel evidence.
    /// The digest itself never confers live process or socket custody.
    ///
    /// # Errors
    /// The session was not claimed, evidence changed, or publication fails.
    pub fn bind_agent_session_process(
        &self,
        binding: &AgentSessionBinding,
        attestor: &DeviceCert,
        evidence_digest: [u8; 32],
        now: SystemTime,
    ) -> Result<(), String> {
        if evidence_digest == [0; 32] {
            return Err("agent session process evidence is empty".into());
        }
        self.transition_agent_session(
            binding,
            attestor,
            SessionPhase::ProcessBound { evidence_digest },
            now,
        )
    }

    /// Permanently close launch custody; reopening never revives this identity.
    ///
    /// # Errors
    /// Host authority or the binding is stale, or publication fails.
    pub fn close_agent_session(
        &self,
        binding: &AgentSessionBinding,
        attestor: &DeviceCert,
        now: SystemTime,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(binding.room, |view| Ok(view.clone()))?;
        authorize_host(&candidate, attestor, now)?;
        let Some(session) = candidate.record.agent_sessions.get(&binding.launch) else {
            let launch = uuid::Uuid::parse_str(&binding.launch)
                .map_err(|_| "invalid native Agent launch UUID")?;
            let archived = crate::agent_session_history::lookup(
                self.0.store.as_ref(),
                candidate.record.agent_session_history_root,
                launch,
            )?;
            return if archived.is_some_and(|claim| claim.binding_digest == binding_digest(binding))
            {
                Ok(())
            } else {
                Err("native agent session is not reserved".into())
            };
        };
        if session.binding != *binding {
            return Err("native agent session binding changed".into());
        }
        let launch = uuid::Uuid::parse_str(&binding.launch)
            .map_err(|_| "invalid native Agent launch UUID")?;
        let root = crate::agent_session_history::insert(
            self.0.store.as_ref(),
            candidate.record.agent_session_history_root,
            crate::agent_session_history::Claim {
                launch,
                binding_digest: binding_digest(binding),
            },
        )?;
        candidate.record.agent_session_history_root = Some(root);
        candidate.record.agent_sessions.remove(&binding.launch);
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_room(binding.room, candidate, None)
    }

    fn transition_agent_session(
        &self,
        binding: &AgentSessionBinding,
        attestor: &DeviceCert,
        next: SessionPhase,
        now: SystemTime,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(binding.room, |view| Ok(view.clone()))?;
        let current = candidate
            .record
            .agent_sessions
            .get(&binding.launch)
            .ok_or("native agent session is not reserved")?;
        if current.binding != *binding {
            return Err("native agent session binding changed".into());
        }
        authorize(&candidate, &current.reservation, attestor, now)?;
        let permitted = matches!(
            (&current.phase, &next),
            (SessionPhase::Reserved, SessionPhase::Claimed)
                | (SessionPhase::Claimed, SessionPhase::ProcessBound { .. })
        );
        if current.phase == next && next != SessionPhase::Claimed {
            return Ok(());
        }
        if !permitted {
            return Err("native agent session transition is stale or already consumed".into());
        }
        candidate
            .record
            .agent_sessions
            .get_mut(&binding.launch)
            .ok_or("native agent session disappeared")?
            .phase = next;
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_room(binding.room, candidate, None)
    }
}

fn binding_digest(binding: &AgentSessionBinding) -> [u8; 32] {
    let mut digest = blake3::Hasher::new();
    digest.update(b"tilo/native-agent-session-binding/v1");
    digest.update(&binding.room.as_uuid().as_bytes()[..]);
    for field in [&binding.agent, &binding.launch, &binding.runtime_epoch] {
        digest.update(&(field.len() as u64).to_le_bytes());
        digest.update(field.as_bytes());
    }
    digest.update(&binding.runtime_digest);
    digest.update(&binding.generation.to_le_bytes());
    *digest.finalize().as_bytes()
}

#[cfg(test)]
#[path = "agent_session_tests.rs"]
pub(crate) mod tests;
