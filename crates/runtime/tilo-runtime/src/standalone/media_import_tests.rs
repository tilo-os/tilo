use super::*;
use runtime_fs::blobs::{BlobClient, ReadOutcome};
use std::collections::BTreeMap;
use tilo_actions::remote::{Confirmation, InvokeArgument, PendingPoll, RemoteActionTarget};

const TEST: &str = "standalone::media_import_tests::owner_import_through_the_canonical_door_grants_only_the_selected_room";

#[test]
fn retained_owner_facade_refuses_revoked_device_before_import() {
    let Some(profile) = super::tests::isolated_root(
        "standalone::media_import_tests::retained_owner_facade_refuses_revoked_device_before_import",
    ) else {
        return;
    };
    let session = Standalone::start_reporting(&profile, &mut |_| {}).unwrap();
    // The persistent OS-search door and CLI use this same runtime facade.
    // It retains certified custody without installing a native seat service.
    let host = session.action_host().unwrap();
    let owner = Caller::local_person(session.actor()).in_room(session.room());
    let runtime = session.media_host().handle().unwrap();
    let store = session.media_host().store();
    let first = runtime
        .block_on(store.ingest(b"approved".to_vec()))
        .unwrap();
    let untouched = runtime.block_on(store.ingest(b"revoked".to_vec())).unwrap();
    let accepted = request(session.room(), &first.to_hex());
    let result = finish(&host, &owner, &accepted).unwrap();
    assert!(matches!(
        result.update,
        tilo_actions::InvocationUpdate::Succeeded { .. }
    ));
    let selected = tilo_identity_host::registry::selected_identity_profile(session.room())
        .unwrap()
        .unwrap();
    let (_, certificate) =
        tilo_identity_host::broker::trusted_current_device(session.room()).unwrap();
    tilo_identity_host::device_fleet::DeviceFleetStore::from_env()
        .unwrap()
        .revoke(
            &selected.id,
            certificate.device_public_key,
            tilo_user_identity::RevocationReason::Retired,
            tilo_clock::unix_now() * 1_000_000_000,
        )
        .unwrap();
    for asked in [
        accepted.clone(),
        request(session.room(), &untouched.to_hex()),
    ] {
        assert!(matches!(
            host.invoke(&owner, asked).poll_ready(),
            PendingPoll::Ready(Err(_))
        ));
    }
    assert!(host.observe(&owner, &accepted.invocation).is_none());
    assert!(host.next_change(&owner, &accepted.invocation).is_none());
    assert!(host.cancel(&owner, &accepted.invocation).is_err());
    let client = BlobClient::connect_session(&profile.join(BLOB_SOCKET)).unwrap();
    let scope = client.scope(session.room(), "so.tilo.photos").unwrap();
    assert_eq!(
        scope.read_all(&untouched.to_hex()).unwrap(),
        ReadOutcome::Unavailable
    );
}

fn request(room: &str, hash: &str) -> InvokeRequest {
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_actions::remote::workspace_provider(room),
            address: tilo_actions::ActionAddress::new(media_import::RESOURCE, media_import::ACTION),
        },
        argument: InvokeArgument::Json(
            serde_json::from_value(serde_json::json!({
                "source_store": "runtime-profile-content", "source_room": "unscoped",
                "destination_room": room, "hashes": [hash], "max_bytes": 1024
            }))
            .unwrap(),
        ),
        context: None,
        confirmation: Some(Confirmation::Confirmed),
        revision: None,
        wait_ms: None,
        appearance: None,
        on_behalf_of: None,
    }
}

fn finish(
    host: &RuntimeActionHost,
    caller: &Caller,
    request: &InvokeRequest,
) -> Result<InvocationState, ActionHostError> {
    let runtime = tilo_rt::try_app().unwrap();
    let _entered = runtime.handle().enter();
    host.invoke(caller, request.clone())
        .wait_timeout(
            tilo_test_env::hang_deadline().saturating_duration_since(std::time::Instant::now()),
        )
        .unwrap_or_else(|_| panic!("workspace admission must finish"))
        .expect("workspace admission must not be abandoned")?;
    Ok(wait_for_terminal(host, caller, request.invocation))
}

fn wait_for_terminal(
    host: &RuntimeActionHost,
    caller: &Caller,
    invocation: tilo_actions::InvocationId,
) -> InvocationState {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let change = host
            .next_change(caller, &invocation)
            .expect("owned invocation");
        let state = host.observe(caller, &invocation).expect("owned invocation");
        if state.is_terminal() {
            return state;
        }
        assert!(
            change
                .wait_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .is_ok(),
            "owner action did not finish"
        );
    }
}

fn refused(host: &RuntimeActionHost, caller: &Caller, mut request: InvokeRequest) {
    request.invocation = tilo_actions::InvocationId::new();
    let result = finish(host, caller, &request);
    assert!(
        matches!(
            result,
            Err(_)
                | Ok(InvocationState {
                    update: tilo_actions::InvocationUpdate::Failed { .. },
                    ..
                })
        ),
        "{result:?}"
    );
}

fn refuse_scoped_imports(
    session: &Standalone,
    host: &RuntimeActionHost,
    owner: &Caller,
    asked: &InvokeRequest,
) {
    let room = session.room();
    refused(
        host,
        &Caller::local_agent("limited").in_room(room),
        asked.clone(),
    );
    refused(
        host,
        &Caller::local_app("so.tilo.photos").in_room(room),
        asked.clone(),
    );
    refused(
        host,
        &Caller::local_person("other-owner").in_room(room),
        asked.clone(),
    );
    refused(
        host,
        &owner.clone().running_shortcut("saved"),
        asked.clone(),
    );
    // Confirmation is not owner custody when an owner delegates the call or
    // attributes it to another invocation.
    refused(host, &owner.clone().delegated_to("limited"), asked.clone());
    refused(
        host,
        owner,
        InvokeRequest {
            on_behalf_of: Some(tilo_actions::InvocationId::new()),
            ..asked.clone()
        },
    );
    let selected = tilo_identity_host::registry::selected_identity_profile(room)
        .unwrap()
        .unwrap();
    refused(
        host,
        &Caller::authenticated(selected.user_public_key, [9; 32]).in_room(room),
        asked.clone(),
    );
    let mut spoofed = asked.clone();
    if let InvokeArgument::Json(fields) = &mut spoofed.argument {
        fields.insert("account".into(), serde_json::json!(session.actor()));
    }
    refused(host, owner, spoofed);
    refused(
        host,
        owner,
        InvokeRequest {
            confirmation: None,
            ..asked.clone()
        },
    );
    let mut outside = asked.clone();
    outside.invocation = tilo_actions::InvocationId::new();
    if let InvokeArgument::Json(fields) = &mut outside.argument {
        fields.insert("destination_room".into(), serde_json::json!("another-room"));
    }
    refused(host, owner, outside);
    let mut excess = asked.clone();
    excess.invocation = tilo_actions::InvocationId::new();
    if let InvokeArgument::Json(fields) = &mut excess.argument {
        fields.insert("max_bytes".into(), serde_json::json!(1));
    }
    refused(host, owner, excess);
}

#[test]
fn owner_import_through_the_canonical_door_grants_only_the_selected_room() {
    let Some(profile) = super::tests::isolated_root(TEST) else {
        return;
    };
    let session = Standalone::start_reporting(&profile, &mut |_| {}).unwrap();
    let host = session.action_host().unwrap();
    let room = session.room();
    let owner = Caller::local_person(session.actor()).in_room(room);
    // Historical raw content in the actual runtime profile store, without a
    // media room tag. No synthetic source-room root is manufactured.
    let store = session.media_host().store();
    let hash = session
        .media_host()
        .handle()
        .unwrap()
        .block_on(store.ingest(b"old profile content".to_vec()))
        .unwrap();
    let untouched = session
        .media_host()
        .handle()
        .unwrap()
        .block_on(store.ingest(b"not selected".to_vec()))
        .unwrap();
    let client = BlobClient::connect_session(&profile.join(BLOB_SOCKET)).unwrap();
    let own = client.scope(room, "so.tilo.photos").unwrap();
    let other = client.scope("another-room", "so.tilo.photos").unwrap();
    assert_eq!(
        own.read_all(&hash.to_hex()).unwrap(),
        ReadOutcome::Unavailable
    );
    let runtime = session.media_host().handle().unwrap();
    let route_content = tilo_relay::offload::WorkspaceContent::of_media(
        Arc::new(tilo_relay::offload::SessionMedia::new(
            profile.join(BLOB_SOCKET),
        )),
        runtime.clone(),
    );
    let own_scope = runtime_fs::blobs::BlobScope::new(room, "so.tilo.photos").unwrap();
    assert_eq!(
        runtime.block_on(route_content.bind_blob(&own_scope, *hash.as_bytes())),
        Err(tilo_relay::offload::BindError::NotLocal)
    );
    let asked = request(room, &hash.to_hex());
    refuse_scoped_imports(&session, &host, &owner, &asked);
    assert_eq!(
        own.read_all(&hash.to_hex()).unwrap(),
        ReadOutcome::Unavailable
    );
    let result = finish(&host, &owner, &asked).unwrap();
    assert!(
        matches!(
            result.update,
            tilo_actions::InvocationUpdate::Succeeded { .. }
        ),
        "{result:?}"
    );
    assert_eq!(
        own.read_all(&hash.to_hex()).unwrap(),
        ReadOutcome::Ready {
            bytes: b"old profile content".to_vec(),
            total_len: 19,
        }
    );
    assert_eq!(
        runtime.block_on(route_content.bind_blob(&own_scope, *hash.as_bytes())),
        Ok(19)
    );
    let other_scope = runtime_fs::blobs::BlobScope::new("another-room", "so.tilo.photos").unwrap();
    assert_eq!(
        runtime.block_on(route_content.bind_blob(&other_scope, *hash.as_bytes())),
        Err(tilo_relay::offload::BindError::NotLocal)
    );
    assert_eq!(
        other.read_all(&hash.to_hex()).unwrap(),
        ReadOutcome::Unavailable
    );
    assert_eq!(
        own.read_all(&untouched.to_hex()).unwrap(),
        ReadOutcome::Unavailable
    );
    assert!(
        session
            .media_host()
            .handle()
            .unwrap()
            .block_on(store.has(untouched))
    );
    // Retry uses the canonical invocation journal; it does not mint another import.
    assert!(matches!(
        host.invoke(&owner, asked).poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    let cli_request = serde_json::to_string(&request(room, &untouched.to_hex())).unwrap();
    // An active daemon owns the profile. The owner CLI must never start a
    // second server or bypass that custody.
    assert!(crate::cli::owner_action::<Standalone>(&profile, &cli_request).is_err());
    drop(host);
    drop(route_content);
    drop(runtime);
    drop(own);
    drop(other);
    drop(client);
    drop(store);
    drop(session);
    check_owner_cli(&profile, &cli_request, &untouched.to_hex());
}

// Opening the journal may compact its append log. Compare durable records,
// not bytes, so compaction cannot hide a second admission or transition.
fn journal_records(path: &Path) -> BTreeMap<String, serde_json::Value> {
    let mut records = BTreeMap::new();
    for line in std::fs::read_to_string(path).unwrap().lines() {
        let entry: serde_json::Value = serde_json::from_str(line).unwrap();
        let rows = entry.get("records").map_or_else(
            || vec![entry.get("record").expect("journal entry record").clone()],
            |rows| rows.as_array().expect("journal snapshot records").clone(),
        );
        for record in rows {
            let key =
                serde_json::to_string(record.get("key").expect("journal record key")).unwrap();
            records.insert(key, record);
        }
    }
    assert!(
        !records.is_empty(),
        "the original admission must be durable"
    );
    records
}

fn check_owner_cli(profile: &Path, cli_request: &str, hash: &str) {
    let receipt = crate::cli::owner_action::<Standalone>(profile, cli_request).unwrap();
    assert!(matches!(
        receipt.status,
        tilo_actions::remote::ReceiptStatus::Completed { .. }
    ));
    let journal = profile
        .join(workspace::INVOCATION_JOURNAL)
        .join("records.json");
    let admitted = journal_records(&journal);
    let replay = crate::cli::owner_action::<Standalone>(profile, cli_request).unwrap();
    assert_eq!(
        replay, receipt,
        "certified restart replay keeps the original outcome"
    );
    assert_eq!(
        journal_records(&journal),
        admitted,
        "a spent original owner invocation must not be admitted or run again after restart"
    );
    let reopened = Standalone::start_reporting(profile, &mut |_| {}).unwrap();
    let client = BlobClient::connect_session(&profile.join(BLOB_SOCKET)).unwrap();
    let own = client.scope(reopened.room(), "so.tilo.photos").unwrap();
    assert!(matches!(
        own.read_all(hash).unwrap(),
        ReadOutcome::Ready { .. }
    ));
    drop(reopened);
}
