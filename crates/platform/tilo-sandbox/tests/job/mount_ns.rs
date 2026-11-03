//! A job's mount namespace made from the one its caller names
//! ([`tilo_sandbox::job::Launch::mount_ns`]).

use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};

use tilo_sandbox::job::{JobSpec, Launch, Mount, Root};

use crate::needs_sandbox;
use crate::support::{pipe, shell};

/// Runs `f` on a thread of its own, in a mount namespace of its own with a
/// tmpfs at `at` holding `name`, and hands it `template`: the namespace that
/// thread was in before it mounted the tmpfs, which lacks it, as a worker's
/// template lacks the mounts of the jobs it started after making it. Or why
/// there is none (no right to make one here).
fn after_a_template<T: Send + 'static>(
    at: &Path,
    name: &str,
    f: impl FnOnce(&OwnedFd) -> T + Send + 'static,
) -> Result<T, String> {
    let (at, name) = (at.to_path_buf(), name.to_string());
    std::thread::spawn(move || -> Result<T, String> {
        let fail = |what: &str| format!("{what}: {}", std::io::Error::last_os_error());
        // SAFETY: this thread's own filesystem context and mount namespaces.
        unsafe {
            if libc::unshare(libc::CLONE_FS | libc::CLONE_NEWNS) != 0 {
                return Err(fail("unshare"));
            }
            if libc::mount(
                c"none".as_ptr(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            ) != 0
            {
                return Err(fail("make / private"));
            }
        }
        let template: OwnedFd = std::fs::File::open("/proc/thread-self/ns/mnt")
            .map_err(|e| e.to_string())?
            .into();
        let dir = std::ffi::CString::new(at.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: as above; a NUL-terminated path.
        unsafe {
            if libc::unshare(libc::CLONE_NEWNS) != 0 {
                return Err(fail("a second namespace"));
            }
            if libc::mount(
                c"tmpfs".as_ptr(),
                dir.as_ptr(),
                c"tmpfs".as_ptr(),
                0,
                std::ptr::null(),
            ) != 0
            {
                return Err(fail("mount a tmpfs"));
            }
        }
        std::fs::write(at.join(&name), "here").map_err(|e| e.to_string())?;
        Ok(f(&template))
    })
    .join()
    .unwrap()
}

/// What a job with `spec`, started from `ns` where one is given, prints.
fn output(spec: &JobSpec, ns: Option<&OwnedFd>) -> String {
    let (mut out_r, out_w) = pipe();
    let mut launch = Launch::new(spec).stdout(out_w);
    if let Some(ns) = ns {
        launch = launch.mount_ns(ns.as_fd());
    }
    let mut job = launch.spawn().expect("start the job");
    job.wait().expect("wait for the job");
    let mut out = String::new();
    out_r.read_to_string(&mut out).unwrap();
    out.trim().to_string()
}

/// A job that says whether it sees `file`, bound from its directory.
fn sees(file: &Path) -> JobSpec {
    let dir = file.parent().unwrap().to_path_buf();
    let mut spec = JobSpec::new(Root::Host { extra: vec![dir] }, ["/bin/sh"]);
    shell(
        &mut spec,
        &format!("test -e {} && echo sees || echo lacks", file.display()),
    );
    spec
}

/// A job made from another mount namespace sees what one made from its
/// caller's sees: a tmpfs its caller mounted after that namespace was made
/// is the job's, bound as a tree from the caller's (not idmapped: the job
/// has no ids of its own), never looked up in the namespace, which lacks it.
/// The caller's thread is back in its own namespace and directory once the
/// job started.
#[test]
fn a_job_made_from_another_namespace_binds_its_callers_mounts() {
    needs_sandbox!();
    let dir = tempfile::tempdir().unwrap();
    let at = dir.path().join("only-here");
    std::fs::create_dir(&at).unwrap();
    let file = at.join("file");
    let ran = after_a_template(&at, "file", move |template| {
        let before = std::fs::read_link("/proc/thread-self/ns/mnt").unwrap();
        let cwd = std::env::current_dir().unwrap();
        let from_template = output(&sees(&file), Some(template));
        let back = std::fs::read_link("/proc/thread-self/ns/mnt").unwrap() == before
            && std::env::current_dir().unwrap() == cwd;
        (from_template, output(&sees(&file), None), back)
    });
    let (from_template, from_callers, back) = match ran {
        Ok(r) => r,
        Err(why) => {
            eprintln!("skipped: no mount namespace of the test's own here: {why}");
            return;
        }
    };
    assert_eq!(from_callers, "sees", "made from the caller's");
    assert_eq!(
        from_template, "sees",
        "the caller's tmpfs, made from the template"
    );
    assert!(back, "back in its own namespace and directory");
    assert!(
        !dir.path().join("only-here/file").exists(),
        "the test's namespace never had it"
    );
}

/// An overlay's layer that is a mount its caller made after the namespace
/// the job is made from is the job's as well: its layers are trees taken
/// from the caller's too.
#[test]
fn a_job_made_from_another_namespace_overlays_its_callers_mounts() {
    needs_sandbox!();
    let dir = tempfile::tempdir().unwrap();
    let at = dir.path().join("lower");
    std::fs::create_dir(&at).unwrap();
    let writes = |name: &str| -> (PathBuf, PathBuf) {
        let (upper, work) = (
            dir.path().join(name).join("upper"),
            dir.path().join(name).join("work"),
        );
        std::fs::create_dir_all(&upper).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        (upper, work)
    };
    // Overlays need user extended attributes under the temporary directory.
    let probe_lower = dir.path().join("probe-lower");
    std::fs::create_dir(&probe_lower).unwrap();
    let (upper, work) = writes("probe");
    let mut probe = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/true"]);
    probe.mount(Mount::overlay(vec![probe_lower], &upper, &work, "/o"));
    match Launch::new(&probe)
        .spawn()
        .map_err(|e| e.to_string())
        .and_then(|mut j| j.wait().map_err(|e| e.to_string()))
    {
        Ok(s) if s.success() => {}
        other => {
            eprintln!(
                "skipped: no overlays under {}: {other:?}",
                dir.path().display()
            );
            return;
        }
    }
    let (upper, work) = writes("job");
    let lower = at.clone();
    let ran = after_a_template(&at, "file", move |template| {
        let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/sh"]);
        spec.mount(Mount::overlay(vec![lower], &upper, &work, "/o"));
        shell(&mut spec, "cat /o/file 2>/dev/null || echo lacks");
        output(&spec, Some(template))
    });
    match ran {
        Ok(out) => assert_eq!(out, "here", "the caller's tmpfs as the overlay's layer"),
        Err(why) => eprintln!("skipped: no mount namespace of the test's own here: {why}"),
    }
}
