//! Installed runtime over the real private file, its barriers and reopen.
//! The receiver below models only the existing durable dedup contract of the
//! broker journal (same UUID and intent returns the retained row, a changed
//! intent is refused); it is not the broker and grants no authority.
use super::*;
use crate::host_worker::managed_sender::{NativeSenderOutboxV1, attempt::RequestView};
use crate::host_worker::private_actions::*;
use crate::portable_storage::PortableStorage;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn caller() -> SenderCaller {
    SenderCaller::new([1; 32], [2; 32], "so.tilo.agent", "", Some("Scribe")).unwrap()
}
fn binding() -> PublishedInvocationBinding {
    PublishedInvocationBinding {
        host: [3; 16],
        peer_id: 7,
        app_id: "so.tilo.word-processor".into(),
        record_id: "brief".into(),
        root: [4; 32],
        device: [5; 32],
        target_binding: [6; 32],
    }
}
fn intent(action: &str, argument: &str) -> ChildIntent {
    ChildIntent {
        binding: binding(),
        revision: "original".into(),
        resource: "document".into(),
        action: action.into(),
        argument: Some(argument.into()),
    }
}
fn step(index: u32) -> ParentStep {
    ParentStep::new([9; 16], index).unwrap()
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

/// Reopen the same selected private file through the existing-only reader.
fn reopen(root: &Path) -> (SenderRuntime, PortableStorage) {
    let cache = PortableStorage::new("profile", "device").unwrap();
    let scope = SenderScope::native(uuid::Uuid::from_bytes([1; 16]), [2; 32], [3; 32]).unwrap();
    let file = PrivateActionsFile::from_held(
        tilo_files::native::atomic::StorageDirectory::open(root).unwrap(),
        scope,
        &cache,
    )
    .unwrap_or_else(|e| panic!("reopen {:?}", e.code()));
    let read = match file.read_existing() {
        PrivateReadOutcome::Committed(read) => read,
        PrivateReadOutcome::Failed(error) => panic!("read {:?}", error.code()),
    };
    let state = read
        .hydrate_current()
        .unwrap_or_else(|e| panic!("hydrate {:?}", e.code()));
    (SenderRuntime::install(state, caller()).unwrap(), cache)
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Row {
    child: Vec<u8>,
    intent: [u8; 32],
    result: Option<[u8; 32]>,
    json: Option<String>,
    continuation: Option<Vec<u8>>,
}
#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    rows: BTreeMap<u32, Row>,
    /// Count of fresh parent advances; an equal repeat never increments it.
    advances: u32,
}
/// File-backed parent owner: every change is written and renamed before the
/// call returns, and a reopen reads only that committed file.
struct DurableParent {
    path: PathBuf,
    ledger: Ledger,
    refuse_bind: bool,
    refuse_commit: bool,
    lose_commit_reply: bool,
}
impl DurableParent {
    fn open(path: &Path) -> Self {
        let ledger = std::fs::read(path)
            .map(|bytes| serde_json::from_slice(&bytes).unwrap())
            .unwrap_or_default();
        Self {
            path: path.into(),
            ledger,
            refuse_bind: false,
            refuse_commit: false,
            lose_commit_reply: false,
        }
    }
    fn persist(&self) {
        use std::io::Write;
        let temporary = self.path.with_extension("next");
        let mut file = std::fs::File::create(&temporary).unwrap();
        file.write_all(&serde_json::to_vec(&self.ledger).unwrap())
            .unwrap();
        file.sync_all().unwrap();
        std::fs::rename(&temporary, &self.path).unwrap();
    }
    fn record(&self, index: u32, row: &Row) -> ParentRecord {
        ParentRecord {
            step: step(index),
            child: ChildTicket::from_bytes(&row.child.clone().try_into().unwrap()).unwrap(),
            intent: row.intent,
            committed: row.result.map(|result| ParentCommitted {
                result,
                continuation: row.continuation.clone().unwrap(),
            }),
        }
    }
    fn continuation(&self, index: u32) -> String {
        String::from_utf8(self.ledger.rows[&index].continuation.clone().unwrap()).unwrap()
    }
}
impl ParentOwner for DurableParent {
    fn lookup(&self, wanted: ParentStep) -> Result<Option<ParentRecord>, ParentFailure> {
        assert_eq!(wanted.parent(), [9; 16]);
        Ok(self
            .ledger
            .rows
            .get(&wanted.index())
            .map(|row| self.record(wanted.index(), row)))
    }
    fn find_child(&self, child: ChildTicket) -> Result<Option<ParentRecord>, ParentFailure> {
        Ok(self
            .ledger
            .rows
            .iter()
            .find(|(_, row)| row.child == child.to_bytes())
            .map(|(index, row)| self.record(*index, row)))
    }
    fn bind(
        &mut self,
        wanted: ParentStep,
        child: ChildTicket,
        intent: [u8; 32],
    ) -> Result<ParentRecord, ParentFailure> {
        if self.refuse_bind {
            return Err(ParentFailure::Unavailable(
                "injected loss before bind".into(),
            ));
        }
        if let Some(row) = self.ledger.rows.get(&wanted.index()) {
            if row.child != child.to_bytes() || row.intent != intent {
                return Err(ParentFailure::Conflict);
            }
        } else {
            self.ledger.rows.insert(
                wanted.index(),
                Row {
                    child: child.to_bytes().to_vec(),
                    intent,
                    ..Row::default()
                },
            );
            self.persist();
        }
        Ok(self.lookup(wanted)?.unwrap())
    }
    fn commit(&mut self, commit: ChildCommit<'_>) -> Result<ParentReceipt, ParentFailure> {
        if self.refuse_commit {
            return Err(ParentFailure::Unavailable(
                "injected crash before commit".into(),
            ));
        }
        let row = self
            .ledger
            .rows
            .get_mut(&commit.step.index())
            .ok_or(ParentFailure::Conflict)?;
        if row.child != commit.child.to_bytes() || row.intent != commit.intent {
            return Err(ParentFailure::Conflict);
        }
        match row.result {
            Some(result)
                if result == commit.result
                    && row.continuation.as_deref() == Some(commit.continuation) => {}
            Some(_) => return Err(ParentFailure::Conflict),
            None => {
                row.result = Some(commit.result);
                row.json = Some(commit.result_json.into());
                row.continuation = Some(commit.continuation.into());
                self.ledger.advances += 1;
                self.persist();
            }
        }
        if self.lose_commit_reply {
            return Err(ParentFailure::Uncertain(
                "injected crash after commit".into(),
            ));
        }
        Ok(ParentReceipt {
            step: commit.step,
            child: commit.child,
            result: commit.result,
        })
    }
}

/// Durable receiver dedup plus the target worker's actual effect.
#[derive(Default)]
struct Receiver {
    rows: BTreeMap<[u8; 16], ([u8; 32], String)>,
    effects: Vec<([u8; 16], String, Option<String>)>,
}
impl Receiver {
    fn serve(&mut self, runtime: &SenderRuntime, attempt: ManagedAttempt) -> ManagedAttemptResult {
        let route = runtime.route();
        let (host, invocation, reply) = match attempt.claim(&route).unwrap().unwrap() {
            RequestView::Invoke {
                invocation,
                binding,
                revision,
                resource,
                action,
                argument,
                ..
            } => {
                let digest = ChildIntent {
                    binding: binding.clone(),
                    revision: revision.into(),
                    resource: resource.into(),
                    action: action.into(),
                    argument: argument.map(str::to_owned),
                }
                .digest();
                let id = uuid::Uuid::from_bytes(*invocation);
                let row = self.rows.entry(*invocation).or_insert_with(|| {
                    // First admission is the only effect permit.
                    self.effects
                        .push((*invocation, action.into(), argument.map(str::to_owned)));
                    let content = hex::encode(blake3::hash(argument.unwrap_or("").as_bytes()).as_bytes());
                    (
                        digest,
                        format!(
                            r#"{{"state":"succeeded","invocation":"{id}","output":{{"value":{{"content":"{content}"}}}}}}"#
                        ),
                    )
                });
                assert_eq!(
                    row.0, digest,
                    "same UUID with a changed body reached admission"
                );
                (binding.host, *invocation, Some(row.1.clone()))
            }
            RequestView::Observe { host, invocation }
            | RequestView::Cancel { host, invocation } => (
                *host,
                *invocation,
                self.rows.get(invocation).map(|row| row.1.clone()),
            ),
        };
        attempt
            .finish(Ok(runtime_collab::PublishedInvocationObservation {
                host,
                invocation,
                result: reply.map(|update_json| runtime_collab::PublishedInvocationResult {
                    id: 11,
                    update_json,
                    uncertain: false,
                    cancellation_requested: false,
                }),
            }))
            .unwrap()
    }
}

/// The parent's next-step continuation: the content produced by the exact
/// committed child observation, never an in-memory copy of the reply.
fn continuation(_: ParentStep, json: &str) -> Result<Vec<u8>, String> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    value["output"]["value"]["content"]
        .as_str()
        .map(|content| content.as_bytes().to_vec())
        .ok_or_else(|| "child produced no content".into())
}

fn outbox_rows(runtime: &SenderRuntime) -> usize {
    runtime
        .sender
        .inspect(runtime.caller.view())
        .unwrap()
        .iter()
        .flatten()
        .count()
}

fn run_step(
    runtime: &mut SenderRuntime,
    parent: &mut DurableParent,
    receiver: &mut Receiver,
    index: u32,
    intent: &ChildIntent,
) -> ChildTicket {
    let child = match runtime.begin_step(parent, step(index), intent).unwrap() {
        StepStart::Fresh(child) => child,
        other => panic!("expected a fresh child, got {other:?}"),
    };
    let attempt = runtime
        .attempt(parent, step(index), AttemptKind::Invoke, "", deadline())
        .unwrap();
    let reply = receiver.serve(runtime, attempt);
    assert_eq!(runtime.accept(reply).unwrap(), Reply::Retained);
    runtime
        .settle(parent, step(index), &mut continuation)
        .unwrap();
    child
}

#[test]
fn managed_runtime_two_step_handoff_recovers_after_crash_before_parent_commit() {
    let (root, state, cache) = committed_test_fixture(NativeSenderOutboxV1::empty_bytes());
    let ledger = root.path().join("parent.json");
    let mut receiver = Receiver::default();
    let mut runtime = SenderRuntime::install(state, caller()).unwrap();
    let mut parent = DurableParent::open(&ledger);
    let first = intent("draft", "launch brief");
    let StepStart::Fresh(original) = runtime.begin_step(&mut parent, step(0), &first).unwrap()
    else {
        panic!("fresh child expected")
    };
    let attempt = runtime
        .attempt(&parent, step(0), AttemptKind::Invoke, "", deadline())
        .unwrap();
    // The receiver admits and applies the child, then the reply is lost.
    drop(receiver.serve(&runtime, attempt));
    drop((runtime, parent, cache));

    let (mut runtime, cache) = reopen(root.path());
    let mut parent = DurableParent::open(&ledger);
    let recovery = runtime.recover(&mut parent, &mut continuation).unwrap();
    assert_eq!(
        recovery.iter().collect::<Vec<_>>(),
        vec![Recovered::Drive(step(0), original)]
    );
    // A replanned identical step reuses the uncertain original, never a new UUID.
    assert_eq!(
        runtime.begin_step(&mut parent, step(0), &first).unwrap(),
        StepStart::Recovered(original)
    );
    let attempt = runtime
        .attempt(&parent, step(0), AttemptKind::Invoke, "", deadline())
        .unwrap();
    let reply = receiver.serve(&runtime, attempt);
    assert_eq!(runtime.accept(reply).unwrap(), Reply::Retained);
    // Crash with the certain terminal observation durable but no parent commit.
    parent.refuse_commit = true;
    assert!(matches!(
        runtime.settle(&mut parent, step(0), &mut continuation),
        Err(SenderError::Parent(ParentFailure::Unavailable(_)))
    ));
    assert_eq!(
        outbox_rows(&runtime),
        1,
        "child retained without parent commit"
    );
    drop((runtime, parent, cache));

    let (mut runtime, _cache) = reopen(root.path());
    let mut parent = DurableParent::open(&ledger);
    assert_eq!(parent.ledger.advances, 0);
    let recovery = runtime.recover(&mut parent, &mut continuation).unwrap();
    assert_eq!(
        recovery.iter().collect::<Vec<_>>(),
        vec![Recovered::Settled(step(0))]
    );
    assert_eq!(parent.ledger.advances, 1);
    assert_eq!(outbox_rows(&runtime), 0);
    assert!(
        parent.ledger.rows[&0]
            .json
            .as_deref()
            .unwrap()
            .contains(&uuid::Uuid::from_bytes(original.invocation()).to_string()),
        "parent retains the exact original child result"
    );
    // The next step consumes only the committed continuation.
    let handoff = parent.continuation(0);
    let second = run_step(
        &mut runtime,
        &mut parent,
        &mut receiver,
        1,
        &intent("publish", &handoff),
    );
    assert_eq!(parent.ledger.advances, 2);
    assert_eq!(
        receiver
            .effects
            .iter()
            .map(|(id, action, argument)| (*id, action.as_str(), argument.clone()))
            .collect::<Vec<_>>(),
        vec![
            (original.invocation(), "draft", Some("launch brief".into())),
            (second.invocation(), "publish", Some(handoff)),
        ],
        "each child's effect happened exactly once"
    );
    assert_eq!(outbox_rows(&runtime), 0);
}

#[test]
fn managed_runtime_two_step_handoff_recovers_after_commit_before_child_removal() {
    let (root, state, cache) = committed_test_fixture(NativeSenderOutboxV1::empty_bytes());
    let ledger = root.path().join("parent.json");
    let mut receiver = Receiver::default();
    let mut runtime = SenderRuntime::install(state, caller()).unwrap();
    let mut parent = DurableParent::open(&ledger);
    let first = intent("draft", "launch brief");
    let StepStart::Fresh(original) = runtime.begin_step(&mut parent, step(0), &first).unwrap()
    else {
        panic!("fresh child expected")
    };
    let attempt = runtime
        .attempt(&parent, step(0), AttemptKind::Invoke, "", deadline())
        .unwrap();
    let reply = receiver.serve(&runtime, attempt);
    assert_eq!(runtime.accept(reply).unwrap(), Reply::Retained);
    // The parent commits durably; the process dies before the child is removed.
    parent.lose_commit_reply = true;
    assert!(matches!(
        runtime.settle(&mut parent, step(0), &mut continuation),
        Err(SenderError::Parent(ParentFailure::Uncertain(_)))
    ));
    assert_eq!(outbox_rows(&runtime), 1);
    drop((runtime, parent, cache));

    let (mut runtime, _cache) = reopen(root.path());
    let mut parent = DurableParent::open(&ledger);
    assert_eq!(parent.ledger.advances, 1);
    let recovery = runtime.recover(&mut parent, &mut continuation).unwrap();
    assert_eq!(
        recovery.iter().collect::<Vec<_>>(),
        vec![Recovered::Settled(step(0))]
    );
    assert_eq!(
        parent.ledger.advances, 1,
        "recovery never advances the parent twice"
    );
    assert_eq!(outbox_rows(&runtime), 0);
    // A repeated settle after a lost consumption acknowledgement is inert.
    let receipt = runtime
        .settle(&mut parent, step(0), &mut continuation)
        .unwrap();
    assert_eq!(receipt.child, original);
    assert_eq!(Some(receipt.result), parent.ledger.rows[&0].result);
    assert_eq!(parent.ledger.advances, 1);
    assert_eq!(
        runtime.begin_step(&mut parent, step(0), &first).unwrap(),
        StepStart::Committed(original),
        "a replayed committed step is not dispatched again"
    );
    assert!(matches!(
        runtime.attempt(&parent, step(0), AttemptKind::Invoke, "", deadline()),
        Err(SenderError::NotRetained)
    ));
    let handoff = parent.continuation(0);
    run_step(
        &mut runtime,
        &mut parent,
        &mut receiver,
        1,
        &intent("publish", &handoff),
    );
    assert_eq!(parent.ledger.advances, 2);
    assert_eq!(receiver.effects.len(), 2);
    assert_eq!(receiver.effects[0].0, original.invocation());
}

#[test]
fn managed_runtime_same_step_with_changed_body_is_refused_without_a_new_child() {
    let (root, state, _cache) = committed_test_fixture(NativeSenderOutboxV1::empty_bytes());
    let mut runtime = SenderRuntime::install(state, caller()).unwrap();
    let mut parent = DurableParent::open(&root.path().join("parent.json"));
    runtime
        .begin_step(&mut parent, step(0), &intent("draft", "one"))
        .unwrap();
    for changed in [
        intent("draft", "two"),
        ChildIntent {
            argument: None,
            ..intent("draft", "")
        },
        intent("draft", ""),
    ] {
        assert_eq!(
            runtime.begin_step(&mut parent, step(0), &changed),
            Err(SenderError::IdentityConflict)
        );
    }
    assert_eq!(outbox_rows(&runtime), 1);
    assert_ne!(
        ChildIntent {
            argument: None,
            ..intent("draft", "")
        }
        .digest(),
        intent("draft", "").digest(),
        "None and Some(empty) are distinct intents"
    );
}

#[test]
fn managed_runtime_nothing_is_sent_before_the_parent_mapping_and_restart_adopts_the_original() {
    let (root, state, cache) = committed_test_fixture(NativeSenderOutboxV1::empty_bytes());
    let ledger = root.path().join("parent.json");
    let mut runtime = SenderRuntime::install(state, caller()).unwrap();
    let mut parent = DurableParent::open(&ledger);
    parent.refuse_bind = true;
    let first = intent("draft", "launch brief");
    assert!(matches!(
        runtime.begin_step(&mut parent, step(0), &first),
        Err(SenderError::Parent(ParentFailure::Unavailable(_)))
    ));
    assert_eq!(outbox_rows(&runtime), 1, "row committed before mapping");
    assert_eq!(
        runtime
            .attempt(&parent, step(0), AttemptKind::Invoke, "", deadline())
            .unwrap_err(),
        SenderError::Unmapped
    );
    drop((runtime, parent, cache));

    let (mut runtime, _cache) = reopen(root.path());
    let mut parent = DurableParent::open(&ledger);
    let recovery = runtime.recover(&mut parent, &mut continuation).unwrap();
    let [Recovered::Unmapped(original)] = recovery.iter().collect::<Vec<_>>()[..] else {
        panic!("unmapped original expected")
    };
    // A different request never adopts the unmapped row.
    let StepStart::Fresh(other) = runtime
        .begin_step(&mut parent, step(5), &intent("draft", "other"))
        .unwrap()
    else {
        panic!("fresh child expected")
    };
    assert_ne!(other, original);
    assert_eq!(
        runtime.begin_step(&mut parent, step(0), &first).unwrap(),
        StepStart::Recovered(original)
    );
    assert_eq!(outbox_rows(&runtime), 2);
    let mut receiver = Receiver::default();
    let attempt = runtime
        .attempt(&parent, step(0), AttemptKind::Invoke, "", deadline())
        .unwrap();
    let reply = receiver.serve(&runtime, attempt);
    assert_eq!(runtime.accept(reply).unwrap(), Reply::Retained);
    assert_eq!(receiver.effects[0].0, original.invocation());
}

#[test]
fn managed_runtime_uncertain_or_absent_child_is_retained_and_not_settled() {
    let (root, state, _cache) = committed_test_fixture(NativeSenderOutboxV1::empty_bytes());
    let mut runtime = SenderRuntime::install(state, caller()).unwrap();
    let mut parent = DurableParent::open(&root.path().join("parent.json"));
    let mut receiver = Receiver::default();
    runtime
        .begin_step(&mut parent, step(0), &intent("draft", "one"))
        .unwrap();
    let attempt = runtime
        .attempt(&parent, step(0), AttemptKind::Observe, "", deadline())
        .unwrap();
    let reply = receiver.serve(&runtime, attempt);
    assert_eq!(runtime.accept(reply).unwrap(), Reply::Absent);
    assert_eq!(
        runtime.settle(&mut parent, step(0), &mut continuation),
        Err(SenderError::NotTerminal)
    );
    assert_eq!(parent.ledger.advances, 0);
    assert_eq!(outbox_rows(&runtime), 1);
}
