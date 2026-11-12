//! The desktop's window policy: a compositor's windows as tabs of the portable
//! desktop surface.
//! A compositor knows windows as toplevels; where they go, which has focus and which
//! the person closed is the desktop's answer. [`DesktopPolicy`] gives each window a
//! tab of the [`DesktopSurface`]'s strips ([`TileContent::Foreign`], keyed by the
//! window's id), so windows open beside the focused one, group into tabs, pan with
//! their strip and move between strips and workspaces through the same widgets and
//! model the browser desktop runs. After each layout pass the compositor
//! reads [`DesktopPolicy::layout`]: the windows to show and where, and those to hide.
//! Dialogs float over the window they belong to ([`floating`]), and across several
//! displays each display holds its own windows. Nothing here knows Wayland.
pub mod cards;
pub mod displays;
pub mod floating;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use tilo_desktop_model::commands::move_tab_to_workspace;
use tilo_desktop_model::{
    DesktopTree, Display, GroupId, IdMinter, Tab, TabId, TileContent, Workspace, WorkspaceSlot,
    peer_salt_of,
};
use tilo_desktop_surface::navigation::{ChromeEffect, DeskEffect, RouteIds, active_route};
use tilo_desktop_surface::outcome::{Closed, Outcome};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource, seed_tree};
use ui::kurbo::Rect;
use ui::widget::TileHost;

/// The launch id a window's tab is opened under.
const LAUNCH_PREFIX: &str = "window:";

/// The launch id a crashed app's notice tab is opened under.
const CRASHED_PREFIX: &str = "crashed:";

/// The launch id a tab held for an app's next window is opened under, in
/// front and behind.
const HELD_PREFIX: &str = "held:";
const HELD_BEHIND_PREFIX: &str = "held-behind:";

/// The name of a launch whose place the desk holds for a window the session
/// starts itself.
fn held_name(app: &str, background: bool) -> String {
    format!(
        "{}{app}",
        if background {
            HELD_BEHIND_PREFIX
        } else {
            HELD_PREFIX
        }
    )
}

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
    /// The process that made the window, when the compositor knows it: what tells the workspace
    /// it was launched into ([`DesktopPolicy::reads_launch_workspaces`]).
    pub pid: Option<u32>,
}

/// The workspace the process `pid` of `app` was launched into, by its id, as the session that
/// launched it knows.
pub type LaunchWorkspaces = Rc<dyn Fn(u32, &str) -> Option<String>>;

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
    /// Each window floating over the desk, and what the desk covers the
    /// windows under it with for that window: its bar and body, the whole
    /// rect they fill, in surface coordinates ([`DesktopPolicy::float_covers`]).
    pub covers: Vec<(u64, Rect)>,
}

/// Windows waiting for their tab, shared with the surface's tile source.
type Arriving = Rc<RefCell<BTreeMap<u64, Window>>>;

/// One launch the desk asks this session to make: the command naming the app,
/// and the tab the desk holds for the window it opens, when it made one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchAsk {
    /// The launch command, as the desk was asked for it.
    pub app: String,
    /// The tab the window takes when it maps. It is the window's instance:
    /// what the app's own continuation reports are addressed by.
    pub tab: Option<TabId>,
}

/// The launches the desktop asked for and no window is behind: the session
/// starts each one ([`DesktopPolicy::take_launches`]).
type Launches = Rc<RefCell<Vec<LaunchAsk>>>;

/// A tab the desk made for a window whose app asked for it: the session starts
/// that window beside it, so the window takes the tab as its instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldLaunch {
    /// The app whose window the tab waits for.
    pub app: String,
    /// The tab the window takes when it maps.
    pub tab: TabId,
}

/// The tabs the desk made for the windows apps asked it for, oldest first: the
/// session starts each one ([`DesktopPolicy::take_held_launches`]).
type HeldLaunches = Rc<RefCell<Vec<HeldLaunch>>>;

/// What each window on this desk shows, as its app holds the record: the
/// record it is on and the kind of resource that record keeps.
pub trait WindowNotes {
    /// The record `tab`'s window shows and the kind of resource it keeps,
    /// when its app holds one.
    fn window_record(&self, tab: TabId) -> Option<(String, String)>;
}

/// The session's notes, shared with the surface this policy places windows on:
/// in place once the session that keeps them runs.
type Notes = Rc<RefCell<Option<Rc<dyn WindowNotes>>>>;

/// The app each notice tab is about, by tab: the launch its Reopen asks for
/// again. What the tile source draws in place of a window that is not
/// coming.
type Crashed = Rc<RefCell<BTreeMap<TabId, String>>>;

/// What each app that left an answer is told of it, by the app: that it did
/// not start, that it stopped working, or that the session closed it for
/// memory.
type Answers = Rc<RefCell<BTreeMap<String, Outcome>>>;

/// The name each app is known by, for the notice it leaves when it stops.
/// A launch id carries the app id alone, so its tab is named from here.
type Titles = Rc<RefCell<BTreeMap<String, String>>>;

/// What the session calls each app, shared with the surface's tile source:
/// the installed package's localised name, else the machine's desktop entry,
/// else the registry's. A desk told no names uses the ones every desktop
/// carries ([`tilo_desktop_search::names::app_name`]).
type AppNames = Rc<RefCell<Option<Rc<dyn Fn(&str) -> String>>>>;

/// What each other device of the workspaces is called, by the salt it mints
/// its ids under: a window open on one of them says which. The session's,
/// from the devices it hears there ([`DesktopPolicy::name_devices`]); a
/// device it heard no name of is not here.
type DeviceNames = Rc<RefCell<BTreeMap<u16, String>>>;

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

/// A tab the person carried to another workspace whose window an app this
/// device started fills: the session starts that app again where the person
/// put it, and the tab takes the window that comes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CarriedTab {
    /// The tab the person carried.
    pub tab: TabId,
    /// The workspace they carried it to.
    pub to: WorkspaceSlot,
}

/// The tabs the person carried to another workspace, waiting for the session.
type Carried = Rc<RefCell<Vec<CarriedTab>>>;

/// Where a window of an app that asked to open beside one of its own lands:
/// the tab it lands after, and whether it takes focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Beside {
    beside: RouteIds,
    background: bool,
}

/// What the tab of a window open on another device says: which device, when the session heard
/// what `device` is called, and what brings the window's picture here.
fn open_elsewhere(device: Option<&str>) -> String {
    match device.map(shown_name).filter(|name| !name.is_empty()) {
        Some(name) => {
            tilo_desktop_surface::i18n::t_args("peer-window-on-device", &[("device", name.into())])
        }
        None => tilo_desktop_surface::i18n::t("peer-window-elsewhere"),
    }
}

/// A device's name as a sentence holds it. The name is a person's text and is shown as the text
/// it is, on the sentence's line and in the sentence's direction: a line break, a control
/// character or a direction mark in it changes nothing of the words around it.
fn shown_name(name: &str) -> String {
    name.chars()
        .filter(|c| !directs_text(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether `c` is one of Unicode's direction marks, embeddings, overrides or isolates: each
/// draws nothing and turns the text that follows it.
const fn directs_text(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// The tile source behind the policy's surface: a window's tab is opened from the
/// window waiting under its launch id, and its tile is the arriving placeholder the
/// window's own buffer covers. Every other launch is the session's to start, so it
/// opens no tab here.
struct Windows {
    arriving: Arriving,
    launches: Launches,
    /// The app each notice tab is about, which Reopen asks for again.
    crashed: Crashed,
    /// What each of those apps is told of its not being here.
    answers: Answers,
    /// The name each app is known by, for its notice.
    titles: Titles,
    /// What the session calls the apps, for the name a tab opens with.
    names: AppNames,
    /// The tabs held for the apps' coming windows.
    held: Held,
    /// The tabs the desk made for the windows apps asked it for.
    held_launches: HeldLaunches,
    /// The tabs the person carried to another workspace, which only the
    /// session can move.
    carried: Carried,
    /// What each window shows, as its app reports it.
    notes: Notes,
    /// What the workspaces' other devices are called.
    devices: DeviceNames,
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
            // A window that runs on another device of the workspace has no
            // surface here and none is on its way: its tab says where the
            // window is and what brings its picture here, rather than
            // standing empty for good. A share from its device takes the
            // tab over with that picture. The tab's id is of the device
            // that opened the window, so the device is named when the
            // session heard what it is called.
            if matches!(tab.tile, TileContent::PeerWindow { .. }) {
                let devices = self.devices.borrow();
                let device = devices.get(&peer_salt_of(tab.id));
                return TileHost::new(ui::widget::TileContent::Notice {
                    title: tab.title.clone(),
                    detail: open_elsewhere(device.map(String::as_str)),
                });
            }
            return TileHost::new(ui::widget::TileContent::Arriving);
        };
        // The app is gone but its tab stays until the person decides what to
        // do: a notice with the name it had, and Reopen to try again.
        let title = self
            .titles
            .borrow()
            .get(&app)
            .cloned()
            .unwrap_or_else(|| app_name(&self.names, &app));
        let answer = self.answers.borrow().get(&app).cloned().unwrap_or_else(|| {
            Closed::default().outcome(&title, tilo_desktop_surface::i18n::t("answer-reopen"))
        });
        let launches = Rc::clone(&self.launches);
        let held = Rc::clone(&self.held);
        let reopened = app;
        let answered_tab = tab.id;
        TileHost::new(ui::widget::TileContent::App(Box::new(
            tilo_desktop_surface::outcome::OutcomeCard::new(&answer, move || {
                // The answer keeps its tab: the window that comes takes it
                // when the app is back, so the launch names no other.
                ask_again(&held, &launches, &reopened, answered_tab);
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
            drop(held);
            // The tab is the window's instance: the session starts it now
            // that the desk holds the place it takes.
            self.held_launches.borrow_mut().push(HeldLaunch {
                app: app.to_owned(),
                tab: id,
            });
            let tab = self.place(id, app);
            self.titles
                .borrow_mut()
                .entry(app.to_owned())
                .or_insert_with(|| tab.title.clone());
            return Some(tab);
        }
        if let Some(app) = app.strip_prefix(CRASHED_PREFIX) {
            self.crashed.borrow_mut().insert(id, app.to_owned());
            return Some(self.place(id, app));
        }
        if let Some(window_id) = app.strip_prefix(LAUNCH_PREFIX) {
            let window = self
                .arriving
                .borrow_mut()
                .remove(&window_id.parse().ok()?)?;
            let app_id = window.app_id.unwrap_or_default();
            Some(Tab {
                id,
                title: tab_title(&self.names, &app_id, window.title.as_deref()),
                tile: TileContent::Foreign {
                    app_id,
                    surface_id: window.id,
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            })
        } else {
            // A Tilo package or one of the machine's apps: this session starts it, and its
            // window takes the tab the desk holds for it. The tab is what the window is known
            // by — an app's continuation reports are addressed by its id — so the launch names
            // it.
            self.held
                .borrow_mut()
                .entry(app.to_owned())
                .or_default()
                .push(HeldTab {
                    tab: Some(id),
                    background: false,
                });
            self.launches.borrow_mut().push(LaunchAsk {
                app: app.to_owned(),
                tab: Some(id),
            });
            Some(self.place(id, app))
        }
    }

    /// The window was closed for good and the person asked for it back: the
    /// tab keeps its place for the next window of the app it holds, and the
    /// app is asked for by the name its windows announce — the session's
    /// launch record knows which of this machine's apps that is.
    fn reopened(&mut self, _: &Workspace, tab: &Tab) {
        let TileContent::LocalApp { app_id, note, .. } = &tab.tile else {
            return;
        };
        if app_id.trim().is_empty() || !note.is_empty() {
            return;
        }
        self.held
            .borrow_mut()
            .entry(app_id.clone())
            .or_default()
            .push(HeldTab {
                tab: Some(tab.id),
                background: false,
            });
        self.launches.borrow_mut().push(LaunchAsk {
            app: app_id.clone(),
            tab: Some(tab.id),
        });
    }

    fn window_record(&self, tab: TabId) -> Option<(String, String)> {
        let notes = self.notes.borrow().clone()?;
        notes.window_record(tab)
    }

    fn carry(&self, tab: TabId, to: WorkspaceSlot) -> bool {
        self.carried.borrow_mut().push(CarriedTab { tab, to });
        true
    }
}

impl Windows {
    /// The tab an app's place holds while its window comes: named as the app,
    /// showing the app's own name until the window fills it.
    fn place(&self, id: TabId, app: &str) -> Tab {
        let title = self
            .titles
            .borrow()
            .get(app)
            .cloned()
            .unwrap_or_else(|| app_name(&self.names, app));
        Tab {
            id,
            title,
            tile: TileContent::AppPlaceholder {
                kind: tilo_desktop_model::AppKind::Other(app.to_owned()),
                label: app.to_owned(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
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
    /// What each of those apps is told of its not being here.
    answers: Answers,
    /// The name each app is known by, for its notice.
    titles: Titles,
    /// What the session calls the apps, for the name a tab opens with.
    names: AppNames,
    /// The tabs held for the apps' coming windows.
    held: Held,
    /// The tabs the desk made for the windows apps asked it for, waiting for
    /// the session.
    held_launches: HeldLaunches,
    /// The tabs the person carried to another workspace, waiting for the
    /// session.
    carried: Carried,
    /// What each window shows, as its app reports it.
    notes: Notes,
    /// What the workspaces' other devices are called.
    devices: DeviceNames,
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
    /// The app-drawn cards floating over the desk, as the session placed
    /// them.
    cards: cards::Placements,
    /// The workspace each launched window belongs to, as the session that launched it knows:
    /// a window launched into a workspace that is not in front opens there, never in front.
    launched_into: Option<LaunchWorkspaces>,
    /// The windows launched into a workspace not in front whose tab the desk has not made yet,
    /// each with where it goes ([`Self::carry_launched`]).
    to_carry: Vec<(u64, WorkspaceSlot)>,
}

impl DesktopPolicy {
    /// A policy over a desktop of one workspace with one empty strip, and
    /// the surface the compositor hosts under its windows.
    #[must_use]
    pub fn new(salt: u16) -> (Self, DesktopSurface) {
        let mut minter = IdMinter::new(salt);
        Self::with_tree(seed_tree(&mut minter, "Desk", Vec::new()), salt)
    }

    /// A policy over `tree`, the desktop as last kept, minting under `salt`:
    /// a kept tree carries no range of its own (it is not written to the
    /// file), so the device whose session builds the desk says which it is.
    #[must_use]
    pub fn with_tree(tree: DesktopTree, salt: u16) -> (Self, DesktopSurface) {
        let mut tree = tree;
        tree.salt = salt;
        let arriving = Arriving::default();
        let launches = Launches::default();
        let crashed = Crashed::default();
        let answers = Answers::default();
        let titles = Titles::default();
        let names = AppNames::default();
        let held = Held::default();
        let held_launches = HeldLaunches::default();
        let carried = Carried::default();
        let notes = Notes::default();
        let devices = DeviceNames::default();
        let (surface, handle) = DesktopSurface::new(
            tree,
            salt,
            Box::new(Windows {
                arriving: Rc::clone(&arriving),
                launches: Rc::clone(&launches),
                crashed: Rc::clone(&crashed),
                answers: Rc::clone(&answers),
                titles: Rc::clone(&titles),
                names: Rc::clone(&names),
                held: Rc::clone(&held),
                held_launches: Rc::clone(&held_launches),
                carried: Rc::clone(&carried),
                notes: Rc::clone(&notes),
                devices: Rc::clone(&devices),
            }),
        );
        let policy = Self {
            handle,
            arriving,
            launches,
            crashed,
            answers,
            titles,
            names,
            held,
            held_launches,
            carried,
            notes,
            devices,
            places: BTreeMap::new(),
            placed: BTreeSet::new(),
            floating: BTreeMap::new(),
            displays: Vec::new(),
            cards: cards::Placements::new(),
            launched_into: None,
            to_carry: Vec::new(),
        };
        (policy, surface)
    }

    /// The launches the person asked for that no window of theirs is behind,
    /// oldest first: the session starts each. A window that already arrived
    /// is not here ([`TileSource::launch`] takes it instead), and neither is
    /// a repeated notice tab.
    pub fn take_launches(&mut self) -> Vec<LaunchAsk> {
        std::mem::take(&mut *self.launches.borrow_mut())
    }

    /// The tabs the desk made for windows apps asked for, oldest first: the
    /// session starts each window beside the tab it takes, so the window is
    /// that tab's instance.
    pub fn take_held_launches(&mut self) -> Vec<HeldLaunch> {
        std::mem::take(&mut *self.held_launches.borrow_mut())
    }

    /// Read what each window shows from `notes`, the session's: the desk asks
    /// it for the windows a person may share, and a window whose app reports
    /// no record shows none.
    pub fn reads_window_records(&mut self, notes: Rc<dyn WindowNotes>) {
        *self.notes.borrow_mut() = Some(notes);
    }

    /// Name apps with `names`, the session's: the installed package's
    /// localised name, else the machine's desktop entry, else the registry's.
    /// A tab opens with the name of its app, never with the app's id.
    pub fn reads_app_names(&mut self, names: Rc<dyn Fn(&str) -> String>) {
        *self.names.borrow_mut() = Some(names);
    }

    /// Call the workspaces' other devices by `names`, the session's: what each device it hears
    /// there goes by, under the salt that device mints its ids under. A window open on one of
    /// them says which in its tab, and says it again when the device is called something else;
    /// a window of a device with no name here says it is open on another device.
    pub fn name_devices(&mut self, names: BTreeMap<u16, String>) {
        let renamed: BTreeSet<u16> = {
            let known = self.devices.borrow();
            known
                .keys()
                .chain(names.keys())
                .copied()
                .filter(|salt| known.get(salt) != names.get(salt))
                .collect()
        };
        if renamed.is_empty() {
            return;
        }
        *self.devices.borrow_mut() = names;
        // Each of those windows' notices stood from the frame its tab was shown in: what it
        // says now takes its place on the desk's next pass.
        let elsewhere: Vec<TabId> = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .filter(|tab| {
                    matches!(tab.tile, TileContent::PeerWindow { .. })
                        && renamed.contains(&peer_salt_of(tab.id))
                })
                .map(|tab| tab.id)
                .collect()
        });
        for tab in elsewhere {
            self.handle.rebuild(tab);
        }
    }

    /// Tell which workspace a window's process was launched into with `workspaces`, the
    /// session's: an app an agent or a service opened in a workspace other than the one in front
    /// opens its window there.
    pub fn reads_launch_workspaces(&mut self, workspaces: LaunchWorkspaces) {
        self.launched_into = Some(workspaces);
    }

    /// The workspace `window` was launched into, when it is not the one in front: where its tab
    /// goes instead of the front one's strip.
    fn launched_elsewhere(&self, window: &Window) -> Option<WorkspaceSlot> {
        let workspaces = self.launched_into.as_ref()?;
        let launched = workspaces(window.pid?, window.app_id.as_deref()?)?;
        self.handle.read(|tree| {
            let front = tree.active_workspace()?.id().to_string();
            (front != launched)
                .then(|| {
                    tree.workspaces
                        .iter()
                        .find(|held| held.id().to_string() == launched)
                        .map(|held| held.slot)
                })
                .flatten()
        })
    }

    /// Show that the app `asked` names stopped, in a tab of its own with its button
    /// asking for `asked` again: the window it had is gone, and this is what the person
    /// finds in its place.
    /// # Errors
    /// The desktop has no workspace or strip to open it in.
    pub fn stopped(&mut self, asked: &str, title: &str, closed: Closed) -> Result<(), String> {
        let answer = closed.outcome(title, tilo_desktop_surface::i18n::t("answer-reopen"));
        self.titles
            .borrow_mut()
            .insert(asked.to_owned(), title.to_owned());
        self.answers.borrow_mut().insert(asked.to_owned(), answer);
        // The tab this opens is named by its launch id; the title it shows
        // and the launch its button asks for come from the maps the tile
        // source reads.
        let launch = ChromeEffect::OpenApp {
            app: format!("{CRASHED_PREFIX}{asked}"),
            beside: None,
            background: None,
            float: None,
        };
        self.handle.dispatch(launch)
    }

    /// What the person asked for in `tab` did not happen, and `answer` says what and
    /// why: the tab keeps its place, shows the answer instead of a window that is not
    /// coming, and trying again opens the window in that same tab. The tab may not be on
    /// the desk yet: the answer waits in the maps the tile source reads.
    pub fn failed_at(&mut self, tab: TabId, asked: &str, answer: Outcome) {
        self.crashed.borrow_mut().insert(tab, asked.to_owned());
        self.answers.borrow_mut().insert(asked.to_owned(), answer);
        // The tab's content stood from the frame it was opened in: the
        // answer takes its place on the desk's next pass.
        self.handle.rebuild(tab);
    }

    /// The name the person knows `asked` by, for the words its answer uses:
    /// what the desk showed in the app's tab, else the app's own id.
    #[must_use]
    pub fn name_of(&self, asked: &str) -> String {
        self.titles
            .borrow()
            .get(asked)
            .cloned()
            .unwrap_or_else(|| app_name(&self.names, asked))
    }

    /// The answer `app` left on the desk goes, `tab` having shown its window
    /// again.
    fn keep_answer_gone(&self, tab: TabId) {
        let app = self.crashed.borrow_mut().remove(&tab);
        let Some(app) = app else {
            return;
        };
        let still_shown = self.crashed.borrow().values().any(|known| *known == app);
        if !still_shown {
            self.answers.borrow_mut().remove(&app);
            self.titles.borrow_mut().remove(&app);
        }
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

    /// The outputs the desktop spreads over, primary first: a mirror set is one display
    /// ([`displays::distinct`]), and with several each display holds its own windows.
    /// Whether anything changed.
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

    /// Whether `window` is a dialog floating over its parent, or a card an
    /// app draws over the desk.
    #[must_use]
    pub fn is_floating(&self, window: u64) -> bool {
        self.floating.contains_key(&window)
    }

    /// Place the cards the session authorises: where the floating layer puts
    /// each one is the session's to say, and this desk's to lay out.
    pub fn use_cards(&mut self, cards: cards::Placements) {
        self.cards = cards;
    }

    /// The cards, as the session places them.
    #[must_use]
    pub fn cards(&self) -> cards::Placements {
        self.cards.clone()
    }

    /// Whether `window` is a card an app draws, drawn or suspended.
    #[must_use]
    pub fn draws_card(&self, window: u64) -> bool {
        self.cards.draws(window)
    }

    /// Where the layer puts the card `window`, while it shows it.
    #[must_use]
    pub fn card_place(&self, window: u64) -> Option<Rect> {
        self.cards.card_of(window).map(|(_, at)| at)
    }

    /// The size the app asked for the card `window`.
    #[must_use]
    pub fn card_size(&self, window: u64) -> Option<(i32, i32)> {
        self.cards.size_of(window)
    }

    /// Whether `app` has a window shown on the desk now: the activity it
    /// runs draws there, and its floating card is suspended until it does
    /// not.
    #[must_use]
    pub fn shows(&self, app: &str) -> bool {
        let shown: BTreeSet<u64> = self.layout().shown.into_iter().map(|(id, _)| id).collect();
        self.handle.read(|tree| {
            tabs(tree)
                .filter(|tab| foreign(tab).is_some_and(|window| shown.contains(&window)))
                .filter_map(|tab| match &tab.tile {
                    TileContent::Foreign { app_id, .. } => Some(app_id.clone()),
                    _ => tilo_desktop_model::apps::canonical_app_id(&tab.tile),
                })
                .any(|shown| app_ids_match(&shown, app))
        })
    }

    /// Give `window` its place: a dialog ([`floating::floats`]) floats over the window it
    /// belongs to; any other gets a tab beside the focused window of the shown strip,
    /// where it takes focus. One whose app asked to open beside a tab of its own takes
    /// its place right after that tab instead.
    /// # Errors
    /// The desktop has no workspace or strip to open it in.
    pub fn map(&mut self, window: Window) -> Result<(), String> {
        // A window carrying a card's token is the app's card: it floats where
        // the layer puts it and never takes a tab. One carrying a token this
        // session granted to nobody is no card and gets no place at all.
        if let Some(token) = cards::Placements::token_of(window.app_id.as_deref()) {
            if self.cards.draws_token(token) {
                self.cards.mapped(token, window.id);
                self.floating.insert(window.id, None);
            }
            return Ok(());
        }
        let display = self
            .working_display()
            .map(|display| (display.width, display.height));
        if floating::floats(&window, display) {
            let parent = window.parent.filter(|parent| self.holds(*parent));
            self.floating.insert(window.id, parent);
            return Ok(());
        }
        let id = window.id;
        let elsewhere = self.launched_elsewhere(&window);
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
            .map(|place| ChromeEffect::OpenApp {
                app: launch.clone(),
                beside: Some(place.beside.tab.to_string()),
                background: Some(place.background),
                float: None,
            });
        self.arriving.borrow_mut().insert(id, window);
        let placed = match beside {
            // The tab it was asked to open beside may be gone by now: it
            // lands as any launch does then.
            Some(verb) => self.handle.dispatch(verb).or_else(|_| {
                self.handle.dispatch(ChromeEffect::OpenApp {
                    app: launch,
                    beside: None,
                    background: None,
                    float: None,
                })
            }),
            None => self.handle.dispatch(ChromeEffect::OpenApp {
                app: launch,
                beside: None,
                background: None,
                float: None,
            }),
        };
        placed.inspect_err(|_| {
            self.arriving.borrow_mut().remove(&id);
        })?;
        // Launched into a workspace that is not in front (an agent's, a service's): its tab goes
        // there, so the workspace in front shows only its own windows and the other holds its own.
        if let Some(slot) = elsewhere {
            self.to_carry.push((id, slot));
            self.carry_launched();
        }
        Ok(())
    }

    /// Move each window launched into a workspace that is not in front into that workspace, once
    /// the desk made its tab: what a host calls after it lays the desk out for a mapped window.
    pub fn carry_launched(&mut self) {
        let pending = std::mem::take(&mut self.to_carry);
        for (window, slot) in pending {
            let Some(tab) = self.tab_of(window) else {
                // Its tab comes with the desk's next layout.
                self.to_carry.push((window, slot));
                continue;
            };
            // ignored-failure: a workspace removed since the launch leaves the window where it
            // opened, in front, rather than nowhere
            let _ = self.carry(tab, slot);
        }
    }

    /// The tab of `app` a request from it answers to, with its group and its
    /// place in it: the focused tab when it shows `app`, else the app's
    /// first tab, in the workspace in front. Only `app`'s own tabs are
    /// candidates, so a request never anchors on another app's tab.
    #[must_use]
    pub fn anchor_for(&self, app: &str) -> Option<(TabId, GroupId, usize)> {
        self.handle.read(|tree| anchor_for(tree, app))
    }

    /// The tab of `app` the window `instance` runs in, when the desk shows it:
    /// the tab a window is given is the window's instance
    /// ([`HeldLaunch`], [`LaunchAsk::tab`]).
    #[must_use]
    pub fn tab_of_instance(&self, app: &str, instance: u64) -> Option<TabId> {
        self.handle.read(|tree| {
            tabs(tree)
                .find(|tab| {
                    tab.id == instance
                        && tilo_desktop_model::apps::canonical_app_id(&tab.tile)
                            .is_some_and(|id| app_ids_match(&id, app))
                })
                .map(|tab| tab.id)
        })
    }

    /// Where the window of `tab` shows its content, in the desk's own
    /// pixels, as of the surface's last layout: where a box a window drew
    /// stands on the desk.
    #[must_use]
    pub fn content_rect(&self, tab: TabId) -> Option<Rect> {
        self.handle
            .shown_tabs()
            .into_iter()
            .find_map(|(shown, rect)| (shown == tab).then_some(rect))
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
        let command = DeskEffect::CloseTab {
            tab: beside.tab.to_string(),
        };
        self.handle.dispatch(command).is_ok()
    }

    /// Put the shown workspace's most recently closed tab back where it was,
    /// as an app's reopen asks.
    pub fn reopen_closed(&mut self) -> bool {
        self.handle.dispatch(ChromeEffect::ReopenClosed).is_ok()
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
                .find(|workspace| workspace.slot == ids.workspace)?
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
        let command = DeskEffect::Focus {
            tab: target.tab.to_string(),
        };
        self.handle.dispatch(command).is_ok()
    }

    /// Hold a tab for the next window of `app`, right after `anchor` (the tab
    /// [`Self::anchor_for`] finds when none is named), taking focus there unless
    /// `background`. False when the desktop holds no tab of `app` to open beside.
    pub fn hold_beside(&mut self, app: &str, anchor: Option<TabId>, background: bool) -> bool {
        let Some(tab) = anchor.or_else(|| self.anchor_for(app).map(|(tab, ..)| tab)) else {
            return false;
        };
        let Some(beside) = self.handle.read(|tree| route_of(tree, tab)) else {
            return false;
        };
        let command = ChromeEffect::OpenApp {
            app: held_name(app, background),
            beside: Some(beside.tab.to_string()),
            background: Some(background),
            float: None,
        };
        self.hold_launch(app, command, background)
    }

    /// Hold a tab for the next window of `app` in a window of its own, right
    /// after the one in front and standing over the desk beside it: the window
    /// a send landed in, which the desk gives the strip's room to so the card
    /// it draws at stands beside the window that sent rather than over it.
    /// Taking focus there unless `background`.
    pub fn hold_window(&mut self, app: &str, background: bool) -> bool {
        let command = ChromeEffect::OpenApp {
            app: held_name(app, background),
            beside: None,
            background: Some(background),
            float: Some(true),
        };
        self.hold_launch(app, command, background)
    }

    /// Ask the desk for `command`'s window and keep the place it makes for
    /// `app`. The desktop makes the tab on its next pass: the place is asked
    /// for here, so one that never comes can be given up before then.
    fn hold_launch(&self, app: &str, command: ChromeEffect, background: bool) -> bool {
        if self.handle.dispatch(command).is_err() {
            return false;
        }
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
        self.close_held(tab);
    }

    /// Let go of the place held for `app`'s next window, leaving the tab it
    /// was held for where it is: the launch it was held for started nothing,
    /// and a tab a person left a window in is theirs to close, not the
    /// launch's.
    pub fn give_up_held(&mut self, app: &str) {
        self.forget_held(app);
    }

    /// The launch the desk held `tab` for opens no window in it: the place
    /// held for the app's next window and the tab go.
    pub fn release_tab(&mut self, tab: TabId) {
        self.held.borrow_mut().retain(|_, queue| {
            queue.retain(|held| held.tab != Some(tab));
            !queue.is_empty()
        });
        self.close_held(tab);
    }

    /// Close `tab`, one the desk held for a window that never came.
    fn close_held(&self, tab: TabId) {
        let mut tree = self.handle.read(Clone::clone);
        if tree.close_tab(tab).is_some() {
            self.handle.replace_tree(tree);
        }
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

    /// Hold `tab` for the next window `app` opens, in the background: the window takes
    /// the tab the person left it in — its place, its width, the strip it was in — and
    /// does not take focus. False when `tab` is not on the desk any more.
    pub fn adopt(&mut self, app: &str, tab: TabId) -> bool {
        self.hold_tab(app.to_owned(), tab, true)
    }

    /// Hold `tab` for the window a launch of `app` is on its way to:
    /// [`Self::adopt`], with the focus a launch the person asked for takes.
    fn hold_tab(&self, app: String, tab: TabId, background: bool) -> bool {
        if self.handle.read(|tree| route_of(tree, tab)).is_none() {
            return false;
        }
        self.held
            .borrow_mut()
            .entry(app)
            .or_default()
            .push(HeldTab {
                tab: Some(tab),
                background,
            });
        true
    }

    /// `tab` stands in for the app whose window did not come, saying why:
    /// what the person sees where the window would be, and closes when they
    /// are done with it. False when the tab is gone or holds no app.
    pub fn note_in_tab(&mut self, tab: TabId, note: &str) -> bool {
        self.handle.note_in_tab(tab, note)
    }

    /// The tabs the person carried to another workspace whose window an app
    /// this device started fills, oldest first: the session starts each app
    /// again where the person put it, and the tab takes the window that comes.
    pub fn take_carried(&self) -> Vec<CarriedTab> {
        std::mem::take(&mut *self.carried.borrow_mut())
    }

    /// Carry tab `tab` into the workspace at `to`, with a window of its own.
    /// # Errors
    /// The tab is not on the desk, it is already in that workspace, or there is no such
    /// workspace.
    pub fn carry(&mut self, tab: TabId, to: WorkspaceSlot) -> Result<(), String> {
        let mut tree = self.handle.read(Clone::clone);
        let mut mint = || Ok(self.handle.mint_id());
        move_tab_to_workspace(&mut tree, tab, to, &mut mint)?;
        self.handle.replace_tree(tree);
        Ok(())
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

    /// Whether `tab` still shows the place an app's launch held, no window
    /// having come to fill it: the launch ended in front of the person as a
    /// window that did not arrive.
    #[must_use]
    pub fn holds_a_place(&self, tab: TabId) -> bool {
        self.handle.read(|tree| {
            tabs(tree)
                .find(|found| found.id == tab)
                .is_some_and(|found| matches!(found.tile, TileContent::AppPlaceholder { .. }))
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
        let title = tab_title(&self.names, &app_id, window.title.as_deref());
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
                .find(|workspace| workspace.slot == ids.workspace)
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
        self.keep_answer_gone(held_tab);
        if !held.background {
            self.handle.focus_tab(held_tab);
        }
        true
    }

    /// The next window `app` maps lands right after `anchor` (`None`: the tab
    /// [`Self::anchor_for`] finds), in front unless `background`. Queued per app, oldest
    /// first, so a burst of opens keeps each window's place.
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
        self.to_carry.retain(|(pending, _)| *pending != window);
        self.cards.gone(window);
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

    /// A window's title changed. A window is named by what it shows: while its app names
    /// that, that name stands and the window's own title shows once it names nothing. A
    /// window's own title that is only its app's name under another spelling shows the
    /// app's name as this desk says it. The person's own name for the tab stands in front
    /// of both.
    pub fn retitle(&mut self, window: u64, title: &str) {
        let mut tree = self.handle.read(Clone::clone);
        let Some(tab) = tab_mut(&mut tree, window) else {
            return;
        };
        let app_id = match &tab.tile {
            TileContent::Foreign { app_id, .. } => app_id.clone(),
            _ => String::new(),
        };
        let name = self
            .handle
            .window_name(tab.id)
            .unwrap_or_else(|| tab_title(&self.names, &app_id, Some(title)));
        if tab.set_window_title(&name) {
            self.handle.replace_tree(tree);
        }
    }

    /// The app `window` says it belongs to changed. Its tab is that app's from here on,
    /// so the desk names the window by the program its client claims; an id the client
    /// clears leaves the tab as it is.
    pub fn set_app_id(&mut self, window: u64, app_id: &str) {
        if app_id.trim().is_empty() {
            return;
        }
        let mut tree = self.handle.read(Clone::clone);
        let Some(tab) = tab_mut(&mut tree, window) else {
            return;
        };
        if tab.set_window_app_id(app_id) {
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

    /// The tab the desktop gave `window`, once it mapped: the key a shell's well names it by
    /// (shell.md §8). A dialog has no tab; it floats over the window it belongs to.
    #[must_use]
    pub fn tab_of(&self, window: u64) -> Option<TabId> {
        self.handle.read(|tree| tab_of(tree, window))
    }

    /// The tabs the desk has for apps on the workspace in front whose window is there or
    /// on its way, each with the id its app announces itself by: what a launch of an app
    /// that may already be running is decided against. A window that mapped is here, and
    /// so is the tab a launch holds until its window fills it, so a launch of an app that
    /// is starting raises the window it will have. A tab a saved window waits in, that
    /// nothing has asked for again, is not ([`Self::waiting_tab`] is what a launch starts
    /// into).
    #[must_use]
    pub fn app_tabs_in_front(&self) -> Vec<(TabId, String)> {
        let held = self.held.borrow();
        self.handle.read(|tree| {
            let Some(workspace) = tree.active_workspace() else {
                return Vec::new();
            };
            let answered = self.crashed.borrow();
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .filter(|tab| !answered.contains_key(&tab.id))
                .filter(|tab| comes(tab, &held))
                .filter_map(|tab| {
                    let app = tilo_desktop_model::apps::canonical_app_id(&tab.tile)?;
                    Some((tab.id, app))
                })
                .collect()
        })
    }

    /// The tab a saved window of `app` waits in, nothing having asked for it again: what
    /// a launch of the app starts into, so the window the person left open takes its own
    /// tab instead of a second one beside it.
    #[must_use]
    pub fn waiting_tab(&self, app: &str) -> Option<TabId> {
        let held = self.held.borrow();
        self.handle.read(|tree| {
            let workspace = tree.active_workspace()?;
            let answered = self.crashed.borrow();
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .find(|tab| {
                    matches!(&tab.tile, TileContent::LocalApp { note, .. } if note.is_empty())
                        && !answered.contains_key(&tab.id)
                        && !comes(tab, &held)
                        && tilo_desktop_model::apps::canonical_app_id(&tab.tile)
                            .is_some_and(|id| app_ids_match(&id, app))
                })
                .map(|tab| tab.id)
        })
    }

    /// Hold `tab`, the tab a saved window of an app waits in, for the window a
    /// launch asks for now: the window takes that exact tab, and with
    /// `background` it does so without taking focus, as the restore asks it.
    /// False when the tab is gone or holds no saved window.
    pub fn adopt_tab(&mut self, tab: TabId, background: bool) -> bool {
        let Some(app) = self.handle.read(|tree| {
            tabs(tree)
                .find(|found| found.id == tab)
                .and_then(|found| tilo_desktop_model::apps::canonical_app_id(&found.tile))
        }) else {
            return false;
        };
        self.hold_tab(app, tab, background)
    }

    /// Whether a launch holds the place of `tab`: a window was asked for it
    /// and has not come yet, so nothing else asks for the same window again.
    #[must_use]
    pub fn on_its_way(&self, tab: TabId) -> bool {
        let held = self.held.borrow();
        held.values().flatten().any(|held| held.tab == Some(tab))
    }

    /// Bring the tab `tab` forward and focus it, on the next update pass: what
    /// a launch of an app whose window is there, or on its way in this tab,
    /// does instead of starting it again. False when the desk holds no such
    /// tab.
    pub fn focus_tab(&mut self, tab: TabId) -> bool {
        self.handle.focus_tab(tab)
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
            covers: self.float_covers(),
            shown,
            hidden,
            floating,
        }
    }

    /// Each window floating over the desk, and the rect the desk draws over the windows under
    /// it for that window: the whole of its bar and body, which it paints opaque, in surface
    /// coordinates. What lies under it there is drawn around it.
    #[must_use]
    pub fn float_covers(&self) -> Vec<(u64, Rect)> {
        self.handle.read(|tree| {
            self.handle
                .float_covers()
                .into_iter()
                .filter_map(|(tab, cover)| Some((window_of(tree, tab)?, cover)))
                .collect()
        })
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
        let mut answers = self.answers.borrow_mut();
        let mut titles = self.titles.borrow_mut();
        crashed.retain(|tab, app| {
            let kept = open.contains(tab);
            if !kept {
                titles.remove(app);
            }
            kept
        });
        let shown: BTreeSet<String> = crashed.values().cloned().collect();
        answers.retain(|app, _| shown.contains(app));
        closed
    }
}

/// Every window the tree holds a tab of, in tree order.
fn windows(tree: &DesktopTree) -> Vec<u64> {
    tabs(tree).filter_map(foreign).collect()
}

/// Ask for `app` again, its window taking `tab`: where the answer in the tab
/// stands, the place it holds is asked for once, so a second press does not
/// leave a place behind for a window nobody asked for.
fn ask_again(
    held: &RefCell<BTreeMap<String, Vec<HeldTab>>>,
    launches: &RefCell<Vec<LaunchAsk>>,
    app: &str,
    tab: TabId,
) {
    let mut held = held.borrow_mut();
    let queue = held.entry(app.to_owned()).or_default();
    if !queue.iter().any(|record| record.tab == Some(tab)) {
        queue.push(HeldTab {
            tab: Some(tab),
            background: false,
        });
    }
    drop(held);
    launches.borrow_mut().push(LaunchAsk {
        app: app.to_owned(),
        tab: Some(tab),
    });
}

/// What the app `app_id` is called to the person. Never the id: an app no
/// list names is written as words ([`tilo_desktop_search::names::app_name`]).
fn app_name(names: &AppNames, app_id: &str) -> String {
    let said = names.borrow().clone();
    said.map_or_else(
        || tilo_desktop_search::names::app_name(app_id),
        |name| name(app_id),
    )
}

/// What a tab showing a window of `app_id` is titled: the window's own
/// title, unless that is only the app's name under another spelling — the
/// toolkit labels a window with the app's unlocalised name — in which case
/// it is the app's name as this desk says it.
fn tab_title(names: &AppNames, app_id: &str, title: Option<&str>) -> String {
    match title.map(str::trim).filter(|title| !title.is_empty()) {
        Some(title) if !tilo_desktop_search::names::is_apps_own_name(app_id, title) => {
            title.to_owned()
        }
        _ => app_name(names, app_id),
    }
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

/// Whether a window of `tab`'s app is showing in it, or a launch holds its
/// place until one comes: the tab a window of the app is, or is on its way
/// to.
fn comes(tab: &Tab, held: &BTreeMap<String, Vec<HeldTab>>) -> bool {
    matches!(tab.tile, TileContent::Foreign { .. })
        || held.values().flatten().any(|held| held.tab == Some(tab.id))
}

/// Whether the app a window announces is the app a request named: the id
/// itself, or the short name it ends with (`so.tilo.notes` is `notes`).
#[must_use]
pub fn app_ids_match(announced: &str, asked: &str) -> bool {
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
                workspace: workspace.slot,
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
