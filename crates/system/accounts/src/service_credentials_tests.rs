use super::*;
use std::path::PathBuf;

fn root() -> PathBuf {
    std::env::temp_dir().join(format!("accounts-services-{}", uuid::Uuid::new_v4()))
}

#[test]
fn account_custody_adopts_legacy_entries_and_preserves_settings_interoperability() {
    let root = root();
    let id = AccountId::new("legacy-account");
    let mut settings = SecretVault::open_in(&root);
    let legacy = Secret {
        password: "legacy-password".into(),
        token: Some("legacy-oauth-grant".into()),
    };
    settings.put(&id, &legacy);
    settings.try_save().unwrap();
    let mut custody = AccountCredentials::open_in(&root).unwrap();
    assert_eq!(custody.get(&id), Some(legacy));
    let refreshed = Secret {
        password: String::new(),
        token: Some("refreshed-grant".into()),
    };
    custody.put(&id, &refreshed);
    custody.try_save().unwrap();
    assert_eq!(SecretVault::open_in(&root).get(&id), Some(refreshed));
    let mut settings = SecretVault::open_in(&root);
    settings.remove(&id);
    settings.try_save().unwrap();
    assert!(
        AccountCredentials::open_in(&root)
            .unwrap()
            .get(&id)
            .is_none()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn account_custody_rejects_stale_replacements_and_corrupt_storage() {
    let root = root();
    let id = AccountId::new("account");
    let mut first = AccountCredentials::open_in(&root).unwrap();
    let mut stale = AccountCredentials::open_in(&root).unwrap();
    first.put(&id, &Secret::password("first"));
    first.try_save().unwrap();
    stale.put(&id, &Secret::password("stale"));
    assert!(stale.try_save().is_err());
    assert_eq!(
        AccountCredentials::open_in(&root)
            .unwrap()
            .get(&id)
            .unwrap()
            .password,
        "first"
    );
    std::fs::write(crate::vault_dir_in(&root).join("vault.enc"), b"damaged").unwrap();
    assert!(AccountCredentials::open_in(&root).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

fn authorization(origin: &str) -> ServiceAuthorization {
    ServiceAuthorization::new("model-gateway", "provider-api", origin).unwrap()
}

#[test]
fn exact_origin_service_and_purpose_are_required() {
    let root = root();
    let store = ServiceCredentials::open_in(&root);
    let allowed = authorization("https://api.example/v1");
    let reference = allowed.credential_ref("profile-1").unwrap();
    store
        .store(&reference, &allowed, "private-api-key")
        .unwrap();
    assert!(store.status(&reference, &allowed).unwrap());
    assert_eq!(
        store
            .resolve(&reference, &allowed)
            .unwrap()
            .unwrap()
            .as_str(),
        "private-api-key"
    );
    for denied in [
        authorization("https://api.example.evil/v1"),
        authorization("http://api.example/v1"),
        ServiceAuthorization::new("other-service", "provider-api", "https://api.example").unwrap(),
        ServiceAuthorization::new("model-gateway", "other-purpose", "https://api.example").unwrap(),
    ] {
        assert!(matches!(
            store.status(&reference, &denied),
            Err(BrokerError::Denied)
        ));
        assert!(matches!(
            store.resolve(&reference, &denied),
            Err(BrokerError::Denied)
        ));
        assert!(matches!(
            store.store(&reference, &denied, "replacement"),
            Err(BrokerError::Denied)
        ));
        assert!(matches!(
            store.remove(&reference, &denied),
            Err(BrokerError::Denied)
        ));
    }
    assert!(store.remove(&reference, &allowed).unwrap());
    assert!(!store.status(&reference, &allowed).unwrap());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn storage_failures_are_not_reported_as_success_or_absence() {
    let root = root();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("accounts"), b"not a directory").unwrap();
    let store = ServiceCredentials::open_in(&root);
    let allowed = authorization("https://api.example");
    let reference = allowed.credential_ref("profile-1").unwrap();
    assert!(matches!(
        store.store(&reference, &allowed, "key"),
        Err(BrokerError::Storage(_))
    ));
    assert!(matches!(
        store.status(&reference, &allowed),
        Err(BrokerError::Storage(_))
    ));
    assert!(matches!(
        store.remove(&reference, &allowed),
        Err(BrokerError::Storage(_))
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_provider_migration_is_explicit_origin_safe_and_idempotent() {
    let root = root();
    let legacy = AccountId::new("agent-provider-profile-1");
    let mut vault = SecretVault::open_in(&root);
    vault.put(&legacy, &Secret::password("legacy-key"));
    vault.try_save().unwrap();
    let store = ServiceCredentials::open_in(&root);
    let original = authorization("https://original.example/v1");
    let changed = authorization("https://changed.example/v1");
    assert!(matches!(
        store.migrate_agent_provider("profile-1", &changed, "https://original.example"),
        Err(BrokerError::Denied)
    ));
    assert_eq!(
        SecretVault::open_in(&root).get(&legacy).unwrap().password,
        "legacy-key"
    );
    let reference = store
        .migrate_agent_provider("profile-1", &original, "https://original.example")
        .unwrap();
    assert!(SecretVault::open_in(&root).get(&legacy).is_none());
    assert_eq!(
        store
            .resolve(&reference, &original)
            .unwrap()
            .unwrap()
            .as_str(),
        "legacy-key"
    );
    store.store(&reference, &original, "replacement").unwrap();
    assert_eq!(
        store
            .migrate_agent_provider("profile-1", &original, "https://original.example")
            .unwrap(),
        reference
    );
    assert_eq!(
        store
            .resolve(&reference, &original)
            .unwrap()
            .unwrap()
            .as_str(),
        "replacement"
    );
    assert!(matches!(
        store.resolve(&reference, &changed),
        Err(BrokerError::Denied)
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn conflicting_same_key_edits_fail_without_rebinding() {
    let root = root();
    let mut first = SecretVault::open_in(&root);
    let mut second = SecretVault::open_in(&root);
    let key = AccountId::new("shared");
    first.put(&key, &Secret::password("first"));
    second.put(&key, &Secret::password("second"));
    first.try_save().unwrap();
    assert!(second.try_save().is_err());
    assert_eq!(
        SecretVault::open_in(&root).get(&key).unwrap().password,
        "first"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn concurrent_inventory_writers_preserve_independent_entries() {
    let root = root();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let workers: Vec<_> = (0..4)
        .map(|index| {
            let root = root.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut vault = SecretVault::open_in(&root);
                vault.put(
                    &AccountId::new(format!("service-{index}")),
                    &Secret::password("key"),
                );
                barrier.wait();
                vault.try_save().unwrap();
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let vault = SecretVault::open_in(&root);
    for index in 0..4 {
        assert_eq!(
            vault
                .get(&AccountId::new(format!("service-{index}")))
                .unwrap()
                .password,
            "key"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn stale_inventory_writers_merge_only_their_explicit_changes() {
    let root = root();
    let mut first = SecretVault::open_in(&root);
    let mut second = SecretVault::open_in(&root);
    let a = AccountId::new("mail");
    let b = AccountId::new("provider");
    first.put(&a, &Secret::password("mail-key"));
    second.put(&b, &Secret::password("provider-key"));
    first.try_save().unwrap();
    second.try_save().unwrap();
    let reopened = SecretVault::open_in(&root);
    assert_eq!(reopened.get(&a).unwrap().password, "mail-key");
    assert_eq!(reopened.get(&b).unwrap().password, "provider-key");
    first.remove(&a);
    first.try_save().unwrap();
    second.put(&b, &Secret::password("new-provider-key"));
    second.try_save().unwrap();
    // A stale writer must not resurrect the unrelated deleted account.
    assert!(SecretVault::open_in(&root).get(&a).is_none());
    assert_eq!(
        SecretVault::open_in(&root).get(&b).unwrap().password,
        "new-provider-key"
    );
    std::fs::remove_dir_all(root).unwrap();
}
