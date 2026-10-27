//! The thread panel: an open message's replies under their parent, the panel
//! header that names and closes it, and how each reply is measured and
//! painted in it.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::Widget;
use ui::chrome::nav_row_height;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{AvatarSize, Button, ButtonFace, ControlSize};

use super::agent_work::measure_agent_work;
use super::{clock_label, composer_editor_width, square};
use crate::app::{ChatApp, Hit};
use crate::model;
use crate::model::Message;
use crate::paint::{line_in, line_of, push_clip, text, width_of};

impl ChatApp {
    pub(crate) fn draw_thread(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let Some(parent_id) = self.thread_open.clone() else {
            return;
        };
        let panel = self.thread_rect();
        let width =
            composer_editor_width(panel.width() - 2.0 * self.chrome.composer_pad, theme, true);
        self.composer_thread
            .measure(width, f32::INFINITY, theme, tr);
        let comp = self.thread_composer_rect(panel);
        let head = self.draw_thread_header(scene, tf, tr, theme, panel, hits);
        let body = Rect::new(
            panel.x0,
            head.y1,
            panel.x1,
            comp.y0 - f64::from(theme.tokens.spacing.s2),
        );
        self.draw_thread_scroller(PaintCtx::new(scene, tf, tr, theme), body, &parent_id, hits);
        if !self.can_participate() {
            self.composer_thread.set_focus(false);
            Self::draw_read_only_composer(scene, tf, tr, theme, comp);
            return;
        }
        let focused = self.is_focused()
            && matches!(self.focus, crate::app::Focus::Thread)
            && self.editing.is_none();
        let draft = self.composer_thread.value();
        let send_enabled = !draft.trim().is_empty();
        let inner = self.draw_input_box(
            PaintCtx::new(scene, tf, tr, theme),
            comp,
            &draft,
            (send_enabled, false, true),
            Hit::FocusThread,
            hits,
        );
        self.composer_thread
            .set_mention_paint(self.mention_paint(theme));
        self.composer_thread
            .set_mention_people(self.mention_people());
        self.composer_thread.set_popover_bounds(self.rect);
        self.composer_thread.layout(inner, theme, tr);
        self.composer_thread.set_focus(focused);
        self.composer_thread.draw(scene, tf, tr, theme, presence);
        if let Some(draft) = self.thread_draft_key() {
            let key = ChatApp::draft_presence_key(&draft);
            Self::draw_draft_presence(&self.composer_thread, &key, scene, tf, tr, theme, presence);
        }
    }

    /// Fixed navigation stays reachable even when a provider answer fills the thread.
    fn draw_thread_scroller(
        &mut self,
        paint: PaintCtx<'_>,
        body: Rect,
        parent_id: &str,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let gap = f64::from(theme.tokens.spacing.s2);
        let height = ButtonFace::height(ControlSize::Regular, theme);
        let mut controls = vec![
            (
                crate::i18n::t("chat-thread-start"),
                Hit::ThreadScroll(false),
            ),
            (crate::i18n::t("chat-thread-end"), Hit::ThreadScroll(true)),
        ];
        if let Some(message) = self.messages.iter().rev().find(|message| {
            (message.id == parent_id || message.parent.as_deref() == Some(parent_id))
                && message
                    .work
                    .as_ref()
                    .and_then(|work| work.stream.as_ref())
                    .is_some_and(|stream| !stream.reasoning.is_empty())
        }) {
            controls.push((
                crate::i18n::t(if self.expanded_reasoning.contains(&message.id) {
                    "chat-agent-hide-reasoning"
                } else {
                    "chat-agent-show-reasoning"
                }),
                Hit::AgentReasoning(message.id.clone()),
            ));
        }
        let width = (body.width() - gap * (controls.len() as f64 + 1.0)) / controls.len() as f64;
        for (index, (caption, hit)) in controls.into_iter().enumerate() {
            let x = body.x0 + gap + index as f64 * (width + gap);
            let rect = Rect::new(x, body.y0 + gap, x + width, body.y0 + gap + height);
            Button::paint(scene, tf, tr, theme, &ButtonFace::new(rect, &caption));
            Self::push_hit(hits, rect, hit);
        }
        self.draw_thread_body(
            PaintCtx::new(scene, tf, tr, theme),
            Rect::new(
                body.x0,
                (body.y0 + height + 2.0 * gap).min(body.y1),
                body.x1,
                body.y1,
            ),
            parent_id,
            hits,
        );
    }

    /// A detail panel's part of the header row: its title, and its close
    /// control. Returns the header band.
    pub(super) fn draw_panel_header(
        &self,
        paint: PaintCtx<'_>,
        panel: Rect,
        title: &str,
        close_hit: Hit,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> Rect {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let sp = &theme.tokens.spacing;
        let head = Rect::new(
            panel.x0,
            panel.y0,
            panel.x1,
            panel.y0 + self.chrome.header_h,
        );
        let target = ButtonFace::height(ControlSize::Regular, theme);
        let close_label = if matches!(&close_hit, Hit::CloseThread) {
            crate::i18n::t("chat-thread-close")
        } else {
            crate::i18n::t("chat-close")
        };
        let title_x = head.x0 + f64::from(sp.s4);
        let close = square(
            Point::new(head.x1 - f64::from(sp.s3) - target / 2.0, head.center().y),
            target,
        );
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &ButtonFace::icon_only(close, Icon::X, &close_label).hovered(self.hovered(close)),
        );
        ChatApp::push_hit(hits, close, close_hit);
        let right = close.x0 - f64::from(sp.s2);
        let mut ctx = PaintCtx::new(scene, tf, tr, theme);
        line_in(
            &mut ctx,
            title,
            &theme.text(TextRole::Heading),
            theme.text_primary,
            title_x,
            head,
            right - title_x,
        );
        ui::surface::separator(
            scene,
            tf,
            head.x0,
            head.x1,
            head.y1,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
        head
    }

    /// The open thread's title: its agent work's state, else Thread.
    pub(crate) fn thread_title(&self) -> String {
        self.messages
            .iter()
            .rev()
            .filter(|message| {
                self.thread_open.as_deref().is_some_and(|parent| {
                    message.id == parent || message.parent.as_deref() == Some(parent)
                })
            })
            .find_map(|message| message.work.as_ref()?.stream.as_ref())
            .map_or_else(
                || crate::i18n::t("chat-thread-title"),
                |stream| crate::i18n::t(stream.status_key()),
            )
    }

    /// The thread panel's header; a thread that fills the window has none:
    /// the window bar leads it with Back and names it.
    fn draw_thread_header(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> Rect {
        if self.thread_is_page() {
            return Rect::new(panel.x0, panel.y0, panel.x1, panel.y0);
        }
        self.draw_panel_header(
            PaintCtx::new(scene, tf, tr, theme),
            panel,
            &self.thread_title(),
            Hit::CloseThread,
            hits,
        )
    }

    /// The thread: the parent message, a "N replies" divider, then every
    /// reply in order, clipped between the header and the thread composer.
    fn draw_thread_body(
        &mut self,
        paint: PaintCtx<'_>,
        body: Rect,
        parent_id: &str,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let sp = &theme.tokens.spacing;
        let first_hit = hits.len();
        push_clip(scene, tf, body);
        let gutter = f64::from(sp.s4);
        let column = Rect::new(body.x0 + gutter, body.y0, body.x1 - gutter, body.y1);
        let tx = column.x0 + AvatarSize::Small.side(theme) + f64::from(sp.s2);
        let tw = ui::num::f32_of_f64((column.x1 - tx).max(f64::from(sp.s16)));
        let parent = self.message(parent_id).cloned();
        let replies: Vec<Message> = self
            .messages
            .iter()
            .filter(|message| message.parent.as_deref() == Some(parent_id))
            .cloned()
            .collect();
        let mut content = 2.0 * f64::from(sp.s4);
        for message in parent.iter().chain(replies.iter()) {
            content += self.measure_thread_message(message, tw, theme, tr);
        }
        if parent.is_some() {
            content += nav_row_height(theme);
        }
        self.thread_body = body;
        self.thread_scroll.set_viewport_rect(body);
        self.thread_scroll.set_content_size(
            ui::num::f32_of_f64(body.width()),
            ui::num::f32_of_f64(content),
        );
        if self.thread_follow {
            self.thread_scroll
                .set_offset_y(self.thread_scroll.max_offset_y());
        }
        let mut y = body.y0 + f64::from(sp.s4) - f64::from(self.thread_scroll.offset_y());
        if let Some(parent) = parent {
            y = self.draw_thread_message(
                PaintCtx::new(scene, tf, tr, theme),
                &parent,
                column,
                y,
                hits,
            );
            let n = model::reply_count(&self.messages, parent_id);
            let label = crate::i18n::t_args("chat-thread-replies", &[("count", n.into())]);
            let style = theme.text(TextRole::Caption);
            let w = width_of(tr, &label, &style);
            let row_h = nav_row_height(theme);
            let band = Rect::new(column.x0, y, column.x1, y + row_h);
            let ground = theme.tokens.colors.surfaces.content;
            ui::surface::separator(
                scene,
                tf,
                band.x0 + w + f64::from(sp.s3),
                band.x1,
                band.center().y,
                0.0,
                ground,
            );
            let mut ctx = PaintCtx::new(scene, tf, tr, theme);
            line_in(
                &mut ctx,
                &label,
                &style,
                theme.text_dim,
                band.x0,
                band,
                w + 1.0,
            );
            y = band.y1;
        }
        for r in &replies {
            y = self.draw_thread_message(PaintCtx::new(scene, tf, tr, theme), r, column, y, hits);
        }
        scene.pop_layer();
        for (rect, _) in hits.iter_mut().skip(first_hit) {
            *rect = rect.intersect(body);
        }
        hits.retain(|(rect, _)| rect.width() > 0.0 && rect.height() > 0.0);
        self.thread_scroll.draw_bar_overlay(scene, tf, theme);
    }

    /// Uses the same flow heights as the thread painter, including wrapped
    /// durable evidence, so the shared scroll service can reach every reply.
    fn measure_thread_message(
        &self,
        message: &Message,
        width: f32,
        theme: &Theme,
        tr: &mut TextRenderer,
    ) -> f64 {
        let sp = &theme.tokens.spacing;
        let name_height = tr
            .measure_style(
                &self.user_name(&message.author),
                None,
                &theme.text(TextRole::Label).strong(),
            )
            .1;
        let mut height =
            f64::from(name_height).max(AvatarSize::Small.side(theme)) + f64::from(sp.s1);
        if !message.text.is_empty()
            || message
                .work
                .as_ref()
                .and_then(|work| work.stream.as_ref())
                .is_some()
        {
            height += self.layout_body(message, Point::ZERO, width, theme, tr) + f64::from(sp.s1);
        }
        if message
            .work
            .as_ref()
            .and_then(|work| work.stream.as_ref())
            .is_some_and(|stream| !stream.reasoning.is_empty())
        {
            height += ButtonFace::height(ControlSize::Regular, theme) + f64::from(sp.s2);
        }
        if let Some(work) = &message.work {
            height += measure_agent_work(tr, theme, work, width);
        }
        if let Some(attachment) = &message.attachment {
            height += self.attachment_flow_height(attachment, f64::from(width), tr, theme);
        }
        if !message.reactions.is_empty() {
            height += ui::widget::reaction_strip::measure_height(
                &message.reactions,
                self.can_participate(),
                f64::from(width),
                tr,
                theme,
            ) + f64::from(sp.s2);
        }
        height += f64::from(sp.s3);
        height
    }

    /// One message in a thread: avatar, author and time, body, attachment and
    /// reactions, on one left edge. Returns the y after it.
    fn draw_thread_message(
        &self,
        paint: PaintCtx<'_>,
        m: &Message,
        column: Rect,
        y: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let sp = &theme.tokens.spacing;
        let side = AvatarSize::Small.side(theme);
        let tx = column.x0 + side + f64::from(sp.s2);
        let tw = (column.x1 - tx).max(f64::from(theme.tokens.spacing.s16)) as f32;
        let mut ctx = PaintCtx::new(scene, tf, tr, theme);
        let (end_x, nh, _) = self.paint_author(
            &mut ctx,
            m,
            Rect::new(column.x0, y, column.x0 + side, y + side),
            Point::new(tx, y),
            column.x1,
        );
        let time_style = theme.text(TextRole::Caption).numeric();
        let th = line_of(ctx.text, &time_style);
        text(
            &mut ctx,
            &clock_label(m.ts),
            &time_style,
            theme.text_dim,
            Point::new(end_x + f64::from(sp.s2), y + nh - th),
            None,
        );
        let mut yy = y + nh.max(side) + f64::from(sp.s1);
        if !m.text.is_empty()
            || m.work
                .as_ref()
                .and_then(|work| work.stream.as_ref())
                .is_some()
        {
            let body_h = self.layout_body(m, Point::new(tx, yy), tw, theme, tr);
            self.draw_body(m, &mut PaintCtx::new(scene, tf, tr, theme));
            yy += body_h + f64::from(sp.s1);
        }
        if m.work
            .as_ref()
            .and_then(|work| work.stream.as_ref())
            .is_some_and(|stream| !stream.reasoning.is_empty())
        {
            let height = ButtonFace::height(ControlSize::Regular, theme);
            let caption = crate::i18n::t(if self.expanded_reasoning.contains(&m.id) {
                "chat-agent-hide-reasoning"
            } else {
                "chat-agent-show-reasoning"
            });
            let rect = Rect::new(tx, yy, tx + f64::from(tw), yy + height);
            Button::paint(scene, tf, tr, theme, &ButtonFace::new(rect, &caption));
            Self::push_hit(hits, rect, Hit::AgentReasoning(m.id.clone()));
            yy += height + f64::from(sp.s2);
        }
        if let Some(work) = &m.work {
            let height = measure_agent_work(tr, theme, work, tw);
            Self::draw_agent_work(
                PaintCtx::new(scene, tf, tr, theme),
                work,
                Rect::new(tx, yy, tx + f64::from(tw), yy + height),
            );
            yy += height;
        }
        if let Some(att) = &m.attachment {
            self.draw_attachment(
                PaintCtx::new(scene, tf, tr, theme),
                att,
                Point::new(tx, yy),
                f64::from(tw),
            );
            yy += self.attachment_flow_height(att, f64::from(tw), tr, theme);
        }
        if !m.reactions.is_empty() {
            yy += self.draw_reactions(
                PaintCtx::new(scene, tf, tr, theme),
                m,
                (true, Point::new(tx, yy), f64::from(tw)),
                hits,
            ) + f64::from(sp.s2);
        }
        yy + f64::from(sp.s3)
    }
}
