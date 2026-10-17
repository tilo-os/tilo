use super::*;
use crate::{RuntimeServices as _, standalone::Standalone};
use tilo_action_broker::collab_broker_capnp::BrokerActor;
use tilo_actions::remote::Caller;

#[test]
fn issued_owner_permits_recheck_execution_and_device_revocation() {
    let Some(profile) = crate::standalone::tests::isolated_root(
        "standalone::media_import::authority_tests::issued_owner_permits_recheck_execution_and_device_revocation",
    ) else {
        return;
    };
    let session = Standalone::start_reporting(&profile, &mut |_| {}).unwrap();
    let room = session.room();
    let runtime = session.media_host().handle().unwrap();
    let store = session.media_host().store();
    let media = DocumentMedia::new(store.clone());
    let hash = runtime.block_on(store.ingest(b"legacy".to_vec())).unwrap();
    let service = MediaImport::new(
        Arc::new(ProfileWorkspace::open(&profile).unwrap()),
        store,
        runtime.clone(),
    );
    let caller = Caller::local_person(session.actor())
        .in_room(room)
        .in_new_execution();
    let actor = BrokerActor::person(session.actor());
    let argument = format!(
        "{}{}",
        tilo_actions::ARGUMENTS_JSON_PREFIX,
        serde_json::json!({
            "source_store": SOURCE, "source_room": "unscoped", "destination_room": room,
            "hashes": [hash.to_hex()], "max_bytes": 100
        })
    );
    let confirmation = serde_json::json!({"confirm": true});
    let invocation = WorkspaceGovernanceRequest {
        local_device: true,
        execution_lease: caller.execution_lease(),
        room,
        actor: &actor,
        resource: RESOURCE,
        action: ACTION,
        argument: Some(&argument),
        confirmation: &confirmation,
        invocation: tilo_actions::InvocationId::new(),
    };
    let (host, permit) = service.prepare(&invocation).unwrap();
    caller.retire_execution();
    assert!(runtime.block_on(host.import(permit)).is_err());
    let live = Caller::local_person(session.actor())
        .in_room(room)
        .in_new_execution();
    let invocation = WorkspaceGovernanceRequest {
        execution_lease: live.execution_lease(),
        invocation: tilo_actions::InvocationId::new(),
        ..invocation
    };
    let (host, permit) = service.prepare(&invocation).unwrap();
    let selected = tilo_identity_host::registry::selected_identity_profile(room)
        .unwrap()
        .unwrap();
    let (_, cert) = tilo_identity_host::broker::trusted_current_device(room).unwrap();
    tilo_identity_host::device_fleet::DeviceFleetStore::from_env()
        .unwrap()
        .revoke(
            &selected.id,
            cert.device_public_key,
            tilo_user_identity::RevocationReason::Retired,
            tilo_clock::unix_now() * 1_000_000_000,
        )
        .unwrap();
    assert!(runtime.block_on(host.import(permit)).is_err());
    let scope = runtime_fs::blobs::BlobScope::new(room, "so.tilo.photos").unwrap();
    assert!(!runtime.block_on(media.contains(&scope, hash)));
    drop(session);
}

#[test]
fn owner_revision_ignores_task_and_effect_updates_but_not_ownership() {
    struct Membership(Workspace);
    impl tilo_identity_host::broker::WorkspaceMembership for Membership {
        fn with_workspace(&self, room: &RoomId, read: &mut dyn FnMut(&Workspace)) {
            if &self.0.room == room {
                read(&self.0);
            }
        }
    }
    let mut membership = Membership(Workspace::new(tilo_desktop_model::WorkspaceId(1), "Room"));
    membership.0.set_owner("owner");
    let room = membership.0.room.to_string();
    let revision = owner_revision(&membership, &room, "owner").unwrap();
    membership.0.tasks.push(tilo_desktop_model::WorkspaceTask {
        id: "task".into(),
        title: "Import unrelated notes".into(),
        created_by: "owner".into(),
        assigned_agent: None,
        route: tilo_desktop_model::WorkspaceTaskRoute::Manual,
        status: tilo_desktop_model::WorkspaceTaskStatus::Queued,
        resource: None,
        subject: None,
        created_at: 1,
        updated_at: 1,
        audit: Vec::new(),
    });
    membership
        .0
        .set_task_status_at(
            "task",
            tilo_desktop_model::WorkspaceTaskStatus::Done,
            "owner",
            2,
        )
        .unwrap();
    membership
        .0
        .effect_receipts
        .push(tilo_desktop_model::WorkspaceEffectReceipt::recorded(
            tilo_actions::InvocationId::new(),
            "owner",
            tilo_actions::ActionContext::default(),
            tilo_actions::EffectReceiptStatus::Succeeded,
            2,
        ));
    assert_eq!(
        owner_revision(&membership, &room, "owner").unwrap(),
        revision
    );
    membership.0.set_owner("new-owner");
    assert!(owner_revision(&membership, &room, "owner").is_err());
    assert_ne!(
        owner_revision(&membership, &room, "new-owner").unwrap(),
        revision
    );
}
