//! How each desk widget looks: its glance form, its open card at each width
//! stage ([`Stage`]), and the controls on and around it (collapse, pin,
//! remove). Geometry the layer also hit-tests (the controls, the team rows)
//! is computed here once and shared, so what is drawn is exactly what takes
//! a press.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "widget geometry is finite logical-pixel kurbo arithmetic"
)]

use ui::TextRole;
use ui::desk_widgets::WidgetId;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Size};
use vello::peniko::{BlendMode, Fill};

use super::calendar::{self, Month};
use super::sizing::Stage;
use super::system_monitor::Reading;
use super::team::{self, TeamMember};
use crate::i18n::{t, t_args};

/// The calendar's glance form: the weekday over the day of the month.
pub(super) const CALENDAR_GLANCE: Size = Size::new(52.0, 56.0);
const CALENDAR_HEADER: f64 = ui::tokens::Metrics::BASE.bar_height as f64;
const CALENDAR_WEEKDAYS: f64 = ui::tokens::Spacing::BASE.s6 as f64;
const CALENDAR_ROW: f64 = ui::tokens::Spacing::BASE.s8 as f64;
/// Team avatars in the glance form.
pub(super) const AVATAR: f64 = ui::tokens::Metrics::BASE.bar_control_height as f64;
pub(super) const AVATAR_STEP: f64 = ui::tokens::Layout::BASE.avatar_step as f64;
pub(super) const GLANCE_AVATARS: usize = 5;
const TEAM_HEADER: f64 = ui::tokens::Layout::BASE.row_header as f64;
const TEAM_ROW: f64 = ui::tokens::Layout::BASE.team_row as f64;
const TEAM_MORE: f64 = ui::tokens::Spacing::BASE.s6 as f64;
/// The band above an open card holding its collapse, pin and remove
/// controls.
pub(super) const BAND: f64 = ui::tokens::Spacing::BASE.s8 as f64;
/// One band control's square.
pub(super) const CONTROL: f64 = ui::tokens::Layout::BASE.pin as f64;
/// The remove badge on a hovered glance.
pub(super) const BADGE: f64 = ui::tokens::Layout::BASE.desk_badge as f64;
/// The system monitor's glance form: its two short readouts.
pub(super) const MONITOR_GLANCE: Size = Size::new(176.0, 40.0);
const MONITOR_HEADER: f64 = ui::tokens::Layout::BASE.row_header as f64;
const MONITOR_ROW: f64 = ui::tokens::Metrics::BASE.bar_height as f64;
/// The resize handle in a card's free corner, and the band along its free
/// side that resizes too.
pub(super) const GRIP: f64 = ui::tokens::Layout::BASE.desk_grip as f64;
const GRIP_EDGE: f64 = ui::tokens::Layout::BASE.resize_grip as f64;
/// Inset of a card's content from its edge.
const PAD: f64 = ui::tokens::Spacing::BASE.s4 as f64;

/// Run `paint` at `alpha`, in a layer only while it is fading.
pub(super) fn with_alpha(
    scene: &mut Scene,
    transform: Affine,
    clip: Rect,
    alpha: f32,
    paint: impl FnOnce(&mut Scene),
) {
    let fading = alpha < 1.0 - f32::EPSILON;
    if fading {
        scene.push_layer(Fill::NonZero, BlendMode::default(), alpha, transform, &clip);
    }
    paint(scene);
    if fading {
        scene.pop_layer();
    }
}

/// The card part of an open rect, under its band.
pub(super) fn card_rect(open: Rect) -> Rect {
    Rect::new(open.x0, open.y0 + BAND, open.x1, open.y1)
}

/// The open card's band controls: collapse at its left edge, pin and remove
/// at its right.
pub(super) fn collapse_rect(open: Rect) -> Rect {
    Rect::new(open.x0, open.y0, open.x0 + CONTROL, open.y0 + CONTROL)
}

pub(super) fn pin_rect(open: Rect) -> Rect {
    Rect::new(open.x1 - CONTROL, open.y0, open.x1, open.y0 + CONTROL)
}

pub(super) fn remove_rect(open: Rect) -> Rect {
    Rect::new(
        open.x1 - 2.0 * CONTROL - 4.0,
        open.y0,
        open.x1 - CONTROL - 4.0,
        open.y0 + CONTROL,
    )
}

/// The remove badge of a hovered glance at `glance`, on its corner toward
/// the display's middle so it is never off the screen.
pub(super) fn badge_rect(glance: Rect, (right, bottom): (bool, bool)) -> Rect {
    let x = if right { glance.x0 } else { glance.x1 };
    let y = if bottom { glance.y0 } else { glance.y1 };
    Rect::from_center_size(Point::new(x, y), (BADGE, BADGE))
}

/// The team card's rows at `stage`: one rect per person shown, under the
/// header.
pub(super) fn team_rows(card: Rect, stage: Stage, people: usize) -> Vec<Rect> {
    let (row, max) = team_row_shape(stage);
    (0..people.min(max))
        .map(|k| {
            let y = card.y0 + TEAM_HEADER + row * ui::num::f64_of_usize(k);
            Rect::new(card.x0, y, card.x1, y + row)
        })
        .collect()
}

/// A team row's height and how many rows a stage shows.
fn team_row_shape(stage: Stage) -> (f64, usize) {
    match stage {
        Stage::Compact => (40.0, team::MAX_ROWS),
        Stage::Regular => (TEAM_ROW, team::MAX_ROWS),
        Stage::Large => (TEAM_ROW + 8.0, team::MAX_ROWS + 3),
    }
}

pub(super) fn team_card_height(stage: Stage, people: usize) -> f64 {
    let (row, max) = team_row_shape(stage);
    let rows = people.min(max);
    let more = if people > max { TEAM_MORE } else { 0.0 };
    TEAM_HEADER + row * ui::num::f64_of_usize(rows) + more + 8.0
}

/// The team's glance size with `people` faces: stacked down a side edge,
/// lined up anywhere else.
pub(super) fn team_glance_size(vertical: bool, people: usize) -> Size {
    let shown = people.clamp(1, GLANCE_AVATARS);
    let long = 12.0 + AVATAR + AVATAR_STEP * ui::num::f64_of_usize(shown - 1);
    if vertical {
        Size::new(AVATAR + 12.0, long)
    } else {
        Size::new(long, AVATAR + 12.0)
    }
}

/// A calendar row's height at `stage` and `width`.
fn calendar_row(stage: Stage, width: f64) -> f64 {
    match stage {
        Stage::Compact | Stage::Regular => CALENDAR_ROW,
        Stage::Large => CALENDAR_ROW * (width / 320.0).max(1.0),
    }
}

pub(super) fn calendar_card_height(stage: Stage, width: f64) -> f64 {
    let rows = match stage {
        Stage::Compact => 1,
        Stage::Regular | Stage::Large => calendar::WEEKS,
    };
    CALENDAR_HEADER
        + CALENDAR_WEEKDAYS
        + calendar_row(stage, width) * ui::num::f64_of_usize(rows)
        + 8.0
}

fn monitor_row(stage: Stage) -> f64 {
    match stage {
        Stage::Compact | Stage::Regular => MONITOR_ROW,
        Stage::Large => MONITOR_ROW * 1.35,
    }
}

pub(super) fn monitor_card_height(stage: Stage) -> f64 {
    match stage {
        Stage::Compact => MONITOR_HEADER + 52.0,
        Stage::Regular | Stage::Large => MONITOR_HEADER + monitor_row(stage) * 3.0 + 8.0,
    }
}

/// The resize handle of the card at `card`, in its corner toward the
/// display's middle (`right`/`bottom`: the corner the card is anchored by,
/// opposite the handle).
pub(super) fn grip_rect(card: Rect, (right, bottom): (bool, bool)) -> Rect {
    let x0 = if right { card.x0 } else { card.x1 - GRIP };
    let y0 = if bottom { card.y0 } else { card.y1 - GRIP };
    Rect::from_origin_size(Point::new(x0, y0), Size::new(GRIP, GRIP))
}

/// The band along the card's free side that resizes it too.
pub(super) fn grip_edge(card: Rect, (right, _): (bool, bool)) -> Rect {
    if right {
        Rect::new(
            card.x0 - GRIP_EDGE / 2.0,
            card.y0,
            card.x0 + GRIP_EDGE / 2.0,
            card.y1,
        )
    } else {
        Rect::new(
            card.x1 - GRIP_EDGE / 2.0,
            card.y0,
            card.x1 + GRIP_EDGE / 2.0,
            card.y1,
        )
    }
}

/// The ground every desk card stands on: the one floating card
/// ([`ui::widget::floating_surface::paint_card`]), as banners and the
/// palette.
pub(super) fn paint_card_ground(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
    ui::widget::floating_surface::paint_card(scene, transform, rect, theme);
}

/// The deeper shadow of a widget lifted under the hand, `lift` of the way
/// up.
pub(super) fn paint_lift(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    rect: Rect,
    radius: f64,
    lift: f64,
) {
    if lift <= f64::EPSILON {
        return;
    }
    #[allow(clippy::cast_possible_truncation, reason = "a unit fraction")]
    let shadow = theme
        .tokens
        .elevation
        .floating
        .with_alpha_scaled(0.9 * lift as f32);
    ui::surface::shadow(
        scene,
        transform,
        rect.inflate(4.0 * lift, 6.0 * lift) + vello::kurbo::Vec2::new(0.0, 6.0 * lift),
        radius,
        &shadow,
    );
}

/// The radius a glance form's ground has.
pub(super) fn glance_radius(id: WidgetId, theme: &Theme, rect: Rect) -> f64 {
    match id {
        WidgetId::Calendar => ui::chrome::ChromeRole::Panel.radius(theme, rect.height()),
        WidgetId::Music | WidgetId::Team | WidgetId::SystemMonitor => {
            ui::chrome::ChromeRole::Selector.radius(theme, rect.height())
        }
    }
}

/// A glance form's ground, when it stands alone (not mid-expansion).
pub(super) fn paint_glance_ground(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    id: WidgetId,
    rect: Rect,
) {
    let radius = glance_radius(id, theme, rect);
    let shadow = theme.tokens.elevation.floating.with_alpha_scaled(0.6);
    ui::surface::shadow(scene, transform, rect, radius, &shadow);
    ui::surface::card(
        scene,
        transform,
        rect,
        radius,
        theme.tokens.colors.surfaces.overlay,
    );
    ui::paint::stroke_round(
        scene,
        transform,
        rect,
        radius,
        ui::chrome::panel_border(theme),
        1.0,
    );
}

fn paint_avatar(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    member: &TeamMember,
) {
    // The one face people have everywhere: one initial (or the agent mark)
    // in a ring of their colour, dimmed while idle, ringed while in a call.
    let face = ui::presence::Face {
        is_agent: member.row.is_agent,
        dimmed: member.row.availability.dims(),
        in_call: member.row.call.is_some().then_some(1.0),
        ..ui::presence::Face::new(&member.row.label, &member.row.id, member.row.color)
    };
    face.paint(
        &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
        ui::presence::Face::disc_in(rect),
        theme.tokens.colors.surfaces.overlay,
    );
}

/// The team's glance: the faces of everyone here, and how many more.
pub(super) fn paint_team_glance(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    vertical: bool,
    members: &[TeamMember],
) {
    let cell = |k: usize| {
        let offset = 6.0 + AVATAR_STEP * ui::num::f64_of_usize(k);
        let origin = if vertical {
            Point::new(rect.x0 + 6.0, rect.y0 + offset)
        } else {
            Point::new(rect.x0 + offset, rect.y0 + 6.0)
        };
        Rect::from_origin_size(origin, (AVATAR, AVATAR))
    };
    // Past the last place, a drawn count of everyone else takes it, as in
    // every face stack.
    let (shown, more) = if members.len() > GLANCE_AVATARS {
        let shown = GLANCE_AVATARS - 1;
        (shown, members.len() - shown)
    } else {
        (members.len(), 0)
    };
    for (k, member) in members.iter().take(shown).enumerate() {
        paint_avatar(scene, transform, text, theme, cell(k), member);
    }
    if more > 0 {
        ui::widget::AvatarFace::paint_overflow(
            &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
            ui::presence::Face::disc_in(cell(shown)),
            more,
            Some(theme.tokens.colors.surfaces.overlay),
        );
    }
}

fn draw_header(
    text: &mut TextRenderer,
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    card: Rect,
    key: &str,
    band: f64,
) {
    let caption = theme.text(TextRole::Caption);
    let title = t(key);
    let (_, title_h) = text.measure_style(&title, None, &caption);
    text.draw_style(
        scene,
        &title,
        theme.tokens.colors.text_dim,
        transform
            .pre_translate((card.x0 + PAD, card.y0 + (band - f64::from(title_h)) / 2.0).into()),
        None,
        &caption,
    );
}

/// The team's open card: everyone here, each with what they are doing and
/// where they are.
#[allow(
    clippy::cast_possible_truncation,
    reason = "widths are bounded by the display; f32 is the shaping precision"
)]
#[allow(clippy::too_many_lines, reason = "one card's rows at each stage")]
pub(super) fn paint_team_card(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    card: Rect,
    members: &[TeamMember],
    stage: Stage,
    cursor: Option<Point>,
) {
    let colors = &theme.tokens.colors;
    let caption = theme.text(TextRole::Caption);
    let name_style = theme.text(TextRole::Body).with_weight(TextWeight::Semibold);
    draw_header(
        text,
        scene,
        transform,
        theme,
        card,
        "desk-team-title",
        TEAM_HEADER,
    );
    let avatar = match stage {
        Stage::Compact => 28.0,
        Stage::Regular => 32.0,
        Stage::Large => 40.0,
    };
    let text_x = card.x0 + PAD + avatar + 10.0;
    let width = (card.x1 - PAD - text_x).max(1.0) as f32;
    let rows = team_rows(card, stage, members.len());
    for (member, row) in members.iter().zip(rows.iter()) {
        if cursor.is_some_and(|p| row.contains(p)) {
            ui::surface::card(
                scene,
                transform,
                row.inset((-4.0, -1.0)),
                6.0,
                ui::paint::with_alpha_scaled(colors.text_primary, 0.06),
            );
        }
        let top = if stage == Stage::Compact {
            row.center().y - avatar / 2.0
        } else {
            row.y0 + 6.0
        };
        paint_avatar(
            scene,
            transform,
            text,
            theme,
            Rect::new(card.x0 + PAD, top, card.x0 + PAD + avatar, top + avatar),
            member,
        );
        let name = text.elide_to_width_style(&member.row.label, width, &name_style);
        let (_, name_h) = text.measure_style(&name, None, &name_style);
        let mut line_y = if stage == Stage::Compact {
            row.center().y - f64::from(name_h) / 2.0
        } else {
            row.y0 + 4.0
        };
        text.draw_style(
            scene,
            &name,
            colors.text_primary,
            transform.pre_translate((text_x, line_y).into()),
            None,
            &name_style,
        );
        if stage == Stage::Compact {
            continue;
        }
        line_y += f64::from(name_h);
        if let Some(status) = member.row.status.as_ref() {
            // Who wrote the line: a pencil for their own words, sparkles
            // for a line their machine generated.
            let icon = match status.source {
                crate::collaborators::LineSource::Written => ui::icons::Icon::Pencil,
                crate::collaborators::LineSource::Generated
                | crate::collaborators::LineSource::Observed => ui::icons::Icon::Sparkles,
            };
            let (_, line_h) = text.measure_style("Ag", None, &caption);
            let mark = Rect::from_origin_size(
                Point::new(text_x, line_y + (f64::from(line_h) - 11.0) / 2.0),
                (11.0, 11.0),
            );
            ui::icons::draw_icon(scene, transform, mark, icon, colors.text_dim, 1.2);
            let line = text.elide_to_width_style(&status.text, (width - 16.0).max(1.0), &caption);
            text.draw_style(
                scene,
                &line,
                colors.text_primary,
                transform.pre_translate((text_x + 16.0, line_y).into()),
                None,
                &caption,
            );
            line_y += f64::from(line_h);
        }
        if let Some(place) = member.place.as_ref() {
            let line = text.elide_to_width_style(place, width, &caption);
            text.draw_style(
                scene,
                &line,
                colors.text_dim,
                transform.pre_translate((text_x, line_y).into()),
                None,
                &caption,
            );
        }
    }
    if members.len() > rows.len() {
        let more = t_args(
            "desk-team-more",
            &[("count", (members.len() - rows.len()).into())],
        );
        let y = rows.last().map_or(card.y0 + TEAM_HEADER, |row| row.y1);
        text.draw_style(
            scene,
            &more,
            colors.text_dim,
            transform.pre_translate((text_x, y + 2.0).into()),
            None,
            &caption,
        );
    }
}

/// Draw `line` centred on `center` in `style` and `color`.
fn draw_centered(
    text: &mut TextRenderer,
    scene: &mut Scene,
    transform: Affine,
    (line, center): (&str, Point),
    style: &ui::text_render::TextStyle,
    color: vello::peniko::Color,
) {
    let (w, h) = text.measure_style(line, None, style);
    text.draw_style(
        scene,
        line,
        color,
        transform
            .pre_translate((center.x - f64::from(w) / 2.0, center.y - f64::from(h) / 2.0).into()),
        None,
        style,
    );
}

/// The calendar's glance: the weekday over the day of the month.
pub(super) fn paint_calendar_glance(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    month: &Month,
) {
    use chrono::Datelike as _;
    let colors = &theme.tokens.colors;
    let weekday = ui::format::weekday_short(month.today.weekday());
    draw_centered(
        text,
        scene,
        transform,
        (&weekday, Point::new(rect.center().x, rect.y0 + 15.0)),
        &theme
            .text(TextRole::Caption)
            .with_weight(TextWeight::Semibold),
        colors.text_dim,
    );
    draw_centered(
        text,
        scene,
        transform,
        (
            &month.today.day().to_string(),
            Point::new(rect.center().x, rect.y1 - 20.0),
        ),
        &theme
            .text(TextRole::Title)
            .with_weight(TextWeight::Semibold),
        colors.text_primary,
    );
}

/// The calendar's open card: the month from the person's first weekday,
/// today marked; compact, only today's week; large, roomier rows.
pub(super) fn paint_calendar_card(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    card: Rect,
    month: &Month,
    stage: Stage,
) {
    let colors = &theme.tokens.colors;
    let heading = theme.text(TextRole::Body).with_weight(TextWeight::Semibold);
    let title = ui::format::month_heading(month.first);
    let (_, title_h) = text.measure_style(&title, None, &heading);
    text.draw_style(
        scene,
        &title,
        colors.text_primary,
        transform.pre_translate(
            (
                card.x0 + PAD,
                card.y0 + (CALENDAR_HEADER - f64::from(title_h)) / 2.0,
            )
                .into(),
        ),
        None,
        &heading,
    );
    let grid_x0 = card.x0 + 10.0;
    let cell_w = (card.width() - 20.0) / 7.0;
    let center_x = |col: usize| grid_x0 + cell_w * (ui::num::f64_of_usize(col) + 0.5);
    let caption = theme.text(TextRole::Caption);
    let weekdays_y = card.y0 + CALENDAR_HEADER + CALENDAR_WEEKDAYS / 2.0;
    for (col, weekday) in month.columns.iter().enumerate() {
        let label = ui::format::weekday_short(*weekday);
        draw_centered(
            text,
            scene,
            transform,
            (&label, Point::new(center_x(col), weekdays_y)),
            &caption,
            colors.text_dim,
        );
    }
    let today = month.today_cell();
    let body = if stage == Stage::Large {
        theme.text(TextRole::Title)
    } else {
        theme.text(TextRole::Body)
    };
    let row_h = calendar_row(stage, card.width());
    let rows_y0 = card.y0 + CALENDAR_HEADER + CALENDAR_WEEKDAYS;
    let weeks: Vec<(usize, &[Option<u32>; 7])> = match stage {
        Stage::Compact => today
            .and_then(|(row, _)| month.weeks.get(row).map(|week| (row, week)))
            .into_iter()
            .collect(),
        Stage::Regular | Stage::Large => month.weeks.iter().enumerate().collect(),
    };
    for (k, (row, week)) in weeks.into_iter().enumerate() {
        let cy = rows_y0 + row_h * (ui::num::f64_of_usize(k) + 0.5);
        for (col, day) in week.iter().enumerate() {
            let Some(day) = day else {
                continue;
            };
            let center = Point::new(center_x(col), cy);
            // Today: an inverted disc, the way Tilo marks the one choice.
            let ink = if today == Some((row, col)) {
                let side = (row_h - 6.0).min(cell_w - 4.0).max(20.0);
                let disc = Rect::from_center_size(center, (side, side));
                ui::surface::card(scene, transform, disc, side / 2.0, colors.text_primary);
                colors.surfaces.overlay
            } else {
                colors.text_primary
            };
            draw_centered(
                text,
                scene,
                transform,
                (&day.to_string(), center),
                &body,
                ink,
            );
        }
    }
}

/// One band control (collapse, pin, remove): the face every plain toggle
/// uses.
pub(super) fn paint_band_control(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    (icon, label): (ui::icons::Icon, &str),
    (toggled, hovered): (bool, bool),
) {
    let face = ui::widget::ButtonFace::icon_only(rect, icon, label)
        .role(ui::widget::ButtonRole::Plain)
        .toggled(toggled)
        .hovered(hovered);
    ui::widget::Button::paint(scene, transform, text, theme, &face);
}

/// The remove badge on a hovered glance: a small round X on the overlay
/// ground.
pub(super) fn paint_badge(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    badge: Rect,
    hovered: bool,
) {
    let colors = &theme.tokens.colors;
    let shadow = theme.tokens.elevation.floating.with_alpha_scaled(0.5);
    ui::surface::shadow(scene, transform, badge, BADGE / 2.0, &shadow);
    ui::surface::card(
        scene,
        transform,
        badge,
        BADGE / 2.0,
        if hovered {
            colors.text_primary
        } else {
            colors.surfaces.overlay
        },
    );
    ui::paint::stroke_round(
        scene,
        transform,
        badge,
        BADGE / 2.0,
        ui::chrome::panel_border(theme),
        1.0,
    );
    ui::icons::draw_icon(
        scene,
        transform,
        Rect::from_center_size(badge.center(), (10.0, 10.0)),
        ui::icons::Icon::X,
        if hovered {
            colors.surfaces.overlay
        } else {
            colors.text_primary
        },
        1.6,
    );
}

/// The system monitor's glance: load and memory, labels in quiet ink.
pub(super) fn paint_monitor_glance(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    reading: Reading,
) {
    let colors = &theme.tokens.colors;
    let label_style = theme
        .text(TextRole::Caption)
        .with_weight(TextWeight::Semibold);
    let value_style = theme.text(TextRole::Caption).with_weight(TextWeight::Bold);
    if reading.is_empty() {
        // The widget is up and the first reading is on its way: its name,
        // dim, until there is a number.
        let title = t("desk-monitor-title");
        let (_, h) = text.measure_style(&title, None, &value_style);
        text.draw_style(
            scene,
            &title,
            colors.text_dim,
            transform.pre_translate((rect.x0 + PAD, rect.center().y - f64::from(h) / 2.0).into()),
            None,
            &value_style,
        );
        return;
    }
    let pairs = [
        ("desk-monitor-cpu-short", reading.cpu_percent),
        ("desk-monitor-memory-short", reading.memory_percent),
    ];
    let mut x = rect.x0 + PAD;
    for (key, value) in pairs {
        let Some(value) = value else {
            continue;
        };
        let label = t(key);
        let value = format!("{value}%");
        for (run, style, color) in [
            (&label, &label_style, colors.text_dim),
            (&value, &value_style, colors.text_primary),
        ] {
            let (w, h) = text.measure_style(run, None, style);
            text.draw_style(
                scene,
                run,
                color,
                transform.pre_translate((x, rect.center().y - f64::from(h) / 2.0).into()),
                None,
                style,
            );
            x += f64::from(w) + 4.0;
        }
        x += 10.0;
    }
}

/// The system monitor's open card: compact, load and memory as two large
/// readouts; otherwise one row per reading the machine offers, each with a
/// quiet meter, roomier when large.
#[allow(clippy::too_many_lines, reason = "one card's readouts at each stage")]
pub(super) fn paint_monitor_card(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    card: Rect,
    reading: Reading,
    stage: Stage,
) {
    let colors = &theme.tokens.colors;
    draw_header(
        text,
        scene,
        transform,
        theme,
        card,
        "desk-monitor-title",
        MONITOR_HEADER,
    );
    if stage == Stage::Compact {
        let label_style = theme.text(TextRole::Caption);
        let value_style = theme
            .text(TextRole::Title)
            .with_weight(TextWeight::Semibold);
        let half = (card.width() - 2.0 * PAD) / 2.0;
        for (k, (key, value)) in [
            ("desk-monitor-cpu", reading.cpu_percent),
            ("desk-monitor-memory", reading.memory_percent),
        ]
        .into_iter()
        .enumerate()
        {
            let Some(value) = value else {
                continue;
            };
            let x = card.x0 + PAD + half * ui::num::f64_of_usize(k);
            let y = card.y0 + MONITOR_HEADER;
            let value = format!("{value}%");
            let (_, vh) = text.measure_style(&value, None, &value_style);
            text.draw_style(
                scene,
                &value,
                colors.text_primary,
                transform.pre_translate((x, y).into()),
                None,
                &value_style,
            );
            text.draw_style(
                scene,
                &t(key),
                colors.text_dim,
                transform.pre_translate((x, y + f64::from(vh)).into()),
                None,
                &label_style,
            );
        }
        return;
    }
    let rows = [
        (
            "desk-monitor-cpu",
            reading
                .cpu_percent
                .map(|v| (format!("{v}%"), f64::from(v) / 100.0)),
        ),
        (
            "desk-monitor-memory",
            reading
                .memory_percent
                .map(|v| (format!("{v}%"), f64::from(v) / 100.0)),
        ),
        (
            "desk-monitor-temperature",
            reading
                .temperature_celsius
                .map(|c| (format!("{c}\u{b0}"), (f64::from(c) / 100.0).clamp(0.0, 1.0))),
        ),
    ];
    let label_style = theme.text(TextRole::Body);
    let value_style = if stage == Stage::Large {
        theme
            .text(TextRole::Title)
            .with_weight(TextWeight::Semibold)
    } else {
        theme.text(TextRole::Body).with_weight(TextWeight::Semibold)
    };
    let meter = if stage == Stage::Large { 6.0 } else { 4.0 };
    let mut y = card.y0 + MONITOR_HEADER;
    for (key, value) in rows {
        let Some((value, share)) = value else {
            continue;
        };
        let label = t(key);
        let (_, h) = text.measure_style(&label, None, &label_style);
        let (vw, vh) = text.measure_style(&value, None, &value_style);
        let line_y = y + 4.0;
        text.draw_style(
            scene,
            &label,
            colors.text_primary,
            transform.pre_translate(
                (
                    card.x0 + PAD,
                    line_y + (f64::from(vh) - f64::from(h)).max(0.0) / 2.0,
                )
                    .into(),
            ),
            None,
            &label_style,
        );
        text.draw_style(
            scene,
            &value,
            colors.text_primary,
            transform.pre_translate((card.x1 - PAD - f64::from(vw), line_y).into()),
            None,
            &value_style,
        );
        // Secondary ink, never a status hue: a resting readout is not news.
        let track_y = line_y + f64::from(h.max(vh)) + 6.0;
        let track = Rect::new(card.x0 + PAD, track_y, card.x1 - PAD, track_y + meter);
        ui::surface::card(
            scene,
            transform,
            track,
            meter / 2.0,
            ui::paint::with_alpha_scaled(colors.text_dim, 0.25),
        );
        let fill = Rect::new(
            track.x0,
            track.y0,
            track.x0 + track.width() * share.clamp(0.0, 1.0),
            track.y1,
        );
        if fill.width() > 0.0 {
            ui::surface::card(scene, transform, fill, meter / 2.0, colors.text_dim);
        }
        y += monitor_row(stage);
    }
}

/// The resize handle: the corner grip in the card's free corner, pointing
/// the way a drag grows the card, brighter under the pointer.
pub(super) fn paint_grip(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    grip: Rect,
    (right, bottom): (bool, bool),
    hovered: bool,
) {
    let color = if hovered {
        theme.tokens.colors.text_primary
    } else {
        ui::paint::with_alpha_scaled(theme.tokens.colors.text_dim, 0.7)
    };
    // The handle stands on the side the card is free on (the display's
    // middle is behind it), so its arrow points out of the card: the way a
    // drag grows it.
    let icon = match (right, bottom) {
        (true, true) => ui::icons::Icon::ArrowUpLeft,
        (true, false) => ui::icons::Icon::ArrowDownLeft,
        (false, true) => ui::icons::Icon::ArrowUpRight,
        (false, false) => ui::icons::Icon::ArrowDownRight,
    };
    ui::icons::draw_icon(scene, transform, grip, icon, color, 1.4);
}

/// A widget's name, for the controls that open, fold, pin or remove it.
pub(super) fn desk_widget_title(id: WidgetId) -> String {
    t(match id {
        WidgetId::Music => "desk-widget-music",
        WidgetId::Team => "desk-widget-team",
        WidgetId::Calendar => "desk-widget-calendar",
        WidgetId::SystemMonitor => "desk-widget-system-monitor",
    })
}
