//! Per-keystroke latency and memory of a retained search catalogue.
//!
//! Builds a [`SearchCatalog`] of file-like records at several corpus sizes,
//! then replays queries one typed character at a time, the way the palette
//! asks for a first page on every keystroke. Prints the median, 95th
//! percentile and worst page latency per size, the build time, and the
//! resident memory the catalogue added.
//!
//! ```sh
//! cargo run --release -p ui-examples --no-default-features --example search-latency
//! ```

use std::time::{Duration, Instant};

use tilo_actions::ActionAddress;
use tilo_ui::search::{FieldWeight, SearchCatalog, SearchRecord, SearchResult};

const SIZES: [usize; 3] = [1_000, 10_000, 100_000];
const PAGE: usize = 20;
const ROUNDS: usize = 5;
const TYPED: [&str; 5] = [
    "quarterly report",
    "budget 2024",
    "invoice-7",
    "notes",
    "zqxv",
];
const WORDS: [&str; 24] = [
    "quarterly",
    "report",
    "budget",
    "invoice",
    "notes",
    "meeting",
    "draft",
    "final",
    "summary",
    "design",
    "review",
    "photo",
    "holiday",
    "plan",
    "roadmap",
    "contract",
    "receipt",
    "letter",
    "schedule",
    "agenda",
    "minutes",
    "backup",
    "archive",
    "project",
];
const EXTENSIONS: [&str; 6] = ["pdf", "md", "txt", "png", "docx", "rs"];

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(self.0 >> 33).unwrap_or_default()
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        let at = self.next().checked_rem(items.len()).unwrap_or_default();
        items.get(at).copied().unwrap_or_default()
    }
}

fn record(index: usize, rng: &mut Lcg) -> SearchRecord {
    let title = format!(
        "{} {} {}.{}",
        rng.pick(&WORDS),
        rng.pick(&WORDS),
        rng.next().checked_rem(10_000).unwrap_or_default(),
        rng.pick(&EXTENSIONS)
    );
    let path = format!("~/{}/{}/{}", rng.pick(&WORDS), rng.pick(&WORDS), title);
    let id = format!("file:{index}");
    let result = SearchResult::for_action("Files", title.clone(), ActionAddress::new(&id, "open"))
        .with_subtitle(path.clone());
    SearchRecord::new(id, result)
        .field(title, FieldWeight::TITLE)
        .field(path, FieldWeight::SUBTITLE)
}

fn resident_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|statm| statm.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map_or(0, |pages| pages.saturating_mul(4096))
}

fn micros(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e6
}

#[allow(
    clippy::cast_precision_loss,
    reason = "a report of resident mebibytes needs no byte precision"
)]
fn mebibytes(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

/// The sample at `percent` of the sorted samples.
fn percentile(sorted: &[Duration], percent: usize) -> Duration {
    let at = sorted.len().saturating_mul(percent) / 100;
    sorted
        .get(at.min(sorted.len().saturating_sub(1)))
        .copied()
        .unwrap_or_default()
}

/// Each prefix of `query`, as typed one character at a time.
fn prefixes(query: &str) -> impl Iterator<Item = &str> {
    query
        .char_indices()
        .filter_map(move |(at, ch)| query.get(..at.saturating_add(ch.len_utf8())))
}

fn main() {
    for size in SIZES {
        let mut rng = Lcg(u64::try_from(size).unwrap_or_default());
        let records: Vec<_> = (0..size).map(|index| record(index, &mut rng)).collect();
        let before = resident_bytes();
        let started = Instant::now();
        let catalog = SearchCatalog::new("Files");
        if let Err(error) = catalog.replace_all_records(records) {
            eprintln!("could not build the catalogue: {error}");
            return;
        }
        // The first query pays for any deferred indexing work.
        let _ = catalog.query_page("warm", None, 256, PAGE);
        let built = started.elapsed();
        let added = resident_bytes().saturating_sub(before);

        let mut samples = Vec::new();
        let mut hits = 0usize;
        for _ in 0..ROUNDS {
            for typed in TYPED.iter().flat_map(|query| prefixes(query)) {
                let started = Instant::now();
                let page = catalog.query_page(typed, None, 256, PAGE);
                samples.push(started.elapsed());
                hits = hits.saturating_add(page.results.len());
            }
        }
        samples.sort_unstable();
        println!(
            "{size:>7} records: build {:>8.1} ms, +{:>6.1} MiB resident, keystroke median {:>7.1} us, p95 {:>7.1} us, max {:>7.1} us ({} keystrokes, {hits} hits)",
            built.as_secs_f64() * 1e3,
            mebibytes(added),
            micros(percentile(&samples, 50)),
            micros(percentile(&samples, 95)),
            micros(percentile(&samples, 100)),
            samples.len(),
        );
    }
}
