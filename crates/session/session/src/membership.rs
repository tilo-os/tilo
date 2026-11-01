//! Changes to a workspace's people, written where the workspace keeps them.
//!
//! The workspace's settings change its people on this desk at once; the same
//! change goes into the workspace's own governance, signed with this device's
//! key, so every member admits it as it admits every change its people make
//! ([`NativeWorkspaceOwner::commit_workspace_membership_command`]): a person
//! added, a role set, someone removed, a grant made or taken away.
//!
//! And back: who the workspace's governance here has in it is what the desk's
//! copy of the workspace shows, so a role another member set or a person they
//! took in reaches the People tab without that member's device being here.

use std::collections::BTreeMap;

use crate::engine::Engine;
use tilo_action_policy::{Change, Role};
use tilo_desktop_model::grants::PolicyEdit;
use tilo_desktop_model::{RoomId, UserId, Workspace};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;

/// One workspace's people, as this device writes them: the workspace's own
/// document, as the engine holds it here.
pub(crate) struct Membership<'a> {
    engine: &'a Engine,
    room: RoomId,
}

impl<'a> Membership<'a> {
    /// The people of the workspace `room` names.
    pub(crate) fn of(engine: &'a Engine, room: RoomId) -> Self {
        Self { engine, room }
    }

    /// The changes `edit` made to `workspace`, as the workspace's governance
    /// writes them: the people whose role it set or who it took in, those it
    /// removed, and the grants it made and took away. What the sandbox holds
    /// is the workspace's own, not part of who its people are.
    ///
    /// Who is already in the workspace is the workspace's roster here that
    /// says: naming a role for someone it does not have adds them, and one it
    /// has has their role set.
    pub(crate) fn changes(&self, workspace: &Workspace, edit: &PolicyEdit) -> Vec<Change> {
        let mut changes = Vec::new();
        for person in &edit.members {
            changes.push(match workspace.policy.members.get(person) {
                Some(role) if self.has(person) => Change::SetRole {
                    person: person.clone(),
                    role: *role,
                },
                Some(role) => Change::AddMember {
                    person: person.clone(),
                    role: *role,
                },
                None => Change::RemoveMember {
                    person: person.clone(),
                },
            });
        }
        for id in &edit.granted {
            if let Some(grant) = workspace.policy.grants.iter().find(|grant| &grant.id == id) {
                changes.push(Change::Grant(grant.clone()));
            }
        }
        for id in &edit.revoked {
            changes.push(Change::Revoke(id.clone()));
        }
        changes
    }

    /// Whether the workspace has `person` in it, as its own governance here
    /// says. A workspace this device does not hold has nobody here.
    fn has(&self, person: &str) -> bool {
        self.engine
            .people(&self.room.to_string())
            .is_ok_and(|people| {
                people
                    .iter()
                    .any(|held| held.person == person && !held.removed)
            })
    }

    /// Write `changes` into the workspace's governance as this device, each
    /// signed with its own key: the workspace's admission decides them, here
    /// and on every device that receives them, by the grants of the person
    /// acting. A change it refuses changes nothing there, and says why in the
    /// log.
    pub(crate) fn write(&self, changes: &[Change]) {
        for change in changes {
            if let Err(error) = self.commit(change) {
                tracing::debug!(
                    %error,
                    "a change to the workspace's people was not written into its governance"
                );
            }
        }
    }

    /// [`Self::write`] one change, saying why when the workspace refuses it
    /// or cannot publish it.
    pub(crate) fn commit(&self, change: &Change) -> Result<(), String> {
        let identity = self.engine.identity();
        let request = OwnerCommitRequest {
            workspace: self.room.to_string(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: identity.device_cert.clone(),
                command: WorkspaceMembershipCommand::Member {
                    change: change.clone(),
                },
            },
        };
        self.engine
            .owner()
            .commit_workspace_membership_command(
                request,
                &identity.device_signing_key,
                std::time::SystemTime::now(),
            )
            .map(|_receipt| ())
    }
}

impl crate::Shared {
    /// Who each workspace on the desk has in it, as the workspace's own
    /// governance here says: a role another member set, someone they took in
    /// or removed, written into the desk's copy of the workspace, which is
    /// what the People tab and every other list of the roster read. Nothing of
    /// a workspace's state reaches this device without the engine admitting
    /// it, so a revision that has not moved has nothing to show.
    pub(crate) fn mirror_membership(&self) {
        let Some(engine) = &self.engine else {
            return;
        };
        let revision = engine.owner().revision();
        if self.membership_fed.get() == Some(revision) {
            return;
        }
        self.membership_fed.set(Some(revision));
        let mut tree = self.handle.read(Clone::clone);
        let mut changed = false;
        for workspace in &mut tree.workspaces {
            let Ok(people) = engine.people(&workspace.room().to_string()) else {
                // A workspace this device holds no room of keeps the people
                // the desk has of it.
                continue;
            };
            changed |= mirror(workspace, &people);
        }
        if changed {
            self.handle.replace_tree(tree);
        }
    }
}

/// Write `people` into `workspace` as the roster the workspace's governance
/// names: the part each of them takes there, the owner aside, whom the desk's
/// policy names on its own. Whether anything changed.
fn mirror(workspace: &mut Workspace, people: &[crate::Person]) -> bool {
    let owner = workspace.policy.owner.clone();
    let members: BTreeMap<UserId, Role> = people
        .iter()
        .filter(|person| !person.removed)
        .filter(|person| owner.as_deref() != Some(person.person.as_str()))
        .map(|person| (person.person.clone(), Role::from(person.role)))
        .collect();
    if workspace.policy.members == members {
        return false;
    }
    workspace.policy.members = members;
    true
}

#[cfg(test)]
mod tests {
    //! The workspace's own governance takes the people the settings change,
    //! and refuses what it refuses, whatever this desk holds; and what it has
    //! of them is what the desk's copy shows.

    tilo_automerge_isolate::test_worker!();

    use std::collections::HashMap;

    use ed25519_dalek::SigningKey;
    use tilo_action_policy::Role;
    use tilo_desktop_model::WorkspaceId;
    use tilo_network_actor::HostIdentity;
    use tilo_peer_state::join::MemberRole;
    use tilo_peer_state::owner::testing::{self, governor::TestGovernor};

    use super::*;

    const ROOM: &str = testing::WORKSPACE;

    /// The identity of the device certified as `cert`, signing with `key`.
    fn identity(cert: &tilo_user_identity::DeviceCert, key: &SigningKey) -> HostIdentity {
        HostIdentity {
            device_cert: cert.clone(),
            device_signing_key: key.clone(),
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        }
    }

    /// This device's engine, holding the workspace it governs, and the desk's
    /// own view of it with the person in it as its owner. `joiner` joins the
    /// workspace too, as their own device, and comes back as the person the
    /// workspace knows them by.
    fn workspace_here(joiner: &str) -> (Engine, Workspace, String, tempfile::TempDir) {
        let person_root = SigningKey::from_bytes(&[9; 32]);
        let person_key = SigningKey::from_bytes(&[10; 32]);
        let cert = testing::certificate(&person_root, &person_key, "This device");
        let directory = tempfile::TempDir::new().expect("a state directory");
        let engine = Engine::open(
            identity(&cert, &person_key),
            Some(directory.path()),
            std::iter::empty(),
        )
        .expect("this device's engine opens");
        let governor = TestGovernor::empty_interactive(cert.clone(), person_key.clone(), ROOM)
            .expect("this device governs its workspace");
        let mut governor = testing::enroll_into(governor, engine.owner(), &person_key, &cert)
            .expect("this device's workspace is its own");
        // The person the settings change, as their own device of the same
        // workspace: a person the workspace has a device of is one it has.
        let her_root = SigningKey::from_bytes(&[21; 32]);
        let her_key = SigningKey::from_bytes(&[22; 32]);
        let her = testing::certificate(&her_root, &her_key, joiner);
        let joining = tempfile::TempDir::new().expect("her state directory");
        let hers = tilo_peer_state::owner::open_file(&joining.path().join("owner.json"))
            .expect("her owner opens");
        governor = testing::enroll_into(governor, &hers, &her_key, &her)
            .expect("the workspace admits her device");
        // What her admission changed reaches this device's copy of the
        // workspace, as it would over the workspace's own plane.
        let room = RoomId::parse(ROOM).expect("a workspace");
        let now = std::time::SystemTime::now();
        for (bytes, _) in governor.outgoing() {
            engine
                .owner()
                .receive(
                    room,
                    tilo_peer_state::TransportOrigin::RetainedRelayDelta(cert.device_public_key),
                    postcard::from_bytes(bytes).expect("a workspace change"),
                    now,
                )
                .expect("this device takes the workspace as its governor has it");
        }
        let mut workspace = Workspace::in_workspace(WorkspaceId::of_room(room), room, "Test");
        workspace
            .policy
            .members
            .insert(hex::encode(cert.user_public_key), Role::Owner);
        workspace
            .policy
            .members
            .insert(joiner.to_owned(), Role::Editor);
        (
            engine,
            workspace,
            hex::encode(her.user_public_key),
            directory,
        )
    }

    /// The role the workspace `ROOM` gives `person`, as its governance here
    /// has it.
    fn role_of(engine: &Engine, person: &str) -> Option<MemberRole> {
        engine
            .people(ROOM)
            .expect("the workspace's people")
            .into_iter()
            .find(|held| held.person == person && !held.removed)
            .map(|held| held.role)
    }

    /// Two devices of one workspace: the owner's, which signs for the
    /// workspace, and this desk's, both holding what the workspace admitted.
    struct TwoDesks {
        room: RoomId,
        owner: Engine,
        owner_cert: tilo_user_identity::DeviceCert,
        desk: Engine,
        desk_cert: tilo_user_identity::DeviceCert,
        person: String,
        now: std::time::SystemTime,
        _states: (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir),
    }

    /// The workspace's owner admits ada as their own device, and what that
    /// changed reaches both devices' copies of the workspace, as the
    /// workspace's host carries it.
    fn the_owner_admitted_ada() -> TwoDesks {
        let room = RoomId::parse(ROOM).expect("a workspace");
        let now = std::time::SystemTime::now();
        // The workspace's owner, whose own device signs for it.
        let a_root = SigningKey::from_bytes(&[51; 32]);
        let a_key = SigningKey::from_bytes(&[52; 32]);
        let a_cert = testing::certificate(&a_root, &a_key, "The owner's laptop");
        let governor = TestGovernor::empty_interactive(a_cert.clone(), a_key.clone(), ROOM)
            .expect("the workspace's owner governs it");
        let a_state = tempfile::TempDir::new().expect("a state directory");
        let a_engine = Engine::open(
            identity(&a_cert, &a_key),
            Some(a_state.path()),
            std::iter::empty(),
        )
        .expect("the owner's engine opens");
        let mut governor = testing::enroll_into(governor, a_engine.owner(), &a_key, &a_cert)
            .expect("the workspace is the owner's");
        // The person at this desk joins it as their own device, and so does
        // another member.
        let b_root = SigningKey::from_bytes(&[53; 32]);
        let b_key = SigningKey::from_bytes(&[54; 32]);
        let b_cert = testing::certificate(&b_root, &b_key, "This desk");
        let b_state = tempfile::TempDir::new().expect("this desk's state directory");
        let b_engine = Engine::open(
            identity(&b_cert, &b_key),
            Some(b_state.path()),
            [(WorkspaceId::of_room(room), room)],
        )
        .expect("this desk's engine opens");
        governor = testing::enroll_into(governor, b_engine.owner(), &b_key, &b_cert)
            .expect("the workspace admits this desk");
        let her_root = SigningKey::from_bytes(&[55; 32]);
        let her_key = SigningKey::from_bytes(&[56; 32]);
        let her = testing::certificate(&her_root, &her_key, "ada");
        let hers_at = tempfile::TempDir::new().expect("her state directory");
        let hers = tilo_peer_state::owner::open_file(&hers_at.path().join("owner.json"))
            .expect("her owner opens");
        governor = testing::enroll_into(governor, &hers, &her_key, &her)
            .expect("the workspace admits her device");
        let admitted: Vec<neighbour_wire::NeighbourMsg> = governor
            .outgoing()
            .map(|(bytes, _)| postcard::from_bytes(bytes).expect("a workspace change"))
            .collect();
        for engine in [&a_engine, &b_engine] {
            for message in &admitted {
                engine
                    .owner()
                    .receive(
                        room,
                        tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                            a_cert.device_public_key,
                        ),
                        message.clone(),
                        now,
                    )
                    .expect("a device takes the workspace as its host has it");
            }
        }
        TwoDesks {
            room,
            owner: a_engine,
            owner_cert: a_cert,
            desk: b_engine,
            desk_cert: b_cert,
            person: hex::encode(her.user_public_key),
            now,
            _states: (a_state, b_state, hers_at),
        }
    }

    /// What this desk holds of a workspace two devices share after the owner
    /// raised ada from their own device: the people the workspace has here,
    /// the owner, this desk's person, and ada, as the owner's change left her.
    fn the_owner_raised_ada() -> (RoomId, Vec<crate::Person>, String, String, String) {
        let desks = the_owner_admitted_ada();
        assert_eq!(
            role_of(&desks.desk, &desks.person),
            Some(MemberRole::Editor),
            "the workspace has her as an editor"
        );

        // The owner gives her moderation from their own device, and what they
        // wrote reaches this desk.
        Membership::of(&desks.owner, desks.room)
            .commit(&Change::SetRole {
                person: desks.person.clone(),
                role: Role::Moderator,
            })
            .expect("the workspace takes the change");
        let written = desks
            .owner
            .owner()
            .with_outgoing(desks.room, desks.now, |_, outgoing| {
                outgoing
                    .entries()
                    .map(|entry| {
                        postcard::from_bytes(entry.bytes()).map_err(|error| error.to_string())
                    })
                    .collect::<Result<Vec<neighbour_wire::NeighbourMsg>, String>>()
            })
            .expect("the owner's device published the change");
        assert!(
            !written.is_empty(),
            "the owner's device published something"
        );
        for message in written {
            desks
                .desk
                .owner()
                .receive(
                    desks.room,
                    tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                        desks.owner_cert.device_public_key,
                    ),
                    message,
                    desks.now,
                )
                .expect("this desk takes the change");
        }
        assert_eq!(
            role_of(&desks.desk, &desks.person),
            Some(MemberRole::Moderator),
            "this desk's workspace has her as a moderator"
        );
        let people = desks.desk.people(ROOM).expect("the workspace's people");
        (
            desks.room,
            people,
            hex::encode(desks.owner_cert.user_public_key),
            hex::encode(desks.desk_cert.user_public_key),
            desks.person,
        )
    }

    /// A role one member's device sets reaches another member's desk: what
    /// this desk has of the workspace's people — the roster the People tab
    /// shows — has her with it, though the device that set it is not here.
    #[test]
    fn a_role_another_member_sets_reaches_this_desks_people() {
        let (room, people, owner, desk, person) = the_owner_raised_ada();
        // What this desk had of the workspace's people before the change is
        // the owner's and this person's; after it, what the workspace has.
        let mut workspace = Workspace::in_workspace(WorkspaceId::of_room(room), room, "Test");
        workspace.set_owner(owner.clone());
        workspace.policy.members.insert(desk.clone(), Role::Editor);
        assert!(
            mirror(&mut workspace, &people),
            "the people the workspace has are written into this desk's copy"
        );
        assert_eq!(
            workspace.role_of(&person),
            Some(Role::Moderator),
            "the People tab shows her with the role the owner set"
        );
        assert_eq!(
            workspace.role_of(&desk),
            Some(Role::Editor),
            "and this person as the workspace has her"
        );
        assert!(
            !workspace.policy.members.contains_key(&owner),
            "the owner is the workspace's owner, not one more of its members"
        );
        assert!(
            !mirror(&mut workspace, &people),
            "a desk already showing them is not written again"
        );
    }

    /// A person named in the workspace's settings is given another role,
    /// removed, and named again: each is a change the workspace's own
    /// admission makes, so it reaches every member, not only this desk.
    #[test]
    fn the_workspace_takes_the_people_its_settings_change() {
        let (engine, mut workspace, person, _state) = workspace_here("ada");
        let membership = Membership::of(&engine, workspace.room());
        assert_eq!(
            role_of(&engine, &person),
            Some(MemberRole::Editor),
            "the workspace has her as an editor"
        );

        // The person gives her moderation.
        workspace
            .policy
            .members
            .insert(person.clone(), Role::Moderator);
        let set = membership.changes(
            &workspace,
            &PolicyEdit {
                members: vec![person.clone()],
                ..PolicyEdit::default()
            },
        );
        assert_eq!(
            set,
            vec![Change::SetRole {
                person: person.clone(),
                role: Role::Moderator
            }],
            "a person the workspace has has their role set"
        );
        membership
            .commit(&set[0])
            .expect("the workspace takes the change");
        assert_eq!(
            role_of(&engine, &person),
            Some(MemberRole::Moderator),
            "the workspace has made her a moderator"
        );

        workspace.policy.members.remove(&person);
        let removed = membership.changes(
            &workspace,
            &PolicyEdit {
                members: vec![person.clone()],
                ..PolicyEdit::default()
            },
        );
        assert_eq!(
            removed,
            vec![Change::RemoveMember {
                person: person.clone()
            }],
            "a person the settings drop is removed"
        );
        membership
            .commit(&removed[0])
            .expect("the workspace takes the removal");
        assert_eq!(
            role_of(&engine, &person),
            None,
            "the workspace has her no more"
        );

        // And the settings name her again, as a person the workspace does not
        // have: that takes her back in.
        workspace
            .policy
            .members
            .insert(person.clone(), Role::Editor);
        let added = membership.changes(
            &workspace,
            &PolicyEdit {
                members: vec![person.clone()],
                ..PolicyEdit::default()
            },
        );
        assert_eq!(
            added,
            vec![Change::AddMember {
                person: person.clone(),
                role: Role::Editor
            }],
            "a person the workspace does not have is added"
        );
        membership
            .commit(&added[0])
            .expect("the workspace takes her back");
        assert_eq!(
            role_of(&engine, &person),
            Some(MemberRole::Editor),
            "the workspace has her again"
        );
    }

    /// A change the workspace's admission refuses leaves its people as they
    /// were, whatever this desk's copy of them says.
    #[test]
    fn a_change_the_workspace_refuses_leaves_its_people_as_they_were() {
        let (engine, workspace, _, _state) = workspace_here("ada");
        let membership = Membership::of(&engine, workspace.room());
        // A person this desk's copy names and the workspace never took in:
        // a role for her is refused, because there is no membership of hers
        // to give a role.
        let stranger = hex::encode(SigningKey::from_bytes(&[31; 32]).verifying_key().to_bytes());
        assert!(
            membership
                .commit(&Change::SetRole {
                    person: stranger.clone(),
                    role: Role::Editor,
                })
                .is_err(),
            "the workspace refuses a role for someone it does not have"
        );
        assert_eq!(
            role_of(&engine, &stranger),
            None,
            "the workspace never had her"
        );
        let owner = workspace
            .policy
            .members
            .iter()
            .find(|(_, role)| **role == Role::Owner)
            .map(|(person, _)| person.clone())
            .expect("the workspace's owner");
        let before = role_of(&engine, &owner);
        assert!(
            membership
                .commit(&Change::RemoveMember {
                    person: owner.clone(),
                })
                .is_err(),
            "the workspace refuses to remove its owner"
        );
        assert_eq!(
            role_of(&engine, &owner),
            before,
            "the workspace's owner stays its owner"
        );
        assert!(
            before.is_some(),
            "the workspace had its owner to begin with"
        );
    }

    /// This computer's identity, kept under a root of the test's own, and the
    /// device certificate a session started here signs with.
    fn this_computer(
        env: &mut tilo_test_env::ScopedEnv,
    ) -> (
        tempfile::TempDir,
        SigningKey,
        tilo_user_identity::DeviceCert,
    ) {
        let root = tempfile::tempdir().expect("an identity root");
        let paths = tilo_user_identity::IdentityPaths::under(root.path());
        std::fs::create_dir_all(paths.device_privkey.parent().expect("the key's directory"))
            .expect("this computer's key directory");
        let person = SigningKey::from_bytes(&[0x61; 32]);
        let device = SigningKey::from_bytes(&[0x62; 32]);
        tilo_user_identity::persist::write_device_key(&paths.device_privkey, &device)
            .expect("this computer's device key");
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: person.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "The laptop".to_owned(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
        tilo_user_identity::persist::write_device_cert(&paths.device_cert, &cert)
            .expect("this computer's certificate");
        env.set("TILO_IDENTITY_ROOT", root.path());
        (root, device, cert)
    }

    /// The desk this computer keeps under `state`.
    fn session_keeping(state: &std::path::Path) -> crate::Session {
        crate::Session::headless(
            crate::Options {
                presentation: ui::headless::fixture_runner_inputs,
                collab: false,
                state: Some(state.to_path_buf()),
                ..crate::Options::default()
            },
            &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
        )
        .expect("a session")
    }

    /// A person another device of the workspace took in is on this desk's
    /// copy of it, in the part the workspace gives them, when the desk comes
    /// back: the People tab shows the workspace's people, not only those this
    /// desk put there.
    #[test]
    fn the_people_another_device_admits_are_on_this_desks_people() {
        let mut env = tilo_test_env::ScopedEnv::new();
        let (_identity, key, cert) = this_computer(&mut env);
        let state = tempfile::tempdir().expect("a state directory");
        // A frame brings the desk and its own workspace up.
        let room = {
            let mut session = session_keeping(state.path());
            session
                .step(std::time::Duration::from_millis(1))
                .expect("a frame");
            session
                .handle()
                .read(|tree| tree.workspaces.first().map(Workspace::room))
                .expect("a workspace on the desk")
        };
        // The workspace, as this computer's device governs it, takes ada in:
        // a change another member's device makes while this desk is away.
        let now = std::time::SystemTime::now();
        let her_root = SigningKey::from_bytes(&[0x63; 32]);
        let her_key = SigningKey::from_bytes(&[0x64; 32]);
        let her = testing::certificate(&her_root, &her_key, "ada");
        {
            let engine = Engine::open(
                identity(&cert, &key),
                Some(state.path()),
                [(WorkspaceId::of_room(room), room)],
            )
            .expect("this computer's engine opens");
            let governor =
                TestGovernor::empty_interactive(cert.clone(), key.clone(), &room.to_string())
                    .expect("this computer's device governs the workspace");
            let mut governor = testing::enroll_into(governor, engine.owner(), &key, &cert)
                .expect("the workspace is this computer's");
            let hers_at = tempfile::TempDir::new().expect("her state directory");
            let hers = tilo_peer_state::owner::open_file(&hers_at.path().join("owner.json"))
                .expect("her owner opens");
            governor = testing::enroll_into(governor, &hers, &her_key, &her)
                .expect("the workspace admits her device");
            for (bytes, _) in governor.outgoing() {
                engine
                    .owner()
                    .receive(
                        room,
                        tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                            cert.device_public_key,
                        ),
                        postcard::from_bytes(bytes).expect("a workspace change"),
                        now,
                    )
                    .expect("this computer's room takes the workspace's change");
            }
        }
        let person = hex::encode(her.user_public_key);
        // A new session of the same desk shows the workspace's people.
        let mut session = session_keeping(state.path());
        session
            .step(std::time::Duration::from_millis(1))
            .expect("a frame");
        let role = session
            .handle()
            .read(|tree| tree.workspaces.first().and_then(|ws| ws.role_of(&person)));
        assert_eq!(
            role,
            Some(Role::Editor),
            "the desk's workspace has the person the workspace took in"
        );
        // The sheet shows her in the People tab.
        session
            .invoke(
                tilo_desktop::desktop_actions::CHROME_RESOURCE,
                "workspace-settings",
                &serde_json::Map::new(),
            )
            .expect("settings opens on the workspace in front");
        session
            .step(std::time::Duration::from_millis(1))
            .expect("a frame");
        let row = session
            .workspace_settings()
            .expect("the session hosts workspace settings")
            .settings()
            .people
            .people
            .into_iter()
            .find(|listed| listed.key == person)
            .expect("the People tab lists her");
        assert_eq!(
            row.role_key.as_deref(),
            Some("editor"),
            "the People tab says which part the workspace gives her"
        );
    }

    /// An invitation this device's person makes into `room` from their own
    /// device: the workspace's record has it, and the link it signed.
    fn invite_from_this_device(
        state: &std::path::Path,
        room: RoomId,
        cert: &tilo_user_identity::DeviceCert,
        key: &SigningKey,
    ) -> tilo_peer_state::join::invitations::IssuedInvitation {
        use tilo_peer_state::join::invitations::IssueInvitationRequest;

        let engine = Engine::open(
            identity(cert, key),
            Some(state),
            [(WorkspaceId::of_room(room), room)],
        )
        .expect("this computer's engine opens");
        let now = std::time::SystemTime::now();
        let governor =
            TestGovernor::empty_interactive(cert.clone(), key.clone(), &room.to_string())
                .expect("this computer's device governs the workspace");
        let governor = testing::enroll_into(governor, engine.owner(), key, cert)
            .expect("the workspace is this computer's");
        for (bytes, _) in governor.outgoing() {
            engine
                .owner()
                .receive(
                    room,
                    tilo_peer_state::TransportOrigin::RetainedRelayDelta(cert.device_public_key),
                    postcard::from_bytes(bytes).expect("a workspace change"),
                    now,
                )
                .expect("this computer's room takes the workspace's change");
        }
        engine
            .owner()
            .issue_invitation(
                room,
                cert,
                IssueInvitationRequest {
                    request_id: [0x71; 32],
                    role: MemberRole::Editor,
                    recipient_user_public_key: None,
                    invitee_name: Some("Ada".into()),
                    expires_at_ns: tilo_clock::unix_now_ns()
                        .checked_add(60_000_000_000)
                        .expect("an expiry"),
                    inviter_name: None,
                },
                Vec::new(),
                key,
                now,
            )
            .expect("this device issues the link")
    }

    /// The invitations the desk's own action lists for the workspace `workspace`.
    fn listed_invitations(session: &mut crate::Session, workspace: &str) -> Vec<serde_json::Value> {
        let mut arguments = serde_json::Map::new();
        arguments.insert("workspace".into(), workspace.into());
        let listed = session
            .invoke(
                tilo_surface_host::shell_part::DESK_RESOURCE,
                "invitations",
                &arguments,
            )
            .expect("the desk lists the invitations it made")
            .completed()
            .expect("listing finishes on return")
            .value
            .expect("the listing's invitations");
        serde_json::from_value(listed).expect("a list of invitations")
    }

    /// Whether the desk's own action cancelling `invite` changed the workspace.
    fn cancelled_at_the_desk(session: &mut crate::Session, workspace: &str, invite: &str) -> bool {
        let mut arguments = serde_json::Map::new();
        arguments.insert("workspace".into(), workspace.into());
        arguments.insert("invite".into(), invite.into());
        let answer = session
            .invoke(
                tilo_surface_host::shell_part::DESK_RESOURCE,
                "cancel-invitation",
                &arguments,
            )
            .expect("the desk cancels the link this device made")
            .completed()
            .expect("cancelling finishes on return")
            .value
            .expect("whether the workspace changed");
        answer["cancelled"]
            .as_bool()
            .expect("whether the workspace changed")
    }

    /// An invitation this device's person made into a workspace is listed by
    /// the desk's own action and cancelled there, and the desk lists none
    /// afterwards: inviting and taking a link back need no host, only the
    /// device that made it.
    #[test]
    fn the_desk_lists_and_cancels_the_invitations_this_device_made() {
        let mut env = tilo_test_env::ScopedEnv::new();
        let (_identity, key, cert) = this_computer(&mut env);
        let state = tempfile::tempdir().expect("a state directory");
        let room = {
            let mut session = session_keeping(state.path());
            session
                .step(std::time::Duration::from_millis(1))
                .expect("a frame");
            session
                .handle()
                .read(|tree| tree.workspaces.first().map(Workspace::room))
                .expect("a workspace on the desk")
        };
        let invitation = invite_from_this_device(state.path(), room, &cert, &key);
        let mut session = session_keeping(state.path());
        session
            .step(std::time::Duration::from_millis(1))
            .expect("a frame");
        let workspace = session
            .handle()
            .read(|tree| {
                tree.workspaces
                    .first()
                    .map(|workspace| workspace.id.0.to_string())
            })
            .expect("a workspace on the desk");
        let listed = listed_invitations(&mut session, &workspace);
        assert_eq!(listed.len(), 1, "the one link this device made");
        assert_eq!(listed[0]["invite"], hex::encode(invitation.invite_id));
        assert_eq!(listed[0]["role"], "editor");
        assert_eq!(listed[0]["invitee_name"], "Ada");
        let invite = hex::encode(invitation.invite_id);
        assert!(
            cancelled_at_the_desk(&mut session, &workspace, &invite),
            "the link is cancelled"
        );
        assert!(
            !cancelled_at_the_desk(&mut session, &workspace, &invite),
            "cancelling it again changes nothing"
        );
        assert!(
            listed_invitations(&mut session, &workspace).is_empty(),
            "the cancelled link is not listed"
        );
    }
}
