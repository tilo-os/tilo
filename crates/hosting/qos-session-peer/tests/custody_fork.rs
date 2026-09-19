//! Process inheritance must not extend custody after the owning session drops.
#![cfg(unix)]
use qos_session_peer::SessionOwner;

#[test]
fn dropping_owner_releases_custody_while_a_fork_child_still_inherits_descriptors() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    let owner = SessionOwner::initialize(&path).unwrap();
    assert!(
        SessionOwner::open(&path).is_err(),
        "live owner must retain exclusive custody"
    );
    let mut pipe = [-1; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
        // After fork the child uses only async-signal-safe libc operations. The
        // alarm is a bounded escape hatch even if the parent panics or dies.
        unsafe {
            libc::close(pipe[1]);
            libc::alarm(5);
            let mut byte = 0u8;
            libc::read(pipe[0], (&mut byte as *mut u8).cast(), 1);
            libc::_exit(0);
        }
    }
    unsafe {
        libc::close(pipe[0]);
    }
    struct Child {
        pid: libc::pid_t,
        release: libc::c_int,
    }
    impl Drop for Child {
        fn drop(&mut self) {
            unsafe {
                // EOF releases the parked child; waitpid joins our child only.
                libc::close(self.release);
                libc::waitpid(self.pid, std::ptr::null_mut(), 0);
            }
        }
    }
    let child = Child {
        pid,
        release: pipe[1],
    };
    drop(owner);
    let reopened = SessionOwner::open(&path);
    let failure = reopened.as_ref().err().map(|e| format!("{e:#}"));
    drop(child);
    if reopened.is_ok() {
        assert!(
            SessionOwner::open(&path).is_err(),
            "inherited child exit must not unlock the replacement owner"
        );
    }
    drop(reopened);
    // Isolate inherited-descriptor lifetime from a stale on-disk lock file.
    assert!(
        SessionOwner::open(&path).is_ok(),
        "custody remained unavailable after child exit"
    );
    assert!(
        failure.is_none(),
        "dropped owner retained custody through fork child: {failure:?}"
    );
}
