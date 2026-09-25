use super::*;
#[test]
fn agent_prelaunch_recovery_current_publication_keeps_exact_retained_grants() {
    // Negative/availability decision only: no live runtime/process authority fixture.
    let (dir, store, device) = crate::agent_owner_tests::enrolled();
    let caller = store.agent_caller(device).unwrap();
    let room: Uuid = store.state.room.parse().unwrap();
    let grant = GrantAction {
        app_id: "so.tilo.chat".into(),
        record_id: "@qos:package-primary".into(),
        resource: "Document".into(),
        action: "collapse".into(),
    };
    let input = AgentIntentV2 {
        scope: qos_peer_state::owner_commit::AgentInvocationScopeV2 {
            machine: store.state.machine_id.clone(),
            room: room.to_string(),
            host: room.to_string(),
            caller: caller.certificate().clone(),
        },
        route: qos_peer_state::owner_commit::AgentProviderRouteV2 {
            app_id: "so.tilo.agent".into(),
            app_peer: "8".into(),
            cluster_id: room.to_string(),
            record_id: "".into(),
            resource: format!("workspace:{room}:agents"),
            resource_kind: "workspace.agents".into(),
            action: "spawn".into(),
        },
        descriptor_revision: "a".repeat(64),
        request: String::from_utf8(
            qos_agent_control_protocol::canonical_request(&AgentRequest::Spawn {
                version: 1,
                name: "name".into(),
                task: "task".into(),
                grant_set_id: "expired-original-offer".into(),
            })
            .unwrap(),
        )
        .unwrap(),
        grants: vec![grant.clone()],
    };
    let host = store.peer().unwrap().authority().device_public_key;
    let mut publication = AgentProviderState {
        machine: store.state.machine_id.clone(),
        room,
        host: room,
        host_device: host,
        app_peer: 8,
        governance_revision: caller.governance_revision().into(),
        publication_revision: "new-live-publication".into(),
        grant_offers: vec![],
        private_v2_ready: true,
        required_services_ready: true,
    };
    let mut additional = grant.clone();
    additional.action = "expand".into();
    let current = vec![grant.clone(), additional];
    assert!(
        validate_retained_publication(
            &publication,
            &current,
            &input,
            caller.governance_revision(),
            host
        )
        .is_ok()
    );
    assert_eq!(input.grants, vec![grant.clone()]);
    assert!(
        validate_retained_publication(
            &publication,
            &current[1..],
            &input,
            caller.governance_revision(),
            host
        )
        .is_err()
    );
    publication.required_services_ready = false;
    assert!(
        validate_retained_publication(
            &publication,
            &current,
            &input,
            caller.governance_revision(),
            host
        )
        .is_err()
    );
    publication.required_services_ready = true;
    assert!(
        validate_retained_publication(&publication, &current, &input, "changed-governance", host)
            .is_err()
    );
    assert!(
        validate_retained_publication(
            &publication,
            &current,
            &input,
            caller.governance_revision(),
            [0; 32]
        )
        .is_err()
    );
    drop(store);
    drop(dir);
}
