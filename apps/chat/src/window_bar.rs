//! Chat's toolbar, as parts in the window's bar ([`ui::window_toolbar`]).
//!
//! The bar shows the shell's sidebar toggle, the conversation (a channel, a
//! person or an agent's task) as the window's location, and what can be done
//! to it: stopping a shared view, the call in progress, its microphone and
//! leaving it, the pinned messages and the people; the host's
//! model commands and creating a channel in its menu. A conversation with an
//! agent adds the model its answers come from: every model of every provider the
//! server keeps, changed mid-session through the workspace's agents resource, with what the conversation's answers cost so far. Search is
//! the bar's own. Each part dispatches the window's own [`Hit`], as a press on the
//! control the conversation used to draw did. A thread that fills the window
//! is the window's location, with Back to the conversation.

use ui::framework::BarCommands;
use ui::window_toolbar::{
    LocationAvatar, LocationPresence, ToolbarAction, ToolbarEvent, ToolbarLocation,
    ToolbarNavigation, WindowToolbar,
};

use ui::tilo_resource::Id;
use ui::tilo_workspace_agent::{Agent, WorkspaceAgents, workspace_agents};

use crate::app::{ChatApp, Hit};
use crate::model::{ChannelKind, Presence};

/// The document the workspace keeps its agents in, as the window opens it.
fn agents_document() -> String {
    ui::resource::workspace::document_id(ui::tilo_workspace_agent::KIND)
}

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
        // The model every answer of an agent conversation comes from, across
        // every provider the server keeps: changed here, mid-session,
        // through the workspace's own agents resource.
        if let Some(agent) = self.agent_in_view() {
            actions.push(self.agent_model_action(&agent));
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

    /// What the window is called outside it (the shell's tab, the switcher, History): its
    /// location as Chat writes it, a channel with its "#" ("#design-crit"), which the bar draws
    /// as an icon beside the bare name.
    pub(crate) fn window_name(&self) -> String {
        let name = self.location_name();
        let shown = if self.call.active && self.call.expanded {
            &self.call.channel
        } else {
            &self.active_channel
        };
        let channel = !self.thread_is_page()
            && self
                .channel(shown)
                .is_some_and(|channel| !channel.direct && channel.name == name);
        if channel && !name.is_empty() && !name.starts_with('#') {
            format!("#{name}")
        } else {
            name
        }
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
        if let Some(members) = self.active_direct_members() {
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
        let icon = match self.channel(&self.active_channel) {
            Some(channel) if channel.kind == ChannelKind::Voice => "volume-2",
            Some(_) => "hash",
            None => return (None, None),
        };
        (Some(icon.to_owned()), None)
    }

    pub(crate) fn declare_window_toolbar(&self) {
        // A view shown alone is all the window shows: nothing of Chat stands around it, neither
        // its parts in the window's bar nor its places beside the view.
        let alone = self.alone.is_some();
        if let Some(bar) = &self.bar {
            bar.show_content_alone(alone);
        }
        ui::window_toolbar::declare((!alone).then(|| self.window_toolbar()));
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

/// The model switcher of the conversation with an agent, and what changing
/// it means for the workspace's agents resource.
impl ChatApp {
    /// The agent the conversation in view is with: one direct conversation
    /// with exactly one agent.
    pub(crate) fn conversation_agent(&self) -> Option<String> {
        let members = self.active_direct_members()?;
        self.conversation_peer(&members)
            .filter(ui::tilo_resource::MemberId::is_agent)
            .map(|peer| peer.id().to_owned())
    }

    /// The agent `id` of the workspace's agents resource, once this
    /// window holds it: what answers the conversation, who added it and its
    /// model.
    pub(crate) fn agent(&self, id: &str) -> Option<Agent> {
        let agents = self.agents.as_ref()?.open(&agents_document())?;
        agents
            .get()
            .agents
            .find(&Id::from_raw(id.to_owned()))
            .cloned()
    }

    /// The agent whose conversation is in view, once the workspace's agents
    /// resource reached this window.
    fn agent_in_view(&self) -> Option<Agent> {
        let id = self.conversation_agent()?;
        self.agent(&id)
    }

    /// The switcher: the model the agent's answers answer on, every model of
    /// every provider the server keeps, and — a conversation is one
    /// session — what its answers cost so far, which the menu shows under the
    /// model as its own line.
    fn agent_model_action(&self, agent: &Agent) -> ToolbarAction {
        self.models.poll();
        let chosen = agent.model.as_deref();
        let label = self
            .models
            .label(chosen)
            .or_else(|| chosen.map(str::to_owned))
            .unwrap_or_else(|| crate::i18n::t("chat-agent-model"));
        // The model is the workspace's choice for its agent: whoever the workspace's grants let
        // change its agents chooses it.
        let may_choose = self
            .agents
            .as_ref()
            .and_then(|agents| agents.open(&agents_document()))
            .is_some_and(|agents| agents.may("set-agent-model"));
        let mut items = Vec::new();
        if may_choose {
            let current = chosen
                .and_then(|named| self.models.find(named))
                .or_else(|| {
                    if chosen.is_none() {
                        self.models.default_model()
                    } else {
                        None
                    }
                })
                .map(|found| found.value);
            for choice in self.models.choices().unwrap_or_default() {
                // The provider is the row's own value, never joined into the
                // model's name.
                let mut item = self
                    .bar_entries
                    .entry(
                        "sparkles",
                        choice.label,
                        Hit::AgentModel(Some(choice.value.clone())),
                    )
                    .with_detail(choice.provider);
                if current.as_deref() == Some(choice.value.as_str()) {
                    item = item.on(true);
                }
                items.push(item);
            }
            if chosen.is_some() {
                items.push(self.bar_entries.entry(
                    "rotate-ccw",
                    crate::i18n::t("chat-agent-model-default"),
                    Hit::AgentModel(None),
                ));
            }
        }
        if !self.models.has_choices() {
            items.push(self.bar_entries.entry(
                "settings",
                crate::i18n::t("chat-agent-model-settings"),
                Hit::OpenModelSettings,
            ));
        }
        let enabled = !items.is_empty();
        let action = ToolbarAction::new("chat-agent-model", "sparkles", label)
            .with_items(items)
            .labelled()
            .enabled(enabled);
        match self.session_cost_micros() {
            0 => action,
            cost => action.with_detail(crate::i18n::t_args(
                "chat-agent-model-cost",
                &[("cost", ui::format::money(cost).into())],
            )),
        }
    }

    /// What the answers in the conversation in view cost so far, as each one
    /// recorded it: the session's own total, which stays right across a
    /// model switch.
    pub(crate) fn session_cost_micros(&self) -> u64 {
        self.messages
            .iter()
            .filter_map(|message| message.work.as_ref()?.answered.as_ref())
            .map(|answered| answered.cost_micros)
            .fold(0, u64::saturating_add)
    }

    /// Choose the model the agent of the conversation in view answers with,
    /// through the workspace's agents resource: `None` follows the
    /// workspace's default again.
    pub(crate) fn choose_agent_model(&self, model: Option<String>) {
        let Some(agent) = self.conversation_agent() else {
            return;
        };
        let Some(agents) = self
            .agents
            .as_ref()
            .and_then(|agents| agents.open(&agents_document()))
        else {
            return;
        };
        let _ = agents.invoke(&WorkspaceAgents::SET_AGENT_MODEL.with(
            workspace_agents::SetAgentModel {
                agent: Id::from_raw(agent),
                model,
            },
        ));
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use tilo_actions::remote::person_models::{
        ModelProviders, OfferedModel, Protocol, ProviderView,
    };
    use ui::framework::testing::TestApp;
    use ui::tilo_resource::NonBlank;
    use ui::tilo_workspace_agent::workspace_agents::CreateAgent;

    use super::*;
    use crate::testing::MountedChat;

    /// The workspace the window is in.
    const WORKSPACE: &str = "6f0c3a52-8e1b-4d6a-9c2f-3b7e5d1a4c90";

    /// One provider, as the server lists it, with `models` as `(id, label)`.
    fn provider(
        id: &str,
        name: &str,
        default: Option<&str>,
        models: &[(&str, &str)],
    ) -> ProviderView {
        ProviderView {
            id: id.to_owned(),
            name: name.to_owned(),
            protocol: Protocol::default(),
            endpoint: "https://models.example.com/v1".to_owned(),
            configured: true,
            signs_in: false,
            models: models
                .iter()
                .map(|(id, label)| OfferedModel {
                    id: (*id).to_owned(),
                    label: (*label).to_owned(),
                })
                .collect(),
            default_model: default.map(str::to_owned),
            reasoning: None,
            headers: Vec::new(),
        }
    }

    /// A window on the conversation with an agent the person keeps, and the
    /// providers the server keeps beside it.
    fn window_with_agent() -> TestApp<crate::ChatShell> {
        ui::i18n::I18nManager::init("en-US");
        let mut window = TestApp::<crate::ChatShell>::in_workspace(WORKSPACE).expect("a window");
        window.frame();
        // The server's listing, as it answered before the conversation opened: a test asks no
        // server for one.
        {
            let mut chat = window.chat();
            chat.models = crate::model_choice::ModelChoices::answered(ModelProviders {
                providers: vec![
                    provider(
                        "chatgpt",
                        "ChatGPT",
                        Some("gpt-5.5"),
                        &[("gpt-5.5", "GPT-5.5")],
                    ),
                    provider(
                        "anthropic",
                        "Anthropic",
                        Some("sonnet"),
                        &[("sonnet", "Sonnet")],
                    ),
                ],
                default_provider: Some("chatgpt".to_owned()),
                assignments: Vec::new(),
            });
        }
        let agents = window
            .shared_workspace::<WorkspaceAgents>(ui::tilo_workspace_agent::OWNER)
            .expect("the workspace's agents");
        agents
            .invoke(&WorkspaceAgents::CREATE_AGENT.with(CreateAgent {
                name: NonBlank::parse("Assistant").expect("a name"),
                id: Some("assistant".to_owned()),
                model: None,
                mcp_client: None,
            }))
            .expect("the agent is made");
        window.frame();
        window
            .call_on(
                "chat.workspace",
                "open-agent",
                serde_json::json!({ "agent": "assistant" }),
            )
            .expect("the conversation with the agent opens");
        window.settle();
        window
    }

    #[test]
    fn an_agent_dm_with_no_configured_models_offers_provider_settings() {
        let mut window = window_with_agent();
        let mut chat = window.chat();
        chat.models = crate::model_choice::ModelChoices::answered(ModelProviders {
            providers: Vec::new(),
            default_provider: None,
            assignments: Vec::new(),
        });
        let picker = chat
            .window_bar_actions()
            .into_iter()
            .find(|action| action.id == "chat-agent-model")
            .unwrap();
        let settings = picker
            .items
            .iter()
            .find(|item| item.name == "Connect a model provider")
            .expect("provider setup");
        assert_eq!(
            chat.bar_entries.pressed(&ToolbarEvent::Action {
                id: settings.id.clone()
            }),
            Some(Hit::OpenModelSettings)
        );
    }

    /// The model an agent conversation answers on, as the window bar offers it:
    /// every model of every provider the server keeps, the one in use
    /// marked, and each choice its own command.
    #[test]
    fn the_conversation_with_an_agent_offers_every_model_and_chooses_one_through_its_own_action() {
        let mut window = window_with_agent();
        let chat = window.chat();
        let action = chat
            .window_bar_actions()
            .into_iter()
            .find(|action| action.id == "chat-agent-model")
            .expect("the switcher is in the conversation's header");
        assert_eq!(action.name, "GPT-5.5", "the model it answers on");
        assert_eq!(
            action.detail, "",
            "a conversation that has cost nothing says nothing"
        );
        let names: Vec<&str> = action.items.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names, ["GPT-5.5", "Sonnet"]);
        let providers: Vec<&str> = action
            .items
            .iter()
            .map(|item| item.detail.as_str())
            .collect();
        assert_eq!(providers, ["ChatGPT", "Anthropic"], "each row its provider");
        let used = action
            .items
            .iter()
            .find(|item| item.on)
            .expect("the model in use is marked");
        assert_eq!(used.name, "GPT-5.5");

        // A choice's press is the window's own command, which changes the
        // agent's model through the resource's action.
        let sonnet = action
            .items
            .iter()
            .find(|item| item.name == "Sonnet")
            .expect("the other model");
        assert_eq!(
            chat.bar_entries.pressed(&ToolbarEvent::Action {
                id: sonnet.id.clone(),
            }),
            Some(Hit::AgentModel(Some("anthropic/sonnet".to_owned())))
        );
        chat.choose_agent_model(Some("anthropic/sonnet".to_owned()));
        drop(chat);
        window.frame();
        window.settle();
        let agents = window
            .shared_workspace::<WorkspaceAgents>(ui::tilo_workspace_agent::OWNER)
            .expect("the workspace's agents");
        let agent = agents
            .get()
            .agents
            .find(&Id::from_raw("assistant".to_owned()))
            .cloned()
            .expect("the agent");
        assert_eq!(agent.model.as_deref(), Some("anthropic/sonnet"));
    }

    /// A conversation that is no agent's offers no model: a channel, and a
    /// conversation with a person, never name one.
    #[test]
    fn only_a_conversation_with_an_agent_offers_a_model() {
        let mut window = window_with_agent();
        window
            .chat()
            .answer_effect(crate::effects::ChatEffect::OpenChannel {
                channel: "c_chat".to_owned(),
            })
            .expect("a channel opens");
        window.settle();
        let chat = window.chat();
        assert!(
            !chat
                .window_bar_actions()
                .iter()
                .any(|action| action.id == "chat-agent-model"),
            "a channel uses no agent's model"
        );
    }
}
