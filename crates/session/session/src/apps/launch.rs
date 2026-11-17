//! What becomes of a launch this session started: the windows the desktop gives a place, the
//! launches those windows belong to, and how a launch that ended is read.

use super::{Apps, Cause, Closed, Got, LaunchState, WidgetHost, not_opened};

impl Apps {
    /// The apps that ended since the last pass: one whose window came leaves it behind and is
    /// gone; one that crashed gets a notice in the place it had, offering it again; and one that
    /// ended before any window of it appeared could not open, and says so with the app's own
    /// reason.
    pub(super) fn follow_ended(&self) {
        self.mark_placed();
        let ended = self.state.borrow().spawner.events();
        for event in ended {
            // An agent has no window and no place of its own on the shell: its end is logged, and
            // the record's next change starts it again rather than this pass starting it in a loop.
            if let Some((workspace, agent)) = self.agent_ended(event.id) {
                tracing::warn!(
                    %agent,
                    %workspace,
                    end = %event.end,
                    "an agent stopped"
                );
                continue;
            }
            // The launch is over: its link retires with it, so the app's next window there starts a
            // process again, and its record keeps how far it got, for whoever reads it later.
            self.state.borrow_mut().held.remove(&event.id);
            let launch = self.state.borrow().launched.get(&event.id).cloned();
            let lived = launch.as_ref().map(|launch| launch.started.elapsed());
            // A launch that ended before any window of its own appeared never opened, however
            // long it ran and however it ended: the person asked for something that did not come,
            // and is told so with the app's own reason.
            let could_not_open = launch
                .as_ref()
                .is_some_and(|launch| launch.got == Got::Arriving);
            // The reason is the app's own last word when it said one: "it closed before its
            // window came" names no cause the person or an agent can act on.
            let why = event
                .said
                .stderr
                .last()
                .filter(|_| could_not_open)
                .cloned()
                .unwrap_or_else(|| tilo_desktop_surface::i18n::t("answer-why-not-opened"));
            self.end_launch(
                event.id,
                if could_not_open {
                    Got::CouldNotOpen { why: why.clone() }
                } else {
                    Got::Closed
                },
            );
            // An app that ran with no window at all (an action's run) and one whose window came
            // are closed by an end they chose; one that crashed gets a notice.
            if !event.end.crashed() && !could_not_open {
                tracing::info!(app = %event.app, "an app closed");
                continue;
            }
            if could_not_open {
                tracing::warn!(
                    app = %event.app,
                    lived_ms = lived.map_or(0, |lived| lived.as_millis()),
                    end = %event.end,
                    stderr = %event.said.stderr.join(" | "),
                    "an app could not open: it ended without ever showing a window"
                );
            } else {
                tracing::warn!(app = %event.app, end = %event.end, "an app stopped");
            }
            // What the app itself said before it ended, at warn beside the end: a launch that never
            // opened is otherwise a warning with no reason in it, and the app's own account is the
            // only one there is.
            for said in &event.said.stderr {
                tracing::warn!(
                    target: tilo_startup_metrics::APP_OUTPUT_TARGET,
                    app = %event.app,
                    stream = "stderr",
                    "{}",
                    tilo_app_launch::output::line(&event.app, "stderr", said)
                );
            }
            // Reopen asks for the app the way it was asked for this time: a host app by its desktop
            // id, a Tilo app by its package id.
            let asked = launch
                .as_ref()
                .map_or_else(|| event.app.clone(), |launch| launch.asked.clone());
            if launch.is_none() {
                tracing::warn!(app = %event.app, "a stopped app this session did not start");
            }
            // An app the session closed for memory did not stop working: the notice says which it
            // was, so the person does not read a closing they caused as a crash.
            let closed = match event.end.memory() {
                Some(Cause::Kernel) => Closed::Memory,
                Some(Cause::Runaway) => Closed::Runaway,
                None => Closed::Crashed,
            };
            if let Some(desktop) = self.desktop.borrow().clone() {
                // The tab the window was asked for says the launch could not open, rather than
                // waiting on a window that is not coming.
                let held = launch.as_ref().and_then(|launch| launch.instance);
                if let Some(tab) = held.filter(|_| could_not_open) {
                    self.could_not_open(Some(tab), &asked, &why);
                    continue;
                }
                let notice = desktop.with_policy(|policy| {
                    if could_not_open {
                        let answer = not_opened(policy, &asked, &why);
                        policy.not_opened(&asked, &event.title, answer)
                    } else {
                        policy.stopped(&asked, &event.title, closed)
                    }
                });
                if let Err(error) = notice {
                    tracing::warn!(%error, app = %event.app, "no notice for the app");
                }
                // The notice is a shell command like any other: the shell carries it out, and lays
                // its tab out, on its next pass.
                desktop.with_widgets(WidgetHost::relayout);
            }
        }
    }

    /// Read the windows the desktop gave a place since the last pass: a window whose process
    /// descends from a launch this session started, while that launch had no window yet, says
    /// that launch opened. This is the one place a launch becomes landed, so nothing has to guess
    /// it from what the shell shows of the app.
    pub(super) fn mark_placed(&self) {
        if self.state.borrow().launched.is_empty() {
            return;
        }
        let Some(desktop) = self.desktop.borrow().clone() else {
            return;
        };
        let placed = desktop.with_policy(tilo_desktop_policy::DesktopPolicy::take_mapped);
        let mut state = self.state.borrow_mut();
        for placed in placed {
            let Some(pid) = placed.pid else {
                continue;
            };
            let lineage = tilo_app_launch::links::ancestry(pid);
            let Some(launch) = state.launched.values_mut().find(|launch| {
                launch.got == Got::Arriving
                    && launch
                        .pid
                        .is_some_and(|launcher| lineage.contains(&launcher))
            }) else {
                continue;
            };
            launch.got = Got::Landed;
            tracing::info!(
                app = %launch.app,
                window = placed.window,
                "a launch opened: a window of it appeared"
            );
        }
    }

    /// Write down that launch `id` ended with `got`: what the launch's window is read as having
    /// done, for whoever asks after it.
    fn end_launch(&self, id: u64, got: Got) {
        let mut state = self.state.borrow_mut();
        if let Some(launch) = state.launched.get_mut(&id) {
            launch.got = got;
        }
    }

    /// How far the launch of `app` in `workspace` has got, while one this session started runs:
    /// what the tab that waits for its window and an agent that asked for it read, rather than
    /// guessing from what the shell shows of the app. The page names its windows' states by the
    /// same words.
    #[must_use]
    pub fn launch_state(&self, app: &str, workspace: &str) -> Option<LaunchState> {
        let state = self.state.borrow();
        let launch = state.launched.values().find(|launch| {
            // A launch with no window to show has no window state to report.
            launch.got != Got::Windowless
                && launch.workspace == workspace
                && (launch.app == app || launch.asked == app)
        })?;
        Some(match &launch.got {
            Got::Windowless | Got::Landed | Got::Closed => LaunchState::Landed,
            Got::Arriving => LaunchState::Arriving,
            Got::CouldNotOpen { why } => LaunchState::CouldNotOpen { why: why.clone() },
        })
    }
}
