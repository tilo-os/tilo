//! Content transport for tapp bytes — iroh-blobs, keyed by blake3.
//!
//! This is the same content-addressed transport the FUSE filesystem
//! uses. A `.tapp` is a single raw blob; its blake3 hash is both the
//! registry-index `tapp_hash` and the iroh-blobs key, so the shared
//! runtime bundle and every thin app blob are downloaded exactly once
//! and deduped across all apps on the node.
//!
//! The publish side ([`BlobNode`]) serves a blob store over the iroh
//! router; the fetch side ([`fetch_blob`]) connects to the node named
//! in a [`BlobTicket`] and streams the blob, which iroh verifies
//! against its hash as it arrives (bao). We additionally re-check the
//! blake3 so the failure is explicit and local. The same node serves the
//! payload blobs of the thin packages it publishes ([`ProviderBlobs`]), so
//! an installer fetches only the blobs its store lacks.

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use iroh::Endpoint;
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh_base::EndpointAddr;
use iroh_blobs::api::Store;
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::store::mem::MemStore;
use iroh_blobs::ticket::BlobTicket;
use iroh_blobs::{BlobFormat, BlobsProtocol, Hash};
use std::path::Path;
use tilo_app_package::{BlobFetcher, BlobHash, BlobSink};

/// An iroh-blobs store served over the network — the provider side of
/// the transport. Holds the endpoint + router alive for its lifetime.
pub struct BlobNode {
    endpoint: Endpoint,
    backend: Backend,
    _router: Router,
}

enum Backend {
    Mem(MemStore),
    Fs(FsStore),
}

impl Backend {
    fn store(&self) -> &Store {
        match self {
            Backend::Mem(m) => m,
            Backend::Fs(f) => f,
        }
    }
}

impl BlobNode {
    /// Bind a fresh endpoint and serve an in-memory blob store.
    /// Ephemeral — used by tests and short-lived transfers.
    pub async fn memory() -> Result<Self> {
        let store = MemStore::new();
        Self::spawn(Backend::Mem(store)).await
    }

    /// Bind a fresh endpoint and serve a persistent blob store rooted
    /// at `dir` (created if missing). This is what a self-hosted
    /// `file://` registry node serves from.
    ///
    /// # Errors
    ///
    /// Returns an error if `dir` can't be created, the store fails to
    /// load, or binding the endpoint fails.
    pub async fn persistent(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create blob store {}", dir.display()))?;
        let store = FsStore::load(dir)
            .await
            .context("load iroh-blobs FsStore")?;
        Self::spawn(Backend::Fs(store)).await
    }

    async fn spawn(backend: Backend) -> Result<Self> {
        let endpoint = Endpoint::bind(presets::N0)
            .await
            .map_err(|e| anyhow!("bind endpoint: {e}"))?;
        let protocol = BlobsProtocol::new(backend.store(), None);
        let router = Router::builder(endpoint.clone())
            .accept(iroh_blobs::ALPN, protocol)
            .spawn();
        Ok(Self {
            endpoint,
            backend,
            _router: router,
        })
    }

    /// This node's dialable address.
    pub fn addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    /// The underlying endpoint (for callers that also fetch).
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Ingest `bytes`, returning the blake3 content hash. Idempotent.
    pub async fn add(&self, bytes: impl Into<Bytes>) -> Result<Hash> {
        let tag = self
            .backend
            .store()
            .blobs()
            .add_bytes(bytes.into())
            .await
            .map_err(|e| anyhow!("add_bytes: {e}"))?;
        Ok(tag.hash)
    }

    /// Whether `hash` is present in the served store.
    pub async fn has(&self, hash: Hash) -> bool {
        self.backend
            .store()
            .blobs()
            .has(hash)
            .await
            .unwrap_or(false)
    }

    /// A ticket that lets a peer fetch `hash` from this node.
    pub fn ticket(&self, hash: Hash) -> BlobTicket {
        BlobTicket::new(self.addr(), hash, BlobFormat::Raw)
    }

    /// Ingest + produce a fetch ticket in one step.
    ///
    /// # Errors
    ///
    /// Returns an error if [`Self::add`] fails.
    pub async fn add_and_ticket(&self, bytes: impl Into<Bytes>) -> Result<(Hash, BlobTicket)> {
        let hash = self.add(bytes).await?;
        Ok((hash, self.ticket(hash)))
    }
}

/// Bind a fresh client endpoint for fetching (no serving).
pub async fn client_endpoint() -> Result<Endpoint> {
    Endpoint::bind(presets::N0)
        .await
        .map_err(|e| anyhow!("bind client endpoint: {e}"))
}

/// Fetch the blob named by `ticket` from its node, requiring the
/// content to hash to `expected` (the registry-index `tapp_hash`).
///
/// Fail-closed on every axis:
/// - the ticket's advertised hash must equal `expected` (a registry
///   cannot point a well-known hash at unrelated bytes);
/// - iroh streams the blob under bao verification against `expected`,
///   so wrong bytes abort the stream;
/// - we re-derive the blake3 and compare, so any residual mismatch is
///   an explicit local error rather than silent acceptance.
pub async fn fetch_blob(
    endpoint: &Endpoint,
    ticket: &BlobTicket,
    expected: [u8; 32],
) -> Result<Bytes> {
    let want = Hash::from(expected);
    if ticket.hash() != want {
        bail!(
            "ticket hash {} does not match the index tapp_hash {}",
            ticket.hash(),
            want
        );
    }
    let conn = endpoint
        .connect(ticket.addr().clone(), iroh_blobs::ALPN)
        .await
        .map_err(|e| anyhow!("connect to provider: {e}"))?;
    let (bytes, _stats) = iroh_blobs::get::request::get_blob(conn, want)
        .bytes_and_stats()
        .await
        .map_err(|e| anyhow!("get_blob: {e}"))?;
    let got = blake3::hash(&bytes);
    if got.as_bytes() != &expected {
        bail!(
            "downloaded blob hashes to {} but the index promised {}",
            hex::encode(got.as_bytes()),
            hex::encode(expected)
        );
    }
    Ok(bytes)
}

/// The provider of a package's payload blobs: the node that served the
/// package. A registry serves thin packages, so installing one fetches only
/// the payload blobs the device's store lacks, by hash, from here.
#[derive(Clone, Debug)]
pub struct ProviderBlobs {
    addr: EndpointAddr,
}

impl ProviderBlobs {
    pub fn new(addr: EndpointAddr) -> Self {
        Self { addr }
    }

    pub fn addr(&self) -> &EndpointAddr {
        &self.addr
    }

    /// Fetch every blob in `hashes` over one connection, in order. iroh
    /// verifies each stream against its hash as it arrives; the blake3 is
    /// re-derived here so a mismatch is an explicit local error.
    ///
    /// # Errors
    ///
    /// Returns an error if the provider cannot be reached, lacks a blob, or
    /// serves bytes that do not match.
    pub async fn fetch_all(&self, hashes: &[BlobHash]) -> Result<Vec<Bytes>> {
        let endpoint = client_endpoint().await?;
        let conn = endpoint
            .connect(self.addr.clone(), iroh_blobs::ALPN)
            .await
            .map_err(|e| anyhow!("connect to provider: {e}"))?;
        let mut out = Vec::with_capacity(hashes.len());
        for blob in hashes {
            let want = Hash::from(*blob.as_bytes());
            let (bytes, _stats) = iroh_blobs::get::request::get_blob(conn.clone(), want)
                .bytes_and_stats()
                .await
                .map_err(|e| anyhow!("get payload blob {blob}: {e}"))?;
            if blake3::hash(&bytes).as_bytes() != blob.as_bytes() {
                bail!("provider served bytes that do not hash to payload blob {blob}");
            }
            out.push(bytes);
        }
        Ok(out)
    }
}

impl BlobFetcher for ProviderBlobs {
    fn fetch(&self, hashes: &[BlobHash], sink: &mut dyn BlobSink) -> std::io::Result<()> {
        // Installers call this from synchronous code; nesting a runtime
        // inside an async task would deadlock or panic, so refuse clearly.
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(std::io::Error::other(
                "payload fetch must not run on an async runtime thread",
            ));
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let blobs = runtime
            .block_on(self.fetch_all(hashes))
            .map_err(std::io::Error::other)?;
        for (hash, bytes) in hashes.iter().zip(blobs) {
            sink.accept(hash, bytes.len() as u64, &mut bytes.as_ref())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real two-node transfer, entirely in-process: node A serves blobs, node
    /// B fetches them by ticket or hash and verifies each hash.
    #[tokio::test]
    async fn blobs_transfer_between_nodes_verified_by_hash() {
        let node = BlobNode::memory().await.unwrap();
        let client = client_endpoint().await.unwrap();

        let payload = Bytes::from_static(b"a tapp's worth of bytes");
        let expected = *blake3::hash(&payload).as_bytes();
        let (hash, ticket) = node.add_and_ticket(payload.clone()).await.unwrap();
        assert_eq!(hash.as_bytes(), &expected);
        assert!(node.has(hash).await);
        let got = fetch_blob(&client, &ticket, expected).await.unwrap();
        assert_eq!(got, payload);

        let (_hash, ticket) = node
            .add_and_ticket(Bytes::from_static(b"real"))
            .await
            .unwrap();
        let bogus = [0xabu8; 32];
        assert!(fetch_blob(&client, &ticket, bogus).await.is_err());

        // A provider serves payload blobs by hash, in the order asked.
        node.add(Bytes::from_static(b"payload a")).await.unwrap();
        node.add(Bytes::from_static(b"payload b")).await.unwrap();
        let provider = ProviderBlobs::new(node.addr());
        let got = provider
            .fetch_all(&[BlobHash::of(b"payload b"), BlobHash::of(b"payload a")])
            .await
            .unwrap();
        assert_eq!(
            got,
            vec![
                Bytes::from_static(b"payload b"),
                Bytes::from_static(b"payload a")
            ]
        );
        assert!(
            provider
                .fetch_all(&[BlobHash::of(b"never published")])
                .await
                .is_err()
        );
    }
}
