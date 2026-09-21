//! Public-command regressions; portable cases never create native authority.
use super::*;
#[cfg(target_os = "linux")]
use crate::connected::ConflictChoice;
#[cfg(not(target_os = "linux"))]
use crate::connected::RecoveryId;
use crate::connected::SelectionReviewId;

// Removing the facade bound would let callers monopolize the serialized owner.
#[test]
fn recovery_commands_bound_pages_and_reject_foreign_reviews() {
    let (temp, mut store) = fixture_store();
    let id = seed(&mut store, 1, temp.path());
    let service = spawn(store).unwrap();
    for limit in [0, 65, usize::MAX] {
        assert!(matches!(
            service.recovery(id, None, limit),
            Err(ConnectionError::LimitExceeded)
        ));
    }
    let page = service.recovery(id, None, 1).unwrap().wait().unwrap();
    assert!(page.entries.is_empty());
    assert!(page.next.is_none());
    let observer = service.reopen(id).unwrap().wait().unwrap();
    let revision = observer.snapshot().connection.revision;
    let review = SelectionReviewId(ReviewId {
        generation: 1,
        lifetime: Arc::new(()),
    });
    assert_eq!(
        service.restore(review.clone()).unwrap().wait(),
        Err(ConnectionError::StaleReview)
    );
    assert_eq!(
        service.replace(review).unwrap().wait(),
        Err(ConnectionError::StaleReview)
    );
    assert_eq!(observer.snapshot().connection.revision, revision);
    service.disconnect(id).unwrap().wait().unwrap();
    assert!(
        service
            .recovery(id, None, 64)
            .unwrap()
            .wait()
            .unwrap()
            .entries
            .is_empty()
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn recovery_preview_on_unavailable_platform_never_touches_endpoints() {
    let (temp, mut store) = fixture_store();
    let id = seed(&mut store, 2, temp.path());
    let service = spawn(store).unwrap();
    service.reopen(id).unwrap().wait().unwrap();
    let candidate = RecoveryId {
        connection: id,
        operation: 1,
        slot: 0,
    };
    assert_eq!(
        service
            .preview_recovery(candidate, Side::Source)
            .unwrap()
            .wait()
            .err(),
        Some(ConnectionError::UnsupportedHost)
    );
    assert!(!temp.path().join("unavailable-source-2").exists());
    assert!(!temp.path().join("unavailable-destination-2").exists());
}

#[cfg(target_os = "linux")]
fn baseline(fixture: &NativeFixture) -> (ConnectedService, ConnectionInfo, Observer) {
    std::fs::write(fixture.source.join("document"), b"original").unwrap();
    let service = fixture.service();
    let info = fixture.create(&service);
    let observer = service.start(info.id).unwrap().wait().unwrap();
    accept(&service, &observer);
    assert_native_up_to_date(&observer);
    (service, info, observer)
}

#[cfg(target_os = "linux")]
fn divergent(
    service: &ConnectedService,
    fixture: &NativeFixture,
    id: ConnectionId,
) -> crate::connected::ConflictInfo {
    std::fs::write(fixture.source.join("document"), b"source edit").unwrap();
    std::fs::write(fixture.destination.join("document"), b"destination edit").unwrap();
    let observer = service.start(id).unwrap().wait().unwrap();
    assert_eq!(terminal(&observer).phase, Phase::Conflict);
    service
        .conflicts(id, None)
        .unwrap()
        .wait()
        .unwrap()
        .entries
        .into_iter()
        .find(|c| !c.resolved)
        .unwrap()
}

#[cfg(target_os = "linux")]
fn source_choice(conflict: &crate::connected::ConflictInfo) -> ConflictChoice {
    conflict.choices.iter().find(|choice| matches!(choice, ConflictChoice::Recovery(entry) if entry.original_side == Side::Source)).unwrap().clone()
}

#[cfg(target_os = "linux")]
fn preserved_bytes(fixture: &NativeFixture, entry: &crate::connected::RecoveryInfo) -> Vec<u8> {
    let connection: String = entry
        .id
        .connection
        .0
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    std::fs::read(
        fixture
            .store
            .join("recovery")
            .join(connection)
            .join(format!("{}-{}", entry.id.operation, entry.id.slot)),
    )
    .unwrap()
}

// Wrong default/replacement routing loses the occupied destination or fails to
// resolve using the explicitly selected real preserved source version.
#[cfg(target_os = "linux")]
#[test]
fn native_recovery_commands_preserve_collision_then_explicitly_resolve() {
    let fixture = NativeFixture::new();
    let (service, info, observer) = baseline(&fixture);
    let conflict = divergent(&service, &fixture, info.id);
    assert_eq!(
        conflict
            .choices
            .iter()
            .filter(|c| matches!(c, ConflictChoice::Recovery(_)))
            .count(),
        2
    );
    let preview = service
        .preview_conflict(
            info.id,
            conflict.serial,
            conflict.revision,
            source_choice(&conflict),
            Side::Destination,
        )
        .unwrap()
        .wait()
        .unwrap();
    assert!(preview.destination.is_some());
    service.restore(preview.review).unwrap().wait().unwrap();
    assert_eq!(terminal(&observer).phase, Phase::Conflict);
    assert_eq!(
        std::fs::read(fixture.destination.join("document")).unwrap(),
        b"destination edit"
    );
    let collision = service
        .conflicts(info.id, None)
        .unwrap()
        .wait()
        .unwrap()
        .entries
        .into_iter()
        .max_by_key(|c| c.serial)
        .unwrap();
    assert!(collision.choices.iter().any(|choice| matches!(choice, ConflictChoice::Recovery(entry) if entry.id.operation == conflict.serial && entry.original_side == Side::Source)));
    let preview = service
        .preview_conflict(
            info.id,
            collision.serial,
            collision.revision,
            source_choice(&conflict),
            Side::Destination,
        )
        .unwrap()
        .wait()
        .unwrap();
    service.replace(preview.review).unwrap().wait().unwrap();
    // Consent to the collision does not resolve the original conflict.
    let after_collision = terminal(&observer);
    assert!(matches!(after_collision.phase, Phase::Pending(_)));
    assert!(!after_collision.pending_operation);
    assert_eq!(
        std::fs::read(fixture.source.join("document")).unwrap(),
        b"source edit"
    );
    assert_eq!(
        std::fs::read(fixture.destination.join("document")).unwrap(),
        b"source edit"
    );
    let still_open = service.conflicts(info.id, None).unwrap().wait().unwrap();
    assert_eq!(
        still_open
            .entries
            .iter()
            .filter(|entry| !entry.resolved)
            .count(),
        2
    );
    assert!(
        still_open
            .entries
            .iter()
            .any(|entry| entry.serial == collision.serial && !entry.resolved)
    );
    let original = still_open
        .entries
        .into_iter()
        .find(|entry| entry.serial == conflict.serial)
        .unwrap();
    assert!(!original.resolved);
    let preview = service
        .preview_conflict(
            info.id,
            original.serial,
            original.revision,
            source_choice(&original),
            Side::Destination,
        )
        .unwrap()
        .wait()
        .unwrap();
    service.replace(preview.review).unwrap().wait().unwrap();
    assert_native_up_to_date(&observer);
    assert_eq!(
        std::fs::read(fixture.source.join("document")).unwrap(),
        b"source edit"
    );
    assert_eq!(
        std::fs::read(fixture.destination.join("document")).unwrap(),
        b"source edit"
    );
    let recovery = service.recovery(info.id, None, 64).unwrap().wait().unwrap();
    assert!(
        recovery
            .entries
            .iter()
            .any(|entry| preserved_bytes(&fixture, entry) == b"source edit")
    );
    assert!(
        recovery
            .entries
            .iter()
            .any(|entry| preserved_bytes(&fixture, entry) == b"destination edit")
    );
    assert!(recovery.entries.iter().any(|entry| {
        entry.original_side == Side::Destination
            && entry
                .content
                .as_ref()
                .is_some_and(|state| matches!(state, EntryState::File { bytes: 16, .. }))
    }));
    assert!(
        service
            .conflicts(info.id, None)
            .unwrap()
            .wait()
            .unwrap()
            .entries
            .iter()
            .all(|c| c.resolved)
    );
}

// Losing selection freshness would replace a change made after the displayed
// review. Pause/restart and a foreign worker must invalidate the same token.
#[cfg(target_os = "linux")]
#[test]
fn native_recovery_commands_refuse_changed_bytes_revision_pause_and_foreign_worker() {
    let fixture = NativeFixture::new();
    let (service, info, observer) = baseline(&fixture);
    let conflict = divergent(&service, &fixture, info.id);
    assert_eq!(
        service
            .preview_conflict(
                info.id,
                conflict.serial,
                conflict.revision + 1,
                source_choice(&conflict),
                Side::Destination
            )
            .unwrap()
            .wait()
            .err(),
        Some(ConnectionError::StaleReview)
    );
    let preview = service
        .preview_conflict(
            info.id,
            conflict.serial,
            conflict.revision,
            source_choice(&conflict),
            Side::Destination,
        )
        .unwrap()
        .wait()
        .unwrap();
    std::fs::write(fixture.destination.join("document"), b"new unreviewed edit").unwrap();
    assert_eq!(
        service.replace(preview.review.clone()).unwrap().wait(),
        Err(ConnectionError::StaleReview)
    );
    assert_eq!(
        std::fs::read(fixture.destination.join("document")).unwrap(),
        b"new unreviewed edit"
    );
    assert_ne!(observer.snapshot().phase, Phase::UpToDate);
    let preview = service
        .preview_conflict(
            info.id,
            conflict.serial,
            conflict.revision,
            source_choice(&conflict),
            Side::Destination,
        )
        .unwrap()
        .wait()
        .unwrap();
    std::fs::write(fixture.source.join("document"), b"new source edit").unwrap();
    assert_eq!(
        service.replace(preview.review).unwrap().wait(),
        Err(ConnectionError::StaleReview)
    );
    assert_eq!(
        std::fs::read(fixture.destination.join("document")).unwrap(),
        b"new unreviewed edit"
    );
    let preview = service
        .preview_conflict(
            info.id,
            conflict.serial,
            conflict.revision,
            source_choice(&conflict),
            Side::Destination,
        )
        .unwrap()
        .wait()
        .unwrap();
    service.pause(info.id).unwrap().wait().unwrap();
    assert_eq!(
        service.replace(preview.review.clone()).unwrap().wait(),
        Err(ConnectionError::StaleReview)
    );
    drop(service);
    let service = reopen_service(&fixture);
    assert_eq!(
        service.replace(preview.review).unwrap().wait(),
        Err(ConnectionError::StaleReview)
    );
    assert!(
        !service
            .recovery(info.id, None, 64)
            .unwrap()
            .wait()
            .unwrap()
            .entries
            .is_empty()
    );
}

// A metadata-only restore could claim success while never producing the lost
// bytes. The public command must restore the actual preserved native content.
#[cfg(target_os = "linux")]
#[test]
fn native_recovery_commands_restore_deleted_bytes_and_keep_disconnected_inventory() {
    let fixture = NativeFixture::new();
    let (service, info, observer) = baseline(&fixture);
    std::fs::remove_file(fixture.source.join("document")).unwrap();
    service.start(info.id).unwrap().wait().unwrap();
    assert_native_up_to_date(&observer);
    assert!(!fixture.destination.join("document").exists());
    let recovery = service.recovery(info.id, None, 1).unwrap().wait().unwrap();
    let selected = recovery
        .entries
        .into_iter()
        .find(|entry| entry.original_path == RelativePath::unix(b"document").unwrap())
        .unwrap();
    let preview = service
        .preview_recovery(selected.id, Side::Source)
        .unwrap()
        .wait()
        .unwrap();
    assert!(preview.source.is_none());
    assert!(preview.destination.is_none());
    service.restore(preview.review).unwrap().wait().unwrap();
    let partial = terminal(&observer);
    assert_ne!(partial.phase, Phase::UpToDate);
    assert!(!partial.pending_operation);
    assert_eq!(
        std::fs::read(fixture.source.join("document")).unwrap(),
        b"original"
    );
    assert!(!fixture.destination.join("document").exists());
    // The first review authorized Source only. Destination requires its own
    // displayed current state and explicit acceptance before equality can close.
    let destination_review = service
        .preview_recovery(selected.id, Side::Destination)
        .unwrap()
        .wait()
        .unwrap();
    assert!(destination_review.source.is_some());
    assert!(destination_review.destination.is_none());
    service
        .restore(destination_review.review)
        .unwrap()
        .wait()
        .unwrap();

    assert_native_up_to_date(&observer);
    assert_eq!(
        std::fs::read(fixture.source.join("document")).unwrap(),
        b"original"
    );
    assert_eq!(
        std::fs::read(fixture.destination.join("document")).unwrap(),
        b"original"
    );
    let preview = service
        .preview_recovery(selected.id, Side::Source)
        .unwrap()
        .wait()
        .unwrap();
    service.disconnect(info.id).unwrap().wait().unwrap();
    assert_eq!(
        service.replace(preview.review).unwrap().wait(),
        Err(ConnectionError::StaleReview)
    );
    let before = service
        .recovery(info.id, None, 64)
        .unwrap()
        .wait()
        .unwrap()
        .entries;
    drop(service);
    let service = reopen_service(&fixture);
    let after = service
        .recovery(info.id, None, 64)
        .unwrap()
        .wait()
        .unwrap()
        .entries;
    assert_eq!(before, after);
    service.reopen(info.id).unwrap().wait().unwrap();
    assert_eq!(
        service
            .preview_recovery(selected.id, Side::Source)
            .unwrap()
            .wait()
            .err(),
        Some(ConnectionError::Disconnected)
    );
}

// Dropping recorded tombstone choices would make explicit deletion resolution
// inaccessible; choosing it must still preserve the live edited target.
#[cfg(target_os = "linux")]
#[test]
fn native_recovery_commands_offer_recorded_tombstone_and_preserve_displaced_edit() {
    let fixture = NativeFixture::new();
    let (service, info, observer) = baseline(&fixture);
    std::fs::remove_file(fixture.source.join("document")).unwrap();
    std::fs::write(
        fixture.destination.join("document"),
        b"edited after deletion",
    )
    .unwrap();
    service.start(info.id).unwrap().wait().unwrap();
    assert_eq!(terminal(&observer).phase, Phase::Conflict);
    let conflict = service
        .conflicts(info.id, None)
        .unwrap()
        .wait()
        .unwrap()
        .entries
        .into_iter()
        .find(|entry| !entry.resolved)
        .unwrap();
    let choice = conflict.choices.iter().find(|choice| matches!(choice, ConflictChoice::Tombstone(value) if value.original_side() == Side::Source)).unwrap().clone();
    let preview = service
        .preview_conflict(
            info.id,
            conflict.serial,
            conflict.revision,
            choice,
            Side::Destination,
        )
        .unwrap()
        .wait()
        .unwrap();
    assert!(preview.candidate.is_none());
    service.replace(preview.review).unwrap().wait().unwrap();
    assert_native_up_to_date(&observer);
    assert!(!fixture.source.join("document").exists());
    assert!(!fixture.destination.join("document").exists());
    assert!(
        service
            .recovery(info.id, None, 64)
            .unwrap()
            .wait()
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry
                .content
                .as_ref()
                .is_some_and(|state| matches!(state, EntryState::File { bytes: 21, .. })))
    );
}

// A stale cursor must never silently drift across durable mutations; a current
// review cannot authorize writes to a replacement endpoint at the same path.
#[cfg(target_os = "linux")]
#[test]
fn native_recovery_commands_page_stably_and_refuse_replaced_endpoint() {
    let fixture = NativeFixture::new();
    let (service, info, observer) = baseline(&fixture);
    let conflict = divergent(&service, &fixture, info.id);
    let page = service.recovery(info.id, None, 1).unwrap().wait().unwrap();
    assert_eq!(page.entries.len(), 1);
    let first = page.entries[0].id;
    let next = service
        .recovery(info.id, page.next, 1)
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(next.entries.len(), 1);
    assert_ne!(first, next.entries[0].id);
    let stale = service
        .recovery(info.id, None, 1)
        .unwrap()
        .wait()
        .unwrap()
        .next
        .unwrap();
    let preview = service
        .preview_conflict(
            info.id,
            conflict.serial,
            conflict.revision,
            source_choice(&conflict),
            Side::Destination,
        )
        .unwrap()
        .wait()
        .unwrap();
    let moved = fixture.destination.with_file_name("original-destination");
    std::fs::rename(&fixture.destination, &moved).unwrap();
    std::fs::create_dir(&fixture.destination).unwrap();
    std::fs::write(fixture.destination.join("document"), b"unrelated folder").unwrap();
    assert!(service.replace(preview.review).unwrap().wait().is_err());
    assert_eq!(
        std::fs::read(fixture.destination.join("document")).unwrap(),
        b"unrelated folder"
    );
    assert_eq!(
        std::fs::read(moved.join("document")).unwrap(),
        b"destination edit"
    );
    assert!(observer.snapshot().endpoint_issue.is_some());
    service.disconnect(info.id).unwrap().wait().unwrap();
    assert_eq!(
        service
            .recovery(info.id, Some(stale), 1)
            .unwrap()
            .wait()
            .err(),
        Some(ConnectionError::StaleReview)
    );
}

// The accepted command owns its durable step even if both command and response
// handles disappear before the UI can observe completion.
#[cfg(target_os = "linux")]
#[test]
fn native_recovery_commands_complete_accepted_replacement_after_ui_drop() {
    let fixture = NativeFixture::new();
    let (service, info, observer) = baseline(&fixture);
    let conflict = divergent(&service, &fixture, info.id);
    let preview = service
        .preview_conflict(
            info.id,
            conflict.serial,
            conflict.revision,
            source_choice(&conflict),
            Side::Destination,
        )
        .unwrap()
        .wait()
        .unwrap();
    let request = service.replace(preview.review).unwrap();
    drop(service);
    drop(request);
    let deadline = Instant::now() + Duration::from_secs(90);
    while observer.snapshot().phase != Phase::Detached {
        assert!(
            Instant::now() < deadline,
            "accepted replacement did not finish: {:?}",
            observer.snapshot()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        std::fs::read(fixture.destination.join("document")).unwrap(),
        b"source edit"
    );
    let service = reopen_service(&fixture);
    assert!(
        service
            .recovery(info.id, None, 64)
            .unwrap()
            .wait()
            .unwrap()
            .entries
            .iter()
            .any(|entry| preserved_bytes(&fixture, entry) == b"destination edit")
    );
    // Detached completion is not an optimistic comparison claim. A fresh run
    // must finish the native equality/baseline receipt before reporting current.
    let observer = service.start(info.id).unwrap().wait().unwrap();
    assert_native_up_to_date(&observer);
}

// Durable candidate metadata cannot authorize publication when its current
// private bytes fail the native reader's paid integrity proof.
#[cfg(target_os = "linux")]
#[test]
fn native_recovery_commands_refuse_changed_private_candidate_and_pending_recovery() {
    let fixture = NativeFixture::new();
    let (service, info, observer) = baseline(&fixture);
    let conflict = divergent(&service, &fixture, info.id);
    let choice = source_choice(&conflict);
    let preview = service
        .preview_conflict(
            info.id,
            conflict.serial,
            conflict.revision,
            choice.clone(),
            Side::Destination,
        )
        .unwrap()
        .wait()
        .unwrap();
    let ConflictChoice::Recovery(entry) = &choice else {
        panic!("expected preserved source")
    };
    let connection: String = entry
        .id
        .connection
        .0
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let path = fixture
        .store
        .join("recovery")
        .join(connection)
        .join(format!("{}-{}", entry.id.operation, entry.id.slot));
    std::fs::write(path, b"bad choice!").unwrap();
    assert!(service.replace(preview.review).unwrap().wait().is_err());
    assert_eq!(
        std::fs::read(fixture.destination.join("document")).unwrap(),
        b"destination edit"
    );
    assert_ne!(observer.snapshot().phase, Phase::UpToDate);
    assert!(observer.snapshot().pending_operation);
    assert_eq!(
        service
            .preview_conflict(
                info.id,
                conflict.serial,
                conflict.revision,
                choice,
                Side::Destination
            )
            .unwrap()
            .wait()
            .err(),
        Some(ConnectionError::RecoveryRequired)
    );
}

// Removing key-range pruning makes an empty/final/late page read unrelated
// checkpointed history, and shadowed rows must appear exactly once.
#[test]
fn recovery_pages_seek_checkpointed_history_and_preserve_overlay_cursor_rules() {
    use crate::connection::store::worker_projection_tests;
    let (temp, mut store) = fixture_store();
    seed(&mut store, 1, temp.path());
    let id = seed(&mut store, 127, temp.path());
    let empty = seed(&mut store, 128, temp.path());
    seed(&mut store, 250, temp.path());
    let unrelated = worker_projection_tests::checkpoint_inventory(&mut store, id);
    worker_projection_tests::shadow_first(&mut store, id);
    let selected_page = worker_projection_tests::selected_page(&store, id);
    let reads = worker_projection_tests::watch(&temp.path().canonicalize().unwrap().join("store"));
    // Make unrelated leaf bytes unavailable only after a real durable checkpoint.
    // Correct prefix pruning never needs these leaves; selected pages stay intact.
    let hidden: Vec<_> = unrelated
        .into_iter()
        .map(|path| {
            let hidden = path.with_extension("hidden");
            std::fs::rename(&path, &hidden).unwrap();
            (path, hidden)
        })
        .collect();
    let service = spawn(store).unwrap();
    let first = service.recovery(id, None, 64).unwrap().wait().unwrap();
    assert_eq!(first.entries.len(), 64);
    assert_eq!(
        first.entries[0].original_path,
        RelativePath::unix(b"overlay-shadow").unwrap()
    );
    assert_eq!(first.entries[1].id.operation, 2);
    let middle = service
        .recovery(id, first.next, 64)
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(middle.entries[0].id.operation, 65);
    assert_eq!(middle.entries.len(), 64);
    let last = service
        .recovery(id, middle.next, 64)
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(
        last.entries
            .iter()
            .map(|entry| entry.id.operation)
            .collect::<Vec<_>>(),
        vec![129, 130]
    );
    assert!(last.next.is_none());
    assert!(
        service
            .recovery(empty, None, 64)
            .unwrap()
            .wait()
            .unwrap()
            .entries
            .is_empty()
    );
    assert!(
        service
            .conflicts(id, None)
            .unwrap()
            .wait()
            .unwrap()
            .entries
            .is_empty()
    );
    let cursor = service
        .recovery(id, None, 1)
        .unwrap()
        .wait()
        .unwrap()
        .next
        .unwrap();
    for (path, hidden) in hidden {
        std::fs::rename(hidden, path).unwrap();
    }
    let observed: Vec<_> = reads.try_iter().collect();
    assert_eq!(observed.len(), 6);
    for stats in observed {
        eprintln!("checkpointed facade page actual reads: {stats:?}");
        assert!(
            stats.pages > 0 && stats.pages <= 8,
            "range seek reread unrelated history: {stats:?}"
        );
        assert!(
            stats.requests > 0 && stats.bytes <= 256 * 1024,
            "page requests exceeded a small query: {stats:?}"
        );
        assert!(
            stats.work <= 16_384,
            "projection work did not stay local: {stats:?}"
        );
    }
    // Pruning never excuses corruption of a page actually referenced by the query.
    let hidden_selected = selected_page.with_extension("selected-hidden");
    std::fs::rename(&selected_page, &hidden_selected).unwrap();
    assert_eq!(
        service.recovery(id, None, 64).unwrap().wait().err(),
        Some(ConnectionError::Persistence)
    );
    std::fs::rename(hidden_selected, &selected_page).unwrap();
    let original = std::fs::read(&selected_page).unwrap();
    std::fs::write(&selected_page, b"damaged selected page").unwrap();
    assert_eq!(
        service.recovery(id, None, 64).unwrap().wait().err(),
        Some(ConnectionError::CorruptStore)
    );
    std::fs::write(&selected_page, original).unwrap();
    assert_eq!(
        service
            .recovery(id, None, 64)
            .unwrap()
            .wait()
            .unwrap()
            .entries
            .len(),
        64
    );
    service.disconnect(id).unwrap().wait().unwrap();
    assert_eq!(
        service.recovery(id, Some(cursor), 1).unwrap().wait().err(),
        Some(ConnectionError::StaleReview)
    );
}

// A refused/abandoned projection must not leave an ambient budget attached to
// the store and poison subsequent public commands or ordinary mutations.
#[test]
fn recovery_projection_budget_exhaustion_and_unwind_release_the_scope() {
    use crate::connection::store::worker_projection_tests;
    let (temp, mut store) = fixture_store();
    let id = seed(&mut store, 7, temp.path());
    let path = temp.path().canonicalize().unwrap().join("store");
    let mut worker = Worker {
        store,
        active: None,
        lifetime: Arc::new(()),
        generation: 0,
    };
    let (sender, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
    let service = ConnectedService::from_sender(sender);
    for unwind in [false, true] {
        worker_projection_tests::fail_next_read(&path, unwind);
        let request = service.recovery(id, None, 1).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            worker.command(receiver.recv().unwrap())
        }));
        if unwind {
            assert!(result.is_err());
            assert_eq!(request.wait().err(), Some(ConnectionError::Closed));
        } else {
            assert!(result.is_ok());
            assert_eq!(request.wait().err(), Some(ConnectionError::LimitExceeded));
        }
        let request = service.recovery(id, None, 1).unwrap();
        worker.command(receiver.recv().unwrap());
        assert!(request.wait().unwrap().entries.is_empty());
    }
    let outer = worker.store.worker_projection().unwrap();
    assert!(matches!(
        worker.store.worker_projection(),
        Err(StoreError::Busy)
    ));
    outer.finish().unwrap();
    let request = service.disconnect(id).unwrap();
    worker.command(receiver.recv().unwrap());
    assert_eq!(
        request.wait().unwrap().lifecycle,
        ConnectionLifecycle::Disconnected
    );
}

// Deterministic native cut tests: real public queue, Worker dispatch and native
// engines; only the next separately scheduled comparison turn is deferred.
#[cfg(target_os = "linux")]
struct NativeCutWorker {
    worker: Worker,
    service: ConnectedService,
    commands: mpsc::Receiver<Command>,
}
#[cfg(target_os = "linux")]
impl NativeCutWorker {
    fn open(fixture: &NativeFixture) -> Self {
        let (sender, commands) = mpsc::sync_channel(COMMAND_CAPACITY);
        Self {
            worker: Worker {
                store: ConnectionStore::open_at(&fixture.store).unwrap(),
                active: None,
                lifetime: Arc::new(()),
                generation: 0,
            },
            service: ConnectedService::from_sender(sender),
            commands,
        }
    }
    fn command<T>(&mut self, request: crate::connected::Request<T>) -> T {
        self.worker.command(self.commands.recv().unwrap());
        request.wait().unwrap()
    }
    fn settle(&mut self) -> Snapshot {
        let mut steps = 0;
        while self
            .worker
            .active
            .as_ref()
            .is_some_and(|active| active.running)
        {
            assert!(steps < 32, "bounded real worker did not settle");
            self.worker.step();
            steps += 1;
        }
        self.worker.active.as_ref().unwrap().observer.snapshot()
    }
}

#[cfg(target_os = "linux")]
fn native_attention_cut(
    fixture: &NativeFixture,
    checkpoint: bool,
) -> (
    NativeCutWorker,
    ConnectionInfo,
    crate::connected::RecoveryInfo,
) {
    std::fs::write(fixture.source.join("document"), b"original").unwrap();
    let mut cut = NativeCutWorker::open(fixture);
    let info = cut.command(
        cut.service
            .create(SelectedFolders::probe(&fixture.source, &fixture.destination).unwrap())
            .unwrap(),
    );
    cut.command(cut.service.start(info.id).unwrap());
    let Phase::AwaitingReview { review, .. } = cut.settle().phase else {
        panic!("real initial review");
    };
    cut.command(cut.service.accept_initial(review).unwrap());
    assert_eq!(cut.settle().phase, Phase::UpToDate);
    std::fs::write(fixture.source.join("document"), b"source edit").unwrap();
    std::fs::write(fixture.destination.join("document"), b"destination edit").unwrap();
    cut.command(cut.service.start(info.id).unwrap());
    assert_eq!(cut.settle().phase, Phase::Conflict);
    let original = cut
        .command(cut.service.conflicts(info.id, None).unwrap())
        .entries
        .into_iter()
        .find(|entry| !entry.resolved)
        .unwrap();
    let preview = cut.command(
        cut.service
            .preview_conflict(
                info.id,
                original.serial,
                original.revision,
                source_choice(&original),
                Side::Destination,
            )
            .unwrap(),
    );
    cut.command(cut.service.restore(preview.review).unwrap());
    assert_eq!(cut.settle().phase, Phase::Conflict);
    let collision = cut
        .command(cut.service.conflicts(info.id, None).unwrap())
        .entries
        .into_iter()
        .max_by_key(|entry| entry.serial)
        .unwrap();
    let preview = cut.command(
        cut.service
            .preview_conflict(
                info.id,
                collision.serial,
                collision.revision,
                source_choice(&original),
                Side::Destination,
            )
            .unwrap(),
    );
    cut.command(cut.service.replace(preview.review).unwrap());
    assert!(matches!(cut.settle().phase, Phase::Pending(_)));
    let prior_displaced = cut
        .command(cut.service.recovery(info.id, None, 64).unwrap())
        .entries
        .into_iter()
        .filter(|entry| entry.reason == crate::connected::RecoveryReason::DisplacedVersion)
        .max_by_key(|entry| entry.id.operation)
        .unwrap();
    assert_eq!(
        preserved_bytes(fixture, &prior_displaced),
        b"destination edit"
    );
    // Reopen at the one-consent cut cannot grant the missing original consent.
    if checkpoint {
        cut.worker.store.checkpoint().unwrap();
    }
    drop(cut);
    let mut cut = NativeCutWorker::open(fixture);
    cut.command(cut.service.start(info.id).unwrap());
    assert!(matches!(cut.settle().phase, Phase::Pending(_)));
    let open = cut.command(cut.service.conflicts(info.id, None).unwrap());
    assert_eq!(
        open.entries.iter().filter(|entry| !entry.resolved).count(),
        2
    );
    let original = open
        .entries
        .into_iter()
        .find(|entry| entry.serial == original.serial)
        .unwrap();
    let preview = cut.command(
        cut.service
            .preview_conflict(
                info.id,
                original.serial,
                original.revision,
                source_choice(&original),
                Side::Destination,
            )
            .unwrap(),
    );
    cut.command(cut.service.replace(preview.review).unwrap());
    // The genuine selected operation is durable. Do not dispatch its follow-up
    // comparison yet; this is a legal boundary in the production run loop.
    assert_eq!(
        cut.worker
            .active
            .as_ref()
            .unwrap()
            .observer
            .snapshot()
            .phase,
        Phase::Syncing
    );
    assert!(
        !cut.worker
            .active
            .as_ref()
            .unwrap()
            .observer
            .snapshot()
            .pending_operation
    );
    let conflicts = cut.command(cut.service.conflicts(info.id, None).unwrap());
    assert_eq!(
        conflicts
            .entries
            .iter()
            .filter(|entry| !entry.resolved)
            .count(),
        2
    );
    if checkpoint {
        cut.worker.store.checkpoint().unwrap();
    }
    (cut, info, prior_displaced)
}

#[cfg(target_os = "linux")]
#[test]
fn native_recovery_attention_cut_reopens_log_and_checkpoint_before_live_equality() {
    for checkpoint in [false, true] {
        let fixture = NativeFixture::new();
        let (mut cut, info, prior) = native_attention_cut(&fixture, checkpoint);
        let before = cut
            .command(cut.service.conflicts(info.id, None).unwrap())
            .entries;
        let inventory = cut
            .command(cut.service.recovery(info.id, None, 64).unwrap())
            .entries;
        let retained_bytes: Vec<_> = inventory
            .iter()
            .map(|entry| {
                let bytes = preserved_bytes(&fixture, entry);
                assert!(bytes == b"source edit" || bytes == b"destination edit");
                (entry.id, bytes)
            })
            .collect();
        drop(cut);
        let mut cut = NativeCutWorker::open(&fixture);
        let after = cut
            .command(cut.service.conflicts(info.id, None).unwrap())
            .entries;
        assert_eq!(
            after, before,
            "reopen reconstructs consent metadata without closing conflicts"
        );
        let reopened = cut
            .command(cut.service.recovery(info.id, None, 64).unwrap())
            .entries;
        assert_eq!(
            reopened.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            inventory.iter().map(|entry| entry.id).collect::<Vec<_>>()
        );
        assert_eq!(preserved_bytes(&fixture, &prior), b"destination edit");
        cut.command(cut.service.start(info.id).unwrap());
        assert_eq!(
            cut.settle().phase,
            Phase::UpToDate,
            "fresh native equality must close both explicit consents"
        );
        assert!(
            cut.command(cut.service.conflicts(info.id, None).unwrap())
                .entries
                .iter()
                .all(|entry| entry.resolved)
        );
        assert_eq!(
            std::fs::read(fixture.source.join("document")).unwrap(),
            b"source edit"
        );
        assert_eq!(
            std::fs::read(fixture.destination.join("document")).unwrap(),
            b"source edit"
        );
        for (entry, (id, expected)) in inventory.iter().zip(&retained_bytes) {
            assert_eq!(entry.id, *id);
            assert_eq!(preserved_bytes(&fixture, entry), *expected);
        }
        cut.worker.store.checkpoint().unwrap();
        drop(cut);
        let mut cut = NativeCutWorker::open(&fixture);
        assert!(
            cut.command(cut.service.conflicts(info.id, None).unwrap())
                .entries
                .iter()
                .all(|entry| entry.resolved)
        );
        cut.command(cut.service.start(info.id).unwrap());
        assert_eq!(cut.settle().phase, Phase::UpToDate);
        let final_inventory = cut
            .command(cut.service.recovery(info.id, None, 64).unwrap())
            .entries;
        for (id, expected) in &retained_bytes {
            let entry = final_inventory
                .iter()
                .find(|entry| entry.id == *id)
                .expect("retained version survives final checkpoint");
            assert_eq!(preserved_bytes(&fixture, entry), *expected);
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn native_recovery_attention_cut_missing_or_corrupt_prior_bytes_refuses_live_equality() {
    for (checkpoint, missing) in [(false, false), (true, true)] {
        let fixture = NativeFixture::new();
        let (mut cut, info, prior) = native_attention_cut(&fixture, checkpoint);
        let connection: String = info.id.0.iter().map(|byte| format!("{byte:02x}")).collect();
        let private = fixture
            .store
            .join("recovery")
            .join(connection)
            .join(format!("{}-{}", prior.id.operation, prior.id.slot));
        let before = cut
            .command(cut.service.conflicts(info.id, None).unwrap())
            .entries;
        let before_record = cut.worker.store.load_connection(info.id).unwrap();
        let (pending, latest) = cut.worker.store.executor_position(info.id).unwrap();
        assert!(pending.is_none());
        let selected = latest.unwrap();
        assert_eq!(
            cut.worker.store.executor_receipt(selected).unwrap().stage,
            crate::connection::store::ReceiptStage::SelectedApplied
        );
        drop(cut);
        if missing {
            std::fs::remove_file(&private).unwrap();
        } else {
            std::fs::write(&private, b"corrupt displaced").unwrap();
        }
        // Historical replay/open intentionally performs no fresh byte proof.
        let mut cut = NativeCutWorker::open(&fixture);
        assert_eq!(
            cut.command(cut.service.conflicts(info.id, None).unwrap())
                .entries,
            before
        );
        cut.command(cut.service.start(info.id).unwrap());
        let result = cut.settle();
        assert!(
            matches!(result.phase, Phase::RecoveryRequired(_) | Phase::Pending(_)),
            "bad earlier displacement must refuse live baseline: {result:?}"
        );
        if missing {
            // A failed physical open poisons this owner. Its public projection
            // must refuse; only a fresh metadata reopen can inspect the cut.
            let request = cut.service.conflicts(info.id, None).unwrap();
            cut.worker.command(cut.commands.recv().unwrap());
            assert!(matches!(
                request.wait(),
                Err(ConnectionError::RecoveryRequired)
            ));
        } else {
            assert_eq!(
                cut.command(cut.service.conflicts(info.id, None).unwrap())
                    .entries,
                before
            );
        }
        drop(cut);
        let mut cut = NativeCutWorker::open(&fixture);
        let after = cut
            .command(cut.service.conflicts(info.id, None).unwrap())
            .entries;
        assert_eq!(after, before, "refusal cannot resolve or revise a conflict");
        assert!(after.iter().all(|entry| !entry.resolved));
        let record = cut.worker.store.load_connection(info.id).unwrap();
        assert_eq!(record.baseline_revision, before_record.baseline_revision);
        assert!(record.revision > before_record.revision);
        let (pending, latest) = cut.worker.store.executor_position(info.id).unwrap();
        let comparison = pending.expect("failed proof leaves the real comparison pending");
        assert_eq!(latest, Some(comparison));
        assert_eq!(comparison.serial, selected.serial + 1);
        assert_eq!(
            cut.worker.store.executor_receipt(comparison).unwrap().stage,
            crate::connection::store::ReceiptStage::Verified
        );
        assert_eq!(
            cut.worker.store.executor_receipt(selected).unwrap().stage,
            crate::connection::store::ReceiptStage::SelectedApplied
        );
        assert_eq!(
            std::fs::read(fixture.source.join("document")).unwrap(),
            b"source edit"
        );
        assert_eq!(
            std::fs::read(fixture.destination.join("document")).unwrap(),
            b"source edit"
        );
    }
}

// Catches carry validation being omitted from actual event dispatch, or legacy
// completion accidentally retaining prior consent. All history below is emitted
// by the real worker/native engines before the store-side replay assertions.
#[cfg(target_os = "linux")]
#[test]
fn native_recovery_attention_events_validate_refs_and_replay_legacy_supersession() {
    let fixture = NativeFixture::new();
    let (mut cut, info, _) = native_attention_cut(&fixture, false);
    // Pause the separately scheduled comparison through the public command;
    // preview must reopen fresh qualified endpoints before the next choice.
    cut.command(cut.service.pause(info.id).unwrap());
    let original = cut
        .command(cut.service.conflicts(info.id, None).unwrap())
        .entries
        .into_iter()
        .min_by_key(|entry| entry.serial)
        .unwrap();
    let preview = cut.command(
        cut.service
            .preview_conflict(
                info.id,
                original.serial,
                original.revision,
                source_choice(&original),
                Side::Destination,
            )
            .unwrap(),
    );
    cut.command(cut.service.restore(preview.review).unwrap());
    assert_eq!(cut.settle().phase, Phase::Conflict);
    let collision = cut
        .command(cut.service.conflicts(info.id, None).unwrap())
        .entries
        .into_iter()
        .max_by_key(|entry| entry.serial)
        .unwrap();
    assert_ne!(collision.serial, original.serial);
    let preview = cut.command(
        cut.service
            .preview_conflict(
                info.id,
                collision.serial,
                collision.revision,
                source_choice(&original),
                Side::Destination,
            )
            .unwrap(),
    );
    cut.command(cut.service.replace(preview.review).unwrap());
    let snapshot = cut.worker.active.as_ref().unwrap().observer.snapshot();
    assert_eq!(snapshot.phase, Phase::Syncing);
    assert!(!snapshot.pending_operation);
    assert_eq!(
        std::fs::read(fixture.source.join("document")).unwrap(),
        b"source edit"
    );
    assert_eq!(
        std::fs::read(fixture.destination.join("document")).unwrap(),
        b"source edit"
    );
    cut.worker
        .store
        .assert_selected_attention_event_history(info.id);
}
