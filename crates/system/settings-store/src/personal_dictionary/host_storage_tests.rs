use super::{Host, Service};
use crate::{ConditionalUpdate, DisplayUpdate, SettingsSource, Store};
use serde_json::{Map, Value, json};
use std::sync::Arc;

fn fixture() -> (tempfile::TempDir, Arc<Store>, Host) {
    let directory = crate::test_dir();
    let session = Store::open(directory.path()).unwrap();
    let service = Arc::new(Service::new(directory.path().join("dictionary")).unwrap());
    let host = Host::new(session.clone(), service, Arc::new(|| None));
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
    let (directory, session, host) = fixture();
    assert!(matches!(
        host.compare_display_update(update(&host)).unwrap(),
        ConditionalUpdate::Stored
    ));
    session.flush_pending();
    drop((host, session));
    let reopened = Store::open(directory.path()).unwrap();
    let durable = reopened.load();
    assert_eq!(durable["display_transaction"]["id"], 7);
    assert_eq!(
        durable["display_profiles"]["profiles"]["panel"]["outputs"],
        json!({})
    );
}

#[test]
fn dictionary_host_atomic_display_update_preserves_newer_profiles() {
    let (_directory, session, host) = fixture();
    let intent = update(&host);
    session
        .update_section("display_profiles", object(json!({"external_revision": 8})))
        .unwrap();
    assert!(matches!(
        host.compare_display_update(intent).unwrap(),
        ConditionalUpdate::Superseded
    ));
    let durable = session.load();
    assert!(!durable.contains_key("display_transaction"));
    assert_eq!(durable["display_profiles"]["external_revision"], 8);
}
