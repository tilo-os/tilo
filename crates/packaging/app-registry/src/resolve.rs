//! Resolution + fetch: turn an app-id into verified `.tapp` bytes.
//!
//! The flow, across the configured registries in priority order:
//!
//! 1. Fetch the registry's index (`file://` read or `https://` GET).
//! 2. If the registry is pinned to an operator key, verify the index
//!    signature; an unsigned/mismatched index means we don't trust
//!    this registry's data and move on.
//! 3. Pick the best entry for the host (arch + ISA + runtime), highest
//!    semver; verify the entry's publisher signature.
//! 4. Download the tapp blob over iroh-blobs, keyed by the entry's
//!    blake3 hash. A thin tapp's payload blobs come later from the same
//!    provider ([`FetchOutcome::payload`]), only those the device lacks.
//! 5. Verify the downloaded bytes fail-closed: blake3 matches, and the
//!    embedded publisher signature validates against the entry's
//!    publisher (via [`tilo_app_package::verify()`]).
//!
//! A hash/signature mismatch on downloaded bytes aborts the whole
//! fetch — those bytes are never returned for install. A registry that
//! doesn't carry the app, or whose index we can't trust, is
//! skipped so a later registry can serve it.

use anyhow::{Context, Result, anyhow, bail};
use semver::VersionReq;

use tilo_app_package::PackageId;

use crate::config::{RegistriesConfig, RegistryRef, RegistryTrust};
use crate::index::{HostProfile, RegistryIndex};
use crate::tapp;
use crate::transport::{self, ProviderBlobs, fetch_blob};
use iroh::Endpoint;
use iroh_blobs::ticket::BlobTicket;

/// Verified bytes ready to hand to the installer.
#[derive(Clone, Debug)]
pub struct FetchOutcome {
    /// The package as the registry serves it: thin for a registry that
    /// also serves payload blobs, full otherwise.
    pub bytes: Vec<u8>,
    /// Where the payload blobs a thin package does not carry come from.
    pub payload: ProviderBlobs,
    pub package_id: PackageId,
    pub app_id: String,
    pub version: String,
    pub arch: String,
    pub isa: String,
    /// Which registry served it.
    pub registry: String,
}

/// Fetch the raw index document bytes for a registry.
///
/// # Errors
///
/// Returns an error if a `file://` registry's `index.json` can't be
/// read, an `https://`/`http://` GET fails or returns non-success, or
/// `base_url` uses an unsupported scheme.
pub async fn fetch_index_bytes(registry: &RegistryRef) -> Result<Vec<u8>> {
    if let Some(path) = registry.file_path() {
        let p = path.join("index.json");
        return tokio::fs::read(&p)
            .await
            .with_context(|| format!("read {}", p.display()));
    }
    if registry.base_url.starts_with("https://") || registry.base_url.starts_with("http://") {
        let url = format!("{}/index.json", registry.base_url.trim_end_matches('/'));
        let resp = tilo_net::http::client()
            .build()
            .context("build registry HTTP client")?
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("GET {url}"))?;
        return Ok(resp.bytes().await.context("read index body")?.to_vec());
    }
    bail!(
        "unsupported registry base-url scheme: {}",
        registry.base_url
    );
}

/// Resolve `app_id` (optionally constrained by `req`) to verified
/// bytes across all configured registries. Binds one client endpoint
/// for the whole sweep.
pub async fn fetch_tapp(
    config: &RegistriesConfig,
    app_id: &str,
    req: Option<&VersionReq>,
    host: &HostProfile,
) -> Result<FetchOutcome> {
    let client = transport::client_endpoint().await?;
    for registry in &config.registries {
        match try_registry(&client, registry, app_id, req, host).await {
            Ok(Some(outcome)) => return Ok(outcome),
            Ok(None) => {}
            // A crypto/hash failure on real bytes is fatal — fail closed.
            Err(e) => return Err(e),
        }
    }
    bail!("no configured registry offers {app_id} for this host")
}

/// Try one registry. `Ok(Some)` = served + verified; `Ok(None)` =
/// soft skip (untrusted index / app absent); `Err` = fail-closed
/// (downloaded bytes failed verification, or a trusted index carried
/// a malformed entry).
async fn try_registry(
    client: &Endpoint,
    registry: &RegistryRef,
    app_id: &str,
    req: Option<&VersionReq>,
    host: &HostProfile,
) -> Result<Option<FetchOutcome>> {
    let raw = match fetch_index_bytes(registry).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(registry = %registry.name, error = %e, "index unavailable, skipping");
            return Ok(None);
        }
    };
    let index = match RegistryIndex::from_json(&raw) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(registry = %registry.name, error = %e, "index malformed, skipping");
            return Ok(None);
        }
    };

    // Index-level trust gate.
    match &registry.trust {
        RegistryTrust::Key(k) => {
            if let Err(e) = index.verify_signature(k) {
                tracing::warn!(registry = %registry.name, error = %e, "index signature not trusted, skipping");
                return Ok(None);
            }
        }
        RegistryTrust::Unsigned => {
            // Index signature not required; per-entry publisher
            // signatures are still enforced below.
        }
    }

    let Some(entry) = index.resolve_best(app_id, req, host) else {
        return Ok(None);
    };

    // From here on the entry came out of a trusted index; a malformed
    // ticket / hash is a hard error.
    let ticket: BlobTicket = entry
        .ticket
        .parse()
        .with_context(|| format!("entry {app_id}@{} has an unparseable ticket", entry.version))?;
    let expected = entry.tapp_hash_bytes()?;
    let bytes = fetch_blob(client, &ticket, expected)
        .await
        .with_context(|| format!("download {app_id}@{}", entry.version))?;

    let package_id = entry.parse_package_id()?;
    let publisher = entry.publisher_key()?;
    // Fail closed: verify signature + hashes of the actual bytes.
    tapp::verify_bytes(&bytes, package_id, publisher)
        .with_context(|| format!("verify {app_id}@{}", entry.version))?;

    Ok(Some(FetchOutcome {
        bytes: bytes.to_vec(),
        payload: ProviderBlobs::new(ticket.addr().clone()),
        package_id,
        app_id: entry.app_id.clone(),
        version: entry.version.clone(),
        arch: crate::index::arch_wire(entry.arch).to_string(),
        isa: entry.isa.clone(),
        registry: registry.name.clone(),
    }))
}

/// Convenience: load the registries config (global + optional
/// workspace), detect the host, and fetch. `version` is an optional
/// semver requirement string (`"^1.2"`, `"=0.3.0"`, …).
pub async fn resolve_and_fetch(
    workspace_root: Option<&std::path::Path>,
    host: &HostProfile,
    app_id: &str,
    version: Option<&str>,
) -> Result<FetchOutcome> {
    let config = RegistriesConfig::load(workspace_root).map_err(|e| anyhow!("load config: {e}"))?;
    let req = match version {
        Some(v) => Some(VersionReq::parse(v).with_context(|| format!("bad version req {v:?}"))?),
        None => None,
    };
    fetch_tapp(&config, app_id, req.as_ref(), host).await
}
