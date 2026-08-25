//! What one keystroke costs, measured on the path a person's keystroke takes.
//!
//! A character typed into a document runs the app's whole frame: `handle_event`
//! mutates the CRDT, then `sync` → `build_layout` → `apply_layout` → `draw`
//! rebuild what is on screen. If that round trip is slower than the gap between
//! keystrokes, characters queue in the compositor and land in bursts — which is
//! what "no typing, then a whole bunch of letters at once" looks like from the
//! keyboard.
//!
//! So this measures per-character cost, not per-frame average, and prints the
//! distribution: a burst is a tail, and a mean hides it. It also types into
//! documents of different sizes, because the failure mode worth ruling in or
//! out is cost that grows with the document — a CRDT write that rewrites the
//! whole text on every press is O(n) per key and O(n²) over a paragraph.
//!
//! ```text
//! cargo run -p word-processor --release --example typing_latency
//! cargo run -p word-processor --release --example typing_latency -- --chars 400
//! ```
//!
//! Release only. A debug build of this tree is slow enough to make every
//! reading meaningless.

use std::path::PathBuf;
use std::time::Instant;

use ui::headless::Driver;
use word_processor::{open_document, EditorDoc};

/// One paragraph of ordinary prose, used to pad a document to a size.
const PARAGRAPH: &str = "The rehearsal ran long and the doors opened late, so the \
first half went out to a room that was still finding its seats. Nobody minded much. \
By the interval the balcony had filled and the noise off it was the good kind.";

/// A document on disk with `paragraphs` paragraphs in it, opened the way a
/// person's is: through `open_document`, decoded by the format's codec.
fn document_with(paragraphs: usize) -> PathBuf {
    let dir = std::env::temp_dir().join("word-processor-typing-latency");
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let path = dir.join(format!("body-{paragraphs}.md"));
    let mut body = String::new();
    for _ in 0..paragraphs {
        body.push_str(PARAGRAPH);
        body.push_str("\n\n");
    }
    std::fs::write(&path, &body).expect("the document is written");
    path
}

/// Nearest-rank percentile (NIST): the value at 1-indexed rank `ceil(p/100·n)`.
fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

/// Type `chars` characters into a document of `paragraphs` paragraphs, timing
/// each character end to end. Returns the per-character costs in order typed.
fn type_and_time(paragraphs: usize, chars: usize) -> Vec<u64> {
    let path = document_with(paragraphs);
    let (state, wp, _fmt) = open_document(&path).expect("the document opens");
    let mut driver = Driver::new(state, Box::new(wp), (1200, 800));

    // Warm up the shaping caches so the first reading is not a cold-start
    // artefact, and leave a caret in the body the way an opened document does.
    driver.type_text("warmup");

    let text: Vec<char> = "the quick brown fox jumps over the lazy dog "
        .chars()
        .cycle()
        .take(chars)
        .collect();

    let mut per_char = Vec::with_capacity(chars);
    for ch in text {
        let t = Instant::now();
        driver.key(&ch.to_string());
        per_char.push(t.elapsed().as_micros() as u64);
    }

    // Prove the characters actually landed: a harness that measured an editor
    // quietly dropping input would report a very fast, very wrong number.
    let st = driver.state_mut();
    let ed = EditorDoc::bind(&mut st.doc, &st.root);
    let total: usize = ed
        .blocks(&st.doc)
        .iter()
        .map(|id| ed.text(&st.doc, id).unwrap_or_default().chars().count())
        .sum();
    let expected = paragraphs * PARAGRAPH.chars().count() + "warmup".len() + chars;
    assert!(
        total >= expected,
        "only {total} characters are in the document, expected at least \
         {expected}: the harness typed into something that did not keep the text",
    );

    per_char
}

/// Print the distribution of `per_char`, plus how the cost moves from the start
/// of the run to the end — a document-size effect shows up there before it
/// shows up in the percentiles.
fn report(label: &str, per_char: &[u64]) {
    let mut sorted = per_char.to_vec();
    sorted.sort_unstable();
    let ms = |us: u64| us as f64 / 1000.0;
    let mean = per_char.iter().sum::<u64>() / per_char.len().max(1) as u64;

    let quarter = per_char.len() / 4;
    let first = &per_char[..quarter.max(1)];
    let last = &per_char[per_char.len() - quarter.max(1)..];
    let avg = |v: &[u64]| v.iter().sum::<u64>() / v.len().max(1) as u64;

    // 60 fps: a character that costs more than one frame cannot be drawn
    // before the next one arrives at any ordinary typing speed.
    let over = per_char.iter().filter(|&&us| us > 16_667).count();

    println!(
        "{label:<28} n={:<4} mean {:>7.2}  p50 {:>7.2}  p95 {:>7.2}  p99 {:>7.2}  max {:>7.2}   \
         first¼ {:>7.2} → last¼ {:>7.2} ({:.1}×)   over 16.7ms: {} ({:.0}%)",
        per_char.len(),
        ms(mean),
        ms(percentile(&sorted, 50.0)),
        ms(percentile(&sorted, 95.0)),
        ms(percentile(&sorted, 99.0)),
        ms(percentile(&sorted, 100.0)),
        ms(avg(first)),
        ms(avg(last)),
        avg(last) as f64 / avg(first).max(1) as f64,
        over,
        over as f64 * 100.0 / per_char.len() as f64,
    );
}

fn main() {
    ui::i18n::I18nManager::init("en-US");

    let args: Vec<String> = std::env::args().collect();
    let chars = args
        .iter()
        .position(|a| a == "--chars")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(300usize);

    println!(
        "per-character cost, end to end (handle_event + sync + layout + draw), \
         milliseconds\n"
    );
    for paragraphs in [0usize, 1, 4, 16] {
        let per_char = type_and_time(paragraphs, chars);
        report(&format!("{paragraphs} paragraph(s) of body"), &per_char);
    }
}
