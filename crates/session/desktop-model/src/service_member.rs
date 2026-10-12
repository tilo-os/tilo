//! Service members: cloud and relay identities admitted to a room's
//! end-to-end encrypted channels as explicit members.
//!
//! A cloud agent run, server-side search or a preview renderer never gets a
//! plaintext side door. It reads a room's content only as a member of the
//! room's channels, like a person's device:
//!
//! - it has its own device key, and its MLS leaf is that key;
//! - an administrator grants it membership with a purpose and the kinds of
//!   action it may perform, optionally until a deadline;
//! - it is listed with the room's agents, with its purpose, for as long as
//!   it holds the grant;
//! - when the grant expires or its job ends and the grant is revoked, the
//!   next reconcile removes its device from every channel of the room. The
//!   removal is an MLS commit, so the content keys rotate: the service
//!   keeps what it read before and cannot read anything written after.
//!
//! A grant is an ordinary grant on the room whose subject is the agent
//! `service:<device key hex>` and whose id carries the purpose, so it
//! replicates, persists and is revoked with the rest of the room's grants.

use collab_types::Ed25519PublicKey;
use tilo_action_policy::{Answer, Grant, GrantId, KindSet, Lifetime, Subject, TreePath};

use crate::Workspace;
use crate::workspace::context_at;

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
    /// The kinds of action it may perform in the room.
    pub kinds: KindSet,
    /// Wall-clock deadline (Unix seconds), if the grant expires.
    pub expires_at: Option<u64>,
    /// The grant id, to revoke it by.
    pub grant_id: GrantId,
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

/// The room grant that makes `device` a service member, given by
/// `granted_by`.
pub fn service_membership(
    room: &str,
    device: &Ed25519PublicKey,
    purpose: &str,
    kinds: KindSet,
    expires_at: Option<u64>,
    granted_by: Subject,
) -> Grant {
    let purpose: String = purpose
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_PURPOSE_CHARS)
        .collect();
    let grant = Grant::new(
        format!(
            "{ID_PREFIX}{}:{}",
            hex::encode(device.as_bytes()),
            purpose.trim()
        ),
        Subject::agent(service_principal(device)),
        TreePath::room(room),
        kinds,
        Answer::Allow,
        granted_by,
    );
    match expires_at {
        Some(deadline) => grant.with_lifetime(Lifetime::Until(deadline.saturating_mul(1000))),
        None => grant,
    }
}

/// Every live service member of `workspace` at `now` (Unix seconds).
pub fn service_members(workspace: &Workspace, now: u64) -> Vec<ServiceMember> {
    let cx = context_at(now);
    let room = workspace.node();
    let mut members: Vec<ServiceMember> = Vec::new();
    for grant in &workspace.policy.grants {
        if grant.answer == Answer::Deny || !grant.is_live(&cx) || grant.node != room {
            continue;
        }
        let Subject::Agent(principal) = &grant.subject else {
            continue;
        };
        let Some(device) = service_device(principal) else {
            continue;
        };
        let purpose = grant
            .id
            .as_str()
            .strip_prefix(ID_PREFIX)
            .and_then(|rest| rest.split_once(':'))
            .map(|(_, purpose)| purpose.to_string())
            .unwrap_or_default();
        let expires_at = match grant.lifetime {
            Lifetime::Until(deadline) => Some(deadline / 1000),
            _ => None,
        };
        members.push(ServiceMember {
            device,
            purpose,
            kinds: grant.kinds,
            expires_at,
            grant_id: grant.id.clone(),
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
