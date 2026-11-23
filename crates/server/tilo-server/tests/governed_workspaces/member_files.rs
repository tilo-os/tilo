//! A visitor's file reaches the workspace's host whole, as large as the host
//! keeps: a member's content is bounded by what the host stores for a
//! workspace, not by a budget on how much they may send.
//!
//! A large file crosses in windows — an answer carries what the host lacks of
//! it, up to [`neighbour_wire::content::PIECES_PER_ANSWER`] pieces — and the
//! host asks again for the rest once the pieces stop arriving, so a piece a
//! session's outbound queue dropped costs a re-ask rather than leaving the
//! file unfinished until someone writes again.
use std::time::{Duration, Instant};

use super::*;
use neighbour_wire::content::{PIECE_PATIENCE_MS, PIECES_PER_ANSWER};
use tilo_server::chat_attachments::{AttachmentLimits, Referenced, WorkspaceAttachments};

/// A workspace where Alice's laptop hosts a Lounge, her home device keeps the
/// members' files, and Bob's phone is in it with `workspace`'s id.
struct Workspace {
    server: Server,
    listing: WorkspaceListing,
    host: TestDevice,
    laptop: TestDevice,
    phone: TestDevice,
    bob: TestUser,
    keeper: std::sync::Arc<WorkspaceAttachments>,
    state: tempfile::TempDir,
}

impl Workspace {
    /// A workspace whose host keeps members' files within `limits`.
    async fn start(limits: AttachmentLimits) -> Self {
        let alice = TestUser::fresh("Alice");
        let bob = TestUser::fresh("Bob");
        let host = alice.enrol("alice-home", DeviceCapabilities::member());
        let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
        let phone = bob.enrol("bob-browser", DeviceCapabilities::member());
        let state = tempfile::tempdir().unwrap();
        let server = crate::common::start_server(
            ServerConfig::loopback(state.path().to_path_buf()),
            tilo_server::daemon::ServerIdentity {
                user_public_key: alice.user_pk,
                device_signing_key: host.sk.clone(),
                device_cert: host.cert.clone(),
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        let workspace = create_and_enter(&server, &laptop, "Lounge").await;
        super::sealed_documents::join(
            &server,
            &laptop,
            &workspace.workspace,
            &bob,
            &phone,
            [5; 32],
        )
        .await
        .unwrap();
        let keeper = WorkspaceAttachments::new(
            server.workspaces().unwrap().clone(),
            workspace.workspace.clone(),
            host.device_pubkey(),
            server.content().get(&workspace.workspace).unwrap(),
            state.path().join("attachments.json"),
            limits,
        );
        let kept: std::sync::Arc<dyn tilo_server::workspaces::ContentKeeper> = keeper.clone();
        server.workspaces().unwrap().keep_content(&kept);
        Self {
            server,
            listing: workspace,
            host,
            laptop,
            phone,
            bob,
            keeper,
            state,
        }
    }

    /// What the host asked the workspace for after Bob posted `file`: one request
    /// per file it lacks, the `first` of them.
    fn asked_for(&self, file: &[u8]) -> Vec<u8> {
        let asks = self.keeper.asks(
            &[Referenced {
                hash: blake3::hash(file).to_hex().to_string(),
                member: tilo_resource::MemberId::person(hex::encode(self.bob.user_pk)),
                size: Some(u64::try_from(file.len()).unwrap()),
            }],
            tilo_clock::unix_now_ms(),
        );
        assert_eq!(asks.len(), 1, "the host asks for the file");
        asks[0].clone()
    }
}

/// Whether `payload`, a presence frame, is the host's request for `hash`.
fn asks_for(payload: &[u8], hash: &str) -> bool {
    matches!(
        postcard::from_bytes::<NeighbourMsg>(payload),
        Ok(NeighbourMsg::DocumentContentWant { hash: asked, .. }) if asked == hash
    )
}

/// Bob's page answers `ask` with the pieces the host lacks of `file`, as every
/// key holder answers them ([`neighbour_wire::content::answer`]), and leaves
/// out one piece when `lost` says the server's route drops it. How many pieces
/// it sent.
fn answer_window(
    session: &ServerCoeditSession,
    (host, phone): (&TestDevice, &TestDevice),
    ask: &[u8],
    file: &[u8],
    hash: &str,
    lost: &mut bool,
) -> usize {
    let pieces = neighbour_wire::content::answer(
        &phone.device_pubkey(),
        &host.device_pubkey(),
        ask,
        |_, _, wanted| (wanted == hash).then(|| file.to_vec()),
    );
    let answered = pieces.len();
    for (index, piece) in pieces.into_iter().enumerate() {
        if *lost && index == 1 {
            *lost = false;
            continue;
        }
        session
            .publish_record_content(host.device_pubkey(), piece)
            .expect("the session carries every piece of a window");
    }
    answered
}

/// The host's next request for `hash`, as the page in `session` hears it, and
/// how long it took to come: `None` once it keeps the file, which an answer
/// that completes it does instead of asking.
async fn next_ask(
    workspace: &Workspace,
    session: &ServerCoeditSession,
    hash: &str,
) -> Option<(Vec<u8>, Duration)> {
    let began = Instant::now();
    tilo_test_env::within("the host asks again for what it is missing", async {
        loop {
            if workspace.keeper.kept_for(hash).is_some() {
                return None;
            }
            let heard = tokio::time::timeout(Duration::from_millis(50), session.recv()).await;
            if let Ok(Some(Inbound::Presence { payload, .. })) = heard
                && asks_for(&payload, hash)
            {
                return Some((payload, began.elapsed()));
            }
        }
    })
    .await
}

/// Answer the host's requests until it keeps `file`, as its page does, losing
/// `lost`'s piece once: the windows it took, and how long it took.
async fn send_until_kept(
    workspace: &Workspace,
    session: &ServerCoeditSession,
    file: &[u8],
    hash: &str,
    lost: bool,
) -> (usize, Duration) {
    let mut ask = workspace.asked_for(file);
    let mut lost = lost;
    let began = Instant::now();
    let mut windows = 0_usize;
    loop {
        windows = windows.saturating_add(1);
        let answered = answer_window(
            session,
            (&workspace.host, &workspace.phone),
            &ask,
            file,
            hash,
            &mut lost,
        );
        assert!(answered > 0, "the page answers what it keeps");
        assert!(
            answered <= PIECES_PER_ANSWER,
            "an answer carries {answered} pieces, never the whole file at once"
        );
        match next_ask(workspace, session, hash).await {
            None => return (windows, began.elapsed()),
            Some((again, took)) => {
                assert!(
                    took < Duration::from_millis(PIECE_PATIENCE_MS * 16),
                    "the host asks again within a moment of the pieces it got, not a minute: {took:?}"
                );
                ask = again;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_visitors_file_as_large_as_the_host_keeps_reaches_it() {
    let limits = AttachmentLimits::default();
    let workspace = Workspace::start(limits).await;
    // Bob posted a file exactly as large as the host keeps; the host asks for
    // it, and his page answers as a browser page does.
    let file: Vec<u8> = (0..=u8::MAX)
        .cycle()
        .take(usize::try_from(limits.file_bytes).unwrap())
        .collect();
    let hash = blake3::hash(&file).to_hex().to_string();
    let bob_id = tilo_resource::MemberId::person(hex::encode(workspace.bob.user_pk));
    let session = session(
        &workspace.server,
        &workspace.phone,
        workspace.listing.workspace_id,
    )
    .await;
    crate::common::wait_until_subscribed(
        &workspace.server,
        workspace.listing.workspace_id,
        &workspace.phone.device_pubkey(),
    )
    .await;
    // The host asks again for what a window left missing, as the server runs
    // its keeper.
    tokio::spawn(std::sync::Arc::clone(&workspace.keeper).run());

    let (windows, took) = send_until_kept(&workspace, &session, &file, &hash, false).await;
    eprintln!(
        "{} MiB in {windows} windows, whole in {took:?}",
        limits.file_bytes / (1024 * 1024)
    );
    assert!(windows > 1, "the file crosses in windows of pieces");
    assert_eq!(workspace.keeper.kept_for(&hash), Some(bob_id));
    workspace.server.shutdown().await;
}

/// A piece the server's content route loses on the way to the host does not
/// leave the file unfinished until someone writes again: the host asks again
/// within [`PIECE_PATIENCE_MS`] of the last piece it got, naming what it
/// holds, and the piece the page answers with completes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_piece_lost_on_the_way_is_asked_for_again_within_a_second() {
    // A host keeping small files: the file is a handful of pieces, so what
    // this measures is the re-ask a lost piece costs, not the transfer.
    const KEPT: u64 = 2 * 1024 * 1024;
    let limits = AttachmentLimits {
        file_bytes: KEPT,
        member_bytes: 8 * KEPT,
        workspace_bytes: 64 * KEPT,
    };
    let workspace = Workspace::start(limits).await;
    let file: Vec<u8> = (0..=u8::MAX)
        .cycle()
        .take(usize::try_from(limits.file_bytes).unwrap())
        .collect();
    let hash = blake3::hash(&file).to_hex().to_string();
    let bob_id = tilo_resource::MemberId::person(hex::encode(workspace.bob.user_pk));
    let session = session(
        &workspace.server,
        &workspace.phone,
        workspace.listing.workspace_id,
    )
    .await;
    crate::common::wait_until_subscribed(
        &workspace.server,
        workspace.listing.workspace_id,
        &workspace.phone.device_pubkey(),
    )
    .await;
    tokio::spawn(std::sync::Arc::clone(&workspace.keeper).run());

    let (windows, took) = send_until_kept(&workspace, &session, &file, &hash, true).await;
    eprintln!(
        "{} MiB in {windows} windows, one piece lost, whole in {took:?}",
        limits.file_bytes / (1024 * 1024)
    );
    assert_eq!(workspace.keeper.kept_for(&hash), Some(bob_id));
    assert_eq!(
        windows, 2,
        "one window of pieces, then the piece that was lost"
    );
    assert!(
        took < Duration::from_secs(8),
        "a lost piece costs a re-ask, not a minute: {took:?}"
    );
    workspace.server.shutdown().await;
}

/// Every file under `root`, read whole.
fn files_under(root: &std::path::Path) -> Vec<Vec<u8>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else if let Ok(bytes) = std::fs::read(&path) {
            found.push(bytes);
        }
    }
    found
}

/// What the host serves a member of `workspace` asking for `address`, its
/// pieces put together.
fn served(workspace: &Workspace, member: [u8; 32], address: &str) -> Vec<u8> {
    let want = postcard::to_allocvec(&NeighbourMsg::DocumentContentWant {
        requester: tilo_peer_state::peer_handle(&member),
        app_id: "so.tilo.chat".into(),
        cluster_id: workspace.listing.workspace.clone(),
        hash: address.to_owned(),
        have: Vec::new(),
    })
    .unwrap();
    let mut whole = Vec::new();
    for frame in workspace.server.workspaces().unwrap().content_answer(
        &workspace.listing.workspace,
        &member,
        &want,
    ) {
        let NeighbourMsg::DocumentContentPiece { offset, bytes, .. } =
            postcard::from_bytes(&frame).unwrap()
        else {
            panic!("the host serves what it keeps");
        };
        assert_eq!(offset, u64::try_from(whole.len()).unwrap());
        whole.extend(bytes);
    }
    whole
}

/// A picture Bob posts in a sealed chat reaches the host, and stays there
/// for whoever joins later, as ciphertext alone: nothing on the host's disk
/// and nothing it serves holds the picture, while every member holding the
/// message (its reference, with the key) reads it, Carol too, who joined
/// after it was posted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sealed_attachment_is_ciphertext_on_the_host_and_its_readers_open_it() {
    let workspace = Workspace::start(AttachmentLimits::default()).await;
    let picture: Vec<u8> = b"a picture only the chat's members see. "
        .iter()
        .copied()
        .cycle()
        .take(700 * 1024)
        .collect();
    // Bob's chat window keeps the attachment in its record's store, which
    // seals; the message keeps the reference.
    let bob_store = tilo_content::ContentStore::memory().sealing();
    let reference = bob_store.put(&picture).unwrap();
    let address = tilo_content::content_address(&reference).unwrap();
    assert!(tilo_content::is_sealed(&reference));
    let sealed = bob_store.read(&address).unwrap();
    assert!(!sealed.windows(39).any(|w| w == &picture[..39]));

    let session = session(
        &workspace.server,
        &workspace.phone,
        workspace.listing.workspace_id,
    )
    .await;
    crate::common::wait_until_subscribed(
        &workspace.server,
        workspace.listing.workspace_id,
        &workspace.phone.device_pubkey(),
    )
    .await;
    tokio::spawn(std::sync::Arc::clone(&workspace.keeper).run());
    let mut ask = workspace.keeper.asks(
        &[Referenced {
            hash: address.clone(),
            member: tilo_resource::MemberId::person(hex::encode(workspace.bob.user_pk)),
            size: Some(u64::try_from(sealed.len()).unwrap()),
        }],
        tilo_clock::unix_now_ms(),
    );
    assert_eq!(ask.len(), 1, "the host asks for the ciphertext's address");
    let mut lost = false;
    loop {
        answer_window(
            &session,
            (&workspace.host, &workspace.phone),
            &ask[0],
            &sealed,
            &address,
            &mut lost,
        );
        match next_ask(&workspace, &session, &address).await {
            None => break,
            Some((again, _)) => ask = vec![again],
        }
    }
    assert!(workspace.keeper.kept_for(&address).is_some());

    let on_disk = files_under(workspace.state.path());
    assert!(
        on_disk.iter().any(|file| file == &sealed),
        "the host keeps the ciphertext"
    );
    assert!(
        on_disk
            .iter()
            .all(|file| !file.windows(39).any(|w| w == &picture[..39])),
        "nothing on the host's disk holds the picture"
    );
    let bytes = served(&workspace, [7; 32], &address);
    assert_eq!(bytes, sealed, "the host serves the ciphertext alone");

    // Alice's laptop and Bob's phone read the message's reference.
    let alice_store = tilo_content::ContentStore::memory().sealing();
    alice_store.put_public(&bytes).unwrap();
    assert_eq!(alice_store.read(&reference), Some(picture.clone()));
    assert_eq!(bob_store.read(&reference), Some(picture.clone()));
    assert_eq!(
        alice_store.read(&address),
        Some(sealed.clone()),
        "the address alone, all the host knows, is ciphertext"
    );

    // Carol joins afterwards: the message's reference reaches her with the
    // record's history, and the host she fetches the picture from still
    // holds it sealed.
    let carol = TestUser::fresh("Carol");
    let carol_phone = carol.enrol("carol-phone", DeviceCapabilities::member());
    super::sealed_documents::join(
        &workspace.server,
        &workspace.laptop,
        &workspace.listing.workspace,
        &carol,
        &carol_phone,
        [6; 32],
    )
    .await
    .unwrap();
    let carol_store = tilo_content::ContentStore::memory().sealing();
    carol_store
        .put_public(&served(&workspace, [9; 32], &address))
        .unwrap();
    assert_eq!(carol_store.read(&reference), Some(picture));
    workspace.server.shutdown().await;
}
