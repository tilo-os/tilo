//! Mail as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path. The compose draft body is a rich-text child resource.

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::EncodedAction;
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{MAIL_ACTION_TYPE, MailAction, register};
use crate::app::MailApp;
use crate::doc::MailDoc;
use crate::model::Folder;

/// Lightweight mailbox projection for search and the published tree.
#[derive(Clone, Debug, Default)]
pub struct MailboxState {
    pub folders: Vec<&'static str>,
    pub has_draft_body: bool,
}

/// The mail mailbox resource.
pub struct Mail;

impl Resource for Mail {
    const KIND: &'static str = "mail.mailbox";
    const LABEL: &'static str = "Mail";
    const ACTION_TYPE: &'static str = MAIL_ACTION_TYPE;

    type State = MailboxState;
    type Action = MailAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<MailboxState, ModelError> {
        Ok(MailboxState {
            folders: Folder::ALL.iter().map(|folder| folder.as_key()).collect(),
            has_draft_body: MailDoc::draft_body(doc).is_some(),
        })
    }

    fn initialize(_doc: &mut Doc) -> Result<(), ModelError> {
        // Live mail starts empty — accounts and messages arrive through the
        // engine / document actions, never as seeded sample content.
        Ok(())
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        // Prompted / batch mailbox specs stay on the published tree.
        Vec::new()
    }

    fn children(state: &MailboxState) -> Vec<tilo_actions::Resource> {
        if !state.has_draft_body {
            return Vec::new();
        }
        vec![tilo_actions::Resource::new(
            "mail.draft:current/body",
            tilo_document::RichText::KIND,
            "Draft body",
        )]
    }

    fn search(state: &MailboxState) -> Vec<SearchEntry> {
        state
            .folders
            .iter()
            .map(|folder| SearchEntry {
                anchor: Some(format!("mail.folder:{folder}")),
                title: (*folder).to_owned(),
                text: String::new(),
            })
            .collect()
    }
}

/// Framework app: the mail widget is the surface over [`Mail`].
pub struct MailShell;

impl ResourceApp for MailShell {
    type Resource = Mail;
    const ID: &'static str = "so.tilo.mail";
    const TITLE: &'static str = "Mail";
    const ICON: Option<&'static str> = Some("mail");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Mail>) -> Element {
        cx.surface(MailApp::new_live())
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "wide",
            "narrow",
            "narrow_detail",
            "narrow_drawer",
            "mid",
            "compose",
            "compose_narrow",
            "setup",
            "setup_narrow",
            "setup_add",
            "setup_oauth",
            "firstrun",
            "html_mail",
            "search",
            "preview",
            "bulk",
            "bulk_purge_confirmation",
            "reply",
            "context_menu",
            "loading",
            "empty",
            "offline",
            "error",
        ]
    }
}

impl ResourceSurface<Mail> for MailApp {
    fn present(&mut self, _state: &std::rc::Rc<MailboxState>) {}

    fn attach(&mut self, cx: SurfaceCx<Mail>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Mail>>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Mail>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
    }

    pub fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &MailAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        let _ = self.try_dispatch_with_context(doc, type_id, action, context);
    }

    pub fn try_dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &MailAction,
        context: ui::tilo_actions::ActionContext,
    ) -> Result<(), tilo_actions::ActionError> {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(crate::actions::encode(action));
                dispatcher.apply_now_with(doc, action, context)
            }
            None => self
                .local
                .try_dispatch_with_context(doc, type_id, action, context),
        }
    }

    pub fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }

    pub fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }
}
