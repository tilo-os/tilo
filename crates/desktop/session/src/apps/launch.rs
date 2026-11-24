//! What becomes of a launch this session started: the windows the desktop gives a place, the
//! launches those windows belong to, and how a launch that ended is read.

use tilo_app_launch::{Event, HeldLink, Planned};

use super::{ApiError, Apps, Cause, Closed, Got, LaunchState, Launching, not_opened};

impl Apps {
    /// Start `planned` with the link `link` , which the launch holds until it ends, and write it
    /// down as a launch of this session before the app can reach a service of it. A service asked
    /// as nothing has no window to offer again. `instance` is the tab the shell held for its
    /// window, when it held one. `timed` is the launch timing of the request that led here, which
    /// an agent start of no person's ask begins for itself.
    pub(super) fn spawn(
        &self,
        planned: &Planned,
        asked: Option<&str>,
        workspace: &str,
        instance: Option<u64>,
        link: Option<HeldLink>,
        timed: Option<u64>,
    ) -> Result<u64, ApiError> {
        self.spawn_by(
            planned,
            asked,
            workspace,
            instance,
            timed,
            |spawner, write_down| spawner.start_with(planned, link, write_down, workspace, asked),
        )
    }

    /// [`Self::spawn`], with `start` starting the launch on the spawner: what it starts is
    /// written down by the function it is handed before the app can reach a service of it.
    pub(super) fn spawn_by(
        &self,
        planned: &Planned,
        asked: Option<&str>,
        workspace: &str,
        instance: Option<u64>,
        timed: Option<u64>,
        start: impl FnOnce(
            &mut tilo_app_launch::Spawner,
            &dyn Fn(u32),
        ) -> Result<
            (tilo_app_launch::Started, Option<HeldLink>),
            tilo_app_launch::spawner::SpawnError,
        >,
    ) -> Result<u64, ApiError> {
        // An app of a workspace this device is not admitted to yet would take part in nothing
        // there, and would not join once the host admits the device: it is not started, and
        // whoever asked is told why at once.
        self.admitted(workspace)?;
        let launches = self.state.borrow().launches.clone();
        let mut state = self.state.borrow_mut();
        let windowless = planned.env.contains_key(ui::window_link::NO_WINDOW_ENV);
        // The launch's share in the launch-latency log runs from the request to the first presented
        // frame: the launcher's and the app's in this log, the surface's in the compositor's. A
        // launch with no window presents none.
        let timed = (!windowless)
            .then(|| timed.unwrap_or_else(|| tilo_startup_metrics::launch_requested(&planned.app)));
        let started = start(&mut state.spawner, &|pid| {
            if let Some(launches) = &launches {
                // The launcher starts a package in its sandbox and nothing else there, so a
                // package is the launch whose files are answered with references.
                if windowless {
                    launches.started_windowless(pid, &planned.app, workspace);
                } else {
                    launches.started(pid, &planned.app, workspace, !planned.kind.is_host());
                    if let Some(tab) = instance {
                        launches.held_for(pid, tab);
                    }
                }
            }
        });
        let started = match started {
            Ok(started) => started,
            Err(error) => {
                if let Some(timed) = timed {
                    tilo_startup_metrics::launch_failed(timed);
                }
                tracing::warn!(app = %planned.app, %workspace, %error, "a launch did not start");
                return Err(ApiError::Failed(error.to_string()));
            }
        };
        let (started, link) = started;
        if let Some(timed) = timed {
            tilo_startup_metrics::launch_spawned(timed, started.pid);
        }
        if let Some(asked) = asked {
            // This launch's ask replaces the last one of the same app in the same workspace, once
            // that one ended: what a caller reads is how the app's latest launch went.
            state.launched.retain(|_, launch| {
                !(launch.app == planned.app
                    && launch.workspace == workspace
                    && matches!(launch.got, Got::Closed | Got::CouldNotOpen { .. }))
            });
            // A launch with no window to show (an action's run) is not one that could fail to open.
            let got = if windowless {
                Got::Windowless
            } else {
                Got::Arriving
            };
            state.launched.insert(
                started.id,
                Launching {
                    asked: asked.to_owned(),
                    app: planned.app.clone(),
                    workspace: workspace.to_owned(),
                    pid: Some(started.pid),
                    instance,
                    window: None,
                    tab: None,
                    started: std::time::Instant::now(),
                    got,
                },
            );
        }
        if let Some(link) = link {
            state.held.insert(started.id, link);
        }
        drop(state);
        tracing::info!(
            app = %planned.app,
            pid = started.pid,
            "started {}",
            planned.describe()
        );
        Ok(started.id)
    }

    /// The apps that ended since the last pass: one whose window came leaves it behind and is
    /// gone; one that crashed gets a notice in the place it had, offering it again; and one that
    /// ended before any window of it appeared could not open, and says so with a designed
    /// sentence and what the app itself said under it.
    pub(super) fn follow_ended(&self) {
        self.mark_placed();
        let ended = self.state.borrow().spawner.events();
        for event in ended {
            // A spare that was never bound was no app's launch: nothing was asked of it.
            #[cfg(target_os = "linux")]
            if self.spare_ended(event.id) {
                continue;
            }
            #[cfg(target_os = "linux")]
            self.windows_program_ended(&event);
            #[cfg(target_os = "linux")]
            if self.outdated_ended(&event) {
                continue;
            }
            // The launch is over: its link retires with it, so the app's next window there starts a
            // process again, and its record keeps how far it got, for whoever reads it later.
            self.state.borrow_mut().held.remove(&event.id);
            let launch = self.state.borrow().launched.get(&event.id).cloned();
            // A keeper killed while no desktop was there took its app with it: this session never
            // started the app, and the tab it stood in says it stopped working once the saved
            // layout is laid out, which the restore does.
            if event.killed {
                continue;
            }
            let lived = launch.as_ref().map(|launch| launch.started.elapsed());
            // A launch that ended before any window of its own appeared never opened, however
            // long it ran and however it ended: the person asked for something that did not come,
            // and is told so with the app's own reason.
            let could_not_open = launch
                .as_ref()
                .is_some_and(|launch| launch.got == Got::Arriving);
            // The card leads with a designed sentence; what the app itself said goes under it,
            // in its own words, where a person can read them.
            let (why, said, printed) = not_opened_card(&event, could_not_open);
            self.end_launch(
                event.id,
                if could_not_open {
                    // What the app said, where it said anything, is what an agent asking after
                    // this launch reads; the card's own sentence is for the person.
                    Got::CouldNotOpen {
                        why: said.clone().unwrap_or_else(|| why.clone()),
                    }
                } else {
                    Got::Closed
                },
            );
            if event.asked_to_end {
                self.person_ended(&event, launch.as_ref(), could_not_open);
                continue;
            }
            // The stop that ends the session ends its apps with it: the tab stays as the person
            // left it, saved with the layout, and the next login opens the app there again rather
            // than offering to reopen an app that stopped working.
            if (self.state.borrow().session_ending)() {
                tracing::info!(app = %event.app, end = %event.end, "an app ended with the session");
                continue;
            }
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
            log_what_it_said(&event);
            self.tell_ended(
                &event,
                launch.as_ref(),
                could_not_open,
                &why,
                said.as_deref(),
                &printed,
            );
        }
    }

    /// Tell the person `event` ended: the tab its window was asked for says it could not open,
    /// with `why`, `said` what the app itself said and `printed` everything the launch printed; or
    /// the place its last window had says it stopped working. A launch no tab held is answered by
    /// whoever asked for it.
    fn tell_ended(
        &self,
        event: &Event,
        launch: Option<&Launching>,
        could_not_open: bool,
        why: &str,
        said: Option<&str>,
        printed: &str,
    ) {
        // Reopen asks for the app the way it was asked for this time: a host app by its desktop
        // id, a Tilo app by its package id.
        let asked = launch.map_or_else(|| event.app.clone(), |launch| launch.asked.clone());
        // A Windows program is asked by its entry in the manager's list, an identifier no person
        // reads; one that never showed a window never had a place on the shell.
        let windows_program = crate::windows_compat::asked_entry(&asked).is_some();
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
        let Some(desktop) = self.desktop.borrow().clone() else {
            return;
        };
        // The tab the window was asked for says the launch could not open, rather than waiting on
        // a window that is not coming.
        let held = launch.and_then(|launch| launch.instance);
        if let Some(tab) = held.filter(|_| could_not_open) {
            self.could_not_open_saying(Some(tab), &asked, Some(&event.title), why, said, printed);
            return;
        }
        // A Windows program with no window that ends at once never showed: the manager that
        // started it says what became of it, and a tab for something that never showed is noise
        // in the place the person is not looking.
        if could_not_open && windows_program {
            tracing::info!(app = %event.app, "a Windows program that never showed a window leaves no tab");
            return;
        }
        let notice = desktop.with_policy(|policy| {
            if could_not_open {
                let answer = not_opened(policy, &asked, Some(&event.title), why, said, printed);
                policy.not_opened(&asked, &event.title, answer)
            } else {
                // In the tab its last window was in, where the person left it.
                let tab = launch.and_then(|launch| launch.tab);
                policy.stopped(&asked, &event.title, closed, tab)
            }
        });
        if let Err(error) = notice {
            tracing::warn!(%error, app = %event.app, "no notice for the app");
        }
        // The notice is a shell command like any other: the shell carries it out, and lays its tab
        // out, on its next pass.
        desktop.ask();
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
            // A window of a connection the session made for an app, a spare's, is that app's
            // window for the instance it was bound with: its process id says nothing.
            if let (None, Some(instance)) = (placed.pid, placed.instance)
                && let Some(launch) = state
                    .launched
                    .values_mut()
                    .find(|launch| launch.got == Got::Arriving && launch.instance == Some(instance))
            {
                launch.window = Some(placed.window);
                launch.got = Got::Landed;
                tracing::info!(
                    app = %launch.app,
                    window = placed.window,
                    "a launch opened: a window of it appeared"
                );
                continue;
            }
            let Some(pid) = placed.pid else {
                continue;
            };
            let lineage = tilo_app_launch::links::ancestry(pid);
            let of_it = |launch: &&mut Launching| {
                launch
                    .pid
                    .is_some_and(|launcher| lineage.contains(&launcher))
            };
            // A launch still waiting for its first window is the one a new window answers.
            let arriving = state.launched.values().any(|launch| {
                launch.got == Got::Arriving
                    && launch
                        .pid
                        .is_some_and(|launcher| lineage.contains(&launcher))
            });
            let Some(launch) = state
                .launched
                .values_mut()
                .filter(of_it)
                .find(|launch| !arriving || launch.got == Got::Arriving)
            else {
                continue;
            };
            launch.window = Some(placed.window);
            if launch.got == Got::Arriving {
                launch.got = Got::Landed;
                tracing::info!(
                    app = %launch.app,
                    window = placed.window,
                    "a launch opened: a window of it appeared"
                );
            }
        }
        // Where each launch's window stands while it is shown: once it is gone, the tab it was
        // last in is where its end is told.
        for launch in state.launched.values_mut() {
            if let Some(tab) = launch
                .window
                .and_then(|window| desktop.with_policy(|policy| policy.tab_of(window)))
            {
                launch.tab = Some(tab);
            }
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

impl Apps {
    /// A stop the person asked for is theirs, as closing the app's last window is: the tab closes
    /// quietly, and nothing stands where the window was to say it stopped working. A tab the shell
    /// held for a window that was not coming is closed with it.
    fn person_ended(&self, event: &Event, launch: Option<&Launching>, could_not_open: bool) {
        tracing::info!(app = %event.app, end = %event.end, "an app the person ended closed");
        if could_not_open && let Some(tab) = launch.and_then(|launch| launch.instance) {
            self.close_tab(tab);
        }
    }
}

/// What the app itself said before it ended, at warn beside the end: a launch that never opened is
/// otherwise a warning with no reason in it, and the app's own account is the only one there is.
fn log_what_it_said(event: &Event) {
    for said in &event.said.stderr {
        tracing::warn!(
            target: tilo_startup_metrics::APP_OUTPUT_TARGET,
            app = %event.app,
            stream = "stderr",
            "{}",
            tilo_app_launch::output::line(&event.app, "stderr", said)
        );
    }
}

/// The Details of a launch `event` that ended before its window came: what the app and the
/// launcher said, line for line, without the launcher's own name on its lines.
fn details_said(event: &Event) -> String {
    event
        .said
        .stderr
        .iter()
        .map(|said| said.strip_prefix(launcher::SAYS).unwrap_or(said))
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a launch's answer is made of: the designed sentence the card leads with, what the app
/// itself said, quoted under it, and every line the launch printed, behind Details.
fn not_opened_card(event: &Event, could_not_open: bool) -> (String, Option<String>, String) {
    (
        why_not_opened(event, could_not_open),
        what_it_said(event, could_not_open),
        details_said(event),
    )
}

/// How many of the last lines an app printed a launch quotes when it ended before its window
/// came: enough to show what it was doing as it went, few enough to read.
const SAID_LINES: usize = 4;

/// The most of one quoted line: a launch's own account of the argv it ran is one enormous line,
/// and a card ends where a person can act on it.
const SAID_LINE_CHARS: usize = 240;

/// What the app itself said, when `event` ended before its window came: a panic's message, else
/// the last of its stderr lines that the launcher did not print, each clipped and joined by a
/// newline. "tilo-launcher: … ended: exit status: 1" is no reason the app gave, and tells a
/// person nothing to act on. A program that prints several lines as it fails keeps them all, so
/// a launch that dies inside its sandbox is never reported as one that said nothing at all.
fn what_it_said(event: &Event, could_not_open: bool) -> Option<String> {
    if !could_not_open {
        return None;
    }
    said_lines(&event.said.stderr)
}

/// The lines of an app's stderr that tell why it could not come: the message of the panic that
/// ended it (`thread 'main' panicked at src/main.rs:12:5:` says nothing a person can act on; the
/// line after it does), else its last lines that are not the launcher's own account or a note
/// the toolchain printed for whoever debugs it. A program that printed nothing of its own is
/// answered by the launcher's own account of how it ended, its prefix dropped, rather than by a
/// sentence with no fact under it.
fn said_lines(stderr: &[String]) -> Option<String> {
    // A panic's message is the cause on its own; the lines around it are the toolchain's.
    if stderr.iter().any(|line| line.contains("panicked at")) {
        return telling_line(stderr);
    }
    let lines: Vec<&str> = stderr
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect();
    // The launcher's own account of the launch is no reason the app gave, but a program that
    // printed nothing of its own is still reported by how it ended rather than by silence.
    let app: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| !line.starts_with(launcher::SAYS))
        .collect();
    let quoted: Vec<String> = if app.is_empty() {
        // The launcher's account of the argv it ran is one enormous line, and says how the
        // program was started rather than how it ended: what fits is quoted, and the enormous
        // line only where nothing else is.
        let fits: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|line| line.chars().count() <= SAID_LINE_CHARS)
            .collect();
        let from = if fits.is_empty() { &lines } else { &fits };
        from.iter()
            .rev()
            .take(SAID_LINES)
            .rev()
            .map(|line| line.strip_prefix(launcher::SAYS).unwrap_or(line))
            .map(|line| line.chars().take(SAID_LINE_CHARS).collect())
            .collect()
    } else {
        app.iter()
            .skip(app.len().saturating_sub(SAID_LINES))
            .map(|line| line.chars().take(SAID_LINE_CHARS).collect())
            .collect()
    };
    (!quoted.is_empty()).then(|| quoted.join("\n"))
}

/// The line of an app's stderr that best tells why it could not come: the message of the panic
/// that ended it, else its last line that is not the launcher's own or a note the toolchain
/// printed for whoever debugs it.
fn telling_line(stderr: &[String]) -> Option<String> {
    let mut lines: Vec<&str> = stderr
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with(launcher::SAYS))
        .collect();
    if lines.iter().any(|line| line.contains("panicked at")) {
        // The notes Rust prints for whoever debugs a panic are no cause the app gave.
        lines.retain(|line| !line.starts_with("note:"));
    }
    if let Some(at) = lines.iter().rposition(|line| line.contains("panicked at")) {
        let line = *lines.get(at)?;
        // Rust keeps the message on the line after "panicked at …:", and older versions keep it
        // in quotes on the same line.
        if let Some((message, close)) = line
            .split_once("panicked at ")
            .map(|(_, rest)| rest)
            .and_then(|rest| {
                rest.strip_prefix('\'')
                    .map(|message| (message, '\''))
                    .or_else(|| rest.strip_prefix('`').map(|message| (message, '`')))
            })
            && let Some(end) = message.find(close)
        {
            return message.get(..end).map(str::to_owned);
        }
        let next = at.checked_add(1).and_then(|next| lines.get(next));
        return next.map(|message| (*message).to_owned());
    }
    lines.last().map(|line| (*line).to_owned())
}

/// The sentence the app whose launch `event` ended leads the person with: the cause the launcher
/// named by its exit, when it is one it knows, else the shell's account that its window did not
/// come. What the app printed stays out of it: [`what_it_said`] shows that as a quote.
fn why_not_opened(event: &Event, could_not_open: bool) -> String {
    if !could_not_open {
        return tilo_session_core::desktop::i18n::t("answer-why-not-opened");
    }
    let key = match event.end {
        tilo_app_launch::End::Exited {
            code: launcher::user_namespaces::EXIT_NO_USER_NAMESPACES,
            ..
        } => "answer-why-no-sandbox",
        _ => "answer-why-not-opened",
    };
    tilo_session_core::desktop::i18n::t(key)
}

#[cfg(test)]
mod tests {
    use super::{SAID_LINE_CHARS, said_lines, telling_line};

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    /// A panic says where it happened in one line that helps nobody, and what went wrong in the
    /// next: the card quotes the second, never the address or the backtrace note.
    #[test]
    fn a_panics_message_is_what_the_app_said() {
        let said = telling_line(&lines(
            "thread 'main' panicked at apps/notes/src/main.rs:12:5:\n\
             called `Option::unwrap()` on a `None` value\n\
             note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\
             tilo-launcher: so.tilo.notes ended: exit status: 101",
        ));
        assert_eq!(
            said.as_deref(),
            Some("called `Option::unwrap()` on a `None` value")
        );
    }

    /// An older Rust keeps the message on the panic line, in quotes.
    #[test]
    fn an_older_panics_message_is_read_from_its_quotes() {
        let said = telling_line(&lines(
            "thread 'main' panicked at 'called `Result::unwrap()` on an `Err` value: NoSpace', src/main.rs:30:9\n\
             note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace",
        ));
        assert_eq!(
            said.as_deref(),
            Some("called `Result::unwrap()` on an `Err` value: NoSpace")
        );
    }

    /// An app that says why in words, and then a launcher that says the process ended, is quoted
    /// by its own words alone.
    #[test]
    fn an_apps_last_words_are_quoted_without_the_launchers() {
        let said = telling_line(&lines(
            "the API failed error=the database went away (os error 88)\n\
             tilo-launcher: so.tilo.photos ended: exit status: 1",
        ));
        assert_eq!(
            said.as_deref(),
            Some("the API failed error=the database went away (os error 88)")
        );
    }

    /// Nothing the app said, only the launcher's own account: there is no line the launcher can
    /// quote as the app's reason.
    #[test]
    fn a_launcher_alone_says_nothing_of_the_app() {
        assert_eq!(
            telling_line(&lines("tilo-launcher: so.tilo.notes ended: exit status: 1")),
            None
        );
        assert_eq!(telling_line(&[]), None);
    }

    /// A program that says several things as it fails keeps them all: the last lines it printed
    /// are quoted in order, not only the last one.
    #[test]
    fn the_last_lines_an_app_printed_are_all_quoted() {
        let said = said_lines(&lines(
            "tilolang: the package's module is not installed\n\
             tilolang: the workspace refused its first write\n\
             tilo-launcher: so.tilo.chat ended: exit status: 2",
        ));
        assert_eq!(
            said.as_deref(),
            Some(
                "tilolang: the package's module is not installed\n\
                 tilolang: the workspace refused its first write"
            )
        );
    }

    /// Only the last few lines are quoted, so a launch that printed a page still names what it
    /// was doing when it went.
    #[test]
    fn only_the_last_few_lines_are_quoted() {
        let text = (1..=10)
            .map(|n| format!("step {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let said = said_lines(&lines(&text)).expect("the app's lines");
        assert_eq!(said, "step 7\nstep 8\nstep 9\nstep 10");
    }

    /// A program that printed nothing of its own is still reported by how it ended: the launch
    /// never answers with a sentence and no fact under it.
    #[test]
    fn a_silent_child_is_reported_by_how_it_ended() {
        let said = said_lines(&lines(
            "tilo-launcher: so.tilo.chat ran bwrap --unshare-all --clearenv\n\
             tilo-launcher: so.tilo.chat ended: exit status: 2",
        ));
        assert_eq!(
            said.as_deref(),
            Some(
                "so.tilo.chat ran bwrap --unshare-all --clearenv\n\
                 so.tilo.chat ended: exit status: 2"
            ),
            "the launcher's account is quoted, without its prefix, when the app said nothing"
        );
    }

    /// An enormous line — the launcher's account of the argv it ran — is clipped, so the quote
    /// stays something a person can read.
    #[test]
    fn an_enormous_line_is_clipped() {
        let long = format!("tilo-launcher: {} ended: exit status: 2", "x".repeat(1000));
        let said = said_lines(&lines(&long)).expect("how it ended");
        assert_eq!(said.chars().count(), SAID_LINE_CHARS);
    }

    /// A silent child's answer names how it ended, not the page of mounts the launcher ran it
    /// with: the enormous line is skipped while anything shorter is there to say.
    #[test]
    fn a_silent_childs_answer_skips_the_argument_page() {
        let huge = format!(
            "tilo-launcher: so.tilo.chat ran bwrap {}",
            "--ro-bind /a /b ".repeat(400)
        );
        let said = said_lines(&lines(&format!(
            "{huge}\ntilo-launcher: so.tilo.chat ended: exit status: 2"
        )));
        assert_eq!(
            said.as_deref(),
            Some("so.tilo.chat ended: exit status: 2"),
            "how it ended, without the arguments it was started with"
        );
    }
}
