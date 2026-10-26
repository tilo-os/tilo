//! Adopt foreign Wayland clients into the active workspace's tab tree.
//!
//! On a client's first `xdg_toplevel` commit, splices a new tab into the workspace tree so
//! chrome paints a pill and the frame loop blits into the tile rect; the reverse path
//! (destroyed surface → tab close) also lives here. Entry points take a `surface_id: u64`
//! ([`crate::compositor::state::surface_id`], composing connection index + protocol id — a bare
//! protocol id collides across clients) rather than an `&WlSurface`, so unit tests don't need a running `Display`.

use crate::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth,
    WorkspaceId, WorkspaceTree, next_id,
};

/// A window the person asked to be taken to: what
/// [`WorkspaceTree::brought_forward`] holds. `serial` orders them, so each
/// consumer acts on a request once without having to clear it for the
/// other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BroughtForward {
    pub serial: u64,
    pub workspace: WorkspaceId,
    pub strip: StripId,
    pub group: GroupId,
    /// The window itself: a window maximized elsewhere that steps back for
    /// it is lent to it ([`crate::shell::stepped_back::SteppedBack`]).
    pub tab: TabId,
}

impl BroughtForward {
    /// Whether this window is still the one in front: the active
    /// workspace, its active strip and that strip's active group. A request
    /// the person has since moved away from is not followed any more.
    #[must_use]
    pub fn is_focused(&self, tree: &WorkspaceTree) -> bool {
        tree.desktop.active == self.workspace
            && tree.desktop.active_strip.get(&self.workspace) == Some(&self.strip)
            && tree.desktop.active_group_for(self.strip) == Some(self.group)
    }
}

static BROUGHT_FORWARD_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The one way a window comes forward because the person asked for it:
/// they launched it, opened a file or link into it, chose it from search,
/// a notification or the window switcher, or an app they were using
/// handed it a valid activation token. Shows `tab` in its group, makes the
/// group the focused one of its strip, the strip the active one of its
/// workspace and the workspace the active one, so the view goes there
/// across strips, workspaces and displays. A window maximized over that
/// strip steps back so the strip can scroll to it, and is maximized again
/// when this window goes away
/// ([`crate::shell::stepped_back::SteppedBack`]); the request left in
/// [`WorkspaceTree::brought_forward`] has the compositor end a full screen
/// elsewhere and the desktop move the view with standard motion.
///
/// Something opened for an agent, a collaborator or a background process
/// never comes through here: it keeps the person where they are.
///
/// Returns `false`, changing nothing, when the address is not in the tree.
pub fn bring_forward(
    tree: &mut WorkspaceTree,
    workspace: WorkspaceId,
    strip: StripId,
    group: GroupId,
    tab: TabId,
) -> bool {
    let Some(target_strip) = tree
        .desktop
        .workspaces
        .iter_mut()
        .find(|candidate| candidate.id == workspace)
        .and_then(|ws| ws.strips.iter_mut().find(|candidate| candidate.id == strip))
    else {
        return false;
    };
    if !target_strip
        .window_groups
        .iter()
        .any(|candidate| candidate.id == group && candidate.tabs.iter().any(|t| t.id == tab))
    {
        return false;
    }
    let mut stepping_back = Vec::new();
    for other in &mut target_strip.window_groups {
        if other.id == group {
            other.activate_tab(tab);
        } else if other.maximized {
            stepping_back.push(other.id);
        }
    }
    for other in stepping_back {
        crate::shell::stepped_back::step_back_for(tree, other, tab);
    }
    tree.desktop.active = workspace;
    tree.desktop.active_strip.insert(workspace, strip);
    tree.desktop.set_active_group(strip, group);
    tree.brought_forward = Some(BroughtForward {
        serial: BROUGHT_FORWARD_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        workspace,
        strip,
        group,
        tab,
    });
    true
}

/// Notify the retained chrome that a protocol-side tree mutation landed.
/// Native protocol callbacks and chrome share this tree, so there is no
/// rendered-tree diff left to provide the repaint signal.
pub(crate) fn mark_tree_changed(tree: &mut WorkspaceTree) {
    tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
}

/// A window's artwork changed without the tree changing: repaint its tab.
pub fn note_tab_artwork_changed(tree: &mut WorkspaceTree) {
    mark_tree_changed(tree);
}

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
    /// A tab at an exact insertion index in an existing group. Pointer drops use
    /// this instead of [`Self::TabIn`] so landing left or right of a particular
    /// tab remains stable even if focus changes while the app is starting.
    TabAt(GroupId, usize),
    /// [`Self::TabAt`] without taking focus: the group keeps showing the tab
    /// it showed. What a link opened in a background tab gets.
    BackgroundTabAt(GroupId, usize),
    /// Its own window group, right of the group named here. Unlike [`Placement::NewGroup`] ("beside
    /// whatever has focus"), names a specific group, so the answer survives focus moving between launch and arrival. Falls back to [`Placement::NewGroup`] when gone, as [`Placement::TabIn`] does.
    RightOf(GroupId),
    /// [`Self::RightOf`] without taking focus: a view an actor's action
    /// needs (an agent's, a Shortcut's, another app's), opened beside the
    /// group that actor works in, on whichever strip holds it. The person
    /// keeps their place and focus.
    Beside(GroupId),
    /// A new window group at an exact seam in a named strip. The index is
    /// clamped to the live group count; a stale strip falls back to
    /// [`Self::NewGroup`] rather than placing the window in an arbitrary strip.
    GroupAt(StripId, usize),
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
/// [`crate::shell::placement::placement_for`] when unrecognised.
///
/// When the active workspace has no strips, a default strip is synthesized so the tab always
/// lands visible — otherwise the foreign surface leaks buffer + shm-pool memory until the client tears the toplevel down on its own.
pub fn adopt_toplevel(tree: &mut WorkspaceTree, surface_id: u64, app_id: Option<String>) -> TabId {
    let tab = adopt_toplevel_unpersisted(tree, surface_id, app_id);
    persist_adopted_toplevel(tree, surface_id);
    tab
}

// Room-attributed adoption temporarily changes `active`; its caller must
// restore that selection before writing the desktop's durable snapshot.
fn adopt_toplevel_unpersisted(
    tree: &mut WorkspaceTree,
    surface_id: u64,
    app_id: Option<String>,
) -> TabId {
    let pending = app_id
        .as_deref()
        .and_then(|id| crate::shell::launcher::placement_and_instance_for_toplevel(id, surface_id));
    let placement = pending.map_or_else(
        || {
            crate::shell::placement::placement_for(
                tree,
                tilo_desktop_model::commands::OpenIntent::Launch,
                None,
            )
        },
        |(placement, _)| placement,
    );
    // If this window is what a document open asked for, record that here — the launch and the window it produced meet at this point.
    if let Some(id) = app_id.as_deref() {
        crate::shell::doc_open::claim_surface_for_document(id, surface_id);
    }
    let tab = adopt_toplevel_at(tree, surface_id, app_id, placement);
    if matches!(placement, Placement::IntoTab(_)) {
        tree.claim_continuation_tab(tab);
    }
    if let Some((_, Some(instance))) = pending {
        tree.remember_local_app_record(tab, instance);
        if let Ok(mut reports) =
            crate::shell::personal_continuity_registry::global_registry().lock()
        {
            reports.bind_tab(
                crate::shell::personal_continuity::AppInstanceId(tab),
                crate::shell::personal_continuity::AppInstanceId(instance),
            );
        }
    }
    tab
}

/// Adopt a launcher-attributed toplevel into its durable workspace room even
/// when the person switched workspaces while the process was starting.
/// Unknown rooms are denied rather than falling back to the active workspace.
pub fn adopt_toplevel_in_room(
    tree: &mut WorkspaceTree,
    surface_id: u64,
    app_id: Option<String>,
    room: crate::shell::state::RoomId,
) -> Option<TabId> {
    let target = tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .map(|workspace| workspace.id)?;
    let previous = tree.desktop.active;
    tree.desktop.active = target;
    let tab = adopt_toplevel_unpersisted(tree, surface_id, app_id);
    tree.desktop.active = previous;
    persist_adopted_toplevel(tree, surface_id);
    Some(tab)
}

fn persist_adopted_toplevel(tree: &WorkspaceTree, surface_id: u64) {
    // A provisional anonymous identity cannot be relaunched. Admission has
    // already established any native package identity before this point;
    // this guard only decides whether adoption is ready to save.
    if app_id_for_surface(tree, surface_id)
        .is_some_and(|app_id| !app_id.trim().is_empty() && app_id != format!("anon-{surface_id}"))
    {
        tree.queue_save();
    }
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
        mark_tree_changed(tree);
        return target;
    }
    let new_tab = Tab {
        id: next_id(),
        title: tab_label_for_foreign(&resolved_id),
        tile: TileContent::Foreign {
            app_id: resolved_id,
            surface_id,
        },
        app_title: None,
    };
    let tab_id = place_tab(tree, new_tab, placement);
    mark_tree_changed(tree);
    tab_id
}

/// Point the existing tab `target` at `surface_id`, and bring it to the front of its group.
/// Returns whether the tab was found; `false` means it's gone since the launch started, and the
/// caller places the window the ordinary way. Searches the whole tree, not the active workspace — the tab arrived from a peer, whose target workspace the launch didn't know.
fn bind_into_tab(tree: &mut WorkspaceTree, target: TabId, app_id: &str, surface_id: u64) -> bool {
    let focused_workspace = tree.desktop.active;
    let focused_strip = tree.desktop.active_strip.get(&focused_workspace).copied();
    let focused_group =
        focused_strip.and_then(|strip| tree.desktop.active_group.get(&strip).copied());
    for ws in &mut tree.desktop.workspaces {
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
                crate::shell::foreign::store().bind_opening_tab(target, surface_id);
                // Front the tab, but move focus no further: stealing the strip because somebody else opened something is exactly what the room model rules out.
                group.activate_tab(target);
                if focused_workspace != ws.id
                    || focused_strip != Some(strip_id)
                    || focused_group != Some(group.id)
                {
                    // It came to the front somewhere the person is not
                    // looking. Say so on its pill rather than moving them.
                    crate::shell::attention::ask_tab(target);
                }
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
/// [`crate::shell::placement::placement_for`] itself. Tile is a
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
        app_title: None,
    };
    let tab_id = place_tab(tree, new_tab, placement);
    // Hosted apps have no Wayland surface whose later adoption can write the
    // tab back.  Persist both authorities at the point of creation, or a
    // restart/peer export that happens before the next unrelated mutation
    // loses the newly-opened native window. Foreign bindings stay out of
    // the room document; their adoption wrappers persist symbolic launch
    // records only after placement and workspace selection are settled.
    tree.seed_doc_from_cache();
    tree.queue_save();
    tab_id
}

/// Hold the place a launch will open in from the moment it is asked for: a
/// tab at `placement` showing the opening card, which the app's window then
/// binds into ([`Placement::IntoTab`]). A cold start takes seconds, so
/// without it nothing on screen answers the activation until the window
/// maps. A failed launch or a missed deadline turns the card into the
/// retry card (`peer_windows::settle_local_restore`), and the card is never
/// saved as opening (`persist::clear_transient_launch_notes`).
pub fn hold_place_for_launch(
    tree: &mut WorkspaceTree,
    package_id: &str,
    label: &str,
    placement: Placement,
) -> TabId {
    let new_tab = Tab {
        id: next_id(),
        title: label.to_owned(),
        tile: TileContent::LocalApp {
            app_id: package_id.to_owned(),
            label: label.to_owned(),
            note: crate::shell::peer_windows::OPENING.to_owned(),
        },
        app_title: None,
    };
    let tab = place_tab(tree, new_tab, placement);
    crate::shell::launcher::set_pending_placement(package_id, Placement::IntoTab(tab));
    mark_tree_changed(tree);
    tab
}

/// Rehome an existing tab through the same exact destination grammar used by
/// a fresh drop. This is the deduplicated-document path: dragging a document
/// which is already open moves its one live window instead of launching a
/// duplicate or merely focusing the old location.
#[allow(
    clippy::too_many_lines,
    reason = "relocation validates and applies every shared placement variant as one atomic topology update"
)]
pub fn relocate_tab(tree: &mut WorkspaceTree, tab_id: TabId, placement: Placement) -> bool {
    let source = tree.desktop.workspaces.iter().find_map(|workspace| {
        workspace.strips.iter().find_map(|strip| {
            strip.window_groups.iter().find_map(|group| {
                group
                    .tabs
                    .iter()
                    .position(|tab| tab.id == tab_id)
                    .map(|tab_index| (strip.id, group.id, tab_index))
            })
        })
    });
    let Some((source_strip, source_group, source_index)) = source else {
        return false;
    };
    let destination = match placement {
        Placement::TabAt(group, index) | Placement::BackgroundTabAt(group, index) => {
            tree.desktop.workspaces.iter().find_map(|workspace| {
                workspace.strips.iter().find_map(|strip| {
                    strip
                        .window_groups
                        .iter()
                        .any(|candidate| candidate.id == group)
                        .then_some((
                            strip.id,
                            tilo_desktop_model::DropTarget::ExistingGroup {
                                group_id: group,
                                tab_index: index,
                            },
                        ))
                })
            })
        }
        Placement::TabIn(group) => tree.desktop.workspaces.iter().find_map(|workspace| {
            workspace.strips.iter().find_map(|strip| {
                strip.window_groups.iter().find_map(|candidate| {
                    (candidate.id == group).then_some((
                        strip.id,
                        tilo_desktop_model::DropTarget::ExistingGroup {
                            group_id: group,
                            tab_index: candidate.tabs.len(),
                        },
                    ))
                })
            })
        }),
        Placement::GroupAt(strip, index) => Some((
            strip,
            tilo_desktop_model::DropTarget::GroupGap { gap_index: index },
        )),
        Placement::NewGroup
        | Placement::RightOf(_)
        | Placement::Beside(_)
        | Placement::IntoTab(_) => None,
    };
    let Some((destination_strip, mut target)) = destination else {
        return false;
    };
    if destination_strip != source_strip {
        return tree.move_tab_across_strips(
            source_strip,
            destination_strip,
            tab_id,
            next_id(),
            target,
        );
    }

    let Some(strip) = tree
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .find(|strip| strip.id == source_strip)
    else {
        return false;
    };
    let source_group_index = strip
        .window_groups
        .iter()
        .position(|group| group.id == source_group)
        .unwrap_or(0);
    let Some(source_group_state) = strip.window_groups.get(source_group_index) else {
        return false;
    };
    let source_group_len = source_group_state.tabs.len();
    match &mut target {
        tilo_desktop_model::DropTarget::ExistingGroup {
            group_id,
            tab_index,
        } if *group_id == source_group && source_index < *tab_index => {
            *tab_index = tab_index.saturating_sub(1);
        }
        tilo_desktop_model::DropTarget::GroupGap { gap_index }
            if source_group_len == 1 && source_group_index < *gap_index =>
        {
            *gap_index = gap_index.saturating_sub(1);
        }
        _ => {}
    }
    let mut topology: Vec<tilo_desktop_model::GroupShape> = strip
        .window_groups
        .iter()
        .map(|group| tilo_desktop_model::GroupShape {
            id: group.id,
            tab_ids: group.tabs.iter().map(|tab| tab.id).collect(),
            active_tab: group.active_tab,
            width: group.width,
        })
        .collect();
    let Some(source_group_index) = topology.iter().position(|group| group.id == source_group)
    else {
        return false;
    };
    let remove_source_group = {
        let Some(source_group_state) = topology.get_mut(source_group_index) else {
            return false;
        };
        if source_index >= source_group_state.tab_ids.len() {
            return false;
        }
        source_group_state.tab_ids.remove(source_index);
        if source_group_state.active_tab == tab_id {
            source_group_state.active_tab =
                source_group_state.tab_ids.first().copied().unwrap_or(0);
        }
        source_group_state.tab_ids.is_empty()
    };
    if remove_source_group {
        topology.remove(source_group_index);
    }
    match target {
        tilo_desktop_model::DropTarget::ExistingGroup {
            group_id,
            tab_index,
        } => {
            let Some(group) = topology.iter_mut().find(|group| group.id == group_id) else {
                return false;
            };
            group
                .tab_ids
                .insert(tab_index.min(group.tab_ids.len()), tab_id);
            group.active_tab = tab_id;
        }
        tilo_desktop_model::DropTarget::GroupGap { gap_index } => topology.insert(
            gap_index.min(topology.len()),
            tilo_desktop_model::GroupShape {
                id: next_id(),
                tab_ids: vec![tab_id],
                active_tab: tab_id,
                width: tilo_desktop_model::WindowWidth::Normal,
            },
        ),
    }
    tree.reconcile_strip_topology(source_strip, &topology)
}

/// Where a tab placed in `group` goes: the exact index a placement names,
/// else right of the tab the user was on, not at the end of the bar.
fn tab_insert_index(group: &WindowGroup, placement: Placement) -> usize {
    match placement {
        Placement::TabAt(_, index) | Placement::BackgroundTabAt(_, index) => {
            index.min(group.tabs.len())
        }
        _ => group
            .tabs
            .iter()
            .position(|t| t.id == group.active_tab)
            .map_or(group.tabs.len(), |i| i.saturating_add(1)),
    }
}

/// A new window group holding only `tab`, which is active in it.
fn lone_group(tab: Tab) -> WindowGroup {
    let active_tab = tab.id;
    WindowGroup {
        id: next_id(),
        tabs: vec![tab],
        active_tab,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

/// Splice an already-built [`Tab`] into the active workspace at `placement`, and focus it.
/// Placement and tree decide where a window lands, not what's inside it, so both foreign and
/// hosted-app opens route through here. Returns `new_tab`'s id even when it can't be spliced (no active workspace).
pub fn place_tab(tree: &mut WorkspaceTree, new_tab: Tab, placement: Placement) -> TabId {
    let tab_id = new_tab.id;

    // Resolve the focused strip + group *before* taking the mutable workspace borrow: both live on the tree, not on the workspace.
    let Some(ws_id) = tree.desktop.active_workspace().map(|w| w.id) else {
        return tab_id;
    };
    let active_strip = tree.desktop.active_strip.get(&ws_id).copied();
    let active_group = active_strip.and_then(|s| tree.desktop.active_group_for(s));

    let Some(ws) = tree.desktop.active_workspace_mut() else {
        return tab_id;
    };

    // No strips at all: synthesize one around the new tab. Mirrors `apply_workspace_drop`'s empty-strip path.
    if ws.strips.is_empty() {
        let group = lone_group(new_tab);
        let group_id = group.id;
        let strip = Strip {
            id: StripId::from(next_id()),
            name: String::new(),
            window_groups: vec![group],
        };
        let strip_id = strip.id;
        ws.strips.push(strip);
        bring_forward(tree, ws_id, strip_id, group_id, tab_id);
        return tab_id;
    }

    // An explicit pointer target can name a strip other than the focused one.
    // Resolve it once; stale targets deliberately degrade to the ordinary
    // focused-strip placement rule rather than landing in a random strip.
    let explicit_strip = match placement {
        Placement::GroupAt(strip, _) => ws.strips.iter().position(|s| s.id == strip),
        _ => named_group(placement).and_then(|group| {
            ws.strips
                .iter()
                .position(|s| s.window_groups.iter().any(|g| g.id == group))
        }),
    };
    // Target strip: explicit destination, then focused one, else the first.
    let strip_idx = explicit_strip
        .or_else(|| active_strip.and_then(|id| ws.strips.iter().position(|s| s.id == id)))
        .unwrap_or(0);
    // `strip_idx` is either a position found in `ws.strips` or the fallback `0`, and the
    // empty-strips case already returned above, so this is always in bounds — `get_mut` keeps
    // that an assertion the type system checks rather than a trusted invariant.
    let Some(strip) = ws.strips.get_mut(strip_idx) else {
        return tab_id;
    };
    let strip_id = strip.id;

    // A tab target whose group is gone degrades to a new group rather than
    // silently landing somewhere arbitrary.
    let tab_target = match placement {
        Placement::TabIn(g) | Placement::TabAt(g, _) | Placement::BackgroundTabAt(g, _) => {
            strip.window_groups.iter().position(|w| w.id == g)
        }
        // `IntoTab` only reaches here when its tab has gone; it degrades to a new group, as the variant's own documentation says.
        Placement::NewGroup
        | Placement::RightOf(_)
        | Placement::Beside(_)
        | Placement::GroupAt(_, _)
        | Placement::IntoTab(_) => None,
    };

    if let Some(idx) = tab_target {
        // `idx` came from `.position()` on this same `strip.window_groups` just above.
        let Some(group) = strip.window_groups.get_mut(idx) else {
            return tab_id;
        };
        group
            .tabs
            .insert(tab_insert_index(group, placement), new_tab);
        if matches!(placement, Placement::BackgroundTabAt(..)) {
            // Behind the tab the person is reading: nothing moves.
            return tab_id;
        }
        let group_id = group.id;
        bring_forward(tree, ws_id, strip_id, group_id, tab_id);
        return tab_id;
    }

    let group = lone_group(new_tab);
    let group_id = group.id;
    // Right of the group the placement named, falling back to the focused one when that group is gone, and to the end of the strip when nothing is focused either.
    let anchor = match placement {
        Placement::RightOf(g) | Placement::Beside(g) => {
            Some(g).filter(|g| strip.window_groups.iter().any(|w| w.id == *g))
        }
        _ => None,
    }
    .or(active_group);
    let insert_at = match placement {
        Placement::GroupAt(target_strip, index) if target_strip == strip_id => {
            index.min(strip.window_groups.len())
        }
        _ => anchor
            .and_then(|g| strip.window_groups.iter().position(|w| w.id == g))
            .map_or(strip.window_groups.len(), |i| i.saturating_add(1)),
    };
    strip.window_groups.insert(insert_at, group);
    // Someone else's view: the person stays where they are. A new window
    // the person opened is where they go next.
    if !matches!(placement, Placement::Beside(_)) {
        bring_forward(tree, ws_id, strip_id, group_id, tab_id);
    }
    tab_id
}

/// The existing group `placement` names, whatever it does there.
const fn named_group(placement: Placement) -> Option<GroupId> {
    match placement {
        Placement::TabAt(group, _)
        | Placement::BackgroundTabAt(group, _)
        | Placement::TabIn(group)
        | Placement::RightOf(group)
        | Placement::Beside(group) => Some(group),
        Placement::NewGroup | Placement::GroupAt(..) | Placement::IntoTab(_) => None,
    }
}

/// Rewrite the `app_id` and tab `title` of a previously-adopted `TileContent::Foreign` tab on
/// `xdg_toplevel::set_app_id`. Walks the tree to find the tab still carrying the synthetic
/// `anon-<surface_id>` placeholder from [`adopt_toplevel`]. Returns `false` when no matching tab exists.
pub fn rename_tab_for_app_id(tree: &mut WorkspaceTree, surface_id: u64, new_app_id: &str) -> bool {
    rename_tab_for_app_id_with_title(tree, surface_id, new_app_id, None)
}

/// Rewrite a foreign tab's app-id after a late `set_app_id`, retaining an
/// already-authoritative label when a compositor-issued host-launch token
/// identified this exact surface.  The app-id remains current for later
/// routing; only the untrusted late label is ignored.
pub fn rename_tab_for_app_id_with_title(
    tree: &mut WorkspaceTree,
    surface_id: u64,
    new_app_id: &str,
    authoritative_title: Option<&str>,
) -> bool {
    let new_title =
        authoritative_title.map_or_else(|| tab_label_for_foreign(new_app_id), str::to_string);
    for ws in &mut tree.desktop.workspaces {
        for strip in &mut ws.strips {
            for group in &mut strip.window_groups {
                for tab in &mut group.tabs {
                    let renamed = if let TileContent::Foreign {
                        surface_id: s,
                        app_id,
                    } = &mut tab.tile
                        && *s == surface_id
                    {
                        new_app_id.clone_into(app_id);
                        tab.set_window_title(&new_title);
                        true
                    } else {
                        false
                    };
                    if renamed {
                        mark_tree_changed(tree);
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Finish adoption when a client (notably Electron and some GTK stacks) sets
/// its app-id after `new_toplevel`.  The anonymous adoption has already used
/// the ordinary placement rule, so claim the launch note now and re-home that
/// exact surface before merely changing its label.  The note is intentionally
/// left claimed: the rendered mirror asks the same API and receives the same
/// placement, while a later surface retires the one-shot note normally.
pub fn rebind_toplevel_for_app_id(
    tree: &mut WorkspaceTree,
    surface_id: u64,
    new_app_id: &str,
    authoritative_title: Option<&str>,
) -> bool {
    let title =
        authoritative_title.map_or_else(|| tab_label_for_foreign(new_app_id), str::to_string);
    let current_app_id = app_id_for_surface(tree, surface_id).map(str::to_string);
    let identity_changed = current_app_id
        .as_deref()
        .is_some_and(|app_id| app_id != new_app_id);
    if identity_changed
        && let Some(placement) =
            crate::shell::launcher::placement_for_toplevel(new_app_id, surface_id)
    {
        // `relocate_foreign_toplevel` deliberately preserves the identity
        // already stored on the tab. Replace the synthetic identity first so
        // the relocated tab cannot remain `anon-*` even though its title and
        // destination changed successfully.
        if !rename_tab_for_app_id_with_title(tree, surface_id, new_app_id, Some(&title)) {
            return false;
        }
        return relocate_foreign_toplevel(tree, surface_id, placement, &title);
    }
    rename_tab_for_app_id_with_title(tree, surface_id, new_app_id, Some(&title))
}

/// Re-home an already mapped foreign surface after its compositor-minted
/// activation token identifies the user-selected launch. The original tab is
/// process-local; remove it before creating the replacement so neither the
/// durable tree nor a rendered mirror can retain a duplicate surface.
pub fn relocate_foreign_toplevel(
    tree: &mut WorkspaceTree,
    surface_id: u64,
    placement: Placement,
    title: &str,
) -> bool {
    let Some(app_id) = app_id_for_surface(tree, surface_id).map(str::to_string) else {
        return false;
    };
    // The ordinary first-adoption path may already have claimed this launch's
    // pending `IntoTab` note before the activation request arrives.  That is
    // the desired destination, so do not remove the target tab and try to
    // bind it again: removing it would make `IntoTab` fall back to a new
    // group.  Activation still gives us the canonical display label.
    if let Placement::IntoTab(target) = placement
        && tree
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .flat_map(|group| group.tabs.iter())
            .any(|tab| {
                tab.id == target
                    && matches!(
                        &tab.tile,
                        TileContent::Foreign { surface_id: id, .. } if *id == surface_id
                    )
            })
    {
        return rename_tab_for_app_id(tree, surface_id, &app_id)
            && rename_tab_for_title(tree, surface_id, title);
    }
    // The window keeps its identity through the move: the replacement tab
    // takes the old tab's id, and when the window had a group of its own and
    // gets one again, that group keeps its id too. A launch that activates
    // its token a few seconds after mapping (Chromium) otherwise saw its
    // window's group vanish and a new one appear in its place. A window bound
    // into an existing tab takes that tab, and with it that tab's id.
    let previous = if matches!(placement, Placement::IntoTab(_)) {
        None
    } else {
        tree.desktop
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .find_map(|group| {
                group
                    .tabs
                    .iter()
                    .find(|tab| {
                        matches!(&tab.tile, TileContent::Foreign { surface_id: id, .. } if *id == surface_id)
                    })
                    .map(|tab| {
                        let name = tab.is_named().then(|| tab.title.clone());
                        (tab.id, group.id, group.tabs.len() == 1, name)
                    })
            })
    };
    if !close_tab_for_destroyed_surface(tree, surface_id) {
        return false;
    }
    let tab_id = adopt_toplevel_at(tree, surface_id, Some(app_id), placement);
    let mut regrouped = None;
    let mut titled = false;
    for workspace in &mut tree.desktop.workspaces {
        for strip in &mut workspace.strips {
            for group in &mut strip.window_groups {
                let Some(tab) = group.tabs.iter_mut().find(|tab| tab.id == tab_id) else {
                    continue;
                };
                tab.title = title.to_string();
                titled = true;
                if let Some((old_tab, old_group, was_alone, name)) = previous.clone() {
                    // A name the person gave the window stays through the move.
                    if let Some(name) = name {
                        tab.rename(&name);
                    }
                    tab.id = old_tab;
                    if group.active_tab == tab_id {
                        group.activate_tab(old_tab);
                    }
                    if was_alone && group.tabs.len() == 1 && group.id != old_group {
                        regrouped = Some((group.id, old_group));
                        group.id = old_group;
                    }
                }
            }
        }
    }
    if let Some((minted, kept)) = regrouped {
        for active in tree.desktop.active_group.values_mut() {
            if *active == minted {
                *active = kept;
            }
        }
    }
    if titled {
        mark_tree_changed(tree);
    }
    titled
}

/// Rewrite only a foreign tab's client-provided visible title.  App identity
/// remains stable for launch matching and grouping; an empty title clears
/// stale document metadata exactly as `xdg_toplevel.set_title("")` requests.
pub fn rename_tab_for_title(tree: &mut WorkspaceTree, surface_id: u64, title: &str) -> bool {
    for workspace in &mut tree.desktop.workspaces {
        for strip in &mut workspace.strips {
            for group in &mut strip.window_groups {
                let titled = if let Some(tab) = group.tabs.iter_mut().find(|tab| {
                    matches!(
                        &tab.tile,
                        TileContent::Foreign { surface_id: id, .. } if *id == surface_id
                    )
                }) {
                    let location = match &tab.tile {
                        TileContent::Foreign { app_id, .. } => {
                            location_from_title(title, &tab_label_for_foreign(app_id))
                        }
                        _ => title.to_owned(),
                    };
                    // Terminals re-send their title continuously; an identical
                    // one is not a change, and must not bump the tree revision
                    // (a chrome rebuild and repaint per event). While a person's
                    // name stands, the window's title is kept behind it.
                    if !tab.set_window_title(&location) {
                        return true;
                    }
                    true
                } else {
                    false
                };
                if titled {
                    mark_tree_changed(tree);
                    return true;
                }
            }
        }
    }
    false
}

/// Whether a person at this machine may rename `tab`: it shows a host
/// (non-Tilo) window, whose title is the window's own rather than an app's
/// resource, and it is in this device's id namespace (the single writer that
/// projects it to the room) or in none. A window hosted by another device is
/// that device's to rename, as a strip is
/// ([`WorkspaceTree::may_rename_strip`]).
pub fn may_rename_tab(tree: &WorkspaceTree, tab_id: TabId) -> bool {
    tree.desktop
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .find(|tab| tab.id == tab_id)
        .is_some_and(|tab| {
            let host_window = match &tab.tile {
                TileContent::Foreign { app_id, .. } => {
                    tree.package_launches.get(&tab.id) != Some(app_id)
                }
                TileContent::HostedWindow { .. } => true,
                _ => false,
            };
            let salt = crate::shell::state::peer_salt_of(tab.id);
            host_window && (salt == 0 || salt == tree.local_salt())
        })
}

/// A person names host window `tab_id` `name` ([`Tab::rename`]); a blank
/// name clears theirs, and the tab shows its window's latest title again.
/// The change is recorded like any other change to the tree: the desktop
/// document, the room on the next push, and the restart snapshot. `None`
/// when the tab may not be renamed here ([`may_rename_tab`]), else whether
/// anything changed.
pub fn rename_host_tab(tree: &mut WorkspaceTree, tab_id: TabId, name: &str) -> Option<bool> {
    if !may_rename_tab(tree, tab_id) {
        return None;
    }
    let changed = tree
        .desktop
        .workspaces
        .iter_mut()
        .flat_map(|workspace| workspace.strips.iter_mut())
        .flat_map(|strip| strip.window_groups.iter_mut())
        .flat_map(|group| group.tabs.iter_mut())
        .find(|tab| tab.id == tab_id)
        .is_some_and(|tab| tab.rename(name));
    if changed {
        tree.seed_doc_from_cache();
        mark_tree_changed(tree);
        tree.queue_save();
    }
    Some(changed)
}

/// What a foreign window's title says about where it is, without the app's
/// own name: the bar already shows the app's icon, so `Q3 report.odt -
/// LibreOffice Writer` is `Q3 report.odt`. Only the app's exact name,
/// joined by one of the usual separators at either end, is removed; any
/// other title is left as the app wrote it. A title that is nothing but the
/// app's name stays its name: the app shows no location.
pub(crate) fn location_from_title(title: &str, app_name: &str) -> String {
    const SEPARATORS: [&str; 4] = [" - ", " \u{2014} ", " \u{2013} ", " | "];
    let name = app_name.trim();
    let trimmed = title.trim();
    if name.is_empty() || trimmed.eq_ignore_ascii_case(name) {
        return trimmed.to_owned();
    }
    for separator in SEPARATORS {
        let suffix = format!("{separator}{name}");
        if let Some(split) = trimmed.len().checked_sub(suffix.len())
            && split > 0
            && trimmed
                .get(split..)
                .is_some_and(|tail| tail.eq_ignore_ascii_case(&suffix))
            && let Some(head) = trimmed.get(..split)
        {
            return head.trim().to_owned();
        }
        let prefix = format!("{name}{separator}");
        if trimmed.len() > prefix.len()
            && trimmed
                .get(..prefix.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(&prefix))
            && let Some(tail) = trimmed.get(prefix.len()..)
        {
            return tail.trim().to_owned();
        }
    }
    trimmed.to_owned()
}

/// Resolve the [`GroupId`] owning the `TileContent::Foreign` tab bound to `surface_id`. Used by
/// `xdg_toplevel.set_fullscreen` to find which group to promote; `None` when the surface was never adopted (e.g. racing surface teardown).
pub fn group_id_for_surface(tree: &WorkspaceTree, surface_id: u64) -> Option<GroupId> {
    for ws in &tree.desktop.workspaces {
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

/// Whether focus left the full-screen `group` for another window of its
/// strip, so the full screen should end: the strip's active group is a
/// different one. A window whose adoption is still provisional (no frame
/// yet; it may turn out to be a dialog and be withdrawn, handing focus
/// back) does not count yet. A group no strip holds any more has nothing
/// to keep full screen.
pub fn focus_left_fullscreen_group(
    tree: &WorkspaceTree,
    group: GroupId,
    provisional: impl Fn(u64) -> bool,
) -> bool {
    let Some(strip) = tree
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .find(|strip| strip.window_groups.iter().any(|g| g.id == group))
    else {
        return true;
    };
    let Some(active) = tree.desktop.active_group_for(strip.id) else {
        return false;
    };
    if active == group {
        return false;
    }
    !group_holds_provisional_window(tree, active, provisional)
}

/// Whether `group` holds a window still opening without a frame, whose
/// adoption is provisional: it may turn out to be a dialog and be
/// withdrawn, handing focus back.
pub fn group_holds_provisional_window(
    tree: &WorkspaceTree,
    group: GroupId,
    provisional: impl Fn(u64) -> bool,
) -> bool {
    tree.desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .find(|g| g.id == group)
        .is_some_and(|g| {
            g.tabs.iter().any(|tab| {
                matches!(&tab.tile, TileContent::Foreign { surface_id, .. } if provisional(*surface_id))
            })
        })
}

/// Bring the already-adopted foreign toplevel to the front, including a
/// workspace/strip transition when it lives outside the current view.
///
/// This is the compositor-side effect of an accepted `xdg_activation_v1`
/// request. It deliberately mutates only local focus pointers; no window is
/// created, moved, or written into collaborative continuity state.
pub fn activate_surface(tree: &mut WorkspaceTree, surface_id: u64) -> bool {
    let location = tree.desktop.workspaces.iter().find_map(|workspace| {
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

    if !bring_forward(tree, workspace_id, strip_id, group_id, tab_id) {
        return false;
    }
    mark_tree_changed(tree);
    tree.queue_save();
    true
}

/// App id recorded for a foreign surface, if it has been adopted.
pub fn app_id_for_surface(tree: &WorkspaceTree, surface_id: u64) -> Option<&str> {
    tree.desktop
        .workspaces
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
/// `destroyed_surface_id`, remove it, return `true` if found. [`remove_tab`]
/// also removes a group left empty by the close and advances focus off it, so
/// a host/X11 window cannot leave a dead column for the next open or reorder.
pub fn close_tab_for_destroyed_surface(
    tree: &mut WorkspaceTree,
    destroyed_surface_id: u64,
) -> bool {
    foreign_tab_for_surface(tree, destroyed_surface_id)
        .is_some_and(|tab_id| remove_tab(tree, tab_id))
}

pub(crate) fn tab_for_surface(tree: &WorkspaceTree, surface_id: u64) -> Option<TabId> {
    foreign_tab_for_surface(tree, surface_id)
}

fn foreign_tab_for_surface(tree: &WorkspaceTree, surface_id: u64) -> Option<TabId> {
    tree.desktop
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .find_map(|tab| match &tab.tile {
            TileContent::Foreign { surface_id: id, .. } if *id == surface_id => Some(tab.id),
            _ => None,
        })
}

/// Remove an adopted client which never produced a frame after its launcher
/// reported failure.  A painted foreign surface is deliberately left alone:
/// a launcher parent can fail after handing off to a healthy GUI child, and
/// that visible client must remain usable.
///
/// Removing the tab through [`remove_tab`] also repairs the focused group,
/// returning focus to the adjacent group that was active before this
/// placeholder was opened.
pub fn retire_starting_foreign_surface(tree: &mut WorkspaceTree, surface_id: u64) -> bool {
    let tab_id = tree
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .find_map(|tab| {
            matches!(&tab.tile, TileContent::Foreign { surface_id: id, .. } if *id == surface_id)
                .then_some(tab.id)
        });
    let Some(tab_id) = tab_id else {
        return false;
    };
    if crate::shell::foreign::store().frame_seq(surface_id) != 0 {
        return false;
    }
    let removed = remove_tab(tree, tab_id);
    if removed {
        // A live Foreign tile serialises as a retryable LocalApp.  Failure is
        // terminal for this unpainted launch, so persist its removal before a
        // restart can turn the dead placeholder back into another launch.
        tree.queue_save();
    }
    removed
}

/// Remove a destroyed foreign surface and immediately persist the authoritative
/// tree.  Ordinary reconciliation also uses [`close_tab_for_destroyed_surface`]
/// while it is converging two in-memory trees; only the protocol-owned close
/// paths use this wrapper because a surface destruction is a durable user
/// decision and must not leave its relaunchable `LocalApp` record on disk.
pub fn close_tab_for_destroyed_surface_and_persist(
    tree: &mut WorkspaceTree,
    destroyed_surface_id: u64,
) -> bool {
    // The window really went away: the group returns to the tab that was in
    // front before it, not to whatever sits at its index. (Re-adoption and
    // dialog promotion also remove a provisional tab, but those keep the
    // plain rule through [`close_tab_for_destroyed_surface`].)
    let removed = foreign_tab_for_surface(tree, destroyed_surface_id)
        .is_some_and(|tab_id| close_tab_back_to_previous(tree, tab_id));
    if removed {
        tree.queue_save();
    }
    removed
}

/// Which strip each workspace showed and which group each strip had in
/// front, taken just before a new toplevel was adopted.
///
/// A toplevel is adopted as a tab, with focus, the moment it is created,
/// before the client has said what it is. When it then turns out to be no
/// window of the strip (a dialog, which becomes a sheet on its parent; a
/// floating or presentation surface; the keyboard), or goes away before it
/// ever showed a frame, its provisional tab is withdrawn. Plain removal hands
/// focus to whatever group now sits at the withdrawn one's index, the one
/// right of where the person was, and the strip pans to it although nothing
/// the person did asked for that. This is what withdrawal restores instead.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FocusBefore {
    active_strip: std::collections::HashMap<crate::shell::state::WorkspaceId, StripId>,
    active_group: std::collections::HashMap<StripId, GroupId>,
}

impl FocusBefore {
    pub fn of(tree: &WorkspaceTree) -> Self {
        Self {
            active_strip: tree.desktop.active_strip.clone(),
            active_group: tree.desktop.active_group.clone(),
        }
    }
}

/// Withdraw the provisional tab of `surface_id` (see [`FocusBefore`]) and,
/// if that tab still held its strip's focus, give focus back to the group
/// and strip that had it before the adoption. A tab the toplevel was bound
/// into an existing group returns that group to the tab in front before it.
pub fn withdraw_provisional_tab(
    tree: &mut WorkspaceTree,
    surface_id: u64,
    before: &FocusBefore,
) -> bool {
    let Some(tab_id) = foreign_tab_for_surface(tree, surface_id) else {
        return false;
    };
    let held = tree.desktop.workspaces.iter().find_map(|workspace| {
        workspace.strips.iter().find_map(|strip| {
            strip
                .window_groups
                .iter()
                .find(|group| group.tabs.iter().any(|tab| tab.id == tab_id))
                .filter(|group| tree.desktop.active_group_for(strip.id) == Some(group.id))
                .map(|_| (workspace.id, strip.id))
        })
    });
    // What stepped back for this window is given back once focus is where
    // it was, not to whatever the removal alone leaves focused.
    let lent: Vec<crate::shell::stepped_back::SteppedBack> = tree
        .stepped_back
        .iter()
        .filter(|held| held.for_tab == tab_id)
        .copied()
        .collect();
    tree.stepped_back.retain(|held| held.for_tab != tab_id);
    let withdrawn = close_tab_back_to_previous(tree, tab_id);
    tree.stepped_back.extend(lent);
    if !withdrawn {
        return false;
    }
    restore_focus_before(tree, held, before);
    crate::shell::stepped_back::give_back_place(tree, tab_id);
    true
}

/// Focus back on the group and strip `before` had, where the withdrawn
/// window held its strip's focus (`held`).
fn restore_focus_before(
    tree: &mut WorkspaceTree,
    held: Option<(crate::shell::state::WorkspaceId, StripId)>,
    before: &FocusBefore,
) {
    let Some((workspace_id, strip_id)) = held else {
        return;
    };
    let exists = |tree: &WorkspaceTree, strip: StripId, group: Option<GroupId>| {
        tree.desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .and_then(|workspace| workspace.strips.iter().find(|s| s.id == strip))
            .is_some_and(|s| group.is_none_or(|g| s.window_groups.iter().any(|w| w.id == g)))
    };
    if let Some(&group) = before.active_group.get(&strip_id)
        && exists(tree, strip_id, Some(group))
    {
        tree.desktop.set_active_group(strip_id, group);
    }
    if tree.desktop.active_strip.get(&workspace_id) == Some(&strip_id)
        && let Some(&strip) = before.active_strip.get(&workspace_id)
        && exists(tree, strip, None)
    {
        tree.desktop.active_strip.insert(workspace_id, strip);
    }
}

/// Remove `tab_id` from wherever it is in the tree, repairing its group's selection if it was in
/// front. The undo for a placement whose window never came — a failed launch takes its
/// placeholder away rather than leaving a dead card. [`close_tab_for_destroyed_surface`] is the same removal keyed on a surface.
pub fn remove_tab(tree: &mut WorkspaceTree, tab_id: TabId) -> bool {
    remove_tab_choosing(tree, tab_id, false)
}

/// [`remove_tab`] for a window the person closed: the group returns to the tab that was in front
/// before it, and the strip to the window that was focused before it.
pub(crate) fn close_tab_back_to_previous(tree: &mut WorkspaceTree, tab_id: TabId) -> bool {
    remove_tab_choosing(tree, tab_id, true)
}

#[allow(
    clippy::indexing_slicing,
    reason = "the locations are produced by a complete immutable traversal immediately above and the tree is not mutated until the indexed edits begin"
)]
fn remove_tab_choosing(tree: &mut WorkspaceTree, tab_id: TabId, back_to_previous: bool) -> bool {
    let located = tree
        .desktop
        .workspaces
        .iter()
        .enumerate()
        .find_map(|(ws_idx, ws)| {
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
                            .map(|tab_idx| {
                                (ws_idx, strip_idx, group_idx, tab_idx, strip.id, group.id)
                            })
                    })
            })
        });
    let Some((ws_idx, strip_idx, group_idx, tab_idx, strip_id, group_id)) = located else {
        return false;
    };

    let remove_group = tree.desktop.workspaces[ws_idx].strips[strip_idx].window_groups[group_idx]
        .tabs
        .len()
        == 1;
    let cache_only = tree
        .desktop
        .workspaces
        .get(ws_idx)
        .and_then(|workspace| workspace.strips.get(strip_idx))
        .and_then(|strip| strip.window_groups.get(group_idx))
        .and_then(|group| group.tabs.get(tab_idx))
        .is_some_and(|tab| matches!(&tab.tile, TileContent::Foreign { .. }))
        || !tree.doc_holds_tab(strip_id, group_id, tab_id, remove_group);
    // Delete from the CRDT before touching the cache. Besides making the
    // ordering explicit, this guarantees that a revision/peer merge between
    // the two representations cannot treat the stale document entry as an
    // insertion and resurrect it. A newly adopted `Foreign` surface and a
    // tab placed since the document last took the cache (an opening card
    // whose launch was refused in the same frame) may never have entered the replicated
    // document; absence there is therefore already the desired durable
    // state, not a failed close.
    if !tree.doc_remove_tab(strip_id, group_id, tab_id, remove_group) && !cache_only {
        return false;
    }

    let removed = if back_to_previous {
        tree.desktop.close_tab(tab_id)
    } else {
        tree.desktop.remove_tab(tab_id)
    };
    let Some(removed) = removed.map(|removed| removed.tab) else {
        return false;
    };
    tree.local_app_records.remove(&tab_id);
    crate::shell::foreign::store().forget_opening_tab(tab_id);
    if let Some(room) = tree.continuation_owned_tabs.remove(&tab_id) {
        tree.continuation_closed_tabs.insert(tab_id, room);
    }
    if let Ok(mut registry) = crate::shell::personal_continuity_registry::global_registry().lock() {
        registry.forget_local_tab(crate::shell::personal_continuity::AppInstanceId(removed.id));
    }
    mark_tree_changed(tree);
    crate::shell::stepped_back::give_back_place(tree, tab_id);
    true
}

/// Turn a wayland app-id into a tab-bar label. The app's own name wins (this session's
/// launches, the installed package's manifest, or a host `.desktop` entry), since a tab must
/// never show a raw machine identifier. Otherwise the trailing component is humanised
/// (`so.tilo.word-processor` → `Word Processor`), the same transform the package registry
/// applies. Shared with window cards, so an app is never named two different ways.
pub(crate) fn tab_label_for_foreign(app_id: &str) -> String {
    if let Some(name) = crate::shell::launcher::display_name_for_app_id(app_id) {
        return name;
    }
    // A legacy VM's window: its installed app's search entry names it (found
    // above by `StartupWMClass`); otherwise its own id, and always the guest.
    if let Some((guest, inner)) = legacy_agent_protocol::window::split_host_app_id(app_id) {
        let name = crate::shell::launcher::display_name_for_package_id(inner);
        let name = if name.trim().is_empty() {
            inner.to_string()
        } else {
            name
        };
        let name: String = name.chars().take(24).collect();
        return format!("{name} ({})", guest.label());
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
    use std::collections::BTreeMap;

    /// The group holding the foreign tab for `surface_id`, wherever placement put it — tests assert
    /// on the adopted tab, not a fixed index, so a placement change fails as a placement assertion, not five unrelated ones.
    fn group_of(tree: &WorkspaceTree, surface_id: u64) -> &crate::shell::state::WindowGroup {
        tree.desktop.workspaces
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
        let strip_id = tree.desktop.workspaces[0].strips[0].id;
        let group = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
        group.tabs.truncate(1);
        group.active_tab = group.tabs[0].id;
        let group_id = group.id;
        tree.desktop
            .active_strip
            .insert(tree.desktop.active, strip_id);
        tree.desktop.set_active_group(strip_id, group_id);
        (strip_id, group_id)
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn activation_focuses_and_reveals_the_exact_surface_and_its_group() {
        {
            let dir = tempfile::tempdir().expect("state dir");
            let path = dir.path().join("workspace.json");
            let mut tree = WorkspaceTree::bootstrap_demo();
            tree.persist_path = Some(path.clone());
            let original_workspace = tree.desktop.active;
            let surface = 0xabc;
            let tab = adopt_toplevel(&mut tree, surface, Some("org.example.Activated".into()));
            let target_strip = tree.desktop.active_strip[&original_workspace];
            let target_group = group_of(&tree, surface).id;

            let other = tree.desktop.workspaces[1].id;
            tree.set_active(other);
            assert!(activate_surface(&mut tree, surface));

            let restarted = crate::shell::persist::load_tree_from(&path).expect("activation state");
            assert_eq!(restarted.desktop.active, original_workspace);
            assert_eq!(
                restarted.desktop.active_strip.get(&original_workspace),
                Some(&target_strip)
            );
            assert_eq!(
                restarted.desktop.active_group_for(target_strip),
                Some(target_group)
            );
            let group = restarted.desktop.workspaces[0].strips[0]
                .window_groups
                .iter()
                .find(|group| group.id == target_group)
                .expect("activated group");
            assert_eq!(group.active_tab, tab);
        }

        {
            crate::shell::launcher::clear_pending_placements();
            let mut tree = WorkspaceTree::bootstrap_demo();
            let _ = adopt_toplevel(&mut tree, 7001, Some("org.example.Editor".into()));
            let original_workspace = tree.desktop.active;
            let original_group = group_of(&tree, 7001).id;
            let other = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.id != original_workspace)
                .map(|workspace| workspace.id)
                .expect("demo has another workspace");
            tree.desktop.active = other;

            assert!(activate_surface(&mut tree, 7001));
            assert_eq!(tree.desktop.active, original_workspace);
            assert_eq!(group_id_for_surface(&tree, 7001), Some(original_group));
            assert_eq!(app_id_for_surface(&tree, 7001), Some("org.example.Editor"));
            assert!(!activate_surface(&mut tree, 9999));
        }

        {
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
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn launch_placements_are_claimed_exactly_once_by_the_matching_window() {
        {
            // Phase `a_window_started_for_an_existing_tab_lands_in_it`.
            // A window started for a tab the room already has lands *in* that tab, not in a second one beside it.
            {
                crate::shell::launcher::clear_pending_placements();
                let mut tree = WorkspaceTree::bootstrap_demo();
                let target = {
                    let group = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
                    let tab = Tab {
                        id: next_id(),
                        title: "Word Processor".into(),
                        tile: TileContent::PeerWindow {
                            app_id: "so.tilo.word-processor".into(),
                            label: "Word Processor".into(),
                            note: "Opening here…".into(),
                            missing: None,
                        },
                        app_title: None,
                    };
                    let id = tab.id;
                    group.tabs.push(tab);
                    id
                };
                let groups_before = tree.desktop.workspaces[0].strips[0].window_groups.len();
                let tabs_before: usize = tree.desktop.workspaces[0].strips[0]
                    .window_groups
                    .iter()
                    .map(|g| g.tabs.len())
                    .sum();

                crate::shell::launcher::set_pending_placement(
                    "so.tilo.word-processor",
                    Placement::IntoTab(target),
                );
                let landed = adopt_toplevel(&mut tree, 7777, Some("so.tilo.word-processor".into()));

                assert_eq!(
                    landed, target,
                    "the window belongs to the tab it was started for"
                );
                let tabs_after: usize = tree.desktop.workspaces[0].strips[0]
                    .window_groups
                    .iter()
                    .map(|g| g.tabs.len())
                    .sum();
                assert_eq!(
                    tabs_after, tabs_before,
                    "binding into an existing tab must not mint a second one"
                );
                assert_eq!(
                    tree.desktop.workspaces[0].strips[0].window_groups.len(),
                    groups_before,
                    "nor a second group to put it in"
                );
                match &tab_of(&tree, 7777).tile {
                    TileContent::Foreign { app_id, surface_id } => {
                        assert_eq!(app_id, "so.tilo.word-processor");
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

            // Phase `activation_relocation_keeps_claimed_target_and_retires_host_launch_note`.
            // A host launch can be adopted by the ordinary identity path just before
            // its activation request arrives.  Consuming the activation must retain
            // that existing `IntoTab` binding, then retire the launch note so a
            // second, unrelated window with the same class cannot inherit it.
            {
                crate::shell::launcher::clear_pending_placements();
                let mut tree = WorkspaceTree::bootstrap_demo();
                let target = tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id;
                let identity = "org.example.Host".to_string();
                crate::shell::launcher::set_pending_placement_for_identities(
                    "org.example.Host.desktop",
                    std::slice::from_ref(&identity),
                    Placement::IntoTab(target),
                );

                assert_eq!(
                    adopt_toplevel(&mut tree, 0xfeed, Some(identity.clone())),
                    target,
                    "the initial identity adoption claimed the reserved tab"
                );
                crate::shell::launcher::clear_pending_placement("org.example.Host.desktop");
                assert!(relocate_foreign_toplevel(
                    &mut tree,
                    0xfeed,
                    Placement::IntoTab(target),
                    "Host application",
                ));
                assert_eq!(
                    tab_of(&tree, 0xfeed).id,
                    target,
                    "relocation must not delete an already-claimed IntoTab target"
                );
                assert_eq!(tab_of(&tree, 0xfeed).title, "Host application");
                assert_eq!(
                    tree.desktop.workspaces
                        .iter()
                        .flat_map(|workspace| workspace.strips.iter())
                        .flat_map(|strip| strip.window_groups.iter())
                        .flat_map(|group| group.tabs.iter())
                        .filter(|tab| matches!(&tab.tile, TileContent::Foreign { surface_id, .. } if *surface_id == 0xfeed))
                        .count(),
                    1,
                    "the same toplevel remains represented by exactly one local tab"
                );
                assert!(
                    crate::shell::launcher::placement_for_toplevel(&identity, 0xbeef).is_none(),
                    "the token's desktop-id note is gone before another matching window arrives"
                );
                let later = adopt_toplevel(&mut tree, 0xbeef, Some(identity));
                assert_ne!(
                    later, target,
                    "a later matching window must not replace the token-bound surface"
                );
                crate::shell::launcher::clear_pending_placements();
            }

            // Phase `a_late_activation_moves_the_window_without_replacing_its_group`.
            // Chromium maps its window, then activates its launch token seconds
            // later. The move to the launch's placement keeps the window's tab
            // and group ids: a group that vanished and reappeared under a new
            // id lost every reference to it (A08).
            {
                crate::shell::launcher::clear_pending_placements();
                let mut tree = WorkspaceTree::bootstrap_demo();
                let first = tree.desktop.workspaces[0].strips[0].window_groups[0].id;
                let tab = adopt_toplevel_at(
                    &mut tree,
                    0xc0de,
                    Some("chromium-browser".into()),
                    Placement::NewGroup,
                );
                let group = group_of(&tree, 0xc0de).id;
                let groups_before = tree.desktop.workspaces[0].strips[0].window_groups.len();

                assert!(relocate_foreign_toplevel(
                    &mut tree,
                    0xc0de,
                    Placement::RightOf(first),
                    "Chromium",
                ));
                let strip = &tree.desktop.workspaces[0].strips[0];
                assert_eq!(
                    group_of(&tree, 0xc0de).id,
                    group,
                    "the window keeps its group"
                );
                assert_eq!(tab_of(&tree, 0xc0de).id, tab, "and its tab");
                assert_eq!(group_of(&tree, 0xc0de).active_tab, tab);
                assert_eq!(tab_of(&tree, 0xc0de).title, "Chromium");
                assert_eq!(
                    strip.window_groups.len(),
                    groups_before,
                    "no group is added"
                );
                assert_eq!(
                    strip.window_groups.iter().position(|g| g.id == group),
                    strip
                        .window_groups
                        .iter()
                        .position(|g| g.id == first)
                        .map(|i| i + 1),
                    "it moved where the launch asked for it"
                );
                assert_eq!(
                    tree.desktop.active_group_for(strip.id),
                    Some(group),
                    "and stays the focused group"
                );
            }

            // Phase `a_window_whose_tab_vanished_is_still_placed`.
            // The tab was closed by its owner while the local instance was starting; placement degrades to the ordinary launch rule.
            {
                crate::shell::launcher::clear_pending_placements();
                let mut tree = WorkspaceTree::bootstrap_demo();
                let groups_before = tree.desktop.workspaces[0].strips[0].window_groups.len();

                crate::shell::launcher::set_pending_placement(
                    "so.tilo.gone",
                    // An id no tab in the tree carries.
                    Placement::IntoTab(next_id()),
                );
                let landed = adopt_toplevel(&mut tree, 7878, Some("so.tilo.gone".into()));

                assert_eq!(
                    tree.desktop.workspaces[0].strips[0].window_groups.len(),
                    groups_before + 1,
                    "a window with nowhere booked still opens its own group"
                );
                assert_eq!(tab_of(&tree, 7878).id, landed);
            }

            // Phase `a_recorded_placement_is_consumed_by_the_matching_adoption`.
            // The launch-time note and the adoption are two events on two processes; this is the whole correlation, driven end to end.
            {
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
                        focus_history: Vec::new(),
                    };
                    let id = g.id;
                    tree.desktop.workspaces[0].strips[0].window_groups.push(g);
                    id
                };
                let groups_before = tree.desktop.workspaces[0].strips[0].window_groups.len();

                launcher::set_pending_placement("so.tilo.placefixture", Placement::TabIn(named));
                let _ = adopt_toplevel(&mut tree, 8080, Some("placefixture".into()));
                assert_eq!(
                    tree.desktop.workspaces[0].strips[0].window_groups.len(),
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
                tree.desktop.set_active_group(strip_id, selected);

                // The note belongs to that first window: a *second* window from the same client is a new thing again, and takes the rule.
                let _ = adopt_toplevel(&mut tree, 8081, Some("placefixture".into()));
                assert_eq!(
                    tree.desktop.workspaces[0].strips[0].window_groups.len(),
                    groups_before + 1,
                    "a claimed placement must not apply to the next window too"
                );
                assert_eq!(
                    tree.desktop.workspaces[0].strips[0]
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

            // Phase `a_tab_placement_naming_a_dead_group_falls_back_to_a_new_group`.
            // The opener can be gone by the time the document's window maps. That degrades to the launch placement rather than dropping the window into an arbitrary group.
            {
                crate::shell::launcher::clear_pending_placements();
                let mut tree = WorkspaceTree::bootstrap_demo();
                let strip_id = tree.desktop.workspaces[0].strips[0].id;
                let focused = tree.desktop.workspaces[0].strips[0].window_groups[0].id;
                tree.desktop
                    .active_strip
                    .insert(tree.desktop.active, strip_id);
                tree.desktop.set_active_group(strip_id, focused);
                let groups_before = tree.desktop.workspaces[0].strips[0].window_groups.len();

                let _ = adopt_toplevel_at(
                    &mut tree,
                    7070,
                    Some("org.example.orphan".into()),
                    // A group id that never existed in this tree.
                    Placement::TabIn(999_999_999),
                );

                assert_eq!(
                    tree.desktop.workspaces[0].strips[0].window_groups.len(),
                    groups_before + 1,
                    "a placement naming a group that is gone opens a new one"
                );
                assert_eq!(group_of(&tree, 7070).tabs.len(), 1);
            }
        }

        {
            // Phase `late_app_id_claims_placement_once_and_does_not_leak_to_next_window`.
            // Electron/GTK may create the xdg-toplevel before publishing its app-id.
            // The anonymous first adoption must still claim the launch placement when
            // the identity arrives; otherwise the next same-app window consumes the
            // stale note and lands in the first launch's intended group.
            {
                crate::shell::launcher::clear_pending_placements();
                let mut tree = WorkspaceTree::bootstrap_demo();
                let (_strip, opener) = focus_lone_window(&mut tree);
                crate::shell::launcher::set_pending_placement(
                    "org.example.electron",
                    Placement::TabIn(opener),
                );

                // The first protocol callback has no app-id yet, so it follows the
                // ordinary fallback and is initially beside the focused group.
                let first = adopt_toplevel(&mut tree, 0x101, None);
                assert_ne!(group_of(&tree, 0x101).id, opener);
                assert!(rebind_toplevel_for_app_id(
                    &mut tree,
                    0x101,
                    "org.example.electron",
                    None,
                ));
                assert_eq!(group_of(&tree, 0x101).id, opener);
                assert_eq!(tab_of(&tree, 0x101).title, "Electron");
                assert_eq!(
                    app_id_for_surface(&tree, 0x101),
                    Some("org.example.electron"),
                    "late placement must replace the synthetic identity as well as the title"
                );

                // A second window must not inherit the one-shot placement claimed by
                // the first surface, even when it announces the app-id immediately.
                // Move focus to a distinct one-tab group so the ordinary placement
                // rule is observably different from the original `TabIn(opener)`
                // launch request.
                let _ = adopt_toplevel_at(
                    &mut tree,
                    0x999,
                    Some("org.example.anchor".into()),
                    Placement::NewGroup,
                );
                let groups_before = tree.desktop.workspaces[0].strips[0].window_groups.len();
                let second = adopt_toplevel(&mut tree, 0x102, Some("org.example.electron".into()));
                assert_ne!(second, first);
                assert_ne!(
                    group_of(&tree, 0x102).id,
                    opener,
                    "the second surface must follow current placement, not the claimed launch note"
                );
                assert_eq!(
                    tree.desktop.workspaces[0].strips[0].window_groups.len(),
                    groups_before + 1,
                    "the claimed late identity note must not place the next window as a tab"
                );
                crate::shell::launcher::clear_pending_placements();
            }

            // Phase `changed_provisional_x11_app_id_claims_pending_placement`.
            // Xwayland bridges can publish a provisional WM_CLASS-derived app-id
            // before the final class reaches `app_id_changed`. It is no more
            // authoritative than `anon-*`: a matching launch note must still rehome
            // that exact surface when its identity changes.
            {
                crate::shell::launcher::clear_pending_placements();
                let mut tree = WorkspaceTree::bootstrap_demo();
                let target = tree.desktop.workspaces[0].strips[0].window_groups[0].id;
                crate::shell::launcher::set_pending_placement_for_identities(
                    "chatgpt",
                    &["chatgpt".to_string()],
                    Placement::NewGroup,
                );

                let surface = 0x5151;
                let _ = adopt_toplevel_at(
                    &mut tree,
                    surface,
                    Some("electron".into()),
                    Placement::TabIn(target),
                );
                assert_eq!(group_of(&tree, surface).id, target);
                assert!(rebind_toplevel_for_app_id(
                    &mut tree,
                    surface,
                    "Chatgpt",
                    Some("ChatGPT"),
                ));
                assert_ne!(group_of(&tree, surface).id, target);
                assert_eq!(app_id_for_surface(&tree, surface), Some("Chatgpt"));
                assert_eq!(tab_of(&tree, surface).title, "ChatGPT");
                crate::shell::launcher::clear_pending_placements();
            }
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn launches_documents_and_drops_land_beside_or_inside_the_right_group() {
        {
            // Phase `a_launch_beside_a_lone_window_opens_its_own_group_to_the_right`.
            // A window opened while a single window has focus is a new thing beside it: its own column, immediately to the right.
            {
                crate::shell::launcher::clear_pending_placements();
                let mut tree = WorkspaceTree::bootstrap_demo();
                let (strip_id, _focused) = focus_lone_window(&mut tree);
                let groups_before = tree.desktop.workspaces[0].strips[0].window_groups.len();
                let tabs_in_focused = tree.desktop.workspaces[0].strips[0].window_groups[0]
                    .tabs
                    .len();

                let tab_id = adopt_toplevel(&mut tree, 4242, Some("org.example.app".into()));

                let strip = &tree.desktop.workspaces[0].strips[0];
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
                    tree.desktop.active_group_for(strip_id).unwrap(),
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

            // Phase `a_launch_from_inside_a_tabbed_group_becomes_the_next_tab_in_it`.
            // Selection inside a group of several tabs joins the launch as a tab right of the selection, rather than splitting into another column.
            {
                crate::shell::launcher::clear_pending_placements();
                let mut tree = WorkspaceTree::bootstrap_demo();
                let strip_id = tree.desktop.workspaces[0].strips[0].id;
                // `bootstrap_demo`'s group holds four tabs. Select the second one, so "immediately right of the selected tab" is distinct from both ends of the bar.
                let selected_idx = 1usize;
                let (group_id, selected_tab, tab_ids_before) = {
                    let g = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
                    assert!(g.tabs.len() > 2, "fixture: a genuinely tabbed group");
                    g.activate_tab(g.tabs[selected_idx].id);
                    (
                        g.id,
                        g.active_tab,
                        g.tabs.iter().map(|t| t.id).collect::<Vec<_>>(),
                    )
                };
                tree.desktop
                    .active_strip
                    .insert(tree.desktop.active, strip_id);
                tree.desktop.set_active_group(strip_id, group_id);
                let groups_before = tree.desktop.workspaces[0].strips[0].window_groups.len();

                let tab_id = adopt_toplevel(&mut tree, 4343, Some("org.example.joins".into()));

                let strip = &tree.desktop.workspaces[0].strips[0];
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

            // Phase `the_new_group_is_inserted_beside_the_focused_group_not_at_the_end`.
            // The new column goes right of the focused group, not the end of the strip — next to the one it was opened from, so the strip doesn't scroll away.
            {
                use crate::shell::state::{WindowGroup, WindowWidth, next_id};
                crate::shell::launcher::clear_pending_placements();
                let mut tree = WorkspaceTree::bootstrap_demo();
                let (_strip_id, focused) = focus_lone_window(&mut tree);
                // Two more groups after the first, so "beside" and "at the end" are distinguishable positions.
                for _ in 0..2 {
                    tree.desktop.workspaces[0].strips[0]
                        .window_groups
                        .push(WindowGroup {
                            id: next_id(),
                            tabs: vec![],
                            active_tab: 0,
                            width: WindowWidth::Normal,
                            maximized: false,
                            focus_history: Vec::new(),
                        });
                }
                let last_before = tree.desktop.workspaces[0].strips[0].window_groups[2].id;

                let _ = adopt_toplevel(&mut tree, 5150, Some("org.example.beside".into()));

                let strip = &tree.desktop.workspaces[0].strips[0];
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

            // Phase `a_hosted_app_opened_beside_a_lone_window_takes_the_column_to_its_right`.
            // A shell-hosted app opened beside a lone window takes the column to the right, landing as a tile resolving to a live app widget rather than a stub that looks identical in the tree but blank on screen.
            {
                let mut tree = WorkspaceTree::bootstrap_demo();
                let (strip_id, focused) = focus_lone_window(&mut tree);
                let groups_before = tree.desktop.workspaces[0].strips[0].window_groups.len();

                let placement = crate::shell::placement::placement_for(
                    &tree,
                    tilo_desktop_model::commands::OpenIntent::Launch,
                    None,
                );
                assert_eq!(
                    placement,
                    Placement::RightOf(focused),
                    "a lone selected window puts the next window beside it"
                );
                let tab_id = open_hosted_app(&mut tree, AppKind::FileExplorer, "Files", placement);

                let strip = &tree.desktop.workspaces[0].strips[0];
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
                    tree.desktop.active_group_for(strip_id).unwrap(),
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

            // Phase `both_trees_place_the_same_toplevel_the_same_way`.
            // Independent tree consumers must apply an explicit placement the same
            // way. Production now shares one handle; this regression retains the
            // lower-level placement contract for callers that still reconcile copies.
            {
                use crate::shell::launcher;
                launcher::clear_pending_placements();

                let mut protocol = WorkspaceTree::bootstrap_demo();
                // A lone selected window, so the rule's answer (beside it) differs from the note's (inside it) — without that the two placements coincide and the test proves nothing.
                let (_strip_id, opener) = focus_lone_window(&mut protocol);
                // The rendered tree is a clone of the persisted one, so the two share group ids — which is what makes a placement naming a group meaningful across both.
                let mut rendered = protocol.clone();
                let groups_before = rendered.desktop.workspaces[0].strips[0].window_groups.len();

                launcher::set_pending_placement("so.tilo.twotrees", Placement::TabIn(opener));

                // Protocol side adopts first, then the rendered tree mirrors it.
                let _ = adopt_toplevel(&mut protocol, 4711, Some("twotrees".into()));
                let _ = adopt_toplevel(&mut rendered, 4711, Some("twotrees".into()));

                assert_eq!(
                    rendered.desktop.workspaces[0].strips[0].window_groups.len(),
                    groups_before,
                    "the rendered tree must honour the same TabIn placement the \
                     protocol tree did, not fall back to opening a column"
                );
                assert_eq!(
                    protocol.desktop.workspaces[0].strips[0].window_groups.len(),
                    groups_before,
                    "fixture sanity: the protocol tree honoured it too"
                );
                launcher::clear_pending_placements();
            }
        }

        {
            // Phase `a_document_opened_from_an_app_becomes_a_tab_in_that_apps_group`.
            // A document opened from another app is the same task as the app that asked for it, so it joins that app's group as a tab.
            {
                crate::shell::launcher::clear_pending_placements();
                let mut tree = WorkspaceTree::bootstrap_demo();
                let strip_id = tree.desktop.workspaces[0].strips[0].id;
                let opener = tree.desktop.workspaces[0].strips[0].window_groups[0].id;
                tree.desktop
                    .active_strip
                    .insert(tree.desktop.active, strip_id);
                tree.desktop.set_active_group(strip_id, opener);
                let groups_before = tree.desktop.workspaces[0].strips[0].window_groups.len();
                let tabs_before = tree.desktop.workspaces[0].strips[0].window_groups[0]
                    .tabs
                    .len();

                let tab_id = adopt_toplevel_at(
                    &mut tree,
                    6060,
                    Some("org.example.editor".into()),
                    Placement::TabIn(opener),
                );

                let strip = &tree.desktop.workspaces[0].strips[0];
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

            {
                let mut tree = WorkspaceTree::bootstrap_demo();
                tree.persist_path = None;
                let strip_id = tree.desktop.workspaces[0].strips[0].id;
                let group_id = tree.desktop.workspaces[0].strips[0].window_groups[0].id;
                let original_first =
                    tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id;

                let left = open_hosted_app(
                    &mut tree,
                    AppKind::Terminal,
                    "left",
                    Placement::TabAt(group_id, 0),
                );
                let right = open_hosted_app(
                    &mut tree,
                    AppKind::Terminal,
                    "right",
                    Placement::TabAt(group_id, usize::MAX),
                );
                let group = &tree.desktop.workspaces[0].strips[0].window_groups[0];
                assert_eq!(group.tabs.first().map(|tab| tab.id), Some(left));
                assert_eq!(group.tabs.get(1).map(|tab| tab.id), Some(original_first));
                assert_eq!(group.tabs.last().map(|tab| tab.id), Some(right));

                let first_group = open_hosted_app(
                    &mut tree,
                    AppKind::Terminal,
                    "before",
                    Placement::GroupAt(strip_id, 0),
                );
                let last_group = open_hosted_app(
                    &mut tree,
                    AppKind::Terminal,
                    "after",
                    Placement::GroupAt(strip_id, usize::MAX),
                );
                let strip = &tree.desktop.workspaces[0].strips[0];
                assert_eq!(strip.window_groups[0].tabs[0].id, first_group);
                assert_eq!(strip.window_groups.last().unwrap().tabs[0].id, last_group);
            }

            {
                let mut tree = WorkspaceTree::bootstrap_demo();
                tree.persist_path = None;
                tree.seed_doc_from_cache();
                let strip_id = tree.desktop.workspaces[0].strips[0].id;
                let group_id = tree.desktop.workspaces[0].strips[0].window_groups[0].id;
                let tab_id = tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id;
                let before = tree.desktop.workspaces[0].strips[0]
                    .window_groups
                    .iter()
                    .flat_map(|group| &group.tabs)
                    .count();

                assert!(relocate_tab(
                    &mut tree,
                    tab_id,
                    Placement::GroupAt(strip_id, usize::MAX)
                ));
                let strip = &tree.desktop.workspaces[0].strips[0];
                assert_eq!(
                    strip
                        .window_groups
                        .iter()
                        .flat_map(|group| &group.tabs)
                        .filter(|tab| tab.id == tab_id)
                        .count(),
                    1
                );
                assert_eq!(
                    strip
                        .window_groups
                        .iter()
                        .flat_map(|group| &group.tabs)
                        .count(),
                    before
                );
                assert_eq!(strip.window_groups.last().unwrap().tabs[0].id, tab_id);
                assert_ne!(strip.window_groups.last().unwrap().id, group_id);
            }
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn closing_or_destroying_a_surface_prunes_its_records_without_stale_state() {
        {
            crate::shell::launcher::clear_pending_placements();
            let mut tree = WorkspaceTree::bootstrap_demo();
            let _ = adopt_toplevel_at(
                &mut tree,
                42,
                Some("io.example.thing".into()),
                Placement::NewGroup,
            );
            let owner = group_of(&tree, 42).id;
            let strip_id = tree.desktop.workspaces[0].strips[0].id;
            let groups_before = tree.desktop.workspaces[0].strips[0].window_groups.len();
            let removed = close_tab_for_destroyed_surface(&mut tree, 42);
            assert!(removed, "close should report it removed a tab");
            let strip = &tree.desktop.workspaces[0].strips[0];
            assert_eq!(
                strip.window_groups.len(),
                groups_before - 1,
                "a closed one-window host/X11 group must not leave a dead column"
            );
            assert!(
                !strip.window_groups.iter().any(|group| group.id == owner),
                "the now-empty group must be removed from the strip"
            );
            assert_ne!(
                tree.desktop.active_group_for(strip_id),
                Some(owner),
                "focus must never keep pointing at the removed group"
            );
            let still_there = tree
                .desktop
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

            let _ = adopt_toplevel_at(
                &mut tree,
                43,
                Some("io.example.thing".into()),
                Placement::NewGroup,
            );
            let reopened = group_of(&tree, 43).id;
            assert_eq!(
                tree.desktop.workspaces[0].strips[0].window_groups.len(),
                groups_before,
                "reopening starts one live group, not a live group beside a retained corpse"
            );
            assert!(
                tree.reorder_group_for_tests(strip_id, reopened, 0),
                "the reopened group can move across the compact live ordering"
            );
            assert_eq!(
                tree.desktop.workspaces[0].strips[0].window_groups[0].id, reopened,
                "reorder must address the live host/X11 window directly"
            );
        }

        {
            crate::shell::launcher::clear_pending_placements();
            let state_dir = tempfile::tempdir().expect("temporary state directory");
            let state_path = state_dir.path().join("state.json");
            let mut tree = WorkspaceTree::bootstrap_demo();
            tree.persist_path = Some(state_path.clone());
            let _ = adopt_toplevel(&mut tree, 42, Some("io.example.thing".into()));

            // The persistence writer deliberately turns a live Foreign tile into
            // a symbolic LocalApp record.  A real surface destroy must remove that
            // record, not leave it available for the next session to relaunch.
            tree.queue_save();
            let saved = crate::shell::persist::load_tree_from(&state_path)
                .expect("live foreign state is a valid persistence snapshot");
            assert!(
                saved.desktop.workspaces.iter().any(|workspace| workspace
                    .strips
                    .iter()
                    .flat_map(|strip| strip.window_groups.iter())
                    .flat_map(|group| group.tabs.iter())
                    .any(|tab| matches!(
                        &tab.tile,
                        TileContent::LocalApp { app_id, .. } if app_id == "io.example.thing"
                    ))),
                "precondition: a live foreign surface is represented as a relaunchable record"
            );

            assert!(close_tab_for_destroyed_surface_and_persist(&mut tree, 42));

            let restarted = crate::shell::persist::load_tree_from(&state_path)
                .expect("destroyed foreign state is a valid persistence snapshot");
            assert!(
                restarted
                    .desktop
                    .workspaces
                    .iter()
                    .all(|workspace| workspace
                        .strips
                        .iter()
                        .flat_map(|strip| strip.window_groups.iter())
                        .flat_map(|group| group.tabs.iter())
                        .all(|tab| !matches!(
                            &tab.tile,
                            TileContent::Foreign { surface_id, .. } if *surface_id == 42
                        ) && !matches!(
                            &tab.tile,
                            TileContent::LocalApp { app_id, .. } if app_id == "io.example.thing"
                        ))),
                "a surface destroyed before restart must not resurrect as a LocalApp"
            );
        }

        {
            let state_dir = tempfile::tempdir().expect("state directory");
            let state_path = state_dir.path().join("workspace.json");
            let app_id = "io.example.Restored";
            let surface_id = 0xfeed_u64;

            // Write the same durable LocalApp record that a prior session leaves
            // behind. The native compositor then hands this restored tree handle
            // to both State and FrameBuilder.
            let mut persisted = WorkspaceTree::bootstrap_demo();
            persisted.persist_path = Some(state_path.clone());
            let target_tab = persisted.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id;
            persisted.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].tile =
                TileContent::LocalApp {
                    app_id: app_id.into(),
                    label: "Restored app".into(),
                    note: String::new(),
                };
            persisted.queue_save();
            drop(persisted);

            let mut restored =
                crate::shell::persist::load_tree_from(&state_path).expect("restore tree");
            // The native loader attaches the session path after deserialisation;
            // mirror that wiring here so destroy exercises the durable close path.
            restored.persist_path = Some(state_path.clone());
            let canonical = restored.shared();
            {
                let mut state_tree = canonical.write();
                assert_eq!(
                    adopt_toplevel_at(
                        &mut state_tree,
                        surface_id,
                        Some(app_id.into()),
                        Placement::IntoTab(target_tab),
                    ),
                    target_tab,
                    "surface adoption must target the restored tab"
                );
            }
            {
                let rendered_tree = canonical.read();
                let adopted = rendered_tree
                    .desktop
                    .workspaces
                    .iter()
                    .flat_map(|workspace| workspace.strips.iter())
                    .flat_map(|strip| strip.window_groups.iter())
                    .flat_map(|group| group.tabs.iter())
                    .find(|tab| tab.id == target_tab)
                    .expect("rendered tree sees the adopted tab");
                assert!(matches!(
                    &adopted.tile,
                    TileContent::Foreign {
                        surface_id: id,
                        ..
                    } if *id == surface_id
                ));
                assert_eq!(
                    rendered_tree
                        .desktop
                        .workspaces
                        .iter()
                        .flat_map(|workspace| workspace.strips.iter())
                        .flat_map(|strip| strip.window_groups.iter())
                        .flat_map(|group| group.tabs.iter())
                        .filter(|tab| matches!(&tab.tile, TileContent::Foreign { .. }))
                        .count(),
                    1,
                    "adoption through the canonical handle must not create a second card"
                );
                drop(rendered_tree);
            }
            {
                let mut state_tree = canonical.write();
                assert!(close_tab_for_destroyed_surface_and_persist(
                    &mut state_tree,
                    surface_id
                ));
            }

            let reloaded = crate::shell::persist::load_tree_from(&state_path).expect("reload tree");
            assert!(
                reloaded
                    .desktop
                    .workspaces
                    .iter()
                    .flat_map(|workspace| workspace.strips.iter())
                    .flat_map(|strip| strip.window_groups.iter())
                    .flat_map(|group| group.tabs.iter())
                    .all(|tab| !matches!(
                        &tab.tile,
                        TileContent::LocalApp { app_id: id, .. } if id == app_id
                    ) && !matches!(
                        &tab.tile,
                        TileContent::Foreign { surface_id: id, .. } if *id == surface_id
                    )),
                "destroying the adopted surface must remove its durable card before reload"
            );
        }

        {
            use crate::shell::personal_continuity::{
                AppInstanceId, AppInstanceState, ContinuityStamp, DeviceContinuationSnapshot,
                DeviceId, LaunchIntent, TabPlacement, WorkspaceContinuation,
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
                        LaunchIntent::app("so.tilo.browser").unwrap(),
                        None,
                        ContinuityStamp {
                            counter: 10,
                            device: source,
                        },
                    )],
                },
                &crate::shell::doc_open::DesktopContinuationLabels,
            );
            crate::shell::launcher::set_pending_placement(
                "so.tilo.browser",
                Placement::IntoTab(0x7171),
            );
            assert_eq!(
                adopt_toplevel(&mut tree, 8080, Some("so.tilo.browser".into())),
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
    }

    #[test]
    fn remove_tab_selects_the_immediate_right_tab_when_the_active_middle_tab_closes() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let group = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
        group.tabs.truncate(4);
        let middle = group.tabs[1].id;
        let right = group.tabs[2].id;
        group.active_tab = middle;

        assert!(remove_tab(&mut tree, middle));

        assert_eq!(
            tree.desktop.workspaces[0].strips[0].window_groups[0].active_tab, right,
            "the tab immediately to the right in visible order remains selected"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn failed_launch_cleanup_retires_only_the_unpainted_tab_it_owns() {
        {
            const PACKAGE: &str = "so.tilo.failed-host";
            const SURFACE: u64 = 0xface;
            crate::shell::launcher::clear_pending_placements();
            let state_dir = tempfile::tempdir().expect("temporary state directory");
            let state_path = state_dir.path().join("state.json");
            let mut tree = WorkspaceTree::bootstrap_demo();
            tree.persist_path = Some(state_path.clone());
            let (strip_id, prior_group) = focus_lone_window(&mut tree);

            crate::shell::launcher::set_pending_placement(PACKAGE, Placement::NewGroup);
            let failed_tab = adopt_toplevel(&mut tree, SURFACE, Some(PACKAGE.into()));
            assert_ne!(
                tree.desktop.active_group_for(strip_id),
                Some(prior_group),
                "fixture sanity: the new foreign pane took focus while opening"
            );
            tree.queue_save();

            assert_eq!(
                crate::shell::launcher::take_sole_claimed_surface_for_failed_launch(PACKAGE),
                Some(SURFACE),
                "the failure must identify the exact adopted surface"
            );
            assert!(
                retire_starting_foreign_surface(&mut tree, SURFACE),
                "an unpainted failed launch must not leave a blank foreign pane"
            );
            assert!(tree.desktop.workspaces.iter().all(|workspace| {
                workspace
                    .strips
                    .iter()
                    .flat_map(|strip| strip.window_groups.iter())
                    .flat_map(|group| group.tabs.iter())
                    .all(|tab| tab.id != failed_tab)
            }));
            assert_eq!(
                tree.desktop.active_group_for(strip_id),
                Some(prior_group),
                "removing the failed pane returns focus to the group that opened it"
            );
            let restarted = crate::shell::persist::load_tree_from(&state_path)
                .expect("the retired launch writes a valid persistence snapshot");
            assert!(
                restarted
                    .desktop
                    .workspaces
                    .iter()
                    .all(|workspace| workspace
                        .strips
                        .iter()
                        .flat_map(|strip| strip.window_groups.iter())
                        .flat_map(|group| group.tabs.iter())
                        .all(|tab| !matches!(
                            &tab.tile,
                            TileContent::LocalApp { app_id, .. } if app_id == PACKAGE
                        ))),
                "a restart must not relaunch the failed placeholder"
            );

            crate::shell::launcher::set_pending_placement(PACKAGE, Placement::NewGroup);
            let retry_tab = adopt_toplevel(&mut tree, 0xbeef, Some(PACKAGE.into()));
            assert_ne!(retry_tab, failed_tab, "a retry gets a fresh tab");
            assert!(tree.desktop.workspaces.iter().all(|workspace| {
                workspace
                    .strips
                    .iter()
                    .flat_map(|strip| strip.window_groups.iter())
                    .flat_map(|group| group.tabs.iter())
                    .all(|tab| tab.id != failed_tab)
            }));
            crate::shell::launcher::clear_pending_placements();
        }

        {
            const PACKAGE: &str = "so.tilo.painted-child";
            const SURFACE: u64 = 0xfade;
            crate::shell::launcher::clear_pending_placements();
            let mut tree = WorkspaceTree::bootstrap_demo();
            crate::shell::launcher::set_pending_placement(PACKAGE, Placement::NewGroup);
            let tab = adopt_toplevel(&mut tree, SURFACE, Some(PACKAGE.into()));
            crate::shell::foreign::store().publish_rgba(SURFACE, &[1, 2, 3, 4], 1, 1);

            assert_eq!(
                crate::shell::launcher::take_sole_claimed_surface_for_failed_launch(PACKAGE),
                Some(SURFACE),
            );
            assert!(
                !retire_starting_foreign_surface(&mut tree, SURFACE),
                "a painted child may outlive its launcher parent and must remain open"
            );
            assert!(tree.desktop.workspaces.iter().any(|workspace| {
                workspace
                    .strips
                    .iter()
                    .flat_map(|strip| strip.window_groups.iter())
                    .flat_map(|group| group.tabs.iter())
                    .any(|candidate| candidate.id == tab)
            }));
            crate::shell::foreign::store().forget(SURFACE);
        }

        {
            const PACKAGE: &str = "so.tilo.concurrent-host";
            const SURFACE: u64 = 0xc0de;
            crate::shell::launcher::clear_pending_placements();
            let mut tree = WorkspaceTree::bootstrap_demo();
            crate::shell::launcher::set_pending_placement(PACKAGE, Placement::NewGroup);
            let tab = adopt_toplevel(&mut tree, SURFACE, Some(PACKAGE.into()));
            // A second user action is still awaiting a toplevel when the first
            // worker reports failure. Its event has no launch token, so cleanup
            // must leave the claimed pane alone rather than guessing.
            crate::shell::launcher::set_pending_placement(PACKAGE, Placement::NewGroup);

            assert_eq!(
                crate::shell::launcher::take_sole_claimed_surface_for_failed_launch(PACKAGE),
                None,
                "concurrent launches intentionally make failure cleanup conservative"
            );
            assert!(tree.desktop.workspaces.iter().any(|workspace| {
                workspace
                    .strips
                    .iter()
                    .flat_map(|strip| strip.window_groups.iter())
                    .flat_map(|group| group.tabs.iter())
                    .any(|candidate| candidate.id == tab)
            }));
            crate::shell::launcher::clear_pending_placements();
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn remove_tab_repairs_selection_survives_rehydration_and_rejects_unknown_tabs() {
        // Phase `remove_tab_takes_a_placed_tab_out_and_repairs_selection`.
        // `remove_tab` takes the tab out of whichever group holds it and repairs the group's selection when it was the tab in front.
        {
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
                .desktop
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

        {
            crate::shell::launcher::clear_pending_placements();
            let mut tree = WorkspaceTree::bootstrap_demo();
            let group_id = tree.desktop.workspaces[0].strips[0].window_groups[0].id;
            let tab_ids = tree.desktop.workspaces[0].strips[0].window_groups[0]
                .tabs
                .iter()
                .map(|tab| tab.id)
                .collect::<Vec<_>>();

            for tab_id in tab_ids {
                assert!(remove_tab(&mut tree, tab_id));
            }
            assert!(
                tree.desktop.workspaces[0].strips[0]
                    .window_groups
                    .is_empty(),
                "the cache drops the group when its last tab closes"
            );

            tree.resync_cache_from_doc();
            assert!(
                tree.desktop.workspaces[0].strips[0]
                    .window_groups
                    .iter()
                    .all(|group| group.id != group_id),
                "rehydrating from the durable document must not resurrect the closed group"
            );
        }

        // Phase `remove_tab_reports_false_for_an_unknown_tab`.
        // A tab id nothing in the tree holds is reported as not removed, not silently accepted — a caller unwinding a launch has to know whether there was anything to unwind.
        {
            crate::shell::launcher::clear_pending_placements();
            let mut tree = WorkspaceTree::bootstrap_demo();
            assert!(!remove_tab(&mut tree, 0xdead_beef));
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn tab_labels_take_the_best_known_name_and_rename_only_the_label() {
        {
            {
                let mut tree = WorkspaceTree::bootstrap_demo();
                // First adopt without an app-id so the tab gets the `anon-<surface_id>` placeholder.
                let _ = adopt_toplevel(&mut tree, 99, None);
                let renamed = rename_tab_for_app_id(&mut tree, 99, "org.example.late");
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

            {
                let mut tree = WorkspaceTree::bootstrap_demo();
                let renamed = rename_tab_for_app_id(&mut tree, 12345, "x");
                assert!(
                    !renamed,
                    "rename must report `false` when the surface isn't adopted"
                );
            }

            {
                let mut tree = WorkspaceTree::bootstrap_demo();
                let _ = adopt_toplevel(&mut tree, 99, Some("org.example.Editor".into()));

                assert!(rename_tab_for_title(
                    &mut tree,
                    99,
                    "draft.md — Tilo Writer"
                ));
                let tab = tab_of(&tree, 99);
                assert_eq!(tab.title, "draft.md — Tilo Writer");
                match &tab.tile {
                    TileContent::Foreign { app_id, .. } => assert_eq!(app_id, "org.example.Editor"),
                    other => panic!("expected Foreign tile, got {other:?}"),
                }

                assert!(rename_tab_for_title(&mut tree, 99, ""));
                assert!(tab_of(&tree, 99).title.is_empty());
                assert!(!rename_tab_for_title(&mut tree, 12345, "gone"));

                // A title re-sent unchanged finds its tab but is no change:
                // the tree revision (a chrome rebuild) stays where it was.
                assert!(rename_tab_for_title(&mut tree, 99, "same"));
                let revision = tree.desktop.external_revision;
                assert!(rename_tab_for_title(&mut tree, 99, "same"));
                assert_eq!(tree.desktop.external_revision, revision);
                assert!(rename_tab_for_title(&mut tree, 99, "other"));
                assert_ne!(tree.desktop.external_revision, revision);
            }
        }

        {
            {
                let mut tree = WorkspaceTree::bootstrap_demo();
                let _ = adopt_toplevel(&mut tree, 99, None);

                assert!(rename_tab_for_app_id_with_title(
                    &mut tree,
                    99,
                    "com.openai.ChatGPT",
                    Some("ChatGPT"),
                ));
                let tab = tab_of(&tree, 99);
                assert!(matches!(
                    &tab.tile,
                    TileContent::Foreign { app_id, surface_id: 99 } if app_id == "com.openai.ChatGPT"
                ));
                assert_eq!(tab.title, "ChatGPT");
            }

            // Phase `a_hyphenated_app_id_becomes_a_name_not_a_slug`.
            // With nothing installed and nothing launched, the label is still built out of words rather than out of an identifier.
            {
                let tmp = tempfile::tempdir().unwrap();
                // Point the launcher at an empty registry so the lookup misses and the fallback is what is under test.
                let mut env = tilo_test_env::ScopedEnv::new();
                env.set("TILO_APPS_DATA_DIR", tmp.path());
                crate::shell::launcher::invalidate_app_cache();
                crate::shell::launcher::clear_launched_for_test();

                assert_eq!(
                    tab_label_for_foreign("so.tilo.word-processor"),
                    "Word Processor"
                );
                assert_eq!(
                    tab_label_for_foreign("so.tilo.file-explorer"),
                    "File Explorer"
                );
                assert_eq!(tab_label_for_foreign("org.example.MyApp"), "MyApp");

                env.remove("TILO_APPS_DATA_DIR");
                crate::shell::launcher::invalidate_app_cache();
            }

            // Phase `a_tab_takes_the_name_the_package_declares_even_without_a_launch`.
            // The package's manifest names the app, and that name is the app's name whoever started the window.
            {
                let tmp = tempfile::tempdir().unwrap();
                install_named(tmp.path(), "so.tilo.file-explorer", "Files");
                let mut env = tilo_test_env::ScopedEnv::new();
                env.set("TILO_APPS_DATA_DIR", tmp.path());
                crate::shell::launcher::invalidate_app_cache();
                crate::shell::launcher::clear_launched_for_test();

                assert_eq!(tab_label_for_foreign("so.tilo.file-explorer"), "Files");
                assert_eq!(tab_label_for_foreign("file-explorer"), "Files");

                let mut tree = WorkspaceTree::bootstrap_demo();
                let _ = adopt_toplevel(&mut tree, 4242, Some("so.tilo.file-explorer".to_string()));
                assert_eq!(
                    tab_of(&tree, 4242).title,
                    "Files",
                    "the tab the compositor actually builds carries the real name"
                );

                env.remove("TILO_APPS_DATA_DIR");
                crate::shell::launcher::invalidate_app_cache();
            }

            // Phase `adopted_tab_takes_the_launched_app_name`.
            // A toplevel from a package this session launched is labelled with the installed app's name, not the raw wayland app-id; launch history is process-global, so the fixture uses an id no other test registers.
            {
                crate::shell::launcher::remember_launch(&crate::shell::launcher::InstalledApp {
                    package_id: "so.tilo.adoptfixture".into(),
                    display_name: "Adopt Fixture".into(),
                    localized_names: BTreeMap::default(),
                    keywords: Vec::new(),
                    category: None,
                    version: "0.1.0".into(),
                    manifest_path: std::path::PathBuf::new(),
                    payload_root: std::path::PathBuf::new(),
                    has_gui: true,
                    opens_extensions: Vec::new(),
                    opens_mime_types: Vec::new(),
                    opens_uri_schemes: Vec::new(),
                    opens_secondary: false,
                    offers: Vec::new(),
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
                let _ = adopt_toplevel(&mut tree, 31338, None);
                assert!(rename_tab_for_app_id(
                    &mut tree,
                    31338,
                    "so.tilo.adoptfixture"
                ));
                assert_eq!(tab_of(&tree, 31338).title, "Adopt Fixture");
            }

            {
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
    }

    // A tab carries the app's name, never a raw identifier like `app` or `word-processor`.

    /// Minimal installed package: a manifest declaring a name, and a registry entry pointing at it. Same shape `tilo-install` writes.
    fn install_named(dir: &std::path::Path, package_id: &str, name: &str) {
        use std::fmt::Write as _;
        let payload = dir.join("payloads").join(package_id);
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        let manifest_path = payload.join("manifest.toml");
        std::fs::write(
            &manifest_path,
            crate::shell::test_support::canonical_manifest(
                &format!(
                    "id = \"{package_id}\"\nname = \"{name}\"\nversion = \"1.0.0\"\nkind = \"app\""
                ),
                "[entrypoint]\nbin = \"bin/app\"\ntype = \"gui\"\n",
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
    fn adopted_toplevels_land_in_their_room_and_persist_only_real_identity() {
        // Phase `adopt_toplevel_creates_strip_when_workspace_empty`.
        // Adoption into a workspace with no strips must synthesize a default strip + group rather than orphan the surface.
        {
            let mut tree = WorkspaceTree::bootstrap_demo();
            // Start from a known-good tree and erase the active workspace's strips so we exercise the new branch. Other workspaces (if any) are untouched.
            let active_id = tree.desktop.active;
            {
                let ws = tree
                    .desktop
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
                .desktop
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

        {
            crate::shell::launcher::clear_pending_placements();
            let mut tree = WorkspaceTree::bootstrap_demo();
            assert!(
                tree.desktop.workspaces.len() >= 2,
                "fixture needs distinct workspaces"
            );
            let active = tree.desktop.active;
            let target_room = tree.desktop.workspaces[1].room();
            let target_id = tree.desktop.workspaces[1].id;

            let tab =
                adopt_toplevel_in_room(&mut tree, 8_888, Some("so.tilo.notes".into()), target_room)
                    .expect("known room is accepted");

            assert_eq!(
                tree.desktop.active, active,
                "a background launch cannot switch workspaces"
            );
            let target = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.id == target_id)
                .unwrap();
            assert!(
                target
                    .strips
                    .iter()
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .any(|candidate| candidate.id == tab)
            );
            assert!(
                tree.desktop
                    .workspaces
                    .iter()
                    .filter(|workspace| workspace.id != target_id)
                    .flat_map(|workspace| &workspace.strips)
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .all(|candidate| !matches!(
                        candidate.tile,
                        TileContent::Foreign {
                            surface_id: 8_888,
                            ..
                        }
                    ))
            );
        }

        {
            crate::shell::launcher::clear_pending_placements();
            for attributed in [false, true] {
                let state_dir = tempfile::tempdir().expect("temporary state directory");
                let state_path = state_dir.path().join("state.json");
                let mut tree = WorkspaceTree::bootstrap_demo();
                for workspace in &mut tree.desktop.workspaces {
                    for strip in &mut workspace.strips {
                        strip.window_groups.clear();
                    }
                }
                tree.seed_doc_from_cache();
                tree.persist_path = Some(state_path.clone());
                tree.queue_save();
                let active = tree.desktop.active;
                let target_index = usize::from(attributed);
                let target_id = tree.desktop.workspaces[target_index].id;
                let target_room = tree.desktop.workspaces[target_index].room();
                let target_strip = tree.desktop.workspaces[target_index].strips[0].id;
                let tab = if attributed {
                    adopt_toplevel_in_room(
                        &mut tree,
                        8_889,
                        Some("dev.tilo.calculator".into()),
                        target_room,
                    )
                    .expect("known room accepts the native app")
                } else {
                    adopt_toplevel(&mut tree, 8_889, Some("dev.tilo.calculator".into()))
                };
                assert_eq!(
                    tree.desktop.active, active,
                    "adoption must preserve desktop focus"
                );
                let live_group = group_of(&tree, 8_889).id;

                // Do not flush here: the real adoption must make the new tab
                // restartable without a later workspace edit or permission grant.
                let saved = crate::shell::persist::load_tree_from(&state_path)
                    .expect("adoption leaves a readable durable workspace");
                assert_eq!(
                    saved.desktop.active, active,
                    "persist only after restoring the previously active workspace"
                );
                let persisted = saved
                    .desktop
                    .workspaces
                    .iter()
                    .flat_map(|workspace| {
                        workspace.strips.iter().map(move |strip| (workspace, strip))
                    })
                    .flat_map(|(workspace, strip)| {
                        strip
                            .window_groups
                            .iter()
                            .map(move |group| (workspace, strip, group))
                    })
                    .find(|(_, _, group)| group.tabs.iter().any(|candidate| candidate.id == tab))
                    .expect("native adoption itself must persist its relaunchable tab");
                assert_eq!(persisted.0.id, target_id);
                assert_eq!(persisted.1.id, target_strip);
                assert_eq!(persisted.2.id, live_group);
                assert_eq!(persisted.2.active_tab, tab);
                let persisted_tab = persisted
                    .2
                    .tabs
                    .iter()
                    .find(|candidate| candidate.id == tab)
                    .unwrap();
                assert!(matches!(
                    &persisted_tab.tile,
                    TileContent::LocalApp { app_id, note, .. }
                        if app_id == "dev.tilo.calculator" && note.is_empty()
                ));
            }
            crate::shell::launcher::clear_pending_placements();
        }

        {
            crate::shell::launcher::clear_pending_placements();
            for (attributed, app_id) in [
                (false, None),
                (true, None),
                (false, Some("anon-8890".to_string())),
                (true, Some("anon-8890".to_string())),
            ] {
                let state_dir = tempfile::tempdir().expect("temporary state directory");
                let state_path = state_dir.path().join("state.json");
                let mut tree = WorkspaceTree::bootstrap_default();
                tree.persist_path = Some(state_path.clone());
                tree.queue_save();
                crate::shell::persist::wait_for_saves();
                let before = std::fs::read(&state_path).expect("saved empty desktop");
                if attributed {
                    let room = tree.desktop.workspaces[0].room();
                    assert!(adopt_toplevel_in_room(&mut tree, 8_890, app_id, room).is_some());
                } else {
                    adopt_toplevel(&mut tree, 8_890, app_id);
                }
                assert_eq!(app_id_for_surface(&tree, 8_890), Some("anon-8890"));
                crate::shell::persist::wait_for_saves();
                assert_eq!(
                    std::fs::read(&state_path).expect("desktop remains readable"),
                    before,
                    "an anonymous provisional surface is not a relaunchable application"
                );
            }
        }

        {
            let mut tree = WorkspaceTree::bootstrap_demo();
            let active = tree.desktop.active;
            assert_eq!(
                adopt_toplevel_in_room(
                    &mut tree,
                    9_999,
                    Some("so.tilo.notes".into()),
                    crate::shell::state::RoomId::new()
                ),
                None
            );
            assert_eq!(tree.desktop.active, active);
            assert!(group_id_for_surface(&tree, 9_999).is_none());
        }
    }
}

#[cfg(test)]
mod location_tests {
    use super::location_from_title;

    #[test]
    fn the_apps_own_name_is_removed_from_its_title() {
        let name = "LibreOffice Writer";
        assert_eq!(
            location_from_title("Q3 report.odt - LibreOffice Writer", name),
            "Q3 report.odt"
        );
        assert_eq!(
            location_from_title("LibreOffice Writer \u{2014} Q3 report.odt", name),
            "Q3 report.odt"
        );
        assert_eq!(
            location_from_title("LibreOffice Writer", name),
            "LibreOffice Writer"
        );
        assert_eq!(
            location_from_title("~/work: vim", "Alacritty"),
            "~/work: vim"
        );
        assert_eq!(
            location_from_title("Writer - notes", name),
            "Writer - notes",
            "only the exact name is removed"
        );
    }
}

#[cfg(test)]
#[path = "tab_binding_name_tests.rs"]
mod host_tab_name_tests;
