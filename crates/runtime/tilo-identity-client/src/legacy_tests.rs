use std::io;
use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;

use crate::{NotAvailable, legacy::*};

#[test]
#[serial_test::serial(identity_env)]
fn legacy_handoff_preserves_metadata_refusal_ownership_and_disconnection() {
    let (mut host, app) = UnixStream::pair().unwrap();
    let raw = app.into_raw_fd();
    let worker = std::thread::spawn(move || {
        assert_eq!(read_frame(&mut host).unwrap(), (OP_METADATA, Vec::new()));
        let metadata = PublicMetadata {
            profile_photo: Some(crate::ProfilePhoto::new(1, 1, vec![1, 2, 3, 255]).unwrap()),
            display_name: "Enrolled".into(),
            avatar_color: [1, 2, 3],
            group_public_key: [4; 32],
            threshold: 1,
            total_shares: 1,
            share_index: 1,
            single_device: false,
        };
        write_frame(&mut host, 0, &serde_json::to_vec(&metadata).unwrap()).unwrap();
        assert_eq!(
            read_frame(&mut host).unwrap(),
            (OP_SIGN, b"message".to_vec())
        );
        write_frame(&mut host, 1, b"root signing is refused").unwrap();
        assert_eq!(
            read_frame(&mut host).unwrap(),
            (OP_SIGN, crate::collaboration_join_signing_bytes("room")),
        );
        write_frame(&mut host, 1, b"root signing is refused").unwrap();
    });
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove("TILO_SVC_IDENTITY_FD")
        .set(BROKER_ENV_VAR, raw.to_string());
    let identity = crate::load().unwrap();
    assert_eq!(identity.metadata().display_name(), "Enrolled");
    assert_eq!(identity.metadata().group_public_key(), [4; 32]);
    assert_eq!(
        identity.metadata().profile_photo(),
        Some(&crate::ProfilePhoto::new(1, 1, vec![1, 2, 3, 255]).unwrap()),
    );
    assert!(!identity.metadata().is_single_device());
    assert!(identity.metadata().device_certificate().is_none());
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    match identity.sign_for("any.action", b"message") {
        Err(NotAvailable::ConnectionFailed(error)) => {
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(error.to_string(), "root signing is refused");
        }
        other => panic!("unexpected signing result: {other:?}"),
    }
    assert!(matches!(identity.sign_collaboration_join("room"),
        Err(NotAvailable::ConnectionFailed(error)) if error.kind() == io::ErrorKind::PermissionDenied));
    assert!(matches!(identity.channel_keys("room", "app", "record", 0),
        Err(NotAvailable::ConnectionFailed(error)) if error.kind() == io::ErrorKind::Unsupported));
    worker.join().unwrap();
    assert!(identity.sign(b"disconnected").is_err());
    assert!(
        matches!(crate::load(), Err(NotAvailable::ConnectionFailed(error))
        if error.kind() == io::ErrorKind::AlreadyExists)
    );
    drop(identity);
    assert_eq!(unsafe { libc::fcntl(raw, libc::F_GETFD) }, -1);
}
