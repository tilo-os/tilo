//! The windows this device left open come back: each saved app is asked for its window again, and
//! the window takes the tab the person left it in.
//! A saved window is kept as the app that opens it ([`tilo_desktop_model::TileContent::LocalApp`]),
//! because a live window's surface belongs to the compositor that mapped it and means nothing after
//! a restart. So restoring is asking each of those apps to run again and giving its window the tab,
//! the group and the strip it was in — the layout is the same before and after, and no second tab
//! opens beside a saved one.
//! One app is asked at a time, the workspace in front's first: started together after a login, a
//! whole layout of apps fights over CPU, disk and memory, and each one shows its window several times
//! later than the first would alone; one after another, the first is up soonest and the next starts
//! on a quieter machine. The next is asked once the one before has its window, or could not open
//! one.
//! A tab whose app is gone — not installed any more, or it started nothing — says so where the
//! window would be, naming the app and why, and nothing else on the shell changes: the person closes
//! it when they are done with it. The answer stands for as long as the window would have, and the
//! next login asks again: by then the app may be installed, or the reason it would not start gone.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::platform::surfaces::ShellDesktop;
use tilo_actions::ApiError;
use tilo_app_launch::Launch;
use tilo_desktop_model::{Layout, TabId, TileContent, Workspace, WorkspaceId};

use crate::apps::{self, Apps};
use crate::i18n::t_args;
use crate::windows_compat::Reopen;
use tilo_session_core::desktop::outcome::Closed;

/// How long a saved window may take to show itself before its tab says it did not open: an app that
/// started and never mapped is not coming.
const OPEN_DEADLINE: Duration = Duration::from_secs(15);

/// How long a saved tab whose Windows program the service has not read yet waits before it asks
/// again: the service reads its capsules on a thread of its own, and the next pass is due at once.
const SERVICE_RETRY: Duration = Duration::from_millis(15);

/// How long the windows of the apps kept across an in-place restart wait, unshown, for the saved
/// layout that holds their tabs. The layout reaches a desktop that just started from what the
/// workspaces hold, after the keepers already answered; a window shown before it is there takes a
/// new tab, and its own saved tab would be taken for one the app closed. A layout that never
/// comes (its workspace is not readable on this device any more) shows them as new windows after
/// this.
const KEPT_WAIT: Duration = Duration::from_secs(15);

/// The saved windows of a layout, asked for again one at a time.
#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a distinct one-way step of the restore, not a mode"
)]
pub(crate) struct Restore {
    /// The saved windows not asked for yet, in the order they are asked.
    waiting: VecDeque<Saved>,
    /// The one whose app was asked for and whose window has not come yet.
    opening: Option<Opening>,
    /// The ones of a workspace this device waits for its host to admit it to: each tab says so,
    /// and its app is asked for once the host took the device in, as if it had just been reached.
    held: Vec<Saved>,
    /// When [`Self::held`] was last read against admission: once a second, not on every pass.
    held_checked: Option<Instant>,
    /// Whether the layout has been laid out from the workspaces' layouts, which happens once.
    seeded: bool,
    /// Whether the session has begun restoring at all: its first look at the layouts ran, even
    /// when there was nothing to lay out (a device with no workspace yet).
    begun: bool,
    /// The apps kept across a restart whose windows wait, unshown, for the saved tabs they stood
    /// in to be laid out ([`KEPT_WAIT`]), and since when: they are taken from the launcher once.
    #[cfg(target_os = "linux")]
    kept: Vec<tilo_app_launch::Adopted>,
    kept_since: Option<Instant>,
    /// Every workspace's layout has been read once, or this session reads none: a tab a kept
    /// window names that is not laid out by then is gone.
    read: bool,
    /// What the windows of those apps announce themselves by, and what they were asked as: a saved
    /// tab of one of them that no window of theirs claimed is a window the app closed while the
    /// desktop was away, and is not asked for again, which would start a second instance of an app
    /// that still runs. An app that ended while the desktop was away is one step further: its tabs
    /// close the same way, and its end is reported rather than the app started again. An app whose
    /// keeper was killed outright never ended, so its tab stays to say it stopped working.
    kept_apps: Vec<String>,
    /// The kept launches whose keeper was killed outright while no desktop was there, until their
    /// saved tabs say the app stopped working ([`Self::show_stopped`]).
    #[cfg(target_os = "linux")]
    stopped: Vec<tilo_app_launch::kept::KeptLaunch>,
    /// The revision of the layout the kept windows' keepers were last told their tabs at.
    placed_at: Option<u64>,
    /// The apps still running from before an update that this desktop cannot show, while they
    /// run: a saved tab of theirs, whenever it comes, says so rather than starting a second copy.
    #[cfg(target_os = "linux")]
    outdated: Vec<tilo_app_launch::Adopted>,
    /// A saved tab waited since the last pass for the Windows compatibility service to read what
    /// it can open: another pass is due at once rather than on the next frame.
    service_pending: bool,
    /// The saved tabs this restore asked a window for: a window that came into one of them is the
    /// restore's own doing, not one the person opened beside a saved tab.
    opened: Vec<TabId>,
    /// The windows already standing for a saved tab this restore closed, because the person had
    /// opened the app after the restart: the app's other saved windows are still asked for.
    spoken_for: Vec<TabId>,
}

/// How a saved window's app starts again: a Tilo package or a machine application, or the
/// Windows program of the class the window showed under.
#[derive(Clone)]
enum Start {
    App(Launch),
    /// The entry the compatibility service opens, as the session saw its window here.
    Windows(String),
    /// The entry of a capsule's setup or uninstaller: it ran once, and nothing opens it again.
    Setup(String),
}

/// A window the person left open, and the app that opens it again.
struct Saved {
    tab: TabId,
    /// The app as its windows announce themselves: what the tab holds, and what the window that
    /// comes is matched by.
    app: String,
    /// The name the person knew it by, for what the tab says when the app cannot open.
    name: String,
    /// How to start it again, when this machine still has it.
    start: Option<Start>,
    /// The item its window opened on, as its workspace's layout keeps it beside the tab: what
    /// the app is asked to open on, on every device.
    shows: String,
}

/// The app asked for a saved tab, and when.
#[derive(Clone)]
struct Opening {
    tab: TabId,
    /// The app the tab waits for, as its windows announce themselves.
    app: String,
    name: String,
    asked: Instant,
}

impl Restore {
    /// The session has begun restoring the windows this device left open: it has looked at the
    /// layouts, whether or not they held any to open.
    pub(crate) fn begin(&mut self) {
        self.begun = true;
    }

    /// This session reads no workspace's layout at all: what it laid out is all there is.
    pub(crate) fn without_layouts(&mut self) {
        self.seeded = true;
        self.read = true;
    }

    /// Every workspace's layout has been read once.
    pub(crate) fn layouts_read(&mut self) {
        self.read = true;
    }

    /// Whether the restore has begun: what says a desktop is usable, beside its first frames.
    pub(crate) fn started(&self) -> bool {
        self.seeded || self.begun
    }

    /// Take up the windows `tree` holds the first time this session lays the layout out from the
    /// workspaces' layouts: every window that opens again, the ones on the shell in front first, then
    /// the rest in the order the person left them. Whether this is that first time, in which case
    /// what it took is all of them and no window is one that arrived.
    /// `under_way` says which of this device's places a launch of this session holds for the window
    /// it started: the person may open an app before the first layout is laid out. `items` is the
    /// item each window was opened on, by tab, as the workspaces' layouts hold it: a window that
    /// showed a conversation opens there again.
    pub(crate) fn start(
        &mut self,
        before: &Layout,
        tree: &mut Layout,
        under_way: &dyn Fn(TabId) -> bool,
        items: &std::collections::BTreeMap<TabId, String>,
    ) -> bool {
        if self.seeded {
            return false;
        }
        self.seeded = true;
        without_unfilled(tree, under_way);
        // A window this device already shows is one it opened: its app is running here, and there
        // is nothing to wait for. What the workspace's layout brought is what waits for its app.
        #[expect(
            clippy::redundant_closure_for_method_calls,
            reason = "the field, not a method: a tab's id is read, not asked for"
        )]
        let here: std::collections::HashSet<TabId> = before
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.tabs())
            .map(|tab| tab.id)
            .collect();
        let mut saved: Vec<(WorkspaceId, Saved)> = waiting_in(tree, items, |_| true)
            .into_iter()
            .filter(|(_, saved)| !here.contains(&saved.tab))
            .collect();
        // Laid out after the kept apps claimed their tabs: what is left of theirs closed meanwhile.
        saved.retain(|(_, saved)| {
            let closed = self.kept_by(saved);
            if closed {
                tree.close_tab(saved.tab);
            }
            !closed
        });
        // Stable, so the layout's own order stands within each workspace.
        saved.sort_by_key(|(workspace, _)| *workspace != tree.active);
        self.waiting = saved.into_iter().map(|(_, saved)| saved).collect();
        true
    }

    /// Ask, after what is waiting already, for the windows of the workspace `workspace_id` of `tree` that
    /// wait for their apps: a workspace put on the shell with windows to open, as a branch is.
    pub(crate) fn add(
        &mut self,
        tree: &Layout,
        workspace_id: WorkspaceId,
        items: &std::collections::BTreeMap<TabId, String>,
    ) {
        self.waiting.extend(
            waiting_in(tree, items, |workspace| workspace == workspace_id)
                .into_iter()
                .map(|(_, saved)| saved),
        );
    }

    /// Ask, after what is waiting already, for the windows `after` waits for that `before` did
    /// not: the windows someone else in a workspace opened, which this device opens its own
    /// instance of, each in the tab the workspace put it in.
    pub(crate) fn arrived(
        &mut self,
        before: &Layout,
        after: &Layout,
        items: &std::collections::BTreeMap<TabId, String>,
    ) {
        let known: std::collections::HashSet<TabId> = waiting_in(before, items, |_| true)
            .into_iter()
            .map(|(_, saved)| saved.tab)
            .collect();
        self.waiting.extend(
            waiting_in(after, items, |_| true)
                .into_iter()
                .map(|(_, saved)| saved)
                .filter(|saved| !known.contains(&saved.tab)),
        );
    }

    /// When the app asked for a saved window gives up and its tab says so.
    /// Or when the kept windows stop waiting for their saved layout.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        #[cfg(target_os = "linux")]
        let kept = self
            .kept_since
            .filter(|_| !self.kept.is_empty() || !self.stopped.is_empty())
            .and_then(|since| since.checked_add(KEPT_WAIT));
        #[cfg(not(target_os = "linux"))]
        let kept = None;
        let opening = self
            .opening
            .as_ref()
            .and_then(|opening| opening.asked.checked_add(OPEN_DEADLINE));
        // The compatibility service reads what it can open on a thread of its own: a tab waiting
        // for that is asked again on the next pass, not after a frame.
        let service = self
            .service_pending
            .then(|| Instant::now().checked_add(SERVICE_RETRY))
            .flatten();
        opening.into_iter().chain(kept).chain(service).min()
    }

    /// Ask for the next saved window when the one before has come, or say in its tab why it did
    /// not. A window of a workspace whose host has not taken this device in yet (`awaited` names
    /// that host, by the workspace's id) is held: its tab says what it waits for, and its app is
    /// asked for once the host admitted the device. Started before, the app would take part in
    /// nothing there, and would not join once the device is admitted.
    pub(crate) fn follow(
        &mut self,
        desktop: &ShellDesktop,
        apps: Option<&Apps>,
        awaited: &dyn Fn(&str) -> Option<String>,
    ) {
        let Some(apps) = apps else {
            // This session starts no apps at all, so nothing will come for a saved window: the tabs
            // stay as they are.
            return;
        };
        if self.started() && self.kept_since.is_none() {
            self.kept_since = Some(Instant::now());
            #[cfg(target_os = "linux")]
            {
                let taken = apps.take_kept();
                self.kept = taken.kept;
                for launch in taken.ended {
                    tracing::info!(
                        target: tilo_startup_metrics::KEPT_APPS_TARGET,
                        app = %launch.app,
                        "ended while the desktop was away: its saved tabs close"
                    );
                    self.keep_identities(launch);
                }
                self.stopped = taken.killed;
                self.close_left_by_kept(desktop);
            }
        }
        #[cfg(target_os = "linux")]
        {
            let shown = self.outdated.len();
            for keeper in self.claim_kept(desktop) {
                apps.attach_kept(keeper);
            }
            self.claim_outdated(apps, shown);
            // Before the tabs are asked for: no app is started for one that says its app stopped.
            self.show_stopped(desktop);
        }
        // A saved tab's Windows program the service would not open: the tab says so.
        for refused in apps.take_windows_refused() {
            let note = not_installed(&refused.name);
            desktop.with_policy(|policy| policy.note_in_tab(refused.tab, &note));
            desktop.with_policy(|policy| policy.give_up_held(&refused.app));
            if self
                .opening
                .as_ref()
                .is_some_and(|opening| opening.tab == refused.tab)
            {
                self.opening = None;
            }
        }
        self.service_pending = false;
        self.release_admitted(desktop, awaited);
        // A saved tab may be one a kept window is about to claim: no app is started for one
        // while a kept app still waits for the layout.
        #[cfg(target_os = "linux")]
        if !self.kept.is_empty() {
            return;
        }
        loop {
            if self.waits_for_opening(desktop) {
                return;
            }
            let Some(saved) = self.waiting.pop_front() else {
                return;
            };
            // The person may have opened the app while its saved window was still waiting: its
            // window comes into that tab now, or came, and asking for it again would start the app
            // a second time.
            let coming = desktop.with_policy(|policy| {
                policy.on_its_way(saved.tab) || !policy.waits_for_window(saved.tab)
            });
            if coming {
                continue;
            }
            // A tab that names no app has nothing to start: it stays as it stands, for a kept
            // window that claims it ([`Self::claim_kept`]).
            if saved.app.trim().is_empty() {
                continue;
            }
            let host = workspace_holding(desktop, saved.tab, |held| held.id().to_string())
                .and_then(|workspace| awaited(&workspace));
            if let Some(host) = host {
                let note = waits_for_host(&saved.name, &host);
                desktop.with_policy(|policy| policy.note_in_tab(saved.tab, &note));
                self.held.push(saved);
                continue;
            }
            // The person opened this app before this session read the layout: the window the
            // launch started takes the tab its window was left in, and it is not asked for again.
            if self.take_over_launch(desktop, apps, &saved) {
                return;
            }
            // The person opened the app after the restart, and its window stands here already.
            if self.close_beside_a_window(desktop, &saved) {
                continue;
            }
            if self.open(desktop, apps, saved) {
                return;
            }
        }
    }

    /// Take up the window the restore asked for, once it is due. Whether the tab it was asked
    /// into still waits for its window: until the deadline, or when the window came. Past the
    /// deadline the tab takes the app's place, or goes when a window of the app the person opened
    /// stands elsewhere in the workspace.
    fn waits_for_opening(&mut self, desktop: &ShellDesktop) -> bool {
        let Some(opening) = self.opening.clone() else {
            return false;
        };
        let waiting = desktop.with_policy(|policy| policy.waits_for_window(opening.tab));
        if waiting && opening.asked.elapsed() < OPEN_DEADLINE {
            return true;
        }
        self.opening = None;
        if waiting {
            // The app opened a window of its own while this tab waited, as it raises the one it
            // has instead of opening another: the tab is the old place of a window the person
            // sees, not one whose app opened nothing.
            if let Some(shown) = self.opened_by_person(desktop, opening.tab, &opening.app) {
                self.spoken_for.push(shown);
                desktop.with_policy(|policy| policy.release_tab(opening.tab));
            } else {
                tracing::warn!(
                    app = %opening.name,
                    "a window this device left open did not come"
                );
                let note = did_not_open(&opening.name);
                desktop.with_policy(|policy| policy.note_in_tab(opening.tab, &note));
            }
        }
        false
    }

    /// The window a launch of `saved`'s app started takes the tab its window was left in, rather
    /// than a second window being asked for: the person opened the app before this session read
    /// the layout. Whether the saved tab waits for that window now.
    fn take_over_launch(&mut self, desktop: &ShellDesktop, apps: &Apps, saved: &Saved) -> bool {
        let launched = desktop.with_policy(|policy| policy.launch_tab_of(&saved.app));
        let theirs = launched.filter(|tab| {
            *tab != saved.tab
                && desktop.with_policy(|policy| {
                    policy.waits_for_window(*tab) || policy.holds_a_place(*tab)
                })
                && same_workspace(desktop, *tab, saved.tab)
        });
        // A launch whose tab the shell has not made yet holds a place all the same.
        let unplaced =
            launched.is_none() && desktop.with_policy(|policy| policy.holds_launch_of(&saved.app));
        if theirs.is_none() && !unplaced {
            return false;
        }
        if let Some(theirs) = theirs {
            // The launch's own tab was made and its window is on its way: the place goes.
            desktop.with_policy(|policy| policy.release_tab(theirs));
        } else if !apps.start_into_tab(&saved.app, saved.tab) {
            // The place is held and nothing was planned for it: it goes, and the tab waits as one
            // the restore asks for would.
            desktop.with_policy(|policy| policy.give_up_held(&saved.app));
        }
        // The tab the person left the window in takes the window: the launch's own place, when it
        // had one, is given up before it, so the window lands here and nowhere else.
        desktop.with_policy(|policy| policy.adopt(&saved.app, saved.tab, None));
        self.opened.push(saved.tab);
        self.opening = Some(Opening {
            tab: saved.tab,
            app: saved.app.clone(),
            name: saved.name.clone(),
            asked: Instant::now(),
        });
        true
    }

    /// A window of `app` the person opened stands in `tab`'s workspace already, and this restore
    /// did not ask for it: the place the app took when it was opened after the restart, before
    /// the restore asked for the window the person left. A saved tab beside it would never be
    /// filled — the app shows a window, so a launch of it raises that one — and would end saying
    /// the app opened nothing. `None` when every window of the app here is one this restore opened
    /// (the other saved windows are still asked for) or one already standing for another saved tab,
    /// and for a place another device minted: this device opens its own instance of that window
    /// beside whatever it shows here.
    fn opened_by_person(&self, desktop: &ShellDesktop, tab: TabId, app: &str) -> Option<TabId> {
        let workspace = workspace_holding(desktop, tab, Workspace::id)?;
        desktop.with_policy(|policy| {
            policy.handle().read(|tree| {
                if tilo_desktop_model::peer_salt_of(tab) != tree.salt {
                    return None;
                }
                tree.workspaces
                    .iter()
                    .find(|held| held.id() == workspace)
                    .into_iter()
                    .flat_map(Workspace::tabs)
                    .find(|shown| {
                        shown.id != tab
                            && !self.opened.contains(&shown.id)
                            && !self.spoken_for.contains(&shown.id)
                            && matches!(&shown.tile, TileContent::Foreign { app_id, .. }
                                if tilo_desktop_model::apps::app_ids_match(app_id, app)
                                    || tilo_desktop_model::apps::app_ids_match(app, app_id))
                    })
                    .map(|shown| shown.id)
            })
        })
    }

    /// Close `saved` when a window of its app the person opened after the restart stands in its
    /// workspace: the saved tab is the old place of that window, and asking for the app again
    /// would leave a second place the app never fills — it raises the window it has — which would
    /// then say the app opened nothing. Whether the tab went.
    fn close_beside_a_window(&mut self, desktop: &ShellDesktop, saved: &Saved) -> bool {
        let Some(shown) = self.opened_by_person(desktop, saved.tab, &saved.app) else {
            return false;
        };
        self.spoken_for.push(shown);
        desktop.with_policy(|policy| policy.release_tab(saved.tab));
        true
    }

    /// Give each window of an app that stayed running through an in-place restart the saved tab
    /// its keeper was last told it stood in: the tab waits for exactly that window, which comes
    /// once the app is shown again, and the app is not started a second time. The keepers to show
    /// now, by process: an app is shown once the layout holding every tab its keeper named is laid
    /// out, or every workspace's layout was read (and some of them are gone, or none laid out a
    /// window at all: no layout this device could write holds them), or after [`KEPT_WAIT`];
    /// shown before, its windows would take new tabs. A kept window
    /// whose tab is gone (the layout changed on another device meanwhile), or that opened while
    /// the desktop was away, opens as a new window does. A saved tab of a kept app that none of
    /// its windows claims is one the app closed while the desktop was away: it closes too, which
    /// is only known once both the keeper described its windows and the layout is laid out.
    #[cfg(target_os = "linux")]
    fn claim_kept(&mut self, desktop: &ShellDesktop) -> Vec<u32> {
        if self.kept.is_empty() {
            return Vec::new();
        }
        let waited = self
            .kept_since
            .is_some_and(|since| since.elapsed() >= KEPT_WAIT);
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.kept)
            .into_iter()
            .partition(|kept| {
                waited || self.read || (self.seeded && tabs_laid_out(kept, &self.waiting))
            });
        self.kept = waiting;
        let mut shown = Vec::new();
        for kept in ready {
            if kept.outdated {
                self.outdated.push(kept);
                continue;
            }
            let as_found = if self.seeded {
                "in its saved tabs"
            } else if self.read {
                "as new windows, no saved layout holds its tabs"
            } else {
                "as new windows, its saved layout not read"
            };
            tracing::info!(
                target: tilo_startup_metrics::KEPT_APPS_TARGET,
                app = %kept.launch.app,
                keeper = kept.pid,
                windows = kept.windows.len(),
                "kept across restart: shown {as_found}"
            );
            for window in &kept.windows {
                // A window that opened while the desktop was away was told no place.
                let Some(placement) = window.placement.as_deref() else {
                    continue;
                };
                let tab = match placement.parse::<TabId>() {
                    Ok(tab) => tab,
                    Err(error) => {
                        tracing::warn!(app = %kept.launch.app, placement, %error, "a kept window's place names no tab: it opens as a new window");
                        continue;
                    }
                };
                let Some(at) = self.waiting.iter().position(|saved| saved.tab == tab) else {
                    continue;
                };
                let Some(saved) = self.waiting.remove(at) else {
                    continue;
                };
                let window = tilo_desktop_policy::KeptWindow {
                    keeper: kept.pid,
                    tag: window.tag.clone(),
                };
                desktop.with_policy(|policy| {
                    policy.note_in_tab(saved.tab, "");
                    policy.adopt(&saved.app, saved.tab, Some(window));
                });
            }
            shown.push(kept.pid);
            self.kept_apps
                .extend(kept.windows.into_iter().filter_map(|window| window.app_id));
            self.keep_identities(kept.launch);
        }
        self.close_left_by_kept(desktop);
        shown
    }

    /// Note what the windows of the kept launch `launch` announce themselves by, and what it was
    /// asked as ([`Self::kept_apps`]).
    #[cfg(target_os = "linux")]
    fn keep_identities(&mut self, launch: tilo_app_launch::kept::KeptLaunch) {
        self.kept_apps.push(launch.app);
        self.kept_apps.extend(launch.window_identities);
        self.kept_apps.extend(launch.asked);
    }

    /// Close the saved tabs of the kept apps that none of their windows claimed, once the layout
    /// is laid out; before, [`Self::start`] closes them as it lays the layout out.
    #[cfg(target_os = "linux")]
    fn close_left_by_kept(&mut self, desktop: &ShellDesktop) {
        if !self.seeded {
            return;
        }
        let closed: Vec<TabId> = self
            .waiting
            .iter()
            .filter(|saved| self.kept_by(saved))
            .map(|saved| saved.tab)
            .collect();
        self.waiting.retain(|saved| !closed.contains(&saved.tab));
        for tab in closed {
            desktop.with_policy(|policy| policy.release_tab(tab));
        }
    }

    /// Put the saved tab of each kept app whose keeper was killed outright to saying the app
    /// stopped working, with Reopen: the person lost what it held without closing it. It stands
    /// in the first tab of the app, where the person looks first, and the app's other tabs close
    /// as one the app closed would; an app whose saved tab is gone, or whose layout never comes,
    /// gets a tab of its own, as one that ended while the desktop was away. Only once the layout
    /// is laid out is there a tab to put it in.
    #[cfg(target_os = "linux")]
    fn show_stopped(&mut self, desktop: &ShellDesktop) {
        let waited = self
            .kept_since
            .is_some_and(|since| since.elapsed() >= KEPT_WAIT);
        if self.stopped.is_empty() || !(self.seeded || waited) {
            return;
        }
        for launch in std::mem::take(&mut self.stopped) {
            let tabs: Vec<TabId> = self
                .waiting
                .iter()
                .filter(|saved| launch_by(&launch, saved))
                .map(|saved| saved.tab)
                .collect();
            self.waiting.retain(|saved| !tabs.contains(&saved.tab));
            let asked = launch.asked.clone().unwrap_or_else(|| launch.app.clone());
            let title = launch.title.clone();
            let mut tabs = tabs.into_iter();
            let first = tabs.next();
            for tab in tabs {
                desktop.with_policy(|policy| policy.release_tab(tab));
            }
            if let Err(error) =
                desktop.with_policy(|policy| policy.stopped(&asked, &title, Closed::Crashed, first))
            {
                tracing::warn!(%error, app = %asked, "no notice for an app that stopped");
            }
        }
    }

    /// The saved tabs of each app still open from before an update, which this desktop cannot
    /// show: each says so, with Restart, and the app is not started a second time beside the one
    /// that still holds the person's unsaved work. The ones from `shown` on were just taken back,
    /// and one of those that left no saved tab gets a tab of its own.
    #[cfg(target_os = "linux")]
    fn claim_outdated(&mut self, apps: &Apps, shown: usize) {
        let mut fresh = self.outdated.split_off(shown.min(self.outdated.len()));
        self.outdated.retain(|kept| apps.is_launched(kept.id));
        fresh.retain(|kept| apps.is_launched(kept.id));
        let fresh_from = self.outdated.len();
        self.outdated.append(&mut fresh);
        for (index, kept) in self.outdated.iter().enumerate() {
            let first = index >= fresh_from;
            let ids: Vec<&str> = std::iter::once(kept.launch.app.as_str())
                .chain(kept.launch.window_identities.iter().map(String::as_str))
                .collect();
            let (theirs, rest): (VecDeque<Saved>, VecDeque<Saved>) =
                std::mem::take(&mut self.waiting)
                    .into_iter()
                    .partition(|saved| {
                        ids.iter().any(|id| {
                            tilo_desktop_model::apps::app_ids_match(id, &saved.app)
                                || tilo_desktop_model::apps::app_ids_match(&saved.app, id)
                        })
                    });
            self.waiting = rest;
            if first || !theirs.is_empty() {
                let tabs: Vec<(u64, String)> = theirs
                    .into_iter()
                    .map(|saved| (saved.tab, saved.name))
                    .collect();
                apps.show_outdated(kept, &tabs);
            }
        }
    }

    /// Tell the keeper of each window it shows the tab that window stands in, when the layout
    /// changed since the last time: the run after an in-place restart puts the window back there.
    /// A tab keeps its id while it is dragged, so a drag tells nothing until it lands elsewhere.
    pub(crate) fn tell_placements(&mut self, desktop: &ShellDesktop, apps: Option<&Apps>) {
        let Some(apps) = apps else {
            return;
        };
        let revision = desktop.with_policy(|policy| policy.handle().revision());
        if self.placed_at.replace(revision) == Some(revision) {
            return;
        }
        apps.place_kept(&desktop.with_policy(|policy| policy.kept_placements()));
    }

    /// Whether `saved` is a tab of one of the apps kept across the restart this run follows: by
    /// what its windows announce themselves as, or, for a Windows program, by the entry it was
    /// asked as.
    fn kept_by(&self, saved: &Saved) -> bool {
        if let Some(Start::Windows(entry) | Start::Setup(entry)) = &saved.start
            && self.kept_apps.iter().any(|kept| {
                crate::windows_compat::asked_entry(kept).is_some_and(|(asked, _)| asked == entry)
            })
        {
            return true;
        }
        let app = saved.app.as_str();
        self.kept_apps.iter().any(|kept| {
            tilo_desktop_model::apps::app_ids_match(kept, app)
                || tilo_desktop_model::apps::app_ids_match(app, kept)
        })
    }

    /// The held windows whose workspace's host took this device in since: asked for next, in the
    /// order they were held, ahead of the ones not asked for yet.
    fn release_admitted(
        &mut self,
        desktop: &ShellDesktop,
        awaited: &dyn Fn(&str) -> Option<String>,
    ) {
        if self.held.is_empty()
            || self
                .held_checked
                .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        self.held_checked = Some(Instant::now());
        let (admitted, still): (Vec<Saved>, Vec<Saved>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|saved| {
                workspace_holding(desktop, saved.tab, |held| held.id().to_string())
                    .is_none_or(|workspace| awaited(&workspace).is_none())
            });
        self.held = still;
        for saved in admitted.into_iter().rev() {
            self.waiting.push_front(saved);
        }
    }

    /// Ask for `saved` 's window into its own tab. True when an app was asked and its window may
    /// still come.
    fn open(&mut self, desktop: &ShellDesktop, apps: &Apps, saved: Saved) -> bool {
        if ran_once(desktop, &saved) {
            return false;
        }
        let Some(start) = saved.start.clone() else {
            let note = not_installed(&saved.name);
            desktop.with_policy(|policy| policy.note_in_tab(saved.tab, &note));
            return false;
        };
        let Some(workspace) = workspace_holding(desktop, saved.tab, |held| held.id().to_string())
        else {
            // The tab went with the workspace it was in.
            return false;
        };
        // A Windows program is only asked for once the compatibility service said what it can
        // open: until then the tab waits, and the next pass asks again.
        if let Start::Windows(entry) = &start {
            match apps.windows_reopened(entry) {
                Reopen::Waiting => {
                    self.service_pending = true;
                    self.waiting.push_front(saved);
                    return false;
                }
                Reopen::Gone => {
                    let note = not_installed(&saved.name);
                    desktop.with_policy(|policy| policy.note_in_tab(saved.tab, &note));
                    return false;
                }
                Reopen::Ready => {}
            }
        }
        // Whatever the tab said about an earlier start is the past: it waits for its window now.
        // The place is noted before the app starts, as any launch into a tab of its own is: the
        // window takes that exact tab when it maps, in the background, so the window the person had
        // in front stays in front.
        desktop.with_policy(|policy| policy.note_in_tab(saved.tab, ""));
        if !desktop.with_policy(|policy| policy.adopt(&saved.app, saved.tab, None)) {
            return false;
        }
        let started = match &start {
            Start::App(launch) => apps
                .start_beside(
                    launch,
                    &apps.workspace_named(&workspace),
                    Some(saved.tab),
                    Some(saved.shows.as_str()).filter(|shows| !shows.is_empty()),
                )
                .inspect_err(|error| {
                    let said = error.to_string();
                    let said = said
                        .strip_prefix(&format!("{}: ", launch.id()))
                        .unwrap_or(&said);
                    tracing::warn!(
                        app = %saved.app,
                        error = %said,
                        "a window this device left open did not start"
                    );
                }),
            // The service opens it on the same route a manager's launch takes: planned against
            // the workspace's sandbox and kept behind its own display. A refusal reaches the tab
            // on a later pass ([`Self::follow`]).
            // Left by [`ran_once`]: nothing opens a setup again.
            Start::Setup(_) => return false,
            Start::Windows(entry) => {
                let started = apps.start_saved_windows_program(
                    entry,
                    &workspace,
                    saved.tab,
                    &saved.app,
                    &saved.name,
                );
                if let Err(error) = &started {
                    tracing::warn!(
                        app = %saved.app,
                        %error,
                        "a window this device left open did not start"
                    );
                }
                started.map_err(ApiError::Failed)
            }
        };
        match started {
            Ok(()) => {
                self.opened.push(saved.tab);
                self.opening = Some(Opening {
                    tab: saved.tab,
                    app: saved.app,
                    name: saved.name,
                    asked: Instant::now(),
                });
                true
            }
            Err(_error) => {
                desktop.with_policy(|policy| policy.give_up_held(&saved.app));
                let note = would_not_start(&saved.name);
                desktop.with_policy(|policy| policy.note_in_tab(saved.tab, &note));
                false
            }
        }
    }
}

/// Close `saved` when it is the tab of a capsule's setup or uninstaller that ended with the
/// desktop before: such a program is done once it ends, so there is nothing to open in it again,
/// and a tab saying it is not installed would be wrong. True when it was one.
fn ran_once(desktop: &ShellDesktop, saved: &Saved) -> bool {
    if !matches!(saved.start, Some(Start::Setup(_))) {
        return false;
    }
    desktop.with_policy(|policy| policy.release_tab(saved.tab));
    true
}

/// Whether every tab the keeper of `kept` named for its windows is laid out, waiting for a window.
/// A place that names no tab never is: the app waits until every layout was read.
#[cfg(target_os = "linux")]
fn tabs_laid_out(kept: &tilo_app_launch::Adopted, waiting: &VecDeque<Saved>) -> bool {
    kept.windows
        .iter()
        .filter_map(|window| window.placement.as_deref())
        .all(|placement| {
            waiting
                .iter()
                .any(|saved| saved.tab.to_string() == placement)
        })
}

/// `tree` without the places a launch of this device's left on it that no window came for.
/// A place stands for a launch the session asked for a moment ago. The session that asked is gone
/// by the next start, so nothing is on its way to those any more: they are not windows the person
/// had open, and a start that kept them would show the remains of a launch that ended — one more of
/// them with every restart, since each is written down with the layout.
///
/// Only this device's own places are its earlier session's: a place another device minted is that
/// device's launch, under way while this one starts, and the workspace's layout holds it until its
/// window comes. Closing it here would be a close this device writes, which takes the window away
/// from the device that opened it. A place a launch of this session holds (`under_way`) is the same:
/// the person opened the app before the layout was first laid out, and its window takes that place.
pub(crate) fn without_unfilled(tree: &mut Layout, under_way: &dyn Fn(TabId) -> bool) {
    let salt = tree.salt;
    let unfilled: Vec<TabId> = tree
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .filter(|tab| matches!(tab.tile, TileContent::AppPlaceholder { .. }))
        .filter(|tab| tilo_desktop_model::peer_salt_of(tab.id) == salt)
        .map(|tab| tab.id)
        .filter(|tab| !under_way(*tab))
        .collect();
    for tab in unfilled {
        tree.close_tab(tab);
    }
}

/// What `of` reads of the workspace the tab `tab` stands in: its id, which the tab's app is started
/// into with that workspace's grants.
fn workspace_holding<T>(
    desktop: &ShellDesktop,
    tab: TabId,
    of: impl Fn(&tilo_desktop_model::Workspace) -> T,
) -> Option<T> {
    desktop.with_policy(|policy| {
        policy.handle().read(|tree| {
            tree.workspaces
                .iter()
                .find(|workspace| {
                    workspace
                        .strips
                        .iter()
                        .flat_map(|strip| &strip.window_groups)
                        .any(|group| group.tabs.iter().any(|candidate| candidate.id == tab))
                })
                .map(of)
        })
    })
}

/// Whether the tabs `a` and `b` stand in the same workspace.
fn same_workspace(desktop: &ShellDesktop, a: TabId, b: TabId) -> bool {
    let held = |tab: TabId| workspace_holding(desktop, tab, tilo_desktop_model::Workspace::id);
    matches!((held(a), held(b)), (Some(a), Some(b)) if a == b)
}

/// The windows of the workspaces of `tree` that `wanted` names, each waiting for its app to open
/// it, by the workspace it stands in. A window that showed an item opens again on it, from
/// `items`: the item is not the tab's view, which is what a place shows and is no window.
fn waiting_in(
    tree: &Layout,
    items: &std::collections::BTreeMap<TabId, String>,
    wanted: impl Fn(WorkspaceId) -> bool,
) -> Vec<(WorkspaceId, Saved)> {
    let mut saved = Vec::new();
    for workspace in tree
        .workspaces
        .iter()
        .filter(|workspace| wanted(workspace.id()))
    {
        for tab in workspace.tabs() {
            let TileContent::LocalApp { app_id, label, .. } = &tab.tile else {
                continue;
            };
            // A tab that shows one thing of the app is the person's place of that thing, not a
            // window waiting to be opened again.
            if tilo_desktop_model::apps::shows_a_view(tab) {
                continue;
            }
            // A window that announced no app id (an X11 client without a class, `vkcube`) is
            // saved under none: nothing here can start it again, but its window, kept across an
            // in-place restart, still claims this tab by its id.
            let unnamed = app_id.trim().is_empty();
            let name = if label.trim().is_empty() {
                app_id.clone()
            } else {
                label.clone()
            };
            saved.push((
                workspace.id(),
                Saved {
                    tab: tab.id,
                    app: app_id.clone(),
                    name,
                    start: if unnamed {
                        None
                    } else {
                        apps::launch_of(app_id).map(Start::App).or_else(|| {
                            crate::windows_compat::saved_entry(&workspace.id().to_string(), app_id)
                                .map(|saved| {
                                    if saved.setup {
                                        Start::Setup(saved.entry)
                                    } else {
                                        Start::Windows(saved.entry)
                                    }
                                })
                        })
                    },
                    shows: items.get(&tab.id).cloned().unwrap_or_default(),
                },
            ));
        }
    }
    saved
}

/// Whether `saved` is a tab of one of the windows of the kept launch `launch`: by what its
/// windows announce themselves as, or, for a Windows program, by the entry it was asked as.
#[cfg(target_os = "linux")]
fn launch_by(launch: &tilo_app_launch::kept::KeptLaunch, saved: &Saved) -> bool {
    if let Some(Start::Windows(entry) | Start::Setup(entry)) = &saved.start
        && launch
            .asked
            .as_deref()
            .and_then(crate::windows_compat::asked_entry)
            .is_some_and(|(asked, _)| asked == entry)
    {
        return true;
    }
    let app = saved.app.as_str();
    std::iter::once(launch.app.as_str())
        .chain(launch.window_identities.iter().map(String::as_str))
        .any(|id| {
            tilo_desktop_model::apps::app_ids_match(id, app)
                || tilo_desktop_model::apps::app_ids_match(app, id)
        })
}

/// What a tab says when the app it holds is not on this machine.
fn not_installed(name: &str) -> String {
    t_args("shell-window-not-installed", &[("app", name.into())])
}

/// What a tab says when its app started nothing.
fn would_not_start(name: &str) -> String {
    t_args("shell-window-would-not-start", &[("app", name.into())])
}

/// What a tab says while its app waits for `host` to take this device into its workspace.
fn waits_for_host(name: &str, host: &str) -> String {
    t_args(
        "shell-window-waits-for-host",
        &[("app", name.into()), ("host", host.into())],
    )
}

/// What a tab says when its app started and never showed a window.
fn did_not_open(name: &str) -> String {
    t_args("shell-window-did-not-open", &[("app", name.into())])
}

#[cfg(test)]
mod tests {
    use tilo_desktop_model::{
        Layout, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth, Workspace,
        WorkspaceId,
    };

    use super::{Restore, Saved, Start};

    /// A layout of one workspace whose one window group holds `tabs`.
    fn layout(tabs: Vec<Tab>) -> Layout {
        let mut workspace = Workspace::new(
            WorkspaceId::parse("3024f582-9315-461d-a32f-2c67269385e1").unwrap(),
            "Shared",
        );
        let mut strip = Strip::new(StripId(2), "Main");
        strip.window_groups.push(WindowGroup {
            id: 3,
            active_tab: tabs.first().map_or(0, |tab| tab.id),
            tabs,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        workspace.strips.push(strip);
        Layout {
            active: WorkspaceId::from_u128(1),
            workspaces: vec![workspace],
            ..Layout::default()
        }
    }

    fn window(id: u64, tile: TileContent) -> Tab {
        Tab {
            id,
            title: "Notes".into(),
            tile,
            app_title: None,
            view: String::new(),
            pinned: false,
        }
    }

    fn notes() -> TileContent {
        TileContent::LocalApp {
            app_id: "so.tilo.notes".into(),
            label: "Notes".into(),
            note: String::new(),
        }
    }

    /// The place a launch of this device's earlier session left goes at the start, and the place
    /// another device holds for its own launch, under way as this device starts, stays.
    #[test]
    fn a_start_drops_its_own_unfilled_places_and_keeps_another_devices() {
        let place = || TileContent::AppPlaceholder {
            kind: tilo_desktop_model::AppKind::Other("so.tilo.chat".into()),
            label: "so.tilo.chat".into(),
        };
        let own = tilo_desktop_model::compose_id(7, 4);
        let theirs = tilo_desktop_model::compose_id(8, 5);
        let mut tree = layout(vec![window(own, place()), window(theirs, place())]);
        tree.salt = 7;
        super::without_unfilled(&mut tree, &|_| false);
        let left: Vec<u64> = tree
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.tabs().map(|tab| tab.id))
            .collect();
        assert_eq!(left, vec![theirs]);
    }

    /// A window another member opened arrives in the workspace's layout as one this device opens
    /// its own instance of: the app is asked for it, and for no window that was already here.
    #[test]
    fn a_window_another_member_opened_is_asked_for_and_nothing_already_here() {
        let mine = window(
            4,
            TileContent::Foreign {
                app_id: "so.tilo.notes".into(),
                surface_id: 9,
            },
        );
        let waiting = window(5, notes());
        let before = layout(vec![mine.clone(), waiting.clone()]);
        let after = layout(vec![mine, waiting, window(6, notes())]);
        let mut restore = Restore::default();
        restore.arrived(&before, &after, &std::collections::BTreeMap::new());
        let asked: Vec<u64> = restore.waiting.iter().map(|saved| saved.tab).collect();
        assert_eq!(asked, [6]);
    }

    /// A window whose app says it runs only on the device that opened it (a Browser's page lives
    /// in its engine there) reaches another layout as that device's window. Nothing is asked to
    /// open it here, so its tab never says an app could not open. A window that shows a record,
    /// written beside it, is asked for as before.
    #[test]
    fn a_window_bound_to_the_device_that_opened_it_is_not_asked_for_on_another() {
        use tilo_desktop_model::shared_resource::device;

        const NOTES: u64 = 4001;
        const BROWSER: u64 = 4002;
        let laptop = "ab".repeat(32);
        let foreign = |app: &str, surface_id| TileContent::Foreign {
            app_id: app.into(),
            surface_id,
        };
        // The laptop's layout, with both windows live, as its session writes it into the
        // workspace's layout.
        let opened = layout(vec![
            window(NOTES, foreign("so.tilo.notes", 1)),
            window(BROWSER, foreign("so.tilo.browser", 2)),
        ]);
        let room = opened.workspaces[0].id();
        let published = device::published(
            opened.workspaces[0].clone(),
            &device::Host {
                device: &laptop,
                name: "Laptop",
                runs_here: &|app| app == "so.tilo.browser",
            },
            &|_| true,
        );
        let mut doc = automerge::AutoCommit::new();
        device::write(
            &mut doc,
            room,
            &published,
            &device::Writer {
                person: Some("ada"),
                mine: &|_| true,
                records: device::Writer::NOTHING_NOTED,
                items: device::Writer::NOTHING_ITEMED,
            },
        )
        .unwrap();
        doc.commit();

        // Another device of the workspace lays that layout into its own layout.
        let mut before = layout(Vec::new());
        before.workspaces[0].strips.clear();
        let mut after = before.clone();
        assert!(device::fold(
            &doc,
            &mut after.workspaces[0],
            &"cd".repeat(32),
            &device::Kept::NOTHING,
        ));
        let mut restore = Restore::default();
        restore.arrived(&before, &after, &std::collections::BTreeMap::new());
        let asked: Vec<(u64, &str)> = restore
            .waiting
            .iter()
            .map(|saved| (saved.tab, saved.app.as_str()))
            .collect();
        assert_eq!(
            asked,
            [(NOTES, "so.tilo.notes")],
            "only the window that shows a record opens here"
        );
        let browser = after.workspaces[0]
            .tabs()
            .find(|tab| tab.id == BROWSER)
            .expect("the layout lists the other device's window");
        assert_eq!(
            browser.tile,
            TileContent::HostedWindow {
                app_id: "so.tilo.browser".into(),
                label: "Notes".into(),
                host: laptop,
                host_label: "Laptop".into(),
            },
            "as the window that runs on the device that opened it"
        );
    }

    /// The kept app `org.example.Kept`, its keeper process 50, with windows tagged and placed as
    /// `windows` says.
    #[cfg(target_os = "linux")]
    fn kept(windows: &[(&str, Option<&str>)]) -> tilo_app_launch::Adopted {
        use tilo_app_launch::kept::{KeptLaunch, LAUNCH_VERSION, Window};
        tilo_app_launch::Adopted {
            outdated: false,
            id: 1,
            pid: 50,
            launch: KeptLaunch {
                version: LAUNCH_VERSION,
                app: "org.example.Kept".into(),
                title: "Kept".into(),
                workspace: "workspace-1".into(),
                asked: None,
                window_identities: vec!["org.example.Kept".into()],
                service: false,
            },
            windows: windows
                .iter()
                .map(|(tag, placement)| Window {
                    tag: (*tag).into(),
                    app_id: Some("org.example.Kept".into()),
                    title: None,
                    placement: placement.map(str::to_owned),
                })
                .collect(),
        }
    }

    /// The saved layout of a kept app's two windows, each in a workspace of its own, and a Notes
    /// window beside the first: tabs 41 and 43 in the first workspace, 42 in the second.
    #[cfg(target_os = "linux")]
    fn kept_layout() -> Layout {
        let kept_app = || TileContent::LocalApp {
            app_id: "org.example.Kept".into(),
            label: "Kept".into(),
            note: String::new(),
        };
        let mut tree = layout(vec![window(41, kept_app()), window(43, notes())]);
        let mut second = Workspace::new(WorkspaceId::from_u128(9), "Second");
        second.strips = layout(vec![window(42, kept_app())]).workspaces[0]
            .strips
            .clone();
        tree.workspaces.push(second);
        // A window with no tab held for it lands in the workspace in front.
        tree.active = tree.workspaces[0].id();
        tree
    }

    /// The tab window `id` of the kept app, tagged `tag`, stands in once it maps.
    #[cfg(target_os = "linux")]
    fn map(desktop: &crate::platform::surfaces::ShellDesktop, id: u64, tag: &str) -> Option<u64> {
        desktop.with_policy(|policy| {
            policy
                .map(tilo_desktop_policy::Window {
                    id,
                    app_id: Some("org.example.Kept".into()),
                    pid: Some(50),
                    tag: Some(tag.into()),
                    ..tilo_desktop_policy::Window::default()
                })
                .expect("mapped");
            policy.tab_of(id)
        })
    }

    /// Every tab the desktop holds, in order.
    #[cfg(target_os = "linux")]
    fn tabs(desktop: &crate::platform::surfaces::ShellDesktop) -> Vec<u64> {
        desktop.with_policy(|policy| {
            policy.handle().read(|tree| {
                tree.workspaces
                    .iter()
                    .flat_map(|workspace| workspace.tabs().map(|tab| tab.id))
                    .collect()
            })
        })
    }

    /// After an in-place restart each window of a kept app claims exactly the saved tab its
    /// keeper was told it stood in, in whichever workspace that is. A window whose tab is gone, or
    /// that opened while the desktop was away, claims none and opens as a new window does; a
    /// saved tab of the app that no window claims was closed while the desktop was away and
    /// closes, so the app, still running, is not started a second time. Another app's saved tab
    /// still waits for its app.
    #[test]
    #[cfg(target_os = "linux")]
    fn each_kept_window_claims_its_own_saved_tab_and_a_closed_ones_tab_goes() {
        let mut tree = kept_layout();
        let (desktop, _policy) = crate::platform::surfaces::desktop(7, 1.0);
        desktop.with_widgets(|widgets| {
            widgets.set_placement(tilo_session_core::surface::Placement {
                origin: (0, 0),
                size: (1200, 800),
                scale: 1.0,
            });
        });
        let mut restore = Restore::default();
        restore.begin();
        assert!(restore.start(
            &Layout::default(),
            &mut tree,
            &|_| false,
            &std::collections::BTreeMap::new()
        ));
        desktop.with_policy(|policy| policy.handle().replace_tree(tree.clone()));
        restore.kept = vec![kept(&[
            ("w2", Some("999")),
            ("w1", Some("41")),
            ("w3", None),
        ])];
        restore.kept_since = Some(std::time::Instant::now());
        assert!(
            restore.claim_kept(&desktop).is_empty(),
            "a tab is not laid out, and a workspace may still bring it"
        );
        restore.layouts_read();
        assert_eq!(
            restore.claim_kept(&desktop),
            [50],
            "every layout was read: it shows"
        );
        let waiting: Vec<u64> = restore.waiting.iter().map(|saved| saved.tab).collect();
        assert_eq!(waiting, [43], "only another app's tab still waits");
        assert_eq!(
            tabs(&desktop),
            [41, 43],
            "the tab of the window closed meanwhile goes"
        );
        assert_ne!(
            map(&desktop, 2, "w2"),
            Some(41),
            "a window whose tab is gone takes a new one"
        );
        assert_eq!(
            map(&desktop, 1, "w1"),
            Some(41),
            "the window takes its own tab"
        );
    }

    /// A kept app whose window announces no app id (an X11 client without a class, `vkcube`) was
    /// saved in a tab that names no app. After an in-place restart that window still takes its own
    /// tab, by the tab its keeper was told, instead of opening a new tab beside an empty one.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_kept_window_that_names_no_app_takes_its_own_tab() {
        let unnamed = TileContent::LocalApp {
            app_id: String::new(),
            label: "vkcube".into(),
            note: String::new(),
        };
        let mut tree = layout(vec![window(44, unnamed), window(43, notes())]);
        tree.active = tree.workspaces[0].id();
        let (desktop, _policy) = crate::platform::surfaces::desktop(7, 1.0);
        desktop.with_widgets(|widgets| {
            widgets.set_placement(tilo_session_core::surface::Placement {
                origin: (0, 0),
                size: (1200, 800),
                scale: 1.0,
            });
        });
        let mut restore = Restore::default();
        restore.begin();
        assert!(restore.start(
            &Layout::default(),
            &mut tree,
            &|_| false,
            &std::collections::BTreeMap::new()
        ));
        desktop.with_policy(|policy| policy.handle().replace_tree(tree.clone()));
        let mut cube = kept(&[("w1", Some("44"))]);
        cube.windows[0].app_id = None;
        restore.kept = vec![cube];
        restore.kept_since = Some(std::time::Instant::now());
        assert_eq!(
            restore.claim_kept(&desktop),
            [50],
            "its tab is laid out: it shows without waiting for every layout"
        );
        let waiting: Vec<u64> = restore.waiting.iter().map(|saved| saved.tab).collect();
        assert_eq!(waiting, [43], "only another app's tab still waits");
        let tab = desktop.with_policy(|policy| {
            policy
                .map(tilo_desktop_policy::Window {
                    id: 1,
                    app_id: None,
                    pid: Some(50),
                    tag: Some("w1".into()),
                    ..tilo_desktop_policy::Window::default()
                })
                .expect("mapped");
            policy.tab_of(1)
        });
        assert_eq!(tab, Some(44), "the window takes its own tab");
        assert_eq!(tabs(&desktop), [44, 43], "and no other tab opened");
    }

    /// On a real login the keepers answer before the workspaces' layouts are read. Their windows
    /// wait unshown until the layout holding their tabs is laid out, and then each takes its own
    /// tab in its own workspace: none of them opens as a new tab, and none of their saved tabs is
    /// taken for one the app closed meanwhile. No app is started for a saved tab while they wait.
    #[test]
    #[cfg(target_os = "linux")]
    fn kept_windows_that_answer_before_the_layout_wait_for_it_and_take_their_tabs() {
        let (desktop, _policy) = crate::platform::surfaces::desktop(7, 1.0);
        desktop.with_widgets(|widgets| {
            widgets.set_placement(tilo_session_core::surface::Placement {
                origin: (0, 0),
                size: (1200, 800),
                scale: 1.0,
            });
        });
        let mut restore = Restore::default();
        restore.begin();
        restore.kept = vec![kept(&[("w1", Some("41")), ("w2", Some("42"))])];
        restore.kept_since = Some(std::time::Instant::now());
        assert!(
            restore.claim_kept(&desktop).is_empty(),
            "no layout yet: nothing shows"
        );
        assert_eq!(restore.kept.len(), 1, "the app still waits");
        assert!(restore.next_deadline().is_some(), "for a bounded time");

        let mut tree = kept_layout();
        assert!(restore.start(
            &Layout::default(),
            &mut tree,
            &|_| false,
            &std::collections::BTreeMap::new()
        ));
        desktop.with_policy(|policy| policy.handle().replace_tree(tree.clone()));
        assert_eq!(
            tabs(&desktop),
            [41, 43, 42],
            "no saved tab was closed for it"
        );
        assert_eq!(
            restore.claim_kept(&desktop),
            [50],
            "the layout came: it shows"
        );
        let waiting: Vec<u64> = restore.waiting.iter().map(|saved| saved.tab).collect();
        assert_eq!(waiting, [43]);
        assert_eq!(
            map(&desktop, 1, "w1"),
            Some(41),
            "the first window takes its tab"
        );
        assert_eq!(
            map(&desktop, 2, "w2"),
            Some(42),
            "the second takes its own, in the other workspace"
        );
        assert_eq!(tabs(&desktop), [41, 43, 42], "and no other tab opened");
    }

    /// A layout that is laid out before it, but holds only some of a kept app's tabs (the other
    /// workspace's is still being read), keeps the app waiting; a layout that never brings them
    /// shows it after the wait, as new windows, and the saved tabs of it that come later close
    /// rather than start the app a second time. A session that reads no layout at all, or read
    /// every layout and found none of its tabs, shows a kept app at once.
    #[test]
    #[cfg(target_os = "linux")]
    fn kept_windows_stop_waiting_for_a_layout_that_does_not_come() {
        let (desktop, _policy) = crate::platform::surfaces::desktop(7, 1.0);
        let mut partly = Restore::default();
        partly.begin();
        let mut first = kept_layout();
        first.workspaces.truncate(1);
        assert!(partly.start(
            &Layout::default(),
            &mut first,
            &|_| false,
            &std::collections::BTreeMap::new()
        ));
        partly.kept = vec![kept(&[("w1", Some("41")), ("w2", Some("42"))])];
        partly.kept_since = Some(std::time::Instant::now());
        assert!(
            partly.claim_kept(&desktop).is_empty(),
            "one tab is not laid out yet"
        );

        let mut late = Restore::default();
        late.begin();
        late.kept = vec![kept(&[("w1", Some("41")), ("w2", Some("42"))])];
        late.kept_since = std::time::Instant::now().checked_sub(super::KEPT_WAIT);
        assert_eq!(late.claim_kept(&desktop), [50], "it waited long enough");
        let mut tree = kept_layout();
        assert!(late.start(
            &Layout::default(),
            &mut tree,
            &|_| false,
            &std::collections::BTreeMap::new()
        ));
        let saved: Vec<u64> = tree
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.tabs().map(|tab| tab.id))
            .collect();
        assert_eq!(
            saved,
            [43],
            "its saved tabs close: its windows are shown already"
        );

        // Every layout was read and none laid a window out (none this device could write holds
        // them): there is nothing to wait for.
        let mut unsaved = Restore::default();
        unsaved.begin();
        unsaved.layouts_read();
        unsaved.kept = vec![kept(&[("w1", Some("41"))])];
        unsaved.kept_since = Some(std::time::Instant::now());
        assert_eq!(
            unsaved.claim_kept(&desktop),
            [50],
            "no saved tab of it to wait for"
        );

        let mut alone = Restore::default();
        alone.begin();
        alone.without_layouts();
        alone.kept = vec![kept(&[("w1", Some("41"))])];
        alone.kept_since = Some(std::time::Instant::now());
        assert_eq!(alone.claim_kept(&desktop), [50], "nothing to wait for");
    }

    /// A capsule's setup that ended with the desktop before leaves a saved tab its window stood
    /// in: it ran once, so its tab closes rather than saying the setup is not installed. A
    /// program's tab still waits to be opened again.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_setups_saved_tab_closes_and_a_programs_tab_stays() {
        let windows = |app: &str| TileContent::LocalApp {
            app_id: app.into(),
            label: app.into(),
            note: String::new(),
        };
        let tree = layout(vec![
            window(41, windows("putty-setup.exe")),
            window(42, windows("putty.exe")),
        ]);
        let (desktop, _policy) = crate::platform::surfaces::desktop(7, 1.0);
        desktop.with_policy(|policy| policy.handle().replace_tree(tree));
        let saved = |tab: TabId, app: &str, start: Start| Saved {
            tab,
            app: app.into(),
            name: app.into(),
            start: Some(start),
            shows: String::new(),
        };
        assert!(super::ran_once(
            &desktop,
            &saved(41, "putty-setup.exe", Start::Setup("windows-1".into()))
        ));
        assert!(!super::ran_once(
            &desktop,
            &saved(42, "putty.exe", Start::Windows("windows-2".into()))
        ));
        assert_eq!(
            tabs(&desktop),
            [42],
            "the setup's tab goes, the program's stays"
        );
    }

    /// The Terminal app whose saved window the person opens again.
    #[cfg(target_os = "linux")]
    const TERMINAL: &str = "so.tilo.terminal";

    /// `tree` with a saved Terminal window, the restore started on it with that window waiting,
    /// and the desktop showing it.
    #[cfg(target_os = "linux")]
    fn saved_terminal() -> (crate::platform::surfaces::ShellDesktop, Restore) {
        let tab = tilo_desktop_model::compose_id(7, 41);
        let saved = window(tab, saved_terminal_tile());
        let mut tree = layout(vec![saved]);
        tree.active = tree.workspaces[0].id();
        let (desktop, _policy) = crate::platform::surfaces::desktop(7, 1.0);
        desktop.with_widgets(|widgets| {
            widgets.set_placement(tilo_session_core::surface::Placement {
                origin: (0, 0),
                size: (1200, 800),
                scale: 1.0,
            });
        });
        let mut restore = Restore::default();
        restore.begin();
        assert!(restore.start(
            &Layout::default(),
            &mut tree,
            &|_| false,
            &std::collections::BTreeMap::new()
        ));
        desktop.with_policy(|policy| policy.handle().replace_tree(tree));
        assert_eq!(
            restore
                .waiting
                .iter()
                .map(|saved| saved.tab)
                .collect::<Vec<_>>(),
            [tab],
            "the saved window waits for its app"
        );
        (desktop, restore)
    }

    /// The app's window as the person's own open mapped it: a tab of its own beside the saved
    /// one, holding no note.
    #[cfg(target_os = "linux")]
    fn opened_terminal(desktop: &crate::platform::surfaces::ShellDesktop) -> u64 {
        desktop.with_policy(|policy| {
            policy
                .map(tilo_desktop_policy::Window {
                    id: 9,
                    app_id: Some(TERMINAL.into()),
                    pid: Some(900),
                    ..tilo_desktop_policy::Window::default()
                })
                .expect("mapped");
        });
        desktop.with_widgets(crate::platform::surfaces::WidgetSurface::relayout);
        desktop
            .with_policy(|policy| policy.tab_of(9))
            .expect("the window has a tab")
    }

    /// A Terminal tab saved when the desktop went away, its app not yet asked for.
    #[cfg(target_os = "linux")]
    fn saved_terminal_tile() -> TileContent {
        TileContent::LocalApp {
            app_id: TERMINAL.into(),
            label: "Terminal".into(),
            note: String::new(),
        }
    }

    /// The person opened Terminal after the restart, and its window took a tab of its own: the
    /// saved tab is the old place of that window, so the restore closes it instead of asking the
    /// app for a second window that never comes.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_saved_tab_beside_the_window_the_person_opened_asks_for_nothing() {
        let (desktop, mut restore) = saved_terminal();
        let shown = opened_terminal(&desktop);
        let apps = crate::apps::Apps::with_launcher(
            tilo_app_launch::SessionEnvironment::default(),
            None,
            None,
        );
        restore.follow(&desktop, Some(&apps), &|_| None);
        assert_eq!(
            tabs(&desktop),
            [shown],
            "the app's window alone: no place waits beside it"
        );
        assert_eq!(restore.waiting.len(), 0, "the saved tab is settled");
    }

    /// The restore asked for the saved window, and the app answered by raising the window the
    /// person opened: no window comes into the saved tab, which goes rather than telling the
    /// person the app opened nothing while its window stands beside the tab.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_saved_tab_whose_app_opened_elsewhere_does_not_say_it_did_not_open() {
        let (desktop, mut restore) = saved_terminal();
        let shown = opened_terminal(&desktop);
        let tab = tilo_desktop_model::compose_id(7, 41);
        restore.opened = vec![tab];
        restore.opening = Some(super::Opening {
            tab,
            app: TERMINAL.into(),
            name: "Terminal".into(),
            asked: std::time::Instant::now()
                .checked_sub(super::OPEN_DEADLINE)
                .expect("the clock reads past the deadline"),
        });
        let apps = crate::apps::Apps::with_launcher(
            tilo_app_launch::SessionEnvironment::default(),
            None,
            None,
        );
        restore.follow(&desktop, Some(&apps), &|_| None);
        assert_eq!(
            tabs(&desktop),
            [shown],
            "no tab is left saying the app opened nothing"
        );
    }
}
