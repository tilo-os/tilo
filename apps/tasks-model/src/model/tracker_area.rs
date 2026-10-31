//! A tracker: the issues of a workspace, the projects and areas above them,
//! and the people involved in each.
//!
//! A project or an area is an ordinary resource of the tracker, not a
//! project-management object of its own: it names itself, says which kind it
//! is, and can be linked and shared as anything else. What it holds stays
//! where it already lives — a task is an issue that works towards it
//! ([`Issue::area`](super::Issue)), a document is a link on that issue — and
//! the one thing the tracker keeps on the project's behalf is who is
//! involved: the people a task here can be given to without typing them in
//! again. Each of them carries the role they have here, which is not who may
//! read the project: sharing and permissions remain the resource's, granted
//! by the framework to collaborators, not kept in a field.

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Collection, Id, JsonSchema, Model, Refusal};

/// The longest name a project keeps.
pub const MAX_NAME_CHARS: usize = 120;

/// Which kind of project this is.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// A finite goal: the work ends, and so does the project.
    #[default]
    Project,
    /// An ongoing responsibility that has no end, only what is done next.
    Area,
}

impl Kind {
    /// Both kinds, in the order the window offers them.
    pub const ALL: [Self; 2] = [Self::Project, Self::Area];

    /// The Fluent id naming the kind.
    #[must_use]
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Project => "tasks-kind-project",
            Self::Area => "tasks-kind-area",
        }
    }
}

/// A project or an area: an ordinary resource above the tasks written for it.
///
/// It holds no task of its own: an issue works towards it through its own
/// [`area`](super::Issue::area), so the same task reads in the project and in
/// the list of whoever it is assigned to.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct TrackerArea {
    /// Its id
    #[model(readonly)]
    pub id: Id,
    /// Name
    #[model(label)]
    pub name: String,
    /// Kind
    ///
    /// Whether it is a project with an end or an area that goes on.
    #[model(readonly)]
    pub kind: Kind,
    /// People
    ///
    /// Who is involved here, each with the role they have: read on the
    /// project's page as its people. Being named here gives nobody access;
    /// it is who the tasks can be handed to, and to whom the work reads as
    /// theirs.
    pub people: Collection<Person>,
}

impl TrackerArea {
    /// Whether it may be stored as it is: it has a name that fits on a line.
    ///
    /// # Errors
    /// Refuses a blank or too long name.
    pub fn valid(&self) -> Result<(), Refusal> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(Refusal::new("A project has a name"));
        }
        if name.chars().count() > MAX_NAME_CHARS {
            return Err(Refusal::new("A project's name fits on a line"));
        }
        Ok(())
    }
}

/// One person involved in a project, and the role they have there.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Person {
    /// Their name, as the workspace's people read it.
    #[model(label, readonly)]
    pub name: String,
    /// Role
    ///
    /// What they do here, in their own words.
    #[model(readonly)]
    pub role: String,
}
