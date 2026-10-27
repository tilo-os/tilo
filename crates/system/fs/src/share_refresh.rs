//! How a share recipient keeps the one record it mounts current.
//!
//! `fs mount-share` used to poll its host every 5 s: a fresh QUIC
//! connection and mutual peer handshake for the snapshot, then one more of
//! each for the host's replica receipt and for its own, whether or not
//! anything had changed. [`ShareRefresher`] holds one authenticated
//! [`RecordProtocolStream`] instead and waits on the host with
//! [`RecordProtocolOperationV1::AwaitChange`]. The host answers when the
//! record changes, or once a minute ([`crate::RECORD_AWAIT_CHANGE_LIMIT`])
//! that it has not, and the replica requests go on the same stream.
//!
//! Every answer is authorized by the host as a fresh request would be, so
//! revocation and expiry still stop refreshes: a revoked grant wakes the
//! host's await and is answered `Denied`. The recipient bounds its own
//! await by the credential's expiry and stops once it has passed.
//!
//! A host that holds all the awaits it may answers `InvalidRequest`, and
//! the refresher polls it instead, every [`SHARE_POLL_INTERVAL`].

use std::time::Duration;

use collab_types::Ed25519PublicKey;
use fs_protocol::FileInviteV1;
use thiserror::Error;

use crate::clock::unix_now_ms;
use crate::named_share_delivery::{NamedShareEnvelopeV1, NamedShareInboxError};
use crate::record_protocol::{
    RecordProtocolClient, RecordProtocolClientError, RecordProtocolOperationV1,
    RecordProtocolResponseV1, RecordProtocolSnapshotV1, RecordProtocolStream,
};
use crate::record_subscription::RecordSubscriptionFailureCode;
use crate::replica_ack::ReplicaAckRequestV1;
use crate::share_link::{FileShareLinkError, FileShareLinkV2};

/// How often a host that cannot hold an await is polled, and how long the
/// refresher waits before it tries again after a failure.
pub const SHARE_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// What `fs mount-share` prints each time it opens a stream to its host:
/// once while the host keeps it, once more after each failure.
pub const SHARE_STREAM_CONNECTED_LINE: &str = "shared record: opened a stream to the host";

/// The shortest await bound a credential's expiry gives: an await never
/// ends sooner than this, so a clock near the expiry cannot spin it.
const MIN_EXPIRY_WAIT: Duration = Duration::from_secs(1);

/// What a recipient presents for its record: a copied link, or an
/// invitation delivered to this person.
#[derive(Clone)]
pub enum SharedCredential {
    Bearer(FileShareLinkV2),
    Named(NamedShareEnvelopeV1),
}

#[derive(Debug, Error)]
pub enum SharedCredentialError {
    #[error(transparent)]
    Link(#[from] FileShareLinkError),
    #[error(transparent)]
    Named(#[from] NamedShareInboxError),
}

impl SharedCredential {
    #[must_use]
    pub fn invite(&self) -> &FileInviteV1 {
        match self {
            Self::Bearer(link) => &link.invite,
            Self::Named(envelope) => &envelope.invite,
        }
    }

    #[must_use]
    pub fn endpoint(&self) -> &iroh_base::EndpointAddr {
        match self {
            Self::Bearer(link) => &link.endpoint,
            Self::Named(envelope) => &envelope.endpoint,
        }
    }

    /// Whether `person` may still present this credential at `now_ms`.
    ///
    /// # Errors
    ///
    /// Returns why not: expired, malformed, or not this person's.
    pub fn validate_at(
        &self,
        person: Ed25519PublicKey,
        now_ms: u64,
    ) -> Result<(), SharedCredentialError> {
        match self {
            Self::Bearer(link) => link.validate_at(now_ms).map_err(Into::into),
            Self::Named(envelope) => envelope.validate_for_at(person, now_ms).map_err(Into::into),
        }
    }

    async fn request_on(
        &self,
        stream: &mut RecordProtocolStream,
        operation: RecordProtocolOperationV1,
    ) -> Result<RecordProtocolResponseV1, RecordProtocolClientError> {
        match self {
            Self::Bearer(link) => stream.request_bearer_link(link, operation).await,
            Self::Named(envelope) => {
                stream
                    .request_named(envelope.invite.clone(), operation)
                    .await
            }
        }
    }
}

/// Send one request on a connection of its own.
pub async fn request_shared(
    client: &RecordProtocolClient,
    credential: &SharedCredential,
    operation: RecordProtocolOperationV1,
) -> Result<RecordProtocolResponseV1, RecordProtocolClientError> {
    match credential {
        SharedCredential::Bearer(link) => client.request_bearer_link(link, operation).await,
        SharedCredential::Named(envelope) => {
            client
                .request_named(
                    envelope.endpoint.clone(),
                    envelope.invite.clone(),
                    operation,
                )
                .await
        }
    }
}

/// Read the record's snapshot on a connection of its own.
pub async fn fetch_shared_snapshot(
    client: &RecordProtocolClient,
    credential: &SharedCredential,
) -> Result<RecordProtocolSnapshotV1, SharedFetchError> {
    let response = request_shared(client, credential, RecordProtocolOperationV1::Read).await?;
    match snapshot_answer(credential, response)? {
        Answer::Snapshot(snapshot) => Ok(*snapshot),
        Answer::Unchanged | Answer::Unsupported => Err(SharedFetchError::InvalidResponse),
    }
}

#[derive(Debug, Error)]
pub enum SharedFetchError {
    #[error("shared-record transport or authentication failed")]
    Client(#[from] RecordProtocolClientError),
    #[error("shared-record access was denied or revoked")]
    Denied,
    #[error("shared record no longer exists")]
    NotFound,
    #[error("shared-record host returned an invalid response")]
    InvalidResponse,
    #[error("shared-record host answered for another workspace or record than the invite names")]
    OtherSession,
    #[error("shared-record host failed internally")]
    Internal,
    #[error("the shared-record credential has expired or is no longer valid")]
    Expired,
}

impl SharedFetchError {
    /// The secret-free code a subscription records for this failure.
    #[must_use]
    pub fn failure_code(&self) -> RecordSubscriptionFailureCode {
        match self {
            Self::Client(
                RecordProtocolClientError::Transport(_)
                | RecordProtocolClientError::Timeout
                | RecordProtocolClientError::ReplicaUnavailable,
            ) => RecordSubscriptionFailureCode::Offline,
            Self::Client(
                RecordProtocolClientError::Authentication(_)
                | RecordProtocolClientError::LocalEndpointMismatch
                | RecordProtocolClientError::ServerEndpointMismatch
                | RecordProtocolClientError::RevocationStatePoisoned
                | RecordProtocolClientError::ReplicaVerification(_),
            ) => RecordSubscriptionFailureCode::Authentication,
            Self::Denied
            | Self::Expired
            | Self::Client(
                RecordProtocolClientError::Link(_) | RecordProtocolClientError::ReplicaDenied,
            ) => RecordSubscriptionFailureCode::Denied,
            Self::NotFound => RecordSubscriptionFailureCode::NotFound,
            Self::InvalidResponse
            | Self::OtherSession
            | Self::Client(
                RecordProtocolClientError::RequestTooLarge
                | RecordProtocolClientError::InvalidRequest
                | RecordProtocolClientError::ResponseTooLarge
                | RecordProtocolClientError::InvalidResponse,
            ) => RecordSubscriptionFailureCode::InvalidResponse,
            Self::Internal => RecordSubscriptionFailureCode::Internal,
        }
    }
}

enum Answer {
    Snapshot(Box<RecordProtocolSnapshotV1>),
    Unchanged,
    /// The host does not know the operation asked for.
    Unsupported,
}

/// A snapshot request's answer, accepted only from a session on the
/// invite's workspace and record.
fn snapshot_answer(
    credential: &SharedCredential,
    response: RecordProtocolResponseV1,
) -> Result<Answer, SharedFetchError> {
    match response {
        RecordProtocolResponseV1::Snapshot { session, snapshot } => {
            if session.is_for_invite(credential.invite()) {
                Ok(Answer::Snapshot(Box::new(snapshot)))
            } else {
                Err(SharedFetchError::OtherSession)
            }
        }
        RecordProtocolResponseV1::Unchanged { session } => {
            if session.is_for_invite(credential.invite()) {
                Ok(Answer::Unchanged)
            } else {
                Err(SharedFetchError::OtherSession)
            }
        }
        RecordProtocolResponseV1::InvalidRequest => Ok(Answer::Unsupported),
        RecordProtocolResponseV1::Denied => Err(SharedFetchError::Denied),
        RecordProtocolResponseV1::NotFound => Err(SharedFetchError::NotFound),
        RecordProtocolResponseV1::InternalError => Err(SharedFetchError::Internal),
        _ => Err(SharedFetchError::InvalidResponse),
    }
}

/// `interval` from now; `None` only past the clock's range, where the
/// refresher does not wait at all.
fn after(interval: Duration) -> Option<tokio::time::Instant> {
    tokio::time::Instant::now().checked_add(interval)
}

/// What one refresh found.
#[derive(Debug)]
pub enum ShareRefresh {
    /// The record differs from what the recipient held: this is it now.
    Changed(Box<RecordProtocolSnapshotV1>),
    /// The host confirmed the record is as the recipient holds it.
    Unchanged,
}

/// How the refresher learns of changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareRefreshMode {
    /// The host holds an await on one long-lived stream.
    Await,
    /// The host declined the await: poll with `Read`.
    Poll,
}

/// Keeps one shared record current; see the module documentation.
pub struct ShareRefresher {
    client: RecordProtocolClient,
    credential: SharedCredential,
    person: Ed25519PublicKey,
    /// Digest of the snapshot the recipient holds.
    known: Option<[u8; 32]>,
    stream: Option<RecordProtocolStream>,
    mode: ShareRefreshMode,
    poll_interval: Duration,
    /// No request before this: the next poll, or the retry after a failure.
    not_before: Option<tokio::time::Instant>,
    /// Streams opened so far.
    connections: u64,
}

impl std::fmt::Debug for ShareRefresher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShareRefresher")
            .field("mode", &self.mode)
            .field("connections", &self.connections)
            .finish_non_exhaustive()
    }
}

impl ShareRefresher {
    /// A refresher for a recipient that holds `held`, presenting
    /// `credential` as `person`.
    #[must_use]
    pub fn new(
        client: RecordProtocolClient,
        credential: SharedCredential,
        person: Ed25519PublicKey,
        held: &RecordProtocolSnapshotV1,
    ) -> Self {
        Self {
            client,
            credential,
            person,
            known: held.digest(),
            stream: None,
            mode: ShareRefreshMode::Await,
            poll_interval: SHARE_POLL_INTERVAL,
            not_before: None,
            connections: 0,
        }
    }

    /// Poll, and retry after a failure, every `interval` instead of
    /// [`SHARE_POLL_INTERVAL`], so a test sees the fallback in milliseconds.
    #[must_use]
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    #[must_use]
    pub fn mode(&self) -> ShareRefreshMode {
        self.mode
    }

    /// Authenticated streams opened so far: one for as long as the host
    /// keeps the stream, one more after each failure.
    #[must_use]
    pub fn connections(&self) -> u64 {
        self.connections
    }

    /// Wait for the record to change, or for the host to confirm it has
    /// not; in poll mode, wait out the interval and read it.
    ///
    /// # Errors
    ///
    /// [`SharedFetchError::Expired`] once the credential is no longer
    /// valid: the caller stops. Anything else is the failure of this
    /// attempt; the next call retries after the poll interval.
    pub async fn next(&mut self) -> Result<ShareRefresh, SharedFetchError> {
        if let Some(at) = self.not_before.take() {
            tokio::time::sleep_until(at).await;
        }
        loop {
            if self
                .credential
                .validate_at(self.person, unix_now_ms())
                .is_err()
            {
                self.stream = None;
                return Err(SharedFetchError::Expired);
            }
            let result = match self.mode {
                ShareRefreshMode::Await => self.await_change().await,
                ShareRefreshMode::Poll => self.poll().await,
            };
            let refresh = match result {
                Ok(Answer::Snapshot(snapshot)) => {
                    if self.known.is_some() && snapshot.digest() == self.known {
                        ShareRefresh::Unchanged
                    } else {
                        self.known = snapshot.digest();
                        ShareRefresh::Changed(snapshot)
                    }
                }
                Ok(Answer::Unchanged) => ShareRefresh::Unchanged,
                Ok(Answer::Unsupported) => {
                    // A host holding all the awaits it may: poll it,
                    // starting one interval from now.
                    self.stream = None;
                    self.mode = ShareRefreshMode::Poll;
                    tokio::time::sleep(self.poll_interval).await;
                    continue;
                }
                Err(SharedFetchError::Expired) => {
                    self.stream = None;
                    continue;
                }
                Err(error) => {
                    self.stream = None;
                    self.not_before = after(self.poll_interval);
                    return Err(error);
                }
            };
            if self.mode == ShareRefreshMode::Poll {
                self.not_before = after(self.poll_interval);
            }
            return Ok(refresh);
        }
    }

    /// Send `operation` on the held stream, or on a connection of its own
    /// when no stream is held (a polled host, or after a failure).
    ///
    /// # Errors
    ///
    /// Returns an error on credential, transport, authentication or
    /// protocol failure. A failed stream is dropped; the next refresh
    /// opens another.
    pub async fn request(
        &mut self,
        operation: RecordProtocolOperationV1,
    ) -> Result<RecordProtocolResponseV1, RecordProtocolClientError> {
        let Some(stream) = self.stream.as_mut().filter(|stream| stream.is_held()) else {
            return request_shared(&self.client, &self.credential, operation).await;
        };
        let result = self.credential.request_on(stream, operation).await;
        if result.is_err() {
            self.stream = None;
        }
        result
    }

    /// Ask the host to prove it durably holds `request`'s revision, on the
    /// held stream when there is one. `None` for a named invitation, which
    /// has no such exchange.
    pub async fn host_replica_observation(
        &mut self,
        request: ReplicaAckRequestV1,
    ) -> Result<Option<crate::availability::ReplicaObservation>, RecordProtocolClientError> {
        let SharedCredential::Bearer(link) = &self.credential else {
            return Ok(None);
        };
        let Some(stream) = self.stream.as_mut().filter(|stream| stream.is_held()) else {
            return self
                .client
                .request_bearer_link_replica_ack(link, request)
                .await
                .map(Some);
        };
        let result = stream.request_bearer_link_replica_ack(link, request).await;
        if matches!(
            result,
            Err(RecordProtocolClientError::Transport(_) | RecordProtocolClientError::Timeout)
        ) {
            self.stream = None;
        }
        result.map(Some)
    }

    /// One change await on the held stream, opening it first if needed.
    /// The await ends by the credential's expiry at the latest.
    async fn await_change(&mut self) -> Result<Answer, SharedFetchError> {
        let held = self.stream.take().filter(RecordProtocolStream::is_open);
        let stream = if let Some(stream) = held {
            stream
        } else {
            let stream = self
                .client
                .open_stream(self.credential.endpoint().clone())
                .await?;
            self.connections = self.connections.saturating_add(1);
            eprintln!("{SHARE_STREAM_CONNECTED_LINE}");
            stream
        };
        let stream = self.stream.insert(stream);
        let operation = RecordProtocolOperationV1::AwaitChange {
            known: self.known.unwrap_or_default(),
        };
        let exchange = self.credential.request_on(stream, operation);
        let response = match self.credential.invite().expires_at_ms {
            Some(expires_at_ms) => {
                let left = Duration::from_millis(expires_at_ms.saturating_sub(unix_now_ms()))
                    .max(MIN_EXPIRY_WAIT);
                tokio::time::timeout(left, exchange)
                    .await
                    .map_err(|_| SharedFetchError::Expired)??
            }
            None => exchange.await?,
        };
        snapshot_answer(&self.credential, response)
    }

    async fn poll(&self) -> Result<Answer, SharedFetchError> {
        let response = request_shared(
            &self.client,
            &self.credential,
            RecordProtocolOperationV1::Read,
        )
        .await?;
        match snapshot_answer(&self.credential, response)? {
            Answer::Snapshot(snapshot) => Ok(Answer::Snapshot(snapshot)),
            Answer::Unchanged | Answer::Unsupported => Err(SharedFetchError::InvalidResponse),
        }
    }
}
