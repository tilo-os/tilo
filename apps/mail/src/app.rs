//! `MailApp` — the window — and the [`Mailroom`] it shares with the view's
//! effect handler: the engine and its requests, the server search, the mail
//! kept on this device and what effects ask of the window until its next
//! sync. The view ([`window`]) answers effects and publishes rows and search
//! entries from the mailroom.
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
#[path = "peer_copies.rs"]
mod peer_copies;
use peer_copies::kept_baseline;

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
use crate::state::{BulkAction, Command, CtxAction, Intent, MailState, QueuedMessage, Shared};
use crate::util::format_absolute;

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

/// A wake for a worker thread: it wakes the window once the view has
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
        "send" | "smtp" | crate::engine::SMTP_UNREACHED => {
            format!("Couldn’t send the message: {message}")
        }
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
    /// What the window shares with the view's effect handler and what it
    /// publishes.
    room: Rc<RefCell<Mailroom>>,
    /// The mailbox as the document holds it, shown as it changes; `None`
    /// for a window outside the framework.
    live: Option<ui::framework::Live<Mail>>,
    /// The `(revision, loads)` of the document last shown.
    presented: Option<(u64, u64)>,
    /// Where draft attachment bytes live; drafts carry only their content id.
    /// The engine worker holds a clone to build outgoing MIME.
    content: ContentStore,
    nav: NavShell,
    nav_node: Option<NodeId>,
    /// The window bar's actions as last declared, and the command each
    /// queues.
    bar: ui::framework::BarCommands<Command>,
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
    /// The drafts of their own shown in Drafts, by id.
    projected_drafts: RefCell<HashSet<MessageId>>,
    /// The row menu, and the message it acts on while open.
    menu: ContextMenu,
    menu_target: Option<MenuTarget>,
    /// A row menu to open once the list has been laid out (a capture).
    pending_menu: Option<MessageId>,
    /// An explicit destructive confirmation for the selection toolbar. This
    /// is local presentation state; the actual purge remains one canonical
    /// verb of the mailbox, only after the person confirms.
    bulk_purge_confirmation: Option<PurgeConfirmation>,
    /// What the document still has to tell mail servers, on its way to the
    /// engine.
    server_outbox: Rc<RefCell<ServerOutbox>>,
    /// The drain that fills the outbox, told when a server may now accept
    /// what it refused (a fetch arrived, the engine reconnected).
    projection: Option<ui::framework::Projection>,
    /// The drain that hands the document's outbox to the engine.
    delivery: Option<ui::framework::Projection>,
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
    /// The device-local account id last flushed to [`AccountsPersist`]. Pane,
    /// accessibility, and resource operations all select through `MailState`,
    /// so observing the id here gives every selection path one persistence
    /// boundary without leaking that preference into Automerge.
    persisted_active_account: Option<String>,
    /// The person's accounts, which Settings keeps and every device of theirs
    /// shares, followed in a live window; and the revision last shown.
    person_accounts: Option<(PersonRecord<PersonAccounts>, Option<u64>)>,
    /// The contact directory revision and own addresses the sender pictures
    /// were last taken from.
    sender_pictures_seen: Option<(u64, Vec<String>)>,
    /// The person's Mail preferences, followed in a live window: the compose
    /// format and the folded account sidebars.
    preferences: Option<ui::framework::Setting<crate::resource::MailPreferences>>,
    /// The preferences as this window last read or stored them.
    followed_preferences: crate::resource::MailPreferences,
    /// The trusted senders record, followed in a live window.
    pub(crate) trusted_senders: Option<crate::trusted_senders::Followed>,
    connect_requested: bool,
    background_poll_deadline: Cell<Option<ui::time::Instant>>,
    /// Wakes the window from the engine's and the account sheet's worker
    /// threads, so a result is drawn the moment it arrives and nothing
    /// polls while the network is slow. `None` until the view
    /// hands it over; the workers then fall back to the frame poll.
    waker: Arc<std::sync::Mutex<Option<ui::WakeHandle>>>,
    /// What the document and the mailbox were when their flags were last
    /// brought together, so an unchanged frame does not read the record.
    hydrated_at: RefCell<Option<(Vec<ui::automerge::ChangeHash>, usize, u64)>>,
    /// One portal-open attachment staging request. The returned descriptor is
    /// copied straight into the content store; its original path is never retained.
    compose_attachment_rx: Option<OpenFileChannel>,
    /// The file to attach, as its contents, where files have no paths to
    /// open (a browser page).
    compose_attachment_contents: Option<ui::portal::ContentsChannel>,
    /// What search hits and agents asked to open, taken up on the next
    /// settle.
    opens: std::rc::Rc<std::cell::RefCell<Vec<crate::actions::MailTarget>>>,
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
    /// The mail the person's devices hold for one another, where they have
    /// a room of their own ([`crate::copies`]).
    copies: Option<crate::copies::Copies>,
    /// The messages whose notification the person clicked, from whatever
    /// thread the host reported it on; opened on the next settle.
    activations: Arc<std::sync::Mutex<Vec<String>>>,
    /// The host's notifications, once a live window follows them.
    notifier: Option<Arc<dyn ui::notify::Notifier>>,

    rect: Rect,
    /// The host theme captured during layout, for the overlays laid out
    /// outside the taffy tree.
    layout_theme: Theme,
    /// The shell starts docked; the first real layout snaps it to its
    /// resting state for the window's width so a narrow launch does not
    /// animate the drawer closed.
    first_layout: bool,
    dirty: bool,
    /// The layout tree a test laid the app out in, for routing events.
    #[cfg(test)]
    test_layout: Option<(TaffyTree<()>, NodeId)>,
    /// The links the app opened, in order, where a test reads them.
    #[cfg(test)]
    opened_links: Vec<&'static str>,
}

/// What the window and the view's effect handler share: the engine and the
/// requests posted to it, the server search, the attachments kept on this
/// device and what effects ask of the window, waiting for its next sync.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent flags: the engine's outstanding request, the document's draft, the open composer and the redraw"
)]
pub(crate) struct Mailroom {
    pub(crate) shared: Shared,
    /// Opens the platform search from the app's own input handling.
    search: SearchOpener,

    /// The engine worker, in a live window.
    engine: Option<EngineHandle>,
    /// Where account changes persist. The live app targets the real user config
    /// root; the sample / shots / test constructors target a throwaway sandbox
    /// so they can never write into the real account store.
    accounts_persist: AccountsPersist,
    /// The person's contact directory, followed in a live window: the
    /// recipients a contact draft can be prepared for.
    pub(crate) contact_directory: Option<PersonRecord<tilo_contact_directory::ContactDirectory>>,
    /// The document holds a draft already, so a contact draft would
    /// replace it.
    pub(crate) has_contact_draft_conflict: bool,
    /// True between posting a connect/fetch and receiving its result, so the
    /// app only keeps polling the worker while a reply is outstanding (it fully
    /// idles otherwise instead of busy-looping). `pending_engine_requests`
    /// rather than an event-derived boolean is the source of truth: a connect
    /// can emit several events, while successful IMAP writes emit none besides
    /// their terminal worker acknowledgement.
    awaiting_engine: bool,
    pending_engine_requests: HashMap<EngineRequestId, PendingEngineRequest>,
    /// The one refresh invocation currently owned by the resource-operation
    /// host.  It remains occupied until the host has polled its terminal
    /// update, so a subsequent refresh cannot overwrite an unobserved result.
    resource_refresh_invocation: Option<(EngineRequestId, tilo_actions::InvocationId)>,
    resource_refresh_result: Option<tilo_actions::InvocationUpdate>,
    /// What effects asked to show that needs the document, for the next sync.
    reveals: Vec<Reveal>,
    /// Callers waiting for a refresh to settle.
    effect_replies: Vec<(tilo_actions::InvocationId, ui::framework::EffectReply)>,
    /// Where fetched mail is kept on this device.
    store: crate::store::Store,
    /// Text to search the servers for once the person stops typing it, and
    /// when; and the text last searched for.
    server_search: Option<(String, ui::time::Instant)>,
    server_searched: String,
    /// Wall-clock minute of the last automatic inbox refresh, so periodic sync
    /// fires at most once per [`AUTO_REFRESH_MIN`] while the app is in use.
    last_auto_refresh_min: i64,
    /// The screenshot scenario the framework asked to stage, until the next
    /// sync stages it with the document.
    pending_stage: Option<String>,
    /// Outbox messages the engine is delivering, by request.
    pending_send: HashMap<EngineRequestId, PendingSend>,
    /// Whether the composer is open, as the window last showed it.
    composing: bool,
    /// Something changed that the window has yet to draw.
    dirty: bool,
}

impl MailApp {
    /// What the window shares with the view's effect handler.
    pub(crate) fn state(&self) -> std::cell::RefMut<'_, Mailroom> {
        self.room.borrow_mut()
    }

    /// Take up what changed in the document since the window last showed
    /// it: whether it holds a draft a contact draft would replace.
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

    /// Start the effect a published operation's `payload` names, as the
    /// view does when it is invoked, and take up what it asked of the
    /// window, as its next sync does.
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

    /// The store itself, for the view that shares it.
    pub(crate) fn room(&self) -> Rc<RefCell<Mailroom>> {
        Rc::clone(&self.room)
    }

    /// Attach the framework dispatcher, which the window and its composer
    /// edit the document through.
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

    /// Whether workers can wake the window, so nothing has to poll them.
    fn workers_wake(&self) -> bool {
        self.waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    /// A window someone uses: the accounts of the host's profile, and the
    /// engine worker that keeps them current. No fabricated messages.
    pub(crate) fn for_window(
        storage: &ui::storage::AppStorage,
        content: Option<ContentStore>,
    ) -> Self {
        let persist = storage
            .config_root()
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
        let store = storage.files().paths().cache().native_path().map_or_else(
            |_| crate::store::Store::nowhere(),
            |cache| crate::store::Store::at(cache.join("mail")),
        );
        app.shared.borrow_mut().kept = store.clone();
        app.state().store = store;
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
            attachment_opening: None,
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

    /// Two messages waiting to leave, one of them refused by a server, in
    /// the outbox: what the page shows, and the one place the verb that
    /// takes a message back is.
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
}

impl Mailroom {
    /// What the engine reported since it was last asked.
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
}

impl MailApp {
    /// Select all reads the whole folder: the rows held now, and, while the
    /// folder has pages on its server this device has not read, one after
    /// another, each joining the selection until the folder is whole.
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
            st.status = Some("Reading the rest of this folder…".into());
            st.status_undo = false;
            drop(st);
            self.load_older();
        } else {
            let noun = Self::folder_noun(folder, all_inboxes);
            st.finish_selection(&noun);
        }
        self.dirty = true;
    }

    /// The account, folder and both-inboxes state the list shows.
    fn open_listing(&self) -> (AccountConfig, Folder, bool) {
        let st = self.shared.borrow();
        (st.accounts.active_config(), st.folder, st.all_inboxes)
    }

    /// What "all of it" is called: the folder's own name, or both inboxes'.
    fn folder_noun(folder: Folder, all_inboxes: bool) -> String {
        if all_inboxes {
            "these folders".to_owned()
        } else {
            folder.title().to_owned()
        }
    }

    /// Select all reads the whole folder, not the page of it this device
    /// happens to hold: while pages are still on their way it asks for the
    /// next one and lets the rows that arrive join the selection, and when
    /// there are none left it says how many it took.
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
            let noun = Self::folder_noun(folder, all_inboxes);
            self.shared.borrow_mut().finish_selection(&noun);
        }
        self.dirty = true;
    }

    /// Whether the open folder has pages on its server this device has not
    /// read. Without a server, or before the folder's numbering is known,
    /// what the device holds is the whole of it.
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

    /// What the document's outbox holds now: the messages every account
    /// still has to send, with a server's refusal where one refused them.
    /// The outbox page and the sidebar's count both read this.
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
                    "No subject".to_owned()
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

    /// Accept only events that match a request still pending in this app. A
    /// worker event is untrusted transport input: a malformed, delayed, or
    /// out-of-order result must not update a different operation's UI state.
    fn expects_engine_event(&self, request_id: EngineRequestId, kind: EngineRequestKind) -> bool {
        self.state()
            .pending_engine_requests
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

    /// Queue what the person did in the window's bar and apply it with the
    /// document in hand, as the chrome's commands are applied.
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
                    self.state().post_engine(req);
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
                .state()
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
        if let Some(id) = self.compose.draft_id().map(str::to_owned) {
            // A draft of its own shows as itself in Drafts already.
            self.shared
                .borrow_mut()
                .save_draft_as(doc, Some(&id), &draft);
            self.shared.borrow_mut().status = Some("Saved to Drafts".into());
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

    /// `draft` as the Drafts message `id` that shows it.
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
        }
    }

    /// Show every draft of its own as the Drafts message with its id, and
    /// drop the ones that left the drafts. Only what differs is changed.
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
        // A draft's copy on its server, fetched with Drafts, is the draft
        // itself: it shows once, as the draft.
        for copy in mail.draft_copy_ids() {
            if let Ok(copy) = copy.parse::<MessageId>() {
                let mut st = self.shared.borrow_mut();
                if st.mailbox.get(copy).is_some() {
                    st.mailbox.purge(copy);
                    st.dirty = true;
                }
            }
        }
    }

    /// Whether the app is at first run (no account configured and no mail
    /// held) and should show the onboarding screen rather than an empty
    /// mailbox. Mail the record holds is shown whether or not an account is
    /// configured: a past version reads no accounts, and held mail stays
    /// readable after its account is gone.
    fn is_first_run(&self) -> bool {
        let state = self.shared.borrow();
        state.accounts.is_empty() && state.mailbox.messages.is_empty()
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
}

impl Mailroom {
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
                self.accounts_persist.root().as_deref(),
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
}

impl MailApp {
    /// Hand a newly-set status to the shell, which shows it as a toast and
    /// lets it fade on its own.
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
            self.nav
                .toast_with_action("Sending…", ToastKind::Info, "undo-2", "Undo");
        }
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
        synced: crate::sync::Synced,
    ) {
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
            self.state().accounts_persist.root().as_deref(),
            account_id,
            accounts::AccountSyncHealth::Healthy,
            true,
            Some("Mail is up to date"),
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
            let kept = self.state().store.load(&email);
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
            .filter_map(|message| kept_baseline(&mail, message))
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
            .state()
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
        report_account_sync(
            self.state().accounts_persist.root().as_deref(),
            &account_id,
            health,
            false,
            Some(&detail),
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

    /// Hand an outbox message to the engine on the account it is sent from.
    /// One whose account this device does not hold is left for a device
    /// that does.
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

    /// Keep `updated` as the person's mail accounts: through their accounts
    /// record, which follows them to every device, with each password in
    /// this device's vault and the device's account file following the
    /// record. Outside a live window, in the store this window keeps.
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

    fn adopt_person_accounts(&mut self) {
        let root = self.state().accounts_persist.root();
        let Some((record, seen)) = &mut self.person_accounts else {
            return;
        };
        let revision = record.revision();
        if *seen == Some(revision) {
            return;
        }
        *seen = Some(revision);
        let person = record.get();
        let vault = root.map_or_else(accounts::AccountCredentials::open, |root| {
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
                self.state().store.forget(id);
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
        } else if self.state().engine.is_some() && (active_changed || !self.connect_requested) {
            let (cfg, folder) = {
                let st = self.shared.borrow();
                (st.accounts.active_config(), st.folder)
            };
            self.state()
                .post_engine(EngineRequest::connect(cfg.clone()));
            self.state().post_engine(EngineRequest::fetch(cfg, folder));
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
        self.project_drafts(&mail);
    }
}

impl Mailroom {
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
}

impl MailApp {
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
        self.state().accounts_persist.root()
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

/// Record `account_id`'s sync health in the profile at `config_root`; a
/// window without a profile directory (a capture, a past version) records
/// nothing.
fn report_account_sync(
    config_root: Option<&std::path::Path>,
    account_id: &str,
    health: accounts::AccountSyncHealth,
    _success: bool,
    _message: Option<&str>,
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

    /// The outbox page: a row per message still to leave, each saying why it
    /// has not, and the verb that takes it back unsent.
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
                        .with_name(format!("To {}: {}", message.to, message.subject))
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
                    .with_name(format!("Cancel sending to {}", message.to))
                    .with_action_target(format!("mail.outbox-cancel:{}", message.id))
                    .with_state(AccessState::new().focusable(true)),
            );
            children.push(
                AccessNode::new(Role::Group)
                    .with_name("Queued message")
                    .with_description(match &message.reason {
                        Some(reason) => format!("The server refused this message: {reason}"),
                        None => "This message has not been sent yet.".to_owned(),
                    })
                    .with_value(message.body.clone()),
            );
        }
        children.extend(rows);
        let mut list = AccessNode::new(Role::List)
            .with_name("Outbox")
            .with_children(children);
        if total == 0 {
            list = list.with_description("Nothing is waiting to be sent");
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
        self.follow_document();
        // What effects asked of the window since it last synced.
        self.process_intents();
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
            // Live: kick off a connect + inbox fetch for the active account.
            let has_accounts = !self.shared.borrow().accounts.is_empty();
            if self.state().engine.is_some() && has_accounts && !self.connect_requested {
                let cfg = self.shared.borrow().accounts.active_config();
                {
                    // Nothing kept for the account: its rows are on the way.
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

        if self.state().awaiting_engine {
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
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        // Worker and native-picker completions are not animation. Keep their
        // deadline fixed until sync consumes a tick, even with reduced motion.
        // Workers that wake the window themselves are not polled at all.
        let workers_wake = self.workers_wake();
        let pending = (!workers_wake && self.state().awaiting_engine)
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
        (self.state().awaiting_engine && !self.workers_wake())
            || self.shared.borrow().status.is_some()
            || self.shared.borrow().wants_older.get()
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

/// Who a queued message goes to, as one line: every recipient list it has.
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
        let searched = self.state().search.take();
        searched.or_else(|| {
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
    DraftId(String),
    Attachment(MessageId, usize),
    Invitation(Draft),
    Contact(Draft),
}

impl Mailroom {
    /// Start `effect`, invoked as `invocation`. Showing something answers at
    /// once; a refresh answers once the server replied, through
    /// [`Self::take_effect_result`]. Saving an attachment is the view's,
    /// which asks the host where to put it.
    ///
    /// # Errors
    /// Why the effect does not apply to what this window has loaded now: a
    /// message, folder or account that is gone, no connected account.
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
                self.attachment_to_save(&message, index)?;
                Err("saving an attachment asks the host where to put it".into())
            }
            MailEffect::SearchMessages { .. }
            | MailEffect::ExportContent { .. }
            | MailEffect::ReadMessage { .. }
            | MailEffect::DraftReply { .. }
            | MailEffect::Content { .. } => {
                Err("messages are answered from what this device keeps and the mailbox".into())
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

    /// Show `target`, once it is still there.
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
            MailTarget::Attachment { message, index } => {
                let id = message
                    .parse::<MessageId>()
                    .map_err(|_| "invalid message id")?;
                self.attachment_to_save(&message, index)?;
                self.reveals.push(Reveal::Attachment(id, index));
            }
        }
        self.dirty = true;
        Ok(())
    }

    /// Open the composer replying to, replying to all of, or forwarding the
    /// loaded `message`.
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
            MailApp::attachment_save_name(&attachment.filename),
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
}

impl MailApp {
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

    /// Take up what effects asked to show that needs the document.
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
                        self.shared.borrow_mut().status = Some("That draft is gone".into());
                    }
                }
                Reveal::Attachment(message, index) => self.open_attachment(message, index),
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

    /// Open attachment `index` of `message` in the app that shows its kind
    /// (a picture in Photos, a PDF in the PDF viewer), handing it over as
    /// a file of this app's own.
    fn open_attachment(&mut self, message: MessageId, index: usize) {
        let (name, bytes) = match self.state().attachment_to_save(&message.to_string(), index) {
            Ok(found) => found,
            Err(error) => {
                self.shared.borrow_mut().status = Some(error);
                return;
            }
        };
        let opening = self.state().store.opening(&name);
        let Some(path) = opening else {
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
        if let Err(error) = self.state().attachment_to_save(&message.to_string(), index) {
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
}

impl Mailroom {
    /// The rows this window shows beside the mailbox: the accounts, an
    /// in-flight send and a search-index fault.
    pub(crate) fn live_rows(&self) -> Vec<ui::framework::LiveRow> {
        let mut rows = crate::resource::live_extras(self);
        rows.push(crate::actions::accounts_collection(&self.accounts_rows()));
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
                    "(no subject)".into()
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
            "select-all" if mail.bulk_selecting => {
                drop(mail);
                self.select_all_in_folder();
                self.process_intents();
                return true;
            }
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
#[path = "server_deltas.rs"]
mod server_deltas;
use server_deltas::{first_fetch, server_changes, take_found};

#[cfg(test)]
mod tests;

mod view;
pub(crate) use view::window;
