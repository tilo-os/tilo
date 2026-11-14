//! A public workspace's chain: a newcomer is replayed none of its parts, and
//! opening the chain's head sends the newest part and follows the chain.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the member's helpers are part of the test's assertions"
)]

use std::collections::BTreeSet;

use super::fixtures::{SealingMember, member_device};
use super::tests::{Lounge, lounge_with_client, seal_and_send_title, title};
use crate::CoeditPayload;
use tilo_relay_protocol::WorkspaceId;

/// The chain the members write, of the lounge's app.
const CHAIN: &str = "messages";

/// The retained channel a frame the host sent travels on, if it is a record's.
fn frame_channel(envelope: &crate::Envelope) -> Option<[u8; 32]> {
    let crate::Envelope::Delta { delta, .. } = envelope else {
        return None;
    };
    match CoeditPayload::decode(delta)? {
        CoeditPayload::Checkpoint { channel, .. }
        | CoeditPayload::RetainedDelta { channel, .. } => Some(channel),
    }
}

fn part_channel(app: &str, index: u64) -> [u8; 32] {
    neighbour_wire::app_document_channel(app, &neighbour_wire::record_part(CHAIN, index))
}

/// The member starts part `index` of the chain through its own session, as
/// a device's writes reach the host, until the host holds it.
async fn write_part(lounge: &Lounge, client: &crate::RelayClient, index: u64) {
    let record = neighbour_wire::record_part(CHAIN, index);
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
                .record_document(&lounge.workspace, super::tests::APP, &record)
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

/// What the host replays to `guest` until it is ready: the channels of the
/// records it sent, and whether it says records come when opened.
async fn replay(guest: &crate::RelayClient) -> (BTreeSet<[u8; 32]>, bool) {
    tilo_test_env::within("the guest's replay", async {
        let mut replayed = BTreeSet::new();
        loop {
            match guest.recv().await {
                Some(crate::Envelope::Ready {
                    records_when_opened,
                    ..
                }) => return (replayed, records_when_opened),
                Some(crate::Envelope::NotAuthorised { reason, .. }) => {
                    panic!("the guest was refused: {reason}")
                }
                Some(envelope) => replayed.extend(frame_channel(&envelope)),
                None => panic!("the host closed the guest's session"),
            }
        }
    })
    .await
}

/// `guest` opens the record at `head`: the channels of the records the host
/// sent before it answered, and the frames it says it sent.
async fn open_head(
    guest: &crate::RelayClient,
    workspace_id: WorkspaceId,
    head: [u8; 32],
) -> (BTreeSet<[u8; 32]>, u32) {
    guest
        .send(crate::Envelope::OpenRecord {
            workspace_id,
            channel: head,
        })
        .unwrap();
    tilo_test_env::within("the host answers the chain's head", async {
        let mut sent = BTreeSet::new();
        loop {
            match guest.recv().await {
                Some(crate::Envelope::RecordOpened {
                    channel, frames, ..
                }) if channel == head => return (sent, frames),
                Some(envelope) => sent.extend(frame_channel(&envelope)),
                None => panic!("the host closed the guest's session"),
            }
        }
    })
    .await
}

/// A chat's newest part was replayed to every newcomer, so a guest entering a
/// public workspace waited for all of it before its desktop; and a device that
/// holds no part found none to show and started a first part of its own. Now
/// the host keeps the chain for whoever opens its head, which brings the
/// newest part in one answer, and later heads as they start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_public_workspace_sends_a_chains_newest_part_to_who_opens_its_head() {
    let (lounge, client) = lounge_with_client().await;
    let app = super::tests::APP;
    let (workspaces, workspace, workspace_id) = (
        &lounge.workspaces,
        lounge.workspace.as_str(),
        lounge.workspace_id,
    );
    let (guest_key, guest_cert) = member_device(
        workspaces,
        workspace,
        (&lounge.governor_key, &lounge.governor),
        9,
    )
    .await;
    for index in 0..2 {
        write_part(&lounge, &client, index).await;
    }
    workspaces
        .publish(
            workspace,
            &lounge.governor,
            tilo_relay_protocol::directory::Listing {
                description: "Everyone".into(),
                default_role: tilo_relay_protocol::workspaces::Role::Editor,
            },
        )
        .await
        .unwrap();
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&guest_key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let guest = crate::RelayClient::connect_to_pinned_host(
        endpoint,
        lounge.relay.node_addr(),
        guest_cert,
        guest_key.clone(),
        tilo_user_identity::RevocationList::new(),
    )
    .await
    .unwrap();
    guest
        .send(crate::Envelope::Subscribe { workspace_id })
        .unwrap();
    let (replayed, when_opened) = replay(&guest).await;
    assert!(when_opened, "the guest is told records come when opened");
    for index in 0..2 {
        assert!(
            !replayed.contains(&part_channel(app, index)),
            "part {index} came with the replay"
        );
    }

    // Opening the chain's head: the newest part, and only it.
    let head = neighbour_wire::app_document_channel(app, &neighbour_wire::chain_head(CHAIN));
    let (sent, frames) = open_head(&guest, workspace_id, head).await;
    assert!(frames > 0, "the host sent the newest part's frames");
    assert_eq!(
        sent,
        BTreeSet::from([part_channel(app, 1)]),
        "the host sent the newest part alone"
    );

    // A part started after reaches the guest, which follows the chain.
    write_part(&lounge, &client, 2).await;
    tilo_test_env::within("the chain's next part reaches the guest", async {
        loop {
            match guest.recv().await {
                Some(envelope) if frame_channel(&envelope) == Some(part_channel(app, 2)) => {
                    return;
                }
                Some(_) => {}
                None => panic!("the host closed the guest's session"),
            }
        }
    })
    .await;
    drop((guest, client));
    let Lounge {
        relay, workspaces, ..
    } = lounge;
    drop(workspaces);
    relay.shutdown().await;
}

/// A chain with no part: its head is answered with nothing, which a device
/// takes as a chain to start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chain_with_no_part_answers_its_head_with_nothing() {
    let (lounge, guest) = lounge_with_client().await;
    let head = neighbour_wire::app_document_channel(
        super::tests::APP,
        &neighbour_wire::chain_head("nothing yet"),
    );
    guest
        .send(crate::Envelope::OpenRecord {
            workspace_id: lounge.workspace_id,
            channel: head,
        })
        .unwrap();
    let frames = tilo_test_env::within("the host answers the chain's head", async {
        loop {
            if let Some(crate::Envelope::RecordOpened {
                channel, frames, ..
            }) = guest.recv().await
                && channel == head
            {
                return frames;
            }
        }
    })
    .await;
    assert_eq!(frames, 0);
    drop(guest);
    let Lounge {
        relay, workspaces, ..
    } = lounge;
    drop(workspaces);
    relay.shutdown().await;
}
