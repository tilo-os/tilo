//! Mail as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path. The compose draft body is a rich-text child resource.
//!
//! Document mutations (folder CRUD, bulk triage, per-message read/star/
//! archive/delete/labels, discard draft) live on this resource via
//! [`Resource::children`] and [`Resource::bound_actions`]. The window adds
//! only live rows that are not document state ([`ResourceSurface::live_rows`]):
//! accounts, loaded threads and attachments, an in-flight send — never a
//! second `mail.mailbox` door beside [`tilo_resource::publish`].

use tilo_document::RichText;
use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectStatus, Effected, Fixture, LiveRow, ResourceApp,
    ResourceSurface, RunMode, SurfaceCx, ViewCx,
};
use ui::tilo_actions::{ActionParam, ParamType, Resource as PublishedResource};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry, publish_child};
use ui::widget::Element;

use crate::actions::{
    MAIL_ACTION_TYPE, MailAction, MailEffect, MessageRow, Outgoing, ServerUpdate,
    mailbox_bound_document_actions, publish_document_children, register,
};
use crate::app::MailApp;
use crate::doc::MailDoc;
use crate::model::Folder;
use crate::sample::MailFixture;

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
    /// What mail servers still have to be told: the document's flags, moves
    /// and permanent deletions they do not hold yet.
    pub server_updates: Vec<ServerUpdate>,
    /// Messages sent and not yet delivered, with the reason the server
    /// refused one.
    pub outbox: Vec<(Outgoing, Option<String>)>,
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
        let has_draft = !draft.is_empty();
        let messages = MailDoc::list_message_flags(doc)
            .into_iter()
            .map(|flags| MessageRow {
                id: flags.id,
                subject: String::new(),
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
            server_updates: MailDoc::server_updates(doc),
            outbox: MailDoc::outbox(doc),
        })
    }

    fn initialize(_doc: &mut Doc) -> Result<(), ModelError> {
        // Live mail starts empty — accounts and messages arrive through the
        // engine / document actions, never as seeded sample content.
        Ok(())
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        let text = |name, description| ActionParam::required(name, ParamType::Text, description);
        let optional =
            |name, description| ActionParam::optional(name, ParamType::Text, description);
        vec![
            ActionDecl::new(
                "send-message",
                "Send message",
                vec![
                    text(
                        "account",
                        "The address it is sent from: one of the mailbox's accounts",
                    ),
                    text("to", "Recipients, comma-separated"),
                    optional("cc", "Copy recipients, comma-separated"),
                    optional("bcc", "Blind copy recipients, comma-separated"),
                    text("subject", "The subject"),
                    text("body", "The message, as plain text"),
                ],
                |args, _| {
                    let field = |name| args.text(name).unwrap_or_default().to_owned();
                    let draft = crate::model::Draft {
                        to: field("to"),
                        cc: field("cc"),
                        bcc: field("bcc"),
                        subject: field("subject"),
                        body: field("body"),
                        ..crate::model::Draft::default()
                    };
                    Ok(MailAction::Send(Outgoing::new(&field("account"), &draft)))
                },
            )
            .describe(
                "Puts the message in the outbox; a device that holds the account delivers it. \
                 Until then, undo takes it back unsent.",
            )
            .destructive(),
            ActionDecl::new(
                "cancel-send",
                "Cancel sending",
                vec![text("id", "The outbox message")],
                |args, _| {
                    Ok(MailAction::CancelSend {
                        id: args.text("id").unwrap_or_default().to_owned(),
                    })
                },
            )
            .describe("Takes a message out of the outbox unsent."),
        ]
    }

    fn bound_actions(_state: &MailboxState) -> Vec<tilo_actions::ActionSpec> {
        mailbox_bound_document_actions()
    }

    fn child(state: &MailboxState, id: &str) -> Option<tilo_actions::Resource> {
        let message = id.strip_prefix("mail.message:")?;
        state
            .messages
            .iter()
            .find(|row| row.id == message)
            .map(crate::actions::publish_message_document)
    }

    fn children(state: &MailboxState) -> Vec<tilo_actions::Resource> {
        let mut children = publish_document_children(state);
        children.extend(state.outbox.iter().map(|(outgoing, failed)| {
            let subject = if outgoing.draft.subject.is_empty() {
                "(no subject)"
            } else {
                outgoing.draft.subject.as_str()
            };
            let label = match failed {
                Some(reason) => format!("Not sent: {subject} — {reason}"),
                None => format!("Sending: {subject}"),
            };
            PublishedResource::new(
                format!("mail.outbox:{}", outgoing.id),
                "mail.outgoing",
                label,
            )
        }));
        if state.has_draft_body {
            children.push(publish_draft_body());
        }
        children
    }

    /// Every folder is a part of the mailbox search can be narrowed to.
    fn search_scopes(state: &MailboxState) -> Vec<ui::search::ScopeDecl> {
        let mut scopes = crate::model::Folder::ALL
            .into_iter()
            .map(|folder| {
                ui::search::ScopeDecl::new(
                    format!("mail.folder:{}", folder.as_key()),
                    folder.title(),
                )
            })
            .collect::<Vec<_>>();
        scopes.extend(state.custom_folders.iter().map(|(id, name)| {
            ui::search::ScopeDecl::new(format!("mail.folder:{id}"), name.clone())
        }));
        scopes
    }

    fn search(state: &MailboxState) -> Vec<SearchEntry> {
        crate::search::folder_entries(&state.custom_folders)
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

/// Hosted-only extras that are not document state: an in-flight send.
/// Folder/message Document ops stay on [`tilo_resource::publish`].
pub(crate) fn live_extras(app: &MailApp) -> Vec<PublishedResource> {
    let mut extras = Vec::new();
    if app.has_pending_send() {
        extras.push(PublishedResource::new(
            "mail.send:pending",
            "mail.send",
            "Sending message…",
        ));
    }
    extras
}

/// Mail's preferences, which follow the person across their devices and are
/// never seen by collaborators: the format a new message is written in and
/// the accounts whose folders the sidebar keeps folded, and the senders
/// whose remote images load.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MailPreferences {
    #[serde(default)]
    pub compose_format: crate::model::ComposeFormat,
    #[serde(default)]
    pub collapsed_accounts: Vec<String>,
    /// The senders whose remote images load without asking.
    #[serde(default)]
    pub remote_content_senders: Vec<String>,
}

impl ui::storage::SettingsSchema for MailPreferences {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Personal;
}

/// Framework app: the mail widget is the surface over [`Mail`].
pub struct MailShell;

/// Every screenshot scenario. Each shows the sample mailbox and accounts
/// (`firstrun` none, `empty` an account with no mail yet) with the view state
/// its name says, staged by the surface.
const SCENARIOS: &[&str] = &[
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
    "setup_blank",
    "setup_discovering",
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
];

impl ResourceApp for MailShell {
    type Resource = Mail;
    const ID: &'static str = "so.tilo.mail";
    const TITLE: &'static str = "Mail";
    const ICON: Option<&'static str> = Some("mail");

    /// Only a live window reaches mail servers and the account store; History
    /// shows what each past version of the document held, and a screenshot
    /// its scenario's fixture.
    fn view(cx: &mut ViewCx<Mail>) -> Element {
        let app = match cx.mode() {
            RunMode::Live => MailApp::for_window(cx.storage(), cx.content()),
            RunMode::Past => MailApp::for_past_version(),
            RunMode::Capture => {
                let fixture = cx
                    .fixture::<MailFixture>()
                    .unwrap_or_else(|| std::rc::Rc::new(MailFixture::without_accounts()));
                MailApp::for_capture(&fixture)
            }
        };
        cx.surface(app)
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario {
            Some(
                "narrow" | "narrow_detail" | "narrow_drawer" | "compose_narrow" | "setup_narrow",
            ) => (560, 860),
            Some("mid") => (1000, 720),
            Some("compose" | "reply") => (1180, 820),
            Some(
                "setup" | "setup_add" | "setup_blank" | "setup_discovering" | "setup_oauth"
                | "firstrun" | "error",
            ) => (1100, 780),
            Some("html_mail") => (1280, 820),
            _ => (1280, 800),
        }
    }

    fn fixture(fixture: &mut Fixture<Mail>) {
        let mail = match fixture.scenario() {
            Some("firstrun") => MailFixture::without_accounts(),
            Some("empty") => MailFixture::before_first_fetch(),
            _ => MailFixture::bundled(),
        };
        fixture.provide(mail);
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(crate::i18n::catalog())
    }
}

impl ResourceSurface<Mail, MailEffect> for MailApp {
    /// Every message already loaded, outside Trash: its subject, sender and
    /// body come from the engine, not the document.
    fn search_entries(&self) -> Vec<SearchEntry> {
        let mut entries = crate::search::message_entries(&self.shared_mailbox());
        entries.extend(self.contact_recipient_search_entries());
        entries
    }

    fn present(&mut self, state: &std::rc::Rc<MailboxState>) {
        self.has_contact_draft_conflict = state.has_draft || state.has_draft_body;
        // Shared folder/message Document state is adopted through sync from
        // the Automerge document. An empty projection must not wipe the
        // unpublished local mailbox corpus or in-flight compose.
    }

    /// Every change the document records that a mail server does not hold
    /// yet — whoever made it — goes to the server from a live window on a
    /// device with the account, and the engine's acknowledgement records it
    /// as applied.
    fn attach(&mut self, cx: SurfaceCx<Mail>) {
        self.attach_framework(cx.dispatcher());
        // A screenshot shows its fixture as it is, whatever the profile holds.
        if cx.mode().is_live() {
            self.follow_preferences(cx.settings::<MailPreferences>());
            self.contact_directory = cx.shared_person::<tilo_contact_directory::ContactDirectory>();
            if let Some(accounts) = cx.shared_person::<accounts::person::PersonAccounts>() {
                self.follow_person_accounts(accounts);
            }
        }
        let outbox = self.server_outbox();
        let projection = cx.drain(
            |state: &MailboxState| {
                state
                    .server_updates
                    .iter()
                    .map(|update| (update.clone(), update.clone()))
                    .collect()
            },
            move |update: ServerUpdate, cx| {
                let reply = cx.reply();
                outbox.borrow_mut().push(
                    update,
                    Box::new(move |result| {
                        reply.finish(result.map(|()| tilo_actions::ActionOutput::default()));
                    }),
                );
                Ok(Effected::Pending)
            },
        );
        self.follow_projection(projection);
        let outbox = self.server_outbox();
        let delivery = cx.drain(
            |state: &MailboxState| {
                state
                    .outbox
                    .iter()
                    .filter(|(_, failed)| failed.is_none())
                    .map(|(outgoing, _)| (outgoing.id.clone(), outgoing.clone()))
                    .collect()
            },
            move |outgoing: Outgoing, cx| {
                let reply = cx.reply();
                outbox.borrow_mut().deliver(
                    outgoing,
                    Box::new(move |result| {
                        reply.finish(result.map(|()| tilo_actions::ActionOutput::default()));
                    }),
                );
                Ok(Effected::Pending)
            },
        );
        self.follow_delivery(delivery);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        if let Some(scenario) = scenario {
            self.request_stage(scenario);
        }
    }

    fn effect(&mut self, effect: MailEffect, cx: &mut EffectCx<Mail, Self>) -> EffectAnswer {
        if let MailEffect::Save { message, index } = &effect {
            let (name, bytes) = self.attachment_to_save(message, *index)?;
            let resource = format!("mail.attachment:{message}:{index}");
            let request = ui::portal::PickRequest::save("Save attachment", "Save", &name)
                .thread_name("mail-portal-save-attachment")
                .filter(ui::portal::FileFilter::all_files());
            return Ok(
                cx.save_file(request, bytes, move |app: &mut Self, saved, _| {
                    Ok(app.finish_attachment_save(saved, &name, resource))
                }),
            );
        }
        if matches!(effect, MailEffect::PrepareContactDraft { .. }) {
            let state = cx.live().get();
            self.has_contact_draft_conflict = state.has_draft || state.has_draft_body;
        }
        let invocation = cx.invocation().unwrap_or_default();
        let answer = self.begin_effect(invocation, effect)?;
        if matches!(answer, Effected::Pending) {
            self.await_effect(invocation, cx.reply());
        }
        Ok(answer)
    }

    /// Fetching and preparing an invitation need a connected account.
    fn effects(&self) -> Vec<EffectStatus> {
        if self.can_sync() {
            return Vec::new();
        }
        vec![
            EffectStatus::unavailable(
                MailEffect::REFRESH,
                "account_offline",
                "No connected mail account is available",
            ),
            EffectStatus::unavailable(
                MailEffect::PREPARE_CALENDAR_INVITATION,
                "account_offline",
                "Connect a Mail account before preparing an invitation",
            ),
        ]
    }

    /// The accounts, an in-flight send and a search-index fault.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.live_rows()
    }

    /// A loaded message, thread, attachment or account the mailbox does not
    /// publish.
    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.live_row(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::OperationKind;
    use ui::widget::Widget;

    #[test]
    fn effects_reach_the_window_through_the_door() {
        let mut app = ui::framework::testing::TestApp::<MailShell>::new().unwrap();
        let surface = app.agent_surface().to_string();
        assert!(surface.contains("prepare-calendar-invitation"), "{surface}");
        app.effect(&MailEffect::Compose)
            .expect("composing needs no account");
        assert!(
            app.effect(&MailEffect::Refresh).is_err(),
            "nothing to refresh without a connected account"
        );
        app.effect_on(
            "mail.folder:archive",
            &MailEffect::Open {
                target: crate::actions::MailTarget::Folder("archive".into()),
            },
        )
        .expect("a folder row opens its folder");
    }

    #[test]
    fn resource_publish_owns_folder_and_message_document_ops_and_the_window_keeps_no_second_door() {
        let mut app = MailApp::new_sample();
        let mut doc = automerge::AutoCommit::new();
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);

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

        // What the window adds beside the mailbox is its live rows, which
        // never carry a document action.
        for row in app.live_rows() {
            let row = row.into_resource();
            assert_ne!(row.kind, "mail.mailbox");
            assert!(
                row.actions
                    .iter()
                    .all(|action| action.kind != OperationKind::Document),
                "{}",
                row.id.as_str()
            );
        }
        let opened = message
            .actions
            .iter()
            .find(|action| action.id == "open")
            .expect("a published message opens where it is");
        assert_eq!(opened.kind, OperationKind::View);

        // Empty present must not wipe unpublished local mail.
        let before = app.mailbox_message_count();
        assert!(before > 0, "sample app has a local mailbox corpus");
        <MailApp as ResourceSurface<Mail, MailEffect>>::present(
            &mut app,
            &std::rc::Rc::new(MailboxState::default()),
        );
        assert_eq!(
            app.mailbox_message_count(),
            before,
            "empty present must not clear unpublished local mail"
        );
    }

    /// Sending is an action on the mailbox, the same for the composer and
    /// an agent: the message waits in the outbox, and undo takes it back
    /// unsent before a device holding the account delivers it.
    #[test]
    fn a_message_is_sent_through_the_outbox_and_undo_takes_it_back_unsent() {
        use ui::framework::testing::TestApp;
        use ui::tilo_resource::Invoke;

        let mut app = TestApp::<MailShell>::new().unwrap();
        let surface = app.agent_surface().to_string();
        for action in ["send-message", "cancel-send"] {
            assert!(surface.contains(&format!("\"{action}\"")), "{surface}");
        }
        let outgoing = Outgoing::new(
            "nobody-here@example.test",
            &crate::model::Draft {
                to: "you@example.test".into(),
                subject: "Lunch".into(),
                body: "Noon?".into(),
                ..crate::model::Draft::default()
            },
        );
        app.invoke_confirmed(Invoke::apply(&MailAction::Send(outgoing.clone())).unwrap())
            .unwrap();
        app.settle();
        let outbox = app.state().outbox;
        assert_eq!(outbox.len(), 1, "{outbox:?}");
        assert_eq!(outbox[0].0.draft.subject, "Lunch");
        assert!(
            outbox[0].1.is_none(),
            "a device without the account leaves it waiting, not refused"
        );
        assert!(app.undo(), "sending is an undo step until it is delivered");
        assert!(app.state().outbox.is_empty(), "undo took it back unsent");

        assert!(
            app.invoke_confirmed(
                Invoke::apply(&MailAction::Send(Outgoing {
                    draft: crate::actions::DraftData::default(),
                    ..outgoing
                }))
                .unwrap()
            )
            .is_err(),
            "a message without a recipient is refused"
        );
    }

    mod captures {
        use ui::framework::ResourceApp as _;
        use ui::framework::testing::TestApp;

        use super::super::MailShell;
        use crate::model::Folder;

        /// Every scenario captures through the framework at its own
        /// viewport, from the bundled sample accounts rather than the
        /// person's.
        #[test]
        fn every_scenario_captures_the_fixture_at_its_viewport() {
            ui::i18n::I18nManager::init("en-US");
            for scenario in MailShell::scenarios() {
                let mut app = TestApp::<MailShell>::capture(Some(scenario))
                    .unwrap_or_else(|error| panic!("{scenario}: {error}"));
                let (width, height) = MailShell::shot_viewport(Some(scenario));
                assert_eq!(
                    app.harness().inspection().viewport,
                    [width, height],
                    "{scenario}"
                );
                let frame = app.harness().inspection_json().unwrap();
                match *scenario {
                    "firstrun" => assert!(frame.contains("No email account yet"), "{frame}"),
                    "setup" | "setup_narrow" => {
                        let email = crate::sample::sample_account_store().active_config().email;
                        assert!(frame.contains(&email), "{scenario}: {frame}");
                    }
                    "error" => assert!(frame.contains("Couldn’t connect to IMAP"), "{frame}"),
                    _ => {}
                }
            }
        }

        /// The window's one search source finds a loaded message by its
        /// subject and a folder by its name, and each hit opens a resource
        /// the window publishes.
        #[test]
        fn the_window_search_finds_loaded_mail_and_folders() {
            ui::i18n::I18nManager::init("en-US");
            let mut app = TestApp::<MailShell>::capture(None).unwrap();
            let message = crate::sample::sample_mailbox()
                .messages
                .values()
                .find(|message| message.folder == Folder::Inbox)
                .expect("the sample has inbox mail")
                .clone();
            let providers = ui::search::capability::search_providers(app.harness().root_widget());
            assert_eq!(providers.len(), 1, "one search source");
            let hits = providers[0].search(&message.subject);
            let hit = hits
                .iter()
                .find(|hit| hit.kind == crate::search::MAIL_KIND && hit.title == message.subject)
                .expect("the loaded message is found by its subject");
            let address = hit.action_address().expect("a canonical address");
            let harness = app.harness();
            let published = ui::resources::resource(
                harness.root_widget(),
                &harness.state().doc,
                &address.resource,
            )
            .expect("the hit's message resolves in the window's publication");
            assert!(
                published
                    .actions
                    .iter()
                    .any(|action| action.id == address.action)
            );
            let folders = providers[0].search("Archive");
            assert!(
                folders
                    .iter()
                    .any(|hit| hit.kind == crate::search::FOLDER_KIND && hit.title == "Archive")
            );
        }

        /// A message the engine loaded that has no row in the document yet
        /// still opens from a search hit: its address resolves to the
        /// window's live row and its open view.
        #[test]
        fn a_loaded_message_without_a_document_row_opens_from_a_hit() {
            ui::i18n::I18nManager::init("en-US");
            let app = crate::app::MailApp::new_sample();
            let message = app
                .shared_mailbox()
                .messages
                .values()
                .next()
                .expect("the sample has mail")
                .clone();
            let id = format!("mail.message:{}", message.id);
            let row = app
                .live_row(&id)
                .expect("a loaded message resolves to a live row")
                .into_resource();
            assert!(row.actions.iter().any(|action| action.id == "open"));
            assert!(
                app.live_row("mail.message:00000000-0000-0000-0000-000000000000")
                    .is_none(),
                "a message that is not loaded resolves to nothing"
            );
        }

        /// The purge confirmation shows messages that really are in Trash:
        /// staging moved them there through the resource's own action.
        #[test]
        fn the_purge_confirmation_stages_its_messages_in_trash_through_actions() {
            ui::i18n::I18nManager::init("en-US");
            let mut app = TestApp::<MailShell>::capture(Some("bulk_purge_confirmation")).unwrap();
            let trashed = app
                .state()
                .messages
                .iter()
                .filter(|message| message.folder == Folder::Trash)
                .count();
            let sample_trash = crate::sample::sample_mailbox()
                .messages
                .values()
                .filter(|message| message.folder == Folder::Trash)
                .count();
            assert_eq!(trashed, sample_trash + 2);
            let frame = app.harness().inspection_json().unwrap();
            assert!(frame.contains("Delete permanently"), "{frame}");
        }

        /// A keyboard triage goes through the framework's door: the starred
        /// message is in the document, and one undo takes it back.
        #[test]
        fn a_shortcut_is_one_undo_step_through_the_framework() {
            ui::i18n::I18nManager::init("en-US");
            let mut app = TestApp::<MailShell>::capture(Some("wide")).unwrap();
            let starred = |app: &TestApp<MailShell>| {
                app.state()
                    .messages
                    .iter()
                    .filter(|message| message.starred)
                    .count()
            };
            let before = starred(&app);
            app.harness().type_text("s");
            let after = starred(&app);
            assert_ne!(after, before, "S toggled the selected message's star");
            assert!(app.undo());
            assert_eq!(starred(&app), before);
        }

        /// A change from any door — an agent's action, an undo, a
        /// collaborator's edit — is what the server has to be told, exactly
        /// as a click on the message is.
        #[test]
        fn every_door_reaches_the_server_through_the_document() {
            use crate::actions::{IngestBaseline, MailAction, ServerUpdate};
            use crate::doc::MailDoc;
            use ui::tilo_resource::Invoke;

            ui::i18n::I18nManager::init("en-US");
            let mut app = TestApp::<MailShell>::capture(Some("wide")).unwrap();
            let row = app
                .state()
                .messages
                .iter()
                .find(|message| {
                    message.folder == Folder::Inbox && !message.read && !message.starred
                })
                .cloned()
                .expect("an unread inbox message");
            let fetched = IngestBaseline {
                id: row.id.clone(),
                read: Some(false),
                starred: Some(false),
                folder: Some("inbox".into()),
                account: Some(crate::sample::sample_account_store().active_config().email),
                locator: Some("launch@example.test".into()),
            };
            app.invoke(
                Invoke::apply(&MailAction::IngestBaselines {
                    messages: vec![fetched],
                })
                .unwrap(),
            )
            .unwrap();
            assert!(app.state().server_updates.is_empty());

            app.invoke(
                Invoke::apply(&MailAction::SetStarred {
                    ids: vec![row.id.clone()],
                    starred: true,
                })
                .unwrap(),
            )
            .unwrap();
            assert!(matches!(
                app.state().server_updates.as_slice(),
                [ServerUpdate::Flags { starred: true, .. }]
            ));
            assert!(app.undo());
            assert!(
                app.state().server_updates.is_empty(),
                "undone before the server heard of it"
            );

            let id = row.id;
            app.merge_peer_edit(|doc| MailDoc::set_folder_for(doc, &id, Folder::Archive));
            assert!(matches!(
                app.state().server_updates.as_slice(),
                [ServerUpdate::Move {
                    to: Folder::Archive,
                    ..
                }]
            ));
        }

        /// A capture is the same on every run: the sample mailbox's ids do
        /// not change between captures.
        #[test]
        fn the_sample_mailbox_is_the_same_on_every_run() {
            let ids = |mailbox: crate::model::Mailbox| {
                let mut ids: Vec<_> = mailbox.messages.keys().copied().collect();
                ids.sort();
                ids
            };
            assert_eq!(
                ids(crate::sample::sample_mailbox()),
                ids(crate::sample::sample_mailbox())
            );
        }

        /// History previews a past version from the document alone: it reads
        /// no accounts and starts no engine, so the preview of a version is
        /// the onboarding over that version's state.
        #[test]
        fn a_past_version_reads_no_accounts() {
            ui::i18n::I18nManager::init("en-US");
            let app = TestApp::<MailShell>::capture(Some("wide")).unwrap();
            let latest = app.versions().last().map(|version| version.hash).unwrap();
            let past = app.past_version(&[latest]).unwrap();
            let shown = past.texts().join("\n");
            assert!(shown.contains("No email account yet"), "{shown}");
            for account in crate::sample::sample_account_store().accounts {
                assert!(!shown.contains(&account.config.email), "{shown}");
            }
        }
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

        /// The path of the widget that publishes the composer's control
        /// `target`, to address it the way assistive technology does.
        fn owner(app: &mut TestApp<MailShell>, target: &str) -> String {
            fn holds(node: &serde_json::Value, target: &str) -> bool {
                match node {
                    serde_json::Value::Object(fields) => {
                        fields.get("action_target").and_then(|t| t.as_str()) == Some(target)
                            || fields.values().any(|value| holds(value, target))
                    }
                    serde_json::Value::Array(items) => items.iter().any(|v| holds(v, target)),
                    _ => false,
                }
            }
            fn find(node: &serde_json::Value, target: &str) -> Option<String> {
                let children = node.get("children")?.as_array()?;
                if let Some(path) = children.iter().find_map(|child| find(child, target)) {
                    return Some(path);
                }
                node.get("semantics")
                    .is_some_and(|semantics| holds(semantics, target))
                    .then(|| node.get("path")?.as_str().map(str::to_owned))
                    .flatten()
            }
            let inspection: serde_json::Value =
                serde_json::from_str(&app.harness().inspection_json().unwrap()).unwrap();
            find(&inspection["root"], target)
                .unwrap_or_else(|| panic!("something publishes {target}: {inspection}"))
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

        /// The format a person writes in follows them: switching it is
        /// stored in their Mail preferences, and a window opened later (on
        /// this device or another) starts its compositions in it.
        #[test]
        fn the_compose_format_follows_the_person() {
            use super::super::MailPreferences;
            use crate::model::ComposeFormat;

            let mut app = composing();
            let format = control(&mut app, "format");
            app.harness().click_at(format.center().x, format.center().y);
            assert_eq!(
                app.settings::<MailPreferences>().get().compose_format,
                ComposeFormat::Text
            );

            let mut later = TestApp::<MailShell>::new().unwrap();
            later
                .settings::<MailPreferences>()
                .set(&MailPreferences {
                    compose_format: ComposeFormat::Text,
                    ..MailPreferences::default()
                })
                .unwrap();
            later.harness().relayout();
            later.call("compose", serde_json::json!({})).unwrap();
            let shown = later.harness().inspection_json().unwrap();
            assert!(shown.contains("mail.compose:body"), "the composer is open");
            assert!(
                !shown.contains("mail.compose:bold"),
                "a plain-text composition has no formatting: {shown}"
            );
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
            // The command may sit in the bar's overflow menu in a narrow
            // composer, so it is addressed by its name, as a screen reader
            // does.
            let owner = owner(&mut app, "mail.compose:link");
            app.harness()
                .perform_semantic_named(
                    &owner,
                    Some("mail.compose:link"),
                    &ui::a11y::Action::Activate,
                )
                .unwrap();
            assert!(
                app.harness()
                    .inspection_json()
                    .unwrap()
                    .contains("mail.compose:prompt"),
                "the link prompt opened"
            );
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
