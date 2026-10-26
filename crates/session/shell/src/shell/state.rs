use automerge::{AutoCommit, ChangeHash};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tilo_automerge::ApplyIncoming;
use tilo_clock::unix_now_i64;

mod continuation_labels;
mod local_app_records;
mod room_binding;
mod room_outbox;
mod tree_handle;
mod window_hosting;
use super::fs_authority::schedule_fs_authority_publication;
#[cfg(test)]
use super::fs_authority::wait_for_fs_authority_publication_idle;
pub use continuation_labels::{ContinuationLabels, UnresolvedContinuationLabels};
pub use local_app_records::LocalAppRecord;
pub(crate) use room_binding::room_holds_files;
pub use room_outbox::{HostAnswer, Outgoing, RoomChange, Standing};
pub use tree_handle::{
    RoomReadiness, UnreadyRooms, WeakWorkspaceTreeHandle, WorkspaceTreeHandle,
    WorkspaceTreeWriteGuard, next_policy_revision,
};
pub use window_hosting::WindowHosting;

pub use collab_types::caps::{
    Access, Actor, CapDecision, CapOverride, Effect, Expiry, Principal, ResourceRef, Scope,
    Subject, decide_capability,
};
pub use collab_types::role::Role as CapRole;
pub use tilo_action_policy::Preset;
pub use tilo_desktop_model::grants::PolicyEdit;
use tilo_desktop_model::projection;
pub use tilo_desktop_model::{
    APP_ID_PREFIX, AppId, AppKind, Capability, Collaborator, FileId, FsMode, GRANT_ALWAYS,
    GRANT_ONCE, GRANT_SESSION, GrantScope, GroupId, MintIds, PEER_ID_COUNTER_MASK,
    PEER_ID_SALT_SHIFT, PRESET_ASSIGN_ACTION_ID, PRESET_SAVE_ACTION_ID, PresetMutationError,
    RoomId, SharedSurfaces, Strip, StripId, Tab, TabId, TabRef, TileContent, UserId,
    WORKSPACE_GOVERNANCE_APP_ID, WindowGroup, WindowWidth, Workspace, WorkspaceDefaultError,
    WorkspaceEffectReceipt, WorkspaceId, compose_id, counter_of, peer_salt_of, role_from_id,
    role_id,
};
pub use tilo_desktop_model::{
    DesktopTree, Display, DropTarget, OutputPartition, STATE_SCHEMA_VERSION, TabMove,
};

#[cfg(test)]
mod identity_fs_tests {
    use super::*;

    #[test]
    #[serial_test::serial(process_env)]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn capability_export_reaches_a_ready_or_late_authority_in_the_background() {
        // Phase `capability_export_publishes_to_authority_in_the_background`.
        {
            use std::os::unix::fs::PermissionsExt;
            use std::time::Duration;

            let temp = tempfile::tempdir().expect("temp");
            let publisher = temp.path().join("publisher.sh");
            let log = temp.path().join("authority-arguments");
            let started = temp.path().join("publisher-started");
            let release = temp.path().join("publisher-release");
            std::fs::write(
                &publisher,
                "#!/bin/sh\nprintf ready > \"$TILO_FS_AUTHORITY_TEST_STARTED\"\nwhile [ ! -f \"$TILO_FS_AUTHORITY_TEST_RELEASE\" ]; do sleep 0.01; done\nprintf '%s\\n' \"$@\" > \"$TILO_FS_AUTHORITY_TEST_LOG\"\n",
            )
            .expect("publisher script");
            let mut permissions = std::fs::metadata(&publisher)
                .expect("metadata")
                .permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&publisher, permissions).expect("executable publisher");
            let acl = temp.path().join("fs-capabilities.json");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_FS_AUTHORITY_PUBLISHER", &publisher);
            env.set("TILO_FS_AUTHORITY_TEST_LOG", &log);
            env.set("TILO_FS_AUTHORITY_TEST_STARTED", &started);
            env.set("TILO_FS_AUTHORITY_TEST_RELEASE", &release);

            let tree = WorkspaceTree {
                fs_acl_path: Some(acl.clone()),
                ..WorkspaceTree::default()
            };
            tree.export_fs_capabilities();
            let deadline = tilo_test_env::hang_deadline();
            while std::time::Instant::now() < deadline {
                if started.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(started.exists(), "the publisher worker must start");
            assert!(
                !log.exists(),
                "publication returned while the deliberately blocked publisher was unfinished"
            );
            std::fs::write(&release, b"release").expect("release publisher");
            // The publisher writes its log line by line, so the log existing
            // says nothing about it being complete. The worker retires only
            // after the publisher has exited, which is the event to wait on;
            // the budget is a backstop and a stall reports the attempt history.
            wait_for_fs_authority_publication_idle(tilo_test_env::hang_bound())
                .unwrap_or_else(|report| panic!("the released publisher never finished: {report}"));
            let arguments = std::fs::read_to_string(&log).expect("background publication");
            assert_eq!(
                arguments.lines().collect::<Vec<_>>(),
                ["publish-authority", "--acl", acl.to_str().unwrap()],
                "the publisher needs no path into the daemon's root-only cache"
            );
        }

        // Phase `capability_export_retries_until_a_late_authority_service_is_ready`.
        {
            use std::os::unix::fs::PermissionsExt;

            let temp = tempfile::tempdir().expect("temp");
            let publisher = temp.path().join("late-publisher.sh");
            let count = temp.path().join("attempts");
            let log = temp.path().join("authority-arguments");
            std::fs::write(
                &publisher,
                "#!/bin/sh\ncount=0\n[ -f \"$TILO_FS_AUTHORITY_TEST_COUNT\" ] && count=$(cat \"$TILO_FS_AUTHORITY_TEST_COUNT\")\ncount=$((count + 1))\nprintf '%s' \"$count\" > \"$TILO_FS_AUTHORITY_TEST_COUNT\"\n[ \"$count\" -le 4 ] && exit 1\nprintf '%s\\n' \"$@\" > \"$TILO_FS_AUTHORITY_TEST_LOG\"\n",
            )
            .expect("publisher script");
            let mut permissions = std::fs::metadata(&publisher)
                .expect("metadata")
                .permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&publisher, permissions).expect("executable publisher");
            let acl = temp.path().join("fs-capabilities.json");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_FS_AUTHORITY_PUBLISHER", &publisher);
            env.set("TILO_FS_AUTHORITY_TEST_COUNT", &count);
            env.set("TILO_FS_AUTHORITY_TEST_LOG", &log);
            let tree = WorkspaceTree {
                fs_acl_path: Some(acl),
                ..WorkspaceTree::default()
            };
            tree.export_fs_capabilities();
            // The worker only retires once a publication succeeds, so its
            // retirement is the event to wait on. Four backed-off failures take
            // about 1.5 s of sleeps plus five process spawns; the budget is only
            // a backstop for a worker that never succeeds.
            wait_for_fs_authority_publication_idle(tilo_test_env::hang_bound()).unwrap_or_else(
                |report| panic!("the retry worker never published the queued ACL: {report}"),
            );
            assert!(
                log.exists(),
                "the queued ACL survives more than three failures"
            );
            assert!(
                std::fs::read_to_string(&count)
                    .expect("attempt count")
                    .parse::<u8>()
                    .expect("numeric attempt count")
                    > 3
            );
        }

        // Phase `a_device_without_the_governor_key_publishes_once_and_settles`.
        {
            use std::os::unix::fs::PermissionsExt;

            let temp = tempfile::tempdir().expect("temp");
            let publisher = temp.path().join("joined-device-publisher.sh");
            let count = temp.path().join("attempts");
            std::fs::write(
                &publisher,
                "#!/bin/sh\nprintf x >> \"$TILO_FS_AUTHORITY_TEST_COUNT\"\nexit 77\n",
            )
            .expect("publisher script");
            let mut permissions = std::fs::metadata(&publisher)
                .expect("metadata")
                .permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&publisher, permissions).expect("executable publisher");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_FS_AUTHORITY_PUBLISHER", &publisher);
            env.set("TILO_FS_AUTHORITY_TEST_COUNT", &count);
            let tree = WorkspaceTree {
                fs_acl_path: Some(temp.path().join("fs-capabilities.json")),
                ..WorkspaceTree::default()
            };
            tree.export_fs_capabilities();
            wait_for_fs_authority_publication_idle(tilo_test_env::hang_bound()).unwrap_or_else(
                |report| panic!("a joined device kept retrying publication: {report}"),
            );
            assert_eq!(
                std::fs::read_to_string(&count).expect("attempt count"),
                "x",
                "a device that does not govern the workspace asks once"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn filesystem_export_maps_verified_people_and_per_workspace_membership() {
        // Phase `filesystem_export_maps_selected_person_through_verified_device_chain`.
        {
            let temp = tempfile::tempdir().expect("temp");
            let xdg = temp.path().join("config");
            let system = temp.path().join("system");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", &xdg);
            env.set("TILO_IDENTITY_ROOT", &system);
            let identity_dir = xdg.join("tilo/identity");
            let paths = tilo_user_identity::IdentityPaths::under(&system);
            crate::firstboot::ensure_collaboration_identity(&identity_dir, &paths)
                .expect("identity");
            let profile = crate::identity_registry::IdentityRegistryStore::at(&identity_dir)
                .load()
                .expect("registry")
                .selected_for_workspace("new-room")
                .expect("profile")
                .clone();

            let mut tree = WorkspaceTree::default();
            let mut workspace = Workspace::new(WorkspaceId(1), "Files");
            workspace.sandbox.push(CapOverride::grant(
                Subject::person(profile.principal_id()),
                Scope::FsPath("/var/workspace/files".into()),
                Access::Write,
            ));
            tree.desktop.workspaces.push(workspace);
            let output = temp.path().join("fs-caps.json");
            tree.fs_acl_path = Some(output.clone());
            tree.export_fs_capabilities();
            crate::shell::persist::wait_for_saves();

            let exported: Vec<CapOverride> = serde_json::from_slice::<collab_types::FsAuthority>(
                &std::fs::read(&output).expect("export"),
            )
            .expect("json")
            .capabilities;
            let device = tilo_user_identity::persist::read_device_key(&paths.device_privkey)
                .expect("device")
                .verifying_key()
                .to_bytes();
            assert!(exported.iter().any(|capability| {
                matches!(
                    capability.subject,
                    Subject::User(key) if key.as_bytes() == &device
                ) && capability.access == Access::Write
            }));

            crate::device_fleet::DeviceFleetStore::at(&identity_dir, paths)
                .revoke(
                    &profile.id,
                    device,
                    tilo_user_identity::RevocationReason::Retired,
                    1,
                )
                .expect("revoke current device");
            tree.export_fs_capabilities();
            crate::shell::persist::wait_for_saves();
            let after_revoke: Vec<CapOverride> =
                serde_json::from_slice::<collab_types::FsAuthority>(
                    &std::fs::read(&output).expect("export"),
                )
                .expect("json")
                .capabilities;
            assert!(after_revoke.iter().all(|capability| {
                !matches!(
                    capability.subject,
                    Subject::User(key) if key.as_bytes() == &device
                )
            }));
        }

        // Phase `filesystem_export_projects_room_membership_per_workspace_and_preserves_revocation`.
        {
            use tilo_action_policy::{
                Answer, Grant, KindSet, Lifetime, Subject as PolicySubject, TreePath,
            };
            let temp = tempfile::tempdir().expect("temp");
            let output = temp.path().join("fs-caps.json");
            let alice = collab_types::Ed25519PublicKey::from_bytes([0xa1; 32]);
            let bob = collab_types::Ed25519PublicKey::from_bytes([0xb2; 32]);
            let alice_id = hex::encode(alice.as_bytes());
            let bob_id = hex::encode(bob.as_bytes());
            let mut room_a = Workspace::new(WorkspaceId(1), "A");
            let mut room_b = Workspace::new(WorkspaceId(2), "B");
            let primary_relay_scope =
                crate::shell::network::room_topic_id(room_a.room()).to_string();
            let secondary_relay_scope =
                crate::shell::network::room_topic_id(room_b.room()).to_string();
            let reader = |id: &str, person: &str, room: &str| {
                Grant::new(
                    id,
                    PolicySubject::Person(person.to_owned()),
                    TreePath::room(room),
                    KindSet::READ,
                    Answer::Allow,
                    PolicySubject::Person("owner".into()),
                )
            };
            let first_room = room_a.policy.room.clone();
            room_a.grant(reader("alice-reads", &alice_id, &first_room));
            // A Files share remains Files authority, not an implicit relay
            // membership on either room.
            room_a.sandbox.push(CapOverride::new(
                Subject::User(alice),
                Scope::FsPath("/var/workspace/a".into()),
                Effect::Grant,
                Access::Read,
            ));
            let until_ms = (tilo_clock::unix_now() + 3600) * 1000;
            let second_room = room_b.policy.room.clone();
            room_b.grant(
                reader("bob-reads", &bob_id, &second_room).with_lifetime(Lifetime::Until(until_ms)),
            );
            let mut tree = WorkspaceTree {
                desktop: DesktopTree {
                    workspaces: vec![room_a, room_b],
                    ..Default::default()
                },
                fs_acl_path: Some(output.clone()),
                ..WorkspaceTree::default()
            };
            let read = |output: &std::path::Path| -> collab_types::FsAuthority {
                serde_json::from_slice(&std::fs::read(output).expect("export")).expect("json")
            };
            tree.export_fs_capabilities();
            crate::shell::persist::wait_for_saves();

            let exported = read(&output);
            let members_of = |authority: &collab_types::FsAuthority, scope: &str| {
                authority
                    .room_members
                    .get(scope)
                    .cloned()
                    .unwrap_or_default()
            };
            assert!(members_of(&exported, &primary_relay_scope).contains(
                &collab_types::RoomMember {
                    user: alice,
                    until: None,
                }
            ));
            assert!(members_of(&exported, &secondary_relay_scope).contains(
                &collab_types::RoomMember {
                    user: bob,
                    until: Some(until_ms / 1000),
                }
            ));
            assert!(
                members_of(&exported, &secondary_relay_scope)
                    .iter()
                    .all(|member| member.user != alice)
            );
            assert!(exported.capabilities.iter().any(|capability| {
                capability.subject == Subject::User(alice)
                    && capability.scope == Scope::FsPath("/var/workspace/a".into())
            }));

            // A denial in the room's policy keeps the relay closed to her.
            tree.desktop.workspaces[0].grant(Grant::new(
                "alice-denied",
                PolicySubject::Person(alice_id),
                TreePath::room(&first_room),
                KindSet::READ,
                Answer::Deny,
                PolicySubject::Person("owner".into()),
            ));
            tree.export_fs_capabilities();
            crate::shell::persist::wait_for_saves();
            assert!(
                members_of(&read(&output), &primary_relay_scope)
                    .iter()
                    .all(|member| member.user != alice)
            );

            tree.desktop.workspaces[0]
                .revoke_grant(&tilo_action_policy::GrantId::new("alice-denied"));
            tree.desktop.workspaces[0]
                .revoke_grant(&tilo_action_policy::GrantId::new("alice-reads"));
            tree.export_fs_capabilities();
            crate::shell::persist::wait_for_saves();
            assert!(
                members_of(&read(&output), &primary_relay_scope)
                    .iter()
                    .all(|member| member.user != alice)
            );
        }
    }
}

/// Whoever a message arrived from, named as the transport authenticated
/// it — not as the message claims. The only thing about a sender the
/// receiver did not take on faith, so the only thing an authority
/// decision may rest on. Namespaced (`iroh:` / `hub:`) and carried whole
/// rather than hashed: equality must not turn on a digest an attacker
/// can grind against.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PeerOrigin(String);

impl PeerOrigin {
    /// A remote peer, by its authenticated iroh endpoint id.
    pub fn peer(peer: &iroh::EndpointId) -> Self {
        Self(format!("iroh:{peer}"))
    }

    /// A remote peer named as an invite token names it — endpoint text
    /// already validated by
    /// [`crate::network::invite::validate_invite_token`].
    pub fn peer_id(endpoint_id: &str) -> Self {
        Self(format!("iroh:{endpoint_id}"))
    }

    /// An in-process `crate::network::hub::Hub` subscriber.
    pub fn hub(id: u64) -> Self {
        Self(format!("hub:{id}"))
    }

    /// This machine, for a batch that did not come off a wire at all
    /// (a local document fork replayed into a room).
    pub fn local() -> Self {
        Self("local".to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The handle presence frames identify this origin's peer by, or
    /// `None` when this isn't a remote iroh peer — bridges presence and
    /// room replication onto the same row per peer. One-way only, never
    /// for authority: a collision merges a display row, not a permission.
    pub fn liveness_handle(&self) -> Option<u64> {
        Some(crate::network::behaviour::peer_liveness_handle(
            &self.endpoint()?,
        ))
    }

    /// The remote iroh endpoint this origin names, `None` for any other.
    pub fn endpoint(&self) -> Option<iroh::EndpointId> {
        self.0.strip_prefix("iroh:")?.parse().ok()
    }
}

impl std::fmt::Display for PeerOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Root-signed device identity admitted into one room by that room's
/// governance authority. The map key is the authenticated Iroh endpoint id
/// and must equal the certificate's device key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernedPeerIdentity {
    pub device_cert: tilo_user_identity::DeviceCert,
    /// Additive root-signed evidence. Omission in a later announcement cannot
    /// remove an already-governed revocation.
    #[serde(default)]
    pub revocations: Vec<tilo_user_identity::RevocationEntry>,
    /// Set when governance admitted the device as a delegate of its person,
    /// an agent or an installed bot. Absent from every other identity's
    /// bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegate: Option<tilo_peer_state::GovernedDelegateRow>,
}

/// `peer`'s admitted roster: each device it admitted, keyed by endpoint id,
/// with the revocations of its person.
fn admitted_roster(peer: &tilo_peer_state::StatePeer) -> BTreeMap<String, GovernedPeerIdentity> {
    let revocations = peer.revocations();
    peer.certificates()
        .into_iter()
        .filter_map(|device_cert| {
            let endpoint = iroh::EndpointId::from_bytes(&device_cert.device_public_key).ok()?;
            let revocations = revocations
                .iter()
                .filter(|entry| entry.user_public_key == device_cert.user_public_key)
                .cloned()
                .collect();
            Some((
                endpoint.to_string(),
                GovernedPeerIdentity {
                    device_cert,
                    revocations,
                    delegate: None,
                },
            ))
        })
        .collect()
}

impl GovernedPeerIdentity {
    /// How the agent this device is is shown, when governance admitted it
    /// as one: only when its person's root signed the delegation for exactly
    /// this device. Withdrawn and expired agents are still named, so the
    /// work they did stays attributed to them.
    fn projected_agent(&self) -> Option<tilo_user_identity::GovernedAgentIdentityV2> {
        let delegation = &self.delegate.as_ref()?.delegation;
        let agent_id = delegation.delegate.agent_id()?;
        let cert = &self.device_cert;
        (delegation.device_public_key == cert.device_public_key
            && delegation
                .verify(&cert.user_public_key, delegation.issued_at_ns)
                .is_ok())
        .then(|| tilo_user_identity::GovernedAgentIdentityV2 {
            agent_id: agent_id.to_owned(),
            label: delegation.label.clone(),
            color: delegation.color,
        })
    }
}

/// Former name of [`WorkspaceAuthority`], still imported (`as _`) by
/// shell modules written against the Cloud-side extension trait.
pub use tilo_desktop::authority::{WorkspaceAuthority, WorkspaceAuthority as WorkspaceExt};

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

/// The counter behind every id minted in this process — the low 48 bits.
/// The high 16 are the salt of the tree that mints ([`WorkspaceTree::mint_id`]),
/// so the counter is shared and the namespace is not: two trees in one
/// process (simulated devices) mint in their own namespaces, and one
/// counter keeps every id unique within each. Two atomics' worth of state
/// because every subsystem mints concurrently.
/// [`WorkspaceTree::seed_next_id_from_self`] advances it past the highest
/// persisted id at load, or a fresh process restarts at `1` and collides
/// with an already-loaded entry.
pub(crate) static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// An id under `salt`, from the process's counter.
pub(crate) fn mint_under(salt: u16) -> u64 {
    compose_id(salt, NEXT_ID.fetch_add(1, AtomicOrdering::Relaxed))
}

/// An id in the unsalted namespace (salt `0`): what a tree whose session
/// never set a salt mints ([`WorkspaceTree::bootstrap_default`],
/// `WorkspaceTree::default()`), so fixtures built around such a tree can
/// make ids matching it. Ids for a device's tree come from
/// [`WorkspaceTree::mint_id`].
pub(crate) fn next_id() -> u64 {
    mint_under(0)
}

/// Ids under one salt as a [`MintIds`], for model operations that create
/// windows, groups or strips: [`WorkspaceTree::ids`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SaltedIds(pub u16);

impl MintIds for SaltedIds {
    fn mint_id(&mut self) -> u64 {
        mint_under(self.0)
    }
}

/// Tests only: `workspace`'s copy in `room` instead, everything else as it is
/// (its grants rehomed to the room), made through the stored form as a file
/// would hold it. A workspace never changes its room; fixtures that need a
/// workspace of a given room built from another use this.
#[cfg(test)]
pub(crate) fn rehomed(workspace: &Workspace, room: RoomId) -> Workspace {
    let mut stored = serde_json::to_value(workspace).expect("a workspace serializes");
    stored["room"] = serde_json::Value::String(room.to_string());
    let mut copy: Workspace = serde_json::from_value(stored).expect("the stored form reads back");
    copy.collaborators.clone_from(&workspace.collaborators);
    copy.bots.clone_from(&workspace.bots);
    copy
}

/// A fresh workspace or strip id in the unsalted namespace, matching a tree
/// with no salt ([`next_id`]). A device's tree mints its own:
/// [`WorkspaceTree::mint_strip_id`], [`WorkspaceTree::mint_workspace_id`].
pub trait FreshId: Sized {
    /// Allocate a fresh, process-wide unique id under salt `0`.
    fn new() -> Self;
}

impl FreshId for WorkspaceId {
    fn new() -> Self {
        Self(next_id())
    }
}

impl FreshId for StripId {
    fn new() -> Self {
        Self(next_id())
    }
}

/// Device-local identity needed to reconstruct a hosted document tab.
/// It is persisted in the local profile but never projected into a room doc.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeDocumentReference {
    pub provider_id: String,
    pub path: std::path::PathBuf,
}

/// The id of the person sitting at this machine. Derived from the device
/// identity when provisioned, falling back to the OS account name —
/// stable across restarts, or an owner would lose their own workspaces.
/// The person a user root key names, as workspace owners record it.
pub fn person_id_of(user_public_key: [u8; 32]) -> Option<UserId> {
    match Principal::person_from_key(collab_types::crypto::Ed25519PublicKey::from_bytes(
        user_public_key,
    )) {
        Principal::Person(person) => Some(person),
        _ => None,
    }
}

pub fn local_user_id() -> UserId {
    #[cfg(test)]
    crate::shell::test_support::ambient_start("session identity");
    if let Ok(v) = std::env::var("TILO_USER_ID")
        && !v.is_empty()
    {
        return v;
    }
    // The compositor's session keeps it current on a worker: the loop asks
    // for it on every frame.
    crate::watched_identity::session_user().unwrap_or_else(read_local_user_id)
}

/// [`local_user_id`] read from the person's public key on the disk, or the
/// session's account name without one.
pub(crate) fn read_local_user_id() -> UserId {
    if let Ok(bytes) = std::fs::read(crate::firstboot::collaboration_identity_paths().user_pubkey)
        && bytes.len() == 32
    {
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        if let Principal::Person(id) =
            Principal::person_from_key(collab_types::crypto::Ed25519PublicKey::from_bytes(key))
        {
            return id;
        }
    }
    std::env::var("USER").unwrap_or_else(|_| "local".to_string())
}

/// The colour the person at this machine wears: their stored choice
/// (`identity.colour`, roaming with their other personal settings), or the
/// hue their stable identity derives until they choose one — never the
/// session's handle, so every machine they use and every collaborator who
/// sees them agrees. Presence frames carry it to collaborators many times a
/// second, so this reads it again at most once a second;
/// [`read_local_presence_hue`] reads it now.
pub fn local_presence_hue() -> collab_types::presence::PresenceHue {
    const FRESH: std::time::Duration = std::time::Duration::from_secs(1);
    if let Ok(known) = LOCAL_PRESENCE_HUE.lock()
        && let Some((at, hue)) = *known
        && at.elapsed() < FRESH
    {
        return hue;
    }
    read_local_presence_hue()
}

static LOCAL_PRESENCE_HUE: std::sync::Mutex<
    Option<(std::time::Instant, collab_types::presence::PresenceHue)>,
> = std::sync::Mutex::new(None);

/// [`local_presence_hue`] read from the settings document and identity
/// now, for a caller that knows the settings just changed.
pub fn read_local_presence_hue() -> collab_types::presence::PresenceHue {
    let hue = collab_types::presence::PresenceHue::chosen_in(&serde_json::Value::Object(
        settings_store::load(),
    ))
    .unwrap_or_else(|| collab_types::presence::PresenceHue::of_identity(&local_user_id()));
    if let Ok(mut known) = LOCAL_PRESENCE_HUE.lock() {
        *known = Some((std::time::Instant::now(), hue));
    }
    hue
}

/// Whether `room` is the local person's own room. It is never a workspace:
/// it has no roster to invite anyone into, and its records travel only
/// between that person's devices, so no invite, enrollment or continuation
/// may turn it into a room collaborators share.
pub fn is_local_person_room(room: RoomId) -> bool {
    collab_types::person_room_of(&local_user_id()) == Some(room.as_uuid())
}

/// Identity acting in a specific durable room.
///
/// Registry errors fail back to the session identity for compatibility, but
/// a valid explicit binding always wins. Callers that launch an app use the
/// stricter path resolver and fail closed if its private record is unavailable.
pub fn local_user_id_for_room(room: &RoomId) -> UserId {
    #[cfg(test)]
    crate::shell::test_support::ambient_start("room identity");
    crate::identity_registry::selected_principal_id(&room.to_string())
        .ok()
        .flatten()
        .unwrap_or_else(local_user_id)
}

/// [`local_user_id_for_room`] for the compositor's input and frame loop, which
/// asks on every key and frame. After the first answer it never waits for the
/// identity directory: a change another process made shows one refresh later.
/// Code that records who owns something uses the exact function.
pub fn local_user_id_for_room_prompt(room: &RoomId) -> UserId {
    #[cfg(test)]
    crate::shell::test_support::ambient_start("room identity");
    crate::identity_registry::selected_principal_id_prompt(&room.to_string())
        .ok()
        .flatten()
        .unwrap_or_else(local_user_id)
}

/// The name a session goes by before it says who is sitting at it.
/// Derived from the Iroh peer handle, so it must match the fallback
/// label [`crate::shell::peer_presence::fold_inbound`] gives an
/// unnamed publisher, or one peer shows up as two.
pub fn session_handle(peer: u64) -> String {
    format!("p{peer:016x}")
}

/// A change to a room's governance made at this device, for the room's
/// governor to decide ([`WorkspaceTree::request_governance`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GovernanceRequest {
    /// A change to the room's members, roles or grants.
    Policy(tilo_action_policy::Change),
    /// A sandbox capability for the room's apps.
    GrantSandbox(CapOverride),
    /// Take back the sandbox capability with this id.
    RevokeSandbox(String),
    /// A desktop command on the room's governance (its name, presets,
    /// shortcuts, app action grants).
    Command(tilo_desktop_model::commands::DesktopCommand),
}

/// A governance change its room's governor refused
/// ([`WorkspaceTree::refuse_governance`]), for the chrome to say.
#[derive(Debug, Clone, PartialEq)]
pub struct GovernanceRefusal {
    pub room: RoomId,
    pub change: RoomChange,
    /// The governor's reason, worded for the person.
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceTree {
    /// Native authority is never re-derived from desktop caches.
    #[serde(default)]
    pub native_authority: super::native_workspace_owner::NativeAuthorityStorage,
    #[serde(skip)]
    pub native_runtime: super::native_workspace_owner::NativeStateRuntime,
    /// Enrolled rooms whose governor stewards their channels: every hosted
    /// room, whether a person's device or a relay hosts it. Their records
    /// are sealed channels every member opens itself, never documents the
    /// governor commits.
    /// Projected from the owner, so not persisted.
    #[serde(skip)]
    pub stewarded_rooms: BTreeSet<RoomId>,
    /// Enrolled rooms whose host stewards no channels (a host that may not
    /// own workspaces): their records are documents the native owner
    /// commits. A held room is in neither set until its replica is
    /// projected, and then in exactly one. Projected, so not persisted.
    #[serde(skip)]
    pub committed_record_rooms: BTreeSet<RoomId>,
    /// Each room's admitted roster: every device its governor admitted,
    /// keyed by endpoint id (the device key), with its root-signed
    /// certificate and its person's revocations. Projected from the
    /// owner, so not persisted.
    #[serde(skip)]
    pub room_rosters: BTreeMap<RoomId, BTreeMap<String, GovernedPeerIdentity>>,
    /// Which version of the authorization-relevant state (grants, roster,
    /// owner, ...) this tree holds. Advanced by the released write guard that
    /// changed it ([`WorkspaceTreeHandle::write`]), from one process-wide
    /// counter, so a larger value is always a later state. A launch records
    /// the value its captured grants came from; the published policy carries
    /// the value it was exported at.
    #[serde(skip)]
    pub policy_revision: u64,
    /// The host-independent desktop: workspaces, focus pointers and the
    /// schema version. Flattened, so `state.json` keeps its shape.
    #[serde(flatten)]
    pub desktop: DesktopTree,
    /// Automerge backing doc for peer-sync. The cache (other fields) is
    /// the source of truth for reads; this doc for peer change export.
    /// Not in `state.json`: saved beside it with its whole history (see
    /// [`crate::shell::persist::state_doc_path`]), which Activity and
    /// Replay read. Public so tests
    /// can synthesize change-blobs, but real consumers must go through
    /// [`Self::current_heads`] / [`Self::export_changes_since`] /
    /// [`Self::apply_changes`]: direct access bypasses the cache rebuild.
    #[serde(skip, default = "AutoCommit::new")]
    pub doc: AutoCommit,
    /// Each held room's admitted document, as its native owner last
    /// projected it ([`Self::project_native_room`]): what the room's
    /// layout scope, records, names and identities are read from. Written
    /// by that projection alone, never by this device's own edits.
    #[serde(skip, default)]
    pub room_docs: HashMap<RoomId, AutoCommit>,
    /// Rooms of workspaces made here ([`Self::create_hosted_workspace`])
    /// whose room no host holds yet: each waits for the network to create it
    /// on the host it is placed on and to enroll this device's native owner
    /// in it. Persisted, so a workspace made just before a restart is still
    /// placed.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub pending_placements: BTreeSet<RoomId>,
    /// What this device asked of its rooms' hosts (governance changes,
    /// effect receipts) that the rooms' documents do not carry yet
    /// ([`room_outbox`]). A room's governed fields change only at its host,
    /// and reach this tree as the projection of its admitted document.
    /// Persisted, so a change made while its host is out of reach, or just
    /// before a restart, is still asked, and what it shows here ahead of
    /// the room stays shown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub room_outbox: Vec<Outgoing>,
    /// Changes made here whose governor refused them, until the chrome
    /// says so ([`Self::take_governance_refusals`]).
    #[serde(skip)]
    pub governance_refusals: Vec<GovernanceRefusal>,
    /// Strips a person made here, waiting for their room's next projection
    /// to publish them kept while empty: a made strip reaches the others at
    /// once, not with its first window. Persisted, so a strip made just
    /// before a restart still goes out.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub strips_to_retain: BTreeSet<StripId>,
    /// Tabs created from same-identity continuation rather than from this
    /// machine's ordinary layout.  Persisting this marker lets a later
    /// checkpoint remove a remote close without ever deleting a local tab
    /// that happened to use the same app id.
    #[serde(default)]
    pub continuation_tabs: BTreeSet<TabId>,
    /// The person this device is certified for (their user root key), when
    /// the identity layer has certified it. Rooms that person owns are
    /// governed by any of their devices.
    #[serde(skip)]
    pub local_person: Option<[u8; 32]>,
    /// Offered tabs this device has actively taken over. Kept separately so
    /// a launch immediately followed by a close still emits a tombstone even
    /// if no periodic checkpoint ran between those events.
    #[serde(default)]
    pub continuation_owned_tabs: HashMap<TabId, RoomId>,
    #[serde(default)]
    pub continuation_closed_tabs: HashMap<TabId, RoomId>,
    /// Safe record-only file cards received through personal continuity.
    /// The value has hashes/references only; it intentionally never contains
    /// a local pathname or thumbnail bytes.
    #[serde(default)]
    pub continuation_file_references:
        HashMap<TabId, crate::shell::personal_continuity::LaunchIntent>,
    /// Native hosted documents on this device. Paths stay in local `state.json`;
    /// room projection deliberately has no corresponding field.
    #[serde(default)]
    pub native_document_tabs: HashMap<TabId, NativeDocumentReference>,
    /// Local launcher-issued CRDT record identity for each adopted app tab.
    /// Kept only in the device snapshot, never in a room document or peer offer.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub local_app_records: HashMap<TabId, LocalAppRecord>,
    /// The Tilo package each tab's window was launched as, from the
    /// launcher's authenticated launch record. Only these windows are Tilo
    /// apps other devices open themselves; every other window is legacy and
    /// stays hosted here. Device-local and persisted, so a restored window
    /// keeps what it was launched as.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub package_launches: HashMap<TabId, String>,
    /// Workspace ids this device used before that no workspace carries any
    /// more, each with the room it named: a workspace a migration joined to
    /// its room's own workspace. Ids never change, so whatever still names
    /// such an id (an action id, a saved reference) reaches the room's
    /// workspace through it ([`Self::resolve_workspace`]). Device-local.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub workspace_aliases: BTreeMap<WorkspaceId, RoomId>,
    /// Rooms this device is joining through a host, each with its
    /// workspace made for the join: from the moment the join starts until
    /// the room's replica installs here. Such a workspace says Joining… and
    /// nothing is written into it; it is routed to the native owner, as
    /// every room is, never governed here meanwhile. A
    /// join that failed stays, for the person to try again.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub joining_rooms: BTreeSet<RoomId>,
    /// Workspaces this device keeps archived: rooms it holds, and their
    /// workspaces, kept whole but off its switcher and desktop, listed in
    /// workspace settings to restore or delete. Archiving is this device's
    /// presentation, never the room's: nothing about the room changes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub archived_workspaces: Vec<Workspace>,
    /// Where to write this tree when a capability changes. `None`
    /// disables it (tests, offscreen renderer). Set at boot by the
    /// compositor — what makes an `Always` grant survive a restart.
    #[serde(skip)]
    pub persist_path: Option<std::path::PathBuf>,
    /// Where to export the capability list the FUSE mount enforces.
    /// `None` disables the export; set at boot to the same path passed
    /// to `tilo-fs --acl`, so both sides decide from one list.
    #[serde(skip)]
    pub fs_acl_path: Option<std::path::PathBuf>,
    /// This device as host of its unconfined windows, which rooms of the
    /// person's alone carry as hosted here instead of as windows to reopen.
    /// Set at session start.
    #[serde(skip)]
    pub window_hosting: Option<WindowHosting>,
    /// Which of the person's devices each of their workspaces shows on, as
    /// the person's workspace list says; set by the network for the
    /// workspace's menu.
    #[serde(skip)]
    pub personal_device_choices:
        HashMap<RoomId, Vec<crate::shell::personal_workspaces::DeviceChoice>>,
    /// Choices made in a workspace's menu, `(room, device, shown)`, waiting
    /// for the network to write them into the person's workspace list.
    #[serde(skip)]
    pub personal_device_requests: Vec<(RoomId, [u8; 32], bool)>,
    /// The window the person last asked to be taken to, set only by
    /// [`crate::shell::tab_binding::bring_forward`]. The compositor ends a
    /// full screen for it and the desktop moves the view to it, each once
    /// per serial. Local and transient: never saved or replicated.
    #[serde(skip)]
    pub brought_forward: Option<crate::shell::tab_binding::BroughtForward>,
    /// Maximized windows stepped back for a window the person was taken
    /// to, given back when that window goes away
    /// ([`crate::shell::stepped_back::SteppedBack`]). Local and transient.
    #[serde(skip)]
    pub stepped_back: Vec<crate::shell::stepped_back::SteppedBack>,
    /// The fields a state file holds that this tree does not read (the
    /// shell engine's room authorities, peer names and identities and share
    /// floors among them), kept exactly as they were: saving the file again
    /// leaves them in place, and nothing is migrated.
    #[serde(flatten)]
    pub retired_fields: serde_json::Map<String, serde_json::Value>,
}

impl Default for WorkspaceTree {
    fn default() -> Self {
        Self {
            desktop: DesktopTree::default(),
            native_authority: super::native_workspace_owner::NativeAuthorityStorage::default(),
            native_runtime: super::native_workspace_owner::NativeStateRuntime::default(),
            stewarded_rooms: BTreeSet::new(),
            committed_record_rooms: BTreeSet::new(),
            room_rosters: BTreeMap::new(),
            policy_revision: 0,
            doc: AutoCommit::new(),
            room_docs: HashMap::new(),
            pending_placements: BTreeSet::new(),
            room_outbox: Vec::new(),
            governance_refusals: Vec::new(),
            strips_to_retain: BTreeSet::new(),
            continuation_tabs: BTreeSet::new(),
            local_person: None,
            continuation_owned_tabs: HashMap::new(),
            continuation_closed_tabs: HashMap::new(),
            continuation_file_references: HashMap::new(),
            native_document_tabs: HashMap::new(),
            local_app_records: HashMap::new(),
            package_launches: HashMap::new(),
            workspace_aliases: BTreeMap::new(),
            joining_rooms: BTreeSet::new(),
            archived_workspaces: Vec::new(),
            persist_path: None,
            fs_acl_path: None,
            window_hosting: None,
            personal_device_choices: HashMap::new(),
            personal_device_requests: Vec::new(),
            brought_forward: None,
            stepped_back: Vec::new(),
            retired_fields: serde_json::Map::new(),
        }
    }
}

/// What [`WorkspaceTree::export_fs_capabilities`] takes from a workspace
/// to write on the state writer thread.
struct FsExportWorkspace {
    room: RoomId,
    /// Only those that persist across a restart; the export never carries
    /// session grants.
    capabilities: Vec<CapOverride>,
    /// Who may read the whole room, and until when (Unix seconds): the
    /// people the relay admits to its topic.
    members: Vec<(UserId, Option<u64>)>,
}

/// The filesystem's capability list as of one moment, to be written on the
/// state writer thread.
struct FsExport {
    path: std::path::PathBuf,
    workspaces: Vec<FsExportWorkspace>,
    sequence: super::native_workspace_owner::store::StateSequence,
    runtime: super::native_workspace_owner::NativeStateRuntime,
}

impl FsExport {
    /// Queue the write; a newer export queued before it starts replaces it.
    fn submit(self) {
        crate::shell::durable_writer::global().submit(self.path.clone(), move || self.write());
    }

    /// Write it, unless an export from a later moment already landed.
    fn write(self) {
        self.runtime.write_companion(
            super::native_workspace_owner::store::Companion::FsCapabilities,
            self.sequence,
            || WorkspaceTree::write_fs_capabilities(&self.path, &self.workspaces),
        );
    }
}

/// Constructors for [`WorkspaceTreeHandle`] used by tests across modules.
pub trait WorkspaceTreeHandleExt: Sized {
    /// A handle wrapped around a default (empty) tree.
    fn test_default() -> Self;
}

impl std::ops::Deref for WorkspaceTree {
    type Target = DesktopTree;
    fn deref(&self) -> &DesktopTree {
        &self.desktop
    }
}

impl std::ops::DerefMut for WorkspaceTree {
    fn deref_mut(&mut self) -> &mut DesktopTree {
        &mut self.desktop
    }
}

impl WorkspaceTreeHandleExt for WorkspaceTreeHandle {
    fn test_default() -> Self {
        Self::new(WorkspaceTree::default())
    }
}

impl WorkspaceTree {
    /// Wrap this tree in a [`WorkspaceTreeHandle`]. Consumes the tree
    /// (there should only ever be one instance) and returns the shared
    /// handle every subsystem clones from.
    pub fn shared(self) -> WorkspaceTreeHandle {
        WorkspaceTreeHandle::new(self)
    }
}

impl WorkspaceTree {
    pub(crate) fn installed_native_owner(
        &self,
    ) -> Option<super::native_workspace_owner::NativeWorkspaceOwner> {
        self.native_runtime.owner()
    }

    /// Whether this device holds `room`: its replica is installed. A room
    /// still being joined or placed is not held yet.
    pub fn holds_room(&self, room: RoomId) -> bool {
        self.native_authority.holds(room)
    }

    /// Whether `room` has its workspace here, shown or archived.
    pub fn has_workspace(&self, room: RoomId) -> bool {
        self.desktop
            .workspaces
            .iter()
            .chain(&self.archived_workspaces)
            .any(|w| w.room() == room)
    }

    /// Take workspace `id` off this device's switcher and desktop, keeping
    /// it whole among the archived ones. The last shown workspace stays.
    /// Answers whether it was archived.
    pub fn archive_workspace(&mut self, id: WorkspaceId) -> bool {
        if self.desktop.workspaces.len() < 2 {
            return false;
        }
        let Some(index) = self.desktop.workspaces.iter().position(|w| w.id == id) else {
            return false;
        };
        let archived = self.desktop.workspaces.remove(index);
        if self.desktop.active == archived.id {
            let next = self
                .desktop
                .workspaces
                .get(index)
                .or_else(|| {
                    index
                        .checked_sub(1)
                        .and_then(|i| self.desktop.workspaces.get(i))
                })
                .map_or_else(WorkspaceId::default, |workspace| workspace.id);
            self.desktop.active = next;
        }
        self.archived_workspaces.push(archived);
        self.seed_doc_from_cache();
        self.desktop.room_epoch = self.desktop.room_epoch.wrapping_add(1);
        self.desktop.external_revision = self.desktop.external_revision.wrapping_add(1);
        self.queue_save();
        true
    }

    /// Keep `workspace`, deleted while archived, archived again (a deletion
    /// undone). Answers whether it was, refused when its room or id has a
    /// workspace here meanwhile.
    pub fn keep_archived(&mut self, workspace: Workspace) -> bool {
        if self
            .desktop
            .workspaces
            .iter()
            .chain(&self.archived_workspaces)
            .any(|existing| existing.room() == workspace.room() || existing.id == workspace.id)
        {
            return false;
        }
        self.archived_workspaces.push(workspace);
        self.place_own_unhosted_rooms();
        self.queue_save();
        true
    }

    /// Bring the archived workspace that is `room` back to this device's
    /// switcher, on screen. Answers its id.
    pub fn restore_archived(&mut self, room: RoomId) -> Option<WorkspaceId> {
        let index = self
            .archived_workspaces
            .iter()
            .position(|w| w.room() == room)?;
        let workspace = self.archived_workspaces.remove(index);
        let id = workspace.id;
        if let Some(strip) = workspace.strips.first() {
            self.desktop.active_strip.entry(id).or_insert(strip.id);
        }
        self.desktop.workspaces.push(workspace);
        self.desktop.active = id;
        self.seed_doc_from_cache();
        self.desktop.room_epoch = self.desktop.room_epoch.wrapping_add(1);
        self.desktop.external_revision = self.desktop.external_revision.wrapping_add(1);
        self.queue_save();
        Some(id)
    }

    /// Put every room of a workspace of this device's own that no host holds
    /// among [`Self::pending_placements`]: shown or archived, made here or
    /// before rooms had hosts, never joined from elsewhere. A workspace this
    /// person has is a room this device's host holds, or one waiting to be
    /// placed there: never one no app may open a document in. Changes no id
    /// and removes nothing. Answers the rooms now waiting.
    fn place_own_unhosted_rooms(&mut self) -> Vec<RoomId> {
        let unhosted: Vec<RoomId> = self
            .desktop
            .workspaces
            .iter()
            .chain(&self.archived_workspaces)
            .map(Workspace::room)
            .filter(|room| {
                !room.is_nil()
                    && !self.native_authority.requires(*room)
                    && !self.joining_rooms.contains(room)
                    && !self.pending_placements.contains(room)
            })
            .collect();
        if !unhosted.is_empty() {
            self.pending_placements.extend(unhosted.iter().copied());
            self.queue_save();
        }
        unhosted
    }

    /// Whether `room`'s workspace is waiting for its replica: a join through
    /// a host started, under way or failed, and not installed yet.
    pub fn is_joining(&self, room: RoomId) -> bool {
        // A room being placed on its host is this person's own, enrolling
        // its native owner on the way: not a join.
        !self.native_authority.holds(room)
            && !self.pending_placements.contains(&room)
            && (self.joining_rooms.contains(&room)
                || self
                    .native_authority
                    .joins()
                    .any(|(joining, _)| joining == room))
    }

    /// The workspace `id` names: the workspace carrying it, or, for an id a
    /// workspace carried before it was joined to its room's own workspace
    /// ([`Self::workspace_aliases`]), that workspace.
    pub fn resolve_workspace(&self, id: WorkspaceId) -> Option<WorkspaceId> {
        if self.desktop.workspaces.iter().any(|w| w.id == id) {
            return Some(id);
        }
        let room = self.workspace_aliases.get(&id)?;
        self.desktop
            .workspaces
            .iter()
            .find(|w| w.room() == *room)
            .map(|w| w.id)
    }

    /// Join `room` through its host as a workspace of its own named `name`:
    /// [`Self::join_room`], and the workspace says Joining… and stays out of
    /// the shell's own engine until the room's replica installs into it.
    pub fn join_native_room(&mut self, name: &str, room: RoomId) -> Option<WorkspaceId> {
        if !self.native_authority.holds(room) {
            self.joining_rooms.insert(room);
        }
        let joined = self.join_room(name, room);
        if joined.is_none() {
            self.joining_rooms.remove(&room);
        }
        joined
    }

    /// Whether `room`'s app records are documents its native owner commits:
    /// only a held room whose host stewards no channels. A room still
    /// joining or being placed has no records here yet, and a hosted room's
    /// records are sealed ([`Self::seals_records`]).
    pub(crate) fn commits_native_documents(&self, room: RoomId) -> bool {
        self.committed_record_rooms.contains(&room)
    }

    /// Whether `room`'s app records are sealed channels each app opens
    /// itself: a held room its host stewards.
    pub(crate) fn seals_records(&self, room: RoomId) -> bool {
        self.stewarded_rooms.contains(&room)
    }

    /// Why no app opens a document in `room` on this device yet, as a
    /// catalogue id: it is still joining or being placed on its host and
    /// holds no records yet. Every room of a workspace here is held, joining
    /// or being placed ([`Self::place_own_unhosted_rooms`]), so an app waits
    /// for it and is never refused it.
    pub(crate) fn documents_unavailable(&self, room: RoomId) -> Option<&'static str> {
        (self.is_joining(room) || self.pending_placements.contains(&room))
            .then_some("workspace-not-ready-no-documents")
    }

    /// Every room apps cannot open documents in yet, with why
    /// ([`Self::documents_unavailable`]): what a wait for a room compares
    /// to learn that it may have changed.
    pub(crate) fn unready_rooms(&self) -> UnreadyRooms {
        self.desktop
            .workspaces
            .iter()
            .chain(&self.archived_workspaces)
            .map(Workspace::room)
            .chain(self.joining_rooms.iter().copied())
            .chain(self.pending_placements.iter().copied())
            .chain(self.native_authority.joins().map(|(room, _)| room))
            .filter_map(|room| Some((room, self.documents_unavailable(room)?)))
            .collect()
    }

    /// Presentation projection from the actual admitted owner, never a second admission.
    pub(crate) fn project_native_room(&mut self, room: RoomId, peer: &tilo_peer_state::StatePeer) {
        let Some(document) = peer.admitted_room() else {
            return;
        };
        // Whoever hosts the room, a person's device or a relay, its host
        // stewards the channels of the rooms it governs exactly as the host
        // decides it (`tilo_room_host`'s stewardship): the room is sealed.
        if peer.authority().capabilities.stewards_room_channels() {
            self.stewarded_rooms.insert(room);
            self.committed_record_rooms.remove(&room);
        } else {
            self.stewarded_rooms.remove(&room);
            self.committed_record_rooms.insert(room);
        }
        // The room's replica is here: its join, or its placement on its
        // host, is over, and the room has its workspace, the one made for
        // the join or, for a room held without one, a new one.
        self.joining_rooms.remove(&room);
        if self.pending_placements.remove(&room) {
            self.queue_save();
        }
        if !self.has_workspace(room) {
            let name = crate::shell::i18n::t("workspace-joined-name");
            self.append_workspace_for_room(&name, room);
        }
        self.room_docs.insert(room, document.clone());
        self.settle_outbox(room);
        if let Some(archived) = self
            .archived_workspaces
            .iter_mut()
            .find(|ws| ws.room() == room)
        {
            room_outbox::adopt_room_document(archived, document, &self.room_outbox);
        }
        self.room_rosters.insert(room, admitted_roster(peer));
        self.queue_inbound_records(room, document);
        self.merge_room_contents(room);
        let local = self
            .desktop
            .workspaces
            .iter()
            .find(|ws| ws.room() == room)
            .map(|ws| self.native_runtime.preserve_files_home_consent(ws, peer))
            .unwrap_or_default();
        self.adopt_into_workspace(room, document, local);
    }

    /// Take `document`, `room`'s admitted document, into the room's
    /// workspace, with this device's own capabilities `local` and what its
    /// outbox shows ahead of the room ([`room_outbox`]), as every commit of
    /// the room does.
    fn adopt_into_workspace(
        &mut self,
        room: RoomId,
        document: &AutoCommit,
        local: Vec<CapOverride>,
    ) {
        if let Some(ws) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|ws| ws.room() == room)
        {
            room_outbox::adopt_room_document(ws, document, &self.room_outbox);
            ws.sandbox.extend(local);
            room_outbox::keep_taken_back(ws, &self.room_outbox);
        }
        self.desktop.room_epoch = self.desktop.room_epoch.wrapping_add(1);
    }

    /// Commit `document` as `room`'s admitted document here, as its native
    /// owner's projection does: for tests, which hold no replica.
    #[cfg(test)]
    pub(crate) fn admit_room_document_for_test(&mut self, room: RoomId, document: &AutoCommit) {
        self.room_docs.insert(room, document.clone());
        self.settle_outbox(room);
        self.adopt_into_workspace(room, document, Vec::new());
    }

    /// The record the room document names for `tab` of `room`, if any.
    pub(crate) fn room_tab_record(&self, room: RoomId, tab: TabId) -> Option<String> {
        tilo_desktop_model::apps::record_routes(self.room_docs.get(&room)?).remove(&tab)
    }

    /// Give this tree its device's salt: every id it mints from now on
    /// carries it in the high 16 bits, and replication treats that range as
    /// this tree's own. Must be set **before** the tree mints anything that
    /// is kept: an id minted under salt `0` is identical on every first
    /// boot, and the room document keys a window group by id, so two such
    /// devices fold two distinct windows into one. Ids already minted keep
    /// their salt — saved ids never change.
    pub fn set_peer_salt(&mut self, salt: u16) {
        self.desktop.salt = salt;
    }

    /// The salt this tree mints under and replication treats as its own —
    /// the id range this tree may write and sweep.
    pub fn local_salt(&self) -> u16 {
        self.desktop.local_salt()
    }

    /// A fresh id (window group, tab, record instance, …) in this tree's
    /// namespace.
    pub fn mint_id(&self) -> u64 {
        mint_under(self.local_salt())
    }

    /// A fresh strip id in this tree's namespace.
    pub fn mint_strip_id(&self) -> StripId {
        StripId(self.mint_id())
    }

    /// A fresh local workspace id in this tree's namespace.
    pub fn mint_workspace_id(&self) -> WorkspaceId {
        WorkspaceId(self.mint_id())
    }

    /// This tree's namespace as a [`MintIds`], for model operations.
    pub fn ids(&self) -> SaltedIds {
        SaltedIds(self.local_salt())
    }

    /// Walk every id and advance `NEXT_ID` past the highest counter
    /// stamped by the local peer salt — idempotent, called after
    /// deserializing. The counter restarts at `1` each process, so
    /// without this it collides with persisted ids. Only local-salt ids
    /// count; a remote peer's must not advance our counter.
    pub fn seed_next_id_from_self(&self) {
        let local_salt = self.local_salt();
        let mut max_counter: u64 = 0;
        let mut consider = |id: u64| {
            if peer_salt_of(id) == local_salt {
                max_counter = max_counter.max(counter_of(id));
            }
        };
        for ws in self
            .desktop
            .workspaces
            .iter()
            .chain(&self.archived_workspaces)
        {
            // A workspace made since ids follow rooms carries its room's id,
            // which is no counter of this peer's.
            if ws.id != WorkspaceId::of_room(ws.room()) {
                consider(ws.id.raw());
            }
            for strip in &ws.strips {
                consider(strip.id.raw());
                for group in &strip.window_groups {
                    consider(group.id);
                    for tab in &group.tabs {
                        consider(tab.id);
                    }
                }
            }
        }
        for record in self.local_app_records.values() {
            consider(record.record_instance);
        }
        // Advance the counter past `max_counter`. `fetch_max` keeps the
        // higher of the two so a parallel allocator that already
        // pushed past max_counter isn't reset backwards.
        NEXT_ID.fetch_max(max_counter.saturating_add(1), AtomicOrdering::Relaxed);
    }

    /// Close the room document's open transaction, stamping when it
    /// happened — otherwise `AutoCommit`'s implicit close leaves the
    /// advisory timestamp at zero and no activity view can date-filter
    /// the change graph. Also keeps each mirrored mutation its own
    /// change rather than letting a run coalesce. Cheap when nothing is
    /// pending, so the compositor also calls it once a frame.
    pub fn commit_room_change(&mut self) {
        let _ = tilo_resource::history::commit_stamped(&mut self.doc);
    }

    /// A copy of the tree with an empty [`Self::doc`] and no
    /// [`Self::room_docs`]: a candidate to encode, without copying every
    /// document and its whole history.
    pub(crate) fn clone_without_documents(&mut self) -> Self {
        let doc = std::mem::replace(&mut self.doc, AutoCommit::new());
        let room_docs = std::mem::take(&mut self.room_docs);
        let copy = self.clone();
        self.doc = doc;
        self.room_docs = room_docs;
        copy
    }

    /// Snapshot of the current automerge change-graph heads, the
    /// baseline peer-sync computes what to send against. Takes `&mut
    /// self` because `AutoCommit` flushes any pending transaction
    /// before returning heads.
    pub fn current_heads(&mut self) -> Vec<ChangeHash> {
        self.doc.get_heads()
    }

    /// Export every change applied since `since` as opaque change-blobs.
    /// **Not a peer-facing export**: [`Self::doc`] holds every workspace
    /// on this machine, so serving it would leak every other room. Peers
    /// get [`Self::export_room_changes_since`], scoped to one room.
    pub fn export_changes_since(&mut self, since: &[ChangeHash]) -> Vec<Vec<u8>> {
        self.doc
            .get_changes(since)
            .into_iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect()
    }

    /// Bring the cache back in line with the room document after a
    /// *local* rewrite (a history restore doesn't go through
    /// [`Self::apply_changes`], which does this for peer changes).
    pub fn resync_cache_from_doc(&mut self) {
        self.rebuild_cache_from_doc();
    }

    /// Apply a batch of opaque change-blobs received from a peer. After
    /// applying, the cache fields are rebuilt via
    /// `Self::rebuild_cache_from_doc` so subsequent reads observe the
    /// merged state.
    ///
    /// # Errors
    ///
    /// Returns the underlying `automerge::AutomergeError` if any blob
    /// fails to decode as a `Change`, or if applying the (otherwise
    /// well-formed) batch to `self.doc` fails.
    pub fn apply_changes(&mut self, changes: &[Vec<u8>]) -> Result<(), automerge::AutomergeError> {
        let parsed: Result<Vec<automerge::Change>, _> = changes
            .iter()
            .map(|b| automerge::Change::try_from(b.as_slice()))
            .collect();
        self.doc.apply_incoming(parsed?)?;
        self.rebuild_cache_from_doc();
        Ok(())
    }

    // Per-room replication: an invite must not hand over a directory of
    // every other room, and `doc`'s change graph cannot be served in
    // part (changes form a hash chain, so filtering leaves dependencies
    // the receiver never sees and automerge parks them unapplied
    // forever). So the replication unit is a document per room,
    // projected from the cache.

    /// Every room this machine is in, deduplicated, in workspace order.
    /// A workspace whose room is still the unminted sentinel is skipped.
    pub fn rooms(&self) -> Vec<RoomId> {
        let mut seen = std::collections::HashSet::new();
        self.desktop
            .workspaces
            .iter()
            .map(crate::shell::state::Workspace::room)
            .filter(|r| !r.is_nil() && seen.insert(*r))
            .collect()
    }

    /// Current Files projection derived solely from governance names and
    /// root-signed device certificates in the accepted room document.
    pub fn trusted_identity_projection(
        &self,
        room: RoomId,
    ) -> tilo_user_identity::TrustedIdentityProjectionV2 {
        let names = self.room_peer_names_in_doc(room);
        let identities = self.room_peer_identities(room);
        let revoked = identities
            .values()
            .flat_map(|identity| identity.revocations.iter())
            .map(|entry| (entry.user_public_key, entry.revoked_device_public_key))
            .collect::<std::collections::BTreeSet<_>>();
        let mut grouped: std::collections::BTreeMap<
            [u8; 32],
            (
                String,
                Vec<tilo_user_identity::GovernedDeviceIdentityV2>,
                bool,
            ),
        > = std::collections::BTreeMap::new();
        // A delegated agent is named by its delegation, not by a governed
        // peer name, and shown under the person it acts for.
        let mut agents: std::collections::BTreeMap<
            [u8; 32],
            Vec<tilo_user_identity::GovernedDeviceIdentityV2>,
        > = std::collections::BTreeMap::new();
        for (peer, identity) in identities {
            let root_key = identity.device_cert.user_public_key;
            if revoked.contains(&(root_key, identity.device_cert.device_public_key)) {
                continue;
            }
            if identity.delegate.is_some() {
                if let Some(agent) = identity.projected_agent() {
                    agents.entry(root_key).or_default().push(
                        tilo_user_identity::GovernedDeviceIdentityV2 {
                            certificate: identity.device_cert,
                            agent: Some(agent),
                        },
                    );
                }
                continue;
            }
            let Some(name) = names.get(&peer) else {
                continue;
            };
            let entry = grouped
                .entry(root_key)
                .or_insert_with(|| (name.clone(), Vec::new(), true));
            if entry.0 != *name {
                entry.2 = false;
                continue;
            }
            entry.1.push(tilo_user_identity::GovernedDeviceIdentityV2 {
                certificate: identity.device_cert,
                agent: None,
            });
        }
        let humans = grouped
            .into_iter()
            .filter(|(_, (_, devices, consistent))| *consistent && !devices.is_empty())
            .map(|(root_public_key, (display_name, mut devices, _))| {
                devices.extend(agents.remove(&root_public_key).unwrap_or_default());
                devices.sort_by_key(|device| device.certificate.device_public_key);
                tilo_user_identity::GovernedHumanIdentityV2 {
                    root_public_key,
                    display_name,
                    devices,
                }
            })
            .collect();
        let projection = tilo_user_identity::TrustedIdentityProjectionV2 {
            schema_version: tilo_user_identity::TRUSTED_IDENTITY_PROJECTION_VERSION,
            humans,
        };
        if projection.validate().is_ok() {
            projection
        } else {
            tilo_user_identity::TrustedIdentityProjectionV2::empty()
        }
    }

    /// The delegated agents on `room`'s roster as history names them: each
    /// device governance admitted as an agent whose person's root signed the
    /// delegation for exactly that device, under the person's governed name.
    /// The same devices, by the same check, that
    /// [`Self::trusted_identity_projection`] marks as agents for Files, so
    /// the workspace's Activity, an app's own history and Files name an
    /// agent alike. Presentation only: nothing is admitted by it.
    pub fn verified_agents(&self, room: RoomId) -> Vec<collab_types::history::RosterAgent> {
        self.trusted_identity_projection(room)
            .humans
            .into_iter()
            .flat_map(|human| {
                let person = human.root_public_key;
                let person_name = human.display_name;
                human.devices.into_iter().filter_map(move |device| {
                    let agent = device.agent?;
                    Some(collab_types::history::RosterAgent {
                        device: device.certificate.device_public_key,
                        person,
                        person_name: person_name.clone(),
                        agent_id: agent.agent_id,
                        label: agent.label,
                        color: agent.color,
                    })
                })
            })
            .collect()
    }

    /// The names `room` carries, keyed by the peer **handle** the
    /// presence path speaks in
    /// ([`crate::network::behaviour::peer_liveness_handle`]), not the
    /// id — the document stores real identity, this bridges the gap. A
    /// peer with no entry is labelled with its handle.
    /// Every device the room names, by transport endpoint: the devices a
    /// record of the room may be asked for.
    pub fn room_peer_endpoints(&self, room: RoomId) -> Vec<iroh::EndpointId> {
        self.room_peer_names_in_doc(room)
            .keys()
            .filter_map(|peer| peer.parse::<iroh::EndpointId>().ok())
            .collect()
    }

    pub fn room_peer_labels(&self, room: RoomId) -> std::collections::BTreeMap<u64, String> {
        let mut out = std::collections::BTreeMap::new();
        for (peer, name) in self.room_peer_names_in_doc(room) {
            let Ok(parsed) = peer.parse::<iroh::EndpointId>() else {
                continue;
            };
            out.insert(
                crate::network::behaviour::peer_liveness_handle(&parsed),
                name,
            );
        }
        out
    }

    /// The person each of `room`'s certified devices belongs to, keyed by
    /// the same presence handle as [`Self::room_peer_labels`] and spelled
    /// as membership capabilities name people ([`person_id_of`]), so the
    /// roster can ask the policy for a role by principal rather than by
    /// display label. Only governed, root-signed device certificates
    /// count — never a peer's own claim — and a revoked or expired device
    /// maps to nobody.
    pub fn room_peer_principals(&self, room: RoomId) -> std::collections::BTreeMap<u64, UserId> {
        self.room_peer_certificates(room)
            .into_iter()
            .filter_map(|(handle, cert)| Some((handle, person_id_of(cert.user_public_key)?)))
            .collect()
    }

    /// The other devices of this device's person in `room`, keyed by
    /// presence handle, each with the name it was certified under: the
    /// same certified devices as [`Self::room_peer_principals`].
    pub fn room_own_devices(&self, room: RoomId) -> std::collections::BTreeMap<u64, String> {
        let Some(local) = self.local_person else {
            return std::collections::BTreeMap::new();
        };
        self.room_peer_certificates(room)
            .into_iter()
            .filter(|(_, cert)| cert.user_public_key == local)
            .map(|(handle, cert)| (handle, cert.device_label))
            .collect()
    }

    fn room_peer_certificates(
        &self,
        room: RoomId,
    ) -> std::collections::BTreeMap<u64, tilo_user_identity::DeviceCert> {
        let identities = self.room_peer_identities(room);
        let revoked = identities
            .values()
            .flat_map(|identity| identity.revocations.iter())
            .map(|entry| (entry.user_public_key, entry.revoked_device_public_key))
            .collect::<std::collections::BTreeSet<_>>();
        let now = tilo_clock::unix_now_ns();
        identities
            .into_iter()
            .filter_map(|(peer, identity)| {
                let cert = &identity.device_cert;
                if cert.is_expired_at(now)
                    || revoked.contains(&(cert.user_public_key, cert.device_public_key))
                {
                    return None;
                }
                let parsed = peer.parse::<iroh::EndpointId>().ok()?;
                Some((
                    crate::network::behaviour::peer_liveness_handle(&parsed),
                    identity.device_cert,
                ))
            })
            .collect()
    }

    /// Put `who` on the roster of the workspace that addresses `room`.
    /// An **upsert**, not a push — a later frame with no explicit
    /// colour must not erase an existing one. Returns whether the
    /// roster changed.
    pub fn admit_collaborator_in_room(&mut self, room: RoomId, who: Collaborator) -> bool {
        if room.is_nil() {
            return false;
        }
        let Some(ws) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.room() == room)
        else {
            return false;
        };
        Self::admit_into(&mut ws.collaborators, who)
    }

    /// [`Self::admit_collaborator_in_room`] addressed by this machine's
    /// local workspace handle instead of the room's durable identity.
    pub fn admit_collaborator(&mut self, workspace_id: WorkspaceId, who: Collaborator) -> bool {
        let Some(ws) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
        else {
            return false;
        };
        Self::admit_into(&mut ws.collaborators, who)
    }

    /// Take every person who was here through `peer`'s session off every
    /// roster. Sweeps every workspace, not one: a session can be in
    /// several rooms at once, and it left all of them.
    pub fn retire_session(&mut self, peer: u64) -> bool {
        let mut changed = false;
        for ws in &mut self.desktop.workspaces {
            let before = ws.collaborators.len();
            ws.collaborators.retain(|c| c.session != Some(peer));
            changed |= before != ws.collaborators.len();
        }
        changed
    }

    /// Upsert one roster row. Split out so the room-addressed and
    /// handle-addressed entry points cannot drift apart.
    fn admit_into(roster: &mut Vec<Collaborator>, who: Collaborator) -> bool {
        if let Some(existing) = roster.iter_mut().find(|c| c.id == who.id) {
            let mut changed = false;
            if !who.name.is_empty() && existing.name != who.name {
                existing.name = who.name;
                changed = true;
            }
            if who.cursor_color.is_some() && existing.cursor_color != who.cursor_color {
                existing.cursor_color = who.cursor_color;
                changed = true;
            }
            if who.role_id.is_some() && existing.role_id != who.role_id {
                existing.role_id = who.role_id;
                changed = true;
            }
            // A row without a session (read off disk, or a fixture) is
            // invisible to `retire_session`; the arriving row is the one
            // that knows, so it wins.
            if who.session.is_some() && existing.session != who.session {
                existing.session = who.session;
                changed = true;
            }
            return changed;
        }
        roster.push(who);
        true
    }

    /// Key the room document stores its peers' names under.
    const ROOM_PEERS_KEY: &'static str = "peers";

    /// Every device `room`'s governor admitted, keyed by endpoint id, as
    /// this device's native owner last projected the room.
    pub fn room_peer_identities(
        &self,
        room: RoomId,
    ) -> std::collections::BTreeMap<String, GovernedPeerIdentity> {
        self.room_rosters.get(&room).cloned().unwrap_or_default()
    }

    /// Whether `room`'s governor admitted the device `endpoint`, with a
    /// certificate live now and not revoked.
    pub fn room_admits_device(&self, room: RoomId, endpoint: &iroh::EndpointId) -> bool {
        let now = tilo_clock::unix_now_ns();
        self.room_rosters
            .get(&room)
            .and_then(|roster| roster.get(&endpoint.to_string()))
            .is_some_and(|identity| {
                let cert = &identity.device_cert;
                !cert.is_expired_at(now)
                    && !identity
                        .revocations
                        .iter()
                        .any(|entry| entry.revoked_device_public_key == cert.device_public_key)
            })
    }

    /// Cache `room`'s admitted document as naming exactly `names` (device
    /// endpoint, label), as [`Self::project_native_room`] would.
    #[cfg(test)]
    pub(crate) fn cache_admitted_peer_names_for_test(
        &mut self,
        room: RoomId,
        names: &[(String, &str)],
    ) {
        use automerge::transaction::Transactable as _;
        let mut doc = AutoCommit::new();
        let peers = doc
            .put_object(
                automerge::ROOT,
                Self::ROOM_PEERS_KEY,
                automerge::ObjType::Map,
            )
            .expect("room peers map");
        for (peer, name) in names {
            doc.put(&peers, peer.as_str(), *name)
                .expect("room peer name");
        }
        self.room_docs.insert(room, doc);
    }

    /// Every name the room document carries. Read from the **document**,
    /// the one the room's governor admitted, so every member answers this
    /// the same way — one source, and it's the authenticated one.
    pub fn room_peer_names_in_doc(
        &self,
        room: RoomId,
    ) -> std::collections::BTreeMap<String, String> {
        Self::peer_names_of(self.room_docs.get(&room))
    }

    /// Every name room document `doc` carries.
    fn peer_names_of(doc: Option<&AutoCommit>) -> std::collections::BTreeMap<String, String> {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        let mut out = std::collections::BTreeMap::new();
        let Some(doc) = doc else {
            return out;
        };
        let Some((_, peers_id)) = doc.get(&ROOT, Self::ROOM_PEERS_KEY).ok().flatten() else {
            return out;
        };
        let keys: Vec<String> = doc.keys(&peers_id).collect();
        for key in keys {
            if let Some((Value::Scalar(s), _)) = doc.get(&peers_id, &key).ok().flatten()
                && let ScalarValue::Str(v) = s.as_ref()
                && !v.as_str().is_empty()
            {
                out.insert(key, v.to_string());
            }
        }
        out
    }

    /// `room`'s layout scope, as its governance document here says: shared
    /// until this device holds a document that says otherwise.
    #[must_use]
    pub fn room_layout(&self, room: RoomId) -> projection::RoomLayout {
        self.room_docs
            .get(&room)
            .map_or_else(projection::RoomLayout::default, projection::room_layout)
    }

    /// Fold `room`'s replication document into the cached workspace that
    /// addresses it — see [`projection::merge_room_contents`]. Reports
    /// whether anything changed.
    fn merge_room_contents(&mut self, room: RoomId) -> bool {
        let local_salt = self.local_salt();
        if local_salt == 0 {
            return false;
        }
        let Some(doc) = self.room_docs.get(&room) else {
            return false;
        };
        let changed = if projection::room_layout(doc).is_personal() {
            // A room whose layout is personal shares no member's windows:
            // nothing of its contents is taken in, and what this device
            // mirrored of other people's windows while it shared them goes.
            // Every window of this device's own stays where it is.
            let owners = projection::contents_owners_of(doc);
            let theirs = |tab: &Tab| {
                let salt = peer_salt_of(tab.id);
                salt != 0
                    && salt != local_salt
                    && salt != tilo_desktop_model::ids::RESERVED_SALT
                    && (owners.is_empty()
                        || owners.contains_key(&salt)
                        || matches!(
                            tab.tile,
                            TileContent::PeerWindow { .. } | TileContent::HostedWindow { .. }
                        ))
            };
            projection::drop_windows_of_others(&mut self.desktop, room, local_salt, &theirs)
        } else {
            projection::merge_room_contents(
                &mut self.desktop,
                room,
                projection::contents_of(doc),
                projection::Reader::Owner(local_salt),
            )
        };
        let presets = projection::workspace_presets_of(doc);
        let mut reprofiled = false;
        if let Some(ws) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|ws| ws.room() == room)
            && ws.presets != presets
        {
            ws.presets = presets;
            reprofiled = true;
        }
        reprofiled || changed
    }

    /// Tell the tree which records `room`'s index holds. Returns whether
    /// this was news, so the caller can skip re-asking when nothing moved.
    pub fn set_room_records(
        &mut self,
        room: RoomId,
        held: std::collections::BTreeMap<String, String>,
    ) -> bool {
        match self.desktop.room_records_held.get(&room) {
            Some(existing) if *existing == held => false,
            _ => {
                self.desktop.room_records_held.insert(room, held);
                true
            }
        }
    }

    /// Take the references peers have added since this was last called.
    /// Drained rather than read so a queue that only grows isn't a leak.
    pub fn take_inbound_room_records(&mut self) -> Vec<(RoomId, String, String)> {
        std::mem::take(&mut self.desktop.room_records_inbound)
    }

    /// Diffs `candidate`'s room-record index against what's already held
    /// for `room`, queues the difference for the shell to fold in (see
    /// [`Self::take_inbound_room_records`]), and reports whether
    /// anything arrived.
    fn queue_inbound_records(&mut self, room: RoomId, candidate: &AutoCommit) -> bool {
        // The difference from what's already held is what a peer
        // brought — queued for the shell to fold in, since the tree
        // can't reach the room's own index.
        let arrived = projection::room_records_of(candidate);
        let already = self.desktop.room_records_held.get(&room);
        let mut inbound: Vec<(RoomId, String, String)> = Vec::new();
        for (id, kind) in arrived {
            let known = already.and_then(|h| h.get(&id));
            if known.map(std::string::String::as_str) != Some(kind.as_str()) {
                inbound.push((room, id, kind));
            }
        }
        let records_changed = !inbound.is_empty();
        self.desktop.room_records_inbound.extend(inbound);
        records_changed
    }

    /// Seed the automerge doc from the cache. Idempotent and diff-aware:
    /// it writes only what differs, so an unchanged tree adds no history
    /// and a rename is one write. Tests building literal trees
    /// should call this to flow doc-side changes out via
    /// [`Self::export_changes_since`]. `active_strip`/`active_group`
    /// stay cache-only.
    ///
    /// # Panics
    ///
    /// Never panics: every doc write is best-effort (the cache is the
    /// source of truth), so a write that fails just skips the nested
    /// entries it would have contained rather than aborting the seed.
    pub fn seed_doc_from_cache(&mut self) {
        tilo_desktop_model::desktop_doc::seed(&mut self.doc, &self.desktop);
        self.commit_room_change();
    }

    fn rebuild_cache_from_doc(&mut self) {
        tilo_desktop_model::desktop_doc::rebuild(&self.doc, &mut self.desktop);
    }

    /// Save the tree to [`Self::persist_path`], if one is set, and export
    /// the capabilities the filesystem enforces. Called on every mutation
    /// that must survive a restart — a compositor that's killed must not
    /// forget which permissions the user granted.
    ///
    /// The caller pays only for a copy of the tree: the writes and their
    /// fsyncs run on the state writer thread, newest copy first, so a
    /// keypress never waits on the disk.
    /// [`crate::shell::persist::wait_for_saves`] waits for them. A copy the
    /// state file refuses (a stale authority snapshot) exports nothing.
    ///
    /// There is no debounce or batching: the copy is taken and submitted
    /// before this returns, and a submission for a path that has not started
    /// writing is replaced by the newer copy, which holds every earlier
    /// revision. The released write guard calls this itself for every write
    /// that changes authorization state, so no policy revision depends on a
    /// caller remembering to save.
    pub fn queue_save(&self) {
        let started = std::time::Instant::now();
        let export = self.fs_capability_export();
        let actions = crate::shell::action_authority::Export::of(self);
        let failure = actions.failure_handle();
        match (&self.persist_path, export) {
            (Some(path), export) => {
                crate::shell::persist::queue_save_or_report(
                    self,
                    path,
                    move || {
                        actions.publish();
                        if let Some(export) = export {
                            export.write();
                        }
                    },
                    move |error| failure.fail(error),
                );
            }
            (None, Some(export)) => {
                export.submit();
                actions.submit();
            }
            (None, None) => actions.submit(),
        }
        let took = started.elapsed();
        if took > std::time::Duration::from_millis(4) {
            tracing::warn!(
                target: "tilo::latency",
                took_ms = took.as_secs_f64() * 1_000.0,
                "copying the workspace tree for saving was slow"
            );
        }
    }

    /// A reader of the room, as the relay admits them to its exact numeric
    /// shard. The relay authenticates public keys, not display names: a
    /// person whose id is not a canonical hex key cannot be made a network
    /// principal and is omitted fail-closed.
    fn relay_room_member(person: &str, until: Option<u64>) -> Option<collab_types::RoomMember> {
        let bytes: [u8; 32] = hex::decode(person).ok()?.try_into().ok()?;
        Some(collab_types::RoomMember {
            user: collab_types::Ed25519PublicKey::from_bytes(bytes),
            until,
        })
    }

    /// Write the capability list the filesystem enforces. The FUSE mount
    /// is a separate process reading a JSON array from the path its
    /// `--acl` names, written atomically so it never reads a half-written
    /// list and concludes the user has no permissions. All workspaces
    /// export together since `/var/workspace` is one mount, so two
    /// workspaces granting the same path to different subjects union.
    ///
    /// The filesystem authenticates the machine's device key. For a local
    /// identity's path grants, export an equivalent device-key binding only
    /// after the registry and root-signed device certificate establish the
    /// `identity -> device` relationship. Restrictions are duplicated too,
    /// so translation can never widen a grant.
    ///
    /// The list is taken from the tree now and written on the state writer
    /// thread, newest list first; the identity files it consults are read
    /// there too.
    pub fn export_fs_capabilities(&self) {
        if let Some(export) = self.fs_capability_export() {
            export.submit();
        }
    }

    fn fs_capability_export(&self) -> Option<FsExport> {
        let path = self.fs_acl_path.clone()?;
        let now = tilo_clock::unix_now();
        let workspaces: Vec<FsExportWorkspace> = self
            .desktop
            .workspaces
            .iter()
            .map(|workspace| FsExportWorkspace {
                room: workspace.room(),
                capabilities: workspace
                    .sandbox
                    .iter()
                    .filter(|capability| capability.persists_across_restart())
                    .cloned()
                    .collect(),
                members: workspace.room_readers_at(now),
            })
            .collect();
        Some(FsExport {
            path,
            workspaces,
            sequence: self.native_runtime.next_sequence(),
            runtime: self.native_runtime.clone(),
        })
    }

    fn write_fs_capabilities(path: &std::path::Path, workspaces: &[FsExportWorkspace]) {
        let identity_paths = crate::firstboot::collaboration_identity_paths();
        let device_public_key =
            tilo_user_identity::persist::read_device_key(identity_paths.device_privkey.as_path())
                .ok()
                .map(|key| key.verifying_key().to_bytes());
        let mut caps: Vec<CapOverride> = Vec::new();
        let mut room_members = std::collections::BTreeMap::new();
        for workspace in workspaces {
            let profile =
                crate::identity_registry::selected_identity_profile(&workspace.room.to_string())
                    .ok()
                    .flatten()
                    .filter(|profile| {
                        let Some(device) = device_public_key else {
                            return false;
                        };
                        let paths = crate::firstboot::collaboration_identity_paths();
                        tilo_user_identity::persist::read_device_cert(
                            &paths.device_cert_for_user(&profile.user_public_key),
                        )
                        .ok()
                        .filter(|cert| tilo_user_identity::verify_device_cert(cert).is_ok())
                        .is_some_and(|cert| {
                            cert.user_public_key == profile.user_public_key
                                && cert.device_public_key == device
                                && !crate::device_fleet::DeviceFleetStore::from_env()
                                    .and_then(|store| store.load(&profile.id))
                                    .is_ok_and(|fleet| fleet.is_revoked(&device))
                        })
                    });
            let identity_actor = profile.as_ref().map(|profile| {
                Actor::signed(collab_types::crypto::Ed25519PublicKey::from_bytes(
                    profile.user_public_key,
                ))
            });
            for capability in workspace
                .capabilities
                .iter()
                .filter(|capability| matches!(capability.scope, Scope::FsPath(_)))
            {
                caps.push(capability.clone());
                if let (Some(device), Some(actor)) = (device_public_key, identity_actor.as_ref())
                    && capability.subject.matches_actor(actor)
                {
                    let mut device_capability = capability.clone();
                    device_capability.subject =
                        Subject::User(collab_types::crypto::Ed25519PublicKey::from_bytes(device));
                    caps.push(device_capability);
                }
            }
            // Signed room membership is a separate projection from Files
            // access: who the room's policy lets read it, per relay shard.
            room_members.insert(
                crate::shell::network::room_topic_id(workspace.room).to_string(),
                workspace
                    .members
                    .iter()
                    .filter_map(|(person, until)| Self::relay_room_member(person, *until))
                    .collect::<Vec<_>>(),
            );
        }
        let authority = collab_types::FsAuthority {
            capabilities: caps,
            room_members,
        };
        let Ok(json) = serde_json::to_vec_pretty(&authority) else {
            return;
        };
        if let Err(e) = tilo_files::native::atomic::write_creating_parent(path, &json) {
            tracing::warn!("filesystem capability export failed: {e}");
            return;
        }
        schedule_fs_authority_publication(path.to_path_buf());
    }

    /// The durable room identity of `workspace_id`. What the launch path
    /// needs to turn a local handle into the value the wire speaks.
    pub fn room_of(&self, workspace_id: WorkspaceId) -> Option<RoomId> {
        self.desktop
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .map(crate::shell::state::Workspace::room)
    }

    /// Join `room`, a room another device governs, as a workspace of its
    /// own named `name`: the workspace that is `room` already when this
    /// device holds one, else a new one made in it. A workspace is its room
    /// and never becomes another, so joining never re-points the workspace
    /// in front. `None` for the nil room or a person's own room, which are
    /// never joined.
    ///
    /// Joining names no administrator; the invitation that led here pins it
    /// next (`accept_invite_into`, the native enrollment). Until then the
    /// room refuses every governance change rather than trusting whoever
    /// answers.
    pub fn join_room(&mut self, name: &str, room: RoomId) -> Option<WorkspaceId> {
        if room.is_nil() || is_local_person_room(room) {
            return None;
        }
        if let Some(held) = self.desktop.workspaces.iter().find(|w| w.room() == room) {
            return Some(held.id);
        }
        // Joining a room this device keeps archived brings its workspace back.
        if let Some(restored) = self.restore_archived(room) {
            return Some(restored);
        }
        Some(self.append_workspace_for_room(name, room))
    }

    /// Tests only: stand a workspace of `room` in for workspace `id`, under
    /// the same id and with everything it holds, as joining once re-pointed
    /// a workspace. Production never changes a workspace's room (a join
    /// makes the room's own workspace, [`Self::join_room`]); fixtures that
    /// model a workspace already sharing a room with another device use
    /// this. Made through the stored form, as a file from before would be.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn stand_in_room(&mut self, id: WorkspaceId, room: RoomId) -> bool {
        if room.is_nil() || is_local_person_room(room) {
            return false;
        }
        let joining = !self.desktop.workspaces.iter().any(|w| w.room() == room);
        let Some(current) = self.desktop.workspaces.iter_mut().find(|w| w.id == id) else {
            return false;
        };
        if current.room() == room {
            return false;
        }
        let Ok(mut stored) = serde_json::to_value(&*current) else {
            return false;
        };
        let Some(fields) = stored.as_object_mut() else {
            return false;
        };
        fields.insert(
            "room".to_owned(),
            serde_json::Value::String(room.to_string()),
        );
        let Ok(mut workspace) = serde_json::from_value::<Workspace>(stored) else {
            return false;
        };
        workspace.collaborators.clone_from(&current.collaborators);
        workspace.bots.clone_from(&current.bots);
        let surrendered = joining && workspace.surrender_governance();
        *current = workspace;
        let _ = surrendered;
        self.seed_doc_from_cache();
        self.queue_save();
        true
    }

    /// The id a new workspace for `room` gets: the room's own
    /// ([`WorkspaceId::of_room`]), the same on every device. Only when a
    /// workspace stored before already uses that number (a 64-bit
    /// coincidence) does it take a fresh local one, so two workspaces never
    /// share an id.
    pub(crate) fn new_workspace_id(&self, room: RoomId) -> WorkspaceId {
        let derived = WorkspaceId::of_room(room);
        if self
            .desktop
            .workspaces
            .iter()
            .chain(&self.archived_workspaces)
            .any(|w| w.id == derived)
            || self.workspace_aliases.contains_key(&derived)
        {
            self.mint_workspace_id()
        } else {
            derived
        }
    }

    /// Record a change made here to `workspace_id`'s policy, a sandbox
    /// capability this device's apps were given or lost, in this device's
    /// document so a restart reloads it, and ask the room's governor for it
    /// so the room carries it too.
    pub fn doc_write_policy(&mut self, workspace_id: WorkspaceId, edit: &PolicyEdit) {
        if edit.is_empty() {
            return;
        }
        for cap in &edit.sandbox_revoked {
            self.native_runtime
                .forget_files_home_consent(workspace_id, cap);
        }
        let Some(workspace) = self
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
        else {
            return;
        };
        let workspace = workspace.clone();
        edit.write(&mut self.doc, &workspace);
        self.commit_room_change();
        self.request_policy_changes(&workspace, edit);
    }

    /// Replicate a named grant set as workspace policy. The grants an
    /// assignment expands to replicate through [`Self::doc_write_policy`];
    /// this map lets another governor inspect and reuse the definition.
    pub fn doc_upsert_preset(&mut self, workspace_id: WorkspaceId, preset: &Preset) {
        if tilo_desktop_model::desktop_doc::upsert_preset(&mut self.doc, workspace_id, preset) {
            self.commit_room_change();
        }
    }

    /// Persist one terminal effect the host itself made, exactly once, with
    /// what it did. This creates semantic workspace activity, not a mutation
    /// in the target application's CRDT.
    pub fn append_workspace_effect_receipt(
        &mut self,
        workspace_id: WorkspaceId,
        invocation: tilo_actions::InvocationId,
        actor: &str,
        context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
    ) -> bool {
        self.append_workspace_effect_receipt_at(
            workspace_id,
            invocation,
            actor,
            context,
            status,
            unix_now_i64(),
        )
    }

    fn append_workspace_effect_receipt_at(
        &mut self,
        workspace_id: WorkspaceId,
        invocation: tilo_actions::InvocationId,
        actor: &str,
        context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
        completed_at: i64,
    ) -> bool {
        let receipt = tilo_desktop_model::WorkspaceEffectReceipt::recorded(
            invocation,
            actor,
            context,
            status,
            completed_at,
        );
        self.append_receipt(workspace_id, receipt)
    }

    /// Persist one terminal effect an application reports, exactly once:
    /// recorded without its arguments, which may carry private values.
    pub fn append_app_effect_receipt(
        &mut self,
        workspace_id: WorkspaceId,
        invocation: tilo_actions::InvocationId,
        actor: &str,
        context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
    ) -> bool {
        let receipt = tilo_desktop_model::WorkspaceEffectReceipt::redacted(
            invocation,
            actor,
            context,
            status,
            unix_now_i64(),
        );
        self.append_receipt(workspace_id, receipt)
    }

    fn append_receipt(
        &mut self,
        workspace_id: WorkspaceId,
        receipt: tilo_desktop_model::WorkspaceEffectReceipt,
    ) -> bool {
        let room = self
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .map(Workspace::room);
        // A hosted room's history is its document's: its host commits the
        // receipt, and it shows here until the document carries it.
        if let Some(room) = room.filter(|room| self.native_authority.holds(*room)) {
            return self.record_in_room(room, receipt);
        }
        let Some(receipt) = self
            .desktop
            .append_workspace_effect_receipt(workspace_id, receipt)
        else {
            return false;
        };
        self.doc_upsert_workspace_effect_receipt(workspace_id, &receipt);
        self.queue_save();
        true
    }

    /// Deterministic history clock for screenshot journeys that create
    /// several receipts inside one wall-clock second.
    #[cfg(feature = "hosted-test-control")]
    pub fn append_workspace_effect_receipt_for_shots(
        &mut self,
        workspace_id: WorkspaceId,
        invocation: tilo_actions::InvocationId,
        actor: &str,
        context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
    ) -> bool {
        let completed_at = self
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .and_then(|workspace| {
                workspace
                    .effect_receipts
                    .iter()
                    .map(|receipt| receipt.completed_at)
                    .max()
            })
            .unwrap_or_else(unix_now_i64)
            .saturating_add(1);
        self.append_workspace_effect_receipt_at(
            workspace_id,
            invocation,
            actor,
            context,
            status,
            completed_at,
        )
    }

    /// The device revocations the room document of `room` records, against
    /// which a saved shortcut's proof is judged each time it arms
    /// ([`tilo_peer_state::room::proven_saver`]). `None` when this device
    /// holds no document of the room: nothing saved there arms.
    #[must_use]
    pub fn room_revocations(&self, room: RoomId) -> Option<tilo_user_identity::RevocationList> {
        self.room_docs
            .get(&room)
            .map(tilo_peer_state::room::room_revocations)
    }

    /// Record that `claim.device` takes a fire time of one of the shortcuts
    /// saved in `room`, so the saver's other devices see it is taken. A
    /// claim on an earlier fire time of the same schedule is replaced;
    /// claims for shortcuts the workspace no longer holds are dropped.
    /// `false` when nothing changed.
    pub fn claim_schedule(&mut self, room: RoomId, claim: &tilo_automation::ScheduleClaim) -> bool {
        let Some(workspace) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.room() == room)
        else {
            return false;
        };
        if !workspace
            .shortcuts
            .iter()
            .any(|shortcut| shortcut.id == claim.shortcut)
        {
            return false;
        }
        let shortcuts = &workspace.shortcuts;
        workspace.schedule_claims.retain(|held| {
            shortcuts
                .iter()
                .any(|shortcut| shortcut.id == held.shortcut)
        });
        match workspace
            .schedule_claims
            .iter_mut()
            .find(|held| held.key() == claim.key())
        {
            Some(held) if claim.supersedes(held) => *held = claim.clone(),
            Some(_) => return false,
            None => workspace.schedule_claims.push(claim.clone()),
        }
        workspace
            .schedule_claims
            .sort_by_key(tilo_automation::ScheduleClaim::key);
        let workspace_id = workspace.id;
        if tilo_desktop_model::desktop_doc::upsert_schedule_claim(
            &mut self.doc,
            workspace_id,
            claim,
        ) {
            self.commit_room_change();
        }
        self.queue_save();
        true
    }

    fn doc_upsert_workspace_effect_receipt(
        &mut self,
        workspace_id: WorkspaceId,
        receipt: &WorkspaceEffectReceipt,
    ) {
        if tilo_desktop_model::desktop_doc::upsert_effect_receipt(
            &mut self.doc,
            workspace_id,
            receipt,
        ) {
            let _ = tilo_resource::history::commit_stamped_at_with_message(
                &mut self.doc,
                receipt.completed_at,
                receipt.context.change_message(),
            );
        }
    }

    /// Delete one tab from the replicated document, deleting its group as
    /// well when that tab was the group's last member. The in-memory tree is
    /// still the source of truth; this is the narrow document mirror used by
    /// user-driven close so a later merge cannot resurrect the closed tab.
    /// Whether the document holds what [`Self::doc_remove_tab`] would delete.
    pub(crate) fn doc_holds_tab(
        &self,
        strip_id: StripId,
        group_id: GroupId,
        tab_id: TabId,
        remove_group: bool,
    ) -> bool {
        tilo_desktop_model::desktop_doc::holds_tab(
            &self.doc,
            strip_id,
            group_id,
            tab_id,
            remove_group,
        )
    }

    pub(crate) fn doc_remove_tab(
        &mut self,
        strip_id: StripId,
        group_id: GroupId,
        tab_id: TabId,
        remove_group: bool,
    ) -> bool {
        tilo_desktop_model::desktop_doc::remove_tab(
            &mut self.doc,
            strip_id,
            group_id,
            tab_id,
            remove_group,
        )
    }

    /// Append a tab map to the active workspace's first strip's first
    /// window-group's `tabs`. No-op if the doc isn't seeded for the active
    /// workspace.
    pub fn doc_append_tab_to_first_group_of_active_workspace(
        &mut self,
        tab_id: u64,
        title: String,
        tile_blob: Vec<u8>,
    ) -> bool {
        tilo_desktop_model::desktop_doc::append_tab(
            &mut self.doc,
            self.desktop.active,
            None,
            tab_id,
            title,
            tile_blob,
        )
    }

    /// A first-boot tree: one empty workspace, nothing in it. What a
    /// machine with no `state.json` gets — no collaborators, window
    /// groups, or documents, since each would be a claim about someone
    /// who hasn't used the system yet. The workspace itself isn't
    /// fabrication: the compositor needs one to render into.
    ///
    /// For a populated tree, use [`Self::bootstrap_demo`].
    pub fn bootstrap_default() -> Self {
        Self::bootstrap_default_with_salt(0)
    }

    /// [`Self::bootstrap_default`] for the device whose salt is `salt`: its
    /// first workspace is minted in that device's namespace.
    pub fn bootstrap_default_with_salt(salt: u16) -> Self {
        let mut tree = Self::default();
        tree.set_peer_salt(salt);
        let (home, _) = tree.create_hosted_workspace("Home");
        tree.desktop.active = home;
        tree.seed_doc_from_cache();
        tree
    }

    /// A populated tree for demos, `--shots` scenarios and tests.
    /// Everything is invented, so it must never be reachable from a
    /// normal boot; production goes through [`Self::bootstrap_default`].
    pub fn bootstrap_demo() -> Self {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let group_id = next_id();
        let tab_notes = next_id();
        let tab_files = next_id();
        let tab_terminal = next_id();
        let tab_word = next_id();
        // Home opens the four in-process apps at `WindowWidth::Wide` so
        // the card fills more than half the viewport on a 1600 px-wide
        // first-run desktop — matches `WindowFrame.tsx`'s behaviour.
        let home = {
            let mut workspace = Workspace::new(ws_id, String::new());
            workspace.id = ws_id;
            workspace.name = "Home".into();
            workspace.strips = vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups: vec![WindowGroup {
                    id: group_id,
                    tabs: vec![
                        Tab {
                            id: tab_notes,
                            title: "Notes".into(),
                            tile: TileContent::AppPlaceholder {
                                kind: AppKind::Other("Notes".into()),
                                label: "Notes".into(),
                            },
                            app_title: None,
                        },
                        Tab {
                            id: tab_files,
                            title: "Files".into(),
                            tile: TileContent::AppPlaceholder {
                                kind: AppKind::FileExplorer,
                                label: "Files".into(),
                            },
                            app_title: None,
                        },
                        Tab {
                            id: tab_terminal,
                            title: "Terminal".into(),
                            tile: TileContent::AppPlaceholder {
                                kind: AppKind::Terminal,
                                label: "Terminal".into(),
                            },
                            app_title: None,
                        },
                        Tab {
                            id: tab_word,
                            title: "Document".into(),
                            tile: TileContent::AppPlaceholder {
                                kind: AppKind::Other("Word".into()),
                                label: "Document".into(),
                            },
                            app_title: None,
                        },
                    ],
                    active_tab: tab_notes,
                    width: WindowWidth::Wide,
                    maximized: false,
                    focus_history: Vec::new(),
                }],
            }];
            // One invented peer so the top-bar avatar stack has something
            // to paint. "me" is deliberately not seeded so
            // `draw_ws_settings` synthesizes its "You" tile from the
            // fallback path. Hex matches `tilo-concept/src/data.ts`.
            workspace.collaborators = vec![
                Collaborator::new("alice", "Alice")
                    .with_cursor_color("#ec4899")
                    .with_role("editor"),
            ];
            workspace
        };

        let mut tree = Self {
            desktop: DesktopTree {
                workspaces: vec![home],
                active: ws_id,
                active_strip: HashMap::from([(ws_id, strip_id)]),
                active_group: HashMap::from([(strip_id, group_id)]),
                ..DesktopTree::default()
            },
            ..Default::default()
        };

        // Research and Design add empty strips for the overview's 3-card
        // grid; Research also gets two invented collaborators to render.
        let research_id = tree.create_hosted_workspace("Research").0;
        let workspaces = &mut tree.desktop.workspaces;
        if let Some(ws) = workspaces.iter_mut().find(|w| w.id == research_id) {
            ws.collaborators.push(
                Collaborator::new("alice", "Alice")
                    .with_cursor_color("#ec4899")
                    .with_role("editor"),
            );
            ws.collaborators.push(
                Collaborator::new("bob", "Bob")
                    .with_cursor_color("#f97316")
                    .with_role("viewer"),
            );
        }
        tree.create_hosted_workspace("Design");

        // Home stays active — `create_hosted_workspace` doesn't touch
        // `tree.active`. This seeds the doc with the initial Home
        // workspace + active pointer the cache literal set directly.
        tree.seed_doc_from_cache();
        tree
    }

    /// Make a new workspace named `name`: the one way a workspace is made
    /// here. Its room is minted now, so a workspace that exists is a room
    /// that exists, and that room is its room for good. Its room waits among
    /// [`Self::pending_placements`] until the network creates it on the host
    /// it is placed on (this device's own by default) and enrolls this
    /// device's native owner in it. The workspace is in use from the start:
    /// it is this person's own room becoming hosted, never a join, so it
    /// neither says Joining… nor hides its windows meanwhile. Answers the
    /// workspace's id and its room.
    pub fn create_hosted_workspace(&mut self, name: &str) -> (WorkspaceId, RoomId) {
        let room = RoomId::new();
        self.pending_placements.insert(room);
        (self.append_workspace_for_room(name, room), room)
    }

    /// Claim this device's own rooms that no host holds yet: workspaces
    /// made here, or before rooms had hosts, that were not joined from
    /// elsewhere and that no enrollment holds. Each is its creator's, the
    /// person this device acts as in it (`identity_for_room(room)`), who is
    /// the one its host records as owner when it creates the room
    /// ([`Self::pending_placements`]), whose admitted document then says
    /// who owns it. Each such room waits to be placed on this device's host
    /// ([`Self::place_own_unhosted_rooms`]); until it is, the desktop works
    /// in it as its owner and its apps wait for it. Asked again when the
    /// person this device acts as changes (first-login setup), so a room
    /// claimed under the account's name belongs to the new identity.
    /// Changes no id and removes nothing. Answers the workspaces given an
    /// owner.
    pub fn claim_own_unhosted_rooms(
        &mut self,
        identity_for_room: impl Fn(&RoomId) -> UserId,
    ) -> Vec<WorkspaceId> {
        let mut owned = Vec::new();
        for workspace in &mut self.desktop.workspaces {
            let room = workspace.room();
            let joined_from_elsewhere =
                self.joining_rooms.contains(&room) && !self.pending_placements.contains(&room);
            if self.native_authority.requires(room) || joined_from_elsewhere {
                continue;
            }
            let creator = identity_for_room(&room);
            if workspace.owner() != Some(&creator) {
                workspace.set_owner(creator);
                owned.push(workspace.id);
            }
        }
        // The owner is what this device acts as, not something it learned:
        // it is claimed again on every start, and nothing is written for it.
        self.place_own_unhosted_rooms();
        owned
    }

    /// Give every ownerless workspace to `identity_for_room(room)`, as the
    /// projection of a room's admitted document does once its host created
    /// the room for its creator. For shots and tests that need an owned
    /// desktop without running a host; answers the workspaces given.
    #[cfg(any(test, feature = "hosted-test-control"))]
    pub fn own_workspaces_for_test(
        &mut self,
        identity_for_room: impl Fn(&RoomId) -> UserId,
    ) -> Vec<WorkspaceId> {
        let mut owned = Vec::new();
        for workspace in &mut self.desktop.workspaces {
            if workspace.owner().is_none() {
                workspace.set_owner(identity_for_room(&workspace.room()));
                owned.push(workspace.id);
            }
        }
        owned
    }

    /// Ask `room`'s governor for `request`. Nothing changes here until the
    /// room's admitted document carries it, but for a sandbox capability
    /// taken back ([`Self::narrow_sandbox`]), which stays taken back.
    pub fn request_governance(&mut self, room: RoomId, request: GovernanceRequest) {
        let takes_back = match &request {
            GovernanceRequest::RevokeSandbox(id) => {
                self.room_docs.get(&room).and_then(|document| {
                    projection::room_sandbox_of(document)
                        .into_iter()
                        .find(|capability| &capability.id == id)
                })
            }
            _ => None,
        };
        self.send_to_room(room, RoomChange::Governance(request), takes_back);
    }

    /// Take back the sandbox capability `id` of `workspace_id` as `revoker`:
    /// here at once, as narrowing this device's own reach never waits for
    /// anyone, and at the room's governor, so the room drops it too. It
    /// stays taken back here across every commit of the room until the
    /// room's document no longer carries it, even when the governor refuses
    /// ([`room_outbox`]). Whether anything was taken back here.
    pub fn narrow_sandbox(
        &mut self,
        workspace_id: WorkspaceId,
        revoker: &UserId,
        id: &str,
    ) -> bool {
        let Some((room, narrowed)) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
            .map(|workspace| {
                let narrowed = workspace
                    .revoke_sandbox_as_at(revoker, id, tilo_clock::unix_now())
                    .is_ok_and(|edit| !edit.is_empty());
                (workspace.room(), narrowed)
            })
        else {
            return false;
        };
        if narrowed {
            self.queue_save();
            self.request_governance(room, GovernanceRequest::RevokeSandbox(id.to_owned()));
        }
        narrowed
    }

    /// Ask the governor of `workspace_id`'s room for what `edit` changed in
    /// `edited`, a copy of the workspace the change was worked out on. Only
    /// a sandbox capability taken back changes here at once
    /// ([`Self::narrow_sandbox`]); everything else waits for the room's
    /// admitted document.
    pub fn request_policy_edit(
        &mut self,
        workspace_id: WorkspaceId,
        actor: &UserId,
        edited: &Workspace,
        edit: &PolicyEdit,
    ) {
        self.request_policy_changes(
            edited,
            &PolicyEdit {
                sandbox_revoked: Vec::new(),
                ..edit.clone()
            },
        );
        for id in &edit.sandbox_revoked {
            self.narrow_sandbox(workspace_id, actor, id);
        }
    }

    /// Queue for `edited`'s governor every change `edit` names, read from
    /// `edited`.
    fn request_policy_changes(&mut self, edited: &Workspace, edit: &PolicyEdit) {
        use tilo_action_policy::Change;
        let room = edited.room();
        let mut requests = Vec::new();
        for person in &edit.members {
            requests.push(GovernanceRequest::Policy(
                match edited.policy.members.get(person) {
                    Some(role) => Change::SetRole {
                        person: person.clone(),
                        role: *role,
                    },
                    None => Change::RemoveMember {
                        person: person.clone(),
                    },
                },
            ));
        }
        for id in &edit.granted {
            if let Some(grant) = edited.policy.grants.iter().find(|grant| &grant.id == id) {
                requests.push(GovernanceRequest::Policy(Change::Grant(grant.clone())));
            }
        }
        for id in &edit.revoked {
            requests.push(GovernanceRequest::Policy(Change::Revoke(id.clone())));
        }
        for id in &edit.sandbox_granted {
            if let Some(cap) = edited.sandbox.iter().find(|cap| &cap.id == id) {
                requests.push(GovernanceRequest::GrantSandbox(cap.clone()));
            }
        }
        for id in &edit.sandbox_revoked {
            requests.push(GovernanceRequest::RevokeSandbox(id.clone()));
        }
        for request in requests {
            self.request_governance(room, request);
        }
    }

    /// Ask the governor of `workspace_id`'s room for `changes` as `actor`,
    /// all or none, after working them out on a copy of the workspace: the
    /// room's policy as this device holds it refuses what its governor
    /// would. Nothing changes here until the room's admitted document
    /// carries the changes. What was asked for.
    ///
    /// # Errors
    /// The workspace is gone, or the first change the policy refuses.
    pub fn govern_in_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        actor: &UserId,
        changes: Vec<tilo_action_policy::Change>,
    ) -> Result<PolicyEdit, tilo_action_policy::Refusal> {
        let mut edited = self
            .desktop
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .cloned()
            .ok_or_else(|| tilo_action_policy::Refusal::NotPermitted("no such workspace".into()))?;
        let edit = edited.govern_all_at(actor, changes, tilo_clock::unix_now())?;
        self.request_policy_edit(workspace_id, actor, &edited, &edit);
        Ok(edit)
    }

    /// Ask the governor of `workspace_id`'s room to grant `grant`, given by
    /// `grantor`. Its id, or `None` when the room's policy refuses it.
    pub fn grant_in_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        grantor: &UserId,
        grant: tilo_action_policy::Grant,
    ) -> Option<tilo_action_policy::GrantId> {
        let id = grant.id.clone();
        self.govern_in_workspace(
            workspace_id,
            grantor,
            vec![tilo_action_policy::Change::Grant(grant)],
        )
        .ok()?;
        Some(id)
    }

    /// Ask the governor of `workspace_id`'s room to revoke the grant `id`
    /// as `revoker`. Whether there was one to revoke that the room's policy
    /// lets `revoker` take back.
    pub fn revoke_in_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        revoker: &UserId,
        id: &tilo_action_policy::GrantId,
    ) -> bool {
        self.govern_in_workspace(
            workspace_id,
            revoker,
            vec![tilo_action_policy::Change::Revoke(id.clone())],
        )
        .is_ok_and(|edit| !edit.is_empty())
    }

    /// Apply `changes` to `workspace_id`'s policy here as `actor`, as the
    /// room's admitted document would once its governor took them: for
    /// tests and staged scenes, which have no governor.
    #[cfg(any(test, feature = "hosted-test-control"))]
    pub fn govern_here_for_test(
        &mut self,
        workspace_id: WorkspaceId,
        actor: &UserId,
        changes: Vec<tilo_action_policy::Change>,
    ) -> Result<PolicyEdit, tilo_action_policy::Refusal> {
        let edit = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| tilo_action_policy::Refusal::NotPermitted("no such workspace".into()))?
            .govern_all_at(actor, changes, tilo_clock::unix_now())?;
        // An admitted projection reaches the authority exports.
        self.export_fs_capabilities();
        Ok(edit)
    }

    /// Make `person` a member of `workspace_id` holding `role` here, as
    /// [`Self::govern_here_for_test`] does.
    #[cfg(any(test, feature = "hosted-test-control"))]
    pub fn admit_here_for_test(
        &mut self,
        workspace_id: WorkspaceId,
        actor: &UserId,
        person: &str,
        role: CapRole,
    ) -> Option<PolicyEdit> {
        let member = self
            .desktop
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)?
            .role_of(person)
            .is_some();
        let person = person.to_owned();
        let change = if member {
            tilo_action_policy::Change::SetRole { person, role }
        } else {
            tilo_action_policy::Change::AddMember { person, role }
        };
        self.govern_here_for_test(workspace_id, actor, vec![change])
            .ok()
    }

    /// Record the sandbox capability `cap` in `workspace_id` here, given by
    /// `grantor`, as [`Self::govern_here_for_test`] does. Its id, or `None`
    /// when the room's policy refuses it.
    #[cfg(any(test, feature = "hosted-test-control"))]
    pub fn grant_sandbox_here_for_test(
        &mut self,
        workspace_id: WorkspaceId,
        grantor: &UserId,
        cap: CapOverride,
    ) -> Option<String> {
        let granted = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)?
            .grant_sandbox_as_at(grantor, cap, tilo_clock::unix_now())
            .ok()?
            .sandbox_granted
            .first()
            .cloned();
        self.export_fs_capabilities();
        granted
    }

    /// Take the policy changes asked of governors as their governor would,
    /// made by `actor`, and apply them here as the rooms' admitted
    /// documents would carry them: for tests, which have no governor.
    #[cfg(test)]
    pub(crate) fn govern_requests_here_for_test(&mut self, actor: &UserId) {
        use tilo_desktop::workspace_settings::GovernanceOperation as Operation;
        use tilo_desktop_model::commands::DesktopCommand;
        let now = tilo_clock::unix_now();
        for (room, request) in self.take_governance_requests() {
            let Some(workspace) = self
                .desktop
                .workspaces
                .iter_mut()
                .find(|w| w.room() == room)
            else {
                continue;
            };
            let operation = match request {
                GovernanceRequest::Policy(change) => {
                    workspace
                        .govern_as_at(actor, change, now)
                        .expect("the governor takes the change");
                    continue;
                }
                GovernanceRequest::Command(DesktopCommand::RenameWorkspace { name }) => {
                    workspace.name = name;
                    continue;
                }
                GovernanceRequest::Command(DesktopCommand::SavePreset { preset_json }) => {
                    let preset: tilo_action_policy::Preset =
                        serde_json::from_str(&preset_json).expect("a preset");
                    Operation::SavePreset {
                        name: preset.name,
                        description: preset.description,
                        rules: preset.rules,
                    }
                }
                GovernanceRequest::Command(DesktopCommand::AssignPreset {
                    preset,
                    subject_kind,
                    subject,
                    expires_unix,
                }) => Operation::AssignPreset {
                    preset,
                    subject: match subject_kind.as_str() {
                        "app" => tilo_action_policy::Subject::app(subject),
                        "agent" => tilo_action_policy::Subject::agent(subject),
                        _ => tilo_action_policy::Subject::person(subject),
                    },
                    expires_unix,
                },
                GovernanceRequest::Command(DesktopCommand::GrantAppAction {
                    app,
                    target_app,
                    resource,
                    action,
                    lifetime,
                }) => Operation::GrantAppAction {
                    app,
                    target_app,
                    resource,
                    action,
                    lifetime,
                },
                GovernanceRequest::Command(DesktopCommand::RevokeAppAction {
                    app,
                    target_app,
                    resource,
                    action,
                }) => Operation::RevokeAppAction {
                    app,
                    target_app,
                    resource,
                    action,
                },
                GovernanceRequest::GrantSandbox(cap) => {
                    workspace
                        .grant_sandbox_as_at(actor, cap, now)
                        .expect("the governor takes the capability");
                    continue;
                }
                GovernanceRequest::RevokeSandbox(id) => {
                    workspace
                        .revoke_sandbox_as_at(actor, &id, now)
                        .expect("the governor takes the capability back");
                    continue;
                }
                GovernanceRequest::Command(_) => continue,
            };
            tilo_desktop::workspace_settings::apply_governance(workspace, actor, operation, now)
                .expect("the governor takes the change");
        }
    }

    /// Whether a governance change waits to be asked of its governor.
    #[cfg(test)]
    pub fn has_governance_requests(&self) -> bool {
        self.outbox_to_ask()
            .any(|entry| matches!(entry.change, RoomChange::Governance(_)))
    }

    /// The governance changes made here and not asked yet, taken out of
    /// the outbox in the order they were made: for tests, which answer
    /// them as the governor would.
    #[cfg(test)]
    pub fn take_governance_requests(&mut self) -> Vec<(RoomId, GovernanceRequest)> {
        let mut taken = Vec::new();
        self.room_outbox
            .retain(|entry| match (&entry.change, entry.standing) {
                (RoomChange::Governance(request), Standing::Asking) => {
                    taken.push((entry.room, request.clone()));
                    false
                }
                _ => true,
            });
        taken
    }

    /// `room`'s governor refused `request`, a change not in the outbox, for
    /// `reason` ([`Self::refuse_room_change`]).
    pub fn refuse_governance(&mut self, room: RoomId, request: GovernanceRequest, reason: String) {
        self.refuse_room_change(room, RoomChange::Governance(request), reason);
    }

    /// The refusals of governance changes made here, taken.
    pub fn take_governance_refusals(&mut self) -> Vec<GovernanceRefusal> {
        std::mem::take(&mut self.governance_refusals)
    }

    /// Whether a room waits to be placed that no workspace is any longer:
    /// placing it would make a room nothing shows. Such an entry is
    /// bookkeeping, never what the person made.
    pub fn has_orphaned_placements(&self) -> bool {
        self.pending_placements
            .iter()
            .any(|room| !self.has_workspace(*room))
    }

    /// Stop waiting to place each room no workspace is any longer
    /// ([`Self::has_orphaned_placements`]). Answers the rooms dropped.
    pub fn drop_orphaned_placements(&mut self) -> Vec<RoomId> {
        let orphaned: Vec<RoomId> = self
            .pending_placements
            .iter()
            .filter(|room| !self.has_workspace(**room))
            .copied()
            .collect();
        for room in &orphaned {
            self.pending_placements.remove(room);
        }
        if !orphaned.is_empty() {
            self.queue_save();
        }
        orphaned
    }

    /// The rooms of workspaces made here that wait to be created on a host,
    /// taken: the network places each and enrolls this device in it. Saved,
    /// so a placement taken is not taken again after a restart.
    pub fn take_pending_placements(&mut self) -> BTreeSet<RoomId> {
        let taken = std::mem::take(&mut self.pending_placements);
        if !taken.is_empty() {
            self.queue_save();
        }
        taken
    }

    /// Forget each mark that a room is being joined with no enrollment into
    /// it in flight: the person's continuity once marked the rooms it named
    /// without entering them anywhere, and a mark nothing is entering would
    /// keep the room from ever being entered. The mark is bookkeeping;
    /// the room's workspace stays as it is. Answers the rooms whose mark
    /// went.
    pub fn drop_unentered_joins(&mut self) -> Vec<RoomId> {
        let entering: BTreeSet<RoomId> = self
            .native_authority
            .joins()
            .map(|(room, _)| room)
            .collect();
        let unentered: Vec<RoomId> = self
            .joining_rooms
            .iter()
            .filter(|room| !entering.contains(room))
            .copied()
            .collect();
        for room in &unentered {
            self.joining_rooms.remove(room);
        }
        if !unentered.is_empty() {
            self.queue_save();
        }
        unentered
    }

    /// Append a workspace named `name` showing `room`.
    fn append_workspace_for_room(&mut self, name: &str, room: RoomId) -> WorkspaceId {
        let ws_id = self.new_workspace_id(room);
        let strip_id = self.mint_strip_id();
        let mut ws = Workspace::in_room(ws_id, room, name);
        ws.strips = vec![Strip {
            id: strip_id,
            name: "Main".into(),
            window_groups: vec![],
        }];
        self.desktop.active_strip.insert(ws_id, strip_id);
        self.desktop.workspaces.push(ws);

        // Mirror the append into the doc so peer-sync sees a discrete change.
        // Best-effort, same as `seed_doc_from_cache`: the cache push above
        // already happened, so a doc-write failure only means this append
        // doesn't replicate.
        tilo_desktop_model::desktop_doc::append_workspace(&mut self.doc, ws_id, name, room);
        // Its strip goes in with it: a window moved or opened into the new
        // workspace is recorded against that strip, and a move the document
        // cannot record is refused.
        self.seed_doc_from_cache();
        // A newly-created workspace is local session state as well as a
        // collaboration-document change. Persist it at the mutation boundary:
        // otherwise a compositor crash before the next unrelated edit silently
        // loses the room the person just created.
        self.queue_save();
        ws_id
    }

    /// Hold `room` as one of the person's workspaces, named `name`: the
    /// person's workspace list says they are in it. A room this tree lacks
    /// is joined without focus or layout, its workspace joining
    /// ([`Self::join_native_room`]) until the room's replica installs; one
    /// already here takes the listed name. Who
    /// owns and governs the room is the room's own governance, projected
    /// with its replica. Returns whether the tree changed.
    pub fn adopt_personal_room(&mut self, room: RoomId, name: &str) -> bool {
        if room.is_nil() || is_local_person_room(room) {
            return false;
        }
        if let Some(index) = self
            .desktop
            .workspaces
            .iter()
            .position(|workspace| workspace.room() == room)
        {
            let Some(workspace) = self.desktop.workspaces.get_mut(index) else {
                return false;
            };
            if workspace.name == name {
                return false;
            }
            workspace.name = name.to_string();
            self.seed_doc_from_cache();
            self.desktop.room_epoch = self.desktop.room_epoch.wrapping_add(1);
            self.queue_save();
            return true;
        }
        let id = self.new_workspace_id(room);
        let workspace = Workspace::in_room(id, room, name);
        let first = self.desktop.workspaces.is_empty();
        self.desktop.workspaces.push(workspace);
        if first {
            self.desktop.active = id;
        }
        if !self.native_authority.holds(room) {
            self.joining_rooms.insert(room);
        }
        self.seed_doc_from_cache();
        self.desktop.room_epoch = self.desktop.room_epoch.wrapping_add(1);
        self.queue_save();
        true
    }

    /// Take `room`'s workspace off this device: the person deleted it on
    /// another of their devices, or keeps it off this one. Only this
    /// device's copy goes; the room and its content stay with its other
    /// members. If it was showing, the neighbouring workspace shows
    /// instead. Returns its place and value, for an undo.
    pub fn forget_personal_room(&mut self, room: RoomId) -> Option<(usize, Workspace)> {
        if let Some(archived) = self
            .archived_workspaces
            .iter()
            .position(|workspace| workspace.room() == room)
        {
            // An archived workspace goes with its room as a shown one does;
            // it is no place in the list to put back.
            let removed = self.archived_workspaces.remove(archived);
            self.room_docs.remove(&room);
            self.joining_rooms.remove(&room);
            self.pending_placements.remove(&room);
            self.desktop.room_epoch = self.desktop.room_epoch.wrapping_add(1);
            self.queue_save();
            drop(removed);
            return None;
        }
        let index = self
            .desktop
            .workspaces
            .iter()
            .position(|workspace| workspace.room() == room)?;
        let removed = self.desktop.workspaces.remove(index);
        if self.desktop.active == removed.id {
            let next = self
                .desktop
                .workspaces
                .get(index)
                .or_else(|| {
                    index
                        .checked_sub(1)
                        .and_then(|i| self.desktop.workspaces.get(i))
                })
                .map_or_else(WorkspaceId::default, |workspace| workspace.id);
            self.desktop.active = next;
        }
        self.desktop.active_strip.remove(&removed.id);
        for strip in &removed.strips {
            self.desktop.active_group.remove(&strip.id);
        }
        // Who governs the room and where this device's share of it starts
        // stay recorded, so putting the workspace back (an undo, or the
        // person showing it here again) does not make this device claim a
        // room another device governs.
        self.room_docs.remove(&room);
        self.joining_rooms.remove(&room);
        self.pending_placements.remove(&room);
        self.seed_doc_from_cache();
        self.desktop.room_epoch = self.desktop.room_epoch.wrapping_add(1);
        self.queue_save();
        Some((index, removed))
    }

    /// Delete workspace `id` from this person's desktop, on this device and,
    /// through the person's workspace list, on their other devices. The last
    /// workspace stays. Returns its place and value, for an undo.
    ///
    /// A workspace the native owner holds or is joining is not deleted here:
    /// its room goes with it through the owner
    /// (`super::native_workspace_owner::release_workspace_room`), so a
    /// workspace and its room never part.
    pub fn delete_workspace(&mut self, id: WorkspaceId) -> Option<(usize, Workspace)> {
        if self.desktop.workspaces.len() < 2 {
            return None;
        }
        let room = self
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)?
            .room();
        if self.native_authority.requires(room)
            || self.joining_rooms.contains(&room)
            || self.pending_placements.contains(&room)
        {
            return None;
        }
        let removed = self.forget_personal_room(room);
        self.desktop.external_revision = self.desktop.external_revision.wrapping_add(1);
        removed
    }

    /// Put a deleted workspace back where it was. Its room document is
    /// rebuilt from this copy and from the room's other members.
    pub fn restore_workspace(&mut self, index: usize, workspace: Workspace) -> bool {
        if self
            .desktop
            .workspaces
            .iter()
            .any(|existing| existing.room() == workspace.room() || existing.id == workspace.id)
        {
            return false;
        }
        let index = index.min(self.desktop.workspaces.len());
        let id = workspace.id;
        self.desktop.workspaces.insert(index, workspace);
        self.desktop.active = id;
        self.place_own_unhosted_rooms();
        self.seed_doc_from_cache();
        self.desktop.external_revision = self.desktop.external_revision.wrapping_add(1);
        self.queue_save();
        true
    }

    /// Move workspace `id` one place earlier (`-1`) or later (`1`) in the
    /// list. Returns whether it moved.
    pub fn move_workspace(&mut self, id: WorkspaceId, step: i32) -> bool {
        let Some(index) = self.desktop.workspaces.iter().position(|w| w.id == id) else {
            return false;
        };
        let target = match step.signum() {
            -1 => index.checked_sub(1),
            1 => Some(index.saturating_add(1)).filter(|t| *t < self.desktop.workspaces.len()),
            _ => None,
        };
        let Some(target) = target else {
            return false;
        };
        self.desktop.workspaces.swap(index, target);
        self.seed_doc_from_cache();
        self.desktop.external_revision = self.desktop.external_revision.wrapping_add(1);
        self.queue_save();
        true
    }

    /// Order the workspaces of `rooms` as listed, in the places they already
    /// take among this tree's workspaces; every other workspace stays where
    /// it is. Returns whether anything moved.
    pub fn arrange_rooms(&mut self, rooms: &[RoomId]) -> bool {
        let slots: Vec<usize> = self
            .desktop
            .workspaces
            .iter()
            .enumerate()
            .filter(|(_, workspace)| rooms.contains(&workspace.room()))
            .map(|(index, _)| index)
            .collect();
        let mut wanted: Vec<Workspace> = Vec::with_capacity(slots.len());
        for room in rooms {
            if let Some(workspace) = self.desktop.workspaces.iter().find(|w| w.room() == *room) {
                wanted.push(workspace.clone());
            }
        }
        if wanted.len() != slots.len() {
            return false;
        }
        let mut moved = false;
        for (slot, workspace) in slots.into_iter().zip(wanted) {
            if let Some(existing) = self.desktop.workspaces.get_mut(slot)
                && existing.room() != workspace.room()
            {
                *existing = workspace;
                moved = true;
            }
        }
        if moved {
            self.seed_doc_from_cache();
            self.desktop.room_epoch = self.desktop.room_epoch.wrapping_add(1);
            self.queue_save();
        }
        moved
    }

    /// Turn a synchronized offer into a locally-owned live app. The Wayland
    /// toplevel adoption handshake is the only caller: merely receiving or
    /// painting an offer must not make this device echo it.
    pub fn claim_continuation_tab(&mut self, tab: TabId) -> bool {
        if !self.continuation_tabs.remove(&tab) {
            return false;
        }
        self.continuation_file_references.remove(&tab);
        if let Some(room) = self.desktop.workspaces.iter().find_map(|workspace| {
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .any(|candidate| candidate.id == tab)
                .then_some(workspace.room())
        }) {
            self.continuation_closed_tabs.remove(&tab);
            self.continuation_owned_tabs.insert(tab, room);
        }
        true
    }

    /// Materialize another device's continuation as synchronized placeholder
    /// tabs, titled by the session's `labels`. This never activates a
    /// workspace, strip, or group; the compositor opens the matching local
    /// apps through its normal authorized launch path.
    #[allow(
        clippy::too_many_lines,
        clippy::indexing_slicing,
        reason = "continuation materialization grows strips and groups until the validated placement indices exist before indexing them"
    )]
    pub fn materialize_continuation_workspace(
        &mut self,
        _source_device: &str,
        continuation: &crate::shell::personal_continuity::WorkspaceContinuation,
        labels: &dyn ContinuationLabels,
    ) -> bool {
        let salt = self.local_salt();
        let existing_continuation_tabs = self.continuation_tabs.clone();
        let (changed, file_references, file_reference_removals, inserted_continuation_tabs) = {
            let mut file_references = Vec::new();
            let mut file_reference_removals = Vec::new();
            let mut inserted_continuation_tabs = Vec::new();
            let Some(workspace) = self
                .desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.room() == continuation.room)
            else {
                return false;
            };
            let mut changed = false;
            let mut apps = continuation.apps.clone();
            apps.sort_by_key(|app| (app.placement.strip, app.placement.group, app.placement.tab));
            for app in apps {
                let (title, tile_content) = match &app.launch {
                    crate::shell::personal_continuity::LaunchIntent::App { app_id } => {
                        // A newer app winner may reuse the durable tab id of
                        // an earlier file-reference card.  Do not let the
                        // old reference override capture after that change.
                        file_reference_removals.push(app.id.0);
                        if let Some(resume) = app.resume.clone()
                            && let Ok(mut registry) =
                                crate::shell::personal_continuity_registry::global_registry().lock()
                        {
                            registry.store_remote_resume(
                                continuation.room,
                                app.id,
                                app_id.clone(),
                                resume,
                                app.record_instance.unwrap_or(app.id),
                            );
                        } else if let Ok(mut registry) =
                            crate::shell::personal_continuity_registry::global_registry().lock()
                        {
                            registry.store_remote_record(
                                continuation.room,
                                app.id,
                                app_id.clone(),
                                app.record_instance.unwrap_or(app.id),
                            );
                        }
                        (
                            // Built on the compositor loop: the installed
                            // apps as last enumerated, never a registry read.
                            labels.app_name(app_id).unwrap_or_else(|| app_id.clone()),
                            TileContent::PeerWindow {
                                app_id: app_id.clone(),
                                label: "Available on another device".to_string(),
                                note: String::new(),
                                missing: None,
                            },
                        )
                    }
                    crate::shell::personal_continuity::LaunchIntent::FileReference { .. } => {
                        file_references.push((app.id.0, app.launch.clone()));
                        (
                            labels.file_reference_label(&app.launch),
                            TileContent::Stub {
                                kind: AppKind::FileExplorer,
                                label: "Available on another device".to_string(),
                            },
                        )
                    }
                };
                let at = workspace
                    .strips
                    .iter()
                    .enumerate()
                    .find_map(|(strip_index, strip)| {
                        strip
                            .window_groups
                            .iter()
                            .enumerate()
                            .find_map(|(group_index, group)| {
                                let index = group.tabs.iter().position(|tab| tab.id == app.id.0)?;
                                Some((strip_index, group_index, index, group))
                            })
                    });
                let exists = at.is_some();
                if exists && !existing_continuation_tabs.contains(&app.id.0) {
                    continue;
                }
                // Every checkpoint offers every card again: one already
                // projected as offered stays, so an offer that moved nothing
                // changes nothing.
                if let Some((strip_index, group_index, index, group)) = at
                    && strip_index == app.placement.strip as usize
                    && group_index == app.placement.group as usize
                    && index == (app.placement.tab as usize).min(group.tabs.len().saturating_sub(1))
                    && group.tabs[index].title == title
                    && group.tabs[index].tile == tile_content
                {
                    continue;
                }
                // An offer is a projection of its source placement, not a
                // one-time insertion. Remove the old projected card before
                // inserting it at the latest ordinal coordinates. Locally
                // claimed tabs are excluded above and never moved remotely.
                if exists {
                    for strip in &mut workspace.strips {
                        for group in &mut strip.window_groups {
                            group.tabs.retain(|tab| tab.id != app.id.0);
                            if group.active_tab == app.id.0 {
                                group.active_tab = group.tabs.first().map_or(0, |tab| tab.id);
                            }
                        }
                    }
                }
                while workspace.strips.len() <= app.placement.strip as usize {
                    workspace
                        .strips
                        .push(Strip::new(StripId(mint_under(salt)), "Available elsewhere"));
                }
                let strip = &mut workspace.strips[app.placement.strip as usize];
                while strip.window_groups.len() <= app.placement.group as usize {
                    strip.window_groups.push(WindowGroup {
                        id: mint_under(salt),
                        tabs: Vec::new(),
                        active_tab: 0,
                        width: WindowWidth::Normal,
                        maximized: false,
                        focus_history: Vec::new(),
                    });
                }
                let group = &mut strip.window_groups[app.placement.group as usize];
                let tab = Tab {
                    id: app.id.0,
                    title,
                    tile: tile_content,
                    app_title: None,
                };
                if !existing_continuation_tabs.contains(&app.id.0) {
                    inserted_continuation_tabs.push(app.id.0);
                }
                let index = (app.placement.tab as usize).min(group.tabs.len());
                group.tabs.insert(index, tab);
                if group.active_tab == 0 {
                    // This only defines the group's internal default when the
                    // person later enters it; it does not touch tree focus maps.
                    group.activate_tab(app.id.0);
                }
                changed = true;
            }
            (
                changed,
                file_references,
                file_reference_removals,
                inserted_continuation_tabs,
            )
        };
        self.continuation_tabs.extend(inserted_continuation_tabs);
        for tab in file_reference_removals {
            self.continuation_file_references.remove(&tab);
        }
        for (tab, reference) in file_references {
            self.continuation_file_references.insert(tab, reference);
        }
        if changed {
            self.seed_doc_from_cache();
            self.queue_save();
        }
        changed
    }

    /// Remove continuation-owned tabs absent from the current deterministic
    /// winner set. Local user-created tabs are deliberately unmarked and are
    /// never touched by this reconciliation.
    pub fn reconcile_continuation_tabs(&mut self, desired: &BTreeSet<TabId>) -> bool {
        let stale: BTreeSet<_> = self
            .continuation_tabs
            .difference(desired)
            .copied()
            .collect();
        if stale.is_empty() {
            return false;
        }
        for workspace in &mut self.desktop.workspaces {
            for strip in &mut workspace.strips {
                for group in &mut strip.window_groups {
                    group.tabs.retain(|tab| !stale.contains(&tab.id));
                    if stale.contains(&group.active_tab) {
                        group.active_tab = group.tabs.first().map_or(0, |tab| tab.id);
                    }
                }
            }
        }
        self.continuation_tabs.retain(|tab| !stale.contains(tab));
        self.continuation_file_references
            .retain(|tab, _| !stale.contains(tab));
        if let Ok(mut registry) =
            crate::shell::personal_continuity_registry::global_registry().lock()
        {
            for tab in &stale {
                registry.remove_remote_tab(crate::shell::personal_continuity::AppInstanceId(*tab));
            }
        }
        self.seed_doc_from_cache();
        self.queue_save();
        true
    }

    pub fn set_active(&mut self, id: WorkspaceId) {
        if self.desktop.workspaces.iter().any(|w| w.id == id) {
            self.desktop.active = id;
            tilo_desktop_model::desktop_doc::set_active(&mut self.doc, id);
            // Workspace selection is user-visible session state, not merely
            // a render hint. Persist it immediately so a compositor restart
            // opens the workspace the user selected rather than the one that
            // happened to be active at the last unrelated mutation.
            self.queue_save();
        }
    }

    pub fn switch_up(&mut self) -> bool {
        let idx = self
            .desktop
            .workspaces
            .iter()
            .position(|w| w.id == self.desktop.active)
            .unwrap_or(0);
        let Some(prev_idx) = idx.checked_sub(1) else {
            return false;
        };
        let Some(prev) = self.desktop.workspaces.get(prev_idx) else {
            return false;
        };
        self.desktop.active = prev.id;
        tilo_desktop_model::desktop_doc::set_active(&mut self.doc, self.desktop.active);
        self.queue_save();
        true
    }

    pub fn switch_down(&mut self) -> bool {
        let idx = self
            .desktop
            .workspaces
            .iter()
            .position(|w| w.id == self.desktop.active)
            .unwrap_or(0);
        let Some(next) = self.desktop.workspaces.get(idx.saturating_add(1)) else {
            return false;
        };
        self.desktop.active = next.id;
        tilo_desktop_model::desktop_doc::set_active(&mut self.doc, self.desktop.active);
        self.queue_save();
        true
    }

    // Active-group pointer: one [`GroupId`] per [`StripId`], the
    // receiver of focus/keyboard/presence. Closing the active group's
    // last tab latches a 180 ms shrink before pruning, so
    // [`Self::nudge_active_group_off_closing`] advances the pointer to
    // the next surviving sibling at fade-start rather than letting it
    // resolve to the fading group for that whole window.

    /// Mirror a cross-strip tab drag-drop into the persisted tree. The
    /// widget side only mutates the widget tree — without this writeback
    /// the tab would pop back to its source strip on the next restart.
    /// The cache edit is [`DesktopTree::move_tab_across_strips`]; the room
    /// document follows, and both roll back together if it refuses.
    ///
    /// Forgiving: `false` if `tab_id` or either strip id doesn't resolve.
    pub fn move_tab_across_strips(
        &mut self,
        src_strip_id: StripId,
        dest_strip_id: StripId,
        tab_id: TabId,
        new_group_id: GroupId,
        target: DropTarget,
    ) -> bool {
        // Cache and Automerge document form one transaction. Keep snapshots
        // so a stale/malformed document cannot leave the live cache moved
        // while replication still describes the old layout.
        let before_workspaces = self.desktop.workspaces.clone();
        let before_doc = self.doc.clone();
        // The move hands the source strip's focus on when the window's group
        // leaves with it; a refused move must leave that focus where it was.
        let before_focus = self.desktop.active_group.get(&src_strip_id).copied();
        let Some(moved) = self.desktop.move_tab_across_strips(
            src_strip_id,
            dest_strip_id,
            tab_id,
            new_group_id,
            target,
        ) else {
            return false;
        };
        if !tilo_desktop_model::desktop_doc::apply_tab_move(
            &mut self.doc,
            src_strip_id,
            dest_strip_id,
            &moved,
        ) {
            tracing::warn!(
                tab_id,
                src_strip_id = src_strip_id.raw(),
                dest_strip_id = dest_strip_id.raw(),
                "cross-strip move could not be mirrored into the room document"
            );
            self.desktop.workspaces = before_workspaces;
            self.doc = before_doc;
            match before_focus {
                Some(group) => {
                    self.desktop.active_group.insert(src_strip_id, group);
                }
                None => {
                    self.desktop.active_group.remove(&src_strip_id);
                }
            }
            return false;
        }
        self.queue_save();
        true
    }

    /// Split `tab_id` out of its group into a brand-new single-tab group
    /// at `insert_at` in the same strip — the tree-first form of the
    /// in-strip drag-out and of the tab menu's "Split Right" / "Move to
    /// New Group". The cache edit is
    /// [`DesktopTree::split_tab_into_new_group`]; the document follows
    /// through [`tilo_desktop_model::desktop_doc::apply_tab_move`] with the strip as both
    /// source and destination, so a revision from another device cannot
    /// resurrect the tab in its old group.
    ///
    /// Returns the new group's id. `None`, with nothing changed, when the
    /// tab is not in `strip_id`, when its group has no other tab, or when
    /// the document refuses the move.
    pub fn split_tab_into_new_group(
        &mut self,
        strip_id: StripId,
        tab_id: TabId,
        insert_at: usize,
    ) -> Option<GroupId> {
        let before_workspaces = self.desktop.workspaces.clone();
        let before_doc = self.doc.clone();
        let mut ids = self.ids();
        let moved = self
            .desktop
            .split_tab_into_new_group(strip_id, tab_id, insert_at, &mut ids)?;
        if !tilo_desktop_model::desktop_doc::apply_tab_move(
            &mut self.doc,
            strip_id,
            strip_id,
            &moved,
        ) {
            tracing::warn!(
                tab_id,
                strip_id = strip_id.raw(),
                "in-strip split could not be mirrored into the room document"
            );
            self.desktop.workspaces = before_workspaces;
            self.doc = before_doc;
            return None;
        }
        self.queue_save();
        Some(moved.new_group.id)
    }

    /// Replace one strip's group/tab arrangement with the identity-only shape
    /// reported by its live `GroupRow`. App tiles remain sourced from this
    /// tree, so widget code cannot overwrite persisted content while reporting
    /// a same-strip drag result.
    pub fn reconcile_strip_topology(
        &mut self,
        strip_id: StripId,
        topology: &[tilo_desktop_model::GroupShape],
    ) -> bool {
        let Some(persisted) = self.desktop.reconcile_strip_topology(strip_id, topology) else {
            return false;
        };
        self.doc_rewrite_strip_groups(strip_id, &persisted);
        // Same-strip drag/split changes are user-visible layout state. Keep
        // the local restart snapshot in step with the cache and room doc;
        // callers may have no later mutation before a crash or shutdown.
        self.queue_save();
        true
    }

    /// Rewrite only the affected strip's group list in the collaboration
    /// document. Unlike `seed_doc_from_cache`, this preserves concurrent
    /// changes elsewhere in the workspace tree.
    fn doc_rewrite_strip_groups(&mut self, strip_id: StripId, groups: &[WindowGroup]) {
        tilo_desktop_model::desktop_doc::rewrite_strip_groups(&mut self.doc, strip_id, groups);
    }

    /// Bring this device's display partition in line with the displays
    /// now connected, primary first (see [`OutputPartition::reconcile`]).
    /// A partition is kept once a second display has been seen; a device
    /// that only ever had one display carries none. Returns whether the
    /// partition changed. The partition is this device's alone: it never
    /// touches the strips' shared order.
    pub fn reconcile_displays(&mut self, displays: &[Display]) -> bool {
        if displays.len() < 2 && self.desktop.outputs.is_empty() {
            return false;
        }
        let before = self.desktop.outputs.clone();
        let desktop = &mut self.desktop;
        desktop.outputs.reconcile(&desktop.workspaces, displays);
        desktop.outputs.place_unhomed(&desktop.workspaces);
        let changed = self.desktop.outputs != before;
        if changed {
            self.queue_save();
        }
        changed
    }

    /// Show `group_id` on display `display` from now on, at its place in
    /// the strip's shared order (see [`OutputPartition::move_group`]).
    /// `false` when there is no such display or the group is already there.
    pub fn move_group_to_display(&mut self, group_id: GroupId, display: u32) -> bool {
        if self.desktop.outputs.display_of(group_id) == Some(display) {
            return false;
        }
        let desktop = &mut self.desktop;
        if !desktop
            .outputs
            .move_group(&desktop.workspaces, group_id, display)
        {
            return false;
        }
        self.queue_save();
        true
    }

    /// Show `group_id` of `strip_id` on display `display` from now on,
    /// where the person dropped it; its place among that display's groups
    /// is the caller's reorder (see [`OutputPartition::rehome`]). `false`
    /// when it is already there or there is no such display.
    pub fn rehome_group(&mut self, strip_id: StripId, group_id: GroupId, display: u32) -> bool {
        if self.desktop.outputs.display_of(group_id) == Some(display)
            || !self.desktop.outputs.rehome(strip_id, group_id, display)
        {
            return false;
        }
        self.queue_save();
        true
    }

    /// Whether this machine may rename `strip`: it is in this device's id
    /// namespace (the single writer that projects it to the room), or in no
    /// namespace at all (nothing projects it, so the name stays local). A
    /// peer's strip is theirs to rename; the owner's admission
    /// ([`tilo_desktop_model::commands::check_owned`]) refuses it the same way.
    pub fn may_rename_strip(&self, strip: StripId) -> bool {
        let salt = tilo_desktop_model::ids::peer_salt_of(strip.raw());
        salt == 0 || salt == self.local_salt()
    }

    /// Create, rename or move a strip of the active workspace, as a person at
    /// this machine does it: the model's own
    /// [`DesktopCommand::CreateStrip`](tilo_desktop_model::commands::DesktopCommand::CreateStrip),
    /// `RenameStrip` and `MoveStrip` reductions on the cache, then the room document and
    /// the restart snapshot. A created strip takes focus. Returns the strip
    /// the command named or made.
    ///
    /// # Errors
    ///
    /// Returns a description when there is no active workspace, the name is
    /// empty, too long or holds control characters, the strip is gone, or it
    /// belongs to another topology owner.
    pub fn commit_strip_command(
        &mut self,
        command: &tilo_desktop_model::commands::DesktopCommand,
    ) -> Result<StripId, String> {
        use tilo_desktop_model::commands::{DesktopCommand, apply_to_active_workspace};
        let renamed = match command {
            DesktopCommand::CreateStrip { .. } => None,
            DesktopCommand::RenameStrip { strip, .. } | DesktopCommand::MoveStrip { strip, .. } => {
                let strip = StripId(*strip);
                if !self.may_rename_strip(strip) {
                    return Err("target belongs to another topology owner".into());
                }
                Some(strip)
            }
            _ => return Err("not a strip command".into()),
        };
        self.desktop
            .active_workspace()
            .ok_or("there is no active workspace")?;
        let salt = self.local_salt();
        let made =
            apply_to_active_workspace(&mut self.desktop, command, &mut || Ok(mint_under(salt)))?;
        let strip = if let Some(strip) = renamed {
            strip
        } else {
            let made = made.ok_or("the strip was not created")?;
            // Made to put windows in: the others see it now, empty.
            self.strips_to_retain.insert(made);
            made
        };
        self.seed_doc_from_cache();
        self.queue_save();
        Ok(strip)
    }

    /// Commit `command` as this device's topology owner: the shared
    /// [`tilo_desktop_model::commands::apply_command`] reduction on the
    /// cache, mirrored into the room document so peer sync ships it, and
    /// into the restart snapshot. `Ok(false)` when the order did not change.
    ///
    /// Only reorders inside one strip commit here; a close runs its own
    /// lifecycle, in the window actions above the model.
    ///
    /// Like a close, a move is not checked per person: this desktop records
    /// no window authors, so the policy a shared desktop applies to other
    /// people's windows has nothing to decide on here.
    ///
    /// # Errors
    ///
    /// Returns a description when the command no longer matches the tree or
    /// is not a reorder.
    pub fn commit_command(
        &mut self,
        command: &tilo_desktop_model::commands::DesktopCommand,
    ) -> Result<bool, String> {
        use tilo_desktop_model::commands::{DesktopCommand, apply_command};
        let strip_id = match command {
            DesktopCommand::ReorderTab { location, .. } => location.strip,
            DesktopCommand::ReorderGroup { strip, .. } => *strip,
            DesktopCommand::MoveGroup { group, strip, .. } => {
                return self.commit_group_move(command, *group, StripId(*strip));
            }
            _ => return Err("this desktop commits only reorders and moves as commands".into()),
        };
        let strip_id = StripId(strip_id);
        let (ws_idx, strip_idx) = self
            .desktop
            .strip_position(strip_id)
            .ok_or("strip was removed")?;
        let workspace = self
            .desktop
            .workspaces
            .get_mut(ws_idx)
            .ok_or("workspace was removed")?;
        let order = |workspace: &Workspace| {
            workspace.strips.get(strip_idx).map(|strip| {
                strip
                    .window_groups
                    .iter()
                    .map(|group| (group.id, group.tabs.iter().map(|tab| tab.id).collect()))
                    .collect::<Vec<(GroupId, Vec<TabId>)>>()
            })
        };
        let before = order(workspace);
        apply_command(
            workspace,
            command,
            &mut || Err("a reorder mints no identity".into()),
            &mut |_, _| Err("a reorder creates no record".into()),
        )?;
        if order(workspace) == before {
            return Ok(false);
        }
        let groups = &workspace
            .strips
            .get(strip_idx)
            .ok_or("strip was removed")?
            .window_groups;
        match command {
            DesktopCommand::ReorderTab { location, .. } => {
                let tabs = groups
                    .iter()
                    .find(|group| group.id == location.group)
                    .map(|group| group.tabs.clone())
                    .unwrap_or_default();
                tilo_desktop_model::desktop_doc::rewrite_group_tabs(
                    &mut self.doc,
                    location.group,
                    &tabs,
                );
            }
            DesktopCommand::ReorderGroup { group, .. } => {
                if let Some(dest) = groups.iter().position(|candidate| candidate.id == *group)
                    && let Some(moved) = groups.get(dest).cloned()
                {
                    tilo_desktop_model::desktop_doc::reorder_group(
                        &mut self.doc,
                        strip_id,
                        &moved,
                        dest,
                    );
                }
            }
            _ => {}
        }
        self.queue_save();
        Ok(true)
    }

    /// [`Self::commit_command`] for a [`DesktopCommand::MoveGroup`]: the
    /// group leaves its strip for `strip_id`, in the cache and the room
    /// document together, or in neither.
    ///
    /// [`DesktopCommand::MoveGroup`]: tilo_desktop_model::commands::DesktopCommand::MoveGroup
    fn commit_group_move(
        &mut self,
        command: &tilo_desktop_model::commands::DesktopCommand,
        group_id: GroupId,
        strip_id: StripId,
    ) -> Result<bool, String> {
        let (ws_idx, strip_idx) = self
            .desktop
            .strip_position(strip_id)
            .ok_or("strip was removed")?;
        let workspace = self
            .desktop
            .workspaces
            .get_mut(ws_idx)
            .ok_or("workspace was removed")?;
        let before = workspace.strips.clone();
        tilo_desktop_model::commands::apply_command(
            workspace,
            command,
            &mut || Err("a move mints no identity".into()),
            &mut |_, _| Err("a move creates no record".into()),
        )?;
        let landed = workspace.strips.get(strip_idx).and_then(|strip| {
            let index = strip
                .window_groups
                .iter()
                .position(|group| group.id == group_id)?;
            Some((index, strip.window_groups.get(index)?.clone()))
        });
        let order = |strips: &[Strip]| {
            strips
                .iter()
                .map(|strip| {
                    strip
                        .window_groups
                        .iter()
                        .map(|group| group.id)
                        .collect::<Vec<GroupId>>()
                })
                .collect::<Vec<_>>()
        };
        if order(&before) == order(&workspace.strips) {
            return Ok(false);
        }
        let mirrored = landed.is_some_and(|(index, group)| {
            tilo_desktop_model::desktop_doc::move_group(&mut self.doc, strip_id, &group, index)
        });
        if !mirrored {
            if let Some(workspace) = self.desktop.workspaces.get_mut(ws_idx) {
                workspace.strips = before;
            }
            return Err("the room document refused the group move".into());
        }
        self.queue_save();
        Ok(true)
    }

    /// Bind a group reorder the way the desktop surface reports it (the
    /// post-removal slot) and commit it.
    #[cfg(test)]
    pub(crate) fn reorder_group_for_tests(
        &mut self,
        strip: StripId,
        group: GroupId,
        to: usize,
    ) -> bool {
        self.commit_intent_for_tests(tilo_desktop::intents::SessionIntent::ReorderGroup {
            strip,
            group,
            to,
        })
    }

    /// Bind an in-group tab reorder the way the desktop surface reports it
    /// and commit it.
    #[cfg(test)]
    pub(crate) fn reorder_tab_for_tests(&mut self, group: GroupId, from: usize, to: usize) -> bool {
        self.commit_intent_for_tests(tilo_desktop::intents::SessionIntent::ReorderTab {
            group,
            from,
            to,
        })
    }

    #[cfg(test)]
    fn commit_intent_for_tests(&mut self, intent: tilo_desktop::intents::SessionIntent) -> bool {
        let group = match &intent {
            tilo_desktop::intents::SessionIntent::ReorderGroup { group, .. }
            | tilo_desktop::intents::SessionIntent::ReorderTab { group, .. }
            | tilo_desktop::intents::SessionIntent::MoveGroup { group, .. } => *group,
            _ => return false,
        };
        let Some(room) = self
            .desktop
            .workspaces
            .iter()
            .find(|workspace| {
                workspace
                    .strips
                    .iter()
                    .any(|strip| strip.window_groups.iter().any(|g| g.id == group))
            })
            .map(|workspace| workspace.room().to_string())
        else {
            return false;
        };
        let source = tilo_desktop_model::commands::CommandSource {
            room,
            heads: Vec::new(),
        };
        tilo_desktop::session_commands::bind_intent(&self.desktop, source, intent)
            .and_then(|bound| self.commit_command(&bound.command))
            .unwrap_or(false)
    }
}

/// What colour each person on `roster` is drawn in, as this process's
/// local user sees the room; see
/// [`tilo_desktop::collaborators::roster_colors_with_local_user`].
pub fn roster_colors(roster: &[Collaborator]) -> HashMap<String, (u8, u8, u8)> {
    tilo_desktop::collaborators::roster_colors_with_local_user(roster, &local_user_id())
}

/// Present collaborator count for the active workspace, excluding the local
/// user. Presence is workspace-scoped; the legacy tree-level cache must never
/// make somebody from another workspace appear here.
#[cfg(test)]
pub(crate) fn active_workspace_peer_count(tree: &WorkspaceTree) -> usize {
    // Somebody named but not yet arrived is not counted: an invitation
    // that has not been redeemed is not somebody in the workspace.
    let present = |c: &&Collaborator| c.id != "me" && !c.invited;
    tree.desktop.active_workspace().map_or(0, |workspace| {
        workspace.collaborators.iter().filter(present).count()
    })
}

#[cfg(test)]
#[path = "state/own_room_tests.rs"]
mod own_room_tests;

#[cfg(test)]
mod bootstrap_tests {
    //! Coverage for the two bootstrap trees, which exist for opposite
    //! reasons and must not drift into each other: `bootstrap_default` is
    //! pinned empty (no invented collaborators, workspaces, or windows —
    //! see [`WorkspaceTree::bootstrap_default`]), `bootstrap_demo` is the
    //! populated fixture for `--shots` and tests needing a tab to act on.
    //!
    //! Re-pinning the demo fixture may also need re-seeding the e2e
    //! goldens (see `tests/e2e_shell_flows.rs`).

    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn bootstrap_default_and_demo_trees_carry_exactly_what_they_promise() {
        // Phase `bootstrap_default_is_one_empty_workspace_with_no_people_windows_or_roster`.
        {
            // Phase `bootstrap_default_invents_no_people`.
            {
                let tree = WorkspaceTree::bootstrap_default();
                assert!(
                    tree.desktop.collaborators.is_empty(),
                    "first-boot tree carries tree-level collaborators: {:?}",
                    tree.desktop
                        .collaborators
                        .iter()
                        .map(|c| &c.id)
                        .collect::<Vec<_>>()
                );
                for ws in &tree.desktop.workspaces {
                    assert!(
                        ws.collaborators.is_empty(),
                        "first-boot workspace {:?} carries collaborators {:?} — a \
                         roster entry here is a person who does not exist, written \
                         into the user's workspace document and painted in the top bar",
                        ws.name,
                        ws.collaborators.iter().map(|c| &c.id).collect::<Vec<_>>()
                    );
                }
            }

            // Phase `bootstrap_default_opens_no_windows`.
            {
                let tree = WorkspaceTree::bootstrap_default();
                for ws in &tree.desktop.workspaces {
                    for strip in &ws.strips {
                        assert!(
                            strip.window_groups.is_empty(),
                            "first-boot workspace {:?} strip {:?} already holds {} \
                             window group(s) — windows the person never opened",
                            ws.name,
                            strip.name,
                            strip.window_groups.len()
                        );
                    }
                }
                assert!(
                    tree.desktop.active_group.is_empty(),
                    "first-boot tree points at an active group with no groups to point at"
                );
            }

            // Phase `bootstrap_default_is_one_empty_workspace_with_somewhere_to_put_things`.
            {
                let tree = WorkspaceTree::bootstrap_default();
                // Exactly one: two would mean naming a second one, and every name
                // ("Research", "Design") is a claim about what this person does.
                assert_eq!(
                    tree.desktop.workspaces.len(),
                    1,
                    "first boot should be a single empty workspace; got {:?}",
                    tree.desktop
                        .workspaces
                        .iter()
                        .map(|w| &w.name)
                        .collect::<Vec<_>>()
                );
                // Empty, but not structurally broken: the active pointer resolves,
                // and the workspace has a strip for the first window to land in.
                let active = tree
                    .desktop
                    .active_workspace()
                    .expect("first boot must have a resolvable active workspace");
                assert_eq!(active.id, tree.desktop.workspaces[0].id);
                assert_eq!(
                    active.strips.len(),
                    1,
                    "the empty workspace still needs a strip to place a window into"
                );
                assert_eq!(
                    tree.desktop.active_strip.get(&active.id).copied(),
                    Some(active.strips[0].id),
                    "the active-strip pointer must resolve to the workspace's strip"
                );
            }

            // Phase `bootstrap_default_seeds_nobody_into_any_roster`.
            // A machine that has just been installed has had nobody join it, so
            // every roster the first-boot tree hands the chrome is empty.
            {
                let tree = WorkspaceTree::bootstrap_default();
                for ws in &tree.desktop.workspaces {
                    assert!(
                        ws.collaborators.is_empty(),
                        "workspace {:?} boots with {} collaborator(s) nobody invited: \
                         {:?}. The avatar stack is a claim about who is present; \
                         seeding it fabricates participants.",
                        ws.name,
                        ws.collaborators.len(),
                        ws.collaborators.iter().map(|c| &c.name).collect::<Vec<_>>(),
                    );
                }
                assert!(
                    tree.desktop.collaborators.is_empty(),
                    "the tree-level roster fallback boots non-empty: {:?}",
                    tree.desktop
                        .collaborators
                        .iter()
                        .map(|c| &c.name)
                        .collect::<Vec<_>>(),
                );
                // And the chrome's own arithmetic agrees: no peers to count.
                assert_eq!(
                    active_workspace_peer_count(&tree),
                    0,
                    "a fresh boot must count zero peers in the active workspace"
                );
            }
        }

        // Phase `bootstrap_demo_carries_three_workspaces_with_a_wide_first_group`.
        {
            // Phase `bootstrap_demo_carries_three_workspaces`.
            {
                let tree = WorkspaceTree::bootstrap_demo();
                assert!(
                    tree.desktop.workspaces.len() >= 3,
                    "demo tree should carry at least 3 workspaces (Home / Research / \
                     Design) so the overview has meaningful content; got {}",
                    tree.desktop.workspaces.len()
                );
                // This fixture's whole purpose is to be populated —
                // `bootstrap_default`, what a real boot reaches, is pinned empty.
                assert!(
                    tree.desktop
                        .workspaces
                        .iter()
                        .any(|w| !w.collaborators.is_empty()),
                    "at least one demo workspace should carry >=1 collaborator"
                );
            }

            // Phase `bootstrap_demo_first_group_uses_wide_or_larger`.
            {
                let tree = WorkspaceTree::bootstrap_demo();
                let first_ws = tree
                    .desktop
                    .workspaces
                    .first()
                    .expect("at least one workspace");
                let first_strip = first_ws.strips.first().expect("at least one strip");
                let first_group = first_strip
                    .window_groups
                    .first()
                    .expect("at least one group");
                // Widths are fractions of the strip, so "wide enough" is a share
                // of the display rather than a pixel count that means a different
                // share on every monitor.
                let share = first_group.width.permille();
                assert!(
                    share >= tilo_desktop_model::width::WIDTH_NORMAL_PERMILLE,
                    "the demo group should fill at least the Normal share of the \
                     strip so the white card dominates the viewport; got {:?} \
                     ({share}/1000)",
                    first_group.width
                );
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn the_roster_admits_names_colours_and_removes_people_by_presence() {
        // Phase `presence_admits_each_session_once_under_its_best_known_name`.
        {
            // Phase `admitting_the_same_person_twice_is_one_row`.
            // The same person arriving twice is one avatar — presence
            // republishes at ~30 Hz, so a push instead of an upsert would grow
            // the stack by thirty entries a second.
            {
                let mut tree = WorkspaceTree::bootstrap_default();
                let ws = tree.desktop.active;
                assert!(tree.admit_collaborator(ws, Collaborator::new("alice", "Alice")));
                assert!(!tree.admit_collaborator(ws, Collaborator::new("alice", "Alice")));
                assert_eq!(
                    tree.desktop.active_workspace().unwrap().collaborators.len(),
                    1
                );
                assert_eq!(active_workspace_peer_count(&tree), 1);
            }

            // Phase `a_peer_is_admitted_to_the_workspace_that_addresses_the_room`.
            // A room is addressed by its durable identity, not this machine's
            // local handle for it: presence frames carry the room, and every
            // peer numbers its workspaces differently.
            {
                let mut tree = WorkspaceTree::bootstrap_default();
                let room = tree.desktop.active_workspace().unwrap().room();
                assert!(tree.admit_collaborator_in_room(room, Collaborator::new("bob", "Bob")));
                assert_eq!(active_workspace_peer_count(&tree), 1);

                // A room this machine is not in has no workspace to admit into.
                assert!(
                    !tree
                        .admit_collaborator_in_room(RoomId::new(), Collaborator::new("eve", "Eve"))
                );
                assert_eq!(active_workspace_peer_count(&tree), 1);
            }
        }

        // Phase `a_peer_leaves_the_roster_when_removed_quiet_or_departed`.
        {
            // Phase `a_session_going_quiet_takes_its_person_off_the_roster`.
            // A session that goes quiet takes its person off the roster by
            // whichever name they'd acquired — a departure names a peer, never
            // a person, which is what [`Collaborator::session`] bridges.
            {
                let mut tree = WorkspaceTree::bootstrap_default();
                let room = tree.desktop.active_workspace().unwrap().room();
                let alice = 0x11_u64;
                let bob = 0x22_u64;
                assert!(tree.admit_collaborator_in_room(
                    room,
                    Collaborator::new("alice", "alice").with_session(alice)
                ));
                assert!(tree.admit_collaborator_in_room(
                    room,
                    Collaborator::new("bob", "bob").with_session(bob)
                ));
                assert_eq!(active_workspace_peer_count(&tree), 2);

                assert!(tree.retire_session(alice), "alice's session left");
                let left = &tree.desktop.active_workspace().unwrap().collaborators;
                assert_eq!(
                    left.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
                    vec!["bob"],
                    "the person whose session went quiet must come off the bar, \
                     and nobody else; got {left:?}"
                );
                assert!(
                    !tree.retire_session(alice),
                    "a session that already left changes nothing"
                );
            }

            // Phase `a_departure_sweeps_every_room_the_session_was_in`.
            // A departure reaches every room that session was in — one session
            // can be in several rooms at once, and it left all of them.
            {
                // Two rooms, since one can't tell a sweep from a hit; both empty
                // so the "nobody left" assertion isn't confused by seeded people.
                let mut tree = WorkspaceTree::bootstrap_default();
                tree.create_hosted_workspace("Second");
                let rooms: Vec<RoomId> = tree
                    .desktop
                    .workspaces
                    .iter()
                    .map(crate::shell::state::Workspace::room)
                    .collect();
                assert!(
                    rooms.len() >= 2,
                    "need two rooms to tell a sweep from a hit"
                );
                let peer = 0x33_u64;
                for r in &rooms {
                    assert!(tree.admit_collaborator_in_room(
                        *r,
                        Collaborator::new("dana", "dana").with_session(peer)
                    ));
                }
                assert!(tree.retire_session(peer));
                for ws in &tree.desktop.workspaces {
                    assert!(
                        ws.collaborators.is_empty(),
                        "workspace {:?} still shows a peer whose session left: {:?}",
                        ws.name,
                        ws.collaborators
                    );
                }
            }
        }

        // Phase `roster_colours_follow_the_room_unless_explicitly_chosen`.
        {
            // Phase `roster_colours_match_what_the_room_settles_for_cursors`.
            // Roster avatars and peer cursors must be the same colour for the
            // same person — drawn by different code in different files.
            {
                let roster = vec![
                    Collaborator::new("alice", "Alice"),
                    Collaborator::new("bob", "Bob"),
                    Collaborator::new("carol", "Carol"),
                ];
                let mine = roster_colors(&roster);

                // What the cursor overlay computes for the same room: everyone
                // in it, the local user included.
                let local = local_user_id();
                let mut ids: Vec<&str> = roster.iter().map(|c| c.id.as_str()).collect();
                ids.push(local.as_str());
                let cursors = collab_types::presence::assign_peer_colors(&ids, &[]);

                for c in &roster {
                    assert_eq!(
                        mine.get(&c.id),
                        cursors.get(c.id.as_str()),
                        "{}'s avatar and cursor must be the same colour",
                        c.id
                    );
                }
                // `carol` hashes onto `alice`'s pinned pink; the room has to
                // have moved one of them, or two people are one colour.
                assert_ne!(
                    mine.get("alice"),
                    mine.get("carol"),
                    "two people in one room must not be painted the same colour"
                );
            }

            // Phase `an_explicit_roster_colour_wins_and_is_not_reused`.
            // An explicit roster colour outranks the assignment, and is held
            // back from it, so nobody else is computed onto it.
            {
                let pink = (0xec, 0x48, 0x99);
                let roster = vec![
                    Collaborator::new("bob", "Bob").with_cursor_color("#ec4899"),
                    Collaborator::new("alice", "Alice"),
                ];
                let colors = roster_colors(&roster);
                assert_eq!(colors.get("bob"), Some(&pink), "bob asked for pink");
                assert_ne!(
                    colors.get("alice"),
                    Some(&pink),
                    "alice's pinned pink is taken, so the room must move her"
                );
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact persisted width migration values are the behavior under test"
)]
mod automerge_foundation_tests {
    //! Tests for the `WorkspaceTree` ↔ automerge sync surface:
    //! `current_heads`, `export_changes_since`, `apply_changes`, and the
    //! mutators (`append_empty_workspace`, …) that write through them.

    use super::*;
    use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};

    #[test]
    fn appended_workspace_rebuilds_with_the_same_default_grants() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let id = tree.create_hosted_workspace("Project X").0;
        let created = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)
            .unwrap();
        let mut restored = DesktopTree::default();
        tilo_desktop_model::desktop_doc::rebuild(&tree.doc, &mut restored);
        let reconstructed = restored
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)
            .unwrap();
        assert_eq!(reconstructed.room(), created.room());
        assert_eq!(reconstructed.policy, created.policy);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn tree_topology_changes_export_and_apply_as_automerge_changes() {
        // Phase `tree_changes_export_and_apply_as_automerge_changes`.
        {
            // Phase `empty_tree_has_no_heads_and_no_changes`.
            {
                let mut t = WorkspaceTree::default();
                assert!(t.current_heads().is_empty());
                assert!(t.export_changes_since(&[]).is_empty());
            }

            // Phase `apply_changes_round_trips_a_manual_doc_mutation`.
            {
                let mut tree_a = WorkspaceTree::default();
                // Synthesise a change by writing directly into the doc rather
                // than through a mutator.
                tree_a
                    .doc
                    .put(&ROOT, "demo", ScalarValue::Uint(42))
                    .unwrap();

                let heads_after = tree_a.current_heads();
                assert!(!heads_after.is_empty());
                let changes = tree_a.export_changes_since(&[]);
                assert!(!changes.is_empty());

                let mut tree_b = WorkspaceTree::default();
                tree_b.apply_changes(&changes).unwrap();

                // Tree B now sees the same demo value.
                let val = tree_b.doc.get(&ROOT, "demo").unwrap();
                assert!(val.is_some());
            }

            // Phase `export_changes_since_current_heads_yields_empty`.
            {
                let mut t = WorkspaceTree::default();
                t.doc.put(&ROOT, "k", ScalarValue::Uint(1)).unwrap();
                let heads = t.current_heads();
                let changes = t.export_changes_since(&heads);
                assert!(
                    changes.is_empty(),
                    "exporting since current heads must yield no changes"
                );
            }
        }

        // Phase `workspace_room_and_active_topology_propagate_via_apply_changes`.
        {
            // Phase `appending_a_workspace_propagates_via_apply_changes`.
            {
                // Tree B starts from `default()`, not `bootstrap_default`, so
                // its empty doc cleanly absorbs A's full change-graph without
                // both peers concurrently creating the `workspaces` list at
                // ROOT — seeding isn't diff-aware.
                let mut tree_a = WorkspaceTree::bootstrap_demo();
                let initial_count = tree_a.desktop.workspaces.len();

                tree_a.create_hosted_workspace("Project X");
                assert_eq!(tree_a.desktop.workspaces.len(), initial_count + 1);

                let changes = tree_a.export_changes_since(&[]);
                assert!(
                    !changes.is_empty(),
                    "append_empty_workspace must produce doc-side changes"
                );

                let mut tree_b = WorkspaceTree::default();
                tree_b.apply_changes(&changes).expect("apply changes");

                assert!(
                    tree_b
                        .desktop
                        .workspaces
                        .iter()
                        .any(|w| w.name == "Project X"),
                    "tree B should observe the appended workspace after sync; \
                     got names: {:?}",
                    tree_b
                        .desktop
                        .workspaces
                        .iter()
                        .map(|w| &w.name)
                        .collect::<Vec<_>>()
                );
            }

            // Phase `a_workspaces_room_propagates_via_apply_changes`.
            // A peer that hydrates a workspace out of another peer's doc must
            // end up in *that peer's* room. Minting a local one instead would
            // leave it holding a workspace it can see but cannot collaborate
            // in.
            {
                let mut tree_a = WorkspaceTree::bootstrap_demo();
                let new_id = tree_a.create_hosted_workspace("Project X").0;
                let room = tree_a.room_of(new_id).expect("the append mints a room");
                assert!(!room.is_nil());

                let changes = tree_a.export_changes_since(&[]);
                let mut tree_b = WorkspaceTree::default();
                tree_b.apply_changes(&changes).expect("apply changes");

                let hydrated = tree_b
                    .desktop
                    .workspaces
                    .iter()
                    .find(|w| w.name == "Project X")
                    .expect("peer B should observe the appended workspace");
                assert_eq!(
                    hydrated.room(),
                    room,
                    "peer B must adopt the room peer A named, not mint one of its \
                     own — otherwise the two hold the same workspace and address \
                     different clusters"
                );
                assert_eq!(
                    hydrated.collab_cluster_id(),
                    tree_a
                        .desktop
                        .workspaces
                        .iter()
                        .find(|w| w.id == new_id)
                        .expect("peer A's workspace")
                        .collab_cluster_id(),
                    "and the cluster their apps are launched into must match"
                );
            }

            // Phase `a_joined_workspace_reaches_peers_with_its_room`.
            // A workspace joined from an invitation is the inviter's room
            // from the start; the doc carries it as well as the cache, or
            // the workspace never replicates as that room.
            {
                let mut tree_a = WorkspaceTree::bootstrap_demo();
                let inviters_room = RoomId::new();
                let id = tree_a
                    .join_room("Invited", inviters_room)
                    .expect("a room is joined as a workspace of its own");

                let changes = tree_a.export_changes_since(&[]);
                let mut tree_b = WorkspaceTree::default();
                tree_b.apply_changes(&changes).expect("apply changes");

                assert_eq!(
                    tree_b.room_of(id),
                    Some(inviters_room),
                    "a joined workspace must carry its room to its peers"
                );
            }

            // Phase `the_nil_room_is_never_joined`.
            // "Join the unminted room" is never a meaningful request, and
            // honouring it would make a workspace that collaborates with
            // nobody.
            {
                let mut tree = WorkspaceTree::bootstrap_demo();
                let before = tree.desktop.workspaces.len();
                assert!(
                    tree.join_room("Nowhere", RoomId::nil()).is_none(),
                    "joining the nil room must be refused"
                );
                assert_eq!(tree.desktop.workspaces.len(), before);
            }

            // Phase `set_active_propagates_via_apply_changes`.
            {
                // Tree B uses `default()`, same reason as the append test: two
                // `bootstrap_default` calls would race on the workspaces list.
                let mut tree_a = WorkspaceTree::bootstrap_demo();
                let target = tree_a
                    .desktop
                    .workspaces
                    .iter()
                    .find(|w| w.id != tree_a.desktop.active)
                    .map(|w| w.id)
                    .expect("bootstrap seeds >=2 workspaces");
                tree_a.set_active(target);
                assert_eq!(tree_a.desktop.active, target);

                let changes = tree_a.export_changes_since(&[]);
                let mut tree_b = WorkspaceTree::default();
                tree_b.apply_changes(&changes).expect("apply changes");
                assert_eq!(
                    tree_b.desktop.active, target,
                    "tree B's active should match peer A's after sync"
                );
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn cross_strip_tab_moves_propagate_exactly_or_roll_back() {
        // Phase `moving_a_tab_across_strips_propagates_via_apply_changes`.
        {
            let mut tree_a = WorkspaceTree::bootstrap_demo();
            // Add a second strip with an empty group so the move has
            // somewhere to land; re-seed so the doc carries it too.
            let dst_strip_id = StripId::new();
            let dst_group_id = next_id();
            tree_a.desktop.workspaces[0].strips.push(Strip {
                id: dst_strip_id,
                name: "Dest".into(),
                window_groups: vec![WindowGroup {
                    id: dst_group_id,
                    tabs: Vec::new(),
                    active_tab: 0,
                    width: WindowWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
                }],
            });
            tree_a.seed_doc_from_cache();

            let src_strip_id = tree_a.desktop.workspaces[0].strips[0].id;
            let target_tab = tree_a.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id;

            let new_group_id = next_id();
            assert!(tree_a.move_tab_across_strips(
                src_strip_id,
                dst_strip_id,
                target_tab,
                new_group_id,
                DropTarget::GroupGap { gap_index: 1 },
            ));

            let changes = tree_a.export_changes_since(&[]);
            let mut tree_b = WorkspaceTree::default();
            tree_b.apply_changes(&changes).expect("apply changes");

            let dst_strip_in_b = tree_b
                .desktop
                .workspaces
                .iter()
                .flat_map(|w| w.strips.iter())
                .find(|s| s.id == dst_strip_id);
            assert!(
                dst_strip_in_b.is_some(),
                "destination strip must be observable in tree B"
            );
            let landed_in_dst = dst_strip_in_b
                .unwrap()
                .window_groups
                .iter()
                .flat_map(|g| g.tabs.iter())
                .any(|t| t.id == target_tab);
            assert!(
                landed_in_dst,
                "moved tab must appear in destination strip after sync"
            );
            let destination_groups = &dst_strip_in_b.expect("destination strip").window_groups;
            assert_eq!(
                destination_groups
                    .iter()
                    .map(|group| group.id)
                    .collect::<Vec<_>>(),
                vec![dst_group_id, new_group_id],
                "doc resync must preserve the same new group and insertion order as the cache"
            );
            assert_eq!(
                destination_groups[1]
                    .tabs
                    .iter()
                    .map(|tab| tab.id)
                    .collect::<Vec<_>>(),
                vec![target_tab],
                "the moved tab remains a separate group after resync"
            );
        }

        // Phase `moving_a_tab_across_strips_into_existing_group_preserves_exact_index`.
        {
            let mut tree_a = WorkspaceTree::bootstrap_demo();
            let src_strip_id = tree_a.desktop.workspaces[0].strips[0].id;
            let target_tab = tree_a.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id;
            let template = tree_a.desktop.workspaces[0].strips[0].window_groups[0].tabs[1].clone();
            let mut before = template.clone();
            before.id = next_id();
            before.title = "Before".into();
            let mut after = template;
            after.id = next_id();
            after.title = "After".into();
            let expected = vec![before.id, target_tab, after.id];
            let dst_strip_id = StripId::new();
            let dst_group_id = next_id();
            tree_a.desktop.workspaces[0].strips.push(Strip {
                id: dst_strip_id,
                name: "Dest".into(),
                window_groups: vec![WindowGroup {
                    id: dst_group_id,
                    tabs: vec![before, after],
                    active_tab: expected[0],
                    width: WindowWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
                }],
            });
            tree_a.seed_doc_from_cache();

            assert!(tree_a.move_tab_across_strips(
                src_strip_id,
                dst_strip_id,
                target_tab,
                next_id(),
                DropTarget::ExistingGroup {
                    group_id: dst_group_id,
                    tab_index: 1,
                },
            ));
            let cache_group = &tree_a.desktop.workspaces[0]
                .strips
                .iter()
                .find(|strip| strip.id == dst_strip_id)
                .expect("destination strip")
                .window_groups[0];
            assert_eq!(
                cache_group
                    .tabs
                    .iter()
                    .map(|tab| tab.id)
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(cache_group.active_tab, target_tab);

            let changes = tree_a.export_changes_since(&[]);
            let mut tree_b = WorkspaceTree::default();
            tree_b.apply_changes(&changes).expect("apply changes");
            let peer_group = &tree_b
                .desktop
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .find(|strip| strip.id == dst_strip_id)
                .expect("peer destination strip")
                .window_groups[0];
            assert_eq!(
                peer_group.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
                expected,
                "Automerge must preserve the exact existing-group insertion index"
            );
        }

        // Phase `cross_strip_move_rolls_back_when_document_cannot_commit`.
        {
            let mut tree = WorkspaceTree::bootstrap_demo();
            let dst_strip_id = StripId::new();
            tree.desktop.workspaces[0].strips.push(Strip {
                id: dst_strip_id,
                name: "Dest".into(),
                window_groups: Vec::new(),
            });
            let src_strip_id = tree.desktop.workspaces[0].strips[0].id;
            let target_tab = tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id;
            let before = tree.desktop.workspaces.clone();
            // Model a stale/unavailable replication document after the cache was
            // loaded. The move must not become a cache-only layout.
            tree.doc = automerge::AutoCommit::new();

            assert!(!tree.move_tab_across_strips(
                src_strip_id,
                dst_strip_id,
                target_tab,
                next_id(),
                DropTarget::GroupGap { gap_index: 0 },
            ));
            assert_eq!(
                serde_json::to_value(&tree.desktop.workspaces).unwrap(),
                serde_json::to_value(&before).unwrap(),
                "failed document commit must restore the exact source layout"
            );
        }
    }

    #[test]
    fn a_refused_move_leaves_the_strip_focused_on_the_window_that_stayed() {
        // The person's focused window is its group's only tab; the move to
        // another strip is refused because the document cannot record it
        // ("Couldn't move the window. Try again."). The window stays, and
        // so must the strip's focus: handing it to the neighbour, as a move
        // that happened does, would leave the window the person was in
        // unfocused for a move that never happened.
        let mut tree = WorkspaceTree::bootstrap_demo();
        let dst_strip = StripId::new();
        tree.desktop.workspaces[0].strips.push(Strip {
            id: dst_strip,
            name: "Dest".into(),
            window_groups: Vec::new(),
        });
        let src_strip = tree.desktop.workspaces[0].strips[0].id;
        let mut alone = tree.desktop.workspaces[0].strips[0].window_groups[0].clone();
        alone.id = next_id();
        alone.tabs.truncate(1);
        alone.tabs[0].id = next_id();
        alone.active_tab = 0;
        alone.focus_history.clear();
        let (group, tab) = (alone.id, alone.tabs[0].id);
        tree.desktop.workspaces[0].strips[0]
            .window_groups
            .push(alone);
        tree.desktop.set_active_group(src_strip, group);
        tree.doc = automerge::AutoCommit::new();

        assert!(!tree.move_tab_across_strips(
            src_strip,
            dst_strip,
            tab,
            next_id(),
            DropTarget::GroupGap { gap_index: 0 },
        ));
        assert_eq!(tree.desktop.active_group_for(src_strip), Some(group));
    }

    #[test]
    fn a_window_moves_into_a_workspace_created_moments_ago() {
        // The window menu's Move to workspace, right after Create a
        // workspace: nothing else has written the new workspace's strip to
        // the replicated document, and a move the document cannot record
        // is refused, so the window must find that strip there.
        let mut tree = WorkspaceTree::bootstrap_demo();
        let src_strip = tree.desktop.workspaces[0].strips[0].id;
        let tab = tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id;
        let (fresh, _) = tree.create_hosted_workspace("Fresh");
        let dst_strip = tree.desktop.active_strip[&fresh];
        assert!(
            tree.move_tab_across_strips(
                src_strip,
                dst_strip,
                tab,
                next_id(),
                DropTarget::GroupGap { gap_index: 0 },
            ),
            "the new workspace's strip is in the document"
        );
        let landed = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == fresh)
            .is_some_and(|workspace| {
                workspace.strips.iter().any(|strip| {
                    strip
                        .window_groups
                        .iter()
                        .any(|group| group.tabs.iter().any(|t| t.id == tab))
                })
            });
        assert!(landed, "the window is in the new workspace");
    }
}

#[cfg(test)]
mod active_group_tests {
    //! Coverage for the active-group pointer + close-fade reconciler.
    //! Tests drive it directly with a hand-built tree and the
    //! closing-group set the shell would harvest from widgets, keeping
    //! the data-layer contract free of the GPU dependency live widgets
    //! would add.

    use super::*;

    /// Build a single-workspace, single-strip tree carrying `n` empty
    /// window groups with monotonic ids `1..=n`. Returns the strip id
    /// alongside the tree so callers can drive the reconciler directly.
    fn tree_with_n_groups(n: u64) -> (WorkspaceTree, StripId, Vec<GroupId>) {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let group_ids: Vec<GroupId> = (1..=n).collect();
        let strip = Strip {
            id: strip_id,
            name: "Main".into(),
            window_groups: group_ids
                .iter()
                .map(|gid| WindowGroup {
                    id: *gid,
                    tabs: vec![],
                    active_tab: 0,
                    width: WindowWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
                })
                .collect(),
        };
        let ws = {
            let mut workspace = Workspace::new(ws_id, String::new());
            workspace.id = ws_id;
            workspace.name = "Test".into();
            workspace.strips = vec![strip];
            workspace
        };
        let mut active_strip = HashMap::new();
        active_strip.insert(ws_id, strip_id);
        let tree = WorkspaceTree {
            desktop: DesktopTree {
                workspaces: vec![ws],
                active: ws_id,
                active_strip,
                active_group: HashMap::new(),
                collaborators: vec![],
                ..Default::default()
            },
            ..Default::default()
        };
        (tree, strip_id, group_ids)
    }

    fn group_order(tree: &WorkspaceTree, strip_id: StripId) -> Vec<u64> {
        tree.desktop
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .find(|s| s.id == strip_id)
            .unwrap()
            .window_groups
            .iter()
            .map(|g| g.id)
            .collect()
    }

    #[test]
    #[serial_test::serial(next_id_statics)]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn reconcile_strip_topology_persists_splits_but_never_loses_tabs() {
        // Phase `reconcile_strip_topology_persists_a_split_without_copying_tile_state`.
        {
            use automerge::{ROOT, ReadDoc};

            let (mut tree, strip_id, ids) = tree_with_n_groups(1);
            let state_dir = tempfile::tempdir().expect("temporary state directory");
            let state_path = state_dir.path().join("state.json");
            tree.persist_path = Some(state_path.clone());
            let source_group = ids[0];
            let make_tab = |id: u64, title: &str| Tab {
                id,
                title: title.into(),
                tile: TileContent::Stub {
                    kind: AppKind::Terminal,
                    label: title.into(),
                },
                app_title: None,
            };
            tree.desktop.workspaces[0].strips[0].window_groups[0].tabs = vec![
                make_tab(10, "Notes"),
                make_tab(11, "Files"),
                make_tab(12, "Terminal"),
            ];
            tree.desktop.workspaces[0].strips[0].window_groups[0].active_tab = 10;
            tree.seed_doc_from_cache();

            let topology = vec![
                tilo_desktop_model::GroupShape {
                    id: source_group,
                    tab_ids: vec![10, 12],
                    active_tab: 10,
                    width: tilo_desktop_model::WindowWidth::Fraction(700),
                },
                tilo_desktop_model::GroupShape {
                    id: 99,
                    tab_ids: vec![11],
                    active_tab: 11,
                    width: tilo_desktop_model::WindowWidth::Normal,
                },
            ];
            assert!(tree.reconcile_strip_topology(strip_id, &topology));

            let groups = &tree.desktop.workspaces[0].strips[0].window_groups;
            assert_eq!(groups.len(), 2);
            assert_eq!(
                groups[0].tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
                vec![10, 12]
            );
            assert_eq!(groups[1].tabs[0].title, "Files");
            assert!(matches!(
                groups[1].tabs[0].tile,
                TileContent::Stub {
                    kind: AppKind::Terminal,
                    ..
                }
            ));
            assert_eq!(groups[0].width, WindowWidth::Fraction(700));
            assert_eq!(groups[1].width, WindowWidth::Normal);

            let (_, workspaces) = tree.doc.get(&ROOT, "workspaces").unwrap().unwrap();
            let (_, workspace) = tree.doc.get(&workspaces, 0_usize).unwrap().unwrap();
            let (_, strips) = tree.doc.get(&workspace, "strips").unwrap().unwrap();
            let (_, strip) = tree.doc.get(&strips, 0_usize).unwrap().unwrap();
            let (_, doc_groups) = tree.doc.get(&strip, "window_groups").unwrap().unwrap();
            assert_eq!(tree.doc.length(&doc_groups), 2);
            let (_, detached) = tree.doc.get(&doc_groups, 1_usize).unwrap().unwrap();
            let (_, detached_tabs) = tree.doc.get(&detached, "tabs").unwrap().unwrap();
            assert_eq!(tree.doc.length(&detached_tabs), 1);

            let restarted = crate::shell::persist::load_tree_from(&state_path)
                .expect("same-strip topology creates valid durable state");
            let restarted_groups = &restarted.desktop.workspaces[0].strips[0].window_groups;
            assert_eq!(restarted_groups.len(), 2);
            assert_eq!(
                restarted_groups[0]
                    .tabs
                    .iter()
                    .map(|tab| tab.id)
                    .collect::<Vec<_>>(),
                vec![10, 12],
                "the surviving group membership must survive an immediate restart"
            );
            assert_eq!(restarted_groups[1].tabs[0].id, 11);
            assert_eq!(restarted_groups[0].width, WindowWidth::Fraction(700));
        }

        // Phase `reconcile_strip_topology_rejects_a_snapshot_that_would_lose_tabs`.
        {
            let (mut tree, strip_id, ids) = tree_with_n_groups(1);
            let before = 10;
            let group = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
            group.tabs.push(Tab {
                id: before,
                title: "Notes".into(),
                tile: TileContent::Stub {
                    kind: AppKind::Terminal,
                    label: "Notes".into(),
                },
                app_title: None,
            });
            group.active_tab = before;
            let invalid = [tilo_desktop_model::GroupShape {
                id: ids[0],
                tab_ids: vec![],
                active_tab: before,
                width: tilo_desktop_model::WindowWidth::Normal,
            }];

            assert!(!tree.reconcile_strip_topology(strip_id, &invalid));
            let group = &tree.desktop.workspaces[0].strips[0].window_groups[0];
            assert_eq!(group.tabs.len(), 1);
            assert_eq!(group.tabs[0].id, before);
        }
    }

    #[test]
    #[serial_test::serial(next_id_statics)]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn reordering_tabs_and_groups_persists_and_mirrors_into_the_doc() {
        // Phase `reorder_tab_persists_and_mirrors_into_the_doc`.
        // Dragging a tab to a new slot must persist into the tree and
        // mirror into the doc, or the reorder resets on the next restart.
        {
            use automerge::{ROOT, ReadDoc, ScalarValue, Value};

            // One group (id 1) carrying three tabs: [10, 11, 12].
            let (mut tree, _strip_id, ids) = tree_with_n_groups(1);
            let group_id = ids[0];
            let mk = |id: u64, name: &str| Tab {
                id,
                title: name.into(),
                tile: TileContent::Stub {
                    kind: AppKind::Terminal,
                    label: name.into(),
                },
                app_title: None,
            };
            {
                let g = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
                g.tabs = vec![mk(10, "A"), mk(11, "B"), mk(12, "C")];
                g.active_tab = 10;
            }
            tree.seed_doc_from_cache();
            let state_dir = tempfile::tempdir().expect("state dir");
            tree.persist_path = Some(state_dir.path().join("state.json"));

            let tab_order = |t: &WorkspaceTree| -> Vec<u64> {
                t.desktop.workspaces[0].strips[0].window_groups[0]
                    .tabs
                    .iter()
                    .map(|tb| tb.id)
                    .collect()
            };
            assert_eq!(tab_order(&tree), vec![10, 11, 12]);

            // Drag the first tab to the last slot → [11, 12, 10].
            let changed = tree.reorder_tab_for_tests(group_id, 0, 2);
            assert!(changed, "moving a tab to a new slot reports a change");
            assert_eq!(tab_order(&tree), vec![11, 12, 10], "cache order updated");

            // Same slot is a no-op; unknown group is a no-op.
            assert!(!tree.reorder_tab_for_tests(group_id, 1, 1));
            assert!(!tree.reorder_tab_for_tests(999, 0, 1));
            assert_eq!(tab_order(&tree), vec![11, 12, 10]);

            // The doc must carry the new order too, so peers/persistence agree.
            let (_, workspaces_id) = tree.doc.get(&ROOT, "workspaces").unwrap().unwrap();
            let (_, ws0) = tree.doc.get(&workspaces_id, 0_usize).unwrap().unwrap();
            let (_, strips_id) = tree.doc.get(&ws0, "strips").unwrap().unwrap();
            let (_, strip0) = tree.doc.get(&strips_id, 0_usize).unwrap().unwrap();
            let (_, groups_id) = tree.doc.get(&strip0, "window_groups").unwrap().unwrap();
            let (_, g0) = tree.doc.get(&groups_id, 0_usize).unwrap().unwrap();
            let (_, tabs_id) = tree.doc.get(&g0, "tabs").unwrap().unwrap();
            let mut doc_order = Vec::new();
            for i in 0..tree.doc.length(&tabs_id) {
                let (_, t) = tree.doc.get(&tabs_id, i).unwrap().unwrap();
                if let Some((Value::Scalar(s), _)) = tree.doc.get(&t, "id").unwrap()
                    && let ScalarValue::Uint(id) = s.as_ref()
                {
                    doc_order.push(*id);
                }
            }
            assert_eq!(doc_order, vec![11, 12, 10], "doc mirrors the new tab order");

            let reloaded = crate::shell::persist::load_tree_from(
                tree.persist_path.as_ref().expect("persistence configured"),
            )
            .expect("reordered tree reloads");
            assert_eq!(
                tab_order(&reloaded),
                vec![11, 12, 10],
                "a normal restart must retain the reordered tab list"
            );
        }

        // Phase `reorder_group_moves_group_to_dropped_index`.
        {
            // Strip [1, 2, 3]. Drag group 1 to index 2 → [2, 3, 1].
            let (mut tree, strip_id, ids) = tree_with_n_groups(3);
            assert_eq!(group_order(&tree, strip_id), vec![1, 2, 3]);
            let changed = tree.reorder_group_for_tests(strip_id, ids[0], 2);
            assert!(changed, "moving group 1 to a new slot must report a change");
            assert_eq!(
                group_order(&tree, strip_id),
                vec![2, 3, 1],
                "group 1 must land at the dropped index, neighbours shift left",
            );
        }

        // Phase `reorder_group_forward_and_backward`.
        {
            let (mut tree, strip_id, ids) = tree_with_n_groups(4); // [1,2,3,4]
            // Move last group (4) to the front.
            assert!(tree.reorder_group_for_tests(strip_id, ids[3], 0));
            assert_eq!(group_order(&tree, strip_id), vec![4, 1, 2, 3]);
            // Move it back to the last slot.
            assert!(tree.reorder_group_for_tests(strip_id, 4, 3));
            assert_eq!(group_order(&tree, strip_id), vec![1, 2, 3, 4]);
        }

        // Phase `reorder_group_to_same_index_is_a_noop`.
        {
            let (mut tree, strip_id, ids) = tree_with_n_groups(3);
            let changed = tree.reorder_group_for_tests(strip_id, ids[1], 1);
            assert!(!changed, "moving a group to its own slot reports no change");
            assert_eq!(group_order(&tree, strip_id), vec![1, 2, 3]);
        }

        // Phase `reorder_group_unknown_group_is_a_noop`.
        {
            let (mut tree, strip_id, _ids) = tree_with_n_groups(2);
            assert!(!tree.reorder_group_for_tests(strip_id, 999, 0));
            assert_eq!(group_order(&tree, strip_id), vec![1, 2]);
        }

        // Phase `reorder_group_mirrors_into_the_seeded_doc`.
        {
            use automerge::{ROOT, ReadDoc, ScalarValue, Value};

            // With the doc seeded from the cache, a reorder must also reshuffle
            // the doc's `window_groups` list so peer-sync ships the new order.
            let (mut tree, strip_id, ids) = tree_with_n_groups(3);
            for group in &mut tree.desktop.workspaces[0].strips[0].window_groups {
                let tab_id = 10_000 + group.id;
                group.tabs.push(Tab {
                    id: tab_id,
                    title: format!("Group {}", group.id),
                    tile: TileContent::Stub {
                        kind: AppKind::Terminal,
                        label: format!("Group {}", group.id),
                    },
                    app_title: None,
                });
                group.active_tab = tab_id;
            }
            tree.seed_doc_from_cache();
            let state_dir = tempfile::tempdir().expect("state dir");
            tree.persist_path = Some(state_dir.path().join("state.json"));
            tree.reorder_group_for_tests(strip_id, ids[0], 2); // [2,3,1]

            let (_, workspaces_id) = tree.doc.get(&ROOT, "workspaces").unwrap().unwrap();
            let (_, ws0) = tree.doc.get(&workspaces_id, 0_usize).unwrap().unwrap();
            let (_, strips_map_id) = tree.doc.get(&ws0, "strips").unwrap().unwrap();
            let (_, strip0) = tree.doc.get(&strips_map_id, 0_usize).unwrap().unwrap();
            let (_, groups_id) = tree.doc.get(&strip0, "window_groups").unwrap().unwrap();
            let mut doc_order = Vec::new();
            for i in 0..tree.doc.length(&groups_id) {
                let (_, g) = tree.doc.get(&groups_id, i).unwrap().unwrap();
                if let Some((Value::Scalar(s), _)) = tree.doc.get(&g, "id").unwrap()
                    && let ScalarValue::Uint(id) = s.as_ref()
                {
                    doc_order.push(*id);
                }
            }
            assert_eq!(
                doc_order,
                vec![2, 3, 1],
                "the doc's window_groups list must mirror the cache reorder",
            );
            let reloaded = crate::shell::persist::load_tree_from(
                tree.persist_path.as_ref().expect("persistence configured"),
            )
            .expect("reordered tree reloads");
            assert_eq!(
                group_order(&reloaded, strip_id),
                vec![2, 3, 1],
                "a normal restart must retain the reordered group list"
            );
        }

        // Phase `move_group_carries_a_group_across_strips_in_cache_and_doc`.
        {
            use tilo_desktop::intents::SessionIntent;

            let (mut tree, upper, _ids) = tree_with_n_groups(3);
            for group in &mut tree.desktop.workspaces[0].strips[0].window_groups {
                let tab_id = 10_000 + group.id;
                group.tabs.push(Tab {
                    id: tab_id,
                    title: format!("Group {}", group.id),
                    tile: TileContent::Stub {
                        kind: AppKind::Terminal,
                        label: format!("Group {}", group.id),
                    },
                    app_title: None,
                });
                group.active_tab = tab_id;
            }
            let lower = StripId::new();
            tree.desktop.workspaces[0].strips.push(Strip {
                id: lower,
                name: "Below".into(),
                window_groups: Vec::new(),
            });
            tree.seed_doc_from_cache();
            let state_dir = tempfile::tempdir().expect("state dir");
            tree.persist_path = Some(state_dir.path().join("state.json"));

            assert!(tree.commit_intent_for_tests(SessionIntent::MoveGroup {
                group: 2,
                to: lower,
                before: None,
            }));
            assert!(tree.commit_intent_for_tests(SessionIntent::MoveGroup {
                group: 3,
                to: lower,
                before: Some(2),
            }));
            assert_eq!(group_order(&tree, upper), vec![1]);
            assert_eq!(group_order(&tree, lower), vec![3, 2]);
            assert!(
                !tree.commit_intent_for_tests(SessionIntent::MoveGroup {
                    group: 1,
                    to: lower,
                    before: Some(404),
                }),
                "a stale anchor moves nothing"
            );
            assert_eq!(group_order(&tree, upper), vec![1]);

            let reloaded = crate::shell::persist::load_tree_from(
                tree.persist_path.as_ref().expect("persistence configured"),
            )
            .expect("moved tree reloads");
            assert_eq!(group_order(&reloaded, upper), vec![1]);
            assert_eq!(
                group_order(&reloaded, lower),
                vec![3, 2],
                "a restart must find the groups in the strip they were carried to"
            );
            let carried = reloaded.desktop.workspaces[0].strips[1]
                .window_groups
                .iter()
                .find(|group| group.id == 2)
                .expect("group 2 reloads");
            assert_eq!(carried.active_tab, 10_002, "the group keeps its tabs");
        }
    }

    /// Exercise layout edits as one long-lived desktop does, checking the
    /// durable file after every operation rather than only at the end. This
    /// catches an edit that happens to be repaired by a later, unrelated
    /// flush (the exact shape that made closed windows and widths appear to
    /// restore themselves in a real session).
    #[test]
    #[serial_test::serial(next_id_statics)]
    fn mixed_layout_edits_survive_every_restart_boundary() {
        use crate::shell::persist::load_tree_from;
        use crate::shell::tab_binding::{Placement, adopt_toplevel_at, remove_tab};

        let state_dir = tempfile::tempdir().expect("state dir");
        let state_path = state_dir.path().join("state.json");
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.persist_path = Some(state_path.clone());

        let second = tree.create_hosted_workspace("Project").0;
        let reloaded = load_tree_from(&state_path).expect("workspace creation persisted");
        assert!(
            reloaded
                .desktop
                .workspaces
                .iter()
                .any(|workspace| workspace.id == second),
            "a crash immediately after workspace creation must not lose it"
        );

        tree.set_active(second);
        let second_strip = tree.desktop.active_strip[&second];
        let first_tab = adopt_toplevel_at(
            &mut tree,
            101,
            Some("org.example.First".into()),
            Placement::NewGroup,
        );
        // Foreign adoption is process-local until the first durable layout
        // edit. Re-seed to model the symbolic launch record that a normal
        // compositor checkpoint carries across restart.
        tree.seed_doc_from_cache();
        tree.queue_save();
        let first_group = tree
            .desktop
            .active_group_for(second_strip)
            .expect("first group");
        let second_tab = adopt_toplevel_at(
            &mut tree,
            202,
            Some("org.example.Second".into()),
            Placement::NewGroup,
        );
        tree.seed_doc_from_cache();
        tree.queue_save();
        let second_group = tree
            .desktop
            .active_group_for(second_strip)
            .expect("second group");

        assert!(tree.reorder_group_for_tests(second_strip, second_group, 0));
        let reloaded = load_tree_from(&state_path).expect("group reorder persisted");
        assert_eq!(
            group_order(&reloaded, second_strip),
            vec![second_group, first_group],
            "group order must survive immediately, without a later flush"
        );

        assert!(tree.move_tab_across_strips(
            second_strip,
            tree.desktop.active_strip[&tree.desktop.workspaces[0].id],
            first_tab,
            90_001,
            DropTarget::GroupGap { gap_index: 0 },
        ));
        let reloaded = load_tree_from(&state_path).expect("cross-strip move persisted");
        let first_tab_locations = reloaded
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .filter(|strip| {
                strip
                    .window_groups
                    .iter()
                    .flat_map(|group| &group.tabs)
                    .any(|tab| tab.id == first_tab)
            })
            .map(|strip| strip.id)
            .collect::<Vec<_>>();
        assert_eq!(
            first_tab_locations,
            vec![tree.desktop.active_strip[&tree.desktop.workspaces[0].id]],
            "a moved tab must exist exactly once in its destination after restart"
        );

        assert!(remove_tab(&mut tree, second_tab));
        tree.queue_save();
        let reloaded = load_tree_from(&state_path).expect("close persisted");
        assert!(
            reloaded
                .desktop
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .all(|tab| tab.id != second_tab),
            "a closed tab must remain absent at the immediate restart boundary"
        );
        assert!(
            reloaded
                .desktop
                .active_group
                .get(&second_strip)
                .is_none_or(|group| reloaded
                    .desktop
                    .workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.strips)
                    .find(|strip| strip.id == second_strip)
                    .is_some_and(|strip| strip.window_groups.iter().any(|item| item.id == *group))),
            "mixed moves and closes must not leave a dangling active-group pointer"
        );
    }
}

#[cfg(test)]
mod active_workspace_peer_count_tests {
    //! Coverage for [`active_workspace_peer_count`] — the shared helper
    //! used by chrome that summarizes the active workspace roster.

    use super::*;
    use crate::shell::topbar_chrome::TopBarChrome;
    use std::collections::HashMap;

    /// Build a single-workspace tree with the given collaborators on the
    /// active workspace. The tree-level roster is left empty so this
    /// isolates the per-workspace path from the fallback path.
    fn tree_with_active_collaborators(collabs: Vec<Collaborator>) -> WorkspaceTree {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let ws = {
            let mut workspace = Workspace::new(ws_id, String::new());
            workspace.id = ws_id;
            workspace.name = "Active".into();
            workspace.strips = vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups: vec![],
            }];
            workspace.collaborators = collabs;
            workspace
        };
        let mut active_strip = HashMap::new();
        active_strip.insert(ws_id, strip_id);
        WorkspaceTree {
            desktop: DesktopTree {
                workspaces: vec![ws],
                active: ws_id,
                active_strip,
                active_group: HashMap::new(),
                collaborators: vec![],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn topbar_uses_shared_active_workspace_peer_count() {
        // Two explicit collaborators on the active workspace — one is the
        // local user ("me"). Chrome must expose only the remote peer.
        let tree = tree_with_active_collaborators(vec![
            Collaborator::new("me", "Me"),
            Collaborator::new("alice", "Alice"),
        ]);

        let topbar = TopBarChrome::from_tree_with_local_user(&tree.desktop, local_user_id());
        assert_eq!(active_workspace_peer_count(&tree), 1);
        assert_eq!(topbar.collaborator_initials().len(), 1);
    }
}

#[cfg(test)]
mod next_id_seed_tests;
#[cfg(test)]
mod hosted_workspace_tests {
    use super::*;

    fn room_of(tree: &WorkspaceTree, id: WorkspaceId) -> RoomId {
        tree.desktop
            .workspaces
            .iter()
            .chain(&tree.archived_workspaces)
            .find(|workspace| workspace.id == id)
            .map(Workspace::room)
            .expect("the workspace is here")
    }

    /// Making a workspace mints its room, makes the workspace in it joining
    /// and waits for exactly that room to be placed on a host.
    #[test]
    fn a_new_workspace_waits_for_exactly_its_own_room_to_be_placed() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let first: Vec<RoomId> = tree.pending_placements.iter().copied().collect();
        assert_eq!(
            first,
            [room_of(&tree, tree.desktop.active)],
            "the first boot's workspace waits to be placed too"
        );
        tree.take_pending_placements();

        let (id, room) = tree.create_hosted_workspace("Research");
        assert!(!room.is_nil());
        assert_eq!(
            room_of(&tree, id),
            room,
            "the workspace is the room minted for it"
        );
        assert_eq!(
            tree.pending_placements.iter().copied().collect::<Vec<_>>(),
            [room],
            "exactly one placement waits"
        );
        assert!(
            !tree.is_joining(room),
            "the person's own new workspace is never a join"
        );
        assert!(!tree.holds_room(room));

        assert_eq!(
            tree.take_pending_placements()
                .into_iter()
                .collect::<Vec<_>>(),
            [room]
        );
        assert!(tree.take_pending_placements().is_empty(), "taken once");
        assert!(
            !tree.is_joining(room),
            "nor a join once its placement is taken"
        );
    }

    /// Nothing done to a workspace afterwards gives it another room: not a
    /// rename, not archiving and restoring it, not saving and loading it.
    #[test]
    fn a_new_workspaces_room_never_changes() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.json");
        let mut tree = WorkspaceTree::bootstrap_default();
        let (id, room) = tree.create_hosted_workspace("Research");
        tree.set_active(id);

        if let Some(workspace) = tree.desktop.workspaces.iter_mut().find(|w| w.id == id) {
            workspace.name = "Renamed".into();
        }
        assert_eq!(room_of(&tree, id), room);
        assert!(tree.archive_workspace(id));
        assert_eq!(room_of(&tree, id), room);
        assert_eq!(tree.restore_archived(room), Some(id));
        assert_eq!(room_of(&tree, id), room);
        assert_eq!(
            tree.join_native_room("Again", room),
            Some(id),
            "joining it finds it"
        );

        crate::shell::persist::save_tree_to(&tree, &path).expect("save");
        let loaded = crate::shell::persist::load_tree_from(&path).expect("load");
        assert_eq!(room_of(&loaded, id), room);
        assert!(
            loaded.pending_placements.contains(&room),
            "a placement not yet taken survives a restart"
        );
    }

    /// A room marked as being joined that no enrollment is entering is
    /// marked no more after a restart; its workspace and name stay.
    #[test]
    fn a_join_mark_with_nothing_entering_is_cleared_on_load() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.json");
        let mut tree = WorkspaceTree::bootstrap_default();
        let room = RoomId::new();
        let id = tree.join_native_room("Plans", room).expect("joined");
        assert!(tree.is_joining(room));

        crate::shell::persist::save_tree_to(&tree, &path).expect("save");
        let loaded = crate::shell::persist::load_tree_from(&path).expect("load");
        assert!(!loaded.is_joining(room), "nothing enters the room");
        let workspace = loaded
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)
            .expect("the workspace stays");
        assert_eq!(workspace.room(), room);
        assert_eq!(workspace.name, "Plans");
    }
}
#[cfg(test)]
mod preset_assignment {
    use super::*;
    use tilo_action_policy::{ActionKind, Answer, KindSet, Lifetime, Rule, TreePath};

    fn builds() -> TreePath {
        TreePath::root().app("org.tilo.Builds")
    }

    fn approve() -> TreePath {
        builds()
            .kind("builds.release")
            .resource("builds.release")
            .action("release.approve")
    }

    fn preset() -> Preset {
        Preset {
            name: "Release reviewer".into(),
            description: "Read Builds and approve releases".into(),
            rules: vec![
                Rule::new(builds(), KindSet::READ, Answer::Allow),
                Rule::new(approve(), KindSet::UNDOABLE, Answer::Allow),
            ],
        }
    }

    #[test]
    fn presets_are_governance_only_and_round_trip_through_the_room() {
        let now = tilo_clock::unix_now();
        // Phase `owner_can_save_and_atomically_assign_a_preset`.
        {
            let mut workspace = Workspace::new(WorkspaceId::new(), "Demo");
            workspace.set_owner("owner");
            workspace.save_preset_as_at("owner", preset(), now).unwrap();
            let ids = workspace
                .assign_preset_as_at(
                    "owner",
                    "release REVIEWER",
                    &tilo_action_policy::Subject::person("ada"),
                    &Lifetime::UntilRevoked,
                    now,
                )
                .unwrap();
            assert_eq!(ids.len(), 2);
            assert_eq!(workspace.presets.len(), 1);
            assert_eq!(
                workspace.answer_at(
                    "ada",
                    &workspace.node().join(&approve()),
                    ActionKind::Undoable,
                    now
                ),
                Answer::Allow
            );
        }

        // Phase `someone_who_may_not_grant_cannot_save_or_assign_presets`.
        {
            let mut workspace = Workspace::new(WorkspaceId::new(), "Demo");
            workspace.set_owner("owner");
            assert!(matches!(
                workspace.save_preset_as_at("ada", preset(), now),
                Err(PresetMutationError::Refused(_))
            ));
            workspace.save_preset_as_at("owner", preset(), now).unwrap();
            let before = workspace.policy.clone();
            assert!(matches!(
                workspace.assign_preset_as_at(
                    "ada",
                    "Release reviewer",
                    &tilo_action_policy::Subject::person("ada"),
                    &Lifetime::UntilRevoked,
                    now,
                ),
                Err(PresetMutationError::Refused(_))
            ));
            assert_eq!(workspace.policy, before);
        }

        // Phase `preset_definitions_round_trip_through_the_room_document`.
        {
            let id = WorkspaceId::new();
            let mut workspace = Workspace::new(id, "Demo");
            workspace.set_owner("owner");
            workspace.save_preset_as_at("owner", preset(), now).unwrap();
            let mut tree = WorkspaceTree {
                desktop: DesktopTree {
                    workspaces: vec![workspace],
                    active: id,
                    ..Default::default()
                },
                ..WorkspaceTree::default()
            };
            tree.seed_doc_from_cache();
            tree.desktop.workspaces[0].presets.clear();
            tree.rebuild_cache_from_doc();
            assert_eq!(tree.desktop.workspaces[0].presets, vec![preset()]);
        }
    }
}

#[cfg(test)]
mod workspace_tree_lifecycle_tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the ownership-transfer regression keeps signing, persistence, replay, and rejection checks together"
    )]
    fn continuations_reconcile_safely_and_offers_stay_dormant_and_anonymous() {
        // Phase `continuation_reconcile_keeps_local_and_claimed_tabs`.
        {
            // Phase `continuation_reconcile_removes_a_closed_remote_tab_but_not_local_tabs`.
            {
                let mut tree = WorkspaceTree::bootstrap_default();
                let room = RoomId::new();
                tree.join_room("Plans", room);
                let device = crate::shell::personal_continuity::DeviceId::new("laptop").unwrap();
                let app = crate::shell::personal_continuity::AppInstanceState::new(
                    crate::shell::personal_continuity::AppInstanceId(0x99),
                    crate::shell::personal_continuity::TabPlacement {
                        strip: 0,
                        group: 0,
                        tab: 0,
                    },
                    crate::shell::personal_continuity::LaunchIntent::app("so.tilo.browser")
                        .unwrap(),
                    None,
                    crate::shell::personal_continuity::ContinuityStamp { counter: 1, device },
                );
                assert!(tree.materialize_continuation_workspace(
                    "Laptop",
                    &crate::shell::personal_continuity::WorkspaceContinuation {
                        room,
                        apps: vec![app],
                    },
                    &crate::shell::doc_open::DesktopContinuationLabels,
                ));
                assert!(tree.continuation_tabs.contains(&0x99));
                assert!(tree.reconcile_continuation_tabs(&BTreeSet::new()));
                assert!(!tree.continuation_tabs.contains(&0x99));
            }

            // Phase `claimed_continuation_tab_becomes_local_and_survives_remote_reconciliation`.
            {
                let mut tree = WorkspaceTree::bootstrap_default();
                let room = RoomId::new();
                tree.join_room("Plans", room);
                let app = crate::shell::personal_continuity::AppInstanceState::new(
                    crate::shell::personal_continuity::AppInstanceId(0x9a),
                    crate::shell::personal_continuity::TabPlacement {
                        strip: 0,
                        group: 0,
                        tab: 0,
                    },
                    crate::shell::personal_continuity::LaunchIntent::app("so.tilo.browser")
                        .unwrap(),
                    None,
                    crate::shell::personal_continuity::ContinuityStamp {
                        counter: 1,
                        device: crate::shell::personal_continuity::DeviceId::new("laptop").unwrap(),
                    },
                );
                tree.materialize_continuation_workspace(
                    "Laptop",
                    &crate::shell::personal_continuity::WorkspaceContinuation {
                        room,
                        apps: vec![app],
                    },
                    &crate::shell::doc_open::DesktopContinuationLabels,
                );

                assert!(tree.claim_continuation_tab(0x9a));
                assert!(!tree.continuation_tabs.contains(&0x9a));
                assert!(!tree.reconcile_continuation_tabs(&BTreeSet::new()));
                assert!(tree.desktop.workspaces.iter().any(|workspace| {
                    workspace
                        .strips
                        .iter()
                        .flat_map(|strip| &strip.window_groups)
                        .flat_map(|group| &group.tabs)
                        .any(|tab| tab.id == 0x9a)
                }));
            }
        }

        // Phase `continuation_offers_stay_dormant_live_and_free_of_identifiers`.
        {
            // Phase `continuation_offer_tracks_live_placement_and_title`.
            {
                let mut tree = WorkspaceTree::bootstrap_default();
                let room = RoomId::new();
                tree.join_room("Plans", room);
                let device = crate::shell::personal_continuity::DeviceId::new("laptop").unwrap();
                let make = |strip, title_app: &str| {
                    crate::shell::personal_continuity::AppInstanceState::new(
                        crate::shell::personal_continuity::AppInstanceId(0x9b),
                        crate::shell::personal_continuity::TabPlacement {
                            strip,
                            group: 0,
                            tab: 0,
                        },
                        crate::shell::personal_continuity::LaunchIntent::app(title_app).unwrap(),
                        None,
                        crate::shell::personal_continuity::ContinuityStamp {
                            counter: u64::from(strip) + 1,
                            device: device.clone(),
                        },
                    )
                };
                tree.materialize_continuation_workspace(
                    "Laptop",
                    &crate::shell::personal_continuity::WorkspaceContinuation {
                        room,
                        apps: vec![make(0, "so.tilo.browser")],
                    },
                    &crate::shell::doc_open::DesktopContinuationLabels,
                );
                tree.materialize_continuation_workspace(
                    "Laptop",
                    &crate::shell::personal_continuity::WorkspaceContinuation {
                        room,
                        apps: vec![make(1, "so.tilo.music")],
                    },
                    &crate::shell::doc_open::DesktopContinuationLabels,
                );
                let workspace = tree
                    .desktop
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.room() == room)
                    .unwrap();
                assert!(
                    !workspace.strips[0]
                        .window_groups
                        .iter()
                        .flat_map(|group| &group.tabs)
                        .any(|tab| tab.id == 0x9b)
                );
                let moved = workspace.strips[1]
                    .window_groups
                    .iter()
                    .flat_map(|group| &group.tabs)
                    .find(|tab| tab.id == 0x9b)
                    .unwrap();
                assert!(matches!(
                    &moved.tile,
                    TileContent::PeerWindow { app_id, .. } if app_id == "so.tilo.music"
                ));
            }

            // Phase `continuation_placeholders_do_not_render_device_or_record_identifiers`.
            {
                use crate::shell::personal_continuity::{
                    AppInstanceId, AppInstanceState, ContinuityStamp, DeviceId, LaunchIntent,
                    TabPlacement, WorkspaceContinuation,
                };

                let mut tree = WorkspaceTree::bootstrap_default();
                let room = RoomId::new();
                let source = "ab".repeat(32);
                tree.join_room("Plans", room);
                let stamp = ContinuityStamp {
                    counter: 1,
                    device: DeviceId::new("laptop").unwrap(),
                };
                assert!(tree.materialize_continuation_workspace(
                    &source,
                    &WorkspaceContinuation {
                        room,
                        apps: vec![AppInstanceState::new(
                                AppInstanceId(0x61),
                                TabPlacement {
                                    strip: 0,
                                    group: 0,
                                    tab: 0,
                                },
                                LaunchIntent::file_reference(
                                    "record:private-id",
                                    Some([2; 32]),
                                    None
                                )
                                .unwrap(),
                                None,
                                stamp,
                            )],
                    },
                    &crate::shell::doc_open::DesktopContinuationLabels,
                ));
                let workspace = tree
                    .desktop
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.room() == room)
                    .unwrap();
                let tab = &workspace.strips[0].window_groups[0].tabs[0];
                assert!(!workspace.name.contains(&source));
                assert!(!tab.title.contains(&source));
                assert!(!tab.title.contains("private-id"));
                let TileContent::Stub { label, .. } = &tab.tile else {
                    panic!("continuation file remains a dormant file card");
                };
                assert!(!label.contains(&source));
                assert!(!label.contains("private-id"));
            }
        }
    }

    #[test]
    fn relay_authority_never_turns_session_grants_into_timeless_nas_access() {
        use tilo_action_policy::{Answer, Grant, KindSet, Lifetime, Subject, TreePath};
        let mut workspace = Workspace::new(WorkspaceId(1), "A");
        let room = workspace.policy.room.clone();
        let reach = |id: &str, person: &str, lifetime: Lifetime| {
            Grant::new(
                id,
                Subject::Person(person.to_owned()),
                TreePath::room(&room),
                KindSet::READ,
                Answer::Allow,
                Subject::Person("owner".into()),
            )
            .with_lifetime(lifetime)
        };
        let once = "07".repeat(32);
        let session = "08".repeat(32);
        let persistent = "09".repeat(32);
        workspace.grant(reach("once", &once, Lifetime::Once));
        workspace.grant(reach(
            "session",
            &session,
            Lifetime::Session(tilo_desktop_model::session_id().to_owned()),
        ));
        workspace.grant(reach("persistent", &persistent, Lifetime::UntilRevoked));
        let readers: Vec<String> = workspace
            .room_readers_at(tilo_clock::unix_now())
            .into_iter()
            .map(|(person, _)| person)
            .collect();
        assert!(!readers.contains(&once));
        assert!(!readers.contains(&session));
        assert!(readers.contains(&persistent));
    }

    #[test]
    fn newer_app_winner_clears_an_old_file_reference_for_the_same_tab() {
        use crate::shell::personal_continuity::{
            AppInstanceId, AppInstanceState, ContinuityStamp, DeviceId, LaunchIntent, TabPlacement,
            WorkspaceContinuation,
        };

        let mut tree = WorkspaceTree::bootstrap_default();
        let room = RoomId::new();
        tree.join_room("Plans", room);
        let id = AppInstanceId(0x55);
        let placement = TabPlacement {
            strip: 0,
            group: 0,
            tab: 0,
        };
        let stamp = ContinuityStamp {
            counter: 1,
            device: DeviceId::new("laptop").unwrap(),
        };
        let file = AppInstanceState::new(
            id,
            placement,
            LaunchIntent::file_reference("record:book-55", Some([1; 32]), None).unwrap(),
            None,
            stamp.clone(),
        );
        assert!(tree.materialize_continuation_workspace(
            "Laptop",
            &WorkspaceContinuation {
                room,
                apps: vec![file],
            },
            &crate::shell::doc_open::DesktopContinuationLabels,
        ));
        assert!(tree.continuation_file_references.contains_key(&id.0));

        let app = AppInstanceState::new(
            id,
            placement,
            LaunchIntent::app("so.tilo.browser").unwrap(),
            None,
            stamp,
        );
        assert!(tree.materialize_continuation_workspace(
            "Laptop",
            &WorkspaceContinuation {
                room,
                apps: vec![app.clone()],
            },
            &crate::shell::doc_open::DesktopContinuationLabels,
        ));
        assert!(!tree.continuation_file_references.contains_key(&id.0));
        // Every checkpoint offers it again; one that moved nothing changes
        // nothing, so no document or save follows.
        let heads = tree.current_heads();
        assert!(!tree.materialize_continuation_workspace(
            "Laptop",
            &WorkspaceContinuation {
                room,
                apps: vec![app],
            },
            &crate::shell::doc_open::DesktopContinuationLabels,
        ));
        assert_eq!(tree.current_heads(), heads);
        let replacement = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)
            .into_iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|tab| tab.id == id.0)
            .expect("the newer app offer remains in the same logical tab");
        assert!(matches!(
            &replacement.tile,
            TileContent::PeerWindow { app_id, .. } if app_id == "so.tilo.browser"
        ));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn effect_receipts_are_durable_audited_activity() {
        // Phase `an_applications_effect_is_recorded_without_its_arguments`.
        {
            let mut tree = WorkspaceTree::bootstrap_default();
            let workspace = tree.desktop.active;
            let sent = |app: &str| {
                tilo_actions::ActionContext::for_resource("mail.message:7", "mail.message", "Draft")
                    .with_app(app, "Mail")
                    .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
                        tilo_actions::ActionKey::new(app, "mail.message", "send"),
                        [(
                            "to".to_owned(),
                            tilo_actions::Scalar::from("private@example.com"),
                        )],
                    )))
            };
            let args = |tree: &WorkspaceTree, invocation: tilo_actions::InvocationId| {
                let receipt = tree.desktop.workspaces[0]
                    .effect_receipts
                    .iter()
                    .find(|receipt| receipt.invocation == invocation)
                    .unwrap()
                    .clone();
                match receipt.context.what {
                    Some(tilo_actions::Did::Verb(verb)) => verb.args.len(),
                    other => panic!("the receipt keeps what was done: {other:?}"),
                }
            };
            let app = tilo_actions::InvocationId::new();
            assert!(tree.append_app_effect_receipt(
                workspace,
                app,
                "person",
                sent("org.tilo.Mail"),
                tilo_actions::EffectReceiptStatus::Succeeded,
            ));
            assert_eq!(args(&tree, app), 0, "an app's arguments stay private");
            let host = tilo_actions::InvocationId::new();
            assert!(tree.append_workspace_effect_receipt(
                workspace,
                host,
                "person",
                sent(WORKSPACE_GOVERNANCE_APP_ID),
                tilo_actions::EffectReceiptStatus::Succeeded,
            ));
            assert_eq!(
                args(&tree, host),
                1,
                "the host's own record keeps what it did"
            );
        }

        // Phase `effect_receipt_is_durable_semantic_activity_not_an_app_edit`.
        {
            let mut tree = WorkspaceTree::bootstrap_default();
            let workspace = tree.desktop.active;
            let mut app_document = automerge::AutoCommit::new();
            let app_heads_before = app_document.get_heads();
            let invocation = tilo_actions::InvocationId::new();
            let context = tilo_actions::ActionContext::for_resource(
                "mail.message:42",
                "mail.message",
                "Release update",
            )
            .with_app("org.tilo.Mail", "Mail")
            .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
                tilo_actions::ActionKey::new("org.tilo.Mail", "mail.message", "send"),
                [],
            )));

            assert!(tree.append_workspace_effect_receipt(
                workspace,
                invocation,
                "ada",
                context,
                tilo_actions::EffectReceiptStatus::Failed {
                    code: "smtp_rejected".into(),
                },
            ));
            assert!(!tree.append_workspace_effect_receipt(
                workspace,
                invocation,
                "ada",
                tilo_actions::ActionContext::default(),
                tilo_actions::EffectReceiptStatus::Succeeded,
            ));
            assert_eq!(app_document.get_heads(), app_heads_before);

            tree.resync_cache_from_doc();
            let receipt = &tree.desktop.active_workspace().unwrap().effect_receipts[0];
            assert_eq!(receipt.invocation, invocation);
            assert_eq!(receipt.status.label(), "Failed");
            assert!(
                matches!(&receipt.context.what, Some(tilo_actions::Did::Verb(verb)) if verb.action.action == "send"),
                "the receipt records what the effect was: {:?}",
                receipt.context
            );

            let entry = ui::HistorySource::new()
                .entries(&mut tree.doc)
                .into_iter()
                .find(|entry| {
                    entry.context.as_ref().is_some_and(|context| {
                        context
                            .facets
                            .iter()
                            .any(|facet| facet.key == "tilo.receipt" && facet.value == "effect")
                    })
                })
                .expect("effect receipt appears in workspace activity");
            assert!(
                !entry.is_revertible(),
                "external work is not a CRDT undo claim"
            );
        }
    }
}

#[cfg(test)]
mod strip_command_tests {
    use super::*;
    use tilo_desktop_model::commands::DesktopCommand;

    /// Creating and renaming a strip run the model's own reductions: a
    /// created strip is named, appended and focused, a bad name is refused
    /// with the tree untouched, and another device's strip is theirs to name.
    #[test]
    fn strips_are_created_and_renamed_through_the_model_commands() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let ws = tree.desktop.active;
        let made = tree
            .commit_strip_command(&DesktopCommand::CreateStrip {
                name: "  Research ".into(),
            })
            .expect("created");
        let workspace = tree.desktop.active_workspace().unwrap();
        assert_eq!(
            workspace.strips.last().map(|s| (s.id, s.name.as_str())),
            Some((made, "Research"))
        );
        assert_eq!(tree.desktop.active_strip.get(&ws), Some(&made));

        let first = workspace.strips[0].id;
        tree.commit_strip_command(&DesktopCommand::RenameStrip {
            strip: first.raw(),
            name: "Lounge".into(),
        })
        .expect("renamed");
        assert_eq!(
            tree.desktop.active_workspace().unwrap().strips[0].name,
            "Lounge"
        );

        let names = |tree: &WorkspaceTree| -> Vec<(StripId, String)> {
            tree.desktop
                .active_workspace()
                .unwrap()
                .strips
                .iter()
                .map(|s| (s.id, s.name.clone()))
                .collect()
        };
        let before = names(&tree);
        assert!(
            tree.commit_strip_command(&DesktopCommand::CreateStrip { name: " ".into() })
                .is_err()
        );
        assert!(
            tree.commit_strip_command(&DesktopCommand::RenameStrip {
                strip: first.raw(),
                name: "a\u{7}b".into(),
            })
            .is_err()
        );
        assert_eq!(names(&tree), before);

        let other_salt = tree.local_salt().wrapping_add(1).max(1);
        let theirs = StripId(compose_id(other_salt, 42));
        assert!(!tree.may_rename_strip(theirs));
        tree.desktop.workspaces[0]
            .strips
            .push(Strip::new(theirs, "Theirs"));
        assert!(
            tree.commit_strip_command(&DesktopCommand::RenameStrip {
                strip: theirs.raw(),
                name: "Mine".into(),
            })
            .is_err(),
            "a peer's strip is renamed by its owner"
        );
        assert!(
            tree.commit_strip_command(&DesktopCommand::MoveStrip {
                strip: theirs.raw(),
                before: None,
            })
            .is_err(),
            "a peer's strip is moved by its owner"
        );

        // A strip of this machine moves in the stack, and the room
        // document carries the new order.
        let order_before = names(&tree);
        let last = order_before.last().map(|(id, _)| *id).expect("strips");
        tree.commit_strip_command(&DesktopCommand::MoveStrip {
            strip: made.raw(),
            before: Some(first.raw()),
        })
        .expect("moved");
        let moved = names(&tree);
        assert_eq!(moved.first().map(|(id, _)| *id), Some(made));
        assert_eq!(moved.len(), order_before.len());
        assert_eq!(moved.last().map(|(id, _)| *id), Some(last));
    }
}
