//! What an app declares: its resource types, the fields of each, and which types nest in which.
//! Nothing here says how a resource is stored; that is the store's alone.

use tilo_actions::surface::{FieldType, NamedType, Surface};

/// The resource types one app declares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Schema {
    pub types: Vec<Type>,
}

/// One resource type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Type {
    /// What the store and every query name it by.
    pub name: String,
    /// The fields the index holds: what a filter, a sort or a count reads.
    pub fields: Vec<Field>,
    /// The types of the resources nested in one of this type, each under the field holding them.
    pub nested: Vec<(String, String)>,
}

/// One indexed field of a resource type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub kind: FieldKind,
}

/// How the index holds a field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// One value, compared, sorted and ranged.
    Scalar,
    /// Distinct values, filtered by membership.
    Set,
}

impl FieldKind {
    /// How the index holds a field of type `ty`; `None` for one it does not hold: rich text,
    /// lists, maps, nested resources and resources, the platform's reactions and outboxes. Those
    /// are the resource's own content, read from the resource.
    #[must_use]
    pub fn of(ty: &FieldType, enums: &dyn Fn(&str) -> bool) -> Option<Self> {
        match ty {
            FieldType::Option(inner) => Self::of(inner, enums),
            FieldType::Set(inner) => scalar(inner, enums).then_some(Self::Set),
            other => scalar(other, enums).then_some(Self::Scalar),
        }
    }
}

/// Whether a value of `ty` is one scalar: a text, number, flag, date, instant, account, actor,
/// reference to a resource, or a case of an enum without fields.
fn scalar(ty: &FieldType, enums: &dyn Fn(&str) -> bool) -> bool {
    match ty {
        FieldType::Text
        | FieldType::Int
        | FieldType::Float
        | FieldType::Bool
        | FieldType::Date
        | FieldType::Time
        | FieldType::Identifier
        | FieldType::Actor
        | FieldType::Resource(_)
        | FieldType::Duration => true,
        FieldType::Named(name) => enums(name),
        FieldType::Option(inner) => scalar(inner, enums),
        _ => false,
    }
}

impl Schema {
    /// The resource types `surface` declares: each resource type it names, with its fields, and
    /// the types nested in it as collections of them.
    #[must_use]
    pub fn from_surface(surface: &Surface) -> Self {
        let enums = |name: &str| {
            surface.types.iter().any(|ty| {
                ty.name == name
                    && ty.is_enum()
                    && ty.cases.iter().all(|case| case.fields.is_empty())
            })
        };
        let types = surface
            .types
            .iter()
            .filter(|ty| !ty.is_enum())
            .map(|ty| Type::of(ty, &enums))
            .collect();
        Self { types }
    }

    /// The type named `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Type> {
        self.types.iter().find(|ty| ty.name == name)
    }
}

impl Type {
    fn of(declared: &NamedType, enums: &dyn Fn(&str) -> bool) -> Self {
        let mut fields = Vec::new();
        let mut nested = Vec::new();
        for field in &declared.fields {
            match &field.ty {
                FieldType::Collection(item) => {
                    if let FieldType::Named(name) = item.as_ref() {
                        nested.push((field.name.clone(), name.clone()));
                    }
                }
                ty => {
                    if let Some(kind) = FieldKind::of(ty, enums) {
                        fields.push(Field {
                            name: field.name.clone(),
                            kind,
                        });
                    }
                }
            }
        }
        Self {
            name: declared.name.clone(),
            fields,
            nested,
        }
    }

    /// The indexed field `name`.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|field| field.name == name)
    }
}
