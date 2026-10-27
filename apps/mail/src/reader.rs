//! The open conversation: its subject, then each message with its sender,
//! date, recipients, body and attachments, newest last.
//!
//! A message is a document, so the reader paints it, reading every value from
//! the theme; the controls inside it (attachment chips, the remote-content
//! banner) are the toolkit's faces. An HTML body is a sealed document in an
//! isolated engine (`html`); a plain one is text whose addresses are links.

mod html;

use std::collections::{HashMap, HashSet};

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
#[cfg(test)]
use ui::webview::{SharedEngine, WebViewCommand};
use ui::widget::{
    AvatarFace, AvatarSize, Banner, BannerContent, BannerKind, Button, ButtonFace, ChipFace,
    ChipLead, ControlSize, EmptyState, EmptyStateContent, FindMatch, FindOptions, Findable,
    ScrollRegion, TaffyTreeExt, TextInput, Widget,
};
use ui::{TextKind, TextRole};

use crate::model::{Message, MessageId};
use crate::state::{Intent, MailState, ReaderChoice, Shared};
use crate::util::{format_absolute, local_minutes};

pub(crate) use html::is_openable;

/// How the reader sets a message's text: the theme's body size, which
/// headings are scaled from, and the quieter ink of quoted lines.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BodyStyle {
    font_px: f32,
    quiet: [u8; 3],
}

impl BodyStyle {
    fn from_theme(theme: &Theme) -> Self {
        Self {
            // The theme's body type already carries the text size and the
            // UI scale: applying the text size again would grow it twice.
            font_px: theme.text(TextRole::Body).size,
            quiet: ui::color::to_rgb8(theme.tokens.colors.text_dim),
        }
    }
}

/// Whether `theme` is a dark one: its reading ground is dark.
fn is_dark(theme: &Theme) -> bool {
    ui::color::relative_luminance(theme.tokens.colors.surfaces.content) < 0.4
}

/// The open message's text: a read-only field, so it can be selected and
/// copied and its addresses open.
struct BodyText {
    /// The message, whether its HTML part is shown, and whether the quote
    /// it ends with is shown.
    key: (MessageId, bool, bool),
    /// Whether the message ends with a quote that is folded away.
    folded: bool,
    /// The style the runs were set in.
    style: BodyStyle,
    input: TextInput,
}

impl BodyText {
    fn new(
        key: (MessageId, bool, bool),
        style: BodyStyle,
        message: &Message,
        html: Option<&str>,
    ) -> Self {
        let show_quote = key.2;
        let (runs, links, folded) = if let Some(html) = html {
            html_runs(html, style.font_px, style.quiet, show_quote)
        } else {
            let (shown, quote) = split_trailing_quote(&message.body);
            let folded = quote && !show_quote;
            let text = if folded { shown } else { message.body.as_str() };
            let (runs, links) = plain_runs(text, style.quiet);
            (runs, links, folded)
        };
        let input = TextInput::local()
            .multiline(true)
            .with_plain_chrome()
            .with_text_role(TextRole::Body)
            .with_spellcheck(false)
            .with_autocorrect(false)
            .with_id("mail_body")
            .with_accessible_name(message.subject.clone())
            .with_runs(runs)
            .with_links(links)
            .with_read_only(true);
        Self {
            key,
            folded,
            style,
            input,
        }
    }
}

/// `body` without the quote it ends with: the replied-to message under
/// its "On …, X wrote:" line. The second value says whether there was one.
/// A message that is all quote keeps it.
fn split_trailing_quote(body: &str) -> (&str, bool) {
    let lines: Vec<&str> = body.split('\n').collect();
    let line = |index: usize| lines.get(index).copied().unwrap_or_default();
    let mut start = lines.len();
    while start > 0 && line(start - 1).trim().is_empty() {
        start -= 1;
    }
    let end = start;
    while start > 0 && line(start - 1).trim_start().starts_with('>') {
        start -= 1;
    }
    if start == end {
        return (body, false);
    }
    if start > 0
        && line(start - 1)
            .trim_end()
            .to_ascii_lowercase()
            .ends_with("wrote:")
    {
        start -= 1;
    }
    let shown: usize = lines.iter().take(start).map(|kept| kept.len() + 1).sum();
    let shown = body.get(..shown.min(body.len())).unwrap_or(body).trim_end();
    if shown.trim().is_empty() {
        return (body, false);
    }
    (shown, true)
}

/// A plain-text body as runs, quoted lines quieter, and where the
/// addresses it names go, in byte ranges of the runs' text.
fn plain_runs(
    body: &str,
    quiet: [u8; 3],
) -> (Vec<ui::RichSpan>, Vec<(std::ops::Range<usize>, String)>) {
    let mut runs: Vec<ui::RichSpan> = Vec::new();
    let mut text_so_far = String::with_capacity(body.len());
    for (index, line) in body.split('\n').enumerate() {
        let mut text = String::with_capacity(line.len() + 1);
        if index > 0 {
            text.push('\n');
        }
        text.push_str(line.trim_end_matches('\r'));
        let quoted = line.trim_start().starts_with('>');
        text_so_far.push_str(&text);
        runs.push(ui::RichSpan::new(text).color(quoted.then_some(quiet)));
    }
    let links = crate::html_mail::text_links(&text_so_far);
    (runs, links)
}

/// An HTML body as runs, and where its links go, in byte ranges of the
/// runs' text. `body_px` is the size a heading is scaled from.
/// The runs of an HTML body, where its links go, and whether a quote it ends
/// with was folded away (`show_quote` keeps it).
fn html_runs(html: &str, body_px: f32, quiet: [u8; 3], show_quote: bool) -> HtmlRuns {
    let mut runs = Vec::new();
    let mut links = Vec::new();
    let mut at = 0;
    let mut read = crate::html_text::render(html);
    // The quoted runs at the end, and the blank ones among them.
    let quote_from = read
        .iter()
        .rposition(|run| !run.quote && !run.text.trim().is_empty())
        .map_or(0, |last| last + 1);
    let folded = !show_quote
        && quote_from > 0
        && read
            .get(quote_from..)
            .is_some_and(|tail| tail.iter().any(|run| run.quote));
    if folded {
        read.truncate(quote_from);
    }
    for run in read {
        let end = at + run.text.len();
        if let Some(target) = &run.link {
            links.push((at..end, target.clone()));
        }
        let mut span = ui::RichSpan::new(run.text)
            .bold(run.bold)
            .italic(run.italic)
            .color(run.quote.then_some(quiet));
        span.size = match run.heading {
            1 => Some(body_px * 1.5),
            2 => Some(body_px * 1.25),
            3 => Some(body_px * 1.1),
            _ => None,
        };
        runs.push(span);
        at = end;
    }
    (runs, links, folded)
}

/// What [`html_runs`] makes of a body.
type HtmlRuns = (
    Vec<ui::RichSpan>,
    Vec<(std::ops::Range<usize>, String)>,
    bool,
);

pub(crate) fn attachment_label(attachment: &crate::model::Attachment) -> String {
    format!("{} ({})", attachment.filename, attachment.size_human())
}

fn recipients_line(message: &Message) -> String {
    let names: Vec<String> = message
        .to
        .iter()
        .chain(message.cc.iter())
        .map(crate::model::Addr::display)
        .collect();
    if names.is_empty() {
        String::new()
    } else {
        format!("To {}", names.join(", "))
    }
}

/// Paint state shared by the reader's painters.
struct Flow<'a, 'b> {
    ctx: PaintCtx<'b>,
    state: &'a MailState,
    x: f64,
    width: f64,
    y: f64,
    pointer: Point,
    attachments: Vec<(Rect, MessageId, usize)>,
    /// The save buttons beside them.
    saves: Vec<(Rect, MessageId, usize)>,
}

impl Flow<'_, '_> {
    fn style(&self, role: TextRole) -> TextStyle {
        self.ctx.theme.text(role)
    }

    fn line(&mut self, role: TextRole) -> f64 {
        self.ctx.line_box(role)
    }

    /// Paint `text` wrapped to the column and move below it.
    fn paragraph(&mut self, text: &str, style: &TextStyle, dim: bool) {
        let theme = self.ctx.theme;
        let ink = if dim {
            theme.text_dim
        } else {
            theme.text_primary
        };
        self.y += self.wrapped_at(text, style, ink, Point::new(self.x, self.y), self.width);
    }

    /// `text` wrapped to `width` at `at`; returns how tall it is.
    fn wrapped_at(
        &mut self,
        text: &str,
        style: &TextStyle,
        ink: vello::peniko::Color,
        at: Point,
        width: f64,
    ) -> f64 {
        let width = ui::num::f32_of_f64(width.max(1.0));
        let (_, height) = self.ctx.text.measure_style(text, Some(width), style);
        self.ctx.text(text, style, ink, at, Some(width));
        f64::from(height)
    }

    /// One line elided into `width` at `x`, top at `y`.
    fn line_at(
        &mut self,
        text: &str,
        style: &TextStyle,
        ink: vello::peniko::Color,
        at: Point,
        width: f64,
    ) {
        let elided =
            self.ctx
                .text
                .elide_to_width_style(text, ui::num::f32_of_f64(width.max(0.0)), style);
        self.ctx.text(&elided, style, ink, at, None);
    }

    /// The sender block of one message: avatar, name, address, date and
    /// recipients. Moves below it.
    fn sender(&mut self, message: &Message) {
        let theme = self.ctx.theme;
        let spacing = &theme.tokens.spacing;
        let side = AvatarSize::Regular.side(theme);
        let name_style = self.style(TextRole::Body).strong();
        let caption = self.style(TextRole::Caption);
        let address_style = caption.clone().with_kind(TextKind::Identifier);
        let date_style = caption.clone().numeric();
        let name_h = self.line(TextRole::Body);
        let caption_h = self.line(TextRole::Caption);

        let name = message.from.display();
        let image = self.state.avatars.image(&message.from.email);
        AvatarFace::new(&name, &message.from.email)
            .image(image.as_ref())
            .paint(
                &mut self.ctx,
                Rect::new(self.x, self.y, self.x + side, self.y + side),
            );

        let text_x = self.x + side + f64::from(spacing.s3);
        let right = self.x + self.width;
        let full_w = (right - text_x).max(1.0);
        let date = format_absolute(local_minutes(message.timestamp_min));
        let (date_w, _) = self.ctx.text.measure_style(&date, None, &date_style);
        let (name_w, _) = self.ctx.text.measure_style(&name, None, &name_style);
        let gap = f64::from(spacing.s3);
        // The date sits on the name's line box while both fit; otherwise it
        // takes a line of its own rather than the sender's name.
        let date_inline = f64::from(name_w + date_w) + gap <= full_w;
        let mut y = self.y;
        self.line_at(
            &name,
            &name_style,
            theme.text_primary,
            Point::new(text_x, y),
            if date_inline {
                full_w - f64::from(date_w) - gap
            } else {
                full_w
            },
        );
        if date_inline {
            self.ctx.text(
                &date,
                &date_style,
                theme.text_dim,
                Point::new(right - f64::from(date_w), y + (name_h - caption_h) / 2.0),
                None,
            );
        }
        y += name_h;
        if !date_inline {
            y += self.wrapped_at(
                &date,
                &date_style,
                theme.text_dim,
                Point::new(text_x, y),
                full_w,
            );
        }
        self.line_at(
            &message.from.email,
            &address_style,
            theme.text_dim,
            Point::new(text_x, y),
            full_w,
        );
        y += caption_h;
        // Every recipient shows: the line wraps rather than eliding names.
        let to = recipients_line(message);
        if !to.is_empty() {
            y += self.wrapped_at(&to, &caption, theme.text_dim, Point::new(text_x, y), full_w);
        }
        self.y = y.max(self.y + side);
    }

    /// The message's attachments as chips that wrap; each one saves its file.
    fn attachments(&mut self, message: &Message) {
        let theme = self.ctx.theme;
        let gap = f64::from(theme.tokens.spacing.s2);
        let h = ChipFace::height(theme);
        let mut x = self.x;
        let right = self.x + self.width;
        for (index, attachment) in message.shown_attachments() {
            let label = attachment_label(attachment);
            let lead = ChipLead::Glyph(attachment.icon());
            let w = ChipFace::intrinsic_width(&label, lead, false, self.ctx.text, theme)
                .min(self.width);
            if x > self.x && x + w > right {
                x = self.x;
                self.y += h + gap;
            }
            let rect = Rect::new(x, self.y, x + w, self.y + h);
            ChipFace::new(rect, &label)
                .lead(lead)
                .hovered(rect.contains(self.pointer))
                .paint(self.ctx.scene, self.ctx.transform, self.ctx.text, theme);
            self.attachments.push((rect, message.id, index));
            x += w + gap;
            // Beside it, saving it as a file somewhere of the person's
            // choosing; the chip itself opens it.
            if attachment.is_downloadable() {
                let save = Rect::new(x, self.y, x + h, self.y + h);
                Button::paint(
                    self.ctx.scene,
                    self.ctx.transform,
                    self.ctx.text,
                    theme,
                    &ButtonFace::icon_only(save, Icon::Download, "Save")
                        .control_size(ControlSize::Small)
                        .hovered(save.contains(self.pointer)),
                );
                self.saves.push((save, message.id, index));
                x += h + gap;
            }
        }
        self.y += h;
    }
}

/// The remote-content banner's actions, as last painted.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct RemoteButtons {
    /// Load this message's remote images.
    load: Option<Rect>,
    always: Option<Rect>,
    /// Stop loading them for its sender.
    stop: Option<Rect>,
}

impl RemoteButtons {
    fn at(&self, point: Point) -> Option<RemoteChoice> {
        let hit = |rect: Option<Rect>| rect.is_some_and(|rect| rect.contains(point));
        if hit(self.load) {
            Some(RemoteChoice::Load)
        } else if hit(self.always) {
            Some(RemoteChoice::Always)
        } else if hit(self.stop) {
            Some(RemoteChoice::Stop)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteChoice {
    Load,
    Always,
    Stop,
}

/// What the open HTML message asks of its remote content: how much it
/// names, and whether the person allowed it for the message or its sender.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RemoteState {
    count: usize,
    message: bool,
    sender: bool,
}

/// A match of find in a message's text: the message and the byte range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TextMatch {
    message: MessageId,
    start: usize,
    end: usize,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent view flags of one reader, each toggled on its own event"
)]
pub struct MessageReader {
    shared: Shared,
    rect: Rect,
    /// The conversation's viewport: wheel, keyboard, bar and accessible
    /// scrolling.
    region: ScrollRegion,
    /// The theme the reader was last drawn with, for the region's lanes.
    theme: Theme,
    pointer: Point,
    /// Attachment chips `(rect, message, index)` as last painted.
    attachments: Vec<(Rect, MessageId, usize)>,
    /// The save button beside each attachment chip, as last painted.
    attachment_saves: Vec<(Rect, MessageId, usize)>,
    remote_buttons: RemoteButtons,
    /// The conversation the scroll offset belongs to.
    shown: Option<MessageId>,
    html: html::HtmlBody,
    /// The message the HTML body shows.
    html_message: Option<MessageId>,
    /// How many messages the mailbox held when the reader last looked for
    /// HTML mail to start its engine for.
    warm_checked: Option<usize>,
    /// The reader's furthest scroll, as last painted: past it, the wheel
    /// scrolls an HTML body taller than the reader.
    scroll_max: f64,
    /// The text of each message shown open, as last painted: the newest
    /// always, and the earlier ones the person opened.
    bodies: HashMap<MessageId, BodyText>,
    /// The message whose text has the pointer's selection and the keys.
    active: Option<MessageId>,
    /// Earlier messages of the conversation shown open. The newest is
    /// always open; an earlier one opens when it is unread or clicked.
    expanded: HashSet<MessageId>,
    /// The header of each earlier message as last painted: a click opens
    /// or closes that message.
    headers: Vec<(Rect, MessageId)>,
    /// Messages whose closing quote the person unfolded.
    quotes_open: HashSet<MessageId>,
    /// The "show the quote" button under each folded message, as painted.
    quote_buttons: Vec<(Rect, MessageId)>,
    /// What find matched in the messages' text, in reading order, and the
    /// current one.
    text_matches: Vec<TextMatch>,
    text_current: Option<usize>,
    /// The query find runs in the HTML body.
    html_query: Option<String>,
    focused: bool,
    /// A button is held over the text: the pointer is selecting.
    selecting: bool,
    dirty: bool,
}

impl MessageReader {
    pub fn new(shared: Shared) -> Self {
        Self {
            shared,
            rect: Rect::ZERO,
            region: ScrollRegion::vertical(),
            theme: Theme::dark(),
            pointer: Point::new(-1.0, -1.0),
            attachments: Vec::new(),
            attachment_saves: Vec::new(),
            remote_buttons: RemoteButtons::default(),
            shown: None,
            html: html::HtmlBody::new(),
            html_message: None,
            warm_checked: None,
            scroll_max: 0.0,
            bodies: HashMap::new(),
            active: None,
            expanded: HashSet::new(),
            headers: Vec::new(),
            quotes_open: HashSet::new(),
            quote_buttons: Vec::new(),
            text_matches: Vec::new(),
            text_current: None,
            html_query: None,
            focused: false,
            selecting: false,
            dirty: true,
        }
    }

    /// Start the HTML engine once there is HTML mail to show, before the
    /// person opens one: opening a message never waits for a process.
    fn warm(&mut self, state: &MailState) {
        if self.html.started() {
            return;
        }
        let count = state.mailbox.messages.len();
        if self.warm_checked == Some(count) {
            return;
        }
        self.warm_checked = Some(count);
        if state
            .mailbox
            .messages
            .values()
            .any(|message| message.html_body.is_some())
        {
            self.html.warm();
        }
    }

    /// Inject the HTML-body engine (tests observe the commands the reader
    /// sends). Call before the first HTML mail is drawn.
    #[cfg(test)]
    pub(crate) fn set_html_engine(&mut self, engine: SharedEngine) {
        self.html.set_engine(engine);
    }

    /// How the HTML loaded into the engine is set, if any.
    #[cfg(test)]
    pub(crate) fn html_loaded(&self) -> Option<html::Setting> {
        self.html.loaded()
    }

    /// The attachment chips as last painted, for tests.
    #[cfg(test)]
    pub(crate) fn attachment_rects(&self) -> &[(Rect, MessageId, usize)] {
        &self.attachments
    }

    /// The "Load images" button as last painted, for tests.
    #[cfg(test)]
    pub(crate) fn remote_button(&self) -> Option<Rect> {
        self.remote_buttons.load
    }

    fn paint_empty(&self, ctx: &mut PaintCtx<'_>, state: &MailState) {
        // Mail is still arriving: nothing to say about a selection yet.
        if state.first_fetch.is_some() && state.mailbox.messages.is_empty() {
            return;
        }
        let count = state.selected_bulk_ids().len();
        let title = if state.bulk_selecting {
            match count {
                0 => "No messages selected".to_owned(),
                1 => "1 message selected".to_owned(),
                n => format!("{n} messages selected"),
            }
        } else {
            "No message selected".to_owned()
        };
        EmptyState::paint(
            ctx,
            self.rect,
            &EmptyStateContent {
                icon: Some(Icon::Mail),
                ..EmptyStateContent::new(&title)
            },
            Point::new(-1.0, -1.0),
        );
    }

    /// Paint the remote-content banner for an HTML body that names remote
    /// content, at the flow's cursor: what is blocked and the buttons that
    /// load it for this message or always for its sender.
    fn paint_remote_line(&mut self, flow: &mut Flow<'_, '_>, remote: RemoteState) {
        let theme = flow.ctx.theme;
        let t = crate::i18n::t;
        let (icon, title, action, secondary) = if remote.sender {
            (
                Icon::Image,
                t("mail-remote-sender-loads"),
                Some(t("mail-remote-stop")),
                None,
            )
        } else if remote.message {
            (
                Icon::Image,
                t("mail-remote-loaded"),
                Some(t("mail-remote-always")),
                None,
            )
        } else {
            (
                Icon::Shield,
                t("mail-remote-blocked"),
                Some(t("mail-remote-load")),
                Some(t("mail-remote-always")),
            )
        };
        let mut content = BannerContent::new(BannerKind::Neutral, &title).with_icon(icon);
        if let Some(label) = &action {
            content = content.action(label);
        }
        if let Some(label) = &secondary {
            content = content.secondary_action(label);
        }
        let height = Banner::height(&content, flow.width, flow.ctx.text, theme);
        let rect = Rect::new(flow.x, flow.y, flow.x + flow.width, flow.y + height);
        let hit = Banner::paint(&mut flow.ctx, rect, &content, flow.pointer);
        self.remote_buttons = if remote.sender {
            RemoteButtons {
                stop: hit.action,
                ..RemoteButtons::default()
            }
        } else if remote.message {
            RemoteButtons {
                always: hit.action,
                ..RemoteButtons::default()
            }
        } else {
            RemoteButtons {
                load: hit.action,
                always: hit.secondary_action,
                stop: None,
            }
        };
        flow.y += height;
    }

    /// The message text as a field the person can select and copy, its
    /// addresses links: an HTML body read into styled runs, a plain one
    /// with its quoted lines quieter. Moves below it.
    fn paint_body(
        &mut self,
        flow: &mut Flow<'_, '_>,
        message: &Message,
        html: Option<&str>,
        presence: &PresenceCtx,
    ) {
        let theme = flow.ctx.theme;
        let style = BodyStyle::from_theme(theme);
        let key = (
            message.id,
            html.is_some(),
            self.quotes_open.contains(&message.id),
        );
        let needs_rebuild = self
            .bodies
            .get(&message.id)
            .is_none_or(|body| body.key != key || body.style != style);
        if needs_rebuild {
            let mut body = BodyText::new(key, style, message, html);
            body.input
                .set_focus(self.focused && self.active == Some(message.id));
            self.bodies.insert(message.id, body);
        }
        let Some(body) = self.bodies.get_mut(&message.id) else {
            return;
        };
        let inset = body.input.text_inset_x(theme);
        let rect = Rect::new(flow.x - inset, flow.y, flow.x + flow.width + inset, flow.y);
        let height = body.input.place_wrapped(rect, flow.ctx.text, theme);
        body.input.draw(
            flow.ctx.scene,
            flow.ctx.transform,
            flow.ctx.text,
            theme,
            presence,
        );
        for (index, found) in self.text_matches.iter().enumerate() {
            if found.message != message.id {
                continue;
            }
            for span in body.input.selection_rects(theme, found.start, found.end) {
                ui::widget::paint_find_match(
                    flow.ctx.scene,
                    flow.ctx.transform,
                    span,
                    self.text_current == Some(index),
                    theme,
                );
            }
        }
        flow.y += height;
        if body.folded {
            // The quote it ends with waits behind a quiet button.
            flow.y += f64::from(theme.tokens.spacing.s2);
            let side = ButtonFace::height(ControlSize::Small, theme);
            let label = crate::i18n::t("mail-show-quoted");
            let width = ButtonFace::new(Rect::ZERO, &label)
                .control_size(ControlSize::Small)
                .intrinsic_width(flow.ctx.text, theme);
            let button = Rect::new(flow.x, flow.y, flow.x + width, flow.y + side);
            Button::paint(
                flow.ctx.scene,
                flow.ctx.transform,
                flow.ctx.text,
                theme,
                &ButtonFace::new(button, &label)
                    .control_size(ControlSize::Small)
                    .hovered(button.contains(flow.pointer)),
            );
            self.quote_buttons.push((button, message.id));
            flow.y += side;
        }
    }

    /// Prepare and load the open message's HTML for the engine, as the
    /// person set it: its remote content or not, dark or light. Returns how
    /// it asks for remote content, or `None` when it is not shown in the
    /// engine (no isolated renderer, the person reads its text, or it is
    /// too large for one).
    fn load_html(
        &mut self,
        state: &MailState,
        head: &Message,
        theme: &Theme,
    ) -> Option<RemoteState> {
        let html = head.html_body.as_deref()?;
        if !self.html.live() || state.plain_text.contains(&head.id) {
            return None;
        }
        let setting = html::Setting {
            message: head.id,
            remote: state.remote_allowed(head),
            dark: is_dark(theme),
        };
        if self.html.prepared(setting).is_none() {
            let inline = state.inline_parts(head);
            self.html.prepare(setting, html, theme, &inline);
        }
        if !self.html.load(setting) {
            return None;
        }
        if self.html_message != Some(head.id) {
            self.html_message = Some(head.id);
            self.html_query = None;
        }
        Some(RemoteState {
            count: self
                .html
                .prepared(setting)
                .map_or(0, |prepared| prepared.remote),
            message: state.remote_messages.contains(&head.id),
            sender: state
                .remote_senders
                .contains(&head.from.email.to_ascii_lowercase()),
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the conversation is one reading flow: heading, messages and the HTML body share the running cursor"
    )]
    fn paint_thread(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let shared = self.shared.clone();
        let state = shared.borrow();
        self.warm(&state);
        // What this frame paints is all that can be pressed.
        self.remote_buttons = RemoteButtons::default();
        self.quote_buttons.clear();
        let messages = state.selected_thread_messages();
        let Some(head) = messages.last().copied() else {
            self.attachments.clear();
            self.attachment_saves.clear();
            self.html.unload();
            self.html_message = None;
            self.bodies.clear();
            self.headers.clear();
            self.active = None;
            self.shown = None;
            self.paint_empty(&mut PaintCtx::new(scene, t, tr, theme), &state);
            return;
        };
        let head_id = head.id;
        if self.shown != Some(head_id) {
            self.shown = Some(head_id);
            self.region.set_offset(Vec2::ZERO);
            // A conversation opens on its newest message, with the earlier
            // ones not read yet open above it.
            self.expanded = messages
                .iter()
                .filter(|message| !message.read && message.id != head_id)
                .map(|message| message.id)
                .collect();
            self.active = Some(head_id);
            self.quotes_open.clear();
            self.text_matches.clear();
            self.text_current = None;
            self.bodies
                .retain(|id, _| messages.iter().any(|message| message.id == *id));
        }
        let remote = self.load_html(&state, head, theme);
        let html_shown = remote.is_some();
        if !html_shown {
            // Plain text this frame: drop a stale load so returning to an
            // HTML mail loads it again, blocked again.
            self.html.unload();
            self.html_message = None;
        }
        let spacing = theme.tokens.spacing;
        let gutter = f64::from(spacing.s6);
        let r = self.rect;
        // An HTML body whose height is unknown takes the rest of the
        // reader, so the thread above it does not scroll.
        let measured = self.html.measures();
        if html_shown && !measured {
            self.region.set_offset(Vec2::ZERO);
        }
        let scroll = self.region.offset().y;
        let ctx = PaintCtx::new(scene, t, tr, theme);
        ctx.scene
            .push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &r);
        let mut flow = Flow {
            ctx,
            state: &state,
            x: r.x0 + gutter,
            width: (r.width() - 2.0 * gutter).max(1.0),
            y: r.y0 + gutter - scroll,
            pointer: self.pointer,
            attachments: Vec::new(),
            saves: Vec::new(),
        };

        // The conversation's subject, and how many messages it holds.
        let title = flow.style(TextRole::Title);
        let subject = head.thread_subject();
        let subject = if subject.trim().is_empty() {
            "No subject".to_owned()
        } else {
            subject
        };
        flow.paragraph(&subject, &title, false);
        if state.inspection == Some(head.id) {
            flow.y += f64::from(spacing.s2);
            let caption = flow.style(TextRole::Caption);
            let icon = f64::from(theme.tokens.metrics.icon_sm);
            let line = flow.line(TextRole::Caption);
            let top = flow.y;
            Icon::Eye.draw(
                flow.ctx.scene,
                t,
                Rect::from_center_size(
                    Point::new(flow.x + icon / 2.0, top + line / 2.0),
                    (icon, icon),
                ),
                theme.text_dim,
            );
            let text = if head.read {
                "Preview"
            } else {
                "Preview, still unread"
            };
            let x = flow.x + icon + f64::from(theme.tokens.metrics.icon_label_gap);
            let width = flow.width - (x - flow.x);
            flow.line_at(text, &caption, theme.text_dim, Point::new(x, top), width);
            flow.y += line;
        }
        flow.y += f64::from(spacing.s6);

        let mut webview_rect = None;
        // The HTML body fills the reader and scrolls inside its own view.
        let mut html_fills = false;
        let count = messages.len();
        self.headers.clear();
        for (index, message) in messages.iter().enumerate() {
            let is_head = index + 1 == count;
            let open = is_head || self.expanded.contains(&message.id);
            let top = flow.y;
            flow.sender(message);
            let header_bottom = flow.y;
            flow.y += f64::from(spacing.s3);
            if is_head && let Some(remote) = remote {
                if remote.count > 0 {
                    self.paint_remote_line(&mut flow, remote);
                    flow.y += f64::from(spacing.s3);
                }
                if measured {
                    // As tall as its document: the reader scrolls the
                    // conversation and the mail as one page.
                    let width = flow.width;
                    let (laid, kept) = self.html.layout(width, f64::from(spacing.s1));
                    webview_rect = Some(Rect::new(flow.x, flow.y, flow.x + width, flow.y + laid));
                    flow.y += kept;
                } else {
                    let bottom = r.y1 - gutter;
                    if bottom > flow.y + f64::from(spacing.s12) {
                        webview_rect = Some(Rect::new(flow.x, flow.y, flow.x + flow.width, bottom));
                    }
                    flow.y = bottom;
                    html_fills = true;
                }
            } else if open {
                // Without an isolated renderer the HTML is read into styled
                // text; nothing in it is fetched. An earlier message opened
                // in the conversation reads the same way, and so does one
                // the person reads as its plain text.
                let html = message
                    .html_body
                    .as_deref()
                    .filter(|_| !state.plain_text.contains(&message.id));
                self.paint_body(&mut flow, message, html, presence);
                if !is_head {
                    self.headers.push((
                        Rect::new(flow.x, top, flow.x + flow.width, header_bottom),
                        message.id,
                    ));
                }
            } else {
                // A closed earlier message shows its first line; the whole of
                // it opens the message.
                let caption = flow.style(TextRole::Caption);
                let at = Point::new(flow.x, flow.y);
                let width = flow.width;
                flow.line_at(&message.snippet(), &caption, theme.text_dim, at, width);
                flow.y += flow.line(TextRole::Caption);
                self.headers.push((
                    Rect::new(flow.x, top, flow.x + flow.width, flow.y),
                    message.id,
                ));
            }
            if open && message.has_attachments() {
                flow.y += f64::from(spacing.s4);
                flow.attachments(message);
            }
            if !is_head {
                flow.y += f64::from(spacing.s4);
                let ground = theme.tokens.colors.surfaces.content;
                ui::surface::separator(
                    flow.ctx.scene,
                    t,
                    flow.x,
                    flow.x + flow.width,
                    flow.y,
                    0.0,
                    ground,
                );
                flow.y += f64::from(spacing.s4);
            }
        }
        flow.y += gutter;
        let content_h = if html_fills && webview_rect.is_some() {
            r.height()
        } else {
            flow.y + scroll - r.y0
        };
        self.scroll_max = (content_h - r.height()).max(0.0);
        let attachments = std::mem::take(&mut flow.attachments);
        let saves = std::mem::take(&mut flow.saves);
        flow.ctx.scene.pop_layer();
        self.attachments = attachments;
        self.attachment_saves = saves;
        self.region.set_content_size(r.width(), content_h);
        self.region.paint_bars(flow.ctx.scene, t, theme);
        drop(flow);
        drop(state);

        // The HTML body, now that the shared borrow is released.
        if let Some(rect) = webview_rect {
            self.html.place(rect);
            self.html.draw(scene, t, tr, theme, presence, r);
        }
        self.drain_html();
    }

    /// Act on what the HTML body's engine said: a followed link opens in
    /// the Browser, a new size lays the reader out again.
    fn drain_html(&mut self) {
        let said = self.html.drain();
        if said.dirty {
            self.dirty = true;
        }
        if let (Some((top, bottom)), Some(rect)) = (said.found_at, self.html.rect()) {
            self.bring_into_view(rect.y0 + top, rect.y0 + bottom);
        }
        if !said.open.is_empty() {
            let mut state = self.shared.borrow_mut();
            for url in said.open {
                state.push_intent(Intent::OpenLink(url));
            }
        }
    }

    fn press(&mut self, point: Point) -> bool {
        if let Some(choice) = self.remote_buttons.at(point)
            && let Some(id) = self.html_message
        {
            let mut state = self.shared.borrow_mut();
            match choice {
                RemoteChoice::Load => state.load_remote(id),
                RemoteChoice::Always => state.always_load_remote(id),
                RemoteChoice::Stop => state.stop_remote(id),
            }
            state.dirty = true;
            drop(state);
            self.dirty = true;
            return true;
        }
        if let Some(id) = self
            .quote_buttons
            .iter()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, id)| *id)
        {
            if !self.quotes_open.remove(&id) {
                self.quotes_open.insert(id);
            }
            self.dirty = true;
            self.shared.borrow_mut().dirty = true;
            return true;
        }
        if let Some(id) = self
            .headers
            .iter()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, id)| *id)
        {
            if !self.expanded.remove(&id) {
                self.expanded.insert(id);
            }
            if self.active == Some(id) {
                self.active = self.shown;
            }
            self.dirty = true;
            self.shared.borrow_mut().dirty = true;
            return true;
        }
        if let Some((_, message, index)) = self
            .attachment_saves
            .iter()
            .find(|(rect, _, _)| rect.contains(point))
            .copied()
        {
            self.shared
                .borrow_mut()
                .push_intent(Intent::SaveAttachment(message, index));
            return true;
        }
        if let Some((_, message, index)) = self
            .attachments
            .iter()
            .find(|(rect, _, _)| rect.contains(point))
            .copied()
        {
            self.shared
                .borrow_mut()
                .push_intent(Intent::OpenAttachment(message, index));
            return true;
        }
        false
    }

    /// The reader's menu for the open message at `point`: the link under
    /// the pointer (its real address, to open or copy), copying and
    /// selecting the text, and reading the message as plain text or as
    /// it was formatted.
    fn open_menu(&self, point: Point) -> bool {
        let Some(head) = self.shown else {
            return false;
        };
        let over_html = self.html.rect().is_some_and(|rect| rect.contains(point));
        let over_text = self
            .bodies
            .values()
            .any(|body| body.input.get_rect().contains(point));
        if !over_html && !over_text {
            return false;
        }
        let mut choices = Vec::new();
        if over_html {
            if let Some(link) = self.html.hovered().filter(|link| is_openable(link)) {
                choices.push(ReaderChoice::OpenLink(link.to_owned()));
                choices.push(ReaderChoice::CopyLink(link.to_owned()));
            }
            choices.push(ReaderChoice::Copy);
            choices.push(ReaderChoice::SelectAll);
        }
        let state = self.shared.borrow();
        if state
            .mailbox
            .get(head)
            .is_some_and(|message| message.html_body.is_some())
        {
            choices.push(if state.plain_text.contains(&head) {
                ReaderChoice::ShowFormatted
            } else {
                ReaderChoice::ShowPlainText
            });
        }
        drop(state);
        if choices.is_empty() {
            return false;
        }
        self.shared
            .borrow_mut()
            .push_intent(Intent::ReaderMenu(head, point, choices));
        true
    }

    /// Carry out what the reader's menu chose for the HTML body.
    fn take_reader_requests(&mut self) {
        let requests = std::mem::take(&mut self.shared.borrow_mut().reader_requests);
        for request in requests {
            match request {
                ReaderChoice::Copy => self.html.copy(),
                ReaderChoice::SelectAll => self.html.select_all(),
                _ => {}
            }
        }
    }

    /// Whether the wheel `event` scrolls the HTML body rather than the
    /// reader: only an engine that cannot say how tall its document is,
    /// whose body fills the reader. A body sized to its document never
    /// scrolls inside; the reader scrolls it with the conversation.
    fn wheel_scrolls_body(&self, event: &WindowEvent) -> bool {
        matches!(event, WindowEvent::MouseWheel { .. }) && !self.html.measures()
    }

    /// Give the message text the pointer and the keys: selecting, copying and
    /// opening its links. The wheel stays the reader's, which scrolls.
    ///
    /// A press on another message's text moves the selection and the keys
    /// there: one message's text is active at a time.
    fn forward_to_body(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if matches!(event, WindowEvent::MouseWheel { .. })
            || (self.html.is_focused() && matches!(event, WindowEvent::KeyboardInput { .. }))
        {
            return;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            ..
        } = event
            && let Some(id) = self
                .bodies
                .iter()
                .find(|(_, body)| body.input.get_rect().contains(self.pointer))
                .map(|(id, _)| *id)
            && self.active != Some(id)
        {
            if let Some(previous) = self.active.and_then(|active| self.bodies.get_mut(&active)) {
                previous.input.set_focus(false);
            }
            self.active = Some(id);
            if let Some(body) = self.bodies.get_mut(&id) {
                body.input.set_focus(self.focused);
            }
        }
        if let Some(body) = self.active.and_then(|id| self.bodies.get_mut(&id)) {
            body.input.handle_event(event, ctx);
            // A pointer passing over the text changes nothing on screen
            // unless it is dragging out a selection.
            match event {
                WindowEvent::MouseInput { state, .. } => {
                    self.selecting = *state == ElementState::Pressed;
                    self.dirty = true;
                }
                WindowEvent::CursorMoved { .. } if !self.selecting => {}
                _ => self.dirty = true,
            }
        }
    }

    /// What the pointer at `point` lights up: an attachment chip, the
    /// remote-content buttons, a quote button.
    fn hover_at(&self, point: Point) -> (Option<usize>, Option<RemoteChoice>, Option<usize>) {
        (
            self.attachments
                .iter()
                .chain(&self.attachment_saves)
                .position(|(rect, _, _)| rect.contains(point)),
            self.remote_buttons.at(point),
            self.quote_buttons
                .iter()
                .position(|(rect, _)| rect.contains(point)),
        )
    }

    /// The text matches of `query` under `options` in the open messages'
    /// text, in reading order.
    fn find_in_text(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
        self.text_matches.clear();
        self.text_current = None;
        if query.is_empty() {
            return Vec::new();
        }
        let order: Vec<MessageId> = self
            .shared
            .borrow()
            .selected_thread_messages()
            .iter()
            .map(|message| message.id)
            .collect();
        let needle = if options.match_case {
            query.to_owned()
        } else {
            query.to_lowercase()
        };
        for id in order {
            let Some(body) = self.bodies.get(&id) else {
                continue;
            };
            let text = body.input.value();
            let haystack = if options.match_case {
                text.to_owned()
            } else {
                text.to_lowercase()
            };
            // Lower-casing keeps byte offsets only where it keeps lengths.
            if haystack.len() != text.len() {
                continue;
            }
            let mut from = 0;
            while let Some(at) = haystack.get(from..).and_then(|rest| rest.find(&needle)) {
                let start = from + at;
                let end = start + needle.len();
                self.text_matches.push(TextMatch {
                    message: id,
                    start,
                    end,
                });
                from = end.max(start + 1);
                if self.text_matches.len() >= 10_000 {
                    break;
                }
            }
        }
        self.dirty = true;
        self.text_matches
            .iter()
            .map(|found| FindMatch::new(found.start, found.end))
            .collect()
    }
}

impl Findable for MessageReader {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
        if self.html.rect().is_some() || self.html.loaded().is_some() {
            self.html.find(query);
            self.html_query = (!query.is_empty()).then(|| query.to_owned());
        }
        self.find_in_text(query, options)
    }

    fn reveal(&mut self, index: usize) {
        let Some(found) = self.text_matches.get(index).copied() else {
            return;
        };
        self.text_current = Some(index);
        let theme = self.theme.clone();
        if let Some(span) = self.bodies.get(&found.message).and_then(|body| {
            body.input
                .selection_rects(&theme, found.start, found.end)
                .into_iter()
                .next()
        }) {
            self.bring_into_view(span.y0, span.y1);
        }
        self.dirty = true;
    }

    fn clear_find(&mut self) {
        let _ = self.find("", FindOptions::default());
    }

    /// The HTML body's engine finds and steps on its own; the text's
    /// matches step through the bar.
    fn step(&mut self, forward: bool) -> bool {
        match (&self.html_query, self.html.rect()) {
            (Some(query), Some(_)) if self.text_matches.is_empty() => {
                let query = query.clone();
                self.html.step(&query, forward);
                true
            }
            _ => false,
        }
    }

    fn take_engine_results(&mut self) -> Option<(usize, Option<usize>)> {
        let found = self.html.take_found();
        if self.text_matches.is_empty() {
            found
        } else {
            None
        }
    }
}

impl MessageReader {
    /// Scroll the reader so the span from `y0` to `y1` (as drawn) is in
    /// sight, inside its margins: a find match in the text or the mail.
    fn bring_into_view(&mut self, y0: f64, y1: f64) {
        let margin = f64::from(self.theme.tokens.spacing.s6);
        let offset = self.region.offset().y;
        let top = self.rect.y0 + margin;
        let bottom = self.rect.y1 - margin;
        let target = if y0 < top {
            offset - (top - y0)
        } else if y1 > bottom {
            offset + (y1 - bottom)
        } else {
            offset
        };
        // A find's match glides into view, or fades in when far
        // (`ScrollRegion::glide_to`).
        self.region
            .glide_to(Vec2::new(0.0, target.clamp(0.0, self.scroll_max)));
        self.dirty = true;
    }

    /// Whether the window's find acts on the open message: one is shown and
    /// the reader holds the keyboard (or a find in it is running).
    fn offers_find(&self) -> bool {
        self.shown.is_some()
            && (self.focused || self.html_query.is_some() || !self.text_matches.is_empty())
    }
}

impl Widget for MessageReader {
    /// Find in the open message while the reader has the keyboard: Ctrl+F
    /// and the window bar's search find where the person is, the message
    /// they read; from the list they search the folder.
    fn provide<'a>(&'a self, request: &mut ui::widget::capability::Request<'a, '_>) {
        if self.offers_find() && request.wants::<dyn Findable>() {
            request.provide::<dyn Findable>(self);
        }
    }

    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        if self.offers_find() && request.wants::<dyn Findable>() {
            request.provide::<dyn Findable>(self);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.fill_leaf(node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let theme = self.theme.clone();
        self.region.set_viewport(self.rect, &theme);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.take_reader_requests();
        if self.shared.borrow().bulk_selecting {
            self.attachments.clear();
            self.attachment_saves.clear();
            self.remote_buttons = RemoteButtons::default();
            self.html.unload();
            self.html_message = None;
            self.bodies.clear();
            self.headers.clear();
            self.active = None;
            let shared = self.shared.clone();
            let state = shared.borrow();
            self.paint_empty(&mut PaintCtx::new(scene, t, tr, theme), &state);
            return;
        }
        self.theme = theme.clone();
        self.region.set_viewport(self.rect, theme);
        self.paint_thread(scene, t, tr, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if *ctx.handled {
            return;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            let before = self.hover_at(self.pointer);
            self.pointer = ctx.logical(position);
            if self.hover_at(self.pointer) != before {
                self.dirty = true;
            }
        }
        // A right press (a long press on a touch screen) opens the reader's
        // menu: the link under it, copying, the message's format.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Right,
            ..
        } = event
            && self.rect.contains(self.pointer)
            && self.open_menu(self.pointer)
        {
            *ctx.handled = true;
            return;
        }
        let wheel = self.wheel_scrolls_body(event);
        let hovered = self.html.hovered().map(str::to_owned);
        self.html.forward(event, self.pointer, wheel, ctx);
        if self.html.hovered().map(str::to_owned) != hovered {
            self.dirty = true;
        }
        if self.html.is_focused()
            && let Some(body) = self.active.and_then(|id| self.bodies.get_mut(&id))
        {
            body.input.set_focus(false);
        }
        if *ctx.handled {
            return;
        }
        self.forward_to_body(event, ctx);
        if *ctx.handled {
            return;
        }
        let theme = self.theme.clone();
        if self.region.handle_event(event, ctx, &theme) {
            self.dirty = true;
        }
        if *ctx.handled {
            return;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && self.rect.contains(self.pointer)
            && self.press(self.pointer)
        {
            *ctx.handled = true;
        }
    }

    fn name(&self) -> &'static str {
        "MailReader"
    }

    /// The reader takes the keyboard while a message is open, so its text
    /// can be selected and copied from the keys.
    fn is_focusable(&self) -> bool {
        !self.bodies.is_empty() || self.html.rect().is_some()
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if !focused {
            self.html.set_focus(false);
        }
        if let Some(body) = self.active.and_then(|id| self.bodies.get_mut(&id)) {
            body.input.set_focus(focused && !self.html.is_focused());
        }
        self.dirty = true;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn test_id(&self) -> Option<&str> {
        Some("mail_reader")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Over a link of an HTML message, the address it really opens; over an
    /// attachment, what a press does.
    fn tooltip(&self) -> Option<&str> {
        let point = self.pointer;
        if self.html.rect().is_some_and(|rect| rect.contains(point))
            && let Some(link) = self.html.hovered()
        {
            return Some(link);
        }
        if self
            .attachments
            .iter()
            .any(|(rect, _, _)| rect.contains(point))
        {
            return Some("Open");
        }
        if self
            .attachment_saves
            .iter()
            .any(|(rect, _, _)| rect.contains(point))
        {
            return Some("Save");
        }
        None
    }

    fn needs_redraw(&self) -> bool {
        // A leaf WebView's default `needs_redraw` is always true, so it is
        // deliberately not folded in (it would spin the compositor); a new
        // engine frame surfaces through `needs_animation_frame`.
        self.dirty || self.shared.borrow().dirty
    }

    fn needs_animation_frame(&self) -> bool {
        self.region.needs_animation_frame(&self.theme)
            || self.html.needs_animation_frame()
            || self
                .bodies
                .values()
                .any(|body| body.input.needs_animation_frame())
    }

    /// A glide after a flick scrolls the message as the wheel does.
    fn update(&mut self, _state: &ui::widget::AppModel, _path: &ui::widget::ModelPath) -> bool {
        self.dirty |= self.region.advance();
        false
    }

    /// An HTML message still on its way to the screen.
    fn has_pending_visual_work(&self) -> bool {
        self.html.pending()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.region.next_wake_deadline(&self.theme)
    }

    fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        self.region.primary_vertical_scroll()
    }

    fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        let result = self.region.apply_primary_vertical_scroll(action);
        self.dirty = true;
        result
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.html.mark_drawn();
        for body in self.bodies.values_mut() {
            body.input.mark_drawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Folder;
    use crate::state::MailState;
    use ui::webview::test_engine::TestEngine;

    fn shared() -> Shared {
        MailState::shared(MailState::new_sample(crate::avatars::Avatars::new()))
    }

    /// A reader of `w`×`h` showing message `id`, opened as a row click opens it.
    fn reader_for(shared: &Shared, id: MessageId, w: f64, h: f64) -> MessageReader {
        let mut doc = crate::resource::TestMail::doc();
        shared
            .borrow_mut()
            .open_message(&mut ui::widget::Edits::lend(&mut doc), id);
        shared.borrow_mut().take_intents();
        let mut reader = MessageReader::new(shared.clone());
        reader.rect = Rect::new(0.0, 0.0, w, h);
        reader
    }

    /// An engine that paints a mail's pages as Chromium does: each with the
    /// document's end mark (`crate::html_mail::END_MARK`).
    fn html_engine() -> std::sync::Arc<TestEngine> {
        let engine = TestEngine::new();
        engine.mark_page_end();
        engine
    }

    fn draw(reader: &mut MessageReader) {
        let mut scene = Scene::new();
        reader.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
    }

    fn draw_themed(reader: &mut MessageReader, theme: &Theme) {
        let mut scene = Scene::new();
        reader.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            theme,
            &PresenceCtx::empty(),
        );
    }

    /// The message body, plain or HTML, grows once with the text size and
    /// the UI scale: as tall as the theme's body type, never scaled twice.
    #[test]
    fn message_bodies_grow_once_at_enlarged_scales() {
        let shared = shared();
        let id = message_from(&shared, "billing@lumen-energy.example");
        let base = Theme::dark();
        for html in [None, Some("<h1>A heading</h1><p>A message.</p>")] {
            {
                let mut state = shared.borrow_mut();
                let message = state.mailbox.messages.get_mut(&id).unwrap();
                message.body = "A message.".to_owned();
                message.html_body = html.map(str::to_owned);
            }
            let mut reader = reader_for(&shared, id, 960.0, 1200.0);
            draw_themed(&mut reader, &base);
            let base_height = reader.bodies[&id].input.caret_rect(&base, 0).height();
            for theme in [
                Theme::dark().with_text_scale(1.5),
                Theme::dark().with_ui_scale(1.5),
                Theme::dark().with_ui_scale(1.5).with_text_scale(1.5),
            ] {
                draw_themed(&mut reader, &theme);
                let body = &reader.bodies[&id];
                let expected = theme.text(TextRole::Body);
                assert!(
                    (body.style.font_px - expected.size).abs() < 0.01,
                    "{html:?}: the document style is the theme's body type"
                );
                let growth = f64::from(expected.size / base.text(TextRole::Body).size);
                let height = body.input.caret_rect(&theme, 0).height();
                assert!(
                    (height - base_height * growth).abs() < 0.1,
                    "{html:?}: grows once with the theme: {height} vs {base_height} x {growth}"
                );
            }
        }
    }

    /// At a large text size a long sender name keeps its line: the date
    /// moves below it, and every recipient shows, wrapped, above the body.
    #[test]
    fn sender_details_reflow_rather_than_elide_at_large_text() {
        let shared = shared();
        let id = message_from(&shared, "billing@lumen-energy.example");
        let theme = Theme::dark().with_text_scale(1.5);
        {
            let mut state = shared.borrow_mut();
            let message = state.mailbox.messages.get_mut(&id).unwrap();
            message.html_body = None;
            message.from.name = Some("A".to_owned());
            message.to.clear();
            message.cc.clear();
        }
        let mut reader = reader_for(&shared, id, 420.0, 1400.0);
        let body_top = |reader: &mut MessageReader| {
            draw_themed(reader, &theme);
            reader.bodies[&id].input.get_rect().y0
        };
        let inline = body_top(&mut reader);
        shared
            .borrow_mut()
            .mailbox
            .messages
            .get_mut(&id)
            .unwrap()
            .from
            .name = Some("Alexandria from the community garden planning group".to_owned());
        let stacked = body_top(&mut reader);
        let caption = theme.text(TextRole::Caption);
        let mut text = TextRenderer::new();
        let caption_h = f64::from(text.measure_style("Ag", None, &caption).1);
        assert!(
            (stacked - inline - caption_h).abs() < 0.1,
            "a crowded date takes its own line: {inline} then {stacked}"
        );

        let recipients = {
            let mut state = shared.borrow_mut();
            let message = state.mailbox.messages.get_mut(&id).unwrap();
            message.to = vec![crate::model::Addr::new(
                "The community garden volunteer coordinators",
                "volunteers@garden.example",
            )];
            message.cc = vec![crate::model::Addr::new(
                "The neighbourhood planning and maintenance group",
                "planning@garden.example",
            )];
            recipients_line(message)
        };
        let wrapped = body_top(&mut reader);
        assert!(
            wrapped - stacked > 2.0 * caption_h,
            "every recipient shows, wrapped over lines of their own: {stacked} then {wrapped} for {recipients}"
        );
    }

    fn message_from(shared: &Shared, email: &str) -> MessageId {
        shared
            .borrow()
            .mailbox
            .messages
            .values()
            .find(|m| m.from.email == email)
            .unwrap_or_else(|| panic!("sample has mail from {email}"))
            .id
    }

    /// An HTML body is as tall as its document, so the reader scrolls the
    /// conversation and the body as one page, however tall the body is.
    #[test]
    fn an_html_body_is_as_tall_as_its_document_and_scrolls_with_the_reader() {
        use ui::webview::WebViewEvent;
        let shared = shared();
        let id = message_from(&shared, "hello@fieldnotes.example");
        let mut reader = reader_for(&shared, id, 960.0, 820.0);
        let engine = html_engine();
        engine.set_measures_document(true);
        reader.set_html_engine(engine.clone());
        draw(&mut reader);
        assert!(
            engine
                .commands()
                .contains(&WebViewCommand::ReportDocumentSize(true)),
            "the reader asks how tall the document is"
        );
        assert!(
            reader.html.rect().is_none(),
            "not shown before its height is known"
        );

        let document = |height: f64, scroll_y: f64| WebViewEvent::DocumentSize {
            width: 912.0,
            height,
            scroll_y,
        };
        engine.push_event(document(300.0, 0.0));
        draw(&mut reader);
        draw(&mut reader);
        let short = reader.html.rect().expect("shown once measured");
        assert!((short.height() - 300.0).abs() < 0.01, "{short:?}");
        assert!(
            short.y1 < reader.rect.y1,
            "the conversation goes on below a short body"
        );
        let wheel_down = ui::synthetic_input::mouse_wheel(0.0, -3.0);
        reader.pointer = short.center();
        assert!(
            !reader.wheel_scrolls_body(&wheel_down),
            "a body that fits never scrolls inside"
        );

        engine.push_event(document(5000.0, 0.0));
        draw(&mut reader);
        draw(&mut reader);
        let tall = reader.html.rect().expect("still shown");
        assert!(
            (tall.height() - 5000.0).abs() < 0.01,
            "a tall body is as tall as its document: {tall:?}"
        );
        assert!(
            reader.scroll_max > 0.0,
            "the reader scrolls it with the header"
        );
        reader.pointer = tall.center();
        assert!(
            !reader.wheel_scrolls_body(&wheel_down),
            "the reader scrolls a tall body too"
        );
        reader.region.set_offset(Vec2::new(0.0, reader.scroll_max));
        let wheel_up = ui::synthetic_input::mouse_wheel(0.0, 3.0);
        assert!(
            !reader.wheel_scrolls_body(&wheel_down) && !reader.wheel_scrolls_body(&wheel_up),
            "and never inside it: one scroll surface"
        );
    }

    /// The body appears with the engine's first frame of the page at the
    /// size it is shown at. Grown to its document's height, the view has no
    /// such frame until the engine paints again, and a body drawn then is a
    /// blank sheet that flashes before the mail. Once shown, it stays: a
    /// frame the engine delivers late, for another size or before the page
    /// drew, never replaces the mail with a blank sheet.
    #[test]
    fn an_opened_html_mail_is_never_presented_blank() {
        use ui::webview::WebViewEvent;
        let shared = shared();
        let id = message_from(&shared, "hello@fieldnotes.example");
        let mut reader = reader_for(&shared, id, 960.0, 820.0);
        let engine = html_engine();
        engine.set_measures_document(true);
        reader.set_html_engine(engine.clone());
        let present = |reader: &mut MessageReader, when: &str| {
            draw(reader);
            assert!(
                reader.html.rect().is_none() || reader.html.draws_page(),
                "{when}: the body is drawn without a frame of the page at its size"
            );
        };
        present(&mut reader, "opened");
        engine.hold_frames(true);
        engine.push_event(WebViewEvent::DocumentSize {
            width: 912.0,
            height: 1400.0,
            scroll_y: 0.0,
        });
        for _ in 0..4 {
            present(&mut reader, "measured, the engine still painting");
        }
        assert!(
            reader.html.rect().is_none() && reader.html.pending(),
            "hidden, and still on its way, until the engine paints it"
        );
        // The engine paints the grown view before the page has drawn there:
        // its background, at the right size, without the page.
        engine.paint_before_page();
        present(&mut reader, "grown, painted before the page");
        assert!(
            reader.html.rect().is_none() && reader.html.pending(),
            "a frame of the right size without the page is not the page"
        );
        engine.hold_frames(false);
        present(&mut reader, "painted");
        let shown = reader.html.rect().expect("shown with its first frame");
        assert!((shown.height() - 1400.0).abs() < 0.01, "{shown:?}");
        assert!(!reader.html.pending(), "on screen");
        // Ready for the screen, and then the engine delivers a frame it
        // painted for an earlier size, or a blank one, before the draw.
        engine.paint_stale(912, 600);
        present(&mut reader, "a late frame for another size");
        assert!(reader.html.rect().is_some(), "the message stays on screen");
        engine.paint_before_page();
        present(&mut reader, "a late blank frame");
        assert!(reader.html.rect().is_some(), "the message stays on screen");

        // A later resize keeps the last frame up until the next one comes.
        engine.hold_frames(true);
        engine.push_event(WebViewEvent::DocumentSize {
            width: 912.0,
            height: 1600.0,
            scroll_y: 0.0,
        });
        draw(&mut reader);
        draw(&mut reader);
        assert!(reader.html.rect().is_some(), "the message stays on screen");
    }

    /// On a dense display the body appears with the page painted at that
    /// display's scale. Held back until it has painted, it still tells the
    /// engine the scale it will be shown at; a frame painted at another
    /// scale is not the page on screen, and drawn, it is a page at half
    /// size in a blank sheet.
    #[test]
    fn an_html_body_appears_painted_at_the_scale_it_is_shown_at() {
        use ui::webview::WebViewEvent;
        let shared = shared();
        let id = message_from(&shared, "hello@fieldnotes.example");
        let mut reader = reader_for(&shared, id, 960.0, 820.0);
        let engine = html_engine();
        engine.set_measures_document(true);
        reader.set_html_engine(engine.clone());
        let dense = |reader: &mut MessageReader| {
            let mut scene = Scene::new();
            reader.draw(
                &mut scene,
                Affine::scale(2.0),
                &mut TextRenderer::new(),
                &Theme::dark(),
                &PresenceCtx::empty(),
            );
        };
        dense(&mut reader);
        engine.hold_frames(true);
        engine.push_event(WebViewEvent::DocumentSize {
            width: 912.0,
            height: 1400.0,
            scroll_y: 0.0,
        });
        for _ in 0..4 {
            dense(&mut reader);
        }
        assert!(reader.html.pending(), "held back until the engine paints");
        engine.hold_frames(false);
        let frame = ui::webview::WebViewEngine::frames(&*engine)
            .lock()
            .unwrap()
            .clone()
            .expect("painted");
        assert!(!reader.html.pending(), "painted at the size it is shown at");
        assert_eq!(frame.height, 1400 * 2, "painted at the display's scale");
        dense(&mut reader);
        assert!(reader.html.rect().is_some() && reader.html.draws_page());
    }

    /// The `(message, remote)` the HTML view has loaded.
    fn loaded(reader: &MessageReader) -> Option<(MessageId, bool)> {
        reader
            .html_loaded()
            .map(|setting| (setting.message, setting.remote))
    }

    /// The `data:` document the engine was last told to show, decoded.
    fn shown_document(engine: &TestEngine) -> String {
        use base64::Engine as _;
        let url = engine
            .commands()
            .iter()
            .rev()
            .find_map(|c| match c {
                WebViewCommand::Navigate(url) => Some(url.clone()),
                _ => None,
            })
            .expect("the reader navigates the HTML body");
        let encoded = url
            .strip_prefix("data:text/html;charset=utf-8;base64,")
            .unwrap_or_else(|| panic!("a sealed data: document, never a remote URL: {url}"));
        String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .expect("base64"),
        )
        .expect("utf-8")
    }

    #[test]
    fn html_mail_renders_sealed_with_remote_content_blocked_until_allowed() {
        let shared = shared();
        let id = message_from(&shared, "hello@fieldnotes.example");
        let mut reader = reader_for(&shared, id, 960.0, 820.0);
        let engine = html_engine();
        reader.set_html_engine(engine.clone());
        draw(&mut reader);
        assert_eq!(
            loaded(&reader),
            Some((id, false)),
            "the HTML body loads with remote content blocked"
        );
        // The engine refuses every web request before the document loads.
        let commands = engine.commands();
        let rules = commands
            .iter()
            .position(|c| matches!(c, WebViewCommand::SetNativeFilterRules { rules, enabled: Some(true) } if rules.lines().any(|line| line == "|https://")))
            .expect("the request filter blocks the web");
        let navigate = commands
            .iter()
            .position(|c| matches!(c, WebViewCommand::Navigate(_)))
            .expect("the body loads");
        assert!(rules < navigate, "the rules come before the document");
        let document = shown_document(&engine);
        assert!(!document.contains("track.fieldnotes.example"), "{document}");
        assert!(document.contains("img-src data:;"));

        // "Load images" loads this message's, and only this message's.
        let button = reader.remote_button().expect("the Load images button");
        assert!(reader.press(button.center()));
        draw(&mut reader);
        assert_eq!(loaded(&reader), Some((id, true)));
        assert!(shown_document(&engine).contains("track.fieldnotes.example"));
        assert!(
            engine.commands().iter().rev().any(|c| matches!(c, WebViewCommand::SetNativeFilterRules { rules, .. } if !rules.lines().any(|line| line == "|https://"))),
            "allowed, images load; the rules still refuse everything else"
        );
        assert!(
            !shared
                .borrow()
                .remote_senders
                .contains("hello@fieldnotes.example"),
            "one message is not its sender"
        );

        let plain = message_from(&shared, "lena@fischer.example");
        let mut doc = crate::resource::TestMail::doc();
        shared
            .borrow_mut()
            .open_message(&mut ui::widget::Edits::lend(&mut doc), plain);
        draw(&mut reader);
        assert!(loaded(&reader).is_none(), "plain text skips the webview");
        assert!(reader.remote_button().is_none());
        shared
            .borrow_mut()
            .open_message(&mut ui::widget::Edits::lend(&mut doc), id);
        draw(&mut reader);
        assert_eq!(
            loaded(&reader),
            Some((id, true)),
            "the message stays allowed while the window is open"
        );

        // "Always for this sender" is the person's preference.
        shared.borrow_mut().remote_messages.clear();
        draw(&mut reader);
        assert_eq!(loaded(&reader), Some((id, false)));
        let always = reader.remote_buttons.always.expect("the always button");
        assert!(reader.press(always.center()));
        assert!(
            shared
                .borrow()
                .remote_senders
                .contains("hello@fieldnotes.example"),
            "the permission is the person's preference, not the reader's memory"
        );
        draw(&mut reader);
        assert_eq!(loaded(&reader), Some((id, true)));
        let stop = reader.remote_buttons.stop.expect("a way to stop it again");
        assert!(reader.press(stop.center()));
        draw(&mut reader);
        assert_eq!(loaded(&reader), Some((id, false)));
    }

    #[test]
    fn a_followed_link_opens_in_the_browser_and_its_address_shows_first() {
        use ui::webview::WebViewEvent;
        let shared = shared();
        let id = message_from(&shared, "hello@fieldnotes.example");
        let mut reader = reader_for(&shared, id, 960.0, 820.0);
        let engine = html_engine();
        reader.set_html_engine(engine.clone());
        draw(&mut reader);
        let rect = reader.html.rect().expect("the body is shown");
        reader.pointer = rect.center();
        engine.push_event(WebViewEvent::LinkHovered(
            "https://fieldnotes.example/read".into(),
        ));
        draw(&mut reader);
        assert_eq!(
            reader.tooltip(),
            Some("https://fieldnotes.example/read"),
            "the pointer over a link shows where it really goes"
        );
        assert!(
            reader.open_menu(rect.center()),
            "a long press opens the menu"
        );
        let menu = shared.borrow_mut().take_intents();
        assert!(
            menu.iter().any(|intent| matches!(
                intent,
                Intent::ReaderMenu(found, _, choices) if *found == id
                    && choices.first() == Some(&ReaderChoice::OpenLink("https://fieldnotes.example/read".into()))
                    && choices.contains(&ReaderChoice::CopyLink("https://fieldnotes.example/read".into()))
            )),
            "{menu:?}"
        );
        // The engine hands back a followed link; the reader opens nothing
        // but web and mail addresses, and only through the app.
        engine.push_new_tab_request("https://fieldnotes.example/read");
        engine.push_new_tab_request("file:///etc/passwd");
        draw(&mut reader);
        let intents = shared.borrow_mut().take_intents();
        let opened: Vec<&String> = intents
            .iter()
            .filter_map(|intent| match intent {
                Intent::OpenLink(url) => Some(url),
                _ => None,
            })
            .collect();
        assert_eq!(opened, ["https://fieldnotes.example/read"]);
        // A page that leaves the mail is shown the mail again; nothing opens.
        engine.push_event(WebViewEvent::UriChanged(
            "https://elsewhere.example/".into(),
        ));
        draw(&mut reader);
        assert!(
            !shared
                .borrow_mut()
                .take_intents()
                .iter()
                .any(|intent| matches!(intent, Intent::OpenLink(_))),
            "a navigation the person did not ask for opens nothing"
        );
    }

    #[test]
    fn find_and_copy_reach_the_html_body() {
        use ui::webview::WebViewEvent;
        let shared = shared();
        let id = message_from(&shared, "hello@fieldnotes.example");
        let mut reader = reader_for(&shared, id, 960.0, 820.0);
        let engine = html_engine();
        reader.set_html_engine(engine.clone());
        draw(&mut reader);
        let mut bar = ui::widget::FindBar::new();
        bar.set_query("garden");
        bar.drive(&mut reader);
        let find = engine
            .commands()
            .iter()
            .find_map(|c| match c {
                WebViewCommand::Find {
                    id,
                    text,
                    next: false,
                    ..
                } => Some((*id, text.clone())),
                _ => None,
            })
            .expect("the engine finds in the page");
        assert_eq!(find.1, "garden");
        engine.push_event(WebViewEvent::FindResult {
            id: find.0,
            count: 3,
            active: 1,
            final_update: true,
        });
        draw(&mut reader);
        bar.drive(&mut reader);
        assert_eq!(
            bar.position(),
            Some((1, 3)),
            "the bar counts the page's matches"
        );
        bar.next();
        bar.drive(&mut reader);
        assert!(
            engine.commands().iter().any(|c| matches!(
                c,
                WebViewCommand::Find {
                    next: true,
                    forward: true,
                    ..
                }
            )),
            "the engine steps"
        );

        engine.set_selection("Seed swap on Saturday");
        reader.html.copy();
        assert!(
            engine
                .commands()
                .iter()
                .any(|c| matches!(c, WebViewCommand::CopySelection { .. })),
            "a copy reads the page's selection for the system clipboard"
        );
    }

    /// Ctrl+F does what the window bar's search does: with the message in
    /// hand it finds in the message; with the list focused the reader offers
    /// no find, and the key reaches Mail, which searches the folder.
    #[test]
    fn find_is_the_messages_once_the_reader_has_the_keyboard() {
        let shared = shared();
        let id = message_from(&shared, "hello@fieldnotes.example");
        let mut reader = reader_for(&shared, id, 960.0, 820.0);
        reader.set_html_engine(html_engine());
        draw(&mut reader);
        assert!(
            ui::widget::with_findable(&mut reader, &mut |_| ()).is_none(),
            "the list has the keyboard: the folder is searched"
        );
        reader.set_focus(true);
        assert!(
            ui::widget::with_findable(&mut reader, &mut |_| ()).is_some(),
            "the message has the keyboard: it is found in"
        );
    }

    #[test]
    fn a_message_reads_as_selectable_text_and_html_loads_nothing() {
        let shared = shared();
        let html = message_from(&shared, "hello@fieldnotes.example");
        let mut reader = reader_for(&shared, html, 960.0, 820.0);
        assert!(
            !reader.is_focusable(),
            "nothing to select before it is drawn"
        );
        draw(&mut reader);
        assert!(
            reader.html_loaded().is_none(),
            "without an isolated engine nothing is fetched"
        );
        let body = reader
            .active
            .and_then(|id| reader.bodies.get(&id))
            .expect("the message text is a field");
        assert!(body.input.is_read_only(), "selectable, never editable");
        assert!(
            !body.input.value().contains('<'),
            "the markup is read, not shown: {:?}",
            body.input.value()
        );
        assert!(reader.is_focusable(), "the keys can reach the text");
        reader.set_focus(true);
        assert!(
            reader
                .active
                .and_then(|id| reader.bodies.get(&id))
                .is_some_and(|b| b.input.is_focused())
        );
    }

    #[test]
    fn an_attachment_chip_opens_it_and_its_button_saves_it_through_the_portal() {
        let shared = shared();
        let id = message_from(&shared, "billing@lumen-energy.example");
        let mut reader = reader_for(&shared, id, 760.0, 900.0);
        draw(&mut reader);
        let (rect, message, index) = *reader
            .attachment_rects()
            .first()
            .expect("the attachment is a chip");
        assert_eq!(message, id);
        assert!(reader.press(rect.center()), "chip press consumed");
        assert!(
            shared.borrow_mut().take_intents().iter().any(|intent| matches!(
                intent,
                Intent::OpenAttachment(found, found_index) if *found == id && *found_index == index
            )),
            "the chip opens the attachment"
        );
        let (save, _, _) = *reader
            .attachment_saves
            .first()
            .expect("a downloaded attachment can be saved");
        assert!(reader.press(save.center()));
        assert!(
            shared.borrow_mut().take_intents().iter().any(|intent| matches!(
                intent,
                Intent::SaveAttachment(found, found_index) if *found == id && *found_index == index
            )),
            "the reader hands the attachment's address to the app, which owns the portal"
        );
    }

    #[test]
    fn a_refetched_message_is_one_message_in_the_conversation() {
        let shared = MailState::shared(MailState::new_live(
            crate::account::AccountStore::new(),
            crate::avatars::Avatars::new(),
        ));
        let raw = b"From: Maya <maya@x.example>\r\nTo: sam@rivera.example\r\n\
Subject: Launch\r\nMessage-ID: <dup-1@x.example>\r\n\r\nHello there.\r\n";
        {
            let mut st = shared.borrow_mut();
            let m = crate::transport::parse_eml(raw, 100, Folder::Inbox).unwrap();
            st.selected = Some(m.id);
            st.mailbox.insert(m);
            st.mailbox
                .insert(crate::transport::parse_eml(raw, 200, Folder::Inbox).unwrap());
            st.mailbox
                .insert(crate::transport::parse_eml(raw, 300, Folder::Inbox).unwrap());
        }
        assert_eq!(shared.borrow().mailbox.count(Folder::Inbox), 1);
        assert_eq!(shared.borrow().selected_thread_messages().len(), 1);
    }

    fn raw(id: &str, subject: &str, reply_to: Option<&str>, body: &str) -> Vec<u8> {
        let reply = reply_to.map_or_else(String::new, |parent| {
            format!("In-Reply-To: <{parent}>\r\nReferences: <{parent}>\r\n")
        });
        format!(
            "From: Maya <maya@x.example>\r\nTo: sam@rivera.example\r\nSubject: {subject}\r\n\
Message-ID: <{id}>\r\n{reply}\r\n{body}\r\n"
        )
        .into_bytes()
    }

    #[test]
    fn the_reader_shows_the_conversation_the_list_shows_and_opens_earlier_messages() {
        let shared = MailState::shared(MailState::new_live(
            crate::account::AccountStore::new(),
            crate::avatars::Avatars::new(),
        ));
        let parse = |bytes: Vec<u8>, at: i64| {
            let mut message = crate::transport::parse_eml(&bytes, at, Folder::Inbox).unwrap();
            message.read = true;
            message
        };
        // Two conversations that only share a subject, and a reply in the
        // first one.
        let first = parse(raw("a@x", "Status update", None, "First status."), 100);
        let other = parse(raw("b@x", "Status update", None, "Unrelated status."), 200);
        let reply = parse(
            raw(
                "c@x",
                "Re: Status update",
                Some("a@x"),
                "Reply to the first.",
            ),
            300,
        );
        let (first_id, reply_id) = (first.id, reply.id);
        {
            let mut st = shared.borrow_mut();
            st.mailbox.insert(first);
            st.mailbox.insert(other);
            st.mailbox.insert(reply);
            st.selected = Some(reply_id);
        }
        assert_eq!(shared.borrow().list_rows().len(), 2, "two conversations");
        let thread: Vec<MessageId> = shared
            .borrow()
            .selected_thread_messages()
            .iter()
            .map(|message| message.id)
            .collect();
        assert_eq!(
            thread,
            [first_id, reply_id],
            "a shared subject is not a conversation"
        );

        let mut reader = MessageReader::new(shared);
        reader.rect = Rect::new(0.0, 0.0, 900.0, 800.0);
        draw(&mut reader);
        assert!(reader.bodies.contains_key(&reply_id), "the newest is open");
        assert!(
            !reader.bodies.contains_key(&first_id),
            "a read earlier message waits closed"
        );
        let header = reader
            .headers
            .iter()
            .find(|(_, id)| *id == first_id)
            .map(|(rect, _)| *rect)
            .expect("the earlier message's header can be clicked");
        assert!(reader.press(header.center()));
        draw(&mut reader);
        assert!(
            reader.bodies.contains_key(&first_id),
            "a click opens the earlier message"
        );
        let header = reader
            .headers
            .iter()
            .find(|(_, id)| *id == first_id)
            .map(|(rect, _)| *rect)
            .expect("an open earlier message keeps its header");
        assert!(reader.press(header.center()));
        assert!(
            !reader.expanded.contains(&first_id),
            "a second click closes it"
        );
    }

    #[test]
    fn a_closing_quote_folds_behind_a_button() {
        let body =
            "Sounds good, see you then.\n\nOn Jun 30, Maya wrote:\n> Lunch at noon?\n> \n> Maya\n";
        let (shown, quote) = split_trailing_quote(body);
        assert!(quote);
        assert_eq!(shown, "Sounds good, see you then.");
        assert_eq!(
            split_trailing_quote("> all of it is quote\n"),
            ("> all of it is quote\n", false),
            "a message that is only a quote keeps it"
        );
        assert!(!split_trailing_quote("No quote.\n").1);

        let shared = MailState::shared(MailState::new_live(
            crate::account::AccountStore::new(),
            crate::avatars::Avatars::new(),
        ));
        let mut message = crate::transport::parse_eml(
            format!("From: a@x\r\nSubject: S\r\nMessage-ID: <q@x>\r\n\r\n{body}").as_bytes(),
            100,
            Folder::Inbox,
        )
        .unwrap();
        message.read = true;
        let id = message.id;
        {
            let mut st = shared.borrow_mut();
            st.mailbox.insert(message);
            st.selected = Some(id);
        }
        let mut reader = MessageReader::new(shared);
        reader.rect = Rect::new(0.0, 0.0, 900.0, 800.0);
        draw(&mut reader);
        let text = |reader: &MessageReader| {
            reader
                .bodies
                .get(&id)
                .map(|body| body.input.value().to_owned())
                .unwrap_or_default()
        };
        assert!(!text(&reader).contains("Lunch at noon"), "the quote waits");
        let (button, _) = *reader.quote_buttons.first().expect("a button unfolds it");
        assert!(reader.press(button.center()));
        draw(&mut reader);
        assert!(
            text(&reader).contains("Lunch at noon"),
            "and shows on a click"
        );
    }

    #[test]
    fn unread_earlier_messages_open_with_the_conversation() {
        let shared = MailState::shared(MailState::new_live(
            crate::account::AccountStore::new(),
            crate::avatars::Avatars::new(),
        ));
        let first = crate::transport::parse_eml(
            &raw("a@x", "Plan", None, "Unread start."),
            100,
            Folder::Inbox,
        )
        .unwrap();
        let reply = crate::transport::parse_eml(
            &raw("c@x", "Re: Plan", Some("a@x"), "Latest."),
            200,
            Folder::Inbox,
        )
        .unwrap();
        let (first_id, reply_id) = (first.id, reply.id);
        {
            let mut st = shared.borrow_mut();
            st.mailbox.insert(first);
            st.mailbox.insert(reply);
            st.selected = Some(reply_id);
        }
        let mut reader = MessageReader::new(shared);
        reader.rect = Rect::new(0.0, 0.0, 900.0, 800.0);
        draw(&mut reader);
        assert!(
            reader.bodies.contains_key(&first_id),
            "unread mail above the newest is shown open"
        );
    }
}
