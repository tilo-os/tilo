//! Admitting and removing service members through this machine's workspace
//! tree; what a service member is lives in
//! [`tilo_desktop_model::service_member`].

use collab_types::Ed25519PublicKey;
use tilo_action_policy::{GrantId, KindSet, Subject};
pub use tilo_desktop_model::service_member::{
    MAX_PURPOSE_CHARS, ServiceMember, service_device, service_members, service_membership,
    service_principal, service_purpose,
};

use super::state::{WorkspaceId, WorkspaceTree};

/// Grant `device` membership of `workspace`'s channels as `grantor`, who
/// must be allowed to grant in the room, replicated to peers and persisted
/// like any other grant. Returns the grant id to revoke it by when the job
/// ends.
pub fn grant_service_member(
    tree: &mut WorkspaceTree,
    workspace: WorkspaceId,
    grantor: &str,
    device: &Ed25519PublicKey,
    purpose: &str,
    kinds: KindSet,
    expires_at: Option<u64>,
) -> Option<GrantId> {
    let room = tree.room_of(workspace)?;
    let grant = service_membership(
        &room.to_string(),
        device,
        purpose,
        kinds,
        expires_at,
        Subject::person(grantor),
    );
    tree.grant_in_workspace(workspace, &grantor.to_string(), grant)
}

/// End a service member's access: every grant its principal holds in the
/// workspace is revoked. Returns how many were.
pub fn revoke_service_member(
    tree: &mut WorkspaceTree,
    workspace: WorkspaceId,
    revoker: &str,
    device: &Ed25519PublicKey,
) -> usize {
    let subject = Subject::agent(service_principal(device));
    let ids: Vec<GrantId> = tree
        .desktop
        .workspaces
        .iter()
        .find(|candidate| candidate.id == workspace)
        .map(|ws| {
            ws.policy
                .grants_of(&subject)
                .map(|grant| grant.id.clone())
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
            KindSet::READ_UNDOABLE,
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
                kinds: KindSet::READ_UNDOABLE,
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
                KindSet::READ,
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

    // The workspace's policy, re-checked on the steward's own (simulated)
    // interval: the process-wide announcements and expiry timers run on
    // the real clock, which the pumps below outrun, so they are not what
    // decides when the grant ends here.
    struct Polled(crate::shell::channel_policy::WorkspaceChannelDirectory);
    impl crate::shell::channel_encryption::ChannelDirectory for Polled {
        fn governs(&self, room: crate::shell::state::RoomId) -> bool {
            self.0.governs(room)
        }
        fn steward_of(
            &self,
            room: crate::shell::state::RoomId,
        ) -> Option<collab_types::Ed25519PublicKey> {
            self.0.steward_of(room)
        }
        fn allowed_devices(
            &self,
            room: crate::shell::state::RoomId,
            now: u64,
        ) -> std::collections::BTreeSet<collab_types::Ed25519PublicKey> {
            self.0.allowed_devices(room, now)
        }
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
        use crate::shell::channel_encryption::testing::{
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
            Box::new(Polled(WorkspaceChannelDirectory::new(
                tree.clone(),
                Box::new(|_| Vec::new()),
            ))),
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
        // One simulated clock drives every call, so how long the host takes
        // to create groups and seal content cannot outrun the pumps.
        let mut now = Instant::now();
        let run = |steward: &mut ChannelEncryption,
                   service: &mut ChannelEncryption,
                   now: &mut Instant,
                   wall: u64,
                   seconds: u64| {
            for _ in 0..seconds.saturating_mul(4) {
                *now = now.checked_add(Duration::from_millis(250)).unwrap();
                steward.pump(&steward_end, *now, wall);
                service.pump(&service_end, *now, wall);
            }
        };
        let ChannelAccess::Ready(keys) = steward.access(room, channel, now) else {
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
            service.access(room, channel, now),
            ChannelAccess::Waiting
        ));
        run(&mut steward, &mut service, &mut now, 1_000, 5);
        assert!(matches!(
            service.access(room, channel, now),
            ChannelAccess::Waiting
        ));

        grant_service_member(
            &mut tree.write(),
            workspace,
            &owner,
            &public(SERVICE),
            "Cloud agent: summarise the brief",
            KindSet::READ_UNDOABLE,
            Some(2_000),
        )
        .unwrap();
        run(&mut steward, &mut service, &mut now, 1_000, 5);
        let ChannelAccess::Ready(service_keys) = service.access(room, channel, now) else {
            panic!("a granted service member is admitted");
        };
        assert!(
            service_keys
                .open_content(artifact, &before.0, &before.1)
                .is_ok(),
            "it reads the channel's history, like any joiner"
        );
        let ChannelAccess::Ready(keys) = steward.access(room, channel, now) else {
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
        run(&mut steward, &mut service, &mut now, 2_000, 5);
        assert!(matches!(
            service.access(room, channel, now),
            ChannelAccess::Removed
        ));
        let ChannelAccess::Ready(keys) = steward.access(room, channel, now) else {
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
