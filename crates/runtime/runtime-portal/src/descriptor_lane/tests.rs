use super::*;
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::MetadataExt as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

fn identity(fd: impl AsFd) -> (u64, u64) {
    let file = std::fs::File::from(fd.as_fd().try_clone_to_owned().expect("duplicate"));
    let metadata = file.metadata().expect("descriptor identity");
    (metadata.dev(), metadata.ino())
}

#[tokio::test(flavor = "current_thread")]
async fn queued_descriptors_arrive_ahead_of_the_bytes_written_after_them() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let (client, service) = UnixStream::pair().expect("connected pair");
    let sender = DescriptorSender::new();
    let received = ReceivedDescriptors::new(512);
    let mut client = DescriptorStream::sending(client, sender.clone());
    let mut service = DescriptorStream::receiving(service, received.clone());
    // More than one batch, so later batches ride on the request's own bytes.
    let files = (0..BATCH * 2 + 3)
        .map(|index| {
            let path = directory.path().join(index.to_string());
            std::fs::write(&path, b"x").expect("fixture file");
            std::fs::File::open(path).expect("selected object")
        })
        .collect::<Vec<_>>();
    for (index, file) in files.iter().enumerate() {
        let number = sender.send_duplicate(file.as_fd()).expect("queued");
        assert_eq!(usize::try_from(number).expect("number"), index);
    }
    let request = vec![7_u8; 512];
    client.write_all(&request).await.expect("request bytes");
    let mut read = vec![0_u8; request.len()];
    service.read_exact(&mut read).await.expect("whole request");
    assert_eq!(read, request);
    assert_eq!(
        received.held(),
        files.len(),
        "every descriptor precedes the request's end"
    );
    for (index, file) in files.iter().enumerate() {
        let taken = received
            .take(u32::try_from(index).expect("number"))
            .expect("numbered descriptor");
        assert_eq!(identity(&taken), identity(file));
        assert!(
            rustix::io::fcntl_getfd(&taken)
                .expect("flags")
                .contains(rustix::io::FdFlags::CLOEXEC)
        );
    }
    assert!(received.take(0).is_none(), "a descriptor is taken once");
}

#[tokio::test(flavor = "current_thread")]
async fn descriptors_beyond_the_limit_are_closed_and_only_their_numbers_stay_empty() {
    let (client, service) = UnixStream::pair().expect("connected pair");
    let sender = DescriptorSender::new();
    let received = ReceivedDescriptors::new(2);
    let mut client = DescriptorStream::sending(client, sender.clone());
    let mut service = DescriptorStream::receiving(service, received.clone());
    let file = tempfile::tempfile().expect("object");
    for _ in 0..3 {
        sender.send_duplicate(file.as_fd()).expect("queued");
    }
    client.write_all(b"request").await.expect("request");
    let mut read = [0_u8; 7];
    service.read_exact(&mut read).await.expect("request");
    assert_eq!(received.held(), 2);
    assert!(received.take(2).is_none());
    received.discard([0, 1]);
    assert_eq!(received.held(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn a_stream_without_a_receiver_closes_what_the_peer_sends() {
    let (client, service) = UnixStream::pair().expect("connected pair");
    let sender = DescriptorSender::new();
    let mut client = DescriptorStream::sending(client, sender.clone());
    let (watched, peer) = std::os::unix::net::UnixStream::pair().expect("watched pair");
    sender.send(OwnedFd::from(peer)).expect("queued");
    client.write_all(b"r").await.expect("request");
    let mut service = service;
    let mut read = [0_u8; 1];
    service.read_exact(&mut read).await.expect("request");
    drop(client);
    // The service closed its received copy and the client's is gone, so the
    // watched socket's only peer has hung up.
    let mut watched = watched;
    let mut byte = [0_u8; 1];
    assert_eq!(
        std::io::Read::read(&mut watched, &mut byte).expect("hang-up"),
        0
    );
}

/// The peer hands its descriptor over; the receiver never needs rights over
/// the peer's process. A non-dumpable peer refuses `pidfd_getfd` at every
/// `kernel.yama.ptrace_scope` short of `CAP_SYS_PTRACE`, as scope 3 refuses
/// it for every peer.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "current_thread")]
async fn a_peer_that_cannot_be_traced_still_hands_over_its_descriptor() {
    let selected = tempfile::NamedTempFile::new().expect("selected object");
    let expected = identity(selected.as_file());
    let (child_end, service) = std::os::unix::net::UnixStream::pair().expect("connected pair");
    let (release_read, release_write) = std::os::unix::net::UnixStream::pair().expect("release");
    let selected_fd = selected.as_file().as_raw_fd();
    let child_fd = child_end.as_raw_fd();
    let release_fd = release_read.as_raw_fd();
    let release_write_fd = release_write.as_raw_fd();
    // SAFETY: the child calls only async-signal-safe syscalls and `_exit`.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed");
    if pid == 0 {
        // SAFETY: async-signal-safe syscalls on descriptors inherited from the
        // parent; `_exit` never unwinds into the test harness.
        unsafe {
            // Only the parent may hold the release end, so its drop wakes us.
            libc::close(release_write_fd);
            if libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0 {
                libc::_exit(10);
            }
            let socket = BorrowedFd::borrow_raw(child_fd);
            let fd = BorrowedFd::borrow_raw(selected_fd);
            if crate::fd_transport::send_with_fd(socket, b"r", fd).is_err() {
                libc::_exit(11);
            }
            let mut byte = 0_u8;
            libc::read(release_fd, (&raw mut byte).cast(), 1);
            libc::_exit(0);
        }
    }
    drop(child_end);
    service.set_nonblocking(true).expect("async socket");
    let received = ReceivedDescriptors::new(1);
    let mut service = DescriptorStream::receiving(
        UnixStream::from_std(service).expect("async socket"),
        received.clone(),
    );
    let mut read = [0_u8; 1];
    service.read_exact(&mut read).await.expect("request byte");
    let taken = received.take(0).expect("handed-over descriptor");
    assert_eq!(identity(&taken), expected);

    // Pulling the same descriptor out of the peer is refused: the path this
    // lane replaces could not work against this peer.
    // SAFETY: plain syscalls on a live child pid and its pidfd.
    unsafe {
        let pidfd = libc::syscall(libc::SYS_pidfd_open, pid, 0);
        assert!(pidfd >= 0, "pidfd_open");
        let pidfd = i32::try_from(pidfd).expect("pidfd");
        let pulled = libc::syscall(libc::SYS_pidfd_getfd, pidfd, selected_fd, 0);
        let error = std::io::Error::last_os_error().raw_os_error();
        if !rustix::process::geteuid().is_root() {
            assert_eq!(pulled, -1, "a non-dumpable peer refuses pidfd_getfd");
            assert_eq!(error, Some(libc::EPERM));
        }
        if pulled >= 0 {
            libc::close(i32::try_from(pulled).expect("fd"));
        }
        libc::close(pidfd);
    }
    drop(release_write);
    let mut status = 0;
    // SAFETY: reaps this test's own child.
    assert_eq!(unsafe { libc::waitpid(pid, &raw mut status, 0) }, pid);
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "child status {status}"
    );
}
