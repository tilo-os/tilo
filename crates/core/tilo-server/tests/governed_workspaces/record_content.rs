//! A picture a page kept for a record of an app the host has no keeper of its
//! own for — its Photos library, a Camera capture — reaches the workspace's
//! host, so a page that reloads, or a member that joins after the writer left,
//! reads it from the workspace instead of waiting for a device that is gone.
//!
//! The record names the picture by reference alone, as every record does: a
//! sealed reference names the ciphertext's address, which is what the host
//! asks for and keeps; a page opens the ciphertext with the key the reference
//! carries.
use std::time::Duration;

use super::*;

const PHOTOS: &str = "so.tilo.photos";

/// The host's next request for `address` of `PHOTOS`, as `linked` hears it.
async fn asked_for(linked: &mut Linked, address: &str) -> Vec<u8> {
    asked_for_each(linked, &[address]).await.remove(0)
}

/// The host's next request for each of `addresses` of `PHOTOS`, in that order, as `linked`
/// hears them in whatever order they come.
async fn asked_for_each(linked: &mut Linked, addresses: &[&str]) -> Vec<Vec<u8>> {
    tilo_test_env::within("the host asks for the record's content", async {
        let mut asks: Vec<Option<Vec<u8>>> = vec![None; addresses.len()];
        while asks.iter().any(Option::is_none) {
            let Ok(Some(heard)) =
                tokio::time::timeout(Duration::from_millis(50), linked.next()).await
            else {
                continue;
            };
            if let NeighbourMsg::DocumentContentWants { wants, .. } = &heard.message {
                for (at, address) in addresses.iter().enumerate() {
                    if wants.iter().any(|want| want.hash == *address) {
                        asks[at]
                            .get_or_insert_with(|| postcard::to_stdvec(&heard.message).unwrap());
                    }
                }
            }
        }
        asks.into_iter().flatten().collect()
    })
    .await
}

/// `ask`, a content request, with only the want for `address` left: a device
/// answers that one and the rest is the answer the test loses.
fn only(ask: &[u8], address: &str) -> Vec<u8> {
    let NeighbourMsg::DocumentContentWants {
        requester,
        cluster_id,
        wants,
    } = postcard::from_bytes(ask).unwrap()
    else {
        panic!("a content request");
    };
    postcard::to_allocvec(&NeighbourMsg::DocumentContentWants {
        requester,
        cluster_id,
        wants: wants
            .into_iter()
            .filter(|want| want.hash == address)
            .collect(),
    })
    .unwrap()
}

/// The blobs `server` says, in `workspace`'s copy status, that it lacks, once that is `blobs`:
/// what each device reads to tell its person which files are on it alone.
async fn says_it_lacks(server: &Server, workspace: &str, blobs: u32, what: &str) {
    let workspaces = server.workspaces().unwrap();
    tilo_test_env::within(what, async {
        loop {
            let status = workspaces.copies(workspace).await.unwrap();
            let lacked: u32 = status
                .shortfall
                .iter()
                .map(|short| match short {
                    tilo_peer_state::Shortfall::Holder {
                        missing: tilo_peer_state::Missing::Content { blobs },
                        ..
                    } => *blobs,
                    _ => 0,
                })
                .sum();
            if lacked == blobs {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
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
async fn a_sealed_picture_a_record_names_reaches_the_host_and_outlives_its_writer() {
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
    crate::common::wait_until_subscribed(&server, workspace.workspace_id, &phone.device_pubkey())
        .await;

    // Bob's page keeps the picture sealed in its record's store; the workspace's record of the
    // app names the reference, so the host knows the ciphertext's address alone.
    let picture: Vec<u8> = b"a picture only Bob's page kept. "
        .iter()
        .copied()
        .cycle()
        .take(700 * 1024)
        .collect();
    let store = tilo_content::ContentStore::memory().sealing();
    let reference = store.put(&picture).unwrap();
    assert!(tilo_content::is_sealed(&reference));
    let address = tilo_content::content_address(&reference).unwrap();
    let sealed = store.read(&address).unwrap();
    crate::common::write_record(
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

    // The host asks for the address the store keys the ciphertext under, and
    // Bob's page answers it as a page does.
    let ask = asked_for(&mut bob_link, &address).await;
    let server_device = server.workspaces().unwrap().device_cert().device_public_key;
    match postcard::from_bytes::<NeighbourMsg>(&ask).unwrap() {
        NeighbourMsg::DocumentContentWants {
            cluster_id,
            wants,
            requester,
            ..
        } => {
            let asked = wants
                .iter()
                .find(|want| want.hash == address)
                .expect("the ask names the ciphertext's address");
            assert_eq!(
                (asked.app_id.as_str(), cluster_id.as_str()),
                (PHOTOS, workspace.workspace.as_str()),
                "the host asks the record's app for the ciphertext's address"
            );
            assert_eq!(
                requester,
                tilo_peer_state::peer_handle(&server_device),
                "the ask names the device the host's link signs as"
            );
        }
        _ => panic!("a content request"),
    }
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
    // Until it is answered, the host says it lacks the picture: read from the one record that
    // names it, with no pass over the whole workspace since.
    says_it_lacks(
        &server,
        &workspace.workspace,
        1,
        "the host says it lacks the picture the record names",
    )
    .await;
    // Its owner asks the host the same: the picture is still only on Bob's device.
    let held = content_held(&laptop_link, &workspace)
        .await
        .expect("the owner asks how much content the host keeps");
    assert_eq!(
        held["lacking"], 1,
        "the host answers its owner that it lacks the picture: {held}"
    );
    let pieces = neighbour_wire::content::answer(
        &phone.device_pubkey(),
        &server_device,
        &ask,
        |app, cluster, wanted| {
            (app == PHOTOS && cluster == workspace.workspace && wanted == address)
                .then(|| sealed.clone())
        },
    );
    assert!(!pieces.is_empty(), "the page answers what it keeps");
    for piece in pieces {
        bob_link
            .send(postcard::from_bytes(&piece).unwrap())
            .expect("Bob's link carries every piece");
    }
    tilo_test_env::wait_for("the host keeps the picture's ciphertext", || {
        server
            .content()
            .get(&workspace.workspace)
            .ok()?
            .has(&address)
            .then_some(())
    });
    says_it_lacks(
        &server,
        &workspace.workspace,
        0,
        "the host says it lacks nothing once it keeps the picture",
    )
    .await;
    let held = content_held(&laptop_link, &workspace)
        .await
        .expect("the owner asks how much content the host keeps");
    assert_eq!(
        (&held["lacking"], &held["named"]),
        (&serde_json::json!(0), &serde_json::json!(1)),
        "the host answers its owner that it keeps all the content its records name: {held}"
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
    server.shutdown().await;
}

/// An ask the host sent that nothing answers, the ask or its answer lost on a link that fell
/// behind, is asked again once it lapses, though nothing writes to the record that names the
/// content since: the host keeps all the workspace's content once the device that has it is
/// there to answer.
#[test]
fn content_whose_ask_went_unanswered_is_asked_for_again() {
    // The host reads the wall clock on its own threads, so the test takes the
    // process to itself and moves the clock past an ask's lifetime instead of
    // waiting it out.
    tilo_test_env::in_own_process(
        module_path!(),
        "content_whose_ask_went_unanswered_is_asked_for_again",
        || {
            let process = tilo_clock::testing::OwnProcess::of_a_test_in_its_own_process();
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .expect("a runtime for this test alone")
                .block_on(an_unanswered_ask_is_asked_again(process));
        },
    );
}

async fn an_unanswered_ask_is_asked_again(process: tilo_clock::testing::OwnProcess) {
    let started = tilo_clock::wall();
    let _held = tilo_clock::testing::hold_wall_everywhere(process, started);
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
    crate::common::wait_until_subscribed(&server, workspace.workspace_id, &phone.device_pubkey())
        .await;

    // Bob's page keeps two pictures; the workspace's record of the app names both.
    let store = tilo_content::ContentStore::memory().sealing();
    let mut pictures = Vec::new();
    let mut named = Vec::new();
    for (field, name) in [("photo", "answered"), ("picture", "lost")] {
        let picture = format!("the picture whose ask is {name}").into_bytes();
        let reference = store.put(&picture).unwrap();
        let address = tilo_content::content_address(&reference).unwrap();
        named.push((
            field.to_owned(),
            format!(
                "blake3:{}:{}:{reference}",
                blake3::hash(&picture).to_hex(),
                picture.len()
            ),
        ));
        pictures.push((address.clone(), store.read(&address).unwrap()));
    }
    crate::common::write_record(
        server.workspaces().unwrap(),
        &workspace.workspace,
        (PHOTOS, ""),
        |doc| {
            for (field, reference) in &named {
                doc.put(ROOT, field.as_str(), reference.as_str())
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        },
    )
    .await
    .expect("the record names both pictures");
    let [(answered, answered_bytes), (lost, lost_bytes)] = <[_; 2]>::try_from(pictures).unwrap();

    let answer = |linked: &Linked, ask: &[u8], address: &str, bytes: &[u8]| {
        for frame in neighbour_wire::content::answer(
            &phone.device_pubkey(),
            &alice_home.device_pubkey(),
            &only(ask, address),
            |app, cluster, wanted| {
                (app == PHOTOS && cluster == workspace.workspace && wanted == address)
                    .then(|| bytes.to_vec())
            },
        ) {
            linked
                .send(postcard::from_bytes(&frame).unwrap())
                .expect("Bob's link carries every piece");
        }
    };
    // The host asks for both; the answer to one of them never arrives.
    let asks = asked_for_each(&mut bob_link, &[&answered, &lost]).await;
    answer(&bob_link, &asks[0], &answered, &answered_bytes);
    says_it_lacks(
        &server,
        &workspace.workspace,
        1,
        "the host keeps the picture whose answer arrived",
    )
    .await;

    // Long after, with no write to the record since, the host asks again.
    let _later = tilo_clock::testing::hold_wall_everywhere(
        process,
        started
            .checked_add(Duration::from_millis(
                neighbour_wire::content::WANT_TTL_MS + 1_000,
            ))
            .expect("a time past an ask's lifetime"),
    );
    let ask = asked_for(&mut bob_link, &lost).await;
    answer(&bob_link, &ask, &lost, &lost_bytes);
    says_it_lacks(
        &server,
        &workspace.workspace,
        0,
        "the host keeps the picture whose first ask went unanswered",
    )
    .await;

    drop(bob_link);
    server.shutdown().await;
}
