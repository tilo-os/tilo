use automerge::{AutoCommit, ChangeHash};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use tilo_automerge::ApplyIncoming;
use tilo_automerge::WriteTyped as _;
use tilo_clock::unix_now_i64;

mod local_app_records;
mod tree_handle;
mod window_hosting;
use super::fs_authority::schedule_fs_authority_publication;
#[cfg(test)]
use super::fs_authority::wait_for_fs_authority_publication_idle;
pub use local_app_records::LocalAppRecord;
pub use tree_handle::{
    WeakWorkspaceTreeHandle, WorkspaceTreeHandle, WorkspaceTreeWriteGuard, next_policy_revision,
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
    WorkspaceEffectReceipt, WorkspaceId, WorkspaceTask, WorkspaceTaskAuditEvent,
    WorkspaceTaskResource, WorkspaceTaskRoute, WorkspaceTaskStatus, compose_id, counter_of,
    peer_salt_of, role_from_id, role_id,
};
pub use tilo_desktop_model::{
    DesktopTree, Display, DropTarget, OutputPartition, STATE_SCHEMA_VERSION, TabMove,
};

/// Fold this device's schedule claims into those a merged room document
/// carries. A claim the merge lost to an older concurrent one is kept (the
/// newer fire time is the answer), and claims for shortcuts the room no
/// longer holds are dropped.
fn merge_schedule_claims(
    merged: &mut Vec<tilo_automation::ScheduleClaim>,
    local: &[tilo_automation::ScheduleClaim],
    shortcuts: &[tilo_automation::Shortcut],
) {
    for claim in local {
        match merged.iter_mut().find(|held| held.key() == claim.key()) {
            Some(held) if claim.supersedes(held) => *held = claim.clone(),
            Some(_) => {}
            None => merged.push(claim.clone()),
        }
    }
    merged.retain(|claim| {
        shortcuts
            .iter()
            .any(|shortcut| shortcut.id == claim.shortcut)
    });
    merged.sort_by_key(tilo_automation::ScheduleClaim::key);
}

#[cfg(test)]
mod identity_fs_tests {
    /// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
    const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

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
            let deadline = std::time::Instant::now()
                .checked_add(HANG_BOUND)
                .expect("the hang bound fits in an Instant");
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
            let primary_relay_scope = crate::shell::network::room_topic_id(room_a.room).to_string();
            let secondary_relay_scope =
                crate::shell::network::room_topic_id(room_b.room).to_string();
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
        let id = self.0.strip_prefix("iroh:")?;
        let peer = id.parse::<iroh::EndpointId>().ok()?;
        Some(crate::network::behaviour::peer_liveness_handle(&peer))
    }
}

impl std::fmt::Display for PeerOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who may write a room's governance — its owner and capability list —
/// as this machine knows it. Automerge merges a map key
/// last-writer-wins with no per-key ACL, so without this record
/// governance would be decided by which actor id sorts higher.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoomAuthority {
    /// This machine minted the room, so its cache is the governance.
    Local,
    /// A named peer, cross-checked against the invite token's signature.
    Peer(PeerOrigin),
    /// Joined without an invite naming an administrator, so nobody may
    /// write the room's governance: every governance change is refused —
    /// the first snapshot included — until an invite
    /// ([`WorkspaceTree::pin_room_authority`]), the person's signed
    /// workspace directory ([`WorkspaceTree::adopt_personal_room`]) or a
    /// continuation offer from the person's own device
    /// ([`WorkspaceTree::adopt_continuation_room`]) names an authority. A
    /// state file written while a room still waited for its first answer to
    /// pin a governor loads as this same read-only value; no `SyncResp` ever
    /// pins it.
    Unpinned,
    /// The room belongs to a person (their user root key), not to one of
    /// their devices: every authenticated, unrevoked device of that person
    /// governs it, this one included when it is theirs.
    Person([u8; 32]),
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

use std::sync::atomic::{AtomicU16, AtomicU64, Ordering as AtomicOrdering};

/// Process-wide counter behind [`FreshId::new`] and `next_id` — this
/// compositor's host-side [`tilo_desktop_model::IdMinter`], kept as two
/// atomics because every subsystem mints through it concurrently.
/// [`WorkspaceTree::seed_next_id_from_self`] advances it past the highest
/// persisted id at load, or a fresh process restarts at `1` and collides
/// with an already-loaded entry. Only the low 48 bits come from here; the
/// high 16 carry [`NEXT_ID_PEER_SALT`] so two peers minting in parallel
/// never collide — otherwise `apply_changes` folds two distinct
/// workspaces with the same id into one.
pub(crate) static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// High-16-bit peer salt for every id `next_id` mints. Default `0`
/// keeps tests on `0x0001, 0x0002, …`; session bootstrap
/// (`compositor::session_bootstrap::load_session_tree`) overwrites it at
/// startup with the salt stored beside the state file. `Relaxed`: written
/// once at boot, nothing piggy-backs on visibility.
pub(crate) static NEXT_ID_PEER_SALT: AtomicU16 = AtomicU16::new(0);

pub(crate) fn next_id() -> u64 {
    let counter = NEXT_ID.fetch_add(1, AtomicOrdering::Relaxed);
    compose_id(NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed), counter)
}

/// The process-wide id source as a [`MintIds`], for model operations that
/// create windows, groups or strips.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessIds;

impl MintIds for ProcessIds {
    fn mint_id(&mut self) -> u64 {
        next_id()
    }
}

/// Mint a fresh workspace or strip id from the process-wide counter.
pub trait FreshId: Sized {
    /// Allocate a fresh, process-wide unique id.
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

/// The user root key a person id names, when the id is one: the lowercase
/// hex [`person_id_of`] spells it as.
fn person_root(person: &str) -> Option<[u8; 32]> {
    let root: [u8; 32] = hex::decode(person).ok()?.try_into().ok()?;
    (person_id_of(root).as_deref() == Some(person)).then_some(root)
}

/// Who put a room on the person's workspace list; see
/// [`WorkspaceTree::adopt_personal_room`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum RoomListing {
    /// The person's signed directory, which names the room's governor.
    Directory,
    /// The machine that enrolled this device, which hands over only the id.
    Enrolled,
}

pub fn local_user_id() -> UserId {
    #[cfg(test)]
    crate::shell::test_support::ambient_start("session identity");
    if let Ok(v) = std::env::var("TILO_USER_ID")
        && !v.is_empty()
    {
        return v;
    }
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
pub fn local_presence_hue() -> ui::tokens::PresenceHue {
    const FRESH: std::time::Duration = std::time::Duration::from_secs(1);
    if let Ok(known) = LOCAL_PRESENCE_HUE.lock()
        && let Some((at, hue)) = *known
        && at.elapsed() < FRESH
    {
        return hue;
    }
    read_local_presence_hue()
}

static LOCAL_PRESENCE_HUE: std::sync::Mutex<Option<(std::time::Instant, ui::tokens::PresenceHue)>> =
    std::sync::Mutex::new(None);

/// [`local_presence_hue`] read from the settings document and identity
/// now, for a caller that knows the settings just changed.
pub fn read_local_presence_hue() -> ui::tokens::PresenceHue {
    let hue =
        ui::tokens::PresenceHue::chosen_in(&serde_json::Value::Object(settings_store::load()))
            .unwrap_or_else(|| ui::tokens::PresenceHue::of_identity(&local_user_id()));
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

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OwnershipTransferStateError {
    #[error("workspace ownership transfer is not locally governed")]
    NotGoverned,
    #[error("workspace or current structural owner does not match the signed proposal")]
    WrongWorkspaceOrOwner,
    #[error("recipient must remain a member until commit")]
    RecipientNotMember,
    #[error("another live ownership-transfer proposal already exists")]
    ProposalAlreadyPending,
    #[error("ownership-transfer proposal is unknown, cancelled, expired, or already consumed")]
    ProposalUnavailable,
    #[error("ownership-transfer audit history is full; archive it before another transfer")]
    HistoryFull,
    #[error("invalid signed ownership-transfer record: {0}")]
    InvalidSignature(#[from] collab_types::OwnershipTransferError),
    #[error("ownership-transfer persistence failed: {0}")]
    Persistence(String),
}

const MAX_OWNERSHIP_TRANSFER_HISTORY: usize = 4_096;
const MAX_OWNERSHIP_OFFER_SECONDS: u64 = 7 * 24 * 60 * 60;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceTree {
    /// Native authority is never re-derived from desktop caches.
    #[serde(default)]
    pub native_authority: super::native_workspace_owner::NativeAuthorityStorage,
    #[serde(skip)]
    pub native_runtime: super::native_workspace_owner::NativeStateRuntime,
    /// Enrolled rooms whose governor stewards their channels: a node (a
    /// host its person's root let own workspaces). Their records are sealed channels
    /// every member opens itself, never documents the governor commits.
    /// Projected from the owner, so not persisted.
    #[serde(skip)]
    pub stewarded_rooms: BTreeSet<RoomId>,
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
    /// One replication document per room — [`Self::doc`] describes
    /// **every** workspace, so handing it to a peer would leak every
    /// other room's state. Maintained by [`Self::sync_room_doc`];
    /// re-projected from the cache after a restart.
    #[serde(skip, default)]
    pub room_docs: HashMap<RoomId, AutoCommit>,
    /// Who may write each room's governance — see [`RoomAuthority`]. A
    /// room absent from this map is one this machine minted and governs.
    /// Persisted, unlike [`Self::room_docs`] — can't be re-derived.
    #[serde(default)]
    pub room_authority: HashMap<RoomId, RoomAuthority>,
    /// What each peer is **called**, as Iroh `EndpointId` →
    /// display name. Written only by the inviter (non-spoofable);
    /// [`Self::apply_room_changes`] refuses it from anyone else.
    #[serde(default)]
    pub room_peer_names: HashMap<RoomId, std::collections::BTreeMap<String, String>>,
    /// Certified endpoint→human-root associations admitted by the room
    /// governor. A peer announcement never writes this map directly.
    #[serde(default)]
    pub room_peer_identities:
        HashMap<RoomId, std::collections::BTreeMap<String, GovernedPeerIdentity>>,
    /// For a joined room, the `NEXT_ID` value at the moment of
    /// joining — windows minted before that aren't projected in, or
    /// accepting an invite would publish every window already open. A
    /// counter, not a timestamp, so the line survives a restart.
    #[serde(default)]
    pub room_share_floor: HashMap<RoomId, u64>,
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
    /// The person behind each transport-authenticated peer whose device
    /// certificate verifies and is not revoked, as the network layer last
    /// checked it. Session-local: a peer is only here while authenticated.
    #[serde(skip)]
    pub authenticated_persons: HashMap<PeerOrigin, [u8; 32]>,
    /// Rooms introduced only by another device's private continuation offer.
    /// Once a person takes ownership of an offered tab the room becomes local
    /// and is removed from this set.
    #[serde(default)]
    pub continuation_rooms: BTreeSet<RoomId>,
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
    /// This device as host of its legacy windows, which rooms carry as
    /// hosted here instead of as windows to reopen. Set at session start.
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
}

impl Default for WorkspaceTree {
    fn default() -> Self {
        Self {
            desktop: DesktopTree::default(),
            native_authority: super::native_workspace_owner::NativeAuthorityStorage::default(),
            native_runtime: super::native_workspace_owner::NativeStateRuntime::default(),
            stewarded_rooms: BTreeSet::new(),
            policy_revision: 0,
            doc: AutoCommit::new(),
            room_docs: HashMap::new(),
            room_authority: HashMap::new(),
            room_peer_names: HashMap::new(),
            room_peer_identities: HashMap::new(),
            room_share_floor: HashMap::new(),
            continuation_tabs: BTreeSet::new(),
            local_person: None,
            authenticated_persons: HashMap::new(),
            continuation_rooms: BTreeSet::new(),
            continuation_owned_tabs: HashMap::new(),
            continuation_closed_tabs: HashMap::new(),
            continuation_file_references: HashMap::new(),
            native_document_tabs: HashMap::new(),
            local_app_records: HashMap::new(),
            package_launches: HashMap::new(),
            persist_path: None,
            fs_acl_path: None,
            window_hosting: None,
            personal_device_choices: HashMap::new(),
            personal_device_requests: Vec::new(),
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

/// Owner, capability blobs, peer-name and identity claims, coverage,
/// invitation receipts and media policy read from a room document's
/// governance keys. See [`WorkspaceTree::governance_of`].
type RoomGovernance = tilo_peer_state::room::RoomGovernance;

impl WorkspaceTree {
    pub(crate) fn installed_native_owner(
        &self,
    ) -> Option<super::native_workspace_owner::NativeWorkspaceOwner> {
        self.native_runtime.owner()
    }

    pub(crate) fn requires_native_owner(&self, room: RoomId) -> bool {
        self.native_authority.requires(room) || self.native_runtime.requires_uncertain(room)
    }

    /// Whether `room`'s app records are documents its native owner commits.
    /// In a room whose governor stewards channels they are sealed instead:
    /// each app seals and opens its record's channel itself.
    pub(crate) fn commits_native_documents(&self, room: RoomId) -> bool {
        self.requires_native_owner(room) && !self.stewarded_rooms.contains(&room)
    }

    /// Presentation projection from the actual admitted owner, never a second admission.
    pub(crate) fn project_native_room(&mut self, room: RoomId, peer: &tilo_peer_state::StatePeer) {
        let Some(document) = peer.admitted_room() else {
            return;
        };
        if let Ok(endpoint) = iroh::EndpointId::from_bytes(&peer.authority().device_public_key) {
            self.room_authority
                .insert(room, RoomAuthority::Peer(PeerOrigin::peer(&endpoint)));
        }
        // A node: an always-on host its person's root let own workspaces.
        // The relay it runs stewards the channels of the rooms it governs.
        let governor = &peer.authority().capabilities;
        if governor.can_own_workspaces && governor.can_act_as_host && !governor.foreign {
            self.stewarded_rooms.insert(room);
        } else {
            self.stewarded_rooms.remove(&room);
        }
        let (name, policy) = tilo_peer_state::room::extract_governed_fields(document);
        let sandbox = projection::room_sandbox_of(document);
        let presets = projection::workspace_presets_of(document);
        let tasks = projection::workspace_tasks_of(document);
        let receipts = projection::workspace_effect_receipts_of(document);
        self.room_docs.insert(room, document.clone());
        self.queue_inbound_records(room, document);
        self.merge_room_contents(room);
        if let Some(ws) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|ws| ws.room == room)
        {
            if let Some(name) = name {
                ws.name = name;
            }
            let local = self.native_runtime.preserve_files_home_consent(ws, peer);
            ws.policy = policy;
            ws.presets = presets;
            ws.sandbox = sandbox;
            ws.sandbox.extend(local);
            ws.tasks = tasks;
            ws.effect_receipts = receipts;
        }
        self.desktop.room_epoch = self.desktop.room_epoch.wrapping_add(1);
    }

    /// In a room another device governs, a window its owner placed for an
    /// in-process app kind is opened here as that app's own instance (see
    /// `peer_windows`), on the records the owner created for it.
    fn open_governed_placeholders_here(&mut self, room: RoomId) -> bool {
        let Some(ws) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|ws| ws.room == room)
        else {
            return false;
        };
        let mut changed = false;
        for tab in ws
            .strips
            .iter_mut()
            .flat_map(|strip| &mut strip.window_groups)
            .flat_map(|group| &mut group.tabs)
        {
            if let TileContent::AppPlaceholder { kind, label } = &tab.tile {
                tracing::debug!(%room, tab = ?tab.id, app = %kind.app_id(), "opening a governed room's window as its app");
                tab.tile = TileContent::PeerWindow {
                    app_id: kind.app_id(),
                    label: label.clone(),
                    note: String::new(),
                    missing: None,
                };
                changed = true;
            }
        }
        changed
    }

    /// The record the room document names for `tab` of `room`, if any.
    pub(crate) fn room_tab_record(&self, room: RoomId, tab: TabId) -> Option<String> {
        tilo_desktop_model::apps::record_routes(self.room_docs.get(&room)?).remove(&tab)
    }

    /// Set the process-wide peer salt `next_id` stamps into the high
    /// 16 bits of every freshly-allocated id. Default `0` keeps
    /// `WorkspaceId(1)`-style test literals working; two peers under
    /// distinct salts can never collide. Takes `&mut self` even though
    /// the salt is process-global, so a per-tree-salt refactor stays a
    /// one-line change.
    pub fn set_peer_salt(&mut self, salt: u16) {
        Self::set_process_peer_salt(salt);
    }

    /// [`Self::set_peer_salt`] without a tree to call it on. Must run
    /// **before** the tree is loaded: ids minted before the salt exists
    /// land in the shared 0-salt range, identical on every first boot,
    /// and the room document keys a window group by id — so two such
    /// peers fold two distinct windows into one. Re-seeding the counter
    /// afterward does not repair already-minted ids.
    pub fn set_process_peer_salt(salt: u16) {
        NEXT_ID_PEER_SALT.store(salt, AtomicOrdering::Relaxed);
    }

    /// Read the current process-wide peer salt. Mostly used by tests to
    /// assert on the salt without poking the static directly.
    pub fn peer_salt(&self) -> u16 {
        NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed)
    }

    /// The salt replication treats as this tree's own — the id range
    /// this tree may write and sweep. `Self::peer_salt_override`
    /// first, the process-wide salt otherwise.
    pub fn local_salt(&self) -> u16 {
        self.desktop
            .peer_salt_override
            .unwrap_or_else(|| NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed))
    }

    /// Give this tree a salt without touching the process-wide one, for
    /// holding two trees at once. The caller must mint matching ids
    /// itself; `next_id` reads the static.
    pub fn set_salt_override(&mut self, salt: u16) {
        self.desktop.peer_salt_override = Some(salt);
    }

    /// Walk every id and advance `NEXT_ID` past the highest counter
    /// stamped by the local peer salt — idempotent, called after
    /// deserializing. The counter restarts at `1` each process, so
    /// without this it collides with persisted ids. Only local-salt ids
    /// count; a remote peer's must not advance our counter.
    pub fn seed_next_id_from_self(&self) {
        let local_salt = NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed);
        let mut max_counter: u64 = 0;
        let mut consider = |id: u64| {
            if peer_salt_of(id) == local_salt {
                max_counter = max_counter.max(counter_of(id));
            }
        };
        for ws in &self.desktop.workspaces {
            consider(ws.id.raw());
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
    /// [`Self::room_docs`]: a candidate to encode and maybe install, without
    /// copying every document and its whole history. Before installing it,
    /// [`Self::adopt_documents_of`] the tree it was copied from.
    pub(crate) fn clone_without_documents(&mut self) -> Self {
        let doc = std::mem::replace(&mut self.doc, AutoCommit::new());
        let room_docs = std::mem::take(&mut self.room_docs);
        let copy = self.clone();
        self.doc = doc;
        self.room_docs = room_docs;
        copy
    }

    /// Take `source`'s documents into a tree made by
    /// [`Self::clone_without_documents`]. A room document this tree already
    /// holds is newer than `source`'s and stays. Anything written to this
    /// tree's placeholder [`Self::doc`] is dropped: reseed it from the cache.
    pub(crate) fn adopt_documents_of(&mut self, source: &mut Self) {
        self.doc = std::mem::replace(&mut source.doc, AutoCommit::new());
        for (room, doc) in source.room_docs.drain() {
            self.room_docs.entry(room).or_insert(doc);
        }
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
            .map(|w| w.room)
            .filter(|r| !r.is_nil() && seen.insert(*r))
            .collect()
    }

    /// Who may write `room`'s governance. No recorded authority means
    /// this machine minted the room and governs it; joining another's
    /// room ([`Self::set_room`]) records [`RoomAuthority::Unpinned`],
    /// pinned to the named owner on invite accept
    /// ([`Self::pin_room_authority`]). Until then the room refuses every
    /// governance change.
    pub fn room_authority(&self, room: RoomId) -> RoomAuthority {
        self.room_authority
            .get(&room)
            .cloned()
            .unwrap_or(RoomAuthority::Local)
    }

    /// Does this machine hold `room`'s governance? A room minted here does,
    /// and so does a room governed by the person this device belongs to
    /// once that person is its owner. A room the person's own device
    /// offered for continuation is pinned to the person before its
    /// governance arrives; it may turn out to be someone else's room, which
    /// the person's devices relay but do not govern.
    pub fn governs_room(&self, room: RoomId) -> bool {
        match self.room_authority(room) {
            RoomAuthority::Local => true,
            RoomAuthority::Person(person) => {
                self.local_person == Some(person)
                    && self.workspace_in_room(room).is_some_and(|workspace| {
                        workspace.owner().is_some()
                            && workspace.owner().cloned() == person_id_of(person)
                    })
            }
            RoomAuthority::Peer(_) | RoomAuthority::Unpinned => false,
        }
    }

    /// Whether `from` is another device of the person this device belongs to
    /// and that person owns `room`: then it governs `room` as much as this
    /// device does. The person behind `from` is what the transport
    /// authenticated, never what a payload claims.
    fn same_person_governs(&self, room: RoomId, from: &PeerOrigin) -> bool {
        let Some(local) = self.local_person else {
            return false;
        };
        if self.authenticated_persons.get(from) != Some(&local) {
            return false;
        }
        let Principal::Person(person) =
            Principal::person_from_key(collab_types::crypto::Ed25519PublicKey::from_bytes(local))
        else {
            return false;
        };
        self.workspace_in_room(room)
            .is_some_and(|workspace| workspace.is_owner(&person))
    }

    /// Name the peer `peer_id` (iroh `EndpointId`) `name` in
    /// `room`, as `actor`. Deliberately the **inviter's** act, not the
    /// invitee's — a peer naming itself could claim any name. Refused
    /// unless `actor` may administer `room`. An empty `name` un-names
    /// the peer.
    pub fn name_peer(&mut self, room: RoomId, actor: &UserId, peer_id: &str, name: &str) -> bool {
        let peer_id = peer_id.trim();
        if room.is_nil() || peer_id.is_empty() {
            return false;
        }
        // Storing an unparseable peer id would leave a key no presence
        // frame can ever match — reads later as "didn't replicate"
        // rather than "was never addressed to anyone".
        if peer_id.parse::<iroh::EndpointId>().is_err() {
            return false;
        }
        if !self.governs_room(room) {
            return false;
        }
        let Some(ws) = self.workspace_in_room(room) else {
            return false;
        };
        if !ws.may_administer(actor) {
            return false;
        }
        let name = name.trim();
        let entry = self.room_peer_names.entry(room).or_default();
        let changed = if name.is_empty() {
            let removed_name = entry.remove(peer_id).is_some();
            // Retain the signed identity row as room-governance evidence.
            // It is not projected without a name, but its additive revocations
            // must survive un-naming so a later re-name cannot resurrect a
            // revoked endpoint.
            removed_name
        } else {
            entry
                .insert(peer_id.to_string(), name.to_string())
                .as_deref()
                != Some(name)
        };
        if entry.is_empty() {
            self.room_peer_names.remove(&room);
        }
        if changed {
            self.sync_room_doc(room);
            self.queue_save();
        }
        true
    }

    /// Admit a root-signed device certificate under a governed peer name.
    /// Only the room-governing machine may call this successfully; the
    /// transport-authenticated endpoint must be the certified device key.
    pub fn certify_peer_identity(
        &mut self,
        room: RoomId,
        peer_id: &str,
        device_cert: tilo_user_identity::DeviceCert,
    ) -> bool {
        self.certify_peer_identity_with_revocations(room, peer_id, device_cert, Vec::new())
    }

    /// Admit a certificate and additive, root-signed revocation evidence.
    pub fn certify_peer_identity_with_revocations(
        &mut self,
        room: RoomId,
        peer_id: &str,
        device_cert: tilo_user_identity::DeviceCert,
        revocations: Vec<tilo_user_identity::RevocationEntry>,
    ) -> bool {
        if !self.governs_room(room)
            || tilo_user_identity::verify_device_cert(&device_cert).is_err()
            || device_cert.is_expired_at(tilo_clock::unix_now_ns())
            || device_cert.device_label.is_empty()
            || device_cert.device_label.trim() != device_cert.device_label
            || device_cert.device_label.len()
                > tilo_user_identity::MAX_TRUSTED_IDENTITY_DISPLAY_NAME_BYTES
            || device_cert.device_label.chars().any(char::is_control)
        {
            return false;
        }
        if revocations.len() > tilo_user_identity::MAX_TRUSTED_IDENTITY_PROJECTION_DEVICES
            || revocations.iter().any(|entry| {
                entry.user_public_key != device_cert.user_public_key
                    || tilo_user_identity::verify_revocation_entry(entry).is_err()
            })
        {
            return false;
        }
        let Ok(peer) = peer_id.parse::<iroh::EndpointId>() else {
            return false;
        };
        if peer.as_bytes() != &device_cert.device_public_key
            || !self
                .room_peer_names
                .get(&room)
                .is_some_and(|names| names.contains_key(peer_id))
        {
            return false;
        }
        let identities = self.room_peer_identities.entry(room).or_default();
        if !identities.contains_key(peer_id)
            && identities.len() >= tilo_user_identity::MAX_TRUSTED_IDENTITY_PROJECTION_DEVICES
        {
            return false;
        }
        let mut governed_revocations = identities
            .values()
            .filter(|identity| identity.device_cert.user_public_key == device_cert.user_public_key)
            .flat_map(|identity| identity.revocations.iter().cloned())
            .chain(revocations)
            .collect::<Vec<_>>();
        governed_revocations
            .sort_by_key(|entry| (entry.revoked_device_public_key, entry.revoked_at_ns));
        governed_revocations.dedup_by_key(|entry| entry.revoked_device_public_key);
        // Re-certifying a device keeps what governance knows it as.
        let delegate = identities
            .get(peer_id)
            .filter(|known| known.device_cert == device_cert)
            .and_then(|known| known.delegate.clone());
        let row = GovernedPeerIdentity {
            device_cert,
            revocations: governed_revocations,
            delegate,
        };
        let changed = identities.insert(peer_id.to_string(), row.clone()).as_ref() != Some(&row);
        if changed {
            self.sync_room_doc(room);
            self.queue_save();
        }
        changed
    }

    /// Current Files projection derived solely from governance names and
    /// root-signed device certificates in the accepted room document.
    pub fn trusted_identity_projection(
        &self,
        room: RoomId,
    ) -> tilo_user_identity::TrustedIdentityProjectionV2 {
        let names = self.room_peer_names_in_doc(room);
        let identities = self.room_peer_identities_in_doc(room);
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
    pub fn verified_agents(&self, room: RoomId) -> Vec<ui::history_source::RosterAgent> {
        self.trusted_identity_projection(room)
            .humans
            .into_iter()
            .flat_map(|human| {
                let person = human.root_public_key;
                let person_name = human.display_name;
                human.devices.into_iter().filter_map(move |device| {
                    let agent = device.agent?;
                    Some(ui::history_source::RosterAgent {
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
        let named = if self.governs_room(room) {
            self.room_peer_names.get(&room).cloned().unwrap_or_default()
        } else {
            self.room_peer_names_in_doc(room)
        };
        named
            .keys()
            .filter_map(|peer| peer.parse::<iroh::EndpointId>().ok())
            .collect()
    }

    pub fn room_peer_labels(&self, room: RoomId) -> std::collections::BTreeMap<u64, String> {
        let mut out = std::collections::BTreeMap::new();
        // Room documents aren't persisted, so a just-restarted machine
        // holds none until something exports. On a machine that governs
        // the room, falling back to the cache avoids showing invitees
        // as numbers for the first few seconds; a guest gets no such
        // fallback.
        let named = if self.governs_room(room) {
            self.room_peer_names.get(&room).cloned().unwrap_or_default()
        } else {
            self.room_peer_names_in_doc(room)
        };
        for (peer, name) in named {
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
        let identities = if self.governs_room(room) {
            self.room_peer_identities
                .get(&room)
                .cloned()
                .unwrap_or_default()
        } else {
            self.room_peer_identities_in_doc(room)
        };
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
                    person_id_of(cert.user_public_key)?,
                ))
            })
            .collect()
    }

    /// Record that `room`'s governance is written by `peer` and nobody
    /// else. The value comes from a validated invite token (room id and
    /// owner peer id both inside the ed25519 signature), so a peer
    /// can't name itself authority. Refuses to overwrite an authority
    /// already pinned to a different peer.
    pub fn pin_room_authority(&mut self, room: RoomId, peer: PeerOrigin) -> bool {
        if room.is_nil() {
            return false;
        }
        if let Some(RoomAuthority::Peer(existing)) = self.room_authority.get(&room)
            && *existing != peer
        {
            tracing::warn!(
                target: "shell::state",
                room = %room,
                pinned = %existing,
                offered = %peer,
                "room authority already pinned to another peer; keeping it",
            );
            return false;
        }
        self.room_authority.insert(room, RoomAuthority::Peer(peer));
        self.queue_save();
        true
    }

    /// Put `who` on the roster of the workspace that addresses `room`.
    /// An **upsert**, not a push — a later frame with no explicit
    /// colour must not erase an existing one. Returns whether the
    /// roster changed.
    pub fn admit_collaborator_in_room(&mut self, room: RoomId, who: Collaborator) -> bool {
        if room.is_nil() {
            return false;
        }
        let Some(ws) = self.desktop.workspaces.iter_mut().find(|w| w.room == room) else {
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

    /// The cached workspace that addresses `room`, if this machine is in
    /// it. First match wins, in case of a room joined twice locally.
    fn workspace_in_room(&self, room: RoomId) -> Option<&Workspace> {
        if room.is_nil() {
            return None;
        }
        self.desktop.workspaces.iter().find(|w| w.room == room)
    }

    /// Project the cached state of `room`'s workspace into that room's
    /// replication document, writing only what differs; no-op when this
    /// machine is not in `room`.
    ///
    /// Every value is a map key, not a list element, so a re-projection
    /// under a fresh actor after a restart merges last-writer-wins
    /// instead of duplicating. A revoked capability is deleted from the
    /// map so the revocation reaches a peer already holding the grant —
    /// but the room document is not persisted, so that deletion has to
    /// be re-issued after a restart.
    pub fn sync_room_doc(&mut self, room: RoomId) {
        self.try_sync_room_doc(room);
    }

    /// Projection must not partly install governance when a policy fence fails.
    #[allow(
        clippy::too_many_lines,
        reason = "room-document projection keeps all capability map updates in one Automerge transaction"
    )]
    fn try_sync_room_doc(&mut self, room: RoomId) -> bool {
        use automerge::ROOT;
        use tilo_peer_state::room::media;

        if self.requires_native_owner(room) {
            return false;
        }

        let Some(ws) = self.workspace_in_room(room) else {
            return false;
        };
        let name = ws.name.clone();
        let policy = ws.policy.clone();
        let sandbox = ws.sandbox.clone();
        let presets = ws.presets.clone();
        let tasks = ws.tasks.clone();
        let effect_receipts = ws.effect_receipts.clone();
        let shortcuts = ws.shortcuts.clone();
        let schedule_claims = ws.schedule_claims.clone();
        let peer_names: std::collections::BTreeMap<String, String> =
            self.room_peer_names.get(&room).cloned().unwrap_or_default();
        let peer_identities = self
            .room_peer_identities
            .get(&room)
            .cloned()
            .unwrap_or_default();
        let held: std::collections::BTreeMap<String, String> = self
            .desktop
            .room_records_held
            .get(&room)
            .cloned()
            .unwrap_or_default();
        let strips: Vec<Strip> = self.window_hosting.as_ref().map_or_else(
            || ws.strips.clone(),
            |hosting| hosting.published(&ws.strips, &self.package_launches),
        );
        let room_str = room.to_string();
        let local_salt = self.local_salt();
        let share_floor = self.room_share_floor.get(&room).copied();
        // Governance is projected only by the machine that holds it —
        // a guest claiming ownership would put a competing claim on
        // the wire that `apply_room_changes` would only refuse anyway.
        let governs = self.governs_room(room);

        // The cache contains no media definitions. Preserve the admitted field
        // on a candidate; only the committed native owner may supply it anew.
        let before = self.room_docs.get(&room).cloned().unwrap_or_default();
        let mut candidate = before.clone();
        let doc = &mut candidate;

        // Identity first: the receiver checks this field against the
        // room it asked for, so an unlabelled document could be applied
        // to the wrong one.
        let _ = doc.put_str_if_changed(&ROOT, "room", &room_str);

        // The name is governed like the owner: only the governing machine
        // writes it. A member's local name (a device's "From another
        // device" placeholder before its first sync) would otherwise race
        // the real name in the shared document and could win.
        if governs {
            let _ = doc.put_str_if_changed(&ROOT, "name", &name);
        }

        // Projected by **every** member, unlike governance below: a room
        // a guest can't bring a record — or a window — into isn't shared.
        if let Err(err) = projection::project_room_records(doc, &held) {
            tracing::error!(?err, "room doc: records map");
        }
        if let Err(err) = projection::project_owned_contents(
            doc,
            &strips,
            local_salt,
            share_floor,
            projection::EmptyStrips::Skip,
        ) {
            tracing::error!(?err, "room doc: contents map");
        }
        if let Err(error) = projection::project_workspace_tasks(doc, &tasks) {
            tracing::error!(?error, "room doc: tasks map");
        }
        if let Err(error) = projection::project_effect_receipts(doc, &effect_receipts) {
            tracing::error!(?error, "room doc: effect receipts map");
        }
        if let Err(error) = projection::project_workspace_shortcuts(doc, &shortcuts) {
            tracing::error!(?error, "room doc: shortcuts map");
        }
        if let Err(error) = projection::project_schedule_claims(doc, &schedule_claims) {
            tracing::error!(?error, "room doc: schedule claims map");
        }

        if !governs {
            let _ = tilo_resource::history::commit_stamped(doc);
            doc.reset_diff_cursor();
            if media::validate_transition(Some(&before), doc, media::TransitionMode::Authoring)
                .is_err()
            {
                return false;
            }
            self.room_docs.insert(room, candidate);
            return true;
        }

        // Never unset an owner in a governed room: `adopt_ownerless_workspaces`
        // only ever fills it in.
        if policy.owner.is_some()
            && let Err(error) = projection::project_room_policy(doc, &policy)
        {
            tracing::error!(?error, room = %room_str, "room doc: policy");
            return false;
        }
        if let Err(error) = projection::project_room_sandbox(doc, &sandbox) {
            tracing::error!(?error, room = %room_str, "room doc: sandbox");
            return false;
        }
        if let Err(error) = projection::project_workspace_presets(doc, &presets) {
            tracing::error!(?error, room = %room_str, "room doc: presets");
            return false;
        }
        Self::project_peer_names(doc, &peer_names);
        Self::project_peer_identities(doc, &peer_identities);
        let _ = tilo_resource::history::commit_stamped(doc);
        // Nothing reads this doc through patches, so the log is pure
        // overhead — worse, `PatchLog::migrate_actors` panics when a
        // peer's actor can't be spliced into it in sort order.
        doc.reset_diff_cursor();
        if let Err(error) = media::refresh_authorization(&before, doc) {
            tracing::warn!(?error, room = %room, "room projection refused: invalid media policy");
            return false;
        }
        doc.reset_diff_cursor();
        self.room_docs.insert(room, candidate);
        true
    }

    /// Key the room document stores its peers' names under.
    const ROOM_PEERS_KEY: &'static str = "peers";
    const ROOM_PEER_IDENTITIES_KEY: &'static str = "peer_identities_v1";

    /// Project "who the peers in this room are", as Iroh `EndpointId`
    /// -> display name. Called only from the governing branch of
    /// [`Self::sync_room_doc`] — one writer, so no guest can rename
    /// itself or anybody else.
    fn project_peer_names(
        doc: &mut AutoCommit,
        names: &std::collections::BTreeMap<String, String>,
    ) {
        use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};

        let peers_id = if let Some((_, id)) = doc.get(&ROOT, Self::ROOM_PEERS_KEY).ok().flatten() {
            id
        } else {
            // Don't mint an empty map — a room nobody has named
            // anyone in produces no change at all.
            if names.is_empty() {
                return;
            }
            match doc.put_object(&ROOT, Self::ROOM_PEERS_KEY, ObjType::Map) {
                Ok(id) => id,
                Err(err) => {
                    tracing::error!(?err, "room doc: peers map");
                    return;
                }
            }
        };
        let stale: Vec<String> = doc
            .keys(&peers_id)
            .filter(|k| !names.contains_key(k.as_str()))
            .collect();
        for key in stale {
            let _ = doc.delete(&peers_id, key.as_str());
        }
        for (peer, name) in names {
            let unchanged = matches!(
                doc.get(&peers_id, peer).ok().flatten(),
                Some((Value::Scalar(s), _))
                    if matches!(s.as_ref(), ScalarValue::Str(v) if v.as_str() == name)
            );
            if !unchanged {
                let _ = doc.put(&peers_id, peer, name.clone());
            }
        }
    }

    fn project_peer_identities(
        doc: &mut AutoCommit,
        identities: &std::collections::BTreeMap<String, GovernedPeerIdentity>,
    ) {
        use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
        let object = if let Some((_, id)) = doc
            .get(&ROOT, Self::ROOM_PEER_IDENTITIES_KEY)
            .ok()
            .flatten()
        {
            id
        } else {
            if identities.is_empty() {
                return;
            }
            let Ok(id) = doc.put_object(&ROOT, Self::ROOM_PEER_IDENTITIES_KEY, ObjType::Map) else {
                return;
            };
            id
        };
        let stale = doc
            .keys(&object)
            .filter(|key| !identities.contains_key(key))
            .collect::<Vec<_>>();
        for key in stale {
            let _ = doc.delete(&object, key);
        }
        for (peer, identity) in identities {
            let Ok(bytes) = serde_json::to_vec(identity) else {
                continue;
            };
            let unchanged = matches!(
                doc.get(&object, peer).ok().flatten(),
                Some((Value::Scalar(value), _))
                    if matches!(value.as_ref(), ScalarValue::Bytes(current)
                        if current == &bytes
                            || serde_json::from_slice::<GovernedPeerIdentity>(current)
                                .is_ok_and(|admitted| admitted == *identity))
            );
            if !unchanged {
                let _ = doc.put(&object, peer, ScalarValue::Bytes(bytes));
            }
        }
    }

    pub fn room_peer_identities_in_doc(
        &self,
        room: RoomId,
    ) -> std::collections::BTreeMap<String, GovernedPeerIdentity> {
        Self::peer_identities_of(self.room_docs.get(&room))
    }

    /// The valid governed identities room document `doc` carries.
    fn peer_identities_of(
        doc: Option<&AutoCommit>,
    ) -> std::collections::BTreeMap<String, GovernedPeerIdentity> {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        let mut out = std::collections::BTreeMap::new();
        let Some(doc) = doc else {
            return out;
        };
        let Some((_, object)) = doc
            .get(&ROOT, Self::ROOM_PEER_IDENTITIES_KEY)
            .ok()
            .flatten()
        else {
            return out;
        };
        for peer in doc.keys(&object) {
            let Some((Value::Scalar(value), _)) = doc.get(&object, &peer).ok().flatten() else {
                continue;
            };
            let ScalarValue::Bytes(bytes) = value.as_ref() else {
                continue;
            };
            let Ok(identity) = serde_json::from_slice::<GovernedPeerIdentity>(bytes) else {
                continue;
            };
            let Ok(endpoint) = peer.parse::<iroh::EndpointId>() else {
                continue;
            };
            if endpoint.as_bytes() == &identity.device_cert.device_public_key
                && tilo_user_identity::verify_device_cert(&identity.device_cert).is_ok()
                && !identity
                    .device_cert
                    .is_expired_at(tilo_clock::unix_now_ns())
                && identity.revocations.len()
                    <= tilo_user_identity::MAX_TRUSTED_IDENTITY_PROJECTION_DEVICES
                && identity.revocations.iter().all(|entry| {
                    entry.user_public_key == identity.device_cert.user_public_key
                        && tilo_user_identity::verify_revocation_entry(entry).is_ok()
                })
            {
                out.insert(peer, identity);
            }
        }
        out
    }

    /// Every name the room document carries. Read from the **document**,
    /// not [`Self::room_peer_names`], so a guest and the host answer this
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
        let replicated = projection::contents_of(doc);
        let changed = projection::merge_room_contents(
            &mut self.desktop,
            room,
            replicated,
            projection::Reader::Owner(local_salt),
        );
        if self.requires_native_owner(room) {
            let presets = projection::workspace_presets_of(doc);
            let mut reprofiled = false;
            if let Some(ws) = self
                .desktop
                .workspaces
                .iter_mut()
                .find(|ws| ws.room == room)
                && ws.presets != presets
            {
                ws.presets = presets;
                reprofiled = true;
            }
            self.open_governed_placeholders_here(room) || reprofiled || changed
        } else {
            changed
        }
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

    /// The governance a room document records: owner and capability
    /// list — a peer able to write either controls the whole security
    /// policy. The room's **name** is deliberately not governed. The
    /// **id** is stricter still: a mismatched payload is refused outright.
    fn governance_of(doc: Option<&AutoCommit>) -> RoomGovernance {
        tilo_peer_state::room::governance_of(doc)
    }

    /// Current heads of `room`'s replication document — the baseline a
    /// peer sends so the answer carries only what it's missing. Empty
    /// for a room this machine is not in.
    pub fn room_heads(&mut self, room: RoomId) -> Vec<ChangeHash> {
        match self.room_docs.get_mut(&room) {
            Some(doc)
                if tilo_peer_state::room::media::validate_transition(
                    None,
                    doc,
                    tilo_peer_state::room::media::TransitionMode::Restore,
                )
                .is_ok() =>
            {
                doc.get_heads()
            }
            _ => Vec::new(),
        }
    }

    /// Changes to `room` since `since`, as opaque blobs to ship to a
    /// peer. `None` when this machine has no workspace in `room`; the
    /// document is brought up to date from the cache first.
    pub fn export_room_changes_since(
        &mut self,
        room: RoomId,
        since: &[ChangeHash],
    ) -> Option<Vec<Vec<u8>>> {
        self.workspace_in_room(room)?;
        if !self.try_sync_room_doc(room) {
            return None;
        }
        let doc = self.room_docs.get_mut(&room)?;
        tilo_peer_state::room::media::validate_transition(
            None,
            doc,
            tilo_peer_state::room::media::TransitionMode::Restore,
        )
        .ok()?;
        Some(
            doc.get_changes(since)
                .into_iter()
                .map(|c| c.raw_bytes().to_vec())
                .collect(),
        )
    }

    /// Whether `heads`, a peer's heads of `room`, name a change this
    /// machine does not hold.
    pub fn room_lacks_any(&mut self, room: RoomId, heads: &[ChangeHash]) -> bool {
        self.room_docs.get_mut(&room).is_some_and(|doc| {
            heads
                .iter()
                .any(|head| doc.get_change_meta_by_hash(head).is_none())
        })
    }

    /// The sender of a batch for `room` as this machine knows it: the
    /// transport-authenticated origin, and the person behind it when
    /// the network layer has authenticated that person. A sender with no
    /// authenticated person reached the tree through the network gate
    /// (`peer_room_allows`), which already decided it may change the room.
    fn room_writer<'a>(&self, from: &'a PeerOrigin) -> tilo_peer_state::room::Writer<'a> {
        use tilo_peer_state::room::{Standing, Writer};
        let standing = match self.authenticated_persons.get(from) {
            Some(person) => Standing::Member {
                person: *person,
                foreign: false,
            },
            None => Standing::TransportAuthorized,
        };
        Writer {
            key: from.as_str(),
            standing,
        }
    }

    /// True (and logs a warning) if `candidate` adds or changes a shortcut
    /// that claims to have been saved by the person this device acts for,
    /// while `from` is not one of that person's authenticated devices.
    ///
    /// `saved_by` is what arms a shortcut's triggers on this machine and
    /// what lets an unattended run honour the shortcut's advance consents,
    /// so it is authority, not metadata: a peer with write access to the room
    /// must not be able to assert it for someone else. Echoes of a shortcut
    /// this machine already holds are not writes.
    fn shortcut_authorship_refused(
        &self,
        room: RoomId,
        candidate: &AutoCommit,
        from: &PeerOrigin,
    ) -> bool {
        let Some(local) = self.local_person else {
            return false;
        };
        if self.authenticated_persons.get(from) == Some(&local) {
            return false;
        }
        let Some(local_id) = person_id_of(local) else {
            return false;
        };
        let held = self
            .room_docs
            .get(&room)
            .map_or_else(Vec::new, projection::workspace_shortcuts_of);
        let forged = projection::workspace_shortcuts_of(candidate)
            .into_iter()
            .find(|shortcut| {
                shortcut.saved_by.as_deref() == Some(local_id.as_str()) && !held.contains(shortcut)
            });
        match forged {
            Some(shortcut) => {
                tracing::warn!(
                    target: "shell::state",
                    room = %room,
                    from = %from,
                    shortcut = %shortcut.id,
                    "room sync refused: a peer wrote a shortcut as if this \
                     device's person had saved it",
                );
                true
            }
            None => false,
        }
    }

    /// Who may write `room`'s governance — its owner and capability list,
    /// which together control the whole security policy — for a batch from
    /// `from`. Only what this machine pinned from an invite, its own minting
    /// or the person's devices counts; nothing in the batch does.
    /// [`RoomAuthority::Unpinned`] names nobody, so a room joined without an
    /// invite naming its administrator refuses every governance change: the
    /// first peer to answer does not become its governor.
    fn room_authority_pin<'a>(
        &self,
        room: RoomId,
        authority: &'a RoomAuthority,
        local_person: Option<&'a [u8; 32]>,
        from: &PeerOrigin,
    ) -> tilo_peer_state::room::AuthorityPin<'a> {
        use tilo_peer_state::room::AuthorityPin;
        match authority {
            // We administer the room; only another device of the person who
            // owns it governs it alongside us.
            RoomAuthority::Local => match local_person {
                Some(person) if self.same_person_governs(room, from) => {
                    AuthorityPin::Person(person)
                }
                _ => AuthorityPin::Local,
            },
            RoomAuthority::Peer(pinned) => AuthorityPin::Device(pinned.as_str()),
            // A person's room: any of that person's authenticated devices.
            RoomAuthority::Person(person) => AuthorityPin::Person(person),
            RoomAuthority::Unpinned => AuthorityPin::Unpinned,
        }
    }

    /// A media snapshot cannot bootstrap its own authority. The invite path
    /// verifies the device signature before persisting the exact endpoint pin;
    /// transport authenticates `from` independently of the incoming document.
    /// Room documents are not durable, so that pin also authorizes rehydration.
    /// Network certificate checks and native media authorization remain mandatory.
    fn media_authority_admitted(
        &self,
        room: RoomId,
        candidate: &AutoCommit,
        from: &PeerOrigin,
    ) -> bool {
        let RoomAuthority::Peer(pinned) = self.room_authority(room) else {
            // Local rooms refuse any remote governance change separately. An
            // unchanged echo can preserve an already admitted local policy.
            return self.governs_room(room)
                && self.room_docs.get(&room).is_some_and(|before| {
                    tilo_peer_state::room::media::parse(before).is_ok_and(|policy| policy.is_some())
                });
        };
        let Some(endpoint) = pinned.as_str().strip_prefix("iroh:") else {
            return false;
        };
        let Ok(endpoint_key) = endpoint.parse::<iroh::EndpointId>() else {
            return false;
        };
        let before = Self::governance_of(self.room_docs.get(&room));
        let after = Self::governance_of(Some(candidate));
        let Some(proposed) = after
            .identities
            .get(endpoint)
            .and_then(|bytes| serde_json::from_slice::<GovernedPeerIdentity>(bytes).ok())
        else {
            return false;
        };
        let Ok(admitted) = before
            .identities
            .get(endpoint)
            .map(|bytes| serde_json::from_slice::<GovernedPeerIdentity>(bytes))
            .transpose()
        else {
            return false;
        };
        // A first snapshot must arrive from the preexisting invite-pinned
        // endpoint. An existing certificate cannot be replaced by the snapshot.
        if admitted.is_none() && from != &pinned {
            return false;
        }
        let certificate = &proposed.device_cert;
        let now = tilo_clock::unix_now_ns();
        let prior_owner = before.owner.as_deref().or_else(|| {
            self.workspace_in_room(room)
                .and_then(|workspace| workspace.owner().map(String::as_str))
        });
        if admitted
            .as_ref()
            .is_some_and(|admitted| admitted.device_cert != *certificate)
            || certificate.device_public_key != *endpoint_key.as_bytes()
            || certificate.is_expired_at(now)
            || certificate.issued_at_ns > now
            || tilo_user_identity::verify_device_cert(certificate).is_err()
            || after.owner.as_deref() != Some(hex::encode(certificate.user_public_key).as_str())
            || prior_owner.is_some_and(|owner| after.owner.as_deref() != Some(owner))
        {
            return false;
        }
        let mut revocations = tilo_user_identity::RevocationList::new();
        for bytes in before.identities.values().chain(after.identities.values()) {
            let Ok(identity) = serde_json::from_slice::<GovernedPeerIdentity>(bytes) else {
                return false;
            };
            for entry in identity.revocations {
                if entry.user_public_key != identity.device_cert.user_public_key
                    || revocations.insert(entry).is_err()
                {
                    return false;
                }
            }
        }
        !revocations.is_revoked_at(&certificate.device_public_key, now)
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

    /// Merge a peer's changes to `room` into the cached workspace, and
    /// report whether it changed.
    ///
    /// Admission is [`tilo_peer_state::room::admit_room_changes`], the same
    /// function the native room peer runs: the sender holds `Write`, the
    /// payload names this room, governance changes come only from the
    /// writer this machine pinned ([`Self::room_authority`] — never from
    /// whoever answers first), this machine's own windows are not rewritten,
    /// receipts are immutable, coverage and durable receipts never rewind and
    /// the media policy is consistent. This function adds only what is this
    /// machine's own storage: binding a media policy to the pinned device
    /// ([`Self::media_authority_admitted`]), the receipts already projected
    /// into the workspace, and the projection itself.
    ///
    /// Refusal is of the **whole batch**: a partial merge would advance
    /// local heads past rejected ops (the sender's next export then skips
    /// them, unrecoverably) while leaving those ops in the graph exported
    /// onward. This also blocks a stale lineage from resurrecting a
    /// capability revoked while that peer was away. A refused batch changes
    /// nothing here, the room's authority included.
    ///
    /// # Errors
    ///
    /// Returns the underlying `automerge::AutomergeError` if any blob in
    /// `changes` fails to decode, or if applying them to the room's
    /// document fails.
    #[allow(
        clippy::too_many_lines,
        reason = "one candidate-then-commit admission: every governance check runs before the batch installs"
    )]
    pub fn apply_room_changes(
        &mut self,
        room: RoomId,
        changes: &[Vec<u8>],
        from: &PeerOrigin,
    ) -> Result<bool, automerge::AutomergeError> {
        use tilo_peer_state::room::{
            AdmittedRoom, ContentOwnership, RoomAdmission, RoomRefusal, admit_room_changes,
        };
        if self.requires_native_owner(room) {
            return Ok(false);
        }
        if self.workspace_in_room(room).is_none() {
            return Ok(false);
        }
        // A local rename may still live only in the cache when a delayed
        // reply arrives. Stage it in a copy of the existing room document
        // before merging, so projection cannot erase it. Keep this private:
        // a malformed or unauthorized reply must leave our heads unchanged.
        // The room's name is not governed, so every check reads the same
        // governance from the staged copy as from the held document.
        let mut staged_before = None;
        if self.governs_room(room)
            && let Some(before) = self.room_docs.get(&room)
            && let Some(workspace) = self.workspace_in_room(room)
            && let Some(name) = tilo_peer_state::room::extract_governed_fields(before).0
            && name != workspace.name
        {
            use automerge::transaction::Transactable as _;
            let mut staged = before.clone();
            staged.put(automerge::ROOT, "name", workspace.name.clone())?;
            staged_before = Some(staged);
        }
        let before = staged_before.as_ref().or_else(|| self.room_docs.get(&room));
        // Another device of the person that governs the room may have named
        // or certified a peer; what it changed is adopted below.
        let governed_peers_before = self.governs_room(room).then(|| {
            (
                Self::peer_names_of(before),
                Self::peer_identities_of(before),
            )
        });
        let now = tilo_clock::unix_now();
        let authority = self.room_authority(room);
        let local_person = self.local_person;
        let room_id = room.to_string();
        // Applied to a copy first: a check that runs after the merge has
        // already happened is not a check.
        let admitted = admit_room_changes(
            RoomAdmission {
                before,
                room: &room_id,
                source: self.room_writer(from),
                authority: self.room_authority_pin(room, &authority, local_person.as_ref(), from),
                contents: ContentOwnership::Salted {
                    local_salt: self.local_salt(),
                },
                caps_now: now,
            },
            changes,
        );
        let AdmittedRoom { candidate, media } = match admitted {
            Ok(admitted) => admitted,
            // An undecodable batch keeps this API's error type; the detail
            // goes to the log.
            Err(RoomRefusal::Malformed(error)) => {
                tracing::warn!(%error, room = %room, "room changes refused");
                return Err(automerge::AutomergeError::Fail);
            }
            Err(refusal) => {
                tracing::warn!(
                    target: "shell::state",
                    room = %room,
                    from = %from,
                    authority = ?authority,
                    %refusal,
                    "room sync refused",
                );
                return Ok(false);
            }
        };
        if media.is_some() && !self.media_authority_admitted(room, &candidate, from) {
            tracing::warn!(
                target: "shell::state",
                room = %room,
                from = %from,
                "room sync refused: the media policy is not bound to the room's pinned device",
            );
            return Ok(false);
        }
        let media_changed = tilo_peer_state::room::media::governed_value(self.room_docs.get(&room))
            != tilo_peer_state::room::media::governed_value(Some(&candidate));
        if self.shortcut_authorship_refused(room, &candidate, from) {
            return Ok(false);
        }

        let (name, mut policy) = tilo_peer_state::room::extract_governed_fields(&candidate);
        let mut sandbox = projection::room_sandbox_of(&candidate);
        let presets = projection::workspace_presets_of(&candidate);
        let mut tasks = projection::workspace_tasks_of(&candidate);
        let shortcuts = projection::workspace_shortcuts_of(&candidate);
        let mut schedule_claims = projection::schedule_claims_of(&candidate);
        let mut effect_receipts = projection::workspace_effect_receipts_of(&candidate);
        // Receipts already projected into the workspace stay immutable even
        // once the room document omits them.
        if let Some(workspace) = self.workspace_in_room(room) {
            for existing in &workspace.effect_receipts {
                match effect_receipts
                    .iter()
                    .find(|receipt| receipt.invocation == existing.invocation)
                {
                    Some(received) if received != existing => {
                        tracing::warn!(
                            room = %room,
                            invocation = %existing.invocation.0,
                            "room sync refused: an effect receipt is immutable",
                        );
                        return Ok(false);
                    }
                    Some(_) => {}
                    None => effect_receipts.push(existing.clone()),
                }
            }
        }
        let records_changed = self.queue_inbound_records(room, &candidate);
        let peers_changed = governed_peers_before.is_some_and(|(names, identities)| {
            self.adopt_governed_peers(room, &names, &identities, &candidate)
        });
        if peers_changed {
            self.queue_save();
        }

        self.room_docs.insert(room, candidate);

        // Folds the room's windows into this machine's copy, preserving
        // view state; see `merge_room_contents`.
        let contents_changed = self.merge_room_contents(room);

        let Some(ws) = self.desktop.workspaces.iter_mut().find(|w| w.room == room) else {
            return Ok(records_changed || contents_changed || peers_changed);
        };
        let mut fields_changed = false;
        if let Some(name) = name
            && ws.name != name
        {
            ws.name = name;
            fields_changed = true;
        }
        // An owner is never unset by a document that does not name one.
        if policy.owner.is_none() {
            policy.owner.clone_from(&ws.policy.owner);
        }
        // Replaced rather than merged: a revoked grant or capability is
        // simply absent from the peer's document, so carrying the local
        // list over would let it survive.
        policy.grants.sort_by(|a, b| a.id.cmp(&b.id));
        let mut current = ws.policy.clone();
        current.grants.sort_by(|a, b| a.id.cmp(&b.id));
        if current != policy {
            ws.policy = policy;
            fields_changed = true;
        }
        sandbox.sort_by(|a, b| a.id.cmp(&b.id));
        let mut current = ws.sandbox.clone();
        current.sort_by(|a, b| a.id.cmp(&b.id));
        if current != sandbox {
            ws.sandbox = sandbox;
            fields_changed = true;
        }
        if ws.presets != presets {
            ws.presets = presets;
            fields_changed = true;
        }
        tasks.sort_by(|left, right| left.id.cmp(&right.id));
        let mut current_tasks = ws.tasks.clone();
        current_tasks.sort_by(|left, right| left.id.cmp(&right.id));
        if current_tasks != tasks {
            ws.tasks = tasks;
            fields_changed = true;
        }
        merge_schedule_claims(&mut schedule_claims, &ws.schedule_claims, &shortcuts);
        fields_changed |= ws.shortcuts != shortcuts;
        ws.shortcuts = shortcuts;
        fields_changed |= ws.schedule_claims != schedule_claims;
        ws.schedule_claims = schedule_claims;
        effect_receipts.sort_by_key(|receipt| receipt.invocation.0);
        let mut current_receipts = ws.effect_receipts.clone();
        current_receipts.sort_by_key(|receipt| receipt.invocation.0);
        if current_receipts != effect_receipts {
            ws.effect_receipts = effect_receipts;
            fields_changed = true;
        }

        let fields_changed = fields_changed || contents_changed;
        if fields_changed {
            // Mirror the merged state into the tree document and to disk.
            self.seed_doc_from_cache();
            self.queue_save();
        }
        let landed = fields_changed || records_changed || media_changed || peers_changed;
        if landed {
            // A peer's window is in the tree now but not on screen yet —
            // this runs on the network thread, and only the frame
            // thread splices the widget tree, on chrome revision bumps.
            self.desktop.room_epoch = self.desktop.room_epoch.wrapping_add(1);
        }
        Ok(landed)
    }

    /// Adopt what an admitted batch changed in `room`'s governed peer names
    /// and identities into this machine's own, when it governs the room too.
    /// Every device of the person who owns a room governs it, and each
    /// projects its own names into the shared document, sweeping the ones
    /// it does not hold: without adopting a co-governor's naming, two such
    /// devices delete and restore each other's entries forever, a change a
    /// second each way on a desktop nobody uses. Only the delta is taken,
    /// so an entry this machine holds and the batch did not touch stays.
    /// The admission already refused a governance change from anyone but
    /// the room's governor.
    fn adopt_governed_peers(
        &mut self,
        room: RoomId,
        names_before: &std::collections::BTreeMap<String, String>,
        identities_before: &std::collections::BTreeMap<String, GovernedPeerIdentity>,
        candidate: &AutoCommit,
    ) -> bool {
        fn adopt<V: Clone + PartialEq>(
            local: &mut std::collections::BTreeMap<String, V>,
            before: &std::collections::BTreeMap<String, V>,
            after: &std::collections::BTreeMap<String, V>,
        ) -> bool {
            let mut changed = false;
            for (key, value) in after {
                if before.get(key) != Some(value) && local.get(key) != Some(value) {
                    local.insert(key.clone(), value.clone());
                    changed = true;
                }
            }
            for key in before.keys().filter(|key| !after.contains_key(*key)) {
                changed |= local.remove(key).is_some();
            }
            changed
        }
        let names = self.room_peer_names.entry(room).or_default();
        let mut changed = adopt(names, names_before, &Self::peer_names_of(Some(candidate)));
        if names.is_empty() {
            self.room_peer_names.remove(&room);
        }
        let identities = self.room_peer_identities.entry(room).or_default();
        changed |= adopt(
            identities,
            identities_before,
            &Self::peer_identities_of(Some(candidate)),
        );
        if identities.is_empty() {
            self.room_peer_identities.remove(&room);
        }
        changed
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
        let actions = crate::shell::launcher::action_authority::Export::of(self);
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
                room: workspace.room,
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

    /// Give every workspace with no recorded owner to `user`, returning
    /// the ids adopted. Treating unowned as "owned by anyone" would leave
    /// every grant decision wide open, so this is the closed answer.
    ///
    /// A workspace in a room this machine does not govern is **not**
    /// adopted however ownerless it looks — it gave its governance up by
    /// joining someone else's room, and adopting would hand that room
    /// back to this machine on the next boot.
    pub fn adopt_ownerless_workspaces(&mut self, user: &UserId) -> Vec<WorkspaceId> {
        self.adopt_ownerless_workspaces_with(|_| user.clone())
    }

    /// Give each ownerless locally-governed workspace to the identity selected
    /// for its room. This is the multi-identity form of
    /// [`Self::adopt_ownerless_workspaces`].
    pub fn adopt_ownerless_workspaces_with(
        &mut self,
        identity_for_room: impl Fn(&RoomId) -> UserId,
    ) -> Vec<WorkspaceId> {
        let governed_elsewhere: Vec<RoomId> = self
            .room_authority
            .iter()
            .filter(|(_, a)| **a != RoomAuthority::Local)
            .map(|(room, _)| *room)
            .collect();
        let mut adopted = Vec::new();
        for ws in &mut self.desktop.workspaces {
            if ws.owner().is_none() && !governed_elsewhere.contains(&ws.room) {
                let user = identity_for_room(&ws.room);
                let before = ws.policy.clone();
                ws.set_owner(user.clone());
                adopted.push((ws.id, user, PolicyEdit::between(&before, &ws.policy)));
            }
        }
        for (id, user, edit) in &adopted {
            self.doc_set_owner(*id, user);
            self.doc_write_policy(*id, edit);
        }
        if !adopted.is_empty() {
            self.queue_save();
        }
        adopted.into_iter().map(|(id, _, _)| id).collect()
    }

    /// Hand every locally-governed workspace still owned by the OS account
    /// name `account` to the key identity selected for its room.
    ///
    /// Before a device has an identity, [`local_user_id`] falls back to the
    /// account name, so a workspace created then is owned by `"ada"`.
    /// Once the identity exists the same person acts as its key-derived
    /// principal, is no member of that workspace, and every app they open
    /// there is refused its collaboration route and exits before drawing.
    /// Rooms governed elsewhere keep whatever they say: there `account` may
    /// name somebody else entirely.
    pub fn rebind_account_owned_workspaces(
        &mut self,
        account: &str,
        identity_for_room: impl Fn(&RoomId) -> UserId,
    ) -> Vec<WorkspaceId> {
        let governed_elsewhere: Vec<RoomId> = self
            .room_authority
            .iter()
            .filter(|(_, a)| **a != RoomAuthority::Local)
            .map(|(room, _)| *room)
            .collect();
        let mut rebound = Vec::new();
        for ws in &mut self.desktop.workspaces {
            if !ws.is_owner(account) || governed_elsewhere.contains(&ws.room) {
                continue;
            }
            let user = identity_for_room(&ws.room);
            if user == account {
                continue;
            }
            let before = ws.policy.clone();
            let account_subject = tilo_action_policy::Subject::person(account);
            for grant in &mut ws.policy.grants {
                if grant.subject == account_subject {
                    grant.subject = tilo_action_policy::Subject::person(user.clone());
                }
            }
            let mut sandbox_granted = Vec::new();
            for cap in &mut ws.sandbox {
                if matches!(&cap.subject, Subject::Principal(Principal::Person(p)) if p == account)
                {
                    cap.subject = Subject::Principal(Principal::Person(user.clone()));
                    sandbox_granted.push(cap.id.clone());
                }
            }
            ws.set_owner(user.clone());
            let edit = PolicyEdit {
                sandbox_granted,
                ..PolicyEdit::between(&before, &ws.policy)
            };
            rebound.push((ws.id, user, edit));
        }
        for (id, user, edit) in &rebound {
            self.doc_set_owner(*id, user);
            self.doc_write_policy(*id, edit);
        }
        if !rebound.is_empty() {
            self.queue_save();
        }
        rebound.into_iter().map(|(id, _, _)| id).collect()
    }

    /// The durable room identity of `workspace_id`. What the launch path
    /// needs to turn a local handle into the value the wire speaks.
    pub fn room_of(&self, workspace_id: WorkspaceId) -> Option<RoomId> {
        self.desktop
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .map(|w| w.room)
    }

    /// Point `workspace_id` at `room`, mirroring the change to peers and
    /// disk — how an invitee stops being in their own room and starts
    /// being in the inviter's. Refuses a nil `room`. Also where a
    /// workspace stops governing itself
    /// ([`Workspace::surrender_governance`]), since the joined room
    /// already has an administrator.
    pub fn set_room(&mut self, workspace_id: WorkspaceId, room: RoomId) -> bool {
        let changed = self.set_room_in_memory(workspace_id, room);
        if changed {
            self.queue_save();
        }
        changed
    }

    /// Existing join semantics for an unpublished owner candidate. No state,
    /// filesystem authority, relay export or publisher side effects occur here.
    pub(crate) fn set_room_in_memory(&mut self, workspace_id: WorkspaceId, room: RoomId) -> bool {
        if room.is_nil() || is_local_person_room(room) {
            return false;
        }
        // Joining a room some other local workspace is already in is not a
        // join: this machine is already in the room and already knows who
        // governs it.
        let joining = !self.desktop.workspaces.iter().any(|w| w.room == room);
        let Some(ws) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
        else {
            return false;
        };
        if ws.room == room {
            return false;
        }
        ws.set_room(room);
        let surrendered = if joining {
            ws.surrender_governance()
        } else {
            false
        };
        if joining {
            // Joining names no administrator; the invite that led here pins
            // it next (`accept_invite_into`). Until then the room refuses
            // every governance change rather than trusting whoever answers.
            self.room_authority
                .entry(room)
                .or_insert(RoomAuthority::Unpinned);
            // See `Self::room_share_floor`: joining is entering someone
            // else's workspace, not merging yours into it.
            self.room_share_floor
                .insert(room, NEXT_ID.load(AtomicOrdering::Relaxed));
        }
        self.doc_set_room(workspace_id, room);
        if surrendered {
            // The owner and admin capability went; the tree document has
            // to say so too, or the next cache rebuild reads the old
            // owner back out of it.
            self.seed_doc_from_cache();
        }
        true
    }

    /// Mirror a workspace's room into the automerge doc, as a string —
    /// what every other consumer of the value speaks
    /// (`TILO_WORKSPACE_ID`, the invite token, `CrdtRoute::cluster_id`).
    /// Null-tolerant like the other `doc_*` mirrors.
    pub fn doc_set_room(&mut self, workspace_id: WorkspaceId, room: RoomId) {
        if tilo_desktop_model::desktop_doc::set_room(&mut self.doc, workspace_id, room) {
            self.commit_room_change();
        }
    }

    /// Whether this machine may write `workspace_id`'s governance. Refuses
    /// outright in a room whose governance is written elsewhere, whatever
    /// the local cache says: a grant nobody else can see is a local
    /// escalation, honoured here until the room's authority next syncs it
    /// away.
    fn may_write_governance(&self, workspace_id: WorkspaceId, actor: &UserId) -> bool {
        let Some(room) = self.room_of(workspace_id) else {
            return false;
        };
        if !room.is_nil() && !self.governs_room(room) {
            tracing::warn!(
                target: "shell::state",
                room = %room,
                actor = %actor,
                "governance refused: this machine does not govern the room",
            );
            return false;
        }
        true
    }

    /// Apply `changes` to `workspace_id`'s policy as `actor`, all or none,
    /// mirroring them to peers and disk — the single production entry point
    /// for handing out or taking back authority.
    ///
    /// # Errors
    /// This machine does not govern the room, the workspace is gone, or the
    /// first change the policy refuses.
    pub fn govern_in_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        actor: &UserId,
        changes: Vec<tilo_action_policy::Change>,
    ) -> Result<PolicyEdit, tilo_action_policy::Refusal> {
        if !self.may_write_governance(workspace_id, actor) {
            return Err(tilo_action_policy::Refusal::NotPermitted(
                "this machine does not govern the room".into(),
            ));
        }
        let edit = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| tilo_action_policy::Refusal::NotPermitted("no such workspace".into()))?
            .govern_all_at(actor, changes, tilo_clock::unix_now())?;
        self.doc_write_policy(workspace_id, &edit);
        self.queue_save();
        Ok(edit)
    }

    /// Make `person` a member of `workspace_id` holding `role`, as `actor`:
    /// admitted when they are not a member, their role changed when they
    /// are. What changed, or `None` when `actor` may not.
    pub fn admit_in_workspace(
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
        let change = if member {
            tilo_action_policy::Change::SetRole {
                person: person.to_owned(),
                role,
            }
        } else {
            tilo_action_policy::Change::AddMember {
                person: person.to_owned(),
                role,
            }
        };
        self.govern_in_workspace(workspace_id, actor, vec![change])
            .ok()
    }

    /// End `person`'s membership of `workspace_id`, as `actor`. Whether they
    /// were removed.
    pub fn remove_from_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        actor: &UserId,
        person: &str,
    ) -> bool {
        self.govern_in_workspace(
            workspace_id,
            actor,
            vec![tilo_action_policy::Change::RemoveMember {
                person: person.to_owned(),
            }],
        )
        .is_ok_and(|edit| !edit.is_empty())
    }

    /// Grant `grant` in `workspace_id` on behalf of `grantor`. The id stored,
    /// or `None` when the grantor lacked the authority to give it.
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

    /// Revoke the grant `id` in `workspace_id` as `revoker`, mirroring the
    /// removal to peers and disk — a revoke that only touched the local cache
    /// left every peer still holding the grant and let a restart bring it
    /// back.
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

    /// Grant the sandbox capability `cap` in `workspace_id` on behalf of
    /// `grantor`, mirroring it to peers and disk. The id stored, or `None`
    /// when the grantor may not grant in the room.
    pub fn grant_sandbox_in_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        grantor: &UserId,
        cap: CapOverride,
    ) -> Option<String> {
        if !self.may_write_governance(workspace_id, grantor) {
            return None;
        }
        let edit = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)?
            .grant_sandbox_as_at(grantor, cap, tilo_clock::unix_now())
            .ok()?;
        let id = edit.sandbox_granted.first().cloned();
        self.doc_write_policy(workspace_id, &edit);
        self.queue_save();
        id
    }

    /// Revoke the sandbox capability `id` in `workspace_id` as `revoker`.
    /// Stays open in a room governed elsewhere: narrowing your own reach has
    /// to work locally whoever governs the room.
    pub fn revoke_sandbox_in_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        revoker: &UserId,
        id: &str,
    ) -> bool {
        let Some(edit) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .and_then(|ws| {
                ws.revoke_sandbox_as_at(revoker, id, tilo_clock::unix_now())
                    .ok()
            })
            .filter(|edit| !edit.is_empty())
        else {
            return false;
        };
        self.doc_write_policy(workspace_id, &edit);
        self.queue_save();
        true
    }

    /// Record `person` making `app` an author of `workspace_id`'s space —
    /// what opening an app yourself means — unless it already is one. The
    /// grant as stored, or `None` when nothing was recorded: already an
    /// author, or `person` lacks the authority, in which case the app opens
    /// a viewer.
    pub fn grant_app_authorship(
        &mut self,
        workspace_id: WorkspaceId,
        person: &UserId,
        app: &str,
    ) -> Option<tilo_action_policy::Grant> {
        let now = tilo_clock::unix_now();
        let workspace = self
            .desktop
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)?;
        if workspace.app_is_author_at(app, now) || !self.may_write_governance(workspace_id, person)
        {
            return None;
        }
        let grant = tilo_action_policy::roles::app_authorship(
            &workspace.policy.room,
            app,
            tilo_action_policy::Subject::Person(person.clone()),
        );
        let stored = self
            .desktop
            .grant_in_workspace_at(workspace_id, person, grant, now)
            .ok()?;
        self.doc_write_policy(
            workspace_id,
            &PolicyEdit {
                granted: vec![stored.id.clone()],
                ..PolicyEdit::default()
            },
        );
        self.queue_save();
        self.desktop.external_revision = self.desktop.external_revision.wrapping_add(1);
        Some(stored)
    }

    /// The active workspace after the person at this machine opens `app`.
    ///
    /// The launch that follows must use this workspace: its sandbox is what
    /// the new process is told, and a workspace launch with no author
    /// capability opens the app as a viewer.
    pub fn workspace_after_opening_app(&mut self, app: &str) -> Option<Workspace> {
        let (id, room) = {
            let workspace = self.desktop.active_workspace()?;
            (workspace.id, workspace.room)
        };
        let person = local_user_id_for_room(&room);
        let _ = self.grant_app_authorship(id, &person, app);
        self.desktop.active_workspace().cloned()
    }

    /// Persist an exact, owner-signed ownership offer without changing roles.
    ///
    /// # Errors
    ///
    /// Refuses foreign governance, invalid/expired signatures, a mismatched
    /// owner/workspace, a recipient who is no member, replay, or replacement
    /// of another live proposal.
    pub fn propose_workspace_ownership_transfer(
        &mut self,
        workspace_id: WorkspaceId,
        proposal: collab_types::OwnershipTransferProposalV1,
        now_unix: u64,
    ) -> Result<(), OwnershipTransferStateError> {
        let room = self
            .room_of(workspace_id)
            .ok_or(OwnershipTransferStateError::WrongWorkspaceOrOwner)?;
        if !room.is_nil() && !self.governs_room(room) {
            return Err(OwnershipTransferStateError::NotGoverned);
        }
        proposal.verify(now_unix)?;
        if proposal.expires_unix > now_unix.saturating_add(MAX_OWNERSHIP_OFFER_SECONDS) {
            return Err(OwnershipTransferStateError::ProposalUnavailable);
        }
        let expected_owner = public_key_user_id(proposal.current_owner);
        let recipient = public_key_user_id(proposal.recipient);
        let ws = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
            .ok_or(OwnershipTransferStateError::WrongWorkspaceOrOwner)?;
        if proposal.workspace != ws.room.to_string() || !ws.is_owner(&expected_owner) {
            return Err(OwnershipTransferStateError::WrongWorkspaceOrOwner);
        }
        if ws.role_of(&recipient).is_none() {
            return Err(OwnershipTransferStateError::RecipientNotMember);
        }
        if ws.ownership_transfer_audit.len() >= MAX_OWNERSHIP_TRANSFER_HISTORY
            || ws.ownership_transfer_cancellations.len() >= MAX_OWNERSHIP_TRANSFER_HISTORY
        {
            return Err(OwnershipTransferStateError::HistoryFull);
        }
        if ws
            .ownership_transfer_audit
            .iter()
            .any(|audit| audit.proposal.proposal_id == proposal.proposal_id)
            || ws
                .ownership_transfer_cancellations
                .iter()
                .any(|cancelled| cancelled.proposal_digest == proposal.digest().unwrap_or([0; 32]))
        {
            return Err(OwnershipTransferStateError::ProposalUnavailable);
        }
        if ws
            .pending_ownership_transfer
            .as_ref()
            .is_some_and(|pending| pending.expires_unix > now_unix)
        {
            return Err(OwnershipTransferStateError::ProposalAlreadyPending);
        }
        let previous = ws.pending_ownership_transfer.replace(proposal);
        if let Some(path) = &self.persist_path
            && let Err(error) = crate::shell::persist::save_tree_to(self, path)
        {
            let ws = self
                .desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.id == workspace_id)
                .ok_or(OwnershipTransferStateError::WrongWorkspaceOrOwner)?;
            ws.pending_ownership_transfer = previous;
            return Err(OwnershipTransferStateError::Persistence(error.to_string()));
        }
        Ok(())
    }

    /// Cancel the currently pending proposal using the current owner's signed
    /// cancellation record.
    ///
    /// # Errors
    ///
    /// Refuses an absent/stale proposal, a different signer or proposal
    /// digest, or a workspace no longer governed by this host.
    pub fn cancel_workspace_ownership_transfer(
        &mut self,
        workspace_id: WorkspaceId,
        cancellation: &collab_types::OwnershipTransferCancellationV1,
    ) -> Result<(), OwnershipTransferStateError> {
        let room = self
            .room_of(workspace_id)
            .ok_or(OwnershipTransferStateError::ProposalUnavailable)?;
        if !room.is_nil() && !self.governs_room(room) {
            return Err(OwnershipTransferStateError::NotGoverned);
        }
        let ws = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
            .ok_or(OwnershipTransferStateError::ProposalUnavailable)?;
        let proposal = ws
            .pending_ownership_transfer
            .as_ref()
            .ok_or(OwnershipTransferStateError::ProposalUnavailable)?;
        if ws.ownership_transfer_cancellations.len() >= MAX_OWNERSHIP_TRANSFER_HISTORY {
            return Err(OwnershipTransferStateError::HistoryFull);
        }
        if !ws.is_owner(&public_key_user_id(proposal.current_owner)) {
            return Err(OwnershipTransferStateError::WrongWorkspaceOrOwner);
        }
        cancellation.verify(proposal)?;
        let previous = proposal.clone();
        ws.ownership_transfer_cancellations
            .push(cancellation.clone());
        ws.pending_ownership_transfer = None;
        if let Some(path) = &self.persist_path
            && let Err(error) = crate::shell::persist::save_tree_to(self, path)
        {
            let ws = self
                .desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.id == workspace_id)
                .ok_or(OwnershipTransferStateError::ProposalUnavailable)?;
            ws.ownership_transfer_cancellations.pop();
            ws.pending_ownership_transfer = Some(previous);
            return Err(OwnershipTransferStateError::Persistence(error.to_string()));
        }
        Ok(())
    }

    /// Verify both parties and atomically replace the structural owner while
    /// appending both parties' signed audit evidence in the same persisted snapshot.
    ///
    /// # Errors
    ///
    /// Refuses replay, expiry, recipient substitution, role changes, stale
    /// ownership, invalid signatures, foreign governance, or persistence
    /// failure. On persistence failure the in-memory owner and audit roll back.
    pub fn accept_workspace_ownership_transfer(
        &mut self,
        workspace_id: WorkspaceId,
        audit: collab_types::OwnershipTransferAuditV1,
        now_unix: u64,
    ) -> Result<(), OwnershipTransferStateError> {
        let room = self
            .room_of(workspace_id)
            .ok_or(OwnershipTransferStateError::ProposalUnavailable)?;
        if !room.is_nil() && !self.governs_room(room) {
            return Err(OwnershipTransferStateError::NotGoverned);
        }
        audit.verify()?;
        if audit.committed_unix > now_unix {
            return Err(OwnershipTransferStateError::ProposalUnavailable);
        }
        let index = self
            .desktop
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
            .ok_or(OwnershipTransferStateError::ProposalUnavailable)?;
        let previous = self
            .desktop
            .workspaces
            .get(index)
            .cloned()
            .ok_or(OwnershipTransferStateError::ProposalUnavailable)?;
        let ws = self
            .desktop
            .workspaces
            .get_mut(index)
            .ok_or(OwnershipTransferStateError::ProposalUnavailable)?;
        let pending = ws
            .pending_ownership_transfer
            .as_ref()
            .ok_or(OwnershipTransferStateError::ProposalUnavailable)?;
        if pending != &audit.proposal || audit.proposal.workspace != ws.room.to_string() {
            return Err(OwnershipTransferStateError::ProposalUnavailable);
        }
        audit.proposal.verify(now_unix)?;
        if audit.proposal.expires_unix > now_unix.saturating_add(MAX_OWNERSHIP_OFFER_SECONDS) {
            return Err(OwnershipTransferStateError::ProposalUnavailable);
        }
        let current_owner = public_key_user_id(audit.proposal.current_owner);
        let recipient = public_key_user_id(audit.proposal.recipient);
        if !ws.is_owner(&current_owner) {
            return Err(OwnershipTransferStateError::WrongWorkspaceOrOwner);
        }
        if ws.role_of(&recipient).is_none() {
            return Err(OwnershipTransferStateError::RecipientNotMember);
        }
        if ws
            .ownership_transfer_audit
            .iter()
            .any(|prior| prior.proposal.proposal_id == audit.proposal.proposal_id)
        {
            return Err(OwnershipTransferStateError::ProposalUnavailable);
        }
        if ws.ownership_transfer_audit.len() >= MAX_OWNERSHIP_TRANSFER_HISTORY {
            return Err(OwnershipTransferStateError::HistoryFull);
        }
        if !ws.transfer_owner_as_at(&current_owner, &recipient, now_unix) {
            return Err(OwnershipTransferStateError::WrongWorkspaceOrOwner);
        }
        ws.pending_ownership_transfer = None;
        ws.ownership_transfer_audit.push(audit);
        let edit = PolicyEdit::between(&previous.policy, &ws.policy);
        self.doc_set_owner(workspace_id, &recipient);
        self.doc_write_policy(workspace_id, &edit);
        if let Some(path) = &self.persist_path
            && let Err(error) = crate::shell::persist::save_tree_to(self, path)
        {
            let workspace = self
                .desktop
                .workspaces
                .get_mut(index)
                .ok_or(OwnershipTransferStateError::ProposalUnavailable)?;
            *workspace = previous;
            self.seed_doc_from_cache();
            return Err(OwnershipTransferStateError::Persistence(error.to_string()));
        }
        self.export_fs_capabilities();
        Ok(())
    }

    /// Mirror a governance change of `workspace_id` into the automerge doc
    /// so peer-sync ships it and a restart reloads it. Grants, members and
    /// sandbox capabilities live as **maps keyed by id**, not lists:
    /// deleting a map key merges concurrently, while deleting list index 3
    /// on two peers removes two different grants. Without the mirror a
    /// revocation would change only the local cache, and a peer would keep
    /// using what the owner took away.
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
        edit.write(&mut self.doc, workspace);
        self.commit_room_change();
    }

    /// Replicate a named grant set as workspace policy. The grants an
    /// assignment expands to replicate through [`Self::doc_write_policy`];
    /// this map lets another governor inspect and reuse the definition.
    pub fn doc_upsert_preset(&mut self, workspace_id: WorkspaceId, preset: &Preset) {
        if tilo_desktop_model::desktop_doc::upsert_preset(&mut self.doc, workspace_id, preset) {
            self.commit_room_change();
        }
    }

    /// Create a task in the workspace and mirror it immediately into both
    /// durable state and the room document. Creation is data entry, not model
    /// inference: `route` explicitly records whether later execution may use
    /// a model.
    pub fn create_workspace_task(
        &mut self,
        workspace_id: WorkspaceId,
        title: impl Into<String>,
        actor: impl Into<String>,
        assigned_agent: Option<String>,
        route: WorkspaceTaskRoute,
        resource: Option<WorkspaceTaskResource>,
    ) -> Option<String> {
        // Native manual work is committed by the outcome handler after it drops
        // the tree writer. Never mutate a parallel legacy task cache here.
        if route == WorkspaceTaskRoute::Manual
            && self
                .room_of(workspace_id)
                .is_some_and(|room| self.requires_native_owner(room))
        {
            return None;
        }
        let title = title.into().trim().to_string();
        if title.is_empty() {
            return None;
        }
        let actor = actor.into();
        let now = tilo_clock::unix_now_i64();
        let id = format!("task:{}", uuid::Uuid::new_v4());
        let task = WorkspaceTask {
            id: id.clone(),
            title: title.clone(),
            created_by: actor.clone(),
            assigned_agent,
            route,
            status: WorkspaceTaskStatus::Queued,
            resource,
            subject: None,
            created_at: now,
            updated_at: now,
            audit: vec![WorkspaceTaskAuditEvent::new(
                now,
                actor,
                "create-task",
                [("title", tilo_actions::Scalar::text(&title))],
            )],
        };
        self.desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)?
            .tasks
            .push(task.clone());
        self.doc_upsert_workspace_task(workspace_id, &task);
        self.queue_save();
        Some(id)
    }

    pub fn set_workspace_task_status(
        &mut self,
        workspace_id: WorkspaceId,
        task_id: &str,
        status: WorkspaceTaskStatus,
        actor: &str,
    ) -> bool {
        if self
            .room_of(workspace_id)
            .is_some_and(|room| self.requires_native_owner(room))
            && self
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.id == workspace_id)
                .and_then(|workspace| workspace.tasks.iter().find(|task| task.id == task_id))
                .is_some_and(|task| task.route == WorkspaceTaskRoute::Manual)
        {
            // Status needs a future typed owner command; a local cache write
            // cannot complete native coordination work.
            return false;
        }
        let now = tilo_clock::unix_now_i64();
        let Some(task) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
            .and_then(|workspace| workspace.set_task_status_at(task_id, status, actor, now))
            .cloned()
        else {
            return false;
        };
        self.doc_upsert_workspace_task(workspace_id, &task);
        let lapsed = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
            .map(Workspace::lapse_task_grants)
            .unwrap_or_default();
        self.doc_write_policy(
            workspace_id,
            &PolicyEdit {
                revoked: lapsed,
                ..PolicyEdit::default()
            },
        );
        self.queue_save();
        true
    }

    /// Append a broker-authenticated task event without allowing a caller to
    /// alter task state. Agent progress is durable shared audit data, not a
    /// substitute for an arbitrary document write.
    pub fn append_workspace_task_audit(
        &mut self,
        workspace_id: WorkspaceId,
        task_id: &str,
        actor: &str,
        summary: impl Into<String>,
    ) -> bool {
        let now = tilo_clock::unix_now_i64();
        let Some(task) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
            .and_then(|workspace| workspace.append_task_audit_at(task_id, actor, summary, now))
            .cloned()
        else {
            return false;
        };
        self.doc_upsert_workspace_task(workspace_id, &task);
        self.queue_save();
        true
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

    /// Save `shortcut` in `workspace_id`, replacing the shortcut with the
    /// same id. `false` when the workspace is not held here.
    pub fn save_workspace_shortcut(
        &mut self,
        workspace_id: WorkspaceId,
        shortcut: &tilo_automation::Shortcut,
    ) -> bool {
        let Some(workspace) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
        else {
            return false;
        };
        match workspace
            .shortcuts
            .iter_mut()
            .find(|saved| saved.id == shortcut.id)
        {
            Some(saved) => *saved = shortcut.clone(),
            None => workspace.shortcuts.push(shortcut.clone()),
        }
        workspace
            .shortcuts
            .sort_by(|left, right| left.id.cmp(&right.id));
        if tilo_desktop_model::desktop_doc::upsert_shortcut(&mut self.doc, workspace_id, shortcut) {
            self.commit_room_change();
        }
        self.queue_save();
        true
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
            .find(|workspace| workspace.room == room)
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

    /// Delete shortcut `shortcut_id` from `workspace_id`. `false` when there
    /// was no such shortcut.
    pub fn remove_workspace_shortcut(
        &mut self,
        workspace_id: WorkspaceId,
        shortcut_id: &str,
    ) -> bool {
        let Some(workspace) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
        else {
            return false;
        };
        let before = workspace.shortcuts.len();
        workspace
            .shortcuts
            .retain(|shortcut| shortcut.id != shortcut_id);
        if workspace.shortcuts.len() == before {
            return false;
        }
        if tilo_desktop_model::desktop_doc::remove_shortcut(
            &mut self.doc,
            workspace_id,
            shortcut_id,
        ) {
            self.commit_room_change();
        }
        self.queue_save();
        true
    }

    fn doc_upsert_workspace_task(&mut self, workspace_id: WorkspaceId, task: &WorkspaceTask) {
        if tilo_desktop_model::desktop_doc::upsert_task(&mut self.doc, workspace_id, task) {
            self.commit_room_change();
        }
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

    /// Mirror the room's owner into the doc.
    pub fn doc_set_owner(&mut self, workspace_id: WorkspaceId, owner: &str) {
        if tilo_desktop_model::desktop_doc::set_owner(&mut self.doc, workspace_id, owner) {
            self.commit_room_change();
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
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let home = Workspace {
            id: ws_id,
            name: "Home".into(),
            strips: vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups: Vec::new(),
            }],
            ..Workspace::new(ws_id, String::new())
        };

        let mut active_strip = HashMap::new();
        active_strip.insert(ws_id, strip_id);
        let mut tree = Self {
            desktop: DesktopTree {
                workspaces: vec![home],
                active: ws_id,
                active_strip,
                active_group: HashMap::new(),
                ..DesktopTree::default()
            },
            ..Default::default()
        };
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
        let home = Workspace {
            id: ws_id,
            name: "Home".into(),
            strips: vec![Strip {
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
                        },
                        Tab {
                            id: tab_files,
                            title: "Files".into(),
                            tile: TileContent::AppPlaceholder {
                                kind: AppKind::FileExplorer,
                                label: "Files".into(),
                            },
                        },
                        Tab {
                            id: tab_terminal,
                            title: "Terminal".into(),
                            tile: TileContent::AppPlaceholder {
                                kind: AppKind::Terminal,
                                label: "Terminal".into(),
                            },
                        },
                        Tab {
                            id: tab_word,
                            title: "Document".into(),
                            tile: TileContent::AppPlaceholder {
                                kind: AppKind::Other("Word".into()),
                                label: "Document".into(),
                            },
                        },
                    ],
                    active_tab: tab_notes,
                    width: WindowWidth::Wide,
                    maximized: false,
                    focus_history: Vec::new(),
                }],
            }],
            // One invented peer so the top-bar avatar stack has something
            // to paint. "me" is deliberately not seeded so
            // `draw_ws_settings` synthesizes its "You" tile from the
            // fallback path. Hex matches `tilo-concept/src/data.ts`.
            collaborators: vec![
                Collaborator::new("alice", "Alice")
                    .with_cursor_color("#ec4899")
                    .with_role("editor"),
            ],
            ..Workspace::new(ws_id, String::new())
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
        let research_id = tree.append_empty_workspace("Research");
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
        tree.append_empty_workspace("Design");

        // Home stays active — `append_empty_workspace` doesn't touch
        // `tree.active`. This seeds the doc with the initial Home
        // workspace + active pointer the cache literal set directly.
        tree.seed_doc_from_cache();
        tree
    }

    pub fn append_empty_workspace(&mut self, name: &str) -> WorkspaceId {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        // A workspace the user just created is a room that now exists;
        // it gets its identity at the same moment it gets its id, not
        // the first time someone tries to share it.
        let room = RoomId::new();
        let mut ws = Workspace {
            id: ws_id,
            name: name.into(),
            strips: vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups: vec![],
            }],
            ..Workspace::new(ws_id, String::new())
        };
        ws.set_room(room);
        self.desktop.active_strip.insert(ws_id, strip_id);
        self.desktop.workspaces.push(ws);

        // Mirror the append into the doc so peer-sync sees a discrete change.
        // Best-effort, same as `seed_doc_from_cache`: the cache push above
        // already happened, so a doc-write failure only means this append
        // doesn't replicate.
        tilo_desktop_model::desktop_doc::append_workspace(&mut self.doc, ws_id, name, room);
        // A newly-created workspace is local session state as well as a
        // collaboration-document change. Persist it at the mutation boundary:
        // otherwise a compositor crash before the next unrelated edit silently
        // loses the room the person just created.
        self.queue_save();
        ws_id
    }

    /// Add a dormant local entry for a room discovered through the private
    /// same-identity continuation lane. Unlike [`Self::append_empty_workspace`]
    /// this creates no strip or window and never changes focus, so merely
    /// learning that another device has a workspace open cannot publish an
    /// empty local layout into that room or pull the current session away.
    /// The room's authenticated sync response supplies its real name/layout.
    ///
    /// The offer travels only between devices certified for the person at
    /// this machine, so the room's governance is pinned to that person
    /// ([`RoomAuthority::Person`]): their devices relay it, whoever owns the
    /// room. `_source_device` is a continuity label, not a key, and pins
    /// nothing. With no certified person here the room stays
    /// [`RoomAuthority::Unpinned`] and refuses governance; a later offer of
    /// a room already held pins such a room (the deterministic migration of
    /// a continuation room a state file recorded as `Unpinned`).
    pub fn adopt_continuation_room(&mut self, room: RoomId, _source_device: &str) -> bool {
        if room.is_nil() || is_local_person_room(room) {
            return false;
        }
        if self
            .desktop
            .workspaces
            .iter()
            .any(|workspace| workspace.room == room)
        {
            return self.pin_continuation_room(room);
        }
        let id = WorkspaceId::new();
        let mut workspace = Workspace::new(id, "From another device");
        workspace.set_room(room);
        self.desktop.workspaces.push(workspace);
        self.continuation_rooms.insert(room);
        self.room_authority.insert(
            room,
            self.local_person
                .map_or(RoomAuthority::Unpinned, RoomAuthority::Person),
        );
        self.room_share_floor
            .insert(room, NEXT_ID.load(AtomicOrdering::Relaxed));
        self.seed_doc_from_cache();
        self.queue_save();
        true
    }

    /// Pin an unpinned continuation room to the person at this machine; see
    /// [`Self::adopt_continuation_room`]. Returns whether it was pinned.
    fn pin_continuation_room(&mut self, room: RoomId) -> bool {
        let Some(person) = self.local_person else {
            return false;
        };
        if !self.continuation_rooms.contains(&room)
            || self.room_authority(room) != RoomAuthority::Unpinned
        {
            return false;
        }
        self.room_authority
            .insert(room, RoomAuthority::Person(person));
        self.queue_save();
        true
    }

    /// Join a room copied from the machine that enrolled this device.
    ///
    /// This is membership, not a continuation offer, so turning app
    /// continuations off does not remove it.
    pub fn join_enrolled_room(&mut self, room: RoomId) -> bool {
        if is_local_person_room(room)
            || self
                .desktop
                .workspaces
                .iter()
                .any(|workspace| workspace.room == room)
        {
            return false;
        }
        // Enrollment hands over a room id, not who governs it: the room stays
        // unpinned — refusing governance — until the person's signed
        // workspace directory lists it with its governor.
        self.hold_personal_room(
            room,
            "From another device",
            &local_user_id(),
            None,
            RoomListing::Enrolled,
        )
    }

    /// Hold `room` as one of `person`'s workspaces, named `name`: the
    /// person's workspace list says they are in it. `governor` is the peer
    /// that governs a room someone else owns, the same one the device that
    /// accepted the invitation pinned; `None` means the person owns the
    /// room, so its governance is pinned to the person
    /// ([`RoomAuthority::Person`]) and any of their devices may supply it.
    /// A room this tree lacks is joined without focus or layout; a
    /// room only offered by another device's continuation becomes a
    /// workspace of its own; one already held takes the listed name, and a
    /// listed governor replaces an unpinned authority or a person pin on a
    /// room the person does not own (a continuation offer's provisional
    /// pin). Returns whether the tree changed.
    pub fn adopt_personal_room(
        &mut self,
        room: RoomId,
        name: &str,
        person: &UserId,
        governor: Option<&PeerOrigin>,
    ) -> bool {
        self.hold_personal_room(room, name, person, governor, RoomListing::Directory)
    }

    /// [`Self::adopt_personal_room`] and [`Self::join_enrolled_room`]:
    /// `listing` says whether the person's signed directory vouches for who
    /// governs the room.
    fn hold_personal_room(
        &mut self,
        room: RoomId,
        name: &str,
        person: &UserId,
        governor: Option<&PeerOrigin>,
        listing: RoomListing,
    ) -> bool {
        // The authority the listing names: the listed governor, or the person
        // who owns the room. Enrollment names neither.
        let listed = match (listing, governor) {
            (RoomListing::Enrolled, _) => None,
            (RoomListing::Directory, Some(governor)) => Some(RoomAuthority::Peer(governor.clone())),
            (RoomListing::Directory, None) => person_root(person).map(RoomAuthority::Person),
        };
        if room.is_nil() || is_local_person_room(room) {
            return false;
        }
        if let Some(index) = self
            .desktop
            .workspaces
            .iter()
            .position(|workspace| workspace.room == room)
        {
            let offered = self.continuation_rooms.remove(&room);
            let Some(workspace) = self.desktop.workspaces.get_mut(index) else {
                return false;
            };
            let id = workspace.id;
            let renamed = workspace.name != name;
            if renamed {
                workspace.name = name.to_string();
            }
            let owned = governor.is_none() && workspace.owner().is_none();
            if owned {
                workspace.set_owner(person.clone());
            }
            let person_owns = workspace.owner().map(String::as_str) == Some(person.as_str());
            // A person pin on a room the person does not own is provisional
            // (a continuation offer's): the directory says who actually
            // governs the room.
            let current = self.room_authority(room);
            let replaceable = current == RoomAuthority::Unpinned
                || (matches!(current, RoomAuthority::Person(_)) && !person_owns);
            let pinned = match listed {
                // The person governs only a room it owns.
                Some(RoomAuthority::Person(_)) if !person_owns => false,
                Some(listed) if replaceable && listed != current => {
                    self.room_authority.insert(room, listed);
                    true
                }
                _ => false,
            };
            if !(offered || renamed || owned || pinned) {
                return false;
            }
            self.seed_doc_from_cache();
            if owned {
                self.doc_set_owner(id, person);
            }
            self.desktop.room_epoch = self.desktop.room_epoch.wrapping_add(1);
            self.queue_save();
            return true;
        }
        let id = WorkspaceId::new();
        let mut workspace = Workspace::new(id, name);
        workspace.set_room(room);
        // The same person sits at this machine. Without their membership the
        // room exists but every launch from either of their devices is denied.
        // A room someone else owns brings its owner and membership with its
        // governance.
        if governor.is_none() {
            workspace.set_owner(person.clone());
        }
        let first = self.desktop.workspaces.is_empty();
        self.desktop.workspaces.push(workspace);
        if first {
            self.desktop.active = id;
        }
        self.room_authority
            .insert(room, listed.unwrap_or(RoomAuthority::Unpinned));
        self.room_share_floor
            .insert(room, NEXT_ID.load(AtomicOrdering::Relaxed));
        self.seed_doc_from_cache();
        if governor.is_none() {
            self.doc_set_owner(id, person);
        }
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
        let index = self
            .desktop
            .workspaces
            .iter()
            .position(|workspace| workspace.room == room)?;
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
        self.continuation_rooms.remove(&room);
        self.seed_doc_from_cache();
        self.desktop.room_epoch = self.desktop.room_epoch.wrapping_add(1);
        self.queue_save();
        Some((index, removed))
    }

    /// Delete workspace `id` from this person's desktop, on this device and,
    /// through the person's workspace list, on their other devices. The last
    /// workspace stays. Returns its place and value, for an undo.
    pub fn delete_workspace(&mut self, id: WorkspaceId) -> Option<(usize, Workspace)> {
        if self.desktop.workspaces.len() < 2 {
            return None;
        }
        let room = self
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)?
            .room;
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
            .any(|existing| existing.room == workspace.room || existing.id == workspace.id)
        {
            return false;
        }
        let index = index.min(self.desktop.workspaces.len());
        let id = workspace.id;
        self.desktop.workspaces.insert(index, workspace);
        self.desktop.active = id;
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
            .filter(|(_, workspace)| rooms.contains(&workspace.room))
            .map(|(index, _)| index)
            .collect();
        let mut wanted: Vec<Workspace> = Vec::with_capacity(slots.len());
        for room in rooms {
            if let Some(workspace) = self.desktop.workspaces.iter().find(|w| w.room == *room) {
                wanted.push(workspace.clone());
            }
        }
        if wanted.len() != slots.len() {
            return false;
        }
        let mut moved = false;
        for (slot, workspace) in slots.into_iter().zip(wanted) {
            if let Some(existing) = self.desktop.workspaces.get_mut(slot)
                && existing.room != workspace.room
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
                .then_some(workspace.room)
        }) {
            self.continuation_rooms.remove(&room);
            self.continuation_closed_tabs.remove(&tab);
            self.continuation_owned_tabs.insert(tab, room);
        }
        true
    }

    /// Remove rooms which exist here solely because of a continuation offer
    /// and are no longer offered by any device.
    pub fn reconcile_continuation_rooms(&mut self, desired: &BTreeSet<RoomId>) -> bool {
        let stale: BTreeSet<_> = self
            .continuation_rooms
            .difference(desired)
            .copied()
            .collect();
        if stale.is_empty() {
            return false;
        }
        let stale_tabs: BTreeSet<_> = self
            .desktop
            .workspaces
            .iter()
            .filter(|workspace| stale.contains(&workspace.room))
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| tab.id)
            .filter(|tab| self.continuation_tabs.contains(tab))
            .collect();
        self.desktop
            .workspaces
            .retain(|workspace| !stale.contains(&workspace.room));
        self.continuation_rooms.retain(|room| !stale.contains(room));
        self.continuation_tabs
            .retain(|tab| !stale_tabs.contains(tab));
        self.continuation_file_references
            .retain(|tab, _| !stale_tabs.contains(tab));
        if let Ok(mut registry) =
            crate::shell::personal_continuity_registry::global_registry().lock()
        {
            for tab in stale_tabs {
                registry.remove_remote_tab(crate::shell::personal_continuity::AppInstanceId(tab));
            }
        }
        self.room_authority.retain(|room, _| !stale.contains(room));
        self.room_share_floor
            .retain(|room, _| !stale.contains(room));
        if !self
            .desktop
            .workspaces
            .iter()
            .any(|workspace| workspace.id == self.desktop.active)
        {
            self.desktop.active = self
                .desktop
                .workspaces
                .first()
                .map_or_else(WorkspaceId::default, |workspace| workspace.id);
        }
        self.seed_doc_from_cache();
        self.queue_save();
        true
    }

    /// Materialize another device's continuation as synchronized placeholder
    /// tabs. This never activates a workspace, strip, or group; the compositor
    /// opens the matching local apps through its normal authorized launch path.
    #[allow(
        clippy::too_many_lines,
        clippy::indexing_slicing,
        reason = "continuation materialization grows strips and groups until the validated placement indices exist before indexing them"
    )]
    pub fn materialize_continuation_workspace(
        &mut self,
        _source_device: &str,
        continuation: &crate::shell::personal_continuity::WorkspaceContinuation,
    ) -> bool {
        let existing_continuation_tabs = self.continuation_tabs.clone();
        let (changed, file_references, file_reference_removals, inserted_continuation_tabs) = {
            let mut file_references = Vec::new();
            let mut file_reference_removals = Vec::new();
            let mut inserted_continuation_tabs = Vec::new();
            let Some(workspace) = self
                .desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.room == continuation.room)
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
                            crate::shell::launcher::installed_apps_without_waiting()
                                .into_iter()
                                .find(|installed| installed.package_id == *app_id)
                                .map_or_else(|| app_id.clone(), |installed| installed.display_name),
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
                            crate::shell::doc_open::continuity_file_reference_label(&app.launch),
                            TileContent::Stub {
                                kind: AppKind::FileExplorer,
                                label: "Available on another device".to_string(),
                            },
                        )
                    }
                };
                let exists = workspace.strips.iter().any(|strip| {
                    strip
                        .window_groups
                        .iter()
                        .flat_map(|group| &group.tabs)
                        .any(|tab| tab.id == app.id.0)
                });
                if exists && !existing_continuation_tabs.contains(&app.id.0) {
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
                        .push(Strip::new(StripId::new(), "Available elsewhere"));
                }
                let strip = &mut workspace.strips[app.placement.strip as usize];
                while strip.window_groups.len() <= app.placement.group as usize {
                    strip.window_groups.push(WindowGroup {
                        id: next_id(),
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
        let moved =
            self.desktop
                .split_tab_into_new_group(strip_id, tab_id, insert_at, &mut ProcessIds)?;
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
        topology: &[ui::widget::GroupTopology],
    ) -> bool {
        let shape: Vec<tilo_desktop_model::GroupShape> = topology
            .iter()
            .map(|group| tilo_desktop_model::GroupShape {
                id: group.id,
                tab_ids: group.tab_ids.clone(),
                active_tab: group.active_tab,
                width: group.width,
            })
            .collect();
        let Some(persisted) = self.desktop.reconcile_strip_topology(strip_id, &shape) else {
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

    /// Create or rename a strip of the active workspace, as a person at this
    /// machine does it: the model's own
    /// [`DesktopCommand::CreateStrip`](tilo_desktop_model::commands::DesktopCommand::CreateStrip)
    /// and `RenameStrip` reductions on the cache, then the room document and
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
        use tilo_desktop_model::commands::{DesktopCommand, apply_command};
        let renamed = match command {
            DesktopCommand::CreateStrip { .. } => None,
            DesktopCommand::RenameStrip { strip, .. } => {
                let strip = StripId(*strip);
                if !self.may_rename_strip(strip) {
                    return Err("target belongs to another topology owner".into());
                }
                Some(strip)
            }
            _ => return Err("not a strip command".into()),
        };
        let ws_id = self.desktop.active;
        let workspace = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == ws_id)
            .ok_or("there is no active workspace")?;
        let room = workspace.room;
        let before = workspace.strips.len();
        apply_command(workspace, command, &mut || Ok(next_id()), &mut |_, _| {
            Err("a strip command creates no record".into())
        })?;
        let strip = if let Some(strip) = renamed {
            strip
        } else {
            let made = workspace
                .strips
                .get(before)
                .map(|strip| strip.id)
                .ok_or("the strip was not created")?;
            self.desktop.active_strip.insert(ws_id, made);
            made
        };
        self.sync_room_doc(room);
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
    /// lifecycle ([`crate::shell::window_actions::close_tab`]).
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
            .map(|workspace| workspace.room.to_string())
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

fn public_key_user_id(key: collab_types::Ed25519PublicKey) -> UserId {
    hex::encode(key.as_bytes())
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
                    share >= ui::widget::WIDTH_NORMAL_PERMILLE,
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
                let room = tree.desktop.active_workspace().unwrap().room;
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
                let room = tree.desktop.active_workspace().unwrap().room;
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
                tree.append_empty_workspace("Second");
                let rooms: Vec<RoomId> = tree.desktop.workspaces.iter().map(|w| w.room).collect();
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
                let cursors = ui::presence::assign_peer_colors(&ids, &[]);

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
        let id = tree.append_empty_workspace("Project X");
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
        assert_eq!(reconstructed.room, created.room);
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

                tree_a.append_empty_workspace("Project X");
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
                let new_id = tree_a.append_empty_workspace("Project X");
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
                    hydrated.room, room,
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

            // Phase `set_room_propagates_via_apply_changes`.
            // `set_room` is what an accepted invite performs — must reach the
            // doc as well as the cache, or the change never replicates.
            {
                let mut tree_a = WorkspaceTree::bootstrap_demo();
                let id = tree_a.desktop.workspaces[0].id;
                let inviters_room = RoomId::new();
                assert!(tree_a.set_room(id, inviters_room), "set_room must apply");

                let changes = tree_a.export_changes_since(&[]);
                let mut tree_b = WorkspaceTree::default();
                tree_b.apply_changes(&changes).expect("apply changes");

                assert_eq!(
                    tree_b.room_of(id),
                    Some(inviters_room),
                    "a workspace moved into a new room must carry that room to its \
                     peers"
                );
            }

            // Phase `set_room_refuses_the_nil_room`.
            // "Join the unminted room" is never a meaningful request, and
            // honouring it would drop a workspace out of collaboration with no
            // signal at all.
            {
                let mut tree = WorkspaceTree::bootstrap_demo();
                let id = tree.desktop.workspaces[0].id;
                let before = tree.room_of(id).expect("bootstrap mints a room");
                assert!(
                    !tree.set_room(id, RoomId::nil()),
                    "setting the nil room must be refused"
                );
                assert_eq!(
                    tree.room_of(id),
                    Some(before),
                    "and must leave the workspace's real room in place"
                );
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

    /// Two trees in one room, so a record-index claim can be read back
    /// from the side that did not make it.
    fn two_in_a_room() -> (WorkspaceTree, WorkspaceTree, RoomId) {
        let mut host = WorkspaceTree::bootstrap_demo();
        let host_ws = host.desktop.workspaces[0].id;
        let room = host.room_of(host_ws).expect("bootstrap mints a room");
        host.adopt_ownerless_workspaces(&"ada".to_string());

        let mut guest = WorkspaceTree::bootstrap_demo();
        let guest_ws = guest.desktop.workspaces[0].id;
        assert!(guest.set_room(guest_ws, room));
        // The host's invite names it (hub 1, as `ship` delivers it) as the
        // room's governor; a join alone names nobody.
        assert!(guest.pin_room_authority(room, PeerOrigin::hub(1)));
        guest.adopt_ownerless_workspaces(&"bob".to_string());
        (host, guest, room)
    }

    #[test]
    fn a_peer_cannot_save_a_shortcut_on_the_local_persons_behalf() {
        // `saved_by` arms triggers and unlocks advance consents on the
        // device of the person it names, so a room member must not be able
        // to write it for someone else: only that person's own devices may.
        let (mut host, mut guest, room) = two_in_a_room();
        let person = [7u8; 32];
        guest.local_person = Some(person);
        let person_id = person_id_of(person).expect("a person id");
        let forged = tilo_automation::Shortcut {
            id: "nightly".into(),
            name: "Forged".into(),
            inputs: Vec::new(),
            steps: Vec::new(),
            output: None,
            grant: Vec::new(),
            triggers: Vec::new(),
            saved_by: Some(person_id),
        };
        host.desktop
            .workspaces
            .iter_mut()
            .find(|w| w.room == room)
            .expect("the host is in the room")
            .shortcuts
            .push(forged.clone());
        let changes = host
            .export_room_changes_since(room, &[])
            .expect("the host is in the room");
        let guest_shortcuts = |guest: &WorkspaceTree| {
            guest
                .desktop
                .workspaces
                .iter()
                .find(|w| w.room == room)
                .expect("the guest is in the room")
                .shortcuts
                .clone()
        };

        // Phase `an_unrelated_peer_is_refused_outright`.
        assert!(
            !guest
                .apply_room_changes(room, &changes, &PeerOrigin::hub(1))
                .expect("a refusal is not an error"),
            "a batch asserting the local person's authorship must be refused"
        );
        assert!(guest_shortcuts(&guest).is_empty());

        // Phase `the_persons_own_device_may_save_it`.
        guest
            .authenticated_persons
            .insert(PeerOrigin::hub(1), person);
        assert!(
            guest
                .apply_room_changes(room, &changes, &PeerOrigin::hub(1))
                .expect("the person's own device merges")
        );
        assert_eq!(guest_shortcuts(&guest), vec![forged]);
    }

    #[test]
    fn workspaces_owned_by_the_account_name_pass_to_the_device_identity() {
        let key = "61".repeat(32);
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.adopt_ownerless_workspaces(&"ada".to_string());
        let home = tree.desktop.workspaces[0].id;
        let joined = tree.append_empty_workspace("Joined");
        let joined_room = tree.room_of(joined).expect("a room");
        {
            let ws = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|w| w.id == joined)
                .unwrap();
            ws.set_owner("ada".to_string());
        }
        tree.room_authority
            .insert(joined_room, RoomAuthority::Unpinned);

        let rebound = tree.rebind_account_owned_workspaces("ada", |_| key.clone());

        assert_eq!(
            rebound,
            vec![home],
            "only the locally governed workspace moves"
        );
        let ws = tree
            .desktop
            .workspaces
            .iter()
            .find(|w| w.id == home)
            .unwrap();
        assert_eq!(ws.owner().map(String::as_str), Some(key.as_str()));
        assert_eq!(ws.role_of(&key), Some(CapRole::Owner));
        assert_eq!(ws.role_of("ada"), None);
        let other = tree
            .desktop
            .workspaces
            .iter()
            .find(|w| w.id == joined)
            .unwrap();
        assert_eq!(
            other.owner().map(String::as_str),
            Some("ada"),
            "a foreign room keeps its owner"
        );

        let mut replica = WorkspaceTree::default();
        replica
            .apply_changes(&tree.export_changes_since(&[]))
            .expect("apply");
        let mirrored = replica
            .desktop
            .workspaces
            .iter()
            .find(|w| w.id == home)
            .unwrap();
        assert_eq!(
            mirrored.owner().map(String::as_str),
            Some(key.as_str()),
            "the room doc agrees"
        );
        assert_eq!(mirrored.role_of(&key), Some(CapRole::Owner));

        assert!(
            tree.rebind_account_owned_workspaces("ada", |_| "ada".to_string())
                .is_empty(),
            "without an identity the account name is still who sits here"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn remote_windows_arrive_in_shared_strips_without_moving_local_focus() {
        // Phase `a_peers_window_arrives_in_its_shared_strip_without_moving_local_focus`.
        // A peer's strip is shared layout, not an instruction to pour its
        // windows into whichever strip this machine happened to focus. Both
        // strips appear, while the receiving person's active strip remains
        // their own local view state.
        {
            const HOST: u16 = 0x2222;
            const GUEST: u16 = 0x3333;
            let (mut host, mut guest, room) = two_in_a_room();
            host.set_salt_override(HOST);
            guest.set_salt_override(GUEST);

            // Each has one window of their own, in a strip of their own.
            for (tree, salt, title) in [
                (&mut host, HOST, "Host window"),
                (&mut guest, GUEST, "Guest window"),
            ] {
                let ws = tree
                    .desktop
                    .workspaces
                    .iter()
                    .position(|w| w.room == room)
                    .unwrap();
                let tab_id = salted(salt, 3);
                tree.desktop.workspaces[ws].strips = vec![Strip {
                    id: StripId::from(salted(salt, 1)),
                    name: "Main".into(),
                    window_groups: vec![WindowGroup {
                        id: salted(salt, 2),
                        tabs: vec![Tab {
                            id: tab_id,
                            title: title.into(),
                            tile: TileContent::AppPlaceholder {
                                kind: AppKind::Other("Notes".into()),
                                label: title.into(),
                            },
                        }],
                        active_tab: tab_id,
                        width: WindowWidth::Normal,
                        maximized: false,
                        focus_history: Vec::new(),
                    }],
                }];
                tree.sync_room_doc(room);
            }

            let guest_workspace = guest
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room == room)
                .expect("guest workspace")
                .id;
            let guest_strip = guest
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room == room)
                .and_then(|workspace| workspace.strips.first())
                .expect("guest strip")
                .id;
            guest
                .desktop
                .active_strip
                .insert(guest_workspace, guest_strip);

            ship(&mut host, &mut guest, room, 1);

            let ws = guest
                .desktop
                .workspaces
                .iter()
                .find(|w| w.room == room)
                .unwrap();
            let titles: Vec<&str> = ws
                .strips
                .iter()
                .flat_map(|s| s.window_groups.iter())
                .flat_map(|g| g.tabs.iter())
                .map(|t| t.title.as_str())
                .collect();
            assert!(
                titles.contains(&"Host window") && titles.contains(&"Guest window"),
                "the host's window has to arrive at all; got {titles:?}",
            );
            assert_eq!(
                ws.strips.len(),
                2,
                "each shared strip must remain addressable instead of folding \
                 its windows into a local strip; got {:?}",
                ws.strips
                    .iter()
                    .map(|s| (s.id.raw(), s.window_groups.len()))
                    .collect::<Vec<_>>(),
            );
            assert!(
                ws.strips.iter().all(|strip| strip.window_groups.len() == 1),
                "each owner keeps one group in its own strip: {:?}",
                ws.strips
                    .iter()
                    .map(|strip| (strip.id.raw(), strip.window_groups.len()))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                guest.desktop.active_strip.get(&guest_workspace),
                Some(&guest_strip),
                "layout is shared; personal focus is not",
            );
        }

        // Phase `a_window_the_host_opens_appears_for_the_guest_without_moving_them`.
        // A window one member opens appears for the other — **and the
        // other stays on the tab they were reading.** Both halves in one
        // test: projecting the whole struct passes one and fails the
        // other; projecting nothing does the reverse.
        {
            const HOST: u16 = 0xAAAA;
            const GUEST: u16 = 0xBBBB;
            let (mut host, mut guest, room) = two_in_a_room();
            host.set_salt_override(HOST);
            guest.set_salt_override(GUEST);

            let (_host_strip, host_group, host_tab) = seed_single_tab_strip(&mut host, room, HOST);
            // The guest has a desk of their own, and is reading their second
            // tab — the thing that must not move.
            let (guest_ws, guest_strip, guest_group, _guest_tab_a, guest_tab_b) =
                seed_two_tab_strip_and_focus_second(&mut guest, room, GUEST);

            let changes = host
                .export_room_changes_since(room, &[])
                .expect("the host is in the room");
            guest
                .apply_room_changes(room, &changes, &PeerOrigin::hub(1))
                .expect("the guest merges");

            // The window opened for the guest.
            let gws = guest
                .desktop
                .workspaces
                .iter()
                .find(|w| w.room == room)
                .unwrap();
            let landed = gws
                .strips
                .iter()
                .flat_map(|s| s.window_groups.iter())
                .find(|g| g.id == host_group)
                .expect("the host's window group must exist on the guest");
            assert_eq!(
                landed
                    .tabs
                    .iter()
                    .map(|t| t.title.as_str())
                    .collect::<Vec<_>>(),
                vec!["Plan"],
                "the window must arrive with what is in it",
            );
            // Opened on its own first tab — decided here, not carried.
            assert_eq!(landed.active_tab, host_tab);

            // And the guest did not move.
            let mine = gws
                .strips
                .iter()
                .flat_map(|s| s.window_groups.iter())
                .find(|g| g.id == guest_group)
                .expect("the guest keeps its own window");
            assert_eq!(
                mine.active_tab, guest_tab_b,
                "the guest was reading their second tab; a window opening \
                 elsewhere in the room must not move them off it",
            );
            assert_eq!(
                guest.desktop.active_strip.get(&guest_ws).copied(),
                Some(guest_strip),
                "the guest's scrolled-to strip must not follow the host",
            );
            assert_eq!(
                guest.desktop.active_group.get(&guest_strip).copied(),
                Some(guest_group),
                "the guest's focus must not follow the host",
            );
        }

        // Phase `the_rooms_windows_replicate_but_not_the_tab_you_are_on`.
        // The other half of the same rule: the room's windows **are** in
        // the document, but `active_tab` is asserted against the
        // document's own keys rather than inferred from behaviour.
        {
            use automerge::{ROOT, ReadDoc};

            const HOST: u16 = 0x1111;
            let (mut host, _guest, room) = two_in_a_room();
            host.set_salt_override(HOST);
            // Spelled out under a real salt: what `bootstrap_default` made
            // carries salt 0, which is nobody's and is never projected.
            let ws_idx = host
                .desktop
                .workspaces
                .iter()
                .position(|w| w.room == room)
                .unwrap();
            let strip_id = StripId::from(salted(HOST, 1));
            let group_id = salted(HOST, 2);
            let tab_id = salted(HOST, 3);
            host.desktop.workspaces[ws_idx].strips = vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups: vec![WindowGroup {
                    id: group_id,
                    tabs: vec![Tab {
                        id: tab_id,
                        title: "Notes".into(),
                        tile: TileContent::AppPlaceholder {
                            kind: AppKind::Other("Notes".into()),
                            label: "Notes".into(),
                        },
                    }],
                    active_tab: tab_id,
                    width: WindowWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
                }],
            }];
            host.sync_room_doc(room);

            let doc = host.room_docs.get(&room).expect("a room document");
            let (_, contents) = doc
                .get(&ROOT, "contents")
                .ok()
                .flatten()
                .expect("the room's windows must be in its document");
            let (_, strip) = doc
                .get(&contents, projection::contents_key(strip_id.raw()).as_str())
                .ok()
                .flatten()
                .expect("the strip");
            let (_, groups) = doc.get(&strip, "groups").ok().flatten().expect("groups");
            let (_, group) = doc
                .get(&groups, projection::contents_key(group_id).as_str())
                .ok()
                .flatten()
                .expect("the group");

            let group_keys: Vec<String> = doc.keys(&group).collect();
            assert!(
                group_keys.contains(&"tabs".to_string()),
                "the group's tabs must replicate; got {group_keys:?}",
            );
            assert!(
                !group_keys.contains(&"active_tab".to_string()),
                "which tab someone is looking at is theirs, not the room's, and \
                 must not be in the document that crosses; got {group_keys:?}",
            );
        }

        // Phase `a_guest_merges_its_hosts_room_after_projecting_its_own`.
        // A guest that has projected its own room document must survive
        // merging the host's — this is a crash, not a wrong answer: both
        // sides project before syncing, and applying the other's changes
        // runs `PatchLog::migrate_actors`, which panics if the incoming
        // actor can't be spliced into the actor list in sort order.
        {
            for _ in 0..24 {
                // The host's room is the one its own workspace was minted
                // with, so the host actually administers it — the room being
                // synced has a governance to be right about.
                let mut host = WorkspaceTree::bootstrap_demo();
                let host_ws = host.desktop.workspaces[0].id;
                let room = host.room_of(host_ws).expect("bootstrap mints a room");
                host.adopt_ownerless_workspaces(&"alice".to_string());

                let mut guest = WorkspaceTree::bootstrap_demo();
                let guest_ws = guest.desktop.workspaces[0].id;
                assert!(guest.set_room(guest_ws, room));
                // The invite pins the host as the room's governor.
                assert!(guest.pin_room_authority(room, PeerOrigin::hub(1)));
                guest.adopt_ownerless_workspaces(&"bob".to_string());

                // The guest projects first — that is what leaves a patch log
                // on its room document for the host's actor to collide with.
                guest.sync_room_doc(room);

                let changes = host
                    .export_room_changes_since(room, &[])
                    .expect("the host is in the room");
                guest
                    .apply_room_changes(room, &changes, &PeerOrigin::hub(1))
                    .expect("the guest merges its host's room document");

                // The merge completes, and it is the host's governance that
                // comes out of it — not whichever of the two actor ids sorted
                // higher.
                assert_eq!(
                    guest
                        .desktop
                        .workspaces
                        .iter()
                        .find(|w| w.id == guest_ws)
                        .and_then(|w| w.owner().cloned())
                        .as_deref(),
                    Some("alice"),
                    "the guest must end up with the room's owner, not its own",
                );
                assert_eq!(
                    guest.room_of(guest_ws),
                    Some(room),
                    "the guest must still be in the room it merged",
                );
                assert!(
                    !guest
                        .desktop
                        .workspaces
                        .iter()
                        .find(|w| w.id == guest_ws)
                        .expect("the guest's workspace survives the merge")
                        .policy
                        .grants
                        .is_empty(),
                    "the merge must leave the room's grants behind, not wipe them",
                );
            }
        }
    }

    fn ship(from: &mut WorkspaceTree, to: &mut WorkspaceTree, room: RoomId, hub: u64) {
        let changes = from
            .export_room_changes_since(room, &[])
            .expect("in the room");
        to.apply_room_changes(room, &changes, &PeerOrigin::hub(hub))
            .expect("merges");
    }

    fn held(one: (&str, &str)) -> std::collections::BTreeMap<String, String> {
        std::collections::BTreeMap::from([(one.0.to_string(), one.1.to_string())])
    }

    /// An id in `salt`'s range, without touching the process-wide
    /// counter — `next_id()` reads a global salt a test can't flip
    /// without affecting every test beside it.
    fn salted(salt: u16, counter: u64) -> u64 {
        (u64::from(salt) << PEER_ID_SALT_SHIFT) | counter
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn room_records_replicate_as_references_without_view_state() {
        // Phase `a_record_referenced_here_reaches_the_other_member`.
        // The claim the whole room-index path exists to make: a record one
        // member puts in the room reaches the others.
        {
            let (mut host, mut guest, room) = two_in_a_room();
            host.set_room_records(room, held(("file:/docs/plan.md", "document")));

            ship(&mut host, &mut guest, room, 1);

            assert_eq!(
                guest.take_inbound_room_records(),
                vec![(
                    room,
                    "file:/docs/plan.md".to_string(),
                    "document".to_string()
                )],
                "the guest must be handed the reference its host made",
            );
        }

        // Phase `a_guest_can_put_a_record_in_the_room_too`.
        // And the other way: a room only its owner may add to isn't a
        // shared room — the projection isn't gated on governance the
        // way owner/capabilities are.
        {
            let (mut host, mut guest, room) = two_in_a_room();
            guest.set_room_records(room, held(("file:/docs/from-bob.md", "document")));

            ship(&mut guest, &mut host, room, 2);

            assert_eq!(
                host.take_inbound_room_records(),
                vec![(
                    room,
                    "file:/docs/from-bob.md".to_string(),
                    "document".to_string()
                )],
                "a guest's reference must reach the room's owner",
            );
        }

        // Phase `two_members_referencing_at_once_both_survive`.
        // Two people bringing something at the same moment — keyed by
        // record id rather than a list, so the merge is a union.
        {
            let (mut host, mut guest, room) = two_in_a_room();
            host.set_room_records(room, held(("file:/docs/from-ada.md", "document")));
            guest.set_room_records(room, held(("file:/docs/from-bob.md", "document")));

            // Neither has seen the other's yet: both project, then both merge.
            ship(&mut host, &mut guest, room, 1);
            ship(&mut guest, &mut host, room, 2);

            for (who, tree) in [("host", &mut host), ("guest", &mut guest)] {
                let doc = tree.room_docs.get(&room).expect("a room document");
                let ids: Vec<String> = projection::room_records_of(doc)
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect();
                assert_eq!(
                    ids,
                    vec![
                        "file:/docs/from-ada.md".to_string(),
                        "file:/docs/from-bob.md".to_string()
                    ],
                    "{who} lost one of two simultaneous references",
                );
            }
        }

        // Phase `re_projecting_the_same_index_says_nothing_new`.
        // Re-projecting unchanged state must not manufacture a change, or
        // two peers exporting to each other would never fall quiet.
        {
            let (mut host, _guest, room) = two_in_a_room();
            host.set_room_records(room, held(("file:/docs/plan.md", "document")));
            host.sync_room_doc(room);
            let heads = host.room_heads(room);

            host.sync_room_doc(room);
            host.sync_room_doc(room);

            assert_eq!(
                host.room_heads(room),
                heads,
                "an export of unchanged state must add no changes",
            );
        }

        // Phase `a_reference_already_held_here_is_not_an_arrival`.
        // A record already known here isn't re-announced as an arrival —
        // otherwise the shell gets handed the same reference every round.
        {
            let (mut host, mut guest, room) = two_in_a_room();
            host.set_room_records(room, held(("file:/docs/plan.md", "document")));
            ship(&mut host, &mut guest, room, 1);
            assert_eq!(guest.take_inbound_room_records().len(), 1);

            // The shell folded it in, so the guest now holds it too.
            guest.set_room_records(room, held(("file:/docs/plan.md", "document")));
            ship(&mut host, &mut guest, room, 1);

            assert!(
                guest.take_inbound_room_records().is_empty(),
                "a reference this machine already holds must not arrive again",
            );
        }

        // Phase `the_room_document_carries_no_view_state`.
        // **The view does not follow, the state does.** The room document
        // is what crosses, so the guarantee is checkable by looking at
        // what's in it: which records the room holds, nothing about who's
        // standing where.
        {
            use automerge::{ROOT, ReadDoc};

            let (mut host, _guest, room) = two_in_a_room();
            host.set_room_records(room, held(("file:/docs/plan.md", "document")));
            // Put this session somewhere specific, so anything that leaked
            // would have a value to leak.
            let ws = host.desktop.workspaces[0].id;
            let strip = host.desktop.workspaces[0].strips[0].id;
            let group = host.desktop.workspaces[0].strips[0].window_groups[0].id;
            host.desktop.active_strip.insert(ws, strip);
            host.desktop.set_active_group(strip, group);
            host.sync_room_doc(room);

            let doc = host.room_docs.get(&room).expect("a room document");
            let keys: Vec<String> = doc.keys(&ROOT).collect();
            assert!(
                keys.contains(&"records".to_string()),
                "the room's records must be in its document; got {keys:?}",
            );
            for forbidden in ["active", "active_strip", "active_group", "workspaces"] {
                assert!(
                    !keys.contains(&forbidden.to_string()),
                    "{forbidden:?} is a fact about a person at a desk, not about the \
                     room, and must not be in the document that crosses; got {keys:?}",
                );
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn local_geometry_and_own_windows_stay_local_when_topology_syncs() {
        // Phase `remote_topology_updates_preserve_this_desktops_group_geometry`.
        {
            const HOST: u16 = 0xCAFE;
            const GUEST: u16 = 0xBEEF;
            let (mut host, mut guest, room) = two_in_a_room();
            host.set_salt_override(HOST);
            guest.set_salt_override(GUEST);

            let (_host_strip, host_group, _host_tab) = seed_single_tab_strip(&mut host, room, HOST);
            let _ = seed_single_tab_strip(&mut guest, room, GUEST);

            ship(&mut host, &mut guest, room, 1);

            // The guest chooses a deliberately different presentation for the
            // host's shared group. This must survive all later room updates.
            let guest_ws = guest
                .desktop
                .workspaces
                .iter_mut()
                .find(|ws| ws.room == room)
                .unwrap();
            let guest_host_group = guest_ws
                .strips
                .iter_mut()
                .flat_map(|strip| strip.window_groups.iter_mut())
                .find(|group| group.id == host_group)
                .expect("the host group arrives on the guest desktop");
            guest_host_group.width = WindowWidth::Fraction(775);
            guest_host_group.maximized = true;

            // The host makes both a presentation change and an intentional shared
            // topology change. Only the new tab is allowed to cross the room.
            let host_ws = host
                .desktop
                .workspaces
                .iter_mut()
                .find(|ws| ws.room == room)
                .unwrap();
            let host_group_ref = host_ws
                .strips
                .iter_mut()
                .flat_map(|strip| strip.window_groups.iter_mut())
                .find(|group| group.id == host_group)
                .expect("host group remains present");
            host_group_ref.width = WindowWidth::Fraction(500);
            host_group_ref.maximized = false;
            host_group_ref.tabs.push(Tab {
                id: salted(HOST, 4),
                title: "Follow-up".into(),
                tile: TileContent::Stub {
                    kind: AppKind::Terminal,
                    label: "Follow-up".into(),
                },
            });
            host.sync_room_doc(room);
            ship(&mut host, &mut guest, room, 1);

            let guest_host_group = guest
                .desktop
                .workspaces
                .iter()
                .find(|ws| ws.room == room)
                .into_iter()
                .flat_map(|ws| &ws.strips)
                .flat_map(|strip| &strip.window_groups)
                .find(|group| group.id == host_group)
                .expect("the shared group remains present");
            assert_eq!(guest_host_group.width, WindowWidth::Fraction(775));
            assert!(guest_host_group.maximized);
            assert_eq!(
                guest_host_group
                    .tabs
                    .iter()
                    .map(|tab| tab.title.as_str())
                    .collect::<Vec<_>>(),
                vec!["Plan", "Follow-up"],
                "the intentional shared topology update must still arrive"
            );
        }

        // Phase `a_peer_may_not_rewrite_this_machines_own_window`.
        // A peer may add to the room; it may not rewrite what this machine
        // owns. The batch is refused whole rather than partly merged.
        {
            const HOST: u16 = 0xA1A1;
            const GUEST: u16 = 0xB2B2;
            let (mut host, mut guest, room) = two_in_a_room();
            host.set_salt_override(HOST);
            guest.set_salt_override(GUEST);

            let strip = StripId::from(salted(HOST, 1));
            let group = salted(HOST, 2);
            let tab = salted(HOST, 3);
            let hi = host
                .desktop
                .workspaces
                .iter()
                .position(|w| w.room == room)
                .unwrap();
            host.desktop.workspaces[hi].strips = vec![Strip {
                id: strip,
                name: "Main".into(),
                window_groups: vec![WindowGroup {
                    id: group,
                    tabs: vec![Tab {
                        id: tab,
                        title: "Plan".into(),
                        tile: TileContent::Stub {
                            kind: AppKind::Terminal,
                            label: "Plan".into(),
                        },
                    }],
                    active_tab: tab,
                    width: WindowWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
                }],
            }];

            // The guest takes the host's document, renames the host's own
            // window in it, and offers it back.
            let seed = host
                .export_room_changes_since(room, &[])
                .expect("host is in the room");
            guest
                .apply_room_changes(room, &seed, &PeerOrigin::hub(1))
                .expect("guest merges the host's room");
            {
                use automerge::{ROOT, ReadDoc, transaction::Transactable};
                let doc = guest.room_docs.get_mut(&room).expect("room doc");
                let (_, contents) = doc.get(&ROOT, "contents").ok().flatten().expect("contents");
                let (_, s_entry) = doc
                    .get(&contents, projection::contents_key(strip.raw()).as_str())
                    .ok()
                    .flatten()
                    .expect("strip");
                let (_, groups) = doc.get(&s_entry, "groups").ok().flatten().expect("groups");
                let (_, g_entry) = doc
                    .get(&groups, projection::contents_key(group).as_str())
                    .ok()
                    .flatten()
                    .expect("group");
                let (_, tabs) = doc.get(&g_entry, "tabs").ok().flatten().expect("tabs");
                let (_, t_entry) = doc
                    .get(&tabs, projection::contents_key(tab).as_str())
                    .ok()
                    .flatten()
                    .expect("tab");
                let _ = doc.put(&t_entry, "title", "Hijacked".to_string());
            }
            let tampered = guest
                .export_room_changes_since(room, &[])
                .expect("guest is in the room");

            let accepted = host
                .apply_room_changes(room, &tampered, &PeerOrigin::hub(2))
                .expect("the check is a refusal, not an error");
            assert!(
                !accepted,
                "a batch rewriting our own window must be refused"
            );
            assert_eq!(
                host.desktop.workspaces[hi].strips[0].window_groups[0].tabs[0].title, "Plan",
                "and nothing about it may be stored",
            );
        }

        // Phase `maximization_stays_on_the_local_desktop_when_topology_syncs`.
        {
            // Tree B starts from `default()`, same reason as the append test.
            let mut tree_a = WorkspaceTree::bootstrap_demo();
            let target_group = tree_a.desktop.workspaces[0].strips[0].window_groups[0].id;
            assert!(
                !tree_a.desktop.workspaces[0].strips[0].window_groups[0].maximized,
                "bootstrap_default seeds the first group un-maximized"
            );

            // This is local presentation state. Synchronizing the workspace's
            // topology must not carry this maximize decision to a second desktop.
            tree_a.desktop.workspaces[0].strips[0].window_groups[0].maximized = true;

            let changes = tree_a.export_changes_since(&[]);
            let mut tree_b = WorkspaceTree::default();
            tree_b.apply_changes(&changes).expect("apply changes");

            let target = tree_b
                .desktop
                .workspaces
                .iter()
                .flat_map(|w| w.strips.iter())
                .flat_map(|s| s.window_groups.iter())
                .find(|g| g.id == target_group);
            assert!(
                target.is_some(),
                "tree B should observe the window group after sync"
            );
            assert!(
                !target.unwrap().maximized,
                "a peer's maximize flip must not rearrange this desktop; got {target:?}"
            );
        }

        // Phase `width_stays_on_the_local_desktop_when_topology_syncs`.
        {
            let mut tree_a = WorkspaceTree::bootstrap_demo();
            let target_group = tree_a.desktop.workspaces[0].strips[0].window_groups[0].id;
            let new_share: u16 = 600;

            tree_a.desktop.workspaces[0].strips[0].window_groups[0].width =
                WindowWidth::Fraction(new_share);

            let changes = tree_a.export_changes_since(&[]);
            let mut tree_b = WorkspaceTree::default();
            tree_b.apply_changes(&changes).expect("apply changes");

            let target = tree_b
                .desktop
                .workspaces
                .iter()
                .flat_map(|w| w.strips.iter())
                .flat_map(|s| s.window_groups.iter())
                .find(|g| g.id == target_group);
            assert!(
                target.is_some(),
                "tree B should observe the window group after sync"
            );
            assert_eq!(
                target.unwrap().width,
                WindowWidth::Normal,
                "a peer's resize must not change this desktop's width"
            );
        }
    }

    /// Give `tree`'s workspace in `room` a single strip/group holding one
    /// tab titled "Plan", salted under `salt`. Returns `(strip, group,
    /// tab)`. Half of
    /// [`a_window_the_host_opens_appears_for_the_guest_without_moving_them`]'s
    /// fixture — the host's side.
    #[test]
    fn legacy_windows_cross_the_room_as_hosted_and_tilo_apps_as_windows_to_open() {
        const HOST: u16 = 0xCAFE;
        const GUEST: u16 = 0xBEEF;
        let (mut host, mut guest, room) = two_in_a_room();
        host.set_salt_override(HOST);
        guest.set_salt_override(GUEST);
        let (_, _, tab) = seed_single_tab_strip(&mut host, room, HOST);
        let native = salted(HOST, 4);
        {
            let ws = host
                .desktop
                .workspaces
                .iter_mut()
                .find(|w| w.room == room)
                .unwrap();
            let group = &mut ws.strips[0].window_groups[0];
            group.tabs[0].tile = TileContent::Foreign {
                app_id: "org.gimp.GIMP".into(),
                surface_id: 41,
            };
            group.tabs.push(Tab {
                id: native,
                title: "Groceries".into(),
                tile: TileContent::Foreign {
                    app_id: "so.tilo.notes".into(),
                    surface_id: 42,
                },
            });
        }
        host.window_hosting = Some(WindowHosting {
            device: "ab".repeat(32),
            device_label: "Laptop".into(),
        });
        host.remember_package_launch(native, "so.tilo.notes".into());

        ship(&mut host, &mut guest, room, 1);

        let tiles: std::collections::HashMap<u64, TileContent> = guest
            .desktop
            .workspaces
            .iter()
            .find(|w| w.room == room)
            .into_iter()
            .flat_map(|w| &w.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| (tab.id, tab.tile.clone()))
            .collect();
        assert_eq!(
            tiles.get(&tab),
            Some(&TileContent::HostedWindow {
                app_id: "org.gimp.GIMP".into(),
                label: "Plan".into(),
                host: "ab".repeat(32),
                host_label: "Laptop".into(),
            }),
            "a legacy window is shown as running on its host"
        );
        assert!(
            matches!(
                tiles.get(&native),
                Some(TileContent::PeerWindow { app_id, .. }) if app_id == "so.tilo.notes"
            ),
            "a Tilo app's window is one to open here"
        );
        assert!(
            matches!(
                host.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].tile,
                TileContent::Foreign { .. }
            ),
            "the host keeps its own live window"
        );
    }

    fn seed_single_tab_strip(
        tree: &mut WorkspaceTree,
        room: RoomId,
        salt: u16,
    ) -> (StripId, u64, u64) {
        let strip = StripId::from(salted(salt, 1));
        let group = salted(salt, 2);
        let tab = salted(salt, 3);
        let i = tree
            .desktop
            .workspaces
            .iter()
            .position(|w| w.room == room)
            .unwrap();
        tree.desktop.workspaces[i].strips = vec![Strip {
            id: strip,
            name: "Main".into(),
            window_groups: vec![WindowGroup {
                id: group,
                tabs: vec![Tab {
                    id: tab,
                    title: "Plan".into(),
                    tile: TileContent::Stub {
                        kind: AppKind::Terminal,
                        label: "Plan".into(),
                    },
                }],
                active_tab: tab,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            }],
        }];
        (strip, group, tab)
    }

    /// Give `tree`'s workspace in `room` a strip/group holding two tabs
    /// ("One", "Two"), salted under `salt`, and focus it on the second
    /// tab. Returns `(workspace, strip, group, tab_a, tab_b)`. The other
    /// half of
    /// [`a_window_the_host_opens_appears_for_the_guest_without_moving_them`]'s
    /// fixture — the guest's side, whose focus must not move.
    fn seed_two_tab_strip_and_focus_second(
        tree: &mut WorkspaceTree,
        room: RoomId,
        salt: u16,
    ) -> (WorkspaceId, StripId, u64, u64, u64) {
        let strip = StripId::from(salted(salt, 1));
        let group = salted(salt, 2);
        let tab_a = salted(salt, 3);
        let tab_b = salted(salt, 4);
        let i = tree
            .desktop
            .workspaces
            .iter()
            .position(|w| w.room == room)
            .unwrap();
        let ws = tree.desktop.workspaces[i].id;
        tree.desktop.workspaces[i].strips = vec![Strip {
            id: strip,
            name: "Mine".into(),
            window_groups: vec![WindowGroup {
                id: group,
                tabs: vec![
                    Tab {
                        id: tab_a,
                        title: "One".into(),
                        tile: TileContent::Stub {
                            kind: AppKind::Terminal,
                            label: "One".into(),
                        },
                    },
                    Tab {
                        id: tab_b,
                        title: "Two".into(),
                        tile: TileContent::Stub {
                            kind: AppKind::Terminal,
                            label: "Two".into(),
                        },
                    },
                ],
                active_tab: tab_b,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            }],
        }];
        tree.desktop.active_strip.insert(ws, strip);
        tree.desktop.set_active_group(strip, group);
        (ws, strip, group, tab_a, tab_b)
    }

    // Write authority on a room's governance: automerge has no per-key
    // ACL, so a peer projecting its own cache isn't proposing a change,
    // it's entering a coin toss for it — these pin that governance
    // comes only from the entitled peer.

    /// The room's real administrator, as the receiver knows it.
    const ROOM_OWNER: &str = "alice-administers-the-room";
    /// A guest that was invited in.
    const ROOM_GUEST: &str = "bob-was-invited";
    /// The capability the owner granted, and the one a guest tries to add.
    const OWNER_GRANT_PATH: &str = "/home/alice/room-notes";
    const ROGUE_GRANT_PATH: &str = "/etc/shadow";

    /// A host in a room it minted, owning it, with one grant of its own.
    /// Minted via `append_empty_workspace`, not `set_room` — minting
    /// and joining are different acts, and the difference is the
    /// whole subject here.
    fn host_owning_a_room() -> (WorkspaceTree, WorkspaceId, RoomId) {
        let mut host = WorkspaceTree::default();
        let ws = host.append_empty_workspace("Design review");
        let room = host.room_of(ws).expect("a fresh workspace mints a room");
        host.adopt_ownerless_workspaces(&ROOM_OWNER.to_string());
        host.grant_sandbox_in_workspace(
            ws,
            &ROOM_OWNER.to_string(),
            CapOverride::grant(
                Subject::app("files"),
                Scope::FsPath(OWNER_GRANT_PATH.to_string()),
                Access::Read,
            )
            .with_id("owners-own-grant"),
        )
        .expect("the owner may grant in the room it owns");
        (host, ws, room)
    }

    /// The bytes a guest puts on the wire when it does not honour the
    /// join: in `room`, but its own state still says it governs, so its
    /// projection carries its own owner and grants. Built by driving
    /// the guest's projection/export path, not by hand.
    fn governance_claim_from(user: &str, room: RoomId) -> Vec<Vec<u8>> {
        let mut guest = WorkspaceTree::default();
        let guest_ws = guest.append_empty_workspace("Mine");
        assert!(guest.set_room(guest_ws, room), "the guest joins the room");
        // Nothing stops a peer from believing it administers a room —
        // it's *its* machine — which is why the receiver can't take the
        // sender's word for who governs it.
        guest.room_authority.insert(room, RoomAuthority::Local);
        {
            let ws = guest
                .desktop
                .workspaces
                .iter_mut()
                .find(|w| w.id == guest_ws)
                .expect("the guest's own workspace");
            ws.set_owner(user.to_string());
        }
        guest
            .grant_sandbox_in_workspace(
                guest_ws,
                &user.to_string(),
                CapOverride::grant(
                    Subject::app("rogue"),
                    Scope::FsPath(ROGUE_GRANT_PATH.to_string()),
                    Access::Write,
                )
                .with_id("rogue-grant"),
            )
            .expect("the guest grants in its own copy");
        guest.sync_room_doc(room);
        guest
            .export_room_changes_since(room, &[])
            .expect("the guest is in the room it exports")
    }

    /// The owner and capability ids the receiver's own **room document**
    /// records — not its cache, so a divergent fold can't hide behind it.
    fn governance_in_room_doc(tree: &WorkspaceTree, room: RoomId) -> (Option<String>, Vec<String>) {
        use automerge::{ROOT, ReadDoc};
        use tilo_automerge::ReadTyped as _;
        let Some(doc) = tree.room_docs.get(&room) else {
            return (None, Vec::new());
        };
        let owner = doc.str_at(&ROOT, "owner");
        let mut caps: Vec<String> =
            match doc.get(&ROOT, projection::ROOM_SANDBOX_KEY).ok().flatten() {
                Some((_, caps_id)) => doc.keys(&caps_id).collect(),
                None => Vec::new(),
            };
        caps.sort();
        (owner, caps)
    }

    /// A real endpoint id, derived from a keypair like a running
    /// session's, so the tests exercise the same parse production does.
    fn peer_id(seed: u8) -> String {
        iroh::SecretKey::from_bytes(&[seed; 32])
            .public()
            .to_string()
    }

    /// The handle presence frames identify `peer` by.
    fn handle_of(peer: &str) -> u64 {
        crate::network::behaviour::peer_liveness_handle(
            &peer.parse::<iroh::EndpointId>().expect("a peer id"),
        )
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn peer_names_replicate_from_the_inviter_and_need_room_authority() {
        // Phase `peer_names_given_by_the_inviter_replicate_to_every_member`.
        {
            // Phase `the_inviter_names_a_peer_and_the_room_answers_to_its_handle`.
            // A peer the inviter named is shown under that name, addressed by
            // the handle a caret arrives under.
            {
                let (mut host, _ws, room) = host_owning_a_room();
                let bob = peer_id(9);

                assert!(
                    host.name_peer(room, &ROOM_OWNER.to_string(), &bob, "bob"),
                    "the room's administrator may name a peer in it",
                );

                assert_eq!(
                    host.room_peer_names_in_doc(room)
                        .get(&bob)
                        .map(String::as_str),
                    Some("bob"),
                    "the name has to be in the room document, which is what replicates",
                );
                assert_eq!(
                    host.room_peer_labels(room)
                        .get(&handle_of(&bob))
                        .map(String::as_str),
                    Some("bob"),
                    "and it has to be reachable from the handle a presence frame carries",
                );
            }

            // Phase `a_host_that_restarted_still_knows_who_it_invited`.
            // A host that has just restarted still knows who it invited — room
            // documents aren't persisted, but the name itself *is*, so it's
            // reachable before the first sync round.
            {
                let (mut host, _ws, room) = host_owning_a_room();
                let bob = peer_id(9);
                assert!(host.name_peer(room, &ROOM_OWNER.to_string(), &bob, "bob"));

                // What a restart leaves: the cache, and no projected document.
                host.room_docs.clear();

                assert_eq!(
                    host.room_peer_labels(room)
                        .get(&handle_of(&bob))
                        .map(String::as_str),
                    Some("bob"),
                    "the machine that governs the room is the one the document is \
                     projected from, so its cache is the same authority",
                );
            }

            // Phase `a_guest_reads_names_only_out_of_the_replicated_document`.
            // A guest gets no such shortcut — what it knows is what replicated;
            // a name in its cache with none in the document was never entitled.
            {
                let (mut host, mut guest, room) = two_in_a_room();
                let bob = peer_id(9);
                assert!(host.name_peer(room, &"ada".to_string(), &bob, "bob"));
                ship(&mut host, &mut guest, room, 1);
                assert!(
                    guest
                        .room_peer_labels(room)
                        .contains_key(&handle_of(&bob))
                );

                // A name the guest holds locally and the room does not.
                guest
                    .room_peer_names
                    .entry(room)
                    .or_default()
                    .insert(peer_id(11), "Somebody Else".to_string());

                let labels = guest.room_peer_labels(room);
                assert!(
                    !labels.values().any(|n| n == "Somebody Else"),
                    "a guest must not show a name that is not in the room document",
                );
                assert_eq!(
                    labels.get(&handle_of(&bob)).map(String::as_str),
                    Some("bob")
                );
            }

            // Phase `a_peer_the_room_has_not_named_gets_no_label_at_all`.
            // A peer nobody named is left as the id it arrived under — an
            // unnamed caret is honest, an invented name is not.
            {
                let (mut host, _ws, room) = host_owning_a_room();
                let named = peer_id(9);
                let stranger = peer_id(10);
                assert!(host.name_peer(room, &ROOM_OWNER.to_string(), &named, "bob"));

                let labels = host.room_peer_labels(room);
                assert!(
                    !labels.contains_key(&handle_of(&stranger)),
                    "a peer nobody named must have no label, so the caret keeps its id",
                );
                assert_eq!(
                    labels.len(),
                    1,
                    "naming one peer must not manufacture a label for any other",
                );
            }

            // Phase `the_name_the_inviter_gave_reaches_a_peer_that_did_not_write_it`.
            // The name is a fact about the room, so it travels with the room —
            // to a peer that never wrote it and cannot have made it up.
            {
                let (mut host, mut guest, room) = two_in_a_room();
                let bob = peer_id(9);
                // `two_in_a_room` makes the host the owner of its own workspace;
                // it is the machine that minted the room, so it governs it.
                assert!(host.name_peer(room, &"ada".to_string(), &bob, "bob"));

                ship(&mut host, &mut guest, room, 1);

                assert_eq!(
                    guest
                        .room_peer_names_in_doc(room)
                        .get(&bob)
                        .map(String::as_str),
                    Some("bob"),
                    "a guest reads names out of the replicated room document",
                );
                assert_eq!(
                    guest
                        .room_peer_labels(room)
                        .get(&handle_of(&bob))
                        .map(String::as_str),
                    Some("bob"),
                );
            }

            // Phase `un_naming_a_peer_reaches_the_peer_that_held_the_name`.
            // Taking a wrong name back has to reach the peers that already hold
            // it, or a mislabelled caret is mislabelled forever.
            {
                let (mut host, mut guest, room) = two_in_a_room();
                let bob = peer_id(9);
                assert!(host.name_peer(room, &"ada".to_string(), &bob, "bob"));
                ship(&mut host, &mut guest, room, 1);
                assert!(guest.room_peer_names_in_doc(room).contains_key(&bob));

                assert!(
                    host.name_peer(room, &"ada".to_string(), &bob, ""),
                    "an empty name un-names the peer",
                );
                ship(&mut host, &mut guest, room, 1);

                assert!(
                    !guest.room_peer_names_in_doc(room).contains_key(&bob),
                    "the retraction has to replicate, not just the name",
                );
            }
        }

        // Phase `naming_a_peer_needs_room_authority_and_a_real_peer`.
        {
            // Phase `a_guest_cannot_name_a_peer_in_the_room_it_joined`.
            // The whole reason the name lives in governance — a guest able to
            // write this map could put any name over any caret in the room.
            {
                let (mut host, _ws, room) = host_owning_a_room();
                let bob = peer_id(9);
                host.name_peer(room, &ROOM_OWNER.to_string(), &bob, "bob");
                host.sync_room_doc(room);

                // A guest whose own state says it governs the room, projecting
                // a peers map that renames the host's invitee.
                let mut guest = WorkspaceTree::default();
                let guest_ws = guest.append_empty_workspace("Mine");
                assert!(guest.set_room(guest_ws, room));
                guest.room_authority.insert(room, RoomAuthority::Local);
                {
                    let ws = guest
                        .desktop
                        .workspaces
                        .iter_mut()
                        .find(|w| w.id == guest_ws)
                        .expect("the guest's own workspace");
                    ws.set_owner(ROOM_GUEST.to_string());
                }
                assert!(guest.name_peer(room, &ROOM_GUEST.to_string(), &bob, "ada"));
                let claim = guest
                    .export_room_changes_since(room, &[])
                    .expect("the guest is in the room it exports");

                host.apply_room_changes(room, &claim, &PeerOrigin::hub(2))
                    .expect("well-formed change blobs parse");

                assert_eq!(
                    host.room_peer_names_in_doc(room)
                        .get(&bob)
                        .map(String::as_str),
                    Some("bob"),
                    "a guest must not be able to rename anyone in a room it joined",
                );
            }

            // Phase `naming_a_peer_needs_authority_over_the_room`.
            // Naming somebody is a governance act, so it needs the same
            // authority a grant needs — on the machine and on the person.
            {
                let (mut host, _ws, room) = host_owning_a_room();
                let bob = peer_id(9);

                assert!(
                    !host.name_peer(room, &ROOM_GUEST.to_string(), &bob, "bob"),
                    "somebody who does not administer the room may not name in it",
                );
                assert!(
                    host.room_peer_names_in_doc(room).is_empty(),
                    "a refused naming must write nothing",
                );

                // And a machine that does not govern the room may not either,
                // however its own workspace reads.
                let mut joiner = WorkspaceTree::default();
                let joiner_ws = joiner.append_empty_workspace("Mine");
                assert!(joiner.set_room(joiner_ws, room));
                joiner.adopt_ownerless_workspaces(&ROOM_GUEST.to_string());
                assert!(
                    !joiner.name_peer(room, &ROOM_GUEST.to_string(), &bob, "bob"),
                    "a machine that joined the room does not get to name its peers",
                );
            }

            // Phase `a_name_has_to_be_attached_to_a_real_peer_id`.
            // A key that isn't a peer id names nobody — storing one would put
            // a row no presence frame can ever match, reading later as a name
            // that failed to replicate rather than one never addressed to anyone.
            {
                let (mut host, _ws, room) = host_owning_a_room();
                assert!(!host.name_peer(room, &ROOM_OWNER.to_string(), "bob", "bob"));
                assert!(!host.name_peer(room, &ROOM_OWNER.to_string(), "", "bob"));
                assert!(host.room_peer_names_in_doc(room).is_empty());
            }
        }
    }

    #[test]
    fn room_peer_principals_maps_handles_to_certified_people() {
        let (mut host, mut guest, room) = two_in_a_room();
        let endpoint = iroh::SecretKey::generate().public();
        let named_only = iroh::SecretKey::generate().public();
        let root_key = ed25519_dalek::SigningKey::from_bytes(&[93; 32]);
        let mut certificate = tilo_user_identity::DeviceCert {
            user_public_key: root_key.verifying_key().to_bytes(),
            device_public_key: *endpoint.as_bytes(),
            device_label: "Bob's laptop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &root_key).unwrap();
        let owner = "ada".to_string();
        assert!(host.name_peer(room, &owner, &endpoint.to_string(), "Bob"));
        assert!(host.name_peer(room, &owner, &named_only.to_string(), "Carol"));
        assert!(host.certify_peer_identity(room, &endpoint.to_string(), certificate.clone()));

        let handle = crate::network::behaviour::peer_liveness_handle(&endpoint);
        let bob = person_id_of(root_key.verifying_key().to_bytes()).unwrap();
        let expected = std::collections::BTreeMap::from([(handle, bob)]);
        assert_eq!(
            host.room_peer_principals(room),
            expected,
            "a certified device maps to its person, spelled as membership names them; \
             a peer that is only named maps to nobody",
        );

        ship(&mut host, &mut guest, room, 1);
        assert_eq!(
            guest.room_peer_principals(room),
            expected,
            "a guest reads the same people out of the governed room document",
        );

        let mut revocation = tilo_user_identity::RevocationEntry {
            user_public_key: root_key.verifying_key().to_bytes(),
            revoked_device_public_key: *endpoint.as_bytes(),
            revoked_at_ns: 2,
            reason: tilo_user_identity::RevocationReason::Retired,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut revocation, &root_key).unwrap();
        assert!(host.certify_peer_identity_with_revocations(
            room,
            &endpoint.to_string(),
            certificate,
            vec![revocation]
        ));
        assert!(
            host.room_peer_principals(room).is_empty(),
            "a revoked device speaks for nobody",
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn root_signed_governance_projects_identity_and_additive_revocation() {
        // Phase `governed_name_and_root_signed_endpoint_project_human_and_device`.
        {
            let (mut host, mut guest, room) = two_in_a_room();
            let device_secret = iroh::SecretKey::generate();
            let endpoint = device_secret.public();
            let root_key = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
            let mut certificate = tilo_user_identity::DeviceCert {
                user_public_key: root_key.verifying_key().to_bytes(),
                device_public_key: *endpoint.as_bytes(),
                device_label: "Studio laptop".into(),
                capabilities: tilo_user_identity::DeviceCapabilities::member(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            tilo_user_identity::sign_device_cert(&mut certificate, &root_key).unwrap();
            assert!(host.name_peer(
                room,
                &"ada".to_string(),
                &endpoint.to_string(),
                "Ada Lovelace"
            ));
            assert!(host.certify_peer_identity(room, &endpoint.to_string(), certificate.clone()));

            let projection = host.trusted_identity_projection(room);
            assert_eq!(projection.humans.len(), 1);
            assert_eq!(projection.humans[0].display_name, "Ada Lovelace");
            assert_eq!(
                projection.humans[0].devices[0]
                    .certificate
                    .device_public_key,
                *endpoint.as_bytes()
            );

            ship(&mut host, &mut guest, room, 1);
            assert_eq!(guest.trusted_identity_projection(room), projection);

            {
                use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
                let doc = guest.room_docs.get_mut(&room).unwrap();
                let (_, identities) = doc
                    .get(&ROOT, WorkspaceTree::ROOM_PEER_IDENTITIES_KEY)
                    .unwrap()
                    .unwrap();
                let mut spoofed = certificate.clone();
                spoofed.device_label = "Spoofed label".into();
                let bytes = serde_json::to_vec(&GovernedPeerIdentity {
                    device_cert: spoofed,
                    revocations: Vec::new(),
                    delegate: None,
                })
                .unwrap();
                doc.put(&identities, endpoint.to_string(), ScalarValue::Bytes(bytes))
                    .unwrap();
                let _ = tilo_resource::history::commit_stamped(doc);
            }
            let spoof = guest.export_room_changes_since(room, &[]).unwrap();
            assert!(
                !host
                    .apply_room_changes(room, &spoof, &PeerOrigin::hub(2))
                    .unwrap(),
                "a guest cannot rewrite a governed certificate row"
            );
            assert_eq!(host.trusted_identity_projection(room), projection);

            let other = iroh::SecretKey::generate().public();
            assert!(
                !host.certify_peer_identity(room, &other.to_string(), certificate),
                "a valid certificate cannot be rebound to another transport endpoint"
            );
        }

        // Phase `root_signed_room_revocation_is_additive_and_prevents_rename_resurrection`.
        {
            let (mut host, _guest, room) = two_in_a_room();
            let device_secret = iroh::SecretKey::generate();
            let endpoint = device_secret.public();
            let root_key = ed25519_dalek::SigningKey::from_bytes(&[96; 32]);
            let mut certificate = tilo_user_identity::DeviceCert {
                user_public_key: root_key.verifying_key().to_bytes(),
                device_public_key: *endpoint.as_bytes(),
                device_label: "Retired laptop".into(),
                capabilities: tilo_user_identity::DeviceCapabilities::member(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            tilo_user_identity::sign_device_cert(&mut certificate, &root_key).unwrap();
            assert!(host.name_peer(room, &"ada".to_string(), &endpoint.to_string(), "Ada"));
            assert!(host.certify_peer_identity(room, &endpoint.to_string(), certificate.clone()));
            assert_eq!(host.trusted_identity_projection(room).humans.len(), 1);

            let mut revocation = tilo_user_identity::RevocationEntry {
                user_public_key: root_key.verifying_key().to_bytes(),
                revoked_device_public_key: *endpoint.as_bytes(),
                revoked_at_ns: 2,
                reason: tilo_user_identity::RevocationReason::Retired,
                signature: [0; 64],
            };
            tilo_user_identity::sign_revocation_entry(&mut revocation, &root_key).unwrap();
            assert!(host.certify_peer_identity_with_revocations(
                room,
                &endpoint.to_string(),
                certificate.clone(),
                vec![revocation]
            ));
            assert!(host.trusted_identity_projection(room).humans.is_empty());

            // Omission is not removal, and un-name/re-name cannot revive it.
            assert!(!host.certify_peer_identity(room, &endpoint.to_string(), certificate));
            assert!(host.name_peer(room, &"ada".to_string(), &endpoint.to_string(), ""));
            assert!(host.name_peer(
                room,
                &"ada".to_string(),
                &endpoint.to_string(),
                "Ada Renamed"
            ));
            assert!(host.trusted_identity_projection(room).humans.is_empty());
        }
    }

    /// A device governance admitted as a delegated agent is projected to
    /// Files as that agent, under the person it acts for, so the versions
    /// it signs read as the agent's on behalf of that person; a forged
    /// delegation names nothing, and the person stays projected either way.
    #[test]
    fn governed_agents_are_projected_as_agents_of_their_person() {
        let (mut host, mut guest, room) = two_in_a_room();
        let root_key = ed25519_dalek::SigningKey::from_bytes(&[97; 32]);
        let laptop = iroh::SecretKey::generate().public();
        let mut certificate = tilo_user_identity::DeviceCert {
            user_public_key: root_key.verifying_key().to_bytes(),
            device_public_key: *laptop.as_bytes(),
            device_label: "Studio laptop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &root_key).unwrap();
        assert!(host.name_peer(
            room,
            &"ada".to_string(),
            &laptop.to_string(),
            "Ada Lovelace"
        ));
        assert!(host.certify_peer_identity(room, &laptop.to_string(), certificate));

        let agent_device = iroh::SecretKey::generate().public();
        let mut agent_certificate = tilo_user_identity::DeviceCert {
            user_public_key: root_key.verifying_key().to_bytes(),
            device_public_key: *agent_device.as_bytes(),
            device_label: "Agent: Scribe".into(),
            capabilities: tilo_user_identity::CertificationPolicy::agent().ceiling,
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut agent_certificate, &root_key).unwrap();
        let delegation = |signer: &ed25519_dalek::SigningKey| {
            tilo_relay_protocol::delegation::Delegation::sign(
                signer,
                *agent_device.as_bytes(),
                tilo_relay_protocol::delegation::Delegate::Agent {
                    agent_id: "scribe".into(),
                },
                "Scribe",
                [10, 20, 30],
                tilo_relay_protocol::delegation::DelegateScope {
                    rooms: vec![uuid::Uuid::from_u128(7).to_string()],
                    apps: Vec::new(),
                },
                1,
                2,
            )
            .unwrap()
        };
        let govern_agent = |host: &mut WorkspaceTree, signer: &ed25519_dalek::SigningKey| {
            host.room_peer_identities.entry(room).or_default().insert(
                agent_device.to_string(),
                GovernedPeerIdentity {
                    device_cert: agent_certificate.clone(),
                    revocations: Vec::new(),
                    delegate: Some(tilo_peer_state::GovernedDelegateRow {
                        delegation: delegation(signer),
                        withdrawn_at_ns: None,
                        installation: None,
                    }),
                },
            );
            host.sync_room_doc(room);
        };

        // A delegation another root signed is no agent of this person.
        govern_agent(&mut host, &ed25519_dalek::SigningKey::from_bytes(&[98; 32]));
        let projection = host.trusted_identity_projection(room);
        assert_eq!(projection.humans.len(), 1, "the person stays projected");
        assert_eq!(projection.humans[0].devices.len(), 1);

        // Its person's: the agent's device is projected as the agent, even
        // after its window closed, so its work stays attributed.
        govern_agent(&mut host, &root_key);
        let projection = host.trusted_identity_projection(room);
        assert_eq!(projection.humans.len(), 1);
        let human = &projection.humans[0];
        assert_eq!(human.display_name, "Ada Lovelace");
        let agent = human
            .devices
            .iter()
            .find(|device| device.certificate.device_public_key == *agent_device.as_bytes())
            .expect("the agent is projected under its person");
        assert_eq!(
            agent.agent,
            Some(tilo_user_identity::GovernedAgentIdentityV2 {
                agent_id: "scribe".into(),
                label: "Scribe".into(),
                color: [10, 20, 30],
            })
        );
        assert!(
            human
                .devices
                .iter()
                .filter(|device| device.agent.is_none())
                .all(|device| device.certificate.device_public_key == *laptop.as_bytes())
        );

        ship(&mut host, &mut guest, room, 1);
        assert_eq!(guest.trusted_identity_projection(room), projection);
    }

    /// A room whose governance names Ada's laptop and admits Scribe, an
    /// agent device of Ada's, with a delegation `signer` signed. Returns the
    /// host, the room and Scribe's device key.
    fn room_with_ada_and_scribe(
        signer: &ed25519_dalek::SigningKey,
    ) -> (WorkspaceTree, RoomId, [u8; 32]) {
        let (mut host, _guest, room) = two_in_a_room();
        let root_key = ed25519_dalek::SigningKey::from_bytes(&[97; 32]);
        let certified = |device: [u8; 32], label: &str, capabilities| {
            let mut certificate = tilo_user_identity::DeviceCert {
                user_public_key: root_key.verifying_key().to_bytes(),
                device_public_key: device,
                device_label: label.into(),
                capabilities,
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            tilo_user_identity::sign_device_cert(&mut certificate, &root_key).unwrap();
            certificate
        };
        let laptop = iroh::SecretKey::generate().public();
        assert!(host.name_peer(
            room,
            &"ada".to_string(),
            &laptop.to_string(),
            "Ada Lovelace"
        ));
        assert!(host.certify_peer_identity(
            room,
            &laptop.to_string(),
            certified(
                *laptop.as_bytes(),
                "Studio laptop",
                tilo_user_identity::DeviceCapabilities::member()
            )
        ));
        let scribe = iroh::SecretKey::generate().public();
        let delegation = tilo_relay_protocol::delegation::Delegation::sign(
            signer,
            *scribe.as_bytes(),
            tilo_relay_protocol::delegation::Delegate::Agent {
                agent_id: "scribe".into(),
            },
            "Scribe",
            [10, 20, 30],
            tilo_relay_protocol::delegation::DelegateScope {
                rooms: vec![room.to_string()],
                apps: Vec::new(),
            },
            1,
            2,
        )
        .unwrap();
        host.room_peer_identities.entry(room).or_default().insert(
            scribe.to_string(),
            GovernedPeerIdentity {
                device_cert: certified(
                    *scribe.as_bytes(),
                    "Agent: Scribe",
                    tilo_user_identity::CertificationPolicy::agent().ceiling,
                ),
                revocations: Vec::new(),
                delegate: Some(tilo_peer_state::GovernedDelegateRow {
                    delegation,
                    withdrawn_at_ns: None,
                    installation: None,
                }),
            },
        );
        host.sync_room_doc(room);
        (host, room, *scribe.as_bytes())
    }

    /// The workspace's Activity, as the compositor refreshes it, over one
    /// record of the active room whose latest change `device` signed: the
    /// author of that change.
    fn activity_author_of_a_change_by(
        mut tree: WorkspaceTree,
        room: RoomId,
        device: [u8; 32],
    ) -> ui::widget::HistoryAuthor {
        struct Record(ui::record::RecordId, automerge::AutoCommit);
        impl tilo_desktop::activity::PersistedRecordHistory for Record {
            fn load_record(&self, id: &ui::record::RecordId) -> Option<automerge::AutoCommit> {
                (id == &self.0).then(|| self.1.clone())
            }
        }
        let record = ui::record::RecordId::for_file("scribe.qdoc");
        let kind = ui::RecordKind::new("document");
        tree.room_records_held
            .entry(room)
            .or_default()
            .insert(record.to_string(), kind.to_string());
        tree.commit_room_change();
        let mut document = automerge::AutoCommit::new();
        ui::record::init(&mut document, &record, &kind, &room.to_string());
        document.set_actor(automerge::ActorId::from(device.as_slice()));
        document
            .put(automerge::ROOT, "draft", "Written by an agent")
            .unwrap();
        tilo_resource::history::commit_stamped_with_message(&mut document, None);
        let handle = WorkspaceTreeHandle::new(tree);
        let mut pane =
            crate::shell::room_activity::RoomActivityPane::with_local_user("ada".into());
        pane.refresh_observed(
            &handle,
            &ui::record::RecordStore::in_memory(),
            &Record(record, document),
        );
        let short = ui::history_source::short_id(&hex::encode(device));
        pane.entries()
            .iter()
            .find(|entry| entry.author.identity_short == short)
            .map(|entry| entry.author.clone())
            .expect("the agent's change is in the workspace's history")
    }

    /// A change an agent's own key signed reads as the agent's, for its
    /// person, in the workspace's Activity and in what apps' own history is
    /// told, once the room's governance verified the person's delegation
    /// for exactly that device. A delegation another root signed names
    /// nobody: the change stays an unknown participant's.
    #[test]
    fn an_agents_own_key_change_is_named_only_once_its_delegation_verifies() {
        use tilo_action_broker::services::WorkspaceAuthority as _;
        let ada = ed25519_dalek::SigningKey::from_bytes(&[97; 32]);
        let forger = ed25519_dalek::SigningKey::from_bytes(&[98; 32]);

        let (host, room, scribe) = room_with_ada_and_scribe(&forger);
        assert!(host.verified_agents(room).is_empty());
        let authority = crate::shell::broker_services::NativeWorkspaceAuthority(
            WorkspaceTreeHandle::new(host.clone()),
        );
        assert!(authority.history_agents(&room.to_string()).is_empty());
        let author = activity_author_of_a_change_by(host, room, scribe);
        assert_eq!(author.name, ui::history_source::UNKNOWN_AUTHOR_NAME);
        assert_eq!(author.attribution, None);

        let (host, room, scribe) = room_with_ada_and_scribe(&ada);
        let verified = ui::history_source::RosterAgent {
            device: scribe,
            person: ada.verifying_key().to_bytes(),
            person_name: "Ada Lovelace".into(),
            agent_id: "scribe".into(),
            label: "Scribe".into(),
            color: [10, 20, 30],
        };
        assert_eq!(host.verified_agents(room), std::slice::from_ref(&verified));
        let authority = crate::shell::broker_services::NativeWorkspaceAuthority(
            WorkspaceTreeHandle::new(host.clone()),
        );
        assert_eq!(
            authority.history_agents(&room.to_string()),
            [verified],
            "apps' own history is told the same agents"
        );
        let author = activity_author_of_a_change_by(host, room, scribe);
        assert_eq!(author.name, "Scribe · Agent, for Ada Lovelace");
        assert_eq!(author.rgb(), (10, 20, 30), "the agent's presence colour");
        assert_eq!(
            author.attribution,
            Some(ui::widget::history::HistoryAttribution {
                actor: "agent:scribe".into(),
                kind: tilo_actions::ActorKind::Agent,
                actor_name: "Scribe".into(),
                on_behalf_of: Some(hex::encode(ada.verifying_key().to_bytes())),
                on_behalf_of_name: Some("Ada Lovelace".into()),
            })
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn room_governance_is_learned_from_the_inviter_and_never_rewritten_by_guests() {
        // Phase `a_guest_cannot_rewrite_the_governance_of_the_room_it_joined`.
        // A guest projects its own cache into a room it was invited to; the
        // owner and capability list on the **host** must stay exactly what
        // the host granted. Two orderings, since whether the host had
        // already projected decides whether the guest's write races on
        // actor-id sort order — 32 rounds so a hole can't get lucky once.
        {
            for host_projected_first in [false, true] {
                for round in 0..32 {
                    let (mut host, host_ws, room) = host_owning_a_room();
                    if host_projected_first {
                        host.sync_room_doc(room);
                    }
                    let claim = governance_claim_from(ROOM_GUEST, room);

                    host.apply_room_changes(room, &claim, &PeerOrigin::hub(2))
                        .expect("well-formed change blobs parse");

                    // The receiver's cache: who the host believes administers
                    // its own room, and what it believes is granted in it.
                    let ws = host
                        .desktop
                        .workspaces
                        .iter()
                        .find(|w| w.id == host_ws)
                        .expect("the host keeps its workspace");
                    assert_eq!(
                        ws.owner().map(String::as_str),
                        Some(ROOM_OWNER),
                        "round {round} (host projected first: {host_projected_first}): \
                         a guest's projection took ownership of the host's room",
                    );
                    assert!(
                        ws.may_administer(&ROOM_OWNER.to_string()),
                        "round {round}: the owner must still administer its own room",
                    );
                    assert!(
                        !ws.may_administer(&ROOM_GUEST.to_string()),
                        "round {round}: the guest must not have become an \
                         administrator of the host's room",
                    );
                    let cap_ids: Vec<&str> = ws.sandbox.iter().map(|c| c.id.as_str()).collect();
                    assert!(
                        cap_ids.contains(&"owners-own-grant"),
                        "round {round}: the owner's own grant was replaced by the \
                         guest's list; held {cap_ids:?}",
                    );
                    assert!(
                        !cap_ids.contains(&"rogue-grant"),
                        "round {round}: a grant the guest wrote entered the host's \
                         capability list; held {cap_ids:?}",
                    );

                    // And the same read off the receiver's own copy of the
                    // replicated document, which is what it will hand to the
                    // next peer that asks.
                    let (doc_owner, doc_caps) = governance_in_room_doc(&host, room);
                    assert_ne!(
                        doc_owner.as_deref(),
                        Some(ROOM_GUEST),
                        "round {round}: the host's own room document records the \
                         guest as owner",
                    );
                    assert!(
                        !doc_caps.iter().any(|c| c == "rogue-grant"),
                        "round {round}: the guest's grant is in the host's own room \
                         document; it holds {doc_caps:?}",
                    );
                }
            }
        }

        // Phase `one_guest_cannot_rewrite_the_governance_another_guest_learned`.
        // The same forgery between two guests — neither administers the
        // room, so the receiver must know *which* peer governance comes
        // from and refuse the rest.
        {
            for round in 0..32 {
                let (mut host, _host_ws, room) = host_owning_a_room();
                let legitimate = host
                    .export_room_changes_since(room, &[])
                    .expect("the host is in its own room");

                // Carol joins and learns the room from the host it was
                // invited by. `PeerOrigin::hub(1)` stands for that host.
                let mut carol = WorkspaceTree::default();
                let carol_ws = carol.append_empty_workspace("Mine");
                assert!(carol.set_room(carol_ws, room), "carol joins the room");
                carol
                    .room_authority
                    .insert(room, RoomAuthority::Peer(PeerOrigin::hub(1)));
                carol
                    .apply_room_changes(room, &legitimate, &PeerOrigin::hub(1))
                    .expect("the host's document parses");
                assert_eq!(
                    carol
                        .desktop
                        .workspaces
                        .iter()
                        .find(|w| w.id == carol_ws)
                        .and_then(|w| w.owner().cloned())
                        .as_deref(),
                    Some(ROOM_OWNER),
                    "round {round}: carol must learn the room's owner from the \
                     peer that invited her — without that the refusal below \
                     would be vacuous",
                );

                // Bob, in the same room, claims it.
                let claim = governance_claim_from(ROOM_GUEST, room);
                carol
                    .apply_room_changes(room, &claim, &PeerOrigin::hub(2))
                    .expect("well-formed change blobs parse");

                let ws = carol
                    .desktop
                    .workspaces
                    .iter()
                    .find(|w| w.id == carol_ws)
                    .expect("carol keeps her workspace");
                assert_eq!(
                    ws.owner().map(String::as_str),
                    Some(ROOM_OWNER),
                    "round {round}: a guest rewrote another guest's copy of the \
                     room's owner",
                );
                let cap_ids: Vec<&str> = ws.sandbox.iter().map(|c| c.id.as_str()).collect();
                assert!(
                    !cap_ids.contains(&"rogue-grant"),
                    "round {round}: a guest's grant entered another guest's \
                     capability list; held {cap_ids:?}",
                );
                let (doc_owner, doc_caps) = governance_in_room_doc(&carol, room);
                assert_eq!(
                    doc_owner.as_deref(),
                    Some(ROOM_OWNER),
                    "round {round}: carol's own room document records the wrong \
                     owner",
                );
                assert!(
                    !doc_caps.iter().any(|c| c == "rogue-grant"),
                    "round {round}: carol's own room document carries the rogue \
                     grant; it holds {doc_caps:?}",
                );
            }
        }

        // Phase `a_guest_learns_the_governance_of_the_room_that_invited_it`.
        // The other half: a guest joining a room **must** learn its owner
        // and capabilities from the peer that admitted it — refusing every
        // change would close collaboration along with the hole.
        {
            let (mut host, _host_ws, room) = host_owning_a_room();
            let from_host = host
                .export_room_changes_since(room, &[])
                .expect("the host is in its own room");

            let mut guest = WorkspaceTree::default();
            let guest_ws = guest.append_empty_workspace("Mine");
            assert!(guest.set_room(guest_ws, room), "the guest joins the room");
            guest
                .room_authority
                .insert(room, RoomAuthority::Peer(PeerOrigin::hub(1)));

            let changed = guest
                .apply_room_changes(room, &from_host, &PeerOrigin::hub(1))
                .expect("the host's document parses");
            assert!(changed, "the round must change the guest's copy");

            let ws = guest
                .desktop
                .workspaces
                .iter()
                .find(|w| w.id == guest_ws)
                .expect("the guest keeps its workspace");
            assert_eq!(
                ws.owner().map(String::as_str),
                Some(ROOM_OWNER),
                "the guest must learn who administers the room it joined",
            );
            assert!(
                ws.sandbox.iter().any(|c| c.id == "owners-own-grant"),
                "the guest must learn the room's capabilities; it holds {:?}",
                ws.sandbox.iter().map(|c| &c.id).collect::<Vec<_>>(),
            );
            assert!(
                !ws.may_administer(&ROOM_GUEST.to_string()),
                "and joining a room must not make the joiner its administrator",
            );
        }

        // Phase `a_machine_cannot_grant_in_a_room_it_does_not_administer`.
        // The local grant path has to agree with the replication path: a
        // grant in a room this machine doesn't administer would be
        // honoured by its own sandbox while the room never heard of it —
        // a local escalation dressed as a grant.
        {
            let (_host, _host_ws, room) = host_owning_a_room();

            let mut guest = WorkspaceTree::default();
            let guest_ws = guest.append_empty_workspace("Mine");
            assert!(guest.set_room(guest_ws, room), "the guest joins the room");
            // A cache that still names the local user as owner — what an
            // upgrade from a state file written before joining looks like. The
            // ownership check alone would say yes here.
            guest
                .desktop
                .workspaces
                .iter_mut()
                .find(|w| w.id == guest_ws)
                .expect("the guest's workspace")
                .set_owner(ROOM_GUEST.to_string());
            assert!(
                guest
                    .desktop
                    .workspaces
                    .iter()
                    .find(|w| w.id == guest_ws)
                    .expect("the guest's workspace")
                    .may_administer(&ROOM_GUEST.to_string()),
                "precondition: the cache says the guest administers it, so the \
                 refusal below is the room's write authority and not ownership",
            );

            let granted = guest.grant_sandbox_in_workspace(
                guest_ws,
                &ROOM_GUEST.to_string(),
                CapOverride::grant(
                    Subject::app("rogue"),
                    Scope::FsPath(ROGUE_GRANT_PATH.to_string()),
                    Access::Write,
                ),
            );
            assert!(
                granted.is_none(),
                "a grant in a room administered elsewhere must be refused",
            );
            assert!(
                !guest
                    .desktop
                    .workspaces
                    .iter()
                    .find(|w| w.id == guest_ws)
                    .expect("the guest's workspace")
                    .sandbox
                    .iter()
                    .any(|c| matches!(&c.scope, Scope::FsPath(p) if p == ROGUE_GRANT_PATH)),
                "and must record nothing",
            );

            // The control: the same call in a room this machine minted is
            // allowed, so the refusal above is about the room and not the call.
            let mut own = WorkspaceTree::default();
            let own_ws = own.append_empty_workspace("Mine");
            own.adopt_ownerless_workspaces(&ROOM_GUEST.to_string());
            assert!(
                own.grant_sandbox_in_workspace(
                    own_ws,
                    &ROOM_GUEST.to_string(),
                    CapOverride::grant(
                        Subject::app("files"),
                        Scope::FsPath(OWNER_GRANT_PATH.to_string()),
                        Access::Read,
                    ),
                )
                .is_some(),
                "granting in a room this machine administers must still work",
            );
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
        let ws = Workspace {
            id: ws_id,
            name: "Test".into(),
            strips: vec![strip],
            ..Workspace::new(ws_id, String::new())
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
            };
            tree.desktop.workspaces[0].strips[0].window_groups[0].tabs = vec![
                make_tab(10, "Notes"),
                make_tab(11, "Files"),
                make_tab(12, "Terminal"),
            ];
            tree.desktop.workspaces[0].strips[0].window_groups[0].active_tab = 10;
            tree.seed_doc_from_cache();

            let topology = vec![
                ui::widget::GroupTopology {
                    id: source_group,
                    tab_ids: vec![10, 12],
                    active_tab: 10,
                    width: ui::widget::WindowWidth::Fraction(700),
                },
                ui::widget::GroupTopology {
                    id: 99,
                    tab_ids: vec![11],
                    active_tab: 11,
                    width: ui::widget::WindowWidth::Normal,
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
            });
            group.active_tab = before;
            let invalid = [ui::widget::GroupTopology {
                id: ids[0],
                tab_ids: vec![],
                active_tab: before,
                width: ui::widget::WindowWidth::Normal,
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

        let second = tree.append_empty_workspace("Project");
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
#[path = "state/media_policy_tests.rs"]
mod shell_media_policy;

#[cfg(test)]
#[path = "state/apply_room_changes_tests.rs"]
mod apply_room_changes_tests;

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
        let ws = Workspace {
            id: ws_id,
            name: "Active".into(),
            strips: vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups: vec![],
            }],
            collaborators: collabs,
            ..Workspace::new(ws_id, String::new())
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
mod next_id_seed_tests {
    //! [`WorkspaceTree::seed_next_id_from_self`] must advance the
    //! counter past a loaded tree's highest id. The counter is
    //! process-wide, so tests observe `>= max+1`, not an exact value.
    use super::*;

    /// Restores the process-wide peer salt at scope-exit, unconditionally
    /// (RAII), so a test that flips it doesn't leak into others.
    struct SaltGuard(u16);
    impl SaltGuard {
        fn new() -> Self {
            Self(NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed))
        }
    }
    impl Drop for SaltGuard {
        fn drop(&mut self) {
            NEXT_ID_PEER_SALT.store(self.0, AtomicOrdering::Relaxed);
        }
    }

    #[test]
    #[serial_test::serial(next_id_statics)]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn next_ids_are_seeded_above_use_and_partitioned_by_peer_salt() {
        // Phase `next_id_is_seeded_above_every_used_id`.
        {
            // Phase `next_id_seeds_above_max_used_id_on_load`.
            {
                let _guard = SaltGuard::new();
                NEXT_ID_PEER_SALT.store(0, AtomicOrdering::Relaxed);
                let mut tree = WorkspaceTree::default();
                tree.desktop
                    .workspaces
                    .push(Workspace::new(WorkspaceId::from(1), "First"));
                tree.desktop
                    .workspaces
                    .push(Workspace::new(WorkspaceId::from(50), "Fiftieth"));

                tree.seed_next_id_from_self();

                // Mint a fresh id — it must be strictly greater than every
                // persisted id, including the gap-jumping `50`.
                let allocated = next_id();
                assert!(
                    allocated > 50,
                    "after seeding, next_id() must skip past every persisted \
                     id (max was 50); got {allocated} which would collide with the \
                     existing tree (D-9 regression)"
                );
            }

            // Phase `seed_next_id_from_self_respects_peer_salt`.
            // `seed_next_id_from_self`: the counter advances based ONLY on ids
            // minted under the local salt — a remote peer's ids live in a
            // disjoint range and must not drag it forward.
            {
                let _guard = SaltGuard::new();
                NEXT_ID_PEER_SALT.store(0x1111, AtomicOrdering::Relaxed);
                NEXT_ID.store(1, AtomicOrdering::Relaxed);

                // A tree containing one id under the local salt (counter=5)
                // and a much larger id under a remote salt (counter=1000).
                let local_id_raw = ((0x1111u64) << PEER_ID_SALT_SHIFT) | 5;
                let remote_id_raw = ((0x2222u64) << PEER_ID_SALT_SHIFT) | 0x3E8;
                let mut tree = WorkspaceTree::default();
                tree.desktop
                    .workspaces
                    .push(Workspace::new(WorkspaceId::from(local_id_raw), "Local"));
                tree.desktop
                    .workspaces
                    .push(Workspace::new(WorkspaceId::from(remote_id_raw), "Remote"));

                tree.seed_next_id_from_self();

                // The counter must have advanced to 6 (= local max + 1), NOT
                // to 1001 — remote ids do not consume our counter space.
                let next = NEXT_ID.load(AtomicOrdering::Relaxed);
                assert_eq!(
                    next, 6,
                    "seed_next_id_from_self must only consider ids under the \
                     local salt; got counter={next} but the only local-salt id had \
                     counter=5 (remote-salt id counter=1000 is in a disjoint \
                     range and must be ignored)"
                );
            }
        }

        // Phase `peer_salt_partitions_ids_so_independent_peers_cannot_collide`.
        {
            // Phase `peer_salt_zero_default_keeps_old_ids`.
            // A salt of `0` reproduces the exact id shape every fixture that
            // expects `WorkspaceId(1)` to round-trip depends on.
            {
                let _guard = SaltGuard::new();
                // Reset both the salt and the counter so a parallel test that
                // bumped them doesn't leak.
                NEXT_ID_PEER_SALT.store(0, AtomicOrdering::Relaxed);
                NEXT_ID.store(1, AtomicOrdering::Relaxed);

                let id = next_id();
                assert_eq!(
                    id, 1,
                    "salt-0 default must mint the bare counter; got 0x{id:016x}"
                );
                assert_eq!(peer_salt_of(id), 0);
                assert_eq!(counter_of(id), 1);
            }

            // Phase `peer_salt_high_bits_are_the_salt`.
            // After `set_peer_salt(0xABCD)`, the top 16 bits of every minted
            // id must equal `0xABCD` — sanity check on the bit-packing.
            {
                let _guard = SaltGuard::new();
                let mut tree = WorkspaceTree::default();
                tree.set_peer_salt(0xABCD);

                // Reset the counter so the assertion below sees a small value
                // it can compare cleanly.
                NEXT_ID.store(1, AtomicOrdering::Relaxed);
                let id = next_id();

                assert_eq!(
                    peer_salt_of(id),
                    0xABCD,
                    "set_peer_salt(0xABCD) must stamp 0xABCD into the high 16 \
                     bits of next_id(); got id=0x{:016x} (salt=0x{:04x})",
                    id,
                    peer_salt_of(id)
                );
                // Counter portion must remain monotonic and small.
                assert_eq!(counter_of(id), 1);
            }

            // Phase `two_peers_with_independent_next_id_collide_under_sync`.
            // Two peers run independent `next_id` counters in parallel; with
            // distinct salts, their ids live in disjoint ranges so CRDT sync
            // surfaces both workspaces instead of folding them into one row.
            // They share a base doc before appending, so only the minted id
            // differs between the two `append` calls.
            {
                let _guard = SaltGuard::new();

                NEXT_ID_PEER_SALT.store(0x1111, AtomicOrdering::Relaxed);
                NEXT_ID.store(1, AtomicOrdering::Relaxed);
                let mut tree_a = WorkspaceTree::default();
                tree_a.seed_doc_from_cache();
                let base_changes = tree_a.export_changes_since(&[]);
                let mut tree_b = WorkspaceTree::default();
                tree_b.apply_changes(&base_changes).expect("seed B from A");
                // Snapshot the heads after the base sync so the post-fork
                // export only ships the *incremental* appends.
                let base_heads = tree_a.current_heads();

                // Peer B switches to its own salt and appends a workspace.
                NEXT_ID_PEER_SALT.store(0x2222, AtomicOrdering::Relaxed);
                NEXT_ID.store(1, AtomicOrdering::Relaxed);
                let id_b = tree_b.append_empty_workspace("PeerB");

                // Peer A appends concurrently under its own salt. Without the
                // salt, both peers would mint id `1` here and collide.
                NEXT_ID_PEER_SALT.store(0x1111, AtomicOrdering::Relaxed);
                NEXT_ID.store(1, AtomicOrdering::Relaxed);
                let id_a = tree_a.append_empty_workspace("PeerA");

                assert_ne!(
                    id_a,
                    id_b,
                    "two peers with distinct salts must mint distinct ids; \
                     got A=0x{:016x} B=0x{:016x} (C-1 regression — pre-patch \
                     both peers would have minted id 1)",
                    id_a.raw(),
                    id_b.raw()
                );
                assert_eq!(peer_salt_of(id_a.raw()), 0x1111);
                assert_eq!(peer_salt_of(id_b.raw()), 0x2222);

                // Sync the incremental appends in both directions and rebuild
                // each peer's cache from the merged doc.
                let changes_a = tree_a.export_changes_since(&base_heads);
                let changes_b = tree_b.export_changes_since(&base_heads);
                tree_b.apply_changes(&changes_a).expect("apply A→B");
                tree_a.apply_changes(&changes_b).expect("apply B→A");

                // Both peers must observe BOTH workspaces — no collision-fold.
                // A shared id would cause `rebuild_cache_from_doc` to merge the
                // two distinct rows into one (strips collide, names overwrite,
                // grants merge).
                for (label, tree) in [("A", &tree_a), ("B", &tree_b)] {
                    let has_a = tree.desktop.workspaces.iter().any(|w| w.id == id_a);
                    let has_b = tree.desktop.workspaces.iter().any(|w| w.id == id_b);
                    assert!(
                        has_a && has_b,
                        "peer {label} must see both workspaces after sync; \
                         has_a={has_a} has_b={has_b}, workspaces={:?}",
                        tree.desktop
                            .workspaces
                            .iter()
                            .map(|w| (w.id.raw(), &w.name))
                            .collect::<Vec<_>>()
                    );
                }
            }
        }
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
    fn ownership_changes_are_signed_durable_and_replay_proof() {
        // Phase `replacing_owner_removes_the_previous_owner`.
        {
            let mut ws = Workspace::new(WorkspaceId(0x5151), "Security review");
            ws.set_owner("alice");
            ws.set_owner("bob");

            assert_eq!(ws.owner().map(String::as_str), Some("bob"));
            assert_eq!(ws.role_of("bob"), Some(CapRole::Owner));
            assert_eq!(ws.role_of("alice"), None);
        }

        // Phase `ownership_transfer_requires_both_signatures_survives_restart_and_rejects_replay`.
        {
            let directory = tempfile::tempdir().unwrap();
            let state_path = directory.path().join("state.json");
            let mut tree = WorkspaceTree {
                persist_path: Some(state_path.clone()),
                ..WorkspaceTree::default()
            };
            let workspace_id = tree.append_empty_workspace("Security review");
            let (alice_key, alice_public) = collab_types::generate_keypair();
            let (bob_key, bob_public) = collab_types::generate_keypair();
            let (mallory_key, _) = collab_types::generate_keypair();
            let alice = public_key_user_id(alice_public);
            let bob = public_key_user_id(bob_public);
            tree.adopt_ownerless_workspaces(&alice);
            let room = tree.room_of(workspace_id).unwrap();
            tree.admit_in_workspace(workspace_id, &alice, &bob, tilo_action_policy::Role::Editor)
                .expect("owner may promote a collaborator");
            let overlong = collab_types::OwnershipTransferProposalV1::new(
                room.to_string(),
                [6; 32],
                alice_public,
                bob_public,
                604_901,
                &alice_key,
            )
            .unwrap();
            assert!(matches!(
                tree.propose_workspace_ownership_transfer(workspace_id, overlong, 100),
                Err(OwnershipTransferStateError::ProposalUnavailable)
            ));
            let proposal = collab_types::OwnershipTransferProposalV1::new(
                room.to_string(),
                [7; 32],
                alice_public,
                bob_public,
                200,
                &alice_key,
            )
            .unwrap();
            tree.propose_workspace_ownership_transfer(workspace_id, proposal.clone(), 100)
                .unwrap();
            // A restart ends the process that held the state file's single writer.
            drop(tree);
            let mut tree = crate::shell::persist::load_tree_from(&state_path).unwrap();
            tree.persist_path = Some(state_path);
            let wrong_acceptance =
                collab_types::OwnershipTransferAcceptanceV1::new(&proposal, 101, &mallory_key)
                    .unwrap();
            assert!(wrong_acceptance.verify(&proposal, 101).is_err());
            let acceptance =
                collab_types::OwnershipTransferAcceptanceV1::new(&proposal, 101, &bob_key).unwrap();
            let audit =
                collab_types::OwnershipTransferAuditV1::new(proposal, acceptance, 102).unwrap();
            assert!(tree.remove_from_workspace(workspace_id, &alice, &bob));
            assert!(matches!(
                tree.accept_workspace_ownership_transfer(workspace_id, audit.clone(), 102),
                Err(OwnershipTransferStateError::RecipientNotMember)
            ));
            tree.admit_in_workspace(workspace_id, &alice, &bob, tilo_action_policy::Role::Editor)
                .unwrap();
            tree.accept_workspace_ownership_transfer(workspace_id, audit.clone(), 102)
                .unwrap();
            assert!(matches!(
                tree.accept_workspace_ownership_transfer(workspace_id, audit, 102),
                Err(OwnershipTransferStateError::ProposalUnavailable)
            ));
            let ws = tree
                .desktop
                .workspaces
                .iter()
                .find(|ws| ws.id == workspace_id)
                .unwrap();
            assert_eq!(ws.owner(), Some(&bob));
            assert_eq!(ws.role_of(&alice), None);
            assert_eq!(ws.role_of(&bob), Some(CapRole::Owner));
            assert_eq!(ws.ownership_transfer_audit.len(), 1);
            assert!(ws.pending_ownership_transfer.is_none());
        }

        // Phase `cancelled_transfer_is_durable_and_cannot_be_replayed`.
        {
            let directory = tempfile::tempdir().unwrap();
            let state_path = directory.path().join("state.json");
            let mut tree = WorkspaceTree {
                persist_path: Some(state_path.clone()),
                ..WorkspaceTree::default()
            };
            let workspace_id = tree.append_empty_workspace("Security review");
            let (owner_key, owner_public) = collab_types::generate_keypair();
            let (recipient_key, recipient_public) = collab_types::generate_keypair();
            let owner = public_key_user_id(owner_public);
            let recipient = public_key_user_id(recipient_public);
            tree.adopt_ownerless_workspaces(&owner);
            let room = tree.room_of(workspace_id).unwrap();
            tree.admit_in_workspace(
                workspace_id,
                &owner,
                &recipient,
                tilo_action_policy::Role::Editor,
            )
            .unwrap();
            let proposal = collab_types::OwnershipTransferProposalV1::new(
                room.to_string(),
                [11; 32],
                owner_public,
                recipient_public,
                200,
                &owner_key,
            )
            .unwrap();
            tree.propose_workspace_ownership_transfer(workspace_id, proposal.clone(), 100)
                .unwrap();
            let cancellation =
                collab_types::OwnershipTransferCancellationV1::new(&proposal, 101, &owner_key)
                    .unwrap();
            let acceptance =
                collab_types::OwnershipTransferAcceptanceV1::new(&proposal, 101, &recipient_key)
                    .unwrap();
            let audit =
                collab_types::OwnershipTransferAuditV1::new(proposal.clone(), acceptance, 101)
                    .unwrap();
            tree.cancel_workspace_ownership_transfer(workspace_id, &cancellation)
                .unwrap();
            let mut restarted = crate::shell::persist::load_tree_from(&state_path).unwrap();
            restarted.persist_path = Some(state_path);
            assert!(matches!(
                restarted.propose_workspace_ownership_transfer(workspace_id, proposal, 102),
                Err(OwnershipTransferStateError::ProposalUnavailable)
            ));
            assert!(matches!(
                restarted.accept_workspace_ownership_transfer(workspace_id, audit, 102),
                Err(OwnershipTransferStateError::ProposalUnavailable)
            ));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn continuations_reconcile_safely_and_offers_stay_dormant_and_anonymous() {
        // Phase `continuation_reconcile_keeps_local_and_claimed_tabs`.
        {
            // Phase `continuation_reconcile_removes_a_closed_remote_tab_but_not_local_tabs`.
            {
                let mut tree = WorkspaceTree::bootstrap_default();
                let room = RoomId::new();
                tree.adopt_continuation_room(room, "Laptop");
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
                ));
                assert!(tree.continuation_tabs.contains(&0x99));
                assert!(tree.reconcile_continuation_tabs(&BTreeSet::new()));
                assert!(!tree.continuation_tabs.contains(&0x99));
            }

            // Phase `claimed_continuation_tab_becomes_local_and_survives_remote_reconciliation`.
            {
                let mut tree = WorkspaceTree::bootstrap_default();
                let room = RoomId::new();
                tree.adopt_continuation_room(room, "Laptop");
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
                );

                assert!(tree.claim_continuation_tab(0x9a));
                assert!(!tree.continuation_tabs.contains(&0x9a));
                assert!(!tree.continuation_rooms.contains(&room));
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

            // Phase `vanished_continuation_room_is_removed_but_claimed_room_is_not`.
            {
                let mut tree = WorkspaceTree::bootstrap_default();
                let offered = RoomId::new();
                let claimed = RoomId::new();
                tree.adopt_continuation_room(offered, "Laptop");
                tree.adopt_continuation_room(claimed, "Laptop");
                tree.continuation_rooms.remove(&claimed);

                assert!(tree.reconcile_continuation_rooms(&BTreeSet::new()));
                assert!(
                    !tree
                        .desktop
                        .workspaces
                        .iter()
                        .any(|workspace| workspace.room == offered)
                );
                assert!(
                    tree.desktop
                        .workspaces
                        .iter()
                        .any(|workspace| workspace.room == claimed)
                );
            }
        }

        // Phase `continuation_offers_stay_dormant_live_and_free_of_identifiers`.
        {
            // Phase `continuity_room_is_dormant_and_does_not_steal_focus`.
            {
                let mut tree = WorkspaceTree::bootstrap_default();
                let active_before = tree.desktop.active;
                let room = RoomId::new();

                assert!(tree.adopt_continuation_room(room, "Laptop"));
                assert!(!tree.adopt_continuation_room(room, "Laptop"));
                assert_eq!(tree.desktop.active, active_before);
                let adopted = tree
                    .desktop
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.room == room)
                    .expect("continuation workspace");
                assert_eq!(adopted.name, "From another device");
                assert!(!adopted.name.contains("Laptop"));
                assert!(adopted.strips.is_empty());
                assert_eq!(tree.room_authority(room), RoomAuthority::Unpinned);
            }

            // Phase `continuation_offer_tracks_live_placement_and_title`.
            {
                let mut tree = WorkspaceTree::bootstrap_default();
                let room = RoomId::new();
                tree.adopt_continuation_room(room, "Laptop");
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
                );
                tree.materialize_continuation_workspace(
                    "Laptop",
                    &crate::shell::personal_continuity::WorkspaceContinuation {
                        room,
                        apps: vec![make(1, "so.tilo.music")],
                    },
                );
                let workspace = tree
                    .desktop
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.room == room)
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
                tree.adopt_continuation_room(room, &source);
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
                ));
                let workspace = tree
                    .desktop
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.room == room)
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
        tree.adopt_continuation_room(room, "Laptop");
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
                apps: vec![app],
            },
        ));
        assert!(!tree.continuation_file_references.contains_key(&id.0));
        let replacement = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room == room)
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
    fn workspace_tasks_and_receipts_are_durable_audited_activity() {
        // Phase `workspace_task_is_durable_and_carries_action_provenance`.
        {
            let mut tree = WorkspaceTree::bootstrap_default();
            let workspace = tree.desktop.active;
            let id = tree
                .create_workspace_task(
                    workspace,
                    "Review the release brief",
                    "ada",
                    Some("review-agent".into()),
                    WorkspaceTaskRoute::RulesOnly,
                    Some(WorkspaceTaskResource {
                        app_id: Some("org.tilo.Writer".into()),
                        app_label: Some("Writer".into()),
                        id: "document:brief".into(),
                        kind: "document".into(),
                        label: "Release brief".into(),
                        agent_request: None,
                    }),
                )
                .expect("non-empty task is created");

            let task = tree
                .desktop
                .active_workspace()
                .unwrap()
                .tasks
                .first()
                .unwrap();
            assert_eq!(task.id, id);
            assert_eq!(task.route, WorkspaceTaskRoute::RulesOnly);
            let context = task.action_context();
            assert_eq!(
                context.group.as_ref().map(|group| group.kind.as_str()),
                Some("task")
            );
            assert_eq!(
                context
                    .resource
                    .as_ref()
                    .map(|resource| resource.label.as_str()),
                Some("Release brief")
            );

            tree.resync_cache_from_doc();
            assert_eq!(tree.desktop.active_workspace().unwrap().tasks.len(), 1);
            assert_eq!(
                tree.desktop.active_workspace().unwrap().tasks[0].title,
                "Review the release brief"
            );
        }

        // Phase `task_status_changes_append_a_truthful_audit_event`.
        {
            let mut tree = WorkspaceTree::bootstrap_default();
            let workspace = tree.desktop.active;
            let id = tree
                .create_workspace_task(
                    workspace,
                    "Publish notes",
                    "ada",
                    None,
                    WorkspaceTaskRoute::Manual,
                    None,
                )
                .unwrap();
            assert!(tree.set_workspace_task_status(
                workspace,
                &id,
                WorkspaceTaskStatus::InProgress,
                "ada"
            ));
            assert!(tree.set_workspace_task_status(
                workspace,
                &id,
                WorkspaceTaskStatus::Done,
                "ada"
            ));
            let task = tree
                .desktop
                .active_workspace()
                .unwrap()
                .tasks
                .first()
                .unwrap();
            assert_eq!(task.status, WorkspaceTaskStatus::Done);
            assert_eq!(task.audit.len(), 3);
            let event = task.audit.last().unwrap();
            assert_eq!(event.action(), "set-status");
            assert_eq!(
                &event.what,
                &tilo_actions::Did::verb(
                    tilo_actions::ActionKey::new(
                        tilo_desktop_model::workspace::WORKSPACE_GOVERNANCE_APP_ID,
                        tilo_desktop_model::TASK_KIND,
                        "set-status",
                    ),
                    [("status".to_owned(), tilo_actions::Scalar::from("done"))],
                )
            );
        }

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

#[path = "state/native_manual_tasks.rs"]
pub(crate) mod native_manual_tasks;

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
    }
}
