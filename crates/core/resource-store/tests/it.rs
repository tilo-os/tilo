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

use collab_types::store::{Cell, Filter, Order, PARENT, Query, Value};
use tilo_resource_store::{Change, Commit, CommitHook as _, FieldKind, Index, Schema};

const ISSUE: &str = "Issue";
/// The workspace every store a test makes keeps the resources of.
const WORKSPACE: &str = "5d2e7c1a-9b3f-4e8d-a6c4-1f0b2e3d4c5a";

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

fn query(index: &Index, query: &Query) -> collab_types::store::Page {
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
fn the_values_a_field_holds_are_each_named_once_of_the_resources_asked_for() {
    let mut index = board();
    let values = |index: &Index, filters: &[Filter]| -> Vec<Value> {
        index.values(ISSUE, filters, "labels", &anyone)
    };
    let parent = Filter::Is(PARENT.into(), "board".into());
    // Every issue is the board's: the counts the index keeps answer, each label once.
    assert_eq!(
        values(&index, std::slice::from_ref(&parent)),
        ["all", "done", "open"].map(Value::from)
    );
    assert_eq!(
        values(
            &index,
            &[parent.clone(), Filter::Is("state".into(), "open".into())]
        ),
        ["all", "open"].map(Value::from)
    );
    // Another board's issue is no label of this one's, and a label nothing holds any more is
    // gone.
    commit(
        &mut index,
        "doc-2",
        &[issue("j0", "other", "a0", "Elsewhere", "blocked", 1)],
    );
    for n in (1..10).step_by(2) {
        commit(
            &mut index,
            "doc-1",
            &[issue(
                &format!("i{n}"),
                "board",
                &format!("a{n}"),
                "Reopened",
                "open",
                n,
            )],
        );
    }
    assert_eq!(
        values(&index, std::slice::from_ref(&parent)),
        ["all", "open"].map(Value::from)
    );
    assert_eq!(
        values(&index, &[]),
        ["all", "blocked", "open"].map(Value::from)
    );
    assert_eq!(
        values(&index, &[Filter::Is(PARENT.into(), "none".into())]),
        Vec::<Value>::new()
    );
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
    use collab_types::store::Absent;
    use tilo_resource_store::Change;
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

/// The reminders due by now, asked again each second with that second as their bound, read the
/// few resources that hold a reminder: the thousands that hold none are never walked, so no
/// listing of them is kept and pushes out the listings a window shows.
#[test]
fn a_range_of_a_field_few_hold_reads_only_its_holders() {
    let mut index = Index::new();
    let changes: Vec<Change> = (0..2000)
        .map(|n| {
            let mut change = issue(
                &format!("i{n:04}"),
                "board",
                &format!("{n:04}"),
                &format!("Issue {n}"),
                "open",
                1,
            );
            if n % 100 == 0
                && let Change::Put { fields, .. } = &mut change
            {
                fields.insert(
                    "remind".to_owned(),
                    Cell::One(format!("2026-01-{:02}", n / 100 + 1).into()),
                );
            }
            change
        })
        .collect();
    commit(&mut index, "doc-1", &changes);
    for day in 1..=40 {
        let now = format!("2026-01-{day:02}");
        let due = Query::of(ISSUE, 200)
            .filter(Filter::Present("remind".into()))
            .filter(Filter::Within {
                field: "remind".into(),
                from: Bound::Unbounded,
                to: Bound::Included(now.as_str().into()),
            });
        let expected: Vec<String> = (0..2000)
            .filter(|n| n % 100 == 0 && n / 100 < day)
            .map(|n| format!("i{n:04}"))
            .collect();
        assert_eq!(ids(&index, &due), expected, "the reminders due by {now}");
        assert_eq!(count(&index, ISSUE, &due.filters), expected.len());
    }
    assert_eq!(
        index.kept(),
        0,
        "forty asks of the reminders due, each by another bound, kept {} listings of every issue",
        index.kept()
    );
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
    use collab_types::store::Absent;
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
            collab_types::store::Sort::Field(name) => field(row, name),
            collab_types::store::Sort::Position => {
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

/// A type's newer version only adds fields and containers: one that drops, renames, moves or
/// retypes what an older version declares is refused, and the store keeps the older type.
#[test]
fn a_type_version_only_adds_fields_and_containers() {
    use tilo_actions::surface::Surface;
    // `tracker_surface` at version 2, as `edit` changes it.
    let later = |edit: fn(&mut serde_json::Value)| {
        let mut surface = serde_json::to_value(tracker_surface()).expect("a surface");
        surface["version"] = 2.into();
        edit(&mut surface);
        toml_like(surface)
    };
    let refused = |store: &mut tilo_resource_store::Store, surface: &Surface| {
        store
            .declare(Schema::from_surface(surface))
            .expect_err("a version that does not only add is refused")
    };
    let mut store = tilo_resource_store::Store::new(crate::WORKSPACE);
    store
        .declare(Schema::from_surface(&tracker_surface()))
        .expect("the first version is declared");

    let dropped = later(|surface| {
        surface["types"][0]["fields"]
            .as_array_mut()
            .expect("the tracker's fields")
            .retain(|field| field["name"] != "name");
    });
    assert!(refused(&mut store, &dropped).contains("drops the field `name`"));
    let retyped = later(|surface| surface["types"][0]["fields"][0]["type"] = "int".into());
    assert!(refused(&mut store, &retyped).contains("retypes the field `name`"));
    let moved = later(|surface| surface["types"][0]["fields"][1]["name"] = "rows".into());
    assert!(refused(&mut store, &moved).contains("no longer nests `Issue` in `issues`"));
    assert!(
        store
            .schema()
            .get("Tracker")
            .is_some_and(|tracker| tracker.field("name").is_some()),
        "the store keeps the type the first version declared"
    );

    let added = later(|surface| {
        surface["types"][0]["fields"]
            .as_array_mut()
            .expect("the tracker's fields")
            .push(serde_json::json!({"name": "due", "type": {"option": "date"}}));
    });
    store
        .declare(Schema::from_surface(&added))
        .expect("a version that only adds is declared");
    assert!(
        store
            .schema()
            .get("Tracker")
            .is_some_and(|tracker| tracker.field("due").is_some())
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
    use collab_types::store::Resource;
    use tilo_resource_store::store::DOCUMENT_LIMIT;
    use tilo_resource_store::{Proposal, Received, Refusal, Stamp, Store, StoreError, Written};

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
        let mut store = Store::new(crate::WORKSPACE);
        store
            .declare(Schema {
                types: vec![tilo_resource_store::Type {
                    name: ISSUE.to_owned(),
                    fields: vec![
                        field("title", FieldKind::Scalar),
                        field("state", FieldKind::Scalar),
                        field("labels", FieldKind::Set),
                    ],
                    nested: Vec::new(),
                    live: None,
                    founding: Vec::new(),
                    origin: None,
                    version: 1,
                }],
                ..Schema::default()
            })
            .unwrap();
        store
    }

    /// [`issues`], with one verb declared: what makes a change's verb checked at all, as every
    /// workspace store that declares any verb does.
    fn verbed() -> Store {
        let mut store = issues();
        store
            .declare(Schema {
                verbs: vec![tilo_resource_store::Verb {
                    name: "edit".to_owned(),
                    writes: BTreeMap::from([(
                        ISSUE.to_owned(),
                        tilo_resource_store::Writes::default(),
                    )]),
                    ..tilo_resource_store::Verb::default()
                }],
                ..Schema::default()
            })
            .unwrap();
        store
    }

    /// [`issues`] with an app's own verb named `undo`, which writes only the issue's title:
    /// the platform writes under `$undo`, so no app needs the plain name for it.
    fn app_undo() -> Store {
        let mut store = issues();
        store
            .declare(Schema {
                verbs: vec![tilo_resource_store::Verb {
                    name: "undo".to_owned(),
                    writes: BTreeMap::from([(
                        ISSUE.to_owned(),
                        tilo_resource_store::Writes {
                            fields: BTreeSet::from(["title".to_owned()]),
                            sets_only: false,
                        },
                    )]),
                    ..tilo_resource_store::Verb::default()
                }],
                ..Schema::default()
            })
            .unwrap();
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
            founding: Vec::new(),
            origin: None,
            version: 1,
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
        let mut store = Store::new(crate::WORKSPACE);
        store
            .declare(Schema {
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
                    founding: Vec::new(),
                    origin: None,
                    version: 1,
                }],
                ..Schema::default()
            })
            .unwrap();
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
        use collab_types::store::wire::{Command, Reply};
        use tilo_resource_store::wire::read;
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
        // What is written in a resource's content is a version of it too, in a document of its
        // own: the second's body is written after all that, and a body elsewhere after that.
        let body = |store: &mut Store, id: &str, stamp: &Stamp| {
            use automerge::transaction::Transactable as _;
            let ((), written) = store
                .edit_content(ISSUE, id, stamp, |tx, root| {
                    let text = tx.put_object(root, "body", automerge::ObjType::Text)?;
                    tx.splice_text(&text, 0, 0, "Hello")?;
                    Ok(())
                })
                .unwrap();
            assert!(written.is_some(), "the body is written");
        };
        body(&mut store, "board/b", &at("b's body", 7));
        body(&mut store, "elsewhere/d", &at("d's body", 8));
        let newest = |store: &Store, limit| match read(
            store,
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
            newest(&store, 10),
            (
                owned(&[
                    ("board/b", "b's body"),
                    ("board/c", "remove"),
                    ("board/a", "change"),
                ]),
                false
            )
        );
        assert_eq!(
            newest(&store, 2),
            (
                owned(&[("board/b", "b's body"), ("board/c", "remove")]),
                true
            ),
            "a page, saying there are more"
        );
        // A body written after the page was read stands first in the next.
        body(&mut store, "board/a", &at("a's body", 9));
        assert_eq!(newest(&store, 1), (owned(&[("board/a", "a's body")]), true));
    }

    /// A call that made many resources stamps each with its argument: the page of the newest
    /// versions says who made them, by the call they signed, and not again with what for each,
    /// while each resource's own versions keep it.
    #[test]
    fn the_newest_versions_name_the_signed_call_without_its_argument() {
        use collab_types::store::wire::{Command, Reply};
        use tilo_resource_store::wire::read;
        let mut store = issues();
        let statement = tilo_actions::remote::signed_invocation::InvocationStatement {
            workspace: "w".to_owned(),
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: "so.tilo.issues".to_owned(),
                address: tilo_actions::ActionAddress::new("board", "new_issues"),
            },
            arguments: [0; 32],
            on_behalf_of: None,
            issued_at_ns: 1,
            signer: [0; 32],
        };
        let signed = tilo_actions::remote::SignedInvocation::new(statement, [0; 64], Vec::new());
        let titles =
            tilo_actions::remote::InvokeArgument::Text("a long list of titles".repeat(1000));
        let called = Stamp {
            invocation: "new issues".to_owned(),
            moment: 7,
            signed: Some(Box::new(tilo_actions::remote::SignedCall {
                signed: signed.clone(),
                argument: Some(titles.clone()),
                allowed: None,
                salt: None,
            })),
            ..Stamp::default()
        };
        for id in ["board/d", "board/e"] {
            store
                .put(ISSUE, id, resource("board", id, id, "open"), &called)
                .unwrap();
        }
        let Some(Reply::Newest { versions, .. }) = read(
            &store,
            &Command::Newest {
                within: "board".to_owned(),
                limit: 2,
            },
            &anyone,
        ) else {
            panic!("the newest versions are read");
        };
        for (_, id, version) in versions {
            let call = version.stamp.and_then(|stamp| stamp.signed).unwrap();
            assert_eq!(call.signed, signed, "{id} keeps the call its caller signed");
            assert_eq!(
                call.argument, None,
                "{id} is read without the call's argument"
            );
        }
        assert_eq!(
            store
                .version_end(ISSUE, "board/d", true)
                .unwrap()
                .stamp
                .and_then(|stamp| stamp.signed)
                .and_then(|call| call.argument),
            Some(titles),
            "the resource's own versions keep it"
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

    /// A record whose id is bound to a sealed kind (Terminal's history) is refused under any other
    /// kind on its first write, while the same write of an unbound id lands.
    #[test]
    fn a_first_write_naming_another_kind_for_a_sealed_id_is_refused() {
        let history = collab_types::workspace_document_id("terminal.history");
        let mut here = issues();
        let mut there = issues();
        let sealed = put(
            &mut here,
            &history,
            resource("board", "a0", "Draft", "open"),
        );
        assert!(
            matches!(
                &deliver(&sealed, &mut there, &|_: &Proposal<'_>| true)[..],
                [Refusal::Undeclared(why)] if why.contains("terminal.history")
            ),
            "an issue cannot be what the history's id names"
        );
        // Written where nothing was refused before it: what follows a refused change is refused
        // with it.
        let (mut here, mut there) = (issues(), issues());
        let open = put(&mut here, "a", resource("board", "a1", "Plan", "open"));
        assert!(deliver(&open, &mut there, &|_: &Proposal<'_>| true).is_empty());
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

    /// `stamp`, keeping the call a caller signed for it, whose argument names `titles` records.
    fn signed_stamp(verb: &str, titles: usize) -> Stamp {
        use tilo_actions::remote::signed_invocation::InvocationStatement;
        use tilo_actions::remote::{InvokeRequest, SignedCall, SignedInvocation};
        let titles: Vec<String> = (0..titles)
            .map(|at| format!("Task {at}, written by the person who asked for it"))
            .collect();
        let request: InvokeRequest = serde_json::from_value(serde_json::json!({
            "invocation": tilo_actions::InvocationId::new(),
            "target": {"provider": "so.tilo.tasks", "address": {"resource": "so.tilo.tasks", "action": verb}},
            "argument": {"Json": {"titles": &titles}},
        }))
        .unwrap();
        let statement = InvocationStatement::of(crate::WORKSPACE, &request, 9, [5; 32]);
        let call = SignedCall::with(
            &request,
            SignedInvocation::new(statement, [7; 64], vec![1; 512]),
        );
        // The runtime records the arguments beside the call, as an app's action does.
        Stamp {
            signed: Some(Box::new(call)),
            args: vec![serde_json::json!(titles)],
            ..stamp(verb, "i1")
        }
    }

    /// A call that writes many records of one document keeps itself once there, in its first
    /// change, and every other change names it by digest: what the call writes grows with how
    /// many records it writes, never with that times its argument. A holder reads each change
    /// with the call whole, and refuses one whose call is not kept, or not the one it names.
    #[test]
    fn a_call_writing_many_records_keeps_itself_once_per_document() {
        use collab_types::store::wire::Command;
        let drafted = |count: usize| {
            let mut store = issues();
            let stamp = signed_stamp("make", count);
            let command = Command::Batch(
                (0..count)
                    .map(|at| Command::Put {
                        kind: ISSUE.to_owned(),
                        id: format!("t{at}"),
                        resource: resource("board", &format!("a{at:04}"), "Task", "open"),
                        stamp: stamp.clone(),
                    })
                    .collect(),
            );
            let written = tilo_resource_store::wire::draft(&mut store, command).unwrap();
            assert_eq!(written.len(), count);
            let bytes: usize = written
                .iter()
                .map(|write| write.change.raw_bytes().len())
                .sum();
            (written, bytes)
        };
        let (hundred, _) = drafted(100);
        let (written, bytes) = drafted(200);
        let hundred: usize = hundred
            .iter()
            .map(|write| write.change.raw_bytes().len())
            .sum();
        // Twice the records, about twice the bytes: each change past the first names the call
        // in a fixed few bytes, whatever its argument holds.
        assert!(
            bytes * 10 < hundred * 22,
            "200 records: {bytes} bytes; 100: {hundred}"
        );
        let keeping = written
            .iter()
            .filter(|write| Stamp::of(&write.change).unwrap().signed.is_some())
            .count();
        let documents: BTreeSet<&str> = written.iter().map(|write| write.doc.as_str()).collect();
        assert_eq!(keeping, documents.len(), "one copy per document");
        let naming = written
            .iter()
            .filter(|write| Stamp::of(&write.change).unwrap().signed.is_none())
            .map(|write| write.change.raw_bytes().len())
            .max()
            .unwrap();
        assert!(
            naming < 1024,
            "a change naming its call takes {naming} bytes"
        );

        // Every holder reads every change with the call it was made for.
        let mut there = issues();
        let signed = |proposal: &Proposal<'_>| {
            proposal
                .stamp
                .and_then(|stamp| stamp.signed.as_deref())
                .is_some_and(|call| call.argument_signed() == Some(true))
        };
        assert!(deliver(&written, &mut there, &signed).is_empty());
        assert_eq!(there.index().ids_of(ISSUE).count(), 200);

        // A change naming a call no change of its document keeps, or keeping another call than
        // the one it names, is refused.
        let forge = |named: Option<&Stamp>, kept: bool| {
            let mut forger = issues();
            let mut stamp = signed_stamp("make", 3);
            stamp.name_call(kept);
            if let Some(other) = named {
                stamp.call.clone_from(&other.call);
            }
            let written = forger
                .put(
                    ISSUE,
                    "f",
                    resource("board", "f0", "Forged", "open"),
                    &stamp,
                )
                .unwrap();
            deliver(&written, &mut issues(), &signed)
        };
        assert_eq!(
            forge(None, true),
            [Refusal::Uncalled(
                tilo_actions::stamp::CallUnresolved::Missing
            )]
        );
        let mut other = signed_stamp("make", 4);
        other.name_call(false);
        assert_eq!(
            forge(Some(&other), false),
            [Refusal::Uncalled(
                tilo_actions::stamp::CallUnresolved::Mismatch
            )]
        );
    }

    /// `stamp`, keeping the call a caller signed with `argument`, its parts signed too
    /// ([`tilo_actions::remote::SignedCall::of_parts`]), as the request brings it to the host.
    fn parted_stamp(
        verb: &str,
        argument: &serde_json::Value,
        args: Vec<serde_json::Value>,
    ) -> Stamp {
        use tilo_actions::remote::signed_invocation::InvocationStatement;
        use tilo_actions::remote::{InvokeRequest, SignedCall, SignedInvocation};
        let mut request: InvokeRequest = serde_json::from_value(serde_json::json!({
            "invocation": tilo_actions::InvocationId::new(),
            "target": {"provider": "so.tilo.tasks", "address": {"resource": "so.tilo.tasks", "action": verb}},
            "argument": {"Json": argument},
        }))
        .unwrap();
        let statement = InvocationStatement::of(crate::WORKSPACE, &request, 9, [5; 32]);
        let salt = [3; 32];
        let parts = statement.clone().over_parts(&request.argument, &salt);
        request.signed = Some(Box::new(SignedInvocation::new(
            statement,
            [6; 64],
            vec![1; 64],
        )));
        request.context = Some(tilo_actions::ActionContext {
            signed: Some(Box::new(SignedCall::signed_over_parts(
                SignedInvocation::new(parts, [7; 64], vec![1; 64]),
                &salt,
            ))),
            ..tilo_actions::ActionContext::default()
        });
        let call = SignedCall::of(&request).unwrap();
        assert!(call.in_flight());
        Stamp {
            signed: Some(Box::new(call)),
            args,
            ..stamp(verb, "i1")
        }
    }

    /// One call writes a record its whole workspace reads and one only its caller reads, its
    /// caller having signed its argument's parts: each document keeps the call without its
    /// argument, and of the argument only the parts its own changes hold, with their proof. The
    /// shared document's readers never receive the private part, in any byte of it; every
    /// holder admits both documents' changes, and refuses a part changed, a proof changed, or
    /// the other document's part under this one's proof.
    #[test]
    fn a_call_writing_a_shared_and_a_private_record_keeps_in_each_only_its_own_part() {
        use collab_types::store::wire::Command;
        let (shared, private) = ("Plan the offsite SHARED-7f3", "Ada's diagnosis PRIVATE-91c");
        let call = parted_stamp(
            "make",
            &serde_json::json!({"shared": shared, "private": private, "count": 2}),
            vec![serde_json::json!(shared), serde_json::json!(private)],
        );
        let mut store = issues();
        let written = tilo_resource_store::wire::draft(
            &mut store,
            Command::Batch(vec![
                Command::Put {
                    kind: ISSUE.to_owned(),
                    id: "s".to_owned(),
                    resource: resource("board", "a0", shared, "open"),
                    stamp: call.clone(),
                },
                Command::Put {
                    kind: ISSUE.to_owned(),
                    id: "p".to_owned(),
                    resource: read_by("ada-only", "board", "a1", private, "open"),
                    stamp: call,
                },
            ]),
        )
        .unwrap();
        apart(&written, shared, private);
        for write in &written {
            let stamp = Stamp::of(&write.change).unwrap();
            let parts = stamp.parts.as_ref().expect("each change keeps its parts");
            let paths: Vec<&str> = parts.parts.iter().map(|part| part.path.as_str()).collect();
            let call = stamp.signed.as_deref().unwrap();
            assert!(call.argument.is_none() && call.salt.is_none());
            assert!(parts.proves(&call.signed.statement().arguments));
            if write.readers == MEMBERS {
                assert_eq!(paths, ["/shared"]);
                assert_eq!(
                    stamp.args,
                    [serde_json::json!(shared), serde_json::Value::Null]
                );
            } else {
                assert_eq!(paths, ["/private"]);
                assert_eq!(
                    stamp.args,
                    [serde_json::Value::Null, serde_json::json!(private)]
                );
            }
        }

        // Every holder admits both, each with parts that prove against its call.
        let proven = |proposal: &Proposal<'_>| {
            proposal.stamp.is_some_and(|stamp| {
                stamp
                    .signed
                    .as_deref()
                    .zip(stamp.parts.as_ref())
                    .is_some_and(|(call, parts)| parts.proves(&call.signed.statement().arguments))
            })
        };
        let mut there = issues();
        assert!(deliver(&written, &mut there, &proven).is_empty());
        assert_eq!(there.index().ids_of(ISSUE).count(), 2);

        forgeries_are_refused(&written, shared);
    }

    /// What the shared document's readers receive of `written`, uncompressed, holds `shared`
    /// and not `private`, in any byte; what only Ada reads, the other way around.
    fn apart(written: &[Written], shared: &str, private: &str) {
        let received = |readers: &str| {
            let mut doc = automerge::Automerge::new();
            doc.apply_changes(
                written
                    .iter()
                    .filter(|write| write.readers == readers)
                    .map(|write| write.change.clone()),
            )
            .unwrap();
            doc.save_nocompress()
        };
        let holds = |bytes: &[u8], text: &str| {
            bytes
                .windows(text.len())
                .any(|window| window == text.as_bytes())
        };
        let (everyone, ada) = (received(MEMBERS), received("ada-only"));
        assert!(holds(&everyone, shared) && !holds(&everyone, private));
        assert!(holds(&ada, private) && !holds(&ada, shared));
    }

    /// One call no one signed writes a record its whole workspace reads and one only its caller
    /// reads, its arguments recorded beside it: each document keeps of them only what it holds
    /// once written, every other value null, so the shared document's readers never receive the
    /// private one.
    #[test]
    fn an_unsigned_call_writing_a_shared_and_a_private_record_keeps_in_each_only_its_own_args() {
        use collab_types::store::wire::Command;
        let (shared, private) = ("Plan the offsite SHARED-2d8", "Ada's diagnosis PRIVATE-c40");
        let call = Stamp {
            args: vec![serde_json::json!(shared), serde_json::json!(private)],
            ..stamp("make", "i1")
        };
        let written = tilo_resource_store::wire::draft(
            &mut issues(),
            Command::Batch(vec![
                Command::Put {
                    kind: ISSUE.to_owned(),
                    id: "s".to_owned(),
                    resource: resource("board", "a0", shared, "open"),
                    stamp: call.clone(),
                },
                Command::Put {
                    kind: ISSUE.to_owned(),
                    id: "p".to_owned(),
                    resource: read_by("ada-only", "board", "a1", private, "open"),
                    stamp: call,
                },
            ]),
        )
        .unwrap();
        apart(&written, shared, private);
        for write in &written {
            let args = Stamp::of(&write.change).unwrap().args;
            if write.readers == MEMBERS {
                assert_eq!(args, [serde_json::json!(shared), serde_json::Value::Null]);
            } else {
                assert_eq!(args, [serde_json::Value::Null, serde_json::json!(private)]);
            }
        }
        let mut there = issues();
        assert!(deliver(&written, &mut there, &|_: &Proposal<'_>| true).is_empty());
        assert_eq!(there.index().ids_of(ISSUE).count(), 2);
    }

    /// A part changed, a proof changed, or the private part under the shared one's proof, of the
    /// call [`a_call_writing_a_shared_and_a_private_record_keeps_in_each_only_its_own_part`]
    /// wrote, are refused; the change as written is not.
    fn forgeries_are_refused(written: &[Written], shared: &str) {
        use tilo_actions::stamp::CallUnresolved;
        let of = |readers: &str| {
            let write = written
                .iter()
                .find(|write| write.readers == readers)
                .unwrap();
            Stamp::of(&write.change).unwrap()
        };
        let (kept, other) = (of(MEMBERS), of("ada-only"));
        let forged = |edit: &dyn Fn(&mut Stamp)| {
            let mut stamp = kept.clone();
            edit(&mut stamp);
            let mut forger = issues();
            let written = forger
                .put(ISSUE, "f", resource("board", "f0", shared, "open"), &stamp)
                .unwrap();
            deliver(&written, &mut issues(), &|_: &Proposal<'_>| true)
        };
        let unproven = [Refusal::Uncalled(CallUnresolved::Unproven)];
        assert_eq!(
            forged(&|stamp| stamp.parts.as_mut().unwrap().parts[0].value = serde_json::json!("x")),
            unproven
        );
        assert_eq!(
            forged(&|stamp| stamp.parts.as_mut().unwrap().proof[0] = "01".repeat(32)),
            unproven
        );
        assert_eq!(
            forged(&|stamp| stamp
                .parts
                .as_mut()
                .unwrap()
                .parts
                .clone_from(&other.parts.as_ref().unwrap().parts)),
            unproven
        );
        assert!(forged(&|_| {}).is_empty());
    }

    fn stamp(verb: &str, invocation: &str) -> Stamp {
        Stamp {
            invoked: String::new(),
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
        use collab_types::store::wire::{Command, Edit};
        use serde_json::json;
        use tilo_resource_store::wire::write;

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
        use collab_types::store::wire::{Command, Reply};
        use tilo_resource_store::wire::read;
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
        use collab_types::store::wire::{Command, Reply};
        use tilo_resource_store::wire::read;
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
        use collab_types::store::Query;
        use collab_types::store::wire::{Command, Reply};
        use tilo_resource_store::wire::read;
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
        use collab_types::history::{Version, VersionKind};
        use collab_types::store::wire::{Command, Reply};
        use serde_json::json;
        use tilo_resource_store::wire::read;

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
        use collab_types::store::wire::{Command, Reply};
        use serde_json::json;
        use tilo_resource_store::wire::read;

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
        use collab_types::store::wire::Command;
        use serde_json::json;
        use tilo_resource_store::wire::write;

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

    /// `write` is stamped as `undo`, and says which changes of its document it took back: each
    /// one of the invocation `invocation`.
    /// Asserts `write` is stamped as `undo`, which history reads as an undo of the one invocation
    /// `invocation` and its verb `verb`.
    fn takes_back(write: &Written, undo: &Stamp, invocation: &str, verb: &str) {
        let stamp = Stamp::of(&write.change).expect("the undo is stamped");
        assert_eq!(&stamp, undo);
        let Some(tilo_actions::Did::History(step)) = stamp.action_context().what else {
            panic!("the undo is a step through history: {stamp:?}");
        };
        assert_eq!(step.op, tilo_actions::HistoryOp::Undo);
        assert_eq!(step.undoes.as_deref(), Some(invocation));
        assert_eq!(step.verb, verb);
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
        // Each is stamped as the undo, one invocation per change.
        for write in &written {
            takes_back(write, &undo, "i2", "rename");
        }

        // Undoing the undo takes the rename back again.
        store.undo("i4", &stamp("rename", "i5")).unwrap();
        assert_eq!(
            store.get(ISSUE, "a", &all).unwrap().fields["title"],
            Cell::One("Final".into())
        );
        assert!(store.get(ISSUE, "b", &all).is_some());
    }

    /// A stamp of `verb` in `invocation`, written at `moment`.
    fn stamp_at(verb: &str, invocation: &str, moment: u64) -> Stamp {
        Stamp {
            moment,
            ..stamp(verb, invocation)
        }
    }

    /// An issue drafted, checkpointed as "Drafted", then retitled, given a nested part and new
    /// content: the store, and the checkpoint's version.
    fn drafted_then_changed() -> (Store, String) {
        use automerge::transaction::Transactable as _;

        let at = stamp_at;
        let mut store = issues();
        store
            .put(
                ISSUE,
                "a",
                resource("board", "a0", "Draft", "open"),
                &at("make", "i1", 1),
            )
            .unwrap();
        let ((), _) = store
            .edit_content(ISSUE, "a", &at("make", "i1", 1), |tx, root| {
                tx.put(root, "note", "first")
            })
            .unwrap();
        let checkpoint = store
            .checkpoint(ISSUE, "a", "Drafted", &at("checkpoint", "i2", 2))
            .unwrap();
        let named = store
            .versions(ISSUE, "a")
            .into_iter()
            .find(|version| version.change == checkpoint.change.hash().to_string())
            .expect("the checkpoint is a version of what it names");
        let named = named.stamp.unwrap();
        assert_eq!(named.verb, Stamp::CHECKPOINT_VERB);
        assert_eq!(named.args.first(), Some(&"Drafted".into()));

        // After it: a new title, a new resource nested in it, and new content.
        store
            .put(
                ISSUE,
                "a",
                resource("board", "a0", "Final", "open"),
                &at("edit", "i3", 3),
            )
            .unwrap();
        store
            .put(
                ISSUE,
                "b",
                resource("a", "b0", "Part", "open"),
                &at("edit", "i3", 3),
            )
            .unwrap();
        let ((), _) = store
            .edit_content(ISSUE, "a", &at("edit", "i3", 3), |tx, root| {
                tx.put(root, "note", "second")
            })
            .unwrap();
        (store, checkpoint.change.hash().to_string())
    }

    /// A checkpoint is a version of the resource it names; a restore to it brings the resource,
    /// what is nested in it and its content back as they stood then, after a review that a later
    /// change makes stale, as one step an undo takes back.
    #[test]
    fn a_restore_to_a_checkpoint_brings_back_what_stood_then_and_is_undone_as_one_step() {
        use automerge::ReadDoc as _;

        let all = |_: &str| true;
        let at = stamp_at;
        let note = |store: &mut Store| {
            let (doc, root) = store.content(ISSUE, "a").unwrap().unwrap();
            doc.get(&root, "note").unwrap().unwrap().0.to_string()
        };
        let title = |store: &Store| store.get(ISSUE, "a", &all).unwrap().fields["title"].clone();
        let (mut store, version) = drafted_then_changed();
        let resources = [(ISSUE.to_owned(), "a".to_owned())];
        let review = store.review_restore(&resources, &version).unwrap();
        assert_eq!(review.differing, ["a", "b"]);
        // A change after the review makes it stale: the restore is refused.
        store
            .put(
                ISSUE,
                "a",
                resource("board", "a0", "Final", "done"),
                &at("close", "i4", 4),
            )
            .unwrap();
        let refused = store
            .restore_version(
                &resources,
                &version,
                None,
                Some(&review.live),
                &at("restore", "i5", 5),
            )
            .unwrap_err();
        assert!(matches!(refused, StoreError::History(_)), "{refused}");
        assert_eq!(
            store.get(ISSUE, "a", &all).unwrap().fields["state"],
            Cell::One("done".into())
        );

        let review = store.review_restore(&resources, &version).unwrap();
        let restore = at("restore", "i5", 5);
        let written = store
            .restore_version(&resources, &version, None, Some(&review.live), &restore)
            .unwrap();
        assert_eq!(title(&store), Cell::One("Draft".into()));
        assert_eq!(
            store.get(ISSUE, "a", &all).unwrap().fields["state"],
            Cell::One("open".into())
        );
        assert!(
            store.get(ISSUE, "b", &all).is_none(),
            "what was made after it is gone"
        );
        assert_eq!(note(&mut store), "\"first\"");
        for write in &written {
            let stamp = Stamp::of(&write.change).unwrap();
            assert_eq!(stamp.invocation, "i5");
            let Some(tilo_actions::Did::History(step)) = stamp.context.unwrap().what else {
                panic!("a restore is a step through history");
            };
            assert_eq!(step.op, tilo_actions::HistoryOp::Restore);
        }
        // Everything stands as it did then: a second restore has nothing to write.
        assert!(matches!(
            store.restore_version(&resources, &version, None, None, &at("restore", "i6", 6)),
            Err(StoreError::History(_))
        ));

        // Undoing the restore brings back what it replaced.
        store
            .undo(
                "i5",
                &Stamp {
                    undoes: Some("i5".to_owned()),
                    ..at("restore", "i7", 7)
                },
            )
            .unwrap();
        assert_eq!(title(&store), Cell::One("Final".into()));
        assert!(store.get(ISSUE, "b", &all).is_some());
        assert_eq!(note(&mut store), "\"second\"");

        // A restore of only some of them leaves the rest.
        let only = ["b".to_owned()];
        store
            .restore_version(
                &resources,
                "Drafted",
                Some(&only),
                None,
                &at("restore", "i8", 8),
            )
            .unwrap();
        assert!(store.get(ISSUE, "b", &all).is_none());
        assert_eq!(title(&store), Cell::One("Final".into()));
        // A change of none of them is no version of theirs.
        assert!(matches!(
            store.review_restore(&resources, &"0".repeat(64)),
            Err(StoreError::History(_))
        ));
    }

    /// A checkpoint of `a`, then a resource nested in it: the document of what was named stands
    /// as it did, and what was made after it is in the document its nesting placed it in.
    fn checkpointed_then_nested() -> (Store, String) {
        let at = stamp_at;
        let mut store = issues();
        store
            .put(
                ISSUE,
                "a",
                resource("board", "a0", "Draft", "open"),
                &at("make", "i1", 1),
            )
            .unwrap();
        let checkpoint = store
            .checkpoint(ISSUE, "a", "Drafted", &at("checkpoint", "i2", 2))
            .unwrap();
        store
            .put(
                ISSUE,
                "b",
                resource("a", "b0", "Part", "open"),
                &at("make", "i3", 3),
            )
            .unwrap();
        (store, checkpoint.change.hash().to_string())
    }

    /// A restore is drafted on a private copy of the store before it is written: the copy sees
    /// the documents the live store sees, so what is nested in what it restores goes back too,
    /// and a review of the live store is not stale when the write is drafted.
    #[test]
    fn a_drafted_restore_sees_the_resources_nested_in_what_it_restores() {
        use collab_types::store::wire::Command;

        let all = |_: &str| true;
        let (mut store, version) = checkpointed_then_nested();
        let resources = [(ISSUE.to_owned(), "a".to_owned())];
        let review = store.review_restore(&resources, &version).unwrap();
        assert_eq!(review.differing, ["b"]);

        // By its name: the resource made after the checkpoint is in a document of its own, and
        // the draft that judges the write takes it back with the one it names.
        let by_name = Command::Restore {
            resources: resources.to_vec(),
            change: "Drafted".to_owned(),
            only: None,
            live: None,
            stamp: stamp_at("restore", "i4", 4),
        };
        let drafted = tilo_resource_store::wire::draft(&mut store, by_name)
            .expect("the restore by name is drafted");
        assert!(
            !drafted.is_empty(),
            "the drafted restore takes the nested resource back"
        );

        // Reviewed: the live store's review is not stale when the write is drafted.
        let by_review = Command::Restore {
            resources: resources.to_vec(),
            change: version,
            only: None,
            live: Some(review.live),
            stamp: stamp_at("restore", "i5", 5),
        };
        tilo_resource_store::wire::draft(&mut store, by_review)
            .expect("the reviewed restore is drafted");
        assert!(
            store.get(ISSUE, "b", &all).is_some(),
            "a drafted write leaves the live store as it was"
        );
    }

    /// A drafted restore still refuses a review the store moved after, and still answers that
    /// what it was asked to restore already stands: seeing the nested documents widens what a
    /// restore can take back, never what it accepts.
    #[test]
    fn a_drafted_restore_refuses_a_stale_review_and_answers_one_that_already_stands() {
        use collab_types::store::wire::Command;

        let (mut store, version) = checkpointed_then_nested();
        let resources = [(ISSUE.to_owned(), "a".to_owned())];
        let review = store.review_restore(&resources, &version).unwrap();
        // A change after the review makes it stale, though the change before the nesting is
        // what the restore would take back.
        store
            .put(
                ISSUE,
                "b",
                resource("a", "b0", "Part", "done"),
                &stamp_at("edit", "i4", 4),
            )
            .unwrap();
        let stale = Command::Restore {
            resources: resources.to_vec(),
            change: version,
            only: None,
            live: Some(review.live),
            stamp: stamp_at("restore", "i5", 5),
        };
        let refused = tilo_resource_store::wire::draft(&mut store, stale).unwrap_err();
        assert!(
            refused.error.to_string().contains("review it again"),
            "{refused}"
        );
        // Only `a` is asked for, and it stands where the checkpoint named it: nothing to write.
        let stands = Command::Restore {
            resources: resources.to_vec(),
            change: "Drafted".to_owned(),
            only: Some(vec!["a".to_owned()]),
            live: None,
            stamp: stamp_at("restore", "i6", 6),
        };
        let refused = tilo_resource_store::wire::draft(&mut store, stands).unwrap_err();
        assert!(
            refused.error.to_string().contains("already stands"),
            "{refused}"
        );
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
            there.declare(Schema::from_surface(&surface)).unwrap();
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
            there.declare(Schema::from_surface(&surface)).unwrap();
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
        there
            .declare(Schema::from_surface(&super::tilolang_surface()))
            .unwrap();
        there
            .declare(Schema::from_surface(&super::tilolang_item_surface()))
            .unwrap();
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

    /// The verbs the platform writes under name no app. A store that declares any verb refused
    /// their changes as undeclared, so a device could not keep the call it carried out or the
    /// block that settled it, and a step could be taken neither back nor brought back. Every schema declares them, as it declares the move and the creation: each
    /// writes any field of a declared kind and no kind the store does not declare.
    #[test]
    fn the_platforms_verbs_are_declared_in_every_schema_and_write_a_declared_kind() {
        let anyone = |_: &Proposal<'_>| true;
        let made = |verb: &str, kind: &str| {
            let resource = Resource {
                readers: MEMBERS.to_owned(),
                parent: None,
                order: None,
                fields: fields("platform", "open"),
            };
            issues()
                .put(kind, "p", resource, &stamp(verb, "i1"))
                .unwrap()
        };
        for verb in tilo_resource_store::PLATFORM_VERBS {
            assert_eq!(
                deliver(&made(verb, ISSUE), &mut verbed(), &anyone),
                [],
                "{verb} writes a declared kind"
            );
            assert!(
                matches!(
                    deliver(&made(verb, "gallery.Item"), &mut verbed(), &anyone).as_slice(),
                    [Refusal::Undeclared(why)] if why.contains("gallery.Item")
                ),
                "{verb} writes no kind the store does not declare"
            );
        }
        assert!(
            matches!(
                deliver(&made("$nope", ISSUE), &mut verbed(), &anyone).as_slice(),
                [Refusal::Undeclared(why)] if why.contains("no verb `$nope`")
            ),
            "a verb no schema declares is still refused"
        );
    }

    /// A change that made several actions of a record at once names the group its entry says it
    /// was: every action it names is declared, and one of them admits what the change wrote. An
    /// action no schema declares is refused, and so is a group none of whose actions writes what
    /// the change wrote.
    #[test]
    fn a_change_of_a_group_of_actions_is_admitted_by_the_verbs_it_names() {
        use tilo_actions::{ActionKey, Did};
        let anyone = |_: &Proposal<'_>| true;
        let writing = |fields: &[&str]| {
            BTreeMap::from([(
                ISSUE.to_owned(),
                tilo_resource_store::Writes {
                    fields: fields.iter().map(|field| (*field).to_owned()).collect(),
                    sets_only: false,
                },
            )])
        };
        let verb = |name: &str, fields: &[&str]| tilo_resource_store::Verb {
            name: name.to_owned(),
            writes: writing(fields),
            ..tilo_resource_store::Verb::default()
        };
        let declaring = |verbs: Vec<tilo_resource_store::Verb>| {
            let mut store = issues();
            store
                .declare(Schema {
                    verbs,
                    ..Schema::default()
                })
                .unwrap();
            store
        };
        let grouped = |names: &[&str]| Stamp {
            context: Some(Box::new(tilo_actions::ActionContext {
                what: Some(Did::group(names.iter().map(|name| {
                    Did::verb(ActionKey::new("so.tilo.tasks", ISSUE, *name), [])
                }))),
                ..tilo_actions::ActionContext::default()
            })),
            ..Stamp::default()
        };
        let write = |stamp: &Stamp| {
            issues()
                .put(
                    ISSUE,
                    "g",
                    resource("board", "g0", "Grouped", "open"),
                    stamp,
                )
                .unwrap()
        };
        assert_eq!(
            deliver(
                &write(&grouped(&["first", "second"])),
                &mut declaring(vec![
                    verb("first", &["title", "state", "labels"]),
                    verb("second", &["title", "state", "labels"]),
                ]),
                &anyone
            ),
            [],
            "the record it wrote is admitted by the actions it names"
        );
        assert!(
            matches!(
                deliver(
                    &write(&grouped(&["first", "nope"])),
                    &mut declaring(vec![
                        verb("first", &["title", "state", "labels"]),
                        verb("second", &["title", "state", "labels"]),
                    ]),
                    &anyone
                )
                .as_slice(),
                [Refusal::Undeclared(why)] if why.contains("no verb `nope`")
            ),
            "a group may not name an action the store's schema knows nothing of"
        );
        assert!(
            matches!(
                deliver(
                    &write(&grouped(&["titles", "headings"])),
                    &mut declaring(vec![verb("titles", &["title"]), verb("headings", &["title"])]),
                    &anyone
                )
                .as_slice(),
                [Refusal::Undeclared(why)] if why.contains("labels")
            ),
            "a group none of whose actions writes what it wrote is refused"
        );
    }

    /// A step through history is the platform's own write, under the verb its step is
    /// ([`Stamp::UNDO_VERB`] or its restore): a store stamp names it, and a change's entry says
    /// which step it takes.
    #[test]
    fn a_step_through_history_is_admitted_as_the_platforms_verb() {
        use tilo_actions::{Described, Did, HistoryOp};
        let anyone = |_: &Proposal<'_>| true;
        for (op, id) in [
            (HistoryOp::Undo, "u"),
            (HistoryOp::Redo, "r"),
            (HistoryOp::Restore, "s"),
        ] {
            let stamp = Stamp {
                context: Some(Box::new(
                    tilo_actions::ActionContext::for_resource(id, ISSUE, "Issue")
                        .with_described(Described::new(Did::history(op.clone(), 1, None))),
                )),
                ..Stamp::default()
            };
            let made = issues()
                .put(ISSUE, id, resource("board", id, "History", "open"), &stamp)
                .unwrap();
            assert_eq!(
                deliver(&made, &mut verbed(), &anyone),
                [],
                "{op:?} is the platform's write, whichever form names it"
            );
        }
    }

    /// An app's own verb named `undo` keeps exactly the write set its surface declares: the
    /// platform's wider admission is the reserved `$undo`, which the app may not declare, so a
    /// name collision never borrows it.
    #[test]
    fn an_apps_undo_action_keeps_its_surfaces_write_set() {
        let anyone = |_: &Proposal<'_>| true;
        let only_title = || Resource {
            readers: MEMBERS.to_owned(),
            parent: None,
            order: None,
            fields: BTreeMap::from([("title".to_owned(), Cell::One("undo".into()))]),
        };
        let inside = issues()
            .put(ISSUE, "i", only_title(), &stamp("undo", "i1"))
            .unwrap();
        assert_eq!(
            deliver(&inside, &mut app_undo(), &anyone),
            [],
            "the field its surface lists is the app's to write under undo"
        );
        let outside = issues()
            .put(
                ISSUE,
                "i",
                Resource {
                    readers: MEMBERS.to_owned(),
                    parent: None,
                    order: None,
                    fields: BTreeMap::from([("state".to_owned(), Cell::One("open".into()))]),
                },
                &stamp("undo", "i1"),
            )
            .unwrap();
        assert!(
            matches!(
                deliver(&outside, &mut app_undo(), &anyone).as_slice(),
                [Refusal::Undeclared(why)] if why.contains("`undo` writes no `Issue.state`")
            ),
            "a field its surface lists not is refused: the platform's admission is not borrowed"
        );
    }

    /// A verb the workspace grants its author still writes only the kinds its surface declares:
    /// a change of an issue's field under a verb that writes comments is refused, though the
    /// same author is allowed that verb at the issue. The store checks the write set whatever
    /// the grant says.
    #[test]
    fn a_granted_verb_writes_no_kind_its_surface_does_not() {
        use tilo_resource_store::{Verb, Writes};
        let anyone = |_: &Proposal<'_>| true;
        let mut holder = Store::new(crate::WORKSPACE);
        holder
            .declare(Schema {
                kind: String::new(),
                types: vec![record_type(ISSUE), record_type("Comment")],
                verbs: vec![Verb {
                    name: "reply".to_owned(),
                    writes: BTreeMap::from([("Comment".to_owned(), Writes::default())]),
                    ..Verb::default()
                }],
                readers: BTreeMap::new(),
            })
            .unwrap();
        let change = issues()
            .put(
                ISSUE,
                "a",
                resource("board", "a0", "Draft", "open"),
                &stamp("reply", "i1"),
            )
            .unwrap();
        assert!(
            matches!(
                deliver(&change, &mut holder, &anyone).as_slice(),
                [Refusal::Undeclared(why)] if why.contains("`reply` writes no `Issue`")
            ),
            "a granted verb still writes no kind its surface does not"
        );
    }

    /// A package's root is made by the host as a change of its own, against the package's
    /// surface: the store admits it, since the surface declares the record and that `$create`
    /// writes it. A surface read without its own record refused every app's first change.
    #[test]
    fn a_package_creates_its_root_under_its_surface() {
        let mut there = issues();
        there
            .declare(Schema::from_surface(&super::tilolang_surface()))
            .unwrap();
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
        let mut there = Store::new(crate::WORKSPACE);
        let anyone = |_: &Proposal<'_>| true;
        let made = Store::new(crate::WORKSPACE)
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
        there.declare(holding).unwrap();
        there
            .declare(Schema::from_surface(&super::tilolang_held_surface()))
            .unwrap();
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
        there
            .declare(Schema::from_surface(&super::tracker_surface()))
            .unwrap();
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

    /// An app's record opened empty is founded as created, a verb every schema has and no app
    /// declares: it makes the record and writes nothing in it. A creation that writes is refused,
    /// and a founding that names no verb stays refused.
    #[test]
    fn an_empty_record_is_founded_as_created_and_a_founding_naming_no_verb_is_refused() {
        use tilo_resource_store::{CREATE_VERB, Refusal};
        let mut there = issues();
        there
            .declare(Schema::from_surface(&super::tracker_surface()))
            .unwrap();
        let anyone = |_: &Proposal<'_>| true;
        let founding = |stamp: Stamp, id: &str, text: Option<&str>| {
            let mut fields = BTreeMap::new();
            if let Some(text) = text {
                fields.insert("text".to_owned(), Cell::One(text.into()));
            }
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
            deliver(
                &founding(stamp(CREATE_VERB, "c1"), "c1", None),
                &mut there,
                &anyone
            ),
            []
        );
        assert!(matches!(
            deliver(&founding(stamp(CREATE_VERB, "c2"), "c2", Some("Smuggled")), &mut there, &anyone)
                .as_slice(),
            [Refusal::Undeclared(why)] if why.contains("only founds")
        ));
        assert!(matches!(
            deliver(&founding(Stamp::default(), "c3", None), &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("verb")
        ));
        let all = |_: &str| true;
        assert!(there.get("Comment", "c1", &all).is_some());
        assert!(there.get("Comment", "c2", &all).is_none());
        assert!(there.get("Comment", "c3", &all).is_none());
    }

    /// A branch keeps the history it copies and adds one step naming the store's branch verb: it
    /// founds the record it carried across, or writes that record's content, and nothing else. A
    /// write merely naming the verb, one that writes a field of the store's own, one touching
    /// another record, or one naming no verb is refused.
    #[test]
    fn a_branch_steps_the_copied_history_and_writes_nothing_else() {
        use automerge::transaction::Transactable as _;
        use tilo_resource_store::{BRANCH_VERB, MOVE_VERB, Refusal};
        let mut there = issues();
        there
            .declare(Schema::from_surface(&super::tracker_surface()))
            .unwrap();
        let anyone = |_: &Proposal<'_>| true;
        // The step the fork's writer commits, read back as the store reads a change.
        let step = collab_types::document::identity::branch_step("c1", "Comment", "Comment");
        let message = step.change_message().expect("an entry");
        let entered = Stamp::of_message(&message).expect("the branch's step");
        assert_eq!(entered.verb, BRANCH_VERB);
        let empty = || Resource {
            readers: MEMBERS.to_owned(),
            parent: None,
            order: None,
            fields: BTreeMap::new(),
        };
        // The record it carried across is founded in the branch's store.
        let founding = issues().put("Comment", "c1", empty(), &entered).unwrap();
        assert_eq!(deliver(&founding, &mut there, &anyone), []);
        // Its content step writes the record's content on that history.
        let mut here = issues();
        let placed = here
            .put("Comment", "c2", empty(), &stamp(MOVE_VERB, "i1"))
            .unwrap();
        send(&placed, &mut there);
        let ((), carried) = here
            .edit_content("Comment", "c2", &entered, |tx, root| {
                tx.put(root, "text", "Ada").map(drop)
            })
            .unwrap();
        assert_eq!(
            deliver(&[carried.expect("a change")], &mut there, &anyone),
            []
        );
        // A field of the store's own is no step of a branch's.
        let field = here
            .put(
                "Comment",
                "c2",
                Resource {
                    readers: MEMBERS.to_owned(),
                    parent: None,
                    order: None,
                    fields: BTreeMap::from([("text".to_owned(), Cell::One("Smuggled".into()))]),
                },
                &entered,
            )
            .unwrap();
        assert!(matches!(
            deliver(&field, &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("carries")
        ));
        // One step names one record, never two.
        {
            use automerge::{ObjType, ROOT};
            let mut two = automerge::AutoCommit::new();
            for id in ["c3", "c4"] {
                let held = two
                    .put_object(ROOT, format!("r:{id}"), ObjType::Map)
                    .unwrap();
                two.put(&held, "k", "Comment").unwrap();
            }
            two.commit_with(automerge::transaction::CommitOptions::default().with_message(message));
            let pair = Written {
                doc: "*/Comment#7".to_owned(),
                readers: MEMBERS.to_owned(),
                change: two.get_last_local_change().unwrap(),
            };
            assert!(matches!(
                deliver(&[pair], &mut there, &anyone).as_slice(),
                [Refusal::Undeclared(why)] if why.contains("one record")
            ));
        }
        // A write that merely names the verb is no branch step.
        let bare = here
            .edit_content("Comment", "c2", &stamp(BRANCH_VERB, "i2"), |tx, root| {
                tx.put(root, "text", "Grace").map(drop)
            })
            .unwrap()
            .1
            .expect("a change");
        assert!(matches!(
            deliver(&[bare], &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("only admits the step")
        ));
        // A write naming no verb stays refused.
        let unnamed = issues()
            .put("Comment", "c5", empty(), &Stamp::default())
            .unwrap();
        assert!(matches!(
            deliver(&unnamed, &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("verb")
        ));
    }

    /// The store's origin write names no app: what an outside service said lands as a change of
    /// the platform's, writing any field or content of a kind an origin declares, as the move
    /// places one of any kind. It is declared once, in every schema, and a surface that names it
    /// adds nothing to it: a change whose entry names an action is an app action's, and is
    /// refused, so no app gets the platform's write by declaring the verb its own.
    #[test]
    fn the_origins_write_lands_in_an_origin_kind_and_no_app_action_makes_it() {
        use tilo_resource_store::{ORIGIN_VERB, Refusal};
        let anyone = |_: &Proposal<'_>| true;
        // A store judging verbs, whose surface names the origin's verb its own: what an app
        // declaring it would publish. The kind an origin keeps is declared as its own.
        let mut origin_kind = record_type(ISSUE);
        origin_kind.origin = Some("imap.issue".to_owned());
        let mut there = Store::new(crate::WORKSPACE);
        there
            .declare(Schema {
                types: vec![origin_kind, record_type("gallery.Item")],
                verbs: vec![tilo_resource_store::Verb {
                    name: ORIGIN_VERB.to_owned(),
                    writes: BTreeMap::from([(
                        ISSUE.to_owned(),
                        tilo_resource_store::Writes::default(),
                    )]),
                    ..tilo_resource_store::Verb::default()
                }],
                ..Schema::default()
            })
            .unwrap();
        // What the origin says lands: a declared kind, its declared fields.
        let landed = issues()
            .put(
                ISSUE,
                "o1",
                resource("board", "o0", "Imported", "open"),
                &stamp(ORIGIN_VERB, "i1"),
            )
            .unwrap();
        assert_eq!(deliver(&landed, &mut there, &anyone), []);
        assert!(
            there.get(ISSUE, "o1", &|_: &str| true).is_some(),
            "the import is a record of the store"
        );
        // An ordinary app action names the action it is, in its entry: a change that does is no
        // origin's, whichever name it stamps, and the surface declaring the verb adds nothing.
        let mut app_action = stamp(ORIGIN_VERB, "i2");
        app_action.context = Some(Box::new(tilo_actions::ActionContext {
            what: Some(tilo_actions::Did::verb(
                tilo_actions::ActionKey::new("so.tilo.tasks", "tl.so.tilo.tasks.Task", ORIGIN_VERB),
                [],
            )),
            ..tilo_actions::ActionContext::default()
        }));
        let smuggled = issues()
            .put(
                ISSUE,
                "o2",
                resource("board", "o1", "Smuggled", "open"),
                &app_action,
            )
            .unwrap();
        assert!(matches!(
            deliver(&smuggled, &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("no action makes it")
        ));
        // A kind the store declares, that no origin keeps, is no origin's to write either.
        let not_origin = issues()
            .put(
                "gallery.Item",
                "o3",
                resource("board", "o2", "Typed", "open"),
                &stamp(ORIGIN_VERB, "i3"),
            )
            .unwrap();
        assert!(matches!(
            deliver(&not_origin, &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("no origin's kind")
        ));
        // A kind the store does not declare is no origin's to write.
        let unknown = issues()
            .put(
                "gallery.Unknown",
                "o4",
                resource("board", "o3", "Elsewhere", "open"),
                &stamp(ORIGIN_VERB, "i4"),
            )
            .unwrap();
        assert!(matches!(
            deliver(&unknown, &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("gallery.Unknown")
        ));
    }

    /// A record's founding is derived within the store's workspace: the same record's founding
    /// in another workspace, made alike by every holder there, is no founding of this one's, so
    /// two workspaces never hold one change.
    #[test]
    fn a_founding_derived_in_another_workspace_is_refused() {
        use collab_types::document::founding::{Scope, structure};
        use tilo_resource_store::{MOVE_VERB, Refusal, content_id};
        let schema = Schema::from_surface(&super::tracker_surface());
        let containers = schema.get("Comment").unwrap().founding.clone();
        let mut there = issues();
        there.declare(schema).unwrap();
        let anyone = |_: &Proposal<'_>| true;
        let empty = Resource {
            readers: MEMBERS.to_owned(),
            parent: None,
            order: None,
            fields: BTreeMap::new(),
        };
        let placed = issues()
            .put("Comment", "c1", empty, &stamp(MOVE_VERB, "i1"))
            .unwrap();
        send(&placed, &mut there);
        let founded_in = |workspace| {
            let scope = Scope {
                workspace,
                kind: "Comment",
                id: "c1",
            };
            vec![Written {
                doc: content_id(MEMBERS, "Comment", "c1"),
                readers: MEMBERS.to_owned(),
                change: structure(scope, &containers).unwrap(),
            }]
        };
        assert!(matches!(
            deliver(&founded_in("another-workspace"), &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("not the one its kind derives")
        ));
        assert_eq!(
            deliver(&founded_in(crate::WORKSPACE), &mut there, &anyone),
            []
        );
    }

    /// A record's step to a storage version names the store's migration verb, which no app
    /// declares: any member's copy of it is admitted only when it is the step its kind and id
    /// derive for that version, up to the version its type stands at. The app's rewrite of its
    /// content for a version names the same verb and is decided as any write of the record's, by
    /// who may write it. A write naming no verb stays refused.
    #[test]
    fn a_migration_is_the_derived_step_or_a_rewrite_by_who_may_write() {
        use automerge::transaction::Transactable as _;
        use tilo_resource_store::{MOVE_VERB, Refusal, content_id};
        let schema = Schema::from_surface(&super::tracker_surface());
        let containers = schema.get("Comment").unwrap().founding.clone();
        let comment = collab_types::document::founding::Scope {
            workspace: crate::WORKSPACE,
            kind: "Comment",
            id: "c1",
        };
        let mut there = issues();
        there.declare(schema).unwrap();
        let anyone = |_: &Proposal<'_>| true;
        let nobody = |_: &Proposal<'_>| false;
        let empty = Resource {
            readers: MEMBERS.to_owned(),
            parent: None,
            order: None,
            fields: BTreeMap::new(),
        };
        let placed = issues()
            .put("Comment", "c1", empty, &stamp(MOVE_VERB, "i1"))
            .unwrap();
        send(&placed, &mut there);
        let made = |doc: &mut automerge::AutoCommit, options| {
            let hash = doc.commit_with(options).unwrap();
            doc.get_change_by_hash(&hash).unwrap()
        };
        let content = |change: automerge::Change| Written {
            doc: content_id(MEMBERS, "Comment", "c1"),
            readers: MEMBERS.to_owned(),
            change,
        };
        let chain = collab_types::document::founding::chain(comment, 1, &containers).unwrap();
        let [founding, step] = <[automerge::Change; 2]>::try_from(chain.clone()).unwrap();
        assert_eq!(
            step.hash(),
            collab_types::document::founding::migrate(comment, 1, &containers)
                .unwrap()
                .hash()
        );
        assert_eq!(deliver(&[content(founding)], &mut there, &anyone), []);
        // The derived step is admitted, whoever sends it.
        assert_eq!(deliver(&[content(step.clone())], &mut there, &nobody), []);
        let chained: Vec<u8> = chain
            .iter()
            .flat_map(|change| change.raw_bytes().to_vec())
            .collect();
        let mut held = automerge::AutoCommit::load(&chained).unwrap();
        // A step of another making, though it names the verb and the version, is refused.
        let mut forged = held.fork();
        forged.put(automerge::ROOT, "text", "Forged").unwrap();
        let forged = made(
            &mut forged,
            automerge::transaction::CommitOptions::default()
                .with_message(step.message().unwrap().to_owned())
                .with_time(0),
        );
        assert!(matches!(
            deliver(&[content(forged)], &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("not the one its kind derives")
        ));
        // A step to a version the type does not stand at is refused.
        let beyond = collab_types::document::founding::migrate(comment, 2, &containers).unwrap();
        assert!(matches!(
            deliver(&[content(beyond)], &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("no storage version")
        ));
        // The app's rewrite for the version is decided by who may write the record.
        let rewrite = tilo_actions::ActionContext {
            change: tilo_actions::ChangeKind::Housekeeping,
            ..tilo_actions::ActionContext::for_resource("c1", "Comment", "Comment").with_described(
                tilo_actions::Described::new(tilo_actions::Did::history(
                    tilo_actions::HistoryOp::Migrate {
                        to: 1,
                        rewrite: true,
                    },
                    0,
                    None,
                )),
            )
        };
        let mut rewritten = held.fork();
        rewritten.put(automerge::ROOT, "text", "Ada").unwrap();
        let rewrite_change = made(
            &mut rewritten,
            automerge::transaction::CommitOptions::default()
                .with_message(rewrite.change_message().unwrap())
                .with_time(0),
        );
        assert_eq!(
            deliver(&[content(rewrite_change.clone())], &mut there, &nobody),
            [Refusal::NotPermitted]
        );
        assert_eq!(deliver(&[content(rewrite_change)], &mut there, &anyone), []);
        // A write of the record's content naming no verb stays refused.
        let mut unnamed = held.fork();
        unnamed.put(automerge::ROOT, "text", "Grace").unwrap();
        let unnamed = made(
            &mut unnamed,
            automerge::transaction::CommitOptions::default(),
        );
        assert!(matches!(
            deliver(&[content(unnamed)], &mut there, &anyone).as_slice(),
            [Refusal::Undeclared(why)] if why.contains("verb")
        ));
    }

    /// Two devices whose workspace knows a type at versions 1 and 2, the newer only adding a
    /// field: each writes the fields of its own version and keeps the other's, the newer
    /// device's step reaches the older's store as the one it derives itself, and both stores
    /// come out at the same heads.
    #[test]
    fn two_devices_on_different_type_versions_keep_one_records_fields_and_its_step() {
        use tilo_resource_store::{Expected, Field, content_id};
        // The type the helpers declare, at version 2: `due` alone is added.
        let mut two = record_type(ISSUE);
        for field in &mut two.fields {
            field.value = Expected::Any;
        }
        two.version = 2;
        two.fields.push(Field {
            name: "due".to_owned(),
            kind: FieldKind::Scalar,
            value: Expected::Any,
            optional: true,
        });
        let declares_two = |store: &mut Store| {
            store
                .declare(Schema {
                    types: vec![two.clone()],
                    ..Schema::default()
                })
                .expect("version 2 only adds `due`");
        };
        let mut older = issues();
        declares_two(&mut older);
        let mut newer = issues();
        declares_two(&mut newer);
        let anyone = |_: &Proposal<'_>| true;

        // The record both devices read.
        let seeded = put(&mut older, "a", resource("board", "a0", "Draft", "open"));
        send(&seeded, &mut newer);

        // Its content's chain: the founding and a step to each version, derived alike.
        let chain = older
            .founding(ISSUE, "a")
            .expect("the type is declared")
            .expect("a type with a chain");
        assert_eq!(chain.len(), 3, "the founding and a step to each version");
        let content = content_id(MEMBERS, ISSUE, "a");
        let carried = |change: automerge::Change| Written {
            doc: content.clone(),
            readers: MEMBERS.to_owned(),
            change,
        };
        for store in [&mut older, &mut newer] {
            for change in &chain {
                assert_eq!(deliver(&[carried(change.clone())], store, &anyone), []);
            }
        }
        let heads = |store: &Store| {
            store
                .document(&content)
                .map(automerge::Automerge::get_heads)
        };
        assert_eq!(heads(&older), heads(&newer), "the chain converges");

        // Each device writes the fields of its own version: both are kept.
        let from_older = put(&mut older, "a", resource("board", "a0", "Draft", "done"));
        let with_due = Resource {
            readers: MEMBERS.to_owned(),
            parent: Some("board".to_owned()),
            order: Some("a0".to_owned()),
            fields: BTreeMap::from([("due".to_owned(), Cell::One("2026-12-01".into()))]),
        };
        let from_newer = put(&mut newer, "a", with_due);
        send(&from_older, &mut newer);
        send(&from_newer, &mut older);
        let everyone = |_: &str| true;
        let found = |store: &Store, field: &str, value: &str| {
            listed(
                store,
                &Query::of(ISSUE, 10).filter(Filter::Is(field.to_owned(), value.into())),
                &everyone,
            )
        };
        for store in [&older, &newer] {
            assert_eq!(found(store, "state", "done"), ["a"]);
            assert_eq!(found(store, "due", "2026-12-01"), ["a"]);
        }
        let written = from_older[0].doc.clone();
        let heads = |store: &Store| {
            store
                .document(&written)
                .map(automerge::Automerge::get_heads)
        };
        assert_eq!(heads(&older), heads(&newer), "both stores converge");
    }

    /// A checkpoint, a description and a link are verbs every schema has: a change that writes
    /// nothing, by someone who may write the document, saying little. One that writes, says too
    /// much, comes from someone who may not write, or names a verb nobody declared is refused.
    #[test]
    fn a_history_verb_writes_nothing_and_says_little() {
        use tilo_resource_store::{MAX_MARK_ARGS, Refusal};
        let tracker = || {
            let mut store = issues();
            store
                .declare(Schema::from_surface(&super::tracker_surface()))
                .unwrap();
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
        assert_eq!(
            deliver(std::slice::from_ref(&checkpoint), &mut there, &anyone),
            []
        );
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
                comment,
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
                founding: Vec::new(),
                origin: None,
                version: 1,
            }],
            ..Schema::default()
        };
        let mut here = issues();
        let mut there = issues();
        there.declare(schema).unwrap();
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
        let mut here = Store::new(crate::WORKSPACE);
        let mut holders = [
            Store::new(crate::WORKSPACE),
            Store::new(crate::WORKSPACE),
            Store::new(crate::WORKSPACE),
        ];
        let made = put(&mut here, "a", resource("board", "a0", "Draft", "open"));
        for holder in &mut holders {
            holder.declare(schema.clone()).unwrap();
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
        use tilo_resource_store::{Verb, Writes};
        let ty = |name: &str| record_type(name);
        let verb = |name: &str, kinds: &[&str]| Verb {
            name: name.to_owned(),
            writes: kinds
                .iter()
                .map(|kind| ((*kind).to_owned(), Writes::default()))
                .collect(),
            nested: BTreeSet::new(),
            ..Verb::default()
        };
        let schema = Schema {
            kind: String::new(),
            types: vec![ty(ISSUE), ty("Comment")],
            verbs: vec![
                verb("reply", &["Comment"]),
                verb("convert", &[ISSUE, "Comment"]),
            ],
            readers: BTreeMap::new(),
        };
        let mut here = Store::new(crate::WORKSPACE);
        let mut holders = [
            Store::new(crate::WORKSPACE),
            Store::new(crate::WORKSPACE),
            Store::new(crate::WORKSPACE),
        ];
        let made = put(&mut here, "a", resource("board", "a0", "Draft", "open"));
        for holder in &mut holders {
            // The record's first write names no verb: it is taken by the types alone, and the
            // verbs judge what comes after it.
            holder
                .declare(Schema {
                    types: schema.types.clone(),
                    ..Schema::default()
                })
                .unwrap();
            send(&made, holder);
            holder.declare(schema.clone()).unwrap();
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
        let mut started =
            Store::with_source(crate::WORKSPACE, Box::new(move |_| Some(saved.clone())));
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

        let mut sourced =
            Store::with_source(crate::WORKSPACE, Box::new(move |_| Some(saved.clone())));
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
        let mut store = Store::new(crate::WORKSPACE);
        store
            .declare(Schema {
                readers: BTreeMap::from([(
                    REPORT.to_owned(),
                    ReaderRule::AuthorAnd {
                        role: Role::Moderator,
                    },
                )]),
                ..Schema::default()
            })
            .unwrap();
        let ben = Stamp {
            invoker: "ben".to_owned(),
            ..Stamp::default()
        };
        let put = |store: &mut Store, kind: &str, id: &str, resource: Resource| {
            tilo_resource_store::wire::write(
                store,
                collab_types::store::wire::Command::Put {
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

    /// A store that declares a mailbox every member reads and a draft its author alone reads
    /// until it is sent, the draft nested in the mailbox: what a workspace's store declares of a
    /// mail app's surfaces.
    fn mailboxes() -> Store {
        let mut store = mailboxes_unruled();
        store.declare(draft_rule()).unwrap();
        store
    }

    /// The draft's rule alone: a draft is read by its author until it is sent.
    fn draft_rule() -> Schema {
        use tilo_actions::surface::ReaderRule;
        Schema {
            readers: BTreeMap::from([(
                DRAFT.to_owned(),
                ReaderRule::Author {
                    until: Some("sent".to_owned()),
                },
            )]),
            ..Schema::default()
        }
    }

    /// [`mailboxes`] before the draft's type named its readers.
    fn mailboxes_unruled() -> Store {
        let field = |name: &str, kind: FieldKind| tilo_resource_store::Field {
            name: name.to_owned(),
            kind,
            value: tilo_resource_store::Expected::Any,
            optional: true,
        };
        let ty = |name: &str| tilo_resource_store::Type {
            name: name.to_owned(),
            fields: vec![
                field("subject", FieldKind::Scalar),
                field("sent", FieldKind::Scalar),
            ],
            nested: Vec::new(),
            live: None,
            founding: Vec::new(),
            origin: None,
            version: 1,
        };
        let mut store = Store::new(crate::WORKSPACE);
        store
            .declare(Schema {
                types: vec![ty(MAILBOX), ty(DRAFT)],
                ..Schema::default()
            })
            .unwrap();
        store
    }

    const MAILBOX: &str = "Mailbox";
    const DRAFT: &str = "Draft";

    fn draft(subject: &str, sent: bool) -> Resource {
        Resource {
            readers: String::new(),
            parent: Some("box".to_owned()),
            order: Some("a".to_owned()),
            fields: BTreeMap::from([
                ("subject".to_owned(), Cell::One(subject.into())),
                ("sent".to_owned(), Cell::One(Value::Bool(sent))),
            ]),
        }
    }

    /// What `reader` is answered of `command`, through the one read rule every host answers by.
    fn read_as(
        store: &Store,
        reader: &str,
        command: &collab_types::store::wire::Command,
    ) -> collab_types::store::wire::Reply {
        tilo_resource_store::wire::read(
            store,
            command,
            &tilo_resource_store::wire::reads_as(reader),
        )
        .expect("a read is answered")
    }

    /// Every way a member reads the store, of the draft `id` with `subject`: whether any of them
    /// tells `reader` the draft is there. A get, its content, a listing, a search of its subject,
    /// a count, the subject's values, its history, and the newest changes.
    fn sees_draft(store: &Store, reader: &str, id: &str, subject: &str) -> Vec<&'static str> {
        use collab_types::store::wire::{Command, Reply};
        let mut seen = Vec::new();
        let get = Command::Get {
            kind: DRAFT.to_owned(),
            id: id.to_owned(),
        };
        if matches!(read_as(store, reader, &get), Reply::Resource(Some(_))) {
            seen.push("get");
        }
        let listing = Command::Query(Query::of(DRAFT, 100));
        if let Reply::Page(page) = read_as(store, reader, &listing)
            && page.resources.iter().any(|(held, _)| held == id)
        {
            seen.push("listing");
        }
        let mut search = Query::of(DRAFT, 100);
        search
            .filters
            .push(Filter::Contains("subject".to_owned(), subject.to_owned()));
        if let Reply::Page(page) = read_as(store, reader, &Command::Query(search))
            && !page.resources.is_empty()
        {
            seen.push("search");
        }
        let count = Command::Count {
            kind: DRAFT.to_owned(),
            filters: vec![Filter::Contains("subject".to_owned(), subject.to_owned())],
        };
        if !matches!(read_as(store, reader, &count), Reply::Count(0)) {
            seen.push("count");
        }
        let values = Command::Values {
            kind: DRAFT.to_owned(),
            filters: Vec::new(),
            field: "subject".to_owned(),
        };
        if let Reply::Values(values) = read_as(store, reader, &values)
            && !values.is_empty()
        {
            seen.push("values");
        }
        let versions = Command::Versions {
            kind: DRAFT.to_owned(),
            id: id.to_owned(),
        };
        if let Reply::Versions(versions) = read_as(store, reader, &versions)
            && !versions.is_empty()
        {
            seen.push("history");
        }
        let newest = Command::Newest {
            within: id.to_owned(),
            limit: 100,
        };
        if let Reply::Newest { versions, .. } = read_as(store, reader, &newest)
            && versions.iter().any(|(_, held, _)| held == id)
        {
            seen.push("newest changes");
        }
        seen
    }

    /// A draft is its author's until it is sent: placed by its own rule though it is nested in a
    /// mailbox every member reads, so another member finds it by no read at all (a get, a
    /// listing, a search, a count, a field's values, its history or the newest changes), while
    /// its author finds it by every one. Once sent, every member reads it, and it is not taken
    /// back.
    #[test]
    fn a_draft_is_read_by_its_author_alone_until_it_is_sent() {
        use collab_types::store::wire::Command;
        let mut store = mailboxes();
        let as_ = |who: &str| Stamp {
            invoker: who.to_owned(),
            ..Stamp::default()
        };
        tilo_resource_store::wire::write(
            &mut store,
            Command::Put {
                kind: MAILBOX.to_owned(),
                id: "box".to_owned(),
                resource: Resource {
                    readers: MEMBERS.to_owned(),
                    parent: None,
                    order: None,
                    fields: BTreeMap::new(),
                },
                stamp: as_("ada"),
            },
        )
        .unwrap();
        let put = |store: &mut Store, subject: &str, sent: bool| {
            tilo_resource_store::wire::write(
                store,
                Command::Put {
                    kind: DRAFT.to_owned(),
                    id: "d1".to_owned(),
                    resource: draft(subject, sent),
                    stamp: as_("ada"),
                },
            )
        };
        let written = put(&mut store, "Quarterly plan", false).unwrap();
        assert!(
            written.iter().all(|write| write.readers == "ada"),
            "the draft is sealed to its author, not to the mailbox's readers: {:?}",
            written
                .iter()
                .map(|write| &write.readers)
                .collect::<Vec<_>>()
        );
        let every = vec![
            "get",
            "listing",
            "search",
            "count",
            "values",
            "history",
            "newest changes",
        ];
        assert_eq!(
            sees_draft(&store, "bo", "d1", "Quarterly"),
            Vec::<&str>::new(),
            "another member reads nothing of an unsent draft"
        );
        assert_eq!(
            sees_draft(&store, "ada", "d1", "Quarterly"),
            every,
            "its author reads it every way"
        );

        put(&mut store, "Quarterly plan", true).unwrap();
        assert_eq!(
            sees_draft(&store, "bo", "d1", "Quarterly"),
            every,
            "every member reads a sent draft"
        );
        assert_eq!(sees_draft(&store, "ada", "d1", "Quarterly"), every);
        assert!(
            put(&mut store, "Quarterly plan", false).is_err(),
            "a sent draft is not taken back from the members who read it"
        );
    }

    /// A draft written where every member reads it, before its type named its readers, is read
    /// as the rule says once the store knows it: by its author alone while it is unsent, by no
    /// read at all for another member, and its history stays its author's once it is removed.
    #[test]
    fn a_draft_held_where_everyone_reads_is_read_by_its_rule() {
        use collab_types::store::wire::{Command, Reply};
        let mut store = mailboxes_unruled();
        let ada = Stamp {
            invoker: "ada".to_owned(),
            ..Stamp::default()
        };
        for (kind, id, resource) in [
            (
                MAILBOX,
                "box",
                Resource {
                    readers: MEMBERS.to_owned(),
                    parent: None,
                    order: None,
                    fields: BTreeMap::new(),
                },
            ),
            (DRAFT, "d1", draft("Quarterly plan", false)),
        ] {
            tilo_resource_store::wire::write(
                &mut store,
                Command::Put {
                    kind: kind.to_owned(),
                    id: id.to_owned(),
                    resource,
                    stamp: ada.clone(),
                },
            )
            .unwrap();
        }
        assert_eq!(
            store.readers_of(DRAFT, "d1").as_deref(),
            Some(MEMBERS),
            "with no rule, the draft is where its mailbox is"
        );
        store.declare(draft_rule()).unwrap();
        assert!(
            store.misplaced(DRAFT, "d1"),
            "the draft is held where its rule does not place it"
        );
        assert_eq!(
            sees_draft(&store, "bo", "d1", "Quarterly"),
            Vec::<&str>::new(),
            "another member reads nothing of it, though its document is every member's"
        );
        assert_eq!(
            sees_draft(&store, "ada", "d1", "Quarterly"),
            vec![
                "get",
                "listing",
                "search",
                "count",
                "values",
                "history",
                "newest changes"
            ],
            "its author reads it every way"
        );
        tilo_resource_store::wire::write(
            &mut store,
            Command::Remove {
                kind: DRAFT.to_owned(),
                id: "d1".to_owned(),
                stamp: ada,
            },
        )
        .unwrap();
        let removed = Command::Removed {
            kind: DRAFT.to_owned(),
            parent: Some("box".to_owned()),
        };
        assert!(
            matches!(read_as(&store, "bo", &removed), Reply::Ids(ids) if ids.is_empty()),
            "another member is not told it was removed"
        );
        assert_eq!(
            sees_draft(&store, "bo", "d1", "Quarterly"),
            Vec::<&str>::new(),
            "nor reads its history once it is gone"
        );
        assert!(
            sees_draft(&store, "ada", "d1", "Quarterly").contains(&"history"),
            "its author still reads its history"
        );
    }
}

/// An app's records, as a window that runs with no workspace keeps and reads them: through the
/// record commands a workspace answers.
mod records {
    use automerge::transaction::Transactable as _;
    use collab_types::store::wire::{Bytes, Command, RECORD_PAGE_BYTES, Reply};
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
    use collab_types::store::Resource;
    use collab_types::store::wire::{Command, Reply};
    use tilo_resource_store::{LocalPort, Port, Stamp};

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

mod outside_effects {
    //! Admitting a queued outside effect: the app whose package wrote it declares the capability it
    //! carries out, and a type whose reader rule keeps its parent's readers is still nested only
    //! where its parent's type nests it.
    use std::collections::BTreeMap;

    use collab_types::store::{Cell, Resource};
    use tilo_actions::surface::ReaderRule;
    use tilo_resource_store::{
        Expected, Field, FieldKind, Proposal, Received, Refusal, Schema, Stamp, Store, Type, Verb,
        Writes, Written,
    };

    const ITEM: &str = "tl.so.example.Item";
    const EFFECT: &str = "tilolang.effect";

    fn text(name: &str) -> Field {
        Field {
            name: name.to_owned(),
            kind: FieldKind::Scalar,
            value: Expected::Text,
            optional: true,
        }
    }

    fn flag(name: &str) -> Field {
        Field {
            name: name.to_owned(),
            kind: FieldKind::Scalar,
            value: Expected::Bool,
            optional: true,
        }
    }

    /// The schema of an app that queues outside effects on its items: its surface declares the entry
    /// a send waits in, as every app's does, read by its author alone.
    fn schema(outside: &[(&str, &[&str])]) -> Schema {
        Schema {
            kind: String::new(),
            types: vec![
                Type {
                    name: ITEM.to_owned(),
                    fields: vec![text("title")],
                    nested: Vec::new(),
                    live: None,
                    founding: Vec::new(),
                    origin: None,
                    version: 1,
                },
                Type {
                    name: EFFECT.to_owned(),
                    fields: vec![
                        text("doc"),
                        text("entry"),
                        text("app"),
                        text("capability"),
                        text("payload"),
                        text("owner"),
                        text("state"),
                        text("detail"),
                        text("outcome"),
                        text("reason"),
                        text("held_until"),
                        flag("settled"),
                    ],
                    nested: Vec::new(),
                    live: None,
                    founding: Vec::new(),
                    origin: None,
                    version: 1,
                },
            ],
            verbs: vec![Verb {
                name: "send".to_owned(),
                writes: BTreeMap::from([(ITEM.to_owned(), Writes::default())]),
                outside: outside
                    .iter()
                    .map(|(surface, capabilities)| {
                        (
                            (*surface).to_owned(),
                            capabilities.iter().map(|c| (*c).to_owned()).collect(),
                        )
                    })
                    .collect(),
                ..Verb::default()
            }],
            readers: BTreeMap::from([(EFFECT.to_owned(), ReaderRule::Author { until: None })]),
        }
    }

    fn store(schema: Schema) -> Store {
        let mut store = Store::new(crate::WORKSPACE);
        store.declare(schema).unwrap();
        store
    }

    fn stamp(verb: &str, invocation: &str) -> Stamp {
        Stamp {
            invoked: String::new(),
            verb: verb.to_owned(),
            invoker: "ada".to_owned(),
            invocation: invocation.to_owned(),
            membership: vec!["m1".to_owned()],
            ..Stamp::default()
        }
    }

    fn item(store: &mut Store) -> Vec<Written> {
        store
            .put(
                ITEM,
                "i",
                Resource {
                    readers: String::new(),
                    parent: None,
                    order: None,
                    fields: BTreeMap::from([("title".to_owned(), Cell::One("Hi".into()))]),
                },
                &stamp("send", "i1"),
            )
            .unwrap()
    }

    fn entry(store: &mut Store, app: &str, capability: &str) -> Vec<Written> {
        store
            .put(
                EFFECT,
                "e",
                Resource {
                    readers: "ada".to_owned(),
                    parent: Some("i".to_owned()),
                    order: None,
                    fields: BTreeMap::from([
                        ("doc".to_owned(), Cell::One("d".into())),
                        ("entry".to_owned(), Cell::One("e".into())),
                        ("app".to_owned(), Cell::One(app.into())),
                        ("capability".to_owned(), Cell::One(capability.into())),
                        ("payload".to_owned(), Cell::One("{}".into())),
                        ("owner".to_owned(), Cell::One("dev".into())),
                        ("settled".to_owned(), Cell::One(false.into())),
                    ]),
                },
                &stamp("send", "i2"),
            )
            .unwrap()
    }

    /// Carries `written` to `to`, as sync does, answered by why each refused one was refused.
    fn deliver(written: &[Written], to: &mut Store) -> Vec<Refusal> {
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
                    &|_: &Proposal<'_>| true,
                )
                .unwrap()
                .into_iter()
                .map(|(_, why)| why),
            );
        }
        refused
    }

    fn refused(written: &[Written], to: &mut Store) -> String {
        let refused = deliver(written, to);
        let [Refusal::Undeclared(why)] = &refused[..] else {
            panic!("the write is refused as undeclared: {refused:?}");
        };
        why.clone()
    }

    /// An app queues a send its surface does not declare: admission refuses it, so no app queues a
    /// capability it was never granted.
    #[test]
    fn an_undeclared_capability_is_refused_when_it_is_queued() {
        let declared = schema(&[("tl.so.example", &["imap.check"])]);
        let mut writer = store(declared.clone());
        let mut there = store(declared);
        deliver(&item(&mut writer), &mut there);
        let why = refused(&entry(&mut writer, "so.example", "smtp.send"), &mut there);
        assert!(
            why.contains("declares no outside capability `smtp.send`"),
            "{why}"
        );
    }

    /// An app declares the capability it queues: the entry is admitted, naming the app whose package
    /// wrote it.
    #[test]
    fn a_declared_capability_is_admitted_for_the_app_that_declared_it() {
        let declared = schema(&[("tl.so.example", &["smtp.send"])]);
        let mut writer = store(declared.clone());
        let mut there = store(declared);
        deliver(&item(&mut writer), &mut there);
        let admitted = deliver(&entry(&mut writer, "so.example", "smtp.send"), &mut there);
        assert!(
            admitted.is_empty(),
            "the app declares the capability it queues: {admitted:?}"
        );
    }

    /// An entry claiming another app queued it is refused: the app an entry is for is the one whose
    /// package wrote it, never what the entry says of itself.
    #[test]
    fn an_entry_claiming_another_app_queued_it_is_refused() {
        let declared = schema(&[("tl.so.example", &["smtp.send"])]);
        let mut writer = store(declared.clone());
        let mut there = store(declared);
        deliver(&item(&mut writer), &mut there);
        let why = refused(&entry(&mut writer, "so.tilo.mail", "smtp.send"), &mut there);
        assert!(why.contains("whose package wrote it not"), "{why}");
    }

    /// A type whose reader rule keeps its parent's readers (`author until ready`, once it is ready)
    /// is nested only where its parent's type nests it: the rule's own document does not place it
    /// elsewhere any more.
    #[test]
    fn a_ruled_kind_that_keeps_its_parents_readers_is_nested_where_its_parent_nests_it() {
        let schema = || Schema {
            kind: String::new(),
            types: vec![
                Type {
                    name: "Holder".to_owned(),
                    fields: Vec::new(),
                    nested: Vec::new(),
                    live: None,
                    founding: Vec::new(),
                    origin: None,
                    version: 1,
                },
                Type {
                    name: "Ruled".to_owned(),
                    fields: vec![flag("ready")],
                    nested: Vec::new(),
                    live: None,
                    founding: Vec::new(),
                    origin: None,
                    version: 1,
                },
            ],
            verbs: Vec::new(),
            readers: BTreeMap::from([(
                "Ruled".to_owned(),
                ReaderRule::Author {
                    until: Some("ready".to_owned()),
                },
            )]),
        };
        let held = |ready: bool| {
            let mut writer = store(schema());
            let mut there = store(schema());
            let holder = writer
                .put("Holder", "h", Resource::default(), &stamp("create", "i1"))
                .unwrap();
            deliver(&holder, &mut there);
            let ruled = writer
                .put(
                    "Ruled",
                    "r",
                    Resource {
                        // Its author alone reads it until `ready`, then every member, as its parent
                        // is read: what its rule gives, which the writer names as a store placing it
                        // by the rule does.
                        readers: if ready {
                            "*".to_owned()
                        } else {
                            "ada".to_owned()
                        },
                        parent: Some("h".to_owned()),
                        fields: BTreeMap::from([("ready".to_owned(), Cell::One(ready.into()))]),
                        ..Resource::default()
                    },
                    &stamp("create", "i2"),
                )
                .unwrap();
            deliver(&ruled, &mut there)
        };
        let refused = held(true);
        let [Refusal::Undeclared(why)] = &refused[..] else {
            panic!("the write is refused as undeclared: {refused:?}");
        };
        assert!(why.contains("nests no `Ruled`"), "{why}");
        // Its author alone reads it until then: a document of its own, not its parent's.
        let alone = held(false);
        assert!(
            alone.is_empty(),
            "its author alone reads it, in a document of its own: {alone:?}"
        );
    }

    /// The entry `store` wrote as `entry(...)`, written again with the fields a settlement adds:
    /// the effect taken back before it went.
    fn settled(store: &mut Store, outcome: &str) -> Vec<Written> {
        store
            .put(
                EFFECT,
                "e",
                Resource {
                    readers: "ada".to_owned(),
                    parent: Some("i".to_owned()),
                    order: None,
                    fields: BTreeMap::from([
                        ("doc".to_owned(), Cell::One("d".into())),
                        ("entry".to_owned(), Cell::One("e".into())),
                        ("app".to_owned(), Cell::One("so.example".into())),
                        ("capability".to_owned(), Cell::One("smtp.send".into())),
                        ("payload".to_owned(), Cell::One("{}".into())),
                        ("owner".to_owned(), Cell::One("dev".into())),
                        ("settled".to_owned(), Cell::One(true.into())),
                        ("outcome".to_owned(), Cell::One(outcome.into())),
                        (
                            "reason".to_owned(),
                            Cell::One("Taken back before it went".into()),
                        ),
                    ]),
                },
                &stamp("send", "i3"),
            )
            .unwrap()
    }

    /// Taking back the effect it queued is the action's own change: the entry settles as
    /// withdrawn, and admission takes it. An action says that and nothing else of how an effect
    /// went.
    #[test]
    fn an_action_takes_back_the_effect_it_queued() {
        let declared = schema(&[("tl.so.example", &["smtp.send"])]);
        let mut writer = store(declared.clone());
        let mut there = store(declared);
        deliver(&item(&mut writer), &mut there);
        deliver(&entry(&mut writer, "so.example", "smtp.send"), &mut there);
        let taken_back = deliver(&settled(&mut writer, "withdrawn"), &mut there);
        assert!(
            taken_back.is_empty(),
            "taking the effect back is admitted: {taken_back:?}"
        );
    }

    /// An action saying its effect went — anything but that it was taken back — is refused: only
    /// the device that carried it out says how it went.
    #[test]
    fn an_action_says_no_other_word_of_how_its_effect_went() {
        let declared = schema(&[("tl.so.example", &["smtp.send"])]);
        let mut writer = store(declared.clone());
        let mut there = store(declared);
        deliver(&item(&mut writer), &mut there);
        deliver(&entry(&mut writer, "so.example", "smtp.send"), &mut there);
        let why = refused(&settled(&mut writer, "sent"), &mut there);
        assert!(why.contains("may not change a queued effect"), "{why}");
    }
}

/// The content a resource's fields name is found by its address, as every commit leaves it: a
/// put names it, an edit that drops the reference and a removal take it back, a move to other
/// readers files it under them, and a document the store forgets names nothing.
#[test]
fn the_content_a_resource_names_is_found_by_its_address_as_commits_leave_it() {
    const ATTACHED: &str = "Attached";
    let (sent, other) = ("a1".repeat(32), "b2".repeat(32));
    let attached = |id: &str, field: &str, value: String| {
        let mut fields = BTreeMap::new();
        fields.insert(field.to_owned(), Cell::One(Value::Text(value)));
        Change::Put {
            kind: ATTACHED.to_owned(),
            id: id.to_owned(),
            parent: Some("message".to_owned()),
            order: None,
            fields,
        }
    };
    let naming = |index: &Index, address: &str| -> Vec<(String, String, String)> {
        let mut found: Vec<_> = index
            .naming_content(address)
            .map(|(kind, id, readers)| (kind.to_owned(), id.to_owned(), readers.to_owned()))
            .collect();
        found.sort();
        found
    };
    let named = |id: &str, readers: &str| (ATTACHED.to_owned(), id.to_owned(), readers.to_owned());
    let mut index = Index::new();
    let put = |index: &mut Index, doc: &str, readers: &str, changes: &[Change]| {
        index.committed(&Commit {
            doc,
            readers,
            heads: &[],
            changes,
        });
    };

    // A link and an address name the content; its key, upper case and a text that only looks
    // like one do not change what they name, or name nothing.
    put(
        &mut index,
        "doc-a",
        "everyone",
        &[
            attached("by-link", "blob", format!("tilo-blob://{sent}")),
            attached(
                "by-address",
                "file",
                format!("tilo.content:{}", sent.to_uppercase()),
            ),
            attached("just-text", "note", sent.clone()),
        ],
    );
    assert_eq!(
        naming(&index, &sent),
        vec![
            named("by-address", "everyone"),
            named("by-link", "everyone")
        ],
    );
    assert!(
        naming(&index, &other).is_empty(),
        "nothing names other content"
    );

    // An edit that names other content drops the first.
    put(
        &mut index,
        "doc-a",
        "everyone",
        &[attached("by-link", "blob", format!("tilo-blob://{other}"))],
    );
    assert_eq!(naming(&index, &sent), vec![named("by-address", "everyone")]);
    assert_eq!(naming(&index, &other), vec![named("by-link", "everyone")]);

    // Moved to a document of other readers, it is filed under them.
    put(
        &mut index,
        "doc-b",
        "ada",
        &[attached(
            "by-address",
            "file",
            format!("tilo.content:{sent}"),
        )],
    );
    assert_eq!(naming(&index, &sent), vec![named("by-address", "ada")]);

    // Removed, nothing names it.
    put(
        &mut index,
        "doc-b",
        "ada",
        &[Change::Remove {
            kind: ATTACHED.to_owned(),
            id: "by-address".to_owned(),
        }],
    );
    assert!(
        naming(&index, &sent).is_empty(),
        "the removed resource names nothing"
    );

    // A document the store forgets names nothing it held.
    index.forget("doc-a");
    assert!(
        naming(&index, &other).is_empty(),
        "a forgotten document names nothing"
    );
}
