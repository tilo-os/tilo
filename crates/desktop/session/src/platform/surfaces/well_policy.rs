//! The window policy of a tilolang shell: windows go where the shell's wells stand
//! (`docs/designs/app-language/shell.md` §8).
//!
//! A tilolang shell's views never draw a place's content: each frame puts a
//! [`ui::widget::WindowWell`] where the content goes, and the well reports its rectangle, keyed
//! by the place, to the [`Wells`] handle the surface host shares with this policy, over the
//! session's [`ShellDesktop`]. The policy answers a window's initial
//! size, its place and the placement of every window from that: a window maps into the
//! desktop's tab model (giving it a tab id), and its well is the one keyed by that tab id.
//!
//! A well that moves without changing size moves its window in the same frame: the compositor
//! asks for a placement after every frame the host drew, so a canvas pan or a strip scroll
//! places the window at the well before the frame is shown. A well that changed size configures
//! the client, which catches up on its next commit. A place with no well on screen (scrolled
//! away, or on a workspace the shell does not draw) leaves its window off the output, so it
//! gets no frame callbacks.
//!
//! The tabs live in the session's desktop ([`ShellDesktop::well_policy`]), so a place in the
//! person's placement names the window's tab there.

use std::collections::HashMap;
use std::rc::Rc;

use tilo_compositor::policy::{LayoutPoint, LayoutRect};
use tilo_compositor::{GestureEvent, WindowInfo, WindowPlacement, WindowPolicy};

use ui::widget::Wells;
use ui::widget::arrange::rem_of;

use super::ShellDesktop;
use super::gestures::GestureReader;

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

/// The window policy answered by a tilolang shell's wells, over the desktop they place windows of.
pub struct WellPolicy {
    desktop: ShellDesktop,
    wells: Wells,
    /// The windows given a place, so one gone from the compositor closes its tab.
    mapped: Vec<u64>,
    /// The touchpad gestures the desktop took.
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

impl WellPolicy {
    /// The policy placing the windows of `desktop` where the shell's wells stand.
    #[must_use]
    pub(super) fn over(desktop: ShellDesktop) -> Self {
        Self {
            desktop,
            wells: Wells::new(),
            mapped: Vec::new(),
            gestures: GestureReader::default(),
            pending: Vec::new(),
            configured: HashMap::new(),
            followers: WindowFollowers::default(),
        }
    }

    /// Tell `observer` the process of each window the compositor maps, as the desktop gives
    /// it a place: the app's API opens to it from then on.
    pub fn set_window_mapped(&mut self, observer: Rc<dyn Fn(u32)>) {
        self.followers.map_observer = Some(observer);
    }

    /// Follow the windows the compositor opens and closes with `observer`.
    pub fn observe_windows(&mut self, observer: std::sync::Arc<dyn WindowObserver>) {
        self.followers.observer = Some(observer);
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
        if let Some(size) = self
            .well_for(window, tab)
            .filter(|well| well.width() >= 1.0 && well.height() >= 1.0)
        {
            return (extent(size.width()), extent(size.height()));
        }
        initial_extent(window, outputs)
    }

    /// Where the session stands the card a window draws: the app draws it itself, so it takes no
    /// well of the shell's, and the session says where its window goes.
    fn card_place(&self, window: &WindowInfo) -> Option<ui::kurbo::Rect> {
        let policy = self.desktop.policy();
        policy
            .draws_card(window.id)
            .then(|| policy.card_place(window.id))
            .flatten()
    }

    /// The size the app asked for its card, when the window draws one.
    fn card_size(&self, window: &WindowInfo) -> Option<(i32, i32)> {
        let policy = self.desktop.policy();
        policy
            .draws_card(window.id)
            .then(|| policy.card_size(window.id))
            .flatten()
    }

    /// The well of `window`'s tab `key`: the one the part drawing `window`'s display laid out,
    /// so a strip two displays' parts both drew places the window on its own display. A window
    /// on a desktop of one display, or one whose display is not yet known, reads any part's.
    fn well_for(&self, window: &WindowInfo, tab: Option<&str>) -> Option<ui::kurbo::Rect> {
        let key = tab?;
        self.desktop
            .policy()
            .display_of(window.id)
            .and_then(|display| self.wells.well_on(&display.to_string(), key))
            .or_else(|| self.wells.well(key))
    }

    /// The room the well of tab `key` leaves a window that opened with no well (the frame has
    /// not laid out): off the output.
    fn place_of(&self, window: &WindowInfo, tab: Option<&str>) -> Option<LayoutPoint> {
        let well = self.well_for(window, tab)?;
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
        let mapped = self.desktop.policy().map(tilo_desktop_policy::Window {
            id: window.id,
            app_id: window.app_id.clone(),
            title: window.title.clone(),
            parent: window.parent,
            min_size: window.min_size,
            max_size: window.max_size,
            pid: window.pid,
            tag: window.tag.clone(),
        });
        if let Err(error) = mapped {
            tracing::warn!(%error, window = window.id, "the shell has no place for a window");
            return;
        }
        self.mapped.push(window.id);
        self.desktop.ask();
        // A window launched into a workspace not in front goes there once its tab is made.
        self.desktop.policy().carry_launched();
        self.followers.mapped(window);
    }

    /// The tab the desktop gave `window`, as its well names it.
    fn tab(&mut self, window: &WindowInfo) -> Option<String> {
        self.map(window);
        self.desktop
            .policy()
            .tab_of(window.id)
            .map(|tab| tab.to_string())
    }

    /// The key of the place `window`'s well names it by: the tab id the desktop gave it when it
    /// mapped (shell.md §8). `None` until it mapped, or for a dialog, which floats over the
    /// window it belongs to and takes no place.
    pub fn well_key(&mut self, window: u64) -> Option<String> {
        self.desktop
            .policy()
            .tab_of(window)
            .map(|tab| tab.to_string())
    }
}

impl WindowPolicy for WellPolicy {
    fn monitors(&mut self, monitors: &[(LayoutRect, String)], primary: Option<&str>) {
        self.desktop.monitors(monitors, primary);
    }

    fn initial_size(&mut self, window: &WindowInfo, outputs: &[LayoutRect]) -> Option<(i32, i32)> {
        self.desktop.cover(outputs);
        let tab = self.tab(window);
        if !self.pending.contains(&window.id) {
            self.pending.push(window.id);
        }
        if let Some(size) = self.card_size(window) {
            return Some(size);
        }
        Some(self.well_or_initial_size(tab.as_deref(), window, outputs))
    }

    fn place(
        &mut self,
        window: &WindowInfo,
        _size: (i32, i32),
        outputs: &[LayoutRect],
    ) -> LayoutPoint {
        self.desktop.cover(outputs);
        let tab = self.tab(window);
        self.pending.retain(|id| *id != window.id);
        if let Some(at) = self.card_place(window) {
            return LayoutPoint::from((round(at.x0), round(at.y0)));
        }
        self.place_of(window, tab.as_deref())
            .unwrap_or_else(|| LayoutPoint::from((OFFSTAGE, 0)))
    }

    fn arrange(
        &mut self,
        windows: &[(WindowInfo, LayoutRect)],
        outputs: &[LayoutRect],
    ) -> Option<Vec<WindowPlacement>> {
        self.desktop.cover(outputs);
        // A launched window whose tab came with the last layout goes to its own workspace.
        self.desktop.policy().carry_launched();
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
                self.desktop.policy().unmap(*id);
                self.followers.gone(*id);
            }
            self.desktop.ask();
        }
        self.mapped.retain(|id| !gone.contains(id));
        self.configured.retain(|id, _| !gone.contains(id));
        let mut placement = Vec::with_capacity(windows.len());
        for (window, current) in windows {
            if let Some(title) = &window.title {
                self.desktop.policy().retitle(window.id, title);
            }
            if let Some(app_id) = &window.app_id {
                self.desktop.policy().set_app_id(window.id, app_id);
            }
            let tab = self.tab(window);
            if let Some(at) = self.card_place(window) {
                // A card the app draws itself takes no well of the shell's: the session says
                // where its window stands, and the person moving the picture moves the window.
                placement.push(WindowPlacement {
                    id: window.id,
                    location: LayoutPoint::from((round(at.x0), round(at.y0))),
                    size: Some((extent(at.width()), extent(at.height()))),
                    clip: None,
                    fullscreen: None,
                });
                continue;
            }
            let Some(well) = self.well_for(window, tab.as_deref()) else {
                // No well on screen: the window waits off it, with the size it has.
                placement.push(WindowPlacement {
                    id: window.id,
                    location: LayoutPoint::from((OFFSTAGE, 0)),
                    size: None,
                    clip: None,
                    fullscreen: None,
                });
                continue;
            };
            if well.width() < 1.0 || well.height() < 1.0 {
                placement.push(WindowPlacement {
                    id: window.id,
                    location: LayoutPoint::from((OFFSTAGE, 0)),
                    size: None,
                    clip: None,
                    fullscreen: None,
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
            placement.push(WindowPlacement {
                id: window.id,
                location: LayoutPoint::from((round(well.x0), round(well.y0))),
                size: reconfigure.then_some(size),
                clip: None,
                fullscreen: None,
            });
        }
        Some(placement)
    }

    fn withdrawn(&mut self, window: &WindowInfo) {
        self.pending.retain(|id| *id != window.id);
        self.mapped.retain(|id| *id != window.id);
        self.configured.remove(&window.id);
        self.followers.gone(window.id);
        self.desktop.policy().unmap(window.id);
        self.desktop.ask();
    }

    fn focus_on_click(&mut self, window: &WindowInfo) -> bool {
        let focused = self.desktop.policy().focus(window.id);
        if focused {
            self.desktop.ask();
        }
        true
    }

    fn gesture(&mut self, gesture: &GestureEvent) -> bool {
        let desktop = &self.desktop;
        self.gestures
            .take(gesture, desktop.gesture_config(), |asked| {
                desktop.took_gesture(asked);
            })
    }

    fn focused(&mut self) -> Option<u64> {
        self.desktop.policy().focused()
    }

    fn activate(&mut self, window: &WindowInfo, fresh: bool) -> bool {
        let changed = if fresh {
            self.desktop.policy().focus(window.id)
        } else {
            self.desktop.policy().ask_attention(window.id)
        };
        if changed {
            self.desktop.ask();
        }
        fresh
    }

    fn take_close_requests(&mut self) -> Vec<u64> {
        self.desktop.policy().take_closed()
    }
}

/// Told about the windows a program opens and closes, by the policy: what
/// finds the program behind a window that named itself.
pub trait WindowObserver: Send + Sync {
    /// `window` named itself and is about to map.
    fn opened(&self, window: &WindowInfo);
    /// The window `id` is gone.
    fn closed(&self, id: u64);
}

/// Who the policy tells of the windows the desktop gives a place, and of the ones gone.
#[derive(Default)]
struct WindowFollowers {
    /// Told the process of each window the compositor maps, the moment the
    /// desktop gives it a place.
    map_observer: Option<Rc<dyn Fn(u32)>>,
    /// Who follows the windows, when the session has someone that does.
    observer: Option<std::sync::Arc<dyn WindowObserver>>,
    /// The windows it was told about, so none is told of twice.
    announced: std::collections::HashSet<u64>,
}

impl WindowFollowers {
    /// `window` was given its place: tell its process's observer, and the
    /// window's observer once.
    fn mapped(&mut self, window: &WindowInfo) {
        if let (Some(observer), Some(pid)) = (self.map_observer.as_ref(), window.pid) {
            observer(pid);
        }
        let Some(observer) = self.observer.clone() else {
            return;
        };
        if !self.announced.insert(window.id) {
            return;
        }
        observer.opened(window);
    }

    /// Tell the observer that `id` is gone, once.
    fn gone(&mut self, id: u64) {
        if !self.announced.remove(&id) {
            return;
        }
        if let Some(observer) = &self.observer {
            observer.closed(id);
        }
    }
}
