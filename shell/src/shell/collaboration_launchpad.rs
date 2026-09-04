//! First-use workspace launchpad.
//!
//! An empty collaborative desktop should explain the useful things people can
//! do together, rather than presenting the implementation detail that they can
//! "open an app". Every card below publishes a semantic shell action; the
//! overlay host then routes it through the same app, invite, and Activity paths
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
use ui::widget::{TaffyTreeExt, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

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
            Self::Chat | Self::Meet => Some(("dev.qos.chat", "Chat")),
            Self::Notes => Some(("dev.qos.notes", "Notes")),
            Self::Tasks => Some(("dev.qos.calendar", "Calendar")),
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
}

impl CollaborationLaunchpad {
    pub(crate) fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            card_rects: [Rect::ZERO; 6],
            last_cursor: Point::ZERO,
            focused: false,
            selected: 0,
        }
    }

    fn fire(action: LaunchpadAction, state: &mut AutoCommit, path: &ObjId) {
        let _ = state.put(path, action.path(), true);
    }

    fn hovered(&self) -> Option<usize> {
        self.card_rects
            .iter()
            .position(|rect| rect.contains(self.last_cursor))
    }

    fn card_geometry(rect: Rect) -> ([Rect; 6], usize) {
        let panel_w = (rect.width() - 48.0).clamp(280.0, 900.0);
        let panel_x = rect.center().x - panel_w / 2.0;
        let columns: usize = if panel_w >= 760.0 {
            3
        } else if panel_w >= 520.0 {
            2
        } else {
            1
        };
        let gap = 12.0;
        let card_w = (panel_w - gap * (columns.saturating_sub(1) as f64)) / columns as f64;
        let rows = 6_usize.div_ceil(columns);
        let available_h = (rect.height() - 168.0 - gap * (rows.saturating_sub(1) as f64)).max(0.0);
        let card_h = (available_h / rows as f64).clamp(64.0, 96.0);
        let top = rect.center().y - (rows as f64 * card_h + (rows - 1) as f64 * gap) / 2.0 + 32.0;
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

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let (cards, _) = Self::card_geometry(self.rect);
        self.card_rects = cards;
        let hovered = self.hovered();
        let content_left = cards.first().map_or(self.rect.x0, |card| card.x0);
        let heading_y = cards.first().map_or(self.rect.y0, |card| card.y0) - 90.0;
        text.draw_text(
            scene,
            "Make something together",
            theme.tokens.typography.scale.x3l,
            theme.text_primary,
            transform * Affine::translate((content_left, heading_y)),
            None,
        );
        text.draw_text(
            scene,
            "Choose how to begin. Workspace content syncs; your layout and controls stay local.",
            theme.tokens.typography.scale.sm,
            theme.text_dim,
            transform * Affine::translate((content_left, heading_y + 43.0)),
            None,
        );

        for (index, action) in LaunchpadAction::ALL.into_iter().enumerate() {
            let rect = cards[index];
            let active = hovered == Some(index) || (self.focused && self.selected == index);
            let surface = if active {
                theme.accent_primary.with_alpha_factor(0.13)
            } else {
                theme.bg_primary.with_alpha_factor(0.78)
            };
            let shape = RoundedRect::from_rect(rect, 16.0);
            scene.fill(Fill::NonZero, transform, surface, None, &shape);
            scene.stroke(
                &Stroke::new(if self.focused && self.selected == index {
                    2.0
                } else {
                    1.0
                }),
                transform,
                if self.focused && self.selected == index {
                    theme.border_focus
                } else if active {
                    theme.accent_primary.with_alpha_factor(0.55)
                } else {
                    theme.border_standard.with_alpha_factor(0.72)
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
            text.draw_text(
                scene,
                action.label(),
                theme.tokens.typography.scale.base,
                theme.text_primary,
                transform * Affine::translate((rect.x0 + 56.0, rect.y0 + 16.0)),
                None,
            );
            if rect.height() >= 78.0 {
                text.draw_text(
                    scene,
                    action.detail(),
                    theme.tokens.typography.scale.xs,
                    theme.text_dim,
                    transform * Affine::translate((rect.x0 + 56.0, rect.y0 + 47.0)),
                    Some((rect.width() - 72.0) as f32),
                );
            }
        }

        if let Some(last) = cards.last() {
            text.draw_text(
                scene,
                "Arrow keys move  ·  Enter opens  ·  Super+N shows every app",
                theme.tokens.typography.scale.xs,
                theme.text_tertiary,
                transform * Affine::translate((content_left, last.y1 + 22.0)),
                None,
            );
        }
    }

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
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(index) = self.hovered() {
                    self.selected = index;
                    Self::fire(LaunchpadAction::ALL[index], state, path);
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.focused && event.state == ElementState::Pressed =>
            {
                let (_, columns) = Self::card_geometry(self.rect);
                let next = match event.logical_key {
                    Key::Named(NamedKey::ArrowRight) => {
                        Some((self.selected + 1).min(LaunchpadAction::ALL.len() - 1))
                    }
                    Key::Named(NamedKey::ArrowLeft) => Some(self.selected.saturating_sub(1)),
                    Key::Named(NamedKey::ArrowDown) => {
                        Some((self.selected + columns).min(LaunchpadAction::ALL.len() - 1))
                    }
                    Key::Named(NamedKey::ArrowUp) => Some(self.selected.saturating_sub(columns)),
                    Key::Named(NamedKey::Tab) => {
                        if modifiers.state().shift_key() {
                            Some(
                                self.selected
                                    .checked_sub(1)
                                    .unwrap_or(LaunchpadAction::ALL.len() - 1),
                            )
                        } else {
                            Some((self.selected + 1) % LaunchpadAction::ALL.len())
                        }
                    }
                    Key::Named(NamedKey::Enter | NamedKey::Space) => {
                        Self::fire(LaunchpadAction::ALL[self.selected], state, path);
                        *handled = true;
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
        self.card_rects
            .iter()
            .any(|rect| rect.contains(point))
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
                .with_description(
                    "Choose a shared conversation, notes, tasks, meeting, invitation, or workspace activity.",
                )
                .with_children(
                    LaunchpadAction::ALL
                        .into_iter()
                        .enumerate()
                        .map(|(index, action)| {
                            AccessNode::new(Role::Button)
                                .with_name(action.label())
                                .with_description(action.detail())
                                .with_action_target(action.path())
                                .with_state(AccessState {
                                    focusable: true,
                                    focused: self.focused && self.selected == index,
                                    ..Default::default()
                                })
                        })
                        .collect(),
                ),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else { return None };
        self.card_rects.get(*index).copied()
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [index] = path else { return false };
        if *index >= LaunchpadAction::ALL.len() {
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
            Some(("dev.qos.chat", "Chat"))
        );
        assert_eq!(
            LaunchpadAction::Meet.app_target(),
            Some(("dev.qos.chat", "Chat")),
            "calls are a Chat feature, not a fabricated Meet package"
        );
        assert_eq!(
            LaunchpadAction::Tasks.app_target(),
            Some(("dev.qos.calendar", "Calendar")),
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
}
