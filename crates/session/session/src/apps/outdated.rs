//! Apps still open from before an update that changed how the desktop speaks to their keepers
//! ([`tilo_app_launch::Adopted::outdated`]). This desktop cannot show them, and starting a second
//! copy beside one would leave the person's unsaved work in a window nobody sees: each tab such an
//! app left says so instead, and Restart there ends the old app and opens it again in that tab.

use tilo_session_core::desktop::outcome::Outcome;

use super::{Apps, Launch, as_this_machine_knows_it};

/// An app from before the update, while it is there.
pub(super) struct FromBefore {
    /// What its tabs' button asks for, and what Restart opens.
    asked: String,
    /// The app id its windows announce: what the tab it opens again in is held for.
    app: String,
    /// A service's launch: a Linux VM's window forwarder, which shows all of the VM's apps and
    /// which its service starts again by itself once it ended.
    service: bool,
    /// The saved tabs that say so.
    tabs: Vec<u64>,
    /// Restart was pressed in this tab: the app opens there once the old one ended.
    restarting: Option<u64>,
}

impl Apps {
    /// Show the app of the outdated launch `kept` in the saved tabs `tabs` it left, each with the
    /// name it had there. Its first showing gives an app that left no tab one of its own, as a
    /// window with no saved place opens: unseen, it would hold unsaved work nobody could reach.
    pub(crate) fn show_outdated(&self, kept: &tilo_app_launch::Adopted, tabs: &[(u64, String)]) {
        let asked = kept
            .launch
            .asked
            .clone()
            .unwrap_or_else(|| kept.launch.app.clone());
        let service = kept.launch.service;
        let first = {
            let mut state = self.state.borrow_mut();
            let first = !state.outdated.contains_key(&kept.id);
            let shown = state.outdated.entry(kept.id).or_insert_with(|| FromBefore {
                asked: asked.clone(),
                app: kept.launch.app.clone(),
                service,
                tabs: Vec::new(),
                restarting: None,
            });
            shown.tabs.extend(tabs.iter().map(|(tab, _)| *tab));
            first
        };
        self.answer_in_tabs(|policy| {
            for (tab, name) in tabs {
                policy.note_in_tab(*tab, "");
                policy.failed_at(*tab, &asked, Outcome::from_before_update(name, service));
            }
            if first && tabs.is_empty() {
                let title = &kept.launch.title;
                let answer = Outcome::from_before_update(title, service);
                if let Err(error) = policy.not_opened(&asked, title, answer) {
                    tracing::warn!(app = %kept.launch.app, %error, "an app from before the update has no tab to say so in");
                }
            }
        });
    }

    /// Whether the launch `id` is still there, as this session counts its launches.
    #[must_use]
    pub(crate) fn is_launched(&self, id: u64) -> bool {
        self.state.borrow().launched.contains_key(&id)
    }

    /// Restart pressed in `tab`, which shows an app from before the update: its card says the
    /// restart is under way at once, and the old app is ended; it opens again there once it ended
    /// ([`Self::outdated_ended`]). False when the tab shows none.
    pub(super) fn restart_outdated(&self, tab: u64, asked: &str) -> bool {
        let (id, service, under_way) = {
            let state = self.state.borrow();
            let found = state
                .outdated
                .iter()
                .find(|(_, shown)| shown.tabs.contains(&tab))
                .or_else(|| {
                    state
                        .outdated
                        .iter()
                        .find(|(_, shown)| shown.asked == asked)
                });
            let Some((id, shown)) = found else {
                return false;
            };
            (*id, shown.service, shown.restarting.is_some())
        };
        if !under_way {
            if !self.state.borrow().spawner.restart_outdated(id) {
                return false;
            }
            if let Some(shown) = self.state.borrow_mut().outdated.get_mut(&id) {
                shown.restarting = Some(tab);
            }
        }
        self.answer_in_tabs(|policy| {
            let name = policy.name_of(asked);
            let answer = Outcome::from_before_update(&name, service).restarting(service);
            policy.failed_at(tab, asked, answer);
        });
        true
    }

    /// The outdated launch of `event` ended. Restart ended it: the app opens again in the tab
    /// Restart was pressed in. It ended on its own (its keeper ended it once no desktop that could
    /// show it came back in time): its tabs say so, with Reopen. A Linux VM's apps' tabs close
    /// either way: its service starts a current window forwarder, whose windows open as new tabs.
    /// False when it was no outdated launch.
    pub(super) fn outdated_ended(&self, event: &tilo_app_launch::Event) -> bool {
        let Some(shown) = self.state.borrow_mut().outdated.remove(&event.id) else {
            return false;
        };
        tracing::info!(app = %event.app, end = %event.end, "an app from before the update ended");
        self.state.borrow_mut().launched.remove(&event.id);
        if shown.service {
            self.answer_in_tabs(|policy| policy.close_answers(&shown.asked));
            return true;
        }
        let Some(tab) = shown.restarting else {
            self.answer_in_tabs(|policy| {
                let name = policy.name_of(&shown.asked);
                let answer = Outcome::closed_after_update(&name);
                policy.answer_again(&shown.asked, answer);
            });
            return true;
        };
        let Some(launch) = Launch::parse(&shown.asked) else {
            return true;
        };
        let launch = as_this_machine_knows_it(launch);
        // The press held the tab for what its button asks for, which need not be the id the app's
        // windows announce: the tab is held for those instead, so the new window takes it.
        self.answer_in_tabs(|policy| {
            policy.give_up_held(&shown.asked);
            policy.adopt(&shown.app, tab, None);
        });
        let workspace = self.workspace_of(Some(tab));
        if let Err(error) = self.start_at(&launch, &workspace, Some(tab)) {
            tracing::warn!(asked = %shown.asked, %error, "an app restarted after the update did not start");
            self.could_not_open(Some(tab), &shown.asked, None, &error.to_string());
        }
        true
    }
}
