//! Cap'n Proto `AppHost::Server` implementation. Wires the RPC methods to
//! the device's [`PackageStore`] and its [`Catalog`]. The transport (Unix
//! socket listener) lives in `transport.rs`.
//!
//! capnp 0.26 generates async server methods on `capnp::capability::Rc<Self>`
//! returning an `impl Future<Output = Result<(), capnp::Error>>`; blocking
//! work (fs IO, content-store sessions, materialisation) is routed through
//! `tokio::task::spawn_blocking` so the capnp event loop stays responsive.

use std::sync::Arc;

use capnp::Error;

use crate::app_host_capnp::{
    FlatpakAction as WireFlatpakAction, Pinning as WirePinning, RemoteAction as WireRemoteAction,
    app_host,
};
use crate::install::{InstallExpectation, install_verified_tapp};
use package_store::{Catalog, InstalledPackage, PackageStore, Pinning};
use tilo_app_package::id::PackageId;

/// Service state shared across every accepted connection.
pub struct AppHostServer {
    store: Arc<PackageStore>,
    catalog: Arc<Catalog>,
    max_store_bytes: u64,
}

impl AppHostServer {
    /// Build a new server backed by `store`, listing what `catalog` (the
    /// same user store plus the image's preinstalled packages) can run.
    /// `max_store_bytes` is the soft cap used by the post-install sweep;
    /// pass `0` to disable it.
    pub fn new(store: Arc<PackageStore>, catalog: Arc<Catalog>, max_store_bytes: u64) -> Self {
        Self {
            store,
            catalog,
            max_store_bytes,
        }
    }
}

// -------- helpers ----------------------------------------------------

fn wire_pinning(p: Pinning) -> WirePinning {
    match p {
        Pinning::Tried => WirePinning::Tried,
        Pinning::Pinned => WirePinning::Pinned,
    }
}

fn parse_pinning(p: WirePinning) -> Pinning {
    match p {
        WirePinning::Tried => Pinning::Tried,
        WirePinning::Pinned => Pinning::Pinned,
    }
}

/// Decode raw 32-byte `Data` id into a [`PackageId`].
fn parse_id(bytes: &[u8]) -> Result<PackageId, Error> {
    let raw: [u8; 32] = bytes
        .try_into()
        .map_err(|_| Error::failed(format!("package id must be 32 bytes, got {}", bytes.len())))?;
    Ok(PackageId::from_bytes(raw))
}

fn failed(error: impl std::fmt::Display) -> Error {
    Error::failed(error.to_string())
}

/// A visible package with what the wire reports about it.
struct Listed {
    package: InstalledPackage,
    id: PackageId,
    manifest_text: String,
    size_bytes: u64,
}

fn listed(package: InstalledPackage) -> Option<Listed> {
    // A row without a signed package root has no identity to hand out.
    let id = package.package_id()?;
    let manifest_text = std::fs::read_to_string(&package.manifest_path).ok()?;
    let size_bytes = package_store::tree_size(&package.payload_root);
    Some(Listed {
        package,
        id,
        manifest_text,
        size_bytes,
    })
}

/// The visible package with signed root `id`.
fn find(catalog: &Catalog, id: PackageId) -> Result<InstalledPackage, Error> {
    catalog
        .find_package(id)
        .map_err(failed)?
        .ok_or_else(|| Error::failed(format!("unknown package {id}")))
}

/// The user-managed package with signed root `id`; system packages cannot
/// be pinned or removed here.
fn find_user(catalog: &Catalog, id: PackageId) -> Result<InstalledPackage, Error> {
    let package = find(catalog, id)?;
    if !package.user_managed() {
        return Err(Error::failed(format!(
            "{} is a system package and cannot be changed here",
            package.id
        )));
    }
    Ok(package)
}

/// Populate a wire `PackageEntry` builder from a listed package.
fn fill_entry(mut out: crate::app_host_capnp::package_entry::Builder<'_>, row: &Listed) {
    out.set_id(row.id.as_bytes());
    out.set_pinning(wire_pinning(row.package.pinning()));
    out.set_payload_root(row.package.payload_root.to_string_lossy());
    out.set_manifest_text(&row.manifest_text);
    out.set_size_bytes(row.size_bytes);
    out.set_last_used_unix(row.package.last_used);
    out.set_user_managed(row.package.user_managed());
    out.set_manifest_path(row.package.manifest_path.to_string_lossy());
}

/// Run blocking store work off the capnp event loop.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| Error::failed(format!("join: {e}")))?
}

/// The override the build's gate let through, else the image default.
fn path_or_default(
    allowed_override: Option<std::ffi::OsString>,
    default: &str,
) -> std::path::PathBuf {
    allowed_override.map_or_else(
        || std::path::PathBuf::from(default),
        std::path::PathBuf::from,
    )
}

/// The launcher that builds every app's sandbox. `TILO_LAUNCHER_BIN`
/// re-points it in developer builds only.
fn launcher_bin() -> std::path::PathBuf {
    path_or_default(
        crate::dev_env::dev_override(crate::dev_env::LAUNCHER_BIN_ENV),
        "/usr/bin/tilo-launcher",
    )
}

fn wayland_socket() -> Option<std::path::PathBuf> {
    let display = std::env::var_os("WAYLAND_DISPLAY")?;
    let display = std::path::PathBuf::from(display);
    if display.is_absolute() {
        Some(display)
    } else {
        xdg_paths::xdg::runtime_dir().map(|root| root.join(display))
    }
}

fn spawn_detached(mut command: std::process::Command) -> Result<String, Error> {
    command
        .spawn()
        .map(|child| format!("started process {}", child.id()))
        .map_err(|error| Error::failed(format!("launch failed: {error}")))
}

fn safe_host_diagnostic(bytes: &[u8]) -> String {
    const MAX_DIAGNOSTIC_BYTES: usize = 4096;
    // Invalid UTF-8 expands to a three-byte replacement character. Limit the
    // source window accordingly so even adversarial bytes stay under the RPC
    // response ceiling after lossy decoding.
    const MAX_SOURCE_BYTES: usize = MAX_DIAGNOSTIC_BYTES / 3;
    let start = bytes.len().saturating_sub(MAX_SOURCE_BYTES);
    String::from_utf8_lossy(bytes.get(start..).unwrap_or_default())
        .chars()
        .map(|character| {
            if character.is_control() && !matches!(character, '\n' | '\t') {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn run_host_command(command: std::process::Command) -> Result<String, Error> {
    let output = bounded_command::run_prepared(
        command,
        None,
        1024 * 1024,
        std::time::Duration::from_secs(1800),
    )
    .map_err(|error| Error::failed(format!("host command failed: {error}")))?;
    let mut text = safe_host_diagnostic(&output.stdout);
    if text.is_empty() {
        text = safe_host_diagnostic(&output.stderr);
    }
    if output.status.success() {
        Ok(text)
    } else {
        Err(Error::failed(format!(
            "host command exited {}: {text}",
            output.status.code().unwrap_or(1)
        )))
    }
}

// -------- Server impl -----------------------------------------------

// capnp-rpc capability methods are driven from a single-threaded
// `LocalSet`: `capnp::capability::Rc` (captured as `self`) is a
// non-atomic, non-`Send` reference count by capnp's own design, so
// these futures can never be `Send` and are never meant to cross a
// thread. Forcing `Send` here would use a different threading model.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc Server methods run on a single-threaded LocalSet via the non-atomic capnp::capability::Rc; the futures are never sent across threads"
)]
impl app_host::Server for AppHostServer {
    fn list(
        self: capnp::capability::Rc<Self>,
        _params: app_host::ListParams,
        mut results: app_host::ListResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let catalog = Arc::clone(&self.catalog);
        async move {
            let rows = blocking(move || {
                Ok(catalog
                    .list()
                    .map_err(failed)?
                    .into_iter()
                    .filter_map(listed)
                    .collect::<Vec<_>>())
            })
            .await?;
            let count = u32::try_from(rows.len())
                .map_err(|_| Error::failed("too many packages to list".to_string()))?;
            let mut list = results.get().init_entries(count);
            for (index, row) in (0..count).zip(rows.iter()) {
                fill_entry(list.reborrow().get(index), row);
            }
            Ok(())
        }
    }

    fn resolve(
        self: capnp::capability::Rc<Self>,
        params: app_host::ResolveParams,
        mut results: app_host::ResolveResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let catalog = Arc::clone(&self.catalog);
        async move {
            let id = parse_id(params.get().map_err(failed)?.get_id().map_err(failed)?)?;
            let row = blocking(move || {
                listed(find(&catalog, id)?)
                    .ok_or_else(|| Error::failed(format!("package {id} has no readable manifest")))
            })
            .await?;
            fill_entry(results.get().init_entry(), &row);
            Ok(())
        }
    }

    fn pin(
        self: capnp::capability::Rc<Self>,
        params: app_host::PinParams,
        _results: app_host::PinResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let (store, catalog) = (Arc::clone(&self.store), Arc::clone(&self.catalog));
        async move {
            let id = parse_id(params.get().map_err(failed)?.get_id().map_err(failed)?)?;
            blocking(move || {
                let package = find_user(&catalog, id)?;
                store.set_pinned(&package.id, true).map_err(failed)
            })
            .await
        }
    }

    fn unpin(
        self: capnp::capability::Rc<Self>,
        params: app_host::UnpinParams,
        _results: app_host::UnpinResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let (store, catalog) = (Arc::clone(&self.store), Arc::clone(&self.catalog));
        async move {
            let id = parse_id(params.get().map_err(failed)?.get_id().map_err(failed)?)?;
            blocking(move || {
                let package = find_user(&catalog, id)?;
                store.set_pinned(&package.id, false).map_err(failed)
            })
            .await
        }
    }

    fn remove(
        self: capnp::capability::Rc<Self>,
        params: app_host::RemoveParams,
        _results: app_host::RemoveResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let (store, catalog) = (Arc::clone(&self.store), Arc::clone(&self.catalog));
        async move {
            let id = parse_id(params.get().map_err(failed)?.get_id().map_err(failed)?)?;
            blocking(move || {
                let package = find_user(&catalog, id)?;
                store.remove(&package.id, false).map(drop).map_err(failed)
            })
            .await
        }
    }

    fn mark_used(
        self: capnp::capability::Rc<Self>,
        params: app_host::MarkUsedParams,
        _results: app_host::MarkUsedResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let (store, catalog) = (Arc::clone(&self.store), Arc::clone(&self.catalog));
        async move {
            let id = parse_id(params.get().map_err(failed)?.get_id().map_err(failed)?)?;
            blocking(move || {
                let package = find_user(&catalog, id)?;
                store.mark_used(&package.id).map_err(failed)
            })
            .await
        }
    }

    fn install(
        self: capnp::capability::Rc<Self>,
        params: app_host::InstallParams,
        mut results: app_host::InstallResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let store = Arc::clone(&self.store);
        let max_bytes = self.max_store_bytes;
        async move {
            let p = params.get().map_err(failed)?;
            let tapp_bytes = p.get_tapp_bytes().map_err(failed)?.to_vec();
            let pinning = parse_pinning(p.get_pinning().map_err(failed)?);
            let expected_root = parse_id(p.get_expected_root().map_err(failed)?)?;
            let publisher_bytes = p.get_expected_publisher().map_err(failed)?;
            let publisher: [u8; 32] = publisher_bytes.try_into().map_err(|_| {
                Error::failed(format!(
                    "publisher key must be 32 bytes, got {}",
                    publisher_bytes.len()
                ))
            })?;
            let expected = InstallExpectation {
                package_root: expected_root,
                publisher,
                app_id: p
                    .get_expected_app_id()
                    .map_err(failed)?
                    .to_string()
                    .map_err(failed)?,
                version: p
                    .get_expected_version()
                    .map_err(failed)?
                    .to_string()
                    .map_err(failed)?,
                size_bytes: p.get_expected_size_bytes(),
            };
            let id = blocking(move || {
                let installed =
                    install_verified_tapp(&store, &tapp_bytes, pinning, &expected, None)
                        .map_err(failed)?;
                // Best-effort post-install sweep. Errors here should not
                // fail the install — the package is already committed.
                if let Err(e) = store.sweep(max_bytes) {
                    tracing::warn!(error = %e, "post-install sweep failed");
                }
                installed
                    .package_id()
                    .ok_or_else(|| Error::failed("installed package has no package id".into()))
            })
            .await?;
            results.get().set_id(id.as_bytes());
            Ok(())
        }
    }

    fn disk_usage(
        self: capnp::capability::Rc<Self>,
        _params: app_host::DiskUsageParams,
        mut results: app_host::DiskUsageResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let store = Arc::clone(&self.store);
        async move {
            let usage = blocking(move || store.disk_usage().map_err(failed)).await?;
            let mut out = results.get();
            out.set_total_bytes(usage.total_bytes);
            let mut by = out.init_by_pinning();
            by.set_pinned_bytes(usage.pinned_bytes);
            by.set_tried_bytes(usage.tried_bytes);
            Ok(())
        }
    }

    fn launch(
        self: capnp::capability::Rc<Self>,
        params: app_host::LaunchParams,
        mut results: app_host::LaunchResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let (store, catalog) = (Arc::clone(&self.store), Arc::clone(&self.catalog));
        async move {
            let id = parse_id(params.get().map_err(failed)?.get_id().map_err(failed)?)?;
            let status = blocking(move || {
                let package = find(&catalog, id)?;
                let mut command = std::process::Command::new(launcher_bin());
                command
                    .arg("run")
                    .arg("--manifest")
                    .arg(&package.manifest_path)
                    .arg("--payload-root")
                    .arg(&package.payload_root);
                if let Some(socket) = wayland_socket() {
                    command.arg("--wayland-socket").arg(socket);
                }
                let status = spawn_detached(command)?;
                if package.user_managed() {
                    store.mark_used(&package.id).map_err(failed)?;
                }
                Ok(status)
            })
            .await?;
            results.get().set_status(&status);
            Ok(())
        }
    }

    #[allow(
        clippy::manual_async_fn,
        reason = "the generated Cap'n Proto Server trait requires an opaque Future return type"
    )]
    fn launch_desktop(
        self: capnp::capability::Rc<Self>,
        params: app_host::LaunchDesktopParams,
        mut results: app_host::LaunchDesktopResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        async move {
            let desktop_id = params
                .get()
                .map_err(|e| Error::failed(e.to_string()))?
                .get_desktop_id()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_string()
                .map_err(|e| Error::failed(e.to_string()))?;
            let status = tokio::task::spawn_blocking(move || {
                let mut command = std::process::Command::new(launcher_bin());
                command.arg("run-host").arg("--desktop-id").arg(desktop_id);
                if let Some(socket) = wayland_socket() {
                    command.arg("--wayland-socket").arg(socket);
                }
                spawn_detached(command)
            })
            .await
            .map_err(|e| Error::failed(format!("join: {e}")))??;
            results.get().set_status(&status);
            Ok(())
        }
    }

    #[allow(
        clippy::manual_async_fn,
        reason = "the generated Cap'n Proto Server trait requires an opaque Future return type"
    )]
    fn flatpak(
        self: capnp::capability::Rc<Self>,
        params: app_host::FlatpakParams,
        mut results: app_host::FlatpakResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        async move {
            let p = params.get().map_err(|e| Error::failed(e.to_string()))?;
            let action = p.get_action().map_err(|e| Error::failed(e.to_string()))?;
            let app_id = p
                .get_app_id()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_string()
                .map_err(|e| Error::failed(e.to_string()))?;
            let reference = p
                .get_reference()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_vec();
            if reference.len() > 64 * 1024 {
                return Err(Error::failed("Flatpak reference exceeds 64 KiB".into()));
            }
            let status = tokio::task::spawn_blocking(move || {
                let mut command = std::process::Command::new("flatpak");
                match action {
                    WireFlatpakAction::Install => {
                        command.args(["install", "--user", "--noninteractive"]);
                        if reference.is_empty() {
                            command.args(["flathub", &app_id]);
                        } else {
                            use std::io::Write as _;
                            let mut staged = tempfile::NamedTempFile::new().map_err(|e| {
                                Error::failed(format!("stage Flatpak reference: {e}"))
                            })?;
                            staged.write_all(&reference).map_err(|e| {
                                Error::failed(format!("stage Flatpak reference: {e}"))
                            })?;
                            command.arg(staged.path());
                            return run_host_command(command);
                        }
                    }
                    WireFlatpakAction::Update => {
                        command.args(["update", "--user", "--noninteractive", &app_id]);
                    }
                    WireFlatpakAction::Remove => {
                        command.args(["uninstall", "--user", "--noninteractive", &app_id]);
                    }
                }
                run_host_command(command)
            })
            .await
            .map_err(|e| Error::failed(format!("join: {e}")))??;
            results.get().set_status(&status);
            Ok(())
        }
    }

    #[allow(
        clippy::manual_async_fn,
        reason = "the generated Cap'n Proto Server trait requires an opaque Future return type"
    )]
    fn remote(
        self: capnp::capability::Rc<Self>,
        params: app_host::RemoteParams,
        mut results: app_host::RemoteResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        async move {
            let p = params.get().map_err(|e| Error::failed(e.to_string()))?;
            let action = p.get_action().map_err(|e| Error::failed(e.to_string()))?;
            let name = p
                .get_name()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_string()
                .map_err(|e| Error::failed(e.to_string()))?;
            let url = p
                .get_url()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_string()
                .map_err(|e| Error::failed(e.to_string()))?;
            let status = tokio::task::spawn_blocking(move || {
                let mut command = std::process::Command::new("flatpak");
                match action {
                    WireRemoteAction::Add => {
                        command.args(["remote-add", "--user", "--if-not-exists", &name, &url]);
                    }
                    WireRemoteAction::Remove => {
                        command.args(["remote-delete", "--user", "--force", &name]);
                    }
                }
                run_host_command(command)
            })
            .await
            .map_err(|e| Error::failed(format!("join: {e}")))??;
            results.get().set_status(&status);
            Ok(())
        }
    }
}

#[cfg(test)]
mod command_tests {
    use super::safe_host_diagnostic;

    #[test]
    fn host_diagnostics_are_small_and_strip_terminal_controls() {
        let mut bytes = vec![b'x'; 8192];
        bytes.extend_from_slice(b"\n\x1b[31mfailure\0");
        let diagnostic = safe_host_diagnostic(&bytes);
        assert!(diagnostic.len() <= 4096);
        assert!(!diagnostic.contains('\x1b'));
        assert!(!diagnostic.contains('\0'));
        assert!(diagnostic.ends_with("[31mfailure"));
    }
}

#[cfg(test)]
mod dev_override_tests {
    use super::path_or_default;
    use crate::dev_env::{LAUNCHER_BIN_ENV, dev_override_from};
    use std::path::PathBuf;

    #[test]
    fn a_release_build_keeps_the_image_launcher() {
        let default = "/usr/bin/tilo-launcher";
        let gated = dev_override_from(false, LAUNCHER_BIN_ENV, Some("/tmp/attacker".into()));
        assert_eq!(path_or_default(gated, default), PathBuf::from(default));
        let honoured = dev_override_from(true, LAUNCHER_BIN_ENV, Some("/dev/overlay".into()));
        assert_eq!(
            path_or_default(honoured, default),
            PathBuf::from("/dev/overlay")
        );
    }
}
