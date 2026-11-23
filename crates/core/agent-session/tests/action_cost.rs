//! What an action on a feed nobody watches costs: its own work, never a frame
//! of the app's view.
//!
//! The times are printed for reading (`cargo test -p tilo-agent-session
//! action_cost -- --nocapture`), never asserted as exact durations: what is
//! asserted is that an action lays no view out, whether the feed holds two
//! hundred articles or one.

use std::time::{Duration, Instant};

use tilo_actions::MemberId;
use tilo_actions::api::ActionAddress;
use tilo_agent_session::{Replica, ReplicaActor, Seed};
use ui::tilo_resource::profiling::observe;

/// The articles a feed holds before its actions are timed.
const ARTICLES: usize = 200;

/// The actions timed on it.
const ACTIONS: usize = 30;

/// The subscription the articles come from.
const FEED: &str = "feed-1";

/// The address the feed publishes its articles under.
fn articles() -> String {
    ui::resource::chain_resource_id(&feeds::article_list::chain())
}

/// An article as a refresh brings it: a title and a summary of a few lines.
fn fetched(at: usize) -> serde_json::Value {
    serde_json::json!({
        "title": format!("Article {at}"),
        "url": format!("https://example.test/{at}"),
        "summary": "The service keeps its work beside its peers. ".repeat(20),
    })
}

/// Land articles `from..to` through the chain's own `receive`.
fn receive(page: &mut Replica, address: &ActionAddress, from: usize, to: usize) {
    let list: Vec<serde_json::Value> = (from..to).map(fetched).collect();
    page.invoke(
        address,
        serde_json::json!({"feed": FEED, "articles": list})
            .as_object()
            .unwrap(),
        false,
    )
    .expect("the articles land");
}

/// Make one folder on the library itself, which changes none of its articles:
/// what an agent does to a feed between the articles it reads.
fn folder(page: &mut Replica, address: &ActionAddress, number: usize) {
    page.invoke(
        address,
        serde_json::json!({"name": format!("Folder {number}")})
            .as_object()
            .unwrap(),
        false,
    )
    .expect("the folder is made");
}

fn median(mut took: Vec<Duration>) -> Duration {
    took.sort();
    took.get(took.len() / 2).copied().unwrap_or_default()
}

/// The frames `observed` built.
fn layouts(observed: &[ui::tilo_resource::profiling::Sample]) -> usize {
    observed
        .iter()
        .filter(|phase| phase.phase == "ui.layout")
        .map(|phase| phase.calls)
        .sum()
}

/// A feed nobody watches answers its actions without laying its view out: the
/// work of the action lands before it answers, and a feed holding many
/// articles costs what the one action does, not a frame of all of them.
#[test]
fn an_action_on_a_feed_nobody_watches_lays_nothing_out() {
    let mut page = crate::common::open(
        "so.tilo.feeds",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let receive_address = ActionAddress::new(articles(), "receive");
    for start in (0..ARTICLES).step_by(100) {
        receive(&mut page, &receive_address, start, (start + 100).min(ARTICLES));
    }
    assert!(
        page.survey_rows()
            .iter()
            .filter(|row| row.parent_resource == articles())
            .count()
            >= ARTICLES,
        "the feed holds every article landed through it"
    );

    // The first actions of a view pay what opening it left owed; the steady
    // state an agent moving a feed in sees is what is timed.
    let library = page
        .resources()
        .into_iter()
        .find(|resource| resource.kind == "feeds.library")
        .map(|resource| ActionAddress::new(resource.id.as_str(), "new-folder"))
        .expect("the library is the only one it holds");
    let rows_before = page.survey_rows().len();
    let mut took = Vec::with_capacity(ACTIONS);
    let ((), acting) = observe(|| {
        for number in 0..5 {
            folder(&mut page, &library, number);
        }
        for number in 0..ACTIONS {
            let started = Instant::now();
            folder(&mut page, &library, 5 + number);
            took.push(started.elapsed());
        }
    });
    assert_eq!(
        layouts(&acting),
        0,
        "an action on a feed nobody watches builds no frame: {acting:?}"
    );
    println!(
        "a feed of {ARTICLES} articles: {ACTIONS} actions, median {:?}, total {:?}",
        median(took.clone()),
        took.iter().sum::<Duration>()
    );
    let rows_after = page.survey_rows().len();
    println!("rows before {rows_before}, after {rows_after}");
    assert!(
        rows_after > rows_before,
        "every action left what it made ({rows_before} then {rows_after})"
    );
}
