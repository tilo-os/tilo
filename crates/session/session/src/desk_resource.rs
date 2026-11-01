//! This device's desk as a resource of the session's: the time in the person's style, who they
//! are, their notifications, the workspaces on this device and the strip in front, and the
//! tasks running for them. A shell's views read it (as `session.desk` resources,
//! `tilo_surface_host::shell_part`), and so does a script or an agent, through the door, by
//! the same address: one reading for every reader.

use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_desktop_model::DesktopTree;
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::navigation::Command;
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};
use tilo_surface_host::shell_part::DESK_RESOURCE;
use tilo_surface_host::{DeskReading, shell_part};

use crate::notifications::NoticeSignals;

/// What the desk's reading is made from ([`Desk::key`]).
pub(crate) type DeskKey = (u64, i64, u32, bool, u64, u64, u64, u64, u64, u64, String);

/// The kind of the desk's resource: the origin kind a shell's type holds.
pub(crate) const DESK_KIND: &str = "session.desk";

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
    /// Show the surface `surface` over the desk's windows, or close it when it is up.
    ShowSurface {
        #[param(
            "The surface: `none`, `overview`, `switcher`, `launcher`, `centre`, `workspaces` or `composer`"
        )]
        surface: String,
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
    /// Open the window switcher, or walk it `steps` windows on: Alt+Tab's press.
    WalkSwitcher {
        #[param("How far to walk; 1 the next window, -1 the one before")]
        steps: Option<i64>,
    },
    /// Go to the window the switcher's walk reached, and close the switcher.
    CommitSwitcher,
}

/// The desk as the session reads it. See the module docs.
pub(crate) struct Desk {
    pub(crate) handle: DesktopHandle,
    pub(crate) person: String,
    pub(crate) signals: Rc<NoticeSignals>,
    /// Whether the device has quick settings; without, its status is its notifications.
    pub(crate) status_panel: bool,
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
    /// The surface the person is in, as the chrome keeps it.
    pub(crate) overlays: Rc<crate::overlays::Overlays>,
}

/// What a window with no title is called.
fn untitled() -> String {
    tilo_desktop_surface::i18n::t("window-switcher-untitled")
}

/// The windows the desk shows, by recency: the tabs of every strip of the workspace in front,
/// the focused one first, then those it was in front of, then the rest along their strips.
fn windows(tree: &DesktopTree) -> Vec<shell_part::WindowReading> {
    use tilo_desktop_model::TileContent;
    let Some(workspace) = tree.active_workspace() else {
        return Vec::new();
    };
    let active_strip = tree
        .active_strip
        .get(&workspace.id)
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
                readings.push(shell_part::WindowReading {
                    key: tab.id.to_string(),
                    title: if title.is_empty() {
                        untitled()
                    } else {
                        title.to_owned()
                    },
                    origin_app: tab.app_title.clone().unwrap_or(app),
                    icon: String::new(),
                    strip: strip.name.trim().to_owned(),
                    workspace: workspace.name.clone(),
                    recency: 0,
                    focused,
                    preview: "none".to_owned(),
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
    readings
}

impl Desk {
    /// What the reading is made from, without making it: the tree's revision, the minute,
    /// what the notifications say and who is here. The reading changes only when this does.
    pub(crate) fn key(&self) -> DeskKey {
        (
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
            self.overlays.revision(),
            self.overlays.here(),
        )
    }

    /// The desk's windows, with the one the switcher's walk has reached marked focused while
    /// it is up: the tile its walk chose is the one lifted.
    fn windows_reaching(&self, tree: &DesktopTree) -> Vec<shell_part::WindowReading> {
        let mut windows = windows(tree);
        if self.overlays.here() == "switcher"
            && let Some(at) = self.overlays.reached(
                windows.len(),
                windows.iter().position(|window| window.focused),
            )
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
            status_panel: self.status_panel,
            workspace: tree
                .active_workspace()
                .map(|workspace| workspace.name.clone())
                .unwrap_or_default(),
            strip: strip_in_front(tree),
            workspaces: tree
                .workspaces
                .iter()
                .map(|workspace| shell_part::WorkspaceReading {
                    key: workspace.id.0.to_string(),
                    name: workspace.name.clone(),
                    current: workspace.id == tree.active,
                })
                .collect(),
            // The native session runs no agent tasks of its own yet: none is published until a
            // task host reports to it.
            tasks: Vec::new(),
            title: title_of(tree),
            here: self.overlays.here(),
            spotlight: self.overlays.spotlight(),
            banner: String::new(),
            windows: self.windows_reaching(tree),
            agents: Vec::new(),
            rooms: tree
                .workspaces
                .iter()
                .map(|workspace| shell_part::RoomReading {
                    key: workspace.id.0.to_string(),
                    name: workspace.name.clone(),
                    current: workspace.id == tree.active,
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

/// What the bar names the desk: the workspace in front, and the strip after it.
fn title_of(tree: &DesktopTree) -> String {
    let workspace = tree
        .active_workspace()
        .map(|workspace| workspace.name.clone())
        .unwrap_or_default();
    let strip = strip_in_front(tree);
    if strip.is_empty() {
        workspace
    } else if workspace.is_empty() {
        strip
    } else {
        format!("{workspace} — {strip}")
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
            read: false,
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
    /// walk counts from the window in front as the desk's windows are listed, wrapping.
    fn commit_switcher(&self) {
        let windows = self.handle.read(windows);
        // The window in front leads the list, so the walk counts from its place.
        let front = windows.iter().position(|window| window.focused);
        let at = if windows.is_empty() {
            None
        } else {
            Some(
                self.overlays
                    .walk()
                    .rem_euclid(i64::try_from(windows.len()).unwrap_or(1)),
            )
            .and_then(|walk| {
                let front = i64::try_from(front.unwrap_or(0)).unwrap_or(0);
                usize::try_from(
                    front
                        .saturating_add(walk)
                        .rem_euclid(i64::try_from(windows.len()).unwrap_or(1)),
                )
                .ok()
            })
        };
        let Some(window) = windows.get(at.unwrap_or_default()) else {
            self.overlays.forget_walk();
            self.overlays.show(crate::overlays::Overlay::Rest);
            return;
        };
        let tab = window.key.parse::<u64>().unwrap_or_default();
        self.handle.focus_tab(tab);
        self.overlays.forget_walk();
        self.overlays.show(crate::overlays::Overlay::Rest);
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

impl Service for Desk {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![offer::<DeskEffect>(ResourceSpec::new(
            DESK_RESOURCE,
            DESK_KIND,
            self.person.clone(),
        ))]
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
        if let DeskEffect::ShowSurface { surface } = &effect {
            self.overlays.toggle(surface);
            return Ok(Started::Done(tilo_actions::ActionOutput::default()));
        }
        if let DeskEffect::CloseSurface { surface } = &effect {
            self.overlays.close(surface);
            return Ok(Started::Done(tilo_actions::ActionOutput::default()));
        }
        if matches!(effect, DeskEffect::ClearNotices) {
            self.notices.clear_history();
            return Ok(Started::Done(tilo_actions::ActionOutput::default()));
        }
        if let DeskEffect::WalkSwitcher { steps } = &effect {
            let opened = self.overlays.here() == "switcher";
            self.overlays.show(crate::overlays::Overlay::Switcher);
            if opened {
                self.overlays.step(steps.unwrap_or(1));
            }
            return Ok(Started::Done(tilo_actions::ActionOutput::default()));
        }
        if matches!(effect, DeskEffect::CommitSwitcher) {
            self.commit_switcher();
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
                .any(|candidate| candidate.id.0.to_string() == workspace)
        });
        if !known {
            return Err(DoorError::InvalidArguments(format!(
                "there is no workspace {workspace} on this device"
            )));
        }
        self.handle
            .dispatch(
                &Command::Workspace {
                    generation: crate::GENERATION.into(),
                    workspace,
                },
                crate::GENERATION,
            )
            .map_err(DoorError::Failed)?;
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }
}

/// The desk's action and its argument, by the names the shell's origin invokes them by: one
/// spelling of each, checked against the derived vocabulary.
#[cfg(test)]
mod tests {
    use super::*;
    use tilo_surface_host::shell_part::{SWITCH_WORKSPACE, WORKSPACE_ARG};

    #[test]
    fn the_shell_invokes_the_desk_by_the_names_it_offers() {
        let specs = DeskEffect::specs();
        let switch = specs
            .iter()
            .find(|spec| spec.id == SWITCH_WORKSPACE)
            .expect("the desk offers switch-workspace");
        assert!(
            switch
                .params()
                .iter()
                .any(|param| param.name == WORKSPACE_ARG),
            "switch-workspace takes {WORKSPACE_ARG}"
        );
    }
}
