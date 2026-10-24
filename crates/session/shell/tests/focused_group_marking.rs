//! The active-group pointer has to be visible, and in exactly one place.
//!
//! `WorkspaceTree` persists `active_strip` per workspace and `active_group`
//! per strip — deciding where a new window lands, what a scoped search
//! narrows to, and which tab takes the next keystroke. In a strip of
//! non-overlapping windows there is no z-order to read focus from, so
//! `ShellOverlayHost::mark_focused_group` derives it from `Widget::update`,
//! the per-frame path these tests drive:
//!
//!   1. The group the tree names is the group that reports focus.
//!   2. Exactly one group across every strip reports it.
//!   3. Moving the pointer moves the light, including across strips —
//!      focus is a pure function of the tree, never a latch.
//!   4. A strip whose pointer was never seeded still lights a window.
//!   5. A workspace with a single window lights nothing — with nothing to
//!      tell apart, the marker would be decoration.

use shell::shell::state::FreshId as _;
use std::collections::HashMap;

use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree,
};
use ui::widget::Widget;

/// Strip A holds two groups, strip B one: two in one strip makes "only the
/// active sibling lights" checkable, the second strip makes "focus follows
/// the active *strip*" checkable.
const G_A1: GroupId = 1001;
const G_A2: GroupId = 1002;
const G_B1: GroupId = 2001;

fn tree() -> (WorkspaceTree, StripId, StripId) {
    let ws_id = WorkspaceId::new();
    let s_a = StripId::new();
    let s_b = StripId::new();

    let mk_tab = |id: u64, label: &str| Tab {
        id,
        title: label.into(),
        tile: TileContent::Stub {
            kind: AppKind::Other("Test".into()),
            label: label.into(),
        },
        app_title: None,
    };
    let mk_group = |id: GroupId, tab: u64, label: &str| WindowGroup {
        id,
        tabs: vec![mk_tab(tab, label)],
        active_tab: tab,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };

    let strip_a = Strip {
        id: s_a,
        name: "StripA".into(),
        window_groups: vec![mk_group(G_A1, 11, "A1"), mk_group(G_A2, 12, "A2")],
    };
    let strip_b = Strip {
        id: s_b,
        name: "StripB".into(),
        window_groups: vec![mk_group(G_B1, 21, "B1")],
    };
    let ws = {
        let mut workspace = Workspace::new(ws_id, "Test");
        workspace.strips = vec![strip_a, strip_b];
        workspace
    };

    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, s_a);
    let mut active_group = HashMap::new();
    active_group.insert(s_a, G_A1);
    active_group.insert(s_b, G_B1);

    let t = WorkspaceTree {
        desktop: shell::shell::state::DesktopTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group,
            collaborators: vec![],
            ..Default::default()
        },
        ..Default::default()
    };
    (t, s_a, s_b)
}

/// Drive one production frame — `Widget::update` runs the host's per-frame
/// reconcilers, `mark_focused_group` among them.
fn tick(host: &mut ShellOverlayHost) {
    let doc = automerge::AutoCommit::new();
    let root = automerge::ROOT;
    host.update(&doc, &root);
}

/// Every `(strip, group)` pair currently reporting focus.
fn lit(host: &mut ShellOverlayHost, strips: [StripId; 2]) -> Vec<(StripId, GroupId)> {
    let mut out = Vec::new();
    for sid in strips {
        let Some(row) = host.group_row_mut_for_tests(sid) else {
            continue;
        };
        for g in &row.groups {
            if g.focused() {
                out.push((sid, g.id));
            }
        }
    }
    out
}

#[test]
fn the_focus_light_follows_the_pointer_and_only_shows_with_two_windows() {
    // Phase `the_tree_s_active_group_is_the_one_that_reports_focus`.
    {
        let (t, s_a, s_b) = tree();
        let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&t);
        tick(&mut host);

        assert_eq!(
            lit(&mut host, [s_a, s_b]),
            vec![(s_a, G_A1)],
            "the group named by active_strip + active_group is lit, and it is the only one",
        );
    }

    // Phase `moving_the_pointer_to_a_sibling_moves_the_light`.
    {
        let (t, s_a, s_b) = tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&t);
        tick(&mut host);
        assert_eq!(
            lit(&mut host, [s_a, s_b]),
            vec![(s_a, G_A1)],
            "precondition"
        );

        {
            let st = chrome.borrow();
            st.tree_mut().desktop.set_active_group(s_a, G_A2);
        }
        tick(&mut host);

        assert_eq!(
            lit(&mut host, [s_a, s_b]),
            vec![(s_a, G_A2)],
            "focus follows the pointer to the sibling group, and does not stay latched on the old one",
        );
    }

    // Phase `moving_the_pointer_to_another_strip_moves_the_light`.
    {
        let (t, s_a, s_b) = tree();
        let ws_id = t.desktop.active;
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&t);
        tick(&mut host);
        assert_eq!(
            lit(&mut host, [s_a, s_b]),
            vec![(s_a, G_A1)],
            "precondition"
        );

        {
            let st = chrome.borrow();
            st.tree_mut().desktop.active_strip.insert(ws_id, s_b);
        }
        tick(&mut host);

        assert_eq!(
            lit(&mut host, [s_a, s_b]),
            vec![(s_b, G_B1)],
            "a strip focus change moves the light across strips; strip A goes dark",
        );
    }

    // Phase `a_strip_with_no_recorded_pointer_still_lights_a_window`.
    {
        let (mut t, s_a, s_b) = tree();
        // A workspace can reach a frame with no active_group for a strip (fresh
        // boot, or a strip never clicked into); resolution must fall back rather
        // than leave the strip dark.
        t.desktop.active_group.remove(&s_a);
        let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&t);
        tick(&mut host);

        assert_eq!(
            lit(&mut host, [s_a, s_b]),
            vec![(s_a, G_A1)],
            "an unseeded strip falls back to its leftmost group; leaving every \
             window unlit is the state this signal exists to abolish",
        );
    }

    // Phase `a_workspace_with_one_window_lights_nothing`.
    {
        let (mut t, s_a, s_b) = tree();
        {
            let ws = &mut t.desktop.workspaces[0];
            ws.strips.retain(|s| s.id == s_a);
            ws.strips[0].window_groups.retain(|g| g.id == G_A1);
        }
        let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&t);
        tick(&mut host);

        assert_eq!(
            lit(&mut host, [s_a, s_b]),
            Vec::new(),
            "the only window on the workspace needs no marker telling the user it \
             is the one they are typing into",
        );
    }

    // Phase `a_second_window_anywhere_in_the_workspace_turns_the_marker_on`.
    {
        // One group per strip, two strips: nothing to disambiguate *within* a
        // strip, plenty across the workspace — the case a per-strip count gets wrong.
        let (mut t, s_a, s_b) = tree();
        {
            let ws = &mut t.desktop.workspaces[0];
            ws.strips[0].window_groups.retain(|g| g.id == G_A1);
        }
        let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&t);
        tick(&mut host);

        assert_eq!(
            lit(&mut host, [s_a, s_b]),
            vec![(s_a, G_A1)],
            "two strips holding one window each is exactly when the user cannot \
             tell which window the keyboard is aimed at",
        );
    }
}
