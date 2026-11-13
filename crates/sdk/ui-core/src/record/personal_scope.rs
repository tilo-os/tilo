//! Where a person's own records live, and where the ones written before their
//! own store existed were kept.
//!
//! Their records are keyed in their own scope ([`super::person::PERSONAL_SCOPE`]),
//! which their store carries on a cluster derived from their root key. Before
//! that store existed they were keyed in a workspace derived from the same key,
//! which is gone: what is still stored there is theirs to list and to open, and
//! each record still kept there is read once, on its first open, and taken into
//! the person's own scope by [`RecordSet::take_from_retired_workspace`].
use std::rc::Rc;

use automerge::AutoCommit;
use automerge::transaction::Transactable as _;
use durable_store::{DurableStore, StorageKey};

use super::{
    RecordCheckout, RecordId, RecordKind, RecordOpenError, RecordSet, WORKSPACE, WORKSPACE_ALIAS,
    identity, meta_obj, transfer, workspace_scope,
};

/// Where a person's own records live: the cluster their store carries them on,
/// and the workspace they were filed under before that store existed.
#[derive(Clone)]
pub(super) struct PersonScope {
    /// The person's own cluster: the one their store holds and their records'
    /// routes name ([`collab_types::personal_cluster`]).
    pub(super) cluster: uuid::Uuid,
    /// The workspace their records were keyed by before their store existed,
    /// read once by [`RecordSet::take_from_retired_workspace`].
    pub(super) retired: String,
}

impl PersonScope {
    /// The scope of the person `person` names (the lowercase hex of their
    /// identity root); `None` for anything else, whose records stay on this
    /// device.
    pub(super) fn of(person: &str) -> Option<Self> {
        let mut root = [0u8; 32];
        hex::decode_to_slice(person, &mut root).ok()?;
        Some(Self {
            cluster: collab_types::personal_cluster(&root),
            retired: collab_types::retired_person_workspace(&root).to_string(),
        })
    }
}

impl RecordSet {
    /// A record of this person's still addressed to the workspace their records
    /// were kept in before their own store existed: its document, stamped into
    /// the person's own scope and ready to be written under the same record id.
    /// `None` when this set is not a person's own scope, when nothing of that
    /// record was stored there, when what was stored there is not this record's,
    /// or when it was taken already.
    ///
    /// Nothing is derived from the old id again: this is the one read of it, and
    /// the claim it records in the device's own store is what keeps a second
    /// start from taking the same record twice.
    pub(super) fn take_from_retired_workspace(
        &self,
        backend: &Rc<dyn DurableStore>,
        want: &RecordCheckout,
    ) -> Result<Option<AutoCommit>, RecordOpenError> {
        let Some(retired) = self.retired.as_deref() else {
            return Ok(None);
        };
        // The same key [`RecordSet::canonical_key`] would have named in that
        // workspace then: the workspace's own document, or one of its records.
        let retired_key = if want.id.is_package_primary() {
            StorageKey::workspace_index(&self.app_id, retired)
        } else {
            StorageKey::document(&self.app_id, retired, want.id.storage_key())
        };
        if backend
            .read(&retired_key)?
            .is_none_or(|stored| stored.bytes.is_empty())
        {
            return Ok(None);
        }
        let Some(loaded) = app_state::DocOwner::acquire(Rc::clone(backend), retired_key.clone())?
            .load_complete()?
        else {
            return Ok(None);
        };
        let mut doc = loaded.document().clone();
        if identity(&doc).is_none_or(|found| found.id != want.id || found.kind != want.kind)
            || workspace_scope(&doc).as_deref() != Some(retired)
            || transfer::is_retired(&doc)
        {
            tracing::warn!(
                record = %want.id,
                "a record kept before the person's own store does not match it; not taking it",
            );
            return Ok(None);
        }
        // The record keeps its id and its whole history, attribution and undo
        // among it; only where it is filed becomes theirs. Every field that
        // says where that is moves: the workspace it is bound to (under the
        // one key every reader knows, with the key one build wrote it under
        // dropped so it names one workspace) and the one that minted it, so a
        // record taken here and the same record started fresh on another of
        // the person's devices are one identity, not two.
        let Some(metadata) = meta_obj(&mut doc) else {
            return Ok(None);
        };
        if doc.put(&metadata, "home", &self.home).is_err()
            || doc.put(&metadata, WORKSPACE, &self.home).is_err()
        {
            return Ok(None);
        }
        let _ = doc.delete(&metadata, WORKSPACE_ALIAS);
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Upgrade.change_message()),
        );
        if !backend.claim(&retired_key, &want.id.storage_key())? {
            tracing::info!(
                record = %want.id,
                "a record kept before the person's own store was taken already",
            );
            return Ok(None);
        }
        tracing::info!(
            record = %want.id,
            "a record kept before the person's own store is taken into it",
        );
        Ok(Some(doc))
    }

    /// The keys this store's package keeps in the workspace the person's
    /// records were filed under before their own store existed. Empty for any
    /// other scope, so a store that never had one lists nothing there.
    pub(super) fn retired_record_keys(&self, backend: &Rc<dyn DurableStore>) -> Vec<String> {
        let Some(retired) = self.retired.as_deref() else {
            return Vec::new();
        };
        backend.documents(&self.app_id, retired).unwrap_or_default()
    }

    /// The [window default records](RecordId::window_default) of `kind` the
    /// person kept in the workspace their records were filed under before
    /// their own store existed, read as they are and left where they are, for
    /// the windows an app started there to be listed with the ones it starts
    /// here ([`super::RecordStore::window_records`]). Those of `kept`, which
    /// the store holds already, are left out.
    pub(super) fn retired_window_records(
        &self,
        backend: &Rc<dyn DurableStore>,
        kind: &RecordKind,
        kept: &[RecordId],
    ) -> Vec<(RecordId, AutoCommit)> {
        let Some(retired) = self.retired.as_deref() else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for key in self.retired_record_keys(backend) {
            let Some(id) = RecordId::window_default_of_storage_key(&key) else {
                continue;
            };
            if kept.contains(&id) {
                continue;
            }
            let stored = backend.read(&StorageKey::document(&self.app_id, retired, &key));
            let loaded = match stored {
                Ok(Some(stored)) if !stored.bytes.is_empty() => stored,
                _ => continue,
            };
            let doc = match AutoCommit::load_with_options(
                &loaded.bytes,
                automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Error),
            ) {
                Ok(doc) => doc,
                Err(error) => {
                    tracing::warn!(record = %id, %error, "a window record kept before the person's own store is not readable; left out");
                    continue;
                }
            };
            if identity(&doc).is_none_or(|found| found.id != id || found.kind != *kind)
                || workspace_scope(&doc).as_deref() != Some(retired)
                || transfer::is_retired(&doc)
            {
                tracing::warn!(record = %id, "a record kept before the person's own store does not match it; left out");
                continue;
            }
            found.push((id, doc));
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ReadDoc as _;
    use durable_store::{MemoryStore, WriteOp};

    const PACKAGE: &str = "so.tilo.notes";

    /// The person, and the workspace their records were filed under before
    /// their own store existed.
    fn person() -> (String, String) {
        let root = [7u8; 32];
        (
            hex::encode(root),
            collab_types::retired_person_workspace(&root).to_string(),
        )
    }

    /// `chapter` of a notebook record, as the app kept it in `workspace` then.
    fn keep(store: &MemoryStore, workspace: &str, id: &RecordId, kind: &RecordKind, chapter: &str) {
        let mut doc = AutoCommit::new();
        assert!(crate::record::init_scoped(
            &mut doc,
            id,
            kind,
            workspace,
            Some(workspace)
        ));
        doc.put(automerge::ROOT, "chapter", chapter).unwrap();
        store
            .write(
                &StorageKey::document(PACKAGE, workspace, id.storage_key()),
                WriteOp::Replace(doc.save()),
                None,
            )
            .unwrap();
    }

    fn chapter(doc: &AutoCommit) -> String {
        doc.get(automerge::ROOT, "chapter")
            .unwrap()
            .unwrap()
            .0
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// What the person kept before their own store existed is theirs here: a
    /// store of theirs lists those windows and parts, opens them with the same
    /// id and what was in them, and lists each once afterwards.
    #[test]
    fn what_the_person_kept_before_their_store_is_listed_and_opened_here() {
        let store = Rc::new(MemoryStore::new());
        let (person, retired) = person();
        let kind = RecordKind::new("notebook.page");
        let window = RecordId::window_default(4);
        keep(&store, &retired, &window, &kind, "three");
        let chain = RecordId::minted();
        keep(&store, &retired, &chain.part(0), &kind, "one");
        keep(&store, &retired, &chain.part(1), &kind, "two");

        let mut owners = crate::record::RecordOwners::with_store(store.clone());
        let records = owners.open_personal(PACKAGE, &person).unwrap();

        // A window the app started there is listed as the ones it starts here.
        let windows = records.window_records(&kind);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].0, window, "the window keeps its id");
        assert_eq!(chapter(&windows[0].1), "three");
        // So are the parts of a record that grew there.
        assert_eq!(records.held_parts(&chain), vec![0, 1]);
        // Listing them changes nothing: they are read where they are.
        assert!(
            store
                .read(&StorageKey::document(
                    PACKAGE,
                    &retired,
                    window.storage_key()
                ))
                .unwrap()
                .is_some_and(|stored| !stored.bytes.is_empty())
        );

        // Opening one takes it into the person's own scope: the same record,
        // what was in it, listed once from here on.
        records
            .try_open_record(&RecordCheckout::new(window.clone(), kind.clone()))
            .unwrap();
        let windows = records.window_records(&kind);
        assert_eq!(windows.len(), 1, "the window is listed once");
        assert_eq!(windows[0].0, window);
        assert_eq!(chapter(&windows[0].1), "three");
        assert_eq!(records.held_parts(&chain), vec![0, 1]);
        assert!(
            store
                .read(&StorageKey::document(
                    PACKAGE,
                    crate::record::person::PERSONAL_SCOPE,
                    window.storage_key()
                ))
                .unwrap()
                .is_some_and(|stored| !stored.bytes.is_empty()),
            "the record is written in the person's own scope"
        );
    }
}
