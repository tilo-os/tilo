//! `tilolang run`: the package as an app in a locally built nested Tilo session.
//!
//! The session is `dist/dev/nested-session.sh` (one person) or `dist/dev/nested-pair.sh
//! --peers N` (N people in one community workspace) over the build in
//! `target/nested-session`, which carries the tilolang runtime binary beside the Tilo runtime.
//! The session bakes and signs the tilolang runtime bundle itself. This tool builds each package
//! into a `.tapp` naming the session's exact Tilo runtime, hands the session the `.tapp`s to
//! install, and opens the app through the session's own action (`applications <id>`), so the
//! launcher starts it: its own id, window, custody, record and permissions, as on a real system.
//!
//! On every `.tl` edit (inotify), the packages are rebuilt and reinstalled with
//! `tilo-install --force`, and the app's window is closed and opened again on the new build. No
//! cargo build runs: the runtime is unchanged.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;

use notify::{RecursiveMode, Watcher as _};

use tilo_app_package::pack::SigningIdentity;

use crate::package::{self, BuildOptions, BuiltApp};

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

const SESSION_SCRIPT: &str = "dist/dev/nested-session.sh";
const PAIR_SCRIPT: &str = "dist/dev/nested-pair.sh";
const BUILD_DIR: &str = "target/nested-session";
/// The thin binary the session bakes into the tilolang runtime bundle.
const RUNTIME_BINARY: &str = "thin-app/tilolang-app";

/// The Tilo checkout: `--repo`, `$TILO_REPO`, or the first directory above the package or the
/// current directory that has the nested session.
fn repo_root(options: &RunOptions) -> Result<PathBuf, String> {
    if let Some(repo) = options
        .repo
        .clone()
        .or_else(|| std::env::var_os("TILO_REPO").map(PathBuf::from))
    {
        return if repo.join(SESSION_SCRIPT).is_file() {
            Ok(repo)
        } else {
            Err(format!("{}: no {SESSION_SCRIPT} there", repo.display()))
        };
    }
    let starts = [
        std::fs::canonicalize(&options.dir).ok(),
        std::env::current_dir().ok(),
    ];
    starts
        .into_iter()
        .flatten()
        .find_map(|start| {
            start
                .ancestors()
                .find(|dir| dir.join(SESSION_SCRIPT).is_file())
                .map(Path::to_path_buf)
        })
        .ok_or_else(|| {
            "no Tilo checkout above the package; name one with --repo or $TILO_REPO".to_owned()
        })
}

/// The control socket nested-session.sh gives the session whose state is `state`.
fn control_socket(state: &Path) -> Result<PathBuf, String> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or("XDG_RUNTIME_DIR is unset")?;
    let digest = sha256_hex(state.as_os_str().as_encoded_bytes());
    let short: String = digest.chars().take(12).collect();
    Ok(runtime
        .join(format!("tilo-nested-{short}"))
        .join("control.sock"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// One session the run drives.
struct Peer {
    state: PathBuf,
    control: PathBuf,
}

impl Peer {
    fn new(state: PathBuf) -> Result<Self, String> {
        Ok(Self {
            control: control_socket(&state)?,
            state,
        })
    }

    /// The environment `tilo-install` and `tilo-control` act on this session in: the one
    /// nested-session.sh gives its shell.
    fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        let state = &self.state;
        command
            .env("HOME", state.join("home"))
            .env("XDG_CONFIG_HOME", state.join("config"))
            .env("XDG_DATA_HOME", state.join("data"))
            .env("XDG_STATE_HOME", state.join("state"))
            .env("XDG_CACHE_HOME", state.join("cache"))
            .env("TILO_IDENTITY_ROOT", state.join("identity"))
            .env("TILO_APPS_DATA_DIR", state.join("apps"))
            .env("TILO_RUNTIME_BUNDLE_DIR", state.join("runtimes"))
            .env(
                "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE",
                state.join("keys/runtime-publishers.txt"),
            )
            .env("TILO_TEST_CONTROL", &self.control)
            .env_remove("TILO_APPS_SYSTEM_DIR");
        if let Some(runtime) = self.control.parent() {
            command.env("XDG_RUNTIME_DIR", runtime);
        }
        command
    }
}

fn status(command: &mut Command, what: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("{what}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{what} failed ({status})"))
    }
}

fn output(command: &mut Command, what: &str) -> Result<String, String> {
    let output = command
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| format!("{what}: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!("{what} failed ({})", output.status))
    }
}

fn read_trimmed(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map(|text| text.trim().to_owned())
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// What wakes the run: a file event, or the session's end.
enum Wake {
    File(notify::Result<notify::Event>),
    Ended(std::io::Result<std::process::ExitStatus>),
}

fn ended(status: std::io::Result<std::process::ExitStatus>) -> Result<(), String> {
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("the nested session ended ({status})")),
        Err(error) => Err(error.to_string()),
    }
}

/// Wait until `path` exists, woken by inotify on its directory, or the session ends.
fn wait_for_file(
    path: &Path,
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

/// Open (or close) `app` in `peer` through the session's own action.
fn toggle(peer: &Peer, tilo_control: &Path, app: &str) -> Result<(), String> {
    output(
        peer.command(tilo_control)
            .args(["actions", "invoke", "applications", app]),
        &format!("opening {app}"),
    )
    .map(drop)
}

/// Whether a window of `app` is open in `peer`.
fn is_open(peer: &Peer, tilo_control: &Path, app: &str) -> bool {
    output(peer.command(tilo_control).arg("windows"), "listing windows")
        .is_ok_and(|windows| windows.contains(&format!("\"{app}\"")))
}

fn install(peer: &Peer, bin: &Path, apps: &[BuiltApp]) -> Result<(), String> {
    for app in apps {
        status(
            peer.command(&bin.join("tilo-install"))
                .arg("install")
                .arg(&app.tapp)
                .args(["--allow-self-signed", "--force"])
                .stdout(Stdio::null()),
            &format!("installing {}", app.app),
        )?;
    }
    Ok(())
}

/// The package directories a build reads: the ones to watch.
fn watched_dirs(options: &RunOptions) -> Result<BTreeSet<PathBuf>, String> {
    Ok(
        tilolang::source::dependency_order(&options.dir, &options.search)?
            .into_iter()
            .collect(),
    )
}

/// Prepare each session's state (keys, the Tilo runtime bundle and the tilolang one) and
/// answer the runtimes they baked. One build serves them all, so they bake the same.
fn prepare(script: &Path, peers: &[Peer]) -> Result<(String, String), String> {
    let mut tilo_runtime: Option<String> = None;
    for peer in peers {
        eprintln!("==> preparing {}", peer.state.display());
        status(
            Command::new(script)
                .args(["--no-build", "--prepare-only", "--apps", ""])
                .arg("--state")
                .arg(&peer.state),
            "preparing the nested session",
        )?;
        let version = read_trimmed(&peer.state.join("runtimes/.version"))?;
        if let Some(known) = &tilo_runtime
            && known != &version
        {
            return Err(format!(
                "the sessions baked different Tilo runtimes ({known} and {version})"
            ));
        }
        tilo_runtime = Some(version);
    }
    let first = peers.first().ok_or("no session to run")?;
    let tilolang_runtime = read_trimmed(&first.state.join("runtimes/.tilolang-version"))?;
    Ok((
        format!("tilo-runtime/{}", tilo_runtime.unwrap_or_default()),
        format!("tilolang/{tilolang_runtime}"),
    ))
}

/// The command that starts the sessions with `apps` installed: one nested session, or N in one
/// community workspace.
fn session_command(
    repo: &Path,
    options: &RunOptions,
    base: &Path,
    ready: &Path,
    apps: &[BuiltApp],
) -> Command {
    let mut command = if options.peers == 1 {
        let mut command = Command::new(repo.join(SESSION_SCRIPT));
        command.arg("--state").arg(base.join("a"));
        command
    } else {
        let mut command = Command::new(repo.join(PAIR_SCRIPT));
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

/// The rebuild loop's fixed parts.
struct Session<'a> {
    options: &'a RunOptions,
    build: BuildOptions,
    identity: SigningIdentity,
    peers: Vec<Peer>,
    bin: PathBuf,
    /// The app the run opens: the package, built last after what it uses.
    opened: String,
}

impl Session<'_> {
    fn tilo_control(&self) -> PathBuf {
        self.bin.join("tilo-control")
    }

    /// Rebuild, reinstall everywhere, and open the app again on the new build.
    fn rebuild(&self) {
        let apps = match package::build(&self.build, &self.identity) {
            Ok(apps) => apps,
            Err(error) => {
                eprintln!("{error}");
                return;
            }
        };
        let tilo_control = self.tilo_control();
        for peer in &self.peers {
            if let Err(error) = install(peer, &self.bin, &apps) {
                eprintln!("{error}");
                continue;
            }
            let reopened = if is_open(peer, &tilo_control, &self.opened) {
                toggle(peer, &tilo_control, &self.opened)
                    .and_then(|()| toggle(peer, &tilo_control, &self.opened))
            } else {
                toggle(peer, &tilo_control, &self.opened)
            };
            if let Err(error) = reopened {
                eprintln!("{error}");
            }
        }
        eprintln!("==> rebuilt and reinstalled {}", self.opened);
    }

    /// Watch every package directory the build reads that is not watched yet.
    fn watch(
        &self,
        watcher: &mut notify::RecommendedWatcher,
        watching: &mut BTreeSet<PathBuf>,
    ) -> Result<(), String> {
        for dir in watched_dirs(self.options)? {
            if watching.insert(dir.clone()) {
                watcher
                    .watch(&dir, RecursiveMode::NonRecursive)
                    .map_err(|error| format!("{}: {error}", dir.display()))?;
            }
        }
        Ok(())
    }

    /// Rebuild on every edit until the session ends.
    fn follow(
        &self,
        wakes: &mpsc::Receiver<Wake>,
        watcher: &mut notify::RecommendedWatcher,
    ) -> Result<(), String> {
        let mut watching = BTreeSet::new();
        self.watch(watcher, &mut watching)?;
        loop {
            let event = match wakes.recv() {
                Ok(Wake::File(event)) => event,
                Ok(Wake::Ended(status)) => return ended(status),
                Err(_) => return Err("the file watcher stopped".to_owned()),
            };
            if !event.is_ok_and(|event| is_source_edit(&event)) {
                continue;
            }
            // An editor's save is several events: take every one already queued with this one.
            while let Ok(wake) = wakes.try_recv() {
                if let Wake::Ended(status) = wake {
                    return ended(status);
                }
            }
            self.rebuild();
            if let Err(error) = self.watch(watcher, &mut watching) {
                eprintln!("{error}");
            }
        }
    }
}

/// Run the package as an app until the session ends.
///
/// # Errors
/// There is no nested-session build, a package does not compile at the start, or the session
/// does not come up.
pub fn run(options: &RunOptions) -> Result<(), String> {
    let repo = repo_root(options)?;
    let build_dir = repo.join(BUILD_DIR);
    if !build_dir.join(RUNTIME_BINARY).is_file() {
        return Err(format!(
            "{} has no tilolang runtime; build the nested session with \
             `dist/dev/nested-session-build.sh {BUILD_DIR}`",
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
        .map(|name| Peer::new(base.join(name.to_string())))
        .collect::<Result<_, _>>()?;
    let (tilo_runtime, tilolang_runtime) = prepare(&repo.join(SESSION_SCRIPT), &peers)?;
    let build = BuildOptions {
        dir: options.dir.clone(),
        search: options.search.clone(),
        out: base.join("build"),
        tilo_runtime,
        tilolang_runtime,
        version: "0.1.0".to_owned(),
    };
    let identity = crate::identity_at(&crate::state_dir().join("publisher.json"))?;
    let apps = package::build(&build, &identity)?;
    let opened = apps
        .last()
        .map(|app| app.app.clone())
        .ok_or("the build made no app")?;
    let session = Session {
        options,
        build,
        identity,
        peers,
        bin: build_dir.join("bin"),
        opened,
    };

    // The sessions, with the apps installed as they start.
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
    let tilo_control = session.tilo_control();
    for peer in &session.peers {
        toggle(peer, &tilo_control, &session.opened)?;
        eprintln!(
            "    drive it: TILO_TEST_CONTROL={} {} windows",
            peer.control.display(),
            tilo_control.display()
        );
    }
    eprintln!(
        "==> {} is open; editing a .tl file rebuilds and reinstalls it",
        session.opened
    );
    session.follow(&wakes, &mut watcher)
}

/// Whether `event` changed a package's source or ids lock.
fn is_source_edit(event: &notify::Event) -> bool {
    use notify::EventKind;
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    ) && event
        .paths
        .iter()
        .any(|path| path.extension().is_some_and(|extension| extension == "tl"))
}
