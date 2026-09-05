//! Cap'n Proto `AppHost::Server` implementation. Wires the 8 RPC
//! methods to the shared [`Store`] and the [`gc::sweep`] pass. The
//! transport (Unix socket listener) lives in `transport.rs`.
//!
//! capnp 0.26 generates async server methods on `capnp::capability::Rc<Self>`
//! returning an `impl Future<Output = Result<(), capnp::Error>>`; blocking
//! work (fs IO, blob-store puts, tar unpack) is routed through
//! `tokio::task::spawn_blocking` so the capnp event loop stays responsive.

use std::sync::Arc;

use capnp::Error;

use crate::app_host_capnp::{
    FlatpakAction as WireFlatpakAction, Pinning as WirePinning, RemoteAction as WireRemoteAction,
    app_host,
};
use crate::gc;
use crate::install::{InstallExpectation, install_verified_qpkg};
use crate::registry::{Pinning, RegistryEntry};
use crate::store::Store;
use pkg::id::PackageId;

/// Service state shared across every accepted connection.
pub struct AppHostServer {
    store: Arc<Store>,
    max_store_bytes: u64,
}

impl AppHostServer {
    /// Build a new server backed by `store`. `max_store_bytes` is the
    /// soft cap used by the post-install GC sweep; pass `0` to disable
    /// GC.
    pub fn new(store: Arc<Store>, max_store_bytes: u64) -> Self {
        Self {
            store,
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
    if bytes.len() != 32 {
        return Err(Error::failed(format!(
            "package id must be 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(bytes);
    Ok(PackageId::from_bytes(out))
}

/// Populate a wire `PackageEntry` builder from a registry row.
fn fill_entry(
    mut out: crate::app_host_capnp::package_entry::Builder<'_>,
    e: &RegistryEntry,
    size_bytes: u64,
) {
    out.set_id(e.id.as_bytes());
    out.set_pinning(wire_pinning(e.pinning));
    out.set_payload_root(e.payload_root.to_string_lossy());
    out.set_manifest_text(&e.manifest_text);
    out.set_size_bytes(size_bytes);
    out.set_last_used_unix(e.last_used_unix);
}

/// Best-effort payload-tree size for a given entry. Errors → 0.
///
/// Only the eagerly-extracted payload tree is counted; the shared
/// content-addressed blob store is not attributed per-package because
/// blobs are (deliberately) shared across packages.
fn payload_size(_store: &Store, entry: &RegistryEntry) -> u64 {
    crate::store::dir_size(&entry.payload_root).unwrap_or(0)
}

fn launcher_bin() -> std::path::PathBuf {
    std::env::var_os("QOS_LAUNCHER_BIN").map_or_else(
        || std::path::PathBuf::from("/usr/bin/qos-launcher"),
        std::path::PathBuf::from,
    )
}

fn wayland_socket() -> Option<std::path::PathBuf> {
    let display = std::env::var_os("WAYLAND_DISPLAY")?;
    let display = std::path::PathBuf::from(display);
    if display.is_absolute() {
        Some(display)
    } else {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .map(|root| root.join(display))
    }
}

fn spawn_detached(mut command: std::process::Command) -> Result<String, Error> {
    command
        .spawn()
        .map(|child| format!("started process {}", child.id()))
        .map_err(|error| Error::failed(format!("launch failed: {error}")))
}

fn run_host_command(mut command: std::process::Command) -> Result<String, Error> {
    let output = command
        .output()
        .map_err(|error| Error::failed(format!("host command failed: {error}")))?;
    let mut text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        text = String::from_utf8_lossy(&output.stderr).trim().to_string();
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
// thread. Forcing `Send` here isn't a real fix, it's a different
// (wrong) threading model.
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
        let store = Arc::clone(&self.store);
        async move {
            let entries = tokio::task::spawn_blocking(move || {
                let entries = store.list();
                let sizes: Vec<u64> = entries.iter().map(|e| payload_size(&store, e)).collect();
                (entries, sizes)
            })
            .await
            .map_err(|e| Error::failed(format!("join: {e}")))?;
            let (rows, sizes) = entries;
            let row_count = u32::try_from(rows.len())
                .map_err(|_| Error::failed("too many registry entries to list".to_string()))?;
            let mut list = results.get().init_entries(row_count);
            for (i, (row, size)) in rows.iter().zip(sizes.iter()).enumerate() {
                let idx = u32::try_from(i)
                    .map_err(|_| Error::failed("too many registry entries to list".to_string()))?;
                fill_entry(list.reborrow().get(idx), row, *size);
            }
            Ok(())
        }
    }

    fn resolve(
        self: capnp::capability::Rc<Self>,
        params: app_host::ResolveParams,
        mut results: app_host::ResolveResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let store = Arc::clone(&self.store);
        async move {
            let p = params.get().map_err(|e| Error::failed(e.to_string()))?;
            let id_bytes = p
                .get_id()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_vec();
            let id = parse_id(&id_bytes)?;
            let (entry, size) = tokio::task::spawn_blocking(move || {
                store
                    .resolve(id)
                    .map(|e| {
                        let s = payload_size(&store, &e);
                        (e, s)
                    })
                    .map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| Error::failed(format!("join: {e}")))?
            .map_err(Error::failed)?;
            let out = results.get().init_entry();
            fill_entry(out, &entry, size);
            Ok(())
        }
    }

    fn pin(
        self: capnp::capability::Rc<Self>,
        params: app_host::PinParams,
        _results: app_host::PinResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let store = Arc::clone(&self.store);
        async move {
            let p = params.get().map_err(|e| Error::failed(e.to_string()))?;
            let id_bytes = p
                .get_id()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_vec();
            let id = parse_id(&id_bytes)?;
            tokio::task::spawn_blocking(move || store.pin(id))
                .await
                .map_err(|e| Error::failed(format!("join: {e}")))?
                .map_err(|e| Error::failed(e.to_string()))?;
            Ok(())
        }
    }

    fn unpin(
        self: capnp::capability::Rc<Self>,
        params: app_host::UnpinParams,
        _results: app_host::UnpinResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let store = Arc::clone(&self.store);
        async move {
            let p = params.get().map_err(|e| Error::failed(e.to_string()))?;
            let id_bytes = p
                .get_id()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_vec();
            let id = parse_id(&id_bytes)?;
            tokio::task::spawn_blocking(move || store.unpin(id))
                .await
                .map_err(|e| Error::failed(format!("join: {e}")))?
                .map_err(|e| Error::failed(e.to_string()))?;
            Ok(())
        }
    }

    fn remove(
        self: capnp::capability::Rc<Self>,
        params: app_host::RemoveParams,
        _results: app_host::RemoveResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let store = Arc::clone(&self.store);
        async move {
            let p = params.get().map_err(|e| Error::failed(e.to_string()))?;
            let id_bytes = p
                .get_id()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_vec();
            let id = parse_id(&id_bytes)?;
            tokio::task::spawn_blocking(move || store.remove(id))
                .await
                .map_err(|e| Error::failed(format!("join: {e}")))?
                .map_err(|e| Error::failed(e.to_string()))?;
            Ok(())
        }
    }

    fn mark_used(
        self: capnp::capability::Rc<Self>,
        params: app_host::MarkUsedParams,
        _results: app_host::MarkUsedResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let store = Arc::clone(&self.store);
        async move {
            let p = params.get().map_err(|e| Error::failed(e.to_string()))?;
            let id_bytes = p
                .get_id()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_vec();
            let id = parse_id(&id_bytes)?;
            tokio::task::spawn_blocking(move || store.mark_used(id))
                .await
                .map_err(|e| Error::failed(format!("join: {e}")))?
                .map_err(|e| Error::failed(e.to_string()))?;
            Ok(())
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
            let p = params.get().map_err(|e| Error::failed(e.to_string()))?;
            let qpkg_bytes = p
                .get_qpkg_bytes()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_vec();
            let pinning = parse_pinning(p.get_pinning().map_err(|e| Error::failed(e.to_string()))?);
            let expected_root = parse_id(
                p.get_expected_root()
                    .map_err(|e| Error::failed(e.to_string()))?,
            )?;
            let publisher_bytes = p
                .get_expected_publisher()
                .map_err(|e| Error::failed(e.to_string()))?;
            let publisher: [u8; 32] = publisher_bytes.try_into().map_err(|_| {
                Error::failed(format!(
                    "publisher key must be 32 bytes, got {}",
                    publisher_bytes.len()
                ))
            })?;
            let app_id = p
                .get_expected_app_id()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_string()
                .map_err(|e| Error::failed(e.to_string()))?;
            let version = p
                .get_expected_version()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_string()
                .map_err(|e| Error::failed(e.to_string()))?;
            let expected = InstallExpectation {
                package_root: expected_root,
                publisher,
                app_id,
                version,
                size_bytes: p.get_expected_size_bytes(),
            };
            let id = tokio::task::spawn_blocking(move || {
                let id = install_verified_qpkg(&store, &qpkg_bytes, pinning, &expected)
                    .map_err(|e| e.to_string())?;
                // Best-effort post-install sweep. Errors here should not
                // fail the install — we already have the bytes on disk.
                if let Err(e) = gc::sweep(&store, max_bytes) {
                    tracing::warn!(error = %e, "post-install gc sweep failed");
                }
                Ok::<_, String>(id)
            })
            .await
            .map_err(|e| Error::failed(format!("join: {e}")))?
            .map_err(Error::failed)?;
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
            let usage = tokio::task::spawn_blocking(move || store.disk_usage())
                .await
                .map_err(|e| Error::failed(format!("join: {e}")))?
                .map_err(|e| Error::failed(e.to_string()))?;
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
        let store = Arc::clone(&self.store);
        async move {
            let p = params.get().map_err(|e| Error::failed(e.to_string()))?;
            let id = parse_id(p.get_id().map_err(|e| Error::failed(e.to_string()))?)?;
            let status = tokio::task::spawn_blocking(move || {
                let entry = store
                    .resolve(id)
                    .map_err(|e| Error::failed(e.to_string()))?;
                let manifest = store
                    .root()
                    .join("manifests")
                    .join(format!("{}.toml", id.to_hex()));
                if let Some(parent) = manifest.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| Error::failed(format!("create manifest directory: {e}")))?;
                }
                atomic_file::write(&manifest, entry.manifest_text.as_bytes())
                    .map_err(|e| Error::failed(format!("materialize manifest: {e}")))?;
                let mut command = std::process::Command::new(launcher_bin());
                command
                    .arg("run")
                    .arg("--manifest")
                    .arg(&manifest)
                    .arg("--payload-root")
                    .arg(&entry.payload_root);
                if let Some(socket) = wayland_socket() {
                    command.arg("--wayland-socket").arg(socket);
                }
                let status = spawn_detached(command)?;
                store
                    .mark_used(id)
                    .map_err(|e| Error::failed(e.to_string()))?;
                Ok::<_, Error>(status)
            })
            .await
            .map_err(|e| Error::failed(format!("join: {e}")))??;
            results.get().set_status(&status);
            Ok(())
        }
    }

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
