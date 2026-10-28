//! Typed client for the launcher-issued `app-host` capability.
//!
//! The Store worker is synchronous, so this wrapper owns a current-thread
//! runtime and `LocalSet` while keeping one Cap'n Proto connection alive for
//! its whole lifetime. It never reaches for a host path or executable.

pub use crate::app_host_capnp::CatalogueRefusal as WireCatalogueRefusal;
use crate::app_host_capnp::{
    FlatpakAction as WireFlatpakAction, Pinning as WirePinning, RemoteAction as WireRemoteAction,
    app_host,
};
use crate::install::InstallExpectation;

mod pending;
use package_store::InstalledPackage;
pub use pending::PendingRead;
use tilo_app_package::PackageId;

/// Why the host refused a catalogue read: the one thing the Store decides
/// what to say from. It travels typed on the wire (`CatalogueRefusal`), so a
/// reworded detail never changes how the Store handles it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

impl Refusal {
    /// This refusal as the wire carries it.
    #[must_use]
    pub fn to_wire(self) -> WireCatalogueRefusal {
        match self {
            Self::SystemUpdateNeeded => WireCatalogueRefusal::SystemUpdateNeeded,
            Self::Expired => WireCatalogueRefusal::Expired,
            Self::TimedOut => WireCatalogueRefusal::TimedOut,
            Self::DeviceRepositoryStale => WireCatalogueRefusal::DeviceRepositoryStale,
        }
    }

    /// The refusal the wire names; `None` for `none`.
    #[must_use]
    pub fn from_wire(wire: WireCatalogueRefusal) -> Option<Self> {
        match wire {
            WireCatalogueRefusal::None => None,
            WireCatalogueRefusal::SystemUpdateNeeded => Some(Self::SystemUpdateNeeded),
            WireCatalogueRefusal::Expired => Some(Self::Expired),
            WireCatalogueRefusal::TimedOut => Some(Self::TimedOut),
            WireCatalogueRefusal::DeviceRepositoryStale => Some(Self::DeviceRepositoryStale),
        }
    }
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

/// The refusal a catalogue read's results name, if any.
fn catalogue_refusal(
    refusal: Result<WireCatalogueRefusal, capnp::NotInSchema>,
    detail: Result<capnp::text::Reader<'_>, capnp::Error>,
) -> Option<CatalogueError> {
    // A refusal this client does not know is a host from another runtime
    // version, which the interface check keeps from happening; read it as
    // a failure rather than as rows.
    let refusal = match refusal {
        Ok(wire) => Refusal::from_wire(wire)?,
        Err(unknown) => {
            return Some(CatalogueError::Failed(format!(
                "the app host refused the read for a reason this Store does not know ({unknown})"
            )));
        }
    };
    Some(CatalogueError::Refused {
        refusal,
        detail: text_result(detail).unwrap_or_default(),
    })
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

pub struct BlockingClient {
    runtime: tokio::runtime::Runtime,
    local: tokio::task::LocalSet,
    client: app_host::Client,
}

impl BlockingClient {
    /// Claim the pre-connected descriptor the launcher granted to this app.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime or launcher-issued service cannot be opened.
    pub fn from_service() -> Result<Self, String> {
        Self::connect(|| {
            runtime_portal::try_open_service::<app_host::Client>("app-host")
                .map_err(|error| error.to_string())
        })
    }

    /// Open an explicitly owned connected capability in a test host.
    #[cfg(all(unix, feature = "test-support"))]
    pub fn from_connected_fd(fd: std::os::fd::OwnedFd) -> Result<Self, String> {
        use std::os::fd::IntoRawFd as _;
        Self::connect(move || {
            runtime_portal::open_service_on_fd::<app_host::Client>(fd.into_raw_fd())
                .map_err(|error| error.to_string())
        })
    }

    fn connect(open: impl FnOnce() -> Result<app_host::Client, String>) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("create app-host runtime: {e}"))?;
        let local = tokio::task::LocalSet::new();
        let client = local.block_on(&runtime, async { open() })?;
        Ok(Self {
            runtime,
            local,
            client,
        })
    }

    fn wait<T>(
        &self,
        future: impl core::future::Future<Output = Result<T, capnp::Error>>,
    ) -> Result<T, String> {
        self.local
            .block_on(&self.runtime, future)
            .map_err(|e| e.to_string())
    }

    /// Inspect an installed app's exact optional runtime without fetching payload.
    pub fn inspect_optional_runtime(
        &self,
        app_id: &str,
        root: PackageId,
        runtime: &str,
    ) -> Result<crate::runtime_transaction::RuntimeReview, String> {
        let mut request = self.client.inspect_optional_runtime_request();
        request.get().set_app_id(app_id);
        request.get().set_consumer_root(root.as_bytes());
        request.get().set_runtime(runtime);
        let response = self.wait(request.send().promise)?;
        let json = text_result(response.get().map_err(|e| e.to_string())?.get_review_json())?;
        serde_json::from_str(&json).map_err(|e| e.to_string())
    }

    /// Acquire the exact displayed review after explicit consent. The call
    /// completes only after verified installation. Disconnect is not rollback.
    pub fn acquire_optional_runtime(
        &self,
        review: &crate::runtime_transaction::RuntimeReview,
    ) -> Result<(), String> {
        self.acquire_optional_runtime_reporting(review, |_| {})
    }

    /// Acquire with connection-owned, best-effort real download/install progress.
    /// Disconnecting the observer does not cancel or roll back installation.
    pub fn acquire_optional_runtime_reporting(
        &self,
        review: &crate::runtime_transaction::RuntimeReview,
        progress: impl FnMut(crate::runtime_transaction::RuntimeProgress) + 'static,
    ) -> Result<(), String> {
        let observer: crate::app_host_capnp::runtime_progress::Client = capnp_rpc::new_client(
            ProgressObserver(std::cell::RefCell::new(Box::new(progress))),
        );
        let mut request = self.client.acquire_optional_runtime_request();
        request.get().set_progress(observer);
        request
            .get()
            .set_review_json(serde_json::to_string(review).map_err(|e| e.to_string())?);
        self.wait(request.send().promise)?;
        Ok(())
    }

    /// Review immutable file bytes using the host's current trusted attestors.
    pub fn review_local(&self, bytes: &[u8]) -> Result<crate::local_package::LocalReview, String> {
        let mut request = self.client.review_local_request();
        request.get().set_tapp_bytes(bytes);
        let response = self.wait(request.send().promise)?;
        let result = response.get().map_err(|e| e.to_string())?;
        let json = result
            .get_review_json()
            .map_err(|e| e.to_string())?
            .to_str()
            .map_err(|e| e.to_string())?;
        serde_json::from_str(json).map_err(|e| e.to_string())
    }

    /// Install exactly the immutable archive/root previously reviewed.
    pub fn install_local(
        &self,
        bytes: &[u8],
        review: &crate::local_package::LocalReview,
    ) -> Result<PackageId, String> {
        let mut request = self.client.install_local_request();
        let mut p = request.get();
        p.set_tapp_bytes(bytes);
        p.set_reviewed_root(&review.root);
        p.set_reviewed_archive_hash(&review.archive_hash);
        let response = self.wait(request.send().promise)?;
        let result = response.get().map_err(|e| e.to_string())?;
        parse_id(result.get_id().map_err(|e| e.to_string())?)
    }

    /// Install a verified package through the privileged host service.
    pub fn install(
        &self,
        bytes: &[u8],
        expected: &InstallExpectation,
    ) -> Result<PackageId, String> {
        let mut request = self.client.install_request();
        {
            let mut p = request.get();
            p.set_tapp_bytes(bytes);
            p.set_pinning(WirePinning::Tried);
            p.set_expected_root(expected.package_root.as_bytes());
            p.set_expected_publisher(&expected.publisher);
            p.set_expected_app_id(&expected.app_id);
            p.set_expected_version(&expected.version);
            p.set_expected_size_bytes(expected.size_bytes);
        }
        let response = self.wait(request.send().promise)?;
        let bytes = response
            .get()
            .map_err(|e| e.to_string())?
            .get_id()
            .map_err(|e| e.to_string())?;
        parse_id(bytes)
    }

    /// Install the exact signed catalogue offer through the shared installer.
    pub fn install_offer(
        &self,
        registry: &str,
        expected: &InstallExpectation,
    ) -> Result<PackageId, String> {
        let mut request = self.client.install_offer_request();
        {
            let mut params = request.get();
            params.set_registry(registry);
            params.set_app_id(&expected.app_id);
            params.set_version(&expected.version);
            params.set_expected_root(expected.package_root.as_bytes());
            params.set_expected_publisher(&expected.publisher);
            params.set_expected_size_bytes(expected.size_bytes);
        }
        let response = self.wait(request.send().promise)?;
        parse_id(
            response
                .get()
                .map_err(|error| error.to_string())?
                .get_id()
                .map_err(|error| error.to_string())?,
        )
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
        let mut request = self.client.launch_request();
        request.get().set_id(id.as_bytes());
        text_result(
            self.wait(request.send().promise)?
                .get()
                .map_err(|e| e.to_string())?
                .get_status(),
        )
    }

    /// Update the retention pin for an installed package.
    ///
    /// # Errors
    ///
    /// Returns an error when the host service cannot update the package pin.
    pub fn pin(&self, id: PackageId, pinned: bool) -> Result<(), String> {
        if pinned {
            let mut request = self.client.pin_request();
            request.get().set_id(id.as_bytes());
            self.wait(request.send().promise)?;
        } else {
            let mut request = self.client.unpin_request();
            request.get().set_id(id.as_bytes());
            self.wait(request.send().promise)?;
        }
        Ok(())
    }

    /// Remove an installed package.
    ///
    /// # Errors
    ///
    /// Returns an error when the host service cannot remove the package.
    pub fn remove(&self, id: PackageId) -> Result<(), String> {
        let mut request = self.client.remove_request();
        request.get().set_id(id.as_bytes());
        self.wait(request.send().promise)?;
        Ok(())
    }

    /// Launch an installed desktop application.
    ///
    /// # Errors
    ///
    /// Returns an error when the host service cannot launch the desktop application.
    pub fn launch_desktop(&self, desktop_id: &str) -> Result<String, String> {
        let mut request = self.client.launch_desktop_request();
        request.get().set_desktop_id(desktop_id);
        text_result(
            self.wait(request.send().promise)?
                .get()
                .map_err(|e| e.to_string())?
                .get_status(),
        )
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
        let mut request = self.client.flatpak_request();
        {
            let mut p = request.get();
            p.set_action(match action {
                FlatpakAction::Install => WireFlatpakAction::Install,
                FlatpakAction::Update => WireFlatpakAction::Update,
                FlatpakAction::Remove => WireFlatpakAction::Remove,
            });
            p.set_app_id(app_id);
            p.set_reference(reference.unwrap_or_default());
        }
        text_result(
            self.wait(request.send().promise)?
                .get()
                .map_err(|e| e.to_string())?
                .get_status(),
        )
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
        let mut request = self.client.runtime_offer_request();
        request.get().set_registry(registry);
        request.get().set_runtime(runtime);
        let response = self.wait(request.send().promise)?;
        let json = response
            .get()
            .map_err(|error| error.to_string())?
            .get_offer()
            .map_err(|error| error.to_string())?
            .to_string()
            .map_err(|error| error.to_string())?;
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
        let mut request = self.client.flatpak_install_remote_request();
        request.get().set_remote(remote);
        request.get().set_app_id(app_id);
        request.get().set_branch(branch);
        let response = self.wait(request.send().promise)?;
        response
            .get()
            .map_err(|e| e.to_string())?
            .get_status()
            .map_err(|e| e.to_string())?
            .to_string()
            .map_err(|e| e.to_string())
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
        let mut request = self.client.remote_request();
        request.get().set_action(if enabled {
            WireRemoteAction::Enable
        } else {
            WireRemoteAction::Disable
        });
        request.get().set_name(name);
        request.get().set_url("");
        text_result(
            self.wait(request.send().promise)?
                .get()
                .map_err(|error| error.to_string())?
                .get_status(),
        )
    }

    /// Add or remove a user Flatpak remote through the host service.
    ///
    /// # Errors
    ///
    /// Returns an error when the host service rejects or cannot run the operation.
    pub fn remote(&self, add: bool, name: &str, url: Option<&str>) -> Result<String, String> {
        let mut request = self.client.remote_request();
        {
            let mut p = request.get();
            p.set_action(if add {
                WireRemoteAction::Add
            } else {
                WireRemoteAction::Remove
            });
            p.set_name(name);
            p.set_url(url.unwrap_or(""));
        }
        text_result(
            self.wait(request.send().promise)?
                .get()
                .map_err(|e| e.to_string())?
                .get_status(),
        )
    }
}

type ProgressCallback = Box<dyn FnMut(crate::runtime_transaction::RuntimeProgress)>;
struct ProgressObserver(std::cell::RefCell<ProgressCallback>);

impl crate::app_host_capnp::runtime_progress::Server for ProgressObserver {
    fn update(
        self: capnp::capability::Rc<Self>,
        params: crate::app_host_capnp::runtime_progress::UpdateParams,
        _results: crate::app_host_capnp::runtime_progress::UpdateResults,
    ) -> impl core::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let result = params.get().map(|p| {
            (self.0.borrow_mut())(crate::runtime_transaction::RuntimeProgress {
                installing: p.get_installing(),
                received: p.get_received(),
                total: p.get_total_known().then(|| p.get_total()),
            });
        });
        std::future::ready(result)
    }
}

fn parse_id(bytes: &[u8]) -> Result<PackageId, String> {
    let raw: [u8; 32] = bytes
        .try_into()
        .map_err(|_| format!("app-host returned {}-byte package id", bytes.len()))?;
    Ok(PackageId::from_bytes(raw))
}

fn text_result(value: Result<capnp::text::Reader<'_>, capnp::Error>) -> Result<String, String> {
    value
        .map_err(|e| e.to_string())?
        .to_string()
        .map_err(|e| e.to_string())
}
