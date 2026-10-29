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
use tilo_compositor::{Arrangement, HostLayer, SurfaceHost, WindowInfo, WindowPolicy};
use tilo_desktop_policy::{DesktopPolicy, Window};

use crate::{Placement, WidgetHost};

/// Where a window not shown waits: left of every output, out of sight and
/// out of the pointer's reach.
const OFFSTAGE: i32 = -(1 << 20);

/// The desktop both halves share.
struct Desk {
    policy: DesktopPolicy,
    host: WidgetHost,
}

impl Desk {
    /// Cover `outputs`' bounding box, the surface laid out against it.
    fn cover(&mut self, outputs: &[Rectangle<i32, Logical>]) {
        let Some(bounds) = outputs.iter().copied().reduce(|a, b| a.merge(b)) else {
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
        let displays: Vec<ui::kurbo::Rect> = outputs
            .iter()
            .map(|output| {
                let x = f64::from(output.loc.x - bounds.loc.x);
                let y = f64::from(output.loc.y - bounds.loc.y);
                ui::kurbo::Rect::new(
                    x,
                    y,
                    x + f64::from(output.size.w),
                    y + f64::from(output.size.h),
                )
            })
            .collect();
        self.policy.set_displays(&displays);
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
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "a window's place on a screen"
                )]
                let round = |value: f64| value.round() as i32;
                Rectangle::new(
                    Point::from((ox + round(rect.x0), oy + round(rect.y0))),
                    (round(rect.width()).max(1), round(rect.height()).max(1)).into(),
                )
            })
    }
}

/// The desktop drawn under every window.
pub struct DesktopHost {
    desk: Rc<RefCell<Desk>>,
}

/// The window policy answered by the desktop's strips.
pub struct StripPolicy {
    desk: Rc<RefCell<Desk>>,
    /// The windows given a tab, so one gone from the compositor closes
    /// its tab.
    mapped: Vec<u64>,
}

/// A desktop of one workspace with one empty strip (`salt` seeds its ids),
/// drawn at `scale`: the surface under the windows and the policy placing
/// them. The compositor hosts the one and runs with the other.
#[must_use]
pub fn desktop(salt: u16, scale: f64) -> (DesktopHost, StripPolicy) {
    let (policy, surface) = DesktopPolicy::new(salt);
    let placement = Placement {
        origin: (0, 0),
        size: (1, 1),
        scale,
    };
    let desk = Rc::new(RefCell::new(Desk {
        policy,
        host: WidgetHost::new(Box::new(surface), placement, true),
    }));
    (
        DesktopHost {
            desk: Rc::clone(&desk),
        },
        StripPolicy {
            desk,
            mapped: Vec::new(),
        },
    )
}

impl DesktopHost {
    /// The desktop's policy, for everything else a session drives on it
    /// (strips and workspaces, the tree it keeps).
    pub fn with_policy<R>(&self, read: impl FnOnce(&mut DesktopPolicy) -> R) -> R {
        read(&mut self.desk.borrow_mut().policy)
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
        let rect = self.desk.borrow().shown(window.id)?;
        Some((rect.size.w, rect.size.h))
    }

    fn place(
        &mut self,
        window: &WindowInfo,
        _size: (i32, i32),
        outputs: &[Rectangle<i32, Logical>],
    ) -> Point<i32, Logical> {
        self.map(window, outputs);
        self.desk
            .borrow()
            .shown(window.id)
            .map_or(Point::from((OFFSTAGE, 0)), |rect| rect.loc)
    }

    fn arrange(
        &mut self,
        windows: &[(WindowInfo, Rectangle<i32, Logical>)],
        outputs: &[Rectangle<i32, Logical>],
    ) -> Option<Vec<Arrangement>> {
        let gone: Vec<u64> = self
            .mapped
            .iter()
            .copied()
            .filter(|id| !windows.iter().any(|(window, _)| window.id == *id))
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
                .map(|(window, _)| match desk.shown(window.id) {
                    Some(rect) => Arrangement {
                        id: window.id,
                        location: rect.loc,
                        size: Some((rect.size.w, rect.size.h)),
                    },
                    None => Arrangement {
                        id: window.id,
                        location: Point::from((OFFSTAGE, 0)),
                        size: None,
                    },
                })
                .collect(),
        )
    }

    fn focus_on_click(&mut self, window: &WindowInfo) -> bool {
        self.desk.borrow_mut().policy.focus(window.id);
        true
    }

    fn focused(&mut self) -> Option<u64> {
        self.desk.borrow().policy.focused()
    }

    fn take_close_requests(&mut self) -> Vec<u64> {
        self.desk.borrow_mut().policy.take_closed()
    }
}
