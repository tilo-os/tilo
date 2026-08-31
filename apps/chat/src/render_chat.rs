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
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Avatar;

use crate::app::{AVATAR, ChatApp, Hit, MenuTarget};
use crate::model::{self, Attachment, ChannelKind, Message, Presence};
use crate::paint::{
    band_at_least, blend_toward, fill_circle, fill_rect, fill_round, hline, label, legible_on,
    palette_color, push_clip, stroke_circle, stroke_round, text, truncate_to,
};

/// Body text size for a message. Shared by the measure and the draw so a
/// mention-carrying row reserves exactly the height it paints.
const BODY_SIZE: f32 = 15.0;

/// `RichTextLayout` lays out at `size * 1.4 * mult`; `TextRenderer::measure`
/// uses `size * 1.25`. Feeding this multiplier keeps a body drawn through the
/// rich path on the same line grid as one drawn through the plain path, so a
/// message does not jump when somebody is mentioned in it.
const BODY_LINE_MULT: f32 = 1.25 / 1.4;

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

/// Split a message body into styled runs: prose in body ink, each resolved
/// mention in its participant's colour on its own band.
///
/// Built from the stored [`crate::mention::Mention`] list, never by scanning the
/// text — the renderer and the resolver agree about where a mention is because
/// only one of them ever decides.
pub(crate) fn body_spans(
    app: &ChatApp,
    m: &Message,
    theme: &Theme,
    ground: Color,
) -> Vec<ui::richtext::RichSpan> {
    use ui::richtext::RichSpan;
    let text = m.text.as_str();
    let mut spans: Vec<RichSpan> = Vec::new();
    let mut cursor = 0usize;
    let mut mentions: Vec<&crate::mention::Mention> = m.mentions.iter().collect();
    mentions.sort_by_key(|x| x.start);
    for mention in mentions {
        let (s, e) = (mention.start, mention.end());
        // An offset that no longer lands on the body (an edit that did not
        // re-resolve) is skipped rather than trusted — better plain prose than
        // a highlight over the wrong words.
        if s < cursor || e > text.len() || !text.is_char_boundary(s) || !text.is_char_boundary(e) {
            continue;
        }
        let Some(mention_text) = text.get(s..e) else {
            continue;
        };
        if s > cursor {
            let Some(prefix) = text.get(cursor..s) else {
                continue;
            };
            spans.push(RichSpan::new(prefix));
        }
        // `is_me` rather than a bare comparison: a peer that has adopted the
        // room's address for itself still authored earlier mentions under the
        // id it resolved at startup, and those still refer to this session.
        let is_self = app.is_me(&mention.user);
        let (band, ink) = mention_colors(app, &mention.user, is_self, theme, ground);
        spans.push(
            RichSpan::new(mention_text)
                .bold(true)
                .color(Some([ink.r(), ink.g(), ink.b()]))
                .background(band.map(|c| [c.r(), c.g(), c.b()])),
        );
        cursor = e;
    }
    if let Some(rest) = text.get(cursor..) {
        spans.push(RichSpan::new(rest));
    }
    spans
}

/// Lay out a mention-carrying body at `width`.
pub(crate) fn shape_body(
    tr: &mut TextRenderer,
    spans: &[ui::richtext::RichSpan],
    width: f32,
) -> ui::richtext::RichTextLayout {
    ui::richtext::RichTextLayout::shape(
        &mut tr.font_system,
        spans,
        false,
        BODY_SIZE,
        width,
        ui::richtext::TextAlign::Left,
        ui::richtext::FontFamily::Sans,
        Vec::new(),
        Vec::new(),
        BODY_LINE_MULT,
    )
}

/// Draw a circular initials avatar with an optional presence dot.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_avatar(
    scene: &mut Scene,
    tf: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    center: Point,
    r: f64,
    initials: &str,
    color_idx: usize,
    status: Option<Presence>,
) {
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

fn char_to_byte(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map_or_else(|| s.len(), |(b, _)| b)
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
        for ws in &self.workspaces {
            let active = ws.id == self.active_ws;
            let b = Rect::new(cx - btn / 2.0, y, cx + btn / 2.0, y + btn);
            // selection pill on the rail's left edge
            if active {
                let pill = Rect::new(rail.x0 + 3.0, y + 8.0, rail.x0 + 7.0, y + btn - 8.0);
                fill_round(scene, tf, pill, 2.0, c.text_primary);
            }
            let radius = if active { 16.0 } else { btn / 2.0 };
            let fill = if active {
                palette_color(theme, ws.color_idx)
            } else {
                c.neutral[2]
            };
            fill_round(scene, tf, b, radius, fill);
            let (tw, th) = tr.measure_weighted(&ws.glyph, 17.0, None, TextWeight::Bold);
            let col = if active { Color::WHITE } else { c.text_dim };
            text(
                scene,
                tr,
                tf,
                &ws.glyph,
                17.0,
                col,
                b.center().x - f64::from(tw) / 2.0,
                b.center().y - f64::from(th) / 2.0,
                TextWeight::Bold,
                None,
            );
            ChatApp::push_hit(hits, b, Hit::Workspace(ws.id.clone()));
            y += btn + 12.0;
        }

        // add-workspace affordance — creates a new workspace and switches to it.
        let add = Rect::new(cx - btn / 2.0, y, cx + btn / 2.0, y + btn);
        fill_round(scene, tf, add, btn / 2.0, c.neutral[2]);
        let (aw, ah) = tr.measure_weighted("+", 24.0, None, TextWeight::Regular);
        text(
            scene,
            tr,
            tf,
            "+",
            24.0,
            c.semantic.success,
            add.center().x - f64::from(aw) / 2.0,
            add.center().y - f64::from(ah) / 2.0,
            TextWeight::Regular,
            None,
        );
        ChatApp::push_hit(hits, add, Hit::AddWorkspace);
        // The single user avatar lives in the sidebar identity bar (drawn in
        // `draw_sidebar`), not here — the rail foot intentionally has none.
    }

    // ------------------------------------------------------------- sidebar
    pub(crate) fn draw_sidebar(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let side = self.sidebar_rect();
        fill_rect(scene, tf, side, c.surfaces.raised);

        let header = self.draw_sidebar_header(scene, tf, tr, theme, side, hits);
        self.draw_sidebar_tree(scene, tf, tr, theme, side, header, hits);
        self.draw_sidebar_identity_bar(scene, tf, tr, theme, side, hits);
    }

    /// Workspace title, online-count pill, and the "+ category" affordance
    /// beneath it. Returns the title-row rect so the caller can anchor the
    /// channel tree below it.
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
        let header = Rect::new(side.x0, side.y0, side.x1, side.y0 + 52.0);
        let ws_title = truncate_to(tr, &ws_name, 16.0, side.width() - 60.0);
        label(
            scene,
            tr,
            tf,
            &ws_title,
            16.0,
            c.text_primary,
            side.x0 + 16.0,
            side.y0 + 17.0,
        );
        // a tiny "presence count" pill
        let online = self
            .users
            .iter()
            .filter(|u| u.status == Presence::Online)
            .count();
        let pill_txt = format!("{online} online");
        let (pw, _) = tr.measure(&pill_txt, 10.5, None);
        let pill = Rect::new(
            side.x1 - f64::from(pw) - 28.0,
            side.y0 + 16.0,
            side.x1 - 12.0,
            side.y0 + 34.0,
        );
        fill_round(scene, tf, pill, 9.0, c.surface_white_10);
        fill_circle(
            scene,
            tf,
            Point::new(pill.x0 + 10.0, pill.center().y),
            3.0,
            c.semantic.success,
        );
        label(
            scene,
            tr,
            tf,
            &pill_txt,
            10.5,
            c.text_dim,
            pill.x0 + 17.0,
            pill.center().y - 7.0,
        );
        hline(
            scene,
            tf,
            side.x0 + 12.0,
            side.x1 - 12.0,
            header.y1,
            c.border_standard,
        );

        // "+ category" affordance below the workspace title.
        let add_cat = Rect::new(
            side.x0 + 12.0,
            header.y1 + 6.0,
            side.x0 + 118.0,
            header.y1 + 28.0,
        );
        fill_round(scene, tf, add_cat, 7.0, c.surface_white_5);
        label(
            scene,
            tr,
            tf,
            "＋ Category",
            11.0,
            c.text_dim,
            add_cat.x0 + 9.0,
            add_cat.center().y - 7.0,
        );
        ChatApp::push_hit(hits, add_cat, Hit::AddCategory(self.active_ws.clone()));
        header
    }

    /// The channel tree: an uncategorised group first, then each category
    /// with a collapse caret, a "+" to add a channel, and its channels.
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_sidebar_tree(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        side: Rect,
        header: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let mut y = header.y1 + 38.0;
        let body_bottom = side.y1 - 60.0;

        // Uncategorised channels (category id "").
        y = self.draw_channel_group(scene, tf, tr, theme, side, None, y, hits);

        for cat in self.ws_categories() {
            if y > body_bottom {
                break;
            }
            // Category header row: caret + name + "+".
            let hrow = Rect::new(side.x0 + 8.0, y, side.x1 - 8.0, y + 24.0);
            let caret = if cat.collapsed { "▸" } else { "▾" };
            label(
                scene,
                tr,
                tf,
                caret,
                10.0,
                c.text_dim,
                hrow.x0 + 4.0,
                y + 5.0,
            );
            // Inline rename for a category, else its name.
            let renaming_cat =
                matches!(&self.rename, Some((MenuTarget::Category(id), _)) if id == &cat.id);
            if renaming_cat {
                if let Some((_, txt)) = &self.rename {
                    Self::draw_inline_rename(
                        scene,
                        tf,
                        tr,
                        theme,
                        hrow.x0 + 18.0,
                        y + 4.0,
                        side.x1 - 60.0,
                        txt,
                        hits,
                    );
                }
            } else {
                let name = truncate_to(tr, &cat.name.to_uppercase(), 10.5, hrow.width() - 60.0);
                text(
                    scene,
                    tr,
                    tf,
                    &name,
                    10.5,
                    c.text_dim,
                    hrow.x0 + 18.0,
                    y + 5.0,
                    TextWeight::Semibold,
                    None,
                );
            }
            // "+" add-channel button.
            let plus = Rect::new(hrow.x1 - 22.0, y + 2.0, hrow.x1 - 4.0, y + 20.0);
            label(scene, tr, tf, "+", 16.0, c.text_dim, plus.x0 + 4.0, y + 1.0);
            ChatApp::push_hit(
                hits,
                plus,
                Hit::AddChannel {
                    workspace: self.active_ws.clone(),
                    category: cat.id.clone(),
                },
            );
            // Caret + name toggles collapse; right-click (resolved against the
            // toggle / "+" hit in `on_right_click`) opens the category menu.
            let toggle = Rect::new(hrow.x0, y, plus.x0, y + 24.0);
            ChatApp::push_hit(hits, toggle, Hit::CategoryToggle(cat.id.clone()));
            y += 26.0;

            if !cat.collapsed {
                y = self.draw_channel_group(scene, tf, tr, theme, side, Some(&cat.id), y, hits);
            }
            y += 6.0;
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
        let bar = Rect::new(side.x0, side.y1 - 56.0, side.x1, side.y1);
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
        let Some(me) = self.user(&self.me.id) else {
            return;
        };
        draw_avatar(
            scene,
            tf,
            tr,
            theme,
            Point::new(bar.x0 + 28.0, bar.center().y),
            16.0,
            &me.initials,
            me.color_idx,
            Some(me.status),
        );
        label(
            scene,
            tr,
            tf,
            &me.name,
            13.0,
            c.text_primary,
            bar.x0 + 50.0,
            bar.center().y - 14.0,
        );
        let sub = if self.call.active {
            "In voice"
        } else {
            "Available"
        };
        label(
            scene,
            tr,
            tf,
            sub,
            11.0,
            c.text_dim,
            bar.x0 + 50.0,
            bar.center().y + 2.0,
        );
    }

    /// The deafen + mute toggle buttons on the right of the identity bar,
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
        let deaf = Rect::new(
            bar.x1 - 80.0,
            bar.center().y - 16.0,
            bar.x1 - 48.0,
            bar.center().y + 16.0,
        );
        fill_round(
            scene,
            tf,
            deaf,
            8.0,
            if self.call.deafened {
                c.semantic.danger
            } else {
                c.surface_white_10
            },
        );
        let dglyph = if self.call.deafened { "🔕" } else { "🎧" };
        let (dw, dh) = tr.measure(dglyph, 14.0, None);
        text(
            scene,
            tr,
            tf,
            dglyph,
            14.0,
            c.text_primary,
            deaf.center().x - f64::from(dw) / 2.0,
            deaf.center().y - f64::from(dh) / 2.0,
            TextWeight::Regular,
            None,
        );
        ChatApp::push_hit(hits, deaf, Hit::CallDeafen);

        let mute = Rect::new(
            bar.x1 - 44.0,
            bar.center().y - 16.0,
            bar.x1 - 12.0,
            bar.center().y + 16.0,
        );
        fill_round(
            scene,
            tf,
            mute,
            8.0,
            if self.call.muted {
                c.semantic.danger
            } else {
                c.surface_white_10
            },
        );
        let mglyph = if self.call.muted { "🔇" } else { "🎤" };
        let (mw, mh) = tr.measure(mglyph, 14.0, None);
        text(
            scene,
            tr,
            tf,
            mglyph,
            14.0,
            c.text_primary,
            mute.center().x - f64::from(mw) / 2.0,
            mute.center().y - f64::from(mh) / 2.0,
            TextWeight::Regular,
            None,
        );
        ChatApp::push_hit(hits, mute, Hit::CallMute);
    }

    /// Draw the channels of one group (a category id, or `None` for the
    /// uncategorised group). Returns the new cursor `y`. Records per-row
    /// click + right-click context-menu hits, applies the active row swipe
    /// offset, and renders voice-channel presence (members + speaking) under
    /// each voice channel — Discord-style.
    #[allow(clippy::too_many_arguments)]
    fn draw_channel_group(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        side: Rect,
        category: Option<&str>,
        mut y: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let cat_key = category.unwrap_or("");
        let channels = self.category_channels(cat_key);
        // Index for swipe parity: position among all ws_channels.
        let ws_channels = self.ws_channels();
        for ch in channels {
            // Swipe offset: shift this row left while it's being swiped, and
            // reveal a red delete affordance behind it.
            let swipe_dx = self
                .swipe
                .filter(|s| {
                    ws_channels.get(s.channel_index).map(|cc| cc.id.as_str())
                        == Some(ch.id.as_str())
                })
                .map_or(0.0, |s| s.dx());
            y = self.draw_channel_row(scene, tf, tr, theme, side, ch, y, swipe_dx, hits);

            // Voice presence: list connected members under a voice channel.
            if ch.kind == ChannelKind::Voice {
                y = self.draw_voice_presence_rows(scene, tf, tr, theme, side, &ch.id, y);
            }
        }
        y
    }

    /// One channel row: swipe-to-delete affordance, active/live indicators,
    /// the glyph + name (or its inline rename editor), and the unread badge.
    /// Returns the cursor `y` for the row after this one.
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_channel_row(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        side: Rect,
        ch: &crate::model::Channel,
        y: f64,
        swipe_dx: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let row = Rect::new(
            side.x0 + 8.0 - swipe_dx,
            y,
            side.x1 - 8.0 - swipe_dx,
            y + 32.0,
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
        self.draw_channel_row_label(scene, tf, tr, theme, &ctx, hits);
        self.draw_channel_row_status(scene, tf, tr, theme, &ctx);

        // Click joins/opens; right-click (resolved against this row's
        // click hit in `on_right_click`) opens the channel context menu.
        let click_hit = if ch.kind == ChannelKind::Voice {
            Hit::VoiceChannel(ch.id.clone())
        } else {
            Hit::Channel(ch.id.clone())
        };
        ChatApp::push_hit(hits, row, click_hit);
        y + 34.0
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
            Rect::new(ctx.side.x0 + 8.0, ctx.y, ctx.side.x1 - 8.0, ctx.y + 32.0),
            8.0,
            c.semantic.danger.with_alpha_factor(0.9),
        );
        label(
            scene,
            tr,
            tf,
            "🗑",
            14.0,
            Color::WHITE,
            ctx.side.x1 - 30.0,
            ctx.y + 8.0,
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
        if ctx.is_active {
            fill_round(scene, tf, ctx.row, 8.0, c.surface_white_10);
            let bar = Rect::new(ctx.side.x0, ctx.y + 6.0, ctx.side.x0 + 4.0, ctx.y + 26.0);
            fill_round(scene, tf, bar, 2.0, c.accent[5]);
        }
        let glyph = ctx.ch.kind.glyph();
        let gcol = if ctx.is_active {
            c.accent[6]
        } else {
            c.text_dim
        };
        label(
            scene,
            tr,
            tf,
            glyph,
            14.0,
            gcol,
            ctx.row.x0 + 10.0,
            ctx.y + 8.0,
        );
    }

    /// The channel's inline rename editor when it is the rename target,
    /// otherwise its (possibly truncated) name.
    fn draw_channel_row_label(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        ctx: &ChannelRowCtx<'_>,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let renaming =
            matches!(&self.rename, Some((MenuTarget::Channel(id), _)) if id == &ctx.ch.id);
        if renaming {
            if let Some((_, txt)) = &self.rename {
                Self::draw_inline_rename(
                    scene,
                    tf,
                    tr,
                    theme,
                    ctx.row.x0 + 32.0,
                    ctx.y + 6.0,
                    ctx.row.width() - 80.0,
                    txt,
                    hits,
                );
            }
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
        let name = truncate_to(tr, &ctx.ch.name, 14.0, ctx.row.width() - 80.0);
        text(
            scene,
            tr,
            tf,
            &name,
            14.0,
            name_col,
            ctx.row.x0 + 32.0,
            ctx.y + 8.0,
            weight,
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
            let txt = ctx.ch.unread.to_string();
            let (bw, _) = tr.measure(&txt, 11.0, None);
            let badge = Rect::new(
                ctx.row.x1 - f64::from(bw) - 22.0,
                ctx.y + 7.0,
                ctx.row.x1 - 8.0,
                ctx.y + 25.0,
            );
            fill_round(scene, tf, badge, 9.0, c.accent[5]);
            label(
                scene,
                tr,
                tf,
                &txt,
                11.0,
                Color::WHITE,
                badge.x0 + 7.0,
                badge.center().y - 7.0,
            );
        }
    }

    /// Connected members listed under a voice channel, Discord-style: an
    /// avatar with a speaking ring, their name, and a mic/audio-only glyph.
    /// Returns the cursor `y` after the last member row.
    #[allow(clippy::too_many_arguments)]
    fn draw_voice_presence_rows(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        side: Rect,
        channel_id: &str,
        mut y: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        for m in self.voice_in(channel_id) {
            if let Some(u) = self.user(&m.user) {
                let mrow = Rect::new(side.x0 + 36.0, y, side.x1 - 8.0, y + 24.0);
                let radius = 9.0;
                let cc = Point::new(mrow.x0 + radius + 2.0, mrow.center().y);
                if m.talking && !m.muted {
                    stroke_circle(scene, tf, cc, radius + 2.5, c.semantic.success, 2.0);
                }
                draw_avatar(
                    scene,
                    tf,
                    tr,
                    theme,
                    cc,
                    radius,
                    &u.initials,
                    u.color_idx,
                    None,
                );
                let nm = truncate_to(tr, &u.name, 12.0, mrow.width() - 60.0);
                let ncol = if m.talking && !m.muted {
                    c.text_primary
                } else {
                    c.text_dim
                };
                label(
                    scene,
                    tr,
                    tf,
                    &nm,
                    12.0,
                    ncol,
                    mrow.x0 + 2.0 * radius + 8.0,
                    mrow.center().y - 7.0,
                );
                // mic / audio-only state glyphs on the right.
                if m.muted {
                    label(
                        scene,
                        tr,
                        tf,
                        "🔇",
                        11.0,
                        c.semantic.danger,
                        mrow.x1 - 18.0,
                        mrow.center().y - 7.0,
                    );
                } else if m.audio_only {
                    label(
                        scene,
                        tr,
                        tf,
                        "🎧",
                        11.0,
                        c.text_dim,
                        mrow.x1 - 18.0,
                        mrow.center().y - 7.0,
                    );
                }
                y += 26.0;
            }
        }
        y
    }

    /// Draw the inline rename editor field (a bordered box + the working
    /// text + a caret), recording a confirm hit on it.
    #[allow(clippy::too_many_arguments)]
    fn draw_inline_rename(
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        w: f64,
        value: &str,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let box_r = Rect::new(x - 4.0, y - 3.0, x + w, y + 19.0);
        fill_round(scene, tf, box_r, 5.0, c.neutral[2]);
        stroke_round(scene, tf, box_r, 5.0, c.border_focus, 1.4);
        let (tw, _) = text(
            scene,
            tr,
            tf,
            value,
            13.0,
            c.text_primary,
            x,
            y,
            TextWeight::Regular,
            Some((w - 8.0) as f32),
        );
        let caret = Rect::new(
            x + f64::from(tw) + 1.0,
            y,
            x + f64::from(tw) + 2.4,
            y + 16.0,
        );
        fill_rect(scene, tf, caret, c.border_focus);
        ChatApp::push_hit(hits, box_r, Hit::RenameConfirm);
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
        hline(scene, tf, h.x0, h.x1, h.y1, c.border_standard);

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
            Some(ch) => (ch.name.clone(), ch.topic.clone(), ch.kind.glyph()),
            None => (String::new(), String::new(), "#"),
        };

        // Right-aligned action buttons are placed first so the title + topic
        // know exactly where they must stop — they truncate before the button
        // cluster and can never overlap it, even on a very narrow header.
        let cluster_left = self.draw_header_action_buttons(scene, tf, tr, theme, h, hits);
        Self::draw_header_title(
            scene,
            tf,
            tr,
            theme,
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
        let members = self.users.len();
        let call_w = 84.0;
        let mem_w = 52.0;
        let gap = 8.0;
        let cy = h.center().y;
        // call button (accent when a call is live).
        let call_x0 = h.x1 - 16.0 - call_w;
        let call_btn = Rect::new(call_x0, cy - 16.0, call_x0 + call_w, cy + 16.0);
        let (cbg, ctx) = if self.call.active {
            (c.accent[5], Color::WHITE)
        } else {
            (c.surface_white_10, c.text_primary)
        };
        fill_round(scene, tf, call_btn, 8.0, cbg);
        let call_label = if self.call.active {
            "● In call"
        } else {
            "📹 Call"
        };
        label(
            scene,
            tr,
            tf,
            call_label,
            12.5,
            ctx,
            call_btn.x0 + 12.0,
            call_btn.center().y - 8.0,
        );
        ChatApp::push_hit(hits, call_btn, Hit::StartCall);

        // members pill — toggles the right flap (members / thread).
        let mem_x0 = call_x0 - gap - mem_w;
        let mem = Rect::new(mem_x0, cy - 16.0, mem_x0 + mem_w, cy + 16.0);
        let mem_on = self.right_flap_open();
        fill_round(
            scene,
            tf,
            mem,
            8.0,
            if mem_on {
                c.accent[5].with_alpha_factor(0.22)
            } else {
                c.surface_white_5
            },
        );
        label(
            scene,
            tr,
            tf,
            &format!("👥 {members}"),
            12.5,
            if mem_on { c.accent[7] } else { c.text_dim },
            mem.x0 + 9.0,
            mem.center().y - 8.0,
        );
        ChatApp::push_hit(hits, mem, Hit::ToggleRightFlap);
        mem_x0
    }

    /// The channel glyph + name + topic, truncated to stop before
    /// `cluster_left` so nothing runs under the action buttons.
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_header_title(
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        h: Rect,
        left_inset: f64,
        cluster_left: f64,
        name: &str,
        topic: &str,
        glyph: &str,
    ) {
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
            scene,
            tr,
            tf,
            &name_t,
            17.0,
            c.text_primary,
            nx,
            cy - 11.0,
            TextWeight::Bold,
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
            fill_round(scene, tf, btn, 8.0, c.surface_white_10);
            label(
                scene,
                tr,
                tf,
                "☰",
                16.0,
                c.text_primary,
                btn.x0 + 8.0,
                btn.center().y - 9.0,
            );
            ChatApp::push_hit(hits, btn, Hit::ToggleLeftFlap);
        }
    }

    /// A right-click context menu over a channel / category / workspace.
    /// Lays its items below the anchor and records a per-item action hit
    /// plus a full-window scrim hit to dismiss.
    pub(crate) fn draw_context_menu(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        menu: &crate::app::ContextMenu,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        // Dismiss scrim (lowest priority).
        ChatApp::push_hit(hits, self.rect, Hit::Dismiss);

        let w = 188.0;
        let item_h = 32.0;
        let menu_h = 10.0 + menu.items.len() as f64 * item_h + 6.0;
        let x0 = menu
            .anchor
            .x
            .clamp(self.rect.x0 + 6.0, self.rect.x1 - w - 6.0);
        let y0 = menu.anchor.y.clamp(
            self.rect.y0 + 6.0,
            (self.rect.y1 - menu_h - 6.0).max(self.rect.y0 + 6.0),
        );
        let panel = Rect::new(x0, y0, x0 + w, y0 + menu_h);
        fill_round(
            scene,
            tf,
            panel.inflate(5.0, 5.0),
            16.0,
            Color::from_rgba8(0, 0, 0, 80),
        );
        fill_round(scene, tf, panel, 12.0, c.neutral[1]);
        stroke_round(scene, tf, panel, 12.0, c.border_white_10, 1.0);

        let mut y = panel.y0 + 6.0;
        for item in &menu.items {
            let r = Rect::new(panel.x0 + 5.0, y, panel.x1 - 5.0, y + item_h - 3.0);
            let hot = r.contains(self.last_mouse);
            if hot {
                fill_round(scene, tf, r, 8.0, c.surface_white_10);
            }
            let col = if item.is_destructive() {
                c.semantic.danger
            } else {
                c.text_primary
            };
            label(
                scene,
                tr,
                tf,
                item.label(),
                13.0,
                col,
                r.x0 + 12.0,
                r.center().y - 8.0,
            );
            ChatApp::push_hit(
                hits,
                r,
                Hit::MenuAction {
                    target: menu.target.clone(),
                    action: *item,
                },
            );
            y += item_h;
        }
    }

    // ------------------------------------------------------------ messages
    pub(crate) fn draw_messages(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let text_x = area.x0 + 16.0 + AVATAR + 14.0;
        let text_w = (area.x1 - text_x - 18.0).max(80.0) as f32;

        // Empty channel: a quiet start-of-conversation marker rather than a
        // blank void, so a fresh channel reads as ready, not broken.
        if self.messages.is_empty() {
            self.draw_empty_channel_marker(scene, tf, tr, theme, area);
            return;
        }

        let groups = model::group_messages(&self.messages);
        let divider_h = 36.0;
        let (rows, total) = self.measure_message_rows(tr, theme, &groups, text_w, divider_h);

        let start_y = if total > area.height() {
            area.y1 - total
        } else {
            area.y0
        };
        let max_start = area.y0;
        let min_start = (area.y1 - total).min(area.y0);
        let y = (start_y + self.scroll).clamp(min_start, max_start);

        let mut y = self.draw_messages_date_divider(scene, tf, tr, theme, area, y, divider_h);

        for rm in &rows {
            self.draw_row(scene, tf, tr, theme, rm, area, text_x, text_w, &mut y, hits);
        }
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
        fill_circle(scene, tf, Point::new(cx, cy), 30.0, c.surface_white_5);
        let (gw, gh) = tr.measure_weighted("#", 30.0, None, TextWeight::Bold);
        text(
            scene,
            tr,
            tf,
            "#",
            30.0,
            c.text_dim,
            cx - f64::from(gw) / 2.0,
            cy - f64::from(gh) / 2.0,
            TextWeight::Bold,
            None,
        );
        let title = format!("#{ch_name}");
        let (tw, _) = tr.measure_weighted(&title, 20.0, None, TextWeight::Bold);
        text(
            scene,
            tr,
            tf,
            &title,
            20.0,
            c.text_primary,
            cx - f64::from(tw) / 2.0,
            cy + 46.0,
            TextWeight::Bold,
            None,
        );
        let sub = format!("This is the start of the {title} channel.");
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
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_messages_date_divider(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        y: f64,
        divider_h: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let dl = self.messages.first().map_or_else(
            || "Today".into(),
            |m| model::day_label(m.ts, crate::doc::NOW),
        );
        let (dw, _) = tr.measure(&dl, 11.5, None);
        let dcy = y + divider_h / 2.0;
        hline(
            scene,
            tf,
            area.x0 + 24.0,
            area.center().x - f64::from(dw) / 2.0 - 14.0,
            dcy,
            c.border_standard,
        );
        hline(
            scene,
            tf,
            area.center().x + f64::from(dw) / 2.0 + 14.0,
            area.x1 - 24.0,
            dcy,
            c.border_standard,
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
        let editing = self.editing.as_ref().is_some_and(|(id, _)| id == &m.id);
        // The editor measures its live working text so the timeline reflows as
        // you type; a static message measures its stored body.
        let body_h = if editing {
            let t = self.editing.as_ref().map_or("", |(_, t)| t.as_str());
            f64::from(tr.measure(t, 15.0, Some(text_w)).1).max(19.0)
        } else if m.text.is_empty() {
            0.0
        } else if m.mentions.is_empty() {
            f64::from(tr.measure(&m.text, BODY_SIZE, Some(text_w)).1)
        } else {
            // A mention-carrying body is laid out by the rich path, so it must
            // be measured by the rich path too — measuring one and painting the
            // other is how a row ends up clipping its own last line.
            let spans = body_spans(self, m, theme, theme.tokens.colors.surfaces.base);
            f64::from(shape_body(tr, &spans, text_w).height())
        };
        let att_h = match &m.attachment {
            None => 0.0,
            Some(Attachment::File { .. }) => 70.0,
            Some(Attachment::Image { w, h, .. }) => {
                image_thumb_size(*w, *h, f64::from(text_w)).1 + 24.0
            }
            Some(Attachment::Video { w, h, .. }) => {
                video_card_size(*w, *h, f64::from(text_w)).1 + 24.0
            }
        };
        let react_h = if m.reactions.is_empty() { 0.0 } else { 32.0 };
        // While editing, the "(edited)" marker is suppressed in favour of the
        // editor's own controls row.
        let edited_h = if m.edited && !editing { 18.0 } else { 0.0 };
        let thread_h = if model::reply_count(&self.messages, &m.id) > 0 {
            46.0
        } else {
            0.0
        };
        let edit_ctrl_h = if editing { 30.0 } else { 0.0 };
        RowM {
            idx_id: m.id.clone(),
            show_header,
            body_h,
            att_h,
            react_h,
            edited_h,
            thread_h,
            editing,
            edit_ctrl_h,
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_row(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rm: &RowM,
        area: Rect,
        text_x: f64,
        text_w: f32,
        y: &mut f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let Some(m) = self.message(&rm.idx_id) else {
            return;
        };
        let top_gap = if rm.show_header { 14.0 } else { 3.0 };
        *y += top_gap;
        // Top of the header + body region — the right-click target for your
        // own messages (recorded below, beneath the reaction / thread hits).
        let content_top = *y;

        if rm.show_header {
            self.draw_row_header(scene, tf, tr, theme, area, text_x, m, y);
        }

        self.draw_row_body(
            scene,
            tf,
            tr,
            theme,
            rm,
            m,
            area,
            text_x,
            text_w,
            content_top,
            y,
            hits,
        );
        self.draw_row_footer(scene, tf, tr, theme, rm, m, text_x, text_w, y, hits);
    }

    /// A message group's header: avatar, author name (in their palette
    /// colour), and the timestamp. Advances `*y` past it.
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_row_header(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        text_x: f64,
        m: &Message,
        y: &mut f64,
    ) {
        let c = &theme.tokens.colors;
        let u = self.user(&m.author).cloned();
        let (initials, cidx, status) = u.as_ref().map_or_else(
            || ("?".into(), 0, None),
            |u| (u.initials.clone(), u.color_idx, Some(u.status)),
        );
        draw_avatar(
            scene,
            tf,
            tr,
            theme,
            Point::new(area.x0 + 16.0 + AVATAR / 2.0, *y + AVATAR / 2.0),
            AVATAR / 2.0,
            &initials,
            cidx,
            status,
        );
        let name = self.user_name(&m.author);
        let (nw, _) = text(
            scene,
            tr,
            tf,
            &name,
            14.5,
            palette_color(theme, cidx),
            text_x,
            *y,
            TextWeight::Semibold,
            None,
        );
        label(
            scene,
            tr,
            tf,
            &model::clock(m.ts),
            11.5,
            c.text_dim,
            text_x + f64::from(nw) + 10.0,
            *y + 3.0,
        );
        *y += 22.0;
    }

    /// The editor (while editing) or the plain/mention-attributed body text,
    /// plus the right-click hit region over the header+body block for your
    /// own messages. Advances `*y` past the body.
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_row_body(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rm: &RowM,
        m: &Message,
        area: Rect,
        text_x: f64,
        text_w: f32,
        content_top: f64,
        y: &mut f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        if rm.editing {
            self.draw_message_editor(scene, tf, tr, theme, text_x, text_w, rm, y, hits);
            return;
        }
        if !m.text.is_empty() {
            if m.mentions.is_empty() {
                text(
                    scene,
                    tr,
                    tf,
                    &m.text,
                    BODY_SIZE,
                    c.text_primary,
                    text_x,
                    *y,
                    TextWeight::Regular,
                    Some(text_w),
                );
            } else {
                // Mentions are runs inside the paragraph, so the whole body
                // goes through the attributed layout — one wrap, one line
                // grid, bands painted behind the glyphs they belong to.
                let spans = body_spans(self, m, theme, c.surfaces.base);
                let layout = shape_body(tr, &spans, text_w);
                tr.draw_rich(
                    scene,
                    &layout,
                    tf * Affine::translate((text_x, *y)),
                    c.text_primary,
                );
            }
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
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_row_footer(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rm: &RowM,
        m: &Message,
        text_x: f64,
        text_w: f32,
        y: &mut f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        if m.edited && !rm.editing {
            let edited_label = "(edited) · view history";
            let edited_hit = Rect::new(
                text_x,
                *y,
                text_x + f64::from(tr.measure(edited_label, 11.5, None).0) + 4.0,
                *y + 16.0,
            );
            label(scene, tr, tf, edited_label, 11.5, c.text_dim, text_x, *y);
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
            self.draw_attachment(scene, tf, tr, theme, att, text_x, *y, text_w);
            *y += rm.att_h;
        }

        // Empty reaction strip still gets an add affordance on header rows —
        // handled elsewhere, so there is nothing to draw when it's empty.
        if !m.reactions.is_empty() {
            self.draw_reactions(scene, tf, tr, theme, m, text_x, *y, hits);
            *y += rm.react_h;
        }

        if rm.thread_h > 0.0 {
            self.draw_thread_affordance(scene, tf, tr, theme, m, text_x, *y, text_w, hits);
            *y += rm.thread_h;
        }
        *y += 4.0;
    }

    /// Render the inline message editor in place of a message body: a focused
    /// input box carrying the live working text + caret, and a controls row
    /// (a keyboard hint plus Cancel / Save). Advances `*y` past the whole
    /// editor. The wrap width matches [`Self::measure_row`] so the timeline
    /// height stays exact as the text grows.
    #[allow(clippy::too_many_arguments)]
    fn draw_message_editor(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        text_x: f64,
        text_w: f32,
        rm: &RowM,
        y: &mut f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let editing_text = self.editing.as_ref().map_or("", |(_, t)| t.as_str());
        let box_h = rm.body_h + EDIT_BOX_PAD;
        let er = Rect::new(
            text_x - 8.0,
            *y,
            text_x + f64::from(text_w) + 8.0,
            *y + box_h,
        );
        fill_round(scene, tf, er, 10.0, c.neutral[2]);
        stroke_round(scene, tf, er, 10.0, c.border_focus, 1.6);
        if editing_text.is_empty() {
            label(
                scene,
                tr,
                tf,
                "Edit message…",
                15.0,
                c.text_dim,
                text_x,
                *y + 8.0,
            );
        } else {
            text(
                scene,
                tr,
                tf,
                editing_text,
                15.0,
                c.text_primary,
                text_x,
                *y + 8.0,
                TextWeight::Regular,
                Some(text_w),
            );
        }
        let caret_byte = char_to_byte(editing_text, self.caret);
        let prefix = editing_text.get(..caret_byte).unwrap_or(editing_text);
        let cx =
            (text_x + f64::from(tr.measure(prefix, 15.0, None).0)).min(text_x + f64::from(text_w));
        fill_rect(
            scene,
            tf,
            Rect::new(cx, *y + 8.0, cx + 1.6, *y + 27.0),
            c.border_focus,
        );
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
            scene,
            tr,
            tf,
            "Save",
            12.5,
            Color::WHITE,
            save.center().x - f64::from(sw) / 2.0,
            save.center().y - f64::from(sh) / 2.0,
            TextWeight::Semibold,
            None,
        );
        ChatApp::push_hit(hits, save, Hit::EditSave);
        let cancel = Rect::new(save.x0 - 8.0 - 62.0, cy, save.x0 - 8.0, cy + 24.0);
        fill_round(scene, tf, cancel, 7.0, c.surface_white_10);
        let (kw, kh) = tr.measure("Cancel", 12.5, None);
        text(
            scene,
            tr,
            tf,
            "Cancel",
            12.5,
            c.text_primary,
            cancel.center().x - f64::from(kw) / 2.0,
            cancel.center().y - f64::from(kh) / 2.0,
            TextWeight::Regular,
            None,
        );
        ChatApp::push_hit(hits, cancel, Hit::EditCancel);
        *y += rm.edit_ctrl_h;
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_reactions(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        m: &Message,
        text_x: f64,
        y: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let mut x = text_x;
        let chip_h = 26.0;
        for r in &m.reactions {
            let mine = r.reacted_by(&self.me.id);
            let lbl = format!("{}  {}", r.emoji, r.count());
            let (lw, _) = tr.measure(&lbl, 12.5, None);
            let chip = Rect::new(x, y, x + f64::from(lw) + 20.0, y + chip_h);
            let bg = if mine {
                c.accent[5].with_alpha_factor(0.22)
            } else {
                c.surface_white_5
            };
            fill_round(scene, tf, chip, 13.0, bg);
            if mine {
                stroke_round(scene, tf, chip, 13.0, c.accent[5], 1.0);
            }
            label(
                scene,
                tr,
                tf,
                &lbl,
                12.5,
                if mine { c.accent[7] } else { c.text_primary },
                chip.x0 + 10.0,
                chip.center().y - 8.0,
            );
            ChatApp::push_hit(
                hits,
                chip,
                Hit::Reaction {
                    msg: m.id.clone(),
                    emoji: r.emoji.clone(),
                },
            );
            x = chip.x1 + 6.0;
        }
        // add-reaction affordance
        let add = Rect::new(x, y, x + 34.0, y + chip_h);
        fill_round(scene, tf, add, 13.0, c.surface_white_5);
        let (aw, ah) = tr.measure("☺", 14.0, None);
        text(
            scene,
            tr,
            tf,
            "☺",
            14.0,
            c.text_dim,
            add.center().x - f64::from(aw) / 2.0,
            add.center().y - f64::from(ah) / 2.0,
            TextWeight::Regular,
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

    #[allow(clippy::too_many_arguments)]
    fn draw_thread_affordance(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        m: &Message,
        text_x: f64,
        y: f64,
        text_w: f32,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let count = model::reply_count(&self.messages, &m.id);
        let parts = model::thread_participants(&self.messages, &m.id);
        let bar = Rect::new(text_x, y, text_x + f64::from(text_w).min(360.0), y + 38.0);
        let open = self.thread_open.as_deref() == Some(m.id.as_str());
        fill_round(
            scene,
            tf,
            bar,
            10.0,
            if open {
                c.accent[5].with_alpha_factor(0.14)
            } else {
                c.surface_white_5
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
                    scene,
                    tf,
                    tr,
                    theme,
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
        label(
            scene,
            tr,
            tf,
            &txt,
            13.0,
            c.accent[6],
            ax + 8.0,
            bar.center().y - 8.0,
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

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_attachment(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        att: &Attachment,
        x: f64,
        y: f64,
        text_w: f32,
    ) {
        // Permission boundary: an embed this peer hasn't been granted access to
        // renders as a locked card and never hydrates its bytes off the FUSE
        // mount. Same footprint as the real embed so the timeline layout is
        // identical whether granted or gated.
        if !self.embed_allowed(att.path()) {
            Self::draw_gated_embed(scene, tf, tr, theme, att, x, y, text_w);
            return;
        }
        match att {
            Attachment::Image { name, path, w, h } => {
                self.draw_image_attachment(scene, tf, tr, theme, name, path, *w, *h, x, y, text_w);
            }
            Attachment::Video {
                name, w, h, size, ..
            } => {
                Self::draw_video_attachment(
                    scene, tf, tr, theme, name, *w, *h, *size, x, y, text_w,
                );
            }
            Attachment::File { name, size, .. } => {
                Self::draw_file_attachment(scene, tf, tr, theme, name, *size, x, y, text_w);
            }
        }
    }

    /// The image attachment card: a decoded thumbnail (or a neutral
    /// placeholder while it hydrates), a caption scrim with the filename,
    /// and its pixel dimensions beneath.
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_image_attachment(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        name: &str,
        path: &str,
        img_w: u32,
        img_h: u32,
        x: f64,
        y: f64,
        text_w: f32,
    ) {
        let c = &theme.tokens.colors;
        let (tw, th) = image_thumb_size(img_w, img_h, f64::from(text_w));
        let rect = Rect::new(x, y, x + tw, y + th);
        // Real decoded bytes from the shared background image cache
        // (projected from the FUSE workspace). `get` schedules the
        // load the first time a path is seen and returns the decoded
        // image on a later frame; until then we show a neutral tile.
        if let Some(img) = self.images.get(path) {
            ui::image_store::paint_image_cover(scene, tf, rect, &img, 12.0);
        } else {
            fill_round(scene, tf, rect, 12.0, c.surfaces.raised);
            let dot = Point::new(rect.center().x, rect.center().y - 4.0);
            fill_circle(scene, tf, dot, 12.0, c.surface_white_10);
            let (gw, gh) = tr.measure("🖼", 18.0, None);
            text(
                scene,
                tr,
                tf,
                "🖼",
                18.0,
                c.text_dim,
                dot.x - f64::from(gw) / 2.0,
                dot.y - f64::from(gh) / 2.0,
                TextWeight::Regular,
                None,
            );
        }
        // Caption gradient scrim + filename, then a framing stroke.
        fill_round(
            scene,
            tf,
            Rect::new(rect.x0, rect.y1 - 30.0, rect.x1, rect.y1),
            12.0,
            Color::from_rgba8(0, 0, 0, 120),
        );
        stroke_round(scene, tf, rect, 12.0, c.border_white_10, 1.0);
        label(
            scene,
            tr,
            tf,
            &format!("🖼  {name}"),
            12.0,
            Color::WHITE,
            rect.x0 + 12.0,
            rect.y1 - 22.0,
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
            rect.y1 + 4.0,
        );
    }

    /// The video attachment card: a poster surface with a centred play
    /// glyph, a caption scrim with the filename, and dimensions + size
    /// metadata beneath.
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_video_attachment(
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        name: &str,
        vid_w: u32,
        vid_h: u32,
        size: u64,
        x: f64,
        y: f64,
        text_w: f32,
    ) {
        let c = &theme.tokens.colors;
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
            c.surface_white_5,
        );
        // Centred play button.
        let cc = rect.center();
        fill_circle(scene, tf, cc, 25.0, Color::from_rgba8(0, 0, 0, 150));
        fill_circle(scene, tf, cc, 24.0, c.surface_white_10);
        let (pw, ph) = tr.measure("▶", 20.0, None);
        text(
            scene,
            tr,
            tf,
            "▶",
            20.0,
            Color::WHITE,
            cc.x - f64::from(pw) / 2.0 + 2.0,
            cc.y - f64::from(ph) / 2.0,
            TextWeight::Regular,
            None,
        );
        // Caption scrim + filename, then a framing stroke.
        fill_round(
            scene,
            tf,
            Rect::new(rect.x0, rect.y1 - 30.0, rect.x1, rect.y1),
            12.0,
            Color::from_rgba8(0, 0, 0, 120),
        );
        stroke_round(scene, tf, rect, 12.0, c.border_white_10, 1.0);
        label(
            scene,
            tr,
            tf,
            &format!("🎬  {name}"),
            12.0,
            Color::WHITE,
            rect.x0 + 12.0,
            rect.y1 - 22.0,
        );
        let meta = if vid_w > 0 && vid_h > 0 {
            format!("{vid_w}×{vid_h} · {}", human_size(size))
        } else {
            format!("video · {}", human_size(size))
        };
        label(
            scene,
            tr,
            tf,
            &meta,
            11.0,
            c.text_dim,
            rect.x0,
            rect.y1 + 4.0,
        );
    }

    /// The generic file attachment chip: an icon tile, filename, and human
    /// metadata (kind/size) beneath it.
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_file_attachment(
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        name: &str,
        size: u64,
        x: f64,
        y: f64,
        text_w: f32,
    ) {
        let c = &theme.tokens.colors;
        let chip = Rect::new(x, y, x + f64::from(text_w).min(360.0), y + 58.0);
        fill_round(scene, tf, chip, 10.0, c.surfaces.raised);
        stroke_round(scene, tf, chip, 10.0, c.border_standard, 1.0);
        let icon = Rect::new(
            chip.x0 + 10.0,
            chip.y0 + 11.0,
            chip.x0 + 46.0,
            chip.y1 - 11.0,
        );
        fill_round(scene, tf, icon, 8.0, c.accent[5]);
        let (iw, ih) = tr.measure("📄", 16.0, None);
        text(
            scene,
            tr,
            tf,
            "📄",
            16.0,
            Color::WHITE,
            icon.center().x - f64::from(iw) / 2.0,
            icon.center().y - f64::from(ih) / 2.0,
            TextWeight::Regular,
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
            chip.x0 + 56.0,
            chip.center().y - 14.0,
        );
        let meta = file_chip_meta(name, size);
        label(
            scene,
            tr,
            tf,
            &meta,
            11.0,
            c.text_dim,
            chip.x0 + 56.0,
            chip.center().y + 2.0,
        );
    }

    /// Render an embed this peer isn't permitted to open: a neutral locked card
    /// with the filename but no hydrated content. Sized to match the granted
    /// embed's footprint so the timeline layout is identical either way.
    #[allow(clippy::too_many_arguments)]
    fn draw_gated_embed(
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        att: &Attachment,
        x: f64,
        y: f64,
        text_w: f32,
    ) {
        let c = &theme.tokens.colors;
        let (card_w, card_h) = match att {
            Attachment::Image { w, h, .. } => image_thumb_size(*w, *h, f64::from(text_w)),
            Attachment::Video { w, h, .. } => video_card_size(*w, *h, f64::from(text_w)),
            Attachment::File { .. } => (f64::from(text_w).min(360.0), 58.0),
        };
        let rect = Rect::new(x, y, x + card_w, y + card_h);
        fill_round(scene, tf, rect, 12.0, c.neutral[1]);
        stroke_round(scene, tf, rect, 12.0, c.border_standard, 1.0);
        let cx = rect.center().x;
        let cy = rect.center().y - if card_h > 64.0 { 8.0 } else { 6.0 };
        let (lw, lh) = tr.measure("🔒", 18.0, None);
        text(
            scene,
            tr,
            tf,
            "🔒",
            18.0,
            c.text_dim,
            cx - f64::from(lw) / 2.0,
            cy - f64::from(lh) / 2.0,
            TextWeight::Regular,
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
            cy + 12.0,
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
            cy + 28.0,
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
        // While an inline message edit is in progress the caret lives in the
        // editor, not the composer.
        let focused = matches!(self.focus, crate::app::Focus::Main) && self.editing.is_none();
        let send_enabled = !self.composer_main.value().trim().is_empty();
        let inner = Self::draw_input_box(
            scene,
            tf,
            tr,
            theme,
            rect,
            send_enabled,
            focused,
            Hit::FocusMain,
            true,
            hits,
        );
        self.composer_main.apply_layout_external(inner);
        self.composer_main.set_focus(focused);
        self.composer_main.draw(scene, tf, tr, theme, presence);
    }

    /// Draw the composer box chrome — the rounded field, the attach `+`, the
    /// emoji + send buttons — and record their hit regions. Returns the inner
    /// rect the caller lays the composer [`TextInput`] into; the widget owns
    /// the text, caret, selection, and placeholder inside it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_input_box(
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        send_enabled: bool,
        focused: bool,
        focus_hit: Hit,
        attach: bool,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> Rect {
        let c = &theme.tokens.colors;
        fill_round(scene, tf, rect, 14.0, c.neutral[2]);
        stroke_round(
            scene,
            tf,
            rect,
            14.0,
            if focused {
                c.border_focus
            } else {
                c.border_standard
            },
            if focused { 1.6 } else { 1.0 },
        );
        ChatApp::push_hit(hits, rect, focus_hit);

        let mut tx = rect.x0 + 16.0;
        if attach {
            let plus = Rect::new(
                rect.x0 + 10.0,
                rect.center().y - 14.0,
                rect.x0 + 38.0,
                rect.center().y + 14.0,
            );
            fill_round(scene, tf, plus, 8.0, c.surface_white_10);
            let (pw, ph) = tr.measure("+", 20.0, None);
            text(
                scene,
                tr,
                tf,
                "+",
                20.0,
                c.text_dim,
                plus.center().x - f64::from(pw) / 2.0,
                plus.center().y - f64::from(ph) / 2.0,
                TextWeight::Regular,
                None,
            );
            ChatApp::push_hit(hits, plus, Hit::Attach);
            tx = plus.x1 + 12.0;
        }

        // right cluster: emoji + send
        let send = Rect::new(
            rect.x1 - 56.0,
            rect.center().y - 16.0,
            rect.x1 - 12.0,
            rect.center().y + 16.0,
        );
        fill_round(
            scene,
            tf,
            send,
            9.0,
            if send_enabled {
                c.accent[5]
            } else {
                c.surface_white_5
            },
        );
        let (sw, sh) = tr.measure("➤", 16.0, None);
        text(
            scene,
            tr,
            tf,
            "➤",
            16.0,
            if send_enabled {
                Color::WHITE
            } else {
                c.text_dim
            },
            send.center().x - f64::from(sw) / 2.0,
            send.center().y - f64::from(sh) / 2.0,
            TextWeight::Regular,
            None,
        );
        ChatApp::push_hit(hits, send, Hit::Send);

        let emoji = Rect::new(
            send.x0 - 40.0,
            rect.center().y - 16.0,
            send.x0 - 8.0,
            rect.center().y + 16.0,
        );
        let (ew, eh) = tr.measure("🙂", 16.0, None);
        text(
            scene,
            tr,
            tf,
            "🙂",
            16.0,
            c.text_dim,
            emoji.center().x - f64::from(ew) / 2.0,
            emoji.center().y - f64::from(eh) / 2.0,
            TextWeight::Regular,
            None,
        );
        ChatApp::push_hit(
            hits,
            emoji,
            Hit::ComposerEmoji(Point::new(emoji.x0 - 140.0, rect.y0 - 8.0)),
        );

        // Inner rect for the composer widget: from just after the attach glyph
        // to just before the emoji button, filling the box vertically.
        Rect::new(tx - 4.0, rect.y0 + 4.0, emoji.x0 - 8.0, rect.y1 - 4.0)
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
        hline(scene, tf, panel.x0, panel.x0, panel.y0, c.border_standard);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            tf,
            c.border_standard,
            None,
            &vello::kurbo::Line::new(
                Point::new(panel.x0, panel.y0),
                Point::new(panel.x0, panel.y1),
            ),
        );

        let head = self.draw_thread_header(scene, tf, tr, theme, panel, hits);
        self.draw_thread_body(scene, tf, tr, theme, panel, head, &parent_id, hits);

        // thread composer
        let comp = Rect::new(
            panel.x0 + 12.0,
            panel.y1 - 60.0,
            panel.x1 - 12.0,
            panel.y1 - 12.0,
        );
        let focused = matches!(self.focus, crate::app::Focus::Thread) && self.editing.is_none();
        let send_enabled = !self.composer_thread.value().trim().is_empty();
        let inner = Self::draw_input_box(
            scene,
            tf,
            tr,
            theme,
            comp,
            send_enabled,
            focused,
            Hit::FocusThread,
            false,
            hits,
        );
        self.composer_thread.apply_layout_external(inner);
        self.composer_thread.set_focus(focused);
        self.composer_thread.draw(scene, tf, tr, theme, presence);
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
        let head = Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + 52.0);
        label(
            scene,
            tr,
            tf,
            "Thread",
            15.0,
            c.text_primary,
            panel.x0 + 16.0,
            panel.y0 + 12.0,
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
            panel.x0 + 16.0,
            panel.y0 + 31.0,
        );
        let close = Rect::new(
            panel.x1 - 40.0,
            panel.y0 + 12.0,
            panel.x1 - 12.0,
            panel.y0 + 40.0,
        );
        fill_round(scene, tf, close, 8.0, c.surface_white_10);
        let (xw, xh) = tr.measure("✕", 14.0, None);
        text(
            scene,
            tr,
            tf,
            "✕",
            14.0,
            c.text_primary,
            close.center().x - f64::from(xw) / 2.0,
            close.center().y - f64::from(xh) / 2.0,
            TextWeight::Regular,
            None,
        );
        ChatApp::push_hit(hits, close, Hit::CloseThread);
        hline(
            scene,
            tf,
            panel.x0 + 12.0,
            panel.x1 - 12.0,
            head.y1,
            c.border_standard,
        );
        head
    }

    /// The thread body: the parent message, a "N replies" divider, then
    /// every reply in order — clipped to the scrollable region between the
    /// header and the thread composer.
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_thread_body(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        head: Rect,
        parent_id: &str,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let body = Rect::new(panel.x0, head.y1, panel.x1, panel.y1 - 70.0);
        push_clip(scene, tf, body);
        let tx = panel.x0 + 16.0 + 32.0 + 10.0;
        let tw = (panel.x1 - tx - 14.0).max(60.0) as f32;
        let mut y = body.y0 + 12.0;

        // parent message
        if let Some(parent) = self.message(parent_id).cloned() {
            y = self.draw_thread_message(
                scene,
                tf,
                tr,
                theme,
                &parent,
                panel.x0 + 16.0,
                tx,
                tw,
                y,
                hits,
            );
            // replies divider
            let n = model::reply_count(&self.messages, parent_id);
            let s = format!("{n} {}", if n == 1 { "reply" } else { "replies" });
            let (sw, _) = tr.measure(&s, 11.5, None);
            let dcy = y + 14.0;
            hline(
                scene,
                tf,
                panel.x0 + 16.0,
                panel.center().x - f64::from(sw) / 2.0 - 10.0,
                dcy,
                c.border_standard,
            );
            hline(
                scene,
                tf,
                panel.center().x + f64::from(sw) / 2.0 + 10.0,
                panel.x1 - 16.0,
                dcy,
                c.border_standard,
            );
            label(
                scene,
                tr,
                tf,
                &s,
                11.5,
                c.text_dim,
                panel.center().x - f64::from(sw) / 2.0,
                dcy - 7.0,
            );
            y += 30.0;
        }
        let replies: Vec<Message> = self
            .messages
            .iter()
            .filter(|m| m.parent.as_deref() == Some(parent_id))
            .cloned()
            .collect();
        for r in &replies {
            y = self.draw_thread_message(scene, tf, tr, theme, r, panel.x0 + 16.0, tx, tw, y, hits);
        }
        scene.pop_layer();
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_thread_message(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        m: &Message,
        avx: f64,
        tx: f64,
        tw: f32,
        y: f64,
        _hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let mut yy = y;
        if let Some(u) = self.user(&m.author) {
            draw_avatar(
                scene,
                tf,
                tr,
                theme,
                Point::new(avx + 16.0, yy + 14.0),
                14.0,
                &u.initials,
                u.color_idx,
                Some(u.status),
            );
        }
        let (nw, _) = text(
            scene,
            tr,
            tf,
            &self.user_name(&m.author),
            13.5,
            palette_color(theme, self.user(&m.author).map_or(0, |u| u.color_idx)),
            tx,
            yy,
            TextWeight::Semibold,
            None,
        );
        label(
            scene,
            tr,
            tf,
            &model::clock(m.ts),
            11.0,
            c.text_dim,
            tx + f64::from(nw) + 8.0,
            yy + 2.0,
        );
        yy += 19.0;
        if !m.text.is_empty() {
            let h = f64::from(tr.measure(&m.text, 14.0, Some(tw)).1);
            text(
                scene,
                tr,
                tf,
                &m.text,
                14.0,
                c.text_primary,
                tx,
                yy,
                TextWeight::Regular,
                Some(tw),
            );
            yy += h + 4.0;
        }
        if let Some(att) = &m.attachment {
            self.draw_attachment(scene, tf, tr, theme, att, tx, yy, tw);
            yy += match att {
                Attachment::File { .. } => 66.0,
                Attachment::Image { w, h, .. } => image_thumb_size(*w, *h, f64::from(tw)).1 + 24.0,
                Attachment::Video { w, h, .. } => video_card_size(*w, *h, f64::from(tw)).1 + 24.0,
            };
        }
        if !m.reactions.is_empty() {
            let mut x = tx;
            for r in &m.reactions {
                let lbl = format!("{} {}", r.emoji, r.count());
                let (lw, _) = tr.measure(&lbl, 11.5, None);
                let chip = Rect::new(x, yy, x + f64::from(lw) + 16.0, yy + 22.0);
                fill_round(scene, tf, chip, 11.0, c.surface_white_5);
                label(
                    scene,
                    tr,
                    tf,
                    &lbl,
                    11.5,
                    c.text_primary,
                    chip.x0 + 8.0,
                    chip.center().y - 7.0,
                );
                x = chip.x1 + 5.0;
            }
            yy += 28.0;
        }
        yy + 10.0
    }

    // ---------------------------------------------------------- members roster
    /// The workspace members roster — the other half of the right flap, shown
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
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            tf,
            c.border_standard,
            None,
            &vello::kurbo::Line::new(
                Point::new(panel.x0, panel.y0),
                Point::new(panel.x0, panel.y1),
            ),
        );

        let rows = self.roster_rows();
        let head_y1 = self.draw_members_header(scene, tf, tr, theme, panel, rows.len(), hits);
        self.draw_members_rows(scene, tf, tr, theme, panel, head_y1, &rows);
    }

    /// "Members" + the workspace name (with the roster count), and the
    /// close control. Returns the y-coordinate the scrollable roster starts
    /// beneath.
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_members_header(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        member_count: usize,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        label(
            scene,
            tr,
            tf,
            "Members",
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
        let sub = format!("{ws_name} · {member_count}");
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
        fill_round(scene, tf, close, 8.0, c.surface_white_10);
        let (xw, xh) = tr.measure("✕", 14.0, None);
        text(
            scene,
            tr,
            tf,
            "✕",
            14.0,
            c.text_primary,
            close.center().x - f64::from(xw) / 2.0,
            close.center().y - f64::from(xh) / 2.0,
            TextWeight::Regular,
            None,
        );
        ChatApp::push_hit(hits, close, Hit::ToggleRightFlap);
        let head_y1 = panel.y0 + 52.0;
        hline(
            scene,
            tf,
            panel.x0 + 12.0,
            panel.x1 - 12.0,
            head_y1,
            c.border_standard,
        );
        head_y1
    }

    /// The scrollable roster list beneath the header: each member's avatar,
    /// display name, and presence/voice status.
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_members_rows(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        head_y1: f64,
        rows: &[(crate::model::User, Option<String>)],
    ) {
        let c = &theme.tokens.colors;
        let body = Rect::new(panel.x0, head_y1, panel.x1, panel.y1);
        push_clip(scene, tf, body);
        let row_h = 46.0;
        let mut y = head_y1 + 8.0;
        for (u, voice) in rows {
            if y + row_h > panel.y1 {
                break;
            }
            let avc = Point::new(panel.x0 + 16.0 + 16.0, y + row_h / 2.0);
            draw_avatar(
                scene,
                tf,
                tr,
                theme,
                avc,
                16.0,
                &u.initials,
                u.color_idx,
                Some(u.status),
            );
            let nx = avc.x + 16.0 + 12.0;
            let display = if self.is_me(&u.id) {
                format!("{} (you)", u.name)
            } else {
                u.name.clone()
            };
            let name = truncate_to(tr, &display, 14.0, panel.x1 - nx - 14.0);
            text(
                scene,
                tr,
                tf,
                &name,
                14.0,
                c.text_primary,
                nx,
                avc.y - 14.0,
                TextWeight::Semibold,
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
        fill_round(
            scene,
            tf,
            panel.inflate(5.0, 5.0),
            18.0,
            Color::from_rgba8(0, 0, 0, 70),
        );
        fill_round(scene, tf, panel, 14.0, c.neutral[1]);
        stroke_round(scene, tf, panel, 14.0, c.border_white_10, 1.0);

        let head_h = 40.0;
        self.draw_attach_picker_header(scene, tf, tr, theme, panel, head_h);

        if self.attach_entries.is_empty() {
            Self::draw_attach_picker_empty(scene, tf, tr, theme, panel, head_h);
            return;
        }
        self.draw_attach_picker_rows(scene, tf, tr, theme, panel, head_h, hits);
    }

    /// The picker panel's rect, anchored above the composer's "+" and
    /// clamped to stay inside the window at any size or entry count.
    fn attach_picker_rect(&self, tr: &mut TextRenderer) -> Rect {
        let comp = self.composer_rect(tr);
        let w = 360.0_f64.min(self.rect.width() - 32.0).max(220.0);
        let x0 = comp
            .x0
            .min(self.rect.x1 - w - 12.0)
            .max(self.rect.x0 + 12.0);

        let head_h = 40.0;
        let row_h = 52.0;
        let max_rows = 6usize;
        let shown = self.attach_entries.len().min(max_rows);
        let body_h = if self.attach_entries.is_empty() {
            46.0
        } else {
            shown as f64 * row_h + 8.0
        };
        let more_h = if self.attach_entries.len() > max_rows {
            20.0
        } else {
            0.0
        };
        let panel_h = head_h + body_h + more_h + 8.0;
        let y1 = comp.y0 - 10.0;
        let y0 = (y1 - panel_h).max(self.rect.y0 + 12.0);
        Rect::new(x0, y0, x0 + w, y1)
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
        label(
            scene,
            tr,
            tf,
            "Share a file",
            13.5,
            c.text_primary,
            panel.x0 + 14.0,
            panel.y0 + 12.0,
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
            panel.x1 - f64::from(cw) - 14.0,
            panel.y0 + 14.0,
        );
        hline(
            scene,
            tf,
            panel.x0 + 12.0,
            panel.x1 - 12.0,
            panel.y0 + head_h - 4.0,
            c.border_standard,
        );
    }

    /// The "no media found" message shown in place of the row list when the
    /// workspace has nothing shareable.
    fn draw_attach_picker_empty(
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        head_h: f64,
    ) {
        let c = &theme.tokens.colors;
        let dir = crate::picker::attach_dir();
        let s = format!("No media in {}", dir.display());
        let s = truncate_to(tr, &s, 12.5, panel.width() - 28.0);
        label(
            scene,
            tr,
            tf,
            &s,
            12.5,
            c.text_dim,
            panel.x0 + 14.0,
            panel.y0 + head_h + 10.0,
        );
    }

    /// Up to `max_rows` shareable entries, each with a thumbnail, name, and
    /// size — plus a "+N more" footer note when the workspace has more than
    /// that.
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_attach_picker_rows(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        head_h: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let row_h = 52.0;
        let max_rows = 6usize;
        let mut y = panel.y0 + head_h + 4.0;
        for e in self.attach_entries.iter().take(max_rows) {
            let row = Rect::new(panel.x0 + 6.0, y, panel.x1 - 6.0, y + row_h - 6.0);
            if row.contains(self.last_mouse) {
                fill_round(scene, tf, row, 10.0, c.surface_white_5);
            }
            let thumb = Rect::new(row.x0 + 6.0, row.y0 + 5.0, row.x0 + 62.0, row.y1 - 5.0);
            let key = e.key();
            // A video shows a play glyph (no image decode); an image decodes its
            // real thumbnail through the shared cache, with a neutral tile until
            // the bytes hydrate.
            if e.is_video {
                fill_round(scene, tf, thumb, 8.0, c.neutral[2]);
                let (gw, gh) = tr.measure("🎬", 16.0, None);
                text(
                    scene,
                    tr,
                    tf,
                    "🎬",
                    16.0,
                    c.text_dim,
                    thumb.center().x - f64::from(gw) / 2.0,
                    thumb.center().y - f64::from(gh) / 2.0,
                    TextWeight::Regular,
                    None,
                );
            } else if let Some(img) = self.images.get(&key) {
                ui::image_store::paint_image_cover(scene, tf, thumb, &img, 8.0);
            } else {
                fill_round(scene, tf, thumb, 8.0, c.surfaces.raised);
                let (gw, gh) = tr.measure("🖼", 16.0, None);
                text(
                    scene,
                    tr,
                    tf,
                    "🖼",
                    16.0,
                    c.text_dim,
                    thumb.center().x - f64::from(gw) / 2.0,
                    thumb.center().y - f64::from(gh) / 2.0,
                    TextWeight::Regular,
                    None,
                );
            }
            stroke_round(scene, tf, thumb, 8.0, c.border_white_10, 1.0);
            let name = truncate_to(tr, &e.name, 13.5, row.width() - 96.0);
            label(
                scene,
                tr,
                tf,
                &name,
                13.5,
                c.text_primary,
                thumb.x1 + 12.0,
                row.center().y - 13.0,
            );
            label(
                scene,
                tr,
                tf,
                &human_size(e.size),
                11.0,
                c.text_dim,
                thumb.x1 + 12.0,
                row.center().y + 3.0,
            );
            ChatApp::push_hit(hits, row, Hit::AttachPick(key));
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

/// Per-row measured heights for the timeline layout.
struct RowM {
    idx_id: String,
    show_header: bool,
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
        let header = if self.show_header { 22.0 } else { 0.0 };
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

fn human_size(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if b < K {
        format!("{bytes} B")
    } else if b < K * K {
        format!("{:.1} KB", b / K)
    } else {
        format!("{:.1} MB", b / (K * K))
    }
}

/// The user-facing metadata line for a file-attachment chip: the file type
/// (derived from the display name's extension) and a human-readable size.
///
/// This deliberately carries **no filesystem path**. Where the bytes actually
/// live — the P2P FUSE workspace mount (`/var/workspace/...`) — is an
/// implementation detail; surfacing it in chrome is a HIG §4 violation
/// ("no internal / implementation labels in user-facing chrome … a leaked
/// `/var/workspace` path"). The chip already shows the human filename above
/// this line, which is the only path-ish thing a user cares about.
fn file_chip_meta(name: &str, size: u64) -> String {
    let ext = std::path::Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_uppercase())
        .filter(|e| !e.is_empty());
    match ext {
        Some(e) => format!("{e} · {}", human_size(size)),
        None => human_size(size),
    }
}

/// Horizontal centre of the workspace rail (column of square buttons).
const RAIL_W_CENTER: f64 = crate::app::RAIL_W / 2.0;

#[cfg(test)]
mod tests {
    use super::{file_chip_meta, human_size};

    /// Defect regression: the attachment chip must never leak the internal
    /// mount path. The demo's shared note lives at
    /// `/var/workspace/notes/moq-over-iroh.md` (see `doc.rs`), but its chip
    /// meta must show only the file type + size — no path, no "shared via".
    #[test]
    fn file_chip_meta_never_leaks_a_path() {
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
        assert_eq!(meta, "MD · 18.5 KB", "chip meta should be type + size only");
    }

    #[test]
    fn file_chip_meta_without_extension_is_just_the_size() {
        let meta = file_chip_meta("Makefile", 512);
        assert_eq!(meta, "512 B");
        assert!(!meta.contains('/'));
    }

    #[test]
    fn human_size_scales_bytes_kb_mb() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert!(human_size(3_000_000).ends_with(" MB"));
    }
}
