//! Service members: cloud and relay identities admitted to a room's
//! end-to-end encrypted channels as explicit members.
//!
//! A cloud agent run, server-side search or a preview renderer never gets a
//! plaintext side door. It reads a room's content only as a member of the
//! room's channels, like a person's device:
//!
//! - it has its own device key, and its MLS leaf is that key;
//! - an administrator grants it membership with a purpose and a scope (the
//!   verb it may exercise), optionally until a deadline;
//! - it is listed with the room's agents, with its purpose, for as long as
//!   it holds the grant;
//! - when the grant expires or its job ends and the grant is revoked, the
//!   next reconcile removes its device from every channel of the room. The
//!   removal is an MLS commit, so the content keys rotate: the service
//!   keeps what it read before and cannot read anything written after.
//!
//! A grant is an ordinary room membership capability whose subject is the
//! agent principal `service:<device key hex>` and whose id carries the
//! purpose, so it replicates, persists and is revoked with the rest of the
//! room's permissions.

use collab_types::Ed25519PublicKey;
use collab_types::caps::{CapOverride, Effect, Principal, Scope, Subject, Verb};

use crate::Workspace;

const AGENT_PREFIX: &str = "service:";
const ID_PREFIX: &str = "service-member:";
/// Longest purpose a grant records.
pub const MAX_PURPOSE_CHARS: usize = 80;

/// One live service member of a room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceMember {
    /// The service's device key: its MLS credential and op author.
    pub device: Ed25519PublicKey,
    /// Why it is a member, as the granting administrator put it.
    pub purpose: String,
    /// The strongest verb it holds in the room.
    pub verb: Verb,
    /// Wall-clock deadline (Unix seconds), if the grant expires.
    pub expires_at: Option<u64>,
    /// The capability id, to revoke it by.
    pub grant_id: String,
}

/// The agent principal a service member's grants are held by.
pub fn service_principal(device: &Ed25519PublicKey) -> String {
    format!("{AGENT_PREFIX}{}", hex::encode(device.as_bytes()))
}

/// The device a service principal names, if it is one.
pub fn service_device(principal: &str) -> Option<Ed25519PublicKey> {
    let hex_key = principal.strip_prefix(AGENT_PREFIX)?;
    let bytes: [u8; 32] = hex::decode(hex_key).ok()?.try_into().ok()?;
    Some(Ed25519PublicKey::from_bytes(bytes))
}

/// The room capability that makes `device` a service member.
pub fn service_membership(
    room: &str,
    device: &Ed25519PublicKey,
    purpose: &str,
    verb: Verb,
    expires_at: Option<u64>,
) -> CapOverride {
    let purpose: String = purpose
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_PURPOSE_CHARS)
        .collect();
    let cap = CapOverride::membership(Subject::agent(service_principal(device)), room, verb)
        .with_id(format!(
            "{ID_PREFIX}{}:{}",
            hex::encode(device.as_bytes()),
            purpose.trim()
        ));
    match expires_at {
        Some(deadline) => cap.with_expiry(deadline),
        None => cap,
    }
}

/// Every live service member of `workspace` at `now` (Unix seconds).
pub fn service_members(workspace: &Workspace, now: u64) -> Vec<ServiceMember> {
    let mut members: Vec<ServiceMember> = Vec::new();
    for cap in &workspace.capabilities {
        if cap.effect != Effect::Grant
            || !cap.is_live_at(Some(now))
            || !matches!(cap.scope, Scope::Room(_) | Scope::Workspace)
        {
            continue;
        }
        let Subject::Principal(Principal::Agent(principal)) = &cap.subject else {
            continue;
        };
        let Some(device) = service_device(principal) else {
            continue;
        };
        let purpose = cap
            .id
            .strip_prefix(ID_PREFIX)
            .and_then(|rest| rest.split_once(':'))
            .map(|(_, purpose)| purpose.to_string())
            .unwrap_or_default();
        let expires_at = match cap.expiry {
            collab_types::caps::Expiry::AtUnix(deadline) => Some(deadline),
            _ => None,
        };
        members.push(ServiceMember {
            device,
            purpose,
            verb: cap.verb,
            expires_at,
            grant_id: cap.id.clone(),
        });
    }
    members
}

/// The purpose of the service member an agent-pane row names, if the row
/// is a service member.
pub fn service_purpose(workspace: &Workspace, agent: &str, now: u64) -> Option<ServiceMember> {
    let device = service_device(agent)?;
    service_members(workspace, now)
        .into_iter()
        .find(|member| member.device == device)
}
