//! The shell's own state, published: what the shell shows over its windows, as
//! readings and as the verbs that change it.
//!
//! The shell keeps no view of this. A window's menu is a reading — the
//! window it is for, where it stands, and the rows the shell offers that
//! window — and three verbs: open it (the window in front's, or one named),
//! close it, and choose a row, which sends the same command the shell would
//! send from anywhere. Whoever draws the menu — this desktop's own view, a
//! person's replacement, an agent — reads the same reading through the same
//! API and invokes the same verbs, so the menu a person sees and the menu a
//! script drives are one thing.
//!
//! The same service publishes the surface stack — `desktop:overlays` — which
//! says what stands over the windows and in what order, and the keyboard's
//! target, `desktop:focus`, which derives from it. A surface is opened and
//! closed by name, with the place it hangs at when it is anchored to one.
//!
//! What is kept here is only what a reading cannot derive: which window the
//! menu is for and where the person put it. The rows themselves are the
//! shell's, asked for on every read, so a menu left open while its window
//! changes offers what that window offers now.

use std::rc::Rc;

use serde::Serialize;
use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ActionOutput, ApiError, ResourceSpec};
use tilo_desktop_model::TabId;
use tilo_session_core::desktop::emoji::EmojiLink;
use tilo_session_core::desktop::overlays::{Anchor as Place, Overlay, Part, SwitcherScope};
use tilo_session_core::desktop::state::DesktopHandle;
use tilo_session_core::desktop::super_peek::SuperPeek;

use crate::device_lists::Surfaces;
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};
use tilo_session_core::desktop::navigation::active_route;
use tilo_session_core::desktop::state::{WindowMenuRow, window_menu};
use ui::shortcut_table::{KeyChord, ShortcutAction};

/// The window menu's resource in the session's catalogue.
pub const WINDOW_MENU: &str = "desktop:window-menu";

/// The window menu as the shell publishes it: a reading of the window it is for and the rows the
/// shell offers it, with the verbs that change it.
#[derive(tilo_resource::Row)]
#[row(kind = "desktop.window-menu", label = "Window menu", effects = ShellEffect)]
pub struct WindowMenu;

/// What kind of resource [`WINDOW_MENU`] is.
pub const WINDOW_MENU_KIND: &str = <WindowMenu as tilo_resource::words::RowType>::KIND;

/// The part the window menu is drawn in, as the settings reading names it.
pub const WINDOW_MENU_PART: &str = "window-menu";

/// The app a tilolang view of the menu reads it as: the first part of its kind, which is what a
/// package names in `use desktop as desktop` .
pub const WINDOW_MENU_APP: &str = "desktop";

/// The window menu's surface, as a tilolang package drawing it is compiled against: the fields
/// [`WINDOW_MENU`] reads and the verbs that change it, in the vocabulary a surface file declares
/// (`tilo_actions::surface`, see `docs/developers/part-views.md` ). A view reads the menu through
/// this and invokes it through this; the session publishes the same fields and verbs on its API.
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

/// The surface stack as the shell publishes it: what stands over the windows and in what order.
#[derive(tilo_resource::Row)]
#[row(kind = "desktop.overlays", label = "Surfaces", effects = OverlayEffect)]
pub struct Overlays;

/// What kind of resource [`OVERLAYS`] is.
pub const OVERLAYS_KIND: &str = <Overlays as tilo_resource::words::RowType>::KIND;

/// What holds the keyboard, in the session's catalogue: the window in front,
/// or the surface over it.
pub const FOCUS: &str = "desktop:focus";

/// The keyboard's target as the shell publishes it: the surface a key goes to.
#[derive(tilo_resource::Row)]
#[row(kind = "desktop.focus", label = "Keyboard focus")]
pub struct KeyboardFocus;

/// What kind of resource [`FOCUS`] is.
pub const FOCUS_KIND: &str = <KeyboardFocus as tilo_resource::words::RowType>::KIND;

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
    /// Where the person pressed, or the window's own corner when the verb named no place.
    at: Option<(f64, f64)>,
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
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "menu")]
    Open {
        /// The window to open the menu for, by its tab's id.
        #[param("The window's tab, decimal; the window in front by default")]
        tab: Option<String>,
        /// Where to put the menu, as `x,y` in the shell's coordinates.
        #[param("Where to open it, as \"x,y\"; under the window's corner by default")]
        anchor: Option<String>,
    },
    /// Close the window menu
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Close,
    /// Choose a row
    /// Sends the command the row means, as the shell sends it from anywhere.
    #[effect(kind = "outside")]
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
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "layers")]
    OpenSurface {
        /// The surface, as the reading names it.
        #[param("The surface to open, as the reading names it")]
        part: String,
        /// Where to hang it, as the reading gives it back: `at:x,y` over the thing it acts on,
        /// `beside:x0,y0,x1,y1,gap` clear of it. Its own place by default.
        #[param("Where to hang it, as \"at:x,y\" or \"beside:x0,y0,x1,y1,gap\"")]
        anchor: Option<String>,
    },
    /// Close a surface
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    CloseSurface {
        /// The surface, as the reading names it.
        #[param("The surface to close, as the reading names it")]
        part: String,
    },
    /// Show the surface `surface` over the shell's windows, or close it when it is up.
    #[effect(kind = "outside")]
    ShowSurface {
        #[param(
            "The surface: `none`, `overview`, `switcher`, `launcher`, `centre`, `quick-settings`, `workspaces`, `composer`, `conversation` or `history`"
        )]
        surface: String,
    },
    /// Open the window switcher, or walk it `steps` windows on.
    /// Alt+Tab walks the workspace's windows; Alt and the grave key the ones the app in front
    /// shows.
    #[effect(kind = "outside")]
    WalkSwitcher {
        #[param("How far to walk; 1 the next window, -1 the one before")]
        steps: Option<i64>,
        #[param(
            "Which windows the walk crosses: `workspace` for every window of the workspace, `app` for the ones the app in front shows",
            choices = crate::device_lists::SWITCHER_SCOPES
        )]
        scope: Option<String>,
    },
    /// Choose the window the walk reached, without crossing the ones between: `at` is its place
    /// among the windows the walk crosses, `0` the first of them and `-1` the last. Home, End and a
    /// card under the pointer choose by place.
    #[effect(kind = "outside")]
    SelectSwitcher {
        #[param("The window's place among the ones the walk crosses; -1 for the last")]
        at: i64,
    },
    /// Go to the window the switcher's walk reached, and close the switcher.
    #[effect(kind = "outside")]
    CommitSwitcher,
    /// Write the picked emoji into the field the person types in, and close the picker.
    #[effect(kind = "outside")]
    #[effect(own_device)]
    PickEmoji {
        /// The emoji the person picked.
        #[param("The emoji to write into the field the person types in")]
        text: String,
    },
    /// Close the emoji picker without writing anything.
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    CloseEmoji,
    /// Answer the question waiting on the person's avatar
    #[effect(kind = "outside")]
    #[effect(row, label = "session-answer-ask")]
    AnswerAsk,
    /// Put the question waiting on the person's avatar off for now
    #[effect(kind = "outside")]
    #[effect(row, label = "session-ask-later")]
    PutOffAsk,
}

/// The surface stack as a view of another language reads it: the surface the shell shows over
/// the windows and the switcher's walk.
#[must_use]
pub fn overlays_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<OverlaysSurface, OverlayEffect>(
        "overlays", "Overlays",
    )
}

/// What [`overlays_surface`] describes of the stack's reading.
#[derive(tilo_resource::JsonSchema)]
#[allow(dead_code, reason = "read by the schema it describes, never in Rust")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a surface of its own, read by the view it is for"
)]
struct OverlaysSurface {
    /// The surface the shell shows over the windows: `none` at rest, or `overview`, `switcher`,
    /// `launcher`, `centre`, `quick-settings`, `workspaces`, `composer`, `conversation` or
    /// `history`.
    here: String,
    /// Whether the switcher's panel shows: a quick Alt+Tab switches windows and draws nothing.
    switcher_revealed: bool,
    /// Which windows the switcher's walk crosses: `workspace` for every window, `app` for the
    /// windows of the app in front.
    switcher_scope: String,
    /// What the person last typed in the shell's search: the launcher opens where they left it.
    spotlight: String,
    /// The app the history is narrowed to, by its id, when it was opened on one app's recents;
    /// empty for none.
    history_app: String,
    /// The type the history is narrowed to, as its app publishes it; empty for none.
    history_kind: String,
    /// The question the session puts to the person about themselves; empty when none waits.
    ask: String,
    /// What answering the question brings.
    ask_detail: String,
    /// The question's answer, as its button says it.
    ask_answer: String,
    /// Whether the question stands open on the person's avatar, rather than as its dot.
    ask_open: bool,
    /// Whether the About sheet stands over the windows: what this computer is, and which Tilo it
    /// runs.
    about: bool,
    /// Whether the emoji picker stands over the windows: the card that writes each pick into the
    /// field the person types in.
    emoji: bool,
    /// The keys a held Super reaches, while it is held: each group's title and its rows. Nothing
    /// when Super is not held.
    peek: Option<Peek>,
    /// What this computer's About sheet says: its name, the Tilo it runs, and where to read more.
    about_sheet: Option<AboutSheet>,
    /// Whether the strips list stands under the shell's bar.
    strips_open: bool,
}

/// What a held Super reaches, as the shell's card shows it: the groups of keys, in reading order.
#[derive(Debug, Serialize, tilo_resource::JsonSchema)]
struct Peek {
    groups: Vec<PeekGroup>,
}

/// One group of the peek: its title, and the keys under it.
#[derive(Debug, Serialize, tilo_resource::JsonSchema)]
struct PeekGroup {
    title: String,
    hints: Vec<PeekHint>,
}

/// One hint of the peek: the keys of a chord, and what pressing them does.
#[derive(Debug, Serialize, tilo_resource::JsonSchema)]
struct PeekHint {
    keys: Vec<String>,
    label: String,
}

/// What the About sheet says, as the session reads it from the Tilo this computer runs.
#[derive(Clone, Debug, Serialize, tilo_resource::JsonSchema)]
struct AboutSheet {
    title: String,
    /// This computer's name: what the sheet leads with, when it has one.
    heading: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    version: String,
    /// Tilo's home on the web, as the sheet links it.
    #[serde(skip_serializing_if = "String::is_empty")]
    home: String,
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
    name: String,
}

/// Where a menu stands, in the shell's coordinates.
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a surface of its own, read by the view it is for"
)]
struct StackReading {
    stack: Vec<Surface>,
    here: &'static str,
    switcher_revealed: bool,
    switcher_scope: &'static str,
    spotlight: String,
    history_app: String,
    history_kind: String,
    ask: String,
    ask_detail: String,
    ask_answer: String,
    ask_open: bool,
    about: bool,
    emoji: bool,
    peek: Option<Peek>,
    about_sheet: Option<AboutSheet>,
    strips_open: bool,
}

/// One open surface.
#[derive(Debug, Serialize)]
struct Surface {
    part: &'static str,
    role: &'static str,
    anchor: String,
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
    /// `window` , or `overlay:<part>` .
    target: String,
    /// The surface, while one holds it.
    surface: Option<&'static str>,
}

/// The surface `text` names, as the readings and the verbs spell it.
fn named(text: &str) -> Result<Part, ApiError> {
    Part::named(text.trim()).ok_or_else(|| ApiError::Unavailable {
        code: "unknown-surface".into(),
        reason: format!("{text} is not a surface"),
    })
}

/// The shell's resources, read from the shell and from the state the session keeps beside it.
pub struct ShellService {
    handle: DesktopHandle,
    state: ShellState,
    /// The surfaces over the shell: what stands where, which holds the keyboard, what a press
    /// outside one closes, the shell's own surface and the switcher's walk.
    surfaces: Surfaces,
    /// What the stack's reading was last published from: the shell's surface, whether the
    /// switcher's panel shows, and the stack's own revision.
    seen: Option<(u64, Overlay, bool, u64, bool, u64, bool)>,
    /// The desktop's one shortcut table, so a menu row shows the chord a press doing the same thing
    /// would be, as the person binds it now.
    shortcuts: crate::panels::mount::Shortcuts,
    /// The keys a held Super reaches, as the shell's card shows them.
    peek: SuperPeek,
    /// The picker's picks, written into the field the person types in beside dictation.
    emoji: EmojiLink,
    /// What this computer's About sheet says.
    about: AboutSheet,
}

impl ShellService {
    /// The shell over `handle` , publishing the surfaces in `overlays` .
    #[must_use]
    pub(crate) fn new(
        handle: DesktopHandle,
        surfaces: Surfaces,
        shortcuts: crate::panels::mount::Shortcuts,
        peek: SuperPeek,
        emoji: EmojiLink,
    ) -> Self {
        let copy = tilo_session_core::desktop::about::about_copy(
            tilo_session_core::desktop::about::AboutVariant::Native,
        );
        Self {
            handle,
            state: ShellState::default(),
            surfaces,
            seen: None,
            shortcuts,
            peek,
            emoji,
            about: AboutSheet {
                title: copy.title,
                heading: copy.heading.unwrap_or_default(),
                version: copy.version,
                home: copy.home,
            },
        }
    }

    /// Mount it as one of the session's services, publishing the surfaces in `surfaces` .
    pub(crate) fn mount(
        services: &Rc<std::cell::RefCell<tilo_service::Services>>,
        handle: &DesktopHandle,
        surfaces: Surfaces,
        shortcuts: crate::panels::mount::Shortcuts,
        peek: SuperPeek,
        emoji: EmojiLink,
    ) {
        services.borrow_mut().add(Box::new(Self::new(
            handle.clone(),
            surfaces,
            shortcuts,
            peek,
            emoji,
        )));
    }

    /// What the stack's reading is made from beside the stack itself.
    fn key(&self) -> (u64, Overlay, bool, u64, bool, u64, bool) {
        let shell = &self.surfaces.overlays;
        let views =
            u64::from(self.surfaces.emoji_open()) | (u64::from(self.surfaces.about_open()) << 1);
        (
            shell.revision(),
            shell.here(),
            shell.revealed(tilo_clock::now()),
            self.handle.personal_ask_revision(),
            self.peek.is_up(),
            views,
            self.surfaces.overlays.is_open(Part::Strips),
        )
    }
    /// The tab of the window in front: the active group's, in the strip on the display the person
    /// works on.
    fn focused(&self) -> Option<TabId> {
        self.handle
            .read(|tree| active_route(tree).map(|ids| ids.tab))
    }

    /// Where the menu stands on the display its surface covers, which draws from its own corner.
    fn anchor(&self, at: Option<(f64, f64)>) -> Option<Anchor> {
        let (x, y) = at?;
        let corner = self
            .surfaces
            .overlays
            .place(Part::WindowMenu)
            .map_or(ui::kurbo::Point::ZERO, |placement| placement.room.origin());
        Some(Anchor {
            x: x - corner.x,
            y: y - corner.y,
        })
    }

    /// `text` as a point on the shell.
    fn point(text: &str) -> Option<(f64, f64)> {
        let (x, y) = text.split_once(',')?;
        Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
    }

    /// Open the menu of `tab` (or of the window in front), or close it when it is already up for
    /// that window.
    fn open(&mut self, tab: Option<&str>, anchor: Option<&str>) -> Result<(), ApiError> {
        let tab = match tab.map(str::trim) {
            Some(text) if !text.is_empty() => text.parse().map_err(|_| ApiError::Unavailable {
                code: "unknown-window".into(),
                reason: format!("{text} is not a window"),
            })?,
            _ => self.focused().ok_or_else(|| ApiError::Unavailable {
                code: "no-window".into(),
                reason: "No window is in front".into(),
            })?,
        };
        if self.state.menu_tab() == Some(tab) && anchor.is_none() {
            self.close();
            return Ok(());
        }
        let anchor = anchor.map(|text| {
            Self::point(text).ok_or_else(|| ApiError::Unavailable {
                code: "unknown-anchor".into(),
                reason: format!("{text} is not a place on the shell"),
            })
        });
        let anchor = match anchor {
            Some(anchor) => Some(anchor?),
            None => None,
        };
        if self.handle.window_menu(tab).is_empty() {
            return Err(ApiError::Unavailable {
                code: "no-rows".into(),
                reason: "That window offers nothing".into(),
            });
        }
        // The display holding that point is the one the menu opens on.
        let at = anchor.or_else(|| {
            self.handle
                .window_rect(tab)
                .map(|window| (window.x0, window.y0))
        });
        self.state.menu = Some(OpenMenu { tab, at });
        self.surfaces.overlays.open(
            Part::WindowMenu,
            at.map(|(x, y)| Place::At(ui::kurbo::Point::new(x, y))),
        );
        Ok(())
    }

    /// Close the menu, if one is up.
    fn close(&mut self) {
        self.state.menu = None;
        self.surfaces.overlays.close(Part::WindowMenu);
    }

    /// Open a surface the session itself puts up, at `anchor` or where it
    /// stands by itself. The desktop's own chrome keeps the rest: its menus,
    /// the picker, the overview and the strip ribbon open and close with the
    /// widgets that draw them, and say so to the same stack.
    fn open_surface(&self, part: &str, anchor: Option<&str>) -> Result<(), ApiError> {
        let part = named(part)?;
        if !part.session_closes() {
            return Err(ApiError::Unavailable {
                code: "not-mine".into(),
                reason: format!(
                    "The desktop shows {} itself, in the widget that owns it",
                    part.as_str()
                ),
            });
        }
        if part == Part::WindowMenu {
            return Err(ApiError::Unavailable {
                code: "use-window-menu".into(),
                reason: "The window menu opens from \"desktop:window-menu\", which names \
                         the window it is for"
                    .into(),
            });
        }
        let anchor = anchor
            .map(|text| {
                Place::parse(text).ok_or_else(|| ApiError::Unavailable {
                    code: "unknown-anchor".into(),
                    reason: format!("{text} is not a place to hang a surface"),
                })
            })
            .transpose()?;
        self.surfaces.overlays.open(part, anchor);
        Ok(())
    }

    /// Close a surface the session itself puts up.
    fn close_surface(&mut self, part: &str) -> Result<(), ApiError> {
        // The held Super's keys are a picture, and its hold ends it: there is nothing to close.
        if part.trim() == crate::device_lists::PEEK {
            return Ok(());
        }
        let part = named(part)?;
        if !part.session_closes() {
            return Err(ApiError::Unavailable {
                code: "not-mine".into(),
                reason: format!(
                    "The desktop shows {} itself, in the widget that owns it",
                    part.as_str()
                ),
            });
        }
        if part == Part::WindowMenu {
            // The menu keeps its window and place beside the stack; both go together.
            self.close();
        }
        self.surfaces.overlays.close(part);
        Ok(())
    }

    /// Choose row `row` of the open menu, as the shell would send it.
    fn choose(&mut self, row: &str) -> Result<(), ApiError> {
        let Some(menu) = self.state.menu.clone() else {
            return Err(ApiError::Unavailable {
                code: "closed".into(),
                reason: "The window menu is not open".into(),
            });
        };
        match self.handle.choose_window_menu(menu.tab, row, None) {
            Ok(()) => {
                self.close();
                Ok(())
            }
            Err(reason) => Err(ApiError::Unavailable {
                code: "row-refused".into(),
                reason,
            }),
        }
    }
}

impl Service for ShellService {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![
            offer::<ShellEffect>(
                tilo_resource::LiveRow::typed::<WindowMenu>(
                    WINDOW_MENU,
                    crate::i18n::t("session-resource-window-menu"),
                )
                .into_resource(),
            ),
            offer::<OverlayEffect>(
                tilo_resource::LiveRow::typed::<Overlays>(
                    OVERLAYS,
                    crate::i18n::t("session-resource-surfaces"),
                )
                .into_resource(),
            ),
            tilo_resource::LiveRow::typed::<KeyboardFocus>(
                FOCUS,
                crate::i18n::t("session-resource-keyboard-focus"),
            )
            .into_resource(),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        match resource {
            WINDOW_MENU => Some(reading(&self.window_menu())),
            OVERLAYS => Some(reading(&self.stack())),
            FOCUS => {
                let target = crate::chrome::keyboard_target_in(
                    &self.handle,
                    &self.surfaces.overlays,
                    self.surfaces.shell,
                );
                Some(reading(&Focus {
                    target: target.as_str(),
                    surface: target.part().map(Part::as_str),
                }))
            }
            _ => None,
        }
    }

    fn poll(&mut self) -> bool {
        let now = self.key();
        if self.seen.as_ref() == Some(&now) {
            return false;
        }
        self.seen = Some(now);
        true
    }

    /// The instant the switcher's panel comes up while Alt is held on it.
    fn wakes_in(&self) -> Option<std::time::Duration> {
        self.surfaces
            .overlays
            .deadline(tilo_clock::now())
            .map(|at| at.saturating_duration_since(tilo_clock::now()))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
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
                    // A surface the shell draws opens through the shell's surfaces; the ones the
                    // session puts up in the stack itself (quick settings) keep their place there.
                    OverlayEffect::OpenSurface { part, anchor: None }
                        if Overlay::named(&part).is_some()
                            && !Part::named(&part).is_some_and(Part::session_closes) =>
                    {
                        self.surfaces.open(&part);
                    }
                    OverlayEffect::OpenSurface { part, anchor } => {
                        self.open_surface(&part, anchor.as_deref())?;
                    }
                    OverlayEffect::CloseSurface { part } if Overlay::named(&part).is_some() => {
                        self.surfaces.close(&part);
                        if Part::named(&part).is_some_and(Part::session_closes) {
                            self.close_surface(&part)?;
                        }
                    }
                    OverlayEffect::CloseSurface { part } => self.close_surface(&part)?,
                    OverlayEffect::ShowSurface { surface } => self.surfaces.toggle(&surface),
                    OverlayEffect::WalkSwitcher { steps, scope } => {
                        crate::windows::walk_switcher(
                            &self.handle,
                            &self.surfaces.overlays,
                            scope
                                .as_deref()
                                .and_then(SwitcherScope::named)
                                .unwrap_or_default(),
                            steps.unwrap_or(1),
                        );
                    }
                    OverlayEffect::SelectSwitcher { at } => {
                        crate::windows::select_switcher(&self.handle, &self.surfaces.overlays, at);
                    }
                    OverlayEffect::CommitSwitcher => {
                        crate::windows::commit_switcher(&self.handle, &self.surfaces.overlays);
                    }
                    OverlayEffect::PickEmoji { text } => {
                        self.emoji.note_picked(text);
                        self.surfaces.close(crate::device_lists::EMOJI);
                    }
                    OverlayEffect::CloseEmoji => {
                        self.surfaces.close(crate::device_lists::EMOJI);
                    }
                    OverlayEffect::AnswerAsk => self.handle.answer_personal_ask(),
                    OverlayEffect::PutOffAsk => self.handle.put_off_personal_ask(),
                }
            }
            _ => return Err(ApiError::Unknown(ActionAddress::new(resource, action))),
        }
        Ok(Started::Done(ActionOutput::default()))
    }
}

impl ShellService {
    /// The surface stack as it reads now, with the question waiting on the person's avatar.
    fn stack(&self) -> StackReading {
        let ask = self.handle.personal_ask();
        StackReading {
            stack: self
                .surfaces
                .overlays
                .entries()
                .into_iter()
                .map(|entry| Surface {
                    part: entry.part.as_str(),
                    role: entry.role.as_str(),
                    anchor: entry.anchor.as_str(),
                    size: Extent {
                        width: entry.size.width,
                        height: entry.size.height,
                    },
                    place: entry.place.map(|placement| Area {
                        x: placement.room.x0,
                        y: placement.room.y0,
                        w: placement.room.width(),
                        h: placement.room.height(),
                    }),
                })
                .collect(),
            here: if self.surfaces.emoji_open() {
                crate::device_lists::EMOJI
            } else if self.surfaces.about_open() {
                crate::device_lists::ABOUT
            } else if self.peek.is_up() && self.surfaces.overlays.here() == Overlay::Rest {
                // The held-Super card stands over the windows while the shell is otherwise at
                // rest: a picture, which the part draws over the windows without taking keys.
                crate::device_lists::PEEK
            } else {
                self.surfaces.overlays.here().name()
            },
            switcher_revealed: self.surfaces.overlays.revealed(tilo_clock::now()),
            switcher_scope: self.surfaces.overlays.scope().name(),
            spotlight: self.surfaces.overlays.spotlight(),
            history_app: self.surfaces.overlays.history_scope().0,
            history_kind: self.surfaces.overlays.history_scope().1,
            ask_open: ask.as_ref().is_some_and(|ask| ask.open),
            ask: ask
                .as_ref()
                .map(|ask| ask.title.clone())
                .unwrap_or_default(),
            ask_detail: ask.as_ref().map(|ask| ask.body.clone()).unwrap_or_default(),
            ask_answer: ask.map(|ask| ask.action.title).unwrap_or_default(),
            about: self.surfaces.about_open(),
            emoji: self.surfaces.emoji_open(),
            peek: self.peek_card(),
            about_sheet: Some(self.about.clone()),
            strips_open: self.surfaces.overlays.is_open(Part::Strips),
        }
    }

    /// The keys a held Super reaches, while it is held: the groups the session put up when the
    /// hold began, as the chrome's own card reads them, so the shell shows exactly the glance the
    /// session meant.
    fn peek_card(&self) -> Option<Peek> {
        let groups = self.peek.groups()?;
        Some(Peek {
            groups: groups
                .into_iter()
                .map(|(title, hints)| PeekGroup {
                    title,
                    hints: hints
                        .into_iter()
                        .map(|hint| PeekHint {
                            keys: hint.keys,
                            label: hint.label,
                        })
                        .collect(),
                })
                .collect(),
        })
    }

    /// The window menu as the shell offers it now.
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
            anchor: self.anchor(open.at),
            subject: self
                .handle
                .window_subject(tab)
                .map(|subject| Subject { name: subject.name }),
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
            window_menu::MOVE_LEFT => ShortcutAction::MoveGroupLeft,
            window_menu::MOVE_RIGHT => ShortcutAction::MoveGroupRight,
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
/// catalogue binds one, so a row reads the key the shell advertises rather than every alternative at
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
