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
//! Dialogs float over the window they belong to instead of taking a tab
//! ([`floating`]), and across several displays each display holds its own
//! windows ([`DesktopPolicy::set_displays`]).
//!
//! Nothing here knows Wayland: windows are plain ids, places are rects in
//! the surface's logical coordinates.

pub mod displays;
pub mod floating;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use tilo_desktop_model::{
    DesktopTree, Display, GroupId, IdMinter, Tab, TabId, TileContent, Workspace,
};
use tilo_desktop_surface::crashed_app::Closed;
use tilo_desktop_surface::navigation::{Command, Route, RouteIds, active_route};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource, seed_tree};
use ui::kurbo::Rect;
use ui::widget::TileHost;

/// The generation the policy's own commands carry.
const GENERATION: &str = "window-policy";

/// The launch id a window's tab is opened under.
const LAUNCH_PREFIX: &str = "window:";

/// The launch id a crashed app's notice tab is opened under.
const CRASHED_PREFIX: &str = "crashed:";

/// The launch id a tab held for an app's next window is opened under, in
/// front and behind.
const HELD_PREFIX: &str = "held:";
const HELD_BEHIND_PREFIX: &str = "held-behind:";

/// What the policy learns about a window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Window {
    /// The window's identity, never reused while the compositor runs.
    pub id: u64,
    /// The app it belongs to, when the client named it.
    pub app_id: Option<String>,
    /// Its title, when the client set one.
    pub title: Option<String>,
    /// The window it is a dialog of, when the client named one.
    pub parent: Option<u64>,
    /// The smallest size the client accepts, when it set one.
    pub min_size: Option<(i32, i32)>,
    /// The largest size the client accepts, when it set one.
    pub max_size: Option<(i32, i32)>,
}

/// Where the windows are as of the surface's last layout pass.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    /// Each window shown, front tab of its group on the shown strip, and the
    /// rect its content fills, in surface coordinates.
    pub shown: Vec<(u64, Rect)>,
    /// Every other window the desktop holds: a background tab, on a strip
    /// or workspace not shown, or a dialog of a window not shown.
    pub hidden: Vec<u64>,
    /// Each dialog shown, and the area it floats centred over: its
    /// parent's, or the display the person works on for one without.
    pub floating: Vec<(u64, Rect)>,
    /// The window covering the display the person works on, if one is: it
    /// fills that display's whole area, the bar's room included, and the
    /// bar steps aside while it does.
    pub fullscreen: Option<u64>,
}

/// Windows waiting for their tab, shared with the surface's tile source.
type Arriving = Rc<RefCell<BTreeMap<u64, Window>>>;

/// The launches the desktop asked for and no window is behind: the session
/// starts each one ([`DesktopPolicy::take_launches`]).
type Launches = Rc<RefCell<Vec<String>>>;

/// The app each notice tab is about, by tab: the launch its Reopen asks for
/// again. What the tile source draws in place of a window that is not
/// coming.
type Crashed = Rc<RefCell<BTreeMap<TabId, String>>>;

/// Why each app that left a notice is not running, by the app: a crash, or
/// the session closing it for memory.
type Closures = Rc<RefCell<BTreeMap<String, Closed>>>;

/// The name each app is known by, for the notice it leaves when it stops.
/// A launch id carries the app id alone, so its tab is named from here.
type Titles = Rc<RefCell<BTreeMap<String, String>>>;

/// A tab held for a window of an app that is on its way: the tab it takes
/// when it maps (`None` until the desktop makes it), and whether it takes
/// focus there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HeldTab {
    tab: Option<TabId>,
    background: bool,
}

/// The tabs held for each app's coming windows, oldest first: an app's next
/// window lands in the one its own open asked for.
type Held = Rc<RefCell<BTreeMap<String, Vec<HeldTab>>>>;

/// Where a window of an app that asked to open beside one of its own lands:
/// the tab it lands after, and whether it takes focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Beside {
    beside: RouteIds,
    background: bool,
}

/// The tile source behind the policy's surface: a window's tab is opened
/// from the window waiting under its launch id, and its tile is the
/// arriving placeholder, which the window's own buffer covers. Every other
/// launch is not this surface's to open — the session starts the app — so it
/// is queued for the session and opens no tab here.
struct Windows {
    arriving: Arriving,
    launches: Launches,
    /// The app each notice tab is about, which Reopen asks for again.
    crashed: Crashed,
    /// Why each of those apps is not running.
    closures: Closures,
    /// The name each app is known by, for its notice.
    titles: Titles,
    /// The tabs held for the apps' coming windows.
    held: Held,
}

impl TileSource for Windows {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        // A window this device left open whose app did not come back says so
        // where the window would be, in the tab the person left it in: the
        // name they know it by, and why it is not here. Closing the tab is
        // the end of it.
        if let TileContent::LocalApp { note, .. } = &tab.tile
            && !note.is_empty()
        {
            return TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: note.clone(),
            });
        }
        let Some(app) = self.crashed.borrow().get(&tab.id).cloned() else {
            return TileHost::new(ui::widget::TileContent::Arriving);
        };
        // The app is gone but its tab stays until the person decides what to
        // do: a notice with the name it had, and Reopen to try again.
        let title = self
            .titles
            .borrow()
            .get(&app)
            .cloned()
            .unwrap_or_else(|| shown_name(&app));
        let closed = self
            .closures
            .borrow()
            .get(&app)
            .copied()
            .unwrap_or_default();
        let launches = Rc::clone(&self.launches);
        let reopened = app;
        TileHost::new(ui::widget::TileContent::App(Box::new(
            tilo_desktop_surface::crashed_app::CrashedApp::new(&title, closed, move || {
                launches.borrow_mut().push(reopened.clone());
            }),
        )))
    }

    fn launch(&mut self, _: &Workspace, id: TabId, app: &str) -> Option<Tab> {
        let held = app
            .strip_prefix(HELD_BEHIND_PREFIX)
            .map(|app| (app, true))
            .or_else(|| app.strip_prefix(HELD_PREFIX).map(|app| (app, false)));
        if let Some((app, background)) = held {
            // A window of the app is on its way and may take a while: its
            // tab holds the place it lands in from this frame, showing the
            // app's name, and the window fills it when it maps.
            let mut held = self.held.borrow_mut();
            // The session gave the place up before the desktop made it:
            // nothing comes to fill it.
            let record = held
                .entry(app.to_owned())
                .or_default()
                .iter_mut()
                .find(|record| record.tab.is_none() && record.background == background)?;
            record.tab = Some(id);
            let title = self
                .titles
                .borrow()
                .get(app)
                .cloned()
                .unwrap_or_else(|| shown_name(app));
            return Some(Tab {
                id,
                title,
                tile: TileContent::AppPlaceholder {
                    kind: tilo_desktop_model::AppKind::Other(app.to_owned()),
                    label: app.to_owned(),
                },
                app_title: None,
            });
        }
        if let Some(app) = app.strip_prefix(CRASHED_PREFIX) {
            self.crashed.borrow_mut().insert(id, app.to_owned());
            let title = self
                .titles
                .borrow()
                .get(app)
                .cloned()
                .unwrap_or_else(|| shown_name(app));
            return Some(Tab {
                id,
                title,
                tile: TileContent::AppPlaceholder {
                    kind: tilo_desktop_model::AppKind::Other(app.to_owned()),
                    label: app.to_owned(),
                },
                app_title: None,
            });
        }
        if let Some(window_id) = app.strip_prefix(LAUNCH_PREFIX) {
            let window = self
                .arriving
                .borrow_mut()
                .remove(&window_id.parse().ok()?)?;
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
        } else {
            // A Tilo package or one of the machine's apps: the session starts
            // it, and the window it opens takes its tab then.
            self.launches.borrow_mut().push(app.to_owned());
            None
        }
    }
}

/// The desktop's answer to where windows go and which has focus.
pub struct DesktopPolicy {
    handle: DesktopHandle,
    arriving: Arriving,
    /// Launches the session has to start, oldest first.
    launches: Launches,
    /// The apps whose notice tabs are open, by tab.
    crashed: Crashed,
    /// Why each of those apps is not running.
    closures: Closures,
    /// The name each app is known by, for its notice.
    titles: Titles,
    /// The tabs held for the apps' coming windows.
    held: Held,
    /// The places the apps asked for, oldest first, by the app that asked:
    /// its next window lands at the one its open asked for.
    places: BTreeMap<String, Vec<Beside>>,
    /// Windows the desktop has given a tab, so a tab gone without
    /// [`Self::unmap`] is one the person closed.
    placed: BTreeSet<u64>,
    /// The dialogs, oldest first, each with the window it belongs to.
    floating: BTreeMap<u64, Option<u64>>,
    /// The displays, primary first, a mirror set as one.
    displays: Vec<Display>,
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
        let launches = Launches::default();
        let crashed = Crashed::default();
        let closures = Closures::default();
        let titles = Titles::default();
        let held = Held::default();
        let (surface, handle) = DesktopSurface::new(
            tree,
            salt,
            Box::new(Windows {
                arriving: Rc::clone(&arriving),
                launches: Rc::clone(&launches),
                crashed: Rc::clone(&crashed),
                closures: Rc::clone(&closures),
                titles: Rc::clone(&titles),
                held: Rc::clone(&held),
            }),
        );
        let policy = Self {
            handle,
            arriving,
            launches,
            crashed,
            closures,
            titles,
            held,
            places: BTreeMap::new(),
            placed: BTreeSet::new(),
            floating: BTreeMap::new(),
            displays: Vec::new(),
        };
        (policy, surface)
    }

    /// The launches the person asked for that no window of theirs is behind,
    /// oldest first: the session starts each. A window that already arrived
    /// is not here ([`TileSource::launch`] takes it instead), and neither is
    /// a repeated notice tab.
    pub fn take_launches(&mut self) -> Vec<String> {
        std::mem::take(&mut *self.launches.borrow_mut())
    }

    /// Show that the app `asked` names stopped working, in a tab of its own
    /// with Reopen asking for `asked` again. The window it had is already
    /// gone; this is what the person finds in its place, instead of nothing.
    ///
    /// # Errors
    /// The desktop has no workspace or strip to open it in.
    pub fn crashed(&mut self, asked: &str, title: &str) -> Result<(), String> {
        self.stopped(asked, title, Closed::Crashed)
    }

    /// Show that the app `asked` names is not running any more, for `closed`,
    /// in a tab of its own with Reopen asking for `asked` again: the same
    /// notice a crash leaves, saying why the session closed it when the app
    /// did not stop on its own.
    ///
    /// # Errors
    /// The desktop has no workspace or strip to open it in.
    pub fn stopped(&mut self, asked: &str, title: &str, closed: Closed) -> Result<(), String> {
        self.titles
            .borrow_mut()
            .insert(asked.to_owned(), title.to_owned());
        self.closures.borrow_mut().insert(asked.to_owned(), closed);
        // The tab this opens is named by its launch id; the title it shows
        // and the launch Reopen asks for come from the maps the tile source
        // reads.
        let launch = Command::Launch {
            generation: GENERATION.into(),
            app: format!("{CRASHED_PREFIX}{asked}"),
        };
        self.handle.dispatch(&launch, GENERATION)
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

    /// The outputs the desktop spreads over, primary first, in the
    /// surface's coordinates: a mirror set is one display
    /// ([`displays::distinct`]), and with several each display holds its
    /// own windows, laid out on the surface's next pass. Whether anything
    /// changed.
    pub fn set_displays(&mut self, outputs: &[displays::Output]) -> bool {
        let displays = displays::distinct(outputs);
        let changed = self.displays != displays;
        let moved = self.handle.set_displays(&displays);
        self.displays = displays;
        changed || moved
    }

    /// The display the person works on: the focused one across several,
    /// else the primary. The partition answers both: focus falls back to
    /// the display the person made primary in Settings, else the first.
    fn working_display(&self) -> Option<&Display> {
        let focused = self.handle.focused_display();
        self.displays
            .iter()
            .find(|display| Some(display.id) == focused)
            .or_else(|| self.displays.first())
    }

    /// The displays in reading order, left to right: empty on one.
    #[must_use]
    pub fn displays(&self) -> Vec<u32> {
        self.handle.displays()
    }

    /// The display the window shows on, across several displays.
    #[must_use]
    pub fn display_of(&self, window: u64) -> Option<u32> {
        let group = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .find(|group| group.tabs.iter().any(|tab| foreign(tab) == Some(window)))
                .map(|group| group.id)
        })?;
        self.handle.display_of(group)
    }

    /// Whether `window` is a dialog floating over its parent.
    #[must_use]
    pub fn is_floating(&self, window: u64) -> bool {
        self.floating.contains_key(&window)
    }

    /// Give `window` its place: a dialog ([`floating::floats`]) floats over
    /// the window it belongs to; any other window gets a tab beside the
    /// focused window of the shown strip, on the surface's next update
    /// pass, where it takes focus. One whose app asked to open beside a tab
    /// of its own takes its place right after that tab instead.
    ///
    /// # Errors
    /// The desktop has no workspace or strip to open it in.
    pub fn map(&mut self, window: Window) -> Result<(), String> {
        let display = self
            .working_display()
            .map(|display| (display.width, display.height));
        if floating::floats(&window, display) {
            let parent = window.parent.filter(|parent| self.holds(*parent));
            self.floating.insert(window.id, parent);
            return Ok(());
        }
        let id = window.id;
        // A window of an app that asked for another one right after a tab of
        // its own takes the tab held for it there, from the moment it maps.
        if let Some(held) = self.take_held(window.app_id.as_deref().unwrap_or_default())
            && self.bind_held(&window, held)
        {
            return Ok(());
        }
        let launch = format!("{LAUNCH_PREFIX}{id}");
        let beside = self
            .take_place(window.app_id.as_deref().unwrap_or_default())
            .map(|place| Command::TabOpenBeside {
                generation: GENERATION.into(),
                app: launch.clone(),
                beside: Route::from_ids(place.beside),
                background: place.background,
            });
        self.arriving.borrow_mut().insert(id, window);
        let placed = match beside {
            // The tab it was asked to open beside may be gone by now: it
            // lands as any launch does then.
            Some(command) => self.handle.dispatch(&command, GENERATION).or_else(|_| {
                self.handle.dispatch(
                    &Command::Launch {
                        generation: GENERATION.into(),
                        app: launch,
                    },
                    GENERATION,
                )
            }),
            None => self.handle.dispatch(
                &Command::Launch {
                    generation: GENERATION.into(),
                    app: launch,
                },
                GENERATION,
            ),
        };
        placed.inspect_err(|_| {
            self.arriving.borrow_mut().remove(&id);
        })
    }

    /// The tab of `app` a request from it answers to, with its group and its
    /// place in it: the focused tab when it shows `app`, else the app's
    /// first tab, in the workspace in front. Only `app`'s own tabs are
    /// candidates, so a request never anchors on another app's tab.
    #[must_use]
    pub fn anchor_for(&self, app: &str) -> Option<(TabId, GroupId, usize)> {
        self.handle.read(|tree| anchor_for(tree, app))
    }

    /// Close the tab of `app` a request from it answers to, as the app's own
    /// close asks: its window goes with it, and the tab comes back on a
    /// reopen.
    pub fn close_tab_of(&mut self, app: &str) -> bool {
        let Some(tab) = self.anchor_for(app).map(|(tab, ..)| tab) else {
            return false;
        };
        let Some(beside) = self.handle.read(|tree| route_of(tree, tab)) else {
            return false;
        };
        let command = Command::Close {
            generation: GENERATION.into(),
            route: Route::from_ids(beside),
        };
        self.handle.dispatch(&command, GENERATION).is_ok()
    }

    /// Put the shown workspace's most recently closed tab back where it was,
    /// as an app's reopen asks.
    pub fn reopen_closed(&mut self) -> bool {
        let command = Command::Reopen {
            generation: GENERATION.into(),
        };
        self.handle.dispatch(&command, GENERATION).is_ok()
    }

    /// Step the group in front on by one tab, as an app's own tab chord asks
    /// when its tab is the one in front: no app steps a group it is not
    /// showing in.
    pub fn cycle_tab(&mut self, app: &str, forward: bool) -> bool {
        let Some(anchor) = self.anchor_for(app).map(|(tab, ..)| tab) else {
            return false;
        };
        let target = self.handle.read(|tree| {
            let ids = active_route(tree)?;
            if ids.tab != anchor {
                return None;
            }
            let group = tree
                .workspaces
                .iter()
                .find(|workspace| workspace.id == ids.workspace)?
                .strips
                .iter()
                .find(|strip| strip.id == ids.strip)?
                .window_groups
                .iter()
                .find(|group| group.id == ids.group)?;
            let count = isize::try_from(group.tabs.len()).ok()?;
            if count == 0 {
                return None;
            }
            let current = group
                .tabs
                .iter()
                .position(|tab| tab.id == group.active_tab)?;
            let step = if forward { 1 } else { -1 };
            let next = (isize::try_from(current).ok()?.saturating_add(step)).rem_euclid(count);
            Some(RouteIds {
                tab: group.tabs.get(usize::try_from(next).ok()?)?.id,
                ..ids
            })
        });
        let Some(target) = target else {
            return false;
        };
        let command = Command::Select {
            generation: GENERATION.into(),
            route: Route::from_ids(target),
        };
        self.handle.dispatch(&command, GENERATION).is_ok()
    }

    /// Hold a tab for the next window of `app`, right after `anchor` (the tab
    /// [`Self::anchor_for`] finds when none is named), taking focus there
    /// unless `background`: the window takes the tab's place when it maps, so
    /// the person sees it at once instead of when a launch is through.
    /// False when the desktop holds no tab of `app` to open beside: the
    /// window then lands wherever a launch lands.
    pub fn hold_beside(&mut self, app: &str, anchor: Option<TabId>, background: bool) -> bool {
        let Some(tab) = anchor.or_else(|| self.anchor_for(app).map(|(tab, ..)| tab)) else {
            return false;
        };
        let Some(beside) = self.handle.read(|tree| route_of(tree, tab)) else {
            return false;
        };
        let launch = format!(
            "{}{app}",
            if background {
                HELD_BEHIND_PREFIX
            } else {
                HELD_PREFIX
            }
        );
        let command = Command::TabOpenBeside {
            generation: GENERATION.into(),
            app: launch,
            beside: Route::from_ids(beside),
            background,
        };
        if self.handle.dispatch(&command, GENERATION).is_err() {
            return false;
        }
        // The desktop makes the tab on its next pass: the place is asked for
        // here, so one that never comes can be given up before then.
        self.held
            .borrow_mut()
            .entry(app.to_owned())
            .or_default()
            .push(HeldTab {
                tab: None,
                background,
            });
        true
    }

    /// The launch behind the place held for `app`'s next window started
    /// nothing: the place it held goes, whether the desktop made it yet or
    /// not.
    pub fn release_held(&mut self, app: &str) {
        let Some(HeldTab { tab: Some(tab), .. }) = self.forget_held(app) else {
            return;
        };
        let mut tree = self.handle.read(Clone::clone);
        if tree.close_tab(tab).is_some() {
            self.handle.replace_tree(tree);
        }
    }

    /// Let go of the place held for `app`'s next window, leaving the tab it
    /// was held for where it is: the launch it was held for started nothing,
    /// and a tab a person left a window in is theirs to close, not the
    /// launch's.
    pub fn give_up_held(&mut self, app: &str) {
        self.forget_held(app);
    }

    /// The place held for `app`'s next window, taken back without touching a
    /// tab: the desktop may not have made it yet.
    fn forget_held(&self, app: &str) -> Option<HeldTab> {
        let mut held = self.held.borrow_mut();
        let key = held
            .keys()
            .find(|asked| app_ids_match(app, asked))
            .cloned()?;
        let queue = held.get_mut(&key)?;
        let released = (!queue.is_empty()).then(|| queue.remove(0));
        if queue.is_empty() {
            held.remove(&key);
        }
        released
    }

    /// Hold `tab` for the next window `app` opens, in the background: the
    /// window takes the tab the person left it in — its place, its width,
    /// the strip it was in — instead of a new one beside it, and does not
    /// take focus from the window they had in front.
    ///
    /// `app` is the app as its windows announce themselves, which is what the
    /// tab holds it as when the desk is saved. False when `tab` is not on the
    /// desk any more.
    pub fn adopt(&mut self, app: &str, tab: TabId) -> bool {
        if self.handle.read(|tree| route_of(tree, tab)).is_none() {
            return false;
        }
        self.held
            .borrow_mut()
            .entry(app.to_owned())
            .or_default()
            .push(HeldTab {
                tab: Some(tab),
                background: true,
            });
        true
    }

    /// `tab` stands in for the app whose window did not come, saying why:
    /// what the person sees where the window would be, and closes when they
    /// are done with it. False when the tab is gone or holds no app.
    pub fn note_in_tab(&mut self, tab: TabId, note: &str) -> bool {
        let mut tree = self.handle.read(Clone::clone);
        let Some(found) = tabs_mut(&mut tree).find(|found| found.id == tab) else {
            return false;
        };
        let TileContent::LocalApp { note: said, .. } = &mut found.tile else {
            return false;
        };
        if said == note {
            return false;
        }
        note.clone_into(said);
        self.handle.replace_tree(tree);
        self.handle.rebuild(tab);
        true
    }

    /// Whether `tab` still waits for the window of a saved app: what the
    /// restore asks before it opens the next one.
    pub fn waits_for_window(&self, tab: TabId) -> bool {
        self.handle.read(|tree| {
            tabs(tree)
                .find(|found| found.id == tab)
                .is_some_and(|found| matches!(found.tile, TileContent::LocalApp { .. }))
        })
    }

    /// The tab held for the next window of the app `app` announces, oldest
    /// first: taken once, by the one window it was held for.
    fn take_held(&self, app: &str) -> Option<HeldTab> {
        let mut held = self.held.borrow_mut();
        let key = held
            .keys()
            .find(|asked| app_ids_match(app, asked))
            .cloned()?;
        let queue = held.get_mut(&key)?;
        let index = queue.iter().position(|record| record.tab.is_some())?;
        let taken = queue.remove(index);
        if queue.is_empty() {
            held.remove(&key);
        }
        Some(taken)
    }

    /// `window` takes the place of the tab held for it. False when the tab is
    /// gone (the person closed it while the window was on its way), and the
    /// window lands wherever a launch lands.
    fn bind_held(&self, window: &Window, held: HeldTab) -> bool {
        let Some(held_tab) = held.tab else {
            return false;
        };
        let mut tree = self.handle.read(Clone::clone);
        let Some(ids) = route_of(&tree, held_tab) else {
            return false;
        };
        let app_id = window.app_id.clone().unwrap_or_default();
        let title = window.title.clone().unwrap_or_else(|| app_id.clone());
        let Some(tab) = tabs_mut(&mut tree).find(|tab| tab.id == held_tab) else {
            return false;
        };
        tab.tile = TileContent::Foreign {
            app_id,
            surface_id: window.id,
        };
        tab.set_window_title(&title);
        if !held.background {
            if let Some(group) = tree
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.id == ids.workspace)
                .and_then(|workspace| {
                    workspace
                        .strips
                        .iter_mut()
                        .find(|strip| strip.id == ids.strip)
                })
                .and_then(|strip| {
                    strip
                        .window_groups
                        .iter_mut()
                        .find(|group| group.id == ids.group)
                })
            {
                group.activate_tab(held_tab);
            }
            tree.set_active_group(ids.strip, ids.group);
        }
        self.handle.replace_tree(tree);
        if !held.background {
            self.handle.focus_tab(held_tab);
        }
        true
    }

    /// The next window `app` maps lands right after `anchor` (`None`: the tab
    /// [`Self::anchor_for`] finds), in front unless `background`. Queued per
    /// app, oldest first, so a burst of opens keeps each window's place.
    /// False when the desktop holds no tab of `app` to open beside: the
    /// window then lands wherever a launch lands.
    pub fn place_next_beside(
        &mut self,
        app: &str,
        anchor: Option<TabId>,
        background: bool,
    ) -> bool {
        let Some(tab) = anchor.or_else(|| self.anchor_for(app).map(|(tab, ..)| tab)) else {
            return false;
        };
        let Some(beside) = self.handle.read(|tree| route_of(tree, tab)) else {
            return false;
        };
        self.places
            .entry(app.to_owned())
            .or_default()
            .push(Beside { beside, background });
        true
    }

    /// The place the app of an arriving window asked for, oldest first:
    /// taken once, by the window it was asked for.
    fn take_place(&mut self, app: &str) -> Option<Beside> {
        let key = self
            .places
            .keys()
            .find(|asked| app_ids_match(app, asked))?
            .clone();
        let mut asked = self.places.remove(&key)?;
        let place = (!asked.is_empty()).then(|| asked.remove(0));
        if !asked.is_empty() {
            self.places.insert(key, asked);
        }
        place
    }

    /// `window` went away: its tab closes, and focus goes back to the
    /// window the person had in front before it.
    pub fn unmap(&mut self, window: u64) {
        // Its dialogs float on, over the display.
        for parent in self.floating.values_mut() {
            if *parent == Some(window) {
                *parent = None;
            }
        }
        if self.floating.remove(&window).is_some() {
            return;
        }
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
        // The person's own name for the tab stands in front of the window's,
        // which is kept beside it.
        if tab.set_window_title(title) {
            self.handle.replace_tree(tree);
        }
    }

    /// The window in front of the focused group of the shown strip, or
    /// its newest dialog: a window with a dialog open answers it first.
    #[must_use]
    pub fn focused(&self) -> Option<u64> {
        let mut window = self.handle.read(|tree| {
            let route = tilo_desktop_surface::navigation::active_route(tree)?;
            window_of(tree, route.tab)
        })?;
        while let Some(dialog) = self.newest_dialog_of(window) {
            window = dialog;
        }
        Some(window)
    }

    fn newest_dialog_of(&self, window: u64) -> Option<u64> {
        self.floating
            .iter()
            .rev()
            .find(|(_, parent)| **parent == Some(window))
            .map(|(dialog, _)| *dialog)
    }

    /// Whether the desktop holds `window`, as a tab or a dialog.
    fn holds(&self, window: u64) -> bool {
        self.floating.contains_key(&window)
            || self.arriving.borrow().contains_key(&window)
            || self.handle.read(|tree| tab_of(tree, window)).is_some()
    }

    /// Bring `window` forward and focus it, on the next update pass, as
    /// clicking it does; a dialog brings the window it belongs to. False
    /// when the desktop holds no such window.
    pub fn focus(&mut self, window: u64) -> bool {
        let mut window = window;
        while let Some(parent) = self.floating.get(&window) {
            match parent {
                Some(parent) => window = *parent,
                None => return true,
            }
        }
        self.handle
            .read(|tree| tab_of(tree, window))
            .is_some_and(|tab| self.handle.focus_tab(tab))
    }

    /// Fill the strip with `window`, or give it its width back: what a
    /// client asks for itself, and what the window menu asks of it. False
    /// when the desktop holds no such window (a dialog floats over one).
    pub fn set_maximized(&mut self, window: u64, maximized: bool) -> bool {
        if self.floating.contains_key(&window) {
            return false;
        }
        self.handle
            .read(|tree| tab_of(tree, window))
            .is_some_and(|tab| self.handle.maximize_tab(tab, maximized))
    }

    /// Cover the display with `window`, or give it back: what a client asks
    /// for itself, and what the window menu asks of it. False when the
    /// desktop holds no such window (a dialog floats over one).
    pub fn set_fullscreen(&mut self, window: u64, fullscreen: bool) -> bool {
        if self.floating.contains_key(&window) {
            return false;
        }
        self.handle
            .read(|tree| tab_of(tree, window))
            .is_some_and(|tab| self.handle.fullscreen_tab(tab, fullscreen))
    }

    /// `window` is waiting for the person: its tab says so until they
    /// look at it. False when the desktop holds no such window or it is
    /// the one in front.
    pub fn ask_attention(&mut self, window: u64) -> bool {
        self.handle
            .read(|tree| tab_of(tree, window))
            .is_some_and(|tab| self.handle.ask_attention(tab))
    }

    /// Whether `window` is waiting for the person.
    #[must_use]
    pub fn asks_attention(&self, window: u64) -> bool {
        self.handle
            .read(|tree| tab_of(tree, window))
            .is_some_and(|tab| self.handle.asks_attention(tab))
    }

    /// The windows the desk shows on the workspace in front, each with the
    /// id its app announces itself by: what a launch of an app that may
    /// already be running is decided against. A window still arriving is not
    /// here, since it has no tab yet.
    #[must_use]
    pub fn windows_in_front(&self) -> Vec<(u64, String)> {
        self.handle.read(|tree| {
            let Some(workspace) = tree.active_workspace() else {
                return Vec::new();
            };
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .filter_map(|tab| match &tab.tile {
                    TileContent::Foreign { app_id, surface_id } => {
                        Some((*surface_id, app_id.clone()))
                    }
                    _ => None,
                })
                .collect()
        })
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
        let mut hidden: Vec<u64> = self
            .handle
            .read(windows)
            .into_iter()
            .filter(|window| !shown.iter().any(|(id, _)| id == window))
            .collect();
        let display = self.working_display().map_or(Rect::ZERO, |display| {
            Rect::new(
                display.x,
                display.y,
                display.x + display.width,
                display.y + display.height,
            )
        });
        let mut floating = Vec::new();
        for &dialog in self.floating.keys() {
            // Up to the window the dialog (or the dialog it is a dialog of)
            // belongs to.
            let mut root = dialog;
            let mut depth = 0usize;
            while let Some(Some(parent)) = self.floating.get(&root) {
                root = *parent;
                depth = depth.saturating_add(1);
                if depth > self.floating.len() {
                    break;
                }
            }
            let area = if self.floating.contains_key(&root) {
                Some(display)
            } else {
                shown
                    .iter()
                    .find(|(id, _)| *id == root)
                    .map(|(_, rect)| *rect)
            };
            match area {
                Some(area) => floating.push((dialog, area)),
                None => hidden.push(dialog),
            }
        }
        Layout {
            fullscreen: self.handle.read(covering_window),
            shown,
            hidden,
            floating,
        }
    }

    /// The windows whose tabs the person closed since the last call: the
    /// compositor asks each to close. A window stays placed until it is
    /// unmapped, so one that refuses to close keeps its place in the
    /// compositor until it goes.
    pub fn take_closed(&mut self) -> Vec<u64> {
        let tree = self.handle.read(Clone::clone);
        let present: BTreeSet<u64> = windows(&tree).into_iter().collect();
        let closed: Vec<u64> = self.placed.difference(&present).copied().collect();
        for window in &closed {
            self.placed.remove(window);
        }
        self.placed.extend(present);
        // A notice tab the person closed keeps no app behind it, and the
        // name it showed goes with it.
        let open: BTreeSet<TabId> = tabs(&tree).map(|tab| tab.id).collect();
        self.held.borrow_mut().retain(|_, held| {
            held.retain(|candidate| candidate.tab.is_none_or(|tab| open.contains(&tab)));
            !held.is_empty()
        });
        let mut crashed = self.crashed.borrow_mut();
        let mut closures = self.closures.borrow_mut();
        let mut titles = self.titles.borrow_mut();
        crashed.retain(|tab, app| {
            let kept = open.contains(tab);
            if !kept {
                titles.remove(app);
            }
            kept
        });
        let shown: BTreeSet<String> = crashed.values().cloned().collect();
        closures.retain(|app, _| shown.contains(app));
        closed
    }
}

/// Every window the tree holds a tab of, in tree order.
fn windows(tree: &DesktopTree) -> Vec<u64> {
    tabs(tree).filter_map(foreign).collect()
}

/// What a launch id is called when nothing named its app: a host app's id
/// without the prefix that says it is one.
fn shown_name(app: &str) -> String {
    app.strip_prefix("host:").unwrap_or(app).to_owned()
}

fn tabs(tree: &DesktopTree) -> impl Iterator<Item = &Tab> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
}

fn tabs_mut(tree: &mut DesktopTree) -> impl Iterator<Item = &mut Tab> {
    tree.workspaces
        .iter_mut()
        .flat_map(|workspace| &mut workspace.strips)
        .flat_map(|strip| &mut strip.window_groups)
        .flat_map(|group| &mut group.tabs)
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

/// The window covering the display the person works on, if one is: the tab
/// in front of the group the desk took full screen there.
fn covering_window(tree: &DesktopTree) -> Option<u64> {
    let group = tree.fullscreen_group()?;
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .find(|candidate| candidate.id == group)
        .and_then(|group| window_of(tree, group.active_tab))
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

/// Whether the app a window announces is the app a request named: the id
/// itself, or the short name it ends with (`so.tilo.notes` is `notes`).
fn app_ids_match(announced: &str, asked: &str) -> bool {
    let announced = announced.to_ascii_lowercase();
    let asked = asked.to_ascii_lowercase();
    announced == asked || asked.rsplit('.').next() == Some(announced.as_str())
}

/// Where `tab` sits, for the command naming it.
fn route_of(tree: &DesktopTree, tab: TabId) -> Option<RouteIds> {
    tree.workspaces.iter().find_map(|workspace| {
        workspace.strips.iter().find_map(|strip| {
            let group = strip
                .window_groups
                .iter()
                .find(|group| group.tabs.iter().any(|candidate| candidate.id == tab))?;
            Some(RouteIds {
                workspace: workspace.id,
                strip: strip.id,
                group: group.id,
                tab,
            })
        })
    })
}

/// The tab of `app` a request from it answers to, with its group and place:
/// the focused tab when it shows `app`, else the app's first tab in the
/// workspace in front.
fn anchor_for(tree: &DesktopTree, app: &str) -> Option<(TabId, GroupId, usize)> {
    let workspace = tree.active_workspace()?;
    let focused = tilo_desktop_surface::navigation::active_route(tree).map(|ids| ids.tab);
    let tabs: Vec<(TabId, GroupId, usize)> = workspace
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| {
            group
                .tabs
                .iter()
                .enumerate()
                .map(move |(index, tab)| (tab, group.id, index))
        })
        .filter_map(|(tab, group, index)| {
            let id = tilo_desktop_model::apps::canonical_app_id(&tab.tile)?;
            app_ids_match(&id, app).then_some((tab.id, group, index))
        })
        .collect();
    tabs.iter()
        .copied()
        .find(|(tab, ..)| Some(*tab) == focused)
        .or_else(|| tabs.first().copied())
}
