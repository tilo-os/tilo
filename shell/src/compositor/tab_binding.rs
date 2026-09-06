//! Adopt foreign Wayland clients into the active workspace's tab tree.
//!
//! On a client's first `xdg_toplevel` commit, splices a new tab into the workspace tree so
//! chrome paints a pill and the frame loop blits into the tile rect; the reverse path
//! (destroyed surface → tab close) also lives here. Entry points take a `surface_id: u64`
//! ([`crate::compositor::state::surface_id`], composing connection index + protocol id — a bare
//! protocol id collides across clients) rather than an `&WlSurface`, so unit tests don't need a running `Display`.

use crate::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth,
    WorkspaceTree, next_id,
};

/// Where a newly-adopted toplevel should land — a new window group or a tab in an existing one — decided on purpose rather than inherited from wherever the tree walk landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Placement {
    /// Its own window group, right of the active group in the active strip. What a **launch** gets:
    /// a new task, not a tab hidden in whatever group happens to be focused; inserted beside the focused group so the strip doesn't scroll away on every launch.
    #[default]
    NewGroup,
    /// A tab inside an existing group. What a **document opened from another app** gets, so the
    /// strip doesn't grow a column every peek. Falls back to [`Placement::NewGroup`] when the named group no longer exists.
    TabIn(GroupId),
    /// Its own window group, right of the group named here. Unlike [`Placement::NewGroup`] ("beside
    /// whatever has focus"), names a specific group, so the answer survives focus moving between launch and arrival. Falls back to [`Placement::NewGroup`] when gone, as [`Placement::TabIn`] does.
    RightOf(GroupId),
    /// Into a tab that already exists, replacing whatever it holds. What a launch on behalf of a
    /// peer's window asks for ([`crate::shell::peer_windows`]): the tab already arrived in the room document. Falls back to [`Placement::NewGroup`] when the tab is gone by arrival.
    IntoTab(TabId),
}

/// Adopt a freshly-committed wayland toplevel into the active workspace's tab tree, at the
/// placement the launch asked for.
///
/// `surface_id` is the wayland-server protocol id of the `wl_surface` (widened to `u64`).
/// `app_id` may be `None` before `set_app_id`; a synthetic `anon-<surface_id>` placeholder is
/// recorded until [`rename_tab_for_app_id`] rewrites it. Placement comes from the launch
/// matching this client's app-id, falling back to
/// [`crate::shell::placement::placement_for_new_window`] when unrecognised.
///
/// When the active workspace has no strips, a default strip is synthesized so the tab always
/// lands visible — otherwise the foreign surface leaks buffer + shm-pool memory until the client tears the toplevel down on its own.
pub fn adopt_toplevel(tree: &mut WorkspaceTree, surface_id: u64, app_id: Option<String>) -> TabId {
    let pending = app_id
        .as_deref()
        .and_then(|id| crate::shell::launcher::placement_and_instance_for_toplevel(id, surface_id));
    let placement = pending
        .map(|(placement, _)| placement)
        .unwrap_or_else(|| crate::shell::placement::placement_for_new_window(tree, None));
    // If this window is what a document open asked for, record that here — the launch and the window it produced meet at this point.
    if let Some(id) = app_id.as_deref() {
        crate::shell::doc_open::claim_surface_for_document(id, surface_id);
    }
    let tab = adopt_toplevel_at(tree, surface_id, app_id, placement);
    if matches!(placement, Placement::IntoTab(_)) {
        tree.claim_continuation_tab(tab);
    }
    if let Some((_, instance)) = pending
        && let Some(instance) = instance
        && let Ok(mut reports) =
            crate::shell::personal_continuity_registry::global_registry().lock()
    {
        reports.bind_tab(
            crate::shell::personal_continuity::AppInstanceId(tab),
            crate::shell::personal_continuity::AppInstanceId(instance),
        );
    }
    tab
}

/// [`adopt_toplevel`] with the placement supplied explicitly, so the placement rule is assertable without staging a launch.
pub fn adopt_toplevel_at(
    tree: &mut WorkspaceTree,
    surface_id: u64,
    app_id: Option<String>,
    placement: Placement,
) -> TabId {
    let resolved_id = app_id.unwrap_or_else(|| format!("anon-{surface_id}"));
    // A window started *for* an existing tab is bound into it rather than placed beside it, before the tab is minted.
    if let Placement::IntoTab(target) = placement
        && bind_into_tab(tree, target, &resolved_id, surface_id)
    {
        return target;
    }
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

/// Point the existing tab `target` at `surface_id`, and bring it to the front of its group.
/// Returns whether the tab was found; `false` means it's gone since the launch started, and the
/// caller places the window the ordinary way. Searches the whole tree, not the active workspace — the tab arrived from a peer, whose target workspace the launch didn't know.
fn bind_into_tab(tree: &mut WorkspaceTree, target: TabId, app_id: &str, surface_id: u64) -> bool {
    for ws in &mut tree.workspaces {
        for strip in &mut ws.strips {
            let strip_id = strip.id;
            for group in &mut strip.window_groups {
                let Some(tab) = group.tabs.iter_mut().find(|t| t.id == target) else {
                    continue;
                };
                tab.tile = TileContent::Foreign {
                    app_id: app_id.to_string(),
                    surface_id,
                };
                // Front the tab, but move focus no further: stealing the strip because somebody else opened something is exactly what the room model rules out.
                group.active_tab = target;
                let _ = strip_id;
                return true;
            }
        }
    }
    false
}

/// Open a **shell-hosted** app — drawn in-process (Notes, Editor, Files, Terminal), not a
/// foreign Wayland client — at `placement`.
///
/// The sibling of [`adopt_toplevel_at`] for a different tile kind: no client process, so no
/// toplevel or launch note to correlate — the caller applies
/// [`crate::shell::placement::placement_for_new_window`] itself. Tile is a
/// [`TileContent::AppPlaceholder`], resolved by [`crate::shell::to_ui_tile`].
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

/// Splice an already-built [`Tab`] into the active workspace at `placement`, and focus it.
/// Placement and tree decide where a window lands, not what's inside it, so both foreign and
/// hosted-app opens route through here. Returns `new_tab`'s id even when it can't be spliced (no active workspace).
pub fn place_tab(tree: &mut WorkspaceTree, new_tab: Tab, placement: Placement) -> TabId {
    let tab_id = new_tab.id;

    // Resolve the focused strip + group *before* taking the mutable workspace borrow: both live on the tree, not on the workspace.
    let Some(ws_id) = tree.active_workspace().map(|w| w.id) else {
        return tab_id;
    };
    let active_strip = tree.active_strip.get(&ws_id).copied();
    let active_group = active_strip.and_then(|s| tree.active_group_for(s));

    let Some(ws) = tree.active_workspace_mut() else {
        return tab_id;
    };

    // No strips at all: synthesize one around the new tab. Mirrors `apply_workspace_drop`'s empty-strip path.
    if ws.strips.is_empty() {
        let group = WindowGroup {
            id: next_id(),
            tabs: vec![new_tab],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
        };
        let group_id = group.id;
        let strip = Strip {
            id: StripId::from(next_id()),
            name: String::new(),
            window_groups: vec![group],
        };
        let strip_id = strip.id;
        ws.strips.push(strip);
        tree.active_strip.insert(ws_id, strip_id);
        tree.set_active_group(strip_id, group_id);
        return tab_id;
    }

    // Target strip: the focused one, else the first. A recorded pointer to a strip that has since been closed falls back the same way.
    let strip_idx = active_strip
        .and_then(|id| ws.strips.iter().position(|s| s.id == id))
        .unwrap_or(0);
    // `strip_idx` is either a position found in `ws.strips` or the fallback `0`, and the
    // empty-strips case already returned above, so this is always in bounds — `get_mut` keeps
    // that an assertion the type system checks rather than a trusted invariant.
    let Some(strip) = ws.strips.get_mut(strip_idx) else {
        return tab_id;
    };
    let strip_id = strip.id;

    // A `TabIn` whose group is gone degrades to a new group rather than silently landing somewhere arbitrary.
    let tab_target = match placement {
        Placement::TabIn(g) => strip.window_groups.iter().position(|w| w.id == g),
        // `IntoTab` only reaches here when its tab has gone; it degrades to a new group, as the variant's own documentation says.
        Placement::NewGroup | Placement::RightOf(_) | Placement::IntoTab(_) => None,
    };

    if let Some(idx) = tab_target {
        // `idx` came from `.position()` on this same `strip.window_groups` just above.
        let Some(group) = strip.window_groups.get_mut(idx) else {
            return tab_id;
        };
        // Right of the tab the user was on, not at the end of the bar.
        let after = group
            .tabs
            .iter()
            .position(|t| t.id == group.active_tab)
            .map_or(group.tabs.len(), |i| i.saturating_add(1));
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
    // Right of the group the placement named, falling back to the focused one when that group is gone, and to the end of the strip when nothing is focused either.
    let anchor = match placement {
        Placement::RightOf(g) => Some(g).filter(|g| strip.window_groups.iter().any(|w| w.id == *g)),
        _ => None,
    }
    .or(active_group);
    let insert_at = anchor
        .and_then(|g| strip.window_groups.iter().position(|w| w.id == g))
        .map_or(strip.window_groups.len(), |i| i.saturating_add(1));
    strip.window_groups.insert(insert_at, group);
    tree.active_strip.insert(ws_id, strip_id);
    tree.set_active_group(strip_id, group_id);
    tab_id
}

/// Rewrite the `app_id` and tab `title` of a previously-adopted `TileContent::Foreign` tab on
/// `xdg_toplevel::set_app_id`. Walks the tree to find the tab still carrying the synthetic
/// `anon-<surface_id>` placeholder from [`adopt_toplevel`]. Returns `false` when no matching tab exists.
pub fn rename_tab_for_app_id(
    tree: &mut WorkspaceTree,
    surface_id: u64,
    new_app_id: String,
) -> bool {
    let new_title = tab_label_for_foreign(&new_app_id);
    for ws in &mut tree.workspaces {
        for strip in &mut ws.strips {
            for group in &mut strip.window_groups {
                for tab in &mut group.tabs {
                    if let TileContent::Foreign {
                        surface_id: s,
                        app_id,
                    } = &mut tab.tile
                        && *s == surface_id
                    {
                        *app_id = new_app_id;
                        tab.title = new_title;
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Resolve the [`GroupId`] owning the `TileContent::Foreign` tab bound to `surface_id`. Used by
/// `xdg_toplevel.set_fullscreen` to find which group to promote; `None` when the surface was never adopted (e.g. racing surface teardown).
pub fn group_id_for_surface(tree: &WorkspaceTree, surface_id: u64) -> Option<GroupId> {
    for ws in &tree.workspaces {
        for strip in &ws.strips {
            for group in &strip.window_groups {
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

/// Bring the already-adopted foreign toplevel to the front, including a
/// workspace/strip transition when it lives outside the current view.
///
/// This is the compositor-side effect of an accepted `xdg_activation_v1`
/// request. It deliberately mutates only local focus pointers; no window is
/// created, moved, or written into collaborative continuity state.
pub fn activate_surface(tree: &mut WorkspaceTree, surface_id: u64) -> bool {
    let location = tree.workspaces.iter().find_map(|workspace| {
        workspace.strips.iter().find_map(|strip| {
            strip.window_groups.iter().find_map(|group| {
                group.tabs.iter().find_map(|tab| {
                    matches!(
                        &tab.tile,
                        TileContent::Foreign { surface_id: id, .. } if *id == surface_id
                    )
                    .then_some((workspace.id, strip.id, group.id, tab.id))
                })
            })
        })
    });
    let Some((workspace_id, strip_id, group_id, tab_id)) = location else {
        return false;
    };

    let Some(workspace) = tree.workspaces.iter_mut().find(|ws| ws.id == workspace_id) else {
        return false;
    };
    let Some(group) = workspace
        .strips
        .iter_mut()
        .find(|strip| strip.id == strip_id)
        .and_then(|strip| {
            strip
                .window_groups
                .iter_mut()
                .find(|group| group.id == group_id)
        })
    else {
        return false;
    };
    group.active_tab = tab_id;
    tree.active = workspace_id;
    tree.active_strip.insert(workspace_id, strip_id);
    tree.set_active_group(strip_id, group_id);
    true
}

/// App id recorded for a foreign surface, if it has been adopted.
pub fn app_id_for_surface(tree: &WorkspaceTree, surface_id: u64) -> Option<&str> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .find_map(|tab| match &tab.tile {
            TileContent::Foreign {
                surface_id: id,
                app_id,
            } if *id == surface_id => Some(app_id.as_str()),
            _ => None,
        })
}

/// Walk the tree for a `TileContent::Foreign` tab whose `surface_id` matches
/// `destroyed_surface_id`, remove it, return `true` if found. The widget tree handles close animation; this just records absence.
pub fn close_tab_for_destroyed_surface(
    tree: &mut WorkspaceTree,
    destroyed_surface_id: u64,
) -> bool {
    let tab_id = tree
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .find_map(|tab| match &tab.tile {
            TileContent::Foreign { surface_id, .. } if *surface_id == destroyed_surface_id => {
                Some(tab.id)
            }
            _ => None,
        });
    tab_id.is_some_and(|tab_id| remove_tab(tree, tab_id))
}

/// Remove `tab_id` from wherever it is in the tree, repairing its group's selection if it was in
/// front. The undo for a placement whose window never came — a failed launch takes its
/// placeholder away rather than leaving a dead card. [`close_tab_for_destroyed_surface`] is the same removal keyed on a surface.
pub fn remove_tab(tree: &mut WorkspaceTree, tab_id: TabId) -> bool {
    let located = tree.workspaces.iter().enumerate().find_map(|(ws_idx, ws)| {
        ws.strips.iter().enumerate().find_map(|(strip_idx, strip)| {
            strip
                .window_groups
                .iter()
                .enumerate()
                .find_map(|(group_idx, group)| {
                    group
                        .tabs
                        .iter()
                        .position(|tab| tab.id == tab_id)
                        .map(|tab_idx| (ws_idx, strip_idx, group_idx, tab_idx, strip.id, group.id))
                })
        })
    });
    let Some((ws_idx, strip_idx, group_idx, tab_idx, strip_id, group_id)) = located else {
        return false;
    };

    let remove_group = tree.workspaces[ws_idx].strips[strip_idx].window_groups[group_idx]
        .tabs
        .len()
        == 1;
    let process_local_foreign = tree
        .workspaces
        .get(ws_idx)
        .and_then(|workspace| workspace.strips.get(strip_idx))
        .and_then(|strip| strip.window_groups.get(group_idx))
        .and_then(|group| group.tabs.get(tab_idx))
        .is_some_and(|tab| matches!(&tab.tile, TileContent::Foreign { .. }));
    // Delete from the CRDT before touching the cache. Besides making the
    // ordering explicit, this guarantees that a revision/peer merge between
    // the two representations cannot treat the stale document entry as an
    // insertion and resurrect it. A newly adopted `Foreign` surface is
    // deliberately process-local and may never have entered the replicated
    // document; absence there is therefore already the desired durable
    // state, not a failed close.
    if !tree.doc_remove_tab(strip_id, group_id, tab_id, remove_group) && !process_local_foreign {
        return false;
    }

    let group = &mut tree.workspaces[ws_idx].strips[strip_idx].window_groups[group_idx];
    let removed = group.tabs.remove(tab_idx);
    if group.active_tab == tab_id {
        group.active_tab = group.tabs.last().map_or(0, |tab| tab.id);
    }
    if remove_group {
        let removed_was_active = tree.active_group.get(&strip_id) == Some(&group_id);
        tree.workspaces[ws_idx].strips[strip_idx]
            .window_groups
            .remove(group_idx);
        if removed_was_active {
            let groups = &tree.workspaces[ws_idx].strips[strip_idx].window_groups;
            match groups
                .get(group_idx)
                .or_else(|| groups.last())
                .map(|group| group.id)
            {
                Some(next) => {
                    tree.active_group.insert(strip_id, next);
                }
                None => {
                    tree.active_group.remove(&strip_id);
                }
            }
        }
    }
    if let Some(room) = tree.continuation_owned_tabs.remove(&tab_id) {
        tree.continuation_closed_tabs.insert(tab_id, room);
    }
    if let Ok(mut registry) = crate::shell::personal_continuity_registry::global_registry().lock() {
        registry.forget_local_tab(crate::shell::personal_continuity::AppInstanceId(removed.id));
    }
    true
}

/// Turn a wayland app-id into a tab-bar label. The app's own name wins (this session's
/// launches, the installed package's manifest, or a host `.desktop` entry), since a tab must
/// never show a raw machine identifier. Otherwise the trailing component is humanised
/// (`dev.qos.word-processor` → `Word Processor`), the same transform the package registry
/// applies. Shared with window cards, so an app is never named two different ways.
pub(crate) fn tab_label_for_foreign(app_id: &str) -> String {
    if let Some(name) = crate::shell::launcher::display_name_for_app_id(app_id) {
        return name;
    }
    let label = crate::shell::launcher::display_name_for_package_id(app_id);
    let label = if label.trim().is_empty() {
        app_id.to_string()
    } else {
        label
    };
    if label.chars().count() > 32 {
        label.chars().take(32).collect()
    } else {
        label
    }
}

// Every test drives `launcher`'s process-wide pending-placement table, so they're serialised on
// the crate's one test group; clearing the table only protects against predecessors, not concurrent siblings.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::WorkspaceTree;

    /// The group holding the foreign tab for `surface_id`, wherever placement put it — tests assert
    /// on the adopted tab, not a fixed index, so a placement change fails as a placement assertion, not five unrelated ones.
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

    /// The tab adoption of `surface_id` created, wherever placement put it — never `tabs.last()`, which only happens to be right when the rule appends.
    fn tab_of(tree: &WorkspaceTree, surface_id: u64) -> &crate::shell::state::Tab {
        group_of(tree, surface_id)
            .tabs
            .iter()
            .find(|t| {
                matches!(&t.tile, TileContent::Foreign { surface_id: s, .. } if *s == surface_id)
            })
            .expect("the group holding the surface holds its tab")
    }

    /// Cut the bootstrap strip's group down to the one window it shows and focus it — `bootstrap_demo` ships a four-tab group, so a lone-window fixture must trim it.
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

    /// A window started for a tab the room already has lands *in* that tab, not in a second one beside it.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_window_started_for_an_existing_tab_lands_in_it() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
        // Stand in for the tab a peer's window arrived as.
        let target = {
            let group = &mut tree.workspaces[0].strips[0].window_groups[0];
            let tab = Tab {
                id: next_id(),
                title: "Word Processor".into(),
                tile: TileContent::PeerWindow {
                    app_id: "dev.qos.word-processor".into(),
                    label: "Word Processor".into(),
                    note: "Opening here…".into(),
                },
            };
            let id = tab.id;
            group.tabs.push(tab);
            id
        };
        let groups_before = tree.workspaces[0].strips[0].window_groups.len();
        let tabs_before: usize = tree.workspaces[0].strips[0]
            .window_groups
            .iter()
            .map(|g| g.tabs.len())
            .sum();

        crate::shell::launcher::set_pending_placement(
            "dev.qos.word-processor",
            Placement::IntoTab(target),
        );
        let landed = adopt_toplevel(&mut tree, 7777, Some("dev.qos.word-processor".into()));

        assert_eq!(
            landed, target,
            "the window belongs to the tab it was started for"
        );
        let tabs_after: usize = tree.workspaces[0].strips[0]
            .window_groups
            .iter()
            .map(|g| g.tabs.len())
            .sum();
        assert_eq!(
            tabs_after, tabs_before,
            "binding into an existing tab must not mint a second one"
        );
        assert_eq!(
            tree.workspaces[0].strips[0].window_groups.len(),
            groups_before,
            "nor a second group to put it in"
        );
        match &tab_of(&tree, 7777).tile {
            TileContent::Foreign { app_id, surface_id } => {
                assert_eq!(app_id, "dev.qos.word-processor");
                assert_eq!(*surface_id, 7777);
            }
            other => panic!("the tab must now host the real surface, got {other:?}"),
        }
        assert_eq!(
            group_of(&tree, 7777).active_tab,
            target,
            "the tab showing the window is the one in front of its group"
        );
    }

    /// The tab was closed by its owner while the local instance was starting; placement degrades to the ordinary launch rule.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_window_whose_tab_vanished_is_still_placed() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
        let groups_before = tree.workspaces[0].strips[0].window_groups.len();

        crate::shell::launcher::set_pending_placement(
            "dev.qos.gone",
            // An id no tab in the tree carries.
            Placement::IntoTab(next_id()),
        );
        let landed = adopt_toplevel(&mut tree, 7878, Some("dev.qos.gone".into()));

        assert_eq!(
            tree.workspaces[0].strips[0].window_groups.len(),
            groups_before + 1,
            "a window with nowhere booked still opens its own group"
        );
        assert_eq!(tab_of(&tree, 7878).id, landed);
    }

    /// A window opened while a single window has focus is a new thing beside it: its own column, immediately to the right.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_launch_beside_a_lone_window_opens_its_own_group_to_the_right() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
        let (strip_id, _focused) = focus_lone_window(&mut tree);
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
            other => panic!("expected Foreign tile, got {other:?}"),
        }
    }

    /// Selection inside a group of several tabs joins the launch as a tab right of the selection, rather than splitting into another column.
    #[test]
    fn a_launch_from_inside_a_tabbed_group_becomes_the_next_tab_in_it() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
        let strip_id = tree.workspaces[0].strips[0].id;
        // `bootstrap_demo`'s group holds four tabs. Select the second one, so "immediately right of the selected tab" is distinct from both ends of the bar.
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
        let mut expected = tab_ids_before;
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

    /// The new column goes right of the focused group, not the end of the strip — next to the one it was opened from, so the strip doesn't scroll away.
    #[test]
    #[serial_test::serial(process_env)]
    fn the_new_group_is_inserted_beside_the_focused_group_not_at_the_end() {
        use crate::shell::state::{WindowGroup, WindowWidth, next_id};
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
        let (_strip_id, focused) = focus_lone_window(&mut tree);
        // Two more groups after the first, so "beside" and "at the end" are distinguishable positions.
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

    /// A document opened from another app is the same task as the app that asked for it, so it joins that app's group as a tab.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_document_opened_from_an_app_becomes_a_tab_in_that_apps_group() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
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

    /// The opener can be gone by the time the document's window maps. That degrades to the launch placement rather than dropping the window into an arbitrary group.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_tab_placement_naming_a_dead_group_falls_back_to_a_new_group() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
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

    /// The launch-time note and the adoption are two events on two processes; this is the whole correlation, driven end to end.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_recorded_placement_is_consumed_by_the_matching_adoption() {
        use crate::shell::launcher;
        use crate::shell::state::{WindowGroup, WindowWidth, next_id};
        launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
        let (strip_id, selected) = focus_lone_window(&mut tree);
        // A second group the note can name — somewhere the rule would never send a window, since the selection is elsewhere.
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
        // The client announces the bare trailing component, as the reverse-DNS convention has it.
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

        // Adopting moved selection onto the window just opened — necessarily, since that's what the user now looks at. Put it back on their lone window, the state a second launch would actually happen in.
        tree.set_active_group(strip_id, selected);

        // The note belongs to that first window: a *second* window from the same client is a new thing again, and takes the rule.
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

    /// The compositor adopts every toplevel twice — once into the protocol-side tree, once into the
    /// rendered tree `reconcile_foreign_tabs` mirrors. Both must reach the same placement, or a note consumed by the first reader leaves the rendered tree on the default.
    #[test]
    #[serial_test::serial(process_env)]
    fn both_trees_place_the_same_toplevel_the_same_way() {
        use crate::shell::launcher;
        launcher::clear_pending_placements();

        let mut protocol = WorkspaceTree::bootstrap_demo();
        // A lone selected window, so the rule's answer (beside it) differs from the note's (inside it) — without that the two placements coincide and the test proves nothing.
        let (_strip_id, opener) = focus_lone_window(&mut protocol);
        // The rendered tree is a clone of the persisted one, so the two share group ids — which is what makes a placement naming a group meaningful across both.
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
        let mut tree = WorkspaceTree::bootstrap_demo();
        let _ = adopt_toplevel(&mut tree, 4243, Some("org.example.app".into()));
        let expected = group_of(&tree, 4243).id;
        assert_eq!(
            group_id_for_surface(&tree, 4243),
            Some(expected),
            "a fullscreen request for surface 4243 resolves to its owning group",
        );
        // An unknown surface resolves to nothing — a fullscreen request that raced surface teardown is a safe no-op.
        assert_eq!(group_id_for_surface(&tree, 9999), None);
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn activation_reveals_exact_surface_without_moving_it() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
        let _ = adopt_toplevel(&mut tree, 7001, Some("org.example.Editor".into()));
        let original_workspace = tree.active;
        let original_group = group_of(&tree, 7001).id;
        let other = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.id != original_workspace)
            .map(|workspace| workspace.id)
            .expect("demo has another workspace");
        tree.active = other;

        assert!(activate_surface(&mut tree, 7001));
        assert_eq!(tree.active, original_workspace);
        assert_eq!(group_id_for_surface(&tree, 7001), Some(original_group));
        assert_eq!(app_id_for_surface(&tree, 7001), Some("org.example.Editor"));
        assert!(!activate_surface(&mut tree, 9999));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn close_tab_for_destroyed_surface_removes_matching_foreign_tab() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
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
    fn continuation_takeover_is_captured_locally_and_its_close_is_tombstoned() {
        use crate::shell::personal_continuity::{
            AppInstanceId, AppInstanceState, ContinuityStamp, DeviceContinuationSnapshot, DeviceId,
            LaunchIntent, TabPlacement, WorkspaceContinuation,
        };
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_default();
        let room = crate::shell::state::RoomId::new();
        tree.adopt_continuation_room(room, "Laptop");
        let source = DeviceId::new("laptop").unwrap();
        tree.materialize_continuation_workspace(
            "Laptop",
            &WorkspaceContinuation {
                room,
                apps: vec![AppInstanceState::new(
                    AppInstanceId(0x7171),
                    TabPlacement {
                        strip: 0,
                        group: 0,
                        tab: 0,
                    },
                    LaunchIntent::app("dev.qos.browser").unwrap(),
                    None,
                    ContinuityStamp {
                        counter: 10,
                        device: source,
                    },
                )],
            },
        );
        crate::shell::launcher::set_pending_placement(
            "dev.qos.browser",
            Placement::IntoTab(0x7171),
        );
        assert_eq!(
            adopt_toplevel(&mut tree, 8080, Some("dev.qos.browser".into())),
            0x7171
        );
        assert!(!tree.continuation_tabs.contains(&0x7171));

        let desktop = DeviceId::new("desktop").unwrap();
        let claimed = DeviceContinuationSnapshot::capture(&tree, desktop.clone(), 11).snapshot;
        assert!(claimed.workspaces.iter().any(|workspace| {
            workspace.room == room && workspace.apps.iter().any(|app| app.id.0 == 0x7171)
        }));
        assert!(close_tab_for_destroyed_surface(&mut tree, 8080));
        let mut closed = DeviceContinuationSnapshot::capture(&tree, desktop, 12).snapshot;
        closed.record_closures_from(&claimed).unwrap();
        assert!(
            closed
                .tombstones
                .iter()
                .any(|tombstone| tombstone.room == room && tombstone.id.0 == 0x7171)
        );
        crate::shell::launcher::clear_pending_placements();
    }

    /// `remove_tab` takes the tab out of whichever group holds it and repairs the group's selection when it was the tab in front.
    #[test]
    #[serial_test::serial(process_env)]
    fn remove_tab_takes_a_placed_tab_out_and_repairs_selection() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
        let tab_id = adopt_toplevel(&mut tree, 55, Some("io.example.pending".into()));
        let owner = group_of(&tree, 55).id;
        let before = group_of(&tree, 55).tabs.len();
        assert_eq!(
            group_of(&tree, 55).active_tab,
            tab_id,
            "fixture sanity: the newly adopted tab is the one in front",
        );

        assert!(
            remove_tab(&mut tree, tab_id),
            "the tab exists and is removed"
        );

        let group = tree
            .workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .flat_map(|s| s.window_groups.iter())
            .find(|g| g.id == owner)
            .expect("the owning group still exists");
        assert_eq!(
            group.tabs.len(),
            before - 1,
            "tab count must drop by exactly one"
        );
        assert!(
            group.tabs.iter().all(|t| t.id != tab_id),
            "the removed tab must be gone",
        );
        assert_ne!(
            group.active_tab, tab_id,
            "the group must not point at the tab it no longer has",
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn removing_a_group_survives_document_rehydration() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
        let group_id = tree.workspaces[0].strips[0].window_groups[0].id;
        let tab_ids = tree.workspaces[0].strips[0].window_groups[0]
            .tabs
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>();

        for tab_id in tab_ids {
            assert!(remove_tab(&mut tree, tab_id));
        }
        assert!(
            tree.workspaces[0].strips[0].window_groups.is_empty(),
            "the cache drops the group when its last tab closes"
        );

        tree.resync_cache_from_doc();
        assert!(
            tree.workspaces[0].strips[0]
                .window_groups
                .iter()
                .all(|group| group.id != group_id),
            "rehydrating from the durable document must not resurrect the closed group"
        );
    }

    /// A tab id nothing in the tree holds is reported as not removed, not silently accepted — a caller unwinding a launch has to know whether there was anything to unwind.
    #[test]
    #[serial_test::serial(process_env)]
    fn remove_tab_reports_false_for_an_unknown_tab() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
        assert!(!remove_tab(&mut tree, 0xdead_beef));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn rename_tab_for_app_id_rewrites_anon_placeholder() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        // First adopt without an app-id so the tab gets the `anon-<surface_id>` placeholder.
        let _ = adopt_toplevel(&mut tree, 99, None);
        let renamed = rename_tab_for_app_id(&mut tree, 99, "org.example.late".into());
        assert!(renamed, "rename should report it updated a tab");
        let tab = tab_of(&tree, 99);
        match &tab.tile {
            TileContent::Foreign { app_id, surface_id } => {
                assert_eq!(*surface_id, 99);
                assert_eq!(app_id, "org.example.late");
            }
            other => panic!("expected Foreign tile, got {other:?}"),
        }
        // Humanised, not raw. A tab is a name badge; `late` is a fragment of an identifier and `Late` is a word.
        assert_eq!(tab.title, "Late", "title is the humanised app-id component");
    }

    // A tab carries the app's name, never a raw identifier like `app` or `word-processor`.

    /// With nothing installed and nothing launched, the label is still built out of words rather than out of an identifier.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_hyphenated_app_id_becomes_a_name_not_a_slug() {
        let tmp = tempfile::tempdir().unwrap();
        // Point the launcher at an empty registry so the lookup misses and the fallback is what is under test.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        crate::shell::launcher::invalidate_app_cache();
        crate::shell::launcher::clear_launched_for_test();

        assert_eq!(
            tab_label_for_foreign("dev.qos.word-processor"),
            "Word Processor"
        );
        assert_eq!(
            tab_label_for_foreign("dev.qos.file-explorer"),
            "File Explorer"
        );
        // The documented behaviour for an already-camel-cased id is unchanged.
        assert_eq!(tab_label_for_foreign("org.example.MyApp"), "MyApp");

        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        crate::shell::launcher::invalidate_app_cache();
    }

    /// The package's manifest names the app, and that name is the app's name whoever started the window.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_tab_takes_the_name_the_package_declares_even_without_a_launch() {
        let tmp = tempfile::tempdir().unwrap();
        install_named(tmp.path(), "dev.qos.file-explorer", "Files");
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        crate::shell::launcher::invalidate_app_cache();
        crate::shell::launcher::clear_launched_for_test();

        assert_eq!(tab_label_for_foreign("dev.qos.file-explorer"), "Files");
        // And by the trailing-component convention the app-id may use.
        assert_eq!(tab_label_for_foreign("file-explorer"), "Files");

        let mut tree = WorkspaceTree::bootstrap_demo();
        let _ = adopt_toplevel(&mut tree, 4242, Some("dev.qos.file-explorer".to_string()));
        assert_eq!(
            tab_of(&tree, 4242).title,
            "Files",
            "the tab the compositor actually builds carries the real name"
        );

        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        crate::shell::launcher::invalidate_app_cache();
    }

    /// Minimal installed package: a manifest declaring a name, and a registry entry pointing at it. Same shape `qos-install` writes.
    fn install_named(dir: &std::path::Path, package_id: &str, name: &str) {
        use std::fmt::Write as _;
        let payload = dir.join("payloads").join(package_id);
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        let manifest_path = payload.join("manifest.toml");
        std::fs::write(
            &manifest_path,
            format!(
                "[package]\nid = \"{package_id}\"\nname = \"{name}\"\n\
                 version = \"1.0.0\"\n[entrypoint]\nbin = \"bin/app\"\n\
                 type = \"gui\"\n"
            ),
        )
        .unwrap();
        let registry = dir.join("registry.toml");
        let mut existing = std::fs::read_to_string(&registry).unwrap_or_default();
        let _ = write!(
            existing,
            "[[entry]]\nid = '{package_id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
            payload.display().to_string(),
            manifest_path.display().to_string()
        );
        std::fs::write(&registry, existing).unwrap();
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn rename_tab_for_app_id_returns_false_for_unknown_surface() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let renamed = rename_tab_for_app_id(&mut tree, 12345, "x".into());
        assert!(
            !renamed,
            "rename must report `false` when the surface isn't adopted"
        );
    }

    /// Adoption into a workspace with no strips must synthesize a default strip + group rather than orphan the surface.
    #[test]
    #[serial_test::serial(process_env)]
    fn adopt_toplevel_creates_strip_when_workspace_empty() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        // Start from a known-good tree and erase the active workspace's strips so we exercise the new branch. Other workspaces (if any) are untouched.
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

    /// A toplevel from a package this session launched is labelled with the installed app's name, not the raw wayland app-id; launch history is process-global, so the fixture uses an id no other test registers.
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
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
        });
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
        // The client announces the bare app-id — the trailing component of the package id, which is the reverse-DNS convention.
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

    /// A shell-hosted app opened beside a lone window takes the column to the right, landing as a tile resolving to a live app widget rather than a stub that looks identical in the tree but blank on screen.
    #[test]
    fn a_hosted_app_opened_beside_a_lone_window_takes_the_column_to_its_right() {
        let mut tree = WorkspaceTree::bootstrap_demo();
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
        let mut tree = WorkspaceTree::bootstrap_demo();
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
            other => panic!("expected Foreign tile, got {other:?}"),
        }
    }
}
