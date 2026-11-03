//! This device's desk as a resource of the session's: the time in the person's style, who they
//! are, their notifications, the workspaces on this device and the strip in front, and the
//! tasks running for them. A shell's views read it (as `session.desk` resources,
//! `tilo_surface_host::shell_part`), and so does a script or an agent, through the door, by
//! the same address: one reading for every reader.

use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_desktop_model::{DesktopTree, RoomId, Workspace};
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::navigation::ChromeEffect;
use tilo_peer_state::join::MemberRole;
use tilo_peer_state::join::invitations::PendingInvitation;
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};
use tilo_surface_host::shell_part::DESK_RESOURCE;
use tilo_surface_host::{DeskReading, shell_part};

use crate::notifications::NoticeSignals;

/// What the desk's reading is made from ([`Desk::key`]): equal when the shell's views need not
/// run again.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DeskKey {
    /// The revisions, the minute, the signals and the names the reading is made of.
    reading: (
        u64,
        i64,
        u32,
        bool,
        u64,
        u64,
        u64,
        u64,
        u64,
        u64,
        String,
        u64,
    ),
    /// The agents seated at the person's agent door.
    agents: u64,
    /// Whether the switcher's panel shows: it comes up on its own while Alt is held, with no
    /// revision of its own to say so.
    revealed: bool,
}

/// The kind of the desk's resource: the origin kind a shell's type holds.
pub(crate) const DESK_KIND: &str = "session.desk";

/// The scopes a walk of the switcher crosses, as the walk's action names them: the whole
/// workspace (Alt+Tab), or the app the window in front shows (Alt and the grave key).
pub const SWITCHER_SCOPES: [&str; 2] = ["workspace", "app"];

/// The desk as a view of another language reaches it: its verbs ([`DeskEffect`]), which the
/// shell's views run as typed calls (`device.show_surface(device.desk, "quick-settings")`),
/// each through the desk's door as a script's or an agent's runs. The rest of what the desk
/// holds reaches the shell as the shell's own record (`root Desk`), so this names only the
/// surface that is up.
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<DeskSurface, DeskEffect>("desk", "Desk")
}

/// The desk's reading as [`surface`] publishes it: a slice of [`DeskReading`], by its keys.
#[derive(tilo_resource::JsonSchema)]
struct DeskSurface {
    /// The surface up over the desk's windows (`none`, `centre`, `quick-settings`, …).
    #[allow(dead_code, reason = "read by the schema it describes, never in Rust")]
    here: String,
}

/// What the desk offers.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.desk")]
pub(crate) enum DeskEffect {
    /// Bring a workspace of this device to the front
    SwitchWorkspace {
        #[param("The workspace, by its key")]
        workspace: String,
    },
    /// Answer the question waiting on the person's avatar
    #[effect(row, label = "Answer")]
    AnswerAsk,
    /// Put the question waiting on the person's avatar off for now
    #[effect(row, label = "Ask me later")]
    PutOffAsk,
    /// Open or close the running application's tray item `key`: its window, or its menu when
    /// the item is one
    #[effect(row, label = "Open")]
    OpenTrayItem { key: String },
    /// The invitations this device's person made into a workspace that can still be
    /// redeemed, in the words the workspace's host lists them by
    Invitations {
        /// The workspace, by its key; the one in front when absent
        #[param("The workspace, by its key; the one in front when absent")]
        workspace: Option<String>,
    },
    /// Cancel the invitation `invite` of a workspace: its link admits nobody from now on
    CancelInvitation {
        /// The invitation's id, 64 hex characters, as listing invitations reports it
        #[param("The invitation's id, 64 hex characters, as listing invitations reports it")]
        invite: String,
        /// The workspace, by its key; the one in front when absent
        #[param("The workspace, by its key; the one in front when absent")]
        workspace: Option<String>,
    },
    /// Show the surface `surface` over the desk's windows, or close it when it is up.
    ShowSurface {
        #[param(
            "The surface: `none`, `overview`, `switcher`, `launcher`, `centre`, `quick-settings`, `workspaces`, `composer` or `conversation`"
        )]
        surface: String,
    },
    /// End the agent `agent`'s work here: the connection the person admitted closes and its
    /// presence leaves the bar
    #[effect(label = "Stop")]
    StopAgent {
        #[param("The agent, by the key its presence names it")]
        agent: String,
    },
    /// Close the surface `surface`, when it is the one up.
    CloseSurface {
        #[param("The surface")]
        surface: String,
    },
    /// Clear the notification centre, which Undo puts back.
    ClearNotices,
    /// Dismiss the notice `notice` of the centre, by its id.
    DismissNotice {
        #[param("The notice, by its id")]
        notice: String,
    },
    /// Run one of the choices the notice `notice` offers, by the key its sender gave it.
    ActOnNotice {
        #[param("The notice, by its id")]
        notice: String,
        #[param("The choice, by its key")]
        action: String,
    },
    /// Open the window switcher, or walk it `steps` windows on.
    ///
    /// Alt+Tab walks the workspace's windows; Alt and the grave key the ones the app in
    /// front shows.
    WalkSwitcher {
        #[param("How far to walk; 1 the next window, -1 the one before")]
        steps: Option<i64>,
        #[param(
            "Which windows the walk crosses: `workspace` for every window of the workspace, `app` for the ones the app in front shows",
            choices = SWITCHER_SCOPES
        )]
        scope: Option<String>,
    },
    /// Go to the window the switcher's walk reached, and close the switcher.
    CommitSwitcher,
}

/// The surfaces the desk shows over its windows, and where each stands: the shell's own views,
/// except the quick settings, which stand in the panel's own app where the device has one and
/// no shell covers the desk that app stands in.
/// What `show-surface` and `close-surface` open and close, whether the person asks on the
/// desk's door or on the chrome's above it, so both open the one surface.
#[derive(Clone)]
pub(crate) struct Surfaces {
    /// Whether the device has the quick settings' app; without, the status opens the shell's
    /// own view of them.
    pub(crate) status_panel: bool,
    /// Whether the shell draws the whole screen over the desk: the desk draws none of the
    /// panel's app there, so the quick settings are the shell's own view, as every other
    /// surface is.
    pub(crate) shell: bool,
    /// The stack the panel's own app is placed in.
    pub(crate) panel: tilo_desktop::overlays::Overlays,
    /// The surface the shell's views show (`crate::overlays::Overlay`).
    pub(crate) overlays: Rc<crate::overlays::Overlays>,
}

impl Surfaces {
    /// Whether the quick settings stand in the panel's own app: the device has it, and the
    /// desk draws the app it stands in.
    fn panel_shows(&self, surface: &str) -> bool {
        surface == crate::quick_settings::SURFACE && self.status_panel && !self.shell
    }

    /// Show `surface` (`show-surface`): the one up closes, and any other opens.
    pub(crate) fn toggle(&self, surface: &str) {
        // The quick settings are their own app, standing where the stack places its slot: a
        // press opens the slot, and the panel's app draws it there. A device without that app,
        // or one the shell covers, keeps the shell's own view of the surface.
        if self.panel_shows(surface) {
            use tilo_desktop::overlays::Slot;
            if self.panel.is_open(Slot::QuickSettings) {
                self.panel.close(Slot::QuickSettings);
            } else {
                self.panel.open(Slot::QuickSettings, None);
            }
            return;
        }
        self.overlays.toggle(surface);
    }

    /// Show the surface `name` names (`show-surface`), whether or not it is up.
    pub(crate) fn open(&self, name: &str) {
        if self.panel_shows(name) {
            self.panel
                .open(tilo_desktop::overlays::Slot::QuickSettings, None);
            return;
        }
        self.overlays.open(name);
    }

    /// Close `surface` (`close-surface`), when it is the one up.
    pub(crate) fn close(&self, surface: &str) {
        if self.panel_shows(surface) {
            self.panel
                .close(tilo_desktop::overlays::Slot::QuickSettings);
            return;
        }
        self.overlays.close(surface);
    }
}

/// The desk as the session reads it. See the module docs.
pub(crate) struct Desk {
    pub(crate) handle: DesktopHandle,
    pub(crate) person: String,
    pub(crate) signals: Rc<NoticeSignals>,
    /// The surfaces the desk shows over its windows, and where they stand.
    pub(crate) surfaces: Surfaces,
    /// Who is here, when this session takes part in the collaboration plane.
    pub(crate) sync: Option<Rc<crate::sync::Client>>,
    /// The apps' live activities: Clocks' timer is the bar's clock beside the time, and the
    /// person putting the floating one away hides it until Clocks shows something new.
    pub(crate) clock: Rc<crate::chrome::Clock>,
    /// The running applications' tray items, when their watcher serves: the bar shows them,
    /// and the person's clicks go back through it.
    pub(crate) tray: Option<std::sync::Arc<tilo_tray_host::Tray>>,
    /// The notification centre the reading lists, and what it last said.
    pub(crate) notices: tilo_desktop_surface::DesktopNotifications,
    /// The runs of what the person asked the session's agents: each run's conversation is
    /// the desk's to show (shell.md §8.1).
    pub(crate) runs: std::sync::Arc<tilo_agent_runs::AgentRuns>,
    /// The agents the person admitted at its agent door: each works here as itself, and the
    /// bar shows it as its own presence.
    pub(crate) seats: std::sync::Arc<crate::agent_door::Seats>,
    /// This device's workspaces' record, when the session holds one: the invitations
    /// a workspace's people made are kept there, by the device that made them.
    pub(crate) engine: Option<std::sync::Arc<crate::engine::Engine>>,
}

/// What a window with no title is called.
fn untitled() -> String {
    tilo_desktop_surface::i18n::t("window-switcher-untitled")
}

/// The windows the desk shows, by recency: the tabs of every strip of the workspace in front,
/// the focused one first, then those it was in front of, then the rest along their strips.
///
/// `handle` says what may be pictured: a window whose app holds secrets shows its app's glyph
/// instead of its picture (`withheld`), and one that is not this device's window shows neither
/// (`none`).
fn windows(tree: &DesktopTree, handle: &DesktopHandle) -> Vec<shell_part::WindowReading> {
    use tilo_desktop_model::TileContent;
    let Some(workspace) = tree.active_workspace() else {
        return Vec::new();
    };
    let active_strip = tree
        .active_strip
        .get(&workspace.slot)
        .copied()
        .or_else(|| workspace.strips.first().map(|strip| strip.id));
    // The tabs the focus history remembers, most recent first.
    let history: Vec<u64> = workspace
        .strips
        .iter()
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.focus_history.iter().rev().copied())
        .collect();
    let mut readings = Vec::new();
    let mut focused_tab = None;
    for strip in &workspace.strips {
        let active_group = tree
            .active_group_for(strip.id)
            .filter(|id| strip.window_groups.iter().any(|group| group.id == *id))
            .or_else(|| strip.window_groups.first().map(|group| group.id));
        for group in &strip.window_groups {
            for tab in &group.tabs {
                let focused = Some(strip.id) == active_strip
                    && Some(group.id) == active_group
                    && tab.id == group.active_tab;
                if focused {
                    focused_tab = Some(tab.id);
                }
                let app = match &tab.tile {
                    TileContent::Foreign { app_id, .. }
                    | TileContent::LocalApp { app_id, .. }
                    | TileContent::PeerWindow { app_id, .. }
                    | TileContent::HostedWindow { app_id, .. } => app_id.clone(),
                    TileContent::AppPlaceholder { label, .. } | TileContent::Stub { label, .. } => {
                        label.clone()
                    }
                };
                let title = tab.title.trim();
                // What the window's well shows instead of a picture: its app's own glyph, so
                // two apps' windows never look alike.
                let icon = ui::icons::app_glyph(&app, None).name().to_owned();
                let preview = match &tab.tile {
                    TileContent::Foreign { .. } if handle.withholds(&app) => "withheld",
                    TileContent::Foreign { .. } => "live",
                    _ => "none",
                };
                readings.push(shell_part::WindowReading {
                    key: tab.id.to_string(),
                    title: if title.is_empty() {
                        untitled()
                    } else {
                        title.to_owned()
                    },
                    // The app as people know it, never the id: a window is named by what it
                    // shows, and its `app_title` is a person's own name for the window, not
                    // its app's name.
                    origin_app: crate::app_names::app_name(&app),
                    app_key: app,
                    icon,
                    strip: strip.name.trim().to_owned(),
                    strip_key: strip.id.0.to_string(),
                    workspace: workspace.name.clone(),
                    recency: 0,
                    focused,
                    preview: preview.to_owned(),
                });
            }
        }
    }
    let rank = |key: &str| -> i64 {
        let tab: u64 = key.parse().unwrap_or(u64::MAX);
        if Some(tab) == focused_tab {
            return 0;
        }
        history
            .iter()
            .position(|at| *at == tab)
            .map_or(i64::MAX / 2, |at| {
                i64::try_from(at).unwrap_or(0).saturating_add(1)
            })
    };
    for (at, reading) in readings.iter_mut().enumerate() {
        reading.recency = rank(&reading.key)
            .saturating_mul(1024)
            .saturating_add(i64::try_from(at).unwrap_or(0));
    }
    readings.sort_by_key(|reading| reading.recency);
    // What is left of the rank is the order the walk takes: where each window stands in it, so
    // a view shows the page the walk has reached without knowing how the order was made.
    for (at, reading) in readings.iter_mut().enumerate() {
        reading.recency = i64::try_from(at).unwrap_or(i64::MAX).saturating_add(1);
    }
    readings
}

/// The windows the switcher's walk crosses, as their places in `windows`, with the place in
/// them of the window in front: every window of the workspace, or the ones the app the front
/// window shows has. The walk, the mark the reading carries and the commit all count from
/// this one list.
fn walked(
    windows: &[shell_part::WindowReading],
    scope: crate::overlays::SwitcherScope,
) -> (Vec<usize>, Option<usize>) {
    use crate::overlays::SwitcherScope;
    let front = windows.iter().position(|window| window.focused);
    let application =
        front.and_then(|front| windows.get(front).map(|window| window.app_key.clone()));
    let walked: Vec<usize> = match (scope, application) {
        (SwitcherScope::App, Some(application)) => windows
            .iter()
            .enumerate()
            .filter(|(_, window)| window.app_key == application)
            .map(|(at, _)| at)
            .collect(),
        _ => (0..windows.len()).collect(),
    };
    let front = front.and_then(|front| walked.iter().position(|at| *at == front));
    (walked, front)
}

impl Desk {
    /// What the reading is made from, without making it: the tree's revision, the minute,
    /// what the notifications say and who is here. The reading changes only when this does.
    pub(crate) fn key(&self) -> DeskKey {
        DeskKey {
            reading: (
                self.handle.revision(),
                ui::clock::local_now().timestamp().div_euclid(60),
                self.signals.unread(),
                self.signals.quiet(),
                self.sync
                    .as_ref()
                    .map_or(0, |sync| sync.here().borrow().revision()),
                self.clock.board().revision(),
                self.handle.personal_ask_revision(),
                self.tray.as_ref().map_or(0, |tray| tray.revision()),
                self.notices.revision(),
                self.surfaces.overlays.revision(),
                self.surfaces.overlays.here(),
                self.runs.revision(),
            ),
            agents: self.seats.revision(),
            revealed: self.surfaces.overlays.revealed(std::time::Instant::now()),
        }
    }

    /// When the desk has something to draw that it has not drawn yet, without a frame in
    /// between: the instant the switcher's panel comes up while Alt is held on it, or
    /// the minute the bar's clock shows next.
    pub(crate) fn next_deadline(&self) -> Option<std::time::Instant> {
        let now = std::time::Instant::now();
        let second = ui::clock::local_now().timestamp().rem_euclid(60);
        let minute = now
            .checked_add(std::time::Duration::from_secs(
                60_u64.saturating_sub(u64::try_from(second).unwrap_or(0)),
            ))
            .unwrap_or(now);
        [self.surfaces.overlays.deadline(now), Some(minute)]
            .into_iter()
            .flatten()
            .min()
    }

    /// The desk's windows, with the one the switcher's walk has reached marked focused while
    /// its panel shows: the tile its walk chose is the one lifted. A quick flick shows no
    /// panel, so it lifts nothing either.
    fn windows_reaching(
        &self,
        tree: &DesktopTree,
        now: std::time::Instant,
    ) -> Vec<shell_part::WindowReading> {
        let mut windows = windows(tree, &self.handle);
        if self.surfaces.overlays.revealed(now) {
            let (walked, front) = walked(&windows, self.surfaces.overlays.scope());
            if let Some(at) = self
                .surfaces
                .overlays
                .reached(walked.len(), front)
                .and_then(|at| walked.get(at).copied())
            {
                for (index, window) in windows.iter_mut().enumerate() {
                    window.focused = index == at;
                }
            }
        }
        windows
    }

    /// The desk now.
    pub(crate) fn reading(&self) -> DeskReading {
        let now = ui::clock::local_now();
        let instant = std::time::Instant::now();
        let people = self.handle.read(|tree| {
            self.sync.as_ref().map_or_else(Vec::new, |sync| {
                sync.people(tree, &|_: &str| None)
                    .into_iter()
                    .map(|person| shell_part::PersonReading {
                        key: person.id,
                        name: person.label,
                    })
                    .collect()
            })
        });
        let conversations = Conversations::of(&self.runs.readings());
        self.handle.read(|tree| DeskReading {
            person: self.person.clone(),
            clock: ui::clock::time_of_day(&now),
            timer: self.timer(),
            ask: self
                .handle
                .personal_ask()
                .map(|ask| shell_part::AskReading {
                    question: ask.title,
                    detail: ask.body,
                    answer: ask.action.title,
                    open: ask.open,
                }),
            weekday: ui::format::weekday_short(chrono::Datelike::weekday(&now)),
            date: ui::format::month_day(now.date_naive()),
            unread: self.signals.unread(),
            quiet: self.signals.quiet(),
            status_panel: self.surfaces.status_panel,
            workspace: tree
                .active_workspace()
                .map(|workspace| workspace.name.clone())
                .unwrap_or_default(),
            strip: strip_in_front(tree),
            workspaces: tree
                .workspaces
                .iter()
                .map(|workspace| shell_part::WorkspaceReading {
                    key: workspace.slot.0.to_string(),
                    name: workspace.name.clone(),
                    current: workspace.slot == tree.active,
                })
                .collect(),
            // The native session runs no agent tasks of its own yet: none is published until a
            // task host reports to it.
            tasks: Vec::new(),
            here: self.surfaces.overlays.here(),
            switcher_revealed: self.surfaces.overlays.revealed(instant),
            switcher_scope: self.surfaces.overlays.scope().name().to_owned(),
            spotlight: self.surfaces.overlays.spotlight(),
            banner: self
                .notices
                .log()
                .active()
                .last()
                .map(|notice| notice.id.to_string())
                .unwrap_or_default(),
            windows: self.windows_reaching(tree, instant),
            agents: self.seats.readings(),
            rooms: tree
                .workspaces
                .iter()
                .map(|workspace| shell_part::RoomReading {
                    key: workspace.slot.0.to_string(),
                    name: workspace.name.clone(),
                    current: workspace.slot == tree.active,
                    members: self.person.clone(),
                    present: 0,
                    activity: String::new(),
                })
                .collect(),
            notices: notices(&self.notices),
            notice_actions: notice_actions(&self.notices),
            badges: self.signals.badges(),
            people,
            tray: self.tray_items(),
            runs: conversations.runs,
            run_entries: conversations.entries,
            entry_refs: conversations.refs,
            // What the screen says of its edges moves without the desk; the link publishes it
            // beside this reading and carries it over (`SessionLink::publish_edges`).
            ..DeskReading::default()
        })
    }

    fn timer(&self) -> Option<shell_part::TimerReading> {
        let activity = self.clock.board().activity(crate::chrome::CLOCKS_APP)?;
        let activity: ClocksActivity = serde_json::from_value(activity).ok()?;
        (activity.active && self.clock.floating(activity.overlay)).then_some(
            shell_part::TimerReading {
                activity_id: activity.activity_id,
                label: activity.label,
                remaining_secs: activity.remaining_secs,
                stopwatch: activity.stopwatch,
                paused: activity.paused,
            },
        )
    }

    /// The due timer Clocks wants announced, by its id and in the app's own words when it has
    /// them (the desktop's otherwise): what the bar announces once.
    pub(crate) fn due_timer(&self) -> Option<(String, String, String)> {
        let activity = self.clock.board().activity(crate::chrome::CLOCKS_APP)?;
        let activity: ClocksActivity = serde_json::from_value(activity).ok()?;
        let id = activity.notification_id?;
        let title = if activity.notification_title.is_empty() {
            crate::i18n::t("clock-due-title")
        } else {
            activity.notification_title
        };
        let body = if activity.notification_body.is_empty() {
            crate::i18n::t("clock-due-body")
        } else {
            activity.notification_body
        };
        Some((id, title, body))
    }

    /// The running applications' tray items, in the order they registered: each's key, the
    /// app's own words for it, and whether choosing it opens a menu rather than its window.
    fn tray_items(&self) -> Vec<shell_part::TrayReading> {
        self.tray.as_ref().map_or_else(Vec::new, |tray| {
            tray.snapshot()
                .into_iter()
                .map(|item| shell_part::TrayReading {
                    key: item.key,
                    title: if item.title.trim().is_empty() {
                        item.id
                    } else {
                        item.title
                    },
                    menu: item.item_is_menu,
                })
                .collect()
        })
    }
}

/// The notices the centre keeps, newest first, as the desk's reading names them: the banners
/// still up and the ones retired into the centre.
fn notices(notices: &tilo_desktop_surface::DesktopNotifications) -> Vec<shell_part::NoticeReading> {
    let log = notices.log();
    log.active()
        .iter()
        .chain(log.history().iter())
        .rev()
        .map(|notification| shell_part::NoticeReading {
            key: notification.id.to_string(),
            title: notification.title.clone(),
            body: notification.body.clone(),
            origin_app: notification.app_name.clone().unwrap_or_default(),
            icon: String::new(),
            kind: match notification.severity {
                tilo_desktop_surface::notifications::NotificationSeverity::Info => "info",
                tilo_desktop_surface::notifications::NotificationSeverity::Warning => "warning",
                tilo_desktop_surface::notifications::NotificationSeverity::Error => "error",
            }
            .to_owned(),
            // Seen or not is the desk's own count, the same set the bell counts: the centre's
            // rows and its header never disagree about what is new.
            read: !notices.is_unread(notification.id),
            group: notification
                .app_name
                .clone()
                .unwrap_or_else(|| notification.id.to_string()),
            wants_reply: false,
        })
        .collect()
}

/// Each action a notice offers, as its sender named it.
fn notice_actions(
    notices: &tilo_desktop_surface::DesktopNotifications,
) -> Vec<shell_part::NoticeActionReading> {
    let log = notices.log();
    log.active()
        .iter()
        .chain(log.history().iter())
        .rev()
        .flat_map(|notification| {
            notification
                .actions
                .iter()
                .map(|(key, label)| shell_part::NoticeActionReading {
                    notice: notification.id.to_string(),
                    key: key.clone(),
                    label: label.clone(),
                })
        })
        .collect()
}

/// What the clocks app puts on its live activity, as the bar reads it: the timer or stopwatch
/// running.
#[derive(serde::Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "active, stopwatch, paused and overlay are independent fields of the activity Clocks shows"
)]
struct ClocksActivity {
    #[serde(default = "active_unless_told")]
    active: bool,
    #[serde(default)]
    activity_id: Option<String>,
    /// Its label, such as `Tea`; absent from an activity that is not a timer.
    #[serde(default)]
    label: String,
    /// What the timer has left, or the stopwatch has run, in seconds.
    #[serde(default)]
    remaining_secs: u64,
    #[serde(default)]
    stopwatch: bool,
    #[serde(default)]
    paused: bool,
    /// An overlay the person may have put away.
    #[serde(default)]
    overlay: bool,
    /// The due timer Clocks wants announced, by id, and in its own words when it has them.
    #[serde(default)]
    notification_id: Option<String>,
    #[serde(default)]
    notification_title: String,
    #[serde(default)]
    notification_body: String,
}

/// An app that names no `active` shows its activity.
fn active_unless_told() -> bool {
    true
}

/// The runs of the session's agents as the desk names them: each run, each entry of its
/// conversation, and what each program in it printed and changed (shell.md §8.1), keyed so
/// an entry keeps its key while older ones are let go.
#[derive(Default)]
struct Conversations {
    runs: Vec<shell_part::RunReading>,
    entries: Vec<shell_part::RunEntryReading>,
    refs: Vec<shell_part::EntryRefReading>,
}

impl Conversations {
    fn of(runs: &[tilo_agent_runs::wire::Run]) -> Self {
        use tilo_agent_runs::wire::Said;
        let mut made = Self::default();
        for run in runs {
            made.runs.push(shell_part::RunReading {
                key: run.id.clone(),
                title: run.title.clone(),
                agent: run.agent.clone(),
                phase: phase_name(run.phase).to_owned(),
                active: run.phase.active(),
                live: run
                    .live
                    .as_ref()
                    .filter(|live| live.end.is_none())
                    .map(|live| live.text.clone())
                    .unwrap_or_default(),
            });
            for entry in &run.transcript {
                let key = format!("{}/{}", run.id, entry.seq);
                let mut reading = shell_part::RunEntryReading {
                    key: key.clone(),
                    run: run.id.clone(),
                    seq: i64::try_from(entry.seq).unwrap_or(i64::MAX),
                    author: entry.author.clone(),
                    by_agent: entry.by_agent(),
                    ..shell_part::RunEntryReading::default()
                };
                match &entry.said {
                    Said::Prose { text } => reading.text.clone_from(text),
                    Said::Step { step } => {
                        reading.step = true;
                        match serde_json::from_value::<tilo_reference::ProgramStep>(step.clone()) {
                            Ok(step) => made.step(&key, &mut reading, step),
                            Err(error) => {
                                // The runs wrote it from a `ProgramStep`: one that does not read
                                // back is a bug of theirs, said where it shows and logged.
                                tracing::error!(%error, entry = key, "a run's step does not read");
                                "failed".clone_into(&mut reading.outcome);
                                reading.answer = format!("This step does not read: {error}");
                            }
                        }
                    }
                }
                made.entries.push(reading);
            }
        }
        made
    }

    /// The program `step` into `reading`, and what it printed and changed under `entry`.
    fn step(
        &mut self,
        entry: &str,
        reading: &mut shell_part::RunEntryReading,
        step: tilo_reference::ProgramStep,
    ) {
        use tilo_reference::StepOutcome;
        reading.text = step.program;
        match step.outcome {
            StepOutcome::Done => "done",
            StepOutcome::Rejected => "rejected",
            StepOutcome::Preview => "preview",
            StepOutcome::Asked => "asked",
            StepOutcome::Failed => "failed",
        }
        .clone_into(&mut reading.outcome);
        reading.answer = step.answer.join("\n");
        let printed = step.refs.into_iter().map(|at| (at, false, String::new()));
        let changed = step
            .changes
            .into_iter()
            .map(|change| (change.resource, true, change.action));
        for (number, (at, changed, action)) in printed.chain(changed).enumerate() {
            self.refs.push(shell_part::EntryRefReading {
                key: format!("{entry}/{number}"),
                entry: entry.to_owned(),
                changed,
                resource: at.reference.id,
                app_id: at.app,
                type_kind: at.reference.kind,
                label: at.reference.label,
                change: action,
            });
        }
    }
}

/// A run's phase, by the name its reading gives it.
const fn phase_name(phase: tilo_agent_runs::wire::Phase) -> &'static str {
    use tilo_agent_runs::wire::Phase;
    match phase {
        Phase::Approved => "approved",
        Phase::Accepted => "accepted",
        Phase::Planning => "planning",
        Phase::Executing => "executing",
        Phase::Waiting => "waiting",
        Phase::Done => "done",
        Phase::Failed => "failed",
        Phase::Stopped => "stopped",
    }
}

/// The name of the strip in front in the workspace in front, when it has one: what the bar
/// names after the workspace.
fn strip_in_front(tree: &DesktopTree) -> String {
    let Some(workspace) = tree.active_workspace() else {
        return String::new();
    };
    let active = tree
        .active_strip
        .get(&tree.active)
        .and_then(|id| workspace.strips.iter().find(|strip| strip.id == *id))
        .or_else(|| workspace.strips.first());
    active.map_or_else(String::new, |strip| strip.name.trim().to_owned())
}

impl Desk {
    /// Bring the window the switcher's walk reached to the front, and close the switcher. The
    /// walk counts from the window in front, over the windows its scope crosses, wrapping.
    fn commit_switcher(&self) {
        let windows = self.handle.read(|tree| windows(tree, &self.handle));
        let (walked, front) = walked(&windows, self.surfaces.overlays.scope());
        let reached = self
            .surfaces
            .overlays
            .reached(walked.len(), front)
            .and_then(|at| walked.get(at).copied())
            .and_then(|at| windows.get(at));
        if let Some(tab) = reached.and_then(|window| window.key.parse::<u64>().ok()) {
            self.handle.focus_tab(tab);
        }
        self.surfaces.overlays.forget_walk();
        self.surfaces.overlays.show(crate::overlays::Overlay::Rest);
    }
}

impl Desk {
    /// The id of the desk's workspace `key`, or of the workspace in front
    /// when the caller names none.
    fn workspace_id(&self, workspace: Option<&str>) -> Result<RoomId, DoorError> {
        let workspace_id = self.handle.read(|tree| {
            let workspace = match workspace {
                Some(key) => tree
                    .workspaces
                    .iter()
                    .find(|candidate| candidate.slot.0.to_string() == key),
                None => tree.active_workspace(),
            };
            workspace.map(Workspace::room)
        });
        workspace_id.ok_or_else(|| match workspace {
            Some(key) => {
                DoorError::InvalidArguments(format!("there is no workspace {key} on this device"))
            }
            None => DoorError::Failed("no workspace is in front".to_owned()),
        })
    }

    /// The invitations into a workspace this device's person made that are still
    /// redeemable, as the workspace's host lists them: its id in hex, the part it
    /// grants, whom it is addressed to, how it names them and when it lapses.
    fn invitations(&self, workspace: Option<&str>) -> Result<Started, DoorError> {
        let engine = self.engine()?;
        let workspace_id = self.workspace_id(workspace)?;
        let invitations = engine
            .invitations(&workspace_id.to_string(), std::time::SystemTime::now())
            .map_err(DoorError::Failed)?;
        let value = Value::Array(invitations.iter().map(invitation_json).collect());
        Ok(Started::Done(tilo_actions::ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        }))
    }

    /// Cancel one of them, from the device that issued it: the link admits nobody
    /// from now on.
    fn cancel_invitation(
        &self,
        invite: &str,
        workspace: Option<&str>,
    ) -> Result<Started, DoorError> {
        let engine = self.engine()?;
        let workspace_id = self.workspace_id(workspace)?;
        let cancelled = engine
            .cancel_invitation(
                &workspace_id.to_string(),
                invite,
                std::time::SystemTime::now(),
            )
            .map_err(DoorError::Failed)?;
        Ok(Started::Done(tilo_actions::ActionOutput {
            value: Some(serde_json::json!({"cancelled": cancelled})),
            resources: Vec::new(),
        }))
    }

    /// This device's engine: invitations are its record's, and a session without
    /// one has none.
    fn engine(&self) -> Result<&crate::engine::Engine, DoorError> {
        self.engine
            .as_deref()
            .ok_or_else(|| DoorError::Failed("this session holds no workspace's record".to_owned()))
    }
}

/// A pending invitation as listing invitations reports it, in the workspace host's
/// own words: a client that lists them from a host reads the same fields.
fn invitation_json(invitation: &PendingInvitation) -> Value {
    serde_json::json!({
        "invite": hex::encode(invitation.invite_id),
        "role": role_name(invitation.role),
        "recipient": invitation.recipient_user_public_key.map(hex::encode),
        "invitee_name": invitation.invitee_name,
        "expires_at_ns": invitation.expires_at_ns,
    })
}

/// The part an invitation grants, as the workspace's settings name it.
const fn role_name(role: MemberRole) -> &'static str {
    match role {
        MemberRole::Viewer => "viewer",
        MemberRole::Commenter => "commenter",
        MemberRole::Editor => "editor",
        MemberRole::Moderator => "moderator",
    }
}

impl Desk {
    /// Open or close the running application's tray item `key`, as a press on its icon does:
    /// its window, or its menu when the item is one.
    fn open_tray_item(&self, key: &str) -> Result<Started, DoorError> {
        let Some(tray) = &self.tray else {
            return Err(DoorError::Failed(
                "no tray is served: the press reaches no application".to_owned(),
            ));
        };
        let Some(item) = tray.snapshot().into_iter().find(|item| item.key == key) else {
            return Err(DoorError::InvalidArguments(format!(
                "there is no tray item {key}"
            )));
        };
        let request = if item.item_is_menu {
            tilo_tray_host::TrayRequest::ContextMenu { x: 0, y: 0 }
        } else {
            tilo_tray_host::TrayRequest::Activate { x: 0, y: 0 }
        };
        tray.request_or_else(key, request, |_| {})
            .map_err(DoorError::Failed)?;
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }
}

/// The desk's resource, named `label`: what it publishes, and what the session's package
/// describes of it ([`crate::session_package`]).
pub(crate) fn desk_spec(label: String) -> ResourceSpec {
    offer::<DeskEffect>(ResourceSpec::new(DESK_RESOURCE, DESK_KIND, label))
}

impl Service for Desk {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![desk_spec(self.person.clone())]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == DESK_RESOURCE).then(|| reading(&self.reading()))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        if resource != DESK_RESOURCE {
            return Err(DoorError::Unknown(ActionAddress::new(resource, action)));
        }
        let effect = decode(&self.resources(), resource, action, arguments)?;
        match &effect {
            DeskEffect::AnswerAsk => {
                self.handle.answer_personal_ask();
                return Ok(Started::Done(tilo_actions::ActionOutput::default()));
            }
            DeskEffect::PutOffAsk => {
                self.handle.put_off_personal_ask();
                return Ok(Started::Done(tilo_actions::ActionOutput::default()));
            }
            DeskEffect::OpenTrayItem { key } => return self.open_tray_item(key),
            DeskEffect::Invitations { workspace } => {
                return self.invitations(workspace.as_deref());
            }
            DeskEffect::CancelInvitation { invite, workspace } => {
                return self.cancel_invitation(invite, workspace.as_deref());
            }
            _ => {}
        }
        if let DeskEffect::ShowSurface { surface } = &effect {
            self.surfaces.toggle(surface);
            return Ok(Started::Done(tilo_actions::ActionOutput::default()));
        }
        if let DeskEffect::StopAgent { agent } = &effect {
            if !self.seats.stop(agent) {
                return Err(DoorError::InvalidArguments(format!(
                    "no agent {agent} works here"
                )));
            }
            return Ok(Started::Done(tilo_actions::ActionOutput::default()));
        }
        if let DeskEffect::CloseSurface { surface } = &effect {
            self.surfaces.close(surface);
            return Ok(Started::Done(tilo_actions::ActionOutput::default()));
        }
        if matches!(effect, DeskEffect::ClearNotices) {
            self.notices.clear_all();
            return Ok(Started::Done(tilo_actions::ActionOutput::default()));
        }
        if let DeskEffect::WalkSwitcher { steps, scope } = &effect {
            self.surfaces
                .overlays
                .walk(scope.as_deref(), steps.unwrap_or(1));
            return Ok(Started::Done(tilo_actions::ActionOutput::default()));
        }
        if matches!(effect, DeskEffect::CommitSwitcher) {
            self.commit_switcher();
            return Ok(Started::Done(tilo_actions::ActionOutput::default()));
        }
        if let DeskEffect::ActOnNotice { notice, action } = &effect {
            let id = notice.parse::<u64>().map_err(|_| {
                DoorError::InvalidArguments(format!("`{notice}` is no notice's id"))
            })?;
            self.notices.invoke_action(id, action);
            return Ok(Started::Done(tilo_actions::ActionOutput::default()));
        }
        if let DeskEffect::DismissNotice { notice } = &effect {
            let id = notice.parse::<u64>().map_err(|_| {
                DoorError::InvalidArguments(format!("`{notice}` is no notice's id"))
            })?;
            self.notices.dismiss_history(id);
            return Ok(Started::Done(tilo_actions::ActionOutput::default()));
        }
        let DeskEffect::SwitchWorkspace { workspace } = effect else {
            return Err(DoorError::InvalidArguments(
                "the desk takes switch-workspace, show-surface or close-surface".to_owned(),
            ));
        };
        // The workspace's key names it; the handle brings it to the front.
        let known = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .any(|candidate| candidate.slot.0.to_string() == workspace)
        });
        if !known {
            return Err(DoorError::InvalidArguments(format!(
                "there is no workspace {workspace} on this device"
            )));
        }
        self.handle
            .dispatch(ChromeEffect::SwitchWorkspace {
                workspace: Some(workspace),
                delta: None,
            })
            .map_err(DoorError::Failed)?;
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlays::Overlay;

    fn surfaces(status_panel: bool, shell: bool) -> Surfaces {
        Surfaces {
            status_panel,
            shell,
            panel: tilo_desktop::overlays::Overlays::new(),
            overlays: Rc::new(crate::overlays::Overlays::new()),
        }
    }

    #[test]
    fn a_shell_draws_the_quick_settings_it_shows() {
        for status_panel in [false, true] {
            let surfaces = surfaces(status_panel, true);
            surfaces.toggle(crate::quick_settings::SURFACE);
            assert_eq!(surfaces.overlays.here(), Overlay::QuickSettings.name());
            assert!(
                !surfaces
                    .panel
                    .is_open(tilo_desktop::overlays::Slot::QuickSettings)
            );
            surfaces.close(crate::quick_settings::SURFACE);
            assert_eq!(surfaces.overlays.here(), Overlay::Rest.name());
        }
    }

    #[test]
    fn the_desk_leaves_the_quick_settings_to_the_panels_app_it_has() {
        let surfaces = surfaces(true, false);
        surfaces.toggle(crate::quick_settings::SURFACE);
        assert!(
            surfaces
                .panel
                .is_open(tilo_desktop::overlays::Slot::QuickSettings)
        );
        assert_eq!(surfaces.overlays.here(), Overlay::Rest.name());
        surfaces.close(crate::quick_settings::SURFACE);
        assert!(
            !surfaces
                .panel
                .is_open(tilo_desktop::overlays::Slot::QuickSettings)
        );
    }

    #[test]
    fn without_the_panels_app_the_desk_shows_the_shells_quick_settings() {
        let surfaces = surfaces(false, false);
        surfaces.open(crate::quick_settings::SURFACE);
        assert_eq!(surfaces.overlays.here(), Overlay::QuickSettings.name());
        assert!(
            !surfaces
                .panel
                .is_open(tilo_desktop::overlays::Slot::QuickSettings)
        );
    }
}
