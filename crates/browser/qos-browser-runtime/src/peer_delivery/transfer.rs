//! One pinned transfer's transport integrity; current policy and native admission
//! remain with the closed Peer owner on every transition.
use super::*;
use state::{SignedSnapshotManifest, SnapshotChunk, SnapshotKind, SnapshotManifest};
use tilo_relay::delivery_v3::cursor::CursorToken;

pub(crate) struct PinnedTransfer {
    identity: CursorIdentity,
    generation: u64,
    signed: SignedSnapshotManifest,
    digest: [u8; 32],
    boundary: Option<state::CommitBoundary>,
    deadline: web_time::Instant,
}
impl PinnedTransfer {
    pub fn open(
        identity: CursorIdentity,
        generation: u64,
        raw: &[u8],
        prior: Option<&StoredDeliveryCursor>,
        now: u64,
    ) -> Result<Self, String> {
        if generation == 0 {
            return Err("invalid transfer generation".into());
        }
        let key =
            ed25519_dalek::VerifyingKey::from_bytes(&identity.host).map_err(|e| e.to_string())?;
        let manifest = SnapshotManifest::from_bytes(raw).map_err(|e| e.to_string())?;
        let signed = manifest
            .authenticate(&key, now)
            .map_err(|e| e.to_string())?;
        let issue = signed.issue();
        let scope = &issue.scope;
        if !matches_identity(&identity, scope) || issue.stream_bytes > 256 * 1024 * 1024 {
            return Err("snapshot identity or browser storage bound mismatch".into());
        }
        if issue.kind == SnapshotKind::Replay {
            let prior = prior.ok_or("replay manifest has no committed predecessor")?;
            if prior.identity != identity {
                return Err("foreign predecessor identity".into());
            }
            let prior = CursorToken::from_bytes(&prior.token)
                .map_err(|e| e.to_string())?
                .authenticate(&key)
                .map_err(|e| e.to_string())?;
            let old = prior.issue();
            if issue.predecessor.as_ref() != Some(&old.committed)
                || scope.consumer != old.consumer
                || scope.store_generation != old.store_generation
                || scope.authorization_view != old.authorization_view
            {
                return Err("snapshot replay predecessor changed".into());
            }
        }
        let deadline = web_time::Instant::now()
            .checked_add(std::time::Duration::from_nanos(
                issue
                    .expires_at_ns
                    .checked_sub(now)
                    .ok_or("expired manifest")?,
            ))
            .ok_or("snapshot monotonic deadline overflow")?;
        Ok(Self {
            identity,
            generation,
            digest: manifest.digest(),
            signed,
            boundary: None,
            deadline,
        })
    }
    pub fn issue(&self) -> &state::ManifestIssue {
        self.signed.issue()
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
    pub fn current(&self, generation: u64, now: u64) -> Result<(), String> {
        self.current_at(generation, now, web_time::Instant::now())
    }
    pub(super) fn current_at(
        &self,
        generation: u64,
        now: u64,
        monotonic: web_time::Instant,
    ) -> Result<(), String> {
        if generation != self.generation
            || now >= self.issue().expires_at_ns
            || monotonic >= self.deadline
        {
            return Err("transfer generation or deadline expired".into());
        }
        Ok(())
    }
    pub fn renew(&mut self, generation: u64, raw: &[u8], now: u64) -> Result<(), String> {
        if generation != self.generation {
            return Err("stale transfer generation".into());
        }
        let key = ed25519_dalek::VerifyingKey::from_bytes(&self.identity.host)
            .map_err(|e| e.to_string())?;
        let manifest = SnapshotManifest::from_bytes(raw).map_err(|e| e.to_string())?;
        let signed = manifest
            .authenticate(&key, now)
            .map_err(|e| e.to_string())?;
        if manifest.digest() == self.digest {
            // Idempotent recovery returns the exact current bytes. In particular
            // this does not grant another receipt-relative lifetime.
            return self.current(generation, now);
        }
        if !self.signed.same_content(&signed)
            || signed.issue().expires_at_ns <= self.issue().expires_at_ns
        {
            return Err("renewal changed pinned contents or failed to extend deadline".into());
        }
        let deadline = web_time::Instant::now()
            .checked_add(std::time::Duration::from_nanos(
                signed
                    .issue()
                    .expires_at_ns
                    .checked_sub(now)
                    .ok_or("expired renewal")?,
            ))
            .ok_or("snapshot monotonic deadline overflow")?;
        self.signed = signed;
        self.digest = manifest.digest();
        self.deadline = deadline;
        Ok(())
    }
    pub fn validate_chunk(
        &self,
        generation: u64,
        chunk: &SnapshotChunk<'_>,
        now: u64,
    ) -> Result<(), String> {
        self.current(generation, now)?;
        self.signed.validate_chunk(chunk).map_err(|e| e.to_string())
    }
    /// IDB is storage, not proof. Revalidate every read against the retained
    /// signed digest table before decoding any original publication.
    pub fn validate_staged(
        &self,
        generation: u64,
        offset: u64,
        bytes: &[u8],
        now: u64,
    ) -> Result<(), String> {
        self.current(generation, now)?;
        if offset % u64::from(self.issue().chunk_size) != 0 {
            return Err("unaligned staged transfer read".into());
        }
        let index = u32::try_from(offset / u64::from(self.issue().chunk_size))
            .map_err(|_| "staged index overflow")?;
        let chunk = SnapshotChunk::new(self.issue().pin, self.digest, index, bytes)
            .map_err(|e| e.to_string())?;
        self.signed
            .validate_chunk(&chunk)
            .map_err(|e| e.to_string())
    }
    pub fn set_boundary(
        &mut self,
        generation: u64,
        boundary: state::CommitBoundary,
        now: u64,
    ) -> Result<(), String> {
        self.current(generation, now)?;
        let key = ed25519_dalek::VerifyingKey::from_bytes(&self.identity.host)
            .map_err(|e| e.to_string())?;
        let claims = boundary
            .cursor()
            .authenticate(&key)
            .map_err(|e| e.to_string())?;
        if claims.issue() != &self.issue().scope
            || boundary.committed() != &self.issue().scope.committed
            || boundary.consumer() != self.issue().scope.consumer
            || boundary.authorization_view() != self.issue().scope.authorization_view
            || boundary.store_generation() != self.issue().scope.store_generation
            || boundary.visible_count() != self.issue().item_count
        {
            return Err("final boundary differs from signed transfer manifest".into());
        }
        if self
            .boundary
            .as_ref()
            .is_some_and(|old| old.encode().ok() != boundary.encode().ok())
        {
            return Err("transfer boundary equivocation".into());
        }
        self.boundary = Some(boundary);
        Ok(())
    }
    pub fn finish(
        &self,
        generation: u64,
        visible_order: [u8; 32],
        now: u64,
    ) -> Result<StoredDeliveryCursor, String> {
        self.current(generation, now)?;
        let boundary = self
            .boundary
            .as_ref()
            .ok_or("transfer missing final boundary")?;
        if boundary.visible_order_digest() != visible_order {
            return Err("staged order differs from final boundary".into());
        }
        let scope = &self.issue().scope;
        let cursor = StoredDeliveryCursor {
            version: 1,
            identity: self.identity.clone(),
            consumer: scope.consumer.to_vec(),
            authorization_view: scope.authorization_view.to_vec(),
            store_generation: scope.store_generation.to_vec(),
            token: boundary.cursor().as_bytes().to_vec(),
        };
        cursor.validate_storage_shape()?;
        Ok(cursor)
    }
}
fn matches_identity(
    identity: &CursorIdentity,
    scope: &tilo_relay::delivery_v3::cursor::CursorIssue,
) -> bool {
    scope.workspace == identity.workspace
        && scope.room == identity.room
        && scope.workspace_root == identity.workspace_root
        && scope.host_device == identity.host
        && scope.host_root == identity.host_root
        && scope.consumer_device == identity.device
        && scope.consumer_root == identity.device_root
}
