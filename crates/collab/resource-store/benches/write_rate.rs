//! What a write and a page cost as a kind grows: resources committed per second into an index
//! already holding `n`, and the time of a filtered page over it, at 1k and 100k resources. Both
//! must stay flat in `n`.
//!
//! Measure with:
//!
//!     cargo bench -p tilo-resources --bench write_rate

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

use tilo_resource_store::{
    Cell, Change, Commit, CommitHook as _, Filter, Index, Order, Query, Resource, Store,
};

const KIND: &str = "Issue";
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
            parent,
            order,
            fields,
        },
    )
}

/// Resources per second written through the store into its documents, and received by a peer,
/// once it holds `size`.
fn store(size: usize) {
    const WRITES: usize = 2_000;
    let mut store = Store::new();
    for n in 0..size {
        let (id, resource) = resource(n);
        store.put(KIND, &id, resource).expect("a write");
    }
    let mut written = Vec::with_capacity(WRITES);
    let start = Instant::now();
    for n in size..size + WRITES {
        let (id, resource) = resource(n);
        written.push(store.put(KIND, &id, resource).expect("a write"));
    }
    let writes = WRITES as f64 / start.elapsed().as_secs_f64();

    let mut peer = Store::new();
    let start = Instant::now();
    for write in written {
        peer.receive(&write.doc, vec![write.change])
            .expect("a peer's change");
    }
    let received = WRITES as f64 / start.elapsed().as_secs_f64();
    println!("resources={size} store_writes_per_s={writes:.0} store_received_per_s={received:.0}");
}

fn main() {
    for size in [1_000, 100_000] {
        let mut index = Index::new();
        let seed: Vec<Change> = (0..size).map(issue).collect();
        index.committed(&Commit {
            doc: "doc",
            heads: &[],
            changes: &seed,
        });

        let writes: Vec<Change> = (size..size + MEASURED).map(issue).collect();
        let start = Instant::now();
        for change in &writes {
            index.committed(&Commit {
                doc: "doc",
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
            shown += index.query(&page).resources.len();
        }
        let page_us = start.elapsed().as_secs_f64() * 1e6 / 100.0;
        let start = Instant::now();
        let first = index.query(&Query::of(KIND, 50));
        let unfiltered_us = start.elapsed().as_secs_f64() * 1e6;

        println!(
            "resources={size} writes_per_s={per_second:.0} filtered_page_us={page_us:.0} first_page_us={unfiltered_us:.0} shown={shown} first={}",
            first.resources.len()
        );
        store(size);
    }
}
