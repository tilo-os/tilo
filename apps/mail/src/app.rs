//! `MailApp` — the root widget. Owns the in-memory [`Mailbox`], the multi-
//! account [`AccountStore`], the floating [`ComposeWindow`], and the UI state,
//! and renders a **responsive** mail client:
//!
//! * a **collapsible account/folder sidebar** (Adwaita `OverlaySplitView` style:
//!   persistent and inline when the window is wide; collapsed and overlaying the
//!   content behind a scrim when narrow, closed by default at small sizes);
//! * a **message list** + **reader** shown side-by-side when wide, or as a
//!   single navigation-push pane (list → mail, with a back affordance) when
//!   narrow;
//! * a **right-click context menu** and **swipe-left / swipe-right** row
//!   actions whose action sets mirror each other;
//! * a movable/resizable/detachable [`ComposeWindow`] for writing mail.
//!
//! Like the rest of this app it draws most chrome straight into the Vello scene
//! (data-dense rows are easier to control that way) and embeds `ui::TextInput`s
//! for text entry. Mutable state that must persist / sync / undo (per-message
//! flags, the selected folder / view mode / search / selection, the account
//! config, the compose-format preference) is mirrored into the Automerge doc via
//! [`crate::doc::MailDoc`].
//!
//! ## On OS-window detach
//! The compose window can be popped *out* to sit beside the app content
//! ([`crate::compose::DockState::Detached`]) and back *in*. A real second
//! top-level OS surface needs the qOS compositor's window-group / multi-window
//! support (`shell`), which isn't available through the single-window `ui::App`
//! on a dev host — so the pop-out is fully functional within the app canvas, and
//! `ComposeWindow::detach_request` is the seam to hand the draft to the
//! compositor when running under qOS.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::color_compat::ColorCompat;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::TextInput;

use crate::account::{Account, AccountStore};
use crate::compose::{ComposeAction, ComposeWindow};
use crate::doc::MailDoc;
use crate::model::{AccountConfig, Addr, ComposeFormat, Folder, Mailbox, Message, MessageId};
use crate::util::{format_absolute, format_relative};

const TOPBAR_H: f64 = 56.0;
const SIDEBAR_W: f64 = 248.0;
const LIST_W_MIN: f64 = 360.0;
const FOLDER_ROW_H: f64 = 36.0;
const ACCOUNT_HEADER_H: f64 = 44.0;
const LIST_ROW_H: f64 = 78.0;

/// The width below which the client switches to its narrow single-pane,
/// overlay-sidebar layout. Above it, list + reader sit side-by-side and the
/// sidebar is persistent.
const WIDE_BREAKPOINT: f64 = 980.0;
/// Swipe distance (px) past which a horizontal drag on a row triggers its
/// swipe action.
const SWIPE_THRESHOLD: f64 = 72.0;

/// In the narrow layout the content area is a two-page navigation stack.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NarrowPage {
    List,
    Mail,
}

/// How the sidebar is presented at the current width.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SidebarMode {
    /// Inline, always visible, pushes the content (wide windows).
    Persistent,
    /// Floats over the content behind a scrim; toggled open (narrow windows).
    Overlay,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Mailbox,
    Setup,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    None,
    Search,
    Setup(usize),
}

/// Identifies an actionable region resolved during layout.
#[derive(Clone)]
enum Hit {
    Account(usize),
    AccountToggle(usize),
    Folder(usize, Folder),
    Thread(MessageId),
    ThreadStar(MessageId),
    Compose,
    Settings,
    SidebarToggle,
    ScrimDismiss,
    BackToList,
    SearchClear,
    Reply,
    ArchiveSel,
    DeleteSel,
    StarSel,
    UnreadSel,
    // context menu
    CtxItem(usize),
    // setup overlay
    SetupField(usize),
    ToggleImapTls,
    ToggleSmtpTls,
    SetupSave,
    CancelOverlay,
}

/// One entry in the right-click context menu. The action set here is the
/// single source of truth that the swipe gestures mirror.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CtxAction {
    Reply,
    ReplyAll,
    Forward,
    ToggleRead,
    ToggleStar,
    Archive,
    Delete,
    MoveToArchive,
    MoveToInbox,
    Junk,
}

impl CtxAction {
    fn label(self, read: bool, starred: bool) -> String {
        match self {
            CtxAction::Reply => "Reply".into(),
            CtxAction::ReplyAll => "Reply all".into(),
            CtxAction::Forward => "Forward".into(),
            CtxAction::ToggleRead => if read { "Mark unread".into() } else { "Mark read".into() },
            CtxAction::ToggleStar => if starred { "Remove flag".into() } else { "Flag / star".into() },
            CtxAction::Archive => "Archive".into(),
            CtxAction::Delete => "Delete".into(),
            CtxAction::MoveToArchive => "Move to Archive".into(),
            CtxAction::MoveToInbox => "Move to Inbox".into(),
            CtxAction::Junk => "Mark as junk".into(),
        }
    }
}

/// State of an in-progress horizontal swipe on a list row.
#[derive(Clone, Copy)]
struct SwipeState {
    id: MessageId,
    start_x: f64,
    dx: f64,
}

/// An open context menu anchored at a point, targeting a message.
struct ContextMenu {
    target: MessageId,
    pos: Point,
    items: Vec<CtxAction>,
}

pub struct MailApp {
    mailbox: Mailbox,
    accounts: AccountStore,
    now_min: i64,
    mode: Mode,
    folder: Folder,
    selected: Option<MessageId>,
    focus: Focus,
    search: String,
    seeded: bool,

    // responsive state
    sidebar_open: bool,
    narrow_page: NarrowPage,

    // compose window
    compose: ComposeWindow,

    // context menu + swipe
    context_menu: Option<ContextMenu>,
    swipe: Option<SwipeState>,
    mouse_down_row: Option<(MessageId, Point)>,

    // embedded inputs
    search_input: TextInput,

    // setup fields: name, email, username, password, imap_host, imap_port,
    // smtp_host, smtp_port
    setup_inputs: Vec<TextInput>,
    setup_imap_tls: bool,
    setup_smtp_tls: bool,
    /// Which account index the setup form edits (`None` = adding a new one).
    setup_editing: Option<usize>,

    status: Option<String>,

    // layout caches
    rect: Rect,
    sidebar_mode: SidebarMode,
    sidebar_rect: Rect,
    content_rect: Rect,
    list_rect: Rect,
    reader_rect: Rect,
    hits: Vec<(Rect, Hit)>,
    last_mouse: Point,
    dirty: bool,
}

impl MailApp {
    pub fn new_sample() -> Self {
        let mailbox = crate::sample::sample_mailbox();
        let accounts = crate::sample::sample_account_store();
        let now_min = crate::sample::SAMPLE_NOW_MIN;
        Self::with_data(mailbox, accounts, now_min)
    }

    fn with_data(mailbox: Mailbox, accounts: AccountStore, now_min: i64) -> Self {
        let mut search_input = TextInput::local();
        search_input.set_plain_chrome(true);
        let cfg = accounts.active_config();
        let setup_inputs = build_setup_inputs(&cfg);

        let mut app = Self {
            now_min,
            mode: Mode::Mailbox,
            folder: Folder::Inbox,
            selected: None,
            focus: Focus::None,
            search: String::new(),
            seeded: false,
            sidebar_open: false,
            narrow_page: NarrowPage::List,
            compose: ComposeWindow::new(ComposeFormat::Html),
            context_menu: None,
            swipe: None,
            mouse_down_row: None,
            search_input,
            setup_imap_tls: cfg.imap_tls,
            setup_smtp_tls: cfg.smtp_tls,
            setup_inputs,
            setup_editing: None,
            status: None,
            rect: Rect::ZERO,
            sidebar_mode: SidebarMode::Persistent,
            sidebar_rect: Rect::ZERO,
            content_rect: Rect::ZERO,
            list_rect: Rect::ZERO,
            reader_rect: Rect::ZERO,
            hits: Vec::new(),
            last_mouse: Point::ZERO,
            dirty: true,
            mailbox,
            accounts,
        };
        // Preselect a conversation so the reader is populated on launch / in the
        // headless screenshot.
        let threads = app.current_threads();
        let pick = threads.iter().find(|t| t.message_ids.len() > 1).or_else(|| threads.first());
        if let Some(&newest) = pick.and_then(|t| t.message_ids.last()) {
            app.selected = Some(newest);
            app.mailbox.mark_read(newest, true);
        }
        app
    }

    /// Open the compose window pre-filled with a representative HTML message,
    /// for the `--shots` screenshot. Demonstrates the WYSIWYG editor with mixed
    /// formatting (heading, bold/italic, a bullet list) and the toolbar.
    pub fn prime_compose_shot(&mut self) {
        self.compose.open_demo();
        self.dirty = true;
    }

    /// Show the narrow single-pane layout for the responsive screenshot: the
    /// message list as the sole content pane with the sidebar collapsed (its
    /// default at small sizes — it overlays the content with a scrim only when
    /// toggled open). The breakpoint itself is width-driven at layout time.
    pub fn prime_narrow_shot(&mut self) {
        self.sidebar_open = false;
        self.narrow_page = NarrowPage::List;
        self.dirty = true;
    }

    // ----- derived views -----

    fn current_threads(&self) -> Vec<crate::model::Thread> {
        self.mailbox.threads_in(self.folder)
    }

    fn list_rows(&self) -> Vec<(MessageId, usize)> {
        if !self.search.trim().is_empty() {
            self.mailbox
                .search(&self.search, None)
                .into_iter()
                .map(|m| (m.id, 1))
                .collect()
        } else {
            self.current_threads()
                .into_iter()
                .filter_map(|t| t.message_ids.last().copied().map(|id| (id, t.message_ids.len())))
                .collect()
        }
    }

    fn selected_thread_messages(&self) -> Vec<&Message> {
        let Some(sel) = self.selected.and_then(|id| self.mailbox.get(id)) else {
            return Vec::new();
        };
        let subj = sel.thread_subject();
        let mut v: Vec<&Message> = self
            .mailbox
            .messages
            .values()
            .filter(|m| m.thread_subject() == subj && m.folder == sel.folder)
            .collect();
        v.sort_by_key(|m| m.timestamp_min);
        v
    }

    // ----- doc write-through -----

    fn seed_doc(&mut self, state: &mut AutoCommit) {
        let ids: Vec<_> = self.mailbox.messages.keys().copied().collect();
        for id in ids {
            if let Some(m) = self.mailbox.get(id) {
                MailDoc::store_flags(state, &m.clone());
            }
        }
        MailDoc::store_account(state, &self.accounts.active_config());
        MailDoc::set_folder(state, self.folder);
        MailDoc::set_mode(state, "mailbox");
        MailDoc::set_compose_format(state, self.compose.format().as_key());
        MailDoc::set_active_account(state, self.accounts.active);
        if let Some(sel) = self.selected {
            MailDoc::set_selected(state, Some(&sel.to_string()));
        }
    }

    fn persist_flags(&self, state: &mut AutoCommit, id: MessageId) {
        if let Some(m) = self.mailbox.get(id) {
            MailDoc::store_flags(state, &m.clone());
        }
    }

    // ----- actions -----

    fn select_message(&mut self, state: &mut AutoCommit, id: MessageId) {
        self.selected = Some(id);
        self.mailbox.mark_read(id, true);
        self.persist_flags(state, id);
        MailDoc::set_selected(state, Some(&id.to_string()));
        if matches!(self.sidebar_mode, SidebarMode::Overlay) {
            // On narrow, opening a message pushes the Mail page and closes the
            // overlay sidebar.
            self.narrow_page = NarrowPage::Mail;
            self.sidebar_open = false;
        }
        self.dirty = true;
    }

    fn select_folder(&mut self, state: &mut AutoCommit, folder: Folder) {
        self.folder = folder;
        self.search.clear();
        self.search_input.set_text("");
        MailDoc::set_folder(state, folder);
        self.selected = self
            .current_threads()
            .first()
            .and_then(|t| t.message_ids.last().copied());
        if let Some(sel) = self.selected {
            MailDoc::set_selected(state, Some(&sel.to_string()));
        }
        // Selecting a folder returns the narrow nav to the list page; close the
        // overlay sidebar so the content is visible.
        self.narrow_page = NarrowPage::List;
        if matches!(self.sidebar_mode, SidebarMode::Overlay) {
            self.sidebar_open = false;
        }
        self.dirty = true;
    }

    fn open_compose_new(&mut self) {
        self.compose.open_blank();
        self.context_menu = None;
        self.dirty = true;
    }

    fn open_reply(&mut self, id: MessageId) {
        let Some(msg) = self.mailbox.get(id).cloned() else { return };
        let subj = if msg.subject.to_ascii_lowercase().starts_with("re:") {
            msg.subject.clone()
        } else {
            format!("Re: {}", msg.subject)
        };
        let quote = format!(
            "\n\nOn {}, {} wrote:\n> {}",
            format_absolute(msg.timestamp_min),
            msg.from.display(),
            msg.snippet()
        );
        self.compose.open_reply(&msg.from.rfc5322(), &subj, &quote);
        self.context_menu = None;
        self.dirty = true;
    }

    fn open_forward(&mut self, id: MessageId) {
        let Some(msg) = self.mailbox.get(id).cloned() else { return };
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

    fn send_compose(&mut self, state: &mut AutoCommit) {
        let draft = self.compose.to_draft();
        let acct = self.accounts.active_config();
        let from = Addr::new(acct.display_name.clone(), acct.email.clone());
        match crate::transport::build_mime(&from, &draft) {
            Ok(bytes) => {
                let to = crate::transport::parse_addr_list(&draft.to);
                let msg = Message {
                    id: uuid::Uuid::new_v4(),
                    rfc_message_id: None,
                    in_reply_to: None,
                    from,
                    to,
                    cc: crate::transport::parse_addr_list(&draft.cc),
                    subject: draft.subject.clone(),
                    body: draft.body.clone(),
                    attachments: draft
                        .attachments
                        .iter()
                        .map(|n| crate::model::Attachment {
                            filename: n.clone(),
                            mime: crate::transport::mime_from_filename(n).into(),
                            size: 0,
                            hydrated: true,
                        })
                        .collect(),
                    timestamp_min: self.now_min,
                    read: true,
                    starred: false,
                    folder: Folder::Sent,
                };
                let id = msg.id;
                self.mailbox.insert(msg);
                self.persist_flags(state, id);
                let kind = if draft.html_body.is_some() { "HTML" } else { "text" };
                self.status = Some(format!("Sent {kind} message ({} bytes)", bytes.len()));
            }
            Err(e) => self.status = Some(format!("Send failed: {e}")),
        }
        self.compose.close();
        self.persist_compose_format(state);
        self.dirty = true;
    }

    fn save_compose_draft(&mut self, state: &mut AutoCommit) {
        let draft = self.compose.to_draft();
        MailDoc::store_draft(state, &draft);
        self.status = Some("Draft saved".into());
        self.compose.close();
        self.persist_compose_format(state);
        self.dirty = true;
    }

    fn persist_compose_format(&self, state: &mut AutoCommit) {
        MailDoc::set_compose_format(state, self.compose.format().as_key());
    }

    // ----- setup overlay (in-app add/edit; Settings owns this in production) --

    fn open_setup_edit(&mut self, idx: Option<usize>) {
        self.setup_editing = idx;
        let cfg = match idx {
            Some(i) => self.accounts.accounts.get(i).map(|a| a.config.clone()).unwrap_or_default(),
            None => AccountConfig::default(),
        };
        self.setup_inputs = build_setup_inputs(&cfg);
        self.setup_imap_tls = cfg.imap_tls;
        self.setup_smtp_tls = cfg.smtp_tls;
        self.mode = Mode::Setup;
        self.focus_only(Focus::Setup(1));
        self.context_menu = None;
        self.dirty = true;
    }

    fn save_setup(&mut self, state: &mut AutoCommit) {
        let cfg = self.read_setup_config();
        if cfg.is_valid() {
            let mut account = match self.setup_editing.and_then(|i| self.accounts.accounts.get(i)) {
                Some(existing) => {
                    let mut a = existing.clone();
                    a.config = cfg.clone();
                    a
                }
                None => Account::new(cfg.clone()),
            };
            account.id = if cfg.email.is_empty() { account.id } else { cfg.email.clone() };
            self.accounts.upsert(account);
            MailDoc::store_account(state, &cfg);
            MailDoc::set_active_account(state, self.accounts.active);
            self.status = Some("Account saved".into());
            self.close_overlay(state);
        } else {
            self.status = Some("Fix the highlighted fields".into());
        }
        self.dirty = true;
    }

    fn read_setup_config(&self) -> AccountConfig {
        let g = |i: usize| self.setup_inputs[i].value().to_string();
        AccountConfig {
            display_name: g(0),
            email: g(1),
            username: g(2),
            password: g(3),
            imap_host: g(4),
            imap_port: g(5).trim().parse().unwrap_or(993),
            smtp_host: g(6),
            smtp_port: g(7).trim().parse().unwrap_or(465),
            imap_tls: self.setup_imap_tls,
            smtp_tls: self.setup_smtp_tls,
        }
    }

    fn close_overlay(&mut self, state: &mut AutoCommit) {
        self.mode = Mode::Mailbox;
        self.focus_only(Focus::None);
        MailDoc::set_mode(state, "mailbox");
        self.dirty = true;
    }

    fn focus_only(&mut self, focus: Focus) {
        self.focus = focus;
        self.search_input.set_focus(matches!(focus, Focus::Search));
        for (i, inp) in self.setup_inputs.iter_mut().enumerate() {
            inp.set_focus(matches!(focus, Focus::Setup(j) if j == i));
        }
        self.dirty = true;
    }

    fn focused_input(&mut self) -> Option<&mut TextInput> {
        match self.focus {
            Focus::Search => Some(&mut self.search_input),
            Focus::Setup(i) => self.setup_inputs.get_mut(i),
            _ => None,
        }
    }

    // ----- message mutations shared by context menu + swipe + keys -----

    fn apply_message_action(&mut self, state: &mut AutoCommit, id: MessageId, action: CtxAction) {
        match action {
            CtxAction::Reply | CtxAction::ReplyAll => self.open_reply(id),
            CtxAction::Forward => self.open_forward(id),
            CtxAction::ToggleRead => {
                let now = self.mailbox.get(id).map(|m| m.read).unwrap_or(false);
                self.mailbox.mark_read(id, !now);
                self.persist_flags(state, id);
            }
            CtxAction::ToggleStar => {
                self.mailbox.toggle_star(id);
                self.persist_flags(state, id);
            }
            CtxAction::Archive | CtxAction::MoveToArchive => {
                self.mailbox.archive(id);
                self.persist_flags(state, id);
                self.reselect_after_remove(state);
            }
            CtxAction::Delete => {
                self.mailbox.delete(id);
                self.persist_flags(state, id);
                self.reselect_after_remove(state);
            }
            CtxAction::MoveToInbox => {
                self.mailbox.move_to(id, Folder::Inbox);
                self.persist_flags(state, id);
                self.reselect_after_remove(state);
            }
            CtxAction::Junk => {
                self.mailbox.move_to(id, Folder::Junk);
                self.persist_flags(state, id);
                self.reselect_after_remove(state);
            }
        }
        self.dirty = true;
    }

    fn context_items_for(&self, id: MessageId) -> Vec<CtxAction> {
        let in_inbox = self.mailbox.get(id).map(|m| m.folder == Folder::Inbox).unwrap_or(true);
        let mut items = vec![
            CtxAction::Reply,
            CtxAction::ReplyAll,
            CtxAction::Forward,
            CtxAction::ToggleRead,
            CtxAction::ToggleStar,
            CtxAction::Archive,
            CtxAction::Delete,
        ];
        // Move-to options reflect the current folder.
        if in_inbox {
            items.push(CtxAction::MoveToArchive);
        } else {
            items.push(CtxAction::MoveToInbox);
        }
        items.push(CtxAction::Junk);
        items
    }

    fn open_context_menu(&mut self, id: MessageId, at: Point) {
        let items = self.context_items_for(id);
        self.context_menu = Some(ContextMenu { target: id, pos: at, items });
        self.dirty = true;
    }

    fn reselect_after_remove(&mut self, state: &mut AutoCommit) {
        self.selected = self
            .current_threads()
            .first()
            .and_then(|t| t.message_ids.last().copied());
        match self.selected {
            Some(sel) => MailDoc::set_selected(state, Some(&sel.to_string())),
            None => MailDoc::set_selected(state, None),
        }
        self.dirty = true;
    }
}

fn build_setup_inputs(cfg: &AccountConfig) -> Vec<TextInput> {
    vec![
        TextInput::local().with_initial_value(&cfg.display_name),
        TextInput::local().with_initial_value(&cfg.email),
        TextInput::local().with_initial_value(&cfg.username),
        TextInput::local().with_initial_value(&cfg.password).with_secret(true),
        TextInput::local().with_initial_value(&cfg.imap_host),
        TextInput::local().with_initial_value(&cfg.imap_port.to_string()),
        TextInput::local().with_initial_value(&cfg.smtp_host),
        TextInput::local().with_initial_value(&cfg.smtp_port.to_string()),
    ]
}

// =============================== layout ===============================

impl MailApp {
    fn relayout(&mut self) {
        self.hits.clear();
        let r = self.rect;
        let wide = r.width() >= WIDE_BREAKPOINT;
        self.sidebar_mode = if wide { SidebarMode::Persistent } else { SidebarMode::Overlay };
        // Persistent sidebar is effectively always "open"; overlay defaults closed.
        let sidebar_visible = match self.sidebar_mode {
            SidebarMode::Persistent => true,
            SidebarMode::Overlay => self.sidebar_open,
        };

        // Content area sits right of a persistent sidebar; full-width under an
        // overlay sidebar.
        let content_x0 = match self.sidebar_mode {
            SidebarMode::Persistent => r.x0 + SIDEBAR_W,
            SidebarMode::Overlay => r.x0,
        };
        self.content_rect = Rect::new(content_x0, r.y0, r.x1, r.y1);

        // Sidebar rect (overlay floats over content from the left edge).
        self.sidebar_rect = Rect::new(r.x0, r.y0, r.x0 + SIDEBAR_W, r.y1);

        // ----- topbar controls -----
        // Hamburger toggle (only meaningful for the overlay sidebar, but shown
        // in both so the user can hide a persistent sidebar too).
        let ham = Rect::new(self.content_rect.x0 + 12.0, r.y0 + 12.0, self.content_rect.x0 + 44.0, r.y0 + 44.0);
        self.hits.push((ham, Hit::SidebarToggle));
        // Settings gear (right).
        let gear = Rect::new(r.x1 - 44.0, r.y0 + 12.0, r.x1 - 12.0, r.y0 + 44.0);
        self.hits.push((gear, Hit::Settings));
        // Search box.
        let search_rect = self.search_box_rect();
        self.search_input.apply_layout_external(Rect::new(
            search_rect.x0 + 30.0,
            search_rect.y0,
            search_rect.x1 - 26.0,
            search_rect.y1,
        ));
        if !self.search.is_empty() {
            self.hits.push((
                Rect::new(search_rect.x1 - 26.0, search_rect.y0, search_rect.x1, search_rect.y1),
                Hit::SearchClear,
            ));
        }

        // ----- content panes -----
        let body_top = r.y0 + TOPBAR_H;
        if wide {
            // list + reader side by side
            let list_w = ((self.content_rect.width() - LIST_W_MIN) * 0.0 + 404.0)
                .min(self.content_rect.width() * 0.42)
                .max(LIST_W_MIN.min(self.content_rect.width()));
            let list_x1 = self.content_rect.x0 + list_w;
            self.list_rect = Rect::new(self.content_rect.x0, body_top, list_x1, r.y1);
            self.reader_rect = Rect::new(list_x1, body_top, r.x1, r.y1);
        } else {
            // single page: list OR mail
            match self.narrow_page {
                NarrowPage::List => {
                    self.list_rect = Rect::new(self.content_rect.x0, body_top, r.x1, r.y1);
                    self.reader_rect = Rect::ZERO;
                }
                NarrowPage::Mail => {
                    self.list_rect = Rect::ZERO;
                    self.reader_rect = Rect::new(self.content_rect.x0, body_top, r.x1, r.y1);
                    // Back affordance.
                    let back = Rect::new(self.content_rect.x0 + 12.0, r.y0 + 12.0, self.content_rect.x0 + 88.0, r.y0 + 44.0);
                    self.hits.push((back, Hit::BackToList));
                }
            }
        }

        // ----- sidebar hits (account headers + folder rows) -----
        if sidebar_visible {
            self.layout_sidebar();
            if matches!(self.sidebar_mode, SidebarMode::Overlay) {
                // Scrim covers the content so a click there dismisses the sidebar.
                self.hits.push((self.content_rect, Hit::ScrimDismiss));
            }
        }

        // ----- list rows -----
        if self.list_rect.area() > 0.0 {
            let rows = self.list_rows();
            let mut ly = self.list_rect.y0;
            for (id, _len) in rows {
                let row = Rect::new(self.list_rect.x0, ly, self.list_rect.x1, ly + LIST_ROW_H);
                if row.y1 <= self.list_rect.y1 + LIST_ROW_H {
                    let star = Rect::new(row.x1 - 36.0, row.y0 + 8.0, row.x1 - 12.0, row.y0 + 32.0);
                    self.hits.push((star, Hit::ThreadStar(id)));
                    self.hits.push((row, Hit::Thread(id)));
                }
                ly += LIST_ROW_H;
            }
        }

        // ----- reader action bar -----
        if self.reader_rect.area() > 0.0 && self.selected.is_some() {
            let bar_y = self.reader_rect.y0 + 12.0;
            let mut bx = self.reader_rect.x0 + 20.0;
            for (w, hit) in [
                (78.0, Hit::Reply),
                (92.0, Hit::ArchiveSel),
                (82.0, Hit::DeleteSel),
                (70.0, Hit::StarSel),
                (110.0, Hit::UnreadSel),
            ] {
                let b = Rect::new(bx, bar_y, bx + w, bar_y + 32.0);
                self.hits.push((b, hit));
                bx += w + 8.0;
            }
        }

        // ----- context menu hits (topmost) -----
        if let Some(menu) = &self.context_menu {
            let mr = context_menu_rect(menu, r);
            let mut y = mr.y0 + 6.0;
            for (i, _a) in menu.items.iter().enumerate() {
                let item = Rect::new(mr.x0, y, mr.x1, y + 28.0);
                self.hits.push((item, Hit::CtxItem(i)));
                y += 28.0;
            }
        }

        // ----- setup overlay -----
        if self.mode == Mode::Setup {
            self.layout_setup();
        }

        // ----- compose window lays itself out within the content area -----
        let compose_bounds = if matches!(self.compose.dock(), crate::compose::DockState::Detached) {
            // Detached: park beside the content using the full app rect so it
            // can sit on the right edge like a tiled window.
            self.rect
        } else {
            self.content_rect
        };
        self.compose.layout(compose_bounds);
    }

    fn search_box_rect(&self) -> Rect {
        let r = self.rect;
        let x1 = r.x1 - 56.0;
        let x0 = (x1 - 280.0).max(self.content_rect.x0 + 56.0);
        Rect::new(x0, r.y0 + 12.0, x1, r.y0 + 44.0)
    }

    fn layout_sidebar(&mut self) {
        let sb = self.sidebar_rect;
        let pad = 12.0;
        // Compose button at top.
        let mut y = sb.y0 + 16.0;
        self.hits.push((
            Rect::new(sb.x0 + pad, y, sb.x0 + SIDEBAR_W - pad, y + 40.0),
            Hit::Compose,
        ));
        y += 40.0 + 14.0;

        // Each account: header (toggle collapse) then its folders when expanded.
        for (ai, account) in self.accounts.accounts.iter().enumerate() {
            let header = Rect::new(sb.x0 + 8.0, y, sb.x0 + SIDEBAR_W - 8.0, y + ACCOUNT_HEADER_H);
            self.hits.push((header, Hit::Account(ai)));
            // Collapse chevron hit zone (left).
            let chev = Rect::new(header.x0, header.y0, header.x0 + 28.0, header.y1);
            self.hits.push((chev, Hit::AccountToggle(ai)));
            y += ACCOUNT_HEADER_H;
            if !account.collapsed {
                for f in AccountStore::folders_for(account) {
                    let row = Rect::new(sb.x0 + 16.0, y, sb.x0 + SIDEBAR_W - 8.0, y + FOLDER_ROW_H);
                    self.hits.push((row, Hit::Folder(ai, f)));
                    y += FOLDER_ROW_H;
                }
            }
            y += 8.0;
        }
    }

    fn layout_setup(&mut self) {
        let p = self.overlay_panel_rect();
        let pad = 24.0;
        let lx = p.x0 + pad;
        let rx = p.x1 - pad;
        let field_h = 34.0;
        let label_w = 132.0;
        let mut y = p.y0 + 64.0;
        for i in 0..4 {
            let fr = Rect::new(lx + label_w, y, rx, y + field_h);
            self.setup_inputs[i].apply_layout_external(fr);
            self.hits.push((fr, Hit::SetupField(i)));
            y += field_h + 10.0;
        }
        y += 6.0;
        let half = (rx - lx - label_w - 12.0) * 0.62;
        let imap_host = Rect::new(lx + label_w, y, lx + label_w + half, y + field_h);
        self.setup_inputs[4].apply_layout_external(imap_host);
        self.hits.push((imap_host, Hit::SetupField(4)));
        let port_imap = Rect::new(imap_host.x1 + 12.0, y, imap_host.x1 + 12.0 + 64.0, y + field_h);
        self.setup_inputs[5].apply_layout_external(port_imap);
        self.hits.push((port_imap, Hit::SetupField(5)));
        let imap_tls = Rect::new(port_imap.x1 + 12.0, y, rx, y + field_h);
        self.hits.push((imap_tls, Hit::ToggleImapTls));
        y += field_h + 10.0;
        let smtp_host = Rect::new(lx + label_w, y, lx + label_w + half, y + field_h);
        self.setup_inputs[6].apply_layout_external(smtp_host);
        self.hits.push((smtp_host, Hit::SetupField(6)));
        let port_smtp = Rect::new(smtp_host.x1 + 12.0, y, smtp_host.x1 + 12.0 + 64.0, y + field_h);
        self.setup_inputs[7].apply_layout_external(port_smtp);
        self.hits.push((port_smtp, Hit::SetupField(7)));
        let smtp_tls = Rect::new(port_smtp.x1 + 12.0, y, rx, y + field_h);
        self.hits.push((smtp_tls, Hit::ToggleSmtpTls));
        y += field_h + 10.0;
        let _ = y;
        let by = p.y1 - 52.0;
        self.hits.push((Rect::new(lx, by, lx + 150.0, by + 36.0), Hit::SetupSave));
        self.hits.push((Rect::new(rx - 92.0, by, rx, by + 36.0), Hit::CancelOverlay));
    }

    fn overlay_panel_rect(&self) -> Rect {
        let r = self.rect;
        let w = 640.0_f64.min(r.width() - 80.0);
        let h = 540.0_f64.min(r.height() - 60.0);
        let cx = (r.x0 + r.x1) / 2.0;
        let y0 = r.y0 + 56.0;
        Rect::new(cx - w / 2.0, y0, cx + w / 2.0, y0 + h)
    }
}

fn context_menu_rect(menu: &ContextMenu, bounds: Rect) -> Rect {
    let w = 210.0;
    let h = 6.0 + menu.items.len() as f64 * 28.0 + 6.0;
    let x0 = menu.pos.x.min(bounds.x1 - w - 8.0).max(bounds.x0 + 4.0);
    let y0 = menu.pos.y.min(bounds.y1 - h - 8.0).max(bounds.y0 + 4.0);
    Rect::new(x0, y0, x0 + w, y0 + h)
}

// =============================== drawing ===============================

fn fill_rect(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &rect);
}
fn fill_round(scene: &mut Scene, t: Affine, rect: Rect, r: f64, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &RoundedRect::from_rect(rect, r));
}
fn stroke_round(scene: &mut Scene, t: Affine, rect: Rect, r: f64, w: f64, color: Color) {
    scene.stroke(&Stroke::new(w), t, color, None, &RoundedRect::from_rect(rect, r));
}

fn elide(tr: &mut TextRenderer, text: &str, size: f32, weight: TextWeight, max_w: f64) -> String {
    if max_w <= 1.0 {
        return String::new();
    }
    let (w, _) = tr.measure_weighted(text, size, None, weight);
    if (w as f64) <= max_w {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let cand = format!("{}…", chars.iter().collect::<String>().trim_end());
        let (cw, _) = tr.measure_weighted(&cand, size, None, weight);
        if (cw as f64) <= max_w {
            return cand;
        }
    }
    "…".to_string()
}

fn avatar_color(theme: &Theme, email: &str, tint: u8) -> Color {
    let c = &theme.tokens.colors;
    let palette = [
        c.accent[5],
        c.semantic.success,
        c.semantic.info,
        c.semantic.warning,
        c.accent[6],
        c.accent[3],
        Color::from_rgb8(217, 70, 239),
        Color::from_rgb8(20, 184, 166),
    ];
    let h = email.bytes().fold(tint as u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    palette[(h as usize) % palette.len()]
}

impl MailApp {
    fn draw_avatar(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, center: Point, radius: f64, addr: &Addr, tint: u8) {
        let col = avatar_color(&Theme::dark(), &addr.email, tint);
        scene.fill(Fill::NonZero, t, col, None, &Circle::new(center, radius));
        let initials = addr.initials();
        let size = (radius * 0.9) as f32;
        let (w, _h) = tr.measure_weighted(&initials, size, None, TextWeight::Semibold);
        tr.draw_text_weighted(
            scene,
            &initials,
            size,
            Color::WHITE,
            t * Affine::translate((center.x - w as f64 / 2.0, center.y - size as f64 * 0.62)),
            None,
            TextWeight::Semibold,
        );
    }

    fn draw_sidebar(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let sidebar_visible = match self.sidebar_mode {
            SidebarMode::Persistent => true,
            SidebarMode::Overlay => self.sidebar_open,
        };
        if !sidebar_visible {
            return;
        }
        let c = &theme.tokens.colors;
        let sb = self.sidebar_rect;
        // Overlay sidebar draws a scrim over the content first, then a shadow.
        if matches!(self.sidebar_mode, SidebarMode::Overlay) {
            fill_rect(scene, t, self.content_rect, c.surfaces.overlay);
            let shadow = Rect::new(sb.x1, sb.y0, sb.x1 + 8.0, sb.y1);
            scene.fill(Fill::NonZero, t, Color::from_rgba8(0, 0, 0, 70), None, &shadow);
        }
        fill_rect(scene, t, sb, c.neutral[0]);
        fill_rect(scene, t, Rect::new(sb.x1 - 1.0, sb.y0, sb.x1, sb.y1), c.border_standard);

        // Compose button.
        if let Some((rect, _)) = self.hits.iter().find(|(_, h)| matches!(h, Hit::Compose)) {
            let hov = rect.contains(self.last_mouse);
            let col = if hov { c.accent[4] } else { c.accent[5] };
            fill_round(scene, t, *rect, 10.0, col);
            let label = "\u{270E}  Compose";
            let (w, _) = tr.measure_weighted(label, 14.0, None, TextWeight::Semibold);
            tr.draw_text_weighted(
                scene,
                label,
                14.0,
                Color::WHITE,
                t * Affine::translate(((rect.x0 + rect.x1) / 2.0 - w as f64 / 2.0, rect.y0 + 11.0)),
                None,
                TextWeight::Semibold,
            );
        }

        // Account headers + folders.
        for (rect, hit) in &self.hits {
            match hit {
                Hit::Account(ai) => {
                    let Some(account) = self.accounts.accounts.get(*ai) else { continue };
                    let active = *ai == self.accounts.active;
                    let hov = rect.contains(self.last_mouse);
                    if active {
                        fill_round(scene, t, *rect, 9.0, c.accent[5].with_alpha_factor(0.12));
                    } else if hov {
                        fill_round(scene, t, *rect, 9.0, c.surface_white_5);
                    }
                    // chevron
                    let chev = if account.collapsed { "\u{25B8}" } else { "\u{25BE}" };
                    tr.draw_text(scene, chev, 11.0, c.text_dim, t * Affine::translate((rect.x0 + 8.0, rect.y0 + 15.0)), None);
                    // avatar
                    self.draw_avatar(
                        scene,
                        tr,
                        t,
                        Point::new(rect.x0 + 36.0, rect.y0 + ACCOUNT_HEADER_H / 2.0),
                        13.0,
                        &Addr::new(account.config.display_name.clone(), account.config.email.clone()),
                        account.tint,
                    );
                    let label = elide(tr, account.label(), 13.0, TextWeight::Semibold, (SIDEBAR_W - 80.0) as f64);
                    let email = elide(tr, &account.config.email, 10.5, TextWeight::Regular, (SIDEBAR_W - 80.0) as f64);
                    tr.draw_text_weighted(
                        scene,
                        &label,
                        13.0,
                        c.text_primary,
                        t * Affine::translate((rect.x0 + 54.0, rect.y0 + 6.0)),
                        None,
                        TextWeight::Semibold,
                    );
                    tr.draw_text(
                        scene,
                        &email,
                        10.5,
                        c.text_dim,
                        t * Affine::translate((rect.x0 + 54.0, rect.y0 + 24.0)),
                        None,
                    );
                }
                Hit::Folder(ai, f) => {
                    let f = *f;
                    let active_account = *ai == self.accounts.active;
                    let selected = active_account && f == self.folder && self.mode == Mode::Mailbox;
                    let hov = rect.contains(self.last_mouse);
                    if selected {
                        fill_round(scene, t, *rect, 8.0, c.accent[5].with_alpha_factor(0.20));
                        fill_rect(scene, t, Rect::new(rect.x0, rect.y0 + 6.0, rect.x0 + 3.0, rect.y1 - 6.0), c.accent[5]);
                    } else if hov {
                        fill_round(scene, t, *rect, 8.0, c.surface_white_5);
                    }
                    let ty = rect.y0 + (FOLDER_ROW_H - 14.0) / 2.0;
                    let label_col = if selected { c.text_primary } else { c.neutral[7] };
                    tr.draw_text(scene, f.glyph(), 13.5, label_col, t * Affine::translate((rect.x0 + 12.0, ty)), None);
                    tr.draw_text_weighted(
                        scene,
                        f.title(),
                        13.0,
                        label_col,
                        t * Affine::translate((rect.x0 + 36.0, ty)),
                        Some(120.0),
                        if selected { TextWeight::Semibold } else { TextWeight::Regular },
                    );
                    if active_account {
                        let unread = self.mailbox.unread_count(f);
                        if unread > 0 {
                            let txt = unread.to_string();
                            let (twd, _) = tr.measure_weighted(&txt, 11.0, None, TextWeight::Semibold);
                            let bw = (twd as f64 + 14.0).max(20.0);
                            let badge = Rect::new(rect.x1 - bw - 8.0, rect.y0 + 8.0, rect.x1 - 8.0, rect.y0 + 8.0 + 20.0);
                            let bg = if selected { c.accent[5] } else { c.neutral[3] };
                            fill_round(scene, t, badge, 10.0, bg);
                            tr.draw_text_weighted(
                                scene,
                                &txt,
                                11.0,
                                Color::WHITE,
                                t * Affine::translate(((badge.x0 + badge.x1) / 2.0 - twd as f64 / 2.0, badge.y0 + 4.0)),
                                None,
                                TextWeight::Semibold,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn draw_topbar(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let r = self.rect;
        let bar = Rect::new(self.content_rect.x0, r.y0, r.x1, r.y0 + TOPBAR_H);
        fill_rect(scene, t, bar, c.surfaces.base);
        fill_rect(scene, t, Rect::new(bar.x0, bar.y1 - 1.0, bar.x1, bar.y1), c.border_standard);

        let narrow = self.rect.width() < WIDE_BREAKPOINT;
        // Hamburger or back button.
        if narrow && matches!(self.narrow_page, NarrowPage::Mail) {
            if let Some((br, _)) = self.hits.iter().find(|(_, h)| matches!(h, Hit::BackToList)) {
                let hov = br.contains(self.last_mouse);
                if hov {
                    fill_round(scene, t, *br, 8.0, c.surface_white_5);
                }
                tr.draw_text(scene, "\u{2190}", 17.0, c.neutral[8], t * Affine::translate((br.x0 + 8.0, br.y0 + 6.0)), None);
                tr.draw_text(scene, "Inbox", 13.0, c.neutral[8], t * Affine::translate((br.x0 + 32.0, br.y0 + 8.0)), None);
            }
        } else if let Some((hr, _)) = self.hits.iter().find(|(_, h)| matches!(h, Hit::SidebarToggle)) {
            let hov = hr.contains(self.last_mouse);
            if hov {
                fill_round(scene, t, *hr, 8.0, c.surface_white_5);
            }
            // three-line hamburger
            for k in 0..3 {
                let ly = hr.y0 + 11.0 + k as f64 * 5.0;
                fill_round(scene, t, Rect::new(hr.x0 + 8.0, ly, hr.x1 - 8.0, ly + 2.0), 1.0, c.neutral[8]);
            }
        }

        // Folder title + count (skip when narrow & on the Mail page).
        let show_title = !(narrow && matches!(self.narrow_page, NarrowPage::Mail));
        if show_title {
            let title_x = bar.x0 + 52.0;
            let title = self.folder.title();
            tr.draw_text_weighted(scene, title, 18.0, c.text_primary, t * Affine::translate((title_x, bar.y0 + 16.0)), None, TextWeight::Bold);
            let (twd, _) = tr.measure_weighted(title, 18.0, None, TextWeight::Bold);
            let count = self.mailbox.count(self.folder);
            tr.draw_text(scene, &format!("{count} conversations"), 12.0, c.text_dim, t * Affine::translate((title_x + 10.0 + twd as f64, bar.y0 + 21.0)), None);
        }

        // Search box.
        let sr = self.search_box_rect();
        fill_round(scene, t, sr, 9.0, c.surfaces.raised);
        stroke_round(scene, t, sr, 9.0, 1.0, if matches!(self.focus, Focus::Search) { c.border_focus } else { c.border_standard });
        tr.draw_text(scene, "\u{1F50D}", 13.0, c.text_dim, t * Affine::translate((sr.x0 + 9.0, sr.y0 + 9.0)), None);
        if self.search.is_empty() && !matches!(self.focus, Focus::Search) {
            tr.draw_text(scene, "Search mail", 13.0, c.text_dim, t * Affine::translate((sr.x0 + 30.0, sr.y0 + 9.0)), None);
        }
        if !self.search.is_empty() {
            tr.draw_text(scene, "\u{2715}", 12.0, c.text_dim, t * Affine::translate((sr.x1 - 20.0, sr.y0 + 9.0)), None);
        }

        // Settings gear.
        if let Some((gr, _)) = self.hits.iter().find(|(_, h)| matches!(h, Hit::Settings)) {
            let hov = gr.contains(self.last_mouse);
            if hov {
                fill_round(scene, t, *gr, 8.0, c.surface_white_5);
            }
            tr.draw_text(scene, "\u{2699}", 18.0, c.neutral[7], t * Affine::translate((gr.x0 + 6.0, gr.y0 + 5.0)), None);
        }
    }

    fn draw_list(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        if self.list_rect.area() <= 0.0 {
            return;
        }
        let c = &theme.tokens.colors;
        fill_rect(scene, t, self.list_rect, c.surfaces.base);
        fill_rect(scene, t, Rect::new(self.list_rect.x1 - 1.0, self.list_rect.y0, self.list_rect.x1, self.list_rect.y1), c.border_standard);

        let rows = self.list_rows();
        if rows.is_empty() {
            tr.draw_text(
                scene,
                if self.search.is_empty() { "No messages in this folder" } else { "No results" },
                13.0,
                c.text_dim,
                t * Affine::translate((self.list_rect.x0 + 20.0, self.list_rect.y0 + 24.0)),
                None,
            );
            return;
        }

        for (rect, hit) in &self.hits {
            let Hit::Thread(id) = hit else { continue };
            let id = *id;
            if rect.y1 > self.list_rect.y1 + 1.0 {
                continue;
            }
            let Some(msg) = self.mailbox.get(id) else { continue };
            let len = rows.iter().find(|(rid, _)| *rid == id).map(|(_, l)| *l).unwrap_or(1);
            let selected = self.selected == Some(id);
            let hov = rect.contains(self.last_mouse);

            // Swipe reveal: shift the row and paint a colored action zone behind.
            let swipe_dx = self.swipe.filter(|s| s.id == id).map(|s| s.dx).unwrap_or(0.0);
            if swipe_dx.abs() > 1.0 {
                let (col, glyph, label, on_left) = if swipe_dx < 0.0 {
                    (c.semantic.warning, "\u{1F5C4}", "Archive", false)
                } else {
                    (c.semantic.danger, "\u{1F5D1}", "Delete", true)
                };
                fill_rect(scene, t, *rect, col.with_alpha_factor(0.9));
                let gx = if on_left { rect.x0 + 18.0 } else { rect.x1 - 90.0 };
                tr.draw_text(scene, glyph, 16.0, Color::WHITE, t * Affine::translate((gx, rect.y0 + 28.0)), None);
                tr.draw_text_weighted(scene, label, 12.0, Color::WHITE, t * Affine::translate((gx + 26.0, rect.y0 + 30.0)), None, TextWeight::Semibold);
            }
            let row = Rect::new(rect.x0 + swipe_dx, rect.y0, rect.x1 + swipe_dx, rect.y1);

            if selected {
                fill_rect(scene, t, row, c.accent[5].with_alpha_factor(0.16));
                fill_rect(scene, t, Rect::new(row.x0, row.y0, row.x0 + 3.0, row.y1), c.accent[5]);
            } else if hov && swipe_dx.abs() < 1.0 {
                fill_rect(scene, t, row, c.surface_white_5);
            } else if swipe_dx.abs() < 1.0 {
                fill_rect(scene, t, row, c.surfaces.base);
            }
            fill_rect(scene, t, Rect::new(row.x0 + 12.0, row.y1 - 1.0, row.x1, row.y1), c.neutral[2]);

            let unread = !msg.read;
            if unread {
                scene.fill(Fill::NonZero, t, c.accent[5], None, &Circle::new(Point::new(row.x0 + 14.0, row.y0 + LIST_ROW_H / 2.0), 4.0));
            }
            self.draw_avatar(scene, tr, t, Point::new(row.x0 + 42.0, row.y0 + 26.0), 17.0, &msg.from, 0);

            let text_x = row.x0 + 70.0;
            let right = row.x1 - 14.0;
            let name_weight = if unread { TextWeight::Bold } else { TextWeight::Medium };
            let name = elide(tr, &msg.from.display(), 13.5, name_weight, right - text_x - 56.0);
            tr.draw_text_weighted(scene, &name, 13.5, c.text_primary, t * Affine::translate((text_x, row.y0 + 12.0)), None, name_weight);
            let date = format_relative(msg.timestamp_min, self.now_min);
            let (dw, _) = tr.measure(&date, 11.5, None);
            tr.draw_text(scene, &date, 11.5, c.text_dim, t * Affine::translate((right - dw as f64, row.y0 + 13.0)), None);

            let subj_y = row.y0 + 32.0;
            let subj_col = if unread { c.text_primary } else { c.neutral[7] };
            let subj_weight = if unread { TextWeight::Semibold } else { TextWeight::Regular };
            let subj_budget = right - text_x - if len > 1 { 48.0 } else { 16.0 };
            let subject = elide(tr, &msg.thread_subject(), 12.5, subj_weight, subj_budget);
            tr.draw_text_weighted(scene, &subject, 12.5, subj_col, t * Affine::translate((text_x, subj_y)), None, subj_weight);
            if len > 1 {
                let badge_txt = len.to_string();
                let (bw, _) = tr.measure_weighted(&badge_txt, 10.5, None, TextWeight::Semibold);
                let bx = right - bw as f64 - 12.0;
                let badge = Rect::new(bx - 6.0, subj_y - 1.0, right, subj_y + 15.0);
                fill_round(scene, t, badge, 8.0, c.neutral[3]);
                tr.draw_text_weighted(scene, &badge_txt, 10.5, c.neutral[8], t * Affine::translate((bx, subj_y + 1.0)), None, TextWeight::Semibold);
            }

            let icon_reserve = if msg.has_attachments() { 64.0 } else { 28.0 };
            let snippet = elide(tr, &msg.snippet(), 11.5, TextWeight::Regular, right - text_x - icon_reserve);
            tr.draw_text(scene, &snippet, 11.5, c.text_dim, t * Affine::translate((text_x, row.y0 + 52.0)), None);

            let star_col = if msg.starred { c.semantic.warning } else { c.neutral[4] };
            tr.draw_text(scene, if msg.starred { "\u{2605}" } else { "\u{2606}" }, 15.0, star_col, t * Affine::translate((right - 18.0, row.y0 + 50.0)), None);
            if msg.has_attachments() {
                tr.draw_text(scene, "\u{1F4CE}", 12.0, c.text_dim, t * Affine::translate((right - 40.0, row.y0 + 52.0)), None);
            }
        }
    }

    fn draw_reader(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        if self.reader_rect.area() <= 0.0 {
            return;
        }
        let c = &theme.tokens.colors;
        let rr = self.reader_rect;
        fill_rect(scene, t, rr, c.surfaces.raised);

        let msgs = self.selected_thread_messages();
        if msgs.is_empty() {
            let cx = (rr.x0 + rr.x1) / 2.0;
            tr.draw_text(scene, "\u{2709}", 40.0, c.neutral[4], t * Affine::translate((cx - 18.0, rr.y0 + rr.height() / 2.0 - 40.0)), None);
            let hint = "Select a conversation to read";
            let (w, _) = tr.measure(hint, 14.0, None);
            tr.draw_text(scene, hint, 14.0, c.text_dim, t * Affine::translate((cx - w as f64 / 2.0, rr.y0 + rr.height() / 2.0)), None);
            return;
        }

        self.draw_reader_actions(scene, t, tr, theme);

        let head = &msgs[msgs.len() - 1];
        let content_x = rr.x0 + 24.0;
        let content_w = (rr.width() - 48.0) as f32;
        let mut y = rr.y0 + 60.0;
        let subj = elide(tr, &head.thread_subject(), 20.0, TextWeight::Bold, content_w as f64);
        tr.draw_text_weighted(scene, &subj, 20.0, c.text_primary, t * Affine::translate((content_x, y)), None, TextWeight::Bold);
        y += 30.0;
        tr.draw_text(
            scene,
            &format!("{} message{} · {}", msgs.len(), if msgs.len() == 1 { "" } else { "s" }, self.folder.title()),
            12.0,
            c.text_dim,
            t * Affine::translate((content_x, y)),
            None,
        );
        y += 26.0;
        fill_rect(scene, t, Rect::new(content_x, y, rr.x1 - 24.0, y + 1.0), c.border_standard);
        y += 16.0;

        for (i, m) in msgs.iter().enumerate() {
            if y > rr.y1 - 40.0 {
                break;
            }
            y = self.draw_message_card(scene, t, tr, theme, m, content_x, y, content_w, i == msgs.len() - 1);
            y += 14.0;
        }
    }

    fn draw_reader_actions(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let starred = self.selected.and_then(|id| self.mailbox.get(id)).map(|m| m.starred).unwrap_or(false);
        for (rect, hit) in &self.hits {
            let (label, danger, glyph) = match hit {
                Hit::Reply => ("Reply", false, "\u{21A9}"),
                Hit::ArchiveSel => ("Archive", false, "\u{1F5C4}"),
                Hit::DeleteSel => ("Delete", true, "\u{1F5D1}"),
                Hit::StarSel => (if starred { "Starred" } else { "Star" }, false, if starred { "\u{2605}" } else { "\u{2606}" }),
                Hit::UnreadSel => ("Mark unread", false, "\u{2709}"),
                _ => continue,
            };
            let hov = rect.contains(self.last_mouse);
            let bg = if hov { c.surface_white_10 } else { c.surface_white_5 };
            fill_round(scene, t, *rect, 8.0, bg);
            stroke_round(scene, t, *rect, 8.0, 1.0, c.border_white_10);
            let col = if danger { c.semantic.danger } else if matches!(hit, Hit::StarSel) && starred { c.semantic.warning } else { c.neutral[8] };
            tr.draw_text(scene, glyph, 12.0, col, t * Affine::translate((rect.x0 + 10.0, rect.y0 + 9.0)), None);
            tr.draw_text_weighted(scene, label, 12.0, col, t * Affine::translate((rect.x0 + 28.0, rect.y0 + 9.0)), None, TextWeight::Medium);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_message_card(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, m: &Message, x: f64, y: f64, w: f32, expanded: bool) -> f64 {
        let c = &theme.tokens.colors;
        let card_x1 = x + w as f64;
        self.draw_avatar(scene, tr, t, Point::new(x + 18.0, y + 18.0), 17.0, &m.from, 0);
        let from_name = elide(tr, &m.from.display(), 13.5, TextWeight::Semibold, (w - 150.0) as f64);
        tr.draw_text_weighted(scene, &from_name, 13.5, c.text_primary, t * Affine::translate((x + 44.0, y + 6.0)), None, TextWeight::Semibold);
        let from_email = elide(tr, &m.from.email, 11.5, TextWeight::Regular, (w - 150.0) as f64);
        tr.draw_text(scene, &from_email, 11.5, c.text_dim, t * Affine::translate((x + 44.0, y + 24.0)), None);
        let date = format_absolute(m.timestamp_min);
        let (dw, _) = tr.measure(&date, 11.5, None);
        tr.draw_text(scene, &date, 11.5, c.text_dim, t * Affine::translate((card_x1 - dw as f64, y + 8.0)), None);
        let to_line = elide(tr, &format!("to {}", m.to.iter().map(|a| a.display()).collect::<Vec<_>>().join(", ")), 11.0, TextWeight::Regular, 150.0);
        tr.draw_text(scene, &to_line, 11.0, c.text_dim, t * Affine::translate((card_x1 - 150.0, y + 24.0)), None);

        let mut yy = y + 48.0;
        if expanded {
            for para in m.body.split('\n') {
                if para.trim().is_empty() {
                    yy += 8.0;
                    continue;
                }
                let quote = para.starts_with('>');
                let col = if quote { c.text_dim } else { c.neutral[8] };
                let (_pw, ph) = tr.measure(para, 13.0, Some(w - 8.0));
                tr.draw_text(scene, para, 13.0, col, t * Affine::translate((x, yy)), Some(w - 8.0));
                yy += ph.max(18.0) as f64 + 2.0;
            }
            if m.has_attachments() {
                yy += 8.0;
                tr.draw_text_weighted(scene, &format!("{} attachment{}", m.attachments.len(), if m.attachments.len() == 1 { "" } else { "s" }), 11.5, c.text_dim, t * Affine::translate((x, yy)), None, TextWeight::Medium);
                yy += 20.0;
                let mut ax = x;
                for a in &m.attachments {
                    let label = format!("{}  {}", a.filename, a.size_human());
                    let (lw, _) = tr.measure(&label, 11.5, None);
                    let chip_w = lw as f64 + 44.0;
                    if ax + chip_w > card_x1 {
                        ax = x;
                        yy += 36.0;
                    }
                    let chip = Rect::new(ax, yy, ax + chip_w, yy + 30.0);
                    fill_round(scene, t, chip, 8.0, c.surface_white_5);
                    stroke_round(scene, t, chip, 8.0, 1.0, c.border_white_10);
                    tr.draw_text(scene, a.glyph(), 14.0, c.neutral[7], t * Affine::translate((chip.x0 + 8.0, chip.y0 + 7.0)), None);
                    tr.draw_text(scene, &a.filename, 11.5, c.text_primary, t * Affine::translate((chip.x0 + 30.0, chip.y0 + 4.0)), Some(lw));
                    tr.draw_text(scene, &a.size_human(), 10.0, c.text_dim, t * Affine::translate((chip.x0 + 30.0, chip.y0 + 17.0)), None);
                    ax += chip_w + 10.0;
                }
                yy += 30.0;
            }
        } else {
            tr.draw_text(scene, &m.snippet(), 12.0, c.text_dim, t * Affine::translate((x, yy)), Some(w - 8.0));
            yy += 18.0;
        }
        yy
    }

    fn draw_context_menu(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let Some(menu) = &self.context_menu else { return };
        let c = &theme.tokens.colors;
        let mr = context_menu_rect(menu, self.rect);
        // shadow
        scene.fill(Fill::NonZero, t, Color::from_rgba8(0, 0, 0, 80), None, &RoundedRect::from_rect(Rect::new(mr.x0 + 1.0, mr.y0 + 3.0, mr.x1 + 1.0, mr.y1 + 4.0), 10.0));
        fill_round(scene, t, mr, 10.0, c.surfaces.raised);
        stroke_round(scene, t, mr, 10.0, 1.0, c.border_white_10);
        let (read, starred) = self
            .mailbox
            .get(menu.target)
            .map(|m| (m.read, m.starred))
            .unwrap_or((false, false));
        let mut y = mr.y0 + 6.0;
        for a in &menu.items {
            let item = Rect::new(mr.x0, y, mr.x1, y + 28.0);
            let hov = item.contains(self.last_mouse);
            if hov {
                fill_round(scene, t, Rect::new(item.x0 + 4.0, item.y0 + 1.0, item.x1 - 4.0, item.y1 - 1.0), 6.0, c.surface_white_10);
            }
            let danger = matches!(a, CtxAction::Delete);
            let col = if danger { c.semantic.danger } else { c.text_primary };
            tr.draw_text(scene, &a.label(read, starred), 12.5, col, t * Affine::translate((mr.x0 + 14.0, y + 7.0)), None);
            y += 28.0;
        }
    }

    fn draw_setup(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        fill_rect(scene, t, self.rect, c.surfaces.overlay);
        let p = self.overlay_panel_rect();
        fill_round(scene, t, p, 14.0, c.surfaces.raised);
        stroke_round(scene, t, p, 14.0, 1.0, c.border_white_10);
        let pad = 24.0;
        let lx = p.x0 + pad;
        let heading = if self.setup_editing.is_some() { "Edit account" } else { "Add account" };
        tr.draw_text_weighted(scene, heading, 16.0, c.text_primary, t * Affine::translate((lx, p.y0 + 18.0)), None, TextWeight::Bold);
        tr.draw_text(scene, "Managed in Settings → Accounts; edited here for convenience.", 11.5, c.text_dim, t * Affine::translate((lx, p.y0 + 38.0)), None);

        let labels = ["Display name", "Email", "Username", "Password", "IMAP host", "", "SMTP host", ""];
        let cfg_errs: std::collections::HashSet<&str> = self.read_setup_config().validate().into_iter().map(|(k, _)| k).collect();
        let err_for = |i: usize| match i {
            1 => cfg_errs.contains("email"),
            2 => cfg_errs.contains("username"),
            4 => cfg_errs.contains("imap_host"),
            6 => cfg_errs.contains("smtp_host"),
            _ => false,
        };
        for (rect, hit) in &self.hits {
            let Hit::SetupField(i) = hit else { continue };
            let i = *i;
            if i >= self.setup_inputs.len() {
                continue;
            }
            let focused = matches!(self.focus, Focus::Setup(j) if j == i);
            if i < labels.len() && !labels[i].is_empty() {
                tr.draw_text(scene, labels[i], 12.5, c.text_dim, t * Affine::translate((lx, rect.y0 + 9.0)), None);
            }
            if i == 5 {
                tr.draw_text(scene, "Port", 11.0, c.text_dim, t * Affine::translate((rect.x0, rect.y0 - 14.0)), None);
            }
            fill_round(scene, t, Rect::new(rect.x0 - 2.0, rect.y0 - 2.0, rect.x1 + 2.0, rect.y1 + 2.0), 7.0, c.surfaces.base);
            let border = if err_for(i) { c.semantic.danger } else if focused { c.border_focus } else { c.border_standard };
            stroke_round(scene, t, Rect::new(rect.x0 - 2.0, rect.y0 - 2.0, rect.x1 + 2.0, rect.y1 + 2.0), 7.0, 1.0, border);
        }
        for (rect, hit) in &self.hits {
            let (on, label) = match hit {
                Hit::ToggleImapTls => (self.setup_imap_tls, "TLS"),
                Hit::ToggleSmtpTls => (self.setup_smtp_tls, "TLS"),
                _ => continue,
            };
            let pill = Rect::new(rect.x0, rect.y0 + 4.0, rect.x0 + 44.0, rect.y0 + 26.0);
            fill_round(scene, t, pill, 11.0, if on { c.accent[5] } else { c.neutral[3] });
            let knob_x = if on { pill.x1 - 18.0 } else { pill.x0 + 4.0 };
            scene.fill(Fill::NonZero, t, Color::WHITE, None, &Circle::new(Point::new(knob_x + 7.0, pill.y0 + 11.0), 8.0));
            tr.draw_text(scene, label, 12.0, c.neutral[8], t * Affine::translate((pill.x1 + 8.0, rect.y0 + 8.0)), None);
        }
        let matches_hit = |a: &Hit, b: &Hit| std::mem::discriminant(a) == std::mem::discriminant(b);
        for (rect, hit) in &self.hits {
            if matches_hit(hit, &Hit::SetupSave) {
                let hov = rect.contains(self.last_mouse);
                fill_round(scene, t, *rect, 9.0, if hov { c.accent[4] } else { c.accent[5] });
                let label = "Save account";
                let (w, _) = tr.measure_weighted(label, 13.5, None, TextWeight::Semibold);
                tr.draw_text_weighted(scene, label, 13.5, Color::WHITE, t * Affine::translate(((rect.x0 + rect.x1) / 2.0 - w as f64 / 2.0, rect.y0 + 10.0)), None, TextWeight::Semibold);
            } else if matches!(hit, Hit::CancelOverlay) {
                let hov = rect.contains(self.last_mouse);
                fill_round(scene, t, *rect, 9.0, if hov { c.surface_white_10 } else { c.surface_white_5 });
                let (w, _) = tr.measure("Cancel", 13.0, None);
                tr.draw_text(scene, "Cancel", 13.0, c.neutral[8], t * Affine::translate(((rect.x0 + rect.x1) / 2.0 - w as f64 / 2.0, rect.y0 + 10.0)), None);
            }
        }
    }

    fn draw_status(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let Some(msg) = &self.status else { return };
        let c = &theme.tokens.colors;
        let (w, _) = tr.measure(msg, 12.5, None);
        let toast = Rect::new(self.rect.x1 - w as f64 - 64.0, self.rect.y1 - 52.0, self.rect.x1 - 24.0, self.rect.y1 - 16.0);
        fill_round(scene, t, toast, 10.0, c.neutral[2]);
        stroke_round(scene, t, toast, 10.0, 1.0, c.border_white_10);
        tr.draw_text(scene, "\u{2713}", 13.0, c.semantic.success, t * Affine::translate((toast.x0 + 12.0, toast.y0 + 11.0)), None);
        tr.draw_text(scene, msg, 12.5, c.text_primary, t * Affine::translate((toast.x0 + 32.0, toast.y0 + 11.0)), None);
    }
}

// =============================== Widget impl ===============================

impl Widget for MailApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if !self.seeded {
            self.seed_doc(state);
            self.seeded = true;
        } else {
            MailDoc::apply_flags(state, &mut self.mailbox);
        }
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        self.relayout();
    }

    fn draw(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        let c = &theme.tokens.colors;
        fill_rect(scene, t, self.rect, c.surfaces.base);
        // Content panes first.
        self.draw_topbar(scene, t, tr, theme);
        self.draw_list(scene, t, tr, theme);
        self.draw_reader(scene, t, tr, theme);
        // Sidebar (persistent inline, or overlay-over-content with scrim).
        self.draw_sidebar(scene, t, tr, theme);
        // Search input text/caret on top of the topbar chrome.
        self.search_input.draw(scene, t, tr, theme, presence);

        if self.mode == Mode::Setup {
            self.draw_setup(scene, t, tr, theme);
            for inp in &mut self.setup_inputs {
                inp.draw(scene, t, tr, theme, presence);
            }
        }

        // Compose window (above everything except status/context menu).
        self.compose.draw(scene, t, tr, theme, presence);
        self.compose.draw_inputs(scene, t, tr, theme, presence);

        // Context menu sits on top of all.
        self.draw_context_menu(scene, t, tr, theme);

        self.draw_status(scene, t, tr, theme);
    }

    fn draw_with_presence(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx, _permissions: &PermissionContext) {
        self.draw(scene, t, tr, theme, presence);
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
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
            self.dirty = true;
        }

        // 1) Compose window has top priority and is modal while open: it
        //    consumes any event inside it and every keyboard event, so nothing
        //    falls through to the mailbox behind it.
        if self.compose.open {
            let (consumed, action) = self.compose.handle_event(event, scale_factor, modifiers, state, path, node_id, taffy);
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

        // 2) Esc closes overlays / context menu.
        if let WindowEvent::KeyboardInput { event: KeyEvent { logical_key: Key::Named(NamedKey::Escape), state: ElementState::Pressed, .. }, .. } = event {
            if self.context_menu.is_some() {
                self.context_menu = None;
                *handled = true;
                return;
            }
            if self.mode != Mode::Mailbox {
                self.close_overlay(state);
                *handled = true;
                return;
            }
        }

        // 3) Route keystrokes to a focused embedded input (search / setup).
        if matches!(event, WindowEvent::KeyboardInput { .. }) {
            let focus = self.focus;
            if let Some(inp) = self.focused_input() {
                inp.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy);
                if matches!(focus, Focus::Search) {
                    self.search = self.search_input.value().to_string();
                    MailDoc::set_search(state, &self.search);
                    self.selected = self.list_rows().first().map(|(id, _)| *id);
                }
                if *handled {
                    self.dirty = true;
                    return;
                }
            }
        }

        // 4) Global keyboard shortcuts (mailbox mode, no field focused).
        if self.mode == Mode::Mailbox && matches!(self.focus, Focus::None) {
            if let WindowEvent::KeyboardInput { event: KeyEvent { logical_key, state: ElementState::Pressed, .. }, .. } = event {
                match logical_key {
                    Key::Character(ch) if ch.eq_ignore_ascii_case("c") => {
                        self.open_compose_new();
                        *handled = true;
                        return;
                    }
                    Key::Character(ch) if ch == "/" => {
                        self.focus_only(Focus::Search);
                        *handled = true;
                        return;
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.move_selection(state, 1);
                        *handled = true;
                        return;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.move_selection(state, -1);
                        *handled = true;
                        return;
                    }
                    Key::Character(ch) if ch.eq_ignore_ascii_case("e") => {
                        if let Some(id) = self.selected {
                            self.apply_message_action(state, id, CtxAction::Archive);
                            *handled = true;
                            return;
                        }
                    }
                    Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                        if let Some(id) = self.selected {
                            self.apply_message_action(state, id, CtxAction::Delete);
                            *handled = true;
                            return;
                        }
                    }
                    Key::Character(ch) if ch.eq_ignore_ascii_case("s") => {
                        if let Some(id) = self.selected {
                            self.apply_message_action(state, id, CtxAction::ToggleStar);
                            *handled = true;
                            return;
                        }
                    }
                    _ => {}
                }
            }
        }

        // 5) Right-click → context menu on a row.
        if let WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } = event {
            let pt = self.last_mouse;
            if let Some(id) = self.row_id_at(pt) {
                self.open_context_menu(id, pt);
                *handled = true;
                return;
            }
        }

        // 6) Left-press handling: context-menu items, swipe begin, hits.
        if let WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } = event {
            let pt = self.last_mouse;
            // Context menu open: a click either hits an item or dismisses.
            if self.context_menu.is_some() {
                let item = self.hits.iter().rev().find(|(r, h)| matches!(h, Hit::CtxItem(_)) && r.contains(pt));
                if let Some((_, Hit::CtxItem(i))) = item {
                    let i = *i;
                    if let Some(menu) = &self.context_menu {
                        if let Some(action) = menu.items.get(i).copied() {
                            let target = menu.target;
                            self.context_menu = None;
                            self.apply_message_action(state, target, action);
                        }
                    }
                } else {
                    self.context_menu = None;
                }
                *handled = true;
                return;
            }
            // Begin a potential swipe when pressing on a row.
            if let Some(id) = self.row_id_at(pt) {
                self.mouse_down_row = Some((id, pt));
            }
            // Setup overlay click-out cancels.
            if self.mode == Mode::Setup {
                let panel = self.overlay_panel_rect();
                let hit = self.hits.iter().rev().find(|(r, _)| r.contains(pt)).map(|(_, h)| h.clone());
                if !panel.contains(pt) && hit.is_none() {
                    self.close_overlay(state);
                    *handled = true;
                    return;
                }
            }
            let hit = self.hits.iter().rev().find(|(r, _)| r.contains(pt)).map(|(_, h)| h.clone());
            if let Some(hit) = hit {
                self.dispatch_hit(state, hit, pt);
                *handled = true;
                return;
            }
        }

        // 7) Drag → swipe on a row.
        if let WindowEvent::CursorMoved { .. } = event {
            if let Some((id, start)) = self.mouse_down_row {
                let dx = self.last_mouse.x - start.x;
                if dx.abs() > 6.0 {
                    self.swipe = Some(SwipeState { id, start_x: start.x, dx });
                    self.dirty = true;
                }
            }
        }

        // 8) Release → commit swipe if past threshold.
        if let WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } = event {
            if let Some(s) = self.swipe.take() {
                // swipe-left = Archive, swipe-right = Delete — both mirror the
                // context menu (swipe == context-menu principle).
                if s.dx <= -SWIPE_THRESHOLD {
                    self.apply_message_action(state, s.id, CtxAction::Archive);
                } else if s.dx >= SWIPE_THRESHOLD {
                    self.apply_message_action(state, s.id, CtxAction::Delete);
                }
                let _ = s.start_x;
                *handled = true;
            }
            self.mouse_down_row = None;
        }

        self.dirty = true;
        let _ = focus_target;
    }

    fn name(&self) -> &str {
        "MailApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.compose.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.compose.mark_drawn();
    }
}

impl MailApp {
    fn row_id_at(&self, pt: Point) -> Option<MessageId> {
        self.hits.iter().rev().find_map(|(r, h)| match h {
            Hit::Thread(id) if r.contains(pt) => Some(*id),
            _ => None,
        })
    }

    fn move_selection(&mut self, state: &mut AutoCommit, delta: i32) {
        let rows = self.list_rows();
        if rows.is_empty() {
            return;
        }
        let cur = self
            .selected
            .and_then(|sel| {
                rows.iter().position(|(id, _)| {
                    *id == sel
                        || self.mailbox.get(sel).map(|m| m.thread_subject()) == self.mailbox.get(*id).map(|x| x.thread_subject())
                })
            })
            .unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, rows.len() as i32 - 1) as usize;
        let id = rows[next].0;
        self.select_message(state, id);
    }

    fn dispatch_hit(&mut self, state: &mut AutoCommit, hit: Hit, _pt: Point) {
        match hit {
            Hit::Account(ai) => {
                self.accounts.set_active(ai);
                MailDoc::set_active_account(state, self.accounts.active);
                self.select_folder(state, Folder::Inbox);
            }
            Hit::AccountToggle(ai) => {
                self.accounts.toggle_collapsed(ai);
                self.dirty = true;
            }
            Hit::Folder(ai, f) => {
                self.mode = Mode::Mailbox;
                self.focus_only(Focus::None);
                self.accounts.set_active(ai);
                MailDoc::set_active_account(state, self.accounts.active);
                self.select_folder(state, f);
            }
            Hit::Thread(id) => {
                self.focus_only(Focus::None);
                self.select_message(state, id);
            }
            Hit::ThreadStar(id) => {
                self.mailbox.toggle_star(id);
                self.persist_flags(state, id);
                self.dirty = true;
            }
            Hit::Compose => self.open_compose_new(),
            Hit::Settings => self.open_setup_edit(Some(self.accounts.active)),
            Hit::SidebarToggle => {
                self.sidebar_open = !self.sidebar_open;
                self.dirty = true;
            }
            Hit::ScrimDismiss => {
                self.sidebar_open = false;
                self.dirty = true;
            }
            Hit::BackToList => {
                self.narrow_page = NarrowPage::List;
                self.dirty = true;
            }
            Hit::SearchClear => {
                self.search.clear();
                self.search_input.set_text("");
                MailDoc::set_search(state, "");
                self.focus_only(Focus::None);
            }
            Hit::Reply => {
                if let Some(id) = self.selected {
                    self.open_reply(id);
                }
            }
            Hit::ArchiveSel => {
                if let Some(id) = self.selected {
                    self.apply_message_action(state, id, CtxAction::Archive);
                }
            }
            Hit::DeleteSel => {
                if let Some(id) = self.selected {
                    self.apply_message_action(state, id, CtxAction::Delete);
                }
            }
            Hit::StarSel => {
                if let Some(id) = self.selected {
                    self.apply_message_action(state, id, CtxAction::ToggleStar);
                }
            }
            Hit::UnreadSel => {
                if let Some(id) = self.selected {
                    self.mailbox.mark_read(id, false);
                    self.persist_flags(state, id);
                    self.dirty = true;
                }
            }
            Hit::CtxItem(_) => {} // handled in the context-menu branch
            Hit::SetupField(i) => {
                if i < self.setup_inputs.len() {
                    self.focus_only(Focus::Setup(i));
                }
            }
            Hit::ToggleImapTls => {
                self.setup_imap_tls = !self.setup_imap_tls;
                self.setup_inputs[5].set_text(if self.setup_imap_tls { "993" } else { "143" });
                self.dirty = true;
            }
            Hit::ToggleSmtpTls => {
                self.setup_smtp_tls = !self.setup_smtp_tls;
                self.setup_inputs[7].set_text(if self.setup_smtp_tls { "465" } else { "587" });
                self.dirty = true;
            }
            Hit::SetupSave => self.save_setup(state),
            Hit::CancelOverlay => self.close_overlay(state),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_app() -> (MailApp, AutoCommit) {
        let app = MailApp::new_sample();
        let doc = AutoCommit::new();
        (app, doc)
    }

    #[test]
    fn starts_in_mailbox_with_a_selection() {
        let (app, _) = new_app();
        assert!(matches!(app.mode, Mode::Mailbox));
        assert!(app.selected.is_some());
        assert_eq!(app.folder, Folder::Inbox);
    }

    #[test]
    fn has_multiple_accounts() {
        let (app, _) = new_app();
        assert!(app.accounts.len() >= 2, "sample provides multiple accounts");
    }

    #[test]
    fn selecting_message_marks_read_and_persists() {
        let (mut app, mut doc) = new_app();
        app.sync(&mut doc, &automerge::ROOT);
        let unread = app.mailbox.messages_in(Folder::Inbox).into_iter().find(|m| !m.read).map(|m| m.id);
        let id = unread.expect("some unread message");
        app.select_message(&mut doc, id);
        assert!(app.mailbox.get(id).unwrap().read);
        let mut fresh = app.mailbox.clone();
        for m in fresh.messages.values_mut() {
            m.read = false;
        }
        MailDoc::apply_flags(&doc, &mut fresh);
        assert!(fresh.get(id).unwrap().read);
    }

    #[test]
    fn compose_send_lands_in_sent() {
        let (mut app, mut doc) = new_app();
        let before = app.mailbox.count(Folder::Sent);
        app.open_compose_new();
        app.compose.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        // Drive the compose window through its public draft API.
        let draft = crate::model::Draft { to: "dest@there.example".into(), subject: "Hi".into(), body: "Hello".into(), html_body: Some("<p>Hello</p>".into()), ..Default::default() };
        app.compose.reattach(&draft);
        app.send_compose(&mut doc);
        assert!(!app.compose.open);
        assert_eq!(app.mailbox.count(Folder::Sent), before + 1);
        assert!(app.status.as_deref().unwrap_or("").starts_with("Sent"));
    }

    #[test]
    fn context_menu_archive_moves_message() {
        let (mut app, mut doc) = new_app();
        let id = app.mailbox.messages_in(Folder::Inbox)[0].id;
        app.apply_message_action(&mut doc, id, CtxAction::Archive);
        assert_eq!(app.mailbox.get(id).unwrap().folder, Folder::Archive);
    }

    #[test]
    fn context_and_swipe_actions_are_mirrored() {
        // The swipe gestures must each map to an action that's also in the
        // context menu (swipe == context-menu principle).
        let (app, _) = new_app();
        let id = app.mailbox.messages_in(Folder::Inbox)[0].id;
        let menu = app.context_items_for(id);
        // swipe-left = Archive, swipe-right = Delete
        assert!(menu.contains(&CtxAction::Archive), "swipe-left action present in menu");
        assert!(menu.contains(&CtxAction::Delete), "swipe-right action present in menu");
    }

    #[test]
    fn responsive_breakpoint_switches_sidebar_mode() {
        let (mut app, mut doc) = new_app();
        app.sync(&mut doc, &automerge::ROOT);
        // Wide: persistent sidebar, list + reader side by side.
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert!(matches!(app.sidebar_mode, SidebarMode::Persistent));
        assert!(app.list_rect.area() > 0.0 && app.reader_rect.area() > 0.0);
        // Narrow: overlay sidebar, single content page (list only by default).
        app.rect = Rect::new(0.0, 0.0, 720.0, 800.0);
        app.narrow_page = NarrowPage::List;
        app.relayout();
        assert!(matches!(app.sidebar_mode, SidebarMode::Overlay));
        assert!(app.reader_rect.area() <= 0.0, "narrow list page hides the reader");
    }

    #[test]
    fn narrow_message_select_pushes_mail_page() {
        let (mut app, mut doc) = new_app();
        app.sync(&mut doc, &automerge::ROOT);
        app.rect = Rect::new(0.0, 0.0, 720.0, 800.0);
        app.relayout();
        let id = app.mailbox.messages_in(Folder::Inbox)[0].id;
        app.select_message(&mut doc, id);
        assert!(matches!(app.narrow_page, NarrowPage::Mail));
    }

    #[test]
    fn setup_save_updates_account() {
        let (mut app, mut doc) = new_app();
        app.open_setup_edit(None);
        app.setup_inputs[1].set_text("new@host.example");
        app.setup_inputs[2].set_text("new@host.example");
        app.setup_inputs[4].set_text("imap.host.example");
        app.setup_inputs[6].set_text("smtp.host.example");
        let before = app.accounts.len();
        app.save_setup(&mut doc);
        assert!(matches!(app.mode, Mode::Mailbox));
        assert_eq!(app.accounts.len(), before + 1);
        assert_eq!(app.accounts.active_config().email, "new@host.example");
    }

    #[test]
    fn search_filters_rows() {
        let (mut app, _doc) = new_app();
        app.search = "launch".into();
        let rows = app.list_rows();
        assert!(!rows.is_empty());
        for (id, _) in rows {
            let m = app.mailbox.get(id).unwrap();
            let hay = format!("{} {} {}", m.subject, m.from.display(), m.body).to_ascii_lowercase();
            assert!(hay.contains("launch"));
        }
    }
}
