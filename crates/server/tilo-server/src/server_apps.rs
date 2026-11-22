//! The apps a server runs for the workspaces it holds, with the same sandbox a device runs them
//! with.
//!
//! A workspace's apps are the packages it installed: the server keeps each one's `.tapp` file as
//! every device of the workspace does, installs it into a package store of that workspace's own
//! and verifies the installation exactly as a device's launch verifies it
//! ([`WorkspacePackages::verified`]). A package whose bytes do not hold together, or that is not
//! the app it was installed as, is refused.
//!
//! The system runs a package's code in a tilolang process started through `tilo-launcher
//! tilolang-process` (bubblewrap, Landlock, seccomp): a server without that launcher runs no app
//! at all ([`tilo_launcher`]), never one unconfined.
//!
//! The server runs one content store for every app it runs, beside its packages, served on one
//! socket ([`ContentService`]): each app's connection to it is scoped to that app in the
//! workspace of the call, exactly as a device's launcher scopes it.
//!
//! A workspace's process of an app serves every page open on the app and every call of its
//! actions that shows nothing. The collaboration connection of each window and each call is made
//! by this process, and the broker believes it as that window or call only, once
//! ([`ServerLauncher`]); nothing else that reaches the broker's socket is believed to be one of
//! its apps. How a window is drawn is the page's to say: the connection names a window, not how
//! it shows.

use std::path::{Path, PathBuf};

/// The packages of the workspaces this server hosts, each installed into a store of its
/// workspace's own under `root` and verified there as a launch verifies it.
pub struct WorkspacePackages {
    root: PathBuf,
}

impl WorkspacePackages {
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
        Ok(Self::new(data_home.join("tilo").join("workspace-packages")))
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

    /// `tapp`, the package `workspace` installed as `app`, installed for this
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
        self.named(workspace, app, tapp)
            .map(|(verified, _)| verified)
    }

    /// [`Self::verified`], with the name the package gives its app, when it gives one.
    ///
    /// # Errors
    /// As [`Self::verified`].
    pub fn named(
        &self,
        workspace: &str,
        app: &str,
        tapp: &[u8],
    ) -> Result<
        (
            launcher::run::tilolang_process::VerifiedInstance,
            Option<String>,
        ),
        String,
    > {
        let store = self.store(workspace)?;
        let mut name = None;
        let installed = store
            .install(
                tapp,
                |meta| {
                    let id = &meta.manifest.package.id;
                    if id != app {
                        return Err(format!("the package is {id}, not {app}"));
                    }
                    name.clone_from(&meta.manifest.package.name);
                    Ok(package_store::Admission {
                        pinned: false,
                        attested_by: String::new(),
                        source: format!("installed by workspace {workspace}"),
                        held: false,
                    })
                },
                None,
            )
            .map_err(|error| format!("{app} was refused: {error}"))?
            .package;
        let verified = launcher::run::tilolang_process::verified_instance(
            &installed,
            &installed.manifest_path,
            &installed.payload_root,
            &installed.package_id_hex,
        )
        .map_err(|error| format!("{app} does not verify: {error}"))?;
        Ok((verified, name))
    }
}

/// The content store this server's apps keep the files a person chooses and the pictures their
/// resources hold in, served on its socket: one for the whole server, each connection scoped to
/// one app in one workspace by the launcher that makes it
/// ([`launcher::services::resolve_blob_bind`]).
pub struct ContentService {
    _server: std::thread::JoinHandle<()>,
}

impl ContentService {
    /// Open the content store at `content` and serve it on `socket`, on the runtime this is
    /// awaited on.
    ///
    /// # Errors
    /// The store does not open, or the socket cannot be bound.
    pub async fn start(content: &Path, socket: &Path) -> std::io::Result<Self> {
        let store = std::sync::Arc::new(
            tilo_fs::ContentStore::open(content)
                .await
                .map_err(std::io::Error::other)?,
        );
        let media = tilo_fs::document_media::DocumentMedia::new(store);
        let server =
            tilo_fs::document_media::serve_socket(socket, media).map_err(std::io::Error::other)?;
        Ok(Self { _server: server })
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

/// The first of `candidates` that is a file: the launcher every app on this server is started
/// through.
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

#[cfg(feature = "agents")]
mod app_launcher;
#[cfg(feature = "agents")]
mod brokers;
#[cfg(feature = "agents")]
mod pages;
#[cfg(feature = "agents")]
pub use app_launcher::{ProcessStart, ServerLauncher};
#[cfg(feature = "agents")]
pub use brokers::{ServerApps, ServerWorkspaces};
#[cfg(feature = "agents")]
pub use pages::{Readers, Reading, ServerPages};

/// The processes this server runs its workspaces' apps in: started through its launcher, each
/// running the package its workspace installed, as `packages` verifies it, and keeping what it
/// keeps under `data_home`.
#[cfg(feature = "agents")]
pub fn server_processes(
    workspaces: ServerWorkspaces,
    packages: WorkspacePackages,
    data_home: PathBuf,
) -> ProcessStart {
    use tilo_app_launch::tilolang_processes::Processes;
    let launcher = tilo_launcher(&launcher_candidates())?;
    Ok(Processes::with(
        data_home,
        Processes::started_through(launcher),
        Box::new(move |workspace, app| {
            let tapp = workspaces
                .installed_package(workspace, app)
                .ok_or_else(|| format!("this workspace has not installed {app}"))?;
            packages.verified(workspace, app, &tapp)
        }),
    ))
}

/// This server's device, as its apps' requests are signed by it.
#[cfg(feature = "agents")]
pub struct Device {
    /// Where its key and certificate are kept.
    pub identity_root: PathBuf,
    /// Its certificate.
    pub cert: tilo_user_identity::DeviceCert,
    /// Its key service's keys, when it takes part in its workspaces' key groups: what it hands a
    /// page whose reader may read what the workspace keeps sealed.
    pub keys: Option<tilo_workspace_keys::KeyProvider>,
}

/// Run the apps of the workspaces `workspaces` keeps on this server, whose device is `device`,
/// until the server stops: the content store they keep their files in, under `state_root`, the
/// identity service that signs their changes as this device for the callers they serve, and
/// one broker for each workspace, each running the workspace's packages in this server's
/// processes. Their pages show beside those of `pages`, the server's own, when it shows any. A
/// server that cannot run apps says why once, and keeps serving its workspaces.
#[cfg(feature = "agents")]
pub async fn serve(
    workspaces: crate::workspaces::Workspaces,
    device: Device,
    state_root: PathBuf,
    pages: Option<std::sync::Arc<dyn crate::app_serving::AppActionServer>>,
) {
    let apps = state_root.join("apps");
    let content = match ContentService::start(
        &apps.join("content"),
        &launcher::services::blob_socket(),
    )
    .await
    {
        Ok(content) => content,
        Err(error) => {
            tracing::warn!(%error, "this server's apps have no content store, so none runs");
            return;
        }
    };
    let Some(data_home) = launcher::appdir::host_data_home() else {
        tracing::warn!("this server has no data home to keep its apps' storage in, so none runs");
        return;
    };
    let runtime = tokio::runtime::Handle::current();
    let server = ServerWorkspaces::new(workspaces.clone(), runtime.clone());
    let identity_socket = launcher::services::identity_socket();
    if let Some(parent) = identity_socket.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(%error, "this server's apps have no identity service, so none runs");
        return;
    }
    // Every request a page makes is decided as the one who reads it.
    let readers = std::sync::Arc::new(Readers::default());
    let signer = tilo_workspace_service::apps::caller_signer::CallerSigner::new(
        workspaces,
        runtime,
        tilo_device_identity::persist::IdentityPaths::under(&device.identity_root),
        device.cert.user_public_key,
        std::sync::Arc::clone(&readers)
            as std::sync::Arc<dyn tilo_workspace_service::apps::caller_signer::Callers>,
    );
    let keys: std::sync::Arc<dyn tilo_identity::sources::GroupKeySource> = match device.keys {
        Some(keys) => std::sync::Arc::new(keys),
        None => std::sync::Arc::new(()),
    };
    let identity = match tilo_identity::broker::spawn_signing(
        identity_socket,
        std::sync::Arc::new(server.clone()),
        keys,
        std::sync::Arc::new(()),
        std::sync::Arc::new(signer),
    ) {
        Ok(identity) => identity,
        Err(error) => {
            tracing::warn!(%error, "this server's apps have no identity service, so none runs");
            return;
        }
    };
    let packages = data_home.join("tilo").join("workspace-packages");
    let processes = server_processes(
        server.clone(),
        WorkspacePackages::new(packages.clone()),
        data_home,
    );
    if let Err(error) = &processes {
        tracing::warn!(%error, "this server runs no app");
    }
    let apps = ServerApps::new(
        server,
        tilo_paths::xdg::services_dir().join("apps"),
        apps.join("journals"),
        device.cert.device_public_key,
        processes,
    );
    if let Some(pages) = pages {
        pages.host_pages(std::sync::Arc::new(ServerPages::new(
            std::sync::Arc::clone(&apps),
            WorkspacePackages::new(packages),
            readers,
        )));
    }
    apps.run().await;
    drop((identity, content));
}

/// What a workspace's broker is made of.
#[cfg(feature = "agents")]
struct BrokerParts {
    socket: PathBuf,
    authority: std::sync::Arc<dyn tilo_workspace_service::apps::services::WorkspaceAuthority>,
    commits: std::sync::Arc<tilo_workspace_service::apps::service_workspaces::ServiceCommits>,
    services: tilo_workspace_service::apps::services::ProcessServices,
    channels:
        std::sync::Arc<std::sync::Mutex<tilo_workspace_service::apps::collab_core::BrokerChannels>>,
    fanout: std::sync::Arc<tilo_workspace_service::apps::collab_broker::FanoutState>,
}

/// The one place a workspace's broker is made, as every host that serves apps makes its own.
#[cfg(feature = "agents")]
fn spawn_broker(
    parts: BrokerParts,
) -> std::io::Result<tilo_workspace_service::apps::collab_broker::BrokerHandle> {
    tilo_workspace_service::apps::collab_broker::spawn_with_services(
        parts.socket,
        Some(parts.authority),
        tilo_workspace_service::apps::collab_outbound::PeerPublisher::with_optional_owner_commit(
            None,
            parts.commits,
        ),
        None,
        parts.services,
        parts.channels,
        parts.fanout,
    )
}

#[cfg(test)]
#[path = "server_apps_tests.rs"]
mod tests;
