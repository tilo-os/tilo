//! The index as the store drives it: commits put, move and remove resources, and queries page,
//! filter, sort and count what the commits left.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

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
fn any_of_several_values_lists_and_counts_each_holder_of_one() {
    // What a resource is mentioned by, under each of its names: a set member or a scalar.
    let index = board();
    let filters = vec![Filter::Any(vec![
        Filter::Has("labels".into(), "open".into()),
        Filter::Is("title".into(), "Issue 3".into()),
        Filter::Has("labels".into(), "nothing holds this".into()),
    ])];
    let mut wanted: Vec<String> = (0..10)
        .filter(|n| n % 2 == 0 || *n == 3)
        .map(|n| format!("i{n}"))
        .collect();
    wanted.sort();
    let mut query = Query::of(ISSUE, 2);
    query.filters.clone_from(&filters);
    let mut shown = listed(&index, &query, &anyone);
    shown.sort();
    assert_eq!(shown, wanted);
    assert_eq!(count(&index, ISSUE, &filters), wanted.len());
    // One alternative the index keeps no values of leaves the rest to the filter itself.
    let mut walked = filters;
    if let Some(Filter::Any(any)) = walked.first_mut() {
        any.push(Filter::Not(Box::new(Filter::Present("points".into()))));
    }
    query.filters = walked;
    let mut shown = listed(&index, &query, &anyone);
    shown.sort();
    assert_eq!(shown, wanted);
}

#[test]
fn several_values_asked_together_find_their_holders_in_either_order() {
    // The parent every issue holds and a title one holds: the answer is the same whichever is
    // asked first, and a value no resource holds answers nothing.
    let index = board();
    let parent = Filter::Is(PARENT.into(), "board".into());
    let title = Filter::Is("title".into(), "Issue 3".into());
    for filters in [
        vec![parent.clone(), title.clone()],
        vec![title, parent.clone()],
    ] {
        assert_eq!(count(&index, ISSUE, &filters), 1);
        let mut query = Query::of(ISSUE, 5);
        query.filters = filters;
        assert_eq!(listed(&index, &query, &anyone), ["i3"]);
    }
    let none = Filter::Is("title".into(), "nothing holds this".into());
    assert_eq!(count(&index, ISSUE, &[parent, none]), 0);
}

#[test]
fn a_listing_kept_across_writes_answers_as_one_read_after_them() {
    use tilo_resource_store::{Absent, Change};
    // The index keeps a listing of several filters over hundreds of resources once it is asked
    // for, and each write moves the
    // resources it changed in or out of it. One index is asked before the writes and keeps the
    // listing through them; the other is asked only after, and reads the listing whole.
    let queries = [
        Query::of(ISSUE, 5)
            .filter(Filter::Is(PARENT.into(), "board".into()))
            .filter(Filter::Not(Box::new(Filter::Is(
                "state".into(),
                "done".into(),
            )))),
        Query::of(ISSUE, 5)
            .filter(Filter::Is(PARENT.into(), "board".into()))
            .filter(Filter::Not(Box::new(Filter::Is(
                "state".into(),
                "done".into(),
            ))))
            .order(Order::by("points").absent(Absent::Last).then("title", true)),
    ];
    let seed = |index: &mut Index| {
        let changes: Vec<Change> = (0..1200)
            .map(|n| {
                issue(
                    &format!("i{n:04}"),
                    if n % 4 == 0 { "other" } else { "board" },
                    &format!("{n:04}"),
                    &format!("Issue {n}"),
                    ["open", "doing", "done"][n % 3],
                    i64::try_from(n % 7).unwrap(),
                )
            })
            .collect();
        commit(index, "doc-1", &changes);
    };
    let writes = |index: &mut Index| {
        for n in (0..1200).step_by(5) {
            // Done, then open again with other points, moves it out of the listing and back.
            let state = if n % 10 == 0 { "done" } else { "open" };
            let change = issue(
                &format!("i{n:04}"),
                "board",
                &format!("{n:04}"),
                &format!("Issue {n}"),
                state,
                i64::from(n % 13),
            );
            commit(index, "doc-1", &[change]);
        }
        commit(
            index,
            "doc-1",
            &[Change::Remove {
                kind: ISSUE.to_owned(),
                id: "i0007".to_owned(),
            }],
        );
    };
    let (mut kept, mut fresh) = (Index::new(), Index::new());
    seed(&mut kept);
    seed(&mut fresh);
    for query in &queries {
        assert!(
            kept.len(query, &anyone) > 0,
            "the listing is kept from here on"
        );
    }
    writes(&mut kept);
    writes(&mut fresh);
    for query in &queries {
        let all = listed(&fresh, query, &anyone);
        assert!(
            !all.contains(&"i0007".to_owned()),
            "a removed issue leaves it"
        );
        assert!(
            !all.contains(&"i0010".to_owned()),
            "an issue done leaves it"
        );
        assert!(
            all.contains(&"i0005".to_owned()),
            "an issue moved to it joins it"
        );
        assert_eq!(listed(&kept, query, &anyone), all, "{query:?}");
        assert_eq!(kept.len(query, &anyone), all.len(), "{query:?}");
        for (position, id) in all.iter().enumerate() {
            assert_eq!(
                kept.position(query, id, &anyone),
                Some(position),
                "{query:?} {id}"
            );
        }
        for start in [0, 3, all.len().saturating_sub(2)] {
            assert_eq!(
                kept.at(query, start, &anyone).ids().collect::<Vec<_>>(),
                all.iter()
                    .skip(start)
                    .take(5)
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                "{query:?} at {start}"
            );
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

/// A tilolang package's surface, as `package_surfaces` writes one: its record is described by
/// the fields it lists, under the ids a document stores them by, and `types` names its enums
/// only. The record is the surface's own kind, and no `types` entry names it.
fn tilolang_surface() -> tilo_actions::surface::Surface {
    toml_like(serde_json::json!({
        "kind": "tl.so.tilo.lang.gallery.Gallery",
        "label": "Gallery",
        "version": 1,
        "verbs": [
            {"id": "$create", "kind": "undoable"},
            {"id": "edit-text", "kind": "undoable"},
        ],
        "fields": [
            {"path": "bmdkc22s", "owner": "gallery", "title": "name", "type": "text"},
            {"path": "5fbblehc", "owner": "gallery", "title": "backlog_items",
             "stored": "collection",
             "type": {"collection": {"resource": "tl.so.tilo.lang.gallery.Item"}}},
            {"path": "5fbblehc.*.y5gdkqew", "owner": "item", "title": "title", "type": "text"},
        ],
        "types": [
            {"name": "Urgency", "cases": [{"name": "none"}, {"name": "low"}]},
        ],
    }))
}

/// The surface of a tilolang package's list type: `add` and `move` write the list, and the items
/// they make and move are the kind its collection nests, declared by a surface of its own.
fn tilolang_list_surface() -> tilo_actions::surface::Surface {
    toml_like(serde_json::json!({
        "kind": "tl.so.tilo.todo.TodoList",
        "label": "TodoList",
        "version": 1,
        "verbs": [
            {"id": "$create", "kind": "undoable"},
            {"id": "add", "kind": "undoable"},
            {"id": "move", "kind": "undoable"},
            {"id": "remove", "kind": "undoable"},
            {"id": "mention", "kind": "undoable"},
        ],
        "fields": [
            {"path": "n1", "owner": "todolist", "title": "name", "type": "text"},
            {"path": "i5", "owner": "todolist", "title": "items", "stored": "collection",
             "type": {"collection": {"resource": "tl.so.tilo.todo.Todo"}}},
            {"path": "i5.*.t7", "owner": "todo", "title": "title", "type": "text"},
        ],
    }))
}

/// The surface of the type the list's items are: a record of its own app, with a collection of
/// comments of a third.
fn tilolang_todo_surface() -> tilo_actions::surface::Surface {
    toml_like(serde_json::json!({
        "kind": "tl.so.tilo.todo.Todo",
        "label": "Todo",
        "version": 1,
        "verbs": [
            {"id": "$create", "kind": "undoable"},
            {"id": "toggle", "kind": "undoable"},
        ],
        "fields": [
            {"path": "t7", "owner": "todo", "title": "title", "type": "text"},
            {"path": "d1", "owner": "todo", "title": "done", "type": "bool"},
            {"path": "c3", "owner": "todo", "title": "comments", "stored": "collection",
             "type": {"collection": {"resource": "tl.so.tilo.todo.Comment"}}},
        ],
    }))
}

/// The surface of the type a todo's comments are.
fn tilolang_comment_surface() -> tilo_actions::surface::Surface {
    toml_like(serde_json::json!({
        "kind": "tl.so.tilo.todo.Comment",
        "label": "Comment",
        "version": 1,
        "verbs": [{"id": "$create", "kind": "undoable"}],
        "fields": [
            {"path": "x1", "owner": "comment", "title": "text", "type": "text"},
        ],
    }))
}

/// The surface of a tilolang package's item type, as `package_surfaces` writes one: `$create`
/// and a title a document keeps under its name.
fn tilolang_item_surface() -> tilo_actions::surface::Surface {
    toml_like(serde_json::json!({
        "kind": "tl.so.tilo.lang.gallery.Item",
        "label": "Item",
        "version": 1,
        "verbs": [
            {"id": "$create", "kind": "undoable"},
        ],
        "fields": [
            {"path": "y5gdkqew", "owner": "item", "title": "title", "type": "text"},
        ],
    }))
}

/// The surface of a tilolang package whose record holds one record of another kind in a field
/// (`stored = "record"`), not in a collection.
fn tilolang_holding_surface() -> tilo_actions::surface::Surface {
    toml_like(serde_json::json!({
        "kind": "tl.so.tilo.clocks.Clocks",
        "label": "Clocks",
        "version": 1,
        "verbs": [{"id": "$create", "kind": "undoable"}],
        "fields": [
            {"path": "n1", "owner": "clocks", "title": "name", "type": "text"},
            {"path": "s2", "owner": "clocks", "title": "stopwatch", "stored": "record",
             "type": {"resource": "tl.so.tilo.clocks.Stopwatch"}},
        ],
    }))
}

/// The surface of the record [`tilolang_holding_surface`]'s field holds.
fn tilolang_held_surface() -> tilo_actions::surface::Surface {
    toml_like(serde_json::json!({
        "kind": "tl.so.tilo.clocks.Stopwatch",
        "label": "Stopwatch",
        "version": 1,
        "verbs": [{"id": "$create", "kind": "undoable"}],
        "fields": [
            {"path": "l3", "owner": "stopwatch", "title": "label", "type": "text"},
        ],
    }))
}

/// A record whose collection holds a union of types nests each of them: a channel, a category
/// and a conversation each stand in the one list a chat's root keeps them in.
#[test]
fn a_collection_of_a_union_nests_each_of_its_types() {
    let surface: tilo_actions::surface::Surface = toml_like(serde_json::json!({
        "kind": "tl.so.tilo.chat.Conversations",
        "label": "Conversations",
        "version": 1,
        "verbs": [{"id": "$create", "kind": "undoable"}],
        "fields": [
            {"path": "c1", "owner": "conversations", "title": "channels", "stored": "collection",
             "type": {"collection": {"union": [
                 {"resource": "tl.so.tilo.chat.Category"},
                 {"resource": "tl.so.tilo.chat.TextChannel"},
             ]}}},
        ],
    }));
    let schema = Schema::from_surface(&surface);
    assert_eq!(
        schema.get("tl.so.tilo.chat.Conversations").unwrap().nested,
        [
            ("channels".to_owned(), "tl.so.tilo.chat.Category".to_owned()),
            (
                "channels".to_owned(),
                "tl.so.tilo.chat.TextChannel".to_owned()
            ),
        ]
    );
}

/// A surface that names no type of its own kind still declares the record it describes: the store
/// knows the kind its resources are, the fields they hold, and what each verb writes of them.
#[test]
fn a_surface_describes_its_own_record_and_what_its_verbs_write() {
    let schema = Schema::from_surface(&tilolang_surface());
    let kind = "tl.so.tilo.lang.gallery.Gallery";
    let gallery = schema
        .get(kind)
        .expect("the surface's own record is declared");
    let fields: Vec<&str> = gallery
        .fields
        .iter()
        .map(|field| field.name.as_str())
        .collect();
    assert_eq!(
        fields,
        ["name"],
        "its scalar fields, by the names a document keeps them under"
    );
    assert_eq!(
        gallery.nested,
        [(
            "backlog_items".to_owned(),
            "tl.so.tilo.lang.gallery.Item".to_owned()
        )],
        "the kinds it nests"
    );
    for verb in ["$create", "edit-text"] {
        let writes = schema.verb(verb).expect("the verb is declared");
        let own = writes
            .writes
            .get(kind)
            .unwrap_or_else(|| panic!("{verb} writes the surface's own kind: {writes:?}"));
        assert!(
            own.fields.contains("name"),
            "{verb} writes its fields: {own:?}"
        );
    }
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

    /// A store that declares the issues these tests write, as every workspace store declares the
    /// types of the surfaces it holds: one that declares none admits no change it receives.
    fn issues() -> Store {
        let field = |name: &str, kind: FieldKind| tilo_resource_store::Field {
            name: name.to_owned(),
            kind,
            value: tilo_resource_store::Expected::Any,
            optional: true,
        };
        let mut store = Store::new();
        store.declare(Schema {
            types: vec![tilo_resource_store::Type {
                name: ISSUE.to_owned(),
                fields: vec![
                    field("title", FieldKind::Scalar),
                    field("state", FieldKind::Scalar),
                    field("labels", FieldKind::Set),
                ],
                nested: Vec::new(),
                live: None,
            }],
            ..Schema::default()
        });
        store
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

    /// A type `name` whose records hold what [`fields`] writes.
    fn record_type(name: &str) -> tilo_resource_store::Type {
        use tilo_resource_store::{Expected, Field, FieldKind};
        let field = |name: &str, kind| Field {
            name: name.to_owned(),
            kind,
            value: Expected::Text,
            optional: true,
        };
        tilo_resource_store::Type {
            name: name.to_owned(),
            fields: vec![
                field("title", FieldKind::Scalar),
                field("state", FieldKind::Scalar),
                field("labels", FieldKind::Set),
            ],
            nested: Vec::new(),
            live: None,
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
                        claimed: None,
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

    /// What a write costs follows what it touched, never how much its kind holds: retitling one
    /// issue of a hundred and one of three thousand writes one change to one document, of the
    /// same operations, and the index answers the new title either way.
    #[test]
    fn a_write_costs_what_it_touched_whatever_its_kind_holds() {
        let retitled = |held: usize| {
            let mut store = issues();
            for n in 0..held {
                put(
                    &mut store,
                    &format!("i{n}"),
                    resource("board", &format!("a{n:05}"), "Issue", "open"),
                );
            }
            let written = put(
                &mut store,
                "i7",
                resource("board", "a00007", "Renamed", "open"),
            );
            assert_eq!(written.len(), 1, "one document is written");
            let found = store.query(
                &Query::of(ISSUE, 10).filter(Filter::Is("title".to_owned(), "Renamed".into())),
                &anyone,
            );
            assert_eq!(found.ids().collect::<Vec<_>>(), ["i7"]);
            let change = &written[0].change;
            (change.len(), change.raw_bytes().len())
        };
        let (small_ops, small_bytes) = retitled(100);
        let (large_ops, large_bytes) = retitled(3_000);
        assert_eq!(large_ops, small_ops, "the same operations at either size");
        assert!(
            large_bytes.abs_diff(small_bytes) <= 8,
            "the change is as large at 3000 issues ({large_bytes} bytes) as at 100 ({small_bytes})"
        );
    }

    /// A type that says when one of it is live keeps a row for each that is, derived from what
    /// the store holds: it comes with the change that made it live, naming that change's device
    /// and time, stays as it was while the resource stays live, goes when it stops, and is read
    /// only by whom its document lets read it.
    #[test]
    fn a_resource_is_live_while_its_types_condition_holds_and_read_as_its_document_is() {
        use tilo_resource_store::{Expected, Field, Type};
        let mut store = Store::new();
        store.declare(Schema {
            types: vec![Type {
                name: ISSUE.to_owned(),
                fields: ["title", "state"]
                    .map(|name| Field {
                        name: name.to_owned(),
                        kind: FieldKind::Scalar,
                        value: Expected::Any,
                        optional: true,
                    })
                    .to_vec(),
                nested: Vec::new(),
                live: Some(vec![Filter::Is("state".to_owned(), "playing".into())]),
            }],
            ..Schema::default()
        });
        let on = |device: &str, name: &str, at: &str| Stamp {
            device: device.to_owned(),
            device_name: name.to_owned(),
            at: at.to_owned(),
            ..Stamp::default()
        };
        let song = |title: &str, state: &str| read_by("ada", "board", "a0", title, state);
        let ada = |readers: &str| readers == "ada";
        let bob = |readers: &str| readers == "bob";

        store
            .put(
                ISSUE,
                "song",
                song("Song", "stopped"),
                &on("aa", "Laptop", "2026-11-23T10:00:00Z"),
            )
            .unwrap();
        assert!(store.live(&ada).is_empty(), "a stopped track is not live");
        let before = store.live_revision();

        store
            .put(
                ISSUE,
                "song",
                song("Song", "playing"),
                &on("bb", "Phone", "2026-11-23T10:01:00Z"),
            )
            .unwrap();
        let live = store.live(&ada);
        assert_eq!(
            live.iter()
                .map(|row| (
                    row.kind.as_str(),
                    row.id.as_str(),
                    row.device.as_str(),
                    row.device_name.as_str(),
                    row.since.as_str()
                ))
                .collect::<Vec<_>>(),
            [(ISSUE, "song", "bb", "Phone", "2026-11-23T10:01:00Z")],
            "the change that made it live names where and when"
        );
        assert_ne!(
            store.live_revision(),
            before,
            "whoever shows the rows is told"
        );
        assert!(
            store.live(&bob).is_empty(),
            "who may not read the track sees no row of it"
        );

        store
            .put(
                ISSUE,
                "song",
                song("Song, live", "playing"),
                &on("cc", "Tablet", "2026-11-23T10:02:00Z"),
            )
            .unwrap();
        assert_eq!(
            store.live(&ada)[0].device,
            "bb",
            "a change that leaves it live leaves its row as it was"
        );

        store
            .put(
                ISSUE,
                "song",
                song("Song, live", "stopped"),
                &on("cc", "Tablet", "2026-11-23T10:03:00Z"),
            )
            .unwrap();
        assert!(store.live(&ada).is_empty(), "stopped, it is live no more");
    }

    /// A write taken back leaves the store as it stood before it: a changed resource reads
    /// its earlier fields, one the write made is gone and not counted as removed, and an open
    /// query over the kind is told.
    #[test]
    fn a_write_taken_back_leaves_the_store_as_it_was() {
        let mut store = issues();
        let held = put(&mut store, "a", resource("board", "a0", "Kept", "open"));
        let watch = store.watch(ISSUE);
        let _ = store.changed();
        let renamed = put(&mut store, "a", resource("board", "a0", "Unsent", "open"));
        let made = put(&mut store, "b", resource("board", "a1", "Unsent", "open"));
        assert_eq!(made[0].doc, held[0].doc, "both resources share a document");
        for write in renamed.iter().chain(&made).rev() {
            store.take_back(&write.doc, write.change.deps()).unwrap();
        }
        let title = |store: &Store, id: &str| {
            store
                .get(ISSUE, id, &anyone)
                .and_then(|held| held.fields.get("title").cloned())
        };
        assert_eq!(title(&store, "a"), Some(Cell::One("Kept".into())));
        assert!(
            store.get(ISSUE, "b", &anyone).is_none(),
            "the made resource is gone"
        );
        assert!(
            store.removed(ISSUE, None, &anyone).is_empty(),
            "nothing was removed"
        );
        assert!(store.changed().contains(&watch), "the open query is told");

        let founded = put(
            &mut store,
            "c",
            read_by("ada", "board", "a0", "Alone", "open"),
        );
        assert_ne!(
            founded[0].doc, held[0].doc,
            "another reader set founds its document"
        );
        store
            .take_back(&founded[0].doc, founded[0].change.deps())
            .unwrap();
        assert!(store.get(ISSUE, "c", &anyone).is_none());
        assert!(
            store.document(&founded[0].doc).is_none(),
            "the founded document is let go"
        );
    }

    /// The first and the last version of a resource read alone are the ends of its whole
    /// history, for a root whose history is mostly what was put in it and taken out of it, at
    /// every step, its own removal last.
    #[test]
    fn the_ends_of_a_history_read_alone_are_those_of_the_whole_of_it() {
        let mut store = issues();
        let at = |invocation: &str, moment: u64| Stamp {
            invocation: invocation.to_owned(),
            moment,
            ..Stamp::default()
        };
        let root = |title: &str| Resource {
            readers: MEMBERS.to_owned(),
            parent: None,
            order: None,
            fields: fields(title, "open"),
        };
        let ends_agree = |store: &Store, step: &str| {
            for id in ["board", "board/a", "board/b"] {
                let all = store.versions(ISSUE, id);
                assert_eq!(
                    store.version_end(ISSUE, id, false),
                    all.first().cloned(),
                    "{id}'s first version after {step}"
                );
                assert_eq!(
                    store.version_end(ISSUE, id, true),
                    all.last().cloned(),
                    "{id}'s last version after {step}"
                );
            }
        };
        store
            .put(ISSUE, "board", root("Board"), &at("make", 1))
            .unwrap();
        ends_agree(&store, "the making");
        for (id, moment) in [("board/a", 2), ("board/b", 3)] {
            store
                .put(
                    ISSUE,
                    id,
                    resource("board", id, id, "open"),
                    &at(id, moment),
                )
                .unwrap();
            ends_agree(&store, id);
        }
        store
            .put(ISSUE, "board", root("Renamed"), &at("rename", 4))
            .unwrap();
        ends_agree(&store, "the rename");
        store
            .put(
                ISSUE,
                "board/a",
                resource("board", "a", "Changed", "done"),
                &at("change", 5),
            )
            .unwrap();
        ends_agree(&store, "an item's change");
        store.remove(ISSUE, "board/b", &at("take out", 6)).unwrap();
        ends_agree(&store, "an item's removal");
        let last = store.version_end(ISSUE, "board", true).unwrap();
        assert_eq!(
            last.stamp.map(|stamp| stamp.invocation).as_deref(),
            Some("take out")
        );
        assert!(
            !last.nested.is_empty(),
            "the root's last version names what it took out"
        );
        store.remove(ISSUE, "board/a", &at("empty", 7)).unwrap();
        store.remove(ISSUE, "board", &at("remove", 8)).unwrap();
        ends_agree(&store, "the root's removal");
    }

    /// The newest version of each resource under a document, most recent first, a page of
    /// them: what changed last stands first, a removed one too, and a resource elsewhere is not
    /// among them.
    #[test]
    fn the_newest_versions_under_a_document_are_read_most_recent_first_a_page_at_a_time() {
        use tilo_resource_store::wire::{Command, Reply, read};
        let mut store = issues();
        let at = |invocation: &str, moment: u64| Stamp {
            invocation: invocation.to_owned(),
            moment,
            ..Stamp::default()
        };
        for (id, moment) in [
            ("board/a", 1),
            ("board/b", 2),
            ("board/c", 3),
            ("elsewhere/d", 4),
        ] {
            store
                .put(
                    ISSUE,
                    id,
                    resource("board", id, id, "open"),
                    &at(id, moment),
                )
                .unwrap();
        }
        // The first is changed last, and the third removed after that.
        store
            .put(
                ISSUE,
                "board/a",
                resource("board", "a", "First", "done"),
                &at("change", 5),
            )
            .unwrap();
        store.remove(ISSUE, "board/c", &at("remove", 6)).unwrap();
        let newest = |limit| match read(
            &store,
            &Command::Newest {
                within: "board".to_owned(),
                limit,
            },
            &anyone,
        ) {
            Some(Reply::Newest { versions, more }) => (
                versions
                    .into_iter()
                    .map(|(_, id, version)| {
                        (
                            id,
                            version
                                .stamp
                                .map(|stamp| stamp.invocation)
                                .unwrap_or_default(),
                        )
                    })
                    .collect::<Vec<_>>(),
                more,
            ),
            other => panic!("the newest versions are read: {other:?}"),
        };
        let owned = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(id, invocation)| ((*id).to_owned(), (*invocation).to_owned()))
                .collect()
        };
        assert_eq!(
            newest(10),
            (
                owned(&[
                    ("board/c", "remove"),
                    ("board/a", "change"),
                    ("board/b", "board/b")
                ]),
                false
            )
        );
        assert_eq!(
            newest(2),
            (owned(&[("board/c", "remove"), ("board/a", "change")]), true),
            "a page, saying there are more"
        );
    }

    #[test]
    fn a_write_is_queried_at_once_and_reaches_a_peer() {
        let mut here = issues();
        let mut there = issues();
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
        let mut here = issues();
        let mut there = issues();
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
        let mut here = issues();
        let mut there = issues();
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
        let mut store = issues();
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
        let mut here = issues();
        let mut there = issues();
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

        let mut here = issues();
        let mut there = issues();
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
                        claimed: None,
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
            ..Stamp::default()
        }
    }

    /// Two devices that made a note and its body as one action, then edited its text and counter
    /// at once: `i2` here, `i3` there, each with the other's change.
    fn edited_on_two_devices() -> (Store, Store) {
        use serde_json::json;
        use tilo_resource_store::wire::{Command, Edit, write};

        let mut here = issues();
        let mut there = issues();
        // One action, one request: the note and its body.
        let made = write(
            &mut here,
            Command::Batch(vec![
                Command::Put {
                    kind: ISSUE.to_owned(),
                    id: "a".to_owned(),
                    resource: resource("board", "a0", "Draft", "open"),
                    stamp: stamp("make", "i1"),
                },
                Command::Edit {
                    kind: ISSUE.to_owned(),
                    id: "a".to_owned(),
                    edits: vec![
                        Edit::Splice {
                            field: "body".to_owned(),
                            at: 0,
                            delete: 0,
                            insert: "Hello".to_owned(),
                        },
                        Edit::Add {
                            field: "votes".to_owned(),
                            by: 1,
                        },
                        Edit::Set {
                            field: "meta".to_owned(),
                            value: json!({"tags": ["a", "b"]}),
                        },
                    ],
                    stamp: stamp("make", "i1"),
                },
            ]),
        )
        .unwrap();
        send(&made, &mut there);
        // Two devices edit the same text and counter at once: both edits are kept.
        let edit = |store: &mut Store, insert: &str, at: usize, invocation: &str| {
            write(
                store,
                Command::Edit {
                    kind: ISSUE.to_owned(),
                    id: "a".to_owned(),
                    edits: vec![
                        Edit::Splice {
                            field: "body".to_owned(),
                            at,
                            delete: 0,
                            insert: insert.to_owned(),
                        },
                        Edit::Add {
                            field: "votes".to_owned(),
                            by: 1,
                        },
                    ],
                    stamp: stamp("edit", invocation),
                },
            )
            .unwrap()
        };
        let mine = edit(&mut here, "Oh, ", 0, "i2");
        let theirs = edit(&mut there, " world", 5, "i3");
        send(&theirs, &mut here);
        send(&mine, &mut there);
        (here, there)
    }

    /// The content of the issue `a`, as everyone may read it; `None` for any other answer.
    fn content_of(store: &Store) -> Option<serde_json::Value> {
        use tilo_resource_store::wire::{Command, Reply, read};
        match read(
            store,
            &Command::Content {
                kind: ISSUE.to_owned(),
                id: "a".to_owned(),
            },
            &|_| true,
        ) {
            Some(Reply::Content(content)) => content,
            _ => None,
        }
    }

    #[test]
    fn the_contents_of_several_resources_read_as_one_are_each_as_read_alone() {
        use tilo_resource_store::wire::{Command, Reply, read};
        let (here, _) = edited_on_two_devices();
        let contents = |may_read: &dyn Fn(&str) -> bool| match read(
            &here,
            &Command::Contents {
                kind: ISSUE.to_owned(),
                ids: vec![
                    "a".to_owned(),
                    "nothing by this id".to_owned(),
                    "a".to_owned(),
                ],
            },
            may_read,
        ) {
            Some(Reply::Contents(contents)) => contents,
            other => panic!("contents are read: {other:?}"),
        };
        let alone = content_of(&here);
        assert!(alone.is_some(), "the issue holds content");
        assert_eq!(contents(&|_| true), [alone.clone(), None, alone]);
        assert_eq!(
            contents(&|_| false),
            [None, None, None],
            "content is read only by whom the resource's readers admit"
        );
    }

    #[test]
    fn a_page_of_some_fields_holds_those_of_each_resource_the_whole_page_holds() {
        use tilo_resource_store::Query;
        use tilo_resource_store::wire::{Command, Reply, read};
        let (here, _) = edited_on_two_devices();
        let page = |command: &Command| match read(&here, command, &|_| true) {
            Some(Reply::Page(page)) => page,
            other => panic!("a page is read: {other:?}"),
        };
        let query = Query::of(ISSUE, 10);
        let whole = page(&Command::Query(query.clone()));
        let name = whole
            .resources
            .first()
            .and_then(|(_, resource)| resource.fields.keys().next().cloned())
            .expect("an issue holds an indexed field");
        let some = page(&Command::Fields {
            query,
            fields: vec![name.clone(), "no field by this name".to_owned()],
        });
        assert_eq!(
            some.ids().collect::<Vec<_>>(),
            whole.ids().collect::<Vec<_>>()
        );
        for ((_, part), (_, all)) in some.resources.iter().zip(&whole.resources) {
            assert_eq!(part.fields.keys().collect::<Vec<_>>(), [&name]);
            assert_eq!(part.fields.get(&name), all.fields.get(&name));
            assert_eq!(part.parent, all.parent);
        }
    }

    #[test]
    fn an_apps_commands_write_content_as_one_and_read_back_its_versions() {
        use serde_json::json;
        use tilo_resource_store::wire::{Command, Reply, read};
        use tilo_resource_store::{Version, VersionKind};

        let all = |_: &str| true;
        let (here, there) = edited_on_two_devices();
        let content = content_of;
        assert_eq!(
            content(&here),
            Some(json!({"body": "Oh, Hello world", "votes": 3, "meta": {"tags": ["a", "b"]}}))
        );
        assert_eq!(content(&here), content(&there));
        assert!(
            matches!(
                read(
                    &here,
                    &Command::Content {
                        kind: ISSUE.to_owned(),
                        id: "a".to_owned()
                    },
                    &|_| false
                ),
                Some(Reply::Content(None))
            ),
            "content is read only by whom the resource's readers admit"
        );

        let versions = match read(
            &here,
            &Command::Versions {
                kind: ISSUE.to_owned(),
                id: "a".to_owned(),
            },
            &all,
        ) {
            Some(Reply::Versions(versions)) => versions,
            other => panic!("versions are read: {other:?}"),
        };
        let invocations: Vec<(&str, VersionKind)> = versions
            .iter()
            .map(|version: &Version| {
                (
                    version
                        .stamp
                        .as_ref()
                        .map_or("", |stamp| stamp.invocation.as_str()),
                    version.kind,
                )
            })
            .collect();
        assert_eq!(invocations.first(), Some(&("i1", VersionKind::Made)));
        assert!(
            invocations
                .iter()
                .any(|(invocation, _)| *invocation == "i2")
        );
        assert!(
            invocations
                .iter()
                .any(|(invocation, _)| *invocation == "i3")
        );
    }

    /// A step of a resource's history reads as it stood just before the step and just after:
    /// another device's edit made at the same time is in neither.
    #[test]
    fn a_resource_reads_as_it_stood_before_and_after_a_change() {
        use serde_json::json;
        use tilo_resource_store::wire::{Command, Reply, read};

        let all = |_: &str| true;
        let (here, _) = edited_on_two_devices();
        let Some(Reply::Versions(versions)) = read(
            &here,
            &Command::Versions {
                kind: ISSUE.to_owned(),
                id: "a".to_owned(),
            },
            &all,
        ) else {
            panic!("versions are read");
        };
        let theirs = versions
            .iter()
            .find(|version| {
                version.content
                    && version
                        .stamp
                        .as_ref()
                        .is_some_and(|stamp| stamp.invocation == "i3")
            })
            .expect("the other device's edit is a version");
        let at = |after: bool, may_read: &dyn Fn(&str) -> bool| match read(
            &here,
            &Command::At {
                kind: ISSUE.to_owned(),
                id: "a".to_owned(),
                change: theirs.change.clone(),
                after,
            },
            may_read,
        ) {
            Some(Reply::At(at)) => at,
            other => panic!("the resource is read at the change: {other:?}"),
        };
        let body = |after: bool| {
            at(after, &all)
                .and_then(|(_, content)| content)
                .and_then(|content| content.get("body").cloned())
        };
        assert_eq!(body(false), Some(json!("Hello")));
        assert_eq!(body(true), Some(json!("Hello world")));
        assert!(
            at(true, &|_| false).is_none(),
            "only whom the resource's readers admit reads it at a change"
        );
    }

    #[test]
    fn an_apps_undo_takes_back_only_its_own_characters_and_redo_puts_them_back() {
        use serde_json::json;
        use tilo_resource_store::wire::{Command, write};

        let (mut here, _) = edited_on_two_devices();
        let content = content_of;
        // Redo is the undo of an undo.
        let undo = Stamp {
            undoes: Some("i2".to_owned()),
            ..stamp("edit", "i4")
        };
        write(
            &mut here,
            Command::Undo {
                invocation: "i2".to_owned(),
                stamp: undo,
            },
        )
        .unwrap();
        assert_eq!(content(&here).unwrap()["body"], json!("Hello world"));
        write(
            &mut here,
            Command::Undo {
                invocation: "i4".to_owned(),
                stamp: stamp("edit", "i5"),
            },
        )
        .unwrap();
        assert_eq!(content(&here).unwrap()["body"], json!("Oh, Hello world"));
        assert!(
            write(
                &mut here,
                Command::Count {
                    kind: ISSUE.to_owned(),
                    filters: Vec::new()
                }
            )
            .is_err(),
            "a read is no write"
        );
    }

    #[test]
    fn undo_takes_back_one_invocation_in_every_document_and_nothing_after_it() {
        use automerge::ReadDoc as _;
        use automerge::transaction::Transactable as _;

        let all = |_: &str| true;
        let mut store = issues();
        store
            .put(
                ISSUE,
                "a",
                resource("board", "a0", "Draft", "open"),
                &stamp("make", "i1"),
            )
            .unwrap();
        let ((), _) = store
            .edit_content(ISSUE, "a", &stamp("make", "i1"), |tx, root| {
                tx.put(root, "note", "first")
            })
            .unwrap();
        // One invocation: a field of `a`, a new resource `b`, and `a`'s content.
        let renamed = stamp("rename", "i2");
        store
            .put(
                ISSUE,
                "a",
                resource("board", "a0", "Final", "open"),
                &renamed,
            )
            .unwrap();
        store
            .put(
                ISSUE,
                "b",
                resource("board", "a1", "Split off", "open"),
                &renamed,
            )
            .unwrap();
        let ((), _) = store
            .edit_content(ISSUE, "a", &renamed, |tx, root| {
                tx.put(root, "note", "second")
            })
            .unwrap();
        // A later invocation changes another field of `a`.
        store
            .put(
                ISSUE,
                "a",
                resource("board", "a0", "Final", "done"),
                &stamp("close", "i3"),
            )
            .unwrap();

        let undo = Stamp {
            undoes: Some("i2".to_owned()),
            ..stamp("rename", "i4")
        };
        let written = store.undo("i2", &undo).unwrap();
        // One change in each document the invocation changed: the board's and `a`'s content.
        let mut documents: Vec<&str> = written.iter().map(|write| write.doc.as_str()).collect();
        documents.sort_unstable();
        documents.dedup();
        assert_eq!(documents.len(), 2, "{documents:?}");
        let a = store.get(ISSUE, "a", &all).unwrap();
        assert_eq!(a.fields["title"], Cell::One("Draft".into()));
        assert_eq!(
            a.fields["state"],
            Cell::One("done".into()),
            "the later change stays"
        );
        assert!(store.get(ISSUE, "b", &all).is_none());
        let (doc, root) = store.content(ISSUE, "a").unwrap().unwrap();
        let note = doc.get(&root, "note").unwrap().unwrap().0.to_string();
        assert_eq!(note, "\"first\"");
        // Each is stamped as the undo, and says which of the invocation's changes it took back.
        for write in &written {
            let stamp = Stamp::of(&write.change).expect("the undo is stamped");
            let context = stamp
                .context
                .clone()
                .expect("the undo says what it took back");
            assert_eq!(
                Stamp {
                    context: None,
                    ..stamp
                },
                undo
            );
            let Some(tilo_actions::Did::History(step)) = &context.what else {
                panic!("the undo is a step through history: {context:?}");
            };
            assert_eq!(step.op, tilo_actions::HistoryOp::Undo);
            assert!(!step.takes_back.is_empty(), "{step:?}");
            for hash in &step.takes_back {
                let taken = store
                    .document(&write.doc)
                    .unwrap()
                    .get_change_by_hash(&hash.parse().unwrap())
                    .and_then(|change| Stamp::of(&change))
                    .expect("what it took back is a change of its document");
                assert_eq!(taken.invocation, "i2", "only the rename's changes");
            }
        }

        // Undoing the undo takes the rename back again.
        store.undo("i4", &stamp("rename", "i5")).unwrap();
        assert_eq!(
            store.get(ISSUE, "a", &all).unwrap().fields["title"],
            Cell::One("Final".into())
        );
        assert!(store.get(ISSUE, "b", &all).is_some());
    }

    #[test]
    fn a_refused_change_has_no_effect_and_neither_has_one_built_on_it() {
        use tilo_resource_store::Refusal;
        let mut here = issues();
        let mut there = issues();
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

    /// A list's verbs write the records its type nests, and those records' fields are the ones
    /// their own app declares: a tilolang package publishes a surface per type, so an item and a
    /// comment of it are kinds another surface declares. Made, moved, commented and removed, the
    /// change reaches the store.
    #[test]
    fn an_item_verb_writes_the_items_another_surface_declares() {
        let mut there = issues();
        for surface in [
            super::tilolang_list_surface(),
            super::tilolang_todo_surface(),
            super::tilolang_comment_surface(),
        ] {
            there.declare(Schema::from_surface(&surface));
        }
        let anyone = |_: &Proposal<'_>| true;
        let item = |order: &str| Resource {
            readers: MEMBERS.to_owned(),
            parent: Some("l".to_owned()),
            order: Some(order.to_owned()),
            fields: BTreeMap::from([
                ("title".to_owned(), Cell::One("milk".into())),
                ("done".to_owned(), Cell::One(true.into())),
                ("$in".to_owned(), Cell::One("items".into())),
            ]),
        };
        let made = issues()
            .put("tl.so.tilo.todo.Todo", "t1", item("a"), &stamp("add", "i1"))
            .unwrap();
        assert_eq!(deliver(&made, &mut there, &anyone), []);
        let moved = issues()
            .put(
                "tl.so.tilo.todo.Todo",
                "t1",
                item("b"),
                &stamp("move", "i2"),
            )
            .unwrap();
        assert_eq!(deliver(&moved, &mut there, &anyone), []);
        // A comment of the todo: a kind the list's type nests two records down.
        let comment = Resource {
            readers: MEMBERS.to_owned(),
            parent: Some("t1".to_owned()),
            order: Some("a".to_owned()),
            fields: BTreeMap::from([("text".to_owned(), Cell::One("soon".into()))]),
        };
        let noted = issues()
            .put(
                "tl.so.tilo.todo.Comment",
                "c1",
                comment,
                &stamp("mention", "i3"),
            )
            .unwrap();
        assert_eq!(deliver(&noted, &mut there, &anyone), []);
        // The list's own verb takes the item off it.
        there
            .remove("tl.so.tilo.todo.Todo", "t1", &stamp("remove", "i4"))
            .unwrap();
        let all = |_: &str| true;
        assert!(there.get("tl.so.tilo.todo.Todo", "t1", &all).is_none());
    }

    /// A tilolang action writes the records of its own package that it reaches by reference (a
    /// todo's toggle renames the list it stands in), never another package's.
    #[test]
    fn a_tilolang_verb_writes_its_own_packages_records_and_no_others() {
        use tilo_resource_store::Refusal;
        let mut there = issues();
        for surface in [
            super::tilolang_list_surface(),
            super::tilolang_todo_surface(),
            super::tilolang_item_surface(),
        ] {
            there.declare(Schema::from_surface(&surface));
        }
        let anyone = |_: &Proposal<'_>| true;
        let named = |field: &str| Resource {
            readers: MEMBERS.to_owned(),
            parent: None,
            order: None,
            fields: BTreeMap::from([(field.to_owned(), Cell::One("Errands".into()))]),
        };
        let list = issues()
            .put(
                "tl.so.tilo.todo.TodoList",
                "l",
                named("name"),
                &stamp("toggle", "i1"),
            )
            .unwrap();
        assert_eq!(deliver(&list, &mut there, &anyone), []);
        let item = issues()
            .put(
                "tl.so.tilo.lang.gallery.Item",
                "g1",
                named("title"),
                &stamp("toggle", "i2"),
            )
            .unwrap();
        assert!(matches!(
            deliver(&item, &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("gallery.Item")
        ));
    }

    /// What an item stands in is the platform's field on the item, which no declared type has:
    /// the store admits it beside the fields the type does declare.
    #[test]
    fn a_change_writes_the_platform_field_no_type_declares() {
        let mut there = issues();
        there.declare(Schema::from_surface(&super::tilolang_surface()));
        there.declare(Schema::from_surface(&super::tilolang_item_surface()));
        let anyone = |_: &Proposal<'_>| true;
        let item = Resource {
            readers: MEMBERS.to_owned(),
            parent: Some("g".to_owned()),
            order: Some("a".to_owned()),
            fields: BTreeMap::from([
                ("$in".to_owned(), Cell::One("backlog_items".into())),
                ("title".to_owned(), Cell::One("Dawn".into())),
            ]),
        };
        let made = issues()
            .put(
                "tl.so.tilo.lang.gallery.Item",
                "i",
                item,
                &stamp("$create", "i2"),
            )
            .unwrap();
        assert_eq!(deliver(&made, &mut there, &anyone), []);
        let all = |_: &str| true;
        assert!(
            there
                .get("tl.so.tilo.lang.gallery.Item", "i", &all)
                .is_some()
        );
    }

    /// A package's root is made by the host as a change of its own, against the package's
    /// surface: the store admits it, since the surface declares the record and that `$create`
    /// writes it. A surface read without its own record refused every app's first change.
    #[test]
    fn a_package_creates_its_root_under_its_surface() {
        let mut there = issues();
        there.declare(Schema::from_surface(&super::tilolang_surface()));
        let anyone = |_: &Proposal<'_>| true;
        let mut fields = BTreeMap::new();
        fields.insert("name".to_owned(), Cell::One("Gallery".into()));
        let root = Resource {
            readers: MEMBERS.to_owned(),
            parent: None,
            order: None,
            fields,
        };
        let made = issues()
            .put(
                "tl.so.tilo.lang.gallery.Gallery",
                "g",
                root,
                &stamp("$create", "i1"),
            )
            .unwrap();
        assert_eq!(deliver(&made, &mut there, &anyone), []);
        let all = |_: &str| true;
        let held = there
            .get("tl.so.tilo.lang.gallery.Gallery", "g", &all)
            .expect("the root stands");
        assert_eq!(held.scalar("name"), Value::from("Gallery"));
    }

    /// A store that declares no type admits no change it receives: a writer whose surface was
    /// never named there is refused, not taken on trust.
    #[test]
    fn a_store_that_declares_no_type_admits_no_change() {
        use tilo_resource_store::Refusal;
        let mut there = Store::new();
        let anyone = |_: &Proposal<'_>| true;
        let made = Store::new()
            .put(
                "tl.so.tilo.lang.gallery.Gallery",
                "g",
                Resource {
                    readers: MEMBERS.to_owned(),
                    parent: None,
                    order: None,
                    fields: BTreeMap::from([("name".to_owned(), Cell::One("Gallery".into()))]),
                },
                &stamp("$create", "i1"),
            )
            .unwrap();
        let refused = deliver(&made, &mut there, &anyone);
        assert!(
            matches!(refused.as_slice(), [Refusal::Undeclared(_)]),
            "{refused:?}"
        );
        let all = |_: &str| true;
        assert!(
            there
                .get("tl.so.tilo.lang.gallery.Gallery", "g", &all)
                .is_none()
        );
    }

    /// A record a field holds is nested in the record holding it, as a collection's items are:
    /// the store admits it under its holder, and the holder's type says it nests that kind.
    #[test]
    fn a_record_a_field_holds_is_admitted_under_its_holder() {
        let holding = Schema::from_surface(&super::tilolang_holding_surface());
        assert_eq!(
            holding.get("tl.so.tilo.clocks.Clocks").unwrap().nested,
            [(
                "stopwatch".to_owned(),
                "tl.so.tilo.clocks.Stopwatch".to_owned()
            )],
            "the holder's type nests the held kind"
        );
        let mut there = issues();
        there.declare(holding);
        there.declare(Schema::from_surface(&super::tilolang_held_surface()));
        let anyone = |_: &Proposal<'_>| true;
        let record = |parent: Option<&str>, field: &str, text: &str| Resource {
            readers: MEMBERS.to_owned(),
            parent: parent.map(str::to_owned),
            order: None,
            fields: BTreeMap::from([(field.to_owned(), Cell::One(text.into()))]),
        };
        let mut made = issues();
        let mut changes = made
            .put(
                "tl.so.tilo.clocks.Clocks",
                "c",
                record(None, "name", "Clocks"),
                &stamp("$create", "i1"),
            )
            .unwrap();
        changes.extend(
            made.put(
                "tl.so.tilo.clocks.Stopwatch",
                "s",
                record(Some("c"), "label", "Tea"),
                &stamp("$create", "i2"),
            )
            .unwrap(),
        );
        assert_eq!(deliver(&changes, &mut there, &anyone), []);
        let all = |_: &str| true;
        let held = there
            .get("tl.so.tilo.clocks.Stopwatch", "s", &all)
            .expect("the held record stands");
        assert_eq!(held.scalar("label"), Value::from("Tea"));
    }

    #[test]
    fn a_change_writes_only_what_the_verb_it_names_declares() {
        use tilo_resource_store::Refusal;
        let mut there = issues();
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
            issues().put("Comment", id, resource, &stamp).unwrap()
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

    /// A checkpoint, a description and a link are verbs every schema has: a change that writes
    /// nothing, by someone who may write the document, saying little. One that writes, says too
    /// much, comes from someone who may not write, or names a verb nobody declared is refused.
    #[test]
    fn a_history_verb_writes_nothing_and_says_little() {
        use tilo_resource_store::{MAX_MARK_ARGS, Refusal};
        let tracker = || {
            let mut store = issues();
            store.declare(Schema::from_surface(&super::tracker_surface()));
            store
        };
        let mut here = tracker();
        let mut there = tracker();
        let anyone = |_: &Proposal<'_>| true;
        let mut fields = BTreeMap::new();
        fields.insert("text".to_owned(), Cell::One("Looks right".into()));
        let comment = Resource {
            readers: MEMBERS.to_owned(),
            parent: None,
            order: None,
            fields,
        };
        let made = here
            .put("Comment", "c1", comment.clone(), &stamp("comment", "c1"))
            .unwrap();
        assert_eq!(deliver(&made, &mut there, &anyone), []);
        let marked = |verb: &str, args: Vec<serde_json::Value>| Stamp {
            args,
            ..stamp(verb, &format!("{verb}-mark"))
        };

        // A checkpoint names the version it stands at, and every holder takes it.
        let checkpoint = here
            .mark(
                "Comment",
                "c1",
                &marked("checkpoint", vec!["Reviewed".into()]),
            )
            .unwrap();
        assert_eq!(checkpoint.change.len(), 0, "a checkpoint writes nothing");
        assert_eq!(deliver(&[checkpoint.clone()], &mut there, &anyone), []);
        for verb in ["describe", "link"] {
            let mark = here
                .mark("Comment", "c1", &marked(verb, vec!["said".into()]))
                .unwrap();
            assert_eq!(
                deliver(&[mark], &mut there, &anyone),
                [],
                "{verb} is declared"
            );
        }

        // Someone who may only read the document is refused, as for any write of it.
        let reader_only = |_: &Proposal<'_>| false;
        let mut guarded = tracker();
        assert_eq!(deliver(&made, &mut guarded, &anyone), []);
        assert!(matches!(
            deliver(&[checkpoint], &mut guarded, &reader_only).as_slice(),
            [Refusal::NotPermitted]
        ));

        // A history verb that writes is refused, whatever it writes.
        let writes = tracker()
            .put(
                "Comment",
                "c2",
                comment.clone(),
                &marked("checkpoint", vec!["x".into()]),
            )
            .unwrap();
        assert!(matches!(
            deliver(&writes, &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("writes nothing")
        ));

        // One that says more than a history verb may is refused.
        let long = "x".repeat(MAX_MARK_ARGS);
        // Each from a device of its own that holds the comment, so none is built on another's
        // refusal.
        let holding = || {
            let mut store = tracker();
            assert_eq!(deliver(&made, &mut store, &anyone), []);
            store
        };
        let oversized = holding()
            .mark("Comment", "c1", &marked("describe", vec![long.into()]))
            .unwrap();
        assert!(matches!(
            deliver(&[oversized], &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("at most")
        ));

        // A verb nobody declared stays refused, even writing nothing.
        let nobodys = holding()
            .mark("Comment", "c1", &marked("bless", Vec::new()))
            .unwrap();
        assert!(matches!(
            deliver(&[nobodys], &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("no verb `bless`")
        ));
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
                live: None,
            }],
            ..Schema::default()
        };
        let mut here = issues();
        let mut there = issues();
        there.declare(schema);
        let anyone = |_: &Proposal<'_>| true;
        let made = put(&mut here, "a", resource("board", "a0", "First", "open"));
        assert!(deliver(&made, &mut there, &anyone).is_empty());
        // Each written by another device, each into a document of its own, so neither is built
        // on the other's refusal.
        let lost = put(
            &mut issues(),
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
        let secret = put(&mut issues(), "c", secret);
        assert!(matches!(
            deliver(&secret, &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("secret")
        ));
        assert_eq!(there.count(ISSUE, &[], &|_| true), 1);
    }

    /// A sealed write is decided, by members without its key, by the kinds of the records it
    /// claims to write. Every holder that opens it reads those kinds itself, at the change's own
    /// earlier changes, so all of them read the same: two holders refuse the same lying edit
    /// alike and keep the record exactly as it was, and a third takes it claimed truly. The kind
    /// of an edit after the record's first write comes from the record, never from the change.
    #[test]
    fn a_change_claiming_other_kinds_than_it_writes_is_refused_alike_by_every_holder() {
        use tilo_resource_store::Refusal;
        let ty = |name: &str| record_type(name);
        let schema = Schema {
            types: vec![ty(ISSUE), ty("Comment")],
            ..Schema::default()
        };
        let mut here = Store::new();
        let mut holders = [Store::new(), Store::new(), Store::new()];
        let made = put(&mut here, "a", resource("board", "a0", "Draft", "open"));
        for holder in &mut holders {
            holder.declare(schema.clone());
            send(&made, holder);
        }
        let edited = put(&mut here, "a", resource("board", "a0", "Final", "done"));
        let [edit] = edited.as_slice() else {
            panic!("one change of one document: {edited:?}");
        };
        let truly = here.written_kinds(edit);
        assert_eq!(truly, BTreeSet::from([ISSUE.to_owned()]));
        let claiming = |kinds: &BTreeSet<String>| {
            vec![Received {
                change: edit.change.clone(),
                author: [7; 32],
                claimed: Some(kinds.clone()),
            }]
        };
        let lie = BTreeSet::from(["Comment".to_owned()]);
        let all = |_: &str| true;
        let [first, second, third] = &mut holders;
        for holder in [first, second] {
            let before = (
                holder.document(&edit.doc).expect("held").save(),
                holder.get(ISSUE, "a", &all).cloned(),
            );
            let refused = holder
                .receive(
                    &edit.doc,
                    &edit.readers,
                    claiming(&lie),
                    &|_: &Proposal<'_>| true,
                )
                .unwrap();
            assert!(
                matches!(refused.as_slice(), [(_, Refusal::Misclaimed(why))] if why.contains(ISSUE) && why.contains("Comment")),
                "{refused:?}"
            );
            let after = (
                holder.document(&edit.doc).expect("held").save(),
                holder.get(ISSUE, "a", &all).cloned(),
            );
            assert_eq!(
                after, before,
                "a refused change leaves the record as it was"
            );
        }
        let refused = third
            .receive(
                &edit.doc,
                &edit.readers,
                claiming(&truly),
                &|_: &Proposal<'_>| true,
            )
            .unwrap();
        assert_eq!(refused, []);
        assert_eq!(
            third
                .get(ISSUE, "a", &all)
                .map(|held| held.fields.get("title").cloned()),
            Some(Some(Cell::One("Final".into())))
        );
    }

    /// A change of a record's kind takes it out of its old kind and into the new one: it claims
    /// both, and the verb it names must write both. One that writes only the new kind is refused,
    /// and so is a claim of only the new kind; one that writes both is taken.
    #[test]
    fn a_change_of_a_records_kind_is_refused_where_its_old_kind_does_not_allow_it() {
        use tilo_resource_store::{Refusal, Verb, Writes};
        let ty = |name: &str| record_type(name);
        let verb = |name: &str, kinds: &[&str]| Verb {
            name: name.to_owned(),
            writes: kinds
                .iter()
                .map(|kind| ((*kind).to_owned(), Writes::default()))
                .collect(),
            nested: BTreeSet::new(),
            marks: false,
        };
        let schema = Schema {
            types: vec![ty(ISSUE), ty("Comment")],
            verbs: vec![
                verb("reply", &["Comment"]),
                verb("convert", &[ISSUE, "Comment"]),
            ],
            readers: BTreeMap::new(),
        };
        let mut here = Store::new();
        let mut holders = [Store::new(), Store::new(), Store::new()];
        let made = put(&mut here, "a", resource("board", "a0", "Draft", "open"));
        for holder in &mut holders {
            // The record's first write names no verb: it is taken by the types alone, and the
            // verbs judge what comes after it.
            holder.declare(Schema {
                types: schema.types.clone(),
                ..Schema::default()
            });
            send(&made, holder);
            holder.declare(schema.clone());
        }
        let retyped = |verb: &str| {
            let mut doc = here.document(&made[0].doc).expect("held").clone();
            doc.set_actor(automerge::ActorId::from([9_u8; 16].as_slice()));
            let mut tx = doc.transaction();
            let (_, record) = automerge::ReadDoc::get(&tx, automerge::ROOT, "r:a")
                .unwrap()
                .expect("the record");
            automerge::transaction::Transactable::put(&mut tx, &record, "k", "Comment").unwrap();
            tx.commit_with(
                automerge::transaction::CommitOptions::default()
                    .with_message(stamp(verb, verb).message()),
            );
            Written {
                doc: made[0].doc.clone(),
                readers: made[0].readers.clone(),
                change: doc.get_last_local_change().expect("the change"),
            }
        };
        let deliver_claiming = |holder: &mut Store, write: &Written, kinds: &BTreeSet<String>| {
            holder
                .receive(
                    &write.doc,
                    &write.readers,
                    vec![Received {
                        change: write.change.clone(),
                        author: [7; 32],
                        claimed: Some(kinds.clone()),
                    }],
                    &|_: &Proposal<'_>| true,
                )
                .unwrap()
        };
        let both = BTreeSet::from(["Comment".to_owned(), ISSUE.to_owned()]);
        let (reply, convert) = (retyped("reply"), retyped("convert"));
        assert_eq!(here.written_kinds(&reply), both);
        let [first, second, third] = &mut holders;
        let only_new = BTreeSet::from(["Comment".to_owned()]);
        let refused = deliver_claiming(first, &convert, &only_new);
        assert!(
            matches!(refused.as_slice(), [(_, Refusal::Misclaimed(_))]),
            "{refused:?}"
        );
        let refused = deliver_claiming(second, &reply, &both);
        assert!(
            matches!(refused.as_slice(), [(_, Refusal::Undeclared(why))] if why.contains("writes no `Issue`")),
            "{refused:?}"
        );
        let all = |_: &str| true;
        for holder in [&*first, &*second] {
            assert!(holder.get(ISSUE, "a", &all).is_some(), "still an issue");
        }
        assert_eq!(deliver_claiming(third, &convert, &both), []);
        assert!(third.get("Comment", "a", &all).is_some(), "now a comment");
    }

    #[test]
    fn two_devices_starting_one_document_at_once_both_keep_what_they_wrote() {
        let mut here = issues();
        let mut there = issues();
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
        let mut here = issues();
        let mut there = issues();
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
        let mut here = issues();
        let mut there = issues();
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
        let mut store = issues();
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
        let mut store = issues();
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
        let mut started = issues();
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
        let mut stale = issues();
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
    fn forgetting_a_document_hides_its_rows_until_loaded_again() {
        let (mut store, doc, bytes) = kept();
        let issues = store.watch(ISSUE);
        let _notes = store.watch("Note");

        store.forget_document(&doc);
        assert!(store.document(&doc).is_none());
        assert_eq!(store.count(ISSUE, &[], &anyone), 0);
        assert_eq!(store.changed(), BTreeSet::from([issues]));

        store.load(&doc, &bytes).unwrap();
        assert_eq!(store.count(ISSUE, &[], &anyone), 3);
        assert_eq!(store.changed(), BTreeSet::from([issues]));
    }

    #[test]
    fn a_watch_names_the_commits_of_its_kind() {
        let mut store = issues();
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

    /// A record whose type names its readers is placed by its own rule wherever it is nested: a
    /// report filed in a channel every member reads goes to the document of its reporter and the
    /// workspace's moderators, named by the role and never by who holds it, and a record nested
    /// in the report without a rule of its own goes where the report went.
    #[test]
    fn a_nested_record_is_placed_by_its_own_rule_and_a_role_is_named_not_its_holders() {
        use tilo_actions::policy::Role;
        use tilo_actions::surface::ReaderRule;

        const CHANNEL: &str = "Channel";
        const REPORT: &str = "Report";
        const NOTE: &str = "Note";
        let mut store = Store::new();
        store.declare(Schema {
            readers: BTreeMap::from([(
                REPORT.to_owned(),
                ReaderRule::AuthorAnd {
                    role: Role::Moderator,
                },
            )]),
            ..Schema::default()
        });
        let ben = Stamp {
            invoker: "ben".to_owned(),
            ..Stamp::default()
        };
        let put = |store: &mut Store, kind: &str, id: &str, resource: Resource| {
            tilo_resource_store::wire::write(
                store,
                tilo_resource_store::wire::Command::Put {
                    kind: kind.to_owned(),
                    id: id.to_owned(),
                    resource,
                    stamp: ben.clone(),
                },
            )
        };
        let placed = |written: &[Written]| -> BTreeSet<String> {
            written.iter().map(|write| write.readers.clone()).collect()
        };
        let channel = put(
            &mut store,
            CHANNEL,
            "c1",
            Resource {
                readers: MEMBERS.to_owned(),
                parent: None,
                order: None,
                fields: fields("design", "open"),
            },
        )
        .unwrap();
        assert_eq!(placed(&channel), BTreeSet::from([MEMBERS.to_owned()]));

        let report = put(
            &mut store,
            REPORT,
            "r1",
            Resource {
                readers: String::new(),
                parent: Some("c1".to_owned()),
                order: Some("a".to_owned()),
                fields: fields("spam", "open"),
            },
        )
        .unwrap();
        assert_eq!(
            placed(&report),
            BTreeSet::from(["@role:moderator,ben".to_owned()]),
            "the report is sealed to its reporter and the moderators, not to the channel's readers"
        );

        let note = put(
            &mut store,
            NOTE,
            "n1",
            Resource {
                readers: String::new(),
                parent: Some("r1".to_owned()),
                order: Some("a".to_owned()),
                fields: fields("seen it", "open"),
            },
        )
        .unwrap();
        assert_eq!(
            placed(&note),
            BTreeSet::from(["@role:moderator,ben".to_owned()]),
            "what a report holds is read by whoever reads the report"
        );

        let named = put(
            &mut store,
            REPORT,
            "r2",
            Resource {
                readers: MEMBERS.to_owned(),
                parent: Some("c1".to_owned()),
                order: Some("b".to_owned()),
                fields: fields("named", "open"),
            },
        );
        assert!(
            named.is_err(),
            "a write names no readers for a kind whose rule names them"
        );
    }
}

/// An app's records, as a window that runs with no workspace keeps and reads them: through the
/// record commands a workspace answers.
mod records {
    use automerge::transaction::Transactable as _;
    use tilo_resource_store::wire::{Bytes, Command, RECORD_PAGE_BYTES, Reply};
    use tilo_resource_store::{LocalPort, Port};

    /// The record `id`'s changes past `after`, joined from every page, and its heads.
    fn read(port: &LocalPort, id: &str, after: &[automerge::ChangeHash]) -> (Vec<u8>, Vec<String>) {
        let after: Vec<String> = after.iter().map(ToString::to_string).collect();
        let (mut joined, mut from) = (Vec::new(), 0);
        loop {
            let reply = port
                .command(Command::RecordChanges {
                    record: Some(id.to_owned()),
                    after: after.clone(),
                    from,
                })
                .unwrap();
            let Reply::RecordChanges {
                changes,
                heads,
                next,
                pending: false,
            } = reply
            else {
                panic!("a page of the record: {reply:?}");
            };
            joined.extend(changes.0);
            match next {
                Some(next) => from = next,
                None => return (joined, heads),
            }
        }
    }

    /// `changes` of the record `id`, in runs.
    fn write(port: &LocalPort, id: &str, changes: &[u8]) {
        let runs: Vec<&[u8]> = changes.chunks(RECORD_PAGE_BYTES).collect();
        for (index, run) in runs.iter().enumerate() {
            let reply = port
                .command(Command::RecordChange {
                    record: Some(id.to_owned()),
                    changes: Bytes(run.to_vec()),
                    more: index < runs.len().saturating_sub(1),
                    commit: None,
                })
                .unwrap();
            assert!(matches!(reply, Reply::Written), "{reply:?}");
        }
    }

    /// A record's first write founds it, larger than a page; it reads back whole in pages, then
    /// only what changed past the heads a reader holds; the app's listing names it; and a record
    /// command naming no record has no route here to fall back on.
    #[test]
    fn a_local_port_keeps_an_app_s_records() {
        let port = LocalPort::in_memory().named("so.tilo.lang.tasks");
        let mut doc = automerge::AutoCommit::new();
        let large: String = (0..RECORD_PAGE_BYTES + 1000)
            .map(|at| char::from(b'a' + u8::try_from(at % 26).unwrap()))
            .collect();
        doc.put(automerge::ROOT, "title", large).unwrap();
        doc.commit();
        write(&port, "doc-1", &doc.save_after(&[]));

        let (whole, heads) = read(&port, "doc-1", &[]);
        let mut copy = automerge::AutoCommit::load(&whole).unwrap();
        assert_eq!(copy.get_heads(), doc.get_heads(), "the record reads whole");
        assert_eq!(heads, vec![doc.get_heads()[0].to_string()]);

        let held = doc.get_heads();
        doc.put(automerge::ROOT, "done", true).unwrap();
        doc.commit();
        write(&port, "doc-1", &doc.save_after(&held));
        let (changes, heads) = read(&port, "doc-1", &held);
        assert_eq!(
            changes,
            doc.save_after(&held),
            "only the change past the reader's heads"
        );
        assert_eq!(heads, vec![doc.get_heads()[0].to_string()]);
        assert_eq!(
            read(&port, "doc-2", &[]),
            (Vec::new(), Vec::new()),
            "a record it holds none of"
        );

        let listed = port.command(Command::Records).unwrap();
        assert!(
            matches!(&listed, Reply::Ids(ids) if ids == &["doc-1"]),
            "{listed:?}"
        );
        assert!(
            port.command(Command::RecordChanges {
                record: None,
                after: Vec::new(),
                from: 0,
            })
            .is_err()
        );
    }
}

/// The device's store as a window that runs with no workspace holds it: a port in this process.
mod port {
    use super::*;
    use tilo_resource_store::wire::{Command, Reply};
    use tilo_resource_store::{LocalPort, Port, Resource, Stamp};

    /// A directory no other test here reads, beside the process's own.
    fn dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tilo-resource-store-{name}-{}", std::process::id()))
    }

    #[test]
    fn a_local_port_answers_the_store_s_commands_and_keeps_them_on_disk() {
        let dir = dir("port");
        let _ = std::fs::remove_dir_all(&dir);
        let port = LocalPort::at(dir.clone()).unwrap();
        let mut fields = BTreeMap::new();
        fields.insert("title".to_owned(), Cell::One("Notes".into()));
        fields.insert("state".to_owned(), Cell::One("open".into()));
        let written = port
            .command(Command::Put {
                kind: ISSUE.to_owned(),
                id: "i1".to_owned(),
                resource: Resource {
                    readers: "everyone".to_owned(),
                    parent: Some("board".to_owned()),
                    order: Some("a1".to_owned()),
                    fields,
                },
                stamp: Stamp::default(),
            })
            .unwrap();
        assert!(matches!(written, Reply::Written), "{written:?}");
        let documents = port.documents();
        assert_eq!(documents.len(), 1, "the store placed it in one document");
        drop(port);

        // What the port kept reads again when a store opens the same directory.
        let port = LocalPort::at(dir.clone()).unwrap();
        let read = port
            .command(Command::Get {
                kind: ISSUE.to_owned(),
                id: "i1".to_owned(),
            })
            .unwrap();
        let Reply::Resource(Some(indexed)) = read else {
            panic!("the resource the port wrote reads: {read:?}");
        };
        assert_eq!(
            indexed.fields.get("title"),
            Some(&Cell::One("Notes".into()))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
