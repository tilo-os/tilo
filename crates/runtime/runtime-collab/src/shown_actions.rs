//! The actions a window shows, and what the device's daemon says of each for the person at the
//! window.
//!
//! A window decides no permission itself. It names the actions of the records it shows to its
//! host; the host decides each with the ordinary check in the workspace holding the record (the
//! window's own, or another: a person's profile record is held in their own workspace wherever it
//! is shown), and answers again whenever those grants change. The verdicts only say how the window
//! shows its controls; running an action is admitted by the host when it is called.

use serde::{Deserialize, Serialize};

/// One action of a record a window shows: what the host decides.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ShownAction {
    /// The workspace holding the record when it is not the window's.
    pub workspace: Option<String>,
    /// The app publishing the record.
    pub app: String,
    /// The record's resource kind.
    pub kind: String,
    /// The record as the app publishes it.
    pub resource: String,
    /// The action.
    pub action: String,
    /// What running the action needs ([`tilo_actions::ActionNeed`], whose optional fields a
    /// compact encoding cannot skip): its kind,
    pub kind_needed: tilo_actions::ActionKind,
    /// the field of the record it changes, when it changes one,
    pub field: Option<String>,
    /// and whether it exercises authority over others.
    pub privileged: bool,
}

impl ShownAction {
    /// The action `action` of the record `resource` (of kind `kind`) that the app `app`
    /// publishes, held in `workspace` when that is not the window's, which `need` describes.
    #[must_use]
    pub fn new(
        workspace: Option<&str>,
        app: &str,
        (kind, resource, action): (&str, &str, &str),
        need: &tilo_actions::ActionNeed,
    ) -> Self {
        Self {
            workspace: workspace.map(str::to_owned),
            app: app.to_owned(),
            kind: kind.to_owned(),
            resource: resource.to_owned(),
            action: action.to_owned(),
            kind_needed: need.kind,
            field: need.field.clone(),
            privileged: need.privileged,
        }
    }

    /// What running the action needs.
    #[must_use]
    pub fn need(&self) -> tilo_actions::ActionNeed {
        tilo_actions::ActionNeed {
            kind: self.kind_needed,
            field: self.field.clone(),
            privileged: self.privileged,
        }
    }
}

/// What the host says of one [`ShownAction`] now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionVerdict {
    /// The person may run it.
    Allowed,
    /// The person may run it once they confirm it.
    AsksFirst,
    /// They may not, and why, as the person reads it: refused by the workspace's grants, or the
    /// workspace is not held where the host could ask it.
    Refused(String),
}

/// A [`ShownAction`] and what the host says of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionDecision {
    pub action: ShownAction,
    pub verdict: ActionVerdict,
}
