//! Causal display refresh through the real pump, private snapshots and signed custody.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use settings_store::hardware::{
    DisplayHardwareSnapshot, DisplayOutputSnapshot, LiveOutputPlacement, MonitorIdentity,
    OutputDescriptor, OutputMode,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use super::{HostIdentity, NetCmd, ShellNetwork};
use crate::compositor::selection::SelectionStore;
use crate::notifications::NotificationBridge;
use crate::shell::device_fabric::DisplayRect;
use crate::shell::device_fabric_runtime::{DeviceFabricRuntime, LocalFabricCapabilities};
use crate::shell::personal_continuity::DeviceId;
use crate::shell::state::WorkspaceTree;

#[test]
#[serial_test::serial]
fn a_mode_only_observation_reaches_the_fabric_and_person_view_before_the_periodic_sample() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let policy = serde_json::json!({"enabled": true, "display_input": true});
    settings_store::update_section("continuity", policy.as_object().unwrap().clone()).unwrap();
    let mut hardware = hardware();
    hardware.save().unwrap();
    let (network, _commands) = certified_network();
    let (wake, awakened) = mpsc::channel();
    network.set_wake(Arc::new(move || {
        let _ = wake.send(());
    }));
    let selection = Arc::new(Mutex::new(SelectionStore::default()));
    let local = LocalFabricCapabilities {
        display: Some(DisplayRect {
            x: 40,
            y: 20,
            width: 1280,
            height: 800,
        }),
        pointer: true,
        keyboard: true,
        audio_output: false,
    };
    let owner = network.local_device_fabric_identity().unwrap().0;
    let mut runtime = DeviceFabricRuntime::new(&network, NotificationBridge::new());
    let first_sample = Instant::now();
    runtime.pump(&network, &selection, local);
    assert_person_refresh(59_940);
    let previous = network
        .device_fabric()
        .snapshots()
        .get(&owner)
        .unwrap()
        .display
        .clone()
        .unwrap();

    hardware
        .outputs
        .first_mut()
        .unwrap()
        .current_mode
        .refresh_mhz = 60_000;
    hardware.save().unwrap();
    awakened.recv_timeout(tilo_test_env::hang_bound()).unwrap();
    assert!(
        first_sample.elapsed() < Duration::from_secs(1),
        "fixture must observe the dirty signal before periodic sampling could hide the gap"
    );
    runtime.pump(&network, &selection, local);
    assert!(
        first_sample.elapsed() < Duration::from_secs(1),
        "actual refresh must finish before the periodic sample"
    );
    let fabric = network.device_fabric();
    let current = fabric
        .snapshots()
        .get(&owner)
        .unwrap()
        .display
        .as_ref()
        .unwrap();
    assert_eq!(
        current.rect, previous.rect,
        "mode-only changes preserve physical placement"
    );
    assert!(current.stamp.counter > previous.stamp.counter);
    assert_eq!(
        fabric.display_outputs(&owner).unwrap()[0]
            .output
            .refresh_mhz,
        60_000
    );
    assert_person_refresh(60_000);
}

fn assert_person_refresh(refresh: u32) {
    // The pump hands the person's view to the state writer rather than
    // waiting for the disk on the compositor thread.
    crate::shell::persist::wait_for_saves();
    let view = settings_store::person_displays::PersonDisplays::load().unwrap();
    assert_eq!(view.devices.len(), 1);
    assert!(view.devices.first().unwrap().local);
    assert_eq!(view.devices.first().unwrap().outputs.len(), 1);
    assert_eq!(
        view.devices
            .first()
            .unwrap()
            .outputs
            .first()
            .unwrap()
            .refresh_mhz,
        refresh
    );
}

fn certified_network() -> (ShellNetwork, tokio::sync::mpsc::Receiver<NetCmd>) {
    let root = SigningKey::generate(&mut UnwrapErr(SysRng));
    let device = iroh::SecretKey::generate();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: *device.public().as_bytes(),
        device_label: "Dirty display fixture".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let user = cert.user_public_key;
    let owner = DeviceId::new(hex::encode(cert.device_public_key)).unwrap();
    let identity = HostIdentity {
        device_cert: cert,
        device_signing_key: SigningKey::from_bytes(&device.to_bytes()),
        workspace_device_certs: HashMap::new(),
        workspace_revocations: HashMap::new(),
        identity_paths: None,
    };
    let (mut network, commands) =
        super::native_read_network_for_test(WorkspaceTree::default().shared(), identity);
    network.personal_continuity_user = Some(user);
    network.personal_continuity_device = Some(owner);
    network.require_peer_identity = true;
    network.continuity_policy_override = None;
    (network, commands)
}

fn hardware() -> DisplayHardwareSnapshot {
    let (publisher_pid, publisher_start_ticks) =
        settings_store::hardware::publisher_identity().unwrap();
    DisplayHardwareSnapshot {
        publisher_pid,
        publisher_start_ticks,
        outputs: vec![DisplayOutputSnapshot {
            descriptor: OutputDescriptor {
                connector: "eDP-1".into(),
                connector_label: String::new(),
                modes: Vec::new(),
                hdr: false,
                vrr: false,
                color_profiles: Vec::new(),
                monitor: Some(MonitorIdentity {
                    make: "PanelCo".into(),
                    model: "Panel".into(),
                    serial: "123".into(),
                    physical_size_mm: None,
                }),
            },
            current_mode: OutputMode {
                width: 2560,
                height: 1600,
                refresh_mhz: 59_940,
            },
            scale: 2.0,
            builtin: true,
            sink_hdr: false,
            hdr_unavailable_reason: None,
            sink_vrr: false,
            vrr_unavailable_reason: None,
            color_profile_labels: BTreeMap::new(),
        }],
        layout: vec![LiveOutputPlacement {
            connector: "eDP-1".into(),
            x: 40,
            y: 20,
            width: 1280,
            height: 800,
        }],
        ..DisplayHardwareSnapshot::default()
    }
}
