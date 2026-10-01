//! Rendering for the persistent chat surface: workspace rail, channel
//! sidebar, channel header, the grouped message timeline, the composer,
//! and the thread side-panel. All geometry is computed from `self.rect`;
//! every clickable area is recorded into `hits`. Colours come only from
//! the [`Theme`].

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use ui::ColorCompat;
use ui::Widget;
use ui::format::bytes as format_bytes;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::widget::{Avatar, Button, ButtonFace};

use crate::app::{AVATAR, ChatApp, Hit, MenuTarget};
use crate::geometry::{SidebarHeaderSpec, centered_band, line_top, sidebar_header_layout};
use crate::model::{self, Attachment, ChannelKind, Message, Presence};
use crate::paint::{
    band_at_least, blend_toward, fill_circle, fill_rect, fill_round, hline, label, legible_on,
    palette_color, push_clip, stroke_round, text, truncate_to,
};

/// Body text size for a message. Shared by the measure and the draw so a
/// mention-carrying row reserves exactly the height it paints.
pub(crate) const BODY_SIZE: f32 = 15.0;

/// Author name in a message-group header; the header row's height is this
/// line box (measured) plus one small gap, so it grows with the text scale.
const NAME_SIZE: f32 = 14.5;
/// Timestamp beside the author name; bottom-aligned to the name's line box.
const TIME_SIZE: f32 = 11.5;
/// 1× height of the "(edited) · view history" band under a message body.
const EDITED_MIN_H: f64 = 18.0;
/// Reaction chip label ("🔥  2").
const REACTION_SIZE: f32 = 12.5;
/// A reaction chip is never shorter than this capsule at 1×; it grows with
/// the measured label at larger text. Off the spacing scale on purpose (a
/// 26px capsule around 12.5px text).
const REACTION_CHIP_MIN_H: f64 = 26.0;
/// Space between the reaction strip and whatever follows it.
const REACTION_STRIP_GAP: f64 = 6.0;
/// Thread affordance label ("3 replies").
const THREAD_LABEL_SIZE: f32 = 13.0;
/// The "N replies" bar is never shorter than this at 1×; it grows with its
/// measured label. Off-scale by design (a 38px bar carrying 22px avatars).
const THREAD_BAR_MIN_H: f64 = 38.0;

/// Channel name in a sidebar row; the row is this measured line box plus a
/// small pad, never shorter than the 1× row.
const CHANNEL_NAME_SIZE: f32 = 14.0;
/// 1× channel row height (off-scale by design: a 32px row around 14px text).
const CHANNEL_ROW_MIN_H: f64 = 32.0;
/// Space between consecutive channel rows.
const CHANNEL_ROW_GAP: f64 = 2.0;
/// Member name in a voice-presence row under a voice channel.
/// Category header ("CHANNELS", "VOICE") label size and 1× row height.
const CATEGORY_NAME_SIZE: f32 = 10.5;
const CATEGORY_ROW_MIN_H: f64 = 24.0;
/// Space under a category's last channel before the next header.
const CATEGORY_GAP: f64 = 6.0;
/// "+ Category" chip under the workspace title: catalogue key, label size
/// and the 1× chip it never shrinks below.
const ADD_CATEGORY_KEY: &str = "chat-sidebar-add-category";
const ADD_CATEGORY_SIZE: f32 = 11.0;
const ADD_CATEGORY_CHIP_MIN_W: f64 = 106.0;
const ADD_CATEGORY_CHIP_MIN_H: f64 = 22.0;
/// Identity bar at the sidebar's bottom: name / status sizes, 1× height and
/// 1× avatar radius (the disc scales with the text like the initials do).
const IDENTITY_NAME_SIZE: f32 = 13.0;
const IDENTITY_SUB_SIZE: f32 = 11.0;
const IDENTITY_BAR_MIN_H: f64 = 56.0;
const IDENTITY_AVATAR_R: f64 = 16.0;
/// Call toggle buttons in the identity bar: glyph size and 1× square.
const IDENTITY_CTL_GLYPH: f32 = 14.0;
const IDENTITY_CTL_MIN: f64 = 32.0;
/// Cross-workspace inbox sidebar: channel-name / workspace-label sizes and
/// their 1× rows.
const INBOX_NAME_SIZE: f32 = 13.5;
const INBOX_ROW_MIN_H: f64 = 36.0;
const INBOX_LABEL_SIZE: f32 = 10.0;
const INBOX_LABEL_MIN_H: f64 = 22.0;
const VOICE_NAME_SIZE: f32 = 12.0;
/// 1× voice-presence row height.
const VOICE_ROW_MIN_H: f64 = 24.0;
/// Unread-count badge text and its 1× capsule height.
const UNREAD_BADGE_TEXT: f32 = 11.0;
const UNREAD_BADGE_MIN_H: f64 = 18.0;

/// Height of the channel-sidebar title row. Off the spacing scale on purpose:
/// it is the 52px band the rest of the sidebar chrome (hairline, "+ Category")
/// hangs from.
const SIDEBAR_HEADER_H: f64 = 52.0;
/// The presence pill's text. Between `xs` and `sm` on the type scale; kept as
/// the authored 10.5 so the capsule keeps its current density.
const PRESENCE_PILL_TEXT: f32 = 10.5;
/// Height of the presence capsule around [`PRESENCE_PILL_TEXT`]; off-scale.
const PRESENCE_PILL_H: f64 = 18.0;
/// Radius of the online dot inside the presence pill.
const PRESENCE_DOT_R: f64 = 3.0;
/// Below this remaining width the workspace title would be a stub, so the
/// presence pill drops onto its own row instead.
const SIDEBAR_TITLE_MIN_W: f64 = 72.0;
/// Width of the composer's send button (the emoji and attach buttons are
/// `spacing.s8` squares). Off-scale: a wide target for the primary action.
const SEND_BTN_W: f64 = 44.0;
/// Horizontal padding the shared multiline `TextInput` keeps around its text
/// (its `padding_standard` gutter plus its own inner pad). Used only to pick
/// the wrap width the composer's draft is measured at; the resulting band is
/// clamped to the box, so a small mismatch cannot leak outside it.
const COMPOSER_TEXT_INSET: f64 = 22.0;

/// How much of the participant's colour a mention *of you* keeps when it is
/// tinted toward the surface. A filled band, so it can be found by scanning.
const MENTION_SELF_BAND_MIX: f64 = 0.78;

/// Contrast floors for the mention run, measured rather than eyeballed:
/// AA body text wherever it lands, and the non-text floor for the filled band
/// against the surface it sits on (a band nobody can see is not a highlight).
pub(crate) const MENTION_TEXT_CONTRAST: f64 = 4.5;
pub(crate) const MENTION_BAND_CONTRAST: f64 = 3.0;

/// Geometry + model bundle shared by a channel row's drawing helpers, so each
/// one takes a handful of arguments instead of the whole row's context.
struct ChannelRowCtx<'a> {
    row: Rect,
    side: Rect,
    y: f64,
    is_active: bool,
    ch: &'a crate::model::Channel,
}

#[derive(Clone, Copy)]
struct ChannelRowInput<'a> {
    side: Rect,
    channel: &'a crate::model::Channel,
    y: f64,
    swipe_dx: f64,
}

#[derive(Clone, Copy)]
struct AttachmentLayout {
    origin: Point,
    max_width: f32,
}

#[derive(Clone, Copy)]
struct MessageColumn {
    area: Rect,
    text_x: f64,
    text_width: f32,
}

#[derive(Clone, Copy)]
struct MessageBodyLayout {
    column: MessageColumn,
    content_top: f64,
}

#[derive(Clone, Copy)]
struct MessageRowRef<'a> {
    measurement: &'a RowM,
    message: &'a Message,
}

impl AttachmentLayout {
    fn new(x: f64, y: f64, max_width: f32) -> Self {
        Self {
            origin: Point::new(x, y),
            max_width,
        }
    }
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
    let (r, g, b) = app.participant_color(user);
    let own = Color::from_rgb8(r, g, b);
    if !is_self {
        // Their colour, made legible on the surface the message is on.
        let ink = legible_on(
            own,
            ground,
            MENTION_TEXT_CONTRAST,
            theme.tokens.colors.text_primary,
        );
        return (None, ink);
    }
    let band = band_at_least(
        blend_toward(own, ground, MENTION_SELF_BAND_MIX),
        own,
        ground,
        MENTION_BAND_CONTRAST,
    );
    // Ink for a filled band starts from body ink and is pushed away from the
    // band — text in the band's own hue on the band is the one combination
    // that cannot be rescued.
    let ink = legible_on(
        theme.tokens.colors.text_primary,
        band,
        MENTION_TEXT_CONTRAST,
        theme.tokens.colors.text_primary,
    );
    (Some(band), ink)
}

impl ChatApp {
    /// The colours mentions are painted in on `theme`, for every participant
    /// (a mention of you filled, anyone else's in their colour), and a
    /// fingerprint of what they were built from.
    pub(crate) fn mention_paint(&self, theme: &Theme) -> (u64, ui::widget::marks::MentionPaint) {
        use std::hash::{Hash, Hasher};
        let ground = theme.tokens.colors.surfaces.base;
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
            ink: theme.tokens.colors.accent[5],
            band: None,
        };
        let map: std::collections::HashMap<String, ui::widget::marks::MentionColors> =
            entries.into_iter().collect();
        let paint: ui::widget::marks::MentionPaint =
            std::rc::Rc::new(move |user: &str| map.get(user).copied().unwrap_or(fallback));
        *self.mention_paint_cache.borrow_mut() = Some((fingerprint, std::rc::Rc::clone(&paint)));
        (fingerprint, paint)
    }

    /// Lay a message's body out at `(x, y)` and `width` in its read-only
    /// view, at `size`; returns the view's height. The view is kept by
    /// message id, so an unchanged body is not shaped again.
    fn layout_body(
        &self,
        m: &Message,
        size: f32,
        origin: Point,
        width: f32,
        theme: &Theme,
        tr: &mut TextRenderer,
    ) -> f64 {
        let (fingerprint, paint) = self.mention_paint(theme);
        let mut views = self.body_views.borrow_mut();
        let (painted, view) = views
            .entry((size.to_bits(), m.id.clone()))
            .or_insert_with(|| {
                (
                    0,
                    tilo_rich_text::RichTextView::with_editor(
                        tilo_rich_text::compact_editor().with_body_size(size),
                    ),
                )
            });
        if *painted != fingerprint {
            view.set_mention_paint(paint);
            *painted = fingerprint;
        }
        view.show(&m.blocks());
        view.layout(
            Rect::new(origin.x, origin.y, origin.x + f64::from(width), origin.y),
            theme,
            tr,
        )
    }

    /// Paint a message's body where [`Self::layout_body`] last put it.
    fn draw_body(&self, m: &Message, size: f32, paint: &mut PaintCtx<'_>) {
        if let Some((_, view)) = self
            .body_views
            .borrow_mut()
            .get_mut(&(size.to_bits(), m.id.clone()))
        {
            view.draw(paint.scene, paint.transform, paint.text, paint.theme);
        }
    }
}

/// Draw a circular initials avatar with an optional presence dot.
pub(crate) fn draw_avatar(
    paint: PaintCtx<'_>,
    center: Point,
    r: f64,
    initials: &str,
    color_idx: usize,
    status: Option<Presence>,
) {
    let PaintCtx {
        scene,
        transform: tf,
        text: tr,
        theme,
    } = paint;
    // Peer colour comes from the per-peer palette index (deliberately distinct
    // per identity, and shared with the workspace-tint colouring), so it's
    // passed straight through; the disc + centred monogram are painted by the
    // shared toolkit avatar so shape/placement match every other app's roster.
    let bg = palette_color(theme, color_idx);
    let rect = Rect::new(center.x - r, center.y - r, center.x + r, center.y + r);
    Avatar::draw_at(scene, tf, rect, initials, bg, theme, tr);
    if let Some(st) = status {
        let c = &theme.tokens.colors;
        let dot = match st {
            Presence::Online => c.semantic.success,
            Presence::Idle => c.semantic.warning,
            Presence::Dnd => c.semantic.danger,
            Presence::Offline => c.neutral[5],
        };
        let dc = Point::new(center.x + r * 0.72, center.y + r * 0.72);
        fill_circle(scene, tf, dc, r * 0.32 + 2.0, c.surfaces.base);
        fill_circle(scene, tf, dc, r * 0.32, dot);
    }
}

/// Human label for a presence state, shown under a member in the roster.
fn presence_label(p: Presence) -> &'static str {
    match p {
        Presence::Online => "Online",
        Presence::Idle => "Idle",
        Presence::Dnd => "Do not disturb",
        Presence::Offline => "Offline",
    }
}

impl ChatApp {
    // ---------------------------------------------------------------- rail
    #[allow(
        clippy::too_many_lines,
        reason = "the compact rail paints the two related destinations and their badges from one shared geometry pass"
    )]
    pub(crate) fn draw_rail(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let rail = self.rail_rect();
        fill_rect(scene, tf, rail, c.neutral[0]);

        let btn = 48.0;
        let cx = rail.x0 + RAIL_W_CENTER;
        let mut y = rail.y0 + 16.0;

        // Inbox is a system-wide destination, not another workspace. Its
        // fixed envelope icon and aggregate unread badge make that distinction
        // visible before a person opens it.
        let inbox = Rect::new(cx - btn / 2.0, y, cx + btn / 2.0, y + btn);
        // A rail tile at rest is a raised disc with its derived edge (HIG
        // §9.5); the open state is the accent, its own contrast step.
        if self.inbox_open {
            fill_round(scene, tf, inbox, 16.0, c.accent[5]);
        } else {
            ui::surface::card(scene, tf, inbox, 24.0, c.neutral[2]);
        }
        label(
            scene,
            tr,
            tf,
            "✉",
            20.0,
            if self.inbox_open {
                c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST)
            } else {
                c.text_dim
            },
            inbox.center().x - 10.0,
            inbox.center().y - 12.0,
        );
        let unread: u32 = self.channels.iter().map(|channel| channel.unread).sum();
        if unread > 0 {
            let badge = Rect::new(
                inbox.x1 - 16.0,
                inbox.y0 - 4.0,
                inbox.x1 + 5.0,
                inbox.y0 + 17.0,
            );
            fill_round(scene, tf, badge, 10.5, c.semantic.danger);
            let count = if unread > 99 {
                "99+".into()
            } else {
                unread.to_string()
            };
            let (w, _) = tr.measure(&count, 9.0, None);
            label(
                scene,
                tr,
                tf,
                &count,
                9.0,
                c.on_surface(c.semantic.danger).ink,
                badge.center().x - f64::from(w) / 2.0,
                badge.center().y - 6.0,
            );
        }
        ChatApp::push_hit(hits, inbox, Hit::Inbox);
        y += btn + 18.0;
        hline(
            scene,
            tf,
            rail.x0 + 18.0,
            rail.x1 - 18.0,
            y - 7.0,
            ui::surface::edge_on(c.neutral[0]),
        );

        // One contextual destination: the Tilo workspace that owns this Chat
        // instance. Other workspaces are reachable only through Inbox.
        if let Some(ws) = self.workspaces.iter().find(|ws| ws.id == self.context_ws) {
            let active = !self.inbox_open;
            let b = Rect::new(cx - btn / 2.0, y, cx + btn / 2.0, y + btn);
            if active {
                let pill = Rect::new(rail.x0 + 3.0, y + 8.0, rail.x0 + 7.0, y + btn - 8.0);
                fill_round(scene, tf, pill, 2.0, c.text_primary);
            }
            if active {
                fill_round(scene, tf, b, 16.0, palette_color(theme, ws.color_idx));
            } else {
                ui::surface::card(scene, tf, b, btn / 2.0, c.neutral[2]);
            }
            let (tw, th) = tr.measure_weighted(&ws.glyph, 17.0, None, TextWeight::Bold);
            text(
                &mut PaintCtx::new(scene, tf, tr, theme),
                &ws.glyph,
                &TextStyle::new(17.0).with_weight(TextWeight::Bold),
                if active {
                    c.on_surface(palette_color(theme, ws.color_idx)).ink
                } else {
                    c.text_dim
                },
                Point::new(
                    b.center().x - f64::from(tw) / 2.0,
                    b.center().y - f64::from(th) / 2.0,
                ),
                None,
            );
            ChatApp::push_hit(hits, b, Hit::CurrentContext);
        }
        // The single user avatar lives in the sidebar identity bar (drawn in
        // `draw_sidebar`), not here — the rail foot intentionally has none.
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
        let c = &theme.tokens.colors;
        let side = self.sidebar_rect();
        fill_rect(scene, tf, side, c.surfaces.raised);

        if self.inbox_open {
            self.draw_inbox_sidebar(scene, tf, tr, theme, side, hits);
            return;
        }

        let header = self.draw_sidebar_header(scene, tf, tr, theme, side, hits);
        self.draw_sidebar_tree(
            PaintCtx::new(scene, tf, tr, theme),
            presence,
            side,
            header,
            hits,
        );
        // Tilo stays calm while a workspace is private. The shell already owns
        // the current person's identity; repeating an "Available" row in a
        // one-person Chat workspace only turns an empty conversation into
        // collaboration chrome. It becomes useful once there is someone to be
        // available to, or while a call is active.
        if self.has_other_collaborator() || self.call.active {
            self.draw_sidebar_identity_bar(scene, tf, tr, theme, side, hits);
        }
    }

    /// Cross-workspace conversation navigation. The CRDT keeps its legacy
    /// workspace ids for wire compatibility; this projection makes them source
    /// labels inside one Inbox instead of presenting them as nested workspaces.
    #[allow(
        clippy::too_many_lines,
        reason = "one immediate-mode grouped conversation list keeps its source headings, unread badges, selection and hit geometry aligned"
    )]
    fn draw_inbox_sidebar(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        side: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        label(
            scene,
            tr,
            tf,
            "Inbox",
            17.0,
            c.text_primary,
            side.x0 + 16.0,
            side.y0 + 13.0,
        );
        label(
            scene,
            tr,
            tf,
            "Conversations across Tilo workspaces",
            10.5,
            c.text_dim,
            side.x0 + 16.0,
            side.y0 + 36.0,
        );
        hline(
            scene,
            tf,
            side.x0 + 12.0,
            side.x1 - 12.0,
            side.y0 + 58.0,
            ui::surface::edge_on(c.surfaces.raised),
        );

        let mut y = side.y0 + 68.0;
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
            // Every row is its measured line plus pads (never shorter than
            // the 1× row) and every part of it is centred on the row's
            // measured centre line, so 200% text grows the list instead of
            // stacking rows and labels over each other.
            let sp = &theme.tokens.spacing;
            let label_row_h = inbox_label_h(tr, theme);
            let ws_label = workspace.name.to_uppercase();
            let (_, lh) = tr.measure(&ws_label, INBOX_LABEL_SIZE, None);
            label(
                scene,
                tr,
                tf,
                &ws_label,
                INBOX_LABEL_SIZE,
                c.text_dim.with_alpha_factor(0.78),
                side.x0 + f64::from(sp.s4),
                line_top(y + label_row_h / 2.0, f64::from(lh)),
            );
            y += label_row_h;
            let row_h = inbox_row_h(tr, theme);
            // The same "#" glyph leads every row; measure it once per pass.
            let (_, hash_h) = tr.measure("#", CHANNEL_NAME_SIZE, None);
            for channel in channels {
                let row = Rect::new(side.x0 + 8.0, y, side.x1 - 8.0, y + row_h);
                let cy = row.center().y;
                let selected = channel.id == self.active_channel;
                if selected {
                    fill_round(scene, tf, row, 8.0, c.accent[5].with_alpha_factor(0.16));
                    let bar_h = (row_h - 2.0 * 7.0).max(8.0);
                    fill_round(
                        scene,
                        tf,
                        Rect::new(side.x0, cy - bar_h / 2.0, side.x0 + 4.0, cy + bar_h / 2.0),
                        2.0,
                        c.accent[5],
                    );
                }
                label(
                    scene,
                    tr,
                    tf,
                    "#",
                    CHANNEL_NAME_SIZE,
                    if selected { c.accent[7] } else { c.text_dim },
                    row.x0 + 10.0,
                    line_top(cy, f64::from(hash_h)),
                );
                // The badge is measured first and reserved, then the name is
                // truncated into what remains before it.
                let badge = (channel.unread > 0).then(|| {
                    let count = channel.unread.to_string();
                    let (w, h) = tr.measure(&count, UNREAD_BADGE_TEXT, None);
                    let badge_h = (f64::from(h) + 2.0 * f64::from(sp.s1)).max(UNREAD_BADGE_MIN_H);
                    let badge_w = f64::from(w) + 2.0 * (f64::from(sp.s1) + 3.0);
                    let x1 = row.x1 - 6.0;
                    (
                        count,
                        Rect::new(x1 - badge_w, cy - badge_h / 2.0, x1, cy + badge_h / 2.0),
                    )
                });
                let name_x = row.x0 + 32.0;
                let name_right = badge.as_ref().map_or_else(
                    || row.x1 - f64::from(sp.s2),
                    |(_, b)| b.x0 - f64::from(sp.s2),
                );
                let weight = if channel.unread > 0 {
                    TextWeight::Semibold
                } else {
                    TextWeight::Regular
                };
                let name = truncate_to(tr, &channel.name, INBOX_NAME_SIZE, name_right - name_x);
                let (_, nh) = tr.measure_weighted(&name, INBOX_NAME_SIZE, None, weight);
                text(
                    &mut PaintCtx::new(scene, tf, tr, theme),
                    &name,
                    &TextStyle::new(INBOX_NAME_SIZE).with_weight(weight),
                    if selected || channel.unread > 0 {
                        c.text_primary
                    } else {
                        c.text_dim
                    },
                    Point::new(name_x, line_top(cy, f64::from(nh))),
                    None,
                );
                if let Some((count, badge)) = badge {
                    fill_round(scene, tf, badge, badge.height() / 2.0, c.accent[5]);
                    let (cw, ch) = tr.measure(&count, UNREAD_BADGE_TEXT, None);
                    label(
                        scene,
                        tr,
                        tf,
                        &count,
                        UNREAD_BADGE_TEXT,
                        c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
                        badge.center().x - f64::from(cw) / 2.0,
                        line_top(cy, f64::from(ch)),
                    );
                }
                ChatApp::push_hit(hits, row, Hit::Channel(channel.id.clone()));
                y += row_h + CHANNEL_ROW_GAP;
            }
            y += f64::from(sp.s2);
        }
    }

    /// Workspace title, online-count pill, and the "+ category" affordance
    /// beneath it. Returns the title-row rect so the caller can anchor the
    /// channel tree below it.
    #[expect(
        clippy::too_many_lines,
        reason = "one paint pass keeps the header's measured rows and their hit targets adjacent"
    )]
    fn draw_sidebar_header(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        side: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> Rect {
        let c = &theme.tokens.colors;
        // workspace title header
        let ws_name = self
            .workspaces
            .iter()
            .find(|w| w.id == self.active_ws)
            .map(|w| w.name.clone())
            .unwrap_or_default();
        let sp = &theme.tokens.spacing;
        let title_size = theme.tokens.typography.scale.lg;
        let title_full = format!("{ws_name} channels");
        let title_h = f64::from(tr.measure(&title_full, title_size, None).1);
        // Presence is meaningful only when this workspace has another person
        // in it. Do not surface "1 person online" as a pseudo-status in a
        // private workspace.
        let pill_txt = self.has_other_collaborator().then(|| {
            let online = self
                .users
                .iter()
                .filter(|u| u.status == Presence::Online)
                .count();
            let noun = if self.users.iter().any(|u| u.is_agent) {
                if online == 1 {
                    "collaborator"
                } else {
                    "collaborators"
                }
            } else if online == 1 {
                "person"
            } else {
                "people"
            };
            format!("{online} {noun} online")
        });
        // The pill is measured first and reserved in full — dot, its gap, the
        // text and the capsule's two side pads — and only then is the title
        // budgeted into the remainder, so the two never paint over each other.
        let pill_inset = 2.0 * f64::from(sp.s2) + 2.0 * PRESENCE_DOT_R + f64::from(sp.s1);
        let pill = pill_txt.as_deref().map(|txt| {
            let (pw, _) = tr.measure(txt, PRESENCE_PILL_TEXT, None);
            (f64::from(pw) + pill_inset, PRESENCE_PILL_H)
        });
        let layout = sidebar_header_layout(&SidebarHeaderSpec {
            side,
            row_h: SIDEBAR_HEADER_H,
            inset_l: f64::from(sp.s4),
            inset_r: f64::from(sp.s3),
            gap: f64::from(sp.s2),
            title_h,
            pill,
            min_title_w: SIDEBAR_TITLE_MIN_W,
        });
        let header = layout.header;
        let ws_title = truncate_to(tr, &title_full, title_size, layout.title_max_w);
        label(
            scene,
            tr,
            tf,
            &ws_title,
            title_size,
            c.text_primary,
            layout.title_x,
            layout.title_y,
        );
        if let (Some(txt), Some(pill)) = (pill_txt.as_deref(), layout.pill) {
            // A chip on the sidebar: lifted fill + derived edge (HIG §9.5).
            ui::surface::card(scene, tf, pill, PRESENCE_PILL_H / 2.0, c.lift(0.72));
            let dot_cx = pill.x0 + f64::from(sp.s2) + PRESENCE_DOT_R;
            fill_circle(
                scene,
                tf,
                Point::new(dot_cx, pill.center().y),
                PRESENCE_DOT_R,
                c.semantic.success,
            );
            let (_, ph) = tr.measure(txt, PRESENCE_PILL_TEXT, None);
            label(
                scene,
                tr,
                tf,
                txt,
                PRESENCE_PILL_TEXT,
                c.text_dim,
                dot_cx + PRESENCE_DOT_R + f64::from(sp.s1),
                line_top(pill.center().y, f64::from(ph)),
            );
        }
        // The header's rule is derived against the sidebar's raised ground
        // and stops an `s3` short of both ends (HIG §9.5).
        ui::surface::separator(
            scene,
            tf,
            side.x0,
            side.x1,
            header.y1,
            f64::from(sp.s3),
            c.surfaces.raised,
        );

        // "+ category" affordance below the workspace title: a chip sized
        // from its measured label (never smaller than the 1× chip), with the
        // label centred on the chip's measured line box. Lifted fill plus its
        // derived edge, so the chip reads as a control on both presets.
        let (chip_w, chip_h) = add_category_chip_size(tr, theme);
        let add_cat = Rect::new(
            side.x0 + f64::from(sp.s3),
            header.y1 + CATEGORY_GAP,
            side.x0 + f64::from(sp.s3) + chip_w,
            header.y1 + CATEGORY_GAP + chip_h,
        );
        ui::surface::card(scene, tf, add_cat, 7.0, c.lift(0.35));
        let add_label = crate::i18n::t(ADD_CATEGORY_KEY);
        let (lw, lh) = tr.measure(&add_label, ADD_CATEGORY_SIZE, None);
        label(
            scene,
            tr,
            tf,
            &add_label,
            ADD_CATEGORY_SIZE,
            c.text_dim,
            add_cat.center().x - f64::from(lw) / 2.0,
            line_top(add_cat.center().y, f64::from(lh)),
        );
        ChatApp::push_hit(hits, add_cat, Hit::AddCategory(self.active_ws.clone()));
        header
    }

    /// The channel tree: an uncategorised group first, then each category
    /// with a collapse caret, a "+" to add a channel, and its channels.
    #[expect(
        clippy::too_many_lines,
        reason = "one paint pass walks the channel tree once, drawing each row beside its hit target"
    )]
    fn draw_sidebar_tree(
        &mut self,
        paint: PaintCtx<'_>,
        presence: &PresenceCtx,
        side: Rect,
        header: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let c = &theme.tokens.colors;
        let sp = &theme.tokens.spacing;
        // Start under the measured "+ Category" chip and stop above the
        // measured identity bar, so at a larger text scale the tree neither
        // paints over the chip nor runs under the bar.
        let (_, chip_h) = add_category_chip_size(tr, theme);
        let mut y = header.y1 + CATEGORY_GAP + chip_h + f64::from(sp.s2);
        let body_bottom = side.y1 - identity_bar_h(tr, theme) - f64::from(sp.s1);
        let hrow_h = category_row_h(tr, theme);
        // The caret and "+" glyphs are the same for every category; measure
        // them once per pass rather than once per row.
        let (_, caret_h) = tr.measure("▾", 10.0, None);
        let (pw, ph) = tr.measure("+", 16.0, None);

        // Uncategorised channels (category id "").
        y = self.draw_channel_group(
            PaintCtx::new(scene, tf, tr, theme),
            presence,
            side,
            None,
            y,
            hits,
        );

        let categories: Vec<_> = self.ws_categories().into_iter().cloned().collect();
        for cat in categories {
            if y > body_bottom {
                break;
            }
            // Category header row: caret + name + "+", every part centred on
            // the row's measured centre line.
            let hrow = Rect::new(side.x0 + 8.0, y, side.x1 - 8.0, y + hrow_h);
            let cy = hrow.center().y;
            let caret = if cat.collapsed { "▸" } else { "▾" };
            label(
                scene,
                tr,
                tf,
                caret,
                10.0,
                c.text_dim,
                hrow.x0 + f64::from(sp.s1),
                line_top(cy, f64::from(caret_h)),
            );
            // "+" add-channel button: a square the row's height at the right
            // edge, its glyph centred by measurement.
            let plus = Rect::new(hrow.x1 - hrow_h, y, hrow.x1, y + hrow_h);
            label(
                scene,
                tr,
                tf,
                "+",
                16.0,
                c.text_dim,
                plus.center().x - f64::from(pw) / 2.0,
                line_top(cy, f64::from(ph)),
            );
            ChatApp::push_hit(
                hits,
                plus,
                Hit::AddChannel {
                    workspace: self.active_ws.clone(),
                    category: cat.id.clone(),
                },
            );
            // Inline rename for a category, else its name, budgeted to stop a
            // gap before the "+" so the two never paint over each other.
            let name_x = hrow.x0 + 18.0;
            let name_w = (plus.x0 - f64::from(sp.s2) - name_x).max(20.0);
            let renaming_cat =
                matches!(&self.rename, Some(MenuTarget::Category(id)) if id == &cat.id);
            if renaming_cat {
                self.draw_inline_rename(
                    PaintCtx::new(scene, tf, tr, theme),
                    presence,
                    name_x,
                    y + (hrow_h - 16.0).max(0.0) / 2.0,
                    name_w,
                    hits,
                );
            } else {
                let name = truncate_to(tr, &cat.name.to_uppercase(), CATEGORY_NAME_SIZE, name_w);
                let (_, nh) =
                    tr.measure_weighted(&name, CATEGORY_NAME_SIZE, None, TextWeight::Semibold);
                text(
                    &mut PaintCtx::new(scene, tf, tr, theme),
                    &name,
                    &TextStyle::new(CATEGORY_NAME_SIZE).with_weight(TextWeight::Semibold),
                    c.text_dim,
                    Point::new(name_x, line_top(cy, f64::from(nh))),
                    None,
                );
            }
            // Caret + name toggles collapse; right-click (resolved against the
            // toggle / "+" hit in `on_right_click`) opens the category menu.
            // The toggle stops an `s1` short of the "+" so the two targets
            // never share an edge (HIG §7).
            let toggle = Rect::new(hrow.x0, y, plus.x0 - f64::from(sp.s1), y + hrow_h);
            if !renaming_cat {
                ChatApp::push_hit(hits, toggle, Hit::CategoryToggle(cat.id.clone()));
            }
            y += hrow_h + CHANNEL_ROW_GAP;

            if !cat.collapsed {
                y = self.draw_channel_group(
                    PaintCtx::new(scene, tf, tr, theme),
                    presence,
                    side,
                    Some(&cat.id),
                    y,
                    hits,
                );
            }
            y += CATEGORY_GAP;
        }
    }

    /// The self identity bar pinned to the sidebar bottom — the SINGLE user
    /// avatar lives here (mute + deafen toggles to its right).
    fn draw_sidebar_identity_bar(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        side: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        // The bar is as tall as its measured name + status lines need (never
        // shorter than the 1× bar); the tree above stops at the same edge.
        let bar = Rect::new(
            side.x0,
            side.y1 - identity_bar_h(tr, theme),
            side.x1,
            side.y1,
        );
        fill_rect(scene, tf, bar, c.neutral[0]);
        self.draw_identity_row(scene, tf, tr, theme, bar);
        // Deafen + mute toggles — only while in a call, where they write
        // through to the shared voice membership (`sync_local_voice`). Outside
        // a call there is no mic stream to gate, so the controls are hidden
        // rather than presented as toggling something they don't.
        if self.call.active {
            self.draw_call_controls(scene, tf, tr, theme, bar, hits);
        }
    }

    /// The avatar + name + status line on the left of the identity bar.
    fn draw_identity_row(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        bar: Rect,
    ) {
        let c = &theme.tokens.colors;
        let sp = &theme.tokens.spacing;
        let Some(me) = self.user(&self.me.id) else {
            return;
        };
        // The disc follows the text scale (the shared avatar sizes its
        // monogram through the renderer, which applies that scale), and the
        // name + status stack as one block centred on the bar, so at 200%
        // the two lines sit under each other instead of on top of each other.
        let r = IDENTITY_AVATAR_R * f64::from(theme.text_scale).max(1.0);
        let avatar_cx = bar.x0 + f64::from(sp.s3) + r;
        let cy = bar.center().y;
        draw_avatar(
            PaintCtx::new(scene, tf, tr, theme),
            Point::new(avatar_cx, cy),
            r,
            &me.initials,
            me.color_idx,
            Some(me.status),
        );
        let sub = if self.call.active {
            crate::i18n::t("chat-identity-in-voice")
        } else {
            crate::i18n::t("chat-identity-available")
        };
        let (_, name_h) = tr.measure(&me.name, IDENTITY_NAME_SIZE, None);
        let (_, sub_h) = tr.measure(&sub, IDENTITY_SUB_SIZE, None);
        let text_x = avatar_cx + r + f64::from(sp.s2);
        let name_y = line_top(cy, f64::from(name_h) + f64::from(sub_h));
        label(
            scene,
            tr,
            tf,
            &me.name,
            IDENTITY_NAME_SIZE,
            c.text_primary,
            text_x,
            name_y,
        );
        label(
            scene,
            tr,
            tf,
            &sub,
            IDENTITY_SUB_SIZE,
            c.text_dim,
            text_x,
            name_y + f64::from(name_h),
        );
    }

    /// The native microphone control on the right of the identity bar,
    /// shown only while a call is active.
    fn draw_call_controls(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        bar: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let sp = &theme.tokens.spacing;
        // Each toggle is a square sized from its measured glyph (never
        // smaller than the 1× square), centred on the bar: mute at the right
        // inset, deafen one `s1` left of it.
        let (_, gh) = tr.measure("🎧", IDENTITY_CTL_GLYPH, None);
        let ctl = (f64::from(gh) + 2.0 * f64::from(sp.s2)).max(IDENTITY_CTL_MIN);
        let cy = bar.center().y;
        let mute_x1 = bar.x1 - f64::from(sp.s3);
        let mute = Rect::new(mute_x1 - ctl, cy - ctl / 2.0, mute_x1, cy + ctl / 2.0);
        fill_round(
            scene,
            tf,
            mute,
            8.0,
            if self.call.muted {
                c.semantic.danger
            } else {
                c.lift(0.72)
            },
        );
        let mglyph = if self.call.muted { "🔇" } else { "🎤" };
        let (mw, mh) = tr.measure(mglyph, IDENTITY_CTL_GLYPH, None);
        text(
            &mut PaintCtx::new(scene, tf, tr, theme),
            mglyph,
            &TextStyle::new(IDENTITY_CTL_GLYPH),
            c.text_primary,
            Point::new(
                mute.center().x - f64::from(mw) / 2.0,
                mute.center().y - f64::from(mh) / 2.0,
            ),
            None,
        );
        ChatApp::push_hit(hits, mute, Hit::CallMute);
    }

    /// Draw the channels of one group (a category id, or `None` for the
    /// uncategorised group). Returns the new cursor `y`. Records per-row
    /// click + right-click context-menu hits, applies the active row swipe
    /// offset, and renders voice-channel presence (members + speaking) under
    /// each voice channel — Discord-style.
    fn draw_channel_group(
        &mut self,
        paint: PaintCtx<'_>,
        presence: &PresenceCtx,
        side: Rect,
        category: Option<&str>,
        mut y: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let cat_key = category.unwrap_or("");
        let channels: Vec<_> = self
            .category_channels(cat_key)
            .into_iter()
            .cloned()
            .collect();
        // Index for swipe parity: position among all ws_channels.
        let ws_channel_ids: Vec<_> = self.ws_channels().iter().map(|c| c.id.clone()).collect();
        for ch in &channels {
            // Swipe offset: shift this row left while it's being swiped, and
            // reveal a red delete affordance behind it.
            let swipe_dx = self
                .swipe
                .filter(|s| {
                    ws_channel_ids.get(s.channel_index).map(String::as_str) == Some(ch.id.as_str())
                })
                .map_or(0.0, |s| s.dx());
            y = self.draw_channel_row(
                PaintCtx::new(scene, tf, tr, theme),
                presence,
                ChannelRowInput {
                    side,
                    channel: ch,
                    y,
                    swipe_dx,
                },
                hits,
            );

            // Voice presence: list connected members under a voice channel.
            if ch.kind == ChannelKind::Voice {
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

    /// One channel row: swipe-to-delete affordance, active/live indicators,
    /// the glyph + name (or its inline rename editor), and the unread badge.
    /// Returns the cursor `y` for the row after this one.
    fn draw_channel_row(
        &mut self,
        paint: PaintCtx<'_>,
        presence: &PresenceCtx,
        input: ChannelRowInput<'_>,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let ChannelRowInput {
            side,
            channel: ch,
            y,
            swipe_dx,
        } = input;
        let row_h = channel_row_h(tr, theme);
        let row = Rect::new(
            side.x0 + 8.0 - swipe_dx,
            y,
            side.x1 - 8.0 - swipe_dx,
            y + row_h,
        );
        let is_active = ch.id == self.active_channel && ch.kind == ChannelKind::Text;
        let ctx = ChannelRowCtx {
            row,
            side,
            y,
            is_active,
            ch,
        };

        Self::draw_channel_row_swipe_delete(scene, tf, tr, theme, &ctx, swipe_dx);
        Self::draw_channel_row_glyph(scene, tf, tr, theme, &ctx);
        self.draw_channel_row_label(PaintCtx::new(scene, tf, tr, theme), presence, &ctx, hits);
        self.draw_channel_row_status(scene, tf, tr, theme, &ctx);

        // Click joins/opens; right-click (resolved against this row's
        // click hit in `on_right_click`) opens the channel context menu.
        let click_hit = if ch.kind == ChannelKind::Voice {
            Hit::VoiceChannel(ch.id.clone())
        } else {
            Hit::Channel(ch.id.clone())
        };
        let renaming = matches!(&self.rename, Some(MenuTarget::Channel(id)) if id == &ch.id);
        if !renaming {
            ChatApp::push_hit(hits, row, click_hit);
        }
        y + row_h + CHANNEL_ROW_GAP
    }

    /// The swipe-to-delete red background + trash glyph revealed behind a
    /// channel row while it is being swiped.
    fn draw_channel_row_swipe_delete(
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        ctx: &ChannelRowCtx<'_>,
        swipe_dx: f64,
    ) {
        if swipe_dx <= 1.0 {
            return;
        }
        let c = &theme.tokens.colors;
        fill_round(
            scene,
            tf,
            Rect::new(ctx.side.x0 + 8.0, ctx.row.y0, ctx.side.x1 - 8.0, ctx.row.y1),
            8.0,
            c.semantic.danger.with_alpha_factor(0.9),
        );
        let (_, gh) = tr.measure("🗑", CHANNEL_NAME_SIZE, None);
        label(
            scene,
            tr,
            tf,
            "🗑",
            CHANNEL_NAME_SIZE,
            c.on_surface(c.semantic.danger).ink,
            ctx.side.x1 - 30.0,
            line_top(ctx.row.center().y, f64::from(gh)),
        );
    }

    /// The active-row highlight + left accent bar, and the channel-kind
    /// glyph.
    fn draw_channel_row_glyph(
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        ctx: &ChannelRowCtx<'_>,
    ) {
        let c = &theme.tokens.colors;
        let cy = ctx.row.center().y;
        if ctx.is_active {
            fill_round(scene, tf, ctx.row, 8.0, c.lift(0.72));
            let bar = Rect::new(ctx.side.x0, cy - 10.0, ctx.side.x0 + 4.0, cy + 10.0);
            fill_round(scene, tf, bar, 2.0, c.accent[5]);
        }
        let glyph = ctx.ch.kind.glyph();
        let gcol = if ctx.is_active {
            c.accent[6]
        } else {
            c.text_dim
        };
        let (_, gh) = tr.measure(glyph, CHANNEL_NAME_SIZE, None);
        label(
            scene,
            tr,
            tf,
            glyph,
            CHANNEL_NAME_SIZE,
            gcol,
            ctx.row.x0 + 10.0,
            line_top(cy, f64::from(gh)),
        );
    }

    /// The channel's inline rename editor when it is the rename target,
    /// otherwise its (possibly truncated) name.
    fn draw_channel_row_label(
        &mut self,
        paint: PaintCtx<'_>,
        presence: &PresenceCtx,
        ctx: &ChannelRowCtx<'_>,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let c = &theme.tokens.colors;
        let renaming = matches!(&self.rename, Some(MenuTarget::Channel(id)) if id == &ctx.ch.id);
        if renaming {
            self.draw_inline_rename(
                PaintCtx::new(scene, tf, tr, theme),
                presence,
                ctx.row.x0 + 32.0,
                ctx.y + 6.0,
                ctx.row.width() - 80.0,
                hits,
            );
            return;
        }
        let name_col = if ctx.is_active || ctx.ch.unread > 0 {
            c.text_primary
        } else {
            c.text_dim
        };
        let weight = if ctx.ch.unread > 0 {
            TextWeight::Semibold
        } else {
            TextWeight::Regular
        };
        let name = truncate_to(tr, &ctx.ch.name, CHANNEL_NAME_SIZE, ctx.row.width() - 80.0);
        let (_, nh) = tr.measure_weighted(&name, CHANNEL_NAME_SIZE, None, weight);
        text(
            &mut PaintCtx::new(scene, tf, tr, theme),
            &name,
            &TextStyle::new(CHANNEL_NAME_SIZE).with_weight(weight),
            name_col,
            Point::new(
                ctx.row.x0 + 32.0,
                line_top(ctx.row.center().y, f64::from(nh)),
            ),
            None,
        );
    }

    /// The live-call presence dot, or the unread-count badge.
    fn draw_channel_row_status(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        ctx: &ChannelRowCtx<'_>,
    ) {
        let c = &theme.tokens.colors;
        let is_live = self.call.active && self.call.channel == ctx.ch.id;
        if is_live {
            fill_circle(
                scene,
                tf,
                Point::new(ctx.row.x1 - 16.0, ctx.row.center().y),
                4.0,
                c.semantic.success,
            );
        } else if ctx.ch.unread > 0 {
            // The badge is a capsule around its measured label, centred on the
            // row's centre line.
            let txt = ctx.ch.unread.to_string();
            let (bw, bh) = tr.measure(&txt, UNREAD_BADGE_TEXT, None);
            let badge_h =
                (f64::from(bh) + f64::from(theme.tokens.spacing.s1)).max(UNREAD_BADGE_MIN_H);
            let cy = ctx.row.center().y;
            let badge = Rect::new(
                ctx.row.x1 - f64::from(bw) - 22.0,
                cy - badge_h / 2.0,
                ctx.row.x1 - 8.0,
                cy + badge_h / 2.0,
            );
            fill_round(scene, tf, badge, badge_h / 2.0, c.accent[5]);
            label(
                scene,
                tr,
                tf,
                &txt,
                UNREAD_BADGE_TEXT,
                c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
                badge.x0 + 7.0,
                line_top(cy, f64::from(bh)),
            );
        }
    }

    /// Connected members listed under a voice channel, Discord-style: an
    /// avatar with a speaking ring, their name, and a mic/audio-only glyph.
    /// Returns the cursor `y` after the last member row.
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
        let c = &theme.tokens.colors;
        let row_h = voice_row_h(tr, theme);
        for stream in self.voice_in(channel_id) {
            let row = Rect::new(side.x0 + 36.0, y, side.x1 - 8.0, y + row_h);
            let radius = 9.0;
            draw_avatar(
                PaintCtx::new(scene, tf, tr, theme),
                Point::new(row.x0 + radius + 2.0, row.center().y),
                radius,
                "♫",
                0,
                None,
            );
            let name = truncate_to(
                tr,
                &self.user_name(&stream.user),
                VOICE_NAME_SIZE,
                row.width() - 40.0,
            );
            let (_, height) = tr.measure(&name, VOICE_NAME_SIZE, None);
            label(
                scene,
                tr,
                tf,
                &name,
                VOICE_NAME_SIZE,
                c.text_dim,
                row.x0 + 2.0 * radius + 8.0,
                line_top(row.center().y, f64::from(height)),
            );
            y += row_h + CHANNEL_ROW_GAP;
        }
        y
    }

    /// Draw the inline rename editor field (a bordered box + the working
    /// text + a caret), recording a confirm hit on it.
    fn draw_inline_rename(
        &mut self,
        paint: PaintCtx<'_>,
        presence: &PresenceCtx,
        x: f64,
        y: f64,
        w: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let c = &theme.tokens.colors;
        let box_r = Rect::new(x - 4.0, y - 3.0, x + w, y + 19.0);
        let focused = self.is_focused();
        fill_round(scene, tf, box_r, 5.0, c.neutral[2]);
        stroke_round(
            scene,
            tf,
            box_r,
            5.0,
            if focused {
                c.border_focus
            } else {
                c.border_standard
            },
            if focused { 1.4 } else { 1.0 },
        );
        self.rename_input
            .apply_layout_external(Rect::new(x, y, x + w - 8.0, y + 16.0));
        self.rename_input.set_focus(focused);
        self.rename_input.draw(scene, tf, tr, theme, presence);
        ChatApp::push_hit(hits, box_r, Hit::FocusRename);
    }

    // -------------------------------------------------------------- header
    pub(crate) fn draw_header(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let h = self.header_rect();
        fill_rect(scene, tf, h, c.surfaces.base);
        // Bar-role hairline, derived against the base it closes (HIG §9.5).
        hline(
            scene,
            tf,
            h.x0,
            h.x1,
            h.y1,
            ui::surface::edge_on(c.surfaces.base),
        );

        // When the left flap is hidden/overlay, leave room for the floating
        // hamburger that `draw_flap_toggles` paints at the header's left edge.
        let left_inset =
            if self.left_flap_open() && self.left_mode == crate::responsive::FlapMode::Docked {
                18.0
            } else {
                54.0
            };

        let ch = self.channel(&self.active_channel).cloned();
        let (name, topic, glyph) = match &ch {
            Some(ch) => {
                let topic = if self.inbox_open {
                    let source = self
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.id == ch.workspace)
                        .map_or("Tilo workspace", |workspace| workspace.name.as_str());
                    if ch.topic.is_empty() {
                        format!("From {source}")
                    } else {
                        format!("{source} · {}", ch.topic)
                    }
                } else {
                    ch.topic.clone()
                };
                (ch.name.clone(), topic, ch.kind.glyph())
            }
            None => (String::new(), String::new(), "#"),
        };

        // Right-aligned action buttons are placed first so the title + topic
        // know exactly where they must stop — they truncate before the button
        // cluster and can never overlap it, even on a very narrow header.
        let cluster_left = self.draw_header_action_buttons(scene, tf, tr, theme, h, hits);
        Self::draw_header_title(
            PaintCtx::new(scene, tf, tr, theme),
            h,
            left_inset,
            cluster_left,
            &name,
            &topic,
            glyph,
        );
    }

    /// The right-aligned call + members action buttons. Returns the x
    /// coordinate their cluster starts at, so the title/topic know where to
    /// truncate.
    fn draw_header_action_buttons(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        h: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let collaborators = self.has_other_collaborator();
        // Calls and the People roster matter after a workspace is shared. In
        // a private workspace the unadorned conversation header is the useful
        // resting state; invitation belongs to the workspace surface.
        if !collaborators {
            return h.x1;
        }
        let sp = &theme.tokens.spacing;
        let label_size = theme.tokens.typography.scale.sm;
        let gap = f64::from(sp.s2);
        let half_h = f64::from(sp.s8) / 2.0;
        let cy = h.center().y;
        // Both pills size themselves from their measured labels, so a larger
        // text scale widens the pill instead of pushing the caption past it.
        let call_label = if self.call.active {
            "● In call"
        } else {
            "📹 Call"
        };
        let (call_label_width, call_label_height) = tr.measure(call_label, label_size, None);
        let call_pad = f64::from(sp.s3);
        let call_w = f64::from(call_label_width) + 2.0 * call_pad;
        // call button (accent when a call is live).
        let call_x0 = h.x1 - f64::from(sp.s4) - call_w;
        let call_btn = Rect::new(call_x0, cy - half_h, call_x0 + call_w, cy + half_h);
        // Live call: the accent is its own contrast step (HIG §9.4), ink
        // resolved against it; at rest the pill is a lifted card with the
        // derived edge (HIG §9.5).
        let ctx = if self.call.active {
            fill_round(scene, tf, call_btn, 8.0, c.accent[5]);
            theme.on_surface(c.accent[5]).ink
        } else {
            ui::surface::card(scene, tf, call_btn, 8.0, c.lift(0.72));
            c.text_primary
        };
        label(
            scene,
            tr,
            tf,
            call_label,
            label_size,
            ctx,
            call_btn.x0 + call_pad,
            line_top(cy, f64::from(call_label_height)),
        );
        ChatApp::push_hit(hits, call_btn, Hit::StartCall);

        // members pill — toggles the right flap (members / thread).
        let mem_label = format!("👥 {}", self.users.len());
        let (mem_label_width, mem_label_height) = tr.measure(&mem_label, label_size, None);
        let mem_pad = f64::from(sp.s2);
        let mem_w = f64::from(mem_label_width) + 2.0 * mem_pad;
        let mem_x0 = call_x0 - gap - mem_w;
        let mem = Rect::new(mem_x0, cy - half_h, mem_x0 + mem_w, cy + half_h);
        let mem_on = self.right_flap_open();
        if mem_on {
            fill_round(scene, tf, mem, 8.0, c.accent[5].with_alpha_factor(0.22));
        } else {
            ui::surface::card(scene, tf, mem, 8.0, c.lift(0.35));
        }
        label(
            scene,
            tr,
            tf,
            &mem_label,
            label_size,
            if mem_on { c.accent[7] } else { c.text_dim },
            mem.x0 + mem_pad,
            line_top(cy, f64::from(mem_label_height)),
        );
        ChatApp::push_hit(hits, mem, Hit::ToggleRightFlap);
        mem_x0
    }

    /// The channel glyph + name + topic, truncated to stop before
    /// `cluster_left` so nothing runs under the action buttons.
    fn draw_header_title(
        paint: PaintCtx<'_>,
        h: Rect,
        left_inset: f64,
        cluster_left: f64,
        name: &str,
        topic: &str,
        glyph: &str,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let c = &theme.tokens.colors;
        let cy = h.center().y;
        label(
            scene,
            tr,
            tf,
            glyph,
            18.0,
            c.text_dim,
            h.x0 + left_inset,
            cy - 11.0,
        );
        let nx = h.x0 + left_inset + 22.0;
        let name_max = (cluster_left - 12.0 - nx).max(0.0);
        let name_t = truncate_to(tr, name, 17.0, name_max);
        let (nw, _) = text(
            &mut PaintCtx::new(scene, tf, tr, theme),
            &name_t,
            &TextStyle::new(17.0).with_weight(TextWeight::Bold),
            c.text_primary,
            Point::new(nx, cy - 11.0),
            None,
        );
        if !topic.is_empty() {
            let tx = nx + f64::from(nw) + 18.0;
            let avail = cluster_left - 12.0 - tx;
            if avail >= 48.0 {
                let topic_t = truncate_to(tr, topic, 13.0, avail);
                label(scene, tr, tf, &topic_t, 13.0, c.text_dim, tx, cy - 7.0);
            }
        }
    }

    /// Floating flap toggles: a hamburger at the header's left edge when the
    /// left flap is hidden/overlay, summoning the channel sidebar; the
    /// members pill in `draw_header` toggles the right flap.
    pub(crate) fn draw_flap_toggles(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        // A narrow thread is a pushed detail view with its own Back control;
        // do not leave a menu button floating on top of that content.
        if self.right_mode == crate::responsive::FlapMode::Overlay && self.thread_open.is_some() {
            return;
        }
        let c = &theme.tokens.colors;
        let docked_open =
            self.left_flap_open() && self.left_mode == crate::responsive::FlapMode::Docked;
        if !docked_open {
            let h = self.header_rect();
            let btn = Rect::new(
                h.x0 + 12.0,
                h.center().y - 16.0,
                h.x0 + 44.0,
                h.center().y + 16.0,
            );
            fill_round(scene, tf, btn, 8.0, c.lift(0.72));
            // Centre the glyph on its measured box so it stays inside the
            // button at every text scale.
            let glyph_size = theme.tokens.typography.scale.lg;
            let (gw, gh) = tr.measure("☰", glyph_size, None);
            label(
                scene,
                tr,
                tf,
                "☰",
                glyph_size,
                c.text_primary,
                btn.center().x - f64::from(gw) / 2.0,
                line_top(btn.center().y, f64::from(gh)),
            );
            ChatApp::push_hit(hits, btn, Hit::ToggleLeftFlap);
        }
    }

    // ------------------------------------------------------------ messages
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
        let text_x = area.x0 + 16.0 + AVATAR + 14.0;
        let text_w = (area.x1 - text_x - 18.0).max(80.0) as f32;
        let column = MessageColumn {
            area,
            text_x,
            text_width: text_w,
        };

        // Empty channel: a quiet start-of-conversation marker rather than a
        // blank void, so a fresh channel reads as ready, not broken.
        if self.messages.is_empty() {
            self.scroll_content = 0.0;
            self.scroll_viewport = area.height().max(0.0);
            self.scroll = 0.0;
            self.draw_empty_channel_marker(scene, tf, tr, theme, area);
            return;
        }

        let groups = model::group_messages(&self.messages);
        let divider_h = 36.0;
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
            if let Some(target) = self.revealed_message.as_deref()
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
        if !self.timeline_at_end {
            Self::draw_jump_to_latest(scene, tf, tr, theme, area, hits);
        }
    }

    /// A visible, keyboard-reachable escape hatch from history. It only
    /// changes local viewport state; the durable read watermark advances on a
    /// later sync once the live edge is actually visible.
    fn draw_jump_to_latest(
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let scale = f64::from(theme.text_scale).min(1.35);
        let label_text = "↓ Jump to latest";
        let (lw, _) = tr.measure(label_text, 12.5, None);
        let w = (f64::from(lw) + 28.0 * scale).max(152.0);
        let h = 36.0 * scale;
        let visual = Rect::new(
            area.x1 - w - 16.0,
            area.y1 - h - 14.0,
            area.x1 - 16.0,
            area.y1 - 14.0,
        );
        fill_round(
            scene,
            tf,
            visual.inflate(3.0, 3.0),
            12.0,
            theme.tokens.elevation.e3.color,
        );
        fill_round(scene, tf, visual, 10.0, c.accent[5]);
        let (tw, th) = tr.measure(label_text, 12.5, None);
        text(
            &mut PaintCtx::new(scene, tf, tr, theme),
            label_text,
            &TextStyle::new(12.5).with_weight(TextWeight::Semibold),
            c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
            Point::new(
                visual.center().x - f64::from(tw) / 2.0,
                visual.center().y - f64::from(th) / 2.0,
            ),
            None,
        );
        // The visual remains compact; the 44px target meets touch and switch
        // access guidance without crowding the message column.
        ChatApp::push_hit(
            hits,
            Rect::new(
                visual.x0,
                visual.center().y - 22.0,
                visual.x1,
                visual.center().y + 22.0,
            ),
            Hit::JumpToLatest,
        );
    }

    /// The quiet "start of #channel" marker shown in place of the timeline
    /// when a channel has no messages yet.
    fn draw_empty_channel_marker(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        let ch_name = self
            .channel(&self.active_channel)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let cx = area.center().x;
        let cy = area.y0 + (area.height() * 0.5).min(220.0);
        fill_circle(scene, tf, Point::new(cx, cy), 30.0, c.lift(0.35));
        let (gw, gh) = tr.measure_weighted("#", 30.0, None, TextWeight::Bold);
        text(
            &mut PaintCtx::new(scene, tf, tr, theme),
            "#",
            &TextStyle::new(30.0).with_weight(TextWeight::Bold),
            c.text_dim,
            Point::new(cx - f64::from(gw) / 2.0, cy - f64::from(gh) / 2.0),
            None,
        );
        let title = format!("#{ch_name}");
        let (tw, _) = tr.measure_weighted(&title, 20.0, None, TextWeight::Bold);
        text(
            &mut PaintCtx::new(scene, tf, tr, theme),
            &title,
            &TextStyle::new(20.0).with_weight(TextWeight::Bold),
            c.text_primary,
            Point::new(cx - f64::from(tw) / 2.0, cy + 46.0),
            None,
        );
        let sub = if self.can_participate() {
            format!("This is the start of {title}. Write the first message to begin.")
        } else {
            format!("There are no messages in {title} yet.")
        };
        let (sw, _) = tr.measure(&sub, 13.0, None);
        label(
            scene,
            tr,
            tf,
            &sub,
            13.0,
            c.text_dim,
            cx - f64::from(sw) / 2.0,
            cy + 76.0,
        );
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
                let show_header = gi == 0;
                let rm = self.measure_row(tr, theme, m, show_header, text_w);
                total += rm.height();
                rows.push(rm);
            }
        }
        (rows, total)
    }

    /// The "Today"/date divider above the first row. Returns the cursor `y`
    /// for the first message row beneath it.
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
        let c = &theme.tokens.colors;
        let dl = self.messages.first().map_or_else(
            || "Today".into(),
            |m| model::day_label(m.ts, crate::doc::NOW),
        );
        let (dw, _) = tr.measure(&dl, 11.5, None);
        let dcy = y + divider_h / 2.0;
        // Rules derived against the message area's base (HIG §9.5).
        let rule = ui::surface::edge_on(c.surfaces.base);
        hline(
            scene,
            tf,
            area.x0 + 24.0,
            area.center().x - f64::from(dw) / 2.0 - 14.0,
            dcy,
            rule,
        );
        hline(
            scene,
            tf,
            area.center().x + f64::from(dw) / 2.0 + 14.0,
            area.x1 - 24.0,
            dcy,
            rule,
        );
        label(
            scene,
            tr,
            tf,
            &dl,
            11.5,
            c.text_dim,
            area.center().x - f64::from(dw) / 2.0,
            dcy - 7.0,
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
        let scale = f64::from(theme.text_scale);
        let editing = self.editing.as_ref() == Some(&m.id);
        // The editor measures its live working text so the timeline reflows as
        // you type; a static message measures its stored body.
        let body_h = if editing {
            f64::from(tr.measure(&self.edit_input.value(), 15.0, Some(text_w)).1).max(19.0)
        } else if m.text.is_empty() {
            0.0
        } else {
            // Measured by the same view that paints it, so a row never clips
            // its own last line.
            self.layout_body(m, BODY_SIZE, Point::ZERO, text_w, theme, tr)
        };
        let att_h = m.attachment.as_ref().map_or(0.0, |attachment| {
            attachment_flow_height(attachment, f64::from(text_w), scale)
        });
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
            reaction_chip_h(tr, theme) + REACTION_STRIP_GAP
        };
        // While editing, the "(edited)" marker is suppressed in favour of the
        // editor's own controls row.
        let edited_h = if m.edited && !editing {
            edited_row_h(tr, theme)
        } else {
            0.0
        };
        let thread_h = if model::reply_count(&self.messages, &m.id) > 0 {
            thread_bar_h(tr, theme) + f64::from(theme.tokens.spacing.s2)
        } else {
            0.0
        };
        let edit_ctrl_h = if editing { 30.0 * scale } else { 0.0 };
        RowM {
            idx_id: m.id.clone(),
            show_header,
            header_h,
            body_h,
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
        if self.revealed_message.as_deref() == Some(m.id.as_str()) {
            fill_round(
                scene,
                tf,
                Rect::new(area.x0 + 4.0, *y, area.x1 - 4.0, *y + rm.height()),
                8.0,
                theme.tokens.colors.lift(0.45),
            );
        }
        let top_gap = if rm.show_header { 14.0 } else { 3.0 };
        *y += top_gap;
        // Top of the header + body region — the right-click target for your
        // own messages (recorded below, beneath the reaction / thread hits).
        let content_top = *y;

        if rm.show_header {
            self.draw_row_header(PaintCtx::new(scene, tf, tr, theme), column, &m, y);
        }

        self.draw_row_body(
            PaintCtx::new(scene, tf, tr, theme),
            presence,
            MessageRowRef {
                measurement: rm,
                message: &m,
            },
            MessageBodyLayout {
                column,
                content_top,
            },
            y,
            hits,
        );
        self.draw_row_footer(PaintCtx::new(scene, tf, tr, theme), rm, &m, column, y, hits);
    }

    /// A message group's header: avatar, author name (in their palette
    /// colour), and the timestamp. Advances `*y` past it.
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
        let c = &theme.tokens.colors;
        let u = self.user(&m.author).cloned();
        let (initials, cidx, status) = u.as_ref().map_or_else(
            // An agent writes no user row; its name is on record.
            || {
                if self.agent_authors.contains_key(&m.author) {
                    (
                        self.initials_for(&m.author),
                        crate::identity::color_index_for(m.author.as_bytes()),
                        None,
                    )
                } else {
                    ("?".into(), 0, None)
                }
            },
            |u| (u.initials.clone(), u.color_idx, Some(u.status)),
        );
        draw_avatar(
            PaintCtx::new(scene, tf, tr, theme),
            Point::new(area.x0 + 16.0 + AVATAR / 2.0, *y + AVATAR / 2.0),
            AVATAR / 2.0,
            &initials,
            cidx,
            status,
        );
        let name = self.user_name(&m.author);
        let (nw, nh) = text(
            &mut PaintCtx::new(scene, tf, tr, theme),
            &name,
            &TextStyle::new(NAME_SIZE).with_weight(TextWeight::Semibold),
            palette_color(theme, cidx),
            Point::new(text_x, *y),
            None,
        );
        // The timestamp's line box is bottom-aligned to the name's, which
        // keeps their baselines together at any text scale.
        let clock = model::clock(m.ts);
        let (_, th) = tr.measure(&clock, TIME_SIZE, None);
        label(
            scene,
            tr,
            tf,
            &clock,
            TIME_SIZE,
            c.text_dim,
            text_x + f64::from(nw) + f64::from(theme.tokens.spacing.s2),
            *y + f64::from(nh) - f64::from(th),
        );
        *y += header_row_h(tr, theme);
    }

    /// The editor (while editing) or the plain/mention-attributed body text,
    /// plus the right-click hit region over the header+body block for your
    /// own messages. Advances `*y` past the body.
    fn draw_row_body(
        &mut self,
        paint: PaintCtx<'_>,
        presence: &PresenceCtx,
        row: MessageRowRef<'_>,
        layout: MessageBodyLayout,
        y: &mut f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let MessageRowRef {
            measurement: rm,
            message: m,
        } = row;
        let MessageBodyLayout {
            column:
                MessageColumn {
                    area,
                    text_x,
                    text_width: text_w,
                },
            content_top,
        } = layout;
        if rm.editing {
            self.draw_message_editor(
                PaintCtx::new(scene, tf, tr, theme),
                presence,
                layout.column,
                rm,
                y,
                hits,
            );
            return;
        }
        if !m.text.is_empty() {
            // The body as written: paragraphs, formatting, and mentions in
            // their participants' colours, drawn by the editor that wrote it.
            self.layout_body(m, BODY_SIZE, Point::new(text_x, *y), text_w, theme, tr);
            self.draw_body(m, BODY_SIZE, &mut PaintCtx::new(scene, tf, tr, theme));
            *y += rm.body_h + 2.0;
        }
        // Right-click your own message (header + body) opens the edit menu.
        // Recorded now, before the reaction / thread hits so those still
        // win in their own sub-regions.
        if self.is_me(&m.author) {
            ChatApp::push_hit(
                hits,
                Rect::new(area.x0, content_top, area.x1, *y),
                Hit::MessageMenu { msg: m.id.clone() },
            );
        }
    }

    /// The "(edited)" label, attachment, reactions, and thread affordance
    /// below the body — each only when present. Advances `*y` past all of
    /// them, plus the row's trailing gap.
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
        let c = &theme.tokens.colors;
        if m.edited && !rm.editing {
            let edited_label = "(edited) · view history";
            let (ew, eh) = tr.measure(edited_label, TIME_SIZE, None);
            let edited_hit = Rect::new(
                text_x,
                *y,
                text_x + f64::from(ew) + f64::from(theme.tokens.spacing.s1),
                *y + f64::from(eh),
            );
            label(
                scene,
                tr,
                tf,
                edited_label,
                TIME_SIZE,
                c.text_dim,
                text_x,
                *y,
            );
            ChatApp::push_hit(
                hits,
                edited_hit,
                Hit::EditHistory {
                    msg: m.id.clone(),
                    anchor: Point::new(text_x, *y + 18.0),
                },
            );
            *y += rm.edited_h;
        }

        if let Some(att) = &m.attachment {
            self.draw_attachment(
                PaintCtx::new(scene, tf, tr, theme),
                att,
                AttachmentLayout::new(text_x, *y, text_w),
            );
            *y += rm.att_h;
        }

        // Empty reaction strip still gets an add affordance on header rows —
        // handled elsewhere, so there is nothing to draw when it's empty.
        if !m.reactions.is_empty() {
            self.draw_reactions(PaintCtx::new(scene, tf, tr, theme), m, text_x, *y, hits);
            *y += rm.react_h;
        }

        if rm.thread_h > 0.0 {
            self.draw_thread_affordance(PaintCtx::new(scene, tf, tr, theme), m, column, *y, hits);
            *y += rm.thread_h;
        }
        *y += 4.0;
    }

    /// Render the inline message editor in place of a message body: a focused
    /// input box carrying the live working text + caret, and a controls row
    /// (a keyboard hint plus Cancel / Save). Advances `*y` past the whole
    /// editor. The wrap width matches [`Self::measure_row`] so the timeline
    /// height stays exact as the text grows.
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
        let c = &theme.tokens.colors;
        let box_h = rm.body_h + EDIT_BOX_PAD;
        let er = Rect::new(
            text_x - 8.0,
            *y,
            text_x + f64::from(text_w) + 8.0,
            *y + box_h,
        );
        let focused = self.is_focused();
        fill_round(scene, tf, er, 10.0, c.neutral[2]);
        stroke_round(
            scene,
            tf,
            er,
            10.0,
            if focused {
                c.border_focus
            } else {
                c.border_standard
            },
            if focused { 1.6 } else { 1.0 },
        );
        self.edit_input.set_placeholder("Edit message…");
        self.edit_input.set_mention_paint(self.mention_paint(theme));
        self.edit_input.layout(
            Rect::new(
                text_x - 4.0,
                *y + EDIT_BOX_PAD / 2.0,
                text_x + f64::from(text_w) + 4.0,
                *y + box_h - EDIT_BOX_PAD / 2.0,
            ),
            theme,
            tr,
        );
        self.edit_input.set_focus(focused);
        self.edit_input.draw(scene, tf, tr, theme, presence);
        ChatApp::push_hit(hits, er, Hit::FocusEdit);
        *y += box_h;

        // Controls row: keyboard hint on the left, Cancel + Save on the right.
        let cy = *y + 2.0;
        label(
            scene,
            tr,
            tf,
            "Enter to save · Esc to cancel",
            11.0,
            c.text_dim,
            text_x,
            cy + 4.0,
        );
        let save = Rect::new(er.x1 - 56.0, cy, er.x1, cy + 24.0);
        fill_round(scene, tf, save, 7.0, c.accent[5]);
        let (sw, sh) = tr.measure_weighted("Save", 12.5, None, TextWeight::Semibold);
        text(
            &mut PaintCtx::new(scene, tf, tr, theme),
            "Save",
            &TextStyle::new(12.5).with_weight(TextWeight::Semibold),
            c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
            Point::new(
                save.center().x - f64::from(sw) / 2.0,
                save.center().y - f64::from(sh) / 2.0,
            ),
            None,
        );
        ChatApp::push_hit(hits, save, Hit::EditSave);
        let cancel = Rect::new(save.x0 - 8.0 - 62.0, cy, save.x0 - 8.0, cy + 24.0);
        // The shared Neutral button face (HIG §8).
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &ButtonFace::new(cancel, "Cancel")
                .text_size(12.5)
                .weight(TextWeight::Regular)
                .radius(7.0),
        );
        ChatApp::push_hit(hits, cancel, Hit::EditCancel);
        *y += rm.edit_ctrl_h;
    }

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
        let c = &theme.tokens.colors;
        let mut x = text_x;
        let chip_h = reaction_chip_h(tr, theme);
        let chip_r = chip_h / 2.0;
        for r in &m.reactions {
            let mine = r.reacted_by(&self.me.id);
            let lbl = format!("{}  {}", r.emoji, r.count());
            let (lw, lh) = tr.measure(&lbl, REACTION_SIZE, None);
            let chip = Rect::new(x, y, x + f64::from(lw) + 20.0, y + chip_h);
            // My reaction: accent wash with the accent ring; others: a lifted
            // chip with its derived rest edge (HIG §9.5).
            if mine {
                fill_round(scene, tf, chip, chip_r, c.accent[5].with_alpha_factor(0.22));
                stroke_round(scene, tf, chip, chip_r, c.accent[5], 1.0);
            } else {
                ui::surface::card(scene, tf, chip, chip_r, c.lift(0.35));
            }
            label(
                scene,
                tr,
                tf,
                &lbl,
                REACTION_SIZE,
                if mine { c.accent[7] } else { c.text_primary },
                chip.x0 + 10.0,
                line_top(chip.center().y, f64::from(lh)),
            );
            ChatApp::push_hit(
                hits,
                chip,
                Hit::Reaction {
                    msg: m.id.clone(),
                    emoji: r.emoji.clone(),
                },
            );
            // Sibling chips sit an `s2` apart (HIG §7b).
            x = chip.x1 + f64::from(theme.tokens.spacing.s2);
        }
        // add-reaction affordance
        let add = Rect::new(x, y, x + 34.0, y + chip_h);
        ui::surface::card(scene, tf, add, chip_r, c.lift(0.35));
        let (aw, ah) = tr.measure("☺", 14.0, None);
        text(
            &mut PaintCtx::new(scene, tf, tr, theme),
            "☺",
            &TextStyle::new(14.0),
            c.text_dim,
            Point::new(
                add.center().x - f64::from(aw) / 2.0,
                add.center().y - f64::from(ah) / 2.0,
            ),
            None,
        );
        ChatApp::push_hit(
            hits,
            add,
            Hit::AddReaction {
                msg: m.id.clone(),
                anchor: Point::new(add.x0, y + chip_h + 4.0),
            },
        );
    }

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
        let MessageColumn {
            text_x,
            text_width: text_w,
            ..
        } = column;
        let c = &theme.tokens.colors;
        let count = model::reply_count(&self.messages, &m.id);
        let parts = model::thread_participants(&self.messages, &m.id);
        let bar_h = thread_bar_h(tr, theme);
        let bar = Rect::new(text_x, y, text_x + f64::from(text_w).min(360.0), y + bar_h);
        let open = self.thread_open.as_deref() == Some(m.id.as_str());
        fill_round(
            scene,
            tf,
            bar,
            10.0,
            if open {
                c.accent[5].with_alpha_factor(0.14)
            } else {
                c.lift(0.35)
            },
        );
        if open {
            stroke_round(
                scene,
                tf,
                bar,
                10.0,
                c.accent[5].with_alpha_factor(0.5),
                1.0,
            );
        }
        let mut ax = bar.x0 + 14.0;
        for pid in parts.iter().take(3) {
            if let Some(u) = self.user(pid) {
                fill_circle(
                    scene,
                    tf,
                    Point::new(ax + 11.0, bar.center().y),
                    13.0,
                    c.surfaces.raised,
                );
                draw_avatar(
                    PaintCtx::new(scene, tf, tr, theme),
                    Point::new(ax + 11.0, bar.center().y),
                    11.0,
                    &u.initials,
                    u.color_idx,
                    None,
                );
            }
            ax += 17.0;
        }
        let txt = format!("{count} {}", if count == 1 { "reply" } else { "replies" });
        let (_, lh) = tr.measure(&txt, THREAD_LABEL_SIZE, None);
        label(
            scene,
            tr,
            tf,
            &txt,
            THREAD_LABEL_SIZE,
            c.accent[6],
            ax + 8.0,
            line_top(bar.center().y, f64::from(lh)),
        );
        let last_ts = self
            .messages
            .iter()
            .filter(|r| r.parent.as_deref() == Some(m.id.as_str()))
            .map(|r| r.ts)
            .max();
        if let Some(ts) = last_ts {
            let s = format!("last reply {}", model::clock(ts));
            let sx = bar.x1 - f64::from(tr.measure(&s, 11.0, None).0) - 14.0;
            label(
                scene,
                tr,
                tf,
                &s,
                11.0,
                c.text_dim,
                sx,
                bar.center().y - 6.0,
            );
        }
        ChatApp::push_hit(hits, bar, Hit::OpenThread(m.id.clone()));
    }

    fn draw_attachment(&self, mut paint: PaintCtx<'_>, att: &Attachment, layout: AttachmentLayout) {
        // Permission boundary: an embed this peer hasn't been granted access to
        // renders as a locked card and never requests its shared bytes. Same
        // footprint as the real embed so the timeline layout is
        // identical whether granted or gated.
        if !self.embed_allowed(att.blob()) {
            Self::draw_gated_embed(paint, att, layout);
            return;
        }
        match att {
            Attachment::Image { name, blob, w, h } => {
                self.draw_image_attachment(paint.reborrow(), name, blob, (*w, *h), layout);
            }
            Attachment::Video {
                name, w, h, size, ..
            } => {
                Self::draw_video_attachment(paint, name, (*w, *h), *size, layout);
            }
            Attachment::File { name, size, .. } => {
                Self::draw_file_attachment(paint, name, *size, layout);
            }
        }
    }

    /// The image attachment card: a decoded thumbnail (or a neutral
    /// placeholder while it hydrates), a caption scrim with the filename,
    /// and its pixel dimensions beneath.
    fn draw_image_attachment(
        &self,
        paint: PaintCtx<'_>,
        name: &str,
        blob: &str,
        dimensions: (u32, u32),
        layout: AttachmentLayout,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let (img_w, img_h) = dimensions;
        let Point { x, y } = layout.origin;
        let text_w = layout.max_width;
        let c = &theme.tokens.colors;
        let scale = f64::from(theme.text_scale);
        let (tw, th) = image_thumb_size(img_w, img_h, f64::from(text_w));
        let rect = Rect::new(x, y, x + tw, y + th);
        // Real decoded bytes from the shared background image cache. `get`
        // schedules the load the first time a blob is seen and returns the decoded
        // image on a later frame; until then we show a neutral tile.
        if let Some(img) = self.images.get(&crate::images::blob_key(blob)) {
            ui::image_store::paint_image_cover(scene, tf, rect, &img, 12.0);
        } else {
            fill_round(scene, tf, rect, 12.0, c.surfaces.raised);
            let dot = Point::new(rect.center().x, rect.center().y - 4.0);
            fill_circle(scene, tf, dot, 12.0, c.lift(0.72));
            let (gw, gh) = tr.measure("🖼", 18.0, None);
            text(
                &mut PaintCtx::new(scene, tf, tr, theme),
                "🖼",
                &TextStyle::new(18.0),
                c.text_dim,
                Point::new(dot.x - f64::from(gw) / 2.0, dot.y - f64::from(gh) / 2.0),
                None,
            );
            let status = if blob.starts_with(crate::images::DEMO_SCHEME) {
                "Loading image…"
            } else {
                match self.content.state(blob) {
                    tilo_content::ContentState::Pending => "Loading shared image…",
                    tilo_content::ContentState::Unavailable => "Shared image unavailable",
                    tilo_content::ContentState::Resolved => "Decoding image…",
                }
            };
            let (sw, _) = tr.measure(status, 11.0, None);
            label(
                scene,
                tr,
                tf,
                status,
                11.0,
                c.text_dim,
                rect.center().x - f64::from(sw) / 2.0,
                rect.center().y + 28.0,
            );
        }
        // Caption gradient scrim + filename, then a framing stroke.
        let caption = ui::paint::with_alpha(c.surfaces.scrim, 120);
        fill_round(
            scene,
            tf,
            Rect::new(rect.x0, rect.y1 - 30.0 * scale, rect.x1, rect.y1),
            12.0,
            caption,
        );
        stroke_round(
            scene,
            tf,
            rect,
            12.0,
            ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
            1.0,
        );
        label(
            scene,
            tr,
            tf,
            &format!("🖼  {name}"),
            12.0,
            c.on_surface(c.surfaces.scrim).ink,
            rect.x0 + 12.0 * scale.min(1.2),
            rect.y1 - 22.0 * scale,
        );
        let dim = format!("{img_w}×{img_h}");
        label(
            scene,
            tr,
            tf,
            &dim,
            11.0,
            c.text_dim,
            rect.x0,
            rect.y1 + 4.0 * scale,
        );
    }

    /// The video attachment card: a poster surface with a centred play
    /// glyph, a caption scrim with the filename, and dimensions + size
    /// metadata beneath.
    fn draw_video_attachment(
        paint: PaintCtx<'_>,
        name: &str,
        dimensions: (u32, u32),
        size: u64,
        layout: AttachmentLayout,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let (vid_w, vid_h) = dimensions;
        let Point { x, y } = layout.origin;
        let text_w = layout.max_width;
        let c = &theme.tokens.colors;
        let scale = f64::from(theme.text_scale);
        let (tw, th) = video_card_size(vid_w, vid_h, f64::from(text_w));
        let rect = Rect::new(x, y, x + tw, y + th);
        // A dark poster surface with a play affordance. A real decoded
        // frame needs the media engine's decoder (live-media only); here
        // the card references the projected file and hydrates its bytes
        // on demand — honest, not a fabricated still.
        fill_round(scene, tf, rect, 12.0, c.neutral[2]);
        fill_round(
            scene,
            tf,
            Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + th * 0.5),
            12.0,
            c.lift(0.35),
        );
        // Centred play button.
        let cc = rect.center();
        fill_circle(
            scene,
            tf,
            cc,
            25.0 * scale.min(1.3),
            ui::paint::with_alpha(c.surfaces.scrim, 150),
        );
        fill_circle(scene, tf, cc, 24.0 * scale.min(1.3), c.lift(0.72));
        let (pw, ph) = tr.measure("▶", 20.0, None);
        text(
            &mut PaintCtx::new(scene, tf, tr, theme),
            "▶",
            &TextStyle::new(20.0),
            c.on_surface(c.lift(0.72)).ink,
            Point::new(cc.x - f64::from(pw) / 2.0 + 2.0, cc.y - f64::from(ph) / 2.0),
            None,
        );
        // Caption scrim + filename, then a framing stroke.
        fill_round(
            scene,
            tf,
            Rect::new(rect.x0, rect.y1 - 30.0 * scale, rect.x1, rect.y1),
            12.0,
            ui::paint::with_alpha(c.surfaces.scrim, 120),
        );
        stroke_round(
            scene,
            tf,
            rect,
            12.0,
            ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
            1.0,
        );
        label(
            scene,
            tr,
            tf,
            &format!("🎬  {name}"),
            12.0,
            c.on_surface(c.surfaces.scrim).ink,
            rect.x0 + 12.0 * scale.min(1.2),
            rect.y1 - 22.0 * scale,
        );
        let meta = if vid_w > 0 && vid_h > 0 {
            format!("{vid_w}×{vid_h} · {}", format_bytes(size))
        } else {
            format!("video · {}", format_bytes(size))
        };
        label(
            scene,
            tr,
            tf,
            &meta,
            11.0,
            c.text_dim,
            rect.x0,
            rect.y1 + 4.0 * scale,
        );
    }

    /// The generic file attachment chip: an icon tile, filename, and human
    /// metadata (kind/size) beneath it.
    fn draw_file_attachment(paint: PaintCtx<'_>, name: &str, size: u64, layout: AttachmentLayout) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let Point { x, y } = layout.origin;
        let text_w = layout.max_width;
        let scale = f64::from(theme.text_scale.max(1.0));
        let c = &theme.tokens.colors;
        let chip = Rect::new(x, y, x + f64::from(text_w).min(360.0), y + 58.0);
        ui::surface::card(scene, tf, chip, 10.0, c.surfaces.raised);
        let icon = Rect::new(
            chip.x0 + 10.0 * scale.min(1.2),
            chip.y0 + 11.0 * scale,
            chip.x0 + 46.0 * scale.min(1.2),
            chip.y1 - 11.0 * scale,
        );
        fill_round(scene, tf, icon, 8.0, c.accent[5]);
        let (iw, ih) = tr.measure("📄", 16.0, None);
        text(
            &mut PaintCtx::new(scene, tf, tr, theme),
            "📄",
            &TextStyle::new(16.0),
            c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
            Point::new(
                icon.center().x - f64::from(iw) / 2.0,
                icon.center().y - f64::from(ih) / 2.0,
            ),
            None,
        );
        let fname = truncate_to(tr, name, 13.5, chip.width() - 70.0);
        label(
            scene,
            tr,
            tf,
            &fname,
            13.5,
            c.text_primary,
            chip.x0 + 56.0 * scale.min(1.2),
            chip.center().y - 14.0 * scale,
        );
        let meta = file_chip_meta(name, size);
        label(
            scene,
            tr,
            tf,
            &meta,
            11.0,
            c.text_dim,
            chip.x0 + 56.0 * scale.min(1.2),
            chip.center().y + 2.0 * scale,
        );
    }

    /// Render an embed this peer isn't permitted to open: a neutral locked card
    /// with the filename but no hydrated content. Sized to match the granted
    /// embed's footprint so the timeline layout is identical either way.
    fn draw_gated_embed(paint: PaintCtx<'_>, att: &Attachment, layout: AttachmentLayout) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let Point { x, y } = layout.origin;
        let text_w = layout.max_width;
        let c = &theme.tokens.colors;
        let scale = f64::from(theme.text_scale);
        let (card_w, card_h) = attachment_card_size(att, f64::from(text_w), scale);
        let rect = Rect::new(x, y, x + card_w, y + card_h);
        ui::surface::card(scene, tf, rect, 12.0, c.neutral[1]);
        let cx = rect.center().x;
        let cy = rect.center().y
            - if card_h > 64.0 {
                8.0 * scale
            } else {
                6.0 * scale
            };
        let (lw, lh) = tr.measure("🔒", 18.0, None);
        text(
            &mut PaintCtx::new(scene, tf, tr, theme),
            "🔒",
            &TextStyle::new(18.0),
            c.text_dim,
            Point::new(cx - f64::from(lw) / 2.0, cy - f64::from(lh) / 2.0),
            None,
        );
        let msg = "Access not granted";
        let (mw, _) = tr.measure(msg, 12.0, None);
        label(
            scene,
            tr,
            tf,
            msg,
            12.0,
            c.text_dim,
            cx - f64::from(mw) / 2.0,
            cy + 16.0 * scale,
        );
        let fname = truncate_to(tr, att.name(), 11.0, rect.width() - 24.0);
        let (fw, _) = tr.measure(&fname, 11.0, None);
        label(
            scene,
            tr,
            tf,
            &fname,
            11.0,
            c.text_dim,
            cx - f64::from(fw) / 2.0,
            cy + 40.0 * scale,
        );
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
        let rect = self.composer_rect(tr);
        if !self.can_participate() {
            self.composer_main.set_focus(false);
            Self::draw_read_only_composer(scene, tf, tr, theme, rect);
            return;
        }
        // While an inline message edit is in progress the caret lives in the
        // editor, not the composer.
        let focused = self.is_focused()
            && matches!(self.focus, crate::app::Focus::Main)
            && self.editing.is_none();
        let draft = self.composer_main.value();
        let send_enabled = !draft.trim().is_empty();
        let inner = Self::draw_input_box(
            PaintCtx::new(scene, tf, tr, theme),
            rect,
            &draft,
            send_enabled,
            focused,
            Hit::FocusMain,
            true,
            hits,
        );
        self.composer_main
            .set_mention_paint(self.mention_paint(theme));
        self.composer_main.layout(inner, theme, tr);
        self.composer_main.set_focus(focused);
        self.composer_main.draw(scene, tf, tr, theme, presence);
        let key = ChatApp::draft_presence_key(&self.active_channel);
        Self::draw_draft_presence(&self.composer_main, &key, scene, tf, tr, theme, presence);
    }

    /// Collaborators' carets and selections in a shared draft, drawn with the
    /// composer's own caret/selection geometry (pixel-identical, recoloured).
    fn draw_draft_presence(
        composer: &crate::composer::DraftComposer,
        key: &ui::presence::PresenceKey,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let len = composer.value().len();
        if let Some(list) = presence.selections.get(key) {
            for (user, range) in list {
                let (start, end) = (range.start.min(len), range.end.min(len));
                for rect in composer.selection_rects(theme, start, end) {
                    presence.draw_peer_selection(scene, theme, tf, user, rect);
                }
            }
        }
        if let Some(list) = presence.carets.get(key) {
            for (user, caret) in list {
                let Some(rect) = composer.caret_rect(theme, (*caret).min(len)) else {
                    continue;
                };
                let name = presence.display_name(user);
                presence.draw_peer_caret_labeled(scene, theme, tr, tf, user, &name, rect);
            }
        }
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
        let c = &theme.tokens.colors;
        ui::surface::card(scene, tf, rect, 14.0, c.lift(0.35));
        label(
            scene,
            tr,
            tf,
            "🔒 View-only access to this workspace",
            14.0,
            c.text_dim,
            rect.x0 + 16.0,
            rect.center().y - 8.0,
        );
    }

    /// Draw the composer box chrome — the rounded field, the attach `+`, the
    /// emoji + send buttons — and record their hit regions. Returns the inner
    /// rect the caller lays the composer [`TextInput`] into; the widget owns
    /// the text, caret, selection, and placeholder inside it.
    ///
    /// Every button is the same `spacing.s8` tall and sits on the box's centre
    /// line. The returned band is sized from the measured `draft` (one line
    /// when empty) and centred on that same line, so a short draft, its caret
    /// and the placeholder line up with the buttons instead of hugging the
    /// top edge; a long draft fills the box, which itself grows with the text.
    pub(crate) fn draw_input_box(
        paint: PaintCtx<'_>,
        rect: Rect,
        draft: &str,
        send_enabled: bool,
        focused: bool,
        focus_hit: Hit,
        attach: bool,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> Rect {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let c = &theme.tokens.colors;
        let sp = &theme.tokens.spacing;
        let btn = f64::from(sp.s8);
        let half_btn = btn / 2.0;
        let cy = rect.center().y;
        // The field's rest edge is derived from its fill (HIG §9.5); focus
        // keeps the focus ring.
        fill_round(scene, tf, rect, 14.0, c.neutral[2]);
        stroke_round(
            scene,
            tf,
            rect,
            14.0,
            if focused {
                c.border_focus
            } else {
                ui::surface::edge_on(c.neutral[2])
            },
            if focused { 1.6 } else { 1.0 },
        );
        ChatApp::push_hit(hits, rect, focus_hit);

        let mut tx = rect.x0 + f64::from(sp.s4);
        if attach {
            let plus_x0 = rect.x0 + f64::from(sp.s3);
            let plus = Rect::new(plus_x0, cy - half_btn, plus_x0 + btn, cy + half_btn);
            // The shared Neutral button face (HIG §8).
            Button::paint(
                scene,
                tf,
                tr,
                theme,
                &ButtonFace::new(plus, "+")
                    .text_size(20.0)
                    .weight(TextWeight::Regular)
                    .radius(8.0),
            );
            ChatApp::push_hit(hits, plus, Hit::Attach);
            tx = plus.x1 + f64::from(sp.s3);
        }

        // right cluster: emoji + send
        let send_x1 = rect.x1 - f64::from(sp.s3);
        let send = Rect::new(send_x1 - SEND_BTN_W, cy - half_btn, send_x1, cy + half_btn);
        // The shared button face (HIG §8): Primary while there is something
        // to send, the toolkit's disabled face otherwise.
        let send_face = ButtonFace::new(send, "➤")
            .text_size(16.0)
            .weight(TextWeight::Regular)
            .radius(9.0);
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &if send_enabled {
                send_face.primary()
            } else {
                send_face.disabled(true)
            },
        );
        ChatApp::push_hit(hits, send, Hit::Send);

        let emoji_x1 = send.x0 - f64::from(sp.s2);
        let emoji = Rect::new(emoji_x1 - btn, cy - half_btn, emoji_x1, cy + half_btn);
        let (ew, eh) = tr.measure("🙂", 16.0, None);
        text(
            &mut PaintCtx::new(scene, tf, tr, theme),
            "🙂",
            &TextStyle::new(16.0),
            c.text_dim,
            Point::new(
                emoji.center().x - f64::from(ew) / 2.0,
                line_top(emoji.center().y, f64::from(eh)),
            ),
            None,
        );
        ChatApp::push_hit(
            hits,
            emoji,
            Hit::ComposerEmoji(Point::new(emoji.x0 - 140.0, rect.y0 - 8.0)),
        );

        // Inner band for the composer widget: from just after the attach
        // button to just before the emoji button, centred on the buttons'
        // line and sized from the measured draft (one line when empty).
        let full = Rect::new(
            tx - f64::from(sp.s1),
            rect.y0 + f64::from(sp.s1),
            emoji.x0 - f64::from(sp.s2),
            rect.y1 - f64::from(sp.s1),
        );
        let wrap_w = (full.width() - COMPOSER_TEXT_INSET).max(40.0) as f32;
        let probe = if draft.is_empty() { "Ag" } else { draft };
        let content_h = f64::from(tr.measure(probe, BODY_SIZE, Some(wrap_w)).1);
        centered_band(full, content_h, f64::from(sp.s2))
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
        let c = &theme.tokens.colors;
        let Some(parent_id) = self.thread_open.clone() else {
            return;
        };
        let panel = self.thread_rect();
        fill_rect(scene, tf, panel, c.surfaces.raised);
        // The flap's leading edge is derived against its raised ground (HIG
        // §9.5) and sits on the flap's first pixel column.
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            tf,
            ui::surface::edge_on(c.surfaces.raised),
            None,
            &vello::kurbo::Line::new(
                Point::new(panel.x0 + 0.5, panel.y0),
                Point::new(panel.x0 + 0.5, panel.y1),
            ),
        );

        // The thread composer is the same control as the main composer and
        // follows its height rule (`thread_composer_rect`); the body stops
        // one gap above it.
        let comp = self.thread_composer_rect(tr, panel);
        let head = self.draw_thread_header(scene, tf, tr, theme, panel, hits);
        // The body ends one `s2` above the composer, whatever height the
        // composer measured to this frame.
        let body = Rect::new(
            panel.x0,
            head.y1,
            panel.x1,
            comp.y0 - f64::from(theme.tokens.spacing.s2),
        );
        self.draw_thread_body(PaintCtx::new(scene, tf, tr, theme), body, &parent_id);
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
        let inner = Self::draw_input_box(
            PaintCtx::new(scene, tf, tr, theme),
            comp,
            &draft,
            send_enabled,
            focused,
            Hit::FocusThread,
            false,
            hits,
        );
        self.composer_thread
            .set_mention_paint(self.mention_paint(theme));
        self.composer_thread.layout(inner, theme, tr);
        self.composer_thread.set_focus(focused);
        self.composer_thread.draw(scene, tf, tr, theme, presence);
        if let Some(draft) = self.thread_draft_key() {
            let key = ChatApp::draft_presence_key(&draft);
            Self::draw_draft_presence(&self.composer_thread, &key, scene, tf, tr, theme, presence);
        }
    }

    /// The thread panel header: title, active channel name, and the close
    /// button. Returns the header rect so the caller knows where the
    /// scrollable body starts.
    fn draw_thread_header(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> Rect {
        let c = &theme.tokens.colors;
        let scale = f64::from(self.layout_text_scale);
        let head = Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + 52.0 * scale);
        let narrow_detail = self.right_mode == crate::responsive::FlapMode::Overlay;
        let labelled_back = narrow_detail && self.layout_text_scale <= 1.25;
        let title_x = if labelled_back {
            panel.x0 + 92.0
        } else {
            panel.x0 + 16.0 * scale.min(1.2)
        };
        label(
            scene,
            tr,
            tf,
            "Thread",
            15.0,
            c.text_primary,
            title_x,
            panel.y0 + 12.0 * scale,
        );
        let chn = self
            .channel(&self.active_channel)
            .map(|c| format!("#{}", c.name))
            .unwrap_or_default();
        label(
            scene,
            tr,
            tf,
            &chn,
            11.5,
            c.text_dim,
            title_x,
            panel.y0 + 31.0 * scale,
        );
        let close = if labelled_back {
            Rect::new(
                panel.x0 + 12.0,
                panel.y0 + 10.0,
                panel.x0 + 78.0,
                panel.y0 + 42.0,
            )
        } else {
            let side = 44.0 * scale.min(1.2);
            Rect::new(
                panel.x1 - side - 12.0,
                panel.y0 + 8.0 * scale,
                panel.x1 - 12.0,
                panel.y0 + 8.0 * scale + side,
            )
        };
        let close_label = if narrow_detail { "← Back" } else { "✕" };
        // The shared Neutral button face (HIG §8).
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &ButtonFace::new(close, close_label)
                .text_size(13.0)
                .weight(TextWeight::Regular)
                .radius(8.0),
        );
        ChatApp::push_hit(hits, close, Hit::CloseThread);
        // The header's rule: derived against the flap, inset `s3` (HIG §9.5).
        ui::surface::separator(
            scene,
            tf,
            panel.x0,
            panel.x1,
            head.y1,
            f64::from(theme.tokens.spacing.s3),
            c.surfaces.raised,
        );
        head
    }

    /// The thread body: the parent message, a "N replies" divider, then
    /// every reply in order — clipped to `body`, the scrollable region
    /// between the header and the thread composer.
    fn draw_thread_body(&self, paint: PaintCtx<'_>, body: Rect, parent_id: &str) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let c = &theme.tokens.colors;
        let scale = f64::from(theme.text_scale.max(1.0));
        push_clip(scene, tf, body);
        let tx = body.x0 + (16.0 + 32.0 + 10.0) * scale.min(1.2);
        let tw = (body.x1 - tx - 14.0 * scale.min(1.2)).max(60.0) as f32;
        let mut y = body.y0 + 12.0 * scale;

        // parent message
        if let Some(parent) = self.message(parent_id).cloned() {
            y = self.draw_thread_message(
                PaintCtx::new(scene, tf, tr, theme),
                &parent,
                body.x0 + 16.0 * scale.min(1.2),
                tx,
                tw,
                y,
            );
            // replies divider
            let n = model::reply_count(&self.messages, parent_id);
            let s = format!("{n} {}", if n == 1 { "reply" } else { "replies" });
            let (sw, _) = tr.measure(&s, 11.5, None);
            let dcy = y + 14.0;
            let rule = ui::surface::edge_on(c.surfaces.raised);
            hline(
                scene,
                tf,
                body.x0 + 16.0 * scale.min(1.2),
                body.center().x - f64::from(sw) / 2.0 - 10.0 * scale,
                dcy,
                rule,
            );
            hline(
                scene,
                tf,
                body.center().x + f64::from(sw) / 2.0 + 10.0 * scale,
                body.x1 - 16.0 * scale.min(1.2),
                dcy,
                rule,
            );
            label(
                scene,
                tr,
                tf,
                &s,
                11.5,
                c.text_dim,
                body.center().x - f64::from(sw) / 2.0,
                dcy - 7.0 * scale,
            );
            y += 30.0 * scale;
        }
        let replies: Vec<Message> = self
            .messages
            .iter()
            .filter(|m| m.parent.as_deref() == Some(parent_id))
            .cloned()
            .collect();
        for r in &replies {
            y = self.draw_thread_message(
                PaintCtx::new(scene, tf, tr, theme),
                r,
                body.x0 + 16.0 * scale.min(1.2),
                tx,
                tw,
                y,
            );
        }
        scene.pop_layer();
    }

    fn draw_thread_message(
        &self,
        paint: PaintCtx<'_>,
        m: &Message,
        avx: f64,
        tx: f64,
        tw: f32,
        y: f64,
    ) -> f64 {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let c = &theme.tokens.colors;
        let scale = f64::from(self.layout_text_scale);
        let mut yy = y;
        if let Some(u) = self.user(&m.author) {
            draw_avatar(
                PaintCtx::new(scene, tf, tr, theme),
                Point::new(avx + 16.0 * scale.min(1.2), yy + 14.0 * scale),
                14.0 * scale.min(1.2),
                &u.initials,
                u.color_idx,
                Some(u.status),
            );
        }
        let (nw, _) = text(
            &mut PaintCtx::new(scene, tf, tr, theme),
            &self.user_name(&m.author),
            &TextStyle::new(13.5).with_weight(TextWeight::Semibold),
            palette_color(theme, self.user(&m.author).map_or(0, |u| u.color_idx)),
            Point::new(tx, yy),
            None,
        );
        label(
            scene,
            tr,
            tf,
            &model::clock(m.ts),
            11.0,
            c.text_dim,
            tx + f64::from(nw) + 8.0 * scale,
            yy + 2.0 * scale,
        );
        yy += 19.0 * scale;
        if !m.text.is_empty() {
            let h = self.layout_body(m, 14.0, Point::new(tx, yy), tw, theme, tr);
            self.draw_body(m, 14.0, &mut PaintCtx::new(scene, tf, tr, theme));
            yy += h + 4.0 * scale;
        }
        if let Some(att) = &m.attachment {
            self.draw_attachment(
                PaintCtx::new(scene, tf, tr, theme),
                att,
                AttachmentLayout::new(tx, yy, tw),
            );
            yy += attachment_flow_height(att, f64::from(tw), scale);
        }
        if !m.reactions.is_empty() {
            let mut x = tx;
            for r in &m.reactions {
                let lbl = format!("{} {}", r.emoji, r.count());
                let (lw, _) = tr.measure(&lbl, 11.5, None);
                let chip = Rect::new(x, yy, x + f64::from(lw) + 16.0, yy + 22.0);
                ui::surface::card(scene, tf, chip, 11.0, c.lift(0.35));
                label(
                    scene,
                    tr,
                    tf,
                    &lbl,
                    11.5,
                    c.text_primary,
                    chip.x0 + 8.0 * scale,
                    chip.center().y - 7.0 * scale,
                );
                x = chip.x1 + 5.0 * scale;
            }
            yy += 28.0 * scale;
        }
        yy + 10.0 * scale
    }

    // ----------------------------------------------------------- people roster
    /// The workspace People/Collaborators roster — the other half of the right
    /// flap, shown
    /// when no thread is open (the "👥 N" header pill's panel). Every user in
    /// the workspace with their presence and, when connected, the voice channel
    /// they're in. Pure projection of the shared user list + voice membership;
    /// nothing here is placeholder.
    pub(crate) fn draw_members(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let panel = self.thread_rect();
        fill_rect(scene, tf, panel, c.surfaces.raised);
        // Leading edge derived against the flap's raised ground (HIG §9.5).
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            tf,
            ui::surface::edge_on(c.surfaces.raised),
            None,
            &vello::kurbo::Line::new(
                Point::new(panel.x0 + 0.5, panel.y0),
                Point::new(panel.x0 + 0.5, panel.y1),
            ),
        );

        let rows = self.roster_rows();
        let head_y1 =
            self.draw_members_header(PaintCtx::new(scene, tf, tr, theme), panel, rows.len(), hits);
        self.draw_members_rows(PaintCtx::new(scene, tf, tr, theme), panel, head_y1, &rows);
    }

    /// A precise People/Collaborators label, the workspace name and roster
    /// count, plus the
    /// close control. Returns the y-coordinate the scrollable roster starts
    /// beneath.
    fn draw_members_header(
        &self,
        paint: PaintCtx<'_>,
        panel: Rect,
        people_count: usize,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let c = &theme.tokens.colors;
        label(
            scene,
            tr,
            tf,
            self.roster_label(),
            15.0,
            c.text_primary,
            panel.x0 + 16.0,
            panel.y0 + 12.0,
        );
        let ws_name = self
            .workspaces
            .iter()
            .find(|w| w.id == self.active_ws)
            .map(|w| w.name.clone())
            .unwrap_or_default();
        let noun = if self.users.iter().any(|u| u.is_agent) {
            if people_count == 1 {
                "collaborator"
            } else {
                "collaborators"
            }
        } else if people_count == 1 {
            "person"
        } else {
            "people"
        };
        let sub = format!("{people_count} {noun} · {ws_name}");
        let sub = truncate_to(tr, &sub, 11.5, panel.width() - 70.0);
        label(
            scene,
            tr,
            tf,
            &sub,
            11.5,
            c.text_dim,
            panel.x0 + 16.0,
            panel.y0 + 31.0,
        );
        let close = Rect::new(
            panel.x1 - 40.0,
            panel.y0 + 12.0,
            panel.x1 - 12.0,
            panel.y0 + 40.0,
        );
        // The shared Neutral button face (HIG §8).
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &ButtonFace::new(close, "✕")
                .text_size(14.0)
                .weight(TextWeight::Regular)
                .radius(8.0),
        );
        ChatApp::push_hit(hits, close, Hit::ToggleRightFlap);
        let head_y1 = panel.y0 + 52.0;
        ui::surface::separator(
            scene,
            tf,
            panel.x0,
            panel.x1,
            head_y1,
            f64::from(theme.tokens.spacing.s3),
            c.surfaces.raised,
        );
        head_y1
    }

    /// The scrollable roster list beneath the header: each member's avatar,
    /// display name, and presence/voice status.
    fn draw_members_rows(
        &self,
        paint: PaintCtx<'_>,
        panel: Rect,
        head_y1: f64,
        rows: &[(crate::model::User, Option<String>)],
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let c = &theme.tokens.colors;
        let body = Rect::new(panel.x0, head_y1, panel.x1, panel.y1);
        push_clip(scene, tf, body);
        let row_h = 46.0;
        let mut y = head_y1 + 8.0;
        for (u, voice) in rows {
            if y + row_h > panel.y1 {
                break;
            }
            if self.revealed_person.as_deref() == Some(u.id.as_str()) {
                fill_round(
                    scene,
                    tf,
                    Rect::new(panel.x0 + 8.0, y, panel.x1 - 8.0, y + row_h),
                    10.0,
                    c.lift(0.55),
                );
            }
            let avc = Point::new(panel.x0 + 16.0 + 16.0, y + row_h / 2.0);
            draw_avatar(
                PaintCtx::new(scene, tf, tr, theme),
                avc,
                16.0,
                &u.initials,
                u.color_idx,
                Some(u.status),
            );
            let nx = avc.x + 16.0 + 12.0;
            let display = if self.is_me(&u.id) {
                format!("{} (you)", u.name)
            } else if u.is_agent {
                format!("{} (agent)", u.name)
            } else {
                u.name.clone()
            };
            let name = truncate_to(tr, &display, 14.0, panel.x1 - nx - 14.0);
            text(
                &mut PaintCtx::new(scene, tf, tr, theme),
                &name,
                &TextStyle::new(14.0).with_weight(TextWeight::Semibold),
                c.text_primary,
                Point::new(nx, avc.y - 14.0),
                None,
            );
            let (status_txt, col) = match voice {
                Some(ch) => (format!("🔊 In voice · {ch}"), c.semantic.success),
                None => (presence_label(u.status).to_string(), c.text_dim),
            };
            let status_txt = truncate_to(tr, &status_txt, 11.5, panel.x1 - nx - 14.0);
            label(scene, tr, tf, &status_txt, 11.5, col, nx, avc.y + 2.0);
            y += row_h;
        }
        scene.pop_layer();
    }

    // ------------------------------------------------------- attach picker
    /// The workspace file picker, anchored above the composer's "+". Lists the
    /// real image files found in the shared workspace, each with a decoded
    /// thumbnail (through the same [`ui::ImageStore`] the timeline uses), so a
    /// share is always an actual file the user chose. An empty workspace says
    /// so plainly rather than offering anything fabricated.
    pub(crate) fn draw_attach_picker(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        // Full-window dismiss scrim (lowest priority).
        ChatApp::push_hit(hits, self.rect, Hit::Dismiss);

        let panel = self.attach_picker_rect(tr);
        // The shadow is the panel's own shape, from the elevation ladder (HIG
        // §9.3); the solid 5 px silhouette it replaces was a grey ring on the
        // light preset.
        ui::surface::shadow(scene, tf, panel, 14.0, &theme.tokens.elevation.e2);
        // A floating Panel: the derived panel border, crisp on the outermost
        // pixel row (HIG §9.5).
        fill_round(scene, tf, panel, 14.0, c.neutral[1]);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            tf,
            ui::chrome::panel_border(theme),
            None,
            &ui::surface::edge_outline(panel, 14.0),
        );

        let head_h = 56.0 * f64::from(self.layout_text_scale);
        self.draw_attach_picker_header(scene, tf, tr, theme, panel, head_h);

        if self.attach_entries.is_empty() {
            self.draw_attach_picker_empty(scene, tf, tr, theme, panel, head_h);
            return;
        }
        self.draw_attach_picker_rows(PaintCtx::new(scene, tf, tr, theme), panel, head_h, hits);
    }

    /// The picker panel's rect, anchored above the composer's "+" and
    /// clamped to stay inside the window at any size or entry count.
    fn attach_picker_rect(&self, tr: &mut TextRenderer) -> Rect {
        let comp = self.composer_rect(tr);
        let scale = f64::from(self.layout_text_scale);
        let w = (360.0 * scale.min(1.25))
            .min(self.rect.width() - 24.0)
            .max(220.0);
        let x0 = comp
            .x0
            .min(self.rect.x1 - w - 12.0)
            .max(self.rect.x0 + 12.0);

        let head_h = 56.0 * scale;
        let row_h = 52.0 * scale;
        let max_rows = self.attach_picker_max_rows();
        let shown = self.attach_entries.len().min(max_rows);
        let body_h = if self.attach_entries.is_empty() {
            46.0 * scale
        } else {
            shown as f64 * row_h + 8.0
        };
        let more_h = if self.attach_entries.len() > max_rows {
            20.0 * scale
        } else {
            0.0
        };
        let panel_h = head_h + body_h + more_h + 8.0;
        let y1 = comp.y0 - 10.0;
        let y0 = (y1 - panel_h).max(self.rect.y0 + 12.0);
        Rect::new(x0, y0, x0 + w, y1)
    }

    /// At increased text sizes the picker intentionally becomes a short,
    /// readable sheet instead of shrinking its filenames to preserve six rows.
    fn attach_picker_max_rows(&self) -> usize {
        if self.layout_text_scale > 1.5 {
            2
        } else if self.layout_text_scale > 1.15 {
            4
        } else {
            6
        }
    }

    /// The picker's title, entry count, and the divider beneath them.
    fn draw_attach_picker_header(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        head_h: f64,
    ) {
        let c = &theme.tokens.colors;
        let scale = f64::from(self.layout_text_scale);
        label(
            scene,
            tr,
            tf,
            "Share a file",
            13.5,
            c.text_primary,
            panel.x0 + 14.0 * scale.min(1.2),
            panel.y0 + 12.0 * scale,
        );
        let count = format!("{}", self.attach_entries.len());
        let (cw, _) = tr.measure(&count, 11.5, None);
        label(
            scene,
            tr,
            tf,
            &count,
            11.5,
            c.text_dim,
            panel.x1 - f64::from(cw) - 14.0 * scale.min(1.2),
            panel.y0 + 14.0 * scale,
        );
        if let Some(status) = &self.attachment_status
            && !(self.attachment_picker_pending.is_some() && status == "Loading attachments…")
        {
            let color = if status.contains("failed") || status.contains("cancelled") {
                c.semantic.danger
            } else {
                c.text_dim
            };
            let status = truncate_to(tr, status, 10.5, panel.width() - 28.0);
            label(
                scene,
                tr,
                tf,
                &status,
                10.5,
                color,
                panel.x0 + 14.0 * scale.min(1.2),
                panel.y0 + 30.0 * scale,
            );
        }
        ui::surface::separator(
            scene,
            tf,
            panel.x0,
            panel.x1,
            panel.y0 + head_h - 4.0,
            f64::from(theme.tokens.spacing.s3),
            c.neutral[1],
        );
    }

    /// The "no media found" message shown in place of the row list when the
    /// workspace has nothing shareable.
    fn draw_attach_picker_empty(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        head_h: f64,
    ) {
        let c = &theme.tokens.colors;
        let scale = f64::from(self.layout_text_scale);
        let s = if self.attachment_picker_pending.is_some() {
            "Loading attachments…".to_owned()
        } else {
            let dir = crate::picker::attach_dir();
            format!("No attachments in {}", dir.display())
        };
        let s = truncate_to(tr, &s, 12.5, panel.width() - 28.0);
        label(
            scene,
            tr,
            tf,
            &s,
            12.5,
            c.text_dim,
            panel.x0 + 14.0 * scale.min(1.2),
            panel.y0 + head_h + 10.0 * scale,
        );
    }

    /// Up to `max_rows` shareable entries, each with a thumbnail, name, and
    /// size — plus a "+N more" footer note when the workspace has more than
    /// that.
    #[allow(
        clippy::too_many_lines,
        reason = "one bounded picker-row presentation, not incidental coupling"
    )]
    fn draw_attach_picker_rows(
        &self,
        paint: PaintCtx<'_>,
        panel: Rect,
        head_h: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let c = &theme.tokens.colors;
        let scale = f64::from(self.layout_text_scale);
        let row_h = 52.0 * scale;
        let max_rows = self.attach_picker_max_rows();
        let mut y = panel.y0 + head_h + 4.0;
        for e in self.attach_entries.iter().take(max_rows) {
            let row = Rect::new(
                panel.x0 + 6.0,
                y,
                panel.x1 - 6.0,
                y + row_h - 6.0 * scale.min(1.2),
            );
            if row.contains(self.last_mouse) {
                fill_round(scene, tf, row, 10.0, c.lift(0.35));
            }
            let thumb_size = 52.0 * scale.min(1.15);
            let thumb = Rect::new(
                row.x0 + 6.0 * scale.min(1.2),
                row.y0 + 5.0 * scale.min(1.2),
                row.x0 + 6.0 * scale.min(1.2) + thumb_size,
                row.y1 - 5.0 * scale.min(1.2),
            );
            let key = e.key();
            // A video shows a play glyph (no image decode); an image decodes its
            // real thumbnail through the shared cache, with a neutral tile until
            // the bytes hydrate.
            if e.is_video {
                fill_round(scene, tf, thumb, 8.0, c.neutral[2]);
                let (gw, gh) = tr.measure("🎬", 16.0, None);
                text(
                    &mut PaintCtx::new(scene, tf, tr, theme),
                    "🎬",
                    &TextStyle::new(16.0),
                    c.text_dim,
                    Point::new(
                        thumb.center().x - f64::from(gw) / 2.0,
                        thumb.center().y - f64::from(gh) / 2.0,
                    ),
                    None,
                );
            } else if crate::picker::is_image_name(&e.name)
                && let Some(img) = self.images.get(&key)
            {
                ui::image_store::paint_image_cover(scene, tf, thumb, &img, 8.0);
            } else {
                fill_round(scene, tf, thumb, 8.0, c.surfaces.raised);
                let glyph = if crate::picker::is_image_name(&e.name) {
                    "🖼"
                } else {
                    "📄"
                };
                let (gw, gh) = tr.measure(glyph, 16.0, None);
                text(
                    &mut PaintCtx::new(scene, tf, tr, theme),
                    glyph,
                    &TextStyle::new(16.0),
                    c.text_dim,
                    Point::new(
                        thumb.center().x - f64::from(gw) / 2.0,
                        thumb.center().y - f64::from(gh) / 2.0,
                    ),
                    None,
                );
            }
            stroke_round(
                scene,
                tf,
                thumb,
                8.0,
                ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
                1.0,
            );
            let name = truncate_to(tr, &e.name, 13.5, row.x1 - thumb.x1 - 16.0);
            label(
                scene,
                tr,
                tf,
                &name,
                13.5,
                c.text_primary,
                thumb.x1 + 12.0,
                row.center().y - 15.0 * scale.min(1.2),
            );
            label(
                scene,
                tr,
                tf,
                &format_bytes(e.size),
                11.0,
                c.text_dim,
                thumb.x1 + 12.0,
                row.center().y + 4.0 * scale.min(1.2),
            );
            if self.attachment_pending.is_none() {
                ChatApp::push_hit(hits, row, Hit::AttachPick(key));
            }
            y += row_h;
        }
        if self.attach_entries.len() > max_rows {
            let more = format!(
                "+{} more in the workspace",
                self.attach_entries.len() - max_rows
            );
            label(
                scene,
                tr,
                tf,
                &more,
                11.0,
                c.text_dim,
                panel.x0 + 14.0,
                panel.y1 - 16.0,
            );
        }
    }
}

/// Height of one channel row: the measured name plus a pad top and bottom,
/// never shorter than the 1× row. Shared by the row rect, its hit rect and
/// the y-advance so a larger text scale grows the list instead of stacking
/// rows over each other.
fn channel_row_h(tr: &mut TextRenderer, theme: &Theme) -> f64 {
    let (_, nh) = tr.measure_weighted("Ag", CHANNEL_NAME_SIZE, None, TextWeight::Semibold);
    (f64::from(nh) + 2.0 * f64::from(theme.tokens.spacing.s1)).max(CHANNEL_ROW_MIN_H)
}

/// Height of one voice-presence row under a voice channel.
fn voice_row_h(tr: &mut TextRenderer, theme: &Theme) -> f64 {
    let (_, nh) = tr.measure("Ag", VOICE_NAME_SIZE, None);
    (f64::from(nh) + 2.0 * f64::from(theme.tokens.spacing.s1)).max(VOICE_ROW_MIN_H)
}

/// Height of a category header row ("CHANNELS", "VOICE"): the measured name
/// plus a pad top and bottom, never shorter than the 1× row. Shared by the
/// row rect, the caret / name / "+" centring, the toggle hit rect and the
/// y-advance so the header never paints over the first channel row.
fn category_row_h(tr: &mut TextRenderer, theme: &Theme) -> f64 {
    let (_, nh) = tr.measure_weighted("Ag", CATEGORY_NAME_SIZE, None, TextWeight::Semibold);
    (f64::from(nh) + 2.0 * f64::from(theme.tokens.spacing.s1)).max(CATEGORY_ROW_MIN_H)
}

/// Size `(w, h)` of the "+ Category" chip under the workspace title: its
/// measured label plus `s2` each side and `s1` above and below, never
/// smaller than the 1× chip. Measured in both the header (which paints it)
/// and the tree (which starts beneath it) so the two agree.
fn add_category_chip_size(tr: &mut TextRenderer, theme: &Theme) -> (f64, f64) {
    let sp = &theme.tokens.spacing;
    let (lw, lh) = tr.measure(&crate::i18n::t(ADD_CATEGORY_KEY), ADD_CATEGORY_SIZE, None);
    // The chip is a pointer target, so it is never shorter than the
    // toolkit's minimum target (HIG §7b: a hit target reads `metrics`, not a
    // spacing step) — the authored 22 px chip fell under it.
    let min_h = ADD_CATEGORY_CHIP_MIN_H.max(f64::from(theme.tokens.metrics.min_pointer_target));
    (
        (f64::from(lw) + 2.0 * f64::from(sp.s2)).max(ADD_CATEGORY_CHIP_MIN_W),
        (f64::from(lh) + 2.0 * f64::from(sp.s1)).max(min_h),
    )
}

/// Height of the identity bar pinned to the sidebar's bottom: the name and
/// status line boxes plus `s3` above and below, never shorter than the 1×
/// bar. The tree stops above it and the bar's avatar / toggles centre on it.
fn identity_bar_h(tr: &mut TextRenderer, theme: &Theme) -> f64 {
    let (_, name_h) = tr.measure("Ag", IDENTITY_NAME_SIZE, None);
    let (_, sub_h) = tr.measure("Ag", IDENTITY_SUB_SIZE, None);
    (f64::from(name_h) + f64::from(sub_h) + 2.0 * f64::from(theme.tokens.spacing.s3))
        .max(IDENTITY_BAR_MIN_H)
}

/// Height of one channel row in the cross-workspace inbox sidebar.
fn inbox_row_h(tr: &mut TextRenderer, theme: &Theme) -> f64 {
    let (_, nh) = tr.measure_weighted("Ag", INBOX_NAME_SIZE, None, TextWeight::Semibold);
    (f64::from(nh) + 2.0 * f64::from(theme.tokens.spacing.s2)).max(INBOX_ROW_MIN_H)
}

/// Height of a workspace label row in the inbox sidebar.
fn inbox_label_h(tr: &mut TextRenderer, theme: &Theme) -> f64 {
    let (_, lh) = tr.measure("Ag", INBOX_LABEL_SIZE, None);
    (f64::from(lh) + 2.0 * f64::from(theme.tokens.spacing.s1)).max(INBOX_LABEL_MIN_H)
}

/// Line box of a message-group header (author name + timestamp) plus the gap
/// to the body beneath it. Measured with the name's own style so the header
/// row, the y-advance in `draw_row_header` and `RowM::height` agree.
fn header_row_h(tr: &mut TextRenderer, theme: &Theme) -> f64 {
    let (_, nh) = tr.measure_weighted("Ag", NAME_SIZE, None, TextWeight::Semibold);
    f64::from(nh) + f64::from(theme.tokens.spacing.s1)
}

/// Height of the "(edited) · view history" line: its measured line box plus
/// a small gap, never shorter than the 1× band, so at larger text it does not
/// run into the reaction chips beneath it.
fn edited_row_h(tr: &mut TextRenderer, theme: &Theme) -> f64 {
    let (_, lh) = tr.measure("Ag", TIME_SIZE, None);
    (f64::from(lh) + f64::from(theme.tokens.spacing.s1)).max(EDITED_MIN_H)
}

/// Height of a reaction chip: its measured label plus a small pad top and
/// bottom, never shorter than the 1× capsule.
fn reaction_chip_h(tr: &mut TextRenderer, theme: &Theme) -> f64 {
    let (_, lh) = tr.measure("Ag", REACTION_SIZE, None);
    (f64::from(lh) + 2.0 * f64::from(theme.tokens.spacing.s1)).max(REACTION_CHIP_MIN_H)
}

/// Height of the "N replies" bar: its measured label plus a pad top and
/// bottom, never shorter than the 1× bar.
fn thread_bar_h(tr: &mut TextRenderer, theme: &Theme) -> f64 {
    let (_, lh) = tr.measure("Ag", THREAD_LABEL_SIZE, None);
    (f64::from(lh) + 2.0 * f64::from(theme.tokens.spacing.s2)).max(THREAD_BAR_MIN_H)
}

/// Per-row measured heights for the timeline layout.
struct RowM {
    idx_id: String,
    show_header: bool,
    /// Measured header row (author + timestamp); zero on continuation rows.
    header_h: f64,
    body_h: f64,
    att_h: f64,
    react_h: f64,
    edited_h: f64,
    thread_h: f64,
    /// This row is the message currently being edited inline — its body is
    /// replaced by the editor box (extra padding) plus a controls row.
    editing: bool,
    edit_ctrl_h: f64,
}
impl RowM {
    fn height(&self) -> f64 {
        let header = if self.show_header { self.header_h } else { 0.0 };
        let top_gap = if self.show_header { 14.0 } else { 3.0 };
        let body = if self.editing {
            self.body_h + EDIT_BOX_PAD + self.edit_ctrl_h
        } else if self.body_h > 0.0 {
            self.body_h + 2.0
        } else {
            0.0
        };
        top_gap + header + body + self.edited_h + self.att_h + self.react_h + self.thread_h + 4.0
    }
}

/// Vertical padding added around the inline editor's text (top + bottom).
const EDIT_BOX_PAD: f64 = 16.0;

/// Contain-fit an image into the available text width, capped at a tidy
/// preview size.
fn image_thumb_size(w: u32, h: u32, max_w: f64) -> (f64, f64) {
    let cap_w = max_w.min(380.0);
    let cap_h = 210.0;
    let (w, h) = (f64::from(w.max(1)), f64::from(h.max(1)));
    let mut tw = cap_w;
    let mut th = tw * h / w;
    if th > cap_h {
        th = cap_h;
        tw = th * w / h;
    }
    (tw, th)
}

/// Poster size for an inline video card — the real container dimensions when
/// known, otherwise a 16:9 default, capped exactly like an image thumbnail so
/// the timeline layout stays consistent.
fn video_card_size(w: u32, h: u32, max_w: f64) -> (f64, f64) {
    if w == 0 || h == 0 {
        image_thumb_size(16, 9, max_w)
    } else {
        image_thumb_size(w, h, max_w)
    }
}

/// Painted card dimensions shared by granted and permission-gated embeds.
/// Keeping this independent from the paint path guarantees access control
/// cannot change timeline geometry.
fn attachment_card_size(att: &Attachment, max_w: f64, scale: f64) -> (f64, f64) {
    match att {
        Attachment::Image { w, h, .. } => image_thumb_size(*w, *h, max_w),
        Attachment::Video { w, h, .. } => video_card_size(*w, *h, max_w),
        Attachment::File { .. } => (max_w.min(360.0), 58.0 * scale),
    }
}

/// Vertical space consumed by an attachment, including metadata and the gap
/// before the following message content.
fn attachment_flow_height(att: &Attachment, max_w: f64, scale: f64) -> f64 {
    let (_, card_h) = attachment_card_size(att, max_w, scale);
    card_h
        + if matches!(att, Attachment::File { .. }) {
            12.0 * scale
        } else {
            24.0 * scale
        }
}

/// The user-facing metadata line for a file-attachment chip: the file type
/// (derived from the display name's extension) and a human-readable size.
///
/// This deliberately carries **no filesystem path**. Where the bytes actually
/// live — the app's content store — is an implementation detail;
/// surfacing a local source path in chrome is a HIG §4 violation
/// ("no internal / implementation labels in user-facing chrome … a leaked
/// `/var/workspace` path"). The chip already shows the human filename above
/// this line, which is the only path-ish thing a user cares about.
fn file_chip_meta(name: &str, size: u64) -> String {
    let ext = std::path::Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_uppercase())
        .filter(|e| !e.is_empty());
    match ext {
        Some(e) => format!("{e} · {}", format_bytes(size)),
        None => format_bytes(size),
    }
}

/// Horizontal centre of the workspace rail (column of square buttons).
const RAIL_W_CENTER: f64 = crate::app::RAIL_W / 2.0;

#[cfg(test)]
mod tests {
    use super::{attachment_card_size, attachment_flow_height, file_chip_meta};
    use crate::model::Attachment;

    fn close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < f64::EPSILON);
    }

    #[test]
    fn attachment_chips_show_type_and_size_and_cards_reserve_their_geometry() {
        // Phase: file_chip_meta_never_leaks_a_path.
        // Defect regression: the attachment chip must never leak the internal
        // mount path. The demo's shared note lives at
        // `/var/workspace/notes/moq-over-iroh.md` (see `doc.rs`), but its chip
        // meta must show only the file type + size — no path, no "shared via".
        {
            let meta = file_chip_meta("moq-over-iroh.md", 18_944);
            assert!(
                !meta.contains('/'),
                "chip meta leaked a path separator: {meta:?}"
            );
            assert!(
                !meta.contains("/var"),
                "chip meta leaked the mount path: {meta:?}"
            );
            assert!(
                !meta.to_lowercase().contains("workspace"),
                "chip meta leaked the workspace label: {meta:?}"
            );
            assert!(
                !meta.to_lowercase().contains("shared via"),
                "chip meta still carries stale 'shared via' copy: {meta:?}"
            );
            // It still tells the user the real, useful facts: type + size.
            assert_eq!(
                meta, "MD · 18.5 KiB",
                "chip meta should be type + size only"
            );
        }
        // Phase: file_chip_meta_without_extension_is_just_the_size.
        {
            let meta = file_chip_meta("Makefile", 512);
            assert_eq!(meta, "512 B");
            assert!(!meta.contains('/'));
        }
        // Phase: attachment_size_uses_canonical_binary_units.
        {
            assert_eq!(file_chip_meta("archive.zip", 1536), "ZIP · 1.5 KiB");
        }
        // Phase: attachment_geometry_reserves_card_metadata_and_following_gap.
        {
            let file = Attachment::File {
                name: "notes.md".into(),
                blob: "sha256:file".into(),
                size: 512,
            };
            let image = Attachment::Image {
                name: "wide.png".into(),
                blob: "sha256:image".into(),
                w: 800,
                h: 400,
            };
            let unknown_video = Attachment::Video {
                name: "clip.mp4".into(),
                blob: "sha256:video".into(),
                w: 0,
                h: 0,
                size: 1024,
            };

            let (file_w, file_h) = attachment_card_size(&file, 420.0, 1.0);
            close(file_w, 360.0);
            close(file_h, 58.0);
            close(attachment_flow_height(&file, 420.0, 1.0), 70.0);

            let (image_w, image_h) = attachment_card_size(&image, 300.0, 1.0);
            close(image_w, 300.0);
            close(image_h, 150.0);
            close(attachment_flow_height(&image, 300.0, 1.0), 174.0);

            let (video_w, video_h) = attachment_card_size(&unknown_video, 320.0, 1.0);
            close(video_w, 320.0);
            close(video_h, 180.0);
            close(attachment_flow_height(&unknown_video, 320.0, 1.0), 204.0);
        }
    }
}
