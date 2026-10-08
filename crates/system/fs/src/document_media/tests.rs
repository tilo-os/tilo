use runtime_fs::blobs::{BlobClient, BlobPrincipal, write_handoff};

use super::*;

fn scope(room: &str, app: &str) -> BlobScope {
    BlobScope::new(room, app).unwrap()
}

#[tokio::test]
async fn stored_media_is_readable_only_by_reference_bounded_and_durable() {
    // Phase `bytes_in_the_store_are_not_readable_without_a_reference`.
    {
        let store = Arc::new(ContentStore::memory());
        let media = DocumentMedia::new(Arc::clone(&store));
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
            let media = DocumentMedia::new(Arc::clone(&store));
            let hash = media.store(&photos, b"durable".to_vec()).await.unwrap();
            // Release the database before reopening it, as a daemon restart does.
            store.store().shutdown().await.unwrap();
            hash
        };
        let store = Arc::new(ContentStore::open(dir.path()).await.unwrap());
        let media = DocumentMedia::new(store);
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
        let media = DocumentMedia::new(Arc::new(ContentStore::memory()));
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
        let media = DocumentMedia::new(Arc::new(ContentStore::memory()));
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
