//! Shared helpers: whether the host can run a job, and running one to a
//! captured output.

use std::io::Read;
use std::os::fd::{FromRawFd, OwnedFd};

use tilo_sandbox::job::{JobSpec, Launch, Root, SpawnError};

/// Whether this host can create the job's namespaces at all. When it can't
/// (an old kernel, unprivileged user namespaces disabled, a CI container
/// that forbids them), a test prints why and passes: it cannot observe its
/// subject here.
pub fn can_run() -> Result<(), String> {
    let spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/true"]);
    match Launch::new(&spec).spawn() {
        Ok(mut job) => {
            let _ = job.wait();
            Ok(())
        }
        Err(SpawnError::Unsupported(why) | SpawnError::Setup(why)) => Err(why),
        Err(e) => Err(e.to_string()),
    }
}

/// Run `name`'s body only where a job can start.
#[macro_export]
macro_rules! needs_sandbox {
    () => {
        if let Err(why) = $crate::support::can_run() {
            eprintln!("skipped: no job sandbox here: {why}");
            return;
        }
    };
}

/// A pipe; its read end is a file, its write end goes to the job.
pub fn pipe() -> (std::fs::File, OwnedFd) {
    let mut fds = [0; 2];
    // SAFETY: pipe2 fills `fds` with two new owned descriptors.
    assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
    // SAFETY: the descriptors are new and ours.
    unsafe {
        (
            std::fs::File::from_raw_fd(fds[0]),
            OwnedFd::from_raw_fd(fds[1]),
        )
    }
}

/// Run a job built by `build`, returning its exit status and everything it
/// wrote to stdout and stderr.
pub fn run(build: impl FnOnce(&mut JobSpec)) -> (tilo_sandbox::job::ExitStatus, String) {
    let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/sh"]);
    build(&mut spec);
    let (mut out_r, out_w) = pipe();
    let err_w = out_w.try_clone().expect("clone the output pipe");
    let mut job = Launch::new(&spec)
        .stdout(out_w)
        .stderr(err_w)
        .spawn()
        .expect("start the job");
    let status = job.wait().expect("wait for the job");
    let mut output = String::new();
    out_r.read_to_string(&mut output).expect("read the output");
    (status, output)
}

/// A job that runs `script` with `/bin/sh -c`.
pub fn shell(spec: &mut JobSpec, script: &str) {
    spec.argv = ["/bin/sh", "-c", script].map(Into::into).to_vec();
}
