//! The composer: where a message is written.
//!
//! ## Where it stands
//! Beside the list, the composer takes the message column, where the open
//! message is read ([`DockState::Pane`]): the list stays in reach, a click
//! there gives it the keys back, and nothing floats over the mailbox. In a
//! window too narrow for both it fills the window, as a page. Only where the
//! app gives it no column (a bare host, a test) does it float as a modal
//! panel over the app ([`DockState::Docked`]).
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
//! ## Floating
//! Only the floating panel moves (its title bar) and resizes (its edges),
//! clamped to a minimum size and kept on-screen.
//!
//! ## Look
//! In the column it is the page's own ground; floating, a panel (the
//! toolkit's material, radius and shadow) over the modal scrim. Its fields
//! are the shared text inputs, its buttons and the formatting toolbar are the
//! toolkit's, and every size comes from the theme it is drawn with.

use std::cell::Cell;
use std::rc::Rc;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;

use winit::event::{ElementState, MouseButton, WindowEvent};

use taffy::{NodeId, TaffyTree};
use tilo_rich_text::{RichTextField, RichTextSurface};
use ui::TextRole;
use ui::Widget;
use ui::framework::Dispatcher;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::marks::Mark;
use ui::widget::toolbar::Orientation;
use ui::widget::{
    BlockContent, BlockEditor, BlockKind, Button, ButtonFace, ButtonRole, ChipFace, ChipLead,
    ControlSize, SegmentedFace, TextInput, TextInputKind, ToggleGroup, Toolbar,
};

use crate::actions::DraftBodySeed;
use crate::model::{ComposeFormat, Draft, DraftAttachment};
use crate::resource::{Mail, draft_body_dispatcher, draft_body_object, mailbox};

/// The HTML body: the SDK rich-text editor over the draft's body, every
/// edit a field edit of the body through `dispatcher`; shown view-only
/// until there is one.
type BodyEditor = RichTextSurface<RichTextField>;

fn body_editor(dispatcher: Option<&Dispatcher<Mail>>) -> BodyEditor {
    let field = match dispatcher {
        Some(dispatcher) => {
            RichTextField::new(draft_body_dispatcher(dispatcher), draft_body_object)
        }
        None => RichTextField::view_only(draft_body_object),
    };
    RichTextSurface::field(field).with_editor(
        BlockEditor::new()
            .with_wrap_lines(true)
            .with_empty_hint("Write your message"),
    )
}

/// The editor's own layout inside the body card: the window is drawn
/// immediately, the editor is a laid-out widget.
#[derive(Default)]
struct BodyLayout {
    taffy: TaffyTree<()>,
    node: Option<NodeId>,
    /// The card it was laid out in.
    laid_in: Option<Rect>,
    /// How far the text is scrolled up inside the card.
    scroll: f64,
    /// The text's height, for the scroll limit.
    content_h: f64,
}

/// The body as the `text/plain` alternative: a line per paragraph, list
/// items and quotes marked as a reader of plain text expects.
fn plain_of(blocks: &[BlockContent]) -> String {
    let mut out = String::new();
    let mut number = 1_usize;
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        match block.kind {
            BlockKind::Bullet { .. } | BlockKind::Checklist { .. } => out.push_str("• "),
            BlockKind::Numbered { .. } => {
                out.push_str(&number.to_string());
                out.push_str(". ");
                number = number.saturating_add(1);
            }
            BlockKind::Quote => out.push_str("> "),
            _ => {}
        }
        if !matches!(block.kind, BlockKind::Numbered { .. }) {
            number = 1;
        }
        out.push_str(&block.text);
    }
    out
}

/// The formatting commands of the toolbar, in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Bold,
    Italic,
    Underline,
    Heading1,
    Heading2,
    Bullet,
    Numbered,
    Quote,
    Link,
}

impl Format {
    /// The toolbar's groups: inline marks, headings, blocks, the link.
    const GROUPS: [&'static [Format]; 4] = [
        &[Format::Bold, Format::Italic, Format::Underline],
        &[Format::Heading1, Format::Heading2],
        &[Format::Bullet, Format::Numbered, Format::Quote],
        &[Format::Link],
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Bold => "Bold",
            Self::Italic => "Italic",
            Self::Underline => "Underline",
            Self::Heading1 => "Heading 1",
            Self::Heading2 => "Heading 2",
            Self::Bullet => "Bulleted list",
            Self::Numbered => "Numbered list",
            Self::Quote => "Quote",
            Self::Link => "Link",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::Bold => Icon::Bold,
            Self::Italic => Icon::Italic,
            Self::Underline => Icon::Underline,
            Self::Heading1 => Icon::Heading1,
            Self::Heading2 => Icon::Heading2,
            Self::Bullet => Icon::List,
            Self::Numbered => Icon::ListOrdered,
            Self::Quote => Icon::Quote,
            Self::Link => Icon::Link,
        }
    }

    /// Whether the command is a toggle (everything but inserting a link).
    fn toggles(self) -> bool {
        self != Self::Link
    }
}

/// The formatting toolbar and the command its last click chose.
struct FormatBar {
    bar: Toolbar,
    chosen: Rc<Cell<Option<Format>>>,
}

impl FormatBar {
    fn new() -> Self {
        let chosen = Rc::new(Cell::new(None));
        let mut bar = Toolbar::new(Orientation::Horizontal);
        for (index, group) in Format::GROUPS.iter().enumerate() {
            if index > 0 {
                bar = bar.push_separator();
            }
            for format in group.iter().copied() {
                let chosen = Rc::clone(&chosen);
                let on_click = move || chosen.set(Some(format));
                bar = if format.toggles() {
                    bar.push_icon_toggle(format.icon(), format.label(), false, on_click)
                } else {
                    bar.push_icon(format.icon(), format.label(), on_click)
                };
            }
        }
        Self { bar, chosen }
    }

    /// Where the bar shows each command it has room for, by its label.
    fn command_bounds(&self) -> Vec<(String, Rect)> {
        ui::Widget::accessibility(&self.bar)
            .map(|bar| {
                bar.children
                    .into_iter()
                    .filter_map(|item| Some((item.name?, item.bounds?.to_rect())))
                    .filter(|(_, bounds)| bounds.area() > 0.0)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Where the command `label` is: its own button, not anywhere on the
    /// bar, or the "More commands" button when the bar has no room for it.
    fn command_at(commands: &[(String, Rect)], label: &str) -> Option<Rect> {
        commands
            .iter()
            .find(|(name, _)| name == label)
            .or_else(|| commands.iter().find(|(name, _)| name == "More commands"))
            .map(|(_, bounds)| *bounds)
    }

    /// Show which commands are on, from the editor.
    fn set_active(&mut self, active: impl Fn(Format) -> bool) {
        let mut index = 0;
        for (group_index, group) in Format::GROUPS.iter().enumerate() {
            if group_index > 0 {
                index += 1;
            }
            for format in group.iter().copied() {
                if format.toggles() {
                    self.bar.set_active(index, active(format));
                }
                index += 1;
            }
        }
    }
}

/// Sizes the compose window is laid out with, measured with the theme it is
/// drawn with, so a larger text scale grows its rows instead of stacking
/// them.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ComposeMetrics {
    /// Inset of the content from the window's edge.
    pad: f64,
    /// Gap between related rows and controls.
    gap: f64,
    /// The title bar.
    title_h: f64,
    /// The column the field labels stand in.
    label_w: f64,
    /// A field, the toolbar row and a footer button.
    control_h: f64,
    /// An icon-only button's side.
    icon_button: f64,
    cc_bcc_w: f64,
    format_w: f64,
    send_w: f64,
    discard_w: f64,
    prompt_add_w: f64,
    prompt_cancel_w: f64,
    chip_h: f64,
    min_w: f64,
    min_h: f64,
    default_w: f64,
    default_h: f64,
}

/// The format switch's segments.
const FORMAT_LABELS: [&str; 2] = ["Rich text", "Plain text"];

/// The title of a window writing a fresh message.
const NEW_MESSAGE: &str = "New message";

impl ComposeMetrics {
    fn measure(tr: &mut TextRenderer, theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        let label = theme.text(TextRole::Label);
        let label_w = [
            "mail-compose-label-to",
            "mail-compose-label-cc",
            "mail-compose-label-bcc",
            "mail-compose-label-subject",
        ]
        .iter()
        .map(|key| f64::from(tr.measure_style(&crate::i18n::t(key), None, &label).0))
        .fold(0.0, f64::max)
            + f64::from(spacing.s3);
        let button = |tr: &mut TextRenderer, key: &str, size: ControlSize| {
            ButtonFace::new(Rect::ZERO, key)
                .control_size(size)
                .intrinsic_width(tr, theme)
        };
        let control_h = ControlSize::Regular.height(theme);
        let pad = f64::from(spacing.s4);
        let title_h = f64::from(metrics.bar_height).max(control_h + 2.0 * f64::from(spacing.s1));
        let default_w = f64::from(metrics.panel_width_sheet) * f64::from(theme.text_growth());
        Self {
            pad,
            gap: f64::from(spacing.s2),
            title_h,
            label_w,
            control_h,
            icon_button: f64::from(metrics.min_pointer_target),
            cc_bcc_w: button(tr, "mail-compose-cc-bcc", ControlSize::Regular),
            format_w: SegmentedFace::intrinsic_width(
                &FORMAT_LABELS,
                ControlSize::Regular,
                tr,
                theme,
            ),
            send_w: button(tr, "mail-compose-send", ControlSize::Regular),
            discard_w: button(tr, "mail-compose-discard", ControlSize::Regular),
            prompt_add_w: button(tr, "mail-compose-prompt-add", ControlSize::Small),
            prompt_cancel_w: button(tr, "mail-compose-prompt-cancel", ControlSize::Small),
            chip_h: ChipFace::height(theme),
            // The narrowest window that still holds the footer's buttons
            // and a few lines of body.
            min_w: f64::from(metrics.panel_width_utility),
            min_h: title_h + 6.0 * control_h + 2.0 * pad,
            default_w,
            default_h: default_w,
        }
    }
}

impl Default for ComposeMetrics {
    fn default() -> Self {
        Self::measure(&mut TextRenderer::new(), &Theme::dark())
    }
}

pub(crate) struct ComposeEventContext<'a> {
    pub(crate) scale_factor: f64,
    pub(crate) modifiers: winit::event::Modifiers,
    pub(crate) state: ui::widget::Edits<'a>,
    pub(crate) path: &'a automerge::ObjId,
    pub(crate) node_id: taffy::NodeId,
    pub(crate) taffy: &'a taffy::TaffyTree<()>,
}

impl ComposeEventContext<'_> {
    /// The event context a compose child sees, reporting into `handled` and
    /// `focus_target`.
    fn event_ctx<'b>(
        &'b mut self,
        handled: &'b mut bool,
        focus_target: &'b mut Option<taffy::NodeId>,
    ) -> ui::widget::EventCtx<'b> {
        ui::widget::EventCtx {
            scale_factor: self.scale_factor,
            modifiers: &self.modifiers,
            state: self.state.reborrow(),
            path: self.path,
            handled,
            focus_target,
            node_id: self.node_id,
            taffy: self.taffy,
        }
    }
}

/// Where the compose window is parked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockState {
    /// Centred over the app content as an in-app modal: where the window
    /// has no message column to write in.
    Docked,
    /// In the message column, where the open message is read: the list
    /// beside it stays in reach, as in every desktop mail program.
    Pane,
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
    Close,
    FormatToggle,
    FieldTo,
    FieldCc,
    FieldBcc,
    FieldSubject,
    Body,
    /// The formatting toolbar.
    Toolbar,
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
    /// The window closed; what was written stays a draft.
    Close,
    /// The person threw the message away.
    Discard,
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
    /// The message this one answers, so the reply names it on the wire.
    replying_to: Option<crate::model::MessageId>,
    /// Whether the person typed, attached or formatted anything since the
    /// window opened: closing such a message keeps it as a draft.
    touched: bool,
    /// Who the person writes to, for completing recipients; set by the app
    /// when the composer opens.
    book: Option<crate::address_book::AddressBook>,
    /// The completions offered under the recipient field being typed in.
    completions: ui::widget::SuggestionList,
    dock: DockState,
    /// Window rect in screen coordinates. The source of truth for placement;
    /// movable + resizable mutate it directly.
    rect: Rect,
    /// Bounds the window must stay within (the app content area).
    bounds: Rect,
    /// The message column the composer takes, where there is one.
    pane: Option<Rect>,
    /// Whether keys go to the composer: it was opened or clicked into, and
    /// the person has not clicked the list beside it since.
    has_keys: bool,
    /// `true` once the window has been positioned for the current open, so a
    /// re-layout doesn't recentre it after the user has moved it.
    placed: bool,

    sender: Option<crate::model::DraftSender>,
    // header inputs
    to: TextInput,
    cc: TextInput,
    bcc: TextInput,
    subject: TextInput,
    show_cc_bcc: bool,

    // body
    format: ComposeFormat,
    format_bar: FormatBar,
    editor: BodyEditor,
    body_layout: BodyLayout,
    /// The body to start the draft with, applied when the window next
    /// reaches the document.
    seed: Option<DraftBodySeed>,
    /// Submits [`Mail::START_DRAFT_BODY`].
    dispatcher: Option<Dispatcher<Mail>>,
    /// The body as plain text as of the last frame, for accessibility and a
    /// switch to plain-text authoring.
    body_plain: String,
    /// Plain-text body (Text mode), backed by a shared multiline [`TextInput`].
    plain_body: TextInput,

    /// Portable content-addressed attachment references, never host paths.
    attachments: Vec<DraftAttachment>,
    /// The inline URL prompt, when open.
    prompt: Option<ComposePrompt>,
    focus: Field,
    drag: Option<Drag>,
    last_mouse: Point,
    hits: Vec<(Rect, ComposeHit)>,
    dirty: bool,
    /// What the window is writing ("New message", "Reply", …), its title.
    /// The subject stands in its own field just below, so it is not
    /// repeated here.
    heading: &'static str,
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
    /// The theme the window was last drawn with, for the toolbar's layout
    /// and events between frames.
    theme: Theme,
}

impl ComposeWindow {
    pub fn new(format: ComposeFormat) -> Self {
        // Recipients are addresses and names: no spell check, identifier
        // text.
        let recipients = || TextInput::local().with_kind(TextInputKind::Email);
        let to = recipients().with_placeholder("mail-compose-placeholder-to");
        Self {
            open: false,
            replying_to: None,
            touched: false,
            book: None,
            completions: ui::widget::SuggestionList::new(),
            dock: DockState::Pane,
            pane: None,
            has_keys: false,
            rect: Rect::ZERO,
            bounds: Rect::ZERO,
            placed: false,
            to,
            cc: recipients(),
            bcc: recipients(),
            subject: TextInput::local(),
            show_cc_bcc: false,
            sender: None,
            format,
            format_bar: FormatBar::new(),
            editor: body_editor(None),
            body_layout: BodyLayout::default(),
            seed: None,
            dispatcher: None,
            body_plain: String::new(),
            plain_body: {
                // The body stands on the window like the rich editor does.
                let mut b = TextInput::local()
                    .multiline(true)
                    .with_placeholder("Write your message");
                b.set_plain_chrome(true);
                b
            },
            attachments: Vec::new(),
            prompt: None,
            focus: Field::None,
            drag: None,
            last_mouse: Point::ZERO,
            hits: Vec::new(),
            dirty: true,
            heading: NEW_MESSAGE,
            metrics: ComposeMetrics::default(),
            chips_band: Rect::ZERO,
            prompt_band: Rect::ZERO,
            theme: Theme::dark(),
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
                    self.body_plain.clone()
                } else {
                    self.plain_body.value().to_string()
                },
                rect_for(ComposeHit::Body),
                true,
            ),
        ];
        if let Some(prompt) = &self.prompt {
            children.push(field(
                "Link address",
                "prompt",
                prompt.input.value().to_string(),
                rect_for(ComposeHit::PromptField),
                false,
            ));
        }
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
        children.extend(self.accessibility_controls(rect_for));
        // Beside the list it is a part of the window, not a dialog over it.
        let (role, modal) = if self.in_pane() {
            (Role::Group, false)
        } else {
            (Role::Dialog, true)
        };
        AccessNode::new(role)
            .with_name("Compose email")
            .with_description("Email composer")
            .with_bounds(self.rect)
            .with_state(AccessState::new().modal(modal))
            .with_children(children)
    }

    /// The composer's buttons, each where a person presses it.
    fn accessibility_controls(
        &self,
        rect_for: impl Fn(ComposeHit) -> Option<Rect>,
    ) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let commands = self.format_bar.command_bounds();
        let mut controls = Vec::new();
        for (hit, name, target, checked) in self.accessibility_control_specs() {
            let command = (hit == ComposeHit::Toolbar)
                .then(|| FormatBar::command_at(&commands, name))
                .flatten();
            let Some(rect) = command.or_else(|| rect_for(hit)) else {
                continue;
            };
            let role = if checked.is_some() {
                Role::ToggleButton
            } else {
                Role::Button
            };
            controls.push(
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
        controls
    }

    fn accessibility_control_specs(
        &self,
    ) -> Vec<(ComposeHit, &'static str, &'static str, Option<bool>)> {
        let marks = self.editor.editor().active_marks();
        let block = self.focused_kind();
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
                "Rich text",
                "format",
                Some(self.format == ComposeFormat::Html),
            ),
            (
                ComposeHit::Toolbar,
                "Bold",
                "bold",
                Some(marks.contains(&Mark::Bold)),
            ),
            (
                ComposeHit::Toolbar,
                "Italic",
                "italic",
                Some(marks.contains(&Mark::Italic)),
            ),
            (
                ComposeHit::Toolbar,
                "Underline",
                "underline",
                Some(marks.contains(&Mark::Underline)),
            ),
            (
                ComposeHit::Toolbar,
                "Heading 1",
                "heading-1",
                Some(matches!(block, BlockKind::Heading(1))),
            ),
            (
                ComposeHit::Toolbar,
                "Heading 2",
                "heading-2",
                Some(matches!(block, BlockKind::Heading(2))),
            ),
            (
                ComposeHit::Toolbar,
                "Bulleted list",
                "bullet",
                Some(matches!(block, BlockKind::Bullet { .. })),
            ),
            (
                ComposeHit::Toolbar,
                "Numbered list",
                "numbered",
                Some(matches!(block, BlockKind::Numbered { .. })),
            ),
            (
                ComposeHit::Toolbar,
                "Quote",
                "quote",
                Some(matches!(block, BlockKind::Quote)),
            ),
            (ComposeHit::Toolbar, "Link", "link", None),
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
                        self.start_body(DraftBodySeed::text(value));
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
                return Some(ComposeAction::Discard);
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
                self.toggle_mark(Mark::Bold);
            }
            "italic" if matches!(action, Action::Activate | Action::ToggleCheck) => {
                self.toggle_mark(Mark::Italic);
            }
            "underline" if matches!(action, Action::Activate | Action::ToggleCheck) => {
                self.toggle_mark(Mark::Underline);
            }
            "heading-1" if matches!(action, Action::Activate | Action::ToggleCheck) => {
                self.toggle_kind(BlockKind::Heading(1));
            }
            "heading-2" if matches!(action, Action::Activate | Action::ToggleCheck) => {
                self.toggle_kind(BlockKind::Heading(2));
            }
            "bullet" if matches!(action, Action::Activate | Action::ToggleCheck) => {
                self.toggle_kind(BlockKind::Bullet { indent: 0 });
            }
            "numbered" if matches!(action, Action::Activate | Action::ToggleCheck) => {
                self.toggle_kind(BlockKind::Numbered { indent: 0 });
            }
            "quote" if matches!(action, Action::Activate | Action::ToggleCheck) => {
                self.toggle_kind(BlockKind::Quote);
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
        self.has_keys = true;
        self.book = None;
        self.completions.clear();
        self.replying_to = None;
        self.touched = false;
        self.sender = None;
        self.placed = false;
        self.to.set_text("");
        self.cc.set_text("");
        self.bcc.set_text("");
        self.subject.set_text("");
        self.show_cc_bcc = false;
        self.attachments.clear();
        self.prompt = None;
        self.start_body(DraftBodySeed::default());
        self.plain_body.set_text("");
        self.heading = NEW_MESSAGE;
        self.set_focus(Field::To);
        self.dirty = true;
    }

    /// Seed the window as a reply: recipient, subject, and a quoted body.
    pub fn open_reply(&mut self, to: &str, subject: &str, quote: &str) {
        self.open_blank();
        self.to.set_text(to);
        self.subject.set_text(subject);
        self.heading = "Reply";
        match self.format {
            ComposeFormat::Html => self.start_body(DraftBodySeed::text(quote)),
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
        self.heading = "Reply all";
        if !cc.trim().is_empty() {
            self.cc.set_text(cc);
            self.show_cc_bcc = true;
        }
    }

    /// Seed the window to pass a message on: the subject and the forwarded
    /// message as the body, with no recipient yet.
    pub fn open_forward(&mut self, subject: &str, quoted: &str) {
        self.open_reply("", subject, quoted);
        self.heading = "Forward";
    }

    /// Reopen an existing draft for editing (from the Drafts folder).
    pub fn open_with_draft(&mut self, draft: &Draft) {
        self.open_blank();
        self.sender.clone_from(&draft.sender);
        self.to.set_text(&draft.to);
        self.cc.set_text(&draft.cc);
        self.bcc.set_text(&draft.bcc);
        self.subject.set_text(&draft.subject);
        self.show_cc_bcc = !draft.cc.is_empty() || !draft.bcc.is_empty();
        self.attachments.clone_from(&draft.attachments);
        match self.format {
            ComposeFormat::Html => self.start_body(DraftBodySeed::of_draft(draft)),
            ComposeFormat::Text => self.plain_body.set_text(&draft.body),
        }
        self.heading = "Draft";
        self.replying_to = draft.in_reply_to;
        // A reopened draft is a draft already.
        self.touched = true;
        self.set_focus(Field::Body);
    }

    /// Whether the composer waits for the person's correspondents.
    pub(crate) fn wants_address_book(&self) -> bool {
        self.open && self.book.is_none()
    }

    /// Complete recipients from `book`.
    pub(crate) fn set_address_book(&mut self, book: crate::address_book::AddressBook) {
        self.book = Some(book);
    }

    /// The recipient field with the keys, if one has them.
    fn recipient_field(&mut self) -> Option<&mut TextInput> {
        match self.focus {
            Field::To => Some(&mut self.to),
            Field::Cc => Some(&mut self.cc),
            Field::Bcc => Some(&mut self.bcc),
            _ => None,
        }
    }

    /// Offer the correspondents the recipient being typed could be.
    fn refresh_completions(&mut self) {
        let viewport = self.bounds;
        let Some((text, anchor)) = self
            .recipient_field()
            .map(|field| (field.value().to_owned(), field.get_rect()))
        else {
            self.completions.clear();
            return;
        };
        let Some(book) = self.book.as_ref() else {
            self.completions.clear();
            return;
        };
        let (_, typing) = crate::address_book::last_recipient(&text);
        let offered: Vec<ui::widget::Suggestion> = book
            .complete(typing, &text)
            .into_iter()
            .map(|addr| {
                let suggestion = ui::widget::Suggestion::labelled(addr.rfc5322(), addr.display());
                if addr.name.is_some() {
                    suggestion.with_detail(addr.email)
                } else {
                    suggestion
                }
            })
            .collect();
        self.completions.set_suggestions(offered);
        if self.completions.has_content() {
            self.completions.set_highlight(Some(0));
        }
        self.completions.set_anchor(anchor);
        self.completions.set_viewport(viewport);
    }

    /// Put the offered completion `index` into the recipient field.
    fn accept_completion(&mut self, index: usize) {
        let Some(chosen) = self
            .completions
            .suggestions()
            .get(index)
            .map(|suggestion| suggestion.value.clone())
        else {
            return;
        };
        if let Some(field) = self.recipient_field() {
            let completed = crate::address_book::complete_with(field.value(), &chosen);
            field.set_text(&completed);
        }
        self.completions.clear();
        self.dirty = true;
    }

    /// Keys the offered completions take: arrows move through them, Enter
    /// or Tab takes one, Esc puts them away. `true` when the key was theirs.
    fn completion_key(&mut self, event: &WindowEvent) -> bool {
        use winit::event::KeyEvent as WKeyEvent;
        use winit::keyboard::{Key as WKey, NamedKey as WNamedKey};
        if !self.completions.has_content() {
            return false;
        }
        let WindowEvent::KeyboardInput {
            event:
                WKeyEvent {
                    logical_key: WKey::Named(key),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        match key {
            WNamedKey::ArrowDown => self.completions.move_highlight(true),
            WNamedKey::ArrowUp => self.completions.move_highlight(false),
            WNamedKey::Enter | WNamedKey::Tab => {
                let index = self.completions.highlight().unwrap_or(0);
                self.accept_completion(index);
            }
            WNamedKey::Escape => self.completions.clear(),
            _ => return false,
        }
        self.dirty = true;
        true
    }

    /// The message this one answers.
    pub fn reply_to(&mut self, message: crate::model::MessageId) {
        self.replying_to = Some(message);
    }

    /// Whether closing should keep what was written as a draft.
    pub fn touched(&self) -> bool {
        self.touched
    }

    /// Open the window pre-filled with a representative formatted message, used
    /// by the headless screenshot to show the WYSIWYG editor in action.
    pub fn open_demo(&mut self) {
        self.open_blank();
        self.format = ComposeFormat::Html;
        self.to.set_text("Maya Chen <maya@northwind.example>");
        self.subject.set_text("Re: Spring launch plan");
        self.heading = "Reply";
        let text = "Launch order\n\
            Thanks both. The new order works for me: website first, then the press kit, then the email campaign.\n\
            Website copy final by Wednesday\n\
            Press kit review on Thursday\n\
            Email campaign goes out last\n\
            Let’s not send the campaign before the website is live.";
        self.start_body(DraftBodySeed {
            text: text.to_owned(),
            html: Some(
                "<h2>Launch order</h2>\
                 <p>Thanks both. The new order works for me: <b>website first, then the press kit, then the email campaign</b>.</p>\
                 <ul><li>Website copy final by Wednesday</li>\
                 <li>Press kit review on Thursday</li>\
                 <li>Email campaign goes out last</li></ul>\
                 <blockquote><i>Let’s not send the campaign before the website is live.</i></blockquote>"
                    .to_owned(),
            ),
        });
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
                let flat = self.body_plain.clone();
                self.plain_body.set_text(&flat);
                self.format = ComposeFormat::Text;
            }
            ComposeFormat::Text => {
                // plain → HTML: a paragraph per line.
                self.start_body(DraftBodySeed::text(self.plain_body.value()));
                self.format = ComposeFormat::Html;
            }
        }
        if matches!(self.focus, Field::Body) {
            self.set_focus(Field::Body);
        }
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
        self.start_body(DraftBodySeed::of_draft(draft));
        self.placed = false;
        self.dirty = true;
    }

    /// The current draft, with both a plain body and (in HTML mode) an HTML
    /// body, ready for `transport::build_mime`.
    pub fn to_draft(&mut self, doc: &mut ui::widget::Edits<'_>) -> Draft {
        self.start_pending_body(doc);
        let (body, html_body) = match self.format {
            ComposeFormat::Html => {
                let root = draft_body_object(doc);
                let html = root.as_ref().and_then(|root| {
                    tilo_doc_codecs::export_html::render_html_fragment_under(
                        doc,
                        root,
                        &tilo_content::NoContent,
                    )
                });
                (Self::read_plain(doc), Some(html.unwrap_or_default()))
            }
            ComposeFormat::Text => (self.plain_body.value().to_string(), None),
        };
        Draft {
            sender: self.sender.clone(),
            to: self.to.value().to_string(),
            cc: self.cc.value().to_string(),
            bcc: self.bcc.value().to_string(),
            subject: self.subject.value().to_string(),
            body,
            html_body,
            calendar_ics: None,
            attachments: self.attachments.clone(),
            in_reply_to: self.replying_to,
            wire: crate::model::WireIds::default(),
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
        self.touched = true;
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

    // ---- body ----

    /// Start the body afresh from `seed` when the window next reaches the
    /// document; until then the body reads as the seed's text.
    fn start_body(&mut self, seed: DraftBodySeed) {
        self.editor = body_editor(self.dispatcher.as_ref());
        self.body_layout = BodyLayout::default();
        self.body_plain.clone_from(&seed.text);
        self.seed = Some(seed);
        self.dirty = true;
    }

    /// Attach the framework dispatcher: a new draft body and every edit of
    /// it are that resource's actions.
    pub(crate) fn attach_framework(&mut self, dispatcher: Dispatcher<Mail>) {
        self.editor
            .source_mut()
            .attach(draft_body_dispatcher(&dispatcher));
        self.dispatcher = Some(dispatcher);
    }

    /// Apply a pending [`Self::start_body`].
    fn start_pending_body(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(dispatcher) = &self.dispatcher else {
            return;
        };
        let Some(seed) = self.seed.take() else {
            return;
        };
        let start = Mail::START_DRAFT_BODY.with(mailbox::StartDraftBody {
            text: seed.text,
            html: seed.html,
        });
        if let Err(error) = dispatcher.submit_now(doc, start) {
            tracing::warn!(%error, "draft body could not be started");
        }
    }

    fn read_plain(doc: &automerge::AutoCommit) -> String {
        draft_body_object(doc)
            .and_then(|root| tilo_rich_text::blocks_under(doc, &root).ok())
            .map(|blocks| plain_of(&blocks))
            .unwrap_or_default()
    }

    /// Bring the body up to date with the document: a pending start, and
    /// what peers or an undo changed. Called by the host every frame.
    pub fn sync(&mut self, doc: &automerge::AutoCommit, path: &automerge::ObjId) {
        if !self.open || self.format != ComposeFormat::Html {
            return;
        }
        ui::Widget::sync(&mut self.editor, doc, path);
        self.body_plain = Self::read_plain(doc);
        if ui::Widget::needs_redraw(&self.editor) {
            self.dirty = true;
        }
    }

    /// Author a pending draft start and the body editor's queued actions.
    pub fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &automerge::ObjId) {
        if !self.open || self.format != ComposeFormat::Html {
            return;
        }
        self.start_pending_body(edits);
        ui::Widget::settle(&mut self.editor, edits, path);
    }

    fn focused_kind(&self) -> BlockKind {
        self.editor
            .editor()
            .focused_kind()
            .cloned()
            .unwrap_or(BlockKind::Paragraph)
    }

    fn toggle_mark(&mut self, mark: Mark) {
        self.editor.editor_mut().toggle_mark(mark);
        self.dirty = true;
    }

    /// Make the focused paragraph `kind`, or a plain paragraph again when it
    /// already is one.
    fn toggle_kind(&mut self, kind: BlockKind) {
        let editor = self.editor.editor_mut();
        let Some(index) = editor.focused_index() else {
            return;
        };
        let already = editor.focused_kind() == Some(&kind);
        editor.try_convert_to(index, if already { BlockKind::Paragraph } else { kind });
        self.dirty = true;
    }

    /// Lay the editor out in the body card, when the card or the text
    /// changed since the last layout.
    fn lay_out_body(&mut self, text: &mut TextRenderer, theme: &Theme) {
        let card = self.body_rect();
        let layout = &mut self.body_layout;
        let stale = layout.node.is_none()
            || layout.laid_in != Some(card)
            || ui::Widget::take_structure_dirty(&mut self.editor)
            || ui::Widget::is_layout_dirty(&self.editor);
        if !stale {
            return;
        }
        layout.taffy = TaffyTree::new();
        let mut nodes: std::collections::HashMap<NodeId, *mut dyn ui::Widget> =
            std::collections::HashMap::new();
        let node = ui::Widget::build_layout(&mut self.editor, &mut layout.taffy, theme, &mut nodes);
        // The text's inset from its card: `s3` beside it, `s2` above.
        let (pad_x, pad_y) = (
            f64::from(theme.tokens.spacing.s3),
            f64::from(theme.tokens.spacing.s2),
        );
        let width = (card.width() - 2.0 * pad_x).max(40.0);
        if let Ok(style) = layout.taffy.style(node).cloned() {
            let _ = layout.taffy.set_style(
                node,
                taffy::Style {
                    size: taffy::Size {
                        width: taffy::Dimension::length(width as f32),
                        height: taffy::Dimension::auto(),
                    },
                    ..style
                },
            );
        }
        // Lay the editor out as the host lays out any widget tree, measuring
        // every block with the renderer it is drawn with, so a wrapped
        // paragraph takes the lines it needs.
        let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
        // SAFETY: every pointer in `nodes` was registered into this tree by
        // the editor's own `build_layout` above, and the editor is neither
        // moved nor rebuilt while this layout runs.
        let _ = unsafe {
            bindings.rebuild(&self.editor, &layout.taffy, node, &nodes);
            ui::widget::compute_widget_layout(
                &mut layout.taffy,
                node,
                taffy::prelude::TaffyMaxContent::MAX_CONTENT,
                &nodes,
                &bindings,
                text,
            )
        };
        layout.content_h = layout
            .taffy
            .layout(node)
            .map_or(0.0, |laid| f64::from(laid.size.height));
        let max_scroll = (layout.content_h + 2.0 * pad_y - card.height()).max(0.0);
        layout.scroll = layout.scroll.clamp(0.0, max_scroll);
        layout.node = Some(node);
        layout.laid_in = Some(card);
        let origin = Point::new(card.x0 + pad_x, card.y0 + pad_y - layout.scroll);
        ui::Widget::apply_layout(&mut self.editor, &layout.taffy, node, origin);
    }

    /// An event for the body editor.
    fn body_event(&mut self, event: &WindowEvent, ctx: &mut ComposeEventContext<'_>) {
        self.start_pending_body(&mut ctx.state);
        if self.body_layout.node.is_none() {
            // An event before the first frame: lay out with the last theme.
            let theme = self.theme.clone();
            self.lay_out_body(&mut TextRenderer::new(), &theme);
        }
        let Some(node) = self.body_layout.node else {
            return;
        };
        if let WindowEvent::MouseWheel { delta, .. } = event
            && self.body_rect().contains(self.last_mouse)
        {
            let lines = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => {
                    f64::from(*y) * f64::from(self.theme.text(TextRole::Body).line_height_px())
                }
                winit::event::MouseScrollDelta::PixelDelta(p) => p.y / ctx.scale_factor,
            };
            self.body_layout.scroll -= lines;
            self.body_layout.laid_in = None;
            self.dirty = true;
            return;
        }
        let mut handled = false;
        let mut focus = None;
        ui::Widget::handle_event(
            &mut self.editor,
            event,
            &mut ui::widget::EventCtx {
                scale_factor: ctx.scale_factor,
                modifiers: &ctx.modifiers,
                state: ctx.state.reborrow(),
                path: ctx.path,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &self.body_layout.taffy,
            },
        );
        self.body_plain = Self::read_plain(ctx.state.doc());
        self.dirty = true;
    }

    fn set_focus(&mut self, field: Field) {
        self.focus = field;
        self.to.set_focus(matches!(field, Field::To));
        self.cc.set_focus(matches!(field, Field::Cc));
        self.bcc.set_focus(matches!(field, Field::Bcc));
        self.subject.set_focus(matches!(field, Field::Subject));
        ui::Widget::set_focus(
            &mut self.editor,
            matches!(field, Field::Body) && matches!(self.format, ComposeFormat::Html),
        );
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
    /// The message column to write in (`None` where there is none, and
    /// the composer floats over the app instead).
    pub fn set_pane(&mut self, pane: Option<Rect>) {
        if self.pane != pane {
            self.pane = pane;
            self.dirty = true;
        }
    }

    /// Whether the composer stands in the message column, beside the list.
    pub fn in_pane(&self) -> bool {
        matches!(self.dock, DockState::Pane) && self.pane.is_some() && !self.is_full_window_page()
    }

    /// Whether the composer holds every click and key while open: it
    /// floats over the app or fills the window.
    pub fn is_modal(&self) -> bool {
        self.open && !self.in_pane()
    }

    pub fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        if let Some(pane) = self.pane.filter(|_| self.in_pane()) {
            self.rect = pane;
            self.drag = None;
            self.placed = false;
            self.relayout_regions();
            return;
        }
        if self.is_full_window_page() {
            self.rect = bounds;
            self.drag = None;
            // Returning to a roomy viewport should restore a floating size,
            // rather than preserving the phone page as a desktop window.
            self.placed = false;
            self.relayout_regions();
            return;
        }
        if !self.placed {
            let m = self.metrics;
            let margin = m.pad * 2.0;
            let (w, h) = (
                m.default_w.min(bounds.width() - margin).max(m.min_w),
                m.default_h.min(bounds.height() - margin).max(m.min_h),
            );
            self.rect = Rect::from_center_size(bounds.center(), (w, h));
            self.placed = true;
        }
        self.clamp_into_bounds();
        self.relayout_regions();
    }

    fn is_full_window_page(&self) -> bool {
        ui::widget::modal_dialog::uses_full_window(self.bounds, &self.theme)
    }

    fn clamp_into_bounds(&mut self) {
        let bounds = self.bounds;
        let (min_w, min_h) = (self.metrics.min_w, self.metrics.min_h);
        let r = self.rect;
        let w = r.width().min(bounds.width()).max(min_w.min(bounds.width()));
        let h = r
            .height()
            .min(bounds.height())
            .max(min_h.min(bounds.height()));
        let x0 = r.x0.clamp(bounds.x0, (bounds.x1 - w).max(bounds.x0));
        let y0 = r.y0.clamp(bounds.y0, (bounds.y1 - h).max(bounds.y0));
        self.rect = Rect::new(x0, y0, x0 + w, y0 + h);
    }

    /// Lay out and register the title bar (drag handle and window
    /// buttons), the To/Cc/Bcc/Subject fields and the Attach button.
    /// Returns the y past the header, where the toolbar starts. Part of
    /// [`Self::relayout_regions`].
    fn layout_header_hits(&mut self, r: Rect, lx: f64, rx: f64) -> f64 {
        let m = self.metrics;
        let field_x = lx + m.label_w;

        // Title bar (drag handle) and its icon buttons, centred on it.
        let title = Rect::new(r.x0, r.y0, r.x1, r.y0 + m.title_h);
        self.hits.push((title, ComposeHit::TitleBar));
        let side = m.icon_button;
        let btn_y = title.center().y - side / 2.0;
        let close = Rect::new(rx - side, btn_y, rx, btn_y + side);
        self.hits.push((close, ComposeHit::Close));

        let mut y = title.y1 + m.pad;

        // To row, with the Cc/Bcc button at its end.
        let to_rect = Rect::new(field_x, y, rx - m.cc_bcc_w - m.gap, y + m.control_h);
        self.to.apply_layout_external(to_rect);
        self.hits.push((to_rect, ComposeHit::FieldTo));
        self.hits.push((
            Rect::new(rx - m.cc_bcc_w, y, rx, y + m.control_h),
            ComposeHit::ToggleCcBcc,
        ));
        y += m.control_h + m.gap;
        if self.show_cc_bcc {
            let cc_rect = Rect::new(field_x, y, rx, y + m.control_h);
            self.cc.apply_layout_external(cc_rect);
            self.hits.push((cc_rect, ComposeHit::FieldCc));
            y += m.control_h + m.gap;
            let bcc_rect = Rect::new(field_x, y, rx, y + m.control_h);
            self.bcc.apply_layout_external(bcc_rect);
            self.hits.push((bcc_rect, ComposeHit::FieldBcc));
            y += m.control_h + m.gap;
        }
        // Subject, with the Attach button at its end.
        let subject_rect = Rect::new(field_x, y, rx - side - m.gap, y + m.control_h);
        self.subject.apply_layout_external(subject_rect);
        self.hits.push((subject_rect, ComposeHit::FieldSubject));
        let attach_y = y + (m.control_h - side) / 2.0;
        self.hits.push((
            Rect::new(rx - side, attach_y, rx, attach_y + side),
            ComposeHit::Attach,
        ));
        y + m.control_h + m.gap
    }

    fn relayout_regions(&mut self) {
        self.hits.clear();
        let r = self.rect;
        let m = self.metrics;
        let lx = r.x0 + m.pad;
        let rx = r.x1 - m.pad;
        let mut y = self.layout_header_hits(r, lx, rx);

        // The formatting toolbar (rich text) and the format switch at the
        // end of the same row.
        let format = Rect::new(rx - m.format_w, y, rx, y + m.control_h);
        self.hits.push((format, ComposeHit::FormatToggle));
        if matches!(self.format, ComposeFormat::Html) {
            let toolbar = Rect::new(lx, y, (format.x0 - m.gap).max(lx), y + m.control_h);
            self.hits.push((toolbar, ComposeHit::Toolbar));
        }
        y += m.control_h + m.gap;

        // Below the body, bottom-up: the footer buttons, then (when shown)
        // the link prompt and the attachment chips.
        let by = r.y1 - m.pad - m.control_h;
        let prompt_h = if self.prompt.is_some() {
            m.control_h + m.gap
        } else {
            0.0
        };
        self.prompt_band = Rect::new(lx, by - m.gap - prompt_h, rx, by - m.gap);
        let chips_h = if self.attachments.is_empty() {
            0.0
        } else {
            m.chip_h + m.gap
        };
        self.chips_band = Rect::new(lx, self.prompt_band.y0 - chips_h, rx, self.prompt_band.y0);
        let body = Rect::new(lx, y, rx, (self.chips_band.y0 - m.gap).max(y));
        self.hits.push((body, ComposeHit::Body));
        match self.format {
            // Laid out when drawn: the editor measures with the theme.
            ComposeFormat::Html => {}
            ComposeFormat::Text => self.plain_body.apply_layout_external(body),
        }

        // Footer: Discard at the leading edge, Send at the trailing edge.
        // What is written is kept as a draft when the composer closes, so
        // there is no Save draft button to remember.
        let bh = m.control_h;
        self.hits.push((
            Rect::new(lx, by, lx + m.discard_w, by + bh),
            ComposeHit::Discard,
        ));
        let send = Rect::new(rx - m.send_w, by, rx, by + bh);
        self.hits.push((send, ComposeHit::Send));

        // The link prompt: a field and its two buttons.
        if let Some(p) = self.prompt.as_mut() {
            let band = self.prompt_band;
            let py = band.y0;
            let small = (m.control_h - ButtonFace::height(ControlSize::Small, &self.theme)) / 2.0;
            let cancel = Rect::new(
                rx - m.prompt_cancel_w,
                py + small,
                rx,
                py + m.control_h - small,
            );
            let confirm = Rect::new(
                cancel.x0 - m.gap - m.prompt_add_w,
                cancel.y0,
                cancel.x0 - m.gap,
                cancel.y1,
            );
            let field = Rect::new(lx + m.label_w, py, confirm.x0 - m.gap, py + m.control_h);
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
            inp.handle_event(event, &mut ctx.event_ctx(&mut h, &mut ft));
        }
        // Forward to the plain body too so its hover / drag-selection tracks
        // (Text mode).
        if matches!(self.format, ComposeFormat::Text) {
            self.plain_body
                .handle_event(event, &mut ctx.event_ctx(&mut h, &mut ft));
        }
        if let Some(p) = self.prompt.as_mut() {
            p.input
                .handle_event(event, &mut ctx.event_ctx(&mut h, &mut ft));
        }
        if matches!(self.format, ComposeFormat::Html) {
            self.body_event(event, ctx);
        }
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
        if self.toolbar_takes(event, ctx.scale_factor) {
            self.route_toolbar(event, ctx);
            self.dirty = true;
            return (true, ComposeAction::None);
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                if matches!(self.format, ComposeFormat::Html) {
                    // The toolbar tracks the pointer for its hover.
                    self.route_toolbar(event, ctx);
                }
                self.handle_cursor_moved(*position, event, ctx)
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let pt = self.last_mouse;
                if let Some(index) = self.completions.row_at(pt) {
                    self.accept_completion(index);
                    return (true, ComposeAction::None);
                }
                self.completions.clear();
                if !self.rect.contains(pt) {
                    if self.in_pane() {
                        // The list beside it takes the click, and the keys
                        // with it; the message stays as it is.
                        self.has_keys = false;
                        self.set_focus(Field::None);
                        return (false, ComposeAction::None);
                    }
                    // Click outside the floating window: stay modal. Swallow
                    // it (do not close — explicit Discard/Close only), so
                    // nothing behind steals focus or clicks.
                    return (true, ComposeAction::None);
                }
                self.has_keys = true;
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
            WindowEvent::KeyboardInput { .. } if self.in_pane() && !self.has_keys => {
                (false, ComposeAction::None)
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
        // Offered recipients take their keys first (Esc puts them away
        // rather than closing the message).
        if self.completion_key(event) {
            return (true, ComposeAction::None);
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
            p.input
                .handle_event(event, &mut ctx.event_ctx(&mut h, &mut ft));
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
        self.touched = true;
        match self.format {
            ComposeFormat::Html => self.body_event(event, ctx),
            ComposeFormat::Text => {
                let mut h = false;
                let mut ft: Option<taffy::NodeId> = None;
                self.plain_body
                    .handle_event(event, &mut ctx.event_ctx(&mut h, &mut ft));
            }
        }
        self.dirty = true;
    }

    /// Forward a key to whichever header field (To/Cc/Bcc/Subject) has focus.
    /// Part of [`Self::handle_keyboard_input`].
    fn handle_header_key(&mut self, event: &WindowEvent, ctx: &mut ComposeEventContext<'_>) {
        self.touched = true;
        let mut h = false;
        let mut ft: Option<taffy::NodeId> = None;
        if let Some(inp) = self.focused_input() {
            inp.handle_event(event, &mut ctx.event_ctx(&mut h, &mut ft));
        }
        self.refresh_completions();
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
                if !self.is_full_window_page() && !self.in_pane() {
                    self.drag = Some(Drag::Move {
                        grab: Point::new(pt.x - self.rect.x0, pt.y - self.rect.y0),
                    });
                }
            }
            ComposeHit::Close => {
                self.close();
                return ComposeAction::Close;
            }
            ComposeHit::Discard => {
                self.close();
                return ComposeAction::Discard;
            }
            ComposeHit::Send => return ComposeAction::Send,
            ComposeHit::SaveDraft => return ComposeAction::SaveDraft,
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
            ComposeHit::Toolbar => {
                self.route_toolbar(event, ctx);
            }
            ComposeHit::PromptConfirm => self.confirm_prompt(),
            ComposeHit::PromptCancel => self.prompt = None,
            ComposeHit::PromptField => {
                if let Some(p) = self.prompt.as_mut() {
                    p.input.set_focus(true);
                    let mut h = false;
                    let mut ft: Option<taffy::NodeId> = None;
                    p.input
                        .handle_event(event, &mut ctx.event_ctx(&mut h, &mut ft));
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
                let _ = pt;
                self.body_event(event, ctx);
            }
            ComposeFormat::Text => {
                // Forward the real press so the body places its caret at the
                // click point (the preceding CursorMoved set hover).
                let mut h = false;
                let mut ft: Option<taffy::NodeId> = None;
                self.plain_body
                    .handle_event(event, &mut ctx.event_ctx(&mut h, &mut ft));
            }
        }
    }

    /// Whether the formatting toolbar owns `event`: its overflow menu is
    /// open, it is mid-press, or the pointer is over it.
    fn toolbar_takes(&self, event: &WindowEvent, scale_factor: f64) -> bool {
        if !matches!(self.format, ComposeFormat::Html) {
            return false;
        }
        let bar = &self.format_bar.bar;
        if bar.is_overflow_open() || ui::Widget::wants_pointer_capture(bar) {
            return !matches!(event, WindowEvent::CursorMoved { .. }) || bar.is_overflow_open();
        }
        let over = |point: Point| {
            self.hits
                .iter()
                .any(|(rect, hit)| *hit == ComposeHit::Toolbar && rect.contains(point))
        };
        match event {
            WindowEvent::MouseInput { .. } => over(self.last_mouse),
            WindowEvent::CursorMoved { position, .. } => {
                let _ = (position, scale_factor);
                false
            }
            _ => false,
        }
    }

    /// Route an event to the formatting toolbar and apply what it chose.
    fn route_toolbar(&mut self, event: &WindowEvent, ctx: &mut ComposeEventContext<'_>) {
        let mut handled = false;
        let mut focus = None;
        ui::Widget::handle_event(
            &mut self.format_bar.bar,
            event,
            &mut ctx.event_ctx(&mut handled, &mut focus),
        );
        if let Some(format) = self.format_bar.chosen.take() {
            self.apply_format(format);
        }
    }

    fn apply_format(&mut self, format: Format) {
        match format {
            Format::Bold => self.toggle_mark(Mark::Bold),
            Format::Italic => self.toggle_mark(Mark::Italic),
            Format::Underline => self.toggle_mark(Mark::Underline),
            Format::Heading1 => self.toggle_kind(BlockKind::Heading(1)),
            Format::Heading2 => self.toggle_kind(BlockKind::Heading(2)),
            Format::Bullet => self.toggle_kind(BlockKind::Bullet { indent: 0 }),
            Format::Numbered => self.toggle_kind(BlockKind::Numbered { indent: 0 }),
            Format::Quote => self.toggle_kind(BlockKind::Quote),
            Format::Link => self.open_prompt(PromptKind::Link),
        }
    }

    /// Open the inline prompt bar to collect a URL. File selection always
    /// goes through the root's host-native portal boundary.
    fn open_prompt(&mut self, kind: PromptKind) {
        let mut input = TextInput::local().with_kind(TextInputKind::Url);
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
                        self.editor.editor_mut().insert_link(&val);
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
            inp.handle_event(event, &mut ctx.event_ctx(&mut h, &mut ft));
        }
    }

    // ---- move / resize ----

    fn resize_edge_at(&self, pt: Point) -> Option<Edge> {
        if self.is_full_window_page() || self.in_pane() {
            return None;
        }
        let r = self.rect;
        // The grip is the window's own edge, a `s2` band inside it.
        let grip = f64::from(self.theme.tokens.spacing.s2);
        if !r.contains(pt) {
            return None;
        }
        let near_r = r.x1 - pt.x <= grip;
        let near_l = pt.x - r.x0 <= grip;
        let near_b = r.y1 - pt.y <= grip;
        // The title bar moves the window; only the sides and bottom resize.
        match (near_l, near_r, near_b) {
            (_, true, true) => Some(Edge::BottomRight),
            (true, _, true) => Some(Edge::BottomLeft),
            (_, true, _) => Some(Edge::Right),
            (true, _, _) if pt.y > r.y0 + self.metrics.title_h => Some(Edge::Left),
            (_, _, true) => Some(Edge::Bottom),
            _ => None,
        }
    }

    /// The pointer over the window's edges says it resizes.
    pub fn cursor_shape(&self, pt: Point) -> Option<winit::window::CursorIcon> {
        use winit::window::CursorIcon;
        if !self.open {
            return None;
        }
        let edge = match self.drag {
            Some(Drag::Resize { edge, .. }) => Some(edge),
            _ => self.resize_edge_at(pt),
        };
        edge.map(|edge| match edge {
            Edge::Right | Edge::Left => CursorIcon::EwResize,
            Edge::Bottom => CursorIcon::NsResize,
            Edge::BottomRight => CursorIcon::SeResize,
            Edge::BottomLeft => CursorIcon::SwResize,
        })
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
                let (min_w, min_h) = (self.metrics.min_w, self.metrics.min_h);
                let dx = pt.x - from.x;
                let dy = pt.y - from.y;
                let mut r = start;
                match edge {
                    Edge::Right => r.x1 = (start.x1 + dx).max(start.x0 + min_w),
                    Edge::Bottom => r.y1 = (start.y1 + dy).max(start.y0 + min_h),
                    Edge::BottomRight => {
                        r.x1 = (start.x1 + dx).max(start.x0 + min_w);
                        r.y1 = (start.y1 + dy).max(start.y0 + min_h);
                    }
                    Edge::Left => r.x0 = (start.x0 + dx).min(start.x1 - min_w),
                    Edge::BottomLeft => {
                        r.x0 = (start.x0 + dx).min(start.x1 - min_w);
                        r.y1 = (start.y1 + dy).max(start.y0 + min_h);
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
        // Measure with this frame's theme and lay the regions out again
        // from those sizes before painting, so what is painted is what is
        // hit-tested.
        self.theme = theme.clone();
        let measured = ComposeMetrics::measure(tr, theme);
        if self.metrics != measured {
            self.metrics = measured;
            self.layout(self.bounds);
        }
        // In the message column, or filling the window, it is the page
        // itself; floating, it is a modal panel over the app.
        if self.in_pane() {
            // The message column's own ground: the page where the open
            // message is read.
            scene.fill(
                Fill::NonZero,
                t,
                theme.tokens.colors.surfaces.content,
                None,
                &self.rect,
            );
        } else if self.is_full_window_page() {
            ui::floating_surface::paint_material(
                scene,
                t,
                &self.rect,
                ui::floating_surface::Material::Opaque,
                theme,
            );
        } else {
            ui::overlay::scrim::behind_panel(
                scene,
                t,
                self.bounds,
                vello::kurbo::RoundedRect::from_rect(
                    self.rect,
                    ui::widget::floating_surface::radius(theme),
                ),
                theme,
            );
            ui::widget::floating_surface::paint(scene, t, self.rect, theme);
        }
        // What is drawn on the panel reads against the panel's ground.
        let panel = theme.on_ground(self.ground(theme));
        self.draw_title_bar(scene, t, tr, &panel);
        self.draw_header_fields(scene, t, tr, &panel);
        self.draw_body(scene, t, tr, &panel, presence);
        self.draw_staged_attachment_chips(scene, t, tr, &panel);
        self.draw_prompt(scene, t, tr, &panel, presence);
        for button in [
            (
                ComposeHit::Discard,
                "mail-compose-discard",
                ButtonRole::Destructive,
            ),
            (ComposeHit::Send, "mail-compose-send", ButtonRole::Primary),
        ] {
            self.draw_button(scene, t, tr, &panel, button);
        }
        // The toolbar's overflow menu floats over everything else.
        if matches!(self.format, ComposeFormat::Html) {
            ui::Widget::draw_overlay(&mut self.format_bar.bar, scene, t, tr, &panel, presence);
        }
    }

    /// What the composer is painted on: the message column's ground, or
    /// the panel's where it floats or fills the window.
    fn ground(&self, theme: &Theme) -> vello::peniko::Color {
        if self.in_pane() {
            theme.tokens.colors.surfaces.content
        } else {
            ui::chrome::panel_ground(theme)
        }
    }

    fn hit_rect(&self, hit: ComposeHit) -> Option<Rect> {
        self.hits.iter().find(|(_, h)| *h == hit).map(|(r, _)| *r)
    }

    /// The title, the pop-out and close buttons, and the rule under them.
    fn draw_title_bar(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let m = self.metrics;
        let r = self.rect;
        let bar = Rect::new(r.x0, r.y0, r.x1, r.y0 + m.title_h);
        let heading = self.heading;
        let close = self.hit_rect(ComposeHit::Close).unwrap_or(Rect::ZERO);
        let style = theme.text(TextRole::Subheading);
        let width = (close.x0 - m.gap - (r.x0 + m.pad)).max(0.0);
        let heading = tr.elide_to_width_style(heading, ui::num::f32_of_f64(width), &style);
        let (_, h) = tr.measure_style(&heading, None, &style);
        tr.draw_style(
            scene,
            &heading,
            theme.text_primary,
            t * Affine::translate((r.x0 + m.pad, bar.center().y - f64::from(h) / 2.0)),
            None,
            &style,
        );
        for (hit, icon, name) in [(ComposeHit::Close, Icon::X, "Close")] {
            let Some(rect) = self.hit_rect(hit) else {
                continue;
            };
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::icon_only(rect, icon, name).hovered(rect.contains(self.last_mouse)),
            );
        }
        ui::surface::separator(
            scene,
            t,
            r.x0,
            r.x1,
            bar.y1,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
    }

    /// The field labels, the Cc/Bcc and Attach buttons, the formatting
    /// toolbar and the format switch. Part of [`Self::draw`].
    fn draw_header_fields(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let mut fields = vec![(ComposeHit::FieldTo, "mail-compose-label-to")];
        if self.show_cc_bcc {
            fields.push((ComposeHit::FieldCc, "mail-compose-label-cc"));
            fields.push((ComposeHit::FieldBcc, "mail-compose-label-bcc"));
        }
        fields.push((ComposeHit::FieldSubject, "mail-compose-label-subject"));
        let label = theme.text(TextRole::Label);
        for (hit, key) in fields {
            let Some(field) = self.hit_rect(hit) else {
                continue;
            };
            let text = crate::i18n::t(key);
            let (_, h) = tr.measure_style(&text, None, &label);
            tr.draw_style(
                scene,
                &text,
                theme.text_dim,
                t * Affine::translate((
                    self.rect.x0 + self.metrics.pad,
                    field.center().y - f64::from(h) / 2.0,
                )),
                None,
                &label,
            );
        }
        if let Some(rect) = self.hit_rect(ComposeHit::ToggleCcBcc) {
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::new(rect, "mail-compose-cc-bcc")
                    .role(ButtonRole::Plain)
                    .hovered(rect.contains(self.last_mouse)),
            );
        }
        if let Some(rect) = self.hit_rect(ComposeHit::Attach) {
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::icon_only(rect, Icon::Paperclip, "Attach file")
                    .hovered(rect.contains(self.last_mouse)),
            );
        }
        if matches!(self.format, ComposeFormat::Html)
            && let Some(rect) = self.hit_rect(ComposeHit::Toolbar)
        {
            let marks = self.editor.editor().active_marks();
            let block = self.focused_kind();
            self.format_bar.set_active(|format| match format {
                Format::Bold => marks.contains(&Mark::Bold),
                Format::Italic => marks.contains(&Mark::Italic),
                Format::Underline => marks.contains(&Mark::Underline),
                Format::Heading1 => matches!(block, BlockKind::Heading(1)),
                Format::Heading2 => matches!(block, BlockKind::Heading(2)),
                Format::Bullet => matches!(block, BlockKind::Bullet { .. }),
                Format::Numbered => matches!(block, BlockKind::Numbered { .. }),
                Format::Quote => matches!(block, BlockKind::Quote),
                Format::Link => false,
            });
            self.format_bar.bar.lay_out_in(rect, tr, theme);
            ui::Widget::draw(
                &mut self.format_bar.bar,
                scene,
                t,
                tr,
                theme,
                &PresenceCtx::default(),
            );
        }
        if let Some(rect) = self.hit_rect(ComposeHit::FormatToggle) {
            let segments =
                SegmentedFace::partition(rect, &FORMAT_LABELS, ControlSize::Regular, tr, theme);
            let selected = usize::from(self.format == ComposeFormat::Text);
            let hovered = segments.iter().position(|r| r.contains(self.last_mouse));
            ToggleGroup::paint(
                scene,
                t,
                tr,
                theme,
                &SegmentedFace::new(&segments, &FORMAT_LABELS, selected).hovered(hovered),
            );
        }
    }

    /// The message body: the rich editor or the plain multiline input, on
    /// the window under a rule. Part of [`Self::draw`].
    fn draw_body(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let body = self.body_rect();
        ui::surface::separator(
            scene,
            t,
            self.rect.x0,
            self.rect.x1,
            body.y0 - self.metrics.gap / 2.0,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
        match self.format {
            ComposeFormat::Html => {
                self.lay_out_body(tr, theme);
                scene.push_layer(
                    Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    t,
                    &body,
                );
                ui::Widget::draw(&mut self.editor, scene, t, tr, theme, presence);
                scene.pop_layer();
                ui::Widget::draw_overlay(&mut self.editor, scene, t, tr, theme, presence);
            }
            ComposeFormat::Text => {
                ui::widget::Widget::draw(&mut self.plain_body, scene, t, tr, theme, presence);
            }
        }
    }

    /// The staged attachments as chips above the footer. Part of
    /// [`Self::draw`].
    fn draw_staged_attachment_chips(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let band = self.chips_band;
        let h = self.metrics.chip_h;
        let mut x = band.x0;
        let lead = ChipLead::Glyph(Icon::Paperclip);
        for attachment in &self.attachments {
            let w = ChipFace::intrinsic_width(&attachment.filename, lead, false, tr, theme)
                .min(band.x1 - x);
            if w <= 0.0 {
                break;
            }
            let rect = Rect::new(x, band.y0, x + w, band.y0 + h);
            ChipFace::new(rect, &attachment.filename)
                .lead(lead)
                .paint(scene, t, tr, theme);
            x = rect.x1 + self.metrics.gap;
        }
    }

    /// The link prompt: its label, its field and its two buttons, with the
    /// reason a link was refused. Part of [`Self::draw`].
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
        let Some(field) = self.hit_rect(ComposeHit::PromptField) else {
            return;
        };
        let label = theme.text(TextRole::Label);
        let text = crate::i18n::t(match kind {
            PromptKind::Link => "mail-compose-prompt-link",
        });
        let (_, h) = tr.measure_style(&text, None, &label);
        tr.draw_style(
            scene,
            &text,
            theme.text_dim,
            t * Affine::translate((
                self.rect.x0 + self.metrics.pad,
                field.center().y - f64::from(h) / 2.0,
            )),
            None,
            &label,
        );
        for (hit, key, role) in [
            (
                ComposeHit::PromptConfirm,
                "mail-compose-prompt-add",
                ButtonRole::Primary,
            ),
            (
                ComposeHit::PromptCancel,
                "mail-compose-prompt-cancel",
                ButtonRole::Secondary,
            ),
        ] {
            let Some(rect) = self.hit_rect(hit) else {
                continue;
            };
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::new(rect, key)
                    .role(role)
                    .control_size(ControlSize::Small)
                    .hovered(rect.contains(self.last_mouse)),
            );
        }
        if let Some(error) = &error {
            let caption = theme.text(TextRole::Caption);
            tr.draw_style(
                scene,
                error,
                theme.tokens.colors.semantic.danger,
                t * Affine::translate((field.x0, field.y1)),
                None,
                &caption,
            );
        }
        if let Some(p) = self.prompt.as_mut() {
            p.input.draw(scene, t, tr, theme, presence);
        }
    }

    /// A footer button: where it is, its label key and its role.
    fn draw_button(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        (hit, key, role): (ComposeHit, &str, ButtonRole),
    ) {
        let Some(rect) = self.hit_rect(hit) else {
            return;
        };
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(rect, key)
                .role(role)
                .hovered(rect.contains(self.last_mouse)),
        );
    }

    /// Draw the header inputs (their text and caret). Called by the host
    /// after `draw` so they paint on top of the window.
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
        let panel = theme.on_ground(self.ground(theme));
        self.to.draw(scene, t, tr, &panel, presence);
        if self.show_cc_bcc {
            self.cc.draw(scene, t, tr, &panel, presence);
            self.bcc.draw(scene, t, tr, &panel, presence);
        }
        self.subject.draw(scene, t, tr, &panel, presence);
        // Offered recipients float over everything under the field.
        self.completions.paint(scene, t, tr, theme);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use winit::keyboard::{Key, NamedKey, SmolStr};

    fn key(logical: Key, text: Option<&str>) -> WindowEvent {
        ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
            logical,
            text.map(SmolStr::new),
            ElementState::Pressed,
        ))
    }

    fn win() -> ComposeWindow {
        let mut w = ComposeWindow::new(ComposeFormat::Html);
        w.attach_framework(ui::framework::testing::dispatcher("so.tilo.mail"));
        w.open_blank();
        w.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        w
    }

    #[test]
    fn composer_metrics_follow_the_theme_and_grow_with_the_text() {
        for scale in [1.0_f32, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut text = TextRenderer::new();
            text.set_text_scale(scale);
            let m = ComposeMetrics::measure(&mut text, &theme);
            assert_eq!(m.control_h, ControlSize::Regular.height(&theme));
            let mut w = ComposeWindow::new(ComposeFormat::Html);
            w.open_blank();
            w.metrics = m;
            w.layout(Rect::new(0.0, 0.0, 1280.0, 900.0));
            let to = hit_rect(&w, ComposeHit::FieldTo);
            let subject = hit_rect(&w, ComposeHit::FieldSubject);
            let send = hit_rect(&w, ComposeHit::Send);
            let discard = hit_rect(&w, ComposeHit::Discard);
            assert_eq!(to.height(), m.control_h);
            assert!(
                subject.y0 >= to.y1,
                "the subject row starts under the To row"
            );
            assert!(
                to.x0 >= w.rect.x0 + m.label_w,
                "the fields clear their labels"
            );
            assert_eq!(send.width(), m.send_w, "Send holds its label");
            assert!(discard.x1 < send.x0, "Discard stands apart at the start");
            assert!(
                w.hit_rect(ComposeHit::SaveDraft).is_none(),
                "a draft keeps itself; there is no button for it"
            );
            assert_eq!(send.height(), m.control_h);
            let body = hit_rect(&w, ComposeHit::Body);
            assert!(
                body.height() > 100.0,
                "a body remains at {scale}×: {body:?}"
            );
        }
    }

    #[test]
    fn composer_formats_and_drafts_round_trip_content() {
        // Phase `format_toggle_round_trips_content`.
        {
            let mut w = win();
            let mut doc = crate::resource::TestMail::doc();
            w.set_focus(Field::Body);
            w.start_body(DraftBodySeed::text("Hello world"));
            w.sync(&doc, &automerge::ROOT);
            assert!(
                draft_body_object(&doc).is_none(),
                "projection leaves a pending draft body unwritten"
            );
            w.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            assert!(
                draft_body_object(&doc).is_some(),
                "settlement authors the pending draft body"
            );
            w.sync(&doc, &automerge::ROOT);
            w.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            w.sync(&doc, &automerge::ROOT);
            w.toggle_format();
            assert_eq!(w.format(), ComposeFormat::Text);
            assert!(w.plain_body.value().contains("Hello world"));
            w.toggle_format();
            assert_eq!(w.format(), ComposeFormat::Html);
            assert_eq!(
                w.to_draft(&mut ui::widget::Edits::lend(&mut doc)).body,
                "Hello world"
            );
        }

        // Phase `draft_carries_html_in_html_mode`.
        {
            let mut w = win();
            let mut doc = crate::resource::TestMail::doc();
            w.set_focus(Field::Body);
            w.start_body(DraftBodySeed::text("Body\n> quoted"));
            w.to.set_text("a@b.example");
            let d = w.to_draft(&mut ui::widget::Edits::lend(&mut doc));
            let html = d.html_body.expect("an HTML part");
            assert!(html.contains("<p>Body</p>"), "got: {html}");
            assert!(html.contains("<blockquote>"), "got: {html}");
            assert_eq!(d.body, "Body\n> quoted");
            assert_eq!(d.to, "a@b.example");
        }

        // Phase `a_saved_html_draft_reopens_with_its_formatting`.
        {
            let mut w = win();
            let mut doc = crate::resource::TestMail::doc();
            w.open_with_draft(&Draft {
                body: "Plan\nship it".into(),
                html_body: Some("<h2>Plan</h2><ul><li><b>ship</b> it</li></ul>".into()),
                ..Draft::default()
            });
            let d = w.to_draft(&mut ui::widget::Edits::lend(&mut doc));
            assert_eq!(d.body, "Plan\n• ship it");
            let html = d.html_body.unwrap();
            assert!(html.contains("<h2>Plan</h2>"), "got: {html}");
            assert!(html.contains("<strong>ship</strong>"), "got: {html}");
        }

        // Phase `draft_is_plain_in_text_mode`.
        {
            let mut w = ComposeWindow::new(ComposeFormat::Text);
            w.open_blank();
            w.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
            w.plain_body.set_text("Just text");
            let d = w.to_draft(&mut ui::widget::Edits::lend(
                &mut crate::resource::TestMail::doc(),
            ));
            assert!(d.html_body.is_none());
            assert_eq!(d.body, "Just text");
        }

        // Phase `plain_body_accepts_a_multiline_draft_via_the_real_key_path`.
        // The plain body is now a shared multiline `TextInput`: typing a newline
        // through the window's real keyboard path lands a genuinely multi-line
        // draft body — the thing a single-line field could not do.
        {
            let mut w = ComposeWindow::new(ComposeFormat::Text);
            w.open_blank();
            w.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
            w.set_focus(Field::Body);

            let m = winit::event::Modifiers::default();
            let mut doc = crate::resource::TestMail::doc();
            let taffy: taffy::TaffyTree<()> = taffy::TaffyTree::new();
            let node = taffy::NodeId::new(0);
            let mut send = |ev: &WindowEvent| {
                w.handle_event(
                    ev,
                    &mut ComposeEventContext {
                        scale_factor: 1.0,
                        modifiers: m,
                        state: ui::widget::Edits::lend(&mut doc),
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
                w.to_draft(&mut ui::widget::Edits::lend(&mut doc)).body,
                "Hi\ntwo",
                "Enter split the body across two lines"
            );
        }
    }

    #[test]
    fn composer_window_moves_resizes_docks_and_owns_its_input() {
        // Phase `move_keeps_window_in_bounds`.
        {
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

        // Phase `resize_respects_minimum`.
        {
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
            assert!(r.width() >= w.metrics.min_w - 0.5);
            assert!(r.height() >= w.metrics.min_h - 0.5);
        }

        // Phase `attach_button_requests_the_root_portal_boundary`.
        {
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

        // Phase `click_outside_window_is_consumed_not_passed_through`.
        {
            let mut w = win();
            w.last_mouse = Point::new(5.0, 5.0); // outside the centred window
            let modifiers = winit::event::Modifiers::default();
            let mut doc = crate::resource::TestMail::doc();
            let taffy: taffy::TaffyTree<()> = taffy::TaffyTree::new();
            let press = ui::synthetic_input::mouse_press();
            let (consumed, action) = w.handle_event(
                &press,
                &mut ComposeEventContext {
                    scale_factor: 1.0,
                    modifiers,
                    state: ui::widget::Edits::lend(&mut doc),
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

    #[test]
    fn compact_compose_is_a_full_window_page_and_keeps_close_and_send_reachable() {
        for theme in [Theme::dark(), Theme::light()] {
            for bounds in [
                Rect::new(0.0, 0.0, 360.0, 800.0),
                Rect::new(0.0, 0.0, 800.0, 360.0),
            ] {
                let mut window = win();
                window.theme = theme.clone();
                window.layout(bounds);
                let mut scene = Scene::new();
                let mut text = TextRenderer::new();
                window.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    &PresenceCtx::empty(),
                );
                assert_eq!(window.window_rect(), bounds);
                assert!(window.body_rect().height() > 0.0);
                for target in [ComposeHit::Close, ComposeHit::Send, ComposeHit::Discard] {
                    assert!(
                        bounds.contains_rect(hit_rect(&window, target)),
                        "{target:?} remains reachable"
                    );
                }
                assert!(window.resize_edge_at(bounds.center()).is_none());
                assert!(
                    window
                        .resize_edge_at(Point::new(bounds.x1 - 1.0, bounds.y1 - 1.0))
                        .is_none()
                );
                let close = hit_rect(&window, ComposeHit::Close).center();
                let (handled, action) = press_at(&mut window, close);
                assert!(handled);
                assert!(matches!(action, ComposeAction::Close));
                assert!(!window.open);
            }
        }
    }

    /// Beside the list the composer takes the message column, and the list
    /// keeps its clicks and keys.
    #[test]
    fn in_the_message_column_the_list_keeps_its_clicks_and_keys() {
        let mut w = win();
        assert_eq!(w.dock(), DockState::Pane);
        let column = Rect::new(620.0, 56.0, 1280.0, 800.0);
        w.set_pane(Some(column));
        w.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        assert!(w.in_pane());
        assert!(!w.is_modal(), "the list beside it stays usable");
        assert_eq!(w.window_rect(), column, "it takes the message column");
        assert!(
            w.resize_edge_at(Point::new(column.x0 + 1.0, column.y0 + 100.0))
                .is_none(),
            "a column does not resize"
        );
        let (consumed, _) = press_at(&mut w, Point::new(100.0, 300.0));
        assert!(!consumed, "a click on the list goes to the list");
        let modifiers = winit::event::Modifiers::default();
        let mut doc = crate::resource::TestMail::doc();
        let taffy: taffy::TaffyTree<()> = taffy::TaffyTree::new();
        let (consumed, _) = w.handle_event(
            &key(Key::Character("j".into()), Some("j")),
            &mut ComposeEventContext {
                scale_factor: 1.0,
                modifiers,
                state: ui::widget::Edits::lend(&mut doc),
                path: &automerge::ROOT,
                node_id: taffy::NodeId::new(0),
                taffy: &taffy,
            },
        );
        assert!(!consumed, "and the keys with it");
        let subject = hit_rect(&w, ComposeHit::FieldSubject);
        let (consumed, _) = press_at(&mut w, subject.center());
        assert!(consumed, "a click back into the message takes them again");
    }

    fn press_at(w: &mut ComposeWindow, p: Point) -> (bool, ComposeAction) {
        let m = winit::event::Modifiers::default();
        let mut doc = crate::resource::TestMail::doc();
        let taffy: taffy::TaffyTree<()> = taffy::TaffyTree::new();
        let node = taffy::NodeId::new(0);
        let mv = ui::synthetic_input::cursor_moved(p.x, p.y);
        w.handle_event(
            &mv,
            &mut ComposeEventContext {
                scale_factor: 1.0,
                modifiers: m,
                state: ui::widget::Edits::lend(&mut doc),
                path: &automerge::ROOT,
                node_id: node,
                taffy: &taffy,
            },
        );
        let pr = ui::synthetic_input::mouse_press();
        w.handle_event(
            &pr,
            &mut ComposeEventContext {
                scale_factor: 1.0,
                modifiers: m,
                state: ui::widget::Edits::lend(&mut doc),
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
}
