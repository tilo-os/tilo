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
use tilo_desktop_model::{Layout, TabId, TileContent, WorkspaceId};

use crate::apps::{self, Apps};
use crate::i18n::t_args;
use crate::windows_compat::Reopen;

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
    /// What the windows of those apps announce themselves by: a saved tab of one of them that no
    /// window of theirs claimed is a window the app closed while the desktop was away, and is not
    /// asked for again, which would start a second instance of an app that still runs.
    kept_apps: Vec<String>,
    /// The revision of the layout the kept windows' keepers were last told their tabs at.
    placed_at: Option<u64>,
    /// The apps still running from before an update that this desktop cannot show, while they
    /// run: a saved tab of theirs, whenever it comes, says so rather than starting a second copy.
    #[cfg(target_os = "linux")]
    outdated: Vec<tilo_app_launch::Adopted>,
    /// A saved tab waited since the last pass for the Windows compatibility service to read what
    /// it can open: another pass is due at once rather than on the next frame.
    service_pending: bool,
}

/// How a saved window's app starts again: a Tilo package or a machine application, or the
/// Windows program of the class the window showed under.
#[derive(Clone)]
enum Start {
    App(Launch),
    /// The entry the compatibility service opens, as the session saw its window here.
    Windows(String),
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
    /// The item its window opened on, as its tab's view keeps it on every device.
    shows: String,
}

/// The app asked for a saved tab, and when.
struct Opening {
    tab: TabId,
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
    pub(crate) fn start(&mut self, before: &Layout, tree: &mut Layout) -> bool {
        if self.seeded {
            return false;
        }
        self.seeded = true;
        without_unfilled(tree);
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
        let mut saved: Vec<(WorkspaceId, Saved)> = waiting_in(tree, |_| true)
            .into_iter()
            .filter(|(_, saved)| !here.contains(&saved.tab))
            .collect();
        // Laid out after the kept apps claimed their tabs: what is left of theirs closed meanwhile.
        saved.retain(|(_, saved)| {
            let closed = self.kept_by_app(&saved.app);
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
    pub(crate) fn add(&mut self, tree: &Layout, workspace_id: WorkspaceId) {
        self.waiting.extend(
            waiting_in(tree, |workspace| workspace == workspace_id)
                .into_iter()
                .map(|(_, saved)| saved),
        );
    }

    /// Ask, after what is waiting already, for the windows `after` waits for that `before` did
    /// not: the windows someone else in a workspace opened, which this device opens its own
    /// instance of, each in the tab the workspace put it in.
    pub(crate) fn arrived(&mut self, before: &Layout, after: &Layout) {
        let known: std::collections::HashSet<TabId> = waiting_in(before, |_| true)
            .into_iter()
            .map(|(_, saved)| saved.tab)
            .collect();
        self.waiting.extend(
            waiting_in(after, |_| true)
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
            .filter(|_| !self.kept.is_empty())
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
                self.kept = apps.take_kept();
            }
        }
        #[cfg(target_os = "linux")]
        {
            let shown = self.outdated.len();
            for keeper in self.claim_kept(desktop) {
                apps.attach_kept(keeper);
            }
            self.claim_outdated(apps, shown);
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
            if let Some(opening) = &self.opening {
                let waiting = desktop.with_policy(|policy| policy.waits_for_window(opening.tab));
                if waiting && opening.asked.elapsed() < OPEN_DEADLINE {
                    return;
                }
                if waiting {
                    tracing::warn!(
                        app = %opening.name,
                        "a window this device left open did not come"
                    );
                    let note = did_not_open(&opening.name);
                    desktop.with_policy(|policy| policy.note_in_tab(opening.tab, &note));
                }
                self.opening = None;
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
            let host = workspace_holding(desktop, saved.tab, |held| held.id().to_string())
                .and_then(|workspace| awaited(&workspace));
            if let Some(host) = host {
                let note = waits_for_host(&saved.name, &host);
                desktop.with_policy(|policy| policy.note_in_tab(saved.tab, &note));
                self.held.push(saved);
                continue;
            }
            if self.open(desktop, apps, saved) {
                return;
            }
        }
    }

    /// Give each window of an app that stayed running through an in-place restart the saved tab
    /// its keeper was last told it stood in: the tab waits for exactly that window, which comes
    /// once the app is shown again, and the app is not started a second time. The keepers to show
    /// now, by process: an app is shown once the layout holding every tab its keeper named is laid
    /// out, or every workspace's layout was read and some of them are gone, or after
    /// [`KEPT_WAIT`]; shown before, its windows would take new tabs. A kept window
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
                waited || (self.seeded && (self.read || tabs_laid_out(kept, &self.waiting)))
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
            self.kept_apps.push(kept.launch.app.clone());
            self.kept_apps.extend(kept.launch.window_identities);
            self.kept_apps
                .extend(kept.windows.into_iter().filter_map(|window| window.app_id));
        }
        // Before the layout is laid out, what of a kept app's is left of it closes as it does
        // ([`Self::start`]).
        if self.seeded {
            let closed: Vec<TabId> = self
                .waiting
                .iter()
                .filter(|saved| self.kept_by_app(&saved.app))
                .map(|saved| saved.tab)
                .collect();
            self.waiting.retain(|saved| !closed.contains(&saved.tab));
            for tab in closed {
                desktop.with_policy(|policy| policy.release_tab(tab));
            }
        }
        shown
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

    /// Whether `app` is one of the apps kept across the restart this run follows.
    fn kept_by_app(&self, app: &str) -> bool {
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
                self.opening = Some(Opening {
                    tab: saved.tab,
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

/// `tree` without the places a launch left on it that no window came for.
/// A place stands for a launch the session asked for a moment ago. The session that asked is gone
/// by the next start, so nothing is on its way to those any more: they are not windows the person
/// had open, and a start that kept them would show the remains of a launch that ended — one more of
/// them with every restart, since each is written down with the layout.
pub(crate) fn without_unfilled(tree: &mut Layout) {
    let unfilled: Vec<TabId> = tree
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .filter(|tab| matches!(tab.tile, TileContent::AppPlaceholder { .. }))
        .map(|tab| tab.id)
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

/// The windows of the workspaces of `tree` that `wanted` names, each waiting for its app to open
/// it, by the workspace it stands in.
fn waiting_in(tree: &Layout, wanted: impl Fn(WorkspaceId) -> bool) -> Vec<(WorkspaceId, Saved)> {
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
            if app_id.trim().is_empty() {
                continue;
            }
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
                    start: apps::launch_of(app_id).map(Start::App).or_else(|| {
                        crate::windows_compat::saved_entry(&workspace.id().to_string(), app_id)
                            .map(Start::Windows)
                    }),
                    shows: tab.view.clone(),
                },
            ));
        }
    }
    saved
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
        Layout, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
    };

    use super::Restore;

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
        restore.arrived(&before, &after);
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
        restore.arrived(&before, &after);
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
        let (desktop, _policy) = tilo_shell_surface::desktop(7, 1.0);
        let mut restore = Restore::default();
        restore.begin();
        assert!(restore.start(&Layout::default(), &mut tree));
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

    /// On a real login the keepers answer before the workspaces' layouts are read. Their windows
    /// wait unshown until the layout holding their tabs is laid out, and then each takes its own
    /// tab in its own workspace: none of them opens as a new tab, and none of their saved tabs is
    /// taken for one the app closed meanwhile. No app is started for a saved tab while they wait.
    #[test]
    #[cfg(target_os = "linux")]
    fn kept_windows_that_answer_before_the_layout_wait_for_it_and_take_their_tabs() {
        let (desktop, _policy) = tilo_shell_surface::desktop(7, 1.0);
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
        assert!(restore.start(&Layout::default(), &mut tree));
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
    /// rather than start the app a second time. A session that reads no layout at all shows a
    /// kept app at once.
    #[test]
    #[cfg(target_os = "linux")]
    fn kept_windows_stop_waiting_for_a_layout_that_does_not_come() {
        let (desktop, _policy) = tilo_shell_surface::desktop(7, 1.0);
        let mut partly = Restore::default();
        partly.begin();
        let mut first = kept_layout();
        first.workspaces.truncate(1);
        assert!(partly.start(&Layout::default(), &mut first));
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
        assert!(late.start(&Layout::default(), &mut tree));
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

        let mut alone = Restore::default();
        alone.begin();
        alone.without_layouts();
        alone.kept = vec![kept(&[("w1", Some("41"))])];
        alone.kept_since = Some(std::time::Instant::now());
        assert_eq!(alone.claim_kept(&desktop), [50], "nothing to wait for");
    }
}
