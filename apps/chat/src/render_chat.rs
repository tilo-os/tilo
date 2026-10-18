//! Rendering for the persistent chat surface: the navigation drawer (the
//! workspace's channel list), the conversation header, the grouped
//! message timeline, the composer, and the detail drawer (people or a
//! thread). All geometry is computed from `self.rect` and the drawers; every
//! clickable area is recorded into `hits`.
//!
//! Chrome is composed from the toolkit's faces — navigation rows and section
//! headers, buttons, chips, avatars, list rows, cards and media cards — and
//! every text names a `TextRole`. What remains painted here is content: the
//! message bodies (the rich-text view) and the attachments' pictures.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use ui::TextKind;
use ui::Widget;
use ui::author::Author;
use ui::chrome::{
    self, NavCount, NavRowFace, NavSectionFace, SelectionState, nav_item_height, nav_row_height,
    nav_row_layout, nav_section_height, nav_section_layout,
};
use ui::format::bytes as format_bytes;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    AvatarFace, AvatarSize, Button, ButtonFace, ButtonRole, CardFace, ChipFace, ChipLead,
    ControlSize, EmptyState, EmptyStateContent, ListRowFace, MediaArt, MediaAspect, MediaCardFace,
    MediaSize, RowLeading, Segment, SegmentedFace, SegmentedStyle,
};

use crate::agent_tasks::{AgentComposerIntent, AgentTaskReceipt, AgentTaskView};
use crate::app::{ChatApp, Hit, MenuTarget};
use crate::model::{
    self, AgentRoute, AgentStepOutcome, AgentWork, AgentWorkState, Attachment, ChannelKind,
    Message, Presence, User,
};
use crate::paint::{band_at_least, elide, line_in, line_of, push_clip, text, width_of};

/// How much of the participant's colour a mention *of you* keeps when it is
/// tinted toward the surface. A filled band, so it can be found by scanning.
const MENTION_SELF_BAND_MIX: f32 = 0.78;

/// Contrast floors for the mention run, measured rather than eyeballed:
/// AA body text wherever it lands, and the non-text floor for the filled band
/// against the surface it sits on (a band nobody can see is not a highlight).
use ui::tokens::colors::NON_TEXT_MIN_CONTRAST as MENTION_BAND_CONTRAST;
use ui::tokens::contrast::BODY_TEXT_MIN_CONTRAST as MENTION_TEXT_CONTRAST;

#[derive(Clone, Copy)]
struct MessageColumn {
    area: Rect,
    text_x: f64,
    text_width: f32,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HeaderIdentityText {
    pub(crate) name: String,
    pub(crate) kind: Option<String>,
    pub(crate) context: String,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AgentHeaderWidths {
    pub(crate) name: f64,
    pub(crate) kind: f64,
}

/// Reserve the complete localized collaborator kind before allowing a long
/// display name to consume the Agent header. Authentication is identity, not
/// secondary metadata that may disappear at compact widths.
pub(crate) fn agent_header_widths(
    tr: &mut TextRenderer,
    theme: &Theme,
    available: f64,
    subtitle: &str,
) -> AgentHeaderWidths {
    let gap = f64::from(theme.tokens.spacing.s3);
    let kind = width_of(tr, subtitle, &theme.text(TextRole::Body)).min(available.max(0.0));
    AgentHeaderWidths {
        name: (available - gap - kind).max(0.0),
        kind,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AgentTaskStatusLines {
    pub(crate) primary: String,
    pub(crate) receipt: Option<String>,
}

pub(crate) fn agent_task_status_lines(view: &AgentTaskView) -> AgentTaskStatusLines {
    let primary = if let Some(task) = view.current() {
        let state = crate::i18n::t(match task.status.as_str() {
            "done" => "chat-agent-task-complete",
            "cancelled" => "chat-agent-task-cancelled",
            "ended" => "chat-agent-task-ended",
            "queued" => "chat-agent-task-queued",
            _ => match task.execution_phase() {
                Some("accepted") => "chat-agent-task-accepted",
                Some("planning") => "chat-agent-task-planning",
                Some("executing") => "chat-agent-task-executing",
                _ => "chat-agent-task-phase-unknown",
            },
        });
        if task.run_epoch.is_none_or(|epoch| epoch == 0) {
            return AgentTaskStatusLines {
                primary: state,
                receipt: view.start_error.clone().or_else(|| match view.receipt {
                    AgentTaskReceipt::Queued => Some(crate::i18n::t("chat-agent-task-saved")),
                    AgentTaskReceipt::Refused => {
                        Some(crate::i18n::t("chat-agent-direction-refused"))
                    }
                    _ => None,
                }),
            };
        }
        state
    } else if let Some((task, epoch)) = view.selected_identity() {
        crate::i18n::t_args(
            "chat-agent-task-selected-unavailable",
            &[("task", task.into()), ("run", epoch.to_string().into())],
        )
    } else if view.loading {
        crate::i18n::t("chat-agent-task-loading")
    } else if !view.complete {
        crate::i18n::t("chat-agent-task-unavailable")
    } else if view.tasks.is_empty() {
        crate::i18n::t("chat-agent-task-none")
    } else {
        crate::i18n::t_args(
            "chat-agent-task-choose",
            &[(
                "count",
                i64::try_from(view.tasks.len()).unwrap_or(i64::MAX).into(),
            )],
        )
    };
    let receipt =
        view.start_error.clone().or_else(|| match &view.receipt {
            // Storing the direction is the system's job; only what the agent
            // does with it is news. A start refusal still takes precedence.
            AgentTaskReceipt::None | AgentTaskReceipt::Saving => None,
            AgentTaskReceipt::Saved { .. } => Some(crate::i18n::t(
                if view.current().is_some_and(|task| {
                    matches!(task.status.as_str(), "done" | "cancelled" | "ended")
                }) {
                    "chat-agent-direction-unapplied-ended"
                } else {
                    "chat-agent-direction-waiting"
                },
            )),
            AgentTaskReceipt::Applied => Some(crate::i18n::t(
                if view.current().is_some_and(|task| {
                    matches!(task.status.as_str(), "done" | "cancelled" | "ended")
                }) {
                    "chat-agent-direction-applied-ended"
                } else {
                    "chat-agent-direction-applied"
                },
            )),
            AgentTaskReceipt::Started => Some(crate::i18n::t("chat-agent-task-started")),
            AgentTaskReceipt::StopRequested => Some(crate::i18n::t("chat-agent-task-stopped")),
            AgentTaskReceipt::Queued => Some(crate::i18n::t("chat-agent-task-saved")),
            AgentTaskReceipt::Refused => Some(crate::i18n::t("chat-agent-direction-refused")),
        });
    AgentTaskStatusLines { primary, receipt }
}

/// How a mention of `user` is painted on `ground`: an optional filled band, and
/// the ink for the run.
///
/// The two cases are deliberately different in *kind*, not merely in strength.
/// A mention of somebody else is their name in their own colour — the same
/// colour their caret pill and cursor use, so "who is this about" is answered
/// the same way everywhere. A mention of **you** is the one you have to be able
/// to find by scanning a channel you have been away from, so it is filled:
/// their colour becomes the ground and the text sits on it.
///
/// Both halves are then corrected against what they actually land on, because
/// a colour picked to tell people apart is not a colour picked to be read.
pub(crate) fn mention_colors(
    app: &ChatApp,
    user: &str,
    is_self: bool,
    theme: &Theme,
    ground: Color,
) -> (Option<Color>, Color) {
    let colors = &theme.tokens.colors;
    let own = colors.presence.of_rgb(app.participant_color(user));
    if !is_self {
        // Their colour, made legible on the surface the message is on.
        return (None, colors.ink_on(ground, own, MENTION_TEXT_CONTRAST));
    }
    let band = band_at_least(
        colors,
        ui::tokens::colors::mix8(own, ground, MENTION_SELF_BAND_MIX),
        own,
        ground,
        MENTION_BAND_CONTRAST,
    );
    // Ink for a filled band starts from body ink and is pushed away from the
    // band — text in the band's own hue on the band is the one combination
    // that cannot be rescued.
    let ink = colors.ink_on(band, colors.text_primary, MENTION_TEXT_CONTRAST);
    (Some(band), ink)
}

impl ChatApp {
    pub(crate) fn draw_conversation(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        if self.task_goal.is_some() {
            self.messages.clear();
        } else if let Some(tasks) = self.direct_agent_tasks() {
            self.messages = tasks.conversation_messages(&self.active_channel, &self.me.id);
        }
        self.draw_header(scene, tf, tr, theme, hits);
        let field_w = self.center_right() - self.center_left() - 2.0 * self.chrome.gutter;
        let width = composer_editor_width(
            field_w,
            theme,
            self.direct_agent.is_none() && self.direct_person.is_none() && self.task_goal.is_none(),
        );
        self.composer_main.measure(width, f32::INFINITY, theme, tr);
        let area = self.messages_rect(tr, theme);
        if area.width() > 0.0 && area.height() >= self.chrome.body_line {
            push_clip(scene, tf, area);
            if self.direct_person.is_some() {
                self.draw_person_messages(PaintCtx::new(scene, tf, tr, theme), presence, area);
            } else {
                self.draw_messages(PaintCtx::new(scene, tf, tr, theme), presence, area, hits);
            }
            scene.pop_layer();
        }
        // Older messages fade at the header edge when more history is above.
        if area.height() > 0.0 && self.scroll_content - self.scroll_viewport - self.scroll > 0.5 {
            let column = Rect::new(area.x0, area.y0, self.center_right(), area.y1);
            ui::surface::content_edge_fade(
                scene,
                tf,
                column,
                ui::surface::ContentEdge::Top,
                theme.tokens.colors.surfaces.content,
            );
        }
        self.draw_composer(scene, tf, tr, theme, presence, hits);
    }

    /// The colours mentions are painted in on `theme`, for every participant
    /// (a mention of you filled, anyone else's in their colour), and a
    /// fingerprint of what they were built from.
    pub(crate) fn mention_paint(&self, theme: &Theme) -> (u64, ui::widget::marks::MentionPaint) {
        use std::hash::{Hash, Hasher};
        let ground = theme.tokens.colors.surfaces.content;
        let colors = |user: &str| {
            let (band, ink) = mention_colors(self, user, self.is_me(user), theme, ground);
            ui::widget::marks::MentionColors { ink, band }
        };
        let entries: Vec<(String, ui::widget::marks::MentionColors)> = self
            .participants()
            .into_iter()
            .map(|participant| {
                let painted = colors(&participant.id);
                (participant.id, painted)
            })
            .collect();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for (user, painted) in &entries {
            user.hash(&mut hasher);
            format!("{:?}{:?}", painted.ink, painted.band).hash(&mut hasher);
        }
        format!("{:?}", theme.tokens.colors.text_primary).hash(&mut hasher);
        let fingerprint = hasher.finish();
        if let Some((cached, paint)) = self.mention_paint_cache.borrow().as_ref()
            && *cached == fingerprint
        {
            return (fingerprint, std::rc::Rc::clone(paint));
        }
        let fallback = ui::widget::marks::MentionColors {
            ink: theme.tokens.colors.text_primary,
            band: None,
        };
        let map: std::collections::HashMap<String, ui::widget::marks::MentionColors> =
            entries.into_iter().collect();
        let paint: ui::widget::marks::MentionPaint =
            std::rc::Rc::new(move |user: &str| map.get(user).copied().unwrap_or(fallback));
        *self.mention_paint_cache.borrow_mut() = Some((fingerprint, std::rc::Rc::clone(&paint)));
        (fingerprint, paint)
    }

    /// Lay a message's body out at `origin` and `width` in its read-only
    /// view; returns the view's height. The view is kept by message id, so
    /// an unchanged body is not shaped again.
    fn layout_body(
        &self,
        m: &Message,
        origin: Point,
        width: f32,
        theme: &Theme,
        tr: &mut TextRenderer,
    ) -> f64 {
        let (fingerprint, paint) = self.mention_paint(theme);
        let mut views = self.body_views.borrow_mut();
        let (painted, view) = views.entry((0, m.id.clone())).or_insert_with(|| {
            (
                0,
                tilo_rich_text::RichTextView::with_editor(tilo_rich_text::compact_editor()),
            )
        });
        if *painted != fingerprint {
            view.set_mention_paint(paint);
            *painted = fingerprint;
        }
        let blocks = self.agent_display_blocks(m);
        view.show(&blocks);
        let editor = view.editor_mut();
        editor.clear_document_selection();
        if let Some((message, source, start, end)) = &self.revealed_range
            && message == &m.id
            && self.revealed_message.as_ref() == Some(message)
            && source == &m.text
            && crate::body::plain_text(&m.blocks()) == *source
        {
            let position = |offset: usize| {
                let mut remaining = offset;
                for (block, content) in blocks.iter().enumerate() {
                    if remaining <= content.text.len() {
                        return Some(ui::widget::DocumentPosition {
                            block,
                            byte: remaining,
                        });
                    }
                    remaining = remaining.saturating_sub(content.text.len() + 1);
                }
                None
            };
            if let (Some(anchor), Some(cursor)) = (position(*start), position(*end)) {
                editor.select_document_range(anchor, cursor);
            }
        }
        // The view's paragraphs keep a field's side padding. Lay the view out
        // that far to the leading side so the text itself starts on the
        // message column's edge; the padding is read back from the laid-out
        // paragraph, so it is the toolkit's and never a guess.
        let mut height = 0.0;
        for _ in 0..2 {
            let inset = self.body_text_inset.get();
            let rect = Rect::new(
                origin.x - inset,
                origin.y,
                origin.x + f64::from(width) + inset,
                origin.y,
            );
            height = view.layout(rect, theme, tr);
            let measured = view
                .editor_mut()
                .blocks()
                .iter()
                .find(|block| matches!(block.kind, ui::widget::BlockKind::Paragraph))
                .map_or(inset, |block| block.line.caret_rect(theme, 0).x0 - rect.x0);
            if (measured - inset).abs() < 0.25 {
                break;
            }
            self.body_text_inset.set(measured);
        }
        height
    }

    /// Presentation is projected from received facts, never a simulated timer.
    /// Provider reasoning is separate from the agent's explanation of a decision.
    pub(crate) fn agent_display_blocks(&self, m: &Message) -> Vec<ui::widget::BlockContent> {
        let mut blocks = m.blocks();
        let Some(work) = &m.work else {
            return blocks;
        };
        let Some(stream) = &work.stream else {
            return blocks;
        };
        let mut lines = Vec::new();
        {
            lines.push(crate::i18n::t(stream.status_key()));
            if stream.finish.is_none() && !stream.progress.is_empty() {
                lines.push(stream.progress.clone());
            }
            if !stream.detail.is_empty() {
                lines.push(stream.detail.clone());
            }
            if stream
                .finish
                .is_some_and(|finish| finish != crate::agent_stream::AgentFinish::Complete)
            {
                lines.push(crate::i18n::t("chat-agent-recovery"));
            }
            if self.expanded_reasoning.contains(&m.id) && !stream.reasoning.is_empty() {
                lines.push(crate::i18n::t("chat-agent-provider-reasoning"));
                lines.push(stream.reasoning.clone());
            }
        }
        blocks.push(ui::widget::BlockContent::paragraph(
            format!("{}/agent-status", m.id),
            lines.join("\n"),
        ));
        blocks
    }

    /// Paint a message's body where [`Self::layout_body`] last put it.
    fn draw_body(&self, m: &Message, paint: &mut PaintCtx<'_>) {
        if let Some((_, view)) = self.body_views.borrow_mut().get_mut(&(0, m.id.clone())) {
            view.draw(paint.scene, paint.transform, paint.text, paint.theme);
        }
    }

    /// Paint `id`'s avatar (their picture or initials in their colour) into
    /// `rect`.
    pub(crate) fn paint_avatar(&self, ctx: &mut PaintCtx<'_>, rect: Rect, id: &str) {
        let name = self.user_name(id);
        AvatarFace::new(&name, id)
            .presence(self.identity_colour(id))
            .paint(ctx, rect);
    }

    /// Paint `m`'s author through the shared author helper: the avatar (an
    /// agent's mark, a bot's tile) into `mark`, the name from `name_at`, and
    /// for a bot its `BOT` chip, which is never elided. Nothing goes past
    /// `max_x`. Returns where the line ends and the name line's height.
    fn paint_author(
        &self,
        ctx: &mut PaintCtx<'_>,
        m: &Message,
        mark: Rect,
        name_at: Point,
        max_x: f64,
    ) -> (f64, f64) {
        let name = self.message_author_name(m);
        let initiator = self.user_name(&m.author);
        // The avatar the bot showed, once the content store has it.
        let shown = self.shown_avatar(m);
        let author = self.message_author(m, &name, &initiator, shown.as_ref());
        let members: Vec<&str> = self.users.iter().map(|user| user.name.as_str()).collect();
        let theme = ctx.theme;
        let style = theme.text(TextRole::Body).strong();
        let line_h = line_of(ctx.text, &style);
        // The helper lays its own small mark before the name; start it so
        // the name lands on `name_at`, then put the mark where Chat's avatar
        // column is.
        let lead = ChipFace::height(theme) + f64::from(theme.tokens.spacing.s2);
        let origin = Point::new(name_at.x - lead, name_at.y);
        let mut line = author.layout(&members, origin, max_x - origin.x, ctx.text, theme);
        line.mark = mark;
        line.name = line.name.with_size((line.name.width(), line_h));
        if let Some(chip) = line.chip.as_mut() {
            *chip = chip.with_origin((chip.x0, name_at.y + (line_h - chip.height()) / 2.0));
        }
        author.paint(&line, ctx);
        (line.chip.map_or(line.name.x1, |chip| chip.x1), line_h)
    }

    pub(crate) fn message_author<'a>(
        &'a self,
        m: &'a Message,
        name: &'a str,
        initiator: &'a str,
        shown: Option<&'a vello::peniko::ImageData>,
    ) -> Author<'a> {
        let color = self.identity_colour(&m.author);
        if m.author.starts_with("bot:") {
            Author::Bot {
                bot_name: name,
                // The roster does not name a bot's installer yet; the chip's
                // details are not shown in Chat so far.
                installed_by: "",
                appearance: m.appearance.as_ref(),
                linked: None,
                image: shown,
            }
        } else if self.is_provider_output(m) {
            Author::Agent {
                name,
                for_person: Some(initiator),
                color,
            }
        } else if m
            .author
            .strip_prefix("task-report:")
            .is_some_and(|task_id| {
                // Only the private, typed Assistant transcript projection creates
                // these authors. Public task summaries are never assistant turns.
                self.direct_agent_tasks()
                    .and_then(AgentTaskView::current)
                    .is_some_and(|task| task.task_id == task_id)
            })
            || self.agent_authors.contains_key(&m.author)
            || m.author.strip_prefix("agent:").is_some_and(|agent| {
                self.direct_agent_tasks()
                    .and_then(AgentTaskView::current)
                    .is_some_and(|task| {
                        task.assigned_agent == agent || task.active_agent.as_deref() == Some(agent)
                    })
            })
        {
            Author::Agent {
                name,
                for_person: None,
                color,
            }
        } else {
            Author::Person {
                name,
                identity: &m.author,
                avatar: None,
                color: Some(color),
            }
        }
    }

    /// The colour `id` chose (or was given) as theirs: what their avatar,
    /// cursor and caret wear, before the theme's presence lightness.
    pub(crate) fn identity_colour(&self, id: &str) -> Color {
        let (red, green, blue) = self.participant_color(id);
        Color::from_rgb8(red, green, blue)
    }

    /// Whether the pointer is over `rect`.
    pub(crate) fn hovered(&self, rect: Rect) -> bool {
        rect.contains(self.last_mouse)
    }

    /// Whether an open context menu acts on `target`: its row stays lifted
    /// while the menu is open, so it is clear what the menu will change.
    fn menu_targets(&self, target: &MenuTarget) -> bool {
        self.menu
            .as_ref()
            .is_some_and(|menu| menu.target == *target)
    }
}

/// Human label for a presence state, shown under a member in the roster.
fn presence_label(p: Presence) -> &'static str {
    match p {
        Presence::Online => "Online",
        Presence::Idle => "Away",
        Presence::Dnd => "Do not disturb",
        Presence::Offline => "Offline",
    }
}

/// The badge a member's avatar wears for their presence state.
fn avatar_status(p: Presence) -> ui::widget::PresenceStatus {
    match p {
        Presence::Online => ui::widget::PresenceStatus::Online,
        Presence::Idle => ui::widget::PresenceStatus::Away,
        Presence::Dnd => ui::widget::PresenceStatus::Busy,
        Presence::Offline => ui::widget::PresenceStatus::Offline,
    }
}

/// A square `side` wide centred on `centre`.
fn square(centre: Point, side: f64) -> Rect {
    Rect::from_center_size(centre, (side, side))
}

impl ChatApp {
    // ------------------------------------------------------- navigation header
    /// The navigation drawer's part of the window's header row: the sidebar
    /// toggle at its top-leading corner, the workspace (or Inbox) title and
    /// the "create" action. Who is here is the desktop's to show, not
    /// the chat's.
    pub(crate) fn draw_nav_header(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let nav = self.nav_rect();
        let band = Rect::new(nav.x0, nav.y0, nav.x1, nav.y0 + self.chrome.header_h);
        let sp = &theme.tokens.spacing;
        let target = ButtonFace::height(ControlSize::Regular, theme);
        // The sidebar toggle stays in the window's header row: beside the
        // title while the drawer docks there, above it while it floats.
        let toggle = self.left_flap.toggle_rect();
        let title_x = if self.left_overlay() || toggle.width() <= 0.0 {
            band.x0 + f64::from(sp.s3)
        } else {
            toggle.x1 + f64::from(sp.s2)
        };
        let mut right = band.x1 - f64::from(sp.s3);
        let task_mode = self.direct_agent.is_some() || self.task_goal.is_some();
        if !self.inbox_open && !task_mode && self.can_participate() {
            let add = Rect::new(
                right - target,
                band.center().y - target / 2.0,
                right,
                band.center().y + target / 2.0,
            );
            Button::paint(
                scene,
                tf,
                tr,
                theme,
                &ButtonFace::icon_only(add, Icon::Plus, "Create channel or category")
                    .hovered(self.hovered(add)),
            );
            ChatApp::push_hit(hits, add, Hit::AddCategory(self.active_ws.clone()));
            right = add.x0 - f64::from(sp.s1);
        }

        let title = if task_mode {
            crate::i18n::t("chat-agents-section")
        } else if self.inbox_open {
            "Inbox".to_owned()
        } else {
            self.workspaces
                .iter()
                .find(|w| w.id == self.active_ws)
                .map(|w| w.name.clone())
                .unwrap_or_default()
        };
        let style = theme.text(TextRole::Heading);
        let mut ctx = PaintCtx::new(scene, tf, tr, theme);
        line_in(
            &mut ctx,
            &title,
            &style,
            theme.text_primary,
            title_x,
            band,
            right - title_x,
        );
    }

    // -------------------------------------------------------- destinations
    /// The sidebar's top rows: Tasks, Inbox while there are other workspaces,
    /// and the way back to this workspace's channels from either destination.
    /// One workspace is one place; there is no list of servers beside it.
    /// Returns the cursor `y` under them.
    fn draw_destinations(
        &self,
        paint: PaintCtx<'_>,
        side: Rect,
        mut y: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let elsewhere = self.workspaces.iter().any(|ws| {
            ws.id != self.context_ws
                && self
                    .channels
                    .iter()
                    .any(|c| c.workspace == ws.id && c.kind == ChannelKind::Text && !c.archived)
        });
        let task_mode = self.task_goal.is_some() || self.direct_agent.is_some();
        let row_h = nav_row_height(theme);
        y += f64::from(theme.tokens.spacing.s1);
        if (self.inbox_open || task_mode)
            && self.workspaces.iter().any(|ws| ws.id == self.context_ws)
        {
            let name = self
                .workspaces
                .iter()
                .find(|ws| ws.id == self.context_ws)
                .map_or_else(String::new, |ws| ws.name.clone());
            let row = Rect::new(side.x0, y, side.x1, y + row_h);
            let face = NavRowFace::new(row, &name)
                .icon(Icon::Hash)
                .state(SelectionState::new(false, self.hovered(row), false));
            chrome::paint_nav_row(scene, tf, tr, theme, &face);
            ChatApp::push_hit(hits, row, Hit::CurrentContext);
            y += row_h;
        }
        let row = Rect::new(side.x0, y, side.x1, y + row_h);
        chrome::paint_nav_row(
            scene,
            tf,
            tr,
            theme,
            &NavRowFace::new(row, &crate::i18n::t("chat-agents-section"))
                .icon(Icon::Plus)
                .state(SelectionState::new(task_mode, self.hovered(row), false)),
        );
        ChatApp::push_hit(hits, row, Hit::Tasks);
        y += row_h;
        if !elsewhere && !self.inbox_open {
            return y;
        }
        let mentions: u32 = self
            .channels
            .iter()
            .filter(|c| c.workspace != self.context_ws)
            .map(|c| c.mentions)
            .sum();
        let unread = self
            .channels
            .iter()
            .any(|c| c.workspace != self.context_ws && c.unread > 0);
        let row = Rect::new(side.x0, y, side.x1, y + row_h);
        let mut face = NavRowFace::new(row, "Inbox")
            .icon(Icon::Inbox)
            .unread(unread)
            .state(SelectionState::new(
                self.inbox_open,
                self.hovered(row),
                false,
            ));
        if mentions > 0 {
            face = face.count(NavCount::Live(mentions));
        }
        chrome::paint_nav_row(scene, tf, tr, theme, &face);
        ChatApp::push_hit(hits, row, Hit::Inbox);
        y + row_h
    }

    // ------------------------------------------------------------- sidebar
    pub(crate) fn draw_sidebar(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let side = self.sidebar_rect();
        if side.width() <= 0.5 {
            return;
        }
        if self.task_goal.is_some() || self.direct_agent.is_some() {
            self.draw_task_sidebar(&mut PaintCtx::new(scene, tf, tr, theme), side, hits);
            return;
        }
        self.task_search.apply_layout_external(Rect::ZERO);
        if self.inbox_open {
            self.draw_inbox_sidebar(scene, tf, tr, theme, side, hits);
            return;
        }
        // While in a call, the microphone control stays at hand here.
        let bottom = if self.call.active {
            self.draw_voice_bar(scene, tf, tr, theme, side, hits)
        } else {
            side.y1
        };
        self.draw_sidebar_tree(
            PaintCtx::new(scene, tf, tr, theme),
            presence,
            Rect::new(side.x0, side.y0, side.x1, bottom),
            hits,
        );
    }

    /// Channel and Inbox destinations stay above the task tree's bounded
    /// navigation viewport, independent of its content height.
    fn draw_task_sidebar(
        &mut self,
        paint: &mut PaintCtx<'_>,
        side: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let destinations_bottom = self.draw_destinations(
            PaintCtx::new(paint.scene, paint.transform, paint.text, paint.theme),
            side,
            side.y0,
            hits,
        );
        let rows_top = self
            .draw_agent_navigation(
                PaintCtx::new(paint.scene, paint.transform, paint.text, paint.theme),
                side,
                destinations_bottom,
                true,
                hits,
            )
            .min(side.y1);
        let height = self.task_navigation_height(paint.theme);
        let mut scroll = std::mem::replace(
            &mut self.task_navigation,
            ui::widget::ScrollRegion::vertical().with_edge_fade(),
        );
        self.task_navigation_theme.clone_from(paint.theme);
        scroll.set_viewport(Rect::new(side.x0, rows_top, side.x1, side.y1), paint.theme);
        scroll.set_content_size(side.width(), height);
        let viewport = scroll.content_rect();
        let top = viewport.y0 - scroll.offset().y;
        let content = Rect::new(viewport.x0, top, viewport.x1, top + height);
        let mut rows = Vec::new();
        scroll.paint(paint.scene, paint.transform, paint.theme, |scene, _| {
            self.draw_task_navigation_rows(
                &mut PaintCtx::new(scene, paint.transform, paint.text, paint.theme),
                content,
                top,
                &mut rows,
            );
        });
        hits.extend(rows.into_iter().filter_map(|(rect, hit)| {
            let visible = rect.intersect(viewport);
            (visible.area() > 0.0).then_some((visible, hit))
        }));
        self.task_navigation = scroll;
    }

    fn task_navigation_height(&self, theme: &Theme) -> f64 {
        let tasks = self.agent_tasks.all_tasks();
        let visible = tasks
            .iter()
            .zip(self.task_search_ranks(&tasks))
            .filter(|(task, rank)| rank.is_some() && self.task_row_depth(task, &tasks).is_some())
            .count();
        nav_item_height(theme, true, false) * visible as f64
    }

    /// Cross-workspace conversations: one section per workspace, its text
    /// channels with the most unread first.
    fn draw_inbox_sidebar(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        side: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let row_h = nav_row_height(theme);
        let mut y =
            self.draw_destinations(PaintCtx::new(scene, tf, tr, theme), side, side.y0, hits);
        y = self.draw_person_navigation(PaintCtx::new(scene, tf, tr, theme), side, y, hits);
        let mut first = y <= side.y0;
        let mut workspaces = self.workspaces.clone();
        workspaces.sort_by(|left, right| left.name.cmp(&right.name));
        for workspace in workspaces {
            let mut channels: Vec<_> = self
                .channels
                .iter()
                .filter(|channel| {
                    channel.workspace == workspace.id
                        && channel.kind == ChannelKind::Text
                        && !channel.archived
                })
                .collect();
            if channels.is_empty() {
                continue;
            }
            channels.sort_by(|left, right| {
                right
                    .unread
                    .cmp(&left.unread)
                    .then_with(|| left.order.cmp(&right.order))
            });
            let section_h = nav_section_height(theme, first);
            let header = Rect::new(side.x0, y, side.x1, y + section_h);
            chrome::paint_nav_section(
                scene,
                tf,
                tr,
                theme,
                &NavSectionFace::new(header, &workspace.name),
            );
            y += section_h;
            first = false;
            for channel in channels {
                if y > side.y1 {
                    return;
                }
                let row = Rect::new(side.x0, y, side.x1, y + row_h);
                let mut face = NavRowFace::new(row, &channel.name)
                    .icon(Icon::Hash)
                    .unread(channel.unread > 0)
                    .state(SelectionState::new(
                        channel.id == self.active_channel,
                        self.hovered(row),
                        false,
                    ));
                if channel.mentions > 0 {
                    face = face.count(NavCount::Live(channel.mentions));
                }
                chrome::paint_nav_row(scene, tf, tr, theme, &face);
                ChatApp::push_hit(hits, row, Hit::Channel(channel.id.clone()));
                y += row_h;
            }
        }
    }

    /// The channel tree: the uncategorised channels first, then each category
    /// as a collapsible section with a "new channel" action.
    fn draw_sidebar_tree(
        &mut self,
        paint: PaintCtx<'_>,
        presence: &PresenceCtx,
        side: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let mut y =
            self.draw_destinations(PaintCtx::new(scene, tf, tr, theme), side, side.y0, hits);
        y = self.draw_person_navigation(PaintCtx::new(scene, tf, tr, theme), side, y, hits);
        y = self.draw_channel_group(
            PaintCtx::new(scene, tf, tr, theme),
            presence,
            side,
            "",
            y,
            hits,
        );
        let mut first = y <= side.y0;
        let categories: Vec<_> = self.ws_categories().into_iter().cloned().collect();
        for cat in categories {
            if y > side.y1 {
                break;
            }
            let section_h = nav_section_height(theme, first);
            first = false;
            let band = Rect::new(side.x0, y, side.x1, y + section_h);
            let renaming = matches!(&self.rename, Some(MenuTarget::Category(id)) if id == &cat.id);
            let mut face = NavSectionFace::new(band, if renaming { "" } else { &cat.name })
                .collapsible(!cat.collapsed)
                .hovered(self.hovered(band));
            if self.can_participate() {
                face = face.action(Icon::Plus);
            }
            let layout = nav_section_layout(theme, &face);
            if let Some(plus) = layout.action {
                face.action_hover = self.hovered(plus);
            }
            chrome::paint_nav_section(scene, tf, tr, theme, &face);
            let toggle = Rect::new(
                band.x0,
                layout.band.y0,
                layout.action.map_or(band.x1, |a| a.x0),
                layout.band.y1,
            );
            if renaming {
                let field = Rect::new(
                    layout.title_x0,
                    layout.band.y0,
                    layout.title_x1,
                    layout.band.y1,
                );
                self.draw_inline_rename(PaintCtx::new(scene, tf, tr, theme), presence, field, hits);
            } else {
                ChatApp::push_hit(hits, toggle, Hit::CategoryToggle(cat.id.clone()));
            }
            if let Some(plus) = layout.action {
                ChatApp::push_hit(
                    hits,
                    plus,
                    Hit::AddChannel {
                        workspace: self.active_ws.clone(),
                        category: cat.id.clone(),
                    },
                );
            }
            y += section_h;
            if !cat.collapsed {
                y = self.draw_channel_group(
                    PaintCtx::new(scene, tf, tr, theme),
                    presence,
                    side,
                    &cat.id,
                    y,
                    hits,
                );
            }
        }
    }

    /// Keep private history reachable in both the global Inbox and workspace tree,
    /// before a long channel list can consume the navigation's visible area.
    fn draw_person_navigation(
        &self,
        paint: PaintCtx<'_>,
        side: Rect,
        mut y: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        if self.person_contacts.is_empty() {
            return y;
        }
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let section_h = nav_section_height(theme, y <= side.y0);
        chrome::paint_nav_section(
            scene,
            tf,
            tr,
            theme,
            &NavSectionFace::new(
                Rect::new(side.x0, y, side.x1, y + section_h),
                &crate::i18n::t("chat-contacts-section"),
            ),
        );
        y += section_h;
        for contact in &self.person_contacts {
            let row_h = nav_item_height(theme, false, false);
            let row = Rect::new(side.x0, y, side.x1, y + row_h);
            if row.y1 > side.y1 {
                break;
            }
            let face = NavRowFace::new(row, &contact.display_name).state(SelectionState::new(
                self.direct_person.as_deref() == Some(contact.public_key.as_str()),
                self.hovered(row),
                false,
            ));
            chrome::paint_nav_row(scene, tf, tr, theme, &face);
            ChatApp::push_hit(hits, row, Hit::DirectPerson(contact.public_key.clone()));
            y += row_h;
        }
        y
    }

    /// Tasks are the primary navigation; compute membership remains on the
    /// published workspace-management resources.
    fn draw_agent_navigation(
        &mut self,
        mut paint: PaintCtx<'_>,
        side: Rect,
        mut y: f64,
        first: bool,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        if y > side.y1 {
            return side.y1;
        }
        let section_h = nav_section_height(paint.theme, first);
        chrome::paint_nav_section(
            paint.scene,
            paint.transform,
            paint.text,
            paint.theme,
            &NavSectionFace::new(
                Rect::new(side.x0, y, side.x1, y + section_h),
                &crate::i18n::t("chat-agents-section"),
            ),
        );
        y += section_h;
        let row_h = nav_item_height(paint.theme, true, false);
        let row = Rect::new(side.x0, y, side.x1, y + row_h);
        let (label, detail, hit) = (
            "chat-agent-mode-queue",
            "chat-task-private-goal",
            Hit::NewTask,
        );
        chrome::paint_nav_row(
            paint.scene,
            paint.transform,
            paint.text,
            paint.theme,
            &NavRowFace::new(row, &crate::i18n::t(label)).secondary(&crate::i18n::t(detail)),
        );
        ChatApp::push_hit(hits, row, hit);
        self.draw_task_filters(&mut paint, side, y + row_h, hits)
    }

    fn draw_task_filters(
        &mut self,
        paint: &mut PaintCtx<'_>,
        side: Rect,
        mut y: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let search_h = ControlSize::Regular.height(paint.theme);
        self.task_search
            .apply_layout_external(Rect::new(side.x0, y, side.x1, y + search_h));
        self.task_search.draw(
            paint.scene,
            paint.transform,
            paint.text,
            paint.theme,
            &PresenceCtx::empty(),
        );
        y += search_h;
        let height = SegmentedFace::height(ControlSize::Small, paint.theme);
        let filters = ["all", "active", "completed", "mine"];
        let width = side.width() / filters.len() as f64;
        for (index, filter) in filters.iter().enumerate() {
            let row = Rect::new(
                side.x0 + index as f64 * width,
                y,
                side.x0 + (index + 1) as f64 * width,
                y + height,
            );
            Button::paint(
                paint.scene,
                paint.transform,
                paint.text,
                paint.theme,
                &ButtonFace::new(row, &crate::i18n::t(&format!("chat-tasks-filter-{filter}")))
                    .control_size(ControlSize::Small)
                    .toggled(self.task_filter == *filter)
                    .hovered(self.hovered(row)),
            );
            ChatApp::push_hit(hits, row, Hit::TaskFilter((*filter).into()));
        }
        y += height;
        let labels = self
            .agent_tasks
            .all_tasks()
            .iter()
            .flat_map(|task| task.labels.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>();
        if !labels.is_empty() {
            let labels = std::iter::once(String::new())
                .chain(labels)
                .collect::<Vec<_>>();
            let width = side.width() / labels.len() as f64;
            for (index, label) in labels.iter().enumerate() {
                let row = Rect::new(
                    side.x0 + index as f64 * width,
                    y,
                    side.x0 + (index + 1) as f64 * width,
                    y + height,
                );
                let text = if label.is_empty() {
                    crate::i18n::t("chat-task-all-labels")
                } else {
                    label.clone()
                };
                Button::paint(
                    paint.scene,
                    paint.transform,
                    paint.text,
                    paint.theme,
                    &ButtonFace::new(row, &text)
                        .control_size(ControlSize::Small)
                        .toggled(self.task_label == *label)
                        .hovered(self.hovered(row)),
                );
                ChatApp::push_hit(hits, row, Hit::TaskLabel(label.clone()));
            }
            y += height;
        }
        y
    }

    fn draw_task_navigation_rows(
        &self,
        paint: &mut PaintCtx<'_>,
        side: Rect,
        mut y: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let tasks = self.agent_tasks.all_tasks();
        let ranks = self.task_search_ranks(&tasks);
        let height = nav_item_height(paint.theme, true, false);
        for (task, rank) in tasks.iter().zip(ranks) {
            if rank.is_none() {
                continue;
            }
            let Some(depth) = self.task_row_depth(task, &tasks) else {
                continue;
            };
            if y + height > side.y1 {
                break;
            }
            let row = Rect::new(
                side.x0
                    + (depth as f64 * f64::from(paint.theme.tokens.spacing.s3))
                        .min(side.width() / 3.0),
                y,
                side.x1,
                y + height,
            );
            self.draw_task_navigation_row(paint, row, task, &tasks, hits);
            y += height;
        }
    }

    fn task_search_ranks(
        &self,
        tasks: &[&crate::agent_tasks::AgentTaskFrontier],
    ) -> Vec<Option<i32>> {
        ui::search::ListIndex::default().ranks(
            self.task_search.value(),
            tasks.iter().map(|task| {
                std::iter::once((task.title.as_str(), ui::search::FieldWeight::TITLE)).chain(
                    task.labels
                        .iter()
                        .map(|label| (label.as_str(), ui::search::FieldWeight::KEYWORD)),
                )
            }),
        )
    }

    fn task_row_depth(
        &self,
        task: &crate::agent_tasks::AgentTaskFrontier,
        tasks: &[&crate::agent_tasks::AgentTaskFrontier],
    ) -> Option<usize> {
        let completed = matches!(task.status.as_str(), "done" | "cancelled" | "ended");
        if (!self.task_label.is_empty() && !task.labels.contains(&self.task_label))
            || (self.task_filter == "active" && completed)
            || (self.task_filter == "completed" && !completed)
            || (self.task_filter == "mine"
                && !task
                    .responsible_assignees
                    .iter()
                    .any(|person| self.is_me(person)))
        {
            return None;
        }
        let mut depth = 0_usize;
        let mut parent = task.parent_task_id.as_deref();
        while let Some(id) = parent {
            if depth >= tasks.len() || !self.expanded_tasks.contains(id) {
                return None;
            }
            depth += 1;
            parent = tasks
                .iter()
                .find(|candidate| candidate.task_id == id)
                .and_then(|candidate| candidate.parent_task_id.as_deref());
        }
        Some(depth)
    }

    fn task_row_detail(
        &self,
        task: &crate::agent_tasks::AgentTaskFrontier,
        tasks: &[&crate::agent_tasks::AgentTaskFrontier],
    ) -> String {
        let children = tasks
            .iter()
            .filter(|child| child.parent_task_id.as_deref() == Some(task.task_id.as_str()))
            .collect::<Vec<_>>();
        let mut detail = crate::i18n::t(match task.status.as_str() {
            "queued" => "chat-agent-task-queued",
            "done" => "chat-agent-task-complete",
            "cancelled" => "chat-agent-task-cancelled",
            "ended" => "chat-agent-task-ended",
            "in_progress" => "chat-agent-task-executing",
            _ => "chat-agent-task-phase-unknown",
        });
        if !children.is_empty() {
            let done = children
                .iter()
                .filter(|child| child.status == "done")
                .count();
            detail.push_str(&crate::i18n::t_args(
                "chat-task-child-progress",
                &[
                    ("done", done.to_string().into()),
                    ("total", children.len().to_string().into()),
                ],
            ));
        }
        if !task.labels.is_empty() {
            detail.push_str(" — ");
            detail.push_str(&task.labels.join(", "));
        }
        if !task.responsible_assignees.is_empty() {
            detail.push_str(" — ");
            detail.push_str(
                &task
                    .responsible_assignees
                    .iter()
                    .map(|person| self.user_name(person))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        detail
    }

    fn draw_task_navigation_row(
        &self,
        paint: &mut PaintCtx<'_>,
        row: Rect,
        task: &crate::agent_tasks::AgentTaskFrontier,
        tasks: &[&crate::agent_tasks::AgentTaskFrontier],
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let has_children = tasks
            .iter()
            .any(|child| child.parent_task_id.as_deref() == Some(task.task_id.as_str()));
        let selected = self
            .direct_agent_tasks()
            .and_then(AgentTaskView::selected_identity)
            == task.run_epoch.map(|epoch| (task.task_id.as_str(), epoch));
        let toggle_width = SegmentedFace::height(ControlSize::Small, paint.theme);
        let label_right = if has_children {
            row.x1 - toggle_width
        } else {
            row.x1
        };
        chrome::paint_nav_row(
            paint.scene,
            paint.transform,
            paint.text,
            paint.theme,
            &NavRowFace::new(Rect::new(row.x0, row.y0, label_right, row.y1), &task.title)
                .secondary(&self.task_row_detail(task, tasks))
                .state(SelectionState::new(selected, self.hovered(row), false)),
        );
        ChatApp::push_hit(hits, row, Hit::AgentTask(Box::new(task.clone())));
        if has_children {
            let toggle = Rect::new(label_right, row.y0, row.x1, row.y1);
            let label = crate::i18n::t(if self.expanded_tasks.contains(&task.task_id) {
                "chat-task-collapse"
            } else {
                "chat-task-expand"
            });
            Button::paint(
                paint.scene,
                paint.transform,
                paint.text,
                paint.theme,
                &ButtonFace::new(toggle, &label)
                    .control_size(ControlSize::Small)
                    .hovered(self.hovered(toggle)),
            );
            ChatApp::push_hit(hits, toggle, Hit::TaskExpand(task.task_id.clone()));
        }
    }

    /// The channels of one category (`""` for the uncategorised group), each
    /// voice channel followed by the people connected to it. Returns the
    /// cursor `y` after the group.
    fn draw_channel_group(
        &mut self,
        paint: PaintCtx<'_>,
        presence: &PresenceCtx,
        side: Rect,
        category: &str,
        mut y: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let row_h = nav_row_height(theme);
        let channels: Vec<_> = self
            .category_channels(category)
            .into_iter()
            .cloned()
            .collect();
        let ws_channel_ids: Vec<_> = self.ws_channels().iter().map(|c| c.id.clone()).collect();
        for ch in &channels {
            if y > side.y1 {
                break;
            }
            // A row being swiped slides left over its delete action.
            let swipe_dx = self
                .swipe
                .filter(|s| {
                    ws_channel_ids.get(s.channel_index).map(String::as_str) == Some(ch.id.as_str())
                })
                .map_or(0.0, |s| s.dx());
            let band = Rect::new(side.x0, y, side.x1, y + row_h);
            if swipe_dx > 1.0 {
                let side_w = ButtonFace::height(ControlSize::Regular, theme);
                let reveal = square(
                    Point::new(
                        band.x1 - f64::from(theme.tokens.spacing.s2) - side_w / 2.0,
                        band.center().y,
                    ),
                    side_w,
                );
                Button::paint(
                    scene,
                    tf,
                    tr,
                    theme,
                    &ButtonFace::icon_only(reveal, Icon::Trash2, "Delete channel")
                        .role(ButtonRole::Destructive),
                );
            }
            let row = band + vello::kurbo::Vec2::new(-swipe_dx, 0.0);
            let is_voice = ch.kind == ChannelKind::Voice;
            let in_call = self.call.active && self.call.channel == ch.id;
            let selected = (ch.id == self.active_channel && !is_voice) || (is_voice && in_call);
            let renaming = matches!(&self.rename, Some(MenuTarget::Channel(id)) if id == &ch.id);
            let mut face = NavRowFace::new(row, if renaming { "" } else { &ch.name }).state(
                SelectionState::new(
                    selected,
                    self.hovered(row) || self.menu_targets(&MenuTarget::Channel(ch.id.clone())),
                    false,
                ),
            );
            face = face
                .icon(if is_voice { Icon::Volume2 } else { Icon::Hash })
                .unread(ch.unread > 0 && !is_voice);
            // Unread talk reads in the name; a mention of you is the badge.
            if ch.mentions > 0 && !is_voice {
                face = face.count(NavCount::Live(ch.mentions));
            }
            let layout = nav_row_layout(theme, tr, &face);
            chrome::paint_nav_row(scene, tf, tr, theme, &face);
            if renaming {
                let field = Rect::new(
                    layout.label_x0,
                    layout.selection.y0,
                    layout.label_x1,
                    layout.selection.y1,
                );
                self.draw_inline_rename(PaintCtx::new(scene, tf, tr, theme), presence, field, hits);
            } else {
                let hit = if is_voice {
                    Hit::VoiceChannel(ch.id.clone())
                } else {
                    Hit::Channel(ch.id.clone())
                };
                ChatApp::push_hit(hits, row, hit);
            }
            y += row_h;
            if is_voice {
                y = self.draw_voice_presence_rows(
                    PaintCtx::new(scene, tf, tr, theme),
                    side,
                    &ch.id,
                    y,
                );
            }
        }
        y
    }

    /// The people in a voice channel's call, one nested row each, as the
    /// room's presence says: seen from anywhere in the workspace.
    fn draw_voice_presence_rows(
        &self,
        paint: PaintCtx<'_>,
        side: Rect,
        channel_id: &str,
        mut y: f64,
    ) -> f64 {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let row_h = nav_row_height(theme);
        for person in self.voice_people(channel_id) {
            if y > side.y1 {
                break;
            }
            let row = Rect::new(side.x0, y, side.x1, y + row_h);
            let name = self.user_name(&person);
            let face = NavRowFace::new(row, &name)
                .depth(1)
                .avatar(AvatarFace::new(&name, &person).presence(self.identity_colour(&person)));
            chrome::paint_nav_row(scene, tf, tr, theme, &face);
            y += row_h;
        }
        y
    }

    /// The microphone control pinned under the channel list while in a call:
    /// who you are in the call and the mute toggle. Returns the y it starts at.
    fn draw_voice_bar(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        side: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let sp = &theme.tokens.spacing;
        let call_name = self
            .channel(&self.call.channel)
            .map_or_else(|| "Call".to_owned(), |c| c.name.clone());
        let you = self.user_name(&self.me.id);
        let probe = ListRowFace::new(&you).secondary(&call_name);
        let h = probe.height(tr, theme);
        let bar = Rect::new(side.x0, side.y1 - h, side.x1, side.y1);
        ui::surface::separator(
            scene,
            tf,
            bar.x0,
            bar.x1,
            bar.y0,
            f64::from(sp.s3),
            theme.tokens.colors.surfaces.content,
        );
        let mut ctx = PaintCtx::new(scene, tf, tr, theme);
        let name = self.user_name(&self.me.id);
        let avatar =
            AvatarFace::new(&name, &self.me.id).presence(self.identity_colour(&self.me.id));
        let side_w = ButtonFace::height(ControlSize::Regular, theme);
        let mute = square(
            Point::new(bar.x1 - f64::from(sp.s3) - side_w / 2.0, bar.center().y),
            side_w,
        );
        let row = Rect::new(bar.x0, bar.y0, mute.x0, bar.y1);
        ListRowFace::new(&you)
            .secondary(&call_name)
            .leading(RowLeading::Avatar(avatar))
            .paint(&mut ctx, row);
        let (icon, name) = if self.call.muted {
            (Icon::MicOff, "Unmute microphone")
        } else {
            (Icon::Mic, "Mute microphone")
        };
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &ButtonFace::icon_only(mute, icon, name).hovered(self.hovered(mute)),
        );
        ChatApp::push_hit(hits, mute, Hit::CallMute);
        bar.y0
    }

    /// The inline rename field over a channel or category label.
    fn draw_inline_rename(
        &mut self,
        paint: PaintCtx<'_>,
        presence: &PresenceCtx,
        field: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let focused = self.is_focused();
        self.rename_input.apply_layout_external(field);
        self.rename_input.set_focus(focused);
        self.rename_input.draw(scene, tf, tr, theme, presence);
        ChatApp::push_hit(hits, field, Hit::FocusRename);
    }

    // -------------------------------------------------------------- header
    pub(crate) fn header_identity_text(&self) -> HeaderIdentityText {
        let context = if self.sharing_context {
            crate::i18n::t("chat-context-sharing")
        } else if self.following_context {
            crate::i18n::t("chat-context-following")
        } else {
            String::new()
        };
        if self.task_goal.is_some() {
            return HeaderIdentityText {
                name: crate::i18n::t("chat-agent-mode-queue"),
                kind: Some(crate::i18n::t("chat-task-private-conversation")),
                context: String::new(),
            };
        }
        if let Some(peer) = &self.direct_person {
            return HeaderIdentityText {
                name: self.person_display_name(peer),
                kind: Some(crate::i18n::t("chat-person-chat-private")),
                context: String::new(),
            };
        }
        if self.direct_agent.is_some() {
            return HeaderIdentityText {
                name: self
                    .direct_agent_tasks()
                    .and_then(AgentTaskView::current)
                    .map_or_else(
                        || crate::i18n::t("chat-agent-mode-queue"),
                        |task| task.title.clone(),
                    ),
                kind: Some(crate::i18n::t("chat-task-private-conversation")),
                context: String::new(),
            };
        }

        let (name, topic) = self.channel(&self.active_channel).map_or_else(
            || (String::new(), String::new()),
            |channel| {
                let topic = if self.inbox_open {
                    self.workspaces
                        .iter()
                        .find(|workspace| workspace.id == channel.workspace)
                        .map_or_else(String::new, |workspace| workspace.name.clone())
                } else {
                    channel.topic.clone()
                };
                (channel.name.clone(), topic)
            },
        );
        HeaderIdentityText {
            name,
            kind: None,
            context: if context.is_empty() { topic } else { context },
        }
    }

    /// The conversation's part of the header row: the channel and its topic,
    /// the people button and the call button, over one hairline.
    pub(crate) fn draw_header(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let h = self.header_rect();
        let sp = &theme.tokens.spacing;
        ui::surface::separator(
            scene,
            tf,
            h.x0,
            h.x1,
            h.y1,
            0.0,
            theme.tokens.colors.surfaces.content,
        );

        // Leave room for the sidebar toggle where it stands in this part of
        // the row (the drawer is hidden or floats).
        let toggle = self.left_flap.toggle_rect();
        let left = if self.toggle_in_header() && toggle.x1 > h.x0 && !self.thread_is_page() {
            toggle.x1 + f64::from(sp.s2)
        } else {
            h.x0 + self.chrome.gutter
        };
        let controls = if self.task_goal.is_some()
            || self.direct_agent.is_some()
            || self.direct_person.is_some()
        {
            h
        } else {
            self.draw_context_stop(PaintCtx::new(scene, tf, tr, theme), h, hits)
        };
        let mut cluster_left =
            self.draw_header_action_buttons(scene, tf, tr, theme, controls, hits);
        for command in self
            .model_controls(&self.active_channel)
            .into_iter()
            .filter(|_| {
                self.direct_person.is_none()
                    && self.direct_agent.is_none()
                    && self.task_goal.is_none()
            })
        {
            let caption = crate::i18n::t(&format!("chat-model-{command}"));
            let height = ButtonFace::height(ControlSize::Small, theme);
            let width =
                width_of(tr, &caption, &theme.text(TextRole::Label)) + 2.0 * f64::from(sp.s3);
            let rect = Rect::new(
                cluster_left - width - f64::from(sp.s2),
                h.center().y - height / 2.0,
                cluster_left - f64::from(sp.s2),
                h.center().y + height / 2.0,
            );
            if rect.x0 < left + f64::from(sp.s16) {
                break;
            }
            Button::paint(
                scene,
                tf,
                tr,
                theme,
                &ButtonFace::new(rect, &caption).control_size(ControlSize::Small),
            );
            Self::push_hit(hits, rect, Hit::HostModel(command.into()));
            cluster_left = rect.x0;
        }

        let mut ctx = PaintCtx::new(scene, tf, tr, theme);
        let max = (cluster_left - f64::from(sp.s3) - left).max(0.0);
        let identity = self.header_identity_text();
        Self::draw_header_identity(&mut ctx, identity, h, left, max, cluster_left);
    }

    fn draw_header_identity(
        ctx: &mut PaintCtx<'_>,
        identity: HeaderIdentityText,
        h: Rect,
        left: f64,
        max: f64,
        cluster_left: f64,
    ) {
        let sp = &ctx.theme.tokens.spacing;
        let title_style = ctx.theme.text(TextRole::Heading);
        let topic_style = ctx.theme.text(TextRole::Body);
        let text_primary = ctx.theme.text_primary;
        let text_dim = ctx.theme.text_dim;
        if let Some(kind) = identity.kind {
            let gap = f64::from(sp.s3);
            let full_identity_width = width_of(ctx.text, &identity.name, &title_style)
                + gap
                + width_of(ctx.text, &kind, &topic_style);
            let stacked_name_style = ctx.theme.text(TextRole::Label);
            let stacked_height =
                f64::from(stacked_name_style.line_height_px() + topic_style.line_height_px());
            if full_identity_width > max && stacked_height <= h.height() {
                let y0 = h.center().y - stacked_height / 2.0;
                let name_band = Rect::new(
                    h.x0,
                    y0,
                    h.x1,
                    y0 + f64::from(stacked_name_style.line_height_px()),
                );
                let kind_band = Rect::new(
                    name_band.x0,
                    name_band.y1,
                    name_band.x1,
                    y0 + stacked_height,
                );
                line_in(
                    ctx,
                    &identity.name,
                    &stacked_name_style,
                    text_primary,
                    left,
                    name_band,
                    max,
                );
                line_in(ctx, &kind, &topic_style, text_dim, left, kind_band, max);
                return;
            }
            let widths = agent_header_widths(ctx.text, ctx.theme, max, &kind);
            let name_width = line_in(
                ctx,
                &identity.name,
                &title_style,
                text_primary,
                left,
                h,
                widths.name,
            );
            let kind_x = if name_width > 0.0 {
                left + name_width + gap
            } else {
                left
            };
            let kind_avail = (left + max - kind_x).max(0.0).min(widths.kind);
            line_in(ctx, &kind, &topic_style, text_dim, kind_x, h, kind_avail);
            let context_x = kind_x + kind_avail + gap;
            let context_avail = left + max - context_x;
            if !identity.context.is_empty()
                && width_of(ctx.text, &identity.context, &topic_style) <= context_avail
            {
                line_in(
                    ctx,
                    &identity.context,
                    &topic_style,
                    text_dim,
                    context_x,
                    h,
                    context_avail,
                );
            }
        } else {
            let drawn = line_in(
                ctx,
                &identity.name,
                &title_style,
                text_primary,
                left,
                h,
                max,
            );
            let tx = left + drawn + f64::from(sp.s3);
            let avail = cluster_left - f64::from(sp.s3) - tx;
            if !identity.context.is_empty()
                && width_of(ctx.text, &identity.context, &topic_style) <= avail
            {
                line_in(ctx, &identity.context, &topic_style, text_dim, tx, h, avail);
            }
        }
    }

    /// An immediate stop stays visible while navigation is being published.
    /// Its face is the shared Button, and its input uses the public effect.
    fn draw_context_stop(
        &self,
        paint: PaintCtx<'_>,
        header: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> Rect {
        if !self.sharing_context {
            return header;
        }
        let PaintCtx {
            scene,
            transform,
            text,
            theme,
        } = paint;
        let caption = crate::i18n::t("chat-context-stop");
        let probe = ButtonFace::new(Rect::ZERO, &caption);
        let width = probe.intrinsic_width(text, theme);
        let height = ButtonFace::height(ControlSize::Regular, theme);
        let edge = header.x1 - self.chrome.gutter;
        let rect = Rect::new(
            edge - width,
            header.center().y - height / 2.0,
            edge,
            header.center().y + height / 2.0,
        );
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(rect, &caption).hovered(self.hovered(rect)),
        );
        Self::push_hit(hits, rect, Hit::StopSharingLiveContext);
        Rect::new(
            header.x0,
            header.y0,
            rect.x0 - f64::from(theme.tokens.spacing.s2),
            header.y1,
        )
    }

    /// Whether the sidebar toggle stands in the conversation header (the
    /// navigation drawer is hidden or floats) rather than on the drawer.
    pub(crate) fn toggle_in_header(&self) -> bool {
        !self.left_flap_open() || self.left_overlay()
    }

    /// The trailing call and people buttons. Returns where their cluster
    /// starts, so the title and topic elide before it.
    fn draw_header_action_buttons(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        h: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        if self.direct_person.is_some() || self.direct_agent.is_some() || self.task_goal.is_some() {
            return self.draw_private_header_buttons(
                &mut PaintCtx::new(scene, tf, tr, theme),
                h,
                hits,
            );
        }
        let control = ButtonFace::height(ControlSize::Regular, theme);
        let right = h.x1 - self.chrome.gutter;
        let search = square(Point::new(right - control / 2.0, h.center().y), control);
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &ButtonFace::icon_only(search, Icon::Search, &crate::i18n::t("chat-header-search"))
                .hovered(self.hovered(search)),
        );
        ChatApp::push_hit(hits, search, Hit::Search);
        if !self.has_other_collaborator() {
            return search.x0;
        }
        self.draw_human_header_buttons(
            PaintCtx::new(scene, tf, tr, theme),
            (
                search.x0 - f64::from(theme.tokens.spacing.s2),
                h.center().y,
                control,
            ),
            hits,
        )
    }

    fn draw_private_header_buttons(
        &self,
        paint: &mut PaintCtx<'_>,
        h: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let control = ButtonFace::height(ControlSize::Regular, paint.theme);
        let gap = f64::from(paint.theme.tokens.spacing.s2);
        let mut right = h.x1 - self.chrome.gutter;
        if self.header_has_search(paint.text, paint.theme, h, control, true) {
            let search = square(Point::new(right - control / 2.0, h.center().y), control);
            Button::paint(
                paint.scene,
                paint.transform,
                paint.text,
                paint.theme,
                &ButtonFace::icon_only(search, Icon::Search, &crate::i18n::t("chat-header-search"))
                    .hovered(self.hovered(search)),
            );
            ChatApp::push_hit(hits, search, Hit::Search);
            right = search.x0 - gap;
        }
        let window = square(Point::new(right - control / 2.0, h.center().y), control);
        let label = crate::i18n::t(if self.conversation_surface {
            "chat-agent-return"
        } else {
            "chat-agent-float"
        });
        Button::paint(
            paint.scene,
            paint.transform,
            paint.text,
            paint.theme,
            &ButtonFace::icon_only(window, Icon::ExternalLink, &label)
                .hovered(self.hovered(window)),
        );
        ChatApp::push_hit(hits, window, Hit::AgentWindow);
        if self.conversation_surface {
            return window.x0;
        }
        let drag = square(
            Point::new(window.x0 - gap - control / 2.0, h.center().y),
            control,
        );
        ui::surface::floating_drag_grip(paint.scene, paint.transform, drag, paint.theme.text_dim);
        ChatApp::push_hit(hits, drag, Hit::AgentDragHandle);
        drag.x0
    }

    fn header_has_search(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
        header: Rect,
        control: f64,
        agent_header: bool,
    ) -> bool {
        if !agent_header {
            return true;
        }
        let sp = &theme.tokens.spacing;
        let identity = self.header_identity_text();
        let name_width = width_of(tr, &identity.name, &theme.text(TextRole::Heading));
        let identity_width = identity.kind.as_ref().map_or(name_width, |kind| {
            name_width + f64::from(sp.s3) + width_of(tr, kind, &theme.text(TextRole::Body))
        });
        let agent_actions = control + f64::from(sp.s2);
        let search_room = 2.0 * self.chrome.gutter
            + identity_width
            + f64::from(sp.s3)
            + agent_actions
            + f64::from(sp.s2)
            + control;
        header.width() >= search_room
    }

    fn draw_human_header_buttons(
        &self,
        paint: PaintCtx<'_>,
        (right, cy, control): (f64, f64, f64),
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        // Ordinary channels retain the call entry point; private Task and
        // person conversations use their separate header above.
        let mut right = right;
        {
            let call_label = crate::i18n::t(if self.call.active {
                "chat-header-call-open"
            } else {
                "chat-header-call-start"
            });
            let probe = ButtonFace::new(Rect::ZERO, &call_label);
            let call_w = probe.intrinsic_width(tr, theme);
            let call = Rect::new(
                right - call_w,
                cy - control / 2.0,
                right,
                cy + control / 2.0,
            );
            Button::paint(
                scene,
                tf,
                tr,
                theme,
                &ButtonFace::new(call, &call_label).hovered(self.hovered(call)),
            );
            ChatApp::push_hit(hits, call, Hit::StartCall);
            right = call.x0 - f64::from(theme.tokens.spacing.s2);
        }

        let pins = square(Point::new(right - control / 2.0, cy), control);
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &ButtonFace::icon_only(pins, Icon::Pin, "Pinned messages")
                .hovered(self.hovered(pins))
                .pressed(self.pins_open && self.right_flap_open() && self.thread_open.is_none()),
        );
        ChatApp::push_hit(hits, pins, Hit::TogglePins);
        let right = pins.x0 - f64::from(theme.tokens.spacing.s2);
        let people = square(Point::new(right - control / 2.0, cy), control);
        let open = self.right_flap_open() && self.thread_open.is_none() && !self.pins_open;
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &ButtonFace::icon_only(people, Icon::Users, self.roster_label())
                .hovered(self.hovered(people))
                .pressed(open),
        );
        ChatApp::push_hit(hits, people, Hit::ToggleRightFlap);
        people.x0
    }

    /// The sidebar toggle at the header row's leading corner, where the
    /// drawer's `header` placed it: over the docked drawer's title row, or
    /// at the conversation header's leading edge while the drawer is hidden
    /// or floats under the row. A thread pushed over a narrow window has its
    /// own Back control instead.
    pub(crate) fn draw_flap_toggles(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let toggle = self.left_flap.toggle_rect();
        if self.thread_is_page() || toggle.width() <= 0.0 {
            return;
        }
        self.left_flap
            .draw_toggle(scene, tf, tr, theme, toggle, self.hovered(toggle));
        ChatApp::push_hit(hits, toggle, Hit::ToggleLeftFlap);
    }

    // ------------------------------------------------------------ messages
    fn draw_person_messages(
        &mut self,
        mut paint: PaintCtx<'_>,
        presence: &PresenceCtx,
        area: Rect,
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
            line_in(
                &mut paint, &author, &style, color, heading.x0, heading, width,
            );
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
        let agent_name = self
            .direct_agent
            .as_deref()
            .map(|agent| self.agent_display_name(agent));
        let (direct_title, direct_message) = agent_name.as_deref().map_or_else(
            || (None, None),
            |name| {
                let name_arg = [("name", name.into())];
                if !self.direct_agent_is_available() {
                    return (
                        Some(crate::i18n::t_args(
                            "chat-agent-empty-unavailable-title",
                            &name_arg,
                        )),
                        Some(crate::i18n::t("chat-agent-unavailable")),
                    );
                }
                match self.direct_agent_intent() {
                    AgentComposerIntent::Queue => (
                        Some(crate::i18n::t_args(
                            "chat-agent-empty-queue-title",
                            &name_arg,
                        )),
                        Some(crate::i18n::t("chat-agent-empty-queue-message")),
                    ),
                    AgentComposerIntent::Steer => (
                        Some(crate::i18n::t_args(
                            "chat-agent-empty-steer-title",
                            &name_arg,
                        )),
                        Some(crate::i18n::t("chat-agent-empty-steer-message")),
                    ),
                }
            },
        );
        let goal_title = self
            .task_goal
            .as_ref()
            .map(|_| crate::i18n::t("chat-task-private-goal"));
        let title = if let Some(title) = goal_title.as_deref() {
            title
        } else if let Some(title) = direct_title.as_deref() {
            title
        } else if self.can_participate() {
            "No messages yet"
        } else {
            "Nothing has been said here yet"
        };
        let content = EmptyStateContent {
            icon: self.direct_agent.is_none().then_some(Icon::Send),
            message: direct_message.as_deref(),
            ..EmptyStateContent::new(title)
        };
        EmptyState::paint(
            &mut PaintCtx::new(scene, tf, tr, theme),
            area,
            &content,
            self.last_mouse,
        );
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
        let text_w =
            (area.x1 - text_x - self.chrome.gutter).max(f64::from(theme.tokens.spacing.s16)) as f32;
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

        let groups = model::group_messages(&self.messages);
        let divider_h = nav_row_height(theme);
        let (rows, total) = self.measure_message_rows(tr, theme, &groups, text_w, divider_h);

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
            // A message shown from search, or one opened in the editor.
            if let Some(target) = self.revealed_message.as_deref().or(self.editing.as_deref())
                && let Some(target_index) = rows.iter().position(|row| row.idx_id == target)
            {
                let before = rows
                    .iter()
                    .take(target_index)
                    .map(RowM::height)
                    .sum::<f64>();
                if let Some(row) = rows.get(target_index) {
                    let target_mid = divider_h + before + row.height() / 2.0;
                    self.scroll = area.center().y - start_y - target_mid;
                }
            }
            // A result can disappear between search and paint. Either way,
            // make this a one-shot request so a stale selection never fights
            // normal reader-driven scrolling on later frames.
            self.reveal_scroll_pending = false;
        }
        self.scroll = self.scroll.clamp(0.0, (total - area.height()).max(0.0));
        let y = (start_y + self.scroll).clamp(min_start, max_start);
        // Store the normalised (bounded) offset, not the raw wheel accumulator.
        // This prevents an unbounded value from leaking across a resize or a
        // channel switch, and gives sync an accurate read-edge answer.
        self.scroll = y - start_y;
        // `scroll == 0` is the newest edge: `start_y` starts the timeline at
        // its bottom-aligned position, while increasing offsets travel back
        // through history toward `max_start`.
        self.timeline_at_end = self.scroll <= 0.5;
        self.timeline_total_height = total;

        let mut y = self.draw_messages_date_divider(
            PaintCtx::new(scene, tf, tr, theme),
            area,
            y,
            divider_h,
        );

        let first_row_hit = hits.len();
        for rm in &rows {
            self.draw_row(
                PaintCtx::new(scene, tf, tr, theme),
                presence,
                rm,
                column,
                &mut y,
                hits,
            );
        }
        // A row scrolled half out of the timeline is clipped where it is
        // painted; so is what it answers to, never reaching the header.
        let rows_hits = hits.split_off(first_row_hit);
        hits.extend(rows_hits.into_iter().filter_map(|(rect, hit)| {
            let clipped = rect.intersect(area);
            (clipped.width() > 0.0 && clipped.height() > 0.0).then_some((clipped, hit))
        }));
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
        let label = "Jump to latest";
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

    /// Measure every row across every group (divider + rows), returning the
    /// per-row measurements and the total timeline height.
    fn measure_message_rows(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
        groups: &[model::MessageGroup],
        text_w: f32,
        divider_h: f64,
    ) -> (Vec<RowM>, f64) {
        let mut total = divider_h;
        let mut rows: Vec<RowM> = Vec::new();
        for g in groups {
            for (gi, &idx) in g.message_indices.iter().enumerate() {
                let Some(m) = self.messages.get(idx) else {
                    continue;
                };
                let rm = self.measure_row(tr, theme, m, gi == 0, text_w);
                total += rm.height();
                rows.push(rm);
            }
        }
        (rows, total)
    }

    /// The day the first message was sent, centred between two hairlines.
    /// Returns the cursor `y` for the first message row beneath it.
    fn draw_messages_date_divider(
        &self,
        paint: PaintCtx<'_>,
        area: Rect,
        y: f64,
        divider_h: f64,
    ) -> f64 {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let label = self.messages.first().map_or_else(
            || ui::format::near_day(0).unwrap_or_default(),
            |m| day_label(m.ts),
        );
        let style = theme.text(TextRole::Caption);
        let w = width_of(tr, &label, &style);
        let band = Rect::new(area.x0, y, area.x1, y + divider_h);
        let gap = f64::from(theme.tokens.spacing.s3);
        let ground = theme.tokens.colors.surfaces.content;
        let cx = band.center().x;
        let x0 = area.x0 + self.chrome.gutter;
        let x1 = area.x1 - self.chrome.gutter;
        ui::surface::separator(
            scene,
            tf,
            x0,
            cx - w / 2.0 - gap,
            band.center().y,
            0.0,
            ground,
        );
        ui::surface::separator(
            scene,
            tf,
            cx + w / 2.0 + gap,
            x1,
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
            cx - w / 2.0,
            band,
            w + 1.0,
        );
        y + divider_h
    }

    fn measure_row(
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
            ChipFace::height(theme) + f64::from(sp.s2)
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
            resource_h: if !editing && m.reference.is_some() && m.source_provider.is_some() {
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

    fn draw_row(
        &mut self,
        paint: PaintCtx<'_>,
        presence: &PresenceCtx,
        rm: &RowM,
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
        let area = column.area;
        let Some(m) = self.message(&rm.idx_id).cloned() else {
            return;
        };
        let revealed = self.revealed_message.as_deref() == Some(m.id.as_str());
        let targeted = self.menu_targets(&MenuTarget::Message(m.id.clone()));
        if revealed || targeted {
            let s2 = f64::from(theme.tokens.spacing.s2);
            chrome::paint_row_selection(
                scene,
                tf,
                Rect::new(area.x0 + s2, *y, area.x1 - s2, *y + rm.height()),
                SelectionState::new(revealed, targeted, false),
                theme.tokens.colors.surfaces.content,
                theme,
            );
        }
        *y += rm.top_gap;
        let content_top = *y;

        if rm.show_header {
            self.draw_row_header(PaintCtx::new(scene, tf, tr, theme), column, &m, y);
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
            let caption = crate::i18n::t(if self.expanded_reasoning.contains(&m.id) {
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
            Self::push_hit(hits, rect, Hit::AgentReasoning(m.id.clone()));
            *y += rm.reasoning_h;
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
        if rm.resource_h > 0.0 {
            self.draw_resource_navigation(PaintCtx::new(scene, tf, tr, theme), &m, column, y, hits);
        }
        self.draw_row_footer(PaintCtx::new(scene, tf, tr, theme), rm, &m, column, y, hits);
        *y += rm.pad;
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
        for (open, hit) in [
            (false, Hit::CopyResourceLink(m.id.clone())),
            (true, Hit::OpenResourceLink(m.id.clone())),
        ] {
            *y += gap;
            let caption = self.resource_navigation.label(&m.id, open, false);
            let width = ButtonFace::new(Rect::ZERO, &caption)
                .intrinsic_width(tr, theme)
                .min(f64::from(column.text_width));
            let rect = Rect::new(column.text_x, *y, column.text_x + width, *y + height);
            Button::paint(
                scene,
                tf,
                tr,
                theme,
                &ButtonFace::new(rect, &caption).hovered(self.hovered(rect)),
            );
            ChatApp::push_hit(hits, rect, hit);
            *y += height;
        }
    }

    /// A message group's header: avatar, author and time. Advances `*y`
    /// past it.
    fn draw_row_header(
        &self,
        paint: PaintCtx<'_>,
        column: MessageColumn,
        m: &Message,
        y: &mut f64,
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
        let (end_x, nh) = self.paint_author(
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
        text(
            &mut ctx,
            &clock,
            &time_style,
            theme.text_dim,
            Point::new(clock_x, *y + nh - th),
            None,
        );
        // A pinned message says so where its time is.
        if m.pinned {
            let icon = f64::from(theme.tokens.metrics.icon_sm);
            let x = clock_x
                + width_of(ctx.text, &clock, &time_style)
                + f64::from(theme.tokens.spacing.s2);
            let cy = *y + nh - th / 2.0;
            Icon::Pin.draw(
                ctx.scene,
                tf,
                Rect::new(x, cy - icon / 2.0, x + icon, cy + icon / 2.0),
                theme.text_dim,
            );
            text(
                &mut ctx,
                "Pinned",
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
    fn draw_agent_work(mut paint: PaintCtx<'_>, work: &AgentWork, card: Rect) {
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
            let label = "Edited";
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
            self.draw_attachment(
                PaintCtx::new(scene, tf, tr, theme),
                att,
                Point::new(text_x, *y),
                f64::from(text_w),
            );
            *y += rm.att_h;
        }

        if !m.reactions.is_empty() {
            self.draw_reactions(PaintCtx::new(scene, tf, tr, theme), m, text_x, *y, hits);
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
        self.edit_input.set_placeholder("Edit message");
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
        let save_face = ButtonFace::new(Rect::ZERO, "Save").control_size(ControlSize::Small);
        let save_w = save_face.intrinsic_width(tr, theme);
        let save = Rect::new(field.x1 - save_w, *y, field.x1, *y + h);
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &ButtonFace::new(save, "Save")
                .role(ButtonRole::Primary)
                .control_size(ControlSize::Small)
                .hovered(self.hovered(save)),
        );
        ChatApp::push_hit(hits, save, Hit::EditSave);
        let cancel_w = ButtonFace::new(Rect::ZERO, "Cancel")
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
            &ButtonFace::new(cancel, "Cancel")
                .control_size(ControlSize::Small)
                .hovered(self.hovered(cancel)),
        );
        ChatApp::push_hit(hits, cancel, Hit::EditCancel);
        *y += rm.edit_ctrl_h;
    }

    /// One chip per reaction (yours selected), then a chip to add another.
    fn draw_reactions(
        &self,
        paint: PaintCtx<'_>,
        m: &Message,
        text_x: f64,
        y: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let chip_h = ChipFace::height(theme);
        let gap = f64::from(theme.tokens.spacing.s2);
        let mut x = text_x;
        for reaction in &m.reactions {
            let mine = reaction.reacted_by(&self.me.id);
            let label = format!("{} {}", reaction.emoji, reaction.count());
            let chip_w = ChipFace::intrinsic_width(&label, ChipLead::None, false, tr, theme);
            let chip = Rect::new(x, y, x + chip_w, y + chip_h);
            ChipFace::new(chip, &label)
                .selected(mine)
                .hovered(self.hovered(chip))
                .paint(scene, tf, tr, theme);
            ChatApp::push_hit(
                hits,
                chip,
                Hit::Reaction {
                    msg: m.id.clone(),
                    emoji: reaction.emoji.clone(),
                },
            );
            x = chip.x1 + gap;
        }
        let lead = ChipLead::Glyph(Icon::Plus);
        let add_w = ChipFace::intrinsic_width("", lead, false, tr, theme);
        let add = Rect::new(x, y, x + add_w, y + chip_h);
        ChipFace::new(add, "")
            .lead(lead)
            .hovered(self.hovered(add))
            .paint(scene, tf, tr, theme);
        ChatApp::push_hit(
            hits,
            add,
            Hit::AddReaction {
                msg: m.id.clone(),
                anchor: Point::new(add.x0, add.y1 + gap / 2.0),
            },
        );
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
        let replies = format!("{count} {}", if count == 1 { "reply" } else { "replies" });
        let last = self
            .messages
            .iter()
            .filter(|reply| reply.parent.as_deref() == Some(m.id.as_str()))
            .map(|reply| reply.ts)
            .max()
            .map(|ts| format!("Last reply {}", clock_label(ts)));
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

    // ---------------------------------------------------------- attachments
    /// The vertical space an attachment takes in a column `max_w` wide,
    /// including the gap before what follows it.
    fn attachment_flow_height(
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

    fn draw_attachment(&self, paint: PaintCtx<'_>, att: &Attachment, origin: Point, max_w: f64) {
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
            paint_file_card(
                &mut ctx,
                origin,
                max_w,
                icon,
                att.name(),
                &crate::i18n::t("chat-attachment-no-access"),
            );
            return;
        }
        match att {
            Attachment::File { name, size, .. } => {
                paint_file_card(
                    &mut ctx,
                    origin,
                    max_w,
                    Icon::FileText,
                    name,
                    &format_bytes(*size),
                );
            }
            Attachment::Image { blob, .. } => {
                let image = self.images.get(&crate::images::blob_key(blob));
                let status = if image.is_some() {
                    None
                } else if blob.starts_with(crate::images::DEMO_SCHEME) {
                    Some("Loading")
                } else {
                    Some(match self.content.state(blob) {
                        tilo_content::ContentState::Pending => "Loading",
                        tilo_content::ContentState::Unavailable => "Unavailable",
                        tilo_content::ContentState::Resolved => "Opening",
                    })
                };
                let mut meta = media_meta(att);
                if let Some(status) = status {
                    meta.push(status.to_owned());
                }
                let art = image
                    .as_ref()
                    .map_or(MediaArt::Placeholder, MediaArt::Image);
                let (w, face) = media_face(att, art, &meta, max_w, theme);
                let h = face.height(ctx.text, theme, w);
                face.paint(
                    &mut ctx,
                    Rect::new(origin.x, origin.y, origin.x + w, origin.y + h),
                );
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
                face.paint(
                    &mut ctx,
                    Rect::new(origin.x, origin.y, origin.x + w, origin.y + h),
                );
            }
        }
    }

    // ------------------------------------------------------------ composer
    pub(crate) fn draw_composer(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let rect = self.composer_rect(tr, theme);
        // With no channel open a draft would have nowhere to go, so there is
        // no composer to type into.
        if self.direct_agent.is_none()
            && self.direct_person.is_none()
            && self.task_goal.is_none()
            && self.channel(&self.active_channel).is_none()
        {
            self.composer_main.set_focus(false);
            return;
        }
        if !self.can_participate()
            && self.direct_agent.is_none()
            && self.direct_person.is_none()
            && self.task_goal.is_none()
        {
            self.composer_main.set_focus(false);
            Self::draw_read_only_composer(scene, tf, tr, theme, rect);
            return;
        }
        let interactive = self.main_composer_can_edit();
        self.composer_main.set_read_only(!interactive);
        // While an inline message edit is in progress the caret lives in the
        // editor, not the composer.
        let focused = interactive
            && self.is_focused()
            && matches!(self.focus, crate::app::Focus::Main)
            && self.editing.is_none();
        let draft = self.composer_main.value();
        let input = if self.task_goal.is_some() {
            let height = (self.composer_main.measured_height() + self.chrome.composer_min_h
                - self.chrome.body_line)
                .clamp(self.chrome.composer_min_h, self.chrome.composer_max_h)
                .min(rect.height());
            let input = Rect::new(rect.x0, rect.y1 - height, rect.x1, rect.y1);
            self.draw_task_goal_controls(
                PaintCtx::new(scene, tf, tr, theme),
                Rect::new(rect.x0, rect.y0, rect.x1, input.y0),
                hits,
            );
            input
        } else if self.direct_agent.is_some() {
            let pad = self.chrome.composer_min_h - self.chrome.body_line;
            let input_h = (self.composer_main.measured_height() + pad)
                .clamp(self.chrome.composer_min_h, self.chrome.composer_max_h)
                .min(rect.height());
            let input = Rect::new(rect.x0, rect.y1 - input_h, rect.x1, rect.y1);
            self.draw_agent_task_controls(
                PaintCtx::new(scene, tf, tr, theme),
                Rect::new(rect.x0, rect.y0, rect.x1, input.y0),
                hits,
            );
            input
        } else {
            rect
        };
        if self.direct_agent.is_some() {
            let placeholder = match self.direct_agent_intent() {
                AgentComposerIntent::Queue => crate::i18n::t("chat-agent-task-placeholder"),
                AgentComposerIntent::Steer => crate::i18n::t("chat-agent-steer-placeholder"),
            };
            // The shared editor paints its empty hint on one line. Measure
            // against its actual text slot, not the whole composer field.
            let width = composer_editor_width(input.width(), theme, false);
            let placeholder = elide(tr, &placeholder, &theme.text(TextRole::Body), width);
            self.composer_main.set_placeholder(&placeholder);
        }
        let send_enabled = interactive && !draft.trim().is_empty();
        let inner = self.draw_input_box(
            PaintCtx::new(scene, tf, tr, theme),
            input,
            &draft,
            (
                send_enabled,
                self.direct_agent.is_none()
                    && self.direct_person.is_none()
                    && self.task_goal.is_none(),
                interactive,
            ),
            Hit::FocusMain,
            hits,
        );
        self.composer_main
            .set_mention_paint(self.mention_paint(theme));
        self.composer_main.set_mention_people(self.mention_people());
        self.composer_main.set_popover_bounds(self.rect);
        self.composer_main.layout(inner, theme, tr);
        self.composer_main.set_focus(focused);
        self.composer_main.draw(scene, tf, tr, theme, presence);
        let key = ChatApp::draft_presence_key(&self.active_channel);
        // How a file being shared is doing, while the picker is closed: an
        // upload under way, or why it was not shared.
        let upload = self.attachment_status.as_deref().filter(|status| {
            !self.attach_open
                && (status.starts_with("Uploading")
                    || status.starts_with("Not shared")
                    || status.starts_with("Couldn't")
                    || status.starts_with("The file chooser"))
        });
        if let Some(reason) = &self.not_sent {
            Self::draw_not_sent(PaintCtx::new(scene, tf, tr, theme), rect, reason, hits);
        } else if let Some(status) = upload {
            let style = theme.text(TextRole::Caption);
            let ink = if status.starts_with("Uploading") {
                theme.text_dim
            } else {
                theme.tokens.colors.semantic.danger
            };
            let shown = elide(tr, status, &style, rect.width());
            tr.draw_style(
                scene,
                &shown,
                ink,
                tf.pre_translate(vello::kurbo::Vec2::new(
                    rect.x0,
                    rect.y1 + f64::from(theme.tokens.spacing.s1),
                )),
                None,
                &style,
            );
        } else if self.direct_person.is_none()
            && self.direct_agent.is_none()
            && self.task_goal.is_none()
        {
            Self::draw_draft_presence(&self.composer_main, &key, scene, tf, tr, theme, presence);
        }
    }

    /// Under the composer, while a message was refused: that it was not
    /// sent, why, and a Retry that sends the kept text again.
    fn draw_not_sent(paint: PaintCtx<'_>, field: Rect, reason: &str, hits: &mut Vec<(Rect, Hit)>) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let style = theme.text(TextRole::Caption);
        let y = field.y1 + f64::from(theme.tokens.spacing.s1);
        let retry = "Retry";
        let strong = style.clone().strong();
        let retry_w = width_of(tr, retry, &strong);
        let line = elide(
            tr,
            &format!("Not sent: {reason}"),
            &style,
            (field.width() - retry_w - f64::from(theme.tokens.spacing.s3)).max(0.0),
        );
        let line_w = width_of(tr, &line, &style);
        let danger = theme.tokens.colors.semantic.danger;
        tr.draw_style(
            scene,
            &line,
            danger,
            tf.pre_translate(vello::kurbo::Vec2::new(field.x0, y)),
            None,
            &style,
        );
        let x = field.x0 + line_w + f64::from(theme.tokens.spacing.s3);
        let h = line_of(tr, &style);
        tr.draw_style(
            scene,
            retry,
            theme.text_primary,
            tf.pre_translate(vello::kurbo::Vec2::new(x, y)),
            None,
            &strong,
        );
        ChatApp::push_hit(hits, Rect::new(x, y, x + retry_w, y + h), Hit::Send);
    }

    fn draw_agent_task_controls(
        &self,
        mut paint: PaintCtx<'_>,
        rect: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let Some(tasks) = self.direct_agent_tasks() else {
            return;
        };
        let gap = f64::from(paint.theme.tokens.spacing.s2);
        let row_h = SegmentedFace::height(ControlSize::Small, paint.theme);
        let lines = agent_task_status_lines(tasks);
        let mode_rows = self.agent_intent_picker_rows(paint.text, paint.theme, rect.width());
        let mode_h = mode_rows as f64 * row_h + mode_rows.saturating_sub(1) as f64 * gap;
        let mode = Rect::new(rect.x0, (rect.y1 - mode_h).max(rect.y0), rect.x1, rect.y1);
        self.draw_agent_intent_picker(
            paint.scene,
            paint.transform,
            paint.text,
            paint.theme,
            mode,
            tasks,
            hits,
        );
        let status_bottom = mode.y0 - gap;
        let status_h = self.agent_task_status_height(paint.text, paint.theme, rect.width(), tasks);
        let status = Rect::new(
            rect.x0,
            (status_bottom - status_h).max(rect.y0),
            rect.x1,
            status_bottom.max(rect.y0),
        );
        let action = self.agent_task_run_control(tasks);
        let action_w = if let Some((label, _)) = &action {
            ButtonFace::new(Rect::ZERO, label)
                .control_size(ControlSize::Small)
                .intrinsic_width(paint.text, paint.theme)
        } else {
            0.0
        };
        let text_right = if action.is_some() {
            status.x1 - action_w - gap
        } else {
            status.x1
        };
        let caption = paint.theme.text(TextRole::Caption);
        let ink = paint.theme.text_dim;
        paint.text.draw_style(
            paint.scene,
            &lines.primary,
            paint.theme.text_primary,
            paint.transform * Affine::translate((status.x0, status.y0)),
            Some((text_right - status.x0).max(1.0) as f32),
            &paint.theme.text(TextRole::Body),
        );
        if let Some((label, hit)) = action {
            let control = Rect::new(
                status.x1 - action_w,
                status.y0,
                status.x1,
                (status.y0 + row_h).min(status.y1),
            );
            Button::paint(
                paint.scene,
                paint.transform,
                paint.text,
                paint.theme,
                &ButtonFace::new(control, &label)
                    .control_size(ControlSize::Small)
                    .hovered(self.hovered(control)),
            );
            ChatApp::push_hit(hits, control, hit);
        }
        let mut chooser_bottom = status.y0 - gap;
        if let Some(receipt) = lines.receipt {
            let row = Rect::new(
                rect.x0,
                (chooser_bottom
                    - Self::agent_task_receipt_height(
                        paint.text,
                        paint.theme,
                        rect.width(),
                        tasks,
                    ))
                .max(rect.y0),
                rect.x1,
                chooser_bottom.max(rect.y0),
            );
            paint.text.draw_style(
                paint.scene,
                &receipt,
                ink,
                paint.transform * Affine::translate((row.x0, row.y0)),
                Some(row.width().max(1.0) as f32),
                &caption,
            );
            chooser_bottom = row.y0 - gap;
        }
        self.draw_agent_task_chooser(
            &mut paint,
            Rect::new(rect.x0, rect.y0, rect.x1, chooser_bottom.max(rect.y0)),
            tasks,
            hits,
        );
    }

    fn draw_agent_task_chooser(
        &self,
        paint: &mut PaintCtx<'_>,
        rect: Rect,
        tasks: &AgentTaskView,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let gap = f64::from(paint.theme.tokens.spacing.s2);
        let row_h = SegmentedFace::height(ControlSize::Small, paint.theme);
        let mut y = rect.y0;
        if tasks.can_start() && self.agent_tasks.needs_compute_controls() && y + row_h <= rect.y1 {
            let row = Rect::new(rect.x0, y, rect.x1, y + row_h);
            Button::paint(
                paint.scene,
                paint.transform,
                paint.text,
                paint.theme,
                &ButtonFace::new(row, &self.agent_tasks.setup_label())
                    .control_size(ControlSize::Small)
                    .hovered(self.hovered(row)),
            );
            ChatApp::push_hit(hits, row, Hit::AgentSetup);
            y = row.y1 + gap;
        }
        if tasks.current().is_some() && y + row_h <= rect.y1 {
            let row = Rect::new(rect.x0, y, rect.x1, y + row_h);
            Button::paint(
                paint.scene,
                paint.transform,
                paint.text,
                paint.theme,
                &ButtonFace::new(row, &crate::i18n::t("chat-task-add-subtask"))
                    .control_size(ControlSize::Small)
                    .hovered(self.hovered(row)),
            );
            ChatApp::push_hit(hits, row, Hit::AgentSubtask);
            y = row.y1 + gap;
        }
        y = self.draw_task_output_controls(paint, rect, tasks, y, hits);
        // Keep exact-run switching reachable when the sidebar is collapsed.
        if tasks.tasks.len() > 1 && self.task_filter == "all" {
            for task in tasks.visible_tasks() {
                if tasks
                    .selected_identity()
                    .is_some_and(|(id, epoch)| task.task_id == id && task.run_epoch == Some(epoch))
                {
                    continue;
                }
                if y + row_h > rect.y1 {
                    break;
                }
                let row = Rect::new(rect.x0, y, rect.x1, y + row_h);
                Button::paint(
                    paint.scene,
                    paint.transform,
                    paint.text,
                    paint.theme,
                    &ButtonFace::new(row, &task.title)
                        .control_size(ControlSize::Small)
                        .hovered(self.hovered(row)),
                );
                ChatApp::push_hit(hits, row, Hit::AgentTask(Box::new(task.clone())));
                y = row.y1 + gap;
            }
            if y + row_h <= rect.y1 {
                let remaining = tasks.remaining_after_page();
                let label = if remaining == 0 {
                    crate::i18n::t("chat-agent-task-first")
                } else {
                    crate::i18n::t_args(
                        "chat-agent-task-more",
                        &[("count", remaining.to_string().into())],
                    )
                };
                let row = Rect::new(rect.x0, y, rect.x1, y + row_h);
                Button::paint(
                    paint.scene,
                    paint.transform,
                    paint.text,
                    paint.theme,
                    &ButtonFace::new(row, &label)
                        .control_size(ControlSize::Small)
                        .hovered(self.hovered(row)),
                );
                ChatApp::push_hit(hits, row, Hit::AgentTaskMore);
                y = row.y1 + gap;
            }
        }
        y
    }

    fn draw_task_output_controls(
        &self,
        paint: &mut PaintCtx<'_>,
        rect: Rect,
        tasks: &AgentTaskView,
        mut y: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let row_h = SegmentedFace::height(ControlSize::Small, paint.theme);
        let gap = f64::from(paint.theme.tokens.spacing.s2);
        if !tasks.outputs.is_empty() && y + row_h <= rect.y1 {
            let label = crate::i18n::t_args(
                "chat-task-output-count",
                &[("count", tasks.outputs.len().to_string().into())],
            );
            paint.text.draw_style(
                paint.scene,
                &label,
                paint.theme.text_dim,
                paint.transform * Affine::translate((rect.x0, y)),
                Some(rect.width().max(1.0) as f32),
                &paint.theme.text(TextRole::Caption),
            );
            y += row_h + gap;
        }
        for output in &tasks.outputs {
            if y + row_h > rect.y1 {
                break;
            }
            let row = Rect::new(rect.x0, y, rect.x1, y + row_h);
            let label = crate::agent_tasks::output_label(output);
            Button::paint(
                paint.scene,
                paint.transform,
                paint.text,
                paint.theme,
                &ButtonFace::new(row, &label)
                    .control_size(ControlSize::Small)
                    .hovered(self.hovered(row)),
            );
            ChatApp::push_hit(hits, row, Hit::TaskOutput(Box::new(output.clone())));
            y = row.y1 + gap;
        }
        y
    }

    /// The same run control drives paint and its reserved caption width.
    fn agent_task_run_control(&self, tasks: &AgentTaskView) -> Option<(String, Hit)> {
        if !self.direct_agent_is_available() {
            return None;
        }
        if tasks.can_start() {
            Some((crate::i18n::t("chat-agent-start"), Hit::AgentStart))
        } else if tasks.can_stop() {
            Some((crate::i18n::t("chat-agent-stop"), Hit::AgentStop))
        } else {
            None
        }
    }

    /// Reserve the measured status and run control above the shared editor.
    fn agent_task_status_height(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
        width: f64,
        tasks: &AgentTaskView,
    ) -> f64 {
        let action_w = if let Some((label, _)) = self.agent_task_run_control(tasks) {
            ButtonFace::new(Rect::ZERO, &label)
                .control_size(ControlSize::Small)
                .intrinsic_width(tr, theme)
                + f64::from(theme.tokens.spacing.s2)
        } else {
            0.0
        };
        let (_, height) = tr.measure_style(
            &agent_task_status_lines(tasks).primary,
            Some((width - action_w).max(1.0) as f32),
            &theme.text(TextRole::Body),
        );
        f64::from(height).max(SegmentedFace::height(ControlSize::Small, theme))
    }

    fn agent_task_receipt_height(
        tr: &mut TextRenderer,
        theme: &Theme,
        width: f64,
        tasks: &AgentTaskView,
    ) -> f64 {
        let Some(receipt) = agent_task_status_lines(tasks).receipt else {
            return 0.0;
        };
        let (_, height) = tr.measure_style(
            &receipt,
            Some(width.max(1.0) as f32),
            &theme.text(TextRole::Caption),
        );
        f64::from(height)
    }

    pub(crate) fn agent_task_controls_height(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
        width: f64,
    ) -> f64 {
        let Some(tasks) = self.direct_agent_tasks() else {
            return 0.0;
        };
        let chooser_rows = tasks.outputs.len()
            + usize::from(tasks.can_start() && self.agent_tasks.needs_compute_controls())
            + usize::from(!tasks.outputs.is_empty())
            + usize::from(tasks.current().is_some())
            + if tasks.tasks.len() > 1 && self.task_filter == "all" {
                2
            } else {
                0
            };
        let receipt_rows = usize::from(agent_task_status_lines(tasks).receipt.is_some());
        let rows = chooser_rows + self.agent_intent_picker_rows(tr, theme, width);
        let row_h = SegmentedFace::height(ControlSize::Small, theme);
        let gap = f64::from(theme.tokens.spacing.s2);
        self.agent_task_status_height(tr, theme, width, tasks)
            + Self::agent_task_receipt_height(tr, theme, width, tasks)
            + rows as f64 * row_h
            + (rows + receipt_rows + 1) as f64 * gap
    }

    fn draw_agent_intent_picker(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        tasks: &AgentTaskView,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let available = self.direct_agent_is_available();
        let can_steer = available && tasks.can_steer();
        let segments = vec![
            Segment::new(crate::i18n::t("chat-agent-mode-queue")).with_enabled(available),
            Segment::new(crate::i18n::t(if can_steer {
                "chat-agent-mode-steer"
            } else {
                "chat-agent-mode-steer-unavailable"
            }))
            .with_enabled(can_steer),
        ];
        let selected = match self.direct_agent_intent() {
            AgentComposerIntent::Queue => 0,
            AgentComposerIntent::Steer => 1,
        };
        let stacked = self.agent_intent_picker_rows(tr, theme, rect.width()) > 1;
        let parts = if stacked {
            let gap = f64::from(theme.tokens.spacing.s2);
            let row_h = SegmentedFace::height(ControlSize::Small, theme);
            (0..segments.len())
                .map(|index| {
                    let y0 = rect.y0 + index as f64 * (row_h + gap);
                    Rect::new(rect.x0, y0, rect.x1, (y0 + row_h).min(rect.y1))
                })
                .collect::<Vec<_>>()
        } else {
            SegmentedFace::partition_segments(
                rect,
                &segments,
                SegmentedStyle::Segmented,
                ControlSize::Small,
                tr,
                theme,
            )
        };
        ui::widget::ToggleGroup::paint(
            scene,
            tf,
            tr,
            theme,
            &SegmentedFace::of_segments(&parts, &segments, selected)
                .control_size(ControlSize::Small),
        );
        for (index, part) in parts.into_iter().enumerate() {
            let intent = match index {
                0 if available => Some(AgentComposerIntent::Queue),
                1 if can_steer => Some(AgentComposerIntent::Steer),
                _ => None,
            };
            if let Some(intent) = intent {
                ChatApp::push_hit(hits, part, Hit::AgentIntent(intent));
            }
        }
    }

    pub(crate) fn agent_intent_picker_rows(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
        available_width: f64,
    ) -> usize {
        let labels = [
            crate::i18n::t("chat-agent-mode-queue"),
            crate::i18n::t(
                if self
                    .direct_agent_tasks()
                    .is_some_and(AgentTaskView::can_steer)
                {
                    "chat-agent-mode-steer"
                } else {
                    "chat-agent-mode-steer-unavailable"
                },
            ),
        ];
        let refs = labels.iter().map(String::as_str).collect::<Vec<_>>();
        if SegmentedFace::intrinsic_width(&refs, ControlSize::Small, tr, theme) <= available_width {
            1
        } else {
            labels.len()
        }
    }

    /// Who else is writing in this composer's channel or thread, under the
    /// composer: "Ada is typing…". What they write stays theirs.
    fn draw_draft_presence(
        composer: &crate::composer::DraftComposer,
        key: &ui::presence::PresenceKey,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(line) = typing_line(presence, key) else {
            return;
        };
        let field = composer.rect();
        let style = theme.text(TextRole::Caption);
        let shown = elide(tr, &line, &style, field.width());
        tr.draw_style(
            scene,
            &shown,
            theme.text_dim,
            tf.pre_translate(vello::kurbo::Vec2::new(
                field.x0,
                field.y1 + f64::from(theme.tokens.spacing.s1),
            )),
            None,
            &style,
        );
    }

    /// A viewer must never be handed an editable-looking field that silently
    /// drops a send. This keeps the permission boundary visible at the exact
    /// point where someone would otherwise try to act.
    fn draw_read_only_composer(
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        let mut ctx = PaintCtx::new(scene, tf, tr, theme);
        let pad = f64::from(theme.tokens.spacing.s4);
        line_in(
            &mut ctx,
            &crate::i18n::t("chat-conversation-read-only"),
            &theme.text(TextRole::Body),
            theme.text_dim,
            rect.x0 + pad,
            rect,
            rect.width() - 2.0 * pad,
        );
    }

    /// The composer field and its buttons — attach, emoji and send — with
    /// their hit regions. Returns the band the composer's editor is laid out
    /// into; the editor owns the text, caret, selection and placeholder.
    ///
    /// The band is sized from the measured `draft` (one line when empty) and
    /// centred on the field, so a short draft lines up with the buttons; a
    /// long draft fills the field, which itself grows with the text.
    pub(crate) fn draw_input_box(
        &self,
        paint: PaintCtx<'_>,
        rect: Rect,
        draft: &str,
        (send_enabled, attach, interactive): (bool, bool, bool),
        focus_hit: Hit,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> Rect {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let sp = &theme.tokens.spacing;
        let btn = ButtonFace::height(ControlSize::Regular, theme);
        paint_field(scene, tf, theme, rect);
        if interactive {
            ChatApp::push_hit(hits, rect, focus_hit);
        }

        if !interactive {
            return tilo_rich_text::ComposerFrame::layout(rect, theme, &[], &[]).editor;
        }

        let send_label = if self.task_goal.is_some() {
            crate::i18n::t("chat-task-save-goal")
        } else if self.direct_agent.is_none() {
            crate::i18n::t("chat-agent-send-steer")
        } else {
            match self.direct_agent_intent() {
                AgentComposerIntent::Queue => crate::i18n::t("chat-agent-send-queue"),
                AgentComposerIntent::Steer => crate::i18n::t("chat-agent-send-steer"),
            }
        };
        let send_probe = ButtonFace::icon_only(Rect::ZERO, Icon::Send, &send_label)
            .control_size(ControlSize::Regular);
        let send_w = send_probe.intrinsic_width(tr, theme);
        let leading = if attach { vec![btn] } else { Vec::new() };
        let emoji_w =
            ButtonFace::icon_only(Rect::ZERO, Icon::Smile, "Emoji").intrinsic_width(tr, theme);
        let frame =
            tilo_rich_text::ComposerFrame::layout(rect, theme, &leading, &[send_w, emoji_w]);
        if attach {
            let plus = composer_slot(&frame.leading, 0);
            Button::paint(
                scene,
                tf,
                tr,
                theme,
                &ButtonFace::icon_only(plus, Icon::Paperclip, "Attach a file")
                    .hovered(self.hovered(plus)),
            );
            ChatApp::push_hit(hits, plus, Hit::Attach);
        }

        // Enter sends. The send icon is a quiet one beside the emoji, for a
        // pointer or a touch, lit once there is something to send.
        let send = composer_slot(&frame.trailing, 0);
        let send_face = ButtonFace::icon_only(send, Icon::Send, &send_label)
            .control_size(ControlSize::Regular)
            .hovered(self.hovered(send));
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &if send_enabled {
                send_face
            } else {
                send_face.disabled(true)
            },
        );
        ChatApp::push_hit(hits, send, Hit::Send);

        let emoji = composer_slot(&frame.trailing, 1);
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &ButtonFace::icon_only(emoji, Icon::Smile, "Emoji").hovered(self.hovered(emoji)),
        );
        let picker_w = f64::from(theme.tokens.metrics.panel_width_utility);
        ChatApp::push_hit(
            hits,
            emoji,
            Hit::ComposerEmoji(Point::new(emoji.x1 - picker_w, rect.y0 - f64::from(sp.s2))),
        );

        let _ = draft;
        frame.editor
    }

    // ---------------------------------------------------------------- thread
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
    /// control (Back on a pushed page). Returns the header band.
    fn draw_panel_header(
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
        let page = self.thread_is_page();
        let back_label = if matches!(&close_hit, Hit::CloseThread) {
            crate::i18n::t("chat-thread-back")
        } else {
            "Back".into()
        };
        let close_label = if matches!(&close_hit, Hit::CloseThread) {
            crate::i18n::t("chat-thread-close")
        } else {
            "Close".into()
        };
        let mut title_x = head.x0 + f64::from(sp.s4);
        let close = if page {
            let back = square(
                Point::new(head.x0 + f64::from(sp.s3) + target / 2.0, head.center().y),
                target,
            );
            title_x = back.x1 + f64::from(sp.s2);
            Button::paint(
                scene,
                tf,
                tr,
                theme,
                &ButtonFace::icon_only(back, Icon::ChevronLeft, &back_label)
                    .hovered(self.hovered(back)),
            );
            back
        } else {
            let x = square(
                Point::new(head.x1 - f64::from(sp.s3) - target / 2.0, head.center().y),
                target,
            );
            Button::paint(
                scene,
                tf,
                tr,
                theme,
                &ButtonFace::icon_only(x, Icon::X, &close_label).hovered(self.hovered(x)),
            );
            x
        };
        ChatApp::push_hit(hits, close, close_hit);
        let right = if page {
            head.x1 - f64::from(sp.s4)
        } else {
            close.x0 - f64::from(sp.s2)
        };
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

    fn draw_thread_header(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> Rect {
        self.draw_panel_header(
            PaintCtx::new(scene, tf, tr, theme),
            panel,
            &self
                .messages
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
                ),
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
            height += ChipFace::height(theme) + f64::from(sp.s2);
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
        let (end_x, nh) = self.paint_author(
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
            let chip_h = ChipFace::height(theme);
            let mut chip_x = tx;
            for reaction in &m.reactions {
                let label = format!("{} {}", reaction.emoji, reaction.count());
                let chip_w = ChipFace::intrinsic_width(&label, ChipLead::None, false, tr, theme);
                ChipFace::new(Rect::new(chip_x, yy, chip_x + chip_w, yy + chip_h), &label)
                    .selected(reaction.reacted_by(&self.me.id))
                    .paint(scene, tf, tr, theme);
                chip_x += chip_w + f64::from(sp.s2);
            }
            yy += chip_h + f64::from(sp.s2);
        }
        yy + f64::from(sp.s3)
    }

    // ----------------------------------------------------------- people roster
    /// The workspace's people (and agents) with their presence and, when
    /// connected, the voice channel they are in.
    pub(crate) fn draw_members(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let panel = self.thread_rect();
        let head = self.draw_panel_header(
            PaintCtx::new(scene, tf, tr, theme),
            panel,
            self.roster_label(),
            Hit::ToggleRightFlap,
            hits,
        );
        let rows = self.roster_rows();
        let body = Rect::new(panel.x0, head.y1, panel.x1, panel.y1);
        push_clip(scene, tf, body);
        let inset = f64::from(theme.tokens.spacing.s2);
        let mut y = body.y0;
        let mut ctx = PaintCtx::new(scene, tf, tr, theme);
        // Who is around first, then everyone else, each under a count.
        let here = |person: &User, voice: &Option<String>| {
            voice.is_some() || person.status != Presence::Offline
        };
        let online = rows.iter().filter(|(p, v)| here(p, v)).count();
        let sections = [
            (format!("Online — {online}"), true),
            (format!("Offline — {}", rows.len() - online), false),
        ];
        for (index, (title, present)) in sections.iter().enumerate() {
            let members: Vec<_> = rows
                .iter()
                .filter(|(p, v)| here(p, v) == *present)
                .collect();
            if members.is_empty() {
                continue;
            }
            let section_h = nav_section_height(theme, index == 0 || online == 0);
            chrome::paint_nav_section(
                ctx.scene,
                tf,
                ctx.text,
                theme,
                &NavSectionFace::new(Rect::new(body.x0, y, body.x1, y + section_h), title),
            );
            y += section_h;
            for (person, voice) in members {
                if y > panel.y1 {
                    break;
                }
                // Named as the room names them, numbered when shared: the
                // same label as their lines in the timeline.
                let display = if person.is_agent && !self.is_me(&person.id) {
                    format!("{} (agent)", person.name)
                } else {
                    self.user_name(&person.id)
                };
                // A plain "Online" under every name says nothing the section
                // has not; where someone is, or that they are away, does.
                let status = match voice {
                    Some(ch) => format!("In {ch}"),
                    None if matches!(person.status, Presence::Idle | Presence::Dnd) => {
                        presence_label(person.status).to_owned()
                    }
                    None => String::new(),
                };
                let name = self.user_name(&person.id);
                let mut face = ListRowFace::new(&display)
                    .leading(RowLeading::Avatar(
                        AvatarFace::new(&name, &person.id)
                            .presence(self.identity_colour(&person.id))
                            .status(avatar_status(person.status)),
                    ))
                    .enabled(*present || !person.is_agent)
                    .selected(self.revealed_person.as_deref() == Some(person.id.as_str()));
                if !status.is_empty() {
                    face = face.secondary(&status);
                }
                let row_h = face.height(ctx.text, theme);
                let row = Rect::new(body.x0 + inset, y, body.x1 - inset, y + row_h);
                face.paint(&mut ctx, row);
                y += row_h;
            }
        }
        scene.pop_layer();
    }

    /// The open channel's pinned messages, newest first, in the detail
    /// drawer: each opens where it was written.
    pub(crate) fn draw_pins(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let panel = self.thread_rect();
        let head = self.draw_panel_header(
            PaintCtx::new(scene, tf, tr, theme),
            panel,
            "Pinned messages",
            Hit::TogglePins,
            hits,
        );
        let body = Rect::new(panel.x0, head.y1, panel.x1, panel.y1);
        let pinned: Vec<&Message> = self.messages.iter().rev().filter(|m| m.pinned).collect();
        let mut ctx = PaintCtx::new(scene, tf, tr, theme);
        if pinned.is_empty() {
            let content = EmptyStateContent {
                icon: Some(Icon::Pin),
                message: Some("Pin a message from its menu to keep it here for everyone."),
                ..EmptyStateContent::new("Nothing pinned yet")
            };
            EmptyState::paint(&mut ctx, body, &content, self.last_mouse);
            return;
        }
        push_clip(ctx.scene, tf, body);
        let inset = f64::from(theme.tokens.spacing.s2);
        let mut y = body.y0 + inset;
        for m in pinned {
            if y > body.y1 {
                break;
            }
            let name = self.user_name(&m.author);
            let when = format!("{} · {}", day_label(m.ts), clock_label(m.ts));
            let face = ListRowFace::new(&name)
                .meta(&when)
                .secondary(&m.text)
                .two_line_secondary()
                .leading(RowLeading::Avatar(
                    AvatarFace::new(&name, &m.author).presence(self.identity_colour(&m.author)),
                ));
            let row_h = face.height(ctx.text, theme);
            let row = Rect::new(body.x0 + inset, y, body.x1 - inset, y + row_h);
            let hovered = self.hovered(row);
            face.interaction(ui::widget::Interaction {
                hovered,
                ..ui::widget::Interaction::default()
            })
            .paint(&mut ctx, row);
            ChatApp::push_hit(hits, row, Hit::RevealMessage(m.id.clone()));
            y += row_h;
        }
        ctx.scene.pop_layer();
    }

    // ------------------------------------------------------- attach picker
    /// The workspace file picker, anchored above the composer's attach
    /// button. Lists the shareable files found in the shared workspace, so a
    /// share is always an actual file the person chose.
    #[allow(
        clippy::too_many_lines,
        reason = "one measure-then-paint pass keeps the panel height and its rows in step"
    )]
    pub(crate) fn draw_attach_picker(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        // Full-window dismiss target (lowest priority).
        ChatApp::push_hit(hits, self.rect, Hit::Dismiss);
        let sp = &theme.tokens.spacing;
        let local = theme.on_ground(chrome::panel_ground(theme));
        let pad = f64::from(sp.s2);
        let title_h = nav_row_height(theme);
        let caption_h = line_of(tr, &theme.text(TextRole::Caption));
        let status = self.attachment_status.as_deref().filter(|status| {
            !(self.attachment_picker_pending.is_some() && *status == "Loading attachments…")
        });
        let max_rows = self.attach_picker_max_rows();
        let rows: Vec<_> = self.attach_entries.iter().take(max_rows).collect();
        let row_h = ListRowFace::new("Ag")
            .secondary("Ag")
            .leading(RowLeading::Icon(Icon::Image))
            .height(tr, &local);
        // This device's files first; the workspace's, where there is one,
        // under their own caption.
        let body_h = row_h
            + if rows.is_empty() {
                0.0
            } else {
                caption_h + pad + rows.len() as f64 * row_h
            };
        let more = self.attach_entries.len().saturating_sub(max_rows);
        let panel_h = pad
            + title_h
            + if status.is_some() {
                caption_h + pad
            } else {
                0.0
            }
            + body_h
            + if more > 0 { caption_h + pad } else { 0.0 }
            + pad;
        let comp = self.composer_rect(tr, theme);
        let w = f64::from(theme.tokens.metrics.panel_width_list).min(self.rect.width() - 2.0 * pad);
        let x0 = comp.x0.min(self.rect.x1 - w - pad).max(self.rect.x0 + pad);
        let y1 = comp.y0 - pad;
        let y0 = (y1 - panel_h).max(self.rect.y0 + pad);
        let panel = Rect::new(x0, y0, x0 + w, y1);
        let radius = f64::from(theme.tokens.radii.panel);
        ui::surface::shadow(scene, tf, panel, radius, &theme.tokens.elevation.floating);
        ui::surface::panel(scene, tf, panel, radius, theme);
        push_clip(scene, tf, panel);

        let mut ctx = PaintCtx::new(scene, tf, tr, &local);
        let inner_x = panel.x0 + f64::from(sp.s4);
        let mut y = panel.y0 + pad;
        let title_band = Rect::new(panel.x0, y, panel.x1, y + title_h);
        line_in(
            &mut ctx,
            "Share a file",
            &local.text(TextRole::Subheading),
            local.text_primary,
            inner_x,
            title_band,
            w - 2.0 * f64::from(sp.s4),
        );
        y = title_band.y1;
        if let Some(status) = status {
            let failed = status.starts_with("Not shared")
                || status.starts_with("Couldn't")
                || status.contains("cancelled");
            let ink = if failed {
                local.tokens.colors.semantic.danger
            } else {
                local.text_dim
            };
            let band = Rect::new(panel.x0, y, panel.x1, y + caption_h);
            line_in(
                &mut ctx,
                status,
                &local.text(TextRole::Caption),
                ink,
                inner_x,
                band,
                w - 2.0 * f64::from(sp.s4),
            );
            y = band.y1 + pad;
        }
        let device = Rect::new(panel.x0 + pad, y, panel.x1 - pad, y + row_h);
        ListRowFace::new("Upload from this device")
            .secondary("Files, pictures and videos up to 32 MiB")
            .leading(RowLeading::Icon(Icon::Upload))
            .interaction(ui::widget::Interaction {
                hovered: self.hovered(device),
                ..Default::default()
            })
            .paint(&mut ctx, device);
        ChatApp::push_hit(hits, device, Hit::AttachDevice);
        y = device.y1;
        if !rows.is_empty() {
            let band = Rect::new(panel.x0, y + pad, panel.x1, y + pad + caption_h);
            line_in(
                &mut ctx,
                "From the workspace",
                &local.text(TextRole::Caption),
                local.text_dim,
                inner_x,
                band,
                w - 2.0 * f64::from(sp.s4),
            );
            y = band.y1;
        }
        for e in rows {
            let row = Rect::new(panel.x0 + pad, y, panel.x1 - pad, y + row_h);
            let icon = if e.is_video {
                Icon::Film
            } else if crate::picker::is_image_name(&e.name) {
                Icon::Image
            } else {
                Icon::FileText
            };
            let size = format_bytes(e.size);
            ListRowFace::new(&e.name)
                .secondary(&size)
                .leading(RowLeading::Icon(icon))
                .interaction(ui::widget::Interaction {
                    hovered: self.hovered(row),
                    ..Default::default()
                })
                .paint(&mut ctx, row);
            if self.attachment_pending.is_none() {
                ChatApp::push_hit(hits, row, Hit::AttachPick(e.key()));
            }
            y += row_h;
        }
        if more > 0 {
            let band = Rect::new(panel.x0, y + pad, panel.x1, y + pad + caption_h);
            let label = format!("{more} more in the workspace");
            line_in(
                &mut ctx,
                &label,
                &local.text(TextRole::Caption),
                local.text_dim,
                inner_x,
                band,
                w,
            );
        }
        scene.pop_layer();
    }

    /// At increased text sizes the picker becomes a short, readable sheet
    /// instead of shrinking its file names to preserve six rows.
    fn attach_picker_max_rows(&self) -> usize {
        if self.layout_text_scale > 1.5 {
            2
        } else if self.layout_text_scale > 1.15 {
            4
        } else {
            6
        }
    }
}

/// A composer's face: the raised well with its derived edge. It wears no
/// focus ring: the caret in it says where typing goes, as in any chat.
fn paint_field(scene: &mut Scene, tf: Affine, theme: &Theme, rect: Rect) {
    let radius = f64::from(theme.tokens.radii.control);
    ui::surface::card(scene, tf, rect, radius, theme.tokens.colors.surfaces.raised);
}

/// Who else is writing in the composer scoped by `key`, as a line: "Ada is
/// typing…"; `None` when nobody is.
pub(crate) fn typing_line(
    presence: &PresenceCtx,
    key: &ui::presence::PresenceKey,
) -> Option<String> {
    let mut writers: Vec<String> = presence
        .carets
        .get(key)
        .into_iter()
        .flatten()
        .map(|(user, _)| user)
        .chain(
            presence
                .selections
                .get(key)
                .into_iter()
                .flatten()
                .map(|(user, _)| user),
        )
        .filter(|user| !presence.is_own_identity(&user.0))
        .map(|user| presence.display_name(user))
        .collect();
    writers.sort();
    writers.dedup();
    match writers.as_slice() {
        [] => None,
        [one] => Some(format!("{one} is typing…")),
        [one, two] => Some(format!("{one} and {two} are typing…")),
        _ => Some("Several people are typing…".to_owned()),
    }
}

/// The local time of `ts` as `HH:MM`.
fn clock_label(ts: i64) -> String {
    model::clock(ts + local_offset())
}

/// "Today", "Yesterday" or "N days ago" for `ts`, against the local date.
fn day_label(ts: i64) -> String {
    let offset = local_offset();
    model::day_label(ts + offset, ui::clock::local_now().timestamp() + offset)
}

/// Seconds the local time zone is ahead of UTC now.
fn local_offset() -> i64 {
    i64::from(ui::clock::local_now().offset().local_minus_utc())
}

/// The icon that says what kind of file an attachment is.
fn attachment_icon(att: &Attachment) -> Icon {
    match att {
        Attachment::Image { .. } => Icon::Image,
        Attachment::Video { .. } => Icon::Film,
        Attachment::File { .. } => Icon::FileText,
    }
}

/// The facts a media card lists under its name: an image's pixel size, a
/// video's file size.
fn media_meta(att: &Attachment) -> Vec<String> {
    match att {
        Attachment::Image { w, h, .. } if *w > 0 && *h > 0 => vec![format!("{w} × {h}")],
        Attachment::Video { size, .. } => vec![format_bytes(*size)],
        _ => Vec::new(),
    }
}

/// The media card for an image or video attachment in a column `max_w`
/// wide, and the width it is laid out at: the picture's own shape, never
/// taller than a regular media card.
fn media_face<'a>(
    att: &'a Attachment,
    art: MediaArt<'a>,
    meta: &'a [String],
    max_w: f64,
    theme: &Theme,
) -> (f64, MediaCardFace<'a>) {
    let (w, h) = match att {
        Attachment::Image { w, h, .. } | Attachment::Video { w, h, .. } => (*w, *h),
        Attachment::File { .. } => (0, 0),
    };
    let aspect = if w > 0 && h > 0 {
        MediaAspect::Ratio(f64::from(w) / f64::from(h))
    } else {
        MediaAspect::Wide
    };
    let max_h = MediaSize::Regular.extent(theme);
    let width = (max_h * aspect.ratio()).min(max_w).min(2.0 * max_h);
    let mut face = MediaCardFace::new(art).aspect(aspect).title(att.name());
    for line in meta {
        face = face.meta(line);
    }
    (width, face)
}

/// Width of a file attachment's card in a column `max_w` wide.
fn file_card_width(max_w: f64, theme: &Theme) -> f64 {
    f64::from(theme.tokens.metrics.panel_width_list).min(max_w)
}

/// Height of a file attachment's card: an icon beside the name over one
/// line of detail.
fn file_card_height(tr: &mut TextRenderer, theme: &Theme) -> f64 {
    let body =
        line_of(tr, &theme.text(TextRole::Label)) + line_of(tr, &theme.text(TextRole::Caption));
    CardFace::new().height_for_body(
        tr,
        theme,
        f64::from(theme.tokens.metrics.panel_width_list),
        body,
    )
}

/// A file attachment (or one this peer may not open): a card with the
/// file's kind, its name and one line of detail.
fn paint_file_card(
    ctx: &mut PaintCtx<'_>,
    origin: Point,
    max_w: f64,
    icon: Icon,
    name: &str,
    detail: &str,
) {
    let theme = ctx.theme;
    let w = file_card_width(max_w, theme);
    let h = file_card_height(ctx.text, theme);
    let rect = Rect::new(origin.x, origin.y, origin.x + w, origin.y + h);
    let face = CardFace::new();
    face.paint(ctx, rect);
    let body = face.body_rect(ctx.text, theme, rect);
    let icon_side = f64::from(theme.tokens.metrics.icon_md);
    icon.draw(
        ctx.scene,
        ctx.transform,
        Rect::new(
            body.x0,
            body.center().y - icon_side / 2.0,
            body.x0 + icon_side,
            body.center().y + icon_side / 2.0,
        ),
        theme.text_dim,
    );
    let tx = body.x0 + icon_side + f64::from(theme.tokens.spacing.s3);
    let tw = (body.x1 - tx).max(0.0);
    let name_style = theme.text(TextRole::Label).with_kind(TextKind::Identifier);
    let detail_style = theme.text(TextRole::Caption);
    let name_h = line_of(ctx.text, &name_style);
    let name_shown = elide(ctx.text, name, &name_style, tw);
    let detail_shown = elide(ctx.text, detail, &detail_style, tw);
    ctx.text(
        &name_shown,
        &name_style,
        theme.text_primary,
        Point::new(tx, body.y0),
        None,
    );
    ctx.text(
        &detail_shown,
        &detail_style,
        theme.text_dim,
        Point::new(tx, body.y0 + name_h),
        None,
    );
}

/// Line box of a message-group header (author name + time) plus the gap to
/// the body beneath it, measured in the name's own style.
fn header_row_h(tr: &mut TextRenderer, theme: &Theme) -> f64 {
    line_of(tr, &theme.text(TextRole::Body).strong()) + f64::from(theme.tokens.spacing.s1)
}

/// Height of the thread link: a navigation row.
fn thread_bar_h(theme: &Theme) -> f64 {
    nav_row_height(theme)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentWorkLineStyle {
    Summary,
    Rationale,
    Step,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AgentWorkLine {
    text: String,
    style: AgentWorkLineStyle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AgentWorkPresentation {
    summary: String,
    details: Vec<AgentWorkLine>,
}

/// The same recorded evidence read aloud in the conversation tree.
pub(crate) fn agent_work_accessible_description(work: &AgentWork) -> String {
    let presentation = AgentWorkPresentation::from(work);
    std::iter::once(presentation.summary)
        .chain(presentation.details.into_iter().map(|line| line.text))
        .collect::<Vec<_>>()
        .join("; ")
}

impl From<&AgentWork> for AgentWorkPresentation {
    fn from(work: &AgentWork) -> Self {
        use crate::i18n::{t, t_args};
        let state = work
            .stream
            .as_ref()
            .and_then(|stream| stream.finish)
            .map_or_else(
                || work.state.clone(),
                |outcome| AgentWorkState::ProviderFinished { outcome },
            );
        let summary = match &state {
            AgentWorkState::ProviderFinished { outcome } => {
                use crate::agent_stream::AgentFinish;
                t(match outcome {
                    AgentFinish::Complete => "chat-agent-complete",
                    AgentFinish::Truncated => "chat-agent-truncated",
                    AgentFinish::Refused => "chat-agent-refused",
                    AgentFinish::Cancelled => "chat-agent-cancelled",
                    AgentFinish::Failed => "chat-agent-failed",
                })
            }
            AgentWorkState::Taken => t("chat-agent-work-taken"),
            AgentWorkState::Waiting { app } => {
                t_args("chat-agent-work-waiting", &[("app", app.into())])
            }
            AgentWorkState::Doing { did, about } if about.trim().is_empty() => {
                t_args("chat-agent-work-intent", &[("action", did.into())])
            }
            AgentWorkState::Doing { did, about } => t_args(
                "chat-agent-work-intent-about",
                &[("action", did.into()), ("about", about.into())],
            ),
            AgentWorkState::Done { did } => {
                t_args("chat-agent-work-done", &[("action", did.into())])
            }
            AgentWorkState::Refused => t("chat-agent-work-refused"),
        };
        let mut details = Vec::new();
        if let Some(reasoning) = &work.reasoning {
            let route = match reasoning.route {
                AgentRoute::Model => t("chat-agent-work-model-rationale"),
                AgentRoute::Floor => t("chat-agent-work-rule-match"),
            };
            let text = if reasoning.why.trim().is_empty() {
                match reasoning.route {
                    AgentRoute::Model => t("chat-agent-work-model-choice"),
                    AgentRoute::Floor => t("chat-agent-work-rule-choice"),
                }
            } else {
                t_args(
                    "chat-agent-work-rationale",
                    &[("route", route.into()), ("why", (&reasoning.why).into())],
                )
            };
            details.push(AgentWorkLine {
                text,
                style: AgentWorkLineStyle::Rationale,
            });
        }
        for step in &work.trace {
            // The dispatch record retains the exact resource address for the
            // action reader. The conversation uses the app's readable label.
            let target = step.label.clone();
            let text = match &step.outcome {
                AgentStepOutcome::Asked => t_args(
                    "chat-agent-work-step-requested",
                    &[("target", target.into())],
                ),
                AgentStepOutcome::Applied => {
                    t_args("chat-agent-work-step-applied", &[("target", target.into())])
                }
                AgentStepOutcome::NoChange => t_args(
                    "chat-agent-work-step-no-change",
                    &[("target", target.into())],
                ),
                AgentStepOutcome::Refused { why } if why.trim().is_empty() => {
                    t_args("chat-agent-work-step-refused", &[("target", target.into())])
                }
                AgentStepOutcome::Refused { why } => t_args(
                    "chat-agent-work-step-refused-why",
                    &[("target", target.into()), ("why", why.into())],
                ),
            };
            details.push(AgentWorkLine {
                text,
                style: AgentWorkLineStyle::Step,
            });
        }
        Self { summary, details }
    }
}

fn agent_work_text_style(style: AgentWorkLineStyle, theme: &Theme) -> ui::TextStyle {
    match style {
        AgentWorkLineStyle::Summary => theme.text(TextRole::Body).strong(),
        AgentWorkLineStyle::Rationale | AgentWorkLineStyle::Step => theme.text(TextRole::Caption),
    }
}

fn measure_agent_work(tr: &mut TextRenderer, theme: &Theme, work: &AgentWork, width: f32) -> f64 {
    let presentation = AgentWorkPresentation::from(work);
    let inset = f64::from(theme.tokens.spacing.s3);
    let gap = f64::from(theme.tokens.spacing.s2);
    let text_width = (width - ui::num::f32_of_f64(2.0 * inset)).max(0.0);
    let mut height = 2.0 * inset;
    height += f64::from(
        tr.measure_style(
            &presentation.summary,
            Some(text_width),
            &agent_work_text_style(AgentWorkLineStyle::Summary, theme),
        )
        .1,
    );
    for line in presentation.details {
        height += gap;
        height += f64::from(
            tr.measure_style(
                &line.text,
                Some(text_width),
                &agent_work_text_style(line.style, theme),
            )
            .1,
        );
    }
    height + gap
}

fn draw_agent_work_line(
    paint: &mut PaintCtx<'_>,
    value: &str,
    style: AgentWorkLineStyle,
    at: Point,
    width: f32,
) -> f64 {
    let text_style = agent_work_text_style(style, paint.theme);
    let color = match style {
        AgentWorkLineStyle::Summary => paint.theme.tokens.colors.text_primary,
        AgentWorkLineStyle::Rationale | AgentWorkLineStyle::Step => {
            paint.theme.tokens.colors.text_dim
        }
    };
    f64::from(text(paint, value, &text_style, color, at, Some(width)).1)
}

/// Per-row measured heights for the timeline layout.
struct RowM {
    idx_id: String,
    show_header: bool,
    /// Space above the row: a group's first row stands apart from the one
    /// before it; a continuation row follows closely.
    top_gap: f64,
    /// Space under the row's last part.
    pad: f64,
    /// Measured header row (author + time); zero on continuation rows.
    header_h: f64,
    body_h: f64,
    work_h: f64,
    reasoning_h: f64,
    resource_h: f64,
    att_h: f64,
    react_h: f64,
    edited_h: f64,
    thread_h: f64,
    /// This row is the message being edited inline: its body is the editor
    /// field (padded) plus a controls row.
    editing: bool,
    edit_ctrl_h: f64,
}

impl RowM {
    fn height(&self) -> f64 {
        let header = if self.show_header { self.header_h } else { 0.0 };
        let body = if self.editing {
            self.body_h + 2.0 * self.pad + self.pad + self.edit_ctrl_h
        } else {
            self.body_h
        };
        self.top_gap
            + header
            + body
            + self.work_h
            + self.reasoning_h
            + self.resource_h
            + self.edited_h
            + self.att_h
            + self.react_h
            + self.thread_h
            + self.pad
    }
}

/// Match the shared square control slots actually painted beside the editor.
fn composer_editor_width(field: f64, theme: &Theme, attach: bool) -> f64 {
    let button = ButtonFace::height(ControlSize::Regular, theme);
    let leading = if attach { vec![button] } else { Vec::new() };
    tilo_rich_text::ComposerFrame::layout(
        Rect::new(0.0, 0.0, field, button),
        theme,
        &leading,
        &[button, button],
    )
    .editor
    .width()
    .max(1.0)
}

#[expect(
    clippy::expect_used,
    reason = "ComposerFrame returns one slot for each supplied control width; these callers supply the matching fixed slots"
)]
fn composer_slot(slots: &[Rect], index: usize) -> Rect {
    *slots
        .get(index)
        .expect("supplied composer control has a slot")
}

#[cfg(test)]
mod tests {
    use super::{AgentWorkLineStyle, AgentWorkPresentation, RowM, media_face, media_meta};
    use crate::model::{
        AgentReasoning, AgentRoute, AgentStep, AgentStepOutcome, AgentWork, AgentWorkState,
        Attachment,
    };
    use ui::theme::Theme;
    use ui::widget::{MediaArt, MediaSize};

    #[test]
    fn saved_goal_footer_does_not_invent_a_zero_run_or_model_readiness() {
        let mut app = crate::app::ChatApp::new_ephemeral();
        app.agent_tasks.stage_queued_capture("workspace", "scribe");
        let view = app.agent_tasks.view("scribe").unwrap();
        let status = super::agent_task_status_lines(view);
        assert_eq!(status.primary, crate::i18n::t("chat-agent-task-queued"));
        assert!(!status.primary.contains('0'));
        assert!(!status.primary.contains("Ready"));
        assert!(view.can_start());
        assert!(!view.can_stop());
    }

    #[test]
    fn task_footer_reports_the_actual_run_without_repeating_its_header_title() {
        let mut app = crate::app::ChatApp::new_ephemeral();
        let view = app.agent_tasks.stage_capture("workspace", "scribe");
        let task = view.current().unwrap();
        let status = super::agent_task_status_lines(view);
        assert!(status.primary.contains("Working"));
        assert!(!status.primary.contains("Run 7"));
        assert_eq!(task.run_epoch, Some(7));
        assert!(!status.primary.contains(&task.title));
        assert_eq!(
            status.receipt,
            Some(crate::i18n::t("chat-agent-direction-waiting"))
        );
    }

    #[test]
    fn completed_task_never_claims_pending_direction_or_running_execution() {
        let mut app = crate::app::ChatApp::new_ephemeral();
        app.agent_tasks.stage_answer_capture("workspace", "scribe");
        let view = app.agent_tasks.view("scribe").unwrap();
        let status = super::agent_task_status_lines(view);
        assert_eq!(status.primary, crate::i18n::t("chat-agent-task-complete"));
        assert_eq!(
            status.receipt,
            Some(crate::i18n::t("chat-agent-direction-unapplied-ended"))
        );
        assert!(matches!(
            view.receipt,
            crate::agent_tasks::AgentTaskReceipt::Saved { .. }
        ));
        assert!(!view.can_steer());
        assert!(!view.can_stop());
        assert_eq!(view.selected_identity(), Some(("task:review", 7)));
    }

    #[test]
    fn saving_direction_is_quiet_but_start_errors_are_retained() {
        let mut app = crate::app::ChatApp::new_ephemeral();
        let view = app.agent_tasks.stage_capture("workspace", "scribe");
        view.receipt = crate::agent_tasks::AgentTaskReceipt::Saving;
        assert_eq!(super::agent_task_status_lines(view).receipt, None);
        view.start_error = Some("Approval is no longer current".into());
        assert_eq!(
            super::agent_task_status_lines(view).receipt,
            view.start_error
        );
    }

    #[test]
    fn explicit_start_receipt_reports_approval_not_execution() {
        let mut view = crate::agent_tasks::AgentTaskView::default();
        view.receipt = crate::agent_tasks::AgentTaskReceipt::Started;
        assert_eq!(
            super::agent_task_status_lines(&view).receipt.as_deref(),
            Some("Start approved")
        );
    }

    #[test]
    fn an_empty_frontier_has_no_start_or_stop_control() {
        let app = crate::app::ChatApp::new_ephemeral();
        assert!(
            app.agent_task_run_control(&crate::agent_tasks::AgentTaskView::default())
                .is_none()
        );
    }

    #[test]
    fn media_cards_keep_the_picture_shape_inside_a_regular_card() {
        let theme = Theme::dark();
        let max_h = MediaSize::Regular.extent(&theme);
        let wide = Attachment::Image {
            name: "wide.png".into(),
            blob: "sha256:image".into(),
            w: 800,
            h: 400,
        };
        let meta = media_meta(&wide);
        assert_eq!(meta, vec!["800 × 400".to_owned()]);
        let (w, _) = media_face(&wide, MediaArt::Placeholder, &meta, 1000.0, &theme);
        assert!(
            (w - 2.0 * max_h).abs() < 1e-6,
            "a wide picture is a regular card high"
        );
        let (narrow, _) = media_face(&wide, MediaArt::Placeholder, &meta, 120.0, &theme);
        assert!((narrow - 120.0).abs() < 1e-6, "never wider than its column");

        let unknown_video = Attachment::Video {
            name: "clip.mp4".into(),
            blob: "sha256:video".into(),
            w: 0,
            h: 0,
            size: 1024,
        };
        assert_eq!(media_meta(&unknown_video), vec!["1.0 KiB".to_owned()]);
        let (vw, _) = media_face(&unknown_video, MediaArt::Placeholder, &[], 1000.0, &theme);
        assert!(
            (vw - max_h * 16.0 / 9.0).abs() < 1e-6,
            "an unknown video is 16:9"
        );
    }

    #[test]
    fn a_file_card_names_the_file_and_never_a_path() {
        let file = Attachment::File {
            name: "moq-over-iroh.md".into(),
            blob: "sha256:file".into(),
            size: 18_944,
        };
        assert!(media_meta(&file).is_empty());
        assert_eq!(ui::format::bytes(18_944), "18.5 KiB");
    }

    #[test]
    fn agent_work_projection_names_only_recorded_state_rationale_and_dispatches() {
        let cases = [
            (AgentWorkState::Taken, "Request accepted"),
            (
                AgentWorkState::Waiting {
                    app: "Calculator".into(),
                },
                "Last reported: waiting for app state: Calculator",
            ),
            (
                AgentWorkState::Doing {
                    did: "Add function".into(),
                    about: "the revenue curve".into(),
                },
                "Latest intent: Add function: the revenue curve",
            ),
            (
                AgentWorkState::Done {
                    did: "Add function".into(),
                },
                "Completed: Add function",
            ),
            (AgentWorkState::Refused, "Could not complete this request"),
        ];
        for (state, expected) in cases {
            assert_eq!(
                AgentWorkPresentation::from(&AgentWork::new(state, 42)).summary,
                expected
            );
        }

        let work = AgentWork::new(
            AgentWorkState::Doing {
                did: "Add function".into(),
                about: String::new(),
            },
            42,
        )
        .because(AgentReasoning {
            route: AgentRoute::Floor,
            why: "The request matched the app's exported action.".into(),
        })
        .with_trace(vec![
            AgentStep::authored(
                "calculator.workspace",
                "Add function",
                AgentStepOutcome::Asked,
                0,
            ),
            AgentStep::authored(
                "calculator.workspace",
                "Add function",
                AgentStepOutcome::NoChange,
                1,
            ),
            AgentStep::authored(
                "calculator.graph",
                "Set bounds",
                AgentStepOutcome::Refused {
                    why: "Outside the granted range".into(),
                },
                2,
            ),
        ]);
        let presentation = AgentWorkPresentation::from(&work);
        assert_eq!(presentation.summary, "Latest intent: Add function");
        assert_eq!(
            presentation.details[0].text,
            "Rule match: The request matched the app's exported action."
        );
        assert_eq!(presentation.details[0].style, AgentWorkLineStyle::Rationale);
        assert_eq!(presentation.details[1].text, "Requested: Add function");
        assert_eq!(presentation.details[2].text, "No change: Add function");
        assert_eq!(
            presentation.details[3].text,
            "Refused: Set bounds: Outside the granted range"
        );
        assert!(
            presentation
                .details
                .iter()
                .all(|line| !line.text.to_ascii_lowercase().contains("chain of thought"))
        );
    }

    #[test]
    fn agent_work_projection_keeps_every_recorded_step_and_row_reserves_the_card() {
        let steps = (0..7)
            .map(|index| {
                AgentStep::authored(
                    format!("resource-{index}"),
                    format!("Action {index}"),
                    AgentStepOutcome::Applied,
                    index,
                )
            })
            .collect();
        let work = AgentWork::new(
            AgentWorkState::Done {
                did: "Finish".into(),
            },
            42,
        )
        .with_trace(steps);
        let presentation = AgentWorkPresentation::from(&work);
        assert_eq!(presentation.details.len(), 7);
        for (index, line) in presentation.details.iter().enumerate() {
            assert_eq!(line.text, format!("Applied: Action {index}"));
            assert_eq!(line.style, AgentWorkLineStyle::Step);
        }

        let row = RowM {
            idx_id: "reply".into(),
            show_header: false,
            top_gap: 3.0,
            pad: 4.0,
            header_h: 0.0,
            body_h: 0.0,
            work_h: 96.0,
            reasoning_h: 0.0,
            resource_h: 0.0,
            att_h: 0.0,
            react_h: 0.0,
            edited_h: 0.0,
            thread_h: 0.0,
            editing: false,
            edit_ctrl_h: 0.0,
        };
        assert_eq!(
            row.height(),
            103.0,
            "an empty agent reply reserves its work card"
        );
    }
}
