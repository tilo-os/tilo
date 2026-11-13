//! The windows this device left open come back: each saved app is asked for its window again, and
//! the window takes the tab the person left it in.
//! A saved window is kept as the app that opens it ([`tilo_desktop_model::TileContent::LocalApp`]),
//! because a live window's surface belongs to the compositor that mapped it and means nothing after
//! a restart. So restoring is asking each of those apps to run again and giving its window the tab,
//! the group and the strip it was in — the desk is the same before and after, and no second tab
//! opens beside a saved one.
//! One app is asked at a time, the workspace in front's first: started together after a login, a
//! whole desk of apps fights over CPU, disk and memory, and each one shows its window several times
//! later than the first would alone; one after another, the first is up soonest and the next starts
//! on a quieter machine. The next is asked once the one before has its window, or could not open
//! one.
//! A tab whose app is gone — not installed any more, or it started nothing — says so where the
//! window would be, naming the app and why, and nothing else on the desk changes: the person closes
//! it when they are done with it. The answer stands for as long as the window would have, and the
//! next login asks again: by then the app may be installed, or the reason it would not start gone.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::platform::surfaces::DesktopHost;
use tilo_app_launch::Launch;
use tilo_desktop_model::{DesktopTree, TabId, TileContent, WorkspaceSlot};

use crate::apps::{self, Apps};
use crate::i18n::t_args;

/// How long a saved window may take to show itself before its tab says it did not open: an app that
/// started and never mapped is not coming.
const OPEN_DEADLINE: Duration = Duration::from_secs(15);

/// The saved windows of a desk, asked for again one at a time.
#[derive(Default)]
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
    launch: Option<Launch>,
}

/// The app asked for a saved tab, and when.
struct Opening {
    tab: TabId,
    name: String,
    asked: Instant,
}

impl Restore {
    /// The windows of `tree` that open again, in the order they are asked: the ones on the desk in
    /// front first, then the rest in the order the person left them.
    pub(crate) fn of(tree: &DesktopTree) -> Self {
        let mut saved = waiting_in(tree, |_| true);
        // Stable, so the desk's own order stands within each workspace.
        saved.sort_by_key(|(workspace, _)| *workspace != tree.active);
        Self {
            waiting: saved.into_iter().map(|(_, saved)| saved).collect(),
            opening: None,
            held: Vec::new(),
            held_checked: None,
        }
    }

    /// Ask, after what is waiting already, for the windows of the workspace `slot` of `tree` that
    /// wait for their apps: a workspace put on the desk with windows to open, as a branch is.
    pub(crate) fn add(&mut self, tree: &DesktopTree, slot: WorkspaceSlot) {
        self.waiting.extend(
            waiting_in(tree, |workspace| workspace == slot)
                .into_iter()
                .map(|(_, saved)| saved),
        );
    }

    /// Ask, after what is waiting already, for the windows `after` waits for that `before` did
    /// not: the windows someone else in a workspace opened, which this device opens its own
    /// instance of, each in the tab the workspace put it in.
    pub(crate) fn arrived(&mut self, before: &DesktopTree, after: &DesktopTree) {
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
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.opening
            .as_ref()
            .and_then(|opening| opening.asked.checked_add(OPEN_DEADLINE))
    }

    /// Ask for the next saved window when the one before has come, or say in its tab why it did
    /// not. A window of a workspace whose host has not taken this device in yet (`awaited` names
    /// that host, by the workspace's id) is held: its tab says what it waits for, and its app is
    /// asked for once the host admitted the device. Started before, the app would take part in
    /// nothing there, and would not join once the device is admitted.
    pub(crate) fn follow(
        &mut self,
        desktop: &DesktopHost,
        apps: Option<&Apps>,
        awaited: &dyn Fn(&str) -> Option<String>,
    ) {
        let Some(apps) = apps else {
            // This session starts no apps at all, so nothing will come for a saved window: the tabs
            // stay as they are.
            return;
        };
        self.release_admitted(desktop, awaited);
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

    /// The held windows whose workspace's host took this device in since: asked for next, in the
    /// order they were held, ahead of the ones not asked for yet.
    fn release_admitted(
        &mut self,
        desktop: &DesktopHost,
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
    fn open(&mut self, desktop: &DesktopHost, apps: &Apps, saved: Saved) -> bool {
        let Some(launch) = saved.launch else {
            let note = not_installed(&saved.name);
            desktop.with_policy(|policy| policy.note_in_tab(saved.tab, &note));
            return false;
        };
        let Some(workspace) = workspace_holding(desktop, saved.tab, |held| held.id().to_string())
        else {
            // The tab went with the workspace it was in.
            return false;
        };
        // Whatever the tab said about an earlier start is the past: it waits for its window now.
        // The place is noted before the app starts, as any launch into a tab of its own is: the
        // window takes that exact tab when it maps, in the background, so the window the person had
        // in front stays in front.
        desktop.with_policy(|policy| policy.note_in_tab(saved.tab, ""));
        if !desktop.with_policy(|policy| policy.adopt(&saved.app, saved.tab)) {
            return false;
        }
        match apps.start_beside(
            &launch,
            &apps.workspace_named(&workspace),
            Some(saved.tab),
            None,
        ) {
            Ok(()) => {
                self.opening = Some(Opening {
                    tab: saved.tab,
                    name: saved.name,
                    asked: Instant::now(),
                });
                true
            }
            Err(error) => {
                desktop.with_policy(|policy| policy.give_up_held(&saved.app));
                let said = error.to_string();
                let said = said
                    .strip_prefix(&format!("{}: ", launch.id()))
                    .unwrap_or(&said);
                tracing::warn!(
                    app = %saved.app,
                    error = %said,
                    "a window this device left open did not start"
                );
                let note = would_not_start(&saved.name);
                desktop.with_policy(|policy| policy.note_in_tab(saved.tab, &note));
                false
            }
        }
    }
}

/// `tree` without the places a launch left on it that no window came for.
/// A place stands for a launch the session asked for a moment ago. The session that asked is gone
/// by the next start, so nothing is on its way to those any more: they are not windows the person
/// had open, and a start that kept them would show the remains of a launch that ended — one more of
/// them with every restart, since each is written down with the desk.
pub(crate) fn without_unfilled(tree: &mut DesktopTree) {
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

/// The workspace the tab `tab` stands in, as `of` reads it: its desk slot, where a place stands
/// for the tab, or its id, which the tab's app is started into with that workspace's grants.
fn workspace_holding<T>(
    desktop: &DesktopHost,
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
fn waiting_in(
    tree: &DesktopTree,
    wanted: impl Fn(WorkspaceSlot) -> bool,
) -> Vec<(WorkspaceSlot, Saved)> {
    let mut saved = Vec::new();
    for workspace in tree
        .workspaces
        .iter()
        .filter(|workspace| wanted(workspace.slot))
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
                workspace.slot,
                Saved {
                    tab: tab.id,
                    app: app_id.clone(),
                    name,
                    launch: apps::launch_of(app_id),
                },
            ));
        }
    }
    saved
}

/// What a tab says when the app it holds is not on this machine.
fn not_installed(name: &str) -> String {
    t_args("desk-window-not-installed", &[("app", name.into())])
}

/// What a tab says when its app started nothing.
fn would_not_start(name: &str) -> String {
    t_args("desk-window-would-not-start", &[("app", name.into())])
}

/// What a tab says while its app waits for `host` to take this device into its workspace.
fn waits_for_host(name: &str, host: &str) -> String {
    t_args(
        "desk-window-waits-for-host",
        &[("app", name.into()), ("host", host.into())],
    )
}

/// What a tab says when its app started and never showed a window.
fn did_not_open(name: &str) -> String {
    t_args("desk-window-did-not-open", &[("app", name.into())])
}
