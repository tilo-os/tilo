//! Windows programs as launches of this session: the compatibility service asks, through the
//! display it shows programs on ([`crate::windows_compat::Display`]), and the loop starts and ends
//! them here, on the path every kept app takes.

use tilo_app_launch::{Kind, Planned};

use super::Apps;
use crate::windows_compat::{Ask, Launch, Reopen, asked_entry};

/// A saved tab's Windows program the service was asked for, and has not answered yet.
pub(super) struct Waiting {
    answer: compat_service::ReopenAnswer,
    tab: u64,
    app: String,
    name: String,
}

impl Apps {
    /// Start and end what the compatibility service asked for since the last pass.
    pub(super) fn follow_windows_programs(&self) {
        let display = self.windows_display();
        for ask in display.take_asks() {
            match ask {
                Ask::Start {
                    launch,
                    handed,
                    ending,
                    reply,
                } => {
                    let started = self.start_windows_program(&launch, handed);
                    if let Ok(id) = started {
                        display.started(id, ending);
                    }
                    reply.send(started);
                }
                Ask::Stop(id) => self.state.borrow().spawner.end_one(id),
            }
        }
        self.follow_saved_windows_programs();
        self.follow_windows_networks(&display);
    }

    /// The saved tabs' programs the service answered since the last pass: one it would not open
    /// is handed to the restore, which says so in the tab.
    fn follow_saved_windows_programs(&self) {
        let display = self.windows_display();
        let mut state = self.state.borrow_mut();
        state.windows_waits.retain(|waiting| {
            let Some(outcome) = waiting.answer.take() else {
                return true;
            };
            if let Err(why) = outcome {
                tracing::warn!(
                    app = %waiting.app,
                    %why,
                    "a saved tab's Windows program did not open"
                );
                display.refused(waiting.tab, waiting.app.clone(), waiting.name.clone());
            }
            false
        });
    }

    /// Set each kept program's filtered network to what its workspace's grants allow now: the
    /// proxy lives in the launch, so this reaches a program that outlived the desktop that
    /// started it, and a grant taken back cuts the connections it allowed. A Windows appliance
    /// presentation's egress lived in the desktop that started it; the one that adopted it serves
    /// it again from what the launch kept.
    fn follow_windows_networks(&self, display: &crate::windows_compat::Display) {
        let networks = self.state.borrow().spawner.kept_networks();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|since| since.as_secs());
        let mut wanted = Vec::with_capacity(networks.len());
        let mut appliance = Vec::with_capacity(networks.len());
        for network in networks {
            let workspace = self.workspace_named(&network.workspace);
            let capabilities = launcher::capabilities::CapabilityPlan::for_app(
                &network.app,
                &workspace.grants.sandbox,
                now,
            );
            // The launch's namespace stays empty, so a `*` grant reaches a new launch, not this
            // one; grants taken back do reach it, and leave it nothing.
            if capabilities.net_hosts.iter().any(|name| name == "*") {
                continue;
            }
            if let Some(control) = network.control {
                wanted.push((control, capabilities.net_hosts.clone()));
            }
            if network.appliance && network.adopted {
                appliance.push((network.dir, capabilities.net_hosts.clone()));
            }
        }
        display.apply_networks(wanted);
        display.adopt_vm_networks(appliance);
    }

    /// Start `launch` behind the keeper `handed` was reserved for, through the launcher's
    /// `windows-program`.
    fn start_windows_program(
        &self,
        launch: &Launch,
        handed: tilo_app_launch::Handed,
    ) -> Result<u64, String> {
        let Some(launcher) = self.launcher() else {
            return Err("no tilo-launcher is installed".to_owned());
        };
        let mut args = vec![
            "windows-program".to_owned(),
            "--app-id".to_owned(),
            launch.app_id.clone(),
            "--name".to_owned(),
            launch.title.clone(),
        ];
        if launch.x11 {
            args.push("--x11".to_owned());
        }
        let planned = Planned {
            app: launch.app_id.clone(),
            title: launch.title.clone(),
            kind: Kind::Program,
            program: launcher,
            args,
            env: desktop_entry::launch::inherited_environment(),
            window_identities: vec![launch.app_id.clone()],
            ticket: None,
        };
        let asked = launch.asked.as_str();
        self.spawn_by(
            &planned,
            Some(asked),
            &launch.workspace,
            None,
            None,
            |spawner, write_down| {
                spawner
                    .start_handed(&planned, write_down, &launch.workspace, Some(asked), handed)
                    .map(|started| (started, None))
            },
        )
        .map_err(|error| error.to_string())
    }

    /// The launch `event` names ended: a Windows program's service is told, and one that ended
    /// while no desktop was there is handed back to it with its end.
    pub(super) fn windows_program_ended(&self, event: &tilo_app_launch::Event) {
        let display = self.windows_display();
        display.ended(event);
        let away = self.state.borrow_mut().spawner.ended_away(event.id);
        if let Some(launch) = away {
            display.take_back(event.id, &launch, Some(event));
        }
    }

    /// Hand the service the Windows programs among `adopted`, taken back after a restart.
    pub(super) fn take_back_windows_programs(&self, adopted: &[tilo_app_launch::Adopted]) {
        let display = self.windows_display();
        for kept in adopted.iter().filter(|kept| !kept.launch.service) {
            display.take_back(kept.id, &kept.launch, None);
        }
    }

    /// Whether a Windows program of this session has a window from the last pass whose class is
    /// not known: what makes the pass read the compositor's windows.
    pub(crate) fn windows_wanted(&self) -> bool {
        let state = self.state.borrow();
        state
            .launched
            .values()
            .any(|launch| launch.pid.is_some() && asked_entry(&launch.asked).is_some())
    }

    /// Note the window class each Windows program's window showed under, so a saved tab of it
    /// reopens the same program after a reboot.
    pub(crate) fn note_windows_identities(&self, windows: &[(u32, String)]) {
        let seen: Vec<(String, String, String, bool)> = {
            let state = self.state.borrow();
            let Some(launches) = &state.launches else {
                return;
            };
            let mut seen = Vec::new();
            for (pid, app) in windows {
                // The process the window descends from: the launch it was started as.
                let Some(started) = launches.launch_of(*pid) else {
                    continue;
                };
                let Some(launch) = state
                    .launched
                    .values()
                    .find(|launch| launch.pid == Some(started))
                else {
                    continue;
                };
                let Some((entry, setup)) = asked_entry(&launch.asked) else {
                    continue;
                };
                seen.push((
                    launch.workspace.clone(),
                    app.clone(),
                    entry.to_owned(),
                    setup,
                ));
            }
            seen
        };
        if seen.is_empty() {
            return;
        }
        let display = self.windows_display();
        for (workspace, app, entry, setup) in seen {
            display.seen_window(&workspace, &app, &entry, setup);
        }
    }

    /// How a saved tab's entry `entry` can be reopened: ready, waiting for the service to read
    /// what it can open, or gone from it.
    pub(crate) fn windows_reopened(&self, entry: &str) -> Reopen {
        self.windows_display().reopened(entry)
    }

    /// Open the saved tab `tab`'s entry `entry` in `workspace`, as the program of the class `app`
    /// it stood under: the service starts it on the same route a manager's launch takes, and the
    /// tab says so if it cannot.
    pub(crate) fn start_saved_windows_program(
        &self,
        entry: &str,
        workspace: &str,
        tab: u64,
        app: &str,
        name: &str,
    ) -> Result<(), String> {
        let display = self.windows_display();
        let wake = self.state.borrow().wake.clone();
        let answer = display
            .ask_reopen(entry, workspace, wake)
            .ok_or_else(|| "this session starts no Windows programs".to_owned())?;
        self.state.borrow_mut().windows_waits.push(Waiting {
            answer,
            tab,
            app: app.to_owned(),
            name: name.to_owned(),
        });
        Ok(())
    }

    /// The saved tabs' programs the service would not open since the last pass.
    pub(crate) fn take_windows_refused(&self) -> Vec<crate::windows_compat::Refused> {
        self.windows_display().take_refused()
    }

    /// Keep each Windows program behind a keeper from now on, as the session keeps its other apps.
    pub(super) fn keep_windows_programs(&self) {
        let (dirs, wake) = {
            let state = self.state.borrow();
            (state.spawner.kept_dirs(), state.wake.clone())
        };
        if let Some(dirs) = dirs {
            self.windows_display().keep_in(dirs, wake);
        }
    }
}
