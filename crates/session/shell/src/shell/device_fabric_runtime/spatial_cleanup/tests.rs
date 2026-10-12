use super::*;
use crate::shell::{device_fabric::PersonalFabricAction, network::AuthenticatedPersonalRoute};

fn sender() -> (SpatialInputState, InputLease, AuthenticatedPersonalRoute) {
    let lease = InputLease {
        controller: DeviceId::new("controller").unwrap(),
        target: DeviceId::new("target").unwrap(),
        issued_unix_ms: 100,
        expires_unix_ms: 200,
        nonce: [5; 16],
        allow_pointer: true,
        allow_keyboard: true,
    };
    let route = AuthenticatedPersonalRoute::for_test_epoch(lease.target.clone(), 7);
    let mut state = SpatialInputState::default();
    state.receive_spatial_grant(lease.target.clone(), route.clone(), lease.clone());
    state.negotiate_spatial_protocol(&route, &lease.controller, (lease.nonce, true), 150);
    (state, lease, route)
}
fn key(pressed: bool) -> FabricInputEvent {
    FabricInputEvent::Key {
        keycode: 30,
        pressed,
        modifiers: 0,
    }
}
fn button(pressed: bool) -> FabricInputEvent {
    FabricInputEvent::PointerButton {
        button: 0x110,
        pressed,
    }
}
fn input(lease: &InputLease, event: FabricInputEvent) -> OutboundSpatialInput {
    OutboundSpatialInput {
        target: lease.target.clone(),
        lease_nonce: lease.nonce,
        event,
        sequence: None,
    }
}
fn releases() -> Vec<FabricInputEvent> {
    vec![key(false), button(false)]
}

#[test]
fn full_queue_spatial_cleanup_keeps_every_edge_and_retries_the_exact_sequence() {
    let (mut state, lease, _) = sender();
    for index in 0..super::super::MAX_QUEUED_SPATIAL_INPUT {
        let event = if index == 0 { button(true) } else { key(true) };
        state
            .queue_lease_bound_input(&lease.controller, input(&lease, event), 150)
            .unwrap();
    }
    assert!(
        state
            .queue_lease_bound_input(&lease.controller, input(&lease, key(false)), 150)
            .is_err()
    );
    state
        .queue_cleanup_bound(
            &lease.controller,
            &lease.target,
            (lease.nonce, releases()),
            150,
        )
        .unwrap();
    assert!(
        state
            .queue_lease_bound_input(&lease.controller, input(&lease, key(true)), 150)
            .is_err()
    );
    let mut emitted = Vec::new();
    for _ in 0..super::super::MAX_QUEUED_SPATIAL_INPUT {
        let (mut queued, cleanup) = state.next_spatial_input().unwrap();
        assert!(
            !cleanup,
            "cleanup cannot overtake a previously admitted down edge"
        );
        state.prepare_spatial_input(&mut queued, 150).unwrap();
        emitted.push(queued.event);
    }
    let (mut release, cleanup) = state.next_spatial_input().unwrap();
    assert!(cleanup);
    let action = state.prepare_spatial_input(&mut release, 150).unwrap();
    state.cleanup.prepared(&release);
    // A full transport did not accept this release. Duplicate teardown must
    // neither lose it nor assign a fresh sequence when the retry can send.
    state
        .queue_cleanup_bound(
            &lease.controller,
            &lease.target,
            (lease.nonce, releases()),
            150,
        )
        .unwrap();
    let (mut retry, cleanup) = state.next_spatial_input().unwrap();
    assert!(cleanup);
    assert_eq!(
        state.prepare_spatial_input(&mut retry, 150).unwrap(),
        action
    );
    assert!(matches!(
        action,
        PersonalFabricAction::SpatialInputSequenced { sequence: 257, .. }
    ));
    emitted.push(retry.event);
    state.cleanup.delivered(&retry);
    assert!(state.cleanup.blocks(&lease.target, lease.nonce));
    let (mut release, cleanup) = state.next_spatial_input().unwrap();
    assert!(cleanup);
    state.prepare_spatial_input(&mut release, 150).unwrap();
    state.cleanup.prepared(&release);
    emitted.push(release.event);
    state.cleanup.delivered(&release);
    assert!(!state.cleanup.blocks(&lease.target, lease.nonce));
    assert_eq!(emitted.len(), 258);
    assert_eq!(emitted[0], button(true));
    assert!(emitted[1..256].iter().all(|event| *event == key(true)));
    assert_eq!(&emitted[256..], releases().as_slice());
    let mut held_key = false;
    let mut held_button = false;
    for event in emitted {
        match event {
            FabricInputEvent::Key { pressed, .. } => held_key = pressed,
            FabricInputEvent::PointerButton { pressed, .. } => held_button = pressed,
            _ => panic!("unexpected event in the actual drained input stream"),
        }
    }
    assert!(!held_key && !held_button);
    state
        .queue_lease_bound_input(&lease.controller, input(&lease, key(true)), 150)
        .unwrap();
}

#[test]
fn spatial_cleanup_admission_is_atomic_release_only_and_keeps_lease_capabilities() {
    let (mut state, lease, _) = sender();
    for (controller, target, nonce, now, events) in [
        (
            lease.target.clone(),
            lease.target.clone(),
            lease.nonce,
            150,
            releases(),
        ),
        (
            lease.controller.clone(),
            lease.controller.clone(),
            lease.nonce,
            150,
            releases(),
        ),
        (
            lease.controller.clone(),
            lease.target.clone(),
            [9; 16],
            150,
            releases(),
        ),
        (
            lease.controller.clone(),
            lease.target.clone(),
            lease.nonce,
            201,
            releases(),
        ),
        (
            lease.controller.clone(),
            lease.target.clone(),
            lease.nonce,
            150,
            vec![key(false), key(true)],
        ),
        (
            lease.controller.clone(),
            lease.target.clone(),
            lease.nonce,
            150,
            vec![FabricInputEvent::PointerMotion { x: 0, y: 0 }],
        ),
        (
            lease.controller.clone(),
            lease.target.clone(),
            lease.nonce,
            150,
            vec![FabricInputEvent::Key {
                keycode: 0x300,
                pressed: false,
                modifiers: 0,
            }],
        ),
    ] {
        assert!(
            state
                .queue_cleanup_bound(&controller, &target, (nonce, events), now)
                .is_err()
        );
        assert!(state.cleanup.pending.is_empty());
    }
    state
        .received
        .get_mut(&lease.target)
        .unwrap()
        .allow_keyboard = false;
    assert!(
        state
            .queue_cleanup_bound(
                &lease.controller,
                &lease.target,
                (lease.nonce, releases()),
                150
            )
            .is_err()
    );
    assert!(state.cleanup.pending.is_empty());
    state
        .queue_cleanup_bound(
            &lease.controller,
            &lease.target,
            (lease.nonce, vec![button(false)]),
            150,
        )
        .unwrap();
    assert_eq!(state.next_spatial_input().unwrap().0.event, button(false));
}

#[test]
fn spatial_cleanup_does_not_adopt_replacement_route_nonce_or_expired_authority() {
    let (mut state, lease, route) = sender();
    state
        .queue_cleanup_bound(
            &lease.controller,
            &lease.target,
            (lease.nonce, releases()),
            150,
        )
        .unwrap();
    state.receive_spatial_grant(lease.target.clone(), route, lease.clone());
    assert!(state.cleanup.blocks(&lease.target, lease.nonce));
    state.receive_spatial_grant(
        lease.target.clone(),
        AuthenticatedPersonalRoute::for_test_epoch(lease.target.clone(), 8),
        lease.clone(),
    );
    assert!(state.cleanup.pending.is_empty());
    state
        .queue_cleanup_bound(
            &lease.controller,
            &lease.target,
            (lease.nonce, releases()),
            150,
        )
        .unwrap();
    let replacement = InputLease {
        nonce: [6; 16],
        ..lease.clone()
    };
    state.receive_spatial_grant(
        lease.target.clone(),
        AuthenticatedPersonalRoute::for_test_epoch(lease.target.clone(), 8),
        replacement,
    );
    assert!(state.cleanup.pending.is_empty());
    assert!(
        state
            .queue_cleanup_bound(
                &lease.controller,
                &lease.target,
                (lease.nonce, releases()),
                150
            )
            .is_err()
    );
    state
        .queue_cleanup_bound(&lease.controller, &lease.target, ([6; 16], releases()), 150)
        .unwrap();
    super::super::prune_spatial_state(&mut state, 201, &[], |_| true);
    assert!(state.cleanup.pending.is_empty());
}

#[test]
fn spatial_cleanup_barrier_is_per_lease_and_cannot_be_starved_by_another_target() {
    let (mut state, lease, _) = sender();
    state
        .queue_lease_bound_input(&lease.controller, input(&lease, key(true)), 150)
        .unwrap();
    state
        .queue_cleanup_bound(
            &lease.controller,
            &lease.target,
            (lease.nonce, releases()),
            150,
        )
        .unwrap();
    let other = InputLease {
        target: DeviceId::new("other").unwrap(),
        ..lease.clone()
    };
    state.receive_spatial_grant(
        other.target.clone(),
        AuthenticatedPersonalRoute::for_test_epoch(other.target.clone(), 7),
        other.clone(),
    );
    state
        .queue_lease_bound_input(&other.controller, input(&other, key(true)), 150)
        .unwrap();
    assert!(!state.next_spatial_input().unwrap().1);
    let (release, cleanup) = state.next_spatial_input().unwrap();
    assert!(cleanup);
    assert_eq!(release.target, lease.target);
    assert_eq!(state.outbound.front().unwrap().target, other.target);
}
