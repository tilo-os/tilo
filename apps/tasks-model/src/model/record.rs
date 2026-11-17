//! The tracker: the projects and areas above a workspace's issues, which
//! live in the parts beside it ([`crate::model::IssuePart`]).

use tilo_resource::{self, ActionCx, Collection, Id, Model, NonBlank, Refusal, resource};

use crate::model::tracker_area::{Kind, Person, TrackerArea};

/// The projects and areas above a workspace's issues, and who is involved
/// in each. The issues themselves are kept in parts beside it
/// ([`crate::model::IssuePart`]), so a change to one costs that part.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Tracker {
    /// Projects
    ///
    /// The projects and areas above the issues, each an ordinary resource
    /// of its own. Nothing else is kept here on their behalf: what a
    /// project holds is the issues that work towards it and the links those
    /// carry.
    #[model(unit)]
    pub areas: Collection<TrackerArea>,
}

impl Tracker {
    /// The refusal of a project the tracker does not have.
    #[must_use]
    pub fn no_area() -> Refusal {
        Refusal::new("This project no longer exists")
    }

    fn area_mut(&mut self, area: &Id) -> Result<&mut TrackerArea, Refusal> {
        self.areas.get_mut(area).ok_or_else(Self::no_area)
    }

    /// The project or area named `area`, when it is one of the tracker's.
    #[must_use]
    pub fn area(&self, area: &Id) -> Option<&TrackerArea> {
        self.areas.get(area)
    }

    /// What the record holds: the projects and areas.
    ///
    /// It answers through [`fields`](tilo_resource::read::fields): a caller
    /// with no window open reads the record as it reads any app's record.
    ///
    /// # Errors
    /// Refuses an item the record does not hold.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "tasks.tracker",
    params = tracker,
    effects = crate::model::TasksEffect,
    read_content = Tracker::read_document
)]
impl Tracker {
    /// Create a project or an area: the resource above the issues written
    /// for it. It starts with nobody involved and holds nothing of its own
    /// yet; tasks join it by working towards it.
    ///
    /// # Errors
    /// Refuses a name too long, or an id already taken.
    #[action(step = "create", writes = areas)]
    pub fn create_area(
        &mut self,
        cx: &mut ActionCx,
        #[param("Its name")] name: NonBlank,
        #[param("Whether it is a project with an end or an area that goes on")] kind: Option<Kind>,
    ) -> Result<(), Refusal> {
        let id = cx.id();
        if self.areas.contains(&id) {
            return Err(Refusal::new("That project exists already"));
        }
        let area = TrackerArea {
            id: id.clone(),
            name: name.into_string(),
            kind: kind.unwrap_or_default(),
            ..TrackerArea::default()
        };
        TrackerArea::valid(&area)?;
        self.areas.push(id, area);
        Ok(())
    }

    /// Rename the project or area `area`.
    #[action(item = areas, field = "name", step = "edit", writes = areas)]
    pub fn rename_area(&mut self, area: &Id, name: NonBlank) -> Result<(), Refusal> {
        let target = self.area_mut(area)?;
        if name.chars().count() > crate::model::tracker_area::MAX_NAME_CHARS {
            return Err(Refusal::new("A project's name fits on a line"));
        }
        target.name = name.into_string();
        Ok(())
    }

    /// Say whether the project or area `area` reaches an end or goes on: a
    /// project with a goal, or an area that is a standing responsibility.
    #[action(item = areas, step = "edit", writes = areas)]
    pub fn set_kind(
        &mut self,
        area: &Id,
        #[param("Project or area")] kind: Kind,
    ) -> Result<(), Refusal> {
        self.area_mut(area)?.kind = kind;
        Ok(())
    }

    /// Say who is involved in the project or area `area`, and what they do
    /// there. Naming someone grants them nothing: who may read the project
    /// is the sharing it was given, never a list of people.
    #[action(item = areas, step = "create", writes = areas)]
    pub fn involve(
        &mut self,
        cx: &mut ActionCx,
        area: &Id,
        #[param("The person's name, as the workspace reads it")] name: NonBlank,
        #[param("What they do here, in their own words")] role: Option<String>,
    ) -> Result<(), Refusal> {
        let id = cx.id();
        let target = self.area_mut(area)?;
        target.people.push(
            id,
            Person {
                name: name.into_string(),
                role: role.unwrap_or_default().trim().to_owned(),
            },
        );
        Ok(())
    }

    /// Take someone out of the list of people involved in `area`. They keep
    /// their access to it: that is the project's sharing, not this list.
    #[action(item = areas, step = "delete", writes = areas)]
    pub fn remove_person(&mut self, area: &Id, person: &Id) -> Result<(), Refusal> {
        self.area_mut(area)?.people.remove(person);
        Ok(())
    }

    /// Delete a project or an area. Its issues stay, and read as working
    /// towards no project.
    #[action(item = areas, step = "delete", destructive, writes = areas)]
    pub fn delete_area(&mut self, area: &Id) -> Result<(), Refusal> {
        self.areas.remove(area).ok_or_else(Self::no_area)?;
        Ok(())
    }
}
