#[test]
fn a_workspace_without_an_identity_asks_for_one_and_other_failures_say_why() {
    let no_identity = runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        runtime_identity::NoIdentitySelected,
    ));
    assert!(matches!(
        super::native_launch_refusal(&no_identity),
        crate::launch::LaunchError::IdentityRequired
    ));
    let refused = runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "workspace identity has no certified device",
    ));
    assert!(matches!(
        super::native_launch_refusal(&refused),
        crate::launch::LaunchError::Operation(message)
            if message.contains("workspace identity has no certified device")
    ));
}
