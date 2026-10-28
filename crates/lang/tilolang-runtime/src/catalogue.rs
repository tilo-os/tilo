//! Discovery (implementation design 6.3): signatures an agent reads, built from the
//! metadata, doc comments included.

use crate::meta::{FieldMeta, MergeKind, PackageMeta, ParamMeta, VariantMeta};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignatureKind {
    Type,
    Enum,
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

/// A type as source writes it: metadata marks enums (`enum Status`), source does not.
fn src(ty: &str) -> String {
    ty.replace("enum ", "")
}

fn params(ps: &[ParamMeta]) -> String {
    ps.iter()
        .map(|p| {
            // A default is what an invocation that leaves the argument out gets.
            let default = p
                .default
                .as_ref()
                .map(|d| format!(" = {}", d.text))
                .unwrap_or_default();
            format!("{}: {}{default}", p.name, src(&p.ty))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn result(r: Option<&String>) -> String {
    r.filter(|t| t.as_str() != "()")
        .map_or_else(String::new, |t| format!(" -> {}", src(t)))
}

fn field(f: &FieldMeta) -> String {
    let ty = src(&f.ty);
    match f.merge {
        MergeKind::Counter | MergeKind::Register if ty == "Text" => {
            format!("{}: {ty} merge {}", f.name, f.merge.name())
        }
        MergeKind::Counter => format!("{}: {ty} merge counter", f.name),
        _ => format!("{}: {ty}", f.name),
    }
}

fn variant(v: &VariantMeta) -> String {
    if v.fields.is_empty() {
        return v.name.clone();
    }
    let fields: Vec<String> = v
        .fields
        .iter()
        .map(|f| format!("{}: {}", f.name, src(&f.ty)))
        .collect();
    format!("{}({})", v.name, fields.join(", "))
}

fn doc(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_owned())
}

/// Signatures of everything the given apps declare.
pub fn catalogue(apps: &[&PackageMeta]) -> Vec<Signature> {
    let mut out = Vec::new();
    for m in apps {
        let app = m.app.clone();
        let mut push = |kind, name: &str, id: &str, text_doc: &str, text: String| {
            out.push(Signature {
                kind,
                app: app.clone(),
                name: name.to_owned(),
                id: id.to_owned(),
                doc: doc(text_doc),
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
        for e in &m.enums {
            let variants = e
                .variants
                .iter()
                .map(variant)
                .collect::<Vec<_>>()
                .join(", ");
            push(
                SignatureKind::Enum,
                &e.name,
                &e.id,
                &e.doc,
                format!("enum {app}.{} {{ {variants} }}", e.name),
            );
        }
        for a in &m.actions {
            let proposal = a.proposal.as_ref().map_or_else(String::new, |p| {
                format!(" proposal {}({})", p.name, p.args.join(", "))
            });
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
        for v in m.offered_views() {
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
                    "fn {app}.{}({}) -> {}",
                    f.name,
                    params(&f.params),
                    src(&f.result)
                ),
            );
        }
    }
    out
}
