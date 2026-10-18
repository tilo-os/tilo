//! Withhold the real owner's durable publication, not a task service result.
use super::*;
use tilo_joined_room::{Change, NativeAuthorityStorage, Store};

pub(super) struct TestOwnerWriter {
    inner: Arc<dyn Store>,
    gate: Mutex<Option<PublicationGate>>,
}
struct PublicationGate {
    entered: tokio::sync::oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}
// Unwinding an assertion must release the blocking owner worker too.
pub(super) struct PublicationRelease(Option<std::sync::mpsc::Sender<()>>);
impl PublicationRelease {
    pub(super) fn release(mut self) {
        self.0.take().unwrap().send(()).unwrap();
    }
}
impl Drop for PublicationRelease {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            let _ = release.send(());
        }
    }
}
impl TestOwnerWriter {
    pub(super) fn new(inner: Arc<dyn Store>) -> Self {
        Self {
            inner,
            gate: Mutex::new(None),
        }
    }
    pub(super) fn hold_next(&self) -> (tokio::sync::oneshot::Receiver<()>, PublicationRelease) {
        let (entered, observed) = tokio::sync::oneshot::channel();
        let (release, receiver) = std::sync::mpsc::channel();
        let previous = self.gate.lock().unwrap().replace(PublicationGate {
            entered,
            release: receiver,
        });
        assert!(previous.is_none(), "only one publication may be held");
        (observed, PublicationRelease(Some(release)))
    }
}
impl Store for TestOwnerWriter {
    fn put_history(&self, bytes: &[u8]) -> Result<[u8; 32], String> {
        self.inner.put_history(bytes)
    }
    fn read_history(&self, hash: [u8; 32]) -> Result<Vec<u8>, String> {
        self.inner.read_history(hash)
    }
    fn healthy(&self) -> bool {
        self.inner.healthy()
    }
    fn uncertain(&self, room: tilo_desktop_model::RoomId) -> bool {
        self.inner.uncertain(room)
    }
    fn slot_room(
        &self,
        slot: tilo_desktop_model::WorkspaceId,
    ) -> Option<tilo_desktop_model::RoomId> {
        self.inner.slot_room(slot)
    }
    fn publish(
        &self,
        next: &NativeAuthorityStorage,
        change: Change<'_>,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        let gate = self.gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.entered
                .send(())
                .map_err(|()| "publication observer closed".to_owned())?;
            gate.release
                .recv()
                .map_err(|_| "publication release closed".to_owned())?;
        }
        self.inner.publish(next, change, install)
    }
}

// Both runs are delivered through the actual subscription. Leave the second in
// its RPC stream: reading already-delivered work must not acquire the writer.
async fn two_runs(
    f: &Fixture,
) -> (
    channel_handle::Client,
    task_run_stream::Client,
    runtime_collab::TaskRunEnvelope,
    String,
) {
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
        .into_iter()
        .find(|task| task.id != f.task)
        .unwrap()
        .id;
    f.person_command(WorkspaceTaskCommand::ApproveRun {
        task_id: other,
        agent_id: "scribe".into(),
        expected_run_epoch: 0,
        after_revision: 0,
    })
    .unwrap();
    let channel = f.channel(100, true);
    let reply = channel
        .subscribe_task_runs_request()
        .send()
        .promise
        .await
        .unwrap();
    let stream = reply.get().unwrap().get_stream().unwrap();
    let first = read_run(&stream).await;
    let other = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let other = f
                .state
                .pending_task_runs
                .lock()
                .unwrap()
                .values()
                .find(|run| run.task_id != first.task_id && run.peer_id == 100)
                .map(|run| run.task_id.clone());
            if let Some(other) = other {
                break other;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("both approved tasks must have been offered before holding the writer");
    (channel, stream, first, other)
}
async fn read_run(stream: &task_run_stream::Client) -> runtime_collab::TaskRunEnvelope {
    let reply = tokio::time::timeout(Duration::from_secs(2), stream.read_request().send().promise)
        .await
        .expect("unrelated task delivery must not wait for durable publication")
        .unwrap();
    serde_json::from_str(
        reply
            .get()
            .unwrap()
            .get_task_json()
            .unwrap()
            .to_str()
            .unwrap(),
    )
    .unwrap()
}

async fn cancelled(
    channel: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
) -> bool {
    let mut status = channel.task_run_cancelled_request();
    status.get().set_run_id(run.run_id);
    status.get().set_lease(&run.lease);
    tokio::time::timeout(Duration::from_secs(2), status.send().promise)
        .await
        .expect("read-only control RPC must not wait for publication")
        .unwrap()
        .get()
        .unwrap()
        .get_cancelled()
}

#[tokio::test(flavor = "current_thread")]
async fn withheld_report_publication_keeps_unrelated_task_rpc_live() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let (channel, stream, run, other) = two_runs(&f).await;
            let (entered, release) = f.writer.hold_next();
            let reporting = report(&channel, &run, runtime_collab::TaskRunUpdate::Accepted);
            tokio::pin!(reporting);
            tokio::select! {
                result = &mut reporting => panic!("report completed before publication: {result:?}"),
                result = tokio::time::timeout(Duration::from_secs(2), entered) => {
                    result.unwrap().unwrap();
                }
            }
            assert!(futures::poll!(&mut reporting).is_pending());
            let unrelated = read_run(&stream).await;
            assert_eq!(unrelated.task_id, other);
            assert!(!cancelled(&channel, &unrelated).await);
            assert!(
                f.native.owner.workspace_task_snapshot(room()).unwrap().controls[&run.task_id]
                    .execution
                    .is_none(),
                "a held acceptance must not be visible before publication"
            );
            assert!(futures::poll!(&mut reporting).is_pending());
            release.release();
            tokio::time::timeout(Duration::from_secs(2), reporting)
                .await
                .unwrap()
                .unwrap();
            let snapshot = f.native.owner.workspace_task_snapshot(room()).unwrap();
            assert!(snapshot.controls[&run.task_id].execution.is_some());
            drop(f);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn withheld_action_publication_keeps_rpc_live_and_stop_honestly_pending() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let (channel, stream, run, other) = two_runs(&f).await;
            report(&channel, &run, runtime_collab::TaskRunUpdate::Accepted)
                .await
                .unwrap();
            let (_target, invocations) = notes_target(&f).await;
            grant_notes(&f);
            let (entered, release) = f.writer.hold_next();
            let invoking = invoke(&channel, &run, uuid::Uuid::new_v4());
            tokio::pin!(invoking);
            tokio::select! {
                result = &mut invoking => panic!("ACTION completed before publication: {result:?}"),
                result = tokio::time::timeout(Duration::from_secs(2), entered) => {
                    result.unwrap().unwrap();
                }
            }
            // Stop uses the actual signed owner command. It cannot become
            // durable while ACTION holds the owner's publication lock.
            let native = f.native.clone();
            let task_id = run.task_id.clone();
            let (started, stopping) = tokio::sync::oneshot::channel();
            let stop = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                native.owner.commit_workspace_task_command_with_key(
                    OwnerCommitRequest {
                        room: room().to_string(),
                        operation: CommitOperation::WorkspaceTaskCommand {
                            invocation: uuid::Uuid::new_v4(),
                            authority: WorkspaceTaskAuthority::Person {
                                caller: native.certificate.clone(),
                            },
                            command: WorkspaceTaskCommand::CancelForRun {
                                task_id,
                                run_epoch: 1,
                                after_revision: 0,
                            },
                        },
                    },
                    &native.key,
                    std::time::SystemTime::now(),
                )
            });
            tokio::time::timeout(Duration::from_secs(2), stopping)
                .await
                .unwrap()
                .unwrap();
            assert!(
                !stop.is_finished(),
                "Stop must not acknowledge an uncommitted cancel"
            );
            assert!(futures::poll!(&mut invoking).is_pending());
            let unrelated = read_run(&stream).await;
            assert_eq!(unrelated.task_id, other);
            assert!(!cancelled(&channel, &unrelated).await);
            assert!(!cancelled(&channel, &run).await);
            assert!(!stop.is_finished());
            // Neither the app invocation nor its durable receipt can be
            // delivered merely because preparation started on a worker.
            let delivery = invocations.read_request().send().promise;
            tokio::pin!(delivery);
            assert!(futures::poll!(&mut delivery).is_pending());
            release.release();
            // Keep polling the actual ACTION RPC: its LocalSet callback may
            // still be needed to relinquish custody before Stop can commit.
            // Cancellation may win the post-publication admission recheck.
            let (stopped, _action_result) = tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(stop, invoking)
            })
            .await
            .unwrap();
            stopped.unwrap().unwrap();
            let snapshot = f.native.owner.workspace_task_snapshot(room()).unwrap();
            assert_eq!(
                snapshot
                    .tasks
                    .iter()
                    .find(|task| task.id == run.task_id)
                    .unwrap()
                    .status,
                WorkspaceTaskStatus::Cancelled
            );
            let mut status = channel.task_run_cancelled_request();
            status.get().set_run_id(run.run_id);
            status.get().set_lease(&run.lease);
            assert!(
                tokio::time::timeout(Duration::from_secs(2), status.send().promise)
                    .await
                    .unwrap()
                    .unwrap()
                    .get()
                    .unwrap()
                    .get_cancelled()
            );
            drop(f);
        })
        .await;
}
