//! Live heap of a document right after import, compared with the same document after save/load.
//!
//! `cargo run --release -p tilo-doc-codecs --example import_memory -- 1000000 5000000`
//!
//! Counts bytes through a global allocator, so the numbers are what the document holds, not what
//! the allocator happens to keep mapped.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    reason = "benchmark driver"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use tilo_doc::Document;
use tilo_doc_codecs::{FormatProvider, MarkdownCodec};

#[path = "../tests/support/synth.rs"]
mod synth;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards to the system allocator, only counting sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let now = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        PEAK.fetch_max(now, Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size >= layout.size() {
            let now = LIVE.fetch_add(new_size - layout.size(), Ordering::Relaxed) + new_size
                - layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        } else {
            LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
        }
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn mib(b: usize) -> f64 {
    #[allow(clippy::cast_precision_loss, reason = "display only")]
    let v = b as f64 / (1024.0 * 1024.0);
    v
}

fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

fn reset_peak() {
    PEAK.store(live(), Ordering::Relaxed);
}

fn peak() -> usize {
    PEAK.load(Ordering::Relaxed)
}

fn import(bytes: &[u8]) -> Document {
    let mut doc = Document::new();
    MarkdownCodec
        .import(
            bytes,
            &mut doc.as_mut(),
            &mut tilo_content::ContentInputs::new(),
        )
        .unwrap();
    doc
}

fn main() {
    let sizes: Vec<usize> = std::env::args()
        .skip(1)
        .map(|a| a.parse().expect("size in bytes"))
        .collect();
    for size in sizes {
        let md = synth::markdown(size);
        let base = live();
        reset_peak();
        let t = Instant::now();
        let mut doc = import(&md);
        let t_imp = t.elapsed();
        let imported = live() - base;
        let imp_peak = peak() - base;
        let saved = doc.save();
        drop(doc);
        let base = live();
        reset_peak();
        let t = Instant::now();
        let loaded = Document::load(&saved).unwrap();
        let t_load = t.elapsed();
        let after_load = live() - base;
        let load_peak = peak() - base;
        drop(loaded);
        println!(
            "size={size:>9} import={t_imp:>10.3?} imported={:>8.1}MiB peak={:>8.1}MiB | saved={:>7.1}MiB load={t_load:>10.3?} loaded={:>8.1}MiB peak={:>8.1}MiB",
            mib(imported),
            mib(imp_peak),
            mib(saved.len()),
            mib(after_load),
            mib(load_peak),
        );
    }
}
