//! Typed client for the launcher-issued `app-host` connection.
//!
//! The Store worker is synchronous, so this wrapper owns a current-thread
//! runtime and `LocalSet` while keeping one connection alive for its whole
//! lifetime. It never reaches for a host path or executable.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::rc::Rc;

use crate::install::InstallExpectation;
use crate::wire::{
    Action, Answer, Call, Catalogue, Expected, FlatpakVerb, Operation, RemoteVerb, Reply, Resource,
    WirePinning,
};

mod pending;
use package_store::InstalledPackage;
pub use pending::PendingRead;
use tilo_app_package::PackageId;

/// Why the host refused a catalogue read: the one thing the Store decides
/// what to say from. It travels typed on the wire, so a
/// reworded detail never changes how the Store handles it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Refusal {
    /// The system's interface declaration cannot be read: the system needs
    /// an update before any repository can be judged.
    SystemUpdateNeeded,
    /// The repository's catalogue is authentic but expired.
    Expired,
    /// The repository did not answer within the read's deadline.
    TimedOut,
    /// A device-provided Flatpak repository has no OCI summary yet.
    DeviceRepositoryStale,
}

/// Why a catalogue read gave no rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatalogueError {
    /// The host refused it for a reason the Store explains.
    Refused {
        refusal: Refusal,
        /// For logs; never decides anything.
        detail: String,
    },
    /// Anything else: the transport, the host, the repository.
    Failed(String),
}

impl CatalogueError {
    /// The refusal, when the host refused.
    #[must_use]
    pub fn refusal(&self) -> Option<Refusal> {
        match self {
            Self::Refused { refusal, .. } => Some(*refusal),
            Self::Failed(_) => None,
        }
    }
}

impl std::fmt::Display for CatalogueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { refusal, detail } => write!(f, "{refusal:?}: {detail}"),
            Self::Failed(error) => f.write_str(error),
        }
    }
}

impl From<String> for CatalogueError {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

impl From<&str> for CatalogueError {
    fn from(error: &str) -> Self {
        Self::Failed(error.to_owned())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlatpakAction {
    Install,
    Update,
    Remove,
}

/// One installed package as the host lists it: its store row, and what its
/// signed manifest says to show a person about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedPackage {
    pub package: InstalledPackage,
    pub about: PackageAbout,
}

/// The manifest's `[package]` display metadata. It is installed and signed,
/// so it describes the app whether or not any repository is reachable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageAbout {
    /// `[package].name`, or a name derived from the id when it is unset.
    pub name: String,
    /// `[package.names]`: the name in other languages, by BCP 47 tag.
    pub names: std::collections::BTreeMap<String, String>,
    /// `[package].category`, when declared.
    pub category: Option<String>,
    /// `[package.taglines]`: what the app does in one phrase, by BCP 47 tag.
    pub taglines: std::collections::BTreeMap<String, String>,
    /// `[capabilities]`, required and optional together: what the app's
    /// code may ask the sandbox for (`network.http`, `files.open`, ...).
    pub capabilities: Vec<String>,
}

impl PackageAbout {
    fn from_manifest(package: &tilo_app_package::manifest::Package) -> Self {
        Self {
            name: package
                .name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| package_store::display_name_for_package_id(&package.id)),
            names: package.names.clone(),
            category: package
                .category
                .clone()
                .filter(|category| !category.trim().is_empty()),
            taglines: package.taglines.clone(),
            capabilities: Vec::new(),
        }
    }

    /// What the whole manifest says to show a person: its `[package]`
    /// metadata and the capabilities it declares.
    pub(crate) fn from_full_manifest(manifest: &tilo_app_package::Manifest) -> Self {
        let mut about = Self::from_manifest(&manifest.package);
        about.capabilities = manifest
            .capabilities
            .required
            .iter()
            .chain(&manifest.capabilities.optional)
            .cloned()
            .collect();
        about
    }

    /// The tag line to show in `locale` (BCP 47): its exact tag, else its
    /// language, else English; empty when the app declares none.
    #[must_use]
    pub fn tagline_for(&self, locale: &str) -> &str {
        let language = locale.split(['-', '_']).next().unwrap_or(locale);
        [locale, language, "en"]
            .into_iter()
            .find_map(|wanted| {
                self.taglines
                    .iter()
                    .find(|(tag, _)| tag.eq_ignore_ascii_case(wanted))
            })
            .map_or("", |(_, tagline)| tagline.as_str())
    }

    /// The name to show in `locale` (BCP 47): its exact tag, else its
    /// language, else [`Self::name`].
    #[must_use]
    pub fn name_for(&self, locale: &str) -> &str {
        let language = locale.split(['-', '_']).next().unwrap_or(locale);
        [locale, language]
            .into_iter()
            .find_map(|wanted| {
                self.names
                    .iter()
                    .find(|(tag, _)| tag.eq_ignore_ascii_case(wanted))
            })
            .map_or(self.name.as_str(), |(_, name)| name.as_str())
    }
}

/// Host Flatpak availability and machine-readable installation/remote rows.
#[derive(Clone, Debug, Default)]
pub struct FlatpakStatus {
    pub available: bool,
    pub installed: String,
    pub remotes: String,
}

/// The calls in flight on one connection, answered by its reader task.
struct Connection {
    writer: tokio::sync::Mutex<tokio::net::unix::OwnedWriteHalf>,
    waiting: RefCell<HashMap<u64, tokio::sync::oneshot::Sender<Result<Answer, String>>>>,
    next: Cell<u64>,
    closed: Cell<bool>,
}

impl Connection {
    #[allow(
        clippy::future_not_send,
        reason = "the connection is shared by Rc on its current-thread runtime"
    )]
    fn call(
        self: &Rc<Self>,
        request: Operation,
    ) -> impl Future<Output = Result<Answer, String>> + use<> {
        let connection = Rc::clone(self);
        async move {
            if connection.closed.get() {
                return Err(DISCONNECTED.to_owned());
            }
            let id = connection.next.get();
            connection.next.set(id.wrapping_add(1));
            let (answer, answered) = tokio::sync::oneshot::channel();
            connection.waiting.borrow_mut().insert(id, answer);
            {
                use tokio::io::AsyncWriteExt as _;
                let mut writer = connection.writer.lock().await;
                let sent = async {
                    crate::wire::FRAMING
                        .write_value_async(&mut *writer, &Call { id, request })
                        .await?;
                    writer.flush().await?;
                    Ok::<_, wire_frame::FrameError>(())
                }
                .await;
                if let Err(error) = sent {
                    connection.waiting.borrow_mut().remove(&id);
                    return Err(format!("app-host: {error}"));
                }
            }
            answered.await.map_err(|_| DISCONNECTED.to_owned())?
        }
    }

    #[allow(
        clippy::future_not_send,
        reason = "the connection is shared by Rc on its current-thread runtime"
    )]
    async fn read_replies(self: Rc<Self>, mut reader: tokio::net::unix::OwnedReadHalf) {
        while let Ok(Some(Reply { id, answer })) =
            crate::wire::FRAMING.read_value_async(&mut reader).await
        {
            if let Some(waiting) = self.waiting.borrow_mut().remove(&id) {
                let _ = waiting.send(answer);
            }
        }
        self.closed.set(true);
        self.waiting.borrow_mut().clear();
    }
}

const DISCONNECTED: &str = "the app host connection has ended";

pub struct BlockingClient {
    runtime: tokio::runtime::Runtime,
    local: tokio::task::LocalSet,
    connection: Rc<Connection>,
}

impl BlockingClient {
    /// Claim the pre-connected descriptor the launcher granted to this app.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime or launcher-issued service cannot be opened.
    #[cfg(unix)]
    pub fn from_service() -> Result<Self, String> {
        use std::os::fd::FromRawFd as _;
        use std::sync::atomic::{AtomicBool, Ordering};
        // The launcher hands the process one connection; a second owner would
        // interleave frames and close the descriptor twice.
        static CLAIMED: AtomicBool = AtomicBool::new(false);
        let fd = runtime_portal::service_fd(runtime_portal::runtime_request::SERVICE)
            .ok_or_else(|| "the app host was not handed to this app".to_owned())?;
        if CLAIMED.swap(true, Ordering::SeqCst) {
            return Err("the app host connection is already in use".to_owned());
        }
        // SAFETY: `service_fd` verified the descriptor is open, the launcher
        // gave it to this process alone, and `CLAIMED` makes this its one owner.
        Self::from_connected_fd(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
    }

    /// Open an explicitly owned connection (a test host).
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime cannot start or `fd` is no socket.
    #[cfg(unix)]
    pub fn from_connected_fd(fd: std::os::fd::OwnedFd) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("create app-host runtime: {e}"))?;
        let local = tokio::task::LocalSet::new();
        let stream = std::os::unix::net::UnixStream::from(fd);
        stream
            .set_nonblocking(true)
            .map_err(|e| format!("app-host connection: {e}"))?;
        let connection = local.block_on(&runtime, async {
            let stream = tokio::net::UnixStream::from_std(stream)
                .map_err(|e| format!("app-host connection: {e}"))?;
            let (reader, writer) = stream.into_split();
            let connection = Rc::new(Connection {
                writer: tokio::sync::Mutex::new(writer),
                waiting: RefCell::default(),
                next: Cell::new(0),
                closed: Cell::new(false),
            });
            tokio::task::spawn_local(Rc::clone(&connection).read_replies(reader));
            Ok::<_, String>(connection)
        })?;
        Ok(Self {
            runtime,
            local,
            connection,
        })
    }

    fn wait(&self, request: Operation) -> Result<Answer, String> {
        self.local
            .block_on(&self.runtime, self.connection.call(request))
    }

    fn read(&self, resource: Resource) -> Result<Answer, String> {
        self.wait(Operation::Read(resource))
    }

    fn invoke(&self, action: Action) -> Result<Answer, String> {
        self.wait(Operation::Invoke(action))
    }

    /// Review immutable file bytes using the host's current trusted attestors.
    pub fn review_local(&self, bytes: &[u8]) -> Result<crate::local_package::LocalReview, String> {
        match self.read(Resource::LocalReview {
            tapp_bytes: bytes.to_vec(),
        })? {
            Answer::Review(json) => serde_json::from_str(&json).map_err(|e| e.to_string()),
            other => Err(unexpected(&other)),
        }
    }

    /// Install exactly the immutable archive/root previously reviewed.
    pub fn install_local(
        &self,
        bytes: &[u8],
        review: &crate::local_package::LocalReview,
    ) -> Result<PackageId, String> {
        installed(self.invoke(Action::InstallLocal {
            tapp_bytes: bytes.to_vec(),
            reviewed_root: review.root.clone(),
            reviewed_archive_hash: review.archive_hash.clone(),
        })?)
    }

    /// Install a verified package through the privileged host service.
    pub fn install(
        &self,
        bytes: &[u8],
        expected: &InstallExpectation,
    ) -> Result<PackageId, String> {
        installed(self.invoke(Action::Install {
            tapp_bytes: bytes.to_vec(),
            pinning: WirePinning::Tried,
            expected: Expected::from(expected),
        })?)
    }

    /// Install the exact signed catalogue offer through the shared installer.
    pub fn install_offer(
        &self,
        registry: &str,
        expected: &InstallExpectation,
    ) -> Result<PackageId, String> {
        installed(self.invoke(Action::InstallOffer {
            registry: registry.to_owned(),
            expected: Expected::from(expected),
        })?)
    }

    /// List the packages the host service can launch: the user's, and the
    /// read-only system packages they do not shadow.
    ///
    /// # Errors
    ///
    /// Returns an error when the service request or an installed manifest cannot be read.
    pub fn list(&self) -> Result<Vec<ListedPackage>, String> {
        self.complete(self.begin_list())
    }

    /// Launch an installed package.
    ///
    /// # Errors
    ///
    /// Returns an error when the host service cannot launch the package.
    pub fn launch(&self, id: PackageId) -> Result<String, String> {
        text(self.invoke(Action::Launch { id: *id.as_bytes() })?)
    }

    /// Update the retention pin for an installed package.
    ///
    /// # Errors
    ///
    /// Returns an error when the host service cannot update the package pin.
    pub fn pin(&self, id: PackageId, pinned: bool) -> Result<(), String> {
        self.invoke(Action::SetPinned {
            id: *id.as_bytes(),
            pinned,
        })
        .map(drop)
    }

    /// Remove an installed package.
    ///
    /// # Errors
    ///
    /// Returns an error when the host service cannot remove the package.
    pub fn remove(&self, id: PackageId) -> Result<(), String> {
        self.invoke(Action::Remove { id: *id.as_bytes() }).map(drop)
    }

    /// Launch an installed desktop application.
    ///
    /// # Errors
    ///
    /// Returns an error when the host service cannot launch the desktop application.
    pub fn launch_desktop(&self, desktop_id: &str) -> Result<String, String> {
        text(self.invoke(Action::LaunchDesktop {
            desktop_id: desktop_id.to_owned(),
        })?)
    }

    /// Run a Flatpak lifecycle operation through the host service.
    ///
    /// # Errors
    ///
    /// Returns an error when the host service rejects or cannot run the operation.
    pub fn flatpak(
        &self,
        action: FlatpakAction,
        app_id: &str,
        reference: Option<&[u8]>,
    ) -> Result<String, String> {
        text(self.invoke(Action::Flatpak {
            verb: match action {
                FlatpakAction::Install => FlatpakVerb::Install,
                FlatpakAction::Update => FlatpakVerb::Update,
                FlatpakAction::Remove => FlatpakVerb::Remove,
            },
            app_id: app_id.to_owned(),
            reference: reference.unwrap_or_default().to_vec(),
        })?)
    }

    /// The person's app repositories, as `app repo list --json` prints them.
    ///
    /// # Errors
    ///
    /// Returns an error when the host service cannot read them.
    pub fn repositories(&self) -> Result<String, String> {
        self.complete(self.begin_repositories())
    }

    /// Read verified metadata from an enabled repository configured by this person.
    /// The host retains the durable antirollback checkpoint outside the app sandbox.
    ///
    /// # Errors
    /// Returns why the configured repository cannot be verified or read.
    pub fn registry_catalogue(&self, registry: &str) -> Result<String, CatalogueError> {
        self.complete(self.begin_registry_catalogue(registry))
    }

    /// Resolve a signed offer providing exactly this runtime, without
    /// installation, as its JSON catalogue entry (`null` when none is offered).
    /// The caller owns parsing, so this client stays free of the installer.
    pub fn runtime_offer(&self, registry: &str, runtime: &str) -> Result<String, String> {
        let json = text(self.read(Resource::RuntimeOffer {
            registry: registry.to_owned(),
            runtime: runtime.to_owned(),
        })?)?;
        if json.len() > 256 * 1024 {
            return Err("runtime offer exceeds the response limit".into());
        }
        Ok(json)
    }

    /// The verified metadata the host last accepted from an enabled
    /// repository, without the network. Fails when there is none, or when it
    /// no longer verifies (it expired, or a newer one was accepted since).
    pub fn last_verified_catalogue(&self, registry: &str) -> Result<String, CatalogueError> {
        self.complete(self.begin_last_verified_catalogue(registry))
    }

    /// Read cached app metadata from one configured Flatpak repository.
    ///
    /// # Errors
    /// Returns why the repository cannot be read on this device.
    pub fn flatpak_catalogue(
        &self,
        remote: &str,
        user_managed: bool,
    ) -> Result<String, CatalogueError> {
        self.complete(self.begin_flatpak_catalogue(remote, user_managed))
    }

    /// Install from a configured repository belonging to this account.
    pub fn flatpak_install_remote(
        &self,
        remote: &str,
        app_id: &str,
        branch: &str,
    ) -> Result<String, String> {
        text(self.invoke(Action::FlatpakInstallRemote {
            remote: remote.to_owned(),
            app_id: app_id.to_owned(),
            branch: branch.to_owned(),
        })?)
    }

    /// Read host Flatpak state without executing tools in the app sandbox.
    ///
    /// # Errors
    /// Returns why the host installation cannot be read.
    pub fn flatpak_status(&self) -> Result<FlatpakStatus, String> {
        self.complete(self.begin_flatpak_status())
    }

    /// Enable or disable an existing repository in this account's scope.
    pub fn set_remote_enabled(&self, name: &str, enabled: bool) -> Result<String, String> {
        text(self.invoke(Action::Remote {
            verb: if enabled {
                RemoteVerb::Enable
            } else {
                RemoteVerb::Disable
            },
            name: name.to_owned(),
            url: String::new(),
        })?)
    }

    /// Add or remove a user Flatpak remote through the host service.
    ///
    /// # Errors
    ///
    /// Returns an error when the host service rejects or cannot run the operation.
    pub fn remote(&self, add: bool, name: &str, url: Option<&str>) -> Result<String, String> {
        text(self.invoke(Action::Remote {
            verb: if add {
                RemoteVerb::Add
            } else {
                RemoteVerb::Remove
            },
            name: name.to_owned(),
            url: url.unwrap_or("").to_owned(),
        })?)
    }
}

impl From<&InstallExpectation> for Expected {
    fn from(expected: &InstallExpectation) -> Self {
        Self {
            root: *expected.package_root.as_bytes(),
            publisher: expected.publisher,
            app_id: expected.app_id.clone(),
            version: expected.version.clone(),
            size_bytes: expected.size_bytes,
        }
    }
}

fn unexpected(answer: &Answer) -> String {
    format!("app-host answered {answer:?}")
}

fn installed(answer: Answer) -> Result<PackageId, String> {
    match answer {
        Answer::Installed(id) => Ok(PackageId::from_bytes(id)),
        other => Err(unexpected(&other)),
    }
}

fn text(answer: Answer) -> Result<String, String> {
    match answer {
        Answer::Text(text) => Ok(text),
        other => Err(unexpected(&other)),
    }
}

fn catalogue(answer: Result<Answer, String>) -> Result<String, CatalogueError> {
    match answer? {
        Answer::Catalogue(Catalogue::Rows(rows)) => Ok(rows),
        Answer::Catalogue(Catalogue::Refused { refusal, detail }) => {
            Err(CatalogueError::Refused { refusal, detail })
        }
        other => Err(CatalogueError::Failed(unexpected(&other))),
    }
}
