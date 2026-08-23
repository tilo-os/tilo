//! The active-group pointer has to be visible, and it has to be visible
//! in exactly one place.
//!
//! `WorkspaceTree` has always persisted `active_strip` per workspace and
//! `active_group` per strip. That pair decides where a new window lands,
//! what a scoped search narrows to, and which tab takes the next
//! keystroke — but nothing on screen was derived from it. In a strip of
//! non-overlapping windows there is no z-order to read focus from, so
//! three side-by-side windows looked identical while only one of them
//! would receive what the user typed.
//!
//! `ShellOverlayHost::mark_focused_group` closes that gap, and it runs
//! from `Widget::update` — the real per-frame path, which is what these
//! tests drive. What they pin:
//!
//!   1. The group the tree names is the group that reports focus.
//!   2. Exactly one group across every strip reports it, so a second
//!      lit window can never appear.
//!   3. Moving the pointer moves the light, including across strips —
//!      focus is a pure function of the tree, never a latch.
//!   4. A strip whose pointer was never seeded still lights a window.
//!      "No window is lit" is the state the signal exists to abolish, so
//!      an unset pointer must not be able to reproduce it.

use std::collections::HashMap;

use shell::shell::build_nirios_overlay_host_with_chrome;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree,
};
use ui::widget::Widget;

/// Strip A holds two groups, strip B one. Two groups in one strip is
/// what makes "only the active sibling lights" checkable; a second strip
/// is what makes "focus follows the active *strip*" checkable.
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
    };
    let mk_group = |id: GroupId, tab: u64, label: &str| WindowGroup {
        id,
        tabs: vec![mk_tab(tab, label)],
        active_tab: tab,
        width: WindowWidth::Normal,
        maximized: false,
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
    let ws = Workspace {
        strips: vec![strip_a, strip_b],
        ..Workspace::test_default(ws_id, "Test")
    };

    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, s_a);
    let mut active_group = HashMap::new();
    active_group.insert(s_a, G_A1);
    active_group.insert(s_b, G_B1);

    let t = WorkspaceTree {
        workspaces: vec![ws],
        active: ws_id,
        active_strip,
        active_group,
        collaborators: vec![],
        ..Default::default()
    };
    (t, s_a, s_b)
}

/// Drive one production frame: `Widget::update` is where the host runs
/// its per-frame reconcilers, `mark_focused_group` among them.
fn tick(host: &mut ShellOverlayHost) {
    let mut doc = automerge::AutoCommit::new();
    let root = automerge::ROOT;
    host.update(&mut doc, &root);
}

/// Every `(strip, group)` pair currently reporting focus.
fn lit(host: &mut ShellOverlayHost, strips: [StripId; 2]) -> Vec<(StripId, GroupId)> {
    let mut out = Vec::new();
    for sid in strips {
        let Some(row) = host.group_row_mut_for_tests(sid) else {
            continue;
        };
        for g in row.groups.iter() {
            if g.focused() {
                out.push((sid, g.id));
            }
        }
    }
    out
}

#[test]
fn the_tree_s_active_group_is_the_one_that_reports_focus() {
    let (t, s_a, s_b) = tree();
    let (mut host, _chrome) = build_nirios_overlay_host_with_chrome(&t);
    tick(&mut host);

    assert_eq!(
        lit(&mut host, [s_a, s_b]),
        vec![(s_a, G_A1)],
        "the group named by active_strip + active_group is lit, and it is the only one",
    );
}

#[test]
fn moving_the_pointer_to_a_sibling_moves_the_light() {
    let (t, s_a, s_b) = tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&t);
    tick(&mut host);
    assert_eq!(lit(&mut host, [s_a, s_b]), vec![(s_a, G_A1)], "precondition");

    {
        let st = chrome.borrow();
        st.tree_mut().set_active_group(s_a, G_A2);
    }
    tick(&mut host);

    assert_eq!(
        lit(&mut host, [s_a, s_b]),
        vec![(s_a, G_A2)],
        "focus follows the pointer to the sibling group, and does not stay latched on the old one",
    );
}

#[test]
fn moving_the_pointer_to_another_strip_moves_the_light() {
    let (t, s_a, s_b) = tree();
    let ws_id = t.active;
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&t);
    tick(&mut host);
    assert_eq!(lit(&mut host, [s_a, s_b]), vec![(s_a, G_A1)], "precondition");

    {
        let st = chrome.borrow();
        st.tree_mut().active_strip.insert(ws_id, s_b);
    }
    tick(&mut host);

    assert_eq!(
        lit(&mut host, [s_a, s_b]),
        vec![(s_b, G_B1)],
        "a strip focus change moves the light across strips; strip A goes dark",
    );
}

#[test]
fn a_strip_with_no_recorded_pointer_still_lights_a_window() {
    let (mut t, s_a, s_b) = tree();
    // A workspace can reach a frame with no active_group recorded for a
    // strip: a fresh boot, or a strip the user has never clicked into.
    // Resolution has to fall back rather than leave the strip dark.
    t.active_group.remove(&s_a);
    let (mut host, _chrome) = build_nirios_overlay_host_with_chrome(&t);
    tick(&mut host);

    assert_eq!(
        lit(&mut host, [s_a, s_b]),
        vec![(s_a, G_A1)],
        "an unseeded strip falls back to its leftmost group; leaving every \
         window unlit is the state this signal exists to abolish",
    );
}
