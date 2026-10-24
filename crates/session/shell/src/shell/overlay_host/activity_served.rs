//! The activity model, served to apps ([`crate::shell::activity_provider`]):
//! the reads and writes the broker asked for, run on this thread.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use tilo_action_broker::services::HostStepOutcome;
use tilo_actions::ActionFailure;
use tilo_actions::remote::workspace_activity as wire;

use crate::shell::activity_provider::{refused, unreadable};

/// The arguments a read or write carries, as `T`.
fn read<T: serde::de::DeserializeOwned + Default>(
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<T, String> {
    if arguments.is_empty() {
        return Ok(T::default());
    }
    serde_json::from_value(serde_json::Value::Object(arguments.clone()))
        .map_err(|error| format!("the arguments were not understood: {error}"))
}

fn json(value: &impl serde::Serialize) -> Result<serde_json::Value, Box<ActionFailure>> {
    serde_json::to_value(value).map_err(|error| unreadable(error.to_string()))
}

impl ShellOverlayHost {
    /// Run what the broker asked of the activity model, then say whether
    /// what it serves moved.
    pub(super) fn serve_activity(&mut self) {
        for job in crate::shell::activity_provider::take_jobs() {
            job(self);
        }
        self.publish_activity_revision();
    }

    /// Tell the apps following the activity model to read it again, when
    /// what it serves moved since they last did.
    pub(super) fn publish_activity_revision(&mut self) {
        let revision = self.room_activity.served_revision();
        if self.activity_served != Some(revision) {
            self.activity_served = Some(revision);
            crate::shell::activity_provider::changed();
        }
    }

    /// The active workspace, when it is the one in `room`.
    fn activity_workspace_in(&self, room: &str) -> Option<u64> {
        let chrome = self.chrome.as_ref()?;
        let state = chrome.borrow();
        let tree = state.tree();
        let workspace = tree.desktop.active_workspace()?;
        let shown = (workspace.room().to_string() == room).then_some(workspace.id.raw());
        drop(tree);
        shown
    }

    /// Read `action` of the activity model for `room`; `None` while `room`
    /// is not the workspace shown, whose history the model does not hold.
    pub(crate) fn read_activity(
        &mut self,
        room: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<serde_json::Value>, Box<ActionFailure>> {
        if self.activity_workspace_in(room).is_none() {
            return Ok(None);
        }
        let Some(chrome) = self.chrome.clone() else {
            return Ok(None);
        };
        let pane = &mut self.room_activity;
        let value = match action {
            wire::FEED => {
                let query: wire::FeedQuery = read(arguments).map_err(unreadable)?;
                let state = chrome.borrow();
                let tree = state.tree();
                json(&pane.serve_feed(&query, &*tree))?
            }
            wire::SUMMARY => json(&pane.serve_summary())?,
            wire::REPLAY => json(&pane.serve_replay())?,
            wire::FOCUS => json(&pane.serve_focus())?,
            wire::DETAIL => {
                let query: wire::DetailQuery = read(arguments).map_err(unreadable)?;
                let state = chrome.borrow();
                let records = state.records().clone();
                let mut tree = state.tree_mut();
                json(&pane.serve_detail(&query, &mut *tree, Some(&records)))?
            }
            other => return Err(unreadable(format!("`{other}` is not a read"))),
        };
        // A read can narrow the model (a scope, a filter, a selection).
        self.publish_activity_revision();
        Ok(Some(value))
    }

    /// Run write `action` of the activity model in `room`, as the person
    /// at this device asked.
    pub(crate) fn act_on_activity(
        &mut self,
        room: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> HostStepOutcome {
        if self.activity_workspace_in(room).is_none() {
            return refused(t("host-activity-workspace-not-shown"));
        }
        let Some(chrome) = self.chrome.clone() else {
            return refused(t("host-activity-workspace-not-shown"));
        };
        let outcome = {
            let state = chrome.borrow();
            let records = state.records().clone();
            let mut tree = state.tree_mut();
            self.room_activity
                .serve_action(action, arguments, &mut *tree, Some(&records))
        };
        match outcome {
            Ok(outcome) => {
                Self::apply_room_activity_outcome(&chrome, outcome);
                self.activity_refresh.invalidate();
                self.publish_activity_revision();
                Ok(tilo_actions::ActionOutput::default())
            }
            Err(message) => refused(message),
        }
    }

    /// The person started reading what is new in `room`.
    pub(crate) fn begin_activity_summary(&mut self, room: &str) {
        if let Some(workspace) = self.activity_workspace_in(room) {
            self.room_activity.begin_summary(workspace);
            self.activity_pending_reveal = None;
            self.publish_activity_revision();
        }
    }

    /// The person stopped reading what is new in `room`: what they were
    /// shown is seen, whichever workspace shows now.
    pub(crate) fn end_activity_summary(&mut self, room: &str) {
        let workspace = self.chrome.as_ref().and_then(|chrome| {
            let state = chrome.borrow();
            let tree = state.tree();
            tree.desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room().to_string() == room)
                .map(|workspace| workspace.id.raw())
        });
        if let Some(workspace) = workspace {
            self.room_activity.end_summary(workspace);
            self.activity_refresh.invalidate();
            self.publish_activity_revision();
        }
    }
}

#[cfg(test)]
impl ShellOverlayHost {
    /// Run what the broker asked of the activity model, as the loop does.
    pub(crate) fn serve_activity_for_test(&mut self) {
        self.serve_activity();
    }

    /// Bring the activity model up to date with the room, as a frame does.
    pub(crate) fn refresh_room_activity_for_test(&mut self) {
        self.refresh_room_activity_awareness(true);
        self.publish_activity_revision();
    }
}
