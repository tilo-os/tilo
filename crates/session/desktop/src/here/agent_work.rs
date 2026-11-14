//! Where this person's own agents work, for the desk to show it: the person sees what their agent
//! does. Each time one of their agents moves to another record, or acts again where it is, the
//! window it works in is brought into view. The page does that without taking the keyboard
//! (the desk's `reveal_tab`); someone else's agent is only pointed at.

use std::collections::HashMap;

use tilo_desktop_model::TabId;

use super::{HereState, Person};

/// Where an agent works, as this desk can show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkPlace {
    /// The window showing the record it works in.
    Window(TabId),
    /// The app it works in, where no window here shows its record by the name it is noted under.
    App(String),
}

/// What this page last showed of each of its person's agents, and whether one acted since.
#[derive(Debug, Default)]
pub(super) struct AgentWork {
    /// Where each agent worked when it was last shown, by publisher.
    shown: HashMap<String, WorkPlace>,
    /// An agent acting for this person narrated an action since the last look.
    acted: bool,
}

impl HereState {
    /// An agent acting for `for_person` (a person's root key, either spelling) narrated an action:
    /// where it works is shown again on the next look, even when it did not move.
    pub fn note_agent_acted(&mut self, for_person: &str) {
        let own = self
            .own_user()
            .is_some_and(|own| same_person(own, for_person));
        if own {
            self.agent_work.acted = true;
        }
    }

    /// Whether an agent of this person acted since the last look ([`Self::note_agent_acted`]).
    #[must_use]
    pub fn own_agent_acted(&self) -> bool {
        self.agent_work.acted
    }

    /// Where this person's own agents work now, among `people`, when that is news: an agent seen
    /// working somewhere it was not shown at before, or one that acted again since the last look.
    /// The newest such place, for the desk to bring into view; `None` when nothing moved. Someone
    /// else's agent is theirs to watch.
    pub fn take_own_agent_work(&mut self, people: &[Person]) -> Option<WorkPlace> {
        let acted = std::mem::take(&mut self.agent_work.acted);
        let own = self.own_user()?.to_owned();
        let mut news = None;
        for person in people.iter().filter(|person| person.agent) {
            let theirs = person
                .user
                .as_deref()
                .is_some_and(|user| same_person(user, &own));
            if !theirs {
                continue;
            }
            let Some(place) = person
                .tab
                .map(WorkPlace::Window)
                .or_else(|| person.focus_app.clone().map(WorkPlace::App))
            else {
                continue;
            };
            let moved = self.agent_work.shown.get(&person.id) != Some(&place);
            if moved || acted {
                self.agent_work
                    .shown
                    .insert(person.id.clone(), place.clone());
                news = Some(place);
            }
        }
        let here: Vec<&str> = people.iter().map(|person| person.id.as_str()).collect();
        self.agent_work
            .shown
            .retain(|publisher, _| here.contains(&publisher.as_str()));
        news
    }
}

/// Whether two root keys name the same person, in either spelling and either case of hex.
fn same_person(a: &str, b: &str) -> bool {
    tilo_action_policy::person_root(a).eq_ignore_ascii_case(tilo_action_policy::person_root(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::here::LiveMember;

    const ADA: &str = "ad00";

    /// A page of Ada's.
    fn adas_page() -> HereState {
        let mut here = HereState::default();
        let own = LiveMember {
            publisher: "ada-laptop".into(),
            kind: "person".into(),
            user: Some(ADA.into()),
            ..LiveMember::default()
        };
        here.set_members(Some(own), Some(Vec::new()));
        here
    }

    /// An agent here, of `user`, working in the window `tab` or the app `app`.
    fn agent(id: &str, user: &str, tab: Option<TabId>, app: Option<&str>) -> Person {
        Person {
            id: id.into(),
            user: Some(user.into()),
            member: None,
            label: "Claude Code".into(),
            devices: Vec::new(),
            rgb: (0, 0, 0),
            agent: true,
            tab,
            focus_app: app.map(str::to_owned),
            place: None,
            status: None,
            availability: ui::Availability::default(),
            call: None,
        }
    }

    /// Ada's agent moving from one window to another is news each time, and staying put is not;
    /// acting again where it is shows that window again.
    #[test]
    fn where_the_persons_own_agent_works_is_news_each_time_it_moves_or_acts() {
        let mut here = adas_page();
        let at = |tab| [agent("claude", ADA, Some(tab), Some("so.tilo.notes"))];
        assert_eq!(here.take_own_agent_work(&at(1)), Some(WorkPlace::Window(1)));
        assert_eq!(here.take_own_agent_work(&at(1)), None, "it did not move");
        assert_eq!(here.take_own_agent_work(&at(2)), Some(WorkPlace::Window(2)));
        here.note_agent_acted(&format!("person:{}", ADA.to_uppercase()));
        assert!(here.own_agent_acted());
        assert_eq!(
            here.take_own_agent_work(&at(2)),
            Some(WorkPlace::Window(2)),
            "an action where it is shows it again"
        );
        assert!(!here.own_agent_acted());
        // Its record has no window here yet: its app is where it works.
        let in_tasks = [agent("claude", ADA, None, Some("so.tilo.tasks"))];
        assert_eq!(
            here.take_own_agent_work(&in_tasks),
            Some(WorkPlace::App("so.tilo.tasks".into()))
        );
    }

    /// Someone else's agent is theirs to watch: it never moves Ada's view.
    #[test]
    fn someone_elses_agent_is_not_news() {
        let mut here = adas_page();
        let bens = [agent("bens-claude", "be00", Some(3), None)];
        assert_eq!(here.take_own_agent_work(&bens), None);
        here.note_agent_acted("be00");
        assert!(!here.own_agent_acted());
    }
}
