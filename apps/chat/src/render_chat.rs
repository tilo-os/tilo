//! Rendering for the persistent chat surface: workspace rail, channel
//! sidebar, channel header, the grouped message timeline, the composer,
//! and the thread side-panel. All geometry is computed from `self.rect`;
//! every clickable area is recorded into `hits`. Colours come only from
//! the [`Theme`].

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::icons::{draw_icon, Icon};
use ui::widget::Avatar;
use ui::ColorCompat;
use ui::Widget;

use crate::app::{ChatApp, Hit, MenuTarget, AVATAR};
use crate::model::{self, Attachment, ChannelKind, Message, Presence};
use crate::paint::*;

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
/// Tint strength alone could not carry that difference. On a dark surface a
/// quiet tint of a colour is nearly the surface, so the floor below has to
/// rescue it — and a rescued quiet band lands about where a loud one does,
/// leaving a mention of you looking like every other. Filled-versus-not
/// survives every ground.
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
        let ink = legible_on(own, ground, MENTION_TEXT_CONTRAST, theme.tokens.colors.text_primary);
        return (None, ink);
    }
    let band = band_at_least(
        mix(own, ground, MENTION_SELF_BAND_MIX),
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
        if s > cursor {
            spans.push(RichSpan::new(&text[cursor..s]));
        }
        let is_self = mention.user == app.me.id;
        let (band, ink) = mention_colors(app, &mention.user, is_self, theme, ground);
        spans.push(
            RichSpan::new(&text[s..e])
                .bold(true)
                .color(Some([ink.r(), ink.g(), ink.b()]))
                .background(band.map(|c| [c.r(), c.g(), c.b()])),
        );
        cursor = e;
    }
    if cursor < text.len() {
        spans.push(RichSpan::new(&text[cursor..]));
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
    s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len())
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
            let fill = if active { palette_color(theme, ws.color_idx) } else { c.neutral[2] };
            fill_round(scene, tf, b, radius, fill);
            let (tw, th) = tr.measure_weighted(&ws.glyph, 17.0, None, TextWeight::Bold);
            let col = if active { Color::WHITE } else { c.text_dim };
            text(
                scene, tr, tf, &ws.glyph, 17.0, col,
                b.center().x - tw as f64 / 2.0,
                b.center().y - th as f64 / 2.0,
                TextWeight::Bold, None,
            );
            ChatApp::push_hit(hits, b, Hit::Workspace(ws.id.clone()));
            y += btn + 12.0;
        }

        // add-workspace affordance — creates a new workspace and switches to it.
        let add = Rect::new(cx - btn / 2.0, y, cx + btn / 2.0, y + btn);
        fill_round(scene, tf, add, btn / 2.0, c.neutral[2]);
        let (aw, ah) = tr.measure_weighted("+", 24.0, None, TextWeight::Regular);
        text(
            scene, tr, tf, "+", 24.0, c.semantic.success,
            add.center().x - aw as f64 / 2.0, add.center().y - ah as f64 / 2.0,
            TextWeight::Regular, None,
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

        // workspace title header
        let ws_name = self
            .workspaces
            .iter()
            .find(|w| w.id == self.active_ws)
            .map(|w| w.name.clone())
            .unwrap_or_default();
        let header = Rect::new(side.x0, side.y0, side.x1, side.y0 + 52.0);
        let ws_title = truncate_to(tr, &ws_name, 16.0, side.width() - 60.0);
        label(scene, tr, tf, &ws_title, 16.0, c.text_primary, side.x0 + 16.0, side.y0 + 17.0);
        // a tiny "presence count" pill
        let online = self.users.iter().filter(|u| u.status == Presence::Online).count();
        let pill_txt = format!("{online} online");
        let (pw, _) = tr.measure(&pill_txt, 10.5, None);
        let pill = Rect::new(side.x1 - pw as f64 - 28.0, side.y0 + 16.0, side.x1 - 12.0, side.y0 + 34.0);
        fill_round(scene, tf, pill, 9.0, c.surface_white_10);
        fill_circle(scene, tf, Point::new(pill.x0 + 10.0, pill.center().y), 3.0, c.semantic.success);
        label(scene, tr, tf, &pill_txt, 10.5, c.text_dim, pill.x0 + 17.0, pill.center().y - 7.0);
        hline(scene, tf, side.x0 + 12.0, side.x1 - 12.0, header.y1, c.border_standard);

        // "+ category" affordance below the workspace title.
        let add_cat = Rect::new(side.x0 + 12.0, header.y1 + 6.0, side.x0 + 118.0, header.y1 + 28.0);
        fill_round(scene, tf, add_cat, 7.0, c.surface_white_5);
        label(scene, tr, tf, "＋ Category", 11.0, c.text_dim, add_cat.x0 + 9.0, add_cat.center().y - 7.0);
        ChatApp::push_hit(hits, add_cat, Hit::AddCategory(self.active_ws.clone()));

        // The channel tree: an uncategorised group first, then each category
        // with a collapse caret, a "+" to add a channel, and its channels.
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
            label(scene, tr, tf, caret, 10.0, c.text_dim, hrow.x0 + 4.0, y + 5.0);
            // Inline rename for a category, else its name.
            let renaming_cat = matches!(&self.rename, Some((MenuTarget::Category(id), _)) if id == &cat.id);
            if renaming_cat {
                if let Some((_, txt)) = &self.rename {
                    self.draw_inline_rename(scene, tf, tr, theme, hrow.x0 + 18.0, y + 4.0, side.x1 - 60.0, txt, hits);
                }
            } else {
                let name = truncate_to(tr, &cat.name.to_uppercase(), 10.5, hrow.width() - 60.0);
                text(scene, tr, tf, &name, 10.5, c.text_dim, hrow.x0 + 18.0, y + 5.0, TextWeight::Semibold, None);
            }
            // "+" add-channel button.
            let plus = Rect::new(hrow.x1 - 22.0, y + 2.0, hrow.x1 - 4.0, y + 20.0);
            label(scene, tr, tf, "+", 16.0, c.text_dim, plus.x0 + 4.0, y + 1.0);
            ChatApp::push_hit(hits, plus, Hit::AddChannel { workspace: self.active_ws.clone(), category: cat.id.clone() });
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

        // self identity bar pinned to the sidebar bottom — the SINGLE user
        // avatar lives here (mute + deafen toggles to its right).
        let bar = Rect::new(side.x0, side.y1 - 56.0, side.x1, side.y1);
        fill_rect(scene, tf, bar, c.neutral[0]);
        if let Some(me) = self.user(&self.me.id) {
            // The room's answer, not the copy the document happens to hold. A
            // peer does not write its own name there, so the document's copy is
            // empty on a real first run and this bar drew a blank monogram
            // beside a blank name.
            let name = self.chrome_name(&self.me.id);
            let initials = ui::widget::Avatar::initials_for_name(&name);
            draw_avatar(scene, tf, tr, theme, Point::new(bar.x0 + 28.0, bar.center().y), 16.0, &initials, me.color_idx, Some(me.status));
            label(scene, tr, tf, &name, 13.0, c.text_primary, bar.x0 + 50.0, bar.center().y - 14.0);
            let sub = if self.call.active { "In voice" } else { "Available" };
            label(scene, tr, tf, sub, 11.0, c.text_dim, bar.x0 + 50.0, bar.center().y + 2.0);
        }
        // Deafen + mute toggles — only while in a call, where they write
        // through to the shared voice membership (`sync_local_voice`). Outside
        // a call there is no mic stream to gate, so the controls are hidden
        // rather than presented as toggling something they don't.
        if self.call.active {
            let deaf = Rect::new(bar.x1 - 80.0, bar.center().y - 16.0, bar.x1 - 48.0, bar.center().y + 16.0);
            fill_round(scene, tf, deaf, 8.0, if self.call.deafened { c.semantic.danger } else { c.surface_white_10 });
            let dglyph = if self.call.deafened { "🔕" } else { "🎧" };
            let (dw, dh) = tr.measure(dglyph, 14.0, None);
            text(scene, tr, tf, dglyph, 14.0, c.text_primary, deaf.center().x - dw as f64 / 2.0, deaf.center().y - dh as f64 / 2.0, TextWeight::Regular, None);
            ChatApp::push_hit(hits, deaf, Hit::CallDeafen);

            let mute = Rect::new(bar.x1 - 44.0, bar.center().y - 16.0, bar.x1 - 12.0, bar.center().y + 16.0);
            fill_round(scene, tf, mute, 8.0, if self.call.muted { c.semantic.danger } else { c.surface_white_10 });
            let mglyph = if self.call.muted { "🔇" } else { "🎤" };
            let (mw, mh) = tr.measure(mglyph, 14.0, None);
            text(scene, tr, tf, mglyph, 14.0, c.text_primary, mute.center().x - mw as f64 / 2.0, mute.center().y - mh as f64 / 2.0, TextWeight::Regular, None);
            ChatApp::push_hit(hits, mute, Hit::CallMute);
        }
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
        let c = &theme.tokens.colors;
        let cat_key = category.unwrap_or("");
        let channels = self.category_channels(cat_key);
        // Index for swipe parity: position among all ws_channels.
        let ws_channels = self.ws_channels();
        for ch in channels {
            // Swipe offset: shift this row left while it's being swiped, and
            // reveal a red delete affordance behind it.
            let swipe_dx = self
                .swipe
                .filter(|s| ws_channels.get(s.channel_index).map(|cc| cc.id.as_str()) == Some(ch.id.as_str()))
                .map(|s| s.dx())
                .unwrap_or(0.0);
            let row = Rect::new(side.x0 + 8.0 - swipe_dx, y, side.x1 - 8.0 - swipe_dx, y + 32.0);
            if swipe_dx > 1.0 {
                let trash = Rect::new(side.x1 - 8.0 - swipe_dx + 6.0, y, side.x1 - 8.0, y + 32.0);
                fill_round(scene, tf, Rect::new(side.x0 + 8.0, y, side.x1 - 8.0, y + 32.0), 8.0, c.semantic.danger.with_alpha_factor(0.9));
                let _ = trash;
                label(scene, tr, tf, "🗑", 14.0, Color::WHITE, side.x1 - 30.0, y + 8.0);
            }
            let is_active = ch.id == self.active_channel && ch.kind == ChannelKind::Text;
            let is_live = self.call.active && self.call.channel == ch.id;
            if is_active {
                fill_round(scene, tf, row, 8.0, c.surface_white_10);
                let bar = Rect::new(side.x0, y + 6.0, side.x0 + 4.0, y + 26.0);
                fill_round(scene, tf, bar, 2.0, c.accent[5]);
            }
            let glyph = ch.kind.glyph();
            let gcol = if is_active { c.accent[6] } else { c.text_dim };
            label(scene, tr, tf, glyph, 14.0, gcol, row.x0 + 10.0, y + 8.0);

            // Inline rename for a channel, else its name.
            let renaming = matches!(&self.rename, Some((MenuTarget::Channel(id), _)) if id == &ch.id);
            if renaming {
                if let Some((_, txt)) = &self.rename {
                    self.draw_inline_rename(scene, tf, tr, theme, row.x0 + 32.0, y + 6.0, row.width() - 80.0, txt, hits);
                }
            } else {
                let name_col = if is_active || ch.unread > 0 { c.text_primary } else { c.text_dim };
                let weight = if ch.unread > 0 { TextWeight::Semibold } else { TextWeight::Regular };
                let name = truncate_to(tr, &ch.name, 14.0, row.width() - 80.0);
                text(scene, tr, tf, &name, 14.0, name_col, row.x0 + 32.0, y + 8.0, weight, None);
            }

            if is_live {
                fill_circle(scene, tf, Point::new(row.x1 - 16.0, row.center().y), 4.0, c.semantic.success);
            } else if ch.unread > 0 {
                let txt = ch.unread.to_string();
                let (bw, _) = tr.measure(&txt, 11.0, None);
                let badge = Rect::new(row.x1 - bw as f64 - 22.0, y + 7.0, row.x1 - 8.0, y + 25.0);
                fill_round(scene, tf, badge, 9.0, c.accent[5]);
                label(scene, tr, tf, &txt, 11.0, Color::WHITE, badge.x0 + 7.0, badge.center().y - 7.0);
            }
            // Click joins/opens; right-click (resolved against this row's
            // click hit in `on_right_click`) opens the channel context menu.
            let click_hit = if ch.kind == ChannelKind::Voice { Hit::VoiceChannel(ch.id.clone()) } else { Hit::Channel(ch.id.clone()) };
            ChatApp::push_hit(hits, row, click_hit);
            y += 34.0;

            // Voice presence: list connected members under a voice channel.
            if ch.kind == ChannelKind::Voice {
                for m in self.voice_in(&ch.id) {
                    if let Some(u) = self.user(&m.user) {
                        let mrow = Rect::new(side.x0 + 36.0, y, side.x1 - 8.0, y + 24.0);
                        let r = 9.0;
                        let cc = Point::new(mrow.x0 + r + 2.0, mrow.center().y);
                        if m.talking && !m.muted {
                            stroke_circle(scene, tf, cc, r + 2.5, c.semantic.success, 2.0);
                        }
                        draw_avatar(scene, tf, tr, theme, cc, r, &self.initials_for(&u.id), u.color_idx, None);
                        let nm = truncate_to(tr, &self.chrome_name(&u.id), 12.0, mrow.width() - 60.0);
                        let ncol = if m.talking && !m.muted { c.text_primary } else { c.text_dim };
                        label(scene, tr, tf, &nm, 12.0, ncol, mrow.x0 + 2.0 * r + 8.0, mrow.center().y - 7.0);
                        // mic / audio-only state glyphs on the right.
                        if m.muted {
                            label(scene, tr, tf, "🔇", 11.0, c.semantic.danger, mrow.x1 - 18.0, mrow.center().y - 7.0);
                        } else if m.audio_only {
                            label(scene, tr, tf, "🎧", 11.0, c.text_dim, mrow.x1 - 18.0, mrow.center().y - 7.0);
                        }
                        y += 26.0;
                    }
                }
            }
        }
        y
    }

    /// Draw the inline rename editor field (a bordered box + the working
    /// text + a caret), recording a confirm hit on it.
    #[allow(clippy::too_many_arguments)]
    fn draw_inline_rename(
        &self,
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
        let (tw, _) = text(scene, tr, tf, value, 13.0, c.text_primary, x, y, TextWeight::Regular, Some((w - 8.0) as f32));
        let caret = Rect::new(x + tw as f64 + 1.0, y, x + tw as f64 + 2.4, y + 16.0);
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
        let left_inset = if self.left_flap_open() && self.left_mode == crate::responsive::FlapMode::Docked {
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
        let call_label = if self.call.active { "● In call" } else { "📹 Call" };
        label(scene, tr, tf, call_label, 12.5, ctx, call_btn.x0 + 12.0, call_btn.center().y - 8.0);
        ChatApp::push_hit(hits, call_btn, Hit::StartCall);

        // members pill — toggles the right flap (members / thread).
        let mem_x0 = call_x0 - gap - mem_w;
        let mem = Rect::new(mem_x0, cy - 16.0, mem_x0 + mem_w, cy + 16.0);
        let mem_on = self.right_flap_open();
        fill_round(scene, tf, mem, 8.0, if mem_on { c.accent[5].with_alpha_factor(0.22) } else { c.surface_white_5 });
        label(scene, tr, tf, &format!("👥 {members}"), 12.5, if mem_on { c.accent[7] } else { c.text_dim }, mem.x0 + 9.0, mem.center().y - 8.0);
        ChatApp::push_hit(hits, mem, Hit::ToggleRightFlap);

        // Title + topic occupy the space left of the button cluster; both
        // truncate to that width so nothing runs under the buttons.
        let cluster_left = mem_x0;
        label(scene, tr, tf, glyph, 18.0, c.text_dim, h.x0 + left_inset, cy - 11.0);
        let nx = h.x0 + left_inset + 22.0;
        let name_max = (cluster_left - 12.0 - nx).max(0.0);
        let name_t = truncate_to(tr, &name, 17.0, name_max);
        let (nw, _) = text(scene, tr, tf, &name_t, 17.0, c.text_primary, nx, cy - 11.0, TextWeight::Bold, None);
        if !topic.is_empty() {
            let tx = nx + nw as f64 + 18.0;
            let avail = cluster_left - 12.0 - tx;
            if avail >= 48.0 {
                let topic_t = truncate_to(tr, &topic, 13.0, avail);
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
        let docked_open = self.left_flap_open() && self.left_mode == crate::responsive::FlapMode::Docked;
        if !docked_open {
            let h = self.header_rect();
            let btn = Rect::new(h.x0 + 12.0, h.center().y - 16.0, h.x0 + 44.0, h.center().y + 16.0);
            fill_round(scene, tf, btn, 8.0, c.surface_white_10);
            label(scene, tr, tf, "☰", 16.0, c.text_primary, btn.x0 + 8.0, btn.center().y - 9.0);
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
        let h = 10.0 + menu.items.len() as f64 * item_h + 6.0;
        let x0 = menu.anchor.x.clamp(self.rect.x0 + 6.0, self.rect.x1 - w - 6.0);
        let y0 = menu.anchor.y.clamp(self.rect.y0 + 6.0, (self.rect.y1 - h - 6.0).max(self.rect.y0 + 6.0));
        let panel = Rect::new(x0, y0, x0 + w, y0 + h);
        fill_round(scene, tf, panel.inflate(5.0, 5.0), 16.0, Color::from_rgba8(0, 0, 0, 80));
        fill_round(scene, tf, panel, 12.0, c.neutral[1]);
        stroke_round(scene, tf, panel, 12.0, c.border_white_10, 1.0);

        let mut y = panel.y0 + 6.0;
        for item in &menu.items {
            let r = Rect::new(panel.x0 + 5.0, y, panel.x1 - 5.0, y + item_h - 3.0);
            let hot = r.contains(self.last_mouse);
            if hot {
                fill_round(scene, tf, r, 8.0, c.surface_white_10);
            }
            let col = if item.is_destructive() { c.semantic.danger } else { c.text_primary };
            label(scene, tr, tf, item.label(), 13.0, col, r.x0 + 12.0, r.center().y - 8.0);
            ChatApp::push_hit(hits, r, Hit::MenuAction { target: menu.target.clone(), action: *item });
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
        let c = &theme.tokens.colors;
        let text_x = area.x0 + 16.0 + AVATAR + 14.0;
        let text_w = (area.x1 - text_x - 18.0).max(80.0) as f32;

        // Empty channel: a quiet start-of-conversation marker rather than a
        // blank void, so a fresh channel reads as ready, not broken.
        if self.messages.is_empty() {
            let ch_name = self.channel(&self.active_channel).map(|c| c.name.clone()).unwrap_or_default();
            let cx = area.center().x;
            let cy = area.y0 + (area.height() * 0.5).min(220.0);
            fill_circle(scene, tf, Point::new(cx, cy), 30.0, c.surface_white_5);
            let (gw, gh) = tr.measure_weighted("#", 30.0, None, TextWeight::Bold);
            text(scene, tr, tf, "#", 30.0, c.text_dim, cx - gw as f64 / 2.0, cy - gh as f64 / 2.0, TextWeight::Bold, None);
            let title = format!("#{ch_name}");
            let (tw, _) = tr.measure_weighted(&title, 20.0, None, TextWeight::Bold);
            text(scene, tr, tf, &title, 20.0, c.text_primary, cx - tw as f64 / 2.0, cy + 46.0, TextWeight::Bold, None);
            let sub = format!("This is the start of the {title} channel.");
            let (sw, _) = tr.measure(&sub, 13.0, None);
            label(scene, tr, tf, &sub, 13.0, c.text_dim, cx - sw as f64 / 2.0, cy + 76.0);
            return;
        }

        let groups = model::group_messages(&self.messages);

        // Pass 1: measure total height (divider + rows).
        let divider_h = 36.0;
        let mut total = divider_h;
        let mut rows: Vec<RowM> = Vec::new();
        for g in &groups {
            for (gi, &idx) in g.message_indices.iter().enumerate() {
                let m = &self.messages[idx];
                let show_header = gi == 0;
                let rm = self.measure_row(tr, theme, m, show_header, text_w);
                total += rm.height();
                rows.push(rm);
            }
        }

        let start_y = if total > area.height() { area.y1 - total } else { area.y0 };
        let max_start = area.y0;
        let min_start = (area.y1 - total).min(area.y0);
        let mut y = (start_y + self.scroll).clamp(min_start, max_start);

        // date divider
        let dl = self
            .messages
            .first()
            .map(|m| model::day_label(m.ts, crate::doc::NOW))
            .unwrap_or_else(|| "Today".into());
        let (dw, _) = tr.measure(&dl, 11.5, None);
        let dcy = y + divider_h / 2.0;
        hline(scene, tf, area.x0 + 24.0, area.center().x - dw as f64 / 2.0 - 14.0, dcy, c.border_standard);
        hline(scene, tf, area.center().x + dw as f64 / 2.0 + 14.0, area.x1 - 24.0, dcy, c.border_standard);
        label(scene, tr, tf, &dl, 11.5, c.text_dim, area.center().x - dw as f64 / 2.0, dcy - 7.0);
        y += divider_h;

        for rm in &rows {
            self.draw_row(scene, tf, tr, theme, rm, area, text_x, text_w, &mut y, hits);
        }
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
            let t = self.editing.as_ref().map(|(_, t)| t.as_str()).unwrap_or("");
            (tr.measure(t, 15.0, Some(text_w)).1 as f64).max(19.0)
        } else if m.text.is_empty() {
            0.0
        } else if m.mentions.is_empty() {
            tr.measure(&m.text, BODY_SIZE, Some(text_w)).1 as f64
        } else {
            // A mention-carrying body is laid out by the rich path, so it must
            // be measured by the rich path too — measuring one and painting the
            // other is how a row ends up clipping its own last line.
            let spans = body_spans(self, m, theme, theme.tokens.colors.surfaces.base);
            shape_body(tr, &spans, text_w).height() as f64
        };
        let att_h = match &m.attachment {
            None => 0.0,
            Some(Attachment::File { .. }) => 70.0,
            Some(Attachment::Image { w, h, .. }) => image_thumb_size(*w, *h, text_w as f64).1 + 24.0,
            Some(Attachment::Video { w, h, .. }) => video_card_size(*w, *h, text_w as f64).1 + 24.0,
        };
        let react_h = if m.reactions.is_empty() { 0.0 } else { 32.0 };
        // While editing, the "(edited)" marker is suppressed in favour of the
        // editor's own controls row.
        let edited_h = if m.edited && !editing { 18.0 } else { 0.0 };
        let thread_h = if model::reply_count(&self.messages, &m.id) > 0 { 46.0 } else { 0.0 };
        let edit_ctrl_h = if editing { 30.0 } else { 0.0 };
        // The chip stays up while the body is being edited: what a message is
        // about does not change because its wording is being reworked.
        let ref_h = if m.reference.is_some() { REF_CHIP_H } else { 0.0 };
        // The strip belongs to the message that was *addressed*, not to the
        // agent's reply: what a person needs to see is that the sentence they
        // just typed was picked up, and they are looking at their own sentence
        // in the timeline, not at a thread they have not opened.
        let work_h = match self.agent_record(&m.id) {
            Some(reply) => self.work_strip_height(tr, reply, text_w, true),
            None => 0.0,
        };
        RowM {
            idx_id: m.id.clone(),
            show_header,
            body_h,
            att_h,
            react_h,
            edited_h,
            ref_h,
            work_h,
            thread_h,
            editing,
            edit_ctrl_h,
        }
    }

    #[allow(clippy::too_many_arguments)]
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
        let c = &theme.tokens.colors;
        let Some(m) = self.message(&rm.idx_id) else { return };
        let top_gap = if rm.show_header { 14.0 } else { 3.0 };
        *y += top_gap;
        // Top of the header + body region — the right-click target for your
        // own messages (recorded below, beneath the reaction / thread hits).
        let content_top = *y;

        if rm.show_header {
            let u = self.user(&m.author).cloned();
            let (initials, cidx, status) = u
                .as_ref()
                .map(|u| (self.initials_for(&u.id), u.color_idx, Some(u.status)))
                .unwrap_or(("?".into(), 0, None));
            draw_avatar(
                scene, tf, tr, theme,
                Point::new(area.x0 + 16.0 + AVATAR / 2.0, *y + AVATAR / 2.0),
                AVATAR / 2.0, &initials, cidx, status,
            );
            let name = self.user_name(&m.author);
            let (nw, _) = text(scene, tr, tf, &name, 14.5, palette_color(theme, cidx), text_x, *y, TextWeight::Semibold, None);
            label(scene, tr, tf, &model::clock(m.ts), 11.5, c.text_dim, text_x + nw as f64 + 10.0, *y + 3.0);
            *y += 22.0;
        }

        // What the message is about, above what was said about it: the chip is
        // the context the sentence is read in, and a reader who meets the words
        // first has already had to guess what "this" meant.
        if let Some(r) = &m.reference {
            self.draw_reference_chip(scene, tf, tr, theme, r, text_x, *y, text_w);
            *y += rm.ref_h;
        }

        if rm.editing {
            self.draw_message_editor(scene, tf, tr, theme, text_x, text_w, rm, y, hits);
        } else {
            if !m.text.is_empty() {
                if m.mentions.is_empty() {
                    text(scene, tr, tf, &m.text, BODY_SIZE, c.text_primary, text_x, *y, TextWeight::Regular, Some(text_w));
                } else {
                    // Mentions are runs inside the paragraph, so the whole body
                    // goes through the attributed layout — one wrap, one line
                    // grid, bands painted behind the glyphs they belong to.
                    let spans = body_spans(self, m, theme, c.surfaces.base);
                    let layout = shape_body(tr, &spans, text_w);
                    tr.draw_rich(scene, &layout, tf * Affine::translate((text_x, *y)), c.text_primary);
                }
                *y += rm.body_h + 2.0;
            }
            // Right-click your own message (header + body) opens the edit menu.
            // Recorded now, before the reaction / thread hits so those still
            // win in their own sub-regions.
            if m.author == self.me.id {
                ChatApp::push_hit(hits, Rect::new(area.x0, content_top, area.x1, *y), Hit::MessageMenu { msg: m.id.clone() });
            }
        }

        if m.edited && !rm.editing {
            let s = "(edited) · view history";
            let r = Rect::new(text_x, *y, text_x + tr.measure(s, 11.5, None).0 as f64 + 4.0, *y + 16.0);
            label(scene, tr, tf, s, 11.5, c.text_dim, text_x, *y, );
            ChatApp::push_hit(hits, r, Hit::EditHistory { msg: m.id.clone(), anchor: Point::new(text_x, *y + 18.0) });
            *y += rm.edited_h;
        }

        if let Some(att) = &m.attachment {
            self.draw_attachment(scene, tf, tr, theme, att, text_x, *y, text_w);
            *y += rm.att_h;
        }

        if rm.work_h > 0.0 {
            if let Some(reply) = self.agent_record(&m.id) {
                self.draw_work_strip(scene, tf, tr, theme, reply, text_x, *y, text_w, true, hits);
                *y += rm.work_h;
            }
        }

        if !m.reactions.is_empty() {
            self.draw_reactions(scene, tf, tr, theme, m, text_x, *y, hits);
            *y += rm.react_h;
        } else {
            // empty reaction strip still gets an add affordance on header rows
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
        let editing_text = self.editing.as_ref().map(|(_, t)| t.as_str()).unwrap_or("");
        let box_h = rm.body_h + EDIT_BOX_PAD;
        let er = Rect::new(text_x - 8.0, *y, text_x + text_w as f64 + 8.0, *y + box_h);
        fill_round(scene, tf, er, 10.0, c.neutral[2]);
        stroke_round(scene, tf, er, 10.0, c.border_focus, 1.6);
        if editing_text.is_empty() {
            label(scene, tr, tf, "Edit message…", 15.0, c.text_dim, text_x, *y + 8.0);
        } else {
            text(scene, tr, tf, editing_text, 15.0, c.text_primary, text_x, *y + 8.0, TextWeight::Regular, Some(text_w));
        }
        let prefix = &editing_text[..char_to_byte(editing_text, self.caret)];
        let cx = (text_x + tr.measure(prefix, 15.0, None).0 as f64).min(text_x + text_w as f64);
        fill_rect(scene, tf, Rect::new(cx, *y + 8.0, cx + 1.6, *y + 27.0), c.border_focus);
        *y += box_h;

        // Controls row: keyboard hint on the left, Cancel + Save on the right.
        let cy = *y + 2.0;
        label(scene, tr, tf, "Enter to save · Esc to cancel", 11.0, c.text_dim, text_x, cy + 4.0);
        let save = Rect::new(er.x1 - 56.0, cy, er.x1, cy + 24.0);
        fill_round(scene, tf, save, 7.0, c.accent[5]);
        let (sw, sh) = tr.measure_weighted("Save", 12.5, None, TextWeight::Semibold);
        text(scene, tr, tf, "Save", 12.5, Color::WHITE, save.center().x - sw as f64 / 2.0, save.center().y - sh as f64 / 2.0, TextWeight::Semibold, None);
        ChatApp::push_hit(hits, save, Hit::EditSave);
        let cancel = Rect::new(save.x0 - 8.0 - 62.0, cy, save.x0 - 8.0, cy + 24.0);
        fill_round(scene, tf, cancel, 7.0, c.surface_white_10);
        let (kw, kh) = tr.measure("Cancel", 12.5, None);
        text(scene, tr, tf, "Cancel", 12.5, c.text_primary, cancel.center().x - kw as f64 / 2.0, cancel.center().y - kh as f64 / 2.0, TextWeight::Regular, None);
        ChatApp::push_hit(hits, cancel, Hit::EditCancel);
        *y += rm.edit_ctrl_h;
    }

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
        let h = 26.0;
        for r in &m.reactions {
            let mine = r.reacted_by(&self.me.id);
            let lbl = format!("{}  {}", r.emoji, r.count());
            let (lw, _) = tr.measure(&lbl, 12.5, None);
            let chip = Rect::new(x, y, x + lw as f64 + 20.0, y + h);
            let bg = if mine { c.accent[5].with_alpha_factor(0.22) } else { c.surface_white_5 };
            fill_round(scene, tf, chip, 13.0, bg);
            if mine {
                stroke_round(scene, tf, chip, 13.0, c.accent[5], 1.0);
            }
            label(scene, tr, tf, &lbl, 12.5, if mine { c.accent[7] } else { c.text_primary }, chip.x0 + 10.0, chip.center().y - 8.0);
            ChatApp::push_hit(hits, chip, Hit::Reaction { msg: m.id.clone(), emoji: r.emoji.clone() });
            x = chip.x1 + 6.0;
        }
        // add-reaction affordance
        let add = Rect::new(x, y, x + 34.0, y + h);
        fill_round(scene, tf, add, 13.0, c.surface_white_5);
        let (aw, ah) = tr.measure("☺", 14.0, None);
        text(scene, tr, tf, "☺", 14.0, c.text_dim, add.center().x - aw as f64 / 2.0, add.center().y - ah as f64 / 2.0, TextWeight::Regular, None);
        ChatApp::push_hit(hits, add, Hit::AddReaction { msg: m.id.clone(), anchor: Point::new(add.x0, y + h + 4.0) });
    }

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
        let n = model::reply_count(&self.messages, &m.id);
        let parts = model::thread_participants(&self.messages, &m.id);
        let bar = Rect::new(text_x, y, text_x + (text_w as f64).min(360.0), y + 38.0);
        let open = self.thread_open.as_deref() == Some(m.id.as_str());
        fill_round(scene, tf, bar, 10.0, if open { c.accent[5].with_alpha_factor(0.14) } else { c.surface_white_5 });
        if open {
            stroke_round(scene, tf, bar, 10.0, c.accent[5].with_alpha_factor(0.5), 1.0);
        }
        let mut ax = bar.x0 + 14.0;
        for pid in parts.iter().take(3) {
            if let Some(u) = self.user(pid) {
                fill_circle(scene, tf, Point::new(ax + 11.0, bar.center().y), 13.0, c.surfaces.raised);
                draw_avatar(scene, tf, tr, theme, Point::new(ax + 11.0, bar.center().y), 11.0, &self.initials_for(&u.id), u.color_idx, None);
            }
            ax += 17.0;
        }
        let txt = format!("{n} {}", if n == 1 { "reply" } else { "replies" });
        label(scene, tr, tf, &txt, 13.0, c.accent[6], ax + 8.0, bar.center().y - 8.0);
        let last_ts = self
            .messages
            .iter()
            .filter(|r| r.parent.as_deref() == Some(m.id.as_str()))
            .map(|r| r.ts)
            .max();
        if let Some(ts) = last_ts {
            let s = format!("last reply {}", model::clock(ts));
            let sx = bar.x1 - tr.measure(&s, 11.0, None).0 as f64 - 14.0;
            label(scene, tr, tf, &s, 11.0, c.text_dim, sx, bar.center().y - 6.0);
        }
        ChatApp::push_hit(hits, bar, Hit::OpenThread(m.id.clone()));
    }

    /// The chip on a message written *about* something — a quiet accent pill
    /// naming what the author had selected when they said it.
    ///
    /// Deliberately not styled like the attachment card. An attachment is a
    /// thing carried *with* the message; a reference points at something that
    /// already lives somewhere else, and a card would claim the content is
    /// here. A pill reads as a pointer.
    #[allow(clippy::too_many_arguments)]
    fn draw_reference_chip(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &qos_reference::ResourceRef,
        x: f64,
        y: f64,
        max_w: f32,
    ) {
        let ground = theme.tokens.colors.surfaces.base;
        let (fill, edge, ink) = reference_chip_colors(theme, ground);
        let body = reference_chip_label(r);
        // A reference to part of a resource says so. A remark about three lines
        // of a document is not a remark about the document, and a chip that
        // spelled both the same way would lose the only distinction the author
        // took the trouble to make.
        let prefix = if r.is_partial() { "in " } else { "" };
        // The pill's own padding, so the text is truncated to what will fit
        // inside it rather than to the column width.
        let pad = 10.0;
        let avail = (max_w as f64 - pad * 2.0 - 4.0).max(0.0);
        let s = format!("{prefix}{body}");
        let s = truncate_to(tr, &s, REF_CHIP_SIZE, avail);
        let (tw, _) = tr.measure(&s, REF_CHIP_SIZE, None);
        let pill = Rect::new(x, y, x + tw as f64 + pad * 2.0, y + 20.0);
        fill_round(scene, tf, pill, 10.0, fill);
        stroke_round(scene, tf, pill, 10.0, edge, 1.0);
        label(scene, tr, tf, &s, REF_CHIP_SIZE, ink, pill.x0 + pad, pill.center().y - 8.0);
    }


    /// The wording of `reply`'s record and the exact box it occupies.
    ///
    /// One function so the measure pass and the draw pass cannot disagree
    /// about the height — the fault that clips a row's last line.
    ///
    /// `with_reason` is false where the reply's own body is already on screen
    /// — inside the thread, the strip sits directly under the sentence it
    /// would otherwise repeat.
    fn work_strip_of(
        &self,
        tr: &mut TextRenderer,
        reply: &Message,
        text_w: f32,
        with_reason: bool,
    ) -> Option<WorkStrip> {
        let work = reply.work.as_ref()?;
        let line = work_line(
            work,
            &self.agent_label(reply),
            if with_reason { reply.text.trim() } else { "" },
            self.agent_present(&reply.author),
        );
        let inner = (text_w as f64 - WORK_PAD_X * 2.0).max(40.0) as f32;
        let detail_h = if line.detail.is_empty() {
            0.0
        } else {
            tr.measure(&line.detail, WORK_TEXT_SIZE, Some(inner)).1 as f64 + 3.0
        };

        // One row per operation the agent dispatched. The list is whatever the
        // agent wrote and nothing else — this function has no way to add a row
        // and does not look at the reply's text to find one.
        let rows: Vec<TraceRow> = work
            .trace
            .iter()
            .map(|s| trace_row(s, &line.detail))
            .collect();
        let rows_h = if rows.is_empty() {
            0.0
        } else {
            WORK_SECTION_GAP + rows.len() as f64 * WORK_ROW_H
        };

        // Collapsed by default: the question "how did it decide" is one a
        // reader asks about a particular turn, and a transcript that answered
        // it for every turn at once would be unreadable.
        let open = self.reasoning_open(&reply.id);
        let reason = work.reasoning.clone();
        let (reason_head_h, reason_body_h) = match &reason {
            None => (0.0, 0.0),
            Some(r) => {
                let head = WORK_SECTION_GAP + WORK_ROW_H;
                let body = if open && !r.why.trim().is_empty() {
                    // Indented under the line that opened it, so the body is
                    // measured against the width it will actually wrap to.
                    let w = (inner as f64 - WORK_ICON - WORK_ICON_GAP).max(40.0) as f32;
                    tr.measure(r.why.trim(), WORK_TEXT_SIZE, Some(w)).1 as f64 + 3.0
                } else {
                    0.0
                };
                (head, body)
            }
        };

        let box_h = WORK_PAD_Y * 2.0 + WORK_HEAD_H + detail_h + rows_h + reason_head_h + reason_body_h;
        Some(WorkStrip {
            line,
            detail_h,
            rows,
            reason,
            reason_open: open,
            reason_head_h,
            box_h,
            total: box_h + WORK_GAP,
        })
    }

    /// Height reserved for `reply`'s record, or zero when it has none.
    fn work_strip_height(
        &self,
        tr: &mut TextRenderer,
        reply: &Message,
        text_w: f32,
        with_reason: bool,
    ) -> f64 {
        self.work_strip_of(tr, reply, text_w, with_reason)
            .map(|s| s.total)
            .unwrap_or(0.0)
    }

    /// Draw what an agent did with a message: a strip in the tone of the state
    /// it is in, and its own words underneath when it refused.
    ///
    /// Read beneath the message that summoned the agent, so the person who
    /// typed the instruction sees it picked up without opening anything. The
    /// same strip is drawn on the agent's own reply inside the thread, because
    /// that is the message the record actually belongs to.
    #[allow(clippy::too_many_arguments)]
    fn draw_work_strip(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        reply: &Message,
        x: f64,
        y: f64,
        text_w: f32,
        with_reason: bool,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let Some(strip_m) = self.work_strip_of(tr, reply, text_w, with_reason) else {
            return;
        };
        let WorkStrip {
            line,
            detail_h,
            rows,
            reason,
            reason_open,
            reason_head_h,
            box_h,
            ..
        } = &strip_m;
        let (detail_h, box_h) = (*detail_h, *box_h);
        let ground = theme.tokens.colors.surfaces.base;
        let (fill, edge, ink) = work_strip_colors(theme, ground, line.tone);
        let strip = Rect::new(x, y, x + text_w as f64, y + box_h);
        fill_round(scene, tf, strip, WORK_RADIUS, fill);
        stroke_round(scene, tf, strip, WORK_RADIUS, edge, 1.0);
        // A bar down the leading edge in the tone's own colour, so the state
        // is answerable from the shape at a glance and never from the colour
        // alone — the wash is deliberately mostly ground, which is not enough
        // to tell four states apart, and would be nothing at all to a reader
        // who cannot separate them by hue.
        let rail = Rect::new(x, y, x + WORK_RAIL_W, y + box_h);
        fill_round(scene, tf, rail, WORK_RAIL_W / 2.0, edge);

        let tx = x + WORK_PAD_X;
        let inner = (text_w as f64 - WORK_PAD_X * 2.0).max(40.0) as f32;
        let head = truncate_to(tr, &line.head, WORK_TEXT_SIZE, inner as f64);
        text(
            scene, tr, tf, &head, WORK_TEXT_SIZE, ink,
            tx, y + WORK_PAD_Y, TextWeight::Semibold, None,
        );
        if detail_h > 0.0 {
            text(
                scene, tr, tf, &line.detail, WORK_TEXT_SIZE, ink,
                tx, y + WORK_PAD_Y + WORK_HEAD_H + 3.0,
                TextWeight::Regular, Some(inner),
            );
        }

        let mut ry = y + WORK_PAD_Y + WORK_HEAD_H + detail_h;

        // One line per operation the agent dispatched, in the order it
        // dispatched them.
        if !rows.is_empty() {
            ry += WORK_SECTION_GAP;
            for row in rows {
                let mark = row.tone.seed(theme);
                // Floored against the wash it lands on, exactly as the label
                // is: a tone is a colour chosen to mean something, which is
                // never automatically one that can be seen on this ground.
                let mark = band_at_least(mark, mark, fill, WORK_EDGE_CONTRAST);
                let icon_rect = Rect::new(
                    tx,
                    ry + (WORK_ROW_H - WORK_ICON) / 2.0,
                    tx + WORK_ICON,
                    ry + (WORK_ROW_H + WORK_ICON) / 2.0,
                );
                draw_icon(scene, tf, icon_rect, row.icon, mark, 1.5);
                let lx = tx + WORK_ICON + WORK_ICON_GAP;
                let lw = (x + text_w as f64 - WORK_PAD_X) - lx;
                let label = truncate_to(tr, &row.label, WORK_TEXT_SIZE, lw);
                text(
                    scene, tr, tf, &label, WORK_TEXT_SIZE, ink,
                    lx, ry, TextWeight::Regular, None,
                );
                ry += WORK_ROW_H;
            }
        }

        // The line that opens the reasoning, and the reasoning under it when
        // it is open. Drawn last because it is the least urgent thing in the
        // strip and the only part a reader has to ask for.
        if let Some(r) = reason {
            ry += WORK_SECTION_GAP;
            let open = *reason_open;
            let chev = Rect::new(
                tx,
                ry + (WORK_ROW_H - WORK_ICON) / 2.0,
                tx + WORK_ICON,
                ry + (WORK_ROW_H + WORK_ICON) / 2.0,
            );
            draw_icon(
                scene,
                tf,
                chev,
                if open { Icon::ChevronDown } else { Icon::ChevronRight },
                ink,
                1.5,
            );
            let lx = tx + WORK_ICON + WORK_ICON_GAP;
            let lw = (x + text_w as f64 - WORK_PAD_X) - lx;
            let head = truncate_to(tr, &reasoning_head(r), WORK_TEXT_SIZE, lw);
            text(
                scene, tr, tf, &head, WORK_TEXT_SIZE, ink,
                lx, ry, TextWeight::Semibold, None,
            );
            // The whole line is the target, not the chevron: a 10px square is
            // not something to ask anybody to hit.
            ChatApp::push_hit(
                hits,
                Rect::new(x, ry, x + text_w as f64, ry + WORK_ROW_H),
                Hit::AgentReasoning(reply.id.clone()),
            );
            ry += *reason_head_h - WORK_SECTION_GAP;
            if open && !r.why.trim().is_empty() {
                text(
                    scene, tr, tf, r.why.trim(), WORK_TEXT_SIZE, ink,
                    lx, ry + 3.0, TextWeight::Regular,
                    Some((lw as f32).max(40.0)),
                );
            }
        }
    }

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
        let c = &theme.tokens.colors;
        // Permission boundary: an embed this peer hasn't been granted access to
        // renders as a locked card and never hydrates its bytes off the FUSE
        // mount. Same footprint as the real embed so the timeline layout is
        // identical whether granted or gated.
        if !self.embed_allowed(att.path()) {
            self.draw_gated_embed(scene, tf, tr, theme, att, x, y, text_w);
            return;
        }
        match att {
            Attachment::Image { name, path, w, h } => {
                let (tw, th) = image_thumb_size(*w, *h, text_w as f64);
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
                    text(scene, tr, tf, "🖼", 18.0, c.text_dim, dot.x - gw as f64 / 2.0, dot.y - gh as f64 / 2.0, TextWeight::Regular, None);
                }
                // Caption gradient scrim + filename, then a framing stroke.
                fill_round(scene, tf, Rect::new(rect.x0, rect.y1 - 30.0, rect.x1, rect.y1), 12.0, Color::from_rgba8(0, 0, 0, 120));
                stroke_round(scene, tf, rect, 12.0, c.border_white_10, 1.0);
                label(scene, tr, tf, &format!("🖼  {name}"), 12.0, Color::WHITE, rect.x0 + 12.0, rect.y1 - 22.0);
                let dim = format!("{w}×{h}");
                label(scene, tr, tf, &dim, 11.0, c.text_dim, rect.x0, rect.y1 + 4.0);
            }
            Attachment::Video { name, w, h, size, .. } => {
                let (tw, th) = video_card_size(*w, *h, text_w as f64);
                let rect = Rect::new(x, y, x + tw, y + th);
                // A dark poster surface with a play affordance. A real decoded
                // frame needs the media engine's decoder (live-media only); here
                // the card references the projected file and hydrates its bytes
                // on demand — honest, not a fabricated still.
                fill_round(scene, tf, rect, 12.0, c.neutral[2]);
                fill_round(
                    scene, tf,
                    Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + th * 0.5),
                    12.0, c.surface_white_5,
                );
                // Centred play button.
                let cc = rect.center();
                fill_circle(scene, tf, cc, 25.0, Color::from_rgba8(0, 0, 0, 150));
                fill_circle(scene, tf, cc, 24.0, c.surface_white_10);
                let (pw, ph) = tr.measure("▶", 20.0, None);
                text(scene, tr, tf, "▶", 20.0, Color::WHITE, cc.x - pw as f64 / 2.0 + 2.0, cc.y - ph as f64 / 2.0, TextWeight::Regular, None);
                // Caption scrim + filename, then a framing stroke.
                fill_round(scene, tf, Rect::new(rect.x0, rect.y1 - 30.0, rect.x1, rect.y1), 12.0, Color::from_rgba8(0, 0, 0, 120));
                stroke_round(scene, tf, rect, 12.0, c.border_white_10, 1.0);
                label(scene, tr, tf, &format!("🎬  {name}"), 12.0, Color::WHITE, rect.x0 + 12.0, rect.y1 - 22.0);
                let meta = if *w > 0 && *h > 0 {
                    format!("{w}×{h} · {}", human_size(*size))
                } else {
                    format!("video · {}", human_size(*size))
                };
                label(scene, tr, tf, &meta, 11.0, c.text_dim, rect.x0, rect.y1 + 4.0);
            }
            Attachment::File { name, size, .. } => {
                let chip = Rect::new(x, y, x + (text_w as f64).min(360.0), y + 58.0);
                fill_round(scene, tf, chip, 10.0, c.surfaces.raised);
                stroke_round(scene, tf, chip, 10.0, c.border_standard, 1.0);
                let icon = Rect::new(chip.x0 + 10.0, chip.y0 + 11.0, chip.x0 + 46.0, chip.y1 - 11.0);
                fill_round(scene, tf, icon, 8.0, c.accent[5]);
                let (iw, ih) = tr.measure("📄", 16.0, None);
                text(scene, tr, tf, "📄", 16.0, Color::WHITE, icon.center().x - iw as f64 / 2.0, icon.center().y - ih as f64 / 2.0, TextWeight::Regular, None);
                let fname = truncate_to(tr, name, 13.5, chip.width() - 70.0);
                label(scene, tr, tf, &fname, 13.5, c.text_primary, chip.x0 + 56.0, chip.center().y - 14.0);
                let meta = file_chip_meta(name, *size);
                label(scene, tr, tf, &meta, 11.0, c.text_dim, chip.x0 + 56.0, chip.center().y + 2.0);
            }
        }
    }

    /// Render an embed this peer isn't permitted to open: a neutral locked card
    /// with the filename but no hydrated content. Sized to match the granted
    /// embed's footprint so the timeline layout is identical either way.
    fn draw_gated_embed(
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
        let c = &theme.tokens.colors;
        let (w, h) = match att {
            Attachment::Image { w, h, .. } => image_thumb_size(*w, *h, text_w as f64),
            Attachment::Video { w, h, .. } => video_card_size(*w, *h, text_w as f64),
            Attachment::File { .. } => ((text_w as f64).min(360.0), 58.0),
        };
        let rect = Rect::new(x, y, x + w, y + h);
        fill_round(scene, tf, rect, 12.0, c.neutral[1]);
        stroke_round(scene, tf, rect, 12.0, c.border_standard, 1.0);
        let cx = rect.center().x;
        let cy = rect.center().y - if h > 64.0 { 8.0 } else { 6.0 };
        let (lw, lh) = tr.measure("🔒", 18.0, None);
        text(scene, tr, tf, "🔒", 18.0, c.text_dim, cx - lw as f64 / 2.0, cy - lh as f64 / 2.0, TextWeight::Regular, None);
        let msg = "Access not granted";
        let (mw, _) = tr.measure(msg, 12.0, None);
        label(scene, tr, tf, msg, 12.0, c.text_dim, cx - mw as f64 / 2.0, cy + 12.0);
        let fname = truncate_to(tr, att.name(), 11.0, rect.width() - 24.0);
        let (fw, _) = tr.measure(&fname, 11.0, None);
        label(scene, tr, tf, &fname, 11.0, c.text_dim, cx - fw as f64 / 2.0, cy + 28.0);
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
        let inner = self.draw_input_box(
            scene, tf, tr, theme, rect, send_enabled, focused, Hit::FocusMain, true, hits,
        );
        self.composer_main.apply_layout_external(inner);
        self.composer_main.set_focus(focused);
        self.composer_main.draw(scene, tf, tr, theme, presence);
    }

    /// Draw the `@` picker above the composer, anchored so it grows upward out
    /// of the field rather than off the bottom of the window.
    ///
    /// The popover itself is the shared [`ui::widget::ContextMenu`]; all this
    /// does is place it and record the rows' hit regions so a click picks a
    /// participant.
    pub(crate) fn draw_mention_picker(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        if !self.mention_menu.is_open() || self.mention_rows.is_empty() {
            return;
        }
        // Anchored off the composer's own geometry rather than off something
        // remembered from the previous frame, so the very first frame a picker
        // is open on places it correctly — which is the only kind of frame a
        // headless capture ever gets.
        let composer = self.composer_rect(tr);
        let anchor = Point::new(composer.x0 + 16.0, composer.y0);
        // Height first, so the list can be placed above its anchor.
        self.mention_menu.move_to([anchor.x as f32, anchor.y as f32]);
        let h = self.mention_menu.rect().height();
        let top = (anchor.y - h - 8.0).max(self.rect.y0 + 8.0);
        self.mention_menu.move_to([anchor.x as f32, top as f32]);
        // One region for the whole popover; which row a click landed on is the
        // primitive's own `item_at`, so the row layout is never re-derived here.
        ChatApp::push_hit(hits, self.mention_menu.rect(), Hit::MentionMenu);
        self.mention_menu.draw(scene, tf, tr, theme, presence);
    }

    /// Draw the composer box chrome — the rounded field, the attach `+`, the
    /// emoji + send buttons — and record their hit regions. Returns the inner
    /// rect the caller lays the composer [`TextInput`] into; the widget owns
    /// the text, caret, selection, and placeholder inside it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_input_box(
        &self,
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
        stroke_round(scene, tf, rect, 14.0, if focused { c.border_focus } else { c.border_standard }, if focused { 1.6 } else { 1.0 });
        ChatApp::push_hit(hits, rect, focus_hit);

        let mut tx = rect.x0 + 16.0;
        if attach {
            let plus = Rect::new(rect.x0 + 10.0, rect.center().y - 14.0, rect.x0 + 38.0, rect.center().y + 14.0);
            fill_round(scene, tf, plus, 8.0, c.surface_white_10);
            let (pw, ph) = tr.measure("+", 20.0, None);
            text(scene, tr, tf, "+", 20.0, c.text_dim, plus.center().x - pw as f64 / 2.0, plus.center().y - ph as f64 / 2.0, TextWeight::Regular, None);
            ChatApp::push_hit(hits, plus, Hit::Attach);
            tx = plus.x1 + 12.0;
        }

        // right cluster: emoji + send
        let send = Rect::new(rect.x1 - 56.0, rect.center().y - 16.0, rect.x1 - 12.0, rect.center().y + 16.0);
        fill_round(scene, tf, send, 9.0, if send_enabled { c.accent[5] } else { c.surface_white_5 });
        let (sw, sh) = tr.measure("➤", 16.0, None);
        text(scene, tr, tf, "➤", 16.0, if send_enabled { Color::WHITE } else { c.text_dim }, send.center().x - sw as f64 / 2.0, send.center().y - sh as f64 / 2.0, TextWeight::Regular, None);
        ChatApp::push_hit(hits, send, Hit::Send);

        let emoji = Rect::new(send.x0 - 40.0, rect.center().y - 16.0, send.x0 - 8.0, rect.center().y + 16.0);
        let (ew, eh) = tr.measure("🙂", 16.0, None);
        text(scene, tr, tf, "🙂", 16.0, c.text_dim, emoji.center().x - ew as f64 / 2.0, emoji.center().y - eh as f64 / 2.0, TextWeight::Regular, None);
        ChatApp::push_hit(hits, emoji, Hit::ComposerEmoji(Point::new(emoji.x0 - 140.0, rect.y0 - 8.0)));

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
        let Some(parent_id) = self.thread_open.clone() else { return };
        let panel = self.thread_rect();
        fill_rect(scene, tf, panel, c.surfaces.raised);
        hline(scene, tf, panel.x0, panel.x0, panel.y0, c.border_standard);
        scene.stroke(&vello::kurbo::Stroke::new(1.0), tf, c.border_standard, None, &vello::kurbo::Line::new(Point::new(panel.x0, panel.y0), Point::new(panel.x0, panel.y1)));

        // header
        let head = Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + 52.0);
        label(scene, tr, tf, "Thread", 15.0, c.text_primary, panel.x0 + 16.0, panel.y0 + 12.0);
        let chn = self.channel(&self.active_channel).map(|c| format!("#{}", c.name)).unwrap_or_default();
        label(scene, tr, tf, &chn, 11.5, c.text_dim, panel.x0 + 16.0, panel.y0 + 31.0);
        let close = Rect::new(panel.x1 - 40.0, panel.y0 + 12.0, panel.x1 - 12.0, panel.y0 + 40.0);
        fill_round(scene, tf, close, 8.0, c.surface_white_10);
        let (xw, xh) = tr.measure("✕", 14.0, None);
        text(scene, tr, tf, "✕", 14.0, c.text_primary, close.center().x - xw as f64 / 2.0, close.center().y - xh as f64 / 2.0, TextWeight::Regular, None);
        ChatApp::push_hit(hits, close, Hit::CloseThread);
        hline(scene, tf, panel.x0 + 12.0, panel.x1 - 12.0, head.y1, c.border_standard);

        let body = Rect::new(panel.x0, head.y1, panel.x1, panel.y1 - 70.0);
        push_clip(scene, tf, body);
        let tx = panel.x0 + 16.0 + 32.0 + 10.0;
        let tw = (panel.x1 - tx - 14.0).max(60.0) as f32;
        let mut y = body.y0 + 12.0;

        // parent message
        if let Some(parent) = self.message(&parent_id).cloned() {
            y = self.draw_thread_message(scene, tf, tr, theme, &parent, panel.x0 + 16.0, tx, tw, y, hits);
            // replies divider
            let n = model::reply_count(&self.messages, &parent_id);
            let s = format!("{n} {}", if n == 1 { "reply" } else { "replies" });
            let (sw, _) = tr.measure(&s, 11.5, None);
            let dcy = y + 14.0;
            hline(scene, tf, panel.x0 + 16.0, panel.center().x - sw as f64 / 2.0 - 10.0, dcy, c.border_standard);
            hline(scene, tf, panel.center().x + sw as f64 / 2.0 + 10.0, panel.x1 - 16.0, dcy, c.border_standard);
            label(scene, tr, tf, &s, 11.5, c.text_dim, panel.center().x - sw as f64 / 2.0, dcy - 7.0);
            y += 30.0;
        }
        let replies: Vec<Message> = self
            .messages
            .iter()
            .filter(|m| m.parent.as_deref() == Some(parent_id.as_str()))
            .cloned()
            .collect();
        for r in &replies {
            y = self.draw_thread_message(scene, tf, tr, theme, r, panel.x0 + 16.0, tx, tw, y, hits);
        }
        scene.pop_layer();

        // thread composer
        let comp = Rect::new(panel.x0 + 12.0, panel.y1 - 60.0, panel.x1 - 12.0, panel.y1 - 12.0);
        let focused = matches!(self.focus, crate::app::Focus::Thread) && self.editing.is_none();
        let send_enabled = !self.composer_thread.value().trim().is_empty();
        let inner = self.draw_input_box(
            scene, tf, tr, theme, comp, send_enabled, focused, Hit::FocusThread, false, hits,
        );
        self.composer_thread.apply_layout_external(inner);
        self.composer_thread.set_focus(focused);
        self.composer_thread.draw(scene, tf, tr, theme, presence);
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
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let mut yy = y;
        if let Some(u) = self.user(&m.author) {
            draw_avatar(scene, tf, tr, theme, Point::new(avx + 16.0, yy + 14.0), 14.0, &self.initials_for(&u.id), u.color_idx, Some(u.status));
        }
        let (nw, _) = text(scene, tr, tf, &self.user_name(&m.author), 13.5, palette_color(theme, self.user(&m.author).map(|u| u.color_idx).unwrap_or(0)), tx, yy, TextWeight::Semibold, None);
        label(scene, tr, tf, &model::clock(m.ts), 11.0, c.text_dim, tx + nw as f64 + 8.0, yy + 2.0);
        yy += 19.0;
        if !m.text.is_empty() {
            let h = tr.measure(&m.text, 14.0, Some(tw)).1 as f64;
            text(scene, tr, tf, &m.text, 14.0, c.text_primary, tx, yy, TextWeight::Regular, Some(tw));
            yy += h + 4.0;
        }
        // The record on the agent's own reply, where it belongs. The
        // timeline draws the same strip under the message that summoned it —
        // one fact, rendered where each reader is looking.
        if m.work.is_some() {
            self.draw_work_strip(scene, tf, tr, theme, m, tx, yy, tw, false, hits);
            yy += self.work_strip_height(tr, m, tw, false);
        }
        if let Some(att) = &m.attachment {
            self.draw_attachment(scene, tf, tr, theme, att, tx, yy, tw);
            yy += match att {
                Attachment::File { .. } => 66.0,
                Attachment::Image { w, h, .. } => image_thumb_size(*w, *h, tw as f64).1 + 24.0,
                Attachment::Video { w, h, .. } => video_card_size(*w, *h, tw as f64).1 + 24.0,
            };
        }
        if !m.reactions.is_empty() {
            let mut x = tx;
            for r in &m.reactions {
                let lbl = format!("{} {}", r.emoji, r.count());
                let (lw, _) = tr.measure(&lbl, 11.5, None);
                let chip = Rect::new(x, yy, x + lw as f64 + 16.0, yy + 22.0);
                fill_round(scene, tf, chip, 11.0, c.surface_white_5);
                label(scene, tr, tf, &lbl, 11.5, c.text_primary, chip.x0 + 8.0, chip.center().y - 7.0);
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
            &vello::kurbo::Line::new(Point::new(panel.x0, panel.y0), Point::new(panel.x0, panel.y1)),
        );

        let rows = self.roster_rows();
        // Header: "Members" + the workspace name, with a close control.
        label(scene, tr, tf, "Members", 15.0, c.text_primary, panel.x0 + 16.0, panel.y0 + 12.0);
        let ws_name = self
            .workspaces
            .iter()
            .find(|w| w.id == self.active_ws)
            .map(|w| w.name.clone())
            .unwrap_or_default();
        let sub = format!("{} · {}", ws_name, rows.len());
        let sub = truncate_to(tr, &sub, 11.5, panel.width() - 70.0);
        label(scene, tr, tf, &sub, 11.5, c.text_dim, panel.x0 + 16.0, panel.y0 + 31.0);
        let close = Rect::new(panel.x1 - 40.0, panel.y0 + 12.0, panel.x1 - 12.0, panel.y0 + 40.0);
        fill_round(scene, tf, close, 8.0, c.surface_white_10);
        let (xw, xh) = tr.measure("✕", 14.0, None);
        text(scene, tr, tf, "✕", 14.0, c.text_primary, close.center().x - xw as f64 / 2.0, close.center().y - xh as f64 / 2.0, TextWeight::Regular, None);
        ChatApp::push_hit(hits, close, Hit::ToggleRightFlap);
        let head_y1 = panel.y0 + 52.0;
        hline(scene, tf, panel.x0 + 12.0, panel.x1 - 12.0, head_y1, c.border_standard);

        let body = Rect::new(panel.x0, head_y1, panel.x1, panel.y1);
        push_clip(scene, tf, body);
        let row_h = 46.0;
        let mut y = head_y1 + 8.0;
        for (u, voice) in &rows {
            if y + row_h > panel.y1 {
                break;
            }
            let avc = Point::new(panel.x0 + 16.0 + 16.0, y + row_h / 2.0);
            draw_avatar(scene, tf, tr, theme, avc, 16.0, &u.initials, u.color_idx, Some(u.status));
            let nx = avc.x + 16.0 + 12.0;
            let display = if u.id == self.me.id {
                format!("{} (you)", u.name)
            } else {
                u.name.clone()
            };
            let name = truncate_to(tr, &display, 14.0, panel.x1 - nx - 14.0);
            text(scene, tr, tf, &name, 14.0, c.text_primary, nx, avc.y - 14.0, TextWeight::Semibold, None);
            // What an agent has in flight outranks its presence word, and
            // only an agent ever has any: this is the live half of the
            // surface, so it is drawn from a record that has not finished
            // *and* a participant the room still lists. The durable half —
            // what was taken, decided and done — is in the thread, which is
            // where HIG 9.12 says the fact has to remain available when this
            // line has moved on.
            let (status_txt, col) = match (voice, self.agent_in_flight(&u.id)) {
                (Some(ch), _) => (format!("🔊 In voice · {ch}"), c.semantic.success),
                (None, Some(work)) => (
                    work_line(work, &u.name, "", true).head,
                    // Accent, for the same reason the strip uses it: work in
                    // flight is state nobody chose and everybody should see.
                    legible_on(c.accent[5], c.surfaces.raised, ROSTER_WORK_CONTRAST, c.text_primary),
                ),
                (None, None) => (presence_label(u.status).to_string(), c.text_dim),
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
        let more_h = if self.attach_entries.len() > max_rows { 20.0 } else { 0.0 };
        let panel_h = head_h + body_h + more_h + 8.0;
        let y1 = comp.y0 - 10.0;
        let y0 = (y1 - panel_h).max(self.rect.y0 + 12.0);
        let panel = Rect::new(x0, y0, x0 + w, y1);

        fill_round(scene, tf, panel.inflate(5.0, 5.0), 18.0, Color::from_rgba8(0, 0, 0, 70));
        fill_round(scene, tf, panel, 14.0, c.neutral[1]);
        stroke_round(scene, tf, panel, 14.0, c.border_white_10, 1.0);

        label(scene, tr, tf, "Share a file", 13.5, c.text_primary, panel.x0 + 14.0, panel.y0 + 12.0);
        let count = format!("{}", self.attach_entries.len());
        let (cw, _) = tr.measure(&count, 11.5, None);
        label(scene, tr, tf, &count, 11.5, c.text_dim, panel.x1 - cw as f64 - 14.0, panel.y0 + 14.0);
        hline(scene, tf, panel.x0 + 12.0, panel.x1 - 12.0, panel.y0 + head_h - 4.0, c.border_standard);

        if self.attach_entries.is_empty() {
            let dir = crate::picker::attach_dir();
            let s = format!("No media in {}", dir.display());
            let s = truncate_to(tr, &s, 12.5, panel.width() - 28.0);
            label(scene, tr, tf, &s, 12.5, c.text_dim, panel.x0 + 14.0, panel.y0 + head_h + 10.0);
            return;
        }

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
                text(scene, tr, tf, "🎬", 16.0, c.text_dim, thumb.center().x - gw as f64 / 2.0, thumb.center().y - gh as f64 / 2.0, TextWeight::Regular, None);
            } else if let Some(img) = self.images.get(&key) {
                ui::image_store::paint_image_cover(scene, tf, thumb, &img, 8.0);
            } else {
                fill_round(scene, tf, thumb, 8.0, c.surfaces.raised);
                let (gw, gh) = tr.measure("🖼", 16.0, None);
                text(scene, tr, tf, "🖼", 16.0, c.text_dim, thumb.center().x - gw as f64 / 2.0, thumb.center().y - gh as f64 / 2.0, TextWeight::Regular, None);
            }
            stroke_round(scene, tf, thumb, 8.0, c.border_white_10, 1.0);
            let name = truncate_to(tr, &e.name, 13.5, row.width() - 96.0);
            label(scene, tr, tf, &name, 13.5, c.text_primary, thumb.x1 + 12.0, row.center().y - 13.0);
            label(scene, tr, tf, &human_size(e.size), 11.0, c.text_dim, thumb.x1 + 12.0, row.center().y + 3.0);
            ChatApp::push_hit(hits, row, Hit::AttachPick(key));
            y += row_h;
        }
        if self.attach_entries.len() > max_rows {
            let more = format!("+{} more in the workspace", self.attach_entries.len() - max_rows);
            label(scene, tr, tf, &more, 11.0, c.text_dim, panel.x0 + 14.0, panel.y1 - 16.0);
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
    /// Height of the "written about ..." chip, or zero for an ordinary
    /// message that points at nothing.
    ref_h: f64,
    /// Height of the strip showing what an agent did with this message, or
    /// zero when no agent has taken it.
    work_h: f64,
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
        top_gap
            + header
            + self.ref_h
            + body
            + self.edited_h
            + self.att_h
            + self.work_h
            + self.react_h
            + self.thread_h
            + 4.0
    }
}

/// Height reserved for the reference chip, pill plus the gap under it.
const REF_CHIP_H: f64 = 26.0;

/// Vertical padding added around the inline editor's text (top + bottom).
const EDIT_BOX_PAD: f64 = 16.0;

/// Contain-fit an image into the available text width, capped at a tidy
/// preview size.
fn image_thumb_size(w: u32, h: u32, max_w: f64) -> (f64, f64) {
    let maxw = max_w.min(380.0);
    let maxh = 210.0;
    let (w, h) = (w.max(1) as f64, h.max(1) as f64);
    let mut tw = maxw;
    let mut th = tw * h / w;
    if th > maxh {
        th = maxh;
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

/// Type size for the reference chip's label.
const REF_CHIP_SIZE: f32 = 11.5;

/// How much of the surface the chip's wash keeps. Mostly ground: the chip is
/// context, and a solid accent block above every remark would shout over the
/// sentence it is there to introduce.
const REF_CHIP_FILL_MIX: f64 = 0.86;
/// The chip's edge starts far closer to the accent than its wash does — the
/// edge is what makes the pill a pill, so it is the part that must survive.
const REF_CHIP_EDGE_MIX: f64 = 0.45;

/// Contrast floors for the chip, measured rather than eyeballed: AA for its
/// label against its own wash, and the non-text floor for the edge that draws
/// the pill against the surface behind it.
pub(crate) const REF_CHIP_TEXT_CONTRAST: f64 = 4.5;
pub(crate) const REF_CHIP_EDGE_CONTRAST: f64 = 3.0;

/// The wash, edge and ink a reference chip is painted in on `ground`.
///
/// Measured for the same reason the mention run is. The first version of this
/// chip was a translucent accent wash with accent ink on top, which looked
/// right on the dark theme and arrived on the light one as grey-blue text on
/// pale blue — a chip you could see but not read. A colour picked to mean
/// "accent" is not a colour picked to be read on whatever ground the theme
/// happens to put behind it.
///
/// The two halves are floored against different things on purpose: the label
/// has to clear AA against the wash it sits on, while the *edge* is what has to
/// clear the non-text floor against the surface, because the edge is what makes
/// the pill legible as a container. Flooring the wash itself would turn a quiet
/// piece of context into a solid accent block.
pub(crate) fn reference_chip_colors(theme: &Theme, ground: Color) -> (Color, Color, Color) {
    let accent = theme.tokens.colors.accent[5];
    let fill = mix(accent, ground, REF_CHIP_FILL_MIX);
    let edge = band_at_least(
        mix(accent, ground, REF_CHIP_EDGE_MIX),
        accent,
        ground,
        REF_CHIP_EDGE_CONTRAST,
    );
    // Ink starts from body ink and is pushed away from the wash: text in the
    // wash's own hue on the wash is the combination that cannot be rescued.
    let ink = legible_on(
        theme.tokens.colors.text_primary,
        fill,
        REF_CHIP_TEXT_CONTRAST,
        theme.tokens.colors.text_primary,
    );
    (fill, edge, ink)
}

/// The wording on the chip of a message written *about* something.
///
/// Shows [`ResourceRef::label`](qos_reference::ResourceRef::label) — the
/// publishing app's own human phrase for what was selected — and never the
/// address. `kind` and `id` are machine vocabulary (`word_processor.block`,
/// `files.entry:/var/workspace/...`), so putting either in front of a person
/// is the same leak [`file_chip_meta`] exists to prevent, with a second cost
/// on top: a chip that displays the address invites a reader to *resolve* on
/// it, which is exactly the degradation `qos-reference` is built to stop.
///
/// A reference whose label is empty still gets a chip. The message really is
/// about something, and drawing nothing would be a quieter lie than a vague
/// word — so it gets a neutral noun rather than an id.
fn reference_chip_label(r: &qos_reference::ResourceRef) -> String {
    let label = r.label.trim();
    if label.is_empty() {
        return "a selection".to_string();
    }
    label.to_string()
}


// --------------------------------------------------------------- agent work

/// AA floor for the working line beside an agent in the members list. The
/// accent is a colour picked to *mean* "live", which on a light theme lands
/// close enough to the raised surface to be a line you can see and not read.
pub(crate) const ROSTER_WORK_CONTRAST: f64 = 4.5;

/// Geometry of the strip an agent's record is drawn in.
const WORK_PAD_X: f64 = 12.0;
const WORK_PAD_Y: f64 = 6.0;
/// One line of head text at [`WORK_TEXT_SIZE`].
const WORK_HEAD_H: f64 = 16.0;
/// Gap under the strip, before whatever the row draws next.
const WORK_GAP: f64 = 6.0;
const WORK_RADIUS: f64 = 8.0;
/// Width of the bar down the leading edge that carries the state's tone.
const WORK_RAIL_W: f64 = 3.0;
const WORK_TEXT_SIZE: f32 = 11.5;
/// One dispatched operation, or the line that opens the reasoning.
const WORK_ROW_H: f64 = 15.0;
/// Square the outcome mark and the disclosure chevron are drawn in.
const WORK_ICON: f64 = 10.0;
/// Between a row's mark and its words.
const WORK_ICON_GAP: f64 = 6.0;
/// Above the first row of a section, separating it from the line before.
const WORK_SECTION_GAP: f64 = 5.0;
/// The strip is context around a sentence, so it keeps mostly the ground it
/// sits on — the same reasoning, and the same numbers, as the reference chip.
const WORK_FILL_MIX: f64 = 0.86;
const WORK_EDGE_MIX: f64 = 0.45;
pub(crate) const WORK_TEXT_CONTRAST: f64 = 4.5;
pub(crate) const WORK_EDGE_CONTRAST: f64 = 3.0;

/// How an agent's record reads.
///
/// Four tones rather than a colour per state, because the distinction a reader
/// needs is coarser than the record is: is something in flight, did it finish,
/// did it stop, or is nothing happening.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkTone {
    /// Taken, waiting or working, by an agent that is in the room.
    Live,
    /// Applied.
    Done,
    /// Refused — an outcome the agent chose and stated.
    Stopped,
    /// A record that stopped moving and an agent that is not here to move it.
    Silent,
}

impl WorkTone {
    /// The colour this tone starts from, before it is floored against whatever
    /// ground it lands on.
    fn seed(self, theme: &Theme) -> Color {
        let c = &theme.tokens.colors;
        match self {
            // HIG 9.4: accent is reserved for state the user did not choose
            // and should notice — "a sync in flight" is the given example, and
            // an agent that has taken your instruction is the same kind of
            // fact about the same kind of moment.
            WorkTone::Live => c.accent[5],
            WorkTone::Done => c.semantic.success,
            // Warning, not danger. A refusal is a decision the agent made and
            // said out loud, not a fault; painting it in the danger colour
            // would make an honest "I'm not allowed to do that here" look
            // like something broke.
            WorkTone::Stopped => c.semantic.warning,
            // Nothing is happening, and the strip must not suggest otherwise.
            WorkTone::Silent => c.text_dim,
        }
    }
}

/// The wash, edge and ink an agent's strip is painted in on `ground`.
///
/// Derived exactly the way [`reference_chip_colors`] is, and for the same
/// reason: a tone is a colour picked to *mean* something, which is never
/// automatically a colour that can be read on whatever the theme puts behind
/// it. The label is floored against the wash it sits on; the edge is floored
/// against the surface, because the edge is what makes the strip legible as a
/// container at all.
pub(crate) fn work_strip_colors(
    theme: &Theme,
    ground: Color,
    tone: WorkTone,
) -> (Color, Color, Color) {
    let seed = tone.seed(theme);
    let fill = mix(seed, ground, WORK_FILL_MIX);
    let edge = band_at_least(
        mix(seed, ground, WORK_EDGE_MIX),
        seed,
        ground,
        WORK_EDGE_CONTRAST,
    );
    let ink = legible_on(
        theme.tokens.colors.text_primary,
        fill,
        WORK_TEXT_CONTRAST,
        theme.tokens.colors.text_primary,
    );
    (fill, edge, ink)
}

/// An agent's strip, measured once.
///
/// One struct for the measure pass and the draw pass, so the two cannot
/// disagree about the height — the fault that clips a row's last line.
pub(crate) struct WorkStrip {
    pub line: WorkLine,
    pub detail_h: f64,
    pub rows: Vec<TraceRow>,
    pub reason: Option<model::AgentReasoning>,
    pub reason_open: bool,
    pub reason_head_h: f64,
    /// The painted box.
    pub box_h: f64,
    /// The box plus the gap under it — what the row advances by.
    pub total: f64,
}

/// One dispatched operation, worded and toned for a reader.
///
/// Built only from an [`AgentStep`](model::AgentStep), which the agent writes
/// from its own dispatch path. There is deliberately no constructor here that
/// takes a sentence: a row on screen means an operation was attempted, and a
/// row that could be built from prose would mean nothing at all.
pub(crate) struct TraceRow {
    /// The app's own word for the operation.
    pub label: String,
    /// The mark at the head of the row. Shape, not colour — the tone is a
    /// second signal for readers who have it, never the only one.
    pub icon: Icon,
    pub tone: WorkTone,
}

/// Word one dispatched operation.
///
/// `said` is what the strip already shows above the rows — the agent's own
/// sentence about the turn. A refusal whose reason is already in that
/// sentence shows only the operation: the row is there to name which
/// operation was refused, and repeating the reason under it says the same
/// thing twice and reads as two separate problems.
pub(crate) fn trace_row(step: &model::AgentStep, said: &str) -> TraceRow {
    use model::AgentStepOutcome as O;
    match &step.outcome {
        O::Applied => TraceRow {
            label: step.label.clone(),
            icon: Icon::Check,
            tone: WorkTone::Done,
        },
        // Not a tick. It ran and the document is unchanged, and a reader who
        // goes looking for the edit has to find the row that told them there
        // wasn't one.
        O::NoChange => TraceRow {
            label: crate::i18n::t_args(
                "chat-agent-step-unchanged",
                &[("did", step.label.as_str().into())],
            ),
            icon: Icon::Info,
            tone: WorkTone::Silent,
        },
        O::Refused { why } => TraceRow {
            label: if why.trim().is_empty() || said_covers(said, why) {
                step.label.clone()
            } else {
                crate::i18n::t_args(
                    "chat-agent-step-refused",
                    &[("did", step.label.as_str().into()), ("why", why.as_str().into())],
                )
            },
            icon: Icon::Ban,
            tone: WorkTone::Stopped,
        },
    }
}

/// Whether the sentence the agent already wrote says what `why` says.
///
/// Compared on the words rather than on the exact string: the reply is a
/// sentence — signed, punctuated — around the same clause the refusal
/// carries, so the two are never byte-identical even when they say the same
/// thing.
fn said_covers(said: &str, why: &str) -> bool {
    let why = why.trim().trim_end_matches('.').to_lowercase();
    !why.is_empty() && said.to_lowercase().contains(&why)
}

/// How the agent decided, worded for the line that opens it.
pub(crate) fn reasoning_head(reasoning: &model::AgentReasoning) -> String {
    match reasoning.route {
        model::AgentRoute::Model => crate::i18n::t("chat-agent-reasoning-model"),
        // Said plainly rather than hidden. A turn the model never answered
        // must not read like one it did.
        model::AgentRoute::Floor => crate::i18n::t("chat-agent-reasoning-floor"),
    }
}

/// One line of an agent's record, ready to draw.
pub(crate) struct WorkLine {
    /// Who, and what state they are in.
    pub head: String,
    /// The agent's own words, when the caller asked for them and there are
    /// any. Empty otherwise.
    pub detail: String,
    pub tone: WorkTone,
}

/// Word an agent's record for a reader.
///
/// `present` is whether the agent is in the room **now**. It is the only thing
/// that distinguishes a record from a claim: every state below says what the
/// agent did and when, and none of them says anything is happening at this
/// moment — so an unfinished record left behind by an agent that has gone is
/// worded as exactly that, and painted in the tone of nothing happening.
///
/// This is the whole reason liveness is asked separately. A process that dies
/// mid-instruction cannot retract what it wrote, so a record that claimed to
/// be live would go on claiming it for as long as the channel exists.
pub(crate) fn work_line(
    work: &model::AgentWork,
    agent: &str,
    reason: &str,
    present: bool,
) -> WorkLine {
    use model::AgentWorkState as S;
    let at = model::clock(work.since);
    // An unfinished record with nobody behind it is not a state the agent is
    // in; it is where the agent stopped. Said in those words, and never in
    // the present tense.
    if !work.finished() && !present {
        return WorkLine {
            head: crate::i18n::t_args(
                "chat-agent-gone",
                &[("agent", agent.into()), ("at", at.into())],
            ),
            detail: String::new(),
            tone: WorkTone::Silent,
        };
    }
    match &work.state {
        S::Taken => WorkLine {
            head: crate::i18n::t_args(
                "chat-agent-took",
                &[("agent", agent.into()), ("at", at.into())],
            ),
            detail: String::new(),
            tone: WorkTone::Live,
        },
        S::Waiting { app } => WorkLine {
            head: crate::i18n::t_args(
                "chat-agent-waiting",
                &[
                    ("agent", agent.into()),
                    ("app", app.as_str().into()),
                    ("at", at.into()),
                ],
            ),
            detail: String::new(),
            tone: WorkTone::Live,
        },
        S::Doing { did, about } => WorkLine {
            head: if about.is_empty() {
                crate::i18n::t_args(
                    "chat-agent-working",
                    &[
                        ("agent", agent.into()),
                        ("did", did.as_str().into()),
                        ("at", at.into()),
                    ],
                )
            } else {
                crate::i18n::t_args(
                    "chat-agent-working-about",
                    &[
                        ("agent", agent.into()),
                        ("did", did.as_str().into()),
                        ("about", about.as_str().into()),
                        ("at", at.into()),
                    ],
                )
            },
            detail: String::new(),
            tone: WorkTone::Live,
        },
        S::Done { did } => WorkLine {
            head: crate::i18n::t_args(
                "chat-agent-done",
                &[
                    ("agent", agent.into()),
                    ("did", did.as_str().into()),
                    ("at", at.into()),
                ],
            ),
            detail: String::new(),
            tone: WorkTone::Done,
        },
        // The reason travels with the refusal. A person reading the channel
        // has to be able to see *why* without opening the thread — a strip
        // that only said "stopped" would replace one silence with another.
        S::Refused => WorkLine {
            head: crate::i18n::t_args(
                "chat-agent-stopped",
                &[("agent", agent.into()), ("at", at.into())],
            ),
            detail: reason.to_string(),
            tone: WorkTone::Stopped,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        body_spans, file_chip_meta, human_size, mention_colors, reference_chip_colors,
        reference_chip_label, work_line, work_strip_colors, WorkTone, MENTION_BAND_CONTRAST,
        MENTION_TEXT_CONTRAST, REF_CHIP_EDGE_CONTRAST, REF_CHIP_TEXT_CONTRAST,
        WORK_EDGE_CONTRAST, WORK_TEXT_CONTRAST,
    };
    use crate::app::ChatApp;
    use crate::model::Message;
    use ui::theme::Theme;
    use ui::ColorCompat;

    /// Defect regression: the attachment chip must never leak the internal
    /// mount path. The demo's shared note lives at
    /// `/var/workspace/notes/moq-over-iroh.md` (see `doc.rs`), but its chip
    /// meta must show only the file type + size — no path, no "shared via".
    #[test]
    fn file_chip_meta_never_leaks_a_path() {
        let meta = file_chip_meta("moq-over-iroh.md", 18_944);
        assert!(!meta.contains('/'), "chip meta leaked a path separator: {meta:?}");
        assert!(!meta.contains("/var"), "chip meta leaked the mount path: {meta:?}");
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

    /// The chip has to be readable on every ground this app ships on.
    ///
    /// Regression: the first version painted accent ink on a translucent accent
    /// wash. On the dark theme that read cleanly; on the light theme it arrived
    /// as grey-blue on pale blue — visible, but not readable. Contrast is
    /// measured here rather than judged from one screenshot, because one
    /// screenshot is exactly how that shipped.
    #[test]
    fn the_reference_chip_is_readable_on_every_ground() {
        use ui::a11y::contrast_ratio;
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let ground = theme.tokens.colors.surfaces.base;
            let (fill, edge, ink) = reference_chip_colors(&theme, ground);

            let text = contrast_ratio(ink, fill);
            assert!(
                text >= REF_CHIP_TEXT_CONTRAST,
                "{}: the chip's label is {text:.2}:1 on its own wash",
                theme.name
            );
            let rim = contrast_ratio(edge, ground);
            assert!(
                rim >= REF_CHIP_EDGE_CONTRAST,
                "{}: the chip's edge is {rim:.2}:1 on the surface — a pill \
                 nobody can make out is not a container",
                theme.name
            );
            // The figures behind the claim, on the record rather than asserted
            // and forgotten.
            println!(
                "reference chip — {}: label {text:.2}:1 (floor {REF_CHIP_TEXT_CONTRAST}), \
                 edge {rim:.2}:1 (floor {REF_CHIP_EDGE_CONTRAST})",
                theme.name
            );
        }
    }

    /// The chip shows the author's app's own wording and never the address.
    ///
    /// Same rule as `file_chip_meta`, for the same reason plus one more: a chip
    /// that printed the id would teach a reader to resolve on the label, which
    /// is the degradation `qos-reference` exists to prevent.
    #[test]
    fn a_reference_chip_shows_the_label_and_never_the_address() {
        let r = qos_reference::ResourceRef::ranged(
            "word_processor.block",
            "word-processor.block:blk_7",
            "¶3 of Run sheet",
            qos_reference::Span::chars(12, 41),
        );
        let chip = reference_chip_label(&r);
        assert_eq!(chip, "¶3 of Run sheet");
        assert!(!chip.contains("blk_7"), "the chip leaked the resource id: {chip:?}");
        assert!(
            !chip.contains("word_processor.block"),
            "the chip leaked the internal kind tag: {chip:?}"
        );

        // A file reference must not leak the FUSE mount path either.
        let f = qos_reference::ResourceRef::whole(
            "files.entry",
            "files.entry:/var/workspace/photos/harbour.png",
            "harbour.png",
        );
        let chip = reference_chip_label(&f);
        assert_eq!(chip, "harbour.png");
        assert!(!chip.contains('/'), "the chip leaked a path separator: {chip:?}");
        assert!(
            !chip.to_lowercase().contains("workspace"),
            "the chip leaked the mount path: {chip:?}"
        );
    }

    /// An address with no human wording still gets a chip — the message really
    /// is about something — but a neutral noun, never the id.
    #[test]
    fn a_reference_without_a_label_says_so_without_naming_the_address() {
        let r = qos_reference::ResourceRef::whole(
            "files.entry",
            "files.entry:/var/workspace/photos/harbour.png",
            "   ",
        );
        let chip = reference_chip_label(&r);
        assert!(!chip.trim().is_empty(), "a reference drew no chip at all");
        assert!(
            !chip.contains("harbour") && !chip.contains('/'),
            "an unlabelled reference fell back to its address: {chip:?}"
        );
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

    /// A mention is painted in the participant's own colour, and a colour
    /// chosen to tell people apart is not a colour chosen to be read. Measured,
    /// across every shipped ground and every participant colour, because
    /// eyeballing one theme is exactly how a run ends up invisible in another.
    #[test]
    fn a_mention_clears_the_contrast_floors_on_every_ground() {
        use ui::a11y::contrast_ratio;
        let app = ChatApp::new_ephemeral();
        // Ids spanning the palette: `participant_color` hashes the id, so this
        // samples the whole ring rather than one lucky hue.
        let ids: Vec<String> = (0..24).map(|i| format!("u_participant_{i}")).collect();
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let ground = theme.tokens.colors.surfaces.base;
            let (mut worst_text, mut worst_band) = (f64::MAX, f64::MAX);
            for id in &ids {
                for is_self in [false, true] {
                    let (band, ink) = mention_colors(&app, id, is_self, &theme, ground);
                    // Whatever the run lands on, the text on it must clear AA.
                    let behind = band.unwrap_or(ground);
                    let text = contrast_ratio(ink, behind);
                    assert!(
                        text >= MENTION_TEXT_CONTRAST,
                        "{} / {id} / self={is_self}: mention text is {text:.2}:1 on what it sits on",
                        theme.name
                    );
                    worst_text = worst_text.min(text);
                    if let Some(band) = band {
                        let band_r = contrast_ratio(band, ground);
                        assert!(
                            band_r >= MENTION_BAND_CONTRAST,
                            "{} / {id}: the filled band is {band_r:.2}:1 on the surface",
                            theme.name
                        );
                        worst_band = worst_band.min(band_r);
                    }
                }
            }
            // The measured floor, printed so the figures behind the claim are
            // on the record rather than asserted and forgotten.
            println!(
                "mention contrast — {}: worst text {worst_text:.2}:1 (floor {MENTION_TEXT_CONTRAST}), \
                 worst filled band on surface {worst_band:.2}:1 (floor {MENTION_BAND_CONTRAST})",
                theme.name
            );
        }
    }

    /// A mention of you must not read like a mention of somebody else — that
    /// difference is the whole reason to render it at all.
    #[test]
    fn your_own_mention_is_told_apart_from_everybody_elses() {
        let app = ChatApp::new_ephemeral();
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let ground = theme.tokens.colors.surfaces.base;
            for id in ["u_me", "u_participant_3", "u_participant_11"] {
                let (mine, _) = mention_colors(&app, id, true, &theme, ground);
                let (theirs, _) = mention_colors(&app, id, false, &theme, ground);
                // Different in kind, not in degree: yours is filled, theirs is
                // not. A difference of tint alone does not survive every ground.
                assert!(
                    mine.is_some(),
                    "{}: a mention of you must be findable by scanning",
                    theme.name
                );
                assert!(
                    theirs.is_none(),
                    "{}: a mention of somebody else must not read as one of yours",
                    theme.name
                );
            }
        }
    }

    /// The runs a body is split into must reconstruct the body exactly. A
    /// renderer that drops or duplicates a byte is a renderer showing something
    /// nobody wrote.
    #[test]
    fn the_styled_runs_reassemble_the_body_they_came_from() {
        let app = ChatApp::new_ephemeral();
        let theme = Theme::dark();
        let text = "cc @Aria Chen and @Scribe — see the thread";
        let room = [
            crate::mention::Participant { id: "u_aria".into(), name: "Aria Chen".into(), is_agent: false },
            crate::mention::Participant { id: "agent_scribe".into(), name: "Scribe".into(), is_agent: true },
        ];
        let mut m = crate::model::Message {
            id: "m1".into(),
            channel: "c".into(),
            author: "u_me".into(),
            ts: 0,
            text: text.into(),
            edited: false,
            history: vec![],
            parent: None,
            reactions: vec![],
            mentions: crate::mention::resolve(text, &room),
            reference: None,
            attachment: None,
            pinned: false,
            work: None,
        };
        assert_eq!(m.mentions.len(), 2);
        let spans = body_spans(&app, &m, &theme, theme.tokens.colors.surfaces.base);
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, text, "the runs must be the body, exactly");
        assert_eq!(
            spans.iter().filter(|s| s.color.is_some()).count(),
            2,
            "each mention is styled as one"
        );
        assert!(
            spans.iter().all(|s| s.background.is_none()),
            "neither mention is of the local peer, so neither is filled"
        );

        // An offset left over from an edit that did not re-resolve points at
        // the wrong words — better plain prose than a highlight over them.
        m.mentions[0].len = 9_999;
        let spans = body_spans(&app, &m, &theme, theme.tokens.colors.surfaces.base);
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, text, "a stale offset must not eat the body");
        assert_eq!(
            spans.iter().filter(|s| s.color.is_some()).count(),
            1,
            "the stale reference is dropped, the sound one kept"
        );
    }

    // ------------------------------------------------------ agent work strip

    fn work(state: crate::model::AgentWorkState) -> crate::model::AgentWork {
        crate::model::AgentWork::new(state, 50_400)
    }

    /// The strip must be readable in every theme, for the same reason the
    /// reference chip's colours are measured rather than eyeballed: a tone is
    /// a colour chosen to *mean* something, and none of the four was chosen to
    /// be read on whatever ground the theme puts behind it.
    #[test]
    fn the_work_strip_is_readable_in_every_tone_and_every_theme() {
        use ui::a11y::contrast_ratio;
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let ground = theme.tokens.colors.surfaces.base;
            for tone in [WorkTone::Live, WorkTone::Done, WorkTone::Stopped, WorkTone::Silent] {
                let (fill, edge, ink) = work_strip_colors(&theme, ground, tone);
                let text = contrast_ratio(ink, fill);
                assert!(
                    text >= WORK_TEXT_CONTRAST,
                    "{} / {tone:?}: the strip's label is {text:.2}:1 on its own wash",
                    theme.name
                );
                let border = contrast_ratio(edge, ground);
                assert!(
                    border >= WORK_EDGE_CONTRAST,
                    "{} / {tone:?}: the strip's edge is {border:.2}:1 on the surface",
                    theme.name
                );
                println!(
                    "work strip — {} / {tone:?}: label {text:.2}:1, edge {border:.2}:1",
                    theme.name
                );
            }
        }
    }

    /// An unfinished record whose author has left the room must not read as
    /// work in progress.
    ///
    /// The failure this exists to prevent is the whole reason liveness is a
    /// separate question: an agent that is killed mid-instruction cannot
    /// retract what it wrote, so a strip that said "is working" would go on
    /// saying it for as long as the channel exists — a spinner that lies,
    /// which is worse than the silence it replaced.
    #[test]
    fn a_record_nobody_is_behind_never_claims_to_be_working() {
        use crate::model::AgentWorkState as S;
        for state in [
            S::Taken,
            S::Waiting { app: "Photos".into() },
            S::Doing { did: "crop".into(), about: "the harbour".into() },
        ] {
            let w = work(state.clone());
            let live = work_line(&w, "Claude", "", true);
            let gone = work_line(&w, "Claude", "", false);
            assert_eq!(
                live.tone,
                WorkTone::Live,
                "{state:?} with the agent present should read as in flight"
            );
            assert_eq!(
                gone.tone,
                WorkTone::Silent,
                "{state:?} with the agent gone must not read as in flight"
            );
            assert_ne!(
                live.head, gone.head,
                "{state:?} must not be worded the same whether or not anybody is behind it"
            );
            assert!(
                !gone.head.contains("is working") && !gone.head.contains("is reading"),
                "a record nobody is behind said {:?}, which is the present tense",
                gone.head
            );
        }
    }

    /// A finished record says the same thing whether or not the agent that
    /// wrote it is still here. What happened, happened.
    #[test]
    fn a_finished_record_does_not_depend_on_the_agent_still_being_here() {
        use crate::model::AgentWorkState as S;
        for state in [S::Done { did: "crop".into() }, S::Refused] {
            let w = work(state.clone());
            assert_eq!(
                work_line(&w, "Claude", "why not", true).head,
                work_line(&w, "Claude", "why not", false).head,
                "{state:?} was reworded by the agent leaving"
            );
        }
    }

    /// A refusal has to carry its reason where the channel can be read without
    /// opening the thread. A strip that only said "stopped" would replace one
    /// silence with another.
    #[test]
    fn a_refusal_carries_its_reason_into_the_timeline() {
        let w = work(crate::model::AgentWorkState::Refused);
        let why = "Claude: I'm not allowed to crop here";
        assert_eq!(work_line(&w, "Claude", why, true).detail, why);
        // …and not where the reply's own body is already on screen beside it.
        assert!(work_line(&w, "Claude", "", true).detail.is_empty());
    }

    /// Every wording the strip can show has to come out of the catalogue. A
    /// key with no entry resolves to itself, which ships the key to a screen.
    #[test]
    fn every_state_the_strip_can_show_resolves_to_a_real_string() {
        use crate::model::AgentWorkState as S;
        let states = [
            S::Taken,
            S::Waiting { app: "Photos".into() },
            S::Doing { did: "crop".into(), about: String::new() },
            S::Doing { did: "crop".into(), about: "the harbour".into() },
            S::Done { did: "crop".into() },
            S::Refused,
        ];
        for state in states {
            for present in [true, false] {
                let head = work_line(&work(state.clone()), "Claude", "", present).head;
                assert!(
                    head.starts_with("Claude"),
                    "{state:?}/{present}: {head:?} did not interpolate the agent's name"
                );
                assert!(
                    !head.contains("chat-agent-"),
                    "{state:?}/{present}: {head:?} is an unresolved catalogue key"
                );
                assert!(
                    head.contains("14:00"),
                    "{state:?}/{present}: {head:?} does not say when"
                );
            }
        }
    }

    /// The record belongs to the agent that wrote it, and the roster's live
    /// line only shows work that is genuinely in flight.
    #[test]
    fn the_roster_line_shows_only_unfinished_work_by_an_agent_that_is_here() {
        use crate::model::AgentWorkState as S;
        let mut app = ChatApp::new_ephemeral();
        app.messages = vec![
            reply_with("m_1", "u_claude", work(S::Done { did: "crop".into() })),
            reply_with("m_2", "u_claude", work(S::Doing { did: "crop".into(), about: String::new() })),
        ];
        // No roster yet: nothing is known about who is here, so an in-flight
        // record is shown rather than denied.
        assert!(app.agent_in_flight("u_claude").is_some());
        // The room answers, and it does not list the agent.
        app.room_names.insert("u_someone".into(), "Someone".into());
        assert!(
            app.agent_in_flight("u_claude").is_none(),
            "an agent the room does not list has nothing in flight"
        );
        // The room lists it again.
        app.room_names.insert("u_claude".into(), "Claude".into());
        assert!(app.agent_in_flight("u_claude").is_some());
        // A finished record is not in flight.
        app.messages[1].work = Some(work(S::Refused));
        assert!(
            app.agent_in_flight("u_claude").is_none(),
            "a finished record is not work in flight"
        );
    }

    /// A strip must never put a raw participant id in front of a person.
    ///
    /// The case it happens in is exactly the case the strip exists for: the
    /// record is read after the agent has gone, and a name belongs to the
    /// room, which forgets a participant on leave. What the message that
    /// summoned it called it is still the room's answer and is still in the
    /// body, so that is what the strip falls back to.
    #[test]
    fn a_departed_agent_is_named_and_never_shown_its_id() {
        use crate::model::AgentWorkState as S;
        let mut app = ChatApp::new_ephemeral();
        let asked = "@Scribe crop the harbour shot";
        let room = [crate::mention::Participant {
            id: "agent_scribe".into(),
            name: "Scribe".into(),
            is_agent: true,
        }];
        let mut summons = reply_with("m_asked", "u_aria", work(S::Taken));
        summons.parent = None;
        summons.work = None;
        summons.text = asked.into();
        summons.mentions = crate::mention::resolve(asked, &room);
        assert_eq!(summons.mentions.len(), 1, "the fixture must carry the mention");
        let record = reply_with("m_rec", "agent_scribe", work(S::Taken));
        app.messages = vec![summons, record.clone()];
        // The room lists somebody else, so the agent has left and nothing
        // anywhere holds a name for it.
        app.room_names.insert("u_aria".into(), "Aria Chen".into());
        let name = app.agent_label(&record);
        assert_eq!(name, "Scribe");
        let head = work_line(&work(S::Taken), &name, "", false).head;
        assert!(
            !head.contains("agent_scribe"),
            "the strip showed the participant id: {head:?}"
        );
    }

    fn reply_with(id: &str, author: &str, w: crate::model::AgentWork) -> Message {
        Message {
            id: id.into(),
            channel: "c_general".into(),
            author: author.into(),
            ts: 50_400,
            text: String::new(),
            edited: false,
            history: vec![],
            parent: Some("m_asked".into()),
            reactions: vec![],
            mentions: vec![],
            reference: None,
            attachment: None,
            pinned: false,
            work: Some(w),
        }
    }
}

/// Horizontal centre of the workspace rail (column of square buttons).
const RAIL_W_CENTER: f64 = crate::app::RAIL_W / 2.0;

#[cfg(test)]
mod agent_trace_tests {
    use super::{reasoning_head, said_covers, trace_row, WorkTone};
    use crate::model::{AgentReasoning, AgentRoute, AgentStep, AgentStepOutcome};
    use ui::icons::Icon;

    fn step(label: &str, outcome: AgentStepOutcome) -> AgentStep {
        AgentStep {
            resource: "photos.image:harbour".into(),
            label: label.into(),
            outcome,
        }
    }

    /// Each outcome gets its own mark **and** its own tone. The mark is the
    /// load-bearing one: the wash a row sits on is mostly ground, which is
    /// not enough to tell three outcomes apart, and would be nothing at all
    /// to a reader who cannot separate them by hue.
    #[test]
    fn an_outcome_is_answerable_from_the_shape_and_not_only_the_colour() {
        let applied = trace_row(&step("Crop", AgentStepOutcome::Applied), "");
        let nothing = trace_row(&step("Crop", AgentStepOutcome::NoChange), "");
        let refused = trace_row(
            &step("Delete", AgentStepOutcome::Refused { why: "not allowed".into() }),
            "",
        );
        let marks = [applied.icon, nothing.icon, refused.icon];
        assert_eq!(
            marks.len(),
            marks.iter().collect::<std::collections::HashSet<_>>().len(),
            "two outcomes share a mark, so they can only be told apart by colour"
        );
        assert_eq!(applied.icon, Icon::Check);
        assert_eq!(applied.tone, WorkTone::Done);
        // An operation that ran and changed nothing is not a success. A tick
        // here would send a reader looking for an edit nobody made.
        assert_ne!(
            nothing.tone,
            WorkTone::Done,
            "'changed nothing' must not be toned as work carried out"
        );
        assert!(
            nothing.label.to_lowercase().contains("crop"),
            "the row still has to name which operation changed nothing: {:?}",
            nothing.label
        );
        assert_eq!(refused.tone, WorkTone::Stopped);
    }

    /// A refusal carries its reason, unless the sentence above it already
    /// said the same thing — in which case the row says only which operation
    /// was refused. Saying it twice reads as two separate problems.
    #[test]
    fn a_refusal_says_why_once_and_not_twice() {
        let s = step(
            "Delete",
            AgentStepOutcome::Refused { why: "I'm not allowed to delete here".into() },
        );
        let alone = trace_row(&s, "");
        assert!(
            alone.label.contains("not allowed"),
            "with nothing above it, the row is the only place the reason can go: {:?}",
            alone.label
        );
        let beneath = trace_row(&s, "Scribe: I'm not allowed to delete here.");
        assert_eq!(
            beneath.label, "Delete",
            "the reason is already on the line above, so the row must not repeat it"
        );
    }

    /// The comparison is on the words, not the bytes. The reply is a signed,
    /// punctuated sentence wrapped around the same clause, so the two are
    /// never identical even when they say the same thing.
    #[test]
    fn the_sentence_above_is_matched_on_its_words() {
        assert!(said_covers("Scribe: I'm not allowed to delete here.", "I'm not allowed to delete here"));
        assert!(said_covers("Scribe: I'm not allowed to delete here.", "I'm not allowed to delete here."));
        assert!(!said_covers("Scribe: done — crop.", "I'm not allowed to delete here"));
        // An empty reason is covered by nothing, so it can never suppress a
        // row's own words by accident.
        assert!(!said_covers("anything at all", "   "));
    }

    /// The line that opens the reasoning says which path decided. A turn no
    /// model answered must not read like one it did — the whole reason the
    /// route is recorded rather than assumed.
    #[test]
    fn the_disclosure_says_when_no_model_decided() {
        let by_model = reasoning_head(&AgentReasoning {
            route: AgentRoute::Model,
            why: "because".into(),
        });
        let by_floor = reasoning_head(&AgentReasoning {
            route: AgentRoute::Floor,
            why: "no api key".into(),
        });
        assert_ne!(
            by_model, by_floor,
            "both routes read identically, so a turn decided without a model \
             is indistinguishable from one a model chose"
        );
        assert!(
            !by_model.starts_with("chat-agent-"),
            "the string did not resolve through the catalogue: {by_model:?}"
        );
        assert!(
            !by_floor.starts_with("chat-agent-"),
            "the string did not resolve through the catalogue: {by_floor:?}"
        );
    }
}
