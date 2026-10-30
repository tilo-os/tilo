//! The people in this workspace, found by name.
//!
//! "Go to what Mara is doing" is a search like any other: typing a
//! collaborator's name offers them, with where they are. Choosing one runs
//! the chrome's `show-person` by address, which opens the people panel on
//! them, where following is one step. The people are the workspace's
//! collaborators as the runtime peer keeps them in the desktop's model.

use ui::search::FieldWeight;
use ui::search::system::{ResourceKind, rank_fields};

use crate::model::{DESKTOP_APP, Hit, InvokeTarget, Source, Target};

/// The chrome's resource and its action showing one person.
pub const PEOPLE_RESOURCE: &str = "desktop:chrome";
pub const SHOW_PERSON: &str = "show-person";
/// The parameter naming the person.
pub const PERSON_PARAM: &str = "person";

/// One person in the workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PersonEntry {
    pub id: String,
    pub name: String,
    /// Where they are: the app or document in front of them.
    pub activity: Option<String>,
}

/// The kind people are grouped under, in the desktop's language.
#[must_use]
pub fn people_kind() -> ResourceKind {
    ResourceKind::Custom(crate::i18n::t("search-kind-people"))
}

pub struct People {
    /// The room of the workspace they are in.
    room: String,
    people: Vec<PersonEntry>,
}

impl People {
    #[must_use]
    pub fn new(room: impl Into<String>, people: Vec<PersonEntry>) -> Self {
        Self {
            room: room.into(),
            people,
        }
    }
}

impl Source for People {
    fn id(&self) -> &'static str {
        "people"
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        if needle.trim().is_empty() {
            return Vec::new();
        }
        self.people
            .iter()
            .filter_map(|person| {
                let score = rank_fields(needle, &[(&person.name, FieldWeight::TITLE)])?;
                Some(Hit {
                    kind: people_kind(),
                    provider: self.id().into(),
                    id: person.id.clone(),
                    title: person.name.clone(),
                    subtitle: person.activity.clone(),
                    facts: Vec::new(),
                    score,
                    icon: Some(ui::icons::Icon::User),
                    target: Target::Invoke(Box::new(
                        InvokeTarget::action(DESKTOP_APP, &self.room, PEOPLE_RESOURCE, SHOW_PERSON)
                            .with_argument(PERSON_PARAM, person.id.clone()),
                    )),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_collaborator_is_found_by_name_and_shown_by_the_chromes_address() {
        let people = People::new(
            "room",
            vec![PersonEntry {
                id: "mara".into(),
                name: "Mara Lind".into(),
                activity: Some("Sheets".into()),
            }],
        );
        let hits = people.query("mara");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].subtitle.as_deref(), Some("Sheets"));
        let Target::Invoke(target) = &hits[0].target else {
            panic!("a person is shown by address");
        };
        assert_eq!(target.app, DESKTOP_APP);
        assert_eq!(
            target.address,
            tilo_actions::ActionAddress::new(PEOPLE_RESOURCE, SHOW_PERSON)
        );
        assert_eq!(target.arguments[PERSON_PARAM], "mara");
        assert!(people.query("zed").is_empty());
        assert!(people.query("").is_empty());
    }
}
