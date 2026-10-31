//! Who may read a room's encrypted channels.
//!
//! Access follows the room's existing permission model and each person's
//! certified device set, so no separate channel membership list exists to
//! drift from it:
//!
//! - every person the room's roster lists with a role that can read
//!   (owner, editor, commenter or viewer) is allowed; a restriction below
//!   read, an expired grant or a removal takes the person out;
//! - a channel is one record's: a Read Deny on the record, or on its app or
//!   any pattern covering it, takes its person, or service member, out of
//!   that record's channel and no other. A participant names the record from a
//!   grant naming it ([`collab_types::named_record_of_channel`]) or from the
//!   route a device proved with its request ([`collab_types::ChannelRoute`]);
//!   a channel whose record it cannot name may be a member's own record and
//!   is withheld from everyone until a device names it;
//! - each allowed person reads through every certified device of theirs:
//!   the devices the room's governor admitted (root-signed certificate,
//!   unexpired, not revoked), plus, for this machine's own identity, the
//!   devices of its own fleet;
//! - always-on hosts (`can_act_as_host`) are not a person's reading device.
//!   A cloud or relay service reads only as an explicit service member
//!   ([`super::service_members`]): its own device key, admitted with a
//!   purpose, a scope and optionally a deadline, and removed from every
//!   channel when the grant ends.
//! - a person room ([`collab_types::person_room`]) has no roster and no
//!   record grants: its channels are read by the identity it derives from,
//!   through that identity's certified devices that are not revoked. No
//!   governor or workspace takes part, and the identity decides.

use std::collections::BTreeSet;

use collab_types::{ChannelId, ChannelRoute, Ed25519PublicKey};

use super::channel_encryption::ChannelDirectory;
use super::state::{RoomId, WorkspaceTree, WorkspaceTreeHandle};

/// The devices of this machine's own identity in a room, as its fleet
/// certifies them: `(certificate, revoked)`.
pub type OwnDevices = Box<dyn Fn(RoomId) -> Vec<(tilo_user_identity::DeviceCert, bool)> + Send>;

/// The certificates of a user's devices the session authenticated.
pub type AuthenticatedDevices =
    Box<dyn Fn(&[u8; 32]) -> Vec<tilo_user_identity::DeviceCert> + Send>;

/// When the authenticated devices next change by time alone, after a Unix
/// nanosecond instant: a certificate expiring, a revocation dated ahead.
pub type AuthenticatedChanges = Box<dyn Fn(u64) -> Option<u64> + Send>;

/// [`ChannelDirectory`] over the live workspace tree.
///
/// Every input of [`ChannelDirectory::allowed_devices`] announces its writes
/// ([`tilo_identity_host::policy_change`]): the tree as a write guard that
/// changed policy is released, the fleet on save and through the identity directory's watch,
/// the network's authenticated devices on each change. Groups re-check on
/// those announcements and at [`ChannelDirectory::next_policy_change`]
/// rather than every second, while the clock and the watch hold.
pub struct WorkspaceChannelDirectory {
    tree: WorkspaceTreeHandle,
    own_devices: OwnDevices,
    authenticated_changes: AuthenticatedChanges,
    governors: Governors,
    hosted: Option<HostedChannelKeys>,
}

/// A channel's content keys from the room host this device runs, for a room
/// that host takes part in; `None` for any other room. See
/// [`ChannelDirectory::hosted_channel_keys`]. Its answer waits on that host,
/// so the channel service asks it with no lock of its own held
/// ([`ChannelDirectory::hosted_keys`]).
pub type HostedChannelKeys = tilo_action_broker::channel_encryption::HostedKeys;

/// The device keys of this device and of `room`'s governor, as the room's
/// admitted document names them: the governor takes part in every channel
/// of the room, and creates a channel's group when it first opens it. `None`
/// while this device holds no admitted view of the room.
pub type Governors = Box<dyn Fn(RoomId) -> Option<([u8; 32], [u8; 32])> + Send>;

impl WorkspaceChannelDirectory {
    pub fn new(tree: WorkspaceTreeHandle, own_devices: OwnDevices) -> Self {
        Self {
            tree,
            own_devices,
            authenticated_changes: Box::new(|_| None),
            governors: Box::new(|_| None),
            hosted: None,
        }
    }

    /// The directory, taking the keys of the rooms this device's own room
    /// host takes part in from that host, whose leaf in their groups is this
    /// device's own.
    #[must_use]
    pub fn with_hosted_channel_keys(mut self, hosted: HostedChannelKeys) -> Self {
        self.hosted = Some(hosted);
        self
    }

    /// The directory, reading who governs each room from `governors`.
    #[must_use]
    pub fn with_governors(mut self, governors: Governors) -> Self {
        self.governors = governors;
        self
    }

    /// The directory, told when the devices `own_devices` adds from the
    /// session's authentications change by time alone.
    #[must_use]
    pub fn with_authenticated_changes(mut self, changes: AuthenticatedChanges) -> Self {
        self.authenticated_changes = changes;
        self
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

/// The root whose person room `room` is, when it is one of `own_devices`:
/// the room derives from that identity, and the fleet certifies its devices.
fn person_room_root(
    room: RoomId,
    own_devices: &[(tilo_user_identity::DeviceCert, bool)],
) -> Option<[u8; 32]> {
    own_devices
        .iter()
        .map(|(cert, _)| cert.user_public_key)
        .find(|user| collab_types::is_person_room(user, &room.as_uuid()))
}

/// Every device of `root`'s identity that `own_devices` holds, not revoked
/// and not expired: the readers of every channel of that identity's person
/// room. Another person's devices never appear in it, and the identity's
/// own revocations take a device out.
fn person_devices(
    own_devices: &[(tilo_user_identity::DeviceCert, bool)],
    root: [u8; 32],
    now: u64,
) -> BTreeSet<Ed25519PublicKey> {
    let now_ns = now.saturating_mul(1_000_000_000);
    own_devices
        .iter()
        .filter(|(cert, revoked)| {
            !revoked
                && cert.user_public_key == root
                && !cert.is_expired_at(now_ns)
                && tilo_user_identity::verify_device_cert(cert).is_ok()
        })
        .map(|(cert, _)| Ed25519PublicKey::from_bytes(cert.device_public_key))
        .collect()
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

impl ChannelDirectory for WorkspaceChannelDirectory {
    fn hosted_channel_keys(
        &self,
        room: RoomId,
        channel: ChannelId,
        route: Option<&ChannelRoute>,
        create: bool,
    ) -> Option<Result<channel_crypto::ChannelContentKeys, String>> {
        (self.hosted.as_ref()?)(room, channel, route, create)
    }

    fn hosted_keys(&self) -> Option<tilo_action_broker::channel_encryption::HostedKeys> {
        self.hosted.clone()
    }

    fn governor_of(&self, room: RoomId) -> Option<Ed25519PublicKey> {
        (self.governors)(room).map(|(_, governor)| Ed25519PublicKey::from_bytes(governor))
    }

    fn allowed_devices(
        &self,
        room: RoomId,
        channel: ChannelId,
        route: Option<&ChannelRoute>,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        let own = (self.own_devices)(room);
        if let Some(person) = person_room_root(room, &own) {
            // The person room's readers are the identity's devices, whatever
            // channel is asked: the room has no record grants to name one.
            return Some(person_devices(&own, person, now));
        }
        let tree = self.tree.read();
        names_record(&tree, room, channel, route)
            .then(|| allowed_devices(&tree, (room, channel, route), now, &own))
    }

    fn policy_epoch(&self) -> Option<u64> {
        // The fleet is read on every re-check: another process revoking a
        // device in it is seen only through the identity directory's watch.
        (tilo_identity_host::policy_change::timers_reliable()
            && tilo_identity_host::policy_change::identity_files_watched())
        .then(tilo_identity_host::policy_change::epoch)
    }

    fn next_policy_change(&self, room: RoomId, now: u64) -> Option<u64> {
        let own = (self.own_devices)(room);
        if person_room_root(room, &own).is_some() {
            let authenticated = (self.authenticated_changes)(now.saturating_mul(1_000_000_000))
                .map(tilo_action_broker::policy_schedule::second_at_or_after);
            let certificates = own
                .iter()
                .map(|(cert, _)| cert)
                .filter_map(|cert| cert.expires_at_ns)
                .map(tilo_action_broker::policy_schedule::second_at_or_after)
                .filter(|at| *at > now);
            return authenticated.into_iter().chain(certificates).min();
        }
        let authenticated = (self.authenticated_changes)(now.saturating_mul(1_000_000_000))
            .map(tilo_action_broker::policy_schedule::second_at_or_after);
        next_policy_change(&self.tree.read(), room, now, &own)
            .into_iter()
            .chain(authenticated.filter(|at| *at > now))
            .min()
    }
}

/// The first second after `now` at which [`allowed_devices`] for `room` can
/// answer differently with nothing written: a capability of the room
/// expiring (members and service members alike), or a certificate of a
/// device the room admitted or of this identity's fleet expiring.
pub fn next_policy_change(
    tree: &WorkspaceTree,
    room: RoomId,
    now: u64,
    own_devices: &[(tilo_user_identity::DeviceCert, bool)],
) -> Option<u64> {
    let capabilities = tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)?
        .next_grant_expiry_after(now);
    let roster = tree.room_peer_identities(room);
    let admitted = roster.values().map(|identity| &identity.device_cert);
    let certificates = admitted
        .chain(own_devices.iter().map(|(cert, _)| cert))
        .filter_map(|cert| cert.expires_at_ns)
        .map(tilo_action_broker::policy_schedule::second_at_or_after)
        .filter(|at| *at > now);
    capabilities.into_iter().chain(certificates).min()
}

/// A channel of a room, and the route of its record when this device knows
/// it.
pub type ChannelOf<'r> = (RoomId, ChannelId, Option<&'r ChannelRoute>);

/// Every device that may read `channel`, one of `room`'s channels, at `now`
/// (Unix seconds).
///
/// Who reads the room reads a channel when also allowed to read its record,
/// decided by the same `Workspace::answer_at` at the record's
/// [`collab_types::document_node`] that hands this machine's own apps the
/// record's keys. The record is the one a grant names
/// ([`collab_types::named_record_of_channel`]) or `route`, once it proves to
/// be the channel's: then a Deny on the record, its app or any pattern over
/// it withholds the channel. A channel whose record is neither may be any
/// record, a member's own included, so no one reads it until a device names
/// its record.
pub fn allowed_devices(
    tree: &WorkspaceTree,
    (room, channel, route): ChannelOf<'_>,
    now: u64,
    own_devices: &[(tilo_user_identity::DeviceCert, bool)],
) -> BTreeSet<Ed25519PublicKey> {
    let Some(workspace) = tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
    else {
        return BTreeSet::new();
    };
    // Members, and anyone a grant names (a share, a time-limited reader),
    // who may read the room now.
    let mut people: BTreeSet<String> = workspace
        .roster()
        .into_iter()
        .map(|(person, _)| person)
        .collect();
    people.extend(
        workspace
            .policy
            .grants
            .iter()
            .filter_map(|grant| match &grant.subject {
                tilo_action_policy::Subject::Person(person) => Some(person.clone()),
                _ => None,
            }),
    );
    let room_node = workspace.node();
    // A channel whose record nobody named may be anyone's own record
    // (`member:<root>:<kind>`), which its id does not tell: no one reads it
    // until a device names the record.
    let Some(record) = channel_record(workspace, channel, route) else {
        return BTreeSet::new();
    };
    let read = tilo_action_policy::ActionKind::Read;
    let allow = tilo_action_policy::Answer::Allow;
    let readers: BTreeSet<String> = people
        .into_iter()
        .filter(|person| {
            workspace.answer_at(person, &room_node, read, now) == allow
                && workspace.answer_at(person, &record, read, now) == allow
        })
        .collect();
    let now_ns = now.saturating_mul(1_000_000_000);
    let reads = |cert: &tilo_user_identity::DeviceCert| {
        readers.contains(&hex::encode(cert.user_public_key))
            && !cert.capabilities.can_act_as_host
            && !cert.is_expired_at(now_ns)
            && tilo_user_identity::verify_device_cert(cert).is_ok()
    };
    let mut allowed = BTreeSet::new();
    for identity in tree.room_peer_identities(room).values() {
        let cert = &identity.device_cert;
        let revoked = identity
            .revocations
            .iter()
            .any(|entry| entry.revoked_device_public_key == cert.device_public_key);
        if !revoked && reads(cert) {
            allowed.insert(Ed25519PublicKey::from_bytes(cert.device_public_key));
        }
    }
    for (cert, revoked) in own_devices {
        if !revoked && reads(cert) {
            allowed.insert(Ed25519PublicKey::from_bytes(cert.device_public_key));
        }
    }
    for member in super::service_members::service_members(workspace, now) {
        let subject = tilo_action_policy::Subject::agent(
            super::service_members::service_principal(&member.device),
        );
        let authority = workspace.subject_authority(subject);
        let reads_record = authority
            .decide(&record, read, &tilo_desktop_model::context_at(now))
            .answer
            == allow;
        if member.kinds.contains(read) && reads_record {
            allowed.insert(member.device);
        }
    }
    allowed
}

/// Whether a grant names `channel`'s record or `route` proves to be its
/// route: only then can anyone tell who may read it. A room this device does
/// not hold names nothing.
fn names_record(
    tree: &WorkspaceTree,
    room: RoomId,
    channel: ChannelId,
    route: Option<&ChannelRoute>,
) -> bool {
    tree.desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .is_some_and(|workspace| channel_record(workspace, channel, route).is_some())
}

/// The node of `channel`'s record, when a grant names it or `route` proves
/// to be its route.
fn channel_record(
    workspace: &tilo_desktop_model::Workspace,
    channel: ChannelId,
    route: Option<&ChannelRoute>,
) -> Option<tilo_action_policy::TreePath> {
    let room = &workspace.policy.room;
    collab_types::named_record_of_channel(&workspace.policy.grants, room, channel)
        .or_else(|| {
            route
                .filter(|route| route.proves(room, channel))
                .map(|route| (route.app.as_str(), route.record.as_str()))
        })
        .map(|(app, record)| collab_types::document_node(room, app, record))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::GovernedPeerIdentity;
    use tilo_action_policy::{Answer, Grant, KindSet, Lifetime, Role, Subject};

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

    /// The channel of an ordinary record of `room`, and its route.
    fn notes_of(tree: &WorkspaceTree, room: RoomId) -> (ChannelId, ChannelRoute) {
        let workspace = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)
            .unwrap();
        let route = ChannelRoute::new("so.tilo.notes", "plan");
        (route.channel(&workspace.policy.room), route)
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
        tree.room_rosters.entry(room).or_default().insert(
            endpoint(&cert),
            GovernedPeerIdentity {
                device_cert: cert,
                revocations,
                delegate: None,
            },
        );
    }

    #[test]
    fn readers_are_room_members_through_their_certified_devices() {
        let owner = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let bob = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        let carol = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let mut tree = WorkspaceTree::default();
        let workspace = tree.create_hosted_workspace("Shared").0;
        let room = tree.room_of(workspace).unwrap();
        let entry = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|candidate| candidate.id == workspace)
            .unwrap();
        entry.set_owner(person(&owner));
        entry.admit(person(&bob), Role::Editor);
        // Carol may read until 500.
        let node = entry.node();
        entry.grant(
            Grant::new(
                "carol-reads",
                Subject::person(person(&carol)),
                node,
                KindSet::READ,
                Answer::Allow,
                Subject::person(person(&owner)),
            )
            .with_lifetime(Lifetime::Until(500_000)),
        );
        admit(&mut tree, room, cert(&bob, 20, false), Vec::new());
        admit(&mut tree, room, cert(&bob, 21, false), Vec::new());
        // Bob's always-on host is not one of his reading devices.
        admit(&mut tree, room, cert(&bob, 22, true), Vec::new());
        admit(&mut tree, room, cert(&carol, 30, false), Vec::new());
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
        let (notes, route) = notes_of(&tree, room);
        let notes = (room, notes, Some(&route));
        assert_eq!(
            allowed_devices(&tree, notes, 1_000, &own),
            BTreeSet::from([device_key(10), device_key(20), device_key(21)])
        );
        assert!(allowed_devices(&tree, notes, 100, &own).contains(&device_key(30)));

        // Bob is removed from the room: his devices go with him.
        tree.desktop
            .workspaces
            .iter_mut()
            .find(|candidate| candidate.id == workspace)
            .unwrap()
            .dismiss(&person(&bob));
        assert_eq!(
            allowed_devices(&tree, notes, 1_000, &own),
            BTreeSet::from([device_key(10)])
        );
        assert!(allowed_devices(&tree, (RoomId::new(), notes.1, notes.2), 1_000, &own).is_empty());
    }

    /// A Read Deny on one record keeps the denied person's devices, and a
    /// denied service member, out of that record's channel and no other: the
    /// room's other records, and records nobody has written yet, are read as
    /// the room is.
    #[test]
    fn a_read_deny_on_a_record_withholds_that_records_channel_only() {
        let owner = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let bob = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        let carol = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let mut tree = WorkspaceTree::default();
        let workspace = tree.create_hosted_workspace("Shared").0;
        let room = tree.room_of(workspace).unwrap();
        let service = device_key(50);
        let entry = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|candidate| candidate.id == workspace)
            .unwrap();
        entry.set_owner(person(&owner));
        entry.admit(person(&bob), Role::Editor);
        entry.admit(person(&carol), Role::Viewer);
        let room_id = entry.policy.room.clone();
        entry.grant(crate::shell::service_members::service_membership(
            &room_id,
            &service,
            "indexing",
            KindSet::READ,
            None,
            Subject::person(person(&owner)),
        ));
        let app = "so.tilo.sheets";
        let payroll = collab_types::document_node(&room_id, app, "payroll");
        let deny = |id: &str, subject: Subject, node: tilo_action_policy::TreePath| {
            Grant::new(
                id,
                subject,
                node,
                KindSet::READ,
                Answer::Deny,
                Subject::person(person(&owner)),
            )
        };
        entry.grant(deny(
            "bob-payroll",
            Subject::person(person(&bob)),
            payroll.clone(),
        ));
        entry.grant(deny(
            "service-payroll",
            Subject::agent(crate::shell::service_members::service_principal(&service)),
            payroll,
        ));
        admit(&mut tree, room, cert(&bob, 20, false), Vec::new());
        admit(&mut tree, room, cert(&carol, 30, false), Vec::new());
        let own = vec![(cert(&owner, 10, false), false)];

        let payroll = collab_types::document_channel_id(app, &room_id, "payroll");
        let notes = ChannelRoute::new(app, "notes");
        let unwritten = ChannelRoute::new("so.tilo.notes", "draft");
        assert_eq!(
            allowed_devices(&tree, (room, payroll, None), 1_000, &own),
            BTreeSet::from([device_key(10), device_key(30)])
        );
        let everyone = BTreeSet::from([device_key(10), device_key(20), device_key(30), service]);
        for route in [notes, unwritten] {
            let channel = route.channel(&room_id);
            assert_eq!(
                allowed_devices(&tree, (room, channel, Some(&route)), 1_000, &own),
                everyone
            );
        }

        let entry = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|candidate| candidate.id == workspace)
            .unwrap();
        assert!(entry.revoke_grant(&tilo_action_policy::GrantId::new("bob-payroll")));
        let lifted = allowed_devices(&tree, (room, payroll, None), 1_000, &own);
        assert!(lifted.contains(&device_key(20)));
        assert!(!lifted.contains(&service));
    }

    /// "Finance: Deny Read; Finance › summary: Allow Read" for Bob, and
    /// "Finance: Deny Read" for a service member: a channel whose route proves
    /// a finance record is withheld from both, the summary's is not, another
    /// app's is not. A channel whose record is unknown is withheld from
    /// everyone (it may be a member's own record), and a route that is not
    /// the channel's names nothing. A member's own record is theirs alone.
    #[test]
    fn a_read_deny_over_an_app_withholds_every_routed_channel_of_it() {
        let owner = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let bob = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        let carol = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let mut tree = WorkspaceTree::default();
        let workspace = tree.create_hosted_workspace("Shared").0;
        let room = tree.room_of(workspace).unwrap();
        let service = device_key(50);
        let entry = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|candidate| candidate.id == workspace)
            .unwrap();
        entry.set_owner(person(&owner));
        entry.admit(person(&bob), Role::Editor);
        entry.admit(person(&carol), Role::Viewer);
        let room_id = entry.policy.room.clone();
        entry.grant(crate::shell::service_members::service_membership(
            &room_id,
            &service,
            "indexing",
            KindSet::READ,
            None,
            Subject::person(person(&owner)),
        ));
        let finance = tilo_action_policy::TreePath::room(&room_id).app("finance");
        let read = |id: &str, subject: Subject, node, answer| {
            Grant::new(
                id,
                subject,
                node,
                KindSet::READ,
                answer,
                Subject::person(person(&owner)),
            )
        };
        entry.grant(read(
            "bob-finance",
            Subject::person(person(&bob)),
            finance.clone(),
            Answer::Deny,
        ));
        entry.grant(read(
            "bob-summary",
            Subject::person(person(&bob)),
            collab_types::document_node(&room_id, "finance", "summary"),
            Answer::Allow,
        ));
        entry.grant(read(
            "service-finance",
            Subject::agent(crate::shell::service_members::service_principal(&service)),
            finance,
            Answer::Deny,
        ));
        admit(&mut tree, room, cert(&bob, 20, false), Vec::new());
        admit(&mut tree, room, cert(&carol, 30, false), Vec::new());
        let own = vec![(cert(&owner, 10, false), false)];
        let everyone = BTreeSet::from([device_key(10), device_key(20), device_key(30), service]);
        let without_bob_and_service = BTreeSet::from([device_key(10), device_key(30)]);
        let routed = |tree: &WorkspaceTree, app: &str, record: &str| {
            let route = ChannelRoute::new(app, record);
            let channel = route.channel(&room_id);
            allowed_devices(tree, (room, channel, Some(&route)), 1_000, &own)
        };

        for record in ["payroll", "q3", "never-written"] {
            assert_eq!(
                routed(&tree, "finance", record),
                without_bob_and_service,
                "{record}"
            );
        }
        let mut summary = without_bob_and_service.clone();
        summary.insert(device_key(20));
        assert_eq!(routed(&tree, "finance", "summary"), summary);
        assert_eq!(routed(&tree, "so.tilo.notes", "draft"), everyone);

        // A channel nobody named may be anyone's own record: no one reads it.
        let carols = collab_types::member_record_id(&person(&carol), "chat.drafts");
        let unknown = ChannelRoute::new("so.tilo.chat", carols.as_str()).channel(&room_id);
        assert!(allowed_devices(&tree, (room, unknown, None), 1_000, &own).is_empty());
        // A route that is not the channel's is no route at all.
        let forged = ChannelRoute::new("so.tilo.notes", "other");
        assert!(allowed_devices(&tree, (room, unknown, Some(&forged)), 1_000, &own).is_empty());
        // Named, no other person's device reads it, the owner's included.
        let named = routed(&tree, "so.tilo.chat", &carols);
        assert!(named.contains(&device_key(30)));
        assert!(!named.contains(&device_key(10)) && !named.contains(&device_key(20)));

        // Lifting Bob's Deny gives him every channel back; the service
        // member stays out of finance's.
        let entry = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|candidate| candidate.id == workspace)
            .unwrap();
        assert!(entry.revoke_grant(&tilo_action_policy::GrantId::new("bob-finance")));
        let mut with_bob = without_bob_and_service;
        with_bob.insert(device_key(20));
        assert_eq!(routed(&tree, "finance", "payroll"), with_bob);
        assert_eq!(routed(&tree, "so.tilo.notes", "draft"), everyone);
    }

    /// Groups re-check at the first instant the readers change
    /// by time alone, and that instant is exactly where the answer changes.
    #[test]
    fn the_next_policy_change_is_where_the_readers_change_by_time() {
        let owner = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let carol = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let dave = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        let mut tree = WorkspaceTree::default();
        let workspace = tree.create_hosted_workspace("Shared").0;
        let room = tree.room_of(workspace).unwrap();
        let entry = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|candidate| candidate.id == workspace)
            .unwrap();
        entry.set_owner(person(&owner));
        // Carol's grant runs out at 500; Dave's device certificate at 300.
        let node = entry.node();
        let reads = |id: &str, root: &ed25519_dalek::SigningKey| {
            Grant::new(
                id,
                Subject::person(person(root)),
                node.clone(),
                KindSet::READ,
                Answer::Allow,
                Subject::person(person(&owner)),
            )
        };
        entry.grant(reads("carol-reads", &carol).with_lifetime(Lifetime::Until(500_000)));
        entry.grant(reads("dave-reads", &dave));
        admit(&mut tree, room, cert(&carol, 30, false), Vec::new());
        let mut daves = cert(&dave, 40, false);
        daves.expires_at_ns = Some(299_999_999_999);
        tilo_user_identity::sign_device_cert(&mut daves, &dave).unwrap();
        admit(&mut tree, room, daves, Vec::new());
        // This machine's second device's certificate ends at 800.
        let mut second = cert(&owner, 11, false);
        second.expires_at_ns = Some(800_000_000_000);
        tilo_user_identity::sign_device_cert(&mut second, &owner).unwrap();
        let own = vec![(cert(&owner, 10, false), false), (second, false)];

        let (notes, route) = notes_of(&tree, room);
        let notes = (room, notes, Some(&route));
        assert_eq!(next_policy_change(&tree, room, 100, &own), Some(300));
        assert!(allowed_devices(&tree, notes, 299, &own).contains(&device_key(40)));
        assert!(!allowed_devices(&tree, notes, 300, &own).contains(&device_key(40)));
        assert_eq!(next_policy_change(&tree, room, 300, &own), Some(500));
        assert!(allowed_devices(&tree, notes, 499, &own).contains(&device_key(30)));
        assert!(!allowed_devices(&tree, notes, 500, &own).contains(&device_key(30)));
        assert_eq!(next_policy_change(&tree, room, 500, &own), Some(800));
        assert!(!allowed_devices(&tree, notes, 800, &own).contains(&device_key(11)));
        assert_eq!(next_policy_change(&tree, room, 800, &own), None);
        assert_eq!(next_policy_change(&tree, RoomId::new(), 100, &own), None);
    }

    /// The person room has no roster, no grants and no governor: every
    /// channel in it is read by the identity's certified devices that are
    /// not revoked. A revoked device is excluded, and another person's
    /// device never appears.
    #[test]
    fn a_person_rooms_readers_are_the_identitys_unrevoked_devices() {
        let ada = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let bob = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        let room = RoomId::from(collab_types::person_room(&ada.verifying_key().to_bytes()));
        let lost = cert(&ada, 12, false);
        let mut revocation = tilo_user_identity::RevocationEntry {
            user_public_key: lost.user_public_key,
            revoked_device_public_key: lost.device_public_key,
            revoked_at_ns: 2,
            reason: tilo_user_identity::RevocationReason::Lost,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut revocation, &ada).unwrap();
        let own = vec![
            (cert(&ada, 10, false), false),
            (cert(&ada, 11, false), false),
            (lost, true),
            // Another person's device in this device's fleet file.
            (cert(&bob, 20, false), false),
        ];
        let directory = WorkspaceChannelDirectory::new(
            WorkspaceTree::default().shared(),
            Box::new(move |_| own.clone()),
        );
        // A channel of the person room no grant names: the room holds no
        // workspace to name one from.
        let channel = collab_types::document_channel_id(
            "so.tilo.mail",
            &room.to_string(),
            &collab_types::person_record_id("tilo.accounts"),
        );
        assert_eq!(
            directory.allowed_devices(room, channel, None, 1_000),
            Some(BTreeSet::from([device_key(10), device_key(11)])),
            "the identity's devices read every channel of its person room"
        );
        // A room of nobody's in the fleet is no person room: nothing reads
        // it, as before.
        assert_eq!(
            directory.allowed_devices(RoomId::new(), channel, None, 1_000),
            None
        );
    }
}
