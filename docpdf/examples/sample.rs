//! Render a representative qdoc snapshot to `--out PATH` (default
//! `target/docpdf-sample.pdf`) so the output can be opened in a real viewer or
//! validated with `qpdf --check`. Run:
//!
//! ```bash
//! cargo run -p docpdf --example sample -- --out /tmp/sample.pdf
//! ```

use qdoc::snapshot::{AttrVal, BlockView, DocSnapshot};
use std::collections::BTreeMap;

fn block(kind: &str, text: &str, attrs: &[(&str, AttrVal)]) -> BlockView {
    let mut a = BTreeMap::new();
    for (k, v) in attrs {
        a.insert((*k).to_string(), v.clone());
    }
    BlockView {
        id: format!("{kind}-{}", text.len()),
        kind: kind.into(),
        text: text.into(),
        marks: Vec::new(),
        attrs: a,
        children: Vec::new(),
    }
}

fn main() {
    let out = std::env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "--out")
        .map(|w| w[1].clone())
        .unwrap_or_else(|| "target/docpdf-sample.pdf".into());

    let mut blocks = vec![
        block("heading", "qOS Document Export", &[("level", AttrVal::Int(1))]),
        block(
            "paragraph",
            "This document was produced by the docpdf engine from a qdoc DocSnapshot. \
             The paragraph wraps within a US-Letter text column with one-inch margins, \
             demonstrating the word-wrapping and pagination passes.",
            &[],
        ),
        block("heading", "A Numbered List", &[("level", AttrVal::Int(2))]),
        block("list_item", "First item", &[("ordered", AttrVal::Bool(true)), ("index", AttrVal::Int(1))]),
        block("list_item", "Second item, nested", &[("ordered", AttrVal::Bool(true)), ("index", AttrVal::Int(2)), ("level", AttrVal::Int(1))]),
        block("list_item", "Third item", &[("ordered", AttrVal::Bool(true)), ("index", AttrVal::Int(3))]),
        block("heading", "A Table", &[("level", AttrVal::Int(2))]),
    ];

    // Build a small table.
    let row = |cells: &[&str]| {
        let mut r = block("row", "", &[]);
        r.children = cells.iter().map(|c| block("cell", c, &[])).collect();
        r
    };
    let mut table = block("table", "", &[]);
    table.children = vec![
        row(&["Component", "Language", "Status"]),
        row(&["compositor", "Rust", "research"]),
        row(&["ui toolkit", "Rust", "research"]),
    ];
    blocks.push(table);

    let snap = DocSnapshot {
        schema: "qos.richtext".into(),
        schema_version: 1,
        blocks,
        blobs: BTreeMap::new(),
        styles: BTreeMap::new(),
    };

    let bytes = docpdf::render_pdf(&snap).expect("render");
    std::fs::write(&out, &bytes).expect("write pdf");
    println!("wrote {} bytes to {out}", bytes.len());
}
