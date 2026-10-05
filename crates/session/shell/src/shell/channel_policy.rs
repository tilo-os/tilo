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

/// The certificates of a user's devices the session authenticated.
pub type AuthenticatedDevices =
    Box<dyn Fn(&[u8; 32]) -> Vec<tilo_user_identity::DeviceCert> + Send>;

/// [`ChannelDirectory`] over the live workspace tree.
pub struct WorkspaceChannelDirectory {
    tree: WorkspaceTreeHandle,
    own_devices: OwnDevices,
    /// This device's key.
    device: [u8; 32],
}

impl WorkspaceChannelDirectory {
    /// The directory for this device, `device`.
    pub fn new(tree: WorkspaceTreeHandle, own_devices: OwnDevices, device: [u8; 32]) -> Self {
        Self {
            tree,
            own_devices,
            device,
        }
    }

    /// Production source of this machine's own devices: the fleet of the
    /// identity selected for the room.
    pub fn session_fleet() -> OwnDevices {
        Self::session_fleet_with(Box::new(|_| Vec::new()))
    }

    /// [`Self::session_fleet`] plus the identity's devices the session
    /// authenticated: `authenticated(user)` returns the root-signed
    /// certificates of `user`'s devices a transport or workspace host
    /// verified (a browser the user certified through the host is in no
    /// local fleet file). The root signature is what makes a device the
    /// user's; the fleet's revocations still take any of them out.
    pub fn session_fleet_with(authenticated: AuthenticatedDevices) -> OwnDevices {
        Box::new(move |room: RoomId| {
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
            fleet_devices(&fleet, &authenticated)
        })
    }
}

/// A fleet's devices plus the same user's devices the session authenticated,
/// each with whether the fleet revoked it.
pub fn fleet_devices(
    fleet: &crate::device_fleet::DeviceFleet,
    authenticated: &dyn Fn(&[u8; 32]) -> Vec<tilo_user_identity::DeviceCert>,
) -> Vec<(tilo_user_identity::DeviceCert, bool)> {
    let mut devices: Vec<_> = fleet
        .certificates
        .iter()
        .map(|cert| (cert.clone(), fleet.is_revoked(&cert.device_public_key)))
        .collect();
    let users: BTreeSet<[u8; 32]> = fleet
        .certificates
        .iter()
        .map(|cert| cert.user_public_key)
        .collect();
    for user in users {
        for cert in authenticated(&user) {
            if cert.user_public_key == user
                && !devices
                    .iter()
                    .any(|(known, _)| known.device_public_key == cert.device_public_key)
            {
                let revoked = fleet.is_revoked(&cert.device_public_key);
                devices.push((cert, revoked));
            }
        }
    }
    devices
}

impl WorkspaceChannelDirectory {
    /// The device that stewards the channels of `room`, a room this
    /// identity governs: the device the person's workspace list names,
    /// while it may steward; before the list names one, the device that
    /// minted the room. When the named steward no longer may (revoked,
    /// expired, or a host), its successor: the lowest-keyed device of the
    /// fleet that may, the same on every device that holds the same fleet.
    /// `None` while this device cannot know yet.
    fn steward(&self, room: RoomId) -> Option<[u8; 32]> {
        let tree = self.tree.read();
        if !tree.rooms().contains(&room) || !tree.governs_room(room) {
            return None;
        }
        let named = tree.channel_stewards.get(&room).copied();
        let minted_here = tree.room_authority(room) == RoomAuthority::Local;
        let person = tree.local_person;
        drop(tree);
        // A session without an identity has no other devices: the room it
        // minted is its own to steward.
        let Some(person) = person else {
            return minted_here.then_some(self.device);
        };
        let now_ns = tilo_clock::unix_now_ns();
        let fleet = (self.own_devices)(room);
        let may_steward = |device: &[u8; 32]| {
            let listed = fleet
                .iter()
                .find(|(cert, _)| cert.device_public_key == *device);
            match listed {
                Some((cert, revoked)) => !revoked && stewardable(cert, &person, now_ns),
                // This device runs under its own certificate even where
                // the fleet does not list it yet.
                None => *device == self.device,
            }
        };
        let known = |device: &[u8; 32]| {
            *device == self.device
                || fleet
                    .iter()
                    .any(|(cert, _)| cert.device_public_key == *device)
        };
        match named {
            Some(named) if may_steward(&named) => Some(named),
            // A steward this fleet does not know yet may still be valid:
            // wait for its certificate rather than found a second group.
            Some(named) if !known(&named) => None,
            Some(_) => fleet
                .iter()
                .map(|(cert, _)| cert.device_public_key)
                .chain(std::iter::once(self.device))
                .filter(|device| may_steward(device))
                .min(),
            None => minted_here.then_some(self.device),
        }
    }
}

/// Whether `cert` may steward the channels of `person`'s rooms: a current,
/// root-signed device of that person that owns workspaces and is no host.
fn stewardable(cert: &tilo_user_identity::DeviceCert, person: &[u8; 32], now_ns: u64) -> bool {
    cert.user_public_key == *person
        && cert.capabilities.can_own_workspaces
        && !cert.capabilities.can_act_as_host
        && !cert.is_expired_at(now_ns)
        && tilo_user_identity::verify_device_cert(cert).is_ok()
}

impl ChannelDirectory for WorkspaceChannelDirectory {
    fn governs(&self, room: RoomId) -> bool {
        self.steward(room) == Some(self.device)
    }

    /// Someone else's room: its pinned governor. A room this identity
    /// governs: [`Self::steward`], when that is another device.
    fn steward_of(&self, room: RoomId) -> Option<Ed25519PublicKey> {
        let authority = self.tree.read().room_authority(room);
        if let RoomAuthority::Peer(origin) = authority {
            let endpoint = origin.as_str().strip_prefix("iroh:")?;
            let endpoint = endpoint.parse::<iroh::EndpointId>().ok()?;
            return Some(Ed25519PublicKey::from_bytes(*endpoint.as_bytes()));
        }
        self.steward(room)
            .filter(|steward| *steward != self.device)
            .map(Ed25519PublicKey::from_bytes)
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
    // A device this identity's fleet revoked reads nothing, however the
    // room's governance still lists it.
    let fleet_revoked: BTreeSet<[u8; 32]> = own_devices
        .iter()
        .filter(|(_, revoked)| *revoked)
        .map(|(cert, _)| cert.device_public_key)
        .collect();
    let mut allowed = BTreeSet::new();
    if let Some(identities) = tree.room_peer_identities.get(&room) {
        for identity in identities.values() {
            let cert = &identity.device_cert;
            let revoked = fleet_revoked.contains(&cert.device_public_key)
                || identity
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

    /// A device of this machine's own identity that the host authenticated
    /// (a browser the user certified through the host) reads the room's
    /// channels like a device in the local fleet file; another user's
    /// device the session authenticated does not become one of ours.
    #[test]
    fn authenticated_devices_of_the_fleets_user_join_its_devices() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let stranger = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        let mut fleet = crate::device_fleet::DeviceFleet::new("identity".into());
        fleet.certificates.push(cert(&root, 1, false));
        let browser = cert(&root, 2, false);
        let foreign = cert(&stranger, 3, false);
        let authenticated = {
            let (browser, foreign) = (browser.clone(), foreign.clone());
            move |user: &[u8; 32]| {
                [browser.clone(), foreign.clone()]
                    .into_iter()
                    .filter(|cert| cert.user_public_key == *user)
                    .collect()
            }
        };
        let devices = fleet_devices(&fleet, &authenticated);
        let keys: Vec<_> = devices
            .iter()
            .map(|(cert, revoked)| (cert.device_public_key, *revoked))
            .collect();
        assert_eq!(
            keys,
            vec![(device_key(1).0, false), (browser.device_public_key, false)]
        );
        assert_eq!(fleet_devices(&fleet, &|_| vec![foreign.clone()]).len(), 1);
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

    /// In a room the person owns, the device the person's list names
    /// stewards its channels while it may; a host named there never does,
    /// and once the named steward is revoked, the lowest-keyed device of
    /// the fleet that may takes over on every device. Before the list names
    /// one, only the device that minted the room stewards.
    #[test]
    fn one_device_of_the_person_stewards_a_room_they_own() {
        let person_root = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
        let person = person_root.verifying_key().to_bytes();
        let primary = cert(&person_root, 1, false);
        let board = cert(&person_root, 2, false);
        let host = cert(&person_root, 3, true);
        let mut tree = WorkspaceTree::default();
        let workspace = tree.append_empty_workspace("Home");
        let room = tree.room_of(workspace).unwrap();
        tree.local_person = Some(person);
        tree.room_authority
            .insert(room, RoomAuthority::Person(person));
        let tree = tree.shared();
        let revoked = std::sync::Arc::new(std::sync::Mutex::new(Vec::<[u8; 32]>::new()));
        let directory = |device: &tilo_user_identity::DeviceCert| {
            let fleet = [primary.clone(), board.clone(), host.clone()];
            let revoked = std::sync::Arc::clone(&revoked);
            WorkspaceChannelDirectory::new(
                tree.clone(),
                Box::new(move |_| {
                    let revoked = revoked.lock().unwrap();
                    fleet
                        .iter()
                        .map(|cert| (cert.clone(), revoked.contains(&cert.device_public_key)))
                        .collect()
                }),
                device.device_public_key,
            )
        };
        let (on_primary, on_board) = (directory(&primary), directory(&board));
        let key = |cert: &tilo_user_identity::DeviceCert| {
            Some(Ed25519PublicKey::from_bytes(cert.device_public_key))
        };

        // Nobody named yet, and neither minted the room here.
        assert!(!on_primary.governs(room) && !on_board.governs(room));
        assert_eq!(on_board.steward_of(room), None);

        tree.write()
            .channel_stewards
            .insert(room, primary.device_public_key);
        assert!(on_primary.governs(room));
        assert!(!on_board.governs(room));
        assert_eq!(on_board.steward_of(room), key(&primary));

        tree.write()
            .channel_stewards
            .insert(room, host.device_public_key);
        let successor = primary.device_public_key.min(board.device_public_key);
        assert_eq!(
            (on_primary.governs(room), on_board.governs(room)),
            (
                successor == primary.device_public_key,
                successor == board.device_public_key
            ),
            "a host never stewards; the fleet's successor does, the same on every device"
        );

        tree.write()
            .channel_stewards
            .insert(room, primary.device_public_key);
        revoked.lock().unwrap().push(primary.device_public_key);
        assert!(on_board.governs(room), "the revoked steward's successor");
        assert!(!on_primary.governs(room));
        assert_eq!(on_primary.steward_of(room), key(&board));

        // A steward this fleet does not know yet is waited for.
        tree.write().channel_stewards.insert(room, [9; 32]);
        assert!(!on_board.governs(room));
        assert_eq!(on_board.steward_of(room), None);
    }
}
