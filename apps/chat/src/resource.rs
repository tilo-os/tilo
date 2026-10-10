//! Chat as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path. Composer drafts are rich-text child resources.
//!
//! Document mutations (create channel/category, rename/archive/delete channel,
//! post/edit/react/pin/delete message, …) live on this resource via
//! [`Resource::children`]. Hosted [`PublishesResources::resources`] returns
//! only live extras that are not document state — never a second hand-built
//! `chat.workspace` Document door beside [`tilo_resource::publish`].

use automerge::{ObjType, ROOT, ReadDoc, Value};
use tilo_content::ContentStore;
use tilo_document::RichText;
use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, LiveRow, ResourceApp, ResourceSurface, RunMode, SeedCx,
    SurfaceCx, ViewCx,
};
use ui::tilo_actions::{
    ActionAddress, ActionParam, PROMPTED_PARAM, ParamType, Resource as PublishedResource,
};
use ui::tilo_resource::{
    ActionDecl, Doc, Invoke, ModelError, Resource, SearchEntry, publish_child,
};
use ui::widget::Element;

use crate::actions::{self, CHAT_ACTION_TYPE, ChatAction, MessageResource, register};
use crate::app::{ChatApp, ChatRuntime};
use crate::doc::ChatDoc;
use crate::effects::ChatEffect;
use crate::identity::LocalIdentity;
use crate::model::{Category, Channel, User, Workspace};
use ui::tilo_resource::Effect as _;

/// The shared chat document as everyone reads it — the same state for every
/// participant; a view compares the authors it records with its own `me`.
#[derive(Clone, Debug, Default)]
pub struct ChatState {
    pub feedback: Vec<crate::feedback::Report>,
    pub workspaces: Vec<Workspace>,
    pub channels: Vec<Channel>,
    pub categories: Vec<Category>,
    /// Every channel's messages, oldest first: the tree publishes the
    /// latest, resolves any by id, and search indexes them all.
    pub channel_messages: Vec<(String, Vec<MessageResource>)>,
    /// Channel / thread keys that already have a rich-text draft body.
    pub draft_keys: Vec<String>,
    /// The roster: everyone the document names.
    pub users: Vec<User>,
}

impl ChatState {
    /// The messages of `channel`, oldest first.
    pub fn messages_of(&self, channel: &str) -> &[MessageResource] {
        self.channel_messages
            .iter()
            .find(|(id, _)| id == channel)
            .map_or(&[][..], |(_, messages)| messages.as_slice())
    }

    /// Every message of every channel.
    pub fn messages(&self) -> impl Iterator<Item = &MessageResource> {
        self.channel_messages
            .iter()
            .flat_map(|(_, messages)| messages.iter())
    }

    /// The message `id`, in whichever channel it is.
    pub fn message(&self, id: &str) -> Option<&MessageResource> {
        self.messages().find(|message| message.id == id)
    }

    /// Who a `post` may mention: the document's people.
    pub fn participants(&self) -> Vec<crate::mention::Participant> {
        self.users
            .iter()
            .map(|user| crate::mention::Participant {
                id: user.id.clone(),
                name: user.name.clone(),
                is_agent: user.is_agent,
            })
            .collect()
    }
}

/// The chat workspace resource — rooms, channels and nested draft bodies.
pub struct Chat;

impl Resource for Chat {
    const KIND: &'static str = "chat.workspace";
    const LABEL: &'static str = "Chat";
    const ACTION_TYPE: &'static str = CHAT_ACTION_TYPE;

    type State = ChatState;
    type Action = ChatAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<ChatState, ModelError> {
        let workspaces = ChatDoc::list_workspaces(doc);
        let channels = ChatDoc::list_channels(doc);
        let categories = ChatDoc::list_categories(doc);
        let users = ChatDoc::list_users(doc);
        let agents = ChatDoc::agent_authors(doc);
        let channel_messages = channels
            .iter()
            .map(|channel| {
                let messages = ChatDoc::list_messages(doc, &channel.id)
                    .into_iter()
                    .map(|message| {
                        message_resource(
                            message,
                            channel.conversation_with.as_deref(),
                            &users,
                            &agents,
                        )
                    })
                    .collect();
                (channel.id.clone(), messages)
            })
            .collect();
        // Every channel creation scaffolds an empty rich draft so first
        // keystrokes converge. Catalogue only drafts that already have text.
        let draft_keys = draft_keys(doc)
            .into_iter()
            .filter(|key| !ChatDoc::draft_text(doc, key).is_empty())
            .collect();
        Ok(ChatState {
            feedback: crate::feedback::reports(doc)
                .map_err(|error| ModelError::invalid(error.to_string()))?,
            workspaces,
            channels,
            categories,
            channel_messages,
            draft_keys,
            users,
        })
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        // Empty document: no demo people, messages or starter rooms. Shared
        // root maps are created by the creator's seed or the first action.
        let _ = doc;
        Ok(())
    }

    fn permission(action: &ChatAction) -> tilo_actions::PermissionRequirement {
        match action {
            ChatAction::Feedback(action) => action.permission(),
            _ => tilo_actions::PermissionRequirement::Write,
        }
    }

    fn least_permission() -> tilo_actions::PermissionRequirement {
        tilo_actions::PermissionRequirement::Comment
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        // Creating channels and categories is offered on each workspace.
        Vec::new()
    }

    fn children(state: &ChatState) -> Vec<PublishedResource> {
        let mut children = actions::publish_children(state);
        children.extend(state.feedback.iter().map(crate::feedback::publish));
        // Each composer draft is a rich-text child, edited as one.
        children.extend(state.draft_keys.iter().map(|key| {
            let owned = key.clone();
            publish_child::<RichText, Chat>(
                &crate::composer::draft_id(key),
                &format!("Draft in {key}"),
                None,
                move |edit| ChatAction::EditDraft {
                    key: owned.clone(),
                    edit,
                },
            )
        }));
        children
    }

    /// Starting a call rides the root once there is a conversation to call
    /// in, and launches Chat when it is closed (Home's "Plan a meeting", an
    /// agent). A chat holding nothing yet offers nothing to do in it.
    fn bound_actions(state: &ChatState) -> Vec<tilo_actions::ActionSpec> {
        if state.workspaces.is_empty() {
            return Vec::new();
        }
        vec![
            ChatEffect::StartCall.bound().launchable_when_closed(),
            ChatEffect::StartVideoCall
                .bound()
                .launchable_when_closed()
                .unavailable(
                    crate::effects::VIDEO_CALL_UNAVAILABLE,
                    crate::effects::video_call_unavailable_reason(),
                ),
        ]
    }

    fn child(state: &ChatState, id: &str) -> Option<PublishedResource> {
        state
            .feedback
            .iter()
            .find(|report| format!("chat.feedback:{}", report.submission.id) == id)
            .map(crate::feedback::publish)
            .or_else(|| actions::resolve_child(state, id))
    }

    fn search(state: &ChatState) -> Vec<SearchEntry> {
        let open = |resource: &str, action: &str| ActionAddress::new(resource, action);
        let workspace_name = |id: &str| {
            state
                .workspaces
                .iter()
                .find(|workspace| workspace.id == id)
                .map_or("", |workspace| workspace.name.as_str())
        };
        let workspaces = state.workspaces.iter().map(|workspace| {
            let id = format!("chat.workspace:{}", workspace.id);
            SearchEntry::new(&workspace.name, "Workspace")
                .invoking(open(&id, "open-workspace"))
                .anchored(id)
        });
        let channels = state.channels.iter().map(|channel| {
            let id = format!("chat.channel:{}", channel.id);
            SearchEntry::new(
                channel.name.clone(),
                format!("{} {}", channel.topic, workspace_name(&channel.workspace)),
            )
            .invoking(open(&id, "open-channel"))
            .anchored(id)
        });
        let categories = state.categories.iter().map(|category| {
            let id = format!("chat.category:{}", category.id);
            SearchEntry::new(&category.name, workspace_name(&category.workspace))
                .invoking(open(&id, "open-category"))
                .anchored(id)
        });
        let messages = state.messages().map(|message| {
            let id = format!("chat.message:{}", message.id);
            SearchEntry::new(&message.label, &message.text)
                .invoking(open(&id, "reveal-message"))
                .anchored(id)
        });
        let feedback = state.feedback.iter().map(|report| {
            let id = format!("chat.feedback:{}", report.submission.id);
            SearchEntry::new(
                &report.submission.feedback.text,
                workspace_name(&report.submission.destination.room),
            )
            .invoking(open(&id, "reveal-message"))
            .anchored(id)
        });
        let people = state.users.iter().map(|user| {
            let id = format!("chat.person:{}", user.id);
            SearchEntry::new(&user.name, "Person")
                .invoking(open(&id, "open-person"))
                .anchored(id)
        });
        workspaces
            .chain(channels)
            .chain(categories)
            .chain(messages)
            .chain(feedback)
            .chain(people)
            .collect()
    }
}

/// A message as everyone reads it.
fn message_resource(
    message: crate::model::Message,
    conversation_with: Option<&str>,
    users: &[User],
    agents: &std::collections::HashMap<String, String>,
) -> MessageResource {
    let label = actions::message_label_for_resource(
        &message.author,
        &message.text,
        message.attachment.as_ref(),
        users,
        agents,
    );
    MessageResource {
        agent: agents.contains_key(&message.author)
            || message
                .author
                .starts_with(tilo_actions::AGENT_TASK_PARTICIPANT_PREFIX)
            || users
                .iter()
                .any(|user| user.id == message.author && user.is_agent),
        label,
        channel: message.channel,
        attachment: message
            .attachment
            .as_ref()
            .map(|attachment| actions::AttachmentResource {
                name: attachment.name().to_owned(),
                blob: attachment.blob().to_owned(),
            }),
        reactions: message
            .reactions
            .iter()
            .map(|reaction| actions::ReactionResource {
                emoji: reaction.emoji.clone(),
                count: reaction.count(),
            })
            .collect(),
        id: message.id,
        author: message.author,
        text: message.text,
        pinned: message.pinned,
        parent: message.parent,
        conversation_with: conversation_with.map(str::to_owned),
        agent_intent: message.agent_intent,
        reference: message.reference,
        source_provider: message.source_provider,
    }
}

/// Chat as every host runs and mounts it: the chat widget is the surface
/// over [`Chat`], kept by the room as its own record.
pub struct ChatShell;

/// Every named scenario `--shots --scenario NAME` captures.
const SCENARIOS: &[&str] = &[
    "agent-lifecycle-before",
    "agent-lifecycle-before-compact",
    "agent-lifecycle-before-floating",
    "agent-message-refused",
    "agent-message-refused-compact",
    "agent-message-refused-floating",
    "agent-permission-revoked",
    "agent-permission-revoked-compact",
    "agent-permission-revoked-floating",
    "agent-membership-removed",
    "agent-membership-removed-compact",
    "agent-membership-removed-floating",
    "agent-direct",
    "agent-direct-compact",
    "agent-direction-saved",
    "agent-direction-saved-closed",
    "agent-direction-saved-compact",
    "agent-direction-saved-floating",
    "agent-stopping",
    "agent-stopping-compact",
    "agent-long-content",
    "agent-long-content-compact",
    "agent-long-content-floating",
    "agent-work",
    "narrow",
    "history",
    "compact",
    "starter",
    "people",
    "thread-narrow",
    "collaboration",
    "live-context",
    "context",
    "msgmenu",
    "attach",
    "attach-loading",
    "attach-error",
    "edit",
    "composer",
    "call",
    "share",
    "floating-call-attached",
    "floating-call",
    "floating-call-hover",
    "floating-call-returning",
    "floating-call-returned",
];

/// The draft the `composer` scenario shows mid-compose.
const COMPOSER_DRAFT: &str = "Shipping the composer on the shared TextInput — real caret, \
     selection, and Ctrl+C/V now.\nEnter sends, Shift+Enter for a new line.\nPlease check teh adress";

impl ResourceApp for ChatShell {
    type Resource = Chat;
    const ID: &'static str = "so.tilo.chat";
    const TITLE: &'static str = "Chat";
    const ICON: Option<&'static str> = Some("chat");
    const ROOM_RECORD: bool = true;

    fn view(cx: &mut ViewCx<Chat>) -> Element {
        crate::i18n::ensure();
        let me = cx.me().unwrap_or_default().to_owned();
        let live = cx.mode().is_live();
        // Native path access controls attachment import. A live browser
        // independently discovers its host's voice service through media replies.
        let native = live && crate::app::native_file_access();
        let content = cx.content().unwrap_or_else(ContentStore::memory);
        // A capture shows the app as a device with its services draws it;
        // nothing it stages reaches them.
        let runtime = if native || cx.mode() == RunMode::Capture {
            ChatRuntime::Native
        } else {
            ChatRuntime::Unavailable
        };
        let identity = LocalIdentity::for_participant(&me);
        let app = ChatApp::new(identity, runtime, content);
        cx.surface(if live && ui::local_identity::bound().is_some() {
            app.announcing()
        } else {
            app
        })
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        if scenario.is_some_and(|scenario| scenario.ends_with("-floating")) {
            (560, 560)
        } else if scenario.is_some_and(|scenario| scenario.ends_with("-compact")) {
            (800, 600)
        } else {
            (1280, 800)
        }
    }

    /// The first-run workspace for `starter`, the demo conversation for
    /// every other scenario, both read by the capture's own person.
    fn fixture(fixture: &mut ui::framework::Fixture<Chat>) {
        if fixture.scenario() == Some("agent-work") {
            for (author, action) in agent_work_scenario() {
                if let Ok(invoke) = Invoke::apply(&action) {
                    fixture.version(author, crate::doc::NOW * 1000, invoke);
                }
            }
        }
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Chat>> {
        if name == Some("agent-work") {
            return Vec::new();
        }
        let seed = match name {
            Some("starter") => {
                let you = LocalIdentity::demo();
                ChatAction::SeedStarter {
                    name: you.name,
                    initials: you.initials,
                    color_idx: you.color_idx,
                }
            }
            _ => ChatAction::SeedDemo {},
        };
        let mut invokes: Vec<Invoke<Chat>> = Invoke::apply(&seed).into_iter().collect();
        if name == Some("composer") {
            // The composer mid-compose: a multi-line draft in the channel shown.
            let key = "c_crit".to_owned();
            let draft = std::iter::once(ChatAction::EnsureRichDraft { key: key.clone() }).chain(
                COMPOSER_DRAFT.lines().map(|line| ChatAction::EditDraft {
                    key: key.clone(),
                    edit: tilo_document::RichTextAction::AppendParagraph {
                        text: line.to_owned(),
                    },
                }),
            );
            invokes.extend(draft.filter_map(|action| Invoke::apply(&action).ok()));
        }
        invokes
    }

    /// A new record's first workspace, owned by its creator. A capture's
    /// record is its scenario's instead.
    fn seed(cx: &SeedCx<'_>) -> Vec<Invoke<Chat>> {
        if cx.host().is_some_and(|host| host.capture().is_some()) {
            return Vec::new();
        }
        let creator = LocalIdentity::for_participant(cx.me());
        let action = if let Some((workspace, name)) = cx.creation_workspace() {
            ChatAction::SeedWorkspace {
                workspace: workspace.to_owned(),
                workspace_name: if name.trim().is_empty() {
                    workspace.to_owned()
                } else {
                    name.to_owned()
                },
                creator_name: creator.name,
                creator_initials: creator.initials,
                color_idx: creator.color_idx,
            }
        } else {
            ChatAction::SeedStarter {
                name: creator.name,
                initials: creator.initials,
                color_idx: creator.color_idx,
            }
        };
        Invoke::apply(&action).into_iter().collect()
    }
}

pub(crate) fn agent_work_scenario() -> Vec<(&'static str, ChatAction)> {
    use crate::model::{
        AgentReasoning, AgentRoute, AgentStep, AgentStepOutcome, AgentWork, AgentWorkState,
    };
    const AGENT: &str = "agent-task:11111111-1111-4111-8111-111111111111";
    let work = AgentWork::new(
        AgentWorkState::Doing {
            did: "Add function".into(),
            about: "Revenue curve, preserving the current graph bounds".into(),
        },
        crate::doc::NOW - 110,
    )
    .because(AgentReasoning {
        route: AgentRoute::Model,
        why: "Calculator offers the requested graph operation and no bounds change is needed."
            .into(),
    })
    .with_trace(vec![AgentStep::authored(
        "calculator.workspace",
        "Add function",
        AgentStepOutcome::Asked,
        0,
    )]);
    vec![
        (crate::model::ME, ChatAction::SeedDemo {}),
        (
            AGENT,
            ChatAction::EnsureUser {
                name: "Tide".into(),
                initials: "TI".into(),
                color_idx: 6,
                status: crate::model::Presence::Online.as_str().into(),
            },
        ),
        (
            crate::model::ME,
            ChatAction::NameAgent {
                participant: AGENT.into(),
                name: "Tide".into(),
            },
        ),
        (
            crate::model::ME,
            actions::send_message_action(
                "c_crit",
                "m_agent_request",
                crate::doc::NOW - 120,
                "@Tide add the revenue curve to Calculator and keep the current bounds.",
                None,
                None,
                vec![crate::mention::Mention {
                    user: AGENT.into(),
                    start: 0,
                    len: "@Tide".len(),
                    is_agent: true,
                }],
            ),
        ),
        (
            AGENT,
            actions::send_message_action(
                "c_crit",
                "m_agent_reply",
                crate::doc::NOW - 115,
                "",
                Some("m_agent_request".into()),
                None,
                Vec::new(),
            ),
        ),
        (
            AGENT,
            actions::set_agent_work_action("c_crit", "m_agent_reply", work, None),
        ),
    ]
}

fn agent_live_row(app: &ChatApp, agent: &str) -> LiveRow {
    let actor = format!("agent:{agent}");
    let label = app.room_names.get(&actor).map_or(agent, String::as_str);
    let row = LiveRow::new(format!("chat.person:{agent}"), "chat.agent", label).operation(
        ChatEffect::OpenAgentConversation {
            workspace: app.context_ws.clone(),
            agent: agent.to_owned(),
        }
        .bound()
        .launchable_when_closed(),
    );
    match app.agent_presentation_is_detached(&app.context_ws, agent) {
        Some(false) => row.operation(localized_presentation_action(
            &ChatEffect::FloatAgentConversation {
                workspace: app.context_ws.clone(),
                agent: agent.to_owned(),
            },
            "chat-agent-float",
        )),
        Some(true) => row.operation(localized_presentation_action(
            &ChatEffect::ReturnAgentConversation {
                workspace: app.context_ws.clone(),
                agent: agent.to_owned(),
            },
            "chat-agent-return",
        )),
        None => row,
    }
}

impl ResourceSurface<Chat, ChatEffect> for ChatApp {
    fn present(&mut self, _state: &std::rc::Rc<ChatState>) {
        // The surface reads the document it syncs with, and reloads when its
        // heads move.
    }

    fn attach(&mut self, cx: SurfaceCx<Chat>) {
        self.attach_framework(cx.dispatcher());
        if cx.mode().is_live() {
            self.follow_preferences(cx.settings());
        }
    }

    fn take_host_intent(&mut self) -> Option<ui::widget::AppHostIntent> {
        self.host_intent.take()
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario.unwrap_or_default());
    }

    fn effect(&mut self, effect: ChatEffect, cx: &mut EffectCx<Chat, Self>) -> EffectAnswer {
        if matches!(
            effect,
            ChatEffect::CopyResourceLink { .. } | ChatEffect::OpenResourceLink { .. }
        ) {
            return self.begin_resource_navigation(&effect, cx.reply());
        }
        if let Some(url) = crate::external::browser_destination(&effect)? {
            return Ok(cx.thread(
                move |_| ui::widget::try_open_external_url(&url).map_err(|error| error.to_string()),
                |_: &mut Self, opened, _| opened.map(|()| ui::framework::Effected::done()),
            ));
        }
        self.answer_effect(effect)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }
    fn live_rows(&self) -> Vec<LiveRow> {
        self.room_agents
            .iter()
            .map(|agent| agent_live_row(self, agent))
            .collect()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        let agent = id.strip_prefix("chat.person:")?;
        self.room_agents
            .iter()
            .find(|known| known.as_str() == agent)
            .map(|known| agent_live_row(self, known))
    }

    fn row_declarations(&self, row: &str) -> Vec<ActionDecl<Chat>> {
        let Some(agent) = row.strip_prefix("chat.person:") else {
            return Vec::new();
        };
        if !self.room_agents.iter().any(|known| known == agent)
            || !self
                .workspaces
                .iter()
                .any(|workspace| workspace.id == self.active_ws)
        {
            return Vec::new();
        }
        let workspace = self.active_ws.clone();
        let agent = agent.to_owned();
        vec![ActionDecl::new(
            "message",
            "Message",
            vec![ActionParam::required(
                PROMPTED_PARAM,
                ParamType::Text,
                "Message this agent",
            )],
            move |args, now_ms| {
                let text = args
                    .text(PROMPTED_PARAM)
                    .ok_or_else(|| "a message needs text".to_owned())?;
                Ok(actions::post_agent_message_action(
                    &workspace,
                    &agent,
                    text,
                    crate::model::AgentIntent::Message,
                    now_ms.div_euclid(1_000),
                ))
            },
        )]
    }
}

fn localized_presentation_action(effect: &ChatEffect, label: &str) -> ui::tilo_actions::ActionSpec {
    let mut action = effect.bound();
    action.label = crate::i18n::t(label);
    action
}

fn draft_keys(doc: &Doc) -> Vec<String> {
    let Ok(Some((Value::Object(ObjType::Map), drafts))) = doc.get(ROOT, "rich_drafts") else {
        return Vec::new();
    };
    let mut keys: Vec<String> = doc.keys(&drafts).collect();
    keys.sort();
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::framework::testing::TestApp;
    use ui::search::SearchProvider as _;

    #[test]
    fn reviewed_feedback_search_opens_the_report_and_triages_through_the_door() {
        let mut app = TestApp::<ChatShell>::new().unwrap();
        let channel = app
            .state()
            .channels
            .into_iter()
            .find(|channel| channel.id == "c_general")
            .unwrap();
        let mut preview = ui::feedback::Draft {
            text: "The reader loses my position".into(),
            ..Default::default()
        }
        .preview()
        .unwrap();
        preview.approve();
        let submission = preview
            .submission(
                "0123456789abcdef0123456789abcdef".into(),
                ui::feedback::Destination {
                    room: channel.workspace,
                    provider: "demo-chat".into(),
                    channel: channel.id.clone(),
                },
            )
            .unwrap();
        for _ in 0..2 {
            app.call_on(
                &format!("chat.channel:{}", channel.id),
                "submit-feedback",
                serde_json::json!({"submission": submission}),
            )
            .unwrap();
        }
        app.settle();
        let state = app.state();
        assert_eq!(state.feedback.len(), 1);
        let report_id = format!("chat.feedback:{}", submission.id);
        let source = ui::framework::testing::search_source::<Chat>(Chat::search(&state));
        let hit = source
            .search("reader loses")
            .into_iter()
            .find(|hit| {
                hit.action_address()
                    .is_some_and(|address| address.resource == report_id)
            })
            .expect("system search finds the reviewed report as a typed resource");
        let address = hit.action_address().unwrap();
        app.call_on(&address.resource, &address.action, serde_json::json!({}))
            .unwrap();
        app.settle();
        assert_eq!(
            app.surface_mut::<ChatApp>().revealed_message.as_deref(),
            Some(format!("m_feedback_{}", submission.id).as_str())
        );
        app.call_on(
            &report_id,
            "triage",
            serde_json::json!({"update": {"status": "investigating"}}),
        )
        .unwrap();
        app.settle();
        let report = app.state().feedback.remove(0);
        assert_eq!(report.submission, submission);
        assert_eq!(report.triage.len(), 1);
        assert_eq!(report.triage[0].author, report.author);
        assert_eq!(
            report.triage[0].update,
            crate::feedback::TriageUpdate::Investigating
        );
    }

    #[test]
    fn sidebar_remains_docked_and_semantically_resizable_in_a_medium_chat_window() {
        let mut app = TestApp::<ChatShell>::new().unwrap();
        app.harness().resize((760, 720)).unwrap();
        let resources = app.harness().resources();
        let resize = resources
            .iter()
            .find(|resource| resource.label == "Resize sidebar")
            .expect("a medium conversation keeps its shared resize handle");
        let address = ui::tilo_actions::ActionAddress::new(resize.id.as_str(), "set-value");
        let arguments = serde_json::Map::from_iter([(
            ui::tilo_actions::PROMPTED_PARAM.to_owned(),
            serde_json::Value::String("40%".into()),
        )]);
        assert!(
            app.harness()
                .invoke_action(&address, &arguments, false, None)
                .unwrap()
                .completed()
                .is_some()
        );
        assert_eq!(
            app.with_surface::<ChatApp, _>(|surface| (
                surface.left_flap.is_docked(),
                surface.sidebar_rect().width(),
                surface.content_rect().width(),
            )),
            Some((true, 228.0, 456.0))
        );
        assert!(app.harness().component_contract_violations().is_empty());
        app.harness().resize((600, 720)).unwrap();
        assert!(
            app.harness()
                .resources()
                .iter()
                .all(|resource| resource.label != "Resize sidebar"),
            "a genuinely narrow window uses the shared overlay drawer"
        );
        app.harness().resize((760, 720)).unwrap();
        assert_eq!(
            app.with_surface::<ChatApp, _>(|surface| surface.sidebar_rect().width()),
            Some(228.0),
            "the usable resized width survives narrow-window collapse"
        );
    }

    fn collaborator(
        id: &str,
        name: &str,
        is_agent: bool,
        is_self: bool,
    ) -> ui::collaboration::Collaborator {
        ui::collaboration::Collaborator {
            id: id.to_owned(),
            name: name.to_owned(),
            colour: None,
            is_agent,
            is_self,
        }
    }

    fn agent_presentation_actions(
        app: &mut TestApp<ChatShell>,
    ) -> Vec<ui::tilo_actions::ActionDescriptor> {
        app.actions()
            .unwrap()
            .into_iter()
            .filter(|action| {
                action.address.resource == "chat.person:tide"
                    && matches!(
                        action.address.action.as_str(),
                        "float-agent-conversation" | "return-agent-conversation"
                    )
            })
            .collect()
    }

    fn adopt_agent_surface(
        app: &mut TestApp<ChatShell>,
        request: &ui::floating_surface::FloatingSurfaceRequest,
    ) {
        let rect = request
            .initial_rect
            .expect("Float retains its exact source rect");
        app.with_widget::<ChatApp, _>(|chat| {
            ui::widget::FloatingSurfaceWidgetExt::on_floating_surface_host_message(
                chat,
                &ui::floating_surface::FloatingSurfaceHostMessage::PrepareSurface {
                    id: request.id.clone(),
                    token: "0123456789abcdef0123456789abcdef".into(),
                    rect: ui::floating_surface::protocol_rect_array(rect),
                    content_rect: ui::floating_surface::protocol_rect_array(rect),
                    transfer_covered: true,
                    coordinate_offset: [0.0, 0.0],
                },
            );
            ui::widget::FloatingSurfaceWidgetExt::on_floating_surface_host_message(
                chat,
                &ui::floating_surface::FloatingSurfaceHostMessage::Adopted {
                    id: request.id.clone(),
                    rect: ui::floating_surface::protocol_rect_array(rect),
                },
            );
        });
    }

    fn assert_stale_float_preserves_detached_surface(app: &mut TestApp<ChatShell>) {
        let stale_float = app
            .with_widget::<ChatApp, _>(|chat| {
                let before = (
                    chat.active_channel.clone(),
                    chat.direct_agent.clone(),
                    chat.agent_surface_requests.len(),
                );
                let refused = chat.answer_effect(ChatEffect::FloatAgentConversation {
                    workspace: chat.context_ws.clone(),
                    agent: "tide".into(),
                });
                let after = (
                    chat.active_channel.clone(),
                    chat.direct_agent.clone(),
                    chat.agent_surface_requests.len(),
                );
                let retained = chat.agent_surfaces.values().next().map(|surface| {
                    (
                        surface.placement.is_desktop_adopted(),
                        surface.placement.is_return_pending(),
                        surface.view.direct_agent.clone(),
                    )
                });
                (refused, before, after, retained)
            })
            .unwrap();
        assert!(
            stale_float.0.is_err(),
            "a stale Float cannot act on the detached conversation"
        );
        assert_eq!(
            stale_float.1, stale_float.2,
            "refusal does not reselect a route or enqueue another transfer"
        );
        assert_eq!(
            stale_float.3,
            Some((true, false, Some("tide".into()))),
            "the adopted surface and its exact editor view remain untouched"
        );
    }

    #[test]
    fn sidebar_shared_resize_uses_the_complete_mounted_app_door() {
        let mut app = TestApp::<ChatShell>::new().unwrap();
        app.harness().resize((960, 720)).unwrap();
        let resources = app.harness().resources();
        let resize = resources
            .iter()
            .find(|resource| resource.label == "Resize sidebar")
            .expect("the live shared separator has its localized human label");
        let spec = resize
            .actions
            .iter()
            .find(|action| action.id == "set-value")
            .expect("the mounted app publishes exact resize");
        assert_eq!(spec.kind, ui::tilo_actions::OperationKind::Effect);
        assert!(app.harness().component_contract_violations().is_empty());
        let address = ui::tilo_actions::ActionAddress::new(resize.id.as_str(), "set-value");
        let arguments = serde_json::Map::from_iter([(
            ui::tilo_actions::PROMPTED_PARAM.to_owned(),
            serde_json::Value::String("30%".into()),
        )]);
        assert!(
            app.harness()
                .invoke_action(&address, &arguments, false, None)
                .expect("the actual published shared control accepts canonical invocation")
                .completed()
                .is_some()
        );
        assert_eq!(
            app.with_surface::<ChatApp, _>(|surface| surface.left_flap.list_rect().width()),
            Some(212.0)
        );
        let mut viewer = TestApp::<ChatShell>::with_role(ui::permissions::Role::Viewer).unwrap();
        viewer.harness().resize((960, 720)).unwrap();
        assert!(viewer.harness().component_contract_violations().is_empty());
        let resources = viewer.harness().resources();
        let separator = resources
            .iter()
            .find(|resource| resource.label == "Resize sidebar")
            .expect("the Viewer frame observes the actual shared separator");
        for action in &separator.actions {
            if matches!(action.id.as_str(), "set-value" | "increment" | "decrement") {
                assert_eq!(action.kind, ui::tilo_actions::OperationKind::Effect);
            }
        }
    }

    #[test]
    fn resource_range_opens_and_highlights_current_text_through_the_public_view() {
        let mut app = TestApp::<ChatShell>::new().unwrap();
        app.call_on(
            "chat.channel:c_general",
            "post",
            serde_json::json!({"value": "café notes"}),
        )
        .unwrap();
        app.settle();
        let id = app
            .state()
            .messages()
            .find(|m| m.text == "café notes")
            .unwrap()
            .id
            .clone();
        app.call_on(
            &format!("chat.message:{id}"),
            "reveal-message-range",
            serde_json::json!({"start": 3, "end": 4, "unit": "Chars"}),
        )
        .unwrap();
        app.settle();
        let surface = app.surface_mut::<crate::ChatApp>();
        let mut bodies = surface.body_views.borrow_mut();
        let (_, view) = bodies
            .get_mut(&(0, id.clone()))
            .expect("real layout produced the message body");
        assert_eq!(view.editor_mut().selected_text().as_deref(), Some("é"));
        drop(bodies);
        app.call_on(
            &format!("chat.message:{id}"),
            "edit",
            serde_json::json!({"value": "changed"}),
        )
        .unwrap();
        app.settle();
        let surface = app.surface_mut::<crate::ChatApp>();
        let mut bodies = surface.body_views.borrow_mut();
        let (_, view) = bodies.get_mut(&(0, id)).unwrap();
        assert!(
            view.editor_mut().selected_text().is_none(),
            "edits invalidate the old highlight"
        );
    }

    #[test]
    fn shared_space_visible_stop_button_uses_the_same_consent_handler() {
        let mut app = TestApp::<ChatShell>::capture(Some("live-context")).unwrap();
        assert!(app.surface_mut::<crate::ChatApp>().sharing_context);
        assert!(app.harness().a11y_node_named("Stop sharing").is_some());
        let target = app
            .surface_mut::<crate::ChatApp>()
            .hits
            .iter()
            .find(|(_, hit)| matches!(hit, crate::app::Hit::StopSharingLiveContext))
            .expect("the painted stop control has a pointer target")
            .0;
        app.harness().click_at(target.center().x, target.center().y);
        assert!(!app.surface_mut::<crate::ChatApp>().sharing_context);
        assert!(app.harness().a11y_node_named("Stop sharing").is_none());
        assert!(ui::view::capability::view_state(app.harness().root_widget()).is_none());
    }

    #[test]
    fn shared_space_context_consent_is_local_and_relaunch_starts_private() {
        let mut app = TestApp::<ChatShell>::new().unwrap();
        app.call("share-live-context", serde_json::json!({}))
            .unwrap();
        let state = app
            .call("live-context-status", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(state["sharing"], true);
        assert_eq!(state["videoCapture"], false);
        let view = ui::view::capability::view_state(app.harness().root_widget()).unwrap();
        assert!(
            view.view_mode.unwrap().starts_with("chat-context-v1:"),
            "the framework root forwards presence"
        );
        app.call("stop-sharing-live-context", serde_json::json!({}))
            .unwrap();
        assert!(ui::view::capability::view_state(app.harness().root_widget()).is_none());
        app.call("share-live-context", serde_json::json!({}))
            .unwrap();
        app = app.relaunch(&[]).unwrap();
        let state = app
            .call("live-context-status", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(
            state["sharing"], false,
            "consent is never restored from persistence"
        );
    }

    #[test]
    fn shared_space_call_status_returns_a_report_and_never_claims_live_capture() {
        let mut app = TestApp::<ChatShell>::new().unwrap();
        let status = app
            .call_on("chat.call:c_lounge", "call-status", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(status["refreshRequested"], true);
        assert_eq!(status["reported"]["capture_active"], false);
        assert_eq!(status["reported"]["muted"], true);
        assert_eq!(status["channel"], serde_json::Value::Null);
        assert_eq!(app.state().messages().count(), 0);
    }

    #[test]
    fn shared_space_resource_journey_preserves_address_author_and_viewer_boundary() {
        let mut app = TestApp::<ChatShell>::new().unwrap();
        let reference = serde_json::json!({
            "kind": "word_processor.block", "id": "word_processor.block:paragraph-7",
            "label": "Proposal", "span": {"start": 2, "end": 9, "unit": "Chars"}
        });
        app.call_on(
            "chat.channel:c_general",
            "share-resource",
            serde_json::json!({"reference": reference, "sourceProvider": "record:so.tilo.word-processor:proposal", "text": "Discuss this paragraph"}),
        )
        .unwrap();
        app.settle();
        let state = app.state();
        let message = state
            .messages()
            .find(|m| m.text == "Discuss this paragraph")
            .unwrap();
        assert_eq!(serde_json::to_value(&message.reference).unwrap(), reference);
        let id = message.id.clone();
        let author = message.author.clone();
        let result = app
            .call_on(
                &format!("chat.message:{id}"),
                "read-message",
                serde_json::json!({}),
            )
            .unwrap()
            .unwrap();
        assert_eq!(result["reference"], reference);
        assert_eq!(
            result["sourceProvider"],
            "record:so.tilo.word-processor:proposal"
        );
        assert_eq!(result["author"], author);
        app.set_role(ui::permissions::Role::Viewer);
        assert!(
            app.call_on(
                "chat.channel:c_general",
                "share-resource",
                serde_json::json!({"reference": reference})
            )
            .is_err()
        );
        assert_eq!(app.state().messages().count(), 1);
        let read = app
            .call_on(
                &format!("chat.message:{id}"),
                "read-message",
                serde_json::json!({}),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            read["reference"], reference,
            "viewers can inspect an address, with no grant to its target"
        );
    }

    #[test]
    fn shared_space_invalid_resource_never_posts_and_voice_has_no_timeline() {
        let mut app = TestApp::<ChatShell>::new().unwrap();
        for id in [
            "/home/person/private",
            "https://example.test",
            "block\nsecret",
            "",
        ] {
            assert!(
                app.call_on(
                    "chat.channel:c_general",
                    "share-resource",
                    serde_json::json!({
                        "reference": {"kind": "word_processor.block", "id": id, "label": "Proposal"}
                    })
                )
                .is_err()
            );
        }
        assert!(app.call_on("chat.channel:c_lounge", "share-resource", serde_json::json!({
            "reference": {"kind": "word_processor.block", "id": "block-7", "label": "Proposal"}
        })).is_err());
        assert_eq!(app.state().messages().count(), 0);
    }

    #[test]
    fn a_new_room_record_starts_with_its_creators_workspace() {
        let app = TestApp::<ChatShell>::new().unwrap();
        let state = app.state();
        assert_eq!(
            state
                .workspaces
                .iter()
                .map(|workspace| workspace.id.as_str())
                .collect::<Vec<_>>(),
            ["ws_home"]
        );
        assert_eq!(state.users.len(), 1, "only its creator");
        assert!(
            state
                .channels
                .iter()
                .any(|channel| channel.name == "general"),
            "the starter channels"
        );
        assert!(
            state.messages().next().is_none(),
            "no fabricated conversation"
        );
    }

    #[test]
    fn the_root_offers_launchable_call_starts_once_there_is_a_workspace() {
        let mut doc = ui::tilo_resource::Doc::new();
        ChatDoc::seed_if_empty(&mut doc);
        let state = Chat::read(&doc).unwrap();
        let published = ui::tilo_resource::publish::<Chat>(Chat::KIND, "Chat", &state);
        let spec = |id: &str| {
            published
                .actions
                .iter()
                .find(|action| action.id == id)
                .unwrap_or_else(|| panic!("the Chat root publishes {id}"))
        };
        for id in ["start-call", "start-video-call"] {
            let action = spec(id);
            assert_eq!(action.kind, ui::tilo_actions::OperationKind::Effect, "{id}");
            assert_eq!(
                action.contract.discovery,
                ui::tilo_actions::DiscoveryScope::Launchable,
                "{id} launches Chat when it is closed"
            );
            assert_eq!(
                action.contract.retry,
                ui::tilo_actions::RetryPolicy::Idempotent
            );
        }
        assert_eq!(spec("start-call").label, "Start a call");
        assert_eq!(
            spec("start-video-call").contract.availability,
            ui::tilo_actions::Availability::Unavailable {
                code: crate::effects::VIDEO_CALL_UNAVAILABLE.into(),
                reason: crate::effects::video_call_unavailable_reason(),
            }
        );
        let empty = ui::tilo_resource::publish::<Chat>(
            Chat::KIND,
            "Chat",
            &Chat::read(&ui::tilo_resource::Doc::new()).unwrap(),
        );
        assert!(
            !empty
                .actions
                .iter()
                .any(|action| action.id.starts_with("start-")),
            "a chat with no conversation yet offers no call to start"
        );
    }

    #[test]
    fn start_call_through_the_door_makes_a_voice_channel_and_joins_it() {
        let mut app = TestApp::<ChatShell>::new().unwrap();
        app.call("start-call", serde_json::json!({})).unwrap();
        app.settle();
        let state = app.state();
        assert!(
            state
                .channels
                .iter()
                .any(|channel| channel.kind == crate::model::ChannelKind::Voice),
            "the starter's voice channel stands"
        );
        let refused = app.call("start-video-call", serde_json::json!({}));
        assert!(refused.is_err(), "{refused:?}");
    }

    #[test]
    fn a_window_publishes_its_effects_beside_the_chat_edits() {
        let mut app = TestApp::<ChatShell>::new().unwrap();
        let surface = app.agent_surface();
        let on = |resource: &str| {
            surface
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["resource"] == resource)
                .map(|row| row["action"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        let root = on(Chat::KIND);
        assert!(
            root.iter().any(|action| action == "attach-file"),
            "{root:?}"
        );
        assert!(
            root.iter().any(|action| action == "select-input"),
            "{root:?}"
        );
        let general = on("chat.channel:c_general");
        for id in ["open-channel", "post", "mark-read", "read-messages"] {
            assert!(
                general.iter().any(|action| action == id),
                "{id}: {general:?}"
            );
        }
        let lounge = on("chat.call:c_lounge");
        assert!(
            lounge.iter().any(|action| action == "join-call"),
            "{lounge:?}"
        );
    }

    #[test]
    fn authenticated_agent_rows_publish_working_message_and_view_actions_then_withdraw() {
        let mut app = TestApp::<ChatShell>::new().unwrap();
        app.harness().set_collaboration_context(vec![
            collaborator("test-author", "Quinten", false, true),
            collaborator("tide", "Person Tide", false, false),
            collaborator("agent:tide", "Tide", true, false),
        ]);
        let actions = app.actions().unwrap();
        let on_agent = actions
            .iter()
            .filter(|action| action.address.resource == "chat.person:tide")
            .collect::<Vec<_>>();
        assert_eq!(
            on_agent
                .iter()
                .map(|action| action.address.action.as_str())
                .collect::<Vec<_>>(),
            ["message", "open-agent-conversation"],
            "a raw Person id collision does not mint another Agent row"
        );
        assert!(
            on_agent
                .iter()
                .all(|action| action.resource_label == "Tide")
        );

        app.call_on_for(
            ui::tilo_actions::ActionInvoker::actor("test-author"),
            "chat.person:tide",
            "open-agent-conversation",
            serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(
            app.with_widget::<ChatApp, _>(|chat| chat.direct_agent.clone())
                .flatten()
                .as_deref(),
            Some("tide"),
            "the mounted published View action opens the same direct conversation"
        );
        let open = on_agent
            .iter()
            .find(|action| action.address.action == "open-agent-conversation")
            .expect("authenticated Agent has one semantic View");
        assert_eq!(
            open.contract.discovery,
            ui::tilo_actions::DiscoveryScope::Launchable,
            "a cold Chat launch must republish and re-admit this exact current Agent View"
        );

        let invocation = app
            .call_on_for(
                ui::tilo_actions::ActionInvoker::actor("test-author"),
                "chat.person:tide",
                "message",
                serde_json::json!({PROMPTED_PARAM: "Can you summarize the open question?"}),
            )
            .unwrap();
        let message = app.read_document(|doc| {
            let channel = ChatDoc::list_channels(doc)
                .into_iter()
                .find(|channel| channel.conversation_with.as_deref() == Some("tide"))
                .expect("the direct conversation was created atomically");
            ChatDoc::list_messages(doc, &channel.id)
                .into_iter()
                .next()
                .expect("the admitted message was posted")
        });
        let context = app
            .versions()
            .iter()
            .filter_map(|version| version.context.clone())
            .find(|context| context.source_invocation == Some(invocation))
            .expect("the document change retained its action context");
        assert_eq!(message.author, "test-author");
        assert_eq!(
            message.agent_intent,
            Some(crate::model::AgentIntent::Message)
        );
        assert_eq!(context.source_invocation, Some(invocation));
        assert_eq!(message.id, actions::agent_request_message_id(invocation));

        app.harness().set_collaboration_context(vec![collaborator(
            "test-author",
            "Quinten",
            false,
            true,
        )]);
        assert!(
            app.actions()
                .unwrap()
                .iter()
                .all(|action| action.address.resource != "chat.person:tide"),
            "withdrawing the complete Context withdraws both capabilities"
        );
    }

    #[test]
    fn exact_agent_row_keeps_the_enumerated_read_view_for_a_viewer() {
        let mut app = TestApp::<ChatShell>::with_role(ui::permissions::Role::Viewer).unwrap();
        app.harness().set_collaboration_context(vec![
            collaborator("test-author", "Quinten", false, true),
            collaborator("agent:tide", "Tide", true, false),
        ]);
        let offered = app.actions().unwrap();
        let on_agent = offered
            .iter()
            .filter(|action| action.address.resource == "chat.person:tide")
            .collect::<Vec<_>>();
        assert_eq!(
            on_agent
                .iter()
                .map(|action| action.address.action.as_str())
                .collect::<Vec<_>>(),
            ["open-agent-conversation"],
            "a Read ceiling preserves the view while withdrawing Message"
        );

        let (listed, exact) = app
            .with_surface::<ChatApp, _>(|chat| {
                let listed = <ChatApp as ResourceSurface<Chat, ChatEffect>>::live_rows(chat)
                    .into_iter()
                    .find(|row| row.id() == "chat.person:tide")
                    .expect("enumerated Agent row")
                    .into_resource();
                let exact = <ChatApp as ResourceSurface<Chat, ChatEffect>>::live_row(
                    chat,
                    "chat.person:tide",
                )
                .expect("exact Agent row")
                .into_resource();
                (listed, exact)
            })
            .expect("mounted Chat surface");
        assert_eq!(exact.id, listed.id);
        assert_eq!(exact.kind, listed.kind);
        assert_eq!(exact.label, listed.label);
        let listed_actions = ui::tilo_actions::door::survey(std::slice::from_ref(&listed));
        let exact_actions = ui::tilo_actions::door::survey(std::slice::from_ref(&exact));
        assert_eq!(exact_actions, listed_actions);
        let open = exact_actions
            .iter()
            .find(|action| action.address.action == "open-agent-conversation")
            .expect("exact lookup retains the Agent view");
        assert_eq!(
            open.contract.permission,
            ui::tilo_actions::PermissionRequirement::Read
        );
    }

    #[test]
    fn agent_conversation_publishes_one_current_presentation_view_and_rechecks_membership() {
        let mut app = TestApp::<ChatShell>::new().unwrap();
        app.harness().set_collaboration_context(vec![
            collaborator("test-author", "Quinten", false, true),
            collaborator("agent:tide", "Tide", true, false),
        ]);
        app.call_on_for(
            ui::tilo_actions::ActionInvoker::actor("test-author"),
            "chat.person:tide",
            "open-agent-conversation",
            serde_json::json!({}),
        )
        .unwrap();

        let offered = agent_presentation_actions(&mut app);
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].address.action, "float-agent-conversation");
        assert_eq!(
            offered[0].kind,
            ui::tilo_actions::OperationKind::View,
            "moving a view is not a document mutation"
        );
        assert_eq!(
            offered[0].contract.permission,
            ui::tilo_actions::PermissionRequirement::Read
        );

        app.call_on_for(
            ui::tilo_actions::ActionInvoker::actor("test-author"),
            "chat.person:tide",
            "float-agent-conversation",
            serde_json::json!({}),
        )
        .unwrap();
        let request = app
            .with_widget::<ChatApp, _>(|chat| {
                ui::widget::FloatingSurfaceWidgetExt::take_floating_surface_request(chat)
            })
            .flatten()
            .expect("the semantic Float uses the shared surface handoff");
        adopt_agent_surface(&mut app, &request);

        let offered = agent_presentation_actions(&mut app);
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].address.action, "return-agent-conversation");
        assert_stale_float_preserves_detached_surface(&mut app);
        app.call_on_for(
            ui::tilo_actions::ActionInvoker::actor("test-author"),
            "chat.person:tide",
            "return-agent-conversation",
            serde_json::json!({}),
        )
        .unwrap();
        assert!(
            app.with_widget::<ChatApp, _>(|chat| chat
                .agent_surfaces
                .values()
                .any(|surface| surface.placement.is_return_pending()))
                .unwrap(),
            "semantic Return uses the same shared return handoff"
        );
        assert!(
            agent_presentation_actions(&mut app).is_empty(),
            "a transfer in progress cannot be invoked a second time"
        );

        app.with_widget::<ChatApp, _>(|chat| {
            ui::widget::FloatingSurfaceWidgetExt::on_floating_surface_host_message(
                chat,
                &ui::floating_surface::FloatingSurfaceHostMessage::Suspended {
                    id: request.id.clone(),
                },
            );
        });
        assert_eq!(
            agent_presentation_actions(&mut app)[0].address.action,
            "float-agent-conversation"
        );

        app.harness().set_collaboration_context(vec![collaborator(
            "test-author",
            "Quinten",
            false,
            true,
        )]);
        let stale = app.call_on_for(
            ui::tilo_actions::ActionInvoker::actor("test-author"),
            "chat.person:tide",
            "float-agent-conversation",
            serde_json::json!({}),
        );
        assert!(
            stale.is_err(),
            "a stale Agent presentation View is withdrawn"
        );
        assert!(
            app.with_widget::<ChatApp, _>(|chat| chat
                .answer_effect(ChatEffect::FloatAgentConversation {
                    workspace: chat.context_ws.clone(),
                    agent: "tide".into(),
                })
                .is_err())
                .unwrap(),
            "the handler independently rechecks current Agent membership"
        );
    }

    #[test]
    fn every_scenario_stages_through_the_resource_and_its_surface() {
        for scenario in SCENARIOS {
            let app = TestApp::<ChatShell>::capture(Some(scenario))
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            let state = app.state();
            if *scenario == "starter" {
                assert!(state.workspaces.iter().all(|ws| ws.id == "ws_home"));
            } else {
                assert!(
                    state.message("m_concept").is_some(),
                    "{scenario} shows the demo conversation"
                );
            }
        }
        let agent = TestApp::<ChatShell>::capture(Some("agent-work")).unwrap();
        let state = agent.state();
        let reply = state
            .message("m_agent_reply")
            .expect("agent reply is durable");
        assert!(reply.agent, "the agent authors its own reply");
        let work = agent
            .read_document(|doc| {
                ChatDoc::list_messages(doc, "c_crit")
                    .into_iter()
                    .find(|message| message.id == "m_agent_reply")
                    .unwrap()
                    .work
            })
            .expect("reply carries work evidence");
        assert_eq!(
            work.reasoning.as_ref().unwrap().route,
            crate::model::AgentRoute::Model
        );
        assert!(matches!(
            work.trace.as_slice(),
            [crate::model::AgentStep {
                outcome: crate::model::AgentStepOutcome::Asked,
                ..
            }]
        ));
        let composer = TestApp::<ChatShell>::capture(Some("composer")).unwrap();
        let draft = composer.read_document(|doc| ChatDoc::draft_text(doc, "c_crit"));
        assert!(draft.contains("Enter sends"), "{draft:?}");
    }
}
