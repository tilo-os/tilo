//! This device's desk as a resource of the session's: the time in the person's style, who they
//! are, their notifications, the workspaces on this device and the strip in front, and the
//! tasks running for them. A shell's views read it (as `session.desk` resources,
//! [`tilo_session_core::shell`]), and so does a script or an agent, through the door, by the same
//! address: one reading for every reader.

use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop::desktop_host::DesktopAsk;
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId};
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::navigation::ChromeEffect;
use tilo_peer_state::join::MemberRole;
use tilo_peer_state::join::invitations::PendingInvitation;
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};
use tilo_session_core::shell::DESK_RESOURCE;
use tilo_session_core::shell::{self, DeskReading};

use crate::desk_windows::{reaching, windows};
use crate::i18n::t;
use crate::notifications::NoticeSignals;

/// What the desk's reading is made from ([`Desk::key`]): equal when the shell's views need not run
/// again.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DeskKey {
    /// The revisions, the minute, the signals and the names the reading is made of.
    reading: (u64, i64, bool, u64, u64, u64, u64, u64, u64, String),
    /// The agents seated at the person's agent door.
    agents: u64,
    /// Whether the switcher's panel shows: it comes up on its own while Alt is held, with no
    /// revision of its own to say so.
    revealed: bool,
    /// The windows whose app stopped answering ([`DesktopHandle::unresponsive_revision`]).
    not_responding: u64,
    /// The desk styles' revision: a package installed or taken away moves it (shell.md §6).
    styles: u64,
}

/// The kind of the desk's resource: the origin kind a shell's type holds.
pub(crate) const DESK_KIND: &str = "session.desk";

/// The scopes a walk of the switcher crosses, as the walk's action names them: the whole workspace
/// (Alt+Tab), or the app the window in front shows (Alt and the grave key).
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
    /// Open or close the running application's tray item `key`: its window, or its menu when the
    /// item is one
    #[effect(row, label = "Open")]
    OpenTrayItem { key: String },
    /// The invitations this device's person made into a workspace that can still be
    /// redeemed, in the words the workspace's host lists them by
    Invitations {
        /// The workspace, by its key; the one in front when absent
        #[param("The workspace, by its key; the one in front when absent")]
        workspace: Option<String>,
    },
    /// Make an invitation into a workspace from this device: the link to hand to whoever is
    /// being invited, which their device joins with
    IssueInvitation {
        /// The part the invitation brings them in at: `viewer`, `commenter`, `editor` or
        /// `moderator`; editor when absent
        #[param(
            "The part the invitation brings them in at: `viewer`, `commenter`, `editor` or `moderator`"
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
    ShowInvite {
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
    /// Alt+Tab walks the workspace's windows; Alt and the grave key the ones the app in front
    /// shows.
    WalkSwitcher {
        #[param("How far to walk; 1 the next window, -1 the one before")]
        steps: Option<i64>,
        #[param(
            "Which windows the walk crosses: `workspace` for every window of the workspace, `app` for the ones the app in front shows",
            choices = SWITCHER_SCOPES
        )]
        scope: Option<String>,
    },
    /// Choose the window the walk reached, without crossing the ones between: `at` is its place
    /// among the windows the walk crosses, `0` the first of them and `-1` the last. Home, End and a
    /// card under the pointer choose by place.
    SelectSwitcher {
        #[param("The window's place among the ones the walk crosses; -1 for the last")]
        at: i64,
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
    /// Whether the device has the quick settings' app; without, the status opens the shell's own
    /// view of them.
    pub(crate) status_panel: bool,
    /// Whether the shell draws the whole screen over the desk: the desk draws none of the panel's
    /// app there, so the quick settings are the shell's own view, as every other surface is.
    pub(crate) shell: bool,
    /// The stack the panel's own app is placed in.
    pub(crate) panel: tilo_desktop::overlays::Overlays,
    /// The surface the shell's views show (`crate::overlays::Overlay`).
    pub(crate) overlays: Rc<crate::overlays::Overlays>,
    /// Whether the overview is the surface up, as the desk's own widgets fold behind it and as
    /// whoever asks for the desk's state reads it. It turns with the surface itself, so a person's
    /// press answers at once rather than a frame later.
    pub(crate) overview: Rc<std::cell::Cell<bool>>,
}

impl Surfaces {
    /// Whether the quick settings stand in the panel's own app: the device has it, and the desk
    /// draws the app it stands in.
    pub(crate) fn panel_shows(&self, surface: &str) -> bool {
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
                self.clear_shell();
                self.panel.open(Slot::QuickSettings, None);
            }
            return;
        }
        self.panel_closes();
        self.overlays.toggle(surface);
        self.clear_beside();
        self.settled();
    }

    /// Show the surface `name` names (`show-surface`), whether or not it is up.
    pub(crate) fn open(&self, name: &str) {
        if self.panel_shows(name) {
            self.clear_shell();
            self.panel
                .open(tilo_desktop::overlays::Slot::QuickSettings, None);
            return;
        }
        self.panel_closes();
        self.overlays.open(name);
        self.clear_beside();
        self.settled();
    }

    /// One surface at a time: the desk's own popovers go down under the shell's view of the surface
    /// just opened.
    fn clear_beside(&self) {
        if self.overlays.here() == crate::overlays::Overlay::Rest.name() {
            return;
        }
        self.clear_panel();
    }

    /// Take every popover of the desk's own down: a surface outside its stack is coming up instead.
    pub(crate) fn clear_panel(&self) {
        for slot in tilo_desktop::overlays::Slot::ALL {
            if slot.stands_beside() {
                self.panel.close(slot);
            }
        }
    }

    /// One surface at a time: the shell's view goes down under the desk's own popover or the
    /// panel's app.
    pub(crate) fn clear_shell(&self) {
        let here = self.overlays.here();
        if here == crate::overlays::Overlay::Rest.name() {
            return;
        }
        self.overlays.close(&here);
        self.settled();
    }

    /// Whether any of the desk's own popovers stands over the windows now.
    pub(crate) fn beside_up(&self) -> bool {
        tilo_desktop::overlays::Slot::ALL
            .into_iter()
            .any(|slot| slot.stands_beside() && self.panel.is_open(slot))
    }

    /// Close `surface` (`close-surface`), when it is the one up.
    pub(crate) fn close(&self, surface: &str) {
        if self.panel_shows(surface) {
            self.panel
                .close(tilo_desktop::overlays::Slot::QuickSettings);
            return;
        }
        self.overlays.close(surface);
        self.settled();
    }

    /// Close the panel when a surface of the shell's own opens: one surface stands at a time,
    /// so the centre is not opened behind the panel that asked for it.
    fn panel_closes(&self) {
        if self
            .panel
            .is_open(tilo_desktop::overlays::Slot::QuickSettings)
        {
            self.panel
                .close(tilo_desktop::overlays::Slot::QuickSettings);
        }
    }

    /// Keep the desk's reading of the overview in step with the surface just changed: the desk
    /// folds behind it and its state names it as up the moment the person's press opens it,
    /// instead of after the shell's next frame.
    fn settled(&self) {
        self.overview
            .set(self.overlays.here() == crate::overlays::Overlay::Overview.name());
    }
}

/// The desk styles every installed package offers, with the revision that moves when the
/// session re-reads its store: a package installed or taken away moves the desk's reading.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Styles {
    pub(crate) revision: u64,
    pub(crate) list: Vec<shell::DeskStyleReading>,
    /// The store's fingerprint the list was read at: a store that reads the same is not read
    /// again.
    pub(crate) fingerprint: Option<String>,
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
    /// What each window of the desk shows, as its app reports it: where a person's record stands,
    /// so the reading places them in the window they work in.
    pub(crate) notes: std::sync::Arc<crate::continuations::Continuations>,
    /// The apps' live activities: Clocks' timer is the bar's clock beside the time, and the person
    /// putting the floating one away hides it until Clocks shows something new.
    pub(crate) activities: Rc<crate::chrome::Activities>,
    /// The running applications' tray items, when their watcher serves: the bar shows them,
    /// and the person's clicks go back through it.
    pub(crate) tray: Option<std::sync::Arc<tilo_tray_host::Tray>>,
    /// The notification centre the reading lists, and what it last said.
    pub(crate) notices: tilo_desktop_surface::DesktopNotifications,
    /// The agents the person admitted at its agent door: each works here as itself, and the bar
    /// shows it as its own presence.
    pub(crate) seats: std::sync::Arc<crate::agent_door::Seats>,
    /// This device's workspaces' record, when the session holds one: the invitations a workspace's
    /// people made are kept there, by the device that made them.
    pub(crate) engine: Option<std::sync::Arc<crate::engine::Engine>>,
    /// What the desk asks the session for while it runs: opening the invite sheet, which is the
    /// session's own surface and not a layer the desk draws.
    pub(crate) asks: Rc<std::cell::RefCell<Vec<tilo_desktop::desktop_host::DesktopAsk>>>,
    /// The desk styles every installed package offers, as `session.desk_styles` reads them
    /// (shell.md §6): what the person's switch offers. The session fills it from its package
    /// store, and moves it when a package is installed or taken away.
    pub(crate) styles: std::rc::Rc<std::cell::RefCell<Styles>>,
}

/// The pieces a session's desk is made of, once: the same seats, surfaces and records reach the
/// desk's reading and every part of the shell.
pub(crate) struct Shared {
    pub(crate) seats: std::sync::Arc<crate::agent_door::Seats>,
    pub(crate) handle: DesktopHandle,
    pub(crate) person: String,
    pub(crate) signals: Rc<NoticeSignals>,
    pub(crate) activities: Rc<crate::chrome::Activities>,
    pub(crate) surfaces: Surfaces,
    pub(crate) sync: Option<Rc<crate::sync::Client>>,
    pub(crate) notes: std::sync::Arc<crate::continuations::Continuations>,
    pub(crate) notices: tilo_desktop_surface::DesktopNotifications,
    pub(crate) tray: Option<std::sync::Arc<tilo_tray_host::Tray>>,
    pub(crate) engine: Option<std::sync::Arc<crate::engine::Engine>>,
    pub(crate) asks: Rc<std::cell::RefCell<Vec<tilo_desktop::desktop_host::DesktopAsk>>>,
}

/// A factory making the session's desk, its styles read from the person's store as they move
/// (shell.md §6): called once for the desk's own service and once for the shell, each a view of
/// the same seats, surfaces and records.
pub(crate) fn desk_from(shared: Shared) -> impl Fn() -> Desk {
    let styles = Rc::new(std::cell::RefCell::new(Styles::default()));
    move || Desk {
        seats: std::sync::Arc::clone(&shared.seats),
        handle: shared.handle.clone(),
        person: shared.person.clone(),
        signals: Rc::clone(&shared.signals),
        activities: Rc::clone(&shared.activities),
        surfaces: shared.surfaces.clone(),
        sync: shared.sync.clone(),
        notes: std::sync::Arc::clone(&shared.notes),
        notices: shared.notices.clone(),
        tray: shared.tray.clone(),
        engine: shared.engine.clone(),
        asks: Rc::clone(&shared.asks),
        styles: Rc::clone(&styles),
    }
}

impl Desk {
    /// What the reading is made from, without making it: the tree's revision, the minute,
    /// what the notifications say and who is here. The reading changes only when this does.
    pub(crate) fn key(&self) -> DeskKey {
        DeskKey {
            reading: (
                self.handle.revision(),
                ui::clock::local_now().timestamp().div_euclid(60),
                self.signals.quiet(),
                self.sync
                    .as_ref()
                    .map_or(0, |sync| sync.here().borrow().revision()),
                self.activities.board().revision(),
                self.handle.personal_ask_revision(),
                self.tray.as_ref().map_or(0, |tray| tray.revision()),
                self.notices.revision(),
                self.surfaces.overlays.revision(),
                self.surfaces.overlays.here(),
            ),
            styles: self.styles.borrow().revision,
            agents: self.seats.revision(),
            revealed: self.surfaces.overlays.revealed(std::time::Instant::now()),
            // Which apps stopped answering is the compositor's, published through the handle beside
            // the tree: it changes without the tree moving.
            not_responding: self.handle.unresponsive_revision(),
        }
    }

    /// When the desk has something to draw that it has not drawn yet, without a frame in between:
    /// the instant the switcher's panel comes up while Alt is held on it, or the minute the bar's
    /// clock shows next.
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

    /// The desk's windows, with the one the switcher's walk has reached marked focused while its
    /// panel shows: the tile its walk chose is the one lifted. A quick flick shows no panel, so it
    /// lifts nothing either.
    fn windows_reaching(
        &self,
        tree: &DesktopTree,
        now: std::time::Instant,
    ) -> Vec<shell::WindowReading> {
        let mut windows = windows(tree, &self.handle);
        // The app as people know it, never the id: a window is named by what it shows, and its
        // `app_title` is a person's own name for the window, not its app's name.
        for window in &mut windows {
            window.origin_app = crate::app_names::app_name(&window.app_key);
        }
        if self.surfaces.overlays.revealed(now)
            && let Some(at) = reaching(&self.surfaces.overlays, &windows)
        {
            for (index, window) in windows.iter_mut().enumerate() {
                window.focused = index == at;
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
                sync.people(tree, &self.notes.records(), &|_: &str| None)
                    .into_iter()
                    .map(|person| shell::PersonReading {
                        key: person.id,
                        name: person.label,
                    })
                    .collect()
            })
        });
        self.handle.read(|tree| DeskReading {
            person: self.person.clone(),
            clock: ui::clock::time_of_day(&now),
            timer: self.timer(),
            ask: self.handle.personal_ask().map(|ask| shell::AskReading {
                question: ask.title,
                detail: ask.body,
                answer: ask.action.title,
                open: ask.open,
            }),
            weekday: ui::format::weekday_short(chrono::Datelike::weekday(&now)),
            date: ui::format::month_day(now.date_naive()),
            quiet: self.signals.quiet(),
            workspace: tree
                .active_workspace()
                .map(|workspace| workspace.name.clone())
                .unwrap_or_default(),
            strip: strip_in_front(tree),
            workspaces: tree
                .workspaces
                .iter()
                .map(|workspace| shell::WorkspaceReading {
                    key: workspace.slot.0.to_string(),
                    name: workspace.name.clone(),
                    current: workspace.slot == tree.active,
                })
                .collect(),
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
                .map(|workspace| shell::RoomReading {
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
            styles: self.styles.borrow().list.clone(),
            // What the screen says of its edges moves without the desk; the link publishes it
            // beside this reading and carries it over (`SessionLink::publish_edges`).
            ..DeskReading::default()
        })
    }

    /// The floating timer as the web shell reads it: Clocks' summary while it floats over the
    /// windows.
    fn timer(&self) -> Option<shell::TimerReading> {
        let published = self.activities.published(crate::chrome::CLOCKS_APP)?;
        if !published.overlay {
            return None;
        }
        let summary = published.summary?;
        let countdown = summary.countdown.unwrap_or_default();
        Some(shell::TimerReading {
            activity_id: (!summary.id.is_empty()).then(|| summary.id.clone()),
            label: summary.title.clone(),
            remaining_secs: u64::try_from(countdown).unwrap_or_default(),
            stopwatch: summary.kind == ui::live_activity::Kind::Stopwatch,
            // A paused timer is one with time still to run before it is due.
            paused: summary.tone == ui::live_activity::Tone::Neutral && countdown > 0,
        })
    }

    /// The due event Clocks wants announced, by its id and in the app's own words when it has them
    /// (the desktop's otherwise): what the bar announces once.
    pub(crate) fn due_timer(&self) -> Option<(String, String, String)> {
        let announced = self
            .activities
            .published(crate::chrome::CLOCKS_APP)?
            .announce?;
        let title = if announced.title.is_empty() {
            crate::i18n::t("clock-due-title")
        } else {
            announced.title
        };
        let body = if announced.body.is_empty() {
            crate::i18n::t("clock-due-body")
        } else {
            announced.body
        };
        Some((announced.id, title, body))
    }

    /// The running applications' tray items, in the order they registered: each's key, the app's
    /// own words for it, and whether choosing it opens a menu rather than its window.
    fn tray_items(&self) -> Vec<shell::TrayReading> {
        self.tray.as_ref().map_or_else(Vec::new, |tray| {
            tray.snapshot()
                .into_iter()
                .map(|item| shell::TrayReading {
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

/// The notices the centre keeps, newest first, as the desk's reading names them: the banners still
/// up and the ones retired into the centre.
fn notices(notices: &tilo_desktop_surface::DesktopNotifications) -> Vec<shell::NoticeReading> {
    let log = notices.log();
    log.active()
        .iter()
        .chain(log.history().iter())
        .rev()
        .map(|notification| shell::NoticeReading {
            key: notification.id.to_string(),
            title: notification.title.clone(),
            body: notification.body.clone(),
            origin_app: notification.app_name.clone().unwrap_or_default(),
            kind: match notification.severity {
                tilo_desktop_surface::notifications::NotificationSeverity::Info => "info",
                tilo_desktop_surface::notifications::NotificationSeverity::Warning => "warning",
                tilo_desktop_surface::notifications::NotificationSeverity::Error => "error",
            }
            .to_owned(),
            seen: !notices.is_unread(notification.id),
        })
        .collect()
}

/// Each action a notice offers, as its sender named it.
fn notice_actions(
    notices: &tilo_desktop_surface::DesktopNotifications,
) -> Vec<shell::NoticeActionReading> {
    let log = notices.log();
    log.active()
        .iter()
        .chain(log.history().iter())
        .rev()
        .flat_map(|notification| {
            notification
                .actions
                .iter()
                .map(|(key, label)| shell::NoticeActionReading {
                    notice: notification.id.to_string(),
                    key: key.clone(),
                    label: label.clone(),
                })
        })
        .collect()
}

/// The name of the strip in front in the workspace in front, when it has one: what the bar names
/// after the workspace.
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
    /// The id of the desk's workspace `key`, or of the workspace in front when the caller names
    /// none.
    fn workspace_id(&self, workspace: Option<&str>) -> Result<WorkspaceId, DoorError> {
        let workspace_id = self.handle.read(|tree| {
            let workspace = match workspace {
                Some(key) => tree
                    .workspaces
                    .iter()
                    .find(|candidate| candidate.slot.0.to_string() == key),
                None => tree.active_workspace(),
            };
            workspace.map(Workspace::id)
        });
        workspace_id.ok_or_else(|| match workspace {
            Some(key) => {
                DoorError::InvalidArguments(format!("there is no workspace {key} on this device"))
            }
            None => DoorError::Failed("no workspace is in front".to_owned()),
        })
    }

    /// The invitations into a workspace this device's person made that are still redeemable, as the
    /// workspace's host lists them: its id in hex, the part it grants, whom it is addressed to, how
    /// it names them and when it lapses.
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

    /// Cancel one of them, from the device that issued it: the link admits nobody from now on.
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

    /// Hand out a link into a workspace, made by this device's own person: the invitation the desk
    /// lists from now on, in the text another device joins with.
    fn issue_invitation(
        &self,
        role: Option<&str>,
        invitee_name: Option<String>,
        recipient: Option<&str>,
        expires_in_seconds: Option<u64>,
        workspace: Option<&str>,
    ) -> Result<Started, DoorError> {
        let engine = self.engine()?;
        let room = self.workspace_id(workspace)?;
        let role = match role {
            None => MemberRole::Editor,
            Some(name) => role_named(name).ok_or_else(|| {
                DoorError::InvalidArguments(format!("`{name}` is no part of a workspace"))
            })?,
        };
        let recipient = recipient
            .map(|person| {
                let not_a_key = || {
                    DoorError::InvalidArguments(format!(
                        "`{person}` is not a person's key, 64 hex characters"
                    ))
                };
                let key = hex::decode(person).map_err(|_| not_a_key())?;
                <[u8; 32]>::try_from(key.as_slice()).map_err(|_| not_a_key())
            })
            .transpose()?;
        let invitee_name = match (invitee_name, recipient) {
            (None, Some(recipient)) => engine.name_of(&room.to_string(), recipient),
            (name, _) => name,
        };
        // The link names this device, which answers whoever redeems it: a session taking part in no
        // transport has no address to name.
        let endpoint = self
            .sync
            .as_ref()
            .map(|sync| sync.endpoint_addr())
            .ok_or_else(|| {
                DoorError::Failed(
                    "this session takes part in no transport, so no link can name it".to_owned(),
                )
            })?;
        let issued = engine
            .issue_invitation(
                &room.to_string(),
                role,
                invitee_name,
                recipient,
                expires_in_seconds.unwrap_or(DEFAULT_INVITE_SECONDS),
                &endpoint,
                std::time::SystemTime::now(),
            )
            .map_err(DoorError::Failed)?;
        Ok(Started::Done(tilo_actions::ActionOutput {
            value: Some(serde_json::json!({
                "invite": issued.invite,
                "link": issued.link,
                "role": role_name(issued.role),
                "expires_at_ns": issued.expires_at_ns,
            })),
            resources: Vec::new(),
        }))
    }

    /// What an invitation verb answers, when `effect` is one of them: the links that still work,
    /// one made, or one taken back.
    fn invitation(&self, effect: &DeskEffect) -> Option<Result<Started, DoorError>> {
        match effect {
            DeskEffect::Invitations { workspace } => Some(self.invitations(workspace.as_deref())),
            DeskEffect::IssueInvitation {
                role,
                invitee_name,
                recipient,
                expires_in_seconds,
                workspace,
            } => Some(self.issue_invitation(
                role.as_deref(),
                invitee_name.clone(),
                recipient.as_deref(),
                *expires_in_seconds,
                workspace.as_deref(),
            )),
            DeskEffect::CancelInvitation { invite, workspace } => {
                Some(self.cancel_invitation(invite, workspace.as_deref()))
            }
            _ => None,
        }
    }

    /// This device's engine: invitations are its record's, and a session without one has none.
    fn engine(&self) -> Result<&crate::engine::Engine, DoorError> {
        self.engine
            .as_deref()
            .ok_or_else(|| DoorError::Failed("this session holds no workspace's record".to_owned()))
    }
}

impl Desk {
    /// Open the invite sheet on a workspace: the workspace comes to the front, and the sheet the
    /// bar's button opens is shown on it, through the session, which holds the surface.
    fn show_invite(&self, workspace: Option<&str>) -> Result<Started, DoorError> {
        let workspace = match workspace {
            Some(key) => key.to_owned(),
            None => self
                .handle
                .read(|tree| {
                    tree.active_workspace()
                        .map(|active| active.slot.0.to_string())
                })
                .ok_or_else(|| DoorError::Failed("no workspace is in front".to_owned()))?,
        };
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
        let active = self.handle.read(|tree| {
            tree.active_workspace()
                .map(|active| active.slot.0.to_string())
        });
        if active.as_deref() != Some(workspace.as_str()) {
            self.handle
                .dispatch(ChromeEffect::SwitchWorkspace {
                    workspace: Some(workspace),
                    delta: None,
                })
                .map_err(DoorError::Failed)?;
        }
        self.asks
            .borrow_mut()
            .push(DesktopAsk::Intent(ChromeIntent::Invite));
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }
}

/// A pending invitation as listing invitations reports it, in the workspace host's own words: a
/// client that lists them from a host reads the same fields.
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

/// The part `name` names, as [`role_name`] writes it.
fn role_named(name: &str) -> Option<MemberRole> {
    match name {
        "viewer" => Some(MemberRole::Viewer),
        "commenter" => Some(MemberRole::Commenter),
        "editor" => Some(MemberRole::Editor),
        "moderator" => Some(MemberRole::Moderator),
        _ => None,
    }
}

/// How long an invitation the desk hands out without being told otherwise works for: an hour, as a
/// person's invitation does.
const DEFAULT_INVITE_SECONDS: u64 = 3_600;

impl Desk {
    /// Open or close the running application's tray item `key`, as a press on its icon does: its
    /// window, or its menu when the item is one.
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

/// The desk's resource, named `label`: what it publishes, and what the session's package describes
/// of it ([`crate::session_package`]).
pub(crate) fn desk_spec(label: String) -> ResourceSpec {
    let mut resource = offer::<DeskEffect>(ResourceSpec::new(DESK_RESOURCE, DESK_KIND, label));
    // The desk answers in the person's language: the question on their avatar, the tray item, and
    // stopping an agent.
    for spec in &mut resource.actions {
        let label = match spec.id.as_str() {
            "answer-ask" => t("session-desk-answer-ask"),
            "put-off-ask" => t("session-desk-ask-later"),
            "open-tray-item" => t("session-desk-open"),
            "stop-agent" => t("session-desk-stop-agent"),
            _ => continue,
        };
        spec.type_mut().label = label;
    }
    resource
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
            _ => {}
        }
        if let Some(answer) = self.invitation(&effect) {
            return answer;
        }
        if let DeskEffect::ShowInvite { workspace } = &effect {
            return self.show_invite(workspace.as_deref());
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
            crate::desk_windows::walk_switcher(
                &self.handle,
                &self.surfaces.overlays,
                scope.as_deref(),
                steps.unwrap_or(1),
            );
            return Ok(Started::Done(tilo_actions::ActionOutput::default()));
        }
        if let DeskEffect::SelectSwitcher { at } = &effect {
            crate::desk_windows::select_switcher(&self.handle, &self.surfaces.overlays, *at);
            return Ok(Started::Done(tilo_actions::ActionOutput::default()));
        }
        if matches!(effect, DeskEffect::CommitSwitcher) {
            crate::desk_windows::commit_switcher(&self.handle, &self.surfaces.overlays);
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

    /// A window whose app stopped answering says so in the reading every view of the desk reads,
    /// and only for that window.
    #[test]
    fn a_window_whose_app_stopped_answering_says_so_in_the_reading() {
        struct Blank;

        impl tilo_desktop_surface::TileSource for Blank {
            fn tile(
                &mut self,
                _: &tilo_desktop_model::Workspace,
                tab: &tilo_desktop_model::Tab,
            ) -> ui::widget::TileHost {
                ui::widget::TileHost::new(ui::widget::TileContent::Notice {
                    title: tab.title.clone(),
                    detail: String::new(),
                })
            }
        }

        let mut minter = tilo_desktop_model::IdMinter::new(5);
        let tab = tilo_desktop_model::Tab {
            id: minter.mint(),
            title: "Notes".to_owned(),
            tile: tilo_desktop_model::TileContent::Foreign {
                app_id: "so.tilo.notes".to_owned(),
                surface_id: 77,
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let tree = tilo_desktop_surface::seed_tree(&mut minter, "Desk", vec![tab]);
        let (_surface, handle) =
            tilo_desktop_surface::DesktopSurface::new(tree.clone(), 5, Box::new(Blank));
        assert!(!windows(&tree, &handle)[0].not_responding);
        assert!(handle.set_unresponsive_windows(vec![77]));
        assert!(windows(&tree, &handle)[0].not_responding);
        // Another window's silence is not this one's.
        assert!(handle.set_unresponsive_windows(vec![78]));
        assert!(!windows(&tree, &handle)[0].not_responding);
    }

    fn surfaces(status_panel: bool, shell: bool) -> Surfaces {
        Surfaces {
            status_panel,
            shell,
            panel: tilo_desktop::overlays::Overlays::new(),
            overlays: Rc::new(crate::overlays::Overlays::new()),
            overview: Rc::new(std::cell::Cell::new(false)),
        }
    }

    #[test]
    fn the_overview_turns_the_desks_reading_with_the_surface() {
        let surfaces = surfaces(false, true);
        assert!(!surfaces.overview.get());
        surfaces.toggle(Overlay::Overview.name());
        assert!(surfaces.overview.get(), "the overview is up");
        surfaces.toggle(Overlay::Overview.name());
        assert!(!surfaces.overview.get(), "a second press closes it");
        surfaces.open(Overlay::Overview.name());
        assert!(surfaces.overview.get());
        surfaces.open(Overlay::Centre.name());
        assert!(!surfaces.overview.get(), "another surface replaces it");
        surfaces.open(Overlay::Overview.name());
        surfaces.close(Overlay::Centre.name());
        assert!(surfaces.overview.get(), "closing another surface leaves it");
        surfaces.close(Overlay::Overview.name());
        assert!(!surfaces.overview.get());
    }

    #[test]
    fn a_surface_of_the_shells_own_closes_the_panel_that_asked_for_it() {
        let surfaces = surfaces(true, false);
        surfaces.open(crate::quick_settings::SURFACE);
        assert!(
            surfaces
                .panel
                .is_open(tilo_desktop::overlays::Slot::QuickSettings)
        );
        surfaces.open(Overlay::Centre.name());
        assert_eq!(surfaces.overlays.here(), Overlay::Centre.name());
        assert!(
            !surfaces
                .panel
                .is_open(tilo_desktop::overlays::Slot::QuickSettings),
            "one surface at a time: the centre is not opened behind the panel"
        );
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
