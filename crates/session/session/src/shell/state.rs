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
//! The same service publishes the surface stack — `desktop:overlays` — which
//! says what stands over the windows and in what order, and the keyboard's
//! target, `desktop:focus`, which derives from it. A surface is opened and
//! closed by name, with the place it hangs at when it is anchored to one.
//!
//! What is kept here is only what a reading cannot derive: which window the
//! menu is for and where the person put it. The rows themselves are the
//! desk's, asked for on every read, so a menu left open while its window
//! changes offers what that window offers now.

use std::rc::Rc;

use serde::Serialize;
use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ActionOutput, DoorError, ResourceSpec};
use tilo_desktop::overlays::{Anchor as Place, Overlays, Slot};
use tilo_desktop_model::TabId;
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::navigation::active_route;
use tilo_desktop_surface::surface::WindowMenuRow;
use tilo_desktop_surface::surface::window_menu;
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};
use ui::shortcut_table::{KeyChord, ShortcutAction};

/// The window menu's resource in the session's catalogue.
pub const WINDOW_MENU: &str = "desktop:window-menu";

/// What kind of resource [`WINDOW_MENU`] is.
pub const WINDOW_MENU_KIND: &str = "desktop.window-menu";

/// The slot the window menu is drawn in, as the settings reading names it.
pub const WINDOW_MENU_SLOT: &str = "window-menu";

/// The app a tilolang view of the menu reads it as: the first part of its kind, which is what a
/// package names in `use desktop as desktop` .
pub const WINDOW_MENU_APP: &str = "desktop";

/// The window menu's surface, as a tilolang package drawing it is compiled against: the fields
/// [`WINDOW_MENU`] reads and the verbs that change it, in the vocabulary a surface file declares
/// (`tilo_actions::surface`, see `docs/developers/slot-views.md` ). A view reads the menu through
/// this and invokes it through this; the session publishes the same fields and verbs on its door.
pub const WINDOW_MENU_SURFACE: &str = r#"
[[resources]]
kind = "desktop.window-menu"
label = "Window menu"
version = 1
fields = [
  { path = "tab", type = { option = "text" } },
  { path = "anchor", type = { option = { named = "Anchor" } } },
  { path = "subject", type = { option = { named = "Subject" } } },
  { path = "rows", type = { collection = { named = "Row" } } },
]

[[resources.types]]
name = "Anchor"
fields = [
  { name = "x", type = "float" },
  { name = "y", type = "float" },
]

[[resources.types]]
name = "Subject"
fields = [
  { name = "title", type = "text" },
  { name = "app", type = { option = "text" } },
]

[[resources.types]]
name = "Row"
fields = [
  { name = "id", type = "text" },
  { name = "label", type = "text" },
  { name = "enabled", type = "bool" },
  { name = "reason", type = { option = "text" } },
  { name = "shortcut", type = { option = "text" } },
  { name = "icon", type = { option = "text" } },
  { name = "children", type = { list = { named = "Row" } } },
  { name = "separator", type = "bool" },
]

[[resources.verbs]]
id = "open"
label = "Open the window menu"
kind = "outside"
params = [
  { name = "tab", required = false, type = { option = "text" } },
  { name = "anchor", required = false, type = { option = "text" } },
]

[[resources.verbs]]
id = "close"
label = "Close the window menu"
kind = "outside"

[[resources.verbs]]
id = "choose"
label = "Choose a row"
kind = "outside"
params = [{ name = "row", required = true, type = "text" }]
"#;

use tilo_session_core::shell::OVERLAYS;

/// What kind of resource [`OVERLAYS`] is.
pub const OVERLAYS_KIND: &str = "desktop.overlays";

/// What holds the keyboard, in the session's catalogue: the window in front,
/// or the surface over it.
pub const FOCUS: &str = "desktop:focus";

/// What kind of resource [`FOCUS`] is.
pub const FOCUS_KIND: &str = "desktop.focus";

/// The window menu's state, as the session holds it: the rest of the shell's presentation — focus,
/// the surface stack, which strip each display shows — is in the stack beside it.
#[derive(Debug, Default)]
pub struct ShellState {
    /// The window whose menu is up, and where it stands: `None` while no menu is up.
    menu: Option<OpenMenu>,
}

/// A window menu that is up.
#[derive(Clone, Debug)]
struct OpenMenu {
    /// The tab whose rows the menu shows.
    tab: TabId,
    /// Where the person put it, as `x,y` in the desk's coordinates; the window's own corner when
    /// the verb named none.
    anchor: Option<(f64, f64)>,
}

impl ShellState {
    /// The tab whose window menu is up.
    #[must_use]
    pub fn menu_tab(&self) -> Option<TabId> {
        self.menu.as_ref().map(|menu| menu.tab)
    }
}

/// What the shell offers: the window menu's rows, and the verbs that open,
/// close and choose from it.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.desktop.window-menu")]
pub enum ShellEffect {
    /// Open the window menu
    /// The window in front's, or the one named; the same verb closes a menu it finds open for that
    /// window.
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
    /// Sends the command the row means, as the desk sends it from anywhere.
    #[effect(own_device)]
    Choose {
        /// The row's id, as the reading lists it.
        #[param("The row to choose")]
        row: String,
    },
}

/// What the surface stack offers: opening and closing a surface by name.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.desktop.overlays")]
pub enum OverlayEffect {
    /// Open a surface
    /// Named as the reading names it; the session's own surfaces only. A surface already up moves
    /// to where it is asked to stand.
    #[effect(own_device, icon = "layers")]
    Open {
        /// The surface, as the reading names it.
        #[param("The surface to open, as the reading names it")]
        slot: String,
        /// Where to hang it, as the reading gives it back: `at:x,y` over the thing it acts on,
        /// `beside:x0,y0,x1,y1,gap` clear of it. Its own place by default.
        #[param("Where to hang it, as \"at:x,y\" or \"beside:x0,y0,x1,y1,gap\"")]
        anchor: Option<String>,
    },
    /// Close a surface
    #[effect(own_device, idempotent)]
    Close {
        /// The surface, as the reading names it.
        #[param("The surface to close, as the reading names it")]
        slot: String,
    },
}

/// The window menu as it reads: the window it is for, where it stands, and what that window offers
/// now.
#[derive(Debug, Serialize)]
struct Reading {
    tab: Option<String>,
    anchor: Option<Anchor>,
    /// What the menu is on, when it is up: the window, named as the person knows it, so typing into
    /// the menu can be about it.
    #[serde(skip_serializing_if = "Option::is_none")]
    subject: Option<Subject>,
    rows: Vec<Row>,
}

/// The window a menu is on, as a person names it.
#[derive(Debug, Serialize)]
struct Subject {
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    app: Option<String>,
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
    /// The chord that does the same thing, as the desktop's table binds it now: what a person sees
    /// at the row's end.
    #[serde(skip_serializing_if = "Option::is_none")]
    shortcut: Option<String>,
    /// The picture of the command the row runs, by its Lucide name, as the window's own controls
    /// draw it.
    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    /// The rows a row that opens a submenu offers: none for a plain row,
    /// which says so rather than leaving the field out, as its type declares
    /// it.
    children: Vec<Row>,
    /// A rule between two groups of rows, which chooses nothing.
    separator: bool,
}

/// The surface stack as it reads: what stands over the windows, topmost first.
#[derive(Debug, Serialize)]
struct Stack {
    stack: Vec<Surface>,
}

/// One open surface.
#[derive(Debug, Serialize)]
struct Surface {
    slot: &'static str,
    role: &'static str,
    anchor: String,
    modality: &'static str,
    owner: String,
    lifecycle: &'static str,
    size: Extent,
    /// Where the host places it, for a surface whose anchor and size settle a rect. A surface its
    /// own view places has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    place: Option<Area>,
}

/// How much of a display a surface takes, in logical pixels.
#[derive(Debug, Serialize)]
struct Extent {
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<f64>,
}

/// Where a surface stands.
#[derive(Debug, Serialize)]
struct Area {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// What holds the keyboard.
#[derive(Debug, Serialize)]
struct Focus {
    /// `window` , or `overlay:<slot>` .
    target: String,
    /// The surface, while one holds it.
    surface: Option<&'static str>,
}

/// The surface `text` names, as the readings and the verbs spell it.
fn named(text: &str) -> Result<Slot, DoorError> {
    Slot::named(text.trim()).ok_or_else(|| DoorError::Unavailable {
        code: "unknown-surface".into(),
        reason: format!("{text} is not a surface"),
    })
}

/// The shell's resources, read from the desk and from the state the session keeps beside it.
pub struct ShellService {
    handle: DesktopHandle,
    state: ShellState,
    /// The surfaces over the desk: what stands where, which holds the keyboard, and what a press
    /// outside one closes.
    overlays: Overlays,
    /// The desktop's one shortcut table, so a menu row shows the chord a press doing the same thing
    /// would be, as the person binds it now.
    shortcuts: crate::panels::mount::Shortcuts,
}

impl ShellService {
    /// The shell over `handle` , publishing the surfaces in `overlays` .
    #[must_use]
    pub(crate) fn new(
        handle: DesktopHandle,
        overlays: Overlays,
        shortcuts: crate::panels::mount::Shortcuts,
    ) -> Self {
        Self {
            handle,
            state: ShellState::default(),
            overlays,
            shortcuts,
        }
    }

    /// Mount it as one of the session's services, publishing the surfaces in `overlays` .
    pub(crate) fn mount(
        services: &Rc<std::cell::RefCell<tilo_service::Services>>,
        handle: &DesktopHandle,
        overlays: Overlays,
        shortcuts: crate::panels::mount::Shortcuts,
    ) {
        services
            .borrow_mut()
            .add(Box::new(Self::new(handle.clone(), overlays, shortcuts)));
    }

    /// The tab of the window in front: the active group's, in the strip on the display the person
    /// works on.
    fn focused(&self) -> Option<TabId> {
        self.handle
            .read(|tree| active_route(tree).map(|ids| ids.tab))
    }

    /// Where the menu of `tab` stands: where the person put it, or the corner of its window as last
    /// laid out.
    fn anchor(&self, tab: TabId, named: Option<(f64, f64)>) -> Option<Anchor> {
        let corner = named.or_else(|| {
            self.handle
                .window_rect(tab)
                .map(|window| (window.x0, window.y0))
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

    /// Open the menu of `tab` (or of the window in front), or close it when it is already up for
    /// that window.
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
        let at = anchor.unwrap_or((0.0, 0.0));
        self.state.menu = Some(OpenMenu { tab, anchor });
        self.overlays.open(
            Slot::WindowMenu,
            Some(Place::At(ui::kurbo::Point::new(at.0, at.1))),
        );
        Ok(())
    }

    /// Close the menu, if one is up.
    fn close(&mut self) {
        self.state.menu = None;
        self.overlays.close(Slot::WindowMenu);
    }

    /// Open a surface the session itself puts up, at `anchor` or where it
    /// stands by itself. The desktop's own chrome keeps the rest: its menus,
    /// the picker, the overview and the strip ribbon open and close with the
    /// widgets that draw them, and say so to the same stack.
    fn open_surface(&self, slot: &str, anchor: Option<&str>) -> Result<(), DoorError> {
        let slot = named(slot)?;
        if !slot.session_closes() {
            return Err(DoorError::Unavailable {
                code: "not-mine".into(),
                reason: format!(
                    "The desktop shows {} itself, in the widget that owns it",
                    slot.as_str()
                ),
            });
        }
        if slot == Slot::WindowMenu {
            return Err(DoorError::Unavailable {
                code: "use-window-menu".into(),
                reason: "The window menu opens from \"desktop:window-menu\", which names \
                         the window it is for"
                    .into(),
            });
        }
        let anchor = anchor
            .map(|text| {
                Place::parse(text).ok_or_else(|| DoorError::Unavailable {
                    code: "unknown-anchor".into(),
                    reason: format!("{text} is not a place to hang a surface"),
                })
            })
            .transpose()?;
        self.overlays.open(slot, anchor);
        Ok(())
    }

    /// Close a surface the session itself puts up.
    fn close_surface(&mut self, slot: &str) -> Result<(), DoorError> {
        let slot = named(slot)?;
        if !slot.session_closes() {
            return Err(DoorError::Unavailable {
                code: "not-mine".into(),
                reason: format!(
                    "The desktop shows {} itself, in the widget that owns it",
                    slot.as_str()
                ),
            });
        }
        if slot == Slot::WindowMenu {
            // The menu keeps its window and place beside the stack; both go together.
            self.close();
        }
        self.overlays.close(slot);
        Ok(())
    }

    /// Choose row `row` of the open menu, as the desk would send it.
    fn choose(&mut self, row: &str) -> Result<(), DoorError> {
        let Some(menu) = self.state.menu.clone() else {
            return Err(DoorError::Unavailable {
                code: "closed".into(),
                reason: "The window menu is not open".into(),
            });
        };
        match self.handle.choose_window_menu(menu.tab, row, None) {
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

impl Service for ShellService {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![
            offer::<ShellEffect>(ResourceSpec::new(
                WINDOW_MENU,
                WINDOW_MENU_KIND,
                "Window menu",
            )),
            offer::<OverlayEffect>(ResourceSpec::new(OVERLAYS, OVERLAYS_KIND, "Surfaces")),
            ResourceSpec::new(FOCUS, FOCUS_KIND, "Keyboard focus"),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        match resource {
            WINDOW_MENU => Some(reading(&self.window_menu())),
            OVERLAYS => Some(reading(&Stack {
                stack: self
                    .overlays
                    .entries()
                    .into_iter()
                    .map(|entry| Surface {
                        slot: entry.slot.as_str(),
                        role: entry.role.as_str(),
                        anchor: entry.anchor.as_str(),
                        modality: entry.modality.as_str(),
                        owner: entry.owner.as_str(),
                        lifecycle: entry.lifecycle.as_str(),
                        size: Extent {
                            width: entry.size.width,
                            height: entry.size.height,
                        },
                        place: entry.place.map(|standing| Area {
                            x: standing.room.x0,
                            y: standing.room.y0,
                            w: standing.room.width(),
                            h: standing.room.height(),
                        }),
                    })
                    .collect(),
            })),
            FOCUS => {
                let target = crate::chrome::keyboard_target(&self.handle, &self.overlays);
                Some(reading(&Focus {
                    target: target.as_str(),
                    surface: target.slot().map(Slot::as_str),
                }))
            }
            _ => None,
        }
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        match resource {
            WINDOW_MENU => {
                match decode::<ShellEffect>(&self.resources(), resource, action, arguments)? {
                    ShellEffect::Open { tab, anchor } => {
                        self.open(tab.as_deref(), anchor.as_deref())?;
                    }
                    ShellEffect::Close => self.close(),
                    ShellEffect::Choose { row } => self.choose(&row)?,
                }
            }
            OVERLAYS => {
                match decode::<OverlayEffect>(&self.resources(), resource, action, arguments)? {
                    OverlayEffect::Open { slot, anchor } => {
                        self.open_surface(&slot, anchor.as_deref())?;
                    }
                    OverlayEffect::Close { slot } => self.close_surface(&slot)?,
                }
            }
            _ => return Err(DoorError::Unknown(ActionAddress::new(resource, action))),
        }
        Ok(Started::Done(ActionOutput::default()))
    }
}

impl ShellService {
    /// The window menu as the desk offers it now.
    fn window_menu(&self) -> Reading {
        let Some(open) = &self.state.menu else {
            return Reading {
                tab: None,
                anchor: None,
                subject: None,
                rows: Vec::new(),
            };
        };
        let tab = open.tab;
        Reading {
            tab: Some(tab.to_string()),
            anchor: self.anchor(tab, open.anchor),
            subject: self.handle.window_subject(tab).map(|subject| Subject {
                title: subject.title,
                app: subject.app,
            }),
            rows: self.rows(tab),
        }
    }

    /// The rows the window showing `tab` offers, each carrying the chord that does the same thing
    /// and the picture of the command behind it, its submenu's rows with them.
    fn rows(&self, tab: TabId) -> Vec<Row> {
        let mut rows: Vec<Row> = self
            .handle
            .window_menu(tab)
            .into_iter()
            .map(|row| self.reading_row(row))
            .collect();
        // A window of one tab is its tab: the chord that closes the tab closes it, so its Close row
        // says that chord. A window of several offers the tab's close beside the window's, and the
        // chord does only the tab's.
        if !rows.iter().any(|row| row.id == window_menu::CLOSE) {
            let close = fixed_chord("fixed-close-tab");
            for row in rows
                .iter_mut()
                .filter(|row| row.id == window_menu::CLOSE_WINDOW)
            {
                row.shortcut.clone_from(&close);
            }
        }
        rows
    }

    /// One row of the window menu as the reading spells it.
    fn reading_row(&self, row: WindowMenuRow) -> Row {
        let WindowMenuRow {
            id,
            label,
            enabled,
            reason,
            icon,
            children,
            separator,
        } = row;
        Row {
            shortcut: self.row_shortcut(&id),
            icon: icon.map(|icon| icon.name().to_owned()),
            children: children
                .into_iter()
                .map(|child| self.reading_row(child))
                .collect(),
            id,
            label,
            enabled,
            reason,
            separator,
        }
    }

    /// The chord a press doing what row `id` does would be, as the desktop's table binds it now:
    /// the person sees the key they would press. A row no shortcut runs says nothing, rather than a
    /// key that would do something else.
    fn row_shortcut(&self, id: &str) -> Option<String> {
        if let Some(fixed) = fixed_row_shortcut(id) {
            return fixed_chord(fixed);
        }
        let action = match id {
            window_menu::FULLSCREEN | window_menu::EXIT_FULLSCREEN => {
                ShortcutAction::ToggleFullscreen
            }
            window_menu::MAXIMIZE => ShortcutAction::MaximizeWindow,
            window_menu::RESTORE => ShortcutAction::RestoreWindow,
            window_menu::FLOAT | window_menu::SNAP_BACK => ShortcutAction::ToggleFloat,
            window_menu::SPLIT_RIGHT => ShortcutAction::DetachTab,
            window_menu::WIDER => ShortcutAction::ResizeWider,
            window_menu::NARROWER => ShortcutAction::ResizeNarrower,
            _ => return None,
        };
        self.shortcuts
            .borrow()
            .bindings
            .get(&action)
            .map(KeyChord::display)
    }
}

/// The chord the catalogue fixes for `id` , in the system's own spelling: its Super form where the
/// catalogue binds one, so a row reads the key the desk advertises rather than every alternative at
/// once.
fn fixed_chord(id: &str) -> Option<String> {
    let chords = ui::shortcut_table::fixed_shortcut(id)?.chords();
    let chord = chords
        .iter()
        .find(|chord| {
            chord
                .modifiers
                .iter()
                .any(|modifier| modifier.eq_ignore_ascii_case("super"))
        })
        .or_else(|| chords.first())?;
    Some(chord.display())
}

/// The fixed shortcut whose chord does what row `id` does, for the rows a chord that Settings
/// cannot rebind carries.
fn fixed_row_shortcut(id: &str) -> Option<&'static str> {
    Some(match id {
        window_menu::CLOSE => "fixed-close-tab",
        window_menu::NEW_TAB => "fixed-duplicate-tab",
        _ => return None,
    })
}
