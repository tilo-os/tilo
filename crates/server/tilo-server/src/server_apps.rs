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
/// workspace's own under `root` and verified there as a launch verifies it, and the system's apps
/// the server ships, which are to its workspaces what an image's apps are to a device's.
pub struct WorkspacePackages {
    root: PathBuf,
    shipped: Option<package_store::SystemStore>,
}

impl WorkspacePackages {
    /// Packages kept under `root`.
    #[must_use]
    pub const fn new(root: PathBuf) -> Self {
        Self {
            root,
            shipped: None,
        }
    }

    /// These packages, with `packages`, the system's apps this server ships, installed in the
    /// store at `store` as an image's are.
    ///
    /// # Errors
    /// A package does not install.
    pub fn shipping(mut self, store: &Path, packages: &[Vec<u8>]) -> Result<Self, String> {
        let installer = package_store::PackageStore::at(store.to_path_buf());
        for tapp in packages {
            installer
                .install(
                    tapp.as_slice(),
                    |_| {
                        Ok::<_, String>(package_store::Admission {
                            pinned: true,
                            attested_by: String::new(),
                            source: "shipped with this server".to_owned(),
                            held: false,
                        })
                    },
                    None,
                )
                .map_err(|error| format!("a package this server ships: {error}"))?;
        }
        let layout = package_store::Layout::new(store);
        self.shipped = Some(package_store::SystemStore {
            data_dir: layout.root().to_path_buf(),
            payload_dir: layout.packages(),
            interfaces: None,
        });
        Ok(self)
    }

    /// The ids the apps `workspace` runs here were published under before, as the installed
    /// catalogue believes them ([`app_catalogue::apps::earlier_ids`]): the workspace's own store
    /// over the system's apps this server ships, as a device's store lies over its image.
    #[must_use]
    pub fn earlier_ids(&self, workspace: &str) -> tilo_action_policy::EarlierIds {
        let Ok(id) = uuid::Uuid::parse_str(workspace) else {
            return tilo_action_policy::EarlierIds::default();
        };
        let catalog = package_store::Catalog::at(
            self.root.join(id.hyphenated().to_string()),
            self.shipped.clone(),
        );
        app_catalogue::apps::earlier_ids(&catalog).unwrap_or_else(|error| {
            tracing::warn!(%workspace, %error, "the earlier ids of the workspace's apps are not read");
            tilo_action_policy::EarlierIds::default()
        })
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
pub use app_launcher::{ProcessStart, ServerLauncher, Sockets};
#[cfg(feature = "agents")]
pub use brokers::{ServerApps, ServerWorkspaces, Shipped};
#[cfg(feature = "agents")]
pub use pages::{Readers, Reading, ServerPages};

/// The processes this server runs its workspaces' apps in: started as `starts` starts them
/// (through its launcher), each running the package its workspace installed, as `packages`
/// verifies it, and keeping what it keeps under `data_home`.
#[cfg(feature = "agents")]
pub fn server_processes(
    workspaces: ServerWorkspaces,
    packages: std::sync::Arc<WorkspacePackages>,
    data_home: PathBuf,
    starts: Box<tilo_app_launch::tilolang_processes::Starts>,
) -> std::sync::Arc<tilo_app_launch::tilolang_processes::Processes> {
    use tilo_app_launch::tilolang_processes::Processes;
    Processes::with(
        data_home,
        starts,
        Box::new(move |workspace, app| {
            let tapp = workspaces
                .installed_package(workspace, app)
                .ok_or_else(|| format!("this workspace has not installed {app}"))?;
            packages.verified(workspace, app, &tapp)
        }),
    )
}

/// The longest path a Unix socket can be connected by (`sun_path`, less its terminating NUL).
#[cfg(feature = "agents")]
const MAX_SOCKET_PATH: usize = 107;

/// Where the services of the apps of the server whose state is `state_root` listen: a directory
/// only it may enter, among the login's services ([`tilo_paths::xdg::services_dir`]) as a
/// device's are, named for that state so two servers of one account keep apart. A run reaches
/// them by path, which a socket allows only [`MAX_SOCKET_PATH`] bytes: they are never under the
/// state, however deep that lies.
///
/// # Errors
/// The account has no runtime directory, or one too deep for a workspace's socket in it.
#[cfg(feature = "agents")]
pub fn app_sockets(state_root: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::DirBuilderExt as _;
    let runtime = tilo_paths::xdg::runtime_dir_or_default();
    if !runtime.is_dir() {
        return Err(format!(
            "this server's apps listen in its account's runtime directory, and there is none at \
             {}: set XDG_RUNTIME_DIR to a private directory",
            runtime.display()
        ));
    }
    let state = blake3::hash(state_root.as_os_str().as_encoded_bytes());
    let sockets = tilo_paths::xdg::services_dir()
        .join(format!("server-{}", hex::encode(&state.as_bytes()[..6])));
    // The longest socket there: a workspace's broker, named by the workspace's id.
    let longest = sockets.join(format!("{}.sock", tilo_desktop_model::WorkspaceId::nil()));
    if longest.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(format!(
            "the runtime directory {} is too deep for this server's apps' sockets ({} is over {MAX_SOCKET_PATH} bytes): set XDG_RUNTIME_DIR to a shorter one",
            runtime.display(),
            longest.display(),
        ));
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&sockets)
        .map_err(|error| format!("make {}: {error}", sockets.display()))?;
    Ok(sockets)
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

/// The apps of the workspaces a server keeps, started: they run until the server stops
/// ([`Serving::run`]).
#[cfg(feature = "agents")]
pub struct Serving {
    apps: std::sync::Arc<ServerApps>,
    _identity: tilo_identity::broker::IdentityBrokerHandle,
    _content: ContentService,
}

#[cfg(feature = "agents")]
impl Serving {
    /// Run the workspaces' apps until the server stops.
    pub async fn run(self) {
        let Self {
            apps,
            _identity,
            _content,
        } = self;
        apps.run().await;
    }
}

/// The signed packages in `dir`, each a system app this server ships.
///
/// # Errors
/// The directory does not read, or holds no `.tapp`.
fn shipped_packages(dir: &Path) -> Result<Vec<Vec<u8>>, String> {
    let unread = |error: std::io::Error| format!("read the packages in {}: {error}", dir.display());
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(unread)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()
        .map_err(unread)?;
    paths.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "tapp")
    });
    paths.sort();
    if paths.is_empty() {
        return Err(format!("{} holds no .tapp package", dir.display()));
    }
    paths
        .iter()
        .map(|path| {
            std::fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))
        })
        .collect()
}

/// Start the apps of the workspaces `workspaces` keeps on this server, whose device is `device`:
/// the content store they keep their files in under `state_root` and the identity service that
/// signs their changes as this device for the callers they serve, both listening with the
/// workspaces' brokers where [`app_sockets`] says, and the processes that run each workspace's
/// packages through its launcher. The packages in `shipped` are installed in every workspace
/// whose apps this server sets up. Their pages show beside those of `pages`, the server's own,
/// when it shows any.
///
/// # Errors
/// Why this server can run no app: the packages it ships do not read, or its launcher, sockets'
/// directory, content store, identity service or data home is missing.
#[cfg(feature = "agents")]
pub async fn start(
    workspaces: crate::workspaces::Workspaces,
    device: Device,
    state_root: &Path,
    shipped: Option<&Path>,
    pages: Option<std::sync::Arc<dyn crate::app_serving::AppActionServer>>,
) -> Result<Serving, String> {
    let launcher = tilo_launcher(&launcher_candidates())?;
    start_through(
        workspaces,
        device,
        (state_root, shipped),
        pages,
        tilo_app_launch::tilolang_processes::Processes::started_through(launcher),
    )
    .await
}

/// [`start`], with each process started as `starts` starts it.
#[cfg(feature = "agents")]
async fn start_through(
    workspaces: crate::workspaces::Workspaces,
    device: Device,
    (state_root, shipped): (&Path, Option<&Path>),
    pages: Option<std::sync::Arc<dyn crate::app_serving::AppActionServer>>,
    starts: Box<tilo_app_launch::tilolang_processes::Starts>,
) -> Result<Serving, String> {
    let shipped = shipped
        .map(shipped_packages)
        .transpose()?
        .unwrap_or_default();
    let apps = state_root.join("apps");
    let sockets = app_sockets(state_root)?;
    let content = ContentService::start(
        &apps.join("content"),
        &sockets.join(brokers::CONTENT_SOCKET),
    )
    .await
    .map_err(|error| format!("the content store of this server's apps: {error}"))?;
    let data_home = launcher::appdir::host_data_home()
        .ok_or("this server has no data home to keep its apps' storage in")?;
    let runtime = tokio::runtime::Handle::current();
    let server = ServerWorkspaces::new(workspaces.clone(), runtime.clone());
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
    let identity = tilo_identity::broker::spawn_signing(
        sockets.join(brokers::IDENTITY_SOCKET),
        std::sync::Arc::new(server.clone()),
        keys,
        std::sync::Arc::new(()),
        std::sync::Arc::new(signer),
    )
    .map_err(|error| format!("the identity service of this server's apps: {error}"))?;
    let mut packages = WorkspacePackages::new(data_home.join("tilo").join("workspace-packages"));
    if !shipped.is_empty() {
        packages = packages.shipping(&data_home.join("tilo").join("shipped-packages"), &shipped)?;
    }
    let packages = std::sync::Arc::new(packages);
    let processes = server_processes(
        server.clone(),
        std::sync::Arc::clone(&packages),
        data_home,
        starts,
    );
    let apps = ServerApps::new(
        server,
        sockets,
        apps.join("journals"),
        device.cert.device_public_key,
        Ok(processes),
        Shipped {
            packages: shipped,
            kept: std::sync::Arc::clone(&packages),
        },
    );
    if let Some(pages) = pages {
        pages.host_pages(std::sync::Arc::new(ServerPages::new(
            std::sync::Arc::clone(&apps),
            packages,
            readers,
        )));
    }
    Ok(Serving {
        apps,
        _identity: identity,
        _content: content,
    })
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
