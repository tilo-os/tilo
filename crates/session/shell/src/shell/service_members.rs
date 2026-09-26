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

use super::state::{Workspace, WorkspaceId, WorkspaceTree};

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

/// Grant `device` membership of `workspace`'s channels as `grantor`, a room
/// administrator, replicated to peers and persisted like any other grant.
/// Returns the grant id to revoke it by when the job ends.
pub fn grant_service_member(
    tree: &mut WorkspaceTree,
    workspace: WorkspaceId,
    grantor: &str,
    device: &Ed25519PublicKey,
    purpose: &str,
    verb: Verb,
    expires_at: Option<u64>,
) -> Option<String> {
    let room = tree.room_of(workspace)?;
    let cap = service_membership(&room.to_string(), device, purpose, verb, expires_at)
        .granted_by(Principal::Person(grantor.to_string()));
    tree.grant_in_workspace(workspace, &grantor.to_string(), cap)
}

/// End a service member's access: every grant its principal holds in the
/// workspace is revoked. Returns how many were.
pub fn revoke_service_member(
    tree: &mut WorkspaceTree,
    workspace: WorkspaceId,
    revoker: &str,
    device: &Ed25519PublicKey,
) -> usize {
    let principal = service_principal(device);
    let ids: Vec<String> = tree
        .desktop
        .workspaces
        .iter()
        .find(|candidate| candidate.id == workspace)
        .map(|ws| {
            ws.capabilities
                .iter()
                .filter(|cap| {
                    matches!(&cap.subject, Subject::Principal(Principal::Agent(id)) if *id == principal)
                })
                .map(|cap| cap.id.clone())
                .collect()
        })
        .unwrap_or_default();
    ids.iter()
        .filter(|id| tree.revoke_in_workspace(workspace, &revoker.to_string(), id))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(seed: u8) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(
            ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
                .verifying_key()
                .to_bytes(),
        )
    }

    #[test]
    fn a_grant_lists_its_purpose_scope_and_deadline_until_it_ends() {
        let mut tree = WorkspaceTree::default();
        let workspace = tree.append_empty_workspace("Shared");
        let owner = "owner".to_string();
        tree.desktop
            .workspaces
            .iter_mut()
            .find(|ws| ws.id == workspace)
            .unwrap()
            .set_owner(owner.clone());
        let agent_run = device(9);
        let grant = grant_service_member(
            &mut tree,
            workspace,
            &owner,
            &agent_run,
            "Cloud agent: summarise the brief",
            Verb::Write,
            Some(2_000),
        )
        .expect("an administrator grants");
        let ws = tree
            .desktop
            .workspaces
            .iter()
            .find(|ws| ws.id == workspace)
            .unwrap();
        let members = service_members(ws, 1_000);
        assert_eq!(
            members,
            vec![ServiceMember {
                device: agent_run,
                purpose: "Cloud agent: summarise the brief".into(),
                verb: Verb::Write,
                expires_at: Some(2_000),
                grant_id: grant,
            }]
        );
        assert!(
            ws.agent_members_at(1_000)
                .iter()
                .any(|(agent, _)| *agent == service_principal(&agent_run)),
            "the service member is listed with the room's agents"
        );
        assert!(
            service_members(ws, 2_000).is_empty(),
            "expired at its deadline"
        );

        // A non-administrator cannot grant one.
        assert!(
            grant_service_member(
                &mut tree,
                workspace,
                "stranger",
                &device(10),
                "search",
                Verb::Read,
                None
            )
            .is_none()
        );
        assert_eq!(
            revoke_service_member(&mut tree, workspace, &owner, &agent_run),
            1
        );
        let ws = tree
            .desktop
            .workspaces
            .iter()
            .find(|ws| ws.id == workspace)
            .unwrap();
        assert!(service_members(ws, 1_000).is_empty());
        assert_eq!(service_device("service:zz"), None);
        assert_eq!(
            service_device(&service_principal(&agent_run)),
            Some(agent_run)
        );
    }

    /// A cloud agent run is granted temporary membership, joins, reads the
    /// channel's history and new content, then its grant expires: the
    /// steward removes it, the keys rotate, and what is written afterwards
    /// is closed to it.
    #[allow(
        clippy::too_many_lines,
        reason = "one end-to-end scenario: grant, join, read, expire, rotate"
    )]
    #[test]
    fn a_temporary_service_member_reads_until_its_grant_expires() {
        use crate::shell::channel_encryption::tests::{
            Directory, Endpoint, Policy, Room, key, public,
        };
        use crate::shell::channel_encryption::{
            ChannelAccess, ChannelEncryption, ChannelEncryptionConfig,
        };
        use crate::shell::channel_policy::WorkspaceChannelDirectory;
        use crate::shell::collab_core::BrokerChannels;
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};

        const OWNER: u8 = 40;
        const SERVICE: u8 = 41;
        let owner = hex::encode(public(OWNER).as_bytes());
        let mut tree = WorkspaceTree::default();
        let workspace = tree.append_empty_workspace("Brief");
        let room = tree.room_of(workspace).unwrap();
        tree.desktop
            .workspaces
            .iter_mut()
            .find(|ws| ws.id == workspace)
            .unwrap()
            .set_owner(owner.clone());
        let tree = tree.shared();
        let channel =
            collab_types::document_channel_id("so.tilo.writer", &room.to_string(), "brief");
        let artifact = collab_types::ArtifactId::new();

        let transport = Arc::new(Room::default());
        let (steward_dir, service_dir) =
            (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let open =
            |seed: u8,
             dir: &std::path::Path,
             directory: Box<dyn crate::shell::channel_encryption::ChannelDirectory>| {
                ChannelEncryption::open(
                    ChannelEncryptionConfig {
                        root: dir.join("channels"),
                        vault: channel_crypto::VaultKey::new([seed; 32]),
                        device_key: key(seed),
                    },
                    directory,
                    Arc::new(Mutex::new(BrokerChannels::default())),
                )
                .unwrap()
            };
        let mut steward = open(
            OWNER,
            steward_dir.path(),
            Box::new(WorkspaceChannelDirectory::new(
                tree.clone(),
                Box::new(|_| Vec::new()),
            )),
        );
        let mut service = open(
            SERVICE,
            service_dir.path(),
            Box::new(Directory {
                governs: false,
                steward: public(OWNER),
                policy: Policy::default(),
            }),
        );
        let (steward_end, service_end) = (
            Endpoint::join(&transport, room, 0),
            Endpoint::join(&transport, room, 1),
        );
        let mut now = Instant::now();
        let mut run = |steward: &mut ChannelEncryption,
                       service: &mut ChannelEncryption,
                       wall: u64,
                       seconds: u64| {
            for _ in 0..seconds.saturating_mul(4) {
                now = now.checked_add(Duration::from_millis(250)).unwrap();
                steward.pump(&steward_end, now, wall);
                service.pump(&service_end, now, wall);
            }
        };
        let ChannelAccess::Ready(keys) = steward.access(room, channel, Instant::now()) else {
            panic!("the steward's device opens its own channel");
        };
        let before = keys
            .seal_content(
                artifact,
                collab_types::ContentRetention::Delta,
                b"written before the job",
            )
            .unwrap();

        // Without a grant the service only waits.
        assert!(matches!(
            service.access(room, channel, Instant::now()),
            ChannelAccess::Waiting
        ));
        run(&mut steward, &mut service, 1_000, 5);
        assert!(matches!(
            service.access(room, channel, Instant::now()),
            ChannelAccess::Waiting
        ));

        grant_service_member(
            &mut tree.write(),
            workspace,
            &owner,
            &public(SERVICE),
            "Cloud agent: summarise the brief",
            Verb::Write,
            Some(2_000),
        )
        .unwrap();
        run(&mut steward, &mut service, 1_000, 5);
        let ChannelAccess::Ready(service_keys) = service.access(room, channel, Instant::now())
        else {
            panic!("a granted service member is admitted");
        };
        assert!(
            service_keys
                .open_content(artifact, &before.0, &before.1)
                .is_ok(),
            "it reads the channel's history, like any joiner"
        );
        let ChannelAccess::Ready(keys) = steward.access(room, channel, Instant::now()) else {
            panic!("steward keys");
        };
        let during = keys
            .seal_content(
                artifact,
                collab_types::ContentRetention::Delta,
                b"during the job",
            )
            .unwrap();
        assert!(
            service_keys
                .open_content(artifact, &during.0, &during.1)
                .is_ok()
        );

        // The grant's deadline passes: the steward removes the service.
        run(&mut steward, &mut service, 2_000, 5);
        assert!(matches!(
            service.access(room, channel, Instant::now()),
            ChannelAccess::Removed
        ));
        let ChannelAccess::Ready(keys) = steward.access(room, channel, Instant::now()) else {
            panic!("steward keys");
        };
        assert!(
            keys.epoch() > service_keys.epoch(),
            "removal rotated the keys"
        );
        let after = keys
            .seal_content(
                artifact,
                collab_types::ContentRetention::Delta,
                b"after the job",
            )
            .unwrap();
        assert!(
            service_keys
                .open_content(artifact, &after.0, &after.1)
                .is_err(),
            "what is written after expiry is closed to the service"
        );
    }
}
