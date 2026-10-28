use std::io;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use crate::{
    NotAvailable,
    client::{read_frame, write_frame},
    inherited, wire,
};

fn metadata() -> wire::Response {
    wire::Response::Metadata {
        display_name: "Enrolled".into(),
        avatar_color: [1, 2, 3],
        group_public_key: [4; 32],
        signing_public_key: [5; 32],
        device_certificate: Some(vec![6; 64]),
        threshold: 1,
        total_shares: 1,
        share_index: 1,
    }
}

fn error_kind<T>(result: Result<T, NotAvailable>) -> io::ErrorKind {
    match result {
        Err(NotAvailable::ConnectionFailed(error)) => error.kind(),
        _ => panic!("expected a connection failure"),
    }
}

#[test]
fn inherited_consumers_share_metadata_and_the_framing_lock_without_consuming_authority() {
    let (app, mut host) = UnixStream::pair().unwrap();
    assert_eq!(unsafe { libc::fcntl(app.as_raw_fd(), libc::F_SETFD, 0) }, 0);
    let worker = std::thread::spawn(move || {
        assert!(matches!(
            read_frame(&mut host).unwrap(),
            wire::Request::Metadata
        ));
        write_frame(&mut host, &metadata()).unwrap();
        let mut messages = Vec::new();
        for _ in 0..2 {
            let wire::Request::Sign { action, message } = read_frame(&mut host).unwrap() else {
                panic!("metadata must be exchanged only once");
            };
            assert_eq!(action, "document.save");
            messages.push(message);
            write_frame(&mut host, &wire::Response::Denied("revoked".into())).unwrap();
        }
        messages.sort();
        assert_eq!(messages, [b"first".to_vec(), b"second".to_vec()]);
    });
    let first = inherited::connect_service(app.as_fd()).unwrap();
    let second = inherited::connect_service(app.as_fd()).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    std::thread::scope(|scope| {
        let one = scope.spawn(|| error_kind(first.sign_for("document.save", b"first")));
        let two = scope.spawn(|| error_kind(second.sign_for("document.save", b"second")));
        assert_eq!(one.join().unwrap(), io::ErrorKind::PermissionDenied);
        assert_eq!(two.join().unwrap(), io::ErrorKind::PermissionDenied);
    });
    worker.join().unwrap();
    drop((first, second));
    let flags = unsafe { libc::fcntl(app.as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0);
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
}

#[test]
fn a_disconnected_service_is_not_replaced_with_another_identity() {
    let (app, mut host) = UnixStream::pair().unwrap();
    let worker = std::thread::spawn(move || {
        assert!(matches!(
            read_frame(&mut host).unwrap(),
            wire::Request::Metadata
        ));
        write_frame(&mut host, &metadata()).unwrap();
    });
    let identity = inherited::connect_service(app.as_fd()).unwrap();
    worker.join().unwrap();
    for _ in 0..2 {
        assert!(identity.sign_collaboration_join("room").is_err());
        let reacquired = inherited::connect_service(app.as_fd()).unwrap();
        assert!(Arc::ptr_eq(&identity, &reacquired));
        assert_eq!(reacquired.metadata().signing_public_key(), [5; 32]);
    }
}

#[test]
fn a_reused_descriptor_does_not_reuse_another_connections_identity() {
    let (app, mut host) = UnixStream::pair().unwrap();
    let first_worker = std::thread::spawn(move || {
        assert!(matches!(
            read_frame(&mut host).unwrap(),
            wire::Request::Metadata
        ));
        write_frame(&mut host, &metadata()).unwrap();
    });
    let first = inherited::connect_service(app.as_fd()).unwrap();
    first_worker.join().unwrap();
    let (replacement, mut host) = UnixStream::pair().unwrap();
    let second_worker = std::thread::spawn(move || {
        assert!(matches!(
            read_frame(&mut host).unwrap(),
            wire::Request::Metadata
        ));
        let mut reply = metadata();
        let wire::Response::Metadata {
            ref mut signing_public_key,
            ..
        } = reply
        else {
            panic!("expected metadata fixture");
        };
        *signing_public_key = [9; 32];
        write_frame(&mut host, &reply).unwrap();
    });
    // SAFETY: both sockets remain owned; dup2 replaces only app's live descriptor.
    assert_eq!(
        unsafe { libc::dup2(replacement.as_raw_fd(), app.as_raw_fd()) },
        app.as_raw_fd()
    );
    let second = inherited::connect_service(app.as_fd()).unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(first.metadata().signing_public_key(), [5; 32]);
    assert_eq!(second.metadata().signing_public_key(), [9; 32]);
    second_worker.join().unwrap();
}

#[test]
#[serial_test::serial(identity_env)]
fn private_record_and_ambient_config_never_supply_a_client_identity() {
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("primary.json");
    std::fs::write(
        &record,
        br#"{"secret_bytes":[1,2,3],"display_name":"Ambient"}"#,
    )
    .unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove("TILO_SVC_IDENTITY_FD")
        .remove(crate::legacy::BROKER_ENV_VAR)
        .set("TILO_USER_IDENTITY", &record)
        .set("XDG_CONFIG_HOME", dir.path())
        .set("HOME", dir.path());
    assert!(matches!(crate::load(), Err(NotAvailable::ServiceFdMissing)));
    assert!(matches!(
        crate::load_metadata(),
        Err(NotAvailable::ServiceFdMissing)
    ));
    assert!(crate::bound_profile_photo().is_none());
    env.set(crate::legacy::BROKER_ENV_VAR, "not-a-descriptor");
    assert_eq!(error_kind(crate::load()), io::ErrorKind::InvalidInput);
    env.set(crate::legacy::BROKER_ENV_VAR, "2");
    assert_eq!(error_kind(crate::load()), io::ErrorKind::InvalidInput);
}
