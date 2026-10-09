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
        person_layout::refresh(&self.network, ContinuityPolicySnapshot::load());
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
