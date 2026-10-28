//! The overview as a widget: a floating panel over the desk that names the
//! workspace you are in, pictures its strips and windows zoomed out (the
//! same picture the native shell's overview draws,
//! [`crate::overview_picture`]), and offers the other workspaces and a new
//! one, drawn from [`crate::overview::room_cards`].
//!
//! The panel decides nothing. A pick (a window, a workspace, a new
//! workspace, or leaving) is queued for the host with
//! [`OverviewPanel::take_pick`], and the host applies it through its own
//! navigation. A host that draws the overview itself reads the same cards.

use std::collections::HashMap;
use ui::num::f64_of_usize;

use taffy::{NodeId, Style, TaffyTree};
use tilo_desktop_model::{TabId, WorkspaceId};
use ui::TextRole;
use ui::a11y::{AccessNode, AccessState, Role};
use ui::chrome::ChromeRole;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{TaffyTreeExt as _, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Size, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::i18n::t;
use crate::overview::{RoomCard, room_access_label};
use crate::overview_picture::{
    GridMetrics, StripPlace, Tile, band_height, draw_empty_strip, draw_strip_name, draw_tile_title,
    layout_strips,
};
use crate::window_switcher::{
    NoThumbnails, PreviewSource, PreviewWell, SwitcherWindow, WindowAddress, draw_preview_well,
};

/// What the person chose in the overview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverviewPick {
    /// Go to this window of the current workspace.
    Window(TabId),
    /// Go to another workspace.
    Workspace(WorkspaceId),
    NewWorkspace,
    /// Leave the overview where it was.
    Dismiss,
}

/// The panel's width at most, as the native shell's overview.
const PANEL_WIDTH: f64 = 720.0;

/// The panel's density, resolved from the theme once per layout pass so
/// hit-testing (which has no theme in hand) reads the numbers paint uses.
/// Every height is a role's line plus spacing steps, so larger text grows
/// the panel instead of overlapping its lines.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    /// Room kept between the panel and the edges of what it covers (`s6`).
    margin: f64,
    /// Inner padding of the panel and of a row (`s4`).
    pad: f64,
    /// Gap between sections and between workspace rows (`s2`).
    gap: f64,
    /// Air between window rows (`s1`).
    row_gap: f64,
    /// The "you are here" block: its name line, `s1`, the access line and
    /// `s3` above and below.
    here_h: f64,
    /// A section caption band: `s2` above the text, `s1` below it.
    caption_h: f64,
    caption_dy: f64,
    /// One window row (or the "new workspace" row): a body line plus `s2`
    /// above and below, never below the pointer-target floor.
    window_h: f64,
    /// One workspace row: a heading line, `s1`, a caption line and `s3`
    /// above and below.
    room_h: f64,
    /// An app mark, and the step between the marks of the "here" block.
    mark: f64,
    mark_step: f64,
    /// Inset of a row's leading mark and the gap from it to the label
    /// (`s3`).
    inset: f64,
    /// The "You are here" chip's padding (`s3` across, `s1` down).
    chip_pad_x: f64,
    chip_pad_y: f64,
    /// The picture of the strips and windows, as the native overview's.
    grid: GridMetrics,
}

impl Metrics {
    fn for_theme(theme: &Theme) -> Self {
        let tokens = &theme.tokens;
        let spacing = &tokens.spacing;
        let lh = |role| f64::from(tokens.typography.style(role).line_height_px());
        let s1 = f64::from(spacing.s1);
        let s2 = f64::from(spacing.s2);
        let s3 = f64::from(spacing.s3);
        let mark = f64::from(tokens.metrics.icon_sm);
        Self {
            margin: f64::from(spacing.s6),
            pad: f64::from(spacing.s4),
            gap: s2,
            row_gap: s1,
            here_h: s3 + lh(TextRole::Title) + s1 + lh(TextRole::Body) + s3,
            caption_h: s2 + lh(TextRole::Subheading) + s1,
            caption_dy: s2,
            window_h: (lh(TextRole::Body) + 2.0 * s2)
                .max(f64::from(tokens.metrics.min_pointer_target)),
            room_h: s3 + lh(TextRole::Heading) + s1 + lh(TextRole::Caption) + s3,
            mark,
            mark_step: mark + s2,
            inset: s3,
            chip_pad_x: s3,
            chip_pad_y: s1,
            grid: GridMetrics::for_theme(theme),
        }
    }
}

impl Default for Metrics {
    /// The default theme's density, for a panel not yet laid out.
    fn default() -> Self {
        Self::for_theme(&Theme::default())
    }
}
/// One strip of the workspace you are in, as the picture shows it: every
/// tab of every window, left to right, each window's tabs together.
#[derive(Clone, Debug, Default)]
struct PictureStrip {
    name: String,
    current: bool,
    windows: Vec<SwitcherWindow>,
    /// Each one's share of the strip, in per-mille of its width.
    shares: Vec<u32>,
    /// Each one is a tab of the same window as the one before it.
    joined: Vec<bool>,
    /// Each one is the tab its window shows.
    front: Vec<bool>,
    current_window: Vec<bool>,
}

/// What a row of the panel leads to, in keyboard order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    Window(TabId),
    Workspace(WorkspaceId),
    NewWorkspace,
}

pub struct OverviewPanel {
    open: bool,
    rect: Rect,
    here: Option<RoomCard>,
    picture: Vec<PictureStrip>,
    /// Where each strip of the picture stands, as last laid out.
    places: Vec<StripPlace>,
    rooms: Vec<RoomCard>,
    /// Rows as last laid out, in keyboard order.
    rows: Vec<(Target, Rect)>,
    panel: Rect,
    selected: Option<usize>,
    cursor: Point,
    pick: Option<OverviewPick>,
    /// The host may make workspaces.
    can_add: bool,
    metrics: Metrics,
    dirty: bool,
}

impl Default for OverviewPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl OverviewPanel {
    pub fn new() -> Self {
        Self {
            open: false,
            rect: Rect::ZERO,
            here: None,
            picture: Vec::new(),
            places: Vec::new(),
            rooms: Vec::new(),
            rows: Vec::new(),
            panel: Rect::ZERO,
            selected: None,
            cursor: Point::ZERO,
            pick: None,
            can_add: true,
            metrics: Metrics::default(),
            dirty: true,
        }
    }

    /// Offer "New workspace" only when the host can make one.
    #[must_use]
    pub fn with_new_workspace(mut self, offered: bool) -> Self {
        self.can_add = offered;
        self
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open over `cards` (every workspace, as [`crate::overview::room_cards`]
    /// returns them), or close.
    /// Offer a new workspace or not, as the host's policy answers now.
    pub fn set_new_workspace(&mut self, offered: bool) {
        if self.can_add != offered {
            self.can_add = offered;
            self.dirty = true;
        }
    }

    pub fn set_open(&mut self, open: bool, cards: Vec<RoomCard>) {
        self.open = open;
        self.selected = None;
        self.dirty = true;
        if open {
            self.show(cards);
        }
    }

    /// Show `cards` while open, keeping the keyboard selection.
    pub fn show(&mut self, cards: Vec<RoomCard>) {
        let (here, rooms): (Vec<_>, Vec<_>) = cards.into_iter().partition(|c| c.is_current);
        self.here = here.into_iter().next();
        self.rooms = rooms;
        self.picture = self
            .here
            .iter()
            .flat_map(|card| &card.strips)
            .map(|strip| {
                let mut picture = PictureStrip {
                    name: strip.name.clone(),
                    current: strip.is_current,
                    ..PictureStrip::default()
                };
                for group in &strip.groups {
                    for (index, tab) in group.tabs.iter().enumerate() {
                        picture.windows.push(SwitcherWindow {
                            address: WindowAddress {
                                strip: strip.id,
                                group: group.id,
                                tab: tab.id,
                            },
                            title: tab.title.clone(),
                            app: tab.app.clone(),
                            preview: PreviewSource::Unavailable,
                        });
                        picture.shares.push(group.width_permille);
                        picture.joined.push(index > 0);
                        picture.front.push(tab.is_active || group.tabs.len() == 1);
                        picture
                            .current_window
                            .push(group.is_current && strip.is_current && tab.is_active);
                    }
                }
                picture
            })
            .collect();
        self.dirty = true;
    }

    /// Every pictured window in reading order, with whether it is the one
    /// you are in.
    fn pictured(&self) -> impl Iterator<Item = (&SwitcherWindow, bool)> {
        self.picture.iter().flat_map(|strip| {
            strip
                .windows
                .iter()
                .zip(strip.current_window.iter().copied())
        })
    }

    /// Height of the picture inside the panel, or of the line saying the
    /// workspace has no windows.
    fn picture_height(&self) -> f64 {
        let m = self.metrics;
        if self.picture.is_empty() {
            m.window_h + m.row_gap
        } else {
            band_height(self.picture.len(), &m.grid, m.grid.band) + m.row_gap
        }
    }

    /// Each pictured window's size on the desk the panel covers.
    fn picture_sizes(&self) -> (Vec<Vec<Size>>, Vec<Vec<bool>>) {
        let desk = self.rect.size();
        let width = desk.width.max(1.0);
        let height = desk.height.max(1.0);
        self.picture
            .iter()
            .map(|strip| {
                (
                    strip
                        .shares
                        .iter()
                        .map(|share| Size::new(width * f64::from(*share) / 1000.0, height))
                        .collect(),
                    strip.joined.clone(),
                )
            })
            .unzip()
    }

    /// What the person chose since the host last asked.
    pub fn take_pick(&mut self) -> Option<OverviewPick> {
        self.pick.take()
    }

    fn choose(&mut self, pick: OverviewPick) {
        self.pick = Some(pick);
        self.dirty = true;
    }

    fn target_pick(target: &Target) -> OverviewPick {
        match target {
            Target::Window(tab) => OverviewPick::Window(*tab),
            Target::Workspace(id) => OverviewPick::Workspace(*id),
            Target::NewWorkspace => OverviewPick::NewWorkspace,
        }
    }

    fn content_height(&self) -> f64 {
        let m = self.metrics;
        let window_step = m.window_h + m.row_gap;
        let windows = self.picture_height();
        let rooms = if self.rooms.is_empty() {
            0.0
        } else {
            m.caption_h + (m.room_h + m.gap) * f64_of_usize(self.rooms.len())
        };
        let add = if self.can_add {
            window_step + m.row_gap
        } else {
            0.0
        };
        m.gap + m.here_h + m.gap + m.caption_h + windows + m.gap + rooms + add + m.pad
    }

    /// Place the panel and its rows in `area`.
    fn layout_rows(&mut self) {
        let m = self.metrics;
        let area = self.rect;
        let width = (area.width() - 2.0 * m.margin).clamp(0.0, PANEL_WIDTH);
        let height = self
            .content_height()
            .min(area.height() - 2.0 * m.margin)
            .max(0.0);
        let x0 = area.center().x - width / 2.0;
        let y0 = area.y0
            + ((area.height() - height) / 2.0)
                .max(m.margin)
                .min(area.height() / 4.0);
        self.panel = Rect::new(x0, y0, x0 + width, y0 + height);
        let inner_x0 = x0 + m.pad;
        let inner_x1 = x0 + width - m.pad;
        let mut rows = Vec::new();
        let mut y = y0 + m.gap + m.here_h + m.gap + m.caption_h;
        // The picture: each strip under its name, its windows at their
        // shares of the desk, zoomed out to one band.
        let (sizes, joined) = self.picture_sizes();
        self.places = layout_strips(
            Rect::new(inner_x0, y, inner_x1, y),
            &sizes,
            &joined,
            &m.grid,
            Some(m.grid.band),
        );
        let tiles: Vec<(TabId, Rect)> = self
            .picture
            .iter()
            .zip(&self.places)
            .flat_map(|(strip, place)| {
                strip
                    .windows
                    .iter()
                    .zip(place.wells.iter().zip(&place.titles))
                    .map(|(window, (well, title))| (window.address.tab, well.union(*title)))
            })
            .collect();
        for (tab, tile) in tiles {
            rows.push((Target::Window(tab), tile));
        }
        y += self.picture_height();
        y += m.gap;
        if !self.rooms.is_empty() {
            y += m.caption_h;
            for room in &self.rooms {
                rows.push((
                    Target::Workspace(room.id),
                    Rect::new(inner_x0, y, inner_x1, y + m.room_h),
                ));
                y += m.room_h + m.gap;
            }
        }
        if self.can_add {
            rows.push((
                Target::NewWorkspace,
                Rect::new(inner_x0, y, inner_x1, y + m.window_h),
            ));
        }
        // Rows past the panel's bottom are not reachable by pointer.
        let bottom = self.panel.y1;
        rows.retain(|(_, rect)| rect.y1 <= bottom + 0.5);
        self.rows = rows;
    }

    fn hovered(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|(_, rect)| rect.contains(self.cursor))
    }

    fn row_state(&self, index: usize) -> bool {
        self.selected == Some(index) || self.hovered() == Some(index)
    }

    fn draw_row_ground(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
        lifted: bool,
        focused: bool,
    ) {
        let tier = ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(lifted));
        let shape = RoundedRect::from_rect(rect, ChromeRole::Control.radius(theme, rect.height()));
        if lifted {
            scene.fill(Fill::NonZero, transform, tier.fill, None, &shape);
        }
        if focused {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.border_focus,
                None,
                &shape,
            );
        } else if let Some(border) = tier.border.filter(|_| lifted) {
            scene.stroke(&Stroke::new(1.0), transform, border, None, &shape);
        }
    }

    #[allow(
        clippy::too_many_lines,
        clippy::cast_possible_truncation,
        reason = "one immediate-mode pass over the panel's bounded rows; text widths are f32"
    )]
    fn paint(&self, scene: &mut Scene, transform: Affine, text: &mut TextRenderer, theme: &Theme) {
        let m = self.metrics;
        ui::chrome::shell::fill_scrim(scene, transform, self.rect, 0.45, theme);
        let panel = self.panel;
        if panel.width() <= 0.0 || panel.height() <= 0.0 {
            return;
        }
        let painted = ui::chrome::shell::paint_panel(scene, transform, panel, theme, 1.0);
        let shape = painted.body;
        let theme = &painted.theme;
        let colors = &theme.tokens.colors;
        // Everything inside stays inside the panel's outline.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &shape,
        );

        // Here: the workspace you are in.
        let here = Rect::new(
            panel.x0 + m.gap,
            panel.y0 + m.gap,
            panel.x1 - m.gap,
            panel.y0 + m.gap + m.here_h,
        );
        let here_tier = ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(true));
        let here_shape =
            RoundedRect::from_rect(here, ChromeRole::Object.radius(theme, here.height()));
        scene.fill(Fill::NonZero, transform, here_tier.fill, None, &here_shape);
        if let Some(border) = here_tier.border {
            scene.stroke(&Stroke::new(1.0), transform, border, None, &here_shape);
        }
        if let Some(card) = &self.here {
            let name_style = theme.text(TextRole::Title);
            let chip_style = theme.text(TextRole::Caption);
            let body = theme.text(TextRole::Body);
            let apps = card.apps();
            let marks = f64_of_usize(apps.len().min(6));
            let marks_w = marks * m.mark_step;
            let chip_label = t("overview-panel-you-are-here");
            let (chip_w, chip_h) = text.measure_style(&chip_label, None, &chip_style);
            let chip_w = f64::from(chip_w) + 2.0 * m.chip_pad_x;
            let name_room =
                (here.width() - 2.0 * m.pad - marks_w - chip_w - m.inset - m.pad).max(m.pad);
            let name = text.elide_to_width_style(&card.name, name_room as f32, &name_style);
            let (name_w, name_h) = text.measure_style(&name, None, &name_style);
            let name_x = here.x0 + m.pad;
            let name_y = here.y0 + m.inset;
            text.draw_style(
                scene,
                &name,
                colors.text_primary,
                transform.pre_translate((name_x, name_y).into()),
                None,
                &name_style,
            );
            let name_mid = name_y + f64::from(name_h) / 2.0;
            let chip_half = f64::from(chip_h) / 2.0 + m.chip_pad_y;
            let chip_x0 = name_x + f64::from(name_w) + m.inset;
            let chip = Rect::new(
                chip_x0,
                name_mid - chip_half,
                chip_x0 + chip_w,
                name_mid + chip_half,
            );
            let chip_shape = RoundedRect::from_rect(chip, chip.height() / 2.0);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                colors.text_dim,
                None,
                &chip_shape,
            );
            text.draw_style(
                scene,
                &chip_label,
                colors.text_primary,
                transform.pre_translate((chip.x0 + m.chip_pad_x, chip.y0 + m.chip_pad_y).into()),
                None,
                &chip_style,
            );
            let access = text.elide_to_width_style(
                &room_line(card),
                (here.width() - 2.0 * m.pad) as f32,
                &body,
            );
            text.draw_style(
                scene,
                &access,
                colors.text_dim,
                transform.pre_translate((name_x, name_y + f64::from(name_h) + m.row_gap).into()),
                None,
                &body,
            );
            let mut x = here.x1 - m.pad - marks_w;
            let mark_y = name_mid - m.mark / 2.0;
            for app in apps.into_iter().take(6) {
                let x0 = x + (m.mark_step - m.mark) / 2.0;
                ui::widget::draw_app_mark(
                    scene,
                    transform,
                    Rect::new(x0, mark_y, x0 + m.mark, mark_y + m.mark),
                    app.icon.as_ref(),
                    app.kind,
                    colors.text_primary,
                );
                x += m.mark_step;
            }
        }

        // The windows of this workspace.
        let caption = theme.text(TextRole::Subheading);
        let mut y = here.y1 + m.gap;
        text.draw_style(
            scene,
            &t("overview-panel-windows"),
            colors.text_tertiary,
            transform.pre_translate((panel.x0 + m.pad, y + m.caption_dy).into()),
            None,
            &caption,
        );
        y += m.caption_h;
        let body = theme.text(TextRole::Body);
        self.paint_picture(scene, transform, text, theme);
        if self.picture.is_empty() {
            let note = t("overview-panel-no-windows");
            let (_, h) = text.measure_style(&note, None, &body);
            text.draw_style(
                scene,
                &note,
                colors.text_dim,
                transform.pre_translate(
                    (
                        panel.x0 + m.pad + m.inset,
                        y + (m.window_h - f64::from(h)) / 2.0,
                    )
                        .into(),
                ),
                None,
                &body,
            );
        }
        y += self.picture_height();
        y += m.gap;

        // Where else you can go.
        if !self.rooms.is_empty() {
            text.draw_style(
                scene,
                &t("overview-panel-go-to"),
                colors.text_tertiary,
                transform.pre_translate((panel.x0 + m.pad, y + m.caption_dy).into()),
                None,
                &caption,
            );
        }
        let room_name = theme.text(TextRole::Heading);
        let room_sub = theme.text(TextRole::Caption);
        let rows = self.rows.clone();
        for (index, (target, rect)) in rows.iter().enumerate() {
            match target {
                Target::Workspace(id) => {
                    let Some(room) = self.rooms.iter().find(|r| r.id == *id) else {
                        continue;
                    };
                    let idle = ui::surface::edge_on(ui::chrome::shell::panel_ground(theme));
                    let shape = RoundedRect::from_rect(
                        *rect,
                        ChromeRole::Control.radius(theme, rect.height()),
                    );
                    scene.stroke(&Stroke::new(1.0), transform, idle, None, &shape);
                    Self::draw_row_ground(
                        scene,
                        transform,
                        theme,
                        *rect,
                        self.row_state(index),
                        self.selected == Some(index),
                    );
                    let room_w = (rect.width() - 2.0 * m.pad) as f32;
                    let name = text.elide_to_width_style(&room.name, room_w, &room_name);
                    let (_, name_h) = text.measure_style(&name, None, &room_name);
                    let name_y = rect.y0 + m.inset;
                    text.draw_style(
                        scene,
                        &name,
                        colors.text_primary,
                        transform.pre_translate((rect.x0 + m.pad, name_y).into()),
                        None,
                        &room_name,
                    );
                    let line = text.elide_to_width_style(&room_line(room), room_w, &room_sub);
                    text.draw_style(
                        scene,
                        &line,
                        colors.text_dim,
                        transform.pre_translate(
                            (rect.x0 + m.pad, name_y + f64::from(name_h) + m.row_gap).into(),
                        ),
                        None,
                        &room_sub,
                    );
                }
                Target::NewWorkspace => {
                    Self::draw_row_ground(
                        scene,
                        transform,
                        theme,
                        *rect,
                        self.row_state(index),
                        self.selected == Some(index),
                    );
                    let mid = rect.center().y;
                    let mark_x = rect.x0 + m.inset;
                    Icon::Plus.draw(
                        scene,
                        transform,
                        Rect::new(
                            mark_x,
                            mid - m.mark / 2.0,
                            mark_x + m.mark,
                            mid + m.mark / 2.0,
                        ),
                        colors.text_primary,
                    );
                    let label = t("overview-panel-new-workspace");
                    let (_, h) = text.measure_style(&label, None, &body);
                    text.draw_style(
                        scene,
                        &label,
                        colors.text_primary,
                        transform.pre_translate(
                            (mark_x + m.mark + m.inset, mid - f64::from(h) / 2.0).into(),
                        ),
                        None,
                        &body,
                    );
                }
                Target::Window(_) => {}
            }
        }
        scene.pop_layer();
    }

    /// The picture of the workspace you are in: each strip's name, its
    /// windows' pictures (their app's mark: a browser holds no picture of
    /// a window) and titles, the window you are in and the one under the
    /// pointer or the keyboard lifted, as the native overview draws them.
    fn paint_picture(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let g = self.metrics.grid;
        let s1 = f64::from(theme.tokens.spacing.s1);
        let mut index: usize = 0;
        for (strip, place) in self.picture.iter().zip(&self.places) {
            draw_strip_name(
                &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
                place.label,
                place.label.x1,
                (&strip.name, strip.current),
            );
            if let Some(empty) = place.empty {
                draw_empty_strip(
                    &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
                    empty,
                    1.0,
                );
            }
            for (wi, window) in strip.windows.iter().enumerate() {
                let (Some(well), Some(title)) = (place.wells.get(wi), place.titles.get(wi)) else {
                    continue;
                };
                let row = index;
                index = index.saturating_add(1);
                let current = strip.current_window.get(wi).copied().unwrap_or(false);
                let selected = self.selected == Some(row);
                let state = ui::chrome::SelectionState::new(
                    selected || current,
                    self.hovered() == Some(row),
                    false,
                );
                let bounds = well.union(*title);
                if state.active || state.hover {
                    ui::chrome::selection_tier(theme, state).paint(
                        scene,
                        transform,
                        bounds.inflate(s1, s1),
                        g.radius + s1,
                    );
                }
                if selected {
                    ui::widget::paint_focus_ring(
                        scene,
                        transform,
                        bounds.inflate(s1, s1),
                        g.radius + s1,
                        theme,
                    );
                }
                draw_preview_well(
                    scene,
                    transform,
                    theme,
                    PreviewWell {
                        rect: *well,
                        radius: g.radius,
                        icon: f64::from(theme.tokens.metrics.icon_lg).min(well.height() * 0.6),
                    },
                    window,
                    &mut NoThumbnails,
                );
                draw_tile_title(
                    &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
                    &Tile {
                        window,
                        front: strip.front.get(wi).copied().unwrap_or(true),
                        well: *well,
                        title: *title,
                        // Who is where shows on the bar's faces here.
                        faces: &[],
                    },
                    *well,
                    (1.0, current || selected),
                );
            }
        }
    }

    fn key(&mut self, key: &Key) -> bool {
        let last = self.rows.len().checked_sub(1);
        match key {
            Key::Named(NamedKey::Escape) => self.choose(OverviewPick::Dismiss),
            Key::Named(NamedKey::ArrowDown | NamedKey::ArrowRight | NamedKey::Tab) => {
                let Some(last) = last else { return true };
                self.selected = Some(self.selected.map_or(0, |i| i.saturating_add(1).min(last)));
            }
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowLeft) => {
                let Some(last) = last else { return true };
                self.selected = Some(self.selected.map_or(last, |i| i.saturating_sub(1)));
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                let target = self
                    .selected
                    .or_else(|| self.pictured().position(|(_, current)| current))
                    .and_then(|i| self.rows.get(i))
                    .map(|(target, _)| Self::target_pick(target));
                self.choose(target.unwrap_or(OverviewPick::Dismiss));
            }
            _ => return false,
        }
        self.dirty = true;
        true
    }
}

/// Who may be in a room, or, where the host keeps no roster, what is open
/// in it: "No one has access" would be false of a room nobody listed.
fn room_line(card: &RoomCard) -> String {
    if card.people.is_empty() {
        match card.group_count() {
            0 => t("overview-panel-no-windows"),
            count => crate::i18n::t_args("overview-panel-window-count", &[("count", count.into())]),
        }
    } else {
        room_access_label(card)
    }
}

impl Widget for OverviewPanel {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let metrics = Metrics::for_theme(theme);
        if metrics != self.metrics {
            self.metrics = metrics;
            self.dirty = true;
        }
        let style = Style {
            size: taffy::Size::from_percent(1.0_f32, 1.0_f32),
            ..Default::default()
        };
        taffy.widget_leaf(style, map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = ui::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.rect = rect;
            self.dirty = true;
        }
    }

    /// The panel fills its node and lays out its rows itself, open or
    /// closed: there is never a size for the layout solver to measure.
    fn layout_needs_measure(&self) -> bool {
        false
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if !self.open {
            return;
        }
        self.layout_rows();
        self.paint(scene, transform, text, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if !self.open || *ctx.handled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let before = self.hovered();
                self.cursor = ctx.logical(position);
                // Others still see the pointer move, so their hover ends.
                self.dirty |= before != self.hovered();
            }
            // Pointer input over what the panel does not cover (the bar
            // above it) is the host's.
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. }
                if !self.rect.contains(self.cursor) => {}
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let pick = match self.hovered() {
                    Some(index) => self.rows.get(index).map(|(t, _)| Self::target_pick(t)),
                    None if !self.panel.contains(self.cursor) => Some(OverviewPick::Dismiss),
                    None => None,
                };
                if let Some(pick) = pick {
                    self.choose(pick);
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. } => *ctx.handled = true,
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && self.key(&event.logical_key) =>
            {
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    ui::redraw_flag!();

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.open && self.rows.iter().any(|(_, rect)| rect.contains(point)))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn accessibility(&self) -> Option<AccessNode> {
        if !self.open {
            return None;
        }
        let name = |target: &Target| match target {
            Target::Window(tab) => self
                .pictured()
                .find(|(w, _)| w.address.tab == *tab)
                .map(|(w, _)| w.title.clone())
                .unwrap_or_default(),
            Target::Workspace(id) => self
                .rooms
                .iter()
                .find(|r| r.id == *id)
                .map(|r| r.name.clone())
                .unwrap_or_default(),
            Target::NewWorkspace => t("overview-panel-new-workspace"),
        };
        Some(
            AccessNode::new(Role::Dialog)
                .with_name(t("overview-panel-title"))
                .with_bounds(self.panel)
                .with_children(
                    self.rows
                        .iter()
                        .enumerate()
                        .map(|(index, (target, rect))| {
                            AccessNode::new(Role::Button)
                                .with_name(name(target))
                                .with_bounds(*rect)
                                .with_state(
                                    AccessState::new()
                                        .focusable(true)
                                        .focused(self.selected == Some(index)),
                                )
                        })
                        .collect(),
                ),
        )
    }

    fn name(&self) -> &'static str {
        "OverviewPanel"
    }

    fn get_rect(&self) -> Rect {
        if self.open { self.rect } else { Rect::ZERO }
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "test fixtures build small ids"
)]
mod tests {
    use super::*;
    use crate::overview::{OverviewGroup, OverviewStrip, OverviewTab, RoomCard};
    use crate::window_switcher::WindowApp;
    use tilo_desktop_model::{GroupId, StripId};
    use ui::widget::AppKind;

    fn card(id: u64, name: &str, current: bool, windows: &[&str]) -> RoomCard {
        RoomCard {
            id: WorkspaceId(id),
            name: name.into(),
            is_current: current,
            people: Vec::new(),
            shares: Vec::new(),
            strips: vec![OverviewStrip {
                id: StripId(id * 10),
                name: "Main".into(),
                is_current: current,
                groups: windows
                    .iter()
                    .enumerate()
                    .map(|(i, title)| OverviewGroup {
                        id: GroupId::from(id * 100 + i as u64),
                        width_permille: 500,
                        is_current: i == 0,
                        tabs: vec![OverviewTab {
                            id: id * 1000 + i as u64,
                            title: (*title).into(),
                            is_active: true,
                            app: WindowApp {
                                key: format!("app:{title}"),
                                kind: AppKind::Editor,
                                icon: None,
                                glyph: ui::icons::Icon::AppWindow,
                            },
                            people: Vec::new(),
                        }],
                    })
                    .collect(),
            }],
        }
    }

    fn opened() -> OverviewPanel {
        let mut panel = OverviewPanel::new();
        panel.set_open(
            true,
            vec![
                card(1, "Acme", true, &["Notes", "Writer"]),
                card(2, "Research", false, &[]),
            ],
        );
        panel.rect = Rect::new(0.0, 48.0, 1440.0, 900.0);
        panel.layout_rows();
        panel
    }

    #[test]
    fn rows_list_the_windows_here_then_other_workspaces_then_a_new_one() {
        let panel = opened();
        let targets: Vec<_> = panel.rows.iter().map(|(t, _)| t.clone()).collect();
        assert_eq!(
            targets,
            vec![
                Target::Window(1000),
                Target::Window(1001),
                Target::Workspace(WorkspaceId(2)),
                Target::NewWorkspace
            ]
        );
        assert!(panel.panel.width() <= PANEL_WIDTH);
        assert!(
            panel
                .rows
                .iter()
                .all(|(_, r)| panel.panel.contains(r.origin()))
        );
        let (notes, writer) = (panel.rows[0].1, panel.rows[1].1);
        assert!(
            notes.x1 < writer.x0 && (notes.y0 - writer.y0).abs() < 1e-6,
            "a strip's windows stand side by side: {notes:?} {writer:?}"
        );
        assert!(panel.rows[1..].windows(2).all(|w| w[0].1.y1 <= w[1].1.y0));
    }

    /// The overview is a picture, not a list: each strip of the workspace
    /// under its name, top to bottom, its windows left to right at their
    /// shares of the desk, the tabs of one window together.
    #[test]
    fn the_overview_pictures_each_strip_under_its_name_with_its_windows() {
        let mut here = card(1, "Acme", true, &["Notes", "Writer"]);
        here.strips[0].groups[1].width_permille = 1000;
        let mut second = card(3, "Acme", false, &["Chat", "Weather", "Calendar"]).strips[0].clone();
        second.name = "Games".into();
        second.is_current = false;
        // Weather is a tab of Chat's window.
        let weather = second.groups.remove(1).tabs.remove(0);
        second.groups[0].tabs.push(OverviewTab {
            is_active: false,
            ..weather
        });
        here.strips.push(second);
        let mut panel = OverviewPanel::new();
        panel.set_open(true, vec![here]);
        panel.rect = Rect::new(0.0, 48.0, 1440.0, 900.0);
        panel.layout_rows();

        assert_eq!(panel.places.len(), 2, "both strips are pictured");
        assert_eq!(panel.picture[0].name, "Main");
        assert_eq!(panel.picture[1].name, "Games");
        let (main, games) = (&panel.places[0], &panel.places[1]);
        assert!(
            main.label.y1 <= main.wells[0].y0,
            "the name heads its strip"
        );
        assert!(
            main.bounds().y1 <= games.label.y0,
            "strips stack top to bottom"
        );
        let ratio = main.wells[1].width() / main.wells[0].width();
        assert!(
            (ratio - 2.0).abs() < 1e-6,
            "windows keep their shares: {ratio}"
        );
        assert_eq!(games.wells.len(), 3, "every tab of every window");
        let tab_gap = games.wells[1].x0 - games.wells[0].x1;
        let window_gap = games.wells[2].x0 - games.wells[1].x1;
        assert!(tab_gap < window_gap, "a window's tabs stand together");
        assert!(
            panel
                .places
                .iter()
                .all(|place| panel.panel.contains(place.bounds().origin())),
            "the picture is inside the panel"
        );
        // Picking a pictured window goes to it.
        let chat = panel.rows[2].1.center();
        panel.cursor = chat;
        assert_eq!(panel.hovered(), Some(2));
        assert_eq!(panel.rows[2].0, Target::Window(3000));
        // The one drawn shows the strips' names and every window.
        let mut scene = Scene::new();
        panel.paint(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
        );
        assert!(!scene.encoding().is_empty());
    }

    #[test]
    fn keys_walk_the_rows_and_enter_picks_one() {
        let mut panel = opened();
        assert!(panel.key(&Key::Named(NamedKey::ArrowDown)));
        assert!(panel.key(&Key::Named(NamedKey::ArrowDown)));
        assert!(panel.key(&Key::Named(NamedKey::ArrowDown)));
        assert!(panel.key(&Key::Named(NamedKey::Enter)));
        assert_eq!(
            panel.take_pick(),
            Some(OverviewPick::Workspace(WorkspaceId(2)))
        );
        assert!(panel.key(&Key::Named(NamedKey::Escape)));
        assert_eq!(panel.take_pick(), Some(OverviewPick::Dismiss));
        assert_eq!(panel.take_pick(), None);
    }

    #[test]
    fn enter_without_a_selection_returns_to_the_current_window() {
        let mut panel = opened();
        panel.key(&Key::Named(NamedKey::Enter));
        assert_eq!(panel.take_pick(), Some(OverviewPick::Window(1000)));
    }

    #[test]
    fn a_host_that_cannot_make_workspaces_offers_none() {
        let mut panel = OverviewPanel::new().with_new_workspace(false);
        panel.set_open(true, vec![card(1, "Acme", true, &[])]);
        panel.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
        panel.layout_rows();
        assert!(panel.rows.is_empty());
    }

    #[test]
    fn a_closed_panel_draws_and_takes_nothing() {
        let mut panel = OverviewPanel::new();
        assert!(panel.accessibility().is_none());
        assert_eq!(panel.get_rect(), Rect::ZERO);
        let mut scene = Scene::new();
        panel.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::default(),
        );
        assert!(scene.encoding().is_empty());
    }
}
