//! Print the text an Automerge document holds.
//!
//! A late joiner that asks a running app for its document gets back an
//! `Automerge::save()` — and that is not a file you can grep. Automerge
//! writes each column of a change chunk DEFLATE-compressed once it is
//! over a few hundred bytes, so a phrase a person typed is present in
//! the document and absent from its bytes. A harness that searched the
//! bytes therefore reported an empty page for a document that had the
//! text in it, and the size growing run over run made it read like the
//! wrong document was being served rather than the right one being
//! misread.
//!
//! So: decode it properly. This walks the whole object graph and prints
//! every `Text` object and every string value it finds, each with the
//! path it sits at. No knowledge of any app's schema — the point is that
//! a harness should not need one to ask "is this text in there".
//!
//! ```text
//! qos-doc-text <file>                 # every string in the document
//! qos-doc-text -                      # ...read from stdin
//! qos-doc-text <file> --contains WORD # exit 0 if present, 1 if not
//! ```
//!
//! `--contains` is the form an assertion wants: it says nothing on a
//! hit and prints the strings it did find on a miss, so a failing check
//! explains itself instead of just failing.

use std::io::Read;
use std::process::ExitCode;

use automerge::{Automerge, ObjId, ObjType, ReadDoc, Value};

/// How deep the walk goes before giving up. An Automerge object graph is
/// a tree, so this is a guard against a malformed document rather than a
/// real limit — no app nests anywhere near it.
const MAX_DEPTH: usize = 64;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!(
            "usage: qos-doc-text <file|-> [--contains TEXT]\n\
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
            "--contains" => match args.next() {
                Some(v) => needle = Some(v),
                None => {
                    eprintln!("--contains needs a value");
                    return ExitCode::from(2);
                }
            },
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

    // `load` is the strict one and it is the right one: a document this
    // cannot parse is not a document, and reporting "no text found"
    // for it would be the same false negative in a new coat.
    let doc = match Automerge::load(&bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{path} is {} bytes and not an Automerge document: {e}", bytes.len());
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

/// Walk `obj` and push every text/string it holds onto `out` as
/// `(path, text)`.
fn collect(doc: &Automerge, obj: &ObjId, path: &str, depth: usize, out: &mut Vec<(String, String)>) {
    if depth > MAX_DEPTH {
        return;
    }
    match doc.object_type(obj) {
        Ok(ObjType::Text) => {
            if let Ok(t) = doc.text(obj) {
                if !t.is_empty() {
                    out.push((path.to_string(), t));
                }
            }
        }
        Ok(ObjType::List) => {
            for i in 0..doc.length(obj) {
                let child = format!("{path}[{i}]");
                match doc.get(obj, i) {
                    Ok(Some((Value::Object(_), id))) => collect(doc, &id, &child, depth + 1, out),
                    Ok(Some((Value::Scalar(s), _))) => push_scalar(&child, &s, out),
                    _ => {}
                }
            }
        }
        // Map, and anything a future Automerge adds that still keys by
        // string — walking it is strictly better than skipping it.
        _ => {
            for key in doc.keys(obj) {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match doc.get(obj, &key) {
                    Ok(Some((Value::Object(_), id))) => collect(doc, &id, &child, depth + 1, out),
                    Ok(Some((Value::Scalar(s), _))) => push_scalar(&child, &s, out),
                    _ => {}
                }
            }
        }
    }
}

fn push_scalar(
    path: &str,
    scalar: &std::borrow::Cow<'_, automerge::ScalarValue>,
    out: &mut Vec<(String, String)>,
) {
    if let automerge::ScalarValue::Str(s) = scalar.as_ref() {
        if !s.is_empty() {
            out.push((path.to_string(), s.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::AutoCommit;

    const TYPED: &str = "the same document on both machines";

    /// The shape a `ui` app's document actually has: a list of blocks,
    /// each a map with a `Text` under it, beside a map of styles. Built
    /// through the ordinary API and put through `save()`, because it is
    /// the saved form a late joiner is handed.
    fn saved_document(body: &str) -> Vec<u8> {
        let mut doc = AutoCommit::new();
        doc.put(&automerge::ROOT, "schema", "qos.richtext").unwrap();
        let blocks = doc
            .put_object(&automerge::ROOT, "blocks", ObjType::List)
            .unwrap();
        let block = doc.insert_object(&blocks, 0, ObjType::Map).unwrap();
        doc.put(&block, "kind", "paragraph").unwrap();
        let text = doc.put_object(&block, "text", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, body).unwrap();
        // Enough sibling content that the save is the size a real
        // document is, rather than a toy small enough to dodge every
        // encoding decision Automerge makes on the way out.
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

    #[test]
    fn text_typed_into_a_document_is_found_by_path() {
        let found = strings(&saved_document(TYPED));
        let hit = found
            .iter()
            .find(|(_, t)| t == TYPED)
            .expect("the body text must be found");
        assert_eq!(
            hit.0, "blocks[0].text",
            "the path must name where the text sits, so a failing check can say where it looked",
        );
    }

    #[test]
    fn a_document_without_the_text_does_not_report_it() {
        let found = strings(&saved_document("something else entirely"));
        assert!(
            !found.iter().any(|(_, t)| t.contains(TYPED)),
            "a document that does not hold the phrase must not claim to",
        );
        // ...and it is not simply finding nothing: the surrounding
        // content is still there, which is what makes the negative
        // above a real answer rather than a broken walk.
        assert!(found.iter().any(|(p, _)| p == "schema"));
        assert!(found.iter().any(|(p, t)| p == "styles.quote.name" && t == "quote"));
    }

    /// The document a real word processor answered with, kept as it came
    /// off the wire.
    ///
    /// Captured from a running packaged app: a headless session, the
    /// word processor launched into it, `TYPED` typed at the keyboard,
    /// and then a client attached to that session's broker announcing
    /// the app's own address asked for the document. This is that
    /// reply, byte for byte. Recording it is what makes the two tests
    /// below claims about the product rather than guesses about
    /// Automerge's encoder.
    const FROM_A_RUNNING_APP: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/word-processor-typed.automerge"
    ));

    /// The same app's document before anything was typed into it.
    const BEFORE_TYPING: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/word-processor-untouched.automerge"
    ));

    /// The reason this program exists.
    ///
    /// A harness asked a running app for its document and searched the
    /// reply for the phrase that had just been typed into it. The phrase
    /// was in the document and not in the bytes, so the harness reported
    /// an empty page on a screen that plainly had the text on it — and
    /// because the document grows as an app is used, it looked like the
    /// wrong document was being served rather than the right one being
    /// misread.
    #[test]
    fn the_reply_from_a_running_app_cannot_be_searched_as_bytes() {
        assert!(
            !contains_bytes(FROM_A_RUNNING_APP, TYPED),
            "a substring search over the saved document does not find the \
             phrase — this is the false negative the tool exists to remove",
        );
        assert_eq!(
            strings(FROM_A_RUNNING_APP)
                .iter()
                .find(|(_, t)| t == TYPED)
                .map(|(p, _)| p.as_str()),
            Some("blocks.77702d32.0.text"),
            "and decoding it finds the phrase, in the paragraph it was typed into",
        );
    }

    /// The control. Without it the test above would also pass against a
    /// document that never had the text, which is the very confusion it
    /// is meant to end.
    #[test]
    fn the_same_app_before_typing_holds_no_such_text() {
        let found = strings(BEFORE_TYPING);
        assert!(
            !found.iter().any(|(_, t)| t.contains(TYPED)),
            "the untouched document must not report the phrase",
        );
        assert!(
            found.iter().any(|(p, t)| p == "schema" && t == "qos.richtext"),
            "and it is a word-processor document all the same, so the negative \
             above is an answer and not an empty walk",
        );
    }

    fn contains_bytes(haystack: &[u8], needle: &str) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle.as_bytes())
    }

    /// A table, built through the model's own API and put through `save()`,
    /// because that is the form a late joiner is handed. The two rows the
    /// assertions read by name, followed by enough further rows to carry the
    /// text column past Automerge's compression threshold.
    ///
    /// The size matters to the control assertion, not to the walk: below that
    /// threshold the cell text survives verbatim in the saved bytes and a plain
    /// byte search finds it, so the test would prove the opposite of what it is
    /// for. A real table of facts is comfortably past it, and the padding
    /// reproduces the ordinary case rather than contriving one.
    fn table_document() -> Vec<u8> {
        const NAMED: [[&str; 3]; 2] =
            [["alpha one", "alpha two", "alpha three"], ["beta one", "beta two", "beta three"]];
        const PADDING_ROWS: usize = 12;

        let mut rows: Vec<Vec<String>> =
            NAMED.iter().map(|r| r.iter().map(|s| s.to_string()).collect()).collect();
        for row in NAMED.len()..NAMED.len() + PADDING_ROWS {
            rows.push((0..3).map(|col| format!("row {row} column {col} of the same table")).collect());
        }

        let mut d = qdoc::Document::new();
        {
            let mut dm = d.as_mut();
            let mut prev: Option<String> = None;
            for (row, cells) in rows.iter().enumerate() {
                for (col, text) in cells.iter().enumerate() {
                    let id = dm.insert_block(prev.as_deref(), "cell");
                    dm.set_attr(&id, "table", "t1");
                    dm.set_attr(&id, "row", row as i64);
                    dm.set_attr(&id, "col", col as i64);
                    dm.splice_text(&id, 0, 0, text);
                    prev = Some(id);
                }
            }
        }
        d.save()
    }

    /// Text in a table cell is text, and is found.
    ///
    /// A walk that stopped at body text would report an empty page for a document
    /// that keeps its facts in cells. Cells are ordinary blocks, so it does not
    /// stop there — said out loud because "the phrase is only in a table" is the
    /// first guess when a check calls a document empty, and acting on it sends
    /// the fix into the wrong file.
    #[test]
    fn text_written_into_a_table_cell_is_found() {
        let saved = table_document();
        let found = strings(&saved);
        let hit = found
            .iter()
            .find(|(_, t)| t == "beta two")
            .expect("a cell's text must be found");
        assert!(
            hit.0.ends_with(".text"),
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
        // ...and it is genuinely unfindable without decoding, which is why
        // nothing may fall back to searching the bytes.
        assert!(
            !contains_bytes(&saved, "beta two"),
            "the phrase must be absent from the saved bytes — that absence is what \
             a byte search would report as an empty page",
        );
    }

    /// The control for the test above.
    #[test]
    fn a_table_does_not_report_cells_it_never_held() {
        let found = strings(&table_document());
        assert!(
            !found.iter().any(|(_, t)| t.contains("gamma")),
            "a table must not claim a row that was never written into it",
        );
    }

    #[test]
    fn nesting_is_walked_to_the_bottom() {
        let mut doc = AutoCommit::new();
        let a = doc.put_object(&automerge::ROOT, "a", ObjType::Map).unwrap();
        let b = doc.put_object(&a, "b", ObjType::List).unwrap();
        let c = doc.insert_object(&b, 0, ObjType::Map).unwrap();
        let t = doc.put_object(&c, "deep", ObjType::Text).unwrap();
        doc.splice_text(&t, 0, 0, "buried").unwrap();
        let found = strings(&doc.save());
        assert_eq!(
            found.iter().find(|(_, t)| t == "buried").map(|(p, _)| p.as_str()),
            Some("a.b[0].deep"),
        );
    }

    #[test]
    fn empty_strings_are_not_reported() {
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
