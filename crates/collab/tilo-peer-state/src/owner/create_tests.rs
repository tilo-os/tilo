//! A workspace this device founds is a room its own host keeps: it exists
//! from the moment it is made, and it comes back from its record.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]
use crate::owner::open_file;
use crate::owner::testing;
use ed25519_dalek::SigningKey;
use std::time::SystemTime;

#[test]
fn a_founded_workspace_is_held_here_and_restores_from_its_record() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("rooms.json");
    let key = SigningKey::from_bytes(&[12; 32]);
    let device = testing::certificate(&SigningKey::from_bytes(&[11; 32]), &key, "A's desktop");
    let now = SystemTime::now();
    let owner = open_file(&path).unwrap();
    let room = owner
        .create_workspace("Founders", &device, &key, now)
        .unwrap();
    let (hosted, workspace, name) = owner
        .with_current(room, |view| {
            Ok((
                view.hosted_here(),
                view.workspace(),
                view.peer().governance().room_name(),
            ))
        })
        .unwrap();
    assert!(hosted, "a founded workspace is the room its own host keeps");
    assert_eq!(
        Some(workspace),
        tilo_relay_protocol::room_workspace_id(&room.to_string()).ok()
    );
    assert_eq!(name.as_deref(), Some("Founders"));
    let stored = owner.storage();
    assert!(stored.holds(room));
    drop(owner);

    // It comes back from the record it published: founded here, the room
    // holds neither an enrollment nor a vouch, and needs neither.
    let reopened = open_file(&path).unwrap();
    assert_eq!(
        reopened
            .with_current(room, |view| Ok((
                view.hosted_here(),
                view.peer().governance().room_name()
            )))
            .unwrap(),
        (true, Some("Founders".to_owned()))
    );
}

#[test]
fn a_workspace_is_founded_by_its_own_device_with_a_name() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let owner = open_file(&directory.path().join("rooms.json")).unwrap();
    let key = SigningKey::from_bytes(&[13; 32]);
    let device = testing::certificate(&SigningKey::from_bytes(&[14; 32]), &key, "A's desktop");
    let other = SigningKey::from_bytes(&[15; 32]);
    let now = SystemTime::now();
    assert_eq!(
        owner
            .create_workspace("  ", &device, &key, now)
            .unwrap_err(),
        "workspace name must contain 1–80 characters"
    );
    assert_eq!(
        owner
            .create_workspace("Founders", &device, &other, now)
            .unwrap_err(),
        "a workspace is founded with its own device's key"
    );
}
