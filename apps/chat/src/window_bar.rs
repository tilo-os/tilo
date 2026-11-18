//! Chat's toolbar, as parts in the window's bar ([`ui::window_toolbar`]).
//!
//! The bar shows the shell's sidebar toggle, the conversation (a channel, a
//! person or an agent's task) as the window's location, and what can be done
//! to it: stopping a shared view, the call in progress, its microphone and
//! leaving it, the pinned messages and the people; the host's
//! model commands and creating a channel in its menu. Search is the bar's
//! own. Each part runs the window's own [`Hit`], as a press on the control
//! the conversation used to draw did. A thread that fills the window is the
//! window's location, with Back to the conversation.

use ui::framework::BarCommands;
use ui::window_toolbar::{
    LocationAvatar, LocationPresence, ToolbarAction, ToolbarEvent, ToolbarLocation,
    ToolbarNavigation, WindowToolbar,
};

use crate::app::{ChatApp, Hit};
use crate::model::{ChannelKind, Presence};

/// The bar's entries, each carrying the window's own hit.
pub(crate) type BarEntries = BarCommands<Hit>;

impl ChatApp {
    fn window_bar_actions(&self) -> Vec<ToolbarAction> {
        let mut actions = Vec::new();
        if self.sharing_context {
            actions.push(self.bar_entries.entry(
                "eye-off",
                crate::i18n::t("chat-context-stop"),
                Hit::StopSharingLiveContext,
            ));
        }
        // The call in progress, its microphone and leaving it, whatever the
        // window shows and whoever else is in the workspace: someone alone in a
        // call, or in a workspace where nobody else has written yet, still mutes
        // and leaves it.
        if self.call.active {
            let name = self.channel(&self.call.channel).map_or_else(
                || crate::i18n::t("chat-header-call-open"),
                |channel| channel.name.clone(),
            );
            actions.push(
                self.bar_entries
                    .entry("volume-2", name, Hit::StartCall)
                    .on(true),
            );
            // The microphone stays at hand wherever the call is.
            let (icon, label) = if self.microphone_missing() {
                ("mic-off", "chat-call-no-microphone")
            } else if self.call.muted {
                ("mic-off", "chat-call-unmute")
            } else {
                ("mic", "chat-call-mute")
            };
            actions.push(
                self.bar_entries
                    .entry(icon, crate::i18n::t(label), Hit::CallMute)
                    .on(self.call.muted),
            );
            // Hearing the call, beside speaking into it: in the menu, so
            // the row keeps the call, its microphone and Leave.
            let (icon, label) = if self.call.deafened {
                ("headphone-off", "chat-call-undeafen")
            } else {
                ("headphones", "chat-call-deafen")
            };
            actions.push(
                self.bar_entries
                    .entry(icon, crate::i18n::t(label), Hit::CallDeafen)
                    .on(self.call.deafened)
                    .in_menu(),
            );
            actions.push(
                self.bar_entries
                    .entry(
                        "phone-off",
                        crate::i18n::t("chat-call-leave-action"),
                        Hit::CallLeave,
                    )
                    .destructive(),
            );
        }
        if self.direct_person().is_none() && self.has_other_member() {
            let pins = self.pins_open && self.right_flap_open() && self.thread_open.is_none();
            actions.push(
                self.bar_entries
                    .entry("pin", crate::i18n::t("chat-pins"), Hit::TogglePins)
                    .on(pins),
            );
            let people = self.right_flap_open() && self.thread_open.is_none() && !self.pins_open;
            actions.push(
                self.bar_entries
                    .entry("users", self.roster_label(), Hit::ToggleRightFlap)
                    .on(people),
            );
        }
        if self.direct_person().is_none() {
            for command in self.model_controls(&self.active_channel) {
                actions.push(
                    self.bar_entries
                        .entry(
                            "sparkles",
                            crate::i18n::t(&format!("chat-model-{command}")),
                            Hit::ModelStream(command.into()),
                        )
                        .in_menu(),
                );
            }
        }
        if self.keeps_layout() && self.direct_person().is_none() {
            actions.push(
                self.bar_entries
                    .entry("plus", crate::i18n::t("chat-create-menu"), Hit::AddCategory)
                    .in_menu(),
            );
        }
        actions
    }

    /// What the window shows, as it is named: the thread that fills it, the call's stage, or the
    /// conversation in the sidebar.
    pub(crate) fn location_name(&self) -> String {
        if self.thread_is_page() {
            return self.thread_title();
        }
        if self.call.active
            && self.call.expanded
            && let Some(channel) = self.channel(&self.call.channel)
        {
            return channel.name.clone();
        }
        self.header_identity_text().name
    }

    /// The window bar's parts for the conversation shown, starting from the
    /// shell's own.
    fn window_toolbar(&self) -> WindowToolbar {
        let mut bar = self
            .bar
            .as_ref()
            .map(ui::widget::WindowBarLink::parts)
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
        let staged = page
            || (self.call.active
                && self.call.expanded
                && self.channel(&self.call.channel).is_some());
        let label = self.location_name();
        // Who or what the conversation is leads its name: the face of the person or agent a
        // direct one is with, and whether they are here; a channel's mark.
        let (icon, avatar) = if staged {
            (None, None)
        } else {
            self.location_lead()
        };
        bar.location = Some(ToolbarLocation {
            label,
            icon,
            avatar,
            ..ToolbarLocation::default()
        });
        bar.actions = self.window_bar_actions();
        bar
    }

    /// What leads the conversation's name in the bar: the face of the one person or agent a
    /// direct conversation is with, else the channel's icon.
    pub(crate) fn location_lead(&self) -> (Option<String>, Option<LocationAvatar>) {
        let channel = self.channel(&self.active_channel);
        // A direct conversation not listed yet (no message in it) is named by its id.
        let members = match channel {
            Some(channel) => channel.direct.then(|| channel.members.clone()),
            None => crate::chat::Chat::direct_members(&ui::tilo_resource::Id::from_raw(
                self.active_channel.clone(),
            )),
        };
        if let Some(members) = members {
            let Some(other) = self.conversation_peer(&members) else {
                return (Some("users".to_owned()), None);
            };
            let presence = self.user(&other).map(|user| match self.live_status(user) {
                Presence::Online => LocationPresence::Online,
                Presence::Idle => LocationPresence::Away,
                Presence::Dnd => LocationPresence::Busy,
                Presence::Offline => LocationPresence::Offline,
            });
            let avatar = LocationAvatar {
                name: self.display_name(&other),
                hue: Some(self.member_hue(&other)),
                identity: other.id().to_owned(),
                agent: !other.is_person(),
                presence,
            };
            return (None, Some(avatar));
        }
        let icon = match channel {
            Some(channel) if channel.kind == ChannelKind::Voice => "volume-2",
            Some(_) => "hash",
            None => return (None, None),
        };
        (Some(icon.to_owned()), None)
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
            let Some(hit) = self.bar_entries.pressed(&event) else {
                continue;
            };
            self.on_hit(hit, doc);
            self.canonical_dirty = true;
        }
    }
}
