//! How fast the Tasks app writes into a store kept on disk, and whether that holds as the
//! workspace grows: issues written a thousand a change up to each size, then single issues written
//! one change each, as a person adding them one by one does.
//!
//! `cargo run --release -p tilolang-tilo --example write_rate -- 1000 10000 50000`, or through
//! any cargo drop-in (`$CARGO`).

#![allow(
    clippy::print_stdout,
    clippy::unwrap_used,
    reason = "a measurement printed to its runner"
)]

use std::rc::Rc;
use std::time::Instant;

use tilolang_runtime::origin::OriginWrite;
use tilolang_runtime::value::Value;
use tilolang_runtime::{Actor, ids::MemberId};
use tilolang_tilo::host::Host;
use tilolang_tilo::resources::{LocalPort, Port, ResourceStore};

const APP: &str = "so.tilo.lang.tasks";
const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");
const DISCUSSION_IDS: &str = include_str!("../../../../apps/tl/discussion/app.ids");
const SOURCE: &str = include_str!("../../../../apps/tl/tasks/app.tl");
const IDS: &str = include_str!("../../../../apps/tl/tasks/app.ids");
const SINGLES: usize = 200;

fn main() {
    let sizes: Vec<usize> = std::env::args()
        .skip(1)
        .filter_map(|size| size.parse().ok())
        .collect();
    let sizes = if sizes.is_empty() {
        vec![1_000, 10_000]
    } else {
        sizes
    };
    let dir = tempfile::tempdir().unwrap();
    let host = open(dir.path());
    let mut held = 0;
    for size in sizes {
        let started = Instant::now();
        let written =
            tilolang_tilo::seed::seed_more(&host, APP, held, size, "2026-11-20T00:00:00Z").unwrap();
        let batched = started.elapsed();
        held = held.saturating_add(written);
        let started = Instant::now();
        for i in 0..SINGLES {
            single(&host, held.saturating_add(i));
        }
        let singles = started.elapsed();
        held = held.saturating_add(SINGLES);
        let reopened = Instant::now();
        drop(open(dir.path()));
        println!(
            "{held:>7} issues: {:>7.0} issues/s in batches, {:>6.0} single writes/s, reopened in {:>6.0} ms",
            f64::from(u32::try_from(written).unwrap_or(u32::MAX)) / batched.as_secs_f64().max(1e-9),
            f64::from(u32::try_from(SINGLES).unwrap_or(u32::MAX)) / singles.as_secs_f64().max(1e-9),
            reopened.elapsed().as_secs_f64() * 1000.0,
        );
    }
}

fn open(dir: &std::path::Path) -> Host {
    let port = LocalPort::at(dir.join("resources")).unwrap();
    let store = ResourceStore::new(Rc::new(port) as Rc<dyn Port>);
    let host = Host::open(store, Actor::from(MemberId::person("ada")), None, || None).unwrap();
    host.load_builtin(DISCUSSION, DISCUSSION_IDS).unwrap();
    host.load_builtin(SOURCE, IDS).unwrap();
    host
}

/// One issue written as a change of its own.
fn single(host: &Host, i: usize) {
    let (root, field, ty, title) = tilolang_tilo::seed::collection(host, APP).unwrap();
    host.runtime()
        .apply_origin(
            "seed",
            vec![OriginWrite::Insert {
                owner: root,
                field,
                ty,
                fields: vec![(title, Value::Text(format!("Issue {i}")))],
            }],
            Some("2026-11-20T00:00:00Z".to_owned()),
        )
        .unwrap();
}
