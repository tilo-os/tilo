//! Explicit native qualification inside a fresh private, no-network container.
//! Its root launcher provisions `/run/user/<uid>` before dropping to that UID.
#[test]
fn native_goa_owner_read_qualifies_on_isolated_real_dbus() {
    let owner = unsafe { libc::getuid() };
    assert_ne!(owner, 0, "GOA fixture requires an ordinary original owner");
    assert_eq!(owner, unsafe { libc::geteuid() });
    let socket = std::path::PathBuf::from(format!("/run/user/{owner}/bus"));
    assert!(
        !socket.exists(),
        "native qualification requires a fresh private session namespace"
    );
    super::tests::qualify();
}
