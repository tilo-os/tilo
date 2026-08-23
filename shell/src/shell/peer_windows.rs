//! Giving a peer's window a real window here.
//!
//! # The beat
//!
//! Someone in the room opens the word processor. It appears on the other
//! person's screen too, and when the first types the second sees the
//! words. That second half is what settles what the first half has to be.
//!
//! This OS replicates *documents*, not screens. A room carries state and
//! every member runs their own app against it — which is why two word
//! processors in one room converge, and why a picture of somebody else's
//! word processor could not host the beat that follows it. So a tab
//! arriving in the room document is not a request to show a remote
//! window; it is news that the room has one open, and the honest local
//! answer is to open one too.
//!
//! # What arrives, and what it is enough for
//!
//! [`TileContent::PeerWindow`] — the app id, and the peer's own name for
//! it. That is all the room document carries about a window, deliberately:
//! see `WorkspaceTree::project_owned_contents`, which writes a tab's
//! *existence* and never who has it open or where they have scrolled.
//!
//! It is enough. The app id says which app, the workspace says which
//! room, and the app's own state travels per record over the collaboration
//! broker's `(app_id, cluster_id, record_id)` channels — not through here.
//! Two instances of one app launched into one room find each other over
//! that path whether or not this module exists; what this module supplies
//! is the second instance. A record id in the tab would be a fourth copy
//! of a fact three layers already hold, and `plan/rooms.md` is explicit
//! that a room learns *that* a document is in it and nothing about who has
//! it open.
//!
//! # What authorises it
//!
//! A peer opening a window causes a process to start on this machine.
//! That has to answer to the capability model rather than route around it,
//! and it does, in the model's own terms:
//!
//! - **Which room.** A payload for a room this machine is not in never
//!   reaches the tree at all (`WorkspaceTree::apply_room_changes` returns
//!   before it stores anything), and the entries in it are per-owner
//!   write-checked there too. So by the time a `PeerWindow` exists, the
//!   local user is a member of that room and the tab is an entry its
//!   owner wrote. Membership is the local user's own consent: they
//!   accepted the invite.
//! - **Which app.** [`admits`] asks the room whether the app may be
//!   launched in it, with the verb the rest of the OS already uses for
//!   that act — `Write` on `ResourceRef::App(id)`, from
//!   [`crate::shell::search::visibility::required_capability`]: *"launching
//!   an app puts a tab in the workspace, which is a mutation of the room's
//!   contents"*. Asked of the room's **owner**, who is *"the one principal
//!   whose authority is not itself a capability"* — the root the room's
//!   governance descends from, replicated into `Workspace::owner` with the
//!   capabilities beside it. A room with no owner recorded authorises
//!   nothing.
//!
//!   The practical effect is that a `Restrict` on `Scope::App(x)` in a
//!   room stops x being started here by anybody's window, and a room whose
//!   governance has not arrived yet starts nothing at all.
//! - **What it may then reach.** Nothing new. The launch goes through
//!   [`crate::shell::launcher`] like every other, so the app gets the
//!   room's own capability list and `bwrap` binds it exactly one writable
//!   directory, `<data home>/qos-rooms/<room>/<pkg>`. An app started this
//!   way is inside the same sandbox as one started from the palette, with
//!   the same grants — there is no path here that widens either.
//!
//! # One window per tab
//!
//! The launch leaves a placement note naming the tab
//! ([`Placement::IntoTab`]), so the toplevel that comes back is bound into
//! the tab the room already has rather than minting a second one beside
//! it. Once bound, the tile is a [`TileContent::Foreign`] on this
//! machine's own surface, and `WorkspaceTree::localise_tile` keeps it
//! there against the peer's continued re-projection of theirs.
//!
//! It cannot echo back into the room: the projection writes only tabs this
//! machine owns, and a peer's tab is by construction not one of them. So
//! the local instance is invisible to the room, and the two machines do
//! not take turns opening windows at each other.
//!
//! # Deciding once
//!
//! `PeerWindow::note` is both the sentence on the card and the record that
//! a decision was made. An empty note is an unexamined tab; writing one is
//! what stops a refusal being re-stated, or a launch re-started, on every
//! sync round. A tab that is still a card is therefore always showing the
//! reason it is one.

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
/// start a local instance of each one it may.
///
/// Returns whether anything was decided, so the caller can repaint only
/// when a card's text changed or a window is on its way.
///
/// At most one launch per app id per call. The launcher correlates a
/// launch with the window it produces by app id and keeps one note per
/// package, so two launches of one app in flight at once would have the
/// second note displace the first and the first window land in the wrong
/// tab. The tabs left unexamined keep their empty note and are picked up
/// on the next call — which is why the caller re-runs while
/// [`any_pending`] holds rather than once per sync round.
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

/// Start a local instance of `item`'s app in `ws`'s room, bound to
/// `item`'s tab.
///
/// `Err` carries the sentence to put on the card — what is actually the
/// case, never a promise. An app that is not here is not "coming soon";
/// it is not installed, and saying so is the difference between a user who
/// knows what to do next and one who waits.
fn open_for(tree: &mut WorkspaceTree, item: &Pending) -> Result<(), String> {
    let Some(ws) = tree.workspaces.get(item.workspace) else {
        return Err("That workspace is gone".to_string());
    };
    let ws_id = ws.id;
    admits(ws, &item.app_id)?;
    // Whatever a launch from the palette does *around* the launch has to
    // happen here too, or an app started on the room's behalf is not the
    // same app the person would have opened from the palette. The file
    // manager's home grant is the one that bites: without it the window
    // opens on its own sandbox directory, finds nothing but its dotfiles
    // and says the folder is empty — which is what a guest's screen
    // showed while the host's listed the same home's contents.
    //
    // After `admits`, so nothing is granted for a launch the room has
    // just refused.
    crate::shell::search::providers::applications::grant_file_manager_home(
        tree,
        ws_id,
        &item.app_id,
    );
    let Some(ws) = tree.workspaces.get(item.workspace) else {
        return Err("That workspace is gone".to_string());
    };
    // The note goes down before the launch: the app can announce its
    // toplevel the moment it starts, and a note written afterwards would
    // be a race the window can win.
    launcher::set_pending_placement(&item.app_id, Placement::IntoTab(item.tab));
    // The room's capabilities are published first, exactly as a launch
    // from the palette does — so this instance runs with the room's
    // grants and no others.
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

/// Whether `ws`'s room authorises starting `app_id` here.
///
/// See the module docs for why this is asked of the room's owner and with
/// this verb. `Err` carries the sentence for the card.
fn admits(ws: &Workspace, app_id: &str) -> Result<(), String> {
    let Some(owner) = ws.owner.clone() else {
        return Err("This room has not said who governs it".to_string());
    };
    let actor = ws.actor_for_person(&owner);
    if ws.allows(&actor, &ResourceRef::App(app_id.to_string()), Verb::Write) {
        return Ok(());
    }
    Err("This room does not permit opening it here".to_string())
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
/// in `ws`, if it does.
///
/// Returns the coordinates [`crate::shell::chrome_state::ChromeState::focus_tab`]
/// wants: the strip, the group and the tab.
///
/// # What this is for
///
/// A person joins a room where somebody already has the word processor
/// open. [`examine`] starts a local instance for them, bound into the
/// peer's tab, and they now have a working word processor on screen.
/// If they then open the word processor themselves — from the palette,
/// not knowing or caring that the room already had one — they get a
/// second process, a second group, and two windows side by side showing
/// the same document, because it is the same room and therefore the same
/// record.
///
/// That is not two windows in the sense the strip means. It is one
/// window drawn twice, and it also does not stay a local problem: the
/// second tab is one *this* machine owns, so it projects into the room,
/// and every other member's [`examine`] then starts an instance of it
/// too. One person opening an app the room already had turns into a
/// window apiece for everybody, and a third member makes it three.
///
/// So a launch that finds this returns to the window that is already
/// there instead of starting another.
///
/// # Why only a peer's tab counts
///
/// The tab has to be one this machine does *not* own. Two windows of one
/// app that the user opened themselves are two windows they asked for —
/// two terminals in a workspace is an ordinary thing to want, and
/// `placement_for_toplevel` is written for a client that opens several
/// toplevels. What is being collapsed here is narrower: this machine
/// opening its own instance twice for one room, once because a peer had
/// the app open and once because the user asked.
///
/// A tab still showing a card counts as well, as long as it has been
/// answered for (`note` non-empty): the instance is starting, and a
/// launch during that window would race it and produce the same pair.
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
                    TileContent::PeerWindow { app_id: a, note, .. } => {
                        !note.is_empty() && a.eq_ignore_ascii_case(app_id)
                    }
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

    /// A workspace holding one peer window, nobody has answered for yet.
    fn tree_with_peer_window(ws: Workspace) -> (WorkspaceTree, u64) {
        tree_with_peer_window_for(ws, APP)
    }

    /// [`tree_with_peer_window`] for a named app.
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

    /// An id carrying a salt that is not this process's, so tabs built
    /// with it look like a peer's the way replication decides it —
    /// `peer_salt_of(id) != local_salt`.
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

    /// A file manager started because a peer had one open can see the
    /// user's home, exactly as one opened from the palette can.
    ///
    /// Every app is given one directory of its own and nothing else it
    /// was not granted. For the app whose entire subject is the user's
    /// files that means opening on a folder holding nothing but its own
    /// dotfiles and reporting, accurately, that it is empty — which is
    /// what a guest's screen showed while the host's listed the same
    /// home's contents. The grant used to be minted only on the palette
    /// path, and a peer's window does not come through it.
    ///
    /// Asserted through `examine` — the path a peer's window actually
    /// takes — and against `Workspace::allows`, the same call the
    /// sandbox's own capability resolution bottoms out in.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_file_manager_opened_for_the_room_can_see_the_users_home() {
        let home = dirs::home_dir().expect("the test machine has a home directory");
        let (mut tree, _tab) = tree_with_peer_window_for(room_owned_by("ada"), FILES);

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

    /// The grant is for the file manager and nothing else. It names one
    /// package id rather than a rule any package could satisfy.
    #[test]
    #[serial_test::serial(process_env)]
    fn opening_another_app_for_the_room_grants_it_no_home() {
        let home = dirs::home_dir().expect("the test machine has a home directory");
        let (mut tree, _tab) = tree_with_peer_window_for(room_owned_by("ada"), APP);

        let _ = examine(&mut tree);

        assert!(
            !may_read(&tree.workspaces[0], APP, &home),
            "only the file manager gets the user's home; the word processor must not"
        );
    }

    /// Nothing is granted for a launch the room has just refused.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_refused_file_manager_is_granted_nothing() {
        let home = dirs::home_dir().expect("the test machine has a home directory");
        let mut ws = room_owned_by("ada");
        ws.owner = None;
        let (mut tree, _tab) = tree_with_peer_window_for(ws, FILES);

        let _ = examine(&mut tree);

        assert!(
            !may_read(&tree.workspaces[0], FILES, &home),
            "a room that authorises no launch must not hand out the home either"
        );
    }

    /// The window a peer's tab is already hosting here is found, so a
    /// launch can return to it instead of starting a second copy.
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
            opened_for_the_room(&ws, "dev.qos.terminal", 0).is_none(),
            "a different app is not this one"
        );
    }

    /// Two windows of one app that the user opened *themselves* are two
    /// windows they asked for. Only this machine's own second copy for
    /// one room is collapsed, so the tab has to be a peer's.
    #[test]
    fn a_window_this_person_opened_is_not_one_to_return_to() {
        let mut ws = room_owned_by("ada");
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

    /// A card nobody has answered for yet is not a window. Nothing is
    /// running behind it, so a launch has to proceed.
    #[test]
    fn an_unanswered_card_is_not_a_window_to_return_to() {
        let (tree, _tab) = tree_with_peer_window(room_owned_by("ada"));
        assert!(
            opened_for_the_room(&tree.workspaces[0], APP, 0).is_none(),
            "an unexamined peer window has no instance behind it yet"
        );
    }

    /// The ordinary case: a room whose owner has arrived with the rest of
    /// its governance authorises its apps, so a peer's window is one this
    /// machine may open for itself.
    #[test]
    fn a_room_with_an_owner_admits_its_apps() {
        let ws = room_owned_by("ada");
        assert_eq!(admits(&ws, APP), Ok(()));
    }

    /// Fail-closed on the thing that actually gates: with nobody recorded
    /// as governing the room there is no principal whose authority the
    /// launch could descend from, so nothing starts. This is the state a
    /// room is in before its governance has replicated, which is exactly
    /// when a machine must not be starting processes on a peer's say-so.
    #[test]
    fn a_room_with_no_owner_admits_nothing() {
        let mut ws = room_owned_by("ada");
        ws.owner = None;
        let refusal = admits(&ws, APP).expect_err("an ownerless room must authorise nothing");
        assert!(
            refusal.contains("governs"),
            "the card has to say why; got {refusal:?}"
        );
    }

    /// A restriction in the room's own capability list stops the app being
    /// started here — the property that makes this go *through* the
    /// capability model rather than around it. Asserted against
    /// `Workspace::decide`, which is the same call every other enforcement
    /// point in the compositor bottoms out in.
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
        assert_eq!(admits(&ws, "dev.qos.terminal"), Ok(()));
    }

    /// The pending scan finds an unanswered peer window and stops finding
    /// it once a note is on it. This is what stops a refusal being
    /// re-stated, or a launch re-started, on every sync round.
    #[test]
    fn a_note_is_what_marks_a_window_answered() {
        let (mut tree, tab) = tree_with_peer_window(room_owned_by("ada"));
        assert!(any_pending(&tree), "an unexamined peer window is pending");
        assert!(set_note(&mut tree, tab, "Not installed on this machine".into()));
        assert!(
            !any_pending(&tree),
            "a peer window that has been answered for must not be examined again"
        );
    }

    /// A room this machine cannot open the app for keeps the tab and puts
    /// the reason on it — the whole point of the note being a sentence.
    /// Nothing here says "coming soon" about an app that is simply absent.
    #[test]
    fn a_refusal_lands_on_the_tab_as_the_reason() {
        let mut ws = room_owned_by("ada");
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
