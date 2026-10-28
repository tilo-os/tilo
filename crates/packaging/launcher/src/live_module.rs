//! Development sessions only: a running app whose payload is its module follows reinstalls of
//! its package, so a new build takes over without the app restarting.
//!
//! A package whose entrypoint executes its runtime's binary (`[entrypoint] runtime`, a tilolang
//! app) is code the runtime interprets: its payload is a module, its metadata and its surface.
//! In a development session (a launcher built with the developer overrides, started with
//! [`LIVE_MODULES_ENV`] set, which the compositor strips when its own gate is closed) a pinned
//! launch of such a package also gets a directory of later builds of the same package:
//!
//! * the launcher creates it under its runtime directory and binds it read-only into the sandbox
//!   at [`UPDATES_MOUNT`], named to the app by [`UPDATES_ENV`];
//! * it watches the app store, and when the installed package of the app's id changes, it checks
//!   the new installation exactly as a launch does (`run::verified_launch_installation`: the
//!   signed record, the payload's integrity, the revocation list), and takes it only from the
//!   same publisher, executing the same runtime binary;
//! * it copies the verified payload into `<generation>/` and then names that generation in
//!   [`CURRENT_FILE`], each by one rename. The app reads the generation `current` names.
//!
//! So nothing reaches the app that `tilo-install` did not install and the launcher did not
//! verify, the app's process, custody, sandbox and grants are the ones it was launched with, and
//! a shipped launcher has none of it: `package_store::dev_env::dev_override` closes the gate.
//! The only thing that changes is which verified module the runtime interprets.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::manifest::ManifestView;
use crate::run::{RunError, RunOptions};

pub use crate::dev_env::LIVE_MODULES_ENV;
/// The variable that names [`UPDATES_MOUNT`] to the app.
pub const UPDATES_ENV: &str = "TILO_APP_MODULE_UPDATES";
/// Where the app sees the later builds of its package.
pub const UPDATES_MOUNT: &str = "/run/tilo-app-module-updates";
/// The file in the updates directory that names the current generation.
pub const CURRENT_FILE: &str = "current";

/// The registry file of an app store: replaced whenever a package is installed.
const REGISTRY_FILE: &str = package_store::layout::REGISTRY_FILE;

/// A launch that follows reinstalls of its package.
pub struct LiveModule {
    dir: PathBuf,
    app: String,
    /// The package id (hex) of the build the app runs now.
    current: String,
    publisher: [u8; 32],
    runtime: Option<String>,
    bin: String,
    store: PathBuf,
}

/// Whether this launcher follows reinstalls: built with the developer overrides and asked to.
fn enabled() -> bool {
    package_store::dev_env::dev_override(LIVE_MODULES_ENV).is_some_and(|value| value == "1")
}

impl LiveModule {
    /// The live module of this launch, when the session asks for live modules and the launch is
    /// a pinned launch of a package whose payload is its module.
    ///
    /// # Errors
    /// The pinned installation cannot be read again, or the updates directory cannot be made.
    pub fn prepare(manifest: &ManifestView, opts: &RunOptions) -> Result<Option<Self>, RunError> {
        if !enabled() || manifest.entrypoint.runtime.is_none() {
            return Ok(None);
        }
        let (Some(app), Some(expected)) = (
            opts.app_id.as_deref(),
            opts.expected_package_hash.as_deref(),
        ) else {
            return Ok(None);
        };
        let refusal = |what: &str| RunError::InvalidServiceLaunch(format!("live module: {what}"));
        let installed = package_store::find_installed(app)
            .map_err(|error| refusal(&error.to_string()))?
            .ok_or_else(|| refusal("the package is not installed"))?;
        let publisher = publisher_of(&installed, expected)?;
        let store =
            package_store::layout::default_user_root().ok_or_else(|| refusal("no app store"))?;
        let dir = tilo_paths::xdg::runtime_dir_or_default()
            .join("tilo/live-modules")
            .join(std::process::id().to_string());
        create_private_dir(&dir)?;
        Ok(Some(Self {
            dir,
            app: app.to_owned(),
            current: expected.to_owned(),
            publisher,
            runtime: manifest.entrypoint.runtime.clone(),
            bin: manifest.entrypoint.bin.clone(),
            store,
        }))
    }

    /// The bind and the variable that show the app its later builds; `ro_paths` gains the mount
    /// for the sandbox's Landlock plan.
    ///
    /// # Errors
    /// The argv has no entrypoint token.
    pub fn splice(
        &self,
        argv: &mut Vec<String>,
        ro_paths: &mut Vec<String>,
    ) -> Result<(), RunError> {
        let entry = argv.pop().ok_or(RunError::ArgvInvariant)?;
        argv.extend([
            "--ro-bind".to_owned(),
            self.dir.display().to_string(),
            UPDATES_MOUNT.to_owned(),
            "--setenv".to_owned(),
            UPDATES_ENV.to_owned(),
            UPDATES_MOUNT.to_owned(),
            entry,
        ]);
        ro_paths.push(UPDATES_MOUNT.to_owned());
        Ok(())
    }

    /// Follow reinstalls until the returned guard is dropped (the app exited), which stops the
    /// watch and removes the directory.
    ///
    /// # Errors
    /// The store cannot be watched.
    pub fn follow(self) -> Result<Following, RunError> {
        let (replaced, replacements) = mpsc::channel();
        let watch = tilo_files::native::watch_changes(
            &self.store,
            |path| path.file_name().is_some_and(|name| name == REGISTRY_FILE),
            move || {
                let _ = replaced.send(());
            },
        )
        .map_err(RunError::Io)?;
        let dir = self.dir.clone();
        let thread = std::thread::Builder::new()
            .name("live-module".into())
            .spawn(move || self.run(&replacements))
            .map_err(RunError::Spawn)?;
        Ok(Following {
            dir,
            watch: Some(watch),
            thread: Some(thread),
        })
    }

    /// Take each reinstall until the watch ends, which closes `replacements`.
    fn run(mut self, replacements: &mpsc::Receiver<()>) {
        let mut generation = 0_u64;
        while replacements.recv().is_ok() {
            // A burst of replacements is one look at the store.
            while replacements.try_recv().is_ok() {}
            let next = generation.saturating_add(1);
            match self.take_reinstall(next) {
                Ok(Some(taken)) => {
                    generation = next;
                    tracing::info!(
                        app = self.app,
                        generation,
                        package = taken,
                        "live module: the app follows its reinstalled package"
                    );
                    self.current = taken;
                    self.forget_before(generation);
                }
                Ok(None) => {}
                Err(reason) => tracing::warn!(
                    app = self.app,
                    %reason,
                    "live module: the reinstalled package is not taken"
                ),
            }
        }
    }

    /// Take the installed package of the app into generation `generation`, when it changed since
    /// the last one taken; its package id.
    fn take_reinstall(&self, generation: u64) -> Result<Option<String>, String> {
        let Some(installed) =
            package_store::find_installed(&self.app).map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        if installed.package_id_hex == self.current {
            return Ok(None);
        }
        let manifest = crate::run::verified_launch_installation(
            &installed,
            &installed.manifest_path,
            &installed.payload_root,
            &installed.package_id_hex,
        )
        .map_err(|error| error.to_string())?;
        let publisher =
            publisher_of(&installed, &installed.package_id_hex).map_err(|e| e.to_string())?;
        if publisher != self.publisher {
            return Err("another publisher signed it".to_owned());
        }
        if manifest.entrypoint.runtime != self.runtime || manifest.entrypoint.bin != self.bin {
            return Err(format!(
                "it executes another runtime ({}), which only a new start of the app runs",
                manifest.entrypoint.runtime.as_deref().unwrap_or("none")
            ));
        }
        let name = generation.to_string();
        let partial = self.dir.join(format!("{name}.partial"));
        let _ = std::fs::remove_dir_all(&partial);
        copy_tree(&installed.payload_root, &partial).map_err(|error| error.to_string())?;
        std::fs::rename(&partial, self.dir.join(&name)).map_err(|error| error.to_string())?;
        let current = self.dir.join(format!("{CURRENT_FILE}.partial"));
        std::fs::write(&current, format!("{name}\n")).map_err(|error| error.to_string())?;
        std::fs::rename(&current, self.dir.join(CURRENT_FILE))
            .map_err(|error| error.to_string())?;
        Ok(Some(installed.package_id_hex))
    }

    /// Remove generations before the one before `generation`: the app reads the current one,
    /// and may still be reading the one it replaced.
    fn forget_before(&self, generation: u64) {
        let Some(keep) = generation.checked_sub(1) else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let old = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u64>().ok())
                .is_some_and(|number| number < keep);
            if old {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// The publisher whose key signed the installed package `package_id_hex`, from its verified
/// record.
fn publisher_of(
    installed: &package_store::InstalledPackage,
    package_id_hex: &str,
) -> Result<[u8; 32], RunError> {
    package_store::integrity::read_record(
        &package_store::integrity::record_dir(installed),
        &installed.id,
        package_id_hex,
    )
    .map(|record| record.root.publisher_identity)
    .map_err(|error| RunError::InvalidServiceLaunch(format!("live module: {error}")))
}

fn create_private_dir(dir: &Path) -> Result<(), RunError> {
    use std::os::unix::fs::DirBuilderExt as _;
    let _ = std::fs::remove_dir_all(dir);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(RunError::Io)
}

/// Copy the payload tree `from` into `to`: regular files and directories, nothing else (a
/// payload holds no links).
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// The running watch; dropping it stops the watch and removes the updates directory.
pub struct Following {
    dir: PathBuf,
    watch: Option<tilo_files::native::ChangeWatch>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Following {
    fn drop(&mut self) {
        // The watch's end closes the channel the thread waits on.
        drop(self.watch.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
