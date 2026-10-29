//! `tilolang run`: the package as an app in a nested Tilo session this tool starts.
//!
//! The session is `dist/dev/nested-session.sh` (one person) or `dist/dev/nested-pair.sh
//! --peers N` (N people in one community workspace) over the build in
//! `target/nested-session`, which carries the tilolang runtime binary beside the Tilo runtime.
//! The session bakes and signs the tilolang runtime bundle itself. This tool builds each package
//! into a `.tapp` naming the session's exact Tilo runtime, installs it into each session's state,
//! starts the sessions and opens the app through the session's own action
//! (`applications <id>`), so the launcher starts it: its own id, window, custody, record and
//! permissions, as on a real system.
//!
//! From then on it is `tilolang dev` over those sessions ([`crate::dev`]): every saved `.tl`
//! file reaches the running app, with its state.

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;

use tilo_files::native::Watcher;

use crate::dev::{Dev, Wake, ended, watch_into};
use crate::package::BuildOptions;
use crate::session::{self, Peer};

/// What `tilolang run` was asked for.
#[derive(Clone, Debug)]
pub struct RunOptions {
    pub dir: PathBuf,
    pub search: Vec<PathBuf>,
    /// How many sessions (people) share the workspace.
    pub peers: usize,
    pub headless: bool,
    /// Render with lavapipe only, never this machine's GPU (`nested-session.sh --software`).
    pub software: bool,
    /// The sessions' state (default: the tool's state directory).
    pub state: Option<PathBuf>,
    /// The Tilo checkout whose nested session runs the apps.
    pub repo: Option<PathBuf>,
}

/// Prepare each session's state (keys, the Tilo runtime bundle and the tilolang one) and
/// answer the runtimes they baked. One build serves them all, so they bake the same.
fn prepare(script: &std::path::Path, peers: &[Peer]) -> Result<(String, String), String> {
    let mut baked: Option<(String, String)> = None;
    for peer in peers {
        eprintln!("==> preparing {}", peer.state.display());
        session::status(
            Command::new(script)
                .args(["--no-build", "--prepare-only", "--apps", ""])
                .arg("--state")
                .arg(&peer.state),
            "preparing the nested session",
        )?;
        let runtimes = peer.runtimes_baked()?;
        if let Some(known) = &baked
            && known.0 != runtimes.0
        {
            return Err(format!(
                "the sessions baked different Tilo runtimes ({} and {})",
                known.0, runtimes.0
            ));
        }
        baked = Some(runtimes);
    }
    baked.ok_or_else(|| "no session to run".to_owned())
}

/// The command that starts the sessions: one nested session, or N in one community workspace.
fn session_command(
    repo: &std::path::Path,
    options: &RunOptions,
    base: &std::path::Path,
    ready: &std::path::Path,
    apps: &[crate::package::BuiltApp],
) -> Command {
    let mut command = if options.peers == 1 {
        let mut command = Command::new(repo.join(session::SESSION_SCRIPT));
        command.arg("--state").arg(base.join("a"));
        command
    } else {
        let mut command = Command::new(repo.join(session::PAIR_SCRIPT));
        command
            .arg("--peers")
            .arg(options.peers.to_string())
            .arg("--state")
            .arg(base);
        command
    };
    command
        .args(["--no-build", "--apps", ""])
        .arg("--ready")
        .arg(ready);
    for app in apps {
        command.arg("--install-tapp").arg(&app.tapp);
    }
    if options.headless {
        command.arg("--headless");
    }
    if options.software {
        command.arg("--software");
    }
    command
}

/// Have the session script stopped as an interrupt stops it when this process ends in a way
/// that runs none of its own code (a signal, a kill): the kernel sends it SIGTERM then, and its
/// cleanup stops what it started.
fn end_with_this_process(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    let parent = rustix::process::getpid();
    // SAFETY: runs in the forked child before exec, and calls only async-signal-safe system
    // calls (prctl, getppid), allocating nothing.
    unsafe {
        command.pre_exec(move || {
            rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::TERM))?;
            // This process may have ended before the request: then nobody is left to signal,
            // and the session must not start.
            if rustix::process::getppid() != Some(parent) {
                return Err(rustix::io::Errno::SRCH.into());
            }
            Ok(())
        });
    }
}

/// The session script this tool started, by a pidfd that names that process and no other.
///
/// Dropped, it stops the script as an interrupt does: the script's own cleanup stops every
/// process group it started (the shell, the apps, the services) and removes its runtime
/// directory, and this waits until it has. A session that already ended is left as it is.
struct Started {
    pidfd: std::os::fd::OwnedFd,
}

impl Started {
    fn new(child: &std::process::Child) -> Result<Self, String> {
        let pid = i32::try_from(child.id())
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .ok_or("the nested session has no process id")?;
        let pidfd = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())
            .map_err(|error| format!("watching the nested session: {error}"))?;
        Ok(Self { pidfd })
    }
}

impl Drop for Started {
    fn drop(&mut self) {
        use rustix::event::{PollFd, PollFlags, poll};
        let mut exited = [PollFd::new(&self.pidfd, PollFlags::IN)];
        // Readable once the process has exited.
        if poll(&mut exited, Some(&rustix::event::Timespec::default())).is_ok_and(|ready| ready > 0)
        {
            return;
        }
        eprintln!("==> stopping the nested session");
        if rustix::process::pidfd_send_signal(&self.pidfd, rustix::process::Signal::TERM).is_err() {
            return;
        }
        let mut exited = [PollFd::new(&self.pidfd, PollFlags::IN)];
        while poll(&mut exited, None) == Err(rustix::io::Errno::INTR) {}
    }
}

/// Wait until `path` exists, woken by inotify on its directory, or the session ends.
fn wait_for_file(
    path: &std::path::Path,
    wakes: &mpsc::Receiver<Wake>,
    sender: &mpsc::Sender<Wake>,
) -> Result<(), String> {
    let dir = path.parent().ok_or("a ready file has a directory")?;
    let _watch = watch_into(Watcher::entries(dir).presence(), sender.clone())
        .map_err(|error| format!("{}: {error}", dir.display()))?;
    loop {
        if path.exists() {
            break Ok(());
        }
        match wakes.recv() {
            Ok(Wake::File(_)) => {}
            Ok(Wake::Ended(status)) => {
                break Err(ended(status)
                    .err()
                    .unwrap_or_else(|| "the session ended before it was ready".to_owned()));
            }
            Err(_) => break Err("the file watcher stopped".to_owned()),
        }
    }
}

/// Run the package as an app until the session ends.
///
/// # Errors
/// There is no nested-session build, a package does not compile at the start, or the session
/// does not come up.
pub fn run(options: &RunOptions) -> Result<(), String> {
    let repo = session::repo_root(options.repo.as_deref(), &options.dir)?;
    let build_dir = session::build_dir(&repo);
    if !build_dir.join(session::RUNTIME_BINARY).is_file() {
        return Err(format!(
            "{} has no tilolang runtime; build the nested session with \
             `dist/dev/nested-session-build.sh {}`",
            build_dir.display(),
            build_dir.display()
        ));
    }
    let base = options
        .state
        .clone()
        .unwrap_or_else(|| crate::state_dir().join("sessions"));
    std::fs::create_dir_all(&base).map_err(|error| format!("{}: {error}", base.display()))?;
    let base = std::fs::canonicalize(&base).map_err(|error| error.to_string())?;
    let peers: Vec<Peer> = ('a'..='z')
        .take(options.peers)
        .map(|name| Peer::new(base.join(name.to_string()), &repo))
        .collect::<Result<_, _>>()?;
    let (tilo_runtime, tilolang_runtime) = prepare(&repo.join(session::SESSION_SCRIPT), &peers)?;
    let build = BuildOptions {
        dir: options.dir.clone(),
        search: options.search.clone(),
        out: base.join("build"),
        tilo_runtime,
        tilolang_runtime,
        version: "0.1.0".to_owned(),
    };
    let identity = crate::identity_at(&crate::state_dir().join("publisher.json"))?;
    let mut dev = Dev::new(build, identity, peers, build_dir.join("bin"));
    // The sessions install them as they start.
    let apps = dev.build_for_start()?;

    let ready = base.join("ready.json");
    let _ = std::fs::remove_file(&ready);
    let mut command = session_command(&repo, options, &base, &ready, &apps);
    end_with_this_process(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("starting the nested session: {error}"))?;
    // From here on, however this ends, the sessions end with it.
    let _session = match Started::new(&child) {
        Ok(session) => session,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let (sender, wakes) = mpsc::channel();
    let ended_sender = sender.clone();
    std::thread::spawn(move || {
        let _ = ended_sender.send(Wake::Ended(child.wait()));
    });
    wait_for_file(&ready, &wakes, &sender)?;
    dev.open()?;
    for peer in &dev.peers {
        eprintln!(
            "    drive it: TILO_TEST_CONTROL={} {} windows",
            peer.control.display(),
            dev.bin.join("tilo-control").display()
        );
    }
    eprintln!(
        "==> {} is open; a saved .tl file reaches it, with its state",
        dev.opened
    );
    dev.follow(&wakes, &sender)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run that ends, however it ends, stops the session it started and waits until it has.
    #[test]
    fn dropping_a_started_session_stops_it() {
        let mut child = Command::new("sleep")
            .arg("600")
            .spawn()
            .expect("a process stands in for the session");
        drop(Started::new(&child).expect("a pidfd"));
        let status = child
            .try_wait()
            .expect("its status")
            .expect("stopped before the drop returned");
        assert!(!status.success(), "{status}");
    }

    /// A session that already ended is left as it is.
    #[test]
    fn a_session_that_ended_is_not_signalled() {
        let mut child = Command::new("true").spawn().expect("a process");
        let started = Started::new(&child).expect("a pidfd");
        assert!(child.wait().expect("its status").success());
        drop(started);
    }
}
