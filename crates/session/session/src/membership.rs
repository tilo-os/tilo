//! Changes to a workspace's people, written where the workspace keeps them.
//!
//! The workspace's settings change its people on this desk at once; the same
//! change goes into the workspace's own governance, signed with this device's
//! key, so every member admits it as it admits every change its people make
//! ([`NativeWorkspaceOwner::commit_workspace_membership_command`]): a person
//! added, a role set, someone removed, a grant made or taken away.

use crate::engine::Engine;
use tilo_action_policy::Change;
use tilo_desktop_model::grants::PolicyEdit;
use tilo_desktop_model::{RoomId, Workspace};
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

#[cfg(test)]
mod tests {
    //! The workspace's own governance takes the people the settings change,
    //! and refuses what it refuses, whatever this desk holds.

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

    /// This device's engine, holding the workspace it governs, and the desk's
    /// own view of it with the person in it as its owner. `joiner` joins the
    /// workspace too, as their own device, and comes back as the person the
    /// workspace knows them by.
    fn workspace_here(joiner: &str) -> (Engine, Workspace, String, tempfile::TempDir) {
        let person_root = SigningKey::from_bytes(&[9; 32]);
        let person_key = SigningKey::from_bytes(&[10; 32]);
        let cert = testing::certificate(&person_root, &person_key, "This device");
        let identity = HostIdentity {
            device_cert: cert.clone(),
            device_signing_key: person_key.clone(),
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let directory = tempfile::TempDir::new().expect("a state directory");
        let engine = Engine::open(identity, Some(directory.path()), std::iter::empty())
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
        let mut workspace = Workspace::in_room(WorkspaceId::of_room(room), room, "Test");
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
}
