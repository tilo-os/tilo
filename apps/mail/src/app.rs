//! `MailApp` — the root widget.
//!
//! It is the toolkit's window shell, [`ui::widget::NavShell`], with three
//! columns —
//!
//! ```text
//! [ sidebar (accounts + folders) | list (overview) | message (detail) ]
//! ```
//!
//! The shell owns the header row, the sidebar toggle, the widths, the drawer
//! and the push stack. The sidebar is a followed [`NavList`](ui::widget::NavList), the list a
//! [`MessageList`] and the message a [`MessageReader`]; they share one
//! [`MailState`] and raise [`Intent`]s and [`Command`]s for effects that
//! reach past their own column.
//!
//! On top of the shell the app layers the transient overlays it owns directly:
//! the movable [`ComposeWindow`], the row context menu and the account-setup
//! sheet. Status messages show as the shell's toast.
//!
//! A live window loads the [`AccountStore`] of its host's profile and drives
//! the account [`crate::engine`] worker (IMAP fetch / SMTP submit). It does
//! **not** fabricate a sample mailbox — an unconfigured account shows an empty
//! state. The bundled sample corpus is a screenshot's fixture.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::TryRecvError;
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
use ui::framework::PersonRecord;
use ui::icons::Icon;
use ui::portal::{FileFilter, OpenFileChannel, OpenFileOutcome, PickRequest};
use ui::presence::PresenceCtx;
use ui::search::{ScopeDecl, SearchOpener, SearchRequest};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::toast::ToastKind;
use ui::widget::{
    Button, ContextMenu, EmptyState, MenuItem, ModalDialog, NavPane, NavShell, SidebarPlacement,
    TaffyTreeExt, Text, Widget,
};
use ui::{ImageStore, TextRole};

use crate::account::{Account, AccountStore};
use crate::actions::{DraftData, Outgoing, ServerUpdate};
#[path = "contact_recipients.rs"]
mod contact_recipients;

use crate::compose::{ComposeAction, ComposeEventContext, ComposeWindow};
use crate::engine::{EngineEvent, EngineRequest, EngineRequestId, EngineRequestKind};
use crate::header::{BarSpec, header_actions, list_bar, reader_bar};
use crate::list::MessageList;
use crate::model::{
    AccountConfig, Addr, Attachment, ComposeFormat, Draft, Folder, Message, MessageId,
};
use crate::reader::MessageReader;
use crate::resource::{Mail, mailbox, read_mail};
use crate::shortcuts_overlay::MailShortcutsOverlay;
use crate::sidebar;
use crate::state::{BulkAction, Command, CtxAction, Intent, MailState, Shared};
use crate::util::format_absolute;

/// How often the app auto-refreshes the current folder from the server while in
/// use. Checked each active frame; a manual refresh (the `r` shortcut) forces
/// one immediately regardless.
const AUTO_REFRESH_MIN: i64 = 5;
/// A loaded mail body can be huge, but Mail refuses to turn a resource action
/// into an unbounded filesystem write. Larger attachments can be downloaded
/// through Files after Mail exposes a proper streamed handoff.
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

/// What the open row menu acts on: the message and its actions, in the
/// menu's order.
struct MenuTarget {
    message: MessageId,
    items: Vec<CtxAction>,
}

/// The confirmation before trashed messages are deleted for good.
struct PurgeConfirmation {
    dialog: ModalDialog<Text>,
    /// `Some(true)` once confirmed, `Some(false)` once dismissed.
    answer: Rc<Cell<Option<bool>>>,
}

/// The engine worker channel pair (live app only).
struct EngineHandle {
    tx: std::sync::mpsc::Sender<EngineRequest>,
    rx: std::sync::mpsc::Receiver<EngineEvent>,
}

/// A message from the outbox the engine is delivering. It joins Sent only
/// once the server accepts it: treating MIME serialization as delivery makes
/// an offline fallback (and a rejected SMTP send) look like mail was sent.
struct PendingSend {
    outgoing: Outgoing,
    /// The copy Sent shows once it is delivered.
    message: Message,
    done: DeliveryDone,
}

/// How the drain that asked for a server update hears how it went.
type ServerUpdateDone = Box<dyn FnOnce(Result<(), String>)>;

/// How the outbox drain hears how a delivery went: delivered, or refused
/// for good with the server's reason; an error leaves it to try again.
type DeliveryDone = Box<dyn FnOnce(Result<ui::tilo_resource::Settled, String>)>;

/// The server updates the document's projection asked for: queued until the
/// next sync hands them to the engine, then carried out by it. Whichever
/// device's drain asked, the engine's acknowledgement records the update as
/// applied in the document ([`Mail::SERVER_APPLIED`](crate::resource::Mail::SERVER_APPLIED)).
#[derive(Default)]
pub(crate) struct ServerOutbox {
    queued: Vec<(ServerUpdate, ServerUpdateDone)>,
    sent: HashMap<EngineRequestId, (ServerUpdate, ServerUpdateDone)>,
    /// Messages from the document's outbox, waiting for the engine.
    deliveries: Vec<(Outgoing, DeliveryDone)>,
}

impl ServerOutbox {
    /// Carry out `update`, then tell `done` how it went.
    pub(crate) fn push(&mut self, update: ServerUpdate, done: ServerUpdateDone) {
        self.queued.push((update, done));
    }

    /// Deliver `outgoing`, then tell `done` how it went.
    pub(crate) fn deliver(&mut self, outgoing: Outgoing, done: DeliveryDone) {
        self.deliveries.push((outgoing, done));
    }
}

/// An attachment whose bytes were already hydrated and are now waiting for a
/// user-selected destination from the desktop portal. The path itself is never
/// invented or retained before the portal grants it.
/// Where account add / edit / remove is persisted.
///
/// Only a live window writes an account store, and only the one of the
/// profile its host gave it: the person's own (`$XDG_CONFIG_HOME/tilo/accounts/`)
/// in a session, a test or `--data-dir` profile's own otherwise. A screenshot
/// or a past version writes nothing.
#[derive(Clone)]
enum AccountsPersist {
    /// The configuration directory of the host's profile.
    Profile(std::path::PathBuf),
    /// A throwaway directory, removed when the app is dropped.
    #[cfg(test)]
    Sandbox(Arc<tempfile::TempDir>),
    /// Nothing is written: a capture, a past version, or a profile without a
    /// configuration directory.
    Disabled,
}

impl AccountsPersist {
    /// A fresh throwaway sandbox for tests.
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

    /// The configuration directory this persists into.
    fn root(&self) -> Option<std::path::PathBuf> {
        match self {
            AccountsPersist::Profile(root) => Some(root.clone()),
            #[cfg(test)]
            AccountsPersist::Sandbox(dir) => Some(dir.path().to_path_buf()),
            AccountsPersist::Disabled => None,
        }
    }

    /// The accounts stored there; none when nothing is.
    fn load(&self) -> AccountStore {
        self.root()
            .map_or_else(AccountStore::new, |root| AccountStore::load_in(&root))
    }

    /// Persist `accounts` to the target this variant names.
    fn persist(&self, accounts: &AccountStore) -> std::io::Result<()> {
        match self.root() {
            Some(root) => accounts.persist_in(&root),
            None => Ok(()),
        }
    }
}

/// The configuration directory of the profile `storage` belongs to: the
/// person's own in a session, else the directory of the settings document a
/// test, capture or `--data-dir` host gave the app, so such a profile never
/// reads or writes the person's accounts.
fn profile_config_root(storage: &ui::storage::AppStorage) -> Option<std::path::PathBuf> {
    match storage.settings_document() {
        ui::storage::SettingsDocument::Session => accounts::config_root(),
        ui::storage::SettingsDocument::File(path) => {
            path.parent().map(std::path::Path::to_path_buf)
        }
    }
}

struct PendingSetupValidation {
    config: AccountConfig,
    editing: Option<usize>,
    receiver: std::sync::mpsc::Receiver<crate::validate::ValidationState>,
    cancellation: Arc<std::sync::atomic::AtomicBool>,
}

struct PendingSetupDiscovery {
    config: AccountConfig,
    editing: Option<usize>,
    receiver: std::sync::mpsc::Receiver<Option<crate::autoconfig::Autoconfig>>,
    cancellation: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for PendingSetupDiscovery {
    fn drop(&mut self) {
        self.cancellation
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Drop for PendingSetupValidation {
    fn drop(&mut self) {
        self.cancellation
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "the bools split across unrelated concerns (shell layout flags and engine-request bookkeeping); grouping them would need a real design pass"
)]
pub struct MailApp {
    pub(crate) shared: Shared,
    /// Where draft attachment bytes live; drafts carry only their content id.
    /// The engine worker holds a clone to build outgoing MIME.
    content: ContentStore,
    nav: NavShell,
    nav_node: Option<NodeId>,
    /// The title the list's header shows.
    overview_title: String,
    /// The list's and the open message's header actions as last built.
    list_bar: BarSpec,
    reader_bar: BarSpec,
    /// Retained first-run surface so the primary action uses the shared
    /// pointer, focus, keyboard and accessibility contract.
    first_run: EmptyState,
    first_run_node: Option<NodeId>,
    compose: ComposeWindow,
    /// A modal, read-only key map. It is local presentation state; every
    /// shortcut it documents still routes to the normal visible/canonical path.
    shortcuts: MailShortcutsOverlay,
    /// The Drafts-folder message the open composer is editing, if any. On save
    /// the same message is updated (not duplicated); on send it is removed.
    editing_draft: Option<MessageId>,
    /// Retained projection of the canonical CRDT draft; preserves its metadata
    /// when the Drafts-folder row reopens and reuses that row across frames.
    projected_draft: RefCell<Option<(MessageId, Draft)>>,
    /// The row menu, and the message it acts on while open.
    menu: ContextMenu,
    menu_target: Option<MenuTarget>,
    /// A row menu to open once the list has been laid out (a capture).
    pending_menu: Option<MessageId>,
    /// An explicit destructive confirmation for the selection toolbar. This
    /// is local presentation state; the actual purge remains one canonical
    /// verb of the mailbox, only after the person confirms.
    bulk_purge_confirmation: Option<PurgeConfirmation>,
    /// Opens the platform search from the app's own input handling.
    search: SearchOpener,

    /// The account sheet while it is open, and its node in the window's
    /// tree once laid out there.
    setup: Option<crate::setup_sheet::SetupSheet>,
    setup_node: Option<NodeId>,
    setup_editing: Option<usize>,
    setup_validation: Option<PendingSetupValidation>,
    setup_discovery: Option<PendingSetupDiscovery>,

    engine: Option<EngineHandle>,
    /// What the document still has to tell mail servers, on its way to the
    /// engine.
    server_outbox: Rc<RefCell<ServerOutbox>>,
    /// The drain that fills the outbox, told when a server may now accept
    /// what it refused (a fetch arrived, the engine reconnected).
    projection: Option<ui::framework::Projection>,
    /// The drain that hands the document's outbox to the engine.
    delivery: Option<ui::framework::Projection>,
    /// Outbox messages the engine is delivering, by request.
    pending_send: HashMap<EngineRequestId, PendingSend>,
    /// The outbox message the composer was reopened from after its server
    /// refused it; sending again replaces it.
    resending: Option<String>,
    /// Where account changes persist. The live app targets the real user config
    /// root; the sample / shots / test constructors target a throwaway sandbox
    /// so they can never write into the real account store.
    accounts_persist: AccountsPersist,
    /// The device-local account id last flushed to [`AccountsPersist`]. Pane,
    /// accessibility, and resource operations all select through `MailState`,
    /// so observing the id here gives every selection path one persistence
    /// boundary without leaking that preference into Automerge.
    persisted_active_account: Option<String>,
    /// The person's accounts, which Settings keeps and every device of theirs
    /// shares, followed in a live window; and the revision last shown.
    person_accounts: Option<(PersonRecord<PersonAccounts>, Option<u64>)>,
    pub(crate) contact_directory: Option<PersonRecord<tilo_contact_directory::ContactDirectory>>,
    pub(crate) has_contact_draft_conflict: bool,
    /// The person's Mail preferences, followed in a live window: the compose
    /// format and the folded account sidebars.
    preferences: Option<ui::framework::Setting<crate::resource::MailPreferences>>,
    /// The preferences as this window last read or stored them.
    followed_preferences: crate::resource::MailPreferences,
    connect_requested: bool,
    /// True between posting a connect/fetch and receiving its result, so the
    /// app only keeps polling the worker while a reply is outstanding (it fully
    /// idles otherwise instead of busy-looping). `pending_engine_requests`
    /// rather than an event-derived boolean is the source of truth: a connect
    /// can emit several events, while successful IMAP writes emit none besides
    /// their terminal worker acknowledgement.
    awaiting_engine: bool,
    background_poll_deadline: Cell<Option<ui::time::Instant>>,
    pending_engine_requests: HashMap<EngineRequestId, EngineRequestKind>,
    /// The one refresh invocation currently owned by the resource-operation
    /// host.  It remains occupied until the host has polled its terminal
    /// update, so a subsequent refresh cannot overwrite an unobserved result.
    resource_refresh_invocation: Option<(EngineRequestId, tilo_actions::InvocationId)>,
    resource_refresh_result: Option<tilo_actions::InvocationUpdate>,
    /// One portal-open attachment staging request. The returned descriptor is
    /// copied straight into the content store; its original path is never retained.
    compose_attachment_rx: Option<OpenFileChannel>,
    /// What effects asked to show that needs the document, for the next sync.
    reveals: Vec<Reveal>,
    /// What search hits and agents asked to open, taken up on the next
    /// settle.
    opens: std::rc::Rc<std::cell::RefCell<Vec<crate::actions::MailTarget>>>,
    /// Callers waiting for a refresh to settle.
    effect_replies: Vec<(tilo_actions::InvocationId, ui::framework::EffectReply)>,
    /// Wall-clock minute of the last automatic inbox refresh, so periodic sync
    /// fires at most once per [`AUTO_REFRESH_MIN`] while the app is in use.
    last_auto_refresh_min: i64,

    rect: Rect,
    /// The host theme captured during layout, for the overlays laid out
    /// outside the taffy tree.
    layout_theme: Theme,
    /// The shell starts docked; the first real layout snaps it to its
    /// resting state for the window's width so a narrow launch does not
    /// animate the drawer closed.
    first_layout: bool,
    /// The screenshot scenario the framework asked to stage, until the next
    /// sync stages it with the document.
    pending_stage: Option<String>,
    dirty: bool,
    /// The layout tree a test laid the app out in, for routing events.
    #[cfg(test)]
    test_layout: Option<(TaffyTree<()>, NodeId)>,
}

impl MailApp {
    /// Stage screenshot `scenario` on the next sync, which has the document.
    pub(crate) fn request_stage(&mut self, scenario: &str) {
        self.pending_stage = Some(scenario.to_owned());
        self.dirty = true;
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Mail>,
    ) {
        self.shared.borrow_mut().dispatcher = Some(dispatcher.clone());
        self.compose.attach_framework(dispatcher);
    }

    pub(crate) fn has_pending_send(&self) -> bool {
        !self.pending_send.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn mailbox_message_count(&self) -> usize {
        self.shared.borrow().mailbox.messages.len()
    }

    /// A window someone uses: the accounts of the host's profile, and the
    /// engine worker that keeps them current. No fabricated messages.
    pub(crate) fn for_window(
        storage: &ui::storage::AppStorage,
        content: Option<ContentStore>,
    ) -> Self {
        let persist = profile_config_root(storage)
            .map_or(AccountsPersist::Disabled, AccountsPersist::Profile);
        let accounts = persist.load();
        let shared = MailState::shared(MailState::new_live(accounts, avatar_store()));
        let content = content.unwrap_or_else(ContentStore::memory);
        let (tx, rx) = crate::engine::spawn(content.clone());
        Self::assemble(shared, content, Some(EngineHandle { tx, rx }), persist)
    }

    /// A past version in History: what the document holds, and nothing that
    /// reaches a mail server or the account store.
    pub(crate) fn for_past_version() -> Self {
        let shared = MailState::shared(MailState::new_live(
            AccountStore::new(),
            avatar_store_offline(),
        ));
        Self::assemble(
            shared,
            ContentStore::memory(),
            None,
            AccountsPersist::Disabled,
        )
    }

    /// A screenshot: the scenario's fixture mailbox and accounts, no engine,
    /// a pinned clock and nothing written outside the document.
    pub(crate) fn for_capture(fixture: &crate::sample::MailFixture) -> Self {
        let shared = MailState::shared(MailState::from_fixture(fixture, avatar_store_offline()));
        Self::assemble(
            shared,
            ContentStore::memory(),
            None,
            AccountsPersist::Disabled,
        )
    }

    /// The sample mailbox, editing the document it is synced with and
    /// persisting account changes into a sandbox.
    #[cfg(test)]
    pub fn new_sample() -> Self {
        let shared = MailState::shared(MailState::new_sample(avatar_store_offline()));
        let mut app = Self::assemble(
            shared,
            ContentStore::memory(),
            None,
            AccountsPersist::sandbox(),
        );
        app.attach_framework(ui::framework::testing::dispatcher("so.tilo.mail"));
        app
    }

    /// No account configured yet, persisting into a sandbox.
    #[cfg(test)]
    pub fn new_first_run() -> Self {
        let shared = MailState::shared(MailState::new_live(
            AccountStore::new(),
            avatar_store_offline(),
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
        let overview_title = shared.borrow().folder.title().to_owned();
        let nav = Self::build_shell(&shared, &overview_title);
        let (list_bar, reader_bar) = {
            let state = shared.borrow();
            (list_bar(&state), reader_bar(&state))
        };
        Self {
            shared,
            content,
            nav,
            nav_node: None,
            overview_title,
            list_bar,
            reader_bar,
            first_run: EmptyState::new("No email account yet")
                .with_icon(Icon::Mail)
                .with_action("Add account")
                .with_action_id("mail.add-account"),
            first_run_node: None,
            compose: ComposeWindow::new(ComposeFormat::default()),
            shortcuts: MailShortcutsOverlay::new(),
            editing_draft: None,
            projected_draft: RefCell::new(None),
            menu: ContextMenu::new(),
            menu_target: None,
            pending_menu: None,
            bulk_purge_confirmation: None,
            search: SearchOpener::new(),
            setup: None,
            setup_node: None,
            setup_editing: None,
            setup_validation: None,
            setup_discovery: None,
            engine,
            server_outbox: Rc::default(),
            projection: None,
            delivery: None,
            pending_send: HashMap::new(),
            resending: None,
            accounts_persist,
            persisted_active_account: None,
            person_accounts: None,
            contact_directory: None,
            has_contact_draft_conflict: false,
            preferences: None,
            followed_preferences: crate::resource::MailPreferences::default(),
            connect_requested: false,
            awaiting_engine: false,
            background_poll_deadline: Cell::new(None),
            pending_engine_requests: HashMap::new(),
            resource_refresh_invocation: None,
            resource_refresh_result: None,
            compose_attachment_rx: None,
            reveals: Vec::new(),
            opens: std::rc::Rc::default(),
            effect_replies: Vec::new(),
            last_auto_refresh_min: 0,
            rect: Rect::ZERO,
            layout_theme: Theme::dark(),
            first_layout: true,
            pending_stage: None,
            dirty: true,
            #[cfg(test)]
            test_layout: None,
        }
    }

    // ---- screenshot staging ----------------------------------------------

    /// Stage the view state screenshot `scenario` shows over the fixture
    /// mailbox. Runs in the sync after the framework asked for it, with the
    /// document, so a scenario that changes mail (moving messages to Trash
    /// before confirming a purge) does it through the resource's actions.
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
            "setup" | "setup_narrow" => {
                let active = self.shared.borrow().accounts.active;
                self.open_setup_edit(Some(active));
            }
            "setup_add" => self.stage_setup_add(),
            "setup_blank" => self.open_setup_edit(None),
            "setup_discovering" => self.stage_setup_discovering(),
            "setup_oauth" => {
                self.open_setup_edit(None);
                self.set_setup_input(0, "Grace Hopper");
                self.set_setup_input(1, "grace.hopper@gmail.com");
                // The built-in provider table only: a capture opens no socket.
                self.maybe_autofill_live_typing();
                self.setup.as_mut().map(|sheet| sheet.focus_field(1));
            }
            "html_mail" => self.stage_html_mail(doc),
            "search" => self
                .search
                .open(SearchRequest::this_app().with_query("spatial")),
            "preview" => self.stage_preview(),
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
                self.awaiting_engine = true;
                self.shared.borrow_mut().status = Some("Syncing Inbox…".into());
            }
            "offline" => {
                self.shared.borrow_mut().status =
                    Some("You’re offline · Showing downloaded mail".into());
            }
            "error" => {
                self.stage_setup_add();
                self.set_setup_error(Some(
                    "Couldn’t connect to IMAP. Check the server, port, and TLS settings, then try again."
                        .into(),
                ));
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// A blank "Add account" sheet with an email typed and its servers
    /// filled in, focus on the password field so its focus ring shows.
    fn stage_setup_add(&mut self) {
        self.open_setup_edit(None);
        self.set_setup_input(0, "Alex Morgan");
        self.set_setup_input(1, "grace@fastmail.com");
        self.set_setup_input(3, "correct horse battery");
        // The built-in provider table only: a capture opens no socket.
        self.maybe_autofill_live_typing();
        self.setup.as_mut().map(|sheet| sheet.focus_field(3));
    }

    /// An address of a domain the built-in table does not know, its servers
    /// being looked up.
    fn stage_setup_discovering(&mut self) {
        self.open_setup_edit(None);
        self.set_setup_input(1, "grace@hopper-labs.example");
        self.set_setup_input(3, "correct horse battery");
        // The sender stays open for the capture's life: a closed one would
        // end the lookup with no result.
        let (sender, receiver) = std::sync::mpsc::channel();
        std::mem::forget(sender);
        let config = self.read_setup_config();
        self.setup_discovery = Some(PendingSetupDiscovery {
            config,
            editing: None,
            receiver,
            cancellation: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        self.set_provider_status(crate::setup_sheet::ProviderStatus::Finding);
    }

    /// The bundled HTML newsletter open in the reader (it is starred, so the
    /// filled-star action shows too).
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

    /// An unread message inspected through the read-neutral preview, so the
    /// persistent preview indicator shows.
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

    /// Two messages selected for bulk triage: the extra header row, the
    /// selected-row treatment and the bulk controls.
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

    /// Two messages moved to Trash and selected, with the confirmation that
    /// names how many will be deleted for good.
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

    /// A reply to the newest inbox message, rather than a blank composition.
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
        self.dirty = true;
    }

    /// Queue one worker request and account for it exactly once.  Every engine
    /// backend emits a matching `EngineEvent::Completed` after handling the
    /// request, including successful flag/move/purge writes.
    fn post_engine(&mut self, request: EngineRequest) -> Option<EngineRequestId> {
        let Some(engine) = &self.engine else {
            return None;
        };
        let request_id = request.request_id();
        let request_kind = request.kind();
        if engine.tx.send(request).is_err() {
            self.shared.borrow_mut().status = Some("Mail couldn’t reach the mail server".into());
            self.dirty = true;
            return None;
        }
        self.pending_engine_requests
            .insert(request_id, request_kind);
        self.awaiting_engine = true;
        Some(request_id)
    }

    /// Accept only events that match a request still pending in this app. A
    /// worker event is untrusted transport input: a malformed, delayed, or
    /// out-of-order result must not update a different operation's UI state.
    fn expects_engine_event(&self, request_id: EngineRequestId, kind: EngineRequestKind) -> bool {
        self.pending_engine_requests.get(&request_id) == Some(&kind)
    }

    // ---- window shell -----------------------------------------------------

    /// The window shell: the sidebar of accounts and folders, the message
    /// list and the open message, with each column's header actions.
    fn build_shell(shared: &Shared, overview_title: &str) -> NavShell {
        let add_account = {
            let shared = shared.clone();
            Button::icon_only("mail-add-account", ui::icons::Icon::Plus, "Add account").on_view(
                move || {
                    shared
                        .borrow_mut()
                        .push_command(Command::AccountSettings(None));
                },
            )
        };
        NavShell::new()
            .with_id("mail-shell")
            .sidebar(Box::new(sidebar::sidebar(shared)))
            .overview(Box::new(MessageList::new(shared.clone())))
            .content(Box::new(MessageReader::new(shared.clone())))
            .sidebar_header("", vec![Box::new(add_account)])
            .overview_header(
                overview_title,
                header_actions(&list_bar(&shared.borrow()), shared, "list"),
            )
            .content_header(
                "",
                header_actions(&reader_bar(&shared.borrow()), shared, "reader"),
            )
    }

    /// Tell the columns what the shell laid out: whether the list and the
    /// message are separate screens, and which one is in front.
    fn publish_shell_flags(&self) {
        let geometry = self.nav.geometry();
        let narrow = !geometry.stack.is_empty();
        let detail = narrow && self.nav.front_pane() == NavPane::Content;
        let mut st = self.shared.borrow_mut();
        st.narrow = narrow;
        st.detail_pushed = detail;
    }

    /// The list's header title: the open folder, or how many messages are
    /// selected.
    fn overview_title(state: &MailState) -> String {
        if state.bulk_selecting {
            match state.selected_bulk_ids().len() {
                0 => "Select messages".to_owned(),
                1 => "1 selected".to_owned(),
                n => format!("{n} selected"),
            }
        } else {
            state.folder.title().to_owned()
        }
    }

    /// The platform search, narrowed to the open folder.
    fn folder_search(&self) -> SearchRequest {
        let folder = self.shared.borrow().folder;
        SearchRequest::within(ScopeDecl::new(
            format!("mail.folder:{}", folder.as_key()),
            folder.title(),
        ))
    }

    // ---- intent processing ------------------------------------------------

    /// The message currently selected, if any. Lets a test observe where a
    /// Ctrl+K activation navigated.
    pub fn selected_message(&self) -> Option<crate::model::MessageId> {
        self.shared.borrow().selected
    }

    /// Strip a server-supplied attachment name down to the harmless suggested
    /// filename shown by the host Save dialog. The actual destination still
    /// comes exclusively from that dialog; this merely avoids presenting an
    /// absolute path, a traversal component, or a control-character-heavy
    /// name as its default.
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
        if self.compose_attachment_rx.is_some() {
            self.shared.borrow_mut().status = Some("Attachment picker is already open".into());
            return;
        }
        self.compose_attachment_rx = Some(
            PickRequest::open("Attach file", "Attach")
                .thread_name("mail-portal-stage-attachment")
                .filter(FileFilter::all_files())
                .send_open_file(),
        );
        self.shared.borrow_mut().status = Some("Choose a file to attach".into());
        self.dirty = true;
    }

    /// Drain descriptor-only portal selection. A cancel/failure/disconnect is
    /// terminal and leaves the draft unchanged; staged content is verified by
    /// the content store before metadata becomes visible in the composer.
    fn drain_compose_attachment(&mut self) {
        let outcome = self
            .compose_attachment_rx
            .as_ref()
            .map(std::sync::mpsc::Receiver::try_recv);
        let Some(outcome) = outcome else { return };
        match outcome {
            Ok(OpenFileOutcome::Opened(source)) => {
                self.compose_attachment_rx = None;
                if !self.compose.open {
                    self.shared.borrow_mut().status = Some(
                        "Attachment selection cancelled because the composer was closed".into(),
                    );
                    return;
                }
                let name = source.display_name().to_string();
                let result =
                    crate::attachment_store::stage_file(&self.content, source.into_file(), &name)
                        .and_then(|attachment| self.compose.add_attachment(attachment));
                self.shared.borrow_mut().status = Some(match result {
                    Ok(()) => format!("Attached {name}"),
                    Err(error) => format!("Couldn’t attach {name}: {error}"),
                });
            }
            Ok(OpenFileOutcome::Cancelled) => {
                self.compose_attachment_rx = None;
                self.shared.borrow_mut().status = Some("Attachment selection cancelled".into());
            }
            Ok(OpenFileOutcome::Failed(error)) => {
                self.compose_attachment_rx = None;
                self.shared.borrow_mut().status =
                    Some(format!("Couldn’t open attachment picker: {error}"));
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.compose_attachment_rx = None;
                self.shared.borrow_mut().status =
                    Some("Attachment picker disconnected before selecting a file".into());
            }
        }
        self.dirty = true;
    }

    /// Begin a host-native Save flow for a hydrated attachment. This is the
    /// sole Mail path that writes attachment bytes: it asks the portal for a
    /// destination first and is deliberately asynchronous so a slow desktop
    /// chooser never freezes Mail.
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
                Intent::SaveAttachment(message_id, index) => {
                    self.request_attachment_save(message_id, index);
                }
                Intent::Engine(req) => {
                    self.post_engine(req);
                }
                Intent::ConfirmBulkPurge => {
                    let count = self.shared.borrow().selected_bulk_ids().len();
                    if count == 0 {
                        self.shared.borrow_mut().status =
                            Some("Select messages before deleting them permanently".into());
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
                            Some("Permanent deletion requires an all-Trash selection".into());
                    }
                }
            }
        }
        self.dirty = true;
    }

    /// Apply the commands the window's chrome queued while routing an event.
    fn apply_commands(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let commands = self.shared.borrow_mut().take_commands();
        for command in commands {
            match command {
                Command::OpenFolder(account, folder) => {
                    let mut state = self.shared.borrow_mut();
                    state.set_active_account(account);
                    state.select_folder(doc, folder);
                }
                Command::AccountSettings(account) => self.open_setup_edit(account),
                Command::Compose => {
                    self.shared.borrow_mut().push_intent(Intent::Compose);
                }
                Command::ContactRecipients => {
                    self.open_contact_recipients();
                }
                Command::OpenSearch => {
                    let request = self.folder_search();
                    self.search.open(request);
                }
                Command::Refresh => {
                    let _ = self.refresh_active_folder();
                }
                Command::ToggleBulk => {
                    let selecting = self.shared.borrow().bulk_selecting;
                    self.shared.borrow_mut().set_bulk_selecting(!selecting);
                }
                Command::SelectAllVisible => self.shared.borrow_mut().select_all_visible(),
                Command::Bulk(BulkAction::Purge) => {
                    self.shared
                        .borrow_mut()
                        .push_intent(Intent::ConfirmBulkPurge);
                }
                Command::Bulk(action) => {
                    let affected = self.shared.borrow_mut().apply_bulk_action(doc, action);
                    if affected > 0 {
                        let noun = if affected == 1 { "message" } else { "messages" };
                        self.shared.borrow_mut().status = Some(match action {
                            BulkAction::Archive => format!("Archived {affected} {noun}"),
                            BulkAction::Delete => format!("Moved {affected} {noun} to Trash"),
                            BulkAction::MarkRead => format!("Marked {affected} {noun} read"),
                            BulkAction::MarkUnread => {
                                format!("Marked {affected} {noun} unread")
                            }
                            BulkAction::Purge => format!("Deleted {affected} {noun}"),
                        });
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
                Command::MessageMenu => {
                    let selected = self.shared.borrow().selected;
                    if let Some(id) = selected {
                        // The menu hangs from the trailing end of the
                        // message's header, where the button that asked
                        // for it stands.
                        let geometry = self.nav.geometry();
                        let column = geometry.content_column;
                        let at = Point::new(column.x1, column.y0 + geometry.header_height);
                        self.open_menu(id, at, None);
                    }
                }
            }
        }
        self.process_intents();
    }

    // ---- row menu ---------------------------------------------------------

    /// Open the row menu for `id` at `at`, beside `row` when known.
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
        self.menu_target = Some(MenuTarget { message: id, items });
        self.dirty = true;
    }

    fn close_menu(&mut self) {
        self.menu.close();
        self.menu_target = None;
        self.dirty = true;
    }

    /// Apply the row menu's chosen action, if one was chosen.
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
        let Some(action) = target.items.get(index).copied() else {
            return;
        };
        self.apply_menu_action(target.message, action, doc);
    }

    fn apply_menu_action(
        &mut self,
        message: MessageId,
        action: CtxAction,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        if self.shared.borrow().mailbox.get(message).is_none() {
            self.shared.borrow_mut().status = Some("That message is no longer available.".into());
            self.dirty = true;
            return;
        }
        self.shared
            .borrow_mut()
            .apply_message_action(doc, message, action);
        self.process_intents();
    }

    /// Open the row menu on `id`'s row in the list as last laid out.
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

    // ---- permanent deletion -----------------------------------------------

    fn open_purge_confirmation(&mut self, count: usize) {
        let answer = Rc::new(Cell::new(None));
        let noun = if count == 1 { "message" } else { "messages" };
        let body = Text::new(&format!(
            "{count} {noun} in Trash will be deleted from this mailbox and the server. This can’t be undone."
        ))
        .with_role(TextRole::Body);
        let confirm = Rc::clone(&answer);
        let cancel = Rc::clone(&answer);
        let dismiss = Rc::clone(&answer);
        let dialog = ModalDialog::new(
            if count == 1 {
                "Delete message permanently?".to_owned()
            } else {
                format!("Delete {count} messages permanently?")
            },
            body,
        )
        .with_test_id("mail-purge-confirmation")
        .with_destructive(true)
        .primary_action("Delete permanently", move || confirm.set(Some(true)))
        .secondary_action("Cancel", move || cancel.set(Some(false)))
        .on_dismiss(move || dismiss.set(Some(false)))
        .open(true);
        self.bulk_purge_confirmation = Some(PurgeConfirmation { dialog, answer });
        self.dirty = true;
    }

    /// Act on the confirmation's answer, once.
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
        format!(
            "\n\nOn {}, {} wrote:\n> {}",
            format_absolute(msg.timestamp_min),
            msg.from.display(),
            msg.snippet()
        )
    }

    fn open_reply(&mut self, id: MessageId) {
        let Some(msg) = self.shared.borrow().mailbox.get(id).cloned() else {
            return;
        };
        self.editing_draft = None;
        let subj = Self::reply_subject(&msg);
        let quote = Self::reply_quote(&msg);
        self.compose.open_reply(&msg.from.rfc5322(), &subj, &quote);
        self.close_menu();
        self.dirty = true;
    }

    /// Reply to everyone: the original sender in `To`, and every other original
    /// recipient (To + Cc, minus ourselves and the sender) in `Cc`.
    fn open_reply_all(&mut self, id: MessageId) {
        let (msg, my_email) = {
            let st = self.shared.borrow();
            match st.mailbox.get(id).cloned() {
                Some(m) => (m, st.accounts.active_config().email.to_ascii_lowercase()),
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
        self.close_menu();
        self.dirty = true;
    }

    /// Reopen a Drafts-folder message in the composer for further editing.
    fn open_draft(&mut self, id: MessageId) {
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
        self.compose.open_forward(&subj, &quote);
        self.close_menu();
        self.dirty = true;
    }

    /// Send the message the composer holds: it goes into the mailbox's
    /// outbox as an action — undone like any edit until it is delivered —
    /// and a device that holds its account delivers it.
    fn send_compose(&mut self, doc: &mut ui::widget::Edits<'_>) {
        self.store_preferences_if_changed();
        let draft = self.compose.to_draft(doc);
        let acct = self.shared.borrow().accounts.active_config();
        if !self.contact_sender_matches(&draft) {
            self.shared.borrow_mut().status = Some(crate::i18n::t("mail-contact-sender-changed"));
            self.dirty = true;
            return;
        }
        if acct.email.trim().is_empty() {
            self.shared.borrow_mut().status =
                Some("Add a sending account before sending this message".into());
            self.dirty = true;
            return;
        }
        if self.shared.borrow().dispatcher.is_none() {
            self.shared.borrow_mut().status =
                Some("Sending is unavailable in this offline preview; save a draft instead".into());
            self.dirty = true;
            return;
        }
        let from = Addr::new(acct.display_name.clone(), acct.email.clone());
        if let Err(e) = crate::transport::build_mime(&self.content, &from, &draft) {
            self.shared.borrow_mut().status = Some(format!("Send failed: {e}"));
            self.dirty = true;
            return;
        }
        if crate::transport::parse_addr_list(&draft.to).is_empty() {
            self.shared.borrow_mut().status = Some("Add a recipient before sending".into());
            self.dirty = true;
            return;
        }
        {
            let st = self.shared.borrow();
            if let Some(refused) = self.resending.take() {
                st.submit(
                    doc,
                    Mail::CANCEL_SEND.with(mailbox::CancelSend { id: refused }),
                );
            }
            st.submit(
                doc,
                Mail::SEND_MESSAGE.with(mailbox::SendMessage {
                    account: acct.email,
                    message: DraftData::from_draft(&draft),
                    in_reply_to: draft.in_reply_to.map(|id| id.to_string()),
                }),
            );
        }
        // The message is in the outbox now; the draft it was written in is
        // done with.
        if let Some(draft_id) = self.editing_draft.take() {
            let mut st = self.shared.borrow_mut();
            st.mailbox.messages.remove(&draft_id);
            st.mailbox
                .order
                .retain(|message_id| *message_id != draft_id);
        }
        self.compose.close();
        self.shared.borrow_mut().status = Some("Sending…".into());
        self.dirty = true;
    }

    fn save_compose_draft(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let draft = self.compose.to_draft(doc);
        // Persist the draft as an action (survives a crash / syncs to peers /
        // one undo boundary) …
        self.shared.borrow_mut().save_draft(doc, &draft);
        // … and materialise it as a real, visible message in the Drafts folder
        // so it can be found and reopened — not a write-only slot.
        let id = self.upsert_draft_message(doc, &draft);
        self.editing_draft = Some(id);
        self.shared.borrow_mut().status = Some("Draft saved to Drafts".into());
        self.compose.close();
        self.store_preferences_if_changed();
        self.dirty = true;
    }

    /// Insert (or, when editing an existing draft, update in place) a Drafts
    /// folder message reflecting `draft`, and return its id.
    fn upsert_draft_message(&self, doc: &mut ui::widget::Edits<'_>, draft: &Draft) -> MessageId {
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
        let id = self
            .editing_draft
            .or_else(|| self.projected_draft.borrow().as_ref().map(|(id, _)| *id))
            .unwrap_or_else(uuid::Uuid::new_v4);
        let msg = Message {
            id,
            rfc_message_id: None,
            in_reply_to: None,
            from,
            to: crate::transport::parse_addr_list(&draft.to),
            cc: crate::transport::parse_addr_list(&draft.cc),
            subject: if draft.subject.trim().is_empty() {
                "(no subject)".into()
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
        };
        {
            let mut st = self.shared.borrow_mut();
            st.mailbox.insert(msg);
            st.register_message_target(doc, &id.to_string());
        }
        *self.projected_draft.borrow_mut() = Some((id, draft.clone()));
        id
    }

    // ---- account-setup sheet ---------------------------------------------

    fn open_setup_edit(&mut self, idx: Option<usize>) {
        self.setup_validation = None;
        self.setup_discovery = None;
        let cfg = match idx {
            Some(i) => self
                .shared
                .borrow()
                .accounts
                .accounts
                .get(i)
                .map(|a| a.config.clone())
                .unwrap_or_default(),
            None => AccountConfig::default(),
        };
        self.setup_editing = idx;
        self.setup = Some(crate::setup_sheet::SetupSheet::open(
            &cfg,
            idx.is_some(),
            f64::from(self.layout_theme.tokens.metrics.panel_width_sheet),
        ));
        self.close_menu();
        self.dirty = true;
    }

    fn close_setup(&mut self) {
        self.setup_validation = None;
        self.setup_discovery = None;
        self.setup = None;
        self.dirty = true;
    }

    /// Whether the account sheet is open.
    fn setup_open(&self) -> bool {
        self.setup.is_some()
    }

    /// Whether the app is at first run (no account configured) and should show
    /// the onboarding screen rather than an empty mailbox.
    fn is_first_run(&self) -> bool {
        self.shared.borrow().accounts.is_empty() && !self.setup_open()
    }

    /// What the open sheet shows; `None` while it is closed.
    fn setup_fields(&self) -> Option<&crate::setup_sheet::SetupFields> {
        self.setup.as_ref().map(|sheet| &sheet.fields)
    }

    /// Field `i` of the open sheet (see [`crate::setup_sheet`] for the
    /// indices); empty while the sheet is closed.
    fn setup_value(&self, i: usize) -> String {
        self.setup_fields()
            .map(|fields| fields.value(i))
            .unwrap_or_default()
    }

    /// Set field `i`'s text, a silent no-op while the sheet is closed.
    fn set_setup_input(&mut self, i: usize, text: &str) {
        if let Some(fields) = self.setup_fields() {
            fields.set_value(i, text);
        }
        self.dirty = true;
    }

    /// The sheet's sign-in method (the default one while it is closed).
    fn setup_auth(&self) -> crate::model::AuthMethod {
        self.setup_fields()
            .map(crate::setup_sheet::SetupFields::auth)
            .unwrap_or_default()
    }

    fn set_setup_auth(&mut self, auth: crate::model::AuthMethod) {
        if let Some(fields) = self.setup_fields() {
            fields.set_auth(auth);
        }
        self.dirty = true;
    }

    #[cfg(test)]
    fn setup_tls(&self, server: crate::setup_sheet::Server) -> bool {
        self.setup_fields().is_some_and(|fields| fields.tls(server))
    }

    fn set_setup_tls(&mut self, server: crate::setup_sheet::Server, on: bool) {
        if let Some(fields) = self.setup_fields() {
            fields.set_tls(server, on);
        }
        self.dirty = true;
    }

    fn setup_provider(&self) -> Option<crate::oauth::OAuthProvider> {
        self.setup_fields()
            .and_then(crate::setup_sheet::SetupFields::provider)
    }

    /// The sheet's error line: why the last save, sign-in or discovery
    /// failed.
    #[cfg(test)]
    fn setup_error(&self) -> Option<String> {
        self.setup_fields()
            .and_then(crate::setup_sheet::SetupFields::error)
    }

    fn set_setup_error(&mut self, error: Option<String>) {
        if let Some(fields) = self.setup_fields() {
            fields.set_error(error);
        }
        self.dirty = true;
    }

    fn read_setup_config(&self) -> AccountConfig {
        self.setup_fields()
            .map(crate::setup_sheet::SetupFields::config)
            .unwrap_or_default()
    }

    /// Act on what the person did in the sheet: fill the servers in as a
    /// known address is typed or when the address field is left, save,
    /// cancel, remove or sign in, and keep the shown errors and the Save
    /// action current.
    fn settle_setup(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(sheet) = self.setup.as_mut() else {
            return;
        };
        let requests = sheet.take_requests();
        let email_changed = sheet.take_email_changed();
        let email_left = sheet.take_email_left();
        if email_changed {
            self.maybe_autofill_live_typing();
        }
        if email_left {
            self.maybe_autofill();
        }
        for request in requests {
            match request {
                crate::setup_sheet::SetupRequest::Save => self.save_setup(doc),
                crate::setup_sheet::SetupRequest::Cancel => self.close_setup(),
                crate::setup_sheet::SetupRequest::Remove => self.remove_editing_account(doc),
                crate::setup_sheet::SetupRequest::SignIn => self.start_oauth(),
            }
            if self.setup.is_none() {
                return;
            }
        }
        let label = self.setup_save_label();
        let busy = self.setup_validation.is_some() || self.setup_discovery.is_some();
        if let Some(sheet) = self.setup.as_mut() {
            sheet.refresh_field_errors();
            sheet.set_busy(&label, busy);
        }
    }

    /// Fill IMAP/SMTP hosts + ports from the typed email when focus leaves the
    /// email field (and again on save). Consults the network Mozilla autoconfig
    /// lookup for unknown domains under `live`; the built-in provider table
    /// covers the common providers instantly and offline.
    fn maybe_autofill(&mut self) {
        self.apply_autofill(true);
    }

    /// The instant, network-free fill applied as the address is typed: it uses
    /// only the built-in provider table (see [`crate::autoconfig::builtin_autoconfig`]),
    /// so `me@gmail.com` pre-fills the Gmail servers the moment it's complete —
    /// without waiting for a focus change or opening a socket. Unknown domains
    /// are left for the focus-leave/save pass to discover over the network.
    fn maybe_autofill_live_typing(&mut self) {
        self.apply_autofill(false);
    }

    /// Fill IMAP/SMTP host/port/TLS from the typed email when the host fields
    /// are still blank, and detect the provider so Gmail/Outlook preset to
    /// OAuth. Resolution order: the built-in provider table first (instant,
    /// offline), then — only when `network` and nothing matched — the real
    /// Mozilla autoconfig lookup (`live`), then the generic heuristic.
    fn apply_autofill(&mut self, network: bool) {
        let email = self.setup_value(1).trim().to_string();
        if self
            .setup_discovery
            .as_ref()
            .is_some_and(|pending| pending.config.email.trim() != email)
        {
            self.setup_discovery = None;
        }
        if !email.contains('@') {
            self.set_provider_status(crate::setup_sheet::ProviderStatus::Idle);
            return;
        }
        let domain = email
            .split_once('@')
            .map(|(_, d)| d.to_string())
            .unwrap_or_default();

        // Provider detection → OAuth for the providers that block passwords.
        let provider = crate::oauth::OAuthProvider::from_domain(&domain);
        if let Some(fields) = self.setup_fields() {
            fields.set_provider(provider);
        }
        if provider.is_some() {
            self.set_setup_auth(crate::model::AuthMethod::OAuth);
        }

        let imap_blank = self.setup_value(4).trim().is_empty();
        let smtp_blank = self.setup_value(6).trim().is_empty();
        if !imap_blank && !smtp_blank {
            self.setup_discovery = None;
            self.dirty = true;
            return;
        }

        let mut cfg = AccountConfig {
            email: email.clone(),
            username: self.setup_value(2),
            display_name: self.setup_value(0),
            auth: self.setup_auth(),
            ..Default::default()
        };
        // Built-in table first (instant, offline). Network discovery only for
        // domains it doesn't cover, and only on the focus-leave/save pass.
        if let Some(ac) = crate::autoconfig::builtin_autoconfig(&domain) {
            ac.apply_to(&mut cfg);
            self.set_provider_status(found_status(&domain, &ac));
        } else if network {
            self.start_setup_discovery();
            return;
        } else {
            // Unknown domains need discovery when focus leaves the address.
            // A premature guessed host would suppress that lookup.
            return;
        }
        // The provider table fills only the connection fields; default the
        // identity fields from the address (username = the email, display name =
        // its local part) so a table-filled account is immediately savable.
        if cfg.username.trim().is_empty() {
            cfg.username.clone_from(&email);
        }
        if cfg.display_name.trim().is_empty()
            && let Some((local, _)) = email.split_once('@')
        {
            cfg.display_name = local.to_string();
        }

        if imap_blank {
            self.set_setup_input(4, &cfg.imap_host);
            self.set_setup_input(5, &cfg.imap_port.to_string());
            self.set_setup_tls(crate::setup_sheet::Server::Incoming, cfg.imap_tls);
        }
        if smtp_blank {
            self.set_setup_input(6, &cfg.smtp_host);
            self.set_setup_input(7, &cfg.smtp_port.to_string());
            self.set_setup_tls(crate::setup_sheet::Server::Outgoing, cfg.smtp_tls);
        }
        if self.setup_value(2).trim().is_empty() {
            self.set_setup_input(2, &cfg.username);
        }
        if self.setup_value(0).trim().is_empty() {
            self.set_setup_input(0, &cfg.display_name);
        }
        // Autoconfig may have upgraded the auth method (OAuth2 in the XML).
        self.set_setup_auth(cfg.auth);
        self.dirty = true;
    }

    fn start_setup_discovery(&mut self) {
        if self.setup_discovery.is_some() {
            return;
        }
        let config = self.read_setup_config();
        let email = config.email.trim().to_string();
        let (sender, receiver) = std::sync::mpsc::channel();
        let cancellation = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation);
        if let Err(error) = std::thread::Builder::new()
            .name("mail-provider-discovery".into())
            .spawn(move || {
                let result = autoconfig_lookup(&email, &worker_cancellation);
                let _ = sender.send(result);
            })
        {
            self.set_setup_error(Some(format!("Couldn’t start provider discovery: {error}")));
            self.dirty = true;
            return;
        }
        self.setup_discovery = Some(PendingSetupDiscovery {
            config,
            editing: self.setup_editing,
            receiver,
            cancellation,
        });
        self.set_provider_status(crate::setup_sheet::ProviderStatus::Finding);
        self.dirty = true;
    }

    fn set_provider_status(&self, status: crate::setup_sheet::ProviderStatus) {
        if let Some(fields) = self.setup_fields() {
            fields.set_provider_status(status);
        }
    }

    fn drain_setup_discovery(&mut self) {
        let Some(pending) = self.setup_discovery.as_ref() else {
            return;
        };
        let result = match pending.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.setup_discovery = None;
                self.set_provider_status(crate::setup_sheet::ProviderStatus::Idle);
                self.set_setup_error(Some("Couldn’t find the mail servers".into()));
                self.dirty = true;
                return;
            }
        };
        let Some(pending) = self.setup_discovery.take() else {
            return;
        };
        self.dirty = true;
        let current = self.read_setup_config();
        if !self.setup_open()
            || self.setup_editing != pending.editing
            || current.email != pending.config.email
        {
            return;
        }
        let mut discovered = pending.config.clone();
        if let Some(result) = result {
            let domain = current
                .email
                .split_once('@')
                .map_or("", |(_, domain)| domain);
            self.set_provider_status(found_status(domain, &result));
            result.apply_to(&mut discovered);
        } else {
            self.set_provider_status(crate::setup_sheet::ProviderStatus::Idle);
            discovered.autofill_from_email();
        }
        // Preserve manual edits made while discovery was in flight. Each
        // server's host/port/TLS form one setting and are applied together.
        if current.imap_host.trim().is_empty()
            && current.imap_port == pending.config.imap_port
            && current.imap_tls == pending.config.imap_tls
        {
            self.set_setup_input(4, &discovered.imap_host);
            self.set_setup_input(5, &discovered.imap_port.to_string());
            self.set_setup_tls(crate::setup_sheet::Server::Incoming, discovered.imap_tls);
        }
        if current.smtp_host.trim().is_empty()
            && current.smtp_port == pending.config.smtp_port
            && current.smtp_tls == pending.config.smtp_tls
        {
            self.set_setup_input(6, &discovered.smtp_host);
            self.set_setup_input(7, &discovered.smtp_port.to_string());
            self.set_setup_tls(crate::setup_sheet::Server::Outgoing, discovered.smtp_tls);
        }
        if current.username.trim().is_empty() {
            self.set_setup_input(2, &current.email);
        }
        if current.display_name.trim().is_empty() {
            let name = if discovered.display_name.is_empty() {
                current.email.split_once('@').map_or("", |(local, _)| local)
            } else {
                &discovered.display_name
            };
            self.set_setup_input(0, name);
        }
        if current.auth == pending.config.auth {
            self.set_setup_auth(discovered.auth);
        }
        self.dirty = true;
    }

    fn save_setup(&mut self, _doc: &mut ui::widget::Edits<'_>) {
        if self.setup_validation.is_some() {
            return;
        }
        self.maybe_autofill();
        if self.setup_discovery.is_some() {
            self.dirty = true;
            return;
        }
        let config = self.read_setup_config();
        if let Some((_, message)) = config.validate().into_iter().next() {
            self.finish_setup_validation(
                config,
                &crate::validate::ValidationState::Invalid(message),
            );
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let cancellation = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation);
        let worker_config = config.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("mail-account-validation".into())
            .spawn(move || {
                let outcome = crate::validate::validate_account_cancellable(
                    &worker_config,
                    &worker_cancellation,
                );
                let _ = sender.send(outcome);
            })
        {
            self.set_setup_error(Some(format!(
                "Couldn’t start account verification: {error}"
            )));
            self.dirty = true;
            return;
        }
        self.setup_validation = Some(PendingSetupValidation {
            config,
            editing: self.setup_editing,
            receiver,
            cancellation,
        });
        self.set_setup_error(None);
        self.dirty = true;
    }

    fn drain_setup_validation(&mut self) {
        let Some(pending) = self.setup_validation.as_ref() else {
            return;
        };
        let outcome = match pending.receiver.try_recv() {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => crate::validate::ValidationState::Failed(
                "Account verification ended without a result".into(),
            ),
        };
        let Some(pending) = self.setup_validation.take() else {
            return;
        };
        if !self.setup_open() {
            return;
        }
        if self.setup_editing != pending.editing || self.read_setup_config() != pending.config {
            self.set_setup_error(Some("Settings changed".into()));
            self.dirty = true;
            return;
        }
        self.finish_setup_validation(pending.config.clone(), &outcome);
    }

    /// Save the account, but only after its credentials pass a live IMAP login
    /// and SMTP handshake through `probe`. On failure the sheet stays open with
    /// a specific error; on success the account is persisted and connected. The
    /// `probe` seam makes the persist-on-success / keep-on-failure behaviour
    /// unit-testable without a network.
    #[cfg(test)]
    fn save_setup_with(
        &mut self,
        _doc: &mut ui::widget::Edits<'_>,
        probe: &dyn crate::validate::ProbeTransport,
    ) {
        self.maybe_autofill();
        let cfg = self.read_setup_config();
        let outcome = crate::validate::validate_account(&cfg, probe);
        self.finish_setup_validation(cfg, &outcome);
    }

    fn finish_setup_validation(
        &mut self,
        cfg: AccountConfig,
        outcome: &crate::validate::ValidationState,
    ) {
        if !outcome.persisted() {
            if matches!(outcome, crate::validate::ValidationState::Invalid(_))
                && let Some(sheet) = self.setup.as_mut()
            {
                sheet.show_field_errors();
            }
            let msg = outcome.message();
            self.set_setup_error(Some(msg.clone()));
            self.shared.borrow_mut().status = Some(msg);
            self.dirty = true;
            return;
        }
        self.set_setup_error(None);
        {
            let mut st = self.shared.borrow_mut();
            let mut account = match self.setup_editing.and_then(|i| st.accounts.accounts.get(i)) {
                Some(existing) => {
                    let mut a = existing.clone();
                    a.config = cfg.clone();
                    a
                }
                None => Account::new(cfg.clone()),
            };
            account.id = if cfg.email.is_empty() {
                account.id
            } else {
                cfg.email.clone()
            };
            let mut updated = st.accounts.clone();
            updated.upsert(account);
            // Keep it in the person's accounts, on all of their devices, and
            // its password in this device's vault; sample / shots / test
            // builds keep it in a store of their own.
            if let Err(error) = self.persist_accounts(&updated) {
                let message = format!("Couldn’t save account: {error}");
                if let Some(fields) = self.setup_fields() {
                    fields.set_error(Some(message.clone()));
                }
                st.status = Some(message);
                self.dirty = true;
                return;
            }
            st.accounts = updated;
            st.status = Some("Account saved".into());
            self.persisted_active_account = st
                .accounts
                .active_account()
                .map(|account| account.id.clone());
        }
        // Connect + fetch the freshly-saved account through the engine.
        self.post_engine(EngineRequest::connect(cfg.clone()));
        self.post_engine(EngineRequest::fetch(cfg, Folder::Inbox));
        self.close_setup();
    }

    /// Remove the account currently being edited from the person's accounts
    /// (dropping its secret from this device's vault), and close the sheet.
    /// A no-op on the add flow (no editing target).
    fn remove_editing_account(&mut self, _doc: &mut ui::widget::Edits<'_>) {
        let Some(idx) = self.setup_editing else {
            return;
        };
        {
            let mut st = self.shared.borrow_mut();
            let Some(id) = st.accounts.accounts.get(idx).map(|a| a.id.clone()) else {
                return;
            };
            let mut updated = st.accounts.clone();
            updated.remove(&id);
            if let Err(error) = self.persist_accounts(&updated) {
                let message = format!("Couldn’t remove account: {error}");
                if let Some(fields) = self.setup_fields() {
                    fields.set_error(Some(message.clone()));
                }
                st.status = Some(message);
                self.dirty = true;
                return;
            }
            st.accounts = updated;
            st.status = Some("Account removed".into());
            // The removed account's mailbox is no longer relevant; clear it so a
            // stale inbox doesn't linger. A fresh connect repopulates from the
            // new active account.
            st.mailbox = crate::model::Mailbox::new(st.accounts.active_config());
            st.selected = None;
            self.persisted_active_account = st
                .accounts
                .active_account()
                .map(|account| account.id.clone());
        }
        self.connect_requested = false;
        self.close_setup();
        self.dirty = true;
    }

    /// Begin the OAuth authorization-code + PKCE flow for the detected
    /// provider. The engine worker runs the real flow (bind the loopback
    /// redirect, open the browser, capture the code, exchange it, seal the
    /// tokens in the vault); this posts the request and shows progress. Surfaces
    /// a clear note when a registered client id has not been provisioned (the
    /// flow can't complete without one).
    fn start_oauth(&mut self) {
        let email = self.setup_value(1).trim().to_string();
        let Some(provider) = self.setup_provider() else {
            self.shared.borrow_mut().status =
                Some("Enter a Gmail or Outlook address to sign in".into());
            self.dirty = true;
            return;
        };
        if email.is_empty() {
            self.shared.borrow_mut().status = Some("Enter your email address first".into());
            self.dirty = true;
            return;
        }
        if !provider.is_configured() {
            self.set_setup_error(Some(format!(
                "{} sign-in isn’t available here yet. Choose Password to continue with an app password.",
                provider.label()
            )));
            self.dirty = true;
            return;
        }
        match self.engine.as_ref() {
            Some(_) => {
                if let Some(fields) = self.setup_fields() {
                    fields.expect_oauth();
                }
                self.post_engine(EngineRequest::oauth_authorize(
                    email.clone(),
                    provider,
                    email,
                ));
                self.set_setup_error(None);
                self.shared.borrow_mut().status = Some(format!(
                    "Opening {} sign-in in your browser…",
                    provider.label()
                ));
            }
            None => {
                self.shared.borrow_mut().status =
                    Some("Sign-in isn't available in this mode".into());
            }
        }
        self.dirty = true;
    }

    /// The label of the sheet's primary action, which says what is
    /// happening while the account is checked.
    fn setup_save_label(&self) -> String {
        crate::i18n::t(if self.setup_validation.is_some() {
            "mail-setup-verifying"
        } else if self.setup_discovery.is_some() {
            "mail-setup-finding-provider"
        } else {
            "mail-setup-save"
        })
    }

    // ---- sync / refresh ---------------------------------------------------

    /// Post a fetch of the currently-viewed folder for the active account, and
    /// show a syncing indicator. Used by the manual-refresh shortcut and the
    /// periodic auto-refresh. A no-op with no account or no engine.
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
                &account_id,
                accounts::AccountSyncHealth::Syncing,
                false,
                Some("Checking mail"),
            );
            self.last_auto_refresh_min = self.shared.borrow().now_min;
            self.shared.borrow_mut().status = Some("Syncing…".into());
            self.dirty = true;
            return Some(request_id);
        }
        None
    }

    /// Fire a periodic refresh if enough wall-clock time has elapsed since the
    /// last one and no request is already outstanding. Checked once per active
    /// frame in `sync` (so it fires during use); a fully-idle session refreshes
    /// on the next interaction rather than waking the CPU on a timer.
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
            let _ = self.refresh_active_folder();
        }
    }

    // ---- transient status toast ------------------------------------------

    /// Hand a newly-set status to the shell, which shows it as a toast and
    /// lets it fade on its own.
    fn show_status(&mut self) {
        let Some(msg) = self.shared.borrow_mut().status.take() else {
            return;
        };
        let kind = Self::status_kind(&msg);
        self.nav.toast(msg, kind);
    }

    fn status_kind(msg: &str) -> ToastKind {
        let lower = msg.to_ascii_lowercase();
        if lower.contains("failed")
            || lower.contains("couldn’t")
            || lower.contains("couldn't")
            || lower.contains("error")
        {
            ToastKind::Error
        } else if lower.contains("offline") || lower.contains("isn't available") {
            ToastKind::Warning
        } else if lower.contains("sync")
            || lower.contains("connecting")
            || lower.contains("opening")
        {
            ToastKind::Info
        } else {
            ToastKind::Success
        }
    }

    // ---- engine drain -----------------------------------------------------

    fn apply_fetched_messages(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        st: &mut MailState,
        request_id: EngineRequestId,
        account_id: &str,
        folder: Folder,
        messages: Vec<Message>,
    ) {
        let count = messages.len();
        st.ingest_fetched(doc, account_id, &messages);
        // The server answered: what it refused before may go through now.
        for projection in [&self.projection, &self.delivery].into_iter().flatten() {
            projection.outside_changed();
        }
        for message in messages {
            st.mailbox.insert(message);
        }
        if st.folder == folder && st.selected.is_none() {
            st.selected = st
                .current_threads()
                .first()
                .and_then(|thread| thread.message_ids.last().copied());
        }
        st.status = Some(if count == 0 {
            format!("{} up to date", folder.title())
        } else {
            format!("Synced {count} messages")
        });
        report_account_sync(
            account_id,
            accounts::AccountSyncHealth::Healthy,
            true,
            Some("Mail is up to date"),
        );
        if self
            .resource_refresh_invocation
            .is_some_and(|(pending, _)| pending == request_id)
            && let Some((_, invocation)) = self.resource_refresh_invocation
        {
            self.resource_refresh_result = Some(tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            });
        }
    }

    fn apply_sent_message(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        st: &mut MailState,
        request_id: EngineRequestId,
    ) {
        let Some(pending) = self.pending_send.remove(&request_id) else {
            return;
        };
        // A message joins Sent only after SMTP has accepted its bytes.
        st.ingest_messages(doc, std::slice::from_ref(&pending.message));
        st.mailbox.insert(pending.message);
        (pending.done)(Ok(ui::tilo_resource::Settled::Done(())));
        st.status = Some("Delivered".into());
        self.editing_draft = None;
        self.compose.close();
    }

    fn apply_engine_error(
        &mut self,
        st: &mut MailState,
        request_id: EngineRequestId,
        context: &str,
        message: &str,
    ) {
        let detail = format!("{context}: {message}");
        let refused = self.server_outbox.borrow_mut().sent.remove(&request_id);
        if let Some((_, done)) = refused {
            done(Err(detail.clone()));
        }
        st.status = Some(detail.clone());
        let account_id = st.accounts.active_config().email;
        let lower = detail.to_ascii_lowercase();
        let health = if ["auth", "login", "password", "oauth", "credential"]
            .iter()
            .any(|needle| lower.contains(needle))
        {
            accounts::AccountSyncHealth::NeedsAuthentication
        } else {
            accounts::AccountSyncHealth::Error
        };
        report_account_sync(&account_id, health, false, Some(&detail));
        if let Some(pending) = self.pending_send.remove(&request_id) {
            // The server refused it: it stays in the outbox with the reason,
            // and comes back to the composer to be fixed and sent again.
            if !self.compose.open {
                self.compose.open_with_draft(&pending.outgoing.to_draft());
                self.resending = Some(pending.outgoing.id.clone());
            }
            (pending.done)(Ok(ui::tilo_resource::Settled::failed(detail.clone())));
        }
        if self
            .resource_refresh_invocation
            .is_some_and(|(pending, _)| pending == request_id)
            && let Some((_, invocation)) = self.resource_refresh_invocation
        {
            self.resource_refresh_result = Some(tilo_actions::InvocationUpdate::Failed {
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
                st.status = Some(format!("Connected {account_id}"));
                report_account_sync(
                    &account_id,
                    accounts::AccountSyncHealth::Syncing,
                    false,
                    Some("Connected; fetching mail"),
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
                messages,
            } if self.expects_engine_event(request_id, EngineRequestKind::Fetch) => {
                self.apply_fetched_messages(doc, st, request_id, &account_id, folder, messages);
            }
            EngineEvent::Sent { request_id, .. }
                if self.expects_engine_event(request_id, EngineRequestKind::Send) =>
            {
                self.apply_sent_message(doc, st, request_id);
            }
            EngineEvent::Authorized {
                request_id,
                account_id,
            } if self.expects_engine_event(request_id, EngineRequestKind::OAuthAuthorize) => {
                if let Some(fields) = self.setup_fields() {
                    fields.complete_oauth(&account_id);
                }
                st.status = Some(format!("Signed in as {account_id}"));
                report_account_sync(
                    &account_id,
                    accounts::AccountSyncHealth::Healthy,
                    true,
                    Some("Signed in"),
                );
            }
            EngineEvent::Error {
                request_id,
                kind,
                context,
                message,
            } if self.expects_engine_event(request_id, kind) => {
                self.apply_engine_error(st, request_id, &context, &message);
            }
            EngineEvent::Completed { request_id, kind }
                if self.expects_engine_event(request_id, kind) =>
            {
                // Only the worker acknowledgement settles a queued request:
                // connect has two payloads and successful writes have none.
                self.pending_engine_requests.remove(&request_id);
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

    /// Hand the server updates the projection asked for to the engine, each
    /// on the account it belongs to. One this device cannot carry out (no
    /// such account here, no engine) is refused; another device with the
    /// account carries it out.
    fn flush_server_outbox(&mut self) {
        let deliveries = std::mem::take(&mut self.server_outbox.borrow_mut().deliveries);
        for (outgoing, done) in deliveries {
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
            };
            match self.post_engine(request) {
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

    /// Hand an outbox message to the engine on the account it is sent from.
    /// One whose account this device does not hold is left for a device
    /// that does.
    fn deliver(&mut self, outgoing: Outgoing, done: DeliveryDone) {
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
        let draft = outgoing.to_draft();
        let message = Message {
            id: outgoing.id.parse().unwrap_or_else(|_| uuid::Uuid::new_v4()),
            rfc_message_id: None,
            in_reply_to: None,
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
        };
        match self.post_engine(EngineRequest::send(account, Box::new(draft))) {
            Some(request_id) => {
                self.pending_send.insert(
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

    /// The outbox the projection drain fills.
    pub(crate) fn server_outbox(&self) -> Rc<RefCell<ServerOutbox>> {
        Rc::clone(&self.server_outbox)
    }

    /// Keep the drain that fills the outbox.
    pub(crate) fn follow_projection(&mut self, projection: ui::framework::Projection) {
        self.projection = Some(projection);
    }

    /// Keep the drain that hands the document's outbox to the engine.
    pub(crate) fn follow_delivery(&mut self, delivery: ui::framework::Projection) {
        self.delivery = Some(delivery);
    }

    fn drain_engine(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(handle) = &self.engine else { return };
        let mut events = Vec::new();
        while let Ok(event) = handle.rx.try_recv() {
            events.push(event);
        }
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
        self.awaiting_engine = !self.pending_engine_requests.is_empty();
    }

    /// Register the locally hydrated corpus before actions target it. This is
    /// external ingestion, not a user mutation: it never writes flags from a
    /// presentation cache into the collaborative document.
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
    /// This runs after every input path (pane pointer/keyboard, accessibility,
    /// or canonical resource operation) has selected an account. It never
    /// touches the Automerge document.
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
        match self.persist_accounts(&accounts) {
            Ok(()) => self.persisted_active_account = active,
            Err(error) => {
                self.shared.borrow_mut().status = Some(format!(
                    "Selected account, but could not save that preference: {error}"
                ));
            }
        }
    }

    /// Follow the person's accounts in this window. What this device's file
    /// held before it followed the record is taken in first, and the file
    /// then follows the record.
    pub(crate) fn follow_person_accounts(&mut self, accounts: PersonRecord<PersonAccounts>) {
        if let Some(root) = self.accounts_persist.root() {
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

    /// Keep `updated` as the person's mail accounts: through their accounts
    /// record, which follows them to every device, with each password in
    /// this device's vault and the device's account file following the
    /// record. Outside a live window, in the store this window keeps.
    fn persist_accounts(&self, updated: &AccountStore) -> std::io::Result<()> {
        let Some((record, _)) = &self.person_accounts else {
            return self.accounts_persist.persist(updated);
        };
        let (edits, removed) = updated.edits_for(&record.get());
        for edit in &edits {
            record
                .invoke(edit)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
        }
        if let Some(root) = self.accounts_persist.root() {
            updated.keep_secrets_in(&root, &removed)?;
            record.get().project_in(&root)?;
        }
        Ok(())
    }

    /// Show the person's accounts when their record moved: each account's
    /// settings from the record, its password from this device's vault. A
    /// newly shown active account is connected.
    fn adopt_person_accounts(&mut self) {
        let Some((record, seen)) = &mut self.person_accounts else {
            return;
        };
        let revision = record.revision();
        if *seen == Some(revision) {
            return;
        }
        *seen = Some(revision);
        let person = record.get();
        let vault = self
            .accounts_persist
            .root()
            .map_or_else(accounts::AccountCredentials::open, |root| {
                accounts::AccountCredentials::open_in(&root)
            });
        let Ok(vault) = vault else {
            tracing::error!("mail account credentials could not be opened");
            *seen = None;
            return;
        };
        let mut adopted = AccountStore::from_store(&person.store(), &vault);
        let changed = {
            let mut st = self.shared.borrow_mut();
            if let Some(shown) = st
                .accounts
                .active_account()
                .map(|account| account.id.clone())
                && let Some(index) = adopted.accounts.iter().position(|a| a.id == shown)
            {
                adopted.active = index;
            }
            let changed = st.accounts != adopted;
            st.accounts = adopted;
            changed
        };
        if changed && self.connect_requested && self.engine.is_some() {
            let cfg = self.shared.borrow().accounts.active_config();
            self.post_engine(EngineRequest::connect(cfg.clone()));
            self.post_engine(EngineRequest::fetch(cfg, Folder::Inbox));
        }
        self.dirty = true;
    }

    /// Follow the person's Mail preferences in this window.
    pub(crate) fn follow_preferences(
        &mut self,
        preferences: ui::framework::Setting<crate::resource::MailPreferences>,
    ) {
        let current = preferences.get();
        self.preferences = Some(preferences);
        self.adopt_preferences(current);
    }

    /// Show `preferences`: the format a new composition starts in (an open
    /// composer keeps its own) and which account sidebars are folded.
    fn adopt_preferences(&mut self, preferences: crate::resource::MailPreferences) {
        if !self.compose.open {
            self.compose.set_format(preferences.compose_format);
        }
        for account in &mut self.shared.borrow_mut().accounts.accounts {
            account.collapsed = preferences.collapsed_accounts.contains(&account.id);
        }
        self.shared.borrow_mut().remote_senders =
            preferences.remote_content_senders.iter().cloned().collect();
        self.followed_preferences = preferences;
        self.dirty = true;
    }

    /// The preferences as this window shows them now.
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
            remote_content_senders: self
                .shared
                .borrow()
                .remote_senders
                .iter()
                .cloned()
                .collect(),
        }
    }

    /// Adopt what another window or device stored, then store what changed
    /// here: a format switched, an account folded.
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

    /// Store the preferences this window shows when they changed. Called at
    /// every format-changing or composer lifecycle boundary and every sync.
    fn store_preferences_if_changed(&mut self) {
        let Some(preferences) = &self.preferences else {
            return;
        };
        let shown = self.shown_preferences();
        if shown == self.followed_preferences {
            return;
        }
        match preferences.set(shown.clone()) {
            Ok(()) => self.followed_preferences = shown,
            Err(error) => {
                self.shared.borrow_mut().status =
                    Some(format!("Could not save your Mail preferences: {error}"));
            }
        }
    }

    /// Apply collaborative domain state to the local hydrated corpus. Folder,
    /// selection, search, and composer format remain local to this window.
    fn hydrate_domain_state(&self, doc: &mut ui::widget::Edits<'_>) {
        let mail = read_mail(doc.doc()).unwrap_or_default();
        {
            let mut st = self.shared.borrow_mut();
            mail.apply_flags(&mut st.mailbox);
        }
        // A persisted draft becomes a local Drafts projection, but does not
        // force another collaborator's composer open.
        let draft = mail.draft.draft();
        if !draft.is_empty() {
            self.upsert_draft_message(doc, &draft);
        } else if let Some((id, _)) = self.projected_draft.borrow_mut().take() {
            self.shared.borrow_mut().mailbox.purge(id);
        }
    }

    /// Materialise one message resource row on demand. The public resource
    /// tree deliberately carries only a bounded current-folder prefix, but a
    /// stable message id must still resolve for agents and search activation.
    pub(crate) fn resource_message_row(&self, id: &str) -> Option<crate::actions::MessageRow> {
        self.shared
            .borrow()
            .mailbox
            .messages
            .values()
            .find(|message| message.id.to_string() == id)
            .map(crate::actions::message_row)
    }

    /// Lazily materialise a bounded thread resource by its stable key. This
    /// keeps every loaded thread addressable even when the open-resource tree
    /// only carries the first `OPEN_MESSAGE_LIMIT` messages.
    pub(crate) fn resource_thread_rows(
        &self,
        stable: &str,
    ) -> Option<(String, Vec<crate::actions::MessageRow>)> {
        let (folder_raw, anchor_raw) = stable.split_once(':')?;
        let folder = Folder::from_key(folder_raw)?;
        if anchor_raw.is_empty() {
            return None;
        }
        let state = self.shared.borrow();
        let thread = state
            .mailbox
            .threads_in(folder)
            .into_iter()
            .find(|thread| thread.id == anchor_raw)?;
        let subject = thread.subject;
        Some((
            subject,
            thread
                .message_ids
                .into_iter()
                .filter_map(|id| state.mailbox.get(id))
                .take(crate::resource::OPEN_MESSAGE_LIMIT)
                .map(crate::actions::message_row)
                .collect(),
        ))
    }

    // ---- test / probe accessors ------------------------------------------

    /// The column in front of a narrow window (for tests / probes).
    pub fn nav_pane(&self) -> NavPane {
        self.nav.front_pane()
    }

    /// Lay the app out at `w`×`h` as the host does (twice, so the shell
    /// settles for the size), keeping the tree for [`Self::test_layout`].
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
            // A test reads the settled shell, not a frame of its motion.
            self.nav.snap();
        }
        // A frame paints what was laid out, so the columns know their rows.
        let theme = self.layout_theme.clone();
        let mut text = TextRenderer::new();
        text.set_text_scale(theme.text_scale);
        self.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::empty(),
        );
    }

    /// The directory account changes persist into: the real per-user config
    /// root for the live app, or a throwaway sandbox for the sample / shots /
    /// test constructors. `None` when persistence is disabled. Tests use this to
    /// prove sample mode never resolves the real store path.
    #[cfg(test)]
    fn accounts_persist_root(&self) -> Option<std::path::PathBuf> {
        self.accounts_persist.root()
    }
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
/// What `message` says, for a caller with no screen: its people, subject,
/// time, text and attachments' names.
fn message_read(message: &Message) -> serde_json::Value {
    let person = |addr: &crate::model::Addr| match &addr.name {
        Some(name) if !name.trim().is_empty() => format!("{name} <{}>", addr.email),
        _ => addr.email.clone(),
    };
    serde_json::json!({
        "from": person(&message.from),
        "to": message.to.iter().map(person).collect::<Vec<_>>(),
        "cc": message.cc.iter().map(person).collect::<Vec<_>>(),
        "subject": message.subject,
        "sent_minutes_since_epoch": message.timestamp_min,
        "text": message.body,
        "attachments": message.attachments.iter().map(|attachment| serde_json::json!({
            "name": attachment.filename,
            "type": attachment.mime,
            "size": attachment.size,
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
        subject: if m.subject == "(no subject)" {
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
    }
}

fn report_account_sync(
    account_id: &str,
    health: accounts::AccountSyncHealth,
    _success: bool,
    _message: Option<&str>,
) {
    if account_id.trim().is_empty() {
        return;
    }
    let reporter = accounts::AccountSyncReporter::new(account_id.to_string());
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

/// The shared image store used for sender avatars in the **live** app: fetches
/// real Gravatar images over HTTP when built with `live` (the default).
pub fn avatar_store() -> ImageStore {
    ImageStore::new(&avatar_fetcher())
}

/// The avatar store for the hermetic paths — `--shots`, the sample corpus, and
/// tests. It is wired to a no-op fetcher **regardless of the `live` feature**,
/// so a headless render never opens a socket to gravatar.com even though the
/// live fetcher is now compiled in by default; avatars render as initials. This
/// is the runtime half of "features compile the live code in; the runtime mode
/// decides live vs sample".
pub fn avatar_store_offline() -> ImageStore {
    ImageStore::new(&noop_avatar_fetcher())
}

/// A fetcher that never touches the network — the hermetic default.
fn noop_avatar_fetcher() -> ui::Fetcher {
    std::sync::Arc::new(|_url: &str| None)
}

/// Real Gravatar HTTP under `--features live` (default); a no-op otherwise so a
/// build without `live` never opens a socket (avatars then render as initials).
#[cfg(feature = "live")]
fn avatar_fetcher() -> ui::Fetcher {
    use std::sync::{Arc, OnceLock};
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    Arc::new(|url: &str| {
        let client = CLIENT.get_or_init(|| {
            tilo_net::http::blocking_client()
                .timeout(std::time::Duration::from_secs(8))
                .user_agent("Tilo Mail")
                .build()
                .unwrap_or_default()
        });
        let resp = client.get(url).send().ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let bytes = resp.bytes().ok()?;
        if bytes.is_empty() || bytes.len() > 2 * 1024 * 1024 {
            return None;
        }
        Some(bytes.to_vec())
    })
}

#[cfg(not(feature = "live"))]
fn avatar_fetcher() -> ui::Fetcher {
    noop_avatar_fetcher()
}

/// What the sheet shows for a provider found for `domain`: its name (the
/// provider's own, else the domain) and the incoming server.
fn found_status(
    domain: &str,
    found: &crate::autoconfig::Autoconfig,
) -> crate::setup_sheet::ProviderStatus {
    let name = crate::autoconfig::known_provider_for_domain(domain)
        .map(crate::autoconfig::KnownProvider::name)
        .map(str::to_owned)
        .or_else(|| found.display_name.clone())
        .unwrap_or_else(|| domain.to_owned());
    crate::setup_sheet::ProviderStatus::Found { name }
}

/// Mozilla autoconfig discovery for a typed email. Real HTTP under
/// `--features live`; `None` offline (the caller falls back to the heuristic).
#[cfg(feature = "live")]
fn autoconfig_lookup(
    email: &str,
    cancellation: &std::sync::atomic::AtomicBool,
) -> Option<crate::autoconfig::Autoconfig> {
    crate::autoconfig::discover_cancellable(email, cancellation)
}
#[cfg(not(feature = "live"))]
fn autoconfig_lookup(
    _email: &str,
    _cancellation: &std::sync::atomic::AtomicBool,
) -> Option<crate::autoconfig::Autoconfig> {
    None
}

// =============================== Widget impl ===============================

impl MailApp {
    /// The loaded mailbox, which search reads its messages from.
    pub(crate) fn shared_mailbox(&self) -> std::cell::Ref<'_, crate::model::Mailbox> {
        std::cell::Ref::map(self.shared.borrow(), |state| &state.mailbox)
    }

    /// Route events to the open compose modal and report whether it consumed
    /// them.
    fn handle_compose_modal(&mut self, event: &WindowEvent, ctx: &mut EventContext<'_>) -> bool {
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
            ComposeAction::Close => {
                self.dirty = true;
            }
            ComposeAction::None => {}
        }
        if lifecycle_boundary || self.compose.format() != format_before {
            self.store_preferences_if_changed();
        }
        consumed
    }

    fn confirm_bulk_purge(&mut self, state: &mut ui::widget::Edits<'_>) {
        let affected = self
            .shared
            .borrow_mut()
            .apply_bulk_action(state, BulkAction::Purge);
        self.bulk_purge_confirmation = None;
        self.shared.borrow_mut().status = Some(if affected == 0 {
            "Nothing was deleted; the selected messages changed before confirmation".into()
        } else {
            format!(
                "Permanently deleted {affected} trashed message{}",
                if affected == 1 { "" } else { "s" }
            )
        });
        self.process_intents();
        self.dirty = true;
    }

    /// The row menu owns input while open: its accelerators act on the
    /// message, everything else is the menu's (arrows, Enter, Escape, a click
    /// outside closes it).
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
        // While open the menu is modal; a press outside it only closes it.
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

    /// The permanent-deletion confirmation owns all input while open.
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

    /// Route app-owned modal surfaces in front-to-back order. Returning true
    /// stops propagation to the navigation shell and global shortcuts.
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
                self.open_setup_edit(None);
            }
            self.dirty = true;
            return true;
        }

        self.handle_setup_event(event, ctx)
    }

    /// The account sheet owns all input while open. It is laid out in the
    /// window's tree, so its fields take the window's focus and Tab.
    fn handle_setup_event(&mut self, event: &WindowEvent, ctx: &mut EventContext<'_>) -> bool {
        let Some(sheet) = self.setup.as_mut() else {
            return false;
        };
        if let Some(node) = self.setup_node {
            sheet.dialog.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    scale_factor: ctx.scale_factor,
                    modifiers: &ctx.modifiers,
                    state: ctx.state.reborrow(),
                    path: ctx.path,
                    handled: ctx.handled,
                    focus_target: ctx.focus_target,
                    node_id: node,
                    taffy: ctx.taffy,
                },
            );
        }
        self.settle_setup(&mut ctx.state);
        *ctx.handled = true;
        self.dirty = true;
        true
    }

    fn overview_accessibility_node(state: &MailState) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        let rows = state.list_rows();
        let total = rows.len();
        let messages = rows
            .iter()
            .enumerate()
            .filter_map(|(index, (id, thread_len))| {
                let message = state.mailbox.get(*id)?;
                let title = if message.folder == Folder::Drafts {
                    format!("Draft: {}", message.subject)
                } else {
                    format!("{}: {}", message.from.display(), message.subject)
                };
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
                    node = node
                        .with_description(format!("{thread_len} messages in this conversation"));
                }
                Some(node)
            })
            .collect::<Vec<_>>();
        let name = format!("{} messages", state.folder.title());
        let mut children = vec![
            AccessNode::new(Role::Button)
                .with_name(if state.bulk_selecting {
                    "Done selecting messages"
                } else {
                    "Select messages for bulk actions"
                })
                .with_action_target("mail.bulk:toggle")
                .with_state(AccessState::new().focusable(true)),
        ];
        if state.bulk_selecting {
            let selected = state.selected_bulk_ids().len();
            let enabled = selected > 0;
            let all_trash = enabled
                && state.selected_bulk_ids().iter().all(|id| {
                    state
                        .mailbox
                        .get(*id)
                        .is_some_and(|message| message.folder == Folder::Trash)
                });
            let mut controls = vec![
                ("Select all visible messages", "select-all"),
                ("Archive selected messages", "archive"),
                ("Mark selected messages read", "read"),
                ("Mark selected messages unread", "unread"),
            ];
            controls.push(if all_trash {
                ("Delete selected trashed messages permanently", "purge")
            } else {
                ("Move selected messages to Trash", "delete")
            });
            children.push(
                AccessNode::new(Role::Toolbar)
                    .with_name(format!("{selected} selected"))
                    .with_children(
                        controls
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
                    ),
            );
        }
        children.extend(messages);
        let mut list = AccessNode::new(Role::List)
            .with_name(name)
            .with_children(children);
        if total == 0 {
            list = list.with_description(format!("No messages in {}", state.folder.title()));
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
            return AccessNode::new(Role::Group).with_name("No message selected");
        };
        let message_count = thread.len();
        let mut children = vec![
            AccessNode::new(Role::Toolbar)
                .with_name("Message actions")
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
        ];
        children.extend(thread.iter().map(|message| {
            AccessNode::new(Role::Paragraph)
                .with_name(format!("From {}", message.from.display()))
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
                        .with_name(format!(
                            "Save attachment {} ({})",
                            attachment.filename,
                            attachment.size_human()
                        ))
                        .with_action_target(format!(
                            "mail.reader:{}:save-attachment:{index}",
                            current.id
                        ))
                        .with_state(AccessState::new().focusable(true).disabled(!downloadable));
                    if !downloadable {
                        node = node.with_description("Download this attachment before saving it");
                    }
                    node
                }),
        );
        AccessNode::new(Role::Document)
            .with_name(format!("Reading: {}", current.subject))
            .with_description(format!(
                "Current message from {}; {message_count} message{} in this conversation{}",
                current.from.display(),
                if message_count == 1 { "" } else { "s" },
                if state.inspection == Some(current.id) {
                    "; previewing without changing its read state"
                } else {
                    ""
                },
            ))
            .with_children(children)
    }
}

impl Widget for MailApp {
    ui::provides!(dyn ui::widget::controls::ActionControls);
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document_ref(state, path);
        self.background_poll_deadline.set(None);
        self.adopt_person_accounts();
        if self.first_run.take_action() {
            self.open_setup_edit(None);
        }
        self.drain_setup_validation();
        self.drain_setup_discovery();
        // The host Save dialog runs off-thread; its outcome is applied here
        // so attachment export never blocks the frame loop or claims success
        // before the chosen file is actually written.
        self.drain_compose_attachment();
        self.compose.sync(state, path);
        self.show_status();

        let mem_seeded = self.shared.borrow().seeded;
        if !mem_seeded {
            // Live: kick off a connect + inbox fetch for the active account.
            let has_accounts = !self.shared.borrow().accounts.is_empty();
            if self.engine.is_some() && has_accounts && !self.connect_requested {
                let cfg = self.shared.borrow().accounts.active_config();
                if self.engine.is_some() {
                    self.post_engine(EngineRequest::connect(cfg.clone()));
                    self.post_engine(EngineRequest::fetch(cfg, Folder::Inbox));
                    self.shared.borrow_mut().status = Some("Syncing…".into());
                }
                self.connect_requested = true;
            }
            self.shared.borrow_mut().seeded = true;
        }
        // Live app: advance the wall clock (relative timestamps + the periodic
        // refresh cadence read it) and fire a periodic refresh when due. The
        // sample/`--shots` path pins `now_min`, so this only runs live.
        if self.engine.is_some() {
            self.shared.borrow_mut().now_min = crate::state::now_minutes();
            self.maybe_auto_refresh();
        }
        self.persist_active_account_if_changed();
        self.sync_preferences();
        let (title, list, reader) = {
            let state = self.shared.borrow();
            (
                Self::overview_title(&state),
                list_bar(&state),
                reader_bar(&state),
            )
        };
        if list != self.list_bar {
            let actions = header_actions(&list, &self.shared, "list");
            self.nav.set_overview_header(title.clone(), actions);
            self.overview_title = title;
            self.list_bar = list;
        } else if title != self.overview_title {
            self.nav.set_overview_title(title.clone());
            self.overview_title = title;
        }
        if reader != self.reader_bar {
            let actions = header_actions(&reader, &self.shared, "reader");
            self.nav.set_content_header("", actions);
            self.reader_bar = reader;
        }
        self.nav.sync(state, path);
        if let Some(sheet) = self.setup.as_mut() {
            sheet.dialog.sync(state, path);
        }
    }

    fn settle(&mut self, state: &mut ui::widget::Edits<'_>, path: &ObjId) {
        let opens = std::mem::take(&mut *self.opens.borrow_mut());
        for target in opens {
            if let Err(error) = self.open_target(target) {
                tracing::warn!(%error, "mail: a hit could not be opened");
            }
        }
        self.drain_engine(state);
        self.reveal_requested(state);
        self.deliver_effect_replies();
        self.flush_server_outbox();
        self.compose.settle(state, path);
        if !self.shared.borrow().seeded {
            self.register_hydrated_messages(state);
        }
        self.hydrate_domain_state(state);
        if let Some(scenario) = self.pending_stage.take() {
            self.stage_scenario(state, &scenario);
        }
        self.settle_setup(state);
        self.nav.settle(state, path);
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
        // The account sheet stands out of flow over the window, its fields
        // in the window's tree so they take its focus and Tab.
        self.setup_node = self.setup.as_mut().map(|sheet| {
            let node = sheet.dialog.build_layout(taffy, theme, node_map);
            children.push(node);
            node
        });
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
                // Rest the shell for this width before the first frame.
                self.nav.snap();
                self.first_layout = false;
                self.nav.apply_layout(taffy, nav_node, inner);
            }
            self.publish_shell_flags();
        }
        if let (Some(sheet), Some(node)) = (self.setup.as_mut(), self.setup_node) {
            sheet.dialog.apply_layout(taffy, node, inner);
        }
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
        // No ground of our own: the window paints it, the shell its regions.
        if self.is_first_run() {
            self.first_run.draw(scene, t, tr, theme, presence);
        } else {
            self.nav.draw(scene, t, tr, theme, presence);
        }
        if let Some(id) = self.pending_menu.take() {
            self.open_menu_on_row(id, tr, theme);
        }
    }

    /// The window's top layer: the shell's own floating parts and the
    /// confirmation dialog, then the composer, the account sheet, the row
    /// menu and the shortcut list, over the window's chrome.
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
        if let Some(sheet) = &mut self.setup {
            sheet.dialog.draw_overlay(scene, t, tr, theme, presence);
        }
        if self.menu.is_open() {
            self.menu.draw_overlay(scene, t, tr, theme, presence);
        }
        self.shortcuts.draw(scene, t, tr, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            let point = ctx.logical(position);
            self.shared.borrow_mut().last_mouse = point;
            self.dirty = true;
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

        self.dirty = true;
    }

    fn name(&self) -> &'static str {
        "MailApp"
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
        // Second, as in the layout tree: focus order pairs the two.
        if let Some(sheet) = &self.setup {
            children.push(&sheet.dialog);
        }
        if self.menu.is_open() {
            children.push(&self.menu);
        }
        if let Some(confirmation) = &self.bulk_purge_confirmation {
            children.push(&confirmation.dialog);
        }
        children
    }

    /// The tree is rebuilt when the account sheet opens or closes, besides
    /// whatever the children report.
    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = self.setup.is_some() != self.setup_node.is_some();
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
        if let Some(sheet) = &mut self.setup {
            children.push(&mut sheet.dialog);
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
        if self.compose.open {
            return self.compose.cursor_shape(point);
        }
        if self.is_first_run() {
            self.first_run.cursor_shape(point)
        } else {
            self.nav.cursor_shape(point)
        }
    }

    /// The mailbox is painted by three custom panes, so expose their actual
    /// visible composition here rather than leaving assistive technology with
    /// one opaque canvas.  This deliberately follows the shell's compact
    /// state: a narrow drawer hides the list, and a pushed reader hides both
    /// the list and drawer.  It also builds only from the loaded mailbox -- a
    /// live account with no fetched mail is described as empty, never filled
    /// with sample content.
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};

        if self.shortcuts.visible() {
            return Some(self.shortcuts.accessibility());
        }
        if let Some(confirmation) = &self.bulk_purge_confirmation {
            return confirmation.dialog.accessibility();
        }
        if self.compose.open {
            return Some(self.compose.accessibility_node());
        }
        if let Some(sheet) = &self.setup {
            return sheet.dialog.accessibility();
        }
        if self.menu.is_open() {
            return self.menu.accessibility();
        }
        if self.is_first_run() {
            // `first_run` remains a real child widget, which supplies the
            // onboarding action and its existing activation path.
            return Some(AccessNode::new(Role::Window).with_name("Mail"));
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

        if self.awaiting_engine {
            children.push(
                AccessNode::new(Role::Spinner)
                    .with_name("Syncing mail")
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
            children.push(Self::reader_accessibility_node(&st));
        }

        let composition = if drawer_open {
            "folder drawer"
        } else if compact {
            if self.nav.front_pane() == NavPane::Content {
                "reading pane"
            } else {
                "message list"
            }
        } else {
            "folders, message list, and reading pane"
        };
        Some(
            AccessNode::new(Role::Window)
                .with_name("Mail")
                .with_description(format!("Current view: {composition}"))
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
        if let Some(control) = target.strip_prefix("mail.setup:") {
            return self.perform_setup_semantic_action(control, action, state);
        }
        // The open sheet's footer, published on the window's own node.
        if let Some(sheet) = self.setup.as_mut()
            && (target.starts_with("dialog:") || target.starts_with("modal."))
        {
            let handled = sheet
                .dialog
                .perform_semantic_action(target, action, state, path);
            self.settle_setup(state);
            return handled;
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
        } else if self.compose.open {
            Some(self.compose.window_rect())
        } else if let Some(sheet) = &self.setup {
            Some(sheet.dialog.dialog_rect()).filter(|rect| rect.area() > 0.0)
        } else if self.is_first_run() {
            Some(self.rect)
        } else {
            None
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.shared.borrow().dirty
            || self.nav.needs_redraw()
            || self.compose.needs_redraw()
            || self.shortcuts.needs_redraw()
            || self
                .setup
                .as_ref()
                .is_some_and(|sheet| sheet.dialog.needs_redraw())
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        // Worker and native-picker completions are not animation. Keep their
        // deadline fixed until sync consumes a tick, even with reduced motion.
        let pending = self.awaiting_engine
            || self.setup_validation.is_some()
            || self.setup_discovery.is_some()
            || self.compose_attachment_rx.is_some();
        let background = pending.then(|| {
            let deadline = self
                .background_poll_deadline
                .get()
                .unwrap_or_else(|| ui::time::now() + std::time::Duration::from_millis(16));
            self.background_poll_deadline.set(Some(deadline));
            deadline
        });
        background
            .into_iter()
            .chain(self.nav.next_wake_deadline())
            .min()
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the event loop pumping frames while a worker request is in
        // flight so its result (a fetched batch, a send ack, an error) drains
        // and surfaces promptly instead of waiting for unrelated input. Drops
        // back to idle the moment the reply settles.
        self.awaiting_engine || self.nav.needs_animation_frame()
    }

    fn needs_idle_sync(&self) -> bool {
        // Keep syncing while a connect/fetch reply is outstanding or a status
        // waits for `sync` to show it, then idle rather than busy-loop once
        // results have arrived.
        self.awaiting_engine || self.shared.borrow().status.is_some() || self.nav.needs_idle_sync()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.shared.borrow_mut().dirty = false;
        self.nav.mark_drawn();
        self.compose.mark_drawn();
        self.shortcuts.mark_drawn();
        if let Some(sheet) = &mut self.setup {
            sheet.dialog.mark_drawn();
        }
    }
}

impl ui::widget::controls::ActionControls for MailApp {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.search.take().or_else(|| {
            self.children_mut()
                .into_iter()
                .find_map(ui::widget::controls::take_control_invocation)
        })
    }
}

/// What an effect asked the window to show that needs the document to show
/// it: opening a message records it read through the same `SetRead` action
/// as every control, the draft is read from the document, and an invitation
/// is saved as the draft. Taken up on the next sync.
#[derive(Debug)]
enum Reveal {
    Message(MessageId),
    Draft,
    Invitation(Draft),
    Contact(Draft),
}

impl MailApp {
    /// Start `effect`, invoked as `invocation`. Showing something answers at
    /// once; a refresh answers once the server replied, through
    /// [`Self::take_effect_result`]. Saving an attachment is the surface's,
    /// which asks the host where to put it.
    ///
    /// # Errors
    /// Why the effect does not apply to what this window has loaded now: a
    /// message, folder or account that is gone, no connected account.
    /// What the loaded message `message` says.
    fn read_message(&self, message: &str) -> Result<serde_json::Value, String> {
        let id = message
            .parse::<MessageId>()
            .map_err(|_| "invalid message id")?;
        self.shared
            .borrow()
            .mailbox
            .messages
            .get(&id)
            .map(message_read)
            .ok_or_else(|| "message is no longer loaded".into())
    }

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
                self.process_intents();
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
                self.shared.borrow_mut().status = Some("Preview".into());
                self.process_intents();
                Ok(done)
            }
            MailEffect::ReadMessage { message } => self
                .read_message(&message)
                .map(ui::framework::Effected::value),
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
                self.attachment_to_save(&message, index)?;
                Err("saving an attachment asks the host where to put it".into())
            }
        }
    }

    /// Start the effect a published operation's `payload` names, as the
    /// host does when it is invoked.
    #[cfg(test)]
    pub(crate) fn start_effect(
        &mut self,
        invocation: tilo_actions::InvocationId,
        payload: &[u8],
    ) -> Result<ui::framework::Effected, String> {
        let effect =
            <crate::actions::MailEffect as ui::tilo_resource::Effect>::from_payload(payload)?;
        self.begin_effect(invocation, effect)
    }

    /// What opens the place a search hit or an agent's `open` names: a
    /// folder, a message the mailbox publishes or one the window loaded.
    pub(crate) fn opener(
        &self,
    ) -> impl FnMut(&str) -> Result<(), ui::tilo_resource::Refusal> + 'static {
        use crate::actions::MailTarget;
        let opens = std::rc::Rc::clone(&self.opens);
        let shared = std::rc::Rc::clone(&self.shared);
        move |anchor| {
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

    /// Show `target`, once it is still there.
    fn open_target(&mut self, target: crate::actions::MailTarget) -> Result<(), String> {
        use crate::actions::MailTarget;
        match target {
            MailTarget::Folder(folder) => {
                let folder = Folder::from_key(&folder).ok_or("folder no longer exists")?;
                self.shared.borrow_mut().reveal_folder(folder);
                self.process_intents();
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
                self.process_intents();
            }
            MailTarget::Message(id) => {
                let id = id.parse::<MessageId>().map_err(|_| "invalid message id")?;
                if self.shared.borrow().mailbox.get(id).is_none() {
                    return Err("message is no longer loaded".into());
                }
                self.reveals.push(Reveal::Message(id));
            }
            MailTarget::Draft => self.reveals.push(Reveal::Draft),
        }
        self.dirty = true;
        Ok(())
    }

    /// Open the composer replying to, replying to all of, or forwarding the
    /// loaded `message`.
    fn compose_from(
        &mut self,
        message: &str,
        intent: fn(MessageId) -> Intent,
    ) -> Result<(), String> {
        let id = message
            .parse::<MessageId>()
            .map_err(|_| "invalid message id")?;
        if self.shared.borrow().mailbox.get(id).is_none() {
            return Err("message is no longer loaded".into());
        }
        self.shared.borrow_mut().push_intent(intent(id));
        self.process_intents();
        Ok(())
    }

    /// Take up what effects asked to show that needs the document.
    fn reveal_requested(&mut self, doc: &mut ui::widget::Edits<'_>) {
        for reveal in std::mem::take(&mut self.reveals) {
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
                Reveal::Contact(draft) => {
                    self.shared.borrow_mut().save_draft(doc, &draft);
                    self.compose.open_with_draft(&draft);
                    self.editing_draft = None;
                    self.shared.borrow_mut().status =
                        Some(crate::i18n::t("mail-contact-draft-prepared"));
                }
                Reveal::Invitation(draft) => {
                    self.shared.borrow_mut().save_draft(doc, &draft);
                    self.compose.open_with_draft(&draft);
                    self.editing_draft = None;
                    self.shared.borrow_mut().status = Some("Invitation drafted".into());
                }
            }
            self.dirty = true;
        }
    }

    /// The outcome of the refresh invoked as `invocation`, once the server
    /// replied; taken once.
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

    /// Answer `invocation`'s caller through `reply` once the server replied.
    pub(crate) fn await_effect(
        &mut self,
        invocation: tilo_actions::InvocationId,
        reply: ui::framework::EffectReply,
    ) {
        self.effect_replies.push((invocation, reply));
    }

    /// Answer every waiting caller whose refresh has settled.
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

    /// Whether a connected account can fetch and prepare mail now.
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

    /// The attachment `index` of the loaded `message`, as the name to suggest
    /// and the bytes to save.
    ///
    /// # Errors
    /// Why it cannot be saved: gone, not downloaded yet, too large, or its
    /// metadata no longer matching its bytes.
    pub(crate) fn attachment_to_save(
        &self,
        message: &str,
        index: usize,
    ) -> Result<(String, Vec<u8>), String> {
        let message_id = message
            .parse::<MessageId>()
            .map_err(|_| "invalid message id")?;
        let attachment = self
            .shared
            .borrow()
            .mailbox
            .get(message_id)
            .and_then(|message| message.attachments.get(index))
            .cloned()
            .ok_or_else(|| "attachment is no longer loaded".to_string())?;
        if !attachment.is_downloadable() {
            return Err("attachment content is not downloaded yet".into());
        }
        if attachment.content.len() > MAX_ATTACHMENT_SAVE_BYTES {
            return Err(format!(
                "{} is too large to save here; open it in Files instead",
                attachment.filename
            ));
        }
        let content_len = u64::try_from(attachment.content.len())
            .map_err(|_| "attachment is too large to save".to_string())?;
        if attachment.size != content_len {
            return Err("attachment metadata no longer matches its downloaded content".into());
        }
        Ok((
            Self::attachment_save_name(&attachment.filename),
            attachment.content.clone(),
        ))
    }

    /// Say whether the attachment `name` was saved, and answer its caller
    /// with the attachment it saved.
    pub(crate) fn finish_attachment_save(
        &mut self,
        saved: bool,
        name: &str,
        resource: String,
    ) -> ui::framework::Effected {
        self.shared.borrow_mut().status = Some(if saved {
            format!("Saved {name}")
        } else {
            "Attachment save cancelled".into()
        });
        self.dirty = true;
        ui::framework::Effected::Done(tilo_actions::ActionOutput {
            value: None,
            resources: if saved { vec![resource] } else { Vec::new() },
        })
    }

    /// Ask to save the attachment `index` of `message` through the effect
    /// every caller uses, so the window's own button, the palette and an
    /// agent save it the same way.
    fn request_attachment_save(&self, message: MessageId, index: usize) {
        if let Err(error) = self.attachment_to_save(&message.to_string(), index) {
            self.shared.borrow_mut().status = Some(error);
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

    /// The rows this window shows beside the mailbox: the accounts, an
    /// in-flight send and a search-index fault.
    pub(crate) fn live_rows(&self) -> Vec<ui::framework::LiveRow> {
        let mut rows: Vec<_> = crate::resource::live_extras(self)
            .into_iter()
            .map(ui::framework::LiveRow::from_resource)
            .collect();
        rows.push(ui::framework::LiveRow::from_resource(
            crate::actions::accounts_collection(&self.accounts_rows()),
        ));
        rows.extend(self.contact_recipient_rows());
        rows
    }

    /// The row `id` this window has loaded that the mailbox does not hold:
    /// a message the document has no flags for yet, a thread, an attachment,
    /// an account.
    pub(crate) fn live_row(&self, id: &str) -> Option<ui::framework::LiveRow> {
        if id.starts_with("mail.contact") {
            return self.contact_recipient_row(id);
        }
        if id.starts_with("mail.account") {
            let accounts = crate::actions::accounts_collection(&self.accounts_rows());
            if id == "mail.accounts" {
                return Some(ui::framework::LiveRow::from_resource(accounts));
            }
            return accounts
                .children
                .into_iter()
                .find(|account| account.id.as_str() == id)
                .map(ui::framework::LiveRow::from_resource);
        }
        if let Some(message_id) = id.strip_prefix("mail.message:") {
            return self.resource_message_row(message_id).map(|row| {
                ui::framework::LiveRow::from_resource(crate::actions::message_resource(&row))
            });
        }
        if let Some(rest) = id.strip_prefix("mail.attachment:")
            && let Some((message_id, index)) = rest.rsplit_once(':')
            && let Ok(index) = index.parse::<usize>()
        {
            let row = self.resource_message_row(message_id)?;
            return crate::actions::message_resource(&row)
                .children
                .into_iter()
                .nth(index)
                .map(ui::framework::LiveRow::from_resource);
        }
        if let Some(stable) = id.strip_prefix("mail.thread:") {
            let (subject, messages) = self.resource_thread_rows(stable)?;
            let mut thread = ui::framework::LiveRow::new(
                id,
                "mail.thread",
                if subject.is_empty() {
                    "(no subject)".into()
                } else {
                    subject
                },
            );
            for message in &messages {
                thread = thread.row(ui::framework::LiveRow::from_resource(
                    crate::actions::message_resource(message),
                ));
            }
            return Some(thread);
        }
        crate::resource::live_extras(self)
            .into_iter()
            .find(|extra| extra.id.as_str() == id)
            .map(ui::framework::LiveRow::from_resource)
    }

    /// This device's accounts, `(id, label, active)`.
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
        let format_before = self.compose.format();
        let compose_action = self.compose.perform_semantic_action(control, action);
        let lifecycle_boundary = !matches!(&compose_action, Some(ComposeAction::None));
        match compose_action {
            Some(ComposeAction::RequestAttachment) => self.request_compose_attachment(),
            Some(ComposeAction::Send) => self.send_compose(state),
            Some(ComposeAction::SaveDraft) => self.save_compose_draft(state),
            Some(ComposeAction::Close) => self.dirty = true,
            Some(ComposeAction::None) => {}
            None => return false,
        }
        if lifecycle_boundary || self.compose.format() != format_before {
            self.store_preferences_if_changed();
        }
        true
    }

    /// The account sheet's controls by the targets agents address them
    /// with (`mail.setup:…`), and its footer as the dialog publishes it.
    fn perform_setup_semantic_action(
        &mut self,
        control: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
    ) -> bool {
        use crate::model::AuthMethod;
        use crate::setup_sheet::{Server, SetupRequest, default_port};
        use ui::a11y::Action;

        let Some(sheet) = self.setup.as_ref() else {
            return false;
        };
        if let Some(raw) = control.strip_prefix("field:") {
            let Ok(index) = raw.parse::<usize>() else {
                return false;
            };
            let Action::SetValue(value) = action else {
                return false;
            };
            if index >= crate::setup_sheet::FIELD_COUNT
                || (index == crate::setup_sheet::PASSWORD
                    && sheet.fields.auth() != AuthMethod::Password)
            {
                return false;
            }
            sheet.fields.set_value(index, value);
            if index == crate::setup_sheet::EMAIL {
                self.maybe_autofill();
            }
            self.dirty = true;
            return true;
        }
        let toggles = matches!(action, Action::Activate | Action::ToggleCheck);
        let activates = matches!(action, Action::Activate);
        match control {
            "auth-password" if toggles => sheet.fields.set_auth(AuthMethod::Password),
            "auth-oauth" if toggles => sheet.fields.set_auth(AuthMethod::OAuth),
            "imap-tls" | "smtp-tls" if toggles => {
                let server = if control == "imap-tls" {
                    Server::Incoming
                } else {
                    Server::Outgoing
                };
                let (port, on) = (
                    if server == Server::Incoming {
                        crate::setup_sheet::IMAP_PORT
                    } else {
                        crate::setup_sheet::SMTP_PORT
                    },
                    !sheet.fields.tls(server),
                );
                sheet.fields.set_tls(server, on);
                sheet.fields.set_value(port, default_port(server, on));
            }
            "oauth-sign-in" if activates => sheet.request(SetupRequest::SignIn),
            "save" if activates => sheet.request(SetupRequest::Save),
            "cancel" if activates => sheet.request(SetupRequest::Cancel),
            "remove" if activates && self.setup_editing.is_some() => {
                sheet.request(SetupRequest::Remove);
            }
            _ => return false,
        }
        self.settle_setup(state);
        self.dirty = true;
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

    /// Assistive technology reaches the exact same local selection and
    /// canonical bulk-action boundary as the header controls and shortcuts.
    fn perform_bulk_semantic_action(
        &mut self,
        raw: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
    ) -> bool {
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let mut mail = self.shared.borrow_mut();
        match raw {
            "toggle" => {
                let selecting = mail.bulk_selecting;
                mail.set_bulk_selecting(!selecting);
            }
            "select-all" if mail.bulk_selecting => mail.select_all_visible(),
            "archive" if mail.bulk_selecting => {
                let affected = mail.apply_bulk_action(state, BulkAction::Archive);
                if affected == 0 {
                    return false;
                }
                mail.status = Some(format!(
                    "Archived {affected} message{}",
                    if affected == 1 { "" } else { "s" }
                ));
            }
            "delete" if mail.bulk_selecting => {
                let affected = mail.apply_bulk_action(state, BulkAction::Delete);
                if affected == 0 {
                    return false;
                }
                mail.status = Some(format!(
                    "Moved {affected} message{} to Trash",
                    if affected == 1 { "" } else { "s" }
                ));
            }
            "purge" if mail.bulk_selecting => {
                mail.push_intent(Intent::ConfirmBulkPurge);
            }
            "read" if mail.bulk_selecting => {
                let affected = mail.apply_bulk_action(state, BulkAction::MarkRead);
                if affected == 0 {
                    return false;
                }
                mail.status = Some(format!(
                    "Marked {affected} message{} read",
                    if affected == 1 { "" } else { "s" }
                ));
            }
            "unread" if mail.bulk_selecting => {
                let affected = mail.apply_bulk_action(state, BulkAction::MarkUnread);
                if affected == 0 {
                    return false;
                }
                mail.status = Some(format!(
                    "Marked {affected} message{} unread",
                    if affected == 1 { "" } else { "s" }
                ));
            }
            _ => return false,
        }
        drop(mail);
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
                    self.shared.borrow_mut().status = Some("Preview".into());
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
            Key::Character(ch) if plain && ch.eq_ignore_ascii_case("c") => {
                self.compose.open_blank();
                *handled = true;
            }
            // Searching mail is the platform's search, narrowed to the open
            // folder; the palette offers the rest of the mailbox from there.
            Key::Character(ch)
                if (plain && ch == "/") || (command && ch.eq_ignore_ascii_case("f")) =>
            {
                let request = self.folder_search();
                self.search.open(request);
                self.dirty = true;
                *handled = true;
            }
            Key::Named(NamedKey::F5) => {
                let _ = self.refresh_active_folder();
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
                    let affected = self
                        .shared
                        .borrow_mut()
                        .apply_bulk_action(state, BulkAction::Archive);
                    if affected > 0 {
                        self.shared.borrow_mut().status = Some(format!(
                            "Archived {affected} message{}",
                            if affected == 1 { "" } else { "s" }
                        ));
                    }
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
            Key::Named(NamedKey::Delete | NamedKey::Backspace) if plain => {
                if bulk_selecting {
                    let affected = self
                        .shared
                        .borrow_mut()
                        .apply_bulk_action(state, BulkAction::Delete);
                    if affected > 0 {
                        self.shared.borrow_mut().status = Some(format!(
                            "Moved {affected} message{} to Trash",
                            if affected == 1 { "" } else { "s" }
                        ));
                    }
                    self.process_intents();
                    *handled = true;
                } else if let Some(id) = selected {
                    self.shared
                        .borrow_mut()
                        .apply_message_action(state, id, CtxAction::Delete);
                    self.process_intents();
                    *handled = true;
                }
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
                    let affected = self
                        .shared
                        .borrow_mut()
                        .apply_bulk_action(state, BulkAction::MarkUnread);
                    if affected > 0 {
                        self.shared.borrow_mut().status = Some(format!(
                            "Marked {affected} message{} unread",
                            if affected == 1 { "" } else { "s" }
                        ));
                    }
                    *handled = true;
                } else if let Some(id) = selected {
                    self.shared.borrow_mut().set_read(state, id, false);
                    *handled = true;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::TestMail;
    use automerge::ReadDoc;

    #[cfg(feature = "live")]
    #[test]
    fn already_open_mail_adopts_settings_google_consent_on_sync() {
        use account_custody::{ClientTable, OAuthIssuer};
        use settings::accounts::{AccountKind, AccountRoot};
        use settings::panes::accounts::{AccountViewCommand, AccountsPane, BrowserOpener};
        use test_http::{Reply, Responder};
        use tilo_oauth::fake::{FakeAuthorizationServer, browse};

        struct Browser;
        impl BrowserOpener for Browser {
            fn open(&self, url: &str) -> ui::portal::LaunchChannel {
                let url = url.to_string();
                std::thread::spawn(move || browse(&url).unwrap());
                let (sender, receiver) = std::sync::mpsc::channel();
                sender.send(ui::portal::LaunchOutcome::Opened).unwrap();
                receiver
            }
        }

        let mut host =
            ui::framework::testing::TestApp::<settings::resource::SettingsShell>::new().unwrap();
        let record = host.person::<PersonAccounts>().unwrap();
        // No mail engine or remote avatars; the account projection and vault
        // are confined to this window's private temporary profile.
        let mut mail = MailApp::new_first_run();
        let root =
            AccountRoot::at(mail.accounts_persist_root().unwrap()).with_person(record.clone());
        mail.follow_person_accounts(record.clone());
        let doc = TestMail::doc();
        mail.sync(&doc, &automerge::ROOT);
        assert!(mail.shared.borrow().accounts.is_empty());
        assert!(mail.is_first_run());
        assert!(mail.engine.is_none());

        let oauth = FakeAuthorizationServer::start();
        let registration = ClientTable::defaults()
            .client_registration(OAuthIssuer::Google)
            .unwrap();
        oauth.require_client_secret(registration.secret().unwrap());
        let gmail = Responder::start(vec![
            Reply::status(200)
                .header("Content-Type", "application/json")
                .body(r#"{"emailAddress":"ada@example.org"}"#),
        ]);
        let custody = root
            .custodian()
            .with_issuer_endpoints(OAuthIssuer::Google, oauth.provider())
            .with_gmail_profile_endpoint(format!("{}/profile", gmail.url()));
        let mut pane = AccountsPane::in_root(root.clone())
            .with_custodian(custody)
            .with_browser_opener(std::sync::Arc::new(Browser));
        pane.command(AccountViewCommand::Add(AccountKind::GoogleDrive));
        pane.command(AccountViewCommand::SubmitGoogleMail);
        assert!(pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()));
        assert!(
            !pane.sign_in_open(),
            "{:?}",
            pane.sign_in_message_for_test()
        );
        assert_eq!(gmail.next_request().target, "/profile");
        let published = AccountStore::load_in(root.explicit().unwrap());
        assert_eq!(published.accounts.len(), 1);
        assert_eq!(record.get().store().len(), 2);
        assert!(mail.shared.borrow().accounts.is_empty());

        // The same already-open widget observes Settings' shared-record edit
        // through the production sync path, without reopening or re-following.
        mail.sync(&doc, &automerge::ROOT);
        let shown = mail.shared.borrow();
        assert_eq!(shown.accounts.accounts.len(), 1);
        let account = shown.accounts.active_account().unwrap();
        assert_eq!(account.id, published.accounts[0].id);
        assert_eq!(account.config.email, "ada@example.org");
        assert_eq!(account.config.imap_host, "imap.gmail.com");
        assert_eq!(account.config.smtp_host, "smtp.gmail.com");
        assert_eq!(account.config.smtp_port, 587);
        assert_eq!(account.config.auth, accounts::email::AuthMethod::OAuth);
        assert!(account.config.password.is_empty());
        drop(shown);
        assert!(!mail.is_first_run());
        assert!(mail.engine.is_none());
    }

    fn cursor_at(p: Point) -> WindowEvent {
        ui::synthetic_input::cursor_moved(p.x, p.y)
    }
    fn left_press() -> WindowEvent {
        ui::synthetic_input::mouse_press()
    }
    fn esc_event() -> WindowEvent {
        named_key(NamedKey::Escape)
    }
    fn named_key(k: NamedKey) -> WindowEvent {
        ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
            Key::Named(k),
            None,
            ElementState::Pressed,
        ))
    }
    fn char_key(s: &str) -> WindowEvent {
        ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
            Key::Character(s.into()),
            Some(s.into()),
            ElementState::Pressed,
        ))
    }

    /// Route `ev` to the app through the tree it was last laid out in (a
    /// bare node before any layout).
    fn dispatch(app: &mut MailApp, ev: &WindowEvent, doc: &mut AutoCommit) -> bool {
        let (taffy, node) = app.test_layout.take().unwrap_or_else(|| {
            let mut taffy = TaffyTree::<()>::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            (taffy, node)
        });
        let handled = ui::testing::Dispatch::new()
            .doc(doc)
            .layout(&taffy, node)
            .send(app, ev)
            .handled;
        if app.test_layout.is_none() {
            app.test_layout = Some((taffy, node));
        }
        handled
    }

    fn left_release() -> WindowEvent {
        ui::synthetic_input::mouse_release()
    }

    fn app() -> (MailApp, AutoCommit) {
        let mut app = MailApp::new_sample();
        let mut doc = crate::resource::TestMail::doc();
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        (app, doc)
    }

    struct InterleavedEngineFixture {
        app: MailApp,
        doc: AutoCommit,
        incoming: std::sync::mpsc::Sender<EngineEvent>,
        refresh_id: EngineRequestId,
        send_id: EngineRequestId,
        unrelated_id: EngineRequestId,
        invocation: tilo_actions::InvocationId,
    }

    impl InterleavedEngineFixture {
        fn new() -> Self {
            let (mut app, doc) = app();
            let (outgoing, _outgoing_rx) = std::sync::mpsc::channel();
            let (incoming, incoming_rx) = std::sync::mpsc::channel();
            app.engine = Some(EngineHandle {
                tx: outgoing,
                rx: incoming_rx,
            });
            let refresh_id = uuid::Uuid::new_v4();
            let send_id = uuid::Uuid::new_v4();
            let unrelated_id = uuid::Uuid::new_v4();
            let invocation = tilo_actions::InvocationId::new();
            app.resource_refresh_invocation = Some((refresh_id, invocation));
            let message = app.shared.borrow().mailbox.messages_in(Folder::Inbox)[0].clone();
            let outgoing = Outgoing::new(
                "me@example.test",
                &crate::model::Draft {
                    to: "you@example.test".into(),
                    ..Default::default()
                },
            );
            app.pending_send.insert(
                send_id,
                PendingSend {
                    outgoing,
                    message,
                    done: Box::new(|_| {}),
                },
            );
            app.pending_engine_requests
                .insert(refresh_id, EngineRequestKind::Fetch);
            app.pending_engine_requests
                .insert(send_id, EngineRequestKind::Send);
            app.pending_engine_requests
                .insert(unrelated_id, EngineRequestKind::StoreFlag);
            app.awaiting_engine = true;
            Self {
                app,
                doc,
                incoming,
                refresh_id,
                send_id,
                unrelated_id,
                invocation,
            }
        }

        fn send(&self, event: EngineEvent) {
            self.incoming.send(event).unwrap();
        }

        fn drain(&mut self) {
            self.app
                .drain_engine(&mut ui::widget::Edits::lend(&mut self.doc));
        }
    }

    /// What the document still has to tell the server goes to the engine on
    /// the account it belongs to, and the engine's acknowledgement records it
    /// as applied; a refusal leaves it to tell, and an update for an account
    /// this device does not have is left to a device that has it.
    #[test]
    fn server_updates_go_to_the_engine_and_its_acknowledgement_settles_them() {
        type Outcome = Rc<RefCell<Option<Result<(), String>>>>;
        fn push(app: &MailApp, update: ServerUpdate) -> Outcome {
            let outcome: Outcome = Rc::default();
            let seen = Rc::clone(&outcome);
            app.server_outbox().borrow_mut().push(
                update,
                Box::new(move |result| *seen.borrow_mut() = Some(result)),
            );
            outcome
        }

        let (mut app, mut doc) = app();
        let configured_email = app.shared.borrow().accounts.active_config().email;
        let (outgoing, outgoing_rx) = std::sync::mpsc::channel();
        let (incoming, incoming_rx) = std::sync::mpsc::channel();
        app.engine = Some(EngineHandle {
            tx: outgoing,
            rx: incoming_rx,
        });
        let id = app.shared.borrow().mailbox.messages_in(Folder::Inbox)[0].id;
        let message = {
            let mut state = app.shared.borrow_mut();
            let message = state.mailbox.get_mut(id).unwrap();
            message.rfc_message_id = Some("launch@example.test".into());
            message.clone()
        };
        app.shared.borrow().ingest_fetched(
            &mut ui::widget::Edits::lend(&mut doc),
            &configured_email,
            std::slice::from_ref(&message),
        );
        app.shared
            .borrow_mut()
            .set_read(&mut ui::widget::Edits::lend(&mut doc), id, !message.read);
        let [update] = TestMail::server_updates(&doc).try_into().unwrap();

        let outcome = push(&app, update.clone());
        app.flush_server_outbox();
        let request = outgoing_rx
            .try_recv()
            .expect("the update went to the engine");
        let EngineRequest::StoreFlag {
            account,
            message: located,
            ..
        } = &request
        else {
            panic!("a flag change is a flag store: {request:?}");
        };
        assert_eq!(account.email, configured_email);
        assert_eq!(
            located.rfc_message_id.as_deref(),
            Some("launch@example.test")
        );
        assert_eq!(located.read, !message.read);
        assert_eq!(located.folder, Folder::Inbox);

        incoming
            .send(EngineEvent::Error {
                request_id: request.request_id(),
                kind: EngineRequestKind::StoreFlag,
                context: "flag".into(),
                message: "connection reset".into(),
            })
            .unwrap();
        app.drain_engine(&mut ui::widget::Edits::lend(&mut doc));
        assert!(matches!(outcome.borrow().as_ref(), Some(Err(_))));
        assert_eq!(
            TestMail::server_updates(&doc),
            std::slice::from_ref(&update),
            "still to tell"
        );

        let outcome = push(&app, update);
        app.flush_server_outbox();
        let request = outgoing_rx.try_recv().expect("retried");
        incoming
            .send(EngineEvent::Completed {
                request_id: request.request_id(),
                kind: EngineRequestKind::StoreFlag,
            })
            .unwrap();
        app.drain_engine(&mut ui::widget::Edits::lend(&mut doc));
        assert!(matches!(outcome.borrow().as_ref(), Some(Ok(()))));
        assert!(
            TestMail::server_updates(&doc).is_empty(),
            "the server has it"
        );

        let elsewhere = push(
            &app,
            ServerUpdate::Remove {
                id: id.to_string(),
                account: "nobody@example.test".into(),
                locator: "launch@example.test".into(),
                folder: Folder::Trash,
            },
        );
        app.flush_server_outbox();
        assert!(
            outgoing_rx.try_recv().is_err(),
            "nothing went to the engine"
        );
        assert!(matches!(elsewhere.borrow().as_ref(), Some(Err(_))));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn calendar_invitation_handoff_creates_reviewable_drafts_and_refuses_bad_payloads() {
        // Phase `calendar_invitation_handoff_creates_a_durable_reviewable_draft`.
        {
            let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:event@example\r\nDTSTART:20260915T120000Z\r\nDTEND:20260915T130000Z\r\nSUMMARY:Review\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            let draft = prepare_calendar_invitation_draft(
                "guest@example.com",
                "Invitation: Review",
                "Please review this invitation.",
                ics,
            )
            .unwrap();
            let mut doc = crate::resource::TestMail::doc();
            let mut state = MailState::new_sample(avatar_store_offline());
            state.save_draft(&mut ui::widget::Edits::lend(&mut doc), &draft);
            let stored = TestMail::load_draft(&doc);
            assert_eq!(stored.to, "guest@example.com");
            assert_eq!(stored.subject, "Invitation: Review");
            assert_eq!(stored.calendar_ics.as_deref(), Some(ics));
            assert!(
                stored.attachments.is_empty(),
                "no host path crosses the action"
            );
            let steps = ui::tilo_resource::history::versions(&mut doc.clone())
                .into_iter()
                .filter_map(|version| version.context)
                .filter(|context| context.change.is_step())
                .count();
            assert_eq!(steps, 1, "saving the draft is one undo step");
        }

        // Phase `calendar_invitation_handoff_refuses_malformed_and_oversized_payloads`.
        {
            let valid = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            assert!(prepare_calendar_invitation_draft("bad", "Invite", "", valid).is_err());
            assert!(
                prepare_calendar_invitation_draft(
                    "a@example.com",
                    "Invite",
                    "",
                    &"x".repeat(256 * 1024 + 1),
                )
                .is_err()
            );
        }
    }

    fn click_at(app: &mut MailApp, doc: &mut AutoCommit, p: Point) -> bool {
        dispatch(app, &cursor_at(p), doc);
        let pressed = dispatch(app, &left_press(), doc);
        let released = dispatch(app, &left_release(), doc);
        pressed || released
    }

    /// The first list row's centre, as the shell laid the list out.
    fn first_row_centre(app: &MailApp) -> Point {
        let geometry = app.nav.geometry();
        let column = geometry.overview_column;
        Point::new(
            column.center().x,
            column.y0 + geometry.header_height + f64::from(Theme::dark().tokens.spacing.s6),
        )
    }

    /// The mailbox starts with a selection; wide shows three columns, narrow
    /// pushes the message and the drawer, and reflowing restores the columns.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn pane_layout_reflows_between_wide_and_narrow_and_navigates_folders() {
        // Phase `starts_in_mailbox_with_a_selection`.
        {
            let (app, _) = app();
            assert!(app.shared.borrow().selected.is_some());
            assert_eq!(app.shared.borrow().folder, Folder::Inbox);
        }

        // Phase `wide_shows_three_columns_abreast`.
        {
            let (mut app, _doc) = app();
            app.layout_for_test(1280.0, 800.0);
            let geometry = app.nav.geometry();
            assert!(
                geometry.sidebar_column.width() > 1.0,
                "docked sidebar present"
            );
            assert!(
                geometry.overview_column.x0 >= geometry.sidebar_column.x1 - 0.5,
                "list right of sidebar"
            );
            assert!(
                geometry.content_column.x0 >= geometry.overview_column.x1 - 0.5,
                "reader right of list"
            );
            assert!(!app.shared.borrow().narrow);
        }

        // Phase `narrow_drawer_opens_over_a_settled_layout`: the header rows
        // of the columns behind the drawer are laid out hidden, and the
        // layout still settles (it panics in `layout_for_test` otherwise).
        {
            let (mut app, _doc) = app();
            app.nav.open_sidebar();
            app.nav.snap();
            app.layout_for_test(560.0, 860.0);
            assert!(app.shared.borrow().narrow);
        }

        // Phase `narrow_open_message_pushes_detail_then_back`.
        {
            let (mut app, mut doc) = app();
            app.layout_for_test(560.0, 860.0);
            assert!(app.shared.borrow().narrow, "560 separates list and message");
            assert_eq!(app.nav_pane(), NavPane::Overview, "starts on the list");
            let row = first_row_centre(&app);
            assert!(click_at(&mut app, &mut doc, row), "row click consumed");
            app.layout_for_test(560.0, 860.0);
            assert_eq!(
                app.nav_pane(),
                NavPane::Content,
                "opening a message pushes it"
            );
            assert!(app.shared.borrow().detail_pushed);
            assert!(app.shared.borrow().selected.is_some());

            // The shell shows its Back button; Escape goes back too.
            let back = app.nav.geometry().back;
            assert!(back.width() > 0.0, "Back shown while the message is pushed");
            assert!(
                dispatch(&mut app, &esc_event(), &mut doc),
                "Escape consumed"
            );
            assert_eq!(
                app.nav_pane(),
                NavPane::Overview,
                "Escape returns to the list"
            );
        }

        // Phase `narrow_then_wide_reflows_to_three_columns`.
        {
            let (mut app, _doc) = app();
            app.layout_for_test(1280.0, 800.0);
            let wide_content_x0 = app.nav.geometry().content_column.x0;
            app.layout_for_test(560.0, 860.0);
            assert!(app.shared.borrow().narrow, "narrow flag set at 560");
            let geometry = app.nav.geometry();
            let front = match app.nav_pane() {
                NavPane::Content => geometry.content_column,
                _ => geometry.overview_column,
            };
            assert!(
                (front.width() - 560.0).abs() < 2.0,
                "the front screen is full-width narrow: {geometry:?}"
            );
            app.layout_for_test(1280.0, 800.0);
            assert!(!app.shared.borrow().narrow, "wide flag re-derived at 1280");
            assert!(
                (app.nav.geometry().content_column.x0 - wide_content_x0).abs() < 2.0,
                "reflows back to three columns"
            );
        }

        // Phase `choosing_a_folder_switches_the_list`.
        {
            let (mut app, mut doc) = app();
            app.layout_for_test(1280.0, 800.0);
            app.shared
                .borrow_mut()
                .push_command(Command::OpenFolder(0, Folder::Sent));
            app.apply_commands(&mut ui::widget::Edits::lend(&mut doc));
            assert_eq!(app.shared.borrow().folder, Folder::Sent);
            app.sync(&doc, &automerge::ROOT);
            assert_eq!(app.overview_title, "Sent", "the list's header follows");
        }

        // Phase `narrow_toggle_opens_and_esc_closes_drawer`.
        {
            let (mut app, mut doc) = app();
            app.layout_for_test(560.0, 860.0);
            let toggle = app.nav.geometry().toggle;
            assert!(toggle.width() > 0.0, "sidebar toggle on the narrow list");
            // The toggle is the shell's own action; the host runs it.
            app.nav.toggle_sidebar();
            app.layout_for_test(560.0, 860.0);
            assert!(app.nav.is_sidebar_open(), "the toggle opens the drawer");
            assert!(dispatch(&mut app, &esc_event(), &mut doc), "Esc consumed");
            assert!(!app.nav.is_sidebar_open(), "Esc closes the drawer");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn accessibility_routes_panes_reader_attachments_and_context_menu() {
        // Phase `accessibility_tracks_compact_panes_and_routes_mailbox_activation`.
        {
            use ui::a11y::{Action, Role as A11yRole};

            let (mut app, mut doc) = app();
            app.layout_for_test(560.0, 860.0);

            let overview = app.accessibility().expect("mailbox semantics");
            assert!(
                overview
                    .children
                    .iter()
                    .any(|node| node.role == A11yRole::List),
                "the compact overview exposes its visible message list"
            );
            assert!(
                !overview
                    .children
                    .iter()
                    .any(|node| node.role == A11yRole::Document),
                "the covered reader is omitted in the compact overview"
            );
            let message_target = overview
                .children
                .iter()
                .find(|node| node.role == A11yRole::List)
                .and_then(|list| {
                    list.children
                        .iter()
                        .find(|node| node.role == A11yRole::ListItem)
                })
                .and_then(|row| row.action_target.clone())
                .expect("visible message row has a stable activation target");

            assert!(app.perform_semantic_action(
                &message_target,
                &Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            app.layout_for_test(560.0, 860.0);
            assert_eq!(
                app.nav_pane(),
                NavPane::Content,
                "semantic open pushes reader"
            );
            let reader = app.accessibility().expect("reader semantics");
            assert!(
                reader
                    .children
                    .iter()
                    .any(|node| node.role == A11yRole::Document),
                "the pushed compact reader exposes the selected conversation"
            );
            assert!(
                !reader
                    .children
                    .iter()
                    .any(|node| node.role == A11yRole::List),
                "the covered compact overview is absent from AT traversal"
            );

            app.layout_for_test(1280.0, 800.0);
            assert!(app.perform_semantic_action(
                "mail.folder:0:archive",
                &Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert_eq!(
                app.shared.borrow().folder,
                Folder::Archive,
                "semantic folder activation uses the same selection state as the sidebar"
            );
        }

        // Phase `accessibility_exposes_attachment_save_through_the_portal_boundary`.
        {
            use ui::a11y::{Action, Role as A11yRole};

            let (mut app, mut doc) = app();
            let message_id = {
                let state = app.shared.borrow();
                state
                    .mailbox
                    .messages
                    .values()
                    .find(|message| message.attachments.iter().any(Attachment::is_downloadable))
                    .map(|message| message.id)
                    .expect("sample has a hydrated attachment")
            };
            assert!(app.shared.borrow_mut().preview_message(message_id));
            app.process_intents();
            let target = app
                .accessibility()
                .expect("reader semantics")
                .children
                .iter()
                .find(|node| node.role == A11yRole::Document)
                .and_then(|document| {
                    document.children.iter().find_map(|child| {
                        child
                            .action_target
                            .as_ref()
                            .filter(|target| target.contains(":save-attachment:"))
                            .cloned()
                    })
                })
                .expect("hydrated attachment is a named screen-reader button");
            assert!(app.perform_semantic_action(
                &target,
                &Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert!(
                app.shared.borrow().status.is_none(),
                "the reader's button asks to save a downloaded attachment without a refusal"
            );
        }

        // Phase `accessibility_routes_reader_and_modal_controls_to_existing_actions`.
        {
            use ui::a11y::{Action, Role as A11yRole};

            let (mut app, mut doc) = app();
            app.layout_for_test(1280.0, 800.0);
            let selected = app.shared.borrow().selected.expect("sample selection");
            let before_star = app.shared.borrow().mailbox.get(selected).unwrap().starred;
            let reader = app.accessibility().expect("reader semantics");
            let star_target = reader
                .children
                .iter()
                .find(|node| node.role == A11yRole::Document)
                .and_then(|document| document.children.first())
                .and_then(|toolbar| {
                    toolbar
                        .children
                        .iter()
                        .find(|node| node.name.as_deref() == Some("Toggle star"))
                })
                .and_then(|button| button.action_target.clone())
                .expect("reader star control");
            assert!(app.perform_semantic_action(
                &star_target,
                &Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            assert_eq!(
                app.shared.borrow().mailbox.get(selected).unwrap().starred,
                !before_star
            );
            let other = app
                .shared
                .borrow()
                .list_rows()
                .into_iter()
                .map(|(id, _)| id)
                .find(|id| *id != selected)
                .expect("second sample message");
            app.shared
                .borrow_mut()
                .open_message(&mut ui::widget::Edits::lend(&mut doc), other);
            app.process_intents();
            assert!(
                !app.perform_semantic_action(
                    &star_target,
                    &Action::Activate,
                    &mut ui::widget::Edits::lend(&mut doc),
                    &automerge::ROOT
                ),
                "a stale reader control cannot operate a newly selected message"
            );

            app.compose.open_blank();
            let compose = app.accessibility().expect("compose dialog semantics");
            assert_eq!(compose.role, A11yRole::Dialog);
            assert!(compose.children.iter().any(|node| {
                node.role == A11yRole::TextInput && node.name.as_deref() == Some("To")
            }));
            assert!(compose.children.iter().any(|node| {
                node.role == A11yRole::MultilineTextInput
                    && node.name.as_deref() == Some("Message body")
            }));
            assert!(app.perform_semantic_action(
                "mail.compose:discard",
                &Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            assert!(
                !app.compose.open,
                "semantic discard uses compose close path"
            );

            app.open_setup_edit(None);
            let setup = app.accessibility().expect("setup dialog semantics");
            assert_eq!(setup.role, A11yRole::Dialog);
            let email = keyed(
                &app.setup.as_ref().expect("sheet open").dialog,
                "mail.setup:field:1",
            )
            .expect("the address field is published");
            assert_eq!(email.role, A11yRole::TextInput);
            assert_eq!(email.name.as_deref(), Some("Email"));
            assert!(app.perform_semantic_action(
                "mail.setup:cancel",
                &Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            assert!(!app.setup_open(), "semantic cancel closes account setup");
        }

        // Phase `the_row_menu_is_the_shared_menu_and_applies_its_action`.
        {
            let (mut app, mut doc) = app();
            app.layout_for_test(1280.0, 800.0);
            let id = app.shared.borrow().mailbox.messages_in(Folder::Inbox)[0].id;
            app.shared.borrow_mut().push_intent(Intent::ContextMenu(
                id,
                Point::new(400.0, 300.0),
                Rect::new(280.0, 280.0, 620.0, 320.0),
            ));
            app.process_intents();
            let menu = app.accessibility().expect("context menu semantics");
            assert_eq!(menu.role, ui::a11y::Role::Menu);
            let archive = menu
                .children
                .iter()
                .position(|item| item.name.as_deref() == Some("Archive"))
                .expect("Archive is described to assistive technology");
            app.menu.click_item(archive);
            app.take_menu_choice(&mut ui::widget::Edits::lend(&mut doc));
            assert!(!app.menu.is_open());
            assert!(app.menu_target.is_none());
            assert_eq!(
                app.shared
                    .borrow()
                    .mailbox
                    .get(id)
                    .map(|message| message.folder),
                Some(Folder::Archive)
            );
        }
    }

    /// Opening (row, resource, Enter) records the read action; explicit preview (P,
    /// resource preview) and the legacy read-only route stay read-neutral.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn opening_marks_read_through_the_document_action_and_preview_stays_neutral() {
        // Phase `selecting_message_marks_read_and_persists`.
        {
            let (app, mut doc) = app();
            let id = {
                let st = app.shared.borrow();
                st.mailbox
                    .messages_in(Folder::Inbox)
                    .into_iter()
                    .find(|m| !m.read)
                    .map(|m| m.id)
            }
            .expect("an unread message");
            app.shared
                .borrow_mut()
                .open_message(&mut ui::widget::Edits::lend(&mut doc), id);
            assert!(app.shared.borrow().mailbox.get(id).unwrap().read);
            let mut fresh = app.shared.borrow().mailbox.clone();
            for m in fresh.messages.values_mut() {
                m.read = false;
            }
            TestMail::apply_flags(&doc, &mut fresh);
            assert!(
                fresh.get(id).unwrap().read,
                "read flag persisted to the CRDT"
            );
        }

        // Phase `resource_open_marks_message_read_through_the_document_action`.
        {
            let (mut app, mut doc) = app();
            let id = app
                .shared
                .borrow()
                .mailbox
                .messages_in(Folder::Inbox)
                .into_iter()
                .find(|message| !message.read)
                .map(|message| message.id)
                .expect("sample contains an unread message");
            let payload = crate::actions::effect_payload(&crate::actions::MailEffect::Open {
                target: crate::actions::MailTarget::Message(id.to_string()),
            });
            assert!(matches!(
                app.start_effect(tilo_actions::InvocationId::default(), &payload)
                    .unwrap(),
                ui::framework::Effected::Done(_)
            ));
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            assert!(
                app.shared.borrow().mailbox.get(id).unwrap().read,
                "Open dispatches the canonical mark-read action"
            );
            assert_eq!(
                TestMail::get_read(&doc, &id.to_string()),
                Some(true),
                "resource Open writes the same mark-read every on-screen control does"
            );
            assert!(
                doc.get(&automerge::ROOT, "ui")
                    .expect("document remains readable")
                    .is_none(),
                "Open records no selection, folder, search, mode, or compose presentation state"
            );
        }

        // Phase `resource_opens_before_one_sync_each_keep_normal_read_semantics`.
        {
            let (mut app, mut doc) = app();
            let ids: Vec<_> = app
                .shared
                .borrow()
                .mailbox
                .messages_in(Folder::Inbox)
                .into_iter()
                .filter(|message| !message.read)
                .map(|message| message.id)
                .take(2)
                .collect();
            assert_eq!(ids.len(), 2, "sample contains two unread inbox messages");

            for id in &ids {
                let payload = crate::actions::effect_payload(&crate::actions::MailEffect::Open {
                    target: crate::actions::MailTarget::Message(id.to_string()),
                });
                assert!(matches!(
                    app.start_effect(tilo_actions::InvocationId::default(), &payload)
                        .unwrap(),
                    ui::framework::Effected::Done(_)
                ));
            }
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);

            assert!(ids.iter().all(|id| {
                app.shared
                    .borrow()
                    .mailbox
                    .get(*id)
                    .is_some_and(|message| message.read)
            }));
        }

        // The removed read-only operation protocol cannot bypass the authoritative door.
        {
            let mut hosted =
                ui::framework::testing::TestApp::<crate::resource::MailShell>::capture(None)
                    .unwrap();
            let snapshot = |app: &mut MailApp| {
                let state = app.shared.borrow();
                (
                    state.folder,
                    state.selected,
                    state.inspection,
                    state.status.clone(),
                    state
                        .mailbox
                        .messages
                        .values()
                        .map(|m| (m.id, m.read))
                        .collect::<std::collections::BTreeMap<_, _>>(),
                )
            };
            let before = hosted.with_surface(snapshot).unwrap();
            let heads_before = hosted.read_document(|doc| doc.clone().get_heads());
            let instance_id = hosted.harness().component_instance_id().unwrap();
            let legacy = ui::bound_dispatch::BoundActionAddress {
                instance_id,
                address: tilo_actions::ActionAddress::new("mail.mailbox", "mail.operation"),
            };
            let error = hosted
                .harness()
                .invoke_bound_action(&legacy, &serde_json::Map::new())
                .expect_err("the removed protocol is not an admitted mutable operation");
            assert!(
                !error.to_string().is_empty(),
                "the host explains its refusal"
            );
            hosted.settle();
            assert_eq!(
                hosted.read_document(|doc| doc.clone().get_heads()),
                heads_before
            );
            assert_eq!(
                hosted.with_surface(snapshot).unwrap(),
                before,
                "refusal changes neither presentation nor any read flag"
            );
        }

        // Phase `explicit_resource_preview_keeps_an_unread_message_unread_and_announces_that_fact`.
        {
            let (mut app, mut doc) = app();
            let id = app
                .shared
                .borrow()
                .mailbox
                .messages_in(Folder::Inbox)
                .into_iter()
                .find(|message| !message.read)
                .map(|message| message.id)
                .expect("sample contains an unread message");
            let payload = crate::actions::effect_payload(&crate::actions::MailEffect::Inspect {
                message: id.to_string(),
            });
            let heads_before_preview = doc.get_heads();
            assert!(matches!(
                app.start_effect(ui::tilo_actions::InvocationId::new(), &payload)
                    .unwrap(),
                ui::framework::Effected::Done(_)
            ));
            assert_eq!(app.selected_message(), Some(id));
            assert!(
                !app.shared.borrow().mailbox.get(id).unwrap().read,
                "preview is read-state neutral before sync"
            );
            assert_eq!(doc.get_heads(), heads_before_preview);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            assert!(
                !app.shared.borrow().mailbox.get(id).unwrap().read,
                "unlike Open message, explicit preview never queues mark-read"
            );
            assert_ne!(
                TestMail::get_read(&doc, &id.to_string()),
                Some(true),
                "a read-state-neutral preview writes no read state"
            );
            let semantics = app.accessibility().expect("reader accessibility");
            let reader = semantics
                .children
                .iter()
                .find(|node| node.role == ui::a11y::Role::Document)
                .expect("reader document");
            assert!(
                reader
                    .description
                    .as_deref()
                    .is_some_and(|text| text.contains("without changing its read state")),
                "assistive technology receives the same explicit preview distinction"
            );
        }

        // Phase `shortcut_enter_opens_and_records_the_same_read_action_as_a_row`.
        {
            let (mut app, mut doc) = app();
            app.layout_for_test(1280.0, 800.0);
            let unread = app
                .shared
                .borrow()
                .list_rows()
                .into_iter()
                .map(|(id, _)| id)
                .find(|id| {
                    app.shared
                        .borrow()
                        .mailbox
                        .get(*id)
                        .is_some_and(|mail| !mail.read)
                })
                .expect("sample inbox has unread mail");
            app.shared.borrow_mut().selected = Some(unread);

            assert!(dispatch(&mut app, &named_key(NamedKey::Enter), &mut doc));
            assert!(
                app.shared
                    .borrow()
                    .mailbox
                    .get(unread)
                    .is_some_and(|mail| mail.read),
                "Enter uses the normal Open boundary, rather than silently previewing"
            );
            assert_eq!(TestMail::get_read(&doc, &unread.to_string()), Some(true));
            assert_ne!(app.shared.borrow().inspection, Some(unread));
        }

        // Phase `shortcut_p_is_explicit_read_neutral_preview`.
        {
            let (mut app, mut doc) = app();
            app.layout_for_test(1280.0, 800.0);
            let unread = app
                .shared
                .borrow()
                .list_rows()
                .into_iter()
                .map(|(id, _)| id)
                .find(|id| {
                    app.shared
                        .borrow()
                        .mailbox
                        .get(*id)
                        .is_some_and(|mail| !mail.read)
                })
                .expect("sample inbox has unread mail");
            app.shared.borrow_mut().selected = Some(unread);
            let heads_before = doc.get_heads();

            assert!(dispatch(&mut app, &char_key("p"), &mut doc));
            let state = app.shared.borrow();
            assert!(state.mailbox.get(unread).is_some_and(|mail| !mail.read));
            assert_eq!(state.inspection, Some(unread));
            assert_eq!(
                doc.get_heads(),
                heads_before,
                "preview does not author a mail action"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn composer_opens_privately_seeds_replies_and_saves_drafts_in_place() {
        // Phase `resource_compose_actions_open_the_same_private_composer_without_mutating_mail`.
        {
            let (mut app, mut doc) = app();
            let message = {
                let mail = app.shared.borrow();
                mail.mailbox
                    .messages_in(Folder::Inbox)
                    .first()
                    .copied()
                    .cloned()
                    .expect("sample contains an inbox message")
            };
            let heads_before = doc.get_heads();

            let compose = crate::actions::effect_payload(&crate::actions::MailEffect::Compose);
            assert!(matches!(
                app.start_effect(ui::tilo_actions::InvocationId::new(), &compose)
                    .unwrap(),
                ui::framework::Effected::Done(_)
            ));
            assert!(
                app.compose.open,
                "mailbox Compose opens the normal composer"
            );
            app.compose.close();

            let reply = crate::actions::effect_payload(&crate::actions::MailEffect::Reply {
                message: message.id.to_string(),
            });
            assert!(matches!(
                app.start_effect(ui::tilo_actions::InvocationId::new(), &reply)
                    .unwrap(),
                ui::framework::Effected::Done(_)
            ));
            assert!(app.compose.open, "message Reply opens the normal composer");
            assert_eq!(
                app.compose
                    .to_draft(&mut ui::widget::Edits::lend(&mut doc.clone()))
                    .to,
                message.from.rfc5322()
            );
            assert_eq!(
                doc.get_heads(),
                heads_before,
                "an unsaved compose buffer remains local presentation state"
            );
        }

        // Phase `sending_queues_the_message_in_the_outbox_until_it_is_delivered`.
        {
            let (mut app, mut doc) = app();
            let before = app.shared.borrow().mailbox.count(Folder::Sent);
            app.compose.open_blank();
            app.compose.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
            let draft = crate::model::Draft {
                to: "dest@there.example".into(),
                subject: "Hi".into(),
                body: "Hello".into(),
                html_body: Some("<p>Hello</p>".into()),
                ..Default::default()
            };
            app.compose.reattach(&draft);
            app.send_compose(&mut ui::widget::Edits::lend(&mut doc));
            assert!(
                TestMail::outbox(&doc).is_empty(),
                "sending asks the person first"
            );
            let dispatcher = app
                .shared
                .borrow()
                .dispatcher
                .clone()
                .expect("a dispatcher");
            TestMail::confirm(&mut doc, &dispatcher);
            let outbox = TestMail::outbox(&doc);
            let [(outgoing, None)] = outbox.as_slice() else {
                panic!("one message waits in the outbox: {outbox:?}");
            };
            assert_eq!(outgoing.draft.to, "dest@there.example");
            assert_eq!(outgoing.draft.subject, "Hi");
            assert!(
                !app.compose.open,
                "the message left the composer for the outbox"
            );
            assert_eq!(
                app.shared.borrow().mailbox.count(Folder::Sent),
                before,
                "a queued message is not delivered"
            );

            // The server refusing it leaves it in the outbox with the reason,
            // and brings it back to the composer.
            let (outgoing_tx, _outgoing_rx) = std::sync::mpsc::channel();
            let (incoming, incoming_rx) = std::sync::mpsc::channel();
            app.engine = Some(EngineHandle {
                tx: outgoing_tx,
                rx: incoming_rx,
            });
            let heard = std::rc::Rc::new(std::cell::RefCell::new(None));
            let answer = std::rc::Rc::clone(&heard);
            app.server_outbox().borrow_mut().deliver(
                outgoing.clone(),
                Box::new(move |result| *answer.borrow_mut() = Some(result)),
            );
            app.flush_server_outbox();
            let (&request_id, _) = app
                .pending_send
                .iter()
                .next()
                .expect("handed to the engine");
            incoming
                .send(EngineEvent::Error {
                    request_id,
                    kind: EngineRequestKind::Send,
                    context: "send".into(),
                    message: "relay denied".into(),
                })
                .unwrap();
            app.drain_engine(&mut ui::widget::Edits::lend(&mut doc));
            let settled = heard
                .borrow_mut()
                .take()
                .expect("the delivery answered")
                .expect("refused for good, not tried again");
            assert!(!settled.is_done());
            // The outbox drain records how it settled.
            TestMail::settle_delivery(&mut doc, &outgoing.id, &settled);
            let outbox = TestMail::outbox(&doc);
            assert!(
                matches!(outbox.as_slice(), [(_, Some(reason))] if reason.contains("relay denied")),
                "{outbox:?}"
            );
            assert!(
                app.compose.open,
                "a refused message comes back to the composer"
            );

            // Sending it again replaces the refused one.
            app.send_compose(&mut ui::widget::Edits::lend(&mut doc));
            TestMail::confirm(&mut doc, &dispatcher);
            let outbox = TestMail::outbox(&doc);
            assert!(
                matches!(outbox.as_slice(), [(again, None)] if again.id != outgoing.id),
                "{outbox:?}"
            );
        }

        // Phase `reply_all_seeds_sender_in_to_and_others_in_cc`.
        {
            let (mut app, mut doc) = app();
            // A message with more than one recipient.
            let id = {
                let st = app.shared.borrow();
                st.mailbox
                    .messages
                    .values()
                    .find(|m| m.folder == Folder::Inbox && m.to.len() > 1)
                    .expect("a multi-recipient message")
                    .id
            };
            app.shared.borrow_mut().apply_message_action(
                &mut ui::widget::Edits::lend(&mut doc),
                id,
                CtxAction::ReplyAll,
            );
            app.process_intents();
            assert!(app.compose.open, "reply-all opens the composer");
            let d = app.compose.to_draft(&mut ui::widget::Edits::lend(&mut doc));
            let sender = app
                .shared
                .borrow()
                .mailbox
                .get(id)
                .unwrap()
                .from
                .email
                .clone();
            assert!(d.to.contains(&sender), "sender goes in To: {}", d.to);
            assert!(!d.cc.is_empty(), "other recipients populate Cc: {}", d.cc);
            assert!(
                !d.cc.contains("you@example.test"),
                "our own address is excluded from Cc: {}",
                d.cc
            );
            assert!(
                !d.cc.contains(&sender),
                "the sender is not duplicated into Cc: {}",
                d.cc
            );
        }

        // Phase `save_draft_lands_in_drafts_and_reopens`.
        {
            let (mut app, mut doc) = app();
            let before = app.shared.borrow().mailbox.count(Folder::Drafts);
            app.compose.open_blank();
            app.compose.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
            let draft = crate::model::Draft {
                to: "dest@there.example".into(),
                subject: "WIP proposal".into(),
                body: "half written".into(),
                html_body: Some("<p>half written</p>".into()),
                ..Default::default()
            };
            app.compose.reattach(&draft);
            let attachment =
                crate::attachment_store::stage_bytes(&app.content, b"draft agenda", "agenda.txt")
                    .expect("stage portable attachment");
            app.compose
                .add_attachment(attachment.clone())
                .expect("attach to draft");
            app.save_compose_draft(&mut ui::widget::Edits::lend(&mut doc));
            assert!(!app.compose.open, "save closes the composer");
            assert_eq!(
                app.shared.borrow().mailbox.count(Folder::Drafts),
                before + 1,
                "the draft appears in Drafts"
            );

            // Reopening the Drafts message re-seeds the composer for editing.
            let id = {
                let st = app.shared.borrow();
                st.mailbox
                    .messages_in(Folder::Drafts)
                    .iter()
                    .find(|m| m.subject == "WIP proposal")
                    .unwrap()
                    .id
            };
            app.open_draft(id);
            assert!(app.compose.open, "the draft reopens in the composer");
            assert_eq!(
                app.compose
                    .to_draft(&mut ui::widget::Edits::lend(&mut doc))
                    .subject,
                "WIP proposal"
            );
            assert_eq!(
                app.compose
                    .to_draft(&mut ui::widget::Edits::lend(&mut doc))
                    .attachments,
                vec![attachment]
            );
            assert_eq!(app.editing_draft, Some(id));
        }

        // Phase `saving_edited_draft_updates_in_place_not_duplicates`.
        {
            let (mut app, mut doc) = app();
            app.compose.open_blank();
            app.compose.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
            app.compose.reattach(&crate::model::Draft {
                subject: "First".into(),
                body: "one".into(),
                ..Default::default()
            });
            app.save_compose_draft(&mut ui::widget::Edits::lend(&mut doc));
            let after_first = app.shared.borrow().mailbox.count(Folder::Drafts);
            // Re-open + re-save the same draft.
            let id = app.editing_draft.expect("editing the saved draft");
            app.open_draft(id);
            app.save_compose_draft(&mut ui::widget::Edits::lend(&mut doc));
            assert_eq!(
                app.shared.borrow().mailbox.count(Folder::Drafts),
                after_first,
                "re-saving updates in place"
            );
        }

        // Phase `shortcut_c_opens_compose`.
        {
            let (mut app, mut doc) = app();
            app.layout_for_test(1280.0, 800.0);
            assert!(!app.compose.open);
            assert!(
                dispatch(&mut app, &char_key("c"), &mut doc),
                "c shortcut consumed"
            );
            assert!(app.compose.open, "'c' opened the composer");
        }

        // Phase `compose_picker_completion_wakes_without_animation_and_stops_polling`.
        {
            for (outcome, expected) in [
                (
                    Some(OpenFileOutcome::Cancelled),
                    "Attachment selection cancelled",
                ),
                (
                    Some(OpenFileOutcome::Failed("fixture denial".into())),
                    "Couldn’t open attachment picker: fixture denial",
                ),
                (
                    None,
                    "Attachment picker disconnected before selecting a file",
                ),
            ] {
                let (mut app, mut doc) = app();
                app.compose.open_blank();
                let (sender, receiver) = std::sync::mpsc::channel();
                if let Some(outcome) = outcome {
                    sender.send(outcome).unwrap();
                }
                drop(sender);
                app.compose_attachment_rx = Some(receiver);
                let deadline = app
                    .next_wake_deadline()
                    .expect("picker completion must wake");
                assert_eq!(app.next_wake_deadline(), Some(deadline));
                app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
                app.sync(&doc, &automerge::ROOT);
                app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
                app.sync(&doc, &automerge::ROOT);
                assert!(app.compose_attachment_rx.is_none());
                assert_eq!(app.nav.toast_text(), Some(expected));
                app.nav.dismiss_toast();
                assert!(app.next_wake_deadline().is_none());
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn context_swipe_and_bulk_purge_share_confirmed_actions() {
        fn row_menu(app: &mut MailApp, id: MessageId) -> Vec<CtxAction> {
            app.shared.borrow_mut().push_intent(Intent::ContextMenu(
                id,
                Point::new(400.0, 300.0),
                Rect::new(280.0, 280.0, 620.0, 320.0),
            ));
            app.process_intents();
            assert!(app.menu.is_open(), "menu opened");
            app.menu_target
                .as_ref()
                .map(|target| target.items.clone())
                .unwrap_or_default()
        }

        // Phase `bulk_purge_requires_an_explicit_modal_confirmation`.
        {
            use ui::a11y::Role;

            let (mut app, mut doc) = app();
            let ids: Vec<MessageId> = app
                .shared
                .borrow()
                .mailbox
                .messages_in(Folder::Inbox)
                .into_iter()
                .take(2)
                .map(|message| message.id)
                .collect();
            {
                let mut mail = app.shared.borrow_mut();
                mail.set_bulk_selecting(true);
                for id in &ids {
                    mail.mailbox.move_to(*id, Folder::Trash);
                    mail.toggle_bulk_selection(*id);
                }
                mail.push_intent(Intent::ConfirmBulkPurge);
            }
            app.process_intents();
            assert!(app.bulk_purge_confirmation.is_some());
            assert!(
                ids.iter()
                    .all(|id| app.shared.borrow().mailbox.get(*id).is_some()),
                "opening confirmation never performs a purge"
            );
            let dialog = app.accessibility().expect("modal accessibility");
            assert_eq!(dialog.role, Role::Dialog);

            // Escape dismisses without deleting anything.
            assert!(dispatch(&mut app, &esc_event(), &mut doc));
            assert!(app.bulk_purge_confirmation.is_none());
            assert!(
                ids.iter()
                    .all(|id| app.shared.borrow().mailbox.get(*id).is_some())
            );

            // Confirming deletes them.
            app.shared
                .borrow_mut()
                .push_intent(Intent::ConfirmBulkPurge);
            app.process_intents();
            // Return is the safe answer in a destructive confirmation.
            assert!(dispatch(&mut app, &named_key(NamedKey::Enter), &mut doc));
            assert!(app.bulk_purge_confirmation.is_none());
            assert!(
                ids.iter()
                    .all(|id| app.shared.borrow().mailbox.get(*id).is_some()),
                "Enter cancels a permanent delete"
            );

            // Operating "Delete permanently" deletes them.
            app.shared
                .borrow_mut()
                .push_intent(Intent::ConfirmBulkPurge);
            app.process_intents();
            let confirmation = app
                .bulk_purge_confirmation
                .as_mut()
                .expect("the confirmation is open");
            assert!(Widget::perform_semantic_action(
                &mut confirmation.dialog,
                "modal.primary",
                &ui::a11y::Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            app.settle_purge_confirmation(&mut ui::widget::Edits::lend(&mut doc));
            assert!(app.bulk_purge_confirmation.is_none());
            assert!(
                ids.iter()
                    .all(|id| app.shared.borrow().mailbox.get(*id).is_none())
            );
        }

        // Phase `context_and_swipe_actions_are_mirrored`.
        {
            let (app, _doc) = app();
            let id = app.shared.borrow().mailbox.messages_in(Folder::Inbox)[0].id;
            let menu = app.shared.borrow().context_items_for(id);
            assert!(
                menu.contains(&CtxAction::Archive),
                "the swipe's action is in the menu"
            );
            assert!(menu.contains(&CtxAction::Delete));
        }

        // Phase `context_menu_click_applies_the_action`.
        {
            let (mut app, mut doc) = app();
            app.layout_for_test(1280.0, 800.0);
            let id = app.shared.borrow().mailbox.messages_in(Folder::Inbox)[0].id;
            let items = row_menu(&mut app, id);
            let idx = items.iter().position(|a| *a == CtxAction::Archive).unwrap();
            let rect = app.menu.item_rect(idx).expect("archive item rect");
            let before = app.shared.borrow().mailbox.count(Folder::Archive);
            assert!(
                click_at(&mut app, &mut doc, rect.center()),
                "menu item click consumed"
            );
            assert_eq!(
                app.shared.borrow().mailbox.count(Folder::Archive),
                before + 1,
                "clicking Archive archived the message"
            );
            assert!(!app.menu.is_open(), "menu dismissed after the action");
        }

        // Phase `context_menu_keyboard_moves_focus_and_activates_the_visible_action`.
        {
            let (mut app, mut doc) = app();
            app.layout_for_test(1280.0, 800.0);
            let id = app.shared.borrow().mailbox.messages_in(Folder::Inbox)[0].id;
            let items = row_menu(&mut app, id);
            let archive_index = items
                .iter()
                .position(|item| *item == CtxAction::Archive)
                .expect("archive is an offered menu command");
            for _ in 0..archive_index {
                assert!(dispatch(
                    &mut app,
                    &named_key(NamedKey::ArrowDown),
                    &mut doc
                ));
            }
            let before = app.shared.borrow().mailbox.count(Folder::Archive);
            assert!(dispatch(&mut app, &named_key(NamedKey::Enter), &mut doc));
            assert_eq!(
                app.shared.borrow().mailbox.count(Folder::Archive),
                before + 1
            );
            assert!(!app.menu.is_open());
        }

        // Phase `the_menu_accelerators_act_on_its_message`.
        {
            let (mut app, mut doc) = app();
            app.layout_for_test(1280.0, 800.0);
            let id = app.shared.borrow().mailbox.messages_in(Folder::Inbox)[0].id;
            let _ = row_menu(&mut app, id);
            assert!(dispatch(&mut app, &char_key("e"), &mut doc));
            assert_eq!(
                app.shared.borrow().mailbox.get(id).map(|m| m.folder),
                Some(Folder::Archive)
            );
            assert!(!app.menu.is_open());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the regression test spells out a complete interleaving so ownership mistakes remain diagnosable"
    )]
    fn engine_events_settle_only_their_own_request() {
        // Phase `interleaved_engine_events_only_settle_their_own_request`.
        {
            let mut fixture = InterleavedEngineFixture::new();
            let deadline = fixture
                .app
                .next_wake_deadline()
                .expect("worker results wake independently of animation");
            assert_eq!(fixture.app.next_wake_deadline(), Some(deadline));

            fixture.send(EngineEvent::Error {
                request_id: fixture.unrelated_id,
                kind: EngineRequestKind::StoreFlag,
                context: "flag".into(),
                message: "rejected".into(),
            });
            fixture.send(EngineEvent::Sent {
                request_id: fixture.unrelated_id,
                wire_bytes: 9,
            });
            fixture.send(EngineEvent::Completed {
                request_id: fixture.unrelated_id,
                kind: EngineRequestKind::StoreFlag,
            });
            fixture.drain();
            assert!(fixture.app.resource_refresh_invocation.is_some());
            assert!(fixture.app.resource_refresh_result.is_none());
            assert!(!fixture.app.pending_send.is_empty());
            assert!(
                fixture
                    .app
                    .pending_engine_requests
                    .contains_key(&fixture.refresh_id)
            );
            assert!(
                fixture
                    .app
                    .pending_engine_requests
                    .contains_key(&fixture.send_id)
            );
            assert!(
                !fixture
                    .app
                    .pending_engine_requests
                    .contains_key(&fixture.unrelated_id)
            );
            assert!(
                fixture
                    .app
                    .shared
                    .borrow()
                    .status
                    .as_deref()
                    .is_none_or(|status| !status.contains("sent")),
                "an unmatched Sent event must not claim delivery"
            );

            fixture.send(EngineEvent::Fetched {
                request_id: fixture.send_id,
                account_id: "me@example.test".into(),
                folder: Folder::Inbox,
                messages: Vec::new(),
            });
            fixture.drain();
            assert!(
                !fixture.app.pending_send.is_empty(),
                "wrong-kind results are inert"
            );
            assert!(
                fixture
                    .app
                    .pending_engine_requests
                    .contains_key(&fixture.send_id)
            );

            fixture.send(EngineEvent::Fetched {
                request_id: fixture.refresh_id,
                account_id: "me@example.test".into(),
                folder: Folder::Inbox,
                messages: Vec::new(),
            });
            fixture.send(EngineEvent::Completed {
                request_id: fixture.refresh_id,
                kind: EngineRequestKind::Fetch,
            });
            fixture.drain();
            assert!(fixture.app.resource_refresh_invocation.is_some());
            assert!(matches!(
                fixture.app.resource_refresh_result.as_ref(),
                Some(tilo_actions::InvocationUpdate::Succeeded { invocation: got, .. }) if *got == fixture.invocation
            ));
            assert!(matches!(
                fixture.app.take_effect_result(fixture.invocation),
                Some(tilo_actions::InvocationUpdate::Succeeded { invocation: got, .. }) if got == fixture.invocation
            ));
            assert!(fixture.app.resource_refresh_invocation.is_none());
            assert!(!fixture.app.pending_send.is_empty());

            fixture.send(EngineEvent::Sent {
                request_id: fixture.send_id,
                wire_bytes: 42,
            });
            fixture.send(EngineEvent::Completed {
                request_id: fixture.send_id,
                kind: EngineRequestKind::Send,
            });
            fixture.drain();
            assert!(fixture.app.pending_send.is_empty());
            assert!(!fixture.app.awaiting_engine);
            assert_eq!(
                fixture.app.next_wake_deadline(),
                fixture.app.nav.next_wake_deadline()
            );
        }

        // Phase `completed_refresh_cannot_be_replaced_before_its_owner_polls`.
        {
            let (mut app, _doc) = app();
            let (outgoing, _outgoing_rx) = std::sync::mpsc::channel();
            let (_incoming_tx, incoming) = std::sync::mpsc::channel();
            app.engine = Some(EngineHandle {
                tx: outgoing,
                rx: incoming,
            });
            let request_id = uuid::Uuid::new_v4();
            let owner = tilo_actions::InvocationId::new();
            app.resource_refresh_invocation = Some((request_id, owner));
            app.resource_refresh_result = Some(tilo_actions::InvocationUpdate::Succeeded {
                invocation: owner,
                output: tilo_actions::ActionOutput::default(),
            });

            let payload = crate::actions::effect_payload(&crate::actions::MailEffect::Refresh);
            assert!(
                app.start_effect(tilo_actions::InvocationId::new(), &payload)
                    .is_err()
            );
            assert!(
                app.take_effect_result(tilo_actions::InvocationId::new())
                    .is_none()
            );
            assert!(matches!(
                app.take_effect_result(owner),
                Some(tilo_actions::InvocationUpdate::Succeeded { invocation, .. }) if invocation == owner
            ));
            assert!(app.resource_refresh_invocation.is_none());
        }
    }

    /// Setup saves add accounts; sample mode persists to a sandbox and never the real
    /// store; live mode uses the real config root; settings are scoped per account.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn account_saves_persist_to_the_right_root_for_each_mode() {
        // Phase `setup_save_adds_account`.
        {
            let (mut app, mut doc) = app();
            app.open_setup_edit(None);
            app.set_setup_input(1, "new@host.example");
            app.set_setup_input(2, "new@host.example");
            app.set_setup_input(4, "imap.host.example");
            app.set_setup_input(6, "smtp.host.example");
            let before = app.shared.borrow().accounts.len();
            // Drive the offline probe explicitly so this offline-save flow is
            // deterministic under both builds (the live probe's connect gating is
            // covered by the `validate` state-machine tests and the socket tests).
            app.save_setup_with(
                &mut ui::widget::Edits::lend(&mut doc),
                &crate::validate::OfflineProbe,
            );
            assert!(!app.setup_open());
            assert_eq!(app.shared.borrow().accounts.len(), before + 1);
            assert_eq!(
                app.shared.borrow().accounts.active_config().email,
                "new@host.example"
            );
        }

        // Phase `sample_mode_persists_to_a_sandbox_not_the_real_store`.
        {
            // Regression guard for the leak where the bundled demo accounts
            // (you@example.test, you@hooli.example, new@host.example) ended up in the
            // user's real ~/.config/tilo/accounts/store.json: the sample / shots /
            // test app must resolve a throwaway sandbox as its persist target, never
            // the real per-user config root.
            let root = MailApp::new_sample()
                .accounts_persist_root()
                .expect("sample mode has a persist root");
            let real = accounts::config_root().expect("a resolvable real config root");
            assert_ne!(
                root, real,
                "sample mode must NOT target the real config root"
            );
            assert!(
                root.starts_with(std::env::temp_dir()),
                "sample mode must persist under a throwaway temp dir, got {}",
                root.display()
            );
        }

        // Phase `sample_mode_save_never_writes_the_real_account_store`.
        {
            // Snapshot the real store path's bytes (it may or may not exist on this
            // machine), drive the full add-account save in sample mode — which does
            // persist — then prove the account landed in the sandbox and the real
            // store is byte-for-byte unchanged.
            let real_path = accounts::store_path();
            let before = real_path.as_ref().and_then(|p| std::fs::read(p).ok());

            let (mut app, mut doc) = app(); // new_sample: sandboxed
            app.open_setup_edit(None);
            app.set_setup_input(1, "guard@example.test");
            app.set_setup_input(2, "guard@example.test");
            app.set_setup_input(4, "imap.example.test");
            app.set_setup_input(6, "smtp.example.test");
            app.save_setup_with(
                &mut ui::widget::Edits::lend(&mut doc),
                &crate::validate::OfflineProbe,
            );
            assert_eq!(
                app.shared.borrow().accounts.active_config().email,
                "guard@example.test",
                "the account was added in memory"
            );

            // The save landed in the sandbox …
            let root = app.accounts_persist_root().expect("sandbox root");
            let sandboxed = crate::account::AccountStore::load_in(&root);
            assert!(
                sandboxed
                    .accounts
                    .iter()
                    .any(|a| a.config.email == "guard@example.test"),
                "the save must have persisted into the sandbox store"
            );

            // … and the real store is untouched (absent stays absent; present stays
            // identical).
            let after = real_path.as_ref().and_then(|p| std::fs::read(p).ok());
            assert_eq!(
                before, after,
                "sample / shots mode must not create or modify the real account store"
            );
        }

        // Phase `a_live_window_persists_into_its_hosts_profile`.
        {
            // A session window keeps the person's accounts; a test, capture or
            // `--data-dir` profile keeps its own beside its settings document.
            let session = ui::storage::AppStorage::for_app("so.tilo.mail");
            assert_eq!(profile_config_root(&session), accounts::config_root());
            let profile = tempfile::tempdir().unwrap();
            let storage = ui::storage::AppStorage::new(
                "so.tilo.mail",
                session.secrets(),
                ui::storage::SettingsDocument::File(profile.path().join("settings.json")),
            );
            assert_eq!(
                profile_config_root(&storage).as_deref(),
                Some(profile.path())
            );
            let app = MailApp::for_window(&storage, Some(ContentStore::memory()));
            assert_eq!(
                app.accounts_persist_root().as_deref(),
                Some(profile.path()),
                "the live app persists into its host's profile"
            );
            assert!(
                app.shared.borrow().accounts.is_empty(),
                "a fresh profile has no accounts"
            );
            assert!(
                app.shared.borrow().mailbox.messages.is_empty(),
                "no fabricated startup mail"
            );
        }

        // Phase `account_settings_sheet_is_scoped_to_the_selected_account`.
        {
            let (mut app, _doc) = app(); // sample store: 2 accounts, active = 0
            assert_eq!(app.shared.borrow().accounts.active, 0);
            // Opening settings for account #1 (the work account), reached from that
            // account's gear — must populate the form from THAT account, not the
            // active one, so the sheet honestly reads as its settings.
            app.open_setup_edit(Some(1));
            assert_eq!(app.setup_editing, Some(1));
            assert_eq!(
                app.read_setup_config().email,
                "sam@northwind.example",
                "the account-settings form is scoped to the clicked account"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn presentation_state_stays_local() {
        // Phase `presentation_state_is_local_and_never_restored_from_a_peer_document`.
        {
            let mut doc = crate::resource::TestMail::doc();
            {
                let mut app = MailApp::new_sample();
                app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
                app.sync(&doc, &automerge::ROOT);
                app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
                app.sync(&doc, &automerge::ROOT);
                app.shared
                    .borrow_mut()
                    .select_folder(&mut ui::widget::Edits::lend(&mut doc), Folder::Sent);
                app.shared.borrow_mut().set_active_account(1);
                app.compose.set_format(ComposeFormat::Text);
            }
            // A second app sharing the document receives only domain state.
            let mut app2 = MailApp::new_sample();
            app2.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app2.sync(&doc, &automerge::ROOT);
            app2.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app2.sync(&doc, &automerge::ROOT);
            assert_eq!(
                app2.shared.borrow().folder,
                Folder::Inbox,
                "folder is local"
            );
            assert_eq!(
                app2.shared.borrow().accounts.active,
                0,
                "the device-local active account is never restored from shared state"
            );
            assert_eq!(
                app2.compose.format(),
                ComposeFormat::Html,
                "compose format is local"
            );
            assert!(
                doc.get(&automerge::ROOT, "ui")
                    .expect("document remains readable")
                    .is_none(),
                "window presentation never enters collaborative state"
            );
        }

        // Phase `active_account_selection_persists_locally_without_touching_automerge`.
        {
            let (mut app, mut doc) = app();
            let selected_id = app.shared.borrow().accounts.accounts[1].id.clone();
            app.shared.borrow_mut().set_active_account(1);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);

            let root = app
                .accounts_persist_root()
                .expect("sample app has a sandbox");
            let reloaded = AccountStore::load_in(&root);
            assert_eq!(
                reloaded.active_account().map(|account| account.id.as_str()),
                Some(selected_id.as_str()),
                "the shared device-local account store remembers the selected account"
            );
            assert!(
                doc.get(&automerge::ROOT, "account")
                    .expect("document remains readable")
                    .is_none(),
                "account configuration never crosses the collaborative boundary"
            );
        }

        // Phase `first_run_shows_onboarding_and_add_opens_setup`.
        {
            let mut app = MailApp::new_first_run();
            let mut doc = crate::resource::TestMail::doc();
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            app.layout_for_test(1100.0, 780.0);
            assert!(
                app.is_first_run(),
                "no configured account → first-run onboarding"
            );
            let semantics = app.first_run.accessibility().expect("first-run semantics");
            assert_eq!(semantics.name.as_deref(), Some("No email account yet"));
            assert!(semantics.children.iter().any(|node| {
                node.role == ui::a11y::Role::Button && node.name.as_deref() == Some("Add account")
            }));
            assert!(
                app.first_run.perform_action(
                    &ui::a11y::Action::Activate,
                    &[],
                    &mut ui::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                ),
                "assistive activation reaches Add account"
            );
            app.sync(&doc, &automerge::ROOT);
            assert!(
                app.setup_open(),
                "Add account opens the account setup sheet"
            );
            assert!(
                !app.is_first_run(),
                "an open setup sheet supersedes the first-run screen"
            );
        }

        // Phase `status_shows_as_the_shell_toast`.
        {
            let (mut app, _doc) = app();
            app.shared.borrow_mut().status = Some("Synced 3 messages".into());
            app.show_status();
            assert_eq!(app.nav.toast_text(), Some("Synced 3 messages"));
            assert!(
                app.shared.borrow().status.is_none(),
                "the shell owns it now"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn setup_autofill_fills_servers_ports_and_oauth_presets() {
        // Phase `setup_email_autofills_connection_hosts`.
        {
            let (mut app, _doc) = app();
            app.open_setup_edit(None);
            app.set_setup_input(1, "person@gmail.com");
            app.maybe_autofill();
            assert_eq!(app.setup_value(4), "imap.gmail.com", "IMAP host autofilled");
            assert_eq!(app.setup_value(6), "smtp.gmail.com", "SMTP host autofilled");
        }

        // Phase `gmail_email_presets_oauth_and_still_autofills_hosts`.
        {
            let (mut app, _doc) = app();
            app.open_setup_edit(None);
            app.set_setup_input(1, "someone@gmail.com");
            app.maybe_autofill();
            assert_eq!(
                app.setup_auth(),
                crate::model::AuthMethod::OAuth,
                "Gmail presets to OAuth"
            );
            assert!(
                app.setup_provider().is_some(),
                "provider detected from the domain"
            );
            assert_eq!(
                app.setup_value(4),
                "imap.gmail.com",
                "IMAP host still autofilled"
            );
        }

        // Phase `autofill_uses_the_builtin_table_across_providers`.
        {
            // The built-in table fills the RIGHT servers offline, including cases
            // the old generic `imap.<domain>` guess got wrong (Outlook → office365).
            let (mut app, _doc) = app();
            app.open_setup_edit(None);
            app.set_setup_input(1, "person@outlook.de");
            app.maybe_autofill();
            assert_eq!(
                app.setup_value(4),
                "outlook.office365.com",
                "Outlook → office365, any TLD"
            );
            assert_eq!(app.setup_value(6), "smtp.office365.com");

            app.open_setup_edit(None);
            app.set_setup_input(1, "hi@gmx.net");
            app.maybe_autofill();
            assert_eq!(
                app.setup_value(4),
                "imap.gmx.net",
                "GMX resolved offline from the table"
            );

            app.open_setup_edit(None);
            app.set_setup_input(1, "me@fastmail.com");
            app.maybe_autofill();
            assert_eq!(app.setup_value(4), "imap.fastmail.com");
            assert_eq!(app.setup_value(5), "993", "port filled from the table too");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn keyboard_shortcuts_drive_the_visible_triage_actions() {
        // Phase `shortcut_reference_is_modal_and_explains_open_vs_preview`.
        {
            let (mut app, mut doc) = app();
            app.layout_for_test(1280.0, 800.0);
            assert!(dispatch(&mut app, &char_key("?"), &mut doc));
            assert!(app.shortcuts.visible());
            let overlay = app.accessibility().expect("shortcut sheet semantics");
            assert_eq!(overlay.name.as_deref(), Some("Keyboard shortcuts"));
            let rows: Vec<&str> = overlay
                .children
                .iter()
                .filter_map(|row| row.name.as_deref())
                .collect();
            assert!(rows.contains(&"Open message: Enter"), "{rows:?}");
            assert!(
                rows.contains(&"Preview without marking read: P"),
                "{rows:?}"
            );
            assert!(dispatch(&mut app, &esc_event(), &mut doc));
            assert!(!app.shortcuts.visible(), "Escape dismisses the modal sheet");
        }

        // Phase `shortcut_e_archives_selected`.
        {
            let (mut app, mut doc) = app();
            app.layout_for_test(1280.0, 800.0);
            let before = app.shared.borrow().mailbox.count(Folder::Archive);
            let id = app.shared.borrow().selected.expect("a selected message");
            dispatch(&mut app, &char_key("e"), &mut doc);
            assert_eq!(
                app.shared.borrow().mailbox.count(Folder::Archive),
                before + 1,
                "'e' archived the selection"
            );
            assert_eq!(
                TestMail::get_folder_for(&doc, &id.to_string()),
                Some(Folder::Archive)
            );
        }

        // Phase `reader_shortcuts_reply_star_and_mark_unread_through_visible_paths`.
        {
            let (mut app, mut doc) = app();
            app.layout_for_test(1280.0, 800.0);
            let id = app.shared.borrow().selected.expect("selected message");

            assert!(dispatch(&mut app, &char_key("r"), &mut doc));
            assert!(app.compose.open, "R opens the visible reply composer");
            app.compose.close();

            let was_starred = app.shared.borrow().mailbox.get(id).unwrap().starred;
            assert!(dispatch(&mut app, &char_key("s"), &mut doc));
            assert_eq!(
                app.shared.borrow().mailbox.get(id).unwrap().starred,
                !was_starred
            );
            assert_eq!(
                TestMail::get_starred(&doc, &id.to_string()),
                Some(!was_starred)
            );

            assert!(dispatch(&mut app, &char_key("u"), &mut doc));
            assert!(!app.shared.borrow().mailbox.get(id).unwrap().read);
            assert_eq!(TestMail::get_read(&doc, &id.to_string()), Some(false));
        }

        // Phase `shortcut_arrow_down_moves_selection`.
        {
            let (mut app, mut doc) = app();
            app.layout_for_test(1280.0, 800.0);
            let first = app.shared.borrow().selected;
            dispatch(&mut app, &named_key(NamedKey::ArrowDown), &mut doc);
            assert_ne!(
                app.shared.borrow().selected,
                first,
                "ArrowDown moved the selection"
            );
        }

        // Phase `shortcut_delete_trashes_selected`.
        {
            let (mut app, mut doc) = app();
            app.layout_for_test(1280.0, 800.0);
            let before = app.shared.borrow().mailbox.count(Folder::Trash);
            let id = app.shared.borrow().selected.expect("a selected message");
            dispatch(&mut app, &named_key(NamedKey::Delete), &mut doc);
            assert_eq!(
                app.shared.borrow().mailbox.count(Folder::Trash),
                before + 1,
                "Delete moved the selection to Trash"
            );
            assert_eq!(
                TestMail::get_folder_for(&doc, &id.to_string()),
                Some(Folder::Trash)
            );
        }
    }

    // ---- first-run onboarding --------------------------------------------

    // ---- add-account: single-border layout, Tab, autoconfig, OAuth --------

    /// The first accessibility node under `widget` that `matches`.
    fn find_access(
        widget: &dyn Widget,
        matches: &dyn Fn(&ui::a11y::AccessNode) -> bool,
    ) -> Option<ui::a11y::AccessNode> {
        widget
            .accessibility()
            .filter(|node| matches(node))
            .or_else(|| {
                widget
                    .children()
                    .into_iter()
                    .find_map(|child| find_access(child, matches))
            })
    }

    /// The accessibility node of the widget under `widget` keyed `key`.
    fn keyed(widget: &dyn Widget, key: &str) -> Option<ui::a11y::AccessNode> {
        find_access(widget, &|node| node.semantic_key.as_deref() == Some(key))
    }

    /// The sample app in a headless window with the account sheet open on
    /// the account at `editing` (a new one for `None`).
    fn setup_harness(
        editing: Option<usize>,
        viewport: (u32, u32),
        theme: Theme,
    ) -> ui::headless::FrameHarness {
        let mut app = MailApp::new_sample();
        app.layout_theme = theme.clone();
        app.open_setup_edit(editing);
        let mut harness =
            ui::headless::FrameHarness::for_root(ui::State::new(), app, viewport, theme);
        harness.relayout();
        // The window host focuses the sheet's first control when it opens;
        // the headless one leaves that to the first Tab.
        if harness.focused().is_none() {
            harness.key("Tab");
        }
        harness
    }

    fn focused_name(harness: &ui::headless::FrameHarness) -> String {
        harness
            .focused_access_node()
            .and_then(|node| node.name)
            .unwrap_or_default()
    }

    /// The focused control and the next `count - 1` Tab stops, by name.
    fn tab_names(harness: &mut ui::headless::FrameHarness, count: usize) -> Vec<String> {
        let mut names = vec![focused_name(harness)];
        for _ in 1..count {
            harness.key("Tab");
            names.push(focused_name(harness));
        }
        names
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn setup_sheet_is_a_form_the_keyboard_walks_at_every_scale() {
        // Phase `the_sheet_opens_on_the_address_and_tab_walks_every_field`.
        // A short window at 200% holds fewer rows than the sheet; the keyboard
        // still reaches every one.
        for (scale, viewport) in [(1.0_f32, (1100, 780)), (2.0, (1100, 560))] {
            let mut harness = setup_harness(None, viewport, text_scaled(scale));
            assert_eq!(
                tab_names(&mut harness, 3),
                ["Email", "Password", "Advanced"],
                "Tab walks what a person gives at {scale}×, the rest hidden"
            );
            harness
                .root_mut::<MailApp>()
                .setup_fields()
                .expect("the sheet is open")
                .set_advanced(true);
            harness.relayout();
            let names = tab_names(&mut harness, 10);
            assert_eq!(
                &names[1..],
                [
                    "Name",
                    "Username",
                    "Sign in with",
                    "IMAP server",
                    "IMAP port",
                    "IMAP TLS",
                    "SMTP server",
                    "SMTP port",
                    "SMTP TLS",
                ],
                "the rest follows the Advanced switch at {scale}×: {names:?}"
            );
        }

        // Phase `browser_sign_in_takes_the_password_field_s_place`.
        {
            let mut harness = setup_harness(None, (1100, 780), Theme::dark());
            harness
                .root_mut::<MailApp>()
                .set_setup_auth(crate::model::AuthMethod::OAuth);
            harness.relayout();
            let names = tab_names(&mut harness, 6);
            assert!(
                !names.iter().any(|name| name == "Password"),
                "no password field under browser sign-in: {names:?}"
            );
            assert!(
                names.iter().any(|name| name.starts_with("Sign in with ")),
                "the browser sign-in button stands in its place: {names:?}"
            );
        }

        // Phase `the_tls_switch_moves_the_port`.
        {
            let mut harness = setup_harness(None, (1100, 780), Theme::dark());
            assert_eq!(harness.root::<MailApp>().setup_value(5), "993");
            harness
                .root_mut::<MailApp>()
                .setup_fields()
                .expect("the sheet is open")
                .set_advanced(true);
            harness.relayout();
            let names = tab_names(&mut harness, 9);
            assert_eq!(names.last().map(String::as_str), Some("IMAP TLS"));
            harness
                .perform_named("mail.setup:imap-tls", &ui::a11y::Action::Activate)
                .expect("the IMAP TLS switch takes activation");
            harness.relayout();
            let app = harness.root::<MailApp>();
            assert!(
                !app.setup_tls(crate::setup_sheet::Server::Incoming),
                "TLS turned off"
            );
            assert_eq!(
                app.setup_value(5),
                "143",
                "port switched to the plaintext IMAP port"
            );
        }

        // Phase `typing_a_known_address_fills_the_servers`.
        {
            let mut harness = setup_harness(None, (1100, 780), Theme::dark());
            assert_eq!(focused_name(&harness), "Email");
            harness.type_text("me@gmail.com");
            harness.relayout();
            let app = harness.root::<MailApp>();
            assert_eq!(app.setup_value(1), "me@gmail.com");
            assert_eq!(
                app.setup_value(4),
                "imap.gmail.com",
                "servers fill as the address is typed"
            );
            assert_eq!(app.setup_value(6), "smtp.gmail.com");
            assert_eq!(
                app.setup_auth(),
                crate::model::AuthMethod::OAuth,
                "Gmail presets browser sign-in"
            );
        }

        // Phase `leaving_an_unknown_address_looks_its_servers_up`.
        {
            let mut harness = setup_harness(None, (1100, 780), Theme::dark());
            harness.type_text("me@unknown.invalid");
            harness.relayout();
            assert!(
                harness.root::<MailApp>().setup_discovery.is_none(),
                "no lookup while the address is typed"
            );
            harness.key("Tab");
            harness.relayout();
            let app = harness.root::<MailApp>();
            assert!(
                app.setup_discovery.is_some(),
                "leaving the address starts the lookup"
            );
            assert!(
                !app.setup
                    .as_ref()
                    .expect("sheet open")
                    .dialog
                    .primary_enabled(),
                "Save waits while the servers are looked up"
            );
        }

        // Phase `saving_an_incomplete_account_marks_its_fields`.
        {
            let mut harness = setup_harness(None, (1100, 780), Theme::dark());
            harness.key("Enter");
            harness.relayout();
            let app = harness.root::<MailApp>();
            assert!(app.setup_open(), "an incomplete account is not saved");
            let shown = app.setup_fields().expect("sheet open").field_errors();
            assert!(
                shown.iter().any(|(key, _)| *key == "email"),
                "the address is marked: {shown:?}"
            );
            assert!(
                app.setup_error().is_some(),
                "the error line says why it was not saved"
            );
            assert_eq!(
                focused_name(&harness),
                "Email",
                "the field keeps the keyboard while the form shows its marks"
            );
            // The marks follow the fields once shown.
            harness.type_text("me@example.org");
            harness.relayout();
            let shown = harness
                .root::<MailApp>()
                .setup_fields()
                .expect("sheet open")
                .field_errors();
            assert!(
                !shown.iter().any(|(key, _)| *key == "email"),
                "a fixed address loses its mark: {shown:?}"
            );
            assert_eq!(harness.root::<MailApp>().setup_value(1), "me@example.org");
        }

        // Phase `enter_saves_the_account`.
        // Saving checks the account through the build's default probe. On the
        // offline (default) build that is the offline probe, so the account
        // persists; the live build performs a real IMAP/SMTP probe that this
        // test provides no server for (that path is covered by the `validate`
        // state-machine tests and the live socket tests).
        #[cfg(not(feature = "live"))]
        {
            let mut harness = setup_harness(None, (1100, 780), Theme::dark());
            {
                let app = harness.root_mut::<MailApp>();
                app.set_setup_input(1, "z@host.example");
                app.set_setup_input(2, "z@host.example");
                app.set_setup_input(4, "imap.host.example");
                app.set_setup_input(6, "smtp.host.example");
            }
            harness.relayout();
            let before = harness.root::<MailApp>().shared.borrow().accounts.len();
            harness.key("Enter");
            assert!(
                harness.wait_until(ui::time::Duration::from_secs(10), |harness| {
                    !harness.root::<MailApp>().setup_open()
                }),
                "Enter saves the account and closes the sheet"
            );
            assert_eq!(
                harness.root::<MailApp>().shared.borrow().accounts.len(),
                before + 1
            );
        }
    }

    /// The stock dark theme at `scale`× text.
    fn text_scaled(scale: f32) -> Theme {
        Theme::dark().with_text_scale(scale)
    }

    #[test]
    fn attachment_save_offers_only_the_downloaded_bytes_and_reports_truthfully() {
        let (mut app, _doc) = app();
        let (message_id, index, filename, bytes) = {
            let state = app.shared.borrow();
            let (message_id, (index, attachment)) = state
                .mailbox
                .messages
                .values()
                .find_map(|message| {
                    message
                        .attachments
                        .iter()
                        .enumerate()
                        .find(|(_, attachment)| attachment.is_downloadable())
                        .map(|found| (message.id, found))
                })
                .expect("sample has a hydrated attachment");
            (
                message_id,
                index,
                attachment.filename.clone(),
                attachment.content.clone(),
            )
        };

        // What the host is asked to save is exactly the downloaded bytes,
        // under a name that cannot reach outside the chosen folder.
        let (name, offered) = app
            .attachment_to_save(&message_id.to_string(), index)
            .expect("a downloaded attachment can be saved");
        assert_eq!(offered, bytes);
        assert_eq!(name, MailApp::attachment_save_name(&filename));
        assert!(
            app.attachment_to_save(&message_id.to_string(), index + 100)
                .is_err(),
            "an attachment that is not loaded cannot be saved"
        );
        assert!(
            app.start_effect(
                tilo_actions::InvocationId::new(),
                &crate::actions::effect_payload(&crate::actions::MailEffect::Save {
                    message: message_id.to_string(),
                    index,
                }),
            )
            .is_err(),
            "only the surface, with the host's chooser, saves an attachment"
        );

        // A save is reported only once the host wrote it.
        let resource = format!("mail.attachment:{message_id}:{index}");
        let saved = app.finish_attachment_save(true, &name, resource.clone());
        assert!(matches!(
            saved,
            ui::framework::Effected::Done(output) if output.resources == vec![resource.clone()]
        ));
        assert_eq!(
            app.shared.borrow().status.as_deref(),
            Some(format!("Saved {name}").as_str())
        );
        let cancelled = app.finish_attachment_save(false, &name, resource);
        assert!(matches!(
            cancelled,
            ui::framework::Effected::Done(output) if output.resources.is_empty()
        ));
        assert_eq!(
            app.shared.borrow().status.as_deref(),
            Some("Attachment save cancelled")
        );

        assert_eq!(
            MailApp::attachment_save_name("../../secret.pdf"),
            "secret.pdf"
        );
        assert_eq!(MailApp::attachment_save_name(".."), "attachment");
        assert_eq!(MailApp::attachment_save_name("\0bad"), "bad");
    }

    // ---- test-before-save state machine (persist on success only) --------

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn setup_verification_persists_only_the_verified_snapshot() {
        // Phase `closing_setup_cancels_pending_verification_and_discards_late_results`.
        {
            let (mut app, mut doc) = app();
            app.open_setup_edit(None);
            let accounts_before = app.shared.borrow().accounts.accounts.len();
            let (sender, receiver) = std::sync::mpsc::channel();
            let cancellation = Arc::new(std::sync::atomic::AtomicBool::new(false));
            app.setup_validation = Some(PendingSetupValidation {
                config: app.read_setup_config(),
                editing: None,
                receiver,
                cancellation: Arc::clone(&cancellation),
            });
            let deadline = app.next_wake_deadline().unwrap();
            assert_eq!(app.next_wake_deadline(), Some(deadline));
            app.close_setup();
            assert!(cancellation.load(std::sync::atomic::Ordering::Relaxed));
            assert!(sender.send(crate::validate::ValidationState::Ok).is_err());
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            assert_eq!(app.shared.borrow().accounts.accounts.len(), accounts_before);
            assert!(app.setup_validation.is_none());
            assert_eq!(app.next_wake_deadline(), app.nav.next_wake_deadline());
        }

        // Phase `edited_setup_cannot_persist_an_earlier_successful_verification`.
        {
            let (mut app, doc) = app();
            app.open_setup_edit(Some(0));
            let original = app.shared.borrow().accounts.accounts[0].config.clone();
            let (sender, receiver) = std::sync::mpsc::channel();
            app.setup_validation = Some(PendingSetupValidation {
                config: app.read_setup_config(),
                editing: Some(0),
                receiver,
                cancellation: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            });
            app.set_setup_input(4, "changed-after-verification.example");
            sender.send(crate::validate::ValidationState::Ok).unwrap();
            app.sync(&doc, &automerge::ROOT);
            assert!(app.setup_open());
            assert!(
                app.setup_error()
                    .as_deref()
                    .unwrap()
                    .contains("Settings changed")
            );
            assert_eq!(app.shared.borrow().accounts.accounts[0].config, original);
            assert!(app.setup_validation.is_none());
        }

        // Phase `disconnected_setup_worker_keeps_the_sheet_open_without_saving`.
        {
            let (mut app, doc) = app();
            app.open_setup_edit(Some(0));
            let original = app.shared.borrow().accounts.accounts[0].config.clone();
            let (sender, receiver) = std::sync::mpsc::channel();
            app.setup_validation = Some(PendingSetupValidation {
                config: app.read_setup_config(),
                editing: Some(0),
                receiver,
                cancellation: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            });
            drop(sender);
            app.sync(&doc, &automerge::ROOT);
            assert!(app.setup_open());
            assert!(
                app.setup_error()
                    .as_deref()
                    .unwrap()
                    .contains("without a result")
            );
            assert_eq!(app.shared.borrow().accounts.accounts[0].config, original);
            assert!(app.setup_validation.is_none());
        }

        // Phase `successful_setup_worker_persists_only_its_verified_snapshot`.
        {
            let (mut app, doc) = app();
            app.open_setup_edit(Some(0));
            app.set_setup_input(0, "Verified fixture account");
            let verified = app.read_setup_config();
            let root = app.accounts_persist_root().unwrap();
            let (sender, receiver) = std::sync::mpsc::channel();
            app.setup_validation = Some(PendingSetupValidation {
                config: verified.clone(),
                editing: Some(0),
                receiver,
                cancellation: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            });
            sender.send(crate::validate::ValidationState::Ok).unwrap();
            app.sync(&doc, &automerge::ROOT);
            assert!(!app.setup_open());
            assert!(app.setup_validation.is_none());
            assert!(app.setup_error().is_none());
            let restored = AccountStore::load_in(&root);
            assert_eq!(
                restored
                    .accounts
                    .iter()
                    .find(|account| account.id == verified.email)
                    .unwrap()
                    .config,
                verified,
            );
        }

        // Phase `save_persists_only_when_verification_succeeds`.
        {
            let (mut app, mut doc) = app();
            app.open_setup_edit(None);
            app.set_setup_input(1, "new@host.example");
            app.set_setup_input(2, "new@host.example");
            app.set_setup_input(4, "imap.host.example");
            app.set_setup_input(6, "smtp.host.example");
            let before = app.shared.borrow().accounts.len();

            // Failing verification: no persist, sheet stays open, error surfaced.
            app.save_setup_with(
                &mut ui::widget::Edits::lend(&mut doc),
                &FixedProbe { imap_ok: false },
            );
            assert_eq!(
                app.shared.borrow().accounts.len(),
                before,
                "a failed IMAP check must not persist the account"
            );
            assert!(
                app.setup_open(),
                "the sheet stays open on a failed verification"
            );
            assert!(
                app.setup_error().as_deref().unwrap_or("").contains("IMAP"),
                "a specific IMAP error is shown"
            );

            // Succeeding verification: account saved, sheet closes.
            app.save_setup_with(
                &mut ui::widget::Edits::lend(&mut doc),
                &FixedProbe { imap_ok: true },
            );
            assert_eq!(
                app.shared.borrow().accounts.len(),
                before + 1,
                "a verified account persists"
            );
            assert!(!app.setup_open(), "the sheet closes on success");
            assert!(app.setup_error().is_none());
        }

        // Phase `setup_verification_failure_is_exposed_to_assistive_technology`.
        {
            let (mut app, mut doc) = app();
            app.open_setup_edit(None);
            app.set_setup_input(1, "new@host.example");
            app.set_setup_input(2, "new@host.example");
            app.set_setup_input(4, "imap.host.example");
            app.set_setup_input(6, "smtp.host.example");
            app.save_setup_with(
                &mut ui::widget::Edits::lend(&mut doc),
                &FixedProbe { imap_ok: false },
            );
            let dialog = app.accessibility().expect("setup dialog stays available");
            assert_eq!(dialog.role, ui::a11y::Role::Dialog);
            // The form shows the new error once it syncs.
            app.sync(&doc, &automerge::ROOT);
            let sheet = &app.setup.as_ref().expect("sheet stays open").dialog;
            assert!(
                find_access(sheet, &|node| {
                    node.name.as_deref() == Some("IMAP login failed: 535 auth rejected")
                        && node.live == Some(ui::a11y::Politeness::Polite)
                })
                .is_some(),
                "a visible verification error must also be announced in the setup dialog"
            );
        }
    }

    struct FixedProbe {
        imap_ok: bool,
    }
    impl crate::validate::ProbeTransport for FixedProbe {
        fn imap_login(&self, _cfg: &AccountConfig) -> Result<(), String> {
            if self.imap_ok {
                Ok(())
            } else {
                Err("535 auth rejected".into())
            }
        }
        fn smtp_handshake(&self, _cfg: &AccountConfig) -> Result<(), String> {
            Ok(())
        }
    }

    fn pending_discovery(
        app: &mut MailApp,
    ) -> (
        std::sync::mpsc::Sender<Option<crate::autoconfig::Autoconfig>>,
        Arc<std::sync::atomic::AtomicBool>,
    ) {
        let (sender, receiver) = std::sync::mpsc::channel();
        let cancellation = Arc::new(std::sync::atomic::AtomicBool::new(false));
        app.setup_discovery = Some(PendingSetupDiscovery {
            config: app.read_setup_config(),
            editing: app.setup_editing,
            receiver,
            cancellation: Arc::clone(&cancellation),
        });
        (sender, cancellation)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn discovery_results_apply_only_to_the_current_sheet_and_custody_failures_keep_setup_open() {
        // Phase `discovery_applies_through_sync_without_overwriting_manual_server_edits`.
        {
            let (mut app, doc) = app();
            app.open_setup_edit(None);
            app.set_setup_input(1, "fixture@custom.invalid");
            let (sender, _) = pending_discovery(&mut app);
            let deadline = app.next_wake_deadline().unwrap();
            assert_eq!(app.next_wake_deadline(), Some(deadline));
            app.set_setup_input(4, "manual.imap.invalid");
            app.set_setup_input(5, "1993");
            app.set_setup_input(3, "user-entered-secret");
            let discovered = crate::autoconfig::builtin_autoconfig("fastmail.com").unwrap();
            sender.send(Some(discovered.clone())).unwrap();
            app.sync(&doc, &automerge::ROOT);
            assert_eq!(app.setup_value(4), "manual.imap.invalid");
            assert_eq!(app.setup_value(5), "1993");
            assert_eq!(app.setup_value(3), "user-entered-secret");
            assert_eq!(app.setup_value(6), discovered.smtp_host);
            assert!(app.setup_discovery.is_none());
            assert_eq!(app.next_wake_deadline(), app.nav.next_wake_deadline());
        }

        // Phase `discovery_for_a_previous_address_cannot_fill_the_current_sheet`.
        {
            let (mut app, doc) = app();
            app.open_setup_edit(None);
            app.set_setup_input(1, "old@custom.invalid");
            let (sender, _) = pending_discovery(&mut app);
            app.set_setup_input(1, "new@other.invalid");
            sender
                .send(crate::autoconfig::builtin_autoconfig("fastmail.com"))
                .unwrap();
            app.sync(&doc, &automerge::ROOT);
            assert!(app.setup_value(4).is_empty());
            assert!(app.setup_value(6).is_empty());
            assert_eq!(app.setup_value(1), "new@other.invalid");
        }

        // Phase `closing_discovery_cancels_worker_and_reopening_rejects_its_result`.
        {
            let (mut app, doc) = app();
            app.open_setup_edit(None);
            app.set_setup_input(1, "fixture@custom.invalid");
            let (sender, cancellation) = pending_discovery(&mut app);
            app.close_setup();
            assert!(cancellation.load(std::sync::atomic::Ordering::Relaxed));
            app.open_setup_edit(None);
            assert!(
                sender
                    .send(crate::autoconfig::builtin_autoconfig("fastmail.com"))
                    .is_err()
            );
            app.sync(&doc, &automerge::ROOT);
            assert!(app.setup_value(4).is_empty());
            assert!(app.setup_discovery.is_none());
        }

        // Phase `account_custody_failure_keeps_setup_open_and_live_accounts_unchanged`.
        {
            let (mut app, mut doc) = app();
            let root = app
                .accounts_persist_root()
                .expect("isolated account fixture");
            let before = app.shared.borrow().accounts.clone();
            before.persist_in(&root).unwrap();
            let metadata_path = accounts::store_path_in(&root);
            let original_metadata = std::fs::read(&metadata_path).unwrap();
            let vault_path = accounts::vault_dir_in(&root).join("vault.enc");
            std::fs::write(&vault_path, b"damaged fixture vault").unwrap();

            app.open_setup_edit(None);
            app.set_setup_input(1, "new@host.example");
            app.set_setup_input(2, "new@host.example");
            app.set_setup_input(4, "imap.host.example");
            app.set_setup_input(6, "smtp.host.example");
            app.save_setup_with(
                &mut ui::widget::Edits::lend(&mut doc),
                &FixedProbe { imap_ok: true },
            );
            assert!(app.setup_open());
            assert!(
                app.setup_error()
                    .as_deref()
                    .unwrap()
                    .contains("Couldn’t save account")
            );
            assert_eq!(app.shared.borrow().accounts, before);
            assert!(app.pending_engine_requests.is_empty());

            app.open_setup_edit(Some(0));
            app.remove_editing_account(&mut ui::widget::Edits::lend(&mut doc));
            assert!(app.setup_open());
            assert!(
                app.setup_error()
                    .as_deref()
                    .unwrap()
                    .contains("Couldn’t remove account")
            );
            assert_eq!(app.shared.borrow().accounts, before);
            assert_eq!(std::fs::read(metadata_path).unwrap(), original_metadata);
            assert_eq!(std::fs::read(vault_path).unwrap(), b"damaged fixture vault");
        }
    }
}
