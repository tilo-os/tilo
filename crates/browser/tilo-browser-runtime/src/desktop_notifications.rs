//! The browser desktop's notifications: its own notices in the shared banner
//! stack, and from the top bar the shared notification centre in a panel
//! under it. No application can post here, so nothing answers a sender.
use std::cell::{Cell, Ref, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use tilo_desktop::notifications::{
    CenterLayout, CenterOutcome, HistoryActivation, Notification, NotificationCenter,
    NotificationHost, NotificationLog, NotificationStack,
};
use ui::a11y::AccessNode;
use ui::time::Instant;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// Width of the centre panel, and its inset from the desktop edges.
const PANEL_WIDTH: f64 = 360.0;
const PANEL_INSET: f64 = 16.0;

/// The log the browser desktop's notices go to.
#[derive(Default)]
pub(crate) struct BrowserNotifications {
    log: RefCell<NotificationLog>,
    /// Bumped by every change, so the layer repaints only when one happened.
    revision: Cell<u64>,
}

impl BrowserNotifications {
    fn touch(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    /// Show `notification` as a banner; any it pushes off the stack retire
    /// to the history.
    pub fn post(&self, notification: Notification) -> u64 {
        let id = notification.id;
        let evicted = self.log.borrow_mut().show(notification);
        for retired in evicted {
            self.log.borrow_mut().retain(retired);
        }
        self.touch();
        id
    }

    /// Retire banners whose dwell ran out and forget a lapsed Clear all.
    pub fn expire(&self, now: Instant) {
        let expired = self.log.borrow_mut().take_expired(now);
        let changed = !expired.is_empty();
        for retired in expired {
            self.log.borrow_mut().retain(retired);
        }
        if changed | self.log.borrow_mut().expire_clear(now) {
            self.touch();
        }
    }

    fn retire_active(&self, id: u64) {
        let taken = self.log.borrow_mut().take_active(id);
        if let Some(retired) = taken {
            self.log.borrow_mut().retain(retired);
            self.touch();
        }
    }
}

impl NotificationHost for BrowserNotifications {
    fn log(&self) -> Ref<'_, NotificationLog> {
        self.log.borrow()
    }
    fn dismiss(&self, id: u64) {
        self.retire_active(id);
    }
    fn activate(&self, id: u64) {
        self.retire_active(id);
    }
    /// The browser's own notices have no sender to invoke and no app to
    /// open; they stay retained until dismissed.
    fn activate_history(&self, id: u64) -> HistoryActivation {
        if self.log.borrow().history_entry(id).is_some() {
            HistoryActivation::Unavailable
        } else {
            HistoryActivation::Missing
        }
    }
    fn dismiss_history(&self, id: u64) {
        if self.log.borrow_mut().take_history(id).is_some() {
            self.touch();
        }
    }
    fn clear_history(&self) {
        if self.log.borrow_mut().clear_history(ui::time::now()) {
            self.touch();
        }
    }
    fn undo_clear(&self) {
        if self.log.borrow_mut().undo_clear() {
            self.touch();
        }
    }
    fn changed(&self) {
        self.touch();
    }
}

/// The banner stack over the desktop and, while open, the centre panel.
pub(crate) struct NotificationLayer {
    notifications: Rc<BrowserNotifications>,
    stack: NotificationStack,
    center: Option<NotificationCenter>,
    rect: Rect,
    cursor: Option<Point>,
    /// The caption and label sizes the centre paints with, captured from the
    /// theme each paint so input between paints measures the same type.
    type_px: (f32, f32),
    drawn_revision: Cell<Option<u64>>,
    /// Set by input that changed what the layer shows; its layout (one
    /// full-size leaf) never changes, so this only asks for a repaint.
    dirty: bool,
}

impl NotificationLayer {
    pub fn new(notifications: Rc<BrowserNotifications>) -> Self {
        let scale = &ui::Theme::dark().tokens.typography.scale;
        Self {
            notifications,
            stack: NotificationStack::new(),
            center: None,
            rect: Rect::ZERO,
            cursor: None,
            type_px: (scale.xs, scale.sm),
            drawn_revision: Cell::new(None),
            dirty: true,
        }
    }

    pub fn toggle_center(&mut self) {
        self.center = match self.center.take() {
            Some(_) => None,
            None => Some(NotificationCenter::new()),
        };
        self.dirty = true;
    }

    pub fn show_center(&mut self) {
        if self.center.is_none() {
            self.center = Some(NotificationCenter::new());
            self.dirty = true;
        }
    }

    pub fn center_open(&self) -> bool {
        self.center.is_some()
    }

    /// Retire what ran out; returns whether that changed what is shown.
    pub fn tick(&self) -> bool {
        let before = self.notifications.revision.get();
        self.notifications.expire(ui::time::now());
        self.notifications.revision.get() != before
    }

    /// The centre panel: top-right, under the top bar, as tall as its rows
    /// ask within the desktop.
    fn layout(&self) -> Option<CenterLayout> {
        let center = self.center.as_ref()?;
        let r = self.rect;
        let width = PANEL_WIDTH.min(r.width() - 2.0 * PANEL_INSET).max(0.0);
        let y0 = r.y0 + f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX) + PANEL_INSET / 2.0;
        let wanted = center.preferred_height(&self.notifications.log(), self.type_px);
        let height = wanted.min(r.y1 - PANEL_INSET - y0).max(0.0);
        let x1 = r.x1 - PANEL_INSET;
        Some(CenterLayout::new(
            Rect::new(x1 - width, y0, x1, y0 + height),
            self.type_px,
        ))
    }

    fn apply(&mut self, outcome: CenterOutcome) -> bool {
        match outcome {
            CenterOutcome::Ignored => return false,
            CenterOutcome::Handled => {}
            CenterOutcome::Back | CenterOutcome::OpenApp { .. } => self.center = None,
        }
        self.dirty = true;
        true
    }

    /// Route `event`; returns whether the layer consumed it.
    fn route(&mut self, event: &WindowEvent, scale: f64) -> bool {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(Point::new(position.x / scale, position.y / scale));
            if self.center.is_some() {
                // Hover lights the centre's controls.
                self.dirty = true;
            }
            return false;
        }
        let host = self.notifications.clone();
        let layout = self.layout();
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(p) = self.cursor else {
                    return false;
                };
                if let (Some(center), Some(layout)) = (self.center.as_mut(), layout) {
                    if layout.panel().contains(p) {
                        let outcome = center.click(&*host, &layout, p);
                        self.apply(outcome);
                        return true;
                    }
                    // A press outside the open panel closes it and goes no
                    // further, as a popover's does.
                    self.center = None;
                    self.dirty = true;
                    return true;
                }
                self.stack.click_at(&*host, p)
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (Some(center), Some(layout), Some(p)) =
                    (self.center.as_mut(), layout, self.cursor)
                else {
                    return false;
                };
                if !layout.panel().contains(p) {
                    return false;
                }
                center.wheel(&*host, &layout, *delta, scale);
                true
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let (Some(center), Some(layout)) = (self.center.as_mut(), layout) else {
                    return false;
                };
                if event.logical_key == Key::Named(NamedKey::Escape) {
                    self.center = None;
                    self.dirty = true;
                    return true;
                }
                let outcome = center.key(&*host, &layout, &event.logical_key);
                self.apply(outcome)
            }
            _ => false,
        }
    }
}

impl Widget for NotificationLayer {
    fn name(&self) -> &str {
        "DesktopNotifications"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn needs_redraw(&self) -> bool {
        self.dirty || self.drawn_revision.get() != Some(self.notifications.revision.get())
    }
    /// One full-size leaf: nothing it shows changes its size.
    fn is_layout_dirty(&self) -> bool {
        false
    }
    fn needs_animation_frame(&self) -> bool {
        self.stack.needs_animation_frame(&*self.notifications)
    }
    fn next_wake_deadline(&self) -> Option<Instant> {
        let banner = self.stack.next_wake_deadline(&*self.notifications);
        let undo = self.notifications.log().clear_deadline();
        banner.into_iter().chain(undo).min()
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _theme: &ui::Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = tree
            .new_leaf(taffy::Style {
                position: taffy::Position::Absolute,
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap_or_else(|_| NodeId::from(0u64));
        map.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        if let Ok(layout) = tree.layout(node) {
            self.rect = Rect::from_origin_size(
                origin,
                (f64::from(layout.size.width), f64::from(layout.size.height)),
            );
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut ui::TextRenderer,
        theme: &ui::Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        let scale = &theme.tokens.typography.scale;
        self.type_px = (scale.xs, scale.sm);
        let host = self.notifications.clone();
        let bar = f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX);
        self.stack.set_viewport(Rect::new(
            self.rect.x0,
            self.rect.y0 + bar,
            self.rect.x1,
            self.rect.y1,
        ));
        self.stack.draw(&*host, scene, transform, text, theme);
        if let Some(layout) = self.layout()
            && let Some(center) = self.center.as_mut()
        {
            let panel = layout.panel();
            ui::surface::panel(
                scene,
                transform,
                panel,
                f64::from(theme.tokens.radii.lg),
                theme,
            );
            center.draw(&*host, &layout, scene, transform, text, theme, self.cursor);
        }
        self.drawn_revision.set(Some(host.revision.get()));
        self.dirty = false;
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if !*ctx.handled && self.route(event, ctx.scale_factor) {
            *ctx.handled = true;
        }
    }
    /// The live region announcing banners, holding the centre's dialog
    /// while it is open. Absent while there is nothing to announce or show.
    fn accessibility(&self) -> Option<AccessNode> {
        let host = &*self.notifications;
        if self.center.is_none() && host.log().active().is_empty() {
            return None;
        }
        let mut node = self.stack.accessibility(host)?;
        if let (Some(center), Some(layout)) = (self.center.as_ref(), self.layout()) {
            node = node.with_children(vec![center.accessibility(host, &layout, ui::time::now())]);
        }
        Some(node)
    }
    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [0, dialog @ ..] = path else {
            return None;
        };
        let (center, layout) = (self.center.as_ref()?, self.layout()?);
        center.accessibility_rect_for_path(&*self.notifications, &layout, dialog)
    }
    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [0, dialog @ ..] = path else {
            return false;
        };
        let host = self.notifications.clone();
        let Some(center) = self.center.as_mut() else {
            return false;
        };
        let focused = center.focus_accessibility_path(&*host, dialog);
        self.dirty |= focused;
        focused
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer_with(count: usize) -> (NotificationLayer, Rc<BrowserNotifications>) {
        let notifications = Rc::new(BrowserNotifications::default());
        for i in 0..count {
            notifications.post(Notification::new(format!("n{i}"), "body"));
        }
        let mut layer = NotificationLayer::new(notifications.clone());
        layer.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        (layer, notifications)
    }

    fn press(layer: &mut NotificationLayer, p: Point) -> bool {
        layer.cursor = Some(p);
        layer.route(
            &WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            1.0,
        )
    }

    #[test]
    fn notices_retire_into_the_centre_and_it_closes_on_an_outside_press() {
        let (mut layer, notifications) = layer_with(2);
        let id = notifications.log().active()[0].id;
        notifications.dismiss(id);
        assert_eq!(notifications.log().history().len(), 1);
        layer.toggle_center();
        let panel = layer.layout().unwrap().panel();
        assert!(panel.y0 >= f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX));
        assert!(panel.x1 <= 1280.0 - PANEL_INSET);
        let node = layer.accessibility().unwrap();
        assert_eq!(node.children.len(), 1, "the centre's dialog is exposed");
        assert!(press(&mut layer, Point::new(10.0, 700.0)));
        assert!(!layer.center_open());
    }

    #[test]
    fn nothing_is_exposed_or_consumed_without_notices() {
        let (mut layer, _) = layer_with(0);
        assert!(layer.accessibility().is_none());
        assert!(!press(&mut layer, Point::new(1200.0, 80.0)));
    }

    #[test]
    fn a_retained_notice_stays_until_dismissed_from_the_centre() {
        let (mut layer, notifications) = layer_with(1);
        let id = notifications.log().active()[0].id;
        notifications.dismiss(id);
        layer.show_center();
        let layout = layer.layout().unwrap();
        assert!(press(&mut layer, layout.row(0).center()));
        assert_eq!(notifications.log().history().len(), 1);
        let (dismiss, _) = layout.row_controls(layout.row(0), false);
        assert!(press(&mut layer, dismiss.center()));
        assert!(notifications.log().history().is_empty());
    }
}
