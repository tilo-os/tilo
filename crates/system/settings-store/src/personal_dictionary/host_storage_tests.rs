use super::{Host, Service};
use crate::{ConditionalUpdate, DisplayUpdate, SessionPaths, SessionSettings, SettingsSource};
use serde_json::{Map, Value, json};
use std::sync::Arc;

fn fixture() -> (tempfile::TempDir, Arc<SessionSettings>, Host) {
    let directory = crate::test_dir();
    let session = Arc::new(
        SessionSettings::open(SessionPaths::beside(directory.path().join("settings.json")))
            .unwrap(),
    );
    let service = Arc::new(Service::new(directory.path().join("dictionary")).unwrap());
    let host = Host::new(session.host_view().unwrap(), service, Arc::new(|| None));
    (directory, session, host)
}

fn object(value: Value) -> Map<String, Value> {
    serde_json::from_value(value).unwrap()
}

fn update(source: &dyn SettingsSource) -> DisplayUpdate {
    DisplayUpdate::new(
        &source.load(),
        object(json!({"id": 7, "state": "confirmed"})),
        Some(object(json!({"profiles": {"panel": {"outputs": {}}}}))),
    )
}

#[test]
fn dictionary_host_atomic_display_update_persists_both_sections() {
    let (_directory, session, host) = fixture();
    assert!(matches!(
        host.compare_display_update(update(&host)).unwrap(),
        ConditionalUpdate::Stored
    ));
    let reopened = SessionSettings::open(session.paths().clone()).unwrap();
    let durable = reopened.load();
    assert_eq!(durable["display_transaction"]["id"], 7);
    assert_eq!(
        durable["display_profiles"]["profiles"]["panel"]["outputs"],
        json!({})
    );
}

#[test]
fn dictionary_host_atomic_display_update_preserves_new_external_profiles() {
    let (_directory, session, host) = fixture();
    let intent = update(&host);
    crate::update_section_at(
        &session.paths().copy,
        "display_profiles",
        object(json!({"external_revision": 8})),
    )
    .unwrap();
    assert!(matches!(
        host.compare_display_update(intent).unwrap(),
        ConditionalUpdate::Superseded
    ));
    let durable = session.load();
    assert!(!durable.contains_key("display_transaction"));
    assert_eq!(durable["display_profiles"]["external_revision"], 8);
}

#[test]
fn dictionary_host_atomic_display_failure_rolls_back_both_sections() {
    let (_directory, session, host) = fixture();
    let before = session.load();
    let intent = update(&host);
    std::fs::create_dir_all(session.paths().device.parent().unwrap()).unwrap();
    std::fs::create_dir(&session.paths().device).unwrap();
    assert!(host.compare_display_update(intent).is_err());
    assert_eq!(session.load(), before);
    assert_eq!(crate::load_at(&session.paths().copy), before);
}

#[test]
fn dictionary_host_preserves_durable_copy_repair_receipts_for_all_update_paths() {
    for operation in 0..3 {
        let (_directory, session, host) = fixture();
        // Read synchronously: host.load() could queue a refresh that repairs
        // the failed copy publication before the unchanged-copy assertion.
        let intent = update(session.as_ref());
        if operation == 1 {
            host.update_section("display_transaction", object(json!({"id": 0})))
                .unwrap();
            assert_eq!(session.load()["display_transaction"]["id"], 0);
        }
        let copy = std::fs::read(&session.paths().copy).unwrap();
        // Each path publishes one device record before replacing the readable copy.
        let result = tilo_files::native::atomic::test_support::with_failure_on_nth(
            tilo_files::native::atomic::test_support::Checkpoint::BeforePublish,
            2,
            || match operation {
                0 => host.compare_display_update(intent),
                1 => host.compare_update_section(
                    "display_transaction",
                    &object(json!({"id": 0})),
                    object(json!({"id": 7})),
                ),
                _ => host.update_section_report("display_transaction", object(json!({"id": 7}))),
            },
        );
        assert!(
            matches!(result.unwrap(), ConditionalUpdate::StoredWithCopyRepair(_)),
            "operation {operation} must retain the storage owner's durable receipt"
        );
        assert_eq!(std::fs::read(&session.paths().copy).unwrap(), copy);
        let durable = crate::load_at(&session.paths().device);
        assert_eq!(durable["display_transaction"]["id"], 7);
        if operation == 0 {
            assert_eq!(
                durable["display_profiles"]["profiles"]["panel"]["outputs"],
                json!({})
            );
        }
        assert_eq!(session.load(), durable);
        assert_eq!(crate::load_at(&session.paths().copy), durable);
    }
}
