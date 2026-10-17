use super::*;

#[test]
fn capture_owner_refuses_root_or_mismatched_identity_before_runtime_access() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = directory.path().join("must-not-be-opened");
    let actual = rustix::process::getuid().as_raw();
    let different = if actual == 1 { 2 } else { 1 };
    for uid in [0, different] {
        let source = File::open(directory.path()).unwrap();
        let result = CaptureHostOwner::verify(&runtime, "caller-selected", source, uid);
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::PermissionDenied));
    }
    assert!(!runtime.exists());
}

#[test]
fn capture_owner_never_admits_missing_signed_runtime_from_descriptors_alone() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = directory.path().join("absent-runtime");
    let source = File::open(directory.path()).unwrap();
    assert!(
        CaptureHostOwner::verify(
            &runtime,
            &"00".repeat(32),
            source,
            rustix::process::getuid().as_raw(),
        )
        .is_err()
    );
}
