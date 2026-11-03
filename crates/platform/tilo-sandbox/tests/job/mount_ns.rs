//! A job's mount namespace made from the one its caller names
//! ([`tilo_sandbox::job::Launch::mount_ns`]).

use std::io::Read;
use std::os::fd::OwnedFd;

use tilo_sandbox::job::{JobSpec, Launch, Root};

use crate::needs_sandbox;
use crate::support::{pipe, shell};

/// A mount namespace made on a thread of its own, with a tmpfs at `at`
/// holding `name`, which its caller's namespace doesn't have; or why there
/// is none (no right to make one here).
fn template(at: &std::path::Path, name: &str) -> Result<OwnedFd, String> {
    let (at, name) = (at.to_path_buf(), name.to_string());
    std::thread::spawn(move || -> Result<OwnedFd, String> {
        // SAFETY: the calls of this thread's own new mount namespace.
        unsafe {
            if libc::unshare(libc::CLONE_NEWNS) != 0 {
                return Err(format!("unshare: {}", std::io::Error::last_os_error()));
            }
            if libc::mount(
                c"none".as_ptr(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            ) != 0
            {
                return Err(format!(
                    "make / private: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let dir = std::ffi::CString::new(at.as_os_str().as_encoded_bytes()).unwrap();
            if libc::mount(
                c"tmpfs".as_ptr(),
                dir.as_ptr(),
                c"tmpfs".as_ptr(),
                0,
                std::ptr::null(),
            ) != 0
            {
                return Err(format!(
                    "mount a tmpfs: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        std::fs::write(at.join(&name), "here").map_err(|e| e.to_string())?;
        let ns = std::fs::File::open("/proc/thread-self/ns/mnt").map_err(|e| e.to_string())?;
        Ok(ns.into())
    })
    .join()
    .unwrap()
}

/// Whether a job, started from `ns` where one is given, sees `file`.
fn sees(file: &std::path::Path, ns: Option<&OwnedFd>) -> String {
    use std::os::fd::AsFd;
    let dir = file.parent().unwrap().to_path_buf();
    let mut spec = JobSpec::new(Root::Host { extra: vec![dir] }, ["/bin/sh"]);
    shell(
        &mut spec,
        &format!("test -e {} && echo sees || echo lacks", file.display()),
    );
    let (mut out_r, out_w) = pipe();
    let mut launch = Launch::new(&spec).stdout(out_w);
    if let Some(ns) = ns {
        launch = launch.mount_ns(ns.as_fd());
    }
    let mut job = launch.spawn().expect("start the job");
    job.wait().expect("wait for the job");
    let mut out = String::new();
    out_r.read_to_string(&mut out).unwrap();
    out.trim().to_string()
}

/// A job made from a given mount namespace sees its mounts, never the
/// caller's own: a tmpfs only the given namespace has is the job's, and a
/// job made from the caller's doesn't see it. The caller's thread is back in
/// its own namespace and directory once the job started.
#[test]
fn a_job_is_made_from_the_mount_namespace_it_is_given() {
    needs_sandbox!();
    let dir = tempfile::tempdir().unwrap();
    let at = dir.path().join("only-there");
    std::fs::create_dir(&at).unwrap();
    let ns = match template(&at, "file") {
        Ok(ns) => ns,
        Err(why) => {
            eprintln!("skipped: no mount namespace of the test's own here: {why}");
            return;
        }
    };
    let file = at.join("file");
    let before = std::fs::read_link("/proc/thread-self/ns/mnt").unwrap();
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(
        sees(&file, Some(&ns)),
        "sees",
        "the given namespace's tmpfs"
    );
    assert_eq!(
        std::fs::read_link("/proc/thread-self/ns/mnt").unwrap(),
        before,
        "back in its own"
    );
    assert_eq!(std::env::current_dir().unwrap(), cwd);
    assert!(!file.exists(), "the caller's namespace never had it");
    assert_eq!(sees(&file, None), "lacks", "made from the caller's");
}
