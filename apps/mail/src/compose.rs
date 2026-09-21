//! The Compose window — a **movable, resizable, focus-capturing** floating
//! panel for writing a message.
//!
//! ## Focus capture
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
//! ## Detach (Tilo pop-out)
//! [`DockState`] models two placements: `Docked` (centred in-app modal) and
//! `Detached` (parked beside the app content, simulating a separate window).
//! Both are movable + resizable. The Pop-out button toggles between them. A
//! second top-level surface beside the app needs the Tilo compositor's
//! window-group / multi-window support (`shell`), which is not available through
//! the single-window `ui::App`; when that lands, a detached draft is handed back
//! via [`ComposeWindow::reattach`].

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use ui::surface::{card, edge_on, edge_outline, pill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::Widget;
use ui::color_compat::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, TextInput};

use crate::model::{ComposeFormat, Draft, DraftAttachment};
use crate::wysiwyg::{BlockKind, InlineMark, Wysiwyg};

/// Authored 1× bands; the measured rules ([`ComposeMetrics`]) never go below
/// them, so 1× geometry is unchanged and a larger text scale grows the window's
/// rows instead of stacking them.
const TITLE_H: f64 = 38.0;
const TOOLBAR_H: f64 = 36.0;
const FIELD_H: f64 = 32.0;
const BUTTON_H: f64 = 36.0;
const PAD: f64 = 14.0;
const LABEL_W: f64 = 54.0;
const MIN_W: f64 = 380.0;
const MIN_H: f64 = 340.0;
const RESIZE_GRIP: f64 = 8.0;
/// Gap between header field rows: related rows that read as one group, so
/// the `s2` step (HIG §7b).
const HEADER_ROW_GAP: f64 = ui::tokens::spacing::Spacing::BASE.s2 as f64;
/// Gap between two formatting-toolbar glyph squares (`s1`) and the extra
/// gap after the inline-marks group (`s2`, so the group gap is `s3`).
const TOOLBAR_GLYPH_GAP: f64 = ui::tokens::spacing::Spacing::BASE.s1 as f64;
const TOOLBAR_GROUP_EXTRA_GAP: f64 = ui::tokens::spacing::Spacing::BASE.s2 as f64;
/// Default window size at 1×; scaled with the text so the header rows,
/// toolbar and button bar still leave a usable body at 200%.
const DEFAULT_W: f64 = 560.0;
const DEFAULT_H: f64 = 460.0;
/// Renderer-relative text sizes shared by measurement and paint.
const TITLE_SIZE: f32 = 14.0;
const LABEL_SIZE: f32 = 12.0;
const CHIP_SIZE: f32 = 11.5;
const TOOLBAR_GLYPH_SIZE: f32 = 13.0;
const BUTTON_SIZE: f32 = 13.0;

/// Sizes the compose window is laid out with, measured on paint with the
/// styles it paints with. `Default` is the authored 1× rule set, used before
/// the first paint and by the geometry tests (which never paint).
#[derive(Clone, Copy, Debug, PartialEq)]
struct ComposeMetrics {
    scale: f64,
    /// Title bar: its label's line box plus `s2` each side.
    title_h: f64,
    /// Header label line box.
    label_h: f64,
    /// Header field: the shared input's line box plus `s2` each side.
    field_h: f64,
    /// Toolbar row: a glyph line plus `s2` each side plus the row's `6 px`
    /// gap to the body.
    toolbar_h: f64,
    /// Action button height: its label line box plus `s2` each side.
    button_h: f64,
    /// Measured widths (label + pads) of the chip and action buttons.
    cc_bcc_w: f64,
    attach_w: f64,
    toggle_w: f64,
    send_w: f64,
    draft_w: f64,
    discard_w: f64,
    /// Measured widths of the inline prompt bar's Add / Cancel buttons.
    prompt_add_w: f64,
    prompt_cancel_w: f64,
}

impl Default for ComposeMetrics {
    fn default() -> Self {
        Self {
            scale: 1.0,
            title_h: TITLE_H,
            label_h: 15.0,
            field_h: FIELD_H,
            toolbar_h: TOOLBAR_H,
            button_h: BUTTON_H,
            cc_bcc_w: 64.0,
            attach_w: 84.0,
            toggle_w: 96.0,
            send_w: 96.0,
            draft_w: 110.0,
            discard_w: 96.0,
            prompt_add_w: 66.0,
            prompt_cancel_w: 72.0,
        }
    }
}

impl ComposeMetrics {
    fn measure(tr: &mut TextRenderer, theme: &Theme) -> Self {
        let sp = &theme.tokens.spacing;
        let (s2, s3, s4) = (f64::from(sp.s2), f64::from(sp.s3), f64::from(sp.s4));
        let line = |tr: &mut TextRenderer, size: f32, weight: TextWeight| {
            f64::from(tr.measure_weighted("Ag", size, None, weight).1)
        };
        let width = |tr: &mut TextRenderer, key: &str, size: f32, weight: TextWeight| {
            f64::from(
                tr.measure_weighted(&crate::i18n::t(key), size, None, weight)
                    .0,
            )
        };
        let d = Self::default();
        Self {
            scale: f64::from(theme.text_scale).max(1.0),
            title_h: (line(tr, TITLE_SIZE, TextWeight::Semibold) + 2.0 * s2).max(TITLE_H),
            label_h: line(tr, LABEL_SIZE, TextWeight::Regular),
            field_h: (line(tr, 14.0, TextWeight::Regular) + 2.0 * s2).max(FIELD_H),
            toolbar_h: (line(tr, TOOLBAR_GLYPH_SIZE, TextWeight::Bold) + 2.0 * s2 + 6.0)
                .max(TOOLBAR_H),
            button_h: (line(tr, BUTTON_SIZE, TextWeight::Semibold) + 2.0 * s2).max(BUTTON_H),
            cc_bcc_w: (width(tr, "mail-compose-cc-bcc", CHIP_SIZE, TextWeight::Regular) + 2.0 * s3)
                .max(d.cc_bcc_w),
            attach_w: (width(tr, "mail-compose-attach", CHIP_SIZE, TextWeight::Regular) + 2.0 * s3)
                .max(d.attach_w),
            toggle_w: (f64::from(tr.measure("\u{25CF} HTML", CHIP_SIZE, None).0) + 2.0 * s4)
                .max(d.toggle_w),
            send_w: (width(tr, "mail-compose-send", BUTTON_SIZE, TextWeight::Semibold) + 2.0 * s4)
                .max(d.send_w),
            draft_w: (width(
                tr,
                "mail-compose-save-draft",
                BUTTON_SIZE,
                TextWeight::Regular,
            ) + 2.0 * s4)
                .max(d.draft_w),
            discard_w: (width(tr, "mail-compose-discard", BUTTON_SIZE, TextWeight::Regular)
                + 2.0 * s4)
                .max(d.discard_w),
            prompt_add_w: (width(tr, "mail-compose-prompt-add", 12.5, TextWeight::Semibold)
                + 2.0 * s3)
                .max(d.prompt_add_w),
            prompt_cancel_w: (width(tr, "mail-compose-prompt-cancel", 12.0, TextWeight::Regular)
                + 2.0 * s3)
                .max(d.prompt_cancel_w),
        }
    }
}

pub(crate) struct ComposeEventContext<'a> {
    pub(crate) scale_factor: f64,
    pub(crate) modifiers: winit::event::Modifiers,
    pub(crate) state: &'a mut automerge::AutoCommit,
    pub(crate) path: &'a automerge::ObjId,
    pub(crate) node_id: taffy::NodeId,
    pub(crate) taffy: &'a taffy::TaffyTree<()>,
}

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
    // inline URL prompt
    PromptField,
    PromptConfirm,
    PromptCancel,
}

/// What the inline prompt bar is collecting.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PromptKind {
    /// A URL to link the current selection (or insert) to.
    Link,
}

/// A small inline entry bar shown inside the compose window when the user needs
/// to type a link target. File attachments use the host portal, preserving the
/// selected descriptor rather than accepting arbitrary host paths.
struct ComposePrompt {
    kind: PromptKind,
    input: TextInput,
    error: Option<String>,
}

/// What the host should do after dispatching a window action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposeAction {
    None,
    RequestAttachment,
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
    /// Plain-text body (Text mode), backed by a shared multiline [`TextInput`].
    plain_body: TextInput,

    /// Portable qdoc attachment references, never host paths.
    attachments: Vec<DraftAttachment>,
    /// The inline URL prompt, when open.
    prompt: Option<ComposePrompt>,
    focus: Field,
    drag: Option<Drag>,
    last_mouse: Point,
    hits: Vec<(Rect, ComposeHit)>,
    dirty: bool,
    /// The reply target subject line, shown in the title when replying.
    title_hint: String,
    /// Sizes the window is laid out with, measured on paint (the authored
    /// 1× rules until then).
    metrics: ComposeMetrics,
    /// The band above the button row that `relayout_regions` reserved for
    /// the staged-attachment chips (empty when there are none); the chip
    /// painter reads this instead of re-deriving the offset.
    chips_band: Rect,
    /// The band `relayout_regions` reserved for the inline prompt bar (empty
    /// when no prompt is open).
    prompt_band: Rect,
}

impl ComposeWindow {
    pub fn new(format: ComposeFormat) -> Self {
        let mk = || {
            let mut t = TextInput::local();
            t.set_plain_chrome(true);
            t
        };
        // The recipients hint belongs to the shared field so it shares the
        // typed text's font, gutter and centre line (the header used to paint
        // its own placeholder over the box at a fixed offset).
        let mut to = TextInput::local().with_placeholder("mail-compose-placeholder-to");
        to.set_plain_chrome(true);
        Self {
            open: false,
            dock: DockState::Docked,
            rect: Rect::ZERO,
            bounds: Rect::ZERO,
            placed: false,
            to,
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
            metrics: ComposeMetrics::default(),
            chips_band: Rect::ZERO,
            prompt_band: Rect::ZERO,
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

    /// Semantic mirror of the immediate-mode compose surface. Bounds come from
    /// the same hit map as pointer input and values come from the retained text
    /// fields, so screen readers and automation operate the editor users see.
    pub fn accessibility_node(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};
        let rect_for = |hit| {
            self.hits
                .iter()
                .find(|(_, candidate)| *candidate == hit)
                .map(|(rect, _)| *rect)
        };
        let field = |name: &str, target: &str, value: String, rect: Option<Rect>, multiline| {
            let mut node = AccessNode::new(if multiline {
                Role::MultilineTextInput
            } else {
                Role::TextInput
            })
            .with_name(name)
            .with_value(value)
            .with_action_target(format!("mail.compose:{target}"))
            .with_state(AccessState::new().focusable(true).multiline(multiline));
            if let Some(rect) = rect {
                node = node.with_bounds(rect);
            }
            node
        };
        let mut children = vec![
            field(
                "To",
                "to",
                self.to.value().to_string(),
                rect_for(ComposeHit::FieldTo),
                false,
            ),
            field(
                "Subject",
                "subject",
                self.subject.value().to_string(),
                rect_for(ComposeHit::FieldSubject),
                false,
            ),
            field(
                "Message body",
                "body",
                if self.format == ComposeFormat::Html {
                    self.editor.to_plain()
                } else {
                    self.plain_body.value().to_string()
                },
                rect_for(ComposeHit::Body),
                true,
            ),
        ];
        if self.show_cc_bcc {
            children.insert(
                1,
                field(
                    "Cc",
                    "cc",
                    self.cc.value().to_string(),
                    rect_for(ComposeHit::FieldCc),
                    false,
                ),
            );
            children.insert(
                2,
                field(
                    "Bcc",
                    "bcc",
                    self.bcc.value().to_string(),
                    rect_for(ComposeHit::FieldBcc),
                    false,
                ),
            );
        }
        for (hit, name, target, checked) in self.accessibility_control_specs() {
            let Some(rect) = rect_for(hit) else { continue };
            let role = if checked.is_some() {
                Role::ToggleButton
            } else {
                Role::Button
            };
            children.push(
                AccessNode::new(role)
                    .with_name(name)
                    .with_bounds(rect)
                    .with_action_target(format!("mail.compose:{target}"))
                    .with_state(AccessState {
                        focusable: true,
                        checked,
                        ..Default::default()
                    }),
            );
        }
        AccessNode::new(Role::Dialog)
            .with_name("Compose email")
            .with_description("Email composer")
            .with_bounds(self.rect)
            .with_state(AccessState::new().modal(true))
            .with_children(children)
    }

    fn accessibility_control_specs(
        &self,
    ) -> Vec<(ComposeHit, &'static str, &'static str, Option<bool>)> {
        let marks = self.editor.active_marks();
        let block = self.editor.active_block_kind();
        vec![
            (ComposeHit::Send, "Send", "send", None),
            (ComposeHit::SaveDraft, "Save draft", "save", None),
            (ComposeHit::Discard, "Discard", "discard", None),
            (ComposeHit::Attach, "Attach file", "attach", None),
            (
                ComposeHit::ToggleCcBcc,
                "Show Cc and Bcc",
                "cc-bcc",
                Some(self.show_cc_bcc),
            ),
            (
                ComposeHit::FormatToggle,
                "Use HTML formatting",
                "format",
                Some(self.format == ComposeFormat::Html),
            ),
            (ComposeHit::Bold, "Bold", "bold", Some(marks.bold)),
            (ComposeHit::Italic, "Italic", "italic", Some(marks.italic)),
            (
                ComposeHit::Underline,
                "Underline",
                "underline",
                Some(marks.underline),
            ),
            (
                ComposeHit::H1,
                "Heading 1",
                "heading-1",
                Some(matches!(block, BlockKind::Heading(1))),
            ),
            (
                ComposeHit::H2,
                "Heading 2",
                "heading-2",
                Some(matches!(block, BlockKind::Heading(2))),
            ),
            (
                ComposeHit::Bullet,
                "Bulleted list",
                "bullet",
                Some(matches!(block, BlockKind::Bullet)),
            ),
            (
                ComposeHit::Numbered,
                "Numbered list",
                "numbered",
                Some(matches!(block, BlockKind::Numbered)),
            ),
            (
                ComposeHit::Quote,
                "Quote",
                "quote",
                Some(matches!(block, BlockKind::Quote)),
            ),
            (ComposeHit::Link, "Insert link", "link", None),
        ]
    }

    pub fn perform_semantic_action(
        &mut self,
        control: &str,
        action: &ui::a11y::Action,
    ) -> Option<ComposeAction> {
        use ui::a11y::Action;
        let field_action = |field: &mut TextInput, action: &Action| match action {
            Action::SetValue(value) => {
                field.set_text(value);
                true
            }
            _ => false,
        };
        match control {
            "to" | "cc" | "bcc" | "subject" => {
                let focus = match control {
                    "cc" => Field::Cc,
                    "bcc" => Field::Bcc,
                    "subject" => Field::Subject,
                    _ => Field::To,
                };
                match action {
                    Action::Focus | Action::Activate => self.set_focus(focus),
                    Action::SetValue(_) => {
                        let target = match focus {
                            Field::Cc => &mut self.cc,
                            Field::Bcc => &mut self.bcc,
                            Field::Subject => &mut self.subject,
                            _ => &mut self.to,
                        };
                        let _ = field_action(target, action);
                    }
                    _ => return None,
                }
            }
            "body" => match action {
                Action::Focus | Action::Activate => self.set_focus(Field::Body),
                Action::SetValue(value) => {
                    if self.format == ComposeFormat::Html {
                        self.editor.set_plain(value);
                    } else {
                        self.plain_body.set_text(value);
                    }
                }
                _ => return None,
            },
            "send" if matches!(action, Action::Activate) => return Some(ComposeAction::Send),
            "save" if matches!(action, Action::Activate) => return Some(ComposeAction::SaveDraft),
            "discard" if matches!(action, Action::Activate) => {
                self.close();
                return Some(ComposeAction::Close);
            }
            "attach" if matches!(action, Action::Activate) => {
                return Some(ComposeAction::RequestAttachment);
            }
            "cc-bcc" if matches!(action, Action::Activate | Action::ToggleCheck) => {
                self.show_cc_bcc = !self.show_cc_bcc;
                self.relayout_regions();
            }
            "format" if matches!(action, Action::Activate | Action::ToggleCheck) => {
                self.toggle_format();
            }
            "bold" if matches!(action, Action::Activate | Action::ToggleCheck) => {
                self.editor.toggle_inline(InlineMark::Bold);
            }
            "italic" if matches!(action, Action::Activate | Action::ToggleCheck) => {
                self.editor.toggle_inline(InlineMark::Italic);
            }
            "underline" if matches!(action, Action::Activate | Action::ToggleCheck) => {
                self.editor.toggle_inline(InlineMark::Underline);
            }
            "heading-1" if matches!(action, Action::Activate | Action::ToggleCheck) => {
                self.editor.set_block_kind(BlockKind::Heading(1));
            }
            "heading-2" if matches!(action, Action::Activate | Action::ToggleCheck) => {
                self.editor.set_block_kind(BlockKind::Heading(2));
            }
            "bullet" if matches!(action, Action::Activate | Action::ToggleCheck) => {
                self.editor.set_block_kind(BlockKind::Bullet);
            }
            "numbered" if matches!(action, Action::Activate | Action::ToggleCheck) => {
                self.editor.set_block_kind(BlockKind::Numbered);
            }
            "quote" if matches!(action, Action::Activate | Action::ToggleCheck) => {
                self.editor.set_block_kind(BlockKind::Quote);
            }
            "link" if matches!(action, Action::Activate) => self.open_prompt(PromptKind::Link),
            _ => return None,
        }
        self.dirty = true;
        Some(ComposeAction::None)
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
            calendar_ics: None,
            attachments: self.attachments.clone(),
            in_reply_to: None,
        }
    }

    /// Add a verified portal-staged attachment, enforcing the same bounds the
    /// replicated draft action will enforce on every collaborator.
    ///
    /// # Errors
    ///
    /// Returns an error when metadata is invalid/duplicate or attachment
    /// count and total-byte bounds would be exceeded.
    pub fn add_attachment(&mut self, attachment: DraftAttachment) -> Result<(), String> {
        let mut next = self.attachments.clone();
        next.push(attachment);
        let bounded = crate::attachment_store::bounded(&next);
        if bounded.len() != next.len() {
            return Err("Attachment is invalid, duplicated, or exceeds Mail's draft limits".into());
        }
        self.attachments = bounded;
        self.dirty = true;
        Ok(())
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
            // The default window follows the text scale so its header rows,
            // toolbar and button bar still leave a usable body at 200%.
            let scale = self.metrics.scale;
            let (w, h) = (
                (DEFAULT_W * scale).min(bounds.width() - 32.0).max(MIN_W),
                (DEFAULT_H * scale).min(bounds.height() - 32.0).max(MIN_H),
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
        // Every band is its measured text plus token pads (the authored 1×
        // band before the first paint), so 200% text grows the header rows
        // instead of stacking labels, fields and chips over each other.
        let m = self.metrics;
        let field_h = m.field_h;
        // Header labels grow with the text; the field column starts past
        // the widest label at 1× (`LABEL_W`), scaled.
        let label_w = LABEL_W * m.scale;

        // Title bar (drag handle) + window buttons, squares centred on the
        // measured title bar.
        self.hits.push((
            Rect::new(r.x0, r.y0, r.x1, r.y0 + m.title_h),
            ComposeHit::TitleBar,
        ));
        let btn = (m.title_h - 10.0).max(28.0);
        let btn_y = r.y0 + (m.title_h - btn) / 2.0;
        let close = Rect::new(r.x1 - btn - 8.0, btn_y, r.x1 - 8.0, btn_y + btn);
        self.hits.push((close, ComposeHit::Close));
        let popout = Rect::new(close.x0 - btn - 4.0, btn_y, close.x0 - 4.0, btn_y + btn);
        self.hits.push((popout, ComposeHit::PopOut));

        let mut y = r.y0 + m.title_h + 10.0;

        // To row + Cc/Bcc toggle on the right.
        let cc_toggle_w = m.cc_bcc_w;
        let to_rect = Rect::new(lx + label_w, y, rx - cc_toggle_w - 8.0, y + field_h);
        self.to.apply_layout_external(to_rect);
        self.hits.push((to_rect, ComposeHit::FieldTo));
        self.hits.push((
            Rect::new(rx - cc_toggle_w, y, rx, y + field_h),
            ComposeHit::ToggleCcBcc,
        ));
        y += field_h + HEADER_ROW_GAP;
        if self.show_cc_bcc {
            let cc_rect = Rect::new(lx + label_w, y, rx, y + field_h);
            self.cc.apply_layout_external(cc_rect);
            self.hits.push((cc_rect, ComposeHit::FieldCc));
            y += field_h + HEADER_ROW_GAP;
            let bcc_rect = Rect::new(lx + label_w, y, rx, y + field_h);
            self.bcc.apply_layout_external(bcc_rect);
            self.hits.push((bcc_rect, ComposeHit::FieldBcc));
            y += field_h + HEADER_ROW_GAP;
        }
        // Subject + Attach button.
        let attach_w = m.attach_w;
        let subject_rect = Rect::new(lx + label_w, y, rx - attach_w - 8.0, y + field_h);
        self.subject.apply_layout_external(subject_rect);
        self.hits.push((subject_rect, ComposeHit::FieldSubject));
        self.hits.push((
            Rect::new(rx - attach_w, y, rx, y + field_h),
            ComposeHit::Attach,
        ));
        y + field_h + 10.0
    }

    fn relayout_regions(&mut self) {
        self.hits.clear();
        let r = self.rect;
        let lx = r.x0 + PAD;
        let rx = r.x1 - PAD;
        let m = self.metrics;
        let mut y = self.layout_header_hits(r, lx, rx);

        // Formatting toolbar (HTML mode) + format toggle (always). Each
        // glyph button is a square the toolbar row's height (less its gap to
        // the body), so the glyphs fit their squares at every text scale.
        let toolbar_y = y;
        let mut tx = lx;
        let tw = m.toolbar_h - 6.0;
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
                self.hits
                    .push((Rect::new(tx, toolbar_y, tx + tw, toolbar_y + tw), hit));
                // Glyph squares are `s1` apart (they used to sit 2 px apart,
                // touching at 1×); the inline-marks group ends with an extra
                // `s2`, so the group gap is the `s3` sibling step (HIG §7b).
                tx += tw + TOOLBAR_GLYPH_GAP;
                if matches!(hit, ComposeHit::Underline) {
                    tx += TOOLBAR_GROUP_EXTRA_GAP;
                }
            }
        }
        // Format toggle on the right of the toolbar row.
        let ftw = m.toggle_w;
        self.hits.push((
            Rect::new(rx - ftw, toolbar_y, rx, toolbar_y + tw),
            ComposeHit::FormatToggle,
        ));
        y += m.toolbar_h;

        // Below the body, bottom-up: the button bar (the measured button plus
        // `s2` above and below), then — when present — the inline prompt bar
        // (a field-high row plus `s2` above and below) and the attachment
        // chips band. Each band is stored so the painters read the same rects
        // the hit list was built from.
        let buttons_h = m.button_h + 16.0;
        let by = r.y1 - buttons_h + 8.0;
        let prompt_h = if self.prompt.is_some() {
            m.field_h + 16.0
        } else {
            0.0
        };
        self.prompt_band = Rect::new(lx, by - 8.0 - prompt_h, rx, by - 8.0);
        let chips_h = if self.attachments.is_empty() {
            0.0
        } else {
            30.0 * m.scale
        };
        self.chips_band = Rect::new(lx, self.prompt_band.y0 - chips_h, rx, self.prompt_band.y0);
        let body = Rect::new(lx, y, rx, self.chips_band.y0 - PAD);
        self.hits.push((body, ComposeHit::Body));
        match self.format {
            ComposeFormat::Html => self.editor.apply_layout(body),
            ComposeFormat::Text => self.plain_body.apply_layout_external(body),
        }

        // Buttons: Send and Save draft from the left, Discard at the right,
        // each sized from its measured label.
        let bh = m.button_h;
        self.hits
            .push((Rect::new(lx, by, lx + m.send_w, by + bh), ComposeHit::Send));
        let draft_x = lx + m.send_w + 8.0;
        self.hits.push((
            Rect::new(draft_x, by, draft_x + m.draft_w, by + bh),
            ComposeHit::SaveDraft,
        ));
        self.hits.push((
            Rect::new(rx - m.discard_w, by, rx, by + bh),
            ComposeHit::Discard,
        ));

        // Inline prompt bar (Attach path / Link URL): a field-high row in its
        // band, the field between the kind label's column and the two
        // measured buttons.
        if let Some(p) = self.prompt.as_mut() {
            let band = self.prompt_band;
            let py = band.y0 + 8.0;
            let field_h = m.field_h;
            let cancel = Rect::new(rx - m.prompt_cancel_w, py, rx, py + field_h);
            let confirm = Rect::new(
                cancel.x0 - 8.0 - m.prompt_add_w,
                py,
                cancel.x0 - 8.0,
                py + field_h,
            );
            let label_col = 92.0 * m.scale;
            let field = Rect::new(lx + label_col, py, confirm.x0 - 12.0, py + field_h);
            p.input.apply_layout_external(field);
            self.hits.push((field, ComposeHit::PromptField));
            self.hits.push((confirm, ComposeHit::PromptConfirm));
            self.hits.push((cancel, ComposeHit::PromptCancel));
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
    fn handle_cursor_moved(
        &mut self,
        position: winit::dpi::PhysicalPosition<f64>,
        event: &WindowEvent,
        ctx: &mut ComposeEventContext<'_>,
    ) -> (bool, ComposeAction) {
        let pt = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                ctx.scale_factor,
                &ctx.modifiers,
                ctx.state,
                ctx.path,
                &mut h,
                &mut ft,
                ctx.node_id,
                ctx.taffy,
            );
        }
        // Forward to the plain body too so its hover / drag-selection tracks
        // (Text mode).
        if matches!(self.format, ComposeFormat::Text) {
            self.plain_body.handle_event(
                event,
                ctx.scale_factor,
                &ctx.modifiers,
                ctx.state,
                ctx.path,
                &mut h,
                &mut ft,
                ctx.node_id,
                ctx.taffy,
            );
        }
        if let Some(p) = self.prompt.as_mut() {
            p.input.handle_event(
                event,
                ctx.scale_factor,
                &ctx.modifiers,
                ctx.state,
                ctx.path,
                &mut h,
                &mut ft,
                ctx.node_id,
                ctx.taffy,
            );
        }
        self.editor.on_cursor_moved(pt, false);
        // Consume only if inside the window (so hover outside still works
        // for the app, but we stay modal for clicks/keys).
        (self.rect.contains(pt), ComposeAction::None)
    }

    pub(crate) fn handle_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ComposeEventContext<'_>,
    ) -> (bool, ComposeAction) {
        if !self.open {
            return (false, ComposeAction::None);
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(*position, event, ctx)
            }
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
                    return (true, self.dispatch_hit(hit, event, pt, ctx));
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
            WindowEvent::KeyboardInput { .. } => self.handle_keyboard_input(event, ctx),
            _ => (self.rect.contains(self.last_mouse), ComposeAction::None),
        }
    }

    /// A `KeyboardInput`: the inline prompt (if open) captures keys first
    /// (Esc cancels it, Enter confirms), then Esc closes the window, then the
    /// focused body editor or header field gets the key. Always consumed
    /// while the window is open (modal). Part of [`Self::handle_event`].
    fn handle_keyboard_input(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ComposeEventContext<'_>,
    ) -> (bool, ComposeAction) {
        // The inline prompt captures keys while open: Esc cancels it (not the
        // window), Enter confirms, everything else edits the field.
        if self.prompt.is_some() {
            return self.handle_prompt_key(event, ctx);
        }
        // Escape closes.
        if Self::is_escape_press(event) {
            self.close();
            return (true, ComposeAction::Close);
        }
        // Body editor.
        if matches!(self.focus, Field::Body) {
            self.handle_body_key(event, ctx);
            return (true, ComposeAction::None);
        }
        // Header input.
        self.handle_header_key(event, ctx);
        // Always consume keyboard while open (modal).
        (true, ComposeAction::None)
    }

    /// Keys while the inline prompt bar is open: Esc cancels it, Enter
    /// confirms, everything else edits the prompt's field. Part of
    /// [`Self::handle_keyboard_input`].
    fn handle_prompt_key(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ComposeEventContext<'_>,
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
                ctx.scale_factor,
                &ctx.modifiers,
                ctx.state,
                ctx.path,
                &mut h,
                &mut ft,
                ctx.node_id,
                ctx.taffy,
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
    fn handle_body_key(&mut self, event: &WindowEvent, ctx: &mut ComposeEventContext<'_>) {
        let shift = ctx.modifiers.state().shift_key();
        let ctrl = ctx.modifiers.state().control_key();
        match self.format {
            ComposeFormat::Html => {
                self.editor.on_key(event, shift, ctrl);
            }
            ComposeFormat::Text => {
                let mut h = false;
                let mut ft: Option<taffy::NodeId> = None;
                self.plain_body.handle_event(
                    event,
                    ctx.scale_factor,
                    &ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    &mut h,
                    &mut ft,
                    ctx.node_id,
                    ctx.taffy,
                );
            }
        }
        self.dirty = true;
    }

    /// Forward a key to whichever header field (To/Cc/Bcc/Subject) has focus.
    /// Part of [`Self::handle_keyboard_input`].
    fn handle_header_key(&mut self, event: &WindowEvent, ctx: &mut ComposeEventContext<'_>) {
        let mut h = false;
        let mut ft: Option<taffy::NodeId> = None;
        if let Some(inp) = self.focused_input() {
            inp.handle_event(
                event,
                ctx.scale_factor,
                &ctx.modifiers,
                ctx.state,
                ctx.path,
                &mut h,
                &mut ft,
                ctx.node_id,
                ctx.taffy,
            );
        }
        self.dirty = true;
    }

    fn dispatch_hit(
        &mut self,
        hit: ComposeHit,
        event: &WindowEvent,
        pt: Point,
        ctx: &mut ComposeEventContext<'_>,
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
            ComposeHit::Attach => return ComposeAction::RequestAttachment,
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
                self.focus_and_forward(field, event, ctx);
            }
            ComposeHit::Body => self.dispatch_body_press(pt, event, ctx),
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
                        ctx.scale_factor,
                        &ctx.modifiers,
                        ctx.state,
                        ctx.path,
                        &mut h,
                        &mut ft,
                        ctx.node_id,
                        ctx.taffy,
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
    fn dispatch_body_press(
        &mut self,
        pt: Point,
        event: &WindowEvent,
        ctx: &mut ComposeEventContext<'_>,
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
                    ctx.scale_factor,
                    &ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    &mut h,
                    &mut ft,
                    ctx.node_id,
                    ctx.taffy,
                );
            }
        }
    }

    /// Open the inline prompt bar to collect a URL. File selection always
    /// goes through the root's host-native portal boundary.
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
        }
        self.relayout_regions();
        self.dirty = true;
    }

    fn focus_and_forward(
        &mut self,
        field: Field,
        event: &WindowEvent,
        ctx: &mut ComposeEventContext<'_>,
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
                ctx.scale_factor,
                &ctx.modifiers,
                ctx.state,
                ctx.path,
                &mut h,
                &mut ft,
                ctx.node_id,
                ctx.taffy,
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
        // Measure with this frame's text scale and lay the regions out again
        // from those sizes before painting, so what is painted (and
        // hit-tested until the next layout) is the measured geometry even on
        // a single headless frame.
        let measured = ComposeMetrics::measure(tr, theme);
        if self.metrics != measured {
            // A text-scale change rescales the window's *size* from where it
            // stands — its rows, toolbar and button bar need the room — and
            // never its placement: a window the user moved or detached stays
            // put (clamped into the content area).
            if self.metrics.scale != measured.scale && self.placed {
                let k = measured.scale / self.metrics.scale.max(f64::EPSILON);
                let r = self.rect;
                self.rect = Rect::new(
                    r.x0,
                    r.y0,
                    r.x0 + (r.width() * k).max(MIN_W),
                    r.y0 + (r.height() * k).max(MIN_H),
                );
            }
            self.metrics = measured;
            self.layout(self.bounds);
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
        self.draw_staged_attachment_chips(scene, t, tr, theme);

        // Inline prompt bar.
        self.draw_prompt(scene, t, tr, theme, presence);

        // Buttons.
        self.draw_action_button(
            scene,
            t,
            tr,
            theme,
            ComposeHit::Send,
            "mail-compose-send",
            true,
        );
        self.draw_action_button(
            scene,
            t,
            tr,
            theme,
            ComposeHit::SaveDraft,
            "mail-compose-save-draft",
            false,
        );
        self.draw_action_button(
            scene,
            t,
            tr,
            theme,
            ComposeHit::Discard,
            "mail-compose-discard",
            false,
        );

        // Resize affordance (bottom-right grip): lives inside the window's
        // padding ring, an `s1` clear of the Discard button's corner (the
        // two used to share an edge) and `s1` in from the window's own edge.
        let s1 = f64::from(theme.tokens.spacing.s1);
        let g = Rect::new(r.x1 - PAD + s1, r.y1 - PAD + s1, r.x1 - s1, r.y1 - s1);
        scene.stroke(
            &Stroke::new(1.2),
            t,
            ui::surface::edge_on_with(c.surfaces.raised, ui::chrome::GLOW_ALPHA),
            None,
            &RoundedRect::from_rect(g, 2.0),
        );

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
            "mail-compose-label-to",
            &self.to_rect_for(ComposeHit::FieldTo),
            matches!(self.focus, Field::To),
        );
        if self.show_cc_bcc {
            self.draw_field(
                scene,
                t,
                tr,
                theme,
                "mail-compose-label-cc",
                &self.to_rect_for(ComposeHit::FieldCc),
                matches!(self.focus, Field::Cc),
            );
            self.draw_field(
                scene,
                t,
                tr,
                theme,
                "mail-compose-label-bcc",
                &self.to_rect_for(ComposeHit::FieldBcc),
                matches!(self.focus, Field::Bcc),
            );
        }
        self.draw_field(
            scene,
            t,
            tr,
            theme,
            "mail-compose-label-subject",
            &self.to_rect_for(ComposeHit::FieldSubject),
            matches!(self.focus, Field::Subject),
        );

        // Cc/Bcc toggle + Attach buttons. The active highlight reflects on/off.
        self.draw_chip_button(
            scene,
            t,
            tr,
            theme,
            ComposeHit::ToggleCcBcc,
            "mail-compose-cc-bcc",
            self.show_cc_bcc,
        );
        self.draw_chip_button(
            scene,
            t,
            tr,
            theme,
            ComposeHit::Attach,
            "mail-compose-attach",
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
        // The body card's rest edge is derived from its base fill (HIG §9.5);
        // focus keeps the focus ring.
        if matches!(self.focus, Field::Body) {
            pill(
                scene,
                t,
                body,
                c.surfaces.base,
                Some(c.border_focus),
                Some(8.0),
            );
        } else {
            card(scene, t, body, 8.0, c.surfaces.base);
        }
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
        // Drop shadow: the window's own shape on the Object tier of the
        // elevation ladder (HIG §9.1 / §9.3), not a solid rect nudged down,
        // which read as a grey bar under the window on the light preset.
        ui::surface::shadow(scene, t, r, 14.0, &theme.tokens.elevation.e3);
        // Window body: a floating Panel, so the derived panel border, crisp
        // on the outermost pixel row (HIG §9.5).
        pill(scene, t, r, c.surfaces.raised, None, Some(14.0));
        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::chrome::panel_border(theme),
            None,
            &edge_outline(r, 14.0),
        );

        // Title bar: the measured bar height (`ComposeMetrics::title_h`), the
        // heading centred on it from its own line box.
        let title = Rect::new(r.x0, r.y0, r.x1, r.y0 + self.metrics.title_h);
        pill(
            scene,
            t,
            Rect::new(title.x0, title.y0, title.x1, title.y0 + 24.0),
            c.surfaces.base,
            None,
            Some(14.0),
        );
        fill(
            scene,
            t,
            Rect::new(title.x0, title.y0 + 12.0, title.x1, title.y1),
            c.surfaces.base,
        );
        // The title bar's rule is derived against the bar it closes (HIG
        // §9.5); the declared `border_standard` vanished on dark.
        fill(
            scene,
            t,
            Rect::new(title.x0, title.y1 - 1.0, title.x1, title.y1),
            edge_on(c.surfaces.base),
        );
        let heading = if self.title_hint.is_empty() {
            "New message".to_string()
        } else {
            self.title_hint.clone()
        };
        let heading = tr.elide_to_width(&heading, TITLE_SIZE, (r.width() - 100.0) as f32);
        let (_, hh) = tr.measure_weighted(&heading, TITLE_SIZE, None, TextWeight::Semibold);
        tr.draw_text_weighted(
            scene,
            &heading,
            TITLE_SIZE,
            c.text_primary,
            t * Affine::translate((r.x0 + PAD, title.center().y - f64::from(hh) / 2.0)),
            None,
            TextWeight::Semibold,
        );
        // Window buttons, glyphs centred in their squares.
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
                pill(scene, t, *rect, theme.bg_hover, None, Some(7.0));
            }
            let (gw, gh) = tr.measure(glyph, TOOLBAR_GLYPH_SIZE, None);
            tr.draw_text(
                scene,
                glyph,
                TOOLBAR_GLYPH_SIZE,
                c.neutral[8],
                t * Affine::translate((
                    rect.center().x - f64::from(gw) / 2.0,
                    rect.center().y - f64::from(gh) / 2.0,
                )),
                None,
            );
        }
    }

    /// Draw the staged-attachment chips (portable content, shown by filename) above
    /// the button row. Part of [`Self::draw`].
    fn draw_staged_attachment_chips(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.attachments.is_empty() {
            return;
        }
        let c = &theme.tokens.colors;
        // The chips sit in the band `relayout_regions` reserved for them
        // (the same rect the body was shortened by), each chip centred on the
        // band and its glyph + name centred on the chip from measured boxes.
        let band = self.chips_band;
        let chip_h = (band.height() - 6.0 * self.metrics.scale).max(0.0);
        let cy = band.center().y;
        let mut ax = band.x0;
        let (gw, gh) = tr.measure("\u{1F4CE}", 12.0, None);
        for path in &self.attachments {
            let name = &path.filename;
            let (lw, lh) = tr.measure(name, CHIP_SIZE, None);
            let chip = Rect::new(
                ax,
                cy - chip_h / 2.0,
                ax + 7.0 + f64::from(gw) + 7.0 + f64::from(lw) + 8.0,
                cy + chip_h / 2.0,
            );
            card(scene, t, chip, 7.0, c.lift(0.72));
            tr.draw_text(
                scene,
                "\u{1F4CE}",
                12.0,
                c.text_dim,
                t * Affine::translate((chip.x0 + 7.0, cy - f64::from(gh) / 2.0)),
                None,
            );
            tr.draw_text(
                scene,
                name,
                CHIP_SIZE,
                c.text_primary,
                t * Affine::translate((
                    chip.x0 + 7.0 + f64::from(gw) + 7.0,
                    cy - f64::from(lh) / 2.0,
                )),
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

    #[allow(
        clippy::too_many_arguments,
        reason = "compose-field painting shares render context with the localized label key, field bounds and focus state"
    )]
    fn draw_field(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        label_key: &str,
        fr: &Rect,
        focused: bool,
    ) {
        let c = &theme.tokens.colors;
        // The label centres on the field's measured line box (the field's
        // own placeholder, where it has one, is painted by the shared input).
        let label = crate::i18n::t(label_key);
        let (_, lh) = tr.measure(&label, LABEL_SIZE, None);
        tr.draw_text(
            scene,
            &label,
            LABEL_SIZE,
            c.text_dim,
            t * Affine::translate((self.rect.x0 + PAD, fr.center().y - f64::from(lh) / 2.0)),
            None,
        );
        // The field's underline is derived against the window it is drawn on
        // (HIG §9.5); focus keeps the focus colour.
        fill(
            scene,
            t,
            Rect::new(fr.x0, fr.y1 - 1.0, fr.x1, fr.y1),
            if focused {
                c.border_focus
            } else {
                edge_on(c.surfaces.raised)
            },
        );
    }

    fn draw_chip_button(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hit: ComposeHit,
        label_key: &str,
        active: bool,
    ) {
        let c = &theme.tokens.colors;
        let Some(rect) = self.hits.iter().find(|(_, h)| *h == hit).map(|(r, _)| *r) else {
            return;
        };
        let hov = rect.contains(self.last_mouse);
        // At rest the chip is a lifted surface with its derived edge (HIG
        // §9.5); the "on" state is an accent wash, which is its own contrast
        // step and takes no edge.
        if active {
            pill(
                scene,
                t,
                rect,
                c.accent[5].with_alpha_factor(0.22),
                None,
                Some(7.0),
            );
        } else {
            card(
                scene,
                t,
                rect,
                7.0,
                if hov { theme.bg_hover } else { c.lift(0.35) },
            );
        }
        // Label centred on the chip from its measured box, both axes.
        let label = crate::i18n::t(label_key);
        let (lw, lh) = tr.measure(&label, CHIP_SIZE, None);
        tr.draw_text(
            scene,
            &label,
            CHIP_SIZE,
            c.neutral[8],
            t * Affine::translate((
                rect.center().x - f64::from(lw) / 2.0,
                rect.center().y - f64::from(lh) / 2.0,
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
            // Rest: lifted square with its derived edge (HIG §9.5); active:
            // an accent wash, its own contrast step, no edge.
            if active {
                pill(
                    scene,
                    t,
                    *rect,
                    c.accent[5].with_alpha_factor(0.28),
                    None,
                    Some(6.0),
                );
            } else {
                card(
                    scene,
                    t,
                    *rect,
                    6.0,
                    if hov { theme.bg_hover } else { c.lift(0.35) },
                );
            }
            let col = if active { c.text_primary } else { c.neutral[8] };
            // Glyph centred on its square from the measured box; the U's
            // underline sits at the glyph's baseline (the box's lower fifth).
            let (gw, gh) = tr.measure_weighted(glyph, TOOLBAR_GLYPH_SIZE, None, weight);
            let gx = rect.center().x - f64::from(gw) / 2.0;
            let gy = rect.center().y - f64::from(gh) / 2.0;
            tr.draw_text_styled(
                scene,
                glyph,
                TOOLBAR_GLYPH_SIZE,
                col,
                t * Affine::translate((gx, gy)),
                None,
                weight,
                italic,
                false,
            );
            if matches!(hit, ComposeHit::Underline) {
                let uy = gy + f64::from(gh) * 0.8;
                fill(
                    scene,
                    t,
                    Rect::new(gx, uy, gx + f64::from(gw), uy + 1.0),
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
        card(
            scene,
            t,
            rect,
            7.0,
            if hov { theme.bg_hover } else { c.lift(0.35) },
        );
        let label = match self.format {
            ComposeFormat::Html => "\u{25CF} HTML",
            ComposeFormat::Text => "\u{25CB} Text",
        };
        // Centred on the toggle from the measured box, both axes.
        let (lw, lh) = tr.measure(label, CHIP_SIZE, None);
        tr.draw_text(
            scene,
            label,
            CHIP_SIZE,
            c.neutral[8],
            t * Affine::translate((
                rect.center().x - f64::from(lw) / 2.0,
                rect.center().y - f64::from(lh) / 2.0,
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
        // The bar is the band `relayout_regions` reserved (widened by the
        // 6 px the window's padding allows), so it never paints over the body.
        let band = self.prompt_band;
        let panel = Rect::new(band.x0 - 6.0, band.y0, band.x1 + 6.0, band.y1);
        // An inline Panel: the derived panel border, or the danger colour
        // while the prompt is rejecting its input.
        pill(
            scene,
            t,
            panel,
            c.surfaces.raised,
            Some(if error.is_some() {
                c.semantic.danger
            } else {
                ui::chrome::panel_border(theme)
            }),
            Some(9.0),
        );
        let label = crate::i18n::t(match kind {
            PromptKind::Link => "mail-compose-prompt-link",
        });
        let (_, lh) = tr.measure(&label, LABEL_SIZE, None);
        tr.draw_text(
            scene,
            &label,
            LABEL_SIZE,
            c.text_dim,
            t * Affine::translate((panel.x0 + 10.0, field.center().y - f64::from(lh) / 2.0)),
            None,
        );
        // Input chrome: the prompt's field is the focused one, so it wears
        // the focus ring.
        pill(
            scene,
            t,
            Rect::new(
                field.x0 - 2.0,
                field.y0 - 1.0,
                field.x1 + 2.0,
                field.y1 + 1.0,
            ),
            c.surfaces.base,
            Some(c.border_focus),
            Some(6.0),
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
        // The shared button faces (primary / neutral) over the rects the hit
        // list owns; the face resolves the catalogue keys itself.
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(confirm, "mail-compose-prompt-add")
                .primary()
                .radius(7.0)
                .text_size(12.5)
                .weight(TextWeight::Semibold)
                .hovered(confirm.contains(self.last_mouse)),
        );
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(cancel, "mail-compose-prompt-cancel")
                .radius(7.0)
                .text_size(12.0)
                .weight(TextWeight::Regular)
                .hovered(cancel.contains(self.last_mouse)),
        );
    }

    fn draw_action_button(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hit: ComposeHit,
        label_key: &str,
        primary: bool,
    ) {
        let Some(rect) = self.hits.iter().find(|(_, h)| *h == hit).map(|(r, _)| *r) else {
            return;
        };
        // The shared button face (primary / neutral) over the rect the hit
        // list owns: one look, one measured label centring, at every scale.
        // The face resolves the catalogue key itself.
        let face = ButtonFace::new(rect, label_key)
            .radius(9.0)
            .text_size(BUTTON_SIZE)
            .weight(if primary {
                TextWeight::Semibold
            } else {
                TextWeight::Regular
            })
            .hovered(rect.contains(self.last_mouse));
        let face = if primary { face.primary() } else { face };
        Button::paint(scene, t, tr, theme, &face);
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
    fn one_x_metrics_reproduce_the_authored_bands() {
        // The pre-paint rule set keeps every 1× band at its authored size.
        let m = ComposeMetrics::default();
        assert_eq!(m.title_h, TITLE_H);
        assert_eq!(m.field_h, FIELD_H);
        assert_eq!(m.toolbar_h, TOOLBAR_H);
        assert_eq!(m.button_h, BUTTON_H);
        assert_eq!(m.scale, 1.0);
    }

    #[test]
    fn scaled_metrics_grow_the_header_rows_and_the_buttons_hold_their_labels() {
        // 200% text: rows follow their measured lines instead of the fields
        // and labels stacking over each other (the defect at 200% before).
        let big = ComposeMetrics {
            scale: 2.0,
            title_h: 51.0,
            label_h: 30.0,
            field_h: 51.0,
            toolbar_h: 62.0,
            button_h: 50.0,
            cc_bcc_w: 96.0,
            attach_w: 130.0,
            toggle_w: 140.0,
            send_w: 128.0,
            draft_w: 176.0,
            discard_w: 150.0,
            ..ComposeMetrics::default()
        };
        let mut w = ComposeWindow::new(ComposeFormat::Html);
        w.open_blank();
        w.metrics = big;
        w.layout(Rect::new(0.0, 0.0, 1280.0, 900.0));
        let to = hit_rect(&w, ComposeHit::FieldTo);
        let subject = hit_rect(&w, ComposeHit::FieldSubject);
        let send = hit_rect(&w, ComposeHit::Send);
        let draft = hit_rect(&w, ComposeHit::SaveDraft);
        let discard = hit_rect(&w, ComposeHit::Discard);
        assert_eq!(to.height(), big.field_h);
        assert!(
            subject.y0 >= to.y1,
            "the subject row starts under the To row"
        );
        assert_eq!(send.width(), big.send_w);
        assert!(draft.x0 >= send.x1, "Save draft sits after Send");
        assert!(discard.x0 >= draft.x1, "Discard sits after Save draft");
        assert_eq!(send.height(), big.button_h);
        // The default window followed the scale so a body remains.
        let body = hit_rect(&w, ComposeHit::Body);
        assert!(body.height() > 100.0, "body {body:?}");
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
            w.handle_event(
                ev,
                &mut ComposeEventContext {
                    scale_factor: 1.0,
                    modifiers: m,
                    state: &mut doc,
                    path: &automerge::ROOT,
                    node_id: node,
                    taffy: &taffy,
                },
            );
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
        w.handle_event(
            &mv,
            &mut ComposeEventContext {
                scale_factor: 1.0,
                modifiers: m,
                state: &mut doc,
                path: &automerge::ROOT,
                node_id: node,
                taffy: &taffy,
            },
        );
        let pr = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        w.handle_event(
            &pr,
            &mut ComposeEventContext {
                scale_factor: 1.0,
                modifiers: m,
                state: &mut doc,
                path: &automerge::ROOT,
                node_id: node,
                taffy: &taffy,
            },
        )
    }

    fn hit_rect(w: &ComposeWindow, hit: ComposeHit) -> Rect {
        w.hits
            .iter()
            .find(|(_, h)| *h == hit)
            .map(|(r, _)| *r)
            .expect("hit present")
    }

    #[test]
    fn attach_button_requests_the_root_portal_boundary() {
        let mut w = win();
        // The compose widget never accepts a typed path; its root owns the
        // host portal and returns a descriptor-only staged attachment later.
        let attach = hit_rect(&w, ComposeHit::Attach);
        let (consumed, action) = press_at(&mut w, attach.center());
        assert!(consumed);
        assert_eq!(action, ComposeAction::RequestAttachment);
        assert!(
            w.attachments.is_empty(),
            "no host path or fabricated attachment is staged"
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
            .set_text("https://example.test/docs");
        w.confirm_prompt();
        let html = w.editor.to_html();
        assert!(
            html.contains("href=\"https://example.test/docs\""),
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
            &mut ComposeEventContext {
                scale_factor: 1.0,
                modifiers,
                state: &mut doc,
                path: &automerge::ROOT,
                node_id: taffy::NodeId::new(0),
                taffy: &taffy,
            },
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
