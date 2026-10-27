//! Rendering for the persistent chat surface: the navigation drawer (the
//! room's channel list), the conversation header, the grouped
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
use ui::author::Author;
use ui::chrome::nav_row_height;
use ui::format::size as format_bytes;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::conversation::ConversationClock;
use ui::widget::{
    ButtonFace, CardFace, ChipFace, ControlSize, MediaArt, MediaAspect, MediaCardFace, MediaSize,
};

use crate::app::{ChatApp, Hit, MenuTarget};
use crate::model::{Attachment, Message, Presence};
use crate::paint::{band_at_least, elide, line_of, push_clip};

/// How much of the participant's colour a mention *of you* keeps when it is
/// tinted toward the surface. A filled band, so it can be found by scanning.
const MENTION_SELF_BAND_MIX: f32 = 0.78;

/// Contrast floors for the mention run, measured rather than eyeballed:
/// AA body text wherever it lands, and the non-text floor for the filled band
/// against the surface it sits on (a band nobody can see is not a highlight).
use ui::tokens::colors::NON_TEXT_MIN_CONTRAST as MENTION_BAND_CONTRAST;
use ui::tokens::contrast::BODY_TEXT_MIN_CONTRAST as MENTION_TEXT_CONTRAST;

#[derive(Clone, Copy)]
pub(crate) struct MessageColumn {
    pub(crate) area: Rect,
    pub(crate) text_x: f64,
    pub(crate) text_width: f32,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HeaderIdentityText {
    pub(crate) name: String,
    pub(crate) kind: Option<String>,
    pub(crate) context: String,
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
        parts: &mut ui::widget::paint_damage::PaintParts,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let field_w = self.center_right() - self.center_left() - 2.0 * self.chrome.gutter;
        let width = composer_editor_width(field_w, theme, self.direct_person.is_none());
        self.composer_main.measure(width, f32::INFINITY, theme, tr);
        let area = self.messages_rect();
        // The timeline paints inside its clip, its grip and fade in the
        // column above the composer; the composer and the notes under it
        // below that, to the window's foot.
        let column = Rect::new(self.center_left(), area.y0, self.center_right(), area.y1);
        let messages_phase = ui::tilo_resource::profiling::Phase::start("chat.draw.messages");
        parts.draw(scene, "chat-messages", column, |scene| {
            if area.width() > 0.0 && area.height() >= self.chrome.body_line {
                push_clip(scene, tf, area);
                if self.direct_person.is_some() {
                    self.draw_person_messages(
                        PaintCtx::new(scene, tf, tr, theme),
                        presence,
                        area,
                        hits,
                    );
                } else {
                    self.draw_messages(PaintCtx::new(scene, tf, tr, theme), presence, area, hits);
                }
                scene.pop_layer();
            }
            self.draw_conversation_grip(scene, tf, theme, area, hits);
            // Older messages fade at the top edge when more history is above.
            if area.height() > 0.0 && self.scroll_content - self.scroll_viewport - self.scroll > 0.5
            {
                ui::surface::content_edge_fade(
                    scene,
                    tf,
                    column,
                    ui::surface::ContentEdge::Top,
                    theme.tokens.colors.surfaces.content,
                );
            }
        });
        messages_phase.end();
        let _composer_phase = ui::tilo_resource::profiling::Phase::start("chat.draw.composer");
        let composer = Rect::new(column.x0, column.y1, column.x1, self.rect.y1);
        parts.draw(scene, "chat-composer", composer, |scene| {
            self.draw_composer(scene, tf, tr, theme, presence, hits);
        });
        // The attachment the person opened sits over everything, the window's
        // own bar included: it is the whole window's layer, not the column's.
        if self.attachment_view.is_some() {
            parts.draw(scene, "chat-attachment", self.rect, |scene| {
                self.draw_attachment_viewer(scene, tf, tr, theme, hits);
            });
        }
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

    /// Each line of a message's body as [`Self::draw_body`] last drew it:
    /// the line's rect and the words on it. Read off the laid-out view, so
    /// it shapes nothing again.
    fn body_lines(&self, id: &str) -> Vec<(Rect, String)> {
        let mut views = self.body_views.borrow_mut();
        let Some((_, view)) = views.get_mut(&(0, id.to_owned())) else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        for block in view.editor_mut().blocks() {
            let words = block.line.value();
            if words.trim().is_empty() {
                continue;
            }
            let input = block.line.inner();
            for rect in block.line.prepared_range_rects(0..words.len()) {
                let middle = rect.center().y;
                // A row's two ends; a right-to-left row has its start on
                // the right.
                let a = input.byte_at_point(Point::new(rect.x0 + 0.5, middle));
                let b = input.byte_at_point(Point::new(rect.x1 - 0.5, middle));
                let row = words.get(a.min(b)..a.max(b)).unwrap_or_default();
                let row = row.trim();
                if !row.is_empty() && rect.width() > 0.0 && rect.height() > 0.0 {
                    lines.push((rect, row.to_owned()));
                }
            }
        }
        lines
    }

    /// Paint a message's body where [`Self::layout_body`] last put it.
    fn draw_body(&self, m: &Message, paint: &mut PaintCtx<'_>) {
        if let Some((_, view)) = self.body_views.borrow_mut().get_mut(&(0, m.id.clone())) {
            view.draw(paint.scene, paint.transform, paint.text, paint.theme);
        }
    }

    /// Paint `m`'s author through the shared author helper: the avatar (an
    /// agent's mark, a bot's tile) into `mark`, the name from `name_at`, and
    /// for a bot its `BOT` chip, which is never elided. Nothing goes past
    /// `max_x`. Returns where the line ends, the name line's height and
    /// where its parts went.
    fn paint_author(
        &self,
        ctx: &mut PaintCtx<'_>,
        m: &Message,
        mark: Rect,
        name_at: Point,
        max_x: f64,
    ) -> (f64, f64, ui::author::AuthorLine) {
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
        for part in [line.chip.as_mut(), line.badge.as_mut()]
            .into_iter()
            .flatten()
        {
            *part = part.with_origin((part.x0, name_at.y + (line_h - part.height()) / 2.0));
        }
        author.paint(&line, ctx);
        let end = line
            .chip
            .or(line.badge)
            .map_or(line.name.x1, |part| part.x1);
        (end, line_h, line)
    }

    /// A person's avatar and name, through both, open their card; a bot's
    /// or an agent's message has none.
    fn push_author_hit(
        &self,
        m: &Message,
        parts: &crate::app::PaintedMessage,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        if m.author.starts_with("bot:")
            || self.agent_authors.contains_key(&m.author)
            || self.is_provider_output(m)
        {
            return;
        }
        let name = parts.author.as_ref().map(|(rect, _)| *rect);
        let face = match (parts.avatar, name) {
            (Some(avatar), Some(name)) => Some(avatar.union(name)),
            (avatar, name) => avatar.or(name),
        };
        if let Some(face) = face {
            ChatApp::push_hit(
                hits,
                face,
                Hit::ShowPerson {
                    person: m.author.clone(),
                    place: m.id.clone(),
                },
            );
        }
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
            // The member a persona is linked to, by the name the room knows
            // them by: a linked account speaks as that member.
            let linked = m
                .appearance
                .as_ref()
                .and_then(|appearance| appearance.persona.as_ref()?.linked.as_deref())
                .and_then(|person| self.users.iter().find(|user| user.id == person));
            Author::Bot {
                bot_name: name,
                // The roster does not name a bot's installer yet; the chip's
                // details are not shown in Chat so far.
                installed_by: "",
                appearance: m.appearance.as_ref(),
                linked: linked.map(|user| user.name.as_str()),
                // A linked account wears its person's colour, as their own
                // messages, the bar and the call do.
                linked_color: linked.map(|user| self.identity_colour(&user.id)),
                image: shown,
            }
        } else if self.is_provider_output(m) {
            Author::Agent {
                name,
                for_person: Some(initiator),
                color,
            }
        } else if self.agent_authors.contains_key(&m.author) {
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
        self.pointer_over(rect)
    }

    /// Whether an open context menu acts on `target`: its row stays lifted
    /// while the menu is open, so it is clear what the menu will change.
    pub(crate) fn menu_targets(&self, target: &MenuTarget) -> bool {
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
    /// The grip a direct conversation is dragged out of the window by, to
    /// float it over the desktop: at the conversation's top trailing corner,
    /// over the messages. Its Float action is the window bar's.
    fn draw_conversation_grip(
        &self,
        scene: &mut Scene,
        tf: Affine,
        theme: &Theme,
        area: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        if self.direct_person.is_none() || self.conversation_surface || area.width() <= 0.0 {
            return;
        }
        let control = ButtonFace::height(ControlSize::Regular, theme);
        let inset = f64::from(theme.tokens.spacing.s2);
        let grip = Rect::new(
            area.x1 - self.chrome.gutter - control,
            area.y0 + inset,
            area.x1 - self.chrome.gutter,
            area.y0 + inset + control,
        );
        ui::surface::floating_drag_grip(scene, tf, grip, theme.text_dim);
        ChatApp::push_hit(hits, grip, Hit::AgentDragHandle);
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
        if let Some(peer) = &self.direct_person {
            return HeaderIdentityText {
                name: self.person_display_name(peer),
                kind: Some(crate::i18n::t("chat-person-chat-private")),
                context: String::new(),
            };
        }

        let (name, topic) = self.channel(&self.active_channel).map_or_else(
            || (String::new(), String::new()),
            |channel| (channel.name.clone(), channel.topic.clone()),
        );
        HeaderIdentityText {
            name,
            kind: None,
            context: if context.is_empty() { topic } else { context },
        }
    }

    // ------------------------------------------------------------ messages
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
        [one] => Some(crate::i18n::t_args(
            "chat-typing-one",
            &[("name", one.clone().into())],
        )),
        [one, two] => Some(crate::i18n::t_args(
            "chat-typing-two",
            &[
                ("first", one.clone().into()),
                ("second", two.clone().into()),
            ],
        )),
        _ => Some(crate::i18n::t("chat-typing-several")),
    }
}

/// The local time of `ts` as the person reads clocks: the toolkit's clock, the
/// same one Messages shows on every line.
fn clock_label(ts: i64) -> String {
    ConversationClock::local().clock(ts)
}

/// "Today", "Yesterday", the weekday, or the date: the toolkit's day label,
/// the same rule Messages uses for its dividers.
fn day_label(ts: i64) -> String {
    let clock = ConversationClock::local();
    clock.day_label(clock.day(ts))
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
) -> Rect {
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
    rect
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

mod agent_work;
mod attach_picker;
mod composer;
mod members;
mod messages;
mod thread;

pub(crate) use agent_work::agent_work_accessible_description;
/// Per-row measured heights for the timeline layout.
/// One thing the timeline paints, in order: a day divider, or a message.
enum TimelineItem {
    /// The day changed here: the shared divider band, labelled.
    Divider {
        label: String,
    },
    Row(RowM),
}

#[derive(Clone)]
pub(crate) struct RowM {
    pub(crate) idx_id: String,
    pub(crate) show_header: bool,
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
    pub(crate) resource_h: f64,
    att_h: f64,
    react_h: f64,
    edited_h: f64,
    thread_h: f64,
    /// This row is the message being edited inline: its body is the editor
    /// field (padded) plus a controls row.
    pub(crate) editing: bool,
    edit_ctrl_h: f64,
}

impl RowM {
    pub(crate) fn height(&self) -> f64 {
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
    use super::agent_work::{AgentWorkLineStyle, AgentWorkPresentation};
    use super::{RowM, TimelineItem, media_face, media_meta};
    use crate::model::{
        AgentReasoning, AgentRoute, AgentStep, AgentStepOutcome, AgentWork, AgentWorkState,
        Attachment,
    };
    use ui::theme::Theme;
    use ui::widget::{MediaArt, MediaSize};

    /// A conversation that crosses midnight gets a divider at the crossing,
    /// not only at the oldest message: scrolling to a line from three days ago
    /// must show which day it belongs to.
    #[test]
    fn a_divider_appears_wherever_the_day_changes() {
        use crate::model::Message;
        let theme = Theme::dark();
        let mut text = ui::text_render::TextRenderer::new();
        let identity = crate::identity::LocalIdentity::demo();
        let doc = automerge::AutoCommit::new();
        let mut app = crate::ChatApp::new(
            identity,
            crate::app::ChatRuntime::Unavailable,
            tilo_content::ContentStore::memory(),
        );
        app.attach_framework(ui::framework::Dispatcher::detached(
            "so.tilo.chat",
            &app.me.id.clone(),
        ));
        let line = |id: &str, ts: i64| Message {
            id: id.into(),
            channel: "c".into(),
            author: "u_ada".into(),
            ts,
            text: "x".into(),
            body: Vec::new(),
            edited: false,
            history: Vec::new(),
            parent: None,
            reactions: Vec::new(),
            mentions: Vec::new(),
            reference: None,
            source_provider: None,
            attachment: None,
            pinned: false,
            work: None,
            appearance: None,
            share: None,
        };
        // Two hours apart across midnight, and one three days later.
        let start = 1_782_828_300_i64;
        app.messages = vec![
            line("a", start),
            line("b", start.saturating_add(3_600)),
            line("c", start.saturating_add(4 * 86_400)),
        ];
        let groups = crate::model::group_messages(&app.messages);
        let clock = ui::widget::ConversationClock::at(0, start);
        let (items, _) = app.measure_message_rows(&mut text, &theme, &groups, 400.0, clock);
        let dividers = items
            .iter()
            .filter(|item| matches!(item, TimelineItem::Divider { .. }))
            .count();
        assert_eq!(
            dividers, 2,
            "one divider for the first day and one where the day changes"
        );
        let _ = doc;
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
        assert_eq!(media_meta(&unknown_video), vec!["1 kB".to_owned()]);
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
        assert_eq!(ui::format::size(18_944), "18.9 kB");
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
