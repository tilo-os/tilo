//! Import/export timing for every built-in document codec at a range of sizes.
//!
//! `cargo run --release -p tilo-doc-codecs --example codec_scaling -- 10000 1000000`
//! prints one line per codec and size. A Markdown source of the requested
//! size is synthesized, imported, and exported to each format; each format's
//! bytes are then imported back. `CODEC=md` (or `docx`, `odt`, `html`, `rtf`)
//! limits the run to one codec besides the Markdown source. `HOST=1` imports
//! into a new host-owned document instead of an owned [`Document`]; `HOST=edit`
//! imports into one whose generator is seeded from the document, as a host
//! editing an existing document has, which keeps every op in the host's
//! transaction. Peak memory is the process's peak resident set during the
//! Markdown import (Linux only).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    reason = "benchmark driver"
)]

use automerge::AutoCommit;
use std::time::{Duration, Instant};
use tilo_doc::{DocMut, Document, IdGen};
use tilo_doc_codecs::export_html::{HtmlOptions, render_html};
use tilo_doc_codecs::export_rtf::render_rtf;
use tilo_doc_codecs::{DocxCodec, FormatProvider, HtmlCodec, MarkdownCodec, OdtCodec, RtfCodec};

#[path = "../tests/support/synth.rs"]
mod synth;

fn time<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let t = Instant::now();
    let v = f();
    (v, t.elapsed())
}

/// Import `bytes`, timing only the import itself.
fn import(codec: &dyn FormatProvider, bytes: &[u8]) -> (Document, Duration) {
    if std::env::var_os("HOST").is_none() {
        let mut doc = Document::new();
        let ((), t) = time(|| {
            codec
                .import(
                    bytes,
                    &mut doc.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
        });
        return (doc, t);
    }
    let mut am = AutoCommit::new();
    let c = tilo_doc::model::init(&mut am, &automerge::ROOT);
    let actor = am.get_actor().to_hex_string();
    let mut ids = if std::env::var_os("HOST").is_some_and(|h| h == "edit") {
        IdGen::with_counter(actor, 0)
    } else {
        IdGen::new(actor)
    };
    let ((), t) = time(|| {
        let mut dm = DocMut {
            doc: &mut am,
            c: &c,
            ids: &mut ids,
        };
        codec
            .import(bytes, &mut dm, &mut tilo_content::ContentInputs::new())
            .unwrap();
    });
    am.commit();
    (Document::load(&am.save()).unwrap(), t)
}

/// Peak resident set since the previous call, in MiB (Linux; 0 elsewhere).
fn peak_mib() -> u64 {
    let kb = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
        })
        .unwrap_or(0);
    let _ = std::fs::write("/proc/self/clear_refs", "5");
    kb / 1024
}

fn main() {
    let sizes: Vec<usize> = std::env::args()
        .skip(1)
        .map(|a| a.parse().expect("size in bytes"))
        .collect();
    let only = std::env::var("CODEC").ok();
    let want = |name: &str| only.as_deref().is_none_or(|o| o == name);
    for size in sizes {
        let md = synth::markdown(size);
        let _ = peak_mib();
        let (doc, t_imp) = import(&MarkdownCodec, &md);
        let peak = peak_mib();
        let (out, t_exp) = time(|| {
            MarkdownCodec
                .export(doc.as_ref(), &tilo_content::NoContent)
                .unwrap()
        });
        println!(
            "md    size={size:>9} import={t_imp:>12.3?} export={t_exp:>12.3?} out={} peak={peak}MiB",
            out.len()
        );
        drop(out);

        let codecs: [(&str, &dyn FormatProvider); 2] = [("docx", &DocxCodec), ("odt", &OdtCodec)];
        for (name, codec) in codecs {
            if !want(name) {
                continue;
            }
            let (bytes, t_exp) = time(|| {
                codec
                    .export(doc.as_ref(), &tilo_content::NoContent)
                    .unwrap()
            });
            let (_, t_imp) = import(codec, &bytes);
            println!(
                "{name:<5} size={size:>9} import={t_imp:>12.3?} export={t_exp:>12.3?} out={}",
                bytes.len()
            );
        }
        if want("html") {
            let (html, t_exp) = time(|| {
                render_html(
                    &doc.materialize(),
                    HtmlOptions::default(),
                    &tilo_content::NoContent,
                )
            });
            let (_, t_imp) = import(&HtmlCodec, html.as_bytes());
            println!(
                "html  size={size:>9} import={t_imp:>12.3?} export={t_exp:>12.3?} out={}",
                html.len()
            );
        }
        if want("rtf") {
            let (rtf, t_exp) = time(|| render_rtf(&doc.materialize()));
            let (_, t_imp) = import(&RtfCodec, rtf.as_bytes());
            println!(
                "rtf   size={size:>9} import={t_imp:>12.3?} export={t_exp:>12.3?} out={}",
                rtf.len()
            );
        }
    }
}
