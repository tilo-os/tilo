//! Giving a peer's window a real window here.
//!
//! This OS replicates *documents*, not screens: a room carries state and
//! every member runs their own app against it. So a
//! [`TileContent::PeerWindow`] tab arriving in the room document is news
//! that the room has one open, and the honest local answer is to open
//! one too — with the app id and peer name it carries, checked against
//! the room owner's `Write` grant on `ResourceRef::App(id)` via
//! [`admits`], and launched through [`crate::shell::launcher`], binding
//! into the existing tab. `PeerWindow::note` labels the card and marks
//! the decision made, so a refusal or launch is not re-run on every sync.

use crate::shell::launcher;
use crate::shell::state::WorkspaceAuthority as _;
use crate::shell::state::{
    GroupId, ResourceRef, StripId, TabId, TileContent, Verb, Workspace, WorkspaceTree,
};
use crate::shell::tab_binding::Placement;

/// Shown while the local instance is starting.
pub(crate) const OPENING: &str = "Opening here…";
/// A restore that was attempted without Xwayland remains symbolic and is
/// retried if the compatibility bridge recovers. This is intentionally a
/// note, so the person sees why the card has not become a live window.
pub(crate) const X11_RETRY: &str = "Waiting for X11 compatibility to recover…";

/// One symbolic application window, lifted out of the tree so the decision
/// can be made without holding a borrow on it. `local` distinguishes a window
/// this machine persisted for session restore from a peer's room projection.
struct Pending {
    tab: TabId,
    app_id: String,
    /// Index of the owning workspace in `tree.workspaces`.
    workspace: usize,
    local: bool,
    retry_x11: bool,
}

/// Examine every peer window this machine has not yet answered for, and
/// start a local instance of each one it may. Returns whether anything
/// was decided, so the caller can repaint only when needed.
///
/// At most one launch per app id per call: the launcher correlates a
/// launch with its window by app id, so two in flight would have the
/// second displace the first.
pub fn examine(tree: &mut WorkspaceTree) -> bool {
    let todo = pending(tree);
    if todo.is_empty() {
        return false;
    }
    let mut started: Vec<String> = Vec::new();
    let mut decided = false;
    for item in todo {
        // One in flight per app; the rest wait for the next pass.
        if started.iter().any(|a| a.eq_ignore_ascii_case(&item.app_id)) {
            continue;
        }
        let Some(workspace) = tree.desktop.workspaces.get(item.workspace) else {
            continue;
        };
        // A same-identity continuation can discover the room before its
        // signed membership document arrives. Keep it pending so the next
        // reconciliation pass opens it as soon as authority is available.
        if !item.local && workspace.owner.is_none() {
            continue;
        }
        let note = match open_for(tree, &item) {
            Ok(OpenResult::Started) => {
                started.push(item.app_id.clone());
                OPENING.to_string()
            }
            Ok(OpenResult::Deferred(note)) => {
                if set_note(tree, item.tab, note.to_string()) {
                    decided = true;
                }
                continue;
            }
            Err(refusal) => refusal,
        };
        if set_note(tree, item.tab, note) {
            decided = true;
        }
    }
    decided
}

/// Whether any peer window is still waiting to be examined.
pub fn any_pending(tree: &WorkspaceTree) -> bool {
    !pending(tree).is_empty()
}

/// A host restore attempted during a terminal Xwayland outage is observable
/// only when the launch worker reports failure. Keep that symbolic tab
/// retryable instead of leaving the misleading `Opening…` note forever.
pub(crate) fn mark_failed_host_restore(tree: &mut WorkspaceTree, desktop_id: &str) -> bool {
    let Some(app) = crate::shell::host_apps::find(desktop_id) else {
        return false;
    };
    let mut changed = false;
    for ws in &mut tree.desktop.workspaces {
        for strip in &mut ws.strips {
            for group in &mut strip.window_groups {
                for tab in &mut group.tabs {
                    let TileContent::LocalApp { app_id, note, .. } = &mut tab.tile else {
                        continue;
                    };
                    if note == OPENING
                        && (app.desktop_id.eq_ignore_ascii_case(app_id)
                            || app
                                .window_identities
                                .iter()
                                .any(|id| id.eq_ignore_ascii_case(app_id)))
                    {
                        *note = X11_RETRY.to_string();
                        changed = true;
                    }
                }
            }
        }
    }
    changed
}

/// Settle a persisted host-app card whose process started but never produced
/// a surface. Unlike an Xwayland outage this is not automatically retryable:
/// retrying every frame would create an unbounded process loop.
pub(crate) fn mark_timed_out_host_restore(tree: &mut WorkspaceTree, desktop_id: &str) -> bool {
    let Some(app) = crate::shell::host_apps::find(desktop_id) else {
        return false;
    };
    let mut changed = false;
    for ws in &mut tree.workspaces {
        for strip in &mut ws.strips {
            for group in &mut strip.window_groups {
                for tab in &mut group.tabs {
                    let TileContent::LocalApp { app_id, note, .. } = &mut tab.tile else {
                        continue;
                    };
                    if note == OPENING
                        && (app.desktop_id.eq_ignore_ascii_case(app_id)
                            || app
                                .window_identities
                                .iter()
                                .any(|id| id.eq_ignore_ascii_case(app_id)))
                    {
                        *note = "The app started but did not open a window.".to_string();
                        changed = true;
                    }
                }
            }
        }
    }
    if changed {
        tree.flush_to_disk();
    }
    changed
}

/// Every peer window with no note on it yet.
fn pending(tree: &WorkspaceTree) -> Vec<Pending> {
    let mut out = Vec::new();
    for (idx, ws) in tree.desktop.workspaces.iter().enumerate() {
        for strip in &ws.strips {
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    match &tab.tile {
                        TileContent::PeerWindow { app_id, note, .. }
                        | TileContent::LocalApp { app_id, note, .. }
                            if (note.is_empty() || note == X11_RETRY)
                                && !app_id.trim().is_empty() =>
                        {
                            out.push(Pending {
                                tab: tab.id,
                                app_id: app_id.clone(),
                                workspace: idx,
                                local: matches!(&tab.tile, TileContent::LocalApp { .. }),
                                retry_x11: note == X11_RETRY,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    out
}

/// Older builds persisted a host window's toolkit identity and then marked
/// the card permanently missing when that identity was not a package id. If
/// the current desktop-entry catalogue can now resolve that identity, the
/// error is stale rather than user state: clear it so the normal restore path
/// can migrate and relaunch the app. A genuinely absent application remains
/// an answered card and is not retried on every tick.
pub fn retry_resolvable_host_app_errors(tree: &mut WorkspaceTree) -> bool {
    let mut changed = false;
    for workspace in &mut tree.workspaces {
        for strip in &mut workspace.strips {
            for group in &mut strip.window_groups {
                for tab in &mut group.tabs {
                    let TileContent::LocalApp { app_id, note, .. } = &mut tab.tile else {
                        continue;
                    };
                    if note == "Not installed on this machine"
                        && crate::shell::host_apps::resolve(app_id).is_some()
                    {
                        note.clear();
                        changed = true;
                    }
                }
            }
        }
    }
    changed
}

/// Start a local instance of `item`'s app, bound to `item`'s tab. `Err`
/// carries the sentence to put on the card — what is actually the case,
/// never a promise ("not installed", not "coming soon").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenResult {
    Started,
    Deferred(&'static str),
}

fn open_for(tree: &mut WorkspaceTree, item: &Pending) -> Result<OpenResult, String> {
    let Some(ws) = tree.desktop.workspaces.get(item.workspace) else {
        return Err("That workspace is gone".to_string());
    };
    let ws_id = ws.id;
    if !item.local {
        admits(ws, &item.app_id)?;
    }
    // A persisted host window can be X11-only. Host-session Xwayland starts
    // asynchronously after the public Wayland socket begins dispatching, so
    // session restore can reach this path first. Keep the symbolic LocalApp
    // untouched until the compatibility environment is published instead of
    // starting the app without DISPLAY and permanently replacing the retryable
    // state with an "Opening here…" card.
    if item.local && crate::shell::host_apps::resolve(&item.app_id).is_some() {
        let availability = crate::shell::host_apps::x11_availability();
        if availability == crate::shell::host_apps::X11Availability::Starting
            || (item.retry_x11 && availability != crate::shell::host_apps::X11Availability::Ready)
        {
            return Ok(OpenResult::Deferred(X11_RETRY));
        }
    }
    // After `admits`, so nothing is granted for a launch the room refused.
    if !item.local {
        crate::shell::search::providers::applications::grant_file_manager_home(
            tree,
            ws_id,
            &item.app_id,
        );
    }
    let Some(ws) = tree.desktop.workspaces.get(item.workspace) else {
        return Err("That workspace is gone".to_string());
    };
    // Before the launch, or a note written after races the window's toplevel.
    launcher::set_pending_placement(&item.app_id, Placement::IntoTab(item.tab));
    if let Ok(registry) = crate::shell::personal_continuity_registry::global_registry().lock()
        && let Ok(launch) =
            crate::shell::personal_continuity::LaunchIntent::app(item.app_id.clone())
        && let Some(record_instance) = registry.record_instance_for_tab(
            ws.room,
            crate::shell::personal_continuity::AppInstanceId(item.tab),
            &launch,
        )
    {
        if let Some(continuation) = registry.continuation_for_tab(
            ws.room,
            crate::shell::personal_continuity::AppInstanceId(item.tab),
            &launch,
        ) {
            launcher::stage_continuation_resume(
                ws.room,
                item.tab,
                continuation.resume.schema_version(),
                continuation.resume.bytes().to_vec(),
                record_instance.0,
            );
        } else {
            launcher::stage_continuation_record(ws.room, item.tab, record_instance.0);
        }
    }
    let launched = launcher::launch_in_workspace(&item.app_id, ws)
        .map(|_| ())
        .or_else(|err| {
            if item.local
                && matches!(err, launcher::LaunchError::NotInstalled { .. })
                && let Some(app) = crate::shell::host_apps::resolve(&item.app_id)
            {
                crate::shell::host_apps::note_placement(&app, Placement::IntoTab(item.tab));
                return crate::shell::host_apps::launch(&app.desktop_id, None).map(|_| ());
            }
            Err(err)
        });
    match launched {
        Ok(()) => Ok(OpenResult::Started),
        Err(err) => {
            launcher::clear_pending_placement(&item.app_id);
            Err(match err {
                launcher::LaunchError::NotInstalled { .. }
                | launcher::LaunchError::HostAppNotFound { .. } => {
                    "Not installed on this machine".to_string()
                }
                launcher::LaunchError::NotAGuiApp { .. } => {
                    "This app has no window to open".to_string()
                }
                other => format!("Could not open it here: {other}"),
            })
        }
    }
}

/// Whether `ws`'s room authorises starting `app_id` here. See the module
/// docs for why this is asked of the room's owner and with this verb.
fn admits(ws: &Workspace, app_id: &str) -> Result<(), String> {
    let Some(owner) = ws.owner.clone() else {
        return Err("This workspace has no owner yet".to_string());
    };
    let actor = ws.actor_for_person(&owner);
    if ws.allows(&actor, &ResourceRef::App(app_id.to_string()), Verb::Write) {
        return Ok(());
    }
    Err("This workspace does not permit opening it here".to_string())
}

/// Write `note` onto the peer window in `tab`. Returns whether it landed —
/// `false` if the tab went away, or stopped being a peer window, while the
/// decision was being made.
fn set_note(tree: &mut WorkspaceTree, tab: TabId, note: String) -> bool {
    for ws in &mut tree.desktop.workspaces {
        for strip in &mut ws.strips {
            for group in &mut strip.window_groups {
                for t in &mut group.tabs {
                    if t.id != tab {
                        continue;
                    }
                    match &mut t.tile {
                        TileContent::PeerWindow { note: slot, .. }
                        | TileContent::LocalApp { note: slot, .. } => {
                            if slot == &note {
                                return false;
                            }
                            *slot = note;
                            return true;
                        }
                        _ => {}
                    }
                    return false;
                }
            }
        }
    }
    false
}

/// Where this machine already has the room's copy of `app_id` on screen
/// in `ws`, if it does. Without this, opening an app that already has a
/// peer-started instance gets a second process, and propagates into the
/// room so every member's [`examine`] starts one too.
///
/// Only counts a tab this machine does *not* own — two terminals the
/// user opened themselves are two windows they asked for. A tab still
/// showing a card counts too, as long as it's been answered for (`note`
/// non-empty).
pub fn opened_for_the_room(
    ws: &Workspace,
    app_id: &str,
    local_salt: u16,
) -> Option<(StripId, GroupId, TabId)> {
    for strip in &ws.strips {
        for group in &strip.window_groups {
            for tab in &group.tabs {
                if crate::shell::state::peer_salt_of(tab.id) == local_salt {
                    continue;
                }
                let hosts = match &tab.tile {
                    TileContent::Foreign { app_id: a, .. } => a.eq_ignore_ascii_case(app_id),
                    TileContent::PeerWindow {
                        app_id: a, note, ..
                    } => !note.is_empty() && a.eq_ignore_ascii_case(app_id),
                    _ => false,
                };
                if hosts {
                    return Some((strip.id, group.id, tab.id));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{
        CapOverride, Effect, Principal, Scope, Strip, StripId, Tab, TileContent, WindowGroup,
        WindowWidth, Workspace, WorkspaceId, WorkspaceTree, next_id,
    };

    const APP: &str = "so.tilo.word-processor";

    fn room_owned_by(owner: &str) -> Workspace {
        let mut ws = Workspace::new(WorkspaceId::from(next_id()), "shared");
        ws.owner = Some(owner.to_string());
        ws
    }

    const FILES: &str = "so.tilo.file-explorer";

    fn tree_with_local_app(app_id: &str) -> (WorkspaceTree, TabId) {
        let mut ws = room_owned_by("ada");
        let tab = Tab {
            id: next_id(),
            title: "Legacy App".into(),
            tile: TileContent::LocalApp {
                app_id: app_id.into(),
                label: "Legacy App".into(),
                note: String::new(),
            },
        };
        let tab_id = tab.id;
        let group = WindowGroup {
            id: next_id(),
            tabs: vec![tab],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
        };
        let mut strip = Strip::new(StripId::from(next_id()), "main");
        strip.window_groups = vec![group];
        ws.strips = vec![strip];
        let ws_id = ws.id;
        (
            WorkspaceTree {
                desktop: crate::shell::state::DesktopTree {
                    workspaces: vec![ws],
                    active: ws_id,
                    ..Default::default()
                },
                ..Default::default()
            },
            tab_id,
        )
    }

    fn local_app_note(tree: &WorkspaceTree, tab_id: TabId) -> &str {
        tree.desktop
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .flat_map(|group| group.tabs.iter())
            .find_map(|tab| {
                (tab.id == tab_id).then(|| match &tab.tile {
                    TileContent::LocalApp { note, .. } => note.as_str(),
                    other => panic!("expected LocalApp, got {other:?}"),
                })
            })
            .expect("local app tab remains in the tree")
    }

    fn tree_with_peer_window(ws: Workspace) -> (WorkspaceTree, u64) {
        tree_with_peer_window_for(ws, APP)
    }

    fn tree_with_peer_window_for(ws: Workspace, app: &str) -> (WorkspaceTree, u64) {
        let mut ws = ws;
        let tab = Tab {
            id: peer_id(),
            title: "Word Processor".into(),
            tile: TileContent::PeerWindow {
                app_id: app.into(),
                label: "Word Processor".into(),
                note: String::new(),
            },
        };
        let tab_id = tab.id;
        let group = WindowGroup {
            id: next_id(),
            tabs: vec![tab],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
        };
        let mut strip = Strip::new(StripId::from(next_id()), "main");
        strip.window_groups = vec![group];
        ws.strips = vec![strip];
        let ws_id = ws.id;
        let tree = WorkspaceTree {
            desktop: crate::shell::state::DesktopTree {
                workspaces: vec![ws],
                active: ws_id,
                ..Default::default()
            },
            ..Default::default()
        };
        (tree, tab_id)
    }

    /// An id carrying a salt that isn't this process's, so tabs built
    /// with it look like a peer's.
    fn peer_id() -> u64 {
        const SOMEBODY_ELSE: u64 = 0xBEEF;
        (SOMEBODY_ELSE << crate::shell::state::PEER_ID_SALT_SHIFT) | (next_id() & 0xFFFF)
    }

    fn may_read(ws: &Workspace, app: &str, path: &std::path::Path) -> bool {
        ws.allows(
            &ws.actor_for_app(app),
            &ResourceRef::FsPath(path.to_string_lossy().into_owned()),
            Verb::Read,
        )
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_file_manager_opened_for_the_room_can_see_the_users_home() {
        let home = dirs::home_dir().expect("the test machine has a home directory");
        let (mut tree, _tab) = tree_with_peer_window_for(room_owned_by("ada"), FILES);

        assert!(
            !may_read(&tree.desktop.workspaces[0], FILES, &home),
            "fixture sanity: the file manager must start with no grant on the home, \
             or this test cannot tell a granted one from a pre-granted one"
        );

        let _ = examine(&mut tree);

        assert!(
            may_read(&tree.desktop.workspaces[0], FILES, &home),
            "a file manager started on the room's behalf must be able to read the \
             user's home; without it the window opens on its sandbox directory and \
             says the folder is empty"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn opening_another_app_for_the_room_grants_it_no_home() {
        let home = dirs::home_dir().expect("the test machine has a home directory");
        let (mut tree, _tab) = tree_with_peer_window_for(room_owned_by("ada"), APP);

        let _ = examine(&mut tree);

        assert!(
            !may_read(&tree.desktop.workspaces[0], APP, &home),
            "only the file manager gets the user's home; the word processor must not"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_refused_file_manager_is_granted_nothing() {
        let home = dirs::home_dir().expect("the test machine has a home directory");
        let mut ws = room_owned_by("ada");
        ws.owner = None;
        let (mut tree, _tab) = tree_with_peer_window_for(ws, FILES);

        let _ = examine(&mut tree);

        assert!(
            !may_read(&tree.desktop.workspaces[0], FILES, &home),
            "a room that authorises no launch must not hand out the home either"
        );
    }

    #[test]
    fn a_window_this_machine_opened_for_the_room_is_found() {
        let mut ws = room_owned_by("ada");
        let tab = Tab {
            id: peer_id(),
            title: "Word Processor".into(),
            tile: TileContent::Foreign {
                app_id: APP.into(),
                surface_id: 4711,
            },
        };
        let tab_id = tab.id;
        let group = WindowGroup {
            id: peer_id(),
            tabs: vec![tab],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
        };
        let mut strip = Strip::new(StripId::from(peer_id()), "main");
        strip.window_groups = vec![group];
        ws.strips = vec![strip];

        let found = opened_for_the_room(&ws, APP, 0);
        assert_eq!(
            found.map(|(_, _, t)| t),
            Some(tab_id),
            "a peer's tab already hosting this machine's window of the app has to be \
             found, or the launch opens a second one beside it"
        );
        assert!(
            opened_for_the_room(&ws, "so.tilo.terminal", 0).is_none(),
            "a different app is not this one"
        );
    }

    #[test]
    fn a_window_this_person_opened_is_not_one_to_return_to() {
        let mut ws = room_owned_by("ada");
        let tab = Tab {
            id: next_id(),
            title: "Terminal".into(),
            tile: TileContent::Foreign {
                app_id: "so.tilo.terminal".into(),
                surface_id: 99,
            },
        };
        let tab_id = tab.id;
        let group = WindowGroup {
            id: next_id(),
            tabs: vec![tab],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
        };
        let mut strip = Strip::new(StripId::from(next_id()), "main");
        strip.window_groups = vec![group];
        ws.strips = vec![strip];

        let salt = crate::shell::state::peer_salt_of(tab_id);
        assert!(
            opened_for_the_room(&ws, "so.tilo.terminal", salt).is_none(),
            "a second terminal is an ordinary thing to want; a window this person \
             opened must not swallow the next launch of the same app"
        );
    }

    #[test]
    fn an_unanswered_card_is_not_a_window_to_return_to() {
        let (tree, _tab) = tree_with_peer_window(room_owned_by("ada"));
        assert!(
            opened_for_the_room(&tree.desktop.workspaces[0], APP, 0).is_none(),
            "an unexamined peer window has no instance behind it yet"
        );
    }

    #[test]
    fn a_room_with_an_owner_admits_its_apps() {
        let ws = room_owned_by("ada");
        assert_eq!(admits(&ws, APP), Ok(()));
    }

    /// Fail-closed: this is the state a room is in before its governance
    /// has replicated, exactly when a machine must not start processes
    /// on a peer's say-so.
    #[test]
    fn a_room_with_no_owner_admits_nothing() {
        let mut ws = room_owned_by("ada");
        ws.owner = None;
        let refusal = admits(&ws, APP).expect_err("an ownerless room must authorise nothing");
        assert_eq!(refusal, "This workspace has no owner yet");
    }

    /// Goes *through* the capability model rather than around it —
    /// asserted against `Workspace::decide`, the same call every other
    /// enforcement point bottoms out in.
    #[test]
    fn a_restriction_on_the_app_stops_it_opening_here() {
        let mut ws = room_owned_by("ada");
        let _ = ws.grant_capability(CapOverride {
            effect: Effect::Restrict,
            ..CapOverride::grant(
                crate::shell::state::Subject::Principal(Principal::Person("ada".into())),
                Scope::App(APP.to_string()),
                Verb::Read,
            )
        });
        let refusal = admits(&ws, APP).expect_err("a restricted app must not be opened here");
        assert!(
            refusal.contains("does not permit"),
            "the card has to say why; got {refusal:?}"
        );
        // The restriction is about this app, not about the room: another
        // app in the same room is still admitted.
        assert_eq!(admits(&ws, "so.tilo.terminal"), Ok(()));
    }

    #[test]
    fn a_note_is_what_marks_a_window_answered() {
        let (mut tree, tab) = tree_with_peer_window(room_owned_by("ada"));
        assert!(any_pending(&tree), "an unexamined peer window is pending");
        assert!(set_note(
            &mut tree,
            tab,
            "Not installed on this machine".into()
        ));
        assert!(
            !any_pending(&tree),
            "a peer window that has been answered for must not be examined again"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn stale_missing_note_for_a_resolvable_host_app_is_retried_once() {
        let data = tempfile::tempdir().expect("temporary XDG data home");
        let applications = data.path().join("applications");
        std::fs::create_dir_all(&applications).expect("applications directory");
        std::fs::write(
            applications.join("rygel-preferences.desktop"),
            "[Desktop Entry]\nType=Application\nName=Rygel Preferences\nExec=/bin/true\nStartupWMClass=org.gnome.Rygel.Preferences\n",
        )
        .expect("desktop entry");
        let previous_data_home = std::env::var_os("XDG_DATA_HOME");
        unsafe { std::env::set_var("XDG_DATA_HOME", data.path()) };
        crate::shell::host_apps::invalidate_cache();

        let (mut tree, tab) = tree_with_local_app("org.gnome.Rygel.Preferences");
        assert!(set_note(
            &mut tree,
            tab,
            "Not installed on this machine".into()
        ));
        assert!(retry_resolvable_host_app_errors(&mut tree));
        assert_eq!(local_app_note(&tree, tab), "");
        assert!(any_pending(&tree));

        assert!(set_note(
            &mut tree,
            tab,
            "Not installed on this machine".into()
        ));
        if let TileContent::LocalApp { app_id, .. } =
            &mut tree.workspaces[0].strips[0].window_groups[0].tabs[0].tile
        {
            *app_id = "org.example.ActuallyMissing".into();
        }
        assert!(!retry_resolvable_host_app_errors(&mut tree));
        assert_eq!(local_app_note(&tree, tab), "Not installed on this machine");

        crate::shell::host_apps::invalidate_cache();
        unsafe {
            if let Some(previous) = previous_data_home {
                std::env::set_var("XDG_DATA_HOME", previous);
            } else {
                std::env::remove_var("XDG_DATA_HOME");
            }
        }
    }

    #[test]
    fn an_ownerless_window_stays_pending_until_room_authority_arrives() {
        let mut ws = room_owned_by("ada");
        ws.owner = None;
        let (mut tree, tab) = tree_with_peer_window(ws);
        assert!(
            !examine(&mut tree),
            "missing authority is transient and must not become a permanent refusal"
        );
        assert!(any_pending(&tree), "the next authority sync must retry it");
        let note = tree.desktop.workspaces[0].strips[0].window_groups[0]
            .tabs
            .iter()
            .find(|t| t.id == tab)
            .map(|t| match &t.tile {
                TileContent::PeerWindow { note, .. } => note.clone(),
                other => panic!("expected a peer window, got {other:?}"),
            })
            .expect("the tab is still there");
        assert!(
            note.is_empty(),
            "a temporary authority race must not leave a stale error card; got {note:?}"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn restored_host_window_waits_for_xwayland_without_becoming_a_stale_opening_card() {
        let data = tempfile::tempdir().expect("temporary XDG data home");
        let applications = data.path().join("applications");
        std::fs::create_dir_all(&applications).expect("applications directory");
        std::fs::write(
            applications.join("org.example.Legacy.desktop"),
            "[Desktop Entry]\nType=Application\nName=Legacy App\nExec=/bin/true\nStartupWMClass=LegacyWindow\n",
        )
        .expect("desktop entry");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_DATA_HOME", data.path());
        crate::shell::host_apps::invalidate_cache();
        crate::shell::host_apps::set_x11_starting();
        crate::shell::host_apps::set_x11_session(None);
        crate::shell::launcher::clear_pending_placements();

        let (mut tree, tab) = tree_with_local_app("LegacyWindow");
        assert!(crate::shell::host_apps::app_for_window_id("LegacyWindow").is_some());
        assert!(any_pending(&tree));
        assert!(
            examine(&mut tree),
            "waiting for Xwayland must leave a visible retryable restore state"
        );
        assert_eq!(local_app_note(&tree, tab), X11_RETRY);
        assert!(
            any_pending(&tree),
            "the same symbolic tab must be retried after DISPLAY is published"
        );
        assert!(
            crate::shell::launcher::placement_for_toplevel("LegacyWindow", 9001).is_none(),
            "deferral must not stage a placement that can expire before launch"
        );
        crate::shell::host_apps::set_x11_unavailable();
        assert!(
            !examine(&mut tree),
            "a retryable restore must not spin-launch while Xwayland is unavailable"
        );

        crate::shell::host_apps::invalidate_cache();
        crate::shell::launcher::clear_pending_placements();
        crate::shell::host_apps::set_x11_unavailable();
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn timed_out_host_restore_becomes_an_explicit_non_retrying_error() {
        let data = tempfile::tempdir().expect("temporary XDG data home");
        let applications = data.path().join("applications");
        std::fs::create_dir_all(&applications).expect("applications directory");
        std::fs::write(
            applications.join("org.example.Windowless.desktop"),
            "[Desktop Entry]\nType=Application\nName=Windowless App\nExec=/bin/true\nStartupWMClass=Windowless\n",
        )
        .expect("desktop entry");
        let previous_data_home = std::env::var_os("XDG_DATA_HOME");
        unsafe { std::env::set_var("XDG_DATA_HOME", data.path()) };
        crate::shell::host_apps::invalidate_cache();

        let (mut tree, tab) = tree_with_local_app("org.example.Windowless");
        assert!(set_note(&mut tree, tab, OPENING.to_string()));
        assert!(mark_timed_out_host_restore(
            &mut tree,
            "org.example.Windowless"
        ));
        assert_eq!(
            local_app_note(&tree, tab),
            "The app started but did not open a window."
        );
        assert!(
            !any_pending(&tree),
            "timeout errors must not relaunch every frame"
        );

        crate::shell::host_apps::invalidate_cache();
        unsafe {
            if let Some(previous) = previous_data_home {
                std::env::set_var("XDG_DATA_HOME", previous);
            } else {
                std::env::remove_var("XDG_DATA_HOME");
            }
        }
    }
}
