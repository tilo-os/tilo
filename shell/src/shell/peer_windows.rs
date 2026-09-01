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

use crate::compositor::tab_binding::Placement;
use crate::shell::launcher;
use crate::shell::state::{
    GroupId, ResourceRef, StripId, TabId, TileContent, Verb, Workspace, WorkspaceTree,
};

/// Shown while the local instance is starting.
const OPENING: &str = "Opening here…";

/// One unexamined peer window, lifted out of the tree so the decision can
/// be made without holding a borrow on it.
struct Pending {
    tab: TabId,
    app_id: String,
    /// Index of the owning workspace in `tree.workspaces`.
    workspace: usize,
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
        if tree.workspaces.get(item.workspace).is_none() {
            continue;
        }
        let note = match open_for(tree, &item) {
            Ok(()) => {
                started.push(item.app_id.clone());
                OPENING.to_string()
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

/// Every peer window with no note on it yet.
fn pending(tree: &WorkspaceTree) -> Vec<Pending> {
    let mut out = Vec::new();
    for (idx, ws) in tree.workspaces.iter().enumerate() {
        for strip in &ws.strips {
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    if let TileContent::PeerWindow { app_id, note, .. } = &tab.tile
                        && note.is_empty()
                        && !app_id.trim().is_empty()
                    {
                        out.push(Pending {
                            tab: tab.id,
                            app_id: app_id.clone(),
                            workspace: idx,
                        });
                    }
                }
            }
        }
    }
    out
}

/// Start a local instance of `item`'s app, bound to `item`'s tab. `Err`
/// carries the sentence to put on the card — what is actually the case,
/// never a promise ("not installed", not "coming soon").
fn open_for(tree: &mut WorkspaceTree, item: &Pending) -> Result<(), String> {
    let Some(ws) = tree.workspaces.get(item.workspace) else {
        return Err("That workspace is gone".to_string());
    };
    let ws_id = ws.id;
    admits(ws, &item.app_id)?;
    // After `admits`, so nothing is granted for a launch the room refused.
    crate::shell::search::providers::applications::grant_file_manager_home(
        tree,
        ws_id,
        &item.app_id,
    );
    let Some(ws) = tree.workspaces.get(item.workspace) else {
        return Err("That workspace is gone".to_string());
    };
    // Before the launch, or a note written after races the window's toplevel.
    launcher::set_pending_placement(&item.app_id, Placement::IntoTab(item.tab));
    match launcher::launch_in_workspace(&item.app_id, ws) {
        Ok(_) => Ok(()),
        Err(err) => {
            launcher::clear_pending_placement(&item.app_id);
            Err(match err {
                launcher::LaunchError::NotInstalled { .. } => {
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
    for ws in &mut tree.workspaces {
        for strip in &mut ws.strips {
            for group in &mut strip.window_groups {
                for t in &mut group.tabs {
                    if t.id != tab {
                        continue;
                    }
                    if let TileContent::PeerWindow { note: slot, .. } = &mut t.tile {
                        *slot = note;
                        return true;
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

    const APP: &str = "dev.qos.word-processor";

    fn room_owned_by(owner: &str) -> Workspace {
        let mut ws = Workspace::test_default(WorkspaceId::from(next_id()), "shared");
        ws.owner = Some(owner.to_string());
        ws
    }

    const FILES: &str = "dev.qos.file-explorer";

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
            workspaces: vec![ws],
            active: ws_id,
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
        let (mut tree, _tab) = tree_with_peer_window_for(room_owned_by("quinten"), FILES);

        assert!(
            !may_read(&tree.workspaces[0], FILES, &home),
            "fixture sanity: the file manager must start with no grant on the home, \
             or this test cannot tell a granted one from a pre-granted one"
        );

        let _ = examine(&mut tree);

        assert!(
            may_read(&tree.workspaces[0], FILES, &home),
            "a file manager started on the room's behalf must be able to read the \
             user's home; without it the window opens on its sandbox directory and \
             says the folder is empty"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn opening_another_app_for_the_room_grants_it_no_home() {
        let home = dirs::home_dir().expect("the test machine has a home directory");
        let (mut tree, _tab) = tree_with_peer_window_for(room_owned_by("quinten"), APP);

        let _ = examine(&mut tree);

        assert!(
            !may_read(&tree.workspaces[0], APP, &home),
            "only the file manager gets the user's home; the word processor must not"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_refused_file_manager_is_granted_nothing() {
        let home = dirs::home_dir().expect("the test machine has a home directory");
        let mut ws = room_owned_by("quinten");
        ws.owner = None;
        let (mut tree, _tab) = tree_with_peer_window_for(ws, FILES);

        let _ = examine(&mut tree);

        assert!(
            !may_read(&tree.workspaces[0], FILES, &home),
            "a room that authorises no launch must not hand out the home either"
        );
    }

    #[test]
    fn a_window_this_machine_opened_for_the_room_is_found() {
        let mut ws = room_owned_by("quinten");
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
            opened_for_the_room(&ws, "dev.qos.terminal", 0).is_none(),
            "a different app is not this one"
        );
    }

    #[test]
    fn a_window_this_person_opened_is_not_one_to_return_to() {
        let mut ws = room_owned_by("quinten");
        let tab = Tab {
            id: next_id(),
            title: "Terminal".into(),
            tile: TileContent::Foreign {
                app_id: "dev.qos.terminal".into(),
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
            opened_for_the_room(&ws, "dev.qos.terminal", salt).is_none(),
            "a second terminal is an ordinary thing to want; a window this person \
             opened must not swallow the next launch of the same app"
        );
    }

    #[test]
    fn an_unanswered_card_is_not_a_window_to_return_to() {
        let (tree, _tab) = tree_with_peer_window(room_owned_by("quinten"));
        assert!(
            opened_for_the_room(&tree.workspaces[0], APP, 0).is_none(),
            "an unexamined peer window has no instance behind it yet"
        );
    }

    #[test]
    fn a_room_with_an_owner_admits_its_apps() {
        let ws = room_owned_by("quinten");
        assert_eq!(admits(&ws, APP), Ok(()));
    }

    /// Fail-closed: this is the state a room is in before its governance
    /// has replicated, exactly when a machine must not start processes
    /// on a peer's say-so.
    #[test]
    fn a_room_with_no_owner_admits_nothing() {
        let mut ws = room_owned_by("quinten");
        ws.owner = None;
        let refusal = admits(&ws, APP).expect_err("an ownerless room must authorise nothing");
        assert_eq!(refusal, "This workspace has no owner yet");
    }

    /// Goes *through* the capability model rather than around it —
    /// asserted against `Workspace::decide`, the same call every other
    /// enforcement point bottoms out in.
    #[test]
    fn a_restriction_on_the_app_stops_it_opening_here() {
        let mut ws = room_owned_by("quinten");
        let _ = ws.grant_capability(CapOverride {
            effect: Effect::Restrict,
            ..CapOverride::grant(
                crate::shell::state::Subject::Principal(Principal::Person("quinten".into())),
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
        assert_eq!(admits(&ws, "dev.qos.terminal"), Ok(()));
    }

    #[test]
    fn a_note_is_what_marks_a_window_answered() {
        let (mut tree, tab) = tree_with_peer_window(room_owned_by("quinten"));
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
    fn a_refusal_lands_on_the_tab_as_the_reason() {
        let mut ws = room_owned_by("quinten");
        ws.owner = None;
        let (mut tree, tab) = tree_with_peer_window(ws);
        assert!(examine(&mut tree), "an unexamined window must be decided");
        let note = tree.workspaces[0].strips[0].window_groups[0]
            .tabs
            .iter()
            .find(|t| t.id == tab)
            .map(|t| match &t.tile {
                TileContent::PeerWindow { note, .. } => note.clone(),
                other => panic!("expected a peer window, got {other:?}"),
            })
            .expect("the tab is still there");
        assert!(!note.is_empty(), "a refused window must state its reason");
        assert!(
            !note.to_ascii_lowercase().contains("coming soon"),
            "an app that is not here is not coming soon; got {note:?}"
        );
    }
}
