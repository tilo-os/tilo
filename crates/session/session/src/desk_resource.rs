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
pub(crate) type DeskKey = (u64, i64, u32, bool, u64, u64, u64, u64);

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
}

/// The desk as the session reads it. See the module docs.
pub(crate) struct Desk {
    pub(crate) handle: DesktopHandle,
    pub(crate) person: String,
    pub(crate) signals: Rc<NoticeSignals>,
    /// The apps' live activities: Clocks' timer is the bar's clock beside the time, and the
    /// person putting the floating one away hides it until Clocks shows something new.
    pub(crate) clock: Rc<crate::chrome::Clock>,
    /// Whether the device has quick settings; without, its status is its notifications.
    pub(crate) status_panel: bool,
    /// Who is here, when this session takes part in the collaboration plane.
    pub(crate) sync: Option<Rc<crate::sync::Client>>,
    /// The running applications' tray items, when their watcher serves: the bar shows them,
    /// and the person's clicks go back through it.
    pub(crate) tray: Option<std::sync::Arc<tilo_tray_host::Tray>>,
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
        )
    }

    /// The timer or stopwatch Clocks runs, as the bar's clock reads it: what Clocks puts on
    /// its live activity, and nothing when it shows none or marks it inactive.
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

    /// The desk now.
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
            // The native session runs no agent tasks of its own yet: none is published until a
            // task host reports to it.
            tasks: Vec::new(),
            badges: self.signals.badges(),
            people,
            tray: self.tray_items(),
        })
    }
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
        match effect {
            DeskEffect::AnswerAsk => {
                self.handle.answer_personal_ask();
                Ok(Started::Done(tilo_actions::ActionOutput::default()))
            }
            DeskEffect::PutOffAsk => {
                self.handle.put_off_personal_ask();
                Ok(Started::Done(tilo_actions::ActionOutput::default()))
            }
            DeskEffect::SwitchWorkspace { workspace } => self.switch_workspace(&workspace),
            DeskEffect::OpenTrayItem { key } => self.open_tray_item(&key),
        }
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

    /// Bring the workspace `workspace` of this device to the front.
    fn switch_workspace(&self, workspace: &str) -> Result<Started, DoorError> {
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
                    workspace: workspace.to_owned(),
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
