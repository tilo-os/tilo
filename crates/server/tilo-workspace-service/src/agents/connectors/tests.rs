//! A connector's secret lives in the workspace's vault and reaches the worker that signs in
//! with it; no record and nothing a program reads holds it.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion"
)]

use std::sync::Arc;

use super::*;
use crate::workspaces::fixtures::{TestGroupKeys, governor};

/// The secret the connector signs in with: made up, and nobody's.
const TOKEN: &str = "discord-bot-token-made-up";

/// The connector's secret is kept in the workspace's vault, and the store the worker reads
/// answers it there and nowhere else: not through a record, and not through an item the
/// connector never named.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connectors_secret_reaches_the_worker_from_the_vault_and_no_record() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let keys = Arc::new(TestGroupKeys::default());
    let host = crate::workspaces::fixtures::test_host_holding(dir.path(), &key, &cert, keys).await;
    let workspaces = host.workspaces();
    workspaces.set_member_device(Arc::new(crate::workspaces::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    let workspace = workspaces.create("Discord").await.unwrap().workspace;
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: tilo_app_id::apps::PASSWORDS.into(),
        cluster_id: workspace.clone(),
        document: tilo_action_policy::roles::WORKSPACE_VAULT_DOCUMENT.into(),
    };
    let _ = workspaces.own_route_keys(&route, true).await.unwrap();

    let connectors = Connectors::new();
    let store = connectors.secrets(&workspaces, &workspace, "so.tilo.discord");
    assert!(
        store
            .item("bot-token", "default")
            .get()
            .await
            .unwrap()
            .is_none(),
        "the vault keeps nothing yet"
    );

    // What the person does in Passwords: the token is an entry of the workspace's vault, named
    // for the connector so the vault says where it came from.
    let title = person_vault::connector_title("so.tilo.discord", "bot-token", "default");
    let place =
        accounts::vault_items::Place::parse(&person_vault::connector_place("so.tilo.discord"))
            .unwrap();
    let kept = workspaces
        .changing_vault(&workspace, |items| {
            items
                .keep(
                    None,
                    NewItem::new(
                        ItemKind::Value,
                        vec![place],
                        String::new(),
                        TOKEN.to_owned(),
                    ),
                    &title,
                )
                .map(|facts| ItemRef::from_id(facts.id.as_str()).unwrap())
        })
        .await
        .unwrap();

    let released = store
        .item("bot-token", "default")
        .get()
        .await
        .unwrap()
        .expect("the worker is handed the secret");
    assert_eq!(released.expose(), TOKEN.as_bytes());
    assert!(
        store.get("password/default").await.unwrap().is_none(),
        "a purpose the worker may not ask for is none"
    );
    assert!(
        store.get("bot-token/other").await.unwrap().is_none(),
        "an item the vault does not hold for this connector is none"
    );
    let elsewhere = Connectors::new().secrets(&workspaces, &workspace, "so.tilo.other");
    assert!(
        elsewhere
            .item("bot-token", "default")
            .get()
            .await
            .unwrap()
            .is_none(),
        "another connector is handed nothing of this one's"
    );

    // The record a program follows names the item, never the secret: the reference is what a
    // person's change carries and what a program reads.
    let referenced = kept.to_string();
    assert!(!referenced.contains(TOKEN));
    let record = format!(r#"{{"title":"Bridge","token":{referenced:?}}}"#);
    assert!(
        !record.contains(TOKEN),
        "no record holds the secret a connector signs in with"
    );
    host.shutdown().await;
}

/// A record of the connector's app as the drain reads it: the entries a step queued, each
/// settled once its outcome is written back.
#[derive(Default)]
struct Records(std::sync::Mutex<Vec<OutboxRow>>);

impl Records {
    /// A step queues `value` in the record's outbox, as entry `id`.
    fn queue(&self, id: &str, value: Value) {
        self.0.lock().unwrap().push(OutboxRow {
            id: id.to_owned(),
            value,
            settled: None,
        });
    }

    /// How entry `id` settled, if it did.
    fn settled(&self, id: &str) -> Option<Value> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .find(|row| row.id == id)
            .and_then(|row| row.settled.clone())
    }
}

impl EffectRecords for Records {
    fn entries<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        record: &'a str,
    ) -> Answer<'a, Vec<OutboxRow>> {
        let rows = if record == RECORD {
            self.0.lock().unwrap().clone()
        } else {
            Vec::new()
        };
        Box::pin(std::future::ready(rows))
    }

    fn settle<'a>(
        &'a self,
        _: (&'a str, &'a str, &'a str),
        entry: &'a str,
        outcome: Value,
    ) -> Answer<'a, Result<(), String>> {
        let mut rows = self.0.lock().unwrap();
        let settled = rows
            .iter_mut()
            .find(|row| row.id == entry)
            .map(|row| {
                assert!(row.settled.is_none(), "entry {entry} is settled once");
                row.settled = Some(outcome);
            })
            .ok_or_else(|| format!("no entry {entry}"));
        Box::pin(std::future::ready(settled))
    }
}

/// The record a step queued its sends in.
const RECORD: &str = "channel-42";
/// The connector's app.
const DISCORD: &str = "so.tilo.discord";

/// A connector's worker that answers what it is told to, in order, and keeps what it sent.
#[derive(Default)]
struct Worker {
    answers: std::sync::Mutex<std::collections::VecDeque<Result<Value, PerformError>>>,
    sent: std::sync::Mutex<Vec<Value>>,
}

impl Performer for Worker {
    fn perform(&self, queued: Value) -> Result<Value, PerformError> {
        self.sent.lock().unwrap().push(queued);
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(json!({"id": "posted"})))
    }

    fn events_after(&self, _: u64) -> Vec<ReportedEvent> {
        Vec::new()
    }
}

/// A send queued in a record of an app whose connector runs here reaches the worker once, in
/// the order the entries were queued, and what the service answered is settled in the record:
/// a send the service could not take now waits, with what follows it, and goes out again; one
/// it refused is settled failed with its reason; and a send whose settle was lost is settled
/// from what was kept of it without going out a second time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, end to end, reads as one test"
)]
async fn a_queued_send_reaches_the_worker_once_and_its_answer_is_settled_in_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = crate::workspaces::fixtures::test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Discord").await.unwrap().workspace;
    let worker = Arc::new(Worker::default());
    let records = Arc::new(Records::default());
    let mut drain = Connectors::new()
        .running(DISCORD, Arc::clone(&worker) as Arc<dyn Performer>)
        .drain(
            workspaces.clone(),
            Arc::clone(&records) as Arc<dyn EffectRecords>,
        );
    let written = |app: &str| {
        vec![ResourceWrite {
            workspace: workspace.clone(),
            app: app.to_owned(),
            record: RECORD.to_owned(),
        }]
    };

    // Round trip: the step queued it, the worker sends it, the record holds the answer.
    records.queue("q1", json!({"op": {"send": {"content": "hi"}}}));
    drain.pass(written(DISCORD)).await;
    assert_eq!(
        *worker.sent.lock().unwrap(),
        [json!({"op": {"send": {"content": "hi"}}})]
    );
    assert_eq!(
        records.settled("q1"),
        Some(json!({"state": "done", "result": {"id": "posted"}}))
    );
    drain.pass(written(DISCORD)).await;
    assert_eq!(
        worker.sent.lock().unwrap().len(),
        1,
        "a settled entry goes out once"
    );

    // A record of an app whose connector does not run here is not this device's to send.
    drain.pass(written("so.tilo.other")).await;
    assert_eq!(worker.sent.lock().unwrap().len(), 1);

    // Unavailable: it and what follows it wait, and go out in order once the service is back.
    worker
        .answers
        .lock()
        .unwrap()
        .push_back(Err(PerformError::Unavailable("rate limited".into())));
    records.queue("q2", json!({"op": {"send": {"content": "two"}}}));
    records.queue("q3", json!({"op": {"send": {"content": "three"}}}));
    drain.pass(written(DISCORD)).await;
    assert_eq!(records.settled("q2"), None);
    assert_eq!(records.settled("q3"), None, "what follows waits its turn");
    assert_eq!(
        worker.sent.lock().unwrap().len(),
        2,
        "q3 was not sent before q2"
    );
    drain.pass(Vec::new()).await;
    let sent = worker.sent.lock().unwrap().clone();
    assert_eq!(
        sent[1..],
        [
            json!({"op": {"send": {"content": "two"}}}),
            json!({"op": {"send": {"content": "two"}}}),
            json!({"op": {"send": {"content": "three"}}}),
        ],
        "the drain looks again unasked, and sends in order"
    );
    assert!(records.settled("q2").is_some() && records.settled("q3").is_some());

    // Refused: settled failed, with the service's reason, and never sent again.
    worker
        .answers
        .lock()
        .unwrap()
        .push_back(Err(PerformError::Refused("missing permissions".into())));
    records.queue("q4", json!({"op": {"send": {"content": "nope"}}}));
    drain.pass(written(DISCORD)).await;
    assert_eq!(
        records.settled("q4"),
        Some(json!({"state": "failed", "result": {"reason": "missing permissions"}}))
    );

    // A device that sent an entry and stopped before settling it left what the service
    // answered in the workspace: the entry is settled with that, and not sent again.
    let turn = Turn::Send {
        app: DISCORD.into(),
        record: RECORD.into(),
        entry: "q5".into(),
    };
    let claim = takes_turn(workspaces, &workspace, &turn).await.unwrap();
    keep_done(
        workspaces,
        &workspace,
        &turn,
        json!({"state": "done", "result": {"id": "sent-before"}}),
    )
    .await
    .unwrap();
    claim.release().await;
    records.queue("q5", json!({"op": {"send": {"content": "once"}}}));
    let before = worker.sent.lock().unwrap().len();
    drain.pass(written(DISCORD)).await;
    assert_eq!(
        worker.sent.lock().unwrap().len(),
        before,
        "sent once, by whoever sent it"
    );
    assert_eq!(
        records.settled("q5"),
        Some(json!({"state": "done", "result": {"id": "sent-before"}}))
    );
    host.shutdown().await;
}

/// What a connector reports is what the trigger runner reads of it: the worker of the app named,
/// after the cursor named, and nothing for an app whose connector does not run here.
#[test]
fn a_connectors_events_are_read_from_its_worker() {
    struct Reporting;
    impl Performer for Reporting {
        fn perform(&self, _: Value) -> Result<Value, PerformError> {
            Err(PerformError::Refused("reports only".into()))
        }
        fn events_after(&self, cursor: u64) -> Vec<ReportedEvent> {
            (cursor + 1..=2)
                .map(|seq| ReportedEvent {
                    seq,
                    source: "message-created".into(),
                    id: seq.to_string(),
                    at_ms: 0,
                    payload: serde_json::Map::new(),
                })
                .collect()
        }
    }
    let connectors = Connectors::new().running(DISCORD, Arc::new(Reporting));
    assert_eq!(connectors.since(DISCORD, 0).len(), 2);
    assert_eq!(connectors.since(DISCORD, 1)[0].seq, 2);
    assert!(connectors.since("so.tilo.other", 0).is_empty());
}
