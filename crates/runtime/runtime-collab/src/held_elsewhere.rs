//! Records a window shows that another workspace holds, and what that workspace says of the
//! changes the person at the window may make to them.
//!
//! A person's profile record is held in their own workspace, whatever workspace the window
//! showing it is open in, and that workspace decides its changes. The app never holds another
//! workspace's policy: it names each action of such a record to the host, and the host, which
//! holds that workspace's grants, answers a [`HeldDecision`] for each, and answers again whenever
//! those grants change. The decisions only say how the window shows its controls; running an
//! action is admitted by the host against the holding workspace as it is then.

use serde::{Deserialize, Serialize};

/// One action of a record held in a workspace other than the window's: what the host decides.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HeldRecord {
    /// The workspace holding the record, which decides its changes.
    pub workspace: String,
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

impl HeldRecord {
    /// The action `action` of the record `resource` (of kind `kind`) that the app `app`
    /// publishes, held in `workspace`, which `need` describes.
    #[must_use]
    pub fn new(
        workspace: &str,
        app: &str,
        (kind, resource, action): (&str, &str, &str),
        need: &tilo_actions::ActionNeed,
    ) -> Self {
        Self {
            workspace: workspace.to_owned(),
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

/// What the holding workspace says of one [`HeldRecord`] now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeldVerdict {
    /// The person may run it.
    Allowed,
    /// The person may run it once they confirm it.
    AsksFirst,
    /// They may not, and why, as the person reads it: refused by the workspace's grants, or the
    /// workspace is not held where the host could ask it.
    Refused(String),
}

/// A [`HeldRecord`] and what its workspace says of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldDecision {
    pub record: HeldRecord,
    pub verdict: HeldVerdict,
}
