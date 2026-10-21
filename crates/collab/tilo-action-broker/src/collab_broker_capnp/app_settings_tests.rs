//! An app's settings capability over the session's records.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use runtime_collab::AppSettingsHandle;
use runtime_collab::settings::host::{AppSettingsServer, Revalidate};
use serde_json::{Map, Value, json};
use settings_store::{SessionPaths, SessionSettings, SettingsSource, load_at};

const APP: &str = "so.tilo.monitor";

fn object(value: Value) -> Map<String, Value> {
    serde_json::from_value(value).unwrap()
}

fn session(dir: &std::path::Path) -> Arc<SessionSettings> {
    Arc::new(SessionSettings::open(SessionPaths::beside(dir.join("settings.json"))).unwrap())
}

fn admitted() -> Revalidate {
    Arc::new(|| Ok(()))
}

fn handle(source: Arc<SessionSettings>, revalidate: Revalidate) -> AppSettingsHandle {
    let client: runtime_collab::collab_capnp::app_settings::Client =
        capnp_rpc::new_client(AppSettingsServer::new(APP, source, revalidate));
    client.into()
}

fn run<F: std::future::Future>(future: F) -> F::Output {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&runtime, future)
}

fn view(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

#[test]
fn an_apps_write_reaches_the_person_record_and_the_copy() {
    let dir = tempfile::tempdir().unwrap();
    let records = session(dir.path());
    let paths = records.paths().clone();
    let settings = handle(Arc::clone(&records), admitted());
    run(async {
        let written = settings
            .update(
                "app_personal",
                br#"{"so.tilo.monitor": {"confirm_kill": false}}"#,
            )
            .await
            .unwrap();
        assert_eq!(
            view(&written.document_json),
            json!({"app_personal": {APP: {"confirm_kill": false}}})
        );
    });
    assert_eq!(
        Value::Object(load_at(&paths.person)),
        json!({"app_personal": {APP: {"confirm_kill": false}}})
    );
    assert_eq!(
        load_at(&paths.copy)["app_personal"][APP]["confirm_kill"],
        false
    );
}

#[test]
fn an_app_cannot_write_another_apps_entry_or_a_shared_section() {
    let dir = tempfile::tempdir().unwrap();
    let records = session(dir.path());
    let settings = handle(Arc::clone(&records), admitted());
    run(async {
        assert!(
            settings
                .update("app_personal", br#"{"so.tilo.notes": {"font": "mono"}}"#)
                .await
                .is_err()
        );
        assert!(
            settings
                .update("locale", br#"{"so.tilo.monitor": {"region": "US"}}"#)
                .await
                .is_err()
        );
        assert!(settings.update("app_personal", br"[1, 2]").await.is_err());
    });
    assert!(records.load().is_empty());
}

#[test]
fn a_record_change_reaches_a_waiting_reader_and_others_do_not() {
    let dir = tempfile::tempdir().unwrap();
    let records = session(dir.path());
    records
        .update_section("app_personal", object(json!({"so.tilo.notes": {"a": 1}})))
        .unwrap();
    records
        .update_section("locale", object(json!({"region": "DE"})))
        .unwrap();
    let settings = handle(Arc::clone(&records), admitted());
    run(async {
        let first = settings.read(0).await.unwrap();
        assert_eq!(
            view(&first.document_json),
            json!({"locale": {"region": "DE"}}),
            "another app's entry is not seen"
        );

        let waiting = settings.read(first.revision);
        tokio::pin!(waiting);
        // A change the app cannot see does not wake it.
        records
            .update_section("power", object(json!({"auto_suspend": false})))
            .unwrap();
        // The view's revision stays, and a read waiting for another one
        // answers only when it changes.
        let unchanged = settings.read(0).await.unwrap();
        assert_eq!(unchanged.revision, first.revision);
        assert_eq!(unchanged.document_json, first.document_json);
        // Another device's write of the app's personal entry, as the device
        // fabric applies it.
        records
            .update_section("app_personal", object(json!({APP: {"interval_ms": 3000}})))
            .unwrap();
        let next = tokio::time::timeout(tilo_test_env::hang_bound(), waiting)
            .await
            .expect("the change wakes the reader")
            .unwrap();
        assert_ne!(next.revision, first.revision);
        assert_eq!(
            view(&next.document_json)["app_personal"],
            json!({APP: {"interval_ms": 3000}})
        );
    });
}

#[test]
fn a_withdrawn_admission_refuses_reads_and_writes() {
    let dir = tempfile::tempdir().unwrap();
    let records = session(dir.path());
    let admitted = Arc::new(AtomicBool::new(true));
    let gate = Arc::clone(&admitted);
    let settings = handle(
        Arc::clone(&records),
        Arc::new(move || {
            if gate.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(capnp::Error::failed("admission withdrawn".into()))
            }
        }),
    );
    run(async {
        assert!(settings.read(0).await.is_ok());
        admitted.store(false, Ordering::Release);
        assert!(settings.read(0).await.is_err());
        assert!(
            settings
                .update("app_personal", br#"{"so.tilo.monitor": {"x": 1}}"#)
                .await
                .is_err()
        );
    });
    assert!(records.load().is_empty());
}

#[test]
fn display_rpc_requires_explicit_origin_and_exact_published_uuid_before_host_execution() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let record = calls.clone();
    let delegate = Arc::new(
        move |_: &[u8], _: runtime_collab::settings::DisplayOrigin| {
            record.fetch_add(1, Ordering::SeqCst);
            Ok(b"{}".to_vec())
        },
    );
    run(async {
        let client: runtime_collab::collab_capnp::app_settings::Client = capnp_rpc::new_client(
            AppSettingsServer::new("so.tilo.settings", session(dir.path()), admitted())
                .with_display(Some(delegate)),
        );
        let mut missing = client.display_control_request();
        missing
            .get()
            .set_command_json(br#"{"operation":"identify"}"#);
        assert!(
            missing.send().promise.await.is_err(),
            "omitted origin cannot default to local intent"
        );
        for bytes in [vec![], vec![0; 15], vec![0; 17]] {
            let mut request = client.display_control_request();
            request
                .get()
                .set_origin(runtime_collab::collab_capnp::DisplayRequestOrigin::Published);
            request.get().set_published_invocation(&bytes);
            assert!(request.send().promise.await.is_err());
        }
        let mut invalid_local = client.display_control_request();
        invalid_local
            .get()
            .set_origin(runtime_collab::collab_capnp::DisplayRequestOrigin::LocalHost);
        invalid_local.get().set_published_invocation(&[0; 16]);
        assert!(invalid_local.send().promise.await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let settings: AppSettingsHandle = client.into();
        settings
            .display_control(
                br#"{"operation":"inspect"}"#,
                runtime_collab::settings::DisplayOrigin::LocalHost,
            )
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    });
}

fn revocable_admission(allowed: Arc<AtomicBool>) -> Revalidate {
    Arc::new(move || {
        if allowed.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(capnp::Error::failed("revoked".into()))
        }
    })
}

fn dictionary_handle(
    app: &str,
    source: Arc<dyn SettingsSource>,
    permission: Revalidate,
    owner: Option<[u8; 32]>,
) -> AppSettingsHandle {
    let client: runtime_collab::collab_capnp::app_settings::Client = capnp_rpc::new_client(
        AppSettingsServer::new(app, source, permission).with_dictionary_owner(owner),
    );
    client.into()
}

fn dictionary_snapshot(
    reply: &runtime_collab::SettingsView,
    app: &str,
) -> settings_store::personal_dictionary::Snapshot {
    let delivery: settings_store::personal_dictionary::host::Delivery = serde_json::from_value(
        view(&reply.document_json)["app_device_local"][app]
            [settings_store::personal_dictionary::host::VIEW_KEY]
            .clone(),
    )
    .unwrap();
    delivery.snapshot
}

#[test]
fn personal_dictionary_follows_authenticated_settings_without_room_storage() {
    use settings_store::personal_dictionary::{Edit, Service, Word, host};
    let dir = tempfile::tempdir().unwrap();
    let records = session(dir.path());
    let service = Arc::new(Service::new(dir.path().join("dictionary")).unwrap());
    let source: Arc<dyn SettingsSource> = Arc::new(host::Host::new(
        records.clone(),
        service,
        Arc::new(|| Some([1; 32])),
    ));
    let allowed = Arc::new(AtomicBool::new(true));
    let permission = revocable_admission(Arc::clone(&allowed));
    let settings = dictionary_handle(
        "so.tilo.settings",
        source.clone(),
        permission,
        Some([1; 32]),
    );
    let osk = dictionary_handle("so.tilo.osk", source.clone(), admitted(), Some([1; 32]));
    let dictation = dictionary_handle(
        "so.tilo.dictation",
        source.clone(),
        admitted(),
        Some([1; 32]),
    );
    let visitor = dictionary_handle("so.tilo.settings", source, admitted(), None);
    let command = |revision, edit| {
        serde_json::to_vec(&json!({ "so.tilo.settings": { host::EDIT_KEY: host::Command { owner: [1; 32], revision, edit } } })).unwrap()
    };
    run(async {
        let before = osk.read(0).await.unwrap();
        let add = command(0, Edit::Add(Word::new("Tilo", None).unwrap()));
        assert!(visitor.update("app_device_local", &add).await.is_err());
        settings.update("app_device_local", &add).await.unwrap();
        let after = osk.read(before.revision).await.unwrap();
        assert!(after.revision > before.revision);
        assert_eq!(
            dictionary_snapshot(&after, "so.tilo.osk")
                .hints("en")
                .collect::<Vec<_>>(),
            ["Tilo"]
        );
        let voice = dictation.read(0).await.unwrap();
        assert_eq!(
            dictionary_snapshot(&voice, "so.tilo.dictation"),
            dictionary_snapshot(&after, "so.tilo.osk")
        );
        assert!(
            view(&visitor.read(0).await.unwrap().document_json)
                .get("app_device_local")
                .is_none()
        );
        assert!(settings.update("app_device_local", &add).await.is_err());
        settings
            .update(
                "app_device_local",
                &command(
                    1,
                    Edit::Replace {
                        old: Word::new("Tilo", None).unwrap(),
                        new: Word::new("TiLo", None).unwrap(),
                    },
                ),
            )
            .await
            .unwrap();
        let edited = dictation.read(voice.revision).await.unwrap();
        assert_eq!(
            dictionary_snapshot(&edited, "so.tilo.dictation")
                .hints("nl")
                .collect::<Vec<_>>(),
            ["TiLo"]
        );
        settings
            .update(
                "app_device_local",
                &command(2, Edit::Remove(Word::new("TiLo", None).unwrap())),
            )
            .await
            .unwrap();
        assert!(
            dictionary_snapshot(&osk.read(after.revision).await.unwrap(), "so.tilo.osk")
                .words
                .is_empty()
        );
        allowed.store(false, Ordering::Release);
        assert!(settings.read(0).await.is_err());
        assert!(
            settings
                .update("app_device_local", &command(3, Edit::Reset))
                .await
                .is_err()
        );
    });
    assert!(records.load().is_empty());
    assert!(load_at(&records.paths().copy).is_empty());
}
