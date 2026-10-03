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
//! and the push stack. The sidebar is a [`MailSidebar`], the list a
//! [`MessageList`] and the message a [`MessageReader`]; they share one
//! [`MailState`] and raise [`Intent`]s and [`Command`]s for effects that
//! reach past their own column.
//!
//! On top of the shell the app layers the transient overlays it owns directly:
//! the movable [`ComposeWindow`], the row context menu, the account-setup
//! sheet, and a status toast.
//!
//! The live app loads a real [`AccountStore`] and drives the account
//! [`crate::engine`] worker (IMAP fetch / SMTP submit; a network-free stub by
//! default). It does **not** fabricate a sample mailbox — an unconfigured
//! account shows an empty state. The bundled sample corpus is used only by the
//! hermetic `--shots` / test constructors.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::TryRecvError;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size as TSize, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_content::ContentStore;
use ui::icons::Icon;
use ui::permissions::PermissionContext;
use ui::portal::{
    FileFilter, OpenFileChannel, OpenFileOutcome, PickRequest, PortalChannel, PortalOutcome,
};
use ui::presence::PresenceCtx;
use ui::search::{ScopeDecl, SearchOpener, SearchRequest};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    Button, ButtonFace, ContextMenu, EmptyState, MenuItem, ModalDialog, NavPane, NavShell,
    SegmentedFace, SidebarPlacement, TaffyTreeExt, Text, TextInput, Toggle, ToggleFace,
    ToggleGroup, Widget,
};
use ui::{ImageStore, TextRole};

use crate::account::{Account, AccountStore};
use crate::compose::{ComposeAction, ComposeEventContext, ComposeWindow};
use crate::doc::MailDoc;
use crate::engine::{EngineEvent, EngineRequest, EngineRequestId, EngineRequestKind};
use crate::header::CommandBar;
use crate::list::MessageList;
use crate::model::{
    AccountConfig, Addr, Attachment, ComposeFormat, Draft, Folder, Message, MessageId,
};
use crate::reader::MessageReader;
use crate::shortcuts_overlay::MailShortcutsOverlay;
use crate::sidebar::MailSidebar;
use crate::state::{BulkAction, Command, CtxAction, Intent, MailState, Shared};
use crate::util::format_absolute;

/// How often the app auto-refreshes the current folder from the server while in
/// use. Checked each active frame; a manual refresh (the `r` shortcut) forces
/// one immediately regardless.
const AUTO_REFRESH_MIN: i64 = 5;
/// How long a status toast ("Synced N messages", "Account saved", …) stays on
/// screen before it fades out on its own, like any normal transient toast.
const TOAST_TTL: std::time::Duration = std::time::Duration::from_millis(3500);
/// A loaded mail body can be huge, but Mail refuses to turn a resource action
/// into an unbounded filesystem write. Larger attachments can be downloaded
/// through Files after Mail exposes a proper streamed handoff.
const MAX_ATTACHMENT_SAVE_BYTES: usize = crate::attachment_store::MAX_ATTACHMENT_BYTES;

struct EventContext<'a> {
    scale_factor: f64,
    modifiers: winit::event::Modifiers,
    state: &'a mut AutoCommit,
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

/// A hit region in one of the app-owned overlays (setup sheet / context menu /
/// first-run onboarding).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OverlayHit {
    SetupField(usize),
    ToggleImapTls,
    ToggleSmtpTls,
    SetupSave,
    CancelSetup,
    /// Remove the account being edited (only shown when editing an existing one).
    RemoveAccount,
    /// Pick the account's sign-in method (password vs OAuth).
    AuthPassword,
    AuthOAuth,
    /// Trigger the OAuth authorization flow for the detected provider.
    OAuthSignIn,
}

/// The engine worker channel pair (live app only).
struct EngineHandle {
    tx: std::sync::mpsc::Sender<EngineRequest>,
    rx: std::sync::mpsc::Receiver<EngineEvent>,
}

/// A locally-authored message waiting for a real SMTP acknowledgement.  It is
/// deliberately kept out of Sent until the server accepts it: treating MIME
/// serialization as delivery makes an offline fallback (and a rejected SMTP
/// send) look like mail was sent.
struct PendingSend {
    request_id: EngineRequestId,
    message: Message,
    edited_draft: Option<MessageId>,
}

/// An attachment whose bytes were already hydrated and are now waiting for a
/// user-selected destination from the desktop portal. The path itself is never
/// invented or retained before the portal grants it.
struct PendingAttachmentSave {
    message_id: MessageId,
    index: usize,
    attachment: Attachment,
}

/// Where account add / edit / remove is persisted.
///
/// Only the **live** app may write the user's real account store
/// (`$XDG_CONFIG_HOME/tilo/accounts/`). The sample / `--shots` / test
/// constructors target a throwaway sandbox directory instead, so exercising the
/// save flow (or a test that adds an account) can never leak fabricated demo
/// accounts into the real store. The historic leak — `you@example.test`,
/// `you@hooli.example`, `new@host.example` appearing pre-configured in the live
/// app — was exactly this: a test that drove the save path wrote the bundled
/// sample store to the real config root.
#[derive(Clone)]
enum AccountsPersist {
    /// The real per-user config root — live app only.
    Real,
    /// A throwaway directory, removed when the app is dropped. The `TempDir`
    /// guard owns the directory's lifetime.
    Sandbox(Arc<tempfile::TempDir>),
    /// No writable sandbox could be created — persistence is a silent no-op.
    /// A safe fallback that still never touches the real store.
    Disabled,
}

impl AccountsPersist {
    /// A fresh throwaway sandbox for the sample / shots / test constructors.
    /// Falls back to [`AccountsPersist::Disabled`] (never [`AccountsPersist::Real`])
    /// if a temp dir cannot be created, so sample mode can never reach the real
    /// store.
    fn sandbox() -> Self {
        match tempfile::Builder::new()
            .prefix("tilo-mail-sample-")
            .tempdir()
        {
            Ok(dir) => AccountsPersist::Sandbox(Arc::new(dir)),
            Err(_) => AccountsPersist::Disabled,
        }
    }

    /// Persist `accounts` to the target this variant names.
    fn persist(&self, accounts: &AccountStore) -> std::io::Result<()> {
        match self {
            AccountsPersist::Real => accounts.persist(),
            AccountsPersist::Sandbox(dir) => accounts.persist_in(dir.path()),
            AccountsPersist::Disabled => Ok(()),
        }
    }

    /// The device-local preference lives beside the account store, but outside
    /// its shared account schema. It is never a collaborative document field.
    fn load_compose_format(&self) -> ComposeFormat {
        let Some(path) = self.local_preferences_path() else {
            return ComposeFormat::Html;
        };
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<LocalPreferences>(&bytes).ok())
            .map_or(ComposeFormat::Html, |prefs| {
                ComposeFormat::from_key(&prefs.compose_format)
            })
    }

    fn persist_compose_format(&self, format: ComposeFormat) -> std::io::Result<()> {
        let Some(path) = self.local_preferences_path() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec(&LocalPreferences {
            compose_format: format.as_key().to_string(),
        })
        .map_err(std::io::Error::other)?;
        std::fs::write(path, bytes)
    }

    fn local_preferences_path(&self) -> Option<std::path::PathBuf> {
        let root = match self {
            AccountsPersist::Real => accounts::config_root(),
            AccountsPersist::Sandbox(dir) => Some(dir.path().to_path_buf()),
            AccountsPersist::Disabled => None,
        }?;
        Some(root.join("mail-local.json"))
    }
}

/// Device-local Mail preferences. Kept deliberately small and separate from
/// the shared account payload: a peer must never adopt another window's
/// composer mode just because it received mailbox changes.
#[derive(serde::Deserialize, serde::Serialize)]
struct LocalPreferences {
    compose_format: String,
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
    reason = "the bools split across three unrelated concerns (shell layout flags, the setup-sheet's own transient UI state, and engine-request bookkeeping); grouping them would need a real design pass (e.g. a SetupSheetState sub-struct), left for a follow-up rather than forced here"
)]
pub struct MailApp {
    shared: Shared,
    /// Where draft attachment bytes live; drafts carry only their content id.
    /// The engine worker holds a clone to build outgoing MIME.
    content: ContentStore,
    /// One retained catalogue per mailbox revision. Palette providers clone
    /// this Arc-backed handle; typing never rebuilds the loaded-mail index.
    mail_search_provider: crate::search_provider::MailSearchProvider,
    nav: NavShell,
    nav_node: Option<NodeId>,
    /// The title the list's header shows.
    overview_title: String,
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
    /// The row menu, and the message it acts on while open.
    menu: ContextMenu,
    menu_target: Option<MenuTarget>,
    /// A row menu to open once the list has been laid out (a capture).
    pending_menu: Option<MessageId>,
    /// An explicit destructive confirmation for the selection toolbar. This
    /// is local presentation state; the actual purge remains one canonical
    /// `MailAction` only after the person confirms.
    bulk_purge_confirmation: Option<PurgeConfirmation>,
    /// Opens the platform search from the app's own input handling.
    search: SearchOpener,

    // account-setup sheet (Settings owns this in production; offered in-app too)
    setup_open: bool,
    setup_inputs: Vec<TextInput>,
    /// Sizes the account sheet is laid out with, measured on paint with the
    /// styles the sheet paints with (the authored 1× rules until then), so
    /// a larger text scale grows labels, fields and buttons instead of
    /// painting them over each other.
    setup_metrics: SetupMetrics,
    /// `(renderer scale, theme scale, catalogue revision, OAuth label)` the
    /// metrics were measured for; unchanged means the measurement is skipped.
    setup_metrics_key: Option<(u32, u32, u64, String)>,
    /// Scroll offset of the sheet's body when the window is too short for
    /// every row, and the largest offset that still shows the last row.
    setup_scroll: f64,
    setup_scroll_max: f64,
    /// The sheet as last laid out: the scrim, the card, its body band and
    /// its footer buttons.
    setup_layout: Option<ui::widget::ModalLayout>,
    setup_imap_tls: bool,
    setup_smtp_tls: bool,
    setup_editing: Option<usize>,
    setup_focus: Option<usize>,
    /// The account's chosen sign-in method in the setup sheet.
    setup_auth: crate::model::AuthMethod,
    /// The OAuth provider detected from the typed email domain, if any.
    setup_provider: Option<crate::oauth::OAuthProvider>,
    /// The most recent validation message from a save attempt (shown inline in
    /// the sheet); cleared when the sheet is (re)opened.
    setup_error: Option<String>,
    setup_validation: Option<PendingSetupValidation>,
    setup_discovery: Option<PendingSetupDiscovery>,

    engine: Option<EngineHandle>,
    pending_send: Option<PendingSend>,
    /// Where account changes persist. The live app targets the real user config
    /// root; the sample / shots / test constructors target a throwaway sandbox
    /// so they can never write into the real account store.
    accounts_persist: AccountsPersist,
    /// The device-local account id last flushed to [`AccountsPersist`]. Pane,
    /// accessibility, and resource operations all select through `MailState`,
    /// so observing the id here gives every selection path one persistence
    /// boundary without leaking that preference into Automerge.
    persisted_active_account: Option<String>,
    /// Last format persisted to this device-local preference store. This lets
    /// format toggles save immediately without rewriting disk at every close.
    persisted_compose_format: ComposeFormat,
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
    /// One host-native attachment Save chooser at a time. The receiver is
    /// polled from `sync`, never awaited on the UI thread.
    attachment_save_rx: Option<PortalChannel>,
    /// One portal-open attachment staging request. The returned descriptor is
    /// copied straight into the content store; its original path is never retained.
    compose_attachment_rx: Option<OpenFileChannel>,
    pending_attachment_save: Option<PendingAttachmentSave>,
    /// A typed resource operation must not claim success before the chooser
    /// returns and the bytes reach the chosen destination.
    resource_attachment_save_invocation: Option<tilo_actions::InvocationId>,
    resource_attachment_save_result: Option<tilo_actions::InvocationUpdate>,
    /// Wall-clock minute of the last automatic inbox refresh, so periodic sync
    /// fires at most once per [`AUTO_REFRESH_MIN`] while the app is in use.
    last_auto_refresh_min: i64,

    /// The status text currently being shown as a toast, and the instant it
    /// should auto-dismiss. Any code path may set `shared.status`; the app
    /// notices the change each frame, (re)arms the deadline, and clears the
    /// status once [`TOAST_TTL`] elapses — so a "Synced N messages" toast fades
    /// on its own instead of lingering forever.
    toast_text: Option<String>,
    toast_deadline: Option<ui::time::Instant>,

    overlay_hits: Vec<(Rect, OverlayHit)>,
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
}

impl MailApp {
    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Mail>,
    ) {
        self.shared.borrow_mut().actions.attach(dispatcher.clone());
        self.compose.attach_framework(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.shared.borrow().actions.is_hosted()
    }

    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&self) {
        self.shared.borrow_mut().actions.mark_hosted_for_test();
    }

    pub(crate) fn has_pending_send(&self) -> bool {
        self.pending_send.is_some()
    }

    #[cfg(test)]
    pub(crate) fn mailbox_message_count(&self) -> usize {
        self.shared.borrow().mailbox.messages.len()
    }

    /// The hermetic constructor used by `--shots` and tests: the bundled
    /// sample corpus, no engine, deterministic "now".
    pub fn new_sample() -> Self {
        let shared = MailState::shared(MailState::new_sample(avatar_store_offline()));
        Self::assemble(
            shared,
            ContentStore::memory(),
            None,
            AccountsPersist::sandbox(),
        )
    }

    /// The live constructor: a real account list loaded from disk and the
    /// engine worker spawned. No fabricated messages.
    pub fn new_live() -> Self {
        let accounts = load_account_store();
        let shared = MailState::shared(MailState::new_live(accounts, avatar_store()));
        let content = ContentStore::for_app();
        let (tx, rx) = crate::engine::spawn(content.clone());
        // Only the live app writes the user's real account store.
        Self::assemble(
            shared,
            content,
            Some(EngineHandle { tx, rx }),
            AccountsPersist::Real,
        )
    }

    /// A first-run constructor with no configured account and no engine —
    /// renders the onboarding / welcome screen. Used by the `firstrun` shot.
    /// Persists to a throwaway sandbox (never the real store) so a driven
    /// add-account in this mode stays hermetic.
    pub fn new_first_run() -> Self {
        let shared = MailState::shared(MailState::new_live(
            AccountStore::new(),
            avatar_store_offline(),
        ));
        Self::assemble(
            shared,
            ContentStore::memory(),
            None,
            AccountsPersist::sandbox(),
        )
    }

    fn assemble(
        shared: Shared,
        content: ContentStore,
        engine: Option<EngineHandle>,
        accounts_persist: AccountsPersist,
    ) -> Self {
        let mail_search_provider = {
            let state = shared.borrow();
            crate::search_provider::MailSearchProvider::new(&state.mailbox)
        };
        let overview_title = shared.borrow().folder.title().to_owned();
        let nav = Self::build_shell(&shared, &overview_title);

        let (cfg, persisted_active_account) = {
            let state = shared.borrow();
            (
                state.accounts.active_config(),
                state
                    .accounts
                    .active_account()
                    .map(|account| account.id.clone()),
            )
        };
        let compose_format = accounts_persist.load_compose_format();
        Self {
            shared,
            content,
            mail_search_provider,
            nav,
            nav_node: None,
            overview_title,
            first_run: EmptyState::new("Welcome to Mail")
                .with_icon(Icon::Mail)
                .with_message("Add your email account to start reading and sending mail.")
                .with_action("Add account")
                .with_action_id("mail.add-account"),
            first_run_node: None,
            compose: ComposeWindow::new(compose_format),
            shortcuts: MailShortcutsOverlay::new(),
            editing_draft: None,
            menu: ContextMenu::new(),
            menu_target: None,
            pending_menu: None,
            bulk_purge_confirmation: None,
            search: SearchOpener::new(),
            setup_open: false,
            setup_inputs: build_setup_inputs(&cfg),
            setup_metrics: SetupMetrics::default(),
            setup_metrics_key: None,
            setup_scroll: 0.0,
            setup_scroll_max: 0.0,
            setup_layout: None,
            setup_imap_tls: cfg.imap_tls,
            setup_smtp_tls: cfg.smtp_tls,
            setup_editing: None,
            setup_focus: None,
            setup_auth: cfg.auth,
            setup_provider: None,
            setup_error: None,
            setup_validation: None,
            setup_discovery: None,
            engine,
            pending_send: None,
            accounts_persist,
            persisted_active_account,
            persisted_compose_format: compose_format,
            connect_requested: false,
            awaiting_engine: false,
            background_poll_deadline: Cell::new(None),
            pending_engine_requests: HashMap::new(),
            resource_refresh_invocation: None,
            resource_refresh_result: None,
            attachment_save_rx: None,
            compose_attachment_rx: None,
            pending_attachment_save: None,
            resource_attachment_save_invocation: None,
            resource_attachment_save_result: None,
            last_auto_refresh_min: 0,
            toast_text: None,
            toast_deadline: None,
            overlay_hits: Vec::new(),
            rect: Rect::ZERO,
            layout_theme: Theme::dark(),
            first_layout: true,
            dirty: true,
            #[cfg(test)]
            test_layout: None,
        }
    }

    // ---- `--shots` priming ------------------------------------------------

    /// Open the composer with a representative HTML draft (compose shot).
    pub fn prime_compose_shot(&mut self) {
        self.compose.open_demo();
        self.dirty = true;
    }

    /// Push the message in and snap it (narrow "reading a message" shot).
    pub fn prime_detail_shot(&mut self) {
        self.nav.show(NavPane::Content);
        self.nav.snap();
        self.dirty = true;
    }

    /// Open an unread sample message through the same explicit read-state
    /// neutral path that agents and Ctrl+K use, so visual review covers the
    /// persistent preview indicator rather than relying only on a unit test.
    pub fn prime_preview_shot(&mut self) {
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
        self.dirty = true;
    }

    /// Show the local multi-message triage state with a small, deliberate
    /// selection. This exercises the extra header row, selected-row treatment,
    /// and bulk controls without putting synthetic mail on the live path.
    pub fn prime_bulk_shot(&mut self) {
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
        drop(state);
        self.dirty = true;
    }

    /// Render the destructive confirmation after an all-Trash selection. This
    /// is a visual contract test: the review surface names the affected count
    /// and the irreversibility before the canonical purge action is authored.
    pub fn prime_bulk_purge_confirmation_shot(&mut self) {
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
        state.set_bulk_selecting(true);
        for id in ids {
            state.mailbox.move_to(id, Folder::Trash);
            state.toggle_bulk_selection(id);
        }
        state.push_intent(Intent::ConfirmBulkPurge);
        drop(state);
        self.process_intents();
        self.dirty = true;
    }

    /// Review a real reply draft rather than a generic blank composition.
    pub fn prime_reply_shot(&mut self) {
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

    /// Open the same menu a row right-click produces, on the selected row,
    /// once the list has been laid out.
    pub fn prime_context_menu_shot(&mut self) {
        self.pending_menu = self.shared.borrow().selected;
        self.dirty = true;
    }

    /// Open the account-setup sheet (setup shot).
    pub fn prime_setup_shot(&mut self) {
        let active = self.shared.borrow().accounts.active;
        self.open_setup_edit(Some(active));
    }

    /// Open a blank "Add account" sheet with an email typed + servers
    /// autofilled, so the shot shows the add flow with single-border fields.
    pub fn prime_setup_add_shot(&mut self) {
        self.open_setup_edit(None);
        self.set_setup_input(0, "Alex Morgan");
        self.set_setup_input(1, "alex@fastmail.com");
        self.maybe_autofill();
        // Land focus on a server field so the single-border focus ring shows.
        self.focus_setup(Some(4));
    }

    /// Open a blank "Add account" sheet with a Gmail address — which presets to
    /// OAuth — so the shot shows the OAuth sign-in flow.
    pub fn prime_setup_oauth_shot(&mut self) {
        self.open_setup_edit(None);
        self.set_setup_input(0, "Alex Morgan");
        self.set_setup_input(1, "alex.morgan@gmail.com");
        self.maybe_autofill();
        self.focus_setup(Some(1));
    }

    /// Select the bundled HTML newsletter so the reader renders it through the
    /// sandboxed `WebView` (HTML-mail shot). Also demonstrates the filled-star
    /// action (the newsletter is starred).
    pub fn prime_html_mail_shot(&mut self) {
        let id = {
            let st = self.shared.borrow();
            st.mailbox
                .messages
                .values()
                .find(|m| m.from.email == "hello@fieldnotes.example")
                .map(|m| m.id)
        };
        if let Some(id) = id {
            let mut doc = AutoCommit::new();
            self.shared.borrow_mut().select_message(&mut doc, id);
            self.process_intents();
        }
        self.dirty = true;
    }

    /// A configured mailbox before any messages have arrived.
    pub fn prime_empty_shot(&mut self) {
        let mut st = self.shared.borrow_mut();
        let account = st.mailbox.account.clone();
        st.mailbox = crate::model::Mailbox::new(account);
        st.selected = None;
        st.folder = Folder::Inbox;
        st.dirty = true;
        drop(st);
        self.dirty = true;
    }

    /// A real in-flight sync indication over cached mail.
    pub fn prime_loading_shot(&mut self) {
        self.awaiting_engine = true;
        self.shared.borrow_mut().status = Some("Syncing Inbox…".into());
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
            self.shared.borrow_mut().status = Some("Mail worker is unavailable".into());
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

    /// Cached mail remains usable while connectivity is unavailable.
    pub fn prime_offline_shot(&mut self) {
        self.shared.borrow_mut().status = Some("You’re offline · Showing downloaded mail".into());
        self.dirty = true;
    }

    /// A recoverable account error: the configuration stays open and editable.
    pub fn prime_error_shot(&mut self) {
        self.prime_setup_add_shot();
        self.setup_error = Some(
            "Couldn’t connect to IMAP. Check the server, port, and TLS settings, then try again."
                .into(),
        );
        self.dirty = true;
    }

    /// Open the sidebar drawer and snap it (narrow drawer shot).
    pub fn prime_drawer_shot(&mut self) {
        self.nav.open_sidebar();
        self.nav.snap();
        self.dirty = true;
    }

    // ---- window shell -----------------------------------------------------

    /// The window shell: the sidebar of accounts and folders, the message
    /// list and the open message, with each column's header actions.
    fn build_shell(shared: &Shared, overview_title: &str) -> NavShell {
        let add_account = {
            let shared = shared.clone();
            Button::icon_only("mail-add-account", ui::icons::Icon::Plus, "Add account").on_click(
                move || {
                    shared
                        .borrow_mut()
                        .push_command(Command::AccountSettings(None));
                },
            )
        };
        NavShell::new()
            .with_id("mail-shell")
            .sidebar(Box::new(MailSidebar::new(shared.clone())))
            .overview(Box::new(MessageList::new(shared.clone())))
            .content(Box::new(MessageReader::new(shared.clone())))
            .sidebar_header("", vec![Box::new(add_account)])
            .overview_header(
                overview_title,
                vec![Box::new(CommandBar::list(shared.clone()))],
            )
            .content_header("", vec![Box::new(CommandBar::reader(shared.clone()))])
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

    /// Durably replace the portal-authorized target with attachment bytes.
    /// The portal owns the parent/destination choice; Mail only accepts a
    /// normal file (or a new leaf), never follows a selected symlink or
    /// manufactures a missing parent. Atomic replacement ensures an existing
    /// chosen file remains intact if the write fails partway through.
    fn write_portal_attachment(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > MAX_ATTACHMENT_SAVE_BYTES {
            return Err(format!(
                "attachment exceeds the {MAX_ATTACHMENT_SAVE_BYTES}-byte save limit"
            ));
        }
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("refusing to overwrite a symbolic-link destination".into());
            }
            Ok(metadata) if !metadata.is_file() => {
                return Err("selected destination is not a regular file".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("couldn’t inspect selected destination: {error}"));
            }
        }
        let Some(parent) = path.parent() else {
            return Err("selected destination has no parent folder".into());
        };
        let parent_metadata = std::fs::symlink_metadata(parent)
            .map_err(|error| format!("couldn’t inspect selected folder: {error}"))?;
        if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
            return Err("selected destination is outside a regular folder".into());
        }
        tilo_files::native::atomic::write(path, bytes).map_err(|error| error.to_string())
    }

    fn attachment_save_busy(&self) -> bool {
        self.attachment_save_rx.is_some()
            || self.pending_attachment_save.is_some()
            || self.resource_attachment_save_invocation.is_some()
            || self.resource_attachment_save_result.is_some()
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
    fn begin_attachment_save(&mut self, message_id: MessageId, index: usize) -> Result<(), String> {
        if self.attachment_save_busy() {
            return Err("another attachment save is already in progress".into());
        }
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
        let suggested_name = Self::attachment_save_name(&attachment.filename);
        self.pending_attachment_save = Some(PendingAttachmentSave {
            message_id,
            index,
            attachment,
        });
        self.attachment_save_rx = Some(
            PickRequest::save("Save attachment", "Save", &suggested_name)
                .thread_name("mail-portal-save-attachment")
                .filter(FileFilter::all_files())
                .send(),
        );
        self.shared.borrow_mut().status = Some("Choose where to save the attachment".into());
        self.dirty = true;
        Ok(())
    }

    fn finish_attachment_save(
        &mut self,
        outcome: Result<String, Option<String>>,
        saved_resource: Option<String>,
    ) {
        match &outcome {
            Ok(message) | Err(Some(message)) => {
                self.shared.borrow_mut().status = Some(message.clone());
            }
            Err(None) => self.shared.borrow_mut().status = Some("Attachment save cancelled".into()),
        }
        if let Some(invocation) = self.resource_attachment_save_invocation.take() {
            self.resource_attachment_save_result = Some(match outcome {
                Ok(_) => tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput {
                        value: None,
                        resources: saved_resource.into_iter().collect(),
                    },
                },
                Err(Some(message)) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "mail.attachment_save_failed".into(),
                        message,
                        details: None,
                        retryable: false,
                    },
                },
                Err(None) => tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: Some("attachment save cancelled".into()),
                },
            });
        }
        self.dirty = true;
    }

    /// Drain exactly one portal outcome without blocking. A typed resource
    /// effect reaches a terminal result only here, after a successful write;
    /// this prevents history and agents from reporting a saved file merely
    /// because a chooser was opened.
    fn drain_attachment_save(&mut self) {
        let outcome = self
            .attachment_save_rx
            .as_ref()
            .map(std::sync::mpsc::Receiver::try_recv);
        let Some(outcome) = outcome else {
            return;
        };
        match outcome {
            Ok(PortalOutcome::Opened(path)) => {
                self.attachment_save_rx = None;
                let Some(pending) = self.pending_attachment_save.take() else {
                    return;
                };
                let result = Self::write_portal_attachment(&path, &pending.attachment.content)
                    .map_or_else(
                        |error| {
                            Err(Some(format!(
                                "Couldn’t save {}: {error}",
                                pending.attachment.filename
                            )))
                        },
                        |()| {
                            Ok(format!(
                                "Saved {} to {}",
                                pending.attachment.filename,
                                path.display()
                            ))
                        },
                    );
                self.finish_attachment_save(
                    result,
                    Some(format!(
                        "mail.attachment:{}:{}",
                        pending.message_id, pending.index
                    )),
                );
            }
            Ok(PortalOutcome::Cancelled) => {
                self.attachment_save_rx = None;
                self.pending_attachment_save = None;
                self.finish_attachment_save(Err(None), None);
            }
            Ok(PortalOutcome::Failed(reason)) => {
                self.attachment_save_rx = None;
                self.pending_attachment_save = None;
                self.finish_attachment_save(
                    Err(Some(format!("Attachment save unavailable: {reason}"))),
                    None,
                );
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.attachment_save_rx = None;
                self.pending_attachment_save = None;
                self.finish_attachment_save(
                    Err(Some("Attachment save picker stopped unexpectedly".into())),
                    None,
                );
            }
        }
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
                Intent::Settings(idx) => self.open_setup_edit(idx),
                Intent::SaveAttachment(message_id, index) => {
                    if let Err(error) = self.begin_attachment_save(message_id, index) {
                        self.shared.borrow_mut().status = Some(error);
                    }
                }
                Intent::Engine(req) => {
                    self.post_engine(req);
                }
                Intent::Toast(msg) => self.shared.borrow_mut().status = Some(msg),
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
    fn apply_commands(&mut self, doc: &mut AutoCommit) {
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
            menu.push(entry.with_icon(item.icon()));
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
    fn take_menu_choice(&mut self, doc: &mut AutoCommit) {
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

    fn apply_menu_action(&mut self, message: MessageId, action: CtxAction, doc: &mut AutoCommit) {
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
        let top = column.y0 + geometry.header_height + index as f64 * row_h;
        let row = Rect::new(column.x0, top, column.x1, top + row_h);
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
    fn settle_purge_confirmation(&mut self, doc: &mut AutoCommit) {
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
        let draft = match self.shared.borrow().mailbox.get(id) {
            Some(m) => draft_from_message(m),
            None => return,
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
        self.compose.open_reply("", &subj, &quote);
        self.close_menu();
        self.dirty = true;
    }

    fn send_compose(&mut self, doc: &mut AutoCommit) {
        self.persist_compose_format_if_changed();
        if self.pending_send.is_some() {
            self.shared.borrow_mut().status = Some("Message is still sending…".into());
            return;
        }
        let draft = self.compose.to_draft(doc);
        let acct = self.shared.borrow().accounts.active_config();
        if acct.email.trim().is_empty() {
            self.shared.borrow_mut().status =
                Some("Add a sending account before sending this message".into());
            self.dirty = true;
            return;
        }
        if self.engine.is_none() {
            self.shared.borrow_mut().status =
                Some("Sending is unavailable in this offline preview; save a draft instead".into());
            self.dirty = true;
            return;
        }
        let from = Addr::new(acct.display_name.clone(), acct.email.clone());
        match crate::transport::build_mime(&self.content, &from, &draft) {
            Ok(_) => {
                let to = crate::transport::parse_addr_list(&draft.to);
                let now = self.shared.borrow().now_min;
                let msg = Message {
                    id: uuid::Uuid::new_v4(),
                    rfc_message_id: None,
                    in_reply_to: None,
                    from,
                    to,
                    cc: crate::transport::parse_addr_list(&draft.cc),
                    subject: draft.subject.clone(),
                    body: draft.body.clone(),
                    html_body: draft.html_body.clone(),
                    attachments: draft.attachments.iter().map(attachment_meta).collect(),
                    timestamp_min: now,
                    read: true,
                    starred: false,
                    folder: Folder::Sent,
                    labels: Vec::new(),
                };
                let Some(request_id) = self.post_engine(EngineRequest::send(acct, Box::new(draft)))
                else {
                    return;
                };
                self.pending_send = Some(PendingSend {
                    request_id,
                    message: msg,
                    edited_draft: self.editing_draft,
                });
                self.shared.borrow_mut().status = Some("Sending…".into());
            }
            Err(e) => self.shared.borrow_mut().status = Some(format!("Send failed: {e}")),
        }
        self.dirty = true;
    }

    fn save_compose_draft(&mut self, doc: &mut AutoCommit) {
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
        self.persist_compose_format_if_changed();
        self.dirty = true;
    }

    /// Insert (or, when editing an existing draft, update in place) a Drafts
    /// folder message reflecting `draft`, and return its id.
    fn upsert_draft_message(&self, doc: &mut AutoCommit, draft: &Draft) -> MessageId {
        let (from, now) = {
            let st = self.shared.borrow();
            let acct = st.accounts.active_config();
            (Addr::new(acct.display_name.clone(), acct.email), st.now_min)
        };
        let id = self.editing_draft.unwrap_or_else(uuid::Uuid::new_v4);
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
        self.setup_inputs = build_setup_inputs(&cfg);
        self.setup_imap_tls = cfg.imap_tls;
        self.setup_smtp_tls = cfg.smtp_tls;
        self.setup_auth = cfg.auth;
        self.setup_provider = cfg
            .email
            .split_once('@')
            .and_then(|(_, d)| crate::oauth::OAuthProvider::from_domain(d));
        self.setup_error = None;
        self.setup_open = true;
        self.focus_setup(Some(1));
        self.close_menu();
        self.dirty = true;
    }

    fn close_setup(&mut self) {
        self.setup_validation = None;
        self.setup_discovery = None;
        self.setup_open = false;
        self.setup_error = None;
        self.focus_setup(None);
        self.dirty = true;
    }

    /// Whether the app is at first run (no account configured) and should show
    /// the onboarding screen rather than an empty mailbox.
    fn is_first_run(&self) -> bool {
        self.shared.borrow().accounts.is_empty() && !self.setup_open
    }

    /// The ordered field indices the Tab key walks. The password field is
    /// dropped from the order when the account uses OAuth (there is no
    /// password to type).
    fn setup_tab_order(&self) -> Vec<usize> {
        let mut order = vec![0usize, 1, 2];
        if self.setup_auth == crate::model::AuthMethod::Password {
            order.push(3);
        }
        order.extend([4, 5, 6, 7]);
        order
    }

    /// Advance focus to the next (`+1`) / previous (`-1`) setup field, wrapping.
    fn focus_setup_step(&mut self, delta: i32) {
        let order = self.setup_tab_order();
        let cur = self
            .setup_focus
            .and_then(|f| order.iter().position(|&i| i == f))
            .unwrap_or(0) as i32;
        let n = order.len() as i32;
        let next = (((cur + delta) % n + n) % n) as usize;
        let Some(&next_field) = order.get(next) else {
            // `next` is always in `0..order.len()` by construction; bail
            // rather than panic if that invariant is ever violated.
            return;
        };
        // Leaving the email field triggers autoconfig, same as a click-away.
        let leaving_email = self.setup_focus == Some(1) && next_field != 1;
        self.focus_setup(Some(next_field));
        if leaving_email {
            self.maybe_autofill();
        }
    }

    fn focus_setup(&mut self, which: Option<usize>) {
        self.setup_focus = which;
        for (i, inp) in self.setup_inputs.iter_mut().enumerate() {
            inp.set_focus(which == Some(i));
        }
        // Keyboard focus reaches every row: when the row band scrolls, bring
        // the focused field into it (its rect is where the last layout put it,
        // offset by the current scroll).
        if self.setup_scroll_max > 0.0
            && let Some(rect) = which
                .and_then(|i| self.setup_input(i))
                .map(TextInput::get_rect)
            && let Some(band) = self.setup_layout.as_ref().map(|layout| layout.body)
        {
            let (band_top, band_bottom) = (band.y0, band.y1);
            if rect.y0 < band_top {
                self.setup_scroll -= band_top - rect.y0;
            } else if rect.y1 > band_bottom {
                self.setup_scroll += rect.y1 - band_bottom;
            }
            self.setup_scroll = self.setup_scroll.clamp(0.0, self.setup_scroll_max);
            self.rebuild_overlay_hits();
        }
        self.dirty = true;
    }

    /// One of the fixed setup-sheet fields, addressed by its position in
    /// [`build_setup_inputs`] (0 = display name, 1 = email, 2 = username,
    /// 3 = password, 4 = IMAP host, 5 = IMAP port, 6 = SMTP host, 7 = SMTP
    /// port). `None` only if the input list is ever shorter than that fixed
    /// layout; every caller treats it as a no-op read/write rather than a
    /// panic.
    fn setup_input(&self, i: usize) -> Option<&TextInput> {
        self.setup_inputs.get(i)
    }

    fn setup_input_mut(&mut self, i: usize) -> Option<&mut TextInput> {
        self.setup_inputs.get_mut(i)
    }

    /// Set field `i`'s text, a silent no-op if the index is out of range.
    fn set_setup_input(&mut self, i: usize, text: &str) {
        if let Some(inp) = self.setup_input_mut(i) {
            inp.set_text(text);
        }
    }

    /// The OAuth sign-in button's label for the selected provider. One source
    /// for the button painter and the measurement that sizes the button.
    fn oauth_button_label(&self) -> String {
        let prov = self.setup_provider.map_or_else(
            || crate::i18n::t("mail-setup-oauth-provider-fallback"),
            |p| super::oauth::OAuthProvider::label(p).to_string(),
        );
        crate::i18n::t_args(
            "mail-setup-oauth-sign-in",
            &[("provider", ui::i18n::ArgValue::from(prov))],
        )
    }

    /// Record a setup-sheet hit for `rect` only when the whole rect lies in
    /// the scrolling row `band`; a row scrolled past the band's edge is not
    /// clickable (and is not painted), so it can never overlap the footer.
    fn push_setup_hit(hits: &mut Vec<(Rect, OverlayHit)>, band: Rect, rect: Rect, hit: OverlayHit) {
        if rect.y0 >= band.y0 - 0.5 && rect.y1 <= band.y1 + 0.5 {
            hits.push((rect, hit));
        }
    }

    /// Lay field `i` out into `rect`, a silent no-op if the index is out of
    /// range.
    fn layout_setup_input(&mut self, i: usize, rect: Rect) {
        if let Some(inp) = self.setup_input_mut(i) {
            inp.apply_layout_external(rect);
        }
    }

    /// Lay out the incoming and outgoing server rows (host, port, TLS
    /// switch) of the account sheet from `y`, and register their hit
    /// regions. Part of [`Self::rebuild_overlay_hits`]. Returns the y past
    /// them.
    fn layout_server_rows(&mut self, fx: f64, rx: f64, mut y: f64, band: Rect) -> f64 {
        let m = self.setup_metrics.clone();
        let host_w = (rx - fx - m.gap - m.port_w - m.gap - m.tls_w).max(0.0);
        for (host, port, toggle) in [
            (4, 5, OverlayHit::ToggleImapTls),
            (6, 7, OverlayHit::ToggleSmtpTls),
        ] {
            let host_rect = Rect::new(fx, y, fx + host_w, y + m.field_h);
            self.layout_setup_input(host, host_rect);
            Self::push_setup_hit(
                &mut self.overlay_hits,
                band,
                host_rect,
                OverlayHit::SetupField(host),
            );
            let port_rect = Rect::new(
                host_rect.x1 + m.gap,
                y,
                host_rect.x1 + m.gap + m.port_w,
                y + m.field_h,
            );
            self.layout_setup_input(port, port_rect);
            Self::push_setup_hit(
                &mut self.overlay_hits,
                band,
                port_rect,
                OverlayHit::SetupField(port),
            );
            let tls = Rect::new(port_rect.x1 + m.gap, y, rx, y + m.field_h);
            Self::push_setup_hit(&mut self.overlay_hits, band, tls, toggle);
            y += m.field_h + m.row_gap;
        }
        y - m.row_gap
    }

    fn read_setup_config(&self) -> AccountConfig {
        let g = |i: usize| {
            self.setup_input(i)
                .map(|inp| inp.value().to_string())
                .unwrap_or_default()
        };
        AccountConfig {
            display_name: g(0),
            email: g(1),
            username: g(2),
            // An OAuth account authenticates with a vault-sealed token, never a
            // password — keep the field empty so persist doesn't overwrite the
            // sealed token with a password secret.
            password: if self.setup_auth == crate::model::AuthMethod::OAuth {
                String::new()
            } else {
                g(3)
            },
            imap_host: g(4),
            imap_port: g(5).trim().parse().unwrap_or(993),
            smtp_host: g(6),
            smtp_port: g(7).trim().parse().unwrap_or(465),
            imap_tls: self.setup_imap_tls,
            smtp_tls: self.setup_smtp_tls,
            auth: self.setup_auth,
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
        let email = self
            .setup_input(1)
            .map(|inp| inp.value().trim().to_string())
            .unwrap_or_default();
        if self
            .setup_discovery
            .as_ref()
            .is_some_and(|pending| pending.config.email.trim() != email)
        {
            self.setup_discovery = None;
        }
        if !email.contains('@') {
            return;
        }
        let domain = email
            .split_once('@')
            .map(|(_, d)| d.to_string())
            .unwrap_or_default();

        // Provider detection → OAuth for the providers that block passwords.
        self.setup_provider = crate::oauth::OAuthProvider::from_domain(&domain);
        if self.setup_provider.is_some() {
            self.setup_auth = crate::model::AuthMethod::OAuth;
        }

        let imap_blank = self
            .setup_input(4)
            .is_none_or(|inp| inp.value().trim().is_empty());
        let smtp_blank = self
            .setup_input(6)
            .is_none_or(|inp| inp.value().trim().is_empty());
        if !imap_blank && !smtp_blank {
            self.setup_discovery = None;
            self.dirty = true;
            return;
        }

        let mut cfg = AccountConfig {
            email: email.clone(),
            username: self
                .setup_input(2)
                .map(|inp| inp.value().to_string())
                .unwrap_or_default(),
            display_name: self
                .setup_input(0)
                .map(|inp| inp.value().to_string())
                .unwrap_or_default(),
            auth: self.setup_auth,
            ..Default::default()
        };
        // Built-in table first (instant, offline). Network discovery only for
        // domains it doesn't cover, and only on the focus-leave/save pass.
        if let Some(ac) = crate::autoconfig::builtin_autoconfig(&domain) {
            ac.apply_to(&mut cfg);
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
            self.setup_imap_tls = cfg.imap_tls;
        }
        if smtp_blank {
            self.set_setup_input(6, &cfg.smtp_host);
            self.set_setup_input(7, &cfg.smtp_port.to_string());
            self.setup_smtp_tls = cfg.smtp_tls;
        }
        if self
            .setup_input(2)
            .is_some_and(|inp| inp.value().trim().is_empty())
        {
            self.set_setup_input(2, &cfg.username);
        }
        if self
            .setup_input(0)
            .is_some_and(|inp| inp.value().trim().is_empty())
        {
            self.set_setup_input(0, &cfg.display_name);
        }
        // Autoconfig may have upgraded the auth method (OAuth2 in the XML).
        self.setup_auth = cfg.auth;
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
            self.setup_error = Some(format!("Couldn’t start provider discovery: {error}"));
            self.dirty = true;
            return;
        }
        self.setup_discovery = Some(PendingSetupDiscovery {
            config,
            editing: self.setup_editing,
            receiver,
            cancellation,
        });
        self.dirty = true;
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
                self.setup_error = Some(
                    "Provider discovery ended without a result; enter server details manually"
                        .into(),
                );
                self.dirty = true;
                return;
            }
        };
        let Some(pending) = self.setup_discovery.take() else {
            return;
        };
        self.dirty = true;
        let current = self.read_setup_config();
        if !self.setup_open
            || self.setup_editing != pending.editing
            || current.email != pending.config.email
        {
            return;
        }
        let mut discovered = pending.config.clone();
        if let Some(result) = result {
            result.apply_to(&mut discovered);
        } else {
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
            self.setup_imap_tls = discovered.imap_tls;
        }
        if current.smtp_host.trim().is_empty()
            && current.smtp_port == pending.config.smtp_port
            && current.smtp_tls == pending.config.smtp_tls
        {
            self.set_setup_input(6, &discovered.smtp_host);
            self.set_setup_input(7, &discovered.smtp_port.to_string());
            self.setup_smtp_tls = discovered.smtp_tls;
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
            self.setup_auth = discovered.auth;
        }
        self.dirty = true;
    }

    fn save_setup(&mut self, _doc: &mut AutoCommit) {
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
            self.setup_error = Some(format!("Couldn’t start account verification: {error}"));
            self.dirty = true;
            return;
        }
        self.setup_validation = Some(PendingSetupValidation {
            config,
            editing: self.setup_editing,
            receiver,
            cancellation,
        });
        self.setup_error = None;
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
        if !self.setup_open {
            return;
        }
        if self.setup_editing != pending.editing || self.read_setup_config() != pending.config {
            self.setup_error =
                Some("Account settings changed; verify them again before saving".into());
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
        _doc: &mut AutoCommit,
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
            let msg = outcome.message();
            self.setup_error = Some(msg.clone());
            self.shared.borrow_mut().status = Some(msg);
            self.dirty = true;
            return;
        }
        self.setup_error = None;
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
            // Persist to the shared on-disk store + vault so the account (and
            // its password) survive a restart. Routed through `accounts_persist`
            // so sample / shots / test builds never touch the real store.
            if let Err(error) = self.accounts_persist.persist(&updated) {
                let message = format!("Couldn’t save account: {error}");
                self.setup_error = Some(message.clone());
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

    /// Remove the account currently being edited, persist the removal to disk
    /// (dropping it from the shared store + purging its vault secret), and close
    /// the sheet. A no-op on the add flow (no editing target).
    fn remove_editing_account(&mut self, _doc: &mut AutoCommit) {
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
            if let Err(error) = self.accounts_persist.persist(&updated) {
                let message = format!("Couldn’t remove account: {error}");
                self.setup_error = Some(message.clone());
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
        let email = self
            .setup_input(1)
            .map(|inp| inp.value().trim().to_string())
            .unwrap_or_default();
        let Some(provider) = self.setup_provider else {
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
            self.setup_error = Some(format!(
                "{} sign-in isn’t available here yet. Choose Password to continue with an app password.",
                provider.label()
            ));
            self.dirty = true;
            return;
        }
        match self.engine.as_ref() {
            Some(_) => {
                self.post_engine(EngineRequest::oauth_authorize(
                    email.clone(),
                    provider,
                    email,
                ));
                self.setup_error = None;
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

    /// The account sheet's card as last laid out.
    fn overlay_panel_rect(&self) -> Rect {
        self.setup_layout
            .as_ref()
            .map_or(Rect::ZERO, |layout| layout.card)
    }

    /// The account sheet's title.
    fn setup_heading(&self) -> String {
        if self.setup_editing.is_some() {
            crate::i18n::t("mail-setup-heading-edit")
        } else {
            crate::i18n::t("mail-setup-heading-add")
        }
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

    /// Lay the account sheet out in the window with `text` in `theme`:
    /// measure its rows, frame them in the shared modal sheet, and record
    /// where every field, switch and button landed.
    fn layout_setup(&mut self, text: &mut TextRenderer, theme: &Theme) {
        let oauth_label = self.oauth_button_label();
        let key = (
            text.text_scale().to_bits(),
            theme.text_scale.to_bits(),
            ui::i18n::I18nManager::revision(),
            oauth_label.clone(),
        );
        if self.setup_metrics_key.as_ref() != Some(&key) {
            self.setup_metrics_key = Some(key);
            self.setup_metrics = SetupMetrics::measure(text, theme, &oauth_label);
        }
        let heading = self.setup_heading();
        let save = self.setup_save_label();
        let cancel = crate::i18n::t("mail-setup-cancel");
        let actions = [
            ui::widget::ModalAction::secondary(&cancel),
            ui::widget::ModalAction::primary(&save),
        ];
        let rows = self.setup_metrics.rows_h(
            self.setup_auth == crate::model::AuthMethod::Password,
            self.setup_editing.is_some(),
        );
        let spec = ui::widget::ModalSpec::new(&heading)
            .actions(&actions)
            .body_height(rows)
            .max_width(self.setup_metrics.sheet_w);
        let layout = ui::widget::modal_dialog::layout(self.rect, &spec, text, theme);
        self.setup_layout = Some(layout);
        self.rebuild_overlay_hits();
    }

    fn rebuild_overlay_hits(&mut self) {
        self.overlay_hits.clear();
        if !self.setup_open {
            return;
        }
        let Some(layout) = self.setup_layout.clone() else {
            // Not laid out yet: lay out against the host theme with a
            // renderer of its scale.
            let mut text = TextRenderer::new();
            text.set_text_scale(self.layout_theme.text_scale);
            let theme = self.layout_theme.clone();
            self.layout_setup(&mut text, &theme);
            return;
        };
        let m = self.setup_metrics.clone();
        let band = layout.body;
        let fx = band.x0 + m.label_w;
        let rx = band.x1;
        let rows = m.rows_h(
            self.setup_auth == crate::model::AuthMethod::Password,
            self.setup_editing.is_some(),
        );
        // When the window is too short for every row the body scrolls (wheel
        // and focus driven); a row outside it gets no hit rect, so a field
        // never shares pixels with the footer.
        self.setup_scroll_max = (rows - band.height()).max(0.0);
        self.setup_scroll = self.setup_scroll.clamp(0.0, self.setup_scroll_max);
        let mut y = band.y0 - self.setup_scroll;

        // Name (0), email (1), username (2).
        for i in [0usize, 1, 2] {
            let fr = Rect::new(fx, y, rx, y + m.field_h);
            self.layout_setup_input(i, fr);
            Self::push_setup_hit(&mut self.overlay_hits, band, fr, OverlayHit::SetupField(i));
            y += m.field_h + m.row_gap;
        }

        // How the account signs in: one segmented control.
        let half = m.segment_w / 2.0;
        Self::push_setup_hit(
            &mut self.overlay_hits,
            band,
            Rect::new(fx, y, fx + half, y + m.field_h),
            OverlayHit::AuthPassword,
        );
        Self::push_setup_hit(
            &mut self.overlay_hits,
            band,
            Rect::new(fx + half, y, fx + m.segment_w, y + m.field_h),
            OverlayHit::AuthOAuth,
        );
        y += m.field_h + m.row_gap;

        // The password (3), or the browser sign-in button.
        if self.setup_auth == crate::model::AuthMethod::Password {
            let fr = Rect::new(fx, y, rx, y + m.field_h);
            self.layout_setup_input(3, fr);
            Self::push_setup_hit(&mut self.overlay_hits, band, fr, OverlayHit::SetupField(3));
            y += m.field_h;
        } else {
            let btn = Rect::new(fx, y, (fx + m.oauth_w).min(rx), y + m.field_h);
            Self::push_setup_hit(&mut self.overlay_hits, band, btn, OverlayHit::OAuthSignIn);
            y += m.field_h + m.gap + m.caption_h;
        }
        y += m.section_gap;

        // Incoming and outgoing servers.
        y = self.layout_server_rows(fx, rx, y, band);
        y += m.section_gap + m.hint_h;

        // Removing the account being edited, never on the add flow.
        if self.setup_editing.is_some() {
            y += m.section_gap;
            let rect = Rect::new(band.x0, y, band.x0 + m.remove_w, y + m.field_h);
            Self::push_setup_hit(
                &mut self.overlay_hits,
                band,
                rect,
                OverlayHit::RemoveAccount,
            );
        }

        // The footer and the close control are never scrolled.
        if let Some(cancel) = layout.actions.first() {
            self.overlay_hits.push((*cancel, OverlayHit::CancelSetup));
        }
        if let Some(save) = layout.actions.get(1) {
            self.overlay_hits.push((*save, OverlayHit::SetupSave));
        }
        if let Some(close) = layout.close {
            self.overlay_hits.push((close, OverlayHit::CancelSetup));
        }
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

    /// Notice a newly-set status toast, arm its dismiss deadline, and clear the
    /// status once the deadline passes. Parameterised on `now` so the timing is
    /// unit-testable without a real sleep.
    fn expire_toast_at(&mut self, now: ui::time::Instant) {
        let current = self.shared.borrow().status.clone();
        if current != self.toast_text {
            // The status changed (or was cleared): (re)arm the timer.
            self.toast_text.clone_from(&current);
            self.toast_deadline = current.as_ref().map(|_| now + TOAST_TTL);
        }
        if let Some(deadline) = self.toast_deadline
            && now >= deadline
        {
            let mut st = self.shared.borrow_mut();
            st.status = None;
            st.dirty = true;
            self.toast_text = None;
            self.toast_deadline = None;
        }
    }

    /// Whether a toast is currently on screen and still counting down — used to
    /// keep the frame pump alive so the auto-dismiss fires even with no input.
    fn toast_pending(&self) -> bool {
        self.toast_deadline.is_some()
    }

    // ---- engine drain -----------------------------------------------------

    fn apply_fetched_messages(
        &mut self,
        doc: &mut AutoCommit,
        st: &mut MailState,
        request_id: EngineRequestId,
        account_id: &str,
        folder: Folder,
        messages: Vec<Message>,
    ) {
        let count = messages.len();
        st.ingest_messages(doc, &messages);
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
        doc: &mut AutoCommit,
        st: &mut MailState,
        request_id: EngineRequestId,
        wire_bytes: usize,
    ) {
        if self
            .pending_send
            .as_ref()
            .is_none_or(|pending| pending.request_id != request_id)
        {
            return;
        }
        let Some(pending) = self.pending_send.take() else {
            return;
        };
        // A message joins Sent only after SMTP has accepted its bytes. If this
        // was an edited draft, remove it at the same point; failures leave the
        // composer and draft intact for retry.
        if let Some(draft_id) = pending.edited_draft {
            st.mailbox.messages.remove(&draft_id);
            st.mailbox
                .order
                .retain(|message_id| *message_id != draft_id);
        }
        st.ingest_messages(doc, std::slice::from_ref(&pending.message));
        st.mailbox.insert(pending.message);
        self.editing_draft = None;
        self.compose.close();
        st.status = Some(format!("Delivered ({wire_bytes} bytes)"));
    }

    fn apply_engine_error(
        &mut self,
        st: &mut MailState,
        request_id: EngineRequestId,
        context: &str,
        message: &str,
    ) {
        let detail = format!("{context}: {message}");
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
        if self
            .pending_send
            .as_ref()
            .is_some_and(|pending| pending.request_id == request_id)
        {
            // Failed SMTP delivery leaves the composer contents recoverable.
            self.pending_send = None;
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

    fn apply_engine_event(&mut self, doc: &mut AutoCommit, st: &mut MailState, event: EngineEvent) {
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
            EngineEvent::Sent {
                request_id,
                wire_bytes,
            } if self.expects_engine_event(request_id, EngineRequestKind::Send) => {
                self.apply_sent_message(doc, st, request_id, wire_bytes);
            }
            EngineEvent::Authorized {
                request_id,
                account_id,
            } if self.expects_engine_event(request_id, EngineRequestKind::OAuthAuthorize) => {
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
            }
            ignored => {
                tracing::warn!(request_id = %ignored.request_id(), "mail: ignoring unmatched or wrong-kind engine event");
            }
        }
    }

    fn drain_engine(&mut self, doc: &mut AutoCommit) {
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
    fn register_hydrated_messages(&self, doc: &mut AutoCommit) {
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

    /// Flush an active-account change to the device-local shared account store.
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
        match self.accounts_persist.persist(&accounts) {
            Ok(()) => self.persisted_active_account = active,
            Err(error) => {
                self.shared.borrow_mut().status = Some(format!(
                    "Selected account, but could not save that preference: {error}"
                ));
            }
        }
    }

    /// Persist the compose mode on this device only. The caller invokes this
    /// at every format-changing or composer lifecycle boundary; the retained
    /// value makes unchanged close/save/send paths a no-op.
    fn persist_compose_format_if_changed(&mut self) {
        let format = self.compose.format();
        if format == self.persisted_compose_format {
            return;
        }
        match self.accounts_persist.persist_compose_format(format) {
            Ok(()) => self.persisted_compose_format = format,
            Err(error) => {
                self.shared.borrow_mut().status = Some(format!(
                    "Compose format changed, but could not save that preference: {error}"
                ));
            }
        }
    }

    /// Apply collaborative domain state to the local hydrated corpus. Folder,
    /// selection, search, and composer format remain local to this window.
    fn hydrate_domain_state(&self, doc: &mut AutoCommit) {
        {
            let mut st = self.shared.borrow_mut();
            MailDoc::apply_flags(doc, &mut st.mailbox);
        }
        // A persisted draft becomes a local Drafts projection, but does not
        // force another collaborator's composer open.
        let draft = MailDoc::load_draft(doc);
        if !draft.to.is_empty() || !draft.subject.is_empty() || !draft.body.is_empty() {
            self.upsert_draft_message(doc, &draft);
        }
    }

    /// The context the Ctrl+K resource tree is built from: the account label, the
    /// open folder and its messages, the user's local-folder registry, and
    /// whether a draft is in progress. Everything is read from state the document
    /// already holds; nothing is fabricated. See [`crate::actions`].
    pub(crate) fn mail_resource_ctx(&self, doc: &AutoCommit) -> crate::actions::MailResourceCtx {
        let st = self.shared.borrow();
        let mailbox_label = {
            let cfg = st.accounts.active_config();
            if cfg.email.is_empty() {
                "Mailbox".to_string()
            } else {
                cfg.email
            }
        };
        let current_folder = st.folder;
        let accounts = st
            .accounts
            .accounts
            .iter()
            .enumerate()
            .map(|(index, account)| {
                (
                    account.id.clone(),
                    account.label().to_string(),
                    st.accounts.active == index,
                )
            })
            .collect();
        // `messages_in` is the single folder projection shared with the list;
        // importantly it includes the virtual Starred folder.  Give each row
        // the anchor from that same projection so resource/history identities
        // stay independent of user-editable subjects.
        let thread_ids: HashMap<_, _> = st
            .mailbox
            .threads_in(current_folder)
            .into_iter()
            .flat_map(|thread| {
                let anchor = thread.id;
                thread
                    .message_ids
                    .into_iter()
                    .map(move |id| (id, anchor.clone()))
            })
            .collect();
        let messages = st
            .mailbox
            .messages_in(current_folder)
            .into_iter()
            .take(crate::actions::OPEN_MESSAGE_LIMIT)
            .map(|message| {
                let mut row = crate::actions::message_row(message);
                if let Some(thread_id) = thread_ids.get(&message.id) {
                    row.thread_id.clone_from(thread_id);
                }
                row
            })
            .collect();
        let custom_folders = MailDoc::list_folders(doc);
        let draft = MailDoc::load_draft(doc);
        let has_draft = !draft.to.is_empty() || !draft.subject.is_empty() || !draft.body.is_empty();
        let can_sync = self.engine.is_some() && mailbox_label != "Mailbox";
        crate::actions::MailResourceCtx {
            mailbox_label,
            current_folder,
            accounts,
            messages,
            custom_folders,
            has_draft,
            can_sync,
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
                .take(crate::actions::OPEN_MESSAGE_LIMIT)
                .map(crate::actions::message_row)
                .collect(),
        ))
    }

    // ---- drawing the app-owned overlays ----------------------------------

    #[allow(
        clippy::too_many_lines,
        reason = "the popup frame, context header, focus treatment and command bounds use one geometry source so pointer, keyboard and AT cannot drift"
    )]
    fn draw_setup(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.setup_open {
            return;
        }
        // Lay out with this frame's renderer and theme before painting, so
        // what is painted is what is hit-tested.
        self.layout_setup(tr, theme);
        let Some(layout) = self.setup_layout.clone() else {
            return;
        };
        let last_mouse = self.shared.borrow().last_mouse;
        let heading = self.setup_heading();
        let save = self.setup_save_label();
        let busy = self.setup_validation.is_some() || self.setup_discovery.is_some();
        let cancel = crate::i18n::t("mail-setup-cancel");
        let actions = [
            ui::widget::ModalAction::secondary(&cancel).hovered(
                layout
                    .actions
                    .first()
                    .is_some_and(|r| r.contains(last_mouse)),
            ),
            ui::widget::ModalAction::primary(&save)
                .enabled(!busy)
                .hovered(
                    layout
                        .actions
                        .get(1)
                        .is_some_and(|r| r.contains(last_mouse)),
                ),
        ];
        let spec = ui::widget::ModalSpec::new(&heading)
            .actions(&actions)
            .close_hovered(layout.close.is_some_and(|r| r.contains(last_mouse)));
        ui::widget::modal_dialog::paint_backdrop(scene, t, tr, theme, &layout, &spec);
        let sheet = theme.on_ground(ui::chrome::panel_ground(theme));
        // The rows paint inside the body: a row scrolled past its edge is
        // clipped, exactly as its hit rect was dropped.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &layout.body,
        );
        self.draw_setup_rows(scene, t, tr, &sheet, presence);
        scene.pop_layer();
        ui::widget::modal_dialog::paint_actions(scene, t, tr, theme, &layout, &spec);
    }

    /// Every row of the account sheet as last laid out: labels, fields, the
    /// sign-in switch, the server switches, the hint and the remove button.
    #[allow(
        clippy::too_many_lines,
        reason = "the sheet's rows share one scroll offset and one set of measured bands"
    )]
    fn draw_setup_rows(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(layout) = self.setup_layout.clone() else {
            return;
        };
        let last_mouse = self.shared.borrow().last_mouse;
        let m = self.setup_metrics.clone();
        let label_style = theme.text(TextRole::Label);
        let caption = theme.text(TextRole::Caption);
        let errors: std::collections::HashSet<&str> = self
            .read_setup_config()
            .validate()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        let err_for = |i: usize| match i {
            1 => errors.contains("email"),
            2 => errors.contains("username"),
            4 => errors.contains("imap_host"),
            6 => errors.contains("smtp_host"),
            _ => false,
        };
        let x0 = layout.body.x0;
        let label_at =
            |tr: &mut TextRenderer, scene: &mut Scene, text: &str, row: Rect, error: bool| {
                let (_, h) = tr.measure_style(text, None, &label_style);
                let ink = if error {
                    theme.tokens.colors.semantic.danger
                } else {
                    theme.text_dim
                };
                tr.draw_style(
                    scene,
                    text,
                    ink,
                    t * Affine::translate((x0, row.center().y - f64::from(h) / 2.0)),
                    None,
                    &label_style,
                );
            };
        // Field labels and the fields themselves (each input draws its own
        // chrome and focus ring).
        let mut shown = Vec::new();
        let mut sign_in_row = None;
        let mut oauth = None;
        let mut toggles = Vec::new();
        let mut remove = None;
        for (rect, hit) in &self.overlay_hits {
            match hit {
                OverlayHit::SetupField(i) => {
                    shown.push(*i);
                    if let Some(text) = m.labels.get(*i).filter(|l| !l.is_empty()) {
                        label_at(tr, scene, text, *rect, err_for(*i));
                    }
                }
                OverlayHit::AuthPassword => sign_in_row = Some(*rect),
                OverlayHit::OAuthSignIn => oauth = Some(*rect),
                OverlayHit::ToggleImapTls => toggles.push((*rect, self.setup_imap_tls)),
                OverlayHit::ToggleSmtpTls => toggles.push((*rect, self.setup_smtp_tls)),
                OverlayHit::RemoveAccount => remove = Some(*rect),
                _ => {}
            }
        }
        for i in shown {
            if let Some(input) = self.setup_inputs.get_mut(i) {
                input.draw(scene, t, tr, theme, presence);
            }
        }
        if let Some(first) = sign_in_row {
            label_at(tr, scene, &m.sign_in, first, false);
            let half = m.segment_w / 2.0;
            let segments = [
                Rect::new(first.x0, first.y0, first.x0 + half, first.y1),
                Rect::new(first.x0 + half, first.y0, first.x0 + m.segment_w, first.y1),
            ];
            let labels = [m.auth_labels[0].as_str(), m.auth_labels[1].as_str()];
            let selected = usize::from(self.setup_auth == crate::model::AuthMethod::OAuth);
            ToggleGroup::paint(
                scene,
                t,
                tr,
                theme,
                &SegmentedFace::new(&segments, &labels, selected)
                    .hovered(segments.iter().position(|r| r.contains(last_mouse))),
            );
        }
        if let Some(rect) = oauth {
            if let Some(text) = m.labels.get(3) {
                label_at(tr, scene, text, rect, false);
            }
            let label = self.oauth_button_label();
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::new(rect, &label)
                    .role(ui::widget::ButtonRole::Secondary)
                    .hovered(rect.contains(last_mouse)),
            );
            let available = self
                .setup_provider
                .is_some_and(super::oauth::OAuthProvider::is_configured);
            let note = crate::i18n::t(if available {
                "mail-setup-oauth-note-available"
            } else {
                "mail-setup-oauth-note-unavailable"
            });
            tr.draw_style(
                scene,
                &note,
                theme.text_dim,
                t * Affine::translate((rect.x0, rect.y1 + m.gap)),
                Some(ui::num::f32_of_f64(layout.body.x1 - rect.x0)),
                &caption,
            );
        }
        let tls = crate::i18n::t("mail-setup-tls");
        for (rect, on) in &toggles {
            let track = Rect::from_center_size(
                Point::new(rect.x0 + m.toggle.0 / 2.0, rect.center().y),
                m.toggle,
            );
            Toggle::paint(
                scene,
                t,
                theme,
                &ToggleFace::new(track, *on).hovered(rect.contains(last_mouse)),
            );
            let (_, h) = tr.measure_style(&tls, None, &label_style);
            tr.draw_style(
                scene,
                &tls,
                theme.text_primary,
                t * Affine::translate((track.x1 + m.gap, rect.center().y - f64::from(h) / 2.0)),
                None,
                &label_style,
            );
        }
        // The hint under the servers: what Save does, or why it failed.
        let servers_bottom = toggles.last().map_or(layout.body.y0, |(rect, _)| rect.y1);
        let hint_top = servers_bottom + m.section_gap;
        let icon = f64::from(theme.tokens.metrics.icon_sm);
        let icon_rect = Rect::from_center_size(
            Point::new(x0 + icon / 2.0, hint_top + m.hint_h / 2.0),
            (icon, icon),
        );
        let text_x = icon_rect.x1 + f64::from(theme.tokens.metrics.icon_label_gap);
        let width = ui::num::f32_of_f64((layout.body.x1 - text_x).max(0.0));
        if let Some(message) = &self.setup_error {
            Icon::TriangleAlert.draw(scene, t, icon_rect, theme.tokens.colors.semantic.danger);
            let shown = tr.elide_to_width_style(message, width, &caption);
            tr.draw_style(
                scene,
                &shown,
                theme.tokens.colors.semantic.danger,
                t * Affine::translate((text_x, hint_top)),
                None,
                &caption,
            );
        } else {
            Icon::Info.draw(scene, t, icon_rect, theme.text_dim);
            let hint = crate::i18n::t("mail-setup-hint-verified");
            let shown = tr.elide_to_width_style(&hint, width, &caption);
            tr.draw_style(
                scene,
                &shown,
                theme.text_dim,
                t * Affine::translate((text_x, hint_top)),
                None,
                &caption,
            );
        }
        if let Some(rect) = remove {
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::new(rect, "mail-setup-remove")
                    .role(ui::widget::ButtonRole::Destructive)
                    .hovered(rect.contains(last_mouse)),
            );
        }
    }

    fn draw_status(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let st = self.shared.borrow();
        let Some(msg) = &st.status else { return };
        let lower = msg.to_ascii_lowercase();
        let kind = if lower.contains("failed")
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
        };
        paint_transient(
            scene,
            t,
            tr,
            theme,
            self.rect,
            ToastAnchor::BottomRight,
            &ToastCard::new(msg, kind),
        );
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
        match &self.accounts_persist {
            AccountsPersist::Real => accounts::config_root(),
            AccountsPersist::Sandbox(dir) => Some(dir.path().to_path_buf()),
            AccountsPersist::Disabled => None,
        }
    }
}

/// Catalogue keys of the account sheet's field labels, indexed like
/// [`build_setup_inputs`]; the port fields stand beside their host and
/// carry no label of their own.
const SETUP_LABEL_KEYS: [&str; 8] = [
    "mail-setup-label-display-name",
    "mail-setup-label-email",
    "mail-setup-label-username",
    "mail-setup-label-password",
    "mail-setup-label-imap-host",
    "",
    "mail-setup-label-smtp-host",
    "",
];

/// Sizes the account sheet is laid out with, measured with the theme it is
/// drawn with.
#[derive(Clone, Debug, PartialEq)]
struct SetupMetrics {
    /// The field labels (indexed like [`SETUP_LABEL_KEYS`]), the sign-in
    /// label and the two sign-in choices.
    labels: Vec<String>,
    sign_in: String,
    auth_labels: [String; 2],
    /// The label column: the widest label and `s3`.
    label_w: f64,
    field_h: f64,
    gap: f64,
    row_gap: f64,
    section_gap: f64,
    segment_w: f64,
    oauth_w: f64,
    port_w: f64,
    /// The switch's track, and the switch with its label.
    toggle: (f64, f64),
    tls_w: f64,
    caption_h: f64,
    hint_h: f64,
    remove_w: f64,
    /// The widest the sheet is drawn.
    sheet_w: f64,
}

impl Default for SetupMetrics {
    fn default() -> Self {
        Self::measure(&mut TextRenderer::new(), &Theme::dark(), "")
    }
}

impl SetupMetrics {
    /// The field labels from the catalogue, indexed like [`SETUP_LABEL_KEYS`].
    fn resolve_labels() -> Vec<String> {
        SETUP_LABEL_KEYS
            .iter()
            .map(|k| {
                if k.is_empty() {
                    String::new()
                } else {
                    crate::i18n::t(k)
                }
            })
            .collect()
    }

    /// Measure every band with the style it is drawn with. `oauth_label` is
    /// the current "Sign in with …" label.
    fn measure(tr: &mut TextRenderer, theme: &Theme, oauth_label: &str) -> Self {
        let spacing = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        let labels = Self::resolve_labels();
        let sign_in = crate::i18n::t("mail-setup-label-sign-in");
        let label = theme.text(TextRole::Label);
        let label_w = labels
            .iter()
            .chain(std::iter::once(&sign_in))
            .filter(|l| !l.is_empty())
            .map(|l| f64::from(tr.measure_style(l, None, &label).0))
            .fold(0.0_f64, f64::max)
            + f64::from(spacing.s3);
        let auth_labels = [
            crate::i18n::t("mail-setup-auth-password"),
            crate::i18n::t("mail-setup-auth-oauth"),
        ];
        let segment_w = SegmentedFace::intrinsic_width(
            &[auth_labels[0].as_str(), auth_labels[1].as_str()],
            ui::widget::ControlSize::Regular,
            tr,
            theme,
        );
        let button = |tr: &mut TextRenderer, text: &str, role| {
            ButtonFace::new(Rect::ZERO, text)
                .role(role)
                .intrinsic_width(tr, theme)
        };
        // A port holds five digits in the field's own text.
        let digits = theme.text(TextRole::Body).numeric();
        let port_w =
            f64::from(tr.measure_style("65535", None, &digits).0) + 2.0 * f64::from(spacing.s3);
        // The switch's track: an icon-sized knob with one diameter of travel.
        let toggle = (
            f64::from(metrics.icon_lg + metrics.icon_md),
            f64::from(metrics.icon_lg),
        );
        let tls_w = toggle.0
            + f64::from(spacing.s2)
            + f64::from(
                tr.measure_style(&crate::i18n::t("mail-setup-tls"), None, &label)
                    .0,
            );
        let caption = theme.text(TextRole::Caption);
        let caption_h = f64::from(tr.measure_style("Ag", None, &caption).1);
        Self {
            labels,
            sign_in,
            auth_labels,
            label_w,
            field_h: ui::widget::ControlSize::Regular.height(theme),
            gap: f64::from(spacing.s2),
            row_gap: f64::from(spacing.s3),
            section_gap: f64::from(spacing.s6),
            segment_w,
            oauth_w: button(tr, oauth_label, ui::widget::ButtonRole::Secondary),
            port_w,
            toggle,
            tls_w,
            caption_h,
            hint_h: caption_h,
            remove_w: button(tr, "mail-setup-remove", ui::widget::ButtonRole::Destructive),
            sheet_w: f64::from(metrics.panel_width_sheet) * f64::from(theme.text_growth()),
        }
    }

    /// The height every row of the sheet stacks to.
    fn rows_h(&self, password: bool, editing: bool) -> f64 {
        let row = self.field_h + self.row_gap;
        let mut h = 3.0 * row // name, email, username
            + row // sign-in
            + self.field_h; // password, or the sign-in button
        if !password {
            h += self.gap + self.caption_h;
        }
        h += self.section_gap + row + self.field_h // the two servers
            + self.section_gap + self.hint_h;
        if editing {
            h += self.section_gap + self.field_h;
        }
        h
    }
}

fn build_setup_inputs(cfg: &AccountConfig) -> Vec<TextInput> {
    vec![
        TextInput::local()
            .with_kind(ui::widget::TextInputKind::Name)
            .with_initial_value(&cfg.display_name)
            .with_placeholder("Your name"),
        TextInput::local()
            .with_kind(ui::widget::TextInputKind::Email)
            .with_initial_value(&cfg.email)
            .with_placeholder("you@example.com"),
        TextInput::local()
            .with_kind(ui::widget::TextInputKind::Username)
            .with_initial_value(&cfg.username)
            .with_placeholder("Usually your email"),
        TextInput::local()
            .with_kind(ui::widget::TextInputKind::Password)
            .with_initial_value(&cfg.password)
            .with_secret(true)
            .with_placeholder("Account password"),
        TextInput::local()
            .with_kind(ui::widget::TextInputKind::Url)
            .with_initial_value(&cfg.imap_host)
            .with_placeholder("imap.example.com"),
        TextInput::local()
            .with_kind(ui::widget::TextInputKind::Number)
            .with_initial_value(&cfg.imap_port.to_string()),
        TextInput::local()
            .with_kind(ui::widget::TextInputKind::Url)
            .with_initial_value(&cfg.smtp_host)
            .with_placeholder("smtp.example.com"),
        TextInput::local()
            .with_kind(ui::widget::TextInputKind::Number)
            .with_initial_value(&cfg.smtp_port.to_string()),
    ]
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
fn draft_from_message(m: &Message) -> Draft {
    let join = |addrs: &[Addr]| {
        addrs
            .iter()
            .map(super::model::Addr::rfc5322)
            .collect::<Vec<_>>()
            .join(", ")
    };
    Draft {
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

/// Load the account list from the shared canonical store
/// (`$XDG_CONFIG_HOME/tilo/accounts/`), hydrating each password from the
/// encrypted vault. An empty store when nothing is configured yet.
fn load_account_store() -> AccountStore {
    AccountStore::load()
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
    /// The retained search index is deliberately independent from the visible
    /// mailbox. Keep an indexing fault addressable until a later successful
    /// snapshot clears it, instead of hiding it in a transient toast.
    pub(crate) fn mail_search_error(&self) -> Option<String> {
        self.mail_search_provider.last_error()
    }

    #[cfg(test)]
    pub(crate) fn inject_mail_search_snapshot_failure(&self) {
        self.mail_search_provider.inject_snapshot_failure_for_test();
    }

    #[cfg(test)]
    pub(crate) fn refresh_mail_search_for_test(&self) -> Result<(), String> {
        let state = self.shared.borrow();
        self.mail_search_provider.refresh(&state.mailbox)
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
                state: ctx.state,
                path: ctx.path,
                node_id: ctx.node_id,
                taffy: ctx.taffy,
            },
        );
        let lifecycle_boundary = !matches!(&action, ComposeAction::None);
        match action {
            ComposeAction::RequestAttachment => self.request_compose_attachment(),
            ComposeAction::Send => self.send_compose(ctx.state),
            ComposeAction::SaveDraft => self.save_compose_draft(ctx.state),
            ComposeAction::Close => {
                self.dirty = true;
            }
            ComposeAction::None => {}
        }
        if lifecycle_boundary || self.compose.format() != format_before {
            self.persist_compose_format_if_changed();
        }
        consumed
    }

    fn confirm_bulk_purge(&mut self, state: &mut AutoCommit) {
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
                self.apply_menu_action(message, action, ctx.state);
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
                state: ctx.state,
                path: ctx.path,
                handled: &mut handled,
                focus_target: ctx.focus_target,
                node_id: ctx.node_id,
                taffy: ctx.taffy,
            },
        );
        self.take_menu_choice(ctx.state);
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
            ctx.state,
            ctx.path,
        );
        self.settle_purge_confirmation(ctx.state);
        *ctx.handled = true;
        self.dirty = true;
        true
    }

    fn invoke_resource_effect(
        &mut self,
        operation: &crate::actions::MailOperation,
    ) -> Result<bool, String> {
        match operation {
            crate::actions::MailOperation::RefreshCurrentFolder => {
                if self.engine.is_none() {
                    return Err("no connected mail account is available".into());
                }
                let _ = self.refresh_active_folder();
                Ok(true)
            }
            crate::actions::MailOperation::SaveAttachment { .. } => {
                Err("Save attachment must use the admitted portal operation path".into())
            }
            _ => Ok(false),
        }
    }

    fn invoke_resource_view(
        &mut self,
        operation: crate::actions::MailOperation,
        doc: &AutoCommit,
    ) -> Result<bool, String> {
        match operation {
            crate::actions::MailOperation::ComposeNew => {
                self.shared.borrow_mut().push_intent(Intent::Compose);
                self.process_intents();
            }
            crate::actions::MailOperation::RevealFolder { folder } => {
                let Some(folder) = Folder::from_key(&folder) else {
                    return Err("folder no longer exists".into());
                };
                self.shared.borrow_mut().reveal_folder(folder);
                self.process_intents();
            }
            crate::actions::MailOperation::RevealAccount { id } => {
                let Some(index) = self
                    .shared
                    .borrow()
                    .accounts
                    .accounts
                    .iter()
                    .position(|account| account.id == id)
                else {
                    return Err("account no longer exists".into());
                };
                let mut state = self.shared.borrow_mut();
                state.accounts.set_active(index);
                state.reveal_folder(Folder::Inbox);
                drop(state);
                self.process_intents();
            }
            crate::actions::MailOperation::RevealMessage { id: _ } => {
                return Err("Open message must use the admitted mutable operation path".into());
            }
            crate::actions::MailOperation::InspectMessage { id } => {
                let id = id.parse::<MessageId>().map_err(|_| "invalid message id")?;
                if !self.shared.borrow_mut().preview_message(id) {
                    return Err("message is no longer loaded".into());
                }
                self.shared.borrow_mut().status =
                    Some("Previewing without changing this message’s read state".into());
                self.process_intents();
            }
            operation @ (crate::actions::MailOperation::Reply { .. }
            | crate::actions::MailOperation::ReplyAll { .. }
            | crate::actions::MailOperation::Forward { .. }) => {
                let (raw_id, kind) = match operation {
                    crate::actions::MailOperation::Reply { id } => (id, CtxAction::Reply),
                    crate::actions::MailOperation::ReplyAll { id } => (id, CtxAction::ReplyAll),
                    crate::actions::MailOperation::Forward { id } => (id, CtxAction::Forward),
                    _ => return Ok(false),
                };
                let id = raw_id
                    .parse::<MessageId>()
                    .map_err(|_| "invalid message id")?;
                if self.shared.borrow().mailbox.get(id).is_none() {
                    return Err("message is no longer loaded".into());
                }
                let intent = match kind {
                    CtxAction::Reply => Intent::Reply(id),
                    CtxAction::ReplyAll => Intent::ReplyAll(id),
                    CtxAction::Forward => Intent::Forward(id),
                    _ => return Ok(false),
                };
                self.shared.borrow_mut().push_intent(intent);
                self.process_intents();
            }
            crate::actions::MailOperation::RevealDraft => {
                let draft = MailDoc::load_draft(doc);
                self.compose.open_with_draft(&draft);
                self.editing_draft = None;
                self.dirty = true;
            }
            _ => return Ok(false),
        }
        Ok(true)
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
                        state: ctx.state,
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

        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(NamedKey::Escape),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
            && self.setup_open
        {
            self.close_setup();
            *ctx.handled = true;
            return true;
        }

        if self.setup_open {
            self.handle_setup_event(event, ctx);
            if *ctx.handled {
                self.dirty = true;
                return true;
            }
        }
        false
    }

    fn setup_accessibility_fields(&self) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        [
            "Display name",
            "Email address",
            "Username",
            "Password",
            "IMAP server",
            "IMAP port",
            "SMTP server",
            "SMTP port",
        ]
        .iter()
        .enumerate()
        .filter(|(index, _)| self.setup_auth == crate::model::AuthMethod::Password || *index != 3)
        .filter_map(|(index, label)| {
            let input = self.setup_inputs.get(index)?;
            let mut node = AccessNode::new(Role::TextInput)
                .with_name(*label)
                .with_action_target(format!("mail.setup:field:{index}"))
                .with_state(AccessState {
                    focusable: true,
                    focused: self.setup_focus == Some(index),
                    protected: index == 3,
                    ..Default::default()
                });
            if index != 3 {
                node = node.with_value(input.value());
            }
            if let Some(rect) = self.overlay_hits.iter().find_map(|(rect, hit)| {
                matches!(hit, OverlayHit::SetupField(i) if *i == index).then_some(*rect)
            }) {
                node = node.with_bounds(rect);
            }
            Some(node)
        })
        .collect()
    }

    fn setup_accessibility_node(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        let mut controls = self.setup_accessibility_fields();
        controls.extend([
            AccessNode::new(Role::RadioButton)
                .with_name("Password sign-in")
                .with_action_target("mail.setup:auth-password")
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .checked(self.setup_auth == crate::model::AuthMethod::Password),
                ),
            AccessNode::new(Role::RadioButton)
                .with_name("OAuth sign-in")
                .with_action_target("mail.setup:auth-oauth")
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .checked(self.setup_auth == crate::model::AuthMethod::OAuth),
                ),
        ]);
        if self.setup_auth == crate::model::AuthMethod::OAuth {
            controls.push(
                AccessNode::new(Role::Button)
                    .with_name("Sign in with provider")
                    .with_action_target("mail.setup:oauth-sign-in")
                    .with_state(AccessState::new().focusable(true)),
            );
        }
        controls.extend([
            AccessNode::new(Role::ToggleButton)
                .with_name("IMAP TLS")
                .with_action_target("mail.setup:imap-tls")
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .checked(self.setup_imap_tls),
                ),
            AccessNode::new(Role::ToggleButton)
                .with_name("SMTP TLS")
                .with_action_target("mail.setup:smtp-tls")
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .checked(self.setup_smtp_tls),
                ),
            AccessNode::new(Role::Button)
                .with_name("Save and verify")
                .with_action_target("mail.setup:save")
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .busy(self.setup_validation.is_some() || self.setup_discovery.is_some())
                        .disabled(self.setup_validation.is_some()),
                ),
            AccessNode::new(Role::Button)
                .with_name("Cancel")
                .with_action_target("mail.setup:cancel")
                .with_state(AccessState::new().focusable(true)),
        ]);
        if self.setup_editing.is_some() {
            controls.push(
                AccessNode::new(Role::Button)
                    .with_name("Remove account")
                    .with_action_target("mail.setup:remove")
                    .with_state(AccessState::new().focusable(true)),
            );
        }
        if let Some(message) = &self.setup_error {
            controls.push(
                AccessNode::new(Role::StaticText)
                    .with_name(message.clone())
                    .with_live(ui::a11y::Politeness::Polite),
            );
        }
        AccessNode::new(Role::Dialog)
            .with_name("Mail account setup")
            .with_bounds(self.overlay_panel_rect())
            .with_state(AccessState::new().modal(true))
            .with_children(controls)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the list node mirrors the complete visible list and contextual selection toolbar in one accessibility subtree"
    )]
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
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource,
        dyn ui::widget::controls::ActionControls
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::MAIL_OPERATION_TYPE]
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document(state, path);
        self.background_poll_deadline.set(None);
        if self.first_run.take_action() {
            self.open_setup_edit(None);
        }
        // Drain any engine results first so freshly-fetched messages are present
        // before layout/draw.
        self.drain_engine(state);
        self.drain_setup_validation();
        self.drain_setup_discovery();
        // The host Save dialog runs off-thread; its outcome is applied here
        // so attachment export never blocks the frame loop or claims success
        // before the chosen file is actually written.
        self.drain_attachment_save();
        self.drain_compose_attachment();
        self.compose.sync(state, path);
        // Fade out a transient status toast once its lifetime elapses.
        self.expire_toast_at(ui::time::now());

        let mem_seeded = self.shared.borrow().seeded;
        if mem_seeded {
            self.hydrate_domain_state(state);
        } else {
            self.register_hydrated_messages(state);
            // Seeding baselines is not a user undo bracket; discard so a
            // subsequent View (InspectMessage) is not mistaken for a commit.
            let _ = self.shared.borrow_mut().take_committed_action();
            self.hydrate_domain_state(state);
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
        let search_refresh = {
            let state = self.shared.borrow();
            self.mail_search_provider.refresh(&state.mailbox)
        };
        if let Err(error) = search_refresh {
            self.shared.borrow_mut().status = Some(error);
        }
        let title = Self::overview_title(&self.shared.borrow());
        if title != self.overview_title {
            self.nav.set_overview_title(title.clone());
            self.overview_title = title;
        }
        self.nav.sync(state, path);
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = self.admit_resource_operation_with_document(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if self
            .resource_refresh_result
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            self.resource_refresh_invocation = None;
            self.resource_refresh_result.take()
        } else if self
            .resource_attachment_save_result
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            self.resource_attachment_save_result.take()
        } else if cancellation_requested
            && self.resource_attachment_save_invocation == Some(invocation)
        {
            self.attachment_save_rx = None;
            self.pending_attachment_save = None;
            self.resource_attachment_save_invocation = None;
            Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("attachment save cancelled".into()),
            })
        } else {
            None
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The overlays laid out outside the tree (compose, the account
        // sheet) measure against the host theme.
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
        let node = taffy.parent_node(style, &children);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
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
        self.compose.layout(self.rect);
        self.rebuild_overlay_hits();
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
        self.draw_with_presence(
            scene,
            t,
            tr,
            theme,
            presence,
            &PermissionContext::permissive(),
        );
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &PermissionContext,
    ) {
        // No ground of our own: the window paints it, the shell its regions.
        if self.is_first_run() {
            self.first_run.draw(scene, t, tr, theme, presence);
        } else {
            self.nav
                .draw_with_presence(scene, t, tr, theme, presence, permissions);
        }
        if let Some(id) = self.pending_menu.take() {
            self.open_menu_on_row(id, tr, theme);
        }
        // App-owned overlays on top.
        self.compose.draw(scene, t, tr, theme, presence);
        self.compose.draw_inputs(scene, t, tr, theme, presence);
        self.draw_setup(scene, t, tr, theme, presence);
        self.menu.draw(scene, t, tr, theme, presence);
        self.draw_status(scene, t, tr, theme);
        self.shortcuts.draw(scene, t, tr, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            let point = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                state: ctx.state,
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
        self.apply_commands(ctx.state);
        self.process_intents();

        if !*ctx.handled {
            self.global_shortcuts(event, *ctx.modifiers, ctx.state, ctx.handled);
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
        if self.menu.is_open() {
            children.push(&self.menu);
        }
        if let Some(confirmation) = &self.bulk_purge_confirmation {
            children.push(&confirmation.dialog);
        }
        children
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
        if self.setup_open {
            return Some(self.setup_accessibility_node());
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
        if let Some(status) = &st.status {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(status.clone())
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
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, _) = ui::doc::document(state, path);
        if let Some(control) = target.strip_prefix("mail.compose:") {
            return self.perform_compose_semantic_action(control, action, state);
        }
        if let Some(control) = target.strip_prefix("mail.setup:") {
            return self.perform_setup_semantic_action(control, action, state);
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
        } else if self.setup_open {
            Some(self.overlay_panel_rect())
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
            || self.setup_inputs.iter().any(ui::Widget::needs_redraw)
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        // Worker and native-picker completions are not animation. Keep their
        // deadline fixed until sync consumes a tick, even with reduced motion.
        let pending = self.awaiting_engine
            || self.setup_validation.is_some()
            || self.setup_discovery.is_some()
            || self.attachment_save_rx.is_some()
            || self.compose_attachment_rx.is_some();
        let background = pending.then(|| {
            let deadline = self
                .background_poll_deadline
                .get()
                .unwrap_or_else(|| ui::time::now() + std::time::Duration::from_millis(16));
            self.background_poll_deadline.set(Some(deadline));
            deadline
        });
        background.into_iter().chain(self.toast_deadline).min()
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the event loop pumping frames while a worker request is in
        // flight so its result (a fetched batch, a send ack, an error) drains
        // and surfaces promptly instead of waiting for unrelated input, and
        // while a toast is counting down so it fades on time. Drops back to
        // idle the moment the reply settles and the toast clears.
        self.awaiting_engine || self.toast_pending() || self.nav.needs_animation_frame()
    }

    fn needs_idle_sync(&self) -> bool {
        // Keep syncing while a connect/fetch reply is outstanding or a toast is
        // still on screen (so `sync` can expire it), then idle rather than
        // busy-loop once results have arrived and the toast has cleared.
        self.awaiting_engine || self.toast_pending() || self.nav.needs_idle_sync()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.shared.borrow_mut().dirty = false;
        self.nav.mark_drawn();
        self.compose.mark_drawn();
        self.shortcuts.mark_drawn();
        for i in &mut self.setup_inputs {
            i.mark_drawn();
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

impl ui::search::capability::SearchSource for MailApp {
    /// Fold the mailbox into the unified Ctrl+K palette: a snapshot
    /// [`MailSearchProvider`](crate::search_provider::MailSearchProvider) makes
    /// every already-loaded message findable there by subject / sender / body,
    /// alongside the actions the app exports. The provider never touches the
    /// IMAP engine, so a Ctrl+K query stays synchronous.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        let search_refresh = {
            let state = self.shared.borrow();
            self.mail_search_provider.refresh(&state.mailbox)
        };
        if let Err(error) = search_refresh {
            self.shared.borrow_mut().status = Some(error);
        }
        vec![Box::new(self.mail_search_provider.clone())]
    }
}

impl ui::resources::PublishesResources for MailApp {
    /// Standalone / `--shots` keeps the full hand-built door. When hosted,
    /// folder/message Document ops ride [`tilo_resource::publish`]; this
    /// returns only live extras — never a second `mail.mailbox` Document door.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.shared.borrow().actions.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }
    fn action_directory_resource_page(
        &self,
        doc: &AutoCommit,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<ui::widget::ActionDirectoryResourcePage> {
        if self.shared.borrow().actions.is_hosted() {
            // Document mailbox root is ResourceRoot's publish, not a second page.
            return Some(ui::widget::ActionDirectoryResourcePage {
                resources: Vec::new(),
                next_cursor: Vec::new(),
                exhausted: true,
            });
        }
        Some(ui::widget::ActionDirectoryResourcePage {
            resources: if cursor.is_empty() && max_results > 0 {
                self.publish_resources(doc)
            } else {
                Vec::new()
            },
            next_cursor: Vec::new(),
            exhausted: true,
        })
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.is_hosted() && id == "mail.mailbox" {
            return None;
        }
        self.publish_resource(doc, id)
    }
}

impl ui::undo::CommittedActions for MailApp {
    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its mail action, so it records one undo boundary bracketing it (the same
    /// `record_committed_action` path a Ctrl+K dispatch takes). Rising-edge:
    /// cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        // A body edit dispatches through the composer; everything else through
        // Mail's own host. One event does one or the other.
        let body = self.compose.take_committed_action();
        let mail = self.shared.borrow_mut().take_committed_action();
        body.or(mail)
    }
    /// Typing a word in the draft body is one undo step.
    fn keeps_undo_step_open(&self) -> bool {
        self.compose.keeps_undo_step_open()
    }
}

impl MailApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        match kind {
            tilo_actions::OperationKind::Effect => self.invoke_resource_effect(&operation),
            tilo_actions::OperationKind::View => self.invoke_resource_view(operation, doc),
            tilo_actions::OperationKind::Document => Ok(false),
        }
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind != tilo_actions::OperationKind::Effect
            || type_id != crate::actions::MAIL_OPERATION_TYPE
        {
            return Ok(None);
        }
        let operation = crate::actions::decode_operation(type_id, payload)?;
        if let Some(crate::actions::MailOperation::SaveAttachment { message_id, index }) = operation
        {
            let message_id = message_id
                .parse::<MessageId>()
                .map_err(|_| "invalid message id")?;
            self.begin_attachment_save(message_id, index)?;
            self.resource_attachment_save_invocation = Some(invocation);
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if self.resource_refresh_invocation.is_some() || self.awaiting_engine {
            return Err("a mail operation is already running".into());
        }
        if matches!(
            operation,
            Some(crate::actions::MailOperation::RefreshCurrentFolder)
        ) {
            let request_id = self
                .refresh_active_folder()
                .ok_or_else(|| "mail refresh could not be queued".to_string())?;
            self.resource_refresh_invocation = Some((request_id, invocation));
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if !self.answer_resource_operation(kind, type_id, payload, doc, path)? {
            return Ok(None);
        }
        if !self.awaiting_engine {
            return Err("mail refresh could not be queued".into());
        }
        Ok(None)
    }

    /// Admit the compound normal-open operation at the one framework boundary
    /// allowed to mutate the document.  The view transition and its canonical
    /// `SetRead` action are therefore one explicit Open interaction, never a
    /// deferred consequence of rendering or navigation.  Preview deliberately
    /// stays on the read-only operation route above.
    fn admit_resource_operation_with_document(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let operation = crate::actions::decode_operation(type_id, payload)?;
        if kind == tilo_actions::OperationKind::Effect
            && let Some(crate::actions::MailOperation::PrepareCalendarInvitation {
                to,
                subject,
                body,
                calendar_ics,
            }) = operation.clone()
        {
            let draft = prepare_calendar_invitation_draft(&to, &subject, &body, &calendar_ics)?;
            self.shared.borrow_mut().save_draft(doc, &draft);
            self.compose.open_with_draft(&draft);
            self.editing_draft = None;
            self.shared.borrow_mut().status =
                Some("Invitation prepared — review it before sending".into());
            self.dirty = true;
            return Ok(Some(ui::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput {
                    value: None,
                    resources: vec!["mail.draft".into()],
                },
            )));
        }
        if kind == tilo_actions::OperationKind::View
            && let Some(crate::actions::MailOperation::RevealMessage { id }) = operation
        {
            let id = id.parse::<MessageId>().map_err(|_| "invalid message id")?;
            if !self.shared.borrow_mut().open_message(doc, id) {
                return Err("message is no longer loaded".into());
            }
            self.process_intents();
            return Ok(Some(ui::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )));
        }
        self.admit_resource_operation(invocation, kind, type_id, payload, doc, path)
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
        state: &mut AutoCommit,
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
            self.persist_compose_format_if_changed();
        }
        true
    }

    fn perform_setup_semantic_action(
        &mut self,
        control: &str,
        action: &ui::a11y::Action,
        state: &mut AutoCommit,
    ) -> bool {
        use ui::a11y::Action;

        if !self.setup_open {
            return false;
        }
        if let Some(raw) = control.strip_prefix("field:") {
            let Ok(index) = raw.parse::<usize>() else {
                return false;
            };
            if index >= self.setup_inputs.len()
                || (index == 3 && self.setup_auth != crate::model::AuthMethod::Password)
            {
                return false;
            }
            match action {
                Action::Focus | Action::Activate => self.focus_setup(Some(index)),
                Action::SetValue(value) => {
                    self.set_setup_input(index, value);
                    if index == 1 {
                        self.maybe_autofill();
                    }
                }
                _ => return false,
            }
            self.dirty = true;
            return true;
        }
        match (control, action) {
            ("auth-password", Action::Activate | Action::ToggleCheck) => {
                self.setup_auth = crate::model::AuthMethod::Password;
            }
            ("auth-oauth", Action::Activate | Action::ToggleCheck) => {
                self.setup_auth = crate::model::AuthMethod::OAuth;
            }
            ("oauth-sign-in", Action::Activate) => self.start_oauth(),
            ("imap-tls", Action::Activate | Action::ToggleCheck) => {
                self.setup_imap_tls = !self.setup_imap_tls;
                self.set_setup_input(5, if self.setup_imap_tls { "993" } else { "143" });
            }
            ("smtp-tls", Action::Activate | Action::ToggleCheck) => {
                self.setup_smtp_tls = !self.setup_smtp_tls;
                self.set_setup_input(7, if self.setup_smtp_tls { "465" } else { "587" });
            }
            ("save", Action::Activate) => self.save_setup(state),
            ("cancel", Action::Activate) => self.close_setup(),
            ("remove", Action::Activate) if self.setup_editing.is_some() => {
                self.remove_editing_account(state);
            }
            _ => return false,
        }
        self.dirty = true;
        true
    }

    fn perform_reader_semantic_action(
        &mut self,
        raw: &str,
        action: &ui::a11y::Action,
        state: &mut AutoCommit,
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
            if let Err(error) = self.begin_attachment_save(message, index) {
                self.shared.borrow_mut().status = Some(error);
            }
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
        state: &mut AutoCommit,
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
        state: &mut AutoCommit,
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
        state: &mut AutoCommit,
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
            mail.select_message(state, id);
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
        state: &mut AutoCommit,
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

    fn handle_setup_event(&mut self, event: &WindowEvent, ctx: &mut EventContext<'_>) {
        // Wheel: scroll the row band when the window is too short for every
        // row (the footer stays pinned; rows scroll past it are unclickable).
        if let WindowEvent::MouseWheel { delta, .. } = event {
            if self.setup_scroll_max > 0.0 {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, lines) => {
                        f64::from(*lines) * self.setup_metrics.field_h
                    }
                    winit::event::MouseScrollDelta::PixelDelta(p) => {
                        p.y / ctx.scale_factor.max(0.01)
                    }
                };
                self.setup_scroll = (self.setup_scroll - dy).clamp(0.0, self.setup_scroll_max);
                self.rebuild_overlay_hits();
            }
            *ctx.handled = true;
            return;
        }

        // Keyboard: Tab/Shift+Tab move between fields, Enter advances (or
        // submits on the last field); everything else routes to the focused
        // field.
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
            match logical_key {
                Key::Named(NamedKey::Tab) => {
                    let dir = if ctx.modifiers.state().shift_key() {
                        -1
                    } else {
                        1
                    };
                    self.focus_setup_step(dir);
                    *ctx.handled = true;
                    return;
                }
                Key::Named(NamedKey::Enter) => {
                    let order = self.setup_tab_order();
                    let is_last = self.setup_focus.is_some_and(|f| order.last() == Some(&f));
                    if is_last {
                        self.save_setup(ctx.state);
                    } else {
                        self.focus_setup_step(1);
                    }
                    *ctx.handled = true;
                    return;
                }
                _ => {}
            }
        }
        if matches!(event, WindowEvent::KeyboardInput { .. }) {
            let focused = self.setup_focus;
            if let Some(i) = focused.and_then(|i| self.setup_inputs.get_mut(i)) {
                i.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        scale_factor: ctx.scale_factor,
                        modifiers: &ctx.modifiers,
                        state: ctx.state,
                        path: ctx.path,
                        handled: ctx.handled,
                        focus_target: ctx.focus_target,
                        node_id: ctx.node_id,
                        taffy: ctx.taffy,
                    },
                );
            }
            // Typing in the email field pre-fills the server settings from the
            // built-in provider table the instant the address is recognised —
            // the user rarely needs to touch the advanced fields.
            if focused == Some(1) {
                self.maybe_autofill_live_typing();
            }
            return;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            let pt = self.shared.borrow().last_mouse;
            self.handle_setup_click(pt, ctx.state);
            *ctx.handled = true;
        }
    }

    /// The click-routing half of [`Self::handle_setup_event`]: a click at
    /// `pt` lands on a field / pill / toggle / button, or falls outside the
    /// panel and cancels the sheet. Every outcome counts as handled, which
    /// the caller sets unconditionally.
    fn handle_setup_click(&mut self, pt: Point, state: &mut AutoCommit) {
        self.rebuild_overlay_hits();
        let panel = self.overlay_panel_rect();
        let hit = self
            .overlay_hits
            .iter()
            .rev()
            .find(|(rc, _)| rc.contains(pt))
            .map(|(_, h)| *h);
        match hit {
            Some(OverlayHit::SetupField(i)) => {
                let leaving_email = self.setup_focus == Some(1) && i != 1;
                self.focus_setup(Some(i));
                if leaving_email {
                    self.maybe_autofill();
                }
            }
            Some(OverlayHit::AuthPassword) => {
                self.setup_auth = crate::model::AuthMethod::Password;
                self.dirty = true;
            }
            Some(OverlayHit::AuthOAuth) => {
                self.setup_auth = crate::model::AuthMethod::OAuth;
                self.dirty = true;
            }
            Some(OverlayHit::OAuthSignIn) => {
                self.start_oauth();
            }
            Some(OverlayHit::ToggleImapTls) => {
                self.setup_imap_tls = !self.setup_imap_tls;
                self.set_setup_input(5, if self.setup_imap_tls { "993" } else { "143" });
            }
            Some(OverlayHit::ToggleSmtpTls) => {
                self.setup_smtp_tls = !self.setup_smtp_tls;
                self.set_setup_input(7, if self.setup_smtp_tls { "465" } else { "587" });
            }
            Some(OverlayHit::SetupSave) => {
                self.save_setup(state);
            }
            Some(OverlayHit::RemoveAccount) => {
                self.remove_editing_account(state);
            }
            Some(OverlayHit::CancelSetup) => {
                self.close_setup();
            }
            _ => {
                // Click outside the panel cancels.
                if !panel.contains(pt) {
                    self.close_setup();
                }
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "mail's discoverable single-key command map is intentionally centralized to keep chord guards consistent"
    )]
    fn global_shortcuts(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
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
                    self.shared.borrow_mut().status =
                        Some("Previewing without changing this message’s read state".into());
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
    use automerge::ReadDoc;
    use winit::event::{DeviceId, Modifiers};

    fn synth_device() -> DeviceId {
        unsafe { std::mem::zeroed() }
    }
    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    fn esc_event() -> WindowEvent {
        named_key(NamedKey::Escape)
    }
    fn named_key(k: NamedKey) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(k);
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }
    fn char_key(s: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(s.into());
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(s.into());
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }

    /// Route `ev` to the app through the tree it was last laid out in (a
    /// bare node before any layout).
    fn dispatch(app: &mut MailApp, ev: &WindowEvent, doc: &mut AutoCommit) -> bool {
        let (taffy, node) = app.test_layout.take().unwrap_or_else(|| {
            let mut taffy = TaffyTree::<()>::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            (taffy, node)
        });
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        if app.test_layout.is_none() {
            app.test_layout = Some((taffy, node));
        }
        handled
    }

    fn left_release() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Released,
            button: MouseButton::Left,
        }
    }

    fn app() -> (MailApp, AutoCommit) {
        let mut app = MailApp::new_sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
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
            app.pending_send = Some(PendingSend {
                request_id: send_id,
                message,
                edited_draft: None,
            });
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
            self.app.drain_engine(&mut self.doc);
        }
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
            let mut doc = AutoCommit::new();
            let mut state = MailState::new_sample(avatar_store_offline());
            state.save_draft(&mut doc, &draft);
            let stored = MailDoc::load_draft(&doc);
            assert_eq!(stored.to, "guest@example.com");
            assert_eq!(stored.subject, "Invitation: Review");
            assert_eq!(stored.calendar_ics.as_deref(), Some(ics));
            assert!(
                stored.attachments.is_empty(),
                "no host path crosses the action"
            );
            assert!(state.take_committed_action().is_some());
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
            app.apply_commands(&mut doc);
            assert_eq!(app.shared.borrow().folder, Folder::Sent);
            app.sync(&mut doc, &automerge::ROOT);
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
                &mut doc,
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
                &mut doc,
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
                &mut doc,
                &automerge::ROOT,
            ));
            assert!(app.attachment_save_rx.is_some());
            assert!(app.pending_attachment_save.is_some());
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
                &mut doc,
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
            app.shared.borrow_mut().select_message(&mut doc, other);
            app.process_intents();
            assert!(
                !app.perform_semantic_action(
                    &star_target,
                    &Action::Activate,
                    &mut doc,
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
                &mut doc,
                &automerge::ROOT
            ));
            assert!(
                !app.compose.open,
                "semantic discard uses compose close path"
            );

            app.open_setup_edit(None);
            let setup = app.accessibility().expect("setup dialog semantics");
            assert_eq!(setup.role, A11yRole::Dialog);
            assert!(setup.children.iter().any(|node| {
                node.role == A11yRole::TextInput
                    && node.name.as_deref() == Some("Email address")
                    && node.action_target.as_deref() == Some("mail.setup:field:1")
            }));
            assert!(app.perform_semantic_action(
                "mail.setup:cancel",
                &Action::Activate,
                &mut doc,
                &automerge::ROOT
            ));
            assert!(!app.setup_open, "semantic cancel closes account setup");
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
            app.take_menu_choice(&mut doc);
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
            app.shared.borrow_mut().select_message(&mut doc, id);
            assert!(app.shared.borrow().mailbox.get(id).unwrap().read);
            let mut fresh = app.shared.borrow().mailbox.clone();
            for m in fresh.messages.values_mut() {
                m.read = false;
            }
            MailDoc::apply_flags(&doc, &mut fresh);
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
            let payload = serde_json::to_vec(&crate::actions::MailOperation::RevealMessage {
                id: id.to_string(),
            })
            .unwrap();
            assert!(matches!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::default(),
                        tilo_actions::OperationKind::View,
                        crate::actions::MAIL_OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            assert!(
                app.shared.borrow().mailbox.get(id).unwrap().read,
                "Open immediately dispatches the canonical mark-read action"
            );
            let dispatched = app
                .shared
                .borrow()
                .actions
                .last_dispatched()
                .cloned()
                .expect("Open emitted an action");
            let expected = crate::actions::encode(&crate::actions::set_read_action(
                vec![id.to_string()],
                true,
            ));
            assert_eq!(dispatched.type_id, expected.type_id);
            assert_eq!(
                dispatched.payload, expected.payload,
                "resource Open and every on-screen mark-read control use the same action payload"
            );
            assert!(
                doc.get(&automerge::ROOT, "ui")
                    .expect("document remains readable")
                    .is_none(),
                "Open records no selection, folder, search, mode, or compose presentation state"
            );
            assert!(
                ui::undo::take_committed_action(&mut app).is_some(),
                "mark-read remains a replayable collaborative action"
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
                let payload = serde_json::to_vec(&crate::actions::MailOperation::RevealMessage {
                    id: id.to_string(),
                })
                .unwrap();
                assert!(matches!(
                    app.start_operation(
                        &ui::widget::ResourceOperation::local(
                            tilo_actions::InvocationId::default(),
                            tilo_actions::OperationKind::View,
                            crate::actions::MAIL_OPERATION_TYPE,
                            &payload
                        ),
                        &mut doc,
                        &automerge::ROOT
                    )
                    .unwrap(),
                    Some(ui::widget::ResourceOperationStart::Completed(_))
                ));
            }

            assert!(ids.iter().all(|id| {
                app.shared
                    .borrow()
                    .mailbox
                    .get(*id)
                    .is_some_and(|message| message.read)
            }));
        }

        // Phase `legacy_read_only_open_route_refuses_without_changing_presentation_or_document`.
        {
            let (mut app, mut doc) = app();
            let target = app
                .shared
                .borrow()
                .mailbox
                .messages_in(Folder::Inbox)
                .first()
                .map(|message| message.id)
                .expect("sample contains an inbox message");
            let (folder_before, selected_before, inspection_before, status_before) = {
                let state = app.shared.borrow();
                (
                    state.folder,
                    state.selected,
                    state.inspection,
                    state.status.clone(),
                )
            };
            let target_read_before = app
                .shared
                .borrow()
                .mailbox
                .get(target)
                .is_some_and(|message| message.read);
            let heads_before = doc.get_heads();
            let payload = serde_json::to_vec(&crate::actions::MailOperation::RevealMessage {
                id: target.to_string(),
            })
            .unwrap();

            let error = app
                .answer_resource_operation(
                    tilo_actions::OperationKind::View,
                    crate::actions::MAIL_OPERATION_TYPE,
                    &payload,
                    &doc,
                    &automerge::ROOT,
                )
                .expect_err("the read-only route cannot defer an Open mutation");

            assert!(error.contains("admitted mutable operation"));
            assert_eq!(doc.get_heads(), heads_before);
            let state = app.shared.borrow();
            assert_eq!(state.folder, folder_before);
            assert_eq!(state.selected, selected_before);
            assert_eq!(state.inspection, inspection_before);
            assert_eq!(state.status, status_before);
            assert_eq!(
                state
                    .mailbox
                    .get(target)
                    .is_some_and(|message| message.read),
                target_read_before
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
            let payload = serde_json::to_vec(&crate::actions::MailOperation::InspectMessage {
                id: id.to_string(),
            })
            .unwrap();
            let heads_before_preview = doc.get_heads();
            assert!(matches!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::View,
                        crate::actions::MAIL_OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            assert_eq!(app.selected_message(), Some(id));
            assert!(
                !app.shared.borrow().mailbox.get(id).unwrap().read,
                "preview is read-state neutral before sync"
            );
            assert_eq!(doc.get_heads(), heads_before_preview);
            app.sync(&mut doc, &automerge::ROOT);
            assert!(
                !app.shared.borrow().mailbox.get(id).unwrap().read,
                "unlike Open message, explicit preview never queues mark-read"
            );
            assert!(
                ui::undo::take_committed_action(&mut app).is_none(),
                "a read-state-neutral preview creates no collaborative action"
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
            assert!(ui::undo::take_committed_action(&mut app).is_some());
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

            let compose = serde_json::to_vec(&crate::actions::MailOperation::ComposeNew).unwrap();
            assert!(matches!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::View,
                        crate::actions::MAIL_OPERATION_TYPE,
                        &compose
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            assert!(
                app.compose.open,
                "mailbox Compose opens the normal composer"
            );
            app.compose.close();

            let reply = serde_json::to_vec(&crate::actions::MailOperation::Reply {
                id: message.id.to_string(),
            })
            .unwrap();
            assert!(matches!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::View,
                        crate::actions::MAIL_OPERATION_TYPE,
                        &reply
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            assert!(app.compose.open, "message Reply opens the normal composer");
            assert_eq!(
                app.compose.to_draft(&mut doc.clone()).to,
                message.from.rfc5322()
            );
            assert_eq!(
                doc.get_heads(),
                heads_before,
                "an unsaved compose buffer remains local presentation state"
            );
        }

        // Phase `offline_preview_send_keeps_the_message_in_the_composer`.
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
            app.send_compose(&mut doc);
            assert!(app.compose.open, "offline preview must preserve the draft");
            assert_eq!(
                app.shared.borrow().mailbox.count(Folder::Sent),
                before,
                "validating a MIME message locally is not delivery"
            );
            assert!(
                app.shared
                    .borrow()
                    .status
                    .as_deref()
                    .is_some_and(|status| status.contains("offline preview"))
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
            app.shared
                .borrow_mut()
                .apply_message_action(&mut doc, id, CtxAction::ReplyAll);
            app.process_intents();
            assert!(app.compose.open, "reply-all opens the composer");
            let d = app.compose.to_draft(&mut doc);
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
            app.save_compose_draft(&mut doc);
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
            assert_eq!(app.compose.to_draft(&mut doc).subject, "WIP proposal");
            assert_eq!(app.compose.to_draft(&mut doc).attachments, vec![attachment]);
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
            app.save_compose_draft(&mut doc);
            let after_first = app.shared.borrow().mailbox.count(Folder::Drafts);
            // Re-open + re-save the same draft.
            let id = app.editing_draft.expect("editing the saved draft");
            app.open_draft(id);
            app.save_compose_draft(&mut doc);
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
                app.sync(&mut doc, &automerge::ROOT);
                assert!(app.compose_attachment_rx.is_none());
                assert_eq!(app.shared.borrow().status.as_deref(), Some(expected));
                assert_eq!(app.next_wake_deadline(), app.toast_deadline);
                app.expire_toast_at(app.toast_deadline.unwrap());
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
            assert!(dispatch(&mut app, &named_key(NamedKey::Enter), &mut doc));
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
            assert!(fixture.app.pending_send.is_some());
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
                    .is_none_or(|status| !status.contains("Delivered")),
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
                fixture.app.pending_send.is_some(),
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
                fixture.app.poll_resource_operation(
                    fixture.invocation,
                    false,
                    &fixture.doc,
                    &automerge::ROOT,
                ),
                Some(tilo_actions::InvocationUpdate::Succeeded { invocation: got, .. }) if got == fixture.invocation
            ));
            assert!(fixture.app.resource_refresh_invocation.is_none());
            assert!(fixture.app.pending_send.is_some());

            fixture.send(EngineEvent::Sent {
                request_id: fixture.send_id,
                wire_bytes: 42,
            });
            fixture.send(EngineEvent::Completed {
                request_id: fixture.send_id,
                kind: EngineRequestKind::Send,
            });
            fixture.drain();
            assert!(fixture.app.pending_send.is_none());
            assert!(!fixture.app.awaiting_engine);
            assert_eq!(fixture.app.next_wake_deadline(), fixture.app.toast_deadline);
        }

        // Phase `completed_refresh_cannot_be_replaced_before_its_owner_polls`.
        {
            let (mut app, mut doc) = app();
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

            let payload =
                serde_json::to_vec(&crate::actions::MailOperation::RefreshCurrentFolder).unwrap();
            assert!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::Effect,
                        crate::actions::MAIL_OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .is_err()
            );
            assert!(
                app.poll_resource_operation(
                    tilo_actions::InvocationId::new(),
                    false,
                    &doc,
                    &automerge::ROOT
                )
                .is_none()
            );
            assert!(matches!(
                app.poll_resource_operation(owner, false, &doc, &automerge::ROOT),
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
            app.setup_inputs[1].set_text("new@host.example");
            app.setup_inputs[2].set_text("new@host.example");
            app.setup_inputs[4].set_text("imap.host.example");
            app.setup_inputs[6].set_text("smtp.host.example");
            let before = app.shared.borrow().accounts.len();
            // Drive the offline probe explicitly so this offline-save flow is
            // deterministic under both builds (the live probe's connect gating is
            // covered by the `validate` state-machine tests and the socket tests).
            app.save_setup_with(&mut doc, &crate::validate::OfflineProbe);
            assert!(!app.setup_open);
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
            app.setup_inputs[1].set_text("guard@example.test");
            app.setup_inputs[2].set_text("guard@example.test");
            app.setup_inputs[4].set_text("imap.example.test");
            app.setup_inputs[6].set_text("smtp.example.test");
            app.save_setup_with(&mut doc, &crate::validate::OfflineProbe);
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

        // Phase `live_mode_persists_to_the_real_config_root`.
        {
            // The live app is the ONLY constructor allowed to write the real store.
            let app = MailApp::new_live();
            assert_eq!(
                app.accounts_persist_root(),
                accounts::config_root(),
                "the live app must persist to the real per-user config root"
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
                "sam.rivera@northwind.example",
                "the account-settings form is scoped to the clicked account"
            );
        }

        // Phase `sample_compose_format_preference_never_uses_the_real_config_root`.
        {
            let mut app = MailApp::new_sample();
            let sandbox = app.accounts_persist_root().expect("sample sandbox");
            app.compose.set_format(ComposeFormat::Text);
            app.persist_compose_format_if_changed();
            assert!(sandbox.join("mail-local.json").is_file());
            assert_ne!(Some(sandbox), accounts::config_root());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn presentation_state_stays_local_and_the_live_app_starts_empty() {
        // Phase `live_starts_without_fabricated_messages`.
        {
            // The live constructor must not seed a sample corpus.
            let app = MailApp::new_live();
            assert!(
                app.shared.borrow().mailbox.messages.is_empty(),
                "no fabricated startup mail"
            );
        }

        // Phase `presentation_state_is_local_and_never_restored_from_a_peer_document`.
        {
            let mut doc = AutoCommit::new();
            {
                let mut app = MailApp::new_sample();
                app.sync(&mut doc, &automerge::ROOT);
                app.shared
                    .borrow_mut()
                    .select_folder(&mut doc, Folder::Sent);
                app.shared.borrow_mut().set_active_account(1);
                app.compose.set_format(ComposeFormat::Text);
            }
            // A second app sharing the document receives only domain state.
            let mut app2 = MailApp::new_sample();
            app2.sync(&mut doc, &automerge::ROOT);
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

        // Phase `compose_format_restores_per_device_without_entering_the_shared_document`.
        {
            let persistence = AccountsPersist::sandbox();
            let preference_path = persistence
                .local_preferences_path()
                .expect("sandbox has a local preference path");
            let first_state = MailState::shared(MailState::new_sample(avatar_store_offline()));
            let mut first = MailApp::assemble(
                first_state,
                ContentStore::memory(),
                None,
                persistence.clone(),
            );
            first.compose.set_format(ComposeFormat::Text);
            first.persist_compose_format_if_changed();
            assert!(preference_path.is_file(), "format is persisted locally");

            let second_state = MailState::shared(MailState::new_sample(avatar_store_offline()));
            let second = MailApp::assemble(second_state, ContentStore::memory(), None, persistence);
            assert_eq!(
                second.compose.format(),
                ComposeFormat::Text,
                "a later window on this device restores its local preference"
            );

            let mut doc = AutoCommit::new();
            first.sync(&mut doc, &automerge::ROOT);
            let mut peer = MailApp::new_sample();
            peer.sync(&mut doc, &automerge::ROOT);
            assert_eq!(
                peer.compose.format(),
                ComposeFormat::Html,
                "a peer with a different local store never receives this preference"
            );
            assert!(doc.get(&automerge::ROOT, "ui").unwrap().is_none());
        }

        // Phase `active_account_selection_persists_locally_without_touching_automerge`.
        {
            let (mut app, mut doc) = app();
            let selected_id = app.shared.borrow().accounts.accounts[1].id.clone();
            app.shared.borrow_mut().set_active_account(1);
            app.sync(&mut doc, &automerge::ROOT);

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
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            app.layout_for_test(1100.0, 780.0);
            assert!(
                app.is_first_run(),
                "no configured account → first-run onboarding"
            );
            let semantics = app.first_run.accessibility().expect("first-run semantics");
            assert_eq!(semantics.name.as_deref(), Some("Welcome to Mail"));
            assert!(semantics.children.iter().any(|node| {
                node.role == ui::a11y::Role::Button && node.name.as_deref() == Some("Add account")
            }));
            assert!(
                app.first_run.perform_action(
                    &ui::a11y::Action::Activate,
                    &mut doc,
                    &automerge::ROOT,
                ),
                "assistive activation reaches Add account"
            );
            app.sync(&mut doc, &automerge::ROOT);
            assert!(app.setup_open, "Add account opens the account setup sheet");
            assert!(
                !app.is_first_run(),
                "an open setup sheet supersedes the first-run screen"
            );
        }

        // Phase `status_toast_auto_dismisses_after_its_lifetime`.
        {
            // A "Synced N messages" toast must fade on its own, not linger forever.
            let (mut app, _doc) = app();
            let t0 = ui::time::now();
            app.shared.borrow_mut().status = Some("Synced 3 messages".into());
            app.expire_toast_at(t0); // first observation arms the deadline
            assert!(
                app.shared.borrow().status.is_some(),
                "toast shows immediately"
            );
            assert!(app.toast_pending(), "a dismiss deadline is armed");
            assert_eq!(app.next_wake_deadline(), Some(t0 + TOAST_TTL));
            app.expire_toast_at(t0 + std::time::Duration::from_secs(1));
            assert!(
                app.shared.borrow().status.is_some(),
                "still visible within its lifetime"
            );
            app.expire_toast_at(t0 + std::time::Duration::from_secs(5));
            assert!(
                app.shared.borrow().status.is_none(),
                "toast auto-dismissed past its deadline"
            );
            assert!(!app.toast_pending(), "the deadline cleared once dismissed");
            assert!(app.next_wake_deadline().is_none());
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
            app.setup_inputs[1].set_text("person@gmail.com");
            app.maybe_autofill();
            assert_eq!(
                app.setup_inputs[4].value(),
                "imap.gmail.com",
                "IMAP host autofilled"
            );
            assert_eq!(
                app.setup_inputs[6].value(),
                "smtp.gmail.com",
                "SMTP host autofilled"
            );
        }

        // Phase `setup_tls_toggle_flips_the_port`.
        {
            let (mut app, mut doc) = app();
            app.layout_for_test(1100.0, 780.0);
            app.open_setup_edit(None);
            app.rebuild_overlay_hits();
            assert_eq!(
                app.setup_inputs[5].value(),
                "993",
                "IMAP port starts at the TLS default"
            );
            let toggle = app
                .overlay_hits
                .iter()
                .find(|(_, h)| matches!(h, OverlayHit::ToggleImapTls))
                .map(|(r, _)| *r)
                .expect("imap tls toggle");
            assert!(
                click_at(&mut app, &mut doc, toggle.center()),
                "tls toggle consumed"
            );
            assert!(!app.setup_imap_tls, "TLS turned off");
            assert_eq!(
                app.setup_inputs[5].value(),
                "143",
                "port switched to the plaintext IMAP port"
            );
        }

        // Phase `gmail_email_presets_oauth_and_still_autofills_hosts`.
        {
            let (mut app, _doc) = app();
            app.open_setup_edit(None);
            app.setup_inputs[1].set_text("someone@gmail.com");
            app.maybe_autofill();
            assert_eq!(
                app.setup_auth,
                crate::model::AuthMethod::OAuth,
                "Gmail presets to OAuth"
            );
            assert!(
                app.setup_provider.is_some(),
                "provider detected from the domain"
            );
            assert_eq!(
                app.setup_inputs[4].value(),
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
            app.setup_inputs[1].set_text("person@outlook.de");
            app.maybe_autofill();
            assert_eq!(
                app.setup_inputs[4].value(),
                "outlook.office365.com",
                "Outlook → office365, any TLD"
            );
            assert_eq!(app.setup_inputs[6].value(), "smtp.office365.com");

            app.open_setup_edit(None);
            app.setup_inputs[1].set_text("hi@gmx.net");
            app.maybe_autofill();
            assert_eq!(
                app.setup_inputs[4].value(),
                "imap.gmx.net",
                "GMX resolved offline from the table"
            );

            app.open_setup_edit(None);
            app.setup_inputs[1].set_text("me@fastmail.com");
            app.maybe_autofill();
            assert_eq!(app.setup_inputs[4].value(), "imap.fastmail.com");
            assert_eq!(
                app.setup_inputs[5].value(),
                "993",
                "port filled from the table too"
            );
        }

        // Phase `typing_the_email_fills_servers_without_leaving_the_field`.
        {
            // Autofill runs as the address is typed, not only on focus-leave: a key
            // event in the email field pre-fills the servers.
            let (mut app, mut doc) = app();
            app.open_setup_edit(None);
            app.focus_setup(Some(1));
            assert!(
                app.setup_inputs[4].value().is_empty(),
                "IMAP host starts blank"
            );
            app.setup_inputs[1].set_text("me@gmail.com");
            // A key event routed to the focused email field triggers the live fill.
            dispatch(&mut app, &named_key(NamedKey::ArrowRight), &mut doc);
            assert_eq!(
                app.setup_inputs[4].value(),
                "imap.gmail.com",
                "servers fill as the address is typed"
            );
            assert_eq!(app.setup_inputs[6].value(), "smtp.gmail.com");
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
            assert!(app.shared.borrow().selected.is_some());
            dispatch(&mut app, &char_key("e"), &mut doc);
            assert_eq!(
                app.shared.borrow().mailbox.count(Folder::Archive),
                before + 1,
                "'e' archived the selection"
            );
            assert!(ui::undo::take_committed_action(&mut app).is_some());
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
            assert!(ui::undo::take_committed_action(&mut app).is_some());

            assert!(dispatch(&mut app, &char_key("u"), &mut doc));
            assert!(!app.shared.borrow().mailbox.get(id).unwrap().read);
            assert!(ui::undo::take_committed_action(&mut app).is_some());
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
            dispatch(&mut app, &named_key(NamedKey::Delete), &mut doc);
            assert_eq!(
                app.shared.borrow().mailbox.count(Folder::Trash),
                before + 1,
                "Delete moved the selection to Trash"
            );
            assert!(ui::undo::take_committed_action(&mut app).is_some());
        }
    }

    // ---- first-run onboarding --------------------------------------------

    // ---- add-account: single-border layout, Tab, autoconfig, OAuth --------

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn setup_sheet_geometry_and_tab_order_hold_at_every_scale() {
        // Phase `setup_sheet_rows_follow_the_theme_and_never_overlap`.
        {
            for scale in [1.0_f32, 2.0] {
                let (mut app, _doc) = app();
                app.layout_theme = text_scaled(scale);
                app.layout_for_test(1100.0, 900.0);
                app.open_setup_edit(None);
                app.setup_layout = None;
                app.rebuild_overlay_hits();
                let rect_of = |hit: OverlayHit| {
                    app.overlay_hits
                        .iter()
                        .find(|(_, h)| *h == hit)
                        .map(|(r, _)| *r)
                        .expect("hit present")
                };
                let panel = app.overlay_panel_rect();
                let name = rect_of(OverlayHit::SetupField(0));
                let email = rect_of(OverlayHit::SetupField(1));
                let save = rect_of(OverlayHit::SetupSave);
                let cancel = rect_of(OverlayHit::CancelSetup);
                let theme = text_scaled(scale);
                assert_eq!(
                    name.height(),
                    ui::widget::ControlSize::Regular.height(&theme),
                    "a field is a regular control at {scale}×"
                );
                assert!(email.y0 >= name.y1, "rows stack without overlap");
                assert!(
                    name.x0 >= panel.x0 + app.setup_metrics.label_w,
                    "the field column clears the labels"
                );
                assert!(
                    cancel.x1 <= save.x0 || save.x1 <= cancel.x0,
                    "Save and Cancel never overlap"
                );
                assert!(save.y1 <= panel.y1, "footer stays inside the sheet");
            }
        }

        // Phase `setup_sheet_in_a_short_window_at_200_percent_never_overlaps_its_footer`.
        {
            // 200% text in a 720 px window: the sheet cannot hold every row, so
            // the row band scrolls. No row hit may share pixels with Save /
            // Cancel (the footer resolves last, so an overlap would fire the wrong
            // control), every visible row lies inside the band, and scrolling
            // brings the last row into reach.
            let (mut app, _doc) = app();
            app.layout_theme = text_scaled(2.0);
            app.layout_for_test(1100.0, 560.0);
            app.open_setup_edit(None);
            app.setup_layout = None;
            app.rebuild_overlay_hits();
            assert!(
                app.setup_scroll_max > 0.0,
                "the rows do not fit; the band must scroll"
            );
            let footer_top = app
                .overlay_hits
                .iter()
                .filter(|(_, h)| matches!(h, OverlayHit::SetupSave))
                .map(|(r, _)| r.y0)
                .fold(f64::MAX, f64::min);
            let assert_disjoint = |app: &MailApp| {
                // The close control and Cancel both cancel; they are the
                // same command in two places.
                let hits: Vec<(Rect, OverlayHit)> = app.overlay_hits.clone();
                for (i, (a, ha)) in hits.iter().enumerate() {
                    for (b, hb) in hits.iter().skip(i + 1) {
                        let overlap = a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1;
                        assert!(!overlap || ha == hb, "{ha:?} {a:?} overlaps {hb:?} {b:?}");
                    }
                }
                let band = app.setup_layout.as_ref().map(|layout| layout.body).unwrap();
                for (r, h) in &hits {
                    if !matches!(h, OverlayHit::SetupSave | OverlayHit::CancelSetup) {
                        assert!(r.y0 >= band.y0 - 0.5, "{h:?} {r:?} is above the body");
                        assert!(
                            r.y1 <= footer_top + 0.5,
                            "{h:?} {r:?} reaches into the footer"
                        );
                    }
                }
            };
            assert_disjoint(&app);
            // The SMTP row is below the fold until the band scrolls to its end.
            assert!(
                !app.overlay_hits
                    .iter()
                    .any(|(_, h)| matches!(h, OverlayHit::SetupField(6))),
                "the SMTP row is not reachable before scrolling"
            );
            app.setup_scroll = app.setup_scroll_max;
            app.rebuild_overlay_hits();
            assert_disjoint(&app);
            assert!(
                app.overlay_hits
                    .iter()
                    .any(|(_, h)| matches!(h, OverlayHit::SetupField(6))),
                "scrolling to the end reaches the SMTP row"
            );
            // Keyboard focus on the last field scrolls it into the band.
            app.setup_scroll = 0.0;
            app.rebuild_overlay_hits();
            app.focus_setup(Some(7));
            assert!(
                app.overlay_hits
                    .iter()
                    .any(|(_, h)| matches!(h, OverlayHit::SetupField(7))),
                "focusing the SMTP port brings its row into the band"
            );
            assert_disjoint(&app);
        }

        // Phase `setup_fields_use_single_border_no_wrapper`.
        {
            let (mut app, _doc) = app();
            app.layout_for_test(1100.0, 780.0);
            app.open_setup_edit(None);
            app.rebuild_overlay_hits();
            let mut n = 0;
            for (rect, hit) in &app.overlay_hits {
                if let OverlayHit::SetupField(i) = hit {
                    // The input is laid out at exactly the field hit rect, so only
                    // its own single border draws — no wider app-drawn border rect
                    // stacks a second outline on top.
                    assert_eq!(
                        app.setup_inputs[*i].get_rect(),
                        *rect,
                        "field {i}: input rect must equal its hit rect (single border)"
                    );
                    n += 1;
                }
            }
            assert_eq!(n, 8, "all eight fields are shown in password mode");
        }

        // Phase `setup_tab_key_advances_between_fields`.
        {
            let (mut app, mut doc) = app();
            app.layout_for_test(1100.0, 780.0);
            app.open_setup_edit(None);
            app.focus_setup(Some(0));
            assert!(
                dispatch(&mut app, &named_key(NamedKey::Tab), &mut doc),
                "Tab consumed"
            );
            assert_eq!(app.setup_focus, Some(1), "Tab advances to the next field");
            assert!(dispatch(&mut app, &named_key(NamedKey::Tab), &mut doc));
            assert_eq!(app.setup_focus, Some(2), "Tab keeps advancing");
            // Reverse step wraps back.
            app.focus_setup_step(-1);
            assert_eq!(app.setup_focus, Some(1), "Shift+Tab moves back a field");
        }

        // Phase `oauth_mode_drops_password_from_tab_order`.
        {
            let (mut app, _doc) = app();
            app.open_setup_edit(None);
            app.setup_auth = crate::model::AuthMethod::Password;
            assert!(
                app.setup_tab_order().contains(&3),
                "password field is tabbable under password auth"
            );
            app.setup_auth = crate::model::AuthMethod::OAuth;
            assert!(
                !app.setup_tab_order().contains(&3),
                "no password field to tab to under OAuth"
            );
        }

        // Phase `setup_enter_on_last_field_submits`.
        // The Enter-key save runs through the key handler, which uses the build's
        // default probe. On the offline (default) build that is the offline probe,
        // so the account persists; the live build performs a real IMAP/SMTP probe
        // that this test provides no server for (that path is covered by the
        // `validate` state-machine tests and the live socket tests).
        #[cfg(not(feature = "live"))]
        {
            let (mut app, mut doc) = app();
            app.layout_for_test(1100.0, 780.0);
            app.open_setup_edit(None);
            app.setup_inputs[1].set_text("z@host.example");
            app.setup_inputs[2].set_text("z@host.example");
            app.setup_inputs[4].set_text("imap.host.example");
            app.setup_inputs[6].set_text("smtp.host.example");
            let before = app.shared.borrow().accounts.len();
            let last = *app.setup_tab_order().last().unwrap();
            app.focus_setup(Some(last));
            assert!(
                dispatch(&mut app, &named_key(NamedKey::Enter), &mut doc),
                "Enter consumed"
            );
            assert_eq!(
                app.shared.borrow().accounts.len(),
                before + 1,
                "Enter on the last field saves the account"
            );
        }

        // Phase `setup_tab_traverses_every_field_in_order`.
        {
            // Tab must reach every field, in the email → … → IMAP → port → SMTP
            // order the user expects, wrapping at the end; Shift+Tab steps back.
            let (mut app, mut doc) = app();
            app.open_setup_edit(None);
            app.setup_auth = crate::model::AuthMethod::Password; // password field tabbable
            app.focus_setup(Some(0));
            for exp in [1usize, 2, 3, 4, 5, 6, 7, 0] {
                assert!(
                    dispatch(&mut app, &named_key(NamedKey::Tab), &mut doc),
                    "Tab consumed"
                );
                assert_eq!(app.setup_focus, Some(exp), "Tab lands on field {exp}");
            }
            // The order genuinely walks email(1) → IMAP host(4) → IMAP port(5) →
            // SMTP host(6) → SMTP port(7).
            let order = app.setup_tab_order();
            let pos = |i: usize| order.iter().position(|&x| x == i).unwrap();
            assert!(pos(1) < pos(4) && pos(4) < pos(5) && pos(5) < pos(6) && pos(6) < pos(7));
            // Shift+Tab (the reverse the handler drives) steps back a field.
            app.focus_setup(Some(4));
            app.focus_setup_step(-1);
            assert_eq!(
                app.setup_focus,
                Some(3),
                "Shift+Tab moves back to the previous field"
            );
        }
    }

    /// The stock dark theme at `scale`× text.
    fn text_scaled(scale: f32) -> Theme {
        Theme::dark().with_text_scale(scale)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn attachment_save_waits_for_the_portal_and_settles_truthfully_once() {
        // Phase `attachment_resource_save_waits_for_portal_and_real_write`.
        {
            let (mut app, mut doc) = app();
            let (message_id, index, filename, bytes) = {
                let state = app.shared.borrow();
                let (message_id, attachment) = state
                    .mailbox
                    .messages
                    .values()
                    .find_map(|message| {
                        message
                            .attachments
                            .iter()
                            .enumerate()
                            .find(|(_, attachment)| attachment.is_downloadable())
                            .map(|(index, attachment)| (message.id, (index, attachment)))
                    })
                    .expect("sample has a hydrated attachment");
                (
                    message_id,
                    attachment.0,
                    attachment.1.filename.clone(),
                    attachment.1.content.clone(),
                )
            };
            let invocation = tilo_actions::InvocationId::new();
            let payload = serde_json::to_vec(&crate::actions::MailOperation::SaveAttachment {
                message_id: message_id.to_string(),
                index,
            })
            .unwrap();
            let temp = tempfile::tempdir().unwrap();
            let destination = temp.path().join(&filename);
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            recorder.respond_to_pick(PortalOutcome::Opened(destination.clone()));
            assert!(matches!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        crate::actions::MAIL_OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Accepted)
            ));
            assert!(
                app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                    .is_none(),
                "opening a picker cannot be reported as a completed save"
            );
            assert_eq!(recorder.pick_requests()[0].title(), "Save attachment");

            app.drain_attachment_save();
            assert_eq!(std::fs::read(&destination).unwrap(), bytes);
            assert!(matches!(
                app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT),
                Some(tilo_actions::InvocationUpdate::Succeeded { output, .. })
                    if output.resources == vec![format!("mail.attachment:{message_id}:{index}")]
            ));
        }

        // Phase `attachment_save_cancellation_and_direct_path_bypass_are_truthful`.
        {
            let (mut app, mut doc) = app();
            let (message_id, index) = {
                let state = app.shared.borrow();
                state
                    .mailbox
                    .messages
                    .values()
                    .find_map(|message| {
                        message
                            .attachments
                            .iter()
                            .enumerate()
                            .find(|(_, attachment)| attachment.is_downloadable())
                            .map(|(index, _)| (message.id, index))
                    })
                    .expect("sample has a hydrated attachment")
            };
            let payload = serde_json::to_vec(&crate::actions::MailOperation::SaveAttachment {
                message_id: message_id.to_string(),
                index,
            })
            .unwrap();
            assert!(
                app.answer_resource_operation(
                    tilo_actions::OperationKind::Effect,
                    crate::actions::MAIL_OPERATION_TYPE,
                    &payload,
                    &doc,
                    &automerge::ROOT
                )
                .expect_err("only the admitted portal route may save an attachment")
                .contains("admitted portal")
            );

            let invocation = tilo_actions::InvocationId::new();
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            recorder.respond_to_pick(PortalOutcome::Cancelled);
            app.begin_attachment_save(message_id, index).unwrap();
            app.resource_attachment_save_invocation = Some(invocation);
            let deadline = app
                .next_wake_deadline()
                .expect("native picker results wake independently of animation");
            assert_eq!(app.next_wake_deadline(), Some(deadline));
            app.sync(&mut doc, &automerge::ROOT);
            assert!(app.attachment_save_rx.is_none());
            assert_eq!(app.next_wake_deadline(), app.toast_deadline);
            assert!(matches!(
                app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT),
                Some(tilo_actions::InvocationUpdate::Cancelled { .. })
            ));
        }

        // Phase `attachment_save_disconnect_and_host_cancellation_settle_once`.
        {
            let (mut app, doc) = app();
            let (message_id, index) = {
                let state = app.shared.borrow();
                state
                    .mailbox
                    .messages
                    .values()
                    .find_map(|message| {
                        message
                            .attachments
                            .iter()
                            .enumerate()
                            .find(|(_, attachment)| attachment.is_downloadable())
                            .map(|(index, _)| (message.id, index))
                    })
                    .expect("sample has a hydrated attachment")
            };
            let invocation = tilo_actions::InvocationId::new();
            app.begin_attachment_save(message_id, index).unwrap();
            app.resource_attachment_save_invocation = Some(invocation);
            let (sender, receiver) = std::sync::mpsc::channel();
            drop(sender);
            app.attachment_save_rx = Some(receiver);
            app.drain_attachment_save();
            assert!(matches!(
                app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT),
                Some(tilo_actions::InvocationUpdate::Failed { .. })
            ));
            assert!(
                app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                    .is_none(),
                "a disconnected picker cannot produce a second terminal update"
            );

            let cancellation = tilo_actions::InvocationId::new();
            app.begin_attachment_save(message_id, index).unwrap();
            app.resource_attachment_save_invocation = Some(cancellation);
            assert!(matches!(
                app.poll_resource_operation(cancellation, true, &doc, &automerge::ROOT),
                Some(tilo_actions::InvocationUpdate::Cancelled { .. })
            ));
            assert!(app.attachment_save_rx.is_none());
            assert!(app.pending_attachment_save.is_none());
            assert!(
                app.poll_resource_operation(cancellation, false, &doc, &automerge::ROOT)
                    .is_none(),
                "host cancellation is terminal exactly once"
            );
        }

        // Phase `attachment_save_name_and_destination_stay_inside_portal_boundary`.
        {
            assert_eq!(
                MailApp::attachment_save_name("../../secret.pdf"),
                "secret.pdf"
            );
            assert_eq!(MailApp::attachment_save_name(".."), "attachment");
            assert_eq!(MailApp::attachment_save_name("\0bad"), "bad");
            let temp = tempfile::tempdir().unwrap();
            let regular = temp.path().join("regular.txt");
            std::fs::write(&regular, b"old contents").unwrap();
            MailApp::write_portal_attachment(&regular, b"new").unwrap();
            assert_eq!(std::fs::read(&regular).unwrap(), b"new");

            #[cfg(unix)]
            {
                let target = temp.path().join("target.txt");
                let link = temp.path().join("unsafe-link.txt");
                std::fs::write(&target, b"unchanged").unwrap();
                std::os::unix::fs::symlink(&target, &link).unwrap();
                assert!(
                    MailApp::write_portal_attachment(&link, b"must not follow")
                        .expect_err("a portal target that became a symlink is rejected")
                        .contains("symbolic-link")
                );
                assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
            }
        }
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
            app.sync(&mut doc, &automerge::ROOT);
            assert_eq!(app.shared.borrow().accounts.accounts.len(), accounts_before);
            assert!(app.setup_validation.is_none());
            assert_eq!(app.next_wake_deadline(), app.toast_deadline);
        }

        // Phase `edited_setup_cannot_persist_an_earlier_successful_verification`.
        {
            let (mut app, mut doc) = app();
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
            app.sync(&mut doc, &automerge::ROOT);
            assert!(app.setup_open);
            assert!(
                app.setup_error
                    .as_deref()
                    .unwrap()
                    .contains("settings changed")
            );
            assert_eq!(app.shared.borrow().accounts.accounts[0].config, original);
            assert!(app.setup_validation.is_none());
        }

        // Phase `disconnected_setup_worker_keeps_the_sheet_open_without_saving`.
        {
            let (mut app, mut doc) = app();
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
            app.sync(&mut doc, &automerge::ROOT);
            assert!(app.setup_open);
            assert!(
                app.setup_error
                    .as_deref()
                    .unwrap()
                    .contains("without a result")
            );
            assert_eq!(app.shared.borrow().accounts.accounts[0].config, original);
            assert!(app.setup_validation.is_none());
        }

        // Phase `successful_setup_worker_persists_only_its_verified_snapshot`.
        {
            let (mut app, mut doc) = app();
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
            app.sync(&mut doc, &automerge::ROOT);
            assert!(!app.setup_open);
            assert!(app.setup_validation.is_none());
            assert!(app.setup_error.is_none());
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
            app.setup_inputs[1].set_text("new@host.example");
            app.setup_inputs[2].set_text("new@host.example");
            app.setup_inputs[4].set_text("imap.host.example");
            app.setup_inputs[6].set_text("smtp.host.example");
            let before = app.shared.borrow().accounts.len();

            // Failing verification: no persist, sheet stays open, error surfaced.
            app.save_setup_with(&mut doc, &FixedProbe { imap_ok: false });
            assert_eq!(
                app.shared.borrow().accounts.len(),
                before,
                "a failed IMAP check must not persist the account"
            );
            assert!(
                app.setup_open,
                "the sheet stays open on a failed verification"
            );
            assert!(
                app.setup_error.as_deref().unwrap_or("").contains("IMAP"),
                "a specific IMAP error is shown"
            );

            // Succeeding verification: account saved, sheet closes.
            app.save_setup_with(&mut doc, &FixedProbe { imap_ok: true });
            assert_eq!(
                app.shared.borrow().accounts.len(),
                before + 1,
                "a verified account persists"
            );
            assert!(!app.setup_open, "the sheet closes on success");
            assert!(app.setup_error.is_none());
        }

        // Phase `setup_verification_failure_is_exposed_to_assistive_technology`.
        {
            let (mut app, mut doc) = app();
            app.open_setup_edit(None);
            app.set_setup_input(1, "new@host.example");
            app.set_setup_input(2, "new@host.example");
            app.set_setup_input(4, "imap.host.example");
            app.set_setup_input(6, "smtp.host.example");
            app.save_setup_with(&mut doc, &FixedProbe { imap_ok: false });
            let dialog = app.accessibility().expect("setup dialog stays available");
            assert!(
                dialog.children.iter().any(|node| {
                    node.name.as_deref() == Some("IMAP login failed: 535 auth rejected")
                        && node.live == Some(ui::a11y::Politeness::Polite)
                }),
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
            let (mut app, mut doc) = app();
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
            app.sync(&mut doc, &automerge::ROOT);
            assert_eq!(app.setup_inputs[4].value(), "manual.imap.invalid");
            assert_eq!(app.setup_inputs[5].value(), "1993");
            assert_eq!(app.setup_inputs[3].value(), "user-entered-secret");
            assert_eq!(app.setup_inputs[6].value(), discovered.smtp_host);
            assert!(app.setup_discovery.is_none());
            assert_eq!(app.next_wake_deadline(), app.toast_deadline);
        }

        // Phase `discovery_for_a_previous_address_cannot_fill_the_current_sheet`.
        {
            let (mut app, mut doc) = app();
            app.open_setup_edit(None);
            app.set_setup_input(1, "old@custom.invalid");
            let (sender, _) = pending_discovery(&mut app);
            app.set_setup_input(1, "new@other.invalid");
            sender
                .send(crate::autoconfig::builtin_autoconfig("fastmail.com"))
                .unwrap();
            app.sync(&mut doc, &automerge::ROOT);
            assert!(app.setup_inputs[4].value().is_empty());
            assert!(app.setup_inputs[6].value().is_empty());
            assert_eq!(app.setup_inputs[1].value(), "new@other.invalid");
        }

        // Phase `closing_discovery_cancels_worker_and_reopening_rejects_its_result`.
        {
            let (mut app, mut doc) = app();
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
            app.sync(&mut doc, &automerge::ROOT);
            assert!(app.setup_inputs[4].value().is_empty());
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
            app.setup_inputs[1].set_text("new@host.example");
            app.setup_inputs[2].set_text("new@host.example");
            app.setup_inputs[4].set_text("imap.host.example");
            app.setup_inputs[6].set_text("smtp.host.example");
            app.save_setup_with(&mut doc, &FixedProbe { imap_ok: true });
            assert!(app.setup_open);
            assert!(
                app.setup_error
                    .as_deref()
                    .unwrap()
                    .contains("Couldn’t save account")
            );
            assert_eq!(app.shared.borrow().accounts, before);
            assert!(app.pending_engine_requests.is_empty());

            app.open_setup_edit(Some(0));
            app.remove_editing_account(&mut doc);
            assert!(app.setup_open);
            assert!(
                app.setup_error
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
