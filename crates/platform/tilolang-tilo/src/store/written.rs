//! What a change wrote to the fields of a resource, before and after (`host.Version.changes`),
//! read from the change itself through the platform's history and kept per change: a change
//! never changes, so each is read once.

use automerge::{ChangeHash, ReadDoc as _};
use serde_json::Value as Json;
use tilolang_runtime::ids::FieldId;
use tilolang_runtime::store::{FieldSpec, FieldValue, StoreError};
use tilolang_runtime::value::ResourceRef;

use super::{TiloStore, backend, not_found};
use crate::reads::locate_ref;
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
        let wrote = tilo_resource::history::written(committed, &hash, obj).map_err(backend)?;
        let mut held = Vec::with_capacity(wrote.len());
        for written in wrote {
            let Some(spec) = located.schema.field(&written.prop) else {
                continue;
            };
            let side = |json: &Json, heads: &[ChangeHash]| match spec.kind {
                // Rich text is read as the runtime reads it, from the record at that version.
                MergeKind::RichText => crate::reads::rich_at(committed, obj, &spec.id, heads),
                _ => crate::reads::field_of_json(json, spec),
            };
            let made = committed
                .get_change_by_hash(&hash)
                .ok_or_else(|| not_found(format!("change {change}")))?;
            let before = side(&written.before, made.deps())?;
            let after = side(&written.after, &[hash])?;
            if before != after {
                held.push((spec.id.clone(), before, after));
            }
        }
        let out = select_written(&held, fields);
        inner.written.borrow_mut().insert(key, held);
        Ok(out)
    }
}
