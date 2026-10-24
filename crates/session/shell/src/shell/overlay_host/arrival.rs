//! Arriving.
//!
//! Entering a workspace with people in it (joining it, or switching to it)
//! says so on the faces the top bar already shows, never over the windows:
//! those faces pulse and a short name slides out of them
//! ([`tilo_desktop::chrome::ArrivalNotice`]). It is not said again while
//! nobody arrived or left since the last time. The first time in a public
//! workspace it says instead what the people there see of you, and pressing
//! it opens your own menu to choose otherwise. Nothing on it asks for
//! anything; it goes on its own. Someone waving at you shows in the top bar
//! the same way ([`tilo_desktop::chrome::WaveNotice`]).

use std::collections::{BTreeSet, HashMap};

use crate::shell::i18n::t;
use crate::shell::state::RoomId;
use tilo_desktop::collaborators::RosterRow;

/// What the people in a public workspace see of the person, in the few
/// words the bar's status has room for.
fn intro_text(visibility: ui::PresenceVisibility) -> String {
    t(if visibility.shows_where() {
        "presence-arrival-seen-where"
    } else if visibility.shows_presence() {
        "presence-arrival-seen-here"
    } else {
        "presence-arrival-seen-nothing"
    })
}

#[derive(Default)]
pub(crate) struct PresenceNotices {
    /// Who was here when the line last spoke of each workspace.
    pub(crate) seen: HashMap<RoomId, BTreeSet<String>>,
    /// The workspace on screen at the last look, and who was here.
    pub(crate) looked: Option<(RoomId, BTreeSet<String>)>,
    /// Public workspaces whose people this person was told about.
    pub(crate) introduced: BTreeSet<RoomId>,
    /// The workspace tree's write generation at the last look: who is here
    /// is read again only after it changed.
    pub(crate) looked_at: Option<u64>,
}

impl PresenceNotices {
    /// The workspace on screen is `room` with `people` in it: whether to
    /// put the arrival line up, because this is a new arrival or who is here
    /// changed since the line last spoke of it. `intro` says the person is
    /// to be told about being seen there.
    pub(crate) fn look(&mut self, room: RoomId, people: &[String], intro: bool) -> bool {
        let here: BTreeSet<String> = people.iter().cloned().collect();
        let same = self
            .looked
            .as_ref()
            .is_some_and(|(seen_room, seen)| *seen_room == room && *seen == here);
        if same {
            return false;
        }
        let arrived = self
            .looked
            .as_ref()
            .is_none_or(|(seen_room, _)| *seen_room != room);
        self.looked = Some((room, here.clone()));
        // Within a workspace already on screen, the line speaks only of
        // arriving there; people coming and going show on their faces.
        if !arrived || here.is_empty() {
            return false;
        }
        if self.seen.get(&room) == Some(&here) {
            return false;
        }
        self.seen.insert(room, here);
        if intro {
            self.introduced.insert(room);
        }
        true
    }
}

impl super::ShellOverlayHost {
    /// Look at who is in the workspace on screen, and say so in the bar
    /// when there is something to say.
    pub(crate) fn refresh_presence_notices(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let generation = chrome.borrow().tree_handle().generation();
        if self.presence_notices.looked_at == Some(generation) {
            return false;
        }
        self.presence_notices.looked_at = Some(generation);
        let (room, rows) = {
            let state = chrome.borrow();
            let local = state.active_local_user();
            let tree = state.tree();
            let Some(active) = tree.desktop.active_workspace() else {
                return false;
            };
            (
                active.room(),
                tilo_desktop::collaborators::collaborator_roster_rows(&tree.desktop, &local)
                    .into_iter()
                    .filter(|row| !row.invited && !row.own_device)
                    .collect::<Vec<RosterRow>>(),
            )
        };
        let ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
        let intro =
            self.active_workspace_is_public() && !self.presence_notices.introduced.contains(&room);
        if !self.presence_notices.look(room, &ids, intro) {
            return false;
        }
        let names = rows.into_iter().map(|row| row.label).collect();
        self.say_arrived(ids, names);
        if intro {
            self.tell_seen(room);
            self.persist_collaboration_preferences();
        }
        true
    }

    /// Say in the bar that the person arrived where `people` (by roster id,
    /// named `names`) are: their faces pulse and a short name slides out.
    pub(crate) fn say_arrived(&mut self, people: Vec<String>, names: Vec<String>) {
        let Some(chrome) = self.chrome.as_ref() else {
            return;
        };
        self.arrival_pulses.extend(people.iter().cloned());
        let mut state = chrome.borrow_mut();
        state.arrival_notice = Some(tilo_desktop::chrome::ArrivalNotice {
            people,
            names,
            seen: None,
            at: ui::time::now(),
        });
        state.bump();
    }

    /// The status just said also tells what the people in `room` see of
    /// the person.
    fn tell_seen(&self, room: RoomId) {
        let seen = intro_text(self.presence_visibility(room));
        if let Some(chrome) = self.chrome.as_ref() {
            let mut state = chrome.borrow_mut();
            if let Some(notice) = state.arrival_notice.as_mut() {
                notice.seen = Some(seen);
                state.bump();
            }
        }
    }

    /// The faces to pulse since the last call: the people the person just
    /// arrived among. The frame bounces them on the presence it paints.
    pub(crate) fn take_arrival_pulses(&mut self) -> Vec<String> {
        std::mem::take(&mut self.arrival_pulses)
    }

    /// The first time in a public workspace when it is joined: the status
    /// the join put up says what its people see of the person.
    pub(crate) fn introduce_joined(&mut self, room: RoomId) {
        if self.presence_notices.introduced.contains(&room) || !self.room_is_public(room) {
            return;
        }
        self.presence_notices.introduced.insert(room);
        self.tell_seen(room);
        self.persist_collaboration_preferences();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    /// Arriving where people are says who; coming back with nobody new
    /// says nothing; a workspace with nobody else says nothing.
    #[test]
    fn arriving_says_who_is_here_once_until_it_changes() {
        let mut notices = PresenceNotices::default();
        let lounge = RoomId::new();
        let home = RoomId::new();
        assert!(notices.look(lounge, &ids(&["peer@1", "peer@2"]), false));
        assert!(
            !notices.look(lounge, &ids(&["peer@1", "peer@2", "peer@3"]), false),
            "people coming and going while you are here show on their faces"
        );
        assert!(
            !notices.look(home, &[], false),
            "nobody else: nothing to say"
        );
        assert!(
            notices.look(lounge, &ids(&["peer@1", "peer@2", "peer@3"]), false),
            "back, and someone new is here"
        );
        notices.look(home, &[], false);
        assert!(
            !notices.look(lounge, &ids(&["peer@1", "peer@2", "peer@3"]), false),
            "back with nothing changed: not again"
        );
    }

    /// The first time in a public workspace the line tells you what its
    /// people see of you; never again there.
    #[test]
    fn a_public_workspace_tells_you_once_what_its_people_see() {
        let mut notices = PresenceNotices::default();
        let lounge = RoomId::new();
        assert!(notices.look(lounge, &ids(&["peer@1"]), true));
        assert!(notices.introduced.contains(&lounge));
    }
}
