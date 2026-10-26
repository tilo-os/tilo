//! The metadata the runtime reads is the compiler's own `tilolang.meta` (`PackageMeta`).
//! This module re-exports it and adds what only the runtime needs: how a field's merge and
//! type become a [`FieldKind`], and how a type travels through wasm.

pub use tilolang::meta::{
    ActionMeta, AnchorMeta, DefaultViewMeta, EnumMeta, FieldMeta, FlowMeta, FnMeta, PackageMeta,
    ParamMeta, ProposalMeta, Reach, StateMeta, TypeMeta, VariantMeta, ViewMeta,
};
pub use tilolang::types::{EffectClass, MergeKind, Ty, TypeKey};

/// Packages are described by the compiler's metadata.
pub type Meta = PackageMeta;

/// How a field merges concurrent edits (implementation design 3.2 and 4.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum FieldKind {
    Register,
    Counter,
    Text,
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
///
/// # Errors
///
/// A message naming the malformed type.
pub fn parse_ty(text: &str, app: &str) -> Result<Ty, String> {
    Ty::parse_meta(text, app).ok_or_else(|| format!("cannot read the type `{text}`"))
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
