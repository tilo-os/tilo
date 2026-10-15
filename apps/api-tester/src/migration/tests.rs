//! Old-format fixture -> production import transaction -> durable reload.
use super::*;
use automerge::{ObjId, ObjType, transaction::Transactable};
use ui::tilo_resource::{Model, Resource};

fn fixture_into(doc: &mut AutoCommit) {
    fn write(doc: &mut AutoCommit, object: &ObjId, fixture: &serde_json::Value) {
        for (key, value) in fixture.as_object().unwrap() {
            match value {
                serde_json::Value::Object(fields) if fields.contains_key("$text") => {
                    let text = doc.put_object(object, key, ObjType::Text).unwrap();
                    doc.splice_text(&text, 0, 0, fields["$text"].as_str().unwrap())
                        .unwrap();
                }
                serde_json::Value::Object(_) => {
                    let child = doc.put_object(object, key, ObjType::Map).unwrap();
                    write(doc, &child, value);
                }
                serde_json::Value::String(value) => {
                    doc.put(object, key, value.as_str()).unwrap();
                }
                serde_json::Value::Bool(value) => {
                    doc.put(object, key, *value).unwrap();
                }
                _ => panic!("unsupported fixture scalar"),
            }
        }
    }
    write(
        doc,
        &ROOT,
        &serde_json::from_str(include_str!("../../tests/fixtures/legacy-workspace.json")).unwrap(),
    );
}

fn fixture() -> AutoCommit {
    let mut doc = AutoCommit::new();
    fixture_into(&mut doc);
    AutoCommit::load(&doc.save()).unwrap()
}

fn assert_imported(doc: &AutoCommit, store: &SecretStore) {
    let state = Workspace::read(doc).unwrap();
    assert_eq!(state.request.name, "Unsaved draft");
    assert_eq!(state.request.url, "https://example.org/draft?keep=yes");
    assert_eq!(state.request.body, "{\"draft\":true}");
    assert_eq!(state.request.method, crate::model::HttpMethod::Post);
    assert_eq!(state.request.body_mode, crate::model::BodyMode::Json);
    assert_eq!(state.opened.0, "draft-id");
    assert_eq!(state.request.auth.username, "ada");
    let headers: Vec<_> = state.request.headers.iter().collect();
    assert_eq!(headers[0].0.as_str(), "header-id");
    assert_eq!(headers[0].1.value, "preserved");
    assert!(!headers[0].1.enabled);
    assert_eq!(state.collections.len(), 1);
    let saved = state.saved(&Id::from_raw("saved-id")).unwrap().1;
    assert_eq!(saved.url, "https://example.org/saved");
    assert_eq!(saved.body, "saved body");
    assert_eq!(saved.method, crate::model::HttpMethod::Put);
    assert_eq!(saved.body_mode, crate::model::BodyMode::Raw);
    assert_eq!(saved.auth.header, "X-Access");
    assert_eq!(saved.params.iter().next().unwrap().0.as_str(), "param-id");
    assert_eq!(
        state.active_environment,
        Some(Id::from_raw("environment-id"))
    );
    assert_eq!(
        state
            .environments
            .iter()
            .next()
            .unwrap()
            .1
            .variables
            .iter()
            .next()
            .unwrap()
            .1
            .value,
        "example.org"
    );
    assert_eq!(state.history.iter().next().unwrap().1.status, 201);
    for (record, expected) in [
        (&state.request, "old-draft-password"),
        (saved, "old-saved-key"),
    ] {
        let auth = Auth::SecretRef {
            id: record.auth.reference.clone(),
            scheme: String::new(),
        };
        assert_eq!(
            crate::credentials::resolve(&auth, store).unwrap().secret(),
            Some(expected)
        );
    }
    // Check current fields, not compressed save bytes (which retain history).
    for object in [
        ROOT,
        doc.get(ROOT, "api_tester_request").unwrap().unwrap().1,
    ] {
        for key in doc.keys(&object) {
            if let Some((value, _)) = doc.get(&object, &key).unwrap() {
                assert!(
                    !value
                        .as_str()
                        .is_some_and(|text| text.contains("old-draft-password")
                            || text.contains("old-saved-key"))
                );
            }
        }
    }
    let json = state.to_json().to_string();
    assert!(!json.contains("old-draft-password"));
    assert!(!json.contains("old-saved-key"));
}

#[test]
fn old_fixture_opens_migrates_and_reloads_without_losing_the_draft() {
    let mut doc = fixture();
    let old = doc.get_heads();
    let store = SecretStore::memory("so.tilo.api-tester");
    assert!(import(&mut doc, &store, &mut Migration::default()).unwrap());
    let mut reloaded = AutoCommit::load(&doc.save()).unwrap();
    assert_imported(&reloaded, &store);
    assert!(!import(&mut reloaded, &store, &mut Migration::default()).unwrap());
    let historical = reloaded.fork_at(&old).unwrap();
    assert_eq!(
        ApiDoc::request(&historical, &ROOT).unwrap().auth.secret(),
        Some("old-draft-password")
    );
}

#[test]
fn typed_entry_point_imports_before_projecting_the_record() {
    let mut app = ui::framework::testing::TestApp::<crate::ApiTesterShell>::new().unwrap();
    app.merge_peer_edit(|doc| {
        // Replace the typed payload by the old persisted fixture, retaining
        // the host's identity metadata exactly as an existing record does.
        for key in [
            "request",
            "collections",
            "environments",
            "active_environment",
            "opened",
            "history",
        ] {
            doc.delete(ROOT, key).unwrap();
        }
        fixture_into(doc);
    });
    let app = app.reopen().unwrap();
    assert_eq!(app.state().request.name, "Unsaved draft");
    assert!(!app.state().request.auth.reference.is_empty());
    assert_eq!(app.state().request.auth.username, "ada");
}

#[test]
fn quota_failure_keeps_original_credentials_and_retry_publishes_only_refs() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use ui::secrets::{Protection, PutOptions, Secret, SecretBackend, SecretError};
    struct Quota {
        full: AtomicBool,
        store: SecretStore,
    }
    impl SecretBackend for Quota {
        fn protection(&self) -> Protection {
            Protection::Memory
        }
        fn get(&self, _: &str, key: &str) -> Result<Option<Secret>, SecretError> {
            self.store.get_blocking(key)
        }
        fn put(&self, _: &str, key: &str, bytes: &[u8], _: &str) -> Result<(), SecretError> {
            if self.full.load(Ordering::SeqCst)
                && bytes.windows(13).any(|part| part == b"old-saved-key")
            {
                return Err(SecretError::Failed("QuotaExceededError".into()));
            }
            self.store.put_blocking(key, bytes, PutOptions::default())
        }
        fn delete(&self, _: &str, key: &str) -> Result<(), SecretError> {
            self.store.delete_blocking(key)
        }
    }
    let backend = Arc::new(Quota {
        full: AtomicBool::new(true),
        store: SecretStore::memory("test"),
    });
    let store = SecretStore::with_backend("so.tilo.api-tester", backend.clone());
    let mut migration = Migration::default();
    let mut doc = fixture();
    let before = doc.save();
    assert!(import(&mut doc, &store, &mut migration).is_err());
    assert_eq!(doc.save(), before);
    assert!(
        doc.get(ROOT, "request").unwrap().is_none(),
        "no partial typed payload may be published"
    );
    assert_eq!(
        ApiDoc::request(&doc, &ROOT).unwrap().auth.secret(),
        Some("old-draft-password")
    );
    backend.full.store(false, Ordering::SeqCst);
    import(&mut doc, &store, &mut migration).unwrap();
    assert_imported(&AutoCommit::load(&doc.save()).unwrap(), &store);
}

#[test]
fn explicit_snapshot_effect_imports_into_a_fresh_record_without_old_history() {
    use base64::Engine;
    let mut source = fixture();
    let bytes = source.save();
    let mut app = ui::framework::testing::TestApp::<crate::ApiTesterShell>::new().unwrap();
    app.effect(&crate::actions::ApiTesterEffect::ImportLegacy {
        snapshot: base64::engine::general_purpose::STANDARD.encode(&bytes),
    })
    .unwrap();
    app.settle();
    assert_eq!(app.state().request.name, "Unsaved draft");
    assert_eq!(app.state().collections.len(), 1);
    assert_eq!(app.state().opened.0, "draft-id");
    assert_eq!(app.state().history.iter().next().unwrap().1.status, 201);
    assert_eq!(app.state().environments.len(), 1);
    assert_eq!(
        app.state().active_environment,
        Some(Id::from_raw("environment-id"))
    );
    assert!(!app.state().request.auth.reference.is_empty());
    assert_eq!(source.save(), bytes, "the import source is never changed");
    let imported = app.read_document(|doc| doc.clone().save());
    let mut reloaded = AutoCommit::load(&imported).unwrap();
    assert!(reloaded.get(ROOT, "api_tester_request").unwrap().is_none());
    // The source's historical plaintext changes must not have been merged.
    let changes = reloaded.get_changes(&[]);
    for change in changes {
        let expanded = serde_json::to_string(&change.decode()).unwrap();
        assert!(!expanded.contains("old-draft-password"));
        assert!(!expanded.contains("old-saved-key"));
    }
    let before = app.state();
    assert!(
        app.effect(&crate::actions::ApiTesterEffect::ImportLegacy {
            snapshot: base64::engine::general_purpose::STANDARD.encode(&bytes),
        })
        .is_err(),
        "never replace an existing draft with an import"
    );
    assert_eq!(app.state(), before);
}

#[test]
fn concurrent_legacy_map_roots_keep_requests_from_both_peers() {
    let mut app = ui::framework::testing::TestApp::<crate::ApiTesterShell>::new().unwrap();
    let mut base = app.peer_document();
    base.delete(ROOT, "request").unwrap();
    let mut left = base.fork();
    let mut right = base.fork();
    fixture_into(&mut left);
    fixture_into(&mut right);
    let saved = right
        .get(ROOT, "api_tester_saved_request_fields")
        .unwrap()
        .unwrap()
        .1;
    let extra = right
        .put_object(&saved, "other-saved", ObjType::Map)
        .unwrap();
    right
        .put(&extra, "collection_id", "\"collection-id\"")
        .unwrap();
    right.put(&extra, "name", "\"Other peer's draft\"").unwrap();
    right
        .put(&extra, "url", "https://example.org/other")
        .unwrap();
    app.merge_peer(&mut left).unwrap();
    app.merge_peer(&mut right).unwrap();
    let mut left = app.read_document(Clone::clone);
    import(
        &mut left,
        &SecretStore::memory("test"),
        &mut Migration::default(),
    )
    .unwrap();
    let imported = Workspace::read(&left).unwrap();
    assert!(imported.saved(&Id::from_raw("saved-id")).is_some());
    assert_eq!(
        imported.saved(&Id::from_raw("other-saved")).unwrap().1.url,
        "https://example.org/other"
    );
}

#[test]
fn unsupported_secret_variable_refuses_import_without_publishing_its_value() {
    let mut doc = fixture();
    let variables = doc
        .get(ROOT, "api_tester_environment_variables")
        .unwrap()
        .unwrap()
        .1;
    let variable = doc.get(&variables, "variable-id").unwrap().unwrap().1;
    doc.put(&variable, "secret", true).unwrap();
    let before = doc.save();
    let error = import(
        &mut doc,
        &SecretStore::memory("test"),
        &mut Migration::default(),
    )
    .unwrap_err();
    assert!(error.contains("secret variable"));
    assert_eq!(doc.save(), before);
    assert!(doc.get(ROOT, "request").unwrap().is_none());
}
