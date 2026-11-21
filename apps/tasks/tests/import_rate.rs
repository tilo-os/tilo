//! A tracker brought in as `tilo migrate` brings one: lists of 200 issues,
//! each with a description of a few paragraphs, to one `create-many` each.
//! A list of them fills a part of the tracker, so each list starts the next.
//!
//! Each list reads back each description it wrote once, to keep the issue as
//! it reads for whoever reads the tracker next, and that reader takes what
//! the write kept: no description is read section by section, and no issue is
//! read back from the tracker, not on a part's first read and not when a list
//! is too large to follow change by change.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "in a test these are the assertion, not a hazard; the counts are small"
)]

mod common;

use common::chain;
use serde_json::{Value, json};
use tasks::App;
use ui::prelude::*;

const LIST: usize = 200;
const STATUSES: [&str; 3] = ["backlog", "todo", "done"];
const PARA: &str = "The page should load in a second whatever the tracker holds, and a click should answer within a frame. This paragraph stands in for what a real issue says: context, steps to reproduce, what was expected and what happened instead, with a link or two. ";

/// The list of issues from `first` on, as a tracker's export gives them:
/// about 2 KB of description each.
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

#[test]
fn each_list_reads_back_each_description_it_wrote_once() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    let mut read = Vec::new();
    let mut sections = Vec::new();
    let mut items = Vec::new();
    let mut again = Vec::new();
    for first in (0..3 * LIST).step_by(LIST) {
        let list = list(first);
        let ((), observed) = ui::tilo_resource::profiling::observe(|| {
            app.call_on(&chain(), "create-many", json!({ "issues": list }))
                .expect("the issues are made");
        });
        let calls = |phase: &str| {
            observed
                .iter()
                .find(|sample| sample.phase == phase)
                .map_or(0, |sample| sample.calls)
        };
        read.push(calls("rich_text.read.written"));
        sections.push(calls("rich_text.read.parts"));
        items.push(calls("collection.read_item"));
        again.push(calls("rich_text.read.spine") + calls("rich_text.load.materialize"));
    }
    assert_eq!(common::held(&app).len(), 3 * LIST);
    assert!(
        app.chain_parts::<tasks::IssueList>().len() > 1,
        "a list starts a part"
    );
    assert_eq!(read, [LIST; 3], "each list reads its own descriptions once");
    assert_eq!(
        sections, [0; 3],
        "a description just written is not read section by section"
    );
    assert_eq!(
        again, [0; 3],
        "a description just written is not read back from the document"
    );
    assert_eq!(
        items, [0; 3],
        "no list reads an issue back from the tracker, its own or an earlier list's"
    );
}

