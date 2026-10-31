//! The shell's own state, published: what the desk shows over its windows, as
//! readings and as the verbs that change it.
//!
//! The shell keeps no view of this. A window's menu is a reading — the
//! window it is for, where it stands, and the rows the desk offers that
//! window — and three verbs: open it (the window in front's, or one named),
//! close it, and choose a row, which sends the same command the desk would
//! send from anywhere. Whoever draws the menu — this desktop's own view, a
//! person's replacement, an agent — reads the same reading through the same
//! door and invokes the same verbs, so the menu a person sees and the menu a
//! script drives are one thing.
//!
//! What is kept here is only what a reading cannot derive: which window the
//! menu is for and where the person put it. The rows themselves are the
//! desk's, asked for on every read, so a menu left open while its window
//! changes offers what that window offers now.

use std::cell::Cell;
use std::rc::Rc;

use serde::Serialize;
use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ActionOutput, DoorError, ResourceSpec};
use tilo_desktop_model::TabId;
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::navigation::active_route;
use tilo_desktop_surface::surface::WindowMenuRow;
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};

/// The window menu's resource in the session's catalogue.
pub const WINDOW_MENU: &str = "desktop:window-menu";

/// What kind of resource [`WINDOW_MENU`] is.
pub const WINDOW_MENU_KIND: &str = "desktop.window-menu";

/// The shell's own state, as the session holds it.
///
/// It starts with the window menu: the rest of the shell's presentation —
/// focus, the overlay stack, which strip each display shows — joins it as
/// each moves behind its own verbs.
#[derive(Debug, Default)]
pub struct ShellState {
    /// The window whose menu is up, and where it stands: `None` while no
    /// menu is up.
    menu: Option<OpenMenu>,
}

/// A window menu that is up.
#[derive(Clone, Debug)]
struct OpenMenu {
    /// The tab whose rows the menu shows.
    tab: TabId,
    /// Where the person put it, as `x,y` in the desk's coordinates; the
    /// window's own corner when the verb named none.
    anchor: Option<(f64, f64)>,
}

impl ShellState {
    /// The tab whose window menu is up.
    #[must_use]
    pub fn menu_tab(&self) -> Option<TabId> {
        self.menu.as_ref().map(|menu| menu.tab)
    }
}

/// Whether the shell's window menu is up, as the verbs set it and the keys
/// read it: Escape closes it, and until it is closed no press is the window's.
#[derive(Clone, Debug, Default)]
pub struct MenuOpen(Rc<Cell<bool>>);

impl MenuOpen {
    /// Whether it is up.
    #[must_use]
    pub fn get(&self) -> bool {
        self.0.get()
    }

    fn set(&self, open: bool) {
        self.0.set(open);
    }
}

/// What the shell offers: the window menu's rows, and the verbs that open,
/// close and choose from it.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.desktop.window-menu")]
pub enum ShellEffect {
    /// Open the window menu
    ///
    /// The window in front's, or the one named; the same verb closes a menu
    /// it finds open for that window.
    #[effect(own_device, icon = "menu")]
    Open {
        /// The window to open the menu for, by its tab's id.
        #[param("The window's tab, decimal; the window in front by default")]
        tab: Option<String>,
        /// Where to put the menu, as `x,y` in the desk's coordinates.
        #[param("Where to open it, as \"x,y\"; under the window's corner by default")]
        anchor: Option<String>,
    },
    /// Close the window menu
    #[effect(own_device, idempotent)]
    Close,
    /// Choose a row
    ///
    /// Sends the command the row means, as the desk sends it from anywhere.
    #[effect(own_device)]
    Choose {
        /// The row's id, as the reading lists it.
        #[param("The row to choose")]
        row: String,
    },
}

/// The window menu as it reads: the window it is for, where it stands, and
/// what that window offers now.
#[derive(Debug, Serialize)]
struct Reading {
    tab: Option<String>,
    anchor: Option<Anchor>,
    rows: Vec<Row>,
}

/// Where a menu stands, in the desk's coordinates.
#[derive(Debug, Serialize)]
struct Anchor {
    x: f64,
    y: f64,
}

/// One row a window offers.
#[derive(Debug, Serialize)]
struct Row {
    id: String,
    label: String,
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl From<WindowMenuRow> for Row {
    fn from(row: WindowMenuRow) -> Self {
        Self {
            id: row.id,
            label: row.label,
            enabled: row.enabled,
            reason: row.reason,
        }
    }
}

/// The shell's resources, read from the desk and from the state the session
/// keeps beside it.
pub struct Shell {
    handle: DesktopHandle,
    state: ShellState,
    open: MenuOpen,
}

impl Shell {
    /// The shell over `handle`, saying whether its menu is up through
    /// `open`.
    #[must_use]
    pub fn new(handle: DesktopHandle, open: MenuOpen) -> Self {
        Self {
            handle,
            state: ShellState::default(),
            open,
        }
    }

    /// Mount it as one of the session's services, and say whether its menu
    /// is up through the `open` keys read.
    pub fn mount(
        services: &Rc<std::cell::RefCell<tilo_service::Services>>,
        handle: &DesktopHandle,
        open: MenuOpen,
    ) {
        services
            .borrow_mut()
            .add(Box::new(Self::new(handle.clone(), open)));
    }

    /// The tab of the window in front: the active group's, in the strip on
    /// the display the person works on.
    fn focused(&self) -> Option<TabId> {
        self.handle
            .read(|tree| active_route(tree).map(|ids| ids.tab))
    }

    /// Where the menu of `tab` stands: where the person put it, or the
    /// corner of its window as last laid out.
    fn anchor(&self, tab: TabId, named: Option<(f64, f64)>) -> Option<Anchor> {
        let corner = named.or_else(|| {
            self.handle
                .shown_tabs()
                .into_iter()
                .find(|(shown, _)| *shown == tab)
                .map(|(_, at)| (at.x0, at.y0))
        })?;
        Some(Anchor {
            x: corner.0,
            y: corner.1,
        })
    }

    /// `text` as a point on the desk.
    fn point(text: &str) -> Option<(f64, f64)> {
        let (x, y) = text.split_once(',')?;
        Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
    }

    /// Open the menu of `tab` (or of the window in front), or close it when
    /// it is already up for that window.
    fn open(&mut self, tab: Option<&str>, anchor: Option<&str>) -> Result<(), DoorError> {
        let tab = match tab.map(str::trim) {
            Some(text) if !text.is_empty() => text.parse().map_err(|_| DoorError::Unavailable {
                code: "unknown-window".into(),
                reason: format!("{text} is not a window"),
            })?,
            _ => self.focused().ok_or_else(|| DoorError::Unavailable {
                code: "no-window".into(),
                reason: "No window is in front".into(),
            })?,
        };
        if self.state.menu_tab() == Some(tab) && anchor.is_none() {
            self.close();
            return Ok(());
        }
        let anchor = anchor.map(|text| {
            Self::point(text).ok_or_else(|| DoorError::Unavailable {
                code: "unknown-anchor".into(),
                reason: format!("{text} is not a place on the desk"),
            })
        });
        let anchor = match anchor {
            Some(anchor) => Some(anchor?),
            None => None,
        };
        if self.handle.window_menu(tab).is_empty() {
            return Err(DoorError::Unavailable {
                code: "no-rows".into(),
                reason: "That window offers nothing".into(),
            });
        }
        self.state.menu = Some(OpenMenu { tab, anchor });
        self.open.set(true);
        Ok(())
    }

    /// Close the menu, if one is up.
    fn close(&mut self) {
        self.state.menu = None;
        self.open.set(false);
    }

    /// Choose row `row` of the open menu, as the desk would send it.
    fn choose(&mut self, row: &str) -> Result<(), DoorError> {
        let Some(menu) = self.state.menu.clone() else {
            return Err(DoorError::Unavailable {
                code: "closed".into(),
                reason: "The window menu is not open".into(),
            });
        };
        match self
            .handle
            .choose_window_menu(menu.tab, row, None, crate::GENERATION)
        {
            Ok(()) => {
                self.close();
                Ok(())
            }
            Err(reason) => Err(DoorError::Unavailable {
                code: "row-refused".into(),
                reason,
            }),
        }
    }
}

impl Service for Shell {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![offer::<ShellEffect>(ResourceSpec::new(
            WINDOW_MENU,
            WINDOW_MENU_KIND,
            "Window menu",
        ))]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        if resource != WINDOW_MENU {
            return None;
        }
        let Some(menu) = &self.state.menu else {
            return Some(reading(&Reading {
                tab: None,
                anchor: None,
                rows: Vec::new(),
            }));
        };
        let tab = menu.tab;
        Some(reading(&Reading {
            tab: Some(tab.to_string()),
            anchor: self.anchor(tab, menu.anchor),
            rows: self
                .handle
                .window_menu(tab)
                .into_iter()
                .map(Row::from)
                .collect(),
        }))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        if resource != WINDOW_MENU {
            return Err(DoorError::Unknown(ActionAddress::new(resource, action)));
        }
        match decode::<ShellEffect>(&self.resources(), resource, action, arguments)? {
            ShellEffect::Open { tab, anchor } => {
                self.open(tab.as_deref(), anchor.as_deref())?;
            }
            ShellEffect::Close => self.close(),
            ShellEffect::Choose { row } => self.choose(&row)?,
        }
        Ok(Started::Done(ActionOutput::default()))
    }
}
