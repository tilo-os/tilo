//! `tilolang dev`: edit a package and see the running app change, with its state.
//!
//! The tool watches the package's sources (and those of the packages it uses). On every save it
//! compiles in-process; a compile error is printed and the app keeps running the last good
//! build. A good build is signed into a `.tapp` and installed with `tilo-install`, exactly as any
//! install: verified, recorded, its old version kept while something runs it. What happens to the
//! running app is the session's: a development session's launcher follows reinstalls of a module
//! payload's package (`crates/packaging/launcher/src/live_module.rs`), verifies the new
//! installation as a launch would, and hands its payload to the running `tilolang-app`, which
//! takes the new module over on the same store and windows. When the new build cannot read the
//! state the old one wrote ([`tilolang_tilo::upgrade`]), the tool says why in one line and the app
//! starts over on the new build with its data, as its next start would.
//!
//! With `--runtime BIN` the apps run on a tilolang runtime binary of the developer's own build
//! instead of the session's: the tool bakes it into a signed runtime bundle, installs that as a
//! runtime package (`tilolang/<version>+dev`) with `tilo-install`, builds the apps naming it, and
//! restarts their windows on it. Each later change of the binary re-bakes and reinstalls the
//! bundle and restarts the apps that use it; the session keeps running.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Instant;

use notify::{RecursiveMode, Watcher as _};
use tilo_app_package::pack::SigningIdentity;
use tilolang::PackageMeta;
use tilolang_tilo::upgrade::{Upgrade, compare};

use crate::package::{self, BuildOptions, BuiltApp};
use crate::session::{self, Peer};

/// What `tilolang dev` was asked for.
#[derive(Clone, Debug)]
pub struct DevOptions {
    pub dir: PathBuf,
    pub search: Vec<PathBuf>,
    /// The running nested session to develop in: its ready file, runtime directory or control
    /// socket.
    pub session: PathBuf,
    /// A tilolang runtime binary (a build of `apps/tilolang`) to run the apps on.
    pub runtime: Option<PathBuf>,
    /// The key the runtime bundle is signed with (default: the session's runtime publisher).
    pub runtime_identity: Option<PathBuf>,
    /// The Tilo checkout whose nested-session build has the session's tools.
    pub repo: Option<PathBuf>,
}

/// What wakes the loop: a file event, or the end of the session the tool started.
pub(crate) enum Wake {
    File(notify::Result<notify::Event>),
    Ended(std::io::Result<std::process::ExitStatus>),
}

pub(crate) fn ended(status: std::io::Result<std::process::ExitStatus>) -> Result<(), String> {
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("the nested session ended ({status})")),
        Err(error) => Err(error.to_string()),
    }
}

/// The version of the runtime bundle `--runtime` makes: the tool's tilolang runtime version
/// with `+dev` for its build.
fn dev_runtime_version() -> String {
    format!("{}+dev", env!("CARGO_PKG_VERSION"))
}

/// The package id of the development runtime: its own, so installing it never touches the
/// tilolang runtime package a system has.
const DEV_RUNTIME_PACKAGE: &str = "so.tilo.runtime.tilolang.dev";

/// A tilolang runtime binary the apps run on, baked and installed as a runtime package.
struct DevRuntime {
    binary: PathBuf,
    /// The key its bundle is signed with: one the launcher trusts for runtime bundles.
    identity_file: PathBuf,
    version: String,
}

/// The development loop over one or more sessions.
pub(crate) struct Dev {
    pub(crate) build: BuildOptions,
    pub(crate) identity: SigningIdentity,
    pub(crate) peers: Vec<Peer>,
    /// The session's tools: `tilo-install`, `tilo-control`, `tilo-runtime-bundle`.
    pub(crate) bin: PathBuf,
    /// The app the tool opens: the package, built last after what it uses.
    pub(crate) opened: String,
    /// What each app installed last is: its digest and metadata.
    installed: HashMap<String, (String, PackageMeta)>,
    runtime: Option<DevRuntime>,
}

impl Dev {
    pub(crate) fn new(
        build: BuildOptions,
        identity: SigningIdentity,
        peers: Vec<Peer>,
        bin: PathBuf,
    ) -> Self {
        Self {
            build,
            identity,
            peers,
            bin,
            opened: String::new(),
            installed: HashMap::new(),
            runtime: None,
        }
    }

    /// Build, as a session that installs the apps as it starts will have them installed; the
    /// apps, whose `.tapp`s that session installs.
    pub(crate) fn build_for_start(&mut self) -> Result<Vec<BuiltApp>, String> {
        let apps = package::build(&self.build, &self.identity)?;
        for app in &apps {
            self.installed
                .insert(app.app.clone(), (app.digest.clone(), app.meta.clone()));
        }
        if let Some(last) = apps.last() {
            self.opened.clone_from(&last.app);
        }
        Ok(apps)
    }

    /// Build, and install what changed into every session; the apps built, or the compile
    /// errors.
    pub(crate) fn build_and_install(&mut self, saved: Instant) -> Result<Vec<BuiltApp>, String> {
        let apps = package::build(&self.build, &self.identity)?;
        let built = saved.elapsed();
        for app in &apps {
            let previous = self.installed.get(&app.app);
            if previous.is_some_and(|(digest, _)| *digest == app.digest) {
                continue;
            }
            let plan = previous.map(|(_, meta)| compare(meta, &app.meta));
            for peer in &self.peers {
                peer.install(&self.bin, &app.tapp)?;
            }
            let installed = saved.elapsed();
            match plan {
                None => eprintln!(
                    "==> {}: built in {} ms, installed in {} ms",
                    app.title,
                    built.as_millis(),
                    installed.as_millis()
                ),
                Some(Upgrade::InPlace) => eprintln!(
                    "==> {}: built in {} ms, installed in {} ms; the running app takes it with \
                     its state",
                    app.title,
                    built.as_millis(),
                    installed.as_millis()
                ),
                Some(Upgrade::Restart(reason)) => eprintln!(
                    "==> {}: {reason}; installed in {} ms, the running app starts over on it \
                     with its data",
                    app.title,
                    installed.as_millis()
                ),
            }
            self.installed
                .insert(app.app.clone(), (app.digest.clone(), app.meta.clone()));
        }
        if let Some(last) = apps.last() {
            self.opened.clone_from(&last.app);
        }
        Ok(apps)
    }

    /// Open the app in every session where it has no window yet.
    pub(crate) fn open(&self) -> Result<(), String> {
        for peer in &self.peers {
            if !peer.is_open(&self.bin, &self.opened) {
                peer.open(&self.bin, &self.opened)?;
            }
        }
        Ok(())
    }

    /// One save: build, install, and say what the running app does with it.
    fn rebuild(&mut self) {
        let saved = Instant::now();
        if let Err(error) = self.build_and_install(saved) {
            eprintln!("{error}");
            eprintln!("==> the app keeps running the last good build");
        }
    }

    /// Bake the runtime binary into a signed bundle and install it as the development runtime
    /// package; the apps name it from now on.
    fn install_runtime(&mut self) -> Result<(), String> {
        let Some(runtime) = &self.runtime else {
            return Ok(());
        };
        let started = Instant::now();
        let work = self.build.out.join("runtime");
        let bundle = work.join("bundle");
        if bundle.exists() {
            std::fs::remove_dir_all(&bundle)
                .map_err(|error| format!("{}: {error}", bundle.display()))?;
        }
        session::status(
            Command::new(self.bin.join("tilo-runtime-bundle"))
                .args(["build", "--version", &runtime.version, "--out"])
                .arg(&bundle)
                .arg("--include-bin")
                .arg(&runtime.binary)
                .arg("--identity")
                .arg(&runtime.identity_file)
                .stdout(std::process::Stdio::null()),
            "baking the runtime bundle",
        )?;
        let baked = started.elapsed();
        let tapp = runtime_package(&bundle, &runtime.version, &self.identity, &work)?;
        for peer in &self.peers {
            peer.install(&self.bin, &tapp)?;
        }
        eprintln!(
            "==> runtime tilolang/{}: baked in {} ms, installed in {} ms",
            runtime.version,
            baked.as_millis(),
            started.elapsed().as_millis()
        );
        self.build.tilolang_runtime = format!("tilolang/{}", runtime.version);
        Ok(())
    }

    /// The runtime binary changed (or the tool just started on it): re-bake and reinstall it,
    /// build the apps naming it, and restart the apps' windows on it.
    fn runtime_changed(&mut self) {
        let started = Instant::now();
        let result = self
            .install_runtime()
            .and_then(|()| self.build_and_install(started))
            .and_then(|apps| self.restart(&apps));
        match result {
            Ok(()) => eprintln!(
                "==> the apps run on the new runtime, {} ms after it changed",
                started.elapsed().as_millis()
            ),
            Err(error) => eprintln!("{error}"),
        }
    }

    /// Start every app of `apps` that has a window again, on what is installed now.
    fn restart(&self, apps: &[BuiltApp]) -> Result<(), String> {
        for peer in &self.peers {
            for app in apps {
                if peer.is_open(&self.bin, &app.app) {
                    peer.restart(&self.bin, &app.app)?;
                }
            }
        }
        Ok(())
    }

    /// The package directories a build reads: the ones to watch.
    fn watched_dirs(&self) -> Result<BTreeSet<PathBuf>, String> {
        Ok(
            tilolang::source::dependency_order(&self.build.dir, &self.build.search)?
                .into_iter()
                .collect(),
        )
    }

    fn watch(
        &self,
        watcher: &mut notify::RecommendedWatcher,
        watching: &mut BTreeSet<PathBuf>,
    ) -> Result<(), String> {
        let runtime_dir = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.binary.parent().map(Path::to_path_buf));
        for dir in self.watched_dirs()?.into_iter().chain(runtime_dir) {
            if watching.insert(dir.clone()) {
                watcher
                    .watch(&dir, RecursiveMode::NonRecursive)
                    .map_err(|error| format!("{}: {error}", dir.display()))?;
            }
        }
        Ok(())
    }

    /// Rebuild on every edit until the session ends (or forever, attached to one).
    pub(crate) fn follow(
        &mut self,
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
            let Ok(event) = event else { continue };
            let mut source = is_source_edit(&event);
            let mut runtime = self.is_runtime_change(&event);
            if !source && !runtime {
                continue;
            }
            // An editor's save, or a link of the runtime, is several events: take every one
            // already queued with this one.
            while let Ok(wake) = wakes.try_recv() {
                match wake {
                    Wake::Ended(status) => return ended(status),
                    Wake::File(Ok(more)) => {
                        source |= is_source_edit(&more);
                        runtime |= self.is_runtime_change(&more);
                    }
                    Wake::File(Err(_)) => {}
                }
            }
            if runtime {
                self.runtime_changed();
            } else {
                self.rebuild();
            }
            if let Err(error) = self.watch(watcher, &mut watching) {
                eprintln!("{error}");
            }
        }
    }

    fn is_runtime_change(&self, event: &notify::Event) -> bool {
        use notify::EventKind;
        let Some(runtime) = &self.runtime else {
            return false;
        };
        matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_))
            && event.paths.contains(&runtime.binary)
    }
}

/// Whether `event` changed a package's source.
pub(crate) fn is_source_edit(event: &notify::Event) -> bool {
    use notify::EventKind;
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    ) && event
        .paths
        .iter()
        .any(|path| path.extension().is_some_and(|extension| extension == "tl"))
}

/// The signed runtime package that offers the baked bundle in `bundle` as `tilolang/<version>`,
/// written into `work`.
fn runtime_package(
    bundle: &Path,
    version: &str,
    identity: &SigningIdentity,
    work: &Path,
) -> Result<PathBuf, String> {
    let isa = match std::env::consts::ARCH {
        "x86_64" => "x86_64 = \"v3\"",
        "aarch64" => "aarch64 = \"aarch64\"",
        other => return Err(format!("no runtime package for the architecture {other}")),
    };
    let publisher = hex::encode(identity.public_key_bytes());
    let text = format!(
        r#"# Generated by `tilolang dev --runtime`: a tilolang runtime of the developer's own build.

[package]
id = "{DEV_RUNTIME_PACKAGE}"
name = "tilolang development runtime"
category = "System"
version = "{package_version}"
kind = "runtime"
provides = "tilolang/{version}"

[package.isa]
{isa}

[publisher]
identity = "ed25519:{publisher}"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
type = "cli"
bin = "{bin}"
"#,
        package_version = env!("CARGO_PKG_VERSION"),
        bin = package::RUNTIME_BIN,
    );
    let manifest = tilo_app_package::manifest::Manifest::from_toml_str(&text)
        .map_err(|error| format!("the runtime package's manifest: {error}"))?;
    let packed = tilo_app_package::pack::pack(bundle, &manifest, identity)
        .map_err(|error| format!("packing the runtime: {error}"))?;
    let tapp = work.join("tilolang-dev-runtime.tapp");
    package::write_tapp(&tapp, &packed)?;
    Ok(tapp)
}

/// Develop the package in the running session `options.session` until interrupted.
///
/// # Errors
/// The session is not running, the package does not compile at the start, or a tool of the
/// session fails.
pub fn dev(options: &DevOptions) -> Result<(), String> {
    let repo = session::repo_root(options.repo.as_deref(), &options.dir)?;
    let bin = repo.join(session::BUILD_DIR).join("bin");
    let peer = Peer::attach(&options.session)?;
    let (tilo_runtime, tilolang_runtime) = peer.runtimes_baked()?;
    let runtime = options.runtime.as_ref().map(|binary| DevRuntime {
        binary: std::path::absolute(binary).unwrap_or_else(|_| binary.clone()),
        identity_file: options
            .runtime_identity
            .clone()
            .unwrap_or_else(|| peer.runtime_publisher()),
        version: dev_runtime_version(),
    });
    let build = BuildOptions {
        dir: options.dir.clone(),
        search: options.search.clone(),
        out: peer.state.join("dev-build"),
        tilo_runtime,
        tilolang_runtime,
        version: "0.1.0".to_owned(),
    };
    let identity = crate::identity_at(&crate::state_dir().join("publisher.json"))?;
    let mut dev = Dev::new(build, identity, vec![peer], bin);
    dev.runtime = runtime;
    let (sender, wakes) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = sender.send(Wake::File(event));
    })
    .map_err(|error| error.to_string())?;
    if dev.runtime.is_some() {
        dev.install_runtime()?;
    }
    let apps = dev.build_and_install(Instant::now())?;
    if dev.runtime.is_some() {
        dev.restart(&apps)?;
    }
    dev.open()?;
    eprintln!(
        "==> {} is open; a saved .tl file reaches it{}",
        dev.opened,
        if dev.runtime.is_some() {
            ", and so does a new build of the runtime"
        } else {
            ""
        }
    );
    dev.follow(&wakes, &mut watcher)
}
