#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the member's helpers are part of the test's assertions"
)]
//! A compaction replaces the writes of every author it folds, not only its own.

use super::super::Workspaces;
use super::fixtures::SealingMember;
use super::tests::{
    APP, Lounge, RESOURCE, follows_field, lounge_with_client, newcomer_opens, retained_writes,
    send_members_delta, send_sealed,
};
use automerge::{ROOT, ReadDoc as _, transaction::Transactable as _};
use collab_types::{CommitNamed as _, SignedOp};
use neighbour_wire::NeighbourMsg;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

/// The body of every retained write of `resource`, once the workspace's store
/// settled every write its owner took before the call.
async fn retained_write_bodies(
    documents: &crate::workspace_documents::WorkspaceDocuments,
    workspace_id: WorkspaceId,
    resource: &str,
) -> Vec<Vec<u8>> {
    documents
        .retained_settled(workspace_id)
        .await
        .unwrap()
        .into_iter()
        .filter(|retained| {
            matches!(
                postcard::from_bytes::<NeighbourMsg>(&retained.body),
                Ok(NeighbourMsg::AppDocument { document_id, .. }) if document_id == resource
            )
        })
        .map(|retained| retained.body)
        .collect()
}

/// The governor writes `field` of the resource sealed, as it does for its
/// members, `to` times.
async fn governor_writes_sealed(
    workspaces: &Workspaces,
    workspace: &str,
    governor: &DeviceCert,
    field: &str,
    to: usize,
) {
    for index in 0..to {
        workspaces
            .author_workspace_document(workspace, governor, (APP, RESOURCE), |document| {
                document
                    .put(ROOT, field, format!("{field} {index}"))
                    .map_err(|error| error.to_string())
            })
            .await
            .unwrap();
    }
}

/// `member` writes `field` of the resource for each index in `from..to`,
/// sealed and sent to the server as its own writes travel.
fn send_member_fields(
    member: &SealingMember,
    client: &crate::ServerClient,
    workspace: &str,
    workspace_id: WorkspaceId,
    key: &ed25519_dalek::SigningKey,
    field: &str,
    from: usize,
    to: usize,
) {
    for index in from..to {
        send_member_field(
            member,
            client,
            workspace,
            workspace_id,
            key,
            field,
            &format!("{field} {index}"),
        );
    }
}

/// `member` writes `line` to `field` of the resource and sends it to the
/// server sealed, as the member's own writes travel.
fn send_member_field(
    member: &SealingMember,
    client: &crate::ServerClient,
    workspace: &str,
    workspace_id: WorkspaceId,
    key: &ed25519_dalek::SigningKey,
    field: &str,
    line: &str,
) {
    let ((), message) = member.seal(workspace, |document| {
        document.put(ROOT, field, line).unwrap();
        document.commit_edit("fixture", field);
    });
    send_sealed(client, workspace_id, key, &message);
}

/// The governor and `member` each write the resource sealed, and the node
/// follows both: the governor's writes as it makes them, the member's as its
/// own writes travel.
async fn both_authors_write(
    (workspaces, workspace, workspace_id): (&Workspaces, &str, WorkspaceId),
    (governor, member, key, client): (
        &DeviceCert,
        &SealingMember,
        &ed25519_dalek::SigningKey,
        &crate::ServerClient,
    ),
    writes: usize,
) {
    governor_writes_sealed(workspaces, workspace, governor, "governor", writes).await;
    send_member_fields(
        member,
        client,
        workspace,
        workspace_id,
        key,
        "member",
        0,
        writes,
    );
    follows_field(
        workspaces,
        workspace,
        "governor",
        &format!("governor {}", writes.saturating_sub(1)),
    )
    .await;
    follows_field(
        workspaces,
        workspace,
        "member",
        &format!("member {}", writes.saturating_sub(1)),
    )
    .await;
}

/// `field` of the resource a newcomer opening `held` in order reads.
fn newcomer_field(member: &SealingMember, held: &[SignedOp], field: &str) -> Option<String> {
    newcomer_opens(member, held)
        .get(ROOT, field)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

/// A resource the governor and a member both write sealed is folded by the
/// member into one checkpoint. The checkpoint replaces the governor's writes
/// as well as the member's: a newcomer is sent the checkpoint and the writes
/// after it, not the writes it already holds, and it opens to the resource
/// both of them wrote.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_compaction_folds_the_writes_of_every_author_a_newcomer_replays() {
    const GOVERNOR_WRITES: usize = 12;
    const MEMBER_WRITES: usize = 12;
    const AFTER: usize = 3;
    let (
        Lounge {
            _dir,
            server,
            workspaces,
            workspace,
            workspace_id,
            governor,
            member_key,
            member,
            ..
        },
        client,
    ) = lounge_with_client().await;
    // The governor writes the resource sealed, as it does for its members,
    // and the member does too.
    both_authors_write(
        (&workspaces, &workspace, workspace_id),
        (&governor, &member, &member_key, &client),
        MEMBER_WRITES,
    )
    .await;
    let before_writes =
        retained_write_bodies(&server.workspace_documents(), workspace_id, RESOURCE).await;
    assert!(
        before_writes.len() >= GOVERNOR_WRITES + MEMBER_WRITES,
        "the node retains every write a newcomer replays: {}",
        before_writes.len()
    );

    // The member folds the history it holds, the governor's writes included,
    // into one checkpoint and publishes it in place of every write it covers.
    let [compaction]: [NeighbourMsg; 1] = member
        .compact(&workspaces, &workspace)
        .await
        .try_into()
        .unwrap();
    send_members_delta(
        &client,
        workspace_id,
        &member_key,
        &crate::CoeditPayload::Checkpoint {
            channel: neighbour_wire::retention(&compaction, false).unwrap().0,
            body: postcard::to_stdvec(&compaction).unwrap(),
        },
    );
    // What a newcomer is sent past the checkpoint travels as itself.
    send_member_fields(
        &member,
        &client,
        &workspace,
        workspace_id,
        &member_key,
        "member",
        MEMBER_WRITES,
        MEMBER_WRITES + AFTER,
    );
    let newest = format!("member {}", MEMBER_WRITES + AFTER - 1);
    let member_device = member_key.verifying_key().to_bytes();
    let held = tilo_test_env::within(
        "the node retains the checkpoint and the writes after it",
        async {
            loop {
                let held = retained_writes(&server, workspace_id, RESOURCE).await;
                let folded = held.iter().any(|op| {
                    op.author.as_bytes() == &member_device
                        && op.kind == collab_types::OpKind::Compaction
                });
                let after = held.iter().any(|op| {
                    op.author.as_bytes() == &member_device
                        && op.kind != collab_types::OpKind::Compaction
                        && op.seq == (MEMBER_WRITES + AFTER) as u64
                });
                if folded && after {
                    break held;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        },
    )
    .await;
    assert_folded_for_a_newcomer(
        &member,
        (member_device, governor.device_public_key),
        &compaction,
        &held,
        (&newest, "governor 11"),
    );
    let after_writes =
        retained_write_bodies(&server.workspace_documents(), workspace_id, RESOURCE).await;
    assert_sent_less(&before_writes, &after_writes);
    drop(workspaces);
    server.shutdown().await;
}

/// Asserts that the checkpoint stands in for the writes it folds: `after` is
/// less than half the bytes of `before` in fewer writes.
fn assert_sent_less(before: &[Vec<u8>], after: &[Vec<u8>]) {
    let (before_bytes, after_bytes) = (
        before.iter().map(Vec::len).sum::<usize>(),
        after.iter().map(Vec::len).sum::<usize>(),
    );
    assert!(
        after_bytes.saturating_mul(2) < before_bytes,
        "a newcomer is sent the checkpoint in place of the writes it folds: \
         {before_bytes} bytes in {} writes before, {after_bytes} bytes in {} writes after",
        before.len(),
        after.len()
    );
}

/// Asserts that `held`, the writes a newcomer replays, holds none the
/// checkpoint `compaction` covers, whoever wrote it, and opens to the resource
/// as both of its authors left it.
fn assert_folded_for_a_newcomer(
    member: &SealingMember,
    (member_device, governor): ([u8; 32], [u8; 32]),
    compaction: &NeighbourMsg,
    held: &[SignedOp],
    (newest, governor_last): (&str, &str),
) {
    assert_eq!(
        newcomer_field(member, held, "member").as_deref(),
        Some(newest),
        "the resource opens to what the member wrote last"
    );
    assert_eq!(
        newcomer_field(member, held, "governor").as_deref(),
        Some(governor_last),
        "the resource opens to what the governor wrote"
    );
    assert!(
        held.iter().all(|op| op.author.as_bytes() != &governor),
        "a governor write the checkpoint covers stays for a newcomer: {} writes",
        held.len()
    );
    let NeighbourMsg::AppDocument { envelope, .. } = compaction else {
        panic!("a compaction is a document write")
    };
    let covers = collab_types::SealedCompaction::parse(envelope)
        .unwrap()
        .covers;
    for op in held {
        if op.kind == collab_types::OpKind::Compaction {
            continue;
        }
        let floor = covers
            .iter()
            .find(|(author, _)| author.as_bytes() == op.author.as_bytes())
            .map_or(0, |(_, floor)| *floor);
        assert!(
            op.seq > floor,
            "a write the checkpoint covers stays for a newcomer: {}#{}",
            hex::encode(op.author.as_bytes()),
            op.seq
        );
    }
    assert!(
        held.iter().any(|op| op.author.as_bytes() == &member_device
            && op.kind != collab_types::OpKind::Compaction),
        "the writes after the checkpoint reach a newcomer"
    );
}
