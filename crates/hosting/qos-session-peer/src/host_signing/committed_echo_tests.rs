//! Regression: a browser edit admitted through the delivery data door must not
//! break the mounted native worker's scoped republication of merged history.
use super::*;
use crate::action_host::scoped_fixture::ScopedAppFixture;
use std::time::Duration;

fn worker_value(fixture: &mut ScopedAppFixture, field: &str) -> Option<String> {
    use ui::automerge::ReadDoc;
    let id = fixture.record_id();
    fixture
        .records()
        .read(&id, |doc| {
            doc.get(ui::automerge::ROOT, field)
                .ok()
                .flatten()
                .and_then(|(value, _)| value.into_string().ok())
        })
        .unwrap()
}

async fn settle(fixture: &mut ScopedAppFixture, field: &str, value: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    // Keep pumping after the merge so the worker's scoped republication of the
    // merged history reaches the actual native signing door.
    let mut after_merge = 0;
    while after_merge < 20 {
        fixture
            .pump()
            .await
            .unwrap_or_else(|error| panic!("native host signing refused merged history: {error}"));
        if worker_value(fixture, field).as_deref() == Some(value) {
            after_merge += 1;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "mounted worker never merged the browser upload"
        );
    }
}

async fn upload_then_republish(app: &str) -> ScopedAppFixture {
    let mut fixture = ScopedAppFixture::with_delivery(app);
    // Startup: the real mounted worker publishes and replays its exact baseline.
    for _ in 0..40 {
        fixture.pump().await.unwrap();
    }
    for (delivery, value) in [(1, "first browser edit"), (2, "second browser edit")] {
        let generation = fixture.owner.committed_generation();
        fixture.upload_remote_edit(delivery, "browser_probe", value);
        assert_eq!(fixture.owner.committed_generation(), generation + 1);
        settle(&mut fixture, "browser_probe", value).await;
        assert!(fixture.signing().retirement_ready().unwrap());
        assert!(fixture.host.ready());
    }
    let committed = fixture
        .owner
        .peer()
        .document(&fixture.app, &fixture.record)
        .unwrap();
    let mut committed = ui::automerge::AutoCommit::load(&committed).unwrap();
    let id = fixture.record_id();
    let mut local = fixture.records().read(&id, |doc| doc.clone()).unwrap();
    assert_eq!(committed.get_heads(), local.get_heads());
    fixture
}

#[tokio::test(flavor = "current_thread")]
async fn browser_upload_then_worker_republication_keeps_native_signing_healthy() {
    let mut fixture = upload_then_republish("dev.qos.word-processor").await;
    // Orderly close settles every worker write against committed history.
    fixture.host.close(&fixture.owner).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn browser_chat_upload_then_worker_republication_closes_cleanly() {
    let mut fixture = upload_then_republish("dev.qos.chat").await;
    fixture.host.close(&fixture.owner).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn committed_echo_parses_the_exact_body_change_set() {
    use ui::automerge::transaction::Transactable;
    let fixture = ScopedAppFixture::with_delivery("dev.qos.word-processor");
    let owner = &fixture.owner;
    let bytes = owner
        .peer()
        .document(&fixture.app, &fixture.record)
        .unwrap();
    let mut document = ui::automerge::AutoCommit::load(&bytes).unwrap();
    let heads = document.get_heads();
    document
        .put(ui::automerge::ROOT, "never_committed", "x")
        .unwrap();
    document.commit();
    let fresh = DocumentMutation::Incremental(document.save_after(&heads));
    let hashes = crate::owner::committed_echo::mutation_change_hashes(&fresh).unwrap();
    assert_eq!(hashes.len(), 1);
    let committed = DocumentMutation::Snapshot(bytes);
    assert!(
        !crate::owner::committed_echo::mutation_change_hashes(&committed)
            .unwrap()
            .is_empty()
    );
    let mut corrupt = document.save_after(&heads);
    corrupt[0] ^= 0xff;
    assert!(
        crate::owner::committed_echo::mutation_change_hashes(&DocumentMutation::Incremental(
            corrupt
        ))
        .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn committed_echo_refuses_uncommitted_factory_body_through_the_signing_door() {
    let mut fixture = ScopedAppFixture::with_uncommitted_factory_data("dev.qos.notes");
    let pending = fixture.next_signature_for_selected_record().await;
    let generation = fixture.owner.committed_generation();
    let disk = std::fs::read(fixture.owner.root().join("session.bin")).unwrap();
    let mut echo = None;
    let (complete, result) = pending.0.apply_scoped_fenced(
        crate::owner_commit::execution_deadline(),
        |route, _, mutation, authorization| {
            let authorization = authorization.expect("genuine SDK startup custody");
            echo = Some(
                fixture
                    .owner
                    .validate_committed_echo(route, &mutation, authorization),
            );
            Err(HostDocumentTurnError::Refused("probe only".into()))
        },
    );
    complete();
    assert!(result.is_err());
    let error = echo.unwrap().unwrap_err();
    assert!(
        error.contains("neither original caller nor committed"),
        "wrong refusal: {error}"
    );
    assert_eq!(fixture.owner.committed_generation(), generation);
    assert_eq!(
        std::fs::read(fixture.owner.root().join("session.bin")).unwrap(),
        disk
    );
}

// Regression (local run 30/33 stage e): the headless native Chat worker used
// to run `mark_active_read` from `sync` on every host pump, authoring an
// unscoped "Mark channel read" change as the host identity. Required worker
// provenance blocked its publication, the write was never acknowledged, and
// orderly close preserved recovery custody. A hosted worker never presents a
// viewport, so it must not author viewer read receipts.
#[tokio::test(flavor = "current_thread")]
async fn browser_chat_message_then_worker_republication_settles_every_write() {
    let mut fixture = ScopedAppFixture::with_delivery("dev.qos.chat");
    for _ in 0..40 {
        fixture.pump().await.unwrap();
    }
    let bytes = fixture
        .owner
        .peer()
        .document(&fixture.app, &fixture.record)
        .unwrap();
    let document = ui::automerge::AutoCommit::load(&bytes).unwrap();
    let channel = {
        use ui::automerge::ReadDoc;
        let messages = document
            .get(ui::automerge::ROOT, "messages")
            .unwrap()
            .map(|(_, id)| id);
        messages
            .and_then(|id| document.keys(&id).next())
            .unwrap_or_else(|| "c_general".into())
    };
    for (delivery, text) in [(1, "first browser chat"), (2, "second browser chat")] {
        let channel = channel.clone();
        let generation = fixture.owner.committed_generation();
        fixture.upload_remote_change(delivery, move |document| {
            chat::doc::ChatDoc::push_message(
                document,
                &channel,
                chat::doc::NewMessage {
                    author: "u_browser".into(),
                    ts: chat::doc::NOW,
                    text: text.into(),
                    ..Default::default()
                },
            );
        });
        assert_eq!(fixture.owner.committed_generation(), generation + 1);
        for _ in 0..40 {
            fixture.pump().await.unwrap_or_else(|error| {
                panic!("native host signing refused chat history: {error}")
            });
        }
    }
    let committed = fixture
        .owner
        .peer()
        .document(&fixture.app, &fixture.record)
        .unwrap();
    let mut committed = ui::automerge::AutoCommit::load(&committed).unwrap();
    let id = fixture.record_id();
    let mut local = fixture.records().read(&id, |doc| doc.clone()).unwrap();
    assert_eq!(
        committed.get_heads(),
        local.get_heads(),
        "worker holds unpublished local chat changes"
    );
    assert_eq!(
        fixture.pending_worker_writes(),
        0,
        "worker persistence awaits host acknowledgement"
    );
    fixture.host.close(&fixture.owner).unwrap();
}
