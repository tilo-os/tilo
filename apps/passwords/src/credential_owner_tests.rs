use super::*;
use accounts::{
    AccessDecision, AccessRequest, BrokerError, CredentialBroker, CredentialOwner, CredentialScope,
    SecretItem, SecretStore,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use ui::prelude::TestApp;

struct Owned {
    root: tempfile::TempDir,
    broker: Arc<Mutex<CredentialBroker>>,
    owner: CredentialOwner,
    request: AccessRequest,
    scope: CredentialScope,
}
impl Owned {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut store = SecretStore::open_in(root.path());
        let mut item = SecretItem::login(
            "Synthetic private title",
            "synthetic-private-user",
            "synthetic-private-password",
        );
        item.add_origin("https://synthetic-private.test").unwrap();
        store.put(&mut item).unwrap();
        let broker = Arc::new(Mutex::new(CredentialBroker::new(store)));
        let scope = CredentialScope {
            principal: "11".repeat(32),
            device: "test-device".into(),
            service: "signal".into(),
            account: "synthetic-native-account-alpha".into(),
            generation: 1,
        };
        let owner =
            CredentialOwner::from_broker(&scope.principal, &scope.device, broker.clone()).unwrap();
        let request = AccessRequest {
            app_id: "so.tilo.messages".into(),
            item_id: item.id,
            purpose: "Use selected account".into(),
            origin: Some("https://synthetic-private.test".into()),
        };
        Self {
            root,
            broker,
            owner,
            request,
            scope,
        }
    }
    fn grant(&self, scope: &CredentialScope) -> String {
        let mut broker = self.broker.lock().unwrap();
        broker
            .request_scoped(&self.request, scope, |_, _, shown| {
                assert_eq!(shown, scope);
                AccessDecision::AllowForSession
            })
            .unwrap();
        broker
            .current_grants()
            .into_iter()
            .find(|grant| grant.scope.as_ref() == Some(scope))
            .unwrap()
            .id()
            .into()
    }
    fn allowed(&self, scope: &CredentialScope) -> bool {
        self.broker
            .lock()
            .unwrap()
            .request_existing_scoped(&self.request, scope)
            .is_ok()
    }
}

#[test]
fn live_passwords_semantic_revoke_reaches_original_broker_without_cross_account_revocation() {
    let _secrets = ui::secrets::scope_thread(ui::secrets::memory_backend());
    let owned = Owned::new();
    let mut beta = owned.scope.clone();
    beta.account = "synthetic-native-account-beta".into();
    let alpha_id = owned.grant(&owned.scope);
    let beta_id = owned.grant(&beta);
    let mut app = TestApp::<PasswordsShell>::for_person_with_lent(
        &owned.scope.principal,
        owned.owner.clone(),
    )
    .unwrap();
    let published = serde_json::to_string(&app.agent_surface()).unwrap();
    for private in [
        "synthetic-private-password",
        "synthetic-private-user",
        "synthetic-private.test",
        "synthetic-native-account-alpha",
    ] {
        assert!(
            !published.contains(private),
            "private vault metadata never enters published rows"
        );
    }
    assert!(published.contains(&format!("passwords:grant:{alpha_id}")));
    assert!(published.contains(&format!("passwords:grant:{beta_id}")));
    app.effect(&PasswordsEffect::ShowAccess).unwrap();
    assert!(
        app.harness().a11y_nodes().iter().any(|(_, node)| node
            .value()
            .is_some_and(|value| value.contains("Signal")
                && value.contains("synthetic-native-account-alpha"))),
        "private permission UI identifies the exact service and account"
    );
    let revoke = PasswordsEffect::RevokeAccess {
        grant: alpha_id.clone(),
    };
    assert!(
        app.effect(&revoke).is_err(),
        "revocation requires the semantic door's confirmation"
    );
    assert!(owned.allowed(&owned.scope));
    app.effect_confirmed(&revoke).unwrap();
    assert!(!owned.allowed(&owned.scope));
    assert!(owned.allowed(&beta));
    let next = owned.grant(&owned.scope);
    assert_ne!(next, alpha_id);
    app.settle();
    let published = serde_json::to_string(&app.agent_surface()).unwrap();
    assert!(
        published.contains(&format!("passwords:grant:{next}")),
        "the open original window observes current grants"
    );
    assert!(
        app.effect_confirmed(&revoke).is_err(),
        "stale UI permission IDs never revoke their replacement"
    );
    assert!(owned.allowed(&owned.scope));
    app.effect_confirmed(&PasswordsEffect::RevokeAccess { grant: next })
        .unwrap();
    assert!(!owned.allowed(&owned.scope));
    assert!(owned.allowed(&beta));
    drop(app);
    assert!(owned.root.path().exists());
    drop(owned);
}

#[test]
fn foreign_person_or_device_loan_never_opens_vault_or_falls_back_to_ambient_storage() {
    let _secrets = ui::secrets::scope_thread(ui::secrets::memory_backend());
    let root = tempfile::tempdir().unwrap();
    for (person, device) in [
        ("22".repeat(32), "test-device"),
        ("11".repeat(32), "other-device"),
    ] {
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        let path = root.path().to_path_buf();
        let owner = CredentialOwner::deferred(&person, device, move || {
            calls.fetch_add(1, Ordering::SeqCst);
            SecretStore::open_in(&path)
        })
        .unwrap();
        let mut app =
            TestApp::<PasswordsShell>::for_person_with_lent(&"11".repeat(32), owner).unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert!(
            !serde_json::to_string(&app.agent_surface())
                .unwrap()
                .contains("passwords:grant:")
        );
        drop(app);
    }
    assert!(root.path().read_dir().unwrap().next().is_none());
}

#[test]
fn shared_password_edit_and_delete_update_the_original_current_credential_store() {
    let owned = Owned::new();
    owned.grant(&owned.scope);
    let mut model = crate::PasswordsModel::shared(owned.owner.clone());
    let id = owned.request.item_id.clone();
    let mut item = model
        .items()
        .into_iter()
        .find(|item| item.id == id)
        .unwrap();
    item.password = "synthetic-new-private-value".into();
    model.update(&mut item).unwrap();
    assert_eq!(
        &*owned
            .broker
            .lock()
            .unwrap()
            .request_existing_scoped(&owned.request, &owned.scope)
            .unwrap(),
        "synthetic-new-private-value"
    );
    model.remove(&id).unwrap();
    assert_eq!(
        owned
            .broker
            .lock()
            .unwrap()
            .request_existing_scoped(&owned.request, &owned.scope),
        Err(BrokerError::NotFound)
    );
    assert!(
        model.app_grants().is_empty(),
        "deleting an item removes visible current permissions"
    );
    drop(model);
    drop(owned);
}

#[test]
fn protected_semantic_approval_consumes_original_proposal_without_public_metadata_or_implicit_access()
 {
    let _secrets = ui::secrets::scope_thread(ui::secrets::memory_backend());
    let owned = Owned::new();
    let id = owned
        .owner
        .propose_scoped(
            owned.request.clone(),
            owned.scope.clone(),
            Arc::new(|| true),
        )
        .unwrap();
    let mut app = TestApp::<PasswordsShell>::for_person_with_lent(
        &owned.scope.principal,
        owned.owner.clone(),
    )
    .unwrap();
    let public = serde_json::to_string(&app.agent_surface()).unwrap();
    assert!(public.contains(&format!("passwords:request:{id}")));
    for private in [
        "Synthetic private title",
        "synthetic-private-user",
        "synthetic-private-password",
        "synthetic-private.test",
        "synthetic-native-account-alpha",
    ] {
        assert!(
            !public.contains(private),
            "private request metadata escaped the protected window"
        );
    }
    app.effect(&PasswordsEffect::ShowAccess).unwrap();
    let private = app.harness().inspection_json().unwrap();
    for identity in [
        "Signal",
        "synthetic-native-account-alpha",
        "test-device",
        "https://synthetic-private.test",
    ] {
        assert!(
            private.contains(identity),
            "the review omits the exact request identity {identity}"
        );
    }
    let approve = PasswordsEffect::ApproveAccess {
        proposal: id.clone(),
        lifetime: "session".into(),
    };
    assert!(
        app.effect(&approve).is_err(),
        "semantic approval requires sensitive confirmation"
    );
    assert!(!owned.allowed(&owned.scope));
    app.effect_confirmed(&approve).unwrap();
    assert!(owned.allowed(&owned.scope));
    let mut other = owned.scope.clone();
    other.generation = 2;
    assert!(!owned.allowed(&other));
    assert!(
        app.effect_confirmed(&approve).is_err(),
        "the proposal cannot be replayed"
    );
    let public = serde_json::to_string(&app.agent_surface()).unwrap();
    assert!(!public.contains(&format!("passwords:request:{id}")));
    assert!(public.contains("passwords:grant:"));
    drop(app);
    drop(owned);
}

#[test]
fn protected_pointer_review_requires_modal_confirmation_before_granting_original_access() {
    let _secrets = ui::secrets::scope_thread(ui::secrets::memory_backend());
    let owned = Owned::new();
    let id = owned
        .owner
        .propose_scoped(
            owned.request.clone(),
            owned.scope.clone(),
            Arc::new(|| true),
        )
        .unwrap();
    let mut app = TestApp::<PasswordsShell>::for_person_with_lent(
        &owned.scope.principal,
        owned.owner.clone(),
    )
    .unwrap();
    app.effect(&PasswordsEffect::ShowAccess).unwrap();
    app.harness()
        .click_named(&format!("passwords.review-request:{id}:session"))
        .unwrap();
    app.settle();
    assert!(!owned.allowed(&owned.scope));
    let review = app.harness().inspection_json().unwrap();
    assert!(review.contains("passwords.dialog.access"));
    assert!(review.contains("synthetic-native-account-alpha"));
    app.harness()
        .click_named("passwords.dialog.access.primary")
        .unwrap();
    app.settle();
    assert!(owned.allowed(&owned.scope));
    assert!(owned.owner.proposals().unwrap().is_empty());
    drop(app);
    drop(owned);
}

#[test]
fn protected_approval_refuses_retired_request_and_decline_never_grants_access() {
    let _secrets = ui::secrets::scope_thread(ui::secrets::memory_backend());
    let owned = Owned::new();
    let live = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let checked = live.clone();
    let id = owned
        .owner
        .propose_scoped(
            owned.request.clone(),
            owned.scope.clone(),
            Arc::new(move || checked.load(Ordering::SeqCst)),
        )
        .unwrap();
    let mut app = TestApp::<PasswordsShell>::for_person_with_lent(
        &owned.scope.principal,
        owned.owner.clone(),
    )
    .unwrap();
    live.store(false, Ordering::SeqCst);
    assert!(
        app.effect_confirmed(&PasswordsEffect::ApproveAccess {
            proposal: id,
            lifetime: "persistent".into()
        })
        .is_err()
    );
    assert!(!owned.allowed(&owned.scope));
    let declined = owned
        .owner
        .propose_scoped(
            owned.request.clone(),
            owned.scope.clone(),
            Arc::new(|| true),
        )
        .unwrap();
    app.settle();
    app.effect(&PasswordsEffect::DenyAccess { proposal: declined })
        .unwrap();
    assert!(!owned.allowed(&owned.scope));
    assert!(owned.owner.proposals().unwrap().is_empty());
    drop(app);
    drop(owned);
}

#[test]
fn protected_persistent_approval_survives_only_its_exact_reviewed_resource() {
    let _secrets = ui::secrets::scope_thread(ui::secrets::memory_backend());
    let owned = Owned::new();
    let id = owned
        .owner
        .propose_scoped(
            owned.request.clone(),
            owned.scope.clone(),
            Arc::new(|| true),
        )
        .unwrap();
    let mut app = TestApp::<PasswordsShell>::for_person_with_lent(
        &owned.scope.principal,
        owned.owner.clone(),
    )
    .unwrap();
    app.effect_confirmed(&PasswordsEffect::ApproveAccess {
        proposal: id,
        lifetime: "persistent".into(),
    })
    .unwrap();
    assert!(owned.allowed(&owned.scope));
    let grant = owned.owner.grants().unwrap().pop().unwrap();
    assert_eq!(grant.lifetime, accounts::GrantLifetime::Persistent);
    drop(app);
    let mut reopened = CredentialBroker::new(SecretStore::open_in(owned.root.path()));
    assert!(
        reopened
            .request_existing_scoped(&owned.request, &owned.scope)
            .is_ok()
    );
    let mut other = owned.scope.clone();
    other.generation = 2;
    assert_eq!(
        reopened.request_existing_scoped(&owned.request, &other),
        Err(BrokerError::Denied)
    );
    drop(reopened);
    drop(owned);
}

#[test]
fn a_window_on_the_frame_loop_opens_the_vault_on_a_thread_of_its_own() {
    let root = tempfile::tempdir().unwrap();
    let mut store = SecretStore::open_in(root.path());
    let mut item = SecretItem::login("Synthetic opened title", "user", "password");
    store.put(&mut item).unwrap();
    drop(store);
    let path = root.path().to_owned();
    let opens = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&opens);
    let owner = CredentialOwner::deferred(&"11".repeat(32), "test-device", move || {
        counted.fetch_add(1, Ordering::SeqCst);
        SecretStore::open_in(&path)
    })
    .unwrap();
    let _loop = ui::storage::waiting::forbid_strictly_on_this_thread();
    let mut core = crate::vault::Core::with_model(crate::PasswordsModel::shared(owner.clone()));
    assert!(core.opening, "the window says the vault is opening");
    assert!(core.items.is_empty());
    tilo_test_env::wait_until("the vault to open", || owner.is_open());
    core.refresh_model();
    assert!(!core.opening);
    assert_eq!(opens.load(Ordering::SeqCst), 1, "the vault is opened once");
    assert!(
        core.items
            .iter()
            .any(|shown| shown.title == "Synthetic opened title"),
        "the window shows the vault once it is open"
    );
}

/// On the frame loop a vault edit is written by a writer thread, and an edit
/// made while one is under way waits in memory for the next write. An import
/// or a restore makes many edits at once: every one of them reaches the disk,
/// not only those the first writer took.
#[test]
fn every_imported_and_restored_item_reaches_the_disk_from_the_frame_loop() {
    use crate::transfer::ImportLogin;
    use zeroize::Zeroizing;

    const ROWS: usize = 12;
    const PASSPHRASE: &str = "synthetic backup passphrase";
    let root = tempfile::tempdir().unwrap();
    let path = root.path().to_owned();
    let owner = CredentialOwner::deferred(&"11".repeat(32), "test-device", move || {
        SecretStore::open_in(&path)
    })
    .unwrap();
    let backup = crate::transfer::export_encrypted_backup(
        &(0..ROWS)
            .map(|i| SecretItem::login(format!("Restored {i}"), "ada", format!("restored-{i}-A7")))
            .collect::<Vec<_>>(),
        PASSPHRASE,
    )
    .unwrap();
    let kept = |prefix: &str| {
        SecretStore::open_in(root.path())
            .list()
            .iter()
            .filter(|item| item.title.starts_with(prefix))
            .count()
    };
    let settle = |core: &mut crate::vault::Core| {
        tilo_test_env::wait_until("the vault's edits to be written", || {
            core.poll_transfers();
            !core.transfers_pending()
        });
    };
    let mut core = crate::vault::Core::with_model(crate::PasswordsModel::shared(owner));
    core.refresh_model();

    {
        let _loop = ui::storage::waiting::forbid_strictly_on_this_thread();
        let rows = (0..ROWS)
            .map(|i| ImportLogin {
                title: Zeroizing::new(format!("Imported {i}")),
                username: Zeroizing::new("ada".into()),
                origin: Zeroizing::new(String::new()),
                password: Zeroizing::new(format!("imported-{i}-A7")),
                notes: Zeroizing::new(String::new()),
            })
            .collect();
        core.dialog = Some(crate::vault::Dialog::ImportPreview {
            rows,
            skipped: vec![false; ROWS],
        });
        core.apply_import();
        settle(&mut core);
    }
    assert_eq!(kept("Imported"), ROWS, "every imported login is on disk");

    {
        let _loop = ui::storage::waiting::forbid_strictly_on_this_thread();
        core.backup_password = Zeroizing::new(PASSPHRASE.into());
        core.dialog = Some(crate::vault::Dialog::Restore {
            bytes: backup,
            error: None,
        });
        core.apply_encrypted_restore();
        settle(&mut core);
    }
    assert_eq!(kept("Restored"), ROWS, "every restored item is on disk");
}
