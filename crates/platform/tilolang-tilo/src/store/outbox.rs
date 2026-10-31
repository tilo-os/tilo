//! The write the store makes for an outside effect's settle or mark: one change of its own,
//! carried by the action it notes.
//!
//! `$outbox` of a record holds one entry per queued effect: what it is, its payload, the device
//! that carries it out, the moment it is held until when the action held it back, and how it
//! stands. Nothing here is package code's: the runtime settles and marks, and a person's
//! `withdraw` takes one back.

use serde_json::Value as Json;
use tilolang_runtime::ids::DocId;
use tilolang_runtime::store::{
    ChangeNote, DeliveryState, OutboxEntry, OutboxState, Settled, StoreError,
};

use super::{Authorship, TiloStore, lookup, not_found};
use crate::action::{DynAction, DynEdit, outbox_entries};

impl TiloStore {
    /// Write `edit` of the outbox entry `entry` as a change of its own, noted as `note`.
    pub(super) fn write_outbox(
        &self,
        entry: &OutboxEntry,
        edit: DynEdit,
        seed: Vec<u8>,
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
            edit.clone(),
            seed,
            Vec::new(),
            (Vec::new(), Vec::new()),
        )?;
        let kind =
            lookup(&ty).map_or_else(String::new, |schema| format!("tl.{app}.{}", schema.name));
        inner.enqueue(DynAction {
            app,
            kind,
            action_id: note.action.clone(),
            args: note.args.clone(),
            authorship: Authorship {
                invoker: note.invoker.clone(),
                chain: note.chain.clone(),
                module: note.module.clone(),
                at: note.at.clone(),
            },
            edits: vec![edit],
        });
        Ok(())
    }
}
