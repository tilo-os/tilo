//! A pinned signed transfer is inert until all original bytes are verified.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct ManifestContext<'a> {
    pub scope: wire::OnboardingScope<'a>,
    pub purpose: wire::HandoffPurpose,
    pub device: [u8; 32],
    pub request_digest: [u8; 32],
    pub request_nonce: [u8; 32],
    pub expires_at_ns: u64,
}

pub(super) struct Handoff {
    manifest: ChargedBytes,
    output: ChargedBytes,
    position: usize,
    segment: usize,
    segment_start: usize,
    failed: bool,
    expires_at_ns: u64,
    // At most one shared decoder lives per call. Its bounded Vec growth can
    // overlap its old allocation; this reservation covers both capacities.
    _metadata: NativeReservation,
}
impl Handoff {
    pub fn start(
        bytes: &[u8],
        context: ManifestContext<'_>,
        budget: &IncomingBudget,
        now: u64,
    ) -> Result<Self, String> {
        if now >= context.expires_at_ns {
            return Err("original onboarding context expired".into());
        }
        if bytes.len() > wire::MAX_MANIFEST_BYTES {
            return Err("onboarding manifest exceeds bound".into());
        }
        let metadata =
            budget.reserve(3 * wire::MAX_SEGMENTS * std::mem::size_of::<wire::HandoffSegment>())?;
        let manifest = wire::HandoffManifest::decode(bytes).map_err(|e| e.to_string())?;
        manifest
            .verify_host_signature(
                context.scope,
                context.device,
                context.request_digest,
                context.request_nonce,
                now,
            )
            .map_err(|e| e.to_string())?;
        if manifest.purpose != context.purpose {
            return Err("onboarding manifest purpose mismatch".into());
        }
        let length =
            usize::try_from(manifest.total_bytes).map_err(|_| "onboarding size overflow")?;
        let output = ChargedBytes::zeroed(length, budget)?;
        let expires_at_ns = context.expires_at_ns.min(manifest.expires_at_ns);
        let manifest = ChargedBytes::copy(bytes, budget)?;
        Ok(Self {
            manifest,
            output,
            position: 0,
            segment: 0,
            segment_start: 0,
            failed: false,
            expires_at_ns,
            _metadata: metadata,
        })
    }
    pub fn request(&self, key: &SigningKey, now: u64) -> Result<wire::HandoffRequest, String> {
        if self.failed || self.remaining() == 0 {
            return Err("onboarding transfer is closed".into());
        }
        let manifest =
            wire::HandoffManifest::decode(&self.manifest.bytes).map_err(|e| e.to_string())?;
        if key.verifying_key().to_bytes() != manifest.device
            || now < manifest.issued_at_ns
            || now >= manifest.expires_at_ns
            || now >= self.expires_at_ns
        {
            return Err("onboarding transfer identity or lifetime changed".into());
        }
        let (nonce, _) = tilo_user_identity::DeviceIdentity::generate("Onboarding range", now);
        let mut request = wire::HandoffRequest {
            version: 1,
            manifest_digest: manifest.manifest_digest().map_err(|e| e.to_string())?,
            device: manifest.device,
            offset: self.position as u64,
            length: self.remaining().min(wire::MAX_HANDOFF_RANGE_BYTES) as u32,
            nonce: nonce.to_bytes(),
            issued_at_ns: now,
            expires_at_ns: now.saturating_add(30_000_000_000).min(self.expires_at_ns),
            signature: [[0; 32]; 2],
        };
        let signature = key
            .sign(&request.signing_digest().map_err(|e| e.to_string())?)
            .to_bytes();
        request.signature[0].copy_from_slice(&signature[..32]);
        request.signature[1].copy_from_slice(&signature[32..]);
        Ok(request)
    }
    pub fn push(
        &mut self,
        request: &wire::HandoffRequest,
        bytes: &[u8],
        now: u64,
    ) -> Result<(), String> {
        if self.failed {
            return Err("onboarding transfer is closed".into());
        }
        // Any failed verification invalidates accumulated bytes; no partial
        // result or retriable state is exposed after a mismatching reply.
        self.failed = true;
        if now >= self.expires_at_ns || request.expires_at_ns > self.expires_at_ns {
            return Err("original onboarding context expired or extended".into());
        }
        if request.offset != self.position as u64 {
            return Err("onboarding range is not sequential".into());
        }
        let manifest =
            wire::HandoffManifest::decode(&self.manifest.bytes).map_err(|e| e.to_string())?;
        let chunk = wire::HandoffChunk::decode(bytes).map_err(|e| e.to_string())?;
        chunk
            .verify_host_signature(&manifest, request, now)
            .map_err(|e| e.to_string())?;
        let end = self
            .position
            .checked_add(chunk.body.len())
            .filter(|end| *end <= self.output.bytes.len())
            .ok_or("onboarding range overflow")?;
        self.output.bytes[self.position..end].copy_from_slice(chunk.body);
        self.position = end;
        while let Some(segment) = manifest.segments.get(self.segment) {
            let segment_end = self
                .segment_start
                .checked_add(segment.length as usize)
                .ok_or("onboarding segment overflow")?;
            if segment_end > end {
                break;
            }
            if canonical_item_digest(&self.output.bytes[self.segment_start..segment_end])
                != segment.digest
            {
                return Err("onboarding segment digest mismatch".into());
            }
            self.segment_start = segment_end;
            self.segment += 1;
        }
        self.failed = false;
        Ok(())
    }
    pub fn remaining(&self) -> usize {
        self.output.bytes.len() - self.position
    }
    pub fn expires_at_ns(&self) -> u64 {
        self.expires_at_ns
    }
    pub fn finish(self, now: u64) -> Result<ChargedBytes, String> {
        let manifest =
            wire::HandoffManifest::decode(&self.manifest.bytes).map_err(|e| e.to_string())?;
        if now >= self.expires_at_ns
            || self.failed
            || self.remaining() != 0
            || self.segment != manifest.segments.len()
            || canonical_item_digest(&self.output.bytes) != manifest.content_digest
        {
            return Err("onboarding transfer is incomplete or corrupt".into());
        }
        Ok(self.output)
    }
}
