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

use notify::{RecursiveMode, Watcher as _};

use crate::dev::{Dev, Wake, ended};
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

/// Wait until `path` exists, woken by inotify on its directory, or the session ends.
fn wait_for_file(
    path: &std::path::Path,
    wakes: &mpsc::Receiver<Wake>,
    watcher: &mut notify::RecommendedWatcher,
) -> Result<(), String> {
    let dir = path.parent().ok_or("a ready file has a directory")?;
    watcher
        .watch(dir, RecursiveMode::NonRecursive)
        .map_err(|error| format!("{}: {error}", dir.display()))?;
    let result = loop {
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
    };
    let _ = watcher.unwatch(dir);
    result
}

/// Run the package as an app until the session ends.
///
/// # Errors
/// There is no nested-session build, a package does not compile at the start, or the session
/// does not come up.
pub fn run(options: &RunOptions) -> Result<(), String> {
    let repo = session::repo_root(options.repo.as_deref(), &options.dir)?;
    let build_dir = repo.join(session::BUILD_DIR);
    if !build_dir.join(session::RUNTIME_BINARY).is_file() {
        return Err(format!(
            "{} has no tilolang runtime; build the nested session with \
             `dist/dev/nested-session-build.sh {}`",
            build_dir.display(),
            session::BUILD_DIR
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
        .map(|name| Peer::new(base.join(name.to_string())))
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
    let mut child = session_command(&repo, options, &base, &ready, &apps)
        .spawn()
        .map_err(|error| format!("starting the nested session: {error}"))?;
    let (sender, wakes) = mpsc::channel();
    let file_sender = sender.clone();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = file_sender.send(Wake::File(event));
    })
    .map_err(|error| error.to_string())?;
    std::thread::spawn(move || {
        let _ = sender.send(Wake::Ended(child.wait()));
    });
    wait_for_file(&ready, &wakes, &mut watcher)?;
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
    dev.follow(&wakes, &mut watcher)
}
