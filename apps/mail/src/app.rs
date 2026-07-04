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

use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{NavPane, NavShell, TextInput, Widget};
use ui::ImageStore;

use crate::account::{Account, AccountStore};
use crate::compose::{ComposeAction, ComposeWindow};
use crate::doc::MailDoc;
use crate::draw::{fill_rect, fill_round, stroke_round};
use crate::engine::{EngineEvent, EngineRequest};
use crate::model::{AccountConfig, Addr, Attachment, ComposeFormat, Draft, Folder, Message, MessageId};
use crate::pane::{Pane, Role};
use crate::state::{CtxAction, Intent, MailState, Shared};
use crate::util::format_absolute;

/// Width (logical px) at/above which the shell is wide (docked sidebar +
/// three panes abreast); below it the sidebar is an overlay drawer and the
/// content is a single push page. Shared with the [`NavShell`].
const BREAKPOINT: f64 = 860.0;
const SIDEBAR_W: f64 = 260.0;
const OVERVIEW_W: f64 = 340.0;

/// An open row context menu.
struct ContextMenu {
    target: MessageId,
    pos: Point,
    items: Vec<CtxAction>,
}

/// A hit region in one of the app-owned overlays (setup sheet / context menu).
#[derive(Clone)]
enum OverlayHit {
    SetupField(usize),
    ToggleImapTls,
    ToggleSmtpTls,
    SetupSave,
    CancelSetup,
    CtxItem(usize),
}

/// The engine worker channel pair (live app only).
struct EngineHandle {
    tx: std::sync::mpsc::Sender<EngineRequest>,
    rx: std::sync::mpsc::Receiver<EngineEvent>,
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

    engine: Option<EngineHandle>,
    connect_requested: bool,
    /// True between posting a connect/fetch and receiving its result, so the
    /// app only keeps polling the worker while a reply is outstanding (it fully
    /// idles otherwise instead of busy-looping).
    awaiting_engine: bool,

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
        let shared = MailState::shared(MailState::new_sample(avatar_store()));
        Self::assemble(shared, None)
    }

    /// The live constructor: a real account list loaded from disk and the
    /// engine worker spawned. No fabricated messages.
    pub fn new_live() -> Self {
        let accounts = load_account_store();
        let shared = MailState::shared(MailState::new_live(accounts, avatar_store()));
        let (tx, rx) = crate::engine::spawn();
        Self::assemble(shared, Some(EngineHandle { tx, rx }))
    }

    fn assemble(shared: Shared, engine: Option<EngineHandle>) -> Self {
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
            engine,
            connect_requested: false,
            awaiting_engine: false,
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
        let mut st = self.shared.borrow_mut();
        st.narrow = narrow;
        st.detail_pushed = detail;
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
                    size: TSize { width: Dimension::length(w), height: Dimension::length(h) },
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
                    self.context_menu = Some(ContextMenu { target: id, pos: pt, items });
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
            if cc.iter().any(|existing| existing.eq_ignore_ascii_case(&a.rfc5322())) {
                continue;
            }
            cc.push(a.rfc5322());
        }
        self.compose.open_reply_all(&msg.from.rfc5322(), &cc.join(", "), &subj, &quote);
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
                    attachments: draft.attachments.iter().map(|p| attachment_meta(p)).collect(),
                    timestamp_min: now,
                    read: true,
                    starred: false,
                    folder: Folder::Sent,
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
                    let kind = if draft.html_body.is_some() { "HTML" } else { "text" };
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
        // Persist the draft (survives a crash / syncs to peers) …
        MailDoc::store_draft(doc, &draft);
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
            (Addr::new(acct.display_name.clone(), acct.email.clone()), st.now_min)
        };
        let id = self.editing_draft.unwrap_or_else(uuid::Uuid::new_v4);
        let msg = Message {
            id,
            rfc_message_id: None,
            in_reply_to: None,
            from,
            to: crate::transport::parse_addr_list(&draft.to),
            cc: crate::transport::parse_addr_list(&draft.cc),
            subject: if draft.subject.trim().is_empty() { "(no subject)".into() } else { draft.subject.clone() },
            body: draft.body.clone(),
            attachments: draft.attachments.iter().map(|p| attachment_meta(p)).collect(),
            timestamp_min: now,
            read: true,
            starred: false,
            folder: Folder::Drafts,
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
        self.setup_open = true;
        self.focus_setup(Some(1));
        self.context_menu = None;
        self.dirty = true;
    }

    fn close_setup(&mut self) {
        self.setup_open = false;
        self.focus_setup(None);
        self.dirty = true;
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
            password: g(3),
            imap_host: g(4),
            imap_port: g(5).trim().parse().unwrap_or(993),
            smtp_host: g(6),
            smtp_port: g(7).trim().parse().unwrap_or(465),
            imap_tls: self.setup_imap_tls,
            smtp_tls: self.setup_smtp_tls,
        }
    }

    /// Guess IMAP/SMTP hosts + ports from the typed email when the host fields
    /// are still blank, using [`AccountConfig::autofill_from_email`]. Runs when
    /// focus leaves the email field and again on save, so typing an address
    /// actually fills the connection settings.
    fn maybe_autofill(&mut self) {
        let email = self.setup_inputs[1].value().trim().to_string();
        if !email.contains('@') {
            return;
        }
        let imap_blank = self.setup_inputs[4].value().trim().is_empty();
        let smtp_blank = self.setup_inputs[6].value().trim().is_empty();
        if !imap_blank && !smtp_blank {
            return;
        }
        let mut cfg = AccountConfig {
            email,
            username: self.setup_inputs[2].value().to_string(),
            display_name: self.setup_inputs[0].value().to_string(),
            ..Default::default()
        };
        cfg.autofill_from_email();
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
        self.dirty = true;
    }

    fn save_setup(&mut self, doc: &mut AutoCommit) {
        self.maybe_autofill();
        let cfg = self.read_setup_config();
        if !cfg.is_valid() {
            self.shared.borrow_mut().status = Some("Fix the highlighted fields".into());
            self.dirty = true;
            return;
        }
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
            account.id = if cfg.email.is_empty() { account.id } else { cfg.email.clone() };
            st.accounts.upsert(account);
            MailDoc::store_account(doc, &cfg);
            MailDoc::set_active_account(doc, st.accounts.active);
            st.status = Some("Account saved".into());
        }
        // Connect + fetch the freshly-saved account through the engine.
        if let Some(h) = &self.engine {
            let _ = h.tx.send(EngineRequest::Connect { account: cfg.clone() });
            let _ = h.tx.send(EngineRequest::Fetch { account_id: cfg.email.clone(), folder: Folder::Inbox });
            self.awaiting_engine = true;
        }
        self.close_setup();
    }

    fn overlay_panel_rect(&self) -> Rect {
        let r = self.rect;
        let w = 640.0_f64.min(r.width() - 48.0);
        let h = 540.0_f64.min(r.height() - 48.0);
        let cx = (r.x0 + r.x1) / 2.0;
        let y0 = r.y0 + (r.height() - h) / 2.0;
        Rect::new(cx - w / 2.0, y0, cx + w / 2.0, y0 + h)
    }

    fn rebuild_overlay_hits(&mut self) {
        self.overlay_hits.clear();
        if self.setup_open {
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
                self.overlay_hits.push((fr, OverlayHit::SetupField(i)));
                y += field_h + 10.0;
            }
            y += 6.0;
            let half = (rx - lx - label_w - 12.0) * 0.62;
            let imap_host = Rect::new(lx + label_w, y, lx + label_w + half, y + field_h);
            self.setup_inputs[4].apply_layout_external(imap_host);
            self.overlay_hits.push((imap_host, OverlayHit::SetupField(4)));
            let port_imap = Rect::new(imap_host.x1 + 12.0, y, imap_host.x1 + 12.0 + 64.0, y + field_h);
            self.setup_inputs[5].apply_layout_external(port_imap);
            self.overlay_hits.push((port_imap, OverlayHit::SetupField(5)));
            let imap_tls = Rect::new(port_imap.x1 + 12.0, y, rx, y + field_h);
            self.overlay_hits.push((imap_tls, OverlayHit::ToggleImapTls));
            y += field_h + 10.0;
            let smtp_host = Rect::new(lx + label_w, y, lx + label_w + half, y + field_h);
            self.setup_inputs[6].apply_layout_external(smtp_host);
            self.overlay_hits.push((smtp_host, OverlayHit::SetupField(6)));
            let port_smtp = Rect::new(smtp_host.x1 + 12.0, y, smtp_host.x1 + 12.0 + 64.0, y + field_h);
            self.setup_inputs[7].apply_layout_external(port_smtp);
            self.overlay_hits.push((port_smtp, OverlayHit::SetupField(7)));
            let smtp_tls = Rect::new(port_smtp.x1 + 12.0, y, rx, y + field_h);
            self.overlay_hits.push((smtp_tls, OverlayHit::ToggleSmtpTls));
            let by = p.y1 - 52.0;
            self.overlay_hits.push((Rect::new(lx, by, lx + 150.0, by + 36.0), OverlayHit::SetupSave));
            self.overlay_hits.push((Rect::new(rx - 92.0, by, rx, by + 36.0), OverlayHit::CancelSetup));
        }
        if let Some(menu) = &self.context_menu {
            let mr = context_menu_rect(menu, self.rect);
            let mut y = mr.y0 + 6.0;
            for (i, _a) in menu.items.iter().enumerate() {
                self.overlay_hits.push((Rect::new(mr.x0, y, mr.x1, y + 28.0), OverlayHit::CtxItem(i)));
                y += 28.0;
            }
        }
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
                    }
                    EngineEvent::Fetched { folder, messages, .. } => {
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
                        settled = true;
                    }
                    EngineEvent::Sent { wire_bytes } => {
                        st.status = Some(format!("Delivered ({wire_bytes} bytes)"));
                    }
                    EngineEvent::Error { context, message } => {
                        st.status = Some(format!("{context}: {message}"));
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
        if self.shared.borrow().accounts.is_empty() {
            if let Some(cfg) = MailDoc::load_account(doc) {
                if !cfg.email.is_empty() {
                    self.shared.borrow_mut().accounts.upsert(Account::new(cfg));
                }
            }
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
                st.current_threads().first().and_then(|t| t.message_ids.last().copied())
            });
        }
        // An in-progress persisted draft becomes a visible Drafts message.
        let draft = MailDoc::load_draft(doc);
        if !draft.to.is_empty() || !draft.subject.is_empty() || !draft.body.is_empty() {
            self.upsert_draft_message(doc, &draft);
        }
    }

    // ---- drawing the app-owned overlays ----------------------------------

    fn draw_context_menu(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let Some(menu) = &self.context_menu else { return };
        let c = &theme.tokens.colors;
        let mr = context_menu_rect(menu, self.rect);
        scene.fill(Fill::NonZero, t, Color::from_rgba8(0, 0, 0, 80), None, &RoundedRect::from_rect(Rect::new(mr.x0 + 1.0, mr.y0 + 3.0, mr.x1 + 1.0, mr.y1 + 4.0), 10.0));
        fill_round(scene, t, mr, 10.0, c.surfaces.raised);
        stroke_round(scene, t, mr, 10.0, 1.0, c.border_white_10);
        let (read, starred) = self
            .shared
            .borrow()
            .mailbox
            .get(menu.target)
            .map(|m| (m.read, m.starred))
            .unwrap_or((false, false));
        let last_mouse = self.shared.borrow().last_mouse;
        let mut y = mr.y0 + 6.0;
        for a in &menu.items {
            let item = Rect::new(mr.x0, y, mr.x1, y + 28.0);
            if item.contains(last_mouse) {
                fill_round(scene, t, Rect::new(item.x0 + 4.0, item.y0 + 1.0, item.x1 - 4.0, item.y1 - 1.0), 6.0, c.surface_white_10);
            }
            let danger = matches!(a, CtxAction::Delete);
            let col = if danger { c.semantic.danger } else { c.text_primary };
            tr.draw_text(scene, &a.label(read, starred), 12.5, col, t * Affine::translate((mr.x0 + 14.0, y + 7.0)), None);
            y += 28.0;
        }
    }

    fn draw_setup(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        if !self.setup_open {
            return;
        }
        let c = &theme.tokens.colors;
        fill_rect(scene, t, self.rect, c.surfaces.overlay);
        let p = self.overlay_panel_rect();
        fill_round(scene, t, p, 14.0, c.surfaces.raised);
        stroke_round(scene, t, p, 14.0, 1.0, c.border_white_10);
        let pad = 24.0;
        let lx = p.x0 + pad;
        let heading = if self.setup_editing.is_some() { "Edit account" } else { "Add account" };
        tr.draw_text_weighted(scene, heading, 16.0, c.text_primary, t * Affine::translate((lx, p.y0 + 18.0)), None, TextWeight::Bold);
        tr.draw_text(scene, "IMAP fetch and SMTP submit for this address.", 11.5, c.text_dim, t * Affine::translate((lx, p.y0 + 38.0)), None);

        let labels = ["Display name", "Email", "Username", "Password", "IMAP host", "", "SMTP host", ""];
        let cfg_errs: std::collections::HashSet<&str> = self.read_setup_config().validate().into_iter().map(|(k, _)| k).collect();
        let err_for = |i: usize| match i {
            1 => cfg_errs.contains("email"),
            2 => cfg_errs.contains("username"),
            4 => cfg_errs.contains("imap_host"),
            6 => cfg_errs.contains("smtp_host"),
            _ => false,
        };
        let last_mouse = self.shared.borrow().last_mouse;
        for (rect, hit) in &self.overlay_hits {
            let OverlayHit::SetupField(i) = hit else { continue };
            let i = *i;
            if i >= self.setup_inputs.len() {
                continue;
            }
            let focused = self.setup_focus == Some(i);
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
        for (rect, hit) in &self.overlay_hits {
            let (on, label) = match hit {
                OverlayHit::ToggleImapTls => (self.setup_imap_tls, "TLS"),
                OverlayHit::ToggleSmtpTls => (self.setup_smtp_tls, "TLS"),
                _ => continue,
            };
            let pill = Rect::new(rect.x0, rect.y0 + 4.0, rect.x0 + 44.0, rect.y0 + 26.0);
            fill_round(scene, t, pill, 11.0, if on { c.accent[5] } else { c.neutral[3] });
            let knob_x = if on { pill.x1 - 18.0 } else { pill.x0 + 4.0 };
            scene.fill(Fill::NonZero, t, Color::WHITE, None, &Circle::new(Point::new(knob_x + 7.0, pill.y0 + 11.0), 8.0));
            tr.draw_text(scene, label, 12.0, c.neutral[8], t * Affine::translate((pill.x1 + 8.0, rect.y0 + 8.0)), None);
        }
        for (rect, hit) in &self.overlay_hits {
            match hit {
                OverlayHit::SetupSave => {
                    let hov = rect.contains(last_mouse);
                    fill_round(scene, t, *rect, 9.0, if hov { c.accent[4] } else { c.accent[5] });
                    let label = "Save account";
                    let (w, _) = tr.measure_weighted(label, 13.5, None, TextWeight::Semibold);
                    tr.draw_text_weighted(scene, label, 13.5, Color::WHITE, t * Affine::translate(((rect.x0 + rect.x1) / 2.0 - w as f64 / 2.0, rect.y0 + 10.0)), None, TextWeight::Semibold);
                }
                OverlayHit::CancelSetup => {
                    let hov = rect.contains(last_mouse);
                    fill_round(scene, t, *rect, 9.0, if hov { c.surface_white_10 } else { c.surface_white_5 });
                    let (w, _) = tr.measure("Cancel", 13.0, None);
                    tr.draw_text(scene, "Cancel", 13.0, c.neutral[8], t * Affine::translate(((rect.x0 + rect.x1) / 2.0 - w as f64 / 2.0, rect.y0 + 10.0)), None);
                }
                _ => {}
            }
        }
        for inp in &mut self.setup_inputs {
            inp.draw(scene, t, tr, theme, presence);
        }
    }

    fn draw_status(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let st = self.shared.borrow();
        let Some(msg) = &st.status else { return };
        let c = &theme.tokens.colors;
        let (w, _) = tr.measure(msg, 12.5, None);
        let toast = Rect::new(self.rect.x1 - w as f64 - 64.0, self.rect.y1 - 52.0, self.rect.x1 - 24.0, self.rect.y1 - 16.0);
        fill_round(scene, t, toast, 10.0, c.neutral[2]);
        stroke_round(scene, t, toast, 10.0, 1.0, c.border_white_10);
        tr.draw_text(scene, "\u{2713}", 13.0, c.semantic.success, t * Affine::translate((toast.x0 + 12.0, toast.y0 + 11.0)), None);
        tr.draw_text(scene, msg, 12.5, c.text_primary, t * Affine::translate((toast.x0 + 32.0, toast.y0 + 11.0)), None);
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
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Attachment::projected(name, mime, size)
}

/// Reconstruct an editable [`Draft`] from a Drafts-folder message.
fn draft_from_message(m: &Message) -> Draft {
    let join = |addrs: &[Addr]| addrs.iter().map(|a| a.rfc5322()).collect::<Vec<_>>().join(", ");
    Draft {
        to: join(&m.to),
        cc: join(&m.cc),
        bcc: String::new(),
        subject: if m.subject == "(no subject)" { String::new() } else { m.subject.clone() },
        body: m.body.clone(),
        html_body: None,
        attachments: Vec::new(),
        in_reply_to: None,
    }
}

/// Load the shared account store from disk (`~/.config/qos/mail/accounts.json`),
/// or an empty store when it isn't there yet.
fn load_account_store() -> AccountStore {
    let raw_path = AccountStore::SETTINGS_PATH;
    let expanded = if let Some(rest) = raw_path.strip_prefix("~/") {
        match std::env::var("HOME") {
            Ok(home) => format!("{home}/{rest}"),
            Err(_) => return AccountStore::new(),
        }
    } else {
        raw_path.to_string()
    };
    std::fs::read_to_string(&expanded)
        .ok()
        .and_then(|s| AccountStore::from_json(&s))
        .unwrap_or_else(AccountStore::new)
}

/// The shared image store used for sender avatars.
pub fn avatar_store() -> ImageStore {
    ImageStore::new(avatar_fetcher())
}

/// Real Gravatar HTTP under `--features live`; a no-op otherwise so the default
/// build and `--shots` never open a socket (avatars then render as initials).
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
    std::sync::Arc::new(|_url: &str| None)
}

// =============================== Widget impl ===============================

impl Widget for MailApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        // Drain any engine results first so freshly-fetched messages are present
        // before layout/draw.
        self.drain_engine(state);

        let mem_seeded = self.shared.borrow().seeded;
        if !mem_seeded {
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
                    let _ = h.tx.send(EngineRequest::Connect { account: cfg.clone() });
                    let _ = h.tx.send(EngineRequest::Fetch { account_id: cfg.email.clone(), folder: Folder::Inbox });
                    self.awaiting_engine = true;
                }
                self.connect_requested = true;
            }
            self.shared.borrow_mut().seeded = true;
        } else {
            let mut st = self.shared.borrow_mut();
            MailDoc::apply_flags(state, &mut st.mailbox);
        }
        self.nav.sync(state, path);
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: TSize { width: Dimension::percent(1.0), height: Dimension::percent(1.0) },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        self.layout_nav();
    }

    fn draw(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        self.draw_with_presence(scene, t, tr, theme, presence, &PermissionContext::permissive());
    }

    fn draw_with_presence(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx, permissions: &PermissionContext) {
        let c = &theme.tokens.colors;
        fill_rect(scene, t, self.rect, c.surfaces.base);
        // The navigation shell (sidebar + list + reader) draws itself, its
        // dividers, the collapse/back affordance, and the drawer scrim.
        self.nav.draw_with_presence(scene, t, tr, theme, presence, permissions);
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

        // 2) Esc dismisses app-owned overlays first.
        if let WindowEvent::KeyboardInput { event: KeyEvent { logical_key: Key::Named(NamedKey::Escape), state: ElementState::Pressed, .. }, .. } = event {
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
        if self.context_menu.is_some() {
            if let WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } = event {
                let pt = self.shared.borrow().last_mouse;
                self.rebuild_overlay_hits();
                let item = self.overlay_hits.iter().rev().find(|(rc, h)| matches!(h, OverlayHit::CtxItem(_)) && rc.contains(pt)).cloned();
                if let Some((_, OverlayHit::CtxItem(i))) = item {
                    if let Some(menu) = self.context_menu.take() {
                        if let Some(action) = menu.items.get(i).copied() {
                            self.shared.borrow_mut().apply_message_action(state, menu.target, action);
                            self.process_intents();
                        }
                    }
                } else {
                    self.context_menu = None;
                }
                *handled = true;
                return;
            }
        }

        // 4) Setup sheet is modal while open.
        if self.setup_open {
            self.handle_setup_event(event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy);
            if *handled {
                self.dirty = true;
                return;
            }
        }

        // 5) Route to the navigation shell (affordance/scrim/Esc + the panes).
        self.nav.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy);
        // Panes raised intents; apply the cross-pane effects.
        self.process_intents();

        // 6) If nothing consumed it, apply global mailbox shortcuts.
        if !*handled {
            self.global_shortcuts(event, state, handled);
        }

        self.dirty = true;
    }

    fn name(&self) -> &str {
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
        } else {
            None
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.shared.borrow().dirty
            || self.nav.needs_redraw()
            || self.compose.needs_redraw()
            || self.setup_inputs.iter().any(|i| i.needs_redraw())
    }

    fn needs_animation_frame(&self) -> bool {
        self.nav.needs_animation_frame()
    }

    fn needs_idle_sync(&self) -> bool {
        // Only keep syncing while a connect/fetch reply is still outstanding, so
        // the app idles instead of busy-looping once results have arrived.
        self.awaiting_engine || self.nav.needs_idle_sync()
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
        // Keyboard → focused field.
        if matches!(event, WindowEvent::KeyboardInput { .. }) {
            if let Some(i) = self.setup_focus.and_then(|i| self.setup_inputs.get_mut(i)) {
                i.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy);
            }
            return;
        }
        if let WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } = event {
            let pt = self.shared.borrow().last_mouse;
            self.rebuild_overlay_hits();
            let panel = self.overlay_panel_rect();
            let hit = self.overlay_hits.iter().rev().find(|(rc, _)| rc.contains(pt)).map(|(_, h)| h.clone());
            match hit {
                Some(OverlayHit::SetupField(i)) => {
                    let leaving_email = self.setup_focus == Some(1) && i != 1;
                    self.focus_setup(Some(i));
                    if leaving_email {
                        self.maybe_autofill();
                    }
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

    fn global_shortcuts(&mut self, event: &WindowEvent, state: &mut AutoCommit, handled: &mut bool) {
        let search_focused = self.shared.borrow().search_focused;
        if search_focused {
            return;
        }
        let WindowEvent::KeyboardInput { event: KeyEvent { logical_key, state: ElementState::Pressed, .. }, .. } = event else {
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
                    self.shared.borrow_mut().apply_message_action(state, id, CtxAction::Archive);
                    self.process_intents();
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                if let Some(id) = selected {
                    self.shared.borrow_mut().apply_message_action(state, id, CtxAction::Delete);
                    self.process_intents();
                    *handled = true;
                }
            }
            Key::Character(ch) if ch.eq_ignore_ascii_case("s") => {
                if let Some(id) = selected {
                    self.shared.borrow_mut().apply_message_action(state, id, CtxAction::ToggleStar);
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
        WindowEvent::CursorMoved { device_id: synth_device(), position: winit::dpi::PhysicalPosition::new(p.x, p.y) }
    }
    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput { device_id: synth_device(), state: ElementState::Pressed, button: MouseButton::Left }
    }
    fn esc_event() -> WindowEvent {
        named_key(NamedKey::Escape)
    }
    fn named_key(k: NamedKey) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(k);
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput { device_id: synth_device(), event: ev, is_synthetic: false }
    }
    fn char_key(s: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(s.into());
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(s.into());
        WindowEvent::KeyboardInput { device_id: synth_device(), event: ev, is_synthetic: false }
    }

    fn dispatch(app: &mut MailApp, ev: &WindowEvent, doc: &mut AutoCommit) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(ev, 1.0, &Modifiers::default(), doc, &automerge::ROOT, &mut handled, &mut focus, node, &taffy);
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
        assert!(app.nav.sidebar_rect().width() > 1.0, "docked sidebar present");
        assert!(app.nav.overview_rect().x0 >= app.nav.sidebar_rect().x1 - 0.5, "list right of sidebar");
        assert!(app.nav.detail_rect().x0 >= app.nav.overview_rect().x1 - 0.5, "reader right of list");
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
            // First row sits just below the list header.
            Point::new(app.nav.overview_rect().x0 + 120.0, app.nav.overview_rect().y0 + crate::draw::HEADER_H + crate::draw::LIST_ROW_H / 2.0)
        };
        assert!(click_at(&mut app, &mut doc, row_center), "row click consumed");
        app.layout_for_test(560.0, 860.0);
        assert_eq!(app.nav_pane(), NavPane::Detail, "opening a message pushes the detail");
        assert!(app.shared.borrow().selected.is_some());

        // The Back affordance is present; clicking it returns to the overview.
        let back = app.nav.back_button_rect();
        assert!(back.width() > 0.0, "Back affordance shown while detail pushed");
        assert!(click_at(&mut app, &mut doc, back.center()), "back click consumed");
        assert_eq!(app.nav_pane(), NavPane::Overview, "Back returns to the list");
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
        assert!((app.nav.overview_rect().width() - 560.0).abs() < 2.0, "list full-width narrow");
        app.layout_for_test(1280.0, 800.0);
        assert!(!app.shared.borrow().narrow, "wide flag re-derived at 1280");
        assert!((app.nav.detail_rect().x0 - wide_detail_x0).abs() < 2.0, "reflows back to three panes");
    }

    #[test]
    fn wide_sidebar_folder_click_switches_folder() {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        // The Sent row of the active account (see the sidebar layout math).
        let p = Point::new(130.0, 250.0);
        assert!(click_at(&mut app, &mut doc, p), "folder click consumed");
        assert_eq!(app.shared.borrow().folder, Folder::Sent, "clicking Sent switches the folder");
    }

    #[test]
    fn narrow_hamburger_opens_and_esc_closes_drawer() {
        let (mut app, mut doc) = app();
        app.layout_for_test(560.0, 860.0);
        let ham = app.nav.collapse_button_rect();
        assert!(ham.width() > 0.0, "hamburger present on the narrow list");
        assert!(click_at(&mut app, &mut doc, ham.center()), "hamburger click consumed");
        assert!(app.nav.is_sidebar_open(), "hamburger opens the drawer");
        // Esc dismisses the open overlay drawer.
        assert!(dispatch(&mut app, &esc_event(), &mut doc), "Esc consumed");
        assert!(!app.nav.is_sidebar_open(), "Esc closes the drawer");
    }

    #[test]
    fn selecting_message_marks_read_and_persists() {
        let (mut app, mut doc) = app();
        let id = {
            let st = app.shared.borrow();
            st.mailbox.messages_in(Folder::Inbox).into_iter().find(|m| !m.read).map(|m| m.id)
        }
        .expect("an unread message");
        app.shared.borrow_mut().select_message(&mut doc, id);
        assert!(app.shared.borrow().mailbox.get(id).unwrap().read);
        let mut fresh = app.shared.borrow().mailbox.clone();
        for m in fresh.messages.values_mut() {
            m.read = false;
        }
        MailDoc::apply_flags(&doc, &mut fresh);
        assert!(fresh.get(id).unwrap().read, "read flag persisted to the CRDT");
    }

    #[test]
    fn compose_send_lands_in_sent() {
        let (mut app, mut doc) = app();
        let before = app.shared.borrow().mailbox.count(Folder::Sent);
        app.compose.open_blank();
        app.compose.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        let draft = crate::model::Draft { to: "dest@there.example".into(), subject: "Hi".into(), body: "Hello".into(), html_body: Some("<p>Hello</p>".into()), ..Default::default() };
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
        assert!(menu.contains(&CtxAction::Archive), "swipe-left action present in menu");
        assert!(menu.contains(&CtxAction::Delete), "swipe-right action present in menu");
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
        app.save_setup(&mut doc);
        assert!(!app.setup_open);
        assert_eq!(app.shared.borrow().accounts.len(), before + 1);
        assert_eq!(app.shared.borrow().accounts.active_config().email, "new@host.example");
    }

    #[test]
    fn live_starts_without_fabricated_messages() {
        // The live constructor must not seed a sample corpus.
        let app = MailApp::new_live();
        assert!(app.shared.borrow().mailbox.messages.is_empty(), "no fabricated startup mail");
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
        app.shared.borrow_mut().apply_message_action(&mut doc, id, CtxAction::ReplyAll);
        app.process_intents();
        assert!(app.compose.open, "reply-all opens the composer");
        let d = app.compose.to_draft();
        let sender = app.shared.borrow().mailbox.get(id).unwrap().from.email.clone();
        assert!(d.to.contains(&sender), "sender goes in To: {}", d.to);
        assert!(!d.cc.is_empty(), "other recipients populate Cc: {}", d.cc);
        assert!(!d.cc.contains("you@qos.example"), "our own address is excluded from Cc: {}", d.cc);
        assert!(!d.cc.contains(&sender), "the sender is not duplicated into Cc: {}", d.cc);
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
        assert_eq!(app.shared.borrow().mailbox.count(Folder::Drafts), before + 1, "the draft appears in Drafts");

        // Reopening the Drafts message re-seeds the composer for editing.
        let id = {
            let st = app.shared.borrow();
            st.mailbox.messages_in(Folder::Drafts).iter().find(|m| m.subject == "WIP proposal").unwrap().id
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
        app.compose.reattach(&crate::model::Draft { subject: "First".into(), body: "one".into(), ..Default::default() });
        app.save_compose_draft(&mut doc);
        let after_first = app.shared.borrow().mailbox.count(Folder::Drafts);
        // Re-open + re-save the same draft.
        let id = app.editing_draft.expect("editing the saved draft");
        app.open_draft(id);
        app.save_compose_draft(&mut doc);
        assert_eq!(app.shared.borrow().mailbox.count(Folder::Drafts), after_first, "re-saving updates in place");
    }

    #[test]
    fn session_state_restores_from_persisted_document() {
        let mut doc = AutoCommit::new();
        {
            let mut app = MailApp::new_sample();
            app.sync(&mut doc, &automerge::ROOT); // seeds the document
            app.shared.borrow_mut().select_folder(&mut doc, Folder::Sent);
            app.shared.borrow_mut().set_active_account(&mut doc, 1);
            app.compose.set_format(ComposeFormat::Text);
            app.persist_compose_format(&mut doc);
        }
        // A second app opening the same document must restore, not overwrite.
        let mut app2 = MailApp::new_sample();
        app2.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app2.shared.borrow().folder, Folder::Sent, "folder restored");
        assert_eq!(app2.shared.borrow().accounts.active, 1, "active account restored");
        assert_eq!(app2.compose.format(), ComposeFormat::Text, "compose format restored");
    }

    #[test]
    fn setup_email_autofills_connection_hosts() {
        let (mut app, _doc) = app();
        app.open_setup_edit(None);
        app.setup_inputs[1].set_text("person@gmail.com");
        app.maybe_autofill();
        assert_eq!(app.setup_inputs[4].value(), "imap.gmail.com", "IMAP host autofilled");
        assert_eq!(app.setup_inputs[6].value(), "smtp.gmail.com", "SMTP host autofilled");
    }

    #[test]
    fn context_menu_click_applies_the_action() {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let id = app.shared.borrow().mailbox.messages_in(Folder::Inbox)[0].id;
        app.shared.borrow_mut().push_intent(Intent::ContextMenu(id, Point::new(400.0, 300.0)));
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
        assert!(click_at(&mut app, &mut doc, rect.center()), "menu item click consumed");
        assert_eq!(app.shared.borrow().mailbox.count(Folder::Archive), before + 1, "clicking Archive archived the message");
        assert!(app.context_menu.is_none(), "menu dismissed after the action");
    }

    #[test]
    fn setup_tls_toggle_flips_the_port() {
        let (mut app, mut doc) = app();
        app.layout_for_test(1100.0, 780.0);
        app.open_setup_edit(None);
        app.rebuild_overlay_hits();
        assert_eq!(app.setup_inputs[5].value(), "993", "IMAP port starts at the TLS default");
        let toggle = app
            .overlay_hits
            .iter()
            .find(|(_, h)| matches!(h, OverlayHit::ToggleImapTls))
            .map(|(r, _)| *r)
            .expect("imap tls toggle");
        assert!(click_at(&mut app, &mut doc, toggle.center()), "tls toggle consumed");
        assert!(!app.setup_imap_tls, "TLS turned off");
        assert_eq!(app.setup_inputs[5].value(), "143", "port switched to the plaintext IMAP port");
    }

    #[test]
    fn shortcut_c_opens_compose() {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        assert!(!app.compose.open);
        assert!(dispatch(&mut app, &char_key("c"), &mut doc), "c shortcut consumed");
        assert!(app.compose.open, "'c' opened the composer");
    }

    #[test]
    fn shortcut_e_archives_selected() {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let before = app.shared.borrow().mailbox.count(Folder::Archive);
        assert!(app.shared.borrow().selected.is_some());
        dispatch(&mut app, &char_key("e"), &mut doc);
        assert_eq!(app.shared.borrow().mailbox.count(Folder::Archive), before + 1, "'e' archived the selection");
    }

    #[test]
    fn shortcut_arrow_down_moves_selection() {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let first = app.shared.borrow().selected;
        dispatch(&mut app, &named_key(NamedKey::ArrowDown), &mut doc);
        assert_ne!(app.shared.borrow().selected, first, "ArrowDown moved the selection");
    }

    #[test]
    fn shortcut_delete_trashes_selected() {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let before = app.shared.borrow().mailbox.count(Folder::Trash);
        dispatch(&mut app, &named_key(NamedKey::Delete), &mut doc);
        assert_eq!(app.shared.borrow().mailbox.count(Folder::Trash), before + 1, "Delete moved the selection to Trash");
    }
}
