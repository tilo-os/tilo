#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use serde_json::json;
use tilo_actions::remote::{
    ActionCallError, Caller, InvocationReceipt, InvokeRequest, Pending, PendingPoll, ReceiptStatus,
};

use super::{Answers, Found, NOT_NARROWED, Within, serve, surface};

/// A search that finds what its words name, and keeps no pictures.
struct Held;

impl Answers for Held {
    fn find(&self, _caller: &Caller, needle: &str) -> Pending<Result<Vec<Found>, ActionCallError>> {
        Pending::ready(Ok(vec![Found {
            resource: needle.to_owned(),
            kind: "notes.Note".into(),
            label: "Gooseberry jam".into(),
            ..Found::default()
        }]))
    }
}

/// What `Held` answers `request`, asked as a caller.
fn answered(request: &InvokeRequest) -> InvocationReceipt {
    let mut answer = serve(&Held, &Caller::local_person("ada"), request);
    match answer.poll_ready() {
        PendingPoll::Ready(Ok(receipt)) => receipt,
        PendingPoll::Ready(Err(error)) => panic!("the search answers: {error:?}"),
        PendingPoll::Waiting | PendingPoll::Abandoned => panic!("the search answers at once"),
    }
}

/// The search answered as the dispatch asks it: a call is answered by what it declares, a search
/// narrowed to where and when crosses whole, and one that cannot narrow says so rather than
/// answering with everything.
#[test]
fn a_search_crosses_narrowed_and_one_that_cannot_narrow_says_so() {
    use super::{SearchAction, decoded, request};

    let found = request(&SearchAction::Find {
        needle: "note:1".into(),
        within: None,
    });
    // The call's argument is what the declaration names, once decoded.
    let tilo_actions::remote::InvokeArgument::Json(fields) = &found.argument else {
        panic!("a find carries its words");
    };
    assert_eq!(
        decoded(&found.target.address.action, fields),
        Ok(SearchAction::Find {
            needle: "note:1".into(),
            within: None,
        })
    );
    let ReceiptStatus::Completed { output } = answered(&found).status else {
        panic!("the search found what its words name");
    };
    assert_eq!(
        output.value,
        Some(json!([{
            "resource": "note:1",
            "kind": "notes.Note",
            "label": "Gooseberry jam",
        }]))
    );
    // A search narrowed to a place and a time crosses whole, and one that cannot narrow says
    // so: it never answers with everything instead.
    let narrowed = request(&SearchAction::Find {
        needle: "note:1".into(),
        within: Some(Within {
            place: Some("pad/threads/general".into()),
            after: Some(1),
            ..Within::default()
        }),
    });
    let ReceiptStatus::Rejected {
        error: ActionCallError::Unavailable { reason },
    } = answered(&narrowed).status
    else {
        panic!("a search that cannot be narrowed refuses to be");
    };
    assert_eq!(reason, NOT_NARROWED);
    // A search that keeps no pictures says so rather than answering none.
    let ReceiptStatus::Rejected {
        error: ActionCallError::Unavailable { reason },
    } = answered(&request(&SearchAction::Pictures)).status
    else {
        panic!("a search keeping no pictures refuses to list them");
    };
    assert_eq!(reason, "this search keeps no pictures");
}

/// The file a crate that does not depend on this one binds its calls to is what this crate
/// declares. `UPDATE_SNAPSHOTS=1` writes it.
#[test]
fn the_search_surface_file_is_what_this_crate_publishes() {
    let text = tilo_surface_compile::canonical(&json!({ "resources": [surface()] }));
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("search-surface.json");
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
        std::fs::write(&path, &text).expect("the surface file is written");
        return;
    }
    assert_eq!(
        std::fs::read_to_string(&path).ok().as_deref(),
        Some(text.as_str()),
        "crates/core/workspace-records/search-surface.json is not what this crate \
         publishes; regenerate it with UPDATE_SNAPSHOTS=1"
    );
}

#[test]
fn a_search_is_narrowed_by_where_a_thing_is_and_when() {
    let conversation = ("pad/threads/t1", "t1");
    let last_week = Within {
        place: Some("pad/threads/t1".into()),
        after: Within::moment("2026-11-10"),
        before: Within::moment("2026-11-17T00:00:00Z"),
        app: None,
        about: Vec::new(),
    };
    assert_eq!(last_week.after, Some(1_794_268_800));
    let inside = Within::moment("2026-11-12T09:30:00+01:00");
    assert!(last_week.holds(Some(conversation), inside));
    // By the id the place goes by, too.
    let by_id = Within {
        place: Some("t1".into()),
        ..Within::default()
    };
    assert!(by_id.holds(Some(conversation), None));
    // The stretch holds its first second and not its last.
    assert!(last_week.holds(Some(conversation), last_week.after));
    assert!(!last_week.holds(Some(conversation), last_week.before));
    // Elsewhere, earlier, and what says neither where nor when.
    assert!(!last_week.holds(Some(("pad/threads/t2", "t2")), inside));
    assert!(!last_week.holds(Some(conversation), Within::moment("2026-11-09")));
    assert!(!last_week.holds(None, inside));
    assert!(!last_week.holds(Some(conversation), None));
    // Nothing narrows nothing.
    assert!(Within::default().is_everything() && Within::default().holds(None, None));
    assert!(!last_week.is_everything());
    // A moment reads back as it is written, and words that name none name none.
    assert_eq!(
        Within::said_at(1_794_268_800).as_deref(),
        Some("2026-11-10T00:00:00Z")
    );
    assert_eq!(Within::moment("next week"), None);
}

#[test]
fn a_search_scoped_to_a_resource_holds_the_items_that_live_in_it() {
    // A book, a notebook or a folder is a resource; its items carry the publisher's name
    // for it, with or without the app the index files every resource under.
    let book = Within {
        place: Some("tl:so.tilo.ebook:book1".into()),
        ..Within::default()
    };
    let chapter = ("", "tl:so.tilo.ebook:book1/ch1");
    assert!(book.holds_in(None, Some(chapter), None));
    assert!(
        book.holds_in(None, Some(("tl:so.tilo.ebook:book1/ch2", "")), None),
        "the item's own address is under the scope as well"
    );
    assert!(
        book.holds_in(None, Some(("", "book1/ch3")), None),
        "an app hands the id it knows a resource by, without the app's name"
    );
    assert!(
        !book.holds_in(None, Some(("", "book2/ch1")), None),
        "another book's chapter is not inside it"
    );
    assert!(
        !book.holds_in(None, None, None),
        "what names nothing is outside a search narrowed to one place"
    );
}
