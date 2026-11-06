//! Service members: cloud and relay identities recorded in a workspace's
//! grants as explicit members, listed with its agents under their purpose.
//!
//! A service member is an agent principal `service:<device key hex>` holding an
//! ordinary grant on the workspace, whose id is `service-member:<device key>:<purpose>`.
//! That is the stored spelling an older build wrote when an administrator
//! admitted one; the Agents tab still reads it, so the grants a workspace
//! already holds keep their purpose, access and deadline. New agents join as
//! bots ([`crate::bots::WorkspaceBot`]), which hold their own membership.

use collab_types::Ed25519PublicKey;
use tilo_action_policy::{Answer, GrantId, KindSet, Lifetime, Subject};

use crate::Workspace;
use crate::workspace::context_at;

/// The principal prefix a service member's grants are held under.
const AGENT_PREFIX: &str = "service:";
/// The grant id prefix that carries a service member's purpose.
const ID_PREFIX: &str = "service-member:";

/// One live service member of a workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceMember {
    /// The service's device key: its MLS credential and op author.
    pub device: Ed25519PublicKey,
    /// Why it is a member, as the granting administrator put it.
    pub purpose: String,
    /// The kinds of action it may perform in the workspace.
    pub kinds: KindSet,
    /// Wall-clock deadline (Unix seconds), if the grant expires.
    pub expires_at: Option<u64>,
    pub grant_id: GrantId,
}

/// The device key a service member's principal names, or `None` for any other
/// agent.
pub fn service_device(principal: &str) -> Option<Ed25519PublicKey> {
    let hex_key = principal.strip_prefix(AGENT_PREFIX)?;
    let bytes: [u8; 32] = hex::decode(hex_key).ok()?.try_into().ok()?;
    Some(Ed25519PublicKey::from_bytes(bytes))
}

/// Every live service member of `workspace` at `now` (Unix seconds).
pub fn service_members(workspace: &Workspace, now: u64) -> Vec<ServiceMember> {
    let cx = context_at(now);
    let workspace_node = workspace.node();
    let mut members: Vec<ServiceMember> = Vec::new();
    for grant in &workspace.policy.grants {
        if grant.answer == Answer::Deny || !grant.is_live(&cx) || grant.node != workspace_node {
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
