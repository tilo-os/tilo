//! The conversation's timeline: the grouped message rows the scroller holds,
//! what each row says and offers — its author, body, reactions, thread
//! affordance and menu — and the attachments it carries.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::Widget;
use ui::chrome::{self, SelectionState};
use ui::format::size as format_bytes;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::conversation::{ConversationClock, DayBreaks, DayDividerFace};
use ui::widget::{
    AvatarFace, AvatarSize, Button, ButtonFace, ButtonRole, CardFace, ChipFace, ControlSize,
    EmptyState, EmptyStateContent, MediaArt,
};

use super::agent_work::{
    AgentWorkLineStyle, AgentWorkPresentation, draw_agent_work_line, measure_agent_work,
};
use super::{
    MessageColumn, RowM, TimelineItem, attachment_icon, clock_label, file_card_height,
    header_row_h, media_face, media_meta, paint_field, paint_file_card, thread_bar_h,
};
use crate::app::{ChatApp, Hit};
use crate::model::{self, AgentWork, Attachment, Message};
use crate::paint::{line_in, line_of, text, width_of};

impl ChatApp {
    pub(super) fn draw_person_messages(
        &mut self,
        mut paint: PaintCtx<'_>,
        presence: &PresenceCtx,
        area: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let gap = f64::from(paint.theme.tokens.spacing.s3);
        let width = (area.width() - 2.0 * self.chrome.gutter).max(1.0);
        let style = paint.theme.text(TextRole::Caption);
        let heading_h = self.chrome.body_line;
        let mut heights = Vec::new();
        for message in &self.person_messages {
            let (previous, editor) = self
                .person_body_views
                .entry(message.id.clone())
                .or_insert_with(|| (String::new(), crate::composer::MessageEditor::local()));
            if previous != &message.text {
                editor.set_text(&message.text);
                previous.clone_from(&message.text);
            }
            heights.push(
                editor.measure(width, f32::INFINITY, paint.theme, paint.text) + heading_h + gap,
            );
        }
        let total: f64 = heights.iter().sum();
        self.scroll_content = total;
        self.scroll_viewport = area.height();
        let overflow = (total - area.height()).max(0.0);
        self.scroll = self.scroll.clamp(0.0, overflow);
        let mut y = area.y0 - overflow + self.scroll;
        if let Some(error) = &self.person_error {
            let color = paint.theme.text_dim;
            line_in(
                &mut paint,
                error,
                &style,
                color,
                area.x0 + self.chrome.gutter,
                area,
                width,
            );
            return;
        }
        if self.person_messages.is_empty() {
            let label = crate::i18n::t("chat-person-chat-empty-conversation");
            let color = paint.theme.text_dim;
            line_in(
                &mut paint,
                &label,
                &style,
                color,
                area.x0 + self.chrome.gutter,
                area,
                width,
            );
        }
        for (message, height) in self.person_messages.iter().zip(heights) {
            let author = if Some(&message.author) == self.direct_person.as_ref() {
                self.person_display_name(&message.author)
            } else {
                self.me.name.clone()
            };
            let heading = Rect::new(
                area.x0 + self.chrome.gutter,
                y,
                area.x1 - self.chrome.gutter,
                y + heading_h,
            );
            let color = paint.theme.text_dim;
            let shown = line_in(
                &mut paint, &author, &style, color, heading.x0, heading, width,
            );
            // Each line's author opens their card, as in a channel.
            if heading.y1 > area.y0 && heading.y0 < area.y1 {
                ChatApp::push_hit(
                    hits,
                    Rect::new(heading.x0, heading.y0, heading.x0 + shown, heading.y1),
                    Hit::ShowPerson {
                        person: message.author.clone(),
                        place: message.id.clone(),
                    },
                );
            }
            if let Some((_, editor)) = self.person_body_views.get_mut(&message.id) {
                let body = Rect::new(heading.x0, heading.y1, heading.x1, y + height - gap);
                editor.layout(body, paint.theme, paint.text);
                editor.draw(
                    paint.scene,
                    paint.transform,
                    paint.text,
                    paint.theme,
                    presence,
                );
            }
            y += height;
        }
    }

    fn draw_empty_messages(&mut self, paint: PaintCtx<'_>, area: Rect) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        self.scroll_content = 0.0;
        self.scroll_viewport = area.height().max(0.0);
        self.scroll = 0.0;
        let title = crate::i18n::t(if self.can_participate() {
            "chat-empty-title"
        } else {
            "chat-empty-title-read-only"
        });
        // An empty channel says what it is for, or invites the first word:
        // a visitor arriving in a quiet room should know they may start it.
        let channel_message = self.empty_channel_message();
        let content = EmptyStateContent {
            icon: Some(Icon::Send),
            message: channel_message.as_deref(),
            ..EmptyStateContent::new(&title)
        };
        EmptyState::paint(
            &mut PaintCtx::new(scene, tf, tr, theme),
            area,
            &content,
            self.last_mouse,
        );
    }

    /// The line under an empty channel's title: its topic, else, for
    /// someone who may write, an invitation to say the first thing.
    pub(crate) fn empty_channel_message(&self) -> Option<String> {
        if self.direct_person.is_some() {
            return None;
        }
        let channel = self.channel(&self.active_channel)?;
        if !channel.topic.trim().is_empty() {
            return Some(channel.topic.clone());
        }
        self.can_participate().then(|| {
            crate::i18n::t_args(
                "chat-channel-empty-message",
                &[("channel", channel.name.as_str().into())],
            )
        })
    }

    /// How wide a message's text runs in the timeline `area`: the room the
    /// column leaves, but no wider than the readable measure, so a line on a
    /// wide screen stays one the eye can follow back to its start.
    pub(crate) fn message_text_width(&self, area: Rect, theme: &Theme) -> f32 {
        let text_x = area.x0 + self.chrome.text_inset();
        let room = area.x1 - text_x - self.chrome.gutter;
        let readable = f64::from(theme.tokens.metrics.readable_width);
        room.min(readable).max(f64::from(theme.tokens.spacing.s16)) as f32
    }

    pub(crate) fn draw_messages(
        &mut self,
        paint: PaintCtx<'_>,
        presence: &PresenceCtx,
        area: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let text_x = area.x0 + self.chrome.text_inset();
        let text_w = self.message_text_width(area, theme);
        let column = MessageColumn {
            area,
            text_x,
            text_width: text_w,
        };

        // An empty channel or direct Agent conversation explains the exact
        // capability the composer currently offers.
        if self.messages.is_empty() {
            self.draw_empty_messages(PaintCtx::new(scene, tf, tr, theme), area);
            return;
        }

        let groups = self.timeline_groups();
        let clock = ConversationClock::local();
        let (items, total) = self.measure_message_rows(tr, theme, &groups, text_w, clock);

        // The origin is pinned to the newest edge. If a message arrives while
        // the reader is looking back through history, compensate for the new
        // height so the message they were reading stays in the same place.
        // A fresh search reveal deliberately owns its initial position instead.
        if !self.timeline_at_end && !self.reveal_scroll_pending && self.timeline_total_height > 0.0
        {
            self.scroll += total - self.timeline_total_height;
        }

        self.scroll_content = total;
        self.scroll_viewport = area.height().max(0.0);
        let start_y = if total > area.height() {
            area.y1 - total
        } else {
            area.y0
        };
        let max_start = area.y0;
        let min_start = (area.y1 - total).min(area.y0);
        if self.reveal_scroll_pending {
            // A message shown from search, or one opened in the editor. The
            // sum above the target counts the day dividers too, so the row
            // lands where it is painted rather than a divider's height early.
            let target = self.revealed_message.as_deref().or(self.editing.as_deref());
            let target_index = target.and_then(|target| {
                items.iter().position(|item| match item {
                    TimelineItem::Row(row) => row.idx_id == target,
                    TimelineItem::Divider { .. } => false,
                })
            });
            if let Some(target_index) = target_index {
                let before: f64 = items
                    .iter()
                    .take(target_index)
                    .map(|item| Self::item_height(item, theme))
                    .sum();
                if let Some(TimelineItem::Row(row)) = items.get(target_index) {
                    let target_mid = before + row.height() / 2.0;
                    self.scroll = area.center().y - start_y - target_mid;
                }
            }
            // A result can disappear between search and paint. Either way,
            // make this a one-shot request so a stale selection never fights
            // normal reader-driven scrolling on later frames.
            self.reveal_scroll_pending = false;
        }
        self.scroll = self.scroll.clamp(0.0, (total - area.height()).max(0.0));
        let mut y = (start_y + self.scroll).clamp(min_start, max_start);
        // Store the normalised (bounded) offset, not the raw wheel accumulator.
        // This prevents an unbounded value from leaking across a resize or a
        // channel switch, and gives sync an accurate read-edge answer.
        self.scroll = y - start_y;
        // `scroll == 0` is the newest edge: `start_y` starts the timeline at
        // its bottom-aligned position, while increasing offsets travel back
        // through history toward `max_start`.
        let at_end = self.scroll <= 0.5;
        // Scrolling back to the newest message reads it. The read receipt
        // is the sync's, asked for once when the timeline arrives at its
        // newest edge, not on each wheel notch that brought it there (a
        // notch only pans the view: `events.rs`).
        if at_end && !self.timeline_at_end {
            self.read_on_present = true;
        }
        self.timeline_at_end = at_end;
        self.timeline_total_height = total;

        let first_row_hit = hits.len();
        // Only what stands in view is painted; a row keeps its scene for
        // the frames it stays there ([`crate::timeline`]).
        self.begin_timeline_paint(theme, tf, column);
        self.message_rows.clear();
        self.message_bar.lifted = self.bar_message();
        for item in &items {
            let height = Self::item_height(item, theme);
            if y >= area.y1 || y + height <= area.y0 {
                y += height;
                continue;
            }
            match item {
                TimelineItem::Divider { label } => {
                    y = self.draw_day_divider(PaintCtx::new(scene, tf, tr, theme), area, label, y);
                }
                TimelineItem::Row(rm) => {
                    self.paint_row(
                        PaintCtx::new(scene, tf, tr, theme),
                        presence,
                        rm,
                        column,
                        &mut y,
                        hits,
                    );
                }
            }
        }
        self.end_timeline_paint();
        // A row scrolled half out of the timeline is clipped where it is
        // painted; so is what it answers to, never reaching the header.
        let rows_hits = hits.split_off(first_row_hit);
        hits.extend(rows_hits.into_iter().filter_map(|(rect, hit)| {
            let clipped = rect.intersect(area);
            (clipped.width() > 0.0 && clipped.height() > 0.0).then_some((clipped, hit))
        }));
        self.draw_message_bar(PaintCtx::new(scene, tf, tr, theme), area, hits);
        if !self.timeline_at_end {
            self.draw_jump_to_latest(scene, tf, tr, theme, area, hits);
        }
    }

    /// A visible, keyboard-reachable way back from history. It only changes
    /// local viewport state; the durable read watermark advances on a later
    /// sync once the live edge is actually visible.
    fn draw_jump_to_latest(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let label = &crate::i18n::t("chat-jump-latest");
        let face = ButtonFace::new(Rect::ZERO, label).icon(Icon::ArrowDown);
        let w = face.intrinsic_width(tr, theme);
        let h = ButtonFace::height(ControlSize::Regular, theme);
        let inset = self.chrome.gutter;
        let rect = Rect::new(
            area.x1 - inset - w,
            area.y1 - inset / 2.0 - h,
            area.x1 - inset,
            area.y1 - inset / 2.0,
        );
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &ButtonFace::new(rect, label)
                .icon(Icon::ArrowDown)
                .hovered(self.hovered(rect)),
        );
        ChatApp::push_hit(hits, rect, Hit::JumpToLatest);
    }

    /// Measure every row across every group, with a day divider wherever the
    /// local day changes — not only at the oldest loaded message — returning
    /// the timeline's items and its total height.
    pub(super) fn measure_message_rows(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
        groups: &[model::MessageGroup],
        text_w: f32,
        clock: ConversationClock,
    ) -> (Vec<TimelineItem>, f64) {
        let mut total = 0.0;
        let mut items: Vec<TimelineItem> = Vec::new();
        let mut days = DayBreaks::new();
        for g in groups {
            for (gi, &idx) in g.message_indices.iter().enumerate() {
                let Some(m) = self.messages.get(idx) else {
                    continue;
                };
                let day = clock.day(m.ts);
                if days.begins(day) {
                    let height = DayDividerFace::height(theme);
                    total += height;
                    items.push(TimelineItem::Divider {
                        label: clock.day_label(day),
                    });
                }
                let rm = self.measure_row_kept(tr, theme, m, gi == 0, text_w);
                total += rm.height();
                items.push(TimelineItem::Row(rm));
            }
        }
        (items, total)
    }

    /// One timeline item's height: a divider's row, or a message's.
    fn item_height(item: &TimelineItem, theme: &Theme) -> f64 {
        match item {
            TimelineItem::Divider { .. } => DayDividerFace::height(theme),
            TimelineItem::Row(rm) => rm.height(),
        }
    }

    /// One day divider, in the toolkit's face: the same band, rules and
    /// centring Messages draws, so a day reads the same in both apps.
    fn draw_day_divider(&self, mut paint: PaintCtx<'_>, area: Rect, label: &str, y: f64) -> f64 {
        let theme = paint.theme;
        let height = DayDividerFace::height(theme);
        let band = Rect::new(area.x0, y, area.x1, y + height);
        let leading = area.x0 + self.chrome.gutter;
        let trailing = area.x1 - self.chrome.gutter;
        DayDividerFace::new(label).paint(&mut paint.reborrow(), band, leading, trailing);
        y + height
    }

    pub(crate) fn measure_row(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
        m: &Message,
        show_header: bool,
        text_w: f32,
    ) -> RowM {
        let sp = &theme.tokens.spacing;
        let editing = self.editing.as_ref() == Some(&m.id);
        // The editor measures its live working text so the timeline reflows as
        // you type; a static message measures its stored body.
        let body_h = if editing {
            // The editor's text wraps inside its field's side padding.
            let wrap = (f64::from(text_w) - self.body_text_inset.get()).max(1.0) as f32;
            f64::from(
                tr.measure_style(&self.edit_input.value(), Some(wrap), &self.chrome.body)
                    .1,
            )
            .max(self.chrome.body_line)
        } else if m.text.is_empty()
            && m.work
                .as_ref()
                .and_then(|work| work.stream.as_ref())
                .is_none()
        {
            0.0
        } else {
            // Measured by the same view that paints it, so a row never clips
            // its own last line.
            self.layout_body(m, Point::ZERO, text_w, theme, tr)
        };
        let att_h = m.attachment.as_ref().map_or(0.0, |attachment| {
            self.attachment_flow_height(attachment, f64::from(text_w), tr, theme)
        });
        let work_h = if editing {
            0.0
        } else {
            m.work
                .as_ref()
                .map_or(0.0, |work| measure_agent_work(tr, theme, work, text_w))
        };
        // The header, reaction and thread bands are measured through the same
        // helpers the draw pass uses, so the reserved height is the painted
        // height at every text scale.
        let header_h = if show_header {
            header_row_h(tr, theme)
        } else {
            0.0
        };
        let react_h = if m.reactions.is_empty() {
            0.0
        } else {
            ui::widget::reaction_strip::measure_height(
                &m.reactions,
                self.can_participate(),
                f64::from(text_w),
                tr,
                theme,
            ) + f64::from(sp.s2)
        };
        // While editing, the "Edited" marker gives way to the editor's own
        // controls row.
        // The marker keeps an `s1` of air above whatever follows it.
        let edited_h = if m.edited && !editing {
            line_of(tr, &theme.text(TextRole::Caption)) + f64::from(sp.s1)
        } else {
            0.0
        };
        let thread_h = if model::reply_count(&self.messages, &m.id) > 0 {
            thread_bar_h(theme) + f64::from(sp.s2)
        } else {
            0.0
        };
        let edit_ctrl_h = if editing {
            ButtonFace::height(ControlSize::Small, theme) + f64::from(sp.s2)
        } else {
            0.0
        };
        RowM {
            idx_id: m.id.clone(),
            show_header,
            top_gap: if show_header {
                f64::from(sp.s4)
            } else {
                f64::from(sp.s1)
            },
            pad: f64::from(sp.s2),
            header_h,
            body_h,
            work_h,
            reasoning_h: if !editing
                && m.work
                    .as_ref()
                    .and_then(|work| work.stream.as_ref())
                    .is_some_and(|stream| !stream.reasoning.is_empty())
            {
                ButtonFace::height(ControlSize::Regular, theme) + f64::from(sp.s2)
            } else {
                0.0
            },
            resource_h: if !editing && m.share.is_some() {
                self.share_card_height(m, f64::from(text_w), tr, theme)
            } else if !editing && m.reference.is_some() && m.source_provider.is_some() {
                2.0 * (ButtonFace::height(ControlSize::Regular, theme) + f64::from(sp.s2))
            } else {
                0.0
            },
            att_h,
            react_h,
            edited_h,
            thread_h,
            editing,
            edit_ctrl_h,
        }
    }

    /// Paint the row `rm` from `*y`; returns where it drew its texts.
    pub(crate) fn draw_row(
        &mut self,
        paint: PaintCtx<'_>,
        presence: &PresenceCtx,
        rm: &RowM,
        column: MessageColumn,
        y: &mut f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> crate::app::PaintedMessage {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let area = column.area;
        let mut parts = crate::app::PaintedMessage::default();
        let Some(m) = self.message(&rm.idx_id).cloned() else {
            return parts;
        };
        let revealed = self.revealed_message.as_deref() == Some(m.id.as_str());
        let lifted = self.row_lifted(&m.id);
        let s2 = f64::from(theme.tokens.spacing.s2);
        let row = Rect::new(area.x0 + s2, *y, area.x1 - s2, *y + rm.height());
        if revealed || lifted {
            chrome::paint_row_selection(
                scene,
                tf,
                row,
                SelectionState::new(revealed, lifted, false),
                theme.tokens.colors.surfaces.content,
                theme,
            );
        }
        *y += rm.top_gap;
        let content_top = *y;

        if rm.show_header {
            self.draw_row_header(
                PaintCtx::new(scene, tf, tr, theme),
                column,
                &m,
                y,
                &mut parts,
            );
        }
        if rm.editing {
            self.draw_message_editor(
                PaintCtx::new(scene, tf, tr, theme),
                presence,
                column,
                rm,
                y,
                hits,
            );
        } else if !m.text.is_empty()
            || m.work
                .as_ref()
                .and_then(|work| work.stream.as_ref())
                .is_some()
        {
            // The body as written: paragraphs, formatting, and mentions in
            // their participants' colours, drawn by the editor that wrote it.
            self.layout_body(
                &m,
                Point::new(column.text_x, *y),
                column.text_width,
                theme,
                tr,
            );
            self.draw_body(&m, &mut PaintCtx::new(scene, tf, tr, theme));
            *y += rm.body_h;
        }
        if rm.reasoning_h > 0.0 {
            let paint = PaintCtx::new(scene, tf, tr, theme);
            self.draw_reasoning_toggle(paint, rm, &m.id, column, y, hits);
        }
        if !rm.editing
            && let Some(work) = &m.work
        {
            Self::draw_agent_work(
                PaintCtx::new(scene, tf, tr, theme),
                work,
                Rect::new(
                    column.text_x,
                    *y,
                    column.text_x + f64::from(column.text_width),
                    *y + rm.work_h,
                ),
            );
            *y += rm.work_h;
        }
        // Right-click a message (header + body) opens its menu: editing
        // your own, reporting anyone else's, removing it as a moderator.
        // Recorded before the reaction / thread hits so those still win in
        // their own sub-regions.
        if !rm.editing {
            ChatApp::push_hit(
                hits,
                Rect::new(area.x0, content_top, area.x1, *y),
                Hit::MessageMenu { msg: m.id.clone() },
            );
        }
        self.push_author_hit(&m, &parts, hits);
        if !rm.editing {
            parts.lines = self.body_lines(&m.id);
        }
        if rm.resource_h > 0.0 && m.share.is_some() {
            let place = (column.text_x, f64::from(column.text_width), column.area);
            self.draw_share_card(PaintCtx::new(scene, tf, tr, theme), &m, place, y, hits);
        } else if rm.resource_h > 0.0 {
            self.draw_resource_navigation(PaintCtx::new(scene, tf, tr, theme), &m, column, y, hits);
        }
        self.draw_row_footer(PaintCtx::new(scene, tf, tr, theme), rm, &m, column, y, hits);
        *y += rm.pad;
        parts
    }

    /// The button that shows or hides the reasoning an agent's provider
    /// shared. Advances `*y` past it.
    fn draw_reasoning_toggle(
        &self,
        paint: PaintCtx<'_>,
        rm: &RowM,
        id: &str,
        column: MessageColumn,
        y: &mut f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let caption = crate::i18n::t(if self.expanded_reasoning.contains(id) {
            "chat-agent-hide-reasoning"
        } else {
            "chat-agent-show-reasoning"
        });
        let rect = Rect::new(
            column.text_x,
            *y,
            column.text_x + f64::from(column.text_width),
            *y + rm.reasoning_h - f64::from(theme.tokens.spacing.s2),
        );
        Button::paint(scene, tf, tr, theme, &ButtonFace::new(rect, &caption));
        Self::push_hit(hits, rect, Hit::AgentReasoning(id.to_owned()));
        *y += rm.reasoning_h;
    }

    /// Keep where a shown message drew its texts, cut to the timeline's
    /// `area`.
    pub(crate) fn note_painted(&mut self, id: &str, parts: crate::app::PaintedMessage, area: Rect) {
        let parts = parts.clipped(area);
        if !parts.is_empty() {
            self.painted_messages.insert(id.to_owned(), parts);
        }
    }

    fn draw_resource_navigation(
        &self,
        paint: PaintCtx<'_>,
        m: &Message,
        column: MessageColumn,
        y: &mut f64,
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
        self.resource_navigation.shown(&m.id);
        let width = f64::from(column.text_width);
        // The resource as a reference chip, centred in a control's row so it
        // keeps a control's pointer target.
        *y += gap;
        let label = m
            .reference
            .as_ref()
            .map_or("", |reference| reference.label.as_str());
        let lead = ui::framework::reference::LEAD;
        let chip_w = ChipFace::intrinsic_width(label, lead, false, tr, theme).min(width);
        let row = Rect::new(column.text_x, *y, column.text_x + chip_w, *y + height);
        let chip_h = ChipFace::height(theme).min(height);
        let chip = Rect::new(
            row.x0,
            row.center().y - chip_h / 2.0,
            row.x1,
            row.center().y + chip_h / 2.0,
        );
        ChipFace::new(chip, label)
            .lead(lead)
            .hovered(self.hovered(row))
            .paint(scene, tf, tr, theme);
        ChatApp::push_hit(hits, row, Hit::OpenResource(m.id.clone()));
        *y += height + gap;
        let caption = self.resource_navigation.label(&m.id, false);
        let button_w = ButtonFace::new(Rect::ZERO, &caption)
            .intrinsic_width(tr, theme)
            .min(width);
        let rect = Rect::new(column.text_x, *y, column.text_x + button_w, *y + height);
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &ButtonFace::new(rect, &caption).hovered(self.hovered(rect)),
        );
        ChatApp::push_hit(hits, rect, Hit::CopyResourceLink(m.id.clone()));
        *y += height;
    }

    /// A message group's header: avatar, author and time. Advances `*y`
    /// past it and notes in `parts` where each was drawn.
    fn draw_row_header(
        &self,
        paint: PaintCtx<'_>,
        column: MessageColumn,
        m: &Message,
        y: &mut f64,
        parts: &mut crate::app::PaintedMessage,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let MessageColumn { area, text_x, .. } = column;
        let side = self.chrome.avatar;
        let avatar = Rect::new(
            area.x0 + self.chrome.gutter,
            *y,
            area.x0 + self.chrome.gutter + side,
            *y + side,
        );
        let mut ctx = PaintCtx::new(scene, tf, tr, theme);
        let (end_x, nh, line) = self.paint_author(
            &mut ctx,
            m,
            avatar,
            Point::new(text_x, *y),
            area.x1 - self.chrome.gutter,
        );
        // The time's line box is bottom-aligned to the name's, which keeps
        // their baselines together at any text scale.
        let time_style = theme.text(TextRole::Caption).numeric();
        let clock = clock_label(m.ts);
        let th = line_of(ctx.text, &time_style);
        let clock_x = end_x + f64::from(theme.tokens.spacing.s2);
        let (clock_w, clock_h) = text(
            &mut ctx,
            &clock,
            &time_style,
            theme.text_dim,
            Point::new(clock_x, *y + nh - th),
            None,
        );
        let clock_w = f64::from(clock_w);
        parts.avatar = Some(line.mark);
        parts.author = Some((line.name, line.shown));
        // Where a bridged person writes from: its mark, saying so when
        // hovered and to assistive technology.
        parts.service = line
            .badge
            .zip(self.message_author(m, "", "", None).service_tip());
        parts.time = Some((
            Rect::new(
                clock_x,
                *y + nh - th,
                clock_x + clock_w,
                *y + nh - th + f64::from(clock_h),
            ),
            clock,
        ));
        // A pinned message says so where its time is.
        if m.pinned {
            let icon = f64::from(theme.tokens.metrics.icon_sm);
            let x = clock_x + clock_w + f64::from(theme.tokens.spacing.s2);
            let cy = *y + nh - th / 2.0;
            Icon::Pin.draw(
                ctx.scene,
                tf,
                Rect::new(x, cy - icon / 2.0, x + icon, cy + icon / 2.0),
                theme.text_dim,
            );
            text(
                &mut ctx,
                &crate::i18n::t("chat-message-pinned-marker"),
                &time_style,
                theme.text_dim,
                Point::new(x + icon + f64::from(theme.tokens.spacing.s1), *y + nh - th),
                None,
            );
        }
        *y += header_row_h(tr, theme);
    }

    /// Paint the durable account an agent attached to its own reply.
    ///
    /// The card deliberately avoids a spinner or a claim that the agent is
    /// live. `AgentWork` records what has happened; current availability is
    /// still communicated by presence. The rationale is the agent's public
    /// explanation, never hidden chain-of-thought, and trace rows exist only
    /// for operations the dispatch path actually attempted.
    pub(super) fn draw_agent_work(mut paint: PaintCtx<'_>, work: &AgentWork, card: Rect) {
        let theme = paint.theme;
        let spacing = &theme.tokens.spacing;
        CardFace::new().paint(&mut paint, card);

        let presentation = AgentWorkPresentation::from(work);
        let inset = f64::from(spacing.s3);
        let width = ui::num::f32_of_f64((card.width() - 2.0 * inset).max(0.0));
        let mut y = card.y0 + inset;
        y += draw_agent_work_line(
            &mut paint,
            &presentation.summary,
            AgentWorkLineStyle::Summary,
            Point::new(card.x0 + inset, y),
            width,
        );
        for line in presentation.details {
            y += f64::from(spacing.s2);
            y += draw_agent_work_line(
                &mut paint,
                &line.text,
                line.style,
                Point::new(card.x0 + inset, y),
                width,
            );
        }
    }

    /// The "Edited" marker, attachment, reactions and thread link below the
    /// body — each only when present, all on the body's left edge. Advances
    /// `*y` past them.
    fn draw_row_footer(
        &self,
        paint: PaintCtx<'_>,
        rm: &RowM,
        m: &Message,
        column: MessageColumn,
        y: &mut f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let MessageColumn {
            text_x,
            text_width: text_w,
            ..
        } = column;
        if m.edited && !rm.editing {
            let style = theme.text(TextRole::Caption);
            let label = &crate::i18n::t("chat-message-edited");
            let w = width_of(tr, label, &style);
            let band = Rect::new(
                text_x,
                *y,
                text_x + w,
                *y + rm.edited_h - f64::from(theme.tokens.spacing.s1),
            );
            let ink = if self.hovered(band) {
                theme.text_primary
            } else {
                theme.text_tertiary
            };
            let mut ctx = PaintCtx::new(scene, tf, tr, theme);
            line_in(&mut ctx, label, &style, ink, text_x, band, w + 1.0);
            ChatApp::push_hit(
                hits,
                band,
                Hit::EditHistory {
                    msg: m.id.clone(),
                    anchor: Point::new(text_x, band.y1 + f64::from(theme.tokens.spacing.s1)),
                },
            );
            *y += rm.edited_h;
        }

        if let Some(att) = &m.attachment {
            let card = self.draw_attachment(
                PaintCtx::new(scene, tf, tr, theme),
                att,
                Point::new(text_x, *y),
                f64::from(text_w),
            );
            // A shared picture opens: a card nobody can enlarge is a card
            // that answers nothing ([`crate::attachment_view`]). Recorded
            // after the row's menu target, so a click on the card opens it.
            if self.embed_allowed(att.blob()) {
                ChatApp::push_hit(
                    hits,
                    card,
                    Hit::Attachment {
                        msg: m.id.clone(),
                        action: ui::widget::conversation::AttachmentAction::OpenFullSize,
                    },
                );
            }
            *y += rm.att_h;
        }

        if !m.reactions.is_empty() {
            self.draw_reactions(
                PaintCtx::new(scene, tf, tr, theme),
                m,
                (false, Point::new(text_x, *y), f64::from(text_w)),
                hits,
            );
            *y += rm.react_h;
        }

        if rm.thread_h > 0.0 {
            self.draw_thread_affordance(PaintCtx::new(scene, tf, tr, theme), m, column, *y, hits);
            *y += rm.thread_h;
        }
    }

    /// The inline message editor in place of a message body: a field with
    /// the live working text, and Cancel / Save beneath it. Advances `*y`
    /// past the whole editor.
    fn draw_message_editor(
        &mut self,
        paint: PaintCtx<'_>,
        presence: &PresenceCtx,
        column: MessageColumn,
        rm: &RowM,
        y: &mut f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let MessageColumn {
            text_x,
            text_width: text_w,
            ..
        } = column;
        let sp = &theme.tokens.spacing;
        let pad = f64::from(sp.s2);
        // The field's text starts on the message column's edge, where the
        // body it replaces stood, and the field ends where the column (and
        // the composer below it) ends.
        let inset = self.body_text_inset.get();
        let field = Rect::new(
            text_x - inset,
            *y,
            text_x + f64::from(text_w),
            *y + rm.body_h + 2.0 * pad,
        );
        paint_field(scene, tf, theme, field);
        self.edit_input
            .set_placeholder(&crate::i18n::t("chat-edit-field"));
        self.edit_input.set_mention_paint(self.mention_paint(theme));
        self.edit_input.layout(
            Rect::new(field.x0, field.y0 + pad, field.x1, field.y1 - pad),
            theme,
            tr,
        );
        self.edit_input.set_focus(self.is_focused());
        self.edit_input.draw(scene, tf, tr, theme, presence);
        ChatApp::push_hit(hits, field, Hit::FocusEdit);
        *y = field.y1 + f64::from(sp.s2);

        // Cancel and Save at the field's trailing edge.
        let h = ButtonFace::height(ControlSize::Small, theme);
        let save_label = crate::i18n::t("chat-edit-save-button");
        let cancel_label = crate::i18n::t("chat-edit-cancel-button");
        let save_face = ButtonFace::new(Rect::ZERO, &save_label).control_size(ControlSize::Small);
        let save_w = save_face.intrinsic_width(tr, theme);
        let save = Rect::new(field.x1 - save_w, *y, field.x1, *y + h);
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &ButtonFace::new(save, &save_label)
                .role(ButtonRole::Primary)
                .control_size(ControlSize::Small)
                .hovered(self.hovered(save)),
        );
        ChatApp::push_hit(hits, save, Hit::EditSave);
        let cancel_w = ButtonFace::new(Rect::ZERO, &cancel_label)
            .control_size(ControlSize::Small)
            .intrinsic_width(tr, theme);
        let cancel = Rect::new(
            save.x0 - f64::from(sp.s2) - cancel_w,
            *y,
            save.x0 - f64::from(sp.s2),
            *y + h,
        );
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &ButtonFace::new(cancel, &cancel_label)
                .control_size(ControlSize::Small)
                .hovered(self.hovered(cancel)),
        );
        ChatApp::push_hit(hits, cancel, Hit::EditCancel);
        *y += rm.edit_ctrl_h;
    }

    /// The message's reactions as the toolkit's shared strip: a pill per
    /// emoji (yours accented), then the add button while you may react.
    /// `place` is the surface (`true` in the thread), the strip's origin
    /// and the width it wraps at. Returns its height.
    pub(super) fn draw_reactions(
        &self,
        paint: PaintCtx<'_>,
        m: &Message,
        (thread, origin, width): (bool, Point, f64),
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        use ui::widget::reaction_strip::StripHit;
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let mut strips = self.reaction_strips.borrow_mut();
        let strip = strips.entry((thread, m.id.clone())).or_default();
        let laid = strip.layout(
            &m.reactions,
            &self.me.id,
            self.can_participate(),
            (origin, width),
            tr,
            theme,
        );
        let height = laid.height();
        for (rect, target) in laid.targets() {
            let hit = match target {
                StripHit::Toggle(emoji) => Hit::Reaction {
                    msg: m.id.clone(),
                    emoji,
                },
                StripHit::Add { anchor } => Hit::AddReaction {
                    msg: m.id.clone(),
                    anchor,
                },
            };
            ChatApp::push_hit(hits, rect, hit);
        }
        // The strip lifts the chip under the pointer: the row asks about
        // each of them.
        self.note_pointer_probes(laid.targets().into_iter().map(|(rect, _)| rect));
        let hover = (self.menu.is_none() && !self.emoji_open).then_some(self.pointer_in_row());
        strip.paint(&mut PaintCtx::new(scene, tf, tr, theme), hover, None);
        height
    }

    /// The link into a message's thread: who replied, how many replies and
    /// when the last one came. Lifted while the thread is open.
    fn draw_thread_affordance(
        &self,
        paint: PaintCtx<'_>,
        m: &Message,
        column: MessageColumn,
        y: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let sp = &theme.tokens.spacing;
        let MessageColumn { text_x, .. } = column;
        let count = model::reply_count(&self.messages, &m.id);
        let parts = model::thread_participants(&self.messages, &m.id);
        let bar_h = thread_bar_h(theme);
        let pad = f64::from(sp.s2);
        let avatar = AvatarSize::Small.side(theme);
        let shown = parts.iter().take(3).count() as f64;
        let stack_w = if shown > 0.0 {
            avatar + (shown - 1.0) * (avatar * 0.7)
        } else {
            0.0
        };
        let replies = crate::i18n::t_args("chat-thread-replies", &[("count", count.into())]);
        let last = self
            .messages
            .iter()
            .filter(|reply| reply.parent.as_deref() == Some(m.id.as_str()))
            .map(|reply| reply.ts)
            .max()
            .map(|ts| {
                crate::i18n::t_args(
                    "chat-thread-last-reply",
                    &[("time", clock_label(ts).into())],
                )
            });
        let label_style = theme.text(TextRole::Label).strong();
        let meta_style = theme.text(TextRole::Caption);
        let replies_w = width_of(tr, &replies, &label_style);
        let last_w = last
            .as_deref()
            .map_or(0.0, |s| width_of(tr, s, &meta_style));
        let content_w = stack_w + pad + replies_w + if last_w > 0.0 { pad + last_w } else { 0.0 };
        let bar = Rect::new(text_x - pad, y, text_x + content_w + pad, y + bar_h);
        let open = self.thread_open.as_deref() == Some(m.id.as_str());
        chrome::paint_row_selection(
            scene,
            tf,
            bar,
            SelectionState::new(open, self.hovered(bar), false),
            theme.tokens.colors.surfaces.content,
            theme,
        );
        let ground = chrome::selection_tier_on(
            theme,
            SelectionState::new(open, self.hovered(bar), false),
            theme.tokens.colors.surfaces.content,
        )
        .fill;
        let mut ax = text_x;
        let cy = bar.center().y;
        let mut ctx = PaintCtx::new(scene, tf, tr, theme);
        for pid in parts.iter().take(3) {
            let name = self.user_name(pid);
            AvatarFace::new(&name, pid)
                .presence(self.identity_colour(pid))
                .ringed_on(if open || self.hovered(bar) {
                    ground
                } else {
                    theme.tokens.colors.surfaces.content
                })
                .paint(
                    &mut ctx,
                    Rect::new(ax, cy - avatar / 2.0, ax + avatar, cy + avatar / 2.0),
                );
            ax += avatar * 0.7;
        }
        let x = text_x + stack_w + if stack_w > 0.0 { pad } else { 0.0 };
        line_in(
            &mut ctx,
            &replies,
            &label_style,
            theme.text_primary,
            x,
            bar,
            replies_w + 1.0,
        );
        if let Some(last) = last {
            line_in(
                &mut ctx,
                &last,
                &meta_style,
                theme.text_dim,
                x + replies_w + pad,
                bar,
                last_w + 1.0,
            );
        }
        ChatApp::push_hit(hits, bar, Hit::OpenThread(m.id.clone()));
    }

    /// The vertical space an attachment takes in a column `max_w` wide,
    /// including the gap before what follows it.
    pub(super) fn attachment_flow_height(
        &self,
        att: &Attachment,
        max_w: f64,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        let gap = f64::from(theme.tokens.spacing.s2);
        if !self.embed_allowed(att.blob()) || matches!(att, Attachment::File { .. }) {
            return file_card_height(tr, theme) + gap;
        }
        let meta = media_meta(att);
        let (w, face) = media_face(att, MediaArt::Placeholder, &meta, max_w, theme);
        face.height(tr, theme, w) + gap
    }

    pub(super) fn draw_attachment(
        &self,
        paint: PaintCtx<'_>,
        att: &Attachment,
        origin: Point,
        max_w: f64,
    ) -> Rect {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let mut ctx = PaintCtx::new(scene, tf, tr, theme);
        // Permission boundary: an embed this peer has not been granted never
        // requests its shared bytes; it shows what it is and says why it
        // cannot open.
        if !self.embed_allowed(att.blob()) {
            let icon = attachment_icon(att);
            let card = paint_file_card(
                &mut ctx,
                origin,
                max_w,
                icon,
                att.name(),
                &crate::i18n::t("chat-attachment-no-access"),
            );
            return card;
        }
        match att {
            Attachment::File { name, size, .. } => {
                let card = paint_file_card(
                    &mut ctx,
                    origin,
                    max_w,
                    Icon::FileText,
                    name,
                    &format_bytes(*size),
                );
                self.paint_attachment_open(&mut ctx, att, card);
                card
            }
            Attachment::Image { blob, .. } => {
                let image = self.images.get(&crate::images::blob_key(blob));
                let mut meta = media_meta(att);
                let step = (image.is_none() && !blob.starts_with(crate::images::DEMO_SCHEME))
                    .then(|| self.image_arrival(blob, &mut meta))
                    .flatten();
                let art = match (&image, step) {
                    (Some(image), _) => MediaArt::Image(image),
                    (None, Some(step)) => MediaArt::Arriving(step),
                    (None, None) => MediaArt::Placeholder,
                };
                let (w, face) = media_face(att, art, &meta, max_w, theme);
                let h = face.height(ctx.text, theme, w);
                let card = Rect::new(origin.x, origin.y, origin.x + w, origin.y + h);
                face.paint(&mut ctx, card);
                self.paint_attachment_open(&mut ctx, att, card);
                card
            }
            Attachment::Video { blob, .. } => {
                // A frame needs the media engine's decoder, so the card shows
                // the poster the image store resolves for the clip (the demo
                // recording ships one) and the generated art otherwise.
                let poster = self.video_poster(blob);
                let art = poster
                    .as_ref()
                    .map_or(MediaArt::Placeholder, MediaArt::Image);
                let meta = media_meta(att);
                let (w, face) = media_face(att, art, &meta, max_w, theme);
                let h = face.height(ctx.text, theme, w);
                let card = Rect::new(origin.x, origin.y, origin.x + w, origin.y + h);
                face.paint(&mut ctx, card);
                self.paint_attachment_open(&mut ctx, att, card);
                card
            }
        }
    }

    /// The badge that says a card opens, on the card the pointer is over.
    fn paint_attachment_open(&self, paint: &mut PaintCtx<'_>, att: &Attachment, card: Rect) {
        let summary = self.attachment_summary(att);
        ui::widget::conversation::attachment::AttachmentFace::new(&summary).paint_open_badge(
            paint,
            card,
            self.hovered(card),
        );
    }
}
