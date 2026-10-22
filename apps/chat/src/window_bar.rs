//! Chat's toolbar, as slots in the window's bar ([`ui::window_toolbar`]).
//!
//! The bar shows the channels' toggle, the conversation (a channel, a person
//! or an agent's task) as the window's location, and what can be done to it:
//! stopping a shared view, floating an agent's conversation, the call in
//! progress and leaving it, the pinned messages and the people; the host's
//! model commands and creating a channel in its menu. Search is the bar's
//! own. Each slot runs the window's own [`Hit`], as a press on the control
//! the conversation used to draw did.

use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar};

use crate::app::{ChatApp, Hit};

impl ChatApp {
    fn window_bar_actions(&self) -> Vec<(ToolbarAction, Hit)> {
        let mut actions = Vec::new();
        if self.sharing_context {
            actions.push((
                ToolbarAction::new(
                    "stop-sharing",
                    "eye-off",
                    crate::i18n::t("chat-context-stop"),
                ),
                Hit::StopSharingLiveContext,
            ));
        }
        if self.direct_person.is_some() || self.direct_agent.is_some() {
            let label = crate::i18n::t(if self.conversation_surface {
                "chat-agent-return"
            } else {
                "chat-agent-float"
            });
            actions.push((
                ToolbarAction::new("agent-window", "external-link", label),
                Hit::AgentWindow,
            ));
        }
        if self.direct_person.is_none() && self.has_other_collaborator() {
            if self.direct_agent.is_none() {
                if self.call.active {
                    let name = self.channel(&self.call.channel).map_or_else(
                        || crate::i18n::t("chat-header-call-open"),
                        |channel| channel.name.clone(),
                    );
                    actions.push((
                        ToolbarAction::new("call", "volume-2", name).on(true),
                        Hit::StartCall,
                    ));
                    actions.push((
                        ToolbarAction::new(
                            "call-leave",
                            "phone-off",
                            crate::i18n::t("chat-call-leave-action"),
                        ),
                        Hit::CallLeave,
                    ));
                }
                let pins = self.pins_open && self.right_flap_open() && self.thread_open.is_none();
                actions.push((
                    ToolbarAction::new("pins", "pin", "Pinned messages").on(pins),
                    Hit::TogglePins,
                ));
            }
            let people = self.right_flap_open() && self.thread_open.is_none() && !self.pins_open;
            actions.push((
                ToolbarAction::new("people", "users", self.roster_label()).on(people),
                Hit::ToggleRightFlap,
            ));
        }
        if self.direct_person.is_none() {
            for command in self.model_controls(&self.active_channel) {
                actions.push((
                    ToolbarAction::new(
                        format!("model:{command}"),
                        "sparkles",
                        crate::i18n::t(&format!("chat-model-{command}")),
                    )
                    .in_menu(),
                    Hit::HostModel(command.into()),
                ));
            }
        }
        if self.keeps_layout() && self.direct_person.is_none() && self.direct_agent.is_none() {
            actions.push((
                ToolbarAction::new("create", "plus", "Create channel or category").in_menu(),
                Hit::AddCategory,
            ));
        }
        actions
    }

    /// The window bar's slots for the conversation shown.
    fn window_toolbar(&self) -> WindowToolbar {
        let identity = self.header_identity_text();
        WindowToolbar {
            // A thread pushed over a narrow window is the whole page: its
            // own Back leads it.
            sidebar: (!self.thread_is_page()).then(|| self.left_flap_open()),
            location: Some(ToolbarLocation {
                label: identity.name,
                ..ToolbarLocation::default()
            }),
            actions: self
                .window_bar_actions()
                .into_iter()
                .map(|(action, _)| action)
                .collect(),
            ..WindowToolbar::default()
        }
    }

    pub(crate) fn declare_window_toolbar(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    /// Run what the person did in the window's bar as the window's own hits.
    pub(crate) fn apply_window_toolbar(&mut self, doc: &mut ui::widget::Edits<'_>) {
        for event in ui::window_toolbar::take_events() {
            let hit = match event {
                ToolbarEvent::ToggleSidebar => Hit::ToggleLeftFlap,
                ToolbarEvent::Action { id } => {
                    let Some((_, hit)) = self
                        .window_bar_actions()
                        .into_iter()
                        .find(|(action, _)| action.id == id)
                    else {
                        continue;
                    };
                    hit
                }
                _ => continue,
            };
            self.on_hit(hit, doc);
            self.canonical_dirty = true;
        }
    }
}
