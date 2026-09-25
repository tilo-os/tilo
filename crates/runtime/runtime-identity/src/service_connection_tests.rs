use super::*;
use std::os::fd::IntoRawFd;

#[test]
fn metadata_lookup_does_not_consume_inherited_service() {
    let (client, mut server) = UnixStream::pair().unwrap();
    let fd = client.into_raw_fd();
    // Match the launcher: this descriptor was deliberately inherited at exec.
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0);
    let worker = std::thread::spawn(move || {
        for _ in 0..2 {
            assert!(matches!(
                read_frame::<wire::Request>(&mut server).unwrap(),
                wire::Request::Metadata
            ));
            write_frame(
                &mut server,
                &wire::Response::Metadata {
                    display_name: "Runtime test".into(),
                    avatar_color: [1, 2, 3],
                    group_public_key: [1; 32],
                    signing_public_key: [2; 32],
                    device_certificate: None,
                    threshold: 1,
                    total_shares: 1,
                    share_index: 0,
                },
            )
            .unwrap();
        }
    });
    drop(load_from_service(fd).unwrap());
    // A second SDK consumer must still be able to acquire this service.
    let open = unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0;
    if open {
        let second = load_from_service(fd).unwrap();
        assert_eq!(second.display_name(), "Runtime test");
        drop(second);
        worker.join().unwrap();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        unsafe {
            libc::close(fd);
        }
        assert_ne!(
            flags & libc::FD_CLOEXEC,
            0,
            "SDK must not leak the inherited capability into later subprocesses"
        );
    }
    assert!(
        open,
        "metadata lookup closed the inherited identity capability"
    );
}

#[test]
fn simultaneous_sdk_consumers_share_one_portable_client_and_framing_lock() {
    let (client, mut server) = UnixStream::pair().unwrap();
    let fd = client.into_raw_fd();
    let worker = std::thread::spawn(move || {
        assert!(matches!(
            read_frame::<wire::Request>(&mut server).unwrap(),
            wire::Request::Metadata
        ));
        write_frame(
            &mut server,
            &wire::Response::Metadata {
                display_name: "Runtime test".into(),
                avatar_color: [1, 2, 3],
                group_public_key: [1; 32],
                signing_public_key: [2; 32],
                device_certificate: None,
                threshold: 1,
                total_shares: 1,
                share_index: 0,
            },
        )
        .unwrap();
        let mut messages = Vec::new();
        for _ in 0..2 {
            let wire::Request::Sign { action, message } = read_frame(&mut server).unwrap() else {
                panic!("live SDK consumers must share the existing metadata connection");
            };
            assert_eq!(action, "document.save");
            messages.push(message);
            write_frame(&mut server, &wire::Response::Signature(vec![7; 64])).unwrap();
        }
        messages.sort();
        assert_eq!(messages, [b"first".to_vec(), b"second".to_vec()]);
    });
    let first = load_from_service(fd).unwrap();
    let second = load_from_service(fd).unwrap();
    std::thread::scope(|scope| {
        let one = scope.spawn(|| first.sign_for("document.save", b"first").unwrap());
        let two = scope.spawn(|| second.sign_for("document.save", b"second").unwrap());
        assert_eq!(one.join().unwrap(), [7; 64]);
        assert_eq!(two.join().unwrap(), [7; 64]);
    });
    worker.join().unwrap();
    drop((first, second));
    assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
    unsafe {
        libc::close(fd);
    }
}
