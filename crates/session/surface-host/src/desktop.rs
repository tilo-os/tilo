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

use smithay::utils::{Logical, Point, Rectangle};
use tilo_compositor::gestures::{
    GestureAction, GestureConfig, GestureTracker, SWIPE_THRESHOLD, claims,
};
use tilo_compositor::{
    Arrangement, GestureEvent, HostLayer, SurfaceHost, WindowInfo, WindowPolicy,
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
}

impl Desk {
    /// Cover `outputs`' bounding box, the surface laid out against it.
    fn cover(&mut self, outputs: &[Rectangle<i32, Logical>]) {
        let Some(bounds) = outputs.iter().copied().reduce(Rectangle::merge) else {
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
                    monitor: format!("output:{},{}", output.loc.x, output.loc.y),
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
    fn floating(&self, window: u64, size: (i32, i32)) -> Option<Point<i32, Logical>> {
        let (ox, oy) = self.host.placement().origin;
        let (_, area) = self
            .policy
            .layout()
            .floating
            .into_iter()
            .find(|(id, _)| *id == window)?;
        let at = centred(area, (f64::from(size.0), f64::from(size.1)));
        Some(Point::from((
            ox.saturating_add(round(at.x)),
            oy.saturating_add(round(at.y)),
        )))
    }

    /// Where `window`'s content is in the logical layout, as last laid out.
    fn shown(&self, window: u64) -> Option<Rectangle<i32, Logical>> {
        let (ox, oy) = self.host.placement().origin;
        self.policy
            .layout()
            .shown
            .into_iter()
            .find(|(id, _)| *id == window)
            .map(|(_, rect)| {
                Rectangle::new(
                    Point::from((
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

    /// What the touchpad gestures the desktop took asked since the last
    /// call, oldest first.
    pub fn take_gestures(&self) -> Vec<DeskGesture> {
        std::mem::take(&mut self.desk.borrow_mut().gestures)
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

    fn prepare_frame(&mut self) {
        self.desk.borrow_mut().host.prepare_frame();
    }
}

impl StripPolicy {
    /// Give `window` its tab, once, and lay the desktop out so its place
    /// is known.
    fn map(&mut self, window: &WindowInfo, outputs: &[Rectangle<i32, Logical>]) {
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
}

impl WindowPolicy for StripPolicy {
    fn initial_size(
        &mut self,
        window: &WindowInfo,
        outputs: &[Rectangle<i32, Logical>],
    ) -> Option<(i32, i32)> {
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
        outputs: &[Rectangle<i32, Logical>],
    ) -> Point<i32, Logical> {
        self.map(window, outputs);
        self.pending.retain(|id| *id != window.id);
        let desk = self.desk.borrow();
        desk.shown(window.id)
            .map(|rect| rect.loc)
            .or_else(|| desk.floating(window.id, size))
            .unwrap_or_else(|| Point::from((OFFSTAGE, 0)))
    }

    fn arrange(
        &mut self,
        windows: &[(WindowInfo, Rectangle<i32, Logical>)],
        outputs: &[Rectangle<i32, Logical>],
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
                            .unwrap_or_else(|| Point::from((OFFSTAGE, 0))),
                        size: None,
                    }
                })
                .collect(),
        )
    }

    fn withdrawn(&mut self, window: &WindowInfo) {
        self.pending.retain(|id| *id != window.id);
        self.mapped.retain(|id| *id != window.id);
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
}
