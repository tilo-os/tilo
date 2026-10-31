//! The desktop under the windows, and the window policy it answers.
//!
//! [`desktop`] makes the pair the compositor runs a session with: a
//! [`DesktopHost`] drawing the portable desktop surface (its strips, window
//! bars and edges) under every window, and a [`StripPolicy`] placing each
//! window where that surface lays its tab out ([`tilo_desktop_policy`]).
//! Both share one desktop, so a window opened, focused, closed or panned in
//! one is where the other shows it.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_compositor::gestures::{
    GestureAction, GestureConfig, GestureTracker, SWIPE_THRESHOLD, claims,
};
use tilo_compositor::{
    Arrangement, GestureEvent, HostLayer, LayoutPoint, LayoutRect, SurfaceHost, WindowInfo,
    WindowPolicy, WindowRequest,
};
use tilo_desktop_policy::displays::Output;
use tilo_desktop_policy::floating::centred;
use tilo_desktop_policy::{DesktopPolicy, Window};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface};

use crate::{Placement, WidgetHost};

/// Where a window not shown waits: left of every output, out of sight and
/// out of the pointer's reach.
const OFFSTAGE: i32 = -(1 << 20);

/// What a touchpad gesture the desktop took asks of it, once the gesture
/// has said what it means: the same steps the desktop's chords take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeskGesture {
    /// Three fingers across: the window `steps` on along the strip.
    StepWindow(i32),
    /// Three fingers up or down: the strip `steps` on.
    StepStrip(i32),
    /// Four fingers up or down: the workspace `delta` on.
    SwitchWorkspace(i32),
    /// A pinch in opens the overview (`true`), out closes it.
    Overview(bool),
}

/// The desktop both halves share.
struct Desk {
    policy: DesktopPolicy,
    host: WidgetHost,
    /// What the gestures the desktop took asked, for its host to carry out.
    gestures: Vec<DeskGesture>,
    /// How the desktop reads touchpad gestures; `None` leaves every one to
    /// the clients.
    gesture_config: Option<GestureConfig>,
    /// The monitor on each output, as the compositor knows it.
    monitors: Vec<(LayoutRect, String)>,
}

impl Desk {
    /// Cover `outputs`' bounding box, the surface laid out against it.
    fn cover(&mut self, outputs: &[LayoutRect]) {
        let Some(bounds) = outputs.iter().copied().reduce(LayoutRect::merge) else {
            return;
        };
        let (Ok(width), Ok(height)) = (u32::try_from(bounds.size.w), u32::try_from(bounds.size.h))
        else {
            return;
        };
        let placement = Placement {
            origin: (bounds.loc.x, bounds.loc.y),
            size: (width.max(1), height.max(1)),
            scale: self.host.placement().scale,
        };
        if placement != self.host.placement() {
            self.policy.set_viewport_width(f64::from(placement.size.0));
            self.host.set_placement(placement);
        }
        // Each display holds its own windows. A monitor is known by where
        // it sits, as the policy is told it.
        let displays: Vec<Output> = outputs
            .iter()
            .map(|output| {
                let x = f64::from(output.loc.x.saturating_sub(bounds.loc.x));
                let y = f64::from(output.loc.y.saturating_sub(bounds.loc.y));
                Output {
                    monitor: self
                        .monitors
                        .iter()
                        .find(|(rect, _)| rect == output)
                        .map_or_else(
                            || format!("output:{},{}", output.loc.x, output.loc.y),
                            |(_, monitor)| monitor.clone(),
                        ),
                    rect: ui::kurbo::Rect::new(
                        x,
                        y,
                        x + f64::from(output.size.w),
                        y + f64::from(output.size.h),
                    ),
                }
            })
            .collect();
        if self.policy.set_displays(&displays) {
            self.host.relayout();
        }
    }

    /// Where a dialog of `size` floats: centred over its parent, or the
    /// display the person works on, in the logical layout. `None` while it
    /// is hidden with its parent.
    fn floating(&self, window: u64, size: (i32, i32)) -> Option<LayoutPoint> {
        let (ox, oy) = self.host.placement().origin;
        let (_, area) = self
            .policy
            .layout()
            .floating
            .into_iter()
            .find(|(id, _)| *id == window)?;
        let at = centred(area, (f64::from(size.0), f64::from(size.1)));
        Some(LayoutPoint::from((
            ox.saturating_add(round(at.x)),
            oy.saturating_add(round(at.y)),
        )))
    }

    /// Where `window`'s content is in the logical layout, as last laid out.
    fn shown(&self, window: u64) -> Option<LayoutRect> {
        let (ox, oy) = self.host.placement().origin;
        self.policy
            .layout()
            .shown
            .into_iter()
            .find(|(id, _)| *id == window)
            .map(|(_, rect)| {
                LayoutRect::new(
                    LayoutPoint::from((
                        ox.saturating_add(round(rect.x0)),
                        oy.saturating_add(round(rect.y0)),
                    )),
                    (round(rect.width()).max(1), round(rect.height()).max(1)).into(),
                )
            })
    }
}

/// `value` rounded to a whole logical pixel.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a window's place on a screen"
)]
fn round(value: f64) -> i32 {
    value.round() as i32
}

/// The desktop drawn under every window; a clone is the same desktop.
#[derive(Clone)]
pub struct DesktopHost {
    desk: Rc<RefCell<Desk>>,
}

/// Told about the windows a program opens and closes, by the policy: what
/// finds the program behind a window that named itself.
pub trait WindowObserver: Send + Sync {
    /// `window` named itself and is about to map.
    fn opened(&self, window: &WindowInfo);
    /// The window `id` is gone.
    fn closed(&self, id: u64);
}

/// The window policy answered by the desktop's strips.
pub struct StripPolicy {
    desk: Rc<RefCell<Desk>>,
    /// The windows given a place, so one gone from the compositor closes
    /// its tab.
    mapped: Vec<u64>,
    /// The touchpad gesture in flight, when the desktop took it.
    tracker: GestureTracker,
    /// How far the fingers have carried the strip, or the strip deck, in
    /// the gesture in flight.
    travel: f64,
    /// Those of them that have not shown a buffer yet: they wait in their
    /// place, which the compositor does not list among its windows.
    pending: Vec<u64>,
    /// Told the process of each window the compositor maps, the moment the
    /// desktop gives it a place.
    map_observer: Option<Rc<dyn Fn(u32)>>,
    /// Who follows the windows, when the session has someone that does.
    observer: Option<std::sync::Arc<dyn WindowObserver>>,
    /// The windows it was told about, so none is told of twice.
    announced: std::collections::HashSet<u64>,
}

/// A desktop of one workspace with one empty strip (`salt` seeds its ids),
/// drawn at `scale`: the surface under the windows and the policy placing
/// them. The compositor hosts the one and runs with the other.
#[must_use]
pub fn desktop(salt: u16, scale: f64) -> (DesktopHost, StripPolicy) {
    desktop_with(salt, scale, |surface, _| Box::new(surface))
}

/// As [`desktop`], with the surface drawn inside the tree `root` makes of
/// it (the desk's backdrop and launchpad around the strips), given the
/// policy's host side of the surface.
#[must_use]
pub fn desktop_with(
    salt: u16,
    scale: f64,
    root: impl FnOnce(DesktopSurface, &DesktopHandle) -> Box<dyn ui::Widget>,
) -> (DesktopHost, StripPolicy) {
    let (policy, surface) = DesktopPolicy::new(salt);
    let root = root(surface, policy.handle());
    let placement = Placement {
        origin: (0, 0),
        size: (1, 1),
        scale,
    };
    let desk = Rc::new(RefCell::new(Desk {
        policy,
        host: WidgetHost::new(root, placement, true),
        gestures: Vec::new(),
        gesture_config: Some(GestureConfig::default()),
        monitors: Vec::new(),
    }));
    (
        DesktopHost {
            desk: Rc::clone(&desk),
        },
        StripPolicy {
            desk,
            mapped: Vec::new(),
            tracker: GestureTracker::default(),
            travel: 0.0,
            pending: Vec::new(),
            map_observer: None,
            observer: None,
            announced: std::collections::HashSet::new(),
        },
    )
}

impl DesktopHost {
    /// The desktop's policy, for everything else a session drives on it
    /// (strips and workspaces, the tree it keeps).
    pub fn with_policy<R>(&self, read: impl FnOnce(&mut DesktopPolicy) -> R) -> R {
        read(&mut self.desk.borrow_mut().policy)
    }

    /// Read touchpad gestures by `config` from the next one on; `None`
    /// leaves them all to the clients.
    pub fn set_gestures(&self, config: Option<GestureConfig>) {
        self.desk.borrow_mut().gesture_config = config;
    }

    /// Draw the desk with `theme`: the person's palette, tokens and fonts.
    pub fn set_theme(&self, theme: ui::Theme) {
        self.desk.borrow_mut().host.set_theme(theme);
    }

    /// What the touchpad gestures the desktop took asked since the last
    /// call, oldest first.
    pub fn take_gestures(&self) -> Vec<DeskGesture> {
        std::mem::take(&mut self.desk.borrow_mut().gestures)
    }

    /// What the desktop asked to open since the last call, oldest first:
    /// the apps the session plans and starts.
    pub fn take_launches(&self) -> Vec<String> {
        self.with_policy(DesktopPolicy::take_launches)
    }

    /// The desk's widget tree as hosted: what it publishes, its
    /// accessibility tree, the actions it accepts.
    pub fn with_widgets<R>(&self, read: impl FnOnce(&mut WidgetHost) -> R) -> R {
        read(&mut self.desk.borrow_mut().host)
    }
}

impl crate::Published for DesktopHost {
    fn resources(&mut self) -> Vec<ui::tilo_actions::ResourceSpec> {
        crate::Published::resources(&mut self.desk.borrow_mut().host)
    }

    fn revision(&self) -> Option<u64> {
        Some(self.desk.borrow().host.revision())
    }

    fn invoke(
        &mut self,
        address: &ui::tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ui::host_operation::ResourceOperationStart, ui::tilo_actions::DoorError> {
        crate::Published::invoke(&mut self.desk.borrow_mut().host, address, arguments)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        crate::Published::accessible_labels(&mut self.desk.borrow_mut().host)
    }
}

impl SurfaceHost for DesktopHost {
    fn layers(&self) -> Vec<HostLayer> {
        self.desk.borrow().host.layers()
    }

    fn below_windows(&self) -> bool {
        true
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        self.desk.borrow().host.accepts_pointer(x, y)
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.desk.borrow_mut().host.pointer_motion(x, y);
    }

    fn pointer_leave(&mut self) {
        self.desk.borrow_mut().host.pointer_leave();
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.desk.borrow_mut().host.pointer_button(button, pressed);
    }

    fn pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        self.desk
            .borrow_mut()
            .host
            .pointer_axis(horizontal, vertical);
    }

    fn prepare_frame(&mut self) -> bool {
        self.desk.borrow_mut().host.prepare_frame()
    }

    fn window_pictures(&self) -> Vec<tilo_compositor::WindowPicture> {
        self.desk.borrow().host.window_pictures()
    }

    fn backdrops(&self) -> Vec<tilo_compositor::BackdropRequest> {
        self.desk.borrow().host.backdrops()
    }

    fn set_backdrops(&mut self, images: Vec<Option<tilo_compositor::BackdropImage>>) {
        self.desk.borrow_mut().host.set_backdrops(images);
    }
}

impl StripPolicy {
    /// Tell `observer` the process of each window the compositor maps, as the
    /// desktop gives it a place: the app's door opens to it from then on.
    pub fn set_window_mapped(&mut self, observer: Rc<dyn Fn(u32)>) {
        self.map_observer = Some(observer);
    }

    /// Follow the windows the compositor opens and closes with `observer`:
    /// how a session learns which programs the person runs.
    pub fn observe_windows(&mut self, observer: std::sync::Arc<dyn WindowObserver>) {
        self.observer = Some(observer);
    }

    /// Tell the observer about `window`, once.
    fn announce(&mut self, window: &WindowInfo) {
        let Some(observer) = self.observer.clone() else {
            return;
        };
        if !self.announced.insert(window.id) {
            return;
        }
        observer.opened(window);
    }

    /// Tell the observer that `id` is gone, once.
    fn forgot(&mut self, id: u64) {
        if !self.announced.remove(&id) {
            return;
        }
        if let Some(observer) = &self.observer {
            observer.closed(id);
        }
    }

    /// Give `window` its tab, once, and lay the desktop out so its place
    /// is known.
    fn map(&mut self, window: &WindowInfo, outputs: &[LayoutRect]) {
        {
            let mut desk = self.desk.borrow_mut();
            desk.cover(outputs);
            if self.mapped.contains(&window.id) {
                return;
            }
            let mapped = desk.policy.map(Window {
                id: window.id,
                app_id: window.app_id.clone(),
                title: window.title.clone(),
                parent: window.parent,
                min_size: window.min_size,
                max_size: window.max_size,
            });
            if let Err(error) = mapped {
                tracing::warn!(%error, window = window.id, "the desktop has no strip to open a window in");
                return;
            }
            self.mapped.push(window.id);
            desk.host.relayout();
        }
        if let (Some(observer), Some(pid)) = (self.map_observer.as_ref(), window.pid) {
            observer(pid);
        }
        self.announce(window);
    }
}

impl WindowPolicy for StripPolicy {
    /// Focus follows the pointer onto another display: to the window last
    /// focused there, else to none, so the next window opens there.
    fn pointer_moved(&mut self, x: f64, y: f64) {
        let mut desk = self.desk.borrow_mut();
        let (ox, oy) = desk.host.placement().origin;
        let (x, y) = (x - f64::from(ox), y - f64::from(oy));
        let handle = desk.policy.handle().clone();
        let under = handle.read(|tree| {
            if !tree.is_partitioned() {
                return None;
            }
            tree.outputs
                .displays
                .iter()
                .find(|d| x >= d.x && x < d.x + d.width && y >= d.y && y < d.y + d.height)
                .map(|d| d.id)
        });
        if let Some(display) = under
            && handle.focused_display() != Some(display)
            && handle.focus_display(display)
        {
            desk.host.relayout();
        }
    }

    fn monitors(&mut self, monitors: &[(LayoutRect, String)]) {
        monitors.clone_into(&mut self.desk.borrow_mut().monitors);
    }

    fn initial_size(&mut self, window: &WindowInfo, outputs: &[LayoutRect]) -> Option<(i32, i32)> {
        self.map(window, outputs);
        if !self.pending.contains(&window.id) {
            self.pending.push(window.id);
        }
        // A dialog picks its own size.
        let rect = self.desk.borrow().shown(window.id)?;
        Some((rect.size.w, rect.size.h))
    }

    fn place(
        &mut self,
        window: &WindowInfo,
        size: (i32, i32),
        outputs: &[LayoutRect],
    ) -> LayoutPoint {
        self.map(window, outputs);
        self.pending.retain(|id| *id != window.id);
        let desk = self.desk.borrow();
        desk.shown(window.id)
            .map(|rect| rect.loc)
            .or_else(|| desk.floating(window.id, size))
            .unwrap_or_else(|| LayoutPoint::from((OFFSTAGE, 0)))
    }

    fn arrange(
        &mut self,
        windows: &[(WindowInfo, LayoutRect)],
        outputs: &[LayoutRect],
    ) -> Option<Vec<Arrangement>> {
        // A window the compositor lists has shown its buffer.
        self.pending
            .retain(|id| !windows.iter().any(|(window, _)| window.id == *id));
        let gone: Vec<u64> = self
            .mapped
            .iter()
            .copied()
            .filter(|id| {
                !self.pending.contains(id) && !windows.iter().any(|(window, _)| window.id == *id)
            })
            .collect();
        {
            let mut desk = self.desk.borrow_mut();
            desk.cover(outputs);
            for id in &gone {
                desk.policy.unmap(*id);
            }
            for (window, _) in windows {
                if let Some(title) = &window.title {
                    desk.policy.retitle(window.id, title);
                }
            }
            if !gone.is_empty() {
                desk.host.relayout();
            }
        }
        self.mapped.retain(|id| !gone.contains(id));
        for id in &gone {
            self.forgot(*id);
        }
        for (window, _) in windows {
            self.map(window, outputs);
        }
        let desk = self.desk.borrow();
        Some(
            windows
                .iter()
                .map(|(window, current)| {
                    if let Some(rect) = desk.shown(window.id) {
                        return Arrangement {
                            id: window.id,
                            location: rect.loc,
                            size: Some((rect.size.w, rect.size.h)),
                        };
                    }
                    let size = (current.size.w, current.size.h);
                    Arrangement {
                        id: window.id,
                        location: desk
                            .floating(window.id, size)
                            .unwrap_or_else(|| LayoutPoint::from((OFFSTAGE, 0))),
                        size: None,
                    }
                })
                .collect(),
        )
    }

    fn withdrawn(&mut self, window: &WindowInfo) {
        self.pending.retain(|id| *id != window.id);
        self.mapped.retain(|id| *id != window.id);
        self.forgot(window.id);
        let mut desk = self.desk.borrow_mut();
        desk.policy.unmap(window.id);
        desk.host.relayout();
    }

    fn focus_on_click(&mut self, window: &WindowInfo) -> bool {
        let mut desk = self.desk.borrow_mut();
        // Focus moves on the surface's next pass: run it, so the window's
        // place and focus are known when the compositor asks.
        if desk.policy.focus(window.id) {
            desk.host.relayout();
        }
        true
    }

    fn focused(&mut self) -> Option<u64> {
        self.desk.borrow().policy.focused()
    }

    fn activate(&mut self, window: &WindowInfo, fresh: bool) -> bool {
        let mut desk = self.desk.borrow_mut();
        if fresh {
            // What the person just did opened it: bring it forward.
            if desk.policy.focus(window.id) {
                desk.host.relayout();
            }
        } else if desk.policy.ask_attention(window.id) {
            desk.host.relayout();
        }
        fresh
    }

    fn gesture(&mut self, gesture: &GestureEvent) -> bool {
        let action = match *gesture {
            GestureEvent::SwipeBegin { fingers } | GestureEvent::PinchBegin { fingers } => {
                let config = self.desk.borrow().gesture_config;
                let Some(config) = config.filter(|_| claims(fingers)) else {
                    return false;
                };
                self.tracker.set_config(config);
                self.tracker.swipe_begin(fingers);
                self.travel = 0.0;
                None
            }
            GestureEvent::SwipeUpdate { dx, dy } => self.tracker.swipe_update(dx, dy),
            GestureEvent::SwipeEnd { cancelled } | GestureEvent::PinchEnd { cancelled } => {
                self.tracker.swipe_end_with_cancel(cancelled)
            }
            GestureEvent::PinchUpdate { scale, .. } => self.tracker.pinch_update(scale),
            // A hold asks nothing of the desktop: the client keeps it.
            GestureEvent::HoldBegin { .. } | GestureEvent::HoldEnd { .. } => return false,
        };
        let step = |travel: f64| {
            if travel >= SWIPE_THRESHOLD {
                Some(1)
            } else if travel <= -SWIPE_THRESHOLD {
                Some(-1)
            } else {
                None
            }
        };
        let asked = match action {
            Some(
                GestureAction::StripPanUpdate { travel, .. }
                | GestureAction::StripDeckUpdate { travel, .. },
            ) => {
                self.travel = travel;
                None
            }
            Some(GestureAction::StripPanEnd { cancelled: false }) => {
                step(self.travel).map(DeskGesture::StepWindow)
            }
            Some(GestureAction::StripDeckEnd { cancelled: false }) => {
                step(self.travel).map(DeskGesture::StepStrip)
            }
            Some(GestureAction::SwitchWorkspace(delta)) => {
                Some(DeskGesture::SwitchWorkspace(delta))
            }
            Some(GestureAction::ShowOverview) => Some(DeskGesture::Overview(true)),
            Some(GestureAction::HideOverview) => Some(DeskGesture::Overview(false)),
            Some(GestureAction::StripPanEnd { .. } | GestureAction::StripDeckEnd { .. }) | None => {
                None
            }
        };
        if let Some(asked) = asked {
            self.desk.borrow_mut().gestures.push(asked);
        }
        true
    }

    fn take_close_requests(&mut self) -> Vec<u64> {
        self.desk.borrow_mut().policy.take_closed()
    }

    /// A window's own asking. A tab has no free geometry: the strip lays it
    /// out, so a move or a resize asks for nothing. Covering the display is
    /// the desktop's either way, the strip filling or the whole display, and
    /// the compositor is told the desktop took it rather than covering the
    /// window itself.
    fn request(&mut self, window: &WindowInfo, request: &WindowRequest) -> bool {
        let mut desk = self.desk.borrow_mut();
        let answered = match request {
            WindowRequest::Maximize(on) => desk.policy.set_maximized(window.id, *on),
            WindowRequest::Fullscreen { on, .. } => desk.policy.set_fullscreen(window.id, *on),
            WindowRequest::Move | WindowRequest::Resize(_) | WindowRequest::Minimize => {
                return false;
            }
        };
        if answered {
            desk.host.relayout();
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The display the test places windows on.
    fn output() -> LayoutRect {
        LayoutRect::new(LayoutPoint::from((0, 0)), (1280, 800).into())
    }

    /// A window of `app` made by the process `pid`.
    fn window(id: u64, app: &str, pid: Option<u32>) -> WindowInfo {
        WindowInfo {
            id,
            app_id: Some(app.to_owned()),
            pid,
            ..WindowInfo::default()
        }
    }

    /// The policy is told the process of each window the compositor maps, as
    /// the desktop gives it a place, so the app behind it opens its later
    /// windows there. A window without a process, or one already placed, has
    /// nothing to tell.
    #[test]
    fn a_mapped_window_tells_the_policy_its_process() {
        let (_desk, mut policy) = desktop(1, 1.0);
        let told = Rc::new(RefCell::new(Vec::new()));
        let heard = Rc::clone(&told);
        policy.set_window_mapped(Rc::new(move |pid| heard.borrow_mut().push(pid)));

        policy.map(&window(1, "so.tilo.notes", Some(4242)), &[output()]);
        assert_eq!(*told.borrow(), vec![4242], "the mapped window's process");

        policy.map(&window(1, "so.tilo.notes", Some(4242)), &[output()]);
        assert_eq!(*told.borrow(), vec![4242], "a window is placed once");

        policy.map(&window(2, "so.tilo.notes", None), &[output()]);
        assert_eq!(
            *told.borrow(),
            vec![4242],
            "a window without a process has none to tell"
        );
    }
}
