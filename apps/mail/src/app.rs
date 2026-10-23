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
//! On top of the shell the app layers what it owns directly: the
//! [`ComposeWindow`], which writes in the message column (or fills a window
//! too narrow for one), and the row context menu. Status messages show as
//! the shell's toast. Accounts are not edited here: they are kept in
//! Settings' External Accounts, which Mail links to and reads.
//!
//! A live window loads the [`AccountStore`] of its host's profile and drives
//! the account [`crate::engine`] worker (IMAP fetch / SMTP submit). It does
//! **not** fabricate a sample mailbox — an unconfigured account shows an empty
//! state. The bundled sample corpus is a screenshot's fixture.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
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
#[path = "contact_recipients.rs"]
mod contact_recipients;

use crate::compose::{ComposeAction, ComposeEventContext, ComposeWindow};
use crate::engine::{EngineEvent, EngineRequest, EngineRequestId, EngineRequestKind};
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
use crate::window_bar::BarItem;

/// How often the app auto-refreshes the current folder from the server while in
/// use. Checked each active frame; a manual refresh (the `r` shortcut) forces
/// one immediately regardless.
const AUTO_REFRESH_MIN: i64 = 5;
/// How long a search's text has to stay the same before the servers are
/// asked for it.
const SERVER_SEARCH_DEBOUNCE_MS: u64 = 500;
/// The shortest text the servers are searched for.
const SERVER_SEARCH_MIN_CHARS: usize = 3;
/// How long a message sent from this window waits before it goes to the
/// server, so "Undo" on the sending notice takes it back unsent.
const SEND_UNDO_SECS: u64 = 10;

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
    /// The reader's menu instead: what each of its rows chooses (`None`
    /// for a caption).
    reader: Vec<Option<crate::state::ReaderChoice>>,
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

/// A message sent from this window, held back from the server until `due`
/// so it can be taken back unsent.
struct HeldSend {
    outgoing: Outgoing,
    done: DeliveryDone,
    due: ui::time::Instant,
}

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
/// A wake for a worker thread: it wakes the window once a live surface has
/// handed over its waker, and does nothing before.
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

/// What a failed engine request says to the person: what did not happen,
/// and why in their terms, never the protocol step that failed.
fn engine_error_text(context: &str, message: &str, account: &str) -> String {
    let lower = message.to_ascii_lowercase();
    let signed_out = lower.contains("authentication")
        || lower.contains("password")
        || lower.contains("credential")
        || lower.contains("oauth")
        || lower.contains("sign in");
    let offline = lower.contains("network unreachable");
    let account = if account.is_empty() {
        "your account"
    } else {
        account
    };
    match context {
        "connect" | "fetch" if signed_out => {
            format!(
                "Can’t sign in to {account}. Check its password in Settings, External Accounts."
            )
        }
        "connect" | "fetch" if offline => "You’re offline. Mail will check again.".to_owned(),
        "connect" | "fetch" => format!("Can’t check mail for {account}: {message}"),
        "send" | "smtp" => format!("Couldn’t send the message: {message}"),
        "oauth" => format!("Sign-in didn’t finish: {message}"),
        _ => format!("Couldn’t update the mail server: {message}"),
    }
}

/// Where email accounts are added, changed and removed: Settings' External
/// Accounts, the one account editor.
const ACCOUNT_SETTINGS_LINK: &str = "settings://external_accounts";

/// The scheme of the activation URIs Mail's notifications carry.
const NOTIFICATION_SCHEME: &str = "tilo-mail";
/// The one tag new-mail notifications share: a newer one replaces the last.
const NOTIFICATION_TAG: &str = "mail-new";

fn profile_config_root(storage: &ui::storage::AppStorage) -> Option<std::path::PathBuf> {
    match storage.settings_document() {
        ui::storage::SettingsDocument::Session => accounts::config_root(),
        ui::storage::SettingsDocument::File(path) => {
            path.parent().map(std::path::Path::to_path_buf)
        }
    }
}

/// A request posted to the engine and not yet acknowledged: what it does
/// and the account it acts for, so a late answer stays with that account.
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
    /// Where draft attachment bytes live; drafts carry only their content id.
    /// The engine worker holds a clone to build outgoing MIME.
    content: ContentStore,
    nav: NavShell,
    nav_node: Option<NodeId>,
    /// The window bar's actions as last declared, and the command each
    /// queues.
    bar: Vec<BarItem>,
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
    /// Messages sent from this window, waiting out the undo time.
    held_sends: Vec<HeldSend>,
    /// Sends pressed whose outbox messages have yet to reach the drain:
    /// each is held when it does.
    sends_to_hold: usize,
    /// The notice showing (or about to) is the sending one: its "Undo"
    /// takes the held messages back.
    send_notice: bool,
    sending_notice_next: bool,
    /// "Undo" was pressed on the sending notice.
    unsend: bool,
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
    /// The contact directory revision and own addresses the sender pictures
    /// were last taken from.
    sender_pictures_seen: Option<(u64, Vec<String>)>,
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
    /// Wakes the window from the engine's and the account sheet's worker
    /// threads, so a result is drawn the moment it arrives and nothing
    /// polls while the network is slow. `None` until a live surface
    /// attaches; the workers then fall back to the frame poll.
    waker: Arc<std::sync::Mutex<Option<ui::WakeHandle>>>,
    /// What the document and the mailbox were when their flags were last
    /// brought together, so an unchanged frame does not read the record.
    hydrated_at: RefCell<Option<(Vec<ui::automerge::ChangeHash>, usize, u64)>>,
    pending_engine_requests: HashMap<EngineRequestId, PendingEngineRequest>,
    /// The one refresh invocation currently owned by the resource-operation
    /// host.  It remains occupied until the host has polled its terminal
    /// update, so a subsequent refresh cannot overwrite an unobserved result.
    resource_refresh_invocation: Option<(EngineRequestId, tilo_actions::InvocationId)>,
    resource_refresh_result: Option<tilo_actions::InvocationUpdate>,
    /// One portal-open attachment staging request. The returned descriptor is
    /// copied straight into the content store; its original path is never retained.
    compose_attachment_rx: Option<OpenFileChannel>,
    /// The file to attach, as its contents, where files have no paths to
    /// open (a browser page).
    compose_attachment_contents: Option<ui::portal::ContentsChannel>,
    /// What effects asked to show that needs the document, for the next sync.
    reveals: Vec<Reveal>,
    /// What search hits and agents asked to open, taken up on the next
    /// settle.
    opens: std::rc::Rc<std::cell::RefCell<Vec<crate::actions::MailTarget>>>,
    /// Callers waiting for a refresh to settle.
    effect_replies: Vec<(tilo_actions::InvocationId, ui::framework::EffectReply)>,
    /// Where fetched mail is kept on this device.
    store: crate::store::Store,
    /// Text to search the servers for once the person stops typing it, and
    /// when; and the text last searched for.
    server_search: Option<(String, ui::time::Instant)>,
    server_searched: String,
    /// An attachment handed to another app, until the host says whether it
    /// opened.
    attachment_opening: Option<(String, ui::portal::LaunchChannel)>,
    /// Accounts whose watched inbox changed: fetched once the engine's
    /// events are drained.
    pending_watch_fetches: Vec<AccountConfig>,
    /// Whether each account's folder has older mail on its server than the
    /// oldest message held, as the last sync of it found.
    has_older: HashMap<(String, Folder), bool>,
    /// The folders whose older page is being fetched, by request.
    older_pending: HashMap<(String, Folder), EngineRequestId>,
    /// Whether the kept mail the window opened on is registered with the
    /// mailbox record.
    kept_registered: bool,
    /// The accounts whose kept mail is in the mailbox.
    kept_loaded: std::collections::HashSet<String>,
    /// The messages whose notification the person clicked, from whatever
    /// thread the host reported it on; opened on the next settle.
    activations: Arc<std::sync::Mutex<Vec<String>>>,
    /// The host's notifications, once a live surface is attached.
    notifier: Option<Arc<dyn ui::notify::Notifier>>,
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
    /// The links the app opened, in order, where a test reads them.
    #[cfg(test)]
    opened_links: Vec<&'static str>,
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

    /// Let the engine and the account sheet's workers wake the window.
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

    /// Say that new mail arrived: who sent it and what it is about, or how
    /// many messages came. Clicking it opens the newest.
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
                (format!("{} new messages", many.len()), senders.join(", "))
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

    /// Open the messages whose notifications the person clicked.
    fn open_activated(&mut self) {
        let activated = std::mem::take(
            &mut *self
                .activations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for id in activated {
            if let Err(error) = self.open_target(crate::actions::MailTarget::Message(id)) {
                tracing::debug!(%error, "mail: a notification's message could not be opened");
            }
        }
    }

    /// Whether workers can wake the window, so nothing has to poll them.
    fn workers_wake(&self) -> bool {
        self.waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
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
        let shared = MailState::shared(MailState::new_live(
            accounts,
            crate::avatars::Avatars::new(),
        ));
        let content = content.unwrap_or_else(ContentStore::memory);
        let waker: Arc<std::sync::Mutex<Option<ui::WakeHandle>>> = Arc::default();
        let (tx, rx) = crate::engine::spawn(content.clone(), wake_through(&waker));
        let mut app = Self::assemble(shared, content, Some(EngineHandle { tx, rx }), persist);
        app.waker = waker;
        // Fetched mail is kept in the app's cache area; a host without one
        // fetches every time.
        app.store = storage.files().paths().cache().native_path().map_or_else(
            |_| crate::store::Store::nowhere(),
            |cache| crate::store::Store::at(cache.join("mail")),
        );
        app.shared.borrow_mut().kept = app.store.clone();
        app.open_kept_mail();
        app
    }

    /// A past version in History: what the document holds, and nothing that
    /// reaches a mail server or the account store.
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

    /// A screenshot: the scenario's fixture mailbox and accounts, no engine,
    /// a pinned clock and nothing written outside the document.
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

    /// The sample mailbox, editing the document it is synced with and
    /// persisting account changes into a sandbox.
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

    /// No account configured yet, persisting into a sandbox.
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
        Self {
            shared,
            content,
            nav,
            nav_node: None,
            bar: Vec::new(),
            first_run: EmptyState::new("No email account yet")
                .with_icon(Icon::Mail)
                .with_message("Email accounts are added in Settings, under External Accounts.")
                .with_action("Open External Accounts")
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
            engine,
            server_outbox: Rc::default(),
            projection: None,
            delivery: None,
            pending_send: HashMap::new(),
            resending: None,
            held_sends: Vec::new(),
            sends_to_hold: 0,
            send_notice: false,
            sending_notice_next: false,
            unsend: false,
            accounts_persist,
            persisted_active_account: None,
            person_accounts: None,
            contact_directory: None,
            sender_pictures_seen: None,
            has_contact_draft_conflict: false,
            preferences: None,
            followed_preferences: crate::resource::MailPreferences::default(),
            connect_requested: false,
            awaiting_engine: false,
            background_poll_deadline: Cell::new(None),
            waker: Arc::default(),
            hydrated_at: RefCell::new(None),
            pending_engine_requests: HashMap::new(),
            resource_refresh_invocation: None,
            resource_refresh_result: None,
            compose_attachment_rx: None,
            compose_attachment_contents: None,
            reveals: Vec::new(),
            opens: std::rc::Rc::default(),
            effect_replies: Vec::new(),
            last_auto_refresh_min: 0,
            store: crate::store::Store::nowhere(),
            pending_watch_fetches: Vec::new(),
            attachment_opening: None,
            server_search: None,
            server_searched: String::new(),
            has_older: HashMap::new(),
            older_pending: HashMap::new(),
            kept_registered: true,
            kept_loaded: std::collections::HashSet::new(),
            activations: Arc::default(),
            notifier: None,
            rect: Rect::ZERO,
            layout_theme: Theme::dark(),
            first_layout: true,
            pending_stage: None,
            dirty: true,
            #[cfg(test)]
            test_layout: None,
            #[cfg(test)]
            opened_links: Vec::new(),
        }
    }

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
            "html_mail" => self.stage_html_mail(doc),
            gallery if GALLERY_SCENARIOS.iter().any(|(name, _)| *name == gallery) => {
                if let Some((_, sender)) =
                    GALLERY_SCENARIOS.iter().find(|(name, _)| *name == gallery)
                {
                    self.stage_open_from(doc, sender);
                }
            }
            "search" => self
                .search
                .open(SearchRequest::this_app().with_query("launch")),
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
                // A first fetch for an account with nothing kept yet.
                let mut st = self.shared.borrow_mut();
                // Staged a second in, so the shot shows the rows' skeleton.
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
                self.shared.borrow_mut().status =
                    Some("You’re offline · Showing downloaded mail".into());
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// The newest message from `sender` open in the reader.
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
    fn post_engine(&mut self, mut request: EngineRequest) -> Option<EngineRequestId> {
        // A folder sync asks only for what is new to what this device holds.
        if let EngineRequest::Fetch {
            account,
            folder,
            want: crate::sync::Want::Latest { known },
            ..
        } = &mut request
            && known.is_none()
        {
            *known =
                crate::sync::Window::of(&self.shared.borrow().mailbox, &account.email, *folder);
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
            self.shared.borrow_mut().status = Some("Mail couldn’t reach the mail server".into());
            self.dirty = true;
            return None;
        }
        self.pending_engine_requests.insert(request_id, pending);
        self.awaiting_engine = true;
        Some(request_id)
    }

    /// Accept only events that match a request still pending in this app. A
    /// worker event is untrusted transport input: a malformed, delayed, or
    /// out-of-order result must not update a different operation's UI state.
    fn expects_engine_event(&self, request_id: EngineRequestId, kind: EngineRequestKind) -> bool {
        self.pending_engine_requests
            .get(&request_id)
            .is_some_and(|pending| pending.kind == kind)
    }

    /// The window shell: the sidebar of accounts and folders, the message
    /// list and the open message. Its toolbar is the window bar's slots
    /// ([`crate::window_bar`]).
    fn build_shell(shared: &Shared) -> NavShell {
        NavShell::new()
            .content_kind(ui::widget::ContentKind::Reading)
            .with_id("mail-shell")
            .sidebar(Box::new(sidebar::sidebar(shared)))
            .overview(Box::new(MessageList::new(shared.clone())))
            .content(Box::new(MessageReader::new(shared.clone())))
            .with_window_toolbar()
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

    /// Declare the window bar's slots: the shell's own, the folder and
    /// what can be done there ([`crate::window_bar`]).
    fn declare_window_toolbar(&mut self) {
        let first_run = self.is_first_run();
        let (items, toolbar) = {
            let state = self.shared.borrow();
            let composing = self.compose.open.then(|| self.compose.heading());
            let items = crate::window_bar::bar_items(&state, first_run, composing.is_some());
            let toolbar = crate::window_bar::toolbar(
                self.nav.window_toolbar(),
                &state,
                &items,
                first_run,
                composing,
            );
            (items, toolbar)
        };
        self.bar = items;
        ui::window_toolbar::declare(Some(toolbar));
    }

    /// Queue what the person did in the window's bar and apply it with the
    /// document in hand, as the chrome's commands are applied.
    fn apply_window_toolbar(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let mut queued = false;
        for event in ui::window_toolbar::take_events() {
            if self.nav.window_toolbar_event(&event) {
                self.dirty = true;
                continue;
            }
            if let ui::window_toolbar::ToolbarEvent::Action { id } = &event
                && let Some(item) = self.bar.iter().find(|item| item.action.id == *id)
            {
                self.shared.borrow_mut().push_command(item.command.clone());
                queued = true;
            }
        }
        if queued {
            self.apply_commands(doc);
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
        if self.compose_attachment_rx.is_some() || self.compose_attachment_contents.is_some() {
            self.shared.borrow_mut().status = Some("Attachment picker is already open".into());
            return;
        }
        let request = PickRequest::open("Attach file", "Attach")
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
        self.shared.borrow_mut().status = Some("Choose a file to attach".into());
        self.dirty = true;
    }

    /// Drain descriptor-only portal selection. A cancel/failure/disconnect is
    /// terminal and leaves the draft unchanged; staged content is verified by
    /// the content store before metadata becomes visible in the composer.
    fn drain_compose_attachment(&mut self) {
        self.drain_compose_attachment_contents();
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

    /// Drain a contents selection the way [`Self::drain_compose_attachment`]
    /// drains a descriptor one.
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
                    "Attachment selection cancelled because the composer was closed".into()
                }
                Some(file) => {
                    let name = file.name;
                    match crate::attachment_store::stage_bytes(&self.content, &file.bytes, &name)
                        .and_then(|attachment| self.compose.add_attachment(attachment))
                    {
                        Ok(()) => format!("Attached {name}"),
                        Err(error) => format!("Couldn’t attach {name}: {error}"),
                    }
                }
                None => "Attachment selection cancelled".into(),
            },
            Ok(ContentsOutcome::Cancelled) => "Attachment selection cancelled".into(),
            Ok(ContentsOutcome::Failed(error)) => {
                format!("Couldn’t open attachment picker: {error}")
            }
            Err(TryRecvError::Disconnected) => {
                "Attachment picker disconnected before selecting a file".into()
            }
        };
        self.compose_attachment_contents = None;
        self.shared.borrow_mut().status = Some(status);
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
                Intent::ReaderMenu(id, at, choices) => self.open_reader_menu(id, at, choices),
                Intent::OpenLink(url) => open_link(&url),
                Intent::SaveAttachment(message_id, index) => {
                    self.request_attachment_save(message_id, index);
                }
                Intent::OpenAttachment(message_id, index) => {
                    self.open_attachment(message_id, index);
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
                    self.open_contact_recipients();
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
            }
        }
        self.process_intents();
    }

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

    /// The reader's menu on message `id` at `at`: the link under the
    /// pointer, captioned with the address it really opens, then copying
    /// and the message's format.
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

    /// The quoted original under a reply: who wrote it when, and all of
    /// it, every line marked as quoted.
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

    /// A reply goes from the account the answered mail came to, whichever
    /// account is open (every inbox shows together).
    fn answer_from_its_account(&mut self, id: MessageId) {
        let owner = self.shared.borrow().account_of(id);
        if let Some(account) = owner {
            self.compose.send_from(account.email);
        }
    }

    /// The account the composer's message goes from: the one it was set to
    /// go from, otherwise the open one.
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

    /// Keep what was typed into the recipients and subject in the draft, a
    /// moment after the typing stops, as the body is kept while it is
    /// written: a draft survives the window closing or the app quitting.
    fn keep_compose_headers(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(headers) = self.compose.take_due_headers() else {
            return;
        };
        self.store_compose_headers(doc, headers);
    }

    /// Keep `to`, `cc`, `bcc` and `subject` in the draft where they differ
    /// from what it holds.
    fn store_compose_headers(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        [to, cc, bcc, subject]: [String; 4],
    ) {
        let kept = read_mail(doc.doc()).unwrap_or_default().draft;
        let draft = Mail::paths().draft;
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

    /// The composer closed: what the person wrote stays a draft; a message
    /// they never touched leaves nothing behind.
    fn close_compose(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if self.compose.touched() {
            self.save_compose_draft(doc);
        }
        self.dirty = true;
    }

    /// Throw the message away: the draft and its row in Drafts go.
    fn discard_compose(&mut self, doc: &mut ui::widget::Edits<'_>) {
        self.shared
            .borrow()
            .submit(doc, Mail::DISCARD_DRAFT.with(mailbox::DiscardDraft {}));
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

    /// Reply to everyone: the original sender in `To`, and every other original
    /// recipient (To + Cc, minus ourselves and the sender) in `Cc`.
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
            self.shared.borrow_mut().status = Some(left_behind.join("\n"));
        }
        self.close_menu();
        self.dirty = true;
    }

    /// The files a forward of `msg` carries, kept for the draft, and the
    /// names of those it cannot.
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
                .attachment_to_save(&msg.id.to_string(), index)
                .and_then(|(_, bytes)| {
                    crate::attachment_store::stage_bytes(
                        &self.content,
                        &bytes,
                        &attachment.filename,
                    )
                });
            match staged {
                Ok(file) => files.push(file),
                Err(_) => not_downloaded.push(attachment.filename.clone()),
            }
        }
        let carried = crate::attachment_store::bounded(&files);
        // A file whose bytes another one carries already goes with it.
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

    /// Send the message the composer holds: it goes into the mailbox's
    /// outbox as an action — undone like any edit until it is delivered —
    /// and a device that holds its account delivers it.
    fn send_compose(&mut self, doc: &mut ui::widget::Edits<'_>) {
        self.store_preferences_if_changed();
        let draft = self.compose.to_draft(doc);
        let acct = self.sending_account();
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
        if [&draft.to, &draft.cc, &draft.bcc]
            .into_iter()
            .all(|field| crate::transport::parse_addr_list(field).is_empty())
        {
            self.shared.borrow_mut().status = Some("Add a recipient before sending".into());
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
        self.shared.borrow_mut().say_undoable("Sending…");
        self.dirty = true;
    }

    /// Hand the held messages whose undo time is up to the engine. One that
    /// left the outbox meanwhile (undone) comes back to the composer.
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

    /// "Undo" on the sending notice: the held messages leave the outbox
    /// unsent, and the composer opens on them again.
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

    /// Open the composer on `outgoing`, taken back unsent.
    fn reopen_unsent(&mut self, outgoing: &Outgoing) {
        if !self.compose.open {
            self.compose.open_with_draft(&outgoing.to_draft());
        }
        self.dirty = true;
    }

    fn save_compose_draft(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let draft = self.compose.to_draft(doc);
        // Persist the draft as an action (survives a crash / syncs to peers /
        // one undo boundary) …
        self.shared.borrow_mut().save_draft(doc, &draft);
        // … and materialise it as a real, visible message in the Drafts folder
        // so it can be found and reopened — not a write-only slot.
        let id = self.upsert_draft_message(doc, &draft, self.editing_draft);
        self.editing_draft = Some(id);
        // The composer closes: say where the draft went.
        self.shared.borrow_mut().status = Some("Saved to Drafts".into());
        self.compose.close();
        self.store_preferences_if_changed();
        self.dirty = true;
    }

    /// Insert (or, when editing the Drafts message `edited`, update it in
    /// place) a Drafts folder message reflecting `draft`, and return its id.
    /// The id is kept with the draft, so every device and every restart
    /// shows it as the same message.
    fn upsert_draft_message(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        draft: &Draft,
        edited: Option<MessageId>,
    ) -> MessageId {
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
                }),
            );
        }
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
            account: String::new(),
            server: None,
        };
        {
            let mut st = self.shared.borrow_mut();
            st.mailbox.insert(msg);
            st.register_message_target(doc, &id.to_string());
        }
        *self.projected_draft.borrow_mut() = Some((id, draft.clone()));
        id
    }

    /// Whether the app is at first run (no account configured) and should show
    /// the onboarding screen rather than an empty mailbox.
    fn is_first_run(&self) -> bool {
        self.shared.borrow().accounts.is_empty()
    }

    /// Tell the composer whether there is an account to send from: Send
    /// stays disabled until there is one (and a recipient).
    fn tell_compose_the_accounts(&mut self) {
        let has_account = !self.shared.borrow().accounts.is_empty();
        self.compose.set_has_account(has_account);
    }

    /// Take the person to where their accounts are kept: Settings' External
    /// Accounts, the one place an account is added, changed or removed.
    /// Mail reads the accounts from there and edits none itself.
    fn open_account_settings(&mut self) {
        #[cfg(test)]
        self.opened_links.push(ACCOUNT_SETTINGS_LINK);
        #[cfg(not(test))]
        ui::widget::open_external_url(ACCOUNT_SETTINGS_LINK);
        self.close_menu();
        self.dirty = true;
    }

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
            // Checking for mail is routine: new mail appearing in the list
            // is all it shows.
            self.last_auto_refresh_min = self.shared.borrow().now_min;
            self.dirty = true;
            return Some(request_id);
        }
        None
    }

    /// Fire a periodic refresh if enough wall-clock time has elapsed since the
    /// last one and no request is already outstanding. Checked in `sync`,
    /// which [`Self::next_refresh_at`] wakes when the refresh is due, so an
    /// idle window gets its mail too.
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

    /// Have the engine watch every account's inbox, so new mail shows as it
    /// arrives rather than at the next periodic refresh.
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

    /// Bring the inbox of every account but the active one up to date, so
    /// the sidebar's counts are right for all of them.
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

    /// When the next periodic refresh is due, so a window left open
    /// wakes for it instead of waiting for input. `None` while nothing
    /// refreshes (no engine, no account, or a request still in flight).
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

    /// Hand a newly-set status to the shell, which shows it as a toast and
    /// lets it fade on its own.
    fn show_status(&mut self) {
        let (status, undoable) = {
            let mut st = self.shared.borrow_mut();
            (st.status.take(), std::mem::take(&mut st.status_undo))
        };
        let Some(msg) = status else {
            return;
        };
        let kind = Self::status_kind(&msg);
        self.send_notice = std::mem::take(&mut self.sending_notice_next);
        if undoable {
            self.nav.toast_with_action(msg, kind, "undo-2", "Undo");
        } else {
            self.nav.toast(msg, kind);
        }
    }

    fn status_kind(msg: &str) -> ToastKind {
        let lower = msg.to_ascii_lowercase();
        if lower.contains("failed")
            || lower.contains("couldn’t")
            || lower.contains("couldn't")
            || lower.contains("can’t")
            || lower.contains("can't")
            || lower.contains("didn’t")
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

    /// A watched inbox of `account_id` changed: fetch it, if the account is
    /// still on this device.
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

    /// Keep `account_id`'s mail on this device, so the next window opens on
    /// it at once; the attachments whose bytes are kept say so.
    fn keep_on_device(&self, st: &mut MailState, account_id: &str) {
        let kept = self.store.save(
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
        synced: crate::sync::Synced,
    ) {
        let (gone, reflagged) = server_changes(&st.mailbox, account_id, folder, &synced);
        for id in gone {
            st.mailbox.purge(id);
        }
        if account_id == st.accounts.active_config().email {
            st.first_fetch = None;
        }
        st.ingest_fetched(doc, account_id, &reflagged);
        st.ingest_fetched(doc, account_id, &synced.messages);
        // The server answered: what it refused before may go through now.
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
        st.sync_error = None;
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

    /// Put the mail kept on this device for every account not opened yet
    /// into the mailbox, so the window shows it before any server answers.
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
        for email in accounts {
            let kept = self.store.load(&email);
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

    /// Register kept mail with the mailbox record: where each message is on
    /// its server, and, for one the record does not know yet, its flags as
    /// kept. What the record already says of a message is left as it is.
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
            .filter(|message| !message.account.is_empty())
            .map(|message| {
                let mut baseline =
                    crate::actions::IngestBaseline::fetched(message, &message.account);
                if mail.knows(&message.id.to_string()) {
                    baseline.read = None;
                    baseline.starred = None;
                    baseline.folder = None;
                }
                baseline
            })
            .collect();
        if !baselines.is_empty() {
            self.shared.borrow().ingest_baselines(doc, baselines);
        }
    }

    /// Ask for the page before the oldest message of the open folder, once:
    /// the list has scrolled to its end and the server has more.
    pub(crate) fn load_older(&mut self) {
        // Every inbox together asks each account for its older page.
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
        for (account, folder, known) in wanted {
            let Some(known) = known else {
                continue;
            };
            let key = (account.email.clone(), folder);
            if self.has_older.get(&key) == Some(&false) || self.older_pending.contains_key(&key) {
                continue;
            }
            if let Some(request) = self.post_engine(EngineRequest::fetch_wanting(
                account,
                folder,
                crate::sync::Want::Older { known },
            )) {
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
        let Some(pending) = self.pending_send.remove(&request_id) else {
            return;
        };
        // A message joins Sent only after SMTP has accepted its bytes.
        st.ingest_messages(doc, std::slice::from_ref(&pending.message));
        st.mailbox.insert(pending.message);
        (pending.done)(Ok(ui::tilo_resource::Settled::Done(())));
        st.status = Some("Message sent".into());
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
        if context == "search" {
            // The search shows what this device holds either way.
            tracing::debug!(%message, "mail: a server search failed");
            return;
        }
        let detail = format!("{context}: {message}");
        let refused = self.server_outbox.borrow_mut().sent.remove(&request_id);
        if let Some((_, done)) = refused {
            done(Err(detail.clone()));
        }
        // The account that asked, not the one selected since.
        let account_id = self
            .pending_engine_requests
            .get(&request_id)
            .map_or_else(String::new, |pending| pending.account.clone());
        st.first_fetch = None;
        let shown = engine_error_text(context, message, &account_id);
        if matches!(context, "connect" | "fetch") {
            // A failing account is said once, not on every refresh that
            // fails the same way; it is said again when the failure changes.
            if st.sync_error.as_deref() != Some(shown.as_str()) {
                st.status = Some(shown.clone());
                st.sync_error = Some(shown);
            }
        } else {
            st.status = Some(shown);
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
                self.apply_engine_error(st, request_id, &context, &message);
            }
            EngineEvent::Completed { request_id, kind }
                if self.expects_engine_event(request_id, kind) =>
            {
                // Only the worker acknowledgement settles a queued request:
                // connect has two payloads and successful writes have none.
                self.pending_engine_requests.remove(&request_id);
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

    /// Hand the server updates the projection asked for to the engine, each
    /// on the account it belongs to. One this device cannot carry out (no
    /// such account here, no engine) is refused; another device with the
    /// account carries it out.
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

    /// The header identities of outbox message `outgoing` sent from
    /// `sender`: its own `Message-ID` (the same every time it is sent, so a
    /// resend is the same message), and, for a reply, the message it
    /// answers and the conversation's root.
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
        for account in std::mem::take(&mut self.pending_watch_fetches) {
            self.post_engine(EngineRequest::fetch(account, Folder::Inbox));
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
    /// Take the sender pictures from Contacts' directory and the person's
    /// own identity when either changed. A live window only: a capture or a
    /// test shows initials.
    fn adopt_sender_pictures(&mut self) {
        let Some(directory) = &self.contact_directory else {
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
        let active_changed = {
            let mut st = self.shared.borrow_mut();
            let previous = st.accounts.active_account().cloned();
            if let Some(shown) = st
                .accounts
                .active_account()
                .map(|account| account.id.clone())
                && let Some(index) = adopted.accounts.iter().position(|a| a.id == shown)
            {
                adopted.active = index;
            }
            for account in &mut adopted.accounts {
                if let Some(previous) = st.accounts.accounts.iter().find(|a| a.id == account.id) {
                    account.collapsed = previous.collapsed;
                }
            }
            let active_changed = previous.as_ref().map(|a| (&a.id, &a.config))
                != adopted.active_account().map(|a| (&a.id, &a.config));
            // An account removed in Settings takes its mail with it, here
            // and from what this device keeps.
            let removed: Vec<String> = st
                .accounts
                .accounts
                .iter()
                .filter(|old| !adopted.accounts.iter().any(|new| new.id == old.id))
                .map(|old| old.id.clone())
                .collect();
            for id in &removed {
                let gone: Vec<MessageId> = st.mailbox.of_account(id).map(|m| m.id).collect();
                for message in gone {
                    st.mailbox.purge(message);
                }
                self.store.forget(id);
                self.kept_loaded.remove(id);
            }
            if !removed.is_empty() {
                st.selected = None;
            }
            st.accounts = adopted;
            active_changed
        };
        // An empty window has already completed its first sync. Its first
        // Settings-created account must connect here, not wait for the
        // one-time startup path to run again. Inactive additions need no fetch.
        if self.shared.borrow().accounts.is_empty() {
            self.connect_requested = false;
        } else if self.engine.is_some() && (active_changed || !self.connect_requested) {
            let (cfg, folder) = {
                let st = self.shared.borrow();
                (st.accounts.active_config(), st.folder)
            };
            self.post_engine(EngineRequest::connect(cfg.clone()));
            self.post_engine(EngineRequest::fetch(cfg, folder));
            self.connect_requested = true;
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

/// What a forward does with the forwarded message's files.
struct ForwardedFiles {
    /// Kept for the draft: they go with the forward.
    carried: Vec<crate::model::DraftAttachment>,
    /// Not on this device yet, by name.
    not_downloaded: Vec<String>,
    /// Past what one message carries, by name.
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
/// What `message` says, for a caller with no screen: its people, subject,
/// time, text and attachments' names.
/// The HTML gallery's screenshot scenarios ([`crate::sample::MailFixture::html_gallery`]),
/// each with the sender whose mail it opens.
pub(crate) const GALLERY_SCENARIOS: &[(&str, &str)] = &[
    ("html_newsletter", "news@harvest-weekly.example"),
    ("html_github", "notifications@github.example"),
    ("html_receipt", "receipts@lumen-cafe.example"),
    ("html_dark", "digest@orbit-cal.example"),
    ("html_inline", "ana@studio-lima.example"),
    ("plain_text", "lena@fischer.example"),
];

/// Open a link of a message in the Browser, through the host's opener. Only
/// web and mail addresses open.
fn open_link(url: &str) {
    if crate::reader::is_openable(url) {
        ui::widget::open_external_url(url);
    }
}

fn message_read(message: &Message) -> serde_json::Value {
    // Written as a recipient field takes it, so an agent replying with
    // what it read reaches the same people.
    let person = crate::model::Addr::rfc5322;
    serde_json::json!({
        "from": person(&message.from),
        "to": message.to.iter().map(person).collect::<Vec<_>>(),
        "cc": message.cc.iter().map(person).collect::<Vec<_>>(),
        "subject": message.subject,
        "sent_minutes_since_epoch": message.timestamp_min,
        "text": message.body,
        "links": crate::html_mail::message_links(message),
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
        wire: crate::model::WireIds::default(),
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

impl MailApp {
    /// The loaded mailbox, which search reads its messages from.
    pub(crate) fn shared_mailbox(&self) -> std::cell::Ref<'_, crate::model::Mailbox> {
        std::cell::Ref::map(self.shared.borrow(), |state| &state.mailbox)
    }

    /// Route events to the open compose modal and report whether it consumed
    /// them.
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
                self.open_account_settings();
            }
            self.dirty = true;
            return true;
        }
        false
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
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::undo::UndoControl
    );
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document_ref(state, path);
        self.background_poll_deadline.set(None);
        self.adopt_person_accounts();
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
        self.drain_attachment_opening();
        self.post_server_search();
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

        let mem_seeded = self.shared.borrow().seeded;
        if !mem_seeded {
            // Live: kick off a connect + inbox fetch for the active account.
            let has_accounts = !self.shared.borrow().accounts.is_empty();
            if self.engine.is_some() && has_accounts && !self.connect_requested {
                let cfg = self.shared.borrow().accounts.active_config();
                {
                    // Nothing kept for the account: its rows are on the way.
                    let mut st = self.shared.borrow_mut();
                    let nothing_kept = st.mailbox.of_account(&cfg.email).next().is_none();
                    st.first_fetch = nothing_kept.then(|| first_fetch(&cfg));
                }
                self.post_engine(EngineRequest::connect(cfg.clone()));
                self.post_engine(EngineRequest::fetch(cfg, Folder::Inbox));
                self.fetch_other_inboxes();
                self.watch_inboxes();
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
        let wants_older = self.shared.borrow().wants_older.take();
        if wants_older && self.engine.is_some() {
            self.load_older();
        }
        self.persist_active_account_if_changed();
        self.sync_preferences();
        self.declare_window_toolbar();
        self.nav.sync(state, path);
    }

    fn settle(&mut self, state: &mut ui::widget::Edits<'_>, path: &ObjId) {
        let opens = std::mem::take(&mut *self.opens.borrow_mut());
        for target in opens {
            if let Err(error) = self.open_target(target) {
                tracing::warn!(%error, "mail: a hit could not be opened");
            }
        }
        self.apply_window_toolbar(state);
        self.register_kept_mail(state);
        self.open_activated();
        self.drain_engine(state);
        self.reveal_requested(state);
        self.deliver_effect_replies();
        self.take_back_sends(state);
        self.flush_server_outbox();
        self.release_held_sends(state);
        self.compose.settle(state, path);
        self.keep_compose_headers(state);
        if !self.shared.borrow().seeded {
            self.register_hydrated_messages(state);
        }
        self.hydrate_domain_state(state);
        if let Some(scenario) = self.pending_stage.take() {
            self.stage_scenario(state, &scenario);
        }
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
        if self.menu.is_open() {
            children.push(&self.menu);
        }
        if let Some(confirmation) = &self.bulk_purge_confirmation {
            children.push(&confirmation.dialog);
        }
        children
    }

    /// Whatever the children report.
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
        // Before there is a mailbox the composer is the window's page.
        if self.compose.is_modal() || (self.compose.open && self.is_first_run()) {
            return Some(self.compose.accessibility_node());
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
            // The message being written stands where the open one is read.
            children.push(if self.compose.open {
                self.compose.accessibility_node()
            } else {
                Self::reader_accessibility_node(&st)
            });
        } else if self.compose.open {
            // A narrow window's composer is the page itself.
            children.push(self.compose.accessibility_node());
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
            || self.shared.borrow().dirty
            || self.nav.needs_redraw()
            || self.compose.needs_redraw()
            || self.shortcuts.needs_redraw()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        // Worker and native-picker completions are not animation. Keep their
        // deadline fixed until sync consumes a tick, even with reduced motion.
        // Workers that wake the window themselves are not polled at all.
        let workers_wake = self.workers_wake();
        let pending = (!workers_wake && self.awaiting_engine)
            || self.compose_attachment_rx.is_some()
            || self.compose_attachment_contents.is_some()
            || self.attachment_opening.is_some();
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
            .chain(self.next_refresh_at())
            .chain(self.compose.headers_due_at())
            .chain(self.server_search.as_ref().map(|(_, at)| *at))
            .chain(self.held_sends.iter().map(|held| held.due))
            .min()
    }

    fn needs_animation_frame(&self) -> bool {
        // A worker that cannot wake the window (no live surface attached) is
        // drained by pumping frames while its request is in flight; one that
        // can is waited for without drawing anything.
        (self.awaiting_engine && !self.workers_wake()) || self.nav.needs_animation_frame()
    }

    fn needs_idle_sync(&self) -> bool {
        // Keep syncing while a connect/fetch reply is outstanding or a status
        // waits for `sync` to show it, then idle rather than busy-loop once
        // results have arrived.
        (self.awaiting_engine && !self.workers_wake())
            || self.shared.borrow().status.is_some()
            || self.shared.borrow().wants_older.get()
            || self.nav.needs_idle_sync()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.shared.borrow_mut().dirty = false;
        self.nav.mark_drawn();
        self.compose.mark_drawn();
        self.shortcuts.mark_drawn();
    }
}

/// A notice's Undo goes through the host's undo, the same as Ctrl+Z.
impl ui::undo::UndoControl for MailApp {
    fn take_undo_request(&mut self) -> Option<ui::undo::UndoRequest> {
        self.nav.take_toast_action()?;
        if self.send_notice && (!self.held_sends.is_empty() || self.sends_to_hold > 0) {
            // The messages still waiting come back; nothing else is undone.
            self.unsend = true;
            self.dirty = true;
            return None;
        }
        Some(ui::undo::UndoRequest::Undo)
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
            MailEffect::SearchMessages { .. } | MailEffect::ExportContent { .. } => {
                Err("held mail is answered from the mailbox".into())
            }
            effect @ (MailEffect::LoadRemoteContent { .. }
            | MailEffect::AlwaysLoadRemoteContent { .. }
            | MailEffect::OpenLink { .. }) => self.reader_effect(effect).map(|()| done),
        }
    }

    /// What the reader does to a message: load its remote images, for it
    /// or its sender, or open one of its links in the Browser.
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

    /// The loaded HTML message `message` names.
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
        let mut attachment = attachment;
        if attachment.content.is_empty() && attachment.kept {
            // Kept on this device rather than in memory.
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
                .ok_or_else(|| {
                    format!("{} is no longer kept on this device", attachment.filename)
                })?;
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
            format!("Saved a copy as {name}")
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
    /// Search the servers for `text` a moment after it stops changing: a
    /// word of three letters or more, once.
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

    /// Whether a server search is still to be asked or answered.
    pub(crate) fn searching_servers(&self) -> bool {
        self.server_search.is_some()
            || self
                .pending_engine_requests
                .values()
                .any(|pending| pending.kind == EngineRequestKind::Search)
    }

    /// Ask every account's server for the search text that is due.
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

    /// Open attachment `index` of `message` in the app that shows its kind
    /// (a picture in Photos, a PDF in the PDF viewer), handing it over as
    /// a file of this app's own.
    fn open_attachment(&mut self, message: MessageId, index: usize) {
        let (name, bytes) = match self.attachment_to_save(&message.to_string(), index) {
            Ok(found) => found,
            Err(error) => {
                self.shared.borrow_mut().status = Some(error);
                return;
            }
        };
        let Some(path) = self.store.opening(&name) else {
            // Nowhere to put it on this host: save it instead.
            self.request_attachment_save(message, index);
            return;
        };
        if let Err(error) = std::fs::write(&path, &bytes) {
            self.shared.borrow_mut().status = Some(format!("Couldn’t open {name}: {error}"));
            return;
        }
        self.attachment_opening = Some((name, ui::portal::LaunchRequest::open(path).send()));
        self.dirty = true;
    }

    /// Say why an attachment did not open, once the host answers.
    fn drain_attachment_opening(&mut self) {
        let Some((name, channel)) = self.attachment_opening.as_ref() else {
            return;
        };
        let failure = match channel.try_recv() {
            Err(TryRecvError::Empty) => return,
            Ok(ui::portal::LaunchOutcome::Failed(error)) => Some(error),
            Ok(_) | Err(TryRecvError::Disconnected) => None,
        };
        if let Some(error) = failure {
            self.shared.borrow_mut().status = Some(format!("Couldn’t open {name}: {error}"));
        }
        self.attachment_opening = None;
    }

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

    /// Delete, #: the selection's messages, or the open one, to the Trash.
    fn delete_shortcut(&mut self, state: &mut ui::widget::Edits<'_>) {
        let selected = self.shared.borrow().selected;
        if self.shared.borrow().bulk_selecting {
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
        } else if let Some(id) = selected {
            self.shared
                .borrow_mut()
                .apply_message_action(state, id, CtxAction::Delete);
        }
        self.process_intents();
    }
}

/// What the server's answer for `folder` of `account_id` changes in what
/// `mailbox` holds: the messages it no longer has, and those another client
/// read or starred, as reported.
fn server_changes(
    mailbox: &crate::model::Mailbox,
    account_id: &str,
    folder: Folder,
    synced: &crate::sync::Synced,
) -> (Vec<MessageId>, Vec<Message>) {
    let held_here = |message: &Message| {
        message.account == account_id
            && message.folder == folder
            && message.server.is_some_and(|at| at.folder == folder)
    };
    // What the server no longer holds goes: everything of a folder that
    // started over and is not in its new page, and every message of the
    // known window its server did not report.
    let mut gone: Vec<MessageId> = Vec::new();
    if synced.reset {
        let fresh: std::collections::HashSet<MessageId> =
            synced.messages.iter().map(|message| message.id).collect();
        gone.extend(
            mailbox
                .of_account(account_id)
                .filter(|message| held_here(message) && !fresh.contains(&message.id))
                .map(|message| message.id),
        );
    }
    let mut reflagged: Vec<Message> = Vec::new();
    if let Some((oldest, newest)) = synced.window {
        let reported: HashMap<u32, (bool, bool)> = synced
            .flags
            .iter()
            .map(|(uid, seen, flagged)| (*uid, (*seen, *flagged)))
            .collect();
        for message in mailbox.of_account(account_id).filter(|m| held_here(m)) {
            let Some(uid) = message.server.map(|at| at.uid) else {
                continue;
            };
            if uid < oldest || uid > newest {
                continue;
            }
            match reported.get(&uid) {
                None => gone.push(message.id),
                // Another client read or starred it: the record decides
                // whether that or a change made here wins.
                Some(&(seen, flagged)) if seen != message.read || flagged != message.starred => {
                    let mut reported = message.clone();
                    reported.read = seen;
                    reported.starred = flagged;
                    reflagged.push(reported);
                }
                Some(_) => {}
            }
        }
    }
    (gone, reflagged)
}

/// What a server search found joins the mail this device holds, and so the
/// search; nothing held is replaced.
fn take_found(
    doc: &mut ui::widget::Edits<'_>,
    st: &mut MailState,
    account_id: &str,
    messages: Vec<Message>,
) {
    let fresh: Vec<Message> = messages
        .into_iter()
        .filter(|message| st.mailbox.get(message.id).is_none())
        .collect();
    st.ingest_fetched(doc, account_id, &fresh);
    for message in fresh {
        st.mailbox.insert(message);
    }
}

/// The timing of an account's first fetch, starting now: its mail comes
/// from the account's server.
fn first_fetch(config: &crate::model::AccountConfig) -> ui::arriving::ArrivalClock {
    ui::arriving::ArrivalClock::new(
        ui::time::now(),
        ui::arriving::Source::host(config.imap_host.clone()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::TestMail;
    use automerge::ReadDoc;

    #[test]
    fn settings_mail_account_connects_after_an_empty_windows_first_sync() {
        let mut host =
            ui::framework::testing::TestApp::<settings::resource::SettingsShell>::new().unwrap();
        let record = host.person::<PersonAccounts>().unwrap();
        let mut mail = MailApp::new_first_run();
        let (requests, outgoing) = std::sync::mpsc::channel();
        let (_events, incoming) = std::sync::mpsc::channel();
        mail.engine = Some(EngineHandle {
            tx: requests,
            rx: incoming,
        });
        mail.follow_person_accounts(record.clone());
        let doc = TestMail::doc();
        mail.sync(&doc, &automerge::ROOT);
        assert!(mail.shared.borrow().seeded);
        assert!(!mail.connect_requested);
        assert!(outgoing.try_recv().is_err());

        let config = AccountConfig {
            email: "ada@example.org".into(),
            username: "ada@example.org".into(),
            imap_host: "imap.gmail.com".into(),
            smtp_host: "smtp.gmail.com".into(),
            auth: accounts::email::AuthMethod::OAuth,
            ..Default::default()
        };
        let mut stored = accounts::Account::new("gmail-ada", crate::account::TYPE_ID);
        stored.set_config(&config);
        record.invoke(&PersonAccounts::saving(&stored)).unwrap();
        mail.sync(&doc, &automerge::ROOT);
        assert_eq!(mail.shared.borrow().accounts.active_config(), config);
        assert!(!mail.is_first_run());
        assert!(mail.connect_requested);
        assert!(
            matches!(outgoing.try_recv().unwrap(), EngineRequest::Connect { account, .. } if account == config)
        );
        assert!(
            matches!(outgoing.try_recv().unwrap(), EngineRequest::Fetch { account, folder: Folder::Inbox, .. } if account == config)
        );
        assert!(outgoing.try_recv().is_err());

        mail.shared.borrow_mut().accounts.toggle_collapsed(0);
        mail.shared.borrow_mut().folder = Folder::Sent;
        let mut other = accounts::Account::new("gmail-other", crate::account::TYPE_ID);
        let mut other_config = config.clone();
        other_config.email = "other@example.org".into();
        other.set_config(&other_config);
        record.invoke(&PersonAccounts::saving(&other)).unwrap();
        mail.sync(&doc, &automerge::ROOT);
        assert_eq!(mail.shared.borrow().accounts.len(), 2);
        assert_eq!(mail.shared.borrow().accounts.active_config(), config);
        assert!(
            mail.shared
                .borrow()
                .accounts
                .active_account()
                .unwrap()
                .collapsed
        );
        assert_eq!(mail.shared.borrow().folder, Folder::Sent);
        assert!(
            outgoing.try_recv().is_err(),
            "an inactive addition must not refetch the active mailbox"
        );
    }

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

    #[test]
    fn compose_stands_in_the_window_and_sends_only_with_an_account_and_a_recipient() {
        // No account yet: the composer is the window's page, never a panel
        // floating over the empty state, and it cannot send.
        let mut first = MailApp::new_first_run();
        let mut doc = crate::resource::TestMail::doc();
        first.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        first.sync(&doc, &automerge::ROOT);
        first.layout_for_test(1100.0, 780.0);
        first.shared.borrow_mut().push_intent(Intent::Compose);
        first.process_intents();
        first.layout_for_test(1100.0, 780.0);
        assert!(first.compose.open);
        assert!(
            !first.compose.is_modal(),
            "no floating panel over a scrim, so nothing blurred shows through"
        );
        assert_eq!(
            first.compose.window_rect(),
            first.rect,
            "the page is the window"
        );
        first.compose.type_recipient("tomas@example.org");
        first.sync(&doc, &automerge::ROOT);
        assert!(!first.compose.can_send(), "no account to send from");

        // With an account: a reply stands in the message column, beside the
        // list that holds the message it answers.
        let (mut app, doc) = app();
        app.layout_for_test(1280.0, 800.0);
        app.stage_reply();
        app.sync(&doc, &automerge::ROOT);
        app.layout_for_test(1280.0, 800.0);
        assert!(
            app.compose.open && app.compose.in_pane(),
            "the reply is inline"
        );
        assert!(!app.compose.is_modal());
        let geometry = app.nav.geometry();
        let column = app.compose.window_rect();
        assert!(
            column.intersect(geometry.overview_column).is_zero_area(),
            "the reply {column:?} leaves the list {:?} in view",
            geometry.overview_column
        );
        assert!(
            app.compose.can_send(),
            "a reply has its recipient and account"
        );

        app.compose.open_blank();
        app.sync(&doc, &automerge::ROOT);
        app.layout_for_test(1280.0, 800.0);
        assert!(!app.compose.can_send(), "no recipient yet");
        let send = app
            .compose
            .accessibility_node()
            .children
            .into_iter()
            .find(|node| node.name.as_deref() == Some("Send"))
            .expect("Send is published");
        assert!(send.state.disabled, "Send is announced as unavailable");
        app.compose.type_recipient("tomas@example.org");
        assert!(app.compose.can_send());
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

    #[test]
    fn compose_send_accepts_recipients_in_any_header_field() {
        for (to, cc, bcc) in [
            ("dest@example.test", "", ""),
            ("", "dest@example.test", ""),
            ("", "", "hidden@example.test"),
            ("", "", ""),
        ] {
            let (mut app, mut doc) = app();
            // Replace the engine with an in-memory backend: no SMTP or other network IO.
            let (requests, outgoing) = std::sync::mpsc::channel();
            let (_events, incoming) = std::sync::mpsc::channel();
            app.engine = Some(EngineHandle {
                tx: requests,
                rx: incoming,
            });
            let sent_before = app.shared.borrow().mailbox.count(Folder::Sent);
            let draft = Draft {
                to: to.into(),
                cc: cc.into(),
                bcc: bcc.into(),
                subject: "Recipient regression".into(),
                body: "Hello".into(),
                ..Default::default()
            };
            app.compose.open_with_draft(&draft);
            app.send_compose(&mut ui::widget::Edits::lend(&mut doc));
            assert!(
                outgoing.try_recv().is_err(),
                "nothing reaches the server before the undo time is up"
            );
            if to.is_empty() && cc.is_empty() && bcc.is_empty() {
                assert!(app.compose.open, "an empty recipient list keeps the draft");
                assert_eq!(
                    app.shared.borrow().status.as_deref(),
                    Some("Add a recipient before sending")
                );
                continue;
            }
            assert!(!app.compose.open, "To={to:?}, Cc={cc:?}, Bcc={bcc:?}");
            let dispatcher = app.shared.borrow().dispatcher.clone().unwrap();
            TestMail::confirm(&mut doc, &dispatcher);
            let outbox = TestMail::outbox(&doc);
            let [(queued, None)] = outbox.as_slice() else {
                panic!("one send action must queue a message: {outbox:?}");
            };
            assert_eq!(queued.draft.to, to);
            assert_eq!(queued.draft.cc, cc);
            assert_eq!(queued.draft.bcc, bcc);
            app.server_outbox()
                .borrow_mut()
                .deliver(queued.clone(), Box::new(|_| {}));
            app.flush_server_outbox();
            app.undo_time_is_up(&mut doc);
            let EngineRequest::Send { account, draft, .. } = outgoing.try_recv().unwrap() else {
                panic!("the compose action must reach the send backend");
            };
            assert_eq!(draft.to, to);
            assert_eq!(draft.cc, cc);
            assert_eq!(draft.bcc, bcc);
            let wire = crate::transport::build_mime(
                &app.content,
                &Addr::new(account.display_name, account.email),
                &draft,
            )
            .unwrap();
            let wire = String::from_utf8(wire).unwrap();
            assert!(!wire.to_ascii_lowercase().contains("bcc:"));
            assert!(!wire.contains("hidden@example.test"));
            assert!(outgoing.try_recv().is_err(), "only one send is submitted");
            assert_eq!(app.shared.borrow().mailbox.count(Folder::Sent), sent_before);
        }
    }

    fn fetched(account: &str, uid: u32, validity: u32, at: i64) -> Message {
        let raw = format!(
            "From: Ada <ada@x.example>\r\nTo: {account}\r\nSubject: S{uid}\r\n\
Message-ID: <{uid}.{validity}@x.example>\r\n\r\nBody {uid}.\r\n"
        );
        let mut message = crate::transport::parse_eml(raw.as_bytes(), at, Folder::Inbox).unwrap();
        message.account = account.to_owned();
        message.server = Some(crate::model::ServerPlace {
            folder: Folder::Inbox,
            uid,
            uid_validity: validity,
        });
        message
    }

    /// A folder sync brings what is new, takes what another client changed
    /// or removed, keeps the result on the device, and asks only for what
    /// is new the next time.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one sync after another, each depending on what the last one left"
    )]
    fn folder_syncs_apply_new_mail_remote_flags_and_removals_and_are_kept() {
        let (mut app, mut doc) = app();
        let kept = tempfile::tempdir().unwrap();
        app.store = crate::store::Store::at(kept.path().to_path_buf());
        let (outgoing, requests) = std::sync::mpsc::channel();
        let (incoming, incoming_rx) = std::sync::mpsc::channel();
        app.engine = Some(EngineHandle {
            tx: outgoing,
            rx: incoming_rx,
        });
        let account = app.shared.borrow().accounts.active_config().email;
        let sync = |app: &mut MailApp, doc: &mut AutoCommit, synced: crate::sync::Synced| {
            let config = app.shared.borrow().accounts.active_config();
            let request = app
                .post_engine(EngineRequest::fetch(config, Folder::Inbox))
                .unwrap();
            let posted = requests.try_recv().unwrap();
            incoming
                .send(EngineEvent::Fetched {
                    request_id: request,
                    account_id: account.clone(),
                    folder: Folder::Inbox,
                    synced,
                })
                .unwrap();
            incoming
                .send(EngineEvent::Completed {
                    request_id: request,
                    kind: EngineRequestKind::Fetch,
                })
                .unwrap();
            app.drain_engine(&mut ui::widget::Edits::lend(doc));
            app.hydrate_domain_state(&mut ui::widget::Edits::lend(doc));
            posted
        };

        // The first sync knows nothing and starts the folder from its
        // newest page.
        let posted = sync(
            &mut app,
            &mut doc,
            crate::sync::Synced {
                uid_validity: 9,
                reset: true,
                messages: vec![
                    fetched(&account, 1, 9, 100),
                    fetched(&account, 2, 9, 200),
                    fetched(&account, 3, 9, 300),
                ],
                has_older: Some(false),
                ..Default::default()
            },
        );
        assert!(matches!(
            posted,
            EngineRequest::Fetch {
                want: crate::sync::Want::Latest { known: None },
                ..
            }
        ));
        let held = |app: &MailApp| -> Vec<u32> {
            let mut uids: Vec<u32> = app
                .shared
                .borrow()
                .mailbox
                .of_account(&account)
                .filter_map(|message| message.server.map(|at| at.uid))
                .collect();
            uids.sort_unstable();
            uids
        };
        assert_eq!(held(&app), [1, 2, 3]);

        // The next asks only for what is new to the window it holds; the
        // server says 2 was removed and 3 was read elsewhere, and 4 arrived.
        let posted = sync(
            &mut app,
            &mut doc,
            crate::sync::Synced {
                uid_validity: 9,
                messages: vec![fetched(&account, 4, 9, 400)],
                flags: vec![(1, false, false), (3, true, false)],
                window: Some((1, 3)),
                ..Default::default()
            },
        );
        assert!(matches!(
            posted,
            EngineRequest::Fetch {
                want: crate::sync::Want::Latest {
                    known: Some(crate::sync::Window {
                        uid_validity: 9,
                        newest: 3,
                        oldest: 1
                    })
                },
                ..
            }
        ));
        assert_eq!(held(&app), [1, 3, 4], "what the server removed goes");
        let read = app
            .shared
            .borrow()
            .mailbox
            .by_uid(&account, Folder::Inbox, 3)
            .is_some_and(|message| message.read);
        assert!(read, "read on another client reads as read here");

        // What was fetched is kept: a new window opens on it.
        let deadline = tilo_test_env::hang_deadline();
        let kept_uids = loop {
            let mut uids: Vec<u32> = app
                .store
                .load(&account)
                .iter()
                .filter_map(|message| message.server.map(|at| at.uid))
                .collect();
            uids.sort_unstable();
            if uids == [1, 3, 4] || ui::time::now() > deadline {
                break uids;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(kept_uids, [1, 3, 4]);
    }

    /// A watched inbox that changed is fetched at once, for the account it
    /// belongs to; a watch for an account no longer held is ignored.
    #[test]
    fn a_watched_inbox_that_changed_is_fetched_at_once() {
        let (mut app, mut doc) = app();
        let (outgoing, requests) = std::sync::mpsc::channel();
        let (incoming, incoming_rx) = std::sync::mpsc::channel();
        app.engine = Some(EngineHandle {
            tx: outgoing,
            rx: incoming_rx,
        });
        let account = app.shared.borrow().accounts.accounts[1].config.clone();
        for account_id in [account.email.clone(), "gone@nowhere.example".to_owned()] {
            incoming
                .send(EngineEvent::Arrived {
                    request_id: uuid::Uuid::new_v4(),
                    account_id,
                })
                .unwrap();
        }
        app.drain_engine(&mut ui::widget::Edits::lend(&mut doc));
        let posted: Vec<EngineRequest> = requests.try_iter().collect();
        assert_eq!(posted.len(), 1, "{posted:?}");
        assert!(matches!(
            &posted[0],
            EngineRequest::Fetch { account: fetched, folder: Folder::Inbox, .. }
                if fetched.email == account.email
        ));
    }

    /// Every inbox together lists each account's mail, and a reply to a
    /// message goes from the account it came to.
    #[test]
    fn all_inboxes_list_every_account_and_replies_go_from_the_owner() {
        let (mut app, _doc) = app();
        let (first, second) = {
            let st = app.shared.borrow();
            (
                st.accounts.accounts[0].config.email.clone(),
                st.accounts.accounts[1].config.email.clone(),
            )
        };
        let to_first = fetched(&first, 1, 1, 100);
        let to_second = fetched(&second, 2, 1, 200);
        let (first_id, second_id) = (to_first.id, to_second.id);
        {
            let mut st = app.shared.borrow_mut();
            st.mailbox.insert(to_first);
            st.mailbox.insert(to_second);
        }
        let listed = |app: &MailApp| -> Vec<MessageId> {
            app.shared
                .borrow()
                .list_rows()
                .into_iter()
                .map(|(id, _)| id)
                .collect()
        };
        assert!(
            !listed(&app).contains(&second_id),
            "the open account lists its own mail"
        );
        app.shared
            .borrow_mut()
            .push_command(Command::OpenAllInboxes);
        app.apply_commands(&mut ui::widget::Edits::lend(
            &mut crate::resource::TestMail::doc(),
        ));
        app.sync(&crate::resource::TestMail::doc(), &automerge::ROOT);
        let rows = listed(&app);
        assert!(
            rows.contains(&first_id) && rows.contains(&second_id),
            "{rows:?}"
        );
        assert_eq!(
            crate::sidebar::selected_row(&app.shared.borrow()).as_deref(),
            Some(crate::sidebar::ALL_INBOXES)
        );
        app.open_reply(second_id);
        assert_eq!(app.compose.sending_from(), Some(second.as_str()));
        assert_eq!(app.sending_account().email, second);
    }

    /// Searching asks every account's server once the text stops changing,
    /// and what the servers find joins the mail the search reads.
    #[test]
    fn a_search_asks_the_servers_and_keeps_what_they_find() {
        let (mut app, mut doc) = app();
        let (outgoing, requests) = std::sync::mpsc::channel();
        let (incoming, incoming_rx) = std::sync::mpsc::channel();
        app.engine = Some(EngineHandle {
            tx: outgoing,
            rx: incoming_rx,
        });
        app.search_servers("in");
        assert!(app.server_search.is_none(), "two letters search only here");
        assert!(!app.searching_servers(), "nothing for a search to wait on");
        app.search_servers("invoice");
        assert!(app.searching_servers(), "a search waits for the servers");
        app.post_server_search();
        assert!(
            requests.try_recv().is_err(),
            "not while the text may change"
        );
        app.server_search = app
            .server_search
            .take()
            .map(|(text, _)| (text, ui::time::now()));
        app.post_server_search();
        let posted: Vec<EngineRequest> = requests.try_iter().collect();
        assert_eq!(
            posted.len(),
            app.shared.borrow().accounts.accounts.len(),
            "every account is asked"
        );
        let EngineRequest::Search {
            request_id,
            account,
            text,
        } = posted[0].clone()
        else {
            panic!("a search: {posted:?}");
        };
        assert_eq!(text, "invoice");
        assert!(app.searching_servers(), "the servers are looking");
        app.search_servers("invoice");
        assert!(
            app.server_search.is_none(),
            "the same text is not asked again"
        );

        let mut old = fetched(&account.email, 9, 1, 10);
        old.server = None;
        let old_id = old.id;
        incoming
            .send(EngineEvent::Found {
                request_id,
                account_id: account.email,
                messages: vec![old],
            })
            .unwrap();
        incoming
            .send(EngineEvent::Completed {
                request_id,
                kind: EngineRequestKind::Search,
            })
            .unwrap();
        app.drain_engine(&mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(
            app.searching_servers(),
            posted.len() > 1,
            "the other accounts are still looking"
        );
        for other in &posted[1..] {
            incoming
                .send(EngineEvent::Completed {
                    request_id: other.request_id(),
                    kind: EngineRequestKind::Search,
                })
                .unwrap();
        }
        app.drain_engine(&mut ui::widget::Edits::lend(&mut doc));
        assert!(!app.searching_servers(), "every server answered");
        assert!(
            crate::search::message_entries(&app.shared.borrow().mailbox)
                .iter()
                .any(|entry| entry.anchor.as_deref()
                    == Some(format!("mail.message:{old_id}").as_str())),
            "what the server found is searched"
        );
    }

    /// A sent message waits out the undo time before it reaches the server;
    /// "Undo" on the sending notice takes it back to the composer unsent.
    #[test]
    fn a_sent_message_waits_so_undo_takes_it_back_unsent() {
        let send = |app: &mut MailApp, doc: &mut AutoCommit| -> Outgoing {
            app.compose.open_blank();
            app.compose.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
            app.compose.reattach(&crate::model::Draft {
                to: "dest@there.example".into(),
                subject: "Hi".into(),
                body: "Hello".into(),
                ..Default::default()
            });
            app.send_compose(&mut ui::widget::Edits::lend(doc));
            let dispatcher = app
                .shared
                .borrow()
                .dispatcher
                .clone()
                .expect("a dispatcher");
            TestMail::confirm(doc, &dispatcher);
            let outbox = TestMail::outbox(doc);
            let [(outgoing, None)] = outbox.as_slice() else {
                panic!("one message waits in the outbox: {outbox:?}");
            };
            outgoing.clone()
        };
        let (mut app, mut doc) = app();
        let (outgoing_tx, requests) = std::sync::mpsc::channel();
        let (_incoming, incoming_rx) = std::sync::mpsc::channel();
        app.engine = Some(EngineHandle {
            tx: outgoing_tx,
            rx: incoming_rx,
        });
        let outgoing = send(&mut app, &mut doc);
        app.show_status();
        assert!(app.send_notice, "the sending notice offers Undo");
        app.server_outbox()
            .borrow_mut()
            .deliver(outgoing, Box::new(|_| {}));
        app.flush_server_outbox();
        app.release_held_sends(&ui::widget::Edits::lend(&mut doc));
        assert!(requests.try_recv().is_err(), "it waits before it goes");
        assert!(
            app.next_wake_deadline().is_some(),
            "the window wakes to send it"
        );

        app.unsend = true;
        app.take_back_sends(&mut ui::widget::Edits::lend(&mut doc));
        let dispatcher = app
            .shared
            .borrow()
            .dispatcher
            .clone()
            .expect("a dispatcher");
        TestMail::confirm(&mut doc, &dispatcher);
        assert!(TestMail::outbox(&doc).is_empty(), "taken back unsent");
        assert!(app.compose.open, "back in the composer");
        app.flush_server_outbox();
        assert!(requests.try_recv().is_err(), "never sent");

        app.compose.close();
        let outgoing = send(&mut app, &mut doc);
        app.server_outbox()
            .borrow_mut()
            .deliver(outgoing, Box::new(|_| {}));
        app.flush_server_outbox();
        app.undo_time_is_up(&mut doc);
        assert!(
            matches!(requests.try_recv(), Ok(EngineRequest::Send { .. })),
            "after the undo time it goes"
        );
    }

    /// A forward carries the forwarded message's files: one on this device
    /// goes with it, and one not downloaded yet is named, not dropped
    /// unsaid.
    #[test]
    fn a_forward_carries_the_messages_files() {
        let (mut app, mut doc) = app();
        let mut message = app
            .shared
            .borrow()
            .mailbox
            .messages
            .values()
            .next()
            .expect("a seeded message")
            .clone();
        message.id = uuid::Uuid::new_v4();
        message.folder = Folder::Inbox;
        message.subject = "Figures".into();
        message.attachments = vec![
            Attachment::hydrated("q3.csv", "text/csv", b"quarter,total\n3,42\n".to_vec()),
            Attachment::projected("deck.pdf", "application/pdf", 2048),
        ];
        let id = message.id;
        app.shared.borrow_mut().mailbox.insert(message);

        app.open_forward(id);
        assert!(app.compose.open, "Forward opens the composer");
        let draft = app.compose.to_draft(&mut ui::widget::Edits::lend(&mut doc));
        let [file] = draft.attachments.as_slice() else {
            panic!(
                "the downloaded file goes with the forward: {:?}",
                draft.attachments
            );
        };
        assert_eq!(file.filename, "q3.csv");
        assert_eq!(
            crate::attachment_store::materialize(&app.content, file).unwrap(),
            b"quarter,total\n3,42\n"
        );
        let status = app.shared.borrow().status.clone().unwrap_or_default();
        assert!(
            status.contains("deck.pdf") && status.contains("not downloaded"),
            "the file left behind is named: {status}"
        );
        assert!(
            !app.compose.touched(),
            "a forward nobody changed leaves no draft when closed"
        );
    }

    /// A forward says truly why a file stays behind: a downloaded file past
    /// what one message carries is too large, not "not downloaded", and a
    /// file whose bytes another one already carries is not left behind.
    #[test]
    fn a_forward_names_a_file_too_large_to_carry_as_too_large() {
        let (mut app, mut doc) = app();
        let mut message = app
            .shared
            .borrow()
            .mailbox
            .messages
            .values()
            .next()
            .expect("a seeded message")
            .clone();
        message.id = uuid::Uuid::new_v4();
        message.folder = Folder::Inbox;
        message.subject = "Footage".into();
        let past_one_message = vec![0_u8; crate::attachment_store::MAX_ATTACHMENT_BYTES + 1];
        message.attachments = vec![
            Attachment::hydrated("notes.txt", "text/plain", b"take two".to_vec()),
            Attachment::hydrated("copy of notes.txt", "text/plain", b"take two".to_vec()),
            Attachment::hydrated("footage.mp4", "video/mp4", past_one_message),
        ];
        let id = message.id;
        app.shared.borrow_mut().mailbox.insert(message);

        app.open_forward(id);
        let draft = app.compose.to_draft(&mut ui::widget::Edits::lend(&mut doc));
        let [file] = draft.attachments.as_slice() else {
            panic!(
                "the notes go with the forward once: {:?}",
                draft.attachments
            );
        };
        assert_eq!(file.filename, "notes.txt");
        let status = app.shared.borrow().status.clone().unwrap_or_default();
        assert!(
            status.contains("footage.mp4") && status.contains("more than one message can carry"),
            "the downloaded file too large to carry is named as too large: {status}"
        );
        assert!(
            !status.contains("not downloaded"),
            "nothing here waits to be downloaded: {status}"
        );
        assert!(
            !status.contains("copy of notes.txt"),
            "the notes' bytes go with the forward: {status}"
        );
    }

    /// A saved draft, opened again and sent, is done with: it leaves
    /// Drafts, and the mailbox's draft is empty for the next message.
    #[test]
    fn a_sent_draft_leaves_drafts() {
        let (mut app, mut doc) = app();
        TestMail::store_draft(
            &mut doc,
            &crate::model::Draft {
                to: "dest@there.example".into(),
                subject: "Ready to go".into(),
                body: "See you at noon".into(),
                ..Default::default()
            },
        );
        app.hydrate_domain_state_now(&mut ui::widget::Edits::lend(&mut doc));
        let in_drafts = |app: &MailApp| -> Vec<MessageId> {
            app.shared
                .borrow()
                .mailbox
                .messages_in(Folder::Drafts)
                .iter()
                .filter(|message| message.subject == "Ready to go")
                .map(|message| message.id)
                .collect()
        };
        let [id] = in_drafts(&app)[..] else {
            panic!("the saved draft shows in Drafts, once");
        };

        app.open_draft(id);
        app.compose.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        app.send_compose(&mut ui::widget::Edits::lend(&mut doc));
        let outbox = TestMail::outbox(&doc);
        assert!(
            matches!(outbox.as_slice(), [(sent, None)] if sent.draft.subject == "Ready to go"),
            "the draft went to the outbox: {outbox:?}"
        );
        assert!(
            TestMail::load_draft(&doc).is_empty(),
            "sending the draft empties it: {:?}",
            TestMail::load_draft(&doc)
        );
        app.hydrate_domain_state_now(&mut ui::widget::Edits::lend(&mut doc));
        assert!(
            in_drafts(&app).is_empty(),
            "the sent message no longer shows in Drafts"
        );
    }

    impl MailApp {
        /// The undo time of every held message is up: they go.
        fn undo_time_is_up(&mut self, doc: &mut AutoCommit) {
            for held in &mut self.held_sends {
                held.due = ui::time::now();
            }
            self.release_held_sends(&ui::widget::Edits::lend(doc));
        }
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
            for (id, kind) in [
                (refresh_id, EngineRequestKind::Fetch),
                (send_id, EngineRequestKind::Send),
                (unrelated_id, EngineRequestKind::StoreFlag),
            ] {
                app.pending_engine_requests.insert(
                    id,
                    PendingEngineRequest {
                        kind,
                        account: "me@example.test".into(),
                    },
                );
            }
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

    /// A failure that arrives after the person switched accounts is said of
    /// the account that asked, not of the one selected now.
    #[test]
    fn a_late_engine_error_stays_with_the_account_that_asked() {
        let (mut app, mut doc) = app();
        let (outgoing, outgoing_rx) = std::sync::mpsc::channel();
        let (incoming, incoming_rx) = std::sync::mpsc::channel();
        app.engine = Some(EngineHandle {
            tx: outgoing,
            rx: incoming_rx,
        });
        let asking = crate::model::AccountConfig {
            email: "asking@example.test".into(),
            ..Default::default()
        };
        let request = app
            .post_engine(EngineRequest::fetch(asking, Folder::Inbox))
            .expect("the fetch went to the engine");
        assert_eq!(
            outgoing_rx.try_recv().unwrap().account(),
            "asking@example.test"
        );
        app.shared.borrow_mut().accounts =
            crate::account::AccountStore::single(crate::model::AccountConfig {
                email: "selected@example.test".into(),
                ..Default::default()
            });
        incoming
            .send(EngineEvent::Error {
                request_id: request,
                kind: EngineRequestKind::Fetch,
                context: "fetch".into(),
                message: "authentication failed".into(),
            })
            .unwrap();
        app.drain_engine(&mut ui::widget::Edits::lend(&mut doc));
        let status = app.shared.borrow().status.clone().unwrap_or_default();
        assert!(status.contains("asking@example.test"), "{status}");
        assert!(!status.contains("selected@example.test"), "{status}");
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
            let mut state = MailState::new_sample(crate::avatars::Avatars::new());
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
        {
            let (app, _) = app();
            assert!(app.shared.borrow().selected.is_some());
            assert_eq!(app.shared.borrow().folder, Folder::Inbox);
        }

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

        // Phase `narrow_mailboxes_open_over_a_settled_layout`: the header
        // rows of the columns behind the mailboxes screen are laid out
        // hidden, and the layout still settles (it panics in
        // `layout_for_test` otherwise).
        {
            let (mut app, _doc) = app();
            app.nav.open_sidebar();
            app.nav.snap();
            app.layout_for_test(560.0, 860.0);
            assert!(app.shared.borrow().narrow);
        }

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

            // Back is the window bar's slot; Escape goes back too.
            assert!(
                app.nav.can_go_back() && !app.nav.window_toolbar().leading.is_empty(),
                "Back shown in the window's bar while the message is pushed"
            );
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

        {
            let (mut app, mut doc) = app();
            app.layout_for_test(1280.0, 800.0);
            app.shared
                .borrow_mut()
                .push_command(Command::OpenFolder(0, Folder::Sent));
            app.apply_commands(&mut ui::widget::Edits::lend(&mut doc));
            assert_eq!(app.shared.borrow().folder, Folder::Sent);
            app.sync(&doc, &automerge::ROOT);
            let state = app.shared.borrow();
            let expected = state
                .selected
                .and_then(|id| state.mailbox.get(id))
                .map_or_else(|| "Sent".to_owned(), |message| message.subject.clone());
            assert_eq!(
                crate::window_bar::location(&state, None),
                expected,
                "the window's location names the open message, else the folder"
            );
        }

        // Phase `narrow_list_reaches_the_mailboxes_by_the_bars_toggle`: too
        // narrow to dock the mailboxes beside the list, they are the push
        // stack's first screen. The list is the top-level page: the window
        // bar's sidebar toggle reaches the mailboxes, with no Back beside it.
        {
            let (mut app, _doc) = app();
            app.layout_for_test(560.0, 860.0);
            let geometry = app.nav.geometry();
            assert_eq!(geometry.sidebar, ui::widget::SidebarPlacement::Screen);
            assert_eq!(app.nav_pane(), NavPane::Overview, "opens on the list");
            let toolbar = app.nav.window_toolbar();
            assert_eq!(toolbar.sidebar, Some(false), "the toggle is the bar's");
            assert!(
                !app.nav.can_go_back() && toolbar.leading.is_empty(),
                "no Back on the top-level list"
            );
            assert!(!app.nav.is_sidebar_open());
            assert!(
                app.nav
                    .window_toolbar_event(&ui::window_toolbar::ToolbarEvent::ToggleSidebar),
                "the toggle is answered"
            );
            app.nav.snap();
            app.layout_for_test(560.0, 860.0);
            assert_eq!(
                app.nav_pane(),
                NavPane::Sidebar,
                "the toggle shows the mailboxes"
            );
            assert!(app.nav.is_sidebar_open(), "the mailboxes fill the window");
            let sidebar = app.nav.geometry().sidebar_column;
            assert!(
                sidebar.x0.abs() < 2.0 && (sidebar.width() - 560.0).abs() < 2.0,
                "the mailboxes are the full-width front screen: {sidebar:?}"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn accessibility_routes_panes_reader_attachments_and_context_menu() {
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
            // Beside the list the composer stands where the message is read:
            // a part of the window, the list still published beside it.
            let window = app.accessibility().expect("window semantics");
            assert_eq!(window.role, A11yRole::Window);
            let compose = window
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some("Compose email"))
                .cloned()
                .expect("the composer is published in the window");
            assert_eq!(compose.role, A11yRole::Group);
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
        }

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
            let error = hosted
                .harness()
                .call_on("mail.mailbox", "mail.operation", serde_json::json!({}))
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
            let dispatcher = app
                .shared
                .borrow()
                .dispatcher
                .clone()
                .expect("a dispatcher");
            assert!(
                dispatcher.take_asked().expect("builds").is_empty(),
                "the person's own Send asks nothing: Undo is its way back"
            );
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
            app.undo_time_is_up(&mut doc);
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

            app.send_compose(&mut ui::widget::Edits::lend(&mut doc));
            TestMail::confirm(&mut doc, &dispatcher);
            let outbox = TestMail::outbox(&doc);
            assert!(
                matches!(outbox.as_slice(), [(again, None)] if again.id != outgoing.id),
                "{outbox:?}"
            );
        }

        {
            let (mut app, mut doc) = app();
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
            let id = app.editing_draft.expect("editing the saved draft");
            app.open_draft(id);
            app.save_compose_draft(&mut ui::widget::Edits::lend(&mut doc));
            assert_eq!(
                app.shared.borrow().mailbox.count(Folder::Drafts),
                after_first,
                "re-saving updates in place"
            );
        }

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

        // Phase `a_file_chosen_by_contents_is_attached`: where files have no
        // paths (a browser page) the chooser hands over contents, which are
        // staged like an opened file's.
        {
            let (mut app, mut doc) = app();
            app.compose.open_blank();
            let (sender, receiver) = std::sync::mpsc::channel();
            sender
                .send(ui::portal::ContentsOutcome::Picked(vec![
                    ui::portal::PickedContents {
                        name: "notes.txt".into(),
                        media_type: None,
                        bytes: b"attached by contents".to_vec(),
                        location: None,
                    },
                ]))
                .unwrap();
            app.compose_attachment_contents = Some(receiver);
            assert!(app.next_wake_deadline().is_some(), "the answer wakes");
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            assert!(app.compose_attachment_contents.is_none());
            assert_eq!(app.nav.toast_text(), Some("Attached notes.txt"));
            let attachments = app
                .compose
                .to_draft(&mut ui::widget::Edits::lend(&mut doc))
                .attachments;
            assert_eq!(attachments.len(), 1);
            assert_eq!(attachments[0].filename, "notes.txt");
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

            assert!(dispatch(&mut app, &esc_event(), &mut doc));
            assert!(app.bulk_purge_confirmation.is_none());
            assert!(
                ids.iter()
                    .all(|id| app.shared.borrow().mailbox.get(*id).is_some())
            );

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
                synced: crate::sync::Synced::default(),
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
                synced: crate::sync::Synced::default(),
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

    /// Sample mode keeps its accounts in a sandbox, never the real store; a
    /// live window keeps them in its host's profile.
    #[test]
    fn account_roots_follow_each_mode() {
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
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn presentation_state_stays_local() {
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

        {
            let mut first = MailApp::new_first_run();
            let mut doc = crate::resource::TestMail::doc();
            first.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            first.sync(&doc, &automerge::ROOT);
            first.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            first.sync(&doc, &automerge::ROOT);
            first.layout_for_test(1100.0, 780.0);
            assert!(
                first.is_first_run(),
                "no configured account → first-run onboarding"
            );
            let semantics = first
                .first_run
                .accessibility()
                .expect("first-run semantics");
            assert_eq!(semantics.name.as_deref(), Some("No email account yet"));
            assert!(semantics.children.iter().any(|node| {
                node.role == ui::a11y::Role::Button
                    && node.name.as_deref() == Some("Open External Accounts")
            }));
            assert!(
                first.first_run.perform_action(
                    &ui::a11y::Action::Activate,
                    &[],
                    &mut ui::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                ),
                "assistive activation reaches the link"
            );
            first.sync(&doc, &automerge::ROOT);
            assert_eq!(
                first.opened_links,
                ["settings://external_accounts"],
                "the empty state takes the person to Settings' External Accounts"
            );
            assert!(first.is_first_run(), "Mail opened no editor of its own");

            // With accounts, the sidebar has no "+" of its own, and an
            // account's own action goes to the same place.
            let (mut app, mut doc) = app();
            app.layout_for_test(1100.0, 780.0);
            assert!(
                find_access(&app, &|node| node.name.as_deref() == Some("Add account")).is_none(),
                "the sidebar adds no account itself"
            );
            app.shared
                .borrow_mut()
                .push_command(Command::AccountSettings);
            app.apply_commands(&mut ui::widget::Edits::lend(&mut doc));
            assert_eq!(app.opened_links, ["settings://external_accounts"]);
        }

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
    fn keyboard_shortcuts_drive_the_visible_triage_actions() {
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
            Some(format!("Saved a copy as {name}").as_str())
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
}
