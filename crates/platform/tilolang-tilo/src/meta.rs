//! What the platform reads of a package's metadata (`tilolang.meta`).
//!
//! The metadata types are the runtime's ([`tilolang_runtime::Meta`]); this module adds what the
//! platform derives from them: the record schemas, the resource kinds and the subject of an
//! action.

use tilolang_runtime::meta::{
    EffectClass, FieldKind, FieldMeta, Meta, ParamMeta, Ty, TypeMeta, element_record, field_kind,
    parse_ty,
};

use crate::schema::{ColumnKind, ColumnSchema, FieldSchema, MergeKind, TypeSchema, lookup_in};
use tilolang::meta::FilterKind;

/// The contract kind an effect class stands for.
#[must_use]
pub const fn action_kind(class: EffectClass) -> tilo_actions::ActionKind {
    match class {
        EffectClass::Read => tilo_actions::ActionKind::Read,
        EffectClass::Undoable => tilo_actions::ActionKind::Undoable,
        EffectClass::Outside => tilo_actions::ActionKind::Outside,
    }
}

/// Every type `meta` names that the platform reads (each field's, enum case field's, action
/// parameter's and search filter's) and that does not parse, each as where it is, the type and
/// why. A package that has any is refused where it is loaded ([`crate::bundle`]), so what is
/// derived from a loaded package's metadata (schemas, columns, the surface) parses every type it
/// reads.
#[must_use]
pub fn unreadable_types(meta: &Meta) -> Vec<String> {
    let mut out = Vec::new();
    let mut check = |at: String, ty: &str| {
        if let Err(why) = parse_ty(ty.trim(), &meta.app) {
            out.push(format!("{at}: {why}"));
        }
    };
    for ty in &meta.types {
        for field in &ty.fields {
            check(format!("{}.{}", ty.name, field.name), &field.ty);
        }
        for filter in &ty.search.filters {
            // A boolean or predicate filter is a yes/no column: its type is not read.
            if !matches!(filter.kind, FilterKind::Bool | FilterKind::Predicate) {
                check(format!("{} filter {}", ty.name, filter.key), &filter.ty);
            }
        }
    }
    for declared in &meta.enums {
        for variant in &declared.variants {
            for field in &variant.fields {
                check(
                    format!("{}.{}.{}", declared.name, variant.name, field.name),
                    &field.ty,
                );
            }
        }
    }
    for action in &meta.actions {
        for param in &action.params {
            check(format!("{}({})", action.name, param.name), &param.ty);
        }
    }
    out
}

/// Derived facts of a package's metadata.
pub trait MetaExt {
    /// The declared type named `name` (a qualified name's last segment), or whose id is `name`.
    fn type_named(&self, name: &str) -> Option<&TypeMeta>;

    /// The record schemas of every declared type, ready to register.
    fn schemas(&self) -> Vec<TypeSchema>;

    /// The record schemas of every declared type, written out: what an index of the package's
    /// records was built for, and is built anew when it differs.
    fn schema_text(&self) -> String;

    /// Register the schemas of every declared type, and the ids the app had before; peers that
    /// replay this package's actions resolve its records through them.
    fn register_schemas(&self);

    /// The resource kind of the type `type_name`: `tl.<app>.<Type>`.
    fn kind_of(&self, type_name: &str) -> String;

    /// The kind a package-level action (one with no record parameter) is published under.
    fn package_kind(&self) -> String;

    /// The type an action is on: that of its first parameter, when it is a declared record.
    fn subject_of(&self, params: &[ParamMeta]) -> Option<&TypeMeta>;
}

fn field_schema(meta: &Meta, field: &FieldMeta) -> FieldSchema {
    // A loaded package's field types all parse (`unreadable_types`, checked where it is
    // loaded); a hand-built one that does not is kept whole, as a composite register.
    // optional-decode: checked where the package is loaded
    let Ok(ty) = parse_ty(&field.ty, &meta.app) else {
        return FieldSchema::new(&field.id, MergeKind::Register).composite();
    };
    let mut schema = match field_kind(field, &ty) {
        FieldKind::Counter => FieldSchema::new(&field.id, MergeKind::Counter),
        FieldKind::Text => FieldSchema::new(&field.id, MergeKind::Text),
        FieldKind::RichText => FieldSchema::new(&field.id, MergeKind::RichText),
        FieldKind::Set => FieldSchema::new(&field.id, MergeKind::Set),
        FieldKind::Map => FieldSchema::new(&field.id, MergeKind::Map),
        FieldKind::Collection => {
            let schema = FieldSchema::new(&field.id, MergeKind::Collection);
            let item = element_record(&ty).and_then(|key| {
                if key.app == meta.app {
                    meta.type_named(&key.name)
                        .map(|declared| declared.id.clone())
                } else {
                    lookup_in(&key.app, &key.name).map(|found| found.id.clone())
                }
            });
            match item {
                Some(id) => schema.of_record(id),
                None => schema,
            }
        }
        FieldKind::Register => {
            let schema = FieldSchema::new(&field.id, MergeKind::Register);
            if matches!(
                ty,
                Ty::Int
                    | Ty::Float
                    | Ty::Bool
                    | Ty::Text
                    | Ty::Date
                    | Ty::Time
                    | Ty::Identifier
                    | Ty::Choice(_)
            ) {
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

/// The kind of a column holding a value of `ty`, and the cases it can hold when they are
/// known: `None` for a type no column holds.
fn kind_of_ty(meta: &Meta, ty: &Ty) -> Option<(ColumnKind, Vec<String>)> {
    Some(match ty {
        Ty::Option(inner) | Ty::Set(inner) => return kind_of_ty(meta, inner),
        Ty::Bool => (ColumnKind::Bool, Vec::new()),
        Ty::Int => (ColumnKind::Int, Vec::new()),
        Ty::Float => (ColumnKind::Float, Vec::new()),
        Ty::Text => (ColumnKind::Text, Vec::new()),
        Ty::Date => (ColumnKind::Date, Vec::new()),
        Ty::Time => (ColumnKind::Time, Vec::new()),
        Ty::Identifier => (ColumnKind::Ident, Vec::new()),
        Ty::Record(_) | Ty::Union(_) => (ColumnKind::Ref, Vec::new()),
        Ty::Enum(key) => {
            let cases = meta
                .enums
                .iter()
                .find(|e| key.app == meta.app && e.name == key.name)
                .map(|e| e.variants.iter().map(|v| v.id.clone()).collect())
                .unwrap_or_default();
            (ColumnKind::Enum, cases)
        }
        _ => return None,
    })
}

/// The filter columns of the type `ty`, whose fields' schemas are `fields`: one per filter,
/// then one per other field a `filter` compares (a register of a scalar, a counter).
fn columns(meta: &Meta, ty: &TypeMeta, fields: &[FieldSchema]) -> Vec<ColumnSchema> {
    let mut out = Vec::new();
    for f in &ty.search.filters {
        // A filter on the time it is asked at holds of different items as time passes: it is
        // answered from the columns of the fields it reads, never kept as a column of its own.
        if f.pred
            .as_ref()
            .is_some_and(tilolang::pred::Pred::reads_clock)
        {
            continue;
        }
        // optional-decode: a loaded package's filter types all parse (`unreadable_types`).
        let parsed = parse_ty(&f.ty, &meta.app).ok();
        let (kind, cases) = match f.kind {
            FilterKind::Bool | FilterKind::Predicate => (ColumnKind::Bool, Vec::new()),
            _ => match parsed.as_ref().and_then(|t| kind_of_ty(meta, t)) {
                Some(found) => found,
                None => continue,
            },
        };
        out.push(ColumnSchema {
            name: f.field.clone().unwrap_or_else(|| format!("={}", f.key)),
            field: f.field.clone(),
            kind,
            many: f.many,
            pred: f.pred.clone(),
            filter: true,
            cases,
        });
    }
    for (field, schema) in ty.fields.iter().zip(fields) {
        if out
            .iter()
            .any(|c| c.field.as_deref() == Some(schema.id.as_str()))
        {
            continue;
        }
        let columned = match schema.kind {
            MergeKind::Register => !matches!(
                parse_ty(&field.ty, &meta.app),
                Ok(Ty::List(_) | Ty::Tuple(_) | Ty::Map(_) | Ty::Result(..) | Ty::Principal)
            ),
            MergeKind::Counter => true,
            _ => false,
        };
        // optional-decode: a loaded package's field types all parse (`unreadable_types`).
        let Some((kind, cases)) = parse_ty(&field.ty, &meta.app)
            .ok()
            .filter(|_| columned)
            .and_then(|t| kind_of_ty(meta, &t))
        else {
            continue;
        };
        out.push(ColumnSchema {
            name: schema.id.clone(),
            field: Some(schema.id.clone()),
            kind,
            many: false,
            pred: None,
            filter: false,
            cases,
        });
    }
    // Every resource's first and last versions, by their time: what a list sorts or filters
    // by when it reads `created(r)` or `updated(r)`.
    for reflected in [
        crate::index::reflect::CREATED,
        crate::index::reflect::UPDATED,
    ] {
        out.push(ColumnSchema {
            name: reflected.to_owned(),
            field: Some(reflected.to_owned()),
            kind: ColumnKind::Time,
            many: false,
            pred: None,
            filter: false,
            cases: Vec::new(),
        });
    }
    out
}

impl MetaExt for Meta {
    fn type_named(&self, name: &str) -> Option<&TypeMeta> {
        if let Some(found) = self.types.iter().find(|ty| ty.id == name) {
            return Some(found);
        }
        // A qualified name (`so.tilo.todo.Item`) names a type of another app.
        let (app, last) = name
            .rsplit_once('.')
            .map_or((None, name), |(app, last)| (Some(app), last));
        if app.is_some_and(|app| app != self.app) {
            return None;
        }
        self.types.iter().find(|ty| ty.name == last)
    }

    fn schemas(&self) -> Vec<TypeSchema> {
        self.types
            .iter()
            .map(|ty| {
                let mut schema = TypeSchema::new(&ty.id, &self.app, &ty.name);
                for field in &ty.fields {
                    schema.fields.push(field_schema(self, field));
                }
                schema.columns = columns(self, ty, &schema.fields);
                schema
            })
            .collect()
    }

    fn schema_text(&self) -> String {
        // The records an index holds include those stored under the app's earlier ids.
        if self.previously.is_empty() {
            format!("{:?}", self.schemas())
        } else {
            format!("{:?} previously {:?}", self.schemas(), self.previously)
        }
    }

    fn register_schemas(&self) {
        for schema in self.schemas() {
            crate::schema::register(schema);
        }
        crate::schema::register_previously(&self.app, &self.previously);
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
