//! Arriving.
//!
//! Entering a workspace with people in it (joining it, or switching to it)
//! puts up the arrival line ([`crate::shell::arrival_line`]): its name and
//! the faces of who is here. It is not put up again while nobody arrived
//! or left since the last time. The first time in a public workspace it
//! also says what the people there see of you, with "Change" to choose
//! otherwise. Nothing on it asks for anything; any input dismisses it.
//! Someone waving at you shows in the top bar, as it does in a browser tab
//! ([`tilo_desktop::chrome::WaveNotice`]).

use std::collections::{BTreeSet, HashMap};
use std::time::Instant;

use vello::kurbo::Point;

use crate::shell::i18n::t;
use crate::shell::state::RoomId;
use tilo_desktop::collaborators::RosterRow;

/// What the people in a public workspace see of the person, as the
/// arrival line tells them the first time.
fn intro_text(visibility: ui::PresenceVisibility) -> String {
    t(if visibility.shows_where() {
        "presence-arrival-intro-where"
    } else if visibility.shows_presence() {
        "presence-arrival-intro-here"
    } else {
        "presence-arrival-intro-nothing"
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
    /// Look at who is in the workspace on screen, and put the line up when
    /// there is something to say.
    pub(crate) fn refresh_presence_notices(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;
        let Some(chrome) = self.chrome.clone() else {
            return changed;
        };
        let generation = chrome.borrow().tree_handle().generation();
        if self.presence_notices.looked_at == Some(generation) {
            return changed;
        }
        self.presence_notices.looked_at = Some(generation);
        let (room, workspace, rows) = {
            let state = chrome.borrow();
            let local = state.active_local_user();
            let tree = state.tree();
            let Some(active) = tree.desktop.active_workspace() else {
                return changed;
            };
            (
                active.room(),
                active.name.clone(),
                tilo_desktop::collaborators::collaborator_roster_rows(&tree.desktop, &local)
                    .into_iter()
                    .filter(|row| !row.invited)
                    .collect::<Vec<RosterRow>>(),
            )
        };
        let ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
        let intro =
            self.active_workspace_is_public() && !self.presence_notices.introduced.contains(&room);
        if self.presence_notices.look(room, &ids, intro) {
            let people = rows
                .into_iter()
                .map(|row| {
                    let [r, g, b, _] = row.color.to_rgba8().to_u8_array();
                    crate::shell::arrival_line::Present {
                        name: row.label,
                        id: row.id,
                        colour: Some((r, g, b)),
                        is_agent: row.is_agent,
                    }
                })
                .collect();
            self.arrival_line.show(&workspace, people, now);
            if intro {
                self.arrival_line
                    .tell(intro_text(self.presence_visibility(room)));
                self.persist_collaboration_preferences();
            }
            changed = true;
        }
        changed
    }

    /// The first time in a public workspace when it is joined: the arrival
    /// line the join put up says what its people see of the person.
    pub(crate) fn introduce_joined(&mut self, room: RoomId) {
        if self.presence_notices.introduced.contains(&room) || !self.room_is_public(room) {
            return;
        }
        self.presence_notices.introduced.insert(room);
        self.arrival_line
            .tell(intro_text(self.presence_visibility(room)));
        self.persist_collaboration_preferences();
    }

    /// A press on the arrival line's "Change" opens the avatar menu.
    /// Returns whether it was there.
    pub(crate) fn press_presence_notice(&mut self, at: Point) -> bool {
        let change = self.arrival_line.change_at(at);
        if change {
            self.arrival_line.dismiss();
        }
        if change && let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().bar_popover_request =
                Some(crate::shell::chrome_state::BarPopoverRequest::Profile);
            self.harvest_bar_popover_request();
        }
        change
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
