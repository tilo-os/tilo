//! A chat post runs through the host, and what the host does for it is its
//! own work.
//!
//! A member's agent posts into the workspace's chat through the door a page
//! and the front door take: the action is addressed to the chat record's
//! provider, the host starts the chain's part for it and runs it for the
//! caller. What the host does for one post is kept between posts — the
//! record it acts on stays open, one action is resolved by address, and what
//! it wrote is read from the record's state — and never a pass over the
//! history the workspace already holds.
//!
//! What still grows here is the app's own work: a chat re-reads its channels
//! when it changes, so a post costs more the more messages the chat holds.

use super::resource_support::{admit_person, open_admitted_peer};
use super::{
    ActionAddress, ActionHostError, DeviceCapabilities, DocumentRoute, InvokeArgument,
    ReceiptStatus, common, enrol, record_provider, start,
};
use std::time::{Duration, Instant};
use tilo_peer_state::join::MemberRole;

/// How many messages the chat holds by the end: what one post costs is
/// compared between the tenth and the last ten.
const MESSAGES: usize = 40;

/// The words a post says: about what the Lounge's history holds, so the chat
/// grows at the rate a real one does.
fn words(index: usize) -> String {
    const WORDS: &[&str] = &[
        "tilo",
        "lounge",
        "workspace",
        "desk",
        "window",
        "share",
        "agent",
        "record",
        "history",
        "people",
        "call",
        "notes",
        "sheets",
        "music",
        "photos",
        "relay",
        "door",
        "browser",
        "terminal",
        "build",
        "release",
        "sync",
        "device",
    ];
    let count = 20_usize.saturating_add(index % 381);
    let mut said = format!("#{index}");
    for step in 0..count {
        said.push(' ');
        let at = index
            .saturating_mul(7)
            .saturating_add(step.saturating_mul(13))
            .checked_rem(WORDS.len())
            .unwrap_or_default();
        said.push_str(WORDS.get(at).unwrap_or(&"tilo"));
    }
    said
}

fn median(times: &[Duration]) -> Duration {
    let mut sorted: Vec<Duration> = times.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_post_does_not_cost_more_as_the_chat_grows() {
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Pace host", 1);
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Pace member", 1);
    let host = enrol(
        &owner,
        "pace-node",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let guest_device = enrol(&guest, "pace-guest-browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&owner, &host, state.path()).await;
    let workspaces = hosted.relay.workspaces().unwrap();
    let workspace = workspaces.create("Paced workspace").await.unwrap();
    // The workspace's chat, as an operator seeds it when the workspace is made
    // (`--seed-app so.tilo.chat`): its record with the channels every chat
    // opens with, and no message yet.
    workspaces
        .prepare_application(
            &workspace.workspace,
            &host.cert,
            &common::catalogue(),
            "so.tilo.chat",
            "",
        )
        .await
        .unwrap();
    admit_person(
        &hosted,
        &host,
        &workspace.workspace,
        &guest_device,
        MemberRole::Editor,
    )
    .await;
    let mut poster = open_admitted_peer(&hosted, &guest_device, &host, &workspace.workspace).await;

    let chat = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: workspace.workspace.clone(),
        record: String::new(),
    };
    let provider = record_provider(&chat);
    let messages = ActionAddress::new(ui::record::chain_resource_id(&chat::parts::chain()), "post");
    let post = |index: usize| {
        let mut arguments = serde_json::Map::new();
        arguments.insert("channel".into(), serde_json::json!("c_chat"));
        arguments.insert("text".into(), serde_json::json!(words(index)));
        super::request(&provider, messages.clone(), InvokeArgument::Json(arguments))
    };

    let mut times: Vec<Duration> = Vec::with_capacity(MESSAGES);
    for index in 0..MESSAGES {
        let began = Instant::now();
        // A host still settling a workspace it just started says so, and a
        // save it has not made yet is asked again: the same retry a member's
        // agent makes through the door.
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let receipt = poster.invoke(post(index)).await;
            let again = matches!(
                &receipt.status,
                ReceiptStatus::Rejected {
                    error: ActionHostError::Unavailable { .. }
                }
            ) || matches!(&receipt.status, ReceiptStatus::Failed { error } if error.retryable);
            assert!(
                again || matches!(receipt.status, ReceiptStatus::Completed { .. }),
                "post {index} ran: {:?}",
                receipt.status
            );
            if !again {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "post {index} never ran: {:?}",
                receipt.status
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        times.push(began.elapsed());
    }

    // What one post costs once the chat has some history, and what it costs
    // with all of it: the tenth against the last ten. Both are measured in one
    // run on the same machine, so its load does not read as growth.
    let early = median(&times[5..15]);
    let late = median(&times[MESSAGES - 10..]);
    println!(
        "chat post pace: {MESSAGES} messages, the tenth took {:.0} ms, the last ten {:.0} ms",
        early.as_secs_f64() * 1000.0,
        late.as_secs_f64() * 1000.0
    );
    assert!(
        late <= early * 2 + Duration::from_millis(20),
        "a post costs {:.0} ms at message 10 and {:.0} ms at message {MESSAGES}: the host pays \
         for the history the workspace holds",
        early.as_secs_f64() * 1000.0,
        late.as_secs_f64() * 1000.0
    );
    // The posts are in the part the chat's chain keeps them in, which the
    // caller received as the host published it.
    let part = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: workspace.workspace.clone(),
        record: chat::parts::part(0).as_str().to_owned(),
    };
    assert!(
        poster.documents.contains_key(&part),
        "the posts are in the chat's first part"
    );
}
