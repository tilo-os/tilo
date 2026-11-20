//! What one issue made through `create-many` costs, and that it costs the
//! same whether the tracker holds four hundred of them or ten thousand.
//!
//! An issue is its fields and its description, kept in an entry of the
//! tracker's part, written as one step. What that takes is counted two ways
//! that do not depend on the machine: the operations the documents gain per
//! issue, and the splices into automerge's op set each store makes, which is
//! what writing costs — each splice is a seek and a rewrite of every column's
//! slab whatever it carries, so an issue made out of more inserts than it
//! needs costs every issue what one insert costs. The time an issue takes is
//! printed for reading, never asserted, because a shared machine varies with
//! what else the machine is busy with.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::cast_precision_loss,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard; the counts are small"
)]

mod common;

use common::chain;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tasks::App;
use ui::automerge::ReadDoc as _;
use ui::prelude::*;

const LIST: usize = 200;
const STATUSES: [&str; 3] = ["backlog", "todo", "done"];
const PARA: &str = "The page should load in a second whatever the tracker holds, and a click should answer within a frame. This paragraph stands in for what a real issue says: context, steps to reproduce, what was expected and what happened instead, with a link or two. ";

/// The splices into the op set this codebase makes an issue for, at most:
/// the entry with the issue and the description made as one, the two the
/// description's paragraphs take, and the description's own flag, with the
/// part's bookkeeping to spare. An issue made out of more inserts than this
/// costs every issue what one insert costs.
const SPLICES_PER_ISSUE: f64 = 5.2;

/// The list of issues from `first` on: about 2 KB of description each.
fn list(first: usize) -> Vec<Value> {
    (first..first + LIST)
        .map(|at| {
            json!({
                "title": format!("ENG-{at} Fix the list #{at}"),
                "body": format!("## ENG-{at}\n\n{}\n\n- [ ] first step\n- [ ] second step\n", PARA.repeat(2 + at % 7)),
                "status": STATUSES[at % 3],
                "url": format!("https://linear.example/acme/issue/ENG-{at}"),
                "labels": [format!("label-{}", at % 24), "Bug"],
                "origin": {"author": "Grace Hopper", "handle": "grace@example.com", "source": "Linear",
                    "url": format!("https://linear.example/acme/issue/ENG-{at}"), "reference": format!("ENG-{at}"),
                    "created_ms": 1_756_720_800_000_i64 + i64::try_from(at).unwrap() * 60_000, "updated_ms": null},
            })
        })
        .collect()
}

/// What a tracker of `issues` made them for, per issue.
#[derive(Clone, Copy, Debug)]
struct Cost {
    issue: Duration,
    /// The operations the documents gained, per issue.
    ops: f64,
    /// The splices into the op set, per issue.
    splices: f64,
}

impl Cost {
    /// How much dearer an issue is here than `against` was, per issue.
    fn dearer_than(self, against: Self) -> f64 {
        self.issue.as_secs_f64() / against.issue.as_secs_f64()
    }
}

/// The splices into the op set, for `issues` made in a tracker that holds
/// nothing else, and what each issue took.
fn cost(issues: usize) -> (Cost, TestApp<App>) {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    let before_ops = operations(&app);
    let before_splices = ui::automerge::work().op_set_splices;
    let began = Instant::now();
    for first in (0..issues).step_by(LIST) {
        app.call_on(&chain(), "create-many", json!({ "issues": list(first) }))
            .expect("the issues are made");
    }
    let took = began.elapsed();
    let splices = ui::automerge::work()
        .op_set_splices
        .saturating_sub(before_splices);
    assert_eq!(common::held(&app).len(), issues);
    let per_issue = |count: u64| count as f64 / issues as f64;
    (
        Cost {
            issue: took / u32::try_from(issues).unwrap(),
            ops: per_issue(operations(&app).saturating_sub(before_ops)),
            splices: per_issue(splices),
        },
        app,
    )
}

/// The operations the app's documents hold: its own and every part.
fn operations(app: &TestApp<App>) -> u64 {
    let mut total = 0_u64;
    for id in app.open_resources() {
        total = total.saturating_add(
            app.read_document_of(&id, |doc| doc.stats().num_ops)
                .unwrap_or_default(),
        );
    }
    total
}

#[test]
fn an_issue_costs_the_same_in_a_tracker_of_ten_thousand_as_of_four_hundred() {
    // Each measurement is a tracker holding nothing else, so the counts are
    // of like against like. The one at four hundred is taken twice: a busy
    // machine only ever makes a measurement dearer, so the cheaper of the two
    // is the time that measurement was worth, which the print below compares.
    let (once, _) = cost(400);
    let (large, _) = cost(10_000);
    let (twice, _) = cost(400);
    let small = Cost {
        issue: once.issue.min(twice.issue),
        ops: once.ops.min(twice.ops),
        splices: once.splices.min(twice.splices),
    };
    println!("400: {small:?}");
    println!("10,000: {large:?}");
    // A tracker of many parts keeps a little of each part's own beside the
    // issues (its number, what the part before it gave), so the operations
    // per issue are compared with a part's worth of slack, never growing with
    // what an issue itself is made of.
    assert!(
        large.ops <= small.ops * 1.01,
        "an issue takes {:.1} operations in a tracker of ten thousand, {:.1} in one of four hundred: it is written as more",
        large.ops,
        small.ops
    );
    assert!(
        large.splices <= small.splices * 1.01,
        "an issue takes {:.2} splices into the op set in a tracker of ten thousand, {:.2} in one of four hundred",
        large.splices,
        small.splices
    );
    assert!(
        small.splices <= SPLICES_PER_ISSUE,
        "an issue takes {:.1} splices into the op set, more than the {SPLICES_PER_ISSUE} it is made for",
        small.splices
    );
    // Printed, never asserted: the two measurements are not of the machine
    // at the same moment, and a busy machine makes the same issue dearer in
    // time. What an issue is made of is what the counts above compare.
    println!(
        "an issue at ten thousand takes {:.2} times what it takes at four hundred ({:.0} against {:.0} issues a second)",
        large.dearer_than(small),
        1.0 / large.issue.as_secs_f64(),
        1.0 / small.issue.as_secs_f64()
    );
}
