use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::state::WorkspaceTree;
use ui::widget::Container;

fn host_with_two_workspaces() -> (ShellOverlayHost, crate::shell::chrome_state::Chrome) {
    let mut tree = WorkspaceTree::bootstrap_demo();
    // `bootstrap_demo` only seeds one workspace; `switch_down` is a
    // no-op with nothing below.
    tree.append_empty_workspace("Second");
    let chrome = ChromeState::new(tree);
    let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
        .with_chrome(chrome.clone());
    host.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
    (host, chrome)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn two_workspace_host_switches_without_animation_and_draws_only_active_room_peers() {
    /// Switching workspace must not turn on `needs_animation_frame` — a
    /// full-tree slide animation must not get reintroduced by accident.
    fn switching_workspace_animates_nothing() {
        let (h, chrome) = host_with_two_workspaces();
        assert!(
            !h.needs_animation_frame(),
            "an idle host must not be asking for frames"
        );

        chrome.borrow_mut().switch_workspace(1);
        assert!(
            !h.needs_animation_frame(),
            "a workspace switch must not put the shell into an animation \
             loop: the content changes, the screen does not move"
        );

        // And again on the way back — a switch in either direction is
        // the same non-event.
        chrome.borrow_mut().switch_workspace(-1);
        assert!(
            !h.needs_animation_frame(),
            "switching back must not animate either"
        );
    }

    /// The room a peer publishes under, put in and read back out through
    /// the host's own selection — not through a key the test rebuilt for
    /// itself, which would pass even if the two disagreed.
    fn real_peers_are_picked_up_by_room_and_only_the_active_one() {
        use ui::permissions::UserId;
        let (h, chrome) = host_with_two_workspaces();
        let (here, elsewhere) = {
            let c = chrome.borrow();
            let tree = c.tree();
            let active = tree.desktop.active;
            (
                tree.desktop
                    .workspaces
                    .iter()
                    .find(|w| w.id == active)
                    .expect("an active workspace")
                    .room
                    .to_string(),
                tree.desktop
                    .workspaces
                    .iter()
                    .find(|w| w.id != active)
                    .expect("a second workspace")
                    .room
                    .to_string(),
            )
        };
        assert_ne!(here, elsewhere, "two workspaces must be two rooms");

        let mut presence = PresenceCtx::empty();
        presence.set_pointer(
            &PresenceKey::Workspace { id: here },
            UserId("alice".into()),
            (120.0, 240.0),
        );
        presence.set_pointer(
            &PresenceKey::Workspace { id: elsewhere },
            UserId("bob".into()),
            (300.0, 400.0),
        );

        let visible = h.visible_peers(&presence);
        assert_eq!(
            visible.len(),
            1,
            "the peer in this room is drawn and the one in another is not, got {:?}",
            visible.iter().map(|p| &p.label).collect::<Vec<_>>(),
        );
        assert_eq!(visible[0].label, "alice");
        assert_eq!(visible[0].pos, (120.0, 240.0));
    }

    /// Two peers in the same room are drawn in distinct colours.
    fn several_peers_in_one_room_are_all_drawn_in_distinct_colours() {
        use ui::permissions::UserId;
        let (h, chrome) = host_with_two_workspaces();
        let room = {
            let c = chrome.borrow();
            let tree = c.tree();
            let active = tree.desktop.active;
            tree.desktop
                .workspaces
                .iter()
                .find(|w| w.id == active)
                .expect("an active workspace")
                .room
                .to_string()
        };
        let key = PresenceKey::Workspace { id: room };
        let mut presence = PresenceCtx::empty();
        // Two close enough to have stacked under a fixed-offset layout, and
        // two with names differing by one letter.
        for (who, at) in [
            ("alice", (400.0, 300.0)),
            ("bob", (404.0, 302.0)),
            ("dana", (120.0, 500.0)),
            ("danah", (700.0, 120.0)),
        ] {
            presence.set_pointer(&key, UserId(who.into()), at);
        }

        let visible = h.visible_peers(&presence);
        assert_eq!(visible.len(), 4, "every peer in the room must be drawn");

        for (i, a) in visible.iter().enumerate() {
            for b in &visible[i + 1..] {
                let (ac, bc) = (a.color.to_rgba8(), b.color.to_rgba8());
                let d = ui::presence::color_distance((ac.r, ac.g, ac.b), (bc.r, bc.g, bc.b));
                assert!(
                    d >= 60.0,
                    "{} and {} are drawn {d:.1} apart — too close to tell apart",
                    a.label,
                    b.label,
                );
            }
        }

        // And their names do not land on each other, however close their
        // pointers are.
        let requests: Vec<ui::presence::PillRequest> = visible
            .iter()
            .map(|p| ui::presence::PillRequest {
                at: p.pos,
                size: (60.0, 20.0),
            })
            .collect();
        let placed = ui::presence::layout_peer_pills(h.rect, &requests);
        for (i, a) in placed.iter().enumerate() {
            for b in &placed[i + 1..] {
                assert!(
                    a.rect.intersect(b.rect).is_zero_area(),
                    "two name pills overlap: {:?} and {:?}",
                    a.rect,
                    b.rect,
                );
            }
            assert!(
                a.rect.x0 >= h.rect.x0
                    && a.rect.y0 >= h.rect.y0
                    && a.rect.x1 <= h.rect.x1
                    && a.rect.y1 <= h.rect.y1,
                "a name pill at {:?} left the window {:?}",
                a.rect,
                h.rect,
            );
        }
    }

    switching_workspace_animates_nothing();
    real_peers_are_picked_up_by_room_and_only_the_active_one();
    several_peers_in_one_room_are_all_drawn_in_distinct_colours();
}
