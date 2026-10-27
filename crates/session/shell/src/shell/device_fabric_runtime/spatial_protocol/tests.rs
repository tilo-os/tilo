use super::*;
use crate::shell::device_fabric::PersonalFabricActionEnvelope;

fn lease() -> InputLease {
    InputLease {
        controller: DeviceId::new("controller").unwrap(),
        target: DeviceId::new("target").unwrap(),
        issued_unix_ms: 100,
        expires_unix_ms: 200,
        nonce: [5; 16],
        allow_pointer: true,
        allow_keyboard: true,
    }
}
fn receiver() -> (SpatialInputState, AuthenticatedPersonalRoute, InputLease) {
    let lease = lease();
    let route = AuthenticatedPersonalRoute::for_test_epoch(lease.controller.clone(), 7);
    let mut state = SpatialInputState::default();
    state.issued.insert(lease.nonce, lease.clone());
    state.issued_routes.insert(lease.nonce, route.clone());
    (state, route, lease)
}
fn sender() -> (SpatialInputState, AuthenticatedPersonalRoute, InputLease) {
    let lease = lease();
    let route = AuthenticatedPersonalRoute::for_test_epoch(lease.target.clone(), 7);
    let mut state = SpatialInputState::default();
    state.receive_spatial_grant(lease.target.clone(), route.clone(), lease.clone());
    (state, route, lease)
}
fn motion(x: i32) -> FabricInputEvent {
    FabricInputEvent::PointerMotion { x, y: 0 }
}

#[test]
fn overtaken_pointer_positions_are_dropped_but_buttons_and_keys_never_are() {
    let (mut state, route, lease) = receiver();
    assert!(
        state
            .negotiate_spatial_protocol(&route, &lease.target, (lease.nonce, false), 150)
            .is_some()
    );
    let button = FabricInputEvent::PointerButton {
        button: 0x110,
        pressed: true,
    };
    let key = FabricInputEvent::Key {
        keycode: 30,
        pressed: false,
        modifiers: 0,
    };
    for (sequence, event) in [
        (2, motion(10)),
        (1, motion(5)),
        (1, button),
        (1, key),
        (3, motion(12)),
    ] {
        state.accept_sequenced_input(&route, &lease.target, (lease.nonce, sequence, event), 150);
    }
    let applied: Vec<_> = state.inbound.drain(..).map(|(_, event)| event).collect();
    assert_eq!(applied, [motion(10), button, key, motion(12)]);
    // Old packets can still be in flight when the protocol ack arrives. An
    // unsequenced motion cannot move back after a sequenced sample; its key
    // and button edges remain deliverable.
    state.accept_lease_bound_input(&route, &lease.target, (lease.nonce, motion(4)), 150);
    state.accept_lease_bound_input(&route, &lease.target, (lease.nonce, button), 150);
    assert_eq!(
        state
            .inbound
            .into_iter()
            .map(|(_, event)| event)
            .collect::<Vec<_>>(),
        [button]
    );
}

#[test]
fn sequenced_input_requires_negotiation_exact_route_nonce_capability_and_expiry() {
    let (mut state, route, lease) = receiver();
    state.accept_sequenced_input(&route, &lease.target, (lease.nonce, 1, motion(1)), 150);
    assert!(state.inbound.is_empty());
    let replaced_route = AuthenticatedPersonalRoute::for_test_epoch(lease.controller.clone(), 8);
    assert!(
        state
            .negotiate_spatial_protocol(&replaced_route, &lease.target, (lease.nonce, false), 150)
            .is_none()
    );
    assert!(
        state
            .negotiate_spatial_protocol(&route, &lease.target, ([9; 16], false), 150)
            .is_none()
    );
    assert!(
        state
            .negotiate_spatial_protocol(&route, &lease.target, (lease.nonce, false), 201)
            .is_none()
    );
    state
        .negotiate_spatial_protocol(&route, &lease.target, (lease.nonce, false), 150)
        .unwrap();
    for (candidate, target, nonce, sequence, now) in [
        (&replaced_route, &lease.target, lease.nonce, 1, 150),
        (&route, &lease.controller, lease.nonce, 1, 150),
        (&route, &lease.target, [9; 16], 1, 150),
        (&route, &lease.target, lease.nonce, 0, 150),
        (&route, &lease.target, lease.nonce, 1, 201),
    ] {
        state.accept_sequenced_input(candidate, target, (nonce, sequence, motion(1)), now);
        assert!(state.inbound.is_empty());
    }
    state.issued.get_mut(&lease.nonce).unwrap().allow_keyboard = false;
    state.accept_sequenced_input(
        &route,
        &lease.target,
        (
            lease.nonce,
            1,
            FabricInputEvent::Key {
                keycode: 30,
                pressed: true,
                modifiers: 0,
            },
        ),
        150,
    );
    assert!(state.inbound.is_empty());
    state.accept_sequenced_input(&route, &lease.target, (lease.nonce, 1, motion(2)), 150);
    assert_eq!(
        state.inbound.pop_front(),
        Some((lease.controller, motion(2)))
    );
}

#[test]
fn sending_falls_back_to_legacy_and_retries_keep_the_same_sequence() {
    let (mut state, route, lease) = sender();
    let input = |event| OutboundSpatialInput {
        target: lease.target.clone(),
        lease_nonce: lease.nonce,
        event,
        sequence: None,
    };
    let mut legacy = input(motion(1));
    assert!(matches!(
        state.prepare_spatial_input(&mut legacy, 150).unwrap(),
        PersonalFabricAction::Input { .. }
    ));
    let wrong = AuthenticatedPersonalRoute::for_test_epoch(lease.target.clone(), 8);
    state.negotiate_spatial_protocol(&wrong, &lease.controller, (lease.nonce, true), 150);
    assert!(state.protocol.send.is_empty());
    state.negotiate_spatial_protocol(&route, &lease.controller, (lease.nonce, true), 150);
    let mut first = input(motion(2));
    let prepared = state.prepare_spatial_input(&mut first, 150).unwrap();
    assert!(matches!(
        prepared,
        PersonalFabricAction::SpatialInputSequenced { sequence: 1, .. }
    ));
    assert_eq!(
        state.prepare_spatial_input(&mut first, 150).unwrap(),
        prepared
    );
    state.negotiate_spatial_protocol(&route, &lease.controller, (lease.nonce, true), 150);
    state.receive_spatial_grant(lease.target.clone(), route.clone(), lease.clone());
    let mut second = input(FabricInputEvent::PointerButton {
        button: 0x110,
        pressed: true,
    });
    assert!(matches!(
        state.prepare_spatial_input(&mut second, 150).unwrap(),
        PersonalFabricAction::SpatialInputSequenced { sequence: 2, .. }
    ));
    assert!(state.prepare_spatial_input(&mut second, 201).is_err());
}

#[test]
fn replacement_and_departure_remove_protocol_authority_and_sequence_exhaustion_fails_closed() {
    let (mut state, route, lease) = sender();
    state.negotiate_spatial_protocol(&route, &lease.controller, (lease.nonce, true), 150);
    state
        .protocol
        .send
        .insert(lease.target.clone(), (lease.nonce, u64::MAX));
    let mut input = OutboundSpatialInput {
        target: lease.target.clone(),
        lease_nonce: lease.nonce,
        event: motion(1),
        sequence: None,
    };
    assert!(state.prepare_spatial_input(&mut input, 150).is_err());
    assert!(input.sequence.is_none());
    let replacement = InputLease {
        nonce: [6; 16],
        ..lease.clone()
    };
    state.receive_spatial_grant(lease.target.clone(), route.clone(), replacement.clone());
    assert!(state.protocol.send.is_empty());
    state.negotiate_spatial_protocol(&route, &lease.controller, (lease.nonce, true), 150);
    assert!(state.protocol.send.is_empty());
    state.negotiate_spatial_protocol(&route, &lease.controller, (replacement.nonce, true), 150);
    assert_eq!(
        state.protocol.send.get(&replacement.target),
        Some(&(replacement.nonce, 0))
    );
    super::super::prune_spatial_state(&mut state, 150, &[], |_| false);
    assert!(state.protocol.send.is_empty());
    assert!(state.protocol.probes.is_empty());
    let (mut target, target_route, lease) = receiver();
    target.negotiate_spatial_protocol(&target_route, &lease.target, (lease.nonce, false), 150);
    super::super::prune_spatial_state(&mut target, 201, &[], |_| true);
    assert!(target.protocol.receive.is_empty());
}

#[test]
fn appended_actions_preserve_legacy_input_bytes_and_validate_the_new_payloads() {
    let lease = lease();
    let legacy = PersonalFabricAction::Input {
        lease_nonce: [1; 16],
        event: motion(0),
    };
    let mut expected = vec![5];
    expected.extend([1; 16]);
    expected.extend([0, 0, 0]);
    assert_eq!(postcard::to_allocvec(&legacy).unwrap(), expected);
    for action in [
        PersonalFabricAction::SpatialInputProtocol {
            lease_nonce: lease.nonce,
            acknowledgement: false,
        },
        PersonalFabricAction::SpatialInputSequenced {
            lease_nonce: lease.nonce,
            sequence: 1,
            event: motion(1),
        },
    ] {
        let bytes = postcard::to_allocvec(&action).unwrap();
        assert!(bytes[0] > 5);
        assert_eq!(
            postcard::from_bytes::<PersonalFabricAction>(&bytes).unwrap(),
            action
        );
        let envelope = PersonalFabricActionEnvelope {
            request_id: [1; 16],
            source: lease.controller.clone(),
            target: lease.target.clone(),
            issued_unix_ms: 150,
            action,
        };
        assert!(
            envelope
                .validate_at(&lease.controller, &lease.target, 150)
                .is_ok()
        );
    }
    for action in [
        PersonalFabricAction::SpatialInputProtocol {
            lease_nonce: [0; 16],
            acknowledgement: true,
        },
        PersonalFabricAction::SpatialInputSequenced {
            lease_nonce: lease.nonce,
            sequence: 0,
            event: motion(1),
        },
    ] {
        let envelope = PersonalFabricActionEnvelope {
            request_id: [1; 16],
            source: lease.controller.clone(),
            target: lease.target.clone(),
            issued_unix_ms: 150,
            action,
        };
        assert!(
            envelope
                .validate_at(&lease.controller, &lease.target, 150)
                .is_err()
        );
    }
}

#[test]
fn negotiation_keeps_the_existing_privacy_gate_and_motion_coalescing_boundary() {
    use crate::shell::continuity_policy::ContinuityPolicySnapshot;
    let (mut state, route, lease) = sender();
    let action = PersonalFabricAction::SpatialInputProtocol {
        lease_nonce: lease.nonce,
        acknowledgement: false,
    };
    assert!(!ContinuityPolicySnapshot::OFF.permits_action(&action));
    let document = serde_json::json!({"continuity":{"enabled":true,"display_input":true}});
    assert!(
        ContinuityPolicySnapshot::from_document(document.as_object().unwrap())
            .permits_action(&action)
    );
    state.negotiate_spatial_protocol(&route, &lease.controller, (lease.nonce, true), 150);
    let button = FabricInputEvent::PointerButton {
        button: 0x110,
        pressed: true,
    };
    for event in [motion(1), motion(2), button, motion(3)] {
        state
            .queue_lease_bound_input(
                &lease.controller,
                OutboundSpatialInput {
                    sequence: None,
                    target: lease.target.clone(),
                    lease_nonce: lease.nonce,
                    event,
                },
                150,
            )
            .unwrap();
    }
    assert_eq!(state.outbound.len(), 3);
    for (expected, event) in [(1, motion(2)), (2, button), (3, motion(3))] {
        let mut input = state.outbound.pop_front().unwrap();
        assert_eq!(
            state.prepare_spatial_input(&mut input, 150).unwrap(),
            PersonalFabricAction::SpatialInputSequenced {
                lease_nonce: lease.nonce,
                sequence: expected,
                event
            }
        );
    }
}

#[test]
fn a_second_targets_equal_nonce_cannot_negotiate_or_reset_another_targets_stream() {
    let (mut state, route, lease) = sender();
    state.negotiate_spatial_protocol(&route, &lease.controller, (lease.nonce, true), 150);
    let make = |target| OutboundSpatialInput {
        target,
        lease_nonce: lease.nonce,
        event: motion(1),
        sequence: None,
    };
    let mut first = make(lease.target.clone());
    assert!(matches!(
        state.prepare_spatial_input(&mut first, 150).unwrap(),
        PersonalFabricAction::SpatialInputSequenced { sequence: 1, .. }
    ));
    let other = DeviceId::new("other-target").unwrap();
    let other_route = AuthenticatedPersonalRoute::for_test_epoch(other.clone(), 7);
    let other_lease = InputLease {
        target: other.clone(),
        ..lease.clone()
    };
    state.receive_spatial_grant(other.clone(), other_route.clone(), other_lease);
    let mut second = make(other);
    assert!(
        matches!(
            state.prepare_spatial_input(&mut second, 150).unwrap(),
            PersonalFabricAction::Input { .. }
        ),
        "equal nonce does not supply another target's capability negotiation"
    );
    state.negotiate_spatial_protocol(&other_route, &lease.controller, (lease.nonce, true), 150);
    assert!(matches!(
        state.prepare_spatial_input(&mut second, 150).unwrap(),
        PersonalFabricAction::SpatialInputSequenced { sequence: 1, .. }
    ));
    let mut again = make(lease.target.clone());
    assert!(matches!(
        state.prepare_spatial_input(&mut again, 150).unwrap(),
        PersonalFabricAction::SpatialInputSequenced { sequence: 2, .. }
    ));
}

#[test]
fn a_replacement_route_must_negotiate_again_even_when_the_nonce_is_unchanged() {
    let (mut state, route, lease) = sender();
    state.negotiate_spatial_protocol(&route, &lease.controller, (lease.nonce, true), 150);
    let replacement = AuthenticatedPersonalRoute::for_test_epoch(lease.target.clone(), 8);
    state.receive_spatial_grant(lease.target.clone(), replacement.clone(), lease.clone());
    let mut input = OutboundSpatialInput {
        target: lease.target.clone(),
        lease_nonce: lease.nonce,
        event: motion(1),
        sequence: None,
    };
    assert!(matches!(
        state.prepare_spatial_input(&mut input, 150).unwrap(),
        PersonalFabricAction::Input { .. }
    ));
    state.negotiate_spatial_protocol(&route, &lease.controller, (lease.nonce, true), 150);
    assert!(state.protocol.send.is_empty());
    state.negotiate_spatial_protocol(&replacement, &lease.controller, (lease.nonce, true), 150);
    assert!(matches!(
        state.prepare_spatial_input(&mut input, 150).unwrap(),
        PersonalFabricAction::SpatialInputSequenced { sequence: 1, .. }
    ));
}
