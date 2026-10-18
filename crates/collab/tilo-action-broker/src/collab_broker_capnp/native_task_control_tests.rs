//! Exercise the real task subscription against durable owner-private mail.
use super::*;

// Delay the synchronous private-read service seam, not a task model or the
// stream server. All underlying reads and mutations still use the real owner.
struct DelayedControl {
    inner: Arc<Native>,
    gate: Mutex<Option<ControlGate>>,
    subscriptions: Mutex<HashMap<String, usize>>,
}
struct ControlGate {
    task: String,
    entered: tokio::sync::oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}
impl DelayedControl {
    fn fixture() -> (Fixture, Arc<Self>) {
        let mut adapter = None;
        let fixture = Fixture::with_task_execution(None, None, false, |inner| {
            let service = Arc::new(Self {
                inner,
                gate: Mutex::new(None),
                subscriptions: Mutex::new(HashMap::new()),
            });
            adapter = Some(service.clone());
            service
        });
        (fixture, adapter.unwrap())
    }
    fn delay(
        &self,
        task: &str,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (entered, observed) = tokio::sync::oneshot::channel();
        let (release, receiver) = std::sync::mpsc::channel();
        *self.gate.lock().unwrap() = Some(ControlGate {
            task: task.into(),
            entered,
            release: receiver,
        });
        (observed, release)
    }
    fn pause(&self, key: &str) -> Result<(), String> {
        let gate = {
            let mut gate = self.gate.lock().unwrap();
            if gate.as_ref().is_some_and(|gate| gate.task == key) {
                gate.take()
            } else {
                None
            }
        };
        if let Some(gate) = gate {
            gate.entered
                .send(())
                .map_err(|_| "control test receiver closed")?;
            gate.release
                .recv_timeout(Duration::from_secs(10))
                .map_err(|_| "control test transport release missing")?;
        }
        Ok(())
    }
}
impl WorkspaceTaskExecution for DelayedControl {
    fn requires_room(&self, room: &str) -> bool {
        WorkspaceTaskExecution::requires_room(self.inner.as_ref(), room)
    }
    fn snapshot(&self, room: &str) -> Result<WorkspaceTaskSnapshot, String> {
        let snapshot = WorkspaceTaskExecution::snapshot(self.inner.as_ref(), room)?;
        self.pause("governor-snapshot")?;
        Ok(snapshot)
    }
    fn agent_snapshot(&self, room: &str, agent: &str) -> Result<WorkspaceTaskSnapshot, String> {
        self.inner.agent_snapshot(room, agent)
    }
    fn private_instructions(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
    ) -> Result<Option<crate::services::NativeTaskPrivateInstructions>, String> {
        self.inner.private_instructions(room, binding)
    }
    fn private_confirmation(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
    ) -> Result<Option<tilo_desktop_model::WorkspaceTaskConfirmation>, String> {
        self.inner.private_confirmation(room, binding)
    }
    fn messages(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
    ) -> Result<Vec<tilo_actions::task_messaging::TaskMessage>, String> {
        let messages = self.inner.messages(room, binding)?;
        self.pause(&binding.task_id)?;
        Ok(messages)
    }
    fn message_changes_for_run(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
    ) -> Result<Option<watch::Receiver<()>>, String> {
        *self
            .subscriptions
            .lock()
            .unwrap()
            .entry(binding.task_id.clone())
            .or_default() += 1;
        self.inner.message_changes_for_run(room, binding)
    }
    fn report(
        &self,
        room: &str,
        binding: NativeTaskBinding,
        command: WorkspaceTaskCommand,
    ) -> CommitFuture {
        self.inner.report(room, binding, command)
    }
    fn authorize_action(
        &self,
        room: &str,
        agent: &str,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String> {
        self.inner.authorize_action(room, agent, requirement)
    }
    fn admit_host_launch(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        requirement: &NativeTaskActionRequirement,
        launch: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.inner
            .admit_host_launch(room, binding, requirement, launch)
    }
    fn prepare_admission(
        &self,
        room: String,
        binding: NativeTaskBinding,
        requirement: NativeTaskActionRequirement,
        caller: crate::services::NativeTaskAdmissionCaller,
    ) -> crate::services::NativeTaskAdmissionFuture {
        self.inner
            .prepare_admission(room, binding, requirement, caller)
    }
    fn admit(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        requirement: &NativeTaskActionRequirement,
        action: &mut dyn FnMut() -> Result<InvocationJob, String>,
    ) -> Result<InvocationJob, String> {
        self.inner.admit(room, binding, requirement, action)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn delivery_refresh_owner_read_does_not_block_subscription_or_stop() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (f, service) = DelayedControl::fixture();
            f.approve();
            let (entered, release) = service.delay("governor-snapshot");
            let channel = f.channel(100, true);
            let reply = tokio::time::timeout(
                Duration::from_secs(2),
                channel.subscribe_task_runs_request().send().promise,
            )
            .await
            .unwrap()
            .unwrap();
            let _stream = reply.get().unwrap().get_stream().unwrap();
            tokio::time::timeout(Duration::from_secs(2), entered)
                .await
                .unwrap()
                .unwrap();
            // The second callback runs while the first governor read is still held.
            let other = f.channel(101, true);
            let _other_reply = tokio::time::timeout(
                Duration::from_secs(2),
                other.subscribe_task_runs_request().send().promise,
            )
            .await
            .unwrap()
            .unwrap();
            f.person_command(WorkspaceTaskCommand::CancelForRun {
                task_id: f.task.clone(),
                run_epoch: 1,
                after_revision: 0,
            })
            .unwrap();
            assert_eq!(
                f.native
                    .owner
                    .workspace_task_snapshot(room())
                    .unwrap()
                    .tasks[0]
                    .status,
                WorkspaceTaskStatus::Cancelled
            );
            release.send(()).unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "keep both task streams, delayed owner response and Stop in one channel-driven regression"
)]
async fn blocked_private_control_keeps_stop_and_same_actor_other_task_live() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (f, service) = DelayedControl::fixture();
            f.approve();
            f.person_command(WorkspaceTaskCommand::QueueInstruction {
                title: "Independent review".into(),
                instruction: "Review the other document".into(),
                assigned_agent: "scribe".into(),
                route: tilo_desktop_model::WorkspaceTaskRoute::RulesOnly,
            })
            .unwrap();
            let other = f
                .native
                .owner
                .workspace_task_snapshot(room())
                .unwrap()
                .tasks
                .iter()
                .find(|task| task.id != f.task)
                .unwrap()
                .id
                .clone();
            f.person_command(WorkspaceTaskCommand::ApproveRun {
                task_id: other.clone(),
                agent_id: "scribe".into(),
                expected_run_epoch: 0,
                after_revision: 0,
            })
            .unwrap();
            let channel = f.channel(100, true);
            let (stream, first) = subscribed(&channel).await;
            let reply = stream.read_request().send().promise.await.unwrap();
            let second: runtime_collab::TaskRunEnvelope = serde_json::from_str(
                reply
                    .get()
                    .unwrap()
                    .get_task_json()
                    .unwrap()
                    .to_str()
                    .unwrap(),
            )
            .unwrap();
            report(&channel, &first, runtime_collab::TaskRunUpdate::Accepted)
                .await
                .unwrap();
            report(&channel, &second, runtime_collab::TaskRunUpdate::Accepted)
                .await
                .unwrap();
            mail(&f, 9910);
            let other_mail = mail_for(&f, &other, 9911);
            let (entered, release) = service.delay(&f.task);
            let pending = stream.read_control_request().send().promise;
            let reading = tokio::task::spawn_local(async move { pending.await });
            tokio::time::timeout(Duration::from_secs(2), entered)
                .await
                .unwrap()
                .unwrap();
            // Stop crosses the actual owner while Task A's private body is held.
            f.person_command(WorkspaceTaskCommand::CancelForRun {
                task_id: f.task.clone(),
                run_epoch: 1,
                after_revision: 0,
            })
            .unwrap();
            let reply = tokio::time::timeout(Duration::from_secs(2), reading)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let page: runtime_collab::TaskRunControl = serde_json::from_str(
                reply
                    .get()
                    .unwrap()
                    .get_control_json()
                    .unwrap()
                    .to_str()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(page.task_id, other);
            assert_eq!(page.messages, vec![other_mail]);
            // A separate production status RPC must also complete before release.
            let other_run = if first.task_id == other {
                &first
            } else {
                &second
            };
            let mut status = channel.task_run_cancelled_request();
            status.get().set_run_id(other_run.run_id);
            status.get().set_lease(&other_run.lease);
            let reply = tokio::time::timeout(Duration::from_secs(2), status.send().promise)
                .await
                .unwrap()
                .unwrap();
            assert!(!reply.get().unwrap().get_cancelled());
            let next_mail = mail_for(&f, &other, 9914);
            f.state.refresh_task_control();
            let refreshed = control(&stream).await;
            assert_eq!(refreshed.task_id, other);
            assert_eq!(refreshed.messages.last(), Some(&next_mail));
            assert_eq!(service.subscriptions.lock().unwrap().get(&f.task), Some(&1));
            assert_eq!(service.subscriptions.lock().unwrap().get(&other), Some(&1));
            release.send(()).unwrap();
            // A stopped run's captured private body must never be delivered.
            assert!(
                tokio::time::timeout(
                    Duration::from_secs(2),
                    stream.read_control_request().send().promise
                )
                .await
                .unwrap()
                .is_err()
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_read_is_bounded_and_disconnect_does_not_wait_for_private_control() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (f, service) = DelayedControl::fixture();
            f.approve();
            let channel = f.channel(100, true);
            let (stream, run) = subscribed(&channel).await;
            report(&channel, &run, runtime_collab::TaskRunUpdate::Accepted)
                .await
                .unwrap();
            mail(&f, 9912);
            let (entered, release) = service.delay(&f.task);
            let pending = stream.read_control_request().send().promise;
            let reading = tokio::task::spawn_local(async move { pending.await });
            tokio::time::timeout(Duration::from_secs(2), entered)
                .await
                .unwrap()
                .unwrap();
            let duplicate = tokio::time::timeout(
                Duration::from_secs(2),
                stream.read_control_request().send().promise,
            )
            .await
            .unwrap();
            let Err(error) = duplicate else {
                panic!("duplicate control read must be refused");
            };
            assert!(error.to_string().contains("one task control read"));
            f.state.task_subscribers.lock().unwrap().remove(&100);
            assert!(
                tokio::time::timeout(Duration::from_secs(2), reading)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_err()
            );
            release.send(()).unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn revocation_during_private_read_refuses_captured_body() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (f, service) = DelayedControl::fixture();
            f.approve();
            let channel = f.channel(100, true);
            let (stream, run) = subscribed(&channel).await;
            report(&channel, &run, runtime_collab::TaskRunUpdate::Accepted)
                .await
                .unwrap();
            mail(&f, 9913);
            let (entered, release) = service.delay(&f.task);
            let pending = stream.read_control_request().send().promise;
            let reading = tokio::task::spawn_local(async move { pending.await });
            tokio::time::timeout(Duration::from_secs(2), entered)
                .await
                .unwrap()
                .unwrap();
            f.remove_agent();
            release.send(()).unwrap();
            assert!(
                tokio::time::timeout(Duration::from_secs(2), reading)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_err()
            );
        })
        .await;
}

async fn subscribed(
    channel: &channel_handle::Client,
) -> (task_run_stream::Client, runtime_collab::TaskRunEnvelope) {
    let reply = channel
        .subscribe_task_runs_request()
        .send()
        .promise
        .await
        .unwrap();
    let stream = reply.get().unwrap().get_stream().unwrap();
    let reply = stream.read_request().send().promise.await.unwrap();
    let run = serde_json::from_str(
        reply
            .get()
            .unwrap()
            .get_task_json()
            .unwrap()
            .to_str()
            .unwrap(),
    )
    .unwrap();
    (stream, run)
}

fn mail(f: &Fixture, id: u128) -> tilo_actions::task_messaging::TaskMessage {
    mail_for(f, &f.task, id)
}

fn mail_for(f: &Fixture, task: &str, id: u128) -> tilo_actions::task_messaging::TaskMessage {
    use tilo_actions::task_messaging::{TaskMessageSend, TaskRunAddress};
    f.native
        .owner
        .send_task_message(
            room(),
            TaskRunAddress {
                task_id: task.into(),
                run_epoch: 1,
                agent_id: "scribe".into(),
            },
            task,
            uuid::Uuid::from_u128(id),
            TaskMessageSend {
                run_epoch: 1,
                text: "Private context, not an instruction".into(),
            },
            |view| {
                view.peer().workspace().check_task_execution_actor(
                    "scribe",
                    None,
                    std::time::SystemTime::now(),
                )
            },
        )
        .unwrap()
}

async fn control(stream: &task_run_stream::Client) -> runtime_collab::TaskRunControl {
    let reply = tokio::time::timeout(
        Duration::from_secs(2),
        stream.read_control_request().send().promise,
    )
    .await
    .unwrap()
    .unwrap();
    let value: runtime_collab::TaskRunControl = serde_json::from_str(
        reply
            .get()
            .unwrap()
            .get_control_json()
            .unwrap()
            .to_str()
            .unwrap(),
    )
    .unwrap();
    value.validate().unwrap();
    value
}

#[tokio::test(flavor = "current_thread")]
async fn private_control_waits_for_owner_acceptance_and_never_consumes_mail() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let channel = f.channel(100, true);
            let (stream, run) = subscribed(&channel).await;
            let message = mail(&f, 9901);
            f.state.refresh_task_control();
            let pending = stream.read_control_request().send().promise;
            tokio::pin!(pending);
            assert!(futures::poll!(&mut pending).is_pending());
            report(&channel, &run, runtime_collab::TaskRunUpdate::Accepted)
                .await
                .unwrap();
            let reply = tokio::time::timeout(Duration::from_secs(2), pending)
                .await
                .unwrap()
                .unwrap();
            let observed: runtime_collab::TaskRunControl = serde_json::from_str(
                reply
                    .get()
                    .unwrap()
                    .get_control_json()
                    .unwrap()
                    .to_str()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(observed.messages, vec![message.clone()]);
            assert!(observed.directions.is_empty());
            // The owner still offers the message after transport delivery.
            assert_eq!(
                observed_control(&channel, &run).await.messages,
                vec![message.clone()]
            );
            f.state.refresh_task_control();
            let pending = stream.read_control_request().send().promise;
            tokio::pin!(pending);
            assert!(futures::poll!(&mut pending).is_pending());
            let next = mail(&f, 9902);
            f.state.refresh_task_control();
            let reply = tokio::time::timeout(Duration::from_secs(2), pending)
                .await
                .unwrap()
                .unwrap();
            let observed: runtime_collab::TaskRunControl = serde_json::from_str(
                reply
                    .get()
                    .unwrap()
                    .get_control_json()
                    .unwrap()
                    .to_str()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(observed.messages, vec![message, next]);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn replaced_subscription_cannot_read_private_control() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let channel = f.channel(100, true);
            let (old, _) = subscribed(&channel).await;
            let (current, run) = subscribed(&channel).await;
            report(&channel, &run, runtime_collab::TaskRunUpdate::Accepted)
                .await
                .unwrap();
            let message = mail(&f, 9903);
            f.state.refresh_task_control();
            assert!(old.read_control_request().send().promise.await.is_err());
            assert_eq!(control(&current).await.messages, vec![message]);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn control_uses_owner_applied_frontier_without_acknowledging_directions() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let channel = f.channel(100, true);
            let (stream, run) = subscribed(&channel).await;
            report(&channel, &run, runtime_collab::TaskRunUpdate::Accepted)
                .await
                .unwrap();
            f.person_command(WorkspaceTaskCommand::DirectionForRun {
                task_id: f.task.clone(),
                run_epoch: 1,
                after_revision: 0,
                text: "Keep compatibility examples".into(),
            })
            .unwrap();
            f.state.refresh_task_control();
            let page = control(&stream).await;
            assert_eq!(page.applied_revision, 0);
            assert_eq!(page.directions[0].text, "Keep compatibility examples");
            assert_eq!(
                f.native
                    .owner
                    .workspace_task_snapshot(room())
                    .unwrap()
                    .controls[&f.task]
                    .active_run
                    .as_ref()
                    .unwrap()
                    .applied_direction_revision,
                0
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn superseded_private_body_is_reobserved_without_closing_the_control_stream() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (f, service) = DelayedControl::fixture();
            f.approve();
            let channel = f.channel(100, true);
            let (stream, run) = subscribed(&channel).await;
            report(&channel, &run, runtime_collab::TaskRunUpdate::Accepted)
                .await
                .unwrap();
            let message = mail(&f, 9910);
            let (entered, release) = service.delay(&f.task);
            f.state.refresh_task_control();
            let pending = stream.read_control_request().send().promise;
            let reading = tokio::task::spawn_local(async move { pending.await });
            tokio::time::timeout(Duration::from_secs(3), entered)
                .await
                .unwrap()
                .unwrap();
            assert!(
                !reading.is_finished(),
                "the private read did not remain held"
            );
            // The captured private body predates these real owner directions.
            f.person_command(WorkspaceTaskCommand::DirectionForRun {
                task_id: f.task.clone(),
                run_epoch: 1,
                after_revision: 0,
                text: "Preserve the newly approved scope".into(),
            })
            .unwrap();
            f.state.refresh_task_control();
            release.send(()).unwrap();
            let reply = tokio::time::timeout(Duration::from_secs(3), reading)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let page: runtime_collab::TaskRunControl = serde_json::from_str(
                reply
                    .get()
                    .unwrap()
                    .get_control_json()
                    .unwrap()
                    .to_str()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(page.latest_revision, 1);
            assert_eq!(page.directions[0].text, "Preserve the newly approved scope");
            assert_eq!(page.messages, vec![message]);
            assert_eq!(
                page.applied_revision, 0,
                "observation must not acknowledge directions"
            );
            let next = mail(&f, 9911);
            f.state.refresh_task_control();
            let next_page = control(&stream).await;
            assert!(
                next_page.messages.contains(&next),
                "the same stream closed after a superseded read"
            );
        })
        .await;
}
