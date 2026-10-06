//! Opt-in Linux qualification fixture, never an application or provider.
//! Cargo builds this through the checkout's normal compiler budget. Its only
//! protocol is the real managed startup challenge followed by private test bytes.
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        io::{Read, Write},
        os::fd::{AsRawFd, FromRawFd},
        os::unix::net::UnixStream,
        time::Duration,
    };
    fn service(name: &str) -> Result<UnixStream, Box<dyn std::error::Error>> {
        let fd: i32 = std::env::var(name)?.parse()?;
        if fd < 3 {
            return Err("required enrolled descriptor overlaps standard streams".into());
        }
        // SAFETY: the launcher exclusively transfers this descriptor to us.
        Ok(unsafe { UnixStream::from_raw_fd(fd) })
    }
    let mut collab = service("TILO_SVC_COLLAB_FD")?;
    // svc_env_var currently uppercases the canonical service name verbatim.
    let mut model = service("TILO_SVC_MODEL-GATEWAY_FD")?;
    collab.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut challenge = [0; 32];
    collab.read_exact(&mut challenge)?;
    collab.write_all(&challenge)?;
    let mut admitted = [0];
    collab.read_exact(&mut admitted)?;
    if admitted != [1] {
        return Err("actual host admission must precede the fixture".into());
    }
    collab.set_read_timeout(None)?;

    // No threads have been created. After fork the child branches use only libc
    // and terminate with _exit; they never unwind or run inherited Rust drops.
    let intermediate = unsafe { libc::fork() };
    if intermediate < 0 {
        return Err("intermediate fork failed".into());
    }
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
            unsafe { libc::nanosleep(&raw const delay, std::ptr::null_mut()) };
        }
        if unsafe { libc::getppid() } != 1 {
            unsafe { libc::_exit(42) };
        }
        let fd = collab.as_raw_fd();
        // SO_PASSCRED on the host attaches this actual sender's outer PID.
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
    if unsafe { libc::waitpid(intermediate, &raw mut status, 0) } != intermediate {
        return Err("exact intermediate was not reaped".into());
    }
    if !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
        return Err("intermediate fixture did not exit successfully".into());
    }
    collab.write_all(b"P")?; // Intermediate was actually waited/reaped.
    let mut exit = [0];
    model.read_exact(&mut exit)?;
    if exit != *b"E" {
        return Err("private fixture exit command differs".into());
    }
    // The reparented descendant remains blocked in pause until the actual
    // sandbox teardown kills it.
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    panic!("managed-tree qualification fixture requires Linux");
}
