//! Compare the sandboxed helper with the in-process engine:
//! `bench FILE [WIDTH_PX]` (release build recommended).
//!
//! Reports, per engine, the median over several runs of
//! - cold first page: start the engine, open the file, render page 1;
//! - warm first page: open and render page 1 with the engine running;
//! - scroll throughput: render successive pages at the given width;
//! - text throughput: extract and analyse successive pages' text.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    reason = "a measurement tool"
)]

use std::path::Path;
use std::time::{Duration, Instant};

use tilo_pdf::{Engine, RenderOptions};

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v.get(v.len() / 2).copied().unwrap_or_default()
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

struct Row {
    cold: Duration,
    warm: Duration,
    pages_per_s: f64,
    text_pages_per_s: f64,
}

fn measure(bytes: &[u8], width: u32, make: &dyn Fn() -> Engine) -> Row {
    const RUNS: usize = 9;
    let mut cold = Vec::new();
    for _ in 0..RUNS {
        let t = Instant::now();
        let engine = make();
        let doc = engine.open_bytes(bytes).unwrap();
        let size = doc.page_size(0).unwrap();
        doc.render(0, &RenderOptions::fit_width(size, width))
            .unwrap();
        cold.push(t.elapsed());
    }
    let engine = make();
    let mut warm = Vec::new();
    for _ in 0..RUNS {
        let t = Instant::now();
        let doc = engine.open_bytes(bytes).unwrap();
        let size = doc.page_size(0).unwrap();
        doc.render(0, &RenderOptions::fit_width(size, width))
            .unwrap();
        warm.push(t.elapsed());
    }
    let doc = engine.open_bytes(bytes).unwrap();
    let pages = doc.page_count().min(40);
    let mut scroll = Vec::new();
    let mut text = Vec::new();
    for _ in 0..3 {
        let t = Instant::now();
        for p in 0..pages {
            let size = doc.page_size(p).unwrap();
            let bitmap = doc
                .render(p, &RenderOptions::fit_width(size, width))
                .unwrap();
            // Touch the pixels the way an upload would.
            std::hint::black_box(bitmap.pixels().iter().step_by(4096).count());
        }
        scroll.push(t.elapsed());
        // A fresh document so the text cache does not answer.
        let fresh = engine.open_bytes(bytes).unwrap();
        let t = Instant::now();
        for p in 0..pages {
            std::hint::black_box(fresh.text(p).unwrap().len());
        }
        text.push(t.elapsed());
    }
    Row {
        cold: median(cold),
        warm: median(warm),
        pages_per_s: pages as f64 / median(scroll).as_secs_f64(),
        text_pages_per_s: pages as f64 / median(text).as_secs_f64(),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: bench FILE [WIDTH_PX]");
    let width: u32 = args.next().and_then(|w| w.parse().ok()).unwrap_or(1200);
    let bytes = std::fs::read(&path).expect("read file");
    let helper = std::env::var_os("TILO_PDF_HELPER").map_or_else(
        || {
            std::env::current_exe()
                .unwrap()
                .parent()
                .and_then(Path::parent)
                .unwrap()
                .join(tilo_pdf::HELPER_NAME)
        },
        std::path::PathBuf::from,
    );
    let pages = Engine::in_process()
        .unwrap()
        .open_bytes(&bytes)
        .unwrap()
        .page_count();
    println!(
        "{path}: {} KiB, {pages} pages, {width} px wide",
        bytes.len() / 1024
    );
    let local = measure(&bytes, width, &|| Engine::in_process().unwrap());
    let sandboxed = measure(&bytes, width, &|| Engine::with_helper(&helper).unwrap());
    println!(
        "{:<12} {:>12} {:>12} {:>14} {:>14}",
        "engine", "cold p1 ms", "warm p1 ms", "render pg/s", "text pg/s"
    );
    for (name, r) in [("in-process", &local), ("sandboxed", &sandboxed)] {
        println!(
            "{name:<12} {:>12.1} {:>12.1} {:>14.1} {:>14.1}",
            ms(r.cold),
            ms(r.warm),
            r.pages_per_s,
            r.text_pages_per_s
        );
    }
    println!(
        "overhead: cold +{:.1} ms, warm +{:.1} ms, render {:.1}%, text {:.1}%",
        ms(sandboxed.cold) - ms(local.cold),
        ms(sandboxed.warm) - ms(local.warm),
        (local.pages_per_s / sandboxed.pages_per_s - 1.0) * 100.0,
        (local.text_pages_per_s / sandboxed.text_pages_per_s - 1.0) * 100.0
    );
}
