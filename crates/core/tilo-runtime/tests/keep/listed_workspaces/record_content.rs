//! A picture a page kept for a record of an app — its Photos library, a
//! Camera capture — reaches the workspace's host as the page pushes it, so a
//! page that reloads, or a member that joins after the writer left, reads it
//! from the workspace instead of waiting for a device that is gone.
//!
//! The record names the picture by reference alone, as every record does: a
//! sealed reference names the ciphertext's address, which is what the page
//! pushes and the host keeps; a page opens the ciphertext with the key the
//! reference carries. Each push is its member's hold on the content: the host
//! lets the bytes go once no member holds them, because each let go of them or
//! is no member any more.
use super::*;

const PHOTOS: &str = "so.tilo.photos";

/// Push `bytes`, kept at `address`, from `device` over `linked` to the host
/// `host` of `workspace`, unasked, as a page pushes what it keeps.
fn push(
    linked: &Linked,
    device: &TestDevice,
    host: &[u8; 32],
    workspace: &str,
    address: &str,
    bytes: &[u8],
) {
    let mut frames = Vec::new();
    let mut packer = neighbour_wire::content::Packer::new(
        tilo_peer_state::peer_handle(&device.device_pubkey()),
        tilo_peer_state::peer_handle(host),
        workspace,
        |frame| frames.push(frame),
    );
    let total = u64::try_from(bytes.len()).unwrap();
    for (at, piece) in bytes
        .chunks(neighbour_wire::content::PIECE_BYTES)
        .enumerate()
    {
        packer.piece(neighbour_wire::content::Piece {
            app_id: PHOTOS.into(),
            hash: address.to_owned(),
            total,
            offset: u64::try_from(at.saturating_mul(neighbour_wire::content::PIECE_BYTES)).unwrap(),
            bytes: piece.to_vec(),
        });
    }
    packer.finish();
    for frame in frames {
        linked.send(frame).expect("the link carries every piece");
    }
}

/// Let go of `device`'s hold on `address` of `workspace`, over `linked`, as a
/// page does once its own write drops the record's last reference to it.
fn release(linked: &Linked, device: &TestDevice, workspace: &str, address: &str) {
    linked
        .send(NeighbourMsg::DocumentContentReleased {
            requester: tilo_peer_state::peer_handle(&device.device_pubkey()),
            app_id: PHOTOS.into(),
            cluster_id: workspace.to_owned(),
            hash: address.to_owned(),
        })
        .expect("the link carries the release");
}

/// The host's word, as `linked` hears it, that it keeps `address`, which the
/// device `device` pushed it: from the host's device, to that device alone.
async fn kept(linked: &mut Linked, device: &TestDevice, host: &[u8; 32], address: &str) {
    tilo_test_env::within("the host says it keeps what the page pushed", async {
        loop {
            let Some(heard) = linked.next().await else {
                panic!("the link ended before the host said it keeps the picture");
            };
            if let NeighbourMsg::DocumentContentKept {
                responder,
                requester,
                app_id,
                hash,
                ..
            } = &heard.message
                && hash == address
            {
                assert_eq!(
                    (*responder, *requester, app_id.as_str()),
                    (
                        tilo_peer_state::peer_handle(host),
                        tilo_peer_state::peer_handle(&device.device_pubkey()),
                        PHOTOS
                    ),
                    "the host's device says it keeps the picture to the device that pushed it"
                );
                return;
            }
        }
    })
    .await;
}

/// Whether `server` keeps `address` of `workspace`.
fn keeps(server: &Server, workspace: &str, address: &str) -> bool {
    server
        .content()
        .get(workspace)
        .is_ok_and(|store| store.has(address))
}

/// A picture sealed by `store`: its reference, the ciphertext's address, and
/// the ciphertext.
fn sealed_picture(store: &tilo_content::ContentStore, picture: &[u8]) -> (String, String, Vec<u8>) {
    let reference = store.put(picture).unwrap();
    assert!(tilo_content::is_sealed(&reference));
    let address = tilo_content::content_address(&reference).unwrap();
    let sealed = store.read(&address).unwrap();
    (reference, address, sealed)
}

/// What the host answers `linked`'s device when it asks how much of `workspace`'s content it
/// keeps (`workspace/servers#content`), or why it refused.
async fn content_held(
    linked: &Linked,
    workspace: &WorkspaceListing,
) -> Result<serde_json::Value, tilo_actions::remote::ActionCallError> {
    use super::governance_actions::{ask, invocation, outcome};
    let actions = linked
        .asking_in(&workspace.workspace)
        .await
        .expect("the host answers its actions link");
    let message = invocation(
        tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER,
        format!("workspace.permissions:{}/servers", workspace.workspace),
        "content",
        serde_json::json!({}),
    );
    outcome(ask(&actions, message).await)
}

/// What the host serves a device asking `workspace` for `address` of `PHOTOS`,
/// its pieces put together.
fn served(server: &Server, workspace: &str, asking: &TestDevice, address: &str) -> Vec<u8> {
    let want = postcard::to_allocvec(&NeighbourMsg::DocumentContentWants {
        requester: tilo_peer_state::peer_handle(&asking.device_pubkey()),
        cluster_id: workspace.to_owned(),
        wants: vec![neighbour_wire::content::Wanted {
            app_id: PHOTOS.to_owned(),
            hash: address.to_owned(),
            have: Vec::new(),
        }],
    })
    .unwrap();
    let mut whole = Vec::new();
    for frame in
        server
            .workspaces()
            .unwrap()
            .content_answer(workspace, &asking.device_pubkey(), &want)
    {
        let NeighbourMsg::DocumentContentPieces { pieces, .. } =
            postcard::from_bytes(&frame).unwrap()
        else {
            panic!("the host serves what it keeps");
        };
        for piece in pieces {
            assert_eq!(piece.hash, address, "only the content asked for");
            assert_eq!(piece.offset, u64::try_from(whole.len()).unwrap());
            whole.extend(piece.bytes);
        }
    }
    whole
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sealed_picture_a_page_pushes_reaches_the_host_and_outlives_its_writer() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let alice_home = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start(state.path(), &alice, &alice_home).await;
    let workspace = create_and_enter(&server, &laptop, "Photos").await;
    let mut bob_link =
        sealed_documents::joined(&server, &laptop, &workspace, &bob, &phone, [7; 32])
            .await
            .unwrap();
    let laptop_link = Linked::open(&server, &laptop, workspace.workspace_id)
        .await
        .expect("the workspace admits Alice's laptop");
    crate::keep::common::wait_until_subscribed(
        &server,
        workspace.workspace_id,
        &phone.device_pubkey(),
    )
    .await;
    let server_device = server.workspaces().unwrap().device_cert().device_public_key;
    assert_eq!(
        server.node_addr().id.as_bytes(),
        &server_device,
        "the host's endpoint is keyed by the device its certificate names"
    );
    assert_eq!(
        server.device_signing_key().verifying_key().to_bytes(),
        server_device,
        "the key its link signs as is its certified device"
    );

    // Bob's page keeps the picture sealed in its record's store and names the
    // reference: the host knows the ciphertext's address alone.
    let picture: Vec<u8> = b"a picture only Bob's page kept. "
        .iter()
        .copied()
        .cycle()
        .take(700 * 1024)
        .collect();
    let store = tilo_content::ContentStore::memory().sealing();
    let (reference, address, sealed) = sealed_picture(&store, &picture);
    // The workspace's record of the app names the reference, so the host knows
    // the ciphertext's address alone.
    crate::keep::common::write_record(
        server.workspaces().unwrap(),
        &workspace.workspace,
        (PHOTOS, ""),
        |doc| {
            doc.put(
                ROOT,
                "photo",
                format!(
                    "blake3:{}:{}:{reference}",
                    blake3::hash(&picture).to_hex(),
                    picture.len()
                ),
            )
            .map_err(|error| error.to_string())
        },
    )
    .await
    .expect("the record names the picture");

    // The page pushes the ciphertext beside the record, and the host says it
    // keeps it to Bob's device alone.
    push(
        &bob_link,
        &phone,
        &server_device,
        &workspace.workspace,
        &address,
        &sealed,
    );
    kept(&mut bob_link, &phone, &server_device, &address).await;
    assert!(
        keeps(&server, &workspace.workspace, &address),
        "the host keeps the picture's ciphertext once it says so"
    );
    // Its owner asks the host how much it keeps: the picture, held, and
    // nothing still arriving.
    let held = content_held(&laptop_link, &workspace)
        .await
        .expect("the owner asks how much content the host keeps");
    assert_eq!(
        (&held["held"], &held["going"]),
        (&serde_json::json!(1), &serde_json::json!(0)),
        "the host answers its owner that it keeps the picture a member pushed: {held}"
    );

    // A member who does not choose the workspace's servers is not answered.
    assert_eq!(
        content_held(&bob_link, &workspace).await,
        Err(tilo_actions::remote::ActionCallError::UnknownAction),
        "only the workspace's owners ask how much of its content its server keeps"
    );
    // Bob's page is gone; a page that reloads reads the picture from the host,
    // as ciphertext, and opens it with the key its record carries.
    drop(bob_link);
    let carol = TestUser::fresh("Carol");
    let carol_phone = carol.enrol("carol-phone", DeviceCapabilities::member());
    let carol_link =
        sealed_documents::joined(&server, &laptop, &workspace, &carol, &carol_phone, [8; 32])
            .await
            .unwrap();
    let delivered = served(&server, &workspace.workspace, &carol_phone, &address);
    assert_eq!(delivered, sealed, "the host serves the ciphertext alone");
    let reloaded = tilo_content::ContentStore::memory().sealing();
    reloaded.put_public(&delivered).unwrap();
    assert_eq!(reloaded.read(&reference).as_deref(), Some(&picture[..]));

    drop(carol_link);
    drop(laptop_link);
    server.shutdown().await;
}

/// Two members keep one picture: each push is that member's hold on it. The
/// bytes stay while either holds them, a push the host already keeps is said
/// to be kept again (its first word may have been lost on the way), and they
/// go once the second lets go too. A member removed from the workspace holds
/// nothing any more: their pushes go with them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_picture_two_members_push_stays_until_neither_holds_it() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let carol = TestUser::fresh("Carol");
    let alice_home = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let bob_phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let carol_phone = carol.enrol("carol-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start(state.path(), &alice, &alice_home).await;
    let workspace = create_and_enter(&server, &laptop, "Photos").await;
    let mut bob_link =
        sealed_documents::joined(&server, &laptop, &workspace, &bob, &bob_phone, [7; 32])
            .await
            .unwrap();
    let mut carol_link =
        sealed_documents::joined(&server, &laptop, &workspace, &carol, &carol_phone, [8; 32])
            .await
            .unwrap();
    for device in [&bob_phone, &carol_phone] {
        crate::keep::common::wait_until_subscribed(
            &server,
            workspace.workspace_id,
            &device.device_pubkey(),
        )
        .await;
    }
    let host = server.workspaces().unwrap().device_cert().device_public_key;
    let ws = workspace.workspace.as_str();

    // The same picture, pushed by both: one copy on the host, two holds.
    let store = tilo_content::ContentStore::memory().sealing();
    let (_, address, sealed) = sealed_picture(&store, b"the picture Bob and Carol both keep");
    push(&bob_link, &bob_phone, &host, ws, &address, &sealed);
    kept(&mut bob_link, &bob_phone, &host, &address).await;
    push(&carol_link, &carol_phone, &host, ws, &address, &sealed);
    kept(&mut carol_link, &carol_phone, &host, &address).await;
    // Carol's word went missing: she pushes again, and is told again.
    push(&carol_link, &carol_phone, &host, ws, &address, &sealed);
    kept(&mut carol_link, &carol_phone, &host, &address).await;

    // Bob lets go. What Bob pushes next is kept after his release is taken
    // in: the picture is still Carol's.
    release(&bob_link, &bob_phone, ws, &address);
    let (_, later, later_sealed) = sealed_picture(&store, b"a later picture of Bob's");
    push(&bob_link, &bob_phone, &host, ws, &later, &later_sealed);
    kept(&mut bob_link, &bob_phone, &host, &later).await;
    assert!(
        keeps(&server, ws, &address),
        "the host keeps the picture while Carol holds it"
    );

    // Carol lets go too: nobody holds it, and the bytes go.
    release(&carol_link, &carol_phone, ws, &address);
    tilo_test_env::wait_for("the host lets go of the picture no member holds", || {
        (!keeps(&server, ws, &address)).then_some(())
    });

    // Bob's later picture is his alone: once Alice removes him, it goes too.
    server
        .workspaces()
        .unwrap()
        .remove_member(
            ws,
            &laptop.cert,
            RemoveMemberRequest {
                request_id: [9; 32],
                user_public_key: bob.user_pk,
            },
        )
        .await
        .expect("Alice removes Bob");
    tilo_test_env::wait_for(
        "the host lets go of what only a removed member held",
        || (!keeps(&server, ws, &later)).then_some(()),
    );

    drop(bob_link);
    drop(carol_link);
    server.shutdown().await;
}
