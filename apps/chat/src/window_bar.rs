//! Chat's toolbar, as slots in the window's bar ([`ui::window_toolbar`]).
//!
//! The bar shows the shell's sidebar toggle, the conversation (a channel, a
//! person or an agent's task) as the window's location, and what can be done
//! to it: stopping a shared view, floating an agent's conversation, the call
//! in progress, its microphone and leaving it, the pinned messages and the
//! people; the host's
//! model commands and creating a channel in its menu. Search is the bar's
//! own. Each slot runs the window's own [`Hit`], as a press on the control
//! the conversation used to draw did. A thread that fills the window is the
//! window's location, with Back to the conversation.

use ui::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, ToolbarNavigation, WindowToolbar,
};

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
        if self.direct_person.is_some() {
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
        // The call in progress, its microphone and leaving it, whatever the
        // window shows and whoever else is in the room: someone alone in a
        // call, or in a room where nobody else has written yet, still mutes
        // and leaves it.
        if self.call.active {
            let name = self.channel(&self.call.channel).map_or_else(
                || crate::i18n::t("chat-header-call-open"),
                |channel| channel.name.clone(),
            );
            actions.push((
                ToolbarAction::new("call", "volume-2", name).on(true),
                Hit::StartCall,
            ));
            // The microphone stays at hand wherever the call is.
            let (icon, label) = if self.microphone_missing() {
                ("mic-off", "chat-call-no-microphone")
            } else if self.call.muted {
                ("mic-off", "chat-call-unmute")
            } else {
                ("mic", "chat-call-mute")
            };
            let label = crate::i18n::t(label);
            actions.push((ToolbarAction::new("call-mute", icon, label), Hit::CallMute));
            // Hearing the call, beside speaking into it: in the menu, so
            // the row keeps the call, its microphone and Leave.
            let (icon, label) = if self.call.deafened {
                ("headphone-off", "chat-call-undeafen")
            } else {
                ("headphones", "chat-call-deafen")
            };
            actions.push((
                ToolbarAction::new("call-deafen", icon, crate::i18n::t(label))
                    .on(self.call.deafened)
                    .in_menu(),
                Hit::CallDeafen,
            ));
            actions.push((
                ToolbarAction::new(
                    "call-leave",
                    "phone-off",
                    crate::i18n::t("chat-call-leave-action"),
                )
                .destructive(),
                Hit::CallLeave,
            ));
        }
        if self.direct_person.is_none() && self.has_other_collaborator() {
            let pins = self.pins_open && self.right_flap_open() && self.thread_open.is_none();
            actions.push((
                ToolbarAction::new("pins", "pin", crate::i18n::t("chat-pins")).on(pins),
                Hit::TogglePins,
            ));
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
        if self.keeps_layout() && self.direct_person.is_none() {
            actions.push((
                ToolbarAction::new("create", "plus", crate::i18n::t("chat-create-menu")).in_menu(),
                Hit::AddCategory,
            ));
        }
        actions
    }

    /// The window bar's slots for the conversation shown, starting from the
    /// shell's own.
    fn window_toolbar(&self) -> WindowToolbar {
        let identity = self.header_identity_text();
        let mut bar = self
            .bar
            .as_ref()
            .map(ui::widget::WindowBarLink::slots)
            .unwrap_or_default();
        let page = self.thread_is_page();
        if page {
            // A thread pushed over a narrow window is the whole page: Back
            // to the conversation leads it, and the thread is the location.
            bar.sidebar = None;
            bar.leading
                .retain(|action| action.id != ui::widget::WINDOW_BACK);
            bar.navigation = Some(ToolbarNavigation {
                back: true,
                forward: false,
            });
        }
        // The location is what the window shows: the call while its stage
        // fills the window, as the channels' chosen row says.
        let call = (self.call.active && self.call.expanded)
            .then(|| {
                self.channel(&self.call.channel)
                    .map(|channel| channel.name.clone())
            })
            .flatten();
        let label = if page {
            self.thread_title()
        } else {
            call.unwrap_or(identity.name)
        };
        bar.location = Some(ToolbarLocation {
            label,
            ..ToolbarLocation::default()
        });
        bar.actions = self
            .window_bar_actions()
            .into_iter()
            .map(|(action, _)| action)
            .collect();
        bar
    }

    pub(crate) fn declare_window_toolbar(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    /// Run what the person did in the window's bar as the window's own hits.
    pub(crate) fn apply_window_toolbar(&mut self, doc: &mut ui::widget::Edits<'_>) {
        for event in ui::window_toolbar::take_events() {
            // A thread that fills the window: its Back returns to the
            // conversation, not to the shell's screen behind it.
            if matches!(event, ToolbarEvent::Back) && self.thread_is_page() {
                self.on_hit(Hit::CloseThread, doc);
                self.canonical_dirty = true;
                continue;
            }
            if self.bar.as_ref().is_some_and(|bar| bar.event(&event)) {
                self.canonical_dirty = true;
                continue;
            }
            let hit = match event {
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
