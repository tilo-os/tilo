//! Discovery (implementation design 6.3): signatures an agent reads, built from the
//! metadata, doc comments included.

use crate::meta::{FieldMeta, Meta, ParamMeta};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignatureKind {
    Type,
    Action,
    View,
    Flow,
    Fn,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Signature {
    pub kind: SignatureKind,
    pub app: String,
    pub name: String,
    pub id: String,
    pub doc: Option<String>,
    /// The declaration line, without its doc comment.
    pub text: String,
}

impl Signature {
    /// The signature as source: doc comment lines, then the declaration.
    pub fn render(&self) -> String {
        let mut out = String::new();
        if let Some(doc) = &self.doc {
            for line in doc.lines() {
                out.push_str("/// ");
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push_str(&self.text);
        out
    }
}

fn params(ps: &[ParamMeta]) -> String {
    ps.iter()
        .map(|p| format!("{}: {}", p.name, p.ty))
        .collect::<Vec<_>>()
        .join(", ")
}

fn result(r: Option<&String>) -> String {
    r.map_or_else(String::new, |t| format!(" -> {t}"))
}

fn field(f: &FieldMeta) -> String {
    match &f.merge {
        Some(m) => format!("{}: {} merge {m}", f.name, f.ty),
        None => format!("{}: {}", f.name, f.ty),
    }
}

/// Signatures of everything the given apps declare.
pub fn catalogue(apps: &[&Meta]) -> Vec<Signature> {
    let mut out = Vec::new();
    for m in apps {
        let app = m.app.clone();
        let mut push = |kind, name: &str, id: &str, doc: &Option<String>, text: String| {
            out.push(Signature {
                kind,
                app: app.clone(),
                name: name.to_owned(),
                id: id.to_owned(),
                doc: doc.clone(),
                text,
            });
        };
        for t in &m.types {
            let fields = t.fields.iter().map(field).collect::<Vec<_>>().join(", ");
            push(
                SignatureKind::Type,
                &t.name,
                &t.id,
                &t.doc,
                format!("type {app}.{} {{ {fields} }}", t.name),
            );
        }
        for a in &m.actions {
            let proposal = a
                .proposal
                .as_ref()
                .map_or_else(String::new, |p| format!(" proposal {p}"));
            push(
                SignatureKind::Action,
                &a.name,
                &a.id,
                &a.doc,
                format!(
                    "action {app}.{}({}){}{proposal}",
                    a.name,
                    params(&a.params),
                    result(a.result.as_ref())
                ),
            );
        }
        for v in &m.views {
            push(
                SignatureKind::View,
                &v.name,
                &v.id,
                &v.doc,
                format!("view {app}.{}({})", v.name, params(&v.params)),
            );
        }
        for f in &m.flows {
            push(
                SignatureKind::Flow,
                &f.name,
                &f.id,
                &f.doc,
                format!(
                    "flow {app}.{}({}){}",
                    f.name,
                    params(&f.params),
                    result(f.result.as_ref())
                ),
            );
        }
        for f in &m.fns {
            push(
                SignatureKind::Fn,
                &f.name,
                &f.id,
                &f.doc,
                format!(
                    "fn {app}.{}({}){}",
                    f.name,
                    params(&f.params),
                    result(f.result.as_ref())
                ),
            );
        }
    }
    out
}
