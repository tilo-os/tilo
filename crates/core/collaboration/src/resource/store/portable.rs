//! Cache adapter for the canonical record lifecycle; widget, action and history
//! ownership remain in the ordinary ResourceStore.
use super::{
    AutoCommit, DocumentCheckout, DocumentId, Parked, PortableStorage, Rc, Reserved,
    ResourceOpenError, ResourceOpenOutcome, ResourceOwners, ResourceReservation,
    ResourceSaveStatus, ResourceSet, ResourceStore, Transactable, validate_identity,
};
// The browser cache has its own key space (with a package-primary slot).
use crate::portable_storage::StorageKey;

pub(super) fn key(package: &str, workspace: &str, id: &DocumentId) -> StorageKey {
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
fn error(error: impl ToString) -> ResourceOpenError {
    ResourceOpenError::Initialization(error.to_string())
}

impl ResourceOwners {
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
impl ResourceSet {
    pub(super) fn open_portable(
        &mut self,
        want: &DocumentCheckout,
    ) -> Result<ResourceOpenOutcome, ResourceOpenError> {
        if self.pending.contains_key(&want.id) {
            return Err(ResourceOpenError::Busy);
        }
        if let Some(slot) = self.open.get(&want.id) {
            if slot.kind != want.kind {
                return Err(ResourceOpenError::IdentityMismatch);
            }
            validate_identity(
                slot.doc.as_ref().ok_or(ResourceOpenError::Busy)?,
                want,
                &self.home,
            )?;
            return Ok(ResourceOpenOutcome::AlreadyOpen);
        }
        let cache = self
            .portable
            .as_ref()
            .ok_or(ResourceOpenError::IdentityMismatch)?;
        let target = key(&self.app_id, &self.home, &want.id);
        let value = cache
            .get(&target)
            .ok_or(ResourceOpenError::MissingResource)?;
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
        Ok(ResourceOpenOutcome::Loaded)
    }
    pub(super) fn offer_portable_resource(&mut self, id: &DocumentId) -> Result<(), String> {
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
        cache.put_document(key(&self.app_id, &self.home, id), doc, Some(actor))
    }

    /// Hand the open records to the cache. With `accepted_is_written` the
    /// cache's "the host has not acknowledged yet" is what a store commit-
    /// ting on its own thread says for bytes it holds
    /// ([`ResourceSet::try_flush_written`]), not a failure.
    pub(super) fn flush_portable(
        &mut self,
        only: Option<&DocumentId>,
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
            let key = key(&self.app_id, &self.home, id);
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
impl ResourceStore {
    pub(super) fn reserve_portable(
        &self,
        set: &mut ResourceSet,
        want: &DocumentCheckout,
    ) -> Result<ResourceReservation, ResourceOpenError> {
        let cache = set
            .portable
            .as_ref()
            .ok_or(ResourceOpenError::IdentityMismatch)?;
        if set.pending.contains_key(&want.id)
            || set.open.contains_key(&want.id)
            || cache.get(&key(&set.app_id, &set.home, &want.id)).is_some()
        {
            return Err(ResourceOpenError::Busy);
        }
        set.next_reservation = set
            .next_reservation
            .checked_add(1)
            .ok_or(ResourceOpenError::Busy)?;
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
        Ok(ResourceReservation {
            store: self.clone(),
            checkout: want.clone(),
            generation,
        })
    }
}
impl ResourceReservation {
    pub(super) fn initialize_portable(
        &self,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
        seed: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<(), ResourceOpenError> {
        let (actor, workspace) = {
            let set = self.store.0.borrow();
            let pending = set
                .pending
                .get(&self.checkout.id)
                .filter(|p| p.generation == self.generation)
                .ok_or(ResourceOpenError::Busy)?;
            (
                pending
                    .actor
                    .clone()
                    .ok_or(ResourceOpenError::IdentityMismatch)?,
                set.home.clone(),
            )
        };
        let mut doc = AutoCommit::new().with_actor(actor.clone());
        super::genesis(&mut doc, &self.checkout, &workspace, initialize)?;
        seed(&mut doc).map_err(ResourceOpenError::Initialization)?;
        if doc.get_actor() != &actor {
            return Err(ResourceOpenError::IdentityMismatch);
        }
        validate_identity(&doc, &self.checkout, &workspace)?;
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
        );
        let history = crate::state::UndoLog::new(&mut doc);
        let mut set = self.store.set_mut();
        if set
            .pending
            .get(&self.checkout.id)
            .is_none_or(|p| p.generation != self.generation)
        {
            return Err(ResourceOpenError::Busy);
        }
        let cache = set
            .portable
            .as_ref()
            .ok_or(ResourceOpenError::IdentityMismatch)?;
        cache
            .put(
                key(&set.app_id, &set.home, &self.checkout.id),
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
    id: &DocumentId,
    doc: &AutoCommit,
    writers: Option<&crate::state::WritingActors>,
) -> ResourceSaveStatus {
    ResourceSaveStatus::of_portable(cache, &key(package, workspace, id), doc, writers)
}

#[cfg(test)]
mod tests {
    use super::super::ResourceType;
    use super::*;
    use crate::state::WritingActors;
    tilo_test_env::scenario! {
        fn portable_records_are_cached_owned_once_and_written_in_sequence;

        phase cached_heads_save_status_tracks_current_document_and_durability;
        {
            let cache = PortableStorage::new("profile", "device").unwrap();
            let id = DocumentId::minted();
            let storage_key = key("test", "workspace", &id);
            let mut doc = AutoCommit::new();
            doc.put(automerge::ROOT, "value", 1).unwrap();
            cache.put(storage_key.clone(), doc.save(), None).unwrap();
            let older = cache.pending_writes().remove(0);
            assert_eq!(
                save_status(&cache, "test", "workspace", &id, &doc, Some(&WritingActors::default())),
                ResourceSaveStatus::Saving
            );
            cache.acknowledge(&older, Ok(())).unwrap();
            assert_eq!(
                save_status(&cache, "test", "workspace", &id, &doc, Some(&WritingActors::default())),
                ResourceSaveStatus::Saved
            );
            doc.put(automerge::ROOT, "value", 2).unwrap();
            let pending = doc.pending_ops();
            assert_eq!(
                save_status(&cache, "test", "workspace", &id, &doc, Some(&WritingActors::default())),
                ResourceSaveStatus::Saving
            );
            assert_eq!(doc.pending_ops(), pending, "status must not commit typing");
            cache.put(storage_key.clone(), doc.save(), None).unwrap();
            cache.acknowledge(&older, Ok(())).unwrap();
            assert_eq!(
                save_status(&cache, "test", "workspace", &id, &doc, Some(&WritingActors::default())),
                ResourceSaveStatus::Saving
            );
            let newer = cache.pending_writes().remove(0);
            cache.acknowledge(&newer, Err("quota".into())).unwrap();
            assert_eq!(
                save_status(&cache, "test", "workspace", &id, &doc, Some(&WritingActors::default())),
                ResourceSaveStatus::Failed("quota".into())
            );
            cache.acknowledge(&newer, Ok(())).unwrap();
            assert_eq!(
                save_status(&cache, "test", "workspace", &id, &doc, Some(&WritingActors::default())),
                ResourceSaveStatus::Saved
            );
            cache.put(storage_key, vec![1, 2, 3], None).unwrap();
            assert_eq!(
                save_status(&cache, "test", "workspace", &id, &doc, Some(&WritingActors::default())),
                ResourceSaveStatus::Failed("cached document is corrupt".into())
            );
        }

        phase separate_registries_share_one_cache_owner;
        {
            let cache = PortableStorage::new("profile", "device").unwrap();
            let a = ResourceOwners::portable(cache.clone())
                .open("so.tilo.test", "workspace")
                .unwrap();
            let b = ResourceOwners::portable(cache)
                .open("so.tilo.test", "workspace")
                .unwrap();
            assert!(a.same_owner(&b));
        }

        phase portable_convenience_admission_never_seeds_existing_or_uses_filesystem;
        {
            let cache = PortableStorage::new("profile", "device").unwrap();
            let store = ResourceOwners::portable(cache.clone())
                .open("so.tilo.test", "workspace")
                .unwrap();
            let checkout = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
            assert!(matches!(
                store.open_existing_resource_checked(&checkout),
                Err(ResourceOpenError::MissingResource)
            ));
            assert!(!store.open_resource(&checkout));
            assert!(cache.pending_writes().is_empty());
            store.create_resource(&checkout).unwrap();
            assert!(matches!(
                store.create_resource(&checkout),
                Err(ResourceOpenError::AlreadyExists)
            ));
            store.open_existing_resource_checked(&checkout).unwrap();
            assert_eq!(cache.pending_writes().len(), 1);
            let storage = store.history_namespace(&checkout.id).unwrap().0.unwrap();
            assert!(storage.starts_with("portable:"));
            drop(store);
            let restored = ResourceOwners::portable(cache)
                .open("so.tilo.test", "workspace")
                .unwrap();
            restored.open_existing_resource_checked(&checkout).unwrap();
            assert!(restored.is_ready(&checkout.id));
        }

        phase corrupt_hydrated_record_is_retained_and_cannot_be_recreated;
        {
            let cache = PortableStorage::new("profile", "device").unwrap();
            let checkout = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
            let key = key("so.tilo.test", "workspace", &checkout.id);
            cache
                .hydrate(key.clone(), vec![1, 2, 3], Some(vec![42]))
                .unwrap();
            let store = ResourceOwners::portable(cache.clone())
                .open("so.tilo.test", "workspace")
                .unwrap();
            assert!(store.open_existing_resource_checked(&checkout).is_err());
            assert!(store.create_resource(&checkout).is_err());
            assert_eq!(cache.get(&key).unwrap().bytes, vec![1, 2, 3]);
            assert!(cache.pending_writes().is_empty());
        }

        phase a_second_page_writing_a_record_never_writes_a_sequence_twice;
        {
            let cache = PortableStorage::new("profile", "device").unwrap();
            let checkout = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
            let key = key("so.tilo.test", "workspace", &checkout.id);
            let page = ResourceOwners::portable(cache.clone())
                .open("so.tilo.test", "workspace")
                .unwrap();
            page.create_resource(&checkout).unwrap();
            let saved = cache.get(&key).unwrap();
            // Another page of the same profile opens what the first one saved, and
            // both write: both carry the change the first one saved, and each
            // writes its own next one.
            let other = PortableStorage::new("profile", "device").unwrap();
            other
                .hydrate(key.clone(), saved.bytes.clone(), saved.actor.clone())
                .unwrap();
            let second = ResourceOwners::portable(other.clone())
                .open("so.tilo.test", "workspace")
                .unwrap();
            second.open_existing_resource_checked(&checkout).unwrap();
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
                crate::resource::store::mutation_merge::load_incremental(&mut merged, change).unwrap();
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

        phase a_change_after_a_refused_one_never_reuses_its_sequence;
        // A page sends what a person writes in a record as soon as it is made.
        // When the workspace refuses one of those changes, the page takes it
        // back, but the relay already holds it: the person's next change from
        // that page must not carry the refused change's writer and sequence,
        // or the record would hold two changes under one number and read whole
        // nowhere.
        {
            let cache = PortableStorage::new("profile", "device").unwrap();
            let checkout = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
            let store = ResourceOwners::portable(cache.clone())
                .open("so.tilo.test", "workspace")
                .unwrap();
            store.create_resource(&checkout).unwrap();
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
                            crate::history_source::authors::device_actor(&tilo_actions::Actor::Member(tilo_actions::MemberId::person("alice")).actor_bytes(), doc.get_actor()),
                        ));
                        doc.put(automerge::ROOT, "message", value).unwrap();
                        let hash = doc.commit().unwrap();
                        doc.set_actor(copy);
                        (hash, doc.save_after(&heads))
                    })
                    .unwrap();
                crate::resource::store::mutation_merge::load_incremental(&mut relay, &sent).unwrap();
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

        phase initializer_cannot_replace_the_reserved_actor;
        {
            let cache = PortableStorage::new("profile", "device").unwrap();
            let store = ResourceOwners::portable(cache)
                .open("so.tilo.test", "workspace")
                .unwrap();
            let checkout = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
            let mut reservation = store.reserve_resource(&checkout).unwrap();
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
}
