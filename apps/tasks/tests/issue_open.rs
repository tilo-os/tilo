//! What opening an issue's page costs, in the work it asks of text: the description is laid out
//! where the page shows it, not at widths a layout query asks for on the way there.
//!
//! A wrapped layout is keyed by the width it wrapped to, so a layout made at a width the page
//! never shows is work thrown away — and one made at a width no glyph fits in wraps every
//! character onto a row of its own, which for a page of prose is thousands of rows.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]


mod common;

use common::{chain, held};
use serde_json::json;
use tasks::App;
use tasks::model::issue_resource;
use ui::prelude::*;

/// The paragraphs of the description, and the words of each: a page-long issue, as an issue with
/// a written description is.
const PARAGRAPHS: usize = 30;
const PARAGRAPH: &str = "The nightly build fails when the cache is warm and the log only says \
the artifact was already published, which hides the step that broke.";

fn open() -> TestApp<App> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<App>::for_person("ada").expect("open")
}

#[test]
fn opening_an_issue_lays_its_description_out_where_the_page_shows_it() {
    let mut app = open();
    let body = (0..PARAGRAPHS)
        .map(|index| format!("Paragraph {index}: {PARAGRAPH}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    app.call_on(
        &chain(),
        "create",
        json!({ "title": "Fix the nightly", "body": body }),
    )
    .expect("create");
    app.settle();
    let issue = held(&app).ids().into_iter().next().expect("an issue");

    let _ = ui::profiling::text_layouts::take();
    app.call_on(&issue_resource(&issue), "open", json!({}))
        .expect("open");
    app.settle();

    // The description's paragraphs, by their length: the page's other fields hold a word or two.
    let laid_out: Vec<ui::profiling::text_layouts::LaidOutText> =
        ui::profiling::text_layouts::take()
            .into_iter()
            .filter(|text| text.bytes > 100)
            .collect();
    assert!(
        laid_out.len() >= PARAGRAPHS,
        "the page laid its description out"
    );

    // A query without a definite width measures a line at the width a line assumes; the page then
    // lays the text out at the width it shows it at. Neither is a width no line fits in, which
    // would put every character on a row of its own.
    let mut widths: Vec<f32> = laid_out.iter().map(|text| text.width).collect();
    widths.sort_by(f32::total_cmp);
    widths.dedup();
    assert!(
        widths.iter().all(|width| *width >= 300.0),
        "the description is laid out at widths a line fits in: {widths:?}"
    );
    assert!(
        widths.len() <= 2,
        "it is laid out where a line is assumed and where the page shows it: {widths:?}"
    );
    let rows: usize = laid_out.iter().map(|text| text.rows).sum();
    assert!(
        rows <= 10 * PARAGRAPHS,
        "{rows} rows for a description of {PARAGRAPHS} paragraphs laid out at {widths:?}"
    );
}
