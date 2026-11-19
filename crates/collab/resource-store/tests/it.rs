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

fn ids(index: &Index, query: &Query) -> Vec<String> {
    index.query(query).ids().map(str::to_owned).collect()
}

#[test]
fn pages_follow_the_kinds_order_and_resume_after_their_cursor() {
    let index = board();
    let first = index.query(&Query::of(ISSUE, 4));
    assert_eq!(first.ids().collect::<Vec<_>>(), ["i0", "i1", "i2", "i3"]);
    let second = index.query(&Query::of(ISSUE, 4).after(first.next));
    assert_eq!(second.ids().collect::<Vec<_>>(), ["i4", "i5", "i6", "i7"]);
    let last = index.query(&Query::of(ISSUE, 4).after(second.next));
    assert_eq!(last.ids().collect::<Vec<_>>(), ["i8", "i9"]);
    assert!(last.next.is_none(), "the last page names no next one");
}

#[test]
fn a_cursor_still_resumes_after_the_resource_it_ended_at_left() {
    let mut index = board();
    let first = index.query(&Query::of(ISSUE, 3));
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

    assert_eq!(index.count(ISSUE, &[]), 10);
    assert_eq!(index.count(ISSUE, &open.filters), 5);
    assert_eq!(index.count(ISSUE, &both.filters), 1);
}

#[test]
fn a_filtered_page_resumes_after_its_cursor() {
    let index = board();
    let open = Query::of(ISSUE, 2).filter(Filter::Is("state".into(), "open".into()));
    let first = index.query(&open);
    assert_eq!(first.ids().collect::<Vec<_>>(), ["i0", "i2"]);
    let second = index.query(&open.after(first.next));
    assert_eq!(second.ids().collect::<Vec<_>>(), ["i4", "i6"]);
}

#[test]
fn a_field_sorts_either_way() {
    let index = board();
    let most = Query::of(ISSUE, 3).order(Order::by("points").descending());
    let first = index.query(&most);
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
        index.count(ISSUE, &[Filter::Is(PARENT.into(), "board".into())]),
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
    use tilo_resource_store::{Resource, Store};

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
        Resource {
            parent: Some(parent.to_owned()),
            order: Some(order.to_owned()),
            fields: fields(title, state),
        }
    }

    /// Carries every change `from` wrote to `to`, as sync does.
    fn send(written: &[tilo_resource_store::Written], to: &mut Store) {
        for write in written {
            to.receive(&write.doc, vec![write.change.clone()]).unwrap();
        }
    }

    #[test]
    fn a_write_is_queried_at_once_and_reaches_a_peer() {
        let mut here = Store::new();
        let mut there = Store::new();
        let written = vec![
            here.put(ISSUE, "a", resource("board", "a0", "First", "open"))
                .unwrap(),
            here.put(ISSUE, "b", resource("board", "a1", "Second", "done"))
                .unwrap(),
        ];
        let open = Query::of(ISSUE, 10).filter(Filter::Is("state".into(), "open".into()));
        assert_eq!(here.query(&open).ids().collect::<Vec<_>>(), ["a"]);

        send(&written, &mut there);
        assert_eq!(there.query(&open).ids().collect::<Vec<_>>(), ["a"]);
        assert_eq!(
            there.query(&Query::of(ISSUE, 10)).resources,
            here.query(&Query::of(ISSUE, 10)).resources
        );
    }

    #[test]
    fn two_devices_changing_different_fields_keep_both() {
        let mut here = Store::new();
        let mut there = Store::new();
        let made = here
            .put(ISSUE, "a", resource("board", "a0", "First", "open"))
            .unwrap();
        send(&[made], &mut there);

        let titled = here
            .put(ISSUE, "a", resource("board", "a0", "Renamed", "open"))
            .unwrap();
        let closed = there
            .put(ISSUE, "a", resource("board", "a0", "First", "done"))
            .unwrap();
        send(&[titled], &mut there);
        send(&[closed], &mut here);
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
        let made = here
            .put(ISSUE, "a", resource("board", "a0", "First", "open"))
            .unwrap();
        let gone = here.remove(ISSUE, "a").unwrap();
        send(&[made], &mut there);
        assert_eq!(there.count(ISSUE, &[]), 1);
        send(&[gone], &mut there);
        assert_eq!(there.count(ISSUE, &[]), 0);
        assert_eq!(here.count(ISSUE, &[]), 0);
    }

    #[test]
    fn a_full_part_starts_the_next_and_a_query_reads_across_them() {
        let mut store = Store::new();
        let mut docs = BTreeSet::new();
        for n in 0..=DOCUMENT_LIMIT {
            let written = store
                .put(
                    ISSUE,
                    &format!("i{n:05}"),
                    resource("board", &format!("{n:05}"), "x", "open"),
                )
                .unwrap();
            docs.insert(written.doc);
        }
        assert_eq!(docs.into_iter().collect::<Vec<_>>(), ["board#0", "board#1"]);
        assert_eq!(
            store.count(ISSUE, &[Filter::Is(PARENT.into(), "board".into())]),
            DOCUMENT_LIMIT + 1
        );
        let last = store.query(&Query::of(ISSUE, 1).order(Order::default().descending()));
        assert_eq!(
            last.ids().collect::<Vec<_>>(),
            [format!("i{DOCUMENT_LIMIT:05}")]
        );
        let held = store.document("board#0").unwrap();
        assert!(held.save().len() < 1 << 20, "a document stays small");
    }
}
