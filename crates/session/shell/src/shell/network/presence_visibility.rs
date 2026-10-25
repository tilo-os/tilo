//! What the people in each room see of this person, as this session holds
//! every pointer, caret, wave and live frame it sends to it: the person's
//! choice for the room, else the room's default, a public workspace
//! "That I'm here" and a private one "Where I am".
//!
//! The network works this out itself, from what it holds: the choices the
//! person saved (read when the session starts, and each new one the desktop
//! hands over), the rooms held through the web, and what each room's host
//! answered about it being public. So a session without a desktop on
//! screen, an agent's device or a headless one, holds its rooms to the same
//! rule; the desktop only shows the choice and changes it.

use std::collections::{BTreeMap, HashMap};

use super::{RoomId, ShellNetwork, directory::Publication};

/// The settings section the person's choices are saved in, with the rest of
/// their collaboration preferences.
pub(crate) const SECTION: &str = "collaboration";
/// The key under [`SECTION`] that holds the choice per room.
pub(crate) const KEY: &str = "presence_visibility";

/// What the people in a workspace see of this person: their `chosen`
/// visibility, else the workspace's default. `public` is whether it is
/// public, `None` while that is not known: such a workspace is held to the
/// public default, so nothing of where the person is leaves before it is.
#[must_use]
pub(crate) const fn visibility_of(
    chosen: Option<ui::PresenceVisibility>,
    public: Option<bool>,
) -> ui::PresenceVisibility {
    match (chosen, public) {
        (Some(chosen), _) => chosen,
        (None, Some(public)) => ui::PresenceVisibility::default_for(public),
        (None, None) => ui::PresenceVisibility::default_for(true),
    }
}

/// The choices saved in `values` (the [`SECTION`] settings section), per
/// room. A room or choice this build does not read is left out: that
/// workspace takes its default, never something looser.
pub(crate) fn saved_choices(
    values: &serde_json::Map<String, serde_json::Value>,
) -> BTreeMap<RoomId, ui::PresenceVisibility> {
    values
        .get(KEY)
        .and_then(serde_json::Value::as_object)
        .map(|rooms| {
            rooms
                .iter()
                .filter_map(|(room, choice)| {
                    Some((
                        RoomId::parse(room)?,
                        ui::PresenceVisibility::from_id(choice.as_str()?)?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What this session holds about who sees what of this person.
#[derive(Default)]
pub(crate) struct Visibilities {
    /// What the person chose, per room.
    chosen: BTreeMap<RoomId, ui::PresenceVisibility>,
    /// Whether each room is public, as its host answered; `None` while
    /// that is being asked.
    public: HashMap<RoomId, Option<bool>>,
}

impl Visibilities {
    /// Holding `chosen`, the person's saved choices.
    pub(crate) fn with_choices(chosen: BTreeMap<RoomId, ui::PresenceVisibility>) -> Self {
        Self {
            chosen,
            public: HashMap::new(),
        }
    }

    /// The person's saved choices, as the settings they keep them in say.
    pub(crate) fn saved() -> Self {
        Self::with_choices(saved_choices(&settings_store::load_section(SECTION)))
    }
}

impl ShellNetwork {
    /// The person chose `visibility` for what the people in `room` see of
    /// them, from now on. Every pointer, caret, selection and live frame
    /// this session publishes in that room is held to it here, whoever
    /// asked for it to go out.
    pub fn set_presence_visibility(&self, room: &str, visibility: ui::PresenceVisibility) {
        let Some(room) = RoomId::parse(room) else {
            return;
        };
        if let Ok(mut held) = self.presence_visibility.lock() {
            held.chosen.insert(room, visibility);
        }
    }

    /// What the people in `room` see of this person: what they chose there,
    /// else the room's default. A room this device is not in is held to
    /// "Nothing": nothing of the person leaves there, and they see nobody's
    /// pointer. Whether a room this device is in is public is asked of its
    /// host once; until it answers, the room is held to the public default.
    pub fn presence_visibility(&self, room: &str) -> ui::PresenceVisibility {
        let Some(room) = RoomId::parse(room) else {
            return ui::PresenceVisibility::Nothing;
        };
        let known = match self.presence_visibility.lock() {
            Ok(held) => held
                .chosen
                .get(&room)
                .copied()
                .map(|chosen| (Some(chosen), None))
                .or_else(|| held.public.get(&room).map(|public| (None, *public))),
            Err(_) => return ui::PresenceVisibility::Nothing,
        };
        if let Some((chosen, public)) = known {
            return visibility_of(chosen, public);
        }
        if let Some(site) = self
            .site_room_labels()
            .into_iter()
            .find(|site| site.room == room)
        {
            // Held through the web for good: a site's home or one joined
            // from a directory is public, whatever any host would answer.
            let public = site.community || site.public;
            if let Ok(mut held) = self.presence_visibility.lock() {
                held.public.insert(room, Some(public));
            }
            return visibility_of(None, Some(public));
        }
        if !self.tree.read().rooms().contains(&room) {
            return ui::PresenceVisibility::Nothing;
        }
        self.ask_publication(room);
        visibility_of(None, self.room_is_public(room))
    }

    /// Whether `room` is public, as its host last answered; `None` before
    /// it has.
    fn room_is_public(&self, room: RoomId) -> Option<bool> {
        self.presence_visibility
            .lock()
            .ok()
            .and_then(|held| held.public.get(&room).copied().flatten())
    }

    /// Ask `room`'s host whether it is public, unless that is asked
    /// already: on the network's worker, off the frame loop, or here
    /// without one.
    fn ask_publication(&self, room: RoomId) {
        match self.presence_visibility.lock() {
            Ok(mut held) if !held.public.contains_key(&room) => {
                held.public.insert(room, None);
            }
            _ => return,
        }
        if !self.ask_publication_off_loop(room) {
            let _ = self.publication(room);
        }
    }

    /// Hold what `room`'s host answered about it being public: a room it
    /// lists is public; one no host lists, or someone else runs privately,
    /// is private. A room held through the web keeps what its site says.
    pub(super) fn note_publication(&self, room: RoomId, answer: &Result<Publication, String>) {
        if self.site_rooms().contains(&room) {
            return;
        }
        let public = answer.as_ref().is_ok_and(|publication| publication.public);
        if let Ok(mut held) = self.presence_visibility.lock() {
            held.public.insert(room, Some(public));
            if let Ok(publication) = answer
                && publication.room != room
            {
                held.public.insert(publication.room, Some(public));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The person's choice holds wherever they made one; elsewhere a public
    /// workspace shows that they are here and a private one where, and one
    /// not known yet is held to the public default.
    #[test]
    fn a_choice_holds_else_the_workspace_default() {
        use ui::PresenceVisibility::{Nothing, ThatImHere, WhereIAm};
        assert_eq!(visibility_of(Some(Nothing), Some(false)), Nothing);
        assert_eq!(visibility_of(Some(WhereIAm), Some(true)), WhereIAm);
        assert_eq!(visibility_of(None, Some(true)), ThatImHere);
        assert_eq!(visibility_of(None, Some(false)), WhereIAm);
        assert_eq!(visibility_of(None, None), ThatImHere);
    }

    /// Saved choices read back per room; a room or choice this build does
    /// not read is left out, so that workspace takes its default.
    #[test]
    fn saved_choices_read_back_and_skip_what_they_do_not_know() {
        let room = RoomId::new();
        let mut rooms = serde_json::Map::new();
        rooms.insert(
            room.to_string(),
            ui::PresenceVisibility::Nothing.id().into(),
        );
        rooms.insert(
            "not a room".into(),
            ui::PresenceVisibility::WhereIAm.id().into(),
        );
        rooms.insert(RoomId::new().to_string(), "something newer".into());
        let mut values = serde_json::Map::new();
        values.insert(KEY.into(), rooms.into());
        let saved = saved_choices(&values);
        assert_eq!(
            saved,
            BTreeMap::from([(room, ui::PresenceVisibility::Nothing)])
        );
    }
}
