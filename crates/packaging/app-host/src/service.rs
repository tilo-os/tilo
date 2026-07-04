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

use crate::app_host_capnp::{app_host, Pinning as WirePinning};
use crate::gc;
use crate::install::{TrustPolicy, install_qpkg_with_policy};
use crate::registry::{Pinning, RegistryEntry};
use crate::store::Store;
use pkg::id::PackageId;

/// Service state shared across every accepted connection.
pub struct AppHostServer {
    store: Arc<Store>,
    max_store_bytes: u64,
    trust_policy: Arc<TrustPolicy>,
}

impl AppHostServer {
    /// Build a new server backed by `store`. `max_store_bytes` is the
    /// soft cap used by the post-install GC sweep; pass `0` to disable
    /// GC. `trust_policy` gates every install through the same
    /// publisher-trust checks `qos-install` uses.
    pub fn new(
        store: Arc<Store>,
        max_store_bytes: u64,
        trust_policy: Arc<TrustPolicy>,
    ) -> Self {
        Self {
            store,
            max_store_bytes,
            trust_policy,
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
    out.set_payload_root(&e.payload_root.to_string_lossy());
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

// -------- Server impl -----------------------------------------------

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
                let sizes: Vec<u64> =
                    entries.iter().map(|e| payload_size(&store, e)).collect();
                (entries, sizes)
            })
            .await
            .map_err(|e| Error::failed(format!("join: {e}")))?;
            let (rows, sizes) = entries;
            let mut list = results.get().init_entries(rows.len() as u32);
            for (i, (row, size)) in rows.iter().zip(sizes.iter()).enumerate() {
                fill_entry(list.reborrow().get(i as u32), row, *size);
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
            let id_bytes = p.get_id().map_err(|e| Error::failed(e.to_string()))?.to_vec();
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
            let id_bytes = p.get_id().map_err(|e| Error::failed(e.to_string()))?.to_vec();
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
            let id_bytes = p.get_id().map_err(|e| Error::failed(e.to_string()))?.to_vec();
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
            let id_bytes = p.get_id().map_err(|e| Error::failed(e.to_string()))?.to_vec();
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
            let id_bytes = p.get_id().map_err(|e| Error::failed(e.to_string()))?.to_vec();
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
        let trust_policy = Arc::clone(&self.trust_policy);
        async move {
            let p = params.get().map_err(|e| Error::failed(e.to_string()))?;
            let qpkg_bytes = p
                .get_qpkg_bytes()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_vec();
            let pinning = parse_pinning(
                p.get_pinning().map_err(|e| Error::failed(e.to_string()))?,
            );
            let id = tokio::task::spawn_blocking(move || {
                let id = install_qpkg_with_policy(&store, &qpkg_bytes, pinning, &trust_policy)
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
}
