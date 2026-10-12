//! Typed client for the launcher-issued `app-host` capability.
//!
//! The Store worker is synchronous, so this wrapper owns a current-thread
//! runtime and `LocalSet` while keeping one Cap'n Proto connection alive for
//! its whole lifetime. It never reaches for a host path or executable.

use crate::app_host_capnp::{
    FlatpakAction as WireFlatpakAction, Pinning as WirePinning, RemoteAction as WireRemoteAction,
    app_host,
};
use crate::install::InstallExpectation;

mod pending;
use package_store::InstalledPackage;
pub use pending::PendingRead;
use tilo_app_package::PackageId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlatpakAction {
    Install,
    Update,
    Remove,
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
    ///
    /// # Errors
    /// Returns why the runtime or supplied connection could not be opened.
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

    /// Review immutable file bytes using the host's current trusted attestors.
    ///
    /// # Errors
    /// Returns why the host refused the local package.
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
    ///
    /// # Errors
    /// Returns current trust, payload, runtime or local update policy refusal.
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
    ///
    /// # Errors
    ///
    /// Returns an error when the service rejects the package or the response is malformed.
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
    ///
    /// # Errors
    /// Returns why the selected offer, runtime or installation was refused.
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
    pub fn list(&self) -> Result<Vec<InstalledPackage>, String> {
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
    pub fn registry_catalogue(&self, registry: &str) -> Result<String, String> {
        self.complete(self.begin_registry_catalogue(registry))
    }

    /// Read cached app metadata from one configured Flatpak repository.
    ///
    /// # Errors
    /// Returns why the repository cannot be read on this device.
    pub fn flatpak_catalogue(&self, remote: &str, user_managed: bool) -> Result<String, String> {
        self.complete(self.begin_flatpak_catalogue(remote, user_managed))
    }

    /// Install from a configured repository belonging to this account.
    ///
    /// # Errors
    /// Returns why the repository or installation was refused.
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
    ///
    /// # Errors
    /// Returns why the repository change was refused or failed.
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
