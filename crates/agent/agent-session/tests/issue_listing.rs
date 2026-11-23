//! Listing a tracker's issues a page at a time costs each page its rows,
//! whatever the tracker holds: a page deep in the listing costs what the
//! first does.
//!
//! The times are printed for reading, never asserted as exact durations; what
//! is asserted is that a page does not grow with its offset and that the
//! whole listing of thousands costs about what its rows do.

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

/// The sizes listed: a small notebook's worth, and a company's tracker.
const SIZES: [usize; 2] = [100, 2_000];

/// The page sizes a caller lists with: the CLI's, and the largest a catalogue
/// page answers.
const PAGES: [usize; 2] = [40, 256];

/// The most a whole listing may take in an unoptimized build on a shared
/// machine: a bound on the whole, not a duration to keep.
const LISTING_CEILING: Duration = Duration::from_secs(30);

/// The address the tracker publishes its issues under.
fn issues() -> String {
    ui::resource::chain_resource_id(&ui::resource::DocumentId::derived(
        "issues",
        &ui::resource::DocumentId::workspace_default(),
    ))
}

/// An issue's description, as long as one written in a tracker: a few
/// kilobytes, so a part holds a couple of hundred and a tracker of thousands
/// is kept in many, as a moved-in one is.
fn description(at: usize) -> String {
    format!(
        "What issue {at} is about, as it was written there. {}",
        "The service keeps its work beside its peers. ".repeat(50)
    )
}

/// The issues one `create-many` makes: a list stays one part's worth, and one
/// action's payload stays under what one action may carry.
const LIST: usize = 100;

/// Fill `page` with `size` issues, a list at a time as an import makes them, a
/// sixth of them sub-issues of earlier ones in the same call.
fn fill(page: &mut Replica, size: usize) {
    let address = ActionAddress::new(issues(), "create-many");
    for start in (0..size).step_by(LIST) {
        let end = (start + LIST).min(size);
        let list: Vec<serde_json::Value> = (start..end)
            .map(|at| {
                let under = if at % 6 == 5 && at > start {
                    Some(serde_json::json!(format!("k{}", at - start)))
                } else {
                    None
                };
                let mut entry = serde_json::json!({
                    "key": format!("k{}", at - start + 1),
                    "title": format!("Issue {at}"),
                    "body": description(at),
                    "status": "todo",
                    "labels": ["Migration"],
                });
                if let Some(under) = under
                    && let Some(fields) = entry.as_object_mut()
                {
                    fields.insert("under".into(), under);
                }
                entry
            })
            .collect();
        page.invoke(
            &address,
            serde_json::json!({"issues": list}).as_object().unwrap(),
            false,
        )
        .expect("the issues are filed");
    }
}

/// Fill `page` with `size` issues of a title alone, a list at a time, as an
/// import of a tracker's titles makes them: a part holds more of them than the
/// page it publishes, so most are listed from past the parts' pages.
fn fill_titles(page: &mut Replica, size: usize) {
    let address = ActionAddress::new(issues(), "create-many");
    for start in (0..size).step_by(LIST) {
        let list: Vec<serde_json::Value> = (start..(start + LIST).min(size))
            .map(|at| serde_json::json!({"title": format!("Issue {at}")}))
            .collect();
        page.invoke(
            &address,
            serde_json::json!({"issues": list}).as_object().unwrap(),
            false,
        )
        .expect("the issues are filed");
    }
}

fn quantile(took: &[Duration], quantile: f64) -> Duration {
    let mut sorted = took.to_vec();
    sorted.sort();
    let at = ((sorted.len() as f64 - 1.0) * quantile).round() as usize;
    sorted.get(at).copied().unwrap_or_default()
}

/// Every row of the chain listed a page at a time, as a caller pages it, with
/// the time each page took. A page is asked for `page_size` rows and the walk
/// goes on from where it ended until the listing answers no more.
fn every_page(page: &Replica, page_size: usize) -> (Vec<CatalogueRow>, Vec<Duration>) {
    let query = CatalogueQuery {
        parent_resource: Some(issues()),
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
fn listing_every_issue_costs_each_page_and_not_its_offset() {
    let mut per_row = Vec::new();
    for size in SIZES {
        let mut page = crate::common::open(
            "so.tilo.tasks",
            &MemberId::person("ada"),
            ReplicaActor::Owner,
            Seed::New,
        )
        .unwrap();
        fill(&mut page, size);
        let began = Instant::now();
        let all = page.survey_rows();
        eprintln!(
            "{size} issues: {} rows surveyed whole in {:?}",
            all.len(),
            began.elapsed()
        );
        assert!(
            all.iter()
                .filter(|row| row.parent_resource == issues())
                .count()
                >= size,
            "the tracker lists every issue it holds"
        );
        for page_size in PAGES {
            let began = Instant::now();
            let (rows, took) = every_page(&page, page_size);
            let total = began.elapsed();
            assert_eq!(
                resources(&rows).len(),
                size,
                "every issue is listed once at {size}"
            );
            let (first, last) = (
                took.first().copied().unwrap_or_default(),
                took.last().copied().unwrap_or_default(),
            );
            eprintln!(
                "{size} issues, {page_size} rows a page: {} pages in {total:?}; first {first:?}, last {last:?}, p50 {:?}",
                took.len(),
                quantile(&took, 0.5)
            );
            // A page deep in the listing costs no more than a small multiple
            // of the first: pages cost their rows, never everything before
            // them.
            assert!(
                last <= first * 4 + Duration::from_millis(50),
                "the last page at {size} issues cost {last:?}, the first {first:?}"
            );
            assert!(
                total < LISTING_CEILING,
                "listing {size} issues at {page_size} rows a page took {total:?}"
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
        eprintln!("{size} issues, {page_size} rows a page: {row:.5} s per issue");
        assert!(
            *row <= small * 4.0 + 0.002,
            "an issue at {size} issues, {page_size} a page costs {row:.5} s, at {} {small:.5} s",
            SIZES[0]
        );
    }

    // Where the time goes, once, for whoever reads the output: the same walk
    // observed by scope.
    let mut page = crate::common::open(
        "so.tilo.tasks",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    fill(&mut page, 2_000);
    let query = CatalogueQuery {
        parent_resource: Some(issues()),
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

/// A page of a listing past the pages a tracker's parts publish builds about
/// the issues it lists, not every issue after them: a page is asked for in
/// rows, and each issue offers a couple of dozen, so a page of ten thousand
/// rows is a few hundred issues and never the thousands still to come.
#[test]
fn a_page_past_the_parts_pages_builds_the_issues_it_lists() {
    const ROWS: usize = 10_000;
    let mut page = crate::common::open(
        "so.tilo.tasks",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    fill(&mut page, 2_000);
    let query = CatalogueQuery {
        parent_resource: Some(issues()),
        max_results: u32::try_from(ROWS).unwrap(),
        by_resource: true,
        ..CatalogueQuery::default()
    };
    // The first page lists the issues the parts publish and counts them; the
    // second starts past those pages.
    let (_, first) = page.survey_page(&query, (false, (0, ROWS)));
    let offered = |rows: &[CatalogueRow]| -> usize {
        rows.iter()
            .map(|row| {
                row.offered
                    .as_ref()
                    .map_or(1, |offered| offered.actions.len().max(1))
            })
            .sum()
    };
    let next = offered(&first);
    assert_eq!(next, ROWS, "the first page is full");
    let ((_, second), observed) = observe(|| page.survey_page(&query, (false, (next, ROWS))));
    let listed = resources(&second).len();
    let built: usize = observed
        .iter()
        .filter(|sample| sample.phase == "resource.published")
        .map(|sample| sample.calls)
        .sum();
    assert!(listed > 0, "the second page lists issues");
    assert!(
        built <= listed * 2,
        "a page listing {listed} issues built {built} of them"
    );
}

/// The resources a listing of issues publishes offer the actions every
/// resource of their kind shares, built once for the kind and not again for
/// each: listing hundreds of issues publishes hundreds of resources and
/// describes their shared actions a handful of times.
#[test]
fn a_listing_of_issues_builds_their_shared_actions_once_for_the_kind() {
    const ROWS: usize = 10_000;
    let mut page = crate::common::open(
        "so.tilo.tasks",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    fill(&mut page, 600);
    let query = CatalogueQuery {
        parent_resource: Some(issues()),
        max_results: u32::try_from(ROWS).unwrap(),
        by_resource: true,
        ..CatalogueQuery::default()
    };
    let (listed, observed) = observe(|| {
        let (_, first) = page.survey_page(&query, (false, (0, ROWS)));
        let next: usize = first
            .iter()
            .map(|row| {
                row.offered
                    .as_ref()
                    .map_or(1, |offered| offered.actions.len().max(1))
            })
            .sum();
        let (_, second) = page.survey_page(&query, (false, (next, ROWS)));
        resources(&first).len() + resources(&second).len()
    });
    let calls = |name: &str| -> usize {
        observed
            .iter()
            .filter(|sample| sample.phase == name)
            .map(|sample| sample.calls)
            .sum()
    };
    assert!(listed >= 500, "the listing lists the issues: {listed}");
    let described = calls("resource.group_spec") + calls("resource.apply_spec");
    assert!(
        described <= 20,
        "listing {listed} issues described the actions their kinds share {described} times"
    );
    let published = calls("resource.published");
    assert!(
        published >= 100,
        "the listing publishes the resources its issues hold: {published}"
    );
}

/// A page asked for a number of issues builds about that many and holds one
/// past them, which says the listing goes on: a page is not built to as many
/// rows as a guess at what each issue offers.
#[test]
fn a_page_asked_for_a_number_of_issues_builds_about_that_many() {
    const ROWS: usize = 10_000;
    let mut page = crate::common::open(
        "so.tilo.tasks",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    fill_titles(&mut page, 600);
    let whole = CatalogueQuery {
        parent_resource: Some(issues()),
        max_results: u32::try_from(ROWS).unwrap(),
        by_resource: true,
        ..CatalogueQuery::default()
    };
    // The second page starts past the issues the parts publish.
    let (_, first) = page.survey_page(&whole, (false, (0, ROWS)));
    let next: usize = first
        .iter()
        .map(|row| {
            row.offered
                .as_ref()
                .map_or(1, |offered| offered.actions.len().max(1))
        })
        .sum();
    let bounded = CatalogueQuery {
        max_resources: Some(40),
        ..whole
    };
    let ((_, second), observed) = observe(|| page.survey_page(&bounded, (false, (next, ROWS))));
    let calls = |name: &str| -> usize {
        observed
            .iter()
            .filter(|sample| sample.phase == name)
            .map(|sample| sample.calls)
            .sum()
    };
    assert_eq!(
        resources(&second).len(),
        41,
        "the page holds the 40 issues asked for and the first row of the next"
    );
    let built = calls("resource.published");
    assert!(
        built <= 2 * 41,
        "a page of 40 issues published {built} resources"
    );
    // The issues past the parts' pages are laid out where the chain is one
    // by one: as many as the page holds, not as many as its rows guess.
    let carried = calls("framework.past_item");
    assert!(
        (41..=2 * 41).contains(&carried),
        "a page of 40 issues carried out {carried} of the issues past the parts' pages"
    );
}
