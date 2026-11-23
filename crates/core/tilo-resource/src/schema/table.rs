//! What `#[derive(JsonSchema)]` emits: a type's schema as constant data, and
//! the one builder that turns it into JSON. A derived type carries no schema
//! code of its own, only its table.

use serde_json::{Value, json};

use super::{
    adjacent, constant, describe, external, internal, object, one_of, range, string_enum, tuple,
};

/// A type's schema: its description and its shape.
pub struct TypeSchema {
    /// The type's doc comment, plain.
    pub doc: &'static str,
    pub body: Body,
}

/// The shape of a type's serde JSON form.
pub enum Body {
    /// `#[serde(transparent)]`: the field's schema, as it is.
    Transparent(fn() -> Value),
    /// A struct with fields.
    Struct(Content),
    /// A unit struct: `null`.
    Unit,
    /// An enum: one alternative per variant, tagged as serde tags it. An
    /// externally tagged enum of undocumented unit variants (`plain`) is a
    /// string enumeration.
    Enum {
        tagging: Tagging,
        plain: bool,
        variants: &'static [Variant],
    },
}

/// The fields of a struct or variant.
pub enum Content {
    /// Named fields: a closed object.
    Object(&'static [Field]),
    /// One unnamed field: its schema.
    Newtype(fn() -> Value),
    /// Several unnamed fields: an array of that length.
    Tuple(usize),
}

/// A named field.
pub struct Field {
    /// Its JSON name.
    pub name: &'static str,
    /// Its type's schema.
    pub schema: fn() -> Value,
    /// Whether serde needs it present.
    pub required: bool,
    /// Its doc comment, plain.
    pub doc: &'static str,
    /// `#[schema(range(..))]`: the bounds narrowing its type's.
    pub range: Option<Bounds>,
}

/// A field's `(minimum, maximum)`, either side optional.
pub type Bounds = fn() -> (Option<Value>, Option<Value>);

/// An enum variant.
pub struct Variant {
    /// Its JSON name.
    pub name: &'static str,
    /// Its doc comment, plain.
    pub doc: &'static str,
    /// Its fields; `None` for a unit variant.
    pub content: Option<Content>,
}

/// How an enum's variants are told apart in JSON.
pub enum Tagging {
    External,
    Internal(&'static str),
    Adjacent(&'static str, &'static str),
}

/// The JSON Schema `schema` describes.
#[must_use]
pub fn build(schema: &TypeSchema) -> Value {
    match &schema.body {
        Body::Transparent(inner) => inner(),
        Body::Struct(content) => describe(content_schema(content), schema.doc),
        Body::Unit => describe(json!({"type": "null"}), schema.doc),
        Body::Enum {
            tagging,
            plain,
            variants,
        } => describe(enum_schema(tagging, *plain, variants), schema.doc),
    }
}

fn content_schema(content: &Content) -> Value {
    match content {
        Content::Object(fields) => object(
            fields
                .iter()
                .map(|field| {
                    let mut schema = (field.schema)();
                    if let Some(bounds) = field.range {
                        let (minimum, maximum) = bounds();
                        schema = range(schema, minimum, maximum);
                    }
                    (field.name, describe(schema, field.doc), field.required)
                })
                .collect(),
        ),
        Content::Newtype(inner) => inner(),
        Content::Tuple(len) => tuple(*len),
    }
}

fn enum_schema(tagging: &Tagging, plain: bool, variants: &[Variant]) -> Value {
    if plain {
        let names: Vec<&str> = variants.iter().map(|variant| variant.name).collect();
        return string_enum(&names);
    }
    one_of(
        variants
            .iter()
            .map(|variant| {
                let content = variant.content.as_ref().map(content_schema);
                let alternative = match (tagging, content) {
                    (Tagging::External, None) => constant(variant.name),
                    (Tagging::External, Some(content)) => external(variant.name, content),
                    (Tagging::Internal(tag), None) => {
                        internal(tag, variant.name, object(Vec::new()))
                    }
                    (Tagging::Internal(tag), Some(content)) => internal(tag, variant.name, content),
                    (Tagging::Adjacent(tag, key), content) => {
                        adjacent(tag, key, variant.name, content)
                    }
                };
                describe(alternative, variant.doc)
            })
            .collect(),
    )
}
