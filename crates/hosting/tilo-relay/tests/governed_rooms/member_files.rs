//! A visitor's file reaches the room's host whole, as large as the host
//! keeps: the pieces answering the host's own request count against its
//! attachment quotas, not the visitor's hourly content budget, which is far
//! smaller than the largest file the host keeps.
//!
//! A large file crosses in windows — an answer carries what the host lacks of
//! it, up to [`neighbour_wire::content::PIECES_PER_ANSWER`] pieces — and the
//! host asks again for the rest once the pieces stop arriving, so a piece a
//! session's outbound queue dropped costs a re-ask rather than leaving the
//! file unfinished until someone writes again.
use std::time::{Duration, Instant};

use super::*;
use neighbour_wire::content::{PIECE_PATIENCE_MS, PIECES_PER_ANSWER};
use tilo_relay::room_bots::attachments::{AttachmentLimits, Referenced, WorkspaceAttachments};

/// A room where Alice's laptop hosts a Lounge, her home device keeps the
/// members' files, and Bob's phone is in it with `room`'s id.
struct Room {
    relay: Relay,
    listing: WorkspaceListing,
    host: TestDevice,
    phone: TestDevice,
    bob: TestUser,
    keeper: std::sync::Arc<WorkspaceAttachments>,
    _state: tempfile::TempDir,
}

impl Room {
    /// A room whose host keeps members' files within `limits`.
    async fn start(limits: AttachmentLimits) -> Self {
        let alice = TestUser::fresh("Alice");
        let bob = TestUser::fresh("Bob");
        let host = alice.enrol("alice-home", DeviceCapabilities::host());
        let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
        let phone = bob.enrol("bob-browser", DeviceCapabilities::member());
        let state = tempfile::tempdir().unwrap();
        // A public host: every visitor has the member budgets.
        let budgets = tilo_relay::member_limits::MemberLimitsConfig::default();
        let relay = crate::common::start_relay(
            RelayConfig {
                state_root: state.path().to_path_buf(),
                member_limits: Some(budgets),
                ..RelayConfig::default()
            },
            tilo_relay::daemon::RelayIdentity {
                user_public_key: alice.user_pk,
                device_signing_key: host.sk.clone(),
                device_cert: host.cert.clone(),
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        let room = create_and_enter(&relay, &laptop, "Lounge").await;
        super::sealed_records::join(&relay, &laptop, &room.workspace, &bob, &phone, [5; 32])
            .await
            .unwrap();
        let keeper = WorkspaceAttachments::new(
            relay.workspaces().unwrap().clone(),
            room.workspace.clone(),
            host.device_pubkey(),
            relay.content().get(&room.workspace).unwrap(),
            state.path().join("attachments.json"),
            limits,
        );
        let kept: std::sync::Arc<dyn tilo_relay::workspaces::ContentKeeper> = keeper.clone();
        relay.workspaces().unwrap().keep_content(&kept);
        Self {
            relay,
            listing: room,
            host,
            phone,
            bob,
            keeper,
            _state: state,
        }
    }

    /// What the host asked the room for after Bob posted `file`: one request
    /// per file it lacks, the `first` of them.
    fn asked_for(&self, file: &[u8]) -> Vec<u8> {
        let asks = self.keeper.asks(
            &[Referenced {
                hash: blake3::hash(file).to_hex().to_string(),
                member: hex::encode(self.bob.user_pk),
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
        Ok(NeighbourMsg::RecordContentWant { hash: asked, .. }) if asked == hash
    )
}

/// Bob's page answers `ask` with the pieces the host lacks of `file`, as every
/// participant answers them ([`neighbour_wire::content::answer`]), and leaves
/// out one piece when `lost` says the relay's route drops it. How many pieces
/// it sent.
fn answer_window(
    session: &RelayCoeditSession,
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
    room: &Room,
    session: &RelayCoeditSession,
    hash: &str,
) -> Option<(Vec<u8>, Duration)> {
    let began = Instant::now();
    tilo_test_env::within("the host asks again for what it is missing", async {
        loop {
            if room.keeper.kept_for(hash).is_some() {
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
    room: &Room,
    session: &RelayCoeditSession,
    file: &[u8],
    hash: &str,
    lost: bool,
) -> (usize, Duration) {
    let mut ask = room.asked_for(file);
    let mut lost = lost;
    let began = Instant::now();
    let mut windows = 0_usize;
    loop {
        windows = windows.saturating_add(1);
        let answered = answer_window(
            session,
            (&room.host, &room.phone),
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
        match next_ask(room, session, hash).await {
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
    assert!(
        tilo_relay::member_limits::MemberLimitsConfig::default().content_bytes_per_hour
            < limits.file_bytes,
        "the visitor's content budget alone would not carry the file"
    );
    let room = Room::start(limits).await;
    // Bob posted a file exactly as large as the host keeps; the host asks for
    // it, and his page answers as a browser page does.
    let file: Vec<u8> = (0..=u8::MAX)
        .cycle()
        .take(usize::try_from(limits.file_bytes).unwrap())
        .collect();
    let hash = blake3::hash(&file).to_hex().to_string();
    let bob_id = hex::encode(room.bob.user_pk);
    let session = session(&room.relay, &room.phone, room.listing.workspace_id).await;
    crate::common::wait_until_subscribed(
        &room.relay,
        room.listing.workspace_id,
        &room.phone.device_pubkey(),
    )
    .await;
    // The host asks again for what a window left missing, as the relay runs
    // its keeper.
    tokio::spawn(std::sync::Arc::clone(&room.keeper).run());

    let (windows, took) = send_until_kept(&room, &session, &file, &hash, false).await;
    eprintln!(
        "{} MiB in {windows} windows, whole in {took:?}",
        limits.file_bytes / (1024 * 1024)
    );
    assert!(windows > 1, "the file crosses in windows of pieces");
    assert_eq!(room.keeper.kept_for(&hash), Some(bob_id));
    room.relay.shutdown().await;
}

/// A piece the relay's content route loses on the way to the host does not
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
    let room = Room::start(limits).await;
    let file: Vec<u8> = (0..=u8::MAX)
        .cycle()
        .take(usize::try_from(limits.file_bytes).unwrap())
        .collect();
    let hash = blake3::hash(&file).to_hex().to_string();
    let bob_id = hex::encode(room.bob.user_pk);
    let session = session(&room.relay, &room.phone, room.listing.workspace_id).await;
    crate::common::wait_until_subscribed(
        &room.relay,
        room.listing.workspace_id,
        &room.phone.device_pubkey(),
    )
    .await;
    tokio::spawn(std::sync::Arc::clone(&room.keeper).run());

    let (windows, took) = send_until_kept(&room, &session, &file, &hash, true).await;
    eprintln!(
        "{} MiB in {windows} windows, one piece lost, whole in {took:?}",
        limits.file_bytes / (1024 * 1024)
    );
    assert_eq!(room.keeper.kept_for(&hash), Some(bob_id));
    assert_eq!(
        windows, 2,
        "one window of pieces, then the piece that was lost"
    );
    assert!(
        took < Duration::from_secs(8),
        "a lost piece costs a re-ask, not a minute: {took:?}"
    );
    room.relay.shutdown().await;
}
