//! `tilolang dev`: edit a package and see the running app change, with its state.
//!
//! The package is developed inside a Tilo session: one named with `--session` (its ready file,
//! runtime directory, control socket or state directory), or the one the tool starts when none is
//! named, as `tilolang run` starts it. The app runs as this device's development build of it:
//! the tool stages each build as the app's manifest and payload, and the session's daemon marks
//! it, packs it, signs it with the device's own key and runs it under the id its marking derives,
//! apart from any installed app and its grants. The tool reads no publisher key and signs
//! nothing; the daemon's connectors answer for the origins the app's types hold.
//!
//! The tool watches the package's sources (and those of the packages it uses). On every save it
//! compiles in-process; a compile error is printed and the app keeps running the last good
//! build. A save is anything that changes what a build reads: a `.tl` file, a translation in the
//! package's `locales/`, an entry added or removed anywhere below the package's directory. The
//! packages are watched as trees, and each directory's own directory with them, so a package or
//! a build's output directory made, replaced or moved in is a change too. The session follows
//! reinstalls of the apps its tilolang hosts run
//! (`crates/core/tilo-runtime/src/device/processes.rs`), verifies the new installation as a
//! launch would, and hands its module to the host running the app, whose instances take it over
//! in place or start over, with their data.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tilo_files::ChangeKind;
use tilo_files::native::{ChangeWatch, WatchEvent, Watcher};
use tilolang::PackageMeta;
use tilolang_tilo::upgrade::{Upgrade, compare};

use crate::package::{self, BuildOptions};
use crate::session::{self, Peer};

/// The sources of the tool itself: the checkout's answers, which can be newer than the tool.
const TOOL_SOURCES: [&str; 4] = [
    "crates/lang/tilolang/src",
    "crates/lang/tilolang-runtime/src",
    "crates/lang/tilolang-tilo/src",
    "crates/lang/tilolang-process/src",
];

/// How long the loop gathers the events of one change before it acts on
/// them: an editor's save, and a build that writes a file in steps,
/// arrive as several events, and one build is one rebuild or one
/// restart.
const SETTLE: Duration = Duration::from_millis(60);

/// The most one change waits for the events to stop arriving: a build writing a large payload
/// keeps waking the loop, and the save must still be delivered.
const SETTLE_LIMIT: Duration = Duration::from_millis(500);

/// What `tilolang dev` was asked for.
#[derive(Clone, Debug)]
pub struct DevOptions {
    pub dir: PathBuf,
    pub search: Vec<PathBuf>,
    /// A running nested session to develop in: its ready file, runtime directory or control socket.
    /// Without one the tool starts a session, as `tilolang run` does.
    pub session: Option<PathBuf>,
    /// The session the tool starts runs without a window, and renders with lavapipe only: what a
    /// headless check drives.
    pub headless: bool,
    pub software: bool,
    /// The state directory the session the tool starts keeps: its identity, apps and workspace.
    pub state: Option<PathBuf>,
    pub repo: Option<PathBuf>,
}

/// What wakes the loop: a file event, or the end of the session the tool started.
pub(crate) enum Wake {
    File(Vec<WatchEvent>),
    Ended(std::io::Result<std::process::ExitStatus>),
}

pub(crate) fn ended(status: std::io::Result<std::process::ExitStatus>) -> Result<(), String> {
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("the nested session ended ({status})")),
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) struct Dev {
    pub(crate) build: BuildOptions,
    peers: Vec<Peer>,
    bin: PathBuf,
    /// The app's own id, as its sources name it.
    pub(crate) opened: String,
    /// The id each session's daemon installed the app under, in the sessions' order: a
    /// development build is its own on each device, and only that device names it.
    pub(crate) installed: Vec<String>,
    delivered: HashMap<String, (String, PackageMeta)>,
    /// The checkout the tool works in, when there is one: whose sources being newer than the tool
    /// mean the tool cannot know all the types they declare.
    repo: Option<PathBuf>,
    sources: BTreeSet<PathBuf>,
    watching: BTreeMap<PathBuf, ChangeWatch>,
}

impl Dev {
    pub(crate) fn new(
        build: BuildOptions,
        peers: Vec<Peer>,
        bin: PathBuf,
        repo: Option<&Path>,
    ) -> Self {
        Self {
            build,
            peers,
            bin,
            opened: String::new(),
            installed: Vec::new(),
            delivered: HashMap::new(),
            repo: repo.map(Path::to_path_buf),
            sources: BTreeSet::new(),
            watching: BTreeMap::new(),
        }
    }

    pub(crate) fn peers(&self) -> &[Peer] {
        &self.peers
    }

    /// The session's tools: `tilo-install` and `tilo-runtime` (a build, and its start), and
    /// `tilo-control` for the shell's windows.
    pub(crate) fn bin(&self) -> &Path {
        &self.bin
    }

    /// Stage the builds a session installs as it starts, before its daemon runs; the builds.
    pub(crate) fn stage_for_start(&mut self) -> Result<Vec<package::StagedDev>, String> {
        let staged = package::stage_dev(&self.build)?;
        if let Some(last) = staged.last() {
            self.opened.clone_from(&last.app);
        }
        Ok(staged)
    }

    /// Install `staged` on every session of the delivery: each build whose sources changed goes
    /// to the daemon as this device's development build, which names the id it runs under.
    pub(crate) fn install_staged(
        &mut self,
        staged: &[package::StagedDev],
        saved: Instant,
    ) -> Result<(), String> {
        let built = saved.elapsed();
        for app in staged {
            let previous = self.delivered.get(&app.app);
            if previous.is_some_and(|(digest, _)| *digest == app.digest) {
                continue;
            }
            let plan = previous.map(|(_, meta)| compare(meta, &app.meta));
            let mut installed = Vec::with_capacity(self.peers.len());
            for peer in &self.peers {
                installed.push(peer.install_dev(&self.bin, app)?);
            }
            self.installed = installed;
            report(&app.title, plan, built, saved.elapsed(), "installed");
            self.delivered
                .insert(app.app.clone(), (app.digest.clone(), app.meta.clone()));
        }
        Ok(())
    }

    /// Build, and take what changed to what runs it; the builds staged, or the compile errors.
    fn build_and_deliver(&mut self, saved: Instant) -> Result<Vec<package::StagedDev>, String> {
        let staged = package::stage_dev(&self.build)?;
        self.install_staged(&staged, saved)?;
        if let Some(last) = staged.last() {
            self.opened.clone_from(&last.app);
        }
        Ok(staged)
    }

    /// Ask the session's daemon to start the app it installed, as a person's window starts.
    pub(crate) fn open(&self) -> Result<(), String> {
        for (peer, app) in self.peers.iter().zip(&self.installed) {
            peer.start_app(&self.bin, app)?;
        }
        Ok(())
    }

    /// One save: build, install, and say what the running app does with it.
    fn rebuild(&mut self) {
        let saved = Instant::now();
        if let Err(error) = self.build_and_deliver(saved) {
            eprintln!("{error}");
            if let Some(hint) = self.build_failure_hint(&error) {
                eprintln!("==> {hint}");
            }
            eprintln!("==> the app keeps running the last good build");
        }
    }

    /// What a build's errors most likely mean beyond the errors themselves: a type the checkout's
    /// sources declare, which this build of the tool is too old to know, is answered by rebuilding
    /// the tool, not by editing the package.
    fn build_failure_hint(&self, error: &str) -> Option<&'static str> {
        (error.contains("E013") && self.older_than_sources())
            .then_some("the tilolang CLI is older than the sources: rebuild it and start dev again")
    }

    /// Whether the checkout's sources of the tool are newer than the tool itself.
    fn older_than_sources(&self) -> bool {
        let Some(repo) = &self.repo else {
            return false;
        };
        let Ok(built) = std::env::current_exe()
            .and_then(std::fs::metadata)
            .and_then(|meta| meta.modified())
        else {
            return false;
        };
        TOOL_SOURCES
            .iter()
            .filter_map(|dir| newest_source(&repo.join(dir)))
            .any(|newest| newest > built)
    }

    fn watched_dirs(&self) -> Result<BTreeSet<PathBuf>, String> {
        Ok(crate::package_dirs(&self.build.dir, &self.build.search)?
            .into_iter()
            .collect())
    }

    /// Watch what a build reads, so that every change of it wakes the loop.
    fn watch(&mut self, wakes: &mpsc::Sender<Wake>) -> Result<(), String> {
        self.sources = self.watched_dirs()?;
        // The package's own directory, and the one above it,
        // where the package itself may be made, replaced or moved
        // in.
        let mut dirs: Vec<(PathBuf, bool)> = Vec::new();
        for dir in &self.sources {
            dirs.push((dir.clone(), true));
            dirs.extend(dir.parent().map(|above| (above.to_path_buf(), false)));
        }
        // A watch of a directory that went away ends: it is taken again below when the directory is
        // back, and until then what is below it is not watched.
        self.watching.retain(|dir, _| dir.is_dir());
        for (dir, tree) in dirs {
            if !dir.is_dir() {
                continue;
            }
            if let std::collections::btree_map::Entry::Vacant(slot) = self.watching.entry(dir) {
                let watcher = if tree {
                    Watcher::tree(slot.key())
                } else {
                    Watcher::entries(slot.key())
                };
                let watch = watch_into(watcher, wakes.clone())
                    .map_err(|error| format!("{}: {error}", slot.key().display()))?;
                slot.insert(watch);
            }
        }
        Ok(())
    }

    /// Take the watches before the app is started, so that a save made while it starts is
    /// delivered rather than made before anything was watching for it.
    pub(crate) fn start_watching(&mut self, sender: &mpsc::Sender<Wake>) -> Result<(), String> {
        self.watch(sender)
    }

    pub(crate) fn follow(
        &mut self,
        wakes: &mpsc::Receiver<Wake>,
        sender: &mpsc::Sender<Wake>,
    ) -> Result<(), String> {
        self.watch(sender)?;
        loop {
            let events = match wakes.recv() {
                Ok(Wake::File(events)) => events,
                Ok(Wake::Ended(status)) => return ended(status),
                Err(_) => return Err("the file watcher stopped".to_owned()),
            };
            let events = match Self::gather(wakes, events)? {
                Next::Changed(events) => events,
                Next::Ended(status) => return ended(status),
            };
            self.watching.retain(|dir, _| !ends_watch(dir, &events));
            self.handle(&events);
            if let Err(error) = self.watch(sender) {
                eprintln!("{error}");
            }
        }
    }

    /// Take the events of one change: these, and every event that arrives before the change has
    /// been quiet for [`SETTLE`], or for [`SETTLE_LIMIT`] at most.
    fn gather(wakes: &mpsc::Receiver<Wake>, mut events: Vec<WatchEvent>) -> Result<Next, String> {
        let started = Instant::now();
        loop {
            match wakes.recv_timeout(SETTLE) {
                Ok(Wake::File(more)) => {
                    events.extend(more);
                    if started.elapsed() >= SETTLE_LIMIT {
                        return Ok(Next::Changed(events));
                    }
                }
                Ok(Wake::Ended(status)) => return Ok(Next::Ended(status)),
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(Next::Changed(events)),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("the file watcher stopped".to_owned());
                }
            }
        }
    }

    /// What a batch of file events asks for: a build.
    fn handle(&mut self, events: &[WatchEvent]) {
        if events.iter().any(|event| self.is_source_edit(event)) {
            self.rebuild();
        }
    }

    /// Whether `event` changed what a build reads: anything in a package's directory, wherever it
    /// is, or changes in one that may have been missed.
    fn is_source_edit(&self, event: &WatchEvent) -> bool {
        if event.kind == ChangeKind::Rescan {
            // A rescan of a package's directory, or of the
            // directory above it, may have hidden a change of the
            // package.
            return self
                .sources
                .iter()
                .any(|dir| event.path == *dir || dir.starts_with(&event.path));
        }
        self.sources.iter().any(|dir| event.path.starts_with(dir)) && !is_ids_lock(&event.path)
    }
}

fn report(title: &str, plan: Option<Upgrade>, built: Duration, delivered: Duration, how: &str) {
    match plan {
        None => eprintln!(
            "==> {title}: built in {} ms, {how} in {} ms",
            built.as_millis(),
            delivered.as_millis()
        ),
        Some(Upgrade::InPlace) => eprintln!(
            "==> {title}: built in {} ms, {how} in {} ms; the running app takes it with its state",
            built.as_millis(),
            delivered.as_millis()
        ),
        Some(Upgrade::Restart(reason)) => eprintln!(
            "==> {title}: {reason}; {how} in {} ms, the running app starts over on it with its data",
            delivered.as_millis()
        ),
    }
}

enum Next {
    Changed(Vec<WatchEvent>),
    Ended(std::io::Result<std::process::ExitStatus>),
}

/// Whether `events` ends the watch of `dir`: the directory is gone, or the tool is asked to
/// rescan it and what it finds decides.
fn ends_watch(dir: &Path, events: &[WatchEvent]) -> bool {
    events
        .iter()
        .any(|event| event.kind == ChangeKind::Removed && event.path == dir)
}

/// Whether `path` is a package's ids lock, which a build
/// writes itself: its change is the loop's own and not a
/// save.
fn is_ids_lock(path: &Path) -> bool {
    path.file_name() == Some(std::ffi::OsStr::new(tilolang::source::IDS_FILE))
}

/// When the newest file below `dir` was written.
fn newest_source(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest = None;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(entry.path()),
                Ok(_) => {
                    if let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) {
                        newest = newest.max(Some(modified));
                    }
                }
                Err(_) => {}
            }
        }
    }
    newest
}

/// Start `watcher`, its changes sent to `wakes`.
pub(crate) fn watch_into(
    watcher: Watcher,
    wakes: mpsc::Sender<Wake>,
) -> std::io::Result<ChangeWatch> {
    watcher.start(move |events| {
        let _ = wakes.send(Wake::File(events.to_vec()));
    })
}

/// Develop the package in the session `options.session` names, or in the one the tool starts when
/// there is none, until interrupted.
pub fn dev(options: &DevOptions) -> Result<(), String> {
    // One command does not start a session each time: a session this computer runs is the one
    // the package is developed in, and one the tool starts is left running for the next `dev`.
    let Some(session) = &options.session else {
        let repo = session::repo_root(options.repo.as_deref(), &options.dir, "tilolang dev")?;
        let state = options
            .state
            .clone()
            .unwrap_or_else(|| crate::state_dir().join("sessions").join("dev"));
        let ready = state.join("ready.json");
        let (sender, wakes) = mpsc::channel();
        if ready.is_file() && Peer::attach(&ready, &repo).is_ok() {
            eprintln!("==> attaching to the session at {}", state.display());
            return dev_in_session(options, &repo, &ready, &sender, &wakes);
        }
        if let Some(running) = session::newest_running() {
            eprintln!(
                "==> attaching to the running session at {}",
                running.state.display()
            );
            return dev_in_session(options, &repo, &running.state, &sender, &wakes);
        }
        eprintln!(
            "==> starting a session at {} (it stays, and the next dev attaches to it)",
            state.display()
        );
        session::start_detached(&repo, &state, &ready, options.headless, options.software)?;
        return dev_in_session(options, &repo, &ready, &sender, &wakes);
    };
    let repo = session::repo_root(
        options.repo.as_deref(),
        &options.dir,
        "tilolang dev --session",
    )?;
    let (sender, wakes) = mpsc::channel();
    dev_in_session(options, &repo, session, &sender, &wakes)
}

fn dev_in_session(
    options: &DevOptions,
    repo: &Path,
    session: &Path,
    sender: &mpsc::Sender<Wake>,
    wakes: &mpsc::Receiver<Wake>,
) -> Result<(), String> {
    let bin = session::tools(repo)?;
    let peer = Peer::attach(session, repo)?;
    let (tilo_runtime, tilolang_runtime) = peer.runtimes_baked()?;
    let build = BuildOptions {
        dir: options.dir.clone(),
        search: options.search.clone(),
        out: dev_build_dir(&peer.state, &options.dir)?,
        tilo_runtime,
        tilolang_runtime,
        version: "0.1.0".to_owned(),
        locked: false,
    };
    let mut dev = Dev::new(build, vec![peer], bin, Some(repo));
    dev.build_and_deliver(Instant::now())?;
    // Watched before anything of the app runs, so that a save made while it starts is delivered.
    dev.start_watching(sender)?;
    dev.open()?;
    eprintln!(
        "==> {} is open; a saved .tl file reaches it",
        dev.installed.last().unwrap_or(&dev.opened)
    );
    dev.follow(wakes, sender)
}

/// Where a developed package's builds are staged: below `state`, a
/// directory of its own per package, so several developed at once (each with the packages it uses,
/// which they may share) never write over each other's builds.
fn dev_build_dir(state: &Path, dir: &Path) -> Result<PathBuf, String> {
    use sha2::Digest as _;
    let package =
        std::fs::canonicalize(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let digest = sha2::Sha256::digest(package.as_os_str().as_encoded_bytes());
    let short = hex::encode(digest.get(..6).unwrap_or_default());
    let name = package
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(state.join("dev-build").join(format!("{name}-{short}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_dev_loop_knows_its_builds_and_its_watches;

        phase each_developed_package_builds_in_its_own_directory;
        // Two packages developed at once build apart; the
        // same package always builds where it did before.
        {
            let checkout = tempfile::tempdir().unwrap();
            let (notes, tasks) = (checkout.path().join("notes"), checkout.path().join("tasks"));
            std::fs::create_dir_all(&notes).unwrap();
            std::fs::create_dir_all(&tasks).unwrap();
            let state = Path::new("/session/state");
            let one = dev_build_dir(state, &notes).unwrap();
            assert_ne!(one, dev_build_dir(state, &tasks).unwrap());
            assert_eq!(one, dev_build_dir(state, &notes.join(".")).unwrap());
            assert!(
                one.starts_with(state.join("dev-build")),
                "{}",
                one.display()
            );
        }

        phase a_builds_ids_lock_is_no_save;
        // A build's ids lock is written by the tool itself: a save it is not.
        {
            assert!(is_ids_lock(
                &Path::new("/app/notes").join(tilolang::source::IDS_FILE)
            ));
            assert!(!is_ids_lock(Path::new("/app/notes/app.tl")));
            assert!(!is_ids_lock(Path::new("/app/notes/locales/de-DE.ftl")));
        }
    }
}
