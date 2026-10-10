//! Room actions on sealed records of a room the host itself governs.
//!
//! A node (a host certified to own workspaces) governs the room and stewards
//! its channels. Its room actions join as the same device, so they are
//! handed the steward's keys through the relay's admission decision rather
//! than admitted to each channel a second time. The host owner's page acts
//! on a sealed record through the host; another member of the room sees the
//! change arrive sealed and authored by the host; that member is refused
//! when it asks the host to act; and a new record the host opens goes out
//! sealed on a channel the members can join.

use std::time::{Duration, Instant};

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionHostError, CatalogueQuery, InvokeArgument, InvokeRequest, ReceiptStatus,
    application_open_target,
};
use tilo_peer_state::join::MemberRole;

use super::resource_support::{admit_person, open_admitted_peer};
use super::{
    Browser, DeviceCapabilities, DocumentRoute, SHEETS, enrol, record_provider, request,
    sheet_named, sheet_names, start,
};

/// The retained bodies `author` wrote to `route`, and whether each is sealed
/// on the route's channel.
async fn retained_by(
    hosted: &super::Hosted,
    workspace: u64,
    route: &DocumentRoute,
    author: [u8; 32],
) -> Vec<bool> {
    let channel =
        collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record);
    hosted
        .relay
        .room_records()
        .retained(workspace)
        .await
        .unwrap()
        .into_iter()
        .filter(|body| body.author == author)
        .filter_map(|body| {
            match postcard::from_bytes::<tilo_peer_state::NeighbourMsg>(&body.body) {
                Ok(tilo_peer_state::NeighbourMsg::AppDocument {
                    app_id,
                    record_id,
                    envelope,
                    ..
                }) if app_id == route.app_id && record_id == route.record => Some(
                    envelope
                        .encryption
                        .as_ref()
                        .is_some_and(|metadata| metadata.channel == channel),
                ),
                _ => None,
            }
        })
        .collect()
}

/// Wait until the relay retains at least `count` bodies `author` wrote to
/// `route`, then return whether each is sealed on the route's channel.
async fn until_retained(
    hosted: &super::Hosted,
    workspace: u64,
    route: &DocumentRoute,
    author: [u8; 32],
    count: usize,
) -> Vec<bool> {
    let deadline = Instant::now().checked_add(Duration::from_secs(30)).unwrap();
    loop {
        let sealed = retained_by(hosted, workspace, route, author).await;
        if sealed.len() >= count {
            return sealed;
        }
        assert!(
            Instant::now() < deadline,
            "the relay never retained {count} change(s) to {} by {}: {sealed:?}",
            route.record,
            hex::encode(author)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wait until `browser` holds a change `author` made to `route`.
async fn until_authored(browser: &mut Browser, route: &DocumentRoute, author: [u8; 32]) {
    let deadline = Instant::now().checked_add(Duration::from_secs(30)).unwrap();
    loop {
        browser.pump();
        if browser
            .documents
            .get(route)
            .is_some_and(|changes| changes.iter().any(|(by, _)| *by == author))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the member never opened the host's change to {}: {:?}",
            route.record,
            browser.peer.status().channels
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_governing_host_acts_on_sealed_records_for_its_owner_and_no_one_else() {
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (other, _) = tilo_user_identity::UserIdentity::generate("Bob", 1);
    let host = enrol(
        &owner,
        "node",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let page_device = enrol(&owner, "page", DeviceCapabilities::member());
    let member_device = enrol(&other, "member", DeviceCapabilities::member());
    let host_key = host.cert.device_public_key;
    let state = tempfile::tempdir().unwrap();
    let hosted = start(&owner, &host, state.path()).await;
    let room = hosted
        .relay
        .rooms()
        .unwrap()
        .create("Sealed records, governed here")
        .await
        .unwrap();
    admit_person(
        &hosted,
        &host,
        &room.room,
        &member_device,
        MemberRole::Editor,
    )
    .await;
    let mut page = Browser::open_room(&hosted, &page_device, &host, &room.room).await;
    let mut member = open_admitted_peer(&hosted, &member_device, &host, &room.room).await;

    // The owner's page writes a Sheets record. It is sealed on its channel,
    // which this host stewards.
    let route = page.create(SHEETS);
    page.peer.barrier().unwrap().recv().unwrap().unwrap();
    let written = until_retained(
        &hosted,
        room.workspace_id,
        &route,
        page_device.cert.device_public_key,
        1,
    )
    .await;
    assert!(
        written.iter().all(|sealed| *sealed),
        "the record is sealed on its own channel: {written:?}"
    );
    let provider = record_provider(&route);
    let new_sheet = page
        .catalogue(CatalogueQuery {
            provider: Some(provider.clone()),
            max_results: 200,
            ..CatalogueQuery::default()
        })
        .await
        .unwrap()
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .find(|action| action.address.action == "new-sheet")
        .expect("the host opened the sealed record and lists its actions")
        .address
        .clone();

    // The owner acts through the host on the sealed record.
    let started = Instant::now();
    let acted = page
        .invoke(request(
            &provider,
            new_sheet.clone(),
            sheet_named("Through the host"),
        ))
        .await;
    let took = started.elapsed();
    eprintln!("room actions on a sealed record of a governed room: invoke {took:?}");
    assert!(
        matches!(acted.status, ReceiptStatus::Completed { .. }),
        "{acted:?}"
    );
    assert!(
        took < Duration::from_secs(5),
        "the host waited for keys it already stewards: {took:?}"
    );
    let by_host = until_retained(&hosted, room.workspace_id, &route, host_key, 1).await;
    assert!(
        by_host.iter().all(|sealed| *sealed),
        "the host's change is delivered, sealed on the record's channel: {by_host:?}"
    );

    // Another member of the room opens it and sees the sheet.
    until_authored(&mut member, &route, host_key).await;
    let seen = member.mount(&route);
    assert!(
        sheet_names(&seen).contains(&"Through the host".to_owned()),
        "{:?}",
        sheet_names(&seen)
    );

    // That member, an editor of the room, is no person the host's room
    // actions serve: refused before anything is queued, so nothing runs.
    let refused = member
        .invoke(request(
            &provider,
            new_sheet,
            sheet_named("Not through the host"),
        ))
        .await;
    assert_eq!(
        refused.status,
        ReceiptStatus::Rejected {
            error: ActionHostError::UnknownAction
        }
    );
    // The owner's next invocation runs on the service's one thread: once it
    // is answered, anything queued before it has run.
    let opened = page
        .invoke(InvokeRequest {
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: application_open_target(&room.room, SHEETS),
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
        })
        .await;
    let ReceiptStatus::Completed { output } = &opened.status else {
        panic!("the owner opens a new Sheets record through the host: {opened:?}");
    };
    assert_eq!(
        retained_by(&hosted, room.workspace_id, &route, host_key)
            .await
            .len(),
        by_host.len(),
        "the refused invocation wrote nothing"
    );
    let seen = member.mount(&route);
    assert!(
        !sheet_names(&seen).contains(&"Not through the host".to_owned()),
        "{:?}",
        sheet_names(&seen)
    );

    // The new record's first change goes out sealed on a channel the host
    // opened for it, and a member joins that channel and reads it.
    let record = output.value.as_ref().unwrap()["record"]
        .as_str()
        .unwrap()
        .to_owned();
    let created = DocumentRoute {
        app_id: SHEETS.into(),
        cluster_id: room.room.clone(),
        record,
    };
    let first = until_retained(&hosted, room.workspace_id, &created, host_key, 1).await;
    assert!(
        first.iter().all(|sealed| *sealed),
        "a record the host creates is delivered sealed, not held for keys: {first:?}"
    );
    until_authored(&mut member, &created, host_key).await;

    page.peer.close();
    member.peer.close();
    hosted.agents.shutdown();
    drop(hosted.web);
    hosted.relay.shutdown().await;
}
