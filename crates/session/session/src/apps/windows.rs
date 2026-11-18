//! Windows programs as launches of this session: the compatibility service asks, through the
//! display it shows programs on ([`crate::windows_compat::Display`]), and the loop starts and ends
//! them here, on the path every kept app takes.

use tilo_app_launch::{Kind, Planned};

use super::Apps;
use crate::windows_compat::{Ask, Launch};

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
        display.ended(event.id, &event.end);
        let away = self.state.borrow_mut().spawner.ended_away(event.id);
        if let Some(launch) = away {
            display.take_back(event.id, &launch, Some(&event.end));
        }
    }

    /// Hand the service the Windows programs among `adopted`, taken back after a restart.
    pub(super) fn take_back_windows_programs(&self, adopted: &[tilo_app_launch::Adopted]) {
        let display = self.windows_display();
        for kept in adopted.iter().filter(|kept| !kept.launch.service) {
            display.take_back(kept.id, &kept.launch, None);
        }
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
