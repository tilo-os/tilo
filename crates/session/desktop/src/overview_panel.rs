//! The overview as a widget: a floating panel over the desk that names the
//! workspace you are in, lists its windows, and offers the other workspaces
//! and a new one, drawn from [`crate::overview::room_cards`].
//!
//! The panel decides nothing. A pick (a window, a workspace, a new
//! workspace, or leaving) is queued for the host with
//! [`OverviewPanel::take_pick`], and the host applies it through its own
//! navigation. A host that draws the overview itself reads the same cards.

use std::collections::HashMap;

use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use tilo_desktop_model::{TabId, WorkspaceId};
use ui::a11y::{AccessNode, AccessState, Role};
use ui::chrome::ChromeRole;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{AppModel, ModelPath, TaffyTreeExt as _, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::i18n::t;
use crate::overview::{RoomCard, room_access_label};
use crate::window_switcher::WindowApp;

/// What the person chose in the overview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverviewPick {
    /// Go to this window of the current workspace.
    Window(TabId),
    /// Go to another workspace.
    Workspace(WorkspaceId),
    /// Make a workspace and go there.
    NewWorkspace,
    /// Leave the overview where it was.
    Dismiss,
}

/// The panel's width at most, as the native shell's overview.
const PANEL_WIDTH: f64 = 720.0;
/// Room kept between the panel and the edges of what it covers.
const MARGIN: f64 = 24.0;
const PAD: f64 = 16.0;
const HERE_HEIGHT: f64 = 88.0;
const CAPTION_HEIGHT: f64 = 28.0;
const WINDOW_ROW: f64 = 40.0;
const ROOM_ROW: f64 = 62.0;
const NEW_ROW: f64 = 44.0;
const GAP: f64 = 8.0;
/// Windows listed before the rest are counted instead.
const MAX_WINDOWS: usize = 8;

/// One window the panel lists.
#[derive(Clone, Debug)]
struct WindowRow {
    tab: TabId,
    title: String,
    app: WindowApp,
    current: bool,
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
    windows: Vec<WindowRow>,
    more_windows: usize,
    rooms: Vec<RoomCard>,
    /// Rows as last laid out, in keyboard order.
    rows: Vec<(Target, Rect)>,
    panel: Rect,
    selected: Option<usize>,
    cursor: Point,
    pick: Option<OverviewPick>,
    /// The host may make workspaces.
    can_add: bool,
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
            windows: Vec::new(),
            more_windows: 0,
            rooms: Vec::new(),
            rows: Vec::new(),
            panel: Rect::ZERO,
            selected: None,
            cursor: Point::ZERO,
            pick: None,
            can_add: true,
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
        let mut windows: Vec<WindowRow> = self
            .here
            .iter()
            .flat_map(|card| &card.strips)
            .flat_map(|strip| {
                strip.groups.iter().filter_map(move |group| {
                    let tab = group
                        .tabs
                        .iter()
                        .find(|tab| tab.is_active)
                        .or_else(|| group.tabs.first())?;
                    Some(WindowRow {
                        tab: tab.id,
                        title: tab.title.clone(),
                        app: tab.app.clone(),
                        current: group.is_current && strip.is_current,
                    })
                })
            })
            .collect();
        self.more_windows = windows.len().saturating_sub(MAX_WINDOWS);
        windows.truncate(MAX_WINDOWS);
        self.windows = windows;
        self.dirty = true;
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
        let windows = if self.windows.is_empty() {
            WINDOW_ROW
        } else {
            WINDOW_ROW
                * small_f64(
                    self.windows
                        .len()
                        .saturating_add(usize::from(self.more_windows > 0)),
                )
        };
        let rooms = if self.rooms.is_empty() {
            0.0
        } else {
            CAPTION_HEIGHT + (ROOM_ROW + GAP) * small_f64(self.rooms.len())
        };
        let add = if self.can_add { NEW_ROW } else { 0.0 };
        GAP + HERE_HEIGHT + GAP + CAPTION_HEIGHT + windows + GAP + rooms + add + PAD
    }

    /// Place the panel and its rows in `area`.
    fn layout_rows(&mut self) {
        let area = self.rect;
        let width = (area.width() - 2.0 * MARGIN).clamp(0.0, PANEL_WIDTH);
        let height = self
            .content_height()
            .min(area.height() - 2.0 * MARGIN)
            .max(0.0);
        let x0 = area.center().x - width / 2.0;
        let y0 = area.y0
            + ((area.height() - height) / 2.0)
                .max(MARGIN)
                .min(area.height() / 4.0);
        self.panel = Rect::new(x0, y0, x0 + width, y0 + height);
        let inner_x0 = x0 + PAD;
        let inner_x1 = x0 + width - PAD;
        let mut rows = Vec::new();
        let mut y = y0 + GAP + HERE_HEIGHT + GAP + CAPTION_HEIGHT;
        for window in &self.windows {
            rows.push((
                Target::Window(window.tab),
                Rect::new(inner_x0, y, inner_x1, y + WINDOW_ROW - 4.0),
            ));
            y += WINDOW_ROW;
        }
        if self.windows.is_empty() || self.more_windows > 0 {
            y += WINDOW_ROW;
        }
        y += GAP;
        if !self.rooms.is_empty() {
            y += CAPTION_HEIGHT;
            for room in &self.rooms {
                rows.push((
                    Target::Workspace(room.id),
                    Rect::new(inner_x0, y, inner_x1, y + ROOM_ROW),
                ));
                y += ROOM_ROW + GAP;
            }
        }
        if self.can_add {
            rows.push((
                Target::NewWorkspace,
                Rect::new(inner_x0, y, inner_x1, y + NEW_ROW - 4.0),
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
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let scale = text.text_scale();
        let size = |s: f32| s / scale;
        ui::chrome::shell::fill_scrim(scene, transform, self.rect, 0.45, theme);
        let panel = self.panel;
        if panel.width() <= 0.0 || panel.height() <= 0.0 {
            return;
        }
        let radius = ChromeRole::Panel.radius(theme, panel.height());
        ui::chrome::shell::panel_shadow(scene, transform, panel, radius, theme, 1.0);
        let shape = RoundedRect::from_rect(panel, radius);
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::shell::panel_ground(theme),
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            ui::chrome::shell::panel_border(theme),
            None,
            &shape,
        );
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
            panel.x0 + GAP,
            panel.y0 + GAP,
            panel.x1 - GAP,
            panel.y0 + GAP + HERE_HEIGHT,
        );
        let here_tier = ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(true));
        let here_shape =
            RoundedRect::from_rect(here, ChromeRole::Object.radius(theme, here.height()));
        scene.fill(Fill::NonZero, transform, here_tier.fill, None, &here_shape);
        if let Some(border) = here_tier.border {
            scene.stroke(&Stroke::new(1.0), transform, border, None, &here_shape);
        }
        if let Some(card) = &self.here {
            let name_size = size(tokens.typography.scale.x2l);
            let apps = card.apps();
            let marks = small_f64(apps.len().min(6));
            let marks_w = if marks > 0.0 { marks * 24.0 } else { 0.0 };
            let chip_label = t("overview-panel-you-are-here");
            let chip_size = size(tokens.typography.scale.xs);
            let (chip_w, chip_h) =
                text.measure_weighted(&chip_label, chip_size, None, TextWeight::Medium);
            let name_room =
                (here.width() - 2.0 * PAD - marks_w - f64::from(chip_w) - 40.0).max(40.0);
            let name = text.elide_to_width_weighted(
                &card.name,
                name_size,
                name_room as f32,
                TextWeight::Semibold,
            );
            let (name_w, name_h) =
                text.measure_weighted(&name, name_size, None, TextWeight::Semibold);
            let name_x = here.x0 + PAD;
            let name_y = here.y0 + 12.0;
            text.draw_text_weighted(
                scene,
                &name,
                name_size,
                colors.text_primary,
                transform.pre_translate((name_x, name_y).into()),
                None,
                TextWeight::Semibold,
            );
            let chip = Rect::new(
                name_x + f64::from(name_w) + 12.0,
                name_y + f64::from(name_h) / 2.0 - f64::from(chip_h) / 2.0 - 4.0,
                name_x + f64::from(name_w) + 12.0 + f64::from(chip_w) + 20.0,
                name_y + f64::from(name_h) / 2.0 + f64::from(chip_h) / 2.0 + 4.0,
            );
            let chip_shape = RoundedRect::from_rect(chip, chip.height() / 2.0);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                colors.text_dim,
                None,
                &chip_shape,
            );
            text.draw_text_weighted(
                scene,
                &chip_label,
                chip_size,
                colors.text_primary,
                transform.pre_translate((chip.x0 + 10.0, chip.y0 + 4.0).into()),
                None,
                TextWeight::Medium,
            );
            let access = room_line(card);
            text.draw_text(
                scene,
                &access,
                size(tokens.typography.scale.base),
                colors.text_dim,
                transform.pre_translate((name_x, name_y + f64::from(name_h) + 6.0).into()),
                Some((here.width() - 2.0 * PAD) as f32),
            );
            let mut x = here.x1 - PAD - marks_w;
            for app in apps.into_iter().take(6) {
                ui::widget::draw_app_mark(
                    scene,
                    transform,
                    Rect::new(x + 4.0, here.y0 + 16.0, x + 20.0, here.y0 + 32.0),
                    app.icon.as_ref(),
                    app.kind,
                    colors.text_primary,
                );
                x += 24.0;
            }
        }

        // The windows of this workspace.
        let caption_size = size(tokens.typography.scale.sm);
        let mut y = here.y1 + GAP;
        text.draw_text(
            scene,
            &t("overview-panel-windows"),
            caption_size,
            colors.text_tertiary,
            transform.pre_translate((panel.x0 + PAD, y + 8.0).into()),
            None,
        );
        y += CAPTION_HEIGHT;
        let body_size = size(tokens.typography.scale.base);
        let rows = self.rows.clone();
        for (index, (target, rect)) in rows.iter().enumerate() {
            let Target::Window(tab) = target else {
                continue;
            };
            let Some(window) = self.windows.iter().find(|w| w.tab == *tab) else {
                continue;
            };
            let lifted = self.row_state(index) || window.current;
            Self::draw_row_ground(
                scene,
                transform,
                theme,
                *rect,
                lifted,
                self.selected == Some(index),
            );
            let mid = rect.center().y;
            ui::widget::draw_app_mark(
                scene,
                transform,
                Rect::new(rect.x0 + 10.0, mid - 8.0, rect.x0 + 26.0, mid + 8.0),
                window.app.icon.as_ref(),
                window.app.kind,
                colors.text_primary,
            );
            let title = text.elide_to_width(&window.title, body_size, (rect.width() - 48.0) as f32);
            let (_, h) = text.measure(&title, body_size, None);
            text.draw_text(
                scene,
                &title,
                body_size,
                if window.current {
                    colors.text_primary
                } else {
                    colors.text_dim
                },
                transform.pre_translate((rect.x0 + 36.0, mid - f64::from(h) / 2.0).into()),
                None,
            );
            y = rect.y1 + 4.0;
        }
        if self.windows.is_empty() || self.more_windows > 0 {
            let note = if self.windows.is_empty() {
                t("overview-panel-no-windows")
            } else {
                crate::i18n::t_args(
                    "overview-panel-more-windows",
                    &[("count", self.more_windows.into())],
                )
            };
            text.draw_text(
                scene,
                &note,
                body_size,
                colors.text_dim,
                transform.pre_translate((panel.x0 + PAD + 10.0, y + 10.0).into()),
                None,
            );
            y += WINDOW_ROW;
        }
        y += GAP;

        // Where else you can go.
        if !self.rooms.is_empty() {
            text.draw_text(
                scene,
                &t("overview-panel-go-to"),
                caption_size,
                colors.text_tertiary,
                transform.pre_translate((panel.x0 + PAD, y + 8.0).into()),
                None,
            );
        }
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
                    let name_size = size(tokens.typography.scale.lg);
                    let name = text.elide_to_width_weighted(
                        &room.name,
                        name_size,
                        (rect.width() - 2.0 * PAD) as f32,
                        TextWeight::Semibold,
                    );
                    text.draw_text_weighted(
                        scene,
                        &name,
                        name_size,
                        colors.text_primary,
                        transform.pre_translate((rect.x0 + PAD, rect.y0 + 10.0).into()),
                        None,
                        TextWeight::Semibold,
                    );
                    text.draw_text(
                        scene,
                        &room_line(room),
                        caption_size,
                        colors.text_dim,
                        transform.pre_translate((rect.x0 + PAD, rect.y0 + 34.0).into()),
                        None,
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
                    Icon::Plus.draw_with_stroke(
                        scene,
                        transform,
                        Rect::new(rect.x0 + 14.0, mid - 7.0, rect.x0 + 28.0, mid + 7.0),
                        colors.text_primary,
                        1.6,
                    );
                    let label = t("overview-panel-new-workspace");
                    let (_, h) = text.measure(&label, body_size, None);
                    text.draw_text(
                        scene,
                        &label,
                        body_size,
                        colors.text_primary,
                        transform.pre_translate((rect.x0 + 42.0, mid - f64::from(h) / 2.0).into()),
                        None,
                    );
                }
                Target::Window(_) => {}
            }
        }
        scene.pop_layer();
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
                    .or_else(|| self.windows.iter().position(|w| w.current))
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

fn small_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

impl Widget for OverviewPanel {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: taffy::Size {
                width: percent(1.0_f32),
                height: percent(1.0_f32),
            },
            ..Default::default()
        });
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
        if rect != self.rect {
            self.rect = rect;
            self.dirty = true;
        }
    }

    fn update(&mut self, _state: &mut AppModel, _path: &ModelPath) -> bool {
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
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

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
                .windows
                .iter()
                .find(|w| w.tab == *tab)
                .map(|w| w.title.clone())
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
                                .with_state(AccessState {
                                    focusable: true,
                                    focused: self.selected == Some(index),
                                    ..Default::default()
                                })
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
        assert!(panel.rows.windows(2).all(|w| w[0].1.y1 <= w[1].1.y0));
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
