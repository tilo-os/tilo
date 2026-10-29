//! A package's published declarations, printed as tilolang: its types, enums and action
//! signatures, as another package that `use`s it sees them. `tilolang describe` prints any
//! app's catalogue entry this way, native or tilolang.

use std::fmt::Write as _;

use crate::meta::{FieldMeta, PackageMeta, ParamMeta};
use crate::types::Ty;

/// The declarations of `meta`, as tilolang source: the app header, then each type, enum and
/// action signature with its doc comment.
#[must_use]
pub fn declarations(meta: &PackageMeta) -> String {
    let app = meta.app.as_str();
    let ty =
        |text: &str| Ty::parse_meta(text, app).map_or_else(|| text.to_owned(), |t| t.display(app));
    let mut out = format!("app {app} {}\n", quoted(&meta.title));
    let doc = |out: &mut String, doc: &str, indent: &str| {
        for line in doc.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let _ = writeln!(out, "{indent}/// {line}");
        }
    };
    let fields = |out: &mut String, fields: &[FieldMeta], indent: &str| {
        for field in fields {
            doc(out, &field.doc, indent);
            let _ = writeln!(out, "{indent}{}: {}", field.name, ty(&field.ty));
        }
    };
    for t in &meta.types {
        out.push('\n');
        doc(&mut out, &t.doc, "");
        if t.holder
            && let [held] = t.fields.as_slice()
        {
            let _ = writeln!(out, "type {} = {}", t.name, ty(&held.ty));
            continue;
        }
        let _ = writeln!(out, "type {} {{", t.name);
        fields(&mut out, &t.fields, "    ");
        out.push_str("}\n");
    }
    for e in &meta.enums {
        out.push('\n');
        doc(&mut out, &e.doc, "");
        let _ = writeln!(out, "enum {} {{", e.name);
        for v in &e.variants {
            if !v.label.is_empty() && v.label != crate::resolve::sentence_case(&v.name) {
                let _ = writeln!(out, "    /// {}", v.label);
            }
            if v.fields.is_empty() {
                let _ = writeln!(out, "    {}", v.name);
            } else {
                let _ = writeln!(out, "    {} {{", v.name);
                fields(&mut out, &v.fields, "        ");
                out.push_str("    }\n");
            }
        }
        out.push_str("}\n");
    }
    let params = |params: &[ParamMeta]| {
        params
            .iter()
            .map(|p| format!("{}: {}", p.name, ty(&p.ty)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let actions: Vec<_> = meta.offered_actions().collect();
    if !actions.is_empty() {
        out.push('\n');
    }
    for a in actions {
        doc(&mut out, &a.doc, "");
        let role = match a.role.as_deref() {
            Some(role) => format!("{role} "),
            None => String::new(),
        };
        let result = a
            .result
            .as_ref()
            .map_or_else(String::new, |r| format!(" -> {}", ty(r)));
        let _ = writeln!(
            out,
            "{role}action {}({}){result}",
            a.name,
            params(&a.params)
        );
    }
    out
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}
