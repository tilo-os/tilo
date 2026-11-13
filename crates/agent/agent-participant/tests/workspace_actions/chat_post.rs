//! A chat post costs what a post costs, however long the chat has run.
//!
//! A member's agent posts into the workspace's chat through the door a page
//! and the API server take: the action is addressed to the chat record's
//! provider, the host starts the chain's part for it and runs it for the
//! caller. What the host does for one post is its own work — the record it
//! acts on stays open, one action is resolved by address, what it wrote is
//! read from the record's own replica, and the parts of the chat that did not
//! change keep the rows they were read as — and never a pass over the history
//! the workspace already holds.
//!
//! The history is written the way a page writes it: one part, holding the
//! messages the deployed Lounge holds. The same agent then posts into that
//! chat, and into one holding nothing, and the two are compared.

use super::resource_support::{admit_person, open_admitted_peer};
use super::{
    ActionAddress, ActionHostError, Browser, DeviceCapabilities, DocumentRoute, InvokeArgument,
    ReceiptStatus, enrol, record_provider, start,
};
use std::time::{Duration, Instant};
use tilo_agent_participant::{ReplicaActor, Seed};
use tilo_peer_state::join::MemberRole;
use tilo_session_client::Outbox as _;

/// The messages the chat is grown to before the timed posts: a part of about
/// the three and a half megabytes the deployed Lounge holds.
const HISTORY: usize = 2_000;

/// The posts timed in each chat, after one that warms the part.
const MEASURED: usize = 10;

/// How much dearer a post may be into the full chat than into the empty one:
/// what the timing noise of a busy machine costs, not a cost that grows.
const BUDGET: f64 = 1.2;

/// The words a post says: about what a member writes, so the chat grows at
/// the rate a real one does.
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

/// One index's post into the chat, run and timed.
async fn post(
    poster: &mut Browser,
    provider: &str,
    messages: &ActionAddress,
    index: usize,
) -> Duration {
    let mut arguments = serde_json::Map::new();
    arguments.insert("channel".into(), serde_json::json!("c_chat"));
    arguments.insert("text".into(), serde_json::json!(words(index)));
    let began = Instant::now();
    // A host still settling a workspace it just started says so, and a save it
    // has not made yet is asked again: the same retry a member's agent makes
    // through the door.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let request = super::request(
            provider,
            messages.clone(),
            InvokeArgument::Json(arguments.clone()),
        );
        let receipt = poster.invoke(request).await;
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
            return began.elapsed();
        }
        assert!(
            Instant::now() < deadline,
            "post {index} never ran: {:?}",
            receipt.status
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// A post still costs more the longer the chat has run, though no longer for
// its sealed history: a device keeps the record it opened beside its keys, a
// read opens only the writes sealed since, and the host merges only the
// changes after the heads it holds. Measured: 162 ms to 195 ms into a chat
// holding at most ten messages, 231 ms into one holding forty, 259 ms to
// 283 ms into one holding two thousand. What is left grows in the host's app
// worker: the chat's own post action, about 35 ms more at two thousand
// messages, and the relayout of the chat view after each merged change, about
// 12 ms more. Ignored until a post is the same at both sizes.

#[ignore = "a post costs about 1.5 times as much into a chat of 2,000 messages: 184 ms and 283 ms"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_post_costs_the_same_however_long_the_chat_has_run() {
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Pace host", 1);
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Pace member", 1);
    let host = enrol(
        &owner,
        "pace-node",
        tilo_user_identity::DeviceCapabilities::host(),
    );
    let owner_device = enrol(&owner, "pace-owner-browser", DeviceCapabilities::member());
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
            &crate::common::catalogue(),
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
    let mut reporter =
        open_admitted_peer(&hosted, &guest_device, &host, &workspace.workspace).await;
    let chat = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: workspace.workspace.clone(),
        record: String::new(),
    };
    let provider = record_provider(&chat);
    let messages = ActionAddress::new(ui::record::chain_resource_id(&chat::parts::chain()), "post");

    // Into a chat holding nothing.
    let mut empty: Vec<Duration> = Vec::new();
    for index in 0..MEASURED {
        empty.push(post(&mut reporter, &provider, &messages, index).await);
    }

    // The chat grown to the Lounge's size: one part, written as a page of the
    // workspace starts it, holding the messages the deployed Lounge holds.
    let page = Browser::open_workspace(&hosted, &owner_device, &host, &workspace.workspace).await;
    let mut source = crate::common::open_in(
        &workspace.workspace,
        "so.tilo.chat",
        "ada",
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let mut chat_doc = source.read(Clone::clone).unwrap();
    crate::common::add_channel(&mut chat_doc, "ada", "general", None);
    let channel = crate::common::channel(&chat_doc, chat::model::ChannelKind::Text);
    source
        .apply(&collab_types::DocumentMutation::Snapshot(chat_doc.save()))
        .unwrap();
    let mut grown_part = chat::parts::ChatPart::default();
    for index in 0..HISTORY {
        grown_part.messages.push(
            ui::tilo_resource::Id::from_raw(format!("m{index}")),
            chat::record::Message {
                channel: channel.id.clone().into(),
                author: ui::tilo_resource::Atom("ada".to_owned()),
                ts: i64::try_from(index).unwrap_or(i64::MAX),
                // Short: one publication carries the part, and the host
                // reads every message of it all the same.
                text: format!("history {index}"),
                ..chat::record::Message::default()
            },
        );
    }
    let store = ui::record::RecordOwners::new(state.path().join("history"))
        .open("so.tilo.chat", &workspace.workspace)
        .unwrap();
    let base = std::rc::Rc::new(chat::demo::Workspace::of(chat_doc.clone()).read());
    let chain = ui::framework::Chain::<chat::parts::ChatPart>::detached_in(
        store,
        <chat::ChatShell as ui::framework::ResourceApp>::ID,
        "ada",
        move || std::rc::Rc::clone(&base),
    )
    .unwrap();
    chain.write_start(0, &grown_part).unwrap();
    let written = chain.part_bytes(0).expect("the history's part is held");
    page.peer
        .write(
            DocumentRoute {
                app_id: "so.tilo.chat".into(),
                cluster_id: workspace.workspace.clone(),
                record: chat::parts::part(0).route().to_owned(),
            },
            &collab_types::DocumentMutation::Snapshot(written),
        )
        .unwrap();
    page.durable().await;

    // The same agent, posting into that.
    let mut grown: Vec<Duration> = Vec::new();
    for index in HISTORY..HISTORY.saturating_add(MEASURED) {
        grown.push(post(&mut reporter, &provider, &messages, index).await);
    }

    let fresh = median(&empty);
    let long = median(&grown);
    println!(
        "chat post pace: {:.0} ms into a chat holding nothing, {:.0} ms into one holding \
         {HISTORY} messages",
        fresh.as_secs_f64() * 1000.0,
        long.as_secs_f64() * 1000.0
    );
    assert!(
        long <= fresh.mul_f64(BUDGET) + Duration::from_millis(20),
        "a post costs {:.0} ms into an empty chat and {:.0} ms into one holding {HISTORY} \
         messages: it grows with the chat",
        fresh.as_secs_f64() * 1000.0,
        long.as_secs_f64() * 1000.0
    );
}
