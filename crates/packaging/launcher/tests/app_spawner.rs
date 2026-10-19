//! The app spawner on a real cgroup2 hierarchy: launches are born in their
//! own cgroup's main leaf as children of the compositor, a whole app ends
//! with `cgroup.kill`, empty cgroups are removed, a job joins a leaf of its
//! own beside the app's, and a process in an app cgroup can neither move out
//! of it nor change its own limits.
//!
//! They need a cgroup this test may write below its own, as a delegated
//! subtree (a systemd scope of the person's, a `Delegate=yes` unit). Where
//! there is none (a container with a read-only cgroupfs) each says why it
//! skipped and passes, as the kernel cannot be observed there.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::Read as _;
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use launcher::app_cgroup::{self, AppKind};
use launcher::spawner::{DelegatedChild, Request, SpawnError, Spawner};

/// Set only in the probe child [`cgroup_escape_probe_child`] runs as.
const PROBE_ENV: &str = "TILO_APP_SPAWNER_ESCAPE_PROBE";
const PROBE_TEST: &str = "app_spawner::cgroup_escape_probe_child";

/// A scratch tree below this test's own cgroup, removed on drop.
struct Tree {
    dir: PathBuf,
    spawner: Option<Spawner>,
}

impl Tree {
    /// `None`, with the reason printed, where no cgroup can be made.
    fn new(test: u8) -> Option<Self> {
        let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok()?;
        let Some(own) = cgroup.lines().find_map(|line| line.strip_prefix("0::")) else {
            eprintln!("skipped: no unified cgroup hierarchy");
            return None;
        };
        // Named as a real tree is, so what runs in it reads as an app.
        let dir = app_cgroup::fs_path(own)
            .join(format!("tilo-apps-{}{test:02}.scope", std::process::id()));
        if let Err(error) = std::fs::create_dir(&dir) {
            eprintln!("skipped: no cgroup of this test's to write below: {error}");
            return None;
        }
        let spawner =
            match Spawner::start(Path::new(env!("CARGO_BIN_EXE_tilo-launcher")), Some(&dir)) {
                Ok(spawner) => spawner,
                Err(reason) => {
                    let _ = std::fs::remove_dir(&dir);
                    eprintln!("skipped: the spawner cannot adopt a scratch cgroup here: {reason}");
                    return None;
                }
            };
        assert_eq!(spawner.tree(), dir);
        Some(Self {
            dir,
            spawner: Some(spawner),
        })
    }

    fn spawner(&self) -> &Spawner {
        self.spawner.as_ref().unwrap()
    }

    fn spawn(&self, app_id: &str, argv: &[&str], env: &[(&str, &str)]) -> DelegatedChild {
        let request = Request {
            app_id: app_id.to_string(),
            kind: AppKind::Tilo,
            program: PathBuf::from(argv[0]),
            args: argv.iter().map(Into::into).collect(),
            // This process's environment, as a launcher gets the compositor's.
            env: std::env::vars_os()
                .chain(env.iter().map(|(k, v)| (k.into(), v.into())))
                .collect(),
            cwd: PathBuf::from("/"),
        };
        let stdin = OwnedFd::from(std::fs::File::open("/dev/null").unwrap());
        match self.spawner().spawn(&request, &stdin) {
            Ok(child) => child,
            Err(SpawnError::Unavailable(reason)) => panic!("not spawned: {reason}"),
            Err(SpawnError::Exec(error)) => panic!("not executed: {error}"),
        }
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        if let Some(spawner) = self.spawner.take() {
            spawner.shutdown();
        }
        let _ = app_cgroup::remove(&self.dir);
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn stdout_of(child: &mut DelegatedChild) -> String {
    let mut text = String::new();
    child
        .take_stdout()
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    text
}

#[test]
fn a_launch_is_born_in_its_own_cgroup_as_the_callers_child_and_its_cgroup_goes_with_it() {
    let Some(tree) = Tree::new(1) else {
        return;
    };
    // The spawner left the tree's root for its own leaf.
    let spawner_procs =
        std::fs::read_to_string(tree.dir.join(app_cgroup::SPAWNER_LEAF).join("cgroup.procs"))
            .unwrap();
    assert_eq!(spawner_procs.lines().count(), 1, "{spawner_procs}");
    assert_eq!(
        std::fs::read_to_string(tree.dir.join("cgroup.procs")).unwrap(),
        "",
        "no process in the tree's root"
    );

    let started = Instant::now();
    let mut child = tree.spawn(
        "org.example.Probe-x",
        &[
            "/bin/sh",
            "-c",
            "cat /proc/self/cgroup; echo \"ppid $PPID\"",
        ],
        &[],
    );
    let spawn_time = started.elapsed();
    let cgroup = child.cgroup().to_path_buf();
    assert_eq!(
        cgroup.parent(),
        Some(tree.dir.as_path()),
        "{}",
        cgroup.display()
    );
    assert_eq!(
        cgroup.file_name().and_then(|name| name.to_str()),
        Some("app-org.example.Probe\\x2dx-1")
    );
    let out = stdout_of(&mut child);
    let status = child.wait().unwrap();
    assert!(status.success(), "{status:?}: {out}");
    let mut lines = out.lines();
    let own = lines.next().unwrap();
    let main = cgroup.join(app_cgroup::MAIN_LEAF);
    assert!(
        own.starts_with("0::/") && main.ends_with(own.trim_start_matches("0::/")),
        "the child ran in {own}, not {}",
        main.display()
    );
    assert!(
        app_cgroup::app_cgroup_of(own).is_some(),
        "{own} is an app cgroup"
    );
    assert_eq!(
        lines.next(),
        Some(format!("ppid {}", std::process::id()).as_str()),
        "the caller, not the spawner, is the parent"
    );
    // Gone once it ended: nothing of the app is left behind.
    wait_until("the empty cgroup is removed", || !cgroup.exists());
    eprintln!("spawn through the spawner took {spawn_time:?}");
}

#[test]
fn killing_an_app_ends_what_left_its_process_tree() {
    let Some(tree) = Tree::new(2) else {
        return;
    };
    // A daemon in a session of its own, as Wine's prefix server is.
    let mut child = tree.spawn(
        "org.example.Daemon",
        &[
            "/bin/sh",
            "-c",
            "setsid /bin/sleep 600 </dev/null >/dev/null 2>&1 & echo $!; exec /bin/sleep 600",
        ],
        &[],
    );
    let cgroup = child.cgroup().to_path_buf();
    let mut stdout = child.take_stdout().unwrap();
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    while stdout.read(&mut byte).unwrap() == 1 && byte[0] != b'\n' {
        line.push(byte[0]);
    }
    let daemon: u32 = String::from_utf8(line).unwrap().trim().parse().unwrap();
    assert!(app_cgroup::populated(&cgroup).unwrap());
    app_cgroup::kill(&cgroup).unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
    let daemon_alive = || {
        std::fs::read_to_string(format!("/proc/{daemon}/stat")).is_ok_and(|stat| {
            !stat
                .rsplit_once(") ")
                .is_some_and(|(_, rest)| rest.starts_with('Z'))
        })
    };
    wait_until("the daemon in its own session is dead", || !daemon_alive());
    wait_until("the empty cgroup is removed", || !cgroup.exists());
}

#[test]
fn a_launch_that_cannot_be_executed_fails_and_leaves_nothing() {
    let Some(tree) = Tree::new(3) else {
        return;
    };
    let request = Request {
        app_id: "org.example.Missing".into(),
        kind: AppKind::Host,
        program: PathBuf::from("/nonexistent/tilo-launcher"),
        args: vec!["/nonexistent/tilo-launcher".into()],
        env: Vec::new(),
        cwd: PathBuf::from("/"),
    };
    let stdin = OwnedFd::from(std::fs::File::open("/dev/null").unwrap());
    match tree.spawner().spawn(&request, &stdin) {
        Err(SpawnError::Exec(error)) => {
            assert_eq!(error.raw_os_error(), Some(libc::ENOENT), "{error}");
        }
        other => panic!("expected an exec failure, got {other:?}"),
    }
    wait_until("the failed launch's cgroup is removed", || {
        !tree.dir.join("app-org.example.Missing-1").exists()
    });
}

/// A job joins a leaf of its own beside its app's main leaf: what
/// `runtime_portal::jobs` hands out is an open `cgroup.procs` the process
/// writes `0` to between fork and exec. Asked of the spawner on the app's
/// behalf, naming the app by one of its processes, as `ush` does.
#[test]
fn a_job_is_born_in_a_leaf_of_its_own_beside_its_app() {
    let Some(tree) = Tree::new(5) else {
        return;
    };
    let Some(runtime) = runtime_portal::jobs::runtime_dir() else {
        eprintln!("skipped: no XDG_RUNTIME_DIR for the job socket");
        return;
    };
    let mut app = tree.spawn("org.example.Editor", &["/bin/sleep", "600"], &[]);
    let socket = runtime_portal::jobs::socket_path(
        &runtime,
        tree.dir.file_name().unwrap().to_str().unwrap(),
    );
    let member = {
        use std::os::fd::FromRawFd as _;
        // SAFETY: pidfd_open returns a new descriptor or -1.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, app.id(), 0) };
        assert!(fd >= 0, "{}", std::io::Error::last_os_error());
        // SAFETY: the kernel just installed it for this process.
        unsafe { OwnedFd::from_raw_fd(i32::try_from(fd).unwrap()) }
    };
    let door = runtime_portal::jobs::connect(&socket).unwrap();
    let slot =
        runtime_portal::jobs::request(door.as_fd(), "rust-analyzer", Some(member.as_fd())).unwrap();
    assert_eq!(slot.oom_score_adj(), 100, "a job of the focused app");
    let mut command = std::process::Command::new("/bin/sh");
    command
        .args(["-c", "cat /proc/self/cgroup /proc/self/oom_score_adj"])
        .stdout(std::process::Stdio::piped());
    slot.place(&mut command);
    let out = command.output().unwrap();
    let out = String::from_utf8(out.stdout).unwrap();
    let mut lines = out.lines();
    let own = lines.next().unwrap();
    let job = app.cgroup().join("job-1");
    assert!(
        job.ends_with(own.trim_start_matches("0::/")),
        "the job ran in {own}, not {}",
        job.display()
    );
    // Raised to the job's value; a test host may have started it higher
    // already, and an unprivileged process cannot go below that.
    let adj: i32 = lines.next().unwrap().trim().parse().unwrap();
    assert!(adj >= 100, "oom_score_adj {adj}");
    if job.join("memory.oom.group").exists() {
        assert_eq!(
            std::fs::read_to_string(job.join("memory.oom.group"))
                .unwrap()
                .trim(),
            "1",
            "a job is its own OOM group"
        );
        assert_eq!(
            std::fs::read_to_string(
                app.cgroup()
                    .join(app_cgroup::MAIN_LEAF)
                    .join("memory.oom.group")
            )
            .unwrap()
            .trim(),
            "1",
            "a Tilo app is one OOM group"
        );
    }
    // A process outside the tree is no app to make jobs for.
    let stranger = runtime_portal::jobs::own_pidfd().unwrap();
    assert!(runtime_portal::jobs::request(door.as_fd(), "x", Some(stranger.as_fd())).is_err());
    app.kill().unwrap();
    app.wait().unwrap();
    let cgroup = app.cgroup().to_path_buf();
    wait_until("the app and its jobs are removed", || !cgroup.exists());
}

/// The kernel half of the boundary: even a process that could reach a
/// writable cgroupfs (no app can, see
/// `room_confinement::an_app_can_neither_see_nor_reach_the_cgroup_hierarchy`)
/// cannot move itself out of its app's cgroup namespace or change its own
/// cgroup's limits. bwrap gives every app that namespace, rooted at its app
/// cgroup; the probe makes one the same way.
#[test]
fn a_process_in_an_app_cgroup_cannot_move_or_retune_itself() {
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    if !mounts
        .lines()
        .any(|line| line.contains(" /sys/fs/cgroup cgroup2 ") && line.contains("nsdelegate"))
    {
        eprintln!("skipped: cgroup2 is not mounted with nsdelegate here");
        return;
    }
    let Some(tree) = Tree::new(4) else {
        return;
    };
    let mut victim = tree.spawn("org.example.Victim", &["/bin/sleep", "600"], &[]);
    let targets = [
        victim.cgroup().join(app_cgroup::MAIN_LEAF),
        tree.dir.join(app_cgroup::SPAWNER_LEAF),
        tree.dir.clone(),
    ]
    .map(|path| path.display().to_string())
    .join("\n");
    let exe = std::env::current_exe().unwrap();
    let exe = exe.to_str().unwrap();
    let mut probe = tree.spawn(
        "org.example.Probe",
        &[
            exe,
            "--exact",
            PROBE_TEST,
            "--nocapture",
            "--test-threads=1",
        ],
        &[(PROBE_ENV, &targets)],
    );
    let own = probe.cgroup().join(app_cgroup::MAIN_LEAF);
    let out = stdout_of(&mut probe);
    assert!(probe.wait().unwrap().success(), "{out}");
    let report = |name: &str| {
        out.lines()
            .find_map(|line| {
                line.split_once(&format!("probe {name} "))
                    .map(|(_, r)| r.to_string())
            })
            .unwrap_or_else(|| panic!("no {name} in {out}"))
    };
    if report("unshare") != "OK" {
        eprintln!(
            "skipped: no user and cgroup namespace here: {}",
            report("unshare")
        );
        victim.kill().unwrap();
        victim.wait().unwrap();
        return;
    }
    assert_eq!(report("cgroup"), "0::/", "rooted at its own cgroup");
    for escape in ["move-to-other-app", "move-to-spawner", "move-to-tree-root"] {
        assert_eq!(report(escape), "ENOENT", "{escape}: {out}");
    }
    for limit in ["own-max-descendants", "own-max-depth", "own-freeze"] {
        assert_eq!(report(limit), "EPERM", "{limit}: {out}");
    }
    // Every process it had stayed where it was born.
    assert_eq!(
        std::fs::read_to_string(tree.dir.join(app_cgroup::SPAWNER_LEAF).join("cgroup.procs"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(
            victim
                .cgroup()
                .join(app_cgroup::MAIN_LEAF)
                .join("cgroup.procs")
        )
        .unwrap()
        .lines()
        .count(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(own.join("cgroup.max.descendants"))
            .unwrap_or_default()
            .trim(),
        "max"
    );
    victim.kill().unwrap();
    victim.wait().unwrap();
}

/// The probe: run inside an app cgroup by the test above, it enters a user
/// and cgroup namespace as bwrap does for an app, then tries to leave its
/// cgroup and to change its own limits through the host's cgroupfs,
/// printing each answer. Without [`PROBE_ENV`] it is not the probe.
#[test]
fn cgroup_escape_probe_child() {
    let Ok(targets) = std::env::var(PROBE_ENV) else {
        return;
    };
    let mut targets = targets.lines().map(PathBuf::from);
    let (other_app, spawner, tree) = (
        targets.next().unwrap(),
        targets.next().unwrap(),
        targets.next().unwrap(),
    );
    let own_cgroup = std::fs::read_to_string("/proc/self/cgroup").unwrap();
    let own = app_cgroup::fs_path(
        own_cgroup
            .lines()
            .find_map(|l| l.strip_prefix("0::"))
            .unwrap(),
    );
    // A user namespace needs a single-threaded caller, which a test is not:
    // the namespaces are entered in a forked child, and a shell tries the
    // writes from there.
    let script = r#"
try() {
  if out=$( { printf %s "$3" > "$2"; } 2>&1 ); then echo "probe $1 OK"; return; fi
  case $out in
    *"No such file"*) echo "probe $1 ENOENT" ;;
    *"not permitted"*) echo "probe $1 EPERM" ;;
    *"denied"*) echo "probe $1 EACCES" ;;
    *) echo "probe $1 $out" ;;
  esac
}
echo "probe unshare OK"
echo "probe cgroup $(cat /proc/self/cgroup)"
try move-to-other-app "$1/cgroup.procs" 0
try move-to-spawner "$2/cgroup.procs" 0
try move-to-tree-root "$3/cgroup.procs" 0
try own-max-descendants "$4/cgroup.max.descendants" 1000
try own-max-depth "$4/cgroup.max.depth" 5
try own-freeze "$4/cgroup.freeze" 1
"#;
    let mut command = std::process::Command::new("/bin/sh");
    command
        .args(["-c", script, "probe"])
        .args([&other_app, &spawner, &tree, &own])
        .env("LC_ALL", "C");
    // SAFETY: one system call between fork and exec.
    unsafe {
        use std::os::unix::process::CommandExt as _;
        command.pre_exec(|| {
            if libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWCGROUP) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    match command.status() {
        Ok(status) => assert!(status.success(), "{status:?}"),
        Err(error) => println!("probe unshare {error}"),
    }
}
