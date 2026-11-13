//! What one record costs a workspace's index: the rows one record adds, the
//! heads it stamps, the bytes the file grows, and what opening an index that
//! holds `n` records costs. The index is the whole workspace's — one redb file
//! per package and workspace — so nothing here is a file per record; the
//! question is whether a store of many small units stays cheap to open and to
//! answer one lookup from.
//!
//! Run with:
//!
//!     cargo bench -p tilolang-tilo --bench index_units
//!
//! `--sizes 1,10,100,1000,10000 --samples N` change what it measures. One CSV
//! line per scenario and size, on stdout. Set `TMPDIR` to keep the scratch
//! indexes off a small tmpfs.

#![expect(
    clippy::expect_used,
    reason = "a benchmark fixture must fail rather than measure a half-written index"
)]
// A measurement is bookkeeping over the numbers it just took, not app logic:
// the indices are into the sample vectors the fixture just filled, and the
// arithmetic is over counts, sizes and times.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "see the note above"
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use automerge::ChangeHash;
use tilolang_tilo::index::{Held, Index, Opened, file_of};

const APP: &str = "so.tilo.bench";
const SCHEMA: &str = "bench/1";
const TYPE: &str = "bench.Record";

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("index-units-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn record(index: usize) -> String {
    format!("record-{index:05}")
}

fn head(index: usize) -> ChangeHash {
    let mut bytes = [0_u8; 32];
    bytes[..8].copy_from_slice(&(index as u64).to_le_bytes());
    ChangeHash::try_from(&bytes[..]).expect("a change hash")
}

fn open(path: &Path) -> Held {
    match Index::open(path, APP, SCHEMA).expect("an index") {
        Opened::Ready(index, _) | Opened::Empty(index) => index,
    }
}

/// One record's rows: its document, one field a view shows, and the heads
/// the record holds — what one commit of one record's index writes.
fn write_record(index: &Index, n: usize) {
    index
        .write(|writer| {
            writer.put_doc(&record(n), TYPE, 0)?;
            writer.put_text(&record(n), "title", &format!("record {n}"))?;
            writer.set_heads(&std::iter::once((0, vec![head(n)])).collect())
        })
        .expect("a record");
}

fn ms(elapsed: Duration) -> f64 {
    elapsed.as_secs_f64() * 1_000.0
}

fn summarise(mut samples: Vec<f64>) -> (f64, f64) {
    samples.sort_by(|a, b| a.partial_cmp(b).expect("a time"));
    let n = samples.len();
    let mean = samples.iter().sum::<f64>() / n as f64;
    let median = if n % 2 == 1 {
        samples[n / 2]
    } else {
        samples[n / 2 - 1].midpoint(samples[n / 2])
    };
    (median, mean)
}

fn row(scenario: &str, count: usize, samples: Vec<f64>, bytes: u64) {
    let (median, mean) = summarise(samples);
    println!(
        "{scenario},{count},{median:.2},{mean:.2},{:.1},{bytes}",
        median * 1_000.0 / count as f64
    );
}

fn parse() -> (Vec<usize>, usize) {
    let mut sizes = vec![1, 10, 100, 1_000, 10_000];
    let mut samples = 3;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().expect("a value follows the flag");
        match arg.as_str() {
            "--sizes" => {
                sizes = value()
                    .split(',')
                    .map(|s| s.trim().parse().expect("a size"))
                    .collect();
            }
            "--samples" => samples = value().parse().expect("a sample count"),
            "--bench" => {}
            other => eprintln!("index_units: ignoring {other}"),
        }
    }
    (sizes, samples)
}

fn main() {
    // A test runner executes every target it lists, and this one writes
    // thousands of index commits. Only `cargo bench` measures.
    if !std::env::args().skip(1).any(|arg| arg == "--bench") {
        return;
    }
    let (sizes, samples) = parse();
    println!("scenario,records,median_ms,mean_ms,per_record_us,file_bytes");

    for &n in &sizes {
        let mut writes = Vec::new();
        let mut opens = Vec::new();
        let mut lookups = Vec::new();
        let mut bytes = 0;
        for _ in 0..samples {
            let dir = scratch();
            let path = file_of(&dir, APP);
            let index = open(&path);
            let started = Instant::now();
            for record_index in 0..n {
                write_record(&index, record_index);
            }
            writes.push(ms(started.elapsed()));
            index.persist().expect("the index reaches the disk");
            bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
            drop(index);

            let started = Instant::now();
            let reopened = open(&path);
            opens.push(ms(started.elapsed()));

            let started = Instant::now();
            let snapshot = reopened.snapshot().expect("a snapshot");
            for record_index in 0..n {
                let title = snapshot
                    .text(&record(record_index), "title")
                    .expect("a read");
                assert!(title.is_some());
            }
            lookups.push(ms(started.elapsed()));
            drop(reopened);
            let _ = std::fs::remove_dir_all(&dir);
        }
        row("index_write", n, writes, bytes);
        row("index_open", n, opens, bytes);
        row("index_lookup", n, lookups, bytes);
    }
}
