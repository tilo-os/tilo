//! Native owner groundwork for certified cursor captures.
//!
//! This is deliberately test-only until the shots driver can retain the actual
//! broker which admitted the invocation. An owner receipt is not a painted-task
//! origin: this fixture must never manufacture a `SemanticInteraction`, origin
//! UUID, certified-person context, or an accessible marker from that receipt.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use ed25519_dalek::SigningKey;
use tilo_joined_room::{Change, NativeAuthorityStorage, NativeWorkspaceOwner, Store};
use tilo_owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;
use tilo_peer_state::workspace_task::{
    WorkspaceTaskAuthority, WorkspaceTaskCommand, WorkspaceTaskFinish,
};
use tilo_user_identity::DeviceCert;

use crate::shell::state::{RoomId, WorkspaceId};

/// Nothing reaches a state file, the real identity service, or a relay. The
/// owner still publishes through its ordinary Store/installation boundary.
#[derive(Default)]
struct RamStore {
    history: Mutex<BTreeMap<[u8; 32], Vec<u8>>>,
    publication: Mutex<()>,
}

impl Store for RamStore {
    fn healthy(&self) -> bool {
        true
    }

    fn uncertain(&self, _room: RoomId) -> bool {
        false
    }

    fn slot_room(&self, _slot: WorkspaceId) -> Option<RoomId> {
        None
    }

    fn put_history(&self, bytes: &[u8]) -> Result<[u8; 32], String> {
        let hash = *blake3::hash(bytes).as_bytes();
        self.history
            .lock()
            .map_err(|_| "fixture history poisoned")?
            .insert(hash, bytes.to_vec());
        Ok(hash)
    }

    fn read_history(&self, hash: [u8; 32]) -> Result<Vec<u8>, String> {
        self.history
            .lock()
            .map_err(|_| "fixture history poisoned")?
            .get(&hash)
            .filter(|bytes| blake3::hash(bytes).as_bytes() == &hash)
            .cloned()
            .ok_or_else(|| "fixture history absent or corrupt".into())
    }

    fn publish(
        &self,
        _next: &NativeAuthorityStorage,
        _change: Change<'_>,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        let _publication = self
            .publication
            .lock()
            .map_err(|_| "fixture publication poisoned")?;
        install();
        Ok(())
    }
}

/// Genuine signed governor enrollment and person-approved task custody, with
/// generated fixture keys only. No `Debug`/serialization implementation may
/// disclose the private instruction or the device signing key.
struct NativeCursorOwner {
    owner: NativeWorkspaceOwner,
    room: RoomId,
    key: SigningKey,
    certificate: DeviceCert,
    task_a: String,
}

impl NativeCursorOwner {
    fn new() -> Result<Self, String> {
        let owner = NativeWorkspaceOwner::restore(
            Arc::new(RamStore::default()),
            NativeAuthorityStorage::default(),
            SystemTime::now(),
        )?;
        // Public, deterministic test seeds, never the current device's keys.
        let governor_root = SigningKey::from_bytes(&[91; 32]);
        let key = SigningKey::from_bytes(&[92; 32]);
        let certificate =
            tilo_joined_room::testing::certificate(&governor_root, &key, "Cursor fixture");
        tilo_joined_room::testing::enroll_interactive_governor(&owner, &key, &certificate)?;
        let room = RoomId::parse(tilo_joined_room::testing::ROOM).ok_or("invalid fixture room")?;
        owner.commit_workspace_membership_command(
            OwnerCommitRequest {
                room: room.to_string(),
                operation: CommitOperation::WorkspaceMembershipCommand {
                    invocation: uuid::Uuid::new_v4(),
                    caller: certificate.clone(),
                    command: WorkspaceMembershipCommand::AddAgent {
                        agent: "scribe".into(),
                        preset: tilo_action_policy::roles::AgentPreset::Observer,
                    },
                },
            },
            &certificate,
            &key,
            SystemTime::now(),
        )?;
        let mut fixture = Self {
            owner,
            room,
            key,
            certificate,
            task_a: String::new(),
        };
        fixture.task_a =
            fixture.queue("Prepare release notes", "Check the private release notes")?;
        fixture.approve(&fixture.task_a)?;
        Ok(fixture)
    }

    fn person(&self, command: WorkspaceTaskCommand) -> Result<(), String> {
        self.owner
            .commit_workspace_task_command_with_key(
                OwnerCommitRequest {
                    room: self.room.to_string(),
                    operation: CommitOperation::WorkspaceTaskCommand {
                        invocation: uuid::Uuid::new_v4(),
                        authority: WorkspaceTaskAuthority::Person {
                            caller: self.certificate.clone(),
                        },
                        command,
                    },
                },
                &self.key,
                SystemTime::now(),
            )
            .map(|_| ())
    }

    fn queue(&self, title: &str, instruction: &str) -> Result<String, String> {
        let before = self.owner.workspace_task_snapshot(self.room)?;
        self.person(WorkspaceTaskCommand::QueueInstruction {
            title: title.into(),
            instruction: instruction.into(),
            assigned_agent: "scribe".into(),
            route: tilo_desktop_model::WorkspaceTaskRoute::Model,
        })?;
        let after = self.owner.workspace_task_snapshot(self.room)?;
        let mut added = after
            .tasks
            .iter()
            .filter(|task| !before.tasks.iter().any(|prior| prior.id == task.id));
        let task = added.next().ok_or("queue did not create a task")?;
        if added.next().is_some() {
            return Err("queue created ambiguous tasks".into());
        }
        Ok(task.id.clone())
    }

    fn approve(&self, task: &str) -> Result<(), String> {
        self.person(WorkspaceTaskCommand::ApproveRun {
            task_id: task.into(),
            agent_id: "scribe".into(),
            expected_run_epoch: 0,
            after_revision: 0,
        })
    }

    /// Grant the registered Notes action's node through signed governance,
    /// not an injected answer or a synthetic authority context.
    fn allow_notes(&self) -> Result<(), String> {
        use tilo_action_policy::{Answer, Grant, KindSet, Subject, TreePath};
        let now = SystemTime::now();
        let messages = self.owner.with_current(self.room, |view| {
            let mut next = view.peer().clone();
            let node = tilo_actions::action_node(
                &TreePath::room(self.room.to_string()),
                "so.tilo.notes",
                "document",
                "notes:release",
                None,
                "append",
            );
            next.governance_mut().put_governed_grants(
                &[Grant::new(
                    "cursor-fixture-notes",
                    Subject::agent("scribe"),
                    node,
                    KindSet::UNDOABLE,
                    Answer::Allow,
                    Subject::person(hex::encode(self.certificate.user_public_key)),
                )],
                &self.key,
                now,
            )?;
            next.durable_mutation_messages(
                view.peer(),
                &self.key,
                now,
                view.generation()
                    .checked_add(1)
                    .ok_or("fixture generation overflow")?,
            )
        })?;
        for message in messages {
            self.owner.receive(
                self.room,
                tilo_peer_state::TransportOrigin::RetainedNativePublication {
                    publisher: self.certificate.device_public_key,
                },
                message,
                now,
            )?;
        }
        Ok(())
    }

    /// Prepare the same exact native requirement as the broker, deriving the
    /// starter, revision and private digest from the canonical owner. This is
    /// still only owner admission support, not registered broker paint proof.
    fn notes_requirement(
        &self,
    ) -> Result<tilo_joined_room::task_policy::NativeTaskActionRequirement, String> {
        use tilo_joined_room::task_policy::{
            NativeTaskActionRequirement, NativeTaskBinding, NativeTaskInvocation,
        };
        let snapshot = self.owner.workspace_task_snapshot(self.room)?;
        let control = snapshot
            .controls
            .get(&self.task_a)
            .ok_or("A control absent")?;
        let active = control.active_run.as_ref().ok_or("A ended")?;
        let run = NativeTaskBinding {
            task_id: self.task_a.clone(),
            agent_id: active.agent_id.clone(),
            epoch: active.epoch,
            revision: control
                .directions
                .last()
                .map_or(0, |direction| direction.revision),
            starter: Some(
                active
                    .approval
                    .as_ref()
                    .ok_or("A unapproved")?
                    .person
                    .clone(),
            ),
        };
        let contract = tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Undoable,
            ..Default::default()
        };
        let mut invocation = NativeTaskInvocation::new(
            uuid::Uuid::new_v4(),
            "so.tilo.notes",
            Some(r#"{"text":"Preserve references"}"#),
            &contract,
            "",
        )?;
        invocation.private_run_digest = self.owner.with_current(self.room, |view| {
            NativeWorkspaceOwner::current_private_run_digest(view, &run)
        })?;
        Ok(NativeTaskActionRequirement {
            binding: Some(run),
            app: "so.tilo.notes".into(),
            resource_kind: "document".into(),
            resource: "notes:release".into(),
            action: "append".into(),
            need: contract.need(),
            consequential: false,
            confirmation: None,
            invocation: Some(invocation),
        })
    }

    fn admit_notes<T>(
        &self,
        requirement: &tilo_joined_room::task_policy::NativeTaskActionRequirement,
        dispatch: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        use tilo_joined_room::task_admission::NativeInvocationBinding;
        let run = requirement.binding.as_ref().ok_or("task binding absent")?;
        let invocation = requirement.invocation.as_ref().ok_or("invocation absent")?;
        let exact = NativeInvocationBinding {
            room: self.room,
            task: run.task_id.clone(),
            agent: run.agent_id.clone(),
            epoch: run.epoch,
            revision: run.revision,
            invocation: invocation.id,
            request: invocation.request,
            host: self.certificate.device_public_key,
            app: requirement.app.clone(),
            provider: invocation.provider.clone(),
            resource: requirement.resource.clone(),
            action: requirement.action.clone(),
            arguments_digest: invocation.arguments_digest,
            contract_digest: invocation.contract_digest,
            ceiling_digest: requirement.ceiling_digest()?,
            private_run_digest: invocation.private_run_digest,
        };
        self.owner
            .admit_local_native_invocation(
                exact,
                &self.certificate,
                &self.key,
                |view, exact, now| view.check_native_task_action(requirement, exact, now),
                dispatch,
            )
            .map(|(result, _permit)| result)
    }

    fn revision_a(&self) -> Result<u64, String> {
        let snapshot = self.owner.workspace_task_snapshot(self.room)?;
        let control = snapshot
            .controls
            .get(&self.task_a)
            .ok_or("A control absent")?;
        Ok(control
            .directions
            .last()
            .map_or(0, |direction| direction.revision))
    }

    fn direct_a(&self, text: &str) -> Result<(), String> {
        self.person(WorkspaceTaskCommand::DirectionForRun {
            task_id: self.task_a.clone(),
            run_epoch: 1,
            after_revision: self.revision_a()?,
            text: text.into(),
        })
    }

    fn agent(&self, command: WorkspaceTaskCommand) -> Result<(), String> {
        self.owner
            .commit_workspace_task_command_with_key(
                OwnerCommitRequest {
                    room: self.room.to_string(),
                    operation: CommitOperation::WorkspaceTaskCommand {
                        invocation: uuid::Uuid::new_v4(),
                        authority: WorkspaceTaskAuthority::Agent {
                            attestor: self.certificate.clone(),
                            agent_id: "scribe".into(),
                        },
                        command,
                    },
                },
                &self.key,
                SystemTime::now(),
            )
            .map(|_| ())
    }

    fn complete_a_start_b(&self) -> Result<String, String> {
        self.agent(WorkspaceTaskCommand::AppliedAck {
            task_id: self.task_a.clone(),
            run_epoch: 1,
            revision: self.revision_a()?,
        })?;
        self.agent(WorkspaceTaskCommand::Finish {
            task_id: self.task_a.clone(),
            run_epoch: 1,
            outcome: WorkspaceTaskFinish::Done,
            summary: "Completed review".into(),
        })?;
        let task_b = self.queue(
            "Review another document",
            "Check a different private document",
        )?;
        self.approve(&task_b)?;
        Ok(task_b)
    }
}

fn cursor_private_instructions(
    fixture: &NativeCursorOwner,
    task: &str,
) -> Result<Option<tilo_joined_room::NativePrivateTaskInstructions>, String> {
    fixture.owner.workspace_task_private_instructions(
        fixture.room,
        task,
        1,
        &WorkspaceTaskAuthority::Person {
            caller: fixture.certificate.clone(),
        },
        SystemTime::now(),
    )
}

#[test]
fn native_cursor_owner_keeps_private_a_when_the_same_agent_starts_b() {
    let run = || -> Result<(), String> {
        let fixture = NativeCursorOwner::new()?;
        let initial = fixture.owner.workspace_task_snapshot(fixture.room)?;
        let authority_a = initial.controls[&fixture.task_a]
            .execution_authority
            .as_ref()
            .ok_or("approved A has no frozen authority")?;
        assert_eq!(
            authority_a.scope,
            tilo_action_policy::TreePath::room(fixture.room.to_string()),
            "a private instruction must not narrow the starter's room scope"
        );
        let authority_before =
            serde_json::to_value(authority_a).map_err(|error| error.to_string())?;
        fixture.allow_notes()?;
        let requirement = fixture.notes_requirement()?;
        let dispatched = std::cell::Cell::new(0);
        let dispatch = || {
            dispatched.set(dispatched.get() + 1);
            Ok(())
        };
        let mut forged = requirement.clone();
        forged
            .invocation
            .as_mut()
            .ok_or("invocation absent")?
            .private_run_digest = [0; 32];
        assert!(fixture.admit_notes(&forged, dispatch).is_err());
        assert_eq!(
            dispatched.get(),
            0,
            "bad private provenance reached dispatch"
        );
        fixture.admit_notes(&requirement, dispatch)?;
        assert_eq!(dispatched.get(), 1);
        fixture.direct_a("Keep the original references")?;
        // Fresh UUID/frontier: terminal refusal below cannot merely be a replay
        // refusal or an outdated-direction check.
        let unspent_a = fixture.notes_requirement()?;
        let private_a = || cursor_private_instructions(&fixture, &fixture.task_a);
        let before = private_a()?.ok_or("private A missing before completion")?;
        assert_eq!(
            before.instruction.as_deref(),
            Some("Check the private release notes")
        );
        let task_b = fixture.complete_a_start_b()?;
        assert_ne!(fixture.task_a, task_b);
        let snapshot = fixture.owner.workspace_task_snapshot(fixture.room)?;
        assert!(snapshot.controls[&fixture.task_a].active_run.is_none());
        assert_eq!(
            serde_json::to_value(
                snapshot.controls[&fixture.task_a]
                    .execution_authority
                    .as_ref()
            )
            .map_err(|error| error.to_string())?,
            authority_before,
            "completing A and starting B must not rewrite A's retained scope"
        );
        assert_eq!(
            snapshot.controls[&task_b]
                .active_run
                .as_ref()
                .ok_or("B has no approved run")?
                .agent_id,
            "scribe"
        );
        let after = private_a()?.ok_or("historical private A lost after B started")?;
        assert_eq!(before.instruction, after.instruction);
        assert_eq!(before.directions, after.directions);
        assert!(fixture.direct_a("Must not reach B").is_err());
        assert!(fixture.admit_notes(&requirement, dispatch).is_err());
        assert!(fixture.admit_notes(&unspent_a, dispatch).is_err());
        assert_eq!(
            dispatched.get(),
            1,
            "old A admission reached replacement work"
        );
        let private_b =
            cursor_private_instructions(&fixture, &task_b)?.ok_or("private B missing")?;
        assert!(private_b.directions.is_empty());
        Ok(())
    };
    if let Err(error) = run() {
        panic!("native cursor owner custody: {error}");
    }
}
