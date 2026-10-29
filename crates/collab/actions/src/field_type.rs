//! The type of a field or value, the same whichever language declared it.
//!
//! A Rust model derives it (`#[derive(Model)]`, `#[derive(JsonSchema)]`), the
//! tilolang compiler emits it, and a [`Surface`](crate::surface::Surface)
//! carries it for every stored field. Records, enums and unions with a name
//! are declared once, in the surface's [`types`](crate::surface::Surface::types),
//! and referred to by name, so each side renders the other's declarations
//! (`tilolang describe` prints a native app's resources as tilolang) and
//! names the other's types (`use so.tilo.chat as chat`).

use serde::{Deserialize, Deserializer, Serialize};

/// A field's or value's type. See the module docs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    /// A value this vocabulary does not describe: any JSON value.
    #[default]
    Any,
    Text,
    /// Formatted text that references resources (tilo-doc's rich text).
    RichText,
    Int,
    Float,
    Bool,
    /// A calendar day, held as ISO text `YYYY-MM-DD`.
    Date,
    /// An instant, held as ISO-8601 text.
    Time,
    /// An account of some service, held as its URI (`mailto:jane@x.com`).
    Identifier,
    /// Who did something: a person or an agent.
    Principal,
    /// A reference to a resource of this kind; of any kind when empty.
    Resource(String),
    /// A record or enum declared in the surface's types.
    Named(String),
    /// A value or none.
    Option(Box<FieldType>),
    List(Box<FieldType>),
    /// Distinct values.
    Set(Box<FieldType>),
    /// Ordered items, each under an id of its own.
    Collection(Box<FieldType>),
    /// Values under text keys.
    Map(Box<FieldType>),
    /// A value of any of these types, at least two.
    Union(Vec<FieldType>),
    /// The platform's reactions: changed only by the react and unreact
    /// field edits, which store their invoker.
    Reactions,
    /// An outbox of these entries: what a change queues there reaches
    /// outside the record once delivered.
    Outbox(Box<FieldType>),
}

impl FieldType {
    /// Whether it describes no type: any value.
    #[must_use]
    pub const fn is_any(&self) -> bool {
        matches!(self, Self::Any)
    }

    /// Whether it is an outbox.
    #[must_use]
    pub const fn is_outbox(&self) -> bool {
        matches!(self, Self::Outbox(_))
    }

    /// Whether it is the platform's reactions.
    #[must_use]
    pub const fn is_reactions(&self) -> bool {
        matches!(self, Self::Reactions)
    }

    /// The type of a field a surface written before this vocabulary
    /// described by its Rust type's text (`Outbox<Upload>`,
    /// `ui::tilo_resource::Reactions`): what receipt still tells from it.
    #[must_use]
    pub fn from_rust_text(text: &str) -> Self {
        let head = text.split('<').next().unwrap_or_default().trim();
        match head.rsplit("::").next() {
            Some("Outbox") if text.contains('<') => Self::Outbox(Box::new(Self::Any)),
            Some("Reactions") => Self::Reactions,
            _ => Self::Any,
        }
    }

    /// The type of a value of the JSON Schema `schema`, as far as the schema
    /// tells: what a parameter published only as its schema takes.
    #[must_use]
    pub fn from_schema(schema: &serde_json::Value) -> Self {
        use serde_json::Value;
        let Some(node) = schema.as_object() else {
            return Self::Any;
        };
        if let Some(kind) = node
            .get(crate::params::RESOURCE_KIND_ANNOTATION)
            .and_then(Value::as_str)
        {
            return Self::Resource(kind.to_owned());
        }
        for key in ["anyOf", "oneOf"] {
            if let Some(alternatives) = node.get(key).and_then(Value::as_array) {
                let is_null = |alternative: &Value| {
                    alternative.get("type").and_then(Value::as_str) == Some("null")
                };
                let present: Vec<&Value> =
                    alternatives.iter().filter(|item| !is_null(item)).collect();
                if let [one] = present.as_slice()
                    && present.len() < alternatives.len()
                {
                    return Self::Option(Box::new(Self::from_schema(one)));
                }
                return Self::Any;
            }
        }
        let types: Vec<&str> = match node.get("type") {
            Some(Value::String(one)) => vec![one.as_str()],
            Some(Value::Array(many)) => many.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        let nullable = types.contains(&"null");
        let present: Vec<&str> = types.into_iter().filter(|ty| *ty != "null").collect();
        let ty = match present.as_slice() {
            ["string"] => match node.get("format").and_then(Value::as_str) {
                Some("date") => Self::Date,
                Some("date-time") => Self::Time,
                Some("uri") => Self::Identifier,
                _ => Self::Text,
            },
            ["integer"] => Self::Int,
            ["number"] => Self::Float,
            ["boolean"] => Self::Bool,
            ["array"] => {
                let item = node.get("items").map_or(Self::Any, Self::from_schema);
                if node.get("uniqueItems").and_then(Value::as_bool) == Some(true) {
                    Self::Set(Box::new(item))
                } else {
                    Self::List(Box::new(item))
                }
            }
            ["object"] => match node.get("additionalProperties") {
                Some(item @ Value::Object(_)) if node.get("properties").is_none() => {
                    Self::Map(Box::new(Self::from_schema(item)))
                }
                _ => Self::Any,
            },
            _ => Self::Any,
        };
        if nullable && ty != Self::Any {
            Self::Option(Box::new(ty))
        } else {
            ty
        }
    }
}

/// A field type as a surface file holds it: the vocabulary, or the Rust
/// text a file written before it holds, read as [`FieldType::from_rust_text`].
pub(crate) fn field_type_or_text<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<FieldType, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Written {
        Typed(FieldType),
        Text(String),
    }
    Ok(match Written::deserialize(deserializer)? {
        Written::Typed(ty) => ty,
        Written::Text(text) => FieldType::from_rust_text(&text),
    })
}

/// A record or enum with a name, declared once in a surface's types.
///
/// An enum has cases; a record has fields and no cases.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedType {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub doc: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<TypeField>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cases: Vec<TypeCase>,
}

impl NamedType {
    /// Whether it is an enum: it has cases.
    #[must_use]
    pub const fn is_enum(&self) -> bool {
        !self.cases.is_empty()
    }
}

/// A field of a named record or of an enum case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeField {
    pub name: String,
    #[serde(rename = "type", deserialize_with = "field_type_or_text")]
    pub ty: FieldType,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
}

/// A case of a named enum, with the fields its value carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeCase {
    pub name: String,
    /// How it reads, for people.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<TypeField>,
}

/// The named types a surface declares, gathered while its fields' types are
/// worked out: each declared once under one name, however often it is used.
#[derive(Debug, Default)]
pub struct TypeSet {
    /// The declared types, with the identity (a Rust type's path) each was
    /// declared for.
    declared: Vec<(String, NamedType)>,
}

impl TypeSet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The type named `name` declared for `identity` (a Rust type's path),
    /// its body made by `build` the first time it is asked for. A different
    /// type wanting a name already taken is numbered (`Step2`).
    pub fn named(
        &mut self,
        identity: &str,
        name: &str,
        build: impl FnOnce(&mut Self) -> NamedType,
    ) -> FieldType {
        if let Some((_, declared)) = self.declared.iter().find(|(id, _)| id == identity) {
            return FieldType::Named(declared.name.clone());
        }
        let mut unique = name.to_owned();
        let mut number = 2_u32;
        while self.declared.iter().any(|(_, ty)| ty.name == unique) {
            unique = format!("{name}{number}");
            number = number.saturating_add(1);
        }
        // Declared before its body is built, so a type that holds itself
        // refers to its own name.
        let at = self.declared.len();
        self.declared.push((
            identity.to_owned(),
            NamedType {
                name: unique.clone(),
                ..NamedType::default()
            },
        ));
        let mut body = build(self);
        body.name.clone_from(&unique);
        if let Some((_, slot)) = self.declared.get_mut(at) {
            *slot = body;
        }
        FieldType::Named(unique)
    }

    /// The declared types, by name.
    #[must_use]
    pub fn into_types(self) -> Vec<NamedType> {
        let mut types: Vec<NamedType> = self.declared.into_iter().map(|(_, ty)| ty).collect();
        types.sort_by(|a, b| a.name.cmp(&b.name));
        types
    }
}

/// The words of a case or field name as people read them: `in_progress` is
/// "In progress".
#[must_use]
pub fn label_of(name: &str) -> String {
    let words = name.replace(['_', '-'], " ");
    let mut chars = words.trim().chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_type_reads_from_the_vocabulary_or_older_rust_text() {
        #[derive(Deserialize)]
        struct Field {
            #[serde(deserialize_with = "field_type_or_text")]
            ty: FieldType,
        }
        let read = |json: &str| {
            serde_json::from_str::<Field>(json).map_or(FieldType::Any, |field| field.ty)
        };
        assert_eq!(read(r#"{"ty": "text"}"#), FieldType::Text);
        assert_eq!(
            read(r#"{"ty": {"option": {"named": "Channel"}}}"#),
            FieldType::Option(Box::new(FieldType::Named("Channel".into())))
        );
        assert_eq!(read(r#"{"ty": "String"}"#), FieldType::Any);
        assert_eq!(
            read(r#"{"ty": "ui::tilo_resource::Reactions"}"#),
            FieldType::Reactions
        );
        assert!(read(r#"{"ty": "Outbox<Upload>"}"#).is_outbox());
    }

    #[test]
    fn a_type_used_twice_is_declared_once_and_names_stay_unique() {
        let mut types = TypeSet::new();
        let record = |name: &str| NamedType {
            name: name.into(),
            ..NamedType::default()
        };
        let first = types.named("a::Step", "Step", |_| record("Step"));
        let again = types.named("a::Step", "Step", |_| unreachable!());
        let other = types.named("b::Step", "Step", |_| record("Step"));
        assert_eq!(first, again);
        assert_eq!(other, FieldType::Named("Step2".into()));
        let names: Vec<String> = types.into_types().into_iter().map(|ty| ty.name).collect();
        assert_eq!(names, ["Step", "Step2"]);
    }

    #[test]
    fn a_schema_tells_what_it_can() {
        let ty = FieldType::from_schema(&serde_json::json!({"type": ["string", "null"]}));
        assert_eq!(ty, FieldType::Option(Box::new(FieldType::Text)));
        let ty = FieldType::from_schema(
            &serde_json::json!({"type": "array", "items": {"type": "integer"}}),
        );
        assert_eq!(ty, FieldType::List(Box::new(FieldType::Int)));
    }
}
