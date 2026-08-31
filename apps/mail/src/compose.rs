//! The Compose window — a **movable, resizable, focus-capturing** floating
//! panel for writing a message.
//!
//! ## Focus capture (the headline fix)
//! `ui`'s `App` has no modal/overlay focus-trap and `ModalDialog`'s "blocking"
//! is only partially implemented, so a naive overlay lets clicks fall through
//! to whatever is painted behind it, and embedded `TextInput`s never register a
//! click (they require a prior `CursorMoved` to set their internal `hovered`
//! flag before a press counts). This window fixes both:
//!
//! * While it is open the host routes **every** event here first. Any pointer
//!   event whose position is inside the window — and every keyboard event — is
//!   consumed, so nothing reaches the mailbox behind it.
//! * It forwards `CursorMoved` to each visible child `TextInput`/body editor
//!   so their hover state is correct, then forwards the press so the caret
//!   lands. It owns explicit focus (one field at a time).
//!
//! ## Movable / resizable
//! The title bar is a drag handle; the window edges/corners are resize handles.
//! Geometry is clamped to a minimum size and kept on-screen. This works in any
//! environment (it's all in-app drawing), so it's fully exercised by `--shots`.
//!
//! ## Detach (qOS pop-out)
//! [`DockState`] models two placements: `Docked` (centred in-app modal) and
//! `Detached` (parked beside the app content, simulating a separate window).
//! Both are movable + resizable. The Pop-out button toggles between them, so it
//! performs a real, visible placement change. **True OS-window detach** — a real
//! second top-level surface beside the app — needs the qOS compositor's
//! window-group / multi-window support (`shell`), which is not available through
//! the single-window `ui::App`; when that lands, a detached draft is handed back
//! via [`ComposeWindow::reattach`].

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::Widget;
use ui::color_compat::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::TextInput;

use crate::model::{ComposeFormat, Draft, attachment_display_name};
use crate::wysiwyg::{BlockKind, InlineMark, Wysiwyg};

const TITLE_H: f64 = 38.0;
const TOOLBAR_H: f64 = 36.0;
const FIELD_H: f64 = 32.0;
const PAD: f64 = 14.0;
const LABEL_W: f64 = 54.0;
const MIN_W: f64 = 380.0;
const MIN_H: f64 = 340.0;
const RESIZE_GRIP: f64 = 8.0;

/// Where the compose window is parked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockState {
    /// Centred over the app content as an in-app modal.
    Docked,
    /// Parked beside the app content, simulating a detached OS window.
    Detached,
}

/// Which compose control currently has keyboard focus.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    None,
    To,
    Cc,
    Bcc,
    Subject,
    Body,
}

/// An interactive region inside the window, resolved during layout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ComposeHit {
    TitleBar,
    Send,
    Discard,
    SaveDraft,
    Attach,
    ToggleCcBcc,
    PopOut,
    Close,
    FormatToggle,
    FieldTo,
    FieldCc,
    FieldBcc,
    FieldSubject,
    Body,
    // toolbar
    Bold,
    Italic,
    Underline,
    H1,
    H2,
    Bullet,
    Numbered,
    Quote,
    Link,
    // inline prompt (Attach path / Link URL entry)
    PromptField,
    PromptConfirm,
    PromptCancel,
}

/// What the inline prompt bar is collecting.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PromptKind {
    /// A filesystem path to attach as a real file.
    Attach,
    /// A URL to link the current selection (or insert) to.
    Link,
}

/// A small inline entry bar shown inside the compose window when the user needs
/// to type a value the app can't fabricate: a file path to attach, or a link
/// target. Confirming an Attach reads the real file; confirming a Link applies
/// the entered `href`. This exists because `ui` has no file-picker / URL-prompt
/// primitive yet (see the app-level UI-gap notes).
struct ComposePrompt {
    kind: PromptKind,
    input: TextInput,
    error: Option<String>,
}

/// What the host should do after dispatching a window action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposeAction {
    None,
    Send,
    SaveDraft,
    Close,
}

/// A drag in progress on the window itself (move or resize).
#[derive(Clone, Copy)]
enum Drag {
    Move {
        grab: Point,
    },
    Resize {
        edge: Edge,
        start: Rect,
        from: Point,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Edge {
    Right,
    Bottom,
    BottomRight,
    Left,
    BottomLeft,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "the bools split across unrelated concerns (window lifecycle: open/placed, header UI: show_cc_bcc, redraw bookkeeping: dirty); grouping them would need a real design pass, left for a follow-up rather than forced here"
)]
pub struct ComposeWindow {
    pub open: bool,
    dock: DockState,
    /// Window rect in screen coordinates. The source of truth for placement;
    /// movable + resizable mutate it directly.
    rect: Rect,
    /// Bounds the window must stay within (the app content area).
    bounds: Rect,
    /// `true` once the window has been positioned for the current open, so a
    /// re-layout doesn't recentre it after the user has moved it.
    placed: bool,

    // header inputs
    to: TextInput,
    cc: TextInput,
    bcc: TextInput,
    subject: TextInput,
    show_cc_bcc: bool,

    // body
    format: ComposeFormat,
    editor: Wysiwyg,
    /// Plain-text body (Text mode) — a shared multiline [`TextInput`], so the
    /// body gets real caret/selection/clipboard/a11y for free instead of a
    /// hand-rolled `String`.
    plain_body: TextInput,

    /// Filesystem paths of staged attachments (real files read on send).
    attachments: Vec<String>,
    /// The inline path/URL prompt, when open.
    prompt: Option<ComposePrompt>,
    focus: Field,
    drag: Option<Drag>,
    last_mouse: Point,
    hits: Vec<(Rect, ComposeHit)>,
    dirty: bool,
    /// The reply target subject line, shown in the title when replying.
    title_hint: String,
}

impl ComposeWindow {
    pub fn new(format: ComposeFormat) -> Self {
        let mk = || {
            let mut t = TextInput::local();
            t.set_plain_chrome(true);
            t
        };
        Self {
            open: false,
            dock: DockState::Docked,
            rect: Rect::ZERO,
            bounds: Rect::ZERO,
            placed: false,
            to: mk(),
            cc: mk(),
            bcc: mk(),
            subject: mk(),
            show_cc_bcc: false,
            format,
            editor: Wysiwyg::new(),
            plain_body: {
                let mut b = TextInput::local()
                    .multiline(true)
                    .with_placeholder("Write your message…");
                b.set_plain_chrome(true); // the body area draws its own card
                b
            },
            attachments: Vec::new(),
            prompt: None,
            focus: Field::None,
            drag: None,
            last_mouse: Point::ZERO,
            hits: Vec::new(),
            dirty: true,
            title_hint: String::new(),
        }
    }

    pub fn format(&self) -> ComposeFormat {
        self.format
    }

    /// Set the authoring format directly (used to restore the persisted
    /// preference on open). Content is not converted; call on a fresh/empty
    /// window.
    pub fn set_format(&mut self, format: ComposeFormat) {
        self.format = format;
        self.dirty = true;
    }

    pub fn dock(&self) -> DockState {
        self.dock
    }

    pub fn needs_redraw(&self) -> bool {
        self.dirty
    }

    pub fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    pub fn window_rect(&self) -> Rect {
        self.rect
    }

    /// Open a blank compose window (or a reply when fields are pre-seeded by
    /// the caller right after).
    pub fn open_blank(&mut self) {
        self.open = true;
        self.placed = false;
        self.to.set_text("");
        self.cc.set_text("");
        self.bcc.set_text("");
        self.subject.set_text("");
        self.show_cc_bcc = false;
        self.attachments.clear();
        self.prompt = None;
        self.editor = Wysiwyg::new();
        self.plain_body.set_text("");
        self.title_hint.clear();
        self.set_focus(Field::To);
        self.dirty = true;
    }

    /// Seed the window as a reply: recipient, subject, and a quoted body.
    #[allow(clippy::too_many_arguments)]
    pub fn open_reply(&mut self, to: &str, subject: &str, quote: &str) {
        self.open_blank();
        self.to.set_text(to);
        self.subject.set_text(subject);
        self.title_hint = subject.to_string();
        match self.format {
            ComposeFormat::Html => self.editor.set_plain(quote),
            ComposeFormat::Text => {
                self.plain_body.set_text(quote);
            }
        }
        self.set_focus(Field::Body);
    }

    /// Seed the window as a reply-all: the sender in `To`, everyone else in
    /// `Cc` (revealed), the subject, and a quoted body.
    pub fn open_reply_all(&mut self, to: &str, cc: &str, subject: &str, quote: &str) {
        self.open_reply(to, subject, quote);
        if !cc.trim().is_empty() {
            self.cc.set_text(cc);
            self.show_cc_bcc = true;
        }
    }

    /// Reopen an existing draft for editing (from the Drafts folder).
    pub fn open_with_draft(&mut self, draft: &Draft) {
        self.open_blank();
        self.to.set_text(&draft.to);
        self.cc.set_text(&draft.cc);
        self.bcc.set_text(&draft.bcc);
        self.subject.set_text(&draft.subject);
        self.show_cc_bcc = !draft.cc.is_empty() || !draft.bcc.is_empty();
        self.attachments.clone_from(&draft.attachments);
        match self.format {
            ComposeFormat::Html => self.editor.set_plain(&draft.body),
            ComposeFormat::Text => self.plain_body.set_text(&draft.body),
        }
        if !draft.subject.is_empty() {
            self.title_hint.clone_from(&draft.subject);
        }
        self.set_focus(Field::Body);
    }

    /// Open the window pre-filled with a representative formatted message, used
    /// by the headless screenshot to show the WYSIWYG editor in action.
    pub fn open_demo(&mut self) {
        use crate::wysiwyg::{BlockKind, Style};
        self.open_blank();
        self.format = ComposeFormat::Html;
        self.to.set_text("Ada Lovelace <ada@analytical.example>");
        self.subject
            .set_text("Re: Launch plan for the spatial shell");
        self.title_hint = "New message".to_string();
        let bold = Style {
            bold: true,
            ..Default::default()
        };
        let italic = Style {
            italic: true,
            ..Default::default()
        };
        let plain = Style::default();
        self.editor = Wysiwyg::new();
        self.editor
            .push_styled_block(BlockKind::Heading(2), &[("Launch sequencing", plain)]);
        self.editor.push_styled_block(
            BlockKind::Paragraph,
            &[
                ("Thanks both — the new order is ", plain),
                ("tiling → sandbox → streaming", bold),
                (".", plain),
            ],
        );
        self.editor.push_styled_block(
            BlockKind::Bullet,
            &[("Tiling engine skeleton by Wednesday", plain)],
        );
        self.editor.push_styled_block(
            BlockKind::Bullet,
            &[("Bubblewrap sandbox policy review", plain)],
        );
        self.editor.push_styled_block(
            BlockKind::Bullet,
            &[("Collaboration buffer streaming last", plain)],
        );
        self.editor.push_styled_block(
            BlockKind::Quote,
            &[("Don't ship streaming before the policy is solid.", italic)],
        );
        self.editor.set_focus(true);
        self.set_focus(Field::Body);
        self.show_cc_bcc = false;
        self.dirty = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.set_focus(Field::None);
        self.dirty = true;
    }

    /// Toggle between HTML (WYSIWYG) and plain-text authoring, converting the
    /// current body so no content is lost. The host persists the new choice.
    pub fn toggle_format(&mut self) {
        match self.format {
            ComposeFormat::Html => {
                // HTML → plain: flatten.
                let flat = self.editor.to_plain();
                self.plain_body.set_text(&flat);
                self.format = ComposeFormat::Text;
            }
            ComposeFormat::Text => {
                // plain → HTML: re-parse into blocks.
                self.editor.set_plain(self.plain_body.value());
                self.format = ComposeFormat::Html;
            }
        }
        if matches!(self.focus, Field::Body) {
            self.set_focus(Field::Body);
        }
        self.dirty = true;
    }

    /// Pop the window out to the side (Detached) or back in (Docked).
    pub fn toggle_dock(&mut self) {
        self.dock = match self.dock {
            DockState::Docked => DockState::Detached,
            DockState::Detached => DockState::Docked,
        };
        self.placed = false; // reposition for the new placement
        self.dirty = true;
    }

    /// Seam for re-attaching an externally-edited draft back into the in-app
    /// window (e.g. after a real OS pop-out hands the draft back).
    pub fn reattach(&mut self, draft: &Draft) {
        self.to.set_text(&draft.to);
        self.cc.set_text(&draft.cc);
        self.bcc.set_text(&draft.bcc);
        self.subject.set_text(&draft.subject);
        self.plain_body.set_text(&draft.body);
        if let Some(html) = &draft.html_body {
            // Best-effort: re-seed from the plain alternative (HTML re-import is
            // a future enhancement — see wysiwyg promotion note).
            let _ = html;
        }
        self.editor.set_plain(&draft.body);
        self.dock = DockState::Docked;
        self.placed = false;
        self.dirty = true;
    }

    /// The current draft, with both a plain body and (in HTML mode) an HTML
    /// body, ready for `transport::build_mime`.
    pub fn to_draft(&self) -> Draft {
        let (body, html_body) = match self.format {
            ComposeFormat::Html => (self.editor.to_plain(), Some(self.editor.to_html())),
            ComposeFormat::Text => (self.plain_body.value().to_string(), None),
        };
        Draft {
            to: self.to.value().to_string(),
            cc: self.cc.value().to_string(),
            bcc: self.bcc.value().to_string(),
            subject: self.subject.value().to_string(),
            body,
            html_body,
            attachments: self.attachments.clone(),
            in_reply_to: None,
        }
    }

    // ---- focus ----

    fn set_focus(&mut self, field: Field) {
        self.focus = field;
        self.to.set_focus(matches!(field, Field::To));
        self.cc.set_focus(matches!(field, Field::Cc));
        self.bcc.set_focus(matches!(field, Field::Bcc));
        self.subject.set_focus(matches!(field, Field::Subject));
        self.editor
            .set_focus(matches!(field, Field::Body) && matches!(self.format, ComposeFormat::Html));
        self.plain_body
            .set_focus(matches!(field, Field::Body) && matches!(self.format, ComposeFormat::Text));
        self.dirty = true;
    }

    fn focused_input(&mut self) -> Option<&mut TextInput> {
        match self.focus {
            Field::To => Some(&mut self.to),
            Field::Cc => Some(&mut self.cc),
            Field::Bcc => Some(&mut self.bcc),
            Field::Subject => Some(&mut self.subject),
            _ => None,
        }
    }

    // ---- layout ----

    /// Position the window for the current dock state within `bounds`, then
    /// lay out every interactive region. `bounds` is the app content rect.
    pub fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        if !self.placed {
            let (w, h) = (
                560.0_f64.min(bounds.width() - 32.0).max(MIN_W),
                460.0_f64.min(bounds.height() - 32.0).max(MIN_H),
            );
            self.rect = match self.dock {
                DockState::Docked => {
                    let cx = f64::midpoint(bounds.x0, bounds.x1);
                    let y0 = bounds.y0 + (bounds.height() - h) / 2.0;
                    Rect::new(cx - w / 2.0, y0.max(bounds.y0 + 8.0), cx + w / 2.0, y0 + h)
                }
                DockState::Detached => {
                    // Park on the right edge of the content area, like a window
                    // tiled beside the app.
                    let x1 = bounds.x1 - 16.0;
                    let y0 = bounds.y0 + 16.0;
                    Rect::new(x1 - w, y0, x1, (y0 + h).min(bounds.y1 - 16.0))
                }
            };
            self.placed = true;
        }
        self.clamp_into_bounds();
        self.relayout_regions();
    }

    fn clamp_into_bounds(&mut self) {
        let b = self.bounds;
        let mut r = self.rect;
        let w = r.width().min(b.width()).max(MIN_W.min(b.width()));
        let h = r.height().min(b.height()).max(MIN_H.min(b.height()));
        let mut x0 = r.x0.clamp(b.x0, (b.x1 - w).max(b.x0));
        let mut y0 = r.y0.clamp(b.y0, (b.y1 - h).max(b.y0));
        // keep the title bar reachable
        x0 = x0.min(b.x1 - 80.0);
        y0 = y0.max(b.y0);
        r = Rect::new(x0, y0, x0 + w, y0 + h);
        self.rect = r;
    }

    /// Lay out and register the hit regions for the title bar (drag handle +
    /// window buttons), the To/Cc/Bcc/Subject fields, and the Attach button.
    /// Returns the y past the header, where the toolbar starts. Part of
    /// [`Self::relayout_regions`].
    fn layout_header_hits(&mut self, r: Rect, lx: f64, rx: f64) -> f64 {
        // Title bar (drag handle) + window buttons.
        self.hits.push((
            Rect::new(r.x0, r.y0, r.x1, r.y0 + TITLE_H),
            ComposeHit::TitleBar,
        ));
        let btn = 28.0;
        let close = Rect::new(r.x1 - btn - 8.0, r.y0 + 6.0, r.x1 - 8.0, r.y0 + 6.0 + btn);
        self.hits.push((close, ComposeHit::Close));
        let popout = Rect::new(
            close.x0 - btn - 4.0,
            r.y0 + 6.0,
            close.x0 - 4.0,
            r.y0 + 6.0 + btn,
        );
        self.hits.push((popout, ComposeHit::PopOut));

        let mut y = r.y0 + TITLE_H + 10.0;

        // To row + Cc/Bcc toggle on the right.
        let cc_toggle_w = 64.0;
        self.to.apply_layout_external(Rect::new(
            lx + LABEL_W,
            y,
            rx - cc_toggle_w - 8.0,
            y + FIELD_H,
        ));
        self.hits.push((
            Rect::new(lx + LABEL_W, y, rx - cc_toggle_w - 8.0, y + FIELD_H),
            ComposeHit::FieldTo,
        ));
        self.hits.push((
            Rect::new(rx - cc_toggle_w, y, rx, y + FIELD_H),
            ComposeHit::ToggleCcBcc,
        ));
        y += FIELD_H + 6.0;
        if self.show_cc_bcc {
            self.cc
                .apply_layout_external(Rect::new(lx + LABEL_W, y, rx, y + FIELD_H));
            self.hits.push((
                Rect::new(lx + LABEL_W, y, rx, y + FIELD_H),
                ComposeHit::FieldCc,
            ));
            y += FIELD_H + 6.0;
            self.bcc
                .apply_layout_external(Rect::new(lx + LABEL_W, y, rx, y + FIELD_H));
            self.hits.push((
                Rect::new(lx + LABEL_W, y, rx, y + FIELD_H),
                ComposeHit::FieldBcc,
            ));
            y += FIELD_H + 6.0;
        }
        // Subject + Attach button.
        let attach_w = 84.0;
        self.subject.apply_layout_external(Rect::new(
            lx + LABEL_W,
            y,
            rx - attach_w - 8.0,
            y + FIELD_H,
        ));
        self.hits.push((
            Rect::new(lx + LABEL_W, y, rx - attach_w - 8.0, y + FIELD_H),
            ComposeHit::FieldSubject,
        ));
        self.hits.push((
            Rect::new(rx - attach_w, y, rx, y + FIELD_H),
            ComposeHit::Attach,
        ));
        y + FIELD_H + 10.0
    }

    fn relayout_regions(&mut self) {
        self.hits.clear();
        let r = self.rect;
        let lx = r.x0 + PAD;
        let rx = r.x1 - PAD;
        let mut y = self.layout_header_hits(r, lx, rx);

        // Formatting toolbar (HTML mode) + format toggle (always).
        let toolbar_y = y;
        let mut tx = lx;
        let tw = 30.0;
        if matches!(self.format, ComposeFormat::Html) {
            for hit in [
                ComposeHit::Bold,
                ComposeHit::Italic,
                ComposeHit::Underline,
                ComposeHit::H1,
                ComposeHit::H2,
                ComposeHit::Bullet,
                ComposeHit::Numbered,
                ComposeHit::Quote,
                ComposeHit::Link,
            ] {
                self.hits.push((
                    Rect::new(tx, toolbar_y, tx + tw, toolbar_y + TOOLBAR_H - 6.0),
                    hit,
                ));
                tx += tw + 2.0;
                // small gap after the inline-marks group
                if matches!(hit, ComposeHit::Underline) {
                    tx += 6.0;
                }
            }
        }
        // Format toggle on the right of the toolbar row.
        let ftw = 96.0;
        self.hits.push((
            Rect::new(rx - ftw, toolbar_y, rx, toolbar_y + TOOLBAR_H - 6.0),
            ComposeHit::FormatToggle,
        ));
        y += TOOLBAR_H;

        // Body region — fills down to the button bar.
        let buttons_h = 52.0;
        let chips_h = if self.attachments.is_empty() {
            0.0
        } else {
            30.0
        };
        let body = Rect::new(lx, y, rx, r.y1 - buttons_h - chips_h - PAD);
        self.hits.push((body, ComposeHit::Body));
        match self.format {
            ComposeFormat::Html => self.editor.apply_layout(body),
            ComposeFormat::Text => self.plain_body.apply_layout_external(body),
        }

        // Buttons.
        let by = r.y1 - buttons_h + 8.0;
        self.hits
            .push((Rect::new(lx, by, lx + 96.0, by + 36.0), ComposeHit::Send));
        self.hits.push((
            Rect::new(lx + 104.0, by, lx + 104.0 + 110.0, by + 36.0),
            ComposeHit::SaveDraft,
        ));
        self.hits
            .push((Rect::new(rx - 96.0, by, rx, by + 36.0), ComposeHit::Discard));

        // Inline prompt bar (Attach path / Link URL), sitting just above the
        // button row when open.
        if let Some(p) = self.prompt.as_mut() {
            let py = by - 46.0;
            let field = Rect::new(lx + 92.0, py, rx - 150.0, py + 30.0);
            p.input.apply_layout_external(field);
            self.hits.push((field, ComposeHit::PromptField));
            self.hits.push((
                Rect::new(rx - 146.0, py, rx - 80.0, py + 30.0),
                ComposeHit::PromptConfirm,
            ));
            self.hits.push((
                Rect::new(rx - 72.0, py, rx, py + 30.0),
                ComposeHit::PromptCancel,
            ));
        }
    }

    fn body_rect(&self) -> Rect {
        self.hits
            .iter()
            .find(|(_, h)| *h == ComposeHit::Body)
            .map_or(Rect::ZERO, |(r, _)| *r)
    }

    // ---- event handling ----

    /// Returns `true` if the event was consumed (so the host must NOT let it
    /// fall through to the mailbox behind the window). The contract that makes
    /// the window truly modal: **every keyboard event** while open is consumed,
    /// and **every pointer event inside the window rect** is consumed.
    /// A `CursorMoved`: track the active drag if any, otherwise forward the
    /// move to every input (and the prompt, if open) so their hover state
    /// tracks — required before a press will register a caret. Part of
    /// [`Self::handle_event`].
    #[allow(
        clippy::too_many_arguments,
        reason = "a direct pass-through of handle_event's own event-forwarding parameters, which mirror Widget::handle_event's fixed shape"
    )]
    fn handle_cursor_moved(
        &mut self,
        position: winit::dpi::PhysicalPosition<f64>,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) -> (bool, ComposeAction) {
        let pt = Point::new(position.x / scale_factor, position.y / scale_factor);
        self.last_mouse = pt;
        self.dirty = true;
        // Active move/resize drag.
        if let Some(drag) = self.drag {
            self.apply_drag(drag, pt);
            self.relayout_regions();
            return (true, ComposeAction::None);
        }
        // Forward to inputs so their hover flag tracks (required before a
        // press will register a caret).
        let mut h = false;
        let mut ft: Option<taffy::NodeId> = None;
        for inp in [&mut self.to, &mut self.cc, &mut self.bcc, &mut self.subject] {
            inp.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                &mut h,
                &mut ft,
                node_id,
                taffy,
            );
        }
        // Forward to the plain body too so its hover / drag-selection tracks
        // (Text mode).
        if matches!(self.format, ComposeFormat::Text) {
            self.plain_body.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                &mut h,
                &mut ft,
                node_id,
                taffy,
            );
        }
        if let Some(p) = self.prompt.as_mut() {
            p.input.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                &mut h,
                &mut ft,
                node_id,
                taffy,
            );
        }
        self.editor.on_cursor_moved(pt, false);
        // Consume only if inside the window (so hover outside still works
        // for the app, but we stay modal for clicks/keys).
        (self.rect.contains(pt), ComposeAction::None)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "deliberately shaped to match Widget::handle_event's own parameter list (event/scale_factor/modifiers/state/path/node_id/taffy), the convention every host call site in this codebase already expects"
    )]
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) -> (bool, ComposeAction) {
        if !self.open {
            return (false, ComposeAction::None);
        }
        let shift = modifiers.state().shift_key();
        let ctrl = modifiers.state().control_key();

        match event {
            WindowEvent::CursorMoved { position, .. } => self.handle_cursor_moved(
                *position,
                event,
                scale_factor,
                *modifiers,
                state,
                path,
                node_id,
                taffy,
            ),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let pt = self.last_mouse;
                if !self.rect.contains(pt) {
                    // Click outside the window: stay modal. Swallow it (do not
                    // close — explicit Discard/Close only), so nothing behind
                    // steals focus or clicks.
                    return (true, ComposeAction::None);
                }
                // Resize edge?
                if let Some(edge) = self.resize_edge_at(pt) {
                    self.drag = Some(Drag::Resize {
                        edge,
                        start: self.rect,
                        from: pt,
                    });
                    return (true, ComposeAction::None);
                }
                let hit = self
                    .hits
                    .iter()
                    .rev()
                    .find(|(r, _)| r.contains(pt))
                    .map(|(_, h)| *h);
                if let Some(hit) = hit {
                    return (
                        true,
                        self.dispatch_hit(
                            hit,
                            event,
                            pt,
                            scale_factor,
                            *modifiers,
                            state,
                            path,
                            node_id,
                            taffy,
                        ),
                    );
                }
                (true, ComposeAction::None)
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } => {
                if self.drag.is_some() {
                    self.drag = None;
                    return (true, ComposeAction::None);
                }
                (self.rect.contains(self.last_mouse), ComposeAction::None)
            }
            WindowEvent::KeyboardInput { .. } => self.handle_keyboard_input(
                event,
                scale_factor,
                *modifiers,
                state,
                path,
                node_id,
                taffy,
                shift,
                ctrl,
            ),
            _ => (self.rect.contains(self.last_mouse), ComposeAction::None),
        }
    }

    /// A `KeyboardInput`: the inline prompt (if open) captures keys first
    /// (Esc cancels it, Enter confirms), then Esc closes the window, then the
    /// focused body editor or header field gets the key. Always consumed
    /// while the window is open (modal). Part of [`Self::handle_event`].
    #[allow(
        clippy::too_many_arguments,
        reason = "a direct pass-through of handle_event's own event-forwarding parameters, which mirror Widget::handle_event's fixed shape, plus the two already-decoded modifier flags"
    )]
    fn handle_keyboard_input(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
        shift: bool,
        ctrl: bool,
    ) -> (bool, ComposeAction) {
        // The inline prompt captures keys while open: Esc cancels it (not the
        // window), Enter confirms, everything else edits the field.
        if self.prompt.is_some() {
            return self.handle_prompt_key(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                node_id,
                taffy,
            );
        }
        // Escape closes.
        if Self::is_escape_press(event) {
            self.close();
            return (true, ComposeAction::Close);
        }
        // Body editor.
        if matches!(self.focus, Field::Body) {
            self.handle_body_key(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                node_id,
                taffy,
                shift,
                ctrl,
            );
            return (true, ComposeAction::None);
        }
        // Header input.
        self.handle_header_key(event, scale_factor, modifiers, state, path, node_id, taffy);
        // Always consume keyboard while open (modal).
        (true, ComposeAction::None)
    }

    /// Keys while the inline prompt bar is open: Esc cancels it, Enter
    /// confirms, everything else edits the prompt's field. Part of
    /// [`Self::handle_keyboard_input`].
    #[allow(
        clippy::too_many_arguments,
        reason = "a direct pass-through of handle_keyboard_input's own event-forwarding parameters"
    )]
    fn handle_prompt_key(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) -> (bool, ComposeAction) {
        use winit::event::KeyEvent as WKeyEvent;
        use winit::keyboard::{Key as WKey, NamedKey as WNamedKey};
        if let WindowEvent::KeyboardInput {
            event:
                WKeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            match logical_key {
                WKey::Named(WNamedKey::Escape) => {
                    self.prompt = None;
                    self.dirty = true;
                    return (true, ComposeAction::None);
                }
                WKey::Named(WNamedKey::Enter) => {
                    self.confirm_prompt();
                    return (true, ComposeAction::None);
                }
                _ => {}
            }
        }
        let mut h = false;
        let mut ft: Option<taffy::NodeId> = None;
        if let Some(p) = self.prompt.as_mut() {
            p.input.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                &mut h,
                &mut ft,
                node_id,
                taffy,
            );
        }
        self.dirty = true;
        (true, ComposeAction::None)
    }

    fn is_escape_press(event: &WindowEvent) -> bool {
        matches!(
            event,
            WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent {
                    logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                    state: ElementState::Pressed,
                    ..
                },
                ..
            }
        )
    }

    /// Route a key to the HTML editor or the plain-text body input,
    /// depending on the active compose format. Part of
    /// [`Self::handle_keyboard_input`].
    #[allow(
        clippy::too_many_arguments,
        reason = "a direct pass-through of handle_keyboard_input's own event-forwarding parameters"
    )]
    fn handle_body_key(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
        shift: bool,
        ctrl: bool,
    ) {
        match self.format {
            ComposeFormat::Html => {
                self.editor.on_key(event, shift, ctrl);
            }
            ComposeFormat::Text => {
                let mut h = false;
                let mut ft: Option<taffy::NodeId> = None;
                self.plain_body.handle_event(
                    event,
                    scale_factor,
                    &modifiers,
                    state,
                    path,
                    &mut h,
                    &mut ft,
                    node_id,
                    taffy,
                );
            }
        }
        self.dirty = true;
    }

    /// Forward a key to whichever header field (To/Cc/Bcc/Subject) has focus.
    /// Part of [`Self::handle_keyboard_input`].
    #[allow(
        clippy::too_many_arguments,
        reason = "a direct pass-through of handle_keyboard_input's own event-forwarding parameters"
    )]
    fn handle_header_key(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) {
        let mut h = false;
        let mut ft: Option<taffy::NodeId> = None;
        if let Some(inp) = self.focused_input() {
            inp.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                &mut h,
                &mut ft,
                node_id,
                taffy,
            );
        }
        self.dirty = true;
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_hit(
        &mut self,
        hit: ComposeHit,
        event: &WindowEvent,
        pt: Point,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) -> ComposeAction {
        match hit {
            ComposeHit::TitleBar => {
                self.drag = Some(Drag::Move {
                    grab: Point::new(pt.x - self.rect.x0, pt.y - self.rect.y0),
                });
            }
            ComposeHit::Close | ComposeHit::Discard => {
                self.close();
                return ComposeAction::Close;
            }
            ComposeHit::Send => return ComposeAction::Send,
            ComposeHit::SaveDraft => return ComposeAction::SaveDraft,
            ComposeHit::PopOut => self.toggle_dock(),
            ComposeHit::ToggleCcBcc => {
                self.show_cc_bcc = !self.show_cc_bcc;
                self.relayout_regions();
            }
            ComposeHit::Attach => self.open_prompt(PromptKind::Attach),
            ComposeHit::FormatToggle => self.toggle_format(),
            ComposeHit::FieldTo
            | ComposeHit::FieldCc
            | ComposeHit::FieldBcc
            | ComposeHit::FieldSubject => {
                // Each `Field*` hit just differs in which field it focuses;
                // the forwarding that follows is identical.
                let field = match hit {
                    ComposeHit::FieldCc => Field::Cc,
                    ComposeHit::FieldBcc => Field::Bcc,
                    ComposeHit::FieldSubject => Field::Subject,
                    _ => Field::To,
                };
                self.focus_and_forward(
                    field,
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    node_id,
                    taffy,
                );
            }
            ComposeHit::Body => self.dispatch_body_press(
                pt,
                event,
                scale_factor,
                modifiers,
                state,
                path,
                node_id,
                taffy,
            ),
            ComposeHit::Bold => self.editor.toggle_inline(InlineMark::Bold),
            ComposeHit::Italic => self.editor.toggle_inline(InlineMark::Italic),
            ComposeHit::Underline => self.editor.toggle_inline(InlineMark::Underline),
            ComposeHit::H1 => self.editor.set_block_kind(BlockKind::Heading(1)),
            ComposeHit::H2 => self.editor.set_block_kind(BlockKind::Heading(2)),
            ComposeHit::Bullet => self.editor.set_block_kind(BlockKind::Bullet),
            ComposeHit::Numbered => self.editor.set_block_kind(BlockKind::Numbered),
            ComposeHit::Quote => self.editor.set_block_kind(BlockKind::Quote),
            ComposeHit::Link => self.open_prompt(PromptKind::Link),
            ComposeHit::PromptConfirm => self.confirm_prompt(),
            ComposeHit::PromptCancel => self.prompt = None,
            ComposeHit::PromptField => {
                if let Some(p) = self.prompt.as_mut() {
                    p.input.set_focus(true);
                    let mut h = false;
                    let mut ft: Option<taffy::NodeId> = None;
                    p.input.handle_event(
                        event,
                        scale_factor,
                        &modifiers,
                        state,
                        path,
                        &mut h,
                        &mut ft,
                        node_id,
                        taffy,
                    );
                }
            }
        }
        self.dirty = true;
        ComposeAction::None
    }

    /// Handle a press on the body region: place the WYSIWYG caret directly
    /// (HTML mode), or forward the real press so the plain-text body places
    /// its own caret at the click point (the preceding `CursorMoved` already
    /// set its hover). Part of [`Self::dispatch_hit`].
    #[allow(
        clippy::too_many_arguments,
        reason = "a direct pass-through of dispatch_hit's own event-forwarding parameters, which mirror Widget::handle_event's fixed shape"
    )]
    fn dispatch_body_press(
        &mut self,
        pt: Point,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) {
        self.set_focus(Field::Body);
        match self.format {
            ComposeFormat::Html => {
                self.editor.on_mouse_press(pt);
            }
            ComposeFormat::Text => {
                // Forward the real press so the body places its caret at the
                // click point (the preceding CursorMoved set hover).
                let mut h = false;
                let mut ft: Option<taffy::NodeId> = None;
                self.plain_body.handle_event(
                    event,
                    scale_factor,
                    &modifiers,
                    state,
                    path,
                    &mut h,
                    &mut ft,
                    node_id,
                    taffy,
                );
            }
        }
    }

    /// Open the inline prompt bar to collect a file path (Attach) or a URL
    /// (Link). The body/header lose focus so keystrokes land in the prompt.
    fn open_prompt(&mut self, kind: PromptKind) {
        let mut input = TextInput::local();
        input.set_plain_chrome(true);
        if kind == PromptKind::Link {
            input.set_text("https://");
        }
        input.set_focus(true);
        self.set_focus(Field::None);
        self.prompt = Some(ComposePrompt {
            kind,
            input,
            error: None,
        });
        self.relayout_regions();
        self.dirty = true;
    }

    /// Apply the prompt's value: attach the real file at the typed path, or link
    /// the selection/insert the URL. An unreadable path keeps the prompt open
    /// with an error rather than staging a fabricated attachment.
    fn confirm_prompt(&mut self) {
        let Some(p) = self.prompt.take() else { return };
        let val = p.input.value().trim().to_string();
        match p.kind {
            PromptKind::Link => {
                if !val.is_empty() {
                    self.set_focus(Field::Body);
                    if matches!(self.format, ComposeFormat::Html) {
                        self.editor.set_link(&val);
                    }
                }
            }
            PromptKind::Attach => {
                if val.is_empty() {
                    return;
                }
                match std::fs::metadata(&val) {
                    Ok(m) if m.is_file() => {
                        self.attachments.push(val);
                    }
                    _ => {
                        let mut input = TextInput::local();
                        input.set_plain_chrome(true);
                        input.set_text(&val);
                        input.set_focus(true);
                        self.prompt = Some(ComposePrompt {
                            kind: PromptKind::Attach,
                            input,
                            error: Some("No readable file at that path".into()),
                        });
                    }
                }
            }
        }
        self.relayout_regions();
        self.dirty = true;
    }

    #[allow(clippy::too_many_arguments)]
    fn focus_and_forward(
        &mut self,
        field: Field,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) {
        self.set_focus(field);
        // Forward the *real* press the host just received so the input places
        // its caret at the click point. The input only acts on a press while
        // hovered, and the preceding CursorMoved (forwarded in handle_event)
        // already set its hover flag — so this lands precisely.
        let mut h = false;
        let mut ft: Option<taffy::NodeId> = None;
        if let Some(inp) = self.focused_input() {
            inp.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                &mut h,
                &mut ft,
                node_id,
                taffy,
            );
        }
    }

    // ---- move / resize ----

    fn resize_edge_at(&self, pt: Point) -> Option<Edge> {
        let r = self.rect;
        let near_r = (pt.x - r.x1).abs() <= RESIZE_GRIP;
        let near_l = (pt.x - r.x0).abs() <= RESIZE_GRIP;
        let near_b = (pt.y - r.y1).abs() <= RESIZE_GRIP;
        // Don't hijack the title bar for top-resize; only bottom/side resize.
        match (near_l, near_r, near_b) {
            (_, true, true) => Some(Edge::BottomRight),
            (true, _, true) => Some(Edge::BottomLeft),
            (_, true, _) => Some(Edge::Right),
            (true, _, _) if pt.y > r.y0 + TITLE_H => Some(Edge::Left),
            (_, _, true) => Some(Edge::Bottom),
            _ => None,
        }
    }

    fn apply_drag(&mut self, drag: Drag, pt: Point) {
        match drag {
            Drag::Move { grab } => {
                let w = self.rect.width();
                let h = self.rect.height();
                let x0 = pt.x - grab.x;
                let y0 = pt.y - grab.y;
                self.rect = Rect::new(x0, y0, x0 + w, y0 + h);
                self.clamp_into_bounds();
            }
            Drag::Resize { edge, start, from } => {
                let dx = pt.x - from.x;
                let dy = pt.y - from.y;
                let mut r = start;
                match edge {
                    Edge::Right => r.x1 = (start.x1 + dx).max(start.x0 + MIN_W),
                    Edge::Bottom => r.y1 = (start.y1 + dy).max(start.y0 + MIN_H),
                    Edge::BottomRight => {
                        r.x1 = (start.x1 + dx).max(start.x0 + MIN_W);
                        r.y1 = (start.y1 + dy).max(start.y0 + MIN_H);
                    }
                    Edge::Left => r.x0 = (start.x0 + dx).min(start.x1 - MIN_W),
                    Edge::BottomLeft => {
                        r.x0 = (start.x0 + dx).min(start.x1 - MIN_W);
                        r.y1 = (start.y1 + dy).max(start.y0 + MIN_H);
                    }
                }
                self.rect = r;
                self.clamp_into_bounds();
            }
        }
    }

    // ---- drawing ----

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.open {
            return;
        }
        let c = &theme.tokens.colors;
        // Scrim only when docked (a detached window doesn't dim the app).
        if matches!(self.dock, DockState::Docked) {
            fill(scene, t, self.bounds, c.surfaces.overlay);
        }
        let r = self.rect;
        self.draw_window_chrome(scene, t, tr, theme);
        self.draw_header_fields(scene, t, tr, theme);
        self.draw_body(scene, t, tr, theme, presence);

        // Attachment chips (staged real files — shown by their basename).
        self.draw_staged_attachment_chips(scene, t, tr, theme, r);

        // Inline prompt bar.
        self.draw_prompt(scene, t, tr, theme, presence);

        // Buttons.
        self.draw_action_button(scene, t, tr, theme, ComposeHit::Send, "Send", true);
        self.draw_action_button(
            scene,
            t,
            tr,
            theme,
            ComposeHit::SaveDraft,
            "Save draft",
            false,
        );
        self.draw_action_button(scene, t, tr, theme, ComposeHit::Discard, "Discard", false);

        // Resize affordance (bottom-right grip dots).
        let g = Rect::new(r.x1 - 14.0, r.y1 - 14.0, r.x1 - 4.0, r.y1 - 4.0);
        stroke_round(scene, t, g, 2.0, 1.2, c.border_white_20);

        let _ = presence;
    }

    /// Draw the header field rows (To, Cc/Bcc when shown, Subject), the
    /// Cc/Bcc-toggle + Attach chip buttons, and the formatting toolbar when
    /// in HTML mode. Part of [`Self::draw`].
    fn draw_header_fields(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.draw_field(
            scene,
            t,
            tr,
            theme,
            "To",
            &self.to_rect_for(ComposeHit::FieldTo),
            matches!(self.focus, Field::To),
            self.to.value().is_empty(),
            "recipients",
        );
        if self.show_cc_bcc {
            self.draw_field(
                scene,
                t,
                tr,
                theme,
                "Cc",
                &self.to_rect_for(ComposeHit::FieldCc),
                matches!(self.focus, Field::Cc),
                self.cc.value().is_empty(),
                "",
            );
            self.draw_field(
                scene,
                t,
                tr,
                theme,
                "Bcc",
                &self.to_rect_for(ComposeHit::FieldBcc),
                matches!(self.focus, Field::Bcc),
                self.bcc.value().is_empty(),
                "",
            );
        }
        self.draw_field(
            scene,
            t,
            tr,
            theme,
            "Subject",
            &self.to_rect_for(ComposeHit::FieldSubject),
            matches!(self.focus, Field::Subject),
            self.subject.value().is_empty(),
            "",
        );

        // Cc/Bcc toggle + Attach buttons. The active highlight reflects on/off.
        self.draw_chip_button(
            scene,
            t,
            tr,
            theme,
            ComposeHit::ToggleCcBcc,
            "Cc/Bcc",
            self.show_cc_bcc,
        );
        self.draw_chip_button(
            scene,
            t,
            tr,
            theme,
            ComposeHit::Attach,
            "\u{1F4CE} Attach",
            false,
        );

        // Toolbar.
        if matches!(self.format, ComposeFormat::Html) {
            self.draw_toolbar(scene, t, tr, theme);
        }
        self.draw_format_toggle(scene, t, tr, theme);
    }

    /// Draw the message-body card: border, and either the WYSIWYG editor
    /// (HTML mode, clipped to the card) or the plain multiline `TextInput`
    /// (Text mode). Part of [`Self::draw`].
    fn draw_body(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        let body = self.body_rect();
        fill_round(scene, t, body, 8.0, c.surfaces.base);
        stroke_round(
            scene,
            t,
            body,
            8.0,
            1.0,
            if matches!(self.focus, Field::Body) {
                c.border_focus
            } else {
                c.border_standard
            },
        );
        match self.format {
            ComposeFormat::Html => {
                scene.push_layer(
                    Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    t,
                    &body,
                );
                self.editor.draw(scene, t, tr, theme);
                scene.pop_layer();
            }
            ComposeFormat::Text => {
                // The shared multiline TextInput draws the wrapped body, caret,
                // selection, and placeholder inside the card (it clips itself).
                ui::widget::Widget::draw(&mut self.plain_body, scene, t, tr, theme, presence);
            }
        }
    }

    /// Draw the window's drop shadow, body, and title bar (heading + close/
    /// pop-out buttons). Part of [`Self::draw`].
    fn draw_window_chrome(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let r = self.rect;
        // Drop shadow.
        let shadow = Rect::new(r.x0 + 2.0, r.y0 + 4.0, r.x1 + 2.0, r.y1 + 6.0);
        scene.fill(
            Fill::NonZero,
            t,
            Color::from_rgba8(0, 0, 0, 90),
            None,
            &RoundedRect::from_rect(shadow, 14.0),
        );
        // Window body.
        fill_round(scene, t, r, 14.0, c.surfaces.raised);
        stroke_round(scene, t, r, 14.0, 1.0, c.border_white_10);

        // Title bar.
        let title = Rect::new(r.x0, r.y0, r.x1, r.y0 + TITLE_H);
        fill_round(
            scene,
            t,
            Rect::new(title.x0, title.y0, title.x1, title.y0 + 24.0),
            14.0,
            c.surfaces.base,
        );
        fill(
            scene,
            t,
            Rect::new(title.x0, title.y0 + 12.0, title.x1, title.y1),
            c.surfaces.base,
        );
        fill(
            scene,
            t,
            Rect::new(title.x0, title.y1 - 1.0, title.x1, title.y1),
            c.border_standard,
        );
        let heading = if self.title_hint.is_empty() {
            "New message".to_string()
        } else {
            self.title_hint.clone()
        };
        let heading = tr.elide_to_width(&heading, 14.0, (r.width() - 100.0) as f32);
        tr.draw_text_weighted(
            scene,
            &heading,
            14.0,
            c.text_primary,
            t * Affine::translate((r.x0 + PAD, r.y0 + 11.0)),
            None,
            TextWeight::Semibold,
        );
        // Window buttons.
        for (rect, hit) in &self.hits {
            let glyph = match hit {
                ComposeHit::Close => "\u{2715}",
                ComposeHit::PopOut => {
                    if matches!(self.dock, DockState::Detached) {
                        "\u{2750}"
                    } else {
                        "\u{2197}"
                    }
                }
                _ => continue,
            };
            let hov = rect.contains(self.last_mouse);
            if hov {
                fill_round(scene, t, *rect, 7.0, c.surface_white_10);
            }
            tr.draw_text(
                scene,
                glyph,
                13.0,
                c.neutral[8],
                t * Affine::translate((rect.x0 + 8.0, rect.y0 + 6.0)),
                None,
            );
        }
    }

    /// Draw the staged-attachment chips (real files, shown by basename) above
    /// the button row. Part of [`Self::draw`].
    fn draw_staged_attachment_chips(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        if self.attachments.is_empty() {
            return;
        }
        let c = &theme.tokens.colors;
        let mut ax = r.x0 + PAD;
        let ay = r.y1 - 52.0 - 30.0;
        for path in &self.attachments {
            let name = attachment_display_name(path);
            let (lw, _) = tr.measure(name, 11.5, None);
            let chip = Rect::new(ax, ay, ax + f64::from(lw) + 34.0, ay + 24.0);
            fill_round(scene, t, chip, 7.0, c.surface_white_10);
            tr.draw_text(
                scene,
                "\u{1F4CE}",
                12.0,
                c.text_dim,
                t * Affine::translate((chip.x0 + 7.0, chip.y0 + 4.0)),
                None,
            );
            tr.draw_text(
                scene,
                name,
                11.5,
                c.text_primary,
                t * Affine::translate((chip.x0 + 26.0, chip.y0 + 4.0)),
                None,
            );
            ax = chip.x1 + 8.0;
        }
    }

    fn to_rect_for(&self, hit: ComposeHit) -> Rect {
        self.hits
            .iter()
            .find(|(_, h)| *h == hit)
            .map_or(Rect::ZERO, |(r, _)| *r)
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_field(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        label: &str,
        fr: &Rect,
        focused: bool,
        empty: bool,
        placeholder: &str,
    ) {
        let c = &theme.tokens.colors;
        tr.draw_text(
            scene,
            label,
            12.0,
            c.text_dim,
            t * Affine::translate((self.rect.x0 + PAD, fr.y0 + 8.0)),
            None,
        );
        fill(
            scene,
            t,
            Rect::new(fr.x0, fr.y1 - 1.0, fr.x1, fr.y1),
            if focused {
                c.border_focus
            } else {
                c.border_standard
            },
        );
        if empty && !focused && !placeholder.is_empty() {
            tr.draw_text(
                scene,
                placeholder,
                12.5,
                c.neutral[4],
                t * Affine::translate((fr.x0 + 2.0, fr.y0 + 7.0)),
                None,
            );
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "each parameter is a distinct, independently-meaningful piece of one small button's draw call (the universal scene/t/tr/theme quad plus this button's own identity/state); bundling them would just move the same count into a field list"
    )]
    fn draw_chip_button(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hit: ComposeHit,
        label: &str,
        active: bool,
    ) {
        let c = &theme.tokens.colors;
        let Some(rect) = self.hits.iter().find(|(_, h)| *h == hit).map(|(r, _)| *r) else {
            return;
        };
        let hov = rect.contains(self.last_mouse);
        let bg = if active {
            c.accent[5].with_alpha_factor(0.22)
        } else if hov {
            c.surface_white_10
        } else {
            c.surface_white_5
        };
        fill_round(scene, t, rect, 7.0, bg);
        let (lw, _) = tr.measure(label, 11.5, None);
        tr.draw_text(
            scene,
            label,
            11.5,
            c.neutral[8],
            t * Affine::translate((
                f64::midpoint(rect.x0, rect.x1) - f64::from(lw) / 2.0,
                rect.y0 + 8.0,
            )),
            None,
        );
    }

    fn draw_toolbar(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let marks = self.editor.active_marks();
        let bk = self.editor.active_block_kind();
        for (rect, hit) in &self.hits {
            let (glyph, active, weight, italic) = match hit {
                ComposeHit::Bold => ("B", marks.bold, TextWeight::Bold, false),
                ComposeHit::Italic => ("I", marks.italic, TextWeight::Regular, true),
                ComposeHit::Underline => ("U", marks.underline, TextWeight::Regular, false),
                ComposeHit::H1 => (
                    "H1",
                    matches!(bk, BlockKind::Heading(1)),
                    TextWeight::Bold,
                    false,
                ),
                ComposeHit::H2 => (
                    "H2",
                    matches!(bk, BlockKind::Heading(2)),
                    TextWeight::Bold,
                    false,
                ),
                ComposeHit::Bullet => (
                    "\u{2022}",
                    matches!(bk, BlockKind::Bullet),
                    TextWeight::Regular,
                    false,
                ),
                ComposeHit::Numbered => (
                    "1.",
                    matches!(bk, BlockKind::Numbered),
                    TextWeight::Regular,
                    false,
                ),
                ComposeHit::Quote => (
                    "\u{201C}",
                    matches!(bk, BlockKind::Quote),
                    TextWeight::Regular,
                    false,
                ),
                ComposeHit::Link => ("\u{1F517}", false, TextWeight::Regular, false),
                _ => continue,
            };
            let hov = rect.contains(self.last_mouse);
            let bg = if active {
                c.accent[5].with_alpha_factor(0.28)
            } else if hov {
                c.surface_white_10
            } else {
                c.surface_white_5
            };
            fill_round(scene, t, *rect, 6.0, bg);
            let col = if active { c.text_primary } else { c.neutral[8] };
            let (gw, _) = tr.measure_weighted(glyph, 13.0, None, weight);
            tr.draw_text_styled(
                scene,
                glyph,
                13.0,
                col,
                t * Affine::translate((
                    f64::midpoint(rect.x0, rect.x1) - f64::from(gw) / 2.0,
                    rect.y0 + 7.0,
                )),
                None,
                weight,
                italic,
                false,
            );
            if matches!(hit, ComposeHit::Underline) {
                let uy = rect.y0 + 7.0 + 13.0;
                fill(
                    scene,
                    t,
                    Rect::new(
                        f64::midpoint(rect.x0, rect.x1) - f64::from(gw) / 2.0,
                        uy,
                        f64::midpoint(rect.x0, rect.x1) + f64::from(gw) / 2.0,
                        uy + 1.0,
                    ),
                    col,
                );
            }
        }
    }

    fn draw_format_toggle(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let Some(rect) = self
            .hits
            .iter()
            .find(|(_, h)| *h == ComposeHit::FormatToggle)
            .map(|(r, _)| *r)
        else {
            return;
        };
        let hov = rect.contains(self.last_mouse);
        fill_round(
            scene,
            t,
            rect,
            7.0,
            if hov {
                c.surface_white_10
            } else {
                c.surface_white_5
            },
        );
        stroke_round(scene, t, rect, 7.0, 1.0, c.border_white_10);
        let label = match self.format {
            ComposeFormat::Html => "\u{25CF} HTML",
            ComposeFormat::Text => "\u{25CB} Text",
        };
        let (lw, _) = tr.measure(label, 11.5, None);
        tr.draw_text(
            scene,
            label,
            11.5,
            c.neutral[8],
            t * Affine::translate((
                f64::midpoint(rect.x0, rect.x1) - f64::from(lw) / 2.0,
                rect.y0 + 8.0,
            )),
            None,
        );
    }

    fn draw_prompt(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some((kind, error)) = self.prompt.as_ref().map(|p| (p.kind, p.error.clone())) else {
            return;
        };
        let field = self.to_rect_for(ComposeHit::PromptField);
        let confirm = self.to_rect_for(ComposeHit::PromptConfirm);
        let cancel = self.to_rect_for(ComposeHit::PromptCancel);
        if field == Rect::ZERO {
            return;
        }
        let c = &theme.tokens.colors;
        let panel = Rect::new(
            self.rect.x0 + PAD - 6.0,
            field.y0 - 8.0,
            self.rect.x1 - PAD + 6.0,
            field.y1 + 8.0,
        );
        fill_round(scene, t, panel, 9.0, c.surfaces.raised);
        stroke_round(
            scene,
            t,
            panel,
            9.0,
            1.0,
            if error.is_some() {
                c.semantic.danger
            } else {
                c.border_white_10
            },
        );
        let label = match kind {
            PromptKind::Attach => "Attach",
            PromptKind::Link => "Link",
        };
        tr.draw_text(
            scene,
            label,
            12.0,
            c.text_dim,
            t * Affine::translate((panel.x0 + 10.0, field.y0 + 8.0)),
            None,
        );
        // Input chrome.
        fill_round(
            scene,
            t,
            Rect::new(
                field.x0 - 2.0,
                field.y0 - 1.0,
                field.x1 + 2.0,
                field.y1 + 1.0,
            ),
            6.0,
            c.surfaces.base,
        );
        stroke_round(
            scene,
            t,
            Rect::new(
                field.x0 - 2.0,
                field.y0 - 1.0,
                field.x1 + 2.0,
                field.y1 + 1.0,
            ),
            6.0,
            1.0,
            c.border_focus,
        );
        // Buttons.
        self.draw_prompt_buttons(scene, t, tr, theme, confirm, cancel);
        if let Some(e) = &error {
            tr.draw_text(
                scene,
                e,
                10.5,
                c.semantic.danger,
                t * Affine::translate((panel.x0 + 10.0, panel.y1 - 2.0)),
                None,
            );
        }
        if let Some(p) = self.prompt.as_mut() {
            p.input.draw(scene, t, tr, theme, presence);
        }
    }

    /// Draw the inline prompt's Add/Cancel buttons. Part of
    /// [`Self::draw_prompt`].
    #[allow(
        clippy::too_many_arguments,
        reason = "each parameter is a distinct, independently-meaningful piece of this draw call (the universal scene/t/tr/theme quad plus the two button rects); bundling them would just move the same count into a field list"
    )]
    fn draw_prompt_buttons(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        confirm: Rect,
        cancel: Rect,
    ) {
        let c = &theme.tokens.colors;
        let hov_c = confirm.contains(self.last_mouse);
        fill_round(
            scene,
            t,
            confirm,
            7.0,
            if hov_c { c.accent[4] } else { c.accent[5] },
        );
        let (aw, _) = tr.measure_weighted("Add", 12.5, None, TextWeight::Semibold);
        tr.draw_text_weighted(
            scene,
            "Add",
            12.5,
            Color::WHITE,
            t * Affine::translate((
                f64::midpoint(confirm.x0, confirm.x1) - f64::from(aw) / 2.0,
                confirm.y0 + 7.0,
            )),
            None,
            TextWeight::Semibold,
        );
        let hov_x = cancel.contains(self.last_mouse);
        fill_round(
            scene,
            t,
            cancel,
            7.0,
            if hov_x {
                c.surface_white_10
            } else {
                c.surface_white_5
            },
        );
        let (cw, _) = tr.measure("Cancel", 12.0, None);
        tr.draw_text(
            scene,
            "Cancel",
            12.0,
            c.neutral[8],
            t * Affine::translate((
                f64::midpoint(cancel.x0, cancel.x1) - f64::from(cw) / 2.0,
                cancel.y0 + 7.0,
            )),
            None,
        );
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "each parameter is a distinct, independently-meaningful piece of one small button's draw call (the universal scene/t/tr/theme quad plus this button's own identity/state); bundling them would just move the same count into a field list"
    )]
    fn draw_action_button(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hit: ComposeHit,
        label: &str,
        primary: bool,
    ) {
        let c = &theme.tokens.colors;
        let Some(rect) = self.hits.iter().find(|(_, h)| *h == hit).map(|(r, _)| *r) else {
            return;
        };
        let hov = rect.contains(self.last_mouse);
        if primary {
            fill_round(
                scene,
                t,
                rect,
                9.0,
                if hov { c.accent[4] } else { c.accent[5] },
            );
            let (w, _) = tr.measure_weighted(label, 13.5, None, TextWeight::Semibold);
            tr.draw_text_weighted(
                scene,
                label,
                13.5,
                Color::WHITE,
                t * Affine::translate((
                    f64::midpoint(rect.x0, rect.x1) - f64::from(w) / 2.0,
                    rect.y0 + 10.0,
                )),
                None,
                TextWeight::Semibold,
            );
        } else {
            fill_round(
                scene,
                t,
                rect,
                9.0,
                if hov {
                    c.surface_white_10
                } else {
                    c.surface_white_5
                },
            );
            stroke_round(scene, t, rect, 9.0, 1.0, c.border_white_10);
            let (w, _) = tr.measure(label, 13.0, None);
            tr.draw_text(
                scene,
                label,
                13.0,
                c.neutral[8],
                t * Affine::translate((
                    f64::midpoint(rect.x0, rect.x1) - f64::from(w) / 2.0,
                    rect.y0 + 10.0,
                )),
                None,
            );
        }
    }

    /// Draw the embedded header inputs (their text + caret). Called by the host
    /// after `draw` so they paint on top of the field chrome.
    pub fn draw_inputs(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.open {
            return;
        }
        self.to.draw(scene, t, tr, theme, presence);
        if self.show_cc_bcc {
            self.cc.draw(scene, t, tr, theme, presence);
            self.bcc.draw(scene, t, tr, theme, presence);
        }
        self.subject.draw(scene, t, tr, theme, presence);
    }
}

// ---- small scene helpers ----

fn fill(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &rect);
}
fn fill_round(scene: &mut Scene, t: Affine, rect: Rect, r: f64, color: Color) {
    scene.fill(
        Fill::NonZero,
        t,
        color,
        None,
        &RoundedRect::from_rect(rect, r),
    );
}
fn stroke_round(scene: &mut Scene, t: Affine, rect: Rect, r: f64, w: f64, color: Color) {
    scene.stroke(
        &Stroke::new(w),
        t,
        color,
        None,
        &RoundedRect::from_rect(rect, r),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win() -> ComposeWindow {
        let mut w = ComposeWindow::new(ComposeFormat::Html);
        w.open_blank();
        w.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        w
    }

    #[test]
    fn format_toggle_round_trips_content() {
        let mut w = win();
        w.set_focus(Field::Body);
        w.editor.set_plain("Hello world");
        w.toggle_format();
        assert_eq!(w.format(), ComposeFormat::Text);
        assert!(w.plain_body.value().contains("Hello world"));
        w.toggle_format();
        assert_eq!(w.format(), ComposeFormat::Html);
    }

    #[test]
    fn draft_carries_html_in_html_mode() {
        let mut w = win();
        w.set_focus(Field::Body);
        w.editor.set_plain("Body");
        w.to.set_text("a@b.example");
        let d = w.to_draft();
        assert!(d.html_body.is_some());
        assert!(d.html_body.unwrap().contains("Body"));
        assert_eq!(d.to, "a@b.example");
    }

    #[test]
    fn draft_is_plain_in_text_mode() {
        let mut w = ComposeWindow::new(ComposeFormat::Text);
        w.open_blank();
        w.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        w.plain_body.set_text("Just text");
        let d = w.to_draft();
        assert!(d.html_body.is_none());
        assert_eq!(d.body, "Just text");
    }

    /// The plain body is now a shared multiline `TextInput`: typing a newline
    /// through the window's real keyboard path lands a genuinely multi-line
    /// draft body — the thing a single-line field could not do.
    #[test]
    fn plain_body_accepts_a_multiline_draft_via_the_real_key_path() {
        use winit::event::KeyEvent;
        use winit::keyboard::{Key, NamedKey, SmolStr};

        fn key(logical: Key, text: Option<&str>) -> WindowEvent {
            let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
            ev.logical_key = logical;
            ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified,
            );
            ev.location = winit::keyboard::KeyLocation::Standard;
            ev.state = ElementState::Pressed;
            ev.repeat = false;
            ev.text = text.map(SmolStr::new);
            WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: ev,
                is_synthetic: false,
            }
        }

        let mut w = ComposeWindow::new(ComposeFormat::Text);
        w.open_blank();
        w.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        w.set_focus(Field::Body);

        let m = winit::event::Modifiers::default();
        let mut doc = automerge::AutoCommit::new();
        let taffy: taffy::TaffyTree<()> = taffy::TaffyTree::new();
        let node = taffy::NodeId::new(0);
        let mut send = |ev: &WindowEvent| {
            w.handle_event(ev, 1.0, &m, &mut doc, &automerge::ROOT, node, &taffy);
        };
        send(&key(Key::Character(SmolStr::new("H")), Some("H")));
        send(&key(Key::Character(SmolStr::new("i")), Some("i")));
        send(&key(Key::Named(NamedKey::Enter), None));
        send(&key(Key::Character(SmolStr::new("t")), Some("t")));
        send(&key(Key::Character(SmolStr::new("w")), Some("w")));
        send(&key(Key::Character(SmolStr::new("o")), Some("o")));

        assert_eq!(
            w.to_draft().body,
            "Hi\ntwo",
            "Enter split the body across two lines"
        );
    }

    #[test]
    fn move_keeps_window_in_bounds() {
        let mut w = win();
        let before = w.window_rect();
        // Drag the title far past the right edge; clamp must keep it reachable.
        w.drag = Some(Drag::Move {
            grab: Point::new(10.0, 10.0),
        });
        w.apply_drag(w.drag.unwrap(), Point::new(5000.0, 5000.0));
        let r = w.window_rect();
        assert!(r.x0 < 1280.0, "window stayed reachable horizontally");
        assert!(r.y0 < 800.0, "window stayed reachable vertically");
        assert_eq!(r.width(), before.width(), "move does not resize");
    }

    #[test]
    fn resize_respects_minimum() {
        let mut w = win();
        let start = w.window_rect();
        w.apply_drag(
            Drag::Resize {
                edge: Edge::BottomRight,
                start,
                from: Point::new(start.x1, start.y1),
            },
            Point::new(start.x0, start.y0),
        );
        let r = w.window_rect();
        assert!(r.width() >= MIN_W - 0.5);
        assert!(r.height() >= MIN_H - 0.5);
    }

    #[test]
    fn detach_toggles_dock_state() {
        let mut w = win();
        assert_eq!(w.dock(), DockState::Docked);
        w.toggle_dock();
        assert_eq!(w.dock(), DockState::Detached);
    }

    fn press_at(w: &mut ComposeWindow, p: Point) -> (bool, ComposeAction) {
        let m = winit::event::Modifiers::default();
        let mut doc = automerge::AutoCommit::new();
        let taffy: taffy::TaffyTree<()> = taffy::TaffyTree::new();
        let node = taffy::NodeId::new(0);
        let mv = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        };
        w.handle_event(&mv, 1.0, &m, &mut doc, &automerge::ROOT, node, &taffy);
        let pr = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        w.handle_event(&pr, 1.0, &m, &mut doc, &automerge::ROOT, node, &taffy)
    }

    fn hit_rect(w: &ComposeWindow, hit: ComposeHit) -> Rect {
        w.hits
            .iter()
            .find(|(_, h)| *h == hit)
            .map(|(r, _)| *r)
            .expect("hit present")
    }

    #[test]
    fn attach_button_opens_prompt_and_stages_a_real_file() {
        // Stage a real file to attach.
        let path = std::env::temp_dir().join(format!("mail-compose-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"body").unwrap();

        let mut w = win();
        // Click the Attach button through the real hit path → opens the prompt
        // (no fabricated filename is staged).
        let attach = hit_rect(&w, ComposeHit::Attach);
        let (consumed, _) = press_at(&mut w, attach.center());
        assert!(consumed);
        assert!(w.prompt.is_some(), "Attach opens the path prompt");
        assert!(w.attachments.is_empty(), "no fabricated attachment staged");

        // Type the real path and confirm → the real file is staged.
        w.prompt
            .as_mut()
            .unwrap()
            .input
            .set_text(&path.to_string_lossy());
        w.confirm_prompt();
        assert!(w.prompt.is_none(), "a valid path closes the prompt");
        assert_eq!(w.attachments.len(), 1);
        assert_eq!(
            w.to_draft().attachments,
            vec![path.to_string_lossy().to_string()]
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn attach_prompt_rejects_missing_path() {
        let mut w = win();
        w.open_prompt(PromptKind::Attach);
        w.prompt
            .as_mut()
            .unwrap()
            .input
            .set_text("/no/such/mail/file-xyz");
        w.confirm_prompt();
        assert!(w.attachments.is_empty(), "a missing path stages nothing");
        assert!(
            w.prompt.as_ref().is_some_and(|p| p.error.is_some()),
            "the prompt stays open showing an error"
        );
    }

    #[test]
    fn link_prompt_applies_the_typed_href_not_a_placeholder() {
        let mut w = win();
        w.set_focus(Field::Body);
        w.open_prompt(PromptKind::Link);
        // Seeded with a scheme the user edits — never a hardcoded final target.
        assert_eq!(w.prompt.as_ref().unwrap().input.value(), "https://");
        w.prompt
            .as_mut()
            .unwrap()
            .input
            .set_text("https://qos.example/docs");
        w.confirm_prompt();
        let html = w.editor.to_html();
        assert!(
            html.contains("href=\"https://qos.example/docs\""),
            "got: {html}"
        );
        assert!(
            !html.contains("href=\"https://\""),
            "must not link the bare scheme"
        );
    }

    #[test]
    fn click_outside_window_is_consumed_not_passed_through() {
        let mut w = win();
        w.last_mouse = Point::new(5.0, 5.0); // outside the centred window
        let modifiers = winit::event::Modifiers::default();
        let mut doc = automerge::AutoCommit::new();
        let taffy: taffy::TaffyTree<()> = taffy::TaffyTree::new();
        let press = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        let (consumed, action) = w.handle_event(
            &press,
            1.0,
            &modifiers,
            &mut doc,
            &automerge::ROOT,
            taffy::NodeId::new(0),
            &taffy,
        );
        assert!(
            consumed,
            "clicks outside the modal window must be swallowed, not fall through"
        );
        assert_eq!(
            action,
            ComposeAction::None,
            "outside click does not close (explicit Discard only)"
        );
        assert!(w.open, "window stays open");
    }
}
