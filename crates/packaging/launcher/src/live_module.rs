//! Development sessions only: a running app whose payload is its module follows
//! reinstalls of its package, so a new build takes over without the app restarting.
//!
//! A package whose entrypoint executes its runtime's binary (`[entrypoint] runtime`, a
//! tilolang app) is code the runtime interprets: its payload is a module, its metadata
//! and its surface.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::manifest::ManifestView;
use crate::run::{RunError, RunOptions};

pub use crate::dev_env::LIVE_MODULES_ENV;
pub use tilo_app_module::{CURRENT_FILE, UPDATES_ENV};
/// Where the app sees the later builds of its package.
pub const UPDATES_MOUNT: &str = "/run/tilo-app-module-updates";

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
    /// The live module of this launch, when the session asks for live modules and the launch is a
    /// pinned launch of a package whose payload is its module.
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

    /// The bind and the variable that show the app its later builds; `ro_paths` gains the mount for the
    /// sandbox's Landlock plan.
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

    /// Follow reinstalls until the returned guard is dropped (the app exited), which stops the watch
    /// and removes the directory.
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
        while replacements.recv().is_ok() {
            // A burst of replacements is one look at the store.
            while replacements.try_recv().is_ok() {}
            match self.take_reinstall() {
                Ok(Some((generation, taken))) => {
                    tracing::info!(
                        app = self.app,
                        generation,
                        package = taken,
                        "live module: the app follows its reinstalled package"
                    );
                    self.current = taken;
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

    /// Take the installed package of the app as the next generation, when it changed since the last one
    /// taken: its number and package id.
    fn take_reinstall(&self) -> Result<Option<(u64, String)>, String> {
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
        let generation = tilo_app_module::publish(&self.dir, &installed.payload_root)
            .map_err(|error| error.to_string())?;
        Ok(Some((generation, installed.package_id_hex)))
    }
}

/// The publisher whose key signed the installed package `package_id_hex`, from its verified record.
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
    let _ = std::fs::remove_dir_all(dir);
    tilo_app_module::make_dir(dir).map_err(RunError::Io)
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
