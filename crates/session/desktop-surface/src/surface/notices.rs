//! Notifications over the strip: the shared banner stack in the top-right
//! corner and, on request, the shared notification centre under it, both
//! reading a [`DesktopNotifications`] log the host posts into.
//!
//! Every banner that leaves the stack (closed, timed out, chosen, pushed
//! off by newer ones) is retained in the centre until the person dismisses
//! it there, as on the native desktop. What the person chose is handed back
//! to the host as [`NotificationOutcome`]s: only the host can answer the
//! application that sent a notification.
use std::cell::{Cell, Ref, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use ui::a11y::AccessNode;
use ui::kurbo::{Affine, Point, Rect};
use ui::taffy::{NodeId, TaffyTree};
use ui::time::Instant;
use ui::vello::Scene;
use ui::widget::Widget;
use ui::winit::event::{ElementState, MouseButton, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};

use crate::notifications::{
    CenterLayout, CenterOutcome, HistoryActivation, Notification, NotificationCenter,
    NotificationHost, NotificationLog, NotificationStack,
};

/// Width of the centre panel, and its inset from the surface's edges.
const PANEL_WIDTH: f64 = 360.0;
const PANEL_INSET: f64 = 16.0;

/// What the person did with a notification that its sender should hear.
#[derive(Clone, Debug)]
pub enum NotificationOutcome {
    /// The person chose the notification: its default action, or opening
    /// its application.
    Activated {
        notification: Box<Notification>,
        action: Option<String>,
    },
    /// The person removed a retained notification from the centre.
    Dismissed { id: u64 },
}

#[derive(Default)]
struct NoticeState {
    log: RefCell<NotificationLog>,
    /// Bumped by every change, so the layer repaints only when one happened.
    revision: Cell<u64>,
    outcomes: RefCell<Vec<NotificationOutcome>>,
}

impl NoticeState {
    fn touch(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    fn retire_active(&self, id: u64) -> Option<Notification> {
        let taken = self.log.borrow_mut().take_active(id)?;
        self.log.borrow_mut().retain(taken.clone());
        self.touch();
        Some(taken)
    }

    fn activated(&self, notification: Notification) {
        let action = notification.default_action_key();
        self.outcomes
            .borrow_mut()
            .push(NotificationOutcome::Activated {
                notification: Box::new(notification),
                action,
            });
    }
}

impl NotificationHost for NoticeState {
    fn log(&self) -> Ref<'_, NotificationLog> {
        self.log.borrow()
    }
    fn dismiss(&self, id: u64) {
        self.retire_active(id);
    }
    fn activate(&self, id: u64) {
        if let Some(notification) = self.retire_active(id) {
            self.activated(notification);
        }
    }
    fn activate_history(&self, id: u64) -> HistoryActivation {
        let Some(notification) = self.log.borrow().history_entry(id).cloned() else {
            return HistoryActivation::Missing;
        };
        if !notification.actions.is_empty() {
            self.activated(notification);
            return HistoryActivation::Invoked;
        }
        match (&notification.app_id, &notification.app_name) {
            (Some(app_id), label) => HistoryActivation::OpenApp {
                app_id: app_id.clone(),
                label: label.clone().unwrap_or_else(|| app_id.clone()),
            },
            (None, _) => HistoryActivation::Unavailable,
        }
    }
    fn dismiss_history(&self, id: u64) {
        if self.log.borrow_mut().take_history(id).is_some() {
            self.outcomes
                .borrow_mut()
                .push(NotificationOutcome::Dismissed { id });
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

/// A desktop's notifications: the live banners and the retained history
/// the surface shows. The host posts into it and reads back what the person
/// did. Cheap to clone; single-threaded.
#[derive(Clone, Default)]
pub struct DesktopNotifications {
    log: Rc<NoticeState>,
}

impl DesktopNotifications {
    /// Show `notification` as a banner; any it pushes off the stack retire
    /// to the centre. Returns its id.
    pub fn post(&self, notification: Notification) -> u64 {
        let id = notification.id;
        let evicted = self.log.log.borrow_mut().show(notification);
        for retired in evicted {
            self.log.log.borrow_mut().retain(retired);
        }
        self.log.touch();
        id
    }

    /// Withdraw notification `id`, live or retained: its sender took it back.
    pub fn withdraw(&self, id: u64) -> bool {
        let mut log = self.log.log.borrow_mut();
        let taken = log.take_active(id).is_some() | log.take_history(id).is_some();
        drop(log);
        if taken {
            self.log.touch();
        }
        taken
    }

    /// Retire banners whose dwell ran out and forget a lapsed Clear all.
    /// Returns whether anything changed.
    pub fn expire(&self, now: Instant) -> bool {
        let expired = self.log.log.borrow_mut().take_expired(now);
        let mut changed = !expired.is_empty();
        for retired in expired {
            self.log.log.borrow_mut().retain(retired);
        }
        changed |= self.log.log.borrow_mut().expire_clear(now);
        if changed {
            self.log.touch();
        }
        changed
    }

    /// Restore a host's persisted history without showing it as new banners.
    pub fn restore_history(&self, history: Vec<Notification>) {
        self.log.log.borrow_mut().restore_history(history);
        self.log.touch();
    }

    /// What the person chose since the last call, oldest first.
    pub fn take_outcomes(&self) -> Vec<NotificationOutcome> {
        std::mem::take(&mut *self.log.outcomes.borrow_mut())
    }

    pub fn revision(&self) -> u64 {
        self.log.revision.get()
    }

    /// Banners on screen and notifications retained in the centre.
    pub fn counts(&self) -> (usize, usize) {
        let log = self.log.log.borrow();
        (log.active().len(), log.history().len())
    }

    pub fn log(&self) -> Ref<'_, NotificationLog> {
        self.log.log.borrow()
    }

    fn host(&self) -> &dyn NotificationHost {
        &*self.log
    }
}

/// What an application asked of the desktop's notifications, queued until
/// a surface's next update: applications post from wherever their code
/// runs, the surface attributes and shows on its own thread.
#[derive(Debug)]
enum AppRequest {
    Show {
        /// The window whose application code was running.
        tab: Option<u64>,
        notification: ui::notify::Notification,
    },
    Withdraw {
        tab: Option<u64>,
        tag: String,
    },
}

/// Requests and how many surfaces passed them over: a window another
/// surface shows is that surface's to attribute.
type Queue = std::sync::Arc<std::sync::Mutex<Vec<(AppRequest, u8)>>>;

/// Surfaces that pass a request over this many times show it unattributed:
/// the window that posted it has closed.
const UNCLAIMED_PASSES: u8 = 4;

/// Applications' notifications for desktops that show them themselves: the
/// [`Notifier`](ui::notify::Notifier) a host installs as its services'
/// notifications. Each surface showing them takes an [`AppInbox`]; each
/// request is attributed to the window whose application code made it.
///
/// With no surface attached (an application mounted on its own), requests
/// go straight to `forward`, the platform's notifications. While the page
/// is in the background they also go there, where the person can see them.
pub struct AppNotifier {
    queue: Queue,
    inboxes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    forward: std::sync::Arc<dyn ui::notify::Notifier>,
    background: std::sync::Arc<std::sync::atomic::AtomicBool>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
}

/// One surface's side of an [`AppNotifier`].
pub struct AppInbox {
    queue: Queue,
    inboxes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// The banner each application's tag shows, so showing a tag again
    /// replaces it.
    shown: HashMap<(String, String), u64>,
    /// The activation URI each shown notification carries.
    activations: HashMap<u64, String>,
}

impl Drop for AppInbox {
    fn drop(&mut self) {
        self.inboxes
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

impl AppNotifier {
    /// `background` is set by the host while the page cannot be seen and
    /// the person allows `forward`'s notifications; `wake` schedules every
    /// surface's next update.
    pub fn new(
        forward: std::sync::Arc<dyn ui::notify::Notifier>,
        background: std::sync::Arc<std::sync::atomic::AtomicBool>,
        wake: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            queue: Queue::default(),
            inboxes: std::sync::Arc::default(),
            forward,
            background,
            wake,
        }
    }

    /// A surface's inbox of this notifier's requests.
    pub fn inbox(&self) -> AppInbox {
        self.inboxes
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        AppInbox {
            queue: std::sync::Arc::clone(&self.queue),
            inboxes: std::sync::Arc::clone(&self.inboxes),
            shown: HashMap::new(),
            activations: HashMap::new(),
        }
    }

    fn attached(&self) -> bool {
        self.inboxes.load(std::sync::atomic::Ordering::Acquire) > 0
    }

    fn push(&self, request: AppRequest) {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((request, 0));
        (self.wake)();
    }
}

impl ui::notify::Notifier for AppNotifier {
    fn show(&self, notification: &ui::notify::Notification) -> Result<(), ui::notify::NotifyError> {
        if !self.attached() {
            return self.forward.show(notification);
        }
        if notification.tag.is_empty() {
            return Err(ui::notify::NotifyError::Invalid(
                "a notification needs a tag",
            ));
        }
        if notification.title.trim().is_empty() {
            return Err(ui::notify::NotifyError::Invalid(
                "a notification needs a title",
            ));
        }
        if self.background.load(std::sync::atomic::Ordering::Acquire) {
            // The desktop keeps it either way; the platform copy is for a
            // page nobody is looking at.
            let _ = self.forward.show(notification);
        }
        self.push(AppRequest::Show {
            tab: ui::widget::panic_scope::current(),
            notification: notification.clone(),
        });
        Ok(())
    }

    fn withdraw(&self, tag: &str) -> Result<(), ui::notify::NotifyError> {
        if !self.attached() {
            return self.forward.withdraw(tag);
        }
        if tag.is_empty() {
            return Err(ui::notify::NotifyError::Invalid(
                "a notification needs a tag",
            ));
        }
        let _ = self.forward.withdraw(tag);
        self.push(AppRequest::Withdraw {
            tab: ui::widget::panic_scope::current(),
            tag: tag.to_owned(),
        });
        Ok(())
    }
}

/// The application a window runs: its id, display name and workspace.
pub(super) type AppOf<'a> = &'a dyn Fn(u64) -> Option<(String, String, Option<u64>)>;

impl AppInbox {
    /// Show and withdraw what applications asked since the last call.
    /// `app_of` names the application a window of this surface runs; a
    /// request from a window it does not show is left for the surface that
    /// does. Returns whether anything changed.
    pub(super) fn drain(
        &mut self,
        notifications: &DesktopNotifications,
        app_of: AppOf<'_>,
    ) -> bool {
        let requests = std::mem::take(
            &mut *self
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let mut changed = false;
        let mut passed = Vec::new();
        for (request, passes) in requests {
            let tab = match &request {
                AppRequest::Show { tab, .. } | AppRequest::Withdraw { tab, .. } => *tab,
            };
            let app = tab.and_then(app_of);
            if let (Some(_), None) = (tab, &app)
                && passes < UNCLAIMED_PASSES
            {
                passed.push((request, passes.saturating_add(1)));
                continue;
            }
            changed = true;
            let app_id = app.as_ref().map(|(id, ..)| id.clone()).unwrap_or_default();
            match request {
                AppRequest::Show { notification, .. } => {
                    let key = (app_id, notification.tag.clone());
                    if let Some(earlier) = self.shown.remove(&key) {
                        notifications.withdraw(earlier);
                        self.activations.remove(&earlier);
                    }
                    let mut shown = Notification::new(notification.title, notification.body);
                    if let Some((id, name, workspace)) = app {
                        shown.app_id = Some(id);
                        shown.app_name = Some(name);
                        shown.source_workspace = workspace;
                    }
                    if let Some(uri) = notification.activation {
                        shown.actions =
                            vec![("default".into(), crate::i18n::t("notification-action-open"))];
                        self.activations.insert(shown.id, uri);
                    }
                    self.shown.insert(key, notifications.post(shown));
                }
                AppRequest::Withdraw { tag, .. } => {
                    if let Some(id) = self.shown.remove(&(app_id, tag)) {
                        notifications.withdraw(id);
                        self.activations.remove(&id);
                    }
                }
            }
        }
        if !passed.is_empty() {
            self.queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend(passed);
        }
        changed
    }

    /// The activation URI notification `id` carries, once.
    pub fn take_activation(&mut self, id: u64) -> Option<String> {
        self.shown.retain(|_, shown| *shown != id);
        self.activations.remove(&id)
    }
}

/// The banner stack over the surface and, while open, the centre panel.
pub(super) struct NotificationLayer {
    notifications: DesktopNotifications,
    /// Applications' requests, when the host routes them here.
    pub inbox: Option<AppInbox>,
    stack: NotificationStack,
    center: Option<NotificationCenter>,
    /// An application a retained notification asked to open.
    pub open_app: Option<String>,
    rect: Rect,
    cursor: Option<Point>,
    /// The caption and label sizes the centre paints with, captured from the
    /// theme each paint so input between paints measures the same type.
    type_px: (f32, f32),
    drawn_revision: Cell<Option<u64>>,
    /// Input changed what the layer shows; its layout (one full-size leaf)
    /// never changes, so this only asks for a repaint.
    dirty: bool,
}

impl NotificationLayer {
    pub fn new(notifications: DesktopNotifications) -> Self {
        let scale = &ui::Theme::dark().tokens.typography.scale;
        Self {
            notifications,
            inbox: None,
            stack: NotificationStack::new(),
            center: None,
            open_app: None,
            rect: Rect::ZERO,
            cursor: None,
            type_px: (scale.xs, scale.sm),
            drawn_revision: Cell::new(None),
            dirty: true,
        }
    }

    pub fn notifications(&self) -> &DesktopNotifications {
        &self.notifications
    }

    pub fn set_center_open(&mut self, open: bool) {
        if open != self.center.is_some() {
            self.center = open.then(NotificationCenter::new);
            self.dirty = true;
        }
    }

    pub fn center_open(&self) -> bool {
        self.center.is_some()
    }

    /// Retire what ran out; returns whether that changed what is shown.
    pub fn tick(&self) -> bool {
        self.notifications.expire(ui::time::now())
    }

    /// The centre panel: top-right, as tall as its rows ask within the
    /// surface.
    fn layout(&self) -> Option<CenterLayout> {
        let center = self.center.as_ref()?;
        let r = self.rect;
        let width = PANEL_WIDTH.min(r.width() - 2.0 * PANEL_INSET).max(0.0);
        let y0 = r.y0 + PANEL_INSET;
        let wanted = center.preferred_height(&self.notifications.log(), self.type_px);
        let height = wanted.min(r.y1 - PANEL_INSET - y0).max(0.0);
        let x1 = r.x1 - PANEL_INSET;
        Some(CenterLayout::new(
            Rect::new(x1 - width, y0, x1, y0 + height),
            self.type_px,
        ))
    }

    pub(super) fn apply(&mut self, outcome: CenterOutcome) -> bool {
        match outcome {
            CenterOutcome::Ignored => return false,
            CenterOutcome::Handled => {}
            CenterOutcome::Back => self.center = None,
            CenterOutcome::OpenApp { app_id, .. } => {
                self.open_app = Some(app_id);
                self.center = None;
            }
        }
        self.dirty = true;
        true
    }

    /// Route `event`; returns whether the layer consumed it.
    pub fn route(&mut self, event: &WindowEvent, scale: f64) -> bool {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(Point::new(position.x / scale, position.y / scale));
            if self.center.is_some() {
                // Hover lights the centre's controls.
                self.dirty = true;
            }
            return false;
        }
        let notifications = self.notifications.clone();
        let host = notifications.host();
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
                        let outcome = center.click(host, &layout, p);
                        self.apply(outcome);
                        return true;
                    }
                    // A press outside the open panel closes it and goes no
                    // further, as a popover's does.
                    self.center = None;
                    self.dirty = true;
                    return true;
                }
                let hit = self.stack.click_at(host, p);
                self.dirty |= hit;
                hit
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
                center.wheel(host, &layout, *delta, scale);
                self.dirty = true;
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
                let outcome = center.key(host, &layout, &event.logical_key);
                self.apply(outcome)
            }
            _ => false,
        }
    }

    /// Whether the layer shows anything at all.
    pub fn visible(&self) -> bool {
        self.center.is_some() || !self.notifications.log().active().is_empty()
    }
}

impl Widget for NotificationLayer {
    fn name(&self) -> &'static str {
        "DesktopNotifications"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn needs_redraw(&self) -> bool {
        self.dirty || self.drawn_revision.get() != Some(self.notifications.revision())
    }
    fn is_layout_dirty(&self) -> bool {
        false
    }
    fn needs_animation_frame(&self) -> bool {
        self.stack.needs_animation_frame(self.notifications.host())
    }
    fn next_wake_deadline(&self) -> Option<Instant> {
        let banner = self.stack.next_wake_deadline(self.notifications.host());
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
            .new_leaf(super::history::overlay_style())
            .unwrap_or_else(|_| NodeId::from(0u64));
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        if let Ok(layout) = tree.layout(node) {
            self.rect = Rect::from_origin_size(
                Point::new(
                    origin.x + f64::from(layout.location.x),
                    origin.y + f64::from(layout.location.y),
                ),
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
        let notifications = self.notifications.clone();
        let host = notifications.host();
        self.stack.set_viewport(self.rect);
        self.stack.draw(host, scene, transform, text, theme);
        if let Some(layout) = self.layout()
            && let Some(center) = self.center.as_mut()
        {
            let panel = layout.panel();
            ui::surface::panel(
                scene,
                transform,
                panel,
                f64::from(theme.tokens.radii.control),
                theme,
            );
            center.draw(host, &layout, scene, transform, text, theme, self.cursor);
        }
        self.drawn_revision.set(Some(notifications.revision()));
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
        let host = self.notifications.host();
        if !self.visible() {
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
        center.accessibility_rect_for_path(self.notifications.host(), &layout, dialog)
    }
    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [0, dialog @ ..] = path else {
            return false;
        };
        let notifications = self.notifications.clone();
        let Some(center) = self.center.as_mut() else {
            return false;
        };
        let focused = center.focus_accessibility_path(notifications.host(), dialog);
        self.dirty |= focused;
        focused
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer_with(count: usize) -> (NotificationLayer, DesktopNotifications) {
        let notifications = DesktopNotifications::default();
        for i in 0..count {
            notifications.post(
                Notification::new(format!("n{i}"), "body")
                    .with_app_name("Clocks")
                    .with_actions(i),
            );
        }
        let mut layer = NotificationLayer::new(notifications.clone());
        layer.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        (layer, notifications)
    }

    trait WithActions {
        fn with_actions(self, i: usize) -> Self;
    }
    impl WithActions for Notification {
        fn with_actions(mut self, i: usize) -> Self {
            self.app_id = Some("so.tilo.clocks".into());
            if i.is_multiple_of(2) {
                self.actions = vec![("default".into(), "Open".into())];
            }
            self
        }
    }

    fn press(layer: &mut NotificationLayer, p: Point) -> bool {
        layer.cursor = Some(p);
        layer.route(
            &WindowEvent::MouseInput {
                device_id: ui::winit::event::DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            1.0,
        )
    }

    #[test]
    fn banners_retire_into_the_centre_and_it_closes_on_an_outside_press() {
        let (mut layer, notifications) = layer_with(2);
        let id = notifications.log().active()[0].id;
        notifications.host().dismiss(id);
        assert_eq!(notifications.counts(), (1, 1));
        layer.set_center_open(true);
        let panel = layer.layout().unwrap().panel();
        assert!(panel.x1 <= 1280.0 - PANEL_INSET);
        let node = layer.accessibility().unwrap();
        assert_eq!(node.children.len(), 1, "the centre's dialog is exposed");
        assert!(press(&mut layer, Point::new(10.0, 700.0)));
        assert!(!layer.center_open());
        assert!(
            notifications.take_outcomes().is_empty(),
            "closing a banner answers nobody"
        );
    }

    #[test]
    fn nothing_is_exposed_or_consumed_without_notifications() {
        let (mut layer, _) = layer_with(0);
        assert!(layer.accessibility().is_none());
        assert!(!press(&mut layer, Point::new(1200.0, 80.0)));
    }

    #[test]
    fn choosing_a_banner_hands_its_default_action_to_the_host() {
        let (_, notifications) = layer_with(1);
        let id = notifications.log().active()[0].id;
        notifications.host().activate(id);
        let outcomes = notifications.take_outcomes();
        assert!(
            matches!(&outcomes[..], [NotificationOutcome::Activated { notification, action }]
                if notification.id == id && action.as_deref() == Some("default")),
            "{outcomes:?}"
        );
        assert_eq!(
            notifications.counts(),
            (0, 1),
            "a chosen banner is retained"
        );
    }

    #[test]
    fn a_retained_notification_without_actions_opens_its_app_and_stays_until_dismissed() {
        let (mut layer, notifications) = layer_with(2);
        let quiet = notifications.log().active()[1].id;
        notifications.host().dismiss(quiet);
        assert!(matches!(
            notifications.host().activate_history(quiet),
            HistoryActivation::OpenApp { ref app_id, .. } if app_id == "so.tilo.clocks"
        ));
        layer.set_center_open(true);
        let layout = layer.layout().unwrap();
        let (dismiss, _) = layout.row_controls(layout.row(0), false);
        assert!(press(&mut layer, dismiss.center()));
        assert_eq!(notifications.log().history().len(), 0);
        assert!(matches!(
            &notifications.take_outcomes()[..],
            [NotificationOutcome::Dismissed { id }] if *id == quiet
        ));
    }

    #[derive(Default)]
    struct Platform(std::sync::Mutex<Vec<String>>);
    impl ui::notify::Notifier for Platform {
        fn show(&self, n: &ui::notify::Notification) -> Result<(), ui::notify::NotifyError> {
            self.0.lock().unwrap().push(n.title.clone());
            Ok(())
        }
        fn withdraw(&self, _: &str) -> Result<(), ui::notify::NotifyError> {
            Ok(())
        }
    }

    fn notifier() -> (
        AppNotifier,
        std::sync::Arc<Platform>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        let platform = std::sync::Arc::new(Platform::default());
        let background = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let notifier = AppNotifier::new(
            platform.clone(),
            std::sync::Arc::clone(&background),
            std::sync::Arc::new(|| {}),
        );
        (notifier, platform, background)
    }

    fn clocks(tab: u64) -> Option<(String, String, Option<u64>)> {
        (tab == 7).then(|| ("so.tilo.clocks".to_owned(), "Clocks".to_owned(), Some(1)))
    }

    #[test]
    fn an_apps_notifications_show_on_the_desktop_attributed_to_its_window() {
        use ui::notify::Notifier as _;
        let (notifier, platform, background) = notifier();
        let mut inbox = notifier.inbox();
        let notifications = DesktopNotifications::default();
        {
            let _running = ui::widget::panic_scope::enter(Some(7));
            notifier
                .show(
                    &ui::notify::Notification::new("timer", "Tea", "Steeped")
                        .with_activation("tilo-clocks:timer/1"),
                )
                .unwrap();
            notifier
                .show(&ui::notify::Notification::new("timer", "Tea", "Ready"))
                .unwrap();
        }
        assert!(inbox.drain(&notifications, &clocks));
        let log = notifications.log();
        assert_eq!(log.active().len(), 1, "the same tag replaces its banner");
        let shown = &log.active()[0];
        assert_eq!(shown.body, "Ready");
        assert_eq!(shown.app_id.as_deref(), Some("so.tilo.clocks"));
        assert_eq!(shown.app_name.as_deref(), Some("Clocks"));
        drop(log);
        assert!(
            platform.0.lock().unwrap().is_empty(),
            "a page in view keeps them on its desktop"
        );

        background.store(true, std::sync::atomic::Ordering::Release);
        {
            let _running = ui::widget::panic_scope::enter(Some(7));
            notifier
                .show(
                    &ui::notify::Notification::new("alarm", "Alarm", "07:00")
                        .with_activation("tilo-clocks:alarm/1"),
                )
                .unwrap();
        }
        inbox.drain(&notifications, &clocks);
        assert_eq!(
            *platform.0.lock().unwrap(),
            ["Alarm"],
            "a hidden page's go to the platform too"
        );
        let alarm = notifications.log().active()[1].clone();
        assert_eq!(alarm.default_action_key().as_deref(), Some("default"));
        assert_eq!(
            inbox.take_activation(alarm.id).as_deref(),
            Some("tilo-clocks:alarm/1")
        );
        {
            let _running = ui::widget::panic_scope::enter(Some(7));
            notifier.withdraw("timer").unwrap();
        }
        inbox.drain(&notifications, &clocks);
        assert_eq!(notifications.counts(), (1, 0));
    }

    #[test]
    fn another_surfaces_window_is_left_to_it_and_no_surface_means_the_platform() {
        use ui::notify::Notifier as _;
        let (notifier, platform, _) = notifier();
        {
            let inbox = notifier.inbox();
            drop(inbox);
        }
        notifier
            .show(&ui::notify::Notification::new("t", "Alone", ""))
            .unwrap();
        assert_eq!(*platform.0.lock().unwrap(), ["Alone"]);

        let mut ours = notifier.inbox();
        let mut theirs = notifier.inbox();
        let (mine, other) = (
            DesktopNotifications::default(),
            DesktopNotifications::default(),
        );
        {
            let _running = ui::widget::panic_scope::enter(Some(7));
            notifier
                .show(&ui::notify::Notification::new("t", "Theirs", ""))
                .unwrap();
        }
        assert!(!ours.drain(&mine, &|_| None), "not a window of ours");
        assert!(theirs.drain(&other, &clocks));
        assert_eq!((mine.counts(), other.counts()), ((0, 0), (1, 0)));
    }

    #[test]
    fn a_sender_withdraws_live_and_retained_notifications() {
        let (_, notifications) = layer_with(2);
        let live = notifications.log().active()[0].id;
        let retained = notifications.log().active()[1].id;
        notifications.host().dismiss(retained);
        assert!(notifications.withdraw(live));
        assert!(notifications.withdraw(retained));
        assert!(!notifications.withdraw(retained));
        assert_eq!(notifications.counts(), (0, 0));
    }
}
