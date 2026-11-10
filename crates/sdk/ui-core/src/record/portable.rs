//! Cache adapter for the canonical record lifecycle; widget, action and history
//! ownership remain in the ordinary RecordStore.
use super::{
    AutoCommit, Parked, PortableStorage, Rc, RecordCheckout, RecordId, RecordOpenError,
    RecordOpenOutcome, RecordOwners, RecordReservation, RecordSaveStatus, RecordSet, RecordStore,
    Reserved, Transactable, WORKSPACE, WORKSPACE_ALIAS, identity, meta_obj, transfer,
    validate_identity, workspace_scope,
};
// The browser cache has its own key space (with a package-primary slot).
use crate::portable_storage::StorageKey;

pub(super) fn key(package: &str, workspace: &str, id: &RecordId) -> StorageKey {
    if id.is_package_primary() {
        return StorageKey::Primary {
            package: package.into(),
            workspace: workspace.into(),
        };
    }
    StorageKey::Record {
        package: package.into(),
        workspace: workspace.into(),
        record: id.as_str().into(),
    }
}
fn error(error: impl ToString) -> RecordOpenError {
    RecordOpenError::Initialization(error.to_string())
}

impl RecordOwners {
    /// Construct owners over a hydrated cache with explicit profile/device identity.
    pub fn portable(cache: PortableStorage) -> Self {
        Self {
            // Record sets over a portable cache never open this store.
            store: Rc::new(durable_store::MemoryStore::new()),
            portable: Some(cache),
            owners: Rc::default(),
            profile_lifetime: None,
        }
    }
}
impl RecordSet {
    pub(super) fn open_portable(
        &mut self,
        want: &RecordCheckout,
    ) -> Result<RecordOpenOutcome, RecordOpenError> {
        if self.pending.contains_key(&want.id) {
            return Err(RecordOpenError::Busy);
        }
        if let Some(slot) = self.open.get(&want.id) {
            if slot.kind != want.kind {
                return Err(RecordOpenError::IdentityMismatch);
            }
            validate_identity(
                slot.doc.as_ref().ok_or(RecordOpenError::Busy)?,
                want,
                &self.home,
            )?;
            return Ok(RecordOpenOutcome::AlreadyOpen);
        }
        let cache = self
            .portable
            .as_ref()
            .ok_or(RecordOpenError::IdentityMismatch)?;
        let target = key(&self.package_id, &self.home, &want.id);
        if cache.get(&target).is_none() {
            self.take_from_retired_workspace_in_cache(cache, want)?;
        }
        let value = cache.get(&target).ok_or(RecordOpenError::MissingRecord)?;
        // Complete load: corrupt/truncated cache bytes remain untouched for recovery.
        let mut doc = AutoCommit::load_with_options(
            &value.bytes,
            automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Error),
        )
        .map_err(error)?;
        validate_identity(&doc, want, &self.home)?;
        // The row's saved actor is the writer its last save was made by, which
        // may still be live in another page: a writer identity belongs to one
        // writer, so this page writes the record under one it mints, as a
        // record created here does. The row keeps it for the saves that follow.
        let actor = automerge::ActorId::random();
        cache
            .claim(&target, actor.to_bytes().to_vec())
            .map_err(error)?;
        doc.set_actor(actor);
        let history = crate::state::UndoLog::new(&mut doc);
        self.open.insert(
            want.id.clone(),
            Parked {
                doc: Some(doc),
                kind: want.kind.clone(),
                autosave: None,
                history,
                action_state: Default::default(),
            },
        );
        Ok(RecordOpenOutcome::Loaded)
    }
    /// [`RecordSet::take_from_retired_workspace`] for a cache with its own key
    /// space: a record of the person's still kept in the workspace their
    /// records were filed under before their own store existed is read there,
    /// stamped into the scope they have now and written under the same record
    /// id, with the whole history it had.
    fn take_from_retired_workspace_in_cache(
        &self,
        cache: &PortableStorage,
        want: &RecordCheckout,
    ) -> Result<(), RecordOpenError> {
        let Some(retired) = self.retired.as_deref() else {
            return Ok(());
        };
        let Some(former) = cache.get(&key(&self.package_id, retired, &want.id)) else {
            return Ok(());
        };
        let mut doc = AutoCommit::load_with_options(
            &former.bytes,
            automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Error),
        )
        .map_err(error)?;
        if identity(&doc).is_none_or(|found| found.id != want.id || found.kind != want.kind)
            || workspace_scope(&doc).as_deref() != Some(retired)
            || transfer::is_retired(&doc)
        {
            return Ok(());
        }
        let Some(metadata) = meta_obj(&mut doc) else {
            return Ok(());
        };
        doc.put(&metadata, "home", &self.home).map_err(error)?;
        doc.put(&metadata, WORKSPACE, &self.home).map_err(error)?;
        let _ = doc.delete(&metadata, WORKSPACE_ALIAS);
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Upgrade.change_message()),
        );
        let actor = doc.get_actor().to_bytes().to_vec();
        cache
            .put_document(
                key(&self.package_id, &self.home, &want.id),
                &mut doc,
                Some(actor),
            )
            .map_err(error)
    }

    pub(super) fn offer_portable_record(&mut self, id: &RecordId) -> Result<(), String> {
        let cache = self
            .portable
            .as_ref()
            .ok_or("portable persistence unavailable")?;
        let slot = self
            .open
            .get_mut(id)
            .ok_or_else(|| format!("{id}: record is not open"))?;
        let doc = slot
            .doc
            .as_mut()
            .ok_or_else(|| format!("{id}: record is busy"))?;
        slot.action_state.commit_for_save(doc);
        let actor = doc.get_actor().to_bytes().to_vec();
        cache.put_document(key(&self.package_id, &self.home, id), doc, Some(actor))
    }

    /// Hand the open records to the cache. With `accepted_is_written` the
    /// cache's "the host has not acknowledged yet" is what a store commit-
    /// ting on its own thread says for bytes it holds
    /// ([`RecordSet::try_flush_written`]), not a failure.
    pub(super) fn flush_portable(
        &mut self,
        only: Option<&RecordId>,
        accepted_is_written: bool,
    ) -> Result<(), String> {
        let cache = self
            .portable
            .as_ref()
            .ok_or("portable persistence unavailable")?;
        let mut errors = Vec::new();
        for (id, slot) in &mut self.open {
            if only.is_some_and(|only| only != id) {
                continue;
            }
            let Some(doc) = slot.doc.as_mut() else {
                errors.push(format!("{id}: busy"));
                continue;
            };
            let edited = doc.pending_ops() != 0;
            slot.action_state.commit_for_save(doc);
            let key = key(&self.package_id, &self.home, id);
            let actor = doc.get_actor().to_bytes().to_vec();
            match cache
                .put_document(key.clone(), doc, Some(actor))
                .and_then(|()| cache.flush(&key))
            {
                // Edits that changed nothing stored are saved already: no
                // acknowledgement will come to say so.
                Ok(()) if edited => cache.durability_changed(&key),
                Ok(()) => {}
                Err(e)
                    if accepted_is_written
                        && e == crate::portable_storage::PENDING_ACKNOWLEDGEMENT => {}
                Err(e) => errors.push(format!("{id}: {e}")),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}
impl RecordStore {
    pub(super) fn reserve_portable(
        &self,
        set: &mut RecordSet,
        want: &RecordCheckout,
    ) -> Result<RecordReservation, RecordOpenError> {
        let cache = set
            .portable
            .as_ref()
            .ok_or(RecordOpenError::IdentityMismatch)?;
        if set.pending.contains_key(&want.id)
            || set.open.contains_key(&want.id)
            || cache
                .get(&key(&set.package_id, &set.home, &want.id))
                .is_some()
        {
            return Err(RecordOpenError::Busy);
        }
        set.next_reservation = set
            .next_reservation
            .checked_add(1)
            .ok_or(RecordOpenError::Busy)?;
        let generation = set.next_reservation;
        set.pending.insert(
            want.id.clone(),
            Reserved {
                generation,
                kind: want.kind.clone(),
                store: None,
                actor: Some(automerge::ActorId::random()),
            },
        );
        Ok(RecordReservation {
            store: self.clone(),
            checkout: want.clone(),
            generation,
        })
    }
}
impl RecordReservation {
    pub(super) fn initialize_portable(
        &self,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
        seed: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<(), RecordOpenError> {
        let (actor, workspace) = {
            let set = self.store.0.borrow();
            let pending = set
                .pending
                .get(&self.checkout.id)
                .filter(|p| p.generation == self.generation)
                .ok_or(RecordOpenError::Busy)?;
            (
                pending
                    .actor
                    .clone()
                    .ok_or(RecordOpenError::IdentityMismatch)?,
                set.home.clone(),
            )
        };
        let mut doc = AutoCommit::new().with_actor(actor.clone());
        super::genesis(&mut doc, &self.checkout, &workspace, initialize)?;
        seed(&mut doc).map_err(RecordOpenError::Initialization)?;
        if doc.get_actor() != &actor {
            return Err(RecordOpenError::IdentityMismatch);
        }
        validate_identity(&doc, &self.checkout, &workspace)?;
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
        );
        let history = crate::state::UndoLog::new(&mut doc);
        let mut set = self.store.0.borrow_mut();
        if set
            .pending
            .get(&self.checkout.id)
            .is_none_or(|p| p.generation != self.generation)
        {
            return Err(RecordOpenError::Busy);
        }
        let cache = set
            .portable
            .as_ref()
            .ok_or(RecordOpenError::IdentityMismatch)?;
        cache
            .put(
                key(&set.package_id, &set.home, &self.checkout.id),
                doc.save(),
                Some(doc.get_actor().to_bytes().to_vec()),
            )
            .map_err(error)?;
        set.pending.remove(&self.checkout.id);
        set.open.insert(
            self.checkout.id.clone(),
            Parked {
                doc: Some(doc),
                kind: self.checkout.kind.clone(),
                autosave: None,
                history,
                action_state: Default::default(),
            },
        );
        Ok(())
    }
}
pub(super) fn save_status(
    cache: &PortableStorage,
    package: &str,
    workspace: &str,
    id: &RecordId,
    doc: &AutoCommit,
) -> RecordSaveStatus {
    RecordSaveStatus::of_portable(cache, &key(package, workspace, id), doc)
}

#[cfg(test)]
mod tests {
    use super::super::RecordKind;
    use super::*;
    #[test]
    fn cached_heads_save_status_tracks_current_document_and_durability() {
        let cache = PortableStorage::new("profile", "device").unwrap();
        let id = RecordId::minted();
        let storage_key = key("test", "workspace", &id);
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "value", 1).unwrap();
        cache.put(storage_key.clone(), doc.save(), None).unwrap();
        let older = cache.pending_writes().remove(0);
        assert_eq!(
            save_status(&cache, "test", "workspace", &id, &doc),
            RecordSaveStatus::Saving
        );
        cache.acknowledge(&older, Ok(())).unwrap();
        assert_eq!(
            save_status(&cache, "test", "workspace", &id, &doc),
            RecordSaveStatus::Saved
        );
        doc.put(automerge::ROOT, "value", 2).unwrap();
        let pending = doc.pending_ops();
        assert_eq!(
            save_status(&cache, "test", "workspace", &id, &doc),
            RecordSaveStatus::Saving
        );
        assert_eq!(doc.pending_ops(), pending, "status must not commit typing");
        cache.put(storage_key.clone(), doc.save(), None).unwrap();
        cache.acknowledge(&older, Ok(())).unwrap();
        assert_eq!(
            save_status(&cache, "test", "workspace", &id, &doc),
            RecordSaveStatus::Saving
        );
        let newer = cache.pending_writes().remove(0);
        cache.acknowledge(&newer, Err("quota".into())).unwrap();
        assert_eq!(
            save_status(&cache, "test", "workspace", &id, &doc),
            RecordSaveStatus::Failed("quota".into())
        );
        cache.acknowledge(&newer, Ok(())).unwrap();
        assert_eq!(
            save_status(&cache, "test", "workspace", &id, &doc),
            RecordSaveStatus::Saved
        );
        cache.put(storage_key, vec![1, 2, 3], None).unwrap();
        assert_eq!(
            save_status(&cache, "test", "workspace", &id, &doc),
            RecordSaveStatus::Failed("cached document is corrupt".into())
        );
    }
    #[test]
    fn separate_registries_share_one_cache_owner() {
        let cache = PortableStorage::new("profile", "device").unwrap();
        let a = RecordOwners::portable(cache.clone())
            .open("so.tilo.test", "workspace")
            .unwrap();
        let b = RecordOwners::portable(cache)
            .open("so.tilo.test", "workspace")
            .unwrap();
        assert!(a.same_owner(&b));
    }
    #[test]
    fn portable_convenience_admission_never_seeds_existing_or_uses_filesystem() {
        let cache = PortableStorage::new("profile", "device").unwrap();
        let store = RecordOwners::portable(cache.clone())
            .open("so.tilo.test", "workspace")
            .unwrap();
        let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        assert!(matches!(
            store.open_existing_record_checked(&checkout),
            Err(RecordOpenError::MissingRecord)
        ));
        assert!(!store.open_record(&checkout));
        assert!(cache.pending_writes().is_empty());
        store.create_record(&checkout).unwrap();
        assert!(matches!(
            store.create_record(&checkout),
            Err(RecordOpenError::AlreadyExists)
        ));
        store.open_existing_record_checked(&checkout).unwrap();
        assert_eq!(cache.pending_writes().len(), 1);
        let storage = store.history_namespace(&checkout.id).unwrap().0.unwrap();
        assert!(storage.starts_with("portable:"));
        drop(store);
        let restored = RecordOwners::portable(cache)
            .open("so.tilo.test", "workspace")
            .unwrap();
        restored.open_existing_record_checked(&checkout).unwrap();
        assert!(restored.is_ready(&checkout.id));
    }
    #[test]
    fn corrupt_hydrated_record_is_retained_and_cannot_be_recreated() {
        let cache = PortableStorage::new("profile", "device").unwrap();
        let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        let key = key("so.tilo.test", "workspace", &checkout.id);
        cache
            .hydrate(key.clone(), vec![1, 2, 3], Some(vec![42]))
            .unwrap();
        let store = RecordOwners::portable(cache.clone())
            .open("so.tilo.test", "workspace")
            .unwrap();
        assert!(store.open_existing_record_checked(&checkout).is_err());
        assert!(store.create_record(&checkout).is_err());
        assert_eq!(cache.get(&key).unwrap().bytes, vec![1, 2, 3]);
        assert!(cache.pending_writes().is_empty());
    }
    #[test]
    fn a_second_page_writing_a_record_never_writes_a_sequence_twice() {
        let cache = PortableStorage::new("profile", "device").unwrap();
        let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        let key = key("so.tilo.test", "workspace", &checkout.id);
        let page = RecordOwners::portable(cache.clone())
            .open("so.tilo.test", "workspace")
            .unwrap();
        page.create_record(&checkout).unwrap();
        let saved = cache.get(&key).unwrap();
        // Another page of the same profile opens what the first one saved, and
        // both write: both carry the change the first one saved, and each
        // writes its own next one.
        let other = PortableStorage::new("profile", "device").unwrap();
        other
            .hydrate(key.clone(), saved.bytes.clone(), saved.actor.clone())
            .unwrap();
        let second = RecordOwners::portable(other.clone())
            .open("so.tilo.test", "workspace")
            .unwrap();
        second.open_existing_record_checked(&checkout).unwrap();
        let mut written = Vec::new();
        for store in [&page, &second] {
            written.push(
                store
                    .with_action_state(&checkout.id, |doc, _, _| {
                        let heads = doc.get_heads();
                        doc.put(automerge::ROOT, "value", "written").unwrap();
                        doc.commit();
                        doc.save_after(&heads)
                    })
                    .unwrap(),
            );
        }
        let mut merged = AutoCommit::load(&saved.bytes).unwrap();
        for change in &written {
            crate::history_mode::routes::load_incremental(&mut merged, change).unwrap();
        }
        let changes = merged.get_changes(&[]);
        let sequences: std::collections::HashSet<_> = changes
            .iter()
            .map(|change| (change.actor_id().to_bytes(), change.seq()))
            .collect();
        assert_eq!(
            sequences.len(),
            changes.len(),
            "one change per writer and sequence, whichever page wrote it"
        );
    }

    /// A page sends what a person writes in a record as soon as it is made.
    /// When the workspace refuses one of those changes, the page takes it
    /// back, but the relay already holds it: the person's next change from
    /// that page must not carry the refused change's writer and sequence,
    /// or the record would hold two changes under one number and read whole
    /// nowhere.
    #[test]
    fn a_change_after_a_refused_one_never_reuses_its_sequence() {
        let cache = PortableStorage::new("profile", "device").unwrap();
        let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        let store = RecordOwners::portable(cache.clone())
            .open("so.tilo.test", "workspace")
            .unwrap();
        store.create_record(&checkout).unwrap();
        let mut relay = AutoCommit::load(
            &cache
                .get(&key("so.tilo.test", "workspace", &checkout.id))
                .unwrap()
                .bytes,
        )
        .unwrap();
        // What a person's action writes: a change under their writer on
        // this copy, sent to the relay at once.
        let mut send = |value: &str| {
            let (hash, sent) = store
                .with_action_state(&checkout.id, |doc, _, _| {
                    let heads = doc.get_heads();
                    let copy = doc.get_actor().clone();
                    doc.set_actor(automerge::ActorId::from(
                        crate::bound_dispatch::document_writer("alice", doc),
                    ));
                    doc.put(automerge::ROOT, "message", value).unwrap();
                    let hash = doc.commit().unwrap();
                    doc.set_actor(copy);
                    (hash, doc.save_after(&heads))
                })
                .unwrap();
            crate::history_mode::routes::load_incremental(&mut relay, &sent).unwrap();
            hash
        };
        send("hello");
        let refused = send("not allowed here");
        store
            .withdraw_refused(
                &checkout.id,
                &[refused],
                "This workspace doesn't allow that.",
            )
            .unwrap()
            .expect("withdrawn");
        let next = send("hello again");

        let changes = relay.get_changes(&[]);
        let sequences: std::collections::HashSet<_> = changes
            .iter()
            .map(|change| (change.actor_id().to_bytes(), change.seq()))
            .collect();
        assert_eq!(
            sequences.len(),
            changes.len(),
            "one change per writer and sequence, a refused one among them"
        );
        let author = relay
            .get_change_by_hash(&next)
            .unwrap()
            .actor_id()
            .to_hex_string();
        assert_eq!(
            crate::history_source::authors::actor_handle(&author).as_deref(),
            Some("alice"),
            "the change after a refusal is still the person's"
        );
        // The page keeps saving the record under the writer it moved to.
        store.try_flush().unwrap_or_else(|error| {
            assert!(
                error.contains(crate::portable_storage::PENDING_ACKNOWLEDGEMENT),
                "{error}"
            );
        });
        for write in cache.pending_writes() {
            cache.acknowledge(&write, Ok(())).unwrap();
        }
        store.try_flush().unwrap();
    }

    #[test]
    fn initializer_cannot_replace_the_reserved_actor() {
        let cache = PortableStorage::new("profile", "device").unwrap();
        let store = RecordOwners::portable(cache)
            .open("so.tilo.test", "workspace")
            .unwrap();
        let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        let mut reservation = store.reserve_record(&checkout).unwrap();
        assert!(
            reservation
                .initialize(|doc| {
                    doc.set_actor(automerge::ActorId::from(vec![42]));
                    Ok(())
                })
                .is_err()
        );
        assert!(!store.is_ready(&checkout.id));
    }
}
