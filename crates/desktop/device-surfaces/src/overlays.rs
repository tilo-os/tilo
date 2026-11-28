//! The shell's surfaces over the windows, its power menu and the question an irreversible action
//! asks, as the desktop publishes them.

use serde::Serialize;
use tilo_resource::Effect;

/// What the power menu offers: putting it up and taking it down, and the choice each of its rows
/// makes. Every row is an action of its own, so a destructive one is asked about where the person
/// presses it, as from the palette or a script.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.desktop.power-menu")]
pub enum PowerMenuEffect {
    /// Open the power menu
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "power")]
    Open,
    /// Close the power menu
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Close,
    /// Lock the screen
    #[effect(kind = "outside")]
    #[effect(own_device, label = "session-power-lock")]
    PowerLock,
    /// Log out
    #[effect(kind = "outside")]
    #[effect(own_device, destructive, label = "session-power-log-out")]
    PowerLogOut,
    /// Switch user
    #[effect(kind = "outside")]
    #[effect(own_device, label = "session-power-switch-user")]
    PowerSwitchUser,
    /// Suspend
    #[effect(kind = "outside")]
    #[effect(own_device, label = "session-power-suspend")]
    PowerSuspend,
    /// Restart
    #[effect(kind = "outside")]
    #[effect(own_device, destructive, label = "session-power-restart")]
    PowerRestart,
    /// Shut down
    #[effect(kind = "outside")]
    #[effect(own_device, destructive, label = "session-power-shut-down")]
    PowerShutDown,
}

/// What [`power_menu_surface`] describes of the menu's reading.
#[derive(tilo_resource::JsonSchema)]
#[allow(dead_code, reason = "read by the schema it describes, never in Rust")]
struct PowerMenuReadingSurface {
    /// Whether the menu stands over the windows.
    open: bool,
    /// What it offers, in the order it shows: `lock`, `log-out`, `switch-user`, `suspend`,
    /// `restart` and `shut-down`, each only where this session offers it.
    offers: Vec<String>,
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
            "The surface: `none`, `overview`, `switcher`, `launcher`, `centre`, `quick-settings`, `workspaces`, `composer`, `conversation`, `history` or `people`"
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
            choices = crate::lists::SWITCHER_SCOPES
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
    #[effect(label = "session-answer-ask")]
    AnswerAsk,
    /// Put the question waiting on the person's avatar off for now
    #[effect(kind = "outside")]
    #[effect(label = "session-ask-later")]
    PutOffAsk,
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
    /// `launcher`, `centre`, `quick-settings`, `workspaces`, `composer`, `conversation`,
    /// `history` or `people`.
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
    /// Where a surface the person opened beside a caret stands, once the stack settles its room:
    /// the line they type on, clear of it. Nothing while no surface is up, and nothing for one at
    /// its own corner, which the shell's layout places.
    place: Option<Area>,
}

/// What a held Super reaches, as the shell's card shows it: the groups of keys, in reading order.
#[derive(Debug, Serialize, tilo_resource::JsonSchema)]
pub struct Peek {
    pub groups: Vec<PeekGroup>,
}

/// One group of the peek: its title, and the keys under it.
#[derive(Debug, Serialize, tilo_resource::JsonSchema)]
pub struct PeekGroup {
    pub title: String,
    pub hints: Vec<PeekHint>,
}

/// One hint of the peek: the keys of a chord, and what pressing them does.
#[derive(Debug, Serialize, tilo_resource::JsonSchema)]
pub struct PeekHint {
    pub keys: Vec<String>,
    pub label: String,
}

/// What the About sheet says, as the session reads it from the Tilo this computer runs.
#[derive(Clone, Debug, Serialize, tilo_resource::JsonSchema)]
pub struct AboutSheet {
    pub title: String,
    /// This computer's name: what the sheet leads with, when it has one.
    pub heading: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub version: String,
    /// Tilo's home on the web, as the sheet links it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub home: String,
    /// The links under it, in reading order: where to read the code and where its people talk.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<AboutLink>,
}

/// One link the About sheet offers: what it is called, and the address it opens.
#[derive(Clone, Debug, Serialize, tilo_resource::JsonSchema)]
pub struct AboutLink {
    /// What the link is called, in the person's language.
    pub label: String,
    /// The address it opens.
    pub url: String,
}

/// Where a surface stands.
#[derive(Debug, Serialize, tilo_resource::JsonSchema)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// The power menu as a view of another language reads it.
#[must_use]
pub fn power_menu_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<PowerMenuReadingSurface, PowerMenuEffect>(
        "power_menu",
        "Power menu",
    )
}

/// The surface stack as a view of another language reads it: the surface the shell shows over
/// the windows and the switcher's walk.
#[must_use]
pub fn overlays_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<OverlaysSurface, OverlayEffect>(
        "overlays", "Overlays",
    )
}
