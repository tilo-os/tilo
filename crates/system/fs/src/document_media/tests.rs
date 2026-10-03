use std::time::Duration;

use collab_types::{CapOverride, Subject, Verb};
use iroh::protocol::Router;
use runtime_fs::blobs::{BlobClient, BlobPrincipal, write_handoff};

use super::*;
use crate::acl::{AclHandle, WorkspaceAcl};
use crate::hydration::Hydrator;
use crate::sync::{FsTransportIdentity, bind_certified_test_endpoint};
use iroh::Endpoint;

fn scope(room: &str, app: &str) -> BlobScope {
    BlobScope::new(room, app).unwrap()
}

/// Poll a read until it settles; a peer fetch answers `Pending` first.
async fn settle(media: &Arc<DocumentMedia>, scope: &BlobScope, hash: Hash) -> ReadOutcome {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let outcome = media.read_range(scope.clone(), hash, 0, u32::MAX).await;
        if outcome != ReadOutcome::Pending || Instant::now() > deadline {
            return outcome;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn stored_media_is_readable_only_by_reference_bounded_and_durable() {
    // Phase `bytes_in_the_store_are_not_readable_without_a_reference`.
    {
        let store = Arc::new(ContentStore::memory());
        let media = DocumentMedia::new(Arc::clone(&store), None);
        let photos = scope("room-1", "so.tilo.photos");

        let hash = media.store(&photos, b"an image".to_vec()).await.unwrap();
        assert_eq!(
            media.read_range(photos.clone(), hash, 3, 5).await,
            ReadOutcome::Ready {
                bytes: b"image".to_vec(),
                total_len: 8,
            }
        );
        assert!(media.contains(&photos, hash).await);
        assert_eq!(media.len_of(&photos, hash).await, Some(8));

        // Another app in the room may be handed the content by hash.
        assert!(media.contains(&scope("room-1", "so.tilo.chat"), hash).await);

        // Another room: the bytes are right there, and still unreadable, because
        // nothing that room reads references them.
        let other = scope("room-2", "so.tilo.photos");
        assert_eq!(
            media.read_range(other.clone(), hash, 0, u32::MAX).await,
            ReadOutcome::Unavailable
        );
        assert!(!media.contains(&other, hash).await);
        assert_eq!(media.len_of(&other, hash).await, None);

        // Content that reached the store as a file is not document media.
        let file_bytes = store.ingest(Bytes::from_static(b"a file")).await.unwrap();
        assert_eq!(
            media
                .read_range(photos.clone(), file_bytes, 0, u32::MAX)
                .await,
            ReadOutcome::Unavailable
        );
    }
    // Phase `references_survive_a_restart_of_the_store`.
    {
        let dir = tempfile::tempdir().unwrap();
        let photos = scope("room-1", "so.tilo.photos");
        let hash = {
            let store = Arc::new(ContentStore::open(dir.path()).await.unwrap());
            let media = DocumentMedia::new(Arc::clone(&store), None);
            let hash = media.store(&photos, b"durable".to_vec()).await.unwrap();
            // Release the database before reopening it, as a daemon restart does.
            store.store().shutdown().await.unwrap();
            hash
        };
        let store = Arc::new(ContentStore::open(dir.path()).await.unwrap());
        let media = DocumentMedia::new(store, None);
        assert_eq!(
            media.read_range(photos, hash, 0, u32::MAX).await,
            ReadOutcome::Ready {
                bytes: b"durable".to_vec(),
                total_len: 7,
            }
        );
    }
    // Phase `oversized_media_is_refused`.
    {
        let media = DocumentMedia::new(Arc::new(ContentStore::memory()), None);
        let too_big = vec![0_u8; usize::try_from(MAX_BLOB_BYTES).unwrap() + 1];
        assert!(matches!(
            media
                .store(&scope("room-1", "so.tilo.photos"), too_big)
                .await,
            Err(BlobError::TooLarge { .. })
        ));
    }
}

const PROOF_SOCKET: &str = "TILO_MEDIA_PROOF_SOCKET";
const PROOF_HASH: &str = "TILO_MEDIA_PROOF_HASH";
const IMAGE: &[u8] = b"\x89PNG\r\n\x1a\n pixels put by one application";

fn launcher_connection(socket: &Path, room: &str, app: &str) -> BlobClient {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    write_handoff(&mut stream, &BlobPrincipal::App(scope(room, app))).unwrap();
    BlobClient::from_app_stream(stream).unwrap()
}

/// The capability end to end: one app process puts an image through the
/// launcher-scoped socket; a second process and the session (as the shell
/// stages launch content) read it; an app in another room holding the same
/// hash reads nothing.
#[test]
fn an_image_put_by_one_process_is_read_by_another_and_the_session_only() {
    if let (Some(socket), Some(hash)) = (
        std::env::var_os(PROOF_SOCKET),
        std::env::var(PROOF_HASH).ok(),
    ) {
        let reader = launcher_connection(Path::new(&socket), "room-1", "so.tilo.photos");
        assert_eq!(
            reader.read_all(&hash).unwrap(),
            ReadOutcome::Ready {
                bytes: IMAGE.to_vec(),
                total_len: IMAGE.len() as u64,
            }
        );
        return;
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fs-blobs.sock");
    let _server = {
        let _guard = runtime.enter();
        let media = DocumentMedia::new(Arc::new(ContentStore::memory()), None);
        serve_socket(&socket, media).unwrap()
    };

    let writer = launcher_connection(&socket, "room-1", "so.tilo.photos");
    let stored = writer.put(IMAGE, "image/png").unwrap();
    assert_eq!(stored.hash, blake3::hash(IMAGE).to_hex().to_string());

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "document_media::tests::an_image_put_by_one_process_is_read_by_another_and_the_session_only",
            "--nocapture",
        ])
        .env(PROOF_SOCKET, &socket)
        .env(PROOF_HASH, &stored.hash)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "the second process could not read the image: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // libtest exits 0 when `--exact` selects nothing; require the child ran.
    assert!(
        stdout.contains("test result: ok. 1 passed"),
        "the child ran no test: {stdout}"
    );

    let session = BlobClient::connect_session(&socket).unwrap();
    let shell_view = session.scope("room-1", "so.tilo.photos").unwrap();
    assert!(matches!(
        shell_view.read_all(&stored.hash).unwrap(),
        ReadOutcome::Ready { .. }
    ));

    let stranger = launcher_connection(&socket, "room-2", "so.tilo.chat");
    assert_eq!(
        stranger.read_all(&stored.hash).unwrap(),
        ReadOutcome::Unavailable
    );
    assert!(!stranger.has(&stored.hash).unwrap());
    let elsewhere = session.scope("room-2", "so.tilo.photos").unwrap();
    assert_eq!(
        elsewhere.read_all(&stored.hash).unwrap(),
        ReadOutcome::Unavailable
    );
}

/// Two in-process daemons: a collaborator's device fetches a referenced blob
/// for a room its person belongs to, and for no other.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_room_member_fetches_referenced_media_from_a_peer() {
    let (alice_ep, _alice_lookup, alice_identity) = bind_certified_test_endpoint().await;
    let (bob_ep, bob_lookup, bob_identity) = bind_certified_test_endpoint().await;
    let (carol_ep, carol_lookup, carol_identity) = bind_certified_test_endpoint().await;
    bob_lookup.add_endpoint_info(alice_ep.addr());
    carol_lookup.add_endpoint_info(alice_ep.addr());

    let membership = CapOverride::membership(
        Subject::person(hex::encode(bob_identity.user_public_key())),
        "room-1",
        Verb::Read,
    );
    let policy = Some(AclHandle::new(WorkspaceAcl::from_capabilities(
        alice_identity.device_public_key(),
        &[membership],
        &[],
        None,
    )));
    let alice_store = Arc::new(ContentStore::memory());
    let alice_media = DocumentMedia::new(Arc::clone(&alice_store), None);
    let chat = scope("room-1", "so.tilo.chat");
    let hash = alice_media.store(&chat, IMAGE.to_vec()).await.unwrap();
    // Also referenced by a room Bob is not in.
    let secret_room = scope("room-2", "so.tilo.chat");
    let secret = alice_media
        .store(&secret_room, b"another room".to_vec())
        .await
        .unwrap();
    let router = accept_media_protocol(
        Router::builder(alice_ep.clone()),
        &alice_ep,
        &alice_identity,
        &policy,
        &alice_store,
    )
    .spawn();

    let device = |endpoint: &Endpoint, identity: &FsTransportIdentity| {
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint.clone(), Arc::clone(&store)));
        hydrator.add_peer(alice_ep.addr());
        DocumentMedia::new(
            store,
            Some(Arc::new(PeerMediaFetcher::new(
                endpoint.clone(),
                identity.clone(),
                hydrator,
            ))),
        )
    };
    let bob = device(&bob_ep, &bob_identity);

    assert_eq!(
        bob.read_range(chat.clone(), hash, 0, 16).await,
        ReadOutcome::Pending
    );
    assert_eq!(
        settle(&bob, &chat, hash).await,
        ReadOutcome::Ready {
            bytes: IMAGE.to_vec(),
            total_len: IMAGE.len() as u64,
        }
    );
    // Now referenced on Bob's device too, so it no longer needs Alice.
    assert!(bob.contains(&chat, hash).await);

    // Another app in the room is served what chat referenced.
    let notes = scope("room-1", "so.tilo.notes");
    assert!(matches!(
        settle(&bob, &notes, hash).await,
        ReadOutcome::Ready { .. }
    ));
    // Nor content of a room Bob is not a member of, even with its hash.
    assert_eq!(
        settle(&bob, &secret_room, secret).await,
        ReadOutcome::Unavailable
    );
    // And a device whose person holds no membership gets nothing at all.
    let carol = device(&carol_ep, &carol_identity);
    assert_eq!(settle(&carol, &chat, hash).await, ReadOutcome::Unavailable);
    drop(router);
}
