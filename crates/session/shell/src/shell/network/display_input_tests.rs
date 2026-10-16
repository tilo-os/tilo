//! The production arrangement publisher and lease actions, against certified
//! synthetic identities and a private settings/state tree.

use super::tests::{
    authenticated_personal_peer, drain, personal_workspaces_network_for_test,
    rich_display_network_snapshot,
};
use super::{NetCmd, PersonalFabricPayload, ShellNetwork};
use crate::shell::continuity_policy::ContinuityPolicySnapshot;
use crate::shell::device_fabric::{InputLease, PersonalFabricAction, PersonalFabricActionEnvelope};
use crate::shell::device_fabric_runtime::{
    grant_input_control, person_layout, revoke_input_control,
};
use crate::shell::personal_continuity::DeviceId;
use std::collections::BTreeSet;
use tokio::sync::mpsc;

struct Fixture {
    network: ShellNetwork,
    commands: mpsc::Receiver<NetCmd>,
    controller: DeviceId,
    _home: tilo_test_env::TestHome,
}

impl Fixture {
    fn new() -> Self {
        let home = tilo_test_env::TestHome::new().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[0xB1; 32]);
        let peer = iroh::SecretKey::from_bytes(&[0xB2; 32]);
        let local_device = iroh::SecretKey::from_bytes(&[0xB3; 32]);
        let (mut network, commands) = personal_workspaces_network_for_test(
            &root,
            &local_device,
            crate::shell::state::WorkspaceTree::default(),
        );
        network.personal_continuity_device =
            Some(DeviceId::new(hex::encode(local_device.public().as_bytes())).unwrap());
        network.device_fabric_path = Some(home.state().join("fabric.postcard"));
        network
            .registered_home_device_ids
            .lock()
            .unwrap()
            .insert(*peer.public().as_bytes());
        authenticated_personal_peer(&network, &root, &peer);
        let controller = DeviceId::new(hex::encode(peer.public().as_bytes())).unwrap();
        let local = network.personal_continuity_device.clone().unwrap();
        for owner in [&local, &controller] {
            let (mut snapshot, output) = rich_display_network_snapshot(owner.clone());
            snapshot.presence.capabilities.pointer = true;
            snapshot.presence.capabilities.keyboard = true;
            let stamp = snapshot.display.as_ref().unwrap().stamp.clone();
            network
                .device_fabric
                .lock()
                .unwrap()
                .upsert(snapshot)
                .unwrap();
            network
                .observe_display_outputs(owner, &stamp, vec![output])
                .unwrap();
        }
        Self {
            network,
            commands,
            controller,
            _home: home,
        }
    }

    fn policy(automatic: bool) {
        settings_store::update_section("continuity", serde_json::json!({"enabled":true,"display_input":true,"automatic_display_input":automatic}).as_object().unwrap().clone()).unwrap();
    }

    fn refresh(&self) {
        person_layout::refresh(&self.network, ContinuityPolicySnapshot::current());
    }

    fn actions(&mut self) -> Vec<PersonalFabricAction> {
        drain(&mut self.commands)
            .into_iter()
            .filter_map(|command| {
                if let NetCmd::PublishPersonalFabricTarget {
                    payload: PersonalFabricPayload::Action(bytes),
                    ..
                } = command
                {
                    Some(
                        postcard::from_bytes::<PersonalFabricActionEnvelope>(&bytes)
                            .unwrap()
                            .action,
                    )
                } else {
                    None
                }
            })
            .collect()
    }

    fn grants(&mut self) -> Vec<InputLease> {
        self.actions()
            .into_iter()
            .filter_map(|action| match action {
                PersonalFabricAction::InputLeaseGranted { lease } => Some(lease),
                _ => None,
            })
            .collect()
    }
}

#[test]
fn automatic_display_input_preserves_manual_grants_and_persisted_declines() {
    let mut fixture = Fixture::new();
    Fixture::policy(false);
    fixture.refresh();
    assert!(
        fixture.grants().is_empty(),
        "the existing input opt-in alone cannot grant automatically"
    );
    Fixture::policy(true);
    fixture.refresh();
    let automatic = fixture.grants();
    assert_eq!(automatic.len(), 1);
    assert_eq!(automatic[0].controller, fixture.controller);
    assert_eq!(
        automatic[0].target,
        fixture.network.personal_continuity_device.clone().unwrap()
    );
    fixture.refresh();
    assert!(
        fixture.grants().is_empty(),
        "fresh automatic leases do not replace an active gesture every sample"
    );
    let manual = grant_input_control(&fixture.network, &fixture.controller).unwrap();
    assert_ne!(manual.nonce, automatic[0].nonce);
    assert_eq!(fixture.grants(), vec![manual.clone()]);
    Fixture::policy(false);
    fixture.refresh();
    assert!(
        fixture.actions().is_empty(),
        "turning off automatic input cannot revoke a manually replaced lease"
    );
    revoke_input_control(&fixture.network, &fixture.controller).unwrap();
    assert!(fixture.actions().iter().any(|action| matches!(action, PersonalFabricAction::InputLeaseRevoked { nonce } if nonce == &manual.nonce)));
    // Declines are written on the state writer.
    crate::shell::durable_writer::global().flush();
    assert_eq!(
        fixture.network.load_display_input_declines().unwrap(),
        BTreeSet::from([fixture.controller.clone()])
    );
    Fixture::policy(true);
    fixture.refresh();
    assert!(
        fixture.grants().is_empty(),
        "a later sample cannot undo an explicit manual decline"
    );
    let explicit = grant_input_control(&fixture.network, &fixture.controller).unwrap();
    assert_eq!(fixture.grants(), vec![explicit]);
    crate::shell::durable_writer::global().flush();
    assert!(
        fixture
            .network
            .load_display_input_declines()
            .unwrap()
            .is_empty()
    );
    drop(fixture);
}

/// Granting and revoking input control run on the compositor's loop. The
/// decline they record is durable, but its write must not wait for the disk
/// there: under I/O pressure that fsync froze the desktop.
#[cfg(target_os = "linux")]
#[test]
fn recording_a_manual_decline_never_waits_for_the_disk() {
    let mut fixture = Fixture::new();
    Fixture::policy(false);
    let manual = grant_input_control(&fixture.network, &fixture.controller).unwrap();
    assert_eq!(fixture.grants(), vec![manual]);
    crate::shell::disk_sync_guard::on_a_thread_that_cannot_sync(|| {
        revoke_input_control(&fixture.network, &fixture.controller)
    })
    .expect("revoking and recording the decline need no disk sync");
    crate::shell::durable_writer::global().flush();
    assert_eq!(
        fixture.network.load_display_input_declines().unwrap(),
        BTreeSet::from([fixture.controller.clone()])
    );
    let explicit = crate::shell::disk_sync_guard::on_a_thread_that_cannot_sync(|| {
        grant_input_control(&fixture.network, &fixture.controller)
    })
    .expect("granting clears the decline without a disk sync");
    assert!(
        fixture
            .grants()
            .iter()
            .any(|lease| lease.nonce == explicit.nonce)
    );
    crate::shell::durable_writer::global().flush();
    assert!(
        fixture
            .network
            .load_display_input_declines()
            .unwrap()
            .is_empty()
    );
    drop(fixture);
}

#[test]
fn automatic_display_input_requires_current_enrolment_and_certified_route() {
    let mut fixture = Fixture::new();
    Fixture::policy(true);
    fixture
        .network
        .registered_home_device_ids
        .lock()
        .unwrap()
        .clear();
    fixture
        .network
        .personal_device_peers
        .lock()
        .unwrap()
        .invalidate_recipients();
    fixture.refresh();
    assert!(
        fixture.grants().is_empty(),
        "a workspace certificate does not enrol an input controller"
    );
    assert!(grant_input_control(&fixture.network, &fixture.controller).is_err());
    fixture
        .network
        .registered_home_device_ids
        .lock()
        .unwrap()
        .insert(
            *fixture
                .network
                .current_personal_route(&fixture.controller)
                .unwrap()
                .endpoint_id
                .as_bytes(),
        );
    fixture
        .network
        .personal_device_peers
        .lock()
        .unwrap()
        .invalidate_recipients();
    fixture
        .network
        .authenticated_personal_routes
        .lock()
        .unwrap()
        .clear();
    fixture.refresh();
    assert!(
        fixture.grants().is_empty(),
        "cached online presence does not supply route authority"
    );
    assert!(grant_input_control(&fixture.network, &fixture.controller).is_err());
    drop(fixture);
}

#[test]
fn automatic_display_input_unreadable_declines_fail_closed() {
    let mut fixture = Fixture::new();
    Fixture::policy(true);
    let (path, _) = fixture.network.display_input_declines_path().unwrap();
    std::fs::write(&path, b"not a decline document").unwrap();
    fixture.refresh();
    assert!(fixture.grants().is_empty());
    // Explicit manual control remains possible without discarding unreadable
    // holds or allowing automatic grants to other devices.
    grant_input_control(&fixture.network, &fixture.controller).unwrap();
    assert_eq!(fixture.grants().len(), 1);
    Fixture::policy(false);
    fixture.refresh();
    assert!(fixture.actions().is_empty());
    assert_eq!(std::fs::read(path).unwrap(), b"not a decline document");
    drop(fixture);
}

#[test]
fn automatic_display_input_decline_storage_is_private_bounded_and_root_specific() {
    use std::os::unix::fs::PermissionsExt as _;
    let mut fixture = Fixture::new();
    let original = fixture.network.personal_user_public_key().unwrap();
    let declined = BTreeSet::from([fixture.controller.clone()]);
    fixture
        .network
        .save_display_input_declines(&declined)
        .unwrap();
    let (path, _) = fixture.network.display_input_declines_path().unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fixture.network.load_display_input_declines().unwrap(),
        declined
    );
    fixture.network.personal_continuity_user = Some([0xB7; 32]);
    let (other, _) = fixture.network.display_input_declines_path().unwrap();
    assert_ne!(path, other);
    assert!(
        fixture
            .network
            .load_display_input_declines()
            .unwrap()
            .is_empty()
    );
    fixture.network.personal_continuity_user = Some(original);
    assert_eq!(
        fixture.network.load_display_input_declines().unwrap(),
        declined
    );
    let bytes = std::fs::read(&path).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["user_public_key"] = serde_json::json!(vec![0; 32]);
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(fixture.network.load_display_input_declines().is_err());
    std::fs::write(&path, vec![b' '; 262_145]).unwrap();
    assert!(fixture.network.load_display_input_declines().is_err());
    drop(fixture);
}

#[test]
fn automatic_display_input_exclusion_revokes_only_the_matching_automatic_lease() {
    use settings_store::person_displays::Arrangement;
    let mut fixture = Fixture::new();
    Fixture::policy(true);
    fixture.refresh();
    let automatic = fixture.grants().pop().unwrap();
    let view = person_layout::current(&fixture.network).unwrap();
    let local = view
        .layout
        .displays
        .iter()
        .find(|display| display.local)
        .unwrap();
    let excluded =
        view.layout
            .set_enabled(&Arrangement::default(), &local.device, &local.output, false);
    excluded.save().unwrap();
    fixture.refresh();
    assert!(fixture.actions().iter().any(|action| matches!(action, PersonalFabricAction::InputLeaseRevoked { nonce } if nonce == &automatic.nonce)));
    assert!(
        fixture
            .network
            .load_display_input_declines()
            .unwrap()
            .is_empty(),
        "arrangement exclusion is not a manual decline"
    );
    let manual = grant_input_control(&fixture.network, &fixture.controller).unwrap();
    assert_eq!(fixture.grants(), vec![manual]);
    fixture.refresh();
    assert!(
        fixture.actions().is_empty(),
        "an excluded local output cannot revoke a manual replacement"
    );
    Arrangement::default().save().unwrap();
    fixture.refresh();
    assert!(
        fixture.grants().is_empty(),
        "including the output again cannot replace a manual lease"
    );
    drop(fixture);
}

#[test]
fn spatial_protocol_crosses_certified_ingress_and_publishes_a_bound_ack() {
    use crate::shell::device_fabric::FabricInputEvent;
    use crate::shell::device_fabric_runtime::{handle_spatial_action, take_spatial_input};
    let mut fixture = Fixture::new();
    Fixture::policy(false);
    let lease = grant_input_control(&fixture.network, &fixture.controller).unwrap();
    assert_eq!(fixture.grants(), vec![lease.clone()]);
    let route = fixture
        .network
        .current_personal_route(&fixture.controller)
        .unwrap();
    let button = FabricInputEvent::PointerButton {
        button: 0x110,
        pressed: true,
    };
    let key = FabricInputEvent::Key {
        keycode: 30,
        pressed: false,
        modifiers: 0,
    };
    let motion = |x| FabricInputEvent::PointerMotion { x, y: 0 };
    let actions = [
        PersonalFabricAction::SpatialInputProtocol {
            lease_nonce: lease.nonce,
            acknowledgement: false,
        },
        PersonalFabricAction::SpatialInputSequenced {
            lease_nonce: lease.nonce,
            sequence: 2,
            event: motion(10),
        },
        PersonalFabricAction::SpatialInputSequenced {
            lease_nonce: lease.nonce,
            sequence: 1,
            event: motion(5),
        },
        PersonalFabricAction::SpatialInputSequenced {
            lease_nonce: lease.nonce,
            sequence: 1,
            event: button,
        },
        PersonalFabricAction::SpatialInputSequenced {
            lease_nonce: lease.nonce,
            sequence: 1,
            event: key,
        },
        PersonalFabricAction::SpatialInputSequenced {
            lease_nonce: lease.nonce,
            sequence: 3,
            event: motion(12),
        },
    ];
    for (index, action) in actions.into_iter().enumerate() {
        let request_id = [u8::try_from(index + 1).unwrap(); 16];
        let envelope = PersonalFabricActionEnvelope {
            request_id,
            source: fixture.controller.clone(),
            target: lease.target.clone(),
            issued_unix_ms: tilo_clock::unix_now_ms(),
            action,
        };
        fixture.network.dispatch_personal_fabric_action(
            route.endpoint_id,
            *route.endpoint_id.as_bytes(),
            route.clone(),
            &postcard::to_allocvec(&envelope).unwrap(),
        );
        let received = fixture.network.take_personal_fabric_actions();
        assert_eq!(
            received.len(),
            1,
            "the real wire decoder and replay gate must admit each exact certified packet"
        );
        for item in received {
            assert!(fixture.network.personal_route_is_current(&item.route));
            assert!(ContinuityPolicySnapshot::current().permits_action(&item.envelope.action));
            handle_spatial_action(
                &fixture.network,
                &item.route,
                &item.envelope.target,
                item.envelope.action,
            );
        }
    }
    assert_eq!(
        fixture.actions(),
        vec![PersonalFabricAction::SpatialInputProtocol {
            lease_nonce: lease.nonce,
            acknowledgement: true
        }]
    );
    assert_eq!(
        take_spatial_input(),
        [motion(10), button, key, motion(12)]
            .into_iter()
            .map(|event| (fixture.controller.clone(), event))
            .collect::<Vec<_>>()
    );
    drop(fixture);
}

// The tests that tear down or deliver into the compositor's own Wayland seat
// (`compositor::native`, `compositor::state`, `SpatialSeat`) exist only on
// Linux, like the seat; the transport-only tests run everywhere.
#[cfg(target_os = "linux")]
#[test]
fn spatial_cleanup_native_teardown_survives_full_input_and_transport_queues() {
    use crate::shell::device_fabric::FabricInputEvent;
    use crate::shell::device_fabric_runtime::{
        flush_spatial_input, handle_spatial_action, send_spatial_input_bound,
    };
    let mut fixture = Fixture::new();
    Fixture::policy(false);
    let local = fixture.network.local_device_fabric_identity().unwrap().0;
    let now = tilo_clock::unix_now_ms();
    let lease = InputLease {
        controller: local.clone(),
        target: fixture.controller.clone(),
        issued_unix_ms: now,
        expires_unix_ms: now + 60_000,
        nonce: [5; 16],
        allow_pointer: true,
        allow_keyboard: true,
    };
    let route = fixture
        .network
        .current_personal_route(&lease.target)
        .unwrap();
    handle_spatial_action(
        &fixture.network,
        &route,
        &local,
        PersonalFabricAction::InputLeaseGranted {
            lease: lease.clone(),
        },
    );
    let key = |pressed| FabricInputEvent::Key {
        keycode: 30,
        pressed,
        modifiers: 0,
    };
    let button = |pressed| FabricInputEvent::PointerButton {
        button: 0x110,
        pressed,
    };
    let mut expected = vec![button(true)];
    expected.extend(std::iter::repeat_n(key(true), 255));
    for event in &expected {
        send_spatial_input_bound(&fixture.network, &lease.target, lease.nonce, *event).unwrap();
    }
    assert!(
        send_spatial_input_bound(&fixture.network, &lease.target, lease.nonce, key(false)).is_err()
    );
    while fixture.network.cmd_tx.capacity() > 0 {
        let (reply, _) = tokio::sync::oneshot::channel();
        fixture
            .network
            .cmd_tx
            .try_send(NetCmd::WireTally(reply))
            .unwrap();
    }
    assert!(
        crate::compositor::native::teardown_spatial_control_for_test(
            &fixture.network,
            lease.target.clone(),
            lease.nonce,
            (BTreeSet::from([30]), BTreeSet::from([0x110]))
        )
    );
    flush_spatial_input(&fixture.network);
    assert!(
        fixture.actions().is_empty(),
        "the full transport must not report a successful input send"
    );
    let mut emitted = Vec::new();
    for index in 0..10 {
        flush_spatial_input(&fixture.network);
        emitted.extend(
            fixture
                .actions()
                .into_iter()
                .filter_map(|action| match action {
                    PersonalFabricAction::Input { lease_nonce, event } => {
                        assert_eq!(lease_nonce, lease.nonce);
                        Some(event)
                    }
                    _ => None,
                }),
        );
        if index == 0 {
            assert!(
                send_spatial_input_bound(&fixture.network, &lease.target, lease.nonce, key(true))
                    .is_err(),
                "ordinary slots are free but the old lease still owns cleanup"
            );
        }
    }
    expected.extend([key(false), button(false)]);
    assert_eq!(
        emitted, expected,
        "every admitted edge must precede its retained cleanup"
    );
    send_spatial_input_bound(&fixture.network, &lease.target, lease.nonce, key(true)).unwrap();
    drop(fixture);
}

#[cfg(target_os = "linux")]
struct SpatialSeat {
    _event_loop: smithay::reexports::calloop::EventLoop<'static, crate::compositor::state::State>,
    _display: smithay::reexports::wayland_server::Display<crate::compositor::state::State>,
    state: crate::compositor::state::State,
}
#[cfg(target_os = "linux")]
impl SpatialSeat {
    fn new() -> Self {
        let event_loop = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let display = smithay::reexports::wayland_server::Display::new().unwrap();
        let state = crate::compositor::state::State::new(event_loop.handle(), display.handle());
        Self {
            _event_loop: event_loop,
            _display: display,
            state,
        }
    }
    fn pump(&mut self) {
        for batch in crate::shell::device_fabric_runtime::take_spatial_input_batches() {
            crate::compositor::native::dispatch_spatial_input_batch(&mut self.state, batch);
        }
    }
    fn keys(&self) -> BTreeSet<u32> {
        self.state
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
}
fn spatial_key(keycode: u32, pressed: bool) -> crate::shell::device_fabric::FabricInputEvent {
    crate::shell::device_fabric::FabricInputEvent::Key {
        keycode,
        pressed,
        modifiers: 0,
    }
}
#[cfg(target_os = "linux")]
fn spatial_button(button: u32, pressed: bool) -> crate::shell::device_fabric::FabricInputEvent {
    crate::shell::device_fabric::FabricInputEvent::PointerButton { button, pressed }
}
#[cfg(target_os = "linux")]
fn admit_spatial(
    fixture: &Fixture,
    nonce: [u8; 16],
    event: crate::shell::device_fabric::FabricInputEvent,
) {
    let route = fixture
        .network
        .current_personal_route(&fixture.controller)
        .unwrap();
    let target = fixture.network.local_device_fabric_identity().unwrap().0;
    crate::shell::device_fabric_runtime::handle_spatial_action(
        &fixture.network,
        &route,
        &target,
        PersonalFabricAction::Input {
            lease_nonce: nonce,
            event,
        },
    );
}
fn fill_transport(fixture: &Fixture) {
    while fixture.network.cmd_tx.capacity() > 0 {
        let (reply, _) = tokio::sync::oneshot::channel();
        fixture
            .network
            .cmd_tx
            .try_send(NetCmd::WireTally(reply))
            .unwrap();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn spatial_revocation_replacement_preserves_fresh_seat_edges_and_discards_old_nonce() {
    let mut fixture = Fixture::new();
    Fixture::policy(false);
    let mut seat = SpatialSeat::new();
    let old = grant_input_control(&fixture.network, &fixture.controller).unwrap();
    admit_spatial(&fixture, old.nonce, spatial_key(30, true));
    admit_spatial(&fixture, old.nonce, spatial_button(0x110, true));
    seat.pump();
    assert_eq!(seat.keys(), BTreeSet::from([30]));
    for _ in 0..256 {
        admit_spatial(&fixture, old.nonce, spatial_key(31, true));
    }
    let fresh = grant_input_control(&fixture.network, &fixture.controller).unwrap();
    assert_ne!(old.nonce, fresh.nonce);
    admit_spatial(&fixture, old.nonce, spatial_key(33, true));
    admit_spatial(&fixture, fresh.nonce, spatial_key(32, true));
    admit_spatial(&fixture, fresh.nonce, spatial_button(0x111, true));
    seat.pump();
    assert_eq!(
        seat.keys(),
        BTreeSet::from([32]),
        "old cleanup must precede fresh down edges"
    );
    assert!(
        seat.state
            .apply_personal_fabric_input(&fixture.controller, spatial_button(0x111, false)),
        "fresh button attribution must survive the old release barrier"
    );
    assert!(
        !seat
            .state
            .apply_personal_fabric_input(&fixture.controller, spatial_button(0x110, false)),
        "old button attribution must be gone"
    );
    admit_spatial(&fixture, fresh.nonce, spatial_key(32, false));
    seat.pump();
    assert!(seat.keys().is_empty());
    assert_eq!(
        fixture
            .actions()
            .iter()
            .filter(|action| matches!(action, PersonalFabricAction::InputLeaseGranted { .. }))
            .count(),
        2,
    );
    drop(fixture);
}

#[cfg(target_os = "linux")]
#[test]
fn spatial_revocation_failed_notification_still_clears_real_seat_and_refuses_stale_input() {
    let mut fixture = Fixture::new();
    Fixture::policy(false);
    let mut seat = SpatialSeat::new();
    let lease = grant_input_control(&fixture.network, &fixture.controller).unwrap();
    assert_eq!(fixture.grants(), vec![lease.clone()]);
    admit_spatial(&fixture, lease.nonce, spatial_key(30, true));
    admit_spatial(&fixture, lease.nonce, spatial_button(0x110, true));
    seat.pump();
    assert_eq!(seat.keys(), BTreeSet::from([30]));
    admit_spatial(&fixture, lease.nonce, spatial_key(31, true));
    fill_transport(&fixture);
    assert!(revoke_input_control(&fixture.network, &fixture.controller).is_err());
    assert!(
        fixture.actions().is_empty(),
        "full transport delivered no revocation notification"
    );
    admit_spatial(&fixture, lease.nonce, spatial_key(32, true));
    seat.state.remote_input_firewall.engage_kill_switch();
    seat.pump();
    assert!(
        seat.keys().is_empty(),
        "local authority removal must release held and discard queued edges even under kill switch"
    );
    assert!(
        !seat
            .state
            .apply_personal_fabric_input(&fixture.controller, spatial_button(0x110, false))
    );
    drop(fixture);
    assert_eq!(seat.state.release_all_personal_fabric_input(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn spatial_revocation_policy_disable_cleans_target_without_delivery_or_new_authority() {
    use crate::compositor::selection::SharedSelection;
    use crate::shell::device_fabric_runtime::DeviceFabricRuntime;
    let mut fixture = Fixture::new();
    Fixture::policy(false);
    let mut runtime = DeviceFabricRuntime::new(
        &fixture.network,
        crate::notifications::NotificationBridge::new(),
    );
    let selection = SharedSelection::default();
    runtime.enforce_policy_for_test(
        &fixture.network,
        &selection,
        ContinuityPolicySnapshot::current(),
    );
    let lease = grant_input_control(&fixture.network, &fixture.controller).unwrap();
    assert_eq!(fixture.grants(), vec![lease.clone()]);
    let mut seat = SpatialSeat::new();
    admit_spatial(&fixture, lease.nonce, spatial_key(30, true));
    admit_spatial(&fixture, lease.nonce, spatial_button(0x110, true));
    seat.pump();
    assert_eq!(seat.keys(), BTreeSet::from([30]));
    admit_spatial(&fixture, lease.nonce, spatial_key(31, true));
    fill_transport(&fixture);
    settings_store::update_section(
        "continuity",
        serde_json::json!({"enabled":true,"display_input":false,"automatic_display_input":false})
            .as_object()
            .unwrap()
            .clone(),
    )
    .unwrap();
    runtime.enforce_policy_for_test(
        &fixture.network,
        &selection,
        ContinuityPolicySnapshot::current(),
    );
    assert!(fixture.actions().is_empty());
    admit_spatial(&fixture, lease.nonce, spatial_key(32, true));
    seat.state.remote_input_firewall.engage_kill_switch();
    seat.pump();
    assert!(seat.keys().is_empty());
    assert_eq!(seat.state.release_all_personal_fabric_input(), 0);
    assert!(grant_input_control(&fixture.network, &fixture.controller).is_err());
    drop(fixture);
}

#[test]
fn display_canvas_identity_uses_only_current_certified_output_metadata() {
    let fixture = Fixture::new();
    Fixture::policy(false);
    let local = fixture.network.local_device_fabric_identity().unwrap().0;
    let mut fabric = fixture.network.device_fabric();
    let expected = fabric.display_outputs(&fixture.controller).unwrap()[0]
        .output
        .id
        .clone();
    let screens =
        crate::shell::device_fabric_runtime::observed_device_displays(&fabric, Some(&local));
    let peer = screens
        .iter()
        .find(|screen| screen.device == fixture.controller.as_str())
        .unwrap();
    assert_eq!(peer.output_id.as_deref(), Some(expected.as_str()));
    let mut snapshot = fabric.snapshots().get(&fixture.controller).unwrap().clone();
    snapshot.presence.stamp.counter = snapshot.presence.stamp.counter.saturating_add(1);
    snapshot.display.as_mut().unwrap().stamp.counter = snapshot.presence.stamp.counter;
    fabric.upsert(snapshot).unwrap();
    assert!(fabric.display_outputs(&fixture.controller).is_none());
    let screens =
        crate::shell::device_fabric_runtime::observed_device_displays(&fabric, Some(&local));
    let peer = screens
        .iter()
        .find(|screen| screen.device == fixture.controller.as_str())
        .unwrap();
    assert_eq!(
        peer.output_id, None,
        "stale metadata must not assign another topology's stable identity"
    );
    drop(fixture);
}

fn receive_controller_lease(fixture: &Fixture) -> InputLease {
    use crate::shell::device_fabric_runtime::handle_spatial_action;
    let local = fixture.network.local_device_fabric_identity().unwrap().0;
    let route = fixture
        .network
        .current_personal_route(&fixture.controller)
        .unwrap();
    let now = tilo_clock::unix_now_ms();
    let lease = InputLease {
        controller: local.clone(),
        target: fixture.controller.clone(),
        issued_unix_ms: now,
        expires_unix_ms: now.saturating_add(60_000),
        nonce: [5; 16],
        allow_keyboard: true,
        allow_pointer: true,
    };
    handle_spatial_action(
        &fixture.network,
        &route,
        &local,
        PersonalFabricAction::InputLeaseGranted {
            lease: lease.clone(),
        },
    );
    lease
}

#[test]
fn spatial_controller_withdrawal_retries_terminal_after_policy_off_and_transport_full() {
    use crate::shell::device_fabric_runtime::{
        DeviceFabricRuntime, flush_spatial_input, send_spatial_input_bound,
    };
    let mut fixture = Fixture::new();
    Fixture::policy(false);
    let lease = receive_controller_lease(&fixture);
    let mut runtime = DeviceFabricRuntime::new(
        &fixture.network,
        crate::notifications::NotificationBridge::new(),
    );
    let selection = crate::compositor::selection::SharedSelection::default();
    runtime.enforce_policy_for_test(
        &fixture.network,
        &selection,
        ContinuityPolicySnapshot::current(),
    );
    send_spatial_input_bound(
        &fixture.network,
        &lease.target,
        lease.nonce,
        spatial_key(30, true),
    )
    .unwrap();
    fill_transport(&fixture);
    settings_store::update_section(
        "continuity",
        serde_json::json!({"enabled":true,"display_input":false})
            .as_object()
            .unwrap()
            .clone(),
    )
    .unwrap();
    runtime.enforce_policy_for_test(
        &fixture.network,
        &selection,
        ContinuityPolicySnapshot::current(),
    );
    assert!(
        send_spatial_input_bound(
            &fixture.network,
            &lease.target,
            lease.nonce,
            spatial_key(30, false)
        )
        .is_err()
    );
    assert!(
        crate::shell::device_fabric_runtime::queue_spatial_input_cleanup_bound(
            &fixture.network,
            &lease.target,
            lease.nonce,
            vec![spatial_key(30, false)]
        )
        .is_err()
    );
    assert!(
        fixture.actions().is_empty(),
        "full transport cannot report terminal delivery"
    );
    runtime.pump(
        &fixture.network,
        &selection,
        crate::shell::device_fabric_runtime::LocalFabricCapabilities {
            display: None,
            pointer: false,
            keyboard: false,
            audio_output: false,
        },
    );
    assert_eq!(
        fixture.actions(),
        vec![PersonalFabricAction::InputLeaseRevoked { nonce: lease.nonce }]
    );
    flush_spatial_input(&fixture.network);
    assert!(
        fixture.actions().is_empty(),
        "terminal delivery is idempotent and creates no grant or input"
    );
    Fixture::policy(false);
    assert!(
        send_spatial_input_bound(
            &fixture.network,
            &lease.target,
            lease.nonce,
            spatial_key(30, true)
        )
        .is_err(),
        "reenabling policy does not restore withdrawn authority"
    );
    drop(fixture);
}

#[cfg(target_os = "linux")]
#[test]
fn spatial_controller_withdrawal_clears_target_and_fences_late_edges_and_renewal() {
    use crate::shell::device_fabric_runtime::handle_spatial_action;
    let mut fixture = Fixture::new();
    Fixture::policy(true);
    let mut seat = SpatialSeat::new();
    fixture.refresh();
    let grants = fixture.grants();
    assert_eq!(grants.len(), 1);
    let lease = grants[0].clone();
    let local = fixture.network.local_device_fabric_identity().unwrap().0;
    let route = fixture
        .network
        .current_personal_route(&fixture.controller)
        .unwrap();
    let wrong_route =
        super::AuthenticatedPersonalRoute::for_test_epoch(fixture.controller.clone(), 99);
    admit_spatial(&fixture, lease.nonce, spatial_key(30, true));
    admit_spatial(&fixture, lease.nonce, spatial_button(0x110, true));
    seat.pump();
    handle_spatial_action(
        &fixture.network,
        &wrong_route,
        &local,
        PersonalFabricAction::InputLeaseRevoked { nonce: lease.nonce },
    );
    seat.pump();
    assert_eq!(
        seat.keys(),
        BTreeSet::from([30]),
        "wrong route cannot release held input"
    );
    admit_spatial(&fixture, lease.nonce, spatial_key(31, true));
    handle_spatial_action(
        &fixture.network,
        &route,
        &local,
        PersonalFabricAction::InputLeaseRevoked { nonce: lease.nonce },
    );
    admit_spatial(&fixture, lease.nonce, spatial_key(32, true));
    seat.pump();
    assert!(
        seat.keys().is_empty(),
        "withdrawal removes held and queued old input"
    );
    assert_eq!(seat.state.release_all_personal_fabric_input(), 0);
    fixture.refresh();
    assert!(
        fixture.grants().is_empty(),
        "same-route automatic renewal must respect controller withdrawal"
    );
    let fresh = grant_input_control(&fixture.network, &fixture.controller).unwrap();
    admit_spatial(&fixture, fresh.nonce, spatial_key(33, true));
    handle_spatial_action(
        &fixture.network,
        &route,
        &local,
        PersonalFabricAction::InputLeaseRevoked { nonce: lease.nonce },
    );
    seat.pump();
    assert_eq!(
        seat.keys(),
        BTreeSet::from([33]),
        "late terminal cannot erase replacement authority"
    );
    assert_eq!(fixture.grants(), vec![fresh]);
    drop(fixture);
}

#[test]
fn spatial_controller_withdrawal_preserves_original_revoke_wire_bytes() {
    let action = PersonalFabricAction::InputLeaseRevoked { nonce: [7; 16] };
    let mut original = vec![4];
    original.extend_from_slice(&[7; 16]);
    assert_eq!(postcard::to_allocvec(&action).unwrap(), original);
    assert_eq!(
        postcard::from_bytes::<PersonalFabricAction>(&original).unwrap(),
        action
    );
}
