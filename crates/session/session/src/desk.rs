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
}

/// The desk as the session reads it. See the module docs.
pub(crate) struct Desk {
    pub(crate) handle: DesktopHandle,
    pub(crate) person: String,
    pub(crate) signals: Rc<NoticeSignals>,
    /// Whether the device has quick settings; without, its status is its notifications.
    pub(crate) status_panel: bool,
}

impl Desk {
    /// What the reading is made from, without making it: the tree's revision, the minute, and
    /// what the notifications say. The reading changes only when this does.
    pub(crate) fn key(&self) -> (u64, i64, u32, bool) {
        (
            self.handle.revision(),
            ui::clock::local_now().timestamp().div_euclid(60),
            self.signals.unread(),
            self.signals.quiet(),
        )
    }

    /// The desk now.
    pub(crate) fn reading(&self) -> DeskReading {
        let now = ui::clock::local_now();
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
            // The native session runs no agent tasks of its own yet: none is published until a
            // task host reports to it.
            tasks: Vec::new(),
            badges: self.signals.badges(),
        })
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
        let DeskEffect::SwitchWorkspace { workspace } =
            decode(&self.resources(), resource, action, arguments)?;
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
