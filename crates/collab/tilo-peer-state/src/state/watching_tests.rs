//! A device that only watches a workspace, as a page load's: governance
//! records it, every participant's readers admit it to the records no grant
//! covers and withhold it from every other, and releasing it takes it out at
//! the next epoch.
use super::*;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

/// A watching page's device: an hour-long certificate of `root` with no
/// capabilities, as the workspace's host signs it for a visitor.
fn page(root: &SigningKey, seed: u8, now: SystemTime) -> DeviceCert {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Watching visitor".into(),
        capabilities: DeviceCapabilities {
            can_own_workspaces: false,
            can_admit_devices: false,
            can_act_as_host: false,
            can_read_all_workspaces: false,
            foreign: false,
            can_administer_devices: false,
        },
        issued_at_ns: now_ns(now),
        expires_at_ns: Some(now_ns(now).saturating_add(3_600_000_000_000)),
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

#[test]
fn a_watching_device_reads_uncovered_records_and_is_withheld_from_covered_ones() {
    let (mut peer, owner_key, _, member, now) = crate::admission_tests::seeded();
    let root = SigningKey::from_bytes(&[1; 32]);
    let visitor = page(&root, 20, now);
    let device = visitor.device_public_key;

    assert!(
        peer.governance_mut()
            .admit_watching_device(&visitor, &owner_key, now)
            .unwrap()
    );
    // Kept apart from the workspace's members: a device that only watches is
    // no one's member device, and reads through the watching rows alone.
    assert!(
        !peer
            .governance()
            .governed_access(now)
            .unwrap()
            .member_devices
            .contains(&device)
    );
    assert!(
        peer.governance()
            .watching_devices()
            .unwrap()
            .contains(&device)
    );

    let app = "so.tilo.notes";
    let workspace = peer.workspace_id().to_owned();
    // A record no grant covers, and one person's own record, which only that
    // person reads.
    let public = collab_types::ChannelRoute::new(app, "note-1");
    let covered = collab_types::ChannelRoute::new(
        app,
        &collab_types::member_record_id(
            &hex::encode(peer.governance().owner_user_public_key()),
            "notes",
        ),
    );
    let channel = |route: &collab_types::ChannelRoute| route.channel(&workspace);

    let readers = peer
        .governance()
        .withheld_record_readers(now, &[public.clone(), covered.clone()])
        .unwrap();
    assert!(
        !readers.withheld[&channel(&public)].contains(&device),
        "a grant withheld a record no grant covers"
    );
    assert!(
        readers.withheld[&channel(&covered)].contains(&device),
        "a record one person alone reads was not withheld from a visitor"
    );
    assert!(readers.readers.contains(&device));

    // What each channel's group follows.
    assert!(
        peer.governance()
            .channel_readers(now, channel(&public), Some(&public))
            .unwrap()
            .unwrap()
            .contains(&device)
    );
    assert!(
        !peer
            .governance()
            .channel_readers(now, channel(&covered), Some(&covered))
            .unwrap()
            .unwrap()
            .contains(&device)
    );
    // A channel whose record no one named may be anyone's: no one reads it.
    let unnamed = collab_types::document_channel_id(app, &workspace, "unknown");
    assert!(
        peer.governance()
            .channel_readers(now, unnamed, None)
            .unwrap()
            .is_none()
    );

    // A member's own certificate never becomes a visitor's: it carries the
    // capabilities a person's device has.
    let refused = peer
        .governance_mut()
        .admit_watching_device(&member, &owner_key, now)
        .unwrap_err();
    assert!(refused.contains("carries no capabilities"), "{refused}");

    // Let go, it is no reader of the workspace's records at the next epoch.
    assert!(
        peer.governance_mut()
            .release_watching_device(&device, &owner_key, now)
            .unwrap()
    );
    assert!(peer.governance().watching_devices().unwrap().is_empty());
    assert!(
        !peer
            .governance()
            .channel_readers(now, channel(&public), Some(&public))
            .unwrap()
            .unwrap()
            .contains(&device)
    );
    // A page gives back only what a page took in.
    assert!(
        !peer
            .governance_mut()
            .release_watching_device(&member.device_public_key, &owner_key, now)
            .unwrap()
    );
    assert!(peer.permissions(member.device_public_key, now).is_ok());
}
