//! Typed client for the launcher-issued `package-service` connection. Apps are synchronous, so this
//! wrapper owns a runtime and blocks on each call while keeping one connection for its whole
//! lifetime. Calls carry ids, so a reader task answers any of them and several app threads may
//! use the client at once: a long install never holds up a list. It never reaches for a path or
//! executable outside the app.

#[cfg(unix)]
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(unix)]
use std::sync::{Mutex, PoisonError};

use crate::InstallExpectation;
use crate::wire::{
    Action, Answer, Catalogue, Event, Expected, FlatpakVerb, OfferedLanguage, Operation,
    RemoteVerb, Resource, WirePinning,
};
#[cfg(unix)]
use crate::wire::{Call, Frame, Reply};

mod pending;
use package_store::InstalledPackage;
pub use pending::PendingRead;
use tilo_app_package::PackageId;

/// Why the service refused a catalogue read. It travels typed on the wire, so a reworded detail never
/// changes how the Store handles it.
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
    /// The service refused it for a reason the Store explains.
    Refused {
        refusal: Refusal,
        /// For logs; never decides anything.
        detail: String,
    },
    /// Anything else: the transport, the service, the repository.
    Failed(String),
}

impl CatalogueError {
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

/// One installed package as the service lists it: its store row, and what its signed manifest says to
/// show a person about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedPackage {
    pub package: InstalledPackage,
    pub about: PackageAbout,
}

/// The manifest's `[package]` display metadata. It is installed and signed, so it describes the app
/// whether or not any repository is reachable.
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

    /// The `[package]` display metadata and declared capabilities of a whole manifest.
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

    /// The tag line to show in `locale` (BCP 47): its tag, else its language, else English
    /// ([`tilo_app_package::manifest::text_in`]); empty when the app declares none.
    #[must_use]
    pub fn tagline_for(&self, locale: &str) -> &str {
        tilo_app_package::manifest::text_in(&self.taglines, locale).unwrap_or_default()
    }

    /// The name to show in `locale` (BCP 47): its tag, else its language, else [`Self::name`].
    #[must_use]
    pub fn name_for(&self, locale: &str) -> &str {
        tilo_app_package::manifest::text_in(&self.names, locale).unwrap_or(&self.name)
    }
}

/// Flatpak availability and machine-readable installation/remote rows.
#[derive(Clone, Debug, Default)]
pub struct FlatpakStatus {
    pub available: bool,
    pub installed: String,
    pub remotes: String,
}

/// One call in flight: where its progress goes, and where its answer does.
#[cfg(unix)]
struct Waiting {
    /// Where the call's progress goes. It is called on the reader task, so it
    /// must not call back into the client.
    progress: Box<dyn FnMut(Event) + Send>,
    /// The call's answer, once it arrives.
    answer: Arc<Mutex<Option<Result<Answer, String>>>>,
    /// Wakes the caller when `answer` is set.
    answered: Arc<tokio::sync::Notify>,
}

/// The calls in flight on one connection, answered by its reader task.
#[cfg(unix)]
struct Connection {
    writer: tokio::sync::Mutex<tokio::net::unix::OwnedWriteHalf>,
    waiting: Mutex<HashMap<u64, Waiting>>,
    next: AtomicU64,
    closed: AtomicBool,
}

#[cfg(unix)]
impl Connection {
    fn call<F>(
        self: &Arc<Self>,
        request: Operation,
        progress: F,
    ) -> impl Future<Output = Result<Answer, String>> + use<F>
    where
        F: FnMut(Event) + Send + 'static,
    {
        let connection = Arc::clone(self);
        async move {
            if connection.closed.load(Ordering::SeqCst) {
                return Err(DISCONNECTED.to_owned());
            }
            let id = connection.next.fetch_add(1, Ordering::SeqCst);
            let answer = Arc::new(Mutex::new(None));
            let answered = Arc::new(tokio::sync::Notify::new());
            waiting(&connection).insert(
                id,
                Waiting {
                    progress: Box::new(progress),
                    answer: Arc::clone(&answer),
                    answered: Arc::clone(&answered),
                },
            );
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
                    waiting(&connection).remove(&id);
                    return Err(format!("package-service: {error}"));
                }
            }
            loop {
                // Registered before the answer is read, so one that lands in
                // between is never missed.
                let notified = answered.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let landed = answer.lock().unwrap_or_else(PoisonError::into_inner).take();
                if let Some(answer) = landed {
                    return answer;
                }
                notified.await;
            }
        }
    }

    async fn read_replies(self: Arc<Self>, mut reader: tokio::net::unix::OwnedReadHalf) {
        while let Ok(Some(frame)) = crate::wire::FRAMING
            .read_value_async::<_, Frame>(&mut reader)
            .await
        {
            match frame {
                Frame::Reply(Reply { id, answer }) => {
                    let answered = waiting(&self).remove(&id);
                    if let Some(call) = answered {
                        *call.answer.lock().unwrap_or_else(PoisonError::into_inner) = Some(answer);
                        call.answered.notify_waiters();
                    }
                }
                Frame::Event(event) => {
                    // The callback is called without the calls held, so it can
                    // take its time without holding up another call's answer.
                    let id = event.id;
                    let Some(mut call) = waiting(&self).remove(&id) else {
                        continue;
                    };
                    (call.progress)(event);
                    waiting(&self).insert(id, call);
                }
            }
        }
        self.closed.store(true, Ordering::SeqCst);
        let hanging: Vec<Waiting> = waiting(&self).drain().map(|(_, call)| call).collect();
        for call in hanging {
            *call.answer.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(Err(DISCONNECTED.to_owned()));
            call.answered.notify_waiters();
        }
    }
}

#[cfg(unix)]
fn waiting(connection: &Connection) -> std::sync::MutexGuard<'_, HashMap<u64, Waiting>> {
    connection
        .waiting
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

#[cfg(unix)]
const DISCONNECTED: &str = "the package service connection has ended";

/// No transport carries the service off unix, so no client is ever made
/// there. The type exists on every target, so its callers select none.
#[cfg(not(unix))]
struct Connection(std::convert::Infallible);

#[cfg(not(unix))]
impl Connection {
    fn call<F>(
        self: &Arc<Self>,
        _request: Operation,
        _progress: F,
    ) -> impl Future<Output = Result<Answer, String>> + use<F>
    where
        F: FnMut(Event) + Send + 'static,
    {
        let never = self.0;
        async move { match never {} }
    }
}

pub struct BlockingClient {
    runtime: tokio::runtime::Runtime,
    connection: Arc<Connection>,
}

impl BlockingClient {
    /// Claim the pre-connected descriptor the launcher granted to this app.
    #[cfg(unix)]
    pub fn from_service() -> Result<Self, String> {
        use std::os::fd::FromRawFd as _;
        use std::sync::atomic::{AtomicBool, Ordering};
        // The launcher hands the process one connection; a second owner would
        // interleave frames and close the descriptor twice.
        static CLAIMED: AtomicBool = AtomicBool::new(false);
        let fd = runtime_portal::service_fd(crate::SERVICE)
            .ok_or_else(|| "the package service was not handed to this app".to_owned())?;
        if CLAIMED.swap(true, Ordering::SeqCst) {
            return Err("the package service connection is already in use".to_owned());
        }
        // SAFETY: `service_fd` verified the descriptor is open, the launcher
        // gave it to this process alone, and `CLAIMED` makes this its one owner.
        Self::from_connected_fd(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
    }

    /// No transport carries the service off unix, so no session hands one off there: every
    /// caller sees the reason instead of selecting the call by target.
    #[cfg(not(unix))]
    pub fn from_service() -> Result<Self, String> {
        Err("the package service needs a session this platform has none of".to_owned())
    }

    /// Open an explicitly owned connection (a test service).
    #[cfg(unix)]
    pub fn from_connected_fd(fd: std::os::fd::OwnedFd) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|e| format!("create package-service runtime: {e}"))?;
        let stream = std::os::unix::net::UnixStream::from(fd);
        stream
            .set_nonblocking(true)
            .map_err(|e| format!("package-service connection: {e}"))?;
        let (reader, writer) = {
            let _entered = runtime.enter();
            let stream = tokio::net::UnixStream::from_std(stream)
                .map_err(|e| format!("package-service connection: {e}"))?;
            stream.into_split()
        };
        let connection = Arc::new(Connection {
            writer: tokio::sync::Mutex::new(writer),
            waiting: Mutex::default(),
            next: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        });
        runtime.spawn(Arc::clone(&connection).read_replies(reader));
        Ok(Self {
            runtime,
            connection,
        })
    }

    fn wait(&self, request: Operation) -> Result<Answer, String> {
        self.wait_progress(request, |_| {})
    }

    fn wait_progress(
        &self,
        request: Operation,
        progress: impl FnMut(Event) + Send + 'static,
    ) -> Result<Answer, String> {
        self.runtime
            .block_on(self.connection.call(request, progress))
    }

    fn read(&self, resource: Resource) -> Result<Answer, String> {
        self.wait(Operation::Read(resource))
    }

    pub(crate) fn invoke(&self, action: Action) -> Result<Answer, String> {
        self.wait(Operation::Invoke(action))
    }

    /// Review immutable file bytes using the service's current trusted attestors.
    pub fn review_local(&self, bytes: &[u8]) -> Result<crate::LocalReview, String> {
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
        review: &crate::LocalReview,
    ) -> Result<PackageId, String> {
        installed(self.invoke(Action::InstallLocal {
            tapp_bytes: bytes.to_vec(),
            reviewed_root: review.root.clone(),
            reviewed_archive_hash: review.archive_hash.clone(),
        })?)
    }

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

    /// The packages the service can launch: the user's, and the read-only system packages they do not
    /// shadow.
    pub fn list(&self) -> Result<Vec<ListedPackage>, String> {
        self.complete(self.begin_list())
    }

    pub fn launch(&self, id: PackageId) -> Result<String, String> {
        text(self.invoke(Action::Launch { id: *id.as_bytes() })?)
    }

    pub fn pin(&self, id: PackageId, pinned: bool) -> Result<(), String> {
        self.invoke(Action::SetPinned {
            id: *id.as_bytes(),
            pinned,
        })
        .map(drop)
    }

    pub fn remove(&self, id: PackageId) -> Result<(), String> {
        self.invoke(Action::Remove { id: *id.as_bytes() }).map(drop)
    }

    /// Run a Flatpak lifecycle operation through the service.
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
    pub fn repositories(&self) -> Result<String, String> {
        self.complete(self.begin_repositories())
    }

    /// Read verified metadata from an enabled repository configured by this person.
    /// The service retains the durable antirollback checkpoint outside the app sandbox.
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

    /// The verified metadata the service last accepted from an enabled
    /// repository, without the network. Fails when there is none, or when it
    /// no longer verifies (it expired, or a newer one was accepted since).
    pub fn last_verified_catalogue(&self, registry: &str) -> Result<String, CatalogueError> {
        self.complete(self.begin_last_verified_catalogue(registry))
    }

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

    /// Read Flatpak state without executing tools in the app sandbox.
    pub fn flatpak_status(&self) -> Result<FlatpakStatus, String> {
        self.complete(self.begin_flatpak_status())
    }

    /// The language packs the enabled repositories offer, and whether each is
    /// installed. Settings only.
    pub fn language_packs(&self) -> Result<Vec<OfferedLanguage>, String> {
        match self.read(Resource::LanguagePacks)? {
            Answer::Languages(offered) => Ok(offered),
            other => Err(unexpected(&other)),
        }
    }

    /// Make the language pack of `locale` installed, calling `progress` as it
    /// goes; returns once it is. Settings only.
    pub fn install_language_pack(
        &self,
        locale: &str,
        progress: impl FnMut(Event) + Send + 'static,
    ) -> Result<(), String> {
        match self.wait_progress(
            Operation::Invoke(Action::InstallLanguagePack {
                locale: locale.to_owned(),
            }),
            progress,
        )? {
            Answer::Done => Ok(()),
            other => Err(unexpected(&other)),
        }
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

    /// Add or remove a user Flatpak remote through the service.
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
    format!("package-service answered {answer:?}")
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
