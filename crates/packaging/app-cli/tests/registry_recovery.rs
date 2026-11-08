//! What a registry is to a device that has already read it: a catalogue whose
//! identities and sequences are agreed on, not a page that can change freely.
//!
//! A registry is a service with a disk, and a disk can be lost. A host rebuilt
//! from a backup serves a catalogue signed by the right owner and genuinely
//! complete — just from before this device last visited. Accepting it would
//! walk the catalogue backwards, so a device refuses it, says so, and keeps
//! what it already accepted; when the restored registry publishes past that,
//! the device takes its catalogue again.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::net::TcpListener;

use ed25519_dalek::SigningKey;
use registry_types::{
    PublisherAuthorization, RegistryEntry, SignedRegistrySnapshot, sign_snapshot,
};
use tilo_app_package::pack::SigningIdentity;

use crate::registry_to_installed::{
    Device, Registry, author_tapp_version, registry_config, signed_entry_version, test_publisher,
};

const PACKAGE_ID: &str = "so.tilo.catalogue-recovery";

/// One published release: the entry naming the bytes `Registry` serves from
/// `url`, and those bytes.
fn release(version: &str, publisher: &SigningIdentity, url: &str) -> (RegistryEntry, Vec<u8>) {
    let tapp = author_tapp_version(PACKAGE_ID, version, publisher);
    (signed_entry_version(PACKAGE_ID, version, url, &tapp), tapp)
}

/// The signed catalogue that names `entry`, at the sequence after `previous`.
fn catalogue(
    entry: RegistryEntry,
    owner: &SigningKey,
    previous: Option<&SignedRegistrySnapshot>,
) -> SignedRegistrySnapshot {
    let now = tilo_clock::try_unix_now().unwrap();
    let mut snapshot = SignedRegistrySnapshot {
        protocol: registry_types::PACKAGE_FORMAT_VERSION,
        sequence: previous.map_or(1, |previous| previous.sequence.saturating_add(1)),
        issued_at_unix: now,
        expires_at_unix: now.saturating_add(3600),
        previous_snapshot_hash_hex: previous
            .map(|previous| registry_types::snapshot_hash(previous).unwrap())
            .unwrap_or_default(),
        moderation_head_hex: String::new(),
        publishers: vec![PublisherAuthorization {
            pubkey_hex: entry.publisher_pubkey_hex.clone(),
            can_add_new: true,
        }],
        entries: vec![entry],
        owner_signature_hex: String::new(),
        channel_authorization: None,
    };
    sign_snapshot(&mut snapshot, owner).unwrap();
    snapshot
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one registry lifetime: published, lost, rebuilt from a backup, published again"
)]
fn a_registry_rebuilt_from_a_backup_cannot_walk_the_catalogue_backwards() {
    let publisher = test_publisher();
    let owner = SigningKey::from_bytes(&[12; 32]);

    // A loopback port the releases will name, checked free first.
    let probe = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let tapp_url = format!("http://127.0.0.1:{port}/sheets.tapp");

    // The registry before its disk is lost: 1.4.0, then 1.5.0.
    let (older, tapp_1_4) = release("1.4.0", &publisher, &tapp_url);
    let first = catalogue(older, &owner, None);
    let (newer, tapp_1_5) = release("1.5.0", &publisher, &tapp_url);
    let second = catalogue(newer, &owner, Some(&first));
    let live = Registry::start(serde_json::to_string(&second).unwrap(), tapp_1_5);

    let device = Device::new(
        Some(registry_config("local", &live.base_url(), &owner)),
        &[],
    );

    // The device reads the newest catalogue, checkpointing sequence 2.
    let listing = app_cli::available();
    assert!(listing.unreachable.is_empty(), "{listing:?}");
    assert!(
        listing
            .apps
            .iter()
            .any(|app| app.package_id == PACKAGE_ID && app.version == "1.5.0"),
        "the live registry's catalogue must be on offer: {listing:?}"
    );

    // The host and its volume are lost. What comes back is the snapshot the
    // backup captured — sequence 1, with the 1.4.0 package beside it.
    let restored = Registry::start(serde_json::to_string(&first).unwrap(), tapp_1_4);
    device.point_at(registry_config("local", &restored.base_url(), &owner));

    // Asking the restored registry directly names the regression, rather
    // than reading a catalogue that goes backwards.
    let refused =
        app_cli::client::fetch_entries(&registry_config("local", &restored.base_url(), &owner))
            .expect_err("a catalogue behind the accepted one must be refused");
    let behind = refused
        .behind_accepted()
        .expect("the refusal names the regression, not a generic failure");
    assert_eq!(behind.held_sequence, 2);
    assert_eq!(behind.served_sequence, 1);

    // The list says so, and carries nothing from the older catalogue — not
    // the 1.5.0 it already held, and not the 1.4.0 now offered.
    let listing = app_cli::available();
    assert!(
        !listing.apps.iter().any(|app| app.package_id == PACKAGE_ID),
        "nothing from a catalogue that goes backwards may be offered: {listing:?}"
    );
    let said = listing
        .unreachable
        .iter()
        .find(|registry| registry.name == "local")
        .expect("the registry that answered behind is reported, not silent");
    assert!(
        said.detail.contains("sequence 1") && said.detail.contains("sequence 2"),
        "the reason names both sequences: {}",
        said.detail
    );

    // What the device already accepted still describes an install: the
    // recovery of the registry is refused, never the catalogue this device
    // verified, which resolves 1.5.0 — not the 1.4.0 the restored host offers.
    let kept = app_cli::client::last_verified_entries(&registry_config(
        "local",
        &restored.base_url(),
        &owner,
    ))
    .expect("the catalogue this device accepted must still verify");
    assert_eq!(
        kept.iter()
            .filter(|entry| entry.entry.package_id == PACKAGE_ID)
            .map(|entry| entry.entry.versions[0].version.as_str())
            .collect::<Vec<_>>(),
        ["1.5.0"],
        "the kept catalogue is the one this device accepted, not the recovered one"
    );

    // The restored registry publishes again, past what the device holds, and
    // its catalogue is taken normally: nothing stays stuck behind a recovery.
    let (onward, tapp_1_6) = release("1.6.0", &publisher, &tapp_url);
    let third = catalogue(onward, &owner, Some(&second));
    let moved_on = Registry::start(serde_json::to_string(&third).unwrap(), tapp_1_6);
    app_cli::client::fetch_entries(&registry_config("local", &moved_on.base_url(), &owner))
        .expect("a restored registry that publishes past the checkpoint is taken again");
    device.point_at(registry_config("local", &moved_on.base_url(), &owner));
    let listing = app_cli::available();
    assert!(listing.unreachable.is_empty(), "{listing:?}");
    assert!(
        listing
            .apps
            .iter()
            .any(|app| app.package_id == PACKAGE_ID && app.version == "1.6.0"),
        "the newer catalogue is offered after recovery: {listing:?}"
    );
}
