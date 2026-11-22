//! A chain's items read and listed through the action API, whichever part of the chain keeps
//! them.
//!
//! A tracker keeps its issues in numbered parts: a part takes the issues filed while it is the
//! chain's head, and once it is full the issues after it start the next part. A caller naming
//! the tracker's issues — an agent's `tilo_read` and `tilo_ls`, a page's catalogue — names the
//! chain, not the part holding the newest issue: the action API answers with the issues of the
//! parts this host holds, the newest part first, a page at a time, and each issue reads on its
//! own from the part keeping it.
//!
//! The record is held the way a host holds it for the action API, in a confined app worker:
//! the same replica a workspace's host asks for an action or a page.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard, and one scenario runs in order"
)]

use collab_types::DocumentMutation;
use tilo_actions::MemberId;
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{CatalogueQuery, CatalogueRow};
use tilo_agent_session::{ConfinedReplica, Replica, ReplicaActor, Seed};

/// The tracker, which keeps its issues in a chain.
const TASKS: &str = "so.tilo.tasks";

/// The address the tracker publishes its issues under.
fn issues() -> String {
    ui::resource::chain_resource_id(&tasks::issue_list::chain())
}

/// Part `index` of the tracker's issues.
fn part(index: u64) -> ui::resource::DocumentId {
    tasks::issue_list::chain().part(index)
}

/// A tracker a page founded: its record, with no issue yet.
fn founded() -> Replica {
    crate::common::open(
        TASKS,
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap()
}

/// The tracker `page` founded, held as a host holds it for the action API.
fn held(page: &mut Replica) -> ConfinedReplica {
    let Some(DocumentMutation::Snapshot(changes)) = page.take_mutation().unwrap() else {
        panic!("a new record is published whole");
    };
    crate::common::confined(
        TASKS,
        &MemberId::person("host"),
        &ReplicaActor::Owner,
        Seed::Existing {
            record: page.document_id().clone(),
            changes: vec![changes],
        },
    )
    .expect("the tracker opens in its worker")
}

/// File an issue called `title` with `body`, through the action API the host serves.
fn file(held: &ConfinedReplica, title: &str, body: &str) {
    let mut issue = serde_json::json!({"title": title});
    if !body.is_empty() {
        issue["body"] = serde_json::json!(body);
    }
    held.invoke(
        &ActionAddress::new(issues(), "create"),
        issue.as_object().unwrap(),
        false,
    )
    .expect("the issue is filed");
}

/// File issues of a few kilobytes each, `0: `, `1: ` and on, until one no longer fits in the
/// first part and starts the second: how many were filed. The last of them is the second
/// part's first.
fn fill_a_part(held: &ConfinedReplica) -> usize {
    let body = "The service keeps its work beside its peers. ".repeat(80);
    let mut filed = 0_usize;
    while held.read_resource(&part(1), |_| ()).is_none() {
        assert!(filed < 10_000, "a part fills");
        file(held, &format!("{filed}: issue"), &body);
        filed = filed.saturating_add(1);
    }
    filed
}

/// Read the tracker, through the action API, with `arguments`.
fn read(
    held: &ConfinedReplica,
    resource: &str,
    arguments: &serde_json::Value,
) -> serde_json::Value {
    held.invoke(
        &ActionAddress::new(resource, "read"),
        &arguments.as_object().cloned().unwrap_or_default(),
        false,
    )
    .expect("the read is answered")
    .value
    .unwrap_or_else(|| panic!("{resource} answers what it read"))
}

/// The issues one page of a read answers, by their titles.
fn titles(page: &serde_json::Value) -> Vec<String> {
    page.get("issues")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|issue| issue["value"]["title"].as_str())
        .map(str::to_owned)
        .collect()
}

/// The rows `query` lists of `surveyed`, as the action API pages them.
fn rows_of(surveyed: &[CatalogueRow], query: &CatalogueQuery) -> Vec<CatalogueRow> {
    let take = query.page_size();
    Replica::page_of(surveyed, query, (true, (0, take))).1
}

#[test]
fn a_chain_reads_and_lists_the_items_of_every_part_its_host_holds() {
    let mut record = founded();
    let held = held(&mut record);
    // One part takes the issues filed while it is the head, as long as they fit in it: the
    // issue that would overflow it starts the next part, which is the head from then on.
    let filled = fill_a_part(&held);
    file(&held, "first of the newest part", "");
    file(&held, "second of the newest part", "");

    // What is inside the tracker, through the action API: the issues of the part the head
    // is, and the issues of the part before it.
    let surveyed = held.survey_rows();
    let listed: Vec<&CatalogueRow> = surveyed
        .iter()
        .filter(|row| row.parent_resource == issues())
        .collect();
    assert!(!listed.is_empty(), "the action API lists the issues");
    for name in [
        "second of the newest part",
        "0: ",
        format!("{}: ", filled - 2).as_str(),
    ] {
        assert!(
            listed
                .iter()
                .any(|row| row.resource.label.starts_with(name)),
            "the action API lists {name}"
        );
    }

    // What a caller asking for those issues reads: only the rows under the chain, and the
    // words asked for.
    let asked = rows_of(
        &surveyed,
        &CatalogueQuery {
            provider: Some("host".into()),
            parent_resource: Some(issues()),
            max_results: 8,
            needle: "of the newest part".into(),
            ..CatalogueQuery::default()
        },
    );
    assert!(!asked.is_empty(), "the words asked for are listed");
    assert!(
        asked
            .iter()
            .all(|row| row.resource.label.contains("of the newest part")),
        "a page carries what was asked for"
    );

    // Reading the tracker answers its issues from the newest, a page at a time: the page
    // after it goes on into the part before.
    let first = read(&held, &issues(), &serde_json::json!({"pageSize": 2}));
    assert_eq!(
        titles(&first),
        ["second of the newest part", "first of the newest part"],
        "the newest issues first: {first}"
    );
    let cursor = first
        .get("nextCursor")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("a page follows: {first}"))
        .to_owned();
    let second = read(
        &held,
        &issues(),
        &serde_json::json!({"pageSize": 2, "cursor": cursor}),
    );
    let next = titles(&second);
    assert_eq!(next.len(), 2, "a page of two: {second}");
    assert!(
        next[0].starts_with(&format!("{}: ", filled - 1)),
        "the page goes on with the oldest of the newest part: {next:?}"
    );
    assert!(
        next[1].starts_with(&format!("{}: ", filled - 2)),
        "and into the part before, newest first: {next:?}"
    );

    // One issue of a part reads on its own, from the part keeping it, whichever part that is.
    for name in ["second of the newest part", "0: "] {
        let Some(item) = listed
            .iter()
            .find(|row| row.resource.label.starts_with(name))
            .map(|row| row.resource.id.clone())
        else {
            panic!("the action API lists {name}");
        };
        let issue = read(&held, &item, &serde_json::json!({}));
        assert!(
            issue.to_string().contains(name),
            "{name} reads its content: {issue}"
        );
    }
}

/// How many documents `observed` read whole for the content they name.
fn documents_read_for_content(observed: &[ui::tilo_resource::profiling::Sample]) -> usize {
    observed
        .iter()
        .filter(|sample| sample.phase == "app_worker.content_named")
        .map(|sample| sample.calls)
        .sum()
}

#[test]
fn the_content_a_chain_names_is_not_read_again_for_the_workers_own_writes() {
    let mut page = founded();
    let held = held(&mut page).lending(tilo_content::ContentStore::memory());
    fill_a_part(&held);
    file(&held, "first of the newest part", "");
    taken(&held);

    // An effect is given the content its document and every part name.
    let ((), first) = ui::tilo_resource::profiling::observe(|| held.put_held_content().unwrap());
    assert!(documents_read_for_content(&first) >= 1, "{first:?}");
    // Nothing moved: nothing is read again, however much the chain holds.
    let ((), again) = ui::tilo_resource::profiling::observe(|| held.put_held_content().unwrap());
    assert_eq!(documents_read_for_content(&again), 0, "{again:?}");
    // An issue the worker filed itself names only content it made and holds: the part it
    // wrote waits unapplied, and nothing is read again for it.
    file(&held, "second of the newest part", "");
    taken(&held);
    let ((), moved) = ui::tilo_resource::profiling::observe(|| held.put_held_content().unwrap());
    assert_eq!(documents_read_for_content(&moved), 0, "{moved:?}");
}

/// Take what the document and its parts changed, as the server does after each
/// action it saves.
fn taken(held: &ConfinedReplica) {
    held.take_mutation().unwrap();
    held.take_document_mutations().unwrap();
}

/// The resources a page under the tracker's issues lists, in order.
fn listed_ids(held: &ConfinedReplica, window: (usize, usize)) -> (Vec<String>, bool) {
    let query = CatalogueQuery {
        provider: Some("host".into()),
        parent_resource: Some(issues()),
        ..CatalogueQuery::default()
    };
    let (_, rows, arriving) = held.survey_page_arriving(&query, window);
    let mut ids: Vec<String> = Vec::new();
    for row in rows {
        if ids.last() != Some(&row.resource.id) {
            ids.push(row.resource.id.clone());
        }
    }
    (ids, arriving)
}

/// A host that holds only a chain's newest part — one that just started, sent nothing more by
/// the workspace — lists a page reaching past that part only once it holds the part before:
/// the page says it reads a part not here yet, and asks the workspace's hosts for it. It
/// answered the newest part's items as the whole chain, and a caller paging through it read
/// that the chain ended there.
#[test]
fn a_page_reaching_past_the_parts_a_host_holds_asks_for_the_one_before() {
    let mut record = founded();
    let writer = held(&mut record);
    let filled = fill_a_part(&writer);
    file(&writer, "first of the newest part", "");
    file(&writer, "second of the newest part", "");
    taken(&writer);
    let whole = (0, 1_000_000);
    let (every, arriving) = listed_ids(&writer, whole);
    assert!(!arriving, "the writer holds every part");
    assert!(
        every.len() > filled,
        "both parts are listed: {}",
        every.len()
    );
    let saved = |id: &ui::resource::DocumentId| {
        writer
            .read_resource(id, |doc| doc.clone().save())
            .unwrap_or_else(|| panic!("the writer holds {}", id.as_str()))
    };
    let (oldest, newest) = (saved(&part(0)), saved(&part(1)));

    // A host holding the record and the newest part, as one that just started does.
    let fresh = ConfinedReplica::open(
        &crate::common::catalogue(),
        TASKS,
        crate::common::WORKSPACE,
        &MemberId::person("host"),
        &ReplicaActor::Owner,
        Seed::Existing {
            record: record.document_id().clone(),
            changes: vec![writer.read(|doc| doc.clone().save()).unwrap()],
        },
        true,
    )
    .expect("the tracker opens in its worker");
    fresh
        .apply_document(&part(1), &DocumentMutation::Snapshot(newest))
        .unwrap();
    // A page reaching past the newest part reads the part before, which it asks for.
    let (short, arriving) = listed_ids(&fresh, whole);
    assert!(
        arriving,
        "a page reaching past the newest part says it reads one not here: {} of {}",
        short.len(),
        every.len()
    );
    assert!(
        fresh.wanted_resources().contains(&part(0)),
        "the part before is asked for: {:?}",
        fresh.wanted_resources()
    );
    // Handed it, as the host hands what was asked for, the page is the tracker's.
    fresh
        .apply_document(&part(0), &DocumentMutation::Snapshot(oldest))
        .unwrap();
    let (after, arriving) = listed_ids(&fresh, whole);
    assert!(!arriving, "every part is here");
    assert_eq!(
        after, every,
        "the page lists both parts, as the writer's does"
    );
}
