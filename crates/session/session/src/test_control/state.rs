//! What a connection may see of the desk's state: the dump the tools that drive a desktop
//! read, one row per open window, and the narrowing of both to the workspace an app acts in and
//! to nothing of the desk while the screen is locked.

use std::collections::BTreeSet;

use serde_json::{Value as Json, json};
use tilo_desktop_model::DesktopTree;

use super::{Context, Desk, Gate, Scope, Slot};

/// What a socket at `gate` may see of the state `dump`: a development socket sees the desk as
/// it is, and the owner's door sees it too — except while the screen is locked, where it learns
/// that it is and nothing of what the lock covers: no windows, no workspaces, no titles or the
/// text in them.
pub(super) fn owner_shown(gate: Gate, dump: Json) -> Json {
    if gate == Gate::Dev || !locked_in(&dump) {
        return dump;
    }
    let mut dump = dump;
    let Some(state) = dump.get_mut("state").and_then(Json::as_object_mut) else {
        return dump;
    };
    state.insert("workspace_tree".into(), Json::Null);
    state.insert("held_workspaces".into(), json!([]));
    if let Some(open) = state
        .get_mut("chrome")
        .and_then(|chrome| chrome.get_mut("overlays"))
        .and_then(Json::as_object_mut)
    {
        for shown in open.values_mut() {
            *shown = Json::Bool(false);
        }
    }
    dump
}

/// The desk's state as `ctx` may read it: an app's own workspace only, and
/// nothing of the desk while the screen is locked.
pub(super) fn shown(ctx: &Context, dump: Json) -> Json {
    match &ctx.scope {
        None => owner_shown(ctx.gate, dump),
        Some(scope) => owner_shown(ctx.gate, only_workspace(dump, scope)),
    }
}

/// `dump` with every workspace but the one `scope` holds left out.
fn only_workspace(mut dump: Json, scope: &Scope) -> Json {
    let Some(state) = dump.get_mut("state").and_then(Json::as_object_mut) else {
        return dump;
    };
    if let Some(tree) = state.get_mut("workspace_tree")
        && let Some(workspaces) = tree.get_mut("workspaces").and_then(Json::as_array_mut)
    {
        workspaces.retain(|workspace| in_scope(workspace, scope));
    }
    if let Some(held) = state
        .get_mut("held_workspaces")
        .and_then(Json::as_array_mut)
    {
        held.retain(|workspace| in_scope(workspace, scope));
    }
    dump
}

/// Whether a row naming its workspace's room is the one `scope` holds.
fn in_scope(row: &Json, scope: &Scope) -> bool {
    row.get("room").and_then(Json::as_str) == Some(scope.workspace.as_str())
}

/// `tree` with every workspace but the one `scope` holds left out.
pub(super) fn within(tree: &DesktopTree, scope: &Scope) -> DesktopTree {
    let mut tree = tree.clone();
    tree.workspaces
        .retain(|workspace| workspace.id().to_string() == scope.workspace);
    tree
}

/// Whether a state dump says the screen is locked.
fn locked_in(dump: &Json) -> bool {
    dump.get("state")
        .and_then(|state| state.get("chrome"))
        .and_then(|chrome| chrome.get("session_locked"))
        .and_then(Json::as_bool)
        == Some(true)
}

/// The desk as it is now: its tree without its history, the workspaces this computer holds, what
/// stands over it, and whether the screen is locked — the shape the tools that drive a desktop
/// read, and the line a `watch` streams as it changes.
pub(super) fn state_dump(tree: &DesktopTree, desk: &Desk<'_>) -> Json {
    let held: BTreeSet<String> = desk
        .engine
        .map(|engine| {
            engine
                .workspaces()
                .iter()
                .map(std::string::ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    let held_workspaces: Vec<Json> = tree
        .workspaces
        .iter()
        .filter(|workspace| held.contains(&workspace.id().to_string()))
        .map(|workspace| {
            json!({
                "workspace": workspace.slot.0,
                "room": workspace.id().to_string(),
            })
        })
        .collect();
    let mut open = serde_json::Map::new();
    for slot in Slot::ALL {
        let up = desk.overlays.is_open(slot) || (slot == Slot::Overview && desk.overview.get());
        open.insert(slot.as_str().to_owned(), up.into());
    }
    let tree = tilo_desktop_model::history::without_history(|| serde_json::to_value(tree))
        .unwrap_or(Json::Null);
    let agents: Vec<Json> = desk
        .seats
        .readings()
        .iter()
        .map(|agent| {
            json!({
                "key": agent.key,
                "name": agent.name,
                "doing": agent.doing,
                "seat": agent.seat,
            })
        })
        .collect();
    json!({"state": {
        "workspace_tree": tree,
        "held_workspaces": held_workspaces,
        "agents": agents,
        "chrome": {
            "session_locked": desk.locked,
            // The lock screen's own frame is what the outputs show, not the
            // desk's last one: the lock is up and covering the desk.
            "lock_screen_drawn": desk.lock_screen_drawn,
            "in_transition": false,
            "overlays": open,
            // Where the person types, which a surface a key opens stands beside, and where the
            // stack placed each surface that is up: `[x0, y0, x1, y1]`.
            "caret": desk.overlays.caret().map(|at| [at.x0, at.y0, at.x1, at.y1]),
            "places": Slot::ALL
                .into_iter()
                .filter_map(|slot| {
                    let at = desk.overlays.place(slot)?.room;
                    Some((slot.as_str().to_owned(), json!([at.x0, at.y0, at.x1, at.y1])))
                })
                .collect::<serde_json::Map<_, _>>(),
            // Where each surface that is up stands as its app drew it: a panel that fits its
            // content is shorter than the room the stack gave it.
            "surfaces": Slot::ALL
                .into_iter()
                .filter_map(|slot| {
                    let at = desk.overlays.surface(slot)?;
                    Some((slot.as_str().to_owned(), json!([at.x0, at.y0, at.x1, at.y1])))
                })
                .collect::<serde_json::Map<_, _>>(),
            // Whether freed heap is still to be handed back to the kernel
            // (a close asked for a trim), and how many trims have run.
            "heap_trim": {
                "pending": tilo_compositor::heap_trim::close_trim_pending(),
                "trims": tilo_compositor::heap_trim::trims_completed(),
            },
        },
        "peers": Json::Null,
        "sharing": Json::Null,
    }})
}

/// One row per tab of `tree` — where it is, which app shows it, whether it is the tab in
/// front of the active workspace's active strip and group, and, for a tab a window fills,
/// that window and where the frame draws it. A workspace or strip without a name is named by
/// its id; `kind` is the tile's variant and `app_id` the app it names, if any. `rect` is the
/// window's rectangle in the layout, `[x, y, width, height]`, or null while no window of the
/// tab is mapped: it is what a tool aims at to press something the row names. `frames` is how
/// many frames the window's client committed: a window showing the same thing keeps its count,
/// so two reads that agree say it drew nothing between them.
pub(super) fn window_rows(
    tree: &DesktopTree,
    rect_of: impl Fn(u64) -> Option<(i32, i32, i32, i32)>,
    frames_of: impl Fn(u64) -> u64,
) -> Vec<Json> {
    fn named(name: &str, id: u64) -> Json {
        if name.is_empty() {
            id.into()
        } else {
            name.into()
        }
    }
    let mut rows = Vec::new();
    for workspace in &tree.workspaces {
        let workspace_active = workspace.slot == tree.active;
        let active_strip = tree.active_strip.get(&workspace.slot);
        for strip in &workspace.strips {
            let strip_active = workspace_active && active_strip == Some(&strip.id);
            let active_group = tree.active_group.get(&strip.id);
            for group in &strip.window_groups {
                let group_active = strip_active && active_group == Some(&group.id);
                for tab in &group.tabs {
                    let (kind, app_id) = tile_kind(&tab.tile);
                    let window = window_of_tile(&tab.tile);
                    let rect = window
                        .and_then(&rect_of)
                        .map_or(Json::Null, |(x, y, width, height)| {
                            json!([x, y, width, height])
                        });
                    rows.push(json!({
                        "workspace": named(&workspace.name, workspace.slot.0),
                        "strip": named(&strip.name, strip.id.0),
                        "group": group.id,
                        "tab": tab.id,
                        "window": window,
                        "kind": kind,
                        "app_id": app_id,
                        "title": tab.title,
                        "active": group_active && group.active_tab == tab.id,
                        "floating": workspace.arrangement.float_of(group.id).is_some(),
                        "rect": rect,
                        "frames": window.map_or(0, &frames_of),
                    }));
                }
            }
        }
    }
    rows
}

/// The window a tile holds, when a window fills it: a surface this session mapped, or a
/// window another device hosts.
pub(super) fn window_of_tile(tile: &tilo_desktop_model::TileContent) -> Option<u64> {
    match tile {
        tilo_desktop_model::TileContent::Foreign { surface_id, .. } => Some(*surface_id),
        _ => None,
    }
}

/// A tile's variant name and the `app_id` it carries, as its JSON spells them.
pub(super) fn tile_kind(tile: &tilo_desktop_model::TileContent) -> (String, Json) {
    match serde_json::to_value(tile).unwrap_or_default() {
        Json::String(kind) => (kind, Json::Null),
        Json::Object(variant) => variant.into_iter().next().map_or_else(
            || (String::new(), Json::Null),
            |(kind, body)| (kind, body.get("app_id").cloned().unwrap_or_default()),
        ),
        _ => (String::new(), Json::Null),
    }
}
