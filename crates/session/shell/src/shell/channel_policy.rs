//! Who may read a room's encrypted channels.
//!
//! Access follows the room's existing permission model and each person's
//! certified device set, so no separate channel membership list exists to
//! drift from it:
//!
//! - every person the room's roster lists with a role that can read
//!   (owner, editor, commenter or viewer) is allowed; a restriction below
//!   read, an expired grant or a removal takes the person out;
//! - each allowed person reads through every certified device of theirs:
//!   the devices the room's governor admitted (root-signed certificate,
//!   unexpired, not revoked), plus, for this machine's own identity, the
//!   devices of its own fleet;
//! - always-on hosts (`can_act_as_host`) are not a person's reading device.
//!   A cloud or relay service reads only as an explicit service member
//!   ([`super::service_members`]): its own device key, admitted with a
//!   purpose, a scope and optionally a deadline, and removed from every
//!   channel when the grant ends.

use std::collections::BTreeSet;

use collab_types::Ed25519PublicKey;

use super::channel_encryption::ChannelDirectory;
use super::state::{CapRole, RoomAuthority, RoomId, WorkspaceTree, WorkspaceTreeHandle};

/// The devices of this machine's own identity in a room, as its fleet
/// certifies them: `(certificate, revoked)`.
pub type OwnDevices = Box<dyn Fn(RoomId) -> Vec<(tilo_user_identity::DeviceCert, bool)> + Send>;

/// [`ChannelDirectory`] over the live workspace tree.
pub struct WorkspaceChannelDirectory {
    tree: WorkspaceTreeHandle,
    own_devices: OwnDevices,
}

impl WorkspaceChannelDirectory {
    pub fn new(tree: WorkspaceTreeHandle, own_devices: OwnDevices) -> Self {
        Self { tree, own_devices }
    }

    /// Production source of this machine's own devices: the fleet of the
    /// identity selected for the room.
    pub fn session_fleet() -> OwnDevices {
        Box::new(|room: RoomId| {
            let Ok(Some(profile)) =
                crate::identity_registry::selected_identity_profile(&room.to_string())
            else {
                return Vec::new();
            };
            let Ok(fleet) = crate::device_fleet::DeviceFleetStore::from_env()
                .and_then(|store| store.load(&profile.id))
            else {
                return Vec::new();
            };
            fleet
                .certificates
                .iter()
                .map(|cert| (cert.clone(), fleet.is_revoked(&cert.device_public_key)))
                .collect()
        })
    }
}

impl ChannelDirectory for WorkspaceChannelDirectory {
    fn governs(&self, room: RoomId) -> bool {
        let tree = self.tree.read();
        tree.rooms().contains(&room) && tree.governs_room(room)
    }

    fn steward_of(&self, room: RoomId) -> Option<Ed25519PublicKey> {
        let RoomAuthority::Peer(origin) = self.tree.read().room_authority(room) else {
            return None;
        };
        let endpoint = origin.as_str().strip_prefix("iroh:")?;
        let endpoint = endpoint.parse::<iroh::EndpointId>().ok()?;
        Some(Ed25519PublicKey::from_bytes(*endpoint.as_bytes()))
    }

    fn allowed_devices(&self, room: RoomId, now: u64) -> BTreeSet<Ed25519PublicKey> {
        let own = (self.own_devices)(room);
        allowed_devices(&self.tree.read(), room, now, &own)
    }
}

/// Every device that may read `room`'s channels at `now` (Unix seconds).
pub fn allowed_devices(
    tree: &WorkspaceTree,
    room: RoomId,
    now: u64,
    own_devices: &[(tilo_user_identity::DeviceCert, bool)],
) -> BTreeSet<Ed25519PublicKey> {
    let Some(workspace) = tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room == room)
    else {
        return BTreeSet::new();
    };
    let readers: BTreeSet<String> = workspace
        .roster_at(now)
        .into_iter()
        .filter(|(_, role)| role_reads(*role))
        .map(|(person, _)| person)
        .collect();
    let now_ns = now.saturating_mul(1_000_000_000);
    let reads = |cert: &tilo_user_identity::DeviceCert| {
        readers.contains(&hex::encode(cert.user_public_key))
            && !cert.capabilities.can_act_as_host
            && !cert.is_expired_at(now_ns)
            && tilo_user_identity::verify_device_cert(cert).is_ok()
    };
    let mut allowed = BTreeSet::new();
    if let Some(identities) = tree.room_peer_identities.get(&room) {
        for identity in identities.values() {
            let cert = &identity.device_cert;
            let revoked = identity
                .revocations
                .iter()
                .any(|entry| entry.revoked_device_public_key == cert.device_public_key);
            if !revoked && reads(cert) {
                allowed.insert(Ed25519PublicKey::from_bytes(cert.device_public_key));
            }
        }
    }
    for (cert, revoked) in own_devices {
        if !revoked && reads(cert) {
            allowed.insert(Ed25519PublicKey::from_bytes(cert.device_public_key));
        }
    }
    for member in super::service_members::service_members(workspace, now) {
        if member.verb.implies(collab_types::caps::Verb::Read) {
            allowed.insert(member.device);
        }
    }
    allowed
}

fn role_reads(role: CapRole) -> bool {
    matches!(
        role,
        CapRole::Owner | CapRole::Editor | CapRole::Commenter | CapRole::Viewer
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::GovernedPeerIdentity;
    use collab_types::caps::{CapOverride, Principal, Subject, Verb};

    fn cert(
        root: &ed25519_dalek::SigningKey,
        device: u8,
        host: bool,
    ) -> tilo_user_identity::DeviceCert {
        let mut capabilities = tilo_user_identity::DeviceCapabilities::member();
        capabilities.can_act_as_host = host;
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device_key(device).0,
            device_label: format!("device {device}"),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    fn device_key(seed: u8) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(
            ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
                .verifying_key()
                .to_bytes(),
        )
    }

    fn person(root: &ed25519_dalek::SigningKey) -> String {
        hex::encode(root.verifying_key().to_bytes())
    }

    fn member(root: &ed25519_dalek::SigningKey) -> Subject {
        Subject::Principal(Principal::Person(person(root)))
    }

    fn endpoint(cert: &tilo_user_identity::DeviceCert) -> String {
        iroh::EndpointId::from_bytes(&cert.device_public_key)
            .unwrap()
            .to_string()
    }

    fn admit(
        tree: &mut WorkspaceTree,
        room: RoomId,
        cert: tilo_user_identity::DeviceCert,
        revocations: Vec<tilo_user_identity::RevocationEntry>,
    ) {
        tree.room_peer_identities.entry(room).or_default().insert(
            endpoint(&cert),
            GovernedPeerIdentity {
                device_cert: cert,
                revocations,
            },
        );
    }

    #[test]
    fn readers_are_room_members_through_their_certified_devices() {
        let owner = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let bob = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        let carol = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let mut tree = WorkspaceTree::default();
        let workspace = tree.append_empty_workspace("Shared");
        let room = tree.room_of(workspace).unwrap();
        let entry = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|candidate| candidate.id == workspace)
            .unwrap();
        entry.set_owner(person(&owner));
        entry.grant_capability(CapOverride::membership(
            member(&bob),
            room.to_string(),
            Verb::Write,
        ));
        // Carol's grant runs out at 500.
        entry.grant_capability(
            CapOverride::membership(member(&carol), room.to_string(), Verb::Read).with_expiry(500),
        );
        admit(&mut tree, room, cert(&bob, 20, false), Vec::new());
        admit(&mut tree, room, cert(&bob, 21, false), Vec::new());
        // Bob's always-on host is not one of his reading devices.
        admit(&mut tree, room, cert(&bob, 22, true), Vec::new());
        admit(&mut tree, room, cert(&carol, 30, false), Vec::new());
        // A revoked device of Bob's.
        let lost = cert(&bob, 23, false);
        let mut revocation = tilo_user_identity::RevocationEntry {
            user_public_key: lost.user_public_key,
            revoked_device_public_key: lost.device_public_key,
            revoked_at_ns: 2,
            reason: tilo_user_identity::RevocationReason::Lost,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut revocation, &bob).unwrap();
        admit(&mut tree, room, lost.clone(), vec![revocation]);
        // A certificate whose signature does not cover its device key.
        let mut forged = lost;
        forged.device_public_key = device_key(24).0;
        admit(&mut tree, room, forged, Vec::new());

        let own = vec![
            (cert(&owner, 10, false), false),
            (cert(&owner, 11, false), true),
        ];
        assert_eq!(
            allowed_devices(&tree, room, 1_000, &own),
            BTreeSet::from([device_key(10), device_key(20), device_key(21)])
        );
        assert!(allowed_devices(&tree, room, 100, &own).contains(&device_key(30)));

        // Bob is removed from the room: his devices go with him.
        tree.desktop
            .workspaces
            .iter_mut()
            .find(|candidate| candidate.id == workspace)
            .unwrap()
            .revoke_matching(|cap| cap.subject == member(&bob));
        assert_eq!(
            allowed_devices(&tree, room, 1_000, &own),
            BTreeSet::from([device_key(10)])
        );
        assert!(allowed_devices(&tree, RoomId::new(), 1_000, &own).is_empty());
    }
}
