use super::super::*;
use tilo_relay::delivery_v3::descriptor;

#[test]
fn transport_descriptor_admits_real_host_and_persists_exact_revision_across_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owner");
    let mut owner = SessionOwner::initialize(&root).unwrap();
    owner.activate_delivery().unwrap();
    let now = SystemTime::now();
    let hint = owner
        .install_delivery_transport("https://localhost:9443", 0, now)
        .unwrap();
    assert!(
        owner
            .peer()
            .certificates()
            .contains(owner.host_certificate())
    );
    let first = descriptor::inspect_persisted_hint(&hint, owner.host_certificate()).unwrap();
    assert_eq!(first.generation, 1);
    let committed = owner.committed_generation();
    let same = owner
        .install_delivery_transport("https://localhost:9443", 0, now)
        .unwrap();
    assert_eq!(
        postcard::to_stdvec(&hint).unwrap(),
        postcard::to_stdvec(&same).unwrap()
    );
    assert_eq!(owner.committed_generation(), committed);
    drop(owner);
    let mut owner = SessionOwner::open(&root).unwrap();
    let restored = owner
        .install_delivery_transport("https://localhost:9443", 0, now)
        .unwrap();
    assert_eq!(
        postcard::to_stdvec(&hint).unwrap(),
        postcard::to_stdvec(&restored).unwrap()
    );
    let expired = SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(first.expires_at_ns);
    let renewed = owner
        .install_delivery_transport("https://localhost:9443", 0, expired)
        .unwrap();
    assert_eq!(
        descriptor::inspect_persisted_hint(&renewed, owner.host_certificate())
            .unwrap()
            .generation,
        2
    );
    let current = crate::committed_authority::CommittedAuthority::new(&owner);
    let guard = current.read().unwrap();
    let verified = owner
        .verified_delivery_transport(guard.view(), expired, 0)
        .unwrap();
    assert_eq!(verified.generation(), 2);
    assert_eq!(verified.origin(), "https://localhost:9443");
}

#[test]
fn transport_installation_requires_explicit_v5_and_configuration_changes_advance_revision() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let now = SystemTime::now();
    assert!(
        owner
            .install_delivery_transport("https://localhost:9443", 0, now)
            .is_err()
    );
    owner.activate_delivery().unwrap();
    owner
        .install_delivery_transport("https://localhost:9443", 0, now)
        .unwrap();
    let changed = owner
        .install_delivery_transport("https://localhost:9444", 0, now)
        .unwrap();
    assert_eq!(
        descriptor::inspect_persisted_hint(&changed, owner.host_certificate())
            .unwrap()
            .generation,
        2
    );
    let before = owner.committed_generation();
    assert!(
        owner
            .install_delivery_transport("http://localhost:9444", 0, now)
            .is_err()
    );
    assert_eq!(owner.committed_generation(), before);
}
