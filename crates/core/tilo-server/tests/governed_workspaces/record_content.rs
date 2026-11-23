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

/// The host's next request for `address` of `PHOTOS`, as `session` hears it.
async fn asked_for(session: &ServerCoeditSession, address: &str) -> Vec<u8> {
    asked_for_each(session, &[address]).await.remove(0)
}

/// The host's next request for each of `addresses` of `PHOTOS`, in that order, as `session`
/// hears them in whatever order they come.
async fn asked_for_each(session: &ServerCoeditSession, addresses: &[&str]) -> Vec<Vec<u8>> {
    tilo_test_env::within("the host asks for the record's content", async {
        let mut asks: Vec<Option<Vec<u8>>> = vec![None; addresses.len()];
        while asks.iter().any(Option::is_none) {
            let Ok(Some(Inbound::Presence { payload, .. })) =
                tokio::time::timeout(Duration::from_millis(50), session.recv()).await
            else {
                continue;
            };
            if let Ok(NeighbourMsg::DocumentContentWant { hash, .. }) =
                postcard::from_bytes::<NeighbourMsg>(&payload)
                && let Some(at) = addresses.iter().position(|address| *address == hash)
            {
                asks[at].get_or_insert(payload);
            }
        }
        asks.into_iter().flatten().collect()
    })
    .await
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

/// What `server` answers `asking` when it asks how much of `workspace`'s content it keeps
/// (`workspace/servers#content`), or why it refused.
async fn content_held(
    server: &Server,
    host: &TestDevice,
    asking: &TestDevice,
    workspace: &WorkspaceListing,
) -> Result<serde_json::Value, tilo_actions::remote::ActionCallError> {
    use super::governance_actions::{ask, invocation, outcome, subscribed};
    let client = subscribed(server, asking, workspace.workspace_id).await;
    let message = invocation(
        tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER,
        format!("workspace.permissions:{}/servers", workspace.workspace),
        "content",
        serde_json::json!({}),
    );
    outcome(ask(&client, host, workspace.workspace_id, message).await)
}

/// What the host serves a device asking `workspace` for `address` of `PHOTOS`,
/// its pieces put together.
fn served(server: &Server, workspace: &str, asking: &TestDevice, address: &str) -> Vec<u8> {
    let want = postcard::to_allocvec(&NeighbourMsg::DocumentContentWant {
        requester: tilo_peer_state::peer_handle(&asking.device_pubkey()),
        app_id: PHOTOS.to_owned(),
        cluster_id: workspace.to_owned(),
        hash: address.to_owned(),
        have: Vec::new(),
    })
    .unwrap();
    let mut whole = Vec::new();
    for frame in
        server
            .workspaces()
            .unwrap()
            .content_answer(workspace, &asking.device_pubkey(), &want)
    {
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
    let mut peer = sealed_documents::join(
        &server,
        &laptop,
        &workspace.workspace,
        &bob,
        &phone,
        [7; 32],
    )
    .await
    .unwrap();
    let session = session(&server, &phone, workspace.workspace_id).await;
    crate::common::wait_until_subscribed(&server, workspace.workspace_id, &phone.device_pubkey())
        .await;

    // Bob's page keeps the picture sealed in its record's store and names the
    // reference: the host knows the ciphertext's address alone.
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
    let created = peer
        .publication_mut()
        .publish_document(
            PHOTOS,
            "",
            AutoCommit::new().save(),
            &phone.sk,
            SystemTime::now(),
        )
        .unwrap();
    publish(&session, &created);
    let mut doc = AutoCommit::new();
    doc.put(
        ROOT,
        "photo",
        format!(
            "blake3:{}:{}:{reference}",
            blake3::hash(&picture).to_hex(),
            picture.len()
        ),
    )
    .unwrap();
    doc.commit_edit("fixture", "photo", &phone.cert.user_public_key);
    let message = peer
        .publication_mut()
        .publish_incremental(
            PHOTOS,
            "",
            doc.save_after(&[]),
            &phone.sk,
            SystemTime::now(),
        )
        .unwrap();
    publish(&session, &message);

    // The host asks for the address the store keys the ciphertext under, and
    // Bob's page answers it as a page does.
    let ask = asked_for(&session, &address).await;
    match postcard::from_bytes::<NeighbourMsg>(&ask).unwrap() {
        NeighbourMsg::DocumentContentWant {
            app_id,
            cluster_id,
            hash,
            ..
        } => {
            assert_eq!(
                (app_id.as_str(), cluster_id.as_str(), hash.as_str()),
                (PHOTOS, workspace.workspace.as_str(), address.as_str()),
                "the host asks the record's app for the ciphertext's address"
            );
        }
        _ => panic!("a content request"),
    }
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
    let held = content_held(&server, &alice_home, &laptop, &workspace)
        .await
        .expect("the owner asks how much content the host keeps");
    assert_eq!(
        held["lacking"], 1,
        "the host answers its owner that it lacks the picture: {held}"
    );
    let pieces = neighbour_wire::content::answer(
        &phone.device_pubkey(),
        &alice_home.device_pubkey(),
        &ask,
        |app, cluster, wanted| {
            (app == PHOTOS && cluster == workspace.workspace && wanted == address)
                .then(|| sealed.clone())
        },
    );
    assert!(!pieces.is_empty(), "the page answers what it keeps");
    for piece in pieces {
        session
            .publish_record_content(alice_home.device_pubkey(), piece)
            .expect("the session carries every piece");
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
    let held = content_held(&server, &alice_home, &laptop, &workspace)
        .await
        .expect("the owner asks how much content the host keeps");
    assert_eq!(
        (&held["lacking"], &held["named"]),
        (&serde_json::json!(0), &serde_json::json!(1)),
        "the host answers its owner that it keeps all the content its records name: {held}"
    );

    // Bob's page is gone; a page that reloads reads the picture from the host,
    // as ciphertext, and opens it with the key its record carries.
    drop(session);
    // Bob, a member who does not choose the workspace's servers, is not answered.
    assert_eq!(
        content_held(&server, &alice_home, &phone, &workspace).await,
        Err(tilo_actions::remote::ActionCallError::UnknownAction),
        "only the workspace's owners ask how much of its content its server keeps"
    );
    let carol = TestUser::fresh("Carol");
    let carol_phone = carol.enrol("carol-phone", DeviceCapabilities::member());
    sealed_documents::join(
        &server,
        &laptop,
        &workspace.workspace,
        &carol,
        &carol_phone,
        [8; 32],
    )
    .await
    .unwrap();
    let delivered = served(&server, &workspace.workspace, &carol_phone, &address);
    assert_eq!(delivered, sealed, "the host serves the ciphertext alone");
    let reloaded = tilo_content::ContentStore::memory().sealing();
    reloaded.put_public(&delivered).unwrap();
    assert_eq!(reloaded.read(&reference).as_deref(), Some(&picture[..]));

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
    let mut peer = sealed_documents::join(
        &server,
        &laptop,
        &workspace.workspace,
        &bob,
        &phone,
        [7; 32],
    )
    .await
    .unwrap();
    let session = session(&server, &phone, workspace.workspace_id).await;
    crate::common::wait_until_subscribed(&server, workspace.workspace_id, &phone.device_pubkey())
        .await;

    // Bob's page keeps two pictures and names both in its record.
    let store = tilo_content::ContentStore::memory().sealing();
    let mut doc = AutoCommit::new();
    let mut pictures = Vec::new();
    for name in ["answered", "lost"] {
        let picture = format!("the picture whose ask is {name}").into_bytes();
        let reference = store.put(&picture).unwrap();
        let address = tilo_content::content_address(&reference).unwrap();
        doc.put(
            ROOT,
            name,
            format!(
                "blake3:{}:{}:{reference}",
                blake3::hash(&picture).to_hex(),
                picture.len()
            ),
        )
        .unwrap();
        pictures.push((address.clone(), store.read(&address).unwrap()));
    }
    doc.commit_edit("fixture", "pictures", &phone.cert.user_public_key);
    let created = peer
        .publication_mut()
        .publish_document(
            PHOTOS,
            "",
            AutoCommit::new().save(),
            &phone.sk,
            SystemTime::now(),
        )
        .unwrap();
    publish(&session, &created);
    let message = peer
        .publication_mut()
        .publish_incremental(
            PHOTOS,
            "",
            doc.save_after(&[]),
            &phone.sk,
            SystemTime::now(),
        )
        .unwrap();
    publish(&session, &message);
    let [(answered, answered_bytes), (lost, lost_bytes)] = <[_; 2]>::try_from(pictures).unwrap();

    let answer = |ask: &[u8], address: &str, bytes: &[u8]| {
        for piece in neighbour_wire::content::answer(
            &phone.device_pubkey(),
            &alice_home.device_pubkey(),
            ask,
            |app, cluster, wanted| {
                (app == PHOTOS && cluster == workspace.workspace && wanted == address)
                    .then(|| bytes.to_vec())
            },
        ) {
            session
                .publish_record_content(alice_home.device_pubkey(), piece)
                .expect("the session carries every piece");
        }
    };
    // The host asks for both; the answer to one of them never arrives.
    let asks = asked_for_each(&session, &[&answered, &lost]).await;
    answer(&asks[0], &answered, &answered_bytes);
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
    let ask = asked_for(&session, &lost).await;
    answer(&ask, &lost, &lost_bytes);
    says_it_lacks(
        &server,
        &workspace.workspace,
        0,
        "the host keeps the picture whose first ask went unanswered",
    )
    .await;

    server.shutdown().await;
}
