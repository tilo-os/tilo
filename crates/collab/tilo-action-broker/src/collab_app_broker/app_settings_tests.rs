use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use runtime_collab::SettingsView;
use runtime_collab::settings::host::{AppSettingsServer, Revalidate};
use serde_json::{Map, Value, json};
use settings_store::{SettingsSource, Store};

const APP: &str = "so.tilo.monitor";

fn object(value: Value) -> Map<String, Value> {
    serde_json::from_value(value).unwrap()
}

fn session(dir: &std::path::Path) -> Arc<Store> {
    Store::open(dir).unwrap()
}

fn admitted() -> Revalidate {
    Arc::new(|| Ok(()))
}

/// An app's settings calls, answered the way the wire answers them.
struct AppSettingsHandle(AppSettingsServer);

impl From<AppSettingsServer> for AppSettingsHandle {
    fn from(server: AppSettingsServer) -> Self {
        Self(server)
    }
}

#[allow(
    clippy::future_not_send,
    reason = "the host answers each app on its single-threaded broker task"
)]
impl AppSettingsHandle {
    async fn read(&self, after: u64) -> Result<SettingsView, String> {
        self.0.read(after).await.map(settings_view)
    }

    #[allow(clippy::unused_async, reason = "mirrors the app's settings handle")]
    async fn update(
        &self,
        section: &str,
        values: &[u8],
        action: Option<&str>,
    ) -> Result<SettingsView, String> {
        self.0.update(section, values, action).map(settings_view)
    }
}

fn settings_view((revision, document_json): (u64, Vec<u8>)) -> SettingsView {
    SettingsView {
        revision,
        document_json,
    }
}

fn handle(source: Arc<Store>, revalidate: Revalidate) -> AppSettingsHandle {
    AppSettingsServer::new(APP, source, revalidate).into()
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
fn an_apps_write_reaches_the_person_record() {
    let dir = tempfile::tempdir().unwrap();
    let records = session(dir.path());
    let settings = handle(Arc::clone(&records), admitted());
    run(async {
        let written = settings
            .update(
                "app_personal",
                br#"{"so.tilo.monitor": {"confirm_kill": false}}"#,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            view(&written.document_json),
            json!({"app_personal": {APP: {"confirm_kill": false}}})
        );
    });
    assert_eq!(
        Value::Object(records.load()),
        json!({"app_personal": {APP: {"confirm_kill": false}}})
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
                .update(
                    "app_personal",
                    br#"{"so.tilo.notes": {"font": "mono"}}"#,
                    None
                )
                .await
                .is_err()
        );
        assert!(
            settings
                .update("locale", br#"{"so.tilo.monitor": {"region": "US"}}"#, None)
                .await
                .is_err()
        );
        assert!(
            settings
                .update("app_personal", br"[1, 2]", None)
                .await
                .is_err()
        );
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
                Err("admission withdrawn".to_owned())
            }
        }),
    );
    run(async {
        assert!(settings.read(0).await.is_ok());
        admitted.store(false, Ordering::Release);
        assert!(settings.read(0).await.is_err());
        assert!(
            settings
                .update("app_personal", br#"{"so.tilo.monitor": {"x": 1}}"#, None)
                .await
                .is_err()
        );
    });
    assert!(records.load().is_empty());
}

#[test]
fn display_runs_the_hosts_transaction_only_for_an_app_holding_display_authority() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let record = Arc::clone(&calls);
    let delegate: runtime_collab::settings::host::DisplayControl =
        Arc::new(move |command, origin| {
            record.lock().unwrap().push((command.to_vec(), origin));
            Ok(b"{}".to_vec())
        });
    let records: Arc<dyn SettingsSource> = session(dir.path());
    let without = AppSettingsServer::new("so.tilo.notes", records.clone(), admitted());
    assert!(
        without
            .display(
                br#"{"operation":"inspect"}"#,
                runtime_collab::settings::DisplayOrigin::LocalHost
            )
            .is_err()
    );
    assert!(
        AppSettingsServer::new("so.tilo.settings", Arc::clone(&records), admitted())
            .with_display(Some(Arc::clone(&delegate)))
            .display(
                &vec![b' '; 64 * 1024 + 1],
                runtime_collab::settings::DisplayOrigin::LocalHost
            )
            .is_err(),
        "an oversized request never reaches the host"
    );
    let withdrawn = AppSettingsServer::new(
        "so.tilo.settings",
        records.clone(),
        Arc::new(|| Err("revoked".to_owned())),
    )
    .with_display(Some(Arc::clone(&delegate)));
    assert!(
        withdrawn
            .display(b"{}", runtime_collab::settings::DisplayOrigin::LocalHost)
            .is_err()
    );
    assert!(calls.lock().unwrap().is_empty());
    let settings = AppSettingsServer::new("so.tilo.settings", records, admitted())
        .with_display(Some(delegate));
    let published = runtime_collab::settings::DisplayOrigin::Published([7; 16]);
    settings
        .display(br#"{"operation":"identify"}"#, published)
        .unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        [(br#"{"operation":"identify"}"#.to_vec(), published)]
    );
}

#[test]
fn the_settings_apps_write_over_its_channel_lands_in_the_sessions_settings() {
    use settings_store::app_scope::{Reach, SYSTEM_SETTINGS_CAPABILITY};
    let dir = tempfile::tempdir().unwrap();
    let records = session(dir.path());
    let reach = Reach::of("so.tilo.settings", &[SYSTEM_SETTINGS_CAPABILITY]);
    assert_eq!(reach, Reach::System);
    let channel = |app: &str, reach: Reach| -> AppSettingsHandle {
        AppSettingsServer::new(
            app,
            Arc::clone(&records) as Arc<dyn SettingsSource>,
            admitted(),
        )
        .with_reach(reach)
        .into()
    };
    let settings = channel("so.tilo.settings", reach);
    let notes = channel(
        "so.tilo.notes",
        Reach::of("so.tilo.notes", &[SYSTEM_SETTINGS_CAPABILITY]),
    );
    run(async {
        let written = settings
            .update(
                "appearance",
                br#"{"high_contrast": true, "text_scale": 1.3}"#,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            view(&written.document_json)["appearance"]["high_contrast"],
            true
        );
        settings
            .update("input", br#"{"keyboard_layout": "de"}"#, None)
            .await
            .unwrap();
        assert!(
            notes
                .update("appearance", br#"{"high_contrast": false}"#, None)
                .await
                .is_err(),
            "an app without the grant writes only its own entries"
        );
    });
    let document = records.load();
    assert_eq!(document["appearance"]["high_contrast"], true);
    assert_eq!(document["appearance"]["text_scale"], 1.3);
    assert_eq!(document["input"]["keyboard_layout"], "de");
}

// The session's history keeps which action made a change an app wrote over
// its channel, so "who changed this" stays answered; a write no action made
// is kept without one. Who invoked it is not the app's to claim.
#[test]
fn a_write_over_the_channel_keeps_the_action_that_made_it() {
    use settings_store::app_scope::{Reach, SYSTEM_SETTINGS_CAPABILITY};
    let dir = tempfile::tempdir().unwrap();
    let records = session(dir.path());
    let settings: AppSettingsHandle = AppSettingsServer::new(
        "so.tilo.settings",
        Arc::clone(&records) as Arc<dyn SettingsSource>,
        admitted(),
    )
    .with_reach(Reach::of("so.tilo.settings", &[SYSTEM_SETTINGS_CAPABILITY]))
    .into();
    run(async {
        settings
            .update(
                "appearance",
                br#"{"high_contrast": true}"#,
                Some("appearance/high_contrast"),
            )
            .await
            .unwrap();
        settings
            .update("appearance", br#"{"text_scale": 1.3}"#, None)
            .await
            .unwrap();
    });
    let log = records.change_log();
    let entry = |leaf: &str| {
        log.iter()
            .rev()
            .find(|entry| entry["section"] == "appearance" && entry["leaf"] == leaf)
            .cloned()
            .unwrap_or_else(|| panic!("the history has appearance.{leaf}: {log:?}"))
    };
    let contrast = entry("high_contrast");
    assert_eq!(contrast["after"], true);
    assert_eq!(contrast["action"], "appearance/high_contrast");
    assert!(contrast["principal"].is_null(), "{contrast}");
    assert!(entry("text_scale")["action"].is_null());
}
