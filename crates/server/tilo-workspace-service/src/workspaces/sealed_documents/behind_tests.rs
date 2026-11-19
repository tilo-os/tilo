#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the member's helpers are part of the test's assertions"
)]
//! A member's compaction the governor's copy is behind.

use super::tests::{
    APP, Lounge, follows, lounge_with_client, readable, retained_until, seal_and_send_title, title,
};
use automerge::{ROOT, transaction::Transactable as _};
use collab_types::{CommitNamed as _, SignedOp};
use neighbour_wire::NeighbourMsg;

const DOCUMENT: &str = "welcome";

/// A member writes `lines` as the document's title, sealed, and the node follows them: the
/// document's sealed writes the server keeps, in the order it admitted them.
async fn titles(lounge: &Lounge, client: &crate::ServerClient, lines: &[&str]) -> Vec<SignedOp> {
    for line in lines {
        seal_and_send_title(
            &lounge.member,
            client,
            &lounge.workspace,
            lounge.workspace_id,
            &lounge.member_key,
            line,
        );
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

/// A node told of a member's writes out of order passed the earlier one over for good, and
/// refused every compaction the member made of the document. Loading the workspace takes the
/// write the server keeps in again, and the member's compaction is taken.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_copy_that_passed_a_write_over_takes_it_in_again_on_load() {
    let (lounge, client) = lounge_with_client().await;
    titles(&lounge, &client, &["one"]).await;
    // The server keeps the second write, and its governor's copy is told only of the third.
    let ((), second) = lounge.member.seal(&lounge.workspace, |doc| {
        doc.put(ROOT, "title", "two").unwrap();
        doc.commit_edit("fixture", "title");
    });
    let author = lounge.member_key.verifying_key().to_bytes();
    lounge
        .server
        .workspace_documents()
        .carry(
            lounge.workspace_id,
            author,
            crate::workspace_documents::HostCommit {
                body: postcard::to_stdvec(&second).unwrap(),
                channel: neighbour_wire::retention(&second, false).unwrap().0,
                checkpoint: false,
            },
        )
        .await
        .unwrap();
    seal_and_send_title(
        &lounge.member,
        &client,
        &lounge.workspace,
        lounge.workspace_id,
        &lounge.member_key,
        "three",
    );
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
    assert!(
        state.lock().await.refuses(author, &compaction).is_some(),
        "a copy without the second write refuses the member's compaction"
    );

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
    assert_eq!(state.lock().await.refuses(author, &compaction), None);
    drop((state, lounge.workspaces));
    lounge.server.shutdown().await;
}

/// The node folds its copy of a document its members seal, as it does before the history fills,
/// and keeps the fold to itself. A member's compaction of the writes it holds is still taken,
/// in place of the node's fold.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_members_compaction_is_taken_after_the_node_folded_its_copy() {
    let (lounge, client) = lounge_with_client().await;
    let history = titles(&lounge, &client, &["one", "two", "three"]).await;
    let compaction = lounge
        .member
        .compacts(&lounge.workspaces, &lounge.workspace, history)
        .await;
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
    keys.compact(
        &mut folded,
        (APP, DOCUMENT),
        None,
        &lounge.governor_key,
        now,
    )
    .unwrap();
    let collaboration = governed.collaboration().clone();
    governed
        .commit(folded, &lounge.governor_key, now, |_, _| Ok(collaboration))
        .unwrap();
    drop(governed);
    assert_eq!(admits(&lounge, &compaction).await, None);

    let author = lounge.member_key.verifying_key().to_bytes();
    lounge
        .workspaces
        .absorb_kept(
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
        Some("three")
    );
    drop((state, lounge.workspaces));
    lounge.server.shutdown().await;
}
