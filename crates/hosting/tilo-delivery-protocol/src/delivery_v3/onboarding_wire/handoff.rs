//! Explicit pinned onboarding payloads, never an arbitrary blob read capability.
use super::{
    Deserialize, MAX_RESPONSE_BYTES, OnboardingScope, Serialize, WireError, canonical, current,
    digest, encode, lifetime, verify,
};
pub const HANDOFF_CHUNKS_PATH: &str = "/peer/v3/onboarding/chunks";
pub const MAX_MANIFEST_BYTES: usize = 8192;
pub const MAX_HANDOFF_CHUNK_BYTES: usize = 256 * 1024;
/// Body ceiling leaves space for fixed signed chunk metadata within the frame cap.
pub const MAX_HANDOFF_RANGE_BYTES: usize = 240 * 1024;
pub const MAX_GOVERNANCE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_GOVERNANCE_ITEMS: usize = 4096;
pub const MAX_SEGMENTS: usize = 64;
pub const MAX_SEGMENT_BYTES: u64 = 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandoffPurpose {
    LocalGovernance,
    JoinResponse,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffSegment {
    pub digest: [u8; 32],
    pub length: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffManifest<'a> {
    pub version: u8,
    pub purpose: HandoffPurpose,
    #[serde(borrow)]
    pub scope: OnboardingScope<'a>,
    pub device: [u8; 32],
    pub request_digest: [u8; 32],
    pub request_nonce: [u8; 32],
    pub owner_generation: u64,
    pub server_boot_id: [u8; 16],
    pub issued_at_ns: u64,
    pub expires_at_ns: u64,
    pub total_bytes: u64,
    pub content_digest: [u8; 32],
    #[serde(deserialize_with = "segments")]
    pub segments: Vec<HandoffSegment>,
    pub signature: [[u8; 32]; 2],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffRequest {
    pub version: u8,
    pub manifest_digest: [u8; 32],
    pub device: [u8; 32],
    pub offset: u64,
    pub length: u32,
    pub nonce: [u8; 32],
    pub issued_at_ns: u64,
    pub expires_at_ns: u64,
    pub signature: [[u8; 32]; 2],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffChunk<'a> {
    pub version: u8,
    pub manifest_digest: [u8; 32],
    pub request_digest: [u8; 32],
    pub offset: u64,
    pub body_digest: [u8; 32],
    #[serde(borrow, with = "serde_bytes")]
    pub body: &'a [u8],
    pub signature: [[u8; 32]; 2],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceItem<'a> {
    #[serde(borrow, with = "serde_bytes")]
    pub bytes: &'a [u8],
    pub retention: tilo_peer_state::transport::Retention,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceBundle<'a> {
    #[serde(borrow, deserialize_with = "governance_items")]
    pub items: Vec<GovernanceItem<'a>>,
}
fn bounded_items<'de, D, T, const MAX: usize>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct V<T, const MAX: usize>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const MAX: usize> serde::de::Visitor<'de> for V<T, MAX> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "at most {MAX} bounded items")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut a: A) -> Result<Vec<T>, A::Error> {
            if a.size_hint().is_some_and(|n| n > MAX) {
                return Err(serde::de::Error::custom("handoff item count"));
            }
            let mut values = Vec::new();
            while let Some(value) = a.next_element()? {
                if values.len() == MAX {
                    return Err(serde::de::Error::custom("handoff item count"));
                }
                values
                    .try_reserve(1)
                    .map_err(|_| serde::de::Error::custom("handoff allocation"))?;
                values.push(value);
            }
            Ok(values)
        }
    }
    d.deserialize_seq(V::<T, MAX>(std::marker::PhantomData))
}
fn segments<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<HandoffSegment>, D::Error> {
    bounded_items::<D, HandoffSegment, MAX_SEGMENTS>(d)
}
fn governance_items<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Vec<GovernanceItem<'de>>, D::Error> {
    bounded_items::<D, GovernanceItem<'de>, MAX_GOVERNANCE_ITEMS>(d)
}
impl<'a> HandoffManifest<'a> {
    fn validate(&self) -> Result<(), WireError> {
        self.scope.validate()?;
        lifetime(self.issued_at_ns, self.expires_at_ns, 30_000_000_000)?;
        let max = match self.purpose {
            HandoffPurpose::LocalGovernance => MAX_GOVERNANCE_BYTES,
            HandoffPurpose::JoinResponse => MAX_RESPONSE_BYTES,
        };
        if self.version != 1
            || self.device == [0; 32]
            || self.request_digest == [0; 32]
            || self.request_nonce == [0; 32]
            || self.owner_generation == 0
            || self.server_boot_id == [0; 16]
            || self.content_digest == [0; 32]
            || self.total_bytes == 0
            || self.total_bytes > max as u64
            || self.segments.is_empty()
            || self.segments.len() > MAX_SEGMENTS
        {
            return Err(WireError::Invalid);
        }
        let mut total = 0_u64;
        for item in &self.segments {
            if item.digest == [0; 32] || item.length == 0 || item.length > MAX_SEGMENT_BYTES {
                return Err(WireError::Invalid);
            }
            total = total.checked_add(item.length).ok_or(WireError::Oversized)?;
        }
        if total != self.total_bytes {
            return Err(WireError::Invalid);
        }
        Ok(())
    }
    /// # Errors
    ///
    /// Returns `WireError` if the value violates its wire bounds or cannot be serialized.
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        self.validate()?;
        encode(self, MAX_MANIFEST_BYTES)
    }
    /// # Errors
    ///
    /// Returns `WireError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, WireError> {
        let v: Self = canonical(bytes, MAX_MANIFEST_BYTES)?;
        v.validate()?;
        Ok(v)
    }
    /// # Errors
    ///
    /// Returns `WireError` if the value cannot be canonically encoded.
    pub fn signing_digest(&self) -> Result<[u8; 32], WireError> {
        self.validate()?;
        digest(
            b"tilo-delivery-v3-onboarding-manifest\0",
            &(
                self.version,
                self.purpose,
                self.scope,
                self.device,
                self.request_digest,
                self.request_nonce,
                self.owner_generation,
                self.server_boot_id,
                self.issued_at_ns,
                self.expires_at_ns,
                self.total_bytes,
                self.content_digest,
                &self.segments,
            ),
        )
    }
    /// # Errors
    ///
    /// Returns `WireError` if the value cannot be canonically encoded.
    pub fn manifest_digest(&self) -> Result<[u8; 32], WireError> {
        self.validate()?;
        digest(b"tilo-delivery-v3-onboarding-manifest-id\0", self)
    }
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn verify_host_signature(
        &self,
        scope: OnboardingScope<'_>,
        device: [u8; 32],
        request_digest: [u8; 32],
        nonce: [u8; 32],
        now: u64,
    ) -> Result<(), WireError> {
        self.validate()?;
        current(self.issued_at_ns, self.expires_at_ns, now)?;
        if self.scope != scope
            || self.device != device
            || self.request_digest != request_digest
            || self.request_nonce != nonce
        {
            return Err(WireError::Scope);
        }
        verify(&scope.host, &self.signing_digest()?, &self.signature)
    }
}
impl HandoffRequest {
    fn validate(&self) -> Result<(), WireError> {
        lifetime(self.issued_at_ns, self.expires_at_ns, 30_000_000_000)?;
        if self.version != 1
            || self.manifest_digest == [0; 32]
            || self.device == [0; 32]
            || self.nonce == [0; 32]
            || self.length == 0
            || self.length as usize > MAX_HANDOFF_RANGE_BYTES
            || self.offset.checked_add(u64::from(self.length)).is_none()
        {
            return Err(WireError::Invalid);
        }
        Ok(())
    }
    /// # Errors
    ///
    /// Returns `WireError` if the value violates its wire bounds or cannot be serialized.
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        self.validate()?;
        encode(self, 512)
    }
    /// # Errors
    ///
    /// Returns `WireError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let v: Self = canonical(bytes, 512)?;
        v.validate()?;
        Ok(v)
    }
    /// # Errors
    ///
    /// Returns `WireError` if the value cannot be canonically encoded.
    pub fn signing_digest(&self) -> Result<[u8; 32], WireError> {
        self.validate()?;
        digest(
            b"tilo-delivery-v3-onboarding-chunk-request\0",
            &(
                self.version,
                self.manifest_digest,
                self.device,
                self.offset,
                self.length,
                self.nonce,
                self.issued_at_ns,
                self.expires_at_ns,
            ),
        )
    }
    /// # Errors
    ///
    /// Returns `WireError` if the value cannot be canonically encoded.
    pub fn request_digest(&self) -> Result<[u8; 32], WireError> {
        self.validate()?;
        digest(b"tilo-delivery-v3-onboarding-chunk-request-id\0", self)
    }
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn verify_device_signature(
        &self,
        manifest: &HandoffManifest<'_>,
        now: u64,
    ) -> Result<(), WireError> {
        self.validate()?;
        manifest.validate()?;
        current(self.issued_at_ns, self.expires_at_ns, now)?;
        current(manifest.issued_at_ns, manifest.expires_at_ns, now)?;
        if self.manifest_digest != manifest.manifest_digest()?
            || self.device != manifest.device
            || self
                .offset
                .checked_add(u64::from(self.length))
                .is_none_or(|end| end > manifest.total_bytes)
            || self.expires_at_ns > manifest.expires_at_ns
        {
            return Err(WireError::Scope);
        }
        verify(&manifest.device, &self.signing_digest()?, &self.signature)
    }
}
impl<'a> HandoffChunk<'a> {
    fn validate(&self) -> Result<(), WireError> {
        if self.version != 1
            || self.manifest_digest == [0; 32]
            || self.request_digest == [0; 32]
            || self.body.is_empty()
            || self.body.len() > MAX_HANDOFF_RANGE_BYTES
            || self.body_digest != *blake3::hash(self.body).as_bytes()
        {
            return Err(WireError::Invalid);
        }
        Ok(())
    }
    /// # Errors
    ///
    /// Returns `WireError` if the value violates its wire bounds or cannot be serialized.
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        self.validate()?;
        encode(self, MAX_HANDOFF_CHUNK_BYTES)
    }
    /// # Errors
    ///
    /// Returns `WireError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, WireError> {
        let v: Self = canonical(bytes, MAX_HANDOFF_CHUNK_BYTES)?;
        v.validate()?;
        Ok(v)
    }
    /// # Errors
    ///
    /// Returns `WireError` if the value cannot be canonically encoded.
    pub fn signing_digest(&self) -> Result<[u8; 32], WireError> {
        self.validate()?;
        digest(
            b"tilo-delivery-v3-onboarding-chunk-response\0",
            &(
                self.version,
                self.manifest_digest,
                self.request_digest,
                self.offset,
                self.body_digest,
            ),
        )
    }
    /// Validates transport bytes only; caller additionally verifies accumulated
    /// ordered segment/whole-content digests before original bootstrap admission.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn verify_host_signature(
        &self,
        manifest: &HandoffManifest<'_>,
        request: &HandoffRequest,
        now: u64,
    ) -> Result<(), WireError> {
        self.validate()?;
        request.verify_device_signature(manifest, now)?;
        if self.manifest_digest != request.manifest_digest
            || self.request_digest != request.request_digest()?
            || self.offset != request.offset
            || self.body.len() != request.length as usize
        {
            return Err(WireError::Scope);
        }
        verify(
            &manifest.scope.host,
            &self.signing_digest()?,
            &self.signature,
        )
    }
}
impl<'a> GovernanceBundle<'a> {
    /// Bound before streaming serialization; native can serialize borrowed pins
    /// with a range-writing flavor instead of allocating a second whole bundle.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value violates its wire bounds or cannot be serialized.
    pub fn encoded_len(&self) -> Result<usize, WireError> {
        if self.items.is_empty()
            || self.items.len() > MAX_GOVERNANCE_ITEMS
            || self
                .items
                .iter()
                .any(|i| i.bytes.is_empty() || i.bytes.len() > MAX_GOVERNANCE_BYTES)
        {
            return Err(WireError::Invalid);
        }
        let size = postcard::experimental::serialized_size(self).map_err(|_| WireError::Codec)?;
        if size > MAX_GOVERNANCE_BYTES {
            return Err(WireError::Oversized);
        }
        Ok(size)
    }
    /// # Errors
    ///
    /// Returns `WireError` if the input is not a canonical encoding within this type's bounds.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, WireError> {
        let v: Self = canonical(bytes, MAX_GOVERNANCE_BYTES)?;
        v.encoded_len()?;
        Ok(v)
    }
}
#[cfg(test)]
mod tests;
