//! Render a representative tilo-doc snapshot to `--out PATH` (default
//! `target/docpdf-sample.pdf`) so the output can be opened in a real viewer or
//! validated with `qpdf --check`. Run:
//!
//! ```bash
//! cargo run -p docpdf --example sample -- --out /tmp/sample.pdf
//! ```

use std::collections::BTreeMap;
use tilo_doc::snapshot::{AttrVal, BlockView, DocSnapshot};

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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::env::args()
        .collect::<Vec<_>>()
        .windows(2)
        // `windows(2)` guarantees exactly 2 elements per window, so both
        // `.first()`/`.get(1)` are always `Some`; `.get()` just spells
        // that out instead of indexing.
        .find(|w| w.first().is_some_and(|first| first == "--out"))
        .map_or_else(
            || "target/docpdf-sample.pdf".into(),
            |w| w.get(1).cloned().unwrap_or_default(),
        );

    let mut blocks = vec![
        block(
            "heading",
            "Tilo Document Export",
            &[("level", AttrVal::Int(1))],
        ),
        block(
            "paragraph",
            "This document was produced by the docpdf engine from a tilo-doc DocSnapshot. \
             The paragraph wraps within a US-Letter text column with one-inch margins, \
             demonstrating the word-wrapping and pagination passes.",
            &[],
        ),
        block("heading", "A Numbered List", &[("level", AttrVal::Int(2))]),
        block(
            "list_item",
            "First item",
            &[("ordered", AttrVal::Bool(true)), ("index", AttrVal::Int(1))],
        ),
        block(
            "list_item",
            "Second item, nested",
            &[
                ("ordered", AttrVal::Bool(true)),
                ("index", AttrVal::Int(2)),
                ("level", AttrVal::Int(1)),
            ],
        ),
        block(
            "list_item",
            "Third item",
            &[("ordered", AttrVal::Bool(true)), ("index", AttrVal::Int(3))],
        ),
        block("heading", "A Table", &[("level", AttrVal::Int(2))]),
    ];

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
        schema: "tilo.richtext".into(),
        schema_version: 1,
        blocks,
        styles: BTreeMap::new(),
        ..Default::default()
    };

    let bytes = tilo_doc_pdf::render_pdf(&snap, &tilo_content::NoContent)?;
    std::fs::write(&out, &bytes)?;
    println!("wrote {} bytes to {out}", bytes.len());
    Ok(())
}
