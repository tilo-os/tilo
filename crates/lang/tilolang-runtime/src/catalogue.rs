//! Discovery: the signatures an agent reads, built from the metadata, doc comments included,
//! and written like source:
//!
//! ```text
//! /// Add one to the counter.
//! action so.tilo.counter.increment(c: Counter)
//! ```

use crate::meta::{
    ActionMeta, FieldMeta, FlowMeta, FnMeta, MergeKind, NamedRootMeta, PackageMeta, ParamMeta,
    VariantMeta,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignatureKind {
    Type,
    Enum,
    Action,
    /// The root a program reaches an app's own record by.
    Root,
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
    apps.iter().flat_map(|m| of(m)).collect()
}

/// The signature a declaration of `app` is: `id` names it in the catalogue, and `text` is the
/// declaration as source writes it.
fn sig(
    kind: SignatureKind,
    app: &str,
    name: &str,
    id: &str,
    text_doc: &str,
    text: String,
) -> Signature {
    Signature {
        kind,
        app: app.to_owned(),
        name: name.to_owned(),
        id: id.to_owned(),
        doc: doc(text_doc),
        text,
    }
}

/// An action's declaration line.
fn action_text(app: &str, a: &ActionMeta) -> String {
    let proposal = a.proposal.as_ref().map_or_else(String::new, |p| {
        format!(" proposal {}({})", p.name, p.args.join(", "))
    });
    format!(
        "action {app}.{}({}){}{proposal}",
        a.name,
        params(&a.params),
        result(a.result.as_ref())
    )
}

/// A flow's declaration line.
fn flow_text(app: &str, f: &FlowMeta) -> String {
    format!(
        "flow {app}.{}({}){}",
        f.name,
        params(&f.params),
        result(f.result.as_ref())
    )
}

/// A function's declaration line.
fn fn_text(app: &str, f: &FnMeta) -> String {
    format!(
        "fn {app}.{}({}) -> {}",
        f.name,
        params(&f.params),
        src(&f.result)
    )
}

/// The type a root holds, as source writes it.
fn root_type(m: &PackageMeta, root: &NamedRootMeta) -> String {
    let named = m
        .types
        .iter()
        .find(|t| t.id == root.ty)
        .map(|t| t.name.as_str());
    src(named.unwrap_or(&root.ty))
}

/// Signatures of everything `m` declares.
fn of(m: &PackageMeta) -> Vec<Signature> {
    let app = m.app.clone();
    let mut out = Vec::new();
    for t in &m.types {
        let fields = t.fields.iter().map(field).collect::<Vec<_>>().join(", ");
        let text = format!("type {app}.{} {{ {fields} }}", t.name);
        out.push(sig(SignatureKind::Type, &app, &t.name, &t.id, &t.doc, text));
    }
    for e in &m.enums {
        let variants = e
            .variants
            .iter()
            .map(variant)
            .collect::<Vec<_>>()
            .join(", ");
        let text = format!("enum {app}.{} {{ {variants} }}", e.name);
        out.push(sig(SignatureKind::Enum, &app, &e.name, &e.id, &e.doc, text));
    }
    for a in &m.actions {
        let text = action_text(&app, a);
        out.push(sig(
            SignatureKind::Action,
            &app,
            &a.name,
            &a.id,
            &a.doc,
            text,
        ));
    }
    for r in &m.named_roots {
        // A root names a resource the app publishes: `root chat.workspace: Workspace`.
        let text = format!("root {app}.{}: {}", r.name, root_type(m, r));
        out.push(sig(SignatureKind::Root, &app, &r.name, &r.ty, "", text));
    }
    for v in m.offered_views() {
        let text = format!("view {app}.{}({})", v.name, params(&v.params));
        out.push(sig(SignatureKind::View, &app, &v.name, &v.id, &v.doc, text));
    }
    for f in &m.flows {
        let text = flow_text(&app, f);
        out.push(sig(SignatureKind::Flow, &app, &f.name, &f.id, &f.doc, text));
    }
    for f in &m.fns {
        let text = fn_text(&app, f);
        out.push(sig(SignatureKind::Fn, &app, &f.name, &f.id, &f.doc, text));
    }
    out
}
