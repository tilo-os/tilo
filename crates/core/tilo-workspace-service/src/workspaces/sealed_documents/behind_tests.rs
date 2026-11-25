#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the member's helpers are part of the test's assertions"
)]
//! A member's compaction where the governor's copy was behind, or folded the history itself.

use super::tests::{
    APP, Lounge, Session, follows, lounge_holding, lounge_with_link, member_session, readable,
    retained_until, retained_writes, seal_and_send_title, title,
};
use crate::workspaces::fixtures::{TestGroupKeys, test_host_holding};
use automerge::{ROOT, ReadDoc as _, transaction::Transactable as _};
use collab_types::{CommitNamed as _, SignedOp};
use neighbour_wire::NeighbourMsg;

const DOCUMENT: &str = "welcome";

/// A member writes `lines` as the document's title, sealed, and the node follows them: the
/// document's sealed writes the server keeps, in the order it admitted them.
async fn titles(lounge: &Lounge, session: &Session, lines: &[&str]) -> Vec<SignedOp> {
    for line in lines {
        seal_and_send_title(&lounge.member, session, &lounge.workspace, line);
    }
    let last = *lines.last().unwrap();
    follows(&lounge.workspaces, &lounge.workspace, last).await;
    retained_until(&lounge.server, lounge.workspace_id, &lounge.member, |doc| {
        title(&doc.save()).as_deref() == Some(last)
    })
    .await
    .into_iter()
    .filter(|op| op.encryption.is_some())
    .collect()
}

/// Whether the node's admission takes `message`, a write `lounge`'s member sent.
async fn admits(lounge: &Lounge, message: &NeighbourMsg) -> Option<String> {
    lounge
        .workspaces
        .refuses_write(
            &lounge.workspace,
            lounge.member_key.verifying_key().to_bytes(),
            &postcard::to_stdvec(message).unwrap(),
        )
        .await
}

/// The member's write `message`, kept by the server without the governor's copy being told.
fn kept_untold(lounge: &Lounge, message: &NeighbourMsg) {
    lounge
        .server
        .workspace_documents()
        .carry(
            lounge.workspace_id,
            lounge.member_key.verifying_key().to_bytes(),
            &crate::workspace_documents::HostCommit {
                body: postcard::to_stdvec(message).unwrap(),
            },
        )
        .unwrap();
}

/// A member writes "one", "two" and "three"; the server keeps all three, and its governor's
/// copy is told only of the first and the third. Answers the governed workspace, the second
/// write, and the member's compaction of the three, which the copy refuses.
async fn a_copy_without_the_second_write(
    lounge: &Lounge,
    session: &Session,
) -> (
    std::sync::Arc<tokio::sync::Mutex<crate::workspaces::GovernedWorkspace>>,
    SignedOp,
    NeighbourMsg,
) {
    titles(lounge, session, &["one"]).await;
    let ((), second) = lounge.member.seal(&lounge.workspace, |doc| {
        doc.put(ROOT, "title", "two").unwrap();
        doc.put(ROOT, "second", "kept").unwrap();
        doc.commit_edit("fixture", "title", &lounge.member.person());
    });
    kept_untold(lounge, &second);
    seal_and_send_title(&lounge.member, session, &lounge.workspace, "three");
    let history: Vec<SignedOp> =
        retained_until(&lounge.server, lounge.workspace_id, &lounge.member, |doc| {
            title(&doc.save()).as_deref() == Some("three")
        })
        .await
        .into_iter()
        .filter(|op| op.encryption.is_some())
        .collect();
    let NeighbourMsg::AppDocument {
        envelope: second, ..
    } = second
    else {
        panic!("a document write");
    };
    assert!(
        history.contains(&second),
        "the server keeps the second write"
    );
    let third = history.last().unwrap().clone();
    let state = lounge
        .workspaces
        .workspace(&lounge.workspace)
        .await
        .unwrap();
    tilo_test_env::within("the copy takes the third write", async {
        while !state
            .lock()
            .await
            .peer()
            .publication()
            .holds_sealed(APP, DOCUMENT, &third)
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        !state
            .lock()
            .await
            .peer()
            .publication()
            .holds_sealed(APP, DOCUMENT, &second),
        "the copy passed the second write over"
    );
    let compaction = lounge
        .member
        .compacts(&lounge.workspaces, &lounge.workspace, history)
        .await;
    let author = lounge.member_key.verifying_key().to_bytes();
    assert!(
        state.lock().await.refuses(author, &compaction).is_some(),
        "a copy without the second write refuses the member's compaction"
    );
    (state, *second, compaction)
}

/// A node told of a member's writes out of order passed the earlier one over for good, and
/// refused every compaction the member made of the document. Loading the workspace takes the
/// write the server keeps in again, and the member's compaction is taken.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_copy_that_passed_a_write_over_takes_it_in_again_on_load() {
    let (lounge, session) = lounge_with_link().await;
    let (state, second, compaction) = a_copy_without_the_second_write(&lounge, &session).await;

    lounge
        .workspaces
        .recall(&mut *state.lock().await)
        .await
        .unwrap();
    assert!(
        state
            .lock()
            .await
            .peer()
            .publication()
            .holds_sealed(APP, DOCUMENT, &second),
        "the second write is taken in again"
    );
    let author = lounge.member_key.verifying_key().to_bytes();
    assert_eq!(state.lock().await.refuses(author, &compaction), None);
    drop((state, lounge.workspaces));
    lounge.server.shutdown().await;
}

/// The server keeps a member's compaction its governor's copy never decided, as when the node
/// was ended between the two, and the compaction covers a write the copy passed over. The
/// server keeps that write beside the compaction until every device with the record's keys has
/// checked it, the copy among them; loading the workspace takes the compaction with the node's
/// keys, as it holds every change the copy holds, and the record reads whole.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_copy_takes_a_kept_compaction_of_a_write_it_passed_over_on_load() {
    let (lounge, session) = lounge_with_link().await;
    let (state, second, compaction) = a_copy_without_the_second_write(&lounge, &session).await;
    kept_untold(&lounge, &compaction);
    assert!(
        retained_writes(&lounge.server, lounge.workspace_id, DOCUMENT)
            .await
            .contains(&second),
        "the server keeps the write until the copy has checked the compaction"
    );

    lounge
        .workspaces
        .recall(&mut *state.lock().await)
        .await
        .unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = &compaction else {
        panic!("a compaction is a document write");
    };
    let held = state
        .lock()
        .await
        .peer()
        .publication()
        .sealed_history(APP, DOCUMENT);
    assert_eq!(
        held.first().map(|op| op.signature),
        Some(envelope.signature),
        "the member's compaction leads the copy's history"
    );
    assert_eq!(
        readable(&lounge.workspaces, &lounge.workspace)
            .await
            .as_deref(),
        Some("three")
    );
    drop((state, lounge.workspaces));
    lounge.server.shutdown().await;
}

/// The node folds its copy of a document its members seal, as it does before the history fills,
/// and publishes the fold as a member publishes its own: the history the server keeps leads
/// with it. A member's compaction of that history is taken in its place.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_members_compaction_of_the_nodes_fold_is_taken() {
    let (lounge, session) = lounge_with_link().await;
    titles(&lounge, &session, &["one", "two", "three"]).await;
    let state = lounge
        .workspaces
        .workspace(&lounge.workspace)
        .await
        .unwrap();
    let mut governed = state.lock().await;
    let now = tilo_clock::wall();
    let mut folded = governed.peer().clone();
    let keys = governed
        .document_keys(APP, DOCUMENT)
        .await
        .unwrap()
        .unwrap();
    let written = keys
        .compact(
            &mut folded,
            (APP, DOCUMENT),
            None,
            (&lounge.governor, &lounge.governor_key),
            now,
        )
        .unwrap();
    let collaboration = governed.collaboration().clone();
    governed
        .commit(folded, &lounge.governor_key, now, |_, _| Ok(collaboration))
        .unwrap();
    lounge.workspaces.settle(&mut governed).await.unwrap();
    lounge
        .workspaces
        .carry_written(governed.workspace_id(), &written)
        .unwrap();
    let fold = governed.peer().publication().sealed_history(APP, DOCUMENT)[0].clone();
    drop(governed);
    tilo_test_env::within("the server keeps the node's fold", async {
        while !retained_writes(&lounge.server, lounge.workspace_id, DOCUMENT)
            .await
            .contains(&fold)
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    // The history as the member's engine knows the server's: the fold, then what it does not
    // cover.
    let covered = collab_types::SealedCompaction::parse(&fold).unwrap();
    let history: Vec<SignedOp> = titles(&lounge, &session, &["four"])
        .await
        .into_iter()
        .filter(|op| *op == fold || !covered.covers(op))
        .collect();
    let compaction = lounge
        .member
        .compacts(&lounge.workspaces, &lounge.workspace, history)
        .await;
    assert_eq!(admits(&lounge, &compaction).await, None);

    let author = lounge.member_key.verifying_key().to_bytes();
    lounge
        .workspaces
        .absorb(
            &lounge.workspace,
            author,
            &postcard::to_stdvec(&compaction).unwrap(),
        )
        .await
        .unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = &compaction else {
        panic!("a compaction is a document write");
    };
    let held = state
        .lock()
        .await
        .peer()
        .publication()
        .sealed_history(APP, DOCUMENT);
    assert_eq!(
        held.first().map(|op| op.signature),
        Some(envelope.signature),
        "the member's compaction leads the copy's history"
    );
    assert_eq!(
        readable(&lounge.workspaces, &lounge.workspace)
            .await
            .as_deref(),
        Some("four")
    );
    drop((state, lounge.workspaces));
    lounge.server.shutdown().await;
}

/// What a reader of [`DOCUMENT`] reads at `field`.
async fn field(lounge: &Lounge, name: &str) -> Option<String> {
    let bytes = lounge
        .workspaces
        .resource_document(&lounge.workspace, APP, DOCUMENT)
        .await
        .unwrap()?;
    automerge::AutoCommit::load(&bytes)
        .ok()?
        .get(ROOT, name)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

/// A copy that holds only part of what a compaction folds takes it when it arrives live, as one
/// that was away while the history was compacted: the writes it lacks are nowhere but inside the
/// compaction from then on, so a copy that refused it could never read them again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_copy_takes_a_compaction_that_arrives_live_over_a_write_it_lacks() {
    let (lounge, session) = lounge_with_link().await;
    let (state, second, compaction) = a_copy_without_the_second_write(&lounge, &session).await;
    let author = lounge.member_key.verifying_key().to_bytes();
    assert!(
        !state
            .lock()
            .await
            .peer()
            .publication()
            .holds_sealed(APP, DOCUMENT, &second),
        "the copy lacks the second write"
    );
    assert!(
        state.lock().await.refuses(author, &compaction).is_some(),
        "a copy without the second write refuses the member's compaction"
    );

    // The member's compaction reaches the copy as a live write, not through a load.
    lounge
        .workspaces
        .absorb(
            &lounge.workspace,
            author,
            &postcard::to_stdvec(&compaction).unwrap(),
        )
        .await
        .unwrap();

    let NeighbourMsg::AppDocument { envelope, .. } = &compaction else {
        panic!("a compaction is a document write");
    };
    let held = state
        .lock()
        .await
        .peer()
        .publication()
        .sealed_history(APP, DOCUMENT);
    assert_eq!(
        held.first().map(|op| op.signature),
        Some(envelope.signature),
        "the member's compaction leads the copy's history"
    );
    assert_eq!(field(&lounge, "title").await.as_deref(), Some("three"));
    assert_eq!(
        field(&lounge, "second").await.as_deref(),
        Some("kept"),
        "the write the copy lacked reads from the compaction"
    );
    drop((state, lounge.workspaces));
    lounge.server.shutdown().await;
}

/// A copy takes a compaction that arrives before any of the writes it folds, and reads every
/// change it folds: the compaction is the record's base, and the writes that follow it are the
/// ones it already holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_copy_takes_a_compaction_that_arrives_before_any_of_its_writes() {
    let (lounge, session) = lounge_with_link().await;
    let sealed = |message: NeighbourMsg| {
        let NeighbourMsg::AppDocument { envelope, .. } = message else {
            panic!("a sealed write is a document write");
        };
        *envelope
    };
    // The member's writes build on this copy's record, as a page's do on the host it reads.
    let state = lounge
        .workspaces
        .workspace(&lounge.workspace)
        .await
        .unwrap();
    let base = state
        .lock()
        .await
        .peer()
        .document(APP, DOCUMENT)
        .expect("the copy holds the record");
    lounge.member.reset_to(&base);
    let ((), first) = lounge.member.seal(&lounge.workspace, |doc| {
        doc.put(ROOT, "title", "one").unwrap();
        doc.put(ROOT, "first", "kept").unwrap();
        doc.commit_edit("fixture", "title", &lounge.member.person());
    });
    let ((), second) = lounge.member.seal(&lounge.workspace, |doc| {
        doc.put(ROOT, "title", "two").unwrap();
        doc.commit_edit("fixture", "title", &lounge.member.person());
    });
    let ((), third) = lounge.member.seal(&lounge.workspace, |doc| {
        doc.put(ROOT, "title", "three").unwrap();
        doc.commit_edit("fixture", "title", &lounge.member.person());
    });
    let writes = [first.clone(), second.clone(), third.clone()]
        .into_iter()
        .map(sealed)
        .collect();
    let compaction = lounge
        .member
        .compacts(&lounge.workspaces, &lounge.workspace, writes)
        .await;

    // The member's compaction reaches the copy first, then the writes it folds.
    session.send(compaction.clone());
    for write in [first, second, third] {
        session.send(write);
    }
    let NeighbourMsg::AppDocument { envelope, .. } = &compaction else {
        panic!("a compaction is a document write");
    };
    tilo_test_env::within("the copy takes the compaction", async {
        loop {
            let leads = state
                .lock()
                .await
                .peer()
                .publication()
                .sealed_history(APP, DOCUMENT)
                .first()
                .map(|op| op.signature)
                == Some(envelope.signature);
            if leads {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    // The writes it folds follow it: until they are here the record is not whole yet, and reads
    // as such, never as a refusal.
    tilo_test_env::within("the copy reads the record whole", async {
        while lounge
            .workspaces
            .resource_document(&lounge.workspace, APP, DOCUMENT)
            .await
            .unwrap()
            .is_none()
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(field(&lounge, "title").await.as_deref(), Some("three"));
    assert_eq!(field(&lounge, "first").await.as_deref(), Some("kept"));
    drop((state, lounge.workspaces));
    lounge.server.shutdown().await;
}

/// A copy that holds only part of what a compaction folds takes it when its author sends it over
/// the session, as the copy that follows the history sees it: a change refused there is never
/// kept, and the writes the compaction folds are nowhere else.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_copy_takes_a_compaction_a_device_sends_over_a_write_it_lacks() {
    let (lounge, session) = lounge_with_link().await;
    let (state, second, compaction) = a_copy_without_the_second_write(&lounge, &session).await;
    assert!(
        !state
            .lock()
            .await
            .peer()
            .publication()
            .holds_sealed(APP, DOCUMENT, &second),
        "the copy lacks the second write"
    );

    session.send(compaction.clone());
    let NeighbourMsg::AppDocument { envelope, .. } = &compaction else {
        panic!("a compaction is a document write");
    };
    tilo_test_env::within("the copy takes the compaction", async {
        loop {
            let leads = state
                .lock()
                .await
                .peer()
                .publication()
                .sealed_history(APP, DOCUMENT)
                .first()
                .map(|op| op.signature)
                == Some(envelope.signature);
            if leads {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(field(&lounge, "title").await.as_deref(), Some("three"));
    assert_eq!(
        field(&lounge, "second").await.as_deref(),
        Some("kept"),
        "the write the copy lacked reads from the compaction"
    );
    drop((state, lounge.workspaces));
    lounge.server.shutdown().await;
}

/// A copy whose record keys have not arrived keeps a compaction it receives, in the retained
/// store, and a restart before the keys come loses nothing: a replay once the keys are here
/// takes it, and every write it folds reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one story: the copy's missing keys, its restart, and the replay that takes it"
)]
async fn a_copy_takes_a_compaction_it_kept_before_its_keys_after_a_restart() {
    let keys = std::sync::Arc::new(TestGroupKeys::default());
    let lounge = Box::new(lounge_holding(std::sync::Arc::clone(&keys)).await);
    let session = Box::pin(member_session(
        &lounge.server,
        &lounge.member_key,
        &lounge.member_cert,
        lounge.workspace_id,
        0,
    ))
    .await;
    // A copy that holds the first and third of the member's writes, and not the second.
    let (state, _second, compaction) = a_copy_without_the_second_write(&lounge, &session).await;
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: APP.to_owned(),
        cluster_id: lounge.workspace.clone(),
        document: DOCUMENT.to_owned(),
    };
    let record_keys = lounge
        .workspaces
        .own_route_keys(&route, true)
        .await
        .unwrap()
        .unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = &compaction else {
        panic!("a compaction is a document write");
    };
    let kept = envelope.signature;

    // The copy has not been let into the record's channel yet.
    keys.hold_nothing();
    session.send(compaction.clone());
    // A sealed frame the host cannot read is the retained store's, not a refusal.
    tilo_test_env::within("the copy keeps the compaction", async {
        loop {
            let retained = retained_writes(&lounge.server, lounge.workspace_id, DOCUMENT).await;
            if retained.iter().any(|op| op.signature == kept) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        !state
            .lock()
            .await
            .peer()
            .publication()
            .sealed_history(APP, DOCUMENT)
            .iter()
            .any(|op| op.signature == kept),
        "the copy takes nothing it cannot open yet"
    );

    // The copy restarts, with its keys still not here.
    drop((session, state));
    let Lounge {
        _dir: dir,
        server,
        workspaces,
        workspace,
        governor,
        governor_key,
        ..
    } = *lounge;
    drop(workspaces);
    server.shutdown().await;
    let restarted = test_host_holding(dir.path(), &governor_key, &governor, keys.clone()).await;
    let state = restarted.workspaces().workspace(&workspace).await.unwrap();
    restarted
        .workspaces()
        .recall(&mut *state.lock().await)
        .await
        .unwrap();

    // The keys arrive, and the record reads whole: the compaction the store kept is its base,
    // and every write it folds comes with it.
    keys.hand(&workspace, record_keys);
    restarted
        .workspaces()
        .recall(&mut *state.lock().await)
        .await
        .unwrap();
    assert_eq!(
        state
            .lock()
            .await
            .peer()
            .publication()
            .sealed_history(APP, DOCUMENT)
            .first()
            .map(|op| op.signature),
        Some(kept),
        "the compaction the store kept leads the copy's history"
    );
    let bytes = restarted
        .workspaces()
        .resource_document(&workspace, APP, DOCUMENT)
        .await
        .unwrap()
        .unwrap();
    let document = automerge::AutoCommit::load(&bytes).unwrap();
    let read = |name: &str| {
        document
            .get(ROOT, name)
            .ok()
            .flatten()
            .and_then(|(value, _)| value.into_string().ok())
    };
    assert_eq!(read("title").as_deref(), Some("three"));
    assert_eq!(
        read("second").as_deref(),
        Some("kept"),
        "the write the copy lacked, folded into the compaction, reads after the restart"
    );
    drop(state);
    restarted.shutdown().await;
}
