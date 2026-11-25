//! `tilolang run`: the package as an app in a nested Tilo session this tool starts.
//!
//! The session is `dist/dev/nested-session.sh` over the build in `target/nested-session`,
//! which carries the tilolang runtime binary beside the Tilo runtime.
//! The session bakes and signs the tilolang runtime bundle itself. This tool stages each package
//! as the session's own Tilo runtime names it, starts the session, and hands the builds to the
//! daemon that runs beside its desktop: the device installs each as its own development build and
//! starts it, so the launcher runs it as on a real system, under its own id, window, ownership,
//! record and permissions.
//!
//! From then on it is `tilolang dev` over those sessions ([`crate::dev`]): every saved `.tl`
//! file reaches the running app, with its state.

use std::io::PipeWriter;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Instant;

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
) -> Command {
    let mut command = Command::new(repo.join(session::SESSION_SCRIPT));
    command.arg("--state").arg(base.join("a"));
    command
        .args(["--no-build", "--until-stdin-closes", "--apps", ""])
        .arg("--ready")
        .arg(ready);
    if options.headless {
        command.arg("--headless");
    }
    if options.software {
        command.arg("--software");
    }
    command
}

/// Make the stdin of the process `command` starts a pipe whose write end only the answer holds:
/// it closes when the answer drops, and when this tool ends however it ends (the kernel closes it
/// then), and the process ends when it reads that.
pub(crate) fn lifeline(command: &mut Command) -> std::io::Result<PipeWriter> {
    let (stdin, lifeline) = std::io::pipe()?;
    command.stdin(stdin);
    Ok(lifeline)
}

/// The session script this tool started on its [`lifeline`]. Dropping it ends the session and
/// waits until it has.
struct Started {
    lifeline: Option<PipeWriter>,
    waiter: Option<JoinHandle<()>>,
}

impl Started {
    /// Own `child`, and send `wakes` its end when it ends.
    fn watch(mut child: Child, lifeline: PipeWriter, wakes: mpsc::Sender<Wake>) -> Self {
        let waiter = std::thread::spawn(move || {
            let _ = wakes.send(Wake::Ended(child.wait()));
        });
        Self {
            lifeline: Some(lifeline),
            waiter: Some(waiter),
        }
    }
}

impl Drop for Started {
    fn drop(&mut self) {
        drop(self.lifeline.take());
        if let Some(waiter) = self.waiter.take() {
            if !waiter.is_finished() {
                eprintln!("==> stopping the nested session");
            }
            let _ = waiter.join();
        }
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

pub fn run(options: &RunOptions) -> Result<(), String> {
    run_named(options, "tilolang run")
}

/// [`run`], as the command `command` named it: a tool that starts a session for `dev` says so.
pub(crate) fn run_named(options: &RunOptions, command: &str) -> Result<(), String> {
    let repo = session::repo_root(options.repo.as_deref(), &options.dir, command)?;
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
        locked: false,
    };
    let mut dev = Dev::new(build, peers, build_dir.join("bin"), Some(&repo));
    // The session's daemon runs after it starts, so its builds are staged here and installed
    // once it answers: the daemon signs each as this device's development build.
    let staged = dev.stage_for_start()?;

    let (sender, wakes) = mpsc::channel();
    // Watched before the session opens the app, so that a save made while it starts reaches it.
    dev.start_watching(&sender)?;
    let ready = base.join("ready.json");
    let _ = std::fs::remove_file(&ready);
    let mut command = session_command(&repo, options, &base, &ready);
    let starting = |error| format!("starting the nested session: {error}");
    let lifeline = lifeline(&mut command).map_err(starting)?;
    let child = command.spawn().map_err(starting)?;
    let _session = Started::watch(child, lifeline, sender.clone());
    wait_for_file(&ready, &wakes, &sender)?;
    dev.install_staged(&staged, Instant::now())?;
    dev.open()?;
    let bin = dev.bin();
    for peer in dev.peers() {
        eprintln!(
            "    drive it: TILO_TEST_CONTROL={} {} windows",
            peer.control.display(),
            bin.join("tilo-control").display()
        );
    }
    eprintln!(
        "==> {} is open; a saved .tl file reaches it, with its state",
        dev.installed.last().unwrap_or(&dev.opened)
    );
    dev.follow(&wakes, &sender)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_test_env::SpawnUnderEnvLock as _;

    tilo_test_env::scenario! {
        fn a_started_session_is_stopped_with_its_run;

        phase dropping_a_started_session_ends_it;
        // A run that ends, however it ends, ends the session it started and waits until it has:
        // `cat` plays a session that ends when its stdin closes.
        {
            let mut command = Command::new("cat");
            let lifeline = lifeline(&mut command).expect("a pipe");
            let child = command.spawn_locked().expect("a session");
            let (sender, ends) = mpsc::channel();
            drop(Started::watch(child, lifeline, sender));
            let Ok(Wake::Ended(status)) = ends.try_recv() else {
                panic!("ended before the drop returned");
            };
            assert!(status.expect("its status").success());
        }

        phase a_killed_tool_ends_its_session;
        // The kernel closes a killed tool's end: a `sleep` playing the tool holds the only one.
        {
            let mut session = Command::new("cat");
            let lifeline = lifeline(&mut session).expect("a pipe");
            let mut session = session.spawn_locked().expect("a session");
            let mut tool = Command::new("sleep")
                .arg("600")
                .stdout(lifeline)
                .spawn_locked()
                .expect("a tool");
            tool.kill().expect("SIGKILL");
            let _ = tool.wait();
            assert!(session.wait().expect("its status").success());
        }
    }
}
