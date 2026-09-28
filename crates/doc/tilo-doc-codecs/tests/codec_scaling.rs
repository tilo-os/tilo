//! Import and export cost — time for every built-in codec, peak memory for Markdown — grows
//! linearly with document size.
//!
//! What remains per byte is automerge's: every character is an op, integrated at roughly a
//! microsecond each, and an in-memory op set far larger than the saved document (loading the
//! canonical bytes of the 20 MB document alone peaks above a gigabyte), so the budgets below pin
//! the shape rather than a format provider's 256 MiB.
//!
//! Wall-clock budgets only mean something in an optimized build on an otherwise idle machine,
//! so this runs on request:
//!
//! ```text
//! cargo test --release -p tilo-doc-codecs --test it codec_scaling -- --ignored --nocapture
//! ```

use std::time::{Duration, Instant};
use tilo_doc::Document;
use tilo_doc_codecs::export_html::{HtmlOptions, render_html};
use tilo_doc_codecs::export_rtf::render_rtf;
use tilo_doc_codecs::{DocxCodec, FormatProvider, HtmlCodec, MarkdownCodec, OdtCodec, RtfCodec};

use crate::synth;

const SIZES: [usize; 3] = [10_000, 1_000_000, 20_000_000];
/// Markdown import plus export of a 1 MB document.
const MD_1MB_BUDGET: Duration = Duration::from_millis(2_500);
/// Per-byte cost (time, or peak memory) at 20 MB may be at most this multiple of the per-byte
/// cost at 1 MB.
const LINEARITY: f64 = 2.0;

fn time<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let t = Instant::now();
    let v = f();
    (v, t.elapsed())
}

fn import(codec: &dyn FormatProvider, bytes: &[u8]) -> Document {
    let mut doc = Document::new();
    codec
        .import(
            bytes,
            &mut doc.as_mut(),
            &mut tilo_content::ContentInputs::new(),
        )
        .unwrap();
    doc
}

/// Reset the process's peak resident set, so the next [`peak_rss`] covers only what follows.
fn reset_peak_rss() {
    let _ = std::fs::write("/proc/self/clear_refs", "5");
}

/// Peak resident set since the last [`reset_peak_rss`], in bytes (`None` off Linux).
fn peak_rss() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb.saturating_mul(1024))
}

/// `(codec, size, import, export)` for every codec at one size.
fn measure(size: usize) -> Vec<(&'static str, usize, Duration, Duration)> {
    let md = synth::markdown(size);
    let (doc, md_import) = time(|| import(&MarkdownCodec, &md));
    let (_, md_export) = time(|| {
        MarkdownCodec
            .export(doc.as_ref(), &tilo_content::NoContent)
            .unwrap()
    });
    let mut out = vec![("md", size, md_import, md_export)];
    let office: [(&'static str, &dyn FormatProvider); 2] =
        [("docx", &DocxCodec), ("odt", &OdtCodec)];
    for (name, codec) in office {
        let (bytes, export) = time(|| {
            codec
                .export(doc.as_ref(), &tilo_content::NoContent)
                .unwrap()
        });
        let (_, import_t) = time(|| import(codec, &bytes));
        out.push((name, size, import_t, export));
    }
    let (html, export) = time(|| {
        render_html(
            &doc.materialize(),
            HtmlOptions::default(),
            &tilo_content::NoContent,
        )
    });
    let (_, import_t) = time(|| import(&HtmlCodec, html.as_bytes()));
    out.push(("html", size, import_t, export));
    let (rtf, export) = time(|| render_rtf(&doc.materialize()));
    let (_, import_t) = time(|| import(&RtfCodec, rtf.as_bytes()));
    out.push(("rtf", size, import_t, export));
    out
}

/// Peak resident memory of a Markdown import plus export of `size` bytes, per byte.
#[allow(
    clippy::cast_precision_loss,
    reason = "peak memory in bytes, far below 2^52, compared as a ratio"
)]
fn md_peak_per_byte(size: usize) -> Option<f64> {
    let md = synth::markdown(size);
    reset_peak_rss();
    let doc = import(&MarkdownCodec, &md);
    let out = MarkdownCodec
        .export(doc.as_ref(), &tilo_content::NoContent)
        .unwrap();
    let peak = peak_rss()?;
    drop((doc, out));
    println!("md {size:>9} bytes: peak RSS {} MiB", peak / (1024 * 1024));
    Some(peak as f64 / f64::from(u32::try_from(size).ok()?))
}

#[test]
#[ignore = "release-mode benchmark; see the module docs"]
fn codec_scaling() {
    let small_peak = md_peak_per_byte(1_000_000);
    let big_peak = md_peak_per_byte(20_000_000);

    let rows: Vec<_> = SIZES.iter().flat_map(|&s| measure(s)).collect();
    println!("codec  size        import       export");
    for (name, size, import, export) in &rows {
        println!("{name:<6} {size:>9} {import:>12.3?} {export:>12.3?}");
    }
    if let (Some(small), Some(big)) = (small_peak, big_peak) {
        let ratio = big / small;
        assert!(
            ratio < LINEARITY,
            "md: peak memory per byte grew {ratio:.2}x from 1 MB to 20 MB"
        );
    }

    let total = |name: &str, size: usize| {
        rows.iter()
            .find(|r| r.0 == name && r.1 == size)
            .map(|r| r.2 + r.3)
            .unwrap()
    };
    let md_1mb = total("md", 1_000_000);
    assert!(md_1mb < MD_1MB_BUDGET, "1 MB Markdown took {md_1mb:?}");
    for name in ["md", "docx", "odt", "html", "rtf"] {
        let per_byte =
            |size: usize| total(name, size).as_secs_f64() / f64::from(u32::try_from(size).unwrap());
        let ratio = per_byte(20_000_000) / per_byte(1_000_000);
        assert!(
            ratio < LINEARITY,
            "{name}: per-byte cost grew {ratio:.2}x from 1 MB to 20 MB"
        );
    }
}
