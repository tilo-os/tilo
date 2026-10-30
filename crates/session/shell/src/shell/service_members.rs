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
        let workspace = tree.create_hosted_workspace("Shared").0;
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
        tree.govern_requests_here_for_test(&owner);
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
        tree.govern_requests_here_for_test(&owner);
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

    // The workspace's policy, re-checked on the owner's own (simulated)
    // interval: the process-wide announcements and expiry timers run on
    // the real clock, which the pumps below outrun, so they are not what
    // decides when the grant ends here.
    struct Polled(crate::shell::channel_policy::WorkspaceChannelDirectory);
    impl crate::shell::channel_encryption::ChannelDirectory for Polled {
        fn governor_of(
            &self,
            room: crate::shell::state::RoomId,
        ) -> Option<collab_types::Ed25519PublicKey> {
            self.0.governor_of(room)
        }
        fn allowed_devices(
            &self,
            room: crate::shell::state::RoomId,
            channel: collab_types::ChannelId,
            route: Option<&collab_types::ChannelRoute>,
            now: u64,
        ) -> Option<std::collections::BTreeSet<collab_types::Ed25519PublicKey>> {
            self.0.allowed_devices(room, channel, route, now)
        }
    }

    /// A cloud agent run is granted temporary membership, joins, reads the
    /// channel's history and new content, then its grant expires: the
    /// owner removes it, the keys rotate, and what is written afterwards
    /// is closed to it.
    #[allow(
        clippy::too_many_lines,
        reason = "one end-to-end scenario: grant, join, read, expire, rotate"
    )]
    #[test]
    fn a_temporary_service_member_reads_until_its_grant_expires() {
        use crate::shell::channel_encryption::testing::{Endpoint, Room, key, public};
        use crate::shell::channel_encryption::{
            ChannelAccess, ChannelEncryption, ChannelEncryptionConfig,
        };
        use crate::shell::channel_policy::WorkspaceChannelDirectory;
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};
        use tilo_action_broker::collab_core::BrokerChannels;

        const OWNER: u8 = 40;
        const SERVICE: u8 = 41;
        let owner_id = hex::encode(public(OWNER).as_bytes());
        let mut tree = WorkspaceTree::default();
        let workspace = tree.create_hosted_workspace("Brief").0;
        let room = tree.room_of(workspace).unwrap();
        tree.desktop
            .workspaces
            .iter_mut()
            .find(|ws| ws.id == workspace)
            .unwrap()
            .set_owner(owner_id.clone());
        let tree = tree.shared();
        // Each device names the record it asks for: a channel nobody named
        // admits no one.
        let route = collab_types::ChannelRoute::new("so.tilo.writer", "brief");
        let artifact = collab_types::ArtifactId::new();

        let transport = Arc::new(Room::default());
        let (owner_dir, service_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
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
        let mut owner = open(
            OWNER,
            owner_dir.path(),
            Box::new(Polled(
                WorkspaceChannelDirectory::new(tree.clone(), Box::new(|_| Vec::new()))
                    .with_governors(Box::new(|_| {
                        Some((*public(OWNER).as_bytes(), *public(OWNER).as_bytes()))
                    })),
            )),
        );
        // The service reads the same (replicated) workspace tree: every
        // participant refuses a commit its own view of the policy does not
        // back, so it accepts its admission only once it sees its grant.
        let mut service = open(
            SERVICE,
            service_dir.path(),
            Box::new(Polled(
                WorkspaceChannelDirectory::new(tree.clone(), Box::new(|_| Vec::new()))
                    .with_governors(Box::new(|_| {
                        Some((*public(SERVICE).as_bytes(), *public(OWNER).as_bytes()))
                    })),
            )),
        );
        let (owner_end, service_end) = (
            Endpoint::join(&transport, room, 0),
            Endpoint::join(&transport, room, 1),
        );
        // One simulated clock drives every call, so how long the host takes
        // to create groups and seal content cannot outrun the pumps.
        let mut now = Instant::now();
        let run = |owner: &mut ChannelEncryption,
                   service: &mut ChannelEncryption,
                   now: &mut Instant,
                   wall: u64,
                   seconds: u64| {
            for _ in 0..seconds.saturating_mul(4) {
                *now = now.checked_add(Duration::from_millis(250)).unwrap();
                owner.pump(&owner_end, *now, wall);
                service.pump(&service_end, *now, wall);
            }
        };
        // No one has made the channel's group yet: the owner's device, its
        // only participant so far, waits its own short while for a genesis
        // and then creates the group itself.
        assert!(matches!(
            owner.access_record(room, &route, now).1,
            ChannelAccess::Waiting
        ));
        run(&mut owner, &mut service, &mut now, 1_000, 5);
        let ChannelAccess::Ready(keys) = owner.access_record(room, &route, now).1 else {
            panic!("the owner's device opens its own channel");
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
            service.access_record(room, &route, now).1,
            ChannelAccess::Waiting
        ));
        run(&mut owner, &mut service, &mut now, 1_000, 5);
        assert!(matches!(
            service.access_record(room, &route, now).1,
            ChannelAccess::Waiting
        ));

        {
            let mut tree = tree.write();
            grant_service_member(
                &mut tree,
                workspace,
                &owner_id,
                &public(SERVICE),
                "Cloud agent: summarise the brief",
                KindSet::READ_UNDOABLE,
                Some(2_000),
            )
            .unwrap();
            tree.govern_requests_here_for_test(&owner_id);
        }
        run(&mut owner, &mut service, &mut now, 1_000, 5);
        let ChannelAccess::Ready(service_keys) = service.access_record(room, &route, now).1 else {
            panic!("a granted service member is admitted");
        };
        assert!(
            service_keys
                .open_content(artifact, &before.0, &before.1)
                .is_ok(),
            "it reads the channel's history, like any joiner"
        );
        let ChannelAccess::Ready(keys) = owner.access_record(room, &route, now).1 else {
            panic!("owner keys");
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

        // The grant's deadline passes: the owner removes the service.
        run(&mut owner, &mut service, &mut now, 2_000, 5);
        assert!(matches!(
            service.access_record(room, &route, now).1,
            ChannelAccess::Removed
        ));
        let ChannelAccess::Ready(keys) = owner.access_record(room, &route, now).1 else {
            panic!("owner keys");
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
