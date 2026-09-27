//! Canonical agent caller authorization and owner scope. No wire identity or executor.
use crate::StatePeer;
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

/// Host-local authorization evidence, deliberately not Serde and with no public
/// fields. `authenticated_device` MUST come from a completed authenticated channel
/// (e.g. `accept_hello` + `verify_hello_ack2`), never request JSON or a device label.
/// This constructor checks authorization; it does not perform proof of possession.
/// ```compile_fail
/// let _: tilo_peer_state::owner_commit::VerifiedAgentCaller = serde_json::from_str("{}").unwrap();
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
    /// Authorize `authenticated_device` as an agent caller of this room.
    ///
    /// # Errors
    ///
    /// Returns an error if the device is not a current governed member allowed to run agents.
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
    ///
    /// # Errors
    ///
    /// Returns an error if the caller is no longer authorized for `scope` and `generation`.
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
    /// Re-check this caller's authorization against current governance.
    ///
    /// # Errors
    ///
    /// Returns an error if the caller is no longer authorized for `scope` and `generation`.
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

/// The canonical owner of `peer`'s room.
///
/// # Errors
///
/// Returns an error if room governance is missing or malformed.
pub fn canonical_owner(peer: &StatePeer) -> Result<String, String> {
    peer.legacy_canonical_owner()
}
/// Opaque policy identity. Deliberately excludes aggregate generation, room
/// heads, document coverage and runtime task/output progress.
///
/// # Errors
///
/// Returns an error if room governance is missing or malformed.
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
    let mut hash = blake3::Hasher::new_derive_key("Tilo canonical agent owner scope v1");
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

pub use tilo_agent_control_protocol::{AgentError, GrantAction};

pub const MAX_PRIVATE_AGENT_ADMISSIONS: usize = 1024;
/// Live (admitted, not failed, process not exited) spawned agents one
/// workspace host runs at once. An idle agent keeps its process for later
/// messages, so this is the number of agents a workspace can hold, not a
/// concurrency of running tasks. A Spawn beyond it is refused as
/// `AgentCommitError::Capacity { limit: MAX_LIVE_PRIVATE_AGENTS }`.
pub const MAX_LIVE_PRIVATE_AGENTS: usize = 16;
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

/// Every record a retained grant names must be a current canonical document
/// of a delegable app. Scope carriers name all their records; the app list
/// comes from the host-pinned delegation registry, not a hard-coded match.
///
/// # Errors
///
/// Returns an error if a grant names a record that is not a current canonical document
/// of a delegable app.
pub fn grant_records_current(peer: &StatePeer, grants: &[GrantAction]) -> Result<(), String> {
    use tilo_agent_control_protocol::{DelegationRegistry, route_record, scopes_from_grants};
    let registry = DelegationRegistry::first_party();
    let scopes = scopes_from_grants(grants).map_err(|e| e.to_string())?;
    for scope in &scopes {
        if !registry.is_delegable_app(&scope.app_id) || scope.records().is_empty() {
            return Err("agent grant has no exact supported canonical record".into());
        }
        // The desktop's open-application action names the room desktop,
        // which has no application document of its own.
        if scope.app_id == tilo_agent_control_protocol::DESKTOP_APP_ID {
            if scope.records() != [tilo_agent_control_protocol::PRIMARY_RECORD_ALIAS]
                || scope
                    .classes
                    .iter()
                    .any(|class| *class != tilo_agent_control_protocol::OperationClass::AppOpen)
            {
                return Err("desktop grant may only open applications".into());
            }
            continue;
        }
        for record in scope.records() {
            let record = route_record(record).map_err(|e| e.to_string())?;
            if peer.document(&scope.app_id, record).is_none() {
                return Err("agent grant has no exact supported canonical record".into());
            }
        }
    }
    Ok(())
}
