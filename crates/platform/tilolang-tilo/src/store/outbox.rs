//! The write the store makes for an outside effect's settle or mark: one change of its own,
//! carried by the action it notes.

use tilolang_runtime::store::{ChangeNote, OutboxEntry, StoreError};

use super::{Authorship, TiloStore, lookup, not_found};
use crate::action::{DynAction, DynEdit};

impl TiloStore {
    /// Write `edit` of the outbox entry `entry` as a change of its own, noted as `note`.
    pub(super) fn write_outbox(
        &self,
        entry: &OutboxEntry,
        edit: DynEdit,
        seed: &[u8],
        note: &ChangeNote,
    ) -> Result<(), StoreError> {
        self.load_record()?;
        let mut inner = self.mutate();
        let (app, ty) = inner
            .index
            .get(&entry.doc)
            .cloned()
            .ok_or_else(|| not_found(format!("resource {}", entry.doc)))?;
        inner.write(
            &app,
            vec![edit.clone()],
            seed,
            &[],
            (Vec::new(), Vec::new()),
        )?;
        let kind =
            lookup(&ty).map_or_else(String::new, |schema| format!("tl.{app}.{}", schema.name));
        inner.enqueue(DynAction {
            app: app.to_string(),
            kind,
            action_id: note.action.clone(),
            args: note.args.clone(),
            authorship: Authorship {
                invoker: note.invoker.clone(),
                chain: note.chain.clone(),
                module: note.module.clone(),
                at: note.at.clone(),
                from: note.from.clone(),
                suggested_by: note.suggested_by.clone(),
                count: note.count.max(1),
                invocation: note.invocation.clone(),
                gesture: None,
            },
            edits: vec![edit],
        });
        Ok(())
    }
}
