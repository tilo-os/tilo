#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! The index as the store drives it: commits put, move and remove resources, and queries page,
//! filter, sort and count what the commits left.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;

use tilo_resource_store::{
    Cell, Change, Commit, CommitHook as _, FieldKind, Filter, Index, Order, PARENT, Query, Schema,
    Value,
};

const ISSUE: &str = "Issue";

fn issue(id: &str, parent: &str, order: &str, title: &str, state: &str, points: i64) -> Change {
    let mut fields = BTreeMap::new();
    fields.insert("title".to_owned(), Cell::One(title.into()));
    fields.insert("state".to_owned(), Cell::One(state.into()));
    fields.insert("points".to_owned(), Cell::One(points.into()));
    fields.insert(
        "labels".to_owned(),
        Cell::Set(BTreeSet::from([Value::from(state), Value::from("all")])),
    );
    Change::Put {
        kind: ISSUE.to_owned(),
        id: id.to_owned(),
        parent: Some(parent.to_owned()),
        order: Some(order.to_owned()),
        fields,
    }
}

fn commit(index: &mut Index, doc: &str, changes: &[Change]) {
    index.committed(&Commit {
        doc,
        readers: "everyone",
        heads: &[],
        changes,
    });
}

/// Ten issues of one board in one document: `i0`..`i9`, open when even, worth their number.
fn board() -> Index {
    let mut index = Index::new();
    let changes: Vec<Change> = (0..10)
        .map(|n| {
            let state = if n % 2 == 0 { "open" } else { "done" };
            issue(
                &format!("i{n}"),
                "board",
                &format!("a{n}"),
                &format!("Issue {n}"),
                state,
                n,
            )
        })
        .collect();
    commit(&mut index, "doc-1", &changes);
    index
}

/// Whoever reads: every reader set admitted.
fn anyone(_: &str) -> bool {
    true
}

fn query(index: &Index, query: &Query) -> tilo_resource_store::Page {
    index.query(query, &anyone)
}

fn count(index: &Index, kind: &str, filters: &[Filter]) -> usize {
    index.count(kind, filters, &anyone)
}

fn ids(index: &Index, query: &Query) -> Vec<String> {
    self::query(index, query).ids().map(str::to_owned).collect()
}

#[test]
fn pages_follow_the_kinds_order_and_resume_after_their_cursor() {
    let index = board();
    let first = query(&index, &Query::of(ISSUE, 4));
    assert_eq!(first.ids().collect::<Vec<_>>(), ["i0", "i1", "i2", "i3"]);
    let second = query(&index, &Query::of(ISSUE, 4).after(first.next));
    assert_eq!(second.ids().collect::<Vec<_>>(), ["i4", "i5", "i6", "i7"]);
    let last = query(&index, &Query::of(ISSUE, 4).after(second.next));
    assert_eq!(last.ids().collect::<Vec<_>>(), ["i8", "i9"]);
    assert!(last.next.is_none(), "the last page names no next one");
}

#[test]
fn a_cursor_still_resumes_after_the_resource_it_ended_at_left() {
    let mut index = board();
    let first = query(&index, &Query::of(ISSUE, 3));
    commit(
        &mut index,
        "doc-1",
        &[Change::Remove {
            kind: ISSUE.to_owned(),
            id: "i2".to_owned(),
        }],
    );
    let next = ids(&index, &Query::of(ISSUE, 3).after(first.next));
    assert_eq!(next, ["i3", "i4", "i5"]);
}

#[test]
fn filters_narrow_by_value_member_range_and_text() {
    let index = board();
    let open = Query::of(ISSUE, 100).filter(Filter::Is("state".into(), "open".into()));
    assert_eq!(ids(&index, &open), ["i0", "i2", "i4", "i6", "i8"]);

    let labelled = Query::of(ISSUE, 100).filter(Filter::Has("labels".into(), "done".into()));
    assert_eq!(ids(&index, &labelled), ["i1", "i3", "i5", "i7", "i9"]);

    let middle = Query::of(ISSUE, 100).filter(Filter::Within {
        field: "points".into(),
        from: Bound::Excluded(Value::Int(3)),
        to: Bound::Included(Value::Float(6.0)),
    });
    assert_eq!(ids(&index, &middle), ["i4", "i5", "i6"]);

    let both = Query::of(ISSUE, 100)
        .filter(Filter::Is("state".into(), "open".into()))
        .filter(Filter::Contains("title".into(), "ISSUE 4".into()));
    assert_eq!(ids(&index, &both), ["i4"]);

    assert_eq!(count(&index, ISSUE, &[]), 10);
    assert_eq!(count(&index, ISSUE, &open.filters), 5);
    assert_eq!(count(&index, ISSUE, &both.filters), 1);
}

#[test]
fn a_filtered_page_resumes_after_its_cursor() {
    let index = board();
    let open = Query::of(ISSUE, 2).filter(Filter::Is("state".into(), "open".into()));
    let first = query(&index, &open);
    assert_eq!(first.ids().collect::<Vec<_>>(), ["i0", "i2"]);
    let second = query(&index, &open.after(first.next));
    assert_eq!(second.ids().collect::<Vec<_>>(), ["i4", "i6"]);
}

#[test]
fn a_field_sorts_either_way() {
    let index = board();
    let most = Query::of(ISSUE, 3).order(Order::by("points").descending());
    let first = query(&index, &most);
    assert_eq!(first.ids().collect::<Vec<_>>(), ["i9", "i8", "i7"]);
    assert_eq!(ids(&index, &most.after(first.next)), ["i6", "i5", "i4"]);
}

#[test]
fn a_change_moves_a_resource_between_values_and_places() {
    let mut index = board();
    commit(
        &mut index,
        "doc-1",
        &[issue("i1", "board", "a99", "Issue 1", "open", 1)],
    );
    let open = Query::of(ISSUE, 100).filter(Filter::Is("state".into(), "open".into()));
    assert_eq!(ids(&index, &open), ["i0", "i2", "i4", "i6", "i8", "i1"]);
    let done = Query::of(ISSUE, 100).filter(Filter::Has("labels".into(), "done".into()));
    assert_eq!(ids(&index, &done), ["i3", "i5", "i7", "i9"]);
}

#[test]
fn nested_resources_are_listed_by_their_parent_across_documents() {
    let mut index = board();
    commit(
        &mut index,
        "doc-2",
        &[issue("j0", "other-board", "a0", "Elsewhere", "open", 0)],
    );
    let other = Query::of(ISSUE, 100).filter(Filter::Is(PARENT.into(), "other-board".into()));
    assert_eq!(ids(&index, &other), ["j0"]);
    assert_eq!(
        count(&index, ISSUE, &[Filter::Is(PARENT.into(), "board".into())]),
        10
    );
    assert_eq!(index.get(ISSUE, "j0").unwrap().doc, "doc-2");
}

#[test]
fn a_resource_moved_to_another_document_stays_whichever_commit_comes_first() {
    let mut index = board();
    let moved = issue("i3", "board", "a3", "Issue 3", "done", 3);
    // The new document's commit arrives before the old one's removal.
    commit(&mut index, "doc-2", std::slice::from_ref(&moved));
    commit(
        &mut index,
        "doc-1",
        &[Change::Remove {
            kind: ISSUE.to_owned(),
            id: "i3".to_owned(),
        }],
    );
    assert_eq!(index.get(ISSUE, "i3").unwrap().doc, "doc-2");

    index.forget("doc-1");
    assert_eq!(ids(&index, &Query::of(ISSUE, 100)), ["i3"]);
}

#[test]
fn a_surface_declares_types_their_indexed_fields_and_what_nests_in_them() {
    let surface: tilo_actions::surface::Surface = toml_like(serde_json::json!({
        "kind": "tasks.tracker",
        "label": "Tracker",
        "version": 1,
        "types": [
            {"name": "Tracker", "fields": [
                {"name": "name", "type": "text"},
                {"name": "issues", "type": {"collection": {"named": "Issue"}}},
            ]},
            {"name": "Issue", "fields": [
                {"name": "title", "type": "text"},
                {"name": "body", "type": "rich_text"},
                {"name": "state", "type": {"named": "State"}},
                {"name": "due", "type": {"option": "date"}},
                {"name": "labels", "type": {"set": "text"}},
                {"name": "comments", "type": {"collection": {"named": "Comment"}}},
            ]},
            {"name": "Comment", "fields": [{"name": "text", "type": "text"}]},
            {"name": "State", "cases": [{"name": "open"}, {"name": "done"}]},
        ],
    }));
    let schema = Schema::from_surface(&surface);
    let names: Vec<&str> = schema.types.iter().map(|ty| ty.name.as_str()).collect();
    assert_eq!(names, ["Tracker", "Issue", "Comment"]);
    let issue = schema.get("Issue").unwrap();
    let fields: Vec<(&str, FieldKind)> = issue
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field.kind))
        .collect();
    assert_eq!(
        fields,
        [
            ("title", FieldKind::Scalar),
            ("state", FieldKind::Scalar),
            ("due", FieldKind::Scalar),
            ("labels", FieldKind::Set),
        ]
    );
    assert_eq!(
        issue.nested,
        [("comments".to_owned(), "Comment".to_owned())]
    );
    assert_eq!(
        schema.get("Tracker").unwrap().nested,
        [("issues".to_owned(), "Issue".to_owned())]
    );
}

fn toml_like(value: serde_json::Value) -> tilo_actions::surface::Surface {
    serde_json::from_value(value).expect("a surface")
}

mod store {
    use super::*;
    use tilo_resource_store::store::DOCUMENT_LIMIT;
    use tilo_resource_store::{Resource, Store, Written};

    const MEMBERS: &str = "members";

    fn fields(title: &str, state: &str) -> BTreeMap<String, Cell> {
        BTreeMap::from([
            ("title".to_owned(), Cell::One(title.into())),
            ("state".to_owned(), Cell::One(state.into())),
            (
                "labels".to_owned(),
                Cell::Set(BTreeSet::from([Value::from("bug")])),
            ),
        ])
    }

    fn resource(parent: &str, order: &str, title: &str, state: &str) -> Resource {
        read_by(MEMBERS, parent, order, title, state)
    }

    fn read_by(readers: &str, parent: &str, order: &str, title: &str, state: &str) -> Resource {
        Resource {
            readers: readers.to_owned(),
            parent: Some(parent.to_owned()),
            order: Some(order.to_owned()),
            fields: fields(title, state),
        }
    }

    fn put(store: &mut Store, id: &str, resource: Resource) -> Vec<Written> {
        store.put(ISSUE, id, resource).unwrap()
    }

    /// Carries every change written to `to`, as sync does.
    fn send(written: &[Written], to: &mut Store) {
        for write in written {
            to.receive(&write.doc, &write.readers, vec![write.change.clone()])
                .unwrap();
        }
    }

    fn listed(store: &Store, query: &Query, may_read: &dyn Fn(&str) -> bool) -> Vec<String> {
        store
            .query(query, may_read)
            .ids()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn a_write_is_queried_at_once_and_reaches_a_peer() {
        let mut here = Store::new();
        let mut there = Store::new();
        let mut written = put(&mut here, "a", resource("board", "a0", "First", "open"));
        written.extend(put(
            &mut here,
            "b",
            resource("board", "a1", "Second", "done"),
        ));
        let open = Query::of(ISSUE, 10).filter(Filter::Is("state".into(), "open".into()));
        assert_eq!(listed(&here, &open, &anyone), ["a"]);

        send(&written, &mut there);
        assert_eq!(listed(&there, &open, &anyone), ["a"]);
        assert_eq!(
            there.query(&Query::of(ISSUE, 10), &anyone).resources,
            here.query(&Query::of(ISSUE, 10), &anyone).resources
        );
    }

    #[test]
    fn two_devices_changing_different_fields_keep_both() {
        let mut here = Store::new();
        let mut there = Store::new();
        let made = put(&mut here, "a", resource("board", "a0", "First", "open"));
        send(&made, &mut there);

        let titled = put(&mut here, "a", resource("board", "a0", "Renamed", "open"));
        let closed = put(&mut there, "a", resource("board", "a0", "First", "done"));
        send(&titled, &mut there);
        send(&closed, &mut here);
        for store in [&here, &there] {
            let stored = store.index().get(ISSUE, "a").unwrap();
            assert_eq!(stored.scalar("title"), Value::from("Renamed"));
            assert_eq!(stored.scalar("state"), Value::from("done"));
        }
    }

    #[test]
    fn a_removal_reaches_a_peer() {
        let mut here = Store::new();
        let mut there = Store::new();
        let made = put(&mut here, "a", resource("board", "a0", "First", "open"));
        let gone = here.remove(ISSUE, "a").unwrap();
        send(&made, &mut there);
        assert_eq!(there.count(ISSUE, &[], &anyone), 1);
        send(&[gone], &mut there);
        assert_eq!(there.count(ISSUE, &[], &anyone), 0);
        assert_eq!(here.count(ISSUE, &[], &anyone), 0);
    }

    #[test]
    fn a_reader_is_shown_and_counted_only_what_they_may_read() {
        let mut store = Store::new();
        put(
            &mut store,
            "shared",
            resource("board", "a0", "Shared", "open"),
        );
        put(
            &mut store,
            "mine",
            read_by("ada-only", "board", "a1", "Private", "open"),
        );
        let ada = |readers: &str| readers == MEMBERS || readers == "ada-only";
        let bob = |readers: &str| readers == MEMBERS;
        let open = Query::of(ISSUE, 10).filter(Filter::Is("state".into(), "open".into()));
        assert_eq!(listed(&store, &open, &ada), ["shared", "mine"]);
        assert_eq!(listed(&store, &open, &bob), ["shared"]);
        assert_eq!(store.count(ISSUE, &open.filters, &bob), 1);
        assert_eq!(store.count(ISSUE, &[], &bob), 1);
        assert!(store.get(ISSUE, "mine", &bob).is_none());
        assert!(store.get(ISSUE, "mine", &ada).is_some());

        // A page that ends in what one reader set holds resumes in the other's.
        let first = store.query(&Query::of(ISSUE, 1), &ada);
        assert_eq!(first.ids().collect::<Vec<_>>(), ["shared"]);
        let next = store.query(&Query::of(ISSUE, 1).after(first.next), &ada);
        assert_eq!(next.ids().collect::<Vec<_>>(), ["mine"]);
    }

    #[test]
    fn a_resource_whose_readers_change_moves_to_a_document_of_theirs() {
        let mut here = Store::new();
        let mut there = Store::new();
        let made = put(&mut here, "a", resource("board", "a0", "Draft", "open"));
        send(&made, &mut there);
        let moved = put(
            &mut here,
            "a",
            read_by("ada-only", "board", "a0", "Draft", "open"),
        );
        let docs: Vec<(&str, &str)> = moved
            .iter()
            .map(|write| (write.doc.as_str(), write.readers.as_str()))
            .collect();
        assert_eq!(
            docs,
            [
                ("members/board#0", MEMBERS),
                ("ada-only/board#0", "ada-only")
            ]
        );
        send(&moved, &mut there);
        let members = |readers: &str| readers == MEMBERS;
        for store in [&here, &there] {
            assert_eq!(store.count(ISSUE, &[], &members), 0);
            assert_eq!(store.count(ISSUE, &[], &anyone), 1);
            assert_eq!(
                store.index().get(ISSUE, "a").unwrap().doc,
                "ada-only/board#0"
            );
        }
    }

    #[test]
    fn a_full_document_starts_the_next_and_a_query_reads_across_them() {
        let mut store = Store::new();
        let mut docs = BTreeSet::new();
        for n in 0..=DOCUMENT_LIMIT {
            for written in put(
                &mut store,
                &format!("i{n:05}"),
                resource("board", &format!("{n:05}"), "x", "open"),
            ) {
                docs.insert(written.doc);
            }
        }
        assert_eq!(
            docs.into_iter().collect::<Vec<_>>(),
            ["members/board#0", "members/board#1"]
        );
        assert_eq!(
            store.count(ISSUE, &[Filter::Is(PARENT.into(), "board".into())], &anyone),
            DOCUMENT_LIMIT + 1
        );
        let last = store.query(
            &Query::of(ISSUE, 1).order(Order::default().descending()),
            &anyone,
        );
        assert_eq!(
            last.ids().collect::<Vec<_>>(),
            [format!("i{DOCUMENT_LIMIT:05}")]
        );
        let held = store.document("members/board#0").unwrap();
        assert!(held.save().len() < 1 << 20, "a document stays small");
    }

    /// A store holding three issues in one document, and that document saved.
    fn kept() -> (Store, String, Vec<u8>) {
        let mut store = Store::new();
        let mut doc = String::new();
        for (id, state) in [("a", "open"), ("b", "done"), ("c", "open")] {
            doc = put(&mut store, id, resource("board", id, id, state))
                .pop()
                .unwrap()
                .doc;
        }
        let bytes = store.document(&doc).unwrap().save();
        (store, doc, bytes)
    }

    #[test]
    fn a_kept_document_is_loaded_with_its_resources() {
        let (kept, doc, bytes) = kept();
        let mut started = Store::new();
        started.load(&doc, &bytes).unwrap();
        let all = Query::of(ISSUE, 10);
        assert_eq!(
            started.query(&all, &anyone).resources,
            kept.query(&all, &anyone).resources
        );
        // A new resource goes on into the same document.
        let next = put(&mut started, "d", resource("board", "d", "d", "open"));
        assert_eq!(next[0].doc, doc);
    }

    #[test]
    fn a_snapshot_at_the_documents_heads_starts_without_reading_it() {
        let (kept, doc, bytes) = kept();
        let snapshot = kept.snapshot(&doc).unwrap();
        let heads = kept.index().heads(&doc).unwrap().to_vec();

        let saved = bytes.clone();
        let mut started = Store::with_source(Box::new(move |_| Some(saved.clone())));
        assert!(started.restore(&doc, &snapshot, &heads));
        assert_eq!(started.in_memory(), 0, "the document is not read to start");
        let open = Query::of(ISSUE, 10).filter(Filter::Is("state".into(), "open".into()));
        assert_eq!(listed(&started, &open, &anyone), ["a", "c"]);

        // A write reads the document from the source and goes on from it.
        put(&mut started, "b", resource("board", "b", "b", "open"));
        assert_eq!(started.in_memory(), 1);
        assert_eq!(listed(&started, &open, &anyone), ["a", "b", "c"]);
        assert_eq!(started.document(&doc).unwrap().get_heads().len(), 1);

        // A snapshot of other heads is not taken: the document is loaded instead.
        let mut stale = Store::new();
        assert!(!stale.restore(&doc, &snapshot, &[]));
        assert_eq!(stale.count(ISSUE, &[], &anyone), 0);
        stale.load(&doc, &bytes).unwrap();
        assert_eq!(stale.count(ISSUE, &[], &anyone), 3);
    }

    #[test]
    fn an_evicted_document_stays_indexed_and_is_read_again_for_a_write() {
        let (mut store, doc, _) = kept();
        let saved = store.evict(&doc).unwrap();
        assert_eq!(store.in_memory(), 0);
        assert_eq!(store.count(ISSUE, &[], &anyone), 3);

        let mut sourced = Store::with_source(Box::new(move |_| Some(saved.clone())));
        let (_, doc, bytes) = kept();
        sourced.load(&doc, &bytes).unwrap();
        sourced.evict(&doc);
        put(&mut sourced, "a", resource("board", "a", "a", "done"));
        let done = Query::of(ISSUE, 10).filter(Filter::Is("state".into(), "done".into()));
        assert_eq!(listed(&sourced, &done, &anyone), ["a", "b"]);
    }

    #[test]
    fn a_watch_names_the_commits_of_its_kind() {
        let mut store = Store::new();
        let issues = store.watch(ISSUE);
        let notes = store.watch("Note");
        put(&mut store, "a", resource("board", "a", "a", "open"));
        assert_eq!(store.changed(), BTreeSet::from([issues]));
        assert!(store.changed().is_empty(), "each change is named once");
        store.unwatch(issues);
        put(&mut store, "b", resource("board", "b", "b", "open"));
        assert!(store.changed().is_empty());
        let _ = notes;
    }
}
