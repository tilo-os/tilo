//! The peek: a small picture of someone's screen beside the people
//! overlay, and Follow.
//!
//! Nothing of their screen is streamed. The picture is built from what
//! their presence says: where their windows stand, which app each is, and
//! which one is in front. A window on something shared with this person
//! shows as their own copy of it (its live picture, or its title); any
//! other shows its app and nothing more. Someone who keeps where they are
//! to themselves shows no picture at all.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Fill};

use ui::TextRole;
use ui::icons::Icon;
use ui::motion::{Appear, Presence};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::time::Instant;
use ui::tokens::EffectiveMotion;
use ui::widget::{ButtonFace, ButtonRole};

use super::{FACE, PeekPictures, PeerScreen, PeerWindow, row_line};
use crate::collaborators::RosterRow;
use crate::desk_edges::DockEdge;
use crate::i18n::{t, t_args};

/// What one frame of the peek shows.
pub(super) struct PeekModel<'a> {
    pub host: Rect,
    /// The overlay's panel and the row the peek opened from.
    pub panel: Rect,
    pub anchor: Rect,
    /// The edge the panel rests on: the peek opens away from it.
    pub edge: DockEdge,
    pub row: &'a RosterRow,
    pub screen: Option<&'a PeerScreen>,
    pub following: bool,
    pub cursor: Option<Point>,
}

/// The peek across frames: whose, how open, and where its parts were drawn.
#[derive(Default)]
pub(super) struct Peek {
    person: Option<String>,
    shown: Presence,
    /// The card as laid out (at rest), and as drawn now.
    card: Rect,
    pub(super) drawn: Rect,
    follow: Rect,
}

/// The gap between the panel and the peek, and the peek and its message
/// box.
pub(super) fn gap(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s2)
}

impl Peek {
    pub(super) fn person(&self) -> Option<&str> {
        self.person.as_deref()
    }

    pub(super) fn open(&mut self, id: &str, motion: EffectiveMotion, now: Instant) {
        if self.person.as_deref() != Some(id) || !self.shown.is_open() {
            // Another person's peek replaces this one in place: it does not
            // close and open again.
            if self.person.is_none() || !self.shown.is_visible(now) {
                self.shown = Presence::closed();
            }
            self.person = Some(id.to_owned());
            self.shown.show(
                true,
                Appear::SPEC,
                motion,
                ui::tokens::MotionSource::Pointer,
                now,
            );
            if let Some(until) = self.shown.settles_at() {
                ui::motion::want_frames_until(until);
            }
        }
    }

    pub(super) fn close(&mut self, motion: EffectiveMotion) {
        let now = ui::time::now();
        if self.shown.is_open() {
            self.shown.show(
                false,
                Appear::SPEC,
                motion,
                ui::tokens::MotionSource::Pointer,
                now,
            );
            if let Some(until) = self.shown.settles_at() {
                ui::motion::want_frames_until(until);
            }
        }
        if !self.shown.is_visible(now) {
            self.person = None;
        }
        self.follow = Rect::ZERO;
    }

    /// How open the peek is, 0 to 1.
    pub(super) fn openness(&self, now: Instant) -> f64 {
        self.shown.progress(now)
    }

    pub(super) fn is_moving(&self, now: Instant) -> bool {
        self.shown.is_moving(now)
    }

    pub(super) fn settles_at(&self) -> Option<Instant> {
        self.shown.settles_at()
    }

    /// Whether `point` is on the peek as drawn.
    pub(super) fn covers(&self, point: Point) -> bool {
        self.person.is_some() && self.shown.is_open() && self.drawn.contains(point)
    }

    /// The peek's controls, by name, where they take presses.
    pub(super) fn targets(&self) -> Vec<(&'static str, Rect)> {
        if self.shown.is_open() && self.follow.area() > 0.0 {
            vec![("follow", self.follow)]
        } else {
            Vec::new()
        }
    }

    /// Paint the peek; returns the card as laid out, for the message box to
    /// stand beside.
    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &PeekModel<'_>,
        pictures: &mut dyn PeekPictures,
    ) -> Rect {
        let now = ui::motion::frame_now();
        if !self.shown.is_visible(now) {
            if !self.shown.is_open() {
                self.person = None;
            }
            self.drawn = Rect::ZERO;
            self.follow = Rect::ZERO;
            return Rect::ZERO;
        }
        let spacing = &theme.tokens.spacing;
        let pad = f64::from(spacing.s3);
        let width = f64::from(theme.tokens.metrics.panel_width_utility);
        let mut header_face = ui::widget::ListRowFace::new("Ag")
            .secondary("Ag")
            .leading_column(FACE);
        if row_line(model.row).is_some_and(|line| line.place.is_some()) {
            header_face = header_face.subtitle("Ag");
        }
        let header_h = header_face.height(text, theme);
        let screen_w = width - 2.0 * pad;
        let aspect = model
            .screen
            .map_or(16.0 / 10.0, |screen| screen.aspect)
            .clamp(1.0, 2.6);
        let screen_h = (screen_w / aspect).round();
        let control = f64::from(theme.tokens.metrics.bar_control_height);
        let height = pad
            + header_h
            + f64::from(spacing.s2)
            + screen_h
            + f64::from(spacing.s3)
            + control
            + pad;
        let reserve = super::composer::height(theme, false) + gap(theme);
        self.card = place(model, (width, height), reserve, theme);
        let card = self.card;
        let progress = self.shown.progress(now);
        let origin = Appear::origin(card, model.anchor.center());
        let pose = Appear::pose(progress, origin, theme.motion());
        self.drawn = pose.drawn_rect(card);
        let placed = pose.applied_to(transform);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            pose.opacity,
            placed,
            &ui::widget::floating_surface::painted_bounds(card, theme),
        );
        ui::widget::floating_surface::paint(scene, placed, card, theme);
        let inner = card.inset(-pad);
        let header = Rect::new(inner.x0, inner.y0, inner.x1, inner.y0 + header_h);
        paint_header(
            &mut ui::paint::PaintCtx::new(scene, placed, text, theme),
            header,
            model.row,
        );
        let screen = Rect::new(
            inner.x0,
            header.y1 + f64::from(spacing.s2),
            inner.x1,
            header.y1 + f64::from(spacing.s2) + screen_h,
        );
        paint_screen(
            scene,
            placed,
            text,
            theme,
            screen,
            (model.row, model.screen),
            pictures,
        );
        let follow = paint_follow(
            &mut ui::paint::PaintCtx::new(scene, placed, text, theme),
            Rect::new(inner.x0, inner.y1 - control, inner.x1, inner.y1),
            model,
        );
        self.follow = if progress >= 0.999 {
            follow
        } else {
            Rect::ZERO
        };
        scene.pop_layer();
        card
    }
}

/// The peek's header: their face, name and what they are doing (with
/// where they are), software's line marked as such.
fn paint_header(ctx: &mut ui::paint::PaintCtx<'_>, header: Rect, row: &RosterRow) {
    let line = row_line(row);
    let fallback = t("people-overlay-doing-nothing");
    let (doing, place, generated) = line
        .as_ref()
        .map_or((fallback.as_str(), None, false), |line| {
            (line.doing.as_str(), line.place.as_deref(), line.automatic)
        });
    let paint_face = |ctx: &mut ui::paint::PaintCtx<'_>, rect: Rect| {
        let mut face = ui::presence::Face::new(&row.label, &row.id, row.color);
        face.is_agent = row.is_agent;
        face.dimmed = row.availability.dims();
        ui::presence::FaceStack {
            side: FACE - 4.0,
            overlap: 0.0,
            max: 1,
        }
        .paint(
            ctx,
            (rect.center().x + FACE / 2.0, rect.center().y),
            (&[face], 0),
            ui::widget::SurfaceLevel::Floating.color(ctx.theme),
        );
    };
    let mut stack = ui::widget::ListRowFace::new(&row.label)
        .leading(ui::widget::RowLeading::Paint(&paint_face))
        .leading_column(FACE);
    stack = match place {
        Some(place) => stack.subtitle(doing).secondary(place),
        None => stack.secondary(doing),
    };
    if generated {
        stack = stack.mark(Icon::Sparkles);
    }
    let bleed = ui::chrome::row_bleed(ctx.theme);
    stack.paint(ctx, header.inflate(bleed, 0.0));
}

/// Follow (or Stop following) at the start of `slot`; returns where it is,
/// `Rect::ZERO` for someone who cannot be followed.
fn paint_follow(ctx: &mut ui::paint::PaintCtx<'_>, slot: Rect, model: &PeekModel<'_>) -> Rect {
    if !model.row.followable {
        return Rect::ZERO;
    }
    let label = if model.following {
        t("people-panel-stop-following")
    } else {
        t_args(
            "people-overlay-follow-name",
            &[("name", model.row.label.as_str().into())],
        )
    };
    let icon = if model.following {
        Icon::EyeOff
    } else {
        Icon::Eye
    };
    let width = ButtonFace::new(Rect::ZERO, &label)
        .icon(icon)
        .intrinsic_width(ctx.text, ctx.theme);
    let follow = Rect::new(slot.x0, slot.y0, (slot.x0 + width).min(slot.x1), slot.y1);
    ui::widget::Button::paint(
        ctx.scene,
        ctx.transform,
        ctx.text,
        ctx.theme,
        &ButtonFace::new(follow, &label)
            .icon(icon)
            .role(if model.following {
                ButtonRole::Secondary
            } else {
                ButtonRole::Primary
            })
            .hovered(model.cursor.is_some_and(|p| follow.contains(p))),
    );
    follow
}

/// Where the card of `size` goes: beside the panel, away from the edge it
/// rests on, level with the row it opened from, kept on screen with
/// `reserve` free under it for the message box.
fn place(model: &PeekModel<'_>, (width, height): (f64, f64), reserve: f64, theme: &Theme) -> Rect {
    let gap = gap(theme);
    let margin = f64::from(theme.tokens.spacing.s3);
    let host = model.host;
    let clamp_x = |x: f64| {
        x.clamp(
            host.x0 + margin,
            (host.x1 - margin - width).max(host.x0 + margin),
        )
    };
    let clamp_y = |y: f64| {
        y.clamp(
            host.y0 + margin,
            (host.y1 - margin - height - reserve).max(host.y0 + margin),
        )
    };
    let (x, y) = match model.edge {
        DockEdge::Right | DockEdge::BottomRight => (
            model.panel.x0 - gap - width,
            clamp_y(model.anchor.center().y - height / 2.0),
        ),
        DockEdge::Left | DockEdge::BottomLeft => (
            model.panel.x1 + gap,
            clamp_y(model.anchor.center().y - height / 2.0),
        ),
        DockEdge::Bottom => (
            clamp_x(model.panel.x0),
            (model.panel.y0 - gap - height - reserve).max(host.y0 + margin),
        ),
    };
    Rect::new(x, y, x + width, y + height)
}

/// Paint their screen into `rect`: their windows where they stand, or, when
/// they keep where they are to themselves, a line saying so.
fn paint_screen(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    (row, screen): (&RosterRow, Option<&PeerScreen>),
    pictures: &mut dyn PeekPictures,
) {
    let colors = &theme.tokens.colors;
    let radius = f64::from(theme.tokens.radii.control);
    let ground = RoundedRect::from_rect(rect, radius);
    scene.fill(
        Fill::NonZero,
        transform,
        colors.surfaces.base,
        None,
        &ground,
    );
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &ground);
    if let Some(screen) = screen.filter(|screen| !screen.windows.is_empty()) {
        // The window in front last, so it stands over the others as on
        // their screen.
        let mut order: Vec<&PeerWindow> = screen.windows.iter().collect();
        order.sort_by_key(|window| window.focused);
        for window in order {
            paint_window(scene, transform, text, theme, rect, window, row, pictures);
        }
    } else {
        let line = if screen.is_some() {
            t_args(
                "people-overlay-empty-desk",
                &[("name", row.label.as_str().into())],
            )
        } else {
            t_args(
                "people-overlay-screen-private",
                &[("name", row.label.as_str().into())],
            )
        };
        let style = theme.text(TextRole::Caption);
        let max = (rect.width() - 24.0).max(0.0);
        let shown = text.elide_to_width_style(&line, ui::num::f32_of_f64(max), &style);
        let (w, h) = text.measure_style(&shown, None, &style);
        text.draw_style(
            scene,
            &shown,
            colors.text_dim,
            transform.pre_translate(
                (
                    rect.center().x - f64::from(w) / 2.0,
                    rect.center().y - f64::from(h) / 2.0,
                )
                    .into(),
            ),
            None,
            &style,
        );
    }
    scene.pop_layer();
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        colors.surfaces.content_edge,
        None,
        &ground,
    );
}

/// One of their windows inside their screen `screen`: their copy of it
/// when shared and pictured, else its app (and title, when shared).
fn paint_window(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    screen: Rect,
    window: &PeerWindow,
    row: &RosterRow,
    pictures: &mut dyn PeekPictures,
) {
    let colors = &theme.tokens.colors;
    let inset = 1.5;
    let rect = Rect::new(
        screen.x0 + window.rect.x0 * screen.width(),
        screen.y0 + window.rect.y0 * screen.height(),
        screen.x0 + window.rect.x1 * screen.width(),
        screen.y0 + window.rect.y1 * screen.height(),
    )
    .inset(-inset);
    if rect.width() < 4.0 || rect.height() < 4.0 {
        return;
    }
    let shape = RoundedRect::from_rect(rect, 4.0);
    let ground = if window.shared() {
        colors.surfaces.content
    } else {
        // Not shared with this person: a plain tile, quieter than a window
        // they can see.
        colors.surfaces.raised
    };
    scene.fill(Fill::NonZero, transform, ground, None, &shape);
    let live = window.picture.is_some_and(|window_id| {
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &shape);
        let painted = pictures.paint(window_id, scene, transform, rect);
        scene.pop_layer();
        painted
    });
    if !live {
        let ink = colors.text_dim;
        let icon = (rect.height() * 0.32).clamp(10.0, 24.0);
        let label = window.title.as_deref().unwrap_or(&window.app_name);
        let style = theme
            .text(TextRole::Caption)
            .with_weight(TextWeight::Medium);
        let line_h = f64::from(theme.text(TextRole::Caption).line_height_px());
        let shows_label = rect.height() >= icon + line_h + 10.0 && rect.width() >= 40.0;
        let block = if shows_label {
            icon + 4.0 + line_h
        } else {
            icon
        };
        let top = rect.center().y - block / 2.0;
        let mark = Rect::new(
            rect.center().x - icon / 2.0,
            top,
            rect.center().x + icon / 2.0,
            top + icon,
        );
        let glyph = ui::icons::app_glyph(&window.app, None);
        let glyph = if glyph == Icon::AppWindow {
            Icon::Square
        } else {
            glyph
        };
        glyph.draw_with_stroke(scene, transform, mark, ink, 1.5);
        if shows_label {
            let max = (rect.width() - 8.0).max(0.0);
            let shown = text.elide_to_width_style(label, ui::num::f32_of_f64(max), &style);
            let (w, _) = text.measure_style(&shown, None, &style);
            text.draw_style(
                scene,
                &shown,
                ink,
                transform
                    .pre_translate((rect.center().x - f64::from(w) / 2.0, mark.y1 + 4.0).into()),
                None,
                &style,
            );
        }
    }
    let edge = if window.focused {
        row.color
    } else {
        colors.surfaces.content_edge
    };
    let width = if window.focused { 2.0 } else { 1.0 };
    scene.stroke(&Stroke::new(width), transform, edge, None, &shape);
}
