//! Mail as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path. The compose draft body is a rich-text child resource.
//!
//! Document mutations (folder CRUD, bulk triage, per-message read/star/
//! archive/delete/labels, discard draft) live on this resource via
//! [`Resource::children`] and [`Resource::bound_actions`]. Hosted
//! [`PublishesResources::resources`] returns only live extras that are not
//! document state — never a second hand-built `mail.mailbox` Document door
//! beside [`tilo_resource::publish`].

use automerge::AutoCommit;
use tilo_document::RichText;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction, Resource as PublishedResource};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry, publish_child};
use ui::widget::Element;

use crate::actions::{
    MAIL_ACTION_TYPE, MailAction, MessageRow, mailbox_bound_document_actions,
    mailbox_local_operations, publish_document_children, register,
};
use crate::app::MailApp;
use crate::doc::MailDoc;
use crate::model::Folder;

/// Lightweight mailbox projection for search and the published tree.
#[derive(Clone, Debug, Default)]
pub struct MailboxState {
    pub folders: Vec<&'static str>,
    /// User-defined local folders `(id, name)` from the shared document.
    pub custom_folders: Vec<(String, String)>,
    /// Flagged messages from the shared document (corpus subjects stay local).
    pub messages: Vec<MessageRow>,
    /// Whether a persisted draft has recipients / subject / body text.
    pub has_draft: bool,
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
        let draft = MailDoc::load_draft(doc);
        let has_draft = !draft.to.is_empty() || !draft.subject.is_empty() || !draft.body.is_empty();
        let messages = MailDoc::list_message_flags(doc)
            .into_iter()
            .map(|flags| MessageRow {
                id: flags.id.clone(),
                thread_id: flags.id,
                subject: String::new(),
                thread_subject: String::new(),
                folder: flags.folder,
                read: flags.read,
                starred: flags.starred,
                labels: flags.labels,
                attachments: Vec::new(),
            })
            .collect();
        Ok(MailboxState {
            folders: Folder::ALL.iter().map(|folder| folder.as_key()).collect(),
            custom_folders: MailDoc::list_folders(doc),
            messages,
            has_draft,
            has_draft_body: MailDoc::draft_body(doc).is_some(),
        })
    }

    fn initialize(_doc: &mut Doc) -> Result<(), ModelError> {
        // Live mail starts empty — accounts and messages arrive through the
        // engine / document actions, never as seeded sample content.
        Ok(())
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        // Prompted / batch mailbox Document specs are state-bound below.
        Vec::new()
    }

    fn bound_actions(_state: &MailboxState) -> Vec<tilo_actions::ActionSpec> {
        mailbox_bound_document_actions()
    }

    fn local_operations() -> Vec<ActionSpec> {
        mailbox_local_operations()
    }

    fn children(state: &MailboxState) -> Vec<tilo_actions::Resource> {
        let mut children = publish_document_children(state);
        if state.has_draft_body {
            children.push(publish_draft_body());
        }
        children
    }

    fn search(state: &MailboxState) -> Vec<SearchEntry> {
        let folders = state.folders.iter().map(|folder| SearchEntry {
            anchor: Some(format!("mail.folder:{folder}")),
            title: (*folder).to_owned(),
            text: String::new(),
            action: None,
        });
        let custom = state.custom_folders.iter().map(|(id, name)| SearchEntry {
            anchor: Some(format!("mail.folder:{id}")),
            title: name.clone(),
            text: String::new(),
            action: None,
        });
        folders.chain(custom).collect()
    }
}

/// Where the draft body is published, beside the draft it belongs to.
const DRAFT_BODY_ID: &str = "mail.draft:current/body";

/// The draft body as a rich-text resource of its own: its edits are
/// [`MailAction::EditDraftBody`], so an agent edits it through its address
/// exactly as the composer does.
fn publish_draft_body() -> PublishedResource {
    publish_child::<RichText, Mail>(DRAFT_BODY_ID, "Draft body", None, MailAction::EditDraftBody)
}

/// The dispatcher the composer edits the draft body through, at the address
/// [`publish_draft_body`] publishes.
pub(crate) fn draft_body_dispatcher(dispatcher: &Dispatcher<Mail>) -> Dispatcher<RichText> {
    dispatcher.child::<RichText>(DRAFT_BODY_ID, MailAction::EditDraftBody)
}

/// Hosted-only extras that are not document state: an in-flight send and a
/// search-index fault. Folder/message Document ops stay on
/// [`tilo_resource::publish`].
pub(crate) fn live_extras(app: &MailApp) -> Vec<PublishedResource> {
    let mut extras = Vec::new();
    if app.has_pending_send() {
        extras.push(PublishedResource::new(
            "mail.send:pending",
            "mail.send",
            "Sending message…",
        ));
    }
    if let Some(error) = app.mail_search_error() {
        extras.push(PublishedResource::new(
            "mail.search-status",
            "mail.search_status",
            format!("Search index unavailable: {error}"),
        ));
    }
    extras
}

/// Framework app: the mail widget is the surface over [`Mail`].
pub struct MailShell;

impl ResourceApp for MailShell {
    type Resource = Mail;
    const ID: &'static str = "so.tilo.mail";
    const TITLE: &'static str = "Mail";
    const ICON: Option<&'static str> = Some("mail");
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

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
    fn present(&mut self, _state: &std::rc::Rc<MailboxState>) {
        // Shared folder/message Document state is adopted through sync from
        // the Automerge document. An empty projection must not wipe the
        // unpublished local mailbox corpus or in-flight compose.
    }

    fn attach(&mut self, cx: SurfaceCx<Mail>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Mail>>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Mail>) {
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
        action: &MailAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        let _ = self.try_dispatch_with_context(doc, type_id, action, context);
    }

    pub(crate) fn try_dispatch_with_context(
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

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::OperationKind;
    use ui::widget::Widget;

    #[test]
    fn resource_publish_owns_folder_and_message_document_ops_and_hosted_keeps_no_second_door() {
        let mut app = MailApp::new_sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);

        let state = Mail::read(&doc).expect("seeded mail state");
        assert!(
            !state.messages.is_empty(),
            "sample sync must ingest message flags into the document"
        );
        let published = ui::tilo_resource::publish::<Mail>("mail.mailbox", "Mail", &state);
        assert_eq!(published.kind, "mail.mailbox");

        for id in [
            "archive-messages",
            "move-messages-to-trash",
            "mark-messages-read",
            "mark-messages-unread",
            "purge-trashed-messages",
            "new-folder",
        ] {
            assert!(
                published
                    .actions
                    .iter()
                    .any(|action| action.id == id && action.kind == OperationKind::Document),
                "{id} Document op must ride the published mailbox root"
            );
        }

        let inbox = published
            .children
            .iter()
            .find(|child| child.id.as_str() == "mail.folder:inbox")
            .expect("inbox Document child on the published door");
        let message = inbox
            .children
            .iter()
            .find(|child| child.kind == "mail.message")
            .expect("message Document child under a folder");
        for id in ["read", "star", "archive", "delete", "add-label"] {
            assert!(
                message
                    .actions
                    .iter()
                    .any(|action| action.id == id && action.kind == OperationKind::Document),
                "{id} Document op must ride the message child: {:?}",
                message
                    .actions
                    .iter()
                    .map(|action| (action.id.as_str(), action.kind))
                    .collect::<Vec<_>>()
            );
        }

        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&doc);
        assert!(
            standalone
                .iter()
                .any(|resource| resource.kind == "mail.mailbox"),
            "standalone keeps the mailbox root"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        assert!(ui::resources::resource(&app, &doc, "mail.mailbox").is_none());
        assert!(
            ui::resources::resource(&app, &doc, "mail.folder:inbox").is_some(),
            "a hosted folder stays the published row"
        );
        let extras = ui::resources::PublishesResources::resources(&app, &doc);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "mail.mailbox"),
            "hosted must not republish mail.mailbox beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );

        // Empty present must not wipe unpublished local mail.
        let before = app.mailbox_message_count();
        assert!(before > 0, "sample app has a local mailbox corpus");
        <MailApp as ResourceSurface<Mail>>::present(
            &mut app,
            &std::rc::Rc::new(MailboxState::default()),
        );
        assert_eq!(
            app.mailbox_message_count(),
            before,
            "empty present must not clear unpublished local mail"
        );
    }

    mod draft_body {
        use tilo_document::RichTextAction;
        use ui::framework::testing::TestApp;
        use ui::tilo_resource::Invoke;
        use vello::kurbo::Rect;

        use super::super::{DRAFT_BODY_ID, MailShell};
        use crate::doc::MailDoc;
        use tilo_document::RichText;
        use ui::tilo_resource::Resource as _;

        /// The draft body's paragraphs as stored.
        fn body(app: &TestApp<MailShell>) -> Vec<String> {
            app.read_document(|doc| {
                MailDoc::draft_body(doc)
                    .and_then(|root| tilo_rich_text::blocks_under(doc, &root).ok())
                    .map(|blocks| blocks.into_iter().map(|block| block.text).collect())
                    .unwrap_or_default()
            })
        }

        /// The draft body as the HTML part it is sent as.
        fn html(app: &TestApp<MailShell>) -> String {
            app.read_document(|doc| {
                let root = MailDoc::draft_body(doc).unwrap();
                tilo_doc_codecs::export_html::render_html_fragment_under(
                    doc,
                    &root,
                    &tilo_content::NoContent,
                )
                .unwrap_or_default()
            })
        }

        /// Where the composer shows its control `target` (`body`, `bold`).
        fn control(app: &mut TestApp<MailShell>, target: &str) -> Rect {
            fn find(node: &serde_json::Value, target: &str) -> Option<serde_json::Value> {
                match node {
                    serde_json::Value::Object(fields) => {
                        if fields.get("action_target").and_then(|t| t.as_str()) == Some(target) {
                            return fields.get("bounds").cloned();
                        }
                        fields.values().find_map(|value| find(value, target))
                    }
                    serde_json::Value::Array(items) => {
                        items.iter().find_map(|value| find(value, target))
                    }
                    _ => None,
                }
            }
            let inspection: serde_json::Value =
                serde_json::from_str(&app.harness().inspection_json().unwrap()).unwrap();
            let bounds = find(&inspection, &format!("mail.compose:{target}"))
                .unwrap_or_else(|| panic!("the composer shows {target}: {inspection}"));
            let edge = |name: &str| bounds[name].as_f64().unwrap();
            Rect::new(edge("x0"), edge("y0"), edge("x1"), edge("y1"))
        }

        /// Mail with the composer open and the caret in its body.
        fn composing() -> TestApp<MailShell> {
            ui::i18n::I18nManager::init("en-US");
            let mut app = TestApp::<MailShell>::new().unwrap();
            app.call("compose", serde_json::json!({})).unwrap();
            let body = control(&mut app, "body");
            app.harness().click_at(body.x0 + 20.0, body.y0 + 16.0);
            app
        }

        #[test]
        fn typing_in_the_draft_body_edits_it_through_mail_a_word_a_step() {
            let mut app = composing();
            app.harness().type_text("Hi there");
            assert_eq!(body(&app), ["Hi there"]);
            let edits = app
                .versions()
                .into_iter()
                .filter_map(|version| version.context)
                .filter(|context| {
                    context
                        .resource
                        .as_ref()
                        .is_some_and(|resource| resource.id == DRAFT_BODY_ID)
                })
                .count();
            assert!(edits > 0, "history names the draft body as what was edited");
            assert!(app.undo());
            assert_eq!(body(&app), ["Hi "]);
        }

        #[test]
        fn formatting_marks_what_is_typed_next() {
            let mut app = composing();
            let bold = control(&mut app, "bold");
            app.harness().click_at(bold.center().x, bold.center().y);
            app.harness().type_text("Hi");
            assert_eq!(body(&app), ["Hi"]);
            let html = html(&app);
            assert!(html.contains("<strong>Hi</strong>"), "got: {html}");
        }

        #[test]
        fn the_link_prompt_links_the_typed_address() {
            let mut app = composing();
            let link = control(&mut app, "link");
            app.harness().click_at(link.center().x, link.center().y);
            // The prompt starts with the scheme; the address goes after it.
            app.harness().key("End");
            app.harness().type_text("example.test/docs");
            app.harness().key("Enter");
            let html = html(&app);
            assert!(
                html.contains("href=\"https://example.test/docs\""),
                "got: {html}"
            );
        }

        #[test]
        fn an_agent_edits_the_draft_body_through_its_own_address() {
            let mut app = composing();
            let surface = app.agent_surface();
            assert!(
                surface.as_array().unwrap().iter().any(|row| {
                    row["resource"] == DRAFT_BODY_ID && row["resource_kind"] == RichText::KIND
                }),
                "the draft body is published as rich text: {surface}"
            );
            app.invoke_child::<RichText>(
                DRAFT_BODY_ID,
                Invoke::<RichText>::apply(&RichTextAction::ReplaceBody {
                    markdown: "from an agent".into(),
                })
                .unwrap(),
            )
            .unwrap();
            assert_eq!(body(&app), ["from an agent"]);
            let shown = app.harness().inspection_json().unwrap();
            assert!(shown.contains("from an agent"), "the composer shows it");
            assert!(app.undo());
            assert_eq!(body(&app), [""]);
        }
    }
}
