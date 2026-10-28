//! A peer's pointer stays while it is in the room's windows.
use super::*;

/// presence.md: a still pointer stays while it is in the room's
/// windows; only its name steps aside. It goes when its person takes it
/// away, however its tile moves meanwhile, and comes back when they
/// point again. It stays in the room: the anchor is kept for the roster
/// until then.
#[test]
fn a_still_peers_pointer_stays_until_it_is_taken_away() {
    use vello::kurbo::Rect;
    let room = "3f2b1c00-0000-4000-8000-000000000001";
    let key = PresenceKey::Workspace { id: room.into() };
    let bob = ui::permissions::UserId("peer@7".into());
    let mut presence = PresenceCtx::default();
    let mut anchored = AnchoredPointers::new();
    let t0 = Instant::now();
    let heard = |anchored: &mut AnchoredPointers, presence: &mut PresenceCtx, at| {
        fold_inbound(
            vec![arrived(7, room, (30.0, 20.0))],
            anchored,
            presence,
            &unnamed,
            &any_room,
            at,
        )
    };
    let fresh = heard(&mut anchored, &mut presence, t0);
    let here = [(TAB, Rect::new(100.0, 50.0, 700.0, 700.0))];
    assert!(place_pointers(&anchored, &fresh, &mut presence, &here, t0));

    // A minute still while the desk scrolls: the pointer rides the tile,
    // whole, and its name has stepped aside.
    let later = t0 + Duration::from_secs(60);
    let scrolled = [(TAB, Rect::new(40.0, 50.0, 640.0, 700.0))];
    place_pointers(&anchored, &[], &mut presence, &scrolled, later);
    assert_eq!(presence.pointer_for(&key, &bob), Some((70.0, 70.0)));
    assert!(
        presence.pointer_opacity(&key, &bob, later, POINTER_STAYS, ui::presence::POINTER_FADE)
            >= 1.0,
        "a still pointer does not fade"
    );
    assert!(
        presence.pointer_name_opacity(&key, &bob, later, ui::presence::POINTER_FADE) <= 0.0,
        "its name does"
    );

    // Taken away from the room's windows: gone at once.
    let mut gone = arrived(7, room, (0.0, 0.0));
    gone.cleared = true;
    fold_inbound(
        vec![gone],
        &mut anchored,
        &mut presence,
        &unnamed,
        &any_room,
        later,
    );
    assert_eq!(presence.pointer_for(&key, &bob), None, "taken away");
    assert!(!anchored.contains_key("peer@7"));

    let back = later + Duration::from_secs(1);
    let fresh = heard(&mut anchored, &mut presence, back);
    assert!(place_pointers(
        &anchored,
        &fresh,
        &mut presence,
        &scrolled,
        back
    ));
    assert_eq!(presence.pointer_for(&key, &bob), Some((70.0, 70.0)));
}
