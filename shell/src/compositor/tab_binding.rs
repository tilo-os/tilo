//! Adopt foreign Wayland clients into the active workspace's tab tree.
//!
//! Phase 6 (Task 6.1 + 6.2) of
//! `docs/superpowers/plans/2026-05-02-real-wayland-compositor.md`.
//!
//! When a client commits its first `xdg_toplevel`, the compositor needs to
//! splice a new tab into the workspace tree so the chrome paints a tab pill
//! and the frame loop knows which surface to blit into the tile rect. The
//! reverse path — destroyed surface → tab close — also lives here.
//!
//! The two public entry points,
//! [`adopt_toplevel`] and [`close_tab_for_destroyed_surface`], take a
//! `surface_id: u64` (the wayland-server `WlSurface::id().protocol_id()`
//! widened) rather than an `&WlSurface`. That's a deliberate testability
//! refactor: smithay's `WlSurface` cannot be constructed without a running
//! `Display` + client, so unit tests would otherwise need a full server
//! runtime. The compositor's `XdgShellHandler` impl performs the
//! `WlSurface → u64` conversion at the call-site (see
//! `compositor::state::State::new_toplevel`).

use crate::shell::state::{
    GroupId, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth, WorkspaceTree,
    next_id,
};

/// Where a newly-adopted toplevel should land.
///
/// The spatial model is `Workspace → Strip → WindowGroup → Tab`: tabs
/// tile inside a group, groups scroll horizontally along a strip. So the
/// choice between "new group" and "new tab" is the choice between "this
/// is a new thing" and "this belongs with what I was already doing", and
/// it is worth making on purpose rather than inheriting from whatever
/// the tree walk reached first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Its own window group, inserted immediately to the right of the
    /// active group in the active strip.
    ///
    /// This is what a **launch** gets. Opening an app is starting a new
    /// task, and the strip is the axis tasks live on — a launch that
    /// became a tab in whatever group happened to be focused would hide
    /// the window the user just asked for behind another app's tab bar,
    /// one keystroke after they asked for it. Inserting to the right of
    /// the focused group (rather than at the end of the strip) keeps the
    /// new window next to the one it was opened from, so the strip does
    /// not scroll away from the user on every launch.
    NewGroup,
    /// A tab inside an existing group.
    ///
    /// This is what a **document opened from another app** gets — the
    /// file explorer opening a spreadsheet, a search result opening the
    /// file it matched. The document and the app that asked for it are
    /// one task, and a tab group in qOS is precisely the primitive for
    /// "these arbitrary apps belong together". It also keeps the strip
    /// from growing a column every time the user peeks at a file.
    ///
    /// Falls back to [`Placement::NewGroup`] when the named group no
    /// longer exists (the opener was closed while its document was
    /// starting).
    TabIn(GroupId),
}

impl Default for Placement {
    fn default() -> Self {
        Placement::NewGroup
    }
}

/// Adopt a freshly-committed wayland toplevel into the active workspace's
/// tab tree, at the placement the launch asked for.
///
/// `surface_id` is the wayland-server protocol id of the `wl_surface`
/// (widened to `u64`). `app_id` may be `None` if the client hasn't called
/// `set_app_id` yet — in that case a synthetic placeholder app-id of the
/// form `anon-<surface_id>` is recorded; [`rename_adopted_toplevel`]
/// rewrites it when the client eventually sets one.
///
/// Returns the freshly-minted tab id.
///
/// The placement comes from whichever launch this client belongs to
/// ([`crate::shell::launcher::placement_for_toplevel`]), matched by
/// app-id exactly as the tab's label is. A client we did not launch — or
/// one whose app-id we cannot correlate — gets [`Placement::NewGroup`],
/// the same as a launched app: an unrecognised window is still a new
/// thing, and burying it as a tab inside somebody else's group is the
/// worse of the two guesses.
///
/// When the active workspace has no strips, a default strip (with one
/// window group) is synthesized so the new tab always lands somewhere
/// visible. The previous behaviour returned a synthetic id without
/// mutating the tree — the foreign surface was then alive at the
/// protocol layer but invisible to the chrome, leaking buffer + shm-pool
/// memory until the client tore the toplevel down on its own.
pub fn adopt_toplevel(tree: &mut WorkspaceTree, surface_id: u64, app_id: Option<String>) -> TabId {
    let placement = app_id
        .as_deref()
        .and_then(|id| crate::shell::launcher::placement_for_toplevel(id, surface_id))
        .unwrap_or_default();
    adopt_toplevel_at(tree, surface_id, app_id, placement)
}

/// [`adopt_toplevel`] with the placement supplied explicitly. Split out
/// so the placement rule is assertable without staging a launch.
pub fn adopt_toplevel_at(
    tree: &mut WorkspaceTree,
    surface_id: u64,
    app_id: Option<String>,
    placement: Placement,
) -> TabId {
    let resolved_id = app_id.unwrap_or_else(|| format!("anon-{}", surface_id));
    let new_tab = Tab {
        id: next_id(),
        title: tab_label_for_foreign(&resolved_id),
        tile: TileContent::Foreign {
            app_id: resolved_id,
            surface_id,
        },
    };
    let tab_id = new_tab.id;

    // Resolve the focused strip + group *before* taking the mutable
    // workspace borrow: both live on the tree, not on the workspace.
    let Some(ws_id) = tree.active_workspace().map(|w| w.id) else {
        return tab_id;
    };
    let active_strip = tree.active_strip.get(&ws_id).copied();
    let active_group = active_strip.and_then(|s| tree.active_group_for(s));

    let Some(ws) = tree.active_workspace_mut() else {
        return tab_id;
    };

    // No strips at all: synthesize one around the new tab. Mirrors
    // `apply_workspace_drop`'s empty-strip path.
    if ws.strips.is_empty() {
        let group = WindowGroup {
            id: next_id(),
            tabs: vec![new_tab],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
        };
        let strip = Strip {
            id: StripId::from(next_id()),
            name: String::new(),
            window_groups: vec![group],
        };
        let strip_id = strip.id;
        let group_id = strip.window_groups[0].id;
        ws.strips.push(strip);
        tree.active_strip.insert(ws_id, strip_id);
        tree.set_active_group(strip_id, group_id);
        return tab_id;
    }

    // Target strip: the focused one, else the first. A recorded pointer
    // to a strip that has since been closed falls back the same way.
    let strip_idx = active_strip
        .and_then(|id| ws.strips.iter().position(|s| s.id == id))
        .unwrap_or(0);
    let strip = &mut ws.strips[strip_idx];
    let strip_id = strip.id;

    // A `TabIn` whose group is gone degrades to `NewGroup` rather than
    // silently landing somewhere arbitrary.
    let tab_target = match placement {
        Placement::TabIn(g) => strip.window_groups.iter().position(|w| w.id == g),
        Placement::NewGroup => None,
    };

    if let Some(idx) = tab_target {
        let group = &mut strip.window_groups[idx];
        group.tabs.push(new_tab);
        group.active_tab = tab_id;
        let group_id = group.id;
        tree.active_strip.insert(ws_id, strip_id);
        tree.set_active_group(strip_id, group_id);
        return tab_id;
    }

    let group = WindowGroup {
        id: next_id(),
        tabs: vec![new_tab],
        active_tab: tab_id,
        width: WindowWidth::Normal,
        maximized: false,
    };
    let group_id = group.id;
    // Immediately right of the focused group; at the end when nothing is
    // focused or the pointer is stale.
    let insert_at = active_group
        .and_then(|g| strip.window_groups.iter().position(|w| w.id == g))
        .map(|i| i + 1)
        .unwrap_or(strip.window_groups.len());
    strip.window_groups.insert(insert_at, group);
    tree.active_strip.insert(ws_id, strip_id);
    tree.set_active_group(strip_id, group_id);
    tab_id
}

/// Rewrite the `app_id` (and the tab `title`) of a previously-adopted
/// `TileContent::Foreign` tab to match a freshly-received
/// `xdg_toplevel::set_app_id` request.
///
/// The client may call `set_app_id` *after* `xdg_surface::get_toplevel`
/// fires [`adopt_toplevel`], in which case the original adoption recorded a
/// synthetic `anon-<surface_id>` placeholder. This helper walks the
/// workspace tree, finds the foreign tab whose `surface_id` matches, and
/// rewrites both the recorded `app_id` and the tab's display title.
///
/// Returns `true` when a matching tab was found and rewritten; `false`
/// otherwise (e.g. the toplevel was never adopted into the active
/// workspace, or it has since been destroyed).
pub fn rename_tab_for_app_id(
    tree: &mut WorkspaceTree,
    surface_id: u64,
    new_app_id: String,
) -> bool {
    let new_title = tab_label_for_foreign(&new_app_id);
    for ws in tree.workspaces.iter_mut() {
        for strip in ws.strips.iter_mut() {
            for group in strip.window_groups.iter_mut() {
                for tab in group.tabs.iter_mut() {
                    if let TileContent::Foreign {
                        surface_id: s,
                        app_id,
                    } = &mut tab.tile
                    {
                        if *s == surface_id {
                            *app_id = new_app_id;
                            tab.title = new_title;
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// Resolve the window group that owns the `TileContent::Foreign` tab bound to
/// `surface_id`, returning its [`GroupId`]. Used by the compositor's
/// `xdg_toplevel.set_fullscreen` path to find which group a fullscreen request
/// should promote. Returns `None` when the surface was never adopted (e.g. a
/// fullscreen request that races surface teardown).
pub fn group_id_for_surface(tree: &WorkspaceTree, surface_id: u64) -> Option<GroupId> {
    for ws in tree.workspaces.iter() {
        for strip in ws.strips.iter() {
            for group in strip.window_groups.iter() {
                if group.tabs.iter().any(|t| {
                    matches!(
                        &t.tile,
                        TileContent::Foreign { surface_id: s, .. } if *s == surface_id
                    )
                }) {
                    return Some(group.id);
                }
            }
        }
    }
    None
}

/// Walk the persisted tree looking for a `TileContent::Foreign` tab whose
/// `surface_id` matches `destroyed_surface_id`; remove it from its group
/// when found and return `true`. Phase 6 (Task 6.2) of the compositor
/// plan: surface destroy → tab close.
///
/// Returns `false` when no matching tab exists (e.g. the surface was
/// never adopted, or the tab was already removed via another path).
/// The widget tree handles close animation; the data layer just records
/// absence.
pub fn close_tab_for_destroyed_surface(
    tree: &mut WorkspaceTree,
    destroyed_surface_id: u64,
) -> bool {
    for ws in tree.workspaces.iter_mut() {
        for strip in ws.strips.iter_mut() {
            for group in strip.window_groups.iter_mut() {
                if let Some(pos) = group.tabs.iter().position(|t| {
                    matches!(
                        &t.tile,
                        TileContent::Foreign { surface_id: s, .. }
                            if *s == destroyed_surface_id
                    )
                }) {
                    let removed = group.tabs.remove(pos);
                    // If the removed tab was the active one, fall back to
                    // the new last tab (or leave `active_tab` pointing at
                    // a stale id — the chrome's resolver tolerates that
                    // and picks the first existing tab on next frame).
                    if group.active_tab == removed.id {
                        group.active_tab = group.tabs.last().map(|t| t.id).unwrap_or(0);
                    }
                    return true;
                }
            }
        }
    }
    false
}

/// Turn a wayland app-id into a tab-bar friendly label.
///
/// If this session launched the package the client belongs to, the
/// installed app's name wins — a tab for an app the user opened by name
/// in the palette should carry that name. Otherwise we fall back to the
/// trailing app-id component, which is what most apps would want shown
/// anyway: `org.example.MyApp` → `MyApp`.
fn tab_label_for_foreign(app_id: &str) -> String {
    if let Some(name) = crate::shell::launcher::display_name_for_app_id(app_id) {
        return name;
    }
    let trimmed = app_id.split('.').next_back().unwrap_or(app_id);
    if trimmed.len() > 32 {
        trimmed[..32].to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::WorkspaceTree;

    /// The group holding the foreign tab for `surface_id`, wherever
    /// placement put it. Tests assert on the tab they adopted rather
    /// than on a fixed index, so a placement change shows up as a
    /// placement assertion failing and not as five unrelated ones.
    fn group_of(tree: &WorkspaceTree, surface_id: u64) -> &crate::shell::state::WindowGroup {
        tree.workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .flat_map(|s| s.window_groups.iter())
            .find(|g| {
                g.tabs.iter().any(|t| {
                    matches!(&t.tile, TileContent::Foreign { surface_id: s, .. } if *s == surface_id)
                })
            })
            .expect("the adopted surface must live in some group")
    }

    /// A launch is a new task, so its window is a new column beside the
    /// focused one — never a tab buried in whatever group happened to
    /// have focus.
    #[test]
    fn a_launched_app_lands_in_its_own_group_beside_the_focused_one() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_default();
        let strip_id = tree.workspaces[0].strips[0].id;
        let focused = tree.workspaces[0].strips[0].window_groups[0].id;
        tree.active_strip.insert(tree.active, strip_id);
        tree.set_active_group(strip_id, focused);
        let groups_before = tree.workspaces[0].strips[0].window_groups.len();
        let tabs_in_focused = tree.workspaces[0].strips[0].window_groups[0].tabs.len();

        let tab_id = adopt_toplevel(&mut tree, 4242, Some("org.example.app".into()));

        let strip = &tree.workspaces[0].strips[0];
        assert_eq!(
            strip.window_groups.len(),
            groups_before + 1,
            "a launch opens its own window group"
        );
        assert_eq!(
            strip.window_groups[0].tabs.len(),
            tabs_in_focused,
            "the focused group must be left alone — a launch is not a tab in it"
        );
        assert_eq!(
            strip.window_groups[1].tabs.len(),
            1,
            "the new group holds exactly the new window"
        );
        assert_eq!(
            strip.window_groups[1].id,
            tree.active_group_for(strip_id).unwrap(),
            "the newly-opened group takes focus"
        );
        let group = group_of(&tree, 4242);
        let tab = group.tabs.last().unwrap();
        assert_eq!(tab.id, tab_id, "returned tab_id matches the adopted tab");
        assert_eq!(group.active_tab, tab_id, "newly-adopted tab gains focus");
        match &tab.tile {
            TileContent::Foreign { app_id, surface_id } => {
                assert_eq!(app_id, "org.example.app");
                assert_eq!(*surface_id, 4242);
            }
            other => panic!("expected Foreign tile, got {:?}", other),
        }
    }

    /// The new column goes immediately *right of the focused group*, not
    /// at the end of the strip — so the window you just opened is next
    /// to the one you opened it from and the strip doesn't scroll away.
    #[test]
    fn the_new_group_is_inserted_beside_the_focused_group_not_at_the_end() {
        use crate::shell::state::{WindowGroup, WindowWidth, next_id};
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_default();
        let strip_id = tree.workspaces[0].strips[0].id;
        // Two more groups after the first, so "beside" and "at the end"
        // are distinguishable positions.
        for _ in 0..2 {
            tree.workspaces[0].strips[0]
                .window_groups
                .push(WindowGroup {
                    id: next_id(),
                    tabs: vec![],
                    active_tab: 0,
                    width: WindowWidth::Normal,
                    maximized: false,
                });
        }
        let focused = tree.workspaces[0].strips[0].window_groups[0].id;
        let last_before = tree.workspaces[0].strips[0].window_groups[2].id;
        tree.active_strip.insert(tree.active, strip_id);
        tree.set_active_group(strip_id, focused);

        let _ = adopt_toplevel(&mut tree, 5150, Some("org.example.beside".into()));

        let strip = &tree.workspaces[0].strips[0];
        assert_eq!(strip.window_groups.len(), 4);
        assert_eq!(
            strip.window_groups[0].id, focused,
            "focused group stays put"
        );
        assert_eq!(
            strip.window_groups[1].id,
            group_of(&tree, 5150).id,
            "the new group sits immediately right of the focused one, \
             not appended after {last_before:?}"
        );
        assert_eq!(
            strip.window_groups[3].id, last_before,
            "the previously-last group is pushed right, not replaced"
        );
    }

    /// A document opened from another app is the same task as the app
    /// that asked for it, so it joins that app's group as a tab.
    #[test]
    fn a_document_opened_from_an_app_becomes_a_tab_in_that_apps_group() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_default();
        let strip_id = tree.workspaces[0].strips[0].id;
        let opener = tree.workspaces[0].strips[0].window_groups[0].id;
        tree.active_strip.insert(tree.active, strip_id);
        tree.set_active_group(strip_id, opener);
        let groups_before = tree.workspaces[0].strips[0].window_groups.len();
        let tabs_before = tree.workspaces[0].strips[0].window_groups[0].tabs.len();

        let tab_id = adopt_toplevel_at(
            &mut tree,
            6060,
            Some("org.example.editor".into()),
            Placement::TabIn(opener),
        );

        let strip = &tree.workspaces[0].strips[0];
        assert_eq!(
            strip.window_groups.len(),
            groups_before,
            "opening a document must not grow the strip by a column"
        );
        assert_eq!(
            strip.window_groups[0].tabs.len(),
            tabs_before + 1,
            "the document joins the opener's group as a tab"
        );
        assert_eq!(group_of(&tree, 6060).id, opener);
        assert_eq!(
            strip.window_groups[0].active_tab, tab_id,
            "the opened document is the tab you're looking at"
        );
    }

    /// The opener can be gone by the time the document's window maps.
    /// That degrades to the launch placement rather than dropping the
    /// window into an arbitrary group.
    #[test]
    fn a_tab_placement_naming_a_dead_group_falls_back_to_a_new_group() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_default();
        let strip_id = tree.workspaces[0].strips[0].id;
        let focused = tree.workspaces[0].strips[0].window_groups[0].id;
        tree.active_strip.insert(tree.active, strip_id);
        tree.set_active_group(strip_id, focused);
        let groups_before = tree.workspaces[0].strips[0].window_groups.len();

        let _ = adopt_toplevel_at(
            &mut tree,
            7070,
            Some("org.example.orphan".into()),
            // A group id that never existed in this tree.
            Placement::TabIn(999_999_999),
        );

        assert_eq!(
            tree.workspaces[0].strips[0].window_groups.len(),
            groups_before + 1,
            "a placement naming a group that is gone opens a new one"
        );
        assert_eq!(group_of(&tree, 7070).tabs.len(), 1);
    }

    /// The launch-time note and the adoption are two events on two
    /// processes; this is the whole correlation, driven end to end.
    #[test]
    fn a_recorded_placement_is_consumed_by_the_matching_adoption() {
        use crate::shell::launcher;
        launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_default();
        let strip_id = tree.workspaces[0].strips[0].id;
        let opener = tree.workspaces[0].strips[0].window_groups[0].id;
        tree.active_strip.insert(tree.active, strip_id);
        tree.set_active_group(strip_id, opener);
        let groups_before = tree.workspaces[0].strips[0].window_groups.len();

        launcher::set_pending_placement("dev.qos.placefixture", Placement::TabIn(opener));
        // The client announces the bare trailing component, as the
        // reverse-DNS convention has it.
        let _ = adopt_toplevel(&mut tree, 8080, Some("placefixture".into()));
        assert_eq!(
            tree.workspaces[0].strips[0].window_groups.len(),
            groups_before,
            "the recorded TabIn placement must reach the adoption"
        );

        // The note belongs to that window: a *second* window from the
        // same client is a new thing again.
        let _ = adopt_toplevel(&mut tree, 8081, Some("placefixture".into()));
        assert_eq!(
            tree.workspaces[0].strips[0].window_groups.len(),
            groups_before + 1,
            "a claimed placement must not apply to the next window too"
        );
        launcher::clear_pending_placements();
    }

    /// The compositor adopts every toplevel **twice** — once into the
    /// protocol-side tree `XdgShellHandler` mutates, once into the
    /// rendered tree `reconcile_foreign_tabs` mirrors it onto, which is
    /// the one the chrome paints. Both adoptions have to reach the same
    /// answer, or the window is a tab in one tree and a column in the
    /// other and the user sees the wrong one.
    ///
    /// A note that were consumed by the first reader would leave the
    /// rendered tree on the default placement — the bug this pins.
    #[test]
    fn both_trees_place_the_same_toplevel_the_same_way() {
        use crate::shell::launcher;
        launcher::clear_pending_placements();

        let mut protocol = WorkspaceTree::bootstrap_default();
        let strip_id = protocol.workspaces[0].strips[0].id;
        let opener = protocol.workspaces[0].strips[0].window_groups[0].id;
        let active = protocol.active;
        protocol.active_strip.insert(active, strip_id);
        protocol.set_active_group(strip_id, opener);
        // The rendered tree is a clone of the persisted one, so the two
        // share group ids — which is what makes a placement naming a
        // group meaningful across both.
        let mut rendered = protocol.clone();
        let groups_before = rendered.workspaces[0].strips[0].window_groups.len();

        launcher::set_pending_placement("dev.qos.twotrees", Placement::TabIn(opener));

        // Protocol side adopts first, then the rendered tree mirrors it.
        let _ = adopt_toplevel(&mut protocol, 4711, Some("twotrees".into()));
        let _ = adopt_toplevel(&mut rendered, 4711, Some("twotrees".into()));

        assert_eq!(
            rendered.workspaces[0].strips[0].window_groups.len(),
            groups_before,
            "the rendered tree must honour the same TabIn placement the \
             protocol tree did, not fall back to opening a column"
        );
        assert_eq!(
            protocol.workspaces[0].strips[0].window_groups.len(),
            groups_before,
            "fixture sanity: the protocol tree honoured it too"
        );
        launcher::clear_pending_placements();
    }

    #[test]
    fn group_id_for_surface_resolves_the_owning_group() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_default();
        let _ = adopt_toplevel(&mut tree, 4243, Some("org.example.app".into()));
        let expected = group_of(&tree, 4243).id;
        assert_eq!(
            group_id_for_surface(&tree, 4243),
            Some(expected),
            "a fullscreen request for surface 4243 resolves to its owning group",
        );
        // An unknown surface resolves to nothing — a fullscreen request that
        // raced surface teardown is a safe no-op.
        assert_eq!(group_id_for_surface(&tree, 9999), None);
    }

    #[test]
    fn close_tab_for_destroyed_surface_removes_matching_foreign_tab() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_default();
        let _ = adopt_toplevel(&mut tree, 42, Some("io.example.thing".into()));
        let owner = group_of(&tree, 42).id;
        let before = group_of(&tree, 42).tabs.len();
        let removed = close_tab_for_destroyed_surface(&mut tree, 42);
        assert!(removed, "close should report it removed a tab");
        let after = tree
            .workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .flat_map(|s| s.window_groups.iter())
            .find(|g| g.id == owner)
            .expect("the owning group still exists")
            .tabs
            .len();
        assert_eq!(after, before - 1, "tab count must drop by exactly one");
        let still_there = tree
            .workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .flat_map(|s| s.window_groups.iter())
            .flat_map(|g| g.tabs.iter())
            .any(|t| matches!(&t.tile, TileContent::Foreign { surface_id: 42, .. }));
        assert!(
            !still_there,
            "the foreign tab with surface_id=42 must be gone"
        );
    }

    #[test]
    fn rename_tab_for_app_id_rewrites_anon_placeholder() {
        let mut tree = WorkspaceTree::bootstrap_default();
        // First adopt without an app-id so the tab gets the
        // `anon-<surface_id>` placeholder.
        let _ = adopt_toplevel(&mut tree, 99, None);
        let renamed = rename_tab_for_app_id(&mut tree, 99, "org.example.late".into());
        assert!(renamed, "rename should report it updated a tab");
        let group = group_of(&tree, 99);
        let tab = group.tabs.last().unwrap();
        match &tab.tile {
            TileContent::Foreign { app_id, surface_id } => {
                assert_eq!(*surface_id, 99);
                assert_eq!(app_id, "org.example.late");
            }
            other => panic!("expected Foreign tile, got {:?}", other),
        }
        assert_eq!(tab.title, "late", "title is the trailing app-id component");
    }

    #[test]
    fn rename_tab_for_app_id_returns_false_for_unknown_surface() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let renamed = rename_tab_for_app_id(&mut tree, 12345, "x".into());
        assert!(
            !renamed,
            "rename must report `false` when the surface isn't adopted"
        );
    }

    /// C-6 regression: when the active workspace has no strips, a
    /// foreign-toplevel adoption used to be silently dropped — the
    /// function returned a synthetic tab id without touching the tree,
    /// orphaning the surface (alive at the protocol layer but never
    /// painted). Post-fix, the adoption synthesizes a default strip +
    /// group so the new tab is always reachable.
    #[test]
    fn adopt_toplevel_creates_strip_when_workspace_empty() {
        let mut tree = WorkspaceTree::bootstrap_default();
        // Start from a known-good tree and erase the active workspace's
        // strips so we exercise the new branch. Other workspaces (if
        // any) are untouched.
        let active_id = tree.active;
        {
            let ws = tree
                .workspaces
                .iter_mut()
                .find(|w| w.id == active_id)
                .expect("active workspace exists");
            ws.strips.clear();
            assert!(ws.strips.is_empty(), "test fixture: strips emptied");
        }

        let tab_id = adopt_toplevel(&mut tree, 7777, Some("io.example.late".into()));
        assert_ne!(tab_id, 0, "adoption must mint a real tab id");

        let ws = tree
            .workspaces
            .iter()
            .find(|w| w.id == active_id)
            .expect("active workspace still present");
        assert_eq!(
            ws.strips.len(),
            1,
            "C-6 regression: adoption into an empty-strip workspace must \
             synthesize a default strip rather than dropping the tab"
        );
        let strip = &ws.strips[0];
        assert_eq!(strip.window_groups.len(), 1);
        let group = &strip.window_groups[0];
        assert_eq!(group.tabs.len(), 1);
        assert_eq!(group.tabs[0].id, tab_id);
        assert_eq!(group.active_tab, tab_id);
        match &group.tabs[0].tile {
            TileContent::Foreign { app_id, surface_id } => {
                assert_eq!(*surface_id, 7777);
                assert_eq!(app_id, "io.example.late");
            }
            other => panic!("expected Foreign tile, got {other:?}"),
        }
    }

    /// A toplevel from a package this session launched is labelled with
    /// the installed app's name, not the raw wayland app-id. Drives the
    /// production `adopt_toplevel` path — the same call the compositor's
    /// `XdgShellHandler::new_toplevel` makes.
    ///
    /// The launch history is process-global, so the fixture uses an id
    /// no other test registers.
    #[test]
    fn adopted_tab_takes_the_launched_app_name() {
        crate::shell::launcher::remember_launch(&crate::shell::launcher::InstalledApp {
            package_id: "dev.qos.adoptfixture".into(),
            display_name: "Adopt Fixture".into(),
            category: None,
            version: "0.1.0".into(),
            manifest_path: std::path::PathBuf::new(),
            payload_root: std::path::PathBuf::new(),
            has_gui: true,
            opens_extensions: Vec::new(),
        });
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_default();
        // The client announces the bare app-id — the trailing component
        // of the package id, which is the reverse-DNS convention.
        let tab_id = adopt_toplevel(&mut tree, 31337, Some("adoptfixture".into()));
        let tab = group_of(&tree, 31337)
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .expect("adopted tab present");
        assert_eq!(
            tab.title, "Adopt Fixture",
            "a launched app's tab carries the app's name"
        );
        // A late `set_app_id` re-resolves through the same rule.
        let _ = adopt_toplevel(&mut tree, 31338, None);
        assert!(rename_tab_for_app_id(
            &mut tree,
            31338,
            "dev.qos.adoptfixture".into()
        ));
        let renamed = group_of(&tree, 31338)
            .tabs
            .last()
            .expect("renamed tab present");
        assert_eq!(renamed.title, "Adopt Fixture");
    }

    #[test]
    fn anonymous_app_id_falls_back_to_surface_synthetic() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let _ = adopt_toplevel(&mut tree, 7, None);
        let tab = group_of(&tree, 7).tabs.last().unwrap();
        match &tab.tile {
            TileContent::Foreign { app_id, surface_id } => {
                assert_eq!(*surface_id, 7);
                assert_eq!(
                    app_id, "anon-7",
                    "anonymous toplevels get a synthetic anon-<surface_id> placeholder"
                );
            }
            other => panic!("expected Foreign tile, got {:?}", other),
        }
    }
}
