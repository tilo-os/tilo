//! The bar and the desk's rules over what a host knows: one model for every shell host, native or
//! a page, fed by the facts only that host can see ([`ChromeSource`]). Portable display snapshots
//! and typed chrome requests. No system probing or effects.
use std::cell::{Cell, RefCell};

use tilo_desktop_model::DesktopTree;
use tilo_desktop_model::commands::{DesktopCommand, Standing, may_command, may_create_workspace};
use tilo_desktop_model::ownership::Authors;
use tilo_desktop_model::{StripId, WorkspaceSlot};

use crate::DesktopHandle;
use crate::host::contract::DesktopRules;
/// The machine's link, as the toolkits' surfaces draw it: the bar's status cluster and the lock
/// screen's footer share the one reading.
pub use ui::widget::{LinkKind, LinkStatus};
use vello::kurbo::Rect;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeState {
    pub level: f32,
    pub muted: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Microphone,
    Camera,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveMediaUse {
    pub client_id: u32,
    pub label: String,
    pub kind: MediaKind,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemStatus {
    pub battery_percent: Option<u8>,
    pub battery_charging: bool,
    pub battery_low: bool,
    pub battery_critical: bool,
    pub link: Option<LinkStatus>,
    pub volume: Option<VolumeState>,
    pub media: Vec<ActiveMediaUse>,
    pub summary: String,
}
impl SystemStatus {
    pub fn battery_percent(&self) -> Option<u8> {
        self.battery_percent
    }
    pub fn battery_is_charging(&self) -> bool {
        self.battery_charging
    }
    pub fn battery_is_low(&self) -> bool {
        self.battery_low
    }
    pub fn battery_is_critical(&self) -> bool {
        self.battery_critical
    }
    pub fn audio_is_silent(&self) -> bool {
        self.volume
            .is_some_and(|v| v.muted || v.level <= f32::EPSILON)
    }
    pub fn summary(&self) -> String {
        if self.summary.is_empty() {
            crate::i18n::t("desktop-system-status")
        } else {
            self.summary.clone()
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockSnapshot {
    pub unix_seconds: i64,
    pub clock: String,
    pub weekday: String,
    pub date: String,
}
impl ClockSnapshot {
    /// The wall clock now, in the person's time style and locale: what the bar shows on every
    /// desktop, native or in a page.
    #[must_use]
    pub fn now() -> Self {
        let now = ui::clock::local_now();
        Self {
            unix_seconds: now.timestamp(),
            clock: ui::clock::time_of_day(&now),
            weekday: ui::format::weekday_short(chrono::Datelike::weekday(&now)),
            date: ui::format::month_day(now.date_naive()),
        }
    }
}
impl Default for ClockSnapshot {
    fn default() -> Self {
        Self {
            unix_seconds: 0,
            clock: "--:--".into(),
            weekday: String::new(),
            date: String::new(),
        }
    }
}
/// The small piece of the workspace's history passive chrome shows. The history document stays
/// authoritative.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivityGlance {
    pub headline: Option<String>,
    pub unread: usize,
    /// Changes whenever a new latest entry is observed, even when the text is coincidentally
    /// identical, so the bar can restart its brief reveal.
    pub pulse: u64,
    pub focus: Option<ActivityFocus>,
}

/// Intent-addressable destination inside History. Device-local; links from chrome, search and
/// agents all resolve through the same shape.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivityFocus {
    pub author: Option<String>,
    pub group_id: Option<String>,
    pub entry_id: Option<[u8; 16]>,
    /// Stable record/resource identity. Paths are deliberately excluded: rename and move must not
    /// break a Files → Activity hand-off.
    pub resource_id: Option<String>,
    /// App-native immutable version identity, retained for detail renderers.
    pub resource_version: Option<[u8; 16]>,
}

#[derive(Clone, Default)]
pub struct ChromeOverlays {
    pub overview: bool,
    pub ws_settings: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ActiveExposure {
    pub peer: u64,
    pub surface: u64,
    pub can_control: bool,
}
/// Whether the top bar holds its strip of the screen or shows over the windows. The person chooses
/// between the first and the other two (`desktop_controls.hide_top_bar`); the host moves between
/// those two as the pointer, the keyboard and the bar's own menus ask for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TopBarPresence {
    /// Holds its strip of the screen; the windows start below it.
    #[default]
    Docked,
    /// Out of the way until reached for: the windows take its strip, and the bar neither paints nor
    /// takes the pointer.
    Hidden,
    /// Reached for: drawn over the windows' top edge, in the same place it is docked. Nothing under
    /// it moves, coming or going.
    Shown,
}

impl TopBarPresence {
    /// Whether the bar takes no room in the layout.
    #[must_use]
    pub fn overlays(self) -> bool {
        self != Self::Docked
    }
}

#[derive(Clone, Default)]
pub struct DesktopChromeSnapshot {
    pub revision: u64,
    pub status: SystemStatus,
    pub clock: ClockSnapshot,
    pub overlays: ChromeOverlays,
    /// A window on this device covers its display: the bar and its strips step aside until it gives
    /// the display back.
    pub fullscreen: bool,
    pub notification_badge: u32,
    /// Fresh banners are being held (Do Not Disturb, presenting, a focus schedule): the bar's
    /// status cluster shows a silenced bell.
    pub notifications_quiet: bool,
    /// The host has no quick settings: the status cluster is the bell that opens the notification
    /// centre, named and drawn as one, not a settings glyph promising settings that are not there.
    pub notifications_only: bool,
    pub activity_glance: ActivityGlance,
    pub viewport_size: Option<(f64, f64)>,
    /// The bar's pill: the activity it names and how many others wait behind it, ordered once here
    /// rather than at every read.
    pub activities: ui::floating_layer::Pill,
    pub active_exposures: Vec<ActiveExposure>,
    /// Pages reading this workspace right now. The bar counts them beside the avatars so an
    /// audience is never invisible.
    pub watching: u32,
    /// The person at this desktop as everyone here sees them, for the bar's own avatar; `None`
    /// paints none.
    pub own: Option<OwnChip>,
    /// Whether the person may invite others here: the bar keeps its invite button beside the faces,
    /// however many people are present.
    pub invite: Invite,
    /// The last wave at this person: the bar says who waved, for a moment after it came.
    pub waved: Option<WaveNotice>,
    /// The person arrived where others are: their faces pulse in the bar and a short name slides
    /// out of them, for a moment.
    pub arrived: Option<ArrivalNotice>,
    /// Settings asked which screen is which: each display's number and the name Settings gives it,
    /// said on that display for a moment.
    pub identify: Option<Ask>,
}

/// Whether the host lets the person at this desktop invite others here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Invite {
    #[default]
    Unavailable,
    Offered,
}

impl Invite {
    /// [`Self::Offered`] when `offered` .
    #[must_use]
    pub fn when(offered: bool) -> Self {
        if offered {
            Self::Offered
        } else {
            Self::Unavailable
        }
    }
}

/// Someone waved at the person at this desktop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaveNotice {
    /// Who waved, by their name here.
    pub from: String,
    /// When the wave came.
    pub at: ui::time::Instant,
}

/// The person at this desktop arrived where others are (joined the workspace, or came back to it
/// with someone new there). The bar says so on the faces it already shows, never over the windows:
/// those faces pulse and a short name slides out of them, then goes on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrivalNotice {
    /// Who is here, by roster id: the faces that pulse.
    pub people: Vec<String>,
    /// Their names, in the roster's order.
    pub names: Vec<String>,
    /// The first time in a public workspace: what its people see of the person, in a few words.
    /// Pressing the status opens the person's own menu, where they choose otherwise.
    pub seen: Option<String>,
    /// When the person arrived.
    pub at: ui::time::Instant,
}

impl ArrivalNotice {
    /// What slides out of the faces: the first person's short name, and how many others ("Ada",
    /// "Ada and 2 others"), or what the people here see of the person when that is told.
    #[must_use]
    pub fn text(&self) -> String {
        if let Some(seen) = &self.seen {
            return seen.clone();
        }
        let Some(first) = self.names.first() else {
            return String::new();
        };
        // A short name: the first word of the name everyone here sees.
        let short = first.split_whitespace().next().unwrap_or(first);
        let others = self.names.len().saturating_sub(1);
        if others == 0 {
            crate::i18n::t_args("topbar-arrived-one", &[("name", short.into())])
        } else {
            crate::i18n::t_args(
                "topbar-arrived-more",
                &[("name", short.into()), ("count", others.into())],
            )
        }
    }
}

/// The person at this desktop, as their own avatar in the bar shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnChip {
    /// Their name as everyone here sees it, numbered when shared.
    pub name: String,
    /// Their colour.
    pub rgb: (u8, u8, u8),
    /// Who they are (their root key, hex), for the picture they chose;
    /// empty where the host names none.
    pub identity: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChromeIntent {
    ToggleOverview,
    SwitchWorkspace(i32),
    ToggleQuickSettings,
    ShowQuickSettings,
    /// Open the notification centre, or close it when it is up: the bell in the status cluster,
    /// which is its own target inside the cluster's quick settings control.
    ToggleNotifications,
    ToggleCollaborators,
    /// Invite someone to this workspace, straight from the bar's invite button: the host's own
    /// invite flow, in one step.
    Invite,
    /// Add a device of this person's: the link a new device becomes one of theirs with, which its
    /// camera reads.
    PairDevice,
    /// Open who is here at this person (their roster id): their row, with what they are doing and
    /// the wave.
    ShowPerson(String),
    /// Open History, at `focus` when the bar names one.
    ShowActivity(Option<ActivityFocus>),
    /// Bring an activity's card out of the bar's pill: the app that shows it and its id, which is
    /// only its own app's.
    ExpandActivity {
        app: String,
        activity: String,
    },
    /// Focus the tab another of this person's devices offered, here.
    AcceptContinuity,
    /// Hide the continuity offer until that device's focus moves again.
    DismissContinuity,
    /// Show the ask waiting as a dot on the person's own avatar
    /// ([`DesktopChromeSource::avatar_ask`]).
    OpenAvatarAsk,
    /// Fold the open ask into the avatar's dot: a click elsewhere, Escape.
    CollapseAvatarAsk,
    AnswerAvatarAsk,
    /// The person put the ask off ("Not now"): it goes.
    PutOffAvatarAsk,
    /// Show the active workspace's strips to choose from, in the strip switcher: a click on the
    /// strip's name after the workspace's.
    ShowStrips,
    /// Open the strip switcher for the keyboard, or close it when it is up: its chord does both.
    ToggleStrips,
    /// Give the active workspace this name.
    RenameWorkspace {
        name: String,
    },
    /// Take the active workspace off this device's desktop, leaving it to the people in it.
    LeaveWorkspace,
    /// Delete the active workspace, with everything in it: the person answers the question first.
    DeleteWorkspace,
    /// Delete the active workspace, with everything in it, now: the person already answered a
    /// question of their own about it.
    DeleteWorkspaceConfirmed,
    /// Open or close the menu behind the bar's Tilo mark
    /// ([`crate::topbar::TopBarChrome::with_tilo_mark`]), under
    /// [`crate::topbar::TopBarChrome::tilo_mark_rect`].
    ToggleTiloMenu,
    /// Open or close the menu behind the workspace name, under
    /// [`crate::topbar::TopBarChrome::workspace_menu_rect`] — or, `at` a place on the desk a
    /// secondary press named, standing there instead: the same menu, offering the desk's own
    /// commands and kept inside the screen at an edge.
    ToggleWorkspaceMenu {
        at: Option<(i32, i32)>,
    },
    /// Open or close the card behind the person's own avatar, under
    /// [`crate::topbar::TopBarChrome::own_avatar_rect`].
    ToggleProfile,
    /// Deliver a click to a running application's tray item.
    AppTray {
        key: String,
        request: AppTrayRequest,
    },
}

/// Whether a tray item wants attention; passive items are not drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppTrayStatus {
    Passive,
    Active,
    NeedsAttention,
}

/// A tray item's picture: its own pixels, or a letter badge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppTrayIcon {
    /// Straight-alpha RGBA, row-major.
    Pixels {
        width: u32,
        height: u32,
        rgba: std::sync::Arc<Vec<u8>>,
    },
    Letter(char),
}

/// One running application's status-notifier item, as the bar draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppTrayItem {
    /// Stable key the host addresses the item by.
    pub key: String,
    pub id: String,
    /// The application whose connection registered the item, proven from its process. `None` when
    /// it could not be proven; an item's own id is never trusted for this.
    pub owner_app: Option<String>,
    /// What to call the item: the application's own title for it, else its id. Named to the person
    /// and to assistive technology, never empty.
    pub title: String,
    pub status: AppTrayStatus,
    pub icon: AppTrayIcon,
    /// The item has no primary action; activating it opens its menu.
    pub item_is_menu: bool,
}

/// A click on a tray item, at whole-pixel pointer coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppTrayRequest {
    Activate { x: i32, y: i32 },
    SecondaryActivate { x: i32, y: i32 },
    ContextMenu { x: i32, y: i32 },
}
/// An ask about the person themselves (keeping who they are here), put to them from their own
/// avatar in the top bar. Open, it is a small popover in the bar's band beside the avatar, so it
/// never covers a window or takes a click meant for one; clicked away it waits as a dot on the
/// avatar, which opens it again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AvatarAsk {
    /// The question ("Keep your name and work?").
    pub question: String,
    /// What answering brings, said where there is room and to assistive technology.
    pub detail: String,
    /// The answer's label ("Keep with passkey").
    pub answer: String,
    /// Shown as the popover; `false` while it waits as the avatar's dot.
    pub open: bool,
}

pub trait DesktopChromeSource {
    fn snapshot(&self) -> DesktopChromeSnapshot;
    fn tree(&self) -> DesktopTree;
    /// Where each display's bar goes. A host that cannot tell its displays apart answers none, and
    /// the desk carries one full bar across it.
    fn bars(&self) -> Placement {
        Placement::default()
    }
    /// Returns whether the host accepted the request, never proof of a shared mutation.
    fn intent(&self, intent: ChromeIntent) -> bool;
    fn settings_anchor(&self, _rect: Rect) {}
    /// The live offer to continue a tab from another of this person's devices, as the label the bar
    /// shows ("Continue Notes from Laptop").
    fn continuity_offer(&self) -> Option<String> {
        None
    }
    /// The ask waiting on the person's own avatar, open or as its dot.
    fn avatar_ask(&self) -> Option<AvatarAsk> {
        None
    }
    /// Changes whenever the running applications' tray items change.
    fn app_tray_revision(&self) -> u64 {
        0
    }
    /// The running applications' tray items, in the host's order.
    fn app_tray(&self) -> Vec<AppTrayItem> {
        Vec::new()
    }
    /// How many unread notifications the application with this verified id owns, as its icon says
    /// on the bar. Zero when it owns none, or when this person turned app badges off.
    fn app_notification_badge(&self, _app_id: &str) -> u32 {
        0
    }
}
pub type Chrome = std::rc::Rc<dyn DesktopChromeSource>;

/// What a host knows of the desk it shows, which the bar, its rules and the desk's asks are made
/// from: the tree, who is here, the person at this device, and whatever else only it can see (a
/// session's compositor and tray, a page's here-list and storage). The chrome reads these; the host
/// answers nothing about the bar itself.
pub trait ChromeSource {
    fn handle(&self) -> &DesktopHandle;
    /// The person this desktop acts for.
    fn person(&self) -> &str;
    /// Who is here, as the bar's faces and the roster read them: each host's own presence.
    fn people_revision(&self) -> u64 {
        0
    }
    /// Place who is here in `tree`, as the active workspace's collaborators.
    fn place_people(&self, _tree: &mut DesktopTree) {}
    /// Ask `answer` what this person may change here, as this host knows their standing.
    fn with_standing<R>(&self, answer: impl FnOnce(&Standing<'_>) -> R) -> R;
    /// Ask `answer` who made each strip and opened each window here.
    fn with_authors<R>(&self, answer: impl FnOnce(&mut Authors) -> R) -> R;
    /// The person's own avatar: their name, colour and identity.
    fn own_chip(&self) -> OwnChip;
    /// The notifications waiting here, and whether the bell is silenced.
    fn notices(&self) -> Notices;
    /// Whether the person may invite others into the workspace in front.
    fn may_invite(&self) -> bool {
        false
    }
    /// The last wave at this person.
    fn waved(&self) -> Option<WaveNotice> {
        None
    }
    /// Pages reading this workspace right now.
    fn watching(&self) -> u32 {
        0
    }
    /// How many unread notifications the app `app` owns, for its icon's badge.
    fn app_badge(&self, _app: &str) -> u32 {
        0
    }
    /// The running applications' tray items, in the order they registered.
    fn app_tray(&self) -> Vec<AppTrayItem> {
        Vec::new()
    }
    /// Changes whenever the running applications' tray items change.
    fn app_tray_revision(&self) -> u64 {
        0
    }
    /// Add what else this host shows in the bar, which moves its revision as it changes.
    fn facts(&self, facts: &mut Facts);
    /// Fill the bar's facts only this host shows: its machine's status, its activities, what it
    /// identifies on its displays.
    fn fill(&self, _snapshot: &mut DesktopChromeSnapshot) {}
    /// Take a request the bar raised; whether this host takes it.
    fn take_intent(&self, _intent: ChromeIntent) -> bool {
        false
    }
    /// Ask this host for a frame, after the bar raised something.
    fn wake(&self) {}
    /// Where each display's bar goes.
    fn bars(&self) -> Placement {
        Placement::default()
    }
    /// Whether a window on this desk covers its display.
    fn fullscreen(&self) -> bool {
        self.handle().read(|tree| tree.fullscreen_group().is_some())
    }
    /// Whether the workspace at `workspace` may be deleted: nothing of it is left anywhere.
    ///
    /// # Errors
    /// The refusal, in words for the person.
    fn may_delete_workspace(&self, _workspace: WorkspaceSlot) -> Result<(), String> {
        Err(crate::i18n::t("desktop-cannot-delete-workspace"))
    }
    /// Whether the person may branch the workspace `workspace` : they read all of it. Asked after
    /// whether they may make a workspace at all.
    ///
    /// # Errors
    /// The refusal, in words for the person.
    fn may_branch_workspace(&self, _workspace: WorkspaceSlot) -> Result<(), String> {
        Ok(())
    }
    /// Where the workspace shown is hosted (`example.org`); `None` on a desktop of the person's
    /// own.
    fn workspace_host(&self) -> Option<String> {
        None
    }
}

/// What the bar's bell says: the notifications waiting, and whether it is silenced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Notices {
    pub unread: u32,
    pub quiet: bool,
}

/// The facts a bar redraws for, in the order the host gave them: two snaps of the same facts leave
/// the bar's revision where it is.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Facts(Vec<u64>);

impl Facts {
    pub fn number(&mut self, number: u64) {
        self.0.push(number);
    }

    pub fn when(&mut self, on: bool) {
        self.number(u64::from(on));
    }

    /// A fact that is words (a name, a chosen colour): equal words are the same fact.
    pub fn words(&mut self, words: &str) {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        words.hash(&mut hasher);
        self.number(hasher.finish());
    }
}

/// The bar and the desk's rules, over what `source` knows: one model both a native session and a
/// page present, so each of them answers only where its facts come from.
pub struct HostChrome<S: ?Sized> {
    source: std::rc::Rc<S>,
    /// The facts the revision was last bumped for.
    seen: RefCell<Option<Facts>>,
    revision: Cell<u64>,
}

impl<S: ChromeSource + ?Sized> HostChrome<S> {
    pub fn new(source: std::rc::Rc<S>) -> Self {
        Self {
            source,
            seen: RefCell::default(),
            revision: Cell::new(0),
        }
    }

    /// The facts every bar shows, then the ones only this host knows.
    fn facts(&self, clock: &ClockSnapshot) -> Facts {
        let notices = self.source.notices();
        let own = self.source.own_chip();
        let waved = self.source.waved();
        let mut facts = Facts::default();
        facts.number(self.source.handle().revision());
        // Who is here, the window they are in and the call they are in reach the bar as its faces
        // and roster, and none of it moves the tree: without this the face of everyone who arrives
        // after the first frame never draws.
        facts.number(self.source.people_revision());
        facts.number(clock.unix_seconds.unsigned_abs().div_euclid(60));
        facts.when(waved.is_some());
        if let Some(waved) = &waved {
            facts.words(&waved.from);
        }
        facts.number(self.source.handle().personal_ask_revision());
        facts.number(u64::from(notices.unread));
        facts.when(notices.quiet);
        facts.when(self.source.may_invite());
        facts.number(u64::from(self.source.watching()));
        // The person's own name and colour relabel and repaint their chip in the bar.
        facts.words(&own.name);
        facts.words(&own.identity);
        facts.number(u64::from(own.rgb.0));
        facts.number(u64::from(own.rgb.1));
        facts.number(u64::from(own.rgb.2));
        self.source.facts(&mut facts);
        facts
    }
}

impl<S: ChromeSource + ?Sized> DesktopChromeSource for HostChrome<S> {
    fn snapshot(&self) -> DesktopChromeSnapshot {
        let clock = ClockSnapshot::now();
        let facts = self.facts(&clock);
        {
            let mut seen = self.seen.borrow_mut();
            // The bar's pixels for an ask that comes, opens, folds, is answered or is put off
            // change with it, so the revision moves for those as it does for the tree.
            if seen.as_ref() != Some(&facts) {
                *seen = Some(facts);
                self.revision.set(self.revision.get().wrapping_add(1));
            }
        }
        let notices = self.source.notices();
        let mut snapshot = DesktopChromeSnapshot {
            revision: self.revision.get(),
            clock,
            // The status cluster is the bell, unless a host fills its own machine's status in.
            notifications_only: true,
            fullscreen: self.source.fullscreen(),
            notification_badge: notices.unread,
            notifications_quiet: notices.quiet,
            watching: self.source.watching(),
            own: Some(self.source.own_chip()),
            invite: Invite::when(self.source.may_invite()),
            waved: self.source.waved(),
            ..DesktopChromeSnapshot::default()
        };
        self.source.fill(&mut snapshot);
        snapshot
    }

    fn tree(&self) -> DesktopTree {
        let mut tree = self.source.handle().read(Clone::clone);
        self.source.place_people(&mut tree);
        tree
    }

    fn bars(&self) -> Placement {
        self.source.bars()
    }

    fn avatar_ask(&self) -> Option<AvatarAsk> {
        avatar_ask(self.source.handle())
    }

    fn app_notification_badge(&self, app_id: &str) -> u32 {
        self.source.app_badge(app_id)
    }

    fn app_tray(&self) -> Vec<AppTrayItem> {
        self.source.app_tray()
    }

    fn app_tray_revision(&self) -> u64 {
        self.source.app_tray_revision()
    }

    fn intent(&self, intent: ChromeIntent) -> bool {
        let taken = self.source.take_intent(intent);
        self.source.wake();
        taken
    }
}

impl<S: ChromeSource + ?Sized> DesktopRules for HostChrome<S> {
    fn tree(&self) -> DesktopTree {
        DesktopChromeSource::tree(self)
    }

    fn people_revision(&self) -> u64 {
        self.source.people_revision()
    }

    fn author(&self) -> String {
        self.source.person().to_owned()
    }

    fn may(&self, command: &DesktopCommand) -> Result<(), String> {
        self.source.with_authors(|authors| {
            self.source.with_standing(|standing| {
                self.source.handle().read(|tree| {
                    let workspace = tree
                        .active_workspace()
                        .ok_or_else(|| "There is no workspace open".to_owned())?;
                    may_command(standing, authors, workspace, command)
                        .map_err(|refusal| refusal.to_string())
                })
            })
        })
    }

    fn may_create_workspace(&self) -> Result<(), String> {
        self.source.with_standing(|standing| {
            may_create_workspace(standing).map_err(|refusal| refusal.to_string())
        })
    }

    fn may_delete_workspace(&self, workspace: WorkspaceSlot) -> Result<(), String> {
        self.source.may_delete_workspace(workspace)
    }

    fn may_branch_workspace(&self, workspace: WorkspaceSlot) -> Result<(), String> {
        self.may_create_workspace()?;
        self.source.may_branch_workspace(workspace)
    }

    fn made_strip(&self, strip: StripId) {
        self.source.with_authors(|authors| {
            authors.insert(strip.0, self.source.person().to_owned());
        });
    }

    fn workspace_host(&self) -> Option<String> {
        self.source.workspace_host()
    }
}

/// The ask waiting on the person's own avatar in `handle`, open or as its dot.
pub fn avatar_ask(handle: &DesktopHandle) -> Option<AvatarAsk> {
    handle.personal_ask().map(|ask| AvatarAsk {
        question: ask.title,
        detail: ask.body,
        answer: ask.action.title,
        open: ask.open,
    })
}

/// One display's bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarPlace {
    /// The display, by the desktop's id for it.
    pub display: u32,
    /// The display's whole rectangle: the bar stands on its top edge.
    pub rect: Rect,
    /// The full bar (the display in use); the others carry the compact one.
    pub full: bool,
}

/// Where the bars go, in the bar's own coordinates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Placement {
    /// One per display. Empty: one full bar along the bar's own area.
    pub bars: Vec<BarPlace>,
    /// The bars a window covers, by the same key as [`Self::bars`]: `None` for the one bar of a
    /// desk that needs no placing, a display's id for a display's own. Each takes no room there
    /// until it is reached for.
    pub fullscreen: Vec<Option<u32>>,
}

impl Placement {
    /// The bars a desk of `tree` carries: one per display, the display in use full and the others
    /// compact, each on its own display's rectangle. `fullscreen` names the monitors of the
    /// displays a fullscreen window owns; a window the desk itself took full screen covers its
    /// display too, as the compositor never saw that one asked for.
    /// A desk showing a single display carries no bars here: one bar across the desk needs no
    /// placing, and the bar's own layout already stands it there.
    #[must_use]
    pub fn of(tree: &DesktopTree, fullscreen: &[String]) -> Self {
        let displays = &tree.outputs.displays;
        let group = tree
            .fullscreen_group()
            .and_then(|group| tree.outputs.display_of(group));
        let owned = || {
            displays
                .iter()
                .filter(|display| {
                    Some(display.id) == group || fullscreen.contains(&display.monitor)
                })
                .map(|display| Some(display.id))
                .collect()
        };
        if !tree.is_partitioned() {
            // The desk has one display, and any monitor the compositor names is that one.
            let covered = tree.fullscreen_group().is_some() || !fullscreen.is_empty();
            return Self {
                bars: Vec::new(),
                fullscreen: covered.then_some(None).into_iter().collect(),
            };
        }
        let in_use = tree.outputs.focused_display();
        Self {
            bars: displays
                .iter()
                .map(|display| BarPlace {
                    display: display.id,
                    rect: Rect::new(
                        display.x,
                        display.y,
                        display.x + display.width,
                        display.y + display.height,
                    ),
                    full: Some(display.id) == in_use,
                })
                .collect(),
            fullscreen: owned(),
        }
    }
}

/// One display's card: the number Settings shows it by, and the name Settings calls it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    /// The display, by its monitor as the desktop names it.
    pub monitor: String,
    /// Its place in the person's arrangement, counting from one.
    pub number: usize,
    /// What Settings calls it: the built-in display, or the monitor's model.
    pub name: String,
}

/// What Settings asked for: which request it is (asking again shows the cards again) and each
/// display's card.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ask {
    /// The request's stamp: the moment it was written, which changes with every press of Identify.
    pub id: u64,
    /// One per display the session shows something on.
    pub labels: Vec<Label>,
}
