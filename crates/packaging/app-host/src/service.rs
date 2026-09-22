//! Cap'n Proto `AppHost::Server` implementation. Wires the 8 RPC
//! methods to the shared [`Store`] and the [`gc::sweep`] pass. The
//! transport (Unix socket listener) lives in `transport.rs`.
//!
//! capnp 0.26 generates async server methods on `capnp::capability::Rc<Self>`
//! returning an `impl Future<Output = Result<(), capnp::Error>>`; blocking
//! work (fs IO, blob-store puts, tar unpack) is routed through
//! `tokio::task::spawn_blocking` so the capnp event loop stays responsive.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use capnp::Error;

use crate::app_host_capnp::{
    FlatpakAction as WireFlatpakAction, Pinning as WirePinning, RemoteAction as WireRemoteAction,
    app_host,
};
use crate::gc;
use crate::install::{InstallExpectation, install_verified_tapp};
use crate::registry::{Pinning, RegistryEntry};
use crate::store::Store;
use tilo_app_package::id::PackageId;

const SYSTEM_DATA_DIR: &str = "/usr/share/tilo-apps";
const SYSTEM_PAYLOAD_DIR: &str = "/usr/lib/tilo-apps";

#[derive(Debug, serde::Deserialize)]
struct SystemRegistry {
    #[serde(default)]
    entry: Vec<SystemRegistryEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct SystemRegistryEntry {
    id: String,
    package_id_hex: String,
    payload_root: PathBuf,
    manifest_path: PathBuf,
}

#[derive(Clone, Debug)]
struct SystemPackage {
    id: PackageId,
    app_id: String,
    payload_root: PathBuf,
    manifest_path: PathBuf,
    manifest_text: String,
}

fn system_data_dir() -> PathBuf {
    std::env::var_os("TILO_APPS_SYSTEM_DIR")
        .map_or_else(|| PathBuf::from(SYSTEM_DATA_DIR), PathBuf::from)
}

fn system_payload_dir() -> PathBuf {
    std::env::var_os("TILO_APPS_SYSTEM_PAYLOAD_DIR")
        .map_or_else(|| PathBuf::from(SYSTEM_PAYLOAD_DIR), PathBuf::from)
}

fn is_within(root: &Path, path: &Path) -> bool {
    let Ok(root) = fs::canonicalize(root) else {
        return false;
    };
    let Ok(path) = fs::canonicalize(path) else {
        return false;
    };
    path.starts_with(root)
}

fn system_packages() -> Result<Vec<SystemPackage>, Error> {
    system_packages_from(&system_data_dir(), &system_payload_dir())
}

fn system_packages_from(root: &Path, payload_root: &Path) -> Result<Vec<SystemPackage>, Error> {
    let path = root.join("registry.toml");
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(Error::failed(format!(
                "read system app registry {}: {error}",
                path.display()
            )));
        }
    };
    let registry: SystemRegistry = toml::from_str(&raw)
        .map_err(|error| Error::failed(format!("parse system app registry: {error}")))?;
    let mut out = Vec::with_capacity(registry.entry.len());
    for row in registry.entry {
        let bytes = hex::decode(&row.package_id_hex)
            .map_err(|error| Error::failed(format!("system package {} id: {error}", row.id)))?;
        let id: [u8; 32] = bytes.try_into().map_err(|bytes: Vec<u8>| {
            Error::failed(format!(
                "system package {} id has {} bytes, expected 32",
                row.id,
                bytes.len()
            ))
        })?;
        if !is_within(payload_root, &row.payload_root)
            || !is_within(payload_root, &row.manifest_path)
        {
            return Err(Error::failed(format!(
                "system package {} escapes payload root {}",
                row.id,
                payload_root.display()
            )));
        }
        let manifest_text = fs::read_to_string(&row.manifest_path).map_err(|error| {
            Error::failed(format!(
                "read system manifest {}: {error}",
                row.manifest_path.display()
            ))
        })?;
        let manifest = tilo_app_package::Manifest::from_toml_str(&manifest_text)
            .map_err(|error| Error::failed(format!("system manifest {}: {error}", row.id)))?;
        if manifest.package.id != row.id {
            return Err(Error::failed(format!(
                "system registry id {} disagrees with manifest {}",
                row.id, manifest.package.id
            )));
        }
        out.push(SystemPackage {
            id: PackageId::from_bytes(id),
            app_id: manifest.package.id,
            payload_root: row.payload_root,
            manifest_path: row.manifest_path,
            manifest_text,
        });
    }
    Ok(out)
}

fn user_app_ids(rows: &[RegistryEntry]) -> HashSet<String> {
    rows.iter()
        .filter_map(|row| tilo_app_package::Manifest::from_toml_str(&row.manifest_text).ok())
        .map(|manifest| manifest.package.id)
        .collect()
}

fn system_as_registry(row: &SystemPackage) -> RegistryEntry {
    RegistryEntry {
        id: row.id,
        pinning: Pinning::Tried,
        payload_root: row.payload_root.clone(),
        manifest_text: row.manifest_text.clone(),
        installed_at_unix: 0,
        last_used_unix: 0,
    }
}

fn visible_entries(
    user_entries: Vec<RegistryEntry>,
    system_entries: Vec<SystemPackage>,
) -> Vec<(RegistryEntry, bool)> {
    let user_ids = user_app_ids(&user_entries);
    let mut entries: Vec<(RegistryEntry, bool)> = user_entries
        .into_iter()
        .map(|entry| (entry, true))
        .collect();
    for system in system_entries {
        if !user_ids.contains(&system.app_id) {
            entries.push((system_as_registry(&system), false));
        }
    }
    entries
}

fn visible_system_for_id(store: &Store, id: PackageId) -> Result<Option<SystemPackage>, Error> {
    let user_ids = user_app_ids(&store.list());
    Ok(system_packages()?
        .into_iter()
        .find(|entry| entry.id == id && !user_ids.contains(&entry.app_id)))
}

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
    user_managed: bool,
) {
    out.set_id(e.id.as_bytes());
    out.set_pinning(wire_pinning(e.pinning));
    out.set_payload_root(e.payload_root.to_string_lossy());
    out.set_manifest_text(&e.manifest_text);
    out.set_size_bytes(size_bytes);
    out.set_last_used_unix(e.last_used_unix);
    out.set_user_managed(user_managed);
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
    std::env::var_os("TILO_LAUNCHER_BIN").map_or_else(
        || std::path::PathBuf::from("/usr/bin/tilo-launcher"),
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
        let store = Arc::clone(&self.store);
        async move {
            let entries = tokio::task::spawn_blocking(move || {
                let user_entries = store.list();
                let system_entries = system_packages()?;
                let entries = visible_entries(user_entries, system_entries);
                let sizes: Vec<u64> = entries
                    .iter()
                    .map(|(entry, user_managed)| {
                        if *user_managed {
                            payload_size(&store, entry)
                        } else {
                            crate::store::dir_size(&entry.payload_root).unwrap_or(0)
                        }
                    })
                    .collect();
                Ok::<_, Error>((entries, sizes))
            })
            .await
            .map_err(|e| Error::failed(format!("join: {e}")))?;
            let (rows, sizes) = entries?;
            let row_count = u32::try_from(rows.len())
                .map_err(|_| Error::failed("too many registry entries to list".to_string()))?;
            let mut list = results.get().init_entries(row_count);
            for (i, ((row, user_managed), size)) in rows.iter().zip(sizes.iter()).enumerate() {
                let idx = u32::try_from(i)
                    .map_err(|_| Error::failed("too many registry entries to list".to_string()))?;
                fill_entry(list.reborrow().get(idx), row, *size, *user_managed);
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
            let (entry, size, user_managed) = tokio::task::spawn_blocking(move || {
                if let Ok(entry) = store.resolve(id) {
                    let size = payload_size(&store, &entry);
                    return Ok((entry, size, true));
                }
                let system = visible_system_for_id(&store, id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("unknown package {id}"))?;
                let entry = system_as_registry(&system);
                let size = crate::store::dir_size(&entry.payload_root).unwrap_or(0);
                Ok((entry, size, false))
            })
            .await
            .map_err(|e| Error::failed(format!("join: {e}")))?
            .map_err(Error::failed)?;
            let out = results.get().init_entry();
            fill_entry(out, &entry, size, user_managed);
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
            let tapp_bytes = p
                .get_tapp_bytes()
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
                let id = install_verified_tapp(&store, &tapp_bytes, pinning, &expected)
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
                let (entry, manifest, user_managed) = if let Ok(entry) = store.resolve(id) {
                    let manifest = store
                        .root()
                        .join("manifests")
                        .join(format!("{}.toml", id.to_hex()));
                    if let Some(parent) = manifest.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| {
                            Error::failed(format!("create manifest directory: {e}"))
                        })?;
                    }
                    atomic_file::write(&manifest, entry.manifest_text.as_bytes())
                        .map_err(|e| Error::failed(format!("materialize manifest: {e}")))?;
                    (entry, manifest, true)
                } else {
                    let system = visible_system_for_id(&store, id)?
                        .ok_or_else(|| Error::failed(format!("unknown package {id}")))?;
                    let manifest = system.manifest_path.clone();
                    (system_as_registry(&system), manifest, false)
                };
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
                if user_managed {
                    store
                        .mark_used(id)
                        .map_err(|e| Error::failed(e.to_string()))?;
                }
                Ok::<_, Error>(status)
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
mod tests {
    use super::*;
    use std::io::Write;

    fn system_manifest(id: &str) -> String {
        format!(
            r#"[package]
id = "{id}"
version = "1.0.0"
kind = "app"

[publisher]
identity = "ed25519:{identity}"
signature = ""

[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[sandbox.services]

[sandbox.portals]

[entrypoint]
bin = "bin/app"
type = "gui"
"#,
            identity = "0".repeat(64)
        )
    }

    #[test]
    fn system_registry_reads_share_metadata_and_lib_payloads() {
        let root = tempfile::tempdir().unwrap();
        let payload_root = tempfile::tempdir().unwrap();
        let payload = payload_root.path().join("payloads/so.tilo.system");
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::write(payload.join("bin/app"), b"app").unwrap();
        let manifest_path = payload.join("manifest.toml");
        std::fs::write(&manifest_path, system_manifest("so.tilo.system")).unwrap();
        let package_id = "ab".repeat(32);
        let mut registry = std::fs::File::create(root.path().join("registry.toml")).unwrap();
        writeln!(
            registry,
            "[[entry]]\nid = \"so.tilo.system\"\npackage_id_hex = \"{package_id}\"\npayload_root = \"{}\"\nmanifest_path = \"{}\"",
            payload.display(),
            manifest_path.display()
        )
        .unwrap();

        let entries = system_packages_from(root.path(), payload_root.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].app_id, "so.tilo.system");
        assert_eq!(entries[0].id.to_hex(), package_id);
        assert_eq!(entries[0].payload_root, payload);
    }

    #[test]
    fn system_registry_rejects_payload_path_escape() {
        let root = tempfile::tempdir().unwrap();
        let payload_root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let package_id = "cd".repeat(32);
        std::fs::write(root.path().join("registry.toml"), format!(
            "[[entry]]\nid = \"so.tilo.system\"\npackage_id_hex = \"{package_id}\"\npayload_root = \"{}\"\nmanifest_path = \"{}\"\n",
            outside.path().display(), outside.path().display()
        )).unwrap();
        let error = system_packages_from(root.path(), payload_root.path()).unwrap_err();
        assert!(error.to_string().contains("escapes payload root"));
    }

    #[test]
    fn user_entry_shadows_system_entry_by_app_id() {
        let system_id = PackageId::from_bytes([0xab; 32]);
        let user_id = PackageId::from_bytes([0xcd; 32]);
        let system = SystemPackage {
            id: system_id,
            app_id: "so.tilo.same".into(),
            payload_root: "/usr/lib/tilo-apps/payloads/system".into(),
            manifest_path: "/usr/lib/tilo-apps/payloads/system/manifest.toml".into(),
            manifest_text: system_manifest("so.tilo.same"),
        };
        let user = RegistryEntry::new_now(
            user_id,
            Pinning::Tried,
            "/home/user/.local/share/tilo-app-host/payloads/user".into(),
            system_manifest("so.tilo.same"),
        );
        let visible = visible_entries(vec![user], vec![system]);
        assert_eq!(visible.len(), 1);
        assert!(visible[0].1, "the user copy remains mutable");
        assert_eq!(visible[0].0.id, user_id);
    }
}
