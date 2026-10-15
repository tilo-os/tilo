use super::*;

#[test]
fn offload_is_turned_off_only_by_an_explicit_off() {
    for off in ["off", "0", "false", "no", " OFF "] {
        assert!(disabled(Some(off)), "{off:?}");
    }
    for on in [None, Some(""), Some("on"), Some("1")] {
        assert!(!disabled(on), "{on:?}");
    }
}

#[test]
fn a_device_without_a_registry_has_no_relay_to_offload_to() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        read_registry(&dir.path().join("home-peers.json"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_unreadable_registry_is_refused_rather_than_read_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("home-peers.json");
    std::fs::write(&path, b"{ not a registry").unwrap();
    let error = read_registry(&path).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    let oversized = usize::try_from(MAX_REGISTRY_BYTES)
        .unwrap()
        .saturating_add(1);
    std::fs::write(&path, vec![b' '; oversized]).unwrap();
    let error = read_registry(&path).unwrap_err();
    assert!(error.to_string().contains("bound"), "{error}");
}

#[test]
fn a_session_without_rooms_has_no_route() {
    assert!(configured(&[]).unwrap().is_none());
}
