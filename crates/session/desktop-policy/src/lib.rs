//! The desktop's window policy: a compositor's windows as tabs of the
//! portable desktop surface.
//!
//! A compositor knows windows as toplevels; where they go, which has focus
//! and which the person closed is the desktop's answer. [`DesktopPolicy`]
//! gives each window a tab of the [`DesktopSurface`]'s strips
//! ([`TileContent::Foreign`], keyed by the window's id), so windows open
//! beside the focused one, group into tabs, pan with their strip, resize
//! and move between strips and workspaces through the same widgets and
//! model the browser desktop runs. The compositor hosts the surface under
//! its windows, as it hosts any surface app, and after each layout pass
//! reads [`DesktopPolicy::layout`]: the windows to show and where, and those
//! to hide (background tabs, other strips and workspaces).
//!
//! Nothing here knows Wayland: windows are plain ids, places are rects in
//! the surface's logical coordinates.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use tilo_desktop_model::{DesktopTree, IdMinter, Tab, TabId, TileContent, Workspace};
use tilo_desktop_surface::navigation::Command;
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource, seed_tree};
use ui::kurbo::Rect;
use ui::widget::TileHost;

/// The generation the policy's own commands carry.
const GENERATION: &str = "window-policy";

/// The launch id a window's tab is opened under.
const LAUNCH_PREFIX: &str = "window:";

/// What the policy learns about a window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Window {
    /// The window's identity, never reused while the compositor runs.
    pub id: u64,
    /// The app it belongs to, when the client named it.
    pub app_id: Option<String>,
    /// Its title, when the client set one.
    pub title: Option<String>,
}

/// Where the windows are as of the surface's last layout pass.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    /// Each window shown, front tab of its group on the shown strip, and the
    /// rect its content fills, in surface coordinates.
    pub shown: Vec<(u64, Rect)>,
    /// Every other window the desktop holds: a background tab, or on a
    /// strip or workspace not shown.
    pub hidden: Vec<u64>,
}

/// Windows waiting for their tab, shared with the surface's tile source.
type Arriving = Rc<RefCell<BTreeMap<u64, Window>>>;

/// The tile source behind the policy's surface: a window's tab is opened
/// from the window waiting under its launch id, and its tile is the
/// arriving placeholder, which the window's own buffer covers.
struct Windows {
    arriving: Arriving,
}

impl TileSource for Windows {
    fn tile(&mut self, _: &Workspace, _: &Tab) -> TileHost {
        TileHost::new(ui::widget::TileContent::Arriving)
    }

    fn launch(&mut self, _: &Workspace, id: TabId, app: &str) -> Option<Tab> {
        let window: u64 = app.strip_prefix(LAUNCH_PREFIX)?.parse().ok()?;
        let window = self.arriving.borrow_mut().remove(&window)?;
        let app_id = window.app_id.unwrap_or_default();
        Some(Tab {
            id,
            title: window.title.unwrap_or_else(|| app_id.clone()),
            tile: TileContent::Foreign {
                app_id,
                surface_id: window.id,
            },
            app_title: None,
        })
    }
}

/// The desktop's answer to where windows go and which has focus.
pub struct DesktopPolicy {
    handle: DesktopHandle,
    arriving: Arriving,
    /// Windows the desktop has given a tab, so a tab gone without
    /// [`Self::unmap`] is one the person closed.
    placed: BTreeSet<u64>,
}

impl DesktopPolicy {
    /// A policy over a desktop of one workspace with one empty strip, and
    /// the surface the compositor hosts under its windows.
    #[must_use]
    pub fn new(salt: u16) -> (Self, DesktopSurface) {
        let mut minter = IdMinter::new(salt);
        Self::with_tree(seed_tree(&mut minter, "Desk", Vec::new()), salt)
    }

    /// A policy over `tree`, the desktop as last kept.
    #[must_use]
    pub fn with_tree(tree: DesktopTree, salt: u16) -> (Self, DesktopSurface) {
        let arriving = Arriving::default();
        let (surface, handle) = DesktopSurface::new(
            tree,
            salt,
            Box::new(Windows {
                arriving: Rc::clone(&arriving),
            }),
        );
        let policy = Self {
            handle,
            arriving,
            placed: BTreeSet::new(),
        };
        (policy, surface)
    }

    /// The host's side of the surface, for everything else it shows.
    #[must_use]
    pub fn handle(&self) -> &DesktopHandle {
        &self.handle
    }

    /// The logical width the strips lay out against; the host calls it on
    /// every resize.
    pub fn set_viewport_width(&self, width: f64) {
        self.handle.set_viewport_width(width);
    }

    /// Give `window` a tab beside the focused window of the shown strip, on
    /// the surface's next update pass, where it takes focus.
    ///
    /// # Errors
    /// The desktop has no workspace or strip to open it in.
    pub fn map(&mut self, window: Window) -> Result<(), String> {
        let launch = Command::Launch {
            generation: GENERATION.into(),
            app: format!("{LAUNCH_PREFIX}{}", window.id),
        };
        let id = window.id;
        self.arriving.borrow_mut().insert(id, window);
        self.handle.dispatch(&launch, GENERATION).inspect_err(|_| {
            self.arriving.borrow_mut().remove(&id);
        })
    }

    /// `window` went away: its tab closes, and focus goes back to the
    /// window the person had in front before it.
    pub fn unmap(&mut self, window: u64) {
        self.arriving.borrow_mut().remove(&window);
        self.placed.remove(&window);
        let mut tree = self.handle.read(Clone::clone);
        let Some(tab) = tab_of(&tree, window) else {
            return;
        };
        if tree.close_tab(tab).is_some() {
            self.handle.replace_tree(tree);
        }
    }

    /// A window's title changed.
    pub fn retitle(&mut self, window: u64, title: &str) {
        let mut tree = self.handle.read(Clone::clone);
        let Some(tab) = tab_mut(&mut tree, window) else {
            return;
        };
        if tab.title != title {
            title.clone_into(&mut tab.title);
            self.handle.replace_tree(tree);
        }
    }

    /// The window in front of the focused group of the shown strip.
    #[must_use]
    pub fn focused(&self) -> Option<u64> {
        self.handle.read(|tree| {
            let route = tilo_desktop_surface::navigation::active_route(tree)?;
            window_of(tree, route.tab)
        })
    }

    /// Bring `window` forward and focus it, on the next update pass, as
    /// clicking it does. False when the desktop holds no such window.
    pub fn focus(&mut self, window: u64) -> bool {
        self.handle
            .read(|tree| tab_of(tree, window))
            .is_some_and(|tab| self.handle.focus_tab(tab))
    }

    /// Where the windows are as of the surface's last layout pass.
    #[must_use]
    pub fn layout(&self) -> Layout {
        let shown: Vec<(u64, Rect)> = self.handle.read(|tree| {
            self.handle
                .shown_tabs()
                .into_iter()
                .filter_map(|(tab, rect)| Some((window_of(tree, tab)?, rect)))
                .collect()
        });
        let hidden = self
            .handle
            .read(windows)
            .into_iter()
            .filter(|window| !shown.iter().any(|(id, _)| id == window))
            .collect();
        Layout { shown, hidden }
    }

    /// The windows whose tabs the person closed since the last call: the
    /// compositor asks each to close. A window stays placed until it is
    /// unmapped, so one that refuses to close keeps its place in the
    /// compositor until it goes.
    pub fn take_closed(&mut self) -> Vec<u64> {
        let present: BTreeSet<u64> = self.handle.read(windows).into_iter().collect();
        let closed: Vec<u64> = self.placed.difference(&present).copied().collect();
        for window in &closed {
            self.placed.remove(window);
        }
        self.placed.extend(present);
        closed
    }
}

/// Every window the tree holds a tab of, in tree order.
fn windows(tree: &DesktopTree) -> Vec<u64> {
    tabs(tree).filter_map(foreign).collect()
}

fn tabs(tree: &DesktopTree) -> impl Iterator<Item = &Tab> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
}

fn foreign(tab: &Tab) -> Option<u64> {
    match tab.tile {
        TileContent::Foreign { surface_id, .. } => Some(surface_id),
        _ => None,
    }
}

fn window_of(tree: &DesktopTree, tab: TabId) -> Option<u64> {
    tabs(tree).find(|t| t.id == tab).and_then(foreign)
}

fn tab_of(tree: &DesktopTree, window: u64) -> Option<TabId> {
    tabs(tree)
        .find(|tab| foreign(tab) == Some(window))
        .map(|tab| tab.id)
}

fn tab_mut(tree: &mut DesktopTree, window: u64) -> Option<&mut Tab> {
    tree.workspaces
        .iter_mut()
        .flat_map(|workspace| &mut workspace.strips)
        .flat_map(|strip| &mut strip.window_groups)
        .flat_map(|group| &mut group.tabs)
        .find(|tab| foreign(tab) == Some(window))
}
