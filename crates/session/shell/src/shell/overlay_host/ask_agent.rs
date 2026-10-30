//! The ask-agent chord and the questions typed into Tilo apps' context menus: both
//! open the agent quick composer on the focused window's selection
//! ([`crate::shell::selection_agent`]).

use std::time::Instant;

use ui::shared_selection::SharedSelection;
use vello::kurbo::{Point, Rect};

use super::ShellOverlayHost;
use crate::shell::agent_quick_composer::{AgentQuickComposerOrigin, ComposerOwner};
use crate::shell::certified_action_session::ActionSessionSource;
use crate::shell::collab_broker::AppActionBroker;
use crate::shell::selection_agent::{ChordRoute, SelectionSeed};

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
        let focused = self.focused_app_id();
        match self.selection_requests.route(focused.as_deref(), now) {
            Some(ChordRoute::Ask(connections)) => connections,
            Some(ChordRoute::Open) => {
                self.open_ask_agent_once_settled(now);
                Vec::new()
            }
            None => {
                if self.selection_requests.expired(now) {
                    self.open_ask_agent_once_settled(now);
                }
                Vec::new()
            }
        }
    }

    /// Open the composer on the primary selection, or, while this device's
    /// identity in the active room has just changed and is not settled yet
    /// (the composer's session is bound to a settled one), at the moment it
    /// settles: the press and what the palette asked wait for it rather
    /// than being dropped.
    fn open_ask_agent_once_settled(&mut self, now: Instant) {
        if let Some(at) = self.identity_settles_at(now) {
            tracing::debug!(
                wait = ?at.saturating_duration_since(now),
                "the composer opens once this device's identity in the room settles"
            );
            self.selection_requests.defer_open(at);
            return;
        }
        self.open_ask_agent_on_primary();
    }

    /// When this device's identity in the active workspace's room settles,
    /// while it is not settled yet.
    fn identity_settles_at(&self, now: Instant) -> Option<Instant> {
        let room = self
            .chrome
            .as_ref()?
            .borrow()
            .tree()
            .desktop
            .active_workspace()?
            .room()
            .to_string();
        let stamp = tilo_identity_host::join_proof::current_device_stamp(&room)?;
        if stamp.settled() {
            return None;
        }
        let wait = stamp
            .settles_at()?
            .duration_since(std::time::SystemTime::now())
            .unwrap_or_default();
        Some(now.checked_add(wait).unwrap_or(now))
    }

    /// When the ask-agent chord next needs a tick.
    pub(crate) fn ask_agent_deadline(&self, now: Instant) -> Option<Instant> {
        self.selection_requests.deadline(now)
    }

    /// `app_id` (as the launcher vouched for it) asked an agent about its
    /// `selection`, drawn at `area` on the desktop when it said where: in
    /// answer to the chord, or from a sentence typed into a menu of its focused
    /// window. Whether the composer opened.
    pub(crate) fn receive_ask_agent(
        &mut self,
        app_id: &str,
        selection: Option<SharedSelection>,
        area: Option<Rect>,
        ask: Option<String>,
        draft: Option<String>,
    ) -> bool {
        let focused = self.focused_app_id();
        if !self.selection_requests.accept(app_id, focused.as_deref()) {
            return false;
        }
        let label = self.app_label(app_id);
        let mut seed = SelectionSeed::new(app_id, label, selection).drawn_at(area);
        if let Some(text) = ask {
            seed = seed.asking(&text);
        }
        seed.draft = draft.map(|text| {
            text.chars()
                .take(ui::shared_selection::MAX_ASK_CHARS)
                .collect()
        });
        self.open_ask_agent(seed)
    }

    /// A sentence for an agent typed into the menu of a window of `app_id`
    /// at `at` on the desktop (`None`: the composer opens to be typed in):
    /// the composer opens there about that app. It attaches the resource the
    /// app said its window shows, else names the app alone.
    pub(super) fn tell_agent_about_app(
        &mut self,
        app_id: &str,
        at: Point,
        ask: Option<String>,
    ) -> bool {
        let label = self.app_label(app_id);
        let seed = SelectionSeed::new(app_id, label, None)
            .drawn_at(Some(Rect::from_center_size(at, (1.0, 1.0))));
        let seed = match ask {
            Some(text) => seed.asking(&text),
            None => seed,
        };
        let seed = match self.selection_requests.window_of(app_id) {
            Some(resource) => seed.showing(resource.clone()),
            None => seed,
        };
        self.open_ask_agent(seed)
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

    /// Open the quick composer about `seed`. Its agent defaults to a new
    /// agent made for the task the asking creates
    /// ([`crate::shell::task_agents`]); the workspace's collaborators that
    /// take tasks now are offered beside it, and picking one assigns the
    /// task to it. When a new agent cannot be asked here (no agent runtime,
    /// no model, offline) the composer says which and offers the fix in
    /// place: it never waits on an agent that is not coming.
    pub(crate) fn open_ask_agent(&mut self, mut seed: SelectionSeed) -> bool {
        use crate::shell::task_agents::NEW_TASK_AGENT;
        // On a canvas a question's answer is a window where it was asked.
        if self.canvas_shown() && seed.ask.is_some() {
            return self.ask_about_on_canvas(&seed);
        }
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let Some((room, person, members)) = active_room_agents(&chrome.borrow()) else {
            tracing::warn!("the composer did not open: no workspace is active");
            return false;
        };
        let Some((source, collaborators)) = self.live_agents(&room, members) else {
            tracing::warn!("the composer did not open: the session reaches no broker");
            return false;
        };
        let mut choices = vec![NEW_TASK_AGENT.to_owned()];
        choices.extend(collaborators);
        let agent = self
            .selection_requests
            .choose_agent(&room, &choices)
            .unwrap_or_else(|| NEW_TASK_AGENT.to_owned());
        let Some(identity_revision) = source.identity_revision(&room) else {
            tracing::warn!(
                "the composer did not open: this device's identity in the room is not settled yet"
            );
            return false;
        };
        let owner = ComposerOwner {
            room: room.clone(),
            agent: agent.clone(),
            person,
            identity_revision,
        };
        if let Err(error) = self
            .agent_actions
            .entry(owner.clone())
            .or_default()
            .open(source, owner.clone())
        {
            tracing::warn!(%error, "the composer did not open");
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
        // Asked once: the composer keeps what it is about, not the question.
        // A press the palette made carries the palette's question.
        let ask = seed
            .ask
            .take()
            .or_else(|| self.selection_requests.take_asking());
        let draft = seed.draft.take();
        let label = if agent == NEW_TASK_AGENT {
            crate::shell::i18n::t("agent-quick-new-agent-addressed")
        } else {
            agent.clone()
        };
        self.agent_quick_composer.open_about(
            owner,
            &label,
            &AgentQuickComposerOrigin {
                marker: at,
                protected,
                context: String::new(),
            },
            viewport,
            seed,
        );
        self.agent_quick_composer.offer_agents(choices);
        let blocker = (agent == NEW_TASK_AGENT)
            .then(|| {
                crate::shell::search::providers::ask_agent::task_agent_blocker(&chrome.borrow())
            })
            .flatten();
        self.agent_quick_composer.block(blocker);
        if blocker.is_some() {
            // What was asked waits in the field until asking is possible.
            if let Some(text) = ask.or(draft) {
                self.agent_quick_composer.begin_typing(&text);
            }
        } else if let Some(text) = ask {
            self.agent_quick_composer.ask(&text);
        } else if let Some(text) = draft {
            self.agent_quick_composer.begin_typing(&text);
        }
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

    /// The person pressed the composer's fix for what keeps a new agent from
    /// being asked: open its place, as the palette's row does. Whether one
    /// was pressed.
    pub(super) fn fix_composer_blocker(&mut self) -> bool {
        // Set up in place: once the provider settings change, ask again
        // whether a new agent can be asked, so the composer unblocks.
        if self.agent_quick_composer.block_stale()
            && let Some(chrome) = self.chrome.as_ref()
        {
            let blocker =
                crate::shell::search::providers::ask_agent::task_agent_blocker(&chrome.borrow());
            self.agent_quick_composer.block(blocker);
            return true;
        }
        let Some(blocker) = self.agent_quick_composer.take_fix() else {
            return false;
        };
        if let Some(chrome) = self.chrome.as_ref()
            && let Err(error) =
                crate::shell::search::providers::ask_agent::open_fix(blocker, chrome)
        {
            tracing::warn!(?blocker, ?error, "the composer's fix could not be opened");
        }
        true
    }

    /// Open the chats the composer asked for, each in a window of its own:
    /// the desktop's `open` of the chat's address, run here as the person at
    /// this device, as the palette's window actions are. A chat that cannot
    /// open says why in the composer.
    pub(super) fn open_composer_chats(&mut self) -> bool {
        let chats: Vec<_> = self
            .agent_actions
            .values_mut()
            .flat_map(super::agent_actions::AgentActions::take_chats)
            .collect();
        if chats.is_empty() {
            return false;
        }
        for (room, address) in chats {
            let opened = (|| {
                let room = crate::shell::state::RoomId::parse(&room)
                    .ok_or("the chat's workspace is gone")?;
                let target = crate::shell::desktop_layout::Target::parse(&address)?;
                let actor = {
                    let chrome = self
                        .chrome
                        .as_ref()
                        .ok_or("this session shows no desktop")?;
                    crate::shell::desktop_layout::Actor::person(&chrome.borrow(), room)
                };
                let command = crate::shell::desktop_door::Command::Open {
                    target,
                    place: crate::shell::desktop_layout::Place::NewWindow { strip: None },
                    background: false,
                };
                match self.serve_desktop(room, &actor, command) {
                    crate::shell::desktop_door::Served::WhenUp { .. } => Ok(()),
                    crate::shell::desktop_door::Served::Now(Ok(_)) => Ok(()),
                    crate::shell::desktop_door::Served::Now(Err(failure)) => Err(failure.message),
                }
            })();
            if let Err(error) = opened {
                tracing::warn!(%address, %error, "the chat could not be opened");
                self.agent_quick_composer.tell(error);
            }
        }
        true
    }

    /// Launch the agents made for tasks that now exist, each under its own
    /// key in its task's workspace. A launch that fails is said in the
    /// composer. Whether any was asked.
    pub(super) fn launch_task_agents(&mut self) -> bool {
        let ends = std::mem::take(self.task_agent_ends.get_mut());
        let ended = !ends.is_empty();
        for why in ends {
            self.agent_quick_composer.tell(crate::shell::i18n::t_args(
                "agent-quick-task-agent-stopped",
                &[("reason", why.into())],
            ));
        }
        let launches: Vec<_> = self
            .agent_actions
            .values_mut()
            .flat_map(super::agent_actions::AgentActions::take_launches)
            .collect();
        if launches.is_empty() {
            return ended;
        }
        for (room, minted) in launches {
            let workspace = self.chrome.as_ref().and_then(|chrome| {
                chrome
                    .borrow()
                    .tree()
                    .desktop
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.room().to_string() == room)
                    .cloned()
            });
            let outcome = workspace
                .ok_or_else(|| "the task's workspace is gone".to_owned())
                .and_then(|workspace| minted.launch(&workspace));
            match outcome {
                Ok(()) => {
                    tracing::info!(agent = %minted.agent_id, "launched the agent made for the task");
                }
                Err(error) => {
                    tracing::warn!(agent = %minted.agent_id, %error, "the agent made for the task could not be launched");
                    self.agent_quick_composer.tell(error);
                }
            }
        }
        true
    }

    /// Ask agents the work the chrome handed them (a notification, a query
    /// from search), each as an issue in its workspace's Tasks, and open the
    /// issue's page when the ask says so. An agent that is not running is
    /// started, as the ask-agent chord starts it; the issue waits in the
    /// tracker until it takes it. Whether one was asked.
    pub(super) fn pump_agent_asks(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        // The palette's question about what is focused: the chord's way to
        // the focused selection, already asked. The last one asked wins.
        // Taken first: the chrome is borrowed only for the taking, not while it is bumped.
        let asked_in_place = chrome.borrow_mut().take_agent_asks_in_place().pop();
        if let Some(query) = asked_in_place {
            self.selection_requests.press_asking(query);
            self.bump_chrome();
        }
        let asks = chrome.borrow_mut().take_agent_asks();
        let mut asked = false;
        let mut starts = Vec::new();
        for ask in asks {
            let Some(broker) = self.shortcut_triggers.broker() else {
                break;
            };
            let room = ask.room.to_string();
            let live = askable(&broker, &room, &ask.agent);
            let source = ActionSessionSource::new(broker);
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
            let actions = self.agent_actions.entry(owner.clone()).or_default();
            if !live && actions.start_once(Instant::now()) {
                starts.push((ask.room, owner.agent.clone()));
            }
            if actions
                .open_and_ask(
                    source,
                    &owner,
                    (&ask.title, &ask.body),
                    None,
                    ask.show,
                    ask.quick,
                )
                .is_some()
            {
                asked = true;
            } else {
                tracing::warn!(agent = %owner.agent, "work could not be handed to the agent");
            }
        }
        for (room, agent) in starts {
            self.start_agent_for(&room, &agent);
        }
        asked
    }

    /// Start `agent` in the workspace of `room`, for work asked of it while
    /// it was not running. Only someone who may administer the workspace
    /// starts its agents; for anyone else the work waits for it.
    pub(super) fn start_agent_for(&self, room: &crate::shell::state::RoomId, agent: &str) {
        let Some(workspace) = self.chrome.as_ref().and_then(|chrome| {
            chrome
                .borrow()
                .tree()
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room() == *room)
                .cloned()
        }) else {
            return;
        };
        match crate::shell::launcher::launch_workspace_agent(agent, &workspace) {
            Ok(_) => tracing::info!(%agent, "started the agent to take the work asked of it"),
            Err(error) => {
                tracing::info!(%agent, error = %error.user_message(), "the agent asked for work could not be started");
            }
        }
    }

    /// The session source for the workspace, and those of its collaborator
    /// `members` that are admitted (certified under their own key) and hold
    /// a live task subscription, so a task assigned to them reaches them.
    pub(super) fn live_agents(
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

    /// A development session's test control asks to activate the control
    /// `test_id` in the focused window.
    pub(crate) fn queue_test_activation(&mut self, test_id: String) {
        self.test_activations.push(test_id);
    }

    /// The test ids queued to activate, with the focused app whose windows
    /// are asked; nothing while no app window is focused.
    pub(crate) fn take_test_activations(&mut self) -> Option<(String, Vec<String>)> {
        if self.test_activations.is_empty() {
            return None;
        }
        let activations = std::mem::take(&mut self.test_activations);
        let Some(app) = self.focused_app_id() else {
            tracing::warn!(
                ?activations,
                "a test control asked to activate controls while no app window is focused"
            );
            return None;
        };
        Some((app, activations))
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

/// Whether the collaborator `agent` takes asks in `room` now: admitted
/// (certified under its own key) and holding a live task subscription. A new
/// agent made for a task needs neither: it is launched for its task.
pub(super) fn askable(broker: &AppActionBroker, room: &str, agent: &str) -> bool {
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

/// The active workspace's room, the person in it, and its agent members.
fn active_room_agents(
    state: &crate::shell::chrome_state::ChromeState,
) -> Option<(String, crate::shell::state::UserId, Vec<String>)> {
    use crate::shell::state::WorkspaceAuthority as _;
    let tree = state.tree();
    let workspace = tree.desktop.active_workspace()?;
    let room = workspace.room().to_string();
    let members = workspace
        .agent_members()
        .into_iter()
        .map(|(agent, _)| agent)
        .collect();
    let person = state.active_local_user_in(&tree);
    drop(tree);
    Some((room, person, members))
}
