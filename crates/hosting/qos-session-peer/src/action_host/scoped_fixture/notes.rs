//! Actual mounted Notes, original certified RPC caller and scoped chooser.
use super::ScopedAppFixture;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tilo_actions::InvocationUpdate;

fn document_bytes(fixture: &mut ScopedAppFixture) -> Vec<u8> {
    let id = fixture.record_id();
    fixture
        .records()
        .read(&id, |doc| doc.clone().save())
        .unwrap()
}

async fn terminal(fixture: &mut ScopedAppFixture, job: u64) -> InvocationUpdate {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(update) = fixture.invocation_status(job)
            && update.is_terminal()
        {
            return update;
        }
        fixture.pump().await.unwrap();
        assert!(
            tokio::time::Instant::now() < deadline,
            "Notes never completed its original job"
        );
    }
}

async fn closed_final_publication(fixture: &mut ScopedAppFixture) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Err(error) = fixture.pump().await {
            assert_eq!(
                error,
                "startup replay cannot acknowledge invocation contributors"
            );
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "scoped mutation did not reach the actual closed native publication boundary"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn notes_scoped_import_live_caller_mutates_actual_record() {
    let mut fixture = ScopedAppFixture::new("dev.qos.notes");
    fixture.wait_action("import-markdown").await;
    let path = fixture.root.path().join("scoped-import.md");
    std::fs::write(&path, "# Imported with scope\n\nOriginal caller body.\n").unwrap();
    let (_picker, mut held) = held_picker(path);
    let before = document_bytes(&mut fixture);
    let job = fixture
        .invoke("import-markdown", serde_json::Map::new())
        .await;
    held.accepted(&mut fixture, job).await;
    held.release();
    // This actual owner refusal is part of the evidence: local continuation
    // succeeds, while final native publication remains independently closed.
    closed_final_publication(&mut fixture).await;
    let id = fixture.record_id();
    let page = fixture
        .records()
        .read(&id, ::notes::doc::NotesDoc::list_pages)
        .unwrap()
        .into_iter()
        .find(|page| page.title == "scoped-import")
        .expect("real scoped import must mutate its actual canonical record");
    let blocks = fixture
        .records()
        .read(&id, |doc| {
            ::notes::doc::NotesDoc::page_block_snapshots(doc, page.id)
        })
        .unwrap();
    assert!(
        blocks
            .iter()
            .any(|(_, text)| text == "Original caller body.")
    );
    assert_ne!(document_bytes(&mut fixture), before);
}

#[tokio::test(flavor = "current_thread")]
async fn notes_scoped_import_expired_after_chooser_admission_cannot_mutate() {
    let mut fixture = ScopedAppFixture::new("dev.qos.notes");
    fixture.wait_action("import-markdown").await;
    let path = fixture.root.path().join("expired-import.md");
    std::fs::write(&path, "# Expired import\n\nMust never enter the record.\n").unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let _picker = ui::portal::scoped_picker_fixture(Arc::new(move |_| {
        if entered_tx.send(()).is_err()
            || release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .is_err()
        {
            return ui::portal::PortalOutcome::Cancelled;
        }
        ui::portal::PortalOutcome::Opened(path.clone())
    }))
    .unwrap();
    let before = document_bytes(&mut fixture);
    let job = fixture
        .invoke("import-markdown", serde_json::Map::new())
        .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while entered_rx.try_recv().is_err() {
        if let Some(update) = fixture.invocation_status(job) {
            assert!(
                !update.is_terminal(),
                "chooser admission failed: {update:?}"
            );
        }
        fixture.pump().await.unwrap();
        assert!(
            tokio::time::Instant::now() < deadline,
            "scoped chooser never admitted"
        );
    }
    assert!(matches!(
        fixture.invocation_status(job),
        Some(InvocationUpdate::Accepted { .. })
    ));
    fixture.expire_original_caller();
    release_tx.send(()).unwrap();
    let outcome = terminal(&mut fixture, job).await;
    assert!(
        matches!(outcome, InvocationUpdate::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(document_bytes(&mut fixture), before);
    for _ in 0..5 {
        fixture.pump().await.unwrap();
    }
    assert_eq!(
        document_bytes(&mut fixture),
        before,
        "late worker completion must stay retired"
    );
    assert!(matches!(
        fixture.invocation_status(job),
        Some(InvocationUpdate::Failed { .. })
    ));
}

const SEEDED_PAGE: uuid::Uuid = uuid::Uuid::from_u128(0x92);
const LINK_URL: &str = "https://example.com/scoped-notes";
const EXPORT_SENTINEL: &[u8] = b"existing selected destination";

fn seeded_notes() -> ScopedAppFixture {
    ScopedAppFixture::with_record_seed("dev.qos.notes", |doc| {
        ::notes::notebook::validate(doc).unwrap();
        let meta = ::notes::PageMeta {
            id: SEEDED_PAGE,
            title: "Scoped fixture".into(),
            parent: None,
            order: 0,
        };
        let (ops, marks) = ::notes::MainPanel::seed_block_ops_with_marks(
            "Original committed body.\n\n[Scoped link](https://example.com/scoped-notes)\n",
            |_| None,
        );
        let action = ::notes::actions::NotesAction::ImportPage {
            page: ::notes::actions::PageData::from_meta(&meta),
            ops,
            marks,
            assets: vec![],
        };
        tilo_actions::apply_local(doc, &action, ::notes::actions::encode(&action)).unwrap();
        ::notes::notebook::validate(doc).unwrap();
    })
}

/// Dropping the test's sole sender also releases a waiting portal worker.
struct HeldEffect {
    entered: mpsc::Receiver<()>,
    release: Option<mpsc::Sender<()>>,
}
impl HeldEffect {
    fn release(&mut self) {
        if let Some(sender) = self.release.take() {
            let _ = sender.send(());
        }
    }

    async fn accepted(&self, fixture: &mut ScopedAppFixture, job: u64) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut entered = false;
        loop {
            entered |= self.entered.try_recv().is_ok();
            let status = fixture.invocation_status(job);
            if entered && matches!(status, Some(InvocationUpdate::Accepted { .. })) {
                return;
            }
            assert!(
                status.as_ref().is_none_or(|update| !update.is_terminal()),
                "Notes effect was not accepted: {status:?}"
            );
            fixture.pump().await.unwrap();
            assert!(
                tokio::time::Instant::now() < deadline,
                "actual Notes effect worker never entered"
            );
        }
    }
}

fn held_picker(path: std::path::PathBuf) -> (ui::portal::ScopedPickerFixture, HeldEffect) {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let guard = ui::portal::scoped_picker_fixture(Arc::new(move |_| {
        let _ = entered_tx.send(());
        if release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(10))
            .is_err()
        {
            return ui::portal::PortalOutcome::Cancelled;
        }
        ui::portal::PortalOutcome::Opened(path.clone())
    }))
    .unwrap();
    (
        guard,
        HeldEffect {
            entered: entered_rx,
            release: Some(release_tx),
        },
    )
}

async fn export_original_caller(expire: bool) {
    let mut fixture = seeded_notes();
    fixture.wait_action("export-markdown").await;
    let path = fixture.root.path().join("selected.md");
    std::fs::write(&path, EXPORT_SENTINEL).unwrap();
    let (_picker, mut held) = held_picker(path.clone());
    let before = document_bytes(&mut fixture);
    let job = fixture
        .invoke_confirmed("export-markdown", serde_json::Map::new())
        .await;
    held.accepted(&mut fixture, job).await;
    assert_eq!(std::fs::read(&path).unwrap(), EXPORT_SENTINEL);
    if expire {
        fixture.expire_original_caller();
    }
    held.release();
    let outcome = terminal(&mut fixture, job).await;
    if expire {
        assert!(
            matches!(outcome, InvocationUpdate::Failed { .. }),
            "{outcome:?}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), EXPORT_SENTINEL);
    } else {
        assert!(
            matches!(outcome, InvocationUpdate::Succeeded { .. }),
            "{outcome:?}"
        );
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("Original committed body."), "{written}");
        assert!(written.contains(LINK_URL), "{written}");
    }
    for _ in 0..5 {
        fixture.pump().await.unwrap();
    }
    if expire {
        assert_eq!(std::fs::read(&path).unwrap(), EXPORT_SENTINEL);
        assert!(matches!(
            fixture.invocation_status(job),
            Some(InvocationUpdate::Failed { .. })
        ));
    }
    assert_eq!(
        document_bytes(&mut fixture),
        before,
        "export cannot mutate the notebook"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn notes_scoped_export_live_caller_writes_and_reports_actual_completion() {
    export_original_caller(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn notes_scoped_export_expired_accepted_caller_preserves_selected_file() {
    export_original_caller(true).await;
}

async fn image_original_caller(expire: bool) {
    let mut fixture = seeded_notes();
    fixture.wait_action("insert-image").await;
    let path = fixture.root.path().join("selected.png");
    let image = include_bytes!("../../../../../../apps/settings/shots/settings.png");
    std::fs::write(&path, image).unwrap();
    let (_picker, mut held) = held_picker(path);
    let before = document_bytes(&mut fixture);
    let id = fixture.record_id();
    let job = fixture.invoke("insert-image", serde_json::Map::new()).await;
    held.accepted(&mut fixture, job).await;
    assert_eq!(document_bytes(&mut fixture), before);
    if expire {
        fixture.expire_original_caller();
    }
    held.release();
    if expire {
        let outcome = terminal(&mut fixture, job).await;
        assert!(
            matches!(outcome, InvocationUpdate::Failed { .. }),
            "{outcome:?}"
        );
        for _ in 0..5 {
            fixture.pump().await.unwrap();
        }
        assert_eq!(document_bytes(&mut fixture), before);
        assert!(
            fixture
                .records()
                .read(&id, ::notes::notebook::asset_snapshot)
                .unwrap()
                .unwrap()
                .is_empty()
        );
    } else {
        // A real canonical continuation is the positive control. The native
        // final publication evaluator remains separately closed.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let assets = fixture
                .records()
                .read(&id, ::notes::notebook::asset_snapshot)
                .unwrap()
                .unwrap();
            if assets.values().any(|bytes| bytes.as_slice() == image) {
                break;
            }
            let status = fixture.invocation_status(job);
            assert!(
                status.as_ref().is_none_or(|update| !update.is_terminal()),
                "image failed before canonical continuation: {status:?}"
            );
            fixture.pump().await.unwrap();
            assert!(
                tokio::time::Instant::now() < deadline,
                "actual scoped image never entered canonical record"
            );
        }
        let blocks = fixture
            .records()
            .read(&id, |doc| {
                ::notes::doc::NotesDoc::page_block_snapshots(doc, SEEDED_PAGE)
            })
            .unwrap();
        let expected = format!("![selected](asset:{})", blake3::hash(image).to_hex());
        assert!(
            blocks.iter().any(|(_, text)| text == &expected),
            "{blocks:?}"
        );
        assert_ne!(document_bytes(&mut fixture), before);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn notes_scoped_image_live_caller_adopts_asset_into_actual_record() {
    image_original_caller(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn notes_scoped_image_expired_accepted_caller_cannot_adopt_or_mutate() {
    image_original_caller(true).await;
}

async fn uri_original_caller(expire_pending: bool) {
    let mut fixture = seeded_notes();
    let (peer_id, _) = fixture.wait_action("export-markdown").await;
    assert!(
        !fixture
            .host
            .broker
            .published_action_surfaces()
            .iter()
            .filter(|surface| surface.live
                && surface.app_id == fixture.app
                && surface.route.record_id == fixture.record)
            .flat_map(|surface| surface.actions.iter())
            .any(|entry| entry.action.starts_with("open-external-")),
        "this regression must exercise a lazy action absent from the eager catalogue"
    );
    let search = fixture
        .query_directory(runtime_collab::ActionDirectoryQuery {
            peer_id,
            revision: String::new(),
            cursor: vec![],
            needle: "Scoped link".into(),
            parent_resource: String::new(),
            max_results: 8,
            max_bytes: 65536,
            max_work: 64,
            max_depth: 1,
        })
        .await
        .unwrap();
    assert!(!search.stale_cursor);
    let block = search
        .rows
        .iter()
        .find(|row| row.resource_kind == "notes.block")
        .expect("real bounded directory must discover the canonical marked block");
    let search_revision = search.revision.clone();
    let page = fixture
        .query_directory(runtime_collab::ActionDirectoryQuery {
            peer_id,
            revision: search.revision,
            cursor: vec![],
            needle: String::new(),
            parent_resource: block.resource.clone(),
            max_results: 8,
            max_bytes: 65536,
            max_work: 64,
            max_depth: 1,
        })
        .await
        .unwrap();
    assert!(
        !page.stale_cursor,
        "actual directory changed: offered={search_revision:?}, current={:?}, budget_exhausted={}, rows={}",
        page.revision,
        page.budget_exhausted,
        page.rows.len(),
    );
    let row = page
        .rows
        .iter()
        .find(|row| {
            row.action
                .as_ref()
                .is_some_and(|action| action.starts_with("open-external-"))
        })
        .expect("exact lazy block directory must offer its canonical safe URI operation");
    let resource = row.resource.clone();
    let action = row.action.clone().unwrap();
    let revision = page.revision;
    // The genuine remote revision must survive unchanged production model
    // passes before resolve; no eager publication or fixture-local dispatch.
    for _ in 0..3 {
        fixture.pump().await.unwrap();
    }
    let resolved = fixture
        .resolve_directory(peer_id, &revision, &resource, &action)
        .await
        .unwrap();
    let runtime_collab::ActionDirectoryResolution::Found(resolved) = resolved else {
        panic!("actual current lazy action must resolve: {resolved:?}");
    };
    assert_eq!(resolved.resource, resource);
    assert_eq!(resolved.action.as_deref(), Some(action.as_str()));
    let descriptor = resolved
        .descriptor
        .as_ref()
        .expect("the exact resolved URI action must include its current contract");
    let contract: tilo_actions::ActionContract =
        serde_json::from_str(&descriptor.contract_json).unwrap();
    assert_eq!(
        contract.execution,
        tilo_actions::ExecutionMode::Background { cancellable: false }
    );
    assert_eq!(contract.result, tilo_actions::ResultSpec::Job);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let _uri = ui::portal::scoped_uri_fixture(Arc::new(move |url| {
        assert_eq!(
            url, LINK_URL,
            "Notes must dispatch its exact canonical safe URL"
        );
        let _ = entered_tx.send(());
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(10))
            .map_err(|e| e.to_string())?;
        Ok(())
    }))
    .unwrap();
    let mut held = HeldEffect {
        entered: entered_rx,
        release: Some(release_tx),
    };
    let before = document_bytes(&mut fixture);
    let job = fixture
        .invoke_resolved(
            peer_id,
            &revision,
            &resource,
            &action,
            serde_json::Map::new(),
        )
        .await
        .unwrap();
    held.accepted(&mut fixture, job).await;
    // The backend has already passed the actual worker effect guard. Expiring
    // now tests truthful completion, not prevention of an admitted browser effect.
    if expire_pending {
        fixture.expire_original_caller();
    }
    held.release();
    let outcome = terminal(&mut fixture, job).await;
    if expire_pending {
        assert!(
            matches!(outcome, InvocationUpdate::Failed { .. }),
            "{outcome:?}"
        );
    } else {
        assert!(
            matches!(outcome, InvocationUpdate::Succeeded { .. }),
            "{outcome:?}"
        );
    }
    assert_eq!(document_bytes(&mut fixture), before);
}

#[tokio::test(flavor = "current_thread")]
async fn notes_scoped_uri_live_caller_waits_for_actual_backend_completion() {
    uri_original_caller(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn notes_scoped_uri_expired_pending_backend_cannot_report_success() {
    uri_original_caller(true).await;
}
