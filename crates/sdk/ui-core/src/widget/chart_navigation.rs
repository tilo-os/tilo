//! Shared layout, pointer, touch and keyboard semantics for chart widgets.

use vello::kurbo::Point;
use winit::event::{ElementState, KeyEvent, MouseButton, TouchPhase, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// The layout half of a chart's `Widget` impl: a leaf as wide as its parent
/// and `self.height` tall, placed into `self.rect`, dirty until first placed.
macro_rules! chart_leaf {
    () => {
        fn build_layout(
            &mut self,
            taffy: &mut taffy::TaffyTree<()>,
            _theme: &crate::theme::Theme,
            node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            let style = taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::length(self.height),
                },
                ..Default::default()
            };
            crate::widget::TaffyTreeExt::widget_leaf(taffy, style, node_map, self)
        }

        crate::rect_geometry!();

        fn is_layout_dirty(&self) -> bool {
            self.rect == vello::kurbo::Rect::ZERO
        }
    };
}
pub(crate) use chart_leaf;

/// Paints a chart's title, if it has one, in the top-left corner of `rect`.
pub(crate) fn draw_title(
    scene: &mut vello::Scene,
    transform: vello::kurbo::Affine,
    tr: &mut crate::text_render::TextRenderer,
    theme: &crate::theme::Theme,
    rect: vello::kurbo::Rect,
    title: &str,
) {
    if title.is_empty() {
        return;
    }
    tr.draw_style(
        scene,
        title,
        theme.tokens.colors.text_primary,
        transform * vello::kurbo::Affine::translate((rect.x0 + 12.0, rect.y0 + 8.0)),
        None,
        &theme.text(crate::tokens::TextRole::Heading),
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartNavAction {
    Previous,
    Next,
    First,
    Last,
    Activate,
    Clear,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChartTouchAction {
    Inspect(Point),
    Activate(Point),
    Clear,
}

/// Tracks one finger and distinguishes a tap from a scroll/drag. Charts may
/// inspect continuously while only activating on a stationary release.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChartTouchTracker {
    active: Option<u64>,
    start: Point,
    moved: bool,
}

impl ChartTouchTracker {
    pub fn handle(&mut self, event: &WindowEvent, scale_factor: f64) -> Option<ChartTouchAction> {
        let WindowEvent::Touch(touch) = event else {
            return None;
        };
        let point = Point::new(
            touch.location.x / scale_factor,
            touch.location.y / scale_factor,
        );
        self.advance(touch.id, touch.phase, point)
    }

    fn advance(&mut self, id: u64, phase: TouchPhase, point: Point) -> Option<ChartTouchAction> {
        match phase {
            TouchPhase::Started if self.active.is_none() => {
                self.active = Some(id);
                self.start = point;
                self.moved = false;
                Some(ChartTouchAction::Inspect(point))
            }
            TouchPhase::Started => {
                // A second finger turns the gesture into navigation, never a
                // latent tap on the first finger.
                self.moved = true;
                None
            }
            TouchPhase::Moved if self.active == Some(id) => {
                self.moved |= self.start.distance(point) > 10.0;
                Some(ChartTouchAction::Inspect(point))
            }
            TouchPhase::Ended if self.active == Some(id) => {
                self.active = None;
                if std::mem::take(&mut self.moved) {
                    Some(ChartTouchAction::Clear)
                } else {
                    Some(ChartTouchAction::Activate(point))
                }
            }
            TouchPhase::Cancelled if self.active == Some(id) => {
                self.active = None;
                self.moved = false;
                Some(ChartTouchAction::Clear)
            }
            _ => None,
        }
    }
}

pub fn chart_nav_action(event: &WindowEvent) -> Option<ChartNavAction> {
    let WindowEvent::KeyboardInput {
        event:
            KeyEvent {
                logical_key,
                state: ElementState::Pressed,
                ..
            },
        ..
    } = event
    else {
        return None;
    };
    match logical_key {
        Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) => Some(ChartNavAction::Previous),
        Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) => Some(ChartNavAction::Next),
        Key::Named(NamedKey::Home) => Some(ChartNavAction::First),
        Key::Named(NamedKey::End) => Some(ChartNavAction::Last),
        Key::Named(NamedKey::Enter | NamedKey::Space) => Some(ChartNavAction::Activate),
        Key::Named(NamedKey::Escape) => Some(ChartNavAction::Clear),
        _ => None,
    }
}

pub fn moved_index(current: Option<usize>, len: usize, action: ChartNavAction) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let last = len.saturating_sub(1);
    match action {
        ChartNavAction::Previous => Some(current.unwrap_or(0).checked_sub(1).unwrap_or(last)),
        ChartNavAction::Next => Some(
            current
                .and_then(|index| index.checked_add(1))
                .and_then(|index| index.checked_rem(len))
                .unwrap_or_default(),
        ),
        ChartNavAction::First => Some(0),
        ChartNavAction::Last => Some(last),
        ChartNavAction::Clear => None,
        ChartNavAction::Activate => current,
    }
}

/// Move through a filtered list while preserving the caller's original item
/// indices (for example, skipping invalid or zero-valued data).
pub fn moved_item<T: Clone + PartialEq>(
    current: Option<T>,
    items: &[T],
    action: ChartNavAction,
) -> Option<T> {
    let position = current.and_then(|current| items.iter().position(|item| *item == current));
    moved_index(position, items.len(), action).and_then(|position| items.get(position).cloned())
}

/// The item key an accessibility `Activate` at `keys` targets: the last key.
/// Any other action, or an empty path, targets nothing.
pub(crate) fn activated_key(action: &crate::a11y::Action, keys: &[u64]) -> Option<u64> {
    if matches!(action, crate::a11y::Action::Activate) {
        keys.last().copied()
    } else {
        None
    }
}

/// Record in `activated` the id among `ids` whose accessibility key an
/// `Activate` at `keys` targets; whether one did.
pub(crate) fn activate<I: std::hash::Hash + AsRef<str>>(
    activated: &mut Option<String>,
    action: &crate::a11y::Action,
    keys: &[u64],
    ids: impl IntoIterator<Item = I>,
) -> bool {
    let Some(key) = activated_key(action, keys) else {
        return false;
    };
    let Some(id) = ids.into_iter().find(|id| crate::a11y::hash_key(id) == key) else {
        return false;
    };
    *activated = Some(id.as_ref().to_owned());
    true
}

/// The pointer, hovered item and keyboard/touch state an interactive chart
/// keeps between events.
#[derive(Clone, Debug)]
pub(crate) struct ChartInspection<H> {
    /// The last pointer position, or the inspected item's anchor while the
    /// keyboard drives.
    pub(crate) pointer: Point,
    pub(crate) hovered: Option<H>,
    /// Whether the keyboard chose the hovered item, which then survives the
    /// pointer leaving the window.
    pub(crate) keyboard: bool,
    pub(crate) touch: ChartTouchTracker,
}

impl<H> Default for ChartInspection<H> {
    fn default() -> Self {
        Self {
            pointer: Point::ZERO,
            hovered: None,
            keyboard: false,
            touch: ChartTouchTracker::default(),
        }
    }
}

impl<H> ChartInspection<H> {
    pub(crate) fn clear(&mut self) {
        self.hovered = None;
        self.keyboard = false;
    }
}

/// What a chart supplies so [`handle_chart_event`] can drive its hover,
/// activation and keyboard inspection.
pub(crate) trait InteractiveChart {
    type Hover: Clone + PartialEq;
    fn inspection(&mut self) -> &mut ChartInspection<Self::Hover>;
    /// The items the keyboard steps through, in order.
    fn keyboard_items(&self) -> Vec<Self::Hover>;
    /// Where inspecting `item` puts the pointer, so its tooltip shows there;
    /// `None` leaves the pointer where it is.
    fn anchor(&self, item: &Self::Hover) -> Option<Point>;
    /// Puts the pointer on the hovered item's [`anchor`](Self::anchor).
    fn anchor_hovered(&mut self) {
        let Some(item) = self.inspection().hovered.clone() else {
            return;
        };
        if let Some(anchor) = self.anchor(&item) {
            self.inspection().pointer = anchor;
        }
    }
    /// Keeps the pointer on an item the keyboard chose while the chart's
    /// geometry changes under it; call before painting.
    fn follow_keyboard_anchor(&mut self) {
        if self.inspection().keyboard {
            self.anchor_hovered();
        }
    }
    /// Hovers whatever lies under the pointer.
    fn update_hover(&mut self);
    fn clear_hover(&mut self) {
        self.inspection().hovered = None;
    }
    /// Activates the hovered item; false when nothing activatable is hovered.
    fn activate_hovered(&mut self) -> bool;
    /// Steps the hovered item through [`keyboard_items`](Self::keyboard_items)
    /// for any action but `Activate` and anchors the pointer on it; returns
    /// whether an item is now inspected.
    fn move_keyboard_inspection(&mut self, action: ChartNavAction) -> bool {
        let items = self.keyboard_items();
        let hovered = self.inspection().hovered.clone();
        self.inspection().hovered = moved_item(hovered, &items, action);
        self.anchor_hovered();
        self.inspection().hovered.is_some()
    }
    /// Toggles the legend entry under `point`, if the chart has one there.
    fn toggle_legend_at(&mut self, _point: Point) -> bool {
        false
    }
}

/// Hover follows the mouse or finger, a click or tap toggles a legend entry
/// or activates the hovered item, and keys step through the items. Leaving
/// the window keeps an item the keyboard chose.
pub(crate) fn handle_chart_event(
    chart: &mut impl InteractiveChart,
    event: &WindowEvent,
    ctx: &mut crate::widget::EventCtx<'_>,
) {
    let claim = |ctx: &mut crate::widget::EventCtx<'_>| {
        *ctx.handled = true;
        *ctx.focus_target = Some(ctx.node_id);
    };
    if let Some(action) = chart.inspection().touch.handle(event, ctx.scale_factor) {
        match action {
            ChartTouchAction::Inspect(point) => {
                chart.inspection().pointer = point;
                chart.update_hover();
                chart.inspection().keyboard = false;
            }
            ChartTouchAction::Activate(point) => {
                chart.inspection().pointer = point;
                chart.update_hover();
                if chart.toggle_legend_at(point) || chart.activate_hovered() {
                    claim(ctx);
                }
            }
            ChartTouchAction::Clear => chart.clear_hover(),
        }
        return;
    }
    match event {
        WindowEvent::CursorMoved { position, .. } => {
            chart.inspection().pointer = ctx.logical(position);
            chart.update_hover();
            chart.inspection().keyboard = false;
        }
        WindowEvent::CursorLeft { .. } if !chart.inspection().keyboard => chart.clear_hover(),
        WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Left,
            ..
        } => {
            let pointer = chart.inspection().pointer;
            if chart.toggle_legend_at(pointer) || chart.activate_hovered() {
                claim(ctx);
            }
        }
        _ => {
            let Some(action) = chart_nav_action(event) else {
                return;
            };
            if action == ChartNavAction::Activate {
                let _ = chart.activate_hovered();
            } else {
                chart.inspection().keyboard = chart.move_keyboard_inspection(action);
            }
            claim(ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ChartNavAction, ChartTouchAction, ChartTouchTracker, moved_index};
    use vello::kurbo::Point;
    use winit::event::TouchPhase;

    #[test]
    fn chart_navigation_wraps_and_touch_tracker_classifies_gestures() {
        fn navigation_wraps_and_handles_empty_data() {
            assert_eq!(moved_index(None, 0, ChartNavAction::Next), None);
            assert_eq!(moved_index(None, 3, ChartNavAction::Next), Some(0));
            assert_eq!(moved_index(Some(2), 3, ChartNavAction::Next), Some(0));
            assert_eq!(
                moved_index(Some(usize::MAX), 3, ChartNavAction::Next),
                Some(0)
            );
            assert_eq!(moved_index(Some(0), 3, ChartNavAction::Previous), Some(2));
            assert_eq!(
                super::moved_item(Some(4), &[2, 4, 8], ChartNavAction::Next),
                Some(8)
            );
        }

        fn touch_tracker_activates_taps_but_not_scrolls_or_pinches() {
            let mut tracker = ChartTouchTracker::default();
            let start = Point::new(10.0, 10.0);
            assert_eq!(
                tracker.advance(1, TouchPhase::Started, start),
                Some(ChartTouchAction::Inspect(start))
            );
            assert_eq!(
                tracker.advance(1, TouchPhase::Ended, start),
                Some(ChartTouchAction::Activate(start))
            );

            let _ = tracker.advance(2, TouchPhase::Started, start);
            let moved = Point::new(10.0, 30.0);
            assert_eq!(
                tracker.advance(2, TouchPhase::Moved, moved),
                Some(ChartTouchAction::Inspect(moved))
            );
            assert_eq!(
                tracker.advance(2, TouchPhase::Ended, moved),
                Some(ChartTouchAction::Clear)
            );

            let _ = tracker.advance(3, TouchPhase::Started, start);
            assert_eq!(tracker.advance(4, TouchPhase::Started, start), None);
            assert_eq!(
                tracker.advance(3, TouchPhase::Ended, start),
                Some(ChartTouchAction::Clear)
            );
        }

        navigation_wraps_and_handles_empty_data();
        touch_tracker_activates_taps_but_not_scrolls_or_pinches();
    }
}
