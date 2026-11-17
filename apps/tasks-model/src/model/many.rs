//! What `create-many` takes: a list of new issues, each as `create` takes it,
//! where one may stand under another of the same list.
//!
//! A piece of work is filed as an issue with its sub-issues. A sub-issue
//! names its parent, and an issue that is not made yet has no name to give:
//! so within one list an issue is named by a `key` its caller chooses, or by
//! its place, and the list is made in one step. Changes to issues that are
//! there already need no list form of their own: the tracker's `group` takes
//! any of its actions several at a time.

use serde::{Deserialize, Serialize};
use tilo_resource::items::Ref;
use tilo_resource::{JsonSchema, NonBlank, Origin};

use super::{Area, NewIssue, Priority, Status};

/// The most issues one call makes: what keeps a list of new issues within
/// one part, and so one step of its history.
pub const MAX_MANY: usize = 200;

/// A new issue among several made in one call: what `create` takes, with how
/// it is named and nested within the call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListedIssue {
    /// A name for this issue within the call, which the sub-issues after it
    /// name in `under`: any short text that is not a number
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The issue of this call it is a sub-issue of: that one's `key`, or its
    /// place in the list counted from 1. That issue stands earlier in the
    /// list
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub under: Option<String>,
    /// What the issue is about, in a line
    pub title: NonBlank,
    /// An issue the tracker already has that it is a sub-issue of, by its
    /// name or id
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<Ref>,
    /// Its description, in markdown
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Where it starts; Backlog without
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    /// A person's root key, agent:<id>, or outside:<name> for someone not in
    /// the workspace yet
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// The page it is read at elsewhere, when it was written from one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The project or area it works towards, by its name or id
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<Ref>,
    /// How urgent it is: none, urgent, high, medium or low
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// How much work it is, in points
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<u32>,
    /// When it is wanted, in milliseconds since the epoch
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_ms: Option<i64>,
    /// Labels it carries
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    /// Where it was first written, when it is moved here from elsewhere:
    /// shown as its author and date, while its history keeps who moved it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// Its place in the tracker's order: filled in where it is made, from
    /// the issues held there and the issues before it in the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<String>,
    /// The project or area `project` names, as the issue keeps it: filled
    /// in where it is made, from the tracker's projects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<Area>,
}

impl ListedIssue {
    /// A new issue titled `title`, every other argument left out.
    #[must_use]
    pub fn titled(title: NonBlank) -> Self {
        Self::of(NewIssue::titled(title))
    }

    /// `issue` as an entry of a list, neither named nor nested in it.
    #[must_use]
    pub fn of(issue: NewIssue) -> Self {
        let NewIssue {
            title,
            parent,
            body,
            status,
            assignee,
            url,
            project,
            priority,
            estimate,
            due_ms,
            labels,
            origin,
            rank,
            goal,
        } = issue;
        Self {
            key: None,
            under: None,
            title,
            parent,
            body,
            status,
            assignee,
            url,
            project,
            priority,
            estimate,
            due_ms,
            labels,
            origin,
            rank,
            goal,
        }
    }

    /// The issue as `create` takes it: everything but its name and its
    /// nesting within the call.
    #[must_use]
    pub fn into_new(self) -> NewIssue {
        NewIssue {
            title: self.title,
            parent: self.parent,
            body: self.body,
            status: self.status,
            assignee: self.assignee,
            url: self.url,
            project: self.project,
            priority: self.priority,
            estimate: self.estimate,
            due_ms: self.due_ms,
            labels: self.labels,
            origin: self.origin,
            rank: self.rank,
            goal: self.goal,
        }
    }
}
