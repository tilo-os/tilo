//! First-use workspace launchpad.
//!
//! An empty collaborative desktop should explain the useful things people can
//! do together, with a direct route to open any available app. Every card
//! publishes a semantic shell action; the overlay host routes it through
//! the same app, invite, and Activity paths
//! used everywhere else.

use std::collections::HashMap;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use ui::a11y::{AccessNode, AccessState, Action, Role};
use ui::color_compat::ColorCompat;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, TaffyTreeExt, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

pub const EMPTY_WORKSPACE_OPEN_APP: &str = "shell.empty-workspace.open-app";

pub const EMPTY_WORKSPACE_CHAT: &str = "shell.empty-workspace.chat";
pub const EMPTY_WORKSPACE_NOTES: &str = "shell.empty-workspace.notes";
pub const EMPTY_WORKSPACE_TASKS: &str = "shell.empty-workspace.tasks";
pub const EMPTY_WORKSPACE_MEET: &str = "shell.empty-workspace.meet";
pub const EMPTY_WORKSPACE_INVITE: &str = "shell.empty-workspace.invite";
pub const EMPTY_WORKSPACE_ACTIVITY: &str = "shell.empty-workspace.activity";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LaunchpadAction {
    Chat,
    Notes,
    Tasks,
    Meet,
    Invite,
    Activity,
}

impl LaunchpadAction {
    pub(crate) const ALL: [Self; 6] = [
        Self::Chat,
        Self::Notes,
        Self::Tasks,
        Self::Meet,
        Self::Invite,
        Self::Activity,
    ];

    pub(crate) const fn path(self) -> &'static str {
        match self {
            Self::Chat => EMPTY_WORKSPACE_CHAT,
            Self::Notes => EMPTY_WORKSPACE_NOTES,
            Self::Tasks => EMPTY_WORKSPACE_TASKS,
            Self::Meet => EMPTY_WORKSPACE_MEET,
            Self::Invite => EMPTY_WORKSPACE_INVITE,
            Self::Activity => EMPTY_WORKSPACE_ACTIVITY,
        }
    }

    pub(crate) const fn app_target(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Chat | Self::Meet => Some(("so.tilo.chat", "Chat")),
            Self::Notes => Some(("so.tilo.notes", "Notes")),
            Self::Tasks => Some(("so.tilo.calendar", "Calendar")),
            Self::Invite | Self::Activity => None,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Chat => "Discuss",
            Self::Notes => "Notes",
            Self::Tasks => "Tasks",
            Self::Meet => "Plan a meeting",
            Self::Invite => "Invite people",
            Self::Activity => "Catch up",
        }
    }

    const fn detail(self) -> &'static str {
        match self {
            Self::Chat => "Open the shared conversation",
            Self::Notes => "Write ideas together",
            Self::Tasks => "Plan work in Calendar",
            Self::Meet => "Open Chat, then start a voice or video call",
            Self::Invite => "Bring someone into this workspace",
            Self::Activity => "See what changed and why",
        }
    }

    const fn icon(self) -> Icon {
        match self {
            Self::Chat => Icon::Mail,
            Self::Notes => Icon::FileText,
            Self::Tasks => Icon::Check,
            Self::Meet => Icon::Camera,
            Self::Invite => Icon::Users,
            Self::Activity => Icon::History,
        }
    }
}

pub(crate) struct CollaborationLaunchpad {
    rect: Rect,
    card_rects: [Rect; 6],
    last_cursor: Point,
    focused: bool,
    selected: usize,
    app_button_size: (f64, f64),
    /// Measured line height of the keyboard hint under the grid. The
    /// "Open an app" button is centred on the hint's line, so the two read
    /// as one footer row; measured during `draw`, defaulting to the
    /// toolkit's line metrics for the caption size before the first frame.
    hint_line_h: f64,
}

/// Top of the keyboard hint, from the bottom of the last card.
const HINT_TOP: f64 = 22.0;
/// Top of the footer band, from the bottom of the last card, and the band's
/// minimum height (the "Open an app" button's minimum height).
const FOOTER_TOP: f64 = 14.0;
const FOOTER_MIN_H: f64 = 36.0;
/// Height of the heading band (title + subtitle) above the first card row.
const HEADING_BAND: f64 = 90.0;
const KEYBOARD_HINT: &str = "Arrow keys move  ·  Enter opens  ·  Super+N shows every app";

impl CollaborationLaunchpad {
    pub(crate) fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            card_rects: [Rect::ZERO; 6],
            last_cursor: Point::ZERO,
            focused: false,
            selected: 0,
            app_button_size: (160.0, 36.0),
            hint_line_h: f64::from(
                ui::text_render::TextStyle::new(ui::tokens::TypeScale::default().xs)
                    .line_height_px(),
            ),
        }
    }

    fn fire(action: LaunchpadAction, state: &mut AutoCommit, path: &ObjId) {
        let _ = state.put(path, action.path(), true);
    }

    /// The footer reservation under the grid: `FOOTER_TOP` below the last
    /// card, at least the button's minimum height tall. Card overflow is
    /// measured against this band, so the grid does not move when the
    /// button is repositioned inside it.
    fn footer_band(&self) -> Rect {
        let last = self.card_rects[5];
        Rect::new(
            last.x0,
            last.y1 + FOOTER_TOP,
            last.x1,
            last.y1 + FOOTER_TOP + FOOTER_MIN_H,
        )
    }

    /// What the launchpad presents — heading band, cards and footer — as
    /// opposed to its layout rect, which spans the whole canvas under the
    /// floating workspace rail. The access tree reports this, so a geometry
    /// probe does not read the rail's controls as overlapping the launchpad.
    /// The layout rect until the first frame has placed the cards.
    fn content_bounds(&self) -> Rect {
        let Some(first) = self.card_rects.first().copied() else {
            return self.rect;
        };
        if first.area() <= 0.0 {
            return self.rect;
        }
        let last = self.card_rects.last().copied().unwrap_or(first);
        let heading = Rect::new(first.x0, first.y0 - HEADING_BAND, last.x1, first.y0);
        self.card_rects
            .iter()
            .fold(heading, |acc, card| acc.union(*card))
            .union(self.app_button_rect())
    }

    fn app_button_rect(&self) -> Rect {
        let last = self.card_rects[5];
        // Centred on the keyboard hint's line: it used to start 14 px below
        // the grid while the hint started 22 px below it, leaving their
        // centres ~4 px apart (HIG §7: controls in one row share a line).
        let centre = last.y1 + HINT_TOP + self.hint_line_h / 2.0;
        let top = centre - self.app_button_size.1 / 2.0;
        Rect::new(
            last.x1 - self.app_button_size.0,
            top,
            last.x1,
            top + self.app_button_size.1,
        )
    }

    fn hovered(&self) -> Option<usize> {
        self.card_rects
            .iter()
            .position(|rect| rect.contains(self.last_cursor))
    }

    #[allow(
        clippy::arithmetic_side_effects,
        clippy::cast_precision_loss,
        reason = "bounded six-card renderer geometry uses f64 logical pixels"
    )]
    fn card_geometry(rect: Rect) -> ([Rect; 6], usize) {
        // The workspace rail floats over the launchpad rather than shrinking
        // its widget's layout rect. On desktop-sized outputs, centre the
        // launchpad in the remaining canvas so its first heading/card never
        // starts under that rail. Narrow layouts keep the full canvas: the
        // rail is then already collapsed by the surrounding chrome.
        let rail_clearance =
            crate::shell::overlay_host::workspace_rail_clearance_for_viewport(rect.height());
        let content_x0 = if rect.width() >= 720.0 {
            rect.x0 + rail_clearance
        } else {
            rect.x0
        };
        let content_width = rect.x1 - content_x0;
        let panel_w = (content_width - 48.0).clamp(280.0, 900.0);
        let panel_x = content_x0 + (content_width - panel_w) / 2.0;
        let columns: usize = if panel_w >= 760.0 {
            3
        } else if panel_w >= 440.0 {
            2
        } else {
            1
        };
        let gap = 12.0;
        let card_w = (panel_w - gap * (columns.saturating_sub(1) as f64)) / columns as f64;
        let rows = 6_usize.div_ceil(columns);
        let available_h = (rect.height() - 168.0 - gap * (rows.saturating_sub(1) as f64)).max(0.0);
        let card_h = (available_h / rows as f64).clamp(64.0, 96.0);
        let grid_h = (rows as f64).mul_add(card_h, (rows.saturating_sub(1) as f64) * gap);
        let top = rect.center().y - grid_h / 2.0 + 32.0;
        let mut cards = [Rect::ZERO; 6];
        for (index, card) in cards.iter_mut().enumerate() {
            let column = index % columns;
            let row = index / columns;
            let x = panel_x + column as f64 * (card_w + gap);
            let y = top + row as f64 * (card_h + gap);
            *card = Rect::new(x, y, x + card_w, y + card_h);
        }
        (cards, columns)
    }
}

impl Default for CollaborationLaunchpad {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for CollaborationLaunchpad {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            flex_grow: 1.0,
            size: taffy::Size {
                width: percent(1.0_f32),
                height: percent(1.0_f32),
            },
            min_size: taffy::Size {
                width: percent(0.0_f32),
                height: percent(0.0_f32),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        _known: taffy::Size<Option<f32>>,
        _available: taffy::Size<taffy::AvailableSpace>,
        _text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        taffy::Size {
            width: 680.0,
            height: 420.0,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
        (self.card_rects, _) = Self::card_geometry(self.rect);
    }

    #[allow(
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::too_many_lines,
        reason = "immediate-mode renderer composes bounded logical-pixel geometry; text width API uses f32"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Theme tokens already include accessibility scaling; TextRenderer
        // also applies its configured scale to authored font sizes.
        let scale = text.text_scale();
        let base = theme.tokens.typography.scale.base / scale;
        let small = theme.tokens.typography.scale.sm / scale;
        let caption = theme.tokens.typography.scale.xs / scale;
        let heading = theme.tokens.typography.scale.x3l / scale;
        let (button_w, button_h) = text.measure("Open an app", base, None);
        self.app_button_size = (
            (f64::from(button_w) + 32.0).max(160.0),
            (f64::from(button_h) + 16.0).max(FOOTER_MIN_H),
        );
        let (_, hint_h) = text.measure(KEYBOARD_HINT, caption, None);
        self.hint_line_h = f64::from(hint_h);
        self.card_rects = Self::card_geometry(self.rect).0;
        // Leave the persistent help affordance its bottom band on laptops.
        // The grid is pushed up by the footer *band* (a fixed reservation),
        // not by where the button happens to sit inside it, so centring the
        // button on the hint's line cannot move the hint.
        let footer_bottom = self.footer_band().y1.max(self.app_button_rect().y1);
        let overflow = (footer_bottom - (self.rect.y1 - 64.0)).max(0.0);
        for card in &mut self.card_rects {
            *card = *card + vello::kurbo::Vec2::new(0.0, -overflow);
        }
        let cards = self.card_rects;
        let hovered = self.hovered();
        let content_left = cards.first().map_or(self.rect.x0, |card| card.x0);
        let heading_y = cards.first().map_or(self.rect.y0, |card| card.y0) - HEADING_BAND;
        let panel_width = cards[5].x1 - content_left;
        let title = text.elide_to_width("Make something together", heading, panel_width as f32);
        text.draw_text(
            scene,
            &title,
            heading,
            theme.text_primary,
            transform * Affine::translate((content_left, heading_y)),
            None,
        );
        let (_, title_h) = text.measure(&title, heading, None);
        let subtitle = text.elide_to_width(
            "Choose how to begin. Workspace content syncs; your layout and controls stay local.",
            small,
            panel_width as f32,
        );
        let (_, subtitle_h) = text.measure(&subtitle, small, None);
        if f64::from(title_h + subtitle_h) + 9.0 <= HEADING_BAND {
            text.draw_text(
                scene,
                &subtitle,
                small,
                theme.text_dim,
                transform * Affine::translate((content_left, heading_y + f64::from(title_h) + 3.0)),
                None,
            );
        }

        // Control role (HIG §9.1/§9.3): an action tile takes `radii.lg`. The
        // hovered or keyboard-selected tile is a lift with the tier's
        // hairline — never an accent wash, which is reserved for live state
        // (§9.4) — and an idle tile rests on its translucent floor with an
        // edge derived from that floor (§9.5), so the six cards read as cards
        // on the dark grid as well as the light one.
        let floor = theme.bg_primary.with_alpha_factor(0.78);
        let idle_edge = ui::surface::edge_on(theme.bg_primary);
        for (index, (action, rect)) in LaunchpadAction::ALL.into_iter().zip(cards).enumerate() {
            let active = hovered == Some(index) || (self.focused && self.selected == index);
            let tier =
                ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(active));
            let surface = if active { tier.fill } else { floor };
            let shape = RoundedRect::from_rect(
                rect,
                ui::chrome::ChromeRole::Control.radius(theme, rect.height()),
            );
            scene.fill(Fill::NonZero, transform, surface, None, &shape);
            let focused = self.focused && self.selected == index;
            scene.stroke(
                &Stroke::new(if focused { 2.0 } else { 1.0 }),
                transform,
                if focused {
                    theme.border_focus
                } else {
                    tier.border.unwrap_or(idle_edge)
                },
                None,
                &shape,
            );
            let icon_rect = Rect::new(
                rect.x0 + 18.0,
                rect.y0 + 18.0,
                rect.x0 + 42.0,
                rect.y0 + 42.0,
            );
            action
                .icon()
                .draw_with_stroke(scene, transform, icon_rect, theme.accent_primary, 1.6);
            let label = text.elide_to_width(action.label(), base, (rect.width() - 72.0) as f32);
            text.draw_text(
                scene,
                &label,
                base,
                theme.text_primary,
                transform * Affine::translate((rect.x0 + 56.0, rect.y0 + 16.0)),
                None,
            );
            let (_, label_h) = text.measure(&label, base, None);
            let (_, detail_h) = text.measure(action.detail(), caption, None);
            let detail_y = (16.0 + f64::from(label_h) + 6.0).max(47.0);
            if detail_y + f64::from(detail_h) + 6.0 <= rect.height() {
                let detail =
                    text.elide_to_width(action.detail(), caption, (rect.width() - 72.0) as f32);
                text.draw_text(
                    scene,
                    &detail,
                    caption,
                    theme.text_dim,
                    transform * Affine::translate((rect.x0 + 56.0, rect.y0 + detail_y)),
                    Some((rect.width() - 72.0) as f32),
                );
            }
        }

        let button = self.app_button_rect();
        // The shared `Button` face (HIG §9.9) at the Control radius (§9.1):
        // fill, border, hover, focus ring and measured label centring come
        // from the toolkit rather than a local copy of them. `base` is
        // renderer-relative here (divided above), which is what the face
        // expects from a widget painted at the tree's scale.
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(button, "Open an app")
                .radius(ui::chrome::ChromeRole::Control.radius(theme, button.height()))
                .text_size(base)
                .focused(self.focused && self.selected == 6)
                .hovered(button.contains(self.last_cursor)),
        );
        if let Some(last) = cards.last() {
            let hint_width = (button.x0 - content_left - 12.0).max(0.0) as f32;
            let hint = text.elide_to_width(KEYBOARD_HINT, caption, hint_width);
            // The button is centred on this line (`app_button_rect`), so the
            // hint stays where it was and the pair share one centre.
            text.draw_text(
                scene,
                &hint,
                caption,
                theme.text_primary,
                transform * Affine::translate((content_left, last.y1 + HINT_TOP)),
                None,
            );
        }
    }

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "pointer scaling and bounded six-card grid navigation use checked/saturating indices"
    )]
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        if *handled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.app_button_rect().contains(self.last_cursor) {
                    self.selected = 6;
                    let _ = state.put(path, EMPTY_WORKSPACE_OPEN_APP, true);
                    *handled = true;
                } else if let Some(index) = self.hovered() {
                    self.selected = index;
                    if let Some(action) = LaunchpadAction::ALL.get(index).copied() {
                        Self::fire(action, state, path);
                        *handled = true;
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.focused && event.state == ElementState::Pressed =>
            {
                let (_, columns) = Self::card_geometry(self.rect);
                let last_index = LaunchpadAction::ALL.len();
                let next = match event.logical_key {
                    Key::Named(NamedKey::ArrowRight) => {
                        Some(self.selected.saturating_add(1).min(last_index))
                    }
                    Key::Named(NamedKey::ArrowLeft) => Some(self.selected.saturating_sub(1)),
                    Key::Named(NamedKey::ArrowDown) => {
                        Some(self.selected.saturating_add(columns).min(last_index))
                    }
                    Key::Named(NamedKey::ArrowUp) => Some(self.selected.saturating_sub(columns)),
                    Key::Named(NamedKey::Tab) => {
                        if modifiers.state().shift_key() {
                            Some(self.selected.checked_sub(1).unwrap_or(last_index))
                        } else if self.selected >= last_index {
                            Some(0)
                        } else {
                            Some(self.selected.saturating_add(1))
                        }
                    }
                    Key::Named(NamedKey::Enter | NamedKey::Space) => {
                        if self.selected == 6 {
                            let _ = state.put(path, EMPTY_WORKSPACE_OPEN_APP, true);
                            *handled = true;
                        } else if let Some(action) =
                            LaunchpadAction::ALL.get(self.selected).copied()
                        {
                            Self::fire(action, state, path);
                            *handled = true;
                        }
                        None
                    }
                    _ => None,
                };
                if let Some(next) = next {
                    self.selected = next;
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.card_rects.iter().any(|rect| rect.contains(point))
            || self.app_button_rect().contains(point))
        .then_some(winit::window::CursorIcon::Pointer)
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn name(&self) -> &'static str {
        "CollaborationLaunchpad"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(
            AccessNode::new(Role::Group)
                .with_name("Make something together")
                .with_bounds(self.content_bounds())
                .with_description(
                    "Choose a shared conversation, notes, tasks, meeting, invitation, or workspace activity.",
                )
                .with_children(
                    LaunchpadAction::ALL
                        .into_iter()
                        .enumerate()
                        .zip(self.card_rects)
                        .map(|((index, action), bounds)| {
                            AccessNode::new(Role::Button)
                                .with_name(action.label())
                                .with_bounds(bounds)
                                .with_description(action.detail())
                                .with_action_target(action.path())
                                .with_state(AccessState {
                                    focusable: true,
                                    focused: self.focused && self.selected == index,
                                    ..Default::default()
                                })
                        })
                        .chain(std::iter::once(AccessNode::new(Role::Button)
                            .with_name("Open an app")
                            .with_action_target(EMPTY_WORKSPACE_OPEN_APP)
                            .with_bounds(self.app_button_rect())
                            .with_state(AccessState { focusable: true, focused: self.focused && self.selected == 6, ..Default::default() })))
                        .collect(),
                ),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else { return None };
        if *index == 6 {
            Some(self.app_button_rect())
        } else {
            self.card_rects.get(*index).copied()
        }
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [index] = path else { return false };
        if *index > LaunchpadAction::ALL.len() {
            return false;
        }
        self.focused = true;
        self.selected = *index;
        true
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &Action,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        if target == EMPTY_WORKSPACE_OPEN_APP && matches!(action, Action::Activate) {
            let _ = state.put(path, EMPTY_WORKSPACE_OPEN_APP, true);
            return true;
        }
        let Some(item) = LaunchpadAction::ALL
            .into_iter()
            .find(|item| item.path() == target)
        else {
            return false;
        };
        if !matches!(action, Action::Activate) {
            return false;
        }
        Self::fire(item, state, path);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ReadDoc as _;

    /// HIG §9.5: an idle card's edge derives from its own floor and must
    /// register on every shipped palette — the former `border_standard` at
    /// 0.72 alpha was invisible on the dark grid.
    #[test]
    fn launchpad_cards_keep_an_edge_on_every_palette() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let floor = theme.bg_primary;
            let delta = ui::surface::edge_delta(ui::surface::edge_on(floor), floor);
            assert!(
                delta >= ui::tokens::colors::HAIRLINE_MIN_DELTA,
                "{}: the card edge must separate it from its floor (Δ luminance {delta:.4})",
                theme.name
            );
        }
    }

    #[test]
    fn desktop_launchpad_exposes_an_app_opener() {
        let pad = CollaborationLaunchpad::new();
        assert!(
            pad.accessibility()
                .unwrap()
                .children
                .iter()
                .any(|node| node.name.as_deref() == Some("Open an app"))
        );
    }

    #[test]
    fn compact_large_text_heading_stays_below_top_chrome() {
        let mut pad = CollaborationLaunchpad::new();
        pad.rect = Rect::new(0.0, 48.0, 800.0, 700.0);
        let theme = Theme::dark().with_text_scale(2.0);
        let mut text = TextRenderer::new();
        text.set_text_scale(2.0);
        pad.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::default(),
        );
        assert!(
            pad.card_rects[0].y0 - 90.0 >= pad.rect.y0 + 12.0,
            "heading must clear chrome: {:?}",
            pad.card_rects[0]
        );
    }

    #[test]
    fn desktop_app_button_has_visible_bounds_in_compact_layouts() {
        for (width, height) in [(1280.0, 752.0), (960.0, 492.0), (460.0, 712.0)] {
            let mut pad = CollaborationLaunchpad::new();
            pad.rect = Rect::new(0.0, 48.0, width, height + 48.0);
            for scale in [1.0, 1.5, 2.0] {
                let theme = Theme::dark().with_text_scale(scale);
                let mut text = TextRenderer::new();
                text.set_text_scale(scale);
                pad.draw(
                    &mut Scene::new(),
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    &PresenceCtx::default(),
                );
                let button = pad.app_button_rect();
                let (label_w, label_h) = text.measure(
                    "Open an app",
                    theme.tokens.typography.scale.base / scale,
                    None,
                );
                assert!(
                    f64::from(label_w) + 32.0 <= button.width()
                        && f64::from(label_h) + 16.0 <= button.height()
                );
                assert!(button.width() >= 24.0 && button.height() >= 24.0);
                assert!(
                    pad.rect.contains(button.origin())
                        && button.x1 <= pad.rect.x1
                        && button.y1 <= pad.rect.y1,
                    "{button:?} outside {:?}",
                    pad.rect
                );
            }
        }
    }

    #[test]
    fn every_card_has_a_unique_semantic_route() {
        let routes = LaunchpadAction::ALL.map(LaunchpadAction::path);
        let unique = routes.into_iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), LaunchpadAction::ALL.len());
        assert!(
            routes
                .iter()
                .all(|route| route.starts_with("shell.empty-workspace."))
        );
    }

    #[test]
    fn collaboration_cards_target_the_apps_that_own_those_features() {
        assert_eq!(
            LaunchpadAction::Chat.app_target(),
            Some(("so.tilo.chat", "Chat"))
        );
        assert_eq!(
            LaunchpadAction::Meet.app_target(),
            Some(("so.tilo.chat", "Chat")),
            "calls are a Chat feature, not a fabricated Meet package"
        );
        assert_eq!(
            LaunchpadAction::Tasks.app_target(),
            Some(("so.tilo.calendar", "Calendar")),
            "Calendar is the repo's existing task manager"
        );
    }

    #[test]
    fn assistive_activation_publishes_the_same_action_as_a_card_click() {
        let mut launchpad = CollaborationLaunchpad::new();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        assert!(launchpad.perform_semantic_action(
            EMPTY_WORKSPACE_ACTIVITY,
            &Action::Activate,
            &mut doc,
            &root,
        ));
        assert!(matches!(
            doc.get(&root, EMPTY_WORKSPACE_ACTIVITY),
            Ok(Some((automerge::Value::Scalar(value), _)))
                if matches!(value.as_ref(), automerge::ScalarValue::Boolean(true))
        ));
    }

    #[test]
    fn wide_and_narrow_layouts_keep_all_cards_actionable() {
        for rect in [
            Rect::new(0.0, 0.0, 1200.0, 720.0),
            Rect::new(0.0, 0.0, 460.0, 760.0),
        ] {
            let (cards, _) = CollaborationLaunchpad::card_geometry(rect);
            assert!(
                cards
                    .iter()
                    .all(|card| card.width() >= 280.0 && card.height() >= 64.0)
            );
            assert!(cards.windows(2).all(|pair| pair[0] != pair[1]));
        }
    }

    #[test]
    fn desktop_launchpad_clears_the_workspace_rail() {
        let rect = Rect::new(0.0, 0.0, 960.0, 540.0);
        let (cards, _) = CollaborationLaunchpad::card_geometry(rect);
        let rail_clearance =
            crate::shell::overlay_host::workspace_rail_clearance_for_viewport(rect.height());
        assert!(
            cards.iter().all(|card| card.x0 >= rail_clearance),
            "desktop launchpad cards must not start beneath the persistent workspace rail: {cards:?}"
        );
    }
}
