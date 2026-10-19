//! An app's presence reaches the room's live plane under the person's name.
//! The collaboration broker publishes it from its own runtime's thread, on
//! every presence frame an app sends (Settings sends one as it opens). On a
//! desktop whose identity secret lives in the Secret Service, asking for the
//! name loaded that secret over D-Bus from inside the broker's runtime: the
//! blocking call panicked and the session aborted.

use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

use ed25519_dalek::SigningKey;

use super::{Reach, ShellNetwork};
use crate::shell::state::{RoomId, WorkspaceTree};

/// `primary.json` as a desktop with a Secret Service keeps it: public
/// fields here, the signing share in the store.
fn secret_service_identity(config: &std::path::Path) {
    let dir = config.join("tilo/identity");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let key = SigningKey::from_bytes(&[37; 32]);
    let record = serde_json::json!({
        "display_name": "Ada",
        "avatar_color": [10, 20, 30],
        "threshold_identity": {
            "single_device": true,
            "group_public_key": key.verifying_key().to_bytes(),
            "threshold": 1_u16,
            "total_shares": 1_u16,
            "share_index": 1_u16,
            "secret_bytes": [],
        },
        "created_at": "2026-10-03T00:00:00Z",
        "schema_version": 2,
        "secret_ref": {
            "backend": "secret-service-v1",
            "locator": hex::encode(key.verifying_key().to_bytes()),
        },
    });
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dir.join("primary.json"))
        .unwrap()
        .write_all(&serde_json::to_vec(&record).unwrap())
        .unwrap();
}

#[test]
#[serial_test::serial]
fn presence_published_on_the_brokers_runtime_names_the_person_without_the_secret_store() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    secret_service_identity(home.config());
    // A session bus that is not there: the old path reached for it.
    let no_bus = format!("unix:path={}", home.runtime().join("no-bus").display());
    home.env()
        .set("DBUS_SESSION_BUS_ADDRESS", no_bus)
        .remove("TILO_USER_IDENTITY")
        .remove("TILO_IDENTITY_BROKER_FD");

    let dir = tempfile::tempdir().unwrap();
    let mut tree = WorkspaceTree::default();
    let workspace = tree.append_empty_workspace("Home");
    let room: RoomId = tree.room_of(workspace).unwrap();
    let network = ShellNetwork::spawn_with(
        tree.shared(),
        &dir.path().join("device-private/keypair.bin"),
        &dir.path().join("peer-sync"),
        Reach::Loopback,
        |_, _| None,
    )
    .unwrap();

    // The broker's thread drives a current-thread runtime; the publish runs
    // inside it, as an app's presence call does.
    let broker_like = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let participant = broker_like.block_on(async {
        crate::shell::collab_outbound::PeerPublishSink::app_live_presence(
            &network,
            "so.tilo.settings",
            &room.to_string(),
            "",
            &collab_types::PeerPresence {
                key: postcard::to_stdvec(&ui::presence::PresenceKey::pointer_default()).unwrap(),
                payload: collab_types::PeerPresencePayload::Pointer { x: 0.5, y: 0.5 },
            },
        );
        network.live_participant()
    });
    assert_eq!(participant.label, "Ada");
    drop(network);
    drop(home);
}

/// A person is called by their own name in the rooms they are in; without
/// one, "Guest", which the people there tell apart by number and colour.
/// Never by a device's name ("Primary device") nor by a key: the name is
/// all a room, a public one above all, learns of the person.
#[test]
fn a_person_without_a_name_is_a_guest_never_their_device() {
    assert_eq!(super::shared_person_name(Some("Ada".into())), "Ada");
    assert_eq!(super::shared_person_name(Some("  Ada  ".into())), "Ada");
    let guest = crate::shell::i18n::t("presence-guest-name");
    assert_eq!(guest, "Guest");
    assert_eq!(super::shared_person_name(None), guest);
    assert_eq!(super::shared_person_name(Some("   ".into())), guest);
    // Two guests in one room are told apart by number, as any shared name.
    let labels =
        ui::presence::ParticipantLabels::new([("a", guest.as_str()), ("b", guest.as_str())]);
    assert_ne!(labels.label("a", &guest), labels.label("b", &guest));
}
