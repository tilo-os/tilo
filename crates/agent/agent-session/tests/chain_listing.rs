//! A page of a chain's listing costs its page, however many items the chain
//! holds, right after a write as at any other time.
//!
//! A chain keeps its items in numbered parts, and a write changes one of
//! them. A listing of the chain's level read the tree the whole view
//! publishes, which a write leaves to be laid out and built again: every
//! item of every part, so the first page after each write of a chat of ten
//! thousand cost seconds, and the writes waiting behind it waited as long.
//! The level is walked as the parts publish it instead, building the part
//! that changed and the items the page shows.
//!
//! A chat keeps its messages in a chain; any app's chain lists the same way.
//! The times are printed for reading; what is asserted is that the view's
//! tree is never built for a page, and that a page at ten thousand costs
//! within a small factor of one at two hundred.


use std::time::{Duration, Instant};

use tilo_actions::MemberId;
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::CatalogueQuery;
use tilo_agent_session::{Replica, ReplicaActor, Seed};
use ui::tilo_resource::profiling::observe;

/// The chats listed: a small one, and one of ten thousand messages.
const SIZES: [usize; 2] = [200, 10_000];

/// The messages one call posts: an import's list.
const LIST: usize = 100;

/// The resources a page shows, as an agent's listing asks for them.
const PAGE: u32 = 40;

/// The writes each followed by a page.
const ROUNDS: usize = 5;

/// The address the chat publishes its messages under.
fn messages() -> String {
    ui::resource::chain_resource_id(&chat::parts::chain())
}

/// A chat with the channel the posts go in.
fn founded() -> Replica {
    let mut chat = crate::common::open(
        "so.tilo.chat",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    chat.invoke(
        &ActionAddress::new("chat.workspace", "create-channel-with-id"),
        serde_json::json!({"id": "c_chat", "name": "chat", "category": null,
            "kind": null, "topic": null, "members": null})
        .as_object()
        .unwrap(),
        false,
    )
    .expect("the chat takes a channel");
    chat
}

/// Post messages `from..to`, as one change: a message of a couple of
/// kilobytes, so a part holds some dozens and a chat of thousands is kept in
/// many.
fn post(chat: &mut Replica, from: usize, to: usize) {
    let posts: Vec<serde_json::Value> = (from..to)
        .map(|at| {
            serde_json::json!({"action": "post", "args": {
                "channel": "c_chat",
                "text": format!("Message {at}. {}", "The service keeps its work beside its peers. ".repeat(40)),
            }})
        })
        .collect();
    chat.invoke(
        &ActionAddress::new(messages(), "group"),
        serde_json::json!({ "actions": posts }).as_object().unwrap(),
        false,
    )
    .expect("the messages are posted");
}

/// The first page of the chat as an agent lists it: a resource at a time, up
/// to one past the page, which says the listing goes on.
fn first_page(chat: &Replica) -> usize {
    let query = CatalogueQuery {
        parent_resource: Some(messages()),
        max_results: tilo_actions::remote::MAX_BY_RESOURCE_PAGE,
        max_resources: Some(PAGE),
        by_resource: true,
        ..CatalogueQuery::default()
    };
    let take = usize::try_from(tilo_actions::remote::MAX_BY_RESOURCE_PAGE).unwrap();
    let (_, rows) = chat.survey_page(&query, (false, (0, take)));
    rows.iter()
        .map(|row| row.resource.id.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len()
}

fn median(mut took: Vec<Duration>) -> Duration {
    took.sort();
    took.get(took.len() / 2).copied().unwrap_or_default()
}

#[test]
fn a_page_after_a_write_costs_the_same_however_many_items_the_chain_holds() {
    let mut pages = Vec::new();
    let mut built = Vec::new();
    for size in SIZES {
        let mut chat = founded();
        for from in (0..size).step_by(LIST) {
            post(&mut chat, from, (from + LIST).min(size));
        }
        assert_eq!(
            first_page(&chat),
            usize::try_from(PAGE).unwrap() + 1,
            "a page of {size} holds what it was asked for and one past it"
        );
        let mut took = Vec::new();
        let mut trees = 0;
        for round in 0..ROUNDS {
            post(&mut chat, size + round, size + round + 1);
            let began = Instant::now();
            let (listed, observed) = observe(|| first_page(&chat));
            took.push(began.elapsed());
            assert_eq!(listed, usize::try_from(PAGE).unwrap() + 1);
            trees += observed
                .iter()
                .filter(|sample| sample.phase == "framework.published_tree")
                .map(|sample| sample.calls)
                .sum::<usize>();
        }
        let page = median(took);
        eprintln!(
            "{size} messages: a page after a write takes {page:?}, built the view's tree {trees} \
             times in {ROUNDS}"
        );
        pages.push(page);
        built.push(trees);
    }
    let (small, large) = (pages[0], pages[1]);
    assert!(
        large <= small * 4 + Duration::from_millis(40),
        "a page after a write takes {large:?} at {} messages and {small:?} at {}",
        SIZES[1],
        SIZES[0]
    );
    assert_eq!(
        built,
        [0, 0],
        "a page after a write built the tree the view publishes, at {SIZES:?} messages"
    );
}
