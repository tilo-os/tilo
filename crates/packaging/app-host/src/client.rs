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
use tilo_app_package::PackageId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlatpakAction {
    Install,
    Update,
    Remove,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledPackage {
    pub package_root: PackageId,
    pub app_id: String,
    pub version: String,
    pub pinned: bool,
    /// True for packages in the per-user mutable store. System packages are
    /// read-only but remain launchable through the same capability.
    pub user_managed: bool,
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
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("create app-host runtime: {e}"))?;
        let local = tokio::task::LocalSet::new();
        let client = local.block_on(&runtime, async {
            runtime_portal::try_open_service::<app_host::Client>("app-host")
                .map_err(|e| e.to_string())
        })?;
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

    /// List packages installed by the privileged host service.
    ///
    /// # Errors
    ///
    /// Returns an error when the service request or an installed manifest cannot be read.
    pub fn list(&self) -> Result<Vec<InstalledPackage>, String> {
        let response = self.wait(self.client.list_request().send().promise)?;
        let result = response.get().map_err(|e| e.to_string())?;
        let entries = result.get_entries().map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(entries.len() as usize);
        for entry in entries {
            let manifest_text = entry
                .get_manifest_text()
                .map_err(|e| e.to_string())?
                .to_string()
                .map_err(|e| e.to_string())?;
            let manifest = tilo_app_package::Manifest::from_toml_str(&manifest_text)
                .map_err(|e| format!("installed manifest: {e}"))?;
            out.push(InstalledPackage {
                package_root: parse_id(entry.get_id().map_err(|e| e.to_string())?)?,
                app_id: manifest.package.id,
                version: manifest.package.version,
                pinned: entry.get_pinning().map_err(|e| e.to_string())? == WirePinning::Pinned,
                user_managed: entry.get_user_managed(),
            });
        }
        Ok(out)
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
