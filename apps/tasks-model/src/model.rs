//! The `tasks.tracker` record: a workspace's issues, and the projects and
//! areas above them.
//!
//! An issue has a number, a title, a markdown body, a status, an assignee,
//! labels, a parent (nested issues are sub-issues), links to other resources
//! and a thread. An agent works on one by the tracker's own actions, so every
//! edit it makes is a permission in the one action system and an entry in the
//! issue's history.
//!
//! A project or an area (`tracker_area`) is an ordinary resource of the
//! tracker: it has a name, a kind and the people involved. The tasks it
//! holds are the issues that name it as what they work towards, and the
//! documents it holds are the links those issues carry, so one task reads in
//! its project and in whoever's list alike.
//!
//! The app draws the record, and an agent reads and writes it through the
//! verbs below, so both use one definition of every type, verb and typed
//! action argument.

mod effect;
mod goal;
mod issue;
mod record;
pub mod search;
mod tracker_area;

pub use effect::{NOTE_KIND, NOTES, TasksEffect};
pub use goal::Area;
pub use issue::{
    Imported, ImportedMessage, Issue, Link, Message, MessageKind, Status, agent_assignee,
    person_assignee,
};
pub use record::{Tracker, tracker};
pub use tracker_area::{Kind, Person, TrackerArea};

/// The record kind issues live in.
pub const KIND: &str = "tasks.tracker";

/// The app that keeps the record.
pub const APP_ID: &str = "so.tilo.tasks";

/// The resource address of `issue` in the tracker, the one its reference
/// chip, its search hit and its `open` name.
#[must_use]
pub fn issue_resource(issue: &tilo_resource::Id) -> String {
    tilo_resource::items::item_resource_id_in(KIND, "issues", issue)
}

/// The resource address of the project or area `area`, as a task names it in
/// its [`Area`] and as its search hit opens it.
#[must_use]
pub fn area_resource(area: &tilo_resource::Id) -> String {
    tilo_resource::items::item_resource_id_in(KIND, "areas", area)
}

/// What an issue plans as its goal when it works towards the project or area
/// `area` named `name`: this tracker's own resource, so that whoever reads
/// the issue can count it among the project's tasks.
#[must_use]
pub fn area_goal(area: &tilo_resource::Id, name: &str) -> Area {
    Area {
        resource: area.as_str().to_owned(),
        app: APP_ID.to_owned(),
        kind: KIND.to_owned(),
        label: name.to_owned(),
    }
}

#[cfg(test)]
mod tests;
