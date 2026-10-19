//! Authentication is device-local; collaborative requests carry only opaque keys.
//! Legacy documents retain historical plaintext even after migration. Rotate the
//! credentials and reset the shared document (including peers/backups) to remove
//! that history. Updating the current fields is not secure erasure.

use crate::model::Auth;
use ui::secrets::{PutOptions, SecretStore};

const UNAVAILABLE: &str = "Authentication secret unavailable; unlock the host secret store";
/// A staged browser write has not yet received durable acknowledgement.
pub const PENDING: &str = "Saving authentication securely; waiting for browser storage";

/// Seal authentication, returning a reference only after storage acknowledges it.
///
/// # Errors
/// Returns a sanitized failure, or [`PENDING`] while a browser write is staged.
pub fn seal(auth: &Auth, store: &SecretStore) -> Result<Auth, &'static str> {
    let Some(value) = credential_bytes(auth) else {
        return Ok(auth.clone());
    };
    let key = format!("request-auth/{}", uuid::Uuid::new_v4());
    let id = store
        .seal_blocking(&key, &value, PutOptions::default())
        .map_err(|_| UNAVAILABLE)?
        .ok_or(PENDING)?;
    Ok(Auth::SecretRef {
        id,
        scheme: auth.label().into(),
    })
}

/// Seal authentication and wait for the backend's durable acknowledgement.
///
/// # Errors
/// Returns a sanitized failure when protected storage refuses the write.
pub async fn seal_async(auth: &Auth, store: &SecretStore) -> Result<Auth, &'static str> {
    let Some(value) = credential_bytes(auth) else {
        return Ok(auth.clone());
    };
    let id = format!("request-auth/{}", uuid::Uuid::new_v4());
    store
        .put(&id, &value, PutOptions::default())
        .await
        .map_err(|_| UNAVAILABLE)?;
    Ok(Auth::SecretRef {
        id,
        scheme: auth.label().into(),
    })
}

/// Keep the local envelope encoding identical for synchronous and async custody.
fn credential_bytes(auth: &Auth) -> Option<Vec<u8>> {
    let value = match auth {
        Auth::None | Auth::SecretRef { .. } => return None,
        Auth::Bearer { token } => serde_json::json!({"Bearer": {"token": token}}),
        Auth::Basic { username, password } => {
            serde_json::json!({"Basic": {"username": username, "password": password}})
        }
        Auth::ApiKey { header, value } => {
            serde_json::json!({"ApiKey": {"header": header, "value": value}})
        }
    };
    Some(value.to_string().into_bytes())
}

/// Whether storage has unacknowledged work.
pub fn pending() -> bool {
    SecretStore::pending()
}

/// Resolve only an API Tester authentication reference from this device's store.
///
/// # Errors
/// Returns a sanitized failure for malformed, unavailable or corrupt references.
pub fn resolve(auth: &Auth, store: &SecretStore) -> Result<Auth, &'static str> {
    let Auth::SecretRef { id, .. } = auth else {
        return Ok(auth.clone());
    };
    // Do not allow a collaborative reference to address another consumer's keys.
    if id
        .strip_prefix("request-auth/")
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .is_none()
    {
        return Err(UNAVAILABLE);
    }
    let secret = store
        .get_blocking(id)
        .map_err(|_| UNAVAILABLE)?
        .ok_or(UNAVAILABLE)?;
    let auth: Auth = serde_json::from_slice(secret.expose()).map_err(|_| UNAVAILABLE)?;
    if matches!(auth, Auth::SecretRef { .. } | Auth::None) {
        return Err(UNAVAILABLE);
    }
    Ok(auth)
}

/// Retain acknowledged seals across multi-field migration retries.
/// Consuming a browser acknowledgement twice starts a new write, not a replay.
#[derive(Default)]
pub struct Migration {
    sealed: Vec<(Auth, Auth)>,
}

impl Migration {
    pub(super) fn seal(&mut self, auth: &Auth, store: &SecretStore) -> Result<Auth, &'static str> {
        if let Some((_, reference)) = self.sealed.iter().find(|(original, _)| original == auth) {
            return Ok(reference.clone());
        }
        let reference = seal(auth, store)?;
        if reference != *auth {
            self.sealed.push((auth.clone(), reference.clone()));
        }
        Ok(reference)
    }

    pub(super) async fn seal_async(
        &mut self,
        auth: &Auth,
        store: &SecretStore,
    ) -> Result<(), &'static str> {
        if self.sealed.iter().any(|(original, _)| original == auth) {
            return Ok(());
        }
        let reference = seal_async(auth, store).await?;
        if reference != *auth {
            self.sealed.push((auth.clone(), reference));
        }
        Ok(())
    }

    /// Rewrite visible legacy fields; history is not erased. Retain this
    /// migration across pending retries until all edits can commit together.
    ///
    /// # Errors
    /// Returns a sanitized storage or document failure, including [`PENDING`].
    pub fn migrate_document(
        &mut self,
        doc: &mut automerge::AutoCommit,
        root: &automerge::ObjId,
        store: &SecretStore,
    ) -> Result<(), &'static str> {
        migrate_document(doc, root, store, self)?;
        self.sealed.clear();
        Ok(())
    }
}

/// Seal every `auth` object nested anywhere in a legacy JSON value.
fn redact(
    value: &mut serde_json::Value,
    store: &SecretStore,
    migration: &mut Migration,
) -> Result<bool, &'static str> {
    let mut changed = false;
    match value {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                if key == "auth" {
                    if let Ok(auth) = serde_json::from_value::<Auth>(value.clone()) {
                        let sealed = migration.seal(&auth, store)?;
                        if sealed != auth {
                            *value = serde_json::to_value(sealed).map_err(|_| UNAVAILABLE)?;
                            changed = true;
                        }
                    }
                } else {
                    changed |= redact(value, store, migration)?;
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                changed |= redact(value, store, migration)?;
            }
        }
        _ => {}
    }
    Ok(changed)
}

/// Rewrite every visible legacy JSON auth field, including old record blobs.
/// Automerge history is intentionally not described as erased by this operation.
fn migrate_document(
    doc: &mut automerge::AutoCommit,
    root: &automerge::ObjId,
    store: &SecretStore,
    migration: &mut Migration,
) -> Result<(), &'static str> {
    use automerge::{ReadDoc, Value};
    let mut objects = vec![root.clone()];
    let mut updates = Vec::new();
    while let Some(object) = objects.pop() {
        for key in doc.keys(&object) {
            for (value, id) in doc
                .get_all(&object, key.as_str())
                .map_err(|_| UNAVAILABLE)?
            {
                match value {
                    Value::Object(automerge::ObjType::Map) => objects.push(id),
                    Value::Object(_) => {}
                    Value::Scalar(value) => {
                        let Some(text) = value.as_str() else {
                            continue;
                        };
                        let Ok(mut json) = serde_json::from_str::<serde_json::Value>(text) else {
                            continue;
                        };
                        let changed = if key == "auth" {
                            if let Ok(auth) = serde_json::from_value::<Auth>(json.clone()) {
                                let sealed = migration.seal(&auth, store)?;
                                let changed = sealed != auth;
                                json = serde_json::to_value(sealed).map_err(|_| UNAVAILABLE)?;
                                changed
                            } else {
                                false
                            }
                        } else {
                            redact(&mut json, store, migration)?
                        };
                        if changed {
                            updates.push((object.clone(), key.clone(), json.to_string()));
                        }
                    }
                }
            }
        }
    }
    let mut replacements = Vec::new();
    for (object, key, _) in updates {
        // Collapse conflicts without replacing the winning credential with a
        // losing legacy value. The losers remain in history, not current state.
        let Some((Value::Scalar(value), _)) =
            doc.get(&object, key.as_str()).map_err(|_| UNAVAILABLE)?
        else {
            continue;
        };
        let Some(text) = value.as_str() else {
            continue;
        };
        let mut replacement = text.to_owned();
        if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(text) {
            if key == "auth" {
                if let Ok(auth) = serde_json::from_value::<Auth>(json.clone()) {
                    json = serde_json::to_value(migration.seal(&auth, store)?)
                        .map_err(|_| UNAVAILABLE)?;
                }
            } else {
                redact(&mut json, store, migration)?;
            }
            replacement = json.to_string();
        }
        replacements.push((object, key, replacement));
    }
    for (object, key, replacement) in replacements {
        // Removing the key first collapses its conflicting values, even when
        // the winner already reads as the replacement.
        let slot = ui::tilo_resource::Slot::Key(&object, &key);
        slot.remove(doc).map_err(|_| UNAVAILABLE)?;
        slot.put_scalar(doc, automerge::ScalarValue::Str(replacement.into()))
            .map_err(|_| UNAVAILABLE)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrate_document(
        doc: &mut automerge::AutoCommit,
        root: &automerge::ObjId,
        store: &SecretStore,
    ) -> Result<(), &'static str> {
        Migration::default().migrate_document(doc, root, store)
    }

    #[test]
    fn authentication_is_reference_only_and_resolves_from_injected_store() {
        let store = SecretStore::memory("so.tilo.api-tester");
        for auth in [
            Auth::Bearer {
                token: "private-token".into(),
            },
            Auth::Basic {
                username: "private-user".into(),
                password: "private-password".into(),
            },
            Auth::ApiKey {
                header: "X-API-Key".into(),
                value: "private-key".into(),
            },
        ] {
            assert!(serde_json::to_string(&auth).is_err());
            assert!(!format!("{auth:?}").contains("private-"));
            let reference = seal(&auth, &store).unwrap();
            let json = serde_json::to_string(&reference).unwrap();
            assert!(!json.contains("private-"));
            assert_eq!(
                resolve(&serde_json::from_str(&json).unwrap(), &store).unwrap(),
                auth
            );
            assert!(resolve(&reference, &SecretStore::memory("other-host")).is_err());
        }
    }

    #[test]
    fn migration_clears_current_fields_preserves_capability_and_is_idempotent() {
        use automerge::{ROOT, ReadDoc};
        let mut doc = automerge::AutoCommit::new();
        let legacy = r#"{"Bearer":{"token":"historical-plaintext"}}"#;
        ui::tilo_resource::evolution::fixture(
            &mut doc,
            &ROOT,
            &serde_json::json!({
                "auth": legacy,
                "legacy-record": format!(r#"{{"request":{{"auth":{legacy}}}}}"#),
            }),
        )
        .unwrap();
        let legacy_heads = doc.get_heads();
        let store = SecretStore::memory("test");
        migrate_document(&mut doc, &ROOT, &store).unwrap();
        let (value, _) = doc.get(ROOT, "auth").unwrap().unwrap();
        let auth: Auth = serde_json::from_str(value.as_str().unwrap()).unwrap();
        assert_eq!(
            resolve(&auth, &store).unwrap(),
            Auth::Bearer {
                token: "historical-plaintext".into()
            }
        );
        assert!(
            !doc.get(ROOT, "legacy-record")
                .unwrap()
                .unwrap()
                .0
                .as_str()
                .unwrap()
                .contains("historical-plaintext")
        );
        let heads = doc.get_heads();
        migrate_document(&mut doc, &ROOT, &store).unwrap();
        assert_eq!(doc.get_heads(), heads);
        // Field replacement cannot promise erasure from the change history.
        let historical = doc.fork_at(&legacy_heads).unwrap();
        assert_eq!(
            historical.get(ROOT, "auth").unwrap().unwrap().0.as_str(),
            Some(legacy)
        );
    }

    #[test]
    fn failed_protected_write_preserves_legacy_document_for_retry() {
        use automerge::ROOT;
        use ui::secrets::{Protection, Secret, SecretBackend, SecretError};
        struct Locked;
        impl SecretBackend for Locked {
            fn protection(&self) -> Protection {
                Protection::Memory
            }
            fn get(&self, _: &str, _: &str) -> Result<Option<Secret>, SecretError> {
                Err(SecretError::Denied)
            }
            fn put(&self, _: &str, _: &str, _: &[u8], _: &str) -> Result<(), SecretError> {
                Err(SecretError::Denied)
            }
            fn delete(&self, _: &str, _: &str) -> Result<(), SecretError> {
                Err(SecretError::Denied)
            }
        }
        let store = SecretStore::with_backend("test", std::sync::Arc::new(Locked));
        let mut doc = automerge::AutoCommit::new();
        ui::tilo_resource::evolution::fixture(
            &mut doc,
            &ROOT,
            &serde_json::json!({"auth": r#"{"Bearer":{"token":"retry-secret"}}"#}),
        )
        .unwrap();
        let heads = doc.get_heads();
        let error = migrate_document(&mut doc, &ROOT, &store).unwrap_err();
        assert!(!error.contains("retry-secret"));
        assert_eq!(doc.get_heads(), heads);
        migrate_document(&mut doc, &ROOT, &SecretStore::memory("test")).unwrap();
    }

    #[test]
    fn multi_field_migration_retries_do_not_reseal_acknowledged_credentials() {
        use automerge::{ROOT, ReadDoc};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };
        use ui::secrets::{Protection, Secret, SecretBackend, SecretError};

        struct Interrupted {
            store: SecretStore,
            refuse: AtomicBool,
            first_writes: AtomicUsize,
        }
        impl SecretBackend for Interrupted {
            fn protection(&self) -> Protection {
                Protection::Memory
            }
            fn get(&self, _: &str, key: &str) -> Result<Option<Secret>, SecretError> {
                self.store.get_blocking(key)
            }
            fn put(&self, _: &str, key: &str, bytes: &[u8], _: &str) -> Result<(), SecretError> {
                if bytes.windows(5).any(|part| part == b"first") {
                    self.first_writes.fetch_add(1, Ordering::SeqCst);
                }
                if bytes.windows(6).any(|part| part == b"second")
                    && self.refuse.load(Ordering::SeqCst)
                {
                    return Err(SecretError::Denied);
                }
                self.store.put_blocking(key, bytes, PutOptions::default())
            }
            fn delete(&self, _: &str, key: &str) -> Result<(), SecretError> {
                self.store.delete_blocking(key)
            }
        }
        let backend = Arc::new(Interrupted {
            store: SecretStore::memory("migration"),
            refuse: AtomicBool::new(true),
            first_writes: AtomicUsize::new(0),
        });
        let store = SecretStore::with_backend("migration", backend.clone());
        let mut doc = automerge::AutoCommit::new();
        for (key, token) in [("a", "first"), ("b", "second")] {
            ui::tilo_resource::evolution::fixture(
                &mut doc,
                &ROOT,
                &serde_json::json!({
                    key: format!(r#"{{"auth":{{"Bearer":{{"token":"{token}"}}}}}}"#),
                }),
            )
            .unwrap();
        }
        let heads = doc.get_heads();
        let mut migration = Migration::default();
        assert!(migration.migrate_document(&mut doc, &ROOT, &store).is_err());
        assert_eq!(
            doc.get_heads(),
            heads,
            "partial custody cannot publish document edits"
        );
        backend.refuse.store(false, Ordering::SeqCst);
        migration.migrate_document(&mut doc, &ROOT, &store).unwrap();
        assert_eq!(
            backend.first_writes.load(Ordering::SeqCst),
            1,
            "both traversal passes and later retries must reuse the acknowledged seal"
        );
        for (key, token) in [("a", "first"), ("b", "second")] {
            let value = doc.get(ROOT, key).unwrap().unwrap().0;
            let json: serde_json::Value = serde_json::from_str(value.as_str().unwrap()).unwrap();
            let reference: Auth = serde_json::from_value(json["auth"].clone()).unwrap();
            assert!(matches!(reference, Auth::SecretRef { .. }));
            assert_eq!(
                resolve(&reference, &store).unwrap(),
                Auth::Bearer {
                    token: token.into()
                }
            );
        }
        assert!(
            migration.sealed.is_empty(),
            "successful migrations release local drafts"
        );
    }

    #[test]
    fn legacy_auth_can_be_migrated_without_serializing_plaintext_again() {
        let legacy: Auth =
            serde_json::from_str(r#"{"Bearer":{"token":"legacy-plaintext"}}"#).unwrap();
        let store = SecretStore::memory("test");
        let reference = seal(&legacy, &store).unwrap();
        assert_eq!(resolve(&reference, &store).unwrap(), legacy);
        assert!(
            !serde_json::to_string(&reference)
                .unwrap()
                .contains("legacy-plaintext")
        );
    }
}
