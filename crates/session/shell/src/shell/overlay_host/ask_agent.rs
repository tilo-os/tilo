//! The ask-agent chord and the "Tell an agent…" requests Tilo apps send: both
//! open the agent quick composer on the focused window's selection
//! ([`crate::shell::selection_agent`]).

use std::time::Instant;

use ui::shared_selection::SharedSelection;
use vello::kurbo::{Point, Rect};

use super::ShellOverlayHost;
use crate::shell::agent_quick_composer::{AgentQuickComposerOrigin, ComposerOwner};
use crate::shell::certified_action_session::ActionSessionSource;
use crate::shell::collab_broker::AppActionBroker;
use crate::shell::selection_agent::{AgentStart, ChordRoute, SelectionSeed};

impl ShellOverlayHost {
    /// `app_id` on shell-ui `connection` answers selection requests.
    pub(crate) fn note_selection_capable(&mut self, connection: u64, app_id: &str) {
        self.selection_requests.capable(connection, app_id);
    }

    /// The window of `app_id` on shell-ui `connection` shows `resource`.
    pub(crate) fn note_window_resource(
        &mut self,
        connection: u64,
        app_id: &str,
        resource: Option<tilo_actions::ActionResource>,
    ) {
        self.selection_requests
            .window_resource(connection, app_id, resource);
    }

    /// Shell-ui `connection` went away.
    pub(crate) fn forget_selection_capable(&mut self, connection: u64) {
        self.selection_requests.disconnected(connection);
    }

    /// The ask-agent chord went down; the next pump routes it. A held key's
    /// repeats ask nothing more.
    pub(super) fn press_ask_agent_chord(&mut self, repeat: bool) -> bool {
        if !repeat {
            self.selection_requests.press();
            self.bump_chrome();
        }
        true
    }

    /// Advance the ask-agent chord: the shell-ui connections of the focused
    /// app to ask for its selection now. When there are none, or the asked
    /// app let its moment pass, the composer opens with what the desktop
    /// knows itself.
    pub(crate) fn route_ask_agent_chord(&mut self, now: Instant) -> Vec<u64> {
        self.follow_agent_start(now);
        let focused = self.focused_app_id();
        match self.selection_requests.route(focused.as_deref(), now) {
            Some(ChordRoute::Ask(connections)) => connections,
            Some(ChordRoute::Open) => {
                self.open_ask_agent_on_primary();
                Vec::new()
            }
            None => {
                if self.selection_requests.expired(now) {
                    self.open_ask_agent_on_primary();
                }
                Vec::new()
            }
        }
    }

    /// When the ask-agent chord next needs a tick.
    pub(crate) fn ask_agent_deadline(&self, now: Instant) -> Option<Instant> {
        self.selection_requests.deadline(now)
    }

    /// `app_id` (as the launcher vouched for it) asked an agent about its
    /// `selection`, drawn at `area` on the desktop when it said where: in
    /// answer to the chord, or from an "Tell an agent…" entry in its focused
    /// window. Whether the composer opened.
    pub(crate) fn receive_ask_agent(
        &mut self,
        app_id: &str,
        selection: Option<SharedSelection>,
        area: Option<Rect>,
    ) -> bool {
        let focused = self.focused_app_id();
        if !self.selection_requests.accept(app_id, focused.as_deref()) {
            return false;
        }
        let label = self.app_label(app_id);
        self.open_ask_agent(SelectionSeed::new(app_id, label, selection).drawn_at(area))
    }

    /// "Tell an agent…" on a window of `app_id`, chosen at `at` on the
    /// desktop: the composer opens there about that app. Whether it opened.
    /// It attaches the resource the app said its window shows, else names
    /// the app alone.
    pub(super) fn tell_agent_about_app(&mut self, app_id: &str, at: Point) -> bool {
        let label = self.app_label(app_id);
        let window = self.selection_requests.window_of(app_id).map(|resource| {
            SharedSelection::of_text(&resource.label)
                .at(tilo_actions::SelectionPlace::item(resource.id.as_str()))
                .in_resource(resource.clone())
        });
        self.open_ask_agent(
            SelectionSeed::new(app_id, label, window)
                .drawn_at(Some(Rect::from_center_size(at, (1.0, 1.0)))),
        )
    }

    /// The focused window offers no selection of its own: ask about the
    /// primary selection's text, in the focused app.
    fn open_ask_agent_on_primary(&mut self) -> bool {
        let app_id = self.focused_app_id().unwrap_or_default();
        let label = self.app_label(&app_id);
        let selection = ui::text_selection::clipboard_paste_primary()
            .filter(|text| !text.trim().is_empty())
            .map(|text| SharedSelection::of_text(&text));
        self.open_ask_agent(SelectionSeed::new(app_id, label, selection))
    }

    /// Open the quick composer about `seed`, for the agent asked last in the
    /// focused workspace (or its first live one) when that agent is admitted
    /// and holds a live task subscription. Says so when none does.
    pub(crate) fn open_ask_agent(&mut self, seed: SelectionSeed) -> bool {
        use crate::shell::state::WorkspaceAuthority as _;
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let Some((room, person, members)) = ({
            let state = chrome.borrow();
            let tree = state.tree();
            tree.desktop.active_workspace().map(|workspace| {
                (
                    workspace.room().to_string(),
                    state.active_local_user_in(&tree),
                    workspace
                        .agent_members()
                        .into_iter()
                        .map(|(agent, _)| agent)
                        .collect::<Vec<_>>(),
                )
            })
        }) else {
            return false;
        };
        let (source, live) = self
            .live_agents(&room, members.clone())
            .map_or((None, Vec::new()), |(source, live)| (Some(source), live));
        let chosen = self.selection_requests.choose_agent(&room, &live);
        let (Some(source), Some(agent)) = (source, chosen) else {
            let osd = if self.start_admitted_agent(&room, &members, seed) {
                crate::shell::osd::Osd::starting_agent()
            } else {
                crate::shell::osd::Osd::no_agent()
            };
            chrome.borrow_mut().show_osd(osd);
            return false;
        };
        let Some(identity_revision) = source.identity_revision(&room) else {
            return false;
        };
        let owner = ComposerOwner {
            room: room.clone(),
            agent: agent.clone(),
            person,
            identity_revision,
        };
        if !self
            .agent_actions
            .entry(owner.clone())
            .or_default()
            .open(source, owner.clone())
        {
            return false;
        }
        self.selection_requests.asked(&room, &agent);
        let viewport = self.overlay_area();
        // Where the thing is drawn was clipped to its window already when an
        // app said; the desktop's own surfaces (a tab) are outside windows.
        let window = match seed.area {
            Some(_) => viewport,
            None => self.focused_window_rect().unwrap_or(viewport),
        };
        let (at, protected) = selection_area(window, self.text_caret, seed.area);
        self.agent_quick_composer.open_about(
            owner,
            &agent,
            &AgentQuickComposerOrigin {
                marker: at,
                protected,
                context: String::new(),
            },
            viewport,
            seed,
        );
        self.agent_quick_composer.offer_agents(live);
        self.bump_chrome();
        true
    }

    /// The person picked another agent in the composer: open it again for
    /// that one, about the same thing. Whether it opened.
    pub(super) fn switch_composer_agent(&mut self) -> bool {
        let Some(agent) = self.agent_quick_composer.take_agent_switch() else {
            return false;
        };
        let (Some(owner), Some(seed)) = (
            self.agent_quick_composer.owner().cloned(),
            self.agent_quick_composer.about().cloned(),
        ) else {
            return false;
        };
        self.selection_requests.asked(&owner.room, &agent);
        self.open_ask_agent(seed)
    }

    /// No agent of the focused workspace takes tasks: start the one asked
    /// last there (else its first admitted agent), and open the composer about `seed` once it takes them.
    /// Whether one is starting. Only someone who may administer the
    /// workspace starts its agents.
    fn start_admitted_agent(
        &mut self,
        room: &str,
        members: &[String],
        seed: SelectionSeed,
    ) -> bool {
        let Some(agent) = self.selection_requests.choose_agent(room, members) else {
            return false;
        };
        // A chord pressed again while the agent comes up asks about the
        // newer selection; the agent is not started twice.
        if self.selection_requests.is_starting(room, &agent) {
            self.selection_requests
                .starting(room, &agent, seed, Instant::now());
            return true;
        }
        let Some(workspace) = self
            .chrome
            .as_ref()
            .and_then(|chrome| chrome.borrow().tree().desktop.active_workspace().cloned())
        else {
            return false;
        };
        if let Err(error) = crate::shell::launcher::launch_workspace_agent(&agent, &workspace) {
            tracing::info!(%agent, error = %error.user_message(), "the ask-agent chord could not start the agent");
            return false;
        }
        tracing::info!(%agent, "the ask-agent chord started the workspace's agent");
        self.selection_requests
            .starting(room, &agent, seed, Instant::now());
        true
    }

    /// Open the composer once an agent the chord started takes tasks, or
    /// say there is no agent when it did not come up.
    fn follow_agent_start(&mut self, now: Instant) {
        let broker = self.shortcut_triggers.broker();
        // Ready means exactly what `open_ask_agent` asks for. On 2026-10-19
        // this waited only for the task subscription: an agent that had one
        // but no certified identity was "ready", `open_ask_agent` found no
        // agent to ask and started another, every frame, until the shell
        // ran out of file descriptors (87 agents in a second).
        let outcome = self.selection_requests.started(now, |room, agent| {
            broker
                .as_ref()
                .is_some_and(|broker| askable(broker, room, agent))
        });
        match outcome {
            Some(AgentStart::Ready(seed)) => {
                self.open_ask_agent(*seed);
            }
            Some(AgentStart::GaveUp) => {
                if let Some(chrome) = self.chrome.as_ref() {
                    chrome
                        .borrow_mut()
                        .show_osd(crate::shell::osd::Osd::no_agent());
                }
                self.bump_chrome();
            }
            None => {}
        }
    }

    /// The session source for the workspace, and those of its agent
    /// `members` that are admitted (certified under their own key) and hold
    /// a live task subscription, so a queued task reaches them.
    /// Ask agents the work the chrome handed them (a notification), each
    /// as an issue in its workspace's Tasks. Whether one was asked.
    pub(super) fn pump_agent_asks(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let asks = chrome.borrow_mut().take_agent_asks();
        let mut asked = false;
        for ask in asks {
            let Some(broker) = self.shortcut_triggers.broker() else {
                break;
            };
            let source = ActionSessionSource::new(broker);
            let room = ask.room.to_string();
            let Some(identity_revision) = source.identity_revision(&room) else {
                continue;
            };
            let person = chrome.borrow().local_user_for_room(&ask.room);
            let owner = ComposerOwner {
                person,
                room,
                agent: ask.agent,
                identity_revision,
            };
            if self
                .agent_actions
                .entry(owner.clone())
                .or_default()
                .open_and_ask(source, &owner, (&ask.title, &ask.body))
            {
                asked = true;
            } else {
                tracing::warn!(agent = %owner.agent, "a notification could not be handed to the agent");
            }
        }
        asked
    }

    fn live_agents(
        &self,
        room: &str,
        members: Vec<String>,
    ) -> Option<(ActionSessionSource, Vec<String>)> {
        let broker = self.shortcut_triggers.broker()?;
        let live = members
            .into_iter()
            .filter(|agent| askable(&broker, room, agent))
            .collect();
        Some((ActionSessionSource::new(broker), live))
    }

    fn focused_app_id(&self) -> Option<String> {
        self.chrome
            .as_ref()?
            .borrow()
            .active_app_id()
            .map(|app| app.as_str().to_owned())
    }

    /// What `app_id` is called, for the composer and the agent.
    pub(super) fn app_label(&self, app_id: &str) -> String {
        crate::shell::launcher::display_name_for_app_id(app_id)
            .or_else(|| self.chrome.as_ref()?.borrow().active_app_title())
            .unwrap_or_else(|| app_id.to_owned())
    }
}

/// Whether `agent` takes asks in `room`: admitted (certified under its own
/// key) and holding a live task subscription. The one test both the chord
/// and the start it waits for use.
fn askable(broker: &AppActionBroker, room: &str, agent: &str) -> bool {
    broker.agent_is_live_task_capable(room, agent)
        && broker
            .certified_agent_identity(room, &format!("agent:{agent}"))
            .is_some_and(|identity| identity.agent_key == agent)
}

/// Where the composer about a selection in `window` points, and what it must
/// not cover. An app that says where its selection is drawn (`area`: the
/// selected lines, else the text block holding them) gets all of it kept
/// clear, so the composer opens beside, below or above that text rather
/// than over its rest. Otherwise the focused text field's caret, which ends
/// the selection, is all the desktop knows: the composer points at it and
/// keeps clear of that line across the window. Without either it points a
/// third of the way down, where the person was reading. The popover keeps
/// itself inside the screen in every case.
fn selection_area(window: Rect, caret: Option<Rect>, area: Option<Rect>) -> (Rect, Rect) {
    if let Some(area) = area
        .map(|area| area.intersect(window))
        .filter(|area| area.width() > 0.0 && area.height() > 0.0)
    {
        return (area, area);
    }
    if let Some(caret) = caret.filter(|caret| window.contains(caret.center())) {
        return (caret, Rect::new(window.x0, caret.y0, window.x1, caret.y1));
    }
    let at = Rect::from_center_size(
        Point::new(window.center().x, window.y0 + window.height() / 3.0),
        (1.0, 1.0),
    );
    (at, at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_composer_keeps_clear_of_the_line_with_the_caret() {
        let window = Rect::new(100.0, 50.0, 900.0, 650.0);
        let caret = Rect::new(400.0, 300.0, 402.0, 318.0);
        assert_eq!(
            selection_area(window, Some(caret), None),
            (caret, Rect::new(100.0, 300.0, 900.0, 318.0))
        );
        let (at, protected) = selection_area(window, Some(Rect::new(5.0, 5.0, 7.0, 20.0)), None);
        assert_eq!(
            at, protected,
            "a caret outside the window is not the selection"
        );
        assert_eq!(at.center(), Point::new(500.0, 250.0));
        assert_eq!(selection_area(window, None, None), (at, protected));
    }

    #[test]
    fn the_composer_keeps_clear_of_all_the_selected_text_it_was_told_of() {
        let window = Rect::new(100.0, 50.0, 900.0, 650.0);
        let caret = Rect::new(400.0, 300.0, 402.0, 318.0);
        let paragraph = Rect::new(200.0, 264.0, 800.0, 336.0);
        assert_eq!(
            selection_area(window, Some(caret), Some(paragraph)),
            (paragraph, paragraph),
            "the whole selected text is kept clear, not only the caret's line"
        );
        // Only what is in view of the window counts.
        let (at, protected) = selection_area(
            window,
            Some(caret),
            Some(Rect::new(200.0, 0.0, 800.0, 336.0)),
        );
        assert_eq!((at, protected), (Rect::new(200.0, 50.0, 800.0, 336.0), at));
        // Bounds wholly out of view say nothing: the caret's line again.
        assert_eq!(
            selection_area(
                window,
                Some(caret),
                Some(Rect::new(0.0, 700.0, 50.0, 720.0))
            ),
            selection_area(window, Some(caret), None)
        );
    }
}
