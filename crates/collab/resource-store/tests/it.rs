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

/// Every resource `query` lists, page after page by cursor.
fn listed(index: &Index, query: &Query, may_read: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut all = Vec::new();
    let mut after = None;
    loop {
        let page = index.query(&query.clone().after(after), may_read);
        all.extend(page.ids().map(str::to_owned));
        match page.next {
            Some(next) => after = Some(next),
            None => return all,
        }
    }
}

#[test]
fn a_row_is_found_by_its_position_and_a_position_by_its_row() {
    // Three reader sets, interleaved in the kind's order, so a listing merges their tables.
    let mut index = Index::new();
    for (n, readers) in (0..300).zip(["everyone", "ada", "team"].iter().cycle()) {
        let state = ["open", "doing", "done"][n % 7 % 3];
        let change = issue(
            &format!("i{n:03}"),
            &format!("board{}", n % 2),
            &format!("{n:04}"),
            &format!("Issue {n}"),
            state,
            i64::try_from(n % 11).unwrap(),
        );
        index.committed(&Commit {
            doc: readers,
            readers,
            heads: &[],
            changes: &[change],
        });
    }
    let not_ada = |readers: &str| readers != "ada";
    let queries = [
        Query::of(ISSUE, 7),
        Query::of(ISSUE, 7).order(Order::by("points")),
        Query::of(ISSUE, 7).order(Order::by("points").descending()),
        Query::of(ISSUE, 7).filter(Filter::Is("state".into(), "doing".into())),
        Query::of(ISSUE, 7)
            .filter(Filter::Is(PARENT.into(), "board1".into()))
            .order(Order::default().descending()),
        Query::of(ISSUE, 7).filter(Filter::Has("labels".into(), "done".into())),
        Query::of(ISSUE, 7)
            .filter(Filter::Within {
                field: "points".into(),
                from: Bound::Excluded(2.into()),
                to: Bound::Included(8.into()),
            })
            .order(Order::by("points")),
        // Not one span of any order: answered by walking the listing.
        Query::of(ISSUE, 7)
            .filter(Filter::Is("state".into(), "open".into()))
            .order(Order::by("points").descending()),
    ];
    for may_read in [&anyone as &dyn Fn(&str) -> bool, &not_ada] {
        for query in &queries {
            let all = listed(&index, query, may_read);
            assert_eq!(index.len(query, may_read), all.len(), "{query:?}");
            for start in [
                0,
                1,
                6,
                50,
                all.len().saturating_sub(3),
                all.len(),
                all.len() + 4,
            ] {
                let page = index.at(query, start, may_read);
                let expected: Vec<&String> = all.iter().skip(start).take(7).collect();
                assert_eq!(
                    page.ids().collect::<Vec<_>>(),
                    expected,
                    "{query:?} at {start}"
                );
                // The page's cursor continues where it ended.
                let rest = index.query(&query.clone().after(page.next), may_read);
                if start + 7 < all.len() {
                    assert_eq!(rest.ids().next(), all.get(start + 7).map(String::as_str));
                }
            }
            for (position, id) in all.iter().enumerate() {
                assert_eq!(
                    index.position(query, id, may_read),
                    Some(position),
                    "{query:?} {id}"
                );
            }
        }
    }
    let unlisted = Query::of(ISSUE, 7).filter(Filter::Is("state".into(), "doing".into()));
    let open = listed(
        &index,
        &Query::of(ISSUE, 300).filter(Filter::Is("state".into(), "open".into())),
        &anyone,
    );
    assert_eq!(index.position(&unlisted, &open[0], &anyone), None);
    assert_eq!(
        index.position(&Query::of(ISSUE, 7), "i001", &not_ada),
        None,
        "ada's alone"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    clippy::unreachable,
    clippy::string_slice,
    reason = "a full sort of every issue, written out beside the index it checks"
)]
fn an_order_by_several_fields_and_any_filter_pages_as_a_full_sort_would() {
    use tilo_resource_store::Absent;
    // Points repeat, so the order's later fields decide; a third of the issues hold no points.
    let mut index = Index::new();
    let mut all: Vec<(String, Value, String, String)> = Vec::new();
    for (n, readers) in (0..240).zip(["everyone", "ada"].iter().cycle()) {
        let id = format!("i{n:03}");
        let state = ["open", "doing", "done"][n % 5 % 3];
        let points = if n % 3 == 0 {
            Value::Null
        } else {
            Value::from(i64::try_from(n % 4).unwrap())
        };
        let title = format!("{} issue {n}", ["fix", "add", "drop"][n % 7 % 3]);
        let Change::Put {
            kind,
            parent,
            order,
            mut fields,
            ..
        } = issue(&id, "board", &format!("{n:04}"), &title, state, 0)
        else {
            unreachable!()
        };
        fields.insert("points".to_owned(), Cell::One(points.clone()));
        index.committed(&Commit {
            doc: readers,
            readers,
            heads: &[],
            changes: &[Change::Put {
                kind,
                id: id.clone(),
                parent,
                order,
                fields,
            }],
        });
        all.push((id, points, state.to_owned(), title));
    }
    let orders = [
        Order::by("points").then("state", false),
        Order::by("points").descending().then("state", true),
        Order::by("points")
            .absent(Absent::Last)
            .then("title", false),
        Order::by("points").descending().absent(Absent::First),
        Order::default().absent(Absent::Last).then("state", false),
    ];
    let filters = [
        vec![],
        vec![Filter::Any(vec![
            Filter::Is("state".into(), "open".into()),
            Filter::Like("title".into(), "drop*1*".into()),
        ])],
        vec![
            Filter::Present("points".into()),
            Filter::Not(Box::new(Filter::Is("state".into(), "done".into()))),
        ],
    ];
    let rank = |order: &Order,
                a: &(String, Value, String, String),
                b: &(String, Value, String, String)| {
        let field = |row: &(String, Value, String, String), name: &str| match name {
            "points" => row.1.clone(),
            "state" => Value::from(row.2.as_str()),
            "title" => Value::from(row.3.as_str()),
            _ => Value::Null,
        };
        let first = |row: &(String, Value, String, String)| match &order.by {
            tilo_resource_store::Sort::Field(name) => field(row, name),
            tilo_resource_store::Sort::Position => {
                Value::from(format!("{:04}", row.0[1..].parse::<u32>().unwrap()))
            }
        };
        let compare = |x: Value, y: Value, down: bool| match (&x, &y, order.absent) {
            (Value::Null, Value::Null, Some(_)) => std::cmp::Ordering::Equal,
            (Value::Null, _, Some(Absent::First)) | (_, Value::Null, Some(Absent::Last)) => {
                std::cmp::Ordering::Less
            }
            (Value::Null, _, Some(Absent::Last)) | (_, Value::Null, Some(Absent::First)) => {
                std::cmp::Ordering::Greater
            }
            _ if down => y.cmp(&x),
            _ => x.cmp(&y),
        };
        let mut ordering = compare(first(a), first(b), order.descending);
        for (name, down) in &order.then {
            ordering = ordering.then_with(|| compare(field(a, name), field(b, name), *down));
        }
        // Then their place: the issue's number, as its order key.
        let place = a.0.cmp(&b.0);
        ordering.then(if order.descending {
            place.reverse()
        } else {
            place
        })
    };
    let holds = |filters: &[Filter], row: &(String, Value, String, String)| {
        filters.iter().all(|filter| match filter {
            Filter::Any(_) => {
                row.2 == "open" || {
                    let t = &row.3;
                    t.starts_with("drop") && t["drop".len()..].contains('1')
                }
            }
            Filter::Present(_) => row.1 != Value::Null,
            Filter::Not(_) => row.2 != "done",
            _ => true,
        })
    };
    let not_ada = |readers: &str| readers != "ada";
    for may_read in [&anyone as &dyn Fn(&str) -> bool, &not_ada] {
        for order in &orders {
            for filter in &filters {
                let mut expected: Vec<&(String, Value, String, String)> = all
                    .iter()
                    .enumerate()
                    .filter(|(n, row)| {
                        (may_read(if n % 2 == 0 { "everyone" } else { "ada" }))
                            && holds(filter, row)
                    })
                    .map(|(_, row)| row)
                    .collect();
                expected.sort_by(|a, b| rank(order, a, b));
                let expected: Vec<String> = expected.into_iter().map(|row| row.0.clone()).collect();
                let mut query = Query::of(ISSUE, 9).order(order.clone());
                query.filters = filter.clone();
                assert_eq!(
                    listed(&index, &query, may_read),
                    expected,
                    "{order:?} {filter:?}"
                );
                assert_eq!(
                    index.position(&query, &expected[expected.len() / 2], may_read),
                    Some(expected.len() / 2)
                );
            }
        }
    }
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

/// A tracker whose issues are part of it and whose comments are a storage unit of their own.
fn tracker_surface() -> tilo_actions::surface::Surface {
    toml_like(serde_json::json!({
        "kind": "tasks.tracker",
        "label": "Tracker",
        "version": 1,
        "verbs": [
            {"id": "rename", "kind": "undoable"},
            {"id": "comment", "kind": "undoable", "writes": ["comments.*"]},
        ],
        "fields": [
            {"path": "issues.*.state", "owner": "issue", "type": "text", "readonly": true},
            {"path": "issues.*.comments", "owner": "issue", "stored": "collection",
             "type": {"collection": {"named": "Comment"}}, "unit": true},
        ],
        "types": [
            {"name": "Tracker", "fields": [
                {"name": "name", "type": "text"},
                {"name": "issues", "type": {"collection": {"named": "Issue"}}},
            ]},
            {"name": "Issue", "fields": [
                {"name": "title", "type": "text"},
                {"name": "state", "type": "text"},
                {"name": "comments", "type": {"collection": {"named": "Comment"}}},
            ]},
            {"name": "Comment", "fields": [{"name": "text", "type": "text"}]},
        ],
    }))
}

#[test]
fn a_verb_writes_what_its_type_holds_and_the_units_it_declares() {
    use tilo_resource_store::FIELD_EDIT;
    let schema = Schema::from_surface(&tracker_surface());
    let kinds =
        |verb: &str| -> Vec<String> { schema.verb(verb).unwrap().writes.keys().cloned().collect() };
    assert_eq!(kinds("rename"), ["Issue", "Tracker"]);
    assert_eq!(kinds("comment"), ["Comment", "Issue", "Tracker"]);
    let edit = schema.verb(FIELD_EDIT).unwrap();
    assert!(edit.writes["Issue"].sets_only);
    assert!(edit.writes["Issue"].fields.contains("title"));
    assert!(!edit.writes["Issue"].fields.contains("state"));
    assert!(edit.writes["Comment"].fields.contains("text"));
    assert!(!schema.verb("comment").unwrap().writes["Comment"].sets_only);
}

fn toml_like(value: serde_json::Value) -> tilo_actions::surface::Surface {
    serde_json::from_value(value).expect("a surface")
}

mod store {
    use super::*;
    use tilo_resource_store::store::DOCUMENT_LIMIT;
    use tilo_resource_store::{Proposal, Received, Resource, Stamp, Store, Written};

    const MEMBERS: &str = "*";

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
        store.put(ISSUE, id, resource, &Stamp::default()).unwrap()
    }

    /// Carries every change written to `to`, as sync does.
    fn send(written: &[Written], to: &mut Store) {
        for write in written {
            let refused = to
                .receive(
                    &write.doc,
                    &write.readers,
                    vec![Received {
                        change: write.change.clone(),
                        author: [0; 32],
                    }],
                    &|_: &Proposal<'_>| true,
                )
                .unwrap();
            assert!(refused.is_empty(), "{refused:?}");
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
        let gone = here.remove(ISSUE, "a", &Stamp::default()).unwrap();
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
        assert_eq!(docs[0], ("*/board#0", MEMBERS));
        let (ada_only, readers) = docs[1];
        assert_eq!(readers, "ada-only");
        assert!(
            ada_only.ends_with("/board#0") && !ada_only.contains("ada"),
            "a reader set other than the group is named by a digest: {ada_only}"
        );
        let ada_only = ada_only.to_owned();
        send(&moved, &mut there);
        let members = |readers: &str| readers == MEMBERS;
        for store in [&here, &there] {
            assert_eq!(store.count(ISSUE, &[], &members), 0);
            assert_eq!(store.count(ISSUE, &[], &anyone), 1);
            assert_eq!(store.index().get(ISSUE, "a").unwrap().doc, ada_only);
        }
    }

    #[test]
    fn content_is_written_beside_its_resource_and_moves_with_its_readers() {
        use automerge::marks::{ExpandMark, Mark};
        use automerge::transaction::Transactable as _;
        use automerge::{ObjType, ReadDoc as _};

        let mut here = Store::new();
        let mut there = Store::new();
        let mut written = put(&mut here, "a", resource("board", "a0", "Draft", "open"));
        let ((), body) = here
            .edit_content(ISSUE, "a", &Stamp::default(), |tx, root| {
                let text = tx.put_object(root, "body", ObjType::Text)?;
                tx.splice_text(&text, 0, 0, "Hello world")?;
                tx.mark(
                    &text,
                    Mark::new("bold".into(), true, 0, 5),
                    ExpandMark::After,
                )?;
                let steps = tx.put_object(root, "steps", ObjType::List)?;
                tx.insert(&steps, 0, "first")?;
                Ok(())
            })
            .unwrap();
        let body = body.expect("a change");
        assert_eq!(
            body.readers, MEMBERS,
            "content is read by its resource's readers"
        );
        assert_ne!(
            body.doc,
            here.index().get(ISSUE, "a").unwrap().doc,
            "content is in a document of its own"
        );
        written.push(body);
        // An edit that changes nothing writes nothing.
        let ((), none) = here
            .edit_content(ISSUE, "a", &Stamp::default(), |_, _| Ok(()))
            .unwrap();
        assert!(none.is_none());

        send(&written, &mut there);
        let text_of = |store: &mut Store| {
            let (doc, root) = store.content(ISSUE, "a").unwrap()?;
            let (_, text) = doc.get(&root, "body").unwrap()?;
            let marks = doc.marks(&text).unwrap();
            Some((doc.text(&text).unwrap(), marks.len()))
        };
        assert_eq!(text_of(&mut there), Some(("Hello world".to_owned(), 1)));

        // Its readers change: the content moves to a document of theirs, and the old one is
        // emptied, so a member who lost access is sent nothing more of it.
        let moved = put(
            &mut here,
            "a",
            read_by("ada-only", "board", "a0", "Draft", "open"),
        );
        let emptied = moved
            .iter()
            .find(|write| write.readers == MEMBERS && write.doc.contains('~'))
            .expect("the old content emptied");
        assert!(
            moved
                .iter()
                .any(|write| write.readers == "ada-only" && write.doc.contains('~')),
            "the content copied for its new readers"
        );
        assert_eq!(text_of(&mut here), Some(("Hello world".to_owned(), 1)));
        send(&moved, &mut there);
        assert_eq!(text_of(&mut there), Some(("Hello world".to_owned(), 1)));
        let old = there.document(&emptied.doc).unwrap();
        assert_eq!(old.length(automerge::ROOT), 0, "the old content is emptied");
    }

    /// Carries `written` to `to` as sync does, judged by `admit`; answers why each refused one
    /// was refused.
    fn deliver(
        written: &[Written],
        to: &mut Store,
        admit: &dyn tilo_resource_store::Admit,
    ) -> Vec<tilo_resource_store::Refusal> {
        let mut refused = Vec::new();
        for write in written {
            refused.extend(
                to.receive(
                    &write.doc,
                    &write.readers,
                    vec![Received {
                        change: write.change.clone(),
                        author: [7; 32],
                    }],
                    admit,
                )
                .unwrap()
                .into_iter()
                .map(|(_, why)| why),
            );
        }
        refused
    }

    fn stamp(verb: &str, invocation: &str) -> Stamp {
        Stamp {
            verb: verb.to_owned(),
            invoker: "ada".to_owned(),
            invocation: invocation.to_owned(),
            membership: vec!["m1".to_owned()],
        }
    }

    #[test]
    fn a_refused_change_has_no_effect_and_neither_has_one_built_on_it() {
        use tilo_resource_store::Refusal;
        let mut here = Store::new();
        let mut there = Store::new();
        // The author may add and edit, never close.
        let admit = |proposal: &Proposal<'_>| {
            assert_eq!(proposal.author, &[7; 32]);
            proposal
                .stamp
                .is_some_and(|stamp| stamp.verb != "close" && stamp.membership == ["m1"])
        };
        let made = here
            .put(
                ISSUE,
                "a",
                resource("board", "a0", "First", "open"),
                &stamp("add", "i1"),
            )
            .unwrap();
        assert!(deliver(&made, &mut there, &admit).is_empty());
        let closed = here
            .put(
                ISSUE,
                "a",
                resource("board", "a0", "First", "done"),
                &stamp("close", "i2"),
            )
            .unwrap();
        assert_eq!(
            deliver(&closed, &mut there, &admit),
            [Refusal::NotPermitted]
        );
        let renamed = here
            .put(
                ISSUE,
                "a",
                resource("board", "a0", "Second", "done"),
                &stamp("edit", "i3"),
            )
            .unwrap();
        assert_eq!(deliver(&renamed, &mut there, &admit), [Refusal::OnRefused]);
        let held = there.index().get(ISSUE, "a").unwrap();
        assert_eq!(held.scalar("state"), Value::from("open"));
        assert_eq!(held.scalar("title"), Value::from("First"));
        assert_eq!(there.refused(&made[0].doc).count(), 2);
        // A refused change sent again stays refused, and changes nothing.
        assert!(deliver(&closed, &mut there, &admit).is_empty());
        assert_eq!(
            there.index().get(ISSUE, "a").unwrap().scalar("state"),
            Value::from("open")
        );
    }

    #[test]
    fn a_change_writes_only_what_the_verb_it_names_declares() {
        use tilo_resource_store::Refusal;
        let mut there = Store::new();
        there.declare(Schema::from_surface(&super::tracker_surface()));
        let anyone = |_: &Proposal<'_>| true;
        let comment = |stamp: Stamp, id: &str| {
            let mut fields = BTreeMap::new();
            fields.insert("text".to_owned(), Cell::One("Looks right".into()));
            let resource = Resource {
                readers: MEMBERS.to_owned(),
                parent: None,
                order: None,
                fields,
            };
            // Each from a device of its own, so none is built on another's refusal.
            Store::new().put("Comment", id, resource, &stamp).unwrap()
        };
        assert_eq!(
            deliver(&comment(stamp("comment", "c1"), "c1"), &mut there, &anyone),
            []
        );
        assert!(matches!(
            deliver(&comment(stamp("rename", "c2"), "c2"), &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("Comment")
        ));
        assert!(matches!(
            deliver(&comment(stamp(tilo_resource_store::FIELD_EDIT, "c3"), "c3"), &mut there, &anyone)
                .as_slice(),
            [Refusal::Undeclared(why)] if why.contains("only sets")
        ));
        assert!(matches!(
            deliver(&comment(Stamp::default(), "c4"), &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("verb")
        ));
        let all = |_: &str| true;
        assert!(there.get("Comment", "c1", &all).is_some());
        assert!(there.get("Comment", "c2", &all).is_none());
    }

    #[test]
    fn a_change_the_declared_types_do_not_allow_is_refused() {
        use tilo_resource_store::{Expected, Field, FieldKind, Refusal, Schema, Type};
        let field = |name: &str, kind, value, optional| Field {
            name: name.to_owned(),
            kind,
            value,
            optional,
        };
        let schema = Schema {
            types: vec![Type {
                name: ISSUE.to_owned(),
                fields: vec![
                    field("title", FieldKind::Scalar, Expected::Text, false),
                    field(
                        "state",
                        FieldKind::Scalar,
                        Expected::OneOf(vec!["open".into(), "done".into()]),
                        false,
                    ),
                    field("labels", FieldKind::Set, Expected::Text, true),
                ],
                nested: Vec::new(),
            }],
            verbs: Vec::new(),
        };
        let mut here = Store::new();
        let mut there = Store::new();
        there.declare(schema);
        let anyone = |_: &Proposal<'_>| true;
        let made = put(&mut here, "a", resource("board", "a0", "First", "open"));
        assert!(deliver(&made, &mut there, &anyone).is_empty());
        // Each written by another device, each into a document of its own, so neither is built
        // on the other's refusal.
        let lost = put(
            &mut Store::new(),
            "b",
            resource("sprint", "a1", "Second", "lost"),
        );
        assert!(matches!(
            deliver(&lost, &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("state")
        ));
        let mut secret = resource("column", "a2", "Third", "open");
        secret
            .fields
            .insert("secret".to_owned(), Cell::One(1.into()));
        let secret = put(&mut Store::new(), "c", secret);
        assert!(matches!(
            deliver(&secret, &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("secret")
        ));
        assert_eq!(there.count(ISSUE, &[], &|_| true), 1);
    }

    #[test]
    fn two_devices_starting_one_document_at_once_both_keep_what_they_wrote() {
        let mut here = Store::new();
        let mut there = Store::new();
        let mine = put(&mut here, "a", resource("board", "a0", "Mine", "open"));
        let theirs = put(&mut there, "b", resource("board", "a1", "Theirs", "open"));
        assert_eq!(mine[0].doc, theirs[0].doc, "one document, started twice");
        send(&theirs, &mut here);
        send(&mine, &mut there);
        for store in [&here, &there] {
            assert_eq!(listed(store, &Query::of(ISSUE, 10), &anyone), ["a", "b"]);
        }
    }

    #[test]
    fn a_change_that_arrives_before_the_one_it_is_built_on_waits_for_it() {
        let mut here = Store::new();
        let mut there = Store::new();
        let anyone = |_: &Proposal<'_>| true;
        let first = put(&mut here, "a", resource("board", "a0", "First", "open"));
        let second = put(&mut here, "a", resource("board", "a0", "First", "done"));
        assert!(deliver(&second, &mut there, &anyone).is_empty());
        assert_eq!(there.count(ISSUE, &[], &|_| true), 0, "it waits");
        assert!(deliver(&first, &mut there, &anyone).is_empty());
        assert_eq!(
            there.index().get(ISSUE, "a").unwrap().scalar("state"),
            Value::from("done")
        );
    }

    #[test]
    fn a_content_edit_is_judged_as_one_of_its_resource() {
        use automerge::transaction::Transactable as _;
        let mut here = Store::new();
        let mut there = Store::new();
        let made = put(&mut here, "a", resource("board", "a0", "Draft", "open"));
        let anyone = |_: &Proposal<'_>| true;
        assert!(deliver(&made, &mut there, &anyone).is_empty());
        let ((), body) = here
            .edit_content(ISSUE, "a", &stamp("write", "i9"), |tx, root| {
                tx.put(root, "note", "hello")?;
                Ok(())
            })
            .unwrap();
        let seen = std::cell::RefCell::new(None);
        let watch = |proposal: &Proposal<'_>| {
            *seen.borrow_mut() = Some(proposal.touched.clone());
            proposal.stamp.is_some_and(|stamp| stamp.invocation == "i9")
        };
        assert!(deliver(&[body.unwrap()], &mut there, &watch).is_empty());
        let touched = seen.into_inner().unwrap();
        let touch = touched.resources.get("a").unwrap();
        assert!(touch.content && touch.fields.is_empty());
        assert_eq!(touch.kind.as_deref(), Some(ISSUE));
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
            ["*/board#0", "*/board#1"]
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
        let held = store.document("*/board#0").unwrap();
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
