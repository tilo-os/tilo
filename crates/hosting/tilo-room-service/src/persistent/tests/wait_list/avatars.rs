//! The avatars a public host publishes for its bridge: served, marked
//! served, and swept once nothing has asked for them for a month.

use super::*;

/// The avatars directory of an unstarted persistent room in a fresh
/// directory: the sweep only reads `Shared::dir`, never the relay child.
fn persistent() -> (Arc<Persistent>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let shared = Shared::with_key(
        dir.path().to_path_buf(),
        [7; 32],
        200,
        1000,
        (JoinRates::GUESTS, JoinRates::SPECTATORS),
    )
    .unwrap();
    (Persistent::new(Arc::new(shared)), dir)
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

#[test]
fn a_sweep_drops_the_avatars_nothing_served_for_a_month() {
    let (persistent, dir) = persistent();
    let avatars = dir.path().join("avatars");
    // Any name a publish can leave: 64 lowercase hex digits.
    let stale = "ab".repeat(32);
    let fresh = "cd".repeat(32);
    std::fs::write(avatars.join(&stale), PNG).unwrap();
    std::fs::write(avatars.join(&fresh), PNG).unwrap();
    // Not a name this host ever published: never swept.
    std::fs::write(avatars.join("notes.txt"), b"kept").unwrap();
    let month_ago = std::time::SystemTime::now()
        - tilo_relay_protocol::public_avatars::SWEEP_AGE
        - std::time::Duration::from_secs(60);
    std::fs::File::options()
        .write(true)
        .open(avatars.join(&stale))
        .unwrap()
        .set_modified(month_ago)
        .unwrap();
    // Serving a stale one marks it worn: a page fetched its picture.
    assert_eq!(
        persistent.avatar_path(&stale).as_deref(),
        Some(avatars.join(&stale).as_path())
    );
    persistent.avatar_served(&stale);
    assert_eq!(persistent.sweep_avatars(), 0);
    // Once that fetch is a month old too, only it goes.
    std::fs::File::options()
        .write(true)
        .open(avatars.join(&stale))
        .unwrap()
        .set_modified(month_ago)
        .unwrap();
    assert_eq!(persistent.sweep_avatars(), 1);
    assert!(!avatars.join(&stale).exists());
    assert!(avatars.join(&fresh).is_file());
    assert!(avatars.join("notes.txt").is_file());
    // A name that is no hash is not one to mark served either.
    persistent.avatar_served("../blobs/secret");
    assert!(persistent.avatar_path("../blobs/secret").is_none());
}
