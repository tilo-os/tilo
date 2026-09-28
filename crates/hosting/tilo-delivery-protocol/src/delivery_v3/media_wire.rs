//! Bounded inert media control wire values. Decoding never grants authority.
//!
//! Host/current-custody checks belong to the native owner and browser Peer.
//! The inner signature tuples deliberately preserve the existing media protocol.
use serde::{Deserialize, Deserializer, Serialize, de};
use tilo_user_identity::canonical::signing_digest;

/// Complete request/reply ceiling before any decoding allocation.
pub const MAX_CONTROL_BYTES: usize = 8192;
/// Maximum retained canonical grant.
pub const MAX_GRANT_BYTES: usize = 4096;
/// Existing media proof domain; purpose bytes remain unchanged.
pub const MEDIA_PROOF_DOMAIN: &[u8] = b"tilo-media-v1\0";
/// Contextual native challenge attestation domain.
pub const CHALLENGE_DOMAIN: &[u8] = b"tilo-media-control-challenge-v1\0";
/// Contextual native presence attestation domain.
pub const PRESENCE_DOMAIN: &[u8] = b"tilo-media-control-presence-v1\0";
const LIFETIME_NS: u64 = 60_000_000_000;

/// Dedicated first-contact request domain; this proof creates no session authority.
pub const BOOTSTRAP_REQUEST_DOMAIN: &[u8] = b"tilo-delivery-v3-bootstrap-request\0";
/// Dedicated governor attestation for independently appointed host and descriptor floor.
pub const BOOTSTRAP_RESPONSE_DOMAIN: &[u8] = b"tilo-delivery-v3-bootstrap-response\0";
/// Bootstrap request ceiling.
pub const MAX_BOOTSTRAP_REQUEST_BYTES: usize = 4096;
/// Bootstrap response ceiling, including the exact canonical transport hint.
pub const MAX_BOOTSTRAP_RESPONSE_BYTES: usize = 32 * 1024;
/// Inert enrolled-device request; native verifies the current enrolled signer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapRequest {
    /// Exactly one.
    pub version: u8,
    /// Enrolled device signing key.
    pub device: [u8; 32],
    /// Fresh Rust-generated nonce retained until response consumption.
    pub nonce: [u8; 32],
    /// Exact admitted workspace.
    pub workspace: u64,
    /// Independently admitted workspace root.
    pub root: [u8; 32],
    /// Exact admitted room.
    #[serde(deserialize_with = "text128")]
    pub room: String,
    /// Digest of the retained candidate descriptor, not an authority pin.
    pub candidate_digest: [u8; 32],
    /// Enrolled device signature over the dedicated request digest.
    pub signature: [[u8; 32]; 2],
}
/// Inert governor response. Browser must verify against its independently admitted governor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapResponse {
    /// Exactly one.
    pub version: u8,
    /// Exact retained signed request, including nonce.
    pub request: BootstrapRequest,
    /// Canonical `DeviceCert` postcard bytes, at most 4096 bytes.
    #[serde(with = "grant_bytes")]
    pub host_certificate: Vec<u8>,
    /// Exact canonical `TransportHint` postcard bytes, at most 24 KiB.
    #[serde(with = "hint_bytes")]
    pub hint: Vec<u8>,
    /// Owner-stored descriptor generation, never a store-generation substitute.
    pub generation: u64,
    /// Owner-stored descriptor digest, equal to the retained candidate.
    pub descriptor_digest: [u8; 32],
    /// Absolute issue time.
    pub issued_at_ns: u64,
    /// Absolute expiry, at most thirty seconds after issue.
    pub expires_at_ns: u64,
    /// Current governor signature over the dedicated response digest.
    pub signature: [[u8; 32]; 2],
}
impl BootstrapRequest {
    /// Validate structural bounds only.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn validate(&self) -> Result<(), WireError> {
        if self.version != 1 {
            return Err(WireError::Invalid);
        }
        nonzero(self.device)?;
        nonzero(self.nonce)?;
        nonzero(self.root)?;
        nonzero(self.candidate_digest)?;
        text(&self.room, 128)
    }
    /// Exact typed bootstrap signing digest; creates no verified authority.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value cannot be canonically encoded.
    pub fn signing_digest(&self) -> Result<[u8; 32], WireError> {
        self.validate()?;
        digest(
            BOOTSTRAP_REQUEST_DOMAIN,
            &(
                self.version,
                self.device,
                self.nonce,
                self.workspace,
                self.root,
                &self.room,
                self.candidate_digest,
            ),
        )
    }
}
impl BootstrapResponse {
    /// Validate structural bounds only.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn validate(&self) -> Result<(), WireError> {
        self.request.validate()?;
        if self.version != 1
            || self.generation == 0
            || self.descriptor_digest != self.request.candidate_digest
            || self.hint.is_empty()
            || self.hint.len() > 24 * 1024
            || self
                .expires_at_ns
                .checked_sub(self.issued_at_ns)
                .is_none_or(|n| n == 0 || n > 30_000_000_000)
        {
            return Err(WireError::Invalid);
        }
        grant_limit(&self.host_certificate)
    }
    /// Exact typed governor response digest; creates no verified authority.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value cannot be canonically encoded.
    pub fn signing_digest(&self) -> Result<[u8; 32], WireError> {
        self.validate()?;
        digest(
            BOOTSTRAP_RESPONSE_DOMAIN,
            &(
                self.version,
                &self.request,
                serde_bytes::Bytes::new(&self.host_certificate),
                serde_bytes::Bytes::new(&self.hint),
                self.generation,
                self.descriptor_digest,
                self.issued_at_ns,
                self.expires_at_ns,
            ),
        )
    }
}
mod hint_bytes {
    use super::{Deserializer, Serialize, de};
    pub fn serialize<S: serde::Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        serde_bytes::Bytes::new(v).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        struct V;
        impl de::Visitor<'_> for V {
            type Value = Vec<u8>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("1 to 24576 canonical hint bytes")
            }
            fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Vec<u8>, E> {
                if v.is_empty() || v.len() > 24 * 1024 {
                    return Err(E::custom("bootstrap hint exceeds bounds"));
                }
                Ok(v.to_vec())
            }
        }
        d.deserialize_bytes(V)
    }
}

/// Requested or authorized directions; no local microphone consent is implied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaDirections {
    /// Permission to publish.
    pub publish: bool,
    /// Permission to subscribe.
    pub subscribe: bool,
}
/// Exact inert participation intent. Generation zero is reserved for Join reservation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantRequest {
    /// Enrolled device key.
    pub device: [u8; 32],
    /// Native room identifier.
    #[serde(deserialize_with = "text128")]
    pub room: String,
    /// Governed opaque call identifier.
    #[serde(deserialize_with = "text128")]
    pub call_id: String,
    /// Rust-generated fresh participation nonce.
    pub participation_id: [u8; 32],
    /// Native reserved monotonic generation.
    pub generation: u64,
    /// Requested directions.
    pub directions: MediaDirections,
}
/// Existing operation order is part of the signature format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GrantOperation {
    /// Reserve or re-observe the same participation.
    Join,
    /// Refresh grant validity.
    Renew,
    /// End this participation.
    Leave,
    /// Refresh presence only.
    Heartbeat,
}
/// Exact namespace components, never a call-prefix authorization.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaNamespace {
    /// Opaque call component.
    pub call: [u8; 32],
    /// Opaque publisher component.
    pub publisher: [u8; 32],
}
/// Existing grant signature layout; these are unverified claims.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantMetadata {
    /// Participant root.
    pub root: [u8; 32],
    /// Exact admitted intent.
    pub request: GrantRequest,
    /// Governed call epoch.
    pub epoch: u64,
    /// Governed authorization revision.
    pub membership_generation: u64,
    /// Publisher namespace.
    pub namespace: MediaNamespace,
    /// Exact native relay audience.
    #[serde(deserialize_with = "text512")]
    pub audience: String,
    /// Native governor/media signer, distinct from the delivery host device.
    pub host: [u8; 32],
    /// Native media authority boot.
    pub host_boot: [u8; 32],
    /// Absolute issue time.
    pub issued_at_ns: u64,
    /// Absolute expiry, at most sixty seconds from issue.
    pub expires_at_ns: u64,
}
/// Exact existing signed grant encoding; decode is not signature verification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedGrant {
    /// Exactly one.
    pub version: u8,
    /// Inert grant claims.
    pub metadata: GrantMetadata,
    /// Fixed sixty-four signature bytes without a byte-string length prefix.
    pub signature: [[u8; 32]; 2],
}
/// Existing purpose-zero signing tuple.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeChallenge {
    /// Finalized nonzero-generation intent.
    pub request: GrantRequest,
    /// Exact pending operation.
    pub operation: GrantOperation,
    /// Current governed epoch.
    pub epoch: u64,
    /// Current governed authorization revision.
    pub membership_generation: u64,
    /// Current policy directions.
    pub directions: MediaDirections,
    /// Exact relay audience.
    #[serde(deserialize_with = "text512")]
    pub audience: String,
    /// Native media signer.
    pub host: [u8; 32],
    /// Native media boot.
    pub host_boot: [u8; 32],
    /// Single-use nonce.
    pub nonce: [u8; 32],
    /// Issue time.
    pub issued_at_ns: u64,
    /// Expiry time.
    pub expires_at_ns: u64,
}
/// Existing purpose-two signing tuple.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayProofChallenge {
    /// Digest of the exact canonical grant bytes.
    pub grant_digest: [u8; 32],
    /// Exact new relay transport session.
    pub session_id: [u8; 32],
    /// Exact relay audience.
    #[serde(deserialize_with = "text512")]
    pub audience: String,
    /// Native media signer.
    pub host: [u8; 32],
    /// Native media boot.
    pub host_boot: [u8; 32],
    /// Single-use nonce and resulting authorization generation.
    pub nonce: [u8; 32],
    /// Issue time.
    pub issued_at_ns: u64,
    /// Expiry time.
    pub expires_at_ns: u64,
}
/// Existing purpose-five signing tuple.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshChallenge {
    /// Digest of exact replacement grant bytes.
    pub grant_digest: [u8; 32],
    /// Existing relay transport session.
    pub session_id: [u8; 32],
    /// Authorization generation being replaced.
    pub authorization: [u8; 32],
    /// Exact relay audience.
    #[serde(deserialize_with = "text512")]
    pub audience: String,
    /// Native media signer.
    pub host: [u8; 32],
    /// Native media boot.
    pub host_boot: [u8; 32],
    /// Single-use nonce and replacement authorization generation.
    pub nonce: [u8; 32],
    /// Issue time.
    pub issued_at_ns: u64,
    /// Expiry time.
    pub expires_at_ns: u64,
}
/// Closed proof purposes. Serialization of this enum is not the inner signing tuple.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChallengeTranscript {
    /// Native control proof, purpose zero.
    Native(NativeChallenge),
    /// Relay SETUP proof, purpose two.
    Relay(RelayProofChallenge),
    /// Same-session native refresh, purpose five.
    Refresh(RefreshChallenge),
}
impl ChallengeTranscript {
    /// Reconstruct the exact historical domain-separated inner digest.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value cannot be canonically encoded.
    pub fn signing_digest(&self) -> Result<[u8; 32], WireError> {
        self.validate()?;
        match self {
            Self::Native(v) => digest(
                MEDIA_PROOF_DOMAIN,
                &(
                    0_u8,
                    &v.request,
                    v.operation,
                    v.epoch,
                    v.membership_generation,
                    v.directions,
                    &v.audience,
                    v.host,
                    v.host_boot,
                    v.nonce,
                    v.issued_at_ns,
                    v.expires_at_ns,
                ),
            ),
            Self::Relay(v) => digest(
                MEDIA_PROOF_DOMAIN,
                &(
                    2_u8,
                    &v.grant_digest,
                    v.session_id,
                    &v.audience,
                    v.host,
                    v.host_boot,
                    v.nonce,
                    v.issued_at_ns,
                    v.expires_at_ns,
                ),
            ),
            Self::Refresh(v) => digest(
                MEDIA_PROOF_DOMAIN,
                &(
                    5_u8,
                    &v.grant_digest,
                    v.session_id,
                    v.authorization,
                    &v.audience,
                    v.host,
                    v.host_boot,
                    v.nonce,
                    v.issued_at_ns,
                    v.expires_at_ns,
                ),
            ),
        }
    }
    /// Structural limits only; callers must validate retained operation and authority.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn validate(&self) -> Result<(), WireError> {
        match self {
            Self::Native(v) => {
                v.request.validate(false)?;
                if v.epoch == 0 || v.membership_generation == 0 {
                    return Err(WireError::Invalid);
                }
                common_challenge(
                    &v.audience,
                    v.host,
                    v.host_boot,
                    v.nonce,
                    v.issued_at_ns,
                    v.expires_at_ns,
                )
            }
            Self::Relay(v) => {
                nonzero(v.session_id)?;
                common_challenge(
                    &v.audience,
                    v.host,
                    v.host_boot,
                    v.nonce,
                    v.issued_at_ns,
                    v.expires_at_ns,
                )
            }
            Self::Refresh(v) => {
                nonzero(v.session_id)?;
                nonzero(v.authorization)?;
                common_challenge(
                    &v.audience,
                    v.host,
                    v.host_boot,
                    v.nonce,
                    v.issued_at_ns,
                    v.expires_at_ns,
                )
            }
        }
    }
}
/// Exact currently authorized remote publisher set; at most seven other devices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublisherInventory {
    /// Checked owner-monotonic revision of this exact set.
    pub revision: u64,
    /// Strictly sorted unique namespace components.
    #[serde(deserialize_with = "inventory")]
    pub namespaces: Vec<MediaNamespace>,
}
impl PublisherInventory {
    /// Validate count, identities and canonical ordering without granting scope.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn validate(&self) -> Result<(), WireError> {
        if self.revision == 0
            || self.namespaces.len() > 7
            || self
                .namespaces
                .windows(2)
                .any(|pair| matches!(pair, [left, right] if left >= right))
        {
            return Err(WireError::Invalid);
        }
        for v in &self.namespaces {
            nonzero(v.call)?;
            nonzero(v.publisher)?;
        }
        Ok(())
    }
}
/// Owner-signed linkage between delivery custody and current native media policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaContext {
    /// Exactly one.
    pub version: u8,
    /// Already verified complete descriptor digest.
    pub descriptor_digest: [u8; 32],
    /// Already authenticated delivery control session.
    pub delivery_session_id: [u8; 16],
    /// Host-proved delivery boot.
    pub delivery_boot: [u8; 16],
    /// Verified exact HTTPS origin.
    #[serde(deserialize_with = "text512")]
    pub origin: String,
    /// Native workspace identity.
    pub workspace: u64,
    /// Native room identity.
    #[serde(deserialize_with = "text128")]
    pub room: String,
    /// Participant certified root; workspace root remains in the delivery binding.
    pub root: [u8; 32],
    /// Certified delivery host device.
    pub delivery_host: [u8; 32],
    /// Admitted native governor/media grant signer.
    pub media_host: [u8; 32],
    /// Boot authenticated by this envelope, never a JS authority field.
    pub media_boot: [u8; 32],
    /// Finalized exact pending participant intent.
    pub request: GrantRequest,
    /// Current governed call epoch.
    pub epoch: u64,
    /// Current governed authorization revision.
    pub membership_generation: u64,
    /// Current native policy directions.
    pub directions: MediaDirections,
    /// Exact authorized remote namespaces.
    pub inventory: PublisherInventory,
    /// Native issue time.
    pub issued_at_ns: u64,
    /// Actual expiry; receipt cannot extend it.
    pub expires_at_ns: u64,
}
impl MediaContext {
    /// Structural bounds only; decoding is not current owner evidence.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn validate(&self) -> Result<(), WireError> {
        if self.version != 1
            || self.workspace == 0
            || self.epoch == 0
            || self.membership_generation == 0
            || self.room != self.request.room
        {
            return Err(WireError::Invalid);
        }
        self.request.validate(false)?;
        self.inventory.validate()?;
        text(&self.origin, 512)?;
        text(&self.room, 128)?;
        nonzero(self.delivery_session_id)?;
        nonzero(self.delivery_boot)?;
        nonzero(self.root)?;
        nonzero(self.delivery_host)?;
        nonzero(self.media_host)?;
        nonzero(self.media_boot)?;
        lifetime(self.issued_at_ns, self.expires_at_ns)
    }
}
/// Inert signature envelope; verify with the independently pinned native governor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedChallenge {
    /// Native authority context.
    pub context: MediaContext,
    /// Exact inner proof inputs.
    pub transcript: ChallengeTranscript,
    /// Native governor signature over context and transcript.
    pub signature: [[u8; 32]; 2],
}
impl SignedChallenge {
    /// Digest the native owner signs; not a generic browser signing API.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value cannot be canonically encoded.
    pub fn signing_digest(&self) -> Result<[u8; 32], WireError> {
        self.validate()?;
        digest(CHALLENGE_DOMAIN, &(&self.context, &self.transcript))
    }
    /// Validate bounded fields. Identity/cross-context authorization belongs to the client.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn validate(&self) -> Result<(), WireError> {
        self.context.validate()?;
        self.transcript.validate()?;
        let c = &self.context;
        let (host, boot, issued, expires) = match &self.transcript {
            ChallengeTranscript::Native(v) => {
                if c.request != v.request
                    || c.epoch != v.epoch
                    || c.membership_generation != v.membership_generation
                    || c.directions != v.directions
                {
                    return Err(WireError::Invalid);
                }
                (v.host, v.host_boot, v.issued_at_ns, v.expires_at_ns)
            }
            ChallengeTranscript::Relay(v) => (v.host, v.host_boot, v.issued_at_ns, v.expires_at_ns),
            ChallengeTranscript::Refresh(v) => {
                (v.host, v.host_boot, v.issued_at_ns, v.expires_at_ns)
            }
        };
        if c.media_host != host
            || c.media_boot != boot
            || c.issued_at_ns != issued
            || c.expires_at_ns != expires
        {
            return Err(WireError::Invalid);
        }
        Ok(())
    }
}
/// Signed presence expiry and exact current inventory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedPresence {
    /// Current bounded native context; expiry is presence expiry.
    pub context: MediaContext,
    /// Native governor signature.
    pub signature: [[u8; 32]; 2],
}
impl SignedPresence {
    /// Digest under the distinct presence domain.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value cannot be canonically encoded.
    pub fn signing_digest(&self) -> Result<[u8; 32], WireError> {
        self.validate()?;
        digest(PRESENCE_DOMAIN, &self.context)
    }
    /// Presence never extends past fifteen seconds from issue.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn validate(&self) -> Result<(), WireError> {
        self.context.validate()?;
        if self
            .context
            .expires_at_ns
            .saturating_sub(self.context.issued_at_ns)
            > 15_000_000_000
        {
            return Err(WireError::Invalid);
        }
        Ok(())
    }
}
/// Canonical versioned request. Each variant has exactly one HTTP target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlRequest {
    /// Exactly one.
    pub version: u8,
    /// Closed control operation.
    pub command: ControlCommand,
}
/// Inert commands, always subject to current HTTPS `RequestProof` admission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlCommand {
    /// Native operation challenge; Join generation zero reserves a generation.
    GrantChallenge {
        /// Exact requested intent.
        request: GrantRequest,
        /// Native operation.
        operation: GrantOperation,
    },
    /// Complete a caller-bound single-use native challenge.
    GrantComplete {
        /// Native nonce.
        nonce: [u8; 32],
        /// Exact device signature.
        signature: [[u8; 32]; 2],
    },
    /// Request SETUP authorization for the exact grant.
    RelayChallenge {
        /// Canonical grant bytes.
        #[serde(with = "grant_bytes")]
        grant: Vec<u8>,
    },
    /// Replace authorization on the same live relay transport.
    RefreshChallenge {
        /// Existing relay session.
        session_id: [u8; 32],
        /// Exact replacement grant bytes.
        #[serde(with = "grant_bytes")]
        grant: Vec<u8>,
    },
    /// Complete same-session authorization replacement.
    RefreshComplete {
        /// Existing relay session.
        session_id: [u8; 32],
        /// Single-use refresh nonce.
        nonce: [u8; 32],
        /// Device signature.
        signature: [[u8; 32]; 2],
    },
    /// Close only the authenticated caller's exact relay session.
    RelayClose {
        /// Existing relay session.
        session_id: [u8; 32],
    },
    /// Observe exact authorized inventory; heartbeat uses `GrantOperation`.
    Presence {
        /// Existing relay session.
        session_id: [u8; 32],
    },
}
impl ControlCommand {
    /// Exact raw POST target with no aliases, query, or supplied URL.
    pub fn target(&self) -> &'static str {
        match self {
            Self::GrantChallenge { .. } => "/peer/v3/media/grant/challenge",
            Self::GrantComplete { .. } => "/peer/v3/media/grant/complete",
            Self::RelayChallenge { .. } => "/peer/v3/media/relay/challenge",
            Self::RefreshChallenge { .. } => "/peer/v3/media/relay/refresh/challenge",
            Self::RefreshComplete { .. } => "/peer/v3/media/relay/refresh/complete",
            Self::RelayClose { .. } => "/peer/v3/media/relay/close",
            Self::Presence { .. } => "/peer/v3/media/presence",
        }
    }
}
impl ControlRequest {
    /// Check only wire shape; native admission remains required.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn validate(&self) -> Result<(), WireError> {
        if self.version != 1 {
            return Err(WireError::Invalid);
        }
        match &self.command {
            ControlCommand::GrantChallenge { request, operation } => {
                request.validate(*operation == GrantOperation::Join)
            }
            ControlCommand::GrantComplete { nonce, .. } => nonzero(*nonce),
            ControlCommand::RelayChallenge { grant } => grant_limit(grant),
            ControlCommand::RefreshChallenge { session_id, grant } => {
                nonzero(*session_id)?;
                grant_limit(grant)
            }
            ControlCommand::RefreshComplete {
                session_id, nonce, ..
            } => {
                nonzero(*session_id)?;
                nonzero(*nonce)
            }
            ControlCommand::RelayClose { session_id } | ControlCommand::Presence { session_id } => {
                nonzero(*session_id)
            }
        }
    }
}
/// Versioned bounded native response; errors are closed codes, never diagnostic text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlReply {
    /// Exactly one.
    pub version: u8,
    /// Exact operation result.
    pub result: ControlResult,
}
/// Closed native control results.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::large_enum_variant,
    reason = "public wire result; boxing would change every constructor and match in dependent crates"
)]
pub enum ControlResult {
    /// Native-signed complete challenge.
    Challenge(SignedChallenge),
    /// Native-signed canonical grant; not a bearer authorization.
    Granted {
        /// Exact grant bytes.
        #[serde(with = "grant_bytes")]
        grant: Vec<u8>,
    },
    /// Participation ended.
    Left,
    /// Presence/namespace state signed by the native governor.
    Present(SignedPresence),
    /// Same relay session now uses the replacement grant.
    Refreshed {
        /// Exact relay session.
        session_id: [u8; 32],
        /// Actual replacement expiry.
        expires_at_ns: u64,
    },
    /// Relay transport authorization closed.
    Closed,
    /// Closed failure code.
    Error(MediaCode),
}
/// Stable bounded failures; no token or authority diagnostics are serialized.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaCode {
    /// Invalid bounded request.
    InvalidRequest,
    /// Current authority denies.
    Unauthorized,
    /// Replaced or left participation.
    StaleGeneration,
    /// Bounded service is full.
    Capacity,
    /// Signature or bound purpose mismatch.
    InvalidProof,
    /// Missing or consumed challenge.
    UnknownChallenge,
    /// Validity expired.
    Expired,
    /// Invalid canonical grant.
    InvalidGrant,
    /// Owner custody could not sign.
    SigningFailed,
    /// Secure entropy unavailable.
    EntropyUnavailable,
    /// Installation/current authority temporarily unavailable.
    Unavailable,
    /// Unsupported version or feature.
    Unsupported,
}
impl ControlReply {
    /// Structural bounds, not proof of native authority.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn validate(&self) -> Result<(), WireError> {
        if self.version != 1 {
            return Err(WireError::Invalid);
        }
        match &self.result {
            ControlResult::Challenge(v) => v.validate(),
            ControlResult::Granted { grant } => grant_limit(grant),
            ControlResult::Present(v) => v.validate(),
            ControlResult::Refreshed {
                session_id,
                expires_at_ns,
            } => {
                nonzero(*session_id)?;
                if *expires_at_ns == 0 {
                    return Err(WireError::Invalid);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}
impl GrantRequest {
    /// Validate shape; zero generation is allowed only for a Join reservation.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn validate(&self, reservation: bool) -> Result<(), WireError> {
        nonzero(self.device)?;
        nonzero(self.participation_id)?;
        text(&self.room, 128)?;
        if self.call_id.len() != 64
            || !self
                .call_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.call_id.bytes().all(|b| b == b'0')
            || (!reservation && self.generation == 0)
            || (!self.directions.publish && !self.directions.subscribe)
        {
            return Err(WireError::Invalid);
        }
        Ok(())
    }
}
impl SignedGrant {
    /// Existing purpose-one native grant digest.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value cannot be canonically encoded.
    pub fn signing_digest(&self) -> Result<[u8; 32], WireError> {
        self.validate()?;
        digest(MEDIA_PROOF_DOMAIN, &(1_u8, &self.metadata))
    }
    /// Validate bounded metadata, not the governor signature.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn validate(&self) -> Result<(), WireError> {
        let m = &self.metadata;
        if self.version != 1 || m.epoch == 0 || m.membership_generation == 0 {
            return Err(WireError::Invalid);
        }
        m.request.validate(false)?;
        nonzero(m.root)?;
        nonzero(m.namespace.call)?;
        nonzero(m.namespace.publisher)?;
        common_challenge(
            &m.audience,
            m.host,
            m.host_boot,
            [1; 32],
            m.issued_at_ns,
            m.expires_at_ns,
        )
    }
}
/// Standard opaque SETUP submission, exactly 129 bytes. Never a URL token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelaySubmission {
    /// Exactly one.
    pub version: u8,
    /// Exact relay session.
    pub session: [u8; 32],
    /// Native challenge nonce.
    pub nonce: [u8; 32],
    /// Device proof under existing purpose two.
    pub signature: [[u8; 32]; 2],
}
impl RelaySubmission {
    /// Only structural validation, not relay admission.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the value fails validation or verification.
    pub fn validate(&self) -> Result<(), WireError> {
        if self.version != 1 {
            return Err(WireError::Invalid);
        }
        nonzero(self.session)?;
        nonzero(self.nonce)
    }
}
macro_rules! codec {
    ($ty:ty,$cap:expr) => {
        impl $ty {
            /// Canonical encoding after checking all structural bounds.
            ///
            /// # Errors
            ///
            /// Returns `WireError` if the value violates its wire bounds or cannot be serialized.
            pub fn encode(&self) -> Result<Vec<u8>, WireError> {
                self.validate()?;
                let out = postcard::to_allocvec(self).map_err(|_| WireError::Codec)?;
                if out.len() > $cap {
                    return Err(WireError::Codec);
                }
                Ok(out)
            }
            /// Reject outer size before parsing, bounded fields before copies, and noncanonical tails.
            ///
            /// # Errors
            ///
            /// Returns `WireError` if the input is not a canonical encoding within this type's bounds.
            pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
                if bytes.is_empty() || bytes.len() > $cap {
                    return Err(WireError::Codec);
                }
                let value: Self = postcard::from_bytes(bytes).map_err(|_| WireError::Codec)?;
                if value.encode()? != bytes {
                    return Err(WireError::Codec);
                }
                Ok(value)
            }
        }
    };
}
codec!(BootstrapRequest, MAX_BOOTSTRAP_REQUEST_BYTES);
codec!(BootstrapResponse, MAX_BOOTSTRAP_RESPONSE_BYTES);
codec!(ControlRequest, MAX_CONTROL_BYTES);
codec!(ControlReply, MAX_CONTROL_BYTES);
codec!(SignedChallenge, MAX_CONTROL_BYTES);
codec!(SignedPresence, MAX_CONTROL_BYTES);
codec!(SignedGrant, MAX_GRANT_BYTES);
codec!(RelaySubmission, 129);
/// Generate an ephemeral nonce using the existing secure entropy provider.
/// This creates no signing identity and conveys no authority.
///
/// # Errors
///
/// Returns `WireError` if the inputs violate this operation's bounds or invariants.
pub fn fresh_nonce() -> Result<[u8; 32], WireError> {
    let mut nonce = [0; 32];
    getrandom::fill(&mut nonce).map_err(|_| WireError::Entropy)?;
    nonzero(nonce)?;
    Ok(nonce)
}
/// Digest bounded canonical grant bytes for the existing relay proof tuple.
///
/// # Errors
///
/// Returns `WireError` if the value cannot be canonically encoded.
pub fn grant_digest(bytes: &[u8]) -> Result<[u8; 32], WireError> {
    grant_limit(bytes)?;
    Ok(*blake3::hash(bytes).as_bytes())
}
/// Convert a fixed signature into its existing Postcard array shape.
pub fn split_signature(signature: [u8; 64]) -> [[u8; 32]; 2] {
    let mut v = [[0; 32]; 2];
    v[0].copy_from_slice(&signature[..32]);
    v[1].copy_from_slice(&signature[32..]);
    v
}
/// Restore a fixed signature without allocation.
pub fn join_signature(v: [[u8; 32]; 2]) -> [u8; 64] {
    let mut s = [0; 64];
    s[..32].copy_from_slice(&v[0]);
    s[32..].copy_from_slice(&v[1]);
    s
}
fn digest<T: Serialize>(domain: &[u8], v: &T) -> Result<[u8; 32], WireError> {
    signing_digest(domain, v).map_err(|_| WireError::Codec)
}
fn nonzero<const N: usize>(v: [u8; N]) -> Result<(), WireError> {
    if v == [0; N] {
        Err(WireError::Invalid)
    } else {
        Ok(())
    }
}
fn text(v: &str, max: usize) -> Result<(), WireError> {
    if v.is_empty() || v.len() > max || v.chars().any(char::is_control) {
        Err(WireError::Invalid)
    } else {
        Ok(())
    }
}
fn lifetime(start: u64, end: u64) -> Result<(), WireError> {
    if end
        .checked_sub(start)
        .is_none_or(|n| n == 0 || n > LIFETIME_NS)
    {
        Err(WireError::Invalid)
    } else {
        Ok(())
    }
}
fn common_challenge(
    audience: &str,
    host: [u8; 32],
    boot: [u8; 32],
    nonce: [u8; 32],
    start: u64,
    end: u64,
) -> Result<(), WireError> {
    text(audience, 512)?;
    nonzero(host)?;
    nonzero(boot)?;
    nonzero(nonce)?;
    lifetime(start, end)
}
fn grant_limit(v: &[u8]) -> Result<(), WireError> {
    if v.is_empty() || v.len() > MAX_GRANT_BYTES {
        Err(WireError::Invalid)
    } else {
        Ok(())
    }
}
fn bounded_text<'de, D: Deserializer<'de>, const MAX: usize>(d: D) -> Result<String, D::Error> {
    struct V<const N: usize>;
    impl<const N: usize> de::Visitor<'_> for V<N> {
        type Value = String;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "nonempty text of at most {N} bytes")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
            text(v, N).map_err(E::custom)?;
            Ok(v.to_owned())
        }
    }
    d.deserialize_str(V::<MAX>)
}
fn text128<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    bounded_text::<D, 128>(d)
}
fn text512<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    bounded_text::<D, 512>(d)
}
mod grant_bytes {
    use super::{Deserializer, Serialize, de, grant_limit};
    pub fn serialize<S: serde::Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        serde_bytes::Bytes::new(v).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        struct V;
        impl de::Visitor<'_> for V {
            type Value = Vec<u8>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("1 to 4096 grant bytes")
            }
            fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Vec<u8>, E> {
                grant_limit(v).map_err(E::custom)?;
                Ok(v.to_vec())
            }
        }
        d.deserialize_bytes(V)
    }
}
fn inventory<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<MediaNamespace>, D::Error> {
    struct V;
    impl<'de> de::Visitor<'de> for V {
        type Value = Vec<MediaNamespace>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("at most seven exact namespaces")
        }
        fn visit_seq<A: de::SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
            if a.size_hint().is_some_and(|n| n > 7) {
                return Err(de::Error::custom("publisher count"));
            }
            let mut out = Vec::new();
            while let Some(v) = a.next_element()? {
                if out.len() == 7 {
                    return Err(de::Error::custom("publisher count"));
                }
                out.push(v);
            }
            Ok(out)
        }
    }
    d.deserialize_seq(V)
}
/// Closed codec/shape failures; no authority claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// Existing entropy provider failed.
    #[error("media entropy unavailable")]
    Entropy,
    /// Malformed, oversized, truncated, or noncanonical encoding.
    #[error("invalid media wire encoding")]
    Codec,
    /// Invalid bounded inert field.
    #[error("invalid media wire field")]
    Invalid,
}
#[cfg(test)]
mod tests;
