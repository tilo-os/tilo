//! The apps a server runs for the workspaces it hosts, on the same app broker and with the same
//! sandbox a device runs them with.
//!
//! A hosted workspace's apps are the packages its governance installed: the server keeps each
//! one's `.tapp` file as the workspace's other holders do, installs it into a package store of
//! that workspace's own and verifies the installation exactly as a device's launch verifies it
//! ([`HostedPackages::verified`]). A package whose bytes do not hold together, or that is not
//! the app it was installed as, is refused.
//!
//! An instance runs in a tilolang process started through `tilo-launcher tilolang-process`
//! (bubblewrap, Landlock, seccomp): a server without that launcher runs no app at all
//! ([`tilo_launcher`]), never one unconfined.

use std::path::{Path, PathBuf};

/// The packages of the workspaces this server hosts, each installed into a store of its
/// workspace's own under `root` and verified there as a launch verifies it.
pub struct HostedPackages {
    root: PathBuf,
}

impl HostedPackages {
    /// Packages kept under `root`.
    #[must_use]
    pub const fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Packages kept under the host's data home, beside the app directories its launcher mounts.
    ///
    /// # Errors
    /// The host has no data home.
    pub fn at_host_data_home() -> Result<Self, String> {
        let data_home = launcher::appdir::host_data_home()
            .ok_or("this server has no data home to keep its workspaces' apps in")?;
        Ok(Self::new(
            data_home.join("tilo").join("hosted-packages"),
        ))
    }

    /// The package store of `workspace`.
    fn store(&self, workspace: &str) -> Result<package_store::PackageStore, String> {
        // A workspace id names a directory: only a real id may.
        let id = uuid::Uuid::parse_str(workspace)
            .map_err(|_| format!("{workspace:?} is no workspace id"))?;
        Ok(package_store::PackageStore::at(
            self.root.join(id.hyphenated().to_string()),
        ))
    }

    /// `tapp`, the package `workspace`'s governance installed as `app`, installed for this
    /// server to run and verified as a launch verifies it: its files, its signature, its
    /// publisher not revoked, and a tilolang app.
    ///
    /// # Errors
    /// A package that does not unpack or whose bytes do not match their names, one that is not
    /// `app`, one whose installation does not verify, or one that is no tilolang app.
    pub fn verified(
        &self,
        workspace: &str,
        app: &str,
        tapp: &[u8],
    ) -> Result<launcher::run::tilolang_process::VerifiedInstance, String> {
        let store = self.store(workspace)?;
        let installed = store
            .install(
                tapp,
                |meta| {
                    let id = &meta.manifest.package.id;
                    if id != app {
                        return Err(format!("the package is {id}, not {app}"));
                    }
                    Ok(package_store::Admission {
                        pinned: false,
                        attested_by: String::new(),
                        source: format!("the governance of workspace {workspace}"),
                        held: false,
                    })
                },
                None,
            )
            .map_err(|error| format!("{app} was refused: {error}"))?
            .package;
        launcher::run::tilolang_process::verified_instance(
            &installed,
            &installed.manifest_path,
            &installed.payload_root,
            &installed.package_id_hex,
        )
        .map_err(|error| format!("{app} does not verify: {error}"))
    }
}

/// Where `tilo-launcher` is looked for on a server: beside this executable, then the system
/// paths.
#[must_use]
pub fn launcher_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::parent)
    {
        out.push(dir.join("tilo-launcher"));
    }
    out.push(PathBuf::from("/usr/libexec/tilo/tilo-launcher"));
    out.push(PathBuf::from("/usr/libexec/tilo-launcher"));
    out
}

/// The first of `candidates` that is a file: the launcher every app instance on this server is
/// started through.
///
/// # Errors
/// None is installed: then no app runs here, since none may run unconfined.
pub fn tilo_launcher(candidates: &[PathBuf]) -> Result<PathBuf, String> {
    candidates
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .ok_or_else(|| {
            let searched: Vec<String> = candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            format!(
                "no tilo-launcher is installed on this server (looked at {}), so it runs no app: \
                 an app runs only in the launcher's sandbox",
                searched.join(", ")
            )
        })
}

#[cfg(test)]
#[path = "hosted_apps_tests.rs"]
mod tests;
