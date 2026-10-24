//! The commands the Books binary runs instead of opening a window.
//!
//! - `--bench-search [QUERY,QUERY,...]` measures the query view's contract:
//!   how long the library's prose takes to index, how long the library's
//!   results take after a keystroke (over the sample shelf and a shelf of
//!   sixty books), and, typed at a person's pace against Project Gutenberg,
//!   how long the store's first rows take (p50 and p95), each keystroke's
//!   query cancelling the one before.
//! - `--bench-index [BOOKS]` measures the library's index on a fresh device:
//!   real books from Project Gutenberg indexed as the device that adds them
//!   does, their segments' sizes, a library of `BOOKS` (500 when not given)
//!   whose text is all elsewhere read back from its segments, and how long
//!   its results take after each keystroke.
//! - `--bench-catalogue` measures Project Gutenberg's catalogue as the
//!   device keeps a copy of it, fetched as the device fetches it: how long
//!   its index takes to build, and how long its rows take after each
//!   keystroke.

use std::sync::Arc;
use std::time::Duration;

use ui::background::Pumped;
use ui::framework::Command;
use ui::framework::views::{Item, QueryEngine, QuerySpec, RemoteSource};

/// The commands [`crate::EbookShell`] declares.
pub const COMMANDS: &[Command] = &[
    Command {
        option: "--bench-search",
        run: bench_search,
    },
    Command {
        option: "--bench-index",
        run: bench_index,
    },
    Command {
        option: "--bench-catalogue",
        run: bench_catalogue,
    },
];

/// The books the index bench fetches: long and short, prose and verse.
const INDEX_BENCH_BOOKS: &[u64] = &[1342, 2701, 1661, 2600, 84, 98, 11, 1400, 1080, 345];

/// What the index bench types: phrases from the middle of its books.
const INDEX_BENCH_QUERIES: &[&str] = &[
    "universally acknowledged",
    "the whiteness of the whale",
    "speckled band",
    "prince andrew looked",
    "prince andrew",
    "it was the best of times",
    "curiouser and curiouser",
];

/// The library the index bench reads back when not told its size.
const INDEX_BENCH_LIBRARY: usize = 500;

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

/// Where Project Gutenberg publishes its catalogue, as the device fetches
/// its copy.
const CATALOGUE_URL: &str = "https://www.gutenberg.org/cache/epub/feeds/pg_catalog.csv.gz";

/// The most a catalogue the bench fetches may be.
const MAX_CATALOGUE_BYTES: usize = 64 * 1024 * 1024;

/// The catalogue copy's index build and its rows per keystroke.
fn bench_catalogue(_: &str) -> Result<(), String> {
    let bytes = crate::store::fetch(CATALOGUE_URL, &[], MAX_CATALOGUE_BYTES)
        .map_err(|error| format!("--bench-catalogue: not fetched ({error})"))?;
    let started = ui::time::now();
    let index = crate::catalogue::CatalogueIndex::from_gzip(&bytes)
        .ok_or("--bench-catalogue: not a gzip-compressed catalogue")?;
    println!("index of {} books: {:.1}ms", index.len(), millis(started));
    let mut samples = Vec::new();
    for query in BENCH_QUERIES.split(',') {
        for typed in prefixes(query) {
            let words: Vec<String> = typed
                .split_whitespace()
                .map(ui::search::normalize_search_text)
                .collect();
            let started = ui::time::now();
            let rows = index.search(&words, ui::framework::views::GROUP_LIMIT);
            samples.push(millis(started));
            std::hint::black_box(rows);
        }
    }
    report("catalogue rows per keystroke", &mut samples);
    Ok(())
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
    let store: Arc<dyn RemoteSource> = Arc::new(crate::store::Gutenberg::live());
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
                if engine
                    .results()
                    .remote
                    .first()
                    .is_some_and(|section| section.state.is_none() || section.failure().is_some())
                {
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

/// The index bench's books, fetched and indexed as the device adding them
/// does, each its title, author and segment; what each costs is printed.
fn index_bench_segments() -> Result<Vec<(String, String, Vec<u8>)>, String> {
    use crate::library_index::Segment;
    let mut segments = Vec::new();
    for id in INDEX_BENCH_BOOKS {
        let bytes = match crate::store::fetch(
            &crate::store::book_url(*id),
            &[],
            crate::store::MAX_BOOK_BYTES,
        ) {
            Ok(bytes) => bytes,
            Err(error) => {
                println!("book {id}: not fetched ({error})");
                continue;
            }
        };
        let book = crate::epub::parse_epub(&bytes, 0, "")
            .map_err(|error| format!("book {id}: {error}"))?;
        let text: usize = book
            .chapters
            .iter()
            .flat_map(|chapter| &chapter.blocks)
            .map(|block| block.plain_text().len())
            .sum();
        let started = ui::time::now();
        let segment = Segment::build(&book);
        let built = millis(started);
        let encoded = segment.encode();
        let size = |n: usize| ui::format::size(u64::try_from(n).unwrap_or(u64::MAX));
        println!(
            "{:<40} text {:>8}  epub {:>8}  segment {:>8} ({:>8} read back)  built in {built:.0}ms",
            book.title.chars().take(40).collect::<String>(),
            size(text),
            size(bytes.len()),
            size(encoded.len()),
            size(segment.heap_bytes()),
        );
        segments.push((book.title.clone(), book.author.clone(), encoded));
    }
    Ok(segments)
}

fn bench_index(value: &str) -> Result<(), String> {
    use crate::library_index::Segment;
    let size = if value.trim().is_empty() {
        INDEX_BENCH_LIBRARY
    } else {
        value
            .trim()
            .parse()
            .map_err(|_| format!("--bench-index: not a number of books: {value:?}"))?
    };
    let segments = index_bench_segments()?;
    if segments.is_empty() {
        return Err("--bench-index: no book could be fetched".into());
    }
    // A fresh device: every row's segment arrives and is read back; none of
    // the books' text is here.
    let started = ui::time::now();
    let mut entries = Vec::new();
    let mut read_back = Vec::new();
    let (mut stored, mut held) = (0_usize, 0_usize);
    for at in 0..size {
        let Some((title, author, encoded)) = segments.get(at % segments.len()) else {
            continue;
        };
        let segment =
            Segment::decode(encoded).ok_or("--bench-index: a segment did not read back")?;
        stored += encoded.len();
        held += segment.heap_bytes();
        entries.push((
            format!("book:bench-{at}"),
            format!("{title} ({at})"),
            author.clone(),
        ));
        read_back.push(segment);
    }
    println!(
        "{size} books elsewhere: segments {} stored, {} held, read back in {:.0}ms (off the UI thread)",
        ui::format::size(u64::try_from(stored).unwrap_or(u64::MAX)),
        ui::format::size(u64::try_from(held).unwrap_or(u64::MAX)),
        millis(started)
    );
    let source = crate::library_index::elsewhere_books(
        entries
            .into_iter()
            .zip(read_back)
            .map(|((id, title, author), segment)| (id, title, author, Arc::new(segment))),
    );
    let mut engine = QueryEngine::new(crate::app::query_kinds());
    engine.sync_local(Vec::new());
    engine.set_local_source(Some(source));
    let mut samples = Vec::new();
    for query in INDEX_BENCH_QUERIES {
        for prefix in prefixes(query) {
            engine.set_query(QuerySpec::text(prefix));
            if let Some(local) = engine.timings().local {
                samples.push(local.as_secs_f64() * 1000.0);
            }
        }
        let passages: usize = engine
            .results()
            .local
            .iter()
            .map(|group| group.items.len())
            .sum();
        println!("  {query:?}: {passages} passages");
    }
    report(
        &format!("local first results, {size} books elsewhere"),
        &mut samples,
    );
    Ok(())
}
