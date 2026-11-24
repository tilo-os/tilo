use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tilo_actions::remote::search::{Description, IndexKind, Picture};
use tilo_actions::remote::{Caller, Within};
use tilo_resource::search_index::{IndexDelta, IndexEntry, IndexWeight};

use super::{AppContentIndex, IndexKey, IndexSearch};

const WORKSPACE: &str = "w-team";
const ELSEWHERE: &str = "w-elsewhere";
const NOTES: &str = "so.tilo.notes";
const PHOTOS: &str = "so.tilo.photos";

fn entry(key: &str, title: &str) -> IndexEntry {
    IndexEntry {
        key: key.to_owned(),
        resource: key.to_owned(),
        title: title.to_owned(),
        ..IndexEntry::default()
    }
}

fn publish(
    index: &AppContentIndex,
    app: &str,
    workspace: &str,
    record: &str,
    entries: Vec<IndexEntry>,
) {
    index.apply(
        &IndexKey::new(app, workspace, record),
        IndexDelta {
            upsert: entries,
            ..IndexDelta::default()
        },
    );
}

fn titles(found: &[tilo_actions::remote::Found]) -> Vec<&str> {
    found.iter().map(|found| found.label.as_str()).collect()
}

/// What an app published is found by its words, each result naming the workspace, the app and the
/// resource a caller opens, and opened through the app at it.
#[test]
fn a_person_finds_what_an_app_published_and_opens_it_at_the_app() {
    let index = Arc::new(AppContentIndex::in_memory());
    publish(
        &index,
        NOTES,
        WORKSPACE,
        "notes:1",
        vec![entry("n1", "Groceries for the weekend")],
    );
    let search = IndexSearch::new(Arc::clone(&index), Arc::new(|_, _| true));

    let found = search.found(
        &Caller::local_person("ada"),
        "groceries",
        &Within::default(),
    );
    assert_eq!(titles(&found), ["Groceries for the weekend"]);
    assert_eq!(found[0].workspace.as_deref(), Some(WORKSPACE));
    assert_eq!(found[0].app.as_deref(), Some(NOTES));
    assert_eq!(found[0].resource, "n1");
    assert!(found[0].open.is_some(), "a result opens: {:?}", found[0]);
    assert_eq!(found[0].arguments["anchor"], "n1");

    // Narrowed to another app, it is not there.
    let elsewhere = Within {
        app: Some(PHOTOS.to_owned()),
        ..Within::default()
    };
    assert!(
        search
            .found(&Caller::local_person("ada"), "groceries", &elsewhere)
            .is_empty()
    );
}

/// A record a caller may not read is nothing they find, decided again on every search: a read
/// taken away hides what was published before. An agent searches the workspace it works in, the
/// person every workspace they see.
#[test]
fn a_caller_finds_only_what_it_may_read_now() {
    let index = Arc::new(AppContentIndex::in_memory());
    publish(
        &index,
        NOTES,
        WORKSPACE,
        "review",
        vec![entry("r1", "Restricted review")],
    );
    publish(
        &index,
        NOTES,
        WORKSPACE,
        "shared",
        vec![entry("s1", "Shared review")],
    );
    publish(
        &index,
        NOTES,
        ELSEWHERE,
        "shared",
        vec![entry("e1", "Elsewhere review")],
    );
    let revoked = Arc::new(AtomicBool::new(false));
    let search = IndexSearch::new(Arc::clone(&index), {
        let revoked = Arc::clone(&revoked);
        Arc::new(move |caller: &Caller, key: &IndexKey| {
            key.record != "review"
                || (caller.acting().is_person() && !revoked.load(Ordering::Acquire))
        })
    });
    let ada = Caller::local_person("ada");
    let helper = Caller::local_agent("helper").in_workspace(WORKSPACE);

    let mut found = titles(&search.found(&ada, "review", &Within::default())).join(", ");
    assert!(
        found.contains("Restricted review") && found.contains("Elsewhere review"),
        "{found}"
    );
    found = titles(&search.found(&helper, "review", &Within::default())).join(", ");
    assert_eq!(
        found, "Shared review",
        "the agent reads neither the record nor the other workspace"
    );

    revoked.store(true, Ordering::Release);
    found = titles(&search.found(&ada, "review", &Within::default())).join(", ");
    assert!(
        !found.contains("Restricted review"),
        "the read was taken away: {found}"
    );
}

/// Ten thousand entries are searched at once, found by a word only one of them holds, and a
/// listing of what entries are about stops at the most recent five hundred.
#[test]
fn ten_thousand_entries_are_searched_at_once() {
    let index = Arc::new(AppContentIndex::in_memory());
    let people: Vec<IndexEntry> = (0..10_000)
        .map(|n| IndexEntry {
            kind: Some(IndexKind::Person),
            when: Some(n),
            ..entry(&format!("p{n}"), &format!("Person {n}"))
        })
        .chain([entry("needle", "Zanzibar itinerary")])
        .collect();
    publish(&index, NOTES, WORKSPACE, "people", people);
    let search = IndexSearch::new(Arc::clone(&index), Arc::new(|_, _| true));
    let ada = Caller::local_person("ada");

    let started = Instant::now();
    let found = search.found(&ada, "zanzibar", &Within::default());
    let first = started.elapsed();
    assert_eq!(titles(&found), ["Zanzibar itinerary"]);
    let started = Instant::now();
    let found = search.found(&ada, "person 9999", &Within::default());
    let again = started.elapsed();
    assert_eq!(
        found.first().map(|found| found.label.as_str()),
        Some("Person 9999")
    );
    assert!(
        first < Duration::from_secs(2) && again < Duration::from_millis(200),
        "the first search reads the index in {first:?}, the next in {again:?}"
    );

    let listed = search.found(
        &ada,
        "",
        &Within {
            about: vec![IndexKind::Person],
            ..Within::default()
        },
    );
    assert_eq!(listed.len(), 500);
    assert_eq!(listed[0].label, "Person 9999", "the most recent first");
}

/// The image model reads the pictures a caller may read, and what one shows joins the entry its
/// app published for it: one picture is one result, found by its name and by what it shows. A
/// picture in a record the caller may not read is neither listed nor described.
#[test]
fn a_described_picture_is_one_result_found_by_what_it_shows() {
    let index = Arc::new(AppContentIndex::in_memory());
    publish(
        &index,
        PHOTOS,
        WORKSPACE,
        "library",
        vec![IndexEntry {
            image: Some("tilo.content:abc".to_owned()),
            ..entry("tl:so.tilo.photos:photo:1", "At the beach")
        }],
    );
    publish(
        &index,
        PHOTOS,
        WORKSPACE,
        "private",
        vec![IndexEntry {
            image: Some("tilo.content:def".to_owned()),
            ..entry("tl:so.tilo.photos:photo:2", "Kept apart")
        }],
    );
    let search = IndexSearch::new(
        Arc::clone(&index),
        Arc::new(|_, key: &IndexKey| key.record != "private"),
    );
    let ada = Caller::local_person("ada");

    let pictures = search.pictures(&ada);
    assert_eq!(
        pictures,
        [Picture {
            resource: "tl:so.tilo.photos:photo:1".to_owned(),
            app: PHOTOS.to_owned(),
            workspace: WORKSPACE.to_owned(),
            record: "library".to_owned(),
            image: "tilo.content:abc".to_owned(),
        }]
    );
    search
        .describe(
            &ada,
            Description {
                picture: pictures[0].clone(),
                title: "A dog on a beach".to_owned(),
                words: vec![
                    ("dog".to_owned(), IndexWeight::Metadata),
                    ("42 North Street".to_owned(), IndexWeight::Body),
                ],
            },
        )
        .expect("the picture is described");
    let found = search.found(&ada, "dog", &Within::default());
    assert_eq!(titles(&found), ["At the beach"], "the app's own name stays");
    assert_eq!(
        titles(&search.found(&ada, "beach", &Within::default())),
        ["At the beach"]
    );

    let apart = Picture {
        record: "private".to_owned(),
        resource: "tl:so.tilo.photos:photo:2".to_owned(),
        ..pictures[0].clone()
    };
    assert!(
        search
            .describe(
                &ada,
                Description {
                    picture: apart,
                    title: String::new(),
                    words: vec![("cat".to_owned(), IndexWeight::Metadata)],
                },
            )
            .is_err(),
        "a picture the caller may not read is not described"
    );
}
