//! `MailApp` — the root widget.
//!
//! It composes the HIG §1 navigation primitive [`ui::widget::NavShell`] with a
//! genuine three-panel layout —
//!
//! ```text
//! [ sidebar (accounts + folders) | list (overview) | message (detail) ]
//! ```
//!
//! Wide: all three abreast, only the sidebar collapses (via the shell's own
//! chevron/hamburger — never an `X`). Narrow: the sidebar becomes an overlay
//! drawer, the list is full-screen, and opening a message slides the detail in
//! from the right with a Back affordance. The shell owns the breakpoint,
//! collapse/push animations, the scrim, and Back/Esc routing; the three
//! [`Pane`]s (all sharing one [`MailState`]) supply the content and raise
//! [`Intent`]s for cross-pane effects.
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

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size as TSize, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ImageStore;
use ui::color_compat::ColorCompat;
use ui::icons::Icon;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{NavPane, NavShell, TextInput, Widget};

use crate::account::{Account, AccountStore};
use crate::compose::{ComposeAction, ComposeWindow};
use crate::doc::MailDoc;
use crate::draw::{fill_rect, fill_round, stroke_round};
use crate::engine::{EngineEvent, EngineRequest};
use crate::model::{
    AccountConfig, Addr, Attachment, ComposeFormat, Draft, Folder, Message, MessageId,
};
use crate::pane::{Pane, Role};
use crate::state::{CtxAction, Intent, MailState, Shared};
use crate::util::format_absolute;

/// Width (logical px) at/above which the shell is wide (docked sidebar +
/// three panes abreast); below it the sidebar is an overlay drawer and the
/// content is a single push page. Shared with the [`NavShell`].
const BREAKPOINT: f64 = 860.0;
const SIDEBAR_W: f64 = 260.0;
const OVERVIEW_W: f64 = 340.0;
/// How often the app auto-refreshes the current folder from the server while in
/// use. Checked each active frame; a manual refresh (the `r` shortcut) forces
/// one immediately regardless.
const AUTO_REFRESH_MIN: i64 = 5;
/// How long a status toast ("Synced N messages", "Account saved", …) stays on
/// screen before it fades out on its own, like any normal transient toast.
const TOAST_TTL: std::time::Duration = std::time::Duration::from_millis(3500);

/// An open row context menu.
struct ContextMenu {
    target: MessageId,
    pos: Point,
    items: Vec<CtxAction>,
}

/// A hit region in one of the app-owned overlays (setup sheet / context menu /
/// first-run onboarding).
#[derive(Clone)]
enum OverlayHit {
    SetupField(usize),
    ToggleImapTls,
    ToggleSmtpTls,
    SetupSave,
    CancelSetup,
    /// Remove the account being edited (only shown when editing an existing one).
    RemoveAccount,
    CtxItem(usize),
    /// Pick the account's sign-in method (password vs OAuth).
    AuthPassword,
    AuthOAuth,
    /// Trigger the OAuth authorization flow for the detected provider.
    OAuthSignIn,
    /// The first-run "Add account" button.
    FirstRunAdd,
}

/// The engine worker channel pair (live app only).
struct EngineHandle {
    tx: std::sync::mpsc::Sender<EngineRequest>,
    rx: std::sync::mpsc::Receiver<EngineEvent>,
}

/// Where account add / edit / remove is persisted.
///
/// Only the **live** app may write the user's real account store
/// (`$XDG_CONFIG_HOME/qos/accounts/`). The sample / `--shots` / test
/// constructors target a throwaway sandbox directory instead, so exercising the
/// save flow (or a test that adds an account) can never leak fabricated demo
/// accounts into the real store. The historic leak — `you@qos.example`,
/// `you@hooli.example`, `new@host.example` appearing pre-configured in the live
/// app — was exactly this: a test that drove the save path wrote the bundled
/// sample store to the real config root.
enum AccountsPersist {
    /// The real per-user config root — live app only.
    Real,
    /// A throwaway directory, removed when the app is dropped. The `TempDir`
    /// guard owns the directory's lifetime.
    Sandbox(tempfile::TempDir),
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
            .prefix("qos-mail-sample-")
            .tempdir()
        {
            Ok(dir) => AccountsPersist::Sandbox(dir),
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
}

pub struct MailApp {
    shared: Shared,
    nav: NavShell,
    compose: ComposeWindow,
    /// The Drafts-folder message the open composer is editing, if any. On save
    /// the same message is updated (not duplicated); on send it is removed.
    editing_draft: Option<MessageId>,
    context_menu: Option<ContextMenu>,

    // account-setup sheet (Settings owns this in production; offered in-app too)
    setup_open: bool,
    setup_inputs: Vec<TextInput>,
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

    engine: Option<EngineHandle>,
    /// Where account changes persist. The live app targets the real user config
    /// root; the sample / shots / test constructors target a throwaway sandbox
    /// so they can never write into the real account store.
    accounts_persist: AccountsPersist,
    connect_requested: bool,
    /// True between posting a connect/fetch and receiving its result, so the
    /// app only keeps polling the worker while a reply is outstanding (it fully
    /// idles otherwise instead of busy-looping).
    awaiting_engine: bool,
    /// Wall-clock minute of the last automatic inbox refresh, so periodic sync
    /// fires at most once per [`AUTO_REFRESH_MIN`] while the app is in use.
    last_auto_refresh_min: i64,

    /// The status text currently being shown as a toast, and the instant it
    /// should auto-dismiss. Any code path may set `shared.status`; the app
    /// notices the change each frame, (re)arms the deadline, and clears the
    /// status once [`TOAST_TTL`] elapses — so a "Synced N messages" toast fades
    /// on its own instead of lingering forever.
    toast_text: Option<String>,
    toast_deadline: Option<std::time::Instant>,

    overlay_hits: Vec<(Rect, OverlayHit)>,
    rect: Rect,
    /// The shell's reveal flap starts fully open (its docked default); the
    /// first real layout snaps it to the resting state for the current width so
    /// a narrow launch doesn't flash the drawer open before easing it closed.
    first_layout: bool,
    dirty: bool,
}

impl MailApp {
    /// The hermetic constructor used by `--shots` and tests: the bundled
    /// sample corpus, no engine, deterministic "now".
    pub fn new_sample() -> Self {
        let shared = MailState::shared(MailState::new_sample(avatar_store_offline()));
        Self::assemble(shared, None, AccountsPersist::sandbox())
    }

    /// The live constructor: a real account list loaded from disk and the
    /// engine worker spawned. No fabricated messages.
    pub fn new_live() -> Self {
        let accounts = load_account_store();
        let shared = MailState::shared(MailState::new_live(accounts, avatar_store()));
        let (tx, rx) = crate::engine::spawn();
        // Only the live app writes the user's real account store.
        Self::assemble(shared, Some(EngineHandle { tx, rx }), AccountsPersist::Real)
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
        Self::assemble(shared, None, AccountsPersist::sandbox())
    }

    fn assemble(
        shared: Shared,
        engine: Option<EngineHandle>,
        accounts_persist: AccountsPersist,
    ) -> Self {
        let sidebar = Pane::boxed(Role::Sidebar, shared.clone());
        let overview = Pane::boxed(Role::List, shared.clone());
        let detail = Pane::boxed(Role::Reader, shared.clone());
        let nav = NavShell::new(overview, detail)
            .with_sidebar(sidebar)
            .with_breakpoint(BREAKPOINT)
            .with_sidebar_width(SIDEBAR_W)
            .with_overview_width(OVERVIEW_W);

        let cfg = shared.borrow().accounts.active_config();
        Self {
            shared,
            nav,
            compose: ComposeWindow::new(ComposeFormat::Html),
            editing_draft: None,
            context_menu: None,
            setup_open: false,
            setup_inputs: build_setup_inputs(&cfg),
            setup_imap_tls: cfg.imap_tls,
            setup_smtp_tls: cfg.smtp_tls,
            setup_editing: None,
            setup_focus: None,
            setup_auth: cfg.auth,
            setup_provider: None,
            setup_error: None,
            engine,
            accounts_persist,
            connect_requested: false,
            awaiting_engine: false,
            last_auto_refresh_min: 0,
            toast_text: None,
            toast_deadline: None,
            overlay_hits: Vec::new(),
            rect: Rect::ZERO,
            first_layout: true,
            dirty: true,
        }
    }

    // ---- `--shots` priming ------------------------------------------------

    /// Open the composer with a representative HTML draft (compose shot).
    pub fn prime_compose_shot(&mut self) {
        self.compose.open_demo();
        self.dirty = true;
    }

    /// Push the detail pane in and snap it (narrow "reading a message" shot).
    pub fn prime_detail_shot(&mut self) {
        self.nav.show_detail();
        self.nav.snap();
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
        self.setup_inputs[0].set_text("Grace Hopper");
        self.setup_inputs[1].set_text("grace@fastmail.com");
        self.maybe_autofill();
        // Land focus on a server field so the single-border focus ring shows.
        self.focus_setup(Some(4));
    }

    /// Open a blank "Add account" sheet with a Gmail address — which presets to
    /// OAuth — so the shot shows the OAuth sign-in flow.
    pub fn prime_setup_oauth_shot(&mut self) {
        self.open_setup_edit(None);
        self.setup_inputs[0].set_text("Grace Hopper");
        self.setup_inputs[1].set_text("grace.hopper@gmail.com");
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
                .find(|m| m.from.email == "news@weekly.qos.example")
                .map(|m| m.id)
        };
        if let Some(id) = id {
            let mut doc = AutoCommit::new();
            self.shared.borrow_mut().select_message(&mut doc, id);
            self.process_intents();
        }
        self.dirty = true;
    }

    /// Open the sidebar overlay drawer and snap it (narrow drawer shot).
    pub fn prime_drawer_shot(&mut self) {
        self.nav.open_sidebar();
        self.nav.snap_sidebar(true);
        self.dirty = true;
    }

    // ---- nav layout -------------------------------------------------------

    fn publish_shell_flags(&mut self) {
        let narrow = self.rect.width() < BREAKPOINT;
        let detail = self.nav.is_detail_shown();
        let sidebar_open = self.nav.is_sidebar_open();
        let mut st = self.shared.borrow_mut();
        st.narrow = narrow;
        st.detail_pushed = detail;
        st.sidebar_open = sidebar_open;
    }

    /// Lay the shell out into the app's rect via a throwaway sub-taffy, the
    /// same shape the offscreen renderer uses for the whole tree.
    fn layout_nav(&mut self) {
        self.publish_shell_flags();
        let (w, h) = (self.rect.width() as f32, self.rect.height() as f32);
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = self.nav.build_layout(&mut taffy, &theme, &mut map);
        let root = taffy
            .new_with_children(
                Style {
                    size: TSize {
                        width: Dimension::length(w),
                        height: Dimension::length(h),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        let mut tr = TextRenderer::new();
        let measure_map = map.clone();
        let _ = taffy.compute_layout_with_measure(
            root,
            TSize {
                width: taffy::AvailableSpace::Definite(w),
                height: taffy::AvailableSpace::Definite(h),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    TSize::ZERO
                }
            },
        );
        self.nav
            .apply_layout(&taffy, child, Point::new(self.rect.x0, self.rect.y0));
        // Resolve the shell to its resting state for this width on the first
        // layout (no entry animation), then re-place the children with the
        // settled rects so a single frame renders deterministically.
        if self.first_layout {
            self.nav.snap();
            self.first_layout = false;
            self.publish_shell_flags();
            self.nav
                .apply_layout(&taffy, child, Point::new(self.rect.x0, self.rect.y0));
        }
        // Compose + overlays sit within the whole app rect.
        self.compose.layout(self.rect);
        self.rebuild_overlay_hits();
    }

    // ---- intent processing ------------------------------------------------

    /// The message currently selected, if any. Lets a test observe where a
    /// Ctrl+K activation navigated.
    pub fn selected_message(&self) -> Option<crate::model::MessageId> {
        self.shared.borrow().selected
    }

    fn process_intents(&mut self) {
        let intents = self.shared.borrow_mut().take_intents();
        for intent in intents {
            match intent {
                Intent::ShowDetail => self.nav.show_detail(),
                Intent::ShowOverview => self.nav.show_overview(),
                Intent::CloseDrawer => self.nav.close_sidebar(),
                Intent::Compose => {
                    self.editing_draft = None;
                    self.compose.open_blank();
                    self.context_menu = None;
                }
                Intent::Reply(id) => self.open_reply(id),
                Intent::ReplyAll(id) => self.open_reply_all(id),
                Intent::Forward(id) => self.open_forward(id),
                Intent::EditDraft(id) => self.open_draft(id),
                Intent::ContextMenu(id, pt) => {
                    let items = self.shared.borrow().context_items_for(id);
                    self.context_menu = Some(ContextMenu {
                        target: id,
                        pos: pt,
                        items,
                    });
                }
                Intent::Settings(idx) => self.open_setup_edit(idx),
                Intent::Engine(req) => {
                    if let Some(h) = &self.engine {
                        let _ = h.tx.send(req);
                        self.awaiting_engine = true;
                    }
                }
                Intent::Toast(msg) => self.shared.borrow_mut().status = Some(msg),
            }
        }
        self.dirty = true;
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
        let msg = match self.shared.borrow().mailbox.get(id).cloned() {
            Some(m) => m,
            None => return,
        };
        self.editing_draft = None;
        let subj = Self::reply_subject(&msg);
        let quote = Self::reply_quote(&msg);
        self.compose.open_reply(&msg.from.rfc5322(), &subj, &quote);
        self.context_menu = None;
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
        self.context_menu = None;
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
        self.context_menu = None;
        self.dirty = true;
    }

    fn open_forward(&mut self, id: MessageId) {
        let msg = match self.shared.borrow().mailbox.get(id).cloned() {
            Some(m) => m,
            None => return,
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
        self.context_menu = None;
        self.dirty = true;
    }

    fn send_compose(&mut self, doc: &mut AutoCommit) {
        let draft = self.compose.to_draft();
        let acct = self.shared.borrow().accounts.active_config();
        let from = Addr::new(acct.display_name.clone(), acct.email.clone());
        match crate::transport::build_mime(&from, &draft) {
            Ok(bytes) => {
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
                    attachments: draft
                        .attachments
                        .iter()
                        .map(|p| attachment_meta(p))
                        .collect(),
                    timestamp_min: now,
                    read: true,
                    starred: false,
                    folder: Folder::Sent,
                    labels: Vec::new(),
                };
                let id = msg.id;
                let edited_draft = self.editing_draft.take();
                {
                    let mut st = self.shared.borrow_mut();
                    // Sending a draft removes it from the Drafts folder.
                    if let Some(draft_id) = edited_draft {
                        st.mailbox.messages.remove(&draft_id);
                        st.mailbox.order.retain(|x| *x != draft_id);
                    }
                    st.mailbox.insert(msg);
                    st.persist_flags(doc, id);
                    let kind = if draft.html_body.is_some() {
                        "HTML"
                    } else {
                        "text"
                    };
                    st.status = Some(format!("Sent ({} bytes, {kind})", bytes.len()));
                }
                // Hand off to the engine worker for real submission (a no-op
                // ack under the stub backend).
                if let Some(h) = &self.engine {
                    let _ = h.tx.send(EngineRequest::Send {
                        account: acct,
                        draft: Box::new(draft),
                    });
                }
            }
            Err(e) => self.shared.borrow_mut().status = Some(format!("Send failed: {e}")),
        }
        self.compose.close();
        self.persist_compose_format(doc);
        self.dirty = true;
    }

    fn save_compose_draft(&mut self, doc: &mut AutoCommit) {
        let draft = self.compose.to_draft();
        // Persist the draft as an action (survives a crash / syncs to peers /
        // one undo boundary) …
        self.shared.borrow_mut().save_draft(doc, &draft);
        // … and materialise it as a real, visible message in the Drafts folder
        // so it can be found and reopened — not a write-only slot.
        let id = self.upsert_draft_message(doc, &draft);
        self.editing_draft = Some(id);
        self.shared.borrow_mut().status = Some("Draft saved to Drafts".into());
        self.compose.close();
        self.persist_compose_format(doc);
        self.dirty = true;
    }

    /// Insert (or, when editing an existing draft, update in place) a Drafts
    /// folder message reflecting `draft`, and return its id.
    fn upsert_draft_message(&mut self, doc: &mut AutoCommit, draft: &Draft) -> MessageId {
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
            attachments: draft
                .attachments
                .iter()
                .map(|p| attachment_meta(p))
                .collect(),
            timestamp_min: now,
            read: true,
            starred: false,
            folder: Folder::Drafts,
            labels: Vec::new(),
        };
        {
            let mut st = self.shared.borrow_mut();
            st.mailbox.insert(msg);
            st.persist_flags(doc, id);
        }
        id
    }

    fn persist_compose_format(&self, doc: &mut AutoCommit) {
        MailDoc::set_compose_format(doc, self.compose.format().as_key());
    }

    // ---- account-setup sheet ---------------------------------------------

    fn open_setup_edit(&mut self, idx: Option<usize>) {
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
        self.context_menu = None;
        self.dirty = true;
    }

    fn close_setup(&mut self) {
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
        let next = ((cur + delta) % n + n) % n;
        // Leaving the email field triggers autoconfig, same as a click-away.
        let leaving_email = self.setup_focus == Some(1) && order[next as usize] != 1;
        self.focus_setup(Some(order[next as usize]));
        if leaving_email {
            self.maybe_autofill();
        }
    }

    fn focus_setup(&mut self, which: Option<usize>) {
        self.setup_focus = which;
        for (i, inp) in self.setup_inputs.iter_mut().enumerate() {
            inp.set_focus(which == Some(i));
        }
        self.dirty = true;
    }

    fn read_setup_config(&self) -> AccountConfig {
        let g = |i: usize| self.setup_inputs[i].value().to_string();
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
        let email = self.setup_inputs[1].value().trim().to_string();
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

        let imap_blank = self.setup_inputs[4].value().trim().is_empty();
        let smtp_blank = self.setup_inputs[6].value().trim().is_empty();
        if !imap_blank && !smtp_blank {
            self.dirty = true;
            return;
        }

        let mut cfg = AccountConfig {
            email: email.clone(),
            username: self.setup_inputs[2].value().to_string(),
            display_name: self.setup_inputs[0].value().to_string(),
            auth: self.setup_auth,
            ..Default::default()
        };
        // Built-in table first (instant, offline). Network discovery only for
        // domains it doesn't cover, and only on the focus-leave/save pass.
        if let Some(ac) = crate::autoconfig::builtin_autoconfig(&domain) {
            ac.apply_to(&mut cfg);
        } else if network {
            match autoconfig_lookup(&email) {
                Some(ac) => ac.apply_to(&mut cfg),
                None => {
                    cfg.autofill_from_email();
                }
            }
        } else {
            cfg.autofill_from_email();
        }
        // The provider table fills only the connection fields; default the
        // identity fields from the address (username = the email, display name =
        // its local part) so a table-filled account is immediately savable.
        if cfg.username.trim().is_empty() {
            cfg.username = email.clone();
        }
        if cfg.display_name.trim().is_empty()
            && let Some((local, _)) = email.split_once('@')
        {
            cfg.display_name = local.to_string();
        }

        if imap_blank {
            self.setup_inputs[4].set_text(&cfg.imap_host);
            self.setup_inputs[5].set_text(&cfg.imap_port.to_string());
            self.setup_imap_tls = cfg.imap_tls;
        }
        if smtp_blank {
            self.setup_inputs[6].set_text(&cfg.smtp_host);
            self.setup_inputs[7].set_text(&cfg.smtp_port.to_string());
            self.setup_smtp_tls = cfg.smtp_tls;
        }
        if self.setup_inputs[2].value().trim().is_empty() {
            self.setup_inputs[2].set_text(&cfg.username);
        }
        if self.setup_inputs[0].value().trim().is_empty() {
            self.setup_inputs[0].set_text(&cfg.display_name);
        }
        // Autoconfig may have upgraded the auth method (OAuth2 in the XML).
        self.setup_auth = cfg.auth;
        self.dirty = true;
    }

    fn save_setup(&mut self, doc: &mut AutoCommit) {
        self.save_setup_with(doc, &default_probe());
    }

    /// Save the account, but only after its credentials pass a live IMAP login
    /// + SMTP handshake through `probe`. On failure the sheet stays open with a
    /// specific error; on success the account is persisted and connected. The
    /// `probe` seam makes the persist-on-success / keep-on-failure behaviour
    /// unit-testable without a network.
    fn save_setup_with(
        &mut self,
        doc: &mut AutoCommit,
        probe: &dyn crate::validate::ProbeTransport,
    ) {
        self.maybe_autofill();
        let cfg = self.read_setup_config();
        let outcome = crate::validate::validate_account(&cfg, probe);
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
            st.accounts.upsert(account);
            MailDoc::store_account(doc, &cfg);
            MailDoc::set_active_account(doc, st.accounts.active);
            // Persist to the shared on-disk store + vault so the account (and
            // its password) survive a restart. Routed through `accounts_persist`
            // so sample / shots / test builds never touch the real store.
            match self.accounts_persist.persist(&st.accounts) {
                Ok(()) => st.status = Some("Account saved".into()),
                Err(e) => st.status = Some(format!("Saved, but could not write to disk: {e}")),
            }
        }
        // Connect + fetch the freshly-saved account through the engine.
        if let Some(h) = &self.engine {
            let _ = h.tx.send(EngineRequest::Connect {
                account: cfg.clone(),
            });
            let _ = h.tx.send(EngineRequest::Fetch {
                account: cfg.clone(),
                folder: Folder::Inbox,
            });
            self.awaiting_engine = true;
        }
        self.close_setup();
    }

    /// Remove the account currently being edited, persist the removal to disk
    /// (dropping it from the shared store + purging its vault secret), and close
    /// the sheet. A no-op on the add flow (no editing target).
    fn remove_editing_account(&mut self, doc: &mut AutoCommit) {
        let Some(idx) = self.setup_editing else {
            return;
        };
        {
            let mut st = self.shared.borrow_mut();
            let Some(id) = st.accounts.accounts.get(idx).map(|a| a.id.clone()) else {
                return;
            };
            st.accounts.remove(&id);
            MailDoc::set_active_account(doc, st.accounts.active);
            match self.accounts_persist.persist(&st.accounts) {
                Ok(()) => st.status = Some("Account removed".into()),
                Err(e) => st.status = Some(format!("Removed, but could not write to disk: {e}")),
            }
            // The removed account's mailbox is no longer relevant; clear it so a
            // stale inbox doesn't linger. A fresh connect repopulates from the
            // new active account.
            st.mailbox = crate::model::Mailbox::new(st.accounts.active_config());
            st.selected = None;
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
        let email = self.setup_inputs[1].value().trim().to_string();
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
        if provider.client_id().is_none() {
            let env = provider.client_id_env();
            self.shared.borrow_mut().status = Some(format!(
                "Set {env} to complete {} sign-in",
                provider.label()
            ));
            self.dirty = true;
            return;
        }
        match &self.engine {
            Some(h) => {
                let _ = h.tx.send(EngineRequest::OAuthAuthorize {
                    account_id: email.clone(),
                    provider,
                    login_hint: email,
                });
                self.awaiting_engine = true;
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

    fn overlay_panel_rect(&self) -> Rect {
        let r = self.rect;
        let w = 660.0_f64.min(r.width() - 48.0);
        // The first-run welcome is shorter than the taller account form.
        let target_h: f64 = if self.setup_open { 500.0 } else { 440.0 };
        let h = target_h.min(r.height() - 48.0);
        let cx = f64::midpoint(r.x0, r.x1);
        let y0 = r.y0 + (r.height() - h) / 2.0;
        Rect::new(cx - w / 2.0, y0, cx + w / 2.0, y0 + h)
    }

    fn rebuild_overlay_hits(&mut self) {
        self.overlay_hits.clear();
        if self.is_first_run() {
            let p = self.overlay_panel_rect();
            let bw = 200.0;
            let cx = f64::midpoint(p.x0, p.x1);
            let by = p.y0 + p.height() * 0.66;
            self.overlay_hits.push((
                Rect::new(cx - bw / 2.0, by, cx + bw / 2.0, by + 44.0),
                OverlayHit::FirstRunAdd,
            ));
        }
        if self.setup_open {
            let p = self.overlay_panel_rect();
            let pad = 26.0;
            let lx = p.x0 + pad;
            let rx = p.x1 - pad;
            let field_h = 34.0;
            let label_w = 120.0;
            let fx = lx + label_w; // field left edge
            let mut y = p.y0 + 76.0;

            // Identity fields: Display name (0), Email (1), Username (2).
            for i in [0usize, 1, 2] {
                let fr = Rect::new(fx, y, rx, y + field_h);
                self.setup_inputs[i].apply_layout_external(fr);
                self.overlay_hits.push((fr, OverlayHit::SetupField(i)));
                y += field_h + 12.0;
            }

            // Sign-in method pills.
            let pill_w = 104.0;
            let pw_pill = Rect::new(fx, y, fx + pill_w, y + field_h);
            let oauth_pill =
                Rect::new(fx + pill_w + 10.0, y, fx + pill_w * 2.0 + 10.0, y + field_h);
            self.overlay_hits.push((pw_pill, OverlayHit::AuthPassword));
            self.overlay_hits.push((oauth_pill, OverlayHit::AuthOAuth));
            y += field_h + 12.0;

            // Password field (3) OR the OAuth sign-in button.
            if self.setup_auth == crate::model::AuthMethod::Password {
                let fr = Rect::new(fx, y, rx, y + field_h);
                self.setup_inputs[3].apply_layout_external(fr);
                self.overlay_hits.push((fr, OverlayHit::SetupField(3)));
            } else {
                let btn = Rect::new(fx, y, fx + 240.0, y + field_h + 4.0);
                self.overlay_hits.push((btn, OverlayHit::OAuthSignIn));
            }
            y += field_h + 20.0;

            // Server rows: host | port | TLS.
            let port_w = 60.0;
            let tls_w = 58.0;
            let host_w = (rx - fx) - 12.0 - port_w - 12.0 - tls_w;
            let imap_host = Rect::new(fx, y, fx + host_w, y + field_h);
            self.setup_inputs[4].apply_layout_external(imap_host);
            self.overlay_hits
                .push((imap_host, OverlayHit::SetupField(4)));
            let port_imap = Rect::new(
                imap_host.x1 + 12.0,
                y,
                imap_host.x1 + 12.0 + port_w,
                y + field_h,
            );
            self.setup_inputs[5].apply_layout_external(port_imap);
            self.overlay_hits
                .push((port_imap, OverlayHit::SetupField(5)));
            let imap_tls = Rect::new(port_imap.x1 + 12.0, y, rx, y + field_h);
            self.overlay_hits
                .push((imap_tls, OverlayHit::ToggleImapTls));
            y += field_h + 12.0;

            let smtp_host = Rect::new(fx, y, fx + host_w, y + field_h);
            self.setup_inputs[6].apply_layout_external(smtp_host);
            self.overlay_hits
                .push((smtp_host, OverlayHit::SetupField(6)));
            let port_smtp = Rect::new(
                smtp_host.x1 + 12.0,
                y,
                smtp_host.x1 + 12.0 + port_w,
                y + field_h,
            );
            self.setup_inputs[7].apply_layout_external(port_smtp);
            self.overlay_hits
                .push((port_smtp, OverlayHit::SetupField(7)));
            let smtp_tls = Rect::new(port_smtp.x1 + 12.0, y, rx, y + field_h);
            self.overlay_hits
                .push((smtp_tls, OverlayHit::ToggleSmtpTls));

            let by = p.y1 - 54.0;
            self.overlay_hits.push((
                Rect::new(lx, by, lx + 168.0, by + 38.0),
                OverlayHit::SetupSave,
            ));
            self.overlay_hits.push((
                Rect::new(rx - 92.0, by, rx, by + 38.0),
                OverlayHit::CancelSetup,
            ));
            // A destructive "Remove" affordance, only when editing an existing
            // account (never on the add flow).
            if self.setup_editing.is_some() {
                self.overlay_hits.push((
                    Rect::new(lx + 180.0, by, lx + 300.0, by + 38.0),
                    OverlayHit::RemoveAccount,
                ));
            }
        }
        if let Some(menu) = &self.context_menu {
            let mr = context_menu_rect(menu, self.rect);
            let mut y = mr.y0 + 6.0;
            for (i, _a) in menu.items.iter().enumerate() {
                self.overlay_hits
                    .push((Rect::new(mr.x0, y, mr.x1, y + 28.0), OverlayHit::CtxItem(i)));
                y += 28.0;
            }
        }
    }

    // ---- sync / refresh ---------------------------------------------------

    /// Post a fetch of the currently-viewed folder for the active account, and
    /// show a syncing indicator. Used by the manual-refresh shortcut and the
    /// periodic auto-refresh. A no-op with no account or no engine.
    fn refresh_active_folder(&mut self) {
        let (account, folder) = {
            let st = self.shared.borrow();
            (st.accounts.active_config(), st.folder)
        };
        if account.email.is_empty() {
            return;
        }
        if let Some(h) = &self.engine {
            let account_id = account.email.clone();
            let _ = h.tx.send(EngineRequest::Fetch { account, folder });
            report_account_sync(
                &account_id,
                accounts::AccountSyncHealth::Syncing,
                false,
                Some("Checking mail"),
            );
            self.awaiting_engine = true;
            self.last_auto_refresh_min = self.shared.borrow().now_min;
            self.shared.borrow_mut().status = Some("Syncing…".into());
            self.dirty = true;
        }
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
            self.refresh_active_folder();
        }
    }

    // ---- transient status toast ------------------------------------------

    /// Notice a newly-set status toast, arm its dismiss deadline, and clear the
    /// status once the deadline passes. Parameterised on `now` so the timing is
    /// unit-testable without a real sleep.
    fn expire_toast_at(&mut self, now: std::time::Instant) {
        let current = self.shared.borrow().status.clone();
        if current != self.toast_text {
            // The status changed (or was cleared): (re)arm the timer.
            self.toast_text = current.clone();
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

    fn drain_engine(&mut self, doc: &mut AutoCommit) {
        let Some(h) = &self.engine else { return };
        let mut events = Vec::new();
        while let Ok(ev) = h.rx.try_recv() {
            events.push(ev);
        }
        if events.is_empty() {
            return;
        }
        let mut settled = false;
        {
            let mut st = self.shared.borrow_mut();
            for ev in events {
                match ev {
                    EngineEvent::Connected { account_id } => {
                        st.status = Some(format!("Connected {account_id}"));
                        report_account_sync(
                            &account_id,
                            accounts::AccountSyncHealth::Syncing,
                            false,
                            Some("Connected; fetching mail"),
                        );
                    }
                    EngineEvent::Folders { folders, .. } => {
                        // The fixed system-folder set drives the sidebar; log the
                        // server's advertised set for visibility.
                        tracing::info!(count = folders.len(), "mail: server folders listed");
                    }
                    EngineEvent::Fetched {
                        account_id,
                        folder,
                        messages,
                    } => {
                        let count = messages.len();
                        for m in messages {
                            let id = m.id;
                            st.mailbox.insert(m);
                            st.persist_flags(doc, id);
                        }
                        if st.folder == folder && st.selected.is_none() {
                            st.selected = st
                                .current_threads()
                                .first()
                                .and_then(|t| t.message_ids.last().copied());
                        }
                        st.status = Some(if count == 0 {
                            format!("{} up to date", folder.title())
                        } else {
                            format!("Synced {count} messages")
                        });
                        report_account_sync(
                            &account_id,
                            accounts::AccountSyncHealth::Healthy,
                            true,
                            Some("Mail is up to date"),
                        );
                        settled = true;
                    }
                    EngineEvent::Sent { wire_bytes } => {
                        st.status = Some(format!("Delivered ({wire_bytes} bytes)"));
                    }
                    EngineEvent::Authorized { account_id } => {
                        // Tokens are sealed in the vault under the email; the
                        // account (saved with that email as its id) reads them
                        // at connect time.
                        st.status = Some(format!("Signed in as {account_id}"));
                        report_account_sync(
                            &account_id,
                            accounts::AccountSyncHealth::Healthy,
                            true,
                            Some("Signed in"),
                        );
                        settled = true;
                    }
                    EngineEvent::Error { context, message } => {
                        // Surface the specific failure so a fetch never fails
                        // silently into an empty inbox.
                        st.status = Some(format!("{context}: {message}"));
                        let account_id = st.accounts.active_config().email;
                        let lower = format!("{context} {message}").to_ascii_lowercase();
                        let health = if ["auth", "login", "password", "oauth", "credential"]
                            .iter()
                            .any(|needle| lower.contains(needle))
                        {
                            accounts::AccountSyncHealth::NeedsAuthentication
                        } else {
                            accounts::AccountSyncHealth::Error
                        };
                        report_account_sync(
                            &account_id,
                            health,
                            false,
                            Some(&format!("{context}: {message}")),
                        );
                        settled = true;
                    }
                }
            }
            st.dirty = true;
        }
        if settled {
            self.awaiting_engine = false;
        }
    }

    fn seed_doc(&mut self, doc: &mut AutoCommit) {
        let st = self.shared.borrow();
        let ids: Vec<_> = st.mailbox.messages.keys().copied().collect();
        for id in ids {
            if let Some(m) = st.mailbox.get(id) {
                MailDoc::store_flags(doc, &m.clone());
            }
        }
        MailDoc::store_account(doc, &st.accounts.active_config());
        MailDoc::set_folder(doc, st.folder);
        MailDoc::set_mode(doc, "mailbox");
        MailDoc::set_compose_format(doc, self.compose.format().as_key());
        MailDoc::set_active_account(doc, st.accounts.active);
        if let Some(sel) = st.selected {
            MailDoc::set_selected(doc, Some(&sel.to_string()));
        }
    }

    /// Restore the session persisted in an already-seeded document: the active
    /// account, the selected folder, the compose format, per-message flags, the
    /// current selection, and any in-progress draft (materialised back into the
    /// visible Drafts folder). Complements [`Self::seed_doc`].
    fn restore_session(&mut self, doc: &mut AutoCommit) {
        // Active account (clamped inside `set_active`).
        if let Some(idx) = MailDoc::get_active_account(doc) {
            self.shared.borrow_mut().accounts.set_active(idx);
        }
        // If the on-disk store had no accounts but the document carries a
        // synced account config, adopt it.
        if self.shared.borrow().accounts.is_empty()
            && let Some(cfg) = MailDoc::load_account(doc)
            && !cfg.email.is_empty()
        {
            self.shared.borrow_mut().accounts.upsert(Account::new(cfg));
        }
        // Compose body format preference.
        if let Some(fmt) = MailDoc::get_compose_format(doc) {
            self.compose.set_format(ComposeFormat::from_key(&fmt));
        }
        // Folder + flags + selection.
        {
            let mut st = self.shared.borrow_mut();
            st.folder = MailDoc::get_folder(doc);
            MailDoc::apply_flags(doc, &mut st.mailbox);
            let restored = MailDoc::get_selected(doc)
                .and_then(|s| uuid::Uuid::parse_str(&s).ok())
                .filter(|id| st.mailbox.get(*id).is_some());
            st.selected = restored.or_else(|| {
                st.current_threads()
                    .first()
                    .and_then(|t| t.message_ids.last().copied())
            });
        }
        // An in-progress persisted draft becomes a visible Drafts message.
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
        // Cap the enumerated messages so an enormous folder can't build an
        // enormous resource tree every frame; the palette is fuzzy-searched, and
        // the open folder is the "open" scope the model intends.
        const MAX_MESSAGES: usize = 200;
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
        let messages = st
            .mailbox
            .messages_in(current_folder)
            .into_iter()
            .take(MAX_MESSAGES)
            .map(|m| crate::actions::MessageRow {
                id: m.id.to_string(),
                subject: m.subject.clone(),
                read: m.read,
                starred: m.starred,
                labels: m.labels.clone(),
            })
            .collect();
        let custom_folders = MailDoc::list_folders(doc);
        let draft = MailDoc::load_draft(doc);
        let has_draft = !draft.to.is_empty() || !draft.subject.is_empty() || !draft.body.is_empty();
        crate::actions::MailResourceCtx {
            mailbox_label,
            current_folder,
            messages,
            custom_folders,
            has_draft,
        }
    }

    // ---- drawing the app-owned overlays ----------------------------------

    fn draw_context_menu(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(menu) = &self.context_menu else {
            return;
        };
        let c = &theme.tokens.colors;
        let mr = context_menu_rect(menu, self.rect);
        scene.fill(
            Fill::NonZero,
            t,
            Color::from_rgba8(0, 0, 0, 80),
            None,
            &RoundedRect::from_rect(
                Rect::new(mr.x0 + 1.0, mr.y0 + 3.0, mr.x1 + 1.0, mr.y1 + 4.0),
                10.0,
            ),
        );
        fill_round(scene, t, mr, 10.0, c.surfaces.raised);
        stroke_round(scene, t, mr, 10.0, 1.0, c.border_white_10);
        let (read, starred) = self
            .shared
            .borrow()
            .mailbox
            .get(menu.target)
            .map_or((false, false), |m| (m.read, m.starred));
        let last_mouse = self.shared.borrow().last_mouse;
        let mut y = mr.y0 + 6.0;
        for a in &menu.items {
            let item = Rect::new(mr.x0, y, mr.x1, y + 28.0);
            if item.contains(last_mouse) {
                fill_round(
                    scene,
                    t,
                    Rect::new(item.x0 + 4.0, item.y0 + 1.0, item.x1 - 4.0, item.y1 - 1.0),
                    6.0,
                    c.surface_white_10,
                );
            }
            let danger = matches!(a, CtxAction::Delete);
            let col = if danger {
                c.semantic.danger
            } else {
                c.text_primary
            };
            tr.draw_text(
                scene,
                &a.label(read, starred),
                12.5,
                col,
                t * Affine::translate((mr.x0 + 14.0, y + 7.0)),
                None,
            );
            y += 28.0;
        }
    }

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
        let c = &theme.tokens.colors;
        fill_rect(scene, t, self.rect, c.surfaces.overlay);
        let p = self.overlay_panel_rect();
        // Soft drop shadow so the sheet floats above the dimmed mailbox.
        scene.fill(
            Fill::NonZero,
            t,
            Color::from_rgba8(0, 0, 0, 90),
            None,
            &RoundedRect::from_rect(Rect::new(p.x0, p.y0 + 6.0, p.x1, p.y1 + 10.0), 16.0),
        );
        fill_round(scene, t, p, 16.0, c.surfaces.raised);
        stroke_round(scene, t, p, 16.0, 1.0, c.border_white_10);
        let pad = 26.0;
        let lx = p.x0 + pad;
        // Editing reads as scoped to a specific account (its address as the
        // subtitle); adding is its own clearly-separate flow.
        let editing_email = self.setup_editing.and_then(|i| {
            let st = self.shared.borrow();
            st.accounts.accounts.get(i).map(|a| {
                if a.config.email.is_empty() {
                    a.label().to_string()
                } else {
                    a.config.email.clone()
                }
            })
        });
        let (heading, subtitle) = match &editing_email {
            Some(email) => ("Account settings", email.clone()),
            None => (
                "Add account",
                "Enter your email — server details fill in automatically.".to_string(),
            ),
        };
        tr.draw_text_weighted(
            scene,
            heading,
            17.0,
            c.text_primary,
            t * Affine::translate((lx, p.y0 + 20.0)),
            None,
            TextWeight::Bold,
        );
        tr.draw_text(
            scene,
            &subtitle,
            11.5,
            c.text_dim,
            t * Affine::translate((lx, p.y0 + 44.0)),
            None,
        );

        let labels = [
            "Display name",
            "Email",
            "Username",
            "Password",
            "IMAP host",
            "",
            "SMTP host",
            "",
        ];
        let cfg_errs: std::collections::HashSet<&str> = self
            .read_setup_config()
            .validate()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        let err_for = |i: usize| match i {
            1 => cfg_errs.contains("email"),
            2 => cfg_errs.contains("username"),
            4 => cfg_errs.contains("imap_host"),
            6 => cfg_errs.contains("smtp_host"),
            _ => false,
        };
        let last_mouse = self.shared.borrow().last_mouse;

        // Field labels (left column). The single border + focus ring is drawn
        // by each TextInput itself — no second app-drawn border stacks on top.
        let mut shown: Vec<usize> = Vec::new();
        let mut smtp_row_bottom = p.y0;
        for (rect, hit) in &self.overlay_hits {
            let OverlayHit::SetupField(i) = hit else {
                continue;
            };
            let i = *i;
            shown.push(i);
            if i == 6 {
                smtp_row_bottom = rect.y1;
            }
            if i < labels.len() && !labels[i].is_empty() {
                let col = if err_for(i) {
                    c.semantic.danger
                } else {
                    c.text_dim
                };
                tr.draw_text(
                    scene,
                    labels[i],
                    12.5,
                    col,
                    t * Affine::translate((lx, rect.y0 + 9.0)),
                    None,
                );
            }
            if i == 5 {
                tr.draw_text(
                    scene,
                    "Port",
                    11.0,
                    c.text_dim,
                    t * Affine::translate((rect.x0, rect.y0 - 15.0)),
                    None,
                );
            }
        }
        for i in shown {
            self.setup_inputs[i].draw(scene, t, tr, theme, presence);
        }

        // Sign-in method pills + label.
        for (rect, hit) in &self.overlay_hits {
            let (label, active) = match hit {
                OverlayHit::AuthPassword => (
                    "Password",
                    self.setup_auth == crate::model::AuthMethod::Password,
                ),
                OverlayHit::AuthOAuth => {
                    ("OAuth", self.setup_auth == crate::model::AuthMethod::OAuth)
                }
                _ => continue,
            };
            if matches!(hit, OverlayHit::AuthPassword) {
                tr.draw_text(
                    scene,
                    "Sign-in",
                    12.5,
                    c.text_dim,
                    t * Affine::translate((lx, rect.y0 + 9.0)),
                    None,
                );
            }
            let hov = rect.contains(last_mouse);
            let bg = if active {
                c.accent[5].with_alpha_factor(0.22)
            } else if hov {
                c.surface_white_10
            } else {
                c.surface_white_5
            };
            fill_round(scene, t, *rect, 8.0, bg);
            stroke_round(
                scene,
                t,
                *rect,
                8.0,
                1.0,
                if active {
                    c.accent[5]
                } else {
                    c.border_white_10
                },
            );
            let col = if active { c.text_primary } else { c.neutral[7] };
            let (w, _) = tr.measure_weighted(label, 12.5, None, TextWeight::Medium);
            tr.draw_text_weighted(
                scene,
                label,
                12.5,
                col,
                t * Affine::translate((
                    f64::midpoint(rect.x0, rect.x1) - f64::from(w) / 2.0,
                    rect.y0 + 9.0,
                )),
                None,
                TextWeight::Medium,
            );
        }

        // OAuth sign-in button (shown in place of the password field).
        for (rect, hit) in &self.overlay_hits {
            if !matches!(hit, OverlayHit::OAuthSignIn) {
                continue;
            }
            let prov = self
                .setup_provider
                .map_or("your provider", super::oauth::OAuthProvider::label);
            let hov = rect.contains(last_mouse);
            fill_round(
                scene,
                t,
                *rect,
                9.0,
                if hov { c.accent[4] } else { c.accent[5] },
            );
            let label = format!("Sign in with {prov}");
            tr.draw_text_weighted(
                scene,
                &label,
                13.0,
                Color::WHITE,
                t * Affine::translate((rect.x0 + 16.0, rect.y0 + 11.0)),
                None,
                TextWeight::Semibold,
            );
            // Note whether a registered client id is provisioned.
            let has_client = self
                .setup_provider
                .and_then(super::oauth::OAuthProvider::client_id)
                .is_some();
            let note = if has_client {
                "Opens your browser to authorize (OAuth2 + PKCE).".to_string()
            } else {
                let env = self.setup_provider.map_or(
                    "QOS_MAIL_*_CLIENT_ID",
                    super::oauth::OAuthProvider::client_id_env,
                );
                format!("Set {env} to enable sign-in.")
            };
            tr.draw_text(
                scene,
                &note,
                10.5,
                c.text_dim,
                t * Affine::translate((rect.x1 + 12.0, rect.y0 + 12.0)),
                None,
            );
        }

        // TLS toggles.
        for (rect, hit) in &self.overlay_hits {
            let (on, label) = match hit {
                OverlayHit::ToggleImapTls => (self.setup_imap_tls, "TLS"),
                OverlayHit::ToggleSmtpTls => (self.setup_smtp_tls, "TLS"),
                _ => continue,
            };
            let pill = Rect::new(rect.x0, rect.y0 + 6.0, rect.x0 + 40.0, rect.y0 + 28.0);
            fill_round(
                scene,
                t,
                pill,
                11.0,
                if on { c.accent[5] } else { c.neutral[3] },
            );
            let knob_x = if on { pill.x1 - 18.0 } else { pill.x0 + 4.0 };
            scene.fill(
                Fill::NonZero,
                t,
                Color::WHITE,
                None,
                &Circle::new(Point::new(knob_x + 7.0, pill.y0 + 11.0), 8.0),
            );
            tr.draw_text(
                scene,
                label,
                12.0,
                c.neutral[8],
                t * Affine::translate((pill.x1 + 8.0, rect.y0 + 10.0)),
                None,
            );
        }

        // Autoconfig hint + validation message, between the server rows and the
        // button row.
        let hint_y = smtp_row_bottom + 18.0;
        if let Some(msg) = &self.setup_error {
            Icon::Ban.draw(
                scene,
                t,
                Rect::new(lx, hint_y - 1.0, lx + 15.0, hint_y + 14.0),
                c.semantic.danger,
            );
            tr.draw_text(
                scene,
                msg,
                11.5,
                c.semantic.danger,
                t * Affine::translate((lx + 22.0, hint_y + 1.0)),
                None,
            );
        } else {
            Icon::Check.draw(
                scene,
                t,
                Rect::new(lx, hint_y - 1.0, lx + 15.0, hint_y + 14.0),
                c.semantic.success,
            );
            tr.draw_text(
                scene,
                "Credentials are verified before the account is saved.",
                11.5,
                c.text_dim,
                t * Affine::translate((lx + 22.0, hint_y + 1.0)),
                None,
            );
        }

        // Save / Cancel.
        for (rect, hit) in &self.overlay_hits {
            match hit {
                OverlayHit::SetupSave => {
                    let hov = rect.contains(last_mouse);
                    fill_round(
                        scene,
                        t,
                        *rect,
                        9.0,
                        if hov { c.accent[4] } else { c.accent[5] },
                    );
                    let label = "Save & verify";
                    let (w, _) = tr.measure_weighted(label, 13.5, None, TextWeight::Semibold);
                    tr.draw_text_weighted(
                        scene,
                        label,
                        13.5,
                        Color::WHITE,
                        t * Affine::translate((
                            f64::midpoint(rect.x0, rect.x1) - f64::from(w) / 2.0,
                            rect.y0 + 11.0,
                        )),
                        None,
                        TextWeight::Semibold,
                    );
                }
                OverlayHit::CancelSetup => {
                    let hov = rect.contains(last_mouse);
                    fill_round(
                        scene,
                        t,
                        *rect,
                        9.0,
                        if hov {
                            c.surface_white_10
                        } else {
                            c.surface_white_5
                        },
                    );
                    let (w, _) = tr.measure("Cancel", 13.0, None);
                    tr.draw_text(
                        scene,
                        "Cancel",
                        13.0,
                        c.neutral[8],
                        t * Affine::translate((
                            f64::midpoint(rect.x0, rect.x1) - f64::from(w) / 2.0,
                            rect.y0 + 11.0,
                        )),
                        None,
                    );
                }
                OverlayHit::RemoveAccount => {
                    let hov = rect.contains(last_mouse);
                    fill_round(
                        scene,
                        t,
                        *rect,
                        9.0,
                        if hov {
                            c.semantic.danger.with_alpha_factor(0.22)
                        } else {
                            c.surface_white_5
                        },
                    );
                    stroke_round(
                        scene,
                        t,
                        *rect,
                        9.0,
                        1.0,
                        c.semantic.danger.with_alpha_factor(0.5),
                    );
                    let (w, _) = tr.measure("Remove", 13.0, None);
                    tr.draw_text(
                        scene,
                        "Remove",
                        13.0,
                        c.semantic.danger,
                        t * Affine::translate((
                            f64::midpoint(rect.x0, rect.x1) - f64::from(w) / 2.0,
                            rect.y0 + 11.0,
                        )),
                        None,
                    );
                }
                _ => {}
            }
        }
    }

    /// The first-run onboarding screen: a welcome panel with a single
    /// "Add account" call to action that opens the setup flow. Shown whenever
    /// no account is configured (instead of an empty inbox).
    fn draw_first_run(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        if !self.is_first_run() {
            return;
        }
        let c = &theme.tokens.colors;
        // Opaque cover over the (empty) mailbox behind it.
        fill_rect(scene, t, self.rect, c.surfaces.base);
        let p = self.overlay_panel_rect();
        fill_round(scene, t, p, 18.0, c.surfaces.raised);
        stroke_round(scene, t, p, 18.0, 1.0, c.border_white_10);
        let cx = f64::midpoint(p.x0, p.x1);

        // Hero mail glyph in an accent disc.
        let disc_r = 34.0;
        let disc_c = Point::new(cx, p.y0 + p.height() * 0.24);
        scene.fill(
            Fill::NonZero,
            t,
            c.accent[5].with_alpha_factor(0.16),
            None,
            &Circle::new(disc_c, disc_r),
        );
        Icon::Mail.draw(
            scene,
            t,
            Rect::new(
                disc_c.x - 20.0,
                disc_c.y - 20.0,
                disc_c.x + 20.0,
                disc_c.y + 20.0,
            ),
            c.accent[5],
        );

        let title = "Welcome to Mail";
        let (tw, _) = tr.measure_weighted(title, 22.0, None, TextWeight::Bold);
        tr.draw_text_weighted(
            scene,
            title,
            22.0,
            c.text_primary,
            t * Affine::translate((cx - f64::from(tw) / 2.0, disc_c.y + disc_r + 18.0)),
            None,
            TextWeight::Bold,
        );
        let sub = "Add your email account to start reading and sending mail.";
        let (sw, _) = tr.measure(sub, 13.0, None);
        tr.draw_text(
            scene,
            sub,
            13.0,
            c.text_dim,
            t * Affine::translate((cx - f64::from(sw) / 2.0, disc_c.y + disc_r + 50.0)),
            None,
        );

        // The single call to action.
        let last_mouse = self.shared.borrow().last_mouse;
        for (rect, hit) in &self.overlay_hits {
            if !matches!(hit, OverlayHit::FirstRunAdd) {
                continue;
            }
            let hov = rect.contains(last_mouse);
            fill_round(
                scene,
                t,
                *rect,
                11.0,
                if hov { c.accent[4] } else { c.accent[5] },
            );
            let label = "Add account";
            let (w, _) = tr.measure_weighted(label, 14.5, None, TextWeight::Semibold);
            let group_w = 18.0 + 8.0 + f64::from(w);
            let gx = f64::midpoint(rect.x0, rect.x1) - group_w / 2.0;
            Icon::Plus.draw(
                scene,
                t,
                Rect::new(gx, rect.y0 + 13.0, gx + 18.0, rect.y0 + 31.0),
                Color::WHITE,
            );
            tr.draw_text_weighted(
                scene,
                label,
                14.5,
                Color::WHITE,
                t * Affine::translate((gx + 26.0, rect.y0 + 13.0)),
                None,
                TextWeight::Semibold,
            );
        }
    }

    fn draw_status(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let st = self.shared.borrow();
        let Some(msg) = &st.status else { return };
        let c = &theme.tokens.colors;
        let (w, _) = tr.measure(msg, 12.5, None);
        let toast = Rect::new(
            self.rect.x1 - f64::from(w) - 64.0,
            self.rect.y1 - 52.0,
            self.rect.x1 - 24.0,
            self.rect.y1 - 16.0,
        );
        fill_round(scene, t, toast, 10.0, c.neutral[2]);
        stroke_round(scene, t, toast, 10.0, 1.0, c.border_white_10);
        tr.draw_text(
            scene,
            "\u{2713}",
            13.0,
            c.semantic.success,
            t * Affine::translate((toast.x0 + 12.0, toast.y0 + 11.0)),
            None,
        );
        tr.draw_text(
            scene,
            msg,
            12.5,
            c.text_primary,
            t * Affine::translate((toast.x0 + 32.0, toast.y0 + 11.0)),
            None,
        );
    }

    // ---- test / probe accessors ------------------------------------------

    /// The content pane currently fronted (for tests / probes).
    pub fn nav_pane(&self) -> NavPane {
        self.nav.pane()
    }

    #[cfg(test)]
    fn layout_for_test(&mut self, w: f64, h: f64) {
        self.rect = Rect::new(0.0, 0.0, w, h);
        self.layout_nav();
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

fn build_setup_inputs(cfg: &AccountConfig) -> Vec<TextInput> {
    vec![
        TextInput::local()
            .with_initial_value(&cfg.display_name)
            .with_placeholder("Your name"),
        TextInput::local()
            .with_initial_value(&cfg.email)
            .with_placeholder("you@example.com"),
        TextInput::local()
            .with_initial_value(&cfg.username)
            .with_placeholder("Usually your email"),
        TextInput::local()
            .with_initial_value(&cfg.password)
            .with_secret(true)
            .with_placeholder("Account password"),
        TextInput::local()
            .with_initial_value(&cfg.imap_host)
            .with_placeholder("imap.example.com"),
        TextInput::local().with_initial_value(&cfg.imap_port.to_string()),
        TextInput::local()
            .with_initial_value(&cfg.smtp_host)
            .with_placeholder("smtp.example.com"),
        TextInput::local().with_initial_value(&cfg.smtp_port.to_string()),
    ]
}

fn context_menu_rect(menu: &ContextMenu, bounds: Rect) -> Rect {
    let w = 210.0;
    let h = 6.0 + menu.items.len() as f64 * 28.0 + 6.0;
    let x0 = menu.pos.x.min(bounds.x1 - w - 8.0).max(bounds.x0 + 4.0);
    let y0 = menu.pos.y.min(bounds.y1 - h - 8.0).max(bounds.y0 + 4.0);
    Rect::new(x0, y0, x0 + w, y0 + h)
}

/// Metadata-only [`Attachment`] for a staged file path: real basename + real
/// size when the file can be stat'd (never fabricated bytes; the bytes are read
/// from the path at send time by `transport::build_mime`).
fn attachment_meta(path: &str) -> Attachment {
    let name = crate::model::attachment_display_name(path).to_string();
    let mime = crate::transport::mime_from_filename(&name).to_string();
    let size = std::fs::metadata(path).map_or(0, |m| m.len());
    Attachment::projected(name, mime, size)
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
        attachments: Vec::new(),
        in_reply_to: None,
    }
}

/// Load the account list from the shared canonical store
/// (`$XDG_CONFIG_HOME/qos/accounts/`), hydrating each password from the
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
    ImageStore::new(avatar_fetcher())
}

/// The avatar store for the hermetic paths — `--shots`, the sample corpus, and
/// tests. It is wired to a no-op fetcher **regardless of the `live` feature**,
/// so a headless render never opens a socket to gravatar.com even though the
/// live fetcher is now compiled in by default; avatars render as initials. This
/// is the runtime half of "features compile the live code in; the runtime mode
/// decides live vs sample".
pub fn avatar_store_offline() -> ImageStore {
    ImageStore::new(noop_avatar_fetcher())
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
            reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(8))
                .user_agent("qOS Mail")
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

/// The account-verification probe used on save. A real IMAP login + SMTP
/// handshake under `--features live`; a no-op offline probe otherwise, so
/// `--shots` and headless tests still persist an account after the config-level
/// checks pass.
#[cfg(feature = "live")]
fn default_probe() -> crate::validate::LiveProbe {
    crate::validate::LiveProbe
}
#[cfg(not(feature = "live"))]
fn default_probe() -> crate::validate::OfflineProbe {
    crate::validate::OfflineProbe
}

/// Mozilla autoconfig discovery for a typed email. Real HTTP under
/// `--features live`; `None` offline (the caller falls back to the heuristic).
#[cfg(feature = "live")]
fn autoconfig_lookup(email: &str) -> Option<crate::autoconfig::Autoconfig> {
    crate::autoconfig::discover(email)
}
#[cfg(not(feature = "live"))]
fn autoconfig_lookup(_email: &str) -> Option<crate::autoconfig::Autoconfig> {
    None
}

// =============================== Widget impl ===============================

impl Widget for MailApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        // Drain any engine results first so freshly-fetched messages are present
        // before layout/draw.
        self.drain_engine(state);
        // Fade out a transient status toast once its lifetime elapses.
        self.expire_toast_at(std::time::Instant::now());

        let mem_seeded = self.shared.borrow().seeded;
        if mem_seeded {
            let mut st = self.shared.borrow_mut();
            MailDoc::apply_flags(state, &mut st.mailbox);
        } else {
            if MailDoc::is_seeded(state) {
                // Reopening an already-persisted document: restore the saved
                // session (folder / selection / active account / compose format /
                // in-progress draft) instead of overwriting it from defaults.
                self.restore_session(state);
            } else {
                self.seed_doc(state);
                MailDoc::set_seeded(state);
            }
            // Live: kick off a connect + inbox fetch for the active account.
            let has_accounts = !self.shared.borrow().accounts.is_empty();
            if self.engine.is_some() && has_accounts && !self.connect_requested {
                let cfg = self.shared.borrow().accounts.active_config();
                if let Some(h) = &self.engine {
                    let _ = h.tx.send(EngineRequest::Connect {
                        account: cfg.clone(),
                    });
                    let _ = h.tx.send(EngineRequest::Fetch {
                        account: cfg,
                        folder: Folder::Inbox,
                    });
                    self.awaiting_engine = true;
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
        self.nav.sync(state, path);
    }

    /// Publish the open mailbox + its folders/messages so the host's Ctrl+K
    /// action palette scopes to them. Delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its mail action, so it records one undo boundary bracketing it (the same
    /// `record_committed_action` path a Ctrl+K dispatch takes). Rising-edge:
    /// cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.shared.borrow_mut().take_committed_action()
    }

    /// Fold the mailbox into the unified Ctrl+K palette: a snapshot
    /// [`MailSearchProvider`](crate::search_provider::MailSearchProvider) makes
    /// every already-loaded message findable there by subject / sender / body,
    /// alongside the actions the app exports. The provider never touches the
    /// IMAP engine, so a Ctrl+K query stays synchronous.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(crate::search_provider::MailSearchProvider::new(
            &self.shared.borrow().mailbox,
        ))]
    }

    /// Open the message a chosen palette row identifies: switch to its folder,
    /// select it (which marks it read + shows the detail pane), then pump the
    /// resulting navigation intent. Selection is app-local presentation.
    fn activate_search_result(&mut self, action_id: &str, doc: &mut AutoCommit, _path: &ObjId) {
        let Some(id) = crate::search_provider::MailSearchProvider::parse_action_id(action_id)
        else {
            return;
        };
        let folder = self.shared.borrow().mailbox.get(id).map(|m| m.folder);
        if let Some(folder) = folder {
            {
                let mut st = self.shared.borrow_mut();
                st.select_folder(doc, folder);
                st.select_message(doc, id);
            }
            self.process_intents();
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: TSize {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        self.layout_nav();
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
        let c = &theme.tokens.colors;
        fill_rect(scene, t, self.rect, c.surfaces.base);
        // The navigation shell (sidebar + list + reader) draws itself, its
        // dividers, the collapse/back affordance, and the drawer scrim.
        self.nav
            .draw_with_presence(scene, t, tr, theme, presence, permissions);
        // First-run onboarding covers the (empty) mailbox until an account is
        // added.
        self.draw_first_run(scene, t, tr, theme);
        // App-owned overlays on top.
        self.compose.draw(scene, t, tr, theme, presence);
        self.compose.draw_inputs(scene, t, tr, theme, presence);
        self.draw_setup(scene, t, tr, theme, presence);
        self.draw_context_menu(scene, t, tr, theme);
        self.draw_status(scene, t, tr, theme);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            let p = Point::new(position.x / scale_factor, position.y / scale_factor);
            self.shared.borrow_mut().last_mouse = p;
            self.dirty = true;
        }

        // 1) Compose window is modal while open.
        if self.compose.open {
            let (consumed, action) = self.compose.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                node_id,
                taffy,
            );
            match action {
                ComposeAction::Send => self.send_compose(state),
                ComposeAction::SaveDraft => self.save_compose_draft(state),
                ComposeAction::Close => {
                    self.persist_compose_format(state);
                    self.dirty = true;
                }
                ComposeAction::None => {}
            }
            if consumed {
                *handled = true;
                self.dirty = true;
                return;
            }
        }

        // 1.5) First-run onboarding is modal until an account exists: the only
        // action is "Add account", which opens the setup sheet.
        if self.is_first_run()
            && let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } = event
        {
            let pt = self.shared.borrow().last_mouse;
            self.rebuild_overlay_hits();
            if self
                .overlay_hits
                .iter()
                .any(|(rc, h)| matches!(h, OverlayHit::FirstRunAdd) && rc.contains(pt))
            {
                self.open_setup_edit(None);
            }
            *handled = true;
            self.dirty = true;
            return;
        }

        // 2) Esc dismisses app-owned overlays first.
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(NamedKey::Escape),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            if self.context_menu.is_some() {
                self.context_menu = None;
                *handled = true;
                return;
            }
            if self.setup_open {
                self.close_setup();
                *handled = true;
                return;
            }
        }

        // 3) Context menu is modal: a click hits an item or dismisses.
        if self.context_menu.is_some()
            && let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } = event
        {
            let pt = self.shared.borrow().last_mouse;
            self.rebuild_overlay_hits();
            let item = self
                .overlay_hits
                .iter()
                .rev()
                .find(|(rc, h)| matches!(h, OverlayHit::CtxItem(_)) && rc.contains(pt))
                .cloned();
            if let Some((_, OverlayHit::CtxItem(i))) = item {
                if let Some(menu) = self.context_menu.take()
                    && let Some(action) = menu.items.get(i).copied()
                {
                    self.shared
                        .borrow_mut()
                        .apply_message_action(state, menu.target, action);
                    self.process_intents();
                }
            } else {
                self.context_menu = None;
            }
            *handled = true;
            return;
        }

        // 4) Setup sheet is modal while open.
        if self.setup_open {
            self.handle_setup_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            );
            if *handled {
                self.dirty = true;
                return;
            }
        }

        // 5) Route to the navigation shell (affordance/scrim/Esc + the panes).
        self.nav.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
        );
        // Panes raised intents; apply the cross-pane effects.
        self.process_intents();

        // 6) If nothing consumed it, apply global mailbox shortcuts.
        if !*handled {
            self.global_shortcuts(event, state, handled);
        }

        self.dirty = true;
    }

    fn name(&self) -> &'static str {
        "MailApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn modal_capture(&self) -> Option<Rect> {
        if self.compose.open {
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
            || self.setup_inputs.iter().any(ui::Widget::needs_redraw)
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
        for i in &mut self.setup_inputs {
            i.mark_drawn();
        }
    }
}

impl MailApp {
    #[allow(clippy::too_many_arguments)]
    fn handle_setup_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
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
                    let dir = if modifiers.state().shift_key() { -1 } else { 1 };
                    self.focus_setup_step(dir);
                    *handled = true;
                    return;
                }
                Key::Named(NamedKey::Enter) => {
                    let order = self.setup_tab_order();
                    let is_last = self.setup_focus.is_some_and(|f| order.last() == Some(&f));
                    if is_last {
                        self.save_setup(state);
                    } else {
                        self.focus_setup_step(1);
                    }
                    *handled = true;
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
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    node_id,
                    taffy,
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
            self.rebuild_overlay_hits();
            let panel = self.overlay_panel_rect();
            let hit = self
                .overlay_hits
                .iter()
                .rev()
                .find(|(rc, _)| rc.contains(pt))
                .map(|(_, h)| h.clone());
            match hit {
                Some(OverlayHit::SetupField(i)) => {
                    let leaving_email = self.setup_focus == Some(1) && i != 1;
                    self.focus_setup(Some(i));
                    if leaving_email {
                        self.maybe_autofill();
                    }
                    *handled = true;
                }
                Some(OverlayHit::AuthPassword) => {
                    self.setup_auth = crate::model::AuthMethod::Password;
                    self.dirty = true;
                    *handled = true;
                }
                Some(OverlayHit::AuthOAuth) => {
                    self.setup_auth = crate::model::AuthMethod::OAuth;
                    self.dirty = true;
                    *handled = true;
                }
                Some(OverlayHit::OAuthSignIn) => {
                    self.start_oauth();
                    *handled = true;
                }
                Some(OverlayHit::ToggleImapTls) => {
                    self.setup_imap_tls = !self.setup_imap_tls;
                    self.setup_inputs[5].set_text(if self.setup_imap_tls { "993" } else { "143" });
                    *handled = true;
                }
                Some(OverlayHit::ToggleSmtpTls) => {
                    self.setup_smtp_tls = !self.setup_smtp_tls;
                    self.setup_inputs[7].set_text(if self.setup_smtp_tls { "465" } else { "587" });
                    *handled = true;
                }
                Some(OverlayHit::SetupSave) => {
                    self.save_setup(state);
                    *handled = true;
                }
                Some(OverlayHit::RemoveAccount) => {
                    self.remove_editing_account(state);
                    *handled = true;
                }
                Some(OverlayHit::CancelSetup) => {
                    self.close_setup();
                    *handled = true;
                }
                _ => {
                    // Click outside the panel cancels.
                    if !panel.contains(pt) {
                        self.close_setup();
                    }
                    *handled = true;
                }
            }
        }
    }

    fn global_shortcuts(
        &mut self,
        event: &WindowEvent,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        let search_focused = self.shared.borrow().search_focused;
        if search_focused {
            return;
        }
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
        let selected = self.shared.borrow().selected;
        match logical_key {
            Key::Character(ch) if ch.eq_ignore_ascii_case("c") => {
                self.compose.open_blank();
                *handled = true;
            }
            Key::Character(ch) if ch == "/" => {
                self.shared.borrow_mut().focus_search_request = true;
                self.dirty = true;
                *handled = true;
            }
            Key::Character(ch) if ch.eq_ignore_ascii_case("r") => {
                // Manual refresh: fetch the current folder from the server now.
                self.refresh_active_folder();
                *handled = true;
            }
            Key::Named(NamedKey::F5) => {
                self.refresh_active_folder();
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
            Key::Character(ch) if ch.eq_ignore_ascii_case("e") => {
                if let Some(id) = selected {
                    self.shared
                        .borrow_mut()
                        .apply_message_action(state, id, CtxAction::Archive);
                    self.process_intents();
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Delete | NamedKey::Backspace) => {
                if let Some(id) = selected {
                    self.shared
                        .borrow_mut()
                        .apply_message_action(state, id, CtxAction::Delete);
                    self.process_intents();
                    *handled = true;
                }
            }
            Key::Character(ch) if ch.eq_ignore_ascii_case("s") => {
                if let Some(id) = selected {
                    self.shared
                        .borrow_mut()
                        .apply_message_action(state, id, CtxAction::ToggleStar);
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

    fn dispatch(app: &mut MailApp, ev: &WindowEvent, doc: &mut AutoCommit) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            1.0,
            &Modifiers::default(),
            doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        handled
    }

    fn app() -> (MailApp, AutoCommit) {
        let mut app = MailApp::new_sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        (app, doc)
    }

    fn click_at(app: &mut MailApp, doc: &mut AutoCommit, p: Point) -> bool {
        dispatch(app, &cursor_at(p), doc);
        dispatch(app, &left_press(), doc)
    }

    #[test]
    fn starts_in_mailbox_with_a_selection() {
        let (app, _) = app();
        assert!(app.shared.borrow().selected.is_some());
        assert_eq!(app.shared.borrow().folder, Folder::Inbox);
    }

    #[test]
    fn wide_shows_three_panes_abreast() {
        let (mut app, _doc) = app();
        app.layout_for_test(1280.0, 800.0);
        assert!(
            app.nav.sidebar_rect().width() > 1.0,
            "docked sidebar present"
        );
        assert!(
            app.nav.overview_rect().x0 >= app.nav.sidebar_rect().x1 - 0.5,
            "list right of sidebar"
        );
        assert!(
            app.nav.detail_rect().x0 >= app.nav.overview_rect().x1 - 0.5,
            "reader right of list"
        );
    }

    /// The confirmed-defect verification: at a narrow width, opening a message
    /// pushes the detail pane; Back returns to the list.
    #[test]
    fn narrow_open_message_pushes_detail_then_back() {
        let (mut app, mut doc) = app();
        app.layout_for_test(560.0, 860.0);
        assert_eq!(app.nav_pane(), NavPane::Overview, "starts on the list");

        // Click the first list row (a message).
        let row_center = {
            let st = app.shared.borrow();
            let first = st.list_rows()[0].0;
            let _ = first;
            // First row sits just below the list header — measured from
            // where the shell actually puts the content, not from the pane
            // rect. In the narrow layout the sidebar chip floats over the
            // pane's top-left and the shell reserves a band for it, so the
            // content starts lower than the pane does; asking
            // `overview_rect()` here aimed the click into that band and hit
            // nothing.
            let content = app.nav.overview_content_rect();
            Point::new(
                content.x0 + 120.0,
                content.y0 + crate::draw::HEADER_H + crate::draw::LIST_ROW_H / 2.0,
            )
        };
        assert!(
            click_at(&mut app, &mut doc, row_center),
            "row click consumed"
        );
        app.layout_for_test(560.0, 860.0);
        assert_eq!(
            app.nav_pane(),
            NavPane::Detail,
            "opening a message pushes the detail"
        );
        assert!(app.shared.borrow().selected.is_some());

        // The Back affordance is present; clicking it returns to the overview.
        let back = app.nav.back_button_rect();
        assert!(
            back.width() > 0.0,
            "Back affordance shown while detail pushed"
        );
        assert!(
            click_at(&mut app, &mut doc, back.center()),
            "back click consumed"
        );
        assert_eq!(
            app.nav_pane(),
            NavPane::Overview,
            "Back returns to the list"
        );
    }

    #[test]
    fn narrow_then_wide_reflows_to_three_panes() {
        // Same instance, resized wide→narrow→wide must re-derive the layout.
        let (mut app, _doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let wide_detail_x0 = app.nav.detail_rect().x0;
        app.layout_for_test(560.0, 860.0);
        assert!(app.shared.borrow().narrow, "narrow flag set at 560");
        // Overview is full-width in narrow.
        assert!(
            (app.nav.overview_rect().width() - 560.0).abs() < 2.0,
            "list full-width narrow"
        );
        app.layout_for_test(1280.0, 800.0);
        assert!(!app.shared.borrow().narrow, "wide flag re-derived at 1280");
        assert!(
            (app.nav.detail_rect().x0 - wide_detail_x0).abs() < 2.0,
            "reflows back to three panes"
        );
    }

    #[test]
    fn wide_sidebar_folder_click_switches_folder() {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        // The Sent row of the active account. Resolved from the sidebar's own
        // laid-out rect rather than a screen literal: the shell reserves a band
        // at the top of the sidebar for the collapse chip, so the rows sit at an
        // offset the test should not have to restate.
        let sidebar = app.nav.sidebar_content_rect();
        let p = Point::new(sidebar.x0 + 130.0, sidebar.y0 + 250.0);
        assert!(click_at(&mut app, &mut doc, p), "folder click consumed");
        assert_eq!(
            app.shared.borrow().folder,
            Folder::Sent,
            "clicking Sent switches the folder"
        );
    }

    #[test]
    fn narrow_hamburger_opens_and_esc_closes_drawer() {
        let (mut app, mut doc) = app();
        app.layout_for_test(560.0, 860.0);
        let ham = app.nav.collapse_button_rect();
        assert!(ham.width() > 0.0, "hamburger present on the narrow list");
        assert!(
            click_at(&mut app, &mut doc, ham.center()),
            "hamburger click consumed"
        );
        assert!(app.nav.is_sidebar_open(), "hamburger opens the drawer");
        // Esc dismisses the open overlay drawer.
        assert!(dispatch(&mut app, &esc_event(), &mut doc), "Esc consumed");
        assert!(!app.nav.is_sidebar_open(), "Esc closes the drawer");
    }

    #[test]
    fn selecting_message_marks_read_and_persists() {
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

    #[test]
    fn compose_send_lands_in_sent() {
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
        assert!(!app.compose.open);
        assert_eq!(app.shared.borrow().mailbox.count(Folder::Sent), before + 1);
    }

    #[test]
    fn context_and_swipe_actions_are_mirrored() {
        let (app, _doc) = app();
        let id = app.shared.borrow().mailbox.messages_in(Folder::Inbox)[0].id;
        let menu = app.shared.borrow().context_items_for(id);
        assert!(
            menu.contains(&CtxAction::Archive),
            "swipe-left action present in menu"
        );
        assert!(
            menu.contains(&CtxAction::Delete),
            "swipe-right action present in menu"
        );
    }

    #[test]
    fn setup_save_adds_account() {
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

    #[test]
    fn sample_mode_persists_to_a_sandbox_not_the_real_store() {
        // Regression guard for the leak where the bundled demo accounts
        // (you@qos.example, you@hooli.example, new@host.example) ended up in the
        // user's real ~/.config/qos/accounts/store.json: the sample / shots /
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

    #[test]
    fn sample_mode_save_never_writes_the_real_account_store() {
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

    #[test]
    fn live_mode_persists_to_the_real_config_root() {
        // The live app is the ONLY constructor allowed to write the real store.
        let app = MailApp::new_live();
        assert_eq!(
            app.accounts_persist_root(),
            accounts::config_root(),
            "the live app must persist to the real per-user config root"
        );
    }

    #[test]
    fn account_settings_sheet_is_scoped_to_the_selected_account() {
        let (mut app, _doc) = app(); // sample store: 2 accounts, active = 0
        assert_eq!(app.shared.borrow().accounts.active, 0);
        // Opening settings for account #1 (you@hooli.example) — reached from that
        // account's gear — must populate the form from THAT account, not the
        // active one, so the sheet honestly reads as its settings.
        app.open_setup_edit(Some(1));
        assert_eq!(app.setup_editing, Some(1));
        assert_eq!(
            app.read_setup_config().email,
            "you@hooli.example",
            "the account-settings form is scoped to the clicked account"
        );
    }

    #[test]
    fn live_starts_without_fabricated_messages() {
        // The live constructor must not seed a sample corpus.
        let app = MailApp::new_live();
        assert!(
            app.shared.borrow().mailbox.messages.is_empty(),
            "no fabricated startup mail"
        );
    }

    #[test]
    fn reply_all_seeds_sender_in_to_and_others_in_cc() {
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
        let d = app.compose.to_draft();
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
            !d.cc.contains("you@qos.example"),
            "our own address is excluded from Cc: {}",
            d.cc
        );
        assert!(
            !d.cc.contains(&sender),
            "the sender is not duplicated into Cc: {}",
            d.cc
        );
    }

    #[test]
    fn save_draft_lands_in_drafts_and_reopens() {
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
        assert_eq!(app.compose.to_draft().subject, "WIP proposal");
        assert_eq!(app.editing_draft, Some(id));
    }

    #[test]
    fn saving_edited_draft_updates_in_place_not_duplicates() {
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

    #[test]
    fn session_state_restores_from_persisted_document() {
        let mut doc = AutoCommit::new();
        {
            let mut app = MailApp::new_sample();
            app.sync(&mut doc, &automerge::ROOT); // seeds the document
            app.shared
                .borrow_mut()
                .select_folder(&mut doc, Folder::Sent);
            app.shared.borrow_mut().set_active_account(&mut doc, 1);
            app.compose.set_format(ComposeFormat::Text);
            app.persist_compose_format(&mut doc);
        }
        // A second app opening the same document must restore, not overwrite.
        let mut app2 = MailApp::new_sample();
        app2.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app2.shared.borrow().folder, Folder::Sent, "folder restored");
        assert_eq!(
            app2.shared.borrow().accounts.active,
            1,
            "active account restored"
        );
        assert_eq!(
            app2.compose.format(),
            ComposeFormat::Text,
            "compose format restored"
        );
    }

    #[test]
    fn setup_email_autofills_connection_hosts() {
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

    #[test]
    fn context_menu_click_applies_the_action() {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let id = app.shared.borrow().mailbox.messages_in(Folder::Inbox)[0].id;
        app.shared
            .borrow_mut()
            .push_intent(Intent::ContextMenu(id, Point::new(400.0, 300.0)));
        app.process_intents();
        assert!(app.context_menu.is_some(), "menu opened");
        app.rebuild_overlay_hits();
        let items = app.context_menu.as_ref().unwrap().items.clone();
        let idx = items.iter().position(|a| *a == CtxAction::Archive).unwrap();
        let rect = app
            .overlay_hits
            .iter()
            .find_map(|(r, h)| match h {
                OverlayHit::CtxItem(i) if *i == idx => Some(*r),
                _ => None,
            })
            .expect("archive item rect");
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
        assert!(
            app.context_menu.is_none(),
            "menu dismissed after the action"
        );
    }

    #[test]
    fn setup_tls_toggle_flips_the_port() {
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

    #[test]
    fn shortcut_c_opens_compose() {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        assert!(!app.compose.open);
        assert!(
            dispatch(&mut app, &char_key("c"), &mut doc),
            "c shortcut consumed"
        );
        assert!(app.compose.open, "'c' opened the composer");
    }

    #[test]
    fn shortcut_e_archives_selected() {
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
    }

    #[test]
    fn shortcut_arrow_down_moves_selection() {
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

    #[test]
    fn shortcut_delete_trashes_selected() {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let before = app.shared.borrow().mailbox.count(Folder::Trash);
        dispatch(&mut app, &named_key(NamedKey::Delete), &mut doc);
        assert_eq!(
            app.shared.borrow().mailbox.count(Folder::Trash),
            before + 1,
            "Delete moved the selection to Trash"
        );
    }

    // ---- first-run onboarding --------------------------------------------

    #[test]
    fn first_run_shows_onboarding_and_add_opens_setup() {
        let mut app = MailApp::new_first_run();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        app.layout_for_test(1100.0, 780.0);
        assert!(
            app.is_first_run(),
            "no configured account → first-run onboarding"
        );
        app.rebuild_overlay_hits();
        let add = app
            .overlay_hits
            .iter()
            .find_map(|(r, h)| matches!(h, OverlayHit::FirstRunAdd).then_some(*r))
            .expect("the onboarding Add-account button");
        assert!(
            click_at(&mut app, &mut doc, add.center()),
            "Add-account click consumed"
        );
        assert!(app.setup_open, "clicking Add opens the account setup sheet");
        assert!(
            !app.is_first_run(),
            "an open setup sheet supersedes the first-run screen"
        );
    }

    // ---- add-account: single-border layout, Tab, autoconfig, OAuth --------

    #[test]
    fn setup_fields_use_single_border_no_wrapper() {
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

    #[test]
    fn setup_tab_key_advances_between_fields() {
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

    #[test]
    fn oauth_mode_drops_password_from_tab_order() {
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

    // The Enter-key save runs through the key handler, which uses the build's
    // default probe. On the offline (default) build that is the offline probe,
    // so the account persists; the live build performs a real IMAP/SMTP probe
    // that this test provides no server for (that path is covered by the
    // `validate` state-machine tests and the live socket tests).
    #[cfg(not(feature = "live"))]
    #[test]
    fn setup_enter_on_last_field_submits() {
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

    #[test]
    fn gmail_email_presets_oauth_and_still_autofills_hosts() {
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

    #[test]
    fn setup_tab_traverses_every_field_in_order() {
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

    #[test]
    fn autofill_uses_the_builtin_table_across_providers() {
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

    #[test]
    fn typing_the_email_fills_servers_without_leaving_the_field() {
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

    #[test]
    fn status_toast_auto_dismisses_after_its_lifetime() {
        // A "Synced N messages" toast must fade on its own, not linger forever.
        let (mut app, _doc) = app();
        let t0 = std::time::Instant::now();
        app.shared.borrow_mut().status = Some("Synced 3 messages".into());
        app.expire_toast_at(t0); // first observation arms the deadline
        assert!(
            app.shared.borrow().status.is_some(),
            "toast shows immediately"
        );
        assert!(app.toast_pending(), "a dismiss deadline is armed");
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
    }

    // ---- test-before-save state machine (persist on success only) --------

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

    #[test]
    fn save_persists_only_when_verification_succeeds() {
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
}
