//! The open conversation: its subject, then each message with its sender,
//! date, recipients, body and attachments, newest last.
//!
//! A message is a document, so the reader paints it, reading every value from
//! the theme; the controls inside it (attachment chips, the remote-content
//! button) are the toolkit's faces.

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::webview::{SharedEngine, WebViewCommand};
use ui::widget::{
    AvatarFace, AvatarSize, Button, ButtonFace, ButtonRole, ChipFace, ChipLead, ControlSize,
    EmptyState, EmptyStateContent, ScrollMetrics, Scrollbar, TaffyTreeExt, WebView, Widget,
};
use ui::{TextKind, TextRole};

use crate::model::{Message, MessageId};
use crate::state::{Intent, MailState, Shared};
use crate::util::{format_absolute, gravatar_url, local_minutes};

/// The web engine backing HTML-mail rendering. Under the `html-webview`
/// feature this is the real WPE `WebKit` engine (the same one the browser
/// uses), falling back to the hermetic test engine if it can't start;
/// otherwise it is the deterministic test engine, which still exercises the
/// full sandbox and remote-block plumbing in the default build.
fn new_html_engine() -> (SharedEngine, bool) {
    #[cfg(feature = "html-webview")]
    {
        match ui::webview::wpe::WpeEngine::new() {
            Ok(e) => return (e, true),
            Err(err) => tracing::warn!(%err, "mail: WPE engine unavailable; using test engine"),
        }
    }
    let e: SharedEngine = ui::webview::test_engine::TestEngine::new();
    (e, false)
}

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

    /// The message text, paragraph by paragraph; quoted lines are quieter.
    fn body(&mut self, body: &str) {
        let style = self.style(TextRole::Body);
        let blank = self.line(TextRole::Body) / 2.0;
        for paragraph in body.split('\n') {
            if paragraph.trim().is_empty() {
                self.y += blank;
                continue;
            }
            let quote = paragraph.trim_start().starts_with('>');
            self.paragraph(paragraph, &style, quote);
        }
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

pub struct MessageReader {
    shared: Shared,
    rect: Rect,
    scroll: f64,
    content_h: f64,
    pointer: Point,
    /// One line of body text, the step a wheel notch scrolls.
    line: f64,
    scrollbar: Scrollbar,
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
    /// The message the HTML view shows, so remote content is blocked again
    /// for each message.
    html_shown_id: Option<MessageId>,
    /// The person allowed remote images for the open message.
    html_load_remote: bool,
    html_view_rect: Option<Rect>,
    dirty: bool,
}

impl MessageReader {
    pub fn new(shared: Shared) -> Self {
        Self {
            shared,
            rect: Rect::ZERO,
            scroll: 0.0,
            content_h: 0.0,
            pointer: Point::new(-1.0, -1.0),
            line: 0.0,
            scrollbar: Scrollbar::vertical(),
            attachments: Vec::new(),
            remote_button: None,
            shown: None,
            html_engine: None,
            html_engine_live: false,
            html_view: None,
            html_key: None,
            html_shown_id: None,
            html_load_remote: false,
            html_view_rect: None,
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
    /// exact `(message, block state)` is loaded already.
    fn load_html_body(&mut self, id: MessageId, html: &str) {
        let block_remote = !self.html_load_remote;
        let key = (id, block_remote);
        if self.html_key == Some(key) {
            return;
        }
        let url = crate::html_mail::data_url(html, block_remote);
        let filter = std::sync::Arc::new(crate::html_mail::content_filter_json(block_remote));
        let engine = self.ensure_html_view().engine().clone();
        engine.send(WebViewCommand::SetContentFilter(Some(filter)));
        engine.send(WebViewCommand::Navigate(url));
        self.html_key = Some(key);
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

    fn clamp_scroll(&mut self) {
        let max = (self.content_h - self.rect.height()).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    fn paint_empty(&self, ctx: &mut PaintCtx<'_>, state: &MailState) {
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

    /// Paint the remote-content line for an HTML body at the flow's cursor:
    /// what is blocked and the button that changes it.
    fn paint_remote_line(&mut self, flow: &mut Flow<'_, '_>, interactive: bool) {
        let theme = flow.ctx.theme;
        let caption = theme.text(TextRole::Caption);
        let size = ControlSize::Small;
        let button_h = ButtonFace::height(size, theme);
        let (text, button) = if !interactive {
            ("Showing the plain text of this message.", None)
        } else if self.html_load_remote {
            ("Remote images are shown.", Some("Block images"))
        } else {
            (
                "Remote images are blocked to protect your privacy.",
                Some("Load images"),
            )
        };
        let row_h = button_h.max(flow.line(TextRole::Caption));
        let mut text_w = flow.width;
        self.remote_button = None;
        if let Some(label) = button {
            let probe = ButtonFace::new(Rect::ZERO, label).control_size(size);
            let w = probe.intrinsic_width(flow.ctx.text, theme);
            let rect = Rect::from_center_size(
                Point::new(flow.x + flow.width - w / 2.0, flow.y + row_h / 2.0),
                (w, button_h),
            );
            Button::paint(
                flow.ctx.scene,
                flow.ctx.transform,
                flow.ctx.text,
                theme,
                &ButtonFace::new(rect, label)
                    .role(ButtonRole::Secondary)
                    .control_size(size)
                    .hovered(rect.contains(flow.pointer)),
            );
            self.remote_button = Some(rect);
            text_w -= w + f64::from(theme.tokens.spacing.s3);
        }
        let caption_h = flow.line(TextRole::Caption);
        flow.line_at(
            text,
            &caption,
            theme.text_dim,
            Point::new(flow.x, flow.y + (row_h - caption_h) / 2.0),
            text_w,
        );
        flow.y += row_h;
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
        let messages = state.selected_thread_messages();
        let Some(head) = messages.last().copied() else {
            self.attachments.clear();
            self.remote_button = None;
            self.html_view_rect = None;
            self.paint_empty(&mut ctx, &state);
            return;
        };
        let head_id = head.id;
        if self.shown != Some(head_id) {
            self.shown = Some(head_id);
            self.scroll = 0.0;
        }
        let spacing = theme.tokens.spacing;
        let gutter = f64::from(spacing.s6);
        let r = self.rect;
        self.line = ctx.line_box(TextRole::Body);
        let head_html = head.html_body.clone();
        // An HTML body takes the rest of the reader, so the thread above it
        // does not scroll.
        if head_html.is_some() {
            self.scroll = 0.0;
        }
        ctx.scene
            .push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &r);
        let mut flow = Flow {
            ctx,
            state: &state,
            x: r.x0 + gutter,
            width: (r.width() - 2.0 * gutter).max(1.0),
            y: r.y0 + gutter - self.scroll,
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
        if messages.len() > 1 {
            flow.y += f64::from(spacing.s1);
            let caption = flow.style(TextRole::Caption);
            flow.paragraph(&format!("{} messages", messages.len()), &caption, true);
        }
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
                "Previewing: the read state is unchanged."
            } else {
                "Previewing: this message stays unread."
            };
            let x = flow.x + icon + f64::from(theme.tokens.metrics.icon_label_gap);
            let width = flow.width - (x - flow.x);
            flow.line_at(text, &caption, theme.text_dim, Point::new(x, top), width);
            flow.y += line;
        }
        flow.y += f64::from(spacing.s6);

        let mut webview_rect = None;
        let count = messages.len();
        for (index, message) in messages.iter().enumerate() {
            let is_head = index + 1 == count;
            flow.sender(message);
            flow.y += f64::from(spacing.s3);
            if is_head && head_html.is_some() {
                let interactive = self.html_engine_live;
                self.paint_remote_line(&mut flow, interactive);
                flow.y += f64::from(spacing.s3);
                if interactive {
                    let bottom = r.y1 - gutter;
                    if bottom > flow.y + f64::from(spacing.s12) {
                        webview_rect = Some(Rect::new(flow.x, flow.y, flow.x + flow.width, bottom));
                    }
                    flow.y = bottom;
                } else {
                    // Without an isolated renderer the decoded text
                    // alternative is shown, never the test engine's frame.
                    flow.body(&message.body);
                }
            } else if is_head {
                flow.body(&message.body);
            } else {
                // Earlier messages show their first line.
                let caption = flow.style(TextRole::Caption);
                let at = Point::new(flow.x, flow.y);
                let width = flow.width;
                flow.line_at(&message.snippet(), &caption, theme.text_dim, at, width);
                flow.y += flow.line(TextRole::Caption);
            }
            if is_head && message.has_attachments() {
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
            flow.y + self.scroll - r.y0
        };
        let attachments = std::mem::take(&mut flow.attachments);
        flow.ctx.scene.pop_layer();
        self.attachments = attachments;
        self.content_h = content_h;
        let metrics = ScrollMetrics::new(self.content_h, r.height(), self.scroll);
        if metrics.overflows() {
            let gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
            let track = Rect::new(r.x1 - gutter, r.y0, r.x1, r.y1);
            self.scrollbar
                .draw(flow.ctx.scene, t, track, metrics, theme, false);
        }
        drop(flow);
        drop(state);

        // The HTML body, now that the shared borrow is released.
        self.html_view_rect = None;
        match (webview_rect, head_html) {
            (Some(rect), Some(html)) => {
                if self.html_shown_id != Some(head_id) {
                    self.html_load_remote = false;
                    self.html_shown_id = Some(head_id);
                }
                self.load_html_body(head_id, &html);
                let view = self.ensure_html_view();
                view.set_viewport(rect, 1.0);
                view.draw(scene, t, tr, theme, presence);
                self.html_view_rect = Some(rect);
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
            // Reload with the new policy on the next draw.
            self.html_key = None;
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
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        });
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
        self.clamp_scroll();
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
            let shared = self.shared.clone();
            let state = shared.borrow();
            self.paint_empty(&mut PaintCtx::new(scene, t, tr, theme), &state);
            return;
        }
        self.clamp_scroll();
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
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.dirty = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.rect.contains(self.pointer) && self.press(self.pointer) => {
                *ctx.handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } if self.rect.contains(self.pointer) => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * self.line,
                    MouseScrollDelta::PixelDelta(p) => p.y / ctx.scale_factor,
                };
                self.scroll -= dy;
                self.clamp_scroll();
                self.dirty = true;
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "MailReader"
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
        self.html_view
            .as_ref()
            .is_some_and(ui::Widget::needs_animation_frame)
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        if let Some(view) = self.html_view.as_mut() {
            view.mark_drawn();
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
        let mut doc = automerge::AutoCommit::new();
        shared.borrow_mut().select_message(&mut doc, id);
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

        // Another message blocks remote content again.
        let plain = message_from(&shared, "lena@fischer.example");
        let mut doc = automerge::AutoCommit::new();
        shared.borrow_mut().select_message(&mut doc, plain);
        draw(&mut reader);
        assert!(
            reader.html_loaded_key().is_none(),
            "plain text skips the webview"
        );
        assert!(reader.remote_button().is_none());
        shared.borrow_mut().select_message(&mut doc, id);
        draw(&mut reader);
        assert_eq!(reader.html_loaded_key(), Some((id, true)));
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
}
