//! `tilolang run`: the package as an app in a nested Tilo session this tool starts.
//!
//! The session is `dist/dev/nested-session.sh` over the build in `target/nested-session`,
//! which carries the tilolang runtime binary beside the Tilo runtime.
//! The session bakes and signs the tilolang runtime bundle itself. This tool builds each package
//! into a `.tapp` naming the session's exact Tilo runtime, installs it into each session's state,
//! starts the sessions and opens the app through the session's own action
//! (`applications <id>`), so the launcher starts it: its own id, window, ownership, record and
//! permissions, as on a real system.
//!
//! From then on it is `tilolang dev` over those sessions ([`crate::dev`]): every saved `.tl`
//! file reaches the running app, with its state.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex, PoisonError, mpsc};

use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, waitid};

use tilo_files::native::Watcher;

use crate::dev::{Dev, Wake, ended, watch_into};
use crate::package::BuildOptions;
use crate::session::{self, Peer};

/// What `tilolang run` was asked for.
#[derive(Clone, Debug)]
pub struct RunOptions {
    pub dir: PathBuf,
    pub search: Vec<PathBuf>,
    pub headless: bool,
    /// Render with lavapipe only, never this machine's GPU (`nested-session.sh --software`).
    pub software: bool,
    pub state: Option<PathBuf>,
    pub repo: Option<PathBuf>,
}

/// Prepare each session's state (keys, the Tilo runtime
/// bundle and the tilolang one) and answer the runtimes they
/// baked.
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

fn session_command(
    repo: &std::path::Path,
    options: &RunOptions,
    base: &std::path::Path,
    ready: &std::path::Path,
    apps: &[crate::package::BuiltApp],
) -> Command {
    let mut command = Command::new(repo.join(session::SESSION_SCRIPT));
    command.arg("--state").arg(base.join("a"));
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

/// The session script this tool started. Dropping it stops the session, unless it has ended, and
/// waits until it has.
struct Started(Arc<Mutex<Child>>);

impl Started {
    /// Own `child`, and send `wakes` its end when it ends.
    fn watch(child: Child, wakes: mpsc::Sender<Wake>) -> Self {
        let pid = Pid::from_child(&child);
        let child = Arc::new(Mutex::new(child));
        let waiter = Arc::clone(&child);
        std::thread::spawn(move || {
            // Wait for the end without reaping it: the process id stays this child's until the
            // lock's holder has seen it end, so a stop never signals another process.
            let options = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT;
            while matches!(
                waitid(WaitId::Pid(pid), options),
                Err(rustix::io::Errno::INTR)
            ) {}
            let status = waiter.lock().unwrap_or_else(PoisonError::into_inner).wait();
            let _ = wakes.send(Wake::Ended(status));
        });
        Self(child)
    }
}

impl Drop for Started {
    fn drop(&mut self) {
        let mut child = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        eprintln!("==> stopping the nested session");
        let _ = rustix::process::kill_process(Pid::from_child(&child), Signal::TERM);
        let _ = child.wait();
    }
}

/// Wait until `path` exists, woken by inotify on its directory, or the session ends.
fn wait_for_file(
    path: &std::path::Path,
    wakes: &mpsc::Receiver<Wake>,
    sender: &mpsc::Sender<Wake>,
) -> Result<(), String> {
    let dir = path.parent().ok_or("a ready file has a directory")?;
    let _watch = watch_into(Watcher::entries(dir).presence(), sender.clone(), None)
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
    let peers = vec![Peer::new(base.join("a"), &repo)?];
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
    let mut dev = Dev::in_session(build, identity, peers, build_dir.join("bin"), &repo);
    let apps = dev.build_for_start()?;

    let (sender, wakes) = mpsc::channel();
    // Watched before the session opens the app, so that a save made while it starts reaches it.
    dev.start_watching(&sender)?;
    let ready = base.join("ready.json");
    let _ = std::fs::remove_file(&ready);
    let mut command = session_command(&repo, options, &base, &ready, &apps);
    let child = command
        .spawn()
        .map_err(|error| format!("starting the nested session: {error}"))?;
    let _session = Started::watch(child, sender.clone());
    wait_for_file(&ready, &wakes, &sender)?;
    dev.open()?;
    if let Some(bin) = dev.bin() {
        for peer in dev.peers() {
            eprintln!(
                "    drive it: TILO_TEST_CONTROL={} {} windows",
                peer.control.display(),
                bin.join("tilo-control").display()
            );
        }
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
    use tilo_test_env::SpawnUnderEnvLock as _;

    tilo_test_env::scenario! {
        fn a_started_session_is_stopped_with_its_run;

        phase dropping_a_started_session_stops_it;
        // A run that ends, however it ends, stops the session it started and waits until it has.
        {
            let child = Command::new("sleep")
                .arg("600")
                .spawn_locked()
                .expect("a process stands in for the session");
            let (sender, ends) = mpsc::channel();
            drop(Started::watch(child, sender));
            let Ok(Wake::Ended(status)) = ends.recv() else {
                panic!("the session's end");
            };
            let status = status.expect("its status");
            assert!(!status.success(), "{status}");
        }

        phase a_session_that_ended_is_not_signalled;
        {
            let child = Command::new("true").spawn_locked().expect("a process");
            let (sender, ends) = mpsc::channel();
            let started = Started::watch(child, sender);
            let Ok(Wake::Ended(status)) = ends.recv() else {
                panic!("its end");
            };
            assert!(status.expect("its status").success());
            drop(started);
        }
    }
}
