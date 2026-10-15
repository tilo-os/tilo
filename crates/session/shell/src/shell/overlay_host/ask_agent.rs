//! The ask-agent chord and the "Ask agent…" requests Tilo apps send: both
//! open the agent quick composer on the focused window's selection
//! ([`crate::shell::selection_agent`]).

use std::time::Instant;

use ui::shared_selection::SharedSelection;
use vello::kurbo::{Point, Rect};

use super::ShellOverlayHost;
use crate::shell::agent_quick_composer::{AgentQuickComposerOrigin, ComposerOwner};
use crate::shell::certified_action_session::ActionSessionSource;
use crate::shell::selection_agent::{ChordRoute, SelectionSeed};

impl ShellOverlayHost {
    /// `app_id` on shell-ui `connection` answers selection requests.
    pub(crate) fn note_selection_capable(&mut self, connection: u64, app_id: &str) {
        self.selection_requests.capable(connection, app_id);
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
    /// `selection`: in answer to the chord, or from an "Ask agent…" entry
    /// in its focused window. Whether the composer opened.
    pub(crate) fn receive_ask_agent(
        &mut self,
        app_id: &str,
        selection: Option<SharedSelection>,
    ) -> bool {
        let focused = self.focused_app_id();
        if !self.selection_requests.accept(app_id, focused.as_deref()) {
            return false;
        }
        let label = self.app_label(app_id);
        self.open_ask_agent(SelectionSeed::new(app_id, label, selection))
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
            state.tree().desktop.active_workspace().map(|workspace| {
                (
                    workspace.room.to_string(),
                    state.active_local_user(),
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
            .live_agents(&room, members)
            .map_or((None, Vec::new()), |(source, live)| (Some(source), live));
        let chosen = self.selection_requests.choose_agent(&room, &live);
        let (Some(source), Some(agent)) = (source, chosen) else {
            chrome
                .borrow_mut()
                .show_osd(crate::shell::osd::Osd::no_agent());
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
        let window = self.focused_window_rect().unwrap_or(viewport);
        // Over the focused window, a third of the way down, where the
        // person was reading; the popover keeps itself inside the screen.
        let at = Rect::from_center_size(
            Point::new(window.center().x, window.y0 + window.height() / 3.0),
            (1.0, 1.0),
        );
        self.agent_quick_composer.open_about(
            owner,
            &agent,
            AgentQuickComposerOrigin {
                marker: at,
                protected: at,
                context: String::new(),
            },
            viewport,
            seed,
        );
        self.bump_chrome();
        true
    }

    /// The session source for the workspace, and those of its agent
    /// `members` that are admitted (certified under their own key) and hold
    /// a live task subscription, so a queued task reaches them.
    fn live_agents(
        &self,
        room: &str,
        members: Vec<String>,
    ) -> Option<(ActionSessionSource, Vec<String>)> {
        let broker = self.shortcut_triggers.broker()?;
        let live = members
            .into_iter()
            .filter(|agent| {
                broker.agent_is_live_task_capable(room, agent)
                    && broker
                        .certified_agent_identity(room, &format!("agent:{agent}"))
                        .is_some_and(|identity| &identity.agent_key == agent)
            })
            .collect();
        Some((ActionSessionSource::new(broker), live))
    }

    /// The focused tab's app id.
    fn focused_app_id(&self) -> Option<String> {
        self.chrome
            .as_ref()?
            .borrow()
            .active_app_id()
            .map(|app| app.as_str().to_owned())
    }

    /// What `app_id` is called, for the composer and the agent.
    fn app_label(&self, app_id: &str) -> String {
        crate::shell::launcher::display_name_for_app_id(app_id)
            .or_else(|| self.chrome.as_ref()?.borrow().active_app_title())
            .unwrap_or_else(|| app_id.to_owned())
    }
}
