//! Chat as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path. Composer drafts are rich-text child resources.

use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::EncodedAction;
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{CHAT_ACTION_TYPE, ChatAction, register};
use crate::app::ChatApp;
use crate::doc::ChatDoc;
use crate::model::{Channel, Workspace};

/// Lightweight projection of the shared chat document for search and publish.
#[derive(Clone, Debug, Default)]
pub struct ChatState {
    pub workspaces: Vec<Workspace>,
    pub channels: Vec<Channel>,
    /// Channel / thread keys that already have a rich-text draft body.
    pub draft_keys: Vec<String>,
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
        // Every channel creation scaffolds an empty rich draft so first
        // keystrokes converge. Catalogue only drafts that already have text.
        let draft_keys = draft_keys(doc)
            .into_iter()
            .filter(|key| !ChatDoc::draft_text(doc, key).is_empty())
            .collect();
        Ok(ChatState {
            workspaces,
            channels,
            draft_keys,
        })
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        // Empty document: no demo people, messages or starter rooms. Shared
        // root maps are created lazily by the first document action.
        let _ = doc;
        Ok(())
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        // Prompted create-channel / create-category specs stay on the
        // published tree (param `value`, continuation identity). Stub
        // declarations here would shadow those prompts.
        Vec::new()
    }

    fn children(state: &ChatState) -> Vec<tilo_actions::Resource> {
        // Each composer draft is a nested rich-text resource.
        state
            .draft_keys
            .iter()
            .map(|key| {
                tilo_actions::Resource::new(
                    format!("chat.draft:{key}"),
                    tilo_document::RichText::KIND,
                    format!("Draft · {key}"),
                )
            })
            .collect()
    }

    fn search(state: &ChatState) -> Vec<SearchEntry> {
        let workspaces = state.workspaces.iter().map(|workspace| SearchEntry {
            anchor: Some(format!("chat.workspace:{}", workspace.id)),
            title: workspace.name.clone(),
            text: String::new(),
        });
        let channels = state.channels.iter().map(|channel| SearchEntry {
            anchor: Some(format!("chat.channel:{}", channel.id)),
            title: channel.name.clone(),
            text: channel.topic.clone(),
        });
        workspaces.chain(channels).collect()
    }
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
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Chat>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Chat>>,
    last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Chat>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
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
}

fn draft_keys(doc: &Doc) -> Vec<String> {
    let Ok(Some((Value::Object(ObjType::Map), drafts))) = doc.get(ROOT, "rich_drafts") else {
        return Vec::new();
    };
    let mut keys: Vec<String> = doc.keys(&drafts).collect();
    keys.sort();
    keys
}
