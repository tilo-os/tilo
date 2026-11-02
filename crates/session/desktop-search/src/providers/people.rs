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
    /// The workspace they are in.
    workspace: String,
    people: Vec<PersonEntry>,
}

impl People {
    #[must_use]
    pub fn new(workspace: impl Into<String>, people: Vec<PersonEntry>) -> Self {
        Self {
            workspace: workspace.into(),
            people,
        }
    }
}

impl Source for People {
    fn id(&self) -> &'static str {
        "people"
    }

    fn browse_rank(&self) -> u8 {
        crate::model::BROWSE_RANK_PEOPLE
    }

    fn name(&self) -> String {
        crate::i18n::t("search-kind-people")
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        // With nothing typed the people who are doing something are listed:
        // the empty palette is about the workspace now, and where each
        // person is working, not a roster of everyone who is about.
        if needle.trim().is_empty() {
            let busy = self
                .people
                .iter()
                .filter(|person| person.activity.is_some());
            let mut top = ui::search::system::MAX_PROVIDER_SCORE;
            return busy
                .map(|person| {
                    let hit = self.hit(person, top);
                    top = top.saturating_sub(1);
                    hit
                })
                .collect();
        }
        self.people
            .iter()
            .filter_map(|person| {
                let score = rank_fields(needle, &[(&person.name, FieldWeight::TITLE)])?;
                Some(self.hit(person, score))
            })
            .collect()
    }
}

impl People {
    /// `person` as a row scoring `score`: shown by the chrome's `show-person`
    /// at their id.
    fn hit(&self, person: &PersonEntry, score: i32) -> Hit {
        Hit {
            row: None,
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind: people_kind(),
            provider: <Self as Source>::id(self).into(),
            id: person.id.clone(),
            title: person.name.clone(),
            subtitle: person.activity.clone(),
            facts: Vec::new(),
            score,
            icon: Some(ui::icons::Icon::User),
            target: Target::Invoke(Box::new(
                InvokeTarget::action(DESKTOP_APP, &self.workspace, PEOPLE_RESOURCE, SHOW_PERSON)
                    .with_argument(PERSON_PARAM, person.id.clone()),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_collaborator_is_found_by_name_and_shown_by_the_chromes_address() {
        let people = People::new(
            "workspace",
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
        // Nothing typed lists who is working, with where they are: the empty
        // palette is about the workspace now, not a roster of everyone.
        let browsing = people.query("");
        assert_eq!(browsing.len(), 1);
        assert_eq!(browsing[0].title, "Mara Lind");
        assert_eq!(browsing[0].subtitle.as_deref(), Some("Sheets"));
        let idle = People::new(
            "workspace",
            vec![PersonEntry {
                id: "zed".into(),
                name: "Zed".into(),
                activity: None,
            }],
        );
        assert!(idle.query("").is_empty(), "someone idle is not listed");
    }
}
