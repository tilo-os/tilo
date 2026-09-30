//! Secretless, endpoint-bound delivery objects and a durable named-share inbox.
//!
//! The envelope contains no bearer material. Its invite is scoped to one
//! certified human and the issuer signs the exact record-host endpoint. Network
//! delivery additionally uses the mutually certified FS transport; the inbox
//! repeats the subject check before making an entry discoverable locally.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use collab_types::{
    Ed25519PublicKey, FileInviteSubject, FileInviteV1, Signature, VerifyError, canonical_bytes,
    sign_event, verify_event,
};
use iroh::endpoint::{Connection, Endpoint};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh_base::EndpointAddr;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tilo_user_identity::RevocationList;
use uuid::Uuid;
use wire_frame::Framing;

use crate::authenticated_transport::{AuthenticatedTransportConfig, authenticate_fs_peer_parts_at};
use crate::canonical_frame::{self, CanonicalFrameError};
use crate::clock::{unix_now_ms, unix_now_ns};
use crate::identity::PeerCredentials;

pub const NAMED_SHARE_ENVELOPE_VERSION: u16 = 1;
pub const NAMED_SHARE_INBOX_VERSION: u16 = 1;
pub const MAX_NAMED_SHARE_ENVELOPE_BYTES: usize = 32 * 1024;
pub const MAX_NAMED_SHARE_INBOX_ENTRIES: usize = 4_096;
pub const MAX_NAMED_SHARE_INBOX_FILE_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_NAMED_SHARE_RECIPIENT_DEVICES: usize = 32;
const NAMED_SHARE_ENDPOINT_DOMAIN: &str = "tilo-named-share-endpoint-v1";
pub const TILO_FS_NAMED_SHARE_INBOX_ALPN: &[u8] = b"tilo-fs-named-inbox/1";
pub const NAMED_SHARE_DELIVERY_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedShareEnvelopeV1 {
    pub schema_version: u16,
    pub endpoint: EndpointAddr,
    pub invite: FileInviteV1,
    pub endpoint_binding_signature: Signature,
}

#[derive(Serialize)]
struct EndpointBinding<'a> {
    tag: &'static str,
    endpoint: &'a EndpointAddr,
    invite: &'a FileInviteV1,
}

impl NamedShareEnvelopeV1 {
    /// Bind a named-person invite to the endpoint that will deliver it.
    ///
    /// # Errors
    ///
    /// Returns an error when the invite, issuer, endpoint, or resulting
    /// envelope is invalid or exceeds its encoded size bound.
    pub fn bind_at(
        endpoint: EndpointAddr,
        invite: FileInviteV1,
        issuer: &ed25519_dalek::SigningKey,
        now_ms: u64,
    ) -> Result<Self, NamedShareInboxError> {
        invite
            .verify_at(now_ms)
            .map_err(|_| NamedShareInboxError::InvalidEnvelope)?;
        if endpoint.is_empty()
            || !matches!(invite.subject, FileInviteSubject::Person(_))
            || invite.issuer.as_bytes() != &issuer.verifying_key().to_bytes()
        {
            return Err(NamedShareInboxError::InvalidEnvelope);
        }
        let endpoint_binding_signature = sign_event(
            &EndpointBinding {
                tag: NAMED_SHARE_ENDPOINT_DOMAIN,
                endpoint: &endpoint,
                invite: &invite,
            },
            issuer,
        )
        .map_err(|_| NamedShareInboxError::InvalidEnvelope)?;
        let envelope = Self {
            schema_version: NAMED_SHARE_ENVELOPE_VERSION,
            endpoint,
            invite,
            endpoint_binding_signature,
        };
        envelope.validate_for_at(envelope.recipient()?, now_ms)?;
        Ok(envelope)
    }

    /// Return the named human recipient.
    ///
    /// # Errors
    ///
    /// Returns an error for bearer invites, which cannot use named delivery.
    pub fn recipient(&self) -> Result<Ed25519PublicKey, NamedShareInboxError> {
        match self.invite.subject {
            FileInviteSubject::Person(person) => Ok(person),
            FileInviteSubject::Bearer { .. } => Err(NamedShareInboxError::InvalidEnvelope),
        }
    }

    /// Validate this envelope for `local_person` at `now_ms`.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid, expired, misaddressed, incorrectly
    /// signed, or oversized envelopes.
    pub fn validate_for_at(
        &self,
        local_person: Ed25519PublicKey,
        now_ms: u64,
    ) -> Result<(), NamedShareInboxError> {
        if self.schema_version != NAMED_SHARE_ENVELOPE_VERSION
            || self.endpoint.is_empty()
            || self.recipient()? != local_person
        {
            return Err(NamedShareInboxError::InvalidEnvelope);
        }
        self.invite
            .verify_at(now_ms)
            .map_err(|_| NamedShareInboxError::InvalidEnvelope)?;
        verify_event(
            &EndpointBinding {
                tag: NAMED_SHARE_ENDPOINT_DOMAIN,
                endpoint: &self.endpoint,
                invite: &self.invite,
            },
            &self.endpoint_binding_signature,
            &self.invite.issuer,
        )
        .map_err(NamedShareInboxError::BadEndpointBinding)?;
        if canonical_bytes(self)
            .map_err(|_| NamedShareInboxError::InvalidEnvelope)?
            .len()
            > MAX_NAMED_SHARE_ENVELOPE_BYTES
        {
            return Err(NamedShareInboxError::EnvelopeTooLarge);
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InboxDocumentV1 {
    schema_version: u16,
    entries: Vec<NamedShareEnvelopeV1>,
}

pub struct NamedShareInbox {
    path: PathBuf,
    local_person: Ed25519PublicKey,
    entries: Mutex<BTreeMap<Uuid, NamedShareEnvelopeV1>>,
}

/// One crash-safe delivery job. A job is complete after one currently
/// certified device belonging to the named human has authenticated and
/// durably accepted it. Remaining device endpoints are deliberately not a
/// broadcast list: this limits metadata disclosure and retry traffic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedShareOutboxEntryV1 {
    pub envelope: NamedShareEnvelopeV1,
    pub recipient_devices: Vec<Ed25519PublicKey>,
    pub delivered_to: Option<Ed25519PublicKey>,
    pub attempts: u32,
    pub last_attempt_at_ms: Option<u64>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutboxDocumentV1 {
    schema_version: u16,
    entries: Vec<NamedShareOutboxEntryV1>,
}

pub struct NamedShareOutbox {
    path: PathBuf,
    entries: Mutex<BTreeMap<Uuid, NamedShareOutboxEntryV1>>,
}

impl NamedShareOutbox {
    /// Open or create an owner-only durable delivery outbox.
    ///
    /// # Errors
    ///
    /// Returns an error when state cannot be read, validated, or persisted.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, NamedShareInboxError> {
        let path = path.into();
        let entries = match std::fs::metadata(&path) {
            Ok(metadata) => {
                validate_owner_only(&metadata)?;
                if metadata.len() > MAX_NAMED_SHARE_INBOX_FILE_BYTES {
                    return Err(NamedShareInboxError::StateTooLarge);
                }
                let document: OutboxDocumentV1 = serde_json::from_slice(&std::fs::read(&path)?)
                    .map_err(|_| NamedShareInboxError::MalformedState)?;
                if document.schema_version != NAMED_SHARE_INBOX_VERSION {
                    return Err(NamedShareInboxError::UnsupportedVersion);
                }
                if document.entries.len() > MAX_NAMED_SHARE_INBOX_ENTRIES {
                    return Err(NamedShareInboxError::CapacityReached);
                }
                let mut entries = BTreeMap::new();
                for entry in document.entries {
                    validate_devices(&entry.recipient_devices)?;
                    validate_persisted_envelope(&entry.envelope)?;
                    if entries.insert(entry.envelope.invite.id, entry).is_some() {
                        return Err(NamedShareInboxError::GrantCollision);
                    }
                }
                entries
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error.into()),
        };
        let outbox = Self {
            path,
            entries: Mutex::new(entries),
        };
        if !outbox.path.exists() {
            outbox.persist(&BTreeMap::new())?;
        }
        Ok(outbox)
    }

    /// Durably enqueue an envelope for a bounded set of certified devices.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input, collisions, capacity exhaustion, or
    /// persistence failure.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the lock is held across persist so the file and the in-memory map change together"
    )]
    pub fn enqueue(
        &self,
        envelope: NamedShareEnvelopeV1,
        recipient_devices: Vec<Ed25519PublicKey>,
    ) -> Result<bool, NamedShareInboxError> {
        validate_devices(&recipient_devices)?;
        validate_persisted_envelope(&envelope)?;
        if recipient_devices.is_empty() {
            return Err(NamedShareInboxError::NoRecipientDevices);
        }
        let mut current = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = current.get(&envelope.invite.id) {
            return if existing.envelope == envelope
                && existing.recipient_devices == recipient_devices
            {
                Ok(false)
            } else {
                Err(NamedShareInboxError::GrantCollision)
            };
        }
        if current.len() >= MAX_NAMED_SHARE_INBOX_ENTRIES {
            return Err(NamedShareInboxError::CapacityReached);
        }
        let mut next = current.clone();
        next.insert(
            envelope.invite.id,
            NamedShareOutboxEntryV1 {
                envelope,
                recipient_devices,
                delivered_to: None,
                attempts: 0,
                last_attempt_at_ms: None,
            },
        );
        self.persist(&next)?;
        *current = next;
        Ok(true)
    }

    /// Return non-expired deliveries that have not yet succeeded.
    pub fn pending_at(&self, now_ms: u64) -> Vec<NamedShareOutboxEntryV1> {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries
            .values()
            .filter(|entry| {
                entry.delivered_to.is_none()
                    && entry
                        .envelope
                        .invite
                        .expires_at_ms
                        .is_none_or(|deadline| now_ms < deadline)
            })
            .cloned()
            .collect()
    }

    /// Persist one delivery attempt and optional successful recipient device.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown grant, an unlisted device, or a
    /// persistence failure.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the lock is held across persist so the file and the in-memory map change together"
    )]
    pub fn record_attempt(
        &self,
        grant_id: Uuid,
        delivered_to: Option<Ed25519PublicKey>,
        now_ms: u64,
    ) -> Result<(), NamedShareInboxError> {
        let mut current = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(existing) = current.get(&grant_id) else {
            return Err(NamedShareInboxError::UnknownGrant);
        };
        if let Some(device) = delivered_to
            && !existing.recipient_devices.contains(&device)
        {
            return Err(NamedShareInboxError::InvalidRecipientDevices);
        }
        let mut next = current.clone();
        let entry = next
            .get_mut(&grant_id)
            .ok_or(NamedShareInboxError::UnknownGrant)?;
        entry.attempts = entry.attempts.saturating_add(1);
        entry.last_attempt_at_ms = Some(now_ms);
        if delivered_to.is_some() {
            entry.delivered_to = delivered_to;
        }
        self.persist(&next)?;
        *current = next;
        Ok(())
    }

    /// Return one queued delivery, if present.
    pub fn get(&self, grant_id: Uuid) -> Option<NamedShareOutboxEntryV1> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&grant_id)
            .cloned()
    }

    /// Cancel one queued delivery.
    ///
    /// # Errors
    ///
    /// Returns an error when the updated outbox cannot be persisted.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the lock is held across persist so the file and the in-memory map change together"
    )]
    pub fn cancel(&self, grant_id: Uuid) -> Result<bool, NamedShareInboxError> {
        let mut current = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !current.contains_key(&grant_id) {
            return Ok(false);
        }
        let mut next = current.clone();
        next.remove(&grant_id);
        self.persist(&next)?;
        *current = next;
        Ok(true)
    }

    fn persist(
        &self,
        entries: &BTreeMap<Uuid, NamedShareOutboxEntryV1>,
    ) -> Result<(), NamedShareInboxError> {
        let bytes = serde_json::to_vec(&OutboxDocumentV1 {
            schema_version: NAMED_SHARE_INBOX_VERSION,
            entries: entries.values().cloned().collect(),
        })
        .map_err(|_| NamedShareInboxError::MalformedState)?;
        if bytes.len() as u64 > MAX_NAMED_SHARE_INBOX_FILE_BYTES {
            return Err(NamedShareInboxError::StateTooLarge);
        }
        if let Some(parent) = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        atomic_file::write_with_mode(&self.path, &bytes, 0o600)?;
        Ok(())
    }
}

fn validate_devices(devices: &[Ed25519PublicKey]) -> Result<(), NamedShareInboxError> {
    if devices.len() > MAX_NAMED_SHARE_RECIPIENT_DEVICES {
        return Err(NamedShareInboxError::InvalidRecipientDevices);
    }
    let mut unique = std::collections::BTreeSet::new();
    for device in devices {
        ed25519_dalek::VerifyingKey::from_bytes(device.as_bytes())
            .map_err(|_| NamedShareInboxError::InvalidRecipientDevices)?;
        if !unique.insert(*device) {
            return Err(NamedShareInboxError::InvalidRecipientDevices);
        }
    }
    Ok(())
}

fn validate_persisted_envelope(
    envelope: &NamedShareEnvelopeV1,
) -> Result<(), NamedShareInboxError> {
    let recipient = envelope.recipient()?;
    let validation_time = envelope
        .invite
        .expires_at_ms
        .map_or_else(unix_now_ms, |deadline| deadline.saturating_sub(1));
    envelope.validate_for_at(recipient, validation_time)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryRequestV1 {
    protocol_version: u16,
    envelope: NamedShareEnvelopeV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DeliveryResponseV1 {
    Accepted { grant_id: Uuid, inserted: bool },
    Refused,
}

#[derive(Clone)]
pub struct NamedShareInboxProtocol {
    endpoint: Endpoint,
    credentials: PeerCredentials,
    revocations: Arc<RwLock<RevocationList>>,
    inbox: Arc<NamedShareInbox>,
}

impl std::fmt::Debug for NamedShareInboxProtocol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NamedShareInboxProtocol")
            .field("endpoint", &self.endpoint.id())
            .finish_non_exhaustive()
    }
}

impl NamedShareInboxProtocol {
    /// Construct an inbox protocol after checking its local identities agree.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint, device certificate, and inbox human
    /// do not describe the same local identity.
    pub fn new(
        endpoint: Endpoint,
        credentials: PeerCredentials,
        revocations: Arc<RwLock<RevocationList>>,
        inbox: Arc<NamedShareInbox>,
    ) -> Result<Self, NamedShareDeliveryError> {
        if credentials.device_public_key() != *endpoint.id().as_bytes()
            || Ed25519PublicKey::from_bytes(credentials.user_public_key()) != inbox.local_person
        {
            return Err(NamedShareDeliveryError::LocalIdentityMismatch);
        }
        Ok(Self {
            endpoint,
            credentials,
            revocations,
            inbox,
        })
    }

    async fn serve(&self, connection: &Connection) -> Result<(), NamedShareDeliveryError> {
        let (mut send, mut recv) = connection
            .accept_bi()
            .await
            .map_err(|error| NamedShareDeliveryError::Transport(error.to_string()))?;
        let revocations = self
            .revocations
            .read()
            .map_err(|_| NamedShareDeliveryError::RevocationStatePoisoned)?
            .clone();
        let peer = authenticate_fs_peer_parts_at(
            &mut recv,
            &mut send,
            &self.credentials,
            *self.endpoint.id().as_bytes(),
            *connection.remote_id().as_bytes(),
            &revocations,
            unix_now_ns(),
            AuthenticatedTransportConfig::default(),
        )
        .await?;
        let request: DeliveryRequestV1 = read_wire(&mut recv).await?;
        let response = if request.protocol_version != NAMED_SHARE_ENVELOPE_VERSION
            || request.envelope.invite.issuer.as_bytes() != &peer.user_public_key
        {
            DeliveryResponseV1::Refused
        } else {
            match self
                .inbox
                .accept_at(request.envelope.clone(), unix_now_ms())
            {
                Ok(inserted) => DeliveryResponseV1::Accepted {
                    grant_id: request.envelope.invite.id,
                    inserted,
                },
                Err(_) => DeliveryResponseV1::Refused,
            }
        };
        write_wire(&mut send, &response).await?;
        let _ = send.finish();
        Ok(())
    }
}

impl ProtocolHandler for NamedShareInboxProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        match tokio::time::timeout(NAMED_SHARE_DELIVERY_TIMEOUT, self.serve(&connection)).await {
            Ok(Ok(())) => {
                connection.closed().await;
                Ok(())
            }
            Ok(Err(error)) => Err(AcceptError::from_err(std::io::Error::other(
                error.to_string(),
            ))),
            Err(_) => Err(AcceptError::from_err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "named-share delivery timed out",
            ))),
        }
    }
}

#[derive(Clone)]
pub struct NamedShareDeliveryClient {
    endpoint: Endpoint,
    credentials: PeerCredentials,
    revocations: Arc<RwLock<RevocationList>>,
}

impl NamedShareDeliveryClient {
    /// Construct a delivery client for the certified local endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint does not match the certified device.
    pub fn new(
        endpoint: Endpoint,
        credentials: PeerCredentials,
        revocations: Arc<RwLock<RevocationList>>,
    ) -> Result<Self, NamedShareDeliveryError> {
        if credentials.device_public_key() != *endpoint.id().as_bytes() {
            return Err(NamedShareDeliveryError::LocalIdentityMismatch);
        }
        Ok(Self {
            endpoint,
            credentials,
            revocations,
        })
    }

    /// Deliver an envelope to one certified recipient endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error on authentication, transport, recipient, protocol, or
    /// timeout failure.
    pub async fn deliver(
        &self,
        recipient: EndpointAddr,
        envelope: NamedShareEnvelopeV1,
    ) -> Result<bool, NamedShareDeliveryError> {
        let exchange = async {
            let connection = self
                .endpoint
                .connect(recipient.clone(), TILO_FS_NAMED_SHARE_INBOX_ALPN)
                .await
                .map_err(|error| NamedShareDeliveryError::Transport(error.to_string()))?;
            if connection.remote_id() != recipient.id {
                return Err(NamedShareDeliveryError::RemoteEndpointMismatch);
            }
            let (mut send, mut recv) = connection
                .open_bi()
                .await
                .map_err(|error| NamedShareDeliveryError::Transport(error.to_string()))?;
            let revocations = self
                .revocations
                .read()
                .map_err(|_| NamedShareDeliveryError::RevocationStatePoisoned)?
                .clone();
            let peer = authenticate_fs_peer_parts_at(
                &mut recv,
                &mut send,
                &self.credentials,
                *self.endpoint.id().as_bytes(),
                *recipient.id.as_bytes(),
                &revocations,
                unix_now_ns(),
                AuthenticatedTransportConfig::default(),
            )
            .await?;
            if peer.user_public_key != *envelope.recipient()?.as_bytes() {
                return Err(NamedShareDeliveryError::WrongRecipient);
            }
            write_wire(
                &mut send,
                &DeliveryRequestV1 {
                    protocol_version: NAMED_SHARE_ENVELOPE_VERSION,
                    envelope: envelope.clone(),
                },
            )
            .await?;
            let response: DeliveryResponseV1 = read_wire(&mut recv).await?;
            connection.close(0u32.into(), b"");
            match response {
                DeliveryResponseV1::Accepted { grant_id, inserted }
                    if grant_id == envelope.invite.id =>
                {
                    Ok(inserted)
                }
                _ => Err(NamedShareDeliveryError::Refused),
            }
        };
        tokio::time::timeout(NAMED_SHARE_DELIVERY_TIMEOUT, exchange)
            .await
            .map_err(|_| NamedShareDeliveryError::Timeout)?
    }
}

/// Each delivery request and response: a big-endian `u32` length, then the
/// canonical encoding. Empty frames are invalid.
const NAMED_SHARE_DELIVERY_FRAMING: Framing =
    Framing::big_endian(MAX_NAMED_SHARE_ENVELOPE_BYTES).reject_empty();

impl From<CanonicalFrameError> for NamedShareDeliveryError {
    fn from(error: CanonicalFrameError) -> Self {
        match error {
            CanonicalFrameError::Io(error) => Self::Io(error),
            CanonicalFrameError::Empty | CanonicalFrameError::TooLarge(_) => Self::EnvelopeTooLarge,
            CanonicalFrameError::Encode(_) | CanonicalFrameError::Decode(_) => Self::MalformedWire,
        }
    }
}

async fn write_wire<W: tokio::io::AsyncWrite + Unpin, T: Serialize + Sync>(
    writer: &mut W,
    value: &T,
) -> Result<(), NamedShareDeliveryError> {
    use tokio::io::AsyncWriteExt;
    canonical_frame::write(NAMED_SHARE_DELIVERY_FRAMING, writer, value).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_wire<R: tokio::io::AsyncRead + Unpin, T: for<'de> Deserialize<'de>>(
    reader: &mut R,
) -> Result<T, NamedShareDeliveryError> {
    Ok(canonical_frame::read_required(NAMED_SHARE_DELIVERY_FRAMING, reader).await?)
}

#[derive(Debug, Error)]
pub enum NamedShareDeliveryError {
    #[error("named-share local identity does not match endpoint/inbox")]
    LocalIdentityMismatch,
    #[error("named-share revocation state is poisoned")]
    RevocationStatePoisoned,
    #[error("named-share transport failed: {0}")]
    Transport(String),
    #[error("named-share peer authentication failed: {0}")]
    Authentication(#[from] crate::authenticated_transport::AuthenticatedTransportError),
    #[error("named-share inbox rejected delivery")]
    Refused,
    #[error("named-share delivery reached the wrong endpoint")]
    RemoteEndpointMismatch,
    #[error("named-share delivery endpoint is not certified to the named human")]
    WrongRecipient,
    #[error("named-share delivery wire is malformed")]
    MalformedWire,
    #[error("named-share delivery timed out")]
    Timeout,
    #[error(transparent)]
    Inbox(#[from] NamedShareInboxError),
    #[error("named-share delivery I/O failed")]
    Io(#[from] std::io::Error),
    #[error("named-share delivery exceeds its wire bound")]
    EnvelopeTooLarge,
}

impl NamedShareInbox {
    /// Open or create an owner-only inbox for `local_person`.
    ///
    /// # Errors
    ///
    /// Returns an error when state cannot be read, validated, or persisted.
    pub fn open_at(
        path: impl Into<PathBuf>,
        local_person: Ed25519PublicKey,
        now_ms: u64,
    ) -> Result<Self, NamedShareInboxError> {
        let path = path.into();
        let entries = match std::fs::metadata(&path) {
            Ok(metadata) => {
                validate_owner_only(&metadata)?;
                if metadata.len() > MAX_NAMED_SHARE_INBOX_FILE_BYTES {
                    return Err(NamedShareInboxError::StateTooLarge);
                }
                decode(&std::fs::read(&path)?, local_person, now_ms)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error.into()),
        };
        let inbox = Self {
            path,
            local_person,
            entries: Mutex::new(entries),
        };
        if !inbox.path.exists() {
            inbox.persist(&BTreeMap::new())?;
        }
        Ok(inbox)
    }

    /// Validate and durably accept an envelope addressed to this inbox.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input, collisions, capacity exhaustion, or
    /// persistence failure.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the lock is held across persist so the file and the in-memory map change together"
    )]
    pub fn accept_at(
        &self,
        envelope: NamedShareEnvelopeV1,
        now_ms: u64,
    ) -> Result<bool, NamedShareInboxError> {
        envelope.validate_for_at(self.local_person, now_ms)?;
        let mut current = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = current.get(&envelope.invite.id) {
            return if existing == &envelope {
                Ok(false)
            } else {
                Err(NamedShareInboxError::GrantCollision)
            };
        }
        if current.len() >= MAX_NAMED_SHARE_INBOX_ENTRIES {
            return Err(NamedShareInboxError::CapacityReached);
        }
        let mut next = current.clone();
        next.insert(envelope.invite.id, envelope);
        self.persist(&next)?;
        *current = next;
        Ok(true)
    }

    /// Return envelopes that remain valid at `now_ms`.
    pub fn entries_at(&self, now_ms: u64) -> Vec<NamedShareEnvelopeV1> {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries
            .values()
            .filter(|entry| entry.validate_for_at(self.local_person, now_ms).is_ok())
            .cloned()
            .collect()
    }

    fn persist(
        &self,
        entries: &BTreeMap<Uuid, NamedShareEnvelopeV1>,
    ) -> Result<(), NamedShareInboxError> {
        let bytes = serde_json::to_vec(&InboxDocumentV1 {
            schema_version: NAMED_SHARE_INBOX_VERSION,
            entries: entries.values().cloned().collect(),
        })
        .map_err(|_| NamedShareInboxError::MalformedState)?;
        if bytes.len() as u64 > MAX_NAMED_SHARE_INBOX_FILE_BYTES {
            return Err(NamedShareInboxError::StateTooLarge);
        }
        if let Some(parent) = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        atomic_file::write_with_mode(&self.path, &bytes, 0o600)?;
        Ok(())
    }
}

fn decode(
    bytes: &[u8],
    person: Ed25519PublicKey,
    now_ms: u64,
) -> Result<BTreeMap<Uuid, NamedShareEnvelopeV1>, NamedShareInboxError> {
    let document: InboxDocumentV1 =
        serde_json::from_slice(bytes).map_err(|_| NamedShareInboxError::MalformedState)?;
    if document.schema_version != NAMED_SHARE_INBOX_VERSION {
        return Err(NamedShareInboxError::UnsupportedVersion);
    }
    if document.entries.len() > MAX_NAMED_SHARE_INBOX_ENTRIES {
        return Err(NamedShareInboxError::CapacityReached);
    }
    let mut entries = BTreeMap::new();
    for entry in document.entries {
        // Expired entries are omitted on restart; they never become discoverable again.
        if entry
            .invite
            .expires_at_ms
            .is_some_and(|deadline| now_ms >= deadline)
        {
            continue;
        }
        entry.validate_for_at(person, now_ms)?;
        if entries.insert(entry.invite.id, entry).is_some() {
            return Err(NamedShareInboxError::GrantCollision);
        }
    }
    Ok(entries)
}

#[cfg(unix)]
fn validate_owner_only(metadata: &std::fs::Metadata) -> Result<(), NamedShareInboxError> {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o777 != 0o600 {
        return Err(NamedShareInboxError::InsecurePermissions);
    }
    Ok(())
}
#[cfg(not(unix))]
fn validate_owner_only(_: &std::fs::Metadata) -> Result<(), NamedShareInboxError> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum NamedShareInboxError {
    #[error("named-share inbox I/O failed")]
    Io(#[from] std::io::Error),
    #[error("named-share envelope is invalid or addressed to another human")]
    InvalidEnvelope,
    #[error("named-share endpoint binding is invalid")]
    BadEndpointBinding(VerifyError),
    #[error("named-share envelope exceeds its wire bound")]
    EnvelopeTooLarge,
    #[error("named-share inbox state is malformed")]
    MalformedState,
    #[error("named-share inbox state version is unsupported")]
    UnsupportedVersion,
    #[error("named-share inbox state exceeds its bound")]
    StateTooLarge,
    #[error("named-share inbox capacity is exhausted")]
    CapacityReached,
    #[error("named-share inbox file is not owner-only mode 0600")]
    InsecurePermissions,
    #[error("named-share grant id collides with different content")]
    GrantCollision,
    #[error("named-share recipient has no certified delivery device")]
    NoRecipientDevices,
    #[error("named-share recipient device list is invalid")]
    InvalidRecipientDevices,
    #[error("named-share outbox grant does not exist")]
    UnknownGrant,
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{FileInviteSpec, FilePermission, mint_file_invite};
    use iroh::address_lookup::memory::MemoryLookup;
    use iroh::endpoint::presets;
    use iroh::protocol::Router;
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, IdentityPaths, adopt_device_key,
        persist::write_device_cert, sign_device_cert,
    };

    const NOW: u64 = 10_000;

    #[tokio::test]
    async fn delivery_frames_are_big_endian_length_then_canonical_bytes() {
        let mut wire = Vec::new();
        write_wire(&mut wire, &DeliveryResponseV1::Refused)
            .await
            .unwrap();
        assert_eq!(wire, [0, 0, 0, 1, 1]);

        let mut wire: Vec<u8> = vec![0, 0, 0, 19, 0, 16];
        wire.extend_from_slice(&[0x22; 16]);
        wire.push(1);
        let response: DeliveryResponseV1 = read_wire(&mut wire.as_slice()).await.unwrap();
        assert_eq!(
            response,
            DeliveryResponseV1::Accepted {
                grant_id: Uuid::from_bytes([0x22; 16]),
                inserted: true,
            }
        );
    }

    #[tokio::test]
    async fn delivery_frames_reject_empty_oversized_and_truncated_wire() {
        let limit = u32::try_from(MAX_NAMED_SHARE_ENVELOPE_BYTES).unwrap();
        for prefix in [0, limit + 1] {
            let wire = prefix.to_be_bytes();
            let result: Result<DeliveryResponseV1, _> = read_wire(&mut &wire[..]).await;
            assert!(matches!(
                result,
                Err(NamedShareDeliveryError::EnvelopeTooLarge)
            ));
        }
        for wire in [&[][..], &[0, 0][..], &[0, 0, 0, 2, 0][..]] {
            let result: Result<DeliveryResponseV1, _> = read_wire(&mut &wire[..]).await;
            assert!(
                matches!(result, Err(NamedShareDeliveryError::Io(ref error)) if error.kind() == std::io::ErrorKind::UnexpectedEof),
                "{wire:?}"
            );
        }
        let result: Result<DeliveryResponseV1, _> = read_wire(&mut &[0, 0, 0, 1, 9][..]).await;
        assert!(matches!(
            result,
            Err(NamedShareDeliveryError::MalformedWire)
        ));
        let mut wire = Vec::new();
        let oversized = vec![0_u8; MAX_NAMED_SHARE_ENVELOPE_BYTES];
        assert!(matches!(
            write_wire(&mut wire, &oversized).await,
            Err(NamedShareDeliveryError::EnvelopeTooLarge)
        ));
        assert!(wire.is_empty());
    }

    fn envelope(person: Ed25519PublicKey, id_seed: u128) -> NamedShareEnvelopeV1 {
        let issuer = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let invite = mint_file_invite(
            FileInviteSpec {
                workspace_id: Uuid::from_u128(1),
                record_id: collab_types::FileRecordId(Uuid::from_u128(id_seed)),
                subject: FileInviteSubject::Person(person),
                permission: FilePermission::View,
                expires_at_ms: Some(NOW + 100),
                max_redemptions: None,
            },
            &issuer,
        )
        .unwrap();
        let endpoint = EndpointAddr::new(iroh::SecretKey::from_bytes(&[8; 32]).public())
            .with_ip_addr("127.0.0.1:7788".parse().unwrap());
        NamedShareEnvelopeV1::bind_at(endpoint, invite, &issuer, NOW).unwrap()
    }

    #[test]
    fn named_inbox_and_outbox_are_durable_idempotent_and_subject_bound() {
        // Phase `durable_inbox_is_idempotent_private_and_subject_bound`.
        {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("inbox.json");
            let person = Ed25519PublicKey::from_bytes(
                ed25519_dalek::SigningKey::from_bytes(&[9; 32])
                    .verifying_key()
                    .to_bytes(),
            );
            let entry = envelope(person, 2);
            let inbox = NamedShareInbox::open_at(&path, person, NOW).unwrap();
            assert!(inbox.accept_at(entry.clone(), NOW).unwrap());
            assert!(!inbox.accept_at(entry.clone(), NOW).unwrap());
            drop(inbox);
            let reopened = NamedShareInbox::open_at(&path, person, NOW).unwrap();
            assert_eq!(reopened.entries_at(NOW), vec![entry.clone()]);
            assert!(
                entry
                    .validate_for_at(Ed25519PublicKey::from_bytes([3; 32]), NOW)
                    .is_err()
            );
            assert!(reopened.entries_at(NOW + 100).is_empty());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
        // Phase `bearer_invite_is_never_admitted_to_named_inbox`.
        {
            let person = Ed25519PublicKey::from_bytes(
                ed25519_dalek::SigningKey::from_bytes(&[9; 32])
                    .verifying_key()
                    .to_bytes(),
            );
            let mut entry = envelope(person, 3);
            entry.invite.subject = FileInviteSubject::Bearer {
                secret_hash: [1; 32],
            };
            assert!(entry.validate_for_at(person, NOW).is_err());
        }
        // Phase `outbox_survives_offline_attempt_and_restart_until_acceptance`.
        {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("outbox.json");
            let person = Ed25519PublicKey::from_bytes(
                ed25519_dalek::SigningKey::from_bytes(&[9; 32])
                    .verifying_key()
                    .to_bytes(),
            );
            let device = Ed25519PublicKey::from_bytes(
                ed25519_dalek::SigningKey::from_bytes(&[10; 32])
                    .verifying_key()
                    .to_bytes(),
            );
            let entry = envelope(person, 7);
            let grant_id = entry.invite.id;
            let outbox = NamedShareOutbox::open(&path).unwrap();
            assert!(outbox.enqueue(entry, vec![device]).unwrap());
            outbox.record_attempt(grant_id, None, NOW).unwrap();
            drop(outbox);
            let reopened = NamedShareOutbox::open(&path).unwrap();
            assert_eq!(reopened.pending_at(NOW).len(), 1);
            assert_eq!(reopened.get(grant_id).unwrap().attempts, 1);
            reopened
                .record_attempt(grant_id, Some(device), NOW + 1)
                .unwrap();
            assert!(reopened.pending_at(NOW + 1).is_empty());
            drop(reopened);
            let final_state = NamedShareOutbox::open(path).unwrap().get(grant_id).unwrap();
            assert_eq!(final_state.delivered_to, Some(device));
            assert_eq!(final_state.attempts, 2);
        }
    }

    struct Party {
        endpoint: Endpoint,
        credentials: PeerCredentials,
        root: ed25519_dalek::SigningKey,
    }

    async fn party(seed: u8) -> Party {
        let device = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let root = ed25519_dalek::SigningKey::from_bytes(&[seed.wrapping_add(40); 32]);
        let now = unix_now_ns();
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: format!("named-{seed}"),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: now.saturating_sub(1),
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(temp.path());
        adopt_device_key(&paths, &[seed; 32]).unwrap();
        write_device_cert(&paths.device_cert, &cert).unwrap();
        let certified = crate::identity::load_certified_mount_identity(&paths, now).unwrap();
        let (mount, credentials) = certified.into_parts();
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(mount.secret)
            .relay_mode(iroh::RelayMode::Disabled)
            .address_lookup(MemoryLookup::new())
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        Party {
            endpoint,
            credentials,
            root,
        }
    }

    #[tokio::test]
    async fn authenticated_delivery_is_durable_idempotent_and_human_bound() {
        let owner = party(21).await;
        let recipient = party(22).await;
        let temp = tempfile::tempdir().unwrap();
        let person = Ed25519PublicKey::from_bytes(recipient.credentials.user_public_key());
        let inbox_path = temp.path().join("network-inbox.json");
        let inbox = Arc::new(NamedShareInbox::open_at(&inbox_path, person, unix_now_ms()).unwrap());
        let protocol = NamedShareInboxProtocol::new(
            recipient.endpoint.clone(),
            recipient.credentials.clone(),
            Arc::new(RwLock::new(RevocationList::new())),
            Arc::clone(&inbox),
        )
        .unwrap();
        let router = Router::builder(recipient.endpoint.clone())
            .accept(TILO_FS_NAMED_SHARE_INBOX_ALPN, protocol)
            .spawn();
        let invite = mint_file_invite(
            FileInviteSpec {
                workspace_id: Uuid::new_v4(),
                record_id: collab_types::FileRecordId::new(),
                subject: FileInviteSubject::Person(person),
                permission: FilePermission::View,
                expires_at_ms: Some(unix_now_ms() + 60_000),
                max_redemptions: None,
            },
            &owner.root,
        )
        .unwrap();
        let bound_at = unix_now_ms();
        let envelope =
            NamedShareEnvelopeV1::bind_at(owner.endpoint.addr(), invite, &owner.root, bound_at)
                .unwrap();
        let client = NamedShareDeliveryClient::new(
            owner.endpoint.clone(),
            owner.credentials,
            Arc::new(RwLock::new(RevocationList::new())),
        )
        .unwrap();
        assert!(
            client
                .deliver(recipient.endpoint.addr(), envelope.clone())
                .await
                .unwrap()
        );
        assert!(
            !client
                .deliver(recipient.endpoint.addr(), envelope.clone())
                .await
                .unwrap()
        );
        router.shutdown().await.unwrap();
        drop(inbox);
        // Read back at the bind instant: the invite expires on the wall
        // clock, and a slow delivery must not decide what durability means.
        let reopened = NamedShareInbox::open_at(inbox_path, person, bound_at).unwrap();
        assert_eq!(reopened.entries_at(bound_at), vec![envelope]);
        owner.endpoint.close().await;
    }
}
