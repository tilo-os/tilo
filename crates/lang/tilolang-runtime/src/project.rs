//! Model projection (implementation design 6.2): what an agent sees of a value.
//!
//! Records become `{type, id, label, fields}` with every field projected one level less
//! deep, lists keep their first items and report their count, long text is cut and says so,
//! and references below the top level shrink to `{type, id, label}`.

use serde_json::{Map, Value as Json, json};

use crate::core::Core;
use crate::meta::FieldKind;
use crate::value::{RefValue, Value};

/// How much of a value the projection keeps.
#[derive(Clone, Copy, Debug)]
pub struct ProjectOptions {
    pub max_items: usize,
    pub max_text_chars: usize,
    /// How many levels of references expand into their fields.
    pub depth: usize,
}

impl Default for ProjectOptions {
    fn default() -> Self {
        Self {
            max_items: 20,
            max_text_chars: 200,
            depth: 1,
        }
    }
}

pub fn project(core: &Core, v: &Value, opts: ProjectOptions) -> Json {
    match v {
        Value::Unit | Value::Opt(None) => Json::Null,
        Value::Int(i) => json!(i),
        Value::Float(f) => {
            serde_json::Number::from_f64(*f).map_or_else(|| json!(f.to_string()), Json::Number)
        }
        Value::Bool(b) => json!(b),
        Value::Text(s) => project_text(s, opts),
        Value::List(items) => {
            let shown: Vec<Json> = items
                .iter()
                .take(opts.max_items)
                .map(|x| project(core, x, opts))
                .collect();
            let mut m = Map::new();
            m.insert("count".into(), json!(items.len()));
            m.insert("items".into(), Json::Array(shown));
            if items.len() > opts.max_items {
                m.insert("truncated".into(), json!(true));
            }
            Json::Object(m)
        }
        Value::Tuple(items) => Json::Array(items.iter().map(|x| project(core, x, opts)).collect()),
        Value::Opt(Some(x)) => project(core, x, opts),
        Value::Enum(e) => project_enum(core, e, opts),
        Value::Ref(r) => project_ref(core, r, opts),
        Value::Action(a) => json!({
            "action": a.action,
            "args": a.args.iter().map(|x| project(core, x, opts)).collect::<Vec<_>>(),
        }),
        Value::Principal(p) => json!({ "principal": p.0 }),
        Value::Error(msg) => json!({ "error": msg }),
        Value::Node(_) | Value::Rec(_) => json!({ "kind": v.kind_name() }),
    }
}

/// An enum value as `{"variant": name, <field>: value...}`, by the names its metadata gives.
fn project_enum(core: &Core, e: &crate::value::EnumValue, opts: ProjectOptions) -> Json {
    let meta = core.registry.borrow().enum_meta(&e.ty);
    let variant = meta
        .as_ref()
        .and_then(|m| m.variants.iter().find(|v| v.id == e.variant));
    let mut m = Map::new();
    m.insert(
        "variant".into(),
        json!(variant.map_or(e.variant.as_str(), |v| v.name.as_str())),
    );
    for (id, v) in &e.fields {
        let name = variant
            .and_then(|var| var.fields.iter().find(|f| &f.id == id))
            .map_or(id.as_str(), |f| f.name.as_str());
        m.insert(name.to_owned(), project(core, v, opts));
    }
    Json::Object(m)
}

fn project_text(s: &str, opts: ProjectOptions) -> Json {
    let chars = s.chars().count();
    if chars <= opts.max_text_chars {
        return json!(s);
    }
    let cut: String = s.chars().take(opts.max_text_chars).collect();
    json!({ "text": cut, "truncated": true, "chars": chars })
}

fn project_ref(core: &Core, r: &RefValue, opts: ProjectOptions) -> Json {
    let Some((_, info)) = core.registry.borrow().type_info(&r.target.ty) else {
        return json!({ "type": r.target.ty, "id": r.target.id_string() });
    };
    let mut label = None;
    let mut fields = Map::new();
    let deeper = ProjectOptions {
        depth: opts.depth.saturating_sub(1),
        ..opts
    };
    for f in &info.fields {
        let Ok(value) = core.read_value(&r.target, f, None) else {
            continue;
        };
        if f.kind == FieldKind::Text
            && let Value::Text(t) = &value
        {
            let named = matches!(f.name.as_str(), "title" | "name" | "label");
            if named || label.is_none() {
                label = Some(t.chars().take(opts.max_text_chars).collect::<String>());
            }
        }
        if opts.depth > 0 {
            let shown = match (&f.kind, &value) {
                (FieldKind::Collection | FieldKind::Set | FieldKind::Map, Value::List(items)) => {
                    json!({ "count": items.len() })
                }
                _ => project(core, &value, deeper),
            };
            fields.insert(f.name.clone(), shown);
        }
    }
    let mut m = Map::new();
    m.insert("type".into(), json!(info.name));
    m.insert("id".into(), json!(r.target.id_string()));
    m.insert(
        "label".into(),
        json!(label.unwrap_or_else(|| r.target.id_string())),
    );
    if opts.depth > 0 {
        m.insert("fields".into(), Json::Object(fields));
    }
    Json::Object(m)
}
