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

/// A cgroup of the test's own below this process's, removed (its processes killed) when
/// dropped; or why there is none (no delegated cgroup here).
pub struct TestCgroup {
    pub path: std::path::PathBuf,
    dir: std::fs::File,
}

impl TestCgroup {
    pub fn new() -> Result<Self, String> {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let mount = std::path::Path::new("/sys/fs/cgroup");
        let own = tilo_sandbox::job::cgroup::cgroup_of("self", mount).map_err(|e| e.to_string())?;
        let path = own.join(format!(
            "tilo-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let dir = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self { path, dir })
    }

    pub fn fd(&self) -> std::os::fd::BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.dir.as_fd()
    }

    pub fn procs(&self) -> Vec<i32> {
        std::fs::read_to_string(self.path.join("cgroup.procs"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    }

    pub fn kill(&self) {
        std::fs::write(self.path.join("cgroup.kill"), "1").expect("cgroup.kill");
    }
}

impl Drop for TestCgroup {
    fn drop(&mut self) {
        let _ = std::fs::write(self.path.join("cgroup.kill"), "1");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::fs::read_to_string(self.path.join("cgroup.events"))
            .is_ok_and(|e| e.contains("populated 1"))
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = std::fs::remove_dir(&self.path);
    }
}

/// Whether this process is root, which helpers and host ids need.
pub fn is_root() -> bool {
    rustix::process::geteuid().is_root()
}

/// Where [`start_in_child`] tells the starter what to start.
const STARTER_ENV: &str = "TILO_SANDBOX_TEST_STARTER";

/// Start what `dir` describes (see [`starter`]) from another process, a child of this one, on
/// a thread that ends at once, and return the pid of what it started. A detached job's or a
/// helper's init is its starter's parent's child: this process's, which reaps it with
/// [`reap`].
pub fn start_in_child(dir: &std::path::Path) -> i32 {
    let exe = std::env::current_exe().expect("this test binary");
    let status = std::process::Command::new(exe)
        .args([
            "--exact",
            "support::starter",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(STARTER_ENV, dir)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run the starter");
    let error = std::fs::read_to_string(dir.join("error")).unwrap_or_default();
    assert!(
        status.success() && error.is_empty(),
        "the starter failed: {error}"
    );
    std::fs::read_to_string(dir.join("pid"))
        .expect("the starter's pid file")
        .trim()
        .parse()
        .expect("a pid")
}

/// Wait for this process's child `pid` to exit: its raw wait status.
pub fn reap(pid: i32) -> i32 {
    let mut st = 0;
    // SAFETY: waits on a child of ours into a stack variable.
    let r = unsafe { libc::waitpid(pid, &raw mut st, 0) };
    assert_eq!(r, pid, "waitpid: {}", std::io::Error::last_os_error());
    st
}

/// The 4-byte wait status in `path`, if it holds one.
pub fn status_file(path: &std::path::Path) -> Option<i32> {
    let b = std::fs::read(path).ok()?;
    <[u8; 4]>::try_from(b.as_slice())
        .ok()
        .map(i32::from_ne_bytes)
}

/// Not a test of its own: run by [`start_in_child`] in a process of its own. `dir` holds
/// `kind` (`job`, `helper`, `helper-probe` or `helper-netns`), `script` (for `/bin/sh -c`) and
/// optionally `cgroup` (a path); the started job's status goes to `status`, its output to
/// `out`, its pid to `pid`.
#[test]
fn starter() {
    let Some(dir) = std::env::var_os(STARTER_ENV) else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let on = dir.clone();
    // From a thread that ends right after: what it starts must not depend on it.
    let started = std::thread::spawn(move || start(&on))
        .join()
        .unwrap_or_else(|_| Err("the starting thread panicked".into()));
    match started {
        Ok(pid) => std::fs::write(dir.join("pid"), pid.to_string()).unwrap(),
        Err(e) => std::fs::write(dir.join("error"), e).unwrap(),
    }
}

fn start(dir: &std::path::Path) -> Result<i32, String> {
    use std::os::fd::AsFd;
    use tilo_sandbox::job::helper::{self, Helper};
    let read = |name: &str| std::fs::read_to_string(dir.join(name));
    let kind = read("kind").map_err(|e| e.to_string())?;
    let script = read("script").map_err(|e| e.to_string())?;
    let cgroup = match read("cgroup") {
        Ok(p) => Some(std::fs::File::open(p.trim()).map_err(|e| e.to_string())?),
        Err(_) => None,
    };
    let create = |name: &str| -> Result<OwnedFd, String> {
        Ok(std::fs::File::create(dir.join(name))
            .map_err(|e| e.to_string())?
            .into())
    };
    let status = create("status")?;
    let out = create("out")?;
    let err = out.try_clone().map_err(|e| e.to_string())?;
    let argv: Vec<std::ffi::OsString> = ["/bin/sh", "-c", script.as_str()].map(Into::into).to_vec();
    if kind == "job" {
        let spec = JobSpec::new(Root::Host { extra: vec![] }, argv);
        let mut launch = Launch::new(&spec).stdout(out).stderr(err).detached(status);
        if let Some(cg) = &cgroup {
            launch = launch.cgroup_fd(cg.as_fd());
        }
        let job = launch.spawn().map_err(|e| e.to_string())?;
        return Ok(job.into_detached().0);
    }
    let cgroup = cgroup.ok_or("a helper needs a cgroup")?;
    if kind == "helper-probe" {
        probe_own_mount(dir)?;
    }
    let netns = if kind == "helper-netns" {
        Some(own_netns(dir)?)
    } else {
        None
    };
    helper::spawn(Helper {
        argv,
        env: vec![("PATH".into(), tilo_sandbox::job::spec::DEFAULT_PATH.into())],
        binds: vec![],
        devices: vec![],
        cgroup: cgroup.as_fd(),
        netns: netns.as_ref().map(AsFd::as_fd),
        status,
        stdout: Some(out),
        stderr: Some(err),
        nofile_max: false,
    })
    .map_err(|e| e.to_string())
}

/// Mount a tmpfs of the caller's own, in a mount namespace of this thread's, which a helper
/// started from here must not see; whether this thread sees it goes to `probe-seen`.
fn probe_own_mount(dir: &std::path::Path) -> Result<(), String> {
    // SAFETY (this block): namespace and mount calls on static strings, on this thread.
    unsafe {
        if libc::unshare(libc::CLONE_NEWNS) != 0
            || libc::mount(
                c"none".as_ptr(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            ) != 0
        {
            return Err(format!(
                "a mount namespace: {}",
                std::io::Error::last_os_error()
            ));
        }
        let _ = std::fs::create_dir_all("/tmp/t-probe");
        if libc::mount(
            c"tmpfs".as_ptr(),
            c"/tmp/t-probe".as_ptr(),
            c"tmpfs".as_ptr(),
            0,
            std::ptr::null(),
        ) != 0
        {
            return Err(format!(
                "the probe tmpfs: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    let seen = std::fs::read_to_string("/proc/thread-self/mountinfo").unwrap_or_default();
    std::fs::write(
        dir.join("probe-seen"),
        seen.contains(" /tmp/t-probe ").to_string(),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// A network namespace made on this thread, which returns to its own; its name goes to
/// `netns`.
fn own_netns(dir: &std::path::Path) -> Result<std::fs::File, String> {
    let home = std::fs::File::open("/proc/thread-self/ns/net").map_err(|e| e.to_string())?;
    // SAFETY: unshare and setns on this thread, with a namespace descriptor we hold.
    unsafe {
        if libc::unshare(libc::CLONE_NEWNET) != 0 {
            return Err(format!(
                "a network namespace: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    let made = std::fs::File::open("/proc/thread-self/ns/net").map_err(|e| e.to_string())?;
    let link = std::fs::read_link("/proc/thread-self/ns/net").map_err(|e| e.to_string())?;
    std::fs::write(dir.join("netns"), link.to_string_lossy().as_bytes())
        .map_err(|e| e.to_string())?;
    // SAFETY: back to where this thread was, by a descriptor we hold.
    if unsafe { libc::setns(std::os::fd::AsRawFd::as_raw_fd(&home), libc::CLONE_NEWNET) } != 0 {
        return Err(format!(
            "back to our network: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(made)
}
