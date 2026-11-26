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
