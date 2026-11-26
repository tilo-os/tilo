//! What the platform reads of a package's metadata (`tilolang.meta`).
//!
//! The metadata types are the runtime's ([`tilolang_runtime::Meta`]); this module adds what the
//! platform derives from them: the record schemas, the resource kinds and the target of an
//! action.

use tilolang_runtime::meta::{
    ActionMeta, EffectClass, FieldKind, FieldMeta, Meta, ReaderRule, Ty, TypeMeta, element_record,
    field_kind, parse_ty,
};

use crate::schema::{FieldSchema, MergeKind, TypeSchema, lookup_in};
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

/// The action a package declares to open links, on its root: its host publishes it as the
/// system's `open-link` (`tilo_actions::remote::APPLICATION_OPEN_LINK_ACTION`), which every shell
/// delivers a link of a scheme the app claims through, whether the app is open or not.
pub const OPEN_LINK_ACTION: &str = "open_link";

/// Whether `action` runs a check its project's host approved (`uses check`): the app's
/// `run-approved-check`, which runs as a job its invoker follows and can stop.
#[must_use]
pub fn runs_approved_check(action: &ActionMeta) -> bool {
    action.uses.iter().any(|used| used == "check")
}

/// The name the app's action API offers `action` by: its own, except for the action that runs a
/// check its project's host approved, which every app offers as the workspace's one
/// `run-approved-check` (`tilo_action_services::approved_checks::ACTION_ID`), the verb the broker runs a
/// check for, whatever language the app is written in.
#[must_use]
pub fn published_name(action: &ActionMeta) -> &str {
    if runs_approved_check(action) {
        tilo_action_services::approved_checks::ACTION_ID
    } else {
        &action.name
    }
}

/// The root type whose resources an app opens links on: it declares `open_link` on a root, with
/// the link as its one text argument. `None` for an app that opens no links, and then no
/// `open-link` is offered and its manifest carries none.
#[must_use]
pub fn open_link_root(meta: &Meta) -> Option<&TypeMeta> {
    open_link(meta).map(|(root, _)| root)
}

/// The action an app opens links through, and the root resource it acts on: what a shell hands a
/// link to, whether the app runs or has to start ([`open_link_root`]).
#[must_use]
pub fn open_link(meta: &Meta) -> Option<(&TypeMeta, &ActionMeta)> {
    let action = meta
        .actions
        .iter()
        .find(|action| !action.scenario && action.name == OPEN_LINK_ACTION)?;
    let target = meta.target_of(&action.params)?;
    let takes_link = action
        .params
        .iter()
        .skip(1)
        .any(|param| param.name == tilo_actions::remote::OPEN_LINK_PARAM && param.ty == "Text");
    (takes_link && meta.roots.contains(&target.id)).then_some((target, action))
}

/// The action `meta` declares for a file named `name` whose media type is `media`: a file's kind
/// is the extension its name ends in, the narrower claim, else the media type its name declares.
/// `None` for an app that declares no action for that kind, and then a shell offers it for no
/// file of the kind.
#[must_use]
pub fn action_opening<'a>(
    meta: &'a Meta,
    name: Option<&str>,
    media: &str,
) -> Option<&'a ActionMeta> {
    let extension = name.and_then(|name| name.rsplit_once('.').map(|(_, extension)| extension));
    meta.actions
        .iter()
        .filter(|action| !action.scenario && !action.opens.is_empty())
        .find(|action| {
            action.opens.iter().any(|kind| {
                if kind.contains('/') {
                    kind == media
                } else {
                    extension.is_some_and(|extension| extension.eq_ignore_ascii_case(kind))
                }
            })
        })
}

/// The root type whose resource the file-opening `action` acts on, when `meta` declares it as
/// the action's target and as one of its roots: the record a file is opened into.
#[must_use]
pub fn opening_root<'a>(meta: &'a Meta, action: &ActionMeta) -> Option<&'a TypeMeta> {
    let target = meta.target_of(&action.params)?;
    meta.roots.contains(&target.id).then_some(target)
}

/// Derived facts of a package's metadata.
pub trait MetaExt {
    /// The record schemas of every declared type, ready to register.
    fn schemas(&self) -> Vec<TypeSchema>;

    /// The record schemas of every declared type, written out: what an index of the package's
    /// documents was built for, and is built anew when it differs.
    fn schema_text(&self) -> String;

    /// Register the schemas of every declared type; peers that replay this package's actions
    /// resolve its documents through them.
    fn register_schemas(&self);

    /// The action published as `action` (`new_note`, as a native verb is): the one name a caller
    /// and a window's control run it by. The code of one runs under the id its declaration keeps,
    /// which only the document's changes say.
    fn action_published_as(&self, action: &str) -> Option<&ActionMeta>;

    /// The resource kind of the type `type_name`: `tl.<app>.<Type>`.
    fn kind_of(&self, type_name: &str) -> String;

    /// The kind a package-level action (one with no document parameter) is published under.
    fn package_kind(&self) -> String;

    /// The kind of the document a package keeps its resources in: its first root type's, which its
    /// surface publishes (`tl.<app>.<Root>`), else the package's own.
    fn resource_type(&self) -> String;

    /// The kinds the package's own records are of, as its signed manifest declares them: each
    /// root's, the kind of each of its sealed records (Mail's mailbox, the vault), and each
    /// person's own state of it. An app reads and acts
    /// in its own node only within the kinds it declares, so a record it keeps of a kind left out
    /// would be refused to it.
    fn declared_kinds(&self) -> Vec<String>;
}

/// Whether a field's declared type names a resource: a document, or one inside an option.
fn names_a_resource(ty: &Ty) -> bool {
    match ty {
        Ty::Record(_) | Ty::Union(_) => true,
        Ty::Option(inner) => names_a_resource(inner),
        _ => false,
    }
}

fn field_schema(meta: &Meta, field: &FieldMeta) -> FieldSchema {
    // A loaded package's field types all parse (`unreadable_types`, checked where it is
    // loaded); a hand-built one that does not is kept whole, as a composite register.
    let Ok(ty_of_field) = tilo_failure::ignored!(
        "a field type checked where the package is loaded",
        parse_ty(&field.ty, &meta.app)
    ) else {
        return FieldSchema::new(&field.id, MergeKind::Register).composite();
    };
    let mut schema = match field_kind(field, &ty_of_field) {
        FieldKind::Counter => FieldSchema::new(&field.id, MergeKind::Counter),
        FieldKind::Text => FieldSchema::new(&field.id, MergeKind::Text),
        FieldKind::RichText => FieldSchema::new(&field.id, MergeKind::RichText),
        // Bytes keep the content store's stored form, written whole: a composite register the
        // runtime reads as `Bytes` by the field's kind.
        FieldKind::Bytes => FieldSchema::new(&field.id, MergeKind::Register).composite(),
        FieldKind::Set => FieldSchema::new(&field.id, MergeKind::Set),
        FieldKind::Map => FieldSchema::new(&field.id, MergeKind::Map),
        FieldKind::Collection => {
            let schema = FieldSchema::new(&field.id, MergeKind::Collection);
            let item = element_record(&ty_of_field).and_then(|key| {
                if key.app == meta.app {
                    meta.type_named(&key.name)
                        .map(|declared| declared.id.clone())
                } else {
                    lookup_in(&key.app, &key.name).map(|found| found.id.clone())
                }
            });
            match item {
                Some(id) => schema.of_document(id),
                None => schema,
            }
        }
        FieldKind::Register => {
            let mut schema = FieldSchema::new(&field.id, MergeKind::Register);
            if names_a_resource(&ty_of_field) {
                schema.refers = true;
            }
            if matches!(
                ty_of_field,
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
    schema = schema.of_ty(ty_of_field);
    if let Some(default) = &field.default {
        schema = schema.with_default(default.clone());
    }
    schema
}

impl MetaExt for Meta {
    fn schemas(&self) -> Vec<TypeSchema> {
        self.types
            .iter()
            .map(|ty| {
                let mut schema = TypeSchema::new(&ty.id, &self.app, &ty.name);
                schema.origin.clone_from(&ty.origin);
                schema.title = ty.title.as_ref().and_then(|name| {
                    ty.fields
                        .iter()
                        .find(|field| &field.name == name)
                        .map(|field| field.id.clone())
                });
                schema.untitled.clone_from(&ty.untitled);
                schema.search_kind = ty.search.kind;
                schema.people.clone_from(&ty.search.people);
                schema.readers = ty.readers.as_ref().and_then(|rule| match rule {
                    ReaderRule::Field { field } => ty
                        .fields
                        .iter()
                        .find(|candidate| &candidate.name == field)
                        .map(|candidate| candidate.id.clone()),
                    ReaderRule::Author { .. } | ReaderRule::AuthorAnd { .. } => None,
                });
                for field in &ty.fields {
                    schema.fields.push(field_schema(self, field));
                }
                schema
            })
            .collect()
    }

    fn schema_text(&self) -> String {
        // The documents an index holds include those stored under the app's earlier ids.
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
    }

    fn action_published_as(&self, action: &str) -> Option<&ActionMeta> {
        self.actions
            .iter()
            .find(|declared| !declared.scenario && published_name(declared) == action)
    }

    fn kind_of(&self, type_name: &str) -> String {
        format!("tl.{}.{}", self.app, type_name)
    }

    fn package_kind(&self) -> String {
        format!("tl.{}", self.app)
    }

    fn resource_type(&self) -> String {
        let root = self.roots.first().and_then(|id| {
            self.types
                .iter()
                .find(|declared| declared.id == *id)
                .map(|declared| declared.name.as_str())
        });
        root.map_or_else(|| self.package_kind(), |name| self.kind_of(name))
    }

    fn declared_kinds(&self) -> Vec<String> {
        let mut kinds = vec![self.resource_type()];
        let roots = self.roots.iter().filter_map(|id| {
            self.types
                .iter()
                .find(|declared| declared.id == *id)
                .map(|declared| self.kind_of(&declared.name))
        });
        // Its sealed records: the package's own, and those the system keeps for it at a fixed id.
        let kept_apart = collab_types::sealed_kind(&self.app, "")
            .into_iter()
            .chain(
                collab_types::WORKSPACE_DOCUMENTS
                    .iter()
                    .filter(|kept| kept.app == self.app)
                    .filter_map(|kept| collab_types::sealed_kind(&self.app, kept.fixed?)),
            )
            .map(std::borrow::Cow::into_owned);
        for kind in roots
            .chain(kept_apart)
            .chain([tilolang::PERSONAL_RESOURCE_TYPE.to_owned()])
        {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        kinds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TASKS: &str = r#"app t.tasks "Tasks"

type Task {
    title: Text
    due: Option<Date> = .none
    at: Option<Time> = .none
    who: Option<Actor> = .none
}

type Note {
    title: Text
    body: Text
}
"#;

    fn meta() -> Meta {
        let compiled = tilolang::compile_package_with(
            TASKS,
            &tilolang::IdsLock::new(),
            &[],
            &mut tilolang::SeededEntropy(7),
        )
        .unwrap_or_else(|d| panic!("{d:#?}"));
        compiled.meta
    }

    /// The id the schema of `ty` stores the field `name` under.
    fn id_of(schema: &TypeSchema, name: &str) -> String {
        let ty = meta();
        let ty_name = schema.name.clone();
        let declared = ty
            .types
            .iter()
            .find(|ty| ty.name == ty_name)
            .expect("the type");
        declared
            .fields
            .iter()
            .find(|field| field.name == name)
            .expect("the field")
            .id
            .clone()
    }

    #[test]
    fn a_published_schema_names_every_field_the_type_declares() {
        let schemas = meta().schemas();
        let task = schemas
            .iter()
            .find(|schema| schema.name == "Task")
            .expect("Task");
        let declared = meta();
        let declared = declared
            .types
            .iter()
            .find(|ty| ty.name == "Task")
            .expect("Task");
        let fields: Vec<&str> = task.fields.iter().map(|f| f.id.as_str()).collect();
        let expected: Vec<&str> = declared.fields.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(fields, expected);
        assert_eq!(
            task.field(&id_of(task, "due")).map(|f| f.kind),
            Some(MergeKind::Register)
        );
    }
}
