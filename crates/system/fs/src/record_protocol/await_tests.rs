//! Change awaits: a share recipient holds one authenticated
//! stream, the host answers it when the record changes, a revocation ends
//! it, and a host holding all the awaits it may is polled.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, Wake, Waker};
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

/// Counts the wakes of a request future the test polls by hand.
#[derive(Default)]
struct Wakes(AtomicU64);

impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

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

    /// The recipient as the host authenticates it.
    fn peer(&self) -> crate::peer_auth::AuthenticatedFsPeer {
        let cert = self.recipient.credentials.cert().clone();
        crate::peer_auth::AuthenticatedFsPeer {
            user_public_key: cert.user_public_key,
            device_public_key: cert.device_public_key,
            device_label: cert.device_label.clone(),
            capabilities: cert.capabilities.clone(),
            cert,
        }
    }

    /// The recipient's await for the record as it is now, handed to the
    /// host's request handler and polled once: it must be held, not answered.
    async fn held_await<'a>(
        &self,
        protocol: &'a RecordProtocol,
    ) -> (
        Pin<Box<impl Future<Output = RecordProtocolResponseV1> + 'a>>,
        Arc<Wakes>,
    ) {
        let request = |operation| RecordProtocolRequestV1 {
            protocol_version: RECORD_PROTOCOL_VERSION,
            credential: RecordProtocolCredentialV1::BearerLinkV2(self.link.encode_uri().unwrap()),
            operation,
        };
        let RecordProtocolResponseV1::Snapshot { snapshot, .. } = protocol
            .handle_request(self.peer(), request(RecordProtocolOperationV1::Read))
            .await
        else {
            panic!("the live grant must serve the record");
        };
        let mut pending = Box::pin(protocol.handle_request(
            self.peer(),
            request(RecordProtocolOperationV1::AwaitChange {
                known: snapshot.digest().unwrap(),
            }),
        ));
        let woken = Arc::new(Wakes::default());
        let waker = Waker::from(Arc::clone(&woken));
        assert!(
            pending
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        assert_eq!(
            protocol.awaits.available_permits(),
            MAX_RECORD_CHANGE_AWAITS - 1
        );
        (pending, woken)
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
    tilo_test_env::within("the recipient's await to reach the host", async {
        while protocol.awaits.available_permits() == MAX_RECORD_CHANGE_AWAITS {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
}

/// A host write wakes the held await itself, which answers with the record
/// on its next poll: the minute-long limit plays no part.
#[tokio::test]
async fn a_host_write_wakes_the_held_await() {
    let mut fixture = Fixture::new().await;
    let protocol = fixture.protocol(RECORD_AWAIT_CHANGE_LIMIT);
    let (mut pending, woken) = fixture.held_await(&protocol).await;
    fixture.write(2);
    assert!(woken.0.load(Ordering::SeqCst) > 0, "the write must wake it");
    let waker = Waker::from(woken);
    let Poll::Ready(RecordProtocolResponseV1::Snapshot { snapshot, .. }) =
        pending.as_mut().poll(&mut Context::from_waker(&waker))
    else {
        panic!("the woken await must answer with the record");
    };
    assert_eq!(snapshot.metadata.unwrap().observed_at_ms, 2);
    assert_eq!(
        protocol.awaits.available_permits(),
        MAX_RECORD_CHANGE_AWAITS
    );
}

/// With a minute-long limit only a write can answer the held await, and
/// the answer reaches the recipient on its one held stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_write_answers_a_held_await() {
    let mut fixture = Fixture::new().await;
    let protocol = fixture.protocol(RECORD_AWAIT_CHANGE_LIMIT);
    let (_router, accepted) = fixture.serve(protocol.clone());
    let mut refresher = fixture.refresher().await;
    let waiting = tokio::spawn(async move {
        let refresh = refresher.next().await;
        (refresh, refresher)
    });
    until_held(&protocol).await;
    fixture.write(2);
    let (refresh, refresher) = waiting.await.unwrap();
    match refresh.unwrap() {
        ShareRefresh::Changed(snapshot) => {
            assert_eq!(snapshot.metadata.unwrap().observed_at_ms, 2);
        }
        ShareRefresh::Unchanged => panic!("the write must answer the await with the record"),
    }
    assert_eq!(refresher.connections(), 1);
    // The refresh's own snapshot read, then the one stream.
    assert_eq!(accepted.load(Ordering::SeqCst), 2);
}

/// Revoking the grant wakes the held await itself, which answers `Denied`
/// on its next poll. Without the wake the limit would also end in `Denied`,
/// a minute later, so only the wake tells the two apart.
#[tokio::test]
async fn a_revoked_grant_wakes_the_held_await() {
    let fixture = Fixture::new().await;
    let protocol = fixture.protocol(RECORD_AWAIT_CHANGE_LIMIT);
    let (mut pending, woken) = fixture.held_await(&protocol).await;
    assert!(fixture.authority.revoke(fixture.link.invite.id).unwrap());
    assert!(
        woken.0.load(Ordering::SeqCst) > 0,
        "the revocation must wake it"
    );
    let waker = Waker::from(woken);
    assert!(matches!(
        pending.as_mut().poll(&mut Context::from_waker(&waker)),
        Poll::Ready(RecordProtocolResponseV1::Denied)
    ));
    assert_eq!(
        protocol.awaits.available_permits(),
        MAX_RECORD_CHANGE_AWAITS
    );
}

/// A revocation reaches the recipient's held stream as `Denied`, and no
/// request after it gets the record.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoked_grant_ends_a_held_await() {
    let fixture = Fixture::new().await;
    let protocol = fixture.protocol(RECORD_AWAIT_CHANGE_LIMIT);
    let (_router, _accepted) = fixture.serve(protocol.clone());
    let mut refresher = fixture.refresher().await;
    let waiting = tokio::spawn(async move { refresher.next().await });
    until_held(&protocol).await;
    assert!(fixture.authority.revoke(fixture.link.invite.id).unwrap());
    let refresh = waiting.await.unwrap();
    assert!(
        matches!(refresh, Err(SharedFetchError::Denied)),
        "{refresh:?}"
    );
    assert!(matches!(
        fetch_shared_snapshot(&fixture.client(), &fixture.credential()).await,
        Err(SharedFetchError::Denied)
    ));
}

/// A host holding all the awaits it may answers another `InvalidRequest`;
/// the recipient then polls it with `Read` and still sees its writes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_at_its_await_limit_is_polled() {
    let mut fixture = Fixture::new().await;
    let protocol = fixture.protocol(RECORD_AWAIT_CHANGE_LIMIT);
    let _every_await = Arc::clone(&protocol.awaits)
        .acquire_many_owned(u32::try_from(MAX_RECORD_CHANGE_AWAITS).unwrap())
        .await
        .unwrap();
    let (_router, accepted) = fixture.serve(protocol);
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
