//! The commands the Books binary runs instead of opening a window.
//!
//! - `--bench-search [QUERY,QUERY,...]` measures the query view's contract:
//!   how long the library's prose takes to index, how long the library's
//!   results take after a keystroke (over the sample shelf and a shelf of
//!   sixty books), and, typed at a person's pace against Project Gutenberg,
//!   how long the store's first rows take (p50 and p95), each keystroke's
//!   query cancelling the one before.

use std::sync::Arc;
use std::time::Duration;

use ui::background::Pumped;
use ui::framework::Command;
use ui::framework::views::{Item, QueryEngine, QuerySpec, RemoteSource, RemoteState};

/// The commands [`crate::EbookShell`] declares.
pub const COMMANDS: &[Command] = &[Command {
    option: "--bench-search",
    run: bench_search,
}];

/// What the bench types when given nothing: words of the sample shelf and
/// of well-known books in the store.
const BENCH_QUERIES: &str = "glass,amber,whale,pride and prejudice,sherlock,frankenstein,dickens";

/// A person's pace: the time between two keystrokes.
const KEYSTROKE: Duration = Duration::from_millis(180);

/// How many copies of the sample shelf the larger shelf holds.
const LARGE_SHELF_COPIES: usize = 20;

fn percentile(sorted: &[f64], at: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let index = ((sorted.len() - 1) as f64 * at).round() as usize;
    sorted.get(index).copied().unwrap_or(f64::NAN)
}

fn report(name: &str, samples: &mut [f64]) {
    samples.sort_by(f64::total_cmp);
    println!(
        "{name}: n={}  p50={:.1}ms  p95={:.1}ms  max={:.1}ms",
        samples.len(),
        percentile(samples, 0.5),
        percentile(samples, 0.95),
        samples.last().copied().unwrap_or(f64::NAN),
    );
}

fn millis(since: ui::time::Instant) -> f64 {
    elapsed(since).as_secs_f64() * 1000.0
}

fn elapsed(since: ui::time::Instant) -> Duration {
    ui::time::now().saturating_duration_since(since)
}

/// Every prefix of `query`, as a person types it.
fn prefixes(query: &str) -> Vec<String> {
    query
        .char_indices()
        .map(|(at, c)| query.get(..at + c.len_utf8()).unwrap_or(query).to_owned())
        .collect()
}

/// The sample shelf `copies` times over, each copy a book of its own.
fn shelf(copies: usize) -> Vec<crate::model::Book> {
    let sample = crate::content::load_sample_library();
    (0..copies)
        .flat_map(|copy| {
            sample.iter().map(move |book| {
                let mut book = book.clone();
                if copy > 0 {
                    book.id = format!("{}-{copy}", book.id);
                    book.title = format!("{} ({})", book.title, copy + 1);
                }
                book
            })
        })
        .collect()
}

/// The shelf's search entries, as the query view takes them.
fn shelf_items(books: &[crate::model::Book]) -> Vec<Item> {
    let kinds = crate::app::query_kinds();
    crate::search_provider::BookSearch::new(books, &[])
        .entries()
        .iter()
        .filter_map(|entry| Item::from_entry(&kinds, entry))
        .collect()
}

fn bench_search(value: &str) -> Result<(), String> {
    let queries: Vec<&str> = if value.trim().is_empty() {
        BENCH_QUERIES.split(',').collect()
    } else {
        value
            .split(',')
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .collect()
    };
    if queries.is_empty() {
        // Only commas: nothing to type.
        return Err(format!("--bench-search: no queries in {value:?}"));
    }
    for (name, copies) in [("sample shelf", 1), ("60-book shelf", LARGE_SHELF_COPIES)] {
        let books = shelf(copies);
        let pumped = Pumped::new();
        let mut engine =
            QueryEngine::new(crate::app::query_kinds()).with_background(pumped.background());
        let started = ui::time::now();
        engine.sync_local(shelf_items(&books));
        let fields = millis(started);
        let started = ui::time::now();
        pumped.run_pending();
        engine.set_query(QuerySpec::text("a"));
        println!(
            "{name}: {} books, fields indexed in {fields:.1}ms, prose in {:.1}ms (off the UI thread)",
            books.len(),
            millis(started)
        );
        let mut samples = Vec::new();
        for query in &queries {
            for prefix in prefixes(query) {
                engine.set_query(QuerySpec::text(prefix));
                if let Some(local) = engine.timings().local {
                    samples.push(local.as_secs_f64() * 1000.0);
                }
            }
        }
        report(&format!("local first results, {name}"), &mut samples);
    }
    bench_store(&queries);
    Ok(())
}

/// The store's first rows per keystroke, typed at a person's pace.
fn bench_store(queries: &[&str]) {
    let store: Arc<dyn RemoteSource> = Arc::new(crate::store::Gutenberg);
    let mut engine = QueryEngine::new(crate::app::query_kinds()).with_remote(Arc::clone(&store));
    // Focusing the field warms the connection before the first character.
    let started = ui::time::now();
    store.warm();
    println!("warm ({}): {:.1}ms", store.label(), millis(started));
    let mut first = Vec::new();
    let mut superseded = 0_usize;
    let mut failed = 0_usize;
    for query in queries {
        let typed = prefixes(query);
        for (at, prefix) in typed.iter().enumerate() {
            let window = if at + 1 == typed.len() {
                Duration::from_secs(5)
            } else {
                KEYSTROKE
            };
            engine.set_query(QuerySpec::text(prefix.clone()));
            let since = ui::time::now();
            let mut got = false;
            while elapsed(since) < window {
                engine.poll();
                if let Some(Some(time)) = engine.timings().remote_first.first() {
                    first.push(time.as_secs_f64() * 1000.0);
                    got = true;
                    break;
                }
                if engine.results().remote.first().is_some_and(|section| {
                    matches!(
                        section.state,
                        RemoteState::Unreachable(_) | RemoteState::Idle
                    )
                }) {
                    failed += 1;
                    break;
                }
                let _ = ui::rt::sleep_thread(Duration::from_millis(1));
            }
            if !got {
                superseded += 1;
            }
            // The rest of the keystroke's time passes before the next one.
            if let Some(rest) = KEYSTROKE.checked_sub(elapsed(since)) {
                let _ = ui::rt::sleep_thread(rest);
            }
        }
        println!(
            "  {query:?}: {} books from the store",
            engine
                .results()
                .remote
                .first()
                .map_or(0, |section| section.rows().count())
        );
    }
    report("store first rows per keystroke", &mut first);
    println!("keystrokes without rows before the next one: {superseded}; unreachable: {failed}");
}
