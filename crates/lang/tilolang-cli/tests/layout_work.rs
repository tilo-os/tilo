//! What a layout costs, in the toolkit's own units rather than in time: a check lays every view
//! of an app out from every scenario, at every size, display scale and text scale, and what it
//! spends per layout is what makes a run fast or slow. Wall time says nothing on a machine other
//! work shares, so this holds a layout to the frames the toolkit lays out, the layout nodes it
//! builds and the text it shapes outside the cache — counters that the same sources give the
//! same numbers on any machine, in any build profile.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use std::path::Path;

use tilolang_cli::layout_check::{self, Options};

const FRAMES_PER_LAYOUT: u64 = 30;

/// The layout nodes one layout may build: every node of the tree the frame rebuilds, 142 of them
/// for a board of lanes and cards.
const NODES_PER_LAYOUT: u64 = 180;

/// The text shaped outside the renderer's cache, per layout: these strings are the same on every
/// layout, so the cache holds all of them after the first, and only a stage's own words are shaped
/// again: 10 a layout for the board.
const SHAPED_PER_LAYOUT: u64 = 14;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn board() -> layout_check::Report {
    layout_check::check(&Options {
        dirs: vec![manifest().join("tests/layout_check/board")],
        ..Options::default()
    })
    .expect("the fixture lays out")
}

#[test]
fn a_layout_costs_no_more_work_than_it_did() {
    let report = board();
    let layouts = u64::try_from(report.layouts).expect("layouts fit");
    assert!(
        layouts >= 40,
        "every layout of the app runs, not {layouts} of them"
    );
    let per_layout = |count: u64| count / layouts;
    let work = report.work;
    assert!(
        per_layout(work.frames) <= FRAMES_PER_LAYOUT,
        "a layout costs at most {FRAMES_PER_LAYOUT} frames, not {} ({} frames over {layouts} layouts)",
        per_layout(work.frames),
        work.frames
    );
    assert!(
        per_layout(work.nodes) <= NODES_PER_LAYOUT,
        "a layout builds at most {NODES_PER_LAYOUT} layout nodes, not {} ({} nodes over {layouts} layouts)",
        per_layout(work.nodes),
        work.nodes
    );
    assert!(
        per_layout(work.shaped) <= SHAPED_PER_LAYOUT,
        "a layout shapes at most {SHAPED_PER_LAYOUT} runs outside the cache, not {} ({} over {layouts} layouts)",
        per_layout(work.shaped),
        work.shaped
    );
}
