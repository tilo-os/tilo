use super::{Job, MAX_RESULTS, State};
use crate::shell::native_workspace_owner::NativeWorkspaceOwner;
use std::sync::{Arc, Mutex, mpsc};
use std::time::SystemTime;
use tilo_desktop::activity::{TaskReviewCommand, TaskReviewRequest, TaskReviewSnapshot};
use tilo_owner_commit::{CommitOperation, OwnerCommitRequest, OwnerCommitService};
use tilo_peer_state::workspace_task::{
    WorkspaceTaskAuthority, WorkspaceTaskCommand, WorkspaceTaskCommandId,
};

type Certificate =
    Arc<dyn Fn(&str) -> Result<tilo_user_identity::DeviceCert, String> + Send + Sync>;

pub(super) struct Worker {
    pub(super) state: Arc<Mutex<State>>,
    owner: NativeWorkspaceOwner,
    commit: Arc<dyn OwnerCommitService>,
    certificate: Certificate,
}

impl Worker {
    pub(super) fn new(owner: NativeWorkspaceOwner, commit: Arc<dyn OwnerCommitService>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            owner,
            commit,
            certificate: Arc::new(|room| {
                tilo_identity_host::broker::trusted_current_device(room)
                    .map(|(_, certificate)| certificate)
                    .map_err(|error| format!("selected task identity unavailable: {error}"))
            }),
        }
    }

    #[cfg(test)]
    pub(super) fn with_certificate(mut self, certificate: tilo_user_identity::DeviceCert) -> Self {
        self.certificate = Arc::new(move |_| Ok(certificate.clone()));
        self
    }

    #[cfg(test)]
    pub(super) fn with_certificate_resolver(
        mut self,
        resolve: impl Fn(&str) -> Result<tilo_user_identity::DeviceCert, String> + Send + Sync + 'static,
    ) -> Self {
        self.certificate = Arc::new(resolve);
        self
    }

    pub(super) fn run(self, receiver: mpsc::Receiver<Job>) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        for job in receiver {
            match job {
                Job::Snapshot(room) => self.refresh(&room),
                Job::Commit(request) => {
                    let result = runtime
                        .as_ref()
                        .map_err(std::string::ToString::to_string)
                        .and_then(|runtime| self.execute(runtime, &request));
                    self.refresh(&request.room);
                    if let Ok(mut state) = self.state.lock() {
                        state.results.insert(request.invocation, result);
                        state.completed.push_back(request.invocation);
                        while state.completed.len() > MAX_RESULTS {
                            if let Some(old) = state.completed.pop_front() {
                                state.results.remove(&old);
                                state.requests.remove(&old);
                            }
                        }
                    }
                }
            }
        }
    }

    pub(super) fn rooms(&self) -> std::collections::BTreeSet<String> {
        self.owner
            .rooms()
            .into_iter()
            .map(|room| room.to_string())
            .collect()
    }
    fn refresh(&self, room: &str) {
        let snapshot = self.read(room);
        let rooms = self.rooms();
        if let Ok(mut state) = self.state.lock() {
            // A native workspace cannot become an unguarded model merely
            // because its binding or credentials are no longer available.
            state.native_rooms.extend(rooms);
            state.loading.remove(room);
            state.snapshots.insert(room.to_owned(), snapshot);
            if state.snapshots.len() > 16 {
                let old = state
                    .snapshots
                    .keys()
                    .find(|key| key.as_str() != room)
                    .cloned();
                if let Some(old) = old {
                    state.snapshots.remove(&old);
                    state.queried.remove(&old);
                }
            }
        }
    }

    fn read(&self, room: &str) -> Result<std::sync::Arc<TaskReviewSnapshot>, String> {
        let certificate = (self.certificate)(room)?;
        self.read_with_certificate(room, &certificate)
    }

    fn read_with_certificate(
        &self,
        room: &str,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<std::sync::Arc<TaskReviewSnapshot>, String> {
        let id = crate::shell::state::RoomId::parse(room).ok_or("invalid task room")?;
        let now = SystemTime::now();
        let seconds = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_secs();
        self.owner.with_current(id, |view| {
            if view.device().device_public_key != certificate.device_public_key {
                return Err("selected identity differs from this enrolled workspace device".into());
            }
            let member = view
                .peer()
                .permissions(certificate.device_public_key, now)?;
            let room_node = member.policy.node();
            if !member.allows(&room_node, tilo_action_policy::ActionKind::Read, now) {
                return Err("task review read permission is unavailable".into());
            }
            let snapshot = tilo_peer_state::workspace_task::snapshot(
                view.peer()
                    .admitted_room()
                    .ok_or("admitted task document unavailable")?,
            )?;
            Ok(Arc::new(TaskReviewSnapshot {
                room: room.to_owned(),
                generation: view.generation(),
                sampled_at_secs: i64::try_from(seconds).unwrap_or(i64::MAX),
                writable: member.allows(&room_node, tilo_action_policy::ActionKind::Undoable, now),
                actor: member.person,
                tasks: snapshot.tasks,
                controls: snapshot.controls,
            }))
        })
    }

    fn execute(
        &self,
        runtime: &tokio::runtime::Runtime,
        request: &TaskReviewRequest,
    ) -> Result<(), String> {
        if let TaskReviewCommand::Inspect { task_id } = &request.command {
            let snapshot = self.read(&request.room)?;
            if snapshot.actor != request.observed_actor {
                return Err("selected person changed; review before inspecting this task".into());
            }
            return snapshot
                .tasks
                .iter()
                .any(|task| &task.id == task_id)
                .then_some(())
                .ok_or_else(|| "task no longer exists".into());
        }
        let caller = (self.certificate)(&request.room)?;
        let device = caller.device_public_key;
        // Bind the admission read and commit to the same selected certificate.
        // The owner rechecks membership and capabilities in its mutation turn.
        let snapshot = self.read_with_certificate(&request.room, &caller)?;
        if snapshot.actor != request.observed_actor {
            return Err("selected person changed; review before changing this task".into());
        }
        let command = match &request.command {
            TaskReviewCommand::Approve {
                task_id,
                agent_id,
                run_epoch,
                after_revision,
            } => WorkspaceTaskCommand::ApproveRun {
                task_id: task_id.clone(),
                agent_id: agent_id.clone(),
                expected_run_epoch: *run_epoch,
                after_revision: *after_revision,
            },
            TaskReviewCommand::Inspect { .. } => {
                return Err("inspection does not mutate task state".into());
            }
            TaskReviewCommand::Steer {
                task_id,
                run_epoch,
                after_revision,
                text,
            } => WorkspaceTaskCommand::DirectionForRun {
                task_id: task_id.clone(),
                run_epoch: *run_epoch,
                after_revision: *after_revision,
                text: text.clone(),
            },
            TaskReviewCommand::Cancel {
                task_id,
                run_epoch,
                after_revision,
            } => WorkspaceTaskCommand::CancelForRun {
                task_id: task_id.clone(),
                run_epoch: *run_epoch,
                after_revision: *after_revision,
            },
            TaskReviewCommand::Retry { task_id, run_epoch } => WorkspaceTaskCommand::Retry {
                task_id: task_id.clone(),
                run_epoch: *run_epoch,
            },
        };
        let bound = OwnerCommitRequest {
            room: request.room.clone(),
            operation: CommitOperation::WorkspaceTaskCommand {
                invocation: request.invocation,
                authority: WorkspaceTaskAuthority::Person { caller },
                command,
            },
        };
        let receipt = runtime.block_on(self.commit.commit(bound.clone()))?;
        receipt.validate_for(&bound)?;
        let room = crate::shell::state::RoomId::parse(&request.room).ok_or("invalid task room")?;
        self.owner
            .workspace_task_result(
                room,
                WorkspaceTaskCommandId {
                    device,
                    invocation: request.invocation,
                },
            )?
            .ok_or("task commit has no retained owner result")?;
        Ok(())
    }
}
