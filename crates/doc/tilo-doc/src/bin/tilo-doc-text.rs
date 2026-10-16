//! Print the text an Automerge document holds. Automerge compresses each text column once a
//! change chunk passes a few hundred bytes, so a document's text is not found by grepping its
//! saved bytes — this walks the whole object graph and prints every `Text`/string it finds, with
//! no knowledge of any app's schema.
//!
//! ```text
//! tilo-doc-text <file>                 # every string in the document
//! tilo-doc-text -                      # ...read from stdin
//! tilo-doc-text <file> --contains WORD # exit 0 if present, 1 if not
//! ```

use std::io::Read;
use std::process::ExitCode;

use automerge::{Automerge, ObjId, ObjType, ReadDoc, Value};

/// Maximum traversal depth for malformed documents.
const MAX_DEPTH: usize = 64;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!(
            "usage: tilo-doc-text <file|-> [--contains TEXT]\n\
             \n\
             Prints every string an Automerge document holds, one per line,\n\
             as `<path>\\t<text>`. With --contains, prints nothing and exits 0\n\
             when TEXT is present, or lists what was found and exits 1."
        );
        return ExitCode::from(2);
    };

    let mut needle: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--contains" => {
                if let Some(v) = args.next() {
                    needle = Some(v);
                } else {
                    eprintln!("--contains needs a value");
                    return ExitCode::from(2);
                }
            }
            other => {
                eprintln!("unknown argument {other:?}");
                return ExitCode::from(2);
            }
        }
    }

    let bytes = match read_input(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };

    // Treat invalid Automerge bytes as an error rather than reporting no text.
    let doc = match Automerge::load(&bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "{path} is {} bytes and not an Automerge document: {e}",
                bytes.len()
            );
            return ExitCode::from(2);
        }
    };

    let mut found = Vec::new();
    collect(&doc, &automerge::ROOT, "", 0, &mut found);

    match needle {
        None => {
            for (path, text) in &found {
                println!("{path}\t{text}");
            }
            ExitCode::SUCCESS
        }
        Some(want) => {
            if found.iter().any(|(_, t)| t.contains(&want)) {
                ExitCode::SUCCESS
            } else {
                eprintln!(
                    "{want:?} is not in this document. What it does hold ({} strings):",
                    found.len()
                );
                for (path, text) in &found {
                    eprintln!("  {path}\t{text}");
                }
                ExitCode::FAILURE
            }
        }
    }
}

fn read_input(path: &str) -> std::io::Result<Vec<u8>> {
    if path == "-" {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        Ok(buf)
    } else {
        std::fs::read(path)
    }
}

/// Walk `obj` and push every text/string it holds onto `out` as `(path, text)`.
fn collect(
    doc: &Automerge,
    obj: &ObjId,
    path: &str,
    depth: usize,
    out: &mut Vec<(String, String)>,
) {
    if depth > MAX_DEPTH {
        return;
    }
    match doc.object_type(obj) {
        Ok(ObjType::Text) => {
            if let Ok(t) = doc.text(obj)
                && !t.is_empty()
            {
                out.push((path.to_string(), t));
            }
        }
        Ok(ObjType::List) => {
            for i in 0..doc.length(obj) {
                let child = format!("{path}[{i}]");
                match doc.get(obj, i) {
                    Ok(Some((Value::Object(_), id))) => {
                        collect(doc, &id, &child, depth.saturating_add(1), out);
                    }
                    Ok(Some((Value::Scalar(s), _))) => push_scalar(&child, &s, out),
                    _ => {}
                }
            }
        }
        // Map, and anything a future Automerge adds that still keys by string — walking it is strictly better than skipping it.
        _ => {
            for key in doc.keys(obj) {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match doc.get(obj, &key) {
                    Ok(Some((Value::Object(_), id))) => {
                        collect(doc, &id, &child, depth.saturating_add(1), out);
                    }
                    Ok(Some((Value::Scalar(s), _))) => push_scalar(&child, &s, out),
                    _ => {}
                }
            }
        }
    }
}

fn push_scalar(path: &str, scalar: &automerge::ScalarValue, out: &mut Vec<(String, String)>) {
    if let automerge::ScalarValue::Str(s) = scalar
        && !s.is_empty()
    {
        out.push((path.to_string(), s.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use automerge::transaction::Transactable;

    const TYPED: &str = "the same document on both machines";

    /// The shape a `ui` app's document actually has: a list of blocks, each a map with a `Text`
    /// under it, beside a map of styles. Built through the ordinary API and `save()`d, the form a late joiner is handed.
    fn saved_document(body: &str) -> Vec<u8> {
        let mut doc = AutoCommit::new();
        doc.put(&automerge::ROOT, "schema", "tilo.richtext")
            .unwrap();
        let blocks = doc
            .put_object(&automerge::ROOT, "blocks", ObjType::List)
            .unwrap();
        let block = doc.insert_object(&blocks, 0, ObjType::Map).unwrap();
        doc.put(&block, "kind", "paragraph").unwrap();
        let text = doc.put_object(&block, "text", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, body).unwrap();
        // Enough sibling content that the save is the size a real document is, rather than a toy small enough to dodge Automerge's compression.
        let styles = doc
            .put_object(&automerge::ROOT, "styles", ObjType::Map)
            .unwrap();
        for name in ["body", "heading1", "heading2", "heading3", "list", "quote"] {
            let style = doc.put_object(&styles, name, ObjType::Map).unwrap();
            doc.put(&style, "align", "left").unwrap();
            doc.put(&style, "color", "#1c1c20").unwrap();
            doc.put(&style, "font", "sans").unwrap();
            doc.put(&style, "name", name).unwrap();
        }
        doc.save()
    }

    fn strings(bytes: &[u8]) -> Vec<(String, String)> {
        let doc = Automerge::load(bytes).expect("a saved document loads");
        let mut out = Vec::new();
        collect(&doc, &automerge::ROOT, "", 0, &mut out);
        out
    }

    /// The phrase is found where it was typed, both in a document built here
    /// and in the reply a running app gave; the controls — the same shapes
    /// without the phrase — must not report it.
    #[test]
    fn typed_text_is_found_by_path_and_absent_text_is_not() {
        let found = strings(&saved_document(TYPED));
        let hit = found
            .iter()
            .find(|(_, t)| t == TYPED)
            .expect("the body text must be found");
        assert_eq!(
            hit.0, "blocks[0].text",
            "the path must name where the text sits, so a failing check can say where it looked",
        );

        // A document without the text does not report it.
        let found = strings(&saved_document("something else entirely"));
        assert!(
            !found.iter().any(|(_, t)| t.contains(TYPED)),
            "a document that does not hold the phrase must not claim to",
        );
        // Confirms this isn't simply finding nothing: the surrounding content is still there.
        assert!(found.iter().any(|(p, _)| p == "schema"));
        assert!(
            found
                .iter()
                .any(|(p, t)| p == "styles.quote.name" && t == "quote")
        );

        // The reply from a running app cannot be searched as bytes, only decoded.
        assert!(
            !contains_bytes(FROM_A_RUNNING_APP.as_slice(), TYPED),
            "a substring search over the saved document does not find the \
             phrase — this is the false negative the tool exists to remove",
        );
        assert_eq!(
            strings(FROM_A_RUNNING_APP.as_slice())
                .iter()
                .find(|(_, t)| t == TYPED)
                .map(|(p, _)| p.as_str()),
            Some("blocks.77702d32.0.text"),
            "and decoding it finds the phrase, in the paragraph it was typed into",
        );

        // The control: without it the check above would also pass against a
        // document that never had the text.
        let found = strings(&saved_document(""));
        assert!(
            !found.iter().any(|(_, t)| t.contains(TYPED)),
            "the untouched document must not report the phrase",
        );
        assert!(
            found
                .iter()
                .any(|(p, t)| p == "schema" && t == "tilo.richtext"),
            "and it is a word-processor document all the same, so the negative \
             above is an answer and not an empty walk",
        );
    }

    /// The document a real word processor answered with, captured byte-for-byte from a running
    /// packaged app (headless session, text typed, a client asking for the document) — makes the checks that read it claims about the product, not guesses about Automerge's encoder.
    static FROM_A_RUNNING_APP: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/word-processor-typed.automerge.hex"
        ))
        .split_ascii_whitespace()
        .map(|byte| {
            u8::from_str_radix(byte, 16).expect("checked-in fixture contains hexadecimal bytes")
        })
        .collect()
    });

    fn contains_bytes(haystack: &[u8], needle: &str) -> bool {
        haystack
            .windows(needle.len())
            .any(|w| w == needle.as_bytes())
    }

    /// A table, built through the model's own API and `save()`d (the form a late joiner is handed):
    /// the two rows the assertions read by name, plus enough further rows to carry the text column
    /// past Automerge's compression threshold — below it a plain byte search would find the cell text and the test would prove nothing.
    fn table_document() -> Vec<u8> {
        const NAMED: [[&str; 3]; 2] = [
            ["alpha one", "alpha two", "alpha three"],
            ["beta one", "beta two", "beta three"],
        ];
        const PADDING_ROWS: usize = 12;

        let mut rows: Vec<Vec<String>> = NAMED
            .iter()
            .map(|r| r.iter().map(std::string::ToString::to_string).collect())
            .collect();
        for row in NAMED.len()..NAMED.len().saturating_add(PADDING_ROWS) {
            rows.push(
                (0..3)
                    .map(|col| format!("row {row} column {col} of the same table"))
                    .collect(),
            );
        }

        let mut d = tilo_doc::Document::new();
        {
            let mut dm = d.as_mut();
            let mut prev: Option<String> = None;
            for (row, cells) in rows.iter().enumerate() {
                for (col, text) in cells.iter().enumerate() {
                    let id = dm.insert_block(prev.as_deref(), "cell");
                    dm.set_attr(&id, "table", "t1");
                    dm.set_attr(&id, "row", i64::try_from(row).unwrap_or(i64::MAX));
                    dm.set_attr(&id, "col", i64::try_from(col).unwrap_or(i64::MAX));
                    dm.splice_text(&id, 0, 0, text);
                    prev = Some(id);
                }
            }
        }
        d.save()
    }

    /// Text in a table cell is text, and is found: cells are ordinary blocks, so
    /// the walk does not stop at body text. The control: a table does not
    /// report cells it never held.
    #[test]
    fn table_cells_are_found_and_only_the_ones_written() {
        let saved = table_document();
        let found = strings(&saved);
        let hit = found
            .iter()
            .find(|(_, t)| t == "beta two")
            .expect("a cell's text must be found");
        #[allow(
            clippy::case_sensitive_file_extension_comparisons,
            reason = "not a filename: an automerge object-path suffix, exact-match by design"
        )]
        let ends_with_text = hit.0.ends_with(".text");
        assert!(
            ends_with_text,
            "the path must name the cell's text object, got {:?}",
            hit.0,
        );
        // The whole grid, not one lucky cell.
        for cell in ["alpha one", "alpha three", "beta one", "beta three"] {
            assert!(
                found.iter().any(|(_, t)| t == cell),
                "the cell {cell:?} is in the table and must be found",
            );
        }
        // ...and it is genuinely unfindable without decoding, which is why nothing may fall back to searching the bytes.
        assert!(
            !contains_bytes(&saved, "beta two"),
            "the phrase must be absent from the saved bytes — that absence is what \
             a byte search would report as an empty page",
        );

        // The control.
        assert!(
            !found.iter().any(|(_, t)| t.contains("gamma")),
            "a table must not claim a row that was never written into it",
        );
    }

    #[test]
    fn the_walk_reaches_nested_values_and_skips_empty_ones() {
        let mut doc = AutoCommit::new();
        let a = doc.put_object(&automerge::ROOT, "a", ObjType::Map).unwrap();
        let b = doc.put_object(&a, "b", ObjType::List).unwrap();
        let c = doc.insert_object(&b, 0, ObjType::Map).unwrap();
        let t = doc.put_object(&c, "deep", ObjType::Text).unwrap();
        doc.splice_text(&t, 0, 0, "buried").unwrap();
        let found = strings(&doc.save());
        assert_eq!(
            found
                .iter()
                .find(|(_, t)| t == "buried")
                .map(|(p, _)| p.as_str()),
            Some("a.b[0].deep"),
        );

        // Empty strings are not reported.
        let mut doc = AutoCommit::new();
        doc.put(&automerge::ROOT, "blank", "").unwrap();
        let t = doc
            .put_object(&automerge::ROOT, "untouched", ObjType::Text)
            .unwrap();
        let _ = t;
        assert!(
            strings(&doc.save()).is_empty(),
            "an empty value is not content and listing it would only be noise",
        );
    }
}
