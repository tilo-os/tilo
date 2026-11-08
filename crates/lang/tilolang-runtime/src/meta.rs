//! The metadata the runtime reads is the compiler's own `tilolang.meta` (`PackageMeta`).
//! This module re-exports it and adds what only the runtime needs: how a field's merge and
//! type become a [`FieldKind`], and how a type travels through wasm.

pub use tilolang::meta::{
    ActionMeta, AnchorMeta, DefaultMeta, DefaultViewMeta, EnumMeta, FieldMeta, FnMeta,
    NamedRootMeta, PackageMeta, ParamMeta, ProposalMeta, Reach, StateMeta, TriggerArgMeta,
    TriggerMeta, TriggerWhenMeta, TypeMeta, VariantMeta, ViewMeta,
};
pub use tilolang::types::{EffectClass, MergeKind, Ty, TypeKey};

/// Packages are described by the compiler's metadata.
pub type Meta = PackageMeta;

/// How a field merges concurrent edits, and so how a store keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum FieldKind {
    Register,
    Counter,
    Text,
    /// Rich text (`RichText`): tilo-doc's rich text under the field, merged by splices and
    /// marks.
    RichText,
    Collection,
    Set,
    Map,
}

/// How a value of a type is passed to and from wasm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repr {
    I64,
    F64,
    Bool,
    Handle,
}

/// Parse a type as written in metadata; unqualified record names belong to `app`.
pub fn parse_ty(text: &str, app: &str) -> Result<Ty, String> {
    Ty::parse_meta(text, app).ok_or_else(|| format!("cannot read the type `{text}`"))
}

/// What a register of type `ty` reads before anything wrote it: `default`, the field's declared
/// one, else its type's own.
///
/// # Errors
///
/// When the type has no value to start from (a record, a union, an action): a read of the field
/// traps until something writes it, so whatever makes such a record outside the language's
/// constructors (a seed, a fixture) must give it one. A person, a date, a time or an identifier
/// reads as its empty value, as the index compares it.
pub fn unwritten_value(
    ty: &Ty,
    default: Option<&serde_json::Value>,
) -> Result<crate::value::Value, String> {
    crate::host_res::declared_default(ty, default)
}

pub fn repr(ty: &Ty) -> Repr {
    match ty {
        Ty::Int | Ty::Choice(_) => Repr::I64,
        Ty::Float | Ty::Length(_) => Repr::F64,
        Ty::Bool => Repr::Bool,
        _ => Repr::Handle,
    }
}

/// The merge kind of a field of type `ty`.
pub fn field_kind(field: &FieldMeta, ty: &Ty) -> FieldKind {
    match (ty, field.merge) {
        (Ty::Collection(_), _) => FieldKind::Collection,
        (Ty::Set(_), _) => FieldKind::Set,
        (Ty::Map(_), _) => FieldKind::Map,
        (Ty::RichText, _) => FieldKind::RichText,
        (_, MergeKind::Counter) => FieldKind::Counter,
        (_, MergeKind::Text) => FieldKind::Text,
        (_, MergeKind::Register | MergeKind::Structural) => FieldKind::Register,
    }
}

/// The record type a container or option type holds, if its element is a record.
pub fn element_record(ty: &Ty) -> Option<&TypeKey> {
    match ty {
        Ty::Collection(t) | Ty::Set(t) | Ty::Map(t) | Ty::List(t) | Ty::Option(t) => match &**t {
            Ty::Record(key) => Some(key),
            _ => None,
        },
        _ => None,
    }
}
