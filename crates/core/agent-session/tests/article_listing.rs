//! Listing a feed's articles a page at a time costs each page its rows,
//! whatever the feed holds: a page deep in the listing costs what the first
//! does.
//!
//! The times are printed for reading, never asserted as exact durations; what
//! is asserted is that a page does not grow with its offset and that the whole
//! listing of thousands costs about what its rows do.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, Instant};

use tilo_actions::MemberId;
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{CatalogueQuery, CatalogueRow};
use tilo_agent_session::{Replica, ReplicaActor, Seed};
use ui::tilo_resource::profiling::observe;

/// The sizes listed: a small subscription's worth, and a service's feed.
const SIZES: [usize; 2] = [100, 2_000];

/// The page sizes a caller lists with: the CLI's, and the largest a catalogue
/// page answers.
const PAGES: [usize; 2] = [40, 256];

/// The most a whole listing may take in an unoptimized build on a shared
/// machine: a bound on the whole, not a duration to keep.
const LISTING_CEILING: Duration = Duration::from_secs(30);

/// The subscription the articles come from.
const FEED: &str = "feed-1";

/// The articles one `receive` lands: a list stays one action's payload.
const LIST: usize = 100;

/// The address the feed publishes its articles under.
fn articles() -> String {
    ui::resource::chain_resource_id(&feeds::article_list::chain())
}

/// An article's summary, as long as one written in a feed: a couple of
/// kilobytes, so a part holds some dozens and a feed of thousands is kept in
/// many, as a subscribed one is.
fn said(at: usize) -> String {
    format!(
        "What article {at} is about, as it was written there. {}",
        "The service keeps its work beside its peers. ".repeat(45)
    )
}

fn quantile(took: &[Duration], quantile: f64) -> Duration {
    let mut sorted = took.to_vec();
    sorted.sort();
    let at = ((sorted.len() as f64 - 1.0) * quantile).round() as usize;
    sorted.get(at).copied().unwrap_or_default()
}

/// A feed with its own record.
fn founded() -> Replica {
    crate::common::open(
        "so.tilo.feeds",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap()
}

/// One article, as a refresh brings it.
fn fetched(at: usize) -> serde_json::Value {
    serde_json::json!({
        "title": format!("Article {at}"),
        "url": format!("https://example.test/{at}"),
        "summary": said(at),
    })
}

/// Fill `page` with `size` articles, a list at a time as an import lands them.
fn fill(page: &mut Replica, size: usize) {
    for start in (0..size).step_by(LIST) {
        let end = (start + LIST).min(size);
        let list: Vec<serde_json::Value> = (start..end).map(fetched).collect();
        page.invoke(
            &ActionAddress::new(articles(), "receive"),
            serde_json::json!({"feed": FEED, "articles": list})
                .as_object()
                .unwrap(),
            false,
        )
        .expect("the articles land");
    }
}

/// Every row of the chain listed a page at a time, as a caller pages it, with
/// the time each page took. A page is asked for `page_size` rows and the walk
/// goes on from where it ended until the listing answers no more.
fn every_page(page: &Replica, page_size: usize) -> (Vec<CatalogueRow>, Vec<Duration>) {
    let query = CatalogueQuery {
        parent_resource: Some(articles()),
        max_results: u32::try_from(page_size).unwrap(),
        ..CatalogueQuery::default()
    };
    let mut rows = Vec::new();
    let mut took = Vec::new();
    let mut offset = 0usize;
    loop {
        let began = Instant::now();
        let (total, mut listed) = page.survey_page(&query, (false, (offset, page_size)));
        took.push(began.elapsed());
        assert!(
            total >= offset.saturating_add(listed.len()),
            "the listing counts what it walked: {total} at {offset}"
        );
        let count = listed.len();
        rows.append(&mut listed);
        if count < page_size {
            return (rows, took);
        }
        offset = offset.saturating_add(page_size);
    }
}

/// The resources `rows` stand for, each once: what a caller listing a level
/// reads, one per line.
fn resources(rows: &[CatalogueRow]) -> std::collections::HashSet<String> {
    rows.iter().map(|row| row.resource.id.clone()).collect()
}

#[test]
fn listing_every_article_costs_each_page_and_not_its_offset() {
    let mut per_row = Vec::new();
    for size in SIZES {
        let mut page = founded();
        fill(&mut page, size);
        let began = Instant::now();
        let all = page.survey_rows();
        eprintln!(
            "{size} articles: {} rows surveyed whole in {:?}",
            all.len(),
            began.elapsed()
        );
        assert!(
            all.iter()
                .filter(|row| row.parent_resource == articles())
                .count()
                >= size,
            "the feed lists every article it holds"
        );
        for page_size in PAGES {
            let began = Instant::now();
            let (rows, took) = every_page(&page, page_size);
            let total = began.elapsed();
            assert_eq!(
                resources(&rows).len(),
                size,
                "every article is listed once at {size}"
            );
            let (first, last) = (
                took.first().copied().unwrap_or_default(),
                took.last().copied().unwrap_or_default(),
            );
            eprintln!(
                "{size} articles, {page_size} rows a page: {} pages in {total:?}; first {first:?}, last {last:?}, p50 {:?}",
                took.len(),
                quantile(&took, 0.5)
            );
            // A page deep in the listing costs no more than a small multiple
            // of the first: pages cost their rows, never everything before
            // them.
            assert!(
                last <= first * 4 + Duration::from_millis(50),
                "the last page at {size} articles cost {last:?}, the first {first:?}"
            );
            assert!(
                total < LISTING_CEILING,
                "listing {size} articles at {page_size} rows a page took {total:?}"
            );
            per_row.push((size, page_size, total.as_secs_f64() / size as f64));
        }
    }
    let small = per_row
        .iter()
        .filter(|(size, _, _)| *size == SIZES[0])
        .map(|(_, _, row)| *row)
        .fold(f64::INFINITY, f64::max);
    for (size, page_size, row) in &per_row {
        eprintln!("{size} articles, {page_size} rows a page: {row:.5} s per article");
        assert!(
            *row <= small * 4.0 + 0.002,
            "an article at {size} articles, {page_size} a page costs {row:.5} s, at {} {small:.5} s",
            SIZES[0]
        );
    }

    // Where the time goes, once, for whoever reads the output: the same walk
    // observed by scope.
    let mut page = founded();
    fill(&mut page, 2_000);
    let query = CatalogueQuery {
        parent_resource: Some(articles()),
        max_results: 40,
        ..CatalogueQuery::default()
    };
    let ((), samples) = observe(|| {
        let mut offset = 0usize;
        loop {
            let (_, listed) = page.survey_page(&query, (false, (offset, 40)));
            let short = listed.len() < 40;
            offset = offset.saturating_add(40);
            if short {
                break;
            }
        }
    });
    let mut samples = samples;
    samples.sort_by_key(|sample| std::cmp::Reverse(sample.elapsed_ns));
    for sample in samples.iter().take(12) {
        eprintln!(
            "  {:>36} {:>10} calls {:>10.1} ms",
            sample.phase,
            sample.calls,
            sample.elapsed_ns as f64 / 1e6
        );
    }
}
