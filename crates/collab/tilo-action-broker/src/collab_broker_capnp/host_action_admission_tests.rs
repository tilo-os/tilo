use super::*;

#[path = "host_action_lifecycle_tests.rs"]
mod lifecycle;
use crate::collab_broker_capnp::{BrokerChannels, FanoutState, PeerPublisher};
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{InvokeArgument, RemoteActionTarget};

fn state(root: &std::path::Path) -> Arc<CapnpBrokerState> {
    let (state, _bridge) = CapnpBrokerState::new_configured(
        &Arc::new(FanoutState::new()),
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        None,
        None,
        None,
        crate::services::HostServices::default(),
    );
    *state.journal.lock().unwrap() = Ok(Some(
        invocation_journal::InvocationJournal::open(&root.join("journal")).unwrap(),
    ));
    state
}

fn descriptor() -> ActionDescriptor {
    ActionDescriptor {
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new("host-test", "read"),
        resource_kind: "host-test".into(),
        resource_label: "Host test".into(),
        action_label: "Read".into(),
        description: None,
        locus: tilo_actions::ExecutionLocus::default(),
        kind: tilo_actions::OperationKind::Effect,
        contract: tilo_actions::ActionContract::view(),
        input: tilo_actions::InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

fn request(descriptor: &ActionDescriptor) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: "host-test".into(),
            address: descriptor.address.clone(),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}

struct ClosedSeat;
impl crate::services::LocalPersonServices for ClosedSeat {
    fn certificate(&self, _: &str, _: &str) -> Result<tilo_user_identity::DeviceCert, String> {
        Err("native seat unavailable".into())
    }
}

#[test]
fn configured_native_seat_never_admits_an_unsigned_actor_label() {
    let (state, _) = CapnpBrokerState::new_configured(
        &Arc::new(FanoutState::new()),
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        None,
        None,
        None,
        crate::services::HostServices {
            local_person: Some(Arc::new(ClosedSeat)),
            ..Default::default()
        },
    );
    let descriptor = descriptor();
    let request = request(&descriptor);
    let caller = Caller::local_person("owner");
    assert!(
        state
            .admit_host_action(
                &caller,
                None,
                HostActionAdmission {
                    room: "room",
                    app_id: "host-test",
                    descriptor: &descriptor,
                    request: &request,
                }
            )
            .is_err()
    );
    assert!(state.completed_invocations.lock().unwrap().is_empty());
}

struct ClosedCertificates;
impl crate::peer_actions::CertificateAuthority for ClosedCertificates {
    fn authorize(&self, _: uuid::Uuid, _: &tilo_user_identity::DeviceCert) -> Result<(), String> {
        Err("certificate authority closed".into())
    }
}

#[test]
fn configured_certificates_without_a_native_seat_never_admit_unsigned_owners() {
    let (state, _) = CapnpBrokerState::new_configured(
        &Arc::new(FanoutState::new()),
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        None,
        None,
        None,
        crate::services::HostServices {
            native_certificates: Some(Arc::new(ClosedCertificates)),
            ..Default::default()
        },
    );
    assert!(state.services.local_person.is_none());
    let caller = Caller::local_person("owner");
    assert!(
        state
            .host_action_owner(&caller, None, "room", InvocationId::new())
            .is_err()
    );
    let descriptor = descriptor();
    let request = request(&descriptor);
    assert!(
        state
            .admit_host_action(
                &caller,
                None,
                HostActionAdmission {
                    room: "room",
                    app_id: "host-test",
                    descriptor: &descriptor,
                    request: &request,
                }
            )
            .is_err()
    );
    assert!(state.completed_invocations.lock().unwrap().is_empty());
}

#[test]
fn certificate_less_local_origin_is_compatible_only_without_identity_authority() {
    use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
    let root = [17; 32];
    let actor = principal_id_for_root(root);
    let mut workspace = Workspace::new(WorkspaceId(17), "Owner room");
    workspace.set_owner(actor.clone());
    let room = workspace.room.to_string();
    let route = crate::route::CrdtRoute::cluster(&room);
    let admitted = super::super::admit_verified_join_at(
        Some(&workspace),
        &route,
        super::super::VerifiedJoinIdentity {
            user_public_key: root,
            device_public_key: [18; 32],
            capped: false,
        },
        tilo_clock::unix_now(),
    )
    .unwrap();
    assert!(admitted.certificate.is_none());
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    let membership = tree.shared();
    let original = InvocationCaller::Local {
        route,
        admitted: Box::new(admitted),
        time: None,
    };
    let caller = Caller::local_person(actor).in_room(&room);
    for configured in [false, true] {
        let mut services = membership.services();
        if configured {
            services.native_certificates = Some(Arc::new(ClosedCertificates));
        }
        let broker = CapnpBrokerState::test_broker_with_authority(membership.authority(), services);
        let result =
            broker
                .state()
                .host_action_owner(&caller, Some(&original), &room, InvocationId::new());
        if configured {
            assert!(
                matches!(result, Err(ActionHostError::Unavailable { reason })
                if reason.contains("original certificate missing"))
            );
        } else {
            assert!(matches!(result, Ok(InvocationOwner::Local { .. })));
        }
    }
}

#[test]
fn host_uuid_and_terminal_use_the_same_durable_record() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path());
    let descriptor = descriptor();
    let request = request(&descriptor);
    let caller = Caller::local_person("owner");
    let admitted = state
        .admit_host_action(
            &caller,
            None,
            HostActionAdmission {
                room: "room",
                app_id: "host-test",
                descriptor: &descriptor,
                request: &request,
            },
        )
        .unwrap();
    assert_eq!(admitted.invocation, request.invocation);
    assert_ne!(
        admitted.invocation.0,
        uuid::Uuid::from_u128(u128::from(admitted.id))
    );
    let key = admitted.key.as_ref().unwrap();
    let record = state
        .journal
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .lookup(key)
        .unwrap()
        .unwrap();
    assert_eq!(
        record.update,
        InvocationUpdate::Accepted {
            invocation: request.invocation
        }
    );
    let terminal = InvocationUpdate::Succeeded {
        invocation: request.invocation,
        output: tilo_actions::ActionOutput::default(),
    };
    state
        .complete_host_action(&admitted, terminal.clone())
        .unwrap();
    let record = state
        .journal
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .lookup(key)
        .unwrap()
        .unwrap();
    assert_eq!(record.update, terminal);
    assert_eq!(
        state
            .completed_invocations
            .lock()
            .unwrap()
            .get(&admitted.id)
            .unwrap()
            .1,
        terminal
    );
}

#[test]
fn failed_admission_barrier_returns_no_dispatch_permit_or_accepted_update() {
    for failure in [
        invocation_journal::BarrierFailure::File,
        invocation_journal::BarrierFailure::Directory,
        invocation_journal::BarrierFailure::AfterAdmission,
    ] {
        let root = tempfile::tempdir().unwrap();
        let state = state(root.path());
        state
            .journal
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .as_mut()
            .unwrap()
            .fail_at = Some(failure);
        let descriptor = descriptor();
        let request = request(&descriptor);
        assert!(
            state
                .admit_host_action(
                    &Caller::local_person("owner"),
                    None,
                    HostActionAdmission {
                        room: "room",
                        app_id: "host-test",
                        descriptor: &descriptor,
                        request: &request,
                    }
                )
                .is_err()
        );
        assert!(state.completed_invocations.lock().unwrap().is_empty());
    }
}

#[test]
fn failed_terminal_barrier_does_not_publish_success() {
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
    state
        .journal
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .as_mut()
        .unwrap()
        .fail_at = Some(invocation_journal::BarrierFailure::File);
    assert!(
        state
            .complete_host_action(
                &admitted,
                InvocationUpdate::Succeeded {
                    invocation: request.invocation,
                    output: tilo_actions::ActionOutput::default(),
                }
            )
            .is_err()
    );
    assert_eq!(
        state
            .completed_invocations
            .lock()
            .unwrap()
            .get(&admitted.id)
            .unwrap()
            .1,
        InvocationUpdate::Accepted {
            invocation: request.invocation
        }
    );
}
