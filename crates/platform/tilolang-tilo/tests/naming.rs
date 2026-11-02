//! What an item is called: a resource names itself by a text of its own — its `title` field,
//! or its `body`'s first line — never by a text of something it holds. A channel called
//! "design" whose messages say "The new onboarding build is on the test machines." is
//! "design". Read from its first message instead, it shared its name with that message, and a
//! search list showed two rows a person could not tell apart (the two "Open" rows of a hit and
//! its channel).

use std::sync::Arc;

use automerge::AutoCommit;
use tilolang_runtime::Principal;
use tilolang_tilo::publish::{PlanRequest, Planner, Publications};

use crate::fixtures::Replica;

const CHAT: (&str, &str) = (
    include_str!("../../../../apps/tl/chat/app.tl"),
    include_str!("../../../../apps/tl/chat/app.ids"),
);
const APP: &str = "so.tilo.lang.chat";

struct Refusing;

impl Planner for Refusing {
    fn plan(&self, _: &PlanRequest<'_>) -> Result<tilolang_tilo::action::DynAction, String> {
        Err("nothing runs while this test publishes".into())
    }
}

/// The seeded Chat workspace: the team's channels and their messages, as its first scenario makes
/// them, and the root record's id.
fn seeded_chat() -> (Replica, String) {
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[CHAT]);
    let meta = loaded.last().unwrap().meta.clone();
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    let scenario = meta.scenarios.first().expect("Chat declares a scenario");
    ada.host
        .runtime()
        .invoke_action(&Principal::new("ada"), (scenario.action.as_str(), vec![]))
        .unwrap();
    ada.commit();
    (ada, root.doc)
}

/// One published resource, as the tree carries it: its id, how it reads to a person, and its
/// kind (a channel, a message, a category…).
#[derive(Debug)]
struct Row {
    id: String,
    label: String,
    kind: String,
}

fn collect(tree: &tilo_actions::ResourceSpec, out: &mut Vec<Row>) {
    out.push(Row {
        id: tree.id.as_str().to_owned(),
        label: tree.label.clone(),
        kind: tree.kind.clone(),
    });
    for child in &tree.children {
        collect(child, out);
    }
}

fn published_rows() -> Vec<Row> {
    let (replica, root) = seeded_chat();
    let planner: Arc<dyn Planner> = Arc::new(Refusing);
    let mut published = Publications::new(&replica.host);
    published.settle(&replica.host, &planner);
    let mut rows = Vec::new();
    collect(&published.trees()[&root], &mut rows);
    rows
}

#[test]
fn a_channel_is_named_by_its_own_name_not_its_first_message() {
    let rows = published_rows();
    // The "design" channel: named by its own `name` field.
    assert!(
        rows.iter()
            .any(|row| row.label == "design" && row.kind.ends_with(".TextChannel")),
        "a channel called design: {:#?}",
        rows.iter()
            .map(|row| (&row.label, &row.kind))
            .collect::<Vec<_>>()
    );
    // Its first message says something else, and names only the message.
    let said = "The new onboarding build is on the test machines.";
    let named_said: Vec<&Row> = rows.iter().filter(|row| row.label == said).collect();
    assert_eq!(
        named_said.len(),
        1,
        "one resource is called by that message: {named_said:#?}"
    );
    assert!(
        named_said[0].kind.ends_with(".Message"),
        "the message, not a channel: {:?}",
        named_said[0].kind
    );
}

#[test]
fn no_container_is_named_by_a_message_it_holds() {
    // A channel, a category, a thread or a direct conversation never reads as a message: the
    // two would be rows a person cannot tell apart (the two "Open" rows of one search list).
    let rows = published_rows();
    let messages: std::collections::HashSet<&str> = rows
        .iter()
        .filter(|row| row.kind.ends_with(".Message"))
        .map(|row| row.label.as_str())
        .collect();
    for row in &rows {
        if row.kind.ends_with(".Message") {
            continue;
        }
        assert!(
            !messages.contains(row.label.as_str()),
            "{} ({}) reads as a message it holds: {}",
            row.label,
            row.kind,
            row.id
        );
    }
}
