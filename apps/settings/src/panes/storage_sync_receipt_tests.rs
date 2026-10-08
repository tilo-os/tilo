use super::*;
use runtime_fs::storage::{STORAGE_FRAMING, StorageRequest, StorageResponse, SyncedFileState};
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

fn snapshot() -> SyncStorageSnapshot {
    let file = |identity: &str, reclaimable_bytes| SyncedFileState {
        workspace: "owned-workspace".into(),
        app: None,
        logical_path: format!("/{identity}"),
        identity: identity.into(),
        inode: 7,
        version: "observed-version".into(),
        logical_bytes: 48,
        hydrated_bytes: 48,
        remote_only: false,
        dirty: reclaimable_bytes == 0,
        pinned: false,
        in_flight: false,
        remotely_durable: reclaimable_bytes > 0,
        reclaimable_bytes,
    };
    SyncStorageSnapshot {
        workspaces: vec![runtime_fs::storage::StorageAggregate {
            owner: "owned-workspace".into(),
            logical_bytes: 96,
            local_bytes: 96,
            reclaimable_bytes: 48,
            files: 2,
            ..runtime_fs::storage::StorageAggregate::default()
        }],
        online: true,
        files: vec![file("safe", 48), file("dirty", 0)],
        ..SyncStorageSnapshot::default()
    }
}

fn observed_service(
    listener: UnixListener,
    rejection: Option<&'static str>,
    gate: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
) -> std::thread::JoinHandle<Vec<StorageRequest>> {
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut observed = Vec::new();
        let requests = if rejection.is_some() { 2 } else { 3 };
        while observed.len() < requests && Instant::now() < deadline {
            let (mut stream, _) = match listener.accept() {
                Ok(client) => client,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(error) => panic!("owned service accept failed: {error}"),
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let request: StorageRequest = STORAGE_FRAMING.read_value(&mut stream).unwrap().unwrap();
            let response = match &request {
                StorageRequest::Snapshot => StorageResponse::Snapshot(snapshot()),
                StorageRequest::Evict { selected } => {
                    assert_eq!(
                        selected,
                        &[EvictionSelection {
                            identity: "safe".into(),
                            inode: 7,
                            version: "observed-version".into()
                        }]
                    );
                    if let Some((entered, release)) = &gate {
                        entered.send(()).unwrap();
                        release.recv_timeout(Duration::from_secs(3)).unwrap();
                    }
                    if let Some(error) = rejection {
                        StorageResponse::Error(error.into())
                    } else {
                        StorageResponse::Evicted(EvictionReport {
                            queued_files: 1,
                            evicted_files: 1,
                            freed_bytes: 48,
                            skipped: Vec::new(),
                        })
                    }
                }
            };
            STORAGE_FRAMING.write_value(&mut stream, &response).unwrap();
            observed.push(request);
        }
        observed
    })
}

fn mounted_app(socket: std::path::PathBuf) -> ui::prelude::TestApp<crate::resource::SettingsShell> {
    let mut app = ui::prelude::TestApp::<crate::resource::SettingsShell>::new().unwrap();
    let area = app.surface_mut::<SettingsApp>().storage_mut();
    let mut model = StorageModel::with_roots(
        area.cx.clone(),
        StorageRoots {
            app_payloads: None,
            room_data: None,
            app_cache: None,
            sync_replica: None,
        },
    );
    model.storage_socket = socket;
    model.refresh_sync();
    area.model.take();
    area.model
        .set(std::rc::Rc::new(std::cell::RefCell::new(model)))
        .ok()
        .unwrap();
    let address = crate::effects::open_pane_address(crate::categories::Category::Storage);
    app.harness()
        .invoke_action(&address, &serde_json::Map::new(), false, None)
        .unwrap();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(3), |h| h
                .inspection_json()
                .unwrap()
                .contains("/safe"))
    );
    assert!(app.harness().targets().contains_key("storage.sync.free"));
    assert!(app.harness().native_app_audit().is_certified());
    app
}

#[test]
fn confirmed_sync_eviction_completes_with_declared_unit_output_from_observed_service() {
    let runtime = tempfile::tempdir().unwrap();
    let socket = runtime.path().join("owned-storage.sock");
    let server = observed_service(UnixListener::bind(&socket).unwrap(), None, None);
    let mut app = mounted_app(socket);
    let effect = crate::effects::SettingsEffect::from(StorageEffect::FreeSyncedFiles);
    assert!(
        app.effect(&effect).is_err(),
        "destructive confirmation remains required"
    );
    let job = app.start_effect(&effect).unwrap();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(3), |h| h
                .job_updates(job)
                .iter()
                .any(ui::tilo_actions::InvocationUpdate::is_terminal))
    );
    let observed = server.join().unwrap();
    assert_eq!(observed.len(), 3);
    assert!(matches!(observed.first(), Some(StorageRequest::Snapshot)));
    assert!(matches!(
        observed.get(1),
        Some(StorageRequest::Evict { .. })
    ));
    assert!(matches!(observed.last(), Some(StorageRequest::Snapshot)));
    let terminal = app.harness().job_updates(job).last().unwrap();
    if let ui::tilo_actions::InvocationUpdate::Succeeded { output, .. } = terminal {
        let spec = StorageEffect::FreeSyncedFiles.bound();
        assert_eq!(spec.contract.result, ui::tilo_actions::ResultSpec::Job);
        spec.contract.result.validate(output).unwrap();
    }
    assert!(
        matches!(terminal, ui::tilo_actions::InvocationUpdate::Succeeded { output, .. } if output.value.is_none() && output.resources.is_empty()),
        "{terminal:?}"
    );
    let model = app.surface_mut::<SettingsApp>().storage().model().borrow();
    assert!(!model.sync_busy);
    assert_eq!(
        model.sync_message.as_deref(),
        Some(
            t_args(
                "settings-storage-freed-queued",
                &[
                    ("size", ui::format::bytes_si(48).into()),
                    ("evicted", ArgValue::Number(1.0)),
                    ("queued", ArgValue::Number(1.0))
                ]
            )
            .as_str()
        )
    );
}

#[test]
fn observed_sync_eviction_refusal_preserves_failure_and_notice() {
    let runtime = tempfile::tempdir().unwrap();
    let socket = runtime.path().join("owned-storage.sock");
    let server = observed_service(
        UnixListener::bind(&socket).unwrap(),
        Some("the observed version changed"),
        None,
    );
    let mut app = mounted_app(socket);
    let job = app
        .start_effect(&crate::effects::SettingsEffect::from(
            StorageEffect::FreeSyncedFiles,
        ))
        .unwrap();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(3), |h| h
                .job_updates(job)
                .iter()
                .any(ui::tilo_actions::InvocationUpdate::is_terminal))
    );
    assert_eq!(server.join().unwrap().len(), 2);
    let terminal = app.harness().job_updates(job).last().unwrap();
    assert!(
        matches!(terminal, ui::tilo_actions::InvocationUpdate::Failed { error, .. } if error.message.contains("the observed version changed")),
        "{terminal:?}"
    );
    let model = app.surface_mut::<SettingsApp>().storage().model().borrow();
    assert!(!model.sync_busy);
    assert!(
        model
            .sync_message
            .as_ref()
            .unwrap()
            .contains("the observed version changed")
    );
}

#[test]
fn concurrent_sync_eviction_refuses_only_new_caller_without_replaying_rpc() {
    let runtime = tempfile::tempdir().unwrap();
    let socket = runtime.path().join("owned-storage.sock");
    let (entered, observed) = std::sync::mpsc::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let server = observed_service(
        UnixListener::bind(&socket).unwrap(),
        None,
        Some((entered, gate)),
    );
    let mut app = mounted_app(socket);
    let effect = crate::effects::SettingsEffect::from(StorageEffect::FreeSyncedFiles);
    let first = app.start_effect(&effect).unwrap();
    observed.recv_timeout(Duration::from_secs(3)).unwrap();
    let refusal = app.effect_confirmed(&effect).unwrap_err();
    assert!(
        refusal
            .to_string()
            .contains(&t("settings-storage-reclaiming"))
    );
    assert!(
        !app.harness()
            .job_updates(first)
            .iter()
            .any(ui::tilo_actions::InvocationUpdate::is_terminal)
    );
    release.send(()).unwrap();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(3), |h| h
                .job_updates(first)
                .iter()
                .any(ui::tilo_actions::InvocationUpdate::is_terminal))
    );
    assert_eq!(
        server.join().unwrap().len(),
        3,
        "exactly one eviction RPC plus two snapshots"
    );
    assert!(
        matches!(app.harness().job_updates(first).last(), Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) if output.value.is_none() && output.resources.is_empty())
    );
}
