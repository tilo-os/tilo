//! Causal receiver revocation against the production admission/pruner and real
//! native seat dispatcher. No replacement authority comes from cleanup itself.
use super::*;
use crate::compositor::{native::dispatch_spatial_input_batch, state::State};
use crate::shell::network::{AuthenticatedPersonalRoute, PeerDeparture, PeerDepartureCause};
use crate::shell::personal_continuity::DeviceId;
use smithay::reexports::{calloop::EventLoop, wayland_server::Display};
use std::time::SystemTime;

#[derive(Clone, Copy, Debug)]
enum Withdrawal {
    Expiry,
    ExactRouteLost,
    AuthenticatedDeparture,
}
fn key(keycode: u32) -> FabricInputEvent {
    FabricInputEvent::Key {
        keycode,
        pressed: true,
        modifiers: 0,
    }
}
fn button(button: u32) -> FabricInputEvent {
    FabricInputEvent::PointerButton {
        button,
        pressed: true,
    }
}
fn lease(controller: &DeviceId, target: &DeviceId, nonce: u8, expires: u64) -> InputLease {
    InputLease {
        controller: controller.clone(),
        target: target.clone(),
        nonce: [nonce; 16],
        issued_unix_ms: 100,
        expires_unix_ms: expires,
        allow_keyboard: true,
        allow_pointer: true,
    }
}
fn pressed(state: &State) -> BTreeSet<u32> {
    state
        .seat
        .get_keyboard()
        .unwrap()
        .pressed_keys()
        .iter()
        .map(|key| {
            u32::from(*key)
                .checked_sub(8)
                .expect("seat key must be an XKB keycode")
        })
        .collect()
}
fn seed_authority(
    spatial: &mut SpatialInputState,
    peers: (&DeviceId, &DeviceId),
    target: &DeviceId,
    old_route: &AuthenticatedPersonalRoute,
) {
    let (controller, sibling) = peers;
    spatial
        .issued
        .insert([1; 16], lease(controller, target, 1, 200));
    spatial.issued_routes.insert([1; 16], old_route.clone());
    spatial
        .issued
        .insert([2; 16], lease(sibling, target, 2, 1_000));
    spatial.issued_routes.insert(
        [2; 16],
        AuthenticatedPersonalRoute::for_test(sibling.clone()),
    );
}
fn departure(cause: Withdrawal, endpoint: iroh::EndpointId) -> Vec<PeerDeparture> {
    if matches!(cause, Withdrawal::AuthenticatedDeparture) {
        vec![PeerDeparture {
            generation: 1,
            endpoint_id: endpoint,
            membership_epoch: None,
            session_handle: 1,
            route: crate::shell::collab_broker::CrdtRoute::cluster("personal"),
            cause: PeerDepartureCause::LivenessTimedOut,
            user_public_key: Some([3; 32]),
            participant_name: None,
            observed_at: SystemTime::UNIX_EPOCH,
        }]
    } else {
        Vec::new()
    }
}
fn revoked_then_regranted(cause: Withdrawal) {
    let event_loop: EventLoop<'static, State> = EventLoop::try_new().unwrap();
    let display: Display<State> = Display::new().unwrap();
    let mut seat = State::new(event_loop.handle(), display.handle());
    let endpoint = iroh::SecretKey::from_bytes(&[47; 32]).public();
    let controller = DeviceId::new(hex::encode(endpoint.as_bytes())).unwrap();
    let sibling = DeviceId::new("sibling").unwrap();
    let target = DeviceId::new("target").unwrap();
    let old_route = AuthenticatedPersonalRoute::for_test_epoch(controller.clone(), 7);
    let fresh_route = AuthenticatedPersonalRoute::for_test_epoch(controller.clone(), 8);
    let mut spatial = SpatialInputState::default();
    seed_authority(&mut spatial, (&controller, &sibling), &target, &old_route);
    for event in [key(30), key(31), button(0x110)] {
        assert!(seat.apply_personal_fabric_input(&controller, event));
    }
    assert!(!seat.apply_personal_fabric_input(&sibling, key(30)));
    assert!(!seat.apply_personal_fabric_input(&sibling, button(0x110)));
    spatial.accept_lease_bound_input(&old_route, &target, ([1; 16], key(33)), 150);
    let now = if matches!(cause, Withdrawal::Expiry) {
        201
    } else {
        150
    };
    let departures = departure(cause, endpoint);
    prune_spatial_state(&mut spatial, now, &departures, |device| {
        !matches!(cause, Withdrawal::ExactRouteLost) || device != &controller
    });
    assert!(!spatial.issued.contains_key(&[1; 16]), "{cause:?}");
    assert!(
        spatial.issued.contains_key(&[2; 16]),
        "sibling lease must survive {cause:?}"
    );
    assert!(
        spatial.inbound.is_empty(),
        "old queued edge must be removed by {cause:?}"
    );
    spatial
        .issued
        .insert([3; 16], lease(&controller, &target, 3, 1_000));
    spatial.issued_routes.insert([3; 16], fresh_route.clone());
    spatial.accept_lease_bound_input(&old_route, &target, ([1; 16], key(34)), now);
    spatial.accept_lease_bound_input(&old_route, &target, ([3; 16], key(35)), now);
    assert!(
        spatial.inbound.is_empty(),
        "old nonce and route remain denied after {cause:?}"
    );
    spatial.accept_lease_bound_input(&fresh_route, &target, ([3; 16], key(32)), now);
    spatial.accept_lease_bound_input(&fresh_route, &target, ([3; 16], button(0x111)), now);
    for batch in spatial.take_native_batches() {
        dispatch_spatial_input_batch(&mut seat, batch);
    }
    assert_eq!(
        pressed(&seat),
        BTreeSet::from([30, 32]),
        "old release must preserve sibling and replacement input after {cause:?}"
    );
    assert!(seat.apply_personal_fabric_input(
        &controller,
        FabricInputEvent::PointerButton {
            button: 0x111,
            pressed: false
        }
    ));
    assert!(seat.release_personal_fabric_input(&controller));
    assert_eq!(
        pressed(&seat),
        BTreeSet::from([30]),
        "only sibling remains after {cause:?}"
    );
    assert!(seat.release_personal_fabric_input(&sibling));
    assert!(pressed(&seat).is_empty());
    assert_eq!(seat.release_all_personal_fabric_input(), 0);
}
#[test]
fn spatial_revocation_expiry_route_and_departure_barriers_preserve_replacement_and_sibling_seats() {
    for cause in [
        Withdrawal::Expiry,
        Withdrawal::ExactRouteLost,
        Withdrawal::AuthenticatedDeparture,
    ] {
        revoked_then_regranted(cause);
    }
}
