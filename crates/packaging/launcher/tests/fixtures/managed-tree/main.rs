//! Opt-in Linux qualification fixture, never an application or provider.
//! Cargo builds this through the checkout's normal compiler budget. Its only
//! protocol is the real managed startup challenge followed by private test bytes.
#[cfg(target_os = "linux")]
fn main() {
    use std::{
        io::{Read, Write},
        os::fd::{AsRawFd, FromRawFd},
        os::unix::net::UnixStream,
    };
    fn service(name: &str) -> UnixStream {
        let fd: i32 = std::env::var(name)
            .expect("required enrolled descriptor")
            .parse()
            .unwrap();
        assert!(fd >= 3);
        // SAFETY: the launcher exclusively transfers this descriptor to us.
        unsafe { UnixStream::from_raw_fd(fd) }
    }
    let mut collab = service("TILO_SVC_COLLAB_FD");
    // svc_env_var currently uppercases the canonical service name verbatim.
    let mut model = service("TILO_SVC_MODEL-GATEWAY_FD");
    collab
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let mut challenge = [0; 32];
    collab.read_exact(&mut challenge).unwrap();
    collab.write_all(&challenge).unwrap();
    let mut admitted = [0];
    collab.read_exact(&mut admitted).unwrap();
    assert_eq!(
        admitted,
        [1],
        "actual host admission must precede the fixture"
    );
    collab.set_read_timeout(None).unwrap();

    // No threads have been created. After fork the child branches use only libc
    // and terminate with _exit; they never unwind or run inherited Rust drops.
    let intermediate = unsafe { libc::fork() };
    assert!(intermediate >= 0, "intermediate fork failed");
    if intermediate == 0 {
        let original_parent = unsafe { libc::getpid() };
        let grandchild = unsafe { libc::fork() };
        if grandchild < 0 {
            unsafe { libc::_exit(41) };
        }
        if grandchild > 0 {
            unsafe { libc::_exit(0) };
        }
        // The initial application is PID 2 by default, not PID 1: bubblewrap
        // owns the namespace reaper. Wait until this orphan has that parent.
        while unsafe { libc::getppid() } == original_parent {
            let delay = libc::timespec {
                tv_sec: 0,
                tv_nsec: 1_000_000,
            };
            unsafe { libc::nanosleep(&delay, std::ptr::null_mut()) };
        }
        if unsafe { libc::getppid() } != 1 {
            unsafe { libc::_exit(42) };
        }
        let fd = collab.as_raw_fd();
        if unsafe { libc::write(fd, b"R".as_ptr().cast(), 1) } != 1 {
            unsafe { libc::_exit(43) };
        }
        // The orphan has no voluntary exit path, including descriptor EOF.
        // Only real sandbox teardown or the test's exact pidfd failure cleanup
        // can terminate it after READY.
        loop {
            unsafe { libc::pause() };
        }
    }
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(intermediate, &mut status, 0) },
        intermediate
    );
    assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
    collab.write_all(b"P").unwrap(); // Intermediate was actually waited/reaped.
    let mut exit = [0];
    model.read_exact(&mut exit).unwrap();
    assert_eq!(exit, [b'E']);
    // The reparented descendant remains blocked in pause until the actual
    // sandbox teardown kills it.
}

#[cfg(not(target_os = "linux"))]
fn main() {
    panic!("managed-tree qualification fixture requires Linux");
}
