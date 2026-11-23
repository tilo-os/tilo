//! Where this person's own agents work, for the shell to show it: the person sees what their agent
//! does. Each time one of their agents moves to another record, or acts again where it is, the
//! window it works in is brought into view. The page does that without taking the keyboard
//! (the shell's `reveal_tab`); someone else's agent is only pointed at.

use std::collections::HashMap;

use tilo_actions::SemanticInteraction;
use tilo_desktop_model::TabId;

use super::{HereState, Person};

/// Where an agent works, as this session can show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkPlace {
    /// The window showing the record it works in.
    Window(TabId),
    /// The app it works in, where no window here shows its record by the name it is noted under.
    App(String),
}

/// Go to where `who` (a roster id among `people`) works, once: the window of theirs shown here,
/// else a window of the app they are in. Nothing moves when neither stands here. Every shell (a
/// device's session, a page) goes there through this.
pub fn go_to(handle: &crate::desktop::state::DesktopHandle, people: &[Person], who: &str) {
    let Some(person) = people.iter().find(|person| person.id == who) else {
        return;
    };
    if let Some(tab) = person.tab {
        handle.focus_tab(tab);
    } else if let Some(app) = person.followed_app() {
        handle.focus_app(app);
    }
}

/// Bring where this person's own agent works into view, when that is news
/// ([`HereState::take_own_agent_work`]): the strip scrolls to the window without taking the
/// keyboard, and waits while the person types or drags (`DesktopHandle::reveal_tab`). Whether it
/// asked the strip to move.
pub fn reveal_own_agent_work(
    here: &mut HereState,
    people: &[Person],
    handle: &crate::desktop::state::DesktopHandle,
) -> bool {
    let tab = match here.take_own_agent_work(people) {
        Some(WorkPlace::Window(tab)) => Some(tab),
        Some(WorkPlace::App(app)) => handle.tab_of_app(&app),
        None => None,
    };
    tab.is_some_and(|tab| handle.reveal_tab(tab))
}

/// The agents in `narrated` that act for this person show them where they act: each one is noted
/// ([`HereState::note_agent_acted`]), so where it works comes into view on the next look
/// ([`reveal_own_agent_work`]). Someone else's agent moves nothing here.
pub fn note_narrated(here: &mut HereState, narrated: &[SemanticInteraction]) {
    for interaction in narrated {
        if interaction.actor.is_agent()
            && let Some(person) = interaction.actor.acted_for_person()
        {
            here.note_agent_acted(person);
        }
    }
}

/// `narrated` drawn into this shell's `presence`: each action moves its agent's cursor to the
/// control it is bound to, over the window that shows its record. Whether it drew any.
pub fn apply_narrated(
    presence: &mut ui::presence::PresenceCtx,
    narrated: &[SemanticInteraction],
) -> bool {
    let mut shown = false;
    for interaction in narrated {
        shown |= presence
            .apply_interaction(interaction.clone())
            .unwrap_or(false);
    }
    shown
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
    /// An agent acting for `for_person` (a person's root key) narrated an action:
    /// where it works is shown again on the next look, even when it did not move.
    pub fn note_agent_acted(&mut self, for_person: &str) {
        let own = self.own_user().is_some_and(|own| own == for_person);
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
    /// The newest such place, for the shell to bring into view; `None` when nothing moved. Someone
    /// else's agent is theirs to watch.
    pub fn take_own_agent_work(&mut self, people: &[Person]) -> Option<WorkPlace> {
        let acted = std::mem::take(&mut self.agent_work.acted);
        let own = self.own_user()?.to_owned();
        let mut news = None;
        for person in people.iter().filter(|person| person.agent) {
            let theirs = person.user.as_deref().is_some_and(|user| user == own);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::here::LiveMember;

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
        here.note_agent_acted(ADA);
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
