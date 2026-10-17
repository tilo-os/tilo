//! Explicit host-owner import, deliberately absent from `BlobBackend` and its socket.
//!
//! The host keeps this object and its permits private. Constructing it is not
//! authorization: `LegacyImportAuthority` must consult the current authenticated
//! owner invocation, destination authority, enrollment and revocation on every
//! check. An unconfined device-admin script is a trusted owner caller; an app,
//! delegated actor or scoped agent is not. No historical ownership proof is
//! required: this creates new references, not migration evidence.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use super::{DocumentMedia, Hash, MAX_BLOB_BYTES, references};
use runtime_fs::blobs::BlobScope;

// Serializes journal admission across hosts and handles to the same store.
static PUBLICATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(test)]
#[cfg(feature = "filesystem")]
mod crash_tests;
#[cfg(test)]
#[cfg(feature = "filesystem")]
mod tests;

pub const MAX_LEGACY_IMPORT_ITEMS: usize = 128;
pub const MAX_LEGACY_IMPORT_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_LEGACY_IMPORT_LIFETIME: Duration = Duration::from_secs(120);

/// Canonical action arguments. Source identity is host-configured, never a path
/// supplied by a scoped caller. Hashes are exact, unique and bounded, not a scan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyImportRequest {
    pub source_store: String,
    pub source_room: String,
    pub destination_room: String,
    pub hashes: Vec<String>,
    pub max_bytes: u64,
}

/// Original owner-action custody, retained by the host, not accepted from JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyImportApproval {
    pub account: String,
    pub device: String,
    pub authority_revision: String,
    pub invocation: String,
    pub nonce: String,
    /// Digest of the exact canonical owner action arguments (including limits).
    pub argument_digest: [u8; 32],
}

/// Implement on the host's existing authenticated owner-action authority.
/// A check must reject changed arguments/custody and invalidated authority.
/// There is no extra witness requirement after that exact action was approved.
pub trait LegacyImportAuthority: Send + Sync {
    /// # Errors
    /// Refuse unless this exact original owner action and its current authority
    /// still authorize the bounded source selection and destination.
    fn authorize(
        &self,
        request: &LegacyImportRequest,
        approval: &LegacyImportApproval,
    ) -> Result<()>;
}

/// Opaque, non-cloneable, non-serializable and bound to one host instance.
#[must_use]
pub struct LegacyImportPermit {
    host: Arc<()>,
    request: LegacyImportRequest,
    approval: LegacyImportApproval,
    expires: Instant,
}

/// Host-only importer. Keep it separate from app-facing `DocumentMedia` handles.
/// Source and destination handles are immutable, including after refusal.
pub struct LegacyMediaImportHost {
    source_id: String,
    source: Arc<DocumentMedia>,
    destination: Arc<DocumentMedia>,
    authority: Arc<dyn LegacyImportAuthority>,
    identity: Arc<()>,
    unscoped: bool,
}

impl LegacyMediaImportHost {
    pub fn new(
        source_id: String,
        source: Arc<DocumentMedia>,
        destination: Arc<DocumentMedia>,
        authority: Arc<dyn LegacyImportAuthority>,
    ) -> Self {
        Self {
            source_id,
            source,
            destination,
            authority,
            identity: Arc::new(()),
            unscoped: false,
        }
    }

    /// Explicit owner import of resident legacy content without room roots.
    /// This is not a provenance claim. The host fixes the source store; callers
    /// select exact hashes and must name `unscoped` as the source room.
    pub fn new_unscoped(
        source_id: String,
        source: Arc<DocumentMedia>,
        destination: Arc<DocumentMedia>,
        authority: Arc<dyn LegacyImportAuthority>,
    ) -> Self {
        let mut host = Self::new(source_id, source, destination, authority);
        host.unscoped = true;
        host
    }

    /// Mint only for an already authorized explicit owner action. Scoped actors
    /// must never be given this host handle. No source bytes are inspected here.
    ///
    /// # Errors
    /// Refuses invalid limits, selections, source identity or current authority.
    pub fn permit(
        &self,
        request: LegacyImportRequest,
        approval: LegacyImportApproval,
        lifetime: Duration,
    ) -> Result<LegacyImportPermit> {
        ensure!(
            request.source_store == self.source_id,
            "legacy import refused"
        );
        ensure!(
            !request.hashes.is_empty() && request.hashes.len() <= MAX_LEGACY_IMPORT_ITEMS,
            "legacy import selection limit"
        );
        ensure!(
            request.max_bytes > 0 && request.max_bytes <= MAX_LEGACY_IMPORT_BYTES,
            "legacy import byte limit"
        );
        ensure!(
            !lifetime.is_zero() && lifetime <= MAX_LEGACY_IMPORT_LIFETIME,
            "legacy import lifetime limit"
        );
        BlobScope::new(&request.source_room, "legacy-import")?;
        BlobScope::new(&request.destination_room, "legacy-import")?;
        ensure!(
            !self.unscoped || request.source_room == "unscoped",
            "legacy import refused"
        );
        ensure!(
            request.source_room != request.destination_room
                || !Arc::ptr_eq(&self.source.store, &self.destination.store),
            "legacy import source equals destination"
        );
        let mut unique = HashSet::new();
        for hash in &request.hashes {
            ensure!(
                super::parse_hash(hash).is_some() && unique.insert(hash),
                "invalid legacy import selection"
            );
        }
        for field in [
            &approval.account,
            &approval.device,
            &approval.authority_revision,
            &approval.invocation,
            &approval.nonce,
        ] {
            ensure!(
                !field.is_empty() && field.len() <= 256,
                "invalid legacy import custody"
            );
        }
        self.authority.authorize(&request, &approval)?;
        Ok(LegacyImportPermit {
            host: self.identity.clone(),
            request,
            approval,
            expires: Instant::now()
                .checked_add(lifetime)
                .context("legacy import expiry overflow")?,
        })
    }

    /// Consume once. Every attempt that reaches publication is durably spent
    /// before publishing any destination reference. A crash/failure can leave a
    /// partial import, never a success receipt: retry requires a new owner
    /// invocation. Old tags and bytes are never removed or reclassified.
    /// The returned total is acknowledged only after all destination writes sync.
    ///
    /// # Errors
    /// Refuses expired, foreign, revoked or spent permits, unavailable source
    /// references, excess bytes and storage failures. Failure can leave a partial
    /// destination import, but never removes a source root.
    pub async fn import(&self, permit: LegacyImportPermit) -> Result<u64> {
        ensure!(
            Arc::ptr_eq(&permit.host, &self.identity),
            "legacy import refused"
        );
        self.check(&permit)?;
        let source_scope = BlobScope::new(&permit.request.source_room, "legacy-import")?;
        let destination_scope = BlobScope::new(&permit.request.destination_room, "legacy-import")?;
        // Serialize all imports into this destination, even from separate hosts.
        let _publication = PUBLICATION.lock().await;
        self.check(&permit)?;
        let mut journal_key = blake3::Hasher::new();
        for field in [
            &permit.approval.account,
            &permit.approval.device,
            &permit.approval.invocation,
        ] {
            journal_key.update(&(field.len() as u64).to_le_bytes());
            journal_key.update(field.as_bytes());
        }
        let journal_tag = format!("tilo/media-import/v1/{}", journal_key.finalize().to_hex());
        ensure!(
            self.destination
                .store
                .store()
                .tags()
                .get(&journal_tag)
                .await?
                .is_none(),
            "legacy import invocation already spent"
        );
        let mut total = 0_u64;
        let mut selection = Vec::new();
        for hex in &permit.request.hashes {
            let hash: Hash = hex.parse()?;
            // Scoped imports require exact legacy roots. Only the separately
            // constructed owner-only unscoped host may select resident content;
            // that explicitly creates reachability without a provenance claim.
            ensure!(
                self.unscoped || references(&self.source.store, &source_scope, hash).await,
                "legacy import unavailable"
            );
            let len = if self.unscoped {
                match self.source.store.store().blobs().status(hash).await? {
                    iroh_blobs::api::blobs::BlobStatus::Complete { size } => size,
                    _ => bail!("legacy import unavailable"),
                }
            } else {
                self.source
                    .len_of(&source_scope, hash)
                    .await
                    .context("legacy import unavailable")?
            };
            total = total.checked_add(len).context("legacy import byte limit")?;
            ensure!(
                len <= MAX_BLOB_BYTES && total <= permit.request.max_bytes,
                "legacy import byte limit"
            );
            selection.push((hash, len));
        }
        self.check(&permit)?;
        // A durable marker also roots its bounded audit payload. No private
        // paths or source inventory are exposed by the ordinary media API.
        let audit = postcard::to_stdvec(&(
            &permit.request,
            &permit.approval.account,
            &permit.approval.device,
            &permit.approval.authority_revision,
            &permit.approval.invocation,
            &permit.approval.nonce,
            permit.approval.argument_digest,
        ))?;
        let marker = self
            .destination
            .store
            .ingest(bytes::Bytes::from(audit))
            .await?;
        super::record_reference_tag(&self.destination.store, journal_tag.clone(), marker).await?;
        for selected in selection {
            self.publish_one(&permit, selected, &destination_scope)
                .await?;
        }
        self.check(&permit)?;
        super::record_reference_tag(
            &self.destination.store,
            format!("{journal_tag}/complete"),
            marker,
        )
        .await?;
        Ok(total)
    }

    async fn publish_one(
        &self,
        permit: &LegacyImportPermit,
        (hash, len): (Hash, u64),
        destination_scope: &BlobScope,
    ) -> Result<()> {
        self.check(permit)?;
        let source = self.source.clone();
        let chunks = futures_util::stream::try_unfold(0_u64, move |offset| {
            let source = source.clone();
            async move {
                if offset == len {
                    return Ok(None);
                }
                let end = offset.saturating_add(64 * 1024).min(len);
                let (bytes, actual_len) = source
                    .store
                    .read_blob_range(hash, offset..end)
                    .await
                    .ok_or_else(|| std::io::Error::other("legacy import unavailable"))?;
                if actual_len != len || bytes.len() as u64 != end.saturating_sub(offset) {
                    return Err(std::io::Error::other("legacy import unavailable"));
                }
                Ok(Some((bytes::Bytes::from(bytes), end)))
            }
        });
        // Store RPC futures are Send but not Sync. Only poll under this
        // mutex: the ingest API requires a Sync stream, not concurrent polls.
        let chunks = std::sync::Mutex::new(Box::pin(chunks));
        let chunks = futures_util::stream::poll_fn(move |cx| match chunks.lock() {
            Ok(mut stream) => futures_util::Stream::poll_next(stream.as_mut(), cx),
            Err(_) => std::task::Poll::Ready(Some(Err(std::io::Error::other(
                "legacy import stream unavailable",
            )))),
        });
        // Ingest verifies the hash and makes bytes durable before granting
        // destination reachability. Never publish a hash-only reference.
        let (imported, actual_len) = self.destination.store.ingest_stream(chunks).await?;
        if imported != hash || actual_len != len {
            bail!("legacy import content mismatch");
        }
        self.check(permit)?;
        super::record_reference(&self.destination.store, destination_scope, hash).await?;
        Ok(())
    }

    fn check(&self, permit: &LegacyImportPermit) -> Result<()> {
        ensure!(Instant::now() < permit.expires, "legacy import expired");
        self.authority.authorize(&permit.request, &permit.approval)
    }
}
