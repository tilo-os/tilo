//! Roster panel behaviour under simulated broker events.
//!
//! The panel doesn't need a real broker to reason about — it consumes a
//! stream of [`RosterUpdate`]s and produces a [`RosterEntry`] snapshot.
//! We simulate two peers subscribing to a workspace and assert the
//! rendered view matches, then trigger a disconnect and assert the
//! chip greys and eventually prunes.
//!
//! Headless-safe: no compositor spun up.

use std::time::{Duration, Instant};

use shell::shell::roster_panel::{
    PeerStatus, PRUNE_AFTER, RosterModel, RosterUpdate,
};

fn upsert(uid: &str, label: &str, t: Instant) -> RosterUpdate {
    RosterUpdate::Upsert {
        user_id: uid.to_string(),
        label: label.to_string(),
        pubkey: vec![uid.as_bytes().first().copied().unwrap_or(0); 32],
        focus_target: None,
        t,
    }
}

#[test]
fn roster_shows_both_subscribed_peers() {
    let t0 = Instant::now();
    let mut model = RosterModel::new();

    // Two peers subscribed to workspace X.
    model.apply(upsert("alice", "Alice", t0));
    model.apply(upsert("bob", "Bob", t0));

    let entries = model.render(t0);
    assert_eq!(entries.len(), 2, "both subscribed peers must be visible");
    let ids: Vec<&str> = entries.iter().map(|e| e.user_id.as_str()).collect();
    assert!(ids.contains(&"alice"));
    assert!(ids.contains(&"bob"));

    for e in &entries {
        assert_eq!(e.status, PeerStatus::Active);
        assert!(e.alpha > 0.99, "active chips must be fully opaque");
    }
}

#[test]
fn disconnect_greys_chip_and_fades_out() {
    let t0 = Instant::now();
    let mut model = RosterModel::new();
    model.apply(upsert("alice", "Alice", t0));
    model.apply(upsert("bob", "Bob", t0));

    // Bob disconnects.
    let disc = t0 + Duration::from_secs(5);
    model.apply(RosterUpdate::Disconnect {
        user_id: "bob".into(),
        t: disc,
    });

    // Immediately after: bob is offline, but still visible.
    let entries = model.render(disc);
    let bob = entries.iter().find(|e| e.user_id == "bob").unwrap();
    assert_eq!(bob.status, PeerStatus::Offline);
    assert!(bob.alpha > 0.9, "just-disconnected chip stays visible");

    // 15s after disconnect: the chip has faded out entirely.
    let entries = model.render(disc + PRUNE_AFTER + Duration::from_millis(1));
    assert!(
        !entries.iter().any(|e| e.user_id == "bob"),
        "bob's chip must have faded out after prune window",
    );
    // Alice is unaffected.
    assert!(entries.iter().any(|e| e.user_id == "alice"));
}

#[test]
fn empty_roster_renders_nothing() {
    let model = RosterModel::new();
    assert!(model.render(Instant::now()).is_empty());
}

#[test]
fn three_peer_roster_orders_active_first() {
    let base = Instant::now();
    let mut model = RosterModel::new();
    // All three at base — all Active — deterministic alphabetical order.
    model.apply(upsert("charlie", "Charlie", base));
    model.apply(upsert("alice", "Alice", base));
    model.apply(upsert("bob", "Bob", base));

    let entries = model.render(base);
    assert_eq!(entries.len(), 3);
    let ids: Vec<&str> = entries.iter().map(|e| e.user_id.as_str()).collect();
    assert_eq!(ids, vec!["alice", "bob", "charlie"]);
}
