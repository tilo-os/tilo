//! `MailApp` — the root widget. Owns the in-memory [`Mailbox`], the UI state,
//! and renders the three-pane client (folder sidebar · threaded message list ·
//! reader), plus the Compose and Account-Setup overlays. It draws most chrome
//! directly into the Vello scene (data-dense rows are far easier to control
//! that way) and embeds `ui::TextInput`s for every text-entry field.
//!
//! State that must persist / sync / undo is mirrored into the Automerge doc
//! through [`crate::doc::MailDoc`]: per-message flags, the selected folder /
//! view mode / search query / selection, the account config, and the active
//! draft. The immutable message corpus is hydrated into [`Mailbox`] (from the
//! live IMAP fetch, or the bundled sample corpus for `--shots`).

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

use crate::doc::MailDoc;
use crate::model::{AccountConfig, Addr, Draft, Folder, Mailbox, Message, MessageId};
use crate::transport::{build_mime, parse_addr_list};
use crate::util::{format_absolute, format_relative};

const TOPBAR_H: f64 = 56.0;
const SIDEBAR_W: f64 = 236.0;
const LIST_W: f64 = 404.0;
const FOLDER_ROW_H: f64 = 38.0;
const LIST_ROW_H: f64 = 78.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Mailbox,
    Compose,
    Setup,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    None,
    Search,
    ComposeTo,
    ComposeCc,
    ComposeBcc,
    ComposeSubject,
    ComposeBody,
    Setup(usize),
}

/// Identifies an actionable region resolved during layout, so `handle_event`
/// hit-testing and `draw` agree on geometry.
#[derive(Clone)]
enum Hit {
    Folder(Folder),
    Thread(MessageId),
    ThreadStar(MessageId),
    Compose,
    Settings,
    SearchClear,
    Reply,
    ArchiveSel,
    DeleteSel,
    StarSel,
    UnreadSel,
    Send,
    SaveDraft,
    CancelOverlay,
    SetupField(usize),
    ToggleImapTls,
    ToggleSmtpTls,
    SetupSave,
    AddAttachment,
}

pub struct MailApp {
    mailbox: Mailbox,
    now_min: i64,
    mode: Mode,
    folder: Folder,
    selected: Option<MessageId>,
    focus: Focus,
    search: String,
    seeded: bool,

    // embedded inputs
    search_input: TextInput,
    c_to: TextInput,
    c_cc: TextInput,
    c_bcc: TextInput,
    c_subject: TextInput,
    compose_body: String,
    compose_atts: Vec<String>,
    show_cc_bcc: bool,

    // setup fields: name, email, username, password, imap_host, imap_port,
    // smtp_host, smtp_port
    setup_inputs: Vec<TextInput>,
    setup_imap_tls: bool,
    setup_smtp_tls: bool,

    // toast / status line
    status: Option<String>,

    // layout caches
    rect: Rect,
    hits: Vec<(Rect, Hit)>,
    list_rect: Rect,
    reader_rect: Rect,
    last_mouse: Point,
    dirty: bool,
}

impl MailApp {
    pub fn new_sample() -> Self {
        let mailbox = crate::sample::sample_mailbox();
        let now_min = crate::sample::SAMPLE_NOW_MIN;
        Self::with_mailbox(mailbox, now_min)
    }

    fn with_mailbox(mailbox: Mailbox, now_min: i64) -> Self {
        let mut search_input = TextInput::local();
        search_input.set_plain_chrome(true);
        let mk = || {
            let mut t = TextInput::local();
            t.set_plain_chrome(true);
            t
        };
        let cfg = &mailbox.account;
        let setup_inputs = vec![
            TextInput::local().with_initial_value(&cfg.display_name),
            TextInput::local().with_initial_value(&cfg.email),
            TextInput::local().with_initial_value(&cfg.username),
            TextInput::local()
                .with_initial_value(&cfg.password)
                .with_secret(true),
            TextInput::local().with_initial_value(&cfg.imap_host),
            TextInput::local().with_initial_value(&cfg.imap_port.to_string()),
            TextInput::local().with_initial_value(&cfg.smtp_host),
            TextInput::local().with_initial_value(&cfg.smtp_port.to_string()),
        ];

        let mut app = Self {
            now_min,
            mode: Mode::Mailbox,
            folder: Folder::Inbox,
            selected: None,
            focus: Focus::None,
            search: String::new(),
            seeded: false,
            search_input,
            c_to: mk(),
            c_cc: mk(),
            c_bcc: mk(),
            c_subject: mk(),
            compose_body: String::new(),
            compose_atts: Vec::new(),
            show_cc_bcc: false,
            setup_imap_tls: cfg.imap_tls,
            setup_smtp_tls: cfg.smtp_tls,
            setup_inputs,
            status: None,
            rect: Rect::ZERO,
            hits: Vec::new(),
            list_rect: Rect::ZERO,
            reader_rect: Rect::ZERO,
            last_mouse: Point::ZERO,
            dirty: true,
            mailbox,
        };
        // Preselect a conversation so the reader is populated on launch /
        // in the headless screenshot. Prefer the most-recent multi-message
        // thread (richer to read); fall back to the newest single message.
        let threads = app.current_threads();
        let pick = threads
            .iter()
            .find(|t| t.message_ids.len() > 1)
            .or_else(|| threads.first());
        if let Some(&newest) = pick.and_then(|t| t.message_ids.last()) {
            app.selected = Some(newest);
            app.mailbox.mark_read(newest, true);
        }
        app
    }

    // ----- derived views -----

    fn current_threads(&self) -> Vec<crate::model::Thread> {
        self.mailbox.threads_in(self.folder)
    }

    /// The rows shown in the list pane: either search hits (flat) or the
    /// folder's threads (collapsed). Returns (message-id, thread-len).
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

    /// Messages of the thread the selected message belongs to, oldest→newest.
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
        MailDoc::store_account(state, &self.mailbox.account);
        MailDoc::set_folder(state, self.folder);
        MailDoc::set_mode(state, "mailbox");
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
        self.dirty = true;
    }

    fn select_folder(&mut self, state: &mut AutoCommit, folder: Folder) {
        self.folder = folder;
        self.search.clear();
        self.search_input.set_text("");
        MailDoc::set_folder(state, folder);
        // Reselect the top of the new folder.
        self.selected = self
            .current_threads()
            .first()
            .and_then(|t| t.message_ids.last().copied());
        if let Some(sel) = self.selected {
            MailDoc::set_selected(state, Some(&sel.to_string()));
        }
        self.dirty = true;
    }

    fn open_compose_new(&mut self, state: &mut AutoCommit) {
        self.mode = Mode::Compose;
        self.c_to.set_text("");
        self.c_cc.set_text("");
        self.c_bcc.set_text("");
        self.c_subject.set_text("");
        self.compose_body.clear();
        self.compose_atts.clear();
        self.show_cc_bcc = false;
        self.focus_only(Focus::ComposeTo);
        MailDoc::set_mode(state, "compose");
        self.dirty = true;
    }

    fn open_reply(&mut self, state: &mut AutoCommit) {
        let Some(msg) = self.selected.and_then(|id| self.mailbox.get(id)).cloned() else {
            return;
        };
        self.mode = Mode::Compose;
        self.c_to.set_text(&msg.from.rfc5322());
        self.c_cc.set_text("");
        self.c_bcc.set_text("");
        let subj = if msg.subject.to_ascii_lowercase().starts_with("re:") {
            msg.subject.clone()
        } else {
            format!("Re: {}", msg.subject)
        };
        self.c_subject.set_text(&subj);
        self.compose_body = format!(
            "\n\nOn {}, {} wrote:\n> {}",
            format_absolute(msg.timestamp_min),
            msg.from.display(),
            msg.snippet()
        );
        self.compose_atts.clear();
        self.show_cc_bcc = false;
        self.focus_only(Focus::ComposeBody);
        MailDoc::set_mode(state, "compose");
        self.dirty = true;
    }

    fn current_draft(&self) -> Draft {
        Draft {
            to: self.c_to.value().to_string(),
            cc: self.c_cc.value().to_string(),
            bcc: self.c_bcc.value().to_string(),
            subject: self.c_subject.value().to_string(),
            body: self.compose_body.clone(),
            attachments: self.compose_atts.clone(),
            in_reply_to: None,
        }
    }

    fn send_compose(&mut self, state: &mut AutoCommit) {
        let draft = self.current_draft();
        let from = Addr::new(
            self.mailbox.account.display_name.clone(),
            self.mailbox.account.email.clone(),
        );
        match build_mime(&from, &draft) {
            Ok(bytes) => {
                // Land the sent message in the Sent folder so it shows up
                // immediately (the live SMTP submit happens on a worker).
                let to = parse_addr_list(&draft.to);
                let msg = Message {
                    id: uuid::Uuid::new_v4(),
                    rfc_message_id: None,
                    in_reply_to: None,
                    from,
                    to,
                    cc: parse_addr_list(&draft.cc),
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
                self.status = Some(format!("Sent ({} bytes)", bytes.len()));
            }
            Err(e) => self.status = Some(format!("Send failed: {e}")),
        }
        self.close_overlay(state);
    }

    fn save_draft(&mut self, state: &mut AutoCommit) {
        let draft = self.current_draft();
        MailDoc::store_draft(state, &draft);
        self.status = Some("Draft saved".into());
        self.close_overlay(state);
    }

    fn close_overlay(&mut self, state: &mut AutoCommit) {
        self.mode = Mode::Mailbox;
        self.focus_only(Focus::None);
        MailDoc::set_mode(state, "mailbox");
        self.dirty = true;
    }

    fn open_setup(&mut self, state: &mut AutoCommit) {
        self.mode = Mode::Setup;
        self.focus_only(Focus::Setup(1)); // email field
        MailDoc::set_mode(state, "setup");
        self.dirty = true;
    }

    fn save_setup(&mut self, state: &mut AutoCommit) {
        let cfg = self.read_setup_config();
        if cfg.is_valid() {
            self.mailbox.account = cfg.clone();
            MailDoc::store_account(state, &cfg);
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

    fn focus_only(&mut self, focus: Focus) {
        self.focus = focus;
        self.search_input.set_focus(matches!(focus, Focus::Search));
        self.c_to.set_focus(matches!(focus, Focus::ComposeTo));
        self.c_cc.set_focus(matches!(focus, Focus::ComposeCc));
        self.c_bcc.set_focus(matches!(focus, Focus::ComposeBcc));
        self.c_subject.set_focus(matches!(focus, Focus::ComposeSubject));
        for (i, inp) in self.setup_inputs.iter_mut().enumerate() {
            inp.set_focus(matches!(focus, Focus::Setup(j) if j == i));
        }
        self.dirty = true;
    }

    fn focused_input(&mut self) -> Option<&mut TextInput> {
        match self.focus {
            Focus::Search => Some(&mut self.search_input),
            Focus::ComposeTo => Some(&mut self.c_to),
            Focus::ComposeCc => Some(&mut self.c_cc),
            Focus::ComposeBcc => Some(&mut self.c_bcc),
            Focus::ComposeSubject => Some(&mut self.c_subject),
            Focus::Setup(i) => self.setup_inputs.get_mut(i),
            _ => None,
        }
    }
}

// =============================== layout ===============================

impl MailApp {
    fn relayout(&mut self) {
        self.hits.clear();
        let r = self.rect;
        let list_x0 = r.x0 + SIDEBAR_W;
        let reader_x0 = list_x0 + LIST_W;
        self.list_rect = Rect::new(list_x0, r.y0 + TOPBAR_H, reader_x0, r.y1);
        self.reader_rect = Rect::new(reader_x0, r.y0 + TOPBAR_H, r.x1, r.y1);

        // Sidebar: compose button + folder rows.
        let pad = 12.0;
        let mut y = r.y0 + 72.0; // below account header
        self.hits.push((
            Rect::new(r.x0 + pad, y, r.x0 + SIDEBAR_W - pad, y + 40.0),
            Hit::Compose,
        ));
        y += 40.0 + 16.0;
        for f in Folder::ALL {
            let row = Rect::new(r.x0 + 8.0, y, r.x0 + SIDEBAR_W - 8.0, y + FOLDER_ROW_H);
            self.hits.push((row, Hit::Folder(f)));
            y += FOLDER_ROW_H;
        }

        // Top bar: settings gear + search clear.
        let gear = Rect::new(r.x1 - 44.0, r.y0 + 12.0, r.x1 - 12.0, r.y0 + 44.0);
        self.hits.push((gear, Hit::Settings));
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

        // List rows.
        let rows = self.list_rows();
        let mut ly = self.list_rect.y0;
        for (id, _len) in rows {
            let row = Rect::new(self.list_rect.x0, ly, self.list_rect.x1, ly + LIST_ROW_H);
            if row.y1 <= self.list_rect.y1 + LIST_ROW_H {
                // star hit zone (right edge)
                let star = Rect::new(row.x1 - 36.0, row.y0 + 8.0, row.x1 - 12.0, row.y0 + 32.0);
                self.hits.push((star, Hit::ThreadStar(id)));
                self.hits.push((row, Hit::Thread(id)));
            }
            ly += LIST_ROW_H;
        }

        // Reader action bar.
        if self.selected.is_some() {
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

        if self.mode == Mode::Compose {
            self.layout_compose();
        } else if self.mode == Mode::Setup {
            self.layout_setup();
        }
    }

    fn search_box_rect(&self) -> Rect {
        let r = self.rect;
        let x1 = r.x1 - 56.0;
        let x0 = (x1 - 280.0).max(r.x0 + SIDEBAR_W + 16.0);
        Rect::new(x0, r.y0 + 12.0, x1, r.y0 + 44.0)
    }

    fn overlay_panel_rect(&self) -> Rect {
        let r = self.rect;
        let w = 640.0_f64.min(r.width() - 80.0);
        let h: f64 = if self.mode == Mode::Setup { 540.0 } else { 480.0 };
        let h = h.min(r.height() - 60.0);
        let cx = (r.x0 + r.x1) / 2.0;
        let y0 = r.y0 + 56.0;
        Rect::new(cx - w / 2.0, y0, cx + w / 2.0, y0 + h)
    }

    fn layout_compose(&mut self) {
        let p = self.overlay_panel_rect();
        let pad = 20.0;
        let lx = p.x0 + pad;
        let rx = p.x1 - pad;
        let field_h = 34.0;
        let mut y = p.y0 + 60.0;
        // To
        self.c_to.apply_layout_external(Rect::new(lx + 56.0, y, rx, y + field_h));
        y += field_h + 8.0;
        if self.show_cc_bcc {
            self.c_cc.apply_layout_external(Rect::new(lx + 56.0, y, rx, y + field_h));
            y += field_h + 8.0;
            self.c_bcc.apply_layout_external(Rect::new(lx + 56.0, y, rx, y + field_h));
            y += field_h + 8.0;
        }
        // Subject
        self.c_subject.apply_layout_external(Rect::new(lx + 56.0, y, rx, y + field_h));
        y += field_h + 10.0;
        // body region is the rest; compose body caret region implied.
        let _ = y;
        // Buttons at bottom.
        let by = p.y1 - 52.0;
        self.hits.push((Rect::new(lx, by, lx + 92.0, by + 36.0), Hit::Send));
        self.hits.push((Rect::new(lx + 100.0, by, lx + 100.0 + 120.0, by + 36.0), Hit::SaveDraft));
        self.hits.push((Rect::new(rx - 92.0, by, rx, by + 36.0), Hit::CancelOverlay));
        // attach button (left of body, near subject row right)
        self.hits.push((Rect::new(rx - 130.0, p.y0 + 18.0, rx, p.y0 + 44.0), Hit::AddAttachment));
        // focus targets for header fields
        self.hits.push((Rect::new(lx + 56.0, p.y0 + 60.0, rx, p.y0 + 60.0 + field_h), Hit::SetupField(usize::MAX))); // placeholder no-op
    }

    fn layout_setup(&mut self) {
        let p = self.overlay_panel_rect();
        let pad = 24.0;
        let lx = p.x0 + pad;
        let rx = p.x1 - pad;
        let field_h = 34.0;
        let label_w = 132.0;
        let mut y = p.y0 + 64.0;
        // 0 name, 1 email, 2 username, 3 password
        for i in 0..4 {
            let fr = Rect::new(lx + label_w, y, rx, y + field_h);
            self.setup_inputs[i].apply_layout_external(fr);
            self.hits.push((fr, Hit::SetupField(i)));
            y += field_h + 10.0;
        }
        y += 6.0;
        // IMAP host + port + tls
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
        // SMTP host + port + tls
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

/// Trim `text` to a single line that fits `max_w` px at the given size/weight,
/// appending an ellipsis. Returns the original when it already fits. Draw the
/// result with `max_width = None` so it never wraps.
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

/// Stable pseudo-random avatar tint from an email, drawn from the accent +
/// semantic palette so it always reads as on-theme.
fn avatar_color(theme: &Theme, email: &str) -> Color {
    let c = &theme.tokens.colors;
    let palette = [
        c.accent[5],
        c.semantic.success,
        c.semantic.info,
        c.semantic.warning,
        c.accent[6],
        c.accent[3],
        Color::from_rgb8(217, 70, 239), // fuchsia-500
        Color::from_rgb8(20, 184, 166), // teal-500
    ];
    let h = email.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    palette[(h as usize) % palette.len()]
}

impl MailApp {
    fn draw_avatar(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        center: Point,
        radius: f64,
        addr: &Addr,
    ) {
        let col = avatar_color(&Theme::dark(), &addr.email);
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
        let c = &theme.tokens.colors;
        let r = self.rect;
        let sb = Rect::new(r.x0, r.y0, r.x0 + SIDEBAR_W, r.y1);
        fill_rect(scene, t, sb, c.neutral[0]);
        // right divider
        fill_rect(scene, t, Rect::new(sb.x1 - 1.0, sb.y0, sb.x1, sb.y1), c.border_standard);

        // account header
        let acct = &self.mailbox.account;
        let av_c = Point::new(sb.x0 + 28.0, sb.y0 + 30.0);
        self.draw_avatar(
            scene,
            tr,
            t,
            av_c,
            16.0,
            &Addr::new(acct.display_name.clone(), acct.email.clone()),
        );
        let name = if acct.display_name.is_empty() { "No account" } else { &acct.display_name };
        tr.draw_text_weighted(
            scene,
            name,
            14.0,
            c.text_primary,
            t * Affine::translate((sb.x0 + 52.0, sb.y0 + 14.0)),
            Some(SIDEBAR_W as f32 - 64.0),
            TextWeight::Semibold,
        );
        tr.draw_text(
            scene,
            &acct.email,
            11.5,
            c.text_dim,
            t * Affine::translate((sb.x0 + 52.0, sb.y0 + 32.0)),
            Some(SIDEBAR_W as f32 - 64.0),
        );

        // compose button
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

        // folder rows
        for (rect, hit) in &self.hits {
            let Hit::Folder(f) = hit else { continue };
            let f = *f;
            let selected = f == self.folder && self.mode == Mode::Mailbox;
            let hov = rect.contains(self.last_mouse);
            if selected {
                fill_round(scene, t, *rect, 9.0, c.accent[5].with_alpha_factor(0.20));
                fill_rect(
                    scene,
                    t,
                    Rect::new(rect.x0, rect.y0 + 7.0, rect.x0 + 3.0, rect.y1 - 7.0),
                    c.accent[5],
                );
            } else if hov {
                fill_round(scene, t, *rect, 9.0, c.surface_white_5);
            }
            let ty = rect.y0 + (FOLDER_ROW_H - 14.0) / 2.0;
            let label_col = if selected { c.text_primary } else { c.neutral[7] };
            tr.draw_text(scene, f.glyph(), 14.0, label_col, t * Affine::translate((rect.x0 + 14.0, ty)), None);
            tr.draw_text_weighted(
                scene,
                f.title(),
                13.5,
                label_col,
                t * Affine::translate((rect.x0 + 40.0, ty)),
                Some(120.0),
                if selected { TextWeight::Semibold } else { TextWeight::Regular },
            );
            // unread badge
            let unread = self.mailbox.unread_count(f);
            if unread > 0 {
                let txt = unread.to_string();
                let (tw, _) = tr.measure_weighted(&txt, 11.0, None, TextWeight::Semibold);
                let bw = (tw as f64 + 14.0).max(20.0);
                let badge = Rect::new(rect.x1 - bw - 8.0, rect.y0 + 9.0, rect.x1 - 8.0, rect.y0 + 9.0 + 20.0);
                let bg = if selected { c.accent[5] } else { c.neutral[3] };
                fill_round(scene, t, badge, 10.0, bg);
                tr.draw_text_weighted(
                    scene,
                    &txt,
                    11.0,
                    Color::WHITE,
                    t * Affine::translate(((badge.x0 + badge.x1) / 2.0 - tw as f64 / 2.0, badge.y0 + 4.0)),
                    None,
                    TextWeight::Semibold,
                );
            }
        }

        // footer: storage hint
        let foot_y = sb.y1 - 26.0;
        tr.draw_text(
            scene,
            "qOS Mail · CRDT-synced",
            10.5,
            c.text_dim,
            t * Affine::translate((sb.x0 + 16.0, foot_y)),
            Some(SIDEBAR_W as f32 - 24.0),
        );
    }

    fn draw_topbar(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        let c = &theme.tokens.colors;
        let r = self.rect;
        let bar = Rect::new(r.x0 + SIDEBAR_W, r.y0, r.x1, r.y0 + TOPBAR_H);
        fill_rect(scene, t, bar, c.surfaces.base);
        fill_rect(scene, t, Rect::new(bar.x0, bar.y1 - 1.0, bar.x1, bar.y1), c.border_standard);

        // folder title + count
        let title = self.folder.title();
        tr.draw_text_weighted(
            scene,
            title,
            18.0,
            c.text_primary,
            t * Affine::translate((bar.x0 + 20.0, bar.y0 + 16.0)),
            None,
            TextWeight::Bold,
        );
        let (tw, _) = tr.measure_weighted(title, 18.0, None, TextWeight::Bold);
        let count = self.mailbox.count(self.folder);
        tr.draw_text(
            scene,
            &format!("{count} conversations"),
            12.0,
            c.text_dim,
            t * Affine::translate((bar.x0 + 28.0 + tw as f64, bar.y0 + 21.0)),
            None,
        );

        // search box
        let sr = self.search_box_rect();
        fill_round(scene, t, sr, 9.0, c.surfaces.raised);
        stroke_round(scene, t, sr, 9.0, 1.0, if matches!(self.focus, Focus::Search) { c.border_focus } else { c.border_standard });
        tr.draw_text(scene, "\u{1F50D}", 13.0, c.text_dim, t * Affine::translate((sr.x0 + 9.0, sr.y0 + 9.0)), None);
        if self.search.is_empty() && !matches!(self.focus, Focus::Search) {
            tr.draw_text(scene, "Search mail", 13.0, c.text_dim, t * Affine::translate((sr.x0 + 30.0, sr.y0 + 9.0)), None);
        }
        // embedded input draws its own text/caret
        // (drawn from the main draw fn after this; here just chrome)
        let _ = presence;
        if !self.search.is_empty() {
            tr.draw_text(scene, "\u{2715}", 12.0, c.text_dim, t * Affine::translate((sr.x1 - 20.0, sr.y0 + 9.0)), None);
        }

        // settings gear
        if let Some((gr, _)) = self.hits.iter().find(|(_, h)| matches!(h, Hit::Settings)) {
            let hov = gr.contains(self.last_mouse);
            if hov {
                fill_round(scene, t, *gr, 8.0, c.surface_white_5);
            }
            tr.draw_text(scene, "\u{2699}", 18.0, c.neutral[7], t * Affine::translate((gr.x0 + 6.0, gr.y0 + 5.0)), None);
        }
    }

    fn draw_list(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        fill_rect(scene, t, self.list_rect, c.surfaces.base);
        fill_rect(
            scene,
            t,
            Rect::new(self.list_rect.x1 - 1.0, self.list_rect.y0, self.list_rect.x1, self.list_rect.y1),
            c.border_standard,
        );

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
            if selected {
                fill_rect(scene, t, *rect, c.accent[5].with_alpha_factor(0.16));
                fill_rect(scene, t, Rect::new(rect.x0, rect.y0, rect.x0 + 3.0, rect.y1), c.accent[5]);
            } else if hov {
                fill_rect(scene, t, *rect, c.surface_white_5);
            }
            // bottom divider
            fill_rect(scene, t, Rect::new(rect.x0 + 12.0, rect.y1 - 1.0, rect.x1, rect.y1), c.neutral[2]);

            let unread = !msg.read;
            // unread dot
            if unread {
                scene.fill(Fill::NonZero, t, c.accent[5], None, &Circle::new(Point::new(rect.x0 + 14.0, rect.y0 + LIST_ROW_H / 2.0), 4.0));
            }
            // avatar
            self.draw_avatar(scene, tr, t, Point::new(rect.x0 + 42.0, rect.y0 + 26.0), 17.0, &msg.from);

            let text_x = rect.x0 + 70.0;
            let right = rect.x1 - 14.0;
            // sender (single line, elided)
            let name_weight = if unread { TextWeight::Bold } else { TextWeight::Medium };
            let name = elide(tr, &msg.from.display(), 13.5, name_weight, right - text_x - 56.0);
            tr.draw_text_weighted(
                scene,
                &name,
                13.5,
                c.text_primary,
                t * Affine::translate((text_x, rect.y0 + 12.0)),
                None,
                name_weight,
            );
            // date
            let date = format_relative(msg.timestamp_min, self.now_min);
            let (dw, _) = tr.measure(&date, 11.5, None);
            tr.draw_text(scene, &date, 11.5, c.text_dim, t * Affine::translate((right - dw as f64, rect.y0 + 13.0)), None);

            // subject (+ thread count)
            let subj_y = rect.y0 + 32.0;
            let subj_col = if unread { c.text_primary } else { c.neutral[7] };
            let subj_weight = if unread { TextWeight::Semibold } else { TextWeight::Regular };
            let subj_budget = right - text_x - if len > 1 { 48.0 } else { 16.0 };
            let subject = elide(tr, &msg.thread_subject(), 12.5, subj_weight, subj_budget);
            tr.draw_text_weighted(
                scene,
                &subject,
                12.5,
                subj_col,
                t * Affine::translate((text_x, subj_y)),
                None,
                subj_weight,
            );
            if len > 1 {
                let badge_txt = len.to_string();
                let (bw, _) = tr.measure_weighted(&badge_txt, 10.5, None, TextWeight::Semibold);
                let bx = right - bw as f64 - 12.0;
                let badge = Rect::new(bx - 6.0, subj_y - 1.0, right, subj_y + 15.0);
                fill_round(scene, t, badge, 8.0, c.neutral[3]);
                tr.draw_text_weighted(scene, &badge_txt, 10.5, c.neutral[8], t * Affine::translate((bx, subj_y + 1.0)), None, TextWeight::Semibold);
            }

            // snippet (single line, elided; clears the star/attachment icons)
            let icon_reserve = if msg.has_attachments() { 64.0 } else { 28.0 };
            let snippet = elide(tr, &msg.snippet(), 11.5, TextWeight::Regular, right - text_x - icon_reserve);
            tr.draw_text(
                scene,
                &snippet,
                11.5,
                c.text_dim,
                t * Affine::translate((text_x, rect.y0 + 52.0)),
                None,
            );

            // star + attachment indicators bottom-right
            let star_col = if msg.starred { c.semantic.warning } else { c.neutral[4] };
            tr.draw_text(scene, if msg.starred { "\u{2605}" } else { "\u{2606}" }, 15.0, star_col, t * Affine::translate((right - 18.0, rect.y0 + 50.0)), None);
            if msg.has_attachments() {
                tr.draw_text(scene, "\u{1F4CE}", 12.0, c.text_dim, t * Affine::translate((right - 40.0, rect.y0 + 52.0)), None);
            }
        }
    }

    fn draw_reader(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
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

        // action bar
        self.draw_reader_actions(scene, t, tr, theme);

        // thread subject header
        let head = &msgs[msgs.len() - 1];
        let content_x = rr.x0 + 24.0;
        let content_w = (rr.width() - 48.0) as f32;
        let mut y = rr.y0 + 60.0;
        let subj = elide(tr, &head.thread_subject(), 20.0, TextWeight::Bold, content_w as f64);
        tr.draw_text_weighted(
            scene,
            &subj,
            20.0,
            c.text_primary,
            t * Affine::translate((content_x, y)),
            None,
            TextWeight::Bold,
        );
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

        // each message as a card (clipped to reader height)
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
    fn draw_message_card(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        m: &Message,
        x: f64,
        y: f64,
        w: f32,
        expanded: bool,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let card_x1 = x + w as f64;
        // header
        self.draw_avatar(scene, tr, t, Point::new(x + 18.0, y + 18.0), 17.0, &m.from);
        let from_name = elide(tr, &m.from.display(), 13.5, TextWeight::Semibold, (w - 150.0) as f64);
        tr.draw_text_weighted(scene, &from_name, 13.5, c.text_primary, t * Affine::translate((x + 44.0, y + 6.0)), None, TextWeight::Semibold);
        let from_email = elide(tr, &m.from.email, 11.5, TextWeight::Regular, (w - 150.0) as f64);
        tr.draw_text(scene, &from_email, 11.5, c.text_dim, t * Affine::translate((x + 44.0, y + 24.0)), None);
        let date = format_absolute(m.timestamp_min);
        let (dw, _) = tr.measure(&date, 11.5, None);
        tr.draw_text(scene, &date, 11.5, c.text_dim, t * Affine::translate((card_x1 - dw as f64, y + 8.0)), None);
        let to_line = elide(
            tr,
            &format!("to {}", m.to.iter().map(|a| a.display()).collect::<Vec<_>>().join(", ")),
            11.0,
            TextWeight::Regular,
            150.0,
        );
        tr.draw_text(scene, &to_line, 11.0, c.text_dim, t * Affine::translate((card_x1 - 150.0, y + 24.0)), None);

        let mut yy = y + 48.0;
        if expanded {
            // body: wrap manually by feeding paragraphs with max_width.
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
            // attachments
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
            // collapsed: one-line snippet
            tr.draw_text(scene, &m.snippet(), 12.0, c.text_dim, t * Affine::translate((x, yy)), Some(w - 8.0));
            yy += 18.0;
        }
        yy
    }

    // ----- overlays -----

    fn draw_compose(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        // scrim
        fill_rect(scene, t, self.rect, c.surfaces.overlay);
        let p = self.overlay_panel_rect();
        fill_round(scene, t, p, 14.0, c.surfaces.raised);
        stroke_round(scene, t, p, 14.0, 1.0, c.border_white_10);
        let pad = 20.0;
        let lx = p.x0 + pad;
        let rx = p.x1 - pad;
        tr.draw_text_weighted(scene, "New message", 16.0, c.text_primary, t * Affine::translate((lx, p.y0 + 18.0)), None, TextWeight::Bold);
        // attach button
        if let Some((ar, _)) = self.hits.iter().find(|(_, h)| matches!(h, Hit::AddAttachment)) {
            let hov = ar.contains(self.last_mouse);
            fill_round(scene, t, *ar, 8.0, if hov { c.surface_white_10 } else { c.surface_white_5 });
            tr.draw_text(scene, "\u{1F4CE} Attach", 12.0, c.neutral[8], t * Affine::translate((ar.x0 + 10.0, ar.y0 + 6.0)), None);
        }

        // field labels + boxes
        let field_h = 34.0;
        let mut y = p.y0 + 60.0;
        let draw_field_label = |scene: &mut Scene, tr: &mut TextRenderer, label: &str, y: f64| {
            tr.draw_text(scene, label, 12.5, c.text_dim, t * Affine::translate((lx, y + 9.0)), None);
        };
        let field_box = |scene: &mut Scene, focused: bool, y: f64| {
            let fr = Rect::new(lx + 52.0, y - 2.0, rx, y + field_h - 2.0);
            fill_rect(scene, t, Rect::new(fr.x0, fr.y1 - 1.0, fr.x1, fr.y1), if focused { c.border_focus } else { c.border_standard });
        };
        draw_field_label(scene, tr, "To", y);
        field_box(scene, matches!(self.focus, Focus::ComposeTo), y);
        y += field_h + 8.0;
        if self.show_cc_bcc {
            draw_field_label(scene, tr, "Cc", y);
            field_box(scene, matches!(self.focus, Focus::ComposeCc), y);
            y += field_h + 8.0;
            draw_field_label(scene, tr, "Bcc", y);
            field_box(scene, matches!(self.focus, Focus::ComposeBcc), y);
            y += field_h + 8.0;
        }
        draw_field_label(scene, tr, "Subject", y);
        field_box(scene, matches!(self.focus, Focus::ComposeSubject), y);
        y += field_h + 12.0;

        // body
        let body_rect = Rect::new(lx, y, rx, p.y1 - 64.0);
        fill_round(scene, t, body_rect, 8.0, c.surfaces.base);
        stroke_round(scene, t, body_rect, 8.0, 1.0, if matches!(self.focus, Focus::ComposeBody) { c.border_focus } else { c.border_standard });
        // body text wrapped
        let mut by = body_rect.y0 + 10.0;
        let bw = (body_rect.width() - 20.0) as f32;
        let display = if self.compose_body.is_empty() && !matches!(self.focus, Focus::ComposeBody) {
            "Write your message…".to_string()
        } else {
            self.compose_body.clone()
        };
        let body_col = if self.compose_body.is_empty() { c.text_dim } else { c.neutral[8] };
        for para in display.split('\n') {
            if by > body_rect.y1 - 16.0 {
                break;
            }
            let (_w, h) = tr.measure(if para.is_empty() { " " } else { para }, 13.0, Some(bw));
            tr.draw_text(scene, para, 13.0, body_col, t * Affine::translate((body_rect.x0 + 10.0, by)), Some(bw));
            by += h.max(18.0) as f64;
        }
        // caret in body
        if matches!(self.focus, Focus::ComposeBody) {
            let last = self.compose_body.rsplit('\n').next().unwrap_or("");
            let (cw, _) = tr.measure(last, 13.0, None);
            let cx = body_rect.x0 + 10.0 + cw as f64;
            let cy = (by - 18.0).max(body_rect.y0 + 10.0);
            fill_rect(scene, t, Rect::new(cx, cy, cx + 1.5, cy + 16.0), c.accent[5]);
        }

        // attachment chips above buttons
        if !self.compose_atts.is_empty() {
            let mut ax = lx;
            let ay = p.y1 - 92.0;
            for name in &self.compose_atts {
                let (lw, _) = tr.measure(name, 11.5, None);
                let chip = Rect::new(ax, ay, ax + lw as f64 + 34.0, ay + 26.0);
                fill_round(scene, t, chip, 8.0, c.surface_white_10);
                tr.draw_text(scene, "\u{1F4CE}", 12.0, c.text_dim, t * Affine::translate((chip.x0 + 7.0, chip.y0 + 5.0)), None);
                tr.draw_text(scene, name, 11.5, c.text_primary, t * Affine::translate((chip.x0 + 26.0, chip.y0 + 5.0)), None);
                ax = chip.x1 + 8.0;
            }
        }

        // buttons
        self.draw_overlay_buttons(scene, t, tr, theme, "Send", Hit::Send, Some(("Save draft", Hit::SaveDraft)));
    }

    fn draw_overlay_buttons(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, primary: &str, primary_hit: Hit, secondary: Option<(&str, Hit)>) {
        let c = &theme.tokens.colors;
        let matches_hit = |a: &Hit, b: &Hit| std::mem::discriminant(a) == std::mem::discriminant(b);
        for (rect, hit) in &self.hits {
            if matches_hit(hit, &primary_hit) {
                let hov = rect.contains(self.last_mouse);
                fill_round(scene, t, *rect, 9.0, if hov { c.accent[4] } else { c.accent[5] });
                let (w, _) = tr.measure_weighted(primary, 13.5, None, TextWeight::Semibold);
                tr.draw_text_weighted(scene, primary, 13.5, Color::WHITE, t * Affine::translate(((rect.x0 + rect.x1) / 2.0 - w as f64 / 2.0, rect.y0 + 10.0)), None, TextWeight::Semibold);
            } else if matches!(hit, Hit::CancelOverlay) {
                let hov = rect.contains(self.last_mouse);
                fill_round(scene, t, *rect, 9.0, if hov { c.surface_white_10 } else { c.surface_white_5 });
                let (w, _) = tr.measure(if matches!(self.mode, Mode::Setup) { "Cancel" } else { "Discard" }, 13.0, None);
                tr.draw_text(scene, if matches!(self.mode, Mode::Setup) { "Cancel" } else { "Discard" }, 13.0, c.neutral[8], t * Affine::translate(((rect.x0 + rect.x1) / 2.0 - w as f64 / 2.0, rect.y0 + 10.0)), None);
            }
            if let Some((label, ref sh)) = secondary {
                if matches_hit(hit, sh) {
                    let hov = rect.contains(self.last_mouse);
                    fill_round(scene, t, *rect, 9.0, if hov { c.surface_white_10 } else { c.surface_white_5 });
                    stroke_round(scene, t, *rect, 9.0, 1.0, c.border_white_10);
                    let (w, _) = tr.measure(label, 13.0, None);
                    tr.draw_text(scene, label, 13.0, c.neutral[8], t * Affine::translate(((rect.x0 + rect.x1) / 2.0 - w as f64 / 2.0, rect.y0 + 10.0)), None);
                }
            }
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
        tr.draw_text_weighted(scene, "Account setup", 16.0, c.text_primary, t * Affine::translate((lx, p.y0 + 18.0)), None, TextWeight::Bold);
        tr.draw_text(scene, "IMAP for receiving · SMTP for sending", 11.5, c.text_dim, t * Affine::translate((lx, p.y0 + 38.0)), None);

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
            // label to the left (for primary rows) — only rows 0..4 and host rows
            if i < labels.len() && !labels[i].is_empty() {
                tr.draw_text(scene, labels[i], 12.5, c.text_dim, t * Affine::translate((lx, rect.y0 + 9.0)), None);
            }
            if i == 5 {
                tr.draw_text(scene, "Port", 11.0, c.text_dim, t * Affine::translate((rect.x0, rect.y0 - 14.0)), None);
            }
            // field background
            fill_round(scene, t, Rect::new(rect.x0 - 2.0, rect.y0 - 2.0, rect.x1 + 2.0, rect.y1 + 2.0), 7.0, c.surfaces.base);
            let border = if err_for(i) { c.semantic.danger } else if focused { c.border_focus } else { c.border_standard };
            stroke_round(scene, t, Rect::new(rect.x0 - 2.0, rect.y0 - 2.0, rect.x1 + 2.0, rect.y1 + 2.0), 7.0, 1.0, border);
        }
        // TLS toggles
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

        // buttons
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
            // Pull persisted flags back in (covers undo / remote peers).
            MailDoc::apply_flags(state, &mut self.mailbox);
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
        self.draw_sidebar(scene, t, tr, theme);
        self.draw_topbar(scene, t, tr, theme, presence);
        self.draw_list(scene, t, tr, theme);
        self.draw_reader(scene, t, tr, theme);
        // embedded search input text/caret
        self.search_input.draw(scene, t, tr, theme, presence);

        match self.mode {
            Mode::Compose => {
                self.draw_compose(scene, t, tr, theme);
                self.c_to.draw(scene, t, tr, theme, presence);
                if self.show_cc_bcc {
                    self.c_cc.draw(scene, t, tr, theme, presence);
                    self.c_bcc.draw(scene, t, tr, theme, presence);
                }
                self.c_subject.draw(scene, t, tr, theme, presence);
            }
            Mode::Setup => {
                self.draw_setup(scene, t, tr, theme);
                for inp in &mut self.setup_inputs {
                    inp.draw(scene, t, tr, theme, presence);
                }
            }
            Mode::Mailbox => {}
        }

        self.draw_status(scene, t, tr, theme);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
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
        // Track mouse.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
            self.dirty = true;
        }

        // Esc closes overlays.
        if let WindowEvent::KeyboardInput { event: KeyEvent { logical_key: Key::Named(NamedKey::Escape), state: ElementState::Pressed, .. }, .. } = event {
            if self.mode != Mode::Mailbox {
                self.close_overlay(state);
                *handled = true;
                return;
            }
        }

        // Route keystrokes to the focused embedded input first.
        if matches!(event, WindowEvent::KeyboardInput { .. }) {
            // Compose body uses a custom buffer.
            if matches!(self.focus, Focus::ComposeBody) {
                if self.handle_compose_body_key(event) {
                    *handled = true;
                    return;
                }
            }
            // Forward to focused TextInput.
            let focus = self.focus;
            if let Some(inp) = self.focused_input() {
                inp.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy);
                if matches!(focus, Focus::Search) {
                    self.search = self.search_input.value().to_string();
                    MailDoc::set_search(state, &self.search);
                    // reselect first result
                    self.selected = self.list_rows().first().map(|(id, _)| *id);
                }
                if *handled {
                    self.dirty = true;
                    return;
                }
            }
        }

        // Global keyboard shortcuts in mailbox mode (when no field focused).
        if self.mode == Mode::Mailbox && matches!(self.focus, Focus::None) {
            if let WindowEvent::KeyboardInput { event: KeyEvent { logical_key, state: ElementState::Pressed, .. }, .. } = event {
                match logical_key {
                    Key::Character(ch) if ch.eq_ignore_ascii_case("c") => {
                        self.open_compose_new(state);
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
                            self.mailbox.archive(id);
                            self.persist_flags(state, id);
                            self.reselect_after_remove(state);
                            *handled = true;
                            return;
                        }
                    }
                    Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                        if let Some(id) = self.selected {
                            self.mailbox.delete(id);
                            self.persist_flags(state, id);
                            self.reselect_after_remove(state);
                            *handled = true;
                            return;
                        }
                    }
                    Key::Character(ch) if ch.eq_ignore_ascii_case("s") => {
                        if let Some(id) = self.selected {
                            self.mailbox.toggle_star(id);
                            self.persist_flags(state, id);
                            self.dirty = true;
                            *handled = true;
                            return;
                        }
                    }
                    _ => {}
                }
            }
        }

        // Mouse clicks → hit-test regions.
        if let WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } = event {
            let pt = self.last_mouse;
            // overlay click-out closes? Keep panel modal; clicking scrim cancels.
            let hit = self.hits.iter().rev().find(|(r, _)| r.contains(pt)).map(|(_, h)| h.clone());
            // If in overlay and click outside panel → cancel.
            if self.mode != Mode::Mailbox {
                let panel = self.overlay_panel_rect();
                if !panel.contains(pt) && hit.is_none() {
                    self.close_overlay(state);
                    *handled = true;
                    return;
                }
            }
            if let Some(hit) = hit {
                self.dispatch_hit(state, hit, pt);
                *handled = true;
                return;
            }
            // click on a header field box region in compose: focus it.
            if self.mode == Mode::Compose {
                self.focus_compose_field_at(pt);
                *handled = true;
                return;
            }
            // click in body region focuses body
            if self.mode == Mode::Mailbox && !self.list_rect.contains(pt) {
                // clicking reader / empty: drop input focus
                self.focus_only(Focus::None);
            }
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
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

impl MailApp {
    fn move_selection(&mut self, state: &mut AutoCommit, delta: i32) {
        let rows = self.list_rows();
        if rows.is_empty() {
            return;
        }
        let cur = self
            .selected
            .and_then(|sel| rows.iter().position(|(id, _)| {
                // match by thread membership: the row id is the newest msg
                *id == sel || self.mailbox.get(sel).map(|m| m.thread_subject()) == self.mailbox.get(*id).map(|x| x.thread_subject())
            }))
            .unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, rows.len() as i32 - 1) as usize;
        let id = rows[next].0;
        self.select_message(state, id);
    }

    fn reselect_after_remove(&mut self, state: &mut AutoCommit) {
        self.selected = self
            .current_threads()
            .first()
            .and_then(|t| t.message_ids.last().copied());
        if let Some(sel) = self.selected {
            MailDoc::set_selected(state, Some(&sel.to_string()));
        } else {
            MailDoc::set_selected(state, None);
        }
        self.dirty = true;
    }

    fn dispatch_hit(&mut self, state: &mut AutoCommit, hit: Hit, _pt: Point) {
        match hit {
            Hit::Folder(f) => {
                self.mode = Mode::Mailbox;
                self.focus_only(Focus::None);
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
            Hit::Compose => self.open_compose_new(state),
            Hit::Settings => self.open_setup(state),
            Hit::SearchClear => {
                self.search.clear();
                self.search_input.set_text("");
                MailDoc::set_search(state, "");
                self.focus_only(Focus::None);
            }
            Hit::Reply => self.open_reply(state),
            Hit::ArchiveSel => {
                if let Some(id) = self.selected {
                    self.mailbox.archive(id);
                    self.persist_flags(state, id);
                    self.reselect_after_remove(state);
                }
            }
            Hit::DeleteSel => {
                if let Some(id) = self.selected {
                    self.mailbox.delete(id);
                    self.persist_flags(state, id);
                    self.reselect_after_remove(state);
                }
            }
            Hit::StarSel => {
                if let Some(id) = self.selected {
                    self.mailbox.toggle_star(id);
                    self.persist_flags(state, id);
                    self.dirty = true;
                }
            }
            Hit::UnreadSel => {
                if let Some(id) = self.selected {
                    self.mailbox.mark_read(id, false);
                    self.persist_flags(state, id);
                    self.dirty = true;
                }
            }
            Hit::Send => self.send_compose(state),
            Hit::SaveDraft => self.save_draft(state),
            Hit::CancelOverlay => self.close_overlay(state),
            Hit::SetupField(i) => {
                if i < self.setup_inputs.len() {
                    self.focus_only(Focus::Setup(i));
                }
            }
            Hit::ToggleImapTls => {
                self.setup_imap_tls = !self.setup_imap_tls;
                // common port defaults follow the toggle
                self.setup_inputs[5].set_text(if self.setup_imap_tls { "993" } else { "143" });
                self.dirty = true;
            }
            Hit::ToggleSmtpTls => {
                self.setup_smtp_tls = !self.setup_smtp_tls;
                self.setup_inputs[7].set_text(if self.setup_smtp_tls { "465" } else { "587" });
                self.dirty = true;
            }
            Hit::SetupSave => self.save_setup(state),
            Hit::AddAttachment => {
                let n = self.compose_atts.len() + 1;
                self.compose_atts.push(format!("attachment-{n}.pdf"));
                self.dirty = true;
            }
        }
    }

    fn focus_compose_field_at(&mut self, pt: Point) {
        // Determine which header field box (or body) was clicked using the
        // embedded inputs' rects.
        if self.c_to.get_rect().contains(pt) {
            self.focus_only(Focus::ComposeTo);
        } else if self.show_cc_bcc && self.c_cc.get_rect().contains(pt) {
            self.focus_only(Focus::ComposeCc);
        } else if self.show_cc_bcc && self.c_bcc.get_rect().contains(pt) {
            self.focus_only(Focus::ComposeBcc);
        } else if self.c_subject.get_rect().contains(pt) {
            self.focus_only(Focus::ComposeSubject);
        } else {
            // default to body when clicking the lower panel
            let p = self.overlay_panel_rect();
            if pt.y > p.y0 + 120.0 {
                self.focus_only(Focus::ComposeBody);
            }
        }
    }

    /// Handle a keystroke for the custom compose-body buffer. Returns true if
    /// consumed.
    fn handle_compose_body_key(&mut self, event: &WindowEvent) -> bool {
        if let WindowEvent::KeyboardInput { event: KeyEvent { logical_key, text, state: ElementState::Pressed, .. }, .. } = event {
            match logical_key {
                Key::Named(NamedKey::Backspace) => {
                    self.compose_body.pop();
                    self.dirty = true;
                    return true;
                }
                Key::Named(NamedKey::Enter) => {
                    self.compose_body.push('\n');
                    self.dirty = true;
                    return true;
                }
                Key::Named(NamedKey::Space) => {
                    self.compose_body.push(' ');
                    self.dirty = true;
                    return true;
                }
                _ => {}
            }
            if let Some(txt) = text {
                let s = txt.as_str();
                if !s.is_empty() && !s.chars().any(|c| c.is_control()) {
                    self.compose_body.push_str(s);
                    self.dirty = true;
                    return true;
                }
            }
        }
        false
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
        assert!(app.selected.is_some(), "an inbox thread should be preselected");
        assert_eq!(app.folder, Folder::Inbox);
    }

    #[test]
    fn selecting_message_marks_read_and_persists() {
        let (mut app, mut doc) = new_app();
        app.sync(&mut doc, &automerge::ROOT);
        // find an unread inbox message
        let unread = app
            .mailbox
            .messages_in(Folder::Inbox)
            .into_iter()
            .find(|m| !m.read)
            .map(|m| m.id);
        let id = unread.expect("some unread message");
        app.select_message(&mut doc, id);
        assert!(app.mailbox.get(id).unwrap().read);
        // flag persisted to the doc
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
        app.open_compose_new(&mut doc);
        app.c_to.set_text("dest@there.example");
        app.c_subject.set_text("Hi there");
        app.compose_body = "Hello!".into();
        app.send_compose(&mut doc);
        assert!(matches!(app.mode, Mode::Mailbox));
        assert_eq!(app.mailbox.count(Folder::Sent), before + 1);
        assert!(app.status.as_deref().unwrap_or("").starts_with("Sent"));
    }

    #[test]
    fn setup_save_updates_account() {
        let (mut app, mut doc) = new_app();
        app.open_setup(&mut doc);
        app.setup_inputs[1].set_text("new@host.example");
        app.setup_inputs[2].set_text("new@host.example");
        app.setup_inputs[4].set_text("imap.host.example");
        app.setup_inputs[6].set_text("smtp.host.example");
        app.save_setup(&mut doc);
        assert!(matches!(app.mode, Mode::Mailbox));
        assert_eq!(app.mailbox.account.email, "new@host.example");
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

    #[test]
    fn folder_switch_changes_selection_scope() {
        let (mut app, mut doc) = new_app();
        app.select_folder(&mut doc, Folder::Sent);
        assert_eq!(app.folder, Folder::Sent);
        // selection should be a sent message (or none)
        if let Some(id) = app.selected {
            assert_eq!(app.mailbox.get(id).unwrap().folder, Folder::Sent);
        }
    }
}
