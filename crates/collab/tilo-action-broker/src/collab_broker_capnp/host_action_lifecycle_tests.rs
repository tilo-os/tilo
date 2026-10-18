use super::*;

#[test]
fn retired_host_rejects_late_completion_without_rewriting_observation() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path());
    let descriptor = descriptor();
    let request = request(&descriptor);
    let admitted = state
        .admit_host_action(
            &Caller::local_person("owner"),
            None,
            HostActionAdmission {
                room: "room",
                app_id: "host-test",
                descriptor: &descriptor,
                request: &request,
            },
        )
        .unwrap();
    state.stop();
    assert!(
        state
            .complete_host_action(
                &admitted,
                InvocationUpdate::Succeeded {
                    invocation: request.invocation,
                    output: Default::default(),
                }
            )
            .is_err()
    );
    assert!(matches!(
        state.invocation_status(admitted.id),
        Some(InvocationUpdate::Accepted { .. })
    ));
    let journal =
        invocation_journal::InvocationJournal::open(&root.path().join("journal")).unwrap();
    let record = journal
        .lookup(admitted.key.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert!(record.uncertain);
    assert!(record.update.is_terminal());
}

#[test]
fn host_terminal_is_single_assignment_and_never_cancellable() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path());
    let descriptor = descriptor();
    let request = request(&descriptor);
    let admitted = state
        .admit_host_action(
            &Caller::local_person("owner"),
            None,
            HostActionAdmission {
                room: "room",
                app_id: "host-test",
                descriptor: &descriptor,
                request: &request,
            },
        )
        .unwrap();
    assert!(
        state
            .request_cancellation_for_owner(admitted.id, &admitted.owner)
            .is_err()
    );
    assert!(!state.can_cancel_for_owner(admitted.id, &admitted.owner));
    let terminal = InvocationUpdate::Succeeded {
        invocation: request.invocation,
        output: Default::default(),
    };
    state
        .complete_host_action(&admitted, terminal.clone())
        .unwrap();
    assert!(
        state
            .complete_host_action(&admitted, terminal.clone())
            .is_err()
    );
    assert_eq!(state.invocation_status(admitted.id), Some(terminal));
}

#[test]
fn recovered_host_replay_uses_exact_owner_and_request_without_dispatch() {
    let fixture = crate::collab_broker_capnp::host_steps::signed_test_support::Fixture::new(
        Default::default(),
    );
    let descriptor = descriptor();
    let request = request(&descriptor);
    let admitted = fixture
        .state
        .admit_host_action(
            &fixture.caller,
            Some(&fixture.origin),
            HostActionAdmission {
                room: &fixture.room,
                app_id: "host-test",
                descriptor: &descriptor,
                request: &request,
            },
        )
        .unwrap();
    let record = fixture.record(&request);
    assert!(fixture.state.cancel_durable_record(&record).is_err());
    assert!(!fixture.record(&request).cancellation_requested);
    fixture.reopen_journal();
    let replay = fixture
        .state
        .host_action_replay(&fixture.caller, &fixture.origin, &request)
        .unwrap();
    assert!(replay.is_some());
    assert!(fixture.record(&request).uncertain);
    assert!(
        fixture
            .state
            .complete_host_action(
                &admitted,
                InvocationUpdate::Succeeded {
                    invocation: request.invocation,
                    output: Default::default(),
                }
            )
            .is_err(),
        "a recovered unknown observation is not a live completion permit"
    );
    assert_eq!(
        fixture
            .state
            .remote_job(&fixture.caller, &request.invocation),
        Some(admitted.id)
    );
    let mut changed = request.clone();
    changed.argument = tilo_actions::remote::InvokeArgument::Text("changed".into());
    assert!(
        fixture
            .state
            .host_action_replay(&fixture.caller, &fixture.origin, &changed)
            .is_err()
    );
    fixture
        .authority
        .revoked
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        fixture
            .state
            .host_action_replay(&fixture.caller, &fixture.origin, &request)
            .is_err()
    );
}

#[test]
fn app_local_origin_replays_host_terminal_before_live_target_resolution() {
    let fixture = crate::collab_broker_capnp::host_steps::signed_test_support::Fixture::new(
        Default::default(),
    );
    let descriptor = descriptor();
    let mut request = request(&descriptor);
    request.revision = Some("selected-revision".into());
    let admitted = fixture
        .state
        .admit_host_action(
            &fixture.caller,
            Some(&fixture.origin),
            HostActionAdmission {
                room: &fixture.room,
                app_id: "host-test",
                descriptor: &descriptor,
                request: &request,
            },
        )
        .unwrap();
    let terminal = InvocationUpdate::Succeeded {
        invocation: request.invocation,
        output: Default::default(),
    };
    fixture
        .state
        .complete_host_action(&admitted, terminal.clone())
        .unwrap();
    let signed = fixture.assert_signed(&request, true);
    assert_eq!(
        signed
            .admission
            .as_ref()
            .unwrap()
            .public_receipt()
            .facts
            .revision,
        None,
        "a caller-selected revision is not a host-resolved revision"
    );
    assert_eq!(
        signed.intent["host_descriptor_digest"],
        hex::encode(invocation_journal::admission_request_digest(
            &serde_json::to_value(&descriptor).unwrap()
        ))
    );
    fixture.reopen_journal();
    // No host-test service or app exists. Only the retained observation can
    // answer this request; resolving or dispatching it again would fail.
    fixture.invoke(&request).unwrap();
    assert_eq!(fixture.state.invocation_status(admitted.id), Some(terminal));
    assert!(!fixture.record(&request).uncertain);

    let mut changed = request.clone();
    changed.revision = Some("different-revision".into());
    assert!(matches!(
        fixture.invoke(&changed),
        Err(ActionHostError::Refused { .. })
    ));
    fixture.state.stop();
    assert!(fixture.invoke(&request).is_err());
}
