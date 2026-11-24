//! The founding an app's record is derived from: what every holder makes of it alike.
//!
//! An app's record holds its content alone. Two devices that make the same record before they
//! sync (a person's vault opened on a phone and a laptop at once) would each make the containers
//! its fields keep their items in, and the items one of them adds first would land in a
//! container the merge leaves behind. So the containers come first, in one change every holder
//! derives from the record's kind and id alone: an actor derived from them makes it at time zero.
//! The store admits that change only as the one it derives itself ([`structure`]); the app's
//! own first values follow as any change of its.

use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{AutoCommit, ObjType, ROOT};
use tilo_actions::surface::{FieldType, Surface};

/// The containers at the root of `surface`'s record that its top-level fields keep their items
/// in, by field name: a collection or map in a map, a list or set in a list, a record of its own
/// in a map: what [`structure`] makes.
#[must_use]
pub fn containers(surface: &Surface) -> Vec<(String, ObjType)> {
    let mut containers: Vec<(String, ObjType)> = surface
        .fields
        .iter()
        .filter(|field| !field.path.contains('.'))
        .filter_map(|field| {
            let container = match &field.ty {
                FieldType::Collection(_) | FieldType::Map(_) => ObjType::Map,
                FieldType::List(_) | FieldType::Set(_) => ObjType::List,
                FieldType::Named(_) if field.stored == "record" => ObjType::Map,
                _ => return None,
            };
            Some((field.path.clone(), container))
        })
        .collect();
    containers.sort_by(|a, b| a.0.cmp(&b.0));
    containers.dedup_by(|a, b| a.0 == b.0);
    containers
}

/// The containers a record of `surface`'s kind founds with: its own record's ([`containers`]),
/// as the store reads the kind's type from the surface. A surface naming its record by its label
/// declares no type of its kind, and an enum of its kind is no record: either founds with none.
#[must_use]
pub fn of_kind(surface: &Surface) -> Vec<(String, ObjType)> {
    let named = |name: &str| surface.types.iter().find(|ty| ty.name == name);
    let own = match named(&surface.kind) {
        Some(ty) => !ty.is_enum(),
        None => named(&surface.label).is_none(),
    };
    if own { containers(surface) } else { Vec::new() }
}

/// The derived founding of the record `id` of `kind`, whose fields keep their items in the
/// `containers` named at its root: `None` for a kind that keeps none.
#[must_use]
pub fn structure(
    kind: &str,
    id: &str,
    containers: &[(String, ObjType)],
) -> Option<automerge::Change> {
    if containers.is_empty() {
        return None;
    }
    let mut doc = AutoCommit::new().with_actor(tilo_resource::genesis::actor(id, kind, ""));
    for (field, container) in containers {
        doc.put_object(ROOT, field.as_str(), *container).ok()?;
    }
    doc.commit_with(
        CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message())
            .with_time(0),
    );
    doc.get_last_local_change()
}
