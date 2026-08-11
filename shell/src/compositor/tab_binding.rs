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
    AppKind, GroupId, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth,
    WorkspaceTree, next_id,
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
    /// Its own window group, inserted immediately to the right of the
    /// group named here.
    ///
    /// [`Placement::NewGroup`] says "beside whatever has focus"; this
    /// says "beside *that* one". It is what
    /// [`crate::shell::placement::placement_for_new_window`] returns
    /// when the position was decided from something other than the
    /// focus pointer — a seam the user could see, in particular — so the
    /// answer survives focus moving between the launch and the window
    /// arriving.
    ///
    /// Falls back to [`Placement::NewGroup`] when the named group is
    /// gone, exactly as [`Placement::TabIn`] does.
    RightOf(GroupId),
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
/// app-id exactly as the tab's label is — that note was written when the
/// user asked, against the layout they were looking at.
///
/// A client we did not launch, or one whose app-id we cannot correlate,
/// gets the rule applied here instead
/// ([`crate::shell::placement::placement_for_new_window`]) with no
/// layout snapshot: an unrecognised window is still a window opening,
/// and it belongs beside — or inside — whatever the user is in, the same
/// as any other.
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
        .unwrap_or_else(|| crate::shell::placement::placement_for_new_window(tree, None));
    // If this window is the one a document open asked for, that is where
    // the document now lives — recorded here because this is where the
    // launch and the window it produced meet, the same correlation the
    // placement lookup above makes.
    if let Some(id) = app_id.as_deref() {
        crate::shell::doc_open::claim_surface_for_document(id, surface_id);
    }
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
    place_tab(tree, new_tab, placement)
}

/// Open a **shell-hosted** app — one the compositor draws in-process
/// (Notes, Editor, Files, Terminal) rather than one that arrives as a
/// foreign Wayland client — at `placement`.
///
/// The sibling of [`adopt_toplevel_at`]: the same placement rule and the
/// same insertion, around a different kind of tile. A hosted app has no
/// client process, so there is no toplevel to adopt and no launch note to
/// correlate against — the caller applies
/// [`crate::shell::placement::placement_for_new_window`] itself and hands
/// the answer in.
///
/// Returns the freshly-minted tab id. The tile is a
/// [`TileContent::AppPlaceholder`], which is what
/// [`crate::shell::to_ui_tile`] resolves into the live app widget.
pub fn open_hosted_app(
    tree: &mut WorkspaceTree,
    kind: AppKind,
    title: impl Into<String>,
    placement: Placement,
) -> TabId {
    let title = title.into();
    let new_tab = Tab {
        id: next_id(),
        title: title.clone(),
        tile: TileContent::AppPlaceholder { kind, label: title },
    };
    place_tab(tree, new_tab, placement)
}

/// Splice an already-built [`Tab`] into the active workspace at
/// `placement`, and focus it.
///
/// Where a window lands is a property of the placement and the tree, not
/// of what is inside the window — so both the foreign-toplevel adoption
/// and the hosted-app open route through here rather than each growing
/// their own copy of the strip walk.
///
/// Returns `new_tab`'s id, including on the paths that could not splice
/// it (no active workspace), so callers always have the id they minted.
pub fn place_tab(tree: &mut WorkspaceTree, new_tab: Tab, placement: Placement) -> TabId {
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

    // A `TabIn` whose group is gone degrades to a new group rather than
    // silently landing somewhere arbitrary.
    let tab_target = match placement {
        Placement::TabIn(g) => strip.window_groups.iter().position(|w| w.id == g),
        Placement::NewGroup | Placement::RightOf(_) => None,
    };

    if let Some(idx) = tab_target {
        let group = &mut strip.window_groups[idx];
        // Immediately right of the tab the user was on, not at the end
        // of the bar: the new window is the next thing along from what
        // they were looking at, and a tab bar reads left to right.
        let after = group
            .tabs
            .iter()
            .position(|t| t.id == group.active_tab)
            .map(|i| i + 1)
            .unwrap_or(group.tabs.len());
        group.tabs.insert(after, new_tab);
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
    // Immediately right of the group the placement named — the one the
    // rule picked out, falling back to the focused one when that group
    // is gone, and to the end of the strip when nothing is focused
    // either.
    let anchor = match placement {
        Placement::RightOf(g) => Some(g).filter(|g| {
            strip.window_groups.iter().any(|w| w.id == *g)
        }),
        _ => None,
    }
    .or(active_group);
    let insert_at = anchor
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

// Every test here drives `launcher`'s process-wide pending-placement
// table, so they are serialised on the crate's one group for global
// state — against each other and against everything else that stages a
// launch (`shell::doc_open`'s opens, most of `shell::launcher`'s own
// tests). Clearing the table at the top of each test only protects a
// test from its predecessors, not from one running beside it.
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

    /// The tab the adoption of `surface_id` created, wherever placement
    /// put it. Same reason as [`group_of`]: never `tabs.last()`, which
    /// only happens to be the new tab when the rule appends.
    fn tab_of(tree: &WorkspaceTree, surface_id: u64) -> &crate::shell::state::Tab {
        group_of(tree, surface_id)
            .tabs
            .iter()
            .find(|t| {
                matches!(&t.tile, TileContent::Foreign { surface_id: s, .. } if *s == surface_id)
            })
            .expect("the group holding the surface holds its tab")
    }

    /// Cut the bootstrap strip's group down to the one window it is
    /// showing, and focus it.
    ///
    /// Rules 1 and 2 differ on exactly one thing — whether the selection
    /// is one tab of several — so a fixture for either has to say which
    /// it is. `bootstrap_default` ships a four-tab group, which is the
    /// rule-2 shape; this is the rule-1 one.
    fn focus_lone_window(tree: &mut WorkspaceTree) -> (StripId, GroupId) {
        let strip_id = tree.workspaces[0].strips[0].id;
        let group = &mut tree.workspaces[0].strips[0].window_groups[0];
        group.tabs.truncate(1);
        group.active_tab = group.tabs[0].id;
        let group_id = group.id;
        tree.active_strip.insert(tree.active, strip_id);
        tree.set_active_group(strip_id, group_id);
        (strip_id, group_id)
    }

    /// A window opened while a single window has focus is a new thing
    /// beside it: its own column, immediately to the right.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_launch_beside_a_lone_window_opens_its_own_group_to_the_right() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_default();
        let (strip_id, focused) = focus_lone_window(&mut tree);
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
        let tab = tab_of(&tree, 4242);
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

    /// Selection inside a group of several tabs means the person is
    /// working *in* that group, so a window opened from there joins it as
    /// a tab — immediately right of the tab they were on — rather than
    /// splitting the layout into another column.
    #[test]
    fn a_launch_from_inside_a_tabbed_group_becomes_the_next_tab_in_it() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_default();
        let strip_id = tree.workspaces[0].strips[0].id;
        // `bootstrap_default`'s group holds four tabs. Select the second
        // one, so "immediately right of the selected tab" is a position
        // distinct from both ends of the bar.
        let selected_idx = 1usize;
        let (group_id, selected_tab, tab_ids_before) = {
            let g = &mut tree.workspaces[0].strips[0].window_groups[0];
            assert!(g.tabs.len() > 2, "fixture: a genuinely tabbed group");
            g.active_tab = g.tabs[selected_idx].id;
            (
                g.id,
                g.active_tab,
                g.tabs.iter().map(|t| t.id).collect::<Vec<_>>(),
            )
        };
        tree.active_strip.insert(tree.active, strip_id);
        tree.set_active_group(strip_id, group_id);
        let groups_before = tree.workspaces[0].strips[0].window_groups.len();

        let tab_id = adopt_toplevel(&mut tree, 4343, Some("org.example.joins".into()));

        let strip = &tree.workspaces[0].strips[0];
        assert_eq!(
            strip.window_groups.len(),
            groups_before,
            "opening from inside a tabbed group must not grow the strip"
        );
        assert_eq!(
            group_of(&tree, 4343).id,
            group_id,
            "the new window joins the group the selection was in"
        );
        let now: Vec<u64> = strip.window_groups[0].tabs.iter().map(|t| t.id).collect();
        let mut expected = tab_ids_before.clone();
        expected.insert(selected_idx + 1, tab_id);
        assert_eq!(
            now, expected,
            "the new tab sits immediately right of the selected one \
             ({selected_tab:?}), with every other tab in its original order"
        );
        assert_eq!(
            strip.window_groups[0].active_tab, tab_id,
            "the window you just opened is the one you're looking at"
        );
    }

    /// The new column goes immediately *right of the focused group*, not
    /// at the end of the strip — so the window you just opened is next
    /// to the one you opened it from and the strip doesn't scroll away.
    #[test]
    #[serial_test::serial(process_env)]
    fn the_new_group_is_inserted_beside_the_focused_group_not_at_the_end() {
        use crate::shell::state::{WindowGroup, WindowWidth, next_id};
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_default();
        let (_strip_id, focused) = focus_lone_window(&mut tree);
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
        let last_before = tree.workspaces[0].strips[0].window_groups[2].id;

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
    #[serial_test::serial(process_env)]
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
    #[serial_test::serial(process_env)]
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
    #[serial_test::serial(process_env)]
    fn a_recorded_placement_is_consumed_by_the_matching_adoption() {
        use crate::shell::launcher;
        use crate::shell::state::{WindowGroup, WindowWidth, next_id};
        launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_default();
        let (strip_id, selected) = focus_lone_window(&mut tree);
        // A second group the note can name — somewhere the rule would
        // never send a window, since the selection is elsewhere.
        let named = {
            let g = WindowGroup {
                id: next_id(),
                tabs: vec![],
                active_tab: 0,
                width: WindowWidth::Normal,
                maximized: false,
            };
            let id = g.id;
            tree.workspaces[0].strips[0].window_groups.push(g);
            id
        };
        let groups_before = tree.workspaces[0].strips[0].window_groups.len();

        launcher::set_pending_placement("dev.qos.placefixture", Placement::TabIn(named));
        // The client announces the bare trailing component, as the
        // reverse-DNS convention has it.
        let _ = adopt_toplevel(&mut tree, 8080, Some("placefixture".into()));
        assert_eq!(
            tree.workspaces[0].strips[0].window_groups.len(),
            groups_before,
            "the recorded TabIn placement must reach the adoption"
        );
        assert_eq!(
            group_of(&tree, 8080).id,
            named,
            "and it must reach the group the note named"
        );
        let named_tabs = group_of(&tree, 8080).tabs.len();

        // Adopting moved the selection onto the window it just opened —
        // it has to, that is the window the user is now looking at. Put
        // the selection back on their own lone window, which is the state
        // a second launch would actually happen in.
        tree.set_active_group(strip_id, selected);

        // The note belongs to that first window: a *second* window from
        // the same client is a new thing again, and takes the rule.
        let _ = adopt_toplevel(&mut tree, 8081, Some("placefixture".into()));
        assert_eq!(
            tree.workspaces[0].strips[0].window_groups.len(),
            groups_before + 1,
            "a claimed placement must not apply to the next window too"
        );
        assert_eq!(
            tree.workspaces[0].strips[0]
                .window_groups
                .iter()
                .find(|g| g.id == named)
                .expect("the named group is still there")
                .tabs
                .len(),
            named_tabs,
            "the retired note must not have pulled the second window in too"
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
    #[serial_test::serial(process_env)]
    fn both_trees_place_the_same_toplevel_the_same_way() {
        use crate::shell::launcher;
        launcher::clear_pending_placements();

        let mut protocol = WorkspaceTree::bootstrap_default();
        // A lone selected window, so the rule's answer (a column beside
        // it) differs from the note's (a tab inside it) — without that
        // the two placements coincide and the test proves nothing.
        let (_strip_id, opener) = focus_lone_window(&mut protocol);
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
    #[serial_test::serial(process_env)]
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
    #[serial_test::serial(process_env)]
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
    #[serial_test::serial(process_env)]
    fn rename_tab_for_app_id_rewrites_anon_placeholder() {
        let mut tree = WorkspaceTree::bootstrap_default();
        // First adopt without an app-id so the tab gets the
        // `anon-<surface_id>` placeholder.
        let _ = adopt_toplevel(&mut tree, 99, None);
        let renamed = rename_tab_for_app_id(&mut tree, 99, "org.example.late".into());
        assert!(renamed, "rename should report it updated a tab");
        let tab = tab_of(&tree, 99);
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
    #[serial_test::serial(process_env)]
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
    #[serial_test::serial(process_env)]
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
    #[serial_test::serial(process_env)]
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
        assert_eq!(tab_of(&tree, 31338).title, "Adopt Fixture");
    }

    /// A shell-hosted app opened beside a lone window is a window, not a
    /// tab: it takes the column to the right, and what lands in it is a
    /// tile that resolves to a live app widget rather than a stub card.
    ///
    /// That second half is the point of the whole path — a window group
    /// holding a placeholder nothing renders would look identical in the
    /// tree and blank on screen.
    #[test]
    fn a_hosted_app_opened_beside_a_lone_window_takes_the_column_to_its_right() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let (strip_id, focused) = focus_lone_window(&mut tree);
        let groups_before = tree.workspaces[0].strips[0].window_groups.len();

        let placement = crate::shell::placement::placement_for_new_window(&tree, None);
        assert_eq!(
            placement,
            Placement::RightOf(focused),
            "a lone selected window puts the next window beside it"
        );
        let tab_id = open_hosted_app(&mut tree, AppKind::FileExplorer, "Files", placement);

        let strip = &tree.workspaces[0].strips[0];
        assert_eq!(
            strip.window_groups.len(),
            groups_before + 1,
            "the hosted app opens its own window group"
        );
        assert_eq!(
            strip.window_groups[0].id, focused,
            "the window that was selected stays where it was"
        );
        let opened = &strip.window_groups[1];
        assert_eq!(
            opened.tabs.len(),
            1,
            "the new group holds exactly the app that was opened"
        );
        assert_eq!(opened.tabs[0].id, tab_id, "returned id names the new tab");
        assert_eq!(opened.active_tab, tab_id, "the new window takes focus");
        assert_eq!(
            opened.id,
            tree.active_group_for(strip_id).unwrap(),
            "and so does its group"
        );
        assert_eq!(opened.tabs[0].title, "Files");
        match &opened.tabs[0].tile {
            TileContent::AppPlaceholder { kind, label } => {
                assert_eq!(*kind, AppKind::FileExplorer);
                assert_eq!(label, "Files");
            }
            other => panic!("expected an AppPlaceholder tile, got {other:?}"),
        }
        assert_eq!(
            crate::shell::resolve_tile_shape_for_test(&opened.tabs[0].tile),
            "App",
            "a hosted app's tile has to come online as a live widget, not a stub card"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn anonymous_app_id_falls_back_to_surface_synthetic() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let _ = adopt_toplevel(&mut tree, 7, None);
        let tab = tab_of(&tree, 7);
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
