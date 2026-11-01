//! The windows this device left open come back: each saved app is asked for
//! its window again, and the window takes the tab the person left it in.
//!
//! A saved window is kept as the app that opens it
//! ([`tilo_desktop_model::TileContent::LocalApp`]), because a live window's
//! surface belongs to the compositor that mapped it and means nothing after a
//! restart. So restoring is asking each of those apps to run again and giving
//! its window the tab, the group and the strip it was in — the desk is the
//! same before and after, and no second tab opens beside a saved one.
//!
//! One app is asked at a time, the workspace in front's first: started
//! together after a login, a whole desk of apps fights over CPU, disk and
//! memory, and each one shows its window several times later than the first
//! would alone; one after another, the first is up soonest and the next
//! starts on a quieter machine. The next is asked once the one before has its
//! window, or could not open one.
//!
//! A tab whose app is gone — not installed any more, or it started nothing —
//! says so where the window would be, naming the app and why, and nothing else
//! on the desk changes: the person closes it when they are done with it. The
//! answer stands for as long as the window would have, and the next login asks
//! again: by then the app may be installed, or the reason it would not start
//! gone.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use tilo_app_launch::Launch;
use tilo_desktop_model::{DesktopTree, TabId, TileContent, WorkspaceId};
use tilo_surface_host::DesktopHost;

use crate::i18n::t_args;
use crate::mount::apps::Apps;

/// How long a saved window may take to show itself before its tab says it
/// did not open: an app that started and never mapped is not coming.
const OPEN_DEADLINE: Duration = Duration::from_secs(15);

/// The saved windows of a desk, asked for again one at a time.
#[derive(Default)]
pub(crate) struct Restore {
    /// The saved windows not asked for yet, in the order they are asked.
    waiting: VecDeque<Saved>,
    /// The one whose app was asked for and whose window has not come yet.
    opening: Option<Opening>,
}

/// A window the person left open, and the app that opens it again.
struct Saved {
    tab: TabId,
    /// The app as its windows announce themselves: what the tab holds, and
    /// what the window that comes is matched by.
    app: String,
    /// The name the person knew it by, for what the tab says when the app
    /// cannot open.
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
    /// The windows of `tree` that open again, in the order they are asked:
    /// the ones on the desk in front first, then the rest in the order the
    /// person left them.
    pub(crate) fn of(tree: &DesktopTree) -> Self {
        let active = tree.active;
        let mut saved: Vec<(WorkspaceId, Saved)> = Vec::new();
        for workspace in &tree.workspaces {
            for strip in &workspace.strips {
                for group in &strip.window_groups {
                    for tab in &group.tabs {
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
                        let launch = launched(app_id);
                        saved.push((
                            workspace.id,
                            Saved {
                                tab: tab.id,
                                app: app_id.clone(),
                                name,
                                launch,
                            },
                        ));
                    }
                }
            }
        }
        // Stable, so the desk's own order stands within each workspace.
        saved.sort_by_key(|(workspace, _)| *workspace != active);
        Self {
            waiting: saved.into_iter().map(|(_, saved)| saved).collect(),
            opening: None,
        }
    }

    /// Ask for the next saved window when the one before has come, or say in
    /// its tab why it did not.
    pub(crate) fn follow(&mut self, desktop: &DesktopHost, apps: Option<&Apps>) {
        let Some(apps) = apps else {
            // This session starts no apps at all, so nothing will come for a
            // saved window: the tabs stay as they are.
            return;
        };
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
            if self.open(desktop, apps, saved) {
                return;
            }
        }
    }

    /// Ask for `saved`'s window into its own tab. True when an app was asked
    /// and its window may still come.
    fn open(&mut self, desktop: &DesktopHost, apps: &Apps, saved: Saved) -> bool {
        let Some(launch) = saved.launch else {
            let note = not_installed(&saved.name);
            desktop.with_policy(|policy| policy.note_in_tab(saved.tab, &note));
            return false;
        };
        let Some(room) = room_of(desktop, saved.tab) else {
            // The tab went with the workspace it was in.
            return false;
        };
        // Whatever the tab said about an earlier start is the past: it waits
        // for its window now. The place is noted before the app starts, as
        // any launch into a tab of its own is: the window takes that exact
        // tab when it maps, in the background, so the window the person had
        // in front stays in front.
        desktop.with_policy(|policy| policy.note_in_tab(saved.tab, ""));
        if !desktop.with_policy(|policy| policy.adopt(&saved.app, saved.tab)) {
            return false;
        }
        match apps.start_beside(&launch, &apps.room_named(&room)) {
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
                let note = would_not_start(&saved.name, said);
                desktop.with_policy(|policy| policy.note_in_tab(saved.tab, &note));
                false
            }
        }
    }
}

/// The workspace room of the tab `tab` stands in, which its app is started
/// into: the app runs where the person opened it, with that workspace's
/// grants.
fn room_of(desktop: &DesktopHost, tab: TabId) -> Option<String> {
    desktop.with_policy(|policy| {
        policy.handle().read(|tree| {
            tree.workspaces
                .iter()
                .find(|workspace| {
                    workspace.strips.iter().any(|strip| {
                        strip
                            .window_groups
                            .iter()
                            .any(|group| group.tabs.iter().any(|candidate| candidate.id == tab))
                    })
                })
                .map(|workspace| workspace.room().to_string())
        })
    })
}

/// How `app` starts again here: an installed Tilo package through the
/// launcher, or one of the machine's own applications through the entry whose
/// identities hold the name its window announced. `None` when this machine
/// has neither.
fn launched(app: &str) -> Option<Launch> {
    if let Some(package) = installed_package(app) {
        return Some(Launch::Package {
            app: package,
            open: None,
        });
    }
    let scan = desktop_entry::cache::cached_scan();
    let entry = scan.entries.iter().find(|entry| {
        entry.id.eq_ignore_ascii_case(app)
            || entry
                .window_identities()
                .iter()
                .any(|identity| identity.eq_ignore_ascii_case(app))
    })?;
    Some(Launch::Host {
        desktop_id: entry.id.clone(),
        open: None,
    })
}

/// The package id `app` is installed under: the id itself, or the package
/// whose short name it is.
fn installed_package(app: &str) -> Option<String> {
    if let Ok(Some(installed)) = tilo_installed_apps::read_one_from_env(app) {
        return Some(installed.package_id);
    }
    if app.contains('.') {
        return None;
    }
    let installed = tilo_installed_apps::read_from_env().ok()?;
    installed
        .into_iter()
        .find(|package| {
            tilo_app_launch::package_identities(&package.package_id)
                .iter()
                .any(|identity| identity.eq_ignore_ascii_case(app))
        })
        .map(|package| package.package_id)
}

/// What a tab says when the app it holds is not on this machine.
fn not_installed(name: &str) -> String {
    t_args("desk-window-not-installed", &[("app", name.into())])
}

/// What a tab says when its app started nothing.
fn would_not_start(name: &str, why: &str) -> String {
    t_args(
        "desk-window-would-not-start",
        &[("app", name.into()), ("why", why.into())],
    )
}

/// What a tab says when its app started and never showed a window.
fn did_not_open(name: &str) -> String {
    t_args("desk-window-did-not-open", &[("app", name.into())])
}
