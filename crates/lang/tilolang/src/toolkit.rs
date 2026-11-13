//! The package the toolkit provides: its described components, read as the package `ui`. Every module sees
//! it as `ui`, the way it sees the host's package as `host`; the descriptions are the toolkit's own,
//! generated from each widget's Rust definition.

pub use tilo_ui_graph::component::{Accepts, Catalogue, Description, OPENS_PROP, Offered, Type};

pub const UI_APP: &str = "tilo.ui";
pub const UI_ALIAS: &str = "ui";

/// The prefix of a component node's kind: `ui.ListRow`.
pub const KIND_PREFIX: &str = "ui.";

#[must_use]
pub fn catalogue() -> &'static Catalogue {
    tilo_ui_graph::component::catalogue()
}

#[must_use]
pub fn kind(name: &str) -> String {
    format!("{KIND_PREFIX}{name}")
}

/// The language's enums the toolkit declares: each closed set whose cases carry fields (`ui.Shape`), and
/// each set of roles or tokens one of their fields names (`ui.Ink`).
#[must_use]
pub fn enums() -> Vec<crate::resolve::EnumInfo> {
    use crate::resolve::{EnumInfo, FieldInfo, VariantInfo};
    let catalogue = catalogue();
    let mut out: Vec<EnumInfo> = Vec::new();
    let variant = |name: &str, doc: &str, fields: Vec<FieldInfo>| VariantInfo {
        name: name.to_string(),
        // A case and its fields are known by their names: the toolkit reads them so.
        id: name.to_string(),
        doc: doc.to_string(),
        label: crate::resolve::variant_label(name, doc),
        icon: None,
        tone: None,
        fields,
    };
    let mut choices: Vec<(String, Vec<String>)> = Vec::new();
    for described in &catalogue.enums {
        let mut fields = |case: &tilo_ui_graph::component::CaseDescription| {
            case.fields
                .iter()
                .map(|field| {
                    collect_choices(&field.ty, &mut choices);
                    FieldInfo {
                        name: field.name.clone(),
                        id: field.name.clone(),
                        ty: lang_type(&field.ty),
                        merge: crate::types::MergeKind::Register,
                        doc: field.doc.clone(),
                        default: field
                            .default
                            .as_ref()
                            .map(|value| default_json(&field.ty, value)),
                    }
                })
                .collect::<Vec<_>>()
        };
        let variants = described
            .cases
            .iter()
            .map(|case| variant(&case.name, &case.doc, fields(case)))
            .collect();
        out.push(EnumInfo {
            key: key(&described.name),
            id: format!("{UI_APP}.{}", described.name),
            doc: described.doc.clone(),
            variants,
        });
    }
    for (name, cases) in choices {
        out.push(EnumInfo {
            key: key(&name),
            id: format!("{UI_APP}.{name}"),
            doc: String::new(),
            variants: cases
                .iter()
                .map(|case| variant(case, "", Vec::new()))
                .collect(),
        });
    }
    out
}

/// The key of the toolkit's type `name`: `ui.Shape`.
#[must_use]
pub fn key(name: &str) -> crate::types::TypeKey {
    crate::types::TypeKey {
        app: UI_APP.to_string(),
        name: name.to_string(),
    }
}

fn lang_type(ty: &Type) -> crate::types::Ty {
    use crate::types::Ty;
    match ty {
        Type::Text | Type::Picture | Type::Icon | Type::Resource => Ty::Text,
        Type::Int => Ty::Int,
        Type::Float => Ty::Float,
        Type::Bool => Ty::Bool,
        Type::Principal => Ty::Principal,
        Type::Time => Ty::Time,
        Type::RichText => Ty::RichText,
        Type::Option(inner) => Ty::Option(Box::new(lang_type(inner))),
        Type::List(inner) | Type::Set(inner) => Ty::List(Box::new(lang_type(inner))),
        Type::Map { value, .. } => Ty::Map(Box::new(lang_type(value))),
        Type::Choice { name, .. } | Type::Cases { name } => Ty::Enum(key(name)),
        // Whichever enum or record the bound field names, which a description does not name.
        Type::Action | Type::Enum | Type::Link => Ty::Error,
    }
}

fn collect_choices(ty: &Type, into: &mut Vec<(String, Vec<String>)>) {
    match ty {
        Type::Choice { name, cases } if !into.iter().any(|(n, _)| n == name) => {
            into.push((name.clone(), cases.clone()));
        }
        Type::Option(inner) | Type::List(inner) | Type::Set(inner) => collect_choices(inner, into),
        Type::Map { value, .. } => collect_choices(value, into),
        _ => {}
    }
}

fn default_json(ty: &Type, value: &tilo_ui_graph::component::Value) -> serde_json::Value {
    use tilo_ui_graph::component::Value;
    match value {
        Value::Choice(case) => {
            let Type::Choice { name, .. } = ty else {
                return serde_json::Value::Null;
            };
            crate::resolve::enum_json(&format!("{UI_APP}.{name}"), case, &serde_json::Map::new())
        }
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::json!(i),
        Value::Float(f) => serde_json::json!(f),
        Value::Text(t) => serde_json::Value::String(t.clone()),
        _ => serde_json::Value::Null,
    }
}
