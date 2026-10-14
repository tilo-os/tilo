//! Change awaits: a share recipient holds one authenticated
//! stream, the host answers it when the record changes, a revocation ends
//! it at once, and a host that predates the operation is polled.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use collab_types::Ed25519PublicKey;
use ed25519_dalek::SigningKey;
use fs_protocol::{
    FileActor, FileInviteSpec, FileInviteSubject, FilePermission, IndexStatus,
    mint_bearer_file_invite,
};
use iroh::protocol::Router;
use rand::{SeedableRng, rngs::StdRng};

use super::tests::{AlwaysCurrent, Party, party};
use super::*;
use crate::record_replication::{RecordOperationV1, test_authorizers::AllowAll};
use crate::share_refresh::{
    ShareRefresh, ShareRefreshMode, ShareRefresher, SharedCredential, SharedFetchError,
    fetch_shared_snapshot,
};

/// How soon an await must answer a host write: the poll it replaces
/// answered within 5 s.
const WRITE_BOUND: Duration = Duration::from_millis(100);

/// How soon a revoked grant must end a held await: the bound the idle serve gate keeps
/// for a revoked mount.
const REVOKE_BOUND: Duration = Duration::from_secs(1);

/// Counts the connections a host accepts.
#[derive(Clone, Debug)]
struct Counted<H> {
    inner: H,
    accepted: Arc<AtomicU64>,
}

impl<H: ProtocolHandler + Clone> ProtocolHandler for Counted<H> {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.accepted.fetch_add(1, Ordering::SeqCst);
        self.inner.accept(connection).await
    }
}

/// The operations of protocol version 6 as they shipped before
/// `AwaitChange`: what a host that predates it decodes. Boxing does not
/// change the canonical encoding.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum ShippedOperationV1 {
    Read,
    Write(Box<RecordEnvelopeV1>),
    AcknowledgeReplica(Box<ReplicaAckV1>),
    RequestReplicaAcknowledgement(Box<ReplicaAckRequestV1>),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShippedRequestV1 {
    protocol_version: u16,
    credential: RecordProtocolCredentialV1,
    operation: ShippedOperationV1,
}

/// A host that predates `AwaitChange`: one request per connection, decoded
/// as the shipped operations, and `InvalidRequest` for anything else.
#[derive(Clone, Debug)]
struct ShippedHost {
    inner: RecordProtocol,
}

impl ProtocolHandler for ShippedHost {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        let peer = self
            .inner
            .authenticate(&connection, &mut recv, &mut send)
            .await
            .map_err(|error| AcceptError::from_err(io::Error::other(error.to_string())))?;
        let shipped: Result<ShippedRequestV1, _> =
            canonical_frame::read_required(RECORD_PROTOCOL_FRAMING, &mut recv).await;
        let response = match shipped {
            Ok(shipped) => {
                let request =
                    collab_types::canonical_bytes(&shipped).expect("a decoded request encodes");
                let request = collab_types::canonical::from_canonical_bytes(&request)
                    .expect("every shipped operation is a current one");
                self.inner.handle_request(peer, request).await
            }
            Err(_) => RecordProtocolResponseV1::InvalidRequest,
        };
        write_response(&mut send, &response)
            .await
            .map_err(|error| AcceptError::from_err(io::Error::other(error.to_string())))?;
        let _ = send.finish();
        connection.closed().await;
        Ok(())
    }
}

/// A host serving one record, and a recipient holding a view link to it.
struct Fixture {
    _temp: tempfile::TempDir,
    server: Party,
    recipient: Party,
    key: RecordRepositoryKey,
    authority: Arc<ShareAuthority>,
    repository: Arc<RecordRepository>,
    registry: Arc<RecordAccessRegistry>,
    link: FileShareLinkV2,
    writer: SigningKey,
    sequence: u64,
}

impl Fixture {
    async fn new() -> Self {
        let server = party().await;
        let recipient = party().await;
        let key = RecordRepositoryKey {
            workspace_id: Uuid::new_v4(),
            record_id: FileRecordId::new(),
        };
        let issuer = SigningKey::from_bytes(&[79; 32]);
        let issuer_public = Ed25519PublicKey::from_bytes(issuer.verifying_key().to_bytes());
        let temp = tempfile::tempdir().unwrap();
        let authority = Arc::new(ShareAuthority::open(temp.path().join("shares.json")).unwrap());
        let repository = Arc::new(RecordRepository::open(temp.path().join("records")).unwrap());
        let registry = Arc::new(RecordAccessRegistry::new(64, 60_000).unwrap());
        let bearer = mint_bearer_file_invite(
            FileInviteSpec {
                workspace_id: key.workspace_id,
                record_id: key.record_id,
                subject: FileInviteSubject::Bearer {
                    secret_hash: [0; 32],
                },
                permission: FilePermission::View,
                expires_at_ms: None,
                max_redemptions: None,
            },
            &issuer,
            &mut StdRng::seed_from_u64(779),
        )
        .unwrap();
        authority
            .register(&bearer.token, &[issuer_public], 1)
            .unwrap();
        let link = FileShareLinkV2::bind_at(server.endpoint.addr(), bearer, &issuer, 1).unwrap();
        let mut fixture = Self {
            _temp: temp,
            server,
            recipient,
            key,
            authority,
            repository,
            registry,
            link,
            writer: SigningKey::from_bytes(&[80; 32]),
            sequence: 0,
        };
        fixture.write(1);
        fixture
    }

    /// The host's protocol handler, holding awaits for `limit`.
    fn protocol(&self, limit: Duration) -> RecordProtocol {
        RecordProtocol::new(
            self.server.endpoint.clone(),
            self.server.credentials.clone(),
            Arc::new(RwLock::new(RevocationList::new())),
            self.key.workspace_id,
            Arc::clone(&self.authority),
            Arc::new(AlwaysCurrent),
            Arc::clone(&self.registry),
            Arc::clone(&self.repository),
        )
        .unwrap()
        .with_await_limit(limit)
    }

    fn serve<H: ProtocolHandler + Clone>(&self, handler: H) -> (Router, Arc<AtomicU64>) {
        let accepted = Arc::new(AtomicU64::new(0));
        let router = Router::builder(self.server.endpoint.clone())
            .accept(
                TILO_FS_RECORD_ALPN,
                Counted {
                    inner: handler,
                    accepted: Arc::clone(&accepted),
                },
            )
            .spawn();
        (router, accepted)
    }

    fn client(&self) -> RecordProtocolClient {
        RecordProtocolClient::new(
            self.recipient.endpoint.clone(),
            self.recipient.credentials.clone(),
            Arc::new(RwLock::new(RevocationList::new())),
        )
        .unwrap()
    }

    fn credential(&self) -> SharedCredential {
        SharedCredential::Bearer(self.link.clone())
    }

    /// A recipient that holds the record as it is now.
    async fn refresher(&self) -> ShareRefresher {
        let client = self.client();
        let held = fetch_shared_snapshot(&client, &self.credential())
            .await
            .unwrap();
        ShareRefresher::new(
            client,
            self.credential(),
            Ed25519PublicKey::from_bytes(self.recipient.credentials.user_public_key()),
            &held,
        )
    }

    /// Change the record's metadata, as an edit on the host does.
    fn write(&mut self, observed_at_ms: u64) {
        self.sequence = self.sequence.saturating_add(1);
        let actor = FileActor {
            person: Ed25519PublicKey::from_bytes([81; 32]),
            device: Ed25519PublicKey::from_bytes(self.writer.verifying_key().to_bytes()),
        };
        let metadata = FileMetadataV1 {
            schema_version: fs_protocol::files::FILE_METADATA_VERSION,
            record_id: self.key.record_id,
            content_hash: None,
            byte_len: 12,
            mime: Some("text/plain".into()),
            previews: vec![],
            latest_actor: Some(actor),
            observed_at_ms,
            index_status: IndexStatus::Pending,
            properties: None,
        };
        let envelope = RecordEnvelopeV1::signed(
            self.key.record_id,
            actor,
            self.sequence,
            RecordOperationV1::SetMetadata(metadata),
            &self.writer,
        )
        .unwrap();
        self.repository
            .apply(self.key, &envelope, &AllowAll)
            .unwrap();
    }
}

/// An idle recipient asks again only when the host's await limit passes,
/// on the one stream it opened: no new connection, no new handshake. Its
/// other requests go on the same stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_awaits_share_one_connection() {
    let fixture = Fixture::new().await;
    let (_router, accepted) = fixture.serve(fixture.protocol(Duration::from_millis(200)));
    let mut refresher = fixture.refresher().await;
    let before = accepted.load(Ordering::SeqCst);

    let idle = Instant::now();
    for _ in 0..5 {
        assert!(matches!(
            refresher.next().await.unwrap(),
            ShareRefresh::Unchanged
        ));
    }
    assert!(
        idle.elapsed() >= Duration::from_millis(5 * 200),
        "an unchanged record must be held to the await limit, not answered at once"
    );
    assert_eq!(refresher.mode(), ShareRefreshMode::Await);
    assert_eq!(refresher.connections(), 1);
    assert_eq!(accepted.load(Ordering::SeqCst) - before, 1);

    // The recipient's replica requests and receipts go on the held stream.
    assert!(matches!(
        refresher
            .request(RecordProtocolOperationV1::Read)
            .await
            .unwrap(),
        RecordProtocolResponseV1::Snapshot { .. }
    ));
    assert_eq!(accepted.load(Ordering::SeqCst) - before, 1);
}

/// Wait until the host holds the recipient's await. The recipient opens
/// its stream and authenticates first, which can take longer than any fixed
/// pause on a loaded runner; a change made before the await arrives would
/// time that handshake instead of the wake.
async fn until_held(protocol: &RecordProtocol) {
    let started = Instant::now();
    while protocol.awaits.available_permits() == MAX_RECORD_CHANGE_AWAITS {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the recipient's await never reached the host"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// With a minute-long limit only a write can answer the held await, and
/// it does at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_write_answers_a_held_await_at_once() {
    let mut fixture = Fixture::new().await;
    let protocol = fixture.protocol(RECORD_AWAIT_CHANGE_LIMIT);
    let (_router, accepted) = fixture.serve(protocol.clone());
    let mut refresher = fixture.refresher().await;
    let waiting = tokio::spawn(async move {
        let refresh = refresher.next().await;
        (refresh, Instant::now(), refresher)
    });
    until_held(&protocol).await;
    let wrote = Instant::now();
    fixture.write(2);
    let (refresh, answered, refresher) = waiting.await.unwrap();
    let latency = answered.saturating_duration_since(wrote);
    eprintln!(
        "a host write reached the held await in {} us",
        latency.as_micros()
    );
    match refresh.unwrap() {
        ShareRefresh::Changed(snapshot) => {
            assert_eq!(snapshot.metadata.unwrap().observed_at_ms, 2);
        }
        ShareRefresh::Unchanged => panic!("the write must answer the await with the record"),
    }
    assert!(
        latency <= WRITE_BOUND,
        "the await answered {} ms after the write (bound {} ms)",
        latency.as_millis(),
        WRITE_BOUND.as_millis()
    );
    assert_eq!(refresher.connections(), 1);
    // The refresh's own snapshot read, then the one stream.
    assert_eq!(accepted.load(Ordering::SeqCst), 2);
}

/// Revoking the grant wakes a held await, which is answered `Denied`, and
/// no request after it gets the record.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoked_grant_ends_a_held_await_at_once() {
    let fixture = Fixture::new().await;
    let protocol = fixture.protocol(RECORD_AWAIT_CHANGE_LIMIT);
    let (_router, _accepted) = fixture.serve(protocol.clone());
    let mut refresher = fixture.refresher().await;
    let waiting = tokio::spawn(async move {
        let refresh = refresher.next().await;
        (refresh, Instant::now())
    });
    until_held(&protocol).await;
    let revoked = Instant::now();
    assert!(fixture.authority.revoke(fixture.link.invite.id).unwrap());
    let (refresh, answered) = waiting.await.unwrap();
    let latency = answered.saturating_duration_since(revoked);
    eprintln!(
        "a revocation ended the held await in {} us",
        latency.as_micros()
    );
    assert!(
        matches!(refresh, Err(SharedFetchError::Denied)),
        "{refresh:?}"
    );
    assert!(
        latency <= REVOKE_BOUND,
        "the held await outlived its revocation by {} ms",
        latency.as_millis()
    );
    assert!(matches!(
        fetch_shared_snapshot(&fixture.client(), &fixture.credential()).await,
        Err(SharedFetchError::Denied)
    ));
}

/// A host that predates `AwaitChange` answers it `InvalidRequest`; the
/// recipient then polls it with `Read` and still sees its writes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_without_awaits_is_polled() {
    let mut fixture = Fixture::new().await;
    let await_request = RecordProtocolRequestV1 {
        protocol_version: RECORD_PROTOCOL_VERSION,
        credential: RecordProtocolCredentialV1::Named(fixture.link.invite.clone()),
        operation: RecordProtocolOperationV1::AwaitChange { known: [0; 32] },
    };
    assert!(
        collab_types::canonical::from_canonical_bytes::<ShippedRequestV1>(
            &collab_types::canonical_bytes(&await_request).unwrap()
        )
        .is_err(),
        "a host that predates the await must not decode it"
    );

    let shipped = ShippedHost {
        inner: fixture.protocol(RECORD_AWAIT_CHANGE_LIMIT),
    };
    let (_router, accepted) = fixture.serve(shipped);
    let poll = Duration::from_millis(50);
    let mut refresher = fixture.refresher().await.with_poll_interval(poll);
    assert!(matches!(
        refresher.next().await.unwrap(),
        ShareRefresh::Unchanged
    ));
    let previous_poll = Instant::now();
    assert_eq!(refresher.mode(), ShareRefreshMode::Poll);
    assert_eq!(refresher.connections(), 1);
    // The snapshot read, the declined await, then one read per poll.
    assert_eq!(accepted.load(Ordering::SeqCst), 3);

    fixture.write(3);
    match refresher.next().await.unwrap() {
        ShareRefresh::Changed(snapshot) => {
            assert_eq!(snapshot.metadata.unwrap().observed_at_ms, 3);
        }
        ShareRefresh::Unchanged => panic!("a poll must see the write"),
    }
    assert!(previous_poll.elapsed() >= poll, "polls keep their interval");
    assert_eq!(refresher.mode(), ShareRefreshMode::Poll);
    assert_eq!(refresher.connections(), 1);
}
