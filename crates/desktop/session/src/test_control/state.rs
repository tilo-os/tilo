//! What a connection may see of the session's state: the dump the tools that drive a sessiontop
//! read, one row per open window, and the narrowing of both to the workspace an app acts in and
//! to nothing of the session while the screen is locked.

use std::collections::BTreeSet;
use std::hash::Hash as _;

use serde_json::{Value as Json, json};
use tilo_desktop_model::{Layout, TileContent};

use super::{Audience, Context, Part, Scope, Session};

/// What a socket at `who` may see of the state `dump`: a development socket sees the session as
/// it is, and the owner's API sees it too — except while the screen is locked, where it learns
/// that it is and nothing of what the lock covers: no windows, no workspaces, no titles or the
/// text in them.
pub(super) fn owner_shown(who: Audience, dump: Json) -> Json {
    if who == Audience::Dev || !locked_in(&dump) {
        return dump;
    }
    let mut dump = dump;
    let Some(state) = dump.get_mut("state").and_then(Json::as_object_mut) else {
        return dump;
    };
    state.insert("workspace_tree".into(), Json::Null);
    state.insert("held_workspaces".into(), json!([]));
    // Who is here is session state like the rest: a locked session names nobody.
    state.insert("peers".into(), Json::Null);
    state.insert("sharing".into(), Json::Null);
    // What people and apps told the person is theirs: a locked session says none of it.
    state.insert("notifications".into(), Json::Null);
    // Nor what they copied.
    state.insert("clipboard".into(), Json::Null);
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

/// The session's state as `ctx` may read it: an app's own workspace only, and
/// nothing of the session while the screen is locked.
pub(super) fn shown(ctx: &Context, dump: Json) -> Json {
    match &ctx.scope {
        None => owner_shown(ctx.who, dump),
        Some(scope) => owner_shown(ctx.who, only_workspace(dump, scope)),
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
    // Who is here is said per workspace: an app is shown the one it acts in.
    if let Some(peers) = state.get_mut("peers").and_then(Json::as_object_mut)
        && let Some(rows) = peers.get_mut("workspaces").and_then(Json::as_array_mut)
    {
        rows.retain(|workspace| in_scope(workspace, scope));
    }
    // The window shares are the session's, of every workspace it holds: an app reads none of them.
    state.insert("sharing".into(), Json::Null);
    // What the person was told is the person's: an app reads none of it.
    state.insert("notifications".into(), Json::Null);
    // What they copied is read through the clipboard's own service, which grants it: never here.
    state.insert("clipboard".into(), Json::Null);
    dump
}

/// Whether a row naming its workspace is the one `scope` holds.
fn in_scope(row: &Json, scope: &Scope) -> bool {
    row.get("workspace").and_then(Json::as_str) == Some(scope.workspace.as_str())
}

/// `tree` with every workspace but the one `scope` holds left out.
pub(super) fn within(tree: &Layout, scope: &Scope) -> Layout {
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

/// What the session shares of its windows and shows of other devices', as its window-share
/// resource reads it: `sharing` (each shared window and who views it), `starting` (a share that
/// waits, and for what), `watching` (the windows of other devices this one asks for, and of which
/// device), `received` (the streams that reached it, and whether their tab pictures them) and
/// `live` (each workspace's live session, which the pictures travel: open, or what it waits for and
/// which device it waits to reach). A window shared or watched under a live session that is not open
/// says what it waits for on its own row. A tool driving two sessions reads on each end how far a
/// share came. The identifier a window is
/// captured under is left out: it is the compositor's, for the clients it trusts.
fn shares_shown(session: &Session<'_>) -> Json {
    let Ok(shares) = session.shares.try_borrow() else {
        return Json::Null;
    };
    let mut reading = shares.reading();
    for (_, rows) in reading.as_object_mut().into_iter().flatten() {
        for row in rows.as_array_mut().into_iter().flatten() {
            if let Some(row) = row.as_object_mut() {
                row.remove("window");
            }
        }
    }
    reading
}

/// The session as it is now: its tree without its history, the workspaces this computer holds, what
/// stands over it, whether the screen is locked, what the session shares of its windows and what it
/// was told — the shape the tools that drive a desktop read, and the line a `watch` streams as it
/// changes.
pub(super) fn state_dump(tree: &Layout, session: &Session<'_>) -> Json {
    let held: BTreeSet<String> = session
        .view
        .map(|view| {
            view.workspaces()
                .iter()
                .map(std::string::ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    let held_workspaces: Vec<Json> = tree
        .workspaces
        .iter()
        .filter(|workspace| held.contains(&workspace.id().to_string()))
        .map(|workspace| json!({"workspace": workspace.id()}))
        .collect();
    let mut open = serde_json::Map::new();
    for part in Part::ALL {
        let up =
            session.overlays.is_open(part) || (part == Part::Overview && session.overview.get());
        open.insert(part.as_str().to_owned(), up.into());
    }
    let tree = tilo_desktop_model::history::without_history(|| serde_json::to_value(tree))
        .unwrap_or(Json::Null);
    let agents: Vec<Json> = session
        .agent_sessions
        .readings()
        .iter()
        .map(|agent| {
            json!({
                "key": agent.key,
                "name": agent.name,
                "doing": agent.doing,
                "environment": agent.environment,
                "environment_id": agent.environment_id,
            })
        })
        .collect();
    json!({"state": {
        "workspace_tree": tree,
        "held_workspaces": held_workspaces,
        "apps_kept_in_workspace": session.apps_kept,
        "agents": agents,
        "chrome": {
            "session_locked": session.locked.yes(),
            // The lock screen's own frame is what the outputs show, not the
            // session's last one: the lock is up and covering the session.
            "lock_screen_drawn": session.locked.drawn(),
            "in_transition": false,
            "overlays": open,
            // Where the person types, which a surface a key opens stands beside, and where the
            // stack placed each surface that is up: `[x0, y0, x1, y1]`.
            "caret": session.overlays.caret().map(|at| [at.x0, at.y0, at.x1, at.y1]),
            "places": Part::ALL
                .into_iter()
                .filter_map(|part| {
                    let at = session.overlays.place(part)?.room;
                    Some((part.as_str().to_owned(), json!([at.x0, at.y0, at.x1, at.y1])))
                })
                .collect::<serde_json::Map<_, _>>(),
            // Where each surface that is up stands as its app drew it: a panel that fits its
            // content is shorter than the room the stack gave it.
            "surfaces": Part::ALL
                .into_iter()
                .filter_map(|part| {
                    let at = session.overlays.surface(part)?;
                    Some((part.as_str().to_owned(), json!([at.x0, at.y0, at.x1, at.y1])))
                })
                .collect::<serde_json::Map<_, _>>(),
            // Whether freed heap is still to be handed back to the kernel
            // (a close asked for a trim), and how many trims have run.
            "heap_trim": {
                "pending": tilo_compositor::heap_trim::close_trim_pending(),
                "trims": tilo_compositor::heap_trim::trims_completed(),
            },
        },
        "peers": presence(session),
        "sharing": shares_shown(session),
        // What the person was told: the banners up and what the centre keeps, so a tool driving a
        // session reads a notice as the person does, and chooses it by the id it is shown under.
        "notifications": notices_shown(session),
        // The person's keyboard and pointer across their devices: where the pointer is here, the
        // device it drives, and what the devices driving this one sent.
        "shared_input": session.shared_input.map_or_else(
            || json!({"pointer": [session.pointer.0, session.pointer.1], "pointer_shown": session.pointer_shown}),
            |shared_input| shared_input.inspection(session.pointer, session.pointer_shown),
        ),
        // The text on the clipboard, as the history last read it.
        "clipboard": session.clipboard.text(),
        // Whether that is a secret the person copied (a password from the vault), whose text is
        // read by no one but the app it is pasted into.
        "clipboard_secret": session.clipboard.holds_secret(),
    }})
}

/// Who hosts each workspace this computer holds, and whether its host took this computer in yet: a
/// machine copied from another holds its workspaces before their hosts admit it, and `tilo-vm vm
/// branch` waits for that. One row per held workspace: its id, its name on the shell, the
/// host's name as its person gave it, whether this computer is the host, and whether it still
/// waits for the host. A socket sees what [`shown`] lets it see of the session: an app its own
/// workspace, and nothing of a locked session but through a development socket.
pub(super) fn keeping_devices(tree: &Layout, session: &Session<'_>, ctx: &Context) -> Json {
    let rows: Vec<Json> = if ctx.who != Audience::Dev && session.locked.yes() {
        Vec::new()
    } else {
        tree.workspaces
            .iter()
            .filter(|workspace| {
                ctx.scope
                    .as_ref()
                    .is_none_or(|scope| workspace.id().to_string() == scope.workspace)
            })
            .filter_map(|workspace| {
                let hosting = session.view?.hosting(workspace.id())?;
                Some(json!({
                    "workspace": workspace.id().to_string(),
                    "name": workspace.name.clone(),
                    "host": hosting.host,
                    "hosted_here": hosting.here,
                    "awaiting_host": !hosting.admitted,
                }))
            })
            .collect()
    };
    json!({"answer": {"workspaces": rows}})
}

/// What the person was told, as the banners and the centre show it: oldest first, one row per
/// notification, each with its words, app, actions, severity, and whether it is still a banner or
/// has been seen. The id is the one [the notifications' own `act` and `open-notice`
/// verbs](crate::notifications::RESOURCE) take, so a tool chooses a notice as the person does.
fn notices_shown(session: &Session<'_>) -> Json {
    let desktop = &session.notifications;
    let log = desktop.log();
    Json::Array(
        log.history()
            .iter()
            .map(|notification| (notification, false))
            .chain(log.active().iter().map(|notification| (notification, true)))
            .map(|(notification, banner)| {
                json!({
                    "id": notification.id,
                    "app": notification.app_name,
                    "app_id": notification.app_id,
                    "title": notification.title,
                    "body": notification.body,
                    "severity": crate::notifications::severity_name(notification.severity),
                    "banner": banner,
                    "actions": notification
                        .actions
                        .iter()
                        .map(|(key, label)| json!({"key": key, "label": label}))
                        .collect::<Vec<Json>>(),
                    "unread": desktop.is_unread(notification.id),
                })
            })
            .collect(),
    )
}

/// Who is here, as every surface here draws them: this device's own row, and each workspace this
/// session takes part in with everyone present in it, as the peer network says. `Null` when
/// this session takes part in no live session.
fn presence(session: &Session<'_>) -> Json {
    let Some(sync) = session.sync else {
        return Json::Null;
    };
    json!({
        "own": row(&sync.own_here()),
        "workspaces": sync
            .live_workspaces()
            .into_iter()
            .map(|workspace| json!({
                "workspace": workspace,
                "members": sync.members(&workspace).iter().map(row).collect::<Vec<Json>>(),
            }))
            .collect::<Vec<Json>>(),
    })
}

/// One member as a surface here draws them: the row every surface's roster and pointer are drawn
/// from, named by the one rule every surface names a member by — a person nobody named is
/// someone without a name yet, never nothing and never their key.
fn row(member: &collab_types::presence::Shown) -> Json {
    let mut row = serde_json::to_value(member).unwrap_or(Json::Null);
    if let Some(row) = row.as_object_mut() {
        row.insert(
            "label".into(),
            Json::String(tilo_ui_base::presence::person_label(&member.label)),
        );
    }
    row
}

/// The row of `member` as a watcher is told about it: without where their pointer is, which moves
/// every frame of its own and moves nothing of the session.
fn settled(member: &collab_types::presence::Shown) -> String {
    let mut row = row(member);
    if let Some(row) = row.as_object_mut() {
        row.remove("pointer");
    }
    row.to_string()
}

/// What a watcher of the state dump is told changed about who is here: the rows [`presence`]
/// publishes, as one hash. A watcher that asks who is here while nothing moved is answered from
/// the dump it already has.
pub(super) fn presence_stamp(session: &Session<'_>, hash: &mut impl std::hash::Hasher) {
    let Some(sync) = session.sync else {
        return;
    };
    settled(&sync.own_here()).hash(hash);
    for workspace in sync.live_workspaces() {
        workspace.hash(hash);
        for member in sync.members(&workspace) {
            settled(&member).hash(hash);
        }
    }
}

/// One row per tab of `tree` — where it is, which app shows it, whether it is the tab in
/// front of the active workspace's active strip and group, and, for a tab a window fills,
/// that window and where the frame draws it. A workspace or strip without a name is named by
/// its id; `kind` is the tile's variant and `app_id` the app it names, if any. `rect` is the
/// frame the shell drew for the window, `[x, y, width, height]`, or the client's where the shell
/// has laid the window out nowhere yet, and null while no window of the tab is mapped: it is
/// what a tool aims at to press something the row names. `frames` is how
/// many frames the window's client committed: a window showing the same thing keeps its count,
/// so two reads that agree say it drew nothing between them. `resource` is what the window
/// shows, as its bar carries it when the person picks it up: the record its place is on, which
/// a window shows whether its app maps a surface of its own or the shell draws it (a tilolang
/// app's window maps none). `item` is the item the window was opened on (a document of its app's library),
/// which a restart opens it on again: null for a window opened on none.
pub(super) fn window_rows(
    tree: &Layout,
    (rect_of, frames_of, drawn_of, resource_of, item_of): (
        impl Fn(u64) -> Option<(i32, i32, i32, i32)>,
        impl Fn(u64) -> u64,
        impl Fn(u64) -> Option<(i32, i32, i32, i32)>,
        impl Fn(u64) -> Option<String>,
        impl Fn(u64) -> Option<String>,
    ),
    app: Option<&str>,
) -> Vec<Json> {
    fn named(name: &str, id: Json) -> Json {
        if name.is_empty() { id } else { name.into() }
    }
    let mut rows = Vec::new();
    for workspace in &tree.workspaces {
        let workspace_active = workspace.id() == tree.active;
        // The workspace by the one name every surface names it by: the one the shell holds for
        // it, or the shell's own word for a workspace nobody has named yet. Never the id it is
        // numbered by.
        let workspace_name = tilo_session_core::desktop::names::workspace(&workspace.name);
        let active_strip = tree.active_strip_for(workspace);
        for strip in &workspace.strips {
            let strip_active = workspace_active && active_strip == Some(strip.id);
            // The window the strip focuses, read through the strip's own rule: a pointer left on a
            // group carried to another strip names no window here.
            let active_group = tree
                .active_group_for(strip.id)
                .or_else(|| strip.window_groups.first().map(|group| group.id));
            for group in &strip.window_groups {
                let group_active = strip_active && active_group == Some(group.id);
                for tab in &group.tabs {
                    // A tab that shows no window is not one a tool aims at: a card where an app
                    // stopped working, or a place a saved window stood in, belongs to the tab.
                    if matches!(
                        tab.tile,
                        TileContent::AppPlaceholder { .. } | TileContent::Stub { .. }
                    ) {
                        continue;
                    }
                    // One application's windows alone, where the caller named one: whether the
                    // tile is of that app is read off the tile itself, before the row is named
                    // and the place it shows looked up.
                    if let Some(app) = app
                        && !tilo_desktop_model::apps::canonical_app_id(&tab.tile)
                            .is_some_and(|id| tilo_desktop_model::apps::app_ids_match(&id, app))
                    {
                        continue;
                    }
                    let (kind, app_id) = tile_kind(&tab.tile);
                    let window = window_of_tile(&tab.tile);
                    // What the window shows, as its bar carries it: the record its place stands
                    // on. A window this session maps no surface for — a tilolang app's, which
                    // the shell draws — shows its record all the same, so only where the window
                    // is drawn is read of the surface (below).
                    let resource = resource_of(tab.id);
                    // Where the person sees the window: the frame the shell drew for it, or the
                    // client's own rect where the shell has laid out no frame for it yet. A window
                    // the compositor maps none of stands nowhere, whatever frame it last drew.
                    let rect = window
                        .and_then(&rect_of)
                        .map(|client| drawn_of(tab.id).unwrap_or(client))
                        .map_or(Json::Null, |(x, y, width, height)| {
                            json!([x, y, width, height])
                        });
                    rows.push(json!({
                        "workspace": workspace_name,
                        "strip": named(&strip.name, strip.id.0.into()),
                        "group": group.id,
                        "tab": tab.id,
                        "window": window,
                        "kind": kind,
                        "app_id": app_id,
                        "title": tab.title,
                        "active": group_active && group.active_tab == tab.id,
                        "floating": workspace.layout.float_of(group.id).is_some(),
                        "rect": rect,
                        "frames": window.map_or(0, &frames_of),
                        "resource": resource,
                        "item": item_of(tab.id),
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

#[cfg(test)]
mod naming_tests {
    use super::*;

    /// A member's row is named as every surface names them: a person nobody named is someone
    /// without a name yet, and nobody is named by their key.
    #[test]
    fn a_member_nobody_named_reads_as_someone_without_a_name() {
        let unnamed = collab_types::presence::Shown {
            label: String::new(),
            ..collab_types::presence::Shown::default()
        };
        assert_eq!(
            row(&unnamed)["label"],
            serde_json::json!(tilo_ui_base::presence::unnamed_person_label())
        );
        let keyed = collab_types::presence::Shown {
            label: "3f".repeat(32),
            ..collab_types::presence::Shown::default()
        };
        assert_eq!(
            row(&keyed)["label"],
            serde_json::json!(tilo_ui_base::presence::unnamed_person_label()),
            "a key is never read as the person's name"
        );
        let named = collab_types::presence::Shown {
            label: "Ada".to_owned(),
            ..collab_types::presence::Shown::default()
        };
        assert_eq!(row(&named)["label"], serde_json::json!("Ada"));
    }
}
