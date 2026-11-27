//! This device's lists as it publishes them: its windows, the wallpaper behind them, the tray,
//! the agents at the person's peer network, the workspaces' folders, and the shell's search,
//! launchpad and passing notice.

use fs::tree_sync::{Refusal, Waiting};
use tilo_resource::Effect;

/// The picture a workspace shows behind its windows, as the person's choice names it, and the
/// pictures the image ships for a person to choose from.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct Wallpaper {
    /// The value `appearance.wallpaper` holds for the workspace in front, empty for the picture
    /// the image ships.
    pub picture: String,
    /// The picture the image ships, by its id: what shows while the person chooses none.
    pub shipped: String,
    /// Every picture the image ships, in the order a picker shows them.
    pub catalogue: Vec<ShippedWallpaper>,
}

/// One picture the image ships, with what it credits and a thumbnail a view shows.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct ShippedWallpaper {
    /// The `appearance.wallpaper` value that chooses it.
    pub id: String,
    /// What the picture is called, as its source does.
    pub title: String,
    /// Who made it.
    pub author: String,
    /// The licence it is used under, by its identifier (`CC0-1.0`).
    pub license: String,
    /// Where the licence reads.
    pub license_url: String,
    /// How light it is: `light`, `mid` or `dark`.
    pub tone: String,
    /// The thumbnail, as an address a view shows (`data:image/webp;base64,...`); empty while
    /// this device has not installed the image's pictures.
    pub thumbnail: String,
}

/// The empty shell's welcome cards this device offers, as the launcher lists them.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct Launchpad {
    /// The cards, in the order the launchpad shows them.
    pub cards: Vec<LaunchpadCard>,
}

/// One welcome card: the verb it runs, what it is called, and the glyph its row shows.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct LaunchpadCard {
    /// The verb's stable name, which `launchpad` takes as its `card`.
    pub slug: String,
    /// What the card is called, in the person's language.
    pub label: String,
    /// The glyph its row shows, by the platform icon's kebab-case name.
    pub icon: String,
}

/// The shell's open search, as the launcher lists it: what is typed, whether it is still filling
/// in, and the rows it found in the order the person sees them.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct Search {
    /// What the person typed, empty for the empty field.
    pub query: String,
    /// Whether a search is open; none of the rows belong to anything while it is not.
    pub open: bool,
    /// What the open search reaches: `workspace`, `everywhere`, `app:<package id>` for one app's
    /// own search, or `new-tab` for the apps a window group can show. Empty while none is open.
    pub scope: String,
    /// Whether the rows shown are not the search's answer yet.
    pub searching: bool,
    /// Whether a source is still out, so more rows may join the ones shown.
    pub expecting: bool,
    /// The rows the person sees, grouped under the kinds they are, in the order the search ranks
    /// them.
    pub groups: Vec<SearchGroup>,
}

/// A run of the open search's rows of one kind, under their kind's name: the groups the palette
/// draws one after another.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct SearchGroup {
    /// The group's name (`Apps`, `Files`), as the search names its kinds.
    pub kind: String,
    /// The glyph its rows show, by the platform icon's kebab-case name.
    pub icon: String,
    /// Its rows, in the search's order.
    pub rows: Vec<SearchResult>,
}

/// What the shell says in passing over the windows: a short-lived notice, never a notification.
/// Empty while nothing just happened.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct Said {
    /// What happened, in a sentence.
    pub text: String,
    /// The kind it is, as its colour reads it: `info`, `success`, `warning` or `error`.
    pub kind: String,
    /// What the person can still do about it, such as Undo after a close.
    pub actions: Vec<SaidAction>,
}

/// One row the open search found: what it is called, what says what it is, and where choosing it
/// names it.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct SearchResult {
    /// Its place among all the rows, counting from zero: what choosing it names.
    pub index: i64,
    /// What the row is called.
    pub title: String,
    /// What says what it is under its name: where it lives, what it is.
    #[serde(default)]
    pub subtitle: String,
    /// What it says about the thing beyond its name, as the source put it.
    #[serde(default)]
    pub facts: Vec<String>,
}

/// One button a passing notice offers.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct SaidAction {
    /// The action's id, as pressing it names it.
    pub id: String,
    /// Its name on the button.
    pub name: String,
    /// The glyph beside it, by the platform icon's kebab-case name.
    pub icon: String,
}

/// What a passing notice offers: pressing one of its buttons, or taking it away.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.notice")]
pub enum SaidEffect {
    /// Press the notice's button
    #[effect(kind = "outside")]
    #[effect(own_device, label = "session-notice-press")]
    PressNotice {
        /// The action, as the notice lists it
        #[param("The action, as the notice lists it")]
        action: String,
    },
    /// Take the notice away without pressing anything
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent, label = "session-notice-hide")]
    Hide,
}

/// The workspaces' folders on this computer, by workspace.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct Folders {
    pub folders: Vec<Folder>,
}

/// One workspace's folder, as the person is told where it stands.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct Folder {
    /// The workspace, by its key.
    pub workspace: String,
    /// The workspace's name, empty while it has none.
    pub name: String,
    /// The name of the device that keeps the workspace's files, empty while none is named.
    pub server: String,
    /// Where the workspace's files stand on this computer, empty while they are not mounted
    /// here: the place a person opens to browse them.
    #[serde(default)]
    pub path: String,
    /// What the folder is doing: `paused`, `waiting-for-workspace`, `opening`, `stuck`,
    /// `sending`, `content-going-out` (every change went out, and the server still lacks some
    /// files' contents) or `in-sync`.
    pub state: String,
    /// Why changes made here wait, while they do, as the reason the person needs to hear first:
    /// `unopened` (the workspace hasn't said which files it holds), `unanswered` (it hasn't
    /// answered for some of them), `unarrived` (part of its file list hasn't arrived here),
    /// `ordered` (changes wait for others to arrive first), `refused` (the workspace refused some,
    /// for [`Self::refused`]), `unsent` (some are sent again) or `lacking` (files whose details
    /// nobody holds); empty otherwise.
    pub stuck: String,
    /// Why the workspace refused changes, while that is why they wait: `history-full` (the
    /// record's history there has no room until it is compacted), `not-permitted` (the
    /// workspace's rules don't let the person make them), `unanswered` (it didn't answer in
    /// time) or `other`; empty otherwise.
    pub refused: String,
    /// The files in the folder.
    pub files: u64,
}

impl From<tilo_workspace_service::files::Folder> for Folder {
    fn from(folder: tilo_workspace_service::files::Folder) -> Self {
        let state = serde_json::to_value(&folder.status.state)
            .ok()
            .and_then(|state| state.as_str().map(str::to_owned))
            .unwrap_or_default();
        let (stuck, refused) = match folder.status.stuck.into_iter().next() {
            Some(Waiting::Refused(reason)) => ("refused".to_owned(), refusal_kind(&reason)),
            Some(waiting) => (
                serde_json::to_value(&waiting)
                    .ok()
                    .and_then(|said| said.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                String::new(),
            ),
            None => (String::new(), String::new()),
        };
        Self {
            workspace: folder.workspace,
            name: folder.name,
            server: folder.server,
            path: folder.path,
            state,
            stuck,
            refused,
            files: u64::try_from(folder.status.files).unwrap_or(u64::MAX),
        }
    }
}

/// The running applications' tray items, in the order they registered.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct Tray {
    pub items: Vec<TrayItem>,
}

/// A running application's tray item, as the bar shows it.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct TrayItem {
    /// The item's key, which a press on it names.
    pub key: String,
    /// The app's own words for it.
    pub title: String,
    /// Whether choosing it opens its menu rather than its window.
    pub menu: bool,
    /// How many unread notifications the app that owns it has, as its badge shows.
    pub badge: i64,
    /// Whether it wants the person: a pulsing dot, as a status notifier says with `NeedsAttention`.
    pub attention: bool,
}

/// What the tray offers.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.tray")]
pub enum TrayEffect {
    /// Open or close the running application's tray item `key`: its window, or its menu when the
    /// item is one
    #[effect(kind = "outside")]
    #[effect(label = "session-tray-open")]
    OpenTrayItem { key: String },
}

/// What the agents offer.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.agents")]
pub enum AgentsEffect {
    /// End one agent session: its connection closes and its presence leaves the bar.
    #[effect(kind = "outside")]
    #[effect(label = "session-agents-stop", audience = "people")]
    StopAgent {
        #[param("The agent session, by its key")]
        agent: String,
    },
}

/// The scopes a walk of the switcher crosses, as the walk's action names them: the whole workspace
/// (Alt+Tab), or the app the window in front shows (Alt and the grave key).
pub const SWITCHER_SCOPES: [&str; 2] = ["workspace", "app"];

/// The windows as a view of another language reads them (`device.windows`).
#[must_use]
pub fn windows_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<crate::windows::Windows, tilo_resource::NoEffect>(
        "windows", "Windows",
    )
}

/// The wallpaper as a view of another language reads it (`device.wallpaper`): the picture the
/// workspace in front shows, by the name the person's choice gives it, empty for the one the
/// image ships.
#[must_use]
pub fn wallpaper_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Wallpaper, tilo_resource::NoEffect>(
        "wallpaper",
        "Wallpaper",
    )
}

/// The tray as a view of another language reads it (`device.tray`).
#[must_use]
pub fn tray_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Tray, TrayEffect>("tray", "Tray")
}

/// The agents as a view of another language reads them (`device.agents`).
#[must_use]
pub fn agents_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<crate::agents::Agents, AgentsEffect>(
        "agents", "Agents",
    )
}

/// The folders as a view of another language reads them (`device.folders`).
#[must_use]
pub fn folders_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Folders, tilo_resource::NoEffect>(
        "folders", "Folders",
    )
}

/// The shell's search as a view of another language reads it (`device.search`): the launcher draws
/// the rows the search found over everything the person can reach, grouped by kind and chosen
/// through the shell's own `choose-search-row`.
#[must_use]
pub fn search_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Search, tilo_resource::NoEffect>("search", "Search")
}

/// The empty shell's welcome cards as a view of another language reads them (`device.launchpad`):
/// what a new or empty workspace offers before the person opens anything, each card run through
/// the shell's own `launchpad` verb.
#[must_use]
pub fn launchpad_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Launchpad, tilo_resource::NoEffect>(
        "launchpad",
        "Launchpad",
    )
}

/// The passing notice as a view of another language reads it (`device.said`).
#[must_use]
pub fn notice_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Said, SaidEffect>("said", "Notice")
}

/// The invitations into a workspace, and the sheet they are made in, as a view of another
/// language reads them (`device.invite`): the verbs a roster's invite button asks.
#[must_use]
pub fn invite_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Invite, InviteEffect>("invite", "Invite")
}

/// The invitations' resource as a view of another language reads it: the workspace whose sheet an
/// invitation is made in — every invitation itself is read from the workspace's host through the
/// `invitations` verb.
#[derive(Debug, Default, serde::Serialize, tilo_resource::JsonSchema)]
#[allow(dead_code, reason = "read by the schema it describes, never in Rust")]
pub struct Invite {
    /// The workspace in front, by its key: whose sheet the shell's invite button opens.
    pub workspace: String,
}

/// What inviting offers: the links into a workspace, the invite sheet, and adding another device or
/// an agent of the person's.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.invite")]
pub enum InviteEffect {
    /// The invitations this device's person made into a workspace that can still be
    /// redeemed, in the words the workspace's host lists them by
    #[effect(kind = "outside")]
    Invitations {
        /// The workspace, by its key; the one in front when absent
        #[param("The workspace, by its key; the one in front when absent")]
        workspace: Option<String>,
    },
    /// Make an invitation into a workspace from this device: the link to hand to whoever is
    /// being invited, which their device joins with
    #[effect(kind = "outside")]
    IssueInvitation {
        /// The part the invitation brings them in at: `viewer`, `commenter`, `editor`,
        /// `moderator` or `administrator`; editor when absent
        #[param(
            "The part the invitation brings them in at: `viewer`, `commenter`, `editor`, `moderator` or `administrator`"
        )]
        role: Option<String>,
        /// The name the invitation knows them by
        #[param("The name the invitation knows them by")]
        invitee_name: Option<String>,
        /// The person the link is for, by their user key in hex: only a device of theirs may redeem
        /// it. Anyone holding the link may redeem it when absent
        #[param(
            "The person the link is for, by their user key in hex; anyone holding it when absent"
        )]
        recipient: Option<String>,
        /// How long the link works for, in seconds
        #[param("How long the link works for, in seconds")]
        expires_in_seconds: Option<u64>,
        /// The workspace, by its key; the one in front when absent
        #[param("The workspace, by its key; the one in front when absent")]
        workspace: Option<String>,
    },
    /// Open the invite sheet on a workspace: where a link into it is made, the part it brings
    /// someone in at picked and the links that still work taken back. An app shares what it shows
    /// by asking this, so it offers the one sheet the bar's invite button opens.
    #[effect(screen)]
    ShowInvite {
        /// The workspace, by its key; the one in front when absent
        #[param("The workspace, by its key; the one in front when absent")]
        workspace: Option<String>,
    },
    /// Show how an agent or another computer connects as the person: the setup their own card's
    /// "Connect an agent or computer" opens. An app that lists what acts as them offers adding to
    /// it this way.
    #[effect(screen)]
    ShowConnect,
    /// Show how a new device of the person's joins them: Settings' devices page, where "Add a
    /// device" prints the ticket it logs in with. An app that lists what acts as the person offers
    /// adding to it this way.
    #[effect(screen)]
    ShowPairDevice,
    /// Open what a `tilo://open#…` link names: the app that keeps it shows it, started first when
    /// no window of it does. An app that shows a link opens it this way, whoever it was copied
    /// from.
    #[effect(screen)]
    OpenLink {
        /// The link, a `tilo://open#…` address
        #[param("The link, a `tilo://open#…` address")]
        link: String,
    },
    /// Cancel the invitation `invite` of a workspace: its link admits nobody from now on
    #[effect(kind = "outside")]
    CancelInvitation {
        /// The invitation's id, 64 hex characters, as listing invitations reports it
        #[param("The invitation's id, 64 hex characters, as listing invitations reports it")]
        invite: String,
        /// The workspace, by its key; the one in front when absent
        #[param("The workspace, by its key; the one in front when absent")]
        workspace: Option<String>,
    },
}

/// The kind of `refusal`, by the name a view words it from: the detail of any other refusal is
/// in the words of whoever refused it, which no view shows a person.
fn refusal_kind(refusal: &Refusal) -> String {
    match refusal {
        Refusal::Other(_) => "other".to_owned(),
        kind => serde_json::to_value(kind)
            .ok()
            .and_then(|said| said.as_str().map(str::to_owned))
            .unwrap_or_default(),
    }
}
