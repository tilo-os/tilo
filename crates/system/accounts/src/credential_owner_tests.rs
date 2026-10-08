use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn profile_key_refuses_local_custody_even_with_exact_protected_grant() {
    let (root, mut broker, request, scope) = fixture();
    assert_eq!(broker.store.custody(), SecretCustody::LocalFile);
    broker
        .request_scoped(&request, &scope, |_, _, _| AccessDecision::AllowForSession)
        .unwrap();
    assert!(broker.request_existing_scoped(&request, &scope).is_ok());
    assert_eq!(
        broker.request_existing_profile_key(&request, &scope),
        Err(BrokerError::Denied)
    );
    drop(broker);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn profile_key_preparation_refuses_foreign_scope_before_open_and_local_fallback() {
    let opened = Arc::new(AtomicUsize::new(0));
    let count = opened.clone();
    let root = std::env::temp_dir().join(format!("tilo-profile-key-{}", uuid::Uuid::new_v4()));
    let store_root = root.clone();
    let principal = "11".repeat(32);
    let owner = CredentialOwner::deferred(&principal, "test-device", move || {
        count.fetch_add(1, Ordering::SeqCst);
        SecretStore::open_in(&store_root)
    })
    .unwrap();
    let mut scope = CredentialScope {
        principal,
        device: "test-device".into(),
        service: "signal".into(),
        account: "fixture-alpha".into(),
        generation: 1,
    };
    scope.device = "other-device".into();
    assert_eq!(
        owner.prepare_profile_key(&scope, "https://messaging.test"),
        Err(BrokerError::Denied)
    );
    assert_eq!(opened.load(Ordering::SeqCst), 0);
    assert!(!root.exists());
    scope.device = "test-device".into();
    assert_eq!(
        owner.prepare_profile_key(&scope, "https://messaging.test"),
        Err(BrokerError::Denied)
    );
    assert_eq!(opened.load(Ordering::SeqCst), 1);
    assert!(owner.read(SecretStore::list).unwrap().is_empty());
    assert!(owner.grants().unwrap().is_empty());
    drop(owner);
    assert!(!root.exists(), "refused preparation writes no key or vault");
}

pub(super) fn fixture() -> (
    std::path::PathBuf,
    CredentialBroker,
    AccessRequest,
    CredentialScope,
) {
    let root = std::env::temp_dir().join(format!("tilo-owned-credential-{}", uuid::Uuid::new_v4()));
    let mut store = SecretStore::open_in(&root);
    let mut item = SecretItem::login(
        "Synthetic private account",
        "synthetic-user",
        "synthetic-private-value",
    );
    item.add_origin("https://messaging.test").unwrap();
    store.put(&mut item).unwrap();
    let request = AccessRequest {
        app_id: "so.tilo.messages".into(),
        item_id: item.id,
        purpose: "Use selected account".into(),
        origin: Some("https://messaging.test".into()),
    };
    let scope = CredentialScope {
        principal: "11".repeat(32),
        device: "test-device".into(),
        service: "signal".into(),
        account: "synthetic-alpha".into(),
        generation: 1,
    };
    (root, CredentialBroker::new(store), request, scope)
}

#[test]
fn scoped_permission_never_inherits_legacy_account_device_service_or_generation() {
    let (root, mut broker, request, scope) = fixture();
    broker
        .request(&request, |_, _| AccessDecision::AllowForSession)
        .unwrap();
    assert_eq!(
        broker.request_existing_scoped(&request, &scope),
        Err(BrokerError::Denied)
    );
    broker
        .request_scoped(&request, &scope, |_, _, displayed| {
            assert_eq!(
                displayed, &scope,
                "the user decision receives the exact frozen resource"
            );
            AccessDecision::AllowForSession
        })
        .unwrap();
    assert!(broker.request_existing_scoped(&request, &scope).is_ok());
    for field in 0..5 {
        let mut other = scope.clone();
        match field {
            0 => other.principal = "22".repeat(32),
            1 => other.device = "other-device".into(),
            2 => other.service = "whatsapp".into(),
            3 => other.account = "synthetic-beta".into(),
            _ => other.generation = 2,
        }
        assert_eq!(
            broker.request_existing_scoped(&request, &other),
            Err(BrokerError::Denied)
        );
    }
    assert!(broker.persistent_grants().is_empty());
    drop(broker);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn original_owner_revoke_is_exact_and_stale_permission_cannot_revoke_new_generation() {
    let (root, mut broker, request, alpha) = fixture();
    let mut beta = alpha.clone();
    beta.account = "synthetic-beta".into();
    for scope in [&alpha, &beta] {
        broker
            .request_scoped(&request, scope, |_, _, _| AccessDecision::AllowForSession)
            .unwrap();
    }
    let broker = Arc::new(Mutex::new(broker));
    let owner =
        CredentialOwner::from_broker(&alpha.principal, &alpha.device, broker.clone()).unwrap();
    assert!(Arc::ptr_eq(&owner.broker().unwrap(), &broker));
    let grant = owner
        .grants()
        .unwrap()
        .into_iter()
        .find(|grant| grant.scope.as_ref() == Some(&alpha))
        .unwrap();
    assert_eq!(grant.lifetime, GrantLifetime::Session);
    assert!(owner.revoke_grant(&grant).unwrap());
    {
        let mut current = broker.lock().unwrap();
        assert_eq!(
            current.request_existing_scoped(&request, &alpha),
            Err(BrokerError::Denied)
        );
        assert!(current.request_existing_scoped(&request, &beta).is_ok());
        current
            .request_scoped(&request, &alpha, |_, _, _| AccessDecision::AllowForSession)
            .unwrap();
    }
    assert!(
        !owner.revoke_grant(&grant).unwrap(),
        "an old ID is never a substitute for the current permission"
    );
    assert!(
        broker
            .lock()
            .unwrap()
            .request_existing_scoped(&request, &alpha)
            .is_ok()
    );
    assert!(broker.lock().unwrap().persistent_grants().is_empty());
    drop(owner);
    drop(broker);
    assert_eq!(
        CredentialBroker::new(SecretStore::open_in(&root))
            .request_existing_scoped(&request, &alpha),
        Err(BrokerError::Denied)
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn deferred_native_owner_does_not_open_storage_until_needed_and_opens_once() {
    let root =
        std::env::temp_dir().join(format!("tilo-deferred-credential-{}", uuid::Uuid::new_v4()));
    let count = Arc::new(AtomicUsize::new(0));
    let opened = count.clone();
    let path = root.clone();
    let owner = CredentialOwner::deferred("11".repeat(32).as_str(), "test-device", move || {
        opened.fetch_add(1, Ordering::SeqCst);
        SecretStore::open_in(&path)
    })
    .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(owner.revision(), None);
    let first = owner.broker().unwrap();
    let other = owner.clone();
    let worker = std::thread::spawn(move || other.broker().unwrap());
    let second = worker.join().unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert!(Arc::ptr_eq(&first, &owner.broker().unwrap()));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(
        !root.exists(),
        "read-only empty startup never provisions a key or file"
    );
}

#[test]
fn scoped_durable_permission_survives_only_its_exact_frozen_resource() {
    let (root, mut broker, request, scope) = fixture();
    broker
        .request_scoped(&request, &scope, |_, _, _| AccessDecision::AllowAlways)
        .unwrap();
    let first = broker.current_grants().pop().unwrap();
    assert_eq!(first.lifetime, GrantLifetime::Persistent);
    drop(broker);
    let mut reopened = CredentialBroker::new(SecretStore::open_in(&root));
    assert!(reopened.request_existing_scoped(&request, &scope).is_ok());
    assert_eq!(reopened.current_grants().pop().unwrap().id(), first.id());
    let mut next = scope.clone();
    next.generation = 2;
    assert_eq!(
        reopened.request_existing_scoped(&request, &next),
        Err(BrokerError::Denied)
    );
    assert_eq!(
        reopened.request_existing(&request),
        Err(BrokerError::Denied)
    );
    assert!(reopened.revoke_grant(&first).unwrap());
    drop(reopened);
    assert_eq!(
        CredentialBroker::new(SecretStore::open_in(&root))
            .request_existing_scoped(&request, &scope),
        Err(BrokerError::Denied)
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn proposal_creates_no_access_and_confirmed_session_decision_is_exact_and_one_use() {
    let (root, broker, request, scope) = fixture();
    let broker = Arc::new(Mutex::new(broker));
    let owner =
        CredentialOwner::from_broker(&scope.principal, &scope.device, broker.clone()).unwrap();
    let wake_count = Arc::new(AtomicUsize::new(0));
    let wakes = wake_count.clone();
    let reentrant = owner.clone();
    owner.bind_request_wake(Arc::new(move || {
        assert!(
            reentrant.revision().is_some(),
            "wake runs outside the broker lock"
        );
        let _ = reentrant.proposal_wake_after();
        wakes.fetch_add(1, Ordering::SeqCst);
    }));
    let id = owner
        .propose_scoped(request.clone(), scope.clone(), Arc::new(|| true))
        .unwrap();
    assert!(owner.proposal_wake_after().unwrap() <= std::time::Duration::from_secs(120));
    assert!(owner.grants().unwrap().is_empty());
    assert_eq!(
        broker
            .lock()
            .unwrap()
            .request_existing_scoped(&request, &scope),
        Err(BrokerError::Denied)
    );
    let review = owner.proposals().unwrap();
    assert_eq!(review.len(), 1);
    assert_eq!(review[0].request, request);
    assert_eq!(review[0].scope, scope);
    let grant = owner
        .decide_proposal(&id, AccessDecision::AllowForSession)
        .unwrap()
        .unwrap();
    assert_eq!(grant.lifetime, GrantLifetime::Session);
    assert!(
        broker
            .lock()
            .unwrap()
            .request_existing_scoped(&request, &scope)
            .is_ok()
    );
    let mut other = scope;
    other.generation = 2;
    assert_eq!(
        broker
            .lock()
            .unwrap()
            .request_existing_scoped(&request, &other),
        Err(BrokerError::Denied)
    );
    assert!(
        owner
            .decide_proposal(&id, AccessDecision::AllowAlways)
            .is_err()
    );
    assert!(owner.proposals().unwrap().is_empty());
    assert!(wake_count.load(Ordering::SeqCst) >= 2);
    owner.bind_request_wake(Arc::new(|| {}));
    drop(review);
    drop(owner);
    drop(broker);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn proposal_refuses_foreign_or_retired_admission_before_opening_custody() {
    let count = Arc::new(AtomicUsize::new(0));
    let opens = count.clone();
    let (root, broker, request, scope) = fixture();
    drop(broker);
    let custody = root.clone();
    let owner = CredentialOwner::deferred(&scope.principal, &scope.device, move || {
        opens.fetch_add(1, Ordering::SeqCst);
        SecretStore::open_in(&custody)
    })
    .unwrap();
    let mut foreign = scope.clone();
    foreign.device = "foreign-device".into();
    assert!(
        owner
            .propose_scoped(request.clone(), foreign, Arc::new(|| true))
            .is_err()
    );
    assert!(
        owner
            .propose_scoped(request, scope, Arc::new(|| false))
            .is_err()
    );
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(owner.revision(), None);
    drop(owner);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn proposal_vault_edit_invalidates_review_and_never_grants_the_replacement_secret() {
    let (root, broker, request, scope) = fixture();
    let broker = Arc::new(Mutex::new(broker));
    let owner =
        CredentialOwner::from_broker(&scope.principal, &scope.device, broker.clone()).unwrap();
    let id = owner
        .propose_scoped(request.clone(), scope, Arc::new(|| true))
        .unwrap();
    owner
        .edit(|store| {
            let mut item = store.get(&request.item_id).unwrap();
            item.password = "replacement-private-value".into();
            store.put(&mut item).unwrap();
        })
        .unwrap();
    assert!(
        owner
            .decide_proposal(&id, AccessDecision::AllowAlways)
            .is_err()
    );
    assert!(owner.grants().unwrap().is_empty());
    assert!(owner.proposals().unwrap().is_empty());
    drop(owner);
    drop(broker);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn proposal_retirement_during_decision_revokes_only_its_new_exact_permission() {
    let (root, mut broker, request, scope) = fixture();
    let mut other = scope.clone();
    other.account = "other-account".into();
    broker
        .request_scoped(&request, &other, |_, _, _| AccessDecision::AllowForSession)
        .unwrap();
    let broker = Arc::new(Mutex::new(broker));
    let owner =
        CredentialOwner::from_broker(&scope.principal, &scope.device, broker.clone()).unwrap();
    let checks = Arc::new(AtomicUsize::new(0));
    let authority = checks.clone();
    let id = owner
        .propose_scoped(
            request.clone(),
            scope.clone(),
            Arc::new(move || authority.fetch_add(1, Ordering::SeqCst) < 3),
        )
        .unwrap();
    assert!(
        owner
            .decide_proposal(&id, AccessDecision::AllowForSession)
            .is_err()
    );
    assert_eq!(checks.load(Ordering::SeqCst), 4);
    assert_eq!(
        broker
            .lock()
            .unwrap()
            .request_existing_scoped(&request, &scope),
        Err(BrokerError::Denied)
    );
    assert!(
        broker
            .lock()
            .unwrap()
            .request_existing_scoped(&request, &other)
            .is_ok()
    );
    assert_eq!(owner.grants().unwrap().len(), 1);
    drop(owner);
    drop(broker);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn proposal_queue_is_bounded_and_denial_consumes_without_authorizing() {
    let (root, broker, request, scope) = fixture();
    let broker = Arc::new(Mutex::new(broker));
    let owner =
        CredentialOwner::from_broker(&scope.principal, &scope.device, broker.clone()).unwrap();
    let mut ids = Vec::new();
    for generation in 1..=8 {
        let mut current = scope.clone();
        current.generation = generation;
        ids.push(
            owner
                .propose_scoped(request.clone(), current, Arc::new(|| true))
                .unwrap(),
        );
    }
    let mut ninth = scope.clone();
    ninth.generation = 9;
    assert!(
        owner
            .propose_scoped(request.clone(), ninth, Arc::new(|| true))
            .is_err()
    );
    assert!(
        owner
            .propose_scoped(request, scope, Arc::new(|| true))
            .is_err()
    );
    assert!(
        owner
            .decide_proposal(&ids[0], AccessDecision::AllowOnce)
            .is_err()
    );
    assert_eq!(owner.proposals().unwrap().len(), 8);
    assert!(
        owner
            .decide_proposal(&ids[0], AccessDecision::Deny)
            .unwrap()
            .is_none()
    );
    assert!(
        owner
            .decide_proposal(&ids[0], AccessDecision::AllowAlways)
            .is_err()
    );
    assert_eq!(owner.proposals().unwrap().len(), 7);
    assert!(owner.grants().unwrap().is_empty());
    drop(owner);
    drop(broker);
    std::fs::remove_dir_all(root).unwrap();
}
