//! An app's settings capability over the session's records.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

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
        assert!(
            tokio::time::timeout(Duration::from_millis(600), &mut waiting)
                .await
                .is_err()
        );
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
