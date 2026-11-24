//! The device's shared windows as it publishes them: which of its windows it shares with its
//! workspaces, and the windows of the workspaces' members it shows, with the verbs that share a
//! window, stop it, let a member control it and watch another member's.

use tilo_resource::Effect;

/// The shares as a view of another language reads them (`device.shares`): which of this device's
/// windows it shares with its workspaces — whether anything of each goes out, how many ask to see
/// it and whether they may control it — and the windows of its workspaces it shows.
#[derive(tilo_resource::JsonSchema)]
#[allow(dead_code, reason = "read by the schema it describes, never in Rust")]
pub struct SharedWindows {
    /// The windows this device shares.
    sharing: Vec<SharedWindow>,
    /// The windows of other members this device shows.
    watching: Vec<WatchedWindow>,
}

/// One window this device shares with a workspace.
#[derive(tilo_resource::JsonSchema)]
#[allow(dead_code, reason = "read by the schema it describes, never in Rust")]
struct SharedWindow {
    /// The tab this device shows the window in, by its decimal id.
    tab: String,
    /// What the workspace calls it: the app's own name for the window.
    app_id: String,
    /// The workspace it is shared in, by its key.
    workspace: String,
    /// The members viewing it may control it.
    input_allowed: bool,
    /// It goes out now: a member's shell asks the live session for it.
    streaming: bool,
    /// What its pictures wait for, while they wait: the workspace's live session, its app's page or
    /// its first frame. Empty while they can flow.
    waiting_for: String,
    /// The devices showing it.
    viewers: Vec<ShareViewer>,
}

/// One device showing a shared window.
#[derive(tilo_resource::JsonSchema)]
#[allow(dead_code, reason = "read by the schema it describes, never in Rust")]
struct ShareViewer {
    /// The device, by its endpoint key.
    peer: u64,
    /// The person let it control the window.
    can_control: bool,
}

/// A window of another member's this device shows.
#[derive(tilo_resource::JsonSchema)]
#[allow(dead_code, reason = "read by the schema it describes, never in Rust")]
struct WatchedWindow {
    /// The tab this device shows it in.
    tab: String,
    /// The workspace it is shared in.
    workspace: String,
    /// The device sharing it.
    host: String,
    /// The tab that pictures it.
    shown_in: String,
    /// What its picture waits for, while it waits.
    waiting_for: String,
}

/// The window shares as a view of another language reads them (`device.shares`), with the verbs
/// that share a window, stop it, let a member control it, and watch another member's.
#[must_use]
pub fn shares_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<SharedWindows, ShareEffect>(
        "shares",
        "Shared windows",
    )
}

/// What a person asks of the shares, declared once: share a window with the workspace, stop it,
/// let the members viewing it control it, show another member's window, stop showing it.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(
    operation = "tilo.session.window-share",
    catalogue = "../session-core/locales"
)]
pub enum ShareEffect {
    /// Share a window with the workspace
    #[effect(kind = "undoable", label = "session-window-share-share")]
    Share {
        /// The window, by the identifier its app may be captured by; left out for the window the
        /// tab shows
        #[param(
            "The window, by the identifier its app may be captured by; left out for the window \
             the tab shows"
        )]
        window: Option<String>,
        /// The tab this device shows the window in
        #[param("The tab this device shows the window in")]
        tab: String,
        /// The app the window belongs to; left out for the shown window's app
        #[param("The app the window belongs to; left out for the shown window's app")]
        app_id: Option<String>,
        /// The workspace in which it is shared; left out for the one the tab stands in
        #[param("The workspace in which it is shared; left out for the one the tab stands in")]
        workspace: Option<String>,
    },
    /// Stop sharing the window
    #[effect(kind = "undoable", label = "session-window-share-stop")]
    Stop {
        /// The tab this device shows the window in
        #[param("The tab this device shows the window in")]
        tab: String,
    },
    /// Let the workspace control the window
    #[effect(kind = "undoable", label = "session-window-share-allow-input")]
    AllowRemoteInput {
        /// The tab this device shows the window in
        #[param("The tab this device shows the window in")]
        tab: String,
        /// Whether they may control it; left out to toggle
        #[param("Whether they may control it; left out to toggle")]
        on: Option<bool>,
    },
    /// Show another member's shared window
    #[effect(kind = "undoable", label = "session-window-share-watch")]
    Watch {
        /// The tab this device shows the window in
        #[param("The tab this device shows the window in")]
        tab: String,
        /// The workspace in which it is shared
        #[param("The workspace in which it is shared")]
        workspace: String,
    },
    /// Stop showing it
    #[effect(kind = "undoable", label = "session-window-share-unwatch")]
    Unwatch {
        /// The tab this device shows the window in
        #[param("The tab this device shows the window in")]
        tab: String,
        /// The workspace in which it is shared
        #[param("The workspace in which it is shared")]
        workspace: String,
    },
}
