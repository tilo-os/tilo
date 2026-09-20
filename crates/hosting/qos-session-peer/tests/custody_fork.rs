//! Process inheritance must not extend custody after the owning session drops.
#![cfg(unix)]
use qos_session_peer::SessionOwner;

struct Child {
    pid: libc::pid_t,
    release: libc::c_int,
}
impl Child {
    fn release_and_wait(&mut self) -> std::io::Result<libc::c_int> {
        if self.release >= 0 {
            // EOF releases the parked child, including during unwinding.
            unsafe {
                libc::close(self.release);
            }
            self.release = -1;
        }
        let mut status = 0;
        loop {
            let result = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if result == self.pid {
                self.pid = 0;
                return Ok(status);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
    fn join(mut self) {
        let status = self.release_and_wait().expect("join custody fixture child");
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "custody fixture child did not exit normally and successfully"
        );
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        if self.pid > 0 {
            // Cleanup cannot panic while the parent is already unwinding.
            let _ = self.release_and_wait();
        }
    }
}

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
    let child = Child {
        pid,
        release: pipe[1],
    };
    drop(owner);
    let reopened = SessionOwner::open(&path);
    let failure = reopened.as_ref().err().map(|e| format!("{e:#}"));
    child.join();
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

#[test]
fn explicit_child_join_rejects_abnormal_exit() {
    for signaled in [false, true] {
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe {
                if signaled {
                    libc::kill(libc::getpid(), libc::SIGALRM);
                }
                libc::_exit(7);
            }
        }
        let result = std::panic::catch_unwind(|| Child { pid, release: -1 }.join());
        assert!(
            result.is_err(),
            "explicit join must reject unsuccessful or signaled child exit"
        );
    }
}
