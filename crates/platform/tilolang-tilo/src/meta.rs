//! What the platform reads of a package's metadata (`tilolang.meta`, implementation design 5).
//!
//! The metadata types are the runtime's ([`tilolang_runtime::Meta`]); this module adds what the
//! platform derives from them: the record schemas, the resource kinds and the subject of an
//! action.

use tilolang_runtime::meta::{EffectClass, FieldKind, FieldMeta, Meta, ParamMeta, Ty, TypeMeta};

use crate::schema::{FieldSchema, MergeKind, TypeSchema, lookup_named};

/// The contract kind an effect class stands for.
#[must_use]
pub const fn action_kind(class: EffectClass) -> tilo_actions::ActionKind {
    match class {
        EffectClass::Read => tilo_actions::ActionKind::Read,
        EffectClass::Undoable => tilo_actions::ActionKind::Undoable,
        EffectClass::Outside => tilo_actions::ActionKind::Outside,
    }
}

/// Derived facts of a package's metadata.
pub trait MetaExt {
    /// The declared type named `name` (a qualified name's last segment), or whose id is `name`.
    fn type_named(&self, name: &str) -> Option<&TypeMeta>;

    /// The record schemas of every declared type, ready to register.
    fn schemas(&self) -> Vec<TypeSchema>;

    /// Register the schemas of every declared type; peers that replay this package's actions
    /// resolve its records through them.
    fn register_schemas(&self);

    /// The resource kind of the type `type_name`: `tl.<app>.<Type>`.
    fn kind_of(&self, type_name: &str) -> String;

    /// The kind a package-level action (one with no record parameter) is published under.
    fn package_kind(&self) -> String;

    /// The type an action is on: that of its first parameter, when it is a declared record.
    fn subject_of(&self, params: &[ParamMeta]) -> Option<&TypeMeta>;
}

fn field_schema(meta: &Meta, field: &FieldMeta) -> FieldSchema {
    let Ok((ty, kind)) = field.resolve() else {
        return FieldSchema::new(&field.id, MergeKind::Register).composite();
    };
    let mut schema = match kind {
        FieldKind::Counter => FieldSchema::new(&field.id, MergeKind::Counter),
        FieldKind::Text => FieldSchema::new(&field.id, MergeKind::Text),
        FieldKind::Set => FieldSchema::new(&field.id, MergeKind::Set),
        FieldKind::Map => FieldSchema::new(&field.id, MergeKind::Map),
        FieldKind::Collection => {
            let schema = FieldSchema::new(&field.id, MergeKind::Collection);
            let item = match &ty {
                Ty::Collection(inner) => match inner.as_ref() {
                    Ty::Named(name) => meta
                        .type_named(name)
                        .map(|declared| declared.id.clone())
                        .or_else(|| lookup_named(name).map(|found| found.id.clone())),
                    _ => None,
                },
                _ => None,
            };
            match item {
                Some(id) => schema.of_record(id),
                None => schema,
            }
        }
        FieldKind::Register => {
            let schema = FieldSchema::new(&field.id, MergeKind::Register);
            if matches!(ty, Ty::Int | Ty::Float | Ty::Bool | Ty::Text) {
                schema
            } else {
                schema.composite()
            }
        }
    };
    if let Some(default) = &field.default {
        schema = schema.with_default(default.clone());
    }
    schema
}

impl MetaExt for Meta {
    fn type_named(&self, name: &str) -> Option<&TypeMeta> {
        let last = name.rsplit('.').next().unwrap_or(name);
        self.types
            .iter()
            .find(|ty| ty.name == last || ty.id == name)
    }

    fn schemas(&self) -> Vec<TypeSchema> {
        self.types
            .iter()
            .map(|ty| {
                let mut schema = TypeSchema::new(&ty.id, &self.app, &ty.name);
                for field in &ty.fields {
                    schema.fields.push(field_schema(self, field));
                }
                schema
            })
            .collect()
    }

    fn register_schemas(&self) {
        for schema in self.schemas() {
            crate::schema::register(schema);
        }
    }

    fn kind_of(&self, type_name: &str) -> String {
        format!("tl.{}.{}", self.app, type_name)
    }

    fn package_kind(&self) -> String {
        format!("tl.{}", self.app)
    }

    fn subject_of(&self, params: &[ParamMeta]) -> Option<&TypeMeta> {
        params.first().and_then(|param| self.type_named(&param.ty))
    }
}
