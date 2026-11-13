//! The window policy of a tilolang shell: windows go where the shell's wells stand
//! (`docs/designs/app-language/shell.md` §8).
//!
//! A tilolang shell's views never draw a place's content: each frame puts a
//! [`ui::widget::WindowWell`] where the content goes, and the well reports its rectangle, keyed
//! by the place, to the [`Wells`] handle the surface host shares with this policy — as
//! [`DesktopHost`](crate::DesktopHost) and
//! [`StripPolicy`](crate::StripPolicy) share their desk. The policy answers a window's initial
//! size, its place and the arrangement of every window from that: a window maps into the
//! desktop's tab model (giving it a tab id), and its well is the one keyed by that tab id.
//!
//! A well that moves without changing size moves its window in the same frame: the compositor
//! asks for an arrangement after every frame the host drew, so a canvas pan or a strip scroll
//! places the window at the well before the frame is shown. A well that changed size configures
//! the client, which catches up on its next commit. A place with no well on screen (scrolled
//! away, or on a workspace the shell does not draw) leaves its window off the output, so it
//! gets no frame callbacks.
//!
//! The desktop model the tabs live in is the policy's own ([`well_policy`]), or the session's
//! ([`crate::DesktopHost::well_policy`]), so a place in the person's arrangement names the
//! window's tab there.

use std::cell::{RefCell, RefMut};
use std::collections::HashMap;
use std::rc::Rc;

use tilo_compositor::gestures::GestureConfig;
use tilo_compositor::policy::{LayoutPoint, LayoutRect};
use tilo_compositor::{Arrangement, GestureEvent, WindowInfo, WindowPolicy};

use tilo_desktop_policy::DesktopPolicy;
use ui::widget::Wells;
use ui::widget::arrange::rem_of;

use crate::desktop::{DeskGesture, GestureReader, WindowFollowers, WindowObserver};
use crate::widget::WidgetHost;

/// Where a window not shown waits: left of every output, out of sight and out of the pointer's
/// reach.
const OFFSTAGE: i32 = -(1 << 20);

/// A window's smallest width, in `rem`: an app's compact layout, one screen at a time
/// (`ui::tokens::Metrics::window_min_width` at the stock text size).
const MIN_WIDTH_REM: f64 = 20.0;

/// A window's smallest height, in `rem`: a bar over a couple of lines of content.
const MIN_HEIGHT_REM: f64 = 12.0;

/// A window whose well it has no size for yet takes this fraction of the output's width, when
/// its client named no size of its own.
const INITIAL_WIDTH_FRACTION: f64 = 0.5;
/// And of the output's height.
const INITIAL_HEIGHT_FRACTION: f64 = 0.6;

/// The desktop both halves share: its window model and the tree it keeps.
struct Desk {
    policy: DesktopPolicy,
    host: WidgetHost,
}

/// The desktop model a [`WellPolicy`] keeps its windows' tabs in: the tab a window maps into
/// is the key its well is reported by.
pub trait WellDesk {
    /// The desktop's window model.
    fn policy(&self) -> RefMut<'_, DesktopPolicy>;
    /// Lay the desktop's tree out again after its model changed.
    fn relayout(&self);
    /// The outputs the windows stand on, as the compositor knows them now.
    fn cover(&self, _outputs: &[LayoutRect]) {}
    /// How the desk reads touchpad gestures: `None` leaves every one to the
    /// clients.
    fn gesture_config(&self) -> Option<GestureConfig> {
        None
    }
    /// The desk took a gesture, and it asked `gesture` of it: it is the same
    /// desk the strips' policy takes them on, so they reach its chrome the
    /// same way.
    fn took_gesture(&self, _gesture: DeskGesture) {}
}

/// The policy's own desktop ([`well_policy`]).
struct OwnDesk(Rc<RefCell<Desk>>);

impl WellDesk for OwnDesk {
    fn policy(&self) -> RefMut<'_, DesktopPolicy> {
        RefMut::map(self.0.borrow_mut(), |desk| &mut desk.policy)
    }

    fn relayout(&self) {
        self.0.borrow_mut().host.relayout();
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

/// `value` rounded to a whole logical pixel count, at least one.
fn extent(value: f64) -> i32 {
    round(value).max(1)
}

/// The window policy answered by a tilolang shell's wells, and the host side of the desktop
/// they place windows of.
pub struct WellPolicy {
    desk: Rc<dyn WellDesk>,
    wells: Wells,
    /// The windows given a place, so one gone from the compositor closes its tab.
    mapped: Vec<u64>,
    /// The touchpad gestures the desk took.
    gestures: GestureReader,
    /// Those of them that have not shown a buffer yet: they wait in their well, which the
    /// compositor does not list among its windows.
    pending: Vec<u64>,
    /// The size each window was last configured to, by its id: what the policy compares a
    /// well against. A client is configured when the room its well gives it changes, not
    /// when what it last committed differs from that room: a terminal rounds what it commits
    /// to whole cells, so its commit never equals the constraint it was given, and neither a
    /// click nor a frame between would ever stop configuring it.
    configured: HashMap<u64, (i32, i32)>,
    /// Who is told of the windows given a place and of the ones gone.
    followers: WindowFollowers,
}

/// The desk a tilolang shell places its windows on, and the policy that answers where they go
/// with `wells` the shell's views report their wells to. The surface host of the shell's views
/// installs the same handle before it lays them out (`ui::widget::window_well::install`).
#[must_use]
pub fn well_policy(salt: u16, scale: f64) -> (WellPolicy, DeskHost) {
    let (policy, surface) = DesktopPolicy::new(salt);
    let placement = crate::Placement {
        origin: (0, 0),
        size: (1, 1),
        scale,
    };
    let host = WidgetHost::new(Box::new(surface), placement, false);
    let desk = Rc::new(RefCell::new(Desk { policy, host }));
    (
        WellPolicy::over(Rc::new(OwnDesk(Rc::clone(&desk)))),
        DeskHost { desk },
    )
}

impl WellPolicy {
    /// The policy placing the windows of `desk` where the shell's wells stand.
    #[must_use]
    pub fn over(desk: Rc<dyn WellDesk>) -> Self {
        Self {
            desk,
            wells: Wells::new(),
            mapped: Vec::new(),
            gestures: GestureReader::default(),
            pending: Vec::new(),
            configured: HashMap::new(),
            followers: WindowFollowers::default(),
        }
    }

    /// Tell `observer` the process of each window the compositor maps, as the desktop gives
    /// it a place: the app's door opens to it from then on.
    pub fn set_window_mapped(&mut self, observer: Rc<dyn Fn(u32)>) {
        self.followers.set_window_mapped(observer);
    }

    /// Follow the windows the compositor opens and closes with `observer`.
    pub fn observe_windows(&mut self, observer: std::sync::Arc<dyn WindowObserver>) {
        self.followers.observe_windows(observer);
    }

    /// The wells the shell's views report to: the handle the surface host of those views
    /// installs while it lays them out.
    #[must_use]
    pub fn wells(&self) -> Wells {
        self.wells.clone()
    }

    /// How wide the window that mapped as tab `key` is asked to be, given the client's own
    /// request and the outputs it opens on. A well gives the window the room its frame left it;
    /// before a well exists (the frame has not laid out yet) the client's size, or a share of
    /// the output, stands in for it.
    fn well_or_initial_size(
        &self,
        tab: Option<&str>,
        window: &WindowInfo,
        outputs: &[LayoutRect],
    ) -> (i32, i32) {
        if let Some(size) = tab
            .and_then(|key| self.wells.well(key))
            .filter(|well| well.width() >= 1.0 && well.height() >= 1.0)
        {
            return (extent(size.width()), extent(size.height()));
        }
        initial_extent(window, outputs)
    }

    /// The room the well of tab `key` leaves a window that opened with no well (the frame has
    /// not laid out): off the output.
    fn place_of(&self, tab: Option<&str>) -> Option<LayoutPoint> {
        let well = tab.and_then(|key| self.wells.well(key))?;
        if well.width() < 1.0 || well.height() < 1.0 {
            return None;
        }
        Some(LayoutPoint::from((round(well.x0), round(well.y0))))
    }
}

/// The size a window opens at when its well has none: what the client asked for, or a share of
/// the output, never under its smallest.
fn initial_extent(window: &WindowInfo, outputs: &[LayoutRect]) -> (i32, i32) {
    let (width, height) = outputs
        .first()
        .map_or((1280, 800), |output| (output.size.w, output.size.h));
    let share = |whole: i32, fraction: f64, least: f64| {
        let share = round(f64::from(whole) * fraction);
        extent(f64::from(share).max(least))
    };
    // The stock text size the toolkit draws with, until a tree is laid out.
    let rem = rem_of(&ui::Theme::dark());
    let min_width = f64::from(rem) * MIN_WIDTH_REM;
    let min_height = f64::from(rem) * MIN_HEIGHT_REM;
    let mut size = (
        share(width, INITIAL_WIDTH_FRACTION, min_width),
        share(height, INITIAL_HEIGHT_FRACTION, min_height),
    );
    // The client's own request, within its bounds, when it named one.
    if let Some((min_w, min_h)) = window.min_size {
        size.0 = size.0.max(min_w);
        size.1 = size.1.max(min_h);
    }
    if let Some((max_w, max_h)) = window.max_size {
        size.0 = size.0.min(max_w);
        size.1 = size.1.min(max_h);
    }
    size
}

impl WellPolicy {
    /// Give `window` its tab, once, and keep the desktop's model in step with it.
    fn map(&mut self, window: &WindowInfo) {
        if self.mapped.contains(&window.id) {
            return;
        }
        let mapped = self.desk.policy().map(tilo_desktop_policy::Window {
            id: window.id,
            app_id: window.app_id.clone(),
            title: window.title.clone(),
            parent: window.parent,
            min_size: window.min_size,
            max_size: window.max_size,
            pid: window.pid,
        });
        if let Err(error) = mapped {
            tracing::warn!(%error, window = window.id, "the shell has no place for a window");
            return;
        }
        self.mapped.push(window.id);
        self.desk.relayout();
        // A window launched into a workspace not in front goes there once its tab is made.
        self.desk.policy().carry_launched();
        self.followers.mapped(window);
    }

    /// The tab the desktop gave `window`, as its well names it.
    fn tab(&mut self, window: &WindowInfo) -> Option<String> {
        self.map(window);
        self.desk
            .policy()
            .tab_of(window.id)
            .map(|tab| tab.to_string())
    }

    /// The key of the place `window`'s well names it by: the tab id the desktop gave it when it
    /// mapped (shell.md §8). `None` until it mapped, or for a dialog, which floats over the
    /// window it belongs to and takes no place.
    pub fn well_key(&mut self, window: u64) -> Option<String> {
        self.desk.policy().tab_of(window).map(|tab| tab.to_string())
    }
}

impl WindowPolicy for WellPolicy {
    fn initial_size(&mut self, window: &WindowInfo, outputs: &[LayoutRect]) -> Option<(i32, i32)> {
        self.desk.cover(outputs);
        let tab = self.tab(window);
        if !self.pending.contains(&window.id) {
            self.pending.push(window.id);
        }
        Some(self.well_or_initial_size(tab.as_deref(), window, outputs))
    }

    fn place(
        &mut self,
        window: &WindowInfo,
        _size: (i32, i32),
        outputs: &[LayoutRect],
    ) -> LayoutPoint {
        self.desk.cover(outputs);
        let tab = self.tab(window);
        self.pending.retain(|id| *id != window.id);
        self.place_of(tab.as_deref())
            .unwrap_or_else(|| LayoutPoint::from((OFFSTAGE, 0)))
    }

    fn arrange(
        &mut self,
        windows: &[(WindowInfo, LayoutRect)],
        outputs: &[LayoutRect],
    ) -> Option<Vec<Arrangement>> {
        self.desk.cover(outputs);
        // A launched window whose tab came with the last layout goes to its own workspace.
        self.desk.policy().carry_launched();
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
        if !gone.is_empty() {
            for id in &gone {
                self.desk.policy().unmap(*id);
                self.followers.gone(*id);
            }
            self.desk.relayout();
        }
        self.mapped.retain(|id| !gone.contains(id));
        self.configured.retain(|id, _| !gone.contains(id));
        let mut arrangements = Vec::with_capacity(windows.len());
        for (window, current) in windows {
            if let Some(title) = &window.title {
                self.desk.policy().retitle(window.id, title);
            }
            if let Some(app_id) = &window.app_id {
                self.desk.policy().set_app_id(window.id, app_id);
            }
            let tab = self.tab(window);
            let Some(well) = tab.as_deref().and_then(|key| self.wells.well(key)) else {
                // No well on screen: the window waits off it, with the size it has.
                arrangements.push(Arrangement {
                    id: window.id,
                    location: LayoutPoint::from((OFFSTAGE, 0)),
                    size: None,
                });
                continue;
            };
            if well.width() < 1.0 || well.height() < 1.0 {
                arrangements.push(Arrangement {
                    id: window.id,
                    location: LayoutPoint::from((OFFSTAGE, 0)),
                    size: None,
                });
                continue;
            }
            let size = (extent(well.width()), extent(well.height()));
            // The client is configured when the room its well gives it changed. What it was
            // configured to is the policy's own record once it has sent one; before that it is
            // what the compositor lists the window at. A client commits a size it chose within
            // what it was told (a terminal rounds to whole cells), so comparing the room
            // against that commit would configure the client again on every frame.
            let known = self
                .configured
                .get(&window.id)
                .copied()
                .unwrap_or((current.size.w, current.size.h));
            let reconfigure = known != size;
            if reconfigure {
                self.configured.insert(window.id, size);
            }
            arrangements.push(Arrangement {
                id: window.id,
                location: LayoutPoint::from((round(well.x0), round(well.y0))),
                size: reconfigure.then_some(size),
            });
        }
        Some(arrangements)
    }

    fn withdrawn(&mut self, window: &WindowInfo) {
        self.pending.retain(|id| *id != window.id);
        self.mapped.retain(|id| *id != window.id);
        self.configured.remove(&window.id);
        self.followers.gone(window.id);
        self.desk.policy().unmap(window.id);
        self.desk.relayout();
    }

    fn focus_on_click(&mut self, window: &WindowInfo) -> bool {
        let focused = self.desk.policy().focus(window.id);
        if focused {
            self.desk.relayout();
        }
        true
    }

    fn gesture(&mut self, gesture: &GestureEvent) -> bool {
        let desk = &self.desk;
        self.gestures.take(gesture, desk.gesture_config(), |asked| {
            desk.took_gesture(asked);
        })
    }

    fn focused(&mut self) -> Option<u64> {
        self.desk.policy().focused()
    }

    fn activate(&mut self, window: &WindowInfo, fresh: bool) -> bool {
        let changed = if fresh {
            self.desk.policy().focus(window.id)
        } else {
            self.desk.policy().ask_attention(window.id)
        };
        if changed {
            self.desk.relayout();
        }
        fresh
    }

    fn take_close_requests(&mut self) -> Vec<u64> {
        self.desk.policy().take_closed()
    }
}

/// The host side of the desk a [`WellPolicy`] places windows on: its widget tree, and what the
/// session drives on it (strips and workspaces, the tree it keeps).
#[derive(Clone)]
pub struct DeskHost {
    desk: Rc<RefCell<Desk>>,
}

impl DeskHost {
    /// The desk's policy, for everything else a session drives on it.
    pub fn with_policy<R>(&self, read: impl FnOnce(&mut DesktopPolicy) -> R) -> R {
        read(&mut self.desk.borrow_mut().policy)
    }

    /// The desk's widget tree as hosted: what it publishes, its accessibility tree, the
    /// actions it accepts.
    pub fn with_widgets<R>(&self, read: impl FnOnce(&mut WidgetHost) -> R) -> R {
        read(&mut self.desk.borrow_mut().host)
    }
}
