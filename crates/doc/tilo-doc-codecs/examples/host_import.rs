//! Time for importing formatting-heavy RTF into a host-owned document, new and being edited.
//!
//! `cargo run --release -p tilo-doc-codecs --example host_import -- 1000000`
//!
//! Word processors write RTF with the font, size and colour spelled out on every run, so a
//! document carries tens of marks per paragraph. A host editing a document keeps each of them in
//! its open transaction as a local mark; a document being created (a generator from
//! [`IdGen::new`] over nothing but the schema, as a host opening a file has) takes them as
//! changes.

#![allow(
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    reason = "benchmark driver"
)]

use automerge::AutoCommit;
use std::fmt::Write as _;
use std::time::Instant;
use tilo_doc::{DocMut, IdGen};
use tilo_doc_codecs::{FormatProvider, RtfCodec};

/// RTF of at least `target` bytes: paragraphs of 24 runs, each with its own font, size, colour
/// and bold state.
fn rtf(target: usize) -> Vec<u8> {
    let mut s = String::from(
        "{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0 Arial;}{\\f1 Times;}}\
         {\\colortbl;\\red255\\green0\\blue0;\\red0\\green0\\blue255;}\n",
    );
    let mut run = 0usize;
    while s.len() < target {
        s.push_str("\\pard ");
        for word in 0..24 {
            run += 1;
            let font = run % 2;
            let size = 20 + (run % 5) * 2;
            let colour = run % 3;
            let bold = if word % 5 == 0 { "\\b" } else { "\\b0" };
            let _ = write!(s, "{{\\f{font}\\fs{size}\\cf{colour}{bold} word{run} }}");
        }
        s.push_str("\\par\n");
    }
    s.push('}');
    s.into_bytes()
}

fn main() {
    for size in std::env::args().skip(1) {
        let bytes = rtf(size.parse().unwrap());
        for (label, new) in [("editing", false), ("new", true)] {
            let mut am = AutoCommit::new();
            let c = tilo_doc::model::init(&mut am, &automerge::ROOT);
            let actor = am.get_actor().to_hex_string();
            let mut ids = if new {
                IdGen::new(actor)
            } else {
                IdGen::with_counter(actor, 0)
            };
            let t = Instant::now();
            {
                let mut dm = DocMut {
                    doc: &mut am,
                    c: &c,
                    ids: &mut ids,
                };
                RtfCodec
                    .import(&bytes, &mut dm, &mut tilo_content::ContentInputs::new())
                    .unwrap();
            }
            am.commit();
            let elapsed = t.elapsed();
            let snap = tilo_doc::model::materialize(&am, &c);
            let marks: usize = snap.blocks.iter().map(|b| b.marks.len()).sum();
            println!(
                "rtf {:>9} bytes {label:<7} import={elapsed:>10.3?} blocks={} marks={marks}",
                bytes.len(),
                snap.blocks.len()
            );
        }
    }
}
