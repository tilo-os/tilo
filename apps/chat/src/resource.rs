//! Chat as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path. Composer drafts are rich-text child resources.
//!
//! Document mutations (create channel/category, rename/archive/delete channel,
//! post/edit/react/pin/delete message, …) live on this resource via
//! [`Resource::children`]. Hosted [`PublishesResources::resources`] returns
//! only live extras that are not document state — never a second hand-built
//! `chat.workspace` Document door beside [`tilo_resource::publish`].

use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{EncodedAction, Resource as PublishedResource};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    self, CHAT_ACTION_TYPE, ChatAction, MessageResource, RESOLVED_CHANNEL_MESSAGES,
    publish_document_children, register,
};
use crate::app::ChatApp;
use crate::doc::ChatDoc;
use crate::model::{Category, Channel, Workspace};

/// Lightweight projection of the shared chat document for search and publish.
#[derive(Clone, Debug, Default)]
pub struct ChatState {
    pub workspaces: Vec<Workspace>,
    pub channels: Vec<Channel>,
    pub categories: Vec<Category>,
    /// Recent messages keyed by channel id — base for message Document ops.
    pub channel_messages: Vec<(String, Vec<MessageResource>)>,
    /// Top-level message counts keyed by channel id — base for mark-read.
    pub channel_top_level: Vec<(String, i64)>,
    /// Channel / thread keys that already have a rich-text draft body.
    pub draft_keys: Vec<String>,
    /// Local peer id used to bake post / react / mark-read Document payloads.
    pub me: String,
    /// Mentionable people for palette `post` (document users).
    pub participants: Vec<crate::mention::Participant>,
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
        // Prefer the demo peer when the document still names it — tests and
        // `--shots`. Otherwise the resolved local identity for a live peer.
        let me = if users.iter().any(|user| user.id == crate::model::ME) {
            crate::model::ME.to_string()
        } else {
            crate::identity::LocalIdentity::resolve().id
        };
        let participants = users
            .iter()
            .map(|user| crate::mention::Participant {
                id: user.id.clone(),
                name: user.name.clone(),
                is_agent: user.is_agent,
            })
            .collect();
        let mut channel_messages = Vec::new();
        let mut channel_top_level = Vec::new();
        for channel in &channels {
            channel_top_level.push((
                channel.id.clone(),
                ChatDoc::top_level_count(doc, &channel.id) as i64,
            ));
            let mut messages = ChatDoc::list_messages(doc, &channel.id);
            let recent =
                messages.split_off(messages.len().saturating_sub(RESOLVED_CHANNEL_MESSAGES));
            let recent = recent
                .into_iter()
                .map(|message| {
                    let label = message_label(
                        &message.author,
                        &message.text,
                        message.attachment.as_ref(),
                        &users,
                        &agents,
                    );
                    MessageResource {
                        id: message.id,
                        author: message.author,
                        label,
                        pinned: message.pinned,
                        parent: message.parent,
                        attachment: message.attachment.as_ref().map(|attachment| {
                            actions::AttachmentResource {
                                name: attachment.name().to_owned(),
                                blob: attachment.blob().to_owned(),
                            }
                        }),
                        reactions: message
                            .reactions
                            .iter()
                            .map(|reaction| actions::ReactionResource {
                                emoji: reaction.emoji.clone(),
                                count: reaction.count(),
                                reacted_by_me: reaction.reacted_by(&me),
                            })
                            .collect(),
                    }
                })
                .collect();
            channel_messages.push((channel.id.clone(), recent));
        }
        // Every channel creation scaffolds an empty rich draft so first
        // keystrokes converge. Catalogue only drafts that already have text.
        let draft_keys = draft_keys(doc)
            .into_iter()
            .filter(|key| !ChatDoc::draft_text(doc, key).is_empty())
            .collect();
        Ok(ChatState {
            workspaces,
            channels,
            categories,
            channel_messages,
            channel_top_level,
            draft_keys,
            me,
            participants,
        })
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        // Empty document: no demo people, messages or starter rooms. Shared
        // root maps are created lazily by the first document action.
        let _ = doc;
        Ok(())
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        // Prompted create-channel / create-category specs stay on workspace
        // children (continuation identity). Stub root declarations would
        // shadow those prompts.
        Vec::new()
    }

    fn children(state: &ChatState) -> Vec<PublishedResource> {
        let mut children = publish_document_children(state);
        // Each composer draft is a nested rich-text resource.
        children.extend(state.draft_keys.iter().map(|key| {
            PublishedResource::new(
                format!("chat.draft:{key}"),
                tilo_document::RichText::KIND,
                format!("Draft · {key}"),
            )
        }));
        children
    }

    fn search(state: &ChatState) -> Vec<SearchEntry> {
        let workspaces = state.workspaces.iter().map(|workspace| SearchEntry {
            anchor: Some(format!("chat.workspace:{}", workspace.id)),
            title: workspace.name.clone(),
            text: String::new(),
            action: None,
        });
        let channels = state.channels.iter().map(|channel| SearchEntry {
            anchor: Some(format!("chat.channel:{}", channel.id)),
            title: channel.name.clone(),
            text: channel.topic.clone(),
            action: None,
        });
        workspaces.chain(channels).collect()
    }
}

/// Hosted-only extras that are not document state. Channel/message Document
/// ops stay on [`tilo_resource::publish`]. Chat currently has no separate
/// typing / in-flight publish rows — return empty once the door is complete.
pub(crate) fn live_extras(_app: &ChatApp) -> Vec<PublishedResource> {
    Vec::new()
}

/// Framework app: the chat widget is the surface over [`Chat`].
pub struct ChatShell;

impl ResourceApp for ChatShell {
    type Resource = Chat;
    const ID: &'static str = "so.tilo.chat";
    const TITLE: &'static str = "Chat";
    const ICON: Option<&'static str> = Some("chat");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Chat>) -> Element {
        cx.surface(ChatApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "narrow",
            "history",
            "compact",
            "starter",
            "people",
            "thread-narrow",
            "collaboration",
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
        ]
    }
}

impl ResourceSurface<Chat> for ChatApp {
    fn present(&mut self, state: &std::rc::Rc<ChatState>) {
        // Adopt shared rooms/channels only — never wipe unpublished local
        // messages or an unsynced workspace the door has not projected yet.
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Chat>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Chat>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
    pub(crate) hosted: bool,
    pub(crate) last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            hosted: false,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Chat>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &ChatAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(crate::actions::encode(action));
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, type_id, action, context),
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    #[allow(
        dead_code,
        reason = "ChatApp tests read the last dispatch; the hosted path does not"
    )]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

fn draft_keys(doc: &Doc) -> Vec<String> {
    let Ok(Some((Value::Object(ObjType::Map), drafts))) = doc.get(ROOT, "rich_drafts") else {
        return Vec::new();
    };
    let mut keys: Vec<String> = doc.keys(&drafts).collect();
    keys.sort();
    keys
}

fn message_label(
    author: &str,
    text: &str,
    attachment: Option<&crate::model::Attachment>,
    users: &[crate::model::User],
    agents: &std::collections::HashMap<String, String>,
) -> String {
    crate::actions::message_label_for_resource(author, text, attachment, users, agents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::OperationKind;
    use ui::widget::Widget;

    fn seed_doc() -> AutoCommit {
        let mut doc = AutoCommit::new();
        ChatDoc::seed_if_empty(&mut doc);
        doc.commit();
        doc
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario / publish-ownership assertion kept top-to-bottom"
    )]
    fn resource_publish_owns_channel_and_message_document_ops_and_hosted_keeps_no_second_door() {
        let doc = seed_doc();
        let state = Chat::read(&doc).expect("seeded chat state");
        let published = ui::tilo_resource::publish::<Chat>("chat.workspace", "Chat", &state);
        assert_eq!(published.kind, "chat.workspace");

        let workspace = published
            .children
            .iter()
            .find(|child| child.id.as_str() == "chat.workspace:ws_acme")
            .expect("workspace Document child on the published door");
        for id in ["new-channel", "new-category"] {
            assert!(
                workspace
                    .actions
                    .iter()
                    .any(|action| action.id == id && action.kind == OperationKind::Document),
                "{id} Document op must ride the workspace child"
            );
        }

        let channel = workspace
            .children
            .iter()
            .find(|child| child.id.as_str() == "chat.channel:c_crit")
            .expect("channel Document child");
        for id in ["post", "rename", "archive", "mark-read", "delete"] {
            assert!(
                channel
                    .actions
                    .iter()
                    .any(|action| action.id == id && action.kind == OperationKind::Document),
                "{id} Document op must ride the channel child: {:?}",
                channel
                    .actions
                    .iter()
                    .map(|action| (action.id.as_str(), action.kind))
                    .collect::<Vec<_>>()
            );
        }

        let message = channel
            .children
            .iter()
            .find(|child| child.kind == "chat.message")
            .expect("message Document child under the channel");
        for id in ["edit", "react", "pin", "delete"] {
            assert!(
                message
                    .actions
                    .iter()
                    .any(|action| action.id == id && action.kind == OperationKind::Document),
                "{id} Document op must ride the message child"
            );
        }

        let mut app = ChatApp::new_ephemeral();
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert!(
            standalone
                .iter()
                .any(|resource| resource.kind == "chat.workspace"),
            "standalone keeps the chat root"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        assert!(ui::resources::resource(&app, &seeded, "chat.workspace").is_none());
        let hosted_channel = ui::resources::resource(&app, &seeded, "chat.channel:c_crit")
            .expect("a hosted channel is the published row");
        assert!(
            hosted_channel
                .actions
                .iter()
                .any(|action| action.id == "post" && action.kind == OperationKind::Document),
            "the hosted channel keeps the published post action"
        );
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "chat.workspace"),
            "hosted must not republish chat.workspace beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );

        // Empty present must not wipe unpublished local messages.
        let mut local = ChatApp::new_ephemeral();
        let mut seeded_local = doc;
        local.sync(&mut seeded_local, &automerge::ROOT);
        let before = local.messages.len();
        assert!(before > 0, "seeded app has local messages");
        local.adopt_resource_state(&ChatState::default());
        assert_eq!(
            local.messages.len(),
            before,
            "adopting an empty projection must not clear unpublished messages"
        );
        assert!(
            !local.channels.is_empty(),
            "empty adopt must not wipe channels either"
        );
    }
}
