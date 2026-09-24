//! Project admitted canonical documents into the existing checked record owners.
use qos_peer_state::StatePeer;
use ui::{
    portable_storage::{PortableStorage, StorageKey},
    record::{RecordCheckout, RecordId, RecordOwners},
};
fn key(room: &str, app: &str, record: &str) -> StorageKey {
    if record.is_empty() {
        StorageKey::Primary {
            package: app.into(),
            workspace: room.into(),
        }
    } else {
        StorageKey::Record {
            package: app.into(),
            workspace: room.into(),
            record: record.into(),
        }
    }
}
pub(crate) fn install(
    cache: &PortableStorage,
    state: &StatePeer,
    actor: [u8; 32],
) -> Result<(), String> {
    let mut plans = vec![];
    // Use the same schema/identity/room validator before touching any live owner.
    let validation = PortableStorage::new("admission-validation", state.room())?;
    let mut validator = RecordOwners::portable(validation.clone());
    let mut owners = RecordOwners::portable(cache.clone());
    for row in state.rows() {
        // Unknown packages retain their admitted bytes in StatePeer's journal;
        // the desktop renders an explicit unavailable tile instead of inventing a factory.
        let Some(definition) = crate::desktop::definition(&row.app_id) else {
            continue;
        };
        let checkout = if row.app_id == "dev.qos.chat" && row.record_id.is_empty() {
            RecordCheckout::package_primary(definition.record_kind.clone())
        } else {
            RecordCheckout::new(
                RecordId::parse(&row.record_id).ok_or("invalid ordinary record")?,
                definition.record_kind.clone(),
            )
        };
        if row.app_id == "dev.qos.chat" && !checkout.id.is_package_primary() {
            return Err("Chat requires its package-primary route".into());
        }
        let bytes = state
            .document(&row.app_id, &row.record_id)
            .ok_or("missing admitted bytes")?;
        let key = key(state.room(), &row.app_id, &row.record_id);
        validation.put(key.clone(), bytes.clone(), Some(actor.to_vec()))?;
        let checked = validator
            .open(&row.app_id, state.room())
            .map_err(|e| e.to_string())?;
        checked
            .try_open_record(&checkout)
            .map_err(|e| e.to_string())?;
        let owner = if cache.get(&key).is_some() {
            let owner = owners
                .open(&row.app_id, state.room())
                .map_err(|e| e.to_string())?;
            if !owner.can_merge_admitted(&checkout)? {
                return Err("finish pending local mutation before admitting remote state".into());
            }
            Some(owner)
        } else {
            None
        };
        plans.push((key, checkout, bytes, owner));
    }
    for (key, checkout, bytes, owner) in plans {
        if let Some(owner) = owner {
            if owner
                .merge_admitted(
                    &checkout,
                    &qos_peer_state::DocumentMutation::Snapshot(bytes),
                )?
                .is_none()
            {
                return Err("record owner changed during synchronous admission".into());
            }
        } else {
            cache.put(key, bytes, Some(actor.to_vec()))?;
        }
    }
    Ok(())
}

/// Initial projection for a fresh owner. Complete state (restored journal or
/// governance handoff) may already satisfy desktop admission before any
/// delivery cut installs through the ordinary candidate path; incomplete state
/// installs nothing, exactly as that path does.
pub(crate) fn install_initial(
    cache: &PortableStorage,
    state: &StatePeer,
    actor: [u8; 32],
) -> Result<(), String> {
    if state.incomplete_reason().is_some() {
        return Ok(());
    }
    install(cache, state, actor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use ed25519_dalek::SigningKey;
    use tilo_actions::Action;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
    fn state() -> (StatePeer, SigningKey, web_time::SystemTime) {
        let root = SigningKey::from_bytes(&[41; 32]);
        let key = SigningKey::from_bytes(&[42; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "native fixture".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let now = web_time::SystemTime::now();
        let room = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
        let seed = crate::fixture_seed(room, &cert, &[cert.clone()]).unwrap();
        let mut state =
            StatePeer::new(room, cert.clone(), vec![cert], RevocationList::new(), now).unwrap();
        state.install_seed(seed, &key, now).unwrap();
        (state, key, now)
    }
    #[test]
    fn fresh_owner_initial_state_is_openable_before_any_delivery_cut() {
        // A fresh browser owner can reach complete desktop admission from its
        // restored/handoff state alone. The desktop's record owners read the
        // same portable cache, so every admitted row must already be there.
        let (state, _, _) = state();
        assert!(state.incomplete_reason().is_none());
        let app = word_processor::definition();
        let checkout = RecordCheckout::new(RecordId::for_tab(1), app.record_kind.clone());
        let cache = PortableStorage::new("certified:test", "fresh").unwrap();
        let before = RecordOwners::portable(cache.clone())
            .open(app.app_id, state.room())
            .unwrap();
        assert!(matches!(
            before.try_open_record(&checkout),
            Err(ui::record::RecordOpenError::MissingRecord)
        ));
        install_initial(&cache, &state, [13; 32]).unwrap();
        let after = RecordOwners::portable(cache.clone())
            .open(app.app_id, state.room())
            .unwrap();
        after.try_open_record(&checkout).unwrap();
        assert_eq!(
            after
                .read(&checkout.id, |doc| doc.get_actor().to_bytes().to_vec())
                .unwrap(),
            vec![13; 32]
        );
        // Re-projection of the same state is an idempotent merge.
        install_initial(&cache, &state, [13; 32]).unwrap();
    }
    #[test]
    fn concurrent_writer_snapshots_preserve_distinct_local_actor_custody() {
        let (state, _, _) = state();
        let a = PortableStorage::new("test", "a").unwrap();
        let b = PortableStorage::new("test", "b").unwrap();
        install(&a, &state, [11; 32]).unwrap();
        install(&b, &state, [12; 32]).unwrap();
        let app = word_processor::definition();
        let checkout = RecordCheckout::new(RecordId::for_tab(1), app.record_kind.clone());
        let a = RecordOwners::portable(a)
            .open(app.app_id, state.room())
            .unwrap();
        let b = RecordOwners::portable(b)
            .open(app.app_id, state.room())
            .unwrap();
        a.try_open_record(&checkout).unwrap();
        b.try_open_record(&checkout).unwrap();
        let mutate = |store: &ui::record::RecordStore, text: &str| {
            let mut doc = store.read(&checkout.id, Clone::clone).unwrap();
            let containers = qdoc::model::open(&doc, &automerge::ROOT).unwrap();
            let block = qdoc::model::blocks_in_order(&doc, &containers)[0].clone();
            word_processor::actions::WordProcessorAction::SpliceText {
                region: word_processor::actions::RegionKind::Body,
                block,
                at: 0,
                del: 0,
                ins: text.into(),
            }
            .apply(&mut doc)
            .unwrap();
            doc.commit();
            doc.save()
        };
        let delta_a = mutate(&a, "A");
        let delta_b = mutate(&b, "B");
        for store in [&a, &b] {
            for bytes in [&delta_a, &delta_b] {
                assert!(
                    store
                        .merge_admitted(
                            &checkout,
                            &qos_peer_state::DocumentMutation::Snapshot(bytes.clone())
                        )
                        .unwrap()
                        .is_some()
                );
            }
        }
        assert_eq!(
            a.read(&checkout.id, |doc| doc.get_actor().to_bytes().to_vec())
                .unwrap(),
            vec![11; 32]
        );
        assert_eq!(
            b.read(&checkout.id, |doc| doc.get_actor().to_bytes().to_vec())
                .unwrap(),
            vec![12; 32]
        );
        assert_eq!(
            a.read(&checkout.id, |doc| doc.base_heads()),
            b.read(&checkout.id, |doc| doc.base_heads())
        );
        assert_eq!(
            a.read(&checkout.id, |doc| serde_json::to_string(
                &automerge::AutoSerde::from(doc)
            )
            .unwrap()),
            b.read(&checkout.id, |doc| serde_json::to_string(
                &automerge::AutoSerde::from(doc)
            )
            .unwrap())
        );
    }
    #[test]
    fn invalid_later_app_cannot_partially_stage_earlier_admitted_record() {
        let (mut state, key, now) = state();
        let record = RecordId::for_tab(1);
        let mut writer = automerge::AutoCommit::load(
            &state
                .document("dev.qos.word-processor", record.as_str())
                .unwrap(),
        )
        .unwrap();
        use automerge::ReadDoc;
        let (_, metadata) = writer.get(automerge::ROOT, "tilo:record").unwrap().unwrap();
        writer.put(metadata, "room", "wrong-room").unwrap();
        writer.commit();
        state
            .publish_document(
                "dev.qos.word-processor",
                record.as_str(),
                writer.save(),
                &key,
                now,
            )
            .unwrap();
        let cache = PortableStorage::new("test", "invalid").unwrap();
        assert!(install(&cache, &state, [13; 32]).is_err());
        assert!(cache.pending_writes().is_empty());
    }
}

/// Capture through the canonical owner; a renderer must never commit a clone
/// of an unfinished typing transaction under the same Automerge actor.
#[cfg(test)]
pub(crate) fn capture(records: &ui::record::RecordStore, id: &RecordId) -> Result<Vec<u8>, String> {
    Ok(capture_owned(records, id)?.save())
}
pub(crate) fn capture_owned(
    records: &ui::record::RecordStore,
    id: &RecordId,
) -> Result<automerge::AutoCommit, String> {
    use automerge::transaction::Transactable;
    // try_flush stamps the real owned transaction before queuing persistence.
    // Its expected "not yet durable" error does not undo that canonical commit.
    let _ = records.try_flush();
    records
        .read(id, |doc| {
            if doc.pending_ops() == 0 {
                Ok(doc.clone())
            } else {
                Err("record mutation is still unfinished".into())
            }
        })
        .ok_or("record unavailable")?
}
#[cfg(test)]
mod capture_tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    use ui::{
        application::{ComponentRunnerInputs, HostContext, LaunchTarget, RestoreInput},
        application_view::InstanceViewStore,
        permissions::{Actor, PermissionContext, Role, UserId},
    };
    #[test]
    fn actual_chat_typing_capture_matches_the_real_owned_commit() {
        let cache = PortableStorage::new("chat-capture", "actor").unwrap();
        let definition = chat::definition();
        let records = RecordOwners::portable(cache.clone())
            .open(definition.app_id, "room")
            .unwrap();
        let views = Rc::new(InstanceViewStore::portable(cache, "room").unwrap());
        let permissions = Rc::new(RefCell::new(PermissionContext::from_capabilities(
            Actor::person(Some(Role::Owner), "author"),
            vec![],
        )));
        let context = HostContext::prepare(
            definition,
            records.clone(),
            views,
            "actor".into(),
            1,
            permissions,
            UserId("author".into()),
            tilo_actions::ActionContext::default(),
            &LaunchTarget::PrimaryNew,
        )
        .unwrap();
        let component = definition
            .instantiate(context, LaunchTarget::PrimaryNew, RestoreInput::None)
            .unwrap();
        let record = component.record().id.clone();
        let mut frame = ui::headless::FrameHarness::from_component(
            component,
            (900, 640),
            ComponentRunnerInputs {
                theme: ui::Theme::dark(),
                text_renderer: ui::TextRenderer::new(),
            },
        )
        .unwrap();
        frame.click_at(450.0, 580.0);
        frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
            "S".into(),
        )));
        assert_eq!(
            records
                .read(&record, |doc| chat::doc::ChatDoc::draft_text(
                    doc,
                    "c_general"
                ))
                .unwrap(),
            "S"
        );
        let mut published = PublishedRecords::default();
        assert!(
            published
                .publish(1, definition.app_id, &records, &record, |_| Ok(()))
                .unwrap()
        );
        assert!(
            !published
                .publish(1, definition.app_id, &records, &record, |_| panic!(
                    "unchanged typing frame"
                ))
                .unwrap()
        );
        let captured = capture(&records, &record).unwrap();
        let mut sent = automerge::AutoCommit::load(&captured).unwrap();
        let _ = records.try_flush();
        let actual = records.read(&record, |doc| doc.clone().save()).unwrap();
        let mut actual = automerge::AutoCommit::load(&actual).unwrap();
        assert_eq!(
            sent.get_heads(),
            actual.get_heads(),
            "captured bytes must carry the owner's stamped transaction, not a separately committed clone"
        );
        sent.merge(&mut actual).unwrap();
        let checkout = RecordCheckout::package_primary(definition.record_kind.clone());
        let mut remote = prepare_remote_owned(&records, &record).unwrap();
        assert!(
            records.can_merge_admitted(&checkout).unwrap(),
            "external checkpoint must close the actual owned typing boundary before merge"
        );
        remote.set_actor(automerge::ActorId::from(vec![88; 32]));
        use automerge::transaction::Transactable;
        remote
            .put(automerge::ROOT, "remote-checkpoint-probe", true)
            .unwrap();
        remote.commit();
        assert_eq!(
            records
                .merge_admitted(
                    &checkout,
                    &qos_peer_state::DocumentMutation::Snapshot(remote.save())
                )
                .unwrap(),
            Some(true)
        );
        frame.relayout();
        assert!(
            published
                .publish(1, definition.app_id, &records, &record, |_| Ok(()))
                .unwrap(),
            "remote merge changes the observed heads"
        );
        let before = records.read(&record, |doc| doc.base_heads()).unwrap();
        frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
            "T".into(),
        )));
        assert_eq!(
            records.read(&record, |doc| doc.base_heads()).unwrap(),
            before
        );
        assert!(records.read(&record, |doc| doc.pending_ops() > 0).unwrap());
        let mut sent_typing = None;
        assert!(
            published
                .publish(1, definition.app_id, &records, &record, |bytes| {
                    sent_typing = Some(bytes);
                    Ok(())
                })
                .unwrap(),
            "unfinished typing must publish despite unchanged heads"
        );
        let sent_typing = sent_typing.unwrap();
        assert_eq!(
            sent_typing.pending_ops(),
            0,
            "publication receives only the canonical committed transaction"
        );
        assert_eq!(
            chat::doc::ChatDoc::draft_text(&sent_typing, "c_general"),
            "ST"
        );
        let _ = capture(&records, &record).unwrap();
        assert_eq!(
            records
                .read(&record, |doc| chat::doc::ChatDoc::draft_text(
                    doc,
                    "c_general"
                ))
                .unwrap(),
            "ST"
        );
        frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
            "U".into(),
        )));
        assert!(records.read(&record, |doc| doc.pending_ops() > 0).unwrap());
        let prepared = prepare_remote_owned(&records, &record).unwrap();
        assert_eq!(prepared.pending_ops(), 0);
        assert_eq!(
            prepared.base_heads(),
            records.read(&record, |doc| doc.base_heads()).unwrap()
        );
        assert_eq!(
            chat::doc::ChatDoc::draft_text(&prepared, "c_general"),
            "STU"
        );
        assert!(records.can_merge_admitted(&checkout).unwrap());
    }
}

pub(crate) fn prepare_remote_owned(
    records: &ui::record::RecordStore,
    id: &RecordId,
) -> Result<automerge::AutoCommit, String> {
    // An incoming canonical merge is an explicit external history boundary.
    // The existing checked mutable loan stamps and closes the logical local burst.
    records
        .with(id, |_| {})
        .ok_or("record owner unavailable at remote boundary")?;
    capture_owned(records, id)
}

/// Coalesces changed-document publication while edits keep arriving. Each
/// publication captures the canonical owner, signs and encrypts a delta, encodes
/// the journal, clones peer state and queues a strict IndexedDB commit; doing
/// that at most once per window instead of once per keystroke bounds the work
/// between rendered frames. Edits wait at most one window; hiding, closing,
/// suspension, focus loss and IME end publish immediately. A zero window
/// publishes every changed frame.
pub(crate) struct PublicationSchedule {
    window: std::time::Duration,
    /// Configured window; `window` is shorter while [`Self::set_live_window`]
    /// applies (live collaborators receive each publication at once).
    configured: std::time::Duration,
    last: Option<ui::clock::Instant>,
    due: Option<ui::clock::Instant>,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PublicationDecision {
    Now,
    Deferred(ui::clock::Instant),
}
impl PublicationSchedule {
    pub(crate) fn new(window: std::time::Duration) -> Self {
        Self {
            window,
            configured: window,
            last: None,
            due: None,
        }
    }
    /// LIVE_V1: while other members are present, publications forward to them
    /// immediately (the author's own ops track), so a long window would be
    /// visible typing latency. `Some(window)` shortens the configured window
    /// (never lengthens it); `None` restores it. A pending deadline moves with
    /// the window.
    pub(crate) fn set_live_window(&mut self, window: Option<std::time::Duration>) {
        self.window = window.map_or(self.configured, |window| window.min(self.configured));
        if self.due.is_some()
            && let Some(last) = self.last
        {
            self.due = last.checked_add(self.window);
        }
    }
    /// Called when at least one owned record has unpublished changes.
    pub(crate) fn decide(&mut self, now: ui::clock::Instant) -> PublicationDecision {
        if let Some(last) = self.last
            && let Some(due) = last.checked_add(self.window)
            && now < due
        {
            self.due = Some(due);
            return PublicationDecision::Deferred(due);
        }
        self.mark_published(now);
        PublicationDecision::Now
    }
    /// A publication ran now: scheduled, at its deadline, or as a forced flush.
    pub(crate) fn mark_published(&mut self, now: ui::clock::Instant) {
        self.last = Some(now);
        self.due = None;
    }
    /// Nothing is left to publish (another path already published the records).
    pub(crate) fn clear_due(&mut self) {
        self.due = None;
    }
    pub(crate) fn due(&self) -> Option<ui::clock::Instant> {
        self.due
    }
    /// A different peer needs its own first publication without waiting.
    pub(crate) fn reset(&mut self) {
        self.last = None;
        self.due = None;
    }
}

/// Tracks document publication separately from presentation.
#[derive(Default)]
pub(crate) struct PublishedRecords {
    heads: std::collections::HashMap<(u32, String, RecordId), Vec<automerge::ChangeHash>>,
}
impl PublishedRecords {
    /// Whether `publish` would send anything. Unfinished typing always counts:
    /// it must still pass through capture's canonical commit boundary.
    pub(crate) fn changed(
        &self,
        peer: u32,
        app: &str,
        records: &ui::record::RecordStore,
        id: &RecordId,
    ) -> bool {
        use automerge::transaction::Transactable;
        let key = (peer, app.to_owned(), id.clone());
        records
            .read(id, |doc| {
                doc.pending_ops() != 0
                    || self
                        .heads
                        .get(&key)
                        .is_none_or(|heads| *heads != doc.base_heads())
            })
            .unwrap_or(false)
    }
    pub(crate) fn publish(
        &mut self,
        peer: u32,
        app: &str,
        records: &ui::record::RecordStore,
        id: &RecordId,
        send: impl FnOnce(automerge::AutoCommit) -> Result<(), String>,
    ) -> Result<bool, String> {
        use automerge::transaction::Transactable;
        let key = (peer, app.to_owned(), id.clone());
        let unchanged = records
            .read(id, |doc| {
                // Uncommitted typing can have the same heads as the last frame.
                // It must still pass through capture's canonical commit boundary.
                doc.pending_ops() == 0
                    && self
                        .heads
                        .get(&key)
                        .is_some_and(|heads| *heads == doc.base_heads())
            })
            .ok_or("record unavailable")?;
        if unchanged {
            return Ok(false);
        }
        let document = capture_owned(records, id)?;
        let heads = document.base_heads();
        send(document)?;
        // A failed journal/publication must not suppress the next retry.
        self.heads.insert(key, heads);
        Ok(true)
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;
    use automerge::{ReadDoc, transaction::Transactable};
    fn record() -> (ui::record::RecordStore, RecordId) {
        let store = ui::record::RecordStore::in_memory();
        let id = RecordId::for_tab(987);
        store
            .create_record(&RecordCheckout::new(
                id.clone(),
                ui::record::RecordKind::new("test"),
            ))
            .unwrap();
        store
            .with(&id, |doc| {
                doc.put(automerge::ROOT, "text", "initial").unwrap();
                doc.commit();
            })
            .unwrap();
        (store, id)
    }
    #[test]
    fn unchanged_frames_skip_publication_but_edits_and_new_peers_publish() {
        let (store, id) = record();
        let mut published = PublishedRecords::default();
        let mut payloads = Vec::new();
        assert!(
            published
                .publish(1, "test", &store, &id, |bytes| {
                    payloads.push(bytes);
                    Ok(())
                })
                .unwrap()
        );
        for _ in 0..120 {
            assert!(
                !published
                    .publish(1, "test", &store, &id, |_| panic!(
                        "unchanged frame serialized and published its document"
                    ))
                    .unwrap()
            );
        }
        store
            .with(&id, |doc| {
                doc.put(automerge::ROOT, "text", "edited").unwrap();
                doc.commit();
            })
            .unwrap();
        assert!(
            published
                .publish(1, "test", &store, &id, |bytes| {
                    payloads.push(bytes);
                    Ok(())
                })
                .unwrap()
        );
        let doc = &payloads[1];
        assert_eq!(
            doc.get(automerge::ROOT, "text")
                .unwrap()
                .unwrap()
                .0
                .to_str(),
            Some("edited")
        );
        assert!(
            published
                .publish(2, "test", &store, &id, |_| Ok(()))
                .unwrap()
        );
        assert!(
            published
                .publish(2, "different-app", &store, &id, |_| Ok(()))
                .unwrap()
        );
    }
    #[test]
    fn failed_publication_retries_the_same_heads() {
        let (store, id) = record();
        let mut published = PublishedRecords::default();
        assert!(
            published
                .publish(1, "test", &store, &id, |_| Err("storage failed".into()))
                .is_err()
        );
        assert!(
            published
                .publish(1, "test", &store, &id, |_| Ok(()))
                .unwrap()
        );
        store
            .with(&id, |doc| {
                doc.put(automerge::ROOT, "text", "retry edit").unwrap();
                doc.commit();
            })
            .unwrap();
        assert!(
            published
                .publish(1, "test", &store, &id, |_| Err("storage failed".into()))
                .is_err()
        );
        assert!(
            published
                .publish(1, "test", &store, &id, |_| Ok(()))
                .unwrap()
        );
        assert!(
            !published
                .publish(1, "test", &store, &id, |_| panic!("already published"))
                .unwrap()
        );
    }
}

#[cfg(test)]
mod schedule_tests {
    use super::*;
    use automerge::transaction::Transactable;
    use std::time::Duration;
    #[test]
    fn continuous_edits_publish_once_per_window_and_flushes_reset_the_deadline() {
        let t0 = ui::clock::Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut schedule = PublicationSchedule::new(Duration::from_millis(100));
        assert_eq!(schedule.decide(ms(0)), PublicationDecision::Now);
        assert_eq!(
            schedule.decide(ms(30)),
            PublicationDecision::Deferred(ms(100))
        );
        assert_eq!(
            schedule.decide(ms(60)),
            PublicationDecision::Deferred(ms(100))
        );
        assert_eq!(schedule.due(), Some(ms(100)));
        assert_eq!(schedule.decide(ms(100)), PublicationDecision::Now);
        assert_eq!(schedule.due(), None);
        assert_eq!(
            schedule.decide(ms(150)),
            PublicationDecision::Deferred(ms(200))
        );
        schedule.mark_published(ms(160));
        assert_eq!(schedule.due(), None);
        assert_eq!(
            schedule.decide(ms(200)),
            PublicationDecision::Deferred(ms(260))
        );
        schedule.clear_due();
        assert_eq!(schedule.due(), None);
        schedule.reset();
        assert_eq!(schedule.decide(ms(201)), PublicationDecision::Now);
    }
    #[test]
    fn live_collaboration_shortens_the_window_and_moves_the_pending_deadline() {
        let t0 = ui::clock::Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut schedule = PublicationSchedule::new(Duration::from_millis(100));
        assert_eq!(schedule.decide(ms(0)), PublicationDecision::Now);
        assert_eq!(
            schedule.decide(ms(5)),
            PublicationDecision::Deferred(ms(100))
        );
        // A collaborator arrives: the pending edit is due one live window after
        // the last publication instead of the configured window.
        schedule.set_live_window(Some(Duration::from_millis(16)));
        assert_eq!(schedule.due(), Some(ms(16)));
        assert_eq!(schedule.decide(ms(16)), PublicationDecision::Now);
        assert_eq!(
            schedule.decide(ms(20)),
            PublicationDecision::Deferred(ms(32))
        );
        // Never longer than configured.
        schedule.set_live_window(Some(Duration::from_secs(5)));
        assert_eq!(schedule.due(), Some(ms(116)));
        // Alone again: the configured window applies.
        schedule.set_live_window(Some(Duration::from_millis(16)));
        schedule.set_live_window(None);
        assert_eq!(schedule.due(), Some(ms(116)));
        assert_eq!(
            schedule.decide(ms(40)),
            PublicationDecision::Deferred(ms(116))
        );
        // Nothing pending: no deadline is invented.
        schedule.clear_due();
        schedule.set_live_window(Some(Duration::from_millis(16)));
        assert_eq!(schedule.due(), None);
    }
    #[test]
    fn a_zero_window_publishes_every_changed_frame() {
        let now = ui::clock::Instant::now();
        let mut schedule = PublicationSchedule::new(Duration::ZERO);
        assert_eq!(schedule.decide(now), PublicationDecision::Now);
        assert_eq!(schedule.decide(now), PublicationDecision::Now);
        assert_eq!(schedule.due(), None);
    }
    #[test]
    fn changed_records_are_detected_without_capturing_or_publishing() {
        let store = ui::record::RecordStore::in_memory();
        let id = RecordId::for_tab(989);
        store
            .create_record(&RecordCheckout::new(
                id.clone(),
                ui::record::RecordKind::new("test"),
            ))
            .unwrap();
        store
            .with(&id, |doc| {
                doc.put(automerge::ROOT, "text", "initial").unwrap();
                doc.commit();
            })
            .unwrap();
        let mut published = PublishedRecords::default();
        assert!(published.changed(1, "test", &store, &id));
        published
            .publish(1, "test", &store, &id, |_| Ok(()))
            .unwrap();
        assert!(!published.changed(1, "test", &store, &id));
        assert!(
            published.changed(2, "test", &store, &id),
            "another peer has not seen this document"
        );
        store
            .with(&id, |doc| {
                doc.put(automerge::ROOT, "text", "typing").unwrap();
            })
            .unwrap();
        assert!(
            published.changed(1, "test", &store, &id),
            "unfinished typing must reach the commit boundary"
        );
        store.with(&id, |doc| doc.commit()).unwrap();
        assert!(published.changed(1, "test", &store, &id));
        published
            .publish(1, "test", &store, &id, |_| Ok(()))
            .unwrap();
        assert!(!published.changed(1, "test", &store, &id));
        assert!(
            !published.changed(1, "test", &store, &RecordId::for_tab(1)),
            "unknown records never publish"
        );
    }
}

#[cfg(test)]
mod publication_benchmark {
    use super::*;
    use automerge::transaction::Transactable;
    #[test]
    #[ignore = "manual CPU microbenchmark, not a browser frame-rate gate"]
    fn unchanged_document_publication_cost() {
        let cache = PortableStorage::new("publication-benchmark", "browser").unwrap();
        let store = RecordOwners::portable(cache).open("test", "room").unwrap();
        let id = RecordId::for_tab(988);
        store
            .create_record(&RecordCheckout::new(
                id.clone(),
                ui::record::RecordKind::new("test"),
            ))
            .unwrap();
        store
            .with(&id, |doc| {
                for i in 0..100 {
                    doc.put(
                        automerge::ROOT,
                        "body",
                        format!("{i}:{}", "x".repeat(65536)),
                    )
                    .unwrap();
                    doc.commit();
                }
            })
            .unwrap();
        let prior = capture(&store, &id).unwrap();
        let mut published = PublishedRecords::default();
        published
            .publish(1, "test", &store, &id, |_| Ok(()))
            .unwrap();
        let mut baseline = Vec::new();
        let mut candidate = Vec::new();
        for _ in 0..200 {
            let start = std::time::Instant::now();
            let bytes = capture(&store, &id).unwrap();
            let before = automerge::AutoCommit::load(&prior).unwrap();
            let after = automerge::AutoCommit::load(&bytes).unwrap();
            assert_eq!(before.base_heads(), after.base_heads());
            baseline.push(start.elapsed().as_secs_f64() * 1000.0);
            let start = std::time::Instant::now();
            assert!(
                !published
                    .publish(1, "test", &store, &id, |_| panic!(
                        "unchanged document published"
                    ))
                    .unwrap()
            );
            candidate.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        baseline.sort_by(f64::total_cmp);
        candidate.sort_by(f64::total_cmp);
        eprintln!(
            "publication CPU ms (200 samples; native debug; 100 edits of 64KiB): baseline p50={:.3} p99={:.3}; candidate p50={:.3} p99={:.3}",
            baseline[99], baseline[197], candidate[99], candidate[197]
        );
    }
}
