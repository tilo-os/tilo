//! A public workspace's chain: a device that links holds the parts the chain
//! started, and the newest part reaches it live as the chain grows.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the member's helpers are part of the test's assertions"
)]

use super::fixtures::SealingMember;
use super::tests::{Lounge, Peer, lounge_with_client, seal_and_send_title, title};
use neighbour_wire::NeighbourMsg;

/// The chain the members write, of the lounge's app.
const CHAIN: &str = "messages";

/// The member starts part `index` of the chain through its own session, as
/// a device's writes reach the host, until the host holds it.
async fn write_part(lounge: &Lounge, client: &crate::ServerClient, index: u64) {
    let record = neighbour_wire::document_part(CHAIN, index);
    let part: SealingMember = lounge
        .member
        .part(&lounge.workspaces, &lounge.workspace, &record)
        .await;
    let line = format!("part {index}");
    seal_and_send_title(
        &part,
        client,
        &lounge.workspace,
        lounge.workspace_id,
        &lounge.member_key,
        &line,
    );
    tilo_test_env::within("the host holds the part", async {
        loop {
            let held = lounge
                .workspaces
                .resource_document(&lounge.workspace, super::tests::APP, &record)
                .await
                .unwrap();
            if held.as_deref().and_then(title).as_deref() == Some(line.as_str()) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_public_workspace_sends_a_chains_newest_part_to_who_opens_its_head() {
    let (lounge, client) = lounge_with_client().await;
    let app = super::tests::APP;
    for index in 0..2 {
        write_part(&lounge, &client, index).await;
    }
    assert!(
        lounge.workspaces.chain_head_has_part(
            &lounge.workspace,
            app,
            &neighbour_wire::chain_head(CHAIN)
        ),
        "the chain has restarted a part, so a head is answered with one"
    );
    // A device that links now holds every part the chain started.
    let mut peer = Peer::link(&lounge, 9).await;
    for index in 0..2 {
        let record = neighbour_wire::document_part(CHAIN, index);
        assert!(
            peer.link.read(|held| held.document(app, &record).is_some()),
            "a device that caught up holds part {index}: its app reads it, and does not make it again"
        );
    }
    // A part started after reaches the linked device as it starts.
    write_part(&lounge, &client, 2).await;
    let next = neighbour_wire::document_part(CHAIN, 2);
    tilo_test_env::within("the chain's next part reaches the linked device", async {
        loop {
            let frame = peer.heard.recv().await.unwrap();
            if let NeighbourMsg::AppDocument { document_id, .. } = &frame.message
                && document_id == &next
            {
                return;
            }
        }
    })
    .await;
    peer.pairing.abort();
    drop((client, peer));
    let Lounge {
        server, workspaces, ..
    } = lounge;
    drop(workspaces);
    server.shutdown().await;
}

/// A chain with no part: its head is answered with nothing, which a device
/// takes as a chain to start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chain_with_no_part_answers_its_head_with_nothing() {
    let (lounge, client) = lounge_with_client().await;
    let head = neighbour_wire::chain_head("nothing yet");
    assert!(
        !lounge
            .workspaces
            .chain_head_has_part(&lounge.workspace, super::tests::APP, &head),
        "a chain with no part has none: its head is answered with nothing, a chain to start"
    );
    // The first part the chain starts makes its head answer with it.
    write_part(&lounge, &client, 0).await;
    assert!(
        lounge.workspaces.chain_head_has_part(
            &lounge.workspace,
            super::tests::APP,
            &neighbour_wire::chain_head(CHAIN)
        ),
        "the part the chain started is held, and its head answers with it"
    );
    drop(client);
    let Lounge {
        server, workspaces, ..
    } = lounge;
    drop(workspaces);
    server.shutdown().await;
}
