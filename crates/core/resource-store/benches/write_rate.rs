//! What a write and a page cost as a kind grows: resources committed per second into an index
//! already holding `n`, the time of a filtered page over it, of a page in the middle of it and of
//! finding a resource's row, at 1k and 100k resources. All must stay flat in `n`.
//!
//! Measure with:
//!
//!     cargo bench -p tilo-resource-store --bench write_rate

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::print_stdout,
    reason = "a measurement prints what it counted"
)]
#![expect(
    clippy::expect_used,
    clippy::unreachable,
    reason = "a benchmark fixture fails rather than measure a half-written store"
)]

use std::collections::BTreeMap;
use std::time::Instant;

use collab_types::store::{Cell, Filter, Order, Query, Resource};
use tilo_resource_store::{
    Change, Commit, CommitHook as _, Index, Proposal, Received, Stamp, Store,
};

const KIND: &str = "Issue";
/// The workspace every measured store keeps the resources of.
const WORKSPACE: &str = "bench";
/// Resources each measured commit writes, one per commit as a person's edits are.
const MEASURED: usize = 10_000;

fn issue(n: usize) -> Change {
    let mut fields = BTreeMap::new();
    fields.insert("title".to_owned(), Cell::One(format!("Issue {n}").into()));
    fields.insert(
        "state".to_owned(),
        Cell::One(
            match n % 3 {
                0 => "open",
                1 => "doing",
                _ => "done",
            }
            .into(),
        ),
    );
    fields.insert(
        "points".to_owned(),
        Cell::One(i64::try_from(n % 13).unwrap_or(0).into()),
    );
    Change::Put {
        kind: KIND.to_owned(),
        id: format!("issue-{n:08}"),
        parent: Some(format!("board-{}", n % 10)),
        order: Some(format!("{n:010}")),
        fields,
    }
}

/// The resource `n` as a write to the store gives it.
fn resource(n: usize) -> (String, Resource) {
    let Change::Put {
        id,
        parent,
        order,
        fields,
        ..
    } = issue(n)
    else {
        unreachable!("an issue is a put")
    };
    (
        id,
        Resource {
            readers: "members".to_owned(),
            parent,
            order,
            fields,
        },
    )
}

/// A kept document: its id, its bytes, the index's snapshot of it and the heads it was taken at.
type Kept = (String, Vec<u8>, Vec<u8>, Vec<automerge::ChangeHash>);

/// Resources per second written through the store into its documents, and received by a peer,
/// once it holds `size`; then how long a store holding all of them takes to start, reading
/// every document, and from the index's snapshots of them.
fn store(size: usize) {
    const WRITES: usize = 2_000;
    let mut store = Store::new(WORKSPACE);
    let mut docs = std::collections::BTreeSet::new();
    for n in 0..size {
        let (id, resource) = resource(n);
        for written in store
            .put(KIND, &id, resource, &Stamp::default())
            .expect("a write")
        {
            docs.insert(written.doc);
        }
    }
    let mut written = Vec::with_capacity(WRITES);
    let start = Instant::now();
    for n in size..size + WRITES {
        let (id, resource) = resource(n);
        written.extend(
            store
                .put(KIND, &id, resource, &Stamp::default())
                .expect("a write"),
        );
    }
    let writes = WRITES as f64 / start.elapsed().as_secs_f64();

    let mut peer = Store::new(WORKSPACE);
    let start = Instant::now();
    for write in written {
        docs.insert(write.doc.clone());
        let admitted = peer
            .receive(
                &write.doc,
                &write.readers,
                vec![Received {
                    change: write.change,
                    author: [0; 32],
                    claimed: None,
                }],
                &|_: &Proposal<'_>| true,
            )
            .expect("a peer's change");
        assert!(admitted.is_empty(), "every change admitted");
    }
    let received = WRITES as f64 / start.elapsed().as_secs_f64();

    let kept: Vec<Kept> = docs
        .iter()
        .map(|doc| {
            let bytes = store.document(doc).expect("a document").save();
            let snapshot = store.snapshot(doc).expect("a snapshot");
            let heads = store.index().heads(doc).expect("heads").to_vec();
            (doc.clone(), bytes, snapshot, heads)
        })
        .collect();
    let start = Instant::now();
    let mut cold = Store::new(WORKSPACE);
    for (doc, bytes, _, _) in &kept {
        cold.load(doc, bytes).expect("a kept document");
    }
    let cold_ms = start.elapsed().as_secs_f64() * 1e3;
    let start = Instant::now();
    let mut warm = Store::new(WORKSPACE);
    for (doc, _, snapshot, heads) in &kept {
        assert!(
            warm.restore(doc, snapshot, heads),
            "a snapshot at its heads"
        );
    }
    let warm_ms = start.elapsed().as_secs_f64() * 1e3;
    let all = |_: &str| true;
    assert_eq!(cold.count(KIND, &[], &all), warm.count(KIND, &[], &all));
    println!(
        "resources={size} store_writes_per_s={writes:.0} store_received_per_s={received:.0} start_reading_documents_ms={cold_ms:.0} start_from_snapshots_ms={warm_ms:.0} documents={}",
        kept.len()
    );
}

fn main() {
    for size in [1_000, 100_000] {
        let mut index = Index::new();
        let seed: Vec<Change> = (0..size).map(issue).collect();
        index.committed(&Commit {
            doc: "doc",
            readers: "members",
            heads: &[],
            changes: &seed,
        });

        let writes: Vec<Change> = (size..size + MEASURED).map(issue).collect();
        let start = Instant::now();
        for change in &writes {
            index.committed(&Commit {
                doc: "doc",
                readers: "members",
                heads: &[],
                changes: std::slice::from_ref(change),
            });
        }
        let per_second = MEASURED as f64 / start.elapsed().as_secs_f64();

        let page = Query::of(KIND, 50)
            .filter(Filter::Is("state".into(), "doing".into()))
            .order(Order::by("points").descending());
        let start = Instant::now();
        let mut shown = 0;
        for _ in 0..100 {
            shown += index.query(&page, &|_| true).resources.len();
        }
        let page_us = start.elapsed().as_secs_f64() * 1e6 / 100.0;
        let start = Instant::now();
        let first = index.query(&Query::of(KIND, 50), &|_| true);
        let unfiltered_us = start.elapsed().as_secs_f64() * 1e6;

        // A window scrolled to the middle of the kind, and of one state's resources, and the
        // row of a resource found there.
        let all = |_: &str| true;
        let middle = Query::of(KIND, 50);
        let doing = Query::of(KIND, 50).filter(Filter::Is("state".into(), "doing".into()));
        let start = Instant::now();
        for _ in 0..100 {
            shown += index.at(&middle, size / 2, &all).resources.len();
            shown += index.at(&doing, size / 6, &all).resources.len();
        }
        let at_us = start.elapsed().as_secs_f64() * 1e6 / 200.0;
        let target = format!("issue-{:08}", size / 2 + 1);
        let start = Instant::now();
        for _ in 0..100 {
            shown += index.position(&doing, &target, &all).unwrap_or(0);
        }
        let position_us = start.elapsed().as_secs_f64() * 1e6 / 100.0;

        println!(
            "resources={size} writes_per_s={per_second:.0} filtered_page_us={page_us:.0} first_page_us={unfiltered_us:.0} middle_page_us={at_us:.1} position_us={position_us:.1} shown={shown} first={}",
            first.resources.len()
        );
        store(size);
    }
}
