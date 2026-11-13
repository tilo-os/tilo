//! `MailApp`, the window, and the [`Mailroom`] it shares with the view's effect handler: the
//! engine and its requests, the server search, the mail kept on this device, and what effects
//! ask of the window until its next sync.
//!
//! It is the toolkit's window shell, [`ui::widget::NavShell`], with three columns: a sidebar
//! of accounts and folders, the message list, and the open message. The shell owns the header
//! row, the sidebar toggle, the widths, the drawer and the push stack; the three columns share
//! one [`MailState`] and raise [`Intent`]s and [`Command`]s for effects that reach past their
//! own column.
//!
//! The [`ComposeWindow`] writes in the message column, or fills a window too narrow for one,
//! and status shows as the shell's toast. Accounts are edited in Settings' External Accounts,
//! which Mail links to and reads.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::TryRecvError;
use ui::widget::DrawChild;
use ui::widget::{AppModel, ModelPath};

#[cfg(test)]
use automerge::AutoCommit;
use automerge::ObjId;
use taffy::{Dimension, NodeId, Size as TSize, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use accounts::person::PersonAccounts;
use tilo_content::ContentStore;
use ui::TextRole;
use ui::framework::PersonRecord;
use ui::icons::Icon;
use ui::portal::{FileFilter, OpenFileChannel, OpenFileOutcome, PickRequest};
use ui::presence::PresenceCtx;
use ui::search::{ScopeDecl, SearchOpener, SearchRequest};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::toast::ToastKind;
use ui::widget::{
    ContextMenu, EmptyState, MenuItem, ModalDialog, NavPane, NavShell, SidebarPlacement,
    TaffyTreeExt, Text, Widget,
};

use crate::account::AccountStore;
use crate::actions::{DraftData, Outgoing, ServerUpdate};
use crate::i18n::CATALOG;
#[path = "contact_recipients.rs"]
mod contact_recipients;
#[path = "peer_copies.rs"]
mod peer_copies;
use peer_copies::kept_baseline;

use crate::compose::{ComposeAction, ComposeEventContext, ComposeWindow};
use crate::engine::{EngineEvent, EngineRequest, EngineRequestId, EngineRequestKind};
use crate::list::MessageList;
use crate::model::{
    AccountConfig, Addr, Attachment, ComposeFormat, Draft, Folder, Message, MessageId,
};
use crate::notice::{AttachmentUnavailable, engine_error_text, staged_notice};
use crate::reader::MessageReader;
use crate::resource::{Mail, mailbox, read_mail};
use crate::shortcuts_overlay::MailShortcutsOverlay;
use crate::sidebar;
use crate::state::{
    BulkAction, Command, CtxAction, Intent, MailState, Notice, QueuedMessage, Shared,
};
use crate::util::format_absolute;

const AUTO_REFRESH_MIN: i64 = 5;
const SERVER_SEARCH_DEBOUNCE_MS: u64 = 500;
const SERVER_SEARCH_MIN_CHARS: usize = 3;
const SEND_UNDO_SECS: u64 = 10;

const MAX_ATTACHMENT_SAVE_BYTES: usize = crate::attachment_store::MAX_ATTACHMENT_BYTES;

struct EventContext<'a> {
    scale_factor: f64,
    modifiers: winit::event::Modifiers,
    state: ui::widget::Edits<'a>,
    path: &'a ObjId,
    handled: &'a mut bool,
    focus_target: &'a mut Option<NodeId>,
    node_id: NodeId,
    taffy: &'a TaffyTree<()>,
}

struct MenuTarget {
    message: MessageId,
    items: Vec<CtxAction>,
    /// The reader's menu instead: what each of its rows chooses (`None`
    reader: Vec<Option<crate::state::ReaderChoice>>,
}

struct PurgeConfirmation {
    dialog: ModalDialog<Text>,
    answer: Rc<Cell<Option<bool>>>,
}

struct EngineHandle {
    tx: std::sync::mpsc::Sender<EngineRequest>,
    rx: std::sync::mpsc::Receiver<EngineEvent>,
}

struct PendingSend {
    outgoing: Outgoing,
    message: Message,
    done: DeliveryDone,
}

type ServerUpdateDone = Box<dyn FnOnce(Result<(), String>)>;

type DeliveryDone = Box<dyn FnOnce(Result<ui::tilo_resource::Settled, String>)>;

/// A message sent from this window, held back from the server until `due`
struct HeldSend {
    outgoing: Outgoing,
    done: DeliveryDone,
    due: ui::time::Instant,
}

#[derive(Default)]
pub(crate) struct ServerOutbox {
    queued: Vec<(ServerUpdate, ServerUpdateDone)>,
    sent: HashMap<EngineRequestId, (ServerUpdate, ServerUpdateDone)>,
    deliveries: Vec<(Outgoing, DeliveryDone)>,
}

impl ServerOutbox {
    pub(crate) fn push(&mut self, update: ServerUpdate, done: ServerUpdateDone) {
        self.queued.push((update, done));
    }

    pub(crate) fn deliver(&mut self, outgoing: Outgoing, done: DeliveryDone) {
        self.deliveries.push((outgoing, done));
    }
}

#[derive(Clone)]
enum AccountsPersist {
    Profile(std::path::PathBuf),
    #[cfg(test)]
    Sandbox(Arc<tempfile::TempDir>),
    Disabled,
}

impl AccountsPersist {
    #[cfg(test)]
    fn sandbox() -> Self {
        match tempfile::Builder::new()
            .prefix("tilo-mail-sample-")
            .tempdir()
        {
            Ok(dir) => AccountsPersist::Sandbox(Arc::new(dir)),
            Err(_) => AccountsPersist::Disabled,
        }
    }

    fn root(&self) -> Option<std::path::PathBuf> {
        match self {
            AccountsPersist::Profile(root) => Some(root.clone()),
            #[cfg(test)]
            AccountsPersist::Sandbox(dir) => Some(dir.path().to_path_buf()),
            AccountsPersist::Disabled => None,
        }
    }

    fn persist(&self, accounts: &AccountStore) -> std::io::Result<()> {
        match self.root() {
            Some(root) => accounts.persist_in(&root),
            None => Ok(()),
        }
    }
}

fn wake_through(slot: &Arc<std::sync::Mutex<Option<ui::WakeHandle>>>) -> crate::engine::Wake {
    let slot = Arc::clone(slot);
    Arc::new(move || {
        if let Some(waker) = slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            waker.wake();
        }
    })
}

const ACCOUNT_SETTINGS_LINK: &str = "settings://external_accounts";

const ACCOUNT_ROUTE: &str = "mail.add-account";

/// Checking the open account's mail again, the way a failed check's pill does
/// when assistive technology activates it.
const CHECK_ROUTE: &str = "mail.check";

const NOTIFICATION_SCHEME: &str = "tilo-mail";
const NOTIFICATION_TAG: &str = "mail-new";

struct PendingEngineRequest {
    kind: EngineRequestKind,
    account: String,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "the bools split across unrelated concerns (shell layout flags and engine-request bookkeeping); grouping them would need a real design pass"
)]
pub struct MailApp {
    pub(crate) shared: Shared,
    room: Rc<RefCell<Mailroom>>,
    /// The mailbox as the document holds it, shown as it changes; `None`
    live: Option<ui::framework::Live<Mail>>,
    presented: Option<(u64, u64)>,
    /// Where draft attachment bytes live; drafts carry only their content id.
    content: ContentStore,
    nav: NavShell,
    nav_node: Option<NodeId>,
    bar: ui::framework::BarCommands<Command>,
    first_run: EmptyState,
    first_run_node: Option<NodeId>,
    compose: Box<ComposeWindow>,
    shortcuts: MailShortcutsOverlay,
    editing_draft: Option<MessageId>,
    projected_draft: RefCell<Option<(MessageId, Draft)>>,
    projected_drafts: RefCell<HashSet<MessageId>>,
    menu: ContextMenu,
    menu_target: Option<MenuTarget>,
    pending_menu: Option<MessageId>,
    bulk_purge_confirmation: Option<PurgeConfirmation>,
    server_outbox: Rc<RefCell<ServerOutbox>>,
    projection: Option<ui::framework::Projection>,
    delivery: Option<ui::framework::Projection>,
    resending: Option<String>,
    held_sends: Vec<HeldSend>,
    /// Sends pressed whose outbox messages have yet to reach the drain:
    sends_to_hold: usize,
    /// The notice showing (or about to) is the sending one: its "Undo"
    send_notice: bool,
    sending_notice_next: bool,
    unsend: bool,
    persisted_active_account: Option<String>,
    person_accounts: Option<(PersonRecord<PersonAccounts>, Option<u64>)>,
    custody: Option<crate::custody::SecretRelease>,
    sender_pictures_seen: Option<(u64, Vec<String>)>,
    preferences: Option<ui::framework::Setting<crate::resource::MailPreferences>>,
    followed_preferences: crate::resource::MailPreferences,
    pub(crate) trusted_senders: Option<crate::trusted_senders::Followed>,
    connect_requested: bool,
    background_poll_deadline: Cell<Option<ui::time::Instant>>,
    waker: Arc<std::sync::Mutex<Option<ui::WakeHandle>>>,
    hydrated_at: RefCell<Option<(Vec<ui::automerge::ChangeHash>, usize, u64)>>,
    compose_attachment_rx: Option<OpenFileChannel>,
    compose_attachment_contents: Option<ui::portal::ContentsChannel>,
    opens: std::rc::Rc<std::cell::RefCell<Vec<crate::actions::MailTarget>>>,
    pending_watch_fetches: Vec<AccountConfig>,
    has_older: HashMap<(String, Folder), bool>,
    older_pending: HashMap<(String, Folder), EngineRequestId>,
    kept_registered: bool,
    kept_loaded: std::collections::HashSet<String>,
    copies: Option<crate::copies::Copies>,
    activations: Arc<std::sync::Mutex<Vec<String>>>,
    notifier: Option<Arc<dyn ui::notify::Notifier>>,

    rect: Rect,
    layout_theme: Theme,
    first_layout: bool,
    dirty: bool,
    #[cfg(test)]
    test_layout: Option<(TaffyTree<()>, NodeId)>,
    #[cfg(test)]
    opened_links: Vec<&'static str>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent flags: the engine's outstanding request, the document's draft, the open composer and the redraw"
)]
pub(crate) struct Mailroom {
    pub(crate) shared: Shared,
    search: SearchOpener,

    engine: Option<EngineHandle>,
    accounts_persist: AccountsPersist,
    pub(crate) contact_directory: Option<PersonRecord<tilo_contact_directory::ContactDirectory>>,
    pub(crate) has_contact_draft_conflict: bool,
    awaiting_engine: bool,
    pending_engine_requests: HashMap<EngineRequestId, PendingEngineRequest>,
    resource_refresh_invocation: Option<(EngineRequestId, tilo_actions::InvocationId)>,
    resource_refresh_result: Option<tilo_actions::InvocationUpdate>,
    reveals: Vec<Reveal>,
    effect_replies: Vec<(tilo_actions::InvocationId, ui::framework::EffectReply)>,
    store: crate::store::Store,
    server_search: Option<(String, ui::time::Instant)>,
    server_searched: String,
    last_auto_refresh_min: i64,
    pending_stage: Option<String>,
    pending_send: HashMap<EngineRequestId, PendingSend>,
    composing: bool,
    dirty: bool,
}

impl MailApp {
    pub(crate) fn state(&self) -> std::cell::RefMut<'_, Mailroom> {
        self.room.borrow_mut()
    }

    fn follow_document(&mut self) {
        let Some(mail) = self
            .live
            .as_ref()
            .and_then(|live| live.changed_since(&mut self.presented))
        else {
            return;
        };
        self.state().has_contact_draft_conflict = mail.has_draft();
    }

    #[cfg(test)]
    pub(crate) fn start_effect(
        &mut self,
        invocation: tilo_actions::InvocationId,
        payload: &[u8],
    ) -> Result<ui::framework::Effected, String> {
        let started = self.state().start_effect(invocation, payload);
        self.process_intents();
        started
    }

    pub(crate) fn room(&self) -> Rc<RefCell<Mailroom>> {
        Rc::clone(&self.room)
    }

    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Mail>,
    ) {
        self.shared.borrow_mut().dispatcher = Some(dispatcher.clone());
        self.compose.attach_framework(dispatcher);
    }

    pub(crate) fn wake_from_workers(&mut self, waker: ui::WakeHandle) {
        *self
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(waker.clone());
        // New mail is said through the host's notifications; a click on one
        // opens its message here.
        self.notifier = Some(ui::host::current().notifications);
        let activations = Arc::clone(&self.activations);
        ui::notify::set_activation_handler(NOTIFICATION_SCHEME, move |uri| {
            if let Some(id) = uri
                .strip_prefix(NOTIFICATION_SCHEME)
                .and_then(|rest| rest.strip_prefix(":message/"))
            {
                activations
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(id.to_owned());
                waker.wake();
            }
        });
    }

    fn notify_new_mail(&self, account: &str, arrived: &[&Message], several_accounts: bool) {
        let Some(notifier) = &self.notifier else {
            return;
        };
        let Some(newest) = arrived.iter().max_by_key(|message| message.timestamp_min) else {
            return;
        };
        let (title, body) = match arrived {
            [only] => (only.from.display(), only.subject.clone()),
            many => {
                let mut senders: Vec<String> = Vec::new();
                for message in many {
                    let name = message.from.display();
                    if !senders.contains(&name) {
                        senders.push(name);
                    }
                }
                (
                    crate::i18n::t_args(
                        "mail-notify-arrived",
                        &[("count", ArgValue::from(many.len()))],
                    ),
                    senders.join(", "),
                )
            }
        };
        let body = if several_accounts {
            format!("{body}\n{account}")
        } else {
            body
        };
        let notification = ui::notify::Notification::new(NOTIFICATION_TAG, title, body)
            .with_activation(format!("{NOTIFICATION_SCHEME}:message/{}", newest.id));
        if let Err(error) = notifier.show(&notification) {
            tracing::debug!(%error, "mail: new mail was not announced");
        }
    }

    fn open_activated(&self) {
        let activated = std::mem::take(
            &mut *self
                .activations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for id in activated {
            if let Err(error) = self
                .state()
                .open_target(crate::actions::MailTarget::Message(id))
            {
                tracing::debug!(%error, "mail: a notification's message could not be opened");
            }
        }
    }

    fn workers_wake(&self) -> bool {
        self.waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    pub(crate) fn for_window(
        storage: &ui::storage::AppStorage,
        content: Option<ContentStore>,
    ) -> Self {
        let persist = storage
            .config_root()
            .map_or(AccountsPersist::Disabled, AccountsPersist::Profile);
        // In a session the accounts' secrets are the session's to release;
        let custody = crate::custody::SecretRelease::capture();
        let accounts = match (&custody, persist.root()) {
            (_, None) => AccountStore::new(),
            (Some(_), Some(root)) => AccountStore::load_without_secrets_in(&root),
            (None, Some(root)) => AccountStore::load_in(&root),
        };
        let shared = MailState::shared(MailState::new_live(
            accounts,
            crate::avatars::Avatars::new(),
        ));
        let content = content.unwrap_or_else(ContentStore::memory);
        let waker: Arc<std::sync::Mutex<Option<ui::WakeHandle>>> = Arc::default();
        let (tx, rx) = crate::engine::spawn(content.clone(), wake_through(&waker));
        let mut app = Self::assemble(shared, content, Some(EngineHandle { tx, rx }), persist);
        app.waker = waker;
        app.custody = custody;
        // Fetched mail is kept in the app's cache area; a host without one
        // fetches every time.
        let store = storage.files().paths().cache().native_path().map_or_else(
            |_| crate::store::Store::nowhere(),
            |cache| crate::store::Store::at(cache.join("mail")),
        );
        app.shared.borrow_mut().kept = store.clone();
        app.state().store = store;
        app.open_kept_mail();
        app
    }

    pub(crate) fn for_past_version() -> Self {
        let shared = MailState::shared(MailState::new_live(
            AccountStore::new(),
            crate::avatars::Avatars::new(),
        ));
        Self::assemble(
            shared,
            ContentStore::memory(),
            None,
            AccountsPersist::Disabled,
        )
    }

    #[cfg(feature = "samples")]
    pub(crate) fn for_capture(fixture: &crate::sample::MailFixture) -> Self {
        let shared = MailState::shared(MailState::from_fixture(
            fixture,
            crate::avatars::Avatars::new(),
        ));
        Self::assemble(
            shared,
            ContentStore::memory(),
            None,
            AccountsPersist::Disabled,
        )
    }

    #[cfg(test)]
    pub fn new_sample() -> Self {
        let shared = MailState::shared(MailState::new_sample(crate::avatars::Avatars::new()));
        let mut app = Self::assemble(
            shared,
            ContentStore::memory(),
            None,
            AccountsPersist::sandbox(),
        );
        app.attach_framework(ui::framework::testing::dispatcher("so.tilo.mail"));
        app
    }

    #[cfg(test)]
    pub fn new_first_run() -> Self {
        let shared = MailState::shared(MailState::new_live(
            AccountStore::new(),
            crate::avatars::Avatars::new(),
        ));
        let mut app = Self::assemble(
            shared,
            ContentStore::memory(),
            None,
            AccountsPersist::sandbox(),
        );
        app.attach_framework(ui::framework::testing::dispatcher("so.tilo.mail"));
        app
    }

    fn assemble(
        shared: Shared,
        content: ContentStore,
        engine: Option<EngineHandle>,
        accounts_persist: AccountsPersist,
    ) -> Self {
        let nav = Self::build_shell(&shared);
        let room = Mailroom {
            shared: Rc::clone(&shared),
            search: SearchOpener::new(),
            engine,
            pending_send: HashMap::new(),
            accounts_persist,
            contact_directory: None,
            has_contact_draft_conflict: false,
            awaiting_engine: false,
            pending_engine_requests: HashMap::new(),
            resource_refresh_invocation: None,
            resource_refresh_result: None,
            reveals: Vec::new(),
            effect_replies: Vec::new(),
            last_auto_refresh_min: 0,
            store: crate::store::Store::nowhere(),
            server_search: None,
            server_searched: String::new(),
            pending_stage: None,
            composing: false,
            dirty: true,
        };
        Self {
            shared,
            room: Rc::new(RefCell::new(room)),
            live: None,
            presented: None,
            content,
            nav,
            nav_node: None,
            bar: ui::framework::BarCommands::new(),
            first_run: EmptyState::new("mail-first-run-title")
                .with_icon(Icon::Mail)
                .with_message(CATALOG.t("mail-first-run-hint"))
                .with_action(CATALOG.t("mail-first-run-action"))
                .with_action_id(ACCOUNT_ROUTE),
            first_run_node: None,
            compose: Box::new(ComposeWindow::new(ComposeFormat::default())),
            shortcuts: MailShortcutsOverlay::new(),
            editing_draft: None,
            projected_draft: RefCell::new(None),
            projected_drafts: RefCell::new(HashSet::new()),
            menu: ContextMenu::new(),
            menu_target: None,
            pending_menu: None,
            bulk_purge_confirmation: None,
            server_outbox: Rc::default(),
            projection: None,
            delivery: None,
            resending: None,
            held_sends: Vec::new(),
            sends_to_hold: 0,
            send_notice: false,
            sending_notice_next: false,
            unsend: false,
            persisted_active_account: None,
            person_accounts: None,
            custody: None,
            sender_pictures_seen: None,
            preferences: None,
            followed_preferences: crate::resource::MailPreferences::default(),
            trusted_senders: None,
            connect_requested: false,
            background_poll_deadline: Cell::new(None),
            waker: Arc::default(),
            hydrated_at: RefCell::new(None),
            compose_attachment_rx: None,
            compose_attachment_contents: None,
            opens: std::rc::Rc::default(),
            pending_watch_fetches: Vec::new(),
            has_older: HashMap::new(),
            older_pending: HashMap::new(),
            kept_registered: true,
            copies: None,
            kept_loaded: std::collections::HashSet::new(),
            activations: Arc::default(),
            notifier: None,
            rect: Rect::ZERO,
            layout_theme: Theme::dark(),
            first_layout: true,
            dirty: true,
            #[cfg(test)]
            test_layout: None,
            #[cfg(test)]
            opened_links: Vec::new(),
        }
    }

    fn stage_outbox(&mut self, doc: &mut ui::widget::Edits<'_>) {
        for (to, subject, body) in [
            (
                "ada@analytical.example",
                "Notes on the second engine",
                "Sent from the platform on the train. Tell me if the numbers hold.",
            ),
            (
                "grace@navy.example",
                "Dinner on Friday",
                "Seven, if that suits. I will bring the almanac.",
            ),
        ] {
            self.compose.open_with_draft(&crate::model::Draft {
                to: to.into(),
                subject: subject.into(),
                body: body.into(),
                ..Default::default()
            });
            self.send_compose(doc);
        }
        // The first one's server refused it, in its own words: it stays
        // where it is, with the reason, until it is taken back.
        let refused = crate::resource::read_mail(doc.doc())
            .map(|mail| mail.outgoing())
            .unwrap_or_default()
            .first()
            .map(|(outgoing, _)| outgoing.id.clone());
        if let Some(id) = refused {
            self.shared.borrow().submit(
                doc,
                crate::resource::Mail::paths().outbox.settle(
                    &ui::prelude::Id::from_raw(&id),
                    &ui::tilo_resource::Settled::failed(
                        "550 5.7.1 This message was refused as spam".to_owned(),
                    ),
                ),
            );
        }
        // The sends staged here are held for undo as a person's are; the
        // capture is of the outbox, not of that notice, so it comes down.
        self.nav.dismiss_toast();
        self.send_notice = false;
        self.sending_notice_next = false;
        self.held_sends.clear();
        self.sends_to_hold = 0;
        {
            let mut st = self.shared.borrow_mut();
            st.status = None;
            st.status_undo = false;
        }
        let account = self.shared.borrow().accounts.active;
        self.shared.borrow_mut().open_outbox(account);
    }

    fn stage_scenario(&mut self, doc: &mut ui::widget::Edits<'_>, scenario: &str) {
        match scenario {
            "compose" | "compose_narrow" => self.compose.open_demo(),
            "narrow_detail" => {
                self.nav.show(NavPane::Content);
                self.nav.snap();
            }
            "narrow_drawer" => {
                self.nav.open_sidebar();
                self.nav.snap();
            }
            "html_mail" => self.stage_html_mail(doc),
            gallery if GALLERY_SCENARIOS.iter().any(|(name, _)| *name == gallery) => {
                if let Some((_, sender)) =
                    GALLERY_SCENARIOS.iter().find(|(name, _)| *name == gallery)
                {
                    self.stage_open_from(doc, sender);
                }
            }
            "search" => self
                .state()
                .search
                .open(SearchRequest::this_app().with_query("launch")),
            "preview" => self.stage_preview(),
            "outbox" => self.stage_outbox(doc),
            "bulk" => self.stage_bulk(),
            "bulk_purge_confirmation" => self.stage_bulk_purge_confirmation(doc),
            "reply" => self.stage_reply(),
            "context_menu" => {
                let selected = self.shared.borrow().selected;
                if let Some(id) = selected {
                    self.shared.borrow_mut().push_intent(Intent::ContextMenu(
                        id,
                        Point::new(790.0, 206.0),
                        Rect::new(760.0, 180.0, 820.0, 230.0),
                    ));
                    self.process_intents();
                }
            }
            "loading" => {
                let mut st = self.shared.borrow_mut();
                let fetch = first_fetch(&st.accounts.active_config());
                let since = fetch.since();
                let fetch = ui::arriving::ArrivalClock::new(
                    since
                        .checked_sub(ui::time::Duration::from_secs(1))
                        .unwrap_or(since),
                    fetch.source().clone(),
                );
                st.first_fetch = Some(fetch);
                let ids: Vec<MessageId> = st.mailbox.messages.keys().copied().collect();
                for id in ids {
                    st.mailbox.purge(id);
                }
                st.selected = None;
            }
            "offline" => {
                let mut st = self.shared.borrow_mut();
                let account = st.accounts.active_config().label().to_owned();
                st.note_sync_failure(account, crate::state::SyncCause::Offline);
            }
            _ => {}
        }
    }

    fn stage_open_from(&mut self, doc: &mut ui::widget::Edits<'_>, sender: &str) {
        let id = self
            .shared
            .borrow()
            .mailbox
            .messages
            .values()
            .filter(|message| message.from.email == sender)
            .max_by_key(|message| message.timestamp_min)
            .map(|message| message.id);
        if let Some(id) = id {
            self.shared.borrow_mut().open_message(doc, id);
            self.process_intents();
        }
    }

    fn stage_html_mail(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let id = {
            let st = self.shared.borrow();
            st.mailbox
                .messages
                .values()
                .find(|m| m.from.email == "news@weekly.example.test")
                .map(|m| m.id)
        };
        if let Some(id) = id {
            self.shared.borrow_mut().open_message(doc, id);
            self.process_intents();
        }
    }

    fn stage_preview(&mut self) {
        let id = self
            .shared
            .borrow()
            .mailbox
            .messages_in(Folder::Inbox)
            .into_iter()
            .find_map(|message| (!message.read).then_some(message.id));
        if let Some(id) = id {
            self.shared.borrow_mut().preview_message(id);
            self.process_intents();
        }
    }

    fn stage_bulk(&self) {
        let ids: Vec<MessageId> = self
            .shared
            .borrow()
            .list_rows()
            .into_iter()
            .map(|(id, _)| id)
            .take(2)
            .collect();
        let mut state = self.shared.borrow_mut();
        state.set_bulk_selecting(true);
        for id in ids {
            state.toggle_bulk_selection(id);
        }
    }

    fn stage_bulk_purge_confirmation(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let ids: Vec<MessageId> = self
            .shared
            .borrow()
            .mailbox
            .messages_in(Folder::Inbox)
            .into_iter()
            .take(2)
            .map(|message| message.id)
            .collect();
        let mut state = self.shared.borrow_mut();
        for id in &ids {
            state.move_message(doc, *id, Folder::Trash);
        }
        state.reveal_folder(Folder::Trash);
        state.set_bulk_selecting(true);
        for id in ids {
            state.toggle_bulk_selection(id);
        }
        state.push_intent(Intent::ConfirmBulkPurge);
        drop(state);
        self.process_intents();
    }

    fn stage_reply(&mut self) {
        let id = self
            .shared
            .borrow()
            .mailbox
            .messages_in(Folder::Inbox)
            .first()
            .map(|message| message.id);
        if let Some(id) = id {
            self.shared.borrow_mut().push_intent(Intent::Reply(id));
            self.process_intents();
        }
    }
}

impl Mailroom {
    fn engine_events(&self) -> Vec<EngineEvent> {
        let Some(handle) = &self.engine else {
            return Vec::new();
        };
        let mut events = Vec::new();
        while let Ok(event) = handle.rx.try_recv() {
            events.push(event);
        }
        events
    }

    fn post_engine(&mut self, mut request: EngineRequest) -> Option<EngineRequestId> {
        if let EngineRequest::Fetch {
            account,
            folder,
            want: crate::sync::Want::Latest { known },
            ..
        } = &mut request
            && known.is_none()
        {
            let st = self.shared.borrow();
            let passed = st
                .passed_over
                .get(&(account.email.clone(), *folder))
                .copied();
            *known = crate::sync::Window::of(&st.mailbox, &account.email, *folder)
                .map(|held| held.with(passed))
                .or(passed);
        }
        let Some(engine) = &self.engine else {
            return None;
        };
        let request_id = request.request_id();
        let pending = PendingEngineRequest {
            kind: request.kind(),
            account: request.account().to_owned(),
        };
        if engine.tx.send(request).is_err() {
            // The worker is gone: nothing this window asks will reach a
            // server, so the list says so until a check works again.
            let mut st = self.shared.borrow_mut();
            let account = st
                .accounts
                .label_for(&pending.account)
                .unwrap_or_else(|| pending.account.clone());
            st.note_sync_failure(account, crate::state::SyncCause::Unreachable);
            drop(st);
            self.dirty = true;
            return None;
        }
        self.pending_engine_requests.insert(request_id, pending);
        self.awaiting_engine = true;
        Some(request_id)
    }
}

impl MailApp {
    fn select_all_in_folder(&mut self) {
        let (account, folder, all_inboxes) = self.open_listing();
        if self.shared.borrow().outbox_open {
            return;
        }
        let more = self.folder_has_more(&account, folder, all_inboxes);
        let mut st = self.shared.borrow_mut();
        st.select_all_visible();
        st.more_older = more;
        st.selecting_all = more;
        if more {
            drop(st);
            self.load_older();
        } else {
            let named = (!all_inboxes).then(|| crate::i18n::t(folder.message()));
            st.finish_selection(named.as_deref());
        }
        self.dirty = true;
    }

    fn open_listing(&self) -> (AccountConfig, Folder, bool) {
        let st = self.shared.borrow();
        (st.accounts.active_config(), st.folder, st.all_inboxes)
    }

    fn follow_select_all(&mut self) {
        if !self.shared.borrow().selecting_all {
            return;
        }
        let (account, folder, all_inboxes) = self.open_listing();
        let more = self.folder_has_more(&account, folder, all_inboxes);
        {
            let mut st = self.shared.borrow_mut();
            st.more_older = more;
            st.extend_selection();
        }
        if more {
            self.load_older();
        } else {
            let named = (!all_inboxes).then(|| crate::i18n::t(folder.message()));
            self.shared.borrow_mut().finish_selection(named.as_deref());
        }
        self.dirty = true;
    }

    fn folder_has_more(&self, account: &AccountConfig, folder: Folder, all_inboxes: bool) -> bool {
        if self.state().engine.is_none() {
            return false;
        }
        let st = self.shared.borrow();
        let accounts: Vec<String> = if all_inboxes {
            st.accounts
                .accounts
                .iter()
                .map(|account| account.config.email.clone())
                .filter(|email| !email.is_empty())
                .collect()
        } else {
            vec![account.email.clone()]
        };
        drop(st);
        accounts.into_iter().any(|email| {
            let known =
                crate::sync::Window::of(&self.shared.borrow().mailbox, &email, folder).is_some();
            known && self.has_older.get(&(email, folder)) != Some(&false)
        })
    }

    fn read_outbox(&self, doc: &ui::tilo_resource::Doc) {
        let waiting = crate::resource::read_mail(doc)
            .map(|mail| mail.outgoing())
            .unwrap_or_default();
        let queued: Vec<QueuedMessage> = waiting
            .into_iter()
            .map(|(outgoing, reason)| QueuedMessage {
                id: outgoing.id.clone(),
                account: outgoing.account.clone(),
                to: recipient_line(&outgoing.draft),
                subject: if outgoing.draft.subject.trim().is_empty() {
                    crate::i18n::no_subject()
                } else {
                    outgoing.draft.subject.clone()
                },
                body: outgoing.draft.body.clone(),
                reason,
            })
            .collect();
        let mut st = self.shared.borrow_mut();
        if st.outbox != queued {
            st.outbox = queued;
            st.dirty = true;
        }
        // The page always has a message open, and never one that left the
        // outbox (sent, taken back): the newest that a server stopped, since
        // that is the one waiting on the person, else the newest.
        if st.outbox_open && st.open_queued().is_none() {
            let queued = st.queued();
            st.outbox_selected = queued
                .iter()
                .rev()
                .find(|queued| queued.reason.is_some())
                .or_else(|| queued.last())
                .map(|queued| queued.id.clone());
            st.dirty = true;
        }
    }

    fn expects_engine_event(&self, request_id: EngineRequestId, kind: EngineRequestKind) -> bool {
        self.state()
            .pending_engine_requests
            .get(&request_id)
            .is_some_and(|pending| pending.kind == kind)
    }

    fn build_shell(shared: &Shared) -> NavShell {
        NavShell::new()
            .content_kind(ui::widget::ContentKind::Reading)
            .with_id("mail-shell")
            .sidebar(Box::new(sidebar::sidebar(
                shared,
                crate::bindings::account_settings(),
            )))
            .overview(Box::new(MessageList::new(shared.clone())))
            .content(Box::new(MessageReader::new(shared.clone())))
            .with_window_toolbar()
    }

    fn publish_shell_flags(&self) {
        let geometry = self.nav.geometry();
        let narrow = !geometry.stack.is_empty();
        let detail = narrow && self.nav.front_pane() == NavPane::Content;
        let mut st = self.shared.borrow_mut();
        st.narrow = narrow;
        st.detail_pushed = detail;
    }

    fn declare_window_toolbar(&self) {
        let first_run = self.is_first_run();
        let toolbar = {
            let state = self.shared.borrow();
            let composing = self.compose.open.then(|| self.compose.heading());
            let items =
                crate::window_bar::bar_items(&self.bar, &state, first_run, composing.is_some());
            crate::window_bar::toolbar(
                self.nav.window_toolbar(),
                &state,
                items,
                first_run,
                composing,
            )
        };
        ui::window_toolbar::declare(Some(toolbar));
    }

    fn apply_window_toolbar(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let mut queued = false;
        for event in ui::window_toolbar::take_events() {
            if self.nav.window_toolbar_event(&event) {
                self.dirty = true;
                continue;
            }
            if let Some(command) = self.bar.pressed(&event) {
                self.shared.borrow_mut().push_command(command);
                queued = true;
            }
        }
        if queued {
            self.apply_commands(doc);
        }
    }

    fn folder_search(&self) -> SearchRequest {
        let folder = self.shared.borrow().folder;
        SearchRequest::within(ScopeDecl::new(
            format!("mail.folder:{}", folder.as_key()),
            crate::i18n::t(folder.message()),
        ))
    }

    pub fn selected_message(&self) -> Option<crate::model::MessageId> {
        self.shared.borrow().selected
    }

    fn attachment_save_name(filename: &str) -> String {
        let leaf = filename.rsplit(['/', '\\']).next().unwrap_or_default();
        let mut name = String::new();
        for ch in leaf.trim().chars().filter(|ch| !ch.is_control()) {
            if name.len().saturating_add(ch.len_utf8()) > 240 {
                break;
            }
            name.push(ch);
        }
        if name.is_empty() || matches!(name.as_str(), "." | "..") {
            "attachment".into()
        } else {
            name
        }
    }

    fn request_compose_attachment(&mut self) {
        if self.compose_attachment_rx.is_some() || self.compose_attachment_contents.is_some() {
            self.shared.borrow_mut().status =
                Some(Notice::warning(CATALOG.t("mail-status-picker-open")));
            return;
        }
        let (title, accept) = (
            CATALOG.t("mail-attach-dialog-title"),
            CATALOG.t("mail-attach-dialog-accept"),
        );
        let request = PickRequest::open(&title, &accept)
            .thread_name("mail-portal-stage-attachment")
            .filter(FileFilter::all_files());
        // Where files have paths the host hands over an open descriptor, so
        // Mail reads only the file chosen; elsewhere it hands over contents.
        if ui::files::available(ui::files::Feature::NativePaths) {
            self.compose_attachment_rx = Some(request.send_open_file());
        } else {
            self.compose_attachment_contents = Some(
                request
                    .max_contents_bytes(crate::attachment_store::MAX_ATTACHMENT_BYTES + 1)
                    .send_contents(),
            );
        }
        self.shared.borrow_mut().status = Some(Notice::info(CATALOG.t("mail-status-choose-file")));
        self.dirty = true;
    }

    fn drain_compose_attachment(&mut self) {
        self.drain_compose_attachment_contents();
        let outcome = self
            .compose_attachment_rx
            .as_ref()
            .map(std::sync::mpsc::Receiver::try_recv);
        let Some(outcome) = outcome else { return };
        let notice = match outcome {
            Ok(OpenFileOutcome::Opened(source)) => {
                self.compose_attachment_rx = None;
                if !self.compose.open {
                    self.shared.borrow_mut().status = Some(Notice::info(
                        CATALOG.t("mail-status-selection-composer-closed"),
                    ));
                    return;
                }
                let name = source.display_name().to_string();
                let result =
                    crate::attachment_store::stage_file(&self.content, source.into_file(), &name)
                        .and_then(|attachment| self.compose.add_attachment(attachment));
                Some(staged_notice(name, result))
            }
            Ok(OpenFileOutcome::Cancelled) => {
                self.compose_attachment_rx = None;
                Some(Notice::info(CATALOG.t("mail-status-selection-cancelled")))
            }
            Ok(OpenFileOutcome::Failed(error)) => {
                self.compose_attachment_rx = None;
                Some(Notice::error(CATALOG.t_args(
                    "mail-status-picker-failed",
                    &[("error", ArgValue::Text(error))],
                )))
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.compose_attachment_rx = None;
                Some(Notice::warning(
                    CATALOG.t("mail-status-picker-disconnected"),
                ))
            }
        };
        if let Some(notice) = notice {
            self.shared.borrow_mut().status = Some(notice);
        }
        self.dirty = true;
    }

    fn drain_compose_attachment_contents(&mut self) {
        use ui::portal::ContentsOutcome;
        let outcome = self
            .compose_attachment_contents
            .as_ref()
            .map(std::sync::mpsc::Receiver::try_recv);
        let Some(outcome) = outcome else { return };
        let status = match outcome {
            Err(TryRecvError::Empty) => return,
            Ok(ContentsOutcome::Picked(files)) => match files.into_iter().next() {
                Some(_) if !self.compose.open => {
                    Notice::info(CATALOG.t("mail-status-selection-composer-closed"))
                }
                Some(file) => {
                    let name = file.name;
                    let result =
                        crate::attachment_store::stage_bytes(&self.content, &file.bytes, &name)
                            .and_then(|attachment| self.compose.add_attachment(attachment));
                    staged_notice(name, result)
                }
                None => Notice::info(CATALOG.t("mail-status-selection-cancelled")),
            },
            Ok(ContentsOutcome::Cancelled) => {
                Notice::info(CATALOG.t("mail-status-selection-cancelled"))
            }
            Ok(ContentsOutcome::Failed(error)) => Notice::error(CATALOG.t_args(
                "mail-status-picker-failed",
                &[("error", ArgValue::Text(error))],
            )),
            Err(TryRecvError::Disconnected) => {
                Notice::warning(CATALOG.t("mail-status-picker-disconnected"))
            }
        };
        self.compose_attachment_contents = None;
        self.shared.borrow_mut().status = Some(status);
        self.dirty = true;
    }

    fn process_intents(&mut self) {
        let intents = self.shared.borrow_mut().take_intents();
        for intent in intents {
            match intent {
                Intent::ShowDetail => self.nav.show(NavPane::Content),
                Intent::ShowOverview => self.nav.show(NavPane::Overview),
                Intent::CloseDrawer => {
                    if self.nav.geometry().sidebar == SidebarPlacement::Drawer {
                        self.nav.close_sidebar();
                    }
                }
                Intent::Compose => {
                    self.editing_draft = None;
                    self.compose.open_blank();
                    self.close_menu();
                }
                Intent::Reply(id) => self.open_reply(id),
                Intent::ReplyAll(id) => self.open_reply_all(id),
                Intent::Forward(id) => self.open_forward(id),
                Intent::EditDraft(id) => self.open_draft(id),
                Intent::ContextMenu(id, at, row) => self.open_menu(id, at, Some(row)),
                Intent::ReaderMenu(id, at, choices) => self.open_reader_menu(id, at, choices),
                Intent::OpenLink(url) => open_link(&url),
                Intent::SaveAttachment(message_id, index) => {
                    self.request_attachment_save(message_id, index);
                }
                Intent::OpenAttachment(message_id, index) => {
                    self.open_attachment(message_id, index);
                }
                Intent::Engine(req) => {
                    self.state().post_engine(req);
                }
                Intent::ConfirmBulkPurge => {
                    let count = self.shared.borrow().selected_bulk_ids().len();
                    if count == 0 {
                        self.shared.borrow_mut().status = Some(Notice::warning(
                            CATALOG.t("mail-status-select-before-purge"),
                        ));
                    } else if self.shared.borrow().selected_bulk_ids().iter().all(|id| {
                        self.shared
                            .borrow()
                            .mailbox
                            .get(*id)
                            .is_some_and(|message| message.folder == Folder::Trash)
                    }) {
                        self.open_purge_confirmation(count);
                    } else {
                        self.shared.borrow_mut().status =
                            Some(Notice::warning(CATALOG.t("mail-status-purge-needs-trash")));
                    }
                }
                Intent::AccountSettings => self.open_account_settings(),
                Intent::CheckMail => {
                    let _ = self.state().refresh_active_folder();
                }
            }
        }
        self.dirty = true;
    }

    fn apply_commands(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let commands = self.shared.borrow_mut().take_commands();
        for command in commands {
            match command {
                Command::OpenFolder(account, folder) => {
                    let mut state = self.shared.borrow_mut();
                    state.set_active_account(account);
                    state.select_folder(doc, folder);
                }
                Command::OpenOutbox(account) => self.shared.borrow_mut().open_outbox(account),
                Command::CancelSend(id) => {
                    self.shared.borrow_mut().cancel_send(doc, &id);
                }
                Command::OpenAllInboxes => self.shared.borrow_mut().open_all_inboxes(),
                Command::AccountSettings => self.open_account_settings(),
                Command::Compose => {
                    self.shared.borrow_mut().push_intent(Intent::Compose);
                }
                Command::CloseCompose => {
                    if self.compose.open {
                        self.compose.close();
                        self.close_compose(doc);
                    }
                }
                Command::DiscardCompose => {
                    if self.compose.open {
                        self.compose.close();
                        self.discard_compose(doc);
                    }
                }
                Command::ContactRecipients => {
                    self.state().open_contact_recipients();
                }
                Command::ToggleBulk => {
                    let selecting = self.shared.borrow().bulk_selecting;
                    self.shared.borrow_mut().set_bulk_selecting(!selecting);
                }
                Command::SelectAllVisible => self.select_all_in_folder(),
                Command::Bulk(BulkAction::Purge) => {
                    self.shared
                        .borrow_mut()
                        .push_intent(Intent::ConfirmBulkPurge);
                }
                Command::Bulk(action) => {
                    let affected = self.shared.borrow_mut().apply_bulk_action(doc, action);
                    if affected > 0 {
                        #[allow(
                            clippy::cast_precision_loss,
                            reason = "a count only selects a plural form; exactness past 2^52 is \
irrelevant"
                        )]
                        let count = affected as f64;
                        self.shared.borrow_mut().status = Some(Notice::success(CATALOG.t_args(
                            match action {
                                BulkAction::Archive => "mail-status-archived",
                                BulkAction::Delete => "mail-status-trashed",
                                BulkAction::MarkRead => "mail-status-marked-read",
                                BulkAction::MarkUnread => "mail-status-marked-unread",
                                BulkAction::Purge => "mail-status-deleted",
                            },
                            &[("count", ArgValue::Number(count))],
                        )));
                    }
                }
                Command::Message(action) => {
                    let selected = self.shared.borrow().selected;
                    if let Some(id) = selected {
                        self.shared
                            .borrow_mut()
                            .apply_message_action(doc, id, action);
                    }
                }
            }
        }
        self.process_intents();
    }

    fn open_menu(&mut self, id: MessageId, at: Point, row: Option<Rect>) {
        let (items, read, starred) = {
            let state = self.shared.borrow();
            let Some(message) = state.mailbox.get(id) else {
                return;
            };
            (state.context_items_for(id), message.read, message.starred)
        };
        if items.is_empty() {
            return;
        }
        let mut menu = ContextMenu::new();
        for item in &items {
            let label = item.label(read, starred);
            let entry = if *item == CtxAction::Delete {
                MenuItem::destructive_selection(label)
            } else {
                MenuItem::selection(label)
            };
            menu.push(entry.with_icon(item.icon(read)));
        }
        if let Some(row) = row {
            menu.anchor_row(row);
        }
        menu.set_viewport(Some(self.rect));
        menu.open_at_clamped(
            [ui::num::f32_of_f64(at.x), ui::num::f32_of_f64(at.y)],
            self.rect,
        );
        menu.focus_first_action();
        self.menu = menu;
        self.menu_target = Some(MenuTarget {
            message: id,
            items,
            reader: Vec::new(),
        });
        self.dirty = true;
    }

    fn close_menu(&mut self) {
        self.menu.close();
        self.menu_target = None;
        self.dirty = true;
    }

    fn take_menu_choice(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(index) = self.menu.take_selection() else {
            if !self.menu.is_open() {
                self.menu_target = None;
            }
            return;
        };
        let Some(target) = self.menu_target.take() else {
            return;
        };
        self.menu.close();
        if !target.reader.is_empty() {
            if let Some(Some(choice)) = target.reader.get(index).cloned() {
                self.apply_reader_choice(target.message, choice);
            }
            return;
        }
        let Some(action) = target.items.get(index).copied() else {
            return;
        };
        self.apply_menu_action(target.message, action, doc);
    }

    fn open_reader_menu(
        &mut self,
        id: MessageId,
        at: Point,
        choices: Vec<crate::state::ReaderChoice>,
    ) {
        use crate::state::ReaderChoice;
        let t = crate::i18n::t;
        let mut menu = ContextMenu::new();
        let mut rows = Vec::new();
        if let Some(ReaderChoice::OpenLink(url)) = choices.first() {
            menu.push(MenuItem::caption(url.clone()));
            rows.push(None);
        }
        for choice in choices {
            let (label, icon) = match &choice {
                ReaderChoice::OpenLink(_) => (t("mail-link-open"), Icon::ExternalLink),
                ReaderChoice::CopyLink(_) => (t("mail-link-copy"), Icon::Link),
                ReaderChoice::Copy => (t("mail-copy"), Icon::Copy),
                ReaderChoice::SelectAll => (t("mail-select-all"), Icon::SquareDashed),
                ReaderChoice::ShowPlainText => (t("mail-show-plain"), Icon::FileText),
                ReaderChoice::ShowFormatted => (t("mail-show-formatted"), Icon::Mail),
            };
            menu.push(MenuItem::selection(label).with_icon(icon));
            rows.push(Some(choice));
        }
        menu.set_viewport(Some(self.rect));
        menu.open_at_clamped(
            [ui::num::f32_of_f64(at.x), ui::num::f32_of_f64(at.y)],
            self.rect,
        );
        menu.focus_first_action();
        self.menu = menu;
        self.menu_target = Some(MenuTarget {
            message: id,
            items: Vec::new(),
            reader: rows,
        });
        self.dirty = true;
    }

    fn apply_reader_choice(&mut self, message: MessageId, choice: crate::state::ReaderChoice) {
        use crate::state::ReaderChoice;
        match choice {
            ReaderChoice::OpenLink(url) => open_link(&url),
            ReaderChoice::CopyLink(url) => ui::clipboard_copy(&url),
            ReaderChoice::Copy | ReaderChoice::SelectAll => {
                self.shared.borrow_mut().reader_requests.push(choice);
            }
            ReaderChoice::ShowPlainText => {
                self.shared.borrow_mut().plain_text.insert(message);
            }
            ReaderChoice::ShowFormatted => {
                self.shared.borrow_mut().plain_text.remove(&message);
            }
        }
        self.shared.borrow_mut().dirty = true;
        self.dirty = true;
    }

    fn apply_menu_action(
        &mut self,
        message: MessageId,
        action: CtxAction,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        if self.shared.borrow().mailbox.get(message).is_none() {
            self.shared.borrow_mut().status =
                Some(Notice::warning(CATALOG.t("mail-status-message-gone")));
            self.dirty = true;
            return;
        }
        self.shared
            .borrow_mut()
            .apply_message_action(doc, message, action);
        self.process_intents();
    }

    fn open_menu_on_row(&mut self, id: MessageId, text: &mut TextRenderer, theme: &Theme) {
        let index = self
            .shared
            .borrow()
            .list_rows()
            .iter()
            .position(|(row, _)| *row == id);
        let Some(index) = index else {
            return;
        };
        let geometry = self.nav.geometry();
        let column = geometry.overview_column;
        if column.width() <= 0.0 {
            return;
        }
        let row_h = ui::widget::ListRowFace::new("Ag")
            .secondary("Ag")
            .two_line_secondary()
            .height(text, theme);
        let list = Rect::new(
            column.x0,
            column.y0 + geometry.header_height,
            column.x1,
            column.y1,
        );
        let row = crate::list::RowFrame::new(list, row_h, false, theme).row(index, 0.0);
        let at = Point::new(column.x0 + column.width() * 0.6, row.center().y);
        self.open_menu(id, at, Some(row));
    }

    fn open_purge_confirmation(&mut self, count: usize) {
        let answer = Rc::new(Cell::new(None));
        #[allow(
            clippy::cast_precision_loss,
            reason = "a count only selects a plural form; exactness past 2^52 is irrelevant"
        )]
        let number = count as f64;
        let body =
            Text::new(&CATALOG.t_args("mail-purge-body", &[("count", ArgValue::Number(number))]))
                .with_role(TextRole::Body);
        let confirm = Rc::clone(&answer);
        let cancel = Rc::clone(&answer);
        let dismiss = Rc::clone(&answer);
        let dialog = ModalDialog::new(
            CATALOG.t_args("mail-purge-title", &[("count", ArgValue::Number(number))]),
            body,
        )
        .with_test_id("mail-purge-confirmation")
        .with_destructive(true)
        .primary_action(CATALOG.t("mail-delete-permanently"), move || {
            confirm.set(Some(true));
        })
        .secondary_action(CATALOG.t("mail-cancel"), move || cancel.set(Some(false)))
        .on_dismiss(move || dismiss.set(Some(false)))
        .open(true);
        self.bulk_purge_confirmation = Some(PurgeConfirmation { dialog, answer });
        self.dirty = true;
    }

    fn settle_purge_confirmation(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(answer) = self
            .bulk_purge_confirmation
            .as_ref()
            .and_then(|confirmation| confirmation.answer.take())
        else {
            return;
        };
        if answer {
            self.confirm_bulk_purge(doc);
        } else {
            self.bulk_purge_confirmation = None;
            self.dirty = true;
        }
    }

    fn reply_subject(msg: &Message) -> String {
        if msg.subject.to_ascii_lowercase().starts_with("re:") {
            msg.subject.clone()
        } else {
            format!("Re: {}", msg.subject)
        }
    }

    fn reply_quote(msg: &Message) -> String {
        let quoted: Vec<String> = msg
            .body
            .lines()
            .map(|line| {
                let line = line.trim_end_matches('\r');
                if line.is_empty() {
                    ">".to_owned()
                } else {
                    format!("> {line}")
                }
            })
            .collect();
        format!(
            "\n\nOn {}, {} wrote:\n{}",
            format_absolute(crate::util::local_minutes(msg.timestamp_min)),
            msg.from.display(),
            quoted.join("\n")
        )
    }

    fn answer_from_its_account(&mut self, id: MessageId) {
        let owner = self.shared.borrow().account_of(id);
        if let Some(account) = owner {
            self.compose.send_from(account.email);
        }
    }

    fn sending_account(&self) -> AccountConfig {
        let st = self.shared.borrow();
        self.compose
            .sending_from()
            .and_then(|address| {
                st.accounts
                    .accounts
                    .iter()
                    .find(|account| account.config.email == address)
                    .map(|account| account.config.clone())
            })
            .unwrap_or_else(|| st.accounts.active_config())
    }

    fn keep_compose_headers(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(headers) = self.compose.take_due_headers() else {
            return;
        };
        self.store_compose_headers(doc, headers);
    }

    fn store_compose_headers(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        [to, cc, bcc, subject]: [String; 4],
    ) {
        let editing = self.compose.draft_id().map(str::to_owned);
        let Some(kept) = read_mail(doc.doc())
            .unwrap_or_default()
            .draft_at(editing.as_deref())
            .cloned()
        else {
            return;
        };
        let draft = crate::resource::draft_paths(editing.as_deref());
        let st = self.shared.borrow();
        if kept.to != to {
            st.submit(doc, draft.to.set(to));
        }
        if kept.cc != cc {
            st.submit(doc, draft.cc.set(cc));
        }
        if kept.bcc != bcc {
            st.submit(doc, draft.bcc.set(bcc));
        }
        if kept.subject != subject {
            st.submit(doc, draft.subject.set(subject));
        }
    }

    fn close_compose(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if self.compose.touched() {
            self.save_compose_draft(doc);
        }
        self.dirty = true;
    }

    fn discard_compose(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let draft = self.compose.draft_id().map(str::to_owned);
        self.shared.borrow().submit(
            doc,
            Mail::DISCARD_DRAFT.with(mailbox::DiscardDraft { draft }),
        );
        if let Some(draft) = self.editing_draft.take() {
            self.shared.borrow_mut().mailbox.purge(draft);
        }
        if let Some((projected, _)) = self.projected_draft.borrow_mut().take() {
            self.shared.borrow_mut().mailbox.purge(projected);
        }
        self.dirty = true;
    }

    fn open_reply(&mut self, id: MessageId) {
        let Some(msg) = self.shared.borrow().mailbox.get(id).cloned() else {
            return;
        };
        self.editing_draft = None;
        let subj = Self::reply_subject(&msg);
        let quote = Self::reply_quote(&msg);
        self.compose.open_reply(&msg.from.rfc5322(), &subj, &quote);
        self.compose.reply_to(id);
        self.answer_from_its_account(id);
        self.close_menu();
        self.dirty = true;
    }

    fn open_reply_all(&mut self, id: MessageId) {
        let (msg, my_email) = {
            let st = self.shared.borrow();
            let mine = st
                .account_of(id)
                .unwrap_or_else(|| st.accounts.active_config())
                .email
                .to_ascii_lowercase();
            match st.mailbox.get(id).cloned() {
                Some(m) => (m, mine),
                None => return,
            }
        };
        self.editing_draft = None;
        let subj = Self::reply_subject(&msg);
        let quote = Self::reply_quote(&msg);
        let sender = msg.from.email.to_ascii_lowercase();
        let mut cc: Vec<String> = Vec::new();
        for a in msg.to.iter().chain(msg.cc.iter()) {
            let e = a.email.to_ascii_lowercase();
            if e == my_email || e == sender || e.is_empty() {
                continue;
            }
            if cc
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&a.rfc5322()))
            {
                continue;
            }
            cc.push(a.rfc5322());
        }
        self.compose
            .open_reply_all(&msg.from.rfc5322(), &cc.join(", "), &subj, &quote);
        self.compose.reply_to(id);
        self.answer_from_its_account(id);
        self.close_menu();
        self.dirty = true;
    }

    fn open_draft(&mut self, id: MessageId) {
        let named = id.to_string();
        let own = self.live.as_ref().and_then(|live| {
            live.get()
                .drafts
                .get(&ui::tilo_resource::Id::from_raw(named.as_str()))
                .map(crate::resource::DraftState::draft)
        });
        if let Some(draft) = own {
            self.editing_draft = None;
            self.compose.open_draft(Some(&named), &draft);
            self.close_menu();
            self.dirty = true;
            return;
        }
        let draft = self
            .projected_draft
            .borrow()
            .as_ref()
            .filter(|(projected, _)| *projected == id)
            .map(|(_, draft)| draft.clone())
            .or_else(|| self.shared.borrow().mailbox.get(id).map(draft_from_message));
        let Some(draft) = draft else {
            return;
        };
        self.editing_draft = Some(id);
        self.compose.open_with_draft(&draft);
        self.close_menu();
        self.dirty = true;
    }

    fn open_forward(&mut self, id: MessageId) {
        let Some(msg) = self.shared.borrow().mailbox.get(id).cloned() else {
            return;
        };
        self.editing_draft = None;
        let subj = format!("Fwd: {}", msg.thread_subject());
        let quote = format!(
            "\n\n---------- Forwarded message ----------\nFrom: {}\nSubject: {}\n\n{}",
            msg.from.rfc5322(),
            msg.subject,
            msg.body
        );
        let forwarded = self.forwarded_files(&msg);
        self.compose.open_forward(&subj, &quote, &forwarded.carried);
        let left_behind = [
            ("mail-forward-not-downloaded", &forwarded.not_downloaded),
            ("mail-forward-too-large", &forwarded.too_large),
        ]
        .into_iter()
        .filter(|(_, files)| !files.is_empty())
        .map(|(key, files)| {
            crate::i18n::t_args(
                key,
                &[
                    (
                        "count",
                        ui::i18n::ArgValue::Number(f64::from(
                            u32::try_from(files.len()).unwrap_or(u32::MAX),
                        )),
                    ),
                    ("files", ui::i18n::ArgValue::Text(files.join(", "))),
                ],
            )
        })
        .collect::<Vec<_>>();
        if !left_behind.is_empty() {
            self.shared.borrow_mut().status = Some(Notice::warning(left_behind.join("\n")));
        }
        self.close_menu();
        self.dirty = true;
    }

    fn forwarded_files(&self, msg: &Message) -> ForwardedFiles {
        let mut files = Vec::new();
        let mut not_downloaded = Vec::new();
        let mut too_large = Vec::new();
        for (index, attachment) in msg.shown_attachments() {
            // Past what one message carries whether it is downloaded or
            // not: said so, and never read or kept for nothing.
            if attachment.size > crate::attachment_store::MAX_ATTACHMENT_BYTES as u64 {
                too_large.push(attachment.filename.clone());
                continue;
            }
            let staged = self
                .state()
                .attachment_to_save(&msg.id.to_string(), index)
                .map_err(|_| ())
                .and_then(|(_, bytes)| {
                    crate::attachment_store::stage_bytes(
                        &self.content,
                        &bytes,
                        &attachment.filename,
                    )
                    .map_err(|_| ())
                });
            match staged {
                Ok(file) => files.push(file),
                Err(()) => not_downloaded.push(attachment.filename.clone()),
            }
        }
        let carried = crate::attachment_store::bounded(&files);
        too_large.extend(
            files
                .iter()
                .filter(|file| {
                    !carried
                        .iter()
                        .any(|kept| kept.content_id == file.content_id)
                })
                .map(|file| file.filename.clone()),
        );
        ForwardedFiles {
            carried,
            not_downloaded,
            too_large,
        }
    }

    fn send_compose(&mut self, doc: &mut ui::widget::Edits<'_>) {
        self.store_preferences_if_changed();
        let draft = self.compose.to_draft(doc);
        let acct = self.sending_account();
        if !self.contact_sender_matches(&draft) {
            self.shared.borrow_mut().status = Some(Notice::warning(crate::i18n::t(
                "mail-contact-sender-changed",
            )));
            self.dirty = true;
            return;
        }
        if acct.email.trim().is_empty() {
            self.shared.borrow_mut().status =
                Some(Notice::warning(CATALOG.t("mail-status-no-sending-account")));
            self.dirty = true;
            return;
        }
        if self.shared.borrow().dispatcher.is_none() {
            self.shared.borrow_mut().status = Some(Notice::warning(
                CATALOG.t("mail-status-sending-unavailable"),
            ));
            self.dirty = true;
            return;
        }
        let from = Addr::new(acct.display_name.clone(), acct.email.clone());
        if let Err(e) = crate::transport::build_mime(&self.content, &from, &draft) {
            self.shared.borrow_mut().status = Some(Notice::error(
                CATALOG.t_args("mail-status-send-failed", &[("error", ArgValue::Text(e))]),
            ));
            self.dirty = true;
            return;
        }
        if [&draft.to, &draft.cc, &draft.bcc]
            .into_iter()
            .all(|field| crate::transport::parse_addr_list(field).is_empty())
        {
            self.shared.borrow_mut().status =
                Some(Notice::warning(CATALOG.t("mail-status-no-recipient")));
            self.dirty = true;
            return;
        }
        // The draft holds what is sent, even what was typed a moment ago,
        // so sending it empties the draft.
        self.store_compose_headers(
            doc,
            [
                draft.to.clone(),
                draft.cc.clone(),
                draft.bcc.clone(),
                draft.subject.clone(),
            ],
        );
        {
            let st = self.shared.borrow();
            if let Some(refused) = self.resending.take() {
                st.submit(
                    doc,
                    Mail::CANCEL_SEND.with(mailbox::CancelSend { id: refused }),
                );
            }
            // The press is the person's own: Undo on the sending notice is
            // its way back, so nothing is asked.
            st.submit_gesture(
                doc,
                Mail::SEND_MESSAGE.with(mailbox::SendMessage {
                    account: acct.email,
                    message: DraftData::from_draft(&draft),
                    in_reply_to: draft.in_reply_to.map(|id| id.to_string()),
                    draft: self.compose.draft_id().map(str::to_owned),
                }),
            );
        }
        // The message is in the outbox now; the draft it was written in is
        // done with.
        if let Some(draft_id) = self.editing_draft.take() {
            let mut st = self.shared.borrow_mut();
            st.mailbox.purge(draft_id);
        }
        self.compose.close();
        // It waits out the undo time before it goes; the notice's Undo
        // takes it back.
        self.sends_to_hold += 1;
        self.sending_notice_next = true;
        self.shared
            .borrow_mut()
            .say_undoable(ToastKind::Info, CATALOG.t("mail-status-sending"));
        self.dirty = true;
    }

    fn release_held_sends(&mut self, doc: &ui::widget::Edits<'_>) {
        if self.held_sends.is_empty() {
            return;
        }
        let now = ui::time::now();
        let waiting: HashSet<String> = read_mail(doc.doc())
            .map(|mail| {
                mail.outbox
                    .pending()
                    .map(|(id, _)| id.as_str().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        for held in std::mem::take(&mut self.held_sends) {
            if !waiting.contains(&held.outgoing.id) {
                self.reopen_unsent(&held.outgoing);
            } else if held.due <= now {
                self.deliver(held.outgoing, held.done);
            } else {
                self.held_sends.push(held);
            }
        }
    }

    fn take_back_sends(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if !std::mem::take(&mut self.unsend) {
            return;
        }
        self.sends_to_hold = 0;
        for held in std::mem::take(&mut self.held_sends) {
            self.shared.borrow().submit(
                doc,
                Mail::CANCEL_SEND.with(mailbox::CancelSend {
                    id: held.outgoing.id.clone(),
                }),
            );
            self.reopen_unsent(&held.outgoing);
        }
        self.dirty = true;
    }

    fn reopen_unsent(&mut self, outgoing: &Outgoing) {
        if !self.compose.open {
            self.compose.open_with_draft(&outgoing.to_draft());
        }
        self.dirty = true;
    }

    fn save_compose_draft(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let draft = self.compose.to_draft(doc);
        if let Some(id) = self.compose.draft_id().map(str::to_owned) {
            self.shared
                .borrow_mut()
                .save_draft_as(doc, Some(&id), &draft);
            self.compose.close();
            self.store_preferences_if_changed();
            self.dirty = true;
            return;
        }
        // Persist the draft as an action (survives a crash / syncs to peers /
        // one undo boundary) …
        self.shared.borrow_mut().save_draft(doc, &draft);
        // … and materialise it as a real, visible message in the Drafts folder
        // so it can be found and reopened — not a write-only slot.
        let id = self.upsert_draft_message(doc, &draft, self.editing_draft);
        self.editing_draft = Some(id);
        self.compose.close();
        self.store_preferences_if_changed();
        self.dirty = true;
    }

    fn upsert_draft_message(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        draft: &Draft,
        edited: Option<MessageId>,
    ) -> MessageId {
        let shown_as = read_mail(doc.doc())
            .and_then(|mail| mail.draft.shown_as)
            .and_then(|id| id.parse::<MessageId>().ok());
        let id = edited
            .or(shown_as)
            .or_else(|| self.projected_draft.borrow().as_ref().map(|(id, _)| *id))
            .unwrap_or_else(uuid::Uuid::new_v4);
        if shown_as != Some(id) {
            self.shared.borrow().submit(
                doc,
                Mail::SHOW_DRAFT.with(mailbox::ShowDraft {
                    message: id.to_string(),
                    draft: None,
                }),
            );
        }
        let msg = self.draft_shown(id, draft);
        {
            let mut st = self.shared.borrow_mut();
            st.mailbox.insert(msg);
            st.register_message_target(doc, &id.to_string());
        }
        *self.projected_draft.borrow_mut() = Some((id, draft.clone()));
        id
    }

    fn draft_shown(&self, id: MessageId, draft: &Draft) -> Message {
        let (from, now) = {
            let st = self.shared.borrow();
            let acct = st.accounts.active_config();
            let from = draft.sender.as_ref().map_or_else(
                || Addr::new(acct.display_name.clone(), acct.email),
                |sender| {
                    Addr::new(
                        st.accounts
                            .accounts
                            .iter()
                            .find(|account| {
                                account.id == sender.account && account.config.email == sender.email
                            })
                            .map_or_else(String::new, |account| {
                                account.config.display_name.clone()
                            }),
                        sender.email.clone(),
                    )
                },
            );
            (from, st.now_min)
        };
        Message {
            id,
            rfc_message_id: None,
            in_reply_to: None,
            from,
            to: crate::transport::parse_addr_list(&draft.to),
            cc: crate::transport::parse_addr_list(&draft.cc),
            subject: if draft.subject.trim().is_empty() {
                crate::i18n::no_subject()
            } else {
                draft.subject.clone()
            },
            body: draft.body.clone(),
            html_body: draft.html_body.clone(),
            attachments: draft.attachments.iter().map(attachment_meta).collect(),
            timestamp_min: now,
            read: true,
            starred: false,
            folder: Folder::Drafts,
            labels: Vec::new(),
            account: String::new(),
            server: None,
        }
    }

    fn project_drafts(&self, mail: &Mail) {
        let mut projected = self.projected_drafts.borrow_mut();
        let mut kept = HashSet::new();
        for (id, draft) in mail.drafts.iter() {
            let Ok(message_id) = id.as_str().parse::<MessageId>() else {
                continue;
            };
            kept.insert(message_id);
            let mut shown = self.draft_shown(message_id, &draft.draft());
            let mut st = self.shared.borrow_mut();
            if let Some(present) = st.mailbox.get(message_id) {
                shown.timestamp_min = present.timestamp_min;
                if *present == shown {
                    continue;
                }
            }
            st.mailbox.insert(shown);
            st.dirty = true;
        }
        for gone in projected.difference(&kept) {
            self.shared.borrow_mut().mailbox.purge(*gone);
        }
        *projected = kept;
    }

    fn is_first_run(&self) -> bool {
        let state = self.shared.borrow();
        state.accounts.is_empty() && state.mailbox.messages.is_empty()
    }

    fn tell_compose_the_accounts(&mut self) {
        let has_account = !self.shared.borrow().accounts.is_empty();
        self.compose.set_has_account(has_account);
    }

    fn open_account_settings(&mut self) {
        #[cfg(test)]
        self.opened_links.push(ACCOUNT_SETTINGS_LINK);
        #[cfg(not(test))]
        ui::widget::open_external_url(ACCOUNT_SETTINGS_LINK);
        self.close_menu();
        self.dirty = true;
    }
}

impl Mailroom {
    fn refresh_active_folder(&mut self) -> Option<EngineRequestId> {
        let (account, folder) = {
            let st = self.shared.borrow();
            (st.accounts.active_config(), st.folder)
        };
        if account.email.is_empty() {
            return None;
        }
        if self.engine.is_some() {
            let account_id = account.email.clone();
            let request_id = self.post_engine(EngineRequest::fetch(account, folder))?;
            report_account_sync(
                self.accounts_persist.root().as_deref(),
                &account_id,
                accounts::AccountSyncHealth::Syncing,
            );
            // Checking for mail is routine: new mail appearing in the list
            // is all it shows.
            self.last_auto_refresh_min = self.shared.borrow().now_min;
            self.dirty = true;
            return Some(request_id);
        }
        None
    }

    fn maybe_auto_refresh(&mut self) {
        if self.awaiting_engine || self.engine.is_none() {
            return;
        }
        let (now, has_account) = {
            let st = self.shared.borrow();
            (st.now_min, !st.accounts.is_empty())
        };
        if !has_account {
            return;
        }
        if self.last_auto_refresh_min == 0 {
            // Seed the clock on the first frame so the initial connect fetch
            // isn't immediately followed by a redundant refresh.
            self.last_auto_refresh_min = now;
            return;
        }
        if now - self.last_auto_refresh_min >= AUTO_REFRESH_MIN {
            // Due either way: one that cannot run now waits a whole period
            // rather than waking the window again at once.
            self.last_auto_refresh_min = now;
            let _ = self.refresh_active_folder();
            self.fetch_other_inboxes();
        }
    }

    fn watch_inboxes(&mut self) {
        let accounts: Vec<AccountConfig> = self
            .shared
            .borrow()
            .accounts
            .accounts
            .iter()
            .map(|account| account.config.clone())
            .filter(|config| !config.email.is_empty())
            .collect();
        for account in accounts {
            self.post_engine(EngineRequest::watch(account));
        }
    }

    fn fetch_other_inboxes(&mut self) {
        let others: Vec<AccountConfig> = {
            let st = self.shared.borrow();
            st.accounts
                .accounts
                .iter()
                .enumerate()
                .filter(|(index, account)| {
                    *index != st.accounts.active && !account.config.email.is_empty()
                })
                .map(|(_, account)| account.config.clone())
                .collect()
        };
        for account in others {
            self.post_engine(EngineRequest::fetch(account, Folder::Inbox));
        }
    }

    fn next_refresh_at(&self) -> Option<ui::time::Instant> {
        if self.engine.is_none() || self.awaiting_engine || self.last_auto_refresh_min == 0 {
            return None;
        }
        if self.shared.borrow().accounts.is_empty() {
            return None;
        }
        let due = self.last_auto_refresh_min + AUTO_REFRESH_MIN;
        let minutes = (due - crate::state::now_minutes()).max(0);
        let seconds = u64::try_from(minutes).unwrap_or(0) * 60 + 1;
        Some(ui::time::now() + std::time::Duration::from_secs(seconds))
    }
}

impl MailApp {
    fn show_status(&mut self) {
        // A message held for undo keeps its notice up: the notice used to die
        // after four seconds while the message could still be taken back for
        // ten, so anyone reaching for Undo a beat late found nothing and the
        // mail went out (`APP-mail-01`). While a send is held, the notice is
        // put back up whenever it goes.
        if self.send_notice
            && (!self.held_sends.is_empty() || self.sends_to_hold > 0)
            && self.nav.toast_text().is_none()
        {
            self.nav.toast_with_action(
                CATALOG.t("mail-status-sending"),
                ToastKind::Info,
                "undo-2",
                CATALOG.t("mail-undo"),
            );
        }
        let (status, undoable) = {
            let mut st = self.shared.borrow_mut();
            (st.status.take(), std::mem::take(&mut st.status_undo))
        };
        let Some(notice) = status else {
            return;
        };
        self.send_notice = std::mem::take(&mut self.sending_notice_next);
        if undoable {
            self.nav
                .toast_with_action(notice.text, notice.kind, "undo-2", CATALOG.t("mail-undo"));
        } else {
            self.nav.toast(notice.text, notice.kind);
        }
    }

    fn fetch_arrived(&mut self, st: &MailState, account_id: &str) {
        if let Some(account) = st
            .accounts
            .accounts
            .iter()
            .find(|account| account.config.email == account_id)
        {
            self.pending_watch_fetches.push(account.config.clone());
        }
    }

    fn keep_on_device(&self, st: &mut MailState, account_id: &str) {
        let kept = self.state().store.save(
            account_id,
            st.mailbox.of_account(account_id).cloned().collect(),
        );
        for (id, index) in kept {
            if let Some(attachment) = st
                .mailbox
                .messages
                .get_mut(&id)
                .and_then(|message| message.attachments.get_mut(index))
            {
                attachment.kept = true;
            }
        }
    }

    fn apply_fetched_messages(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        st: &mut MailState,
        request_id: EngineRequestId,
        account_id: &str,
        folder: Folder,
        mut synced: crate::sync::Synced,
    ) {
        pass_over_draft_copies(
            st,
            &read_mail(doc.doc()).unwrap_or_default(),
            account_id,
            folder,
            &mut synced,
        );
        let (gone, reflagged) = server_changes(&st.mailbox, account_id, folder, &synced);
        for id in &gone {
            st.mailbox.purge(*id);
        }
        if let Some(copies) = &self.copies {
            // A folder that started over drops what is not in its newest
            // page from this device only; what its server reported removed
            // is gone for every device.
            if !synced.reset {
                copies.forget(&gone);
            }
            copies.keep(account_id, &synced.messages, &synced.originals);
        }
        if account_id == st.accounts.active_config().email {
            st.first_fetch = None;
        }
        st.ingest_fetched(doc, account_id, &reflagged);
        st.ingest_fetched(doc, account_id, &synced.messages);
        for projection in [&self.projection, &self.delivery].into_iter().flatten() {
            projection.outside_changed();
        }
        // New mail in an inbox the device already held is news; a first
        // fetch or a folder starting over is not.
        if !synced.reset && folder == Folder::Inbox {
            let arrived: Vec<&Message> = synced
                .messages
                .iter()
                .filter(|message| !message.read && st.mailbox.get(message.id).is_none())
                .collect();
            if !arrived.is_empty() {
                self.notify_new_mail(account_id, &arrived, st.accounts.accounts.len() > 1);
            }
        }
        for message in synced.messages {
            st.mailbox.insert(message);
        }
        if let Some(older) = synced.has_older {
            self.has_older
                .insert((account_id.to_owned(), folder), older);
        }
        if st.folder == folder && st.selected.is_none() {
            st.selected = st
                .current_threads()
                .first()
                .and_then(|thread| thread.message_ids.last().copied());
        }
        self.keep_on_device(st, account_id);
        // The account works again: a failure it showed is over.
        st.clear_sync_failure();
        report_account_sync(
            self.state().accounts_persist.root().as_deref(),
            account_id,
            accounts::AccountSyncHealth::Healthy,
        );
        let mut room = self.state();
        if let Some((pending, invocation)) = room.resource_refresh_invocation
            && pending == request_id
        {
            room.resource_refresh_result = Some(tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            });
        }
    }

    fn open_kept_mail(&mut self) {
        let accounts: Vec<String> = self
            .shared
            .borrow()
            .accounts
            .accounts
            .iter()
            .map(|account| account.config.email.clone())
            .filter(|email| !email.is_empty() && !self.kept_loaded.contains(email))
            .collect();
        // A draft's server copy kept here before it was known as one is
        // the draft itself: it is not shown again.
        let copies: HashSet<MessageId> = self
            .live
            .as_ref()
            .map(|live| live.get().draft_copy_ids())
            .unwrap_or_default()
            .iter()
            .filter_map(|id| id.parse().ok())
            .collect();
        for email in accounts {
            let mut kept = self.state().store.load(&email);
            kept.retain(|message| !copies.contains(&message.id));
            self.kept_loaded.insert(email);
            if kept.is_empty() {
                continue;
            }
            let mut st = self.shared.borrow_mut();
            for message in kept {
                st.mailbox.insert(message);
            }
            self.kept_registered = false;
            st.dirty = true;
        }
    }

    fn register_kept_mail(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if std::mem::replace(&mut self.kept_registered, true) {
            return;
        }
        let mail = read_mail(doc.doc()).unwrap_or_default();
        let baselines: Vec<crate::actions::IngestBaseline> = self
            .shared
            .borrow()
            .mailbox
            .messages
            .values()
            .filter_map(|message| kept_baseline(&mail, message))
            .collect();
        if !baselines.is_empty() {
            self.shared.borrow().ingest_baselines(doc, baselines);
        }
    }

    fn older_pages_wanted(&self) -> Vec<(AccountConfig, Folder, crate::sync::Window)> {
        if self.state().engine.is_none() {
            return Vec::new();
        }
        let wanted: Vec<(AccountConfig, Folder, Option<crate::sync::Window>)> = {
            let st = self.shared.borrow();
            let accounts: Vec<AccountConfig> = if st.all_inboxes {
                st.accounts
                    .accounts
                    .iter()
                    .map(|account| account.config.clone())
                    .collect()
            } else {
                vec![st.accounts.active_config()]
            };
            accounts
                .into_iter()
                .map(|account| {
                    let known = crate::sync::Window::of(&st.mailbox, &account.email, st.folder);
                    (account, st.folder, known)
                })
                .collect()
        };
        wanted
            .into_iter()
            .filter_map(|(account, folder, known)| {
                let known = known?;
                let key = (account.email.clone(), folder);
                (self.has_older.get(&key) != Some(&false) && !self.older_pending.contains_key(&key))
                    .then_some((account, folder, known))
            })
            .collect()
    }

    /// Ask for the page before the oldest message of the open folder, once:
    pub(crate) fn load_older(&mut self) {
        for (account, folder, known) in self.older_pages_wanted() {
            let key = (account.email.clone(), folder);
            let posted = self.state().post_engine(EngineRequest::fetch_wanting(
                account,
                folder,
                crate::sync::Want::Older { known },
            ));
            if let Some(request) = posted {
                self.older_pending.insert(key, request);
            }
        }
    }

    fn apply_sent_message(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        st: &mut MailState,
        request_id: EngineRequestId,
    ) {
        let Some(pending) = self.state().pending_send.remove(&request_id) else {
            return;
        };
        st.ingest_messages(doc, std::slice::from_ref(&pending.message));
        st.mailbox.insert(pending.message);
        (pending.done)(Ok(ui::tilo_resource::Settled::Done(())));
        st.status = Some(Notice::success(CATALOG.t("mail-status-sent")));
        self.editing_draft = None;
        self.compose.close();
    }

    fn apply_engine_error(
        &mut self,
        st: &mut MailState,
        request_id: EngineRequestId,
        kind: EngineRequestKind,
        context: &str,
        message: &str,
    ) {
        if context == "search" {
            tracing::debug!(%message, "mail: a server search failed");
            return;
        }
        let detail = format!("{context}: {message}");
        let refused = self.server_outbox.borrow_mut().sent.remove(&request_id);
        if let Some((_, done)) = refused {
            done(Err(detail.clone()));
        }
        let account_id = self
            .state()
            .pending_engine_requests
            .get(&request_id)
            .map_or_else(String::new, |pending| pending.account.clone());
        st.first_fetch = None;
        if matches!(kind, EngineRequestKind::Connect | EngineRequestKind::Fetch) {
            // A check that fails is not a notice that fades: the list says it
            // for as long as the account cannot be reached, in the window's
            // language.
            let account = st
                .accounts
                .label_for(&account_id)
                .unwrap_or_else(|| account_id.clone());
            st.note_sync_failure(account, crate::notice::sync_cause(message));
        } else {
            st.status = Some(engine_error_text(context, message));
        }
        let lower = detail.to_ascii_lowercase();
        let health = if ["auth", "login", "password", "oauth", "credential"]
            .iter()
            .any(|needle| lower.contains(needle))
        {
            accounts::AccountSyncHealth::NeedsAuthentication
        } else {
            accounts::AccountSyncHealth::Error
        };
        report_account_sync(
            self.state().accounts_persist.root().as_deref(),
            &account_id,
            health,
        );
        let refused = self.state().pending_send.remove(&request_id);
        if let Some(pending) = refused {
            // The server refused it: it stays in the outbox with the reason,
            // and comes back to the composer to be fixed and sent again.
            if !self.compose.open {
                self.compose.open_with_draft(&pending.outgoing.to_draft());
                self.resending = Some(pending.outgoing.id.clone());
            }
            (pending.done)(Ok(ui::tilo_resource::Settled::failed(detail.clone())));
        }
        let mut room = self.state();
        if let Some((pending, invocation)) = room.resource_refresh_invocation
            && pending == request_id
        {
            room.resource_refresh_result = Some(tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "mail.refresh_failed".into(),
                    message: detail,
                    details: None,
                    retryable: true,
                },
            });
        }
    }

    fn apply_engine_event(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        st: &mut MailState,
        event: EngineEvent,
    ) {
        match event {
            EngineEvent::Connected {
                request_id,
                account_id,
            } if self.expects_engine_event(request_id, EngineRequestKind::Connect) => {
                report_account_sync(
                    self.state().accounts_persist.root().as_deref(),
                    &account_id,
                    accounts::AccountSyncHealth::Syncing,
                );
            }
            EngineEvent::Folders {
                request_id,
                folders,
                ..
            } if self.expects_engine_event(request_id, EngineRequestKind::Connect) => {
                tracing::info!(count = folders.len(), "mail: server folders listed");
            }
            EngineEvent::Fetched {
                request_id,
                account_id,
                folder,
                synced,
            } if self.expects_engine_event(request_id, EngineRequestKind::Fetch) => {
                self.apply_fetched_messages(doc, st, request_id, &account_id, folder, synced);
            }
            EngineEvent::Sent { request_id, .. }
                if self.expects_engine_event(request_id, EngineRequestKind::Send) =>
            {
                self.apply_sent_message(doc, st, request_id);
            }
            EngineEvent::Found {
                request_id,
                account_id,
                messages,
            } if self.expects_engine_event(request_id, EngineRequestKind::Search) => {
                take_found(doc, st, &account_id, messages);
            }
            // A watch outlives its request: what it hears is answered for
            // any account this device still holds.
            EngineEvent::Arrived { account_id, .. } => self.fetch_arrived(st, &account_id),
            EngineEvent::Error {
                request_id,
                kind,
                context,
                message,
            } if self.expects_engine_event(request_id, kind) => {
                self.apply_engine_error(st, request_id, kind, &context, &message);
            }
            EngineEvent::Completed { request_id, kind }
                if self.expects_engine_event(request_id, kind) =>
            {
                // Only the worker acknowledgement settles a queued request:
                self.state().pending_engine_requests.remove(&request_id);
                self.older_pending
                    .retain(|_, pending| *pending != request_id);
                let sent = self.server_outbox.borrow_mut().sent.remove(&request_id);
                if let Some((update, done)) = sent {
                    st.submit(
                        doc,
                        Mail::SERVER_APPLIED.with(mailbox::ServerApplied { update }),
                    );
                    done(Ok(()));
                }
            }
            ignored => {
                tracing::warn!(request_id = %ignored.request_id(), "mail: ignoring unmatched or wrong-kind engine event");
            }
        }
    }

    fn flush_server_outbox(&mut self) {
        let deliveries = std::mem::take(&mut self.server_outbox.borrow_mut().deliveries);
        let due = ui::time::now() + std::time::Duration::from_secs(SEND_UNDO_SECS);
        for (outgoing, done) in deliveries {
            if self.sends_to_hold > 0 {
                self.sends_to_hold -= 1;
                self.held_sends.push(HeldSend {
                    outgoing,
                    done,
                    due,
                });
                continue;
            }
            self.deliver(outgoing, done);
        }
        let queued = std::mem::take(&mut self.server_outbox.borrow_mut().queued);
        for (update, done) in queued {
            let account = self
                .shared
                .borrow()
                .accounts
                .accounts
                .iter()
                .find(|account| account.config.email == update.account())
                .map(|account| account.config.clone());
            let Some(account) = account else {
                done(Err(format!(
                    "{} is not set up on this device",
                    update.account()
                )));
                continue;
            };
            let message = Box::new(update.locate());
            let request = match &update {
                ServerUpdate::Flags { .. } => EngineRequest::store_flag(account, message),
                ServerUpdate::Move { to, .. } => EngineRequest::move_message(account, message, *to),
                ServerUpdate::Remove { .. } => EngineRequest::purge(account, message),
                ServerUpdate::SaveDraft { .. } => {
                    let Some(draft) = update.saved_draft() else {
                        done(Err("not a draft to keep".into()));
                        continue;
                    };
                    EngineRequest::save_draft(account, Box::new(draft))
                }
            };
            let posted = self.state().post_engine(request);
            match posted {
                Some(request_id) => {
                    self.server_outbox
                        .borrow_mut()
                        .sent
                        .insert(request_id, (update, done));
                }
                None => done(Err("the mail engine is not running".into())),
            }
        }
    }

    fn deliver(&self, outgoing: Outgoing, done: DeliveryDone) {
        let account = self
            .shared
            .borrow()
            .accounts
            .accounts
            .iter()
            .find(|account| {
                account.config.email == outgoing.account
                    && outgoing.draft.sender.as_ref().is_none_or(|sender| {
                        account.id == sender.account && account.config.email == sender.email
                    })
            })
            .map(|account| account.config.clone());
        let Some(account) = account else {
            done(Err(format!(
                "{} is not set up on this device",
                outgoing.account
            )));
            return;
        };
        let mut draft = outgoing.to_draft();
        draft.wire = self.wire_ids(&outgoing.id, &account.email, draft.in_reply_to);
        let own_id = draft.wire.message_id.clone().unwrap_or_default();
        let message = Message {
            // The id the server's copy will have too, so the copy kept in
            // Sent and the one the server reports are one message.
            id: crate::transport::stable_message_id(&own_id),
            rfc_message_id: Some(own_id),
            in_reply_to: draft.wire.references.first().cloned(),
            from: Addr::new(account.display_name.clone(), account.email.clone()),
            to: crate::transport::parse_addr_list(&draft.to),
            cc: crate::transport::parse_addr_list(&draft.cc),
            subject: draft.subject.clone(),
            body: draft.body.clone(),
            html_body: draft.html_body.clone(),
            attachments: draft.attachments.iter().map(attachment_meta).collect(),
            timestamp_min: self.shared.borrow().now_min,
            read: true,
            starred: false,
            folder: Folder::Sent,
            labels: Vec::new(),
            account: account.email.clone(),
            server: None,
        };
        let posted = self
            .state()
            .post_engine(EngineRequest::send(account, Box::new(draft)));
        match posted {
            Some(request_id) => {
                self.state().pending_send.insert(
                    request_id,
                    PendingSend {
                        outgoing,
                        message,
                        done,
                    },
                );
            }
            None => done(Err("the mail engine is not running".into())),
        }
    }

    fn wire_ids(
        &self,
        outgoing: &str,
        sender: &str,
        replying_to: Option<MessageId>,
    ) -> crate::model::WireIds {
        let domain = sender
            .split_once('@')
            .map_or("localhost", |(_, domain)| domain);
        let mut wire = crate::model::WireIds {
            message_id: Some(format!("{outgoing}@{domain}")),
            ..Default::default()
        };
        let st = self.shared.borrow();
        if let Some(parent) = replying_to.and_then(|id| st.mailbox.get(id))
            && let Some(parent_id) = parent.rfc_message_id.clone().filter(|id| !id.is_empty())
        {
            if let Some(root) = parent.in_reply_to.clone().filter(|root| *root != parent_id) {
                wire.references.push(root);
            }
            wire.references.push(parent_id.clone());
            wire.in_reply_to = Some(parent_id);
        }
        wire
    }

    pub(crate) fn server_outbox(&self) -> Rc<RefCell<ServerOutbox>> {
        Rc::clone(&self.server_outbox)
    }

    pub(crate) fn follow_projection(&mut self, projection: ui::framework::Projection) {
        self.projection = Some(projection);
    }

    pub(crate) fn follow_delivery(&mut self, delivery: ui::framework::Projection) {
        self.delivery = Some(delivery);
    }

    fn drain_engine(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let events = self.state().engine_events();
        if events.is_empty() {
            return;
        }
        {
            let shared = self.shared.clone();
            let mut state = shared.borrow_mut();
            for event in events {
                self.apply_engine_event(doc, &mut state, event);
            }
            state.dirty = true;
        }
        // A page Select all was reading has landed: it joins the selection,
        // and the next one is asked for if the folder has more.
        self.follow_select_all();
        for account in std::mem::take(&mut self.pending_watch_fetches) {
            self.state()
                .post_engine(EngineRequest::fetch(account, Folder::Inbox));
        }
        let mut room = self.state();
        room.awaiting_engine = !room.pending_engine_requests.is_empty();
    }

    fn register_hydrated_messages(&self, doc: &mut ui::widget::Edits<'_>) {
        let messages: Vec<_> = self
            .shared
            .borrow()
            .mailbox
            .messages
            .values()
            .cloned()
            .collect();
        self.shared.borrow_mut().ingest_messages(doc, &messages);
    }

    /// Keep an active-account change in the person's accounts.
    fn persist_active_account_if_changed(&mut self) {
        let (active, accounts) = {
            let state = self.shared.borrow();
            (
                state
                    .accounts
                    .active_account()
                    .map(|account| account.id.clone()),
                state.accounts.clone(),
            )
        };
        if active == self.persisted_active_account {
            return;
        }
        // Tried once per choice: a store that refuses is said once, not on
        // every pass, and the next choice tries again.
        let saved = self.persist_accounts(&accounts);
        self.persisted_active_account = active;
        if let Err(error) = saved {
            self.shared.borrow_mut().status = Some(Notice::error(CATALOG.t_args(
                "mail-status-account-not-saved",
                &[("error", ArgValue::Text(error.to_string()))],
            )));
        }
    }

    pub(crate) fn follow_person_accounts(&mut self, accounts: PersonRecord<PersonAccounts>) {
        let root = self.state().accounts_persist.root();
        if let Some(root) = root {
            let device = accounts::AccountStore::load_in(&root);
            for edit in accounts.get().taking_in(&device) {
                if let Err(error) = accounts.invoke(&edit) {
                    tracing::warn!(%error, "an account of this device could not be taken in");
                }
            }
            if let Err(error) = accounts.get().project_in(&root) {
                tracing::warn!(%error, "the device's account file could not follow the person's accounts");
            }
        }
        self.person_accounts = Some((accounts, None));
        self.adopt_person_accounts();
    }

    fn persist_accounts(&self, updated: &AccountStore) -> std::io::Result<()> {
        let Some((record, _)) = &self.person_accounts else {
            return self.state().accounts_persist.persist(updated);
        };
        let (edits, removed) = updated.edits_for(&record.get());
        for edit in &edits {
            record
                .invoke(edit)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
        }
        let root = self.state().accounts_persist.root();
        if let Some(root) = root {
            // In a session the secrets are custody's: this sandbox keeps no
            // vault of its own, and a released secret stays in memory.
            if self.custody.is_none() {
                updated.keep_secrets_in(&root, &removed)?;
            }
            record.get().project_in(&root)?;
        }
        Ok(())
    }

    fn adopt_sender_pictures(&mut self) {
        let Some(directory) = self.state().contact_directory.clone() else {
            return;
        };
        let revision = directory.revision();
        let mut st = self.shared.borrow_mut();
        let own: Vec<String> = st
            .accounts
            .accounts
            .iter()
            .map(|account| account.config.email.clone())
            .collect();
        if self
            .sender_pictures_seen
            .as_ref()
            .is_some_and(|(seen, addresses)| *seen == revision && *addresses == own)
        {
            return;
        }
        let book = directory.get();
        st.avatars.follow(
            Some((&book, revision)),
            &own,
            ui::local_identity::own_photo(),
        );
        drop(st);
        self.sender_pictures_seen = Some((revision, own));
    }

    pub(crate) fn follow_preferences(
        &mut self,
        preferences: ui::framework::Setting<crate::resource::MailPreferences>,
    ) {
        let current = preferences.get();
        self.preferences = Some(preferences);
        self.adopt_preferences(current);
    }

    fn adopt_preferences(&mut self, preferences: crate::resource::MailPreferences) {
        if !self.compose.open {
            self.compose.set_format(preferences.compose_format);
        }
        for account in &mut self.shared.borrow_mut().accounts.accounts {
            account.collapsed = preferences.collapsed_accounts.contains(&account.id);
        }
        self.followed_preferences = preferences;
        self.dirty = true;
    }

    fn shown_preferences(&self) -> crate::resource::MailPreferences {
        crate::resource::MailPreferences {
            compose_format: self.compose.format(),
            collapsed_accounts: self
                .shared
                .borrow()
                .accounts
                .accounts
                .iter()
                .filter(|account| account.collapsed)
                .map(|account| account.id.clone())
                .collect(),
        }
    }

    fn sync_preferences(&mut self) {
        let Some(preferences) = &self.preferences else {
            return;
        };
        let stored = preferences.get();
        if stored != self.followed_preferences {
            self.adopt_preferences(stored);
            return;
        }
        self.store_preferences_if_changed();
    }

    fn store_preferences_if_changed(&mut self) {
        let Some(preferences) = &self.preferences else {
            return;
        };
        let shown = self.shown_preferences();
        if shown == self.followed_preferences {
            return;
        }
        let saved = preferences.set(shown.clone());
        self.followed_preferences = shown;
        if let Err(error) = saved {
            self.shared.borrow_mut().status = Some(Notice::error(CATALOG.t_args(
                "mail-status-preferences-not-saved",
                &[("error", ArgValue::Text(error.to_string()))],
            )));
        }
    }

    fn hydrate_domain_state(&self, doc: &mut ui::widget::Edits<'_>) {
        use ui::automerge::transaction::Transactable as _;
        let at = (
            doc.doc().committed_heads(),
            doc.doc().pending_ops(),
            self.shared.borrow().mailbox.search_revision(),
        );
        if self.hydrated_at.borrow().as_ref() == Some(&at) {
            return;
        }
        self.hydrate_domain_state_now(doc);
        // Applying the flags may itself change the mailbox; what it is now
        // is what the next frame compares with.
        let settled = (
            doc.doc().committed_heads(),
            doc.doc().pending_ops(),
            self.shared.borrow().mailbox.search_revision(),
        );
        *self.hydrated_at.borrow_mut() = Some(settled);
    }

    fn hydrate_domain_state_now(&self, doc: &mut ui::widget::Edits<'_>) {
        let mail = read_mail(doc.doc()).unwrap_or_default();
        {
            let mut st = self.shared.borrow_mut();
            mail.show_held(&mut st.mailbox);
            mail.apply_flags(&mut st.mailbox);
        }
        // A persisted draft becomes a local Drafts projection, but does not
        // force another collaborator's composer open.
        let draft = mail.draft.draft();
        if !draft.is_empty() {
            self.upsert_draft_message(doc, &draft, None);
        } else if let Some((id, _)) = self.projected_draft.borrow_mut().take() {
            self.shared.borrow_mut().mailbox.purge(id);
        }
        self.project_drafts(&mail);
    }
}

impl MailApp {
    pub fn nav_pane(&self) -> NavPane {
        self.nav.front_pane()
    }

    #[cfg(test)]
    fn layout_for_test(&mut self, w: f64, h: f64) {
        for _ in 0..2 {
            let theme = self.layout_theme.clone();
            let mut text = TextRenderer::new();
            text.set_text_scale(theme.text_scale);
            let mut taffy = TaffyTree::new();
            let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let child = self.build_layout(&mut taffy, &theme, &mut nodes);
            let root = taffy.parent_node(
                Style {
                    size: TSize {
                        width: Dimension::length(w as f32),
                        height: Dimension::length(h as f32),
                    },
                    ..Default::default()
                },
                &[child],
            );
            let available = TSize {
                width: taffy::AvailableSpace::Definite(w as f32),
                height: taffy::AvailableSpace::Definite(h as f32),
            };
            let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
            // SAFETY: every pointer in `nodes` was registered by this app's
            // own `build_layout` above into this tree, and the app is
            // borrowed mutably (so neither moved nor rebuilt) for the whole
            // layout.
            let laid = unsafe {
                bindings.rebuild(&*self, &taffy, child, &nodes);
                ui::widget::compute_widget_layout(
                    &mut taffy, root, available, &nodes, &bindings, &mut text,
                )
            };
            assert!(laid.is_ok(), "layout: {laid:?}");
            self.apply_layout(&taffy, child, Point::ZERO);
            self.test_layout = Some((taffy, child));
            self.nav.snap();
        }
        let theme = self.layout_theme.clone();
        let mut text = TextRenderer::new();
        text.set_text_scale(theme.text_scale);
        self.draw_child(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::empty(),
        );
    }

    #[cfg(test)]
    fn accounts_persist_root(&self) -> Option<std::path::PathBuf> {
        self.state().accounts_persist.root()
    }
}

struct ForwardedFiles {
    carried: Vec<crate::model::DraftAttachment>,
    not_downloaded: Vec<String>,
    too_large: Vec<String>,
}

fn attachment_meta(attachment: &crate::model::DraftAttachment) -> Attachment {
    let mut projected = Attachment::projected(
        attachment.filename.clone(),
        attachment.mime.clone(),
        attachment.size,
    );
    projected.content_id = Some(attachment.content_id.clone());
    projected
}

/// Reconstruct an editable [`Draft`] from a Drafts-folder message.
pub(crate) const GALLERY_SCENARIOS: &[(&str, &str)] = &[
    ("html_newsletter", "news@harvest-weekly.example"),
    ("html_github", "notifications@github.example"),
    ("html_receipt", "receipts@lumen-cafe.example"),
    ("html_dark", "digest@orbit-cal.example"),
    ("html_inline", "ana@studio-lima.example"),
    ("plain_text", "lena@fischer.example"),
];

fn open_link(url: &str) {
    if crate::reader::is_openable(url) {
        ui::widget::open_external_url(url);
    }
}

pub(crate) fn message_read(message: &Message) -> serde_json::Value {
    // Written as a recipient field takes it, so an agent replying with
    // what it read reaches the same people.
    let person = crate::model::Addr::rfc5322;
    serde_json::json!({
        "id": message.id.to_string(),
        "item": format!("mail.message:{}", message.id),
        "label": crate::actions::message_label_of(message),
        "date": mail_parser::DateTime::from_timestamp(message.timestamp_min.saturating_mul(60)).to_rfc3339(),
        "folder": message.folder.as_key(),
        "account": message.account,
        "from": person(&message.from),
        "to": message.to.iter().map(person).collect::<Vec<_>>(),
        "cc": message.cc.iter().map(person).collect::<Vec<_>>(),
        "subject": message.subject,
        "sent_minutes_since_epoch": message.timestamp_min,
        "text": message.body,
        "links": crate::html_mail::message_links(message),
        "attachments": message.shown_attachments().map(|(index, attachment)| serde_json::json!({
            "item": format!("mail.attachment:{}:{index}", message.id),
            "index": index,
            "name": attachment.filename,
            "type": attachment.mime,
            "size": attachment.size,
            "downloaded": attachment.is_downloadable(),
        })).collect::<Vec<_>>(),
    })
}

fn draft_from_message(m: &Message) -> Draft {
    let join = |addrs: &[Addr]| {
        addrs
            .iter()
            .map(super::model::Addr::rfc5322)
            .collect::<Vec<_>>()
            .join(", ")
    };
    Draft {
        sender: None,
        to: join(&m.to),
        cc: join(&m.cc),
        bcc: String::new(),
        subject: if m.subject.trim().is_empty() || m.subject == crate::i18n::no_subject() {
            String::new()
        } else {
            m.subject.clone()
        },
        body: m.body.clone(),
        html_body: None,
        calendar_ics: None,
        attachments: crate::attachment_store::bounded(
            &m.attachments
                .iter()
                .filter_map(|attachment| {
                    attachment
                        .content_id
                        .clone()
                        .map(|content_id| crate::model::DraftAttachment {
                            content_id,
                            filename: attachment.filename.clone(),
                            mime: attachment.mime.clone(),
                            size: attachment.size,
                        })
                })
                .collect::<Vec<_>>(),
        ),
        in_reply_to: None,
        wire: crate::model::WireIds::default(),
    }
}

fn report_account_sync(
    config_root: Option<&std::path::Path>,
    account_id: &str,
    health: accounts::AccountSyncHealth,
) {
    let Some(config_root) = config_root else {
        return;
    };
    if account_id.trim().is_empty() {
        return;
    }
    let reporter = accounts::AccountSyncReporter::in_root(account_id.to_string(), config_root);
    let persisted = match health {
        accounts::AccountSyncHealth::Never | accounts::AccountSyncHealth::Syncing => {
            reporter.started()
        }
        accounts::AccountSyncHealth::Healthy => reporter.succeeded(),
        accounts::AccountSyncHealth::Offline => reporter.offline(),
        accounts::AccountSyncHealth::NeedsAuthentication => reporter.needs_authentication(),
        accounts::AccountSyncHealth::Error => reporter.failed(),
    };
    if let Err(error) = persisted {
        tracing::warn!(%error, account = account_id, "mail: could not persist sync status");
    }
}

impl MailApp {
    fn handle_compose_modal(&mut self, event: &WindowEvent, ctx: &mut EventContext<'_>) -> bool {
        self.tell_compose_the_accounts();
        let format_before = self.compose.format();
        let (consumed, action) = self.compose.handle_event(
            event,
            &mut ComposeEventContext {
                scale_factor: ctx.scale_factor,
                modifiers: ctx.modifiers,
                state: ctx.state.reborrow(),
                path: ctx.path,
                node_id: ctx.node_id,
                taffy: ctx.taffy,
            },
        );
        let lifecycle_boundary = !matches!(&action, ComposeAction::None);
        match action {
            ComposeAction::RequestAttachment => self.request_compose_attachment(),
            ComposeAction::Send => self.send_compose(&mut ctx.state),
            ComposeAction::SaveDraft => self.save_compose_draft(&mut ctx.state),
            ComposeAction::Close => self.close_compose(&mut ctx.state),
            ComposeAction::Discard => self.discard_compose(&mut ctx.state),
            ComposeAction::None => {}
        }
        if lifecycle_boundary || self.compose.format() != format_before {
            self.store_preferences_if_changed();
        }
        consumed
    }

    fn confirm_bulk_purge(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.bulk_purge_confirmation = None;
        if self.apply_bulk(state, BulkAction::Purge) == 0 {
            self.shared.borrow_mut().status =
                Some(Notice::warning(CATALOG.t("mail-status-purge-nothing")));
        }
        self.process_intents();
        self.dirty = true;
    }

    fn handle_menu_event(&mut self, event: &WindowEvent, ctx: &mut EventContext<'_>) -> bool {
        if !self.menu.is_open() {
            return false;
        }
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            let accelerator = match logical_key {
                Key::Character(ch) if ch.eq_ignore_ascii_case("r") => Some(CtxAction::Reply),
                Key::Character(ch) if ch.eq_ignore_ascii_case("u") => Some(CtxAction::ToggleRead),
                Key::Character(ch) if ch.eq_ignore_ascii_case("s") => Some(CtxAction::ToggleStar),
                Key::Character(ch) if ch.eq_ignore_ascii_case("e") => Some(CtxAction::Archive),
                Key::Named(NamedKey::Delete | NamedKey::Backspace) => Some(CtxAction::Delete),
                _ => None,
            };
            if let Some(action) = accelerator
                && let Some(target) = self
                    .menu_target
                    .as_ref()
                    .filter(|target| target.items.contains(&action))
            {
                let message = target.message;
                self.close_menu();
                self.apply_menu_action(message, action, &mut ctx.state);
                *ctx.handled = true;
                return true;
            }
        }
        let mut handled = false;
        self.menu.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: ctx.scale_factor,
                modifiers: &ctx.modifiers,
                state: ctx.state.reborrow(),
                path: ctx.path,
                handled: &mut handled,
                focus_target: ctx.focus_target,
                node_id: ctx.node_id,
                taffy: ctx.taffy,
            },
        );
        self.take_menu_choice(&mut ctx.state);
        if !self.menu.is_open() {
            self.menu_target = None;
        }
        self.dirty = true;
        let consumed = handled
            || matches!(
                event,
                WindowEvent::KeyboardInput { .. } | WindowEvent::MouseInput { .. }
            );
        if consumed {
            *ctx.handled = true;
        }
        consumed
    }

    fn handle_purge_event(&mut self, event: &WindowEvent, ctx: &mut EventContext<'_>) -> bool {
        let Some(confirmation) = self.bulk_purge_confirmation.as_mut() else {
            return false;
        };
        confirmation.dialog.handle_detached(
            event,
            ctx.scale_factor,
            ctx.modifiers,
            &mut ctx.state,
            ctx.path,
        );
        self.settle_purge_confirmation(&mut ctx.state);
        *ctx.handled = true;
        self.dirty = true;
        true
    }

    fn handle_app_modal_event(&mut self, event: &WindowEvent, ctx: &mut EventContext<'_>) -> bool {
        if self.handle_purge_event(event, ctx) {
            return true;
        }
        if self.compose.open && self.handle_compose_modal(event, ctx) {
            *ctx.handled = true;
            self.dirty = true;
            return true;
        }

        if self.handle_menu_event(event, ctx) {
            return true;
        }

        if self.is_first_run() {
            if let Some(first_run_node) = self.first_run_node {
                self.first_run.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        scale_factor: ctx.scale_factor,
                        modifiers: &ctx.modifiers,
                        state: ctx.state.reborrow(),
                        path: ctx.path,
                        handled: ctx.handled,
                        focus_target: ctx.focus_target,
                        node_id: first_run_node,
                        taffy: ctx.taffy,
                    },
                );
            }
            if self.first_run.take_action() {
                self.open_account_settings();
            }
            self.dirty = true;
            return true;
        }
        false
    }

    fn thread_messages(count: usize) -> String {
        crate::i18n::t_args(
            "mail-a11y-thread-messages",
            &[(
                "count",
                ui::i18n::ArgValue::Number(f64::from(u32::try_from(count).unwrap_or(u32::MAX))),
            )],
        )
    }

    fn overview_accessibility_node(state: &MailState) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        if state.outbox_open {
            return Self::outbox_accessibility_node(state);
        }
        let rows = state.list_rows();
        let total = rows.len();
        let messages = rows
            .iter()
            .enumerate()
            .filter_map(|(index, (id, thread_len))| {
                let message = state.mailbox.get(*id)?;
                let title = Self::row_title(message);
                let snippet = message.snippet();
                let mut node = AccessNode::new(Role::ListItem)
                    .with_name(title)
                    .with_optional_value((!snippet.is_empty()).then_some(snippet))
                    .with_action_target(format!("mail.message:{id}"))
                    .with_position(index + 1, total)
                    .with_state(AccessState::new().focusable(true).selected(
                        if state.bulk_selecting {
                            state.bulk_selection.contains(id)
                        } else {
                            state.selected == Some(*id)
                        },
                    ));
                if *thread_len > 1 {
                    node = node.with_description(Self::thread_messages(*thread_len));
                }
                Some(node)
            })
            .collect::<Vec<_>>();
        let name = CATALOG.t_args(
            "mail-list-name",
            &[(
                "folder",
                ArgValue::Text(crate::i18n::t(state.folder.message())),
            )],
        );
        let mut children = vec![
            AccessNode::new(Role::Button)
                .with_name(if state.bulk_selecting {
                    crate::i18n::t("mail-a11y-bulk-done")
                } else {
                    crate::i18n::t("mail-a11y-bulk-select")
                })
                .with_action_target("mail.bulk:toggle")
                .with_state(AccessState::new().focusable(true)),
        ];
        if state.bulk_selecting {
            children.push(Self::bulk_bar_node(state));
        }
        children.extend(messages);
        // An account that cannot be checked is said above the rows, and it
        // stays said until a check works.
        if let Some(failure) = &state.sync_error {
            children.push(Self::sync_failure_node(&crate::notice::sync_failure(
                failure,
            )));
        }
        // An empty mailbox no mail has arrived in offers the one thing a
        // person can do there: its account's settings, where the account is
        // added, checked and fixed. A mailbox whose check fails offers that
        // failure's own control instead.
        if total == 0 && state.mailbox.messages.is_empty() && state.sync_error.is_none() {
            children.push(Self::account_route_node());
        }
        let mut list = AccessNode::new(Role::List)
            .with_name(name)
            .with_children(children);
        if total == 0 {
            list = list.with_description(CATALOG.t_args(
                "mail-a11y-no-messages",
                &[(
                    "folder",
                    ArgValue::Text(crate::i18n::t(state.folder.message())),
                )],
            ));
        }
        list
    }

    /// The selection's toolbar: what can be done to the messages chosen, and
    /// how many are.
    fn bulk_bar_node(state: &MailState) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};
        let selected = state.selected_bulk_ids().len();
        let enabled = selected > 0;
        let all_trash = enabled
            && state.selected_bulk_ids().iter().all(|id| {
                state
                    .mailbox
                    .get(*id)
                    .is_some_and(|message| message.folder == Folder::Trash)
            });
        AccessNode::new(Role::Toolbar)
            .with_name(CATALOG.t_args(
                "mail-selected-count",
                &[("count", ArgValue::Text(selected.to_string()))],
            ))
            .with_children(
                Self::bulk_controls(all_trash)
                    .into_iter()
                    .map(|(name, action)| {
                        AccessNode::new(Role::Button)
                            .with_name(name)
                            .with_action_target(format!("mail.bulk:{action}"))
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .disabled(!enabled && action != "select-all"),
                            )
                    })
                    .collect(),
            )
    }

    /// A failed check as the window's semantics say it: the words the list
    /// paints, and the control that helps, as its accessible control.
    fn sync_failure_node(text: &crate::notice::SyncFailureText) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut node = AccessNode::new(Role::Group).with_name(text.title.clone());
        if let Some(message) = &text.message {
            node = node.with_description(message.clone());
        }
        if let Some(offer) = &text.action {
            let route = match offer.intent {
                Intent::CheckMail => CHECK_ROUTE,
                _ => ACCOUNT_ROUTE,
            };
            node = node.with_children(vec![
                AccessNode::new(Role::Button)
                    .with_name(offer.label.clone())
                    .with_action_target(route)
                    .with_state(AccessState::new().focusable(true)),
            ]);
        }
        node
    }

    fn row_title(message: &crate::model::Message) -> String {
        if message.folder == Folder::Drafts {
            return CATALOG.t_args(
                "mail-draft-subject",
                &[("subject", ArgValue::Text(message.subject.clone()))],
            );
        }
        CATALOG.t_args(
            "mail-row-name",
            &[
                ("sender", ArgValue::Text(message.from.display())),
                ("subject", ArgValue::Text(message.subject.clone())),
            ],
        )
    }

    fn bulk_controls(all_trash: bool) -> Vec<(String, &'static str)> {
        let names = [
            ("mail-bulk-select-all", "select-all"),
            ("mail-bulk-archive", "archive"),
            ("mail-bulk-read", "read"),
            ("mail-bulk-unread", "unread"),
        ];
        let last = if all_trash {
            ("mail-bulk-purge", "purge")
        } else {
            ("mail-bulk-delete", "delete")
        };
        names
            .into_iter()
            .chain([last])
            .map(|(key, action)| (CATALOG.t(key), action))
            .collect()
    }

    fn outbox_accessibility_node(state: &MailState) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        let queued = state.queued();
        let total = queued.len();
        let rows: Vec<AccessNode> =
            queued
                .iter()
                .enumerate()
                .map(|(index, message)| {
                    AccessNode::new(Role::ListItem)
                        .with_name(CATALOG.t_args(
                            "mail-a11y-outbox-row",
                            &[
                                ("to", ArgValue::Text(message.to.clone())),
                                ("subject", ArgValue::Text(message.subject.clone())),
                            ],
                        ))
                        .with_description(message.state_line())
                        .with_action_target(format!("mail.outbox:{}", message.id))
                        .with_position(index + 1, total)
                        .with_state(AccessState::new().focusable(true).selected(
                            state.outbox_selected.as_deref() == Some(message.id.as_str()),
                        ))
                })
                .collect();
        let mut children = Vec::new();
        if let Some(message) = state.open_queued() {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(CATALOG.t_args(
                        "mail-a11y-cancel-sending",
                        &[("to", ArgValue::Text(message.to.clone()))],
                    ))
                    .with_action_target(format!("mail.outbox-cancel:{}", message.id))
                    .with_state(AccessState::new().focusable(true)),
            );
            children.push(
                AccessNode::new(Role::Group)
                    .with_name(crate::i18n::t("mail-a11y-queued-message"))
                    .with_description(match &message.reason {
                        Some(reason) => CATALOG.t_args(
                            "mail-a11y-refused",
                            &[("reason", ArgValue::Text(reason.clone()))],
                        ),
                        None => CATALOG.t("mail-not-sent-yet"),
                    })
                    .with_value(message.body.clone()),
            );
        }
        children.extend(rows);
        let mut list = AccessNode::new(Role::List)
            .with_name(crate::i18n::t("mail-a11y-outbox"))
            .with_children(children);
        if total == 0 {
            list = list.with_description(crate::i18n::t("mail-a11y-outbox-empty"));
        }
        list
    }

    fn reader_accessibility_node(state: &MailState) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        if state.bulk_selecting {
            let count = state.selected_bulk_ids().len();
            return AccessNode::new(Role::Group).with_name(format!("{count} selected"));
        }

        let thread = state.selected_thread_messages();
        let Some(current) = state.selected.and_then(|id| state.mailbox.get(id)) else {
            return AccessNode::new(Role::Group).with_name(crate::i18n::t("mail-a11y-no-message"));
        };
        let message_count = thread.len();
        // The banner above the conversation, as the reader paints it: the
        // newest message is being looked at without being marked read.
        let preview = thread
            .last()
            .filter(|head| state.inspection == Some(head.id) && !head.read)
            .map(|_| {
                AccessNode::new(Role::StaticText)
                    .with_name(crate::i18n::t("mail-a11y-preview-unread"))
                    .with_hashed_key("mail.preview")
            });
        let mut children: Vec<AccessNode> = preview.into_iter().collect();
        children.push(
            AccessNode::new(Role::Toolbar)
                .with_name(crate::i18n::t("mail-a11y-message-actions"))
                .with_children(
                    [
                        ("Reply", "reply"),
                        ("Archive", "archive"),
                        ("Delete", "delete"),
                        ("Toggle star", "star"),
                        ("Toggle read", "read"),
                    ]
                    .into_iter()
                    .map(|(name, action)| {
                        AccessNode::new(Role::Button)
                            .with_name(name)
                            .with_action_target(format!("mail.reader:{}:{action}", current.id))
                            .with_state(AccessState::new().focusable(true))
                    })
                    .collect(),
                ),
        );
        children.extend(thread.iter().map(|message| {
            AccessNode::new(Role::Paragraph)
                .with_name(CATALOG.t_args(
                    "mail-from",
                    &[("sender", ArgValue::Text(message.from.display()))],
                ))
                .with_value(message.body.clone())
        }));
        children.extend(
            current
                .attachments
                .iter()
                .enumerate()
                .map(|(index, attachment)| {
                    let downloadable = attachment.is_downloadable();
                    let mut node = AccessNode::new(Role::Button)
                        .with_name(CATALOG.t_args(
                            "mail-a11y-save-attachment",
                            &[
                                ("file", ArgValue::Text(attachment.filename.clone())),
                                ("size", ArgValue::Text(attachment.size_human())),
                            ],
                        ))
                        .with_action_target(format!(
                            "mail.reader:{}:save-attachment:{index}",
                            current.id
                        ))
                        .with_state(AccessState::new().focusable(true).disabled(!downloadable));
                    if !downloadable {
                        node = node.with_description(crate::i18n::t("mail-a11y-download-first"));
                    }
                    node
                }),
        );
        AccessNode::new(Role::Document)
            .with_name(CATALOG.t_args(
                "mail-a11y-reading",
                &[("subject", ArgValue::Text(current.subject.clone()))],
            ))
            .with_description(Self::thread_messages(message_count))
            .with_children(children)
    }
}

impl Widget for MailApp {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::undo::UndoControl
    );
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document_ref(state, path);
        self.follow_document();
        self.process_intents();
        self.background_poll_deadline.set(None);
        self.adopt_person_accounts();
        self.take_released_secrets();
        self.tell_compose_the_accounts();
        self.adopt_sender_pictures();
        self.open_kept_mail();
        {
            // The folders show the active account's mail, whichever way it
            // became active.
            let mut st = self.shared.borrow_mut();
            let shown = if st.all_inboxes {
                AccountConfig::default()
            } else {
                st.accounts.active_config()
            };
            if st.mailbox.account.email != shown.email {
                st.mailbox.set_account(shown);
            }
        }
        if self.first_run.take_action() {
            self.open_account_settings();
        }
        // The host Save dialog runs off-thread; its outcome is applied here
        // so attachment export never blocks the frame loop or claims success
        // before the chosen file is actually written.
        self.drain_compose_attachment();
        self.state().post_server_search();
        if self.compose.wants_address_book() {
            let book = {
                let st = self.shared.borrow();
                let own: Vec<String> = st
                    .accounts
                    .accounts
                    .iter()
                    .map(|account| account.config.email.clone())
                    .collect();
                crate::address_book::AddressBook::of(&st.mailbox, &own)
            };
            self.compose.set_address_book(book);
        }
        self.compose.sync(state, path);
        self.show_status();
        self.read_outbox(state);

        let mem_seeded = self.shared.borrow().seeded;
        if !mem_seeded {
            let has_accounts = !self.shared.borrow().accounts.is_empty();
            if self.state().engine.is_some()
                && has_accounts
                && !self.connect_requested
                && !self.awaits_active_secret()
            {
                let cfg = self.shared.borrow().accounts.active_config();
                {
                    let mut st = self.shared.borrow_mut();
                    let nothing_kept = st.mailbox.of_account(&cfg.email).next().is_none();
                    st.first_fetch = nothing_kept.then(|| first_fetch(&cfg));
                }
                self.state()
                    .post_engine(EngineRequest::connect(cfg.clone()));
                self.state()
                    .post_engine(EngineRequest::fetch(cfg, Folder::Inbox));
                self.state().fetch_other_inboxes();
                self.state().watch_inboxes();
                self.connect_requested = true;
            }
            self.shared.borrow_mut().seeded = true;
        }
        // Live app: advance the wall clock (relative timestamps + the periodic
        // refresh cadence read it) and fire a periodic refresh when due. The
        // sample/`--shots` path pins `now_min`, so this only runs live.
        if self.state().engine.is_some() {
            self.shared.borrow_mut().now_min = crate::state::now_minutes();
            self.state().maybe_auto_refresh();
        }
        let wants_older = self.shared.borrow().wants_older.take();
        if wants_older && self.state().engine.is_some() {
            self.load_older();
        }
        self.persist_active_account_if_changed();
        self.sync_preferences();
        self.sync_trusted_senders();
        self.declare_window_toolbar();
        self.nav.sync(state, path);
        self.state().composing = self.compose.open;
    }

    fn settle(&mut self, state: &mut ui::widget::Edits<'_>, path: &ObjId) {
        let opens = std::mem::take(&mut *self.opens.borrow_mut());
        for target in opens {
            if let Err(error) = self.state().open_target(target) {
                tracing::warn!(%error, "mail: a hit could not be opened");
            }
        }
        self.apply_window_toolbar(state);
        self.register_kept_mail(state);
        self.open_activated();
        self.process_intents();
        self.drain_engine(state);
        self.take_peer_copies(state);
        self.reveal_requested(state);
        self.state().deliver_effect_replies();
        self.take_back_sends(state);
        self.flush_server_outbox();
        self.release_held_sends(state);
        self.compose.settle(state, path);
        self.keep_compose_headers(state);
        if !self.shared.borrow().seeded {
            self.register_hydrated_messages(state);
        }
        self.hydrate_domain_state(state);
        let scenario = self.state().pending_stage.take();
        if let Some(scenario) = scenario {
            self.stage_scenario(state, &scenario);
        }
        self.nav.settle(state, path);
        self.state().composing = self.compose.open;
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The overlays laid out outside the tree (compose) measure against
        // the host theme.
        self.layout_theme = theme.clone();
        let mut children = Vec::with_capacity(2);
        if self.is_first_run() {
            self.nav_node = None;
            let first_run_node = self.first_run.build_layout(taffy, theme, node_map);
            self.first_run_node = Some(first_run_node);
            children.push(first_run_node);
        } else {
            self.first_run_node = None;
            let nav_node = self.nav.build_layout(taffy, theme, node_map);
            self.nav_node = Some(nav_node);
            children.push(nav_node);
        }
        let style = Style {
            size: TSize {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &children, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let Some(first_run_node) = self.first_run_node {
            self.first_run.apply_layout(taffy, first_run_node, inner);
            self.first_run.apply_layout_external(self.rect);
        }
        if let Some(nav_node) = self.nav_node {
            self.nav.apply_layout(taffy, nav_node, inner);
            if self.first_layout {
                self.nav.snap();
                self.first_layout = false;
                self.nav.apply_layout(taffy, nav_node, inner);
            }
            self.publish_shell_flags();
        }
        // The composer writes in the message column while it stands beside
        // the list, so a reply stands beside the message it answers; a
        // window too narrow for both, or one with no mailbox yet, gives it
        // the whole window, as the page a message is read on. It never
        // floats over the app.
        let geometry = self.nav.geometry();
        let column = geometry.content_column;
        let pane = if self.nav_node.is_some() && geometry.side_by_side && column.area() > 0.0 {
            Rect::new(
                column.x0,
                column.y0 + geometry.header_height,
                column.x1,
                column.y1,
            )
        } else {
            self.rect
        };
        self.compose.set_pane(Some(pane));
        self.compose.layout(self.rect);
        self.shortcuts.set_viewport(self.rect);
        self.menu.set_viewport(Some(self.rect));
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.is_first_run() {
            self.first_run.draw_child(scene, t, tr, theme, presence);
        } else {
            self.nav.draw_child(scene, t, tr, theme, presence);
        }
        if let Some(id) = self.pending_menu.take() {
            self.open_menu_on_row(id, tr, theme);
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.is_first_run() {
            self.first_run.draw_overlay(scene, t, tr, theme, presence);
        } else {
            self.nav.draw_overlay(scene, t, tr, theme, presence);
        }
        if let Some(confirmation) = &mut self.bulk_purge_confirmation {
            confirmation
                .dialog
                .draw_overlay(scene, t, tr, theme, presence);
        }
        self.compose.draw(scene, t, tr, theme, presence);
        self.compose.draw_inputs(scene, t, tr, theme, presence);
        if self.menu.is_open() {
            self.menu.draw_overlay(scene, t, tr, theme, presence);
        }
        self.shortcuts.draw(scene, t, tr, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let moved = matches!(event, WindowEvent::CursorMoved { .. });
        if let WindowEvent::CursorMoved { position, .. } = event {
            let point = ctx.logical(position);
            self.shared.borrow_mut().last_mouse = point;
        }

        // The shortcut sheet is a genuine modal first-responder.  It must
        // consume all input before any Mail command can observe it.
        if self.shortcuts.handle_event(event, ctx.scale_factor) {
            *ctx.handled = true;
            self.dirty = true;
            return;
        }

        let modal_consumed = {
            let mut context = EventContext {
                scale_factor: ctx.scale_factor,
                modifiers: *ctx.modifiers,
                state: ctx.state.reborrow(),
                path: ctx.path,
                handled: ctx.handled,
                focus_target: ctx.focus_target,
                node_id: ctx.node_id,
                taffy: ctx.taffy,
            };
            self.handle_app_modal_event(event, &mut context)
        };
        if modal_consumed {
            return;
        }

        if let Some(nav_node) = self.nav_node {
            let path = ctx.path;
            self.nav
                .handle_event(event, &mut ctx.reborrow(path, nav_node));
        }
        self.apply_commands(&mut ctx.state);
        self.process_intents();

        if !*ctx.handled {
            self.global_shortcuts(event, *ctx.modifiers, &mut ctx.state, ctx.handled);
        }

        // A pointer moving over the window repaints only what its parts say
        // changed under it (a row's hover, a button's); anything else the
        // person does repaints the window.
        if !moved {
            self.dirty = true;
        }
        self.state().composing = self.compose.open;
    }

    fn name(&self) -> &'static str {
        "MailApp"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = Vec::with_capacity(3);
        if self.is_first_run() {
            children.push(&self.first_run);
        } else {
            children.push(&self.nav);
        }
        if self.menu.is_open() {
            children.push(&self.menu);
        }
        if let Some(confirmation) = &self.bulk_purge_confirmation {
            children.push(&confirmation.dialog);
        }
        children
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = false;
        for child in self.children_mut() {
            dirty |= child.take_structure_dirty();
        }
        dirty
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut (dyn Widget + '_)> = Vec::with_capacity(3);
        if self.is_first_run() {
            children.push(&mut self.first_run);
        } else {
            children.push(&mut self.nav);
        }
        if self.menu.is_open() {
            children.push(&mut self.menu);
        }
        if let Some(confirmation) = &mut self.bulk_purge_confirmation {
            children.push(&mut confirmation.dialog);
        }
        children
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.compose.is_modal()
            || (self.compose.open && self.compose.window_rect().contains(point))
        {
            return self.compose.cursor_shape(point);
        }
        if self.is_first_run() {
            self.first_run.cursor_shape(point)
        } else {
            self.nav.cursor_shape(point)
        }
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};

        if self.shortcuts.visible() {
            return Some(self.shortcuts.accessibility());
        }
        if let Some(confirmation) = &self.bulk_purge_confirmation {
            return confirmation.dialog.accessibility();
        }
        if self.compose.is_modal() || (self.compose.open && self.is_first_run()) {
            return Some(self.compose.accessibility_node());
        }
        if self.menu.is_open() {
            return self.menu.accessibility();
        }
        if self.is_first_run() {
            return Some(self.empty_window_accessibility());
        }

        let st = self.shared.borrow();
        let compact = st.narrow;
        // A drawer covers the columns, so they leave the traversal while it
        // is open; the sidebar list publishes itself.
        let drawer_open =
            self.nav.geometry().sidebar == SidebarPlacement::Drawer && self.nav.is_sidebar_open();
        let overview_visible =
            !drawer_open && (!compact || self.nav.front_pane() == NavPane::Overview);
        let reader_visible =
            !drawer_open && (!compact || self.nav.front_pane() == NavPane::Content);
        let mut children = Vec::new();

        if self.state().awaiting_engine {
            children.push(
                AccessNode::new(Role::Spinner)
                    .with_name(crate::i18n::t("mail-a11y-syncing"))
                    .with_state(AccessState::new().busy(true)),
            );
        }
        if let Some(status) = self.nav.toast_text() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(status.to_owned())
                    .with_live(Politeness::Polite),
            );
        }

        if overview_visible {
            children.push(Self::overview_accessibility_node(&st));
        }

        if reader_visible {
            children.push(if self.compose.open {
                self.compose.accessibility_node()
            } else {
                Self::reader_accessibility_node(&st)
            });
        } else if self.compose.open {
            children.push(self.compose.accessibility_node());
        }

        let composition = if drawer_open {
            CATALOG.t("mail-view-drawer")
        } else if compact {
            if self.nav.front_pane() == NavPane::Content {
                CATALOG.t("mail-view-reading")
            } else {
                CATALOG.t("mail-view-list")
            }
        } else {
            CATALOG.t("mail-view-all")
        };
        Some(
            AccessNode::new(Role::Window)
                .with_name(crate::i18n::t("mail-title"))
                .with_description(CATALOG.t_args(
                    "mail-a11y-current-view",
                    &[("view", ArgValue::Text(composition))],
                ))
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        let (state, _) = ui::doc::document(state, path);
        if let Some(control) = target.strip_prefix("mail.compose:") {
            return self.perform_compose_semantic_action(control, action, state);
        }
        if let Some(raw) = target.strip_prefix("mail.reader:") {
            return self.perform_reader_semantic_action(raw, action, state);
        }
        if let Some(raw) = target.strip_prefix("mail.account:") {
            return self.perform_account_semantic_action(raw, action, state);
        }

        if let Some(raw) = target.strip_prefix("mail.folder:") {
            return self.perform_folder_semantic_action(raw, action, state);
        }
        if let Some(raw) = target.strip_prefix("mail.bulk:") {
            return self.perform_bulk_semantic_action(raw, action, state);
        }
        if let Some(raw) = target.strip_prefix("mail.outbox-cancel:") {
            let mut mail = self.shared.borrow_mut();
            mail.cancel_send(state, raw);
            drop(mail);
            self.dirty = true;
            return true;
        }
        if let Some(raw) = target.strip_prefix("mail.outbox:") {
            self.shared.borrow_mut().select_queued(raw);
            self.dirty = true;
            return true;
        }
        if target == ACCOUNT_ROUTE {
            self.open_account_settings();
            return true;
        }
        if target == CHECK_ROUTE {
            let _ = self.state().refresh_active_folder();
            return true;
        }

        let Some(raw) = target.strip_prefix("mail.message:") else {
            return false;
        };
        self.perform_message_semantic_action(raw, action, state)
    }

    fn modal_capture(&self) -> Option<Rect> {
        if self.shortcuts.visible() {
            self.shortcuts.card_rect()
        } else if let Some(confirmation) = &self.bulk_purge_confirmation {
            Some(confirmation.dialog.dialog_rect()).filter(|rect| rect.area() > 0.0)
        } else if self.compose.is_modal() {
            Some(self.compose.window_rect())
        } else if self.is_first_run() {
            Some(self.rect)
        } else {
            None
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.room.borrow().dirty
            || self.shared.borrow().dirty
            || self.nav.needs_redraw()
            || self.compose.needs_redraw()
            || self.shortcuts.needs_redraw()
            || ui::widget::Widget::needs_animation_frame(self)
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        // Worker and native-picker completions are not animation. Keep their
        // deadline fixed until sync consumes a tick, even with reduced motion.
        // Workers that wake the window themselves are not polled at all.
        let workers_wake = self.workers_wake();
        let pending = (!workers_wake && self.state().awaiting_engine)
            || self.compose_attachment_rx.is_some()
            || self.compose_attachment_contents.is_some();
        let background = pending.then(|| {
            let deadline = self
                .background_poll_deadline
                .get()
                .unwrap_or_else(|| ui::time::now() + std::time::Duration::from_millis(16));
            self.background_poll_deadline.set(Some(deadline));
            deadline
        });
        let (refresh, server_search) = {
            let room = self.state();
            (
                room.next_refresh_at(),
                room.server_search.as_ref().map(|(_, at)| *at),
            )
        };
        background
            .into_iter()
            .chain(self.nav.next_wake_deadline())
            .chain(refresh)
            .chain(self.compose.headers_due_at())
            .chain(server_search)
            .chain(self.held_sends.iter().map(|held| held.due))
            .min()
    }

    fn needs_animation_frame(&self) -> bool {
        // A worker that cannot wake the window (outside a live window) is
        // drained by pumping frames while its request is in flight; one that
        // can is waited for without drawing anything.
        (self.state().awaiting_engine && !self.workers_wake()) || self.nav.needs_animation_frame()
    }

    fn needs_idle_sync(&self) -> bool {
        // Keep syncing while a connect/fetch reply is outstanding or a status
        // waits for `sync` to show it, then idle rather than busy-loop once
        // results have arrived.
        // `wants_older` is what the list last painted wanting (`MessageList`:
        // the end of the rows is on screen), a standing condition rather than
        // news: on its own it would ask for a pass on every frame, and a pass
        // paints the end again. It asks for one only while a page is really
        // left to ask a server for, which is what `load_older` then does.
        let older_left =
            self.shared.borrow().wants_older.get() && !self.older_pages_wanted().is_empty();
        (self.state().awaiting_engine && !self.workers_wake())
            || self.shared.borrow().status.is_some()
            || older_left
            || self.nav.needs_idle_sync()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.state().dirty = false;
        self.shared.borrow_mut().dirty = false;
        self.nav.mark_drawn();
        self.compose.mark_drawn();
        self.shortcuts.mark_drawn();
    }
}

fn recipient_line(draft: &crate::actions::DraftData) -> String {
    let mut to = Vec::new();
    for field in [&draft.to, &draft.cc, &draft.bcc] {
        for address in crate::transport::parse_addr_list(field) {
            to.push(address.display());
        }
    }
    if to.is_empty() {
        "nobody yet".to_owned()
    } else {
        to.join(", ")
    }
}

impl ui::undo::UndoControl for MailApp {
    fn take_undo_request(&mut self) -> Option<ui::undo::UndoRequest> {
        self.nav.take_toast_action()?;
        if self.send_notice && (!self.held_sends.is_empty() || self.sends_to_hold > 0) {
            self.unsend = true;
            self.dirty = true;
            return None;
        }
        Some(ui::undo::UndoRequest::Undo)
    }
}

impl ui::widget::controls::ActionControls for MailApp {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        let searched = self.state().search.take();
        searched.or_else(|| {
            self.children_mut()
                .into_iter()
                .find_map(ui::widget::controls::take_control_invocation)
        })
    }
}

#[derive(Debug)]
enum Reveal {
    Message(MessageId),
    Draft,
    DraftId(String),
    Invitation(Draft),
    Contact(Draft),
}

impl Mailroom {
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered match over every effect a mail window publishes, kept together so a new effect cannot be left unhandled"
    )]
    pub(crate) fn begin_effect(
        &mut self,
        invocation: tilo_actions::InvocationId,
        effect: crate::actions::MailEffect,
    ) -> Result<ui::framework::Effected, String> {
        use crate::actions::MailEffect;
        let done = ui::framework::Effected::done();
        match effect {
            MailEffect::ContactRecipients => {
                self.open_contact_recipients();
                Ok(done)
            }
            MailEffect::Compose => {
                self.shared.borrow_mut().push_intent(Intent::Compose);
                Ok(done)
            }
            MailEffect::AccountSettings => {
                self.shared
                    .borrow_mut()
                    .push_command(Command::AccountSettings);
                Ok(done)
            }
            MailEffect::PrepareContactDraft {
                person,
                label,
                email,
                sender_account,
                sender_email,
            } => {
                let draft = self.prepare_contact_draft(
                    &person,
                    &label,
                    &email,
                    &sender_account,
                    &sender_email,
                )?;
                self.reveals.push(Reveal::Contact(draft));
                self.has_contact_draft_conflict = true;
                Ok(ui::framework::Effected::Done(tilo_actions::ActionOutput {
                    value: Some(
                        serde_json::json!({ "person": person, "recipient": email, "sender_account": sender_account, "sender_email": sender_email }),
                    ),
                    resources: vec!["mail.draft".into()],
                }))
            }
            MailEffect::Refresh => {
                if self.engine.is_none() {
                    return Err("no connected mail account is available".into());
                }
                if self.resource_refresh_invocation.is_some() || self.awaiting_engine {
                    return Err("a mail operation is already running".into());
                }
                let request_id = self
                    .refresh_active_folder()
                    .ok_or_else(|| "mail refresh could not be queued".to_string())?;
                self.resource_refresh_invocation = Some((request_id, invocation));
                Ok(ui::framework::Effected::Pending)
            }
            MailEffect::PrepareCalendarInvitation {
                to,
                subject,
                body,
                calendar_ics,
            } => {
                let draft = prepare_calendar_invitation_draft(&to, &subject, &body, &calendar_ics)?;
                self.reveals.push(Reveal::Invitation(draft));
                self.dirty = true;
                Ok(ui::framework::Effected::Done(tilo_actions::ActionOutput {
                    value: None,
                    resources: vec!["mail.draft".into()],
                }))
            }
            MailEffect::Open { target } => self.open_target(target).map(|()| done),
            MailEffect::Inspect { message } => {
                let id = message
                    .parse::<MessageId>()
                    .map_err(|_| "invalid message id")?;
                if !self.shared.borrow_mut().preview_message(id) {
                    return Err("message is no longer loaded".into());
                }
                Ok(done)
            }
            MailEffect::Reply { message } => {
                self.compose_from(&message, Intent::Reply).map(|()| done)
            }
            MailEffect::ReplyAll { message } => {
                self.compose_from(&message, Intent::ReplyAll).map(|()| done)
            }
            MailEffect::Forward { message } => {
                self.compose_from(&message, Intent::Forward).map(|()| done)
            }
            MailEffect::Save { message, index } => {
                self.attachment_to_save(&message, index)
                    .map_err(|cause| cause.message())?;
                Err("saving an attachment asks the host where to put it".into())
            }
            MailEffect::SearchMessages { .. }
            | MailEffect::ExportContent { .. }
            | MailEffect::ReadMessage { .. }
            | MailEffect::DraftReply { .. }
            | MailEffect::Content { .. }
            | MailEffect::OpenAttachment { .. } => {
                Err("messages are answered from what this device keeps and the mailbox".into())
            }
            effect @ (MailEffect::LoadRemoteContent { .. }
            | MailEffect::AlwaysLoadRemoteContent { .. }
            | MailEffect::OpenLink { .. }) => self.reader_effect(effect).map(|()| done),
        }
    }

    fn reader_effect(&mut self, effect: crate::actions::MailEffect) -> Result<(), String> {
        use crate::actions::MailEffect;
        match effect {
            MailEffect::LoadRemoteContent { message } => {
                let id = self.html_message(&message)?;
                self.shared.borrow_mut().load_remote(id);
                self.dirty = true;
            }
            MailEffect::AlwaysLoadRemoteContent { message } => {
                let id = self.html_message(&message)?;
                self.shared.borrow_mut().always_load_remote(id);
                self.dirty = true;
            }
            MailEffect::OpenLink { message, url } => {
                let id = message
                    .parse::<MessageId>()
                    .map_err(|_| "invalid message id")?;
                let links = self
                    .shared
                    .borrow()
                    .mailbox
                    .get(id)
                    .map(crate::html_mail::message_links)
                    .ok_or("message is no longer loaded")?;
                if !links.contains(&url) {
                    return Err("the message has no such link".into());
                }
                open_link(&url);
            }
            _ => return Err("not an effect of the reader".into()),
        }
        Ok(())
    }

    fn html_message(&self, message: &str) -> Result<MessageId, String> {
        let id = message
            .parse::<MessageId>()
            .map_err(|_| "invalid message id")?;
        match self.shared.borrow().mailbox.get(id) {
            Some(found) if found.html_body.is_some() => Ok(id),
            Some(_) => Err("the message has no remote content".into()),
            None => Err("message is no longer loaded".into()),
        }
    }

    #[cfg(test)]
    pub(crate) fn start_effect(
        &mut self,
        invocation: tilo_actions::InvocationId,
        payload: &[u8],
    ) -> Result<ui::framework::Effected, String> {
        let effect =
            <crate::actions::MailEffect as ui::tilo_resource::Effect>::from_payload(payload)
                .map_err(|error| error.to_string())?;
        self.begin_effect(invocation, effect)
    }

    fn open_target(&mut self, target: crate::actions::MailTarget) -> Result<(), String> {
        use crate::actions::MailTarget;
        match target {
            MailTarget::Folder(folder) => {
                let folder = Folder::from_key(&folder).ok_or("folder no longer exists")?;
                self.shared.borrow_mut().reveal_folder(folder);
            }
            MailTarget::Account(id) => {
                let index = self
                    .shared
                    .borrow()
                    .accounts
                    .accounts
                    .iter()
                    .position(|account| account.id == id)
                    .ok_or("account no longer exists")?;
                let mut state = self.shared.borrow_mut();
                state.accounts.set_active(index);
                state.reveal_folder(Folder::Inbox);
                drop(state);
            }
            MailTarget::Message(id) => {
                let id = id.parse::<MessageId>().map_err(|_| "invalid message id")?;
                if self.shared.borrow().mailbox.get(id).is_none() {
                    return Err("message is no longer loaded".into());
                }
                self.reveals.push(Reveal::Message(id));
            }
            MailTarget::Draft => self.reveals.push(Reveal::Draft),
            MailTarget::DraftId(id) => self.reveals.push(Reveal::DraftId(id)),
        }
        self.dirty = true;
        Ok(())
    }

    fn compose_from(&self, message: &str, intent: fn(MessageId) -> Intent) -> Result<(), String> {
        let id = message
            .parse::<MessageId>()
            .map_err(|_| "invalid message id")?;
        if self.shared.borrow().mailbox.get(id).is_none() {
            return Err("message is no longer loaded".into());
        }
        self.shared.borrow_mut().push_intent(intent(id));
        Ok(())
    }

    pub(crate) fn take_effect_result(
        &mut self,
        invocation: tilo_actions::InvocationId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if !self
            .resource_refresh_result
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            return None;
        }
        self.resource_refresh_invocation = None;
        self.resource_refresh_result.take()
    }

    pub(crate) fn await_effect(
        &mut self,
        invocation: tilo_actions::InvocationId,
        reply: ui::framework::EffectReply,
    ) {
        self.effect_replies.push((invocation, reply));
    }

    fn deliver_effect_replies(&mut self) {
        for (invocation, reply) in std::mem::take(&mut self.effect_replies) {
            match self.take_effect_result(invocation) {
                Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => {
                    reply.answer(Ok(output));
                }
                Some(tilo_actions::InvocationUpdate::Failed { error, .. }) => {
                    reply.answer(Err(error));
                }
                Some(_) => reply.answer(Ok(tilo_actions::ActionOutput::default())),
                None => self.effect_replies.push((invocation, reply)),
            }
        }
    }

    pub(crate) fn can_sync(&self) -> bool {
        self.engine.is_some()
            && !self
                .shared
                .borrow()
                .accounts
                .active_config()
                .email
                .is_empty()
    }

    pub(crate) fn attachment_to_save(
        &self,
        message: &str,
        index: usize,
    ) -> Result<(String, Vec<u8>), AttachmentUnavailable> {
        let message_id = message
            .parse::<MessageId>()
            .map_err(|_| AttachmentUnavailable::UnknownMessage)?;
        let attachment = self
            .shared
            .borrow()
            .mailbox
            .get(message_id)
            .and_then(|message| message.attachments.get(index))
            .cloned()
            .ok_or(AttachmentUnavailable::NotLoaded)?;
        if !attachment.is_downloadable() {
            return Err(AttachmentUnavailable::NotDownloaded);
        }
        let mut attachment = attachment;
        if attachment.content.is_empty() && attachment.kept {
            let account = self
                .shared
                .borrow()
                .mailbox
                .get(message_id)
                .map(|message| message.account.clone())
                .unwrap_or_default();
            attachment.content = self
                .store
                .attachment(&account, message_id, index)
                .ok_or_else(|| AttachmentUnavailable::NotKept(attachment.filename.clone()))?;
        }
        if attachment.content.len() > MAX_ATTACHMENT_SAVE_BYTES {
            return Err(AttachmentUnavailable::TooLarge(attachment.filename));
        }
        let content_len = u64::try_from(attachment.content.len())
            .map_err(|_| AttachmentUnavailable::TooLarge(attachment.filename.clone()))?;
        if attachment.size != content_len {
            return Err(AttachmentUnavailable::Changed);
        }
        Ok((
            MailApp::attachment_save_name(&attachment.filename),
            attachment.content.clone(),
        ))
    }

    pub(crate) fn finish_attachment_save(
        &mut self,
        saved: bool,
        name: &str,
        resource: String,
    ) -> ui::framework::Effected {
        self.shared.borrow_mut().status = Some(if saved {
            Notice::success(CATALOG.t_args(
                "mail-status-copy-saved",
                &[("file", ArgValue::Text(name.to_owned()))],
            ))
        } else {
            Notice::info(CATALOG.t("mail-status-copy-cancelled"))
        });
        self.dirty = true;
        ui::framework::Effected::Done(tilo_actions::ActionOutput {
            value: None,
            resources: if saved { vec![resource] } else { Vec::new() },
        })
    }

    pub(crate) fn search_servers(&mut self, text: &str) {
        let text = text.trim();
        if self.engine.is_none()
            || text.chars().count() < SERVER_SEARCH_MIN_CHARS
            || text == self.server_searched
        {
            self.server_search = None;
            return;
        }
        self.server_search = Some((
            text.to_owned(),
            ui::time::now() + std::time::Duration::from_millis(SERVER_SEARCH_DEBOUNCE_MS),
        ));
        self.dirty = true;
    }

    pub(crate) fn searching_servers(&self) -> bool {
        self.server_search.is_some()
            || self
                .pending_engine_requests
                .values()
                .any(|pending| pending.kind == EngineRequestKind::Search)
    }

    fn post_server_search(&mut self) {
        let due = self
            .server_search
            .as_ref()
            .is_some_and(|(_, at)| ui::time::now() >= *at);
        if !due {
            return;
        }
        let Some((text, _)) = self.server_search.take() else {
            return;
        };
        let accounts: Vec<AccountConfig> = self
            .shared
            .borrow()
            .accounts
            .accounts
            .iter()
            .map(|account| account.config.clone())
            .filter(|config| !config.email.is_empty())
            .collect();
        for account in accounts {
            self.post_engine(EngineRequest::search(account, text.clone()));
        }
        self.server_searched = text;
    }
}

impl MailApp {
    pub(crate) fn opener(
        &self,
    ) -> impl FnMut(&str) -> Result<(), ui::tilo_resource::Refusal> + 'static {
        use crate::actions::MailTarget;
        let opens = std::rc::Rc::clone(&self.opens);
        let shared = std::rc::Rc::clone(&self.shared);
        let live = self.live.clone();
        move |anchor| {
            let draft = anchor
                .strip_prefix("mail.draft:")
                .or_else(|| anchor.rsplit_once("/draft:").map(|(_, id)| id));
            if let Some(draft) = draft {
                let id = crate::resource::draft_named(draft);
                let kept = live
                    .as_ref()
                    .is_some_and(|live| live.get().draft_at(Some(id)).is_some());
                let target = if crate::resource::is_composer_draft(id) {
                    MailTarget::Draft
                } else if kept {
                    MailTarget::DraftId(id.to_owned())
                } else {
                    return Err(ui::tilo_resource::Refusal::new(
                        "there is no such draft: draft-reply answers the drafts it makes",
                    ));
                };
                opens.borrow_mut().push(target);
                return Ok(());
            }
            let message = anchor
                .strip_prefix("mail.message:")
                .or_else(|| anchor.rsplit_once("/message:").map(|(_, id)| id));
            let target = if let Some(id) = message {
                let loaded = id
                    .parse::<MessageId>()
                    .is_ok_and(|id| shared.borrow().mailbox.get(id).is_some());
                if !loaded {
                    return Err(ui::tilo_resource::Refusal::new(
                        "the message is not loaded here",
                    ));
                }
                MailTarget::Message(id.to_owned())
            } else {
                let key = Folder::ALL
                    .into_iter()
                    .find(|folder| crate::resource::folder_resource(folder.as_key()) == anchor)
                    .ok_or_else(|| ui::tilo_resource::Refusal::new("no such folder or message"))?;
                MailTarget::Folder(key.as_key().to_owned())
            };
            opens.borrow_mut().push(target);
            Ok(())
        }
    }

    fn reveal_requested(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let reveals = std::mem::take(&mut self.state().reveals);
        for reveal in reveals {
            match reveal {
                Reveal::Message(id) => {
                    if self.shared.borrow_mut().open_message(doc, id) {
                        self.process_intents();
                    }
                }
                Reveal::Draft => {
                    let draft = read_mail(doc.doc()).unwrap_or_default().draft.draft();
                    self.compose.open_with_draft(&draft);
                    self.editing_draft = None;
                }
                Reveal::DraftId(id) => {
                    let mail = read_mail(doc.doc()).unwrap_or_default();
                    if let Some(draft) = mail.draft_at(Some(&id)) {
                        let named = crate::resource::draft_named(&id);
                        let named = (!crate::resource::is_composer_draft(named)).then_some(named);
                        self.compose.open_draft(named, &draft.draft());
                        self.editing_draft = None;
                    } else {
                        self.shared.borrow_mut().status =
                            Some(Notice::warning(CATALOG.t("mail-status-draft-gone")));
                    }
                }
                Reveal::Contact(draft) => {
                    self.shared.borrow_mut().save_draft(doc, &draft);
                    self.compose.open_with_draft(&draft);
                    self.editing_draft = None;
                    self.shared.borrow_mut().status = Some(Notice::success(crate::i18n::t(
                        "mail-contact-draft-prepared",
                    )));
                }
                Reveal::Invitation(draft) => {
                    self.shared.borrow_mut().save_draft(doc, &draft);
                    self.compose.open_with_draft(&draft);
                    self.editing_draft = None;
                    self.shared.borrow_mut().status =
                        Some(Notice::success(CATALOG.t("mail-status-invitation-drafted")));
                }
            }
            self.dirty = true;
        }
    }

    fn open_attachment(&self, message: MessageId, index: usize) {
        if let Err(cause) = self.state().attachment_to_save(&message.to_string(), index) {
            self.shared.borrow_mut().status = Some(Notice::warning(cause.message()));
            return;
        }
        let dispatcher = self.shared.borrow().dispatcher.clone();
        if let Some(dispatcher) = dispatcher {
            dispatcher.effect_on(
                &format!("mail.attachment:{message}:{index}"),
                &crate::actions::MailEffect::OpenAttachment {
                    message: message.to_string(),
                    index,
                    group: None,
                },
            );
        }
    }

    fn request_attachment_save(&self, message: MessageId, index: usize) {
        if let Err(cause) = self.state().attachment_to_save(&message.to_string(), index) {
            self.shared.borrow_mut().status = Some(Notice::warning(cause.message()));
            return;
        }
        let dispatcher = self.shared.borrow().dispatcher.clone();
        if let Some(dispatcher) = dispatcher {
            dispatcher.effect_on(
                &format!("mail.attachment:{message}:{index}"),
                &crate::actions::MailEffect::Save {
                    message: message.to_string(),
                    index,
                },
            );
        }
    }
}

impl Mailroom {
    pub(crate) fn live_rows(&self) -> Vec<ui::framework::LiveRow> {
        let mut rows = crate::resource::live_extras(self);
        rows.push(crate::actions::accounts_collection(&self.accounts_rows()));
        rows.extend(self.contact_recipient_rows());
        rows
    }

    /// The row `id` this window has loaded that the mailbox does not hold:
    pub(crate) fn live_row(&self, id: &str) -> Option<ui::framework::LiveRow> {
        if id.starts_with("mail.contact") {
            return self.contact_recipient_row(id);
        }
        if id == "mail.accounts" {
            return Some(crate::actions::accounts_collection(&self.accounts_rows()));
        }
        if let Some(account) = id.strip_prefix("mail.account:") {
            return self
                .accounts_rows()
                .into_iter()
                .find(|(each, _, _)| each == account)
                .map(|(id, label, active)| crate::actions::account_row(&id, &label, active));
        }
        if let Some(message_id) = id.strip_prefix("mail.message:") {
            return self
                .resource_message_row(message_id)
                .map(|row| crate::actions::message_row_of(&row));
        }
        if let Some(rest) = id.strip_prefix("mail.attachment:")
            && let Some((message_id, index)) = rest.rsplit_once(':')
            && let Ok(index) = index.parse::<usize>()
        {
            let row = self.resource_message_row(message_id)?;
            return row
                .attachments
                .get(index)
                .map(|attachment| crate::actions::attachment_row(&row.id, index, attachment));
        }
        if let Some(stable) = id.strip_prefix("mail.thread:") {
            let (subject, messages) = self.resource_thread_rows(stable)?;
            let mut thread = ui::framework::LiveRow::new(
                id,
                "mail.thread",
                if subject.is_empty() {
                    crate::i18n::no_subject()
                } else {
                    subject
                },
            );
            for message in &messages {
                thread = thread.row(crate::actions::message_row_of(message));
            }
            return Some(thread);
        }
        crate::resource::live_extras(self)
            .into_iter()
            .find(|extra| extra.id() == id)
    }

    fn accounts_rows(&self) -> Vec<(String, String, bool)> {
        let state = self.shared.borrow();
        state
            .accounts
            .accounts
            .iter()
            .enumerate()
            .map(|(index, account)| {
                (
                    account.id.clone(),
                    account.config.label().to_string(),
                    state.accounts.active == index,
                )
            })
            .collect()
    }
}

fn prepare_calendar_invitation_draft(
    to: &str,
    subject: &str,
    body: &str,
    calendar_ics: &str,
) -> Result<Draft, String> {
    let to = to.trim();
    if to.len() > 8 * 1024 || !to.contains('@') || to.chars().any(char::is_control) {
        return Err("invitation recipient is invalid".into());
    }
    let subject = subject.trim();
    if subject.is_empty() || subject.len() > 998 || subject.chars().any(char::is_control) {
        return Err("invitation subject is invalid".into());
    }
    if body.len() > 256 * 1024 || body.contains('\0') {
        return Err("invitation message is too large or invalid".into());
    }
    if !crate::actions::valid_calendar_invitation(calendar_ics) {
        return Err("calendar invitation is not a bounded METHOD:REQUEST VCALENDAR".into());
    }
    Ok(Draft {
        sender: None,
        to: to.into(),
        cc: String::new(),
        bcc: String::new(),
        subject: subject.into(),
        body: body.to_string(),
        html_body: None,
        calendar_ics: Some(calendar_ics.to_string()),
        attachments: Vec::new(),
        in_reply_to: None,
        wire: crate::model::WireIds::default(),
    })
}

impl MailApp {
    fn perform_compose_semantic_action(
        &mut self,
        control: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
    ) -> bool {
        if !self.compose.open {
            return false;
        }
        self.tell_compose_the_accounts();
        let format_before = self.compose.format();
        let compose_action = self.compose.perform_semantic_action(control, action);
        let lifecycle_boundary = !matches!(&compose_action, Some(ComposeAction::None));
        match compose_action {
            Some(ComposeAction::RequestAttachment) => self.request_compose_attachment(),
            Some(ComposeAction::Send) => self.send_compose(state),
            Some(ComposeAction::SaveDraft) => self.save_compose_draft(state),
            Some(ComposeAction::Close) => self.close_compose(state),
            Some(ComposeAction::Discard) => self.discard_compose(state),
            Some(ComposeAction::None) => {}
            None => return false,
        }
        if lifecycle_boundary || self.compose.format() != format_before {
            self.store_preferences_if_changed();
        }
        true
    }

    fn perform_reader_semantic_action(
        &mut self,
        raw: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
    ) -> bool {
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let Some((message_raw, control)) = raw.split_once(':') else {
            return false;
        };
        let Ok(message) = message_raw.parse::<MessageId>() else {
            return false;
        };
        if self.shared.borrow().selected != Some(message) {
            return false;
        }
        if let Some(index) = control
            .strip_prefix("save-attachment:")
            .and_then(|index| index.parse::<usize>().ok())
        {
            let downloadable = self
                .shared
                .borrow()
                .mailbox
                .get(message)
                .and_then(|mail| mail.attachments.get(index))
                .is_some_and(Attachment::is_downloadable);
            if !downloadable {
                return false;
            }
            self.request_attachment_save(message, index);
            return true;
        }
        let action = match control {
            "reply" => CtxAction::Reply,
            "archive" => CtxAction::Archive,
            "delete" => CtxAction::Delete,
            "star" => CtxAction::ToggleStar,
            "read" => CtxAction::ToggleRead,
            _ => return false,
        };
        self.shared
            .borrow_mut()
            .apply_message_action(state, message, action);
        self.process_intents();
        true
    }

    fn perform_account_semantic_action(
        &mut self,
        raw: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
    ) -> bool {
        use ui::a11y::Action;

        let Ok(account_index) = raw.parse::<usize>() else {
            return false;
        };
        let mut mail = self.shared.borrow_mut();
        let Some(account) = mail.accounts.accounts.get(account_index) else {
            return false;
        };
        let collapsed = account.collapsed;
        match action {
            Action::Activate => {
                mail.set_active_account(account_index);
                mail.select_folder(state, Folder::Inbox);
                mail.push_intent(Intent::CloseDrawer);
            }
            Action::Expand if collapsed => {
                mail.toggle_account_collapsed(account_index);
            }
            Action::Collapse if !collapsed => {
                mail.toggle_account_collapsed(account_index);
            }
            _ => return false,
        }
        drop(mail);
        self.process_intents();
        true
    }

    fn perform_folder_semantic_action(
        &mut self,
        raw: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
    ) -> bool {
        let Some((account_raw, folder_raw)) = raw.split_once(':') else {
            return false;
        };
        let (Ok(account_index), Some(folder)) =
            (account_raw.parse::<usize>(), Folder::from_key(folder_raw))
        else {
            return false;
        };
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let mut mail = self.shared.borrow_mut();
        let Some(account) = mail.accounts.accounts.get(account_index) else {
            return false;
        };
        if account.collapsed || !AccountStore::folders_for(account).contains(&folder) {
            return false;
        }
        mail.set_active_account(account_index);
        mail.select_folder(state, folder);
        mail.push_intent(Intent::CloseDrawer);
        drop(mail);
        self.process_intents();
        true
    }

    fn perform_message_semantic_action(
        &mut self,
        raw: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
    ) -> bool {
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let Ok(id) = raw.parse::<MessageId>() else {
            return false;
        };
        let mut mail = self.shared.borrow_mut();
        if !mail.list_rows().iter().any(|(row_id, _)| *row_id == id) {
            return false;
        }
        if mail.bulk_selecting {
            let selected = mail.toggle_bulk_selection(id);
            drop(mail);
            self.process_intents();
            return selected;
        }
        let is_draft = mail
            .mailbox
            .get(id)
            .is_some_and(|message| message.folder == Folder::Drafts);
        if is_draft {
            mail.push_intent(Intent::EditDraft(id));
        } else {
            mail.open_message(state, id);
        }
        drop(mail);
        self.process_intents();
        true
    }

    fn apply_bulk(&self, state: &mut ui::widget::Edits<'_>, action: BulkAction) -> usize {
        let affected = self.shared.borrow_mut().apply_bulk_action(state, action);
        if affected > 0 {
            #[allow(
                clippy::cast_precision_loss,
                reason = "a count only selects a plural form; exactness past 2^52 is irrelevant"
            )]
            let count = affected as f64;
            self.shared.borrow_mut().status = Some(Notice::success(CATALOG.t_args(
                match action {
                    BulkAction::Archive => "mail-status-archived",
                    BulkAction::Delete => "mail-status-trashed",
                    BulkAction::MarkRead => "mail-status-marked-read",
                    BulkAction::MarkUnread => "mail-status-marked-unread",
                    BulkAction::Purge => "mail-status-purged",
                },
                &[("count", ArgValue::Number(count))],
            )));
        }
        affected
    }

    fn perform_bulk_semantic_action(
        &mut self,
        raw: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
    ) -> bool {
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        if raw == "toggle" {
            let selecting = self.shared.borrow().bulk_selecting;
            self.shared.borrow_mut().set_bulk_selecting(!selecting);
            self.process_intents();
            self.dirty = true;
            return true;
        }
        if !self.shared.borrow().bulk_selecting {
            return false;
        }
        let bulk = match raw {
            "archive" => Some(BulkAction::Archive),
            "delete" => Some(BulkAction::Delete),
            "read" => Some(BulkAction::MarkRead),
            "unread" => Some(BulkAction::MarkUnread),
            _ => None,
        };
        if let Some(bulk) = bulk {
            if self.apply_bulk(state, bulk) == 0 {
                return false;
            }
        } else {
            match raw {
                "select-all" => self.select_all_in_folder(),
                "purge" => self
                    .shared
                    .borrow_mut()
                    .push_intent(Intent::ConfirmBulkPurge),
                _ => return false,
            }
        }
        self.process_intents();
        self.dirty = true;
        true
    }

    #[allow(
        clippy::too_many_lines,
        reason = "mail's discoverable single-key command map is intentionally centralized to keep chord guards consistent"
    )]
    fn global_shortcuts(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) {
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return;
        };
        let command = modifiers.state().control_key() || modifiers.state().super_key();
        // Bare letter shortcuts must never steal a platform/editor chord such
        // as Ctrl+C, Ctrl+S, or Alt+R. The focused widget gets first refusal;
        // this is the final safety net for commands addressed to the shell.
        let plain = !command && !modifiers.state().alt_key();
        let selected = self.shared.borrow().selected;
        let bulk_selecting = self.shared.borrow().bulk_selecting;
        // Ctrl/Cmd+A belongs to a text editor whenever one has focus. Compose
        // is already a modal first-responder, and this extra list-context guard
        // prevents the shell shortcut from ever turning a reader/detail page
        // into a hidden selection operation on narrow layouts.
        let list_context =
            !self.shared.borrow().narrow || self.nav.front_pane() == NavPane::Overview;
        match logical_key {
            // Selection browsing is intentionally harmless (see
            // `move_selection`); Enter is the unmistakable read boundary. It
            // uses the same `open_message` path as a list click and Ctrl+K’s
            // normal Open action, including the canonical SetRead action.
            Key::Named(NamedKey::Enter) if plain && list_context && !bulk_selecting => {
                if let Some(id) = selected
                    && self.shared.borrow_mut().open_message(state, id)
                {
                    self.process_intents();
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Escape) if bulk_selecting => {
                self.shared.borrow_mut().set_bulk_selecting(false);
                *handled = true;
            }
            // `P` is deliberately named Preview, not Open: it keeps the read
            // flag untouched and shows the same persistent reader disclosure
            // as the explicit resource operation.
            Key::Character(ch) if plain && ch.eq_ignore_ascii_case("p") && !bulk_selecting => {
                if let Some(id) = selected
                    && self.shared.borrow_mut().preview_message(id)
                {
                    self.process_intents();
                    *handled = true;
                }
            }
            Key::Character(ch) if plain && ch == "?" => {
                self.shortcuts.show();
                *handled = true;
            }
            Key::Character(ch) if plain && ch.eq_ignore_ascii_case("x") => {
                let selecting = self.shared.borrow().bulk_selecting;
                self.shared.borrow_mut().set_bulk_selecting(!selecting);
                *handled = true;
            }
            Key::Character(ch) if command && ch.eq_ignore_ascii_case("a") && list_context => {
                self.shared.borrow_mut().select_all_visible();
                *handled = true;
            }
            Key::Character(ch)
                if (plain && ch.eq_ignore_ascii_case("c"))
                    || (command && ch.eq_ignore_ascii_case("n")) =>
            {
                self.compose.open_blank();
                *handled = true;
            }
            Key::Character(ch) if plain && (ch == "j" || ch == "k") => {
                let delta = if ch == "j" { 1 } else { -1 };
                self.shared.borrow_mut().move_selection(state, delta);
                *handled = true;
            }
            Key::Character(ch) if plain && (ch == "a" || ch == "f") && !bulk_selecting => {
                if let Some(id) = selected {
                    self.shared.borrow_mut().push_intent(if ch == "a" {
                        Intent::ReplyAll(id)
                    } else {
                        Intent::Forward(id)
                    });
                    self.process_intents();
                    *handled = true;
                }
            }
            Key::Character(ch) if plain && ch == "I" => {
                if bulk_selecting {
                    self.shared
                        .borrow_mut()
                        .apply_bulk_action(state, BulkAction::MarkRead);
                    *handled = true;
                } else if let Some(id) = selected {
                    self.shared.borrow_mut().set_read(state, id, true);
                    *handled = true;
                }
            }
            // Searching mail is the platform's search, narrowed to the open
            // folder; the palette offers the rest of the mailbox from there.
            Key::Character(ch)
                if (plain && ch == "/") || (command && ch.eq_ignore_ascii_case("f")) =>
            {
                let request = self.folder_search();
                self.state().search.open(request);
                self.dirty = true;
                *handled = true;
            }
            Key::Named(NamedKey::F5) => {
                let _ = self.state().refresh_active_folder();
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.shared.borrow_mut().move_selection(state, 1);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.shared.borrow_mut().move_selection(state, -1);
                *handled = true;
            }
            Key::Character(ch) if plain && ch.eq_ignore_ascii_case("e") => {
                if bulk_selecting {
                    self.apply_bulk(state, BulkAction::Archive);
                    self.process_intents();
                    *handled = true;
                } else if let Some(id) = selected {
                    self.shared
                        .borrow_mut()
                        .apply_message_action(state, id, CtxAction::Archive);
                    self.process_intents();
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Delete | NamedKey::Backspace)
                if plain && modifiers.state().shift_key() && bulk_selecting =>
            {
                self.shared
                    .borrow_mut()
                    .push_intent(Intent::ConfirmBulkPurge);
                self.process_intents();
                *handled = true;
            }
            Key::Character(ch) if plain && ch.eq_ignore_ascii_case("s") => {
                if let Some(id) = selected {
                    self.shared
                        .borrow_mut()
                        .apply_message_action(state, id, CtxAction::ToggleStar);
                    *handled = true;
                }
            }
            Key::Character(ch) if plain && ch.eq_ignore_ascii_case("r") => {
                if let Some(id) = selected {
                    self.shared.borrow_mut().push_intent(Intent::Reply(id));
                    self.process_intents();
                    *handled = true;
                }
            }
            Key::Character(ch) if plain && ch.eq_ignore_ascii_case("u") => {
                if bulk_selecting {
                    self.apply_bulk(state, BulkAction::MarkUnread);
                    *handled = true;
                } else if let Some(id) = selected {
                    self.shared.borrow_mut().set_read(state, id, false);
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Delete | NamedKey::Backspace) if plain => {
                self.delete_shortcut(state);
                *handled = true;
            }
            Key::Character(ch) if plain && ch == "#" => {
                self.delete_shortcut(state);
                *handled = true;
            }
            _ => {}
        }
    }

    fn delete_shortcut(&mut self, state: &mut ui::widget::Edits<'_>) {
        let selected = self.shared.borrow().selected;
        if self.shared.borrow().bulk_selecting {
            self.apply_bulk(state, BulkAction::Delete);
        } else if let Some(id) = selected {
            self.shared
                .borrow_mut()
                .apply_message_action(state, id, CtxAction::Delete);
        }
        self.process_intents();
    }
}

mod person_accounts;
#[path = "server_deltas.rs"]
mod server_deltas;
use server_deltas::{first_fetch, pass_over_draft_copies, server_changes, take_found};
use ui::i18n::ArgValue;

#[path = "mailroom_rows.rs"]
mod mailroom_rows;

#[path = "app/empty.rs"]
mod empty;

#[cfg(test)]
mod tests;

mod view;
pub(crate) use view::window;

#[cfg(all(feature = "live", any(test, feature = "fake-servers")))]
mod driven;
