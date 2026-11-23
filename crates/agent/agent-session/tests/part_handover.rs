//! A list that starts a chain part hands it to its host as the changes it made:
//! the host admits them without loading the part again, as it does a list that
//! only adds to a part already started.
//!
//! A chat keeps its messages in numbered parts. A group posts a hundred
//! messages at a time, and every other list fills the part and starts the next.
//! A part the worker starts used to come to the host as a saved document, which
//! the host loaded in a worker and again here; the load here is counted.

use collab_types::DocumentMutation;
use serde_json::{Value, json};
use tilo_actions::MemberId;
use tilo_actions::api::ActionAddress;
use tilo_agent_session::{ReplicaActor, Seed};

/// The messages one list posts, as an import hands them over.
const LIST: usize = 100;

/// The lists posted: enough for a part to fill twice over, so some start a part
/// and some only add to the one before.
const LISTS: usize = 8;

/// The chat's channel the posts go in.
const CHANNEL: &str = "c_chat";

/// The address the chat publishes its messages under.
fn messages() -> String {
    ui::resource::chain_resource_id(&chat::parts::chain())
}

/// A message as an import brings one: a couple of kilobytes of words.
fn imported(index: usize) -> Value {
    let said = "What was seen, what was tried, and what should happen next. ".repeat(40);
    json!({
        "id": format!("m{index}"),
        "channel": CHANNEL,
        "text": format!("Message {index}: {said}"),
    })
}

/// One list of the import, posted into the chat: whether it started a part,
/// and how many documents the host loaded of it.
fn post_list(held: &tilo_agent_session::ConfinedReplica, list: usize) -> (bool, usize) {
    let group = ActionAddress::new(messages(), "group");
    let actions: Vec<Value> = (list * LIST..(list + 1) * LIST)
        .map(|index| json!({"action": "post", "args": imported(index)}))
        .collect();
    held.invoke(
        &group,
        json!({ "actions": actions }).as_object().unwrap(),
        false,
    )
    .expect("the list is posted");
    tilo_automerge::shared::clear();
    let taken = held
        .take_document_mutations()
        .expect("the list's parts are handed over");
    let decoded = tilo_automerge::shared::decoded();
    // A part the host holds none of comes whole: what founds it at a peer.
    let starting = taken.iter().any(|(_, mutation)| {
        matches!(mutation, DocumentMutation::Snapshot(bytes)
            if tilo_automerge_isolate::document_heads(bytes).is_some())
    });
    (starting, decoded)
}

#[test]
fn a_list_that_starts_a_part_is_not_loaded_again_by_its_host() {
    let mut page = crate::common::open(
        "so.tilo.chat",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    page.invoke(
        &ActionAddress::new("chat.workspace", "create-channel-with-id"),
        json!({"id": CHANNEL, "name": "chat", "category": null,
            "kind": null, "topic": null, "members": null})
        .as_object()
        .unwrap(),
        false,
    )
    .expect("the chat takes a channel");
    let Some(DocumentMutation::Snapshot(chat)) = page.take_mutation().unwrap() else {
        panic!("a new chat is published whole");
    };
    let held = crate::common::confined(
        "so.tilo.chat",
        &MemberId::person("host"),
        &ReplicaActor::Owner,
        Seed::Existing {
            record: page.document_id().clone(),
            changes: vec![chat],
        },
    )
    .expect("the chat opens in its worker");

    let mut started = Vec::new();
    let mut continued = Vec::new();
    for list in 0..LISTS {
        let (starting, decoded) = post_list(&held, list);
        if starting {
            started.push((list, decoded));
        } else {
            continued.push((list, decoded));
        }
    }
    eprintln!("lists that started a part (list, decoded): {started:?}");
    eprintln!("lists that added to one (list, decoded): {continued:?}");
    assert!(
        !started.is_empty() && !continued.is_empty(),
        "the lists both start a part and add to one: {started:?} {continued:?}"
    );
    let loading =
        |lists: &[(usize, usize)]| lists.iter().map(|(_, decoded)| *decoded).sum::<usize>();
    assert!(
        loading(&started) <= loading(&continued),
        "a list that starts a part loads it {} times, a list that adds to one {}: the part is \
         handed over as a saved document its host decodes again",
        loading(&started),
        loading(&continued)
    );
}
