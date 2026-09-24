//! Canonical agent authorization and business ledger. No wire identity or executor.
use crate::StatePeer;
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

/// Host-local authorization evidence, deliberately not Serde and with no public
/// fields. `authenticated_device` MUST come from a completed authenticated channel
/// (e.g. accept_hello + verify_hello_ack2), never request JSON or a device label.
/// This constructor checks authorization; it does not perform proof of possession.
/// ```compile_fail
/// let _: qos_peer_state::owner_commit::VerifiedAgentCaller = serde_json::from_str("{}").unwrap();
/// ```
#[derive(Clone, Debug)]
pub struct VerifiedAgentCaller {
    scope: [u8; 32],
    authority: DeviceCert,
    certificate: DeviceCert,
    room: String,
    person: String,
    generation: u64,
    governance: String,
}
impl VerifiedAgentCaller {
    pub fn from_authenticated_source(
        peer: &StatePeer,
        owner_scope: [u8; 32],
        generation: u64,
        room: &str,
        authenticated_device: [u8; 32],
        now: SystemTime,
    ) -> Result<Self, String> {
        if generation == 0 || owner_scope == [0; 32] || room != peer.room() {
            return Err("invalid agent owner context".into());
        }
        let owner = canonical_owner(peer)?;
        // The pinned host is current too; a still-live browser cannot keep a
        // revoked signing authority usable.
        peer.permissions(peer.authority().device_public_key, now)?;
        let (_, _, person) = peer.permissions(authenticated_device, now)?;
        let certificate = peer
            .certificates()
            .into_iter()
            .find(|c| c.device_public_key == authenticated_device)
            .ok_or("current certificate missing")?;
        if person != owner || certificate.capabilities.can_act_as_host {
            return Err("agent control requires the current person owner".into());
        }
        Ok(Self {
            scope: owner_scope,
            authority: peer.authority().clone(),
            certificate,
            room: room.into(),
            person,
            generation,
            governance: governance_revision(peer)?,
        })
    }
    pub fn room(&self) -> &str {
        &self.room
    }
    pub fn person(&self) -> &str {
        &self.person
    }
    pub fn device(&self) -> [u8; 32] {
        self.certificate.device_public_key
    }
    pub fn certificate(&self) -> &DeviceCert {
        &self.certificate
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn governance_revision(&self) -> &str {
        &self.governance
    }
    /// Refresh an existing authenticated channel proof without changing its identity.
    pub fn refresh(
        &self,
        peer: &StatePeer,
        scope: [u8; 32],
        generation: u64,
        now: SystemTime,
    ) -> Result<Self, String> {
        let current = Self::from_authenticated_source(
            peer,
            scope,
            generation,
            &self.room,
            self.device(),
            now,
        )?;
        if self.scope != current.scope
            || self.authority != current.authority
            || self.certificate != current.certificate
            || self.person != current.person
            || self.generation > generation
        {
            return Err("stale or foreign authenticated agent channel".into());
        }
        Ok(current)
    }
    pub fn revalidate(
        &self,
        peer: &StatePeer,
        scope: [u8; 32],
        generation: u64,
        now: SystemTime,
    ) -> Result<(), String> {
        let current = Self::from_authenticated_source(
            peer,
            scope,
            generation,
            &self.room,
            self.device(),
            now,
        )?;
        if self.scope != current.scope
            || self.authority != current.authority
            || self.certificate != current.certificate
            || self.person != current.person
            || self.governance != current.governance
            || self.generation > generation
        {
            return Err("stale or foreign agent owner proof".into());
        }
        Ok(())
    }
}

pub fn canonical_owner(peer: &StatePeer) -> Result<String, String> {
    peer.legacy_canonical_owner()
}
/// Opaque policy identity. Deliberately excludes aggregate generation, room
/// heads, document coverage and runtime task/output progress.
pub fn governance_revision(peer: &StatePeer) -> Result<String, String> {
    peer.legacy_governance_revision()
}

/// Stable local owner namespace. Canonical root path prevents a proof obtained
/// for a separate copy of an aggregate from authorizing this persistence owner.
pub fn owner_scope(
    canonical_root: &std::path::Path,
    namespace: &str,
    peer: &StatePeer,
) -> [u8; 32] {
    let mut hash = blake3::Hasher::new_derive_key("qOS canonical agent owner scope v1");
    for value in [
        canonical_root.as_os_str().as_encoded_bytes(),
        namespace.as_bytes(),
        peer.room().as_bytes(),
        &peer.authority().device_public_key,
    ] {
        hash.update(&(value.len() as u64).to_le_bytes());
        hash.update(value);
    }
    *hash.finalize().as_bytes()
}

use super::{CommitBinding, CommitOperation, CommitTarget, OwnerCommitReceipt, OwnerCommitRequest};
pub use qos_agent_control_protocol::{AgentError, GrantAction};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const PRIVATE_AGENT_LEDGER_VERSION: u32 = 1;
pub const MAX_PRIVATE_AGENT_ADMISSIONS: usize = 1024;
const MUTATIONS_PER_ADMISSION: usize = 8;
pub const MAX_PRIVATE_AGENT_LEDGER_BYTES: usize = 1024 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentCommitError {
    Rejected(String),
    Conflict {
        current_generation: u64,
    },
    Uncertain {
        generation: Option<u64>,
        reason: String,
    },
    Capacity {
        limit: usize,
    },
}
impl std::fmt::Display for AgentCommitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for AgentCommitError {}
pub type AgentCommitFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<OwnerCommitReceipt, AgentCommitError>> + Send>,
>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentIds {
    pub mutation: String,
    pub invocation: String,
    pub agent: String,
    pub task: String,
    pub run: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentControlPhase {
    Admitted,
    Launching,
    Running,
    Completed,
    Failed,
    Unknown,
}
impl AgentControlPhase {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
    fn permits(self, next: Self) -> bool {
        use AgentControlPhase::*;
        matches!(
            (self, next),
            (Admitted, Launching | Unknown | Failed)
                | (Launching, Running | Unknown | Failed)
                | (Running, Completed | Unknown | Failed)
                | (Unknown, Completed | Failed)
        )
    }
}
/// Host-local mutation only. No deserialized mutation carries caller authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum AgentMutation {
    Prepare {
        invocation: String,
        request: Vec<u8>,
        grants: Vec<GrantAction>,
    },
    Transition {
        phase: AgentControlPhase,
        error: Option<AgentError>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentChange {
    pub ids: AgentIds,
    pub caller: DeviceCert,
    pub expected_generation: u64,
    pub expected_governance: String,
    pub payload_digest: [u8; 32],
    pub mutation: AgentMutation,
}
impl AgentChange {
    pub fn new(
        caller: &VerifiedAgentCaller,
        ids: AgentIds,
        mutation: AgentMutation,
    ) -> Result<Self, String> {
        let mut value = Self {
            ids,
            caller: caller.certificate.clone(),
            expected_generation: caller.generation,
            expected_governance: caller.governance.clone(),
            payload_digest: [0; 32],
            mutation,
        };
        value.payload_digest = *blake3::hash(&value.business_bytes()?).as_bytes();
        value.validate()?;
        Ok(value)
    }
    pub fn business_bytes(&self) -> Result<Vec<u8>, String> {
        postcard::to_allocvec(&(
            &self.ids,
            &self.caller,
            &self.expected_governance,
            &self.mutation,
        ))
        .map_err(|e| e.to_string())
    }
    pub fn validate(&self) -> Result<(), String> {
        for id in [
            &self.ids.mutation,
            &self.ids.invocation,
            &self.ids.agent,
            &self.ids.task,
            &self.ids.run,
            &self.expected_governance,
        ] {
            opaque(id)?;
        }
        if self.expected_generation == 0
            || self.payload_digest != *blake3::hash(&self.business_bytes()?).as_bytes()
        {
            return Err("invalid agent binding digest/generation".into());
        }
        if self.business_bytes()?.len() > 128 * 1024 {
            return Err("agent owner mutation exceeds bound".into());
        }
        match &self.mutation {
            AgentMutation::Prepare {
                invocation,
                request,
                grants,
            } => {
                opaque(invocation)?;
                validate_intent(request, grants)?;
            }
            AgentMutation::Transition { phase, error } => {
                if *phase == AgentControlPhase::Admitted
                    || (matches!(
                        phase,
                        AgentControlPhase::Failed | AgentControlPhase::Unknown
                    ) != error.is_some())
                {
                    return Err("invalid agent outcome diagnostic".into());
                }
                if let Some(error) = error {
                    qos_agent_control_protocol::validate_operation_diagnostic(error)
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(())
    }
}
fn opaque(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value.is_ascii()
        || value.trim() != value
        || value.bytes().any(|b| b.is_ascii_control())
    {
        Err("invalid exact agent identity".into())
    } else {
        Ok(())
    }
}
fn validate_intent(request: &[u8], grants: &[GrantAction]) -> Result<(), String> {
    use qos_agent_control_protocol::{AgentRequest, canonical_request, decode_request};
    let decoded = decode_request(request).map_err(|e| e.to_string())?;
    // Follow-up messages need an existing agent/task acceptance binding. This
    // admission-only owner does not supply that runtime contract yet.
    if !matches!(decoded, AgentRequest::Spawn { .. })
        || canonical_request(&decoded).map_err(|e| e.to_string())? != request
    {
        return Err("agent admission requires canonical mutation request".into());
    }
    if grants.is_empty() || grants.len() > 64 {
        return Err("agent admission requires bounded exact grants".into());
    }
    let mut keys = std::collections::BTreeSet::new();
    for g in grants {
        for id in [&g.app_id, &g.record_id, &g.resource, &g.action] {
            opaque(id)?;
        }
        if !keys.insert((&g.app_id, &g.record_id, &g.resource, &g.action)) {
            return Err("duplicate agent grant".into());
        }
    }
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateAgentRecord {
    pub ids: AgentIds,
    pub invocation: String,
    pub caller: DeviceCert,
    pub request: Vec<u8>,
    pub grants: Vec<GrantAction>,
    pub phase: AgentControlPhase,
    pub admitted_generation: u64,
    pub last_generation: u64,
    pub error: Option<AgentError>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMutationReceipt {
    pub binding: CommitBinding,
    pub generation: u64,
    pub invocation: String,
    pub change: AgentChange,
}
/// Private owner data. Never serialize this ledger into a StatePeer room or publication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateAgentLedger {
    pub version: u32,
    pub records: BTreeMap<String, PrivateAgentRecord>,
    pub mutations: BTreeMap<String, AgentMutationReceipt>,
}
impl Default for PrivateAgentLedger {
    fn default() -> Self {
        Self {
            version: PRIVATE_AGENT_LEDGER_VERSION,
            records: BTreeMap::new(),
            mutations: BTreeMap::new(),
        }
    }
}
impl PrivateAgentLedger {
    pub fn validate(&self, room: &str, generation: u64) -> Result<(), String> {
        if self.version != PRIVATE_AGENT_LEDGER_VERSION
            || self.records.len() > MAX_PRIVATE_AGENT_ADMISSIONS
            || self.mutations.len() > MAX_PRIVATE_AGENT_ADMISSIONS * MUTATIONS_PER_ADMISSION
        {
            return Err("unsupported or over-capacity private agent ledger".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut tasks = std::collections::BTreeSet::new();
        let mut runs = std::collections::BTreeSet::new();
        for (key, r) in &self.records {
            opaque(key)?;
            for id in [
                &r.ids.mutation,
                &r.ids.invocation,
                &r.ids.agent,
                &r.ids.task,
                &r.ids.run,
            ] {
                opaque(id)?;
            }
            validate_intent(&r.request, &r.grants)?;
            if key != &r.invocation
                || !ids.insert(&r.ids.invocation)
                || !tasks.insert(&r.ids.task)
                || !runs.insert(&r.ids.run)
                || r.admitted_generation == 0
                || r.last_generation < r.admitted_generation
                || r.last_generation > generation
            {
                return Err("inconsistent private agent identity/generation".into());
            }
            tilo_user_identity::verify_device_cert(&r.caller).map_err(|e| e.to_string())?;
            if r.caller.capabilities.can_act_as_host
                || (matches!(
                    r.phase,
                    AgentControlPhase::Failed | AgentControlPhase::Unknown
                ) != r.error.is_some())
            {
                return Err("invalid retained agent caller/outcome".into());
            }
            if let Some(error) = &r.error {
                qos_agent_control_protocol::validate_operation_diagnostic(error)
                    .map_err(|e| e.to_string())?;
            }
            let admission = self
                .mutations
                .get(&r.ids.mutation)
                .ok_or("private admission receipt missing")?;
            if admission.invocation != *key || admission.generation != r.admitted_generation {
                return Err("private admission receipt differs".into());
            }
            let mut receipts = self
                .mutations
                .values()
                .filter(|m| m.invocation == *key)
                .collect::<Vec<_>>();
            receipts.sort_by_key(|m| m.generation);
            if receipts.len() > MUTATIONS_PER_ADMISSION
                || receipts.iter().map(|m| m.generation).max() != Some(r.last_generation)
            {
                return Err("private outcome receipt differs".into());
            }
            let mut phase = AgentControlPhase::Admitted;
            let mut diagnostic = None;
            let mut previous_generation = 0;
            for (index, receipt) in receipts.into_iter().enumerate() {
                let c = &receipt.change;
                if receipt.generation <= previous_generation
                    || c.caller != r.caller
                    || c.ids.invocation != r.ids.invocation
                    || c.ids.agent != r.ids.agent
                    || c.ids.task != r.ids.task
                    || c.ids.run != r.ids.run
                {
                    return Err("private outcome lost immutable admission binding".into());
                }
                previous_generation = receipt.generation;
                match (&c.mutation, index) {
                    (
                        AgentMutation::Prepare {
                            invocation,
                            request,
                            grants,
                        },
                        0,
                    ) if invocation == key
                        && request == &r.request
                        && grants == &r.grants
                        && c.ids == r.ids =>
                    {
                        ()
                    }
                    (AgentMutation::Transition { phase: next, error }, n)
                        if n > 0 && phase.permits(*next) =>
                    {
                        phase = *next;
                        diagnostic = error.clone();
                    }
                    _ => return Err("private mutation history is inconsistent".into()),
                }
            }
            if phase != r.phase || diagnostic != r.error {
                return Err("private agent outcome is not backed by exact receipts".into());
            }
        }
        for (id, m) in &self.mutations {
            opaque(id)?;
            let request = OwnerCommitRequest {
                room: room.into(),
                operation: CommitOperation::Agent {
                    change: Box::new(m.change.clone()),
                },
            };
            if m.change.ids.mutation != *id
                || m.change.expected_generation.checked_add(1) != Some(m.generation)
                || request.binding()? != m.binding
            {
                return Err("private mutation receipt does not bind exact request".into());
            }
            if m.generation == 0
                || m.generation > generation
                || m.binding.room != room
                || m.binding.request_digest == [0; 32]
                || m.binding.basis == [0; 32]
                || m.binding.target
                    != (CommitTarget::Agent {
                        mutation: id.clone(),
                    })
                || !self.records.contains_key(&m.invocation)
            {
                return Err("invalid private agent receipt".into());
            }
        }
        struct BoundWriter(usize);
        impl std::io::Write for BoundWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 = self
                    .0
                    .checked_add(bytes.len())
                    .ok_or_else(|| std::io::Error::other("private ledger size overflow"))?;
                if self.0 > MAX_PRIVATE_AGENT_LEDGER_BYTES {
                    return Err(std::io::Error::other("private agent ledger exceeds budget"));
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        serde_json::to_writer(&mut BoundWriter(0), self).map_err(|e| e.to_string())
    }
    pub fn record_for(
        &self,
        caller: &VerifiedAgentCaller,
        invocation: &str,
    ) -> Result<Option<&PrivateAgentRecord>, String> {
        opaque(invocation)?;
        let value = self.records.get(invocation);
        if value.is_some_and(|r| r.caller != caller.certificate) {
            return Err("agent invocation belongs to another certified caller".into());
        }
        Ok(value)
    }
}
#[derive(Clone, Debug)]
pub struct AgentStateSnapshot {
    pub room: String,
    pub generation: u64,
    pub governance_revision: String,
    pub ledger: PrivateAgentLedger,
}
pub enum PreparedAgentCommit {
    Replay(AgentMutationReceipt),
    Candidate(PrivateAgentLedger),
}
/// Pure private candidate preparation; neither candidate nor receipt grants execution authority.
pub fn prepare_agent_commit(
    peer: &StatePeer,
    ledger: &PrivateAgentLedger,
    scope: [u8; 32],
    generation: u64,
    request: &OwnerCommitRequest,
    caller: &VerifiedAgentCaller,
    now: SystemTime,
) -> Result<PreparedAgentCommit, AgentCommitError> {
    use AgentCommitError::Rejected;
    caller
        .revalidate(peer, scope, generation, now)
        .map_err(Rejected)?;
    ledger.validate(peer.room(), generation).map_err(Rejected)?;
    let CommitOperation::Agent { change } = &request.operation else {
        return Err(Rejected("not an agent owner request".into()));
    };
    change.validate().map_err(Rejected)?;
    if request.room != peer.room() || change.caller != caller.certificate {
        return Err(Rejected("agent owner scope/caller mismatch".into()));
    }
    let binding = request.binding().map_err(Rejected)?;
    if let Some(prior) = ledger.mutations.get(&change.ids.mutation) {
        if prior.binding != binding {
            return Err(Rejected("agent mutation identity conflict".into()));
        }
        return Ok(PreparedAgentCommit::Replay(prior.clone()));
    }
    if change.expected_governance != caller.governance {
        return Err(Rejected("agent grant policy changed".into()));
    }
    if change.expected_generation != generation {
        return Err(AgentCommitError::Conflict {
            current_generation: generation,
        });
    }
    let next = generation
        .checked_add(1)
        .ok_or_else(|| Rejected("agent generation exhausted".into()))?;
    let mut candidate = ledger.clone();
    let invocation = match &change.mutation {
        AgentMutation::Prepare {
            invocation,
            request,
            grants,
        } => {
            // Reserve the identity globally in this host's single-workspace ledger,
            // not just in a caller-scoped lookup. Existing identities never expire.
            if candidate.records.contains_key(invocation)
                || candidate.records.values().any(|r| {
                    r.ids.invocation == change.ids.invocation
                        || r.ids.task == change.ids.task
                        || r.ids.run == change.ids.run
                })
            {
                return Err(Rejected(
                    "agent invocation/task/run already reserved".into(),
                ));
            }
            if candidate.records.len() >= MAX_PRIVATE_AGENT_ADMISSIONS {
                return Err(AgentCommitError::Capacity {
                    limit: MAX_PRIVATE_AGENT_ADMISSIONS,
                });
            }
            if candidate
                .records
                .values()
                .filter(|r| !r.phase.terminal())
                .count()
                >= 4
            {
                return Err(AgentCommitError::Capacity { limit: 4 });
            }
            for grant in grants {
                let record = if grant.record_id == "@qos:package-primary" {
                    ""
                } else {
                    &grant.record_id
                };
                if !collab_types::first_party_app_ids::is_any_first_party_app(
                    &grant.app_id,
                    &["so.tilo.word-processor", "so.tilo.chat"],
                ) || peer.document(&grant.app_id, record).is_none()
                {
                    return Err(Rejected(
                        "agent grant has no exact supported canonical record".into(),
                    ));
                }
            }
            candidate.records.insert(
                invocation.clone(),
                PrivateAgentRecord {
                    ids: change.ids.clone(),
                    invocation: invocation.clone(),
                    caller: change.caller.clone(),
                    request: request.clone(),
                    grants: grants.clone(),
                    phase: AgentControlPhase::Admitted,
                    admitted_generation: next,
                    last_generation: next,
                    error: None,
                },
            );
            invocation.clone()
        }
        AgentMutation::Transition { phase, error } => {
            let record = candidate
                .records
                .values_mut()
                .find(|r| r.ids.invocation == change.ids.invocation)
                .ok_or_else(|| Rejected("agent admission missing".into()))?;
            if record.caller != change.caller
                || record.ids.agent != change.ids.agent
                || record.ids.task != change.ids.task
                || record.ids.run != change.ids.run
                || !record.phase.permits(*phase)
            {
                return Err(Rejected("invalid agent outcome transition".into()));
            }
            record.phase = *phase;
            record.last_generation = next;
            record.error = error.clone();
            record.invocation.clone()
        }
    };
    candidate.mutations.insert(
        change.ids.mutation.clone(),
        AgentMutationReceipt {
            binding,
            generation: next,
            invocation,
            change: change.as_ref().clone(),
        },
    );
    candidate.validate(peer.room(), next).map_err(Rejected)?;
    Ok(PreparedAgentCommit::Candidate(candidate))
}
