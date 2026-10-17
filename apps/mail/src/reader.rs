//! The open conversation: its subject, then each message with its sender,
//! date, recipients, body and attachments, newest last.
//!
//! A message is a document, so the reader paints it, reading every value from
//! the theme; the controls inside it (attachment chips, the remote-content
//! button) are the toolkit's faces.

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
use ui::webview::{SharedEngine, WebViewCommand};
use ui::widget::{
    AvatarFace, AvatarSize, Banner, BannerContent, BannerKind, Button, ButtonFace, ChipFace,
    ChipLead, ControlSize, EmptyState, EmptyStateContent, ScrollRegion, TaffyTreeExt, TextInput,
    WebView, Widget,
};
use ui::{TextKind, TextRole};

use crate::model::{Message, MessageId};
use crate::state::{Intent, MailState, Shared};
use crate::util::{format_absolute, gravatar_url, local_minutes};

/// The engine for HTML mail: the packaged Chromium runtime when the image has
/// it, over a throwaway profile (no cookies or storage survive the process),
/// otherwise the deterministic test engine, whose frames are never shown.
fn new_html_engine() -> (SharedEngine, bool) {
    #[cfg(feature = "live")]
    if let Some(engine) = native_html_engine() {
        return (engine, true);
    }
    let e: SharedEngine = ui::webview::test_engine::TestEngine::new();
    (e, false)
}

#[cfg(feature = "live")]
fn native_html_engine() -> Option<SharedEngine> {
    use std::sync::OnceLock;
    static PROFILE: OnceLock<Option<tempfile::TempDir>> = OnceLock::new();
    ui::webview::chromium::runtime::locate().ok()?;
    let profile = PROFILE
        .get_or_init(|| {
            tempfile::Builder::new()
                .prefix("tilo-mail-view")
                .tempdir()
                .ok()
        })
        .as_ref()?;
    match ui::webview::chromium::ChromiumEngine::with_profile_extensions(profile.path(), &[]) {
        Ok(engine) => Some(engine as SharedEngine),
        Err(error) => {
            tracing::warn!(%error, "mail: the HTML engine did not start");
            None
        }
    }
}

/// Open a link of a mail in the browser through the host opener. Only the web
/// and mail addresses open.
fn open_link(url: &str) {
    let lower = url.to_ascii_lowercase();
    if ["https://", "http://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
    {
        ui::widget::open_external_url(url);
    }
}

/// The open message's text: a read-only field, so it can be selected and
/// copied and its addresses open.
struct BodyText {
    /// The message, whether its HTML part is shown, and whether the quote
    /// it ends with is shown.
    key: (MessageId, bool, bool),
    /// Whether the message ends with a quote that is folded away.
    folded: bool,
    /// The style the runs were coloured in.
    style: crate::html_mail::DocumentStyle,
    input: TextInput,
}

impl BodyText {
    fn new(
        key: (MessageId, bool, bool),
        style: crate::html_mail::DocumentStyle,
        message: &Message,
        html: Option<&str>,
        theme: &Theme,
    ) -> Self {
        let quiet = ui::color::to_rgb8(theme.tokens.colors.text_dim);
        let show_quote = key.2;
        let (runs, links, folded) = if let Some(html) = html {
            html_runs(html, style.font_px, quiet, show_quote)
        } else {
            let (shown, quote) = split_trailing_quote(&message.body);
            let folded = quote && !show_quote;
            let text = if folded { shown } else { message.body.as_str() };
            (plain_runs(text, quiet), Vec::new(), folded)
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

/// A plain-text body as runs; quoted lines are quieter.
fn plain_runs(body: &str, quiet: [u8; 3]) -> Vec<ui::RichSpan> {
    let mut runs: Vec<ui::RichSpan> = Vec::new();
    for (index, line) in body.split('\n').enumerate() {
        let mut text = String::with_capacity(line.len() + 1);
        if index > 0 {
            text.push('\n');
        }
        text.push_str(line.trim_end_matches('\r'));
        let quoted = line.trim_start().starts_with('>');
        runs.push(ui::RichSpan::new(text).color(quoted.then_some(quiet)));
    }
    runs
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

/// The label of an attachment chip.
pub(crate) fn attachment_label(attachment: &crate::model::Attachment) -> String {
    format!("{} ({})", attachment.filename, attachment.size_human())
}

/// The recipients line under a sender.
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
        let width = ui::num::f32_of_f64(self.width.max(1.0));
        let (_, h) = self.ctx.text.measure_style(text, Some(width), style);
        let theme = self.ctx.theme;
        let ink = if dim {
            theme.text_dim
        } else {
            theme.text_primary
        };
        self.ctx
            .text(text, style, ink, Point::new(self.x, self.y), Some(width));
        self.y += f64::from(h);
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
        let image = self.state.images.get(&gravatar_url(&message.from.email));
        AvatarFace::new(&name, &message.from.email)
            .image(image.as_ref())
            .paint(
                &mut self.ctx,
                Rect::new(self.x, self.y, self.x + side, self.y + side),
            );

        let text_x = self.x + side + f64::from(spacing.s3);
        let right = self.x + self.width;
        let date = format_absolute(local_minutes(message.timestamp_min));
        let (date_w, _) = self.ctx.text.measure_style(&date, None, &date_style);
        let date_x = right - f64::from(date_w);
        let gap = f64::from(spacing.s3);
        let name_w = (date_x - gap - text_x).max(0.0);
        let mut y = self.y;
        self.line_at(
            &name,
            &name_style,
            theme.text_primary,
            Point::new(text_x, y),
            name_w,
        );
        // The date sits on the name's line box.
        self.ctx.text(
            &date,
            &date_style,
            theme.text_dim,
            Point::new(date_x, y + (name_h - caption_h) / 2.0),
            None,
        );
        y += name_h;
        let full_w = (right - text_x).max(0.0);
        self.line_at(
            &message.from.email,
            &address_style,
            theme.text_dim,
            Point::new(text_x, y),
            full_w,
        );
        y += caption_h;
        let to = recipients_line(message);
        if !to.is_empty() {
            self.line_at(&to, &caption, theme.text_dim, Point::new(text_x, y), full_w);
            y += caption_h;
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
        for (index, attachment) in message.attachments.iter().enumerate() {
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
        }
        self.y += h;
    }
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
    /// The "Load images" / "Block images" button as last painted.
    remote_button: Option<Rect>,
    /// The conversation the scroll offset belongs to.
    shown: Option<MessageId>,
    /// The sandboxed `WebView` that renders an HTML body, its engine, and
    /// the `(message, block_remote)` loaded into it. Created on the first
    /// HTML mail, so text-only use stays webview-free.
    html_engine: Option<SharedEngine>,
    /// True only for a real isolated browser renderer; the deterministic
    /// test engine is never presented as message content.
    html_engine_live: bool,
    html_view: Option<WebView>,
    html_key: Option<(MessageId, bool)>,
    /// The style the loaded HTML body was set in.
    html_style: Option<crate::html_mail::DocumentStyle>,
    /// The message the HTML view shows, so remote content is blocked again
    /// for each message.
    html_shown_id: Option<MessageId>,
    /// Whether remote images are loaded for the open message: the person
    /// allowed them for its sender.
    html_load_remote: bool,
    /// The open message's sender.
    html_sender: String,
    html_view_rect: Option<Rect>,
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
    /// Whether the reader has the keyboard, which its text uses.
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
            remote_button: None,
            shown: None,
            html_engine: None,
            html_engine_live: false,
            html_view: None,
            html_key: None,
            html_style: None,
            html_shown_id: None,
            html_load_remote: false,
            html_sender: String::new(),
            html_view_rect: None,
            bodies: HashMap::new(),
            active: None,
            expanded: HashSet::new(),
            headers: Vec::new(),
            quotes_open: HashSet::new(),
            quote_buttons: Vec::new(),
            focused: false,
            selecting: false,
            dirty: true,
        }
    }

    fn ensure_html_view(&mut self) -> &mut WebView {
        if self.html_engine.is_none() {
            let (engine, live) = new_html_engine();
            self.html_engine = Some(engine);
            self.html_engine_live = live;
        }
        let engine = self
            .html_engine
            .clone()
            .unwrap_or_else(|| new_html_engine().0);
        self.html_view
            .get_or_insert_with(|| WebView::new(engine).with_test_id("mail_html_body"))
    }

    /// Load `html` for message `id` into the sandboxed view (the
    /// remote-blocked `data:` document and its content filter) unless that
    /// exact `(message, block state)` is loaded already in this theme.
    fn load_html_body(&mut self, id: MessageId, html: &str, theme: &Theme) {
        let block_remote = !self.html_load_remote;
        let key = (id, block_remote);
        let style = crate::html_mail::DocumentStyle::from_theme(theme);
        if self.html_key == Some(key) && self.html_style.as_ref() == Some(&style) {
            return;
        }
        let url = crate::html_mail::data_url(html, block_remote, &style);
        let filter = std::sync::Arc::new(crate::html_mail::content_filter_json(block_remote));
        let engine = self.ensure_html_view().engine().clone();
        engine.send(WebViewCommand::SetContentFilter(Some(filter)));
        engine.send(WebViewCommand::Navigate(url));
        self.html_key = Some(key);
        self.html_style = Some(style);
    }

    /// Inject the HTML-body engine (tests observe the commands the reader
    /// sends). Call before the first HTML mail is drawn.
    #[cfg(test)]
    pub(crate) fn set_html_engine(&mut self, engine: SharedEngine) {
        self.html_engine = Some(engine);
        self.html_engine_live = true;
    }

    /// The `(message, block_remote)` loaded into the HTML view, if any.
    #[cfg(test)]
    pub(crate) fn html_loaded_key(&self) -> Option<(MessageId, bool)> {
        self.html_key
    }

    /// The attachment chips as last painted, for tests.
    #[cfg(test)]
    pub(crate) fn attachment_rects(&self) -> &[(Rect, MessageId, usize)] {
        &self.attachments
    }

    /// The remote-content button as last painted, for tests.
    #[cfg(test)]
    pub(crate) fn remote_button(&self) -> Option<Rect> {
        self.remote_button
    }

    fn paint_empty(&self, ctx: &mut PaintCtx<'_>, state: &MailState) {
        // Mail is still arriving: nothing to say about a selection yet.
        if state.fetching_first && state.mailbox.messages.is_empty() {
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

    /// Paint the remote-content banner for an HTML body at the flow's
    /// cursor: what is blocked and the button that changes it.
    fn paint_remote_line(&mut self, flow: &mut Flow<'_, '_>) {
        let theme = flow.ctx.theme;
        let (icon, title, action) = if self.html_load_remote {
            (Icon::Image, "Remote images shown", Some("Block images"))
        } else {
            (Icon::Shield, "Remote images blocked", Some("Load images"))
        };
        let mut content = BannerContent::new(BannerKind::Neutral, title).with_icon(icon);
        if let Some(label) = action {
            content = content.action(label);
        }
        let height = Banner::height(&content, flow.width, flow.ctx.text, theme);
        let rect = Rect::new(flow.x, flow.y, flow.x + flow.width, flow.y + height);
        let hit = Banner::paint(&mut flow.ctx, rect, &content, flow.pointer);
        self.remote_button = hit.action;
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
        let style = crate::html_mail::DocumentStyle::from_theme(theme);
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
            let mut body = BodyText::new(key, style, message, html, theme);
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
        flow.y += height;
        if body.folded {
            // The quote it ends with waits behind a quiet button.
            flow.y += f64::from(theme.tokens.spacing.s2);
            let side = ButtonFace::height(ControlSize::Small, theme);
            let button = Rect::new(flow.x, flow.y, flow.x + side * 1.5, flow.y + side);
            Button::paint(
                flow.ctx.scene,
                flow.ctx.transform,
                flow.ctx.text,
                theme,
                &ButtonFace::icon_only(button, Icon::MoreHorizontal, "Show quoted text")
                    .control_size(ControlSize::Small)
                    .hovered(button.contains(flow.pointer)),
            );
            self.quote_buttons.push((button, message.id));
            flow.y += side;
        }
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
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        // What this frame paints is all that can be pressed.
        self.remote_button = None;
        self.quote_buttons.clear();
        let messages = state.selected_thread_messages();
        let Some(head) = messages.last().copied() else {
            self.attachments.clear();
            self.remote_button = None;
            self.html_view_rect = None;
            self.bodies.clear();
            self.headers.clear();
            self.active = None;
            self.paint_empty(&mut ctx, &state);
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
            self.bodies
                .retain(|id, _| messages.iter().any(|message| message.id == *id));
        }
        let spacing = theme.tokens.spacing;
        let gutter = f64::from(spacing.s6);
        let r = self.rect;
        let head_html = head.html_body.clone();
        let head_sender = head.from.email.to_ascii_lowercase();
        // An HTML body takes the rest of the reader, so the thread above it
        // does not scroll.
        if head_html.is_some() && self.html_engine_live {
            self.region.set_offset(Vec2::ZERO);
        }
        let scroll = self.region.offset().y;
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
        let count = messages.len();
        self.headers.clear();
        for (index, message) in messages.iter().enumerate() {
            let is_head = index + 1 == count;
            let open = is_head || self.expanded.contains(&message.id);
            let top = flow.y;
            flow.sender(message);
            let header_bottom = flow.y;
            flow.y += f64::from(spacing.s3);
            if is_head && head_html.is_some() && self.html_engine_live {
                self.paint_remote_line(&mut flow);
                flow.y += f64::from(spacing.s3);
                let bottom = r.y1 - gutter;
                if bottom > flow.y + f64::from(spacing.s12) {
                    webview_rect = Some(Rect::new(flow.x, flow.y, flow.x + flow.width, bottom));
                }
                flow.y = bottom;
            } else if open {
                // Without an isolated renderer the HTML is read into styled
                // text; nothing in it is fetched. An earlier message opened
                // in the conversation reads the same way.
                self.paint_body(&mut flow, message, message.html_body.as_deref(), presence);
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
        // An HTML body fills the reader; it scrolls inside its own view.
        let content_h = if webview_rect.is_some() {
            r.height()
        } else {
            flow.y + scroll - r.y0
        };
        let attachments = std::mem::take(&mut flow.attachments);
        flow.ctx.scene.pop_layer();
        self.attachments = attachments;
        self.region.set_content_size(r.width(), content_h);
        self.region.paint_bars(flow.ctx.scene, t, theme);
        drop(flow);
        drop(state);

        // The HTML body, now that the shared borrow is released.
        self.html_view_rect = None;
        match (webview_rect, head_html) {
            (Some(rect), Some(html)) => {
                if self.html_shown_id != Some(head_id) {
                    self.html_load_remote = shared.borrow().remote_senders.contains(&head_sender);
                    self.html_sender = head_sender;
                    self.html_shown_id = Some(head_id);
                }
                self.load_html_body(head_id, &html, theme);
                let view = self.ensure_html_view();
                view.set_viewport(rect, 1.0);
                view.draw(scene, t, tr, theme, presence);
                self.html_view_rect = Some(rect);
                self.intercept_navigation();
            }
            (None, Some(_)) => {}
            (_, None) => {
                // Plain text this frame: drop a stale load so returning to
                // an HTML mail reloads it, blocked again.
                self.html_key = None;
                self.html_shown_id = None;
            }
        }
    }

    fn press(&mut self, point: Point) -> bool {
        if self.remote_button.is_some_and(|rect| rect.contains(point)) {
            self.html_load_remote = !self.html_load_remote;
            let mut state = self.shared.borrow_mut();
            if self.html_load_remote {
                state.remote_senders.insert(self.html_sender.clone());
            } else {
                state.remote_senders.remove(&self.html_sender);
            }
            drop(state);
            // Reload with the new policy on the next draw.
            self.html_key = None;
            self.dirty = true;
            self.shared.borrow_mut().dirty = true;
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
            .attachments
            .iter()
            .find(|(rect, _, _)| rect.contains(point))
            .copied()
        {
            self.shared
                .borrow_mut()
                .push_intent(Intent::SaveAttachment(message, index));
            return true;
        }
        false
    }

    /// A mail never navigates its own view: a link the person follows opens in
    /// the browser, and the message is shown again.
    fn intercept_navigation(&mut self) {
        let Some(engine) = self.html_view.as_ref().map(|view| view.engine().clone()) else {
            return;
        };
        let mut left = false;
        for (url, _) in engine.take_new_tab_requests() {
            open_link(&url);
        }
        for event in engine.drain_events() {
            if let ui::webview::WebViewEvent::UriChanged(uri) = event
                && !uri.is_empty()
                && !uri.starts_with("data:")
                && uri != "about:blank"
            {
                open_link(&uri);
                left = true;
            }
        }
        if left {
            self.html_key = None;
            self.dirty = true;
        }
    }

    /// Give the message text the pointer and the keys: selecting, copying and
    /// opening its links. The wheel stays the reader's, which scrolls.
    ///
    /// A press on another message's text moves the selection and the keys
    /// there: one message's text is active at a time.
    fn forward_to_body(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if matches!(event, WindowEvent::MouseWheel { .. }) {
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

    /// What the pointer at `point` lights up: an attachment chip, or the
    /// remote-content button.
    fn hover_at(&self, point: Point) -> (Option<usize>, bool, Option<usize>) {
        (
            self.attachments
                .iter()
                .position(|(rect, _, _)| rect.contains(point)),
            self.remote_button.is_some_and(|rect| rect.contains(point)),
            self.quote_buttons
                .iter()
                .position(|(rect, _)| rect.contains(point)),
        )
    }

    fn forward_to_html(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let over = matches!(
            event,
            WindowEvent::MouseWheel { .. } | WindowEvent::CursorMoved { .. }
        ) && self.html_view_rect.is_some();
        if over && let Some(view) = self.html_view.as_mut() {
            view.handle_event(event, ctx);
        }
    }
}

impl Widget for MessageReader {
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
        if self.shared.borrow().bulk_selecting {
            self.attachments.clear();
            self.remote_button = None;
            self.html_view_rect = None;
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
        self.forward_to_html(event, ctx);
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
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let before = self.hover_at(self.pointer);
                self.pointer = ctx.logical(position);
                if self.hover_at(self.pointer) != before {
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.rect.contains(self.pointer) && self.press(self.pointer) => {
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "MailReader"
    }

    /// The reader takes the keyboard while a message is open, so its text
    /// can be selected and copied from the keys.
    fn is_focusable(&self) -> bool {
        !self.bodies.is_empty()
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if let Some(body) = self.active.and_then(|id| self.bodies.get_mut(&id)) {
            body.input.set_focus(focused);
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

    fn tooltip(&self) -> Option<&str> {
        let point = self.pointer;
        if self
            .attachments
            .iter()
            .any(|(rect, _, _)| rect.contains(point))
        {
            return Some("Save attachment");
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
            || self
                .html_view
                .as_ref()
                .is_some_and(ui::Widget::needs_animation_frame)
            || self
                .bodies
                .values()
                .any(|body| body.input.needs_animation_frame())
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
        if let Some(view) = self.html_view.as_mut() {
            view.mark_drawn();
        }
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
        MailState::shared(MailState::new_sample(crate::app::avatar_store_offline()))
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

    #[test]
    fn html_mail_renders_sandboxed_with_remote_content_blocked_until_allowed() {
        let shared = shared();
        let id = message_from(&shared, "hello@fieldnotes.example");
        let mut reader = reader_for(&shared, id, 960.0, 820.0);
        let engine = TestEngine::new();
        reader.set_html_engine(engine.clone());
        draw(&mut reader);
        assert_eq!(
            reader.html_loaded_key(),
            Some((id, true)),
            "the HTML body loads with remote content blocked"
        );
        let commands = engine.commands();
        let url = commands
            .iter()
            .find_map(|c| match c {
                WebViewCommand::Navigate(url) => Some(url.clone()),
                _ => None,
            })
            .expect("the reader navigates the HTML body");
        assert!(
            url.starts_with("data:text/html"),
            "a sandboxed data: document, never a remote URL: {url}"
        );
        assert!(
            commands
                .iter()
                .any(|c| matches!(c, WebViewCommand::SetContentFilter(Some(_)))),
            "a remote-blocking content filter is installed"
        );

        // Loading images reloads the body unblocked.
        let button = reader.remote_button().expect("the remote-content button");
        assert!(reader.press(button.center()));
        draw(&mut reader);
        assert_eq!(reader.html_loaded_key(), Some((id, false)));
        assert!(
            shared
                .borrow()
                .remote_senders
                .contains("hello@fieldnotes.example"),
            "the permission is the person's preference, not the reader's memory"
        );

        // A plain message skips the webview; the allowed sender stays allowed.
        let plain = message_from(&shared, "lena@fischer.example");
        let mut doc = crate::resource::TestMail::doc();
        shared
            .borrow_mut()
            .open_message(&mut ui::widget::Edits::lend(&mut doc), plain);
        draw(&mut reader);
        assert!(
            reader.html_loaded_key().is_none(),
            "plain text skips the webview"
        );
        assert!(reader.remote_button().is_none());
        shared
            .borrow_mut()
            .open_message(&mut ui::widget::Edits::lend(&mut doc), id);
        draw(&mut reader);
        assert_eq!(
            reader.html_loaded_key(),
            Some((id, false)),
            "the sender stays allowed for the session"
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
            reader.html_loaded_key().is_none(),
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
    fn an_attachment_chip_asks_the_app_to_save_through_the_portal() {
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
                Intent::SaveAttachment(found, found_index) if *found == id && *found_index == index
            )),
            "the reader hands the attachment's address to the app, which owns the portal"
        );
    }

    #[test]
    fn a_refetched_message_is_one_message_in_the_conversation() {
        let shared = MailState::shared(MailState::new_live(
            crate::account::AccountStore::new(),
            crate::app::avatar_store_offline(),
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
            crate::app::avatar_store_offline(),
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
            crate::app::avatar_store_offline(),
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
            crate::app::avatar_store_offline(),
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
