//! Listing a workspace's tasks a page at a time costs each page its rows, whatever the workspace
//! holds: a page deep in the listing costs what the first does, and the whole listing of
//! thousands costs about what its rows do.
//!
//! The times are printed for reading, never asserted as exact durations; what is asserted is that
//! a page does not grow with its offset, that an item costs about the same however many there
//! are, and that the whole listing stays under a ceiling, which is held at every page so a listing
//! past it stops there.

use std::time::{Duration, Instant};

use tilo_actions::remote::{CataloguePage, CatalogueQuery, CatalogueRow};
use tilo_resource::profiling::observe;

use super::Tasks;

/// The sizes listed: a small team's tasks, and a workspace's years of them.
const SIZES: [usize; 2] = [100, 2_000];

/// The page sizes a caller lists with: the CLI's, and the largest a catalogue page answers.
const PAGES: [usize; 2] = [40, 256];

/// The most a whole listing may take in an unoptimized build on a shared machine: a bound on the
/// whole, not a duration to keep.
const LISTING_CEILING: Duration = Duration::from_secs(30);

/// The tasks one `new_tasks` lands: a list stays one action's payload.
const LIST: usize = 100;

/// A task's title, as long as one written in a tracker.
fn titled(at: usize) -> String {
    format!("Task {at}: answer the landlord about the boiler and the windows on the third floor")
}

fn quantile(took: &[Duration], quantile: f64) -> Duration {
    let mut sorted = took.to_vec();
    sorted.sort();
    let at = ((sorted.len() as f64 - 1.0) * quantile).round() as usize;
    sorted.get(at).copied().unwrap_or_default()
}

/// Fill the record with `size` tasks, a list at a time as an import lands them.
fn fill(tasks: &mut Tasks, size: usize) {
    for start in (0..size).step_by(LIST) {
        let end = (start + LIST).min(size);
        let titles: Vec<String> = (start..end).map(titled).collect();
        tasks.new_tasks(&titles);
    }
}

/// Every row under the tasks listed a page at a time, as a caller pages it, with the time each
/// page took. A page is asked for `page_size` rows and the walk goes on from the cursor it
/// answered until it answers none.
fn every_page(tasks: &Tasks, page_size: usize) -> (Vec<CatalogueRow>, Vec<Duration>) {
    let mut query = CatalogueQuery {
        parent_resource: Some(tasks.root.clone()),
        max_results: u32::try_from(page_size).unwrap(),
        ..CatalogueQuery::default()
    };
    let mut rows = Vec::new();
    let mut took = Vec::new();
    let walk = Instant::now();
    loop {
        let offset = query.offset();
        let began = Instant::now();
        let CataloguePage {
            rows: listed,
            next_cursor,
            ..
        } = tasks.opened.page(&query);
        took.push(began.elapsed());
        let count = listed.len();
        assert!(count <= page_size, "a page holds at most what was asked");
        rows.extend(listed);
        if next_cursor.is_empty() {
            return (rows, took);
        }
        query.cursor = next_cursor;
        assert_eq!(
            query.offset(),
            offset + count,
            "the listing counts what it walked: {count} rows at {offset}"
        );
        let spent = walk.elapsed();
        assert!(
            spent < LISTING_CEILING,
            "listing at {page_size} rows a page reached row {} in {spent:?}, past the ceiling",
            offset + count
        );
    }
}

/// The resources `rows` stand for, each once: what a caller listing a level reads, one per line.
fn resources(rows: &[CatalogueRow]) -> std::collections::HashSet<String> {
    rows.iter().map(|row| row.resource.id.clone()).collect()
}

#[test]
fn listing_every_task_costs_each_page_and_not_its_offset() {
    let mut per_row = Vec::new();
    for size in SIZES {
        let mut tasks = Tasks::founded();
        fill(&mut tasks, size);
        let began = Instant::now();
        let all = tasks.opened.rows();
        eprintln!(
            "{size} tasks: {} rows surveyed whole in {:?}",
            all.len(),
            began.elapsed()
        );
        assert!(
            all.iter()
                .filter(|row| row.parent_resource == tasks.root)
                .count()
                >= size,
            "the record lists every task it holds"
        );
        for page_size in PAGES {
            let began = Instant::now();
            let (rows, took) = every_page(&tasks, page_size);
            let total = began.elapsed();
            assert_eq!(
                resources(&rows).len(),
                size,
                "every task is listed once at {size}"
            );
            let (first, last) = (
                took.first().copied().unwrap_or_default(),
                took.last().copied().unwrap_or_default(),
            );
            eprintln!(
                "{size} tasks, {page_size} rows a page: {} pages in {total:?}; first {first:?}, last {last:?}, p50 {:?}",
                took.len(),
                quantile(&took, 0.5)
            );
            // A page deep in the listing costs no more than a small multiple of the first: pages
            // cost their rows, never everything before them.
            assert!(
                last <= first * 4 + Duration::from_millis(50),
                "the last page at {size} tasks cost {last:?}, the first {first:?}"
            );
            assert!(
                total < LISTING_CEILING,
                "listing {size} tasks at {page_size} rows a page took {total:?}"
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
        eprintln!("{size} tasks, {page_size} rows a page: {row:.5} s per task");
        assert!(
            *row <= small * 4.0 + 0.002,
            "a task at {size} tasks, {page_size} a page costs {row:.5} s, at {} {small:.5} s",
            SIZES[0]
        );
    }

    // Where the time goes, once, for whoever reads the output: the same walk observed by scope.
    let mut tasks = Tasks::founded();
    fill(&mut tasks, SIZES[1]);
    let ((), samples) = observe(|| {
        let mut query = CatalogueQuery {
            parent_resource: Some(tasks.root.clone()),
            max_results: 40,
            ..CatalogueQuery::default()
        };
        let walk = Instant::now();
        loop {
            let next = tasks.opened.page(&query).next_cursor;
            if next.is_empty() {
                break;
            }
            query.cursor = next;
            assert!(walk.elapsed() < LISTING_CEILING, "the observed walk ends");
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
