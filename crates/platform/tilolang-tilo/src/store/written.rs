//! What a change wrote to the fields of a resource, before and after (`host.Version.changes`):
//! which fields from the change itself, and what each held on either side, read in place
//! through the platform's history ([`tilo_resource::history::wrote`] and
//! [`tilo_resource::history::held_at`]), at the cost of the values and never of the record. A
//! change never changes, so each is read once.
//!
//! Rich text is left out: its value is a document of its own the toolkit reads only as it is
//! now, and reading it as of a version would cost the whole record.

use automerge::{ChangeHash, ReadDoc as _};
use tilolang_runtime::ids::FieldId;
use tilolang_runtime::store::{FieldSpec, FieldValue, StoreError};
use tilolang_runtime::value::ResourceRef;

use super::{TiloStore, backend, not_found};
use crate::reads::{field_of_json, locate_ref};
use crate::schema::MergeKind;

/// What one change wrote to the fields of one resource: each field, before and after.
pub(super) type Written = Vec<(FieldId, FieldValue, FieldValue)>;

/// Of what a change wrote, the fields among `fields`, in their order.
fn select_written(held: &Written, fields: &[FieldSpec]) -> Written {
    fields
        .iter()
        .filter_map(|field| held.iter().find(|(id, ..)| *id == field.id).cloned())
        .collect()
}

impl TiloStore {
    /// [`tilolang_runtime::Store::written`]: what the change `change` wrote to each of `fields`
    /// of `at`, before and after.
    ///
    /// # Errors
    /// The record is not at hand, or does not hold the change or the resource.
    pub(super) fn written_by(
        &self,
        at: &ResourceRef,
        fields: &[FieldSpec],
        change: &str,
    ) -> Result<Vec<(FieldId, FieldValue, FieldValue)>, StoreError> {
        let key = (change.to_owned(), at.id_string());
        if let Some(held) = self.inner.borrow().written.borrow().get(&key) {
            return Ok(select_written(held, fields));
        }
        self.load_record()?;
        let inner = self.inner.borrow();
        let (app, ty) = inner.locate(at)?;
        let mirror = inner.mirror()?;
        let hash: ChangeHash = change
            .parse()
            .map_err(|_| not_found(format!("change {change}")))?;
        let located = locate_ref(mirror, (&app, &ty), at, &inner.nested)?;
        let Some(obj) = located.obj.as_ref() else {
            // An item stored as its order key alone: no change wrote a field of it.
            return Ok(Vec::new());
        };
        let committed = mirror.committed_document();
        let made = committed
            .get_change_by_hash(&hash)
            .ok_or_else(|| not_found(format!("change {change}")))?;
        let keys = tilo_resource::history::wrote(committed, &hash, obj).map_err(backend)?;
        let mut held = Vec::with_capacity(keys.len());
        for prop in keys {
            let Some(spec) = located.schema.field(&prop) else {
                continue;
            };
            if matches!(
                spec.kind,
                MergeKind::RichText | MergeKind::Collection | MergeKind::Record
            ) {
                continue;
            }
            let side = |heads: &[ChangeHash]| {
                let json = tilo_resource::history::held_at(committed, obj, &prop, heads)
                    .map_err(backend)?;
                field_of_json(&json, spec)
            };
            let before = side(made.deps())?;
            let after = side(&[hash])?;
            if before != after {
                held.push((spec.id.clone(), before, after));
            }
        }
        let out = select_written(&held, fields);
        inner.written.borrow_mut().insert(key, held);
        Ok(out)
    }
}
