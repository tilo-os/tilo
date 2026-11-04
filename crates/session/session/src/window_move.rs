//! Carrying a window the person moves to another workspace.
//!
//! A workspace is the sandbox the window's app runs in: the launcher hands the
//! app that workspace's grants, its services and the authority of the person
//! there, and the records the app keeps are that workspace's too. None of that
//! follows a tab into another workspace, so a window never crosses by itself.
//! What crosses is the tab: the app is started again in the workspace the
//! person carried it to, under the destination's grants, and the first window
//! it opens takes the tab where they put it. The instance left behind is asked
//! to close once that window is up, so what shows in the new workspace is never
//! the old workspace's process.
//!
//! The record does not cross either, and does not need to: every app keeps one
//! record per workspace, so the window carried opens what the destination
//! workspace keeps, and what it kept where it was stays there, with the
//! workspace it belongs to.
//!
//! A move into or out of a workspace other people are in is asked about first:
//! there the window and what its app opens are theirs to see, and leaving takes
//! the window away from them. Nothing moves until the person answers. Between
//! their own workspaces there is nobody to ask.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver};

use tilo_app_launch::Launch;
use tilo_compositor::Compositor;
use tilo_desktop_model::{DesktopTree, TabId, TileContent, Workspace, WorkspaceSlot};
use tilo_desktop_surface::DesktopNotifications;
use tilo_desktop_surface::NotificationOutcome;

use crate::Shared;
use crate::i18n::{t, t_args};
use crate::mount::apps::launch_of;
use crate::notices::Notice;

/// The action a move's notice offers: carry the window into the workspace the
/// person carried it to.
const CARRY: &str = "window-move-carry";

/// The action a move's notice offers: leave the window where it is.
const STAY: &str = "window-move-stay";

/// A move the person was asked about, waiting for their answer.
struct Asked {
    /// The tab they carried.
    tab: TabId,
    /// The workspace it is in now, where it stays unless they say otherwise.
    from: WorkspaceSlot,
    /// What that workspace is called, for the notice that answers a move which
    /// could not be made.
    from_name: String,
    /// The workspace they carried it to.
    to: WorkspaceSlot,
    /// What that workspace is called on the desk, and its identity: what the
    /// app is started into.
    to_name: String,
    to_id: String,
    /// The package that runs the window's app.
    app: String,
    /// What the person calls that app.
    app_name: String,
    /// The window it shows now, which the move leaves behind.
    window: u64,
}

/// A move under way: the tab waits where the person put it for the window of
/// the app started there, and the instance it left is closed once that comes.
struct Coming {
    tab: TabId,
    window: u64,
}

/// The windows the person carries between workspaces, and what they answer
/// before one crosses.
pub(crate) struct Moves {
    /// The moves the person was asked about, by the notice that asked.
    asking: RefCell<BTreeMap<u64, Asked>>,
    /// The moves waiting for the destination's window.
    coming: RefCell<Vec<Coming>>,
    /// What the person answered, from the desk's own outcome taps.
    answered: Receiver<(u64, String)>,
    /// The desk's notifications, which the notices are raised through.
    notices: DesktopNotifications,
}

impl Moves {
    /// Carries asked about on `notices`, whose answers come back here.
    pub(crate) fn new(notices: DesktopNotifications) -> Self {
        let (answered, answers) = mpsc::channel();
        notices.on_outcome(move |outcome| {
            if let NotificationOutcome::Activated {
                notification,
                action: Some(key),
            } = outcome
            {
                let _ = answered.send((notification.id, key.clone()));
            }
        });
        Self {
            asking: RefCell::new(BTreeMap::new()),
            coming: RefCell::new(Vec::new()),
            answered: answers,
            notices,
        }
    }

    /// Carry out what the person answered and what they carried since the last
    /// pass, and close the instance a carried tab left behind once its app has
    /// a window in the workspace they moved it to. Called once per pass.
    pub(crate) fn follow(&self, shared: &Shared, compositor: &mut Compositor) {
        for (id, key) in self.answered.try_iter() {
            let Some(asked) = self.asking.borrow_mut().remove(&id) else {
                // Another notice's answer, or one for a move already carried.
                continue;
            };
            if key == CARRY {
                self.carry(shared, asked);
            }
            // Any other answer keeps the window where it is, which is where it
            // never left.
        }
        for carried in shared.desktop.with_policy(|policy| policy.take_carried()) {
            self.consider(shared, carried.tab, carried.to);
        }
        self.left_behind(shared, compositor);
    }

    /// The person carried `tab` to the workspace at `to`: the app it shows is
    /// started again there, and a move that touches other people is asked about
    /// first.
    fn consider(&self, shared: &Shared, tab: TabId, to: WorkspaceSlot) {
        let read = shared.desktop.with_policy(|policy| {
            policy.handle().read(|tree| {
                let here = standing(tree, tab, &shared.person, &shared.earlier)?;
                let there = workspace_at(tree, to, &shared.person, &shared.earlier)?;
                Some((here, there))
            })
        });
        let Some((here, there)) = read else {
            // Nothing of an app shows in the tab any more, or the workspace it
            // was carried to went: there is no window to carry, and the tab
            // goes where the person put it.
            let _ = shared.desktop.with_policy(|policy| policy.carry(tab, to));
            return;
        };
        let Some(Launch::Package { app, .. }) = launch_of(&here.app) else {
            // One of this machine's own programs runs with no workspace around
            // it: no grants, no records, nothing the workspace it shows in
            // changes. Its tab crosses as any tab does.
            let _ = shared.desktop.with_policy(|policy| policy.carry(tab, to));
            return;
        };
        let asked = Asked {
            tab,
            from: here.workspace.slot,
            from_name: here.workspace.name,
            to,
            to_name: there.name,
            to_id: there.id,
            app_name: app_name(&app),
            app,
            window: here.window,
        };
        if here.workspace.others || there.others {
            self.ask(asked, here.workspace.others, there.others);
        } else {
            self.carry(shared, asked);
        }
    }

    /// Ask the person what the move means before anything crosses: the window
    /// and what its app opens there would be other people's to see, or leaving
    /// takes them away from it.
    fn ask(&self, asked: Asked, here: bool, there: bool) {
        let title = t_args(
            "window-move-ask-title",
            &[
                ("app", asked.app_name.clone().into()),
                ("workspace", asked.to_name.clone().into()),
            ],
        );
        let mut body = t("window-move-ask-what");
        if there {
            body.push(' ');
            body.push_str(&t_args(
                "window-move-ask-they-see",
                &[("workspace", asked.to_name.clone().into())],
            ));
        }
        if here {
            body.push(' ');
            body.push_str(&t_args(
                "window-move-ask-you-leave",
                &[("workspace", asked.from_name.clone().into())],
            ));
        }
        let mut notice = Notice::new(t("window-move-app-name"), 1, title, body);
        notice.actions = vec![
            (CARRY.to_owned(), t("window-move-ask-carry")),
            (STAY.to_owned(), t("window-move-ask-stay")),
        ];
        let id = notice.tell_desk(&self.notices);
        self.asking.borrow_mut().insert(id, asked);
    }

    /// Carry the tab to the workspace the person chose and start the app again
    /// there. A start that fails puts the tab back where it was, still showing
    /// the window it had, and says so.
    fn carry(&self, shared: &Shared, asked: Asked) {
        let Asked {
            tab,
            from,
            from_name,
            to,
            to_name,
            to_id,
            app,
            app_name,
            window,
        } = asked;
        // The tab takes the first window the app opens in the destination, and
        // waits there for it.
        if !shared.desktop.with_policy(|policy| policy.adopt(&app, tab)) {
            // The tab went while they were asked.
            return;
        }
        if let Err(error) = shared.desktop.with_policy(|policy| policy.carry(tab, to)) {
            shared
                .desktop
                .with_policy(|policy| policy.give_up_held(&app));
            tracing::warn!(%error, %app, "the window was not carried");
            return;
        }
        let started = shared.launch.as_deref().is_some_and(|apps| {
            let workspace = apps.workspace_named(&to_id);
            let launch = Launch::Package {
                app: app.clone(),
                open: None,
            };
            match apps.start_beside(&launch, &workspace) {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        %app,
                        workspace = %to_name,
                        "an app did not start where the person carried its window"
                    );
                    false
                }
            }
        });
        if !started {
            back(shared, tab, from, &app);
            self.failed(&app_name, &to_name, &from_name);
            return;
        }
        self.coming.borrow_mut().push(Coming { tab, window });
    }

    /// Say the window stayed where it was, and where.
    fn failed(&self, app: &str, workspace: &str, source: &str) {
        let title = t_args(
            "window-move-failed-title",
            &[
                ("app", app.to_owned().into()),
                ("workspace", workspace.to_owned().into()),
            ],
        );
        let body = t_args(
            "window-move-failed-body",
            &[("workspace", source.to_owned().into())],
        );
        Notice::new(t("window-move-app-name"), 1, title, body).tell_desk(&self.notices);
    }

    /// The window of the app started in the destination took the tab: the
    /// instance the tab left behind is asked to close, as its own close button
    /// would. A tab that went meantime takes its window with it.
    fn left_behind(&self, shared: &Shared, compositor: &mut Compositor) {
        let mut coming = self.coming.borrow_mut();
        coming.retain(|underway| {
            let landed = shared.desktop.with_policy(|policy| {
                policy
                    .handle()
                    .read(|tree| match tab_tile(tree, underway.tab) {
                        None => Landed::Gone,
                        Some(TileContent::Foreign { surface_id, .. })
                            if *surface_id != underway.window =>
                        {
                            Landed::Came
                        }
                        Some(_) => Landed::Waiting,
                    })
            });
            match landed {
                Landed::Waiting => true,
                Landed::Came | Landed::Gone => {
                    compositor.close_window(underway.window);
                    false
                }
            }
        });
    }
}

impl crate::Session {
    /// Carry out what the person answered and what they carried to another
    /// workspace since the last pass: they are asked first if the move crosses
    /// to or from other people, then the app runs there, and the window it left
    /// closes once the app has one there.
    pub(crate) fn follow_window_moves(&mut self) {
        self.shared.moves.follow(&self.shared, &mut self.compositor);
    }
}

/// Nothing started where the person put the tab: it goes back where it was,
/// still showing the window the app has there.
fn back(shared: &Shared, tab: TabId, from: WorkspaceSlot, app: &str) {
    shared
        .desktop
        .with_policy(|policy| policy.give_up_held(app));
    let _ = shared.desktop.with_policy(|policy| policy.carry(tab, from));
}

/// Whether the tab a move waits on took the window the app started for it.
enum Landed {
    /// The tab holds another window of the app now: the move landed.
    Came,
    /// The window has not mapped yet: the tab still shows the one it had.
    Waiting,
    /// The tab is gone.
    Gone,
}

/// A workspace as a move reads it.
struct WorkspaceRead {
    slot: WorkspaceSlot,
    id: String,
    name: String,
    /// Whether anyone besides this person belongs to it: what it holds is
    /// shared with them whether or not they are here.
    others: bool,
}

/// What stands at `tab`: the window filling it, the app that window belongs to,
/// and the workspace it is in.
struct Standing {
    app: String,
    window: u64,
    workspace: WorkspaceRead,
}

/// The tile `tab` holds, as the desk has it.
fn tab_tile(tree: &DesktopTree, tab: TabId) -> Option<&TileContent> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .find(|held| held.id == tab)
        .map(|held| &held.tile)
}

/// The window `tab` shows and the workspace the tab stands in. `None` when the
/// tab is not on the desk or holds no window of an app this device started.
fn standing(tree: &DesktopTree, tab: TabId, person: &str, earlier: &[String]) -> Option<Standing> {
    let TileContent::Foreign { app_id, surface_id } = tab_tile(tree, tab)? else {
        return None;
    };
    let workspace = tree.workspaces.iter().find(|workspace| {
        workspace.strips.iter().any(|strip| {
            strip
                .window_groups
                .iter()
                .any(|group| group.tabs.iter().any(|held| held.id == tab))
        })
    })?;
    Some(Standing {
        app: app_id.clone(),
        window: *surface_id,
        workspace: read(workspace, person, earlier),
    })
}

/// The workspace at `slot`, as a move reads it.
fn workspace_at(
    tree: &DesktopTree,
    slot: WorkspaceSlot,
    person: &str,
    earlier: &[String],
) -> Option<WorkspaceRead> {
    tree.workspaces
        .iter()
        .find(|workspace| workspace.slot == slot)
        .map(|workspace| read(workspace, person, earlier))
}

/// `workspace` as a move reads it.
fn read(workspace: &Workspace, person: &str, earlier: &[String]) -> WorkspaceRead {
    let mine = |id: &str| id == person || earlier.iter().any(|old| old == id);
    WorkspaceRead {
        slot: workspace.slot,
        id: workspace.id().to_string(),
        name: workspace.name.clone(),
        others: workspace
            .workspace_readers_at(tilo_clock::unix_now())
            .into_iter()
            .any(|(reader, _)| !mine(&reader)),
    }
}

/// What the person calls `app`, when this machine knows a package by that name.
fn app_name(app: &str) -> String {
    tilo_installed_apps::read_one_from_env(app)
        .ok()
        .flatten()
        .map(|installed| installed.display_name)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| app.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::Role;

    /// A workspace owned by `owner`, with `members` in it.
    fn workspace(owner: &str, members: &[&str]) -> Workspace {
        let mut workspace = Workspace::create("Studio");
        workspace.set_owner(owner.to_owned());
        for member in members {
            workspace.admit((*member).to_owned(), Role::Editor);
        }
        workspace
    }

    /// A move crosses a sharing boundary exactly when someone besides the
    /// person may read the workspace it leaves or enters: the window and what
    /// its app opens there are theirs to see, or leaving takes it from them.
    #[test]
    fn a_move_is_shared_when_someone_else_may_read_either_workspace() {
        let alone = workspace("pat", &[]);
        assert!(
            !read(&alone, "pat", &[]).others,
            "the person's own workspace"
        );
        assert!(
            !read(&alone, "pat", &["pat-was".to_owned()]).others,
            "a workspace under the name this person held before"
        );
        assert!(
            !read(&workspace("pat-was", &[]), "pat", &["pat-was".to_owned()]).others,
            "one of the person's own is its owner"
        );
        assert!(
            read(&workspace("pat", &["ada"]), "pat", &[]).others,
            "a member is in it"
        );
        assert!(
            read(&workspace("ada", &["pat"]), "pat", &[]).others,
            "someone else owns it"
        );
    }
}
